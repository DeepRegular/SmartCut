//! Check the AV1 fields shared by every sample in one MP4 description.
//! FFmpeg extracts the header; only its public container fields are read here.
use crate::SegmentKind;
use anyhow::{bail, Context, Result};
use ff::packet::{Mut, Ref};
use ffmpeg_next as ff;
use smartcut_mpeg2::bits::{Sink, Writer};
use std::ffi::{c_char, c_int, c_void};

// ffmpeg-sys-next 7.1 omits bsf.h. These are the public FFmpeg 7.1
// declarations, not private codec/muxer internals.
#[repr(C)]
struct Bsf {
    class: *const c_void,
    filter: *const c_void,
    private: *mut c_void,
    par_in: *mut ff::ffi::AVCodecParameters,
    par_out: *mut ff::ffi::AVCodecParameters,
    time_base_in: ff::ffi::AVRational,
    time_base_out: ff::ffi::AVRational,
}
extern "C" {
    fn av_bsf_get_by_name(name: *const c_char) -> *const c_void;
    fn av_bsf_alloc(filter: *const c_void, context: *mut *mut Bsf) -> c_int;
    fn av_bsf_init(context: *mut Bsf) -> c_int;
    fn av_bsf_free(context: *mut *mut Bsf);
    fn av_bsf_send_packet(context: *mut Bsf, packet: *mut ff::ffi::AVPacket) -> c_int;
    fn av_bsf_receive_packet(context: *mut Bsf, packet: *mut ff::ffi::AVPacket) -> c_int;
}

#[derive(Debug)]
pub(crate) struct Changed;
impl std::fmt::Display for Changed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AV1 configurations cannot share one MP4 sample description")
    }
}
impl std::error::Error for Changed {}

pub(crate) struct Guard {
    filter: *mut Bsf,
    fields: Option<[u8; 3]>,
    initial: Vec<u8>,
    source_fields: Option<[u8; 3]>,
    has_copy: bool,
}
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe { av_bsf_free(&mut self.filter) }
    }
}
fn check(code: i32) -> Result<()> {
    if code < 0 {
        return Err(ff::Error::from(code).into());
    }
    Ok(())
}
impl Guard {
    pub(crate) fn new(params: &ff::codec::Parameters, has_copy: bool) -> Result<Self> {
        let mut result = Self {
            filter: std::ptr::null_mut(),
            fields: None,
            initial: Vec::new(),
            source_fields: None,
            has_copy,
        };
        unsafe {
            let p = params.as_ptr();
            if !(*p).extradata.is_null() && (*p).extradata_size > 0 {
                let data = std::slice::from_raw_parts((*p).extradata, (*p).extradata_size as usize);
                result.initial = if data.first() == Some(&0x81) {
                    data.get(4..).unwrap_or_default().to_vec()
                } else {
                    data.to_vec()
                };
                // Prefer the actual source sequence. An av1C record may also
                // consist solely of its four fixed bytes, without configOBUs.
                result.source_fields = sequences(&result.initial)
                    .and_then(|s| fields(s[0]))
                    .or(match data {
                        [0x81, profile_level, colour, _, ..] => {
                            Some([0x81, *profile_level, *colour])
                        }
                        _ => None,
                    });
            }
            let filter = av_bsf_get_by_name(c"extract_extradata".as_ptr());
            if filter.is_null() {
                bail!("FFmpeg has no extract_extradata filter");
            }
            check(av_bsf_alloc(filter, &mut result.filter))?;
            check(ff::ffi::avcodec_parameters_copy(
                (*result.filter).par_in,
                params.as_ptr(),
            ))?;
            // Writer assigns final timestamps after this data-only filter.
            (*result.filter).time_base_in = ff::ffi::AVRational { num: 1, den: 1 };
            check(av_bsf_init(result.filter))?;
        }
        Ok(result)
    }

