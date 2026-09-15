//! LATM framing, for the AAC a 4K recording carries.
//!
//! A Japanese HD broadcast carries AAC in ADTS frames -- a sync word, a
//! header, the payload -- and [`crate::adts`] writes those. A 4K broadcast
//! carries the same codec framed the other way the standard allows: LOAS,
//! whose `AudioSyncStream` holds an `AudioMuxElement` that states the
//! stream's configuration in bits rather than in a fixed header, and states
//! it again in every frame a recorder writes.
//!
//! **Without this, a 4K recording's sound could only be copied.** An encoder
//! here hands back a raw AAC frame, the muxer passes it through untouched
//! because a transport stream's LATM track is bytes it does not read, and the
//! frame arrives at the player as a payload with no sync word in front of it
//! -- which is not a frame at all. So the boundary frames were left alone and
//! the cut's sound began and ended on whatever frame the recording had.
//!
//! What is written here is the simple shape, and the recording is read first
//! to be sure it is the shape the recording itself uses: one program, one
//! layer, AAC LC at 1024 samples, a configuration in every frame. Anything
//! else -- a rate written out in full, a channel arrangement described inside
//! the frame -- is not framed by this and the frames are copied as before.

/// The fixed part of a LOAS/LATM stream: everything about it that is the same
/// in every frame, which is the `AudioSpecificConfig` the frame carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatmFormat {
    /// `audioObjectType`; 2 is LC, which is what a broadcast sends.
    pub object_type: u8,
    pub sampling_index: u8,
    pub channel_config: u8,
}

/// The sync word an `AudioSyncStream` opens with, in its top 11 bits.
const SYNC: u16 = 0x2B7;

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u(&mut self, n: usize) -> Option<u32> {
        let mut v = 0;
        for _ in 0..n {
            let byte = *self.data.get(self.pos >> 3)?;
            v = (v << 1) | u32::from((byte >> (7 - (self.pos & 7))) & 1);
            self.pos += 1;
        }
        Some(v)
    }
}

#[derive(Default)]
struct Writer {
    data: Vec<u8>,
    bits: usize,
}

impl Writer {
    fn u(&mut self, value: u32, n: usize) {
        for i in (0..n).rev() {
            if self.bits.is_multiple_of(8) {
                self.data.push(0);
            }
            if (value >> i) & 1 == 1 {
                let at = self.data.len() - 1;
                self.data[at] |= 1 << (7 - (self.bits % 8));
            }
            self.bits += 1;
        }
    }

    /// The bytes written, padded out to the next whole one with zeros --
    /// `byteAlignment()`, which every `AudioMuxElement` ends with.
    fn aligned(mut self) -> Vec<u8> {
        if !self.bits.is_multiple_of(8) {
            self.u(0, 8 - (self.bits % 8));
        }
        self.data
    }
}

