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
        "h264" => 4,   // avcC: configurationVersion, profile, compat, level
        "hevc" => 21,  // hvcC: fixed header before lengthSizeMinusOne
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
                    if e > s && data[e - 1] == 0 { e - 1 } else { e }
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
pub fn is_reference(data: &[u8], codec: &str, framing: NalFraming) -> bool {
    match codec {
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
    data.len() >= 4 && data[0] == 0 && data[1] == 0 && (data[2] == 1 || (data[2] == 0 && data[3] == 1))
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

/// Put the given parameter sets in front of a length-prefixed payload.
pub fn prepend_parameter_sets(data: &[u8], sets: &[Vec<u8>], n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + sets.iter().map(|s| s.len() + n).sum::<usize>());
    for s in sets {
        push_length_prefixed(&mut out, s, n);
    }
    out.extend_from_slice(data);
    out
}


/// How many fields this picture occupies: two normally, three under pulldown.
pub fn display_fields(data: &[u8], codec: &str) -> i64 {
    if codec == "mpeg2video" && mpeg2_repeats_field(data) {
        3
    } else {
        2
    }
}

/// Does this MPEG-2 picture ask for an extra field to be shown?
///
/// `repeat_first_field` is how 24 fps film is carried in a 29.97 stream:
/// every other picture is displayed for three fields instead of two. The
/// pictures then do not arrive at a constant rate, which breaks any output
/// timeline built from a single frame duration.
pub fn mpeg2_repeats_field(data: &[u8]) -> bool {
    let mut i = 0;
    while i + 8 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 && data[i + 3] == 0xB5 {
            // picture coding extension: extension id 0x8 in the top nibble
            if data[i + 4] >> 4 == 0x8 {
                return data[i + 7] & 0x02 != 0;
            }
        }
        i += 1;
    }
    false
}
