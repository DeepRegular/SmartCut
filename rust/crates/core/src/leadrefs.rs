//! Whether the leading pictures of an H.264 entry point that are reference
//! pictures are referenced by anything other than one another.
//!
//! **A leading picture that is a reference is not, by that alone, needed by
//! what follows.** A copy that opens on an open-GOP entry point leaves its
//! leading pictures behind, and that is only safe where no picture the copy
//! keeps predicts from one of them. [`crate::bitstream::is_reference`] asks
//! each picture whether it is a reference at all, which settles it for the
//! usual GOP -- leading B pictures that nothing predicts from -- and refuses
//! every GOP whose leading pictures predict from one another. A recorder's
//! field-coded (PAFF) H.264 is built that way: of the two leading B frames
//! in front of each I, the first is a reference for the second, and nothing
//! after the I is predicted from either -- the P pictures name the I's fields
//! in an explicit reordering that steps over the B, and the B pictures'
//! lists are too short to reach back past the I. Judged by the flag alone,
//! every entry point of such a recording was one no copy may open on: each
//! range's head was re-encoded up to the first point nobody had read, twelve
//! to eighteen seconds, and a range shorter than about thirty seconds was
//! re-encoded whole.
//!
//! So the question is answered as a decoder would have to answer it: the
//! short-term reference pictures are followed from the entry point on
//! (sliding window, `frame_num` gaps, `memory_management_control_operation`
//! 1), and the reference lists of every slice of every picture that is not a
//! leading one are built (8.2.4 of the standard: the initial order, the
//! field alternation, the modifications, the active length). If none of
//! them names a leading picture before the last leading reference picture
//! has left the buffer, the leading pictures are droppable.
//!
//! **And the cut is not the recording.** Each leading reference picture
//! dropped leaves a gap in `frame_num`, and the cut's decoder infers a frame
//! in its place -- the same slot in the window and in a P picture's list,
//! but an order count of the decoder's choosing, so not the same place in a
//! B picture's. While one is held, a B picture the copy keeps has to name
//! every entry of its lists itself ([`Open::alive`]). The recorder unmarks
//! its leading B at the first P after the I, so this costs it nothing; a
//! pressed Blu-ray whose B pictures take list 1 by default stays needed.
//!
//! **Every doubt is answered "needed".** A stream this cannot follow --
//! long-term references, `pic_order_cnt_type` 1, a parameter set it has not
//! been shown, a slice it cannot read, a window that ends while a leading
//! reference is still held -- is left with the answer the flag gives, which
//! is what every recording got before. Where the model is unsure of what is
//! in the buffer it errs towards a leading picture standing *earlier* in a
//! list than it really does:
//!
//! - Pictures from before the entry point are not known and are left out.
//!   They are older than the leading pictures by `frame_num` and earlier by
//!   order count, so they can only stand after them in a list, or push them
//!   further back.
//! - The frames inferred for a gap in `frame_num` (the recorder's stream is
//!   full of them: its `frame_num` counts the B frames it chose not to keep
//!   as references) take their place in the sliding window, as the standard
//!   has them do, but are left out of the lists, which a stream may not
//!   predict from anyway.
//!
//! What pictures from before the entry point do to the buffer does not
//! change what happens to the ones after it: the sliding window takes the
//! oldest first, and those are older than all of them.

use crate::bitstream::{nal_payloads, Bits, NalFraming, H264_VCL};
use std::collections::HashMap;

/// How many pictures one entry point is followed for before it is given up
/// as undecided (and so as needed). A GOP of a recorder or a broadcaster is
/// thirty to a hundred and twenty; the leading reference pictures leave the
/// buffer within a few of them.
const CAP: usize = 1000;

/// How many entry points are followed at once. One is usual -- an entry
/// point is decided within a GOP or two -- and a stream that keeps more open
/// than this has the oldest given up as undecided.
const OPEN: usize = 16;

/// A slice header is a few dozen bytes. Read this much of a slice first, and
/// the whole of it only where that runs out -- a long weight table.
const HEAD_BYTES: usize = 512;

#[derive(Debug, Clone, Copy)]
struct Sps {
    frame_num_bits: u32,
    poc_type: u32,
    poc_lsb_bits: u32,
    max_refs: u32,
    frame_mbs_only: bool,
    separate_planes: bool,
    chroma_array_type: u32,
}

impl Sps {
    fn max_frame_num(&self) -> u32 {
        1 << self.frame_num_bits
    }
}

#[derive(Debug, Clone, Copy)]
struct Pps {
    sps: u32,
    bottom_poc_present: bool,
    l0_default: u32,
    l1_default: u32,
    weighted_pred: bool,
    weighted_bipred: u32,
    redundant_present: bool,
}

#[derive(Debug, Default)]
struct Sets {
    sps: HashMap<u32, Sps>,
    pps: HashMap<u32, Pps>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    P,
    B,
    I,
}

#[derive(Debug, Clone)]
struct Slice {
    kind: Kind,
    /// `num_ref_idx_lX_active_minus1 + 1`, as it stands for this slice.
    active: [u32; 2],
    /// `(modification_of_pic_nums_idc, value)` in order, per list.
    changes: [Vec<(u32, u32)>; 2],
}

#[derive(Debug, Clone)]
struct Picture {
    sps: Sps,
    idr: bool,
    reference: bool,
    frame_num: u32,
    field: bool,
    bottom: bool,
    poc_lsb: u32,
    delta_bottom: i32,
    /// `None` for the sliding window, the operations otherwise.
    marking: Option<Vec<(u32, u32)>>,
    /// Something this does not follow: long-term references.
    long_term: bool,
    slices: Vec<Slice>,
}

/// One slice's header, as far as the lists and the marking go.
struct Head {
    first_mb: u32,
    picture: Picture,
    slice: Option<Slice>,
}

