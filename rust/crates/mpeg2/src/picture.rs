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
            intra: scan::DEFAULT_INTRA,
            non_intra: scan::DEFAULT_NON_INTRA,
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
        // Every place in the picture is kept as a bit offset in 32 bits.
        // No coded picture comes near this; a packet that does is not one.
        if data.len() > (u32::MAX / 8) as usize {
            return Err(Error::NotAPicture);
        }
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
        shape.intra = scan::DEFAULT_INTRA;
    }
    if r.bit1() {
        for v in sent.iter_mut() {
            *v = r.u(8) as u8;
        }
        shape.non_intra = scan::unscan(&sent);
    } else {
        shape.non_intra = scan::DEFAULT_NON_INTRA;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{rewrite_unchanged, Transrater};

    /// Three pictures (I, B, P) of a synthetic test pattern, 48x32: field
    /// motion and field DCT, the non-linear scale, the alternate scan and the
    /// second intra table -- every switch a broadcast turns on.
    const INTERLACED: &str = concat!(
        "000001b303002013ffffe018000001b5148200010000000001b80008004000000100000ffff8000001b58ffff31c0000",
        "000101137ce3a0400309f000c0009801a8023e17fa4ffb0b72ff88ff3f4e08beff412288defecf23fc41247e843c4f4f",
        "98a0398663e5dcd6ee02001e820038803e0420130072087fee005a2411001040217f88036042ff3007800ffd8113fb80",
        "1f0053d0217fa7c779fba088009cbb5b37b1148a005bf4a008c10405c02c75f3f978025ff020025803c0032f90059ec2",
        "b000ec02dc22914103f4c015fc7d001d0055c91b9dc12e22c7986e431f2b76fec00e401c8217de0207e58218058aa089",
        "f9c00a810c0641080348dc4044fec007608404200d01080304803c12089fd98089ff9c656f3010017001f00330400680",
        "40049005c2001e7b0be09c2fd801d7f88bf22c5fda7f401ef95f9ef5feff246e001a0bf814248a016fb3b3367bfcd789",
        "89aa6b7a8081f400063ec0800ac009801c7c80240078ff77fcfd057046c828019f108bc3f9bc11c5f7d0a00720083dfe",
        "826fdbe28a22bcd15aededfb6d96ab670400a104008104002900600097dbdffc2c107fa4008c01cf02456c73f817edd8",
        "8ec40970915de0911d8adce5ddf1b29e366cb2c6f9c7fc008b9002b04002cfa7d4019fc19f5c1646f97675c0bf9fb7b1",
        "1fdc8847e8884711c9148dedf2f70ae7dfa7cc3e9798fcbb9637b9f6404002afb8b007247004624579bbae0884700740",
        "81f89fffa7d0107f97455dfe5111e0024007800dbe001ef66f5df5a2a8be3b36f38a623713ec60207e08038f9004bfe0",
        "4002d22fc107bf71600fc01f802604005b006400e001d020814bfe35e00d708f7ba00b40158022fe002c11e7ec244f1f",
        "39f22847960b72c7cada0200157f3e4103e34103ee8016fd8103f5c01289999f3f5007a00d670004df473c882b7917f6",
        "f815d3f9fb002fe401a7dde47f98feabee89fa08f6151f5fd66656c0009febec00f08800c88a23a15776639d79ef7736",
        "e3bafadb49c45740076444f6b9c822003116dbe51dcb6b001d02002ff008402a00d49a00974110013d810bfc00190217",
        "f880380077f0089fda00f010bfe850217f99185e7ee82200277c6b7501001dc1001b0011002a002ff8f9007c24004600",
        "5ffdebdc46e39fd09f816205fb7bf1ee23b77b88162373b70be3653c6e6e5cb1b57f001a8035042fb5040fc604300c15",
        "a089f92009810c06c10803c89c4044fea006e08404800c0108037dc01c7b8227f560227fe7195b111901000bffa2401a",
        "91400b7dc413f815d7d3ea00d8103ef401fff3f8083fc3a22bffda223c004000d801911801cf668aefa788a2f8ecdde7",
        "16e58dd20800c208005eefa8020fe09fb00544611f6235f2379e7e8477f531c2797f223e4016003ef7fa88e48dfd00af",
        "ecff3eb813c76f7618fab18040fce0072201001fc1000c801ffb00170026e3745dfe003800691c00dc01d8915f513779",
        "f612fa245003fe001f7dbdfda1189deee7389fec00f57e74d72c7b0000000102137cac10c0fc10409813404013ffdc00",
        "ec137fe9ac6ac10c0fc10409813404013ffdc00ec137fe9ac6f380270012826ff8001e826ffe13c137ff5bb410c25c10",
        "40b813403010406813408000d8137ff5b741bcab7aa6f2ade8821fcd82780d027ff9027804b71021807027ffe000b00f",
        "6dd06e8373dba0ddc08602009fff8002c03da410c17013ffa413c03413ffdd80000001000097fffb80000001b5811ff3",
        "1c0000000101128c491f9a38f5af6565339bb123a9160341603fc68bd43fe38082dc71a770f5688244c4e3778f304380",
        "72adf7e63f7a166f229044beebea7a8cd80d095331c2ce1ccdb71b240e0b3c71ad95f2f7638f1be03996256e666316e6",
        "f538df61c406f9c4ab2bf7336526231aac1ca735799ced96ff6848a52c52cdcad95d94b17fd4664f70f23e0a36d4068e",
        "0f5079ddfb70264b806a1c4e163855a80d71e733b3673f2fe7e71d446c6f3cce70e356147d08037dd89cbd85ade00000",
        "0102126d9b00000100005ffffbb8000001b58111131c000000010112e6501b3ffe07efffae60400fdc0372744600de01",
        "9000f0d880785c0c200206ab98c8337ff80b5a063fff805a681d13e00000010212adab",
    );

    /// The same pattern with everything left at its default, and a bit rate
    /// rather than a fixed scale, so macroblocks carry scales of their own.
    const PLAIN: &str = concat!(
        "000001b303002013ffffe028000001b5148a00010000000001b80008004000000100000ffff8000001b58ffff3418000",
        "00010113f9c131000b8034007405004e017e19c9a1b8848600acb0d26934b26a4a0c2613084421a1a1a1bd9480c2ba5b",
        "7ecdbfbb808203e08007200780076007c02800ac00e86019007a015801e8d18007c00784b003f00b5c1100100c12c604",
        "b0c5d9801e8613485c009002f00b400d80a00e8107fa80a06805fca4068600644300cc339080604d2695d44a00b5b749",
        "6c4cddb16943ddb00b0062083fbc083fcc007608401a086018083fce0362802a003b0012001d001e80d9c600580076a0",
        "3208601234200cb5e602001a02001700e800a00342ca0180084b0c0d2c84800ac8605100078007a1a0262c9a4c289894",
        "0620b726979c373868697896a0ce7fbd404002b040fd0007e01800c0a268060189018001ca002ec4dc4c0d4a12828348",
        "5cbe03126a4a26908964c2172fa38d420a7eb2694f6e02004005004004100bc3386814006c4bc82d2189dc04200e4a2c",
        "a0d2bb24a2ca2c06282ca4e024946ecdcef7ce001e80ec01c003221208602148184164d0c21066d8861a1a4d26935230",
        "a26130984cc59349a1a9e7141884a5b7ecdbdee401a06005a5802301893706805e03002a4201d10806e4c286700c780c",
        "004c92600ec02a28a4a1c696e8472b92ba7b149ed71001090800e8341000b403102988600c003001000c0035018001f8",
        "0ec984b480344806601714050032003b2606a4bf80c068c2837935206105e626979ded0103f1c01a014002b00d401d01",
        "4006e4d2c02f410b006bc980073b00c794948098300af02818921e0d2ca269343430a21314053f4a5823a7645800c001",
        "d90c980202c865061484fe8dc67fb0c003d00b002de8df200c62b80dc10ffe9980fb04dd8007808005e0074007230050",
        "007e007031c03100ac00ec964a003c003c7003d00adc1100140c12c604b0c5dd400c81000ec1000d801f8670d01d005c",
        "4b420b2c985ee03b007e516181a57648606860681540686178d4168ddbee77b50054027041fda041fe3003904200e043",
        "00e041fe400b0a00a800e400400070007601638c00a800e5406410c0246840196b100302600565801680849b83401e80",
        "9807605006005002d211433805e9013008124c018801f145251f92cb7415c3392ba7b145f6ba48608006000c002e0289",
        "0d029831d24201d137b130302034348680c480689c0151311c0a93109e5f1a1a181ad9642465c020020002d018802c01",
        "d809c3001e14800c300c4b41443003afc061c34a01d1080621a1803b2f8142c6f260d0146014168dc9aa0d7578000001",
        "0213f95802004301301d0247fe025ffd975956008010c04c074091ff8097ff65d65e72410ffe007608fff89001aa6ed0",
        "4004b04301721023806803e04b006f7d0bcadeaaf2b7a2081f9a08801a091ff209800b7101403e1c45be85d0b9f742ee",
        "01d01f0e22d208005c089fe809000c099ff900000001000097fffb80000001b5811ff341800000010112980d1ffe3963",
        "f8ed000fd8787c003d2701174747f000f805c4184118513dfacf33f33bc00370e5bb8bffbe189a9806dd7c9f9ddc7b8f",
        "3891fc01fac03e1603dd00724710c17969d000f0f593d66675ec98007203353fc626001e870fee394754cc1afffce170",
        "1d23103881048fe0700016001ac003d7500bd8d4a201fb1ce389fff11dd97d1035fffb3bb1849c84c00000010212ac00",
        "7b338000000100005ffffbb8000001b58111134180000001011ab6b0000001021a5b2c",
    );

    fn bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The stream cut at each picture start code, the headers staying with
    /// the picture they come in front of -- which is how a packet arrives.
    fn pictures(stream: &[u8]) -> Vec<&[u8]> {
        let cuts: Vec<usize> = start_codes(stream)
            .into_iter()
            .filter(|&(_, code)| code == 0x00)
            .map(|(at, _)| at)
            .skip(1)
            .collect();
        let mut out = Vec::new();
        let mut from = 0;
        for at in cuts {
            out.push(&stream[from..at]);
            from = at;
        }
        out.push(&stream[from..]);
        out
    }

    #[test]
    fn written_back_unchanged_it_is_the_same_picture() {
        for stream in [INTERLACED, PLAIN] {
            let stream = bytes(stream);
            let mut shape = Shape::default();
            for picture in pictures(&stream) {
                let out = rewrite_unchanged(picture, &mut shape).expect("reads");
                assert_eq!(out, picture);
            }
        }
    }

    #[test]
    fn the_default_intra_matrix_is_already_by_place() {
        // Place 8 is the first sample of the second row, which the standard's
        // default matrix gives 16; the third value it prints, 19, belongs to
        // place 2. Read as if it were sent in zigzag order the two swap.
        let stream = bytes(PLAIN);
        let mut shape = Shape::default();
        Picture::read(pictures(&stream)[0], &mut shape).expect("reads");
        assert_eq!(shape.intra, scan::DEFAULT_INTRA);
        assert_eq!((shape.intra[8], shape.intra[2]), (16, 19));
        let weights = scan::weights(&shape.intra, false);
        assert_eq!(&weights[..4], &[8, 16, 16, 19]);
    }

    #[test]
    fn a_squeezed_picture_reads_back() {
        for stream in [INTERLACED, PLAIN] {
            let stream = bytes(stream);
            let mut rater = Transrater::default();
            for picture in pictures(&stream) {
                let out = rater.picture(picture, 0.5).expect("rewrites");
                let mut shape = rater.shape;
                Picture::read(&out, &mut shape).expect("reads back");
            }
        }
    }

    /// Whatever arrives, the walk says no rather than falling over: every
    /// truncation and every single flipped bit of each picture, read and
    /// written at three strengths.
    #[test]
    fn damaged_pictures_are_declined_not_panicked_on() {
        let q = Quantiser::default();
        let presses = [
            Squeeze::default(),
            Squeeze { lift: 2.5, thin: 3.0 },
            Squeeze { lift: 12.0, thin: 1e6 },
        ];
        let mut scratch = Scratch::default();
        let mut try_one = |data: &[u8], shape: Shape| {
            let mut shape = shape;
            if let Ok(p) = Picture::read(data, &mut shape) {
                for press in presses {
                    let size = p.measure(&q, press, &mut scratch);
                    assert_eq!(p.write(&q, press, &mut scratch).len(), size);
                }
            }
        };
        for stream in [INTERLACED, PLAIN] {
            let stream = bytes(stream);
            let mut shape = Shape::default();
            for picture in pictures(&stream) {
                for len in 0..picture.len() {
                    try_one(&picture[..len], shape);
                }
                let mut data = picture.to_vec();
                for bit in 0..data.len() * 8 {
                    data[bit / 8] ^= 0x80 >> (bit % 8);
                    try_one(&data, shape);
                    data[bit / 8] ^= 0x80 >> (bit % 8);
                }
                Picture::read(picture, &mut shape).expect("reads");
            }
        }
    }
}
