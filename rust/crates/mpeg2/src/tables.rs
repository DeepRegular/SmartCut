//! The variable-length codes of ISO/IEC 13818-2, Annex B.
//!
//! Every field in a macroblock is one of these, so a slice cannot be walked
//! without them -- not even to find where the next macroblock begins. They
//! are the standard's own tables; the transcription here was checked against
//! libavcodec's copy of the same tables, since a table this program reads a
//! recording with has to be right in every row and there is no way to tell
//! by looking.
//!
//! A code is written here as `(code, bits)`: the low `bits` of `code`, most
//! significant first. Sign bits are not part of the code -- where a table row
//! carries a level, the sign follows the code as a single bit, `0` for
//! positive.

/// One row of a table: the code, and how many bits of it are the code.
pub type Code = (u16, u8);

/// Table B.14 -- the coefficient codes every non-intra block uses, and the
/// intra blocks of a picture whose `intra_vlc_format` is 0.
///
/// Rows 0..111 are the run/level pairs of [`RUN`] and [`LEVEL`], row
/// [`ESCAPE`] is the escape, and row [`END_OF_BLOCK`] ends a block. The
/// row for run 0, level 1 is `11`; the *first* coefficient of a non-intra
/// block writes `1` instead, because in that one place `10` is not an end
/// of block. See `parse`.
pub const COEFFICIENTS_ZERO: [Code; 113] = [
    (0x03, 2), (0x04, 4), (0x05, 5), (0x06, 7),
    (0x26, 8), (0x21, 8), (0x0a, 10), (0x1d, 12),
    (0x18, 12), (0x13, 12), (0x10, 12), (0x1a, 13),
    (0x19, 13), (0x18, 13), (0x17, 13), (0x1f, 14),
    (0x1e, 14), (0x1d, 14), (0x1c, 14), (0x1b, 14),
    (0x1a, 14), (0x19, 14), (0x18, 14), (0x17, 14),
    (0x16, 14), (0x15, 14), (0x14, 14), (0x13, 14),
    (0x12, 14), (0x11, 14), (0x10, 14), (0x18, 15),
    (0x17, 15), (0x16, 15), (0x15, 15), (0x14, 15),
    (0x13, 15), (0x12, 15), (0x11, 15), (0x10, 15),
    (0x03, 3), (0x06, 6), (0x25, 8), (0x0c, 10),
    (0x1b, 12), (0x16, 13), (0x15, 13), (0x1f, 15),
    (0x1e, 15), (0x1d, 15), (0x1c, 15), (0x1b, 15),
    (0x1a, 15), (0x19, 15), (0x13, 16), (0x12, 16),
    (0x11, 16), (0x10, 16), (0x05, 4), (0x04, 7),
    (0x0b, 10), (0x14, 12), (0x14, 13), (0x07, 5),
    (0x24, 8), (0x1c, 12), (0x13, 13), (0x06, 5),
    (0x0f, 10), (0x12, 12), (0x07, 6), (0x09, 10),
    (0x12, 13), (0x05, 6), (0x1e, 12), (0x14, 16),
    (0x04, 6), (0x15, 12), (0x07, 7), (0x11, 12),
    (0x05, 7), (0x11, 13), (0x27, 8), (0x10, 13),
    (0x23, 8), (0x1a, 16), (0x22, 8), (0x19, 16),
    (0x20, 8), (0x18, 16), (0x0e, 10), (0x17, 16),
    (0x0d, 10), (0x16, 16), (0x08, 10), (0x15, 16),
    (0x1f, 12), (0x1a, 12), (0x19, 12), (0x17, 12),
    (0x16, 12), (0x1f, 13), (0x1e, 13), (0x1d, 13),
    (0x1c, 13), (0x1b, 13), (0x1f, 16), (0x1e, 16),
    (0x1d, 16), (0x1c, 16), (0x1b, 16), (0x01, 6),
    (0x02, 2),
];