fn read_sps(nal: &[u8]) -> Option<(u32, Sps)> {
    let mut b = Bits::new(nal.get(1..)?);
    let profile = b.u(8)?;
    b.skip(16)?;
    let id = b.ue()?;
    let mut chroma = 1;
    let mut separate_planes = false;
    if [100, 110, 122, 244, 44, 83, 86, 118, 128, 134, 135, 138, 139].contains(&profile) {
        chroma = b.ue()?;
        if chroma == 3 {
            separate_planes = b.u(1)? == 1;
        }
        b.ue()?;
        b.ue()?;
        b.skip(1)?;
        if b.u(1)? == 1 {
            for i in 0..if chroma == 3 { 12 } else { 8 } {
                if b.u(1)? == 0 {
                    continue;
                }
                let (mut last, mut next) = (8i64, 8i64);
                for _ in 0..if i < 6 { 16 } else { 64 } {
                    if next != 0 {
                        next = (last + i64::from(b.se()?) + 256).rem_euclid(256);
                    }
                    if next != 0 {
                        last = next;
                    }
                }
            }
        }
    }
    let frame_num_bits = b.ue()?.checked_add(4)?;
    if frame_num_bits > 16 {
        return None;
    }
    let poc_type = b.ue()?;
    let mut poc_lsb_bits = 0;
    match poc_type {
        0 => {
            poc_lsb_bits = b.ue()?.checked_add(4)?;
            if poc_lsb_bits > 16 {
                return None;
            }
        }
        // Followed nowhere below; read past so the set still reads.
        1 => {
            b.skip(1)?;
            b.se()?;
            b.se()?;
            let cycle = b.ue()?;
            if cycle > 255 {
                return None;
            }
            for _ in 0..cycle {
                b.se()?;
            }
        }
        2 => {}
        _ => return None,
    }
    // Sixteen is the standard's ceiling; a header claiming more is not one
    // to follow a buffer by.
    let max_refs = b.ue()?;
    if max_refs > 16 {
        return None;
    }
    b.skip(1)?;
    b.ue()?;
    b.ue()?;
    let frame_mbs_only = b.u(1)? == 1;
    Some((
        id,
        Sps {
            frame_num_bits,
            poc_type,
            poc_lsb_bits,
            max_refs,
            frame_mbs_only,
            separate_planes,
            chroma_array_type: if separate_planes { 0 } else { chroma },
        },
    ))
}

fn read_pps(nal: &[u8]) -> Option<(u32, Pps)> {
    let mut b = Bits::new(nal.get(1..)?);
    let id = b.ue()?;
    let sps = b.ue()?;
    b.skip(1)?; // entropy_coding_mode_flag
    let bottom_poc_present = b.u(1)? == 1;
    // Slice groups are Baseline and Extended only, and change nothing read
    // here -- but their syntax is long, and nothing here has met one.
    if b.ue()? != 0 {
        return None;
    }
    // At most 32 each, as a field counts them; anything more is a mis-parse.
    let l0_default = b.ue()?.checked_add(1).filter(|&n| n <= 32)?;
    let l1_default = b.ue()?.checked_add(1).filter(|&n| n <= 32)?;
    let weighted_pred = b.u(1)? == 1;
    let weighted_bipred = b.u(2)?;
    b.se()?;
    b.se()?;
    b.se()?;
    b.skip(2)?;
    let redundant_present = b.u(1)? == 1;
    Some((
        id,
        Pps {
            sps,
            bottom_poc_present,
            l0_default,
            l1_default,
            weighted_pred,
            weighted_bipred,
            redundant_present,
        },
    ))
}

fn read_head(nal: &[u8], sets: &Sets) -> Option<Head> {
    let head = |bytes: &[u8]| read_head_from(bytes, sets);
    match head(&nal[..nal.len().min(HEAD_BYTES)]) {
        Some(h) => Some(h),
        None if nal.len() > HEAD_BYTES => head(nal),
        None => None,
    }
}

fn read_head_from(nal: &[u8], sets: &Sets) -> Option<Head> {
    let first = *nal.first()?;
    let reference = (first >> 5) & 3 != 0;
    let idr = first & 0x1F == 5;
    let mut b = Bits::new(&nal[1..]);
    let first_mb = b.ue()?;
    let kind = match b.ue()? % 5 {
        0 | 3 => Kind::P,
        1 => Kind::B,
        _ => Kind::I,
    };
    let pps = *sets.pps.get(&b.ue()?)?;
    let sps = *sets.sps.get(&pps.sps)?;
    if sps.separate_planes {
        b.skip(2)?;
    }
    let frame_num = b.u(sps.frame_num_bits as usize)?;
    let (mut field, mut bottom) = (false, false);
    if !sps.frame_mbs_only {
        field = b.u(1)? == 1;
        if field {
            bottom = b.u(1)? == 1;
        }
    }
    if idr {
        b.ue()?;
    }
    let (mut poc_lsb, mut delta_bottom) = (0, 0);
    match sps.poc_type {
        0 => {
            poc_lsb = b.u(sps.poc_lsb_bits as usize)?;
            if pps.bottom_poc_present && !field {
                delta_bottom = b.se()?;
            }
        }
        2 => {}
        _ => return None,
    }
    let mut redundant = false;
    if pps.redundant_present {
        redundant = b.ue()? != 0;
    }
    if kind == Kind::B {
        b.skip(1)?;
    }
    let scale = if field { 2 } else { 1 };
    let mut active = [pps.l0_default * scale, pps.l1_default * scale];
    if kind != Kind::I && b.u(1)? == 1 {
        active[0] = b.ue()?.checked_add(1)?;
        if kind == Kind::B {
            active[1] = b.ue()?.checked_add(1)?;
        }
    }
    if active[0] > 32 || active[1] > 32 {
        return None;
    }
    let mut changes: [Vec<(u32, u32)>; 2] = [Vec::new(), Vec::new()];
    let lists = match kind {
        Kind::I => 0,
        Kind::P => 1,
        Kind::B => 2,
    };
    let mut long_term = false;
    for list in changes.iter_mut().take(lists) {
        if b.u(1)? == 1 {
            loop {
                let idc = b.ue()?;
                match idc {
                    0 | 1 => list.push((idc, b.ue()?)),
                    2 => {
                        long_term = true;
                        list.push((idc, b.ue()?));
                    }
                    3 => break,
                    _ => return None,
                }
                if list.len() > 33 {
                    return None;
                }
            }
        }
    }
    let weighted = (pps.weighted_pred && kind == Kind::P)
        || (pps.weighted_bipred == 1 && kind == Kind::B);
    if weighted {
        b.ue()?;
        if sps.chroma_array_type != 0 {
            b.ue()?;
        }
        for &n in active.iter().take(lists) {
            for _ in 0..n {
                if b.u(1)? == 1 {
                    b.se()?;
                    b.se()?;
                }
                if sps.chroma_array_type != 0 && b.u(1)? == 1 {
                    for _ in 0..4 {
                        b.se()?;
                    }
                }
            }
        }
    }
    let mut marking = None;
    if reference {
        if idr {
            b.skip(1)?;
            long_term |= b.u(1)? == 1;
        } else if b.u(1)? == 1 {
            let mut ops = Vec::new();
            loop {
                let op = b.ue()?;
                match op {
                    0 => break,
                    1 => ops.push((op, b.ue()?)),
                    2 | 4 | 6 => {
                        long_term = true;
                        ops.push((op, b.ue()?));
                    }
                    3 => {
                        long_term = true;
                        let d = b.ue()?;
                        b.ue()?;
                        ops.push((op, d));
                    }
                    5 => ops.push((op, 0)),
                    _ => return None,
                }
                if ops.len() > 66 {
                    return None;
                }
            }
            marking = Some(ops);
        }
    }
    Some(Head {
        first_mb,
        picture: Picture {
            sps,
            idr,
            reference,
            frame_num,
            field,
            bottom,
            poc_lsb,
            delta_bottom,
            marking,
            long_term,
            slices: Vec::new(),
        },
        slice: (!redundant).then_some(Slice {
            kind,
            active,
            changes,
        }),
    })
}