    pub(crate) fn packet(&mut self, packet: &mut ff::Packet, kind: SegmentKind) -> Result<()> {
        unsafe {
            check(av_bsf_send_packet(self.filter, packet.as_mut_ptr()))?;
            check(av_bsf_receive_packet(self.filter, packet.as_mut_ptr()))?;
        }
        if packet_config(packet).is_none() {
            if self.fields.is_some() {
                return Ok(());
            }
            if self.initial.is_empty() {
                bail!("first AV1 picture has no sequence configuration");
            }
            set_packet_config(packet, &self.initial)?;
        }
        let target = match kind {
            SegmentKind::Reencode if self.has_copy => self.fields.or(self.source_fields),
            _ => None,
        };
        let config = packet_config(packet).context("missing AV1 configuration")?;
        if let Some(target) = target {
            if let Some(aligned) = align_obus(config, target)? {
                // The muxer reads side data; a decoder reads OBUs in the
                // sample. Make the same edit to both, preserving packet props.
                if let Some(bytes) = align_obus(packet.data().unwrap_or_default(), target)? {
                    let size = c_int::try_from(bytes.len())?;
                    unsafe {
                        check(ff::ffi::av_packet_make_writable(packet.as_mut_ptr()))?;
                        let old_size = (*packet.as_ptr()).size;
                        if size > old_size {
                            check(ff::ffi::av_grow_packet(
                                packet.as_mut_ptr(),
                                size - old_size,
                            ))?;
                        } else {
                            ff::ffi::av_shrink_packet(packet.as_mut_ptr(), size);
                        }
                    }
                    packet
                        .data_mut()
                        .context("missing AV1 packet buffer")?
                        .copy_from_slice(&bytes);
                }
                set_packet_config(packet, &aligned)?;
            }
        }
        let config = packet_config(packet).context("missing AV1 configuration")?;
        for sequence in sequences(config).context("invalid extracted AV1 sequence header")? {
            let fields = fields(sequence).context("truncated AV1 sequence header")?;
            if self.fields.is_some_and(|old| old != fields) {
                return Err(Changed.into());
            }
            self.fields = Some(fields);
        }
        Ok(())
    }
}

const NEW_CONFIG: ff::ffi::AVPacketSideDataType =
    ff::ffi::AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA;

fn packet_config(packet: &ff::Packet) -> Option<&[u8]> {
    let mut size = 0;
    unsafe {
        let p = ff::ffi::av_packet_get_side_data(packet.as_ptr(), NEW_CONFIG, &mut size);
        (!p.is_null()).then(|| std::slice::from_raw_parts(p, size))
    }
}

fn set_packet_config(packet: &mut ff::Packet, data: &[u8]) -> Result<()> {
    unsafe {
        let p = ff::ffi::av_packet_new_side_data(packet.as_mut_ptr(), NEW_CONFIG, data.len());
        if p.is_null() {
            bail!("cannot allocate AV1 configuration");
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
    }
    Ok(())
}

struct Obu<'a> {
    prefix: &'a [u8], // header, optional extension and optional LEB128 size
    body: &'a [u8],
    rest: &'a [u8],
}

fn split_obu(data: &[u8]) -> Option<Obu<'_>> {
    let head = *data.first()?;
    if head & 0x81 != 0 {
        return None;
    }
    let mut at = 1 + usize::from(head & 4 != 0);
    let size = if head & 2 == 0 {
        data.len().checked_sub(at)?
    } else {
        let mut value = 0u64;
        for n in 0..8 {
            let byte = *data.get(at)?;
            at += 1;
            value |= u64::from(byte & 127) << (7 * n);
            if byte & 128 == 0 {
                break;
            }
            if n == 7 {
                return None;
            }
        }
        usize::try_from(u32::try_from(value).ok()?).ok()?
    };
    let end = at.checked_add(size)?;
    Some(Obu {
        prefix: data.get(..at)?,
        body: data.get(at..end)?,
        rest: data.get(end..)?,
    })
}

