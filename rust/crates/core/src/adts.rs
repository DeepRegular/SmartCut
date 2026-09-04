//! ADTS headers, so a frame this tool encodes is the same *kind* of frame as
//! the ones it is spliced between.
//!
//! Japanese broadcasts carry MPEG-2 AAC LC: the ADTS `ID` bit is 1. FFmpeg's
//! encoder produces MPEG-4 AAC, and every muxer that frames raw AAC for us --
//! the ADTS muxer, and the MPEG-TS muxer through it -- writes `ID = 0`
//! unconditionally unless the ADTS muxer's own `write_mpeg2` is set, which
//! MPEG-TS has no way to pass down. A cut whose seams were re-encoded then
//! comes out as a stream that is MPEG-2 nearly everywhere and MPEG-4 for one
//! frame per seam, which the tools downstream of a recording read as a
//! malformed stream rather than as the recording they were given.
//!
//! So the headers are written here instead. Every muxer involved leaves a
//! packet that already begins with a sync word alone, which is what makes
//! this work: MPEG-TS passes it through untouched, and MP4 runs it through
//! `aac_adtstoasc` exactly as it does the source's own frames.

/// The fixed part of a stream's ADTS header -- everything that is the same in
/// every frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdtsFormat {
    /// The `ID` bit: MPEG-2 AAC when set, MPEG-4 AAC when clear.
    pub mpeg2: bool,
    /// `profile_ObjectType`; 1 is LC, which is all a broadcast uses.
    pub profile: u8,
    pub sampling_index: u8,
    pub channel_config: u8,
    pub private: bool,
    pub original: bool,
    pub home: bool,
}

/// Which AAC the frames this tool encodes should announce themselves as.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AacVersion {
    /// Whatever the recording's own frames say.
    #[default]
    Auto,
    Mpeg2,
    Mpeg4,
}

impl AacVersion {
    /// The `ID` bit this asks for, or `None` for whatever the recording uses.
    pub fn forced(self) -> Option<bool> {
        match self {
            AacVersion::Auto => None,
            AacVersion::Mpeg2 => Some(true),
            AacVersion::Mpeg4 => Some(false),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AacVersion::Auto => "auto",
            AacVersion::Mpeg2 => "mpeg2",
            AacVersion::Mpeg4 => "mpeg4",
        }
    }
}

/// Header length in bytes: 7, or 9 when the frame carries a CRC.
pub const HEADER_LEN: usize = 7;

fn has_sync(data: &[u8]) -> bool {
    data.len() >= 7 && data[0] == 0xFF && data[1] & 0xF0 == 0xF0
}

impl AdtsFormat {
    /// Read the fixed header off a frame, if that is what this is.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if !has_sync(data) {
            return None;
        }
        Some(AdtsFormat {
            mpeg2: data[1] & 0x08 != 0,
            profile: data[2] >> 6,
            sampling_index: (data[2] >> 2) & 0x0F,
            channel_config: ((data[2] & 0x01) << 2) | (data[3] >> 6),
            private: data[2] & 0x02 != 0,
            original: data[3] & 0x20 != 0,
            home: data[3] & 0x10 != 0,
        })
    }

    /// The same format, saying what `want` asks it to say.
    pub fn as_version(mut self, want: AacVersion) -> Self {
        if let Some(mpeg2) = want.forced() {
            self.mpeg2 = mpeg2;
        }
        self
    }

    /// The same format, for a frame carrying `channels` channels.
    ///
    /// Only for frames whose channel count is not the recording's -- a
    /// downmix. The header is where a transport stream says how many channels
    /// a frame has, so leaving the recording's count on a downmixed frame
    /// would announce 5.1 over a stereo payload, and a decoder that believes
    /// the header ahead of the payload gets neither.
    ///
    /// A count with no `channel_config` of its own is left alone: such a
    /// stream describes itself in a program config element inside the frame,
    /// which is the encoder's business and not this header's.
    pub fn with_channels(mut self, channels: u16) -> Self {
        if let Some(config) = channel_config(channels) {
            self.channel_config = config;
        }
        self
    }

    /// A 7-byte header for a payload of `payload` bytes.
    ///
    /// `protection_absent` is set, so no CRC follows -- it is a per-frame
    /// field, and a frame without one sits legally among frames with one.
    /// Writing a CRC that is subtly wrong would be worse than writing none:
    /// a decoder that checks it would throw the frame away.
    pub fn header(&self, payload: usize) -> [u8; HEADER_LEN] {
        let len = (payload + HEADER_LEN).min(0x1FFF) as u32;
        let mut h = [0u8; HEADER_LEN];
        h[0] = 0xFF;
        h[1] = 0xF0 | ((self.mpeg2 as u8) << 3) | 0x01;
        h[2] = (self.profile << 6)
            | ((self.sampling_index & 0x0F) << 2)
            | ((self.private as u8) << 1)
            | ((self.channel_config >> 2) & 0x01);
        h[3] = ((self.channel_config & 0x03) << 6)
            | ((self.original as u8) << 5)
            | ((self.home as u8) << 4)
            | ((len >> 11) & 0x03) as u8;
        h[4] = ((len >> 3) & 0xFF) as u8;
        h[5] = (((len & 0x07) << 5) | 0x1F) as u8;
        // buffer_fullness 0x7FF -- "variable rate, do not ask" -- and one raw
        // data block, which is what every broadcast frame carries.
        h[6] = 0xFC;
        h
    }

    /// `payload` -- a raw AAC frame, as an encoder hands it over -- with a
    /// header in front of it.
    pub fn wrap(&self, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + HEADER_LEN);
        out.extend_from_slice(&self.header(payload.len()));
        out.extend_from_slice(payload);
        out
    }
}

