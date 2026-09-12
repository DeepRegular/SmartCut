//! Reading picture properties straight out of packet payloads.
//!
//! The only question asked here is whether a picture is used as a reference,
//! which decides if an open-GOP entry point's leading pictures can be cut
//! away. The Python prototype had to shell out to ffmpeg and re-extract an
//! Annex-B window to answer it; with libav the packet is already in hand.

/// How NAL units are framed inside a packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NalFraming {
    /// Start codes, as in MPEG-TS.
    AnnexB,
    /// Length prefixes of the given width, as in MP4.
    Length(usize),
}

/// Read the NAL length size out of an `avcC` / `hvcC` extradata blob.
pub fn framing_from_extradata(codec: &str, extradata: &[u8]) -> NalFraming {
    let offset = match codec {
        "h264" => 4,  // avcC: configurationVersion, profile, compat, level
        "hevc" => 21, // hvcC: fixed header before lengthSizeMinusOne
        _ => return NalFraming::AnnexB,
    };
    match extradata.get(offset) {
        // an avcC/hvcC always starts with configurationVersion 1; anything
        // else (or a start code) means the stream is already Annex-B
        Some(b) if extradata.first() == Some(&1) => NalFraming::Length((b & 0x03) as usize + 1),
        _ => NalFraming::AnnexB,
    }
}

/// Offsets of each NAL unit's payload within a packet.
fn nal_payloads(data: &[u8], framing: NalFraming) -> Vec<&[u8]> {
    let mut out = Vec::new();
    match framing {
        NalFraming::Length(n) => {
            let mut i = 0;
            while i + n <= data.len() {
                let mut len = 0usize;
                for k in 0..n {
                    len = (len << 8) | data[i + k] as usize;
                }
                i += n;
                if len == 0 || i + len > data.len() {
                    break;
                }
                out.push(&data[i..i + len]);
                i += len;
            }
        }
        NalFraming::AnnexB => {
            let mut starts = Vec::new();
            let mut i = 0;
            while i + 3 <= data.len() {
                if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                    starts.push(i + 3);
                    i += 3;
                } else {
                    i += 1;
                }
            }
            for (k, &s) in starts.iter().enumerate() {
                let end = starts.get(k + 1).map_or(data.len(), |&n| {
                    // trim the next start code, and its optional leading zero
                    let e = n - 3;
                    if e > s && data[e - 1] == 0 {
                        e - 1
                    } else {
                        e
                    }
                });
                if s < end {
                    out.push(&data[s..end]);
                }
            }
        }
    }
    out
}

const H264_VCL: std::ops::RangeInclusive<u8> = 1..=5;
/// HEVC leading pictures: the `_N` variants are sub-layer non-reference.
const HEVC_LEADING_NONREF: [u8; 2] = [6, 8]; // RADL_N, RASL_N

/// Is the picture in this packet used as a reference by later pictures?
///
/// Decides whether a leading picture may simply be cut out of a copied
/// segment. MPEG-2 B pictures never reference-back, but H.264/HEVC encoders
/// routinely build B-pyramids whose leading pictures *are* references -- drop
/// one of those and every picture that depended on it decodes to garbage.
///
/// `vc1` is the pair of headers a VC-1 stream declares itself with. It is the
/// one codec here whose pictures cannot be read without them -- whether a
/// picture even states its type in three bits or in one is settled in the
/// sequence header -- and a stream that never produced them is answered for
/// conservatively.
pub fn is_reference(
    data: &[u8],
    codec: &str,
    framing: NalFraming,
    vc1: Option<&smartcut_vc1::Shape>,
) -> bool {
    match codec {
        "vc1" | "wmv3" => match vc1.and_then(|shape| shape.picture(data)) {
            Some(picture) => picture.reference(),
            // Taking a picture for a reference costs only the chance to
            // enter an open GOP at it; taking a reference for a B picture
            // would cut away something the rest of the GOP is decoded from.
            None => true,
        },
        "mpeg2video" | "mpeg4" => {
            // picture_coding_type sits just past the 10-bit temporal_reference
            // of a picture header: 1=I, 2=P, 3=B. Only B is never referenced.
            let mut i = 0;
            while i + 6 <= data.len() {
                if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 && data[i + 3] == 0 {
                    return (data[i + 5] >> 3) & 0x07 != 3;
                }
                i += 1;
            }
            true
        }
        "hevc" => {
            for nal in nal_payloads(data, framing) {
                let Some(&b) = nal.first() else { continue };
                let nal_type = (b >> 1) & 0x3F;
                if nal_type < 32 {
                    return !HEVC_LEADING_NONREF.contains(&nal_type);
                }
            }
            true
        }
        _ => {
            for nal in nal_payloads(data, framing) {
                let Some(&b) = nal.first() else { continue };
                if H264_VCL.contains(&(b & 0x1F)) {
                    return (b >> 5) & 0x03 != 0; // nal_ref_idc
                }
            }
            true
        }
    }
}