/// The pictures a packet carries, in order, with the parameter sets it
/// carries taken in on the way. `None` where a slice could not be read.
fn pictures(sets: &mut Sets, data: &[u8], framing: NalFraming) -> Option<Vec<Picture>> {
    let mut out: Vec<Picture> = Vec::new();
    let mut unreadable = false;
    for nal in nal_payloads(data, framing) {
        let Some(&b) = nal.first() else { continue };
        match b & 0x1F {
            7 => {
                if let Some((id, sps)) = read_sps(nal) {
                    sets.sps.insert(id, sps);
                }
            }
            8 => {
                if let Some((id, pps)) = read_pps(nal) {
                    sets.pps.insert(id, pps);
                }
            }
            // Partitions B and C of a sliced picture carry no slice header:
            // what they belong to was said by its partition A.
            3 | 4 => {}
            t if H264_VCL.contains(&t) => {
                let Some(head) = read_head(nal, sets) else {
                    unreadable = true;
                    continue;
                };
                let same = out.last().is_some_and(|p: &Picture| {
                    head.first_mb != 0
                        && p.frame_num == head.picture.frame_num
                        && p.field == head.picture.field
                        && p.bottom == head.picture.bottom
                        && p.idr == head.picture.idr
                });
                let long_term = head.picture.long_term;
                if !same {
                    out.push(head.picture);
                }
                let last = out.last_mut()?;
                last.long_term |= long_term;
                if let Some(slice) = head.slice {
                    last.slices.push(slice);
                }
            }
            _ => {}
        }
    }
    (!unreadable).then_some(out)
}

/// One frame (or field pair, or lone field) held as a short-term reference.
#[derive(Debug, Clone)]
struct Frame {
    frame_num: u32,
    /// Which fields are marked "used for short-term reference".
    marked: [bool; 2],
    poc: [Option<i64>; 2],
    /// Inferred for a gap in `frame_num`: it holds a place in the window
    /// and is never put in a list here.
    inferred: bool,
    leading: bool,
    /// Inferred while a leading reference picture -- or a frame inferred
    /// after one -- was the newest in the buffer. libavcodec gives an
    /// inferred frame the order of the newest one plus two, and in the cut
    /// that newest one is not the same picture: see [`Open::alive`].
    tainted: bool,
    /// A field whose other half was never a reference picture. The frame
    /// the cut infers in its place has both, which is one entry more in a
    /// field's list and one in a frame's.
    lone: bool,
}

impl Frame {
    fn held(&self) -> bool {
        self.marked[0] || self.marked[1]
    }

    /// The order count an entry is sorted by: of the fields marked.
    fn order(&self) -> Option<i64> {
        (0..2)
            .filter(|&k| self.marked[k])
            .filter_map(|k| self.poc[k])
            .min()
    }
}

/// A list entry: a frame, or one field of it (0 top, 1 bottom).
type Entry = (usize, Option<usize>);

/// The previous picture, for telling the second field of a pair.
#[derive(Debug, Clone, Copy)]
struct Prev {
    frame_num: u32,
    field: bool,
    bottom: bool,
    reference: bool,
    second: bool,
    leading: bool,
}

/// One entry point being followed.
struct Open {
    id: usize,
    key_pts: f64,
    max_frame_num: u32,
    max_refs: u32,
    dpb: Vec<Frame>,
    prev_ref_frame_num: u32,
    prev: Option<Prev>,
    // pic_order_cnt_type 0
    prev_msb: i64,
    prev_lsb: i64,
    // pic_order_cnt_type 2
    offset: i64,
    prev_frame_num: u32,
    past_next_key: bool,
    fed: usize,
    verdict: Option<bool>,
}

impl Open {
    fn start(id: usize, key_pts: f64, key: &Picture) -> Open {
        let mut open = Open {
            id,
            key_pts,
            max_frame_num: key.sps.max_frame_num(),
            max_refs: key.sps.max_refs.max(1),
            dpb: Vec::new(),
            prev_ref_frame_num: key.frame_num,
            prev: None,
            prev_msb: 0,
            prev_lsb: i64::from(key.poc_lsb),
            offset: 0,
            prev_frame_num: key.frame_num,
            past_next_key: false,
            fed: 0,
            verdict: None,
        };
        open.picture(key, Some(key_pts));
        // An entry point that is not itself a reference picture leaves the
        // cut's first inferred frames ordered from the head before it.
        if !key.reference {
            open.verdict = Some(false);
        }
        open
    }

    /// Whether the buffer holds anything that is not the same in the cut.
    ///
    /// **Dropping a leading reference picture leaves a gap in `frame_num`**,
    /// and the decoder of the cut infers a frame in its place: the same slot
    /// in the window, the same place in a P picture's list (which is ordered
    /// by `frame_num`) -- but not the same order count. libavcodec gives it
    /// the newest reference picture's plus two, which puts it *after* the
    /// entry point in a B picture's lists, where the leading picture stood
    /// before it; other decoders give it something else again. A pressed
    /// Blu-ray's leading B pair, the first a reference, the second B after
    /// the I taking the next P from list 1 by default: in the cut its list 1
    /// began with the frame inferred for the dropped B, and two pictures of
    /// every range opened there came out wrong (16 dB). So while such a
    /// frame is held, a B picture the cut keeps has to name every entry of
    /// its lists itself; see [`Open::picture`].
    fn alive(&self) -> bool {
        self.dpb.iter().any(|f| (f.leading || f.tainted) && f.held())
    }

    fn wrap(&self, frame_num: u32, current: u32) -> i64 {
        if frame_num > current {
            i64::from(frame_num) - i64::from(self.max_frame_num)
        } else {
            i64::from(frame_num)
        }
    }

    /// The sliding window, before a new frame takes a place.
    fn slide(&mut self, current: u32) {
        while self.dpb.len() >= self.max_refs as usize {
            let Some((i, _)) = self
                .dpb
                .iter()
                .enumerate()
                .min_by_key(|(_, f)| self.wrap(f.frame_num, current))
            else {
                return;
            };
            self.dpb.remove(i);
        }
    }