fn sequences(mut data: &[u8]) -> Option<Vec<&[u8]>> {
    let mut headers = Vec::new();
    while !data.is_empty() {
        let obu = split_obu(data)?;
        if (obu.prefix[0] >> 3) & 15 == 1 {
            headers.push(obu.body);
        }
        data = obu.rest;
    }
    (!headers.is_empty()).then_some(headers)
}

struct Sequence {
    fields: [u8; 3],
    level_bit: usize,
    reduced: bool,
    decoder_model: bool,
    syntax_end: usize,
}

fn fields(data: &[u8]) -> Option<[u8; 3]> {
    Some(parse_sequence(data)?.fields)
}

/// AV1 5.5.1/5.5.2. Remember the two bit positions needed to splice in
/// operating point zero's level and regenerate trailing_bits.
fn parse_sequence(data: &[u8]) -> Option<Sequence> {
    let mut b = crate::bitstream::Bits::raw(data);
    let profile = b.u(3)?;
    b.u(1)?;
    let reduced = b.u(1)? != 0;
    let (mut level, mut tier) = (0, 0);
    let mut level_bit = b.position();
    let mut model = false;
    if reduced {
        level = b.u(5)?;
    } else {
        let mut buffer_bits = 0;
        if b.u(1)? != 0 {
            b.skip(64)?;
            if b.u(1)? != 0 {
                let mut zeros = 0;
                while b.u(1)? == 0 {
                    zeros += 1;
                    if zeros == 32 {
                        break;
                    }
                }
                if zeros < 32 {
                    b.skip(zeros)?;
                }
            }
            model = b.u(1)? != 0;
            if model {
                buffer_bits = b.u(5)? as usize + 1;
                b.skip(42)?;
            }
        }
        let delay = b.u(1)? != 0;
        let count = b.u(5)? + 1;
        for i in 0..count {
            b.skip(12)?;
            if i == 0 {
                level_bit = b.position();
            }
            let l = b.u(5)?;
            let t = if l > 7 { b.u(1)? } else { 0 };
            if i == 0 {
                level = l;
                tier = t;
            }
            if model && b.u(1)? != 0 {
                b.skip(buffer_bits * 2 + 1)?;
            }
            if delay && b.u(1)? != 0 {
                b.skip(4)?;
            }
        }
    }
    let width = b.u(4)? as usize + 1;
    let height = b.u(4)? as usize + 1;
    b.skip(width + height)?;
    if !reduced && b.u(1)? != 0 {
        b.skip(7)?;
    }
    b.skip(3)?;
    if !reduced {
        b.skip(4)?;
        let order_hint = b.u(1)? != 0;
        if order_hint {
            b.skip(2)?;
        }
        let screen = if b.u(1)? != 0 { 2 } else { b.u(1)? };
        if screen != 0 && b.u(1)? == 0 {
            b.u(1)?;
        }
        if order_hint {
            b.skip(3)?;
        }
    }
    b.skip(3)?;
    let high = b.u(1)?;
    let twelve = if profile == 2 && high != 0 {
        b.u(1)?
    } else {
        0
    };
    let mono = if profile == 1 { 0 } else { b.u(1)? };
    let colour = if b.u(1)? != 0 {
        (b.u(8)?, b.u(8)?, b.u(8)?)
    } else {
        (2, 2, 2)
    };
    let (sx, sy, position) = if mono != 0 {
        b.u(1)?;
        (1, 1, 0)
    } else if colour == (1, 13, 0) {
        (0, 0, 0)
    } else {
        b.u(1)?;
        let (x, y) = match profile {
            0 => (1, 1),
            1 => (0, 0),
            _ if twelve != 0 => {
                let x = b.u(1)?;
                (x, if x != 0 { b.u(1)? } else { 0 })
            }
            _ => (1, 0),
        };
        let p = if x != 0 && y != 0 { b.u(2)? } else { 0 };
        (x, y, p)
    };
    b.skip(usize::from(mono == 0) + 1)?; // separate_uv_delta_q, film_grain_params_present
    Some(Sequence {
        level_bit,
        reduced,
        decoder_model: model,
        syntax_end: b.position(),
        fields: [
            0x81,
            ((profile << 5) | level) as u8,
            ((tier << 7)
                | (high << 6)
                | (twelve << 5)
                | (mono << 4)
                | (sx << 3)
                | (sy << 2)
                | position) as u8,
        ],
    })
}

