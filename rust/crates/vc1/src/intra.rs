//! Writing pictures that reference nothing.
//!
//! A smart cut re-encodes the fragment of a GOP at each end of a range and
//! copies everything between. The fragment is short -- a few dozen pictures
//! at most -- and it is spliced in front of a copy of the recording's own
//! bytes, so nothing outside it may be referenced. That means every picture
//! written here can be an intra picture, and an encoder that only writes
//! intra pictures is a tenth of the format:
//!
//! * no motion estimation, no motion vectors, and none of the syntax that
//!   carries them -- which is most of a VC-1 picture header;
//! * the bit planes that would say which macroblock is coded how are written
//!   in the format's raw mode, a bit per macroblock in the macroblock layer,
//!   so none of the six ways of packing a bit plane has to be produced;
//! * the sequence and entry-point headers are the recording's own, restated
//!   in front of each picture, so the copied pictures across the splice are
//!   decoded against exactly the parameters they were coded with.
//!
//! What it costs is bits: an intra picture is several times the size of the
//! predicted one it replaces. Over a fragment of a GOP that is a fraction of
//! a second of the output, which is a trade worth making to keep the rest of
//! the recording untouched.

use crate::bits::{escape, Writer};
use crate::headers::{Coding, Quantizer};
use crate::tables;
use crate::transform::{forward, Quant};
use crate::Shape;

/// What a stream can be that this encoder cannot write for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// Pan-scan windows, whose syntax sits in the middle of the picture
    /// header and varies in length.
    PanScan,
    /// A quantizer that varies within the picture at the edges, which would
    /// have to be mirrored macroblock by macroblock.
    EdgeQuantizer,
    /// Chroma at anything but 4:2:0, which the format does not have.
    ChromaFormat,
    /// A picture too large or too small to be a picture.
    Size(u32, u32),
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Unsupported::PanScan => write!(f, "the stream carries pan-scan windows"),
            Unsupported::EdgeQuantizer => {
                write!(f, "the stream varies its quantizer at the picture edges")
            }
            Unsupported::ChromaFormat => write!(f, "the stream is not 4:2:0"),
            Unsupported::Size(w, h) => write!(f, "a picture of {w}x{h} is not one this can write"),
        }
    }
}

impl std::error::Error for Unsupported {}

/// One plane of a picture, as it sits in memory.
pub struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
    pub width: usize,
    pub height: usize,
}

impl Plane<'_> {
    /// A sample, less the 128 the decoder adds back, with the edge repeated
    /// beyond the picture.
    ///
    /// A macroblock is sixteen samples square and a picture's height need
    /// not be a multiple of that -- 1080 is not -- so the bottom row of
    /// macroblocks reaches past the last line there is. What is written
    /// there is never displayed; repeating the edge simply keeps it cheap to
    /// code.
    fn at(&self, x: usize, y: usize) -> i16 {
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        self.data[y * self.stride + x] as i16 - 128
    }
}

/// A picture to encode, and how it is to be shown.
pub struct Frame<'a> {
    pub y: Plane<'a>,
    pub u: Plane<'a>,
    pub v: Plane<'a>,
    /// Show the top field first.
    pub tff: bool,
    /// Show the first field again after the second.
    pub rff: bool,
    /// Show the whole frame this many extra times.
    pub rptfrm: u8,
}

/// The AC codes of one coding set, arranged for writing rather than reading.
struct AcCodes {
    set: usize,
    /// Which index says a given run, level and end-of-block, or `NONE`.
    lookup: Vec<u16>,
}

const NONE: u16 = u16::MAX;

impl AcCodes {
    fn new(set: usize) -> Self {
        let mut lookup = vec![NONE; 2 * 64 * 256];
        for index in 0..tables::AC_SIZES[set] - 1 {
            let (run, level) = tables::AC_RUN_LEVEL[set][index];
            let last = index >= tables::AC_LAST_INDEX[set];
            lookup[Self::key(last, run, level)] = index as u16;
        }
        Self { set, lookup }
    }

    fn key(last: bool, run: u8, level: u8) -> usize {
        (last as usize) << 14 | (run as usize) << 8 | level as usize
    }

    /// The index that says exactly this, if the table has one.
    fn find(&self, last: bool, run: u8, level: u8) -> Option<usize> {
        match self.lookup[Self::key(last, run, level)] {
            NONE => None,
            index => Some(index as usize),
        }
    }