/// Table B.15 -- the same run/level pairs, coded differently, for the intra
/// blocks of a picture whose `intra_vlc_format` is 1.
///
/// Intra blocks spend their bits on large coefficients near the start of the
/// block, so the codes are assigned to suit that; a broadcast encoder turns
/// it on. The escape and the run/level rows line up with
/// [`COEFFICIENTS_ZERO`], row for row.
pub const COEFFICIENTS_ONE: [Code; 113] = [
    (0x02, 2), (0x06, 3), (0x07, 4), (0x1c, 5),
    (0x1d, 5), (0x05, 6), (0x04, 6), (0x7b, 7),
    (0x7c, 7), (0x23, 8), (0x22, 8), (0xfa, 8),
    (0xfb, 8), (0xfe, 8), (0xff, 8), (0x1f, 14),
    (0x1e, 14), (0x1d, 14), (0x1c, 14), (0x1b, 14),
    (0x1a, 14), (0x19, 14), (0x18, 14), (0x17, 14),
    (0x16, 14), (0x15, 14), (0x14, 14), (0x13, 14),
    (0x12, 14), (0x11, 14), (0x10, 14), (0x18, 15),
    (0x17, 15), (0x16, 15), (0x15, 15), (0x14, 15),
    (0x13, 15), (0x12, 15), (0x11, 15), (0x10, 15),
    (0x02, 3), (0x06, 5), (0x79, 7), (0x27, 8),
    (0x20, 8), (0x16, 13), (0x15, 13), (0x1f, 15),
    (0x1e, 15), (0x1d, 15), (0x1c, 15), (0x1b, 15),
    (0x1a, 15), (0x19, 15), (0x13, 16), (0x12, 16),
    (0x11, 16), (0x10, 16), (0x05, 5), (0x07, 7),
    (0xfc, 8), (0x0c, 10), (0x14, 13), (0x07, 5),
    (0x26, 8), (0x1c, 12), (0x13, 13), (0x06, 6),
    (0xfd, 8), (0x12, 12), (0x07, 6), (0x04, 9),
    (0x12, 13), (0x06, 7), (0x1e, 12), (0x14, 16),
    (0x04, 7), (0x15, 12), (0x05, 7), (0x11, 12),
    (0x78, 7), (0x11, 13), (0x7a, 7), (0x10, 13),
    (0x21, 8), (0x1a, 16), (0x25, 8), (0x19, 16),
    (0x24, 8), (0x18, 16), (0x05, 9), (0x17, 16),
    (0x07, 9), (0x16, 16), (0x0d, 10), (0x15, 16),
    (0x1f, 12), (0x1a, 12), (0x19, 12), (0x17, 12),
    (0x16, 12), (0x1f, 13), (0x1e, 13), (0x1d, 13),
    (0x1c, 13), (0x1b, 13), (0x1f, 16), (0x1e, 16),
    (0x1d, 16), (0x1c, 16), (0x1b, 16), (0x01, 6),
    (0x06, 4),
];

/// How many zero coefficients come before the one a row codes.
pub const RUN: [u8; 111] = [
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 2, 2, 2, 2, 2, 3,
    3, 3, 3, 4, 4, 4, 5, 5,
    5, 6, 6, 6, 7, 7, 8, 8,
    9, 9, 10, 10, 11, 11, 12, 12,
    13, 13, 14, 14, 15, 15, 16, 16,
    17, 18, 19, 20, 21, 22, 23, 24,
    25, 26, 27, 28, 29, 30, 31,
];

/// The magnitude a row codes. The sign is the bit after the code.
pub const LEVEL: [u8; 111] = [
    1, 2, 3, 4, 5, 6, 7, 8,
    9, 10, 11, 12, 13, 14, 15, 16,
    17, 18, 19, 20, 21, 22, 23, 24,
    25, 26, 27, 28, 29, 30, 31, 32,
    33, 34, 35, 36, 37, 38, 39, 40,
    1, 2, 3, 4, 5, 6, 7, 8,
    9, 10, 11, 12, 13, 14, 15, 16,
    17, 18, 1, 2, 3, 4, 5, 1,
    2, 3, 4, 1, 2, 3, 1, 2,
    3, 1, 2, 3, 1, 2, 1, 2,
    1, 2, 1, 2, 1, 2, 1, 2,
    1, 2, 1, 2, 1, 2, 1, 2,
    1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1,
];

/// The row that is not a run and level but an escape: six bits, then the run
/// in six more and the level in twelve.
pub const ESCAPE: usize = 111;

/// The row that ends a block.
pub const END_OF_BLOCK: usize = 112;

/// The largest run and level the tables can name outright. Anything past
/// either is written as an escape.
pub const MAX_RUN: usize = 31;
pub const MAX_LEVEL: usize = 40;

/// Table B.1 -- how many macroblocks on from the last one this one is.
///
/// Indexed by the increment less one, so row 0 is "the next macroblock".
/// Row 33 is the escape, which adds 33 and is read again; row 34 is the
/// stuffing MPEG-2 forbids and MPEG-1 allowed; row 35 is not a code at all
/// but what the end of a slice looks like.
pub const ADDRESS_INCREMENT: [Code; 36] = [
    (0x01, 1), (0x03, 3), (0x02, 3), (0x03, 4),
    (0x02, 4), (0x03, 5), (0x02, 5), (0x07, 7),
    (0x06, 7), (0x0b, 8), (0x0a, 8), (0x09, 8),
    (0x08, 8), (0x07, 8), (0x06, 8), (0x17, 10),
    (0x16, 10), (0x15, 10), (0x14, 10), (0x13, 10),
    (0x12, 10), (0x23, 11), (0x22, 11), (0x21, 11),
    (0x20, 11), (0x1f, 11), (0x1e, 11), (0x1d, 11),
    (0x1c, 11), (0x1b, 11), (0x1a, 11), (0x19, 11),
    (0x18, 11), (0x08, 11), (0x0f, 11), (0x00, 8),
];