/// Does the picture in this packet restart the coded video sequence?
///
/// **The two things a Blu-ray or a broadcast may put at an entry point are
/// not equally good places to splice onto.** An IDR restarts everything: the
/// pictures after it reference nothing before it, and a decoder meeting one
/// empties what it was holding. An I picture with a recovery point is only a
/// place to *start reading* -- the pictures after it may still reference
/// pictures before it, which is exactly what a splice does not have. Joining
/// a copied segment onto one of those leaves its opening pictures predicted
/// from whatever the decoder happens to be holding, which after a re-encoded
/// run is the pictures this program just wrote; on one disc that put a
/// picture of the outgoing scene a frame *after* the incoming one.
///
/// Every codec here other than H.264 and HEVC states its pictures' display
/// order within the group they belong to and starts each group afresh, so
/// every entry point of one of those is already a clean start and the answer
/// is yes.
pub fn starts_a_sequence(data: &[u8], codec: &str, framing: NalFraming) -> bool {
    match codec {
        "h264" => nal_payloads(data, framing)
            .iter()
            .any(|nal| nal.first().is_some_and(|b| b & 0x1F == 5)),
        // 19 and 20 are the IDRs. The other intra random access pictures --
        // a clean random access, chiefly -- are the recovery point's
        // equivalent here: a place to start reading whose leading pictures
        // may reference what came before.
        "hevc" => nal_payloads(data, framing).iter().any(|nal| {
            nal.first()
                .is_some_and(|b| matches!((b >> 1) & 0x3F, 19 | 20))
        }),
        _ => true,
    }
}

