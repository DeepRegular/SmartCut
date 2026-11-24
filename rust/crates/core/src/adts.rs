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

/// The twelve sync bits, and the two layer bits after the ID, which ADTS
/// always writes as zero. Without them an MPEG audio frame passes too -- its
/// sync is the same twelve bits with a layer after them -- and a broadcast's
/// MP2 was taken for ADTS: re-encoded to AAC into a transport stream, its
/// frames went out under headers built from MP2's bitrate bits, which read
/// as the SSR profile.
fn has_sync(data: &[u8]) -> bool {
    data.len() >= 7 && data[0] == 0xFF && data[1] & 0xF6 == 0xF0
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

    /// The same format, for a frame whose samples run at `rate`.
    ///
    /// Only for frames whose rate is not the recording's. The header is where
    /// a transport stream says what rate a frame runs at, so leaving the
    /// recording's index on a resampled frame would announce 48 kHz over
    /// 44.1 kHz samples, and a decoder that believes the header ahead of the
    /// payload plays the whole track at the wrong speed.
    ///
    /// A rate with no index of its own is left alone: AAC has thirteen it can
    /// name, and one it cannot name is one the encoder will have refused
    /// long before this header is written.
    pub fn with_rate(mut self, rate: u32) -> Self {
        if let Some(index) = sampling_index(rate) {
            self.sampling_index = index;
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

/// The program config element a frame opens on: the element as the frame
/// carries it (its three-bit id first, its comment last, a whole number of
/// bytes from the start of the raw data block), where the frame says
/// `channel_config` 0 and its first element is one.
///
/// **A track whose channels are given this way says so once.** FFmpeg's ADTS
/// muxer writes the element into the first frame only, and every frame after
/// it is read against it: a 2.1 track made by FFmpeg, cut from the middle,
/// began on a frame with nothing in it to say what its elements were. The
/// MP4 and Matroska muxers' `aac_adtstoasc` builds the file's one description
/// from the first frame it is handed and turned every frame of the cut away,
/// and a transport stream of it decoded to nothing. See [`with_pce`].
pub fn pce_of(frame: &[u8]) -> Option<Vec<u8>> {
    let raw = single_block(frame)?;
    if AdtsFormat::parse(frame)?.channel_config != 0 {
        return None;
    }
    let mut r = BitReader::new(raw);
    if r.get(3)? != ID_PCE {
        return None;
    }
    let mut sink = BitWriter::default();
    copy_pce(&mut r, &mut sink)?;
    r.align();
    let comment = r.get(8)? as usize;
    let end = r.pos / 8 + comment;
    (end <= raw.len()).then(|| raw[..end].to_vec())
}

/// `frame` with `pce` ([`pce_of`]) put in front of its elements, where it
/// says `channel_config` 0 and does not open on such an element already;
/// `None` where it is left as it is.
///
/// The element ends on a byte, counted from the start of the raw data block,
/// so it goes in whole bytes and everything after it keeps its alignment.
/// The header is the frame's own, with its length restated and its CRC (a
/// check of what the frame was) dropped.
pub fn with_pce(frame: &[u8], pce: &[u8]) -> Option<Vec<u8>> {
    let raw = single_block(frame)?;
    if AdtsFormat::parse(frame)?.channel_config != 0 || BitReader::new(raw).get(3) == Some(ID_PCE) {
        return None;
    }
    let len = HEADER_LEN + pce.len() + raw.len();
    if len > 0x1FFF {
        return None;
    }
    let mut h = [0u8; HEADER_LEN];
    h.copy_from_slice(&frame[..HEADER_LEN]);
    h[1] |= 0x01;
    h[3] = (h[3] & 0xFC) | ((len >> 11) & 0x03) as u8;
    h[4] = ((len >> 3) & 0xFF) as u8;
    h[5] = (h[5] & 0x1F) | (((len & 0x07) << 5) as u8);
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(&h);
    out.extend_from_slice(pce);
    out.extend_from_slice(raw);
    Some(out)
}

/// The AudioSpecificConfig a track of `format`'s frames described by `pce`
/// ([`pce_of`]) has: what a decoder opened in the middle of the track needs
/// to read its frames at all, as the MP4 muxer would build it.
pub fn asc_with_pce(format: &AdtsFormat, pce: &[u8]) -> Option<Vec<u8>> {
    let mut r = BitReader::new(pce);
    if r.get(3)? != ID_PCE {
        return None;
    }
    let mut w = BitWriter::default();
    w.put(u32::from(format.profile) + 1, 5);
    w.put(u32::from(format.sampling_index), 4);
    w.put(0, 4);
    // 1024-sample frames, no core coder, no extension.
    w.put(0, 3);
    copy_pce(&mut r, &mut w)?;
    copy_comment(&mut r, &mut w)?;
    Some(w.finish())
}

/// The element ([`pce_of`]) inside an AudioSpecificConfig, where it names
/// its channels that way: what an encoder that wrote one says its frames are.
pub fn pce_in_asc(asc: &[u8]) -> Option<Vec<u8>> {
    let mut r = BitReader::new(asc);
    let object = r.get(5)?;
    let index = r.get(4)?;
    let config = r.get(4)?;
    // Escaped object types and explicit rates, a core coder: not what any
    // encoder here writes.
    if object == 31 || index == 15 || config != 0 {
        return None;
    }
    let _frame_length = r.get(1)?;
    if r.get(1)? != 0 {
        return None;
    }
    let _extension = r.get(1)?;
    let mut w = BitWriter::default();
    w.put(ID_PCE, 3);
    copy_pce(&mut r, &mut w)?;
    copy_comment(&mut r, &mut w)?;
    Some(w.finish())
}

/// Whether two program config elements ([`pce_of`]) name the same channels
/// under the same element numbers: everything but their comments.
pub fn same_pce(a: &[u8], b: &[u8]) -> bool {
    let fields = |pce: &[u8]| {
        let mut r = BitReader::new(pce);
        let mut w = BitWriter::default();
        (r.get(3)? == ID_PCE).then_some(())?;
        copy_pce(&mut r, &mut w)?;
        Some(w.finish())
    };
    matches!((fields(a), fields(b)), (Some(x), Some(y)) if x == y)
}

/// `id_syn_ele` of a program config element.
const ID_PCE: u32 = 5;

/// The raw data block of an ADTS frame that holds exactly one, which is what
/// every frame this tool handles carries.
fn single_block(frame: &[u8]) -> Option<&[u8]> {
    AdtsFormat::parse(frame)?;
    if frame[6] & 0x03 != 0 {
        return None;
    }
    let head = if frame[1] & 0x01 != 0 { HEADER_LEN } else { HEADER_LEN + 2 };
    let len = ((usize::from(frame[3]) & 0x03) << 11) | (usize::from(frame[4]) << 3) | (usize::from(frame[5]) >> 5);
    frame.get(head..len.min(frame.len()))
}

/// A program config element after its id, field for field from `r` into
/// `w`, up to the byte alignment before its comment; and the channels it
/// names (front, side and back elements, a pair each where they are pairs,
/// and the LFE).
fn copy_pce(r: &mut BitReader<'_>, w: &mut BitWriter) -> Option<u16> {
    let mut take = |n: u32, w: &mut BitWriter| -> Option<u32> {
        let v = r.get(n)?;
        w.put(v, n);
        Some(v)
    };
    // element_instance_tag, object_type, sampling_frequency_index.
    take(4, w)?;
    take(2, w)?;
    take(4, w)?;
    let front = take(4, w)?;
    let side = take(4, w)?;
    let back = take(4, w)?;
    let lfe = take(2, w)?;
    let assoc = take(3, w)?;
    let cc = take(4, w)?;
    if take(1, w)? == 1 {
        take(4, w)?;
    }
    if take(1, w)? == 1 {
        take(4, w)?;
    }
    if take(1, w)? == 1 {
        take(3, w)?;
    }
    let mut channels = 0u16;
    for _ in 0..front + side + back {
        channels += if take(1, w)? == 1 { 2 } else { 1 };
        take(4, w)?;
    }
    for _ in 0..lfe + assoc {
        take(4, w)?;
    }
    channels += lfe as u16;
    for _ in 0..cc {
        take(5, w)?;
    }
    Some(channels)
}

/// The byte alignment and the comment that end a program config element,
/// each side aligned to its own start.
fn copy_comment(r: &mut BitReader<'_>, w: &mut BitWriter) -> Option<()> {
    r.align();
    w.align();
    let n = r.get(8)?;
    w.put(n, 8);
    for _ in 0..n {
        w.put(r.get(8)?, 8);
    }
    Some(())
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn get(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.data.get(self.pos / 8)?;
            v = (v << 1) | u32::from((byte >> (7 - self.pos % 8)) & 1);
            self.pos += 1;
        }
        Some(v)
    }

    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    fn put(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            if self.bits % 8 == 0 {
                self.bytes.push(0);
            }
            if (v >> i) & 1 == 1 {
                let last = self.bytes.len() - 1;
                self.bytes[last] |= 1 << (7 - self.bits % 8);
            }
            self.bits += 1;
        }
    }

    fn align(&mut self) {
        self.bits = self.bits.div_ceil(8) * 8;
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
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

    /// The head of the first frame FFmpeg's encoder and ADTS muxer write for
    /// 2.1 at 48 kHz: `channel_config` 0, a program config element naming a
    /// pair and an LFE, with the encoder's name as its comment.
    const PCE_FRAME: [u8; 30] = [
        0xFF, 0xF1, 0x4C, 0x00, 0x3D, 0x9F, 0xFC, 0xA0, 0x98, 0x80, 0x20, 0x04, 0x00, 0x0D, b'L', b'a',
        b'v', b'c', b'6', b'1', b'.', b'1', b'9', b'.', b'1', b'0', b'1', 0x21, 0x10, 0x05,
    ];

    #[test]
    fn carries_a_program_config_element_over() {
        let pce = pce_of(&PCE_FRAME).expect("opens on one");
        assert_eq!(pce, PCE_FRAME[7..27]);
        // What FFmpeg's aac_adtstoasc makes of the same frame.
        let mut asc = vec![0x11, 0x80, 0x04, 0xC4, 0x01, 0x00, 0x20, 0x00, 0x0D];
        asc.extend_from_slice(b"Lavc61.19.101");
        let f = AdtsFormat::parse(&PCE_FRAME).unwrap();
        assert_eq!(asc_with_pce(&f, &pce).unwrap(), asc);
        assert_eq!(pce_in_asc(&asc).unwrap(), pce);
        // A later frame of the track, which has none: given it in front.
        let mut later = f.header(3).to_vec();
        later.extend_from_slice(&[0x21, 0x10, 0x05]);
        let given = with_pce(&later, &pce).unwrap();
        assert_eq!(&given[HEADER_LEN..HEADER_LEN + pce.len()], &pce[..]);
        assert_eq!(&given[HEADER_LEN + pce.len()..], &[0x21, 0x10, 0x05]);
        let len = ((given[3] as usize & 3) << 11) | ((given[4] as usize) << 3) | (given[5] as usize >> 5);
        assert_eq!(len, given.len());
        assert_eq!(pce_of(&given).unwrap(), pce);
        // Not twice, and never into a frame that names a configuration.
        assert!(with_pce(&given, &pce).is_none());
        assert!(with_pce(&f.with_channels(2).wrap(&[0x21, 0x10, 0x05]), &pce).is_none());
        assert!(pce_of(&BROADCAST).is_none());
        // The same channels under another comment are the same element.
        let mut other = pce.clone();
        other[pce.len() - 1] = b'2';
        assert!(same_pce(&pce, &other));
        let mut renumbered = pce.clone();
        renumbered[0] ^= 0x02;
        assert!(!same_pce(&pce, &renumbered));
    }

    #[test]
    fn forces_a_version() {
        let f = AdtsFormat::parse(&BROADCAST).unwrap();
        assert!(!f.as_version(AacVersion::Mpeg4).mpeg2);
        assert!(f.as_version(AacVersion::Mpeg2).mpeg2);
        assert!(f.as_version(AacVersion::Auto).mpeg2);
    }

    #[test]
    fn an_mpeg_audio_frame_is_not_adts() {
        // MPEG-1 Layer II, 48 kHz, 256 kbit/s: the same twelve sync bits.
        assert!(AdtsFormat::parse(&[0xFF, 0xFD, 0xC4, 0x00, 0, 0, 0]).is_none());
        assert!(AdtsFormat::parse(&BROADCAST).is_some());
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