/// Table B.9 -- which of the six blocks of a 4:2:0 macroblock are coded.
///
/// Indexed by the pattern itself, high bit first: bit 5 is the first luma
/// block, bit 0 the second chroma one. Row 0 is not a pattern a 4:2:0
/// macroblock can carry -- a macroblock that codes nothing says so in its
/// type instead -- and is here to keep the table square.
pub const CODED_BLOCK_PATTERN: [Code; 64] = [
    (0x01, 9), (0x0b, 5), (0x09, 5), (0x0d, 6),
    (0x0d, 4), (0x17, 7), (0x13, 7), (0x1f, 8),
    (0x0c, 4), (0x16, 7), (0x12, 7), (0x1e, 8),
    (0x13, 5), (0x1b, 8), (0x17, 8), (0x13, 8),
    (0x0b, 4), (0x15, 7), (0x11, 7), (0x1d, 8),
    (0x11, 5), (0x19, 8), (0x15, 8), (0x11, 8),
    (0x0f, 6), (0x0f, 8), (0x0d, 8), (0x03, 9),
    (0x0f, 5), (0x0b, 8), (0x07, 8), (0x07, 9),
    (0x0a, 4), (0x14, 7), (0x10, 7), (0x1c, 8),
    (0x0e, 6), (0x0e, 8), (0x0c, 8), (0x02, 9),
    (0x10, 5), (0x18, 8), (0x14, 8), (0x10, 8),
    (0x0e, 5), (0x0a, 8), (0x06, 8), (0x06, 9),
    (0x12, 5), (0x1a, 8), (0x16, 8), (0x12, 8),
    (0x0d, 5), (0x09, 8), (0x05, 8), (0x05, 9),
    (0x0c, 5), (0x08, 8), (0x04, 8), (0x04, 9),
    (0x07, 3), (0x0a, 5), (0x08, 5), (0x0c, 6),
];

/// Table B.10 -- the coarse half of a motion vector difference. What is left
/// of it follows in `f_code - 1` bits, unless the code is zero.
pub const MOTION_CODE: [Code; 17] = [
    (0x01, 1), (0x01, 2), (0x01, 3), (0x01, 4),
    (0x03, 6), (0x05, 7), (0x04, 7), (0x03, 7),
    (0x0b, 9), (0x0a, 9), (0x09, 9), (0x11, 10),
    (0x10, 10), (0x0f, 10), (0x0e, 10), (0x0d, 10),
    (0x0c, 10),
];

/// Table B.12 -- how many bits the DC difference of a luma block takes.
pub const DC_SIZE_LUMA: [Code; 12] = [
    (0x04, 3), (0x00, 2), (0x01, 2), (0x05, 3),
    (0x06, 3), (0x0e, 4), (0x1e, 5), (0x3e, 6),
    (0x7e, 7), (0xfe, 8), (0x1fe, 9), (0x1ff, 9),
];

/// Table B.13 -- the same for a chroma block.
pub const DC_SIZE_CHROMA: [Code; 12] = [
    (0x00, 2), (0x01, 2), (0x02, 2), (0x06, 3),
    (0x0e, 4), (0x1e, 5), (0x3e, 6), (0x7e, 7),
    (0xfe, 8), (0x1fe, 9), (0x3fe, 10), (0x3ff, 10),
];

/// Table 7-6 -- what a quantiser scale code is worth where `q_scale_type` is
/// 1, which is what a broadcast uses.
///
/// The codes run 1..31 either way. Where this table is not in use the code is
/// worth twice itself, so the scale climbs in steps of two to 62; through
/// here it starts finer and ends far coarser, at 112.
pub const NON_LINEAR_QUANTISER: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7,
    8, 10, 12, 14, 16, 18, 20, 22,
    24, 28, 32, 36, 40, 44, 48, 52,
    56, 64, 72, 80, 88, 96, 104, 112,
];

/// What a macroblock's type says it carries.
pub mod flags {
    pub const QUANT: u8 = 0x10;
    pub const FORWARD: u8 = 0x08;
    pub const BACKWARD: u8 = 0x04;
    pub const PATTERN: u8 = 0x02;
    pub const INTRA: u8 = 0x01;
}

/// A macroblock type: the code, its length, and what it says the macroblock
/// carries.
pub type MbType = (u16, u8, u8);

/// Table B.2 -- an I picture's macroblocks are intra, and say only whether
/// they carry a quantiser scale of their own.
pub const MB_TYPE_I: [MbType; 2] = [
    (0x01, 1, 0x01),
    (0x01, 2, 0x11),
];

/// Table B.3 -- a P picture's.
pub const MB_TYPE_P: [MbType; 7] = [
    (0x03, 5, 0x01),
    (0x01, 2, 0x02),
    (0x01, 3, 0x08),
    (0x01, 1, 0x0a),
    (0x01, 6, 0x11),
    (0x01, 5, 0x12),
    (0x02, 5, 0x1a),
];

/// Table B.4 -- a B picture's.
pub const MB_TYPE_B: [MbType; 11] = [
    (0x03, 5, 0x01),
    (0x02, 3, 0x04),
    (0x03, 3, 0x06),
    (0x02, 4, 0x08),
    (0x03, 4, 0x0a),
    (0x02, 2, 0x0c),
    (0x03, 2, 0x0e),
    (0x01, 6, 0x11),
    (0x02, 6, 0x16),
    (0x03, 6, 0x1a),
    (0x02, 5, 0x1e),
];

