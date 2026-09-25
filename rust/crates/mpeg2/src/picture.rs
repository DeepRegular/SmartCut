//! Reading a coded picture apart, and writing it back with less in it.
//!
//! What is taken apart here is only as much as the job needs. A macroblock's
//! address, its type, its motion vectors and its DC difference are read to
//! find out how long they are, and are then written back as the very bits
//! they arrived as: none of them means anything different at a coarser
//! quantiser. The coefficients are the exception, and they are the whole
//! point -- those are decoded to a place in the block and a level, and then
//! written again more cheaply.
//!
//! Two things make them cheaper, and both are in [`Squeeze`]:
//!
//! - **The scale is lifted.** Every level is divided down to a coarser step,
//!   which is what a requantiser does and what the quantiser scale code in
//!   the bitstream is for.
//! - **The tail of a block is dropped.** A block's last few coefficients are
//!   its dearest -- each one is a code of its own, and they are what keeps
//!   the end-of-block from coming sooner -- and they are also its smallest.
//!   Where what they are worth is less than what they cost, they go.
//!
//! The second does most of the work. Measured against the reference material,
//! reaching three quarters of the size by lifting the scale alone costs four
//! decibels more than reaching it by dropping what is not worth its bits --
//! and the tool that material came out of is doing the same thing: at three
//! quarters of the size it has raised the scale by two thirds of one code and
//! thrown away two coefficients in five.
//!
//! **Nothing here reconstructs a picture.** There is no inverse transform and
//! no motion compensation, which is what makes this fast and also what
//! decides its one cost. See the crate docs.

use crate::bits::{Reader, Sink};
use crate::quant::{phase, Map, Quantiser};
use crate::scan;
use crate::tables::{self, flags};
use crate::vlc::{self, Tables};
use crate::Error;

/// How hard to press on a picture.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Squeeze {
    /// How many codes coarser to write the quantiser scale. Fractional: the
    /// part after the point is the share of macroblocks written one code
    /// coarser still. See [`crate::quant::Map`].
    pub lift: f64,
    /// What one bit is worth, measured against what a coefficient is worth.
    ///
    /// A block's tail goes where the sum of its coefficients' weights squared
    /// is less than this times the bits they take. Zero keeps everything. The
    /// scale the macroblock is written at falls out of both sides, so this
    /// means the same thing in a finely quantised macroblock as in a coarse
    /// one -- which is what keeps the recording's own sharing out of
    /// precision between them.
    pub thin: f64,
}

impl Squeeze {
    /// Whether this would leave the picture exactly as it arrived.
    pub fn is_nothing(&self) -> bool {
        self.lift <= 0.0 && self.thin <= 0.0
    }
}

/// What the sequence header says that a slice cannot be read without.
#[derive(Clone, Copy, Debug)]
pub struct Shape {
    /// 1 is 4:2:0, which is the only one this rewrites. See [`Error::Chroma`].
    pub chroma_format: u8,
    /// The full height, which decides whether a slice carries three more bits
    /// of vertical position. Only above 2800 does it, so only a picture
    /// taller than any broadcast or disc.
    pub vertical_size: u32,
    /// The quantisation matrices, by place in the block rather than in the
    /// order they were sent. What the dropping of a coefficient is weighed
    /// against.
    pub intra: [u8; 64],
    pub non_intra: [u8; 64],
}

impl Default for Shape {
    fn default() -> Self {
        Self {
            chroma_format: 1,
            vertical_size: 1080,
            intra: scan::unscan(&scan::DEFAULT_INTRA),
            non_intra: scan::unscan(&scan::DEFAULT_NON_INTRA),
        }
    }
}

/// What a picture says about how its macroblocks are coded.
#[derive(Clone, Copy, Debug, Default)]
struct Head {
    coding_type: u8,
    f_code: [[u32; 2]; 2],
    picture_structure: u32,
    frame_pred_frame_dct: bool,
    concealment_motion_vectors: bool,
    q_scale_type: bool,
    intra_vlc_format: bool,
    alternate_scan: bool,
    /// Whether a picture coding extension turned up. MPEG-1 has none, and
    /// MPEG-1 is not rewritten here: it has a stuffing code in the macroblock
    /// layer and a different idea of what a slice may carry.
    extended: bool,
}

/// One piece of the picture on its way out: either bytes that are written
/// back unchanged, or a slice that is rewritten.
enum Part {
    Bytes(u32, u32),
    Slice(u32),
}

