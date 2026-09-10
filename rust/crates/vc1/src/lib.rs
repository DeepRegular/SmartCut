//! VC-1: reading what a stream's pictures are, and writing new ones.
//!
//! This exists because there is no VC-1 encoder to be had. libavcodec
//! decodes VC-1 and every hardware encoder ignores it, so a smart cut of a
//! Blu-ray written in VC-1 -- which is most of the discs pressed between
//! 2006 and 2010 -- had nothing to re-encode its partial GOPs with, and the
//! cut fell back to being no cut at all.
//!
//! What is written here is not a general VC-1 encoder and is not meant to
//! become one. A smart cut needs pictures for the fragment of a GOP at each
//! end of a range, a few dozen at most, spliced in front of a copy of the
//! recording's own bytes. Those pictures can all be intra: nothing outside
//! the fragment is referenced, so no motion estimation, no prediction from
//! other pictures, and none of the syntax that carries either. That removes
//! nine tenths of the format and leaves a job worth doing.
//!
//! [`headers`] is the parsing the rest of the program uses to index a
//! stream; the encoder proper is built on top of it.

pub mod bits;
pub mod headers;
pub mod intra;
pub mod tables;
pub mod transform;

pub use headers::{Coding, EntryPoint, Kind, Picture, Quantizer, Sequence};
pub use intra::{Encoder, Frame, Plane, Unsupported};

/// The headers that describe a stream, kept together with the bytes they
/// arrived in.
///
/// A picture header cannot be read without both of them -- whether a
/// picture states its field order, whether it may vary its quantizer, even
/// how many bits its type occupies, all come from here. The raw bytes are
/// kept because an encoded picture has to be introduced by the recording's
/// own headers rather than by ones this program made up: the copied pictures
/// on the other side of the splice were coded against these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    pub sequence: Sequence,
    pub entry: EntryPoint,
    /// The sequence header as it arrived, start code and all.
    pub sequence_bdu: Vec<u8>,
    /// The entry-point header as it arrived, start code and all.
    pub entry_bdu: Vec<u8>,
}

/// How much of a packet is read to find a picture header in it.
///
/// The header runs to at most a few dozen bits, and a picture is hundreds of
/// kilobytes; unescaping all of it to read the first five bytes would cost
/// more than the index it feeds.
const HEADER_WINDOW: usize = 64;

impl Shape {
    /// Read the headers out of a container's extradata, or out of any packet
    /// that carries them.
    ///
    /// A transport stream restates both in front of every entry point, and
    /// libavformat hands the pair over as extradata, so in practice this is
    /// answered before a single packet has been read.
    pub fn read(data: &[u8]) -> Option<Shape> {
        let sequence_payload = headers::find(data, headers::SEQUENCE)?;
        let sequence = headers::sequence(sequence_payload)?;
        let entry_payload = headers::find(data, headers::ENTRY_POINT)?;
        let entry = headers::entry_point(entry_payload, &sequence)?;
        let bdu = |kind: u8, payload: &[u8]| {
            let mut out = vec![0, 0, 1, kind];
            out.extend_from_slice(payload);
            out
        };
        Some(Shape {
            sequence_bdu: bdu(headers::SEQUENCE, sequence_payload),
            entry_bdu: bdu(headers::ENTRY_POINT, entry_payload),
            sequence,
            entry,
        })
    }

    /// Read the picture header of the frame in this packet.
    pub fn picture(&self, packet: &[u8]) -> Option<Picture> {
        let payload = headers::find(packet, headers::FRAME)?;
        let window = &payload[..payload.len().min(HEADER_WINDOW)];
        headers::picture(window, &self.sequence, &self.entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The headers a Blu-ray written in VC-1 hands over as extradata: 1920
    /// by 1080, interlaced, with the pulldown flags in play.
    const EXTRADATA: &[u8] = &[
        0x00, 0x00, 0x01, 0x0f, 0xda, 0x00, 0x3b, 0xf2, 0x1b, 0xca, 0x3b, 0xf8, 0x86, 0xf1, 0x80,
        0xc9, 0x09, 0xaf, 0xa1, 0x1f, 0x27, 0x04, 0x00, 0x00, 0x01, 0x0e, 0x4c, 0x97, 0xf8, 0x80,
    ];

    #[test]
    fn reads_a_disc_sequence_header() {
        let shape = Shape::read(EXTRADATA).expect("headers");
        assert_eq!(shape.sequence.level, 3);
        assert_eq!(
            (shape.sequence.max_width, shape.sequence.max_height),
            (1920, 1080)
        );
        assert!(shape.sequence.interlace);
        assert!(shape.sequence.pulldown);
        assert!(!shape.sequence.psf);
        assert!(!shape.sequence.tfcntr);
        assert_eq!(shape.sequence.hrd_buckets, Some(1));
    }

    #[test]
    fn reads_the_entry_point_beside_it() {
        let shape = Shape::read(EXTRADATA).expect("headers");
        assert!(shape.entry.closed_entry);
        assert!(!shape.entry.panscan);
        assert!(!shape.entry.overlap);
        assert!(!shape.entry.vstransform);
        assert!(!shape.entry.extended_mv);
        assert_eq!(shape.entry.dquant, 1);
        assert_eq!(shape.entry.quantizer, Quantizer::NonUniform);
    }

    #[test]
    fn a_picture_says_what_it_is() {
        let shape = Shape::read(EXTRADATA).expect("headers");
        // An I picture coded as an interlaced frame, top field first. The
        // bits are FCM `10`, PTYPE `110`, TFF `1`, RFF `0`, RNDCTRL `0`;
        // then UVSAMP `0`, PQINDEX `01000` and HALFQP `1`.
        let packet = [0x00, 0x00, 0x01, 0x0d, 0b1011_0100, 0b0010_0010, 0x00, 0x00];
        let picture = shape.picture(&packet).expect("picture");
        assert_eq!(picture.coding, Coding::FrameInterlace);
        assert_eq!(picture.kind, Kind::I);
        assert!(picture.tff);
        assert!(!picture.rff);
        assert!(picture.reference());
        assert_eq!(picture.display_fields(), 2);
        assert_eq!(picture.pqindex, 8);
        assert!(picture.halfqp);
        assert!(!picture.uniform_quantizer);
    }
}
