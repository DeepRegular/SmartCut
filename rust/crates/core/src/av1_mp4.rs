//! Check the AV1 fields shared by every sample in one MP4 description.
//! FFmpeg extracts the header; only its public container fields are read here.
use anyhow::{bail, Context, Result};
use ff::packet::{Mut, Ref};
use ffmpeg_next as ff;
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
    pub(crate) fn new(params: &ff::codec::Parameters) -> Result<Self> {
        let mut result = Self {
            filter: std::ptr::null_mut(),
            fields: None,
            initial: Vec::new(),
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

    pub(crate) fn packet(&mut self, packet: &mut ff::Packet) -> Result<()> {
        unsafe {
            check(av_bsf_send_packet(self.filter, packet.as_mut_ptr()))?;
            check(av_bsf_receive_packet(self.filter, packet.as_mut_ptr()))?;
            let mut size = 0;
            let mut extra = ff::ffi::av_packet_get_side_data(
                packet.as_ptr(),
                ff::ffi::AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
                &mut size,
            );
            if extra.is_null() {
                if self.fields.is_some() {
                    return Ok(());
                }
                if self.initial.is_empty() {
                    bail!("first AV1 picture has no sequence configuration");
                }
                size = self.initial.len();
                extra = ff::ffi::av_packet_new_side_data(
                    packet.as_mut_ptr(),
                    ff::ffi::AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
                    size,
                );
                if extra.is_null() {
                    bail!("cannot allocate initial AV1 configuration");
                }
                std::ptr::copy_nonoverlapping(self.initial.as_ptr(), extra, size);
            }
            let data = std::slice::from_raw_parts(extra, size);
            for sequence in sequences(data).context("invalid extracted AV1 sequence header")? {
                let fields = fields(sequence).context("truncated AV1 sequence header")?;
                if self.fields.is_some_and(|old| old != fields) {
                    return Err(Changed.into());
                }
                self.fields = Some(fields);
            }
        }
        Ok(())
    }
}

fn sequences(mut data: &[u8]) -> Option<Vec<&[u8]>> {
    let mut headers = Vec::new();
    while let Some((&head, rest)) = data.split_first() {
        if head & 0x81 != 0 {
            return None;
        }
        let mut at = usize::from(head & 4 != 0);
        let mut size = 0usize;
        if head & 2 != 0 {
            for n in 0..8 {
                let byte = *rest.get(at)?;
                at += 1;
                size = size.checked_add(usize::from(byte & 127).checked_shl(n * 7)?)?;
                if byte & 128 == 0 {
                    break;
                }
                if n == 7 {
                    return None;
                }
            }
        } else {
            size = rest.len().checked_sub(at)?;
        }
        let end = at.checked_add(size)?;
        let body = rest.get(at..end)?;
        if (head >> 3) & 15 == 1 {
            headers.push(body);
        }
        data = rest.get(end..)?;
    }
    (!headers.is_empty()).then_some(headers)
}

/// AV1 5.5.1/5.5.2, only as far as the av1C colour/subsampling fields.
/// The shared bit reader must not unescape AV1 as though it were a NAL.
fn fields(data: &[u8]) -> Option<[u8; 3]> {
    let mut b = crate::bitstream::Bits::raw(data);
    let profile = b.u(3)?;
    b.u(1)?;
    let reduced = b.u(1)? != 0;
    let (mut level, mut tier) = (0, 0);
    if reduced {
        level = b.u(5)?;
    } else {
        let mut buffer_bits = 0;
        let mut model = false;
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
    Some([
        0x81,
        ((profile << 5) | level) as u8,
        ((tier << 7) | (high << 6) | (twelve << 5) | (mono << 4) | (sx << 3) | (sy << 2) | position)
            as u8,
    ])
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
                ).cast();
                assert!(!(*p).extradata.is_null());
                std::ptr::copy_nonoverlapping(initial.as_ptr(), (*p).extradata, initial.len());
                (*p).extradata_size = initial.len() as i32;
            }
        }
        params
    }

    #[test]
    fn first_output_sequence_overrides_source_extradata() {
        let mut guard = Guard::new(&parameters(EIGHT)).unwrap();
        let mut packet = ff::Packet::copy(TEN);
        guard.packet(&mut packet).unwrap();
        assert_eq!(guard.fields, Some([0x81, 0, 0x4c]));
        assert_eq!(packet.data().unwrap(), TEN);
    }

    #[test]
    fn an_incompatible_later_sequence_requests_one_mp4_retry() {
        let mut guard = Guard::new(&parameters(&[])).unwrap();
        let mut first = ff::Packet::copy(EIGHT);
        guard.packet(&mut first).unwrap();
        let mut repeat = ff::Packet::copy(EIGHT);
        guard.packet(&mut repeat).unwrap();
        assert_eq!(repeat.data().unwrap(), EIGHT);
        let mut changed = ff::Packet::copy(TEN);
        assert!(guard.packet(&mut changed).unwrap_err().is::<Changed>());
    }

    #[test]
    fn an_incompatible_second_header_in_one_packet_is_not_missed() {
        let mut guard = Guard::new(&parameters(&[])).unwrap();
        let mut packet = ff::Packet::copy(&[EIGHT, TEN].concat());
        assert!(guard.packet(&mut packet).unwrap_err().is::<Changed>());
    }

}