/// Read the fixed header off the recording's audio frames.
///
/// Reads forward from wherever the context is, which at the point this is
/// called is the beginning: a transport stream interleaves its audio finely
/// enough that the frames turn up within a few hundred kilobytes.
///
/// Not the *first* audio packet, though -- a recording starts wherever the
/// tuner was told to start, and its opening audio packet is regularly the
/// tail of a frame whose header went out before the recording began. So this
/// keeps looking until a packet parses.
///
/// `None` means the frames are not ADTS at all -- raw AAC in an MP4, or some
/// other codec entirely -- in which case nothing should be framing anything.
pub fn framing(
    ictx: &mut ffmpeg_next::format::context::Input,
    audio_index: usize,
) -> Option<AdtsFormat> {
    let mut seen = 0;
    for (stream, packet) in ictx.packets().take(8192) {
        if stream.index() != audio_index {
            continue;
        }
        if let Some(f) = packet.data().and_then(AdtsFormat::parse) {
            return Some(f);
        }
        seen += 1;
        if seen > 64 {
            return None;
        }
    }
    None
}

/// As [`framing`], for a recording that is not open.
pub fn of_source(src: &crate::Source) -> Option<AdtsFormat> {
    let audio = src.audio.as_ref()?;
    let mut ictx = ffmpeg_next::format::input(&src.input.url).ok()?;
    framing(&mut ictx, audio.stream_index)
}

/// The `channel_config` a channel count is written as, where there is one.
///
/// The configurations run 1..=6 for mono through 5.1 and 7 for 7.1; anything
/// else -- 5.1 rear, say -- is `None`, and carries a program config element
/// instead.
pub fn channel_config(channels: u16) -> Option<u8> {
    match channels {
        1..=6 => Some(channels as u8),
        8 => Some(7),
        _ => None,
    }
}

/// The sampling frequency index a rate is written as.
pub fn sampling_index(rate: u32) -> Option<u8> {
    const RATES: [u32; 13] = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    RATES.iter().position(|&r| r == rate).map(|i| i as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header a broadcast frame actually carries, off the front of an
    /// ARIB recording: MPEG-2, LC, 48 kHz, stereo, CRC present.
    const BROADCAST: [u8; 7] = [0xFF, 0xF8, 0x4C, 0xA0, 0x52, 0xC1, 0x78];

    #[test]
    fn reads_a_broadcast_header() {
        let f = AdtsFormat::parse(&BROADCAST).unwrap();
        assert!(f.mpeg2);
        assert_eq!(f.profile, 1);
        assert_eq!(f.sampling_index, 3);
        assert_eq!(f.channel_config, 2);
        assert_eq!(sampling_index(48000), Some(3));
    }

    #[test]
    fn writes_one_back() {
        let f = AdtsFormat::parse(&BROADCAST).unwrap();
        let h = f.header(675);
        assert_eq!(AdtsFormat::parse(&h).unwrap(), f);
        // Sync, MPEG-2, no CRC.
        assert_eq!(h[0], 0xFF);
        assert_eq!(h[1] & 0x0F, 0x09);
        // Length counts the header.
        let len = ((h[3] as u32 & 0x03) << 11) | ((h[4] as u32) << 3) | (h[5] as u32 >> 5);
        assert_eq!(len, 675 + 7);
        assert_eq!(h[6] & 0x03, 0, "one raw data block");
    }

    #[test]
    fn rewrites_the_channel_count() {
        let f = AdtsFormat::parse(&BROADCAST).unwrap();
        assert_eq!(f.with_channels(1).channel_config, 1);
        assert_eq!(f.with_channels(6).channel_config, 6);
        assert_eq!(f.with_channels(8).channel_config, 7);
        // No configuration of its own, so the header keeps what it had.
        assert_eq!(f.with_channels(7).channel_config, f.channel_config);
        // And it survives a round trip through a header.
        let h = f.with_channels(2).header(675);
        assert_eq!(AdtsFormat::parse(&h).unwrap().channel_config, 2);
    }

    #[test]
    fn forces_a_version() {
        let f = AdtsFormat::parse(&BROADCAST).unwrap();
        assert!(!f.as_version(AacVersion::Mpeg4).mpeg2);
        assert!(f.as_version(AacVersion::Mpeg2).mpeg2);
        assert!(f.as_version(AacVersion::Auto).mpeg2);
    }

    #[test]
    fn wraps_a_payload() {
        let f = AdtsFormat::parse(&BROADCAST).unwrap();
        let wrapped = f.wrap(&[1, 2, 3]);
        assert_eq!(wrapped.len(), 3 + HEADER_LEN);
        assert_eq!(AdtsFormat::parse(&wrapped).unwrap(), f);
        assert_eq!(&wrapped[HEADER_LEN..], &[1, 2, 3]);
    }
}