// Annex A.3 defines these finite indices; 31 removes level constraints.
// Among the listed same-tier promotions, 3.1 -> 4.0 can tighten the
// minimum compression ratio (MainCR / MaxDisplayRate). Explicit decoder
// models also need separate buffer verification for a finite promotion.
fn may_raise(from: u8, to: u8, decoder_model: bool) -> bool {
    let defined = |n| matches!(n, 0 | 1 | 4 | 5 | 8 | 9 | 12..=19);
    defined(from)
        && from < to
        && (to == 31 || (defined(to) && !decoder_model && (from, to) != (5, 8)))
}

impl Sequence {
    fn with_level(&self, data: &[u8], target: [u8; 3]) -> Result<Vec<u8>> {
        let from = self.fields[1] & 31;
        let to = target[1] & 31;
        if self.fields[1] >> 5 != target[1] >> 5
            || self.fields[2] != target[2]
            || !may_raise(from, to, self.decoder_model)
        {
            return Err(Changed.into());
        }
        let end = self.syntax_end / 8;
        let marker = 0x80u8 >> (self.syntax_end % 8);
        if data
            .get(end)
            .is_none_or(|v| v & (marker | (marker - 1)) != marker)
            || data[end + 1..].iter().any(|&v| v != 0)
        {
            bail!("invalid AV1 sequence trailing bits");
        }
        let tail = self.level_bit + 5 + usize::from(!self.reduced && from > 7);
        let mut bits = Writer::new();
        bits.copy(data, 0, self.level_bit);
        bits.u(u32::from(to), 5);
        if !self.reduced && to > 7 {
            bits.u(u32::from(target[2] >> 7), 1);
        }
        bits.copy(data, tail, self.syntax_end - tail);
        bits.u(1, 1); // trailing_one_bit; finish fills to a byte with zeroes
        Ok(bits.finish())
    }
}