impl LatmFormat {
    /// Read the configuration off a frame, if this is a LOAS frame stating
    /// one in a shape this can write back.
    ///
    /// Everything checked here is a thing the writer below assumes. A stream
    /// that breaks one of them is not framed by this at all, rather than
    /// framed wrongly: `None` leaves the frames copied, which is what
    /// happened to every LATM recording before this existed.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 6 || data[0] != (SYNC >> 3) as u8 || data[1] >> 5 != (SYNC & 0x07) as u8 {
            return None;
        }
        let mut r = Reader {
            data: &data[3..],
            pos: 0,
        };
        // A frame that reuses the last frame's configuration does not carry
        // one, and a frame this replaces has to carry its own.
        if r.u(1)? != 0 || r.u(1)? != 0 || r.u(1)? != 1 {
            return None;
        }
        // One sub-frame, one program, one layer.
        if r.u(6)? != 0 || r.u(4)? != 0 || r.u(3)? != 0 {
            return None;
        }
        let object_type = r.u(5)? as u8;
        let sampling_index = r.u(4)? as u8;
        let channel_config = r.u(4)? as u8;
        // GASpecificConfig: 1024 samples a frame, no core coder, no
        // extension. Then `frameLengthType`, which says the payload's length
        // is stated in the frame rather than fixed.
        let plain = r.u(1)? == 0 && r.u(1)? == 0 && r.u(1)? == 0 && r.u(3)? == 0;
        let known = object_type == 2 && sampling_index < 13 && (1..=7).contains(&channel_config);
        (plain && known).then_some(LatmFormat {
            object_type,
            sampling_index,
            channel_config,
        })
    }

    /// The same format, for a frame carrying `channels` channels.
    ///
    /// Only for a downmix, and for the same reason [`crate::adts`] has one:
    /// the frame says how many channels it holds, and a frame that says six
    /// over a stereo payload is one a player gets nothing out of.
    pub fn with_channels(mut self, channels: u16) -> Self {
        if let Some(config) = crate::adts::channel_config(channels) {
            self.channel_config = config;
        }
        self
    }

    /// The same format, for a frame whose samples run at `rate`.
    pub fn with_rate(mut self, rate: u32) -> Self {
        if let Some(index) = crate::adts::sampling_index(rate) {
            self.sampling_index = index;
        }
        self
    }

    /// `payload` -- a raw AAC frame, as an encoder hands it over -- inside an
    /// `AudioSyncStream` of its own.
    ///
    /// The configuration is restated in this frame rather than left to the
    /// one before it. That is what the recorders read here do, and it is the
    /// only way a frame written into the middle of someone else's stream can
    /// be read on its own terms.
    pub fn wrap(&self, payload: &[u8]) -> Vec<u8> {
        let mut w = Writer::default();
        w.u(0, 1); // useSameStreamMux
        w.u(0, 1); // audioMuxVersion
        w.u(1, 1); // allStreamsSameTimeFraming
        w.u(0, 6); // numSubFrames
        w.u(0, 4); // numProgram
        w.u(0, 3); // numLayer
        w.u(u32::from(self.object_type), 5);
        w.u(u32::from(self.sampling_index), 4);
        w.u(u32::from(self.channel_config), 4);
        w.u(0, 3); // GASpecificConfig: 1024 samples, no core coder, no extension
        w.u(0, 3); // frameLengthType
        w.u(0xFF, 8); // latmBufferFullness: "not stated", which is what a
                      // stream nothing is buffering ahead of says
        w.u(0, 1); // otherDataPresent
        w.u(0, 1); // crcCheckPresent
        // PayloadLengthInfo: the length in bytes, 255 at a time.
        let mut left = payload.len();
        while left >= 255 {
            w.u(255, 8);
            left -= 255;
        }
        w.u(left as u32, 8);
        for byte in payload {
            w.u(u32::from(*byte), 8);
        }
        let element = w.aligned();
        // A length this long is not one 13 bits can state. Nothing an
        // encoder here writes comes near it -- 8191 bytes is half a second
        // of stereo at 128 kbit/s -- but a frame that overran would be
        // framed as a shorter one and read as garbage, so it is not framed.
        if element.len() > 0x1FFF {
            return payload.to_vec();
        }
        let mut out = Vec::with_capacity(element.len() + 3);
        let mut head = Writer::default();
        head.u(u32::from(SYNC), 11);
        head.u(element.len() as u32, 13);
        out.extend_from_slice(&head.aligned());
        out.extend_from_slice(&element);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first bytes of a frame a 4K recorder wrote: the sync word, the
    /// length, and a configuration for AAC LC at 48 kHz in stereo.
    const RECORDED: [u8; 8] = [0x56, 0xE2, 0xA7, 0x20, 0x00, 0x11, 0x90, 0x0D];

    #[test]
    fn reads_a_recorders_configuration() {
        let f = LatmFormat::parse(&RECORDED).unwrap();
        assert_eq!(f.object_type, 2);
        assert_eq!(f.sampling_index, 3); // 48 kHz
        assert_eq!(f.channel_config, 2);
    }

    #[test]
    fn a_frame_that_states_no_configuration_is_not_framed_by_this() {
        // `useSameStreamMux` set: this frame is read against the last one.
        let mut same = RECORDED;
        same[3] |= 0x80;
        assert!(LatmFormat::parse(&same).is_none());
        // And a payload with no sync word in front of it is not a frame.
        assert!(LatmFormat::parse(&[0x21, 0x00, 0x49, 0x90]).is_none());
    }

    #[test]
    fn wraps_a_payload_the_way_the_recorder_does() {
        let f = LatmFormat::parse(&RECORDED).unwrap();
        let framed = f.wrap(&[0xA5; 300]);
        // The sync word and a length that counts everything after it.
        assert_eq!(framed[0], 0x56);
        assert_eq!(framed[1] >> 5, 0x07);
        let stated = (usize::from(framed[1] & 0x1F) << 8) | usize::from(framed[2]);
        assert_eq!(stated, framed.len() - 3);
        // The configuration bits, which are the recorder's own.
        assert_eq!(&framed[3..7], &RECORDED[3..7]);
        // And it reads back as what it was written from.
        assert_eq!(LatmFormat::parse(&framed).unwrap(), f);
        // 300 bytes of payload is stated as 255 and then 45.
        assert!(framed.len() > 300);
    }

    #[test]
    fn a_downmixed_frame_says_how_many_channels_it_holds() {
        let f = LatmFormat::parse(&RECORDED).unwrap().with_channels(1);
        assert_eq!(f.channel_config, 1);
        assert_eq!(LatmFormat::parse(&f.wrap(&[0; 8])).unwrap().channel_config, 1);
    }
}