    fn step(&mut self, pics: Option<&[Picture]>, pts: Option<f64>, key: bool) {
        if self.verdict.is_some() {
            return;
        }
        if key {
            self.past_next_key = true;
        }
        let Some(pics) = pics else {
            self.verdict = Some(false);
            return;
        };
        for (n, pic) in pics.iter().enumerate() {
            self.fed += 1;
            if self.fed > CAP {
                self.verdict = Some(false);
                return;
            }
            self.picture(pic, if n == 0 { pts } else { None });
            if self.verdict.is_some() {
                return;
            }
        }
        if self.past_next_key && !self.alive() {
            self.verdict = Some(true);
        }
    }

    fn picture(&mut self, pic: &Picture, pts: Option<f64>) {
        if pic.long_term || (pic.sps.poc_type != 0 && pic.sps.poc_type != 2) {
            self.verdict = Some(false);
            return;
        }
        if pic.sps.max_frame_num() != self.max_frame_num {
            self.verdict = Some(false);
            return;
        }
        let first = self.fed == 0 && self.prev.is_none();
        if pic.idr && !first {
            // Nothing from before an IDR is referenced after it -- once the
            // leading pictures are all behind. One that is not an entry point
            // of its own is not a stream this follows.
            self.dpb.clear();
            self.verdict = Some(self.past_next_key);
            return;
        }
        let second = pic.field
            && self.prev.is_some_and(|p| {
                p.field && !p.second && p.bottom != pic.bottom && p.frame_num == pic.frame_num
            });
        let leading = !self.past_next_key
            && match pts {
                Some(t) => t < self.key_pts,
                None if second => self.prev.is_some_and(|p| p.leading),
                // A picture with no time of its own that is not the second
                // half of one that has: whether it is shown before the entry
                // point cannot be told.
                None => {
                    self.verdict = Some(false);
                    return;
                }
            };
        // A leading reference picture that marks the buffer itself does
        // what the cut, without it, does not -- **even where all it unmarks
        // is from before the entry point.** The cut still holds what it
        // unmarked (a frame inferred for its own gap, in a cut), and a held
        // picture older than the I is not out of the way: by order count it
        // stands below everything after the I, so in a B picture's list 0 it
        // comes in front of every picture shown after that B, and a list 0
        // with nothing below the B left in it is no longer the list 1 it was
        // the same as (the swap of 8.2.4.2.3). A trailing B reference that
        // unmarked the I, and the B after it on its default lists: in the
        // cut its list 0 began with the old frame, and it came out wrong.
        // So any marking of its own: needed.
        if leading && pic.reference && pic.marking.is_some() {
            self.verdict = Some(false);
            return;
        }

        // Gaps in frame_num, as 8.2.5.2 infers them.
        let max = self.max_frame_num;
        if !first
            && !second
            && pic.frame_num != self.prev_ref_frame_num
            && pic.frame_num != (self.prev_ref_frame_num + 1) % max
        {
            let mut unused = (self.prev_ref_frame_num + 1) % max;
            // What libavcodec gives the inferred frames their order from:
            // the newest frame held, and then each of them in turn.
            let tainted = self.dpb.last().is_some_and(|f| f.leading || f.tainted);
            // A gap longer than the buffer leaves nothing in it but the
            // frames inferred last: everything held before has been slid out,
            // the leading pictures with it. Only those last few are made.
            let gap = (pic.frame_num + max - unused) % max;
            if gap > self.max_refs {
                self.dpb.clear();
                unused = (pic.frame_num + max - self.max_refs) % max;
            }
            while unused != pic.frame_num {
                self.slide(unused);
                self.dpb.push(Frame {
                    frame_num: unused,
                    marked: [true, true],
                    poc: [None, None],
                    inferred: true,
                    leading: false,
                    tainted,
                    lone: false,
                });
                self.prev_ref_frame_num = unused;
                unused = (unused + 1) % max;
            }
        }

        // Order counts.
        let (top, bottom) = match pic.sps.poc_type {
            0 => {
                let max_lsb = 1i64 << pic.sps.poc_lsb_bits;
                let lsb = i64::from(pic.poc_lsb);
                let msb = if first {
                    0
                } else if lsb < self.prev_lsb && self.prev_lsb - lsb >= max_lsb / 2 {
                    self.prev_msb + max_lsb
                } else if lsb > self.prev_lsb && lsb - self.prev_lsb > max_lsb / 2 {
                    self.prev_msb - max_lsb
                } else {
                    self.prev_msb
                };
                if pic.reference {
                    self.prev_msb = msb;
                    self.prev_lsb = lsb;
                }
                let at = msb + lsb;
                if !pic.field {
                    (Some(at), Some(at + i64::from(pic.delta_bottom)))
                } else if pic.bottom {
                    (None, Some(at))
                } else {
                    (Some(at), None)
                }
            }
            _ => {
                if !first && pic.frame_num < self.prev_frame_num {
                    self.offset += i64::from(max);
                }
                self.prev_frame_num = pic.frame_num;
                let mut at = 2 * (self.offset + i64::from(pic.frame_num));
                if !pic.reference {
                    at -= 1;
                }
                if !pic.field {
                    (Some(at), Some(at))
                } else if pic.bottom {
                    (None, Some(at))
                } else {
                    (Some(at), None)
                }
            }
        };

        // The lists of a picture the copy keeps.
        if !leading && self.alive() {
            // A leading field held alone stands for one entry where the
            // frame inferred for it in the cut stands for two.
            if pic.slices.iter().any(|s| s.kind != Kind::I)
                && self.dpb.iter().any(|f| f.leading && f.lone && f.held())
            {
                self.verdict = Some(false);
                return;
            }
            for slice in &pic.slices {
                if slice.kind == Kind::I {
                    continue;
                }
                // Every entry named, or the order counts decide, and in the
                // cut they are not the same; see [`Open::alive`].
                if slice.kind == Kind::B
                    && (0..2).any(|x| (slice.changes[x].len() as u32) < slice.active[x])
                {
                    self.verdict = Some(false);
                    return;
                }
                let Some(lists) = self.lists(pic, slice, top, bottom) else {
                    self.verdict = Some(false);
                    return;
                };
                if lists
                    .iter()
                    .flatten()
                    .any(|&(i, _)| self.dpb.get(i).is_some_and(|f| f.leading))
                {
                    self.verdict = Some(false);
                    return;
                }
            }
        }

        // The marking.
        if pic.reference {
            let joins = second && self.prev.is_some_and(|p| p.reference);
            match &pic.marking {
                Some(ops) => {
                    for &(op, value) in ops {
                        match op {
                            1 => self.unmark(pic, value),
                            5 => {
                                self.verdict = Some(false);
                                return;
                            }
                            _ => {
                                self.verdict = Some(false);
                                return;
                            }
                        }
                    }
                }
                None if !joins => self.slide(pic.frame_num),
                None => {}
            }
            let parity = usize::from(pic.bottom);
            let joined = joins
                && self
                    .dpb
                    .iter_mut()
                    .rev()
                    .find(|f| !f.inferred && f.frame_num == pic.frame_num)
                    .map(|f| {
                        f.marked[parity] = true;
                        f.poc[parity] = if parity == 0 { top } else { bottom };
                        f.lone = false;
                    })
                    .is_some();
            if !joined {
                let mut marked = [true, true];
                if pic.field {
                    marked = [!pic.bottom, pic.bottom];
                }
                self.dpb.push(Frame {
                    frame_num: pic.frame_num,
                    marked,
                    poc: [top, bottom],
                    inferred: false,
                    leading,
                    tainted: false,
                    lone: pic.field,
                });
            }
            self.dpb.retain(Frame::held);
            self.prev_ref_frame_num = pic.frame_num;
        }
        self.prev = Some(Prev {
            frame_num: pic.frame_num,
            field: pic.field,
            bottom: pic.bottom,
            reference: pic.reference,
            second,
            leading,
        });
    }