    fn write(&self, w: &mut Writer, index: usize) {
        let (code, bits) = tables::AC_CODE[self.set][index];
        w.vlc(code, bits);
    }

    fn escape(&self, w: &mut Writer) {
        self.write(w, tables::AC_SIZES[self.set] - 1);
    }
}

/// How wide the run and level of an escaped coefficient are written.
///
/// Declared once per picture, by the first coefficient that has to escape,
/// and these are the widest the syntax allows: sixty-three is the longest
/// run a block can hold, and a level is never written in more than eight
/// bits anyway.
const ESCAPE_RUN_BITS: usize = 6;
const ESCAPE_LEVEL_BITS: usize = 8;

/// An encoder for one stream's intra pictures.
pub struct Encoder {
    shape: Shape,
    width: u32,
    height: u32,
    mb_width: usize,
    mb_height: usize,
    /// The index the picture header states, and the step it stands for.
    pqindex: u8,
    quant: Quant,
    luma: AcCodes,
    chroma: AcCodes,
    /// Which of the two DC tables the picture header names.
    dc_table: usize,
    /// Whether pictures are coded as interlaced frames.
    coding: Coding,
    scan: &'static [usize; 64],
}

impl Encoder {
    /// Build an encoder that writes pictures the given stream's decoder will
    /// take, at the given quantizer step.
    ///
    /// `step` is PQUANT: 1 is nearly lossless and enormous, 31 is coarse.
    /// Steps below 3 are refused rather than written badly -- the DC
    /// differential grows extra bits there, and a fragment of a GOP is not
    /// worth spending them on.
    pub fn new(shape: &Shape, width: u32, height: u32, step: u8) -> Result<Self, Unsupported> {
        if shape.entry.panscan {
            return Err(Unsupported::PanScan);
        }
        if shape.entry.dquant == 2 {
            return Err(Unsupported::EdgeQuantizer);
        }
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err(Unsupported::Size(width, height));
        }
        let step = step.clamp(3, 31);
        // The index and the step are the same number except where the
        // entry-point header left the choice to the picture, and there the
        // low indices mean something else. See [`tables::PQUANT`].
        let (pqindex, uniform) = match shape.entry.quantizer {
            Quantizer::Implicit => (step.max(9), false),
            Quantizer::Explicit => (step, false),
            Quantizer::NonUniform => (step, false),
            Quantizer::Uniform => (step, true),
        };
        let row = match shape.entry.quantizer {
            Quantizer::Implicit => 0,
            _ => 1,
        };
        let quant = Quant::new(tables::PQUANT[row][pqindex as usize] as i32, uniform);
        // Table zero of each pair, whose meaning depends on how fine the
        // quantizer is. Naming one of the others would cost a table apiece
        // and save a few percent of a fragment.
        let (luma, chroma) = if pqindex <= 8 {
            (AcCodes::new(6), AcCodes::new(7)) // high rate
        } else {
            (AcCodes::new(2), AcCodes::new(3)) // low motion
        };
        let coding = if shape.sequence.interlace {
            Coding::FrameInterlace
        } else {
            Coding::Progressive
        };
        Ok(Encoder {
            shape: shape.clone(),
            width,
            height,
            mb_width: (width as usize + 15) / 16,
            mb_height: (height as usize + 15) / 16,
            pqindex,
            quant,
            luma,
            chroma,
            dc_table: 1,
            coding,
            scan: match coding {
                Coding::FrameInterlace => &tables::SCAN_INTERLACED,
                _ => &tables::SCAN_PROGRESSIVE,
            },
        })
    }

    /// The quantizer step pictures are written at.
    pub fn step(&self) -> i32 {
        self.quant.step
    }

    /// The picture size this was built for.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Encode one picture, headers and all.
    ///
    /// What comes back is a complete access point: the recording's own
    /// sequence and entry-point headers, then the picture. Restating the
    /// first two in front of every picture is what the discs themselves do,
    /// and it means a cut can begin at any picture this wrote.
    pub fn encode(&self, frame: &Frame) -> Vec<u8> {
        let mut w = Writer::new();
        self.picture_header(&mut w, frame);
        let mut state = Running::new(self);
        for mb_y in 0..self.mb_height {
            for mb_x in 0..self.mb_width {
                self.macroblock(&mut w, &mut state, frame, mb_x, mb_y);
            }
        }
        let mut out = Vec::with_capacity(w.position() / 8 + 64);
        out.extend_from_slice(&self.shape.sequence_bdu);
        out.extend_from_slice(&self.shape.entry_bdu);
        out.extend_from_slice(&[0, 0, 1, crate::headers::FRAME]);
        out.extend_from_slice(&escape(&w.finish()));
        out
    }

    fn picture_header(&self, w: &mut Writer, frame: &Frame) {
        let seq = &self.shape.sequence;
        if seq.interlace {
            // FCM: an interlaced frame, rather than a progressive one or a
            // pair of fields.
            w.u012(1);
        }
        w.u(0b110, 3); // PTYPE: intra
        if seq.tfcntr {
            w.u(0, 8);
        }
        if seq.pulldown {
            if !seq.interlace || seq.psf {
                w.u(frame.rptfrm as u32, 2);
            } else {
                w.bit1(frame.tff);
                w.bit1(frame.rff);
            }
        }
        // RNDCTRL and UVSAMP both describe how pictures are predicted from
        // one another, which an intra picture never is. They are written
        // because the syntax has a place for them.
        w.bit1(true);
        if seq.interlace {
            w.bit1(true);
        }
        // INTERPFRM, likewise: it says whether a decoder is invited to
        // interpolate a picture between this one and the next, and the
        // syntax carries it on progressive pictures wherever the sequence
        // header said pictures might. Only a stream that sets FINTERPFLAG
        // has the bit at all, and leaving it out of one that does shifts
        // everything after it by one place.
        if self.coding == Coding::Progressive && seq.finterp {
            w.bit1(false);
        }
        w.u(self.pqindex as u32, 5);
        if self.pqindex < 9 {
            w.bit1(false); // HALFQP
        }
        if self.shape.entry.quantizer == Quantizer::Explicit {
            w.bit1(self.quant.uniform);
        }
        if seq.postproc {
            w.u(0, 2);
        }
        // The bit planes. Raw means the bits are written in the macroblock
        // layer instead, one per macroblock, which is a byte of header
        // against a bit per macroblock -- a bad trade in general and a fine
        // one for a fragment nobody will ever store.
        if self.coding == Coding::FrameInterlace {
            raw_bitplane(w); // FIELDTX
        }
        raw_bitplane(w); // ACPRED
        if self.shape.entry.overlap && self.quant.step <= 8 {
            w.u012(0); // CONDOVER: no overlap smoothing
        }
        w.u012(0); // TRANSACFRM, which is the chroma table
        w.u012(0); // TRANSACFRM2, the luma one
        w.bit1(self.dc_table == 1);
        if self.shape.entry.dquant != 0 {
            w.bit1(false); // DQUANTFRM: one quantizer for the whole picture
        }
    }

    fn macroblock(&self, w: &mut Writer, state: &mut Running, frame: &Frame, x: usize, y: usize) {
        // Quantise all six blocks first: which of them have anything in them
        // is written before any of them is.
        let mut blocks: [Block; 6] = Default::default();
        for (k, block) in blocks.iter_mut().enumerate() {
            *block = self.quantise(frame, x, y, k);
        }

        if self.coding == Coding::FrameInterlace {
            w.bit1(false); // FIELDTX: transform this macroblock as a frame
        }
        // The pattern says which blocks carry AC coefficients. The four
        // luma bits are written against a prediction from the blocks around
        // them, so the prediction has to be worked out in the same order the
        // decoder will.
        let mut pattern = 0u32;
        for (k, block) in blocks.iter().enumerate() {
            let bit = if k < 4 {
                let predicted = state.coded_prediction(x, y, k);
                state.set_coded(x, y, k, block.coded);
                block.coded ^ predicted
            } else {
                block.coded
            };
            pattern |= (bit as u32) << (5 - k);
        }
        let (code, bits) = tables::MB_INTRA_CBP[pattern as usize];
        w.vlc(code as u32, bits);
        w.bit1(false); // ACPRED: predict no AC coefficients

        for (k, block) in blocks.iter().enumerate() {
            self.write_block(w, state, block, x, y, k);
        }
    }

    /// Transform and quantise one of a macroblock's six blocks.
    fn quantise(&self, frame: &Frame, x: usize, y: usize, k: usize) -> Block {
        let (plane, px, py) = match k {
            0..=3 => (&frame.y, x * 16 + (k & 1) * 8, y * 16 + (k >> 1) * 8),
            4 => (&frame.u, x * 8, y * 8),
            _ => (&frame.v, x * 8, y * 8),
        };
        let mut samples = [0i16; 64];
        for row in 0..8 {
            for col in 0..8 {
                samples[row * 8 + col] = plane.at(px + col, py + row);
            }
        }
        let coefficients = forward(&samples);
        let mut block = Block {
            dc: self.quant.dc_level(coefficients[0]),
            ..Default::default()
        };
        for i in 1..64 {
            let level = self.quant.level(coefficients[self.scan[i]]);
            block.ac[i] = level;
            if level != 0 {
                block.coded = true;
                block.last = i;
            }
        }
        block
    }

    fn write_block(
        &self,
        w: &mut Writer,
        state: &mut Running,
        block: &Block,
        x: usize,
        y: usize,
        k: usize,
    ) {
        // The DC is written as a difference from a neighbour, and is written
        // whether or not the block holds anything else.
        let predicted = state.dc_prediction(x, y, k);
        // A difference is written in eight bits at most, so a level further
        // than that from its prediction is written as near as can be said.
        // What the decoder will hold is what it was told, not what was
        // meant, and the two have to agree or every block after this one is
        // predicted from a different number.
        let difference = (block.dc - predicted).clamp(-255, 255);
        state.set_dc(x, y, k, predicted + difference);
        self.write_dc(w, difference, k >= 4);
        if !block.coded {
            return;
        }
        let codes = if k < 4 { &self.luma } else { &self.chroma };
        let mut run = 0u8;
        for i in 1..64 {
            let level = block.ac[i];
            if level == 0 {
                run += 1;
                continue;
            }
            let magnitude = level.unsigned_abs().min(255) as u8;
            let last = i == block.last;
            match codes.find(last, run, magnitude) {
                Some(index) => {
                    codes.write(w, index);
                    w.bit1(level < 0);
                }
                None => {
                    // Nothing in the table says this, so it is written out
                    // in full. The syntax has three ways of doing that; this
                    // is the one that can say anything at all.
                    codes.escape(w);
                    w.u(0, 2); // the third escape mode
                    w.bit1(last);
                    if !state.escaped {
                        state.escaped = true;
                        self.write_escape_widths(w);
                    }
                    w.u(run as u32, ESCAPE_RUN_BITS);
                    w.bit1(level < 0);
                    w.u(magnitude as u32, ESCAPE_LEVEL_BITS);
                }
            }
            run = 0;
            if last {
                break;
            }
        }
    }

    /// Say how wide an escaped coefficient's run and level are written.
    ///
    /// Said once per picture, by the first coefficient that needs it, and in
    /// one of two ways depending on how fine the quantizer is -- the format
    /// spends fewer bits saying it where a coarse quantizer makes escapes
    /// rare.
    fn write_escape_widths(&self, w: &mut Writer) {
        if self.quant.step < 8 {
            w.u(0, 3); // an escape into the wider form
            w.u(0, 2); // ...which means eight
        } else {
            w.u(0, 6); // six zeroes, i.e. six plus two
        }
        w.u(ESCAPE_RUN_BITS as u32 - 3, 2);
    }

    fn write_dc(&self, w: &mut Writer, difference: i32, chroma: bool) {
        let table = &tables::DC_CODE[self.dc_table][chroma as usize];
        let magnitude = difference.unsigned_abs();
        if magnitude == 0 {
            let (code, bits) = table[0];
            w.vlc(code, bits);
            return;
        }
        if magnitude < 119 {
            let (code, bits) = table[magnitude as usize];
            w.vlc(code, bits);
        } else {
            let (code, bits) = table[119]; // the escape
            w.vlc(code, bits);
            w.u(magnitude, 8);
        }
        w.bit1(difference < 0);
    }
}