struct Slice {
    /// Everything between the start code and the quantiser scale code:
    /// nothing, usually, and three bits of vertical position on a picture
    /// taller than 2800.
    header: (u32, u32),
    quant: u8,
    /// The slice's own extra information, and the bit that says there is no
    /// more of it.
    extra: (u32, u32),
    /// Which of [`Picture::mbs`] are this slice's.
    mbs: (u32, u32),
    /// The bit the macroblocks stop at, which is where the zeroes that pad
    /// the slice out to a byte begin.
    end: u32,
    /// The byte the slice runs to: the next start code, or the end of the
    /// picture. Never before [`Slice::end`] rounded up, and sometimes after
    /// it -- the standard lets a slice be followed by whole bytes of zero,
    /// and an encoder that uses them is padding for reasons of its own. They
    /// are written again, so that a picture nothing asked to change comes
    /// back as itself.
    until: u32,
    q_scale_type: bool,
    intra_vlc_format: bool,
    alternate_scan: bool,
}

impl Slice {
    /// How many whole zero bytes followed this slice's macroblocks.
    fn stuffing(&self) -> u32 {
        self.until.saturating_sub(self.end.div_ceil(8))
    }
}

/// What a macroblock is written against.
///
/// The slice it belongs to, what its quantiser scale codes become at the
/// pressure the picture is being written under, and the pressure itself.
/// Three things that are the same for every macroblock of a slice, handed
/// over as one.
struct Pen<'a> {
    slice: &'a Slice,
    map: &'a Map,
    press: Squeeze,
}

struct Mb {
    /// The address increment, the type, and the mode bits after it.
    pre: (u32, u32),
    /// The quantiser scale code this macroblock carries, where it carries
    /// one. Rewritten in place: the type it belongs to is not touched, so a
    /// macroblock that had a scale of its own still has one, and one that did
    /// not still runs on the slice's.
    quant: Option<u8>,
    /// Motion vectors, and the marker after a concealment vector.
    mid: (u32, u32),
    flags: u8,
    /// The quantiser scale code in force here, whether this macroblock
    /// carries it or an earlier one did.
    q: u8,
    /// Which of [`Picture::blocks`] are this macroblock's.
    blocks: (u32, u32),
}

struct Block {
    /// Which of the six this is, so the pattern can be written again when
    /// some of them turn out to hold nothing.
    which: u8,
    /// The DC size and difference of an intra block, left exactly as it
    /// arrived: the DC of an intra block is quantised by the picture's own
    /// precision and not by the scale being changed here, and it is coded as
    /// a difference from the block before, so changing one would mean
    /// changing every block after it.
    dc: (u32, u32),
    coeffs: (u32, u32),
}

/// One coefficient: where it is in the scan, and what it is worth.
///
/// Where, rather than how far from the last one. The run is what the
/// bitstream carries, but it is the place that stays put when a coefficient
/// earlier in the block is thrown away, and the place is what the
/// quantisation matrix is indexed by.
#[derive(Clone, Copy)]
struct Coeff {
    at: u8,
    level: i16,
}

/// A picture read apart, ready to be written back under any pressure.
///
/// Reading it apart is the expensive half, and the rate control writes the
/// same picture more than once while it looks for the size it wants, so the
/// two are separate: [`Picture::read`] once, then [`Picture::measure`] as
/// often as needed and [`Picture::write`] at the end.
pub struct Picture<'a> {
    data: &'a [u8],
    parts: Vec<Part>,
    slices: Vec<Slice>,
    mbs: Vec<Mb>,
    blocks: Vec<Block>,
    coeffs: Vec<Coeff>,
    /// What each place in the scan weighs, by scan and by whether the block
    /// is intra. Both scans are worked out because a packet can hold two
    /// pictures and they need not agree.
    weights: [[[u16; 64]; 2]; 2],
}

/// Scratch the emit reuses, so that a picture written five times does not
/// allocate five times.
#[derive(Default)]
pub struct Scratch {
    coeffs: Vec<Coeff>,
    bits: Vec<u32>,
    bounds: Vec<(u8, u32, u32)>,
}