    /// `memory_management_control_operation` 1.
    fn unmark(&mut self, pic: &Picture, diff: u32) {
        let cur = pic.frame_num;
        let current = if pic.field { 2 * i64::from(cur) + 1 } else { i64::from(cur) };
        let x = current - (i64::from(diff) + 1);
        let parity = usize::from(pic.bottom);
        for k in 0..self.dpb.len() {
            let w = self.wrap(self.dpb[k].frame_num, cur);
            let f = &mut self.dpb[k];
            if !pic.field {
                if w == x {
                    f.marked = [false, false];
                }
            } else if 2 * w + 1 == x {
                f.marked[parity] = false;
            } else if 2 * w == x {
                f.marked[1 - parity] = false;
            }
        }
        self.dpb.retain(Frame::held);
    }

    /// The two lists of one slice, as 8.2.4 builds them, at their active
    /// length. `None` where they cannot be built here.
    fn lists(
        &self,
        pic: &Picture,
        slice: &Slice,
        top: Option<i64>,
        bottom: Option<i64>,
    ) -> Option<[Vec<Entry>; 2]> {
        let cur = pic.frame_num;
        let usable: Vec<usize> = (0..self.dpb.len())
            .filter(|&i| {
                let f = &self.dpb[i];
                !f.inferred && if pic.field { f.held() } else { f.marked[0] && f.marked[1] }
            })
            .collect();
        let mut init: [Vec<Entry>; 2] = [Vec::new(), Vec::new()];
        match slice.kind {
            Kind::P => {
                let mut order = usable.clone();
                order.sort_by_key(|&i| std::cmp::Reverse(self.wrap(self.dpb[i].frame_num, cur)));
                init[0] = self.expand(&order, pic);
            }
            Kind::B => {
                let at = if pic.field {
                    if pic.bottom { bottom } else { top }
                } else {
                    top.zip(bottom).map(|(t, b)| t.min(b))
                }?;
                let keyed: Vec<(usize, i64)> = usable
                    .iter()
                    .map(|&i| {
                        let f = &self.dpb[i];
                        let o = if pic.field {
                            f.order()
                        } else {
                            f.poc[0].zip(f.poc[1]).map(|(t, b)| t.min(b))
                        };
                        o.map(|o| (i, o))
                    })
                    .collect::<Option<_>>()?;
                let before = |o: i64| if pic.field { o <= at } else { o < at };
                let mut below: Vec<(usize, i64)> =
                    keyed.iter().copied().filter(|&(_, o)| before(o)).collect();
                below.sort_by_key(|&(_, o)| std::cmp::Reverse(o));
                let mut above: Vec<(usize, i64)> =
                    keyed.iter().copied().filter(|&(_, o)| !before(o)).collect();
                above.sort_by_key(|&(_, o)| o);
                let l0: Vec<usize> = below.iter().chain(&above).map(|&(i, _)| i).collect();
                let l1: Vec<usize> = above.iter().chain(&below).map(|&(i, _)| i).collect();
                init[0] = self.expand(&l0, pic);
                init[1] = self.expand(&l1, pic);
                if init[1].len() > 1 && init[0] == init[1] {
                    init[1].swap(0, 1);
                }
            }
            Kind::I => return Some(init),
        }
        let lists = if slice.kind == Kind::B { 2 } else { 1 };
        let mut out: [Vec<Entry>; 2] = [Vec::new(), Vec::new()];
        for x in 0..lists {
            let n = slice.active[x] as usize;
            let mut list = self.explicit(pic, &slice.changes[x])?;
            let named = list.clone();
            list.extend(init[x].iter().copied().filter(|e| !named.contains(e)));
            list.truncate(n);
            out[x] = list;
        }
        Some(out)
    }

    /// Frames in list order to the entries a picture of this structure
    /// takes: as they are for a frame, and alternated by parity for a field
    /// (8.2.4.2.5), the current field's own parity first.
    fn expand(&self, frames: &[usize], pic: &Picture) -> Vec<Entry> {
        if !pic.field {
            return frames.iter().map(|&i| (i, None)).collect();
        }
        let same = usize::from(pic.bottom);
        let of = |parity: usize| -> Vec<Entry> {
            frames
                .iter()
                .filter(|&&i| self.dpb[i].marked[parity])
                .map(|&i| (i, Some(parity)))
                .collect()
        };
        let (a, b) = (of(same), of(1 - same));
        let mut out = Vec::with_capacity(a.len() + b.len());
        let (mut i, mut j) = (0, 0);
        while i < a.len() || j < b.len() {
            if i < a.len() {
                out.push(a[i]);
                i += 1;
            }
            if j < b.len() {
                out.push(b[j]);
                j += 1;
            }
        }
        out
    }

    /// The entries a slice's `ref_pic_list_modification` names, in order.
    /// One that names nothing held is a picture from before the entry point,
    /// which is not a leading one, and is left out.
    fn explicit(&self, pic: &Picture, changes: &[(u32, u32)]) -> Option<Vec<Entry>> {
        let cur = pic.frame_num;
        let (current, max_pic) = if pic.field {
            (2 * i64::from(cur) + 1, 2 * i64::from(self.max_frame_num))
        } else {
            (i64::from(cur), i64::from(self.max_frame_num))
        };
        let mut pred = current;
        let mut out = Vec::new();
        for &(idc, value) in changes {
            let step = i64::from(value) + 1;
            let no_wrap = match idc {
                0 => {
                    let v = pred - step;
                    if v < 0 { v + max_pic } else { v }
                }
                1 => {
                    let v = pred + step;
                    if v >= max_pic { v - max_pic } else { v }
                }
                _ => return None,
            };
            pred = no_wrap;
            let num = if no_wrap > current { no_wrap - max_pic } else { no_wrap };
            let parity = usize::from(pic.bottom);
            let found = self.dpb.iter().enumerate().find_map(|(i, f)| {
                let w = self.wrap(f.frame_num, cur);
                if !pic.field {
                    (w == num && f.marked[0] && f.marked[1]).then_some((i, None))
                } else if 2 * w + 1 == num && f.marked[parity] {
                    Some((i, Some(parity)))
                } else if 2 * w == num && f.marked[1 - parity] {
                    Some((i, Some(1 - parity)))
                } else {
                    None
                }
            });
            if let Some(e) = found {
                out.push(e);
            }
        }
        Some(out)
    }
}