/// One block's worth of quantised coefficients.
#[derive(Clone, Copy)]
struct Block {
    /// The DC level, before it is written as a difference.
    dc: i32,
    /// AC levels in scan order; index 0 is unused.
    ac: [i32; 64],
    /// Whether any AC level is non-zero.
    coded: bool,
    /// The scan index of the last non-zero level.
    last: usize,
}

impl Default for Block {
    fn default() -> Self {
        Block {
            dc: 0,
            ac: [0; 64],
            coded: false,
            last: 0,
        }
    }
}

/// What one picture accumulates as it is written.
///
/// Both the coded-block flags and the DC levels are predicted from
/// neighbours, so the encoder has to keep the same running picture of them
/// that the decoder will build.
struct Running {
    /// Coded flags on the grid of 8x8 luma blocks, with a border.
    coded: Vec<u8>,
    /// DC levels: luma on its own grid, chroma on one each. All bordered.
    dc_luma: Vec<i32>,
    dc_chroma: [Vec<i32>; 2],
    luma_stride: usize,
    chroma_stride: usize,
    /// Whether the widths of an escaped coefficient have been stated yet.
    escaped: bool,
}

impl Running {
    fn new(enc: &Encoder) -> Self {
        let luma_stride = enc.mb_width * 2 + 1;
        let chroma_stride = enc.mb_width + 1;
        Running {
            coded: vec![0; luma_stride * (enc.mb_height * 2 + 1)],
            dc_luma: vec![0; luma_stride * (enc.mb_height * 2 + 1)],
            dc_chroma: [
                vec![0; chroma_stride * (enc.mb_height + 1)],
                vec![0; chroma_stride * (enc.mb_height + 1)],
            ],
            luma_stride,
            chroma_stride,
            escaped: false,
        }
    }