/// Return bytes only when a boundary sequence needs a safe promotion.
/// Keep every other OBU, including its original size spelling, byte-exact.
fn align_obus(data: &[u8], target: [u8; 3]) -> Result<Option<Vec<u8>>> {
    let mut rest = data;
    let mut preserved = data;
    let mut out = Vec::new();
    while !rest.is_empty() {
        let obu = split_obu(rest).context("invalid boundary AV1 OBU")?;
        let head = obu.prefix[0];
        if (head >> 3) & 15 == 1 {
            let seq = parse_sequence(obu.body).context("invalid boundary AV1 sequence")?;
            if seq.fields != target {
                let body = seq.with_level(obu.body, target)?;
                out.extend_from_slice(&preserved[..preserved.len() - rest.len()]);
                if head & 2 == 0 || body.len() == obu.body.len() {
                    out.extend_from_slice(obu.prefix);
                } else {
                    out.extend_from_slice(&obu.prefix[..1 + usize::from(head & 4 != 0)]);
                    let mut size = body.len();
                    while size >= 128 {
                        out.push((size as u8 & 127) | 128);
                        size >>= 7;
                    }
                    out.push(size as u8);
                }
                out.extend_from_slice(&body);
                preserved = obu.rest;
            }
        }
        rest = obu.rest;
    }
    if out.is_empty() {
        Ok(None)
    } else {
        out.extend_from_slice(preserved);
        Ok(Some(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn container_fields_follow_real_eight_and_ten_bit_headers() {
        // Headers from tiny libaom testsrc2 fixtures. 00 00 03 is literal
        // AV1 data; treating it as H.26x emulation prevention corrupts it.
        let ten = [0x0a, 0x0a, 0, 0, 0, 3, 0xb5, 0xfe, 0xf3, 0x6b, 0xe5, 1];
        let eight = [
            0x0a, 0x0d, 0, 0, 0, 3, 0xb5, 0xfe, 0xf3, 0x6b, 0xe4, 0x40, 0x80, 0x80, 0x61,
        ];
        assert_eq!(fields(sequences(&ten).unwrap()[0]), Some([0x81, 0, 0x4c]));
        assert_eq!(fields(sequences(&eight).unwrap()[0]), Some([0x81, 0, 0x0c]));
    }

    #[test]
    fn all_headers_in_one_packet_are_checked() {
        let one = [0x0a, 0x0a, 0, 0, 0, 3, 0xb5, 0xfe, 0xf3, 0x6b, 0xe5, 1];
        let two = [one.as_slice(), one.as_slice()].concat();
        assert_eq!(sequences(&two).unwrap().len(), 2);
        assert!(sequences(&[0x12, 0]).is_none()); // no Sequence Header
    }

    #[test]
    fn malformed_obu_lengths_are_rejected() {
        assert!(sequences(&[0x0a, 0x80]).is_none());
        assert!(sequences(&[0x0a, 255, 255, 255, 255, 255, 255, 255, 255]).is_none());
        assert!(fields(&[]).is_none());
        assert!(fields(&[0]).is_none());
        // AV1 bytes are not subject to H.264 emulation-prevention removal.
        let mut b = crate::bitstream::Bits::raw(&[0, 0, 3]);
        assert_eq!(b.u(24), Some(3));
    }

    const EIGHT: &[u8] = &[
        0x0a, 0x0d, 0, 0, 0, 3, 0xb5, 0xfe, 0xf3, 0x6b, 0xe4, 0x40, 0x80, 0x80, 0x61,
    ];
    const TEN: &[u8] = &[0x0a, 0x0a, 0, 0, 0, 3, 0xb5, 0xfe, 0xf3, 0x6b, 0xe5, 1];

    fn parameters(initial: &[u8]) -> ff::codec::Parameters {
        let mut params = ff::codec::Parameters::new();
        unsafe {
            let p = params.as_mut_ptr();
            (*p).codec_type = ff::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
            (*p).codec_id = ff::codec::Id::AV1.into();
            if !initial.is_empty() {
                (*p).extradata = ff::ffi::av_mallocz(
                    initial.len() + ff::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize,
                )
                .cast();
                assert!(!(*p).extradata.is_null());
                std::ptr::copy_nonoverlapping(initial.as_ptr(), (*p).extradata, initial.len());
                (*p).extradata_size = initial.len() as i32;
            }
        }
        params
    }

    #[test]
    fn first_output_sequence_overrides_source_extradata() {
        let mut guard = Guard::new(&parameters(EIGHT), false).unwrap();
        let mut packet = ff::Packet::copy(TEN);
        guard.packet(&mut packet, SegmentKind::Reencode).unwrap();
        assert_eq!(guard.fields, Some([0x81, 0, 0x4c]));
        assert_eq!(packet.data().unwrap(), TEN);
    }

    #[test]
    fn an_incompatible_later_sequence_requests_one_mp4_retry() {
        let mut guard = Guard::new(&parameters(&[]), false).unwrap();
        let mut first = ff::Packet::copy(EIGHT);
        guard.packet(&mut first, SegmentKind::Reencode).unwrap();
        let mut repeat = ff::Packet::copy(EIGHT);
        guard.packet(&mut repeat, SegmentKind::Reencode).unwrap();
        assert_eq!(repeat.data().unwrap(), EIGHT);
        let mut changed = ff::Packet::copy(TEN);
        assert!(guard
            .packet(&mut changed, SegmentKind::Reencode)
            .unwrap_err()
            .is::<Changed>());
    }

    #[test]
    fn an_incompatible_second_header_in_one_packet_is_not_missed() {
        let mut guard = Guard::new(&parameters(&[]), false).unwrap();
        let mut packet = ff::Packet::copy(&[EIGHT, TEN].concat());
        assert!(guard
            .packet(&mut packet, SegmentKind::Reencode)
            .unwrap_err()
            .is::<Changed>());
    }

    #[test]
    fn a_mixed_export_promotes_only_encoded_headers_and_preserves_packet_properties() {
        // The rav1e MP4 has the fixed av1C fields and no configOBUs.
        let target = [0x81, 31, 0x0c, 0];
        let mut guard = Guard::new(&parameters(&target), true).unwrap();
        let data = [[0x12, 0].as_slice(), EIGHT, EIGHT].concat();
        let mut encoded = ff::Packet::copy(&data);
        encoded.set_flags(ff::packet::Flags::KEY);
        encoded.set_pts(Some(23));
        encoded.set_dts(Some(21));
        encoded.set_duration(2);
        guard.packet(&mut encoded, SegmentKind::Reencode).unwrap();
        assert!(encoded.is_key());
        assert_eq!(
            (encoded.pts(), encoded.dts(), encoded.duration()),
            (Some(23), Some(21), 2)
        );
        let bytes = encoded.data().unwrap();
        assert_eq!(&bytes[..2], &[0x12, 0]);
        let headers = sequences(bytes).unwrap();
        assert_eq!(headers.len(), 2);
        assert_eq!(
            headers,
            sequences(packet_config(&encoded).unwrap()).unwrap()
        );
        for header in headers {
            assert_eq!(fields(header), Some([0x81, 31, 0x0c]));
            assert_eq!(header.len(), EIGHT.len() - 2 + 1); // one tier bit crosses a byte boundary
        }
        let mut copied = ff::Packet::copy(bytes);
        guard.packet(&mut copied, SegmentKind::Copy).unwrap();
        assert_eq!(copied.data().unwrap(), bytes);
        let mut differing_copy = ff::Packet::copy(EIGHT);
        assert!(guard
            .packet(&mut differing_copy, SegmentKind::Copy)
            .unwrap_err()
            .is::<Changed>());
        assert_eq!(differing_copy.data().unwrap(), EIGHT);
    }

    #[test]
    fn an_encoder_level_above_the_source_triggers_retry_and_the_retry_uses_its_own_level() {
        let level4 = align_obus(EIGHT, [0x81, 8, 0x0c]).unwrap().unwrap();
        let src = parameters(EIGHT);
        let mut mixed = Guard::new(&src, true).unwrap();
        let mut boundary = ff::Packet::copy(&level4);
        assert!(mixed
            .packet(&mut boundary, SegmentKind::Reencode)
            .unwrap_err()
            .is::<Changed>());
        let mut full = Guard::new(&src, false).unwrap();
        let mut boundary = ff::Packet::copy(&level4);
        full.packet(&mut boundary, SegmentKind::Reencode).unwrap();
        assert_eq!(full.fields, Some([0x81, 8, 0x0c]));
        assert_eq!(boundary.data().unwrap(), level4);
    }

    #[test]
    fn tier_bit_presence_depends_on_regular_or_reduced_sequence_syntax() {
        // Actual libaom 64x64 still picture. Tier is inferred even at level 31.
        let still = [0x0a, 6, 0x18, 0x15, 0x7f, 0xfd, 0xa0, 0x08];
        let promoted = align_obus(&still, [0x81, 31, 0x0c]).unwrap().unwrap();
        assert_eq!(promoted.len(), still.len());
        let was = parse_sequence(&still[2..]).unwrap();
        let now = parse_sequence(&promoted[2..]).unwrap();
        assert!(now.reduced);
        assert_eq!(was.syntax_end, now.syntax_end);
        assert_eq!(now.fields, [0x81, 31, 0x0c]);
        let level4 = align_obus(EIGHT, [0x81, 8, 0x0c]).unwrap().unwrap();
        let level5 = align_obus(&level4, [0x81, 12, 0x0c]).unwrap().unwrap();
        assert_eq!(level4.len(), level5.len()); // both levels have an explicit tier
        assert_eq!(
            fields(sequences(&level5).unwrap()[0]),
            Some([0x81, 12, 0x0c])
        );
    }

    #[test]
    fn extension_other_obus_and_the_implicit_final_size_are_preserved() {
        // The padded body crosses the 127-byte LEB128 boundary on re-padding.
        let mut input = vec![0x12, 0, 0x0e, 0, 0x80, 1];
        input.extend_from_slice(&EIGHT[2..]);
        input.resize(6 + 128, 0);
        input.extend_from_slice(&[0x7a, 0x81, 0, 0]); // preserve this original size spelling
        input.push(0x08); // the final Sequence Header has no size field
        input.extend_from_slice(&EIGHT[2..]);
        let out = align_obus(&input, [0x81, 31, 0x0c]).unwrap().unwrap();
        assert_eq!(&out[..5], &[0x12, 0, 0x0e, 0, 14]);
        let first = split_obu(&out[2..]).unwrap();
        assert_eq!(&first.rest[..5], &[0x7a, 0x81, 0, 0, 0x08]);
        assert_eq!(sequences(&out).unwrap(), vec![first.body, first.body]);
        let mut damaged = EIGHT.to_vec();
        *damaged.last_mut().unwrap() &= 0xfe;
        assert!(align_obus(&damaged, [0x81, 31, 0x0c]).is_err());
    }

    #[test]
    fn only_operating_point_zero_is_changed_when_timing_and_other_points_are_present() {
        let source = &EIGHT[2..];
        let base = parse_sequence(source).unwrap();
        let mut w = Writer::new();
        w.copy(source, 0, 5);
        w.u(1, 1); // timing_info_present
        w.u(1, 32);
        w.u(25, 32);
        w.u(0, 1); // equal_picture_interval
        w.u(0, 1); // decoder_model_info_present
        w.u(0, 1); // initial_display_delay_present
        w.u(1, 5); // two operating points
        w.u(0x103, 12);
        w.u(0, 5);
        w.u(0x101, 12);
        w.u(8, 5);
        w.u(1, 1); // second point's own high tier
        let tail = base.level_bit + 5;
        w.copy(source, tail, base.syntax_end - tail);
        w.u(1, 1);
        let body = w.finish();
        let framed = [[0x08].as_slice(), &body].concat();
        let out = align_obus(&framed, [0x81, 31, 0x0c]).unwrap().unwrap();
        let before = parse_sequence(&body).unwrap();
        let after = parse_sequence(&out[1..]).unwrap();
        assert_eq!(before.level_bit, after.level_bit);
        assert_eq!(after.syntax_end, before.syntax_end + 1);
        let bit = |data: &[u8], i: usize| (data[i / 8] >> (7 - i % 8)) & 1;
        for i in 0..before.syntax_end {
            if i < before.level_bit {
                assert_eq!(bit(&body, i), bit(&out[1..], i));
            } else if i >= before.level_bit + 5 {
                assert_eq!(bit(&body, i), bit(&out[1..], i + 1));
            }
        }
    }

    #[test]
    fn incompatible_fields_and_unproven_finite_levels_are_not_promoted() {
        for target in [
            [0x81, 8, 0x8c],
            [0x81, 8, 0x4c],
            [0x81, 8, 0x0e],
            [0x81, 0x28, 0x0c],
            [0x81, 2, 0x0c],
            [0x81, 25, 0x0c],
        ] {
            assert!(align_obus(EIGHT, target).unwrap_err().is::<Changed>());
        }
        assert!(!may_raise(31, 8, false));
        assert!(!may_raise(5, 8, false));
        assert!(!may_raise(0, 8, true));
        assert!(may_raise(0, 8, false));
        assert!(may_raise(0, 31, false));
    }
}