/// Follows every entry point of an H.264 stream through the pictures after
/// it, packet by packet in decode order, and answers for each whether its
/// leading reference pictures are referenced by anything that is not a
/// leading picture of it. Other codecs are passed over.
pub(crate) struct Judge {
    on: bool,
    framing: NalFraming,
    sets: Sets,
    open: Vec<Open>,
    done: Vec<(usize, bool)>,
}

impl Judge {
    pub(crate) fn new(codec: &str, framing: NalFraming) -> Judge {
        Judge {
            on: codec == "h264" && !crate::index::off("SMARTCUT_LEAD_REFS"),
            framing,
            sets: Sets::default(),
            open: Vec::new(),
            done: Vec::new(),
        }
    }

    /// The next packet of the picture stream. `id` names an entry point
    /// (`key`) in what is answered; `pts` is on the same clock the caller
    /// decides leading pictures on, `None` where the packet has none.
    pub(crate) fn feed(&mut self, id: usize, key: bool, pts: Option<f64>, data: &[u8]) {
        if !self.on {
            return;
        }
        if self.open.is_empty() && !key {
            return;
        }
        let pics = pictures(&mut self.sets, data, self.framing);
        for open in &mut self.open {
            open.step(pics.as_deref(), pts, key);
        }
        let done = &mut self.done;
        self.open.retain(|o| match o.verdict {
            Some(v) => {
                done.push((o.id, v));
                false
            }
            None => true,
        });
        if key {
            if let (Some(t), Some(pics)) = (pts, pics) {
                if let Some(first) = pics.first() {
                    let mut open = Open::start(id, t, first);
                    if pics.len() > 1 {
                        open.step(Some(&pics[1..]), None, false);
                    }
                    match open.verdict {
                        Some(v) => self.done.push((id, v)),
                        None => self.open.push(open),
                    }
                    if self.open.len() > OPEN {
                        let oldest = self.open.remove(0);
                        self.done.push((oldest.id, false));
                    }
                }
            }
        }
    }