    /// Where a block sits on its plane's grid, counting the border in.
    fn at(&self, x: usize, y: usize, k: usize) -> usize {
        if k < 4 {
            let bx = x * 2 + (k & 1) + 1;
            let by = y * 2 + (k >> 1) + 1;
            by * self.luma_stride + bx
        } else {
            (y + 1) * self.chroma_stride + x + 1
        }
    }

    fn stride(&self, k: usize) -> usize {
        if k < 4 {
            self.luma_stride
        } else {
            self.chroma_stride
        }
    }

    fn dc_plane(&self, k: usize) -> &Vec<i32> {
        match k {
            0..=3 => &self.dc_luma,
            4 => &self.dc_chroma[0],
            _ => &self.dc_chroma[1],
        }
    }

    fn dc_plane_mut(&mut self, k: usize) -> &mut Vec<i32> {
        match k {
            0..=3 => &mut self.dc_luma,
            4 => &mut self.dc_chroma[0],
            _ => &mut self.dc_chroma[1],
        }
    }

    /// Whether the block above, and the one to the left, are inside the
    /// picture and already written.
    fn neighbours(x: usize, y: usize, k: usize) -> (bool, bool) {
        let above = y > 0 || k == 2 || k == 3;
        let left = x > 0 || k == 1 || k == 3;
        (above, left)
    }