/// Pull the parameter sets out of an `avcC` / `hvcC` extradata blob.
///
/// They have to be re-inserted in front of every copied keyframe. A
/// re-encoded segment carries its own SPS in-band, and once the decoder
/// activates that one, the copied pictures that follow would be decoded
/// against the wrong parameter set unless the original is restated.
pub fn parameter_sets(codec: &str, extradata: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let take = |d: &[u8], i: &mut usize, out: &mut Vec<Vec<u8>>| {
        if *i + 2 > d.len() {
            return false;
        }
        let len = ((d[*i] as usize) << 8) | d[*i + 1] as usize;
        *i += 2;
        if *i + len > d.len() {
            return false;
        }
        out.push(d[*i..*i + len].to_vec());
        *i += len;
        true
    };
    match codec {
        "h264" => {
            if extradata.len() < 7 || extradata[0] != 1 {
                return out;
            }
            let mut i = 5;
            let n_sps = (extradata[i] & 0x1F) as usize;
            i += 1;
            for _ in 0..n_sps {
                if !take(extradata, &mut i, &mut out) {
                    return out;
                }
            }
            if i >= extradata.len() {
                return out;
            }
            let n_pps = extradata[i] as usize;
            i += 1;
            for _ in 0..n_pps {
                if !take(extradata, &mut i, &mut out) {
                    return out;
                }
            }
        }
        "hevc" => {
            if extradata.len() < 23 || extradata[0] != 1 {
                return out;
            }
            let mut i = 22;
            let arrays = extradata[i] as usize;
            i += 1;
            for _ in 0..arrays {
                if i + 3 > extradata.len() {
                    return out;
                }
                i += 1; // array_completeness | NAL_unit_type
                let count = ((extradata[i] as usize) << 8) | extradata[i + 1] as usize;
                i += 2;
                for _ in 0..count {
                    if !take(extradata, &mut i, &mut out) {
                        return out;
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// Does this payload start with a start code?
pub fn is_annexb(data: &[u8]) -> bool {
    data.len() >= 4
        && data[0] == 0
        && data[1] == 0
        && (data[2] == 1 || (data[2] == 0 && data[3] == 1))
}

fn push_length_prefixed(out: &mut Vec<u8>, nal: &[u8], n: usize) {
    let len = nal.len();
    for k in (0..n).rev() {
        out.push((len >> (8 * k)) as u8);
    }
    out.extend_from_slice(nal);
}

/// Re-frame an Annex-B payload with length prefixes, as MP4 stores them.
pub fn annexb_to_length(data: &[u8], n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    for nal in nal_payloads(data, NalFraming::AnnexB) {
        push_length_prefixed(&mut out, nal, n);
    }
    out
}

/// Re-frame a length-prefixed payload with start codes, as MPEG-TS wants it.
///
/// The other direction from [`annexb_to_length`], and needed for the same
/// reason: a recording read out of an MP4 carries lengths, and a transport
/// stream carries start codes. Without this the copied pictures reach the
/// file as they were -- four bytes of length where a decoder is looking for
/// `00 00 00 01` -- and everything but the re-encoded fringes is unreadable.
pub fn length_to_annexb(data: &[u8], n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    for nal in nal_payloads(data, NalFraming::Length(n)) {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
    }
    out
}

/// Put the given parameter sets in front of a payload, with start codes.
///
/// An MP4 keeps its parameter sets in the `hvcC`/`avcC` and out of the
/// pictures; a transport stream expects to meet them in the stream itself,
/// in front of the pictures that were coded against them.
pub fn prepend_parameter_sets_annexb(data: &[u8], sets: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + sets.iter().map(|s| s.len() + 4).sum::<usize>());
    for s in sets {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(s);
    }
    out.extend_from_slice(data);
    out
}

/// Put the given parameter sets in front of a length-prefixed payload.
pub fn prepend_parameter_sets(data: &[u8], sets: &[Vec<u8>], n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + sets.iter().map(|s| s.len() + n).sum::<usize>());
    for s in sets {
        push_length_prefixed(&mut out, s, n);
    }
    out.extend_from_slice(data);
    out
}

/// How many fields this picture occupies: two normally, more under pulldown.
pub fn display_fields(data: &[u8], codec: &str, vc1: Option<&smartcut_vc1::Shape>) -> i64 {
    match codec {
        "mpeg2video" if mpeg2_repeats_field(data) => 3,
        // VC-1 says it in the picture header: a repeated field, or -- where
        // the stream is progressive or segmented -- a whole repeated frame.
        "vc1" | "wmv3" => vc1
            .and_then(|shape| shape.picture(data))
            .map_or(2, |p| p.display_fields()),
        _ => 2,
    }
}

/// Does this MPEG-2 picture ask for an extra field to be shown?
///
/// `repeat_first_field` is how 24 fps film is carried in a 29.97 stream:
/// every other picture is displayed for three fields instead of two. The
/// pictures then do not arrive at a constant rate, which breaks any output
/// timeline built from a single frame duration.
pub fn mpeg2_repeats_field(data: &[u8]) -> bool {
    mpeg2_coding_extensions(data)
        .first()
        .is_some_and(|e| e[3] & 0x02 != 0)
}

/// The picture coding extensions in a packet, each from its extension id
/// onwards. Everything MPEG-2 says about how a picture is shown is in here,
/// and there is one per picture -- which is not always one per packet. See
/// [`is_field_picture`].
fn mpeg2_coding_extensions(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 8 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 && data[i + 3] == 0xB5 {
            // picture coding extension: extension id 0x8 in the top nibble
            if data[i + 4] >> 4 == 0x8 {
                out.push(&data[i + 4..]);
                i += 8;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// What an H.264 recording has to be read against for a picture to say
/// whether it is a whole frame or one field of a pair.
///
/// MPEG-2 writes the answer plainly in every picture it codes and needs
/// nothing kept. H.264 writes it as a single bit of the slice header, and
/// where that bit sits depends on numbers that appear nowhere but the
/// sequence parameter set -- so the sequence has to be read first and kept,
/// the way a VC-1 stream's has to. See [`is_reference`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldShape {
    /// Width of `frame_num`, the one field of variable width standing
    /// between the start of a slice header and the flag being looked for.
    frame_num_bits: usize,
    /// Set where each colour plane is coded on its own, which puts one more
    /// field in front of `frame_num`. 4:4:4 only, and vanishingly rare.
    separate_colour_planes: bool,
}

/// Read out of a recording's sequence header what is needed to tell a field
/// picture from a whole frame.
///
/// `data` is a packet or a container's extradata; either may carry the
/// header, and a transport stream restates it in front of every entry point.
/// `None` where the sequence says every picture in it is a whole frame,
/// which is what all but a handful of recordings say -- and `None` again
/// where no header was found or it could not be read. Both answers mean the
/// same thing to a caller: nothing here is coded as fields.
pub fn field_shape(codec: &str, data: &[u8], framing: NalFraming) -> Option<FieldShape> {
    if codec != "h264" {
        return None;
    }
    // An `avcC` is not NAL-framed and has to be unpacked before it can be
    // searched; a packet -- and a transport stream's Annex-B extradata --
    // already is.
    let packed = parameter_sets(codec, data);
    if packed.iter().any(|nal| is_h264_sps(nal)) {
        return packed.iter().find_map(|nal| h264_field_shape(nal));
    }
    nal_payloads(data, framing)
        .into_iter()
        .find_map(h264_field_shape)
}

fn is_h264_sps(nal: &[u8]) -> bool {
    nal.first().is_some_and(|b| b & 0x1F == 7)
}

/// Profiles whose sequence header states its chroma format, and may carry
/// quantiser matrices, ahead of everything read below.
const H264_CHROMA_PROFILES: &[u32] =
    &[100, 110, 122, 244, 44, 83, 86, 118, 128, 134, 135, 138, 139];

fn h264_field_shape(nal: &[u8]) -> Option<FieldShape> {
    if !is_h264_sps(nal) {
        return None;
    }
    let mut b = Bits::new(nal.get(1..)?); // past the one-byte NAL header
    let profile = b.u(8)?;
    b.skip(16)?; // constraint and reserved flags, then level_idc
    b.ue()?; // seq_parameter_set_id
    let mut separate_colour_planes = false;
    if H264_CHROMA_PROFILES.contains(&profile) {
        let chroma = b.ue()?;
        if chroma == 3 {
            separate_colour_planes = b.u(1)? == 1;
        }
        b.ue()?; // bit_depth_luma_minus8
        b.ue()?; // bit_depth_chroma_minus8
        b.skip(1)?; // qpprime_y_zero_transform_bypass_flag
        if b.u(1)? == 1 {
            h264_scaling_lists(&mut b, if chroma == 3 { 12 } else { 8 })?;
        }
    }
    let frame_num_bits = b.ue()? as usize + 4;
    if frame_num_bits > 32 {
        return None;
    }
    match b.ue()? {
        // pic_order_cnt_type 0 and 1 each carry their own tail; 2 carries
        // nothing.
        0 => {
            b.ue()?; // log2_max_pic_order_cnt_lsb_minus4
        }
        1 => {
            b.skip(1)?; // delta_pic_order_always_zero_flag
            b.se()?; // offset_for_non_ref_pic
            b.se()?; // offset_for_top_to_bottom_field
            let cycle = b.ue()?;
            if cycle > 255 {
                return None;
            }
            for _ in 0..cycle {
                b.se()?;
            }
        }
        _ => {}
    }
    b.ue()?; // max_num_ref_frames
    b.skip(1)?; // gaps_in_frame_num_value_allowed_flag
    b.ue()?; // pic_width_in_mbs_minus1
    b.ue()?; // pic_height_in_map_units_minus1
    let frame_mbs_only = b.u(1)? == 1;
    // Set, and every picture in the sequence is a whole frame: no picture
    // need be asked, and nothing after this is read.
    (!frame_mbs_only).then_some(FieldShape {
        frame_num_bits,
        separate_colour_planes,
    })
}

/// Step over the quantiser matrices a sequence header may write for itself.
fn h264_scaling_lists(b: &mut Bits, lists: usize) -> Option<()> {
    for i in 0..lists {
        if b.u(1)? == 0 {
            continue;
        }
        // A matrix is written as differences and stops early at a zero, so
        // how many are actually there can only be found by reading them.
        let (mut last, mut next) = (8i32, 8i32);
        for _ in 0..if i < 6 { 16 } else { 64 } {
            if next != 0 {
                next = (last + b.se()? + 256).rem_euclid(256);
            }
            if next != 0 {
                last = next;
            }
        }
    }
    Some(())
}

/// Does this packet carry one field of a pair rather than a whole frame?
///
/// A recording coded as field pairs hands the two halves of a frame over
/// separately. **The output timeline counts in fields** -- see the writer --
/// so the pair takes two places on it and lasts a field each, where
/// [`display_fields`] answers for a packet that is a whole frame.
///
/// **The question is about the packet, not about how the pictures inside it
/// are coded.** libavcodec's MPEG-2 parser joins a complementary field pair
/// into one packet, and a broadcast in the sample here does that seven times
/// in half a minute: two field pictures, one frame, two fields of the
/// timeline. Its H.264 parser hands each field of a pair over on its own,
/// which is what a recorder's own disc is made of. So what is counted is the
/// pictures the packet opens, and the answer is yes only where there is one
/// of them and it is a field.
///
/// `shape` is what the sequence header said, and `None` -- from a recording
/// that codes no fields, or one whose header could not be read -- is
/// answered no: a frame taken for a field is half a frame missing from the
/// output, while a field taken for a frame is what every recording before
/// this one was.
pub fn is_field_picture(
    data: &[u8],
    codec: &str,
    framing: NalFraming,
    shape: Option<&FieldShape>,
) -> bool {
    match codec {
        // picture_structure in the picture coding extension: 3 is a frame,
        // 1 and 2 are the top and the bottom field on their own. A field
        // picture may not also ask for a repeat, so it is one field exactly.
        "mpeg2video" => match mpeg2_coding_extensions(data).as_slice() {
            [only] => only[2] & 0x03 != 3,
            _ => false,
        },
        "h264" => {
            let Some(shape) = shape else { return false };
            let mut opened = None;
            for nal in nal_payloads(data, framing) {
                if !nal.first().is_some_and(|b| H264_VCL.contains(&(b & 0x1F))) {
                    continue;
                }
                // A picture written in several slices opens once; the slices
                // after the first carry on from a macroblock further in.
                let Some(field) = h264_opens_a_picture(nal, shape) else {
                    continue;
                };
                if opened.is_some() {
                    return false; // a second picture: a whole frame's worth
                }
                opened = Some(field);
            }
            opened.unwrap_or(false)
        }
        _ => false,
    }
}

/// `Some(field_pic_flag)` where this slice opens a picture, `None` where it
/// carries on one already opened.
fn h264_opens_a_picture(nal: &[u8], shape: &FieldShape) -> Option<bool> {
    let mut b = Bits::new(nal.get(1..)?); // past the one-byte NAL header
    if b.ue()? != 0 {
        return None; // first_mb_in_slice
    }
    b.ue()?; // slice_type
    b.ue()?; // pic_parameter_set_id
    if shape.separate_colour_planes {
        b.skip(2)?; // colour_plane_id
    }
    b.skip(shape.frame_num_bits)?;
    Some(b.u(1)? == 1)
}

/// The bits of a NAL unit's payload, with the emulation prevention bytes
/// taken back out.
///
/// Every read is fallible and every one is checked, because this reads a
/// sequence header out of a recording nobody vouched for: a truncated or
/// mangled one has to come back as "no answer", never as a panic or a loop.
struct Bits {
    data: Vec<u8>,
    pos: usize,
}

impl Bits {
    fn new(nal: &[u8]) -> Self {
        let mut data = Vec::with_capacity(nal.len());
        let mut i = 0;
        while i < nal.len() {
            if i + 2 < nal.len() && nal[i] == 0 && nal[i + 1] == 0 && nal[i + 2] == 3 {
                data.extend_from_slice(&[0, 0]);
                i += 3;
            } else {
                data.push(nal[i]);
                i += 1;
            }
        }
        Bits { data, pos: 0 }
    }

    fn u(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.data.get(self.pos >> 3)?;
            v = (v << 1) | u32::from((byte >> (7 - (self.pos & 7))) & 1);
            self.pos += 1;
        }
        Some(v)
    }

    /// Step over bits whose value is not wanted -- runs of reserved flags,
    /// chiefly, which are longer than a single read may be.
    fn skip(&mut self, n: usize) -> Option<()> {
        self.pos = self.pos.checked_add(n)?;
        (self.pos <= self.data.len() * 8).then_some(())
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0usize;
        while self.u(1)? == 0 {
            zeros += 1;
            // A value this long is not a value; it is a mis-parse.
            if zeros > 31 {
                return None;
            }
        }
        if zeros == 0 {
            return Some(0);
        }
        ((1u32 << zeros) - 1).checked_add(self.u(zeros)?)
    }

    fn se(&mut self) -> Option<i32> {
        let k = self.ue()?;
        Some(if k % 2 == 1 {
            k.div_ceil(2) as i32
        } else {
            -((k / 2) as i32)
        })
    }
}

/// The HEVC sequence header in a packet or an extradata blob, if there is one.
pub fn sequence_header<'a>(codec: &str, data: &'a [u8], framing: NalFraming) -> Option<&'a [u8]> {
    if codec != "hevc" {
        return None;
    }
    nal_payloads(data, framing)
        .into_iter()
        .find(|nal| nal.first().is_some_and(|b| (b >> 1) & 0x3F == 33))
}

/// The transfer characteristic an HEVC sequence header writes for itself.
///
/// **Not the same thing as the one a decoder reports.** A broadcast carries
/// HLG the way it has to for a receiver that predates it: the sequence
/// header says `bt2020-10` -- wide-gamut SDR, which every decoder ever built
/// understands -- and an `alternative_transfer_characteristics` SEI beside it
/// says the pictures are really HLG. libavcodec resolves the two and hands
/// back the answer, 18, which is what the pictures are; this hands back 14,
/// which is what the recording *says*. A re-encoded picture has to say the
/// same thing as the copied ones beside it, or a player reading only the
/// sequence header changes its mind partway through the cut.
///
/// `None` where the header cannot be read, or says nothing about colour.
pub fn coded_transfer(sps: &[u8]) -> Option<u8> {
    let mut b = Bits::new(sps.get(2..)?); // past the two-byte NAL header
    b.skip(4)?; // sps_video_parameter_set_id
    let max_sub = b.u(3)? as usize;
    b.skip(1)?; // sps_temporal_id_nesting_flag
    profile_tier_level(&mut b, max_sub)?;
    b.ue()?; // sps_seq_parameter_set_id
    if b.ue()? == 3 {
        b.skip(1)?; // separate_colour_plane_flag
    }
    b.ue()?; // pic_width_in_luma_samples
    b.ue()?; // pic_height_in_luma_samples
    if b.u(1)? == 1 {
        for _ in 0..4 {
            b.ue()?; // conformance window
        }
    }
    b.ue()?; // bit_depth_luma_minus8
    b.ue()?; // bit_depth_chroma_minus8
    let log2_poc_lsb = b.ue()? as usize + 4;
    let per_sub_layer = b.u(1)? == 1;
    for _ in (if per_sub_layer { 0 } else { max_sub })..=max_sub {
        b.ue()?;
        b.ue()?;
        b.ue()?;
    }
    for _ in 0..6 {
        b.ue()?; // coding and transform block sizes, and the two depths
    }
    if b.u(1)? == 1 && b.u(1)? == 1 {
        scaling_list_data(&mut b)?;
    }
    b.skip(2)?; // amp_enabled_flag, sample_adaptive_offset_enabled_flag
    if b.u(1)? == 1 {
        b.skip(8)?; // pcm sample bit depths
        b.ue()?;
        b.ue()?;
        b.skip(1)?;
    }
    let sets = b.ue()? as usize;
    if sets > 64 {
        return None;
    }
    let mut deltas: Vec<u32> = Vec::with_capacity(sets);
    for i in 0..sets {
        let n = short_term_ref_pic_set(&mut b, i, sets, &deltas)?;
        deltas.push(n);
    }
    if b.u(1)? == 1 {
        let n = b.ue()?;
        if n > 64 {
            return None;
        }
        for _ in 0..n {
            b.skip(log2_poc_lsb)?;
            b.skip(1)?;
        }
    }
    b.skip(2)?; // temporal mvp, strong intra smoothing
    if b.u(1)? != 1 {
        return None; // vui_parameters_present_flag
    }
    if b.u(1)? == 1 && b.u(8)? == 255 {
        b.skip(32)?; // an aspect ratio written out as a pair of extents
    }
    if b.u(1)? == 1 {
        b.skip(1)?; // overscan_appropriate_flag
    }
    if b.u(1)? != 1 {
        return None; // video_signal_type_present_flag
    }
    b.skip(4)?; // video_format, video_full_range_flag
    if b.u(1)? != 1 {
        return None; // colour_description_present_flag
    }
    b.skip(8)?; // colour_primaries
    b.u(8).map(|v| v as u8)
}

/// The profile, tier and level, which are only ever stepped over here.
fn profile_tier_level(b: &mut Bits, max_sub: usize) -> Option<()> {
    b.skip(8)?; // profile_space, tier_flag, profile_idc
    b.skip(32)?; // profile compatibility flags
    b.skip(48)?; // source and constraint flags, reserved, inbld
    b.skip(8)?; // general_level_idc
    let mut present = Vec::with_capacity(max_sub);
    for _ in 0..max_sub {
        present.push((b.u(1)? == 1, b.u(1)? == 1));
    }
    if max_sub > 0 {
        b.skip(2 * (8 - max_sub))?;
    }
    for (profile, level) in present {
        if profile {
            b.skip(88)?;
        }
        if level {
            b.skip(8)?;
        }
    }
    Some(())
}

/// The quantiser matrices, where a recording writes its own.
fn scaling_list_data(b: &mut Bits) -> Option<()> {
    for size_id in 0..4usize {
        let step = if size_id == 3 { 3 } else { 1 };
        let mut matrix_id = 0;
        while matrix_id < 6 {
            if b.u(1)? == 0 {
                b.ue()?; // scaling_list_pred_matrix_id_delta
            } else {
                let coefficients = std::cmp::min(64, 1 << (4 + (size_id << 1)));
                if size_id > 1 {
                    b.se()?; // scaling_list_dc_coef_minus8
                }
                for _ in 0..coefficients {
                    b.se()?;
                }
            }
            matrix_id += step;
        }
    }
    Some(())
}

/// One of the reference picture sets, and how many pictures it names --
/// which is what the next one may be written as a difference against.
fn short_term_ref_pic_set(b: &mut Bits, idx: usize, sets: usize, deltas: &[u32]) -> Option<u32> {
    if idx != 0 && b.u(1)? == 1 {
        if idx == sets {
            b.ue()?; // delta_idx_minus1
        }
        b.skip(1)?; // delta_rps_sign
        b.ue()?; // abs_delta_rps_minus1
        let reference = *deltas.get(idx.checked_sub(1)?)?;
        let mut kept = 0;
        for _ in 0..=reference {
            // use_delta_flag is written only where the picture is not used
            // by the current one, so the second read is conditional.
            if b.u(1)? == 1 || b.u(1)? == 1 {
                kept += 1;
            }
        }
        return Some(kept);
    }
    let negative = b.ue()?;
    let positive = b.ue()?;
    if negative > 64 || positive > 64 {
        return None;
    }
    for _ in 0..negative + positive {
        b.ue()?;
        b.skip(1)?;
    }
    Some(negative + positive)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sps(hex: &str) -> Vec<u8> {
        (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    /// A 4K broadcast that carries HLG the backward-compatible way: the
    /// sequence header says 14, and an SEI beside it says the pictures are
    /// really 18. What is wanted here is the 14.
    #[test]
    fn reads_the_transfer_a_broadcast_writes_rather_than_the_one_it_means() {
        let header = sps(
            "420101022000000300b00000030000030099a001e020021c4db18869242942f016a121c136ca0000\
             07d20001d4c0c24820dc0002b4be00015a5f7cf1e3d0",
        );
        assert_eq!(coded_transfer(&header), Some(14));
    }

    /// One that says what it means: PQ, in the header, with nothing to
    /// reconcile.
    #[test]
    fn reads_the_transfer_of_a_recording_that_states_it_plainly() {
        let header = sps(
            "420101222000000300b00000030000030099a001e020021c4d90628d9242942f016a12201228000\
             01f480007530786b041b80007270c00039387f9e3c7a0",
        );
        assert_eq!(coded_transfer(&header), Some(16));
        // And what an encoder of ours writes for the same pictures.
        let written = sps(
            "420101022000000300900000030000030099a001e020021c4d96565924caf016a1224120800001f\
             48000753004",
        );
        assert_eq!(coded_transfer(&written), Some(18));
    }

    /// A Dolby Vision recording states no colour at all -- the RPU carries
    /// it -- so there is nothing to answer with.
    #[test]
    fn says_nothing_where_the_header_describes_no_colour() {
        let header = sps(
            "42010102a000000300b00000030000030096a001e020021c4d94526491b6bc040400000fa400017\
             70186b7bdf8000aba900017d782",
        );
        assert_eq!(coded_transfer(&header), None);
    }

    /// Whatever a damaged recording hands over, an answer comes back rather
    /// than a panic or a loop.
    #[test]
    fn refuses_a_header_it_cannot_read() {
        assert_eq!(coded_transfer(&[]), None);
        assert_eq!(coded_transfer(&[0x42, 0x01]), None);
        assert_eq!(coded_transfer(&[0xff; 64]), None);
        let mut truncated = sps(
            "420101022000000300b00000030000030099a001e020021c4db18869242942f016a121c136ca0000\
             07d20001d4c0c24820dc0002b4be00015a5f7cf1e3d0",
        );
        while truncated.pop().is_some() {
            // every prefix of a real one, none of which may hang
            let _ = coded_transfer(&truncated);
        }
    }

    /// The sequence header is picked out of a packet by its NAL type.
    #[test]
    fn finds_the_sequence_header_among_the_others() {
        let mut packet = vec![0, 0, 1, 0x40, 0x01, 0xaa]; // VPS
        packet.extend_from_slice(&[0, 0, 1, 0x42, 0x01, 0xbb]); // SPS
        let found = sequence_header("hevc", &packet, NalFraming::AnnexB).unwrap();
        assert_eq!(found, &[0x42, 0x01, 0xbb]);
        assert!(sequence_header("h264", &packet, NalFraming::AnnexB).is_none());
    }

    /// Wrap NAL payloads back up as a transport stream's packet would carry
    /// them.
    fn annexb(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in nals {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
        out
    }

    /// A recorder's own disc: 1440x1080 at 29.97, and every picture on it
    /// half a frame. The header says fields are coded at all, and the two
    /// slices are the two halves of one frame.
    #[test]
    fn reads_a_recorder_that_codes_its_pictures_as_field_pairs() {
        let header =
            sps("27640028ac2ca50168111f7ff800200018800001f480007530742000206c80001036651b5805");
        let shape = field_shape("h264", &annexb(&[&header]), NalFraming::AnnexB)
            .expect("the recorder codes fields");
        assert_eq!(shape.frame_num_bits, 8);
        assert!(!shape.separate_colour_planes);
        for slice in ["2188a7de14d29e346f5a", "219927fe3f219e2c5ffa"] {
            let packet = annexb(&[&[0x09, 0x10], &sps(slice)]);
            assert!(is_field_picture(
                &packet,
                "h264",
                NalFraming::AnnexB,
                Some(&shape)
            ));
        }
    }

    /// **The header alone does not settle it.** What this program writes back
    /// over the same recording says fields are coded -- it is interlaced, and
    /// `frame_mbs_only_flag` is clear -- while every picture in it is a whole
    /// frame. So the picture is what is asked, and the header only says how
    /// to ask it.
    #[test]
    fn a_header_that_allows_fields_does_not_make_every_picture_one() {
        let header = sps("67640028ace40168113f7870800001f4800075300f8b1689");
        let shape = field_shape("h264", &annexb(&[&header]), NalFraming::AnnexB).unwrap();
        assert_eq!(shape.frame_num_bits, 4);
        let packet = annexb(&[&sps("6588820804bff626")]);
        assert!(!is_field_picture(
            &packet,
            "h264",
            NalFraming::AnnexB,
            Some(&shape)
        ));
    }

    /// Progressive material says so outright, and then no picture need be
    /// read at all.
    #[test]
    fn a_progressive_recording_codes_no_fields() {
        let header = sps("67f4000d919b28283f6022000003000200000300781e28532c");
        assert!(field_shape("h264", &annexb(&[&header]), NalFraming::AnnexB).is_none());
    }

    /// MPEG-2 says it in the picture itself: `picture_structure` 3 is a whole
    /// frame, 1 and 2 are the top and the bottom field on their own. But a
    /// packet holding the two fields of one frame is a whole frame, which is
    /// how libavcodec's parser hands a pair over.
    #[test]
    fn an_mpeg2_field_pair_arrives_as_one_frame() {
        // A picture header, then a coding extension whose third byte carries
        // intra_dc_precision and picture_structure in its low four bits.
        let picture = |structure: u8| {
            let mut d = vec![0, 0, 1, 0x00, 0x00, 0x08, 0xff, 0xff];
            d.extend_from_slice(&[0, 0, 1, 0xB5, 0x8f, 0xff, 0xf0 | structure, 0x00]);
            d
        };
        let asked = |d: &[u8]| is_field_picture(d, "mpeg2video", NalFraming::AnnexB, None);
        assert!(!asked(&picture(3)));
        assert!(asked(&picture(1)));
        assert!(asked(&picture(2)));
        // The pair joined, which is what the demuxer actually produces.
        let mut pair = picture(1);
        pair.extend_from_slice(&picture(2));
        assert!(!asked(&pair));
        // And a packet with no coding extension in it at all is a frame.
        assert!(!asked(&[0, 0, 1, 0x00]));
    }

    /// A picture written in several slices opens once, and the slices after
    /// the first say nothing about which it is.
    #[test]
    fn a_picture_in_several_slices_is_still_one_picture() {
        let header =
            sps("27640028ac2ca50168111f7ff800200018800001f480007530742000206c80001036651b5805");
        let shape = field_shape("h264", &annexb(&[&header]), NalFraming::AnnexB).unwrap();
        let opening = sps("2188a7de14d29e346f5a");
        // The same slice header with a first_mb_in_slice of 90 in front of
        // it, which is how the second slice of a 1440-wide picture starts.
        let mut later = vec![0x21, 0x00, 0x5a];
        later.extend_from_slice(&opening[1..]);
        let one = annexb(&[&opening, &later]);
        assert!(is_field_picture(
            &one,
            "h264",
            NalFraming::AnnexB,
            Some(&shape)
        ));
        // Two pictures in one packet are a frame's worth between them.
        let two = annexb(&[&opening, &opening]);
        assert!(!is_field_picture(
            &two,
            "h264",
            NalFraming::AnnexB,
            Some(&shape)
        ));
    }

    /// Whatever a damaged recording hands over, an answer comes back rather
    /// than a panic or a loop -- and a picture nothing could be read from is
    /// a whole frame, which is what every recording before the recorder's
    /// own was.
    #[test]
    fn refuses_a_picture_it_cannot_read() {
        assert!(field_shape("h264", &[], NalFraming::AnnexB).is_none());
        assert!(field_shape("h264", &[0xff; 64], NalFraming::AnnexB).is_none());
        let mut header =
            sps("27640028ac2ca50168111f7ff800200018800001f480007530742000206c80001036651b5805");
        let shape = field_shape("h264", &annexb(&[&header]), NalFraming::AnnexB).unwrap();
        while header.pop().is_some() {
            // every prefix of a real one, none of which may hang
            let _ = field_shape("h264", &annexb(&[&header]), NalFraming::AnnexB);
        }
        let mut slice = sps("2188a7de14d29e346f5a");
        while slice.pop().is_some() {
            let _ = is_field_picture(&annexb(&[&slice]), "h264", NalFraming::AnnexB, Some(&shape));
        }
        // And with nothing read from the sequence, no picture is a field.
        assert!(!is_field_picture(
            &annexb(&[&sps("2188a7de14d29e346f5a")]),
            "h264",
            NalFraming::AnnexB,
            None
        ));
    }
}