    /// What was decided. `whole` is whether the stream was read to its end:
    /// then a leading picture still held was never referenced by anything,
    /// and otherwise nothing can be said of it.
    pub(crate) fn finish(mut self, whole: bool) -> Vec<(usize, bool)> {
        for open in self.open.drain(..) {
            let v = !open.alive() && open.past_next_key || whole;
            self.done.push((open.id, v));
        }
        self.done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes the bits of a NAL unit's payload.
    #[derive(Default)]
    struct W {
        bits: Vec<bool>,
    }

    impl W {
        fn u(&mut self, n: usize, v: u32) -> &mut Self {
            for k in (0..n).rev() {
                self.bits.push((v >> k) & 1 == 1);
            }
            self
        }
        fn ue(&mut self, v: u32) -> &mut Self {
            let x = v + 1;
            let len = 32 - x.leading_zeros() as usize;
            self.u(len - 1, 0).u(len, x)
        }
        /// The NAL unit, start code and emulation prevention included.
        fn nal(&mut self, header: u8) -> Vec<u8> {
            self.bits.push(true);
            while self.bits.len() % 8 != 0 {
                self.bits.push(false);
            }
            let mut raw = vec![header];
            for c in self.bits.chunks(8) {
                raw.push(c.iter().fold(0u8, |a, &b| (a << 1) | u8::from(b)));
            }
            let mut out = vec![0, 0, 0, 1];
            let mut zeros = 0;
            for &b in &raw {
                if zeros >= 2 && b <= 3 {
                    out.push(3);
                    zeros = 0;
                }
                zeros = if b == 0 { zeros + 1 } else { 0 };
                out.push(b);
            }
            out
        }
    }

    fn sps(refs: u32) -> Vec<u8> {
        sps_as(refs, true)
    }

    fn sps_as(refs: u32, frames_only: bool) -> Vec<u8> {
        let mut w = W::default();
        w.u(8, 77).u(8, 0).u(8, 40).ue(0); // profile, flags, level, id
        w.ue(4).ue(0).ue(4); // frame_num 8 bits, poc type 0, lsb 8 bits
        w.ue(refs).u(1, 1).ue(39).ue(29); // refs, gaps allowed, size
        w.u(1, u32::from(frames_only));
        if !frames_only {
            w.u(1, 0); // mb_adaptive_frame_field_flag
        }
        w.u(1, 1).u(1, 0).u(1, 0); // direct 8x8, no crop, no vui
        w.nal(0x67)
    }

    fn pps() -> Vec<u8> {
        let mut w = W::default();
        w.ue(0).ue(0).u(1, 0).u(1, 0).ue(0).ue(0).ue(0).u(1, 0).u(2, 0);
        w.ue(0).ue(0).ue(0).u(1, 1).u(1, 0).u(1, 0);
        w.nal(0x68)
    }

    /// One frame's one slice.
    #[derive(Clone)]
    struct S {
        /// 5 P, 6 B, 7 I.
        kind: u32,
        reference: bool,
        frame_num: u32,
        poc: u32,
        active: Option<(u32, u32)>,
        /// `(modification_of_pic_nums_idc, abs_diff_pic_num_minus1)`.
        l0: Vec<(u32, u32)>,
        l1: Vec<(u32, u32)>,
        /// `memory_management_control_operation` 1 with these
        /// `difference_of_pic_nums_minus1`; the sliding window if `None`.
        unmark: Option<Vec<u32>>,
        /// Under a sequence that allows fields: `None` a frame, `Some`
        /// a field, bottom or not.
        paff: Option<Option<bool>>,
    }

    fn s(kind: u32, reference: bool, frame_num: u32, poc: u32) -> S {
        S {
            kind,
            reference,
            frame_num,
            poc,
            active: None,
            l0: vec![],
            l1: vec![],
            unmark: None,
            paff: None,
        }
    }

    impl S {
        fn active(mut self, a: u32, b: u32) -> S {
            self.active = Some((a, b));
            self
        }
        fn l0(mut self, c: &[(u32, u32)]) -> S {
            self.l0 = c.to_vec();
            self
        }
        fn l1(mut self, c: &[(u32, u32)]) -> S {
            self.l1 = c.to_vec();
            self
        }
        fn unmark(mut self, d: &[u32]) -> S {
            self.unmark = Some(d.to_vec());
            self
        }
        fn paff(mut self, field: Option<bool>) -> S {
            self.paff = Some(field);
            self
        }
        fn bytes(&self) -> Vec<u8> {
            let mut w = W::default();
            w.ue(0).ue(self.kind).ue(0).u(8, self.frame_num);
            if let Some(field) = self.paff {
                w.u(1, u32::from(field.is_some()));
                if let Some(bottom) = field {
                    w.u(1, u32::from(bottom));
                }
            }
            w.u(8, self.poc);
            if self.kind == 6 {
                w.u(1, 1); // direct_spatial_mv_pred_flag
            }
            if self.kind != 7 {
                match self.active {
                    Some((a, b)) => {
                        w.u(1, 1).ue(a - 1);
                        if self.kind == 6 {
                            w.ue(b - 1);
                        }
                    }
                    None => {
                        w.u(1, 0);
                    }
                }
                let lists: &[&Vec<(u32, u32)>] =
                    if self.kind == 6 { &[&self.l0, &self.l1] } else { &[&self.l0] };
                for list in lists {
                    if list.is_empty() {
                        w.u(1, 0);
                    } else {
                        w.u(1, 1);
                        for &(idc, v) in list.iter() {
                            w.ue(idc).ue(v);
                        }
                        w.ue(3);
                    }
                }
            }
            if self.reference {
                match &self.unmark {
                    None => {
                        w.u(1, 0);
                    }
                    Some(ds) => {
                        w.u(1, 1);
                        for &d in ds {
                            w.ue(1).ue(d);
                        }
                        w.ue(0);
                    }
                }
            }
            w.ue(0); // slice_qp_delta, so there is something after
            w.nal(if self.reference { 0x21 } else { 0x01 })
        }
    }

    /// The key: I, frame_num 0, order 8, shown at 4.
    fn key(refs: u32) -> Vec<u8> {
        let mut k = sps(refs);
        k.extend(pps());
        k.extend(s(7, true, 0, 8).bytes());
        k
    }

    /// The leading pair: a reference B shown at 2 and a B shown at 3 that
    /// predicts from it.
    fn leading_pair() -> Vec<(bool, Option<f64>, Vec<u8>)> {
        vec![
            (false, Some(2.0), s(6, true, 1, 4).active(1, 1).bytes()),
            (false, Some(3.0), s(6, false, 2, 6).active(1, 1).bytes()),
        ]
    }

    /// The verdict on the key, fed `after` (each shown at `t`), then the
    /// next entry point.
    fn verdict(refs: u32, after: &[(f64, S)], whole: bool) -> Option<bool> {
        let mut packets = vec![(true, Some(4.0), key(refs))];
        packets.extend(leading_pair());
        for (t, sl) in after {
            packets.push((false, Some(*t), sl.bytes()));
        }
        packets.push((true, Some(20.0), key(refs)));
        run(&packets, whole)
    }

    fn run(packets: &[(bool, Option<f64>, Vec<u8>)], whole: bool) -> Option<bool> {
        let mut judge = Judge::new("h264", NalFraming::AnnexB);
        for (id, (k, t, d)) in packets.iter().enumerate() {
            judge.feed(id, *k, *t, d);
        }
        judge.finish(whole).into_iter().find(|&(id, _)| id == 0).map(|(_, v)| v)
    }

    // CurrPicNum 2 to the I's 0: abs_diff_pic_num_minus1 1.
    fn p_on_the_i() -> S {
        s(5, true, 2, 14).active(1, 1).l0(&[(0, 1)])
    }

    // A B frame_num 3, order 10: the I is 0 (3 - 3), the P 2 (3 - 1).
    fn b_on_i_and_p() -> S {
        s(6, false, 3, 10).active(1, 1).l0(&[(0, 2)]).l1(&[(0, 0)])
    }

    #[test]
    fn everything_named_past_the_leading_pair_leaves_it_droppable() {
        let after = [(7.0, p_on_the_i()), (5.0, b_on_i_and_p())];
        assert_eq!(verdict(4, &after, true), Some(true));
    }

    #[test]
    fn a_p_on_its_default_list_takes_the_leading_reference() {
        // Default order is by frame_num, newest first: the leading B.
        let p = s(5, true, 2, 14).active(1, 1);
        assert_eq!(verdict(4, &[(7.0, p), (5.0, b_on_i_and_p())], true), Some(false));
    }

    #[test]
    fn a_p_whose_list_runs_on_past_what_it_names_takes_the_leading_reference() {
        // The I named first, then the default order: the leading B.
        let p = s(5, true, 2, 14).active(2, 1).l0(&[(0, 1)]);
        assert_eq!(verdict(4, &[(7.0, p), (5.0, b_on_i_and_p())], true), Some(false));
    }

    #[test]
    fn a_trailing_b_whose_list_1_names_the_leading_reference_takes_it() {
        // List 1: the P, then the leading B (picture number 1).
        let b = s(6, false, 3, 10).active(1, 2).l0(&[(0, 2)]).l1(&[(0, 0), (0, 0)]);
        assert_eq!(verdict(4, &[(7.0, p_on_the_i()), (5.0, b)], true), Some(false));
    }

    /// The pressed Blu-ray's shape: list 0 names the I, list 1 is left to
    /// the order counts. In the cut the frame inferred for the dropped B
    /// stands at the head of list 1 (libavcodec orders it 10, between the I
    /// and the P), so this is not droppable whatever the original's lists say.
    #[test]
    fn a_trailing_b_on_a_default_list_while_a_leading_reference_is_held_is_not_trusted() {
        let b = s(6, false, 3, 10).active(1, 1).l0(&[(0, 2)]);
        assert_eq!(verdict(4, &[(7.0, p_on_the_i()), (5.0, b)], true), Some(false));
        let b = s(6, false, 3, 10).active(1, 1);
        assert_eq!(verdict(4, &[(7.0, p_on_the_i()), (5.0, b)], true), Some(false));
    }

    /// The recorder's shape: the first P unmarks the leading B, and the B
    /// pictures after it are free to use their default lists.
    #[test]
    fn a_leading_reference_unmarked_by_the_first_p_frees_the_default_lists() {
        // CurrPicNum 2, the leading B's 1: difference_of_pic_nums_minus1 0.
        let p = p_on_the_i().unmark(&[0]);
        let b = s(6, false, 3, 10).active(2, 2);
        assert_eq!(verdict(4, &[(7.0, p), (5.0, b)], true), Some(true));
    }

    /// A gap in `frame_num` right after the leading pair: the frame inferred
    /// for it is ordered from the leading B in the recording and from
    /// something else in the cut, so it holds the B pictures to named lists
    /// for as long as it is held, as the leading B itself did.
    #[test]
    fn a_frame_inferred_after_a_leading_reference_is_not_trusted_either() {
        // P frame_num 3 (2 inferred): CurrPicNum 3, the I 0, the leading B 1.
        let p = s(5, true, 3, 14).active(1, 1).l0(&[(0, 2)]).unmark(&[1]);
        let b = s(6, false, 4, 10).active(1, 1);
        assert_eq!(verdict(4, &[(7.0, p.clone()), (5.0, b.clone())], true), Some(false));
        // The inferred frame (2) unmarked as well: nothing is left.
        let p = p.unmark(&[1, 0]);
        assert_eq!(verdict(4, &[(7.0, p), (5.0, b)], true), Some(true));
    }

    /// A leading reference that marks the buffer does what the cut does not
    /// -- whatever it unmarks. One that unmarks only what came before the
    /// entry point leaves that held in the cut, below everything after the
    /// I: a B picture shown just after the I, once a trailing B reference
    /// has unmarked the I, has nothing below it in the recording and that
    /// old frame in front of its list 0 in the cut (and its list 1 no longer
    /// swapped). Found by editing an x264 stream's headers into this shape;
    /// decoded with and without the leading pictures, that B differed.
    #[test]
    fn a_leading_reference_that_marks_the_buffer_is_needed() {
        let with = |diff: Option<u32>| {
            let mut packets = vec![(true, Some(4.0), key(4))];
            let mut lead = s(6, true, 1, 4).active(1, 1);
            if let Some(d) = diff {
                lead = lead.unmark(&[d]);
            }
            packets.push((false, Some(2.0), lead.bytes()));
            packets.push((false, Some(3.0), s(6, false, 2, 6).active(1, 1).bytes()));
            // The first P unmarks the leading B (2 - 1), as a recorder's does.
            packets.push((false, Some(7.0), p_on_the_i().unmark(&[0]).bytes()));
            // A B reference that unmarks the I (CurrPicNum 3, the I 0).
            packets.push((false, Some(5.5), s(6, true, 3, 12).active(1, 1).unmark(&[2]).bytes()));
            // And a B on its default lists, shown before both that is left.
            packets.push((false, Some(4.5), s(6, false, 4, 10).active(1, 1).bytes()));
            packets.push((true, Some(20.0), key(4)));
            run(&packets, true)
        };
        // CurrPicNum 1: difference 0 is the I (0).
        assert_eq!(with(Some(0)), Some(false));
        // Difference 3 is frame_num 253 of the 256: from before the I.
        assert_eq!(with(Some(3)), Some(false));
        // Marking nothing itself, it leaves nothing behind in the cut.
        assert_eq!(with(None), Some(true));
    }

    #[test]
    fn a_picture_with_no_time_that_is_not_a_second_field_is_not_guessed_at() {
        let mut packets = vec![(true, Some(4.0), key(4))];
        packets.extend(leading_pair());
        packets.push((false, None, p_on_the_i().bytes()));
        packets.push((false, Some(5.0), b_on_i_and_p().bytes()));
        packets.push((true, Some(20.0), key(4)));
        assert_eq!(run(&packets, true), Some(false));
    }

    #[test]
    fn a_reference_still_held_where_the_read_stops_is_needed() {
        let mut packets = vec![(true, Some(4.0), key(4))];
        packets.extend(leading_pair());
        packets.push((false, Some(7.0), p_on_the_i().bytes()));
        packets.push((false, Some(5.0), b_on_i_and_p().bytes()));
        assert_eq!(run(&packets, false), Some(false));
        assert_eq!(run(&packets, true), Some(true));
    }

    #[test]
    fn other_codecs_are_not_followed() {
        let mut judge = Judge::new("hevc", NalFraming::AnnexB);
        judge.feed(0, true, Some(0.0), &sps(4));
        assert!(judge.finish(true).is_empty());
    }

    /// A leading reference that is one field, its other half not a
    /// reference, holds one entry of a list where the frame the cut infers
    /// for it holds two: needed. As a pair, droppable as any other.
    #[test]
    fn a_leading_reference_field_held_alone_is_needed() {
        let with = |bottom_ref: bool| {
            let mut key = sps_as(4, false);
            key.extend(pps());
            key.extend(s(7, true, 0, 8).paff(None).bytes());
            let mut b0 = s(6, true, 1, 4).active(1, 1).paff(Some(false));
            b0.l0 = vec![];
            let b1 = s(6, bottom_ref, 1, 5).active(1, 1).paff(Some(true));
            let packets = vec![
                (true, Some(4.0), key.clone()),
                (false, Some(2.0), b0.bytes()),
                (false, None, b1.bytes()),
                (false, Some(3.0), s(6, false, 2, 6).active(1, 1).paff(None).bytes()),
                (false, Some(7.0), p_on_the_i().paff(None).bytes()),
                (false, Some(5.0), b_on_i_and_p().paff(None).bytes()),
                (true, Some(20.0), key),
            ];
            run(&packets, true)
        };
        assert_eq!(with(false), Some(false));
        assert_eq!(with(true), Some(true));
    }

    /// Bits flipped, bytes cut away, bytes repeated: never a panic, and an
    /// answer for the entry point every time it could be read at all.
    #[test]
    fn a_mangled_stream_is_answered_or_given_up_on() {
        let mut packets = vec![(true, Some(4.0), key(4))];
        packets.extend(leading_pair());
        for (t, sl) in [(7.0, p_on_the_i()), (5.0, b_on_i_and_p())] {
            packets.push((false, Some(t), sl.bytes()));
        }
        for n in 0..12 {
            let t = 8.0 + f64::from(n);
            let sl = s(5, true, 3 + n, 16 + 2 * n).active(1 + n % 3, 1).l0(&[(0, n % 4)]);
            packets.push((false, Some(t), sl.bytes()));
        }
        packets.push((true, Some(30.0), key(4)));
        let mut seed: u64 = 0x5eed;
        let mut next = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };
        for _ in 0..4000 {
            let mut mangled = packets.clone();
            for _ in 0..1 + next() % 4 {
                let k = next() % mangled.len();
                let d = &mut mangled[k].2;
                if d.is_empty() {
                    continue;
                }
                match next() % 4 {
                    0 | 1 => {
                        let at = next() % d.len();
                        d[at] ^= 1 << (next() % 8);
                    }
                    2 => {
                        let at = next() % d.len();
                        d.truncate(at);
                    }
                    _ => {
                        let at = next() % d.len();
                        let tail = d[at..].to_vec();
                        d.extend(tail);
                    }
                }
            }
            let _ = run(&mangled, next() % 2 == 0);
        }
    }
}