    /// The coded flag a decoder will assume for this block.
    fn coded_prediction(&self, x: usize, y: usize, k: usize) -> bool {
        let i = self.at(x, y, k);
        let stride = self.luma_stride;
        let (a, b, c) = (
            self.coded[i - 1],
            self.coded[i - 1 - stride],
            self.coded[i - stride],
        );
        (if b == c { a } else { c }) != 0
    }

    fn set_coded(&mut self, x: usize, y: usize, k: usize, coded: bool) {
        let i = self.at(x, y, k);
        self.coded[i] = coded as u8;
    }

    /// The DC level a decoder will predict for this block.
    ///
    /// The one to the left, or the one above, whichever the gradient across
    /// the three neighbours suggests. Zero where neither is there.
    fn dc_prediction(&self, x: usize, y: usize, k: usize) -> i32 {
        let (above, left) = Self::neighbours(x, y, k);
        let i = self.at(x, y, k);
        let stride = self.stride(k);
        let plane = self.dc_plane(k);
        let (c, b, a) = (plane[i - 1], plane[i - 1 - stride], plane[i - stride]);
        if left && (!above || (a - b).abs() <= (b - c).abs()) {
            c
        } else if above {
            a
        } else {
            0
        }
    }

    fn set_dc(&mut self, x: usize, y: usize, k: usize, level: i32) {
        let i = self.at(x, y, k);
        self.dc_plane_mut(k)[i] = level;
    }
}

/// Say that a bit plane's bits are written in the macroblock layer.
fn raw_bitplane(w: &mut Writer) {
    w.bit1(false); // INVERT, which raw mode ignores
    w.u(0, 4); // IMODE: raw
}