impl<'a> Picture<'a> {
    /// Take a picture apart. `shape` is what the sequence header last said; a
    /// packet that carries one of its own updates it.
    pub fn read(data: &'a [u8], shape: &mut Shape) -> Result<Self, Error> {
        let t = vlc::tables();
        let mut pic = Picture {
            data,
            parts: Vec::new(),
            slices: Vec::new(),
            mbs: Vec::new(),
            blocks: Vec::new(),
            coeffs: Vec::new(),
            weights: [[[0; 64]; 2]; 2],
        };
        let starts = start_codes(data);
        if starts.is_empty() {
            return Err(Error::NotAPicture);
        }
        let mut head: Option<Head> = None;
        let mut copied = 0u32;
        for (k, &(at, code)) in starts.iter().enumerate() {
            let until = starts.get(k + 1).map_or(data.len(), |&(p, _)| p) as u32;
            match code {
                // A slice. Everything up to and including its start code goes
                // back unchanged; the rest of it is rewritten.
                0x01..=0xaf => {
                    let head = head.ok_or(Error::NoPictureHeader)?;
                    if !head.extended {
                        return Err(Error::NotAPicture);
                    }
                    pic.parts.push(Part::Bytes(copied, at as u32 + 4));
                    let index = pic.slices.len() as u32;
                    pic.read_slice(t, (at as u32 + 4) * 8, until * 8, &head, shape)?;
                    pic.parts.push(Part::Slice(index));
                    copied = until;
                }
                0x00 => head = Some(picture_head(data, at)?),
                0xb3 => sequence_header(data, at, shape)?,
                0xb5 => {
                    let mut r = Reader::at(data, (at + 4) * 8);
                    match r.u(4) {
                        1 => sequence_extension(&mut r, shape),
                        3 => quant_matrix_extension(&mut r, shape),
                        5 => return Err(Error::Scalable),
                        8 => {
                            let h = head.as_mut().ok_or(Error::NoPictureHeader)?;
                            picture_coding_extension(&mut r, h);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if pic.slices.is_empty() {
            return Err(Error::NotAPicture);
        }
        pic.parts.push(Part::Bytes(copied, data.len() as u32));
        for alternate in 0..2 {
            pic.weights[alternate][1] = scan::weights(&shape.intra, alternate == 1);
            pic.weights[alternate][0] = scan::weights(&shape.non_intra, alternate == 1);
        }
        Ok(pic)
    }

    /// How many bytes the picture arrived as.
    pub fn source_bytes(&self) -> usize {
        self.data.len()
    }

    /// The quantiser scale code each macroblock was written at.
    ///
    /// What a recording chose, or what something that rewrote it chose. Two
    /// files of the same recording, read side by side, say what the tool that
    /// made the second one did to the first -- which is how the behaviour of
    /// one was measured here.
    pub fn scales(&self) -> impl Iterator<Item = u8> + '_ {
        self.mbs.iter().map(|mb| mb.q)
    }

    /// How many coefficients the picture carries, and how many blocks they
    /// are spread over.
    ///
    /// What a rewrite did to a recording is mostly this: a coefficient that is
    /// not there any more is bits that are not there any more.
    pub fn coefficients(&self) -> (usize, usize) {
        (self.coeffs.len(), self.blocks.len())
    }

    /// Whether the scale can be lifted at all.
    ///
    /// A picture every macroblock of which is already at the coarsest code
    /// there is has nothing more to give that way -- though its coefficients
    /// can still be dropped.
    pub fn at_the_floor(&self, q: &Quantiser) -> bool {
        self.mbs.iter().all(|mb| q.at_the_top(mb.q))
    }

    /// What the picture would come to under this pressure, in bytes.
    pub fn measure(&self, q: &Quantiser, press: Squeeze, scratch: &mut Scratch) -> usize {
        let mut sink = crate::bits::Counter::new();
        self.emit(&mut sink, q, press, scratch);
        sink.bytes()
    }

    /// The picture, written back under this pressure.
    pub fn write(&self, q: &Quantiser, press: Squeeze, scratch: &mut Scratch) -> Vec<u8> {
        let mut sink = crate::bits::Writer::with_capacity(self.data.len() + 64);
        self.emit(&mut sink, q, press, scratch);
        sink.finish()
    }

    fn emit<S: Sink>(&self, sink: &mut S, q: &Quantiser, press: Squeeze, scratch: &mut Scratch) {
        for part in &self.parts {
            match *part {
                Part::Bytes(a, b) => {
                    sink.copy(self.data, a as usize * 8, (b - a) as usize * 8);
                }
                Part::Slice(i) => {
                    self.emit_slice(sink, &self.slices[i as usize], q, press, scratch)
                }
            }
        }
    }

    fn emit_slice<S: Sink>(
        &self,
        sink: &mut S,
        slice: &Slice,
        q: &Quantiser,
        press: Squeeze,
        scratch: &mut Scratch,
    ) {
        let map = q.map(slice.q_scale_type, press.lift);
        let (a, b) = slice.header;
        sink.copy(self.data, a as usize, (b - a) as usize);
        // The slice's own scale is the one its first macroblock runs on, so
        // it is mapped at that macroblock's phase.
        sink.u(u32::from(map.step(slice.quant, phase(slice.mbs.0))), 5);
        let (a, b) = slice.extra;
        sink.copy(self.data, a as usize, (b - a) as usize);
        let pen = Pen { slice, map: &map, press };
        // The phase of whichever wrote the scale in force: the slice, or the
        // last macroblock that carried a scale of its own. A macroblock that
        // carries none is decoded at that scale, so its coefficients have to
        // be divided down to it and not to the one its own phase would pick.
        let mut spread = phase(slice.mbs.0);
        for (k, mb) in self.mbs[slice.mbs.0 as usize..slice.mbs.1 as usize]
            .iter()
            .enumerate()
        {
            if mb.quant.is_some() {
                spread = phase(slice.mbs.0 + k as u32);
            }
            self.emit_mb(sink, mb, &pen, spread, scratch);
        }
        sink.align();
        for _ in 0..slice.stuffing() {
            sink.u(0, 8);
        }
    }

    fn emit_mb<S: Sink>(
        &self,
        sink: &mut S,
        mb: &Mb,
        pen: &Pen,
        spread: f32,
        scratch: &mut Scratch,
    ) {
        let (slice, map, press) = (pen.slice, pen.map, pen.press);
        let (a, b) = mb.pre;
        sink.copy(self.data, a as usize, (b - a) as usize);
        if let Some(code) = mb.quant {
            sink.u(u32::from(map.step(code, spread)), 5);
        }
        let (a, b) = mb.mid;
        sink.copy(self.data, a as usize, (b - a) as usize);

        let intra = mb.flags & flags::INTRA != 0;
        let ratio = map.ratio(mb.q, spread);
        let table = usize::from(intra && slice.intra_vlc_format);
        let weights = &self.weights[usize::from(slice.alternate_scan)][usize::from(intra)];
        let first = mb.blocks.0 as usize;

        // Divide the coefficients down, drop what is not worth its bits, and
        // only then write: a block with nothing left in it is dropped from
        // the pattern, and the pattern is written before the blocks are.
        scratch.coeffs.clear();
        scratch.bits.clear();
        scratch.bounds.clear();
        let mut pattern = 0u8;
        for block in &self.blocks[first..mb.blocks.1 as usize] {
            let from = scratch.coeffs.len() as u32;
            // An intra block has spent place zero on its DC, so its first
            // coefficient is a run of nothing when it sits at place one.
            let mut last = i32::from(intra) - 1;
            for c in &self.coeffs[block.coeffs.0 as usize..block.coeffs.1 as usize] {
                let level = ratio.apply(c.level, intra);
                if level == 0 {
                    continue;
                }
                let run = (i32::from(c.at) - last - 1).max(0) as u32;
                let bits = bits_of(run, level, table, !intra && last < 0);
                last = i32::from(c.at);
                scratch.coeffs.push(Coeff { at: c.at, level });
                scratch.bits.push(bits);
            }
            let keep = if press.thin > 0.0 {
                trim(
                    &scratch.coeffs[from as usize..],
                    &scratch.bits[from as usize..],
                    weights,
                    press.thin,
                )
            } else {
                scratch.coeffs.len() - from as usize
            };
            scratch.coeffs.truncate(from as usize + keep);
            let to = scratch.coeffs.len() as u32;
            if to > from {
                pattern |= 1 << (5 - block.which);
            }
            scratch.bounds.push((block.which, from, to));
        }

        // A macroblock whose type says it codes a pattern has to code
        // something. Where nothing was left of any of its blocks, the largest
        // coefficient of the lot is kept at the smallest level there is --
        // which costs a handful of bits and keeps the syntax true.
        if !intra && mb.flags & flags::PATTERN != 0 && pattern == 0 {
            if let Some((at, place, sign)) = self.biggest(mb) {
                let k = at - first;
                let (which, from, _) = scratch.bounds[k];
                scratch.coeffs.insert(
                    from as usize,
                    Coeff {
                        at: place,
                        level: sign,
                    },
                );
                scratch.bounds[k].2 += 1;
                for bound in &mut scratch.bounds[k + 1..] {
                    bound.1 += 1;
                    bound.2 += 1;
                }
                pattern |= 1 << (5 - which);
            }
        }

        if mb.flags & flags::PATTERN != 0 {
            let (code, bits) = tables::CODED_BLOCK_PATTERN[pattern as usize];
            sink.u(u32::from(code), u32::from(bits));
        }

        for (k, block) in self.blocks[first..mb.blocks.1 as usize].iter().enumerate() {
            let (which, from, to) = scratch.bounds[k];
            if !intra && (pattern >> (5 - which)) & 1 == 0 {
                continue;
            }
            let (a, b) = block.dc;
            sink.copy(self.data, a as usize, (b - a) as usize);
            emit_coeffs(
                sink,
                &scratch.coeffs[from as usize..to as usize],
                table,
                !intra,
            );
        }
    }

    /// The largest coefficient of a macroblock: which block it is in, where
    /// in that block it sits, and which way it points. What a macroblock that
    /// has to keep one coefficient keeps.
    fn biggest(&self, mb: &Mb) -> Option<(usize, u8, i16)> {
        let mut best: Option<(usize, u8, i16, i16)> = None;
        for at in mb.blocks.0 as usize..mb.blocks.1 as usize {
            let block = &self.blocks[at];
            for c in &self.coeffs[block.coeffs.0 as usize..block.coeffs.1 as usize] {
                let mag = c.level.abs();
                if best.is_none_or(|(_, _, _, b)| mag > b) {
                    best = Some((at, c.at, if c.level < 0 { -1 } else { 1 }, mag));
                }
            }
        }
        best.map(|(at, place, sign, _)| (at, place, sign))
    }

    fn read_slice(
        &mut self,
        t: &Tables,
        from: u32,
        until: u32,
        head: &Head,
        shape: &Shape,
    ) -> Result<(), Error> {
        let data = self.data;
        let end = (until as usize / 8).min(data.len());
        let mut r = Reader::at(&data[..end], from as usize);
        let header = from;
        if shape.vertical_size > 2800 {
            r.skip(3);
        }
        let header_to = r.position() as u32;
        let quant = r.u(5) as u8;
        let extra = r.position() as u32;
        if r.peek(1) == 1 {
            // intra_slice_flag, intra_slice, seven reserved bits, and then as
            // much extra information as the slice cares to carry.
            r.skip(9);
            while r.peek(1) == 1 {
                r.skip(9);
            }
        }
        r.skip(1);
        let extra_to = r.position() as u32;

        let mbs_from = self.mbs.len() as u32;
        let mut q = quant;
        while r.peek(23) != 0 && !r.exhausted() {
            self.read_mb(t, &mut r, head, &mut q)?;
        }
        if !r.ok() {
            return Err(Error::Truncated);
        }
        let mbs = (mbs_from, self.mbs.len() as u32);
        if mbs.0 == mbs.1 {
            return Err(Error::EmptySlice);
        }
        self.slices.push(Slice {
            header: (header, header_to),
            quant,
            extra: (extra, extra_to),
            mbs,
            end: r.position() as u32,
            until: until / 8,
            q_scale_type: head.q_scale_type,
            intra_vlc_format: head.intra_vlc_format,
            alternate_scan: head.alternate_scan,
        });
        Ok(())
    }

    fn read_mb(&mut self, t: &Tables, r: &mut Reader, head: &Head, q: &mut u8) -> Result<(), Error> {
        let pre = r.position() as u32;
        // The address increment, and however many escapes it took to say it.
        // Thirty-three macroblocks an escape, and a row of a 1440-wide
        // picture is ninety.
        let mut escapes = 0;
        loop {
            let hit = t.address_increment.read(r).ok_or(Error::BadCode)?;
            match hit.row {
                // The escape: another thirty-three, and read again.
                33 => {
                    escapes += 1;
                    if escapes > 64 {
                        return Err(Error::BadCode);
                    }
                }
                // Stuffing, which MPEG-1 allowed and MPEG-2 forbids, and the
                // row that is not a code but the end of a slice.
                34 | 35 => return Err(Error::BadCode),
                _ => break,
            }
        }
        let types: &[tables::MbType] = match head.coding_type {
            1 => &tables::MB_TYPE_I,
            2 => &tables::MB_TYPE_P,
            3 => &tables::MB_TYPE_B,
            _ => return Err(Error::PictureType),
        };
        let hit = t.mb_type[head.coding_type as usize - 1]
            .read(r)
            .ok_or(Error::BadCode)?;
        let flags = types[hit.row as usize].2;
        let intra = flags & flags::INTRA != 0;
        let pattern = flags & flags::PATTERN != 0;
        let moves = flags & (flags::FORWARD | flags::BACKWARD) != 0;

        let frame = head.picture_structure == 3;
        let motion_type = if moves && !(frame && head.frame_pred_frame_dct) {
            r.u(2)
        } else {
            // Frame-based, which is the only thing a frame picture that
            // predicts frames from frames can mean.
            2
        };
        if frame && !head.frame_pred_frame_dct && (intra || pattern) {
            r.skip(1); // dct_type
        }
        let pre_to = r.position() as u32;

        let quant = (flags & flags::QUANT != 0).then(|| r.u(5) as u8);
        if let Some(code) = quant {
            *q = code;
        }

        let mid = r.position() as u32;
        let conceal = intra && head.concealment_motion_vectors;
        if flags & flags::FORWARD != 0 || conceal {
            read_motion(t, r, head, 0, motion_type, frame, conceal)?;
        }
        if flags & flags::BACKWARD != 0 {
            read_motion(t, r, head, 1, motion_type, frame, false)?;
        }
        if conceal {
            r.skip(1); // marker_bit
        }
        let mid_to = r.position() as u32;

        let cbp = if pattern {
            t.coded_block_pattern.read(r).ok_or(Error::BadCode)?.row
        } else {
            0
        };

        let blocks_from = self.blocks.len() as u32;
        for which in 0..6u8 {
            if !intra && (cbp >> (5 - which)) & 1 == 0 {
                continue;
            }
            self.read_block(t, r, head, which, intra)?;
        }
        if !r.ok() {
            return Err(Error::Truncated);
        }
        self.mbs.push(Mb {
            pre: (pre, pre_to),
            quant,
            mid: (mid, mid_to),
            flags,
            q: *q,
            blocks: (blocks_from, self.blocks.len() as u32),
        });
        Ok(())
    }

    fn read_block(
        &mut self,
        t: &Tables,
        r: &mut Reader,
        head: &Head,
        which: u8,
        intra: bool,
    ) -> Result<(), Error> {
        let dc = r.position() as u32;
        if intra {
            let lut = &t.dc_size[usize::from(which >= 4)];
            let size = lut.read(r).ok_or(Error::BadCode)?.row;
            r.skip(u32::from(size));
        }
        let dc_to = r.position() as u32;

        let table = &t.coefficients[usize::from(intra && head.intra_vlc_format)];
        let from = self.coeffs.len() as u32;
        // Where in the block the next coefficient lands. An intra block has
        // spent place zero on its DC already.
        let mut at = u32::from(intra);
        let mut first = !intra;
        loop {
            if first && r.peek(1) == 1 {
                // The one place `10` is not an end of block: the first
                // coefficient of a non-intra block, where a single `1` and a
                // sign say run nothing, level one.
                r.skip(1);
                let level = if r.bit1() { -1 } else { 1 };
                self.coeffs.push(Coeff { at: 0, level });
                at += 1;
                first = false;
                continue;
            }
            first = false;
            let hit = table.read(r).ok_or(Error::BadCode)?;
            let row = hit.row as usize;
            if row == tables::END_OF_BLOCK {
                break;
            }
            let (run, level) = if row == tables::ESCAPE {
                let run = r.u(6);
                let level = sign_extend(r.u(12), 12);
                if level == 0 || level == -2048 {
                    return Err(Error::BadLevel);
                }
                (run, level as i16)
            } else {
                let run = u32::from(tables::RUN[row]);
                let mag = i16::from(tables::LEVEL[row]);
                (run, if r.bit1() { -mag } else { mag })
            };
            at += run;
            if at > 63 {
                return Err(Error::BlockOverrun);
            }
            self.coeffs.push(Coeff {
                at: at as u8,
                level,
            });
            at += 1;
        }
        self.blocks.push(Block {
            which,
            dc: (dc, dc_to),
            coeffs: (from, self.coeffs.len() as u32),
        });
        Ok(())
    }
}

/// How much of a block's tail to throw away.
///
/// The last coefficients of a block are the ones worth dropping. They are the
/// smallest -- a block is written from the lowest frequency up and the levels
/// fall as it goes -- and they are the dearest, because each one is a code of
/// its own and because they are what keeps the end-of-block from coming
/// sooner. Dropping a suffix also leaves every code before it exactly as it
/// was, so what it saves is known rather than estimated.
///
/// What a coefficient is worth is its level times what the quantisation
/// matrix says about its place in the block, squared. What it costs is the
/// bits of its own code. The tail goes as far back as that trade stays worth
/// making.
fn trim(coeffs: &[Coeff], bits: &[u32], weights: &[u16; 64], thin: f64) -> usize {
    let mut worth = 0.0f64;
    let mut cost = 0.0f64;
    let mut best = 0.0f64;
    let mut keep = coeffs.len();
    for i in (0..coeffs.len()).rev() {
        let value = f64::from(coeffs[i].level) * f64::from(weights[usize::from(coeffs[i].at)]);
        worth += value * value;
        cost += f64::from(bits[i]);
        let gain = worth - thin * cost;
        if gain < best {
            best = gain;
            keep = i;
        }
    }
    keep
}

/// How many bits one coefficient takes.
fn bits_of(run: u32, level: i16, table: usize, first: bool) -> u32 {
    let mag = u32::from(level.unsigned_abs());
    if first && run == 0 && mag == 1 {
        return 2;
    }
    let codes: &[tables::Code] = if table == 0 {
        &tables::COEFFICIENTS_ZERO
    } else {
        &tables::COEFFICIENTS_ONE
    };
    match vlc::coefficient_row(vlc::tables(), run, mag) {
        // The code, and the sign after it.
        Some(row) => u32::from(codes[row as usize].1) + 1,
        // The escape, and the run and level it carries.
        None => u32::from(codes[tables::ESCAPE].1) + 18,
    }
}

/// Write a block's coefficients and the code that ends it.
fn emit_coeffs<S: Sink>(sink: &mut S, coeffs: &[Coeff], table: usize, non_intra: bool) {
    let t = vlc::tables();
    let codes: &[tables::Code] = if table == 0 {
        &tables::COEFFICIENTS_ZERO
    } else {
        &tables::COEFFICIENTS_ONE
    };
    let mut first = non_intra;
    let mut last = i32::from(!non_intra) - 1;
    for c in coeffs {
        let run = (i32::from(c.at) - last - 1).max(0) as u32;
        last = i32::from(c.at);
        let mag = u32::from(c.level.unsigned_abs());
        let sign = u32::from(c.level < 0);
        if first && run == 0 && mag == 1 {
            // `1` and a sign. Two bits, and the only two bits in the whole
            // format whose meaning depends on where they are.
            sink.u(2 | sign, 2);
        } else if let Some(row) = vlc::coefficient_row(t, run, mag) {
            let (code, bits) = codes[row as usize];
            sink.u(u32::from(code), u32::from(bits));
            sink.u(sign, 1);
        } else {
            let (code, bits) = codes[tables::ESCAPE];
            sink.u(u32::from(code), u32::from(bits));
            sink.u(run, 6);
            sink.u(u32::from(c.level as u16) & 0xfff, 12);
        }
        first = false;
    }
    let (code, bits) = codes[tables::END_OF_BLOCK];
    sink.u(u32::from(code), u32::from(bits));
}

/// Step over one macroblock's motion vectors. Nothing about them changes, so
/// this only has to agree with the encoder about how long they are.
fn read_motion(
    t: &Tables,
    r: &mut Reader,
    head: &Head,
    s: usize,
    motion_type: u32,
    frame: bool,
    conceal: bool,
) -> Result<(), Error> {
    // A concealment vector is one vector of the picture's own shape, whatever
    // the macroblock's motion type would otherwise have said.
    let (count, field, dual) = if conceal {
        (1, !frame, false)
    } else if frame {
        match motion_type {
            1 => (2, true, false),
            2 => (1, false, false),
            3 => (1, true, true),
            _ => return Err(Error::MotionType),
        }
    } else {
        match motion_type {
            1 => (1, true, false),
            2 => (2, true, false),
            3 => (1, true, true),
            _ => return Err(Error::MotionType),
        }
    };
    for _ in 0..count {
        if field && (count == 2 || !dual) {
            r.skip(1); // motion_vertical_field_select
        }
        for axis in 0..2 {
            let hit = t.motion_code.read(r).ok_or(Error::BadCode)?;
            if hit.row != 0 {
                r.skip(1); // sign
                let f_code = head.f_code[s][axis];
                if f_code > 1 {
                    r.skip(f_code - 1); // motion_residual
                }
            }
            if dual {
                // dmvector: one bit for nothing, two for either direction.
                if r.bit1() {
                    r.skip(1);
                }
            }
        }
    }
    Ok(())
}

/// Every start code in the payload, as `(byte, code)`.
fn start_codes(data: &[u8]) -> Vec<(usize, u8)> {
    let mut out = Vec::with_capacity(96);
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            out.push((i, data[i + 3]));
            i += 4;
        } else {
            i += 1;
        }
    }
    out
}

fn sequence_header(data: &[u8], at: usize, shape: &mut Shape) -> Result<(), Error> {
    let mut r = Reader::at(data, (at + 4) * 8);
    r.skip(12); // horizontal_size_value
    let vertical = r.u(12);
    if vertical == 0 {
        return Err(Error::NotAPicture);
    }
    // The sequence extension may add two more bits of it; until one arrives
    // this is the whole height.
    shape.vertical_size = vertical;
    r.skip(4 + 4 + 18 + 1 + 10 + 1);
    // A sequence header that carries no matrix of its own is not silent about
    // them: it puts the default ones back.
    let mut sent = [0u8; 64];
    if r.bit1() {
        for v in sent.iter_mut() {
            *v = r.u(8) as u8;
        }
        shape.intra = scan::unscan(&sent);
    } else {
        shape.intra = scan::unscan(&scan::DEFAULT_INTRA);
    }
    if r.bit1() {
        for v in sent.iter_mut() {
            *v = r.u(8) as u8;
        }
        shape.non_intra = scan::unscan(&sent);
    } else {
        shape.non_intra = scan::unscan(&scan::DEFAULT_NON_INTRA);
    }
    Ok(())
}

/// A picture may bring matrices of its own. The chroma ones are not read: in
/// 4:2:0, which is all this rewrites, the chroma blocks use the luma matrices
/// and the chroma ones say nothing.
fn quant_matrix_extension(r: &mut Reader, shape: &mut Shape) {
    let mut sent = [0u8; 64];
    if r.bit1() {
        for v in sent.iter_mut() {
            *v = r.u(8) as u8;
        }
        shape.intra = scan::unscan(&sent);
    }
    if r.bit1() {
        for v in sent.iter_mut() {
            *v = r.u(8) as u8;
        }
        shape.non_intra = scan::unscan(&sent);
    }
}

fn sequence_extension(r: &mut Reader, shape: &mut Shape) {
    r.skip(8); // profile_and_level_indication
    r.skip(1); // progressive_sequence
    shape.chroma_format = r.u(2) as u8;
    r.skip(2); // horizontal_size_extension
    shape.vertical_size |= r.u(2) << 12;
}

fn picture_head(data: &[u8], at: usize) -> Result<Head, Error> {
    let mut r = Reader::at(data, (at + 4) * 8);
    r.skip(10); // temporal_reference
    let coding_type = r.u(3) as u8;
    if !(1..=3).contains(&coding_type) {
        return Err(Error::PictureType);
    }
    Ok(Head {
        coding_type,
        // Everything else arrives in the coding extension, which MPEG-2 says
        // follows this immediately.
        picture_structure: 3,
        ..Head::default()
    })
}

fn picture_coding_extension(r: &mut Reader, head: &mut Head) {
    for s in 0..2 {
        for axis in 0..2 {
            head.f_code[s][axis] = r.u(4);
        }
    }
    r.skip(2); // intra_dc_precision
    head.picture_structure = r.u(2);
    r.skip(1); // top_field_first
    head.frame_pred_frame_dct = r.bit1();
    head.concealment_motion_vectors = r.bit1();
    head.q_scale_type = r.bit1();
    head.intra_vlc_format = r.bit1();
    head.alternate_scan = r.bit1();
    head.extended = true;
}

fn sign_extend(value: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((value << shift) as i32) >> shift
}

/// Where a picture written back unchanged stops being the picture that
/// arrived.
///
/// The identity is this program's test for whether it reads MPEG-2 at all,
/// and a test that says only "these bytes differ" is a test nobody can act
/// on. This walks the same emit macroblock by macroblock and compares each
/// one against the bits it came from, so what comes back is the first
/// macroblock that was written differently and everything known about it.
pub fn first_difference(data: &[u8], shape: &mut Shape) -> Result<Option<String>, Error> {
    let picture = Picture::read(data, shape)?;
    let q = Quantiser::default();
    let press = Squeeze::default();
    let mut scratch = Scratch::default();
    for (index, slice) in picture.slices.iter().enumerate() {
        let map = q.map(slice.q_scale_type, 0.0);
        let mbs = &picture.mbs[slice.mbs.0 as usize..slice.mbs.1 as usize];
        for (k, mb) in mbs.iter().enumerate() {
            // Where this macroblock ended in the source: where the next one
            // starts, or -- for the last of a slice -- where the slice's
            // macroblocks stop and its padding begins.
            let from = mb.pre.0 as usize;
            let until = mbs
                .get(k + 1)
                .map_or(slice.end as usize, |next| next.pre.0 as usize);
            let mut sink = crate::bits::Writer::new();
            let pen = Pen {
                slice,
                map: &map,
                press,
            };
            let spread = phase(slice.mbs.0 + k as u32);
            picture.emit_mb(&mut sink, mb, &pen, spread, &mut scratch);
            let wrote = sink.position();
            let out = sink.finish();
            let mut a = Reader::new(&out);
            let mut b = Reader::at(data, from);
            let same = wrote == until - from && (0..wrote).all(|_| a.u(1) == b.u(1));
            if !same {
                let blocks = &picture.blocks[mb.blocks.0 as usize..mb.blocks.1 as usize];
                let coeffs: Vec<String> = blocks
                    .iter()
                    .map(|b| format!("{}:{}", b.which, b.coeffs.1 - b.coeffs.0))
                    .collect();
                return Ok(Some(format!(
                    "slice {index} macroblock {k} of {}: wrote {wrote} bits where the source has \
                     {}, flags {:#04x}, quantiser code {}{}, blocks {}, picture is {}intra-vlc, {}",
                    mbs.len(),
                    until - from,
                    mb.flags,
                    mb.q,
                    if mb.quant.is_some() { " (its own)" } else { "" },
                    coeffs.join(" "),
                    if slice.intra_vlc_format { "" } else { "not " },
                    if slice.q_scale_type {
                        "non-linear scale"
                    } else {
                        "linear scale"
                    }
                )));
            }
        }
    }
    Ok(None)
}
