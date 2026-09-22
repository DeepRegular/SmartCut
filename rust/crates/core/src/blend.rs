//! The pixel arithmetic a transition is made of.
//!
//! Small, and deliberately the only place any of it is written. A dissolve,
//! a wipe, a slide and a fade are four descriptions of the same operation --
//! for each sample of the output, choose between two numbers or mix them --
//! and what differs between them is one function of where the sample is and
//! how far through the crossing it is. Written four times they would be four
//! places to get the chroma subsampling wrong.
//!
//! ## What a plane is here
//!
//! Every format this touches is planar YUV: one plane of luma at the frame's
//! own size, two of chroma at that size divided by whatever the format
//! subsamples by, and samples that are one byte or two. That covers
//! everything SmartCut decodes -- `yuv420p` off a broadcast, `yuv422p10le`
//! off a professional recorder, `yuv420p10le` off a 4K one. A format that is
//! not planar YUV is refused rather than guessed at: what a transition would
//! do to a packed or RGB frame is write nonsense into it and carry on.
//!
//! Two-byte samples are little-endian in every such format libavutil has,
//! and the descriptor says so; what is not little-endian is refused with
//! everything else.

use anyhow::{bail, Result};
use ffmpeg_next as ff;

use crate::transition::{Shade, Side};

/// What a frame's planes look like, read once off the format's descriptor.
#[derive(Debug, Clone, Copy)]
pub struct Shape {
    /// Bits per sample: 8 for a broadcast, 10 for a 4K recorder.
    pub depth: u32,
    /// Bytes per sample -- one or two, which is what the loops step by.
    pub step: usize,
    /// How much the chroma planes are divided by, as shifts.
    pub log2_w: u32,
    pub log2_h: u32,
}

impl Shape {
    /// The width and height of one plane, in samples.
    pub fn plane(&self, width: u32, height: u32, nth: usize) -> (usize, usize) {
        if nth == 0 {
            (width as usize, height as usize)
        } else {
            (
                (width >> self.log2_w).max(1) as usize,
                (height >> self.log2_h).max(1) as usize,
            )
        }
    }
}

/// What a picture of this format is made of, or why it cannot be blended.
pub fn shape_of(format: ff::format::Pixel) -> Result<Shape> {
    let raw: ff::ffi::AVPixelFormat = format.into();
    let desc = unsafe { ff::ffi::av_pix_fmt_desc_get(raw) };
    if desc.is_null() {
        bail!("libavutil has no description of {format:?}");
    }
    let d = unsafe { &*desc };
    let planar = d.flags & ff::ffi::AV_PIX_FMT_FLAG_PLANAR as u64 != 0;
    let rgb = d.flags & ff::ffi::AV_PIX_FMT_FLAG_RGB as u64 != 0;
    let big_endian = d.flags & ff::ffi::AV_PIX_FMT_FLAG_BE as u64 != 0;
    let depth = u32::from(d.comp[0].depth.max(0) as u16);
    if !planar || rgb || big_endian || d.nb_components < 3 {
        bail!(
            "a transition needs pictures in a planar YUV format and these are {format:?}. \
             Every recording this program decodes is one; a format that is not can still be \
             joined, with no transition between the clips"
        );
    }
    // One byte per sample up to eight bits, two above it. Anything the
    // descriptor says is packed into a stride this does not expect is
    // refused above.
    let step = if depth <= 8 { 1 } else { 2 };
    Ok(Shape {
        depth,
        step,
        log2_w: u32::from(d.log2_chroma_w),
        log2_h: u32::from(d.log2_chroma_h),
    })
}

/// Whether two frames can stand in for each other, which is what every
/// two-picture blend below needs.
fn alike(a: &ff::frame::Video, b: &ff::frame::Video) -> Result<()> {
    if (a.width(), a.height(), a.format()) != (b.width(), b.height(), b.format()) {
        bail!(
            "a transition has {}x{} {:?} on one side and {}x{} {:?} on the other; both clips \
             are brought to the master's shape before this, so one of them was not",
            a.width(),
            a.height(),
            a.format(),
            b.width(),
            b.height(),
            b.format(),
        );
    }
    Ok(())
}

/// Read one sample.
#[inline]
fn get(row: &[u8], x: usize, step: usize) -> u32 {
    if step == 1 {
        u32::from(row[x])
    } else {
        u32::from(u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]))
    }
}

/// Write one sample.
#[inline]
fn put(row: &mut [u8], x: usize, step: usize, v: u32) {
    if step == 1 {
        row[x] = v as u8;
    } else {
        let b = (v as u16).to_le_bytes();
        row[x * 2] = b[0];
        row[x * 2 + 1] = b[1];
    }
}

/// Copy one picture's samples into another of the same shape.
///
/// **A fresh frame per output picture, and not a scratch one reused.** An
/// encoder handed a frame keeps a reference to its buffers until it has
/// finished with it, so writing the next picture into the same buffer
/// rewrites one already queued. Every blend below therefore writes into a
/// frame of its own, and this is what puts the picture into it.
pub fn copy_into(from: &ff::frame::Video, to: &mut ff::frame::Video) -> Result<()> {
    alike(from, to)?;
    let ok = unsafe { ff::ffi::av_frame_copy(to.as_mut_ptr(), from.as_ptr()) };
    if ok < 0 {
        bail!("a picture could not be copied: {}", ff::Error::from(ok));
    }
    Ok(())
}

/// Take a picture towards a colour. `mix` is 0 for the picture untouched and
/// 1 for the colour alone.
pub fn tint(frame: &mut ff::frame::Video, shade: Shade, mix: f64) -> Result<()> {
    let shape = shape_of(frame.format())?;
    let mix = mix.clamp(0.0, 1.0);
    if mix <= 0.0 {
        return Ok(());
    }
    let (luma, chroma) = shade.yuv(shape.depth);
    let (width, height) = (frame.width(), frame.height());
    let scaled = (mix * 4096.0).round() as u32;
    for nth in 0..3 {
        let (w, h) = shape.plane(width, height, nth);
        let target = u32::from(if nth == 0 { luma } else { chroma });
        let stride = frame.stride(nth);
        let step = shape.step;
        let data = frame.data_mut(nth);
        for y in 0..h {
            let row = &mut data[y * stride..y * stride + w * step];
            for x in 0..w {
                let was = get(row, x, step);
                // Integer throughout: a per-sample float multiply over a
                // 4K frame is the difference between a transition that
                // encodes as fast as it writes and one that does not.
                let now = (was * (4096 - scaled) + target * scaled) >> 12;
                put(row, x, step, now);
            }
        }
    }
    Ok(())
}

/// One picture fading through the other. `mix` is 0 for `a` alone and 1 for
/// `b` alone.
pub fn dissolve(
    a: &ff::frame::Video,
    b: &ff::frame::Video,
    out: &mut ff::frame::Video,
    mix: f64,
) -> Result<()> {
    alike(a, b)?;
    alike(a, out)?;
    let shape = shape_of(a.format())?;
    let scaled = (mix.clamp(0.0, 1.0) * 4096.0).round() as u32;
    let (width, height) = (a.width(), a.height());
    for nth in 0..3 {
        let (w, h) = shape.plane(width, height, nth);
        let (sa, sb, so) = (a.stride(nth), b.stride(nth), out.stride(nth));
        let step = shape.step;
        let (da, db) = (a.data(nth), b.data(nth));
        let dout = out.data_mut(nth);
        for y in 0..h {
            let ra = &da[y * sa..y * sa + w * step];
            let rb = &db[y * sb..y * sb + w * step];
            let ro = &mut dout[y * so..y * so + w * step];
            for x in 0..w {
                let va = get(ra, x, step);
                let vb = get(rb, x, step);
                put(ro, x, step, (va * (4096 - scaled) + vb * scaled) >> 12);
            }
        }
    }
    Ok(())
}

/// An edge travelling across the frame, `b` behind it and `a` in front.
///
/// `at` is how far the edge has travelled, 0 to 1. `side` is the side `b`
/// comes in from, so a wipe from the left has `b` filling the frame from the
/// left edge rightwards.
///
/// `moving` is what makes this a slide rather than a wipe: with it, both
/// pictures travel with the edge -- `b` sliding in and `a` being pushed out
/// -- and without it both stand still and only the edge moves.
pub fn wipe(
    a: &ff::frame::Video,
    b: &ff::frame::Video,
    out: &mut ff::frame::Video,
    side: Side,
    at: f64,
    moving: bool,
) -> Result<()> {
    alike(a, b)?;
    alike(a, out)?;
    let shape = shape_of(a.format())?;
    let at = at.clamp(0.0, 1.0);
    let (width, height) = (a.width(), a.height());
    for nth in 0..3 {
        let (w, h) = shape.plane(width, height, nth);
        let (sa, sb, so) = (a.stride(nth), b.stride(nth), out.stride(nth));
        let step = shape.step;
        // Where the edge stands in this plane's own samples. Rounded rather
        // than truncated, so the luma edge and the chroma edge are at the
        // same place on screen at every instant -- a half-sample apart is a
        // coloured fringe travelling across the picture.
        let across = match side {
            Side::Left | Side::Right => (at * w as f64).round() as isize,
            Side::Top | Side::Bottom => (at * h as f64).round() as isize,
        };
        let (da, db) = (a.data(nth), b.data(nth));
        let dout = out.data_mut(nth);
        for y in 0..h {
            let ro = &mut dout[y * so..y * so + w * step];
            for x in 0..w {
                // Which picture this sample comes from, and -- where the
                // pictures move -- which sample of it.
                let (from_b, sx, sy) = place(side, x, y, w, h, across, moving);
                let (src, stride) = if from_b { (db, sb) } else { (da, sa) };
                let v = if sx < w && sy < h {
                    get(&src[sy * stride..sy * stride + w * step], sx, step)
                } else {
                    // Off the edge of the picture it was asked of, which a
                    // slide reaches at its very last frame. The nearest
                    // sample stands in; the alternative is a line of
                    // whatever the buffer held.
                    let (cx, cy) = (sx.min(w - 1), sy.min(h - 1));
                    get(&src[cy * stride..cy * stride + w * step], cx, step)
                };
                put(ro, x, step, v);
            }
        }
    }
    Ok(())
}

/// Where one output sample is read from: which picture, and where in it.
#[inline]
fn place(
    side: Side,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    across: isize,
    moving: bool,
) -> (bool, usize, usize) {
    let (along, extent) = match side {
        Side::Left | Side::Right => (x as isize, w as isize),
        Side::Top | Side::Bottom => (y as isize, h as isize),
    };
    // How far into the frame the edge is, measured from the side `b` comes
    // in from.
    let from_edge = match side {
        Side::Left | Side::Top => along,
        Side::Right | Side::Bottom => extent - 1 - along,
    };
    let from_b = from_edge < across;
    if !moving {
        return (from_b, x, y);
    }
    // Both pictures travel with the edge. `b` is drawn as though its own
    // far edge were at the travelling edge, and `a` as though it had been
    // pushed the same distance.
    let shifted = if from_b {
        from_edge + (extent - across)
    } else {
        from_edge - across
    };
    let back = match side {
        Side::Left | Side::Top => shifted,
        Side::Right | Side::Bottom => extent - 1 - shifted,
    };
    let back = back.clamp(0, extent - 1) as usize;
    match side {
        Side::Left | Side::Right => (from_b, back, y),
        Side::Top | Side::Bottom => (from_b, x, back),
    }
}

/// A still picture laid over the frame.
///
/// The image is in the frame's own format and size -- brought there once
/// when it was read, not per frame -- and carries one alpha value per
/// sample of luma. `opacity` scales the whole of it, which is what a title
/// fading up is.
pub struct Laid {
    /// The picture, in the shape the frames are in.
    pub picture: ff::frame::Video,
    /// One byte per *luma* sample: how much of the image is there. The
    /// chroma planes are read at the same place divided down, which is what
    /// every subsampled composite does.
    pub alpha: Vec<u8>,
}

/// Draw one over the frame.
pub fn over(frame: &mut ff::frame::Video, laid: &Laid, opacity: f64) -> Result<()> {
    alike(&laid.picture, frame)?;
    let shape = shape_of(frame.format())?;
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 0.0 {
        return Ok(());
    }
    let (width, height) = (frame.width(), frame.height());
    let scale = (opacity * 4096.0).round() as u32;
    for nth in 0..3 {
        let (w, h) = shape.plane(width, height, nth);
        let (si, so) = (laid.picture.stride(nth), frame.stride(nth));
        let step = shape.step;
        let (shift_w, shift_h) = if nth == 0 {
            (0, 0)
        } else {
            (shape.log2_w, shape.log2_h)
        };
        let over = laid.picture.data(nth).to_vec();
        let data = frame.data_mut(nth);
        for y in 0..h {
            let ri = &over[y * si..y * si + w * step];
            let ro = &mut data[y * so..y * so + w * step];
            for x in 0..w {
                // The alpha is kept at luma resolution, so a chroma sample
                // asks the luma sample it sits over.
                let a = u32::from(laid.alpha[((y << shift_h) * width as usize) + (x << shift_w)]);
                let mix = (a * scale) / 255;
                if mix == 0 {
                    continue;
                }
                let was = get(ro, x, step);
                let now = get(ri, x, step);
                put(ro, x, step, (was * (4096 - mix) + now * mix) >> 12);
            }
        }
    }
    Ok(())
}

/// Read a still image and bring it to the shape the frames are in.
///
/// Read through libavcodec, like everything else here: a PNG, a JPEG, a BMP
/// or anything else it has a decoder for. What is wanted from it is two
/// things at once -- the colours in the frames' own format, and how much of
/// the image is there at each pixel -- so it is scaled twice, once into the
/// output's YUV and once into RGBA for the alpha channel. Twice over a
/// still, once per run.
///
/// An image with no alpha of its own is solid, which is what libswscale
/// fills the channel with.
///
/// Here rather than beside the cutter that writes the transition, because
/// the window that *sets* one reads the same image to show it: an overlay
/// that looked one way in the preview and another in the output would be
/// worse than no preview at all. See [`crate::crossview`].
pub fn read_laid(path: &str, width: u32, height: u32, want: ff::format::Pixel) -> Result<Laid> {
    let mut ictx = ff::format::input(&path)
        .map_err(|e| anyhow::anyhow!("the image over the transition, {path}: {e}"))?;
    let stream = ictx
        .streams()
        .best(ff::media::Type::Video)
        .ok_or_else(|| anyhow::anyhow!("{path} has no picture in it to lay over the transition"))?;
    let index = stream.index();
    let mut decoder = ff::codec::context::Context::from_parameters(stream.parameters())?
        .decoder()
        .video()?;
    let mut frame = ff::frame::Video::empty();
    let mut got = None;
    for (s, packet) in ictx.packets() {
        if s.index() != index {
            continue;
        }
        decoder.send_packet(&packet)?;
        if decoder.receive_frame(&mut frame).is_ok() {
            got = Some(());
            break;
        }
    }
    if got.is_none() {
        decoder.send_eof()?;
        if decoder.receive_frame(&mut frame).is_err() {
            bail!("{path} could not be decoded into a picture");
        }
    }

    let (w, h) = (width, height);
    // Stretched to the frame rather than placed in a corner. Where to put a
    // smaller image is a question with no answer that suits everybody, and
    // an image with an alpha channel answers it itself: what is transparent
    // is where the programme shows through.
    let mut colour = ff::frame::Video::new(want, w, h);
    ff::software::scaling::Context::get(
        frame.format(),
        frame.width(),
        frame.height(),
        want,
        w,
        h,
        ff::software::scaling::Flags::BICUBIC,
    )?
    .run(&frame, &mut colour)?;

    let mut rgba = ff::frame::Video::new(ff::format::Pixel::RGBA, w, h);
    ff::software::scaling::Context::get(
        frame.format(),
        frame.width(),
        frame.height(),
        ff::format::Pixel::RGBA,
        w,
        h,
        ff::software::scaling::Flags::BICUBIC,
    )?
    .run(&frame, &mut rgba)?;
    let stride = rgba.stride(0);
    let data = rgba.data(0);
    let mut alpha = vec![0u8; (w as usize) * (h as usize)];
    for y in 0..h as usize {
        for x in 0..w as usize {
            alpha[y * w as usize + x] = data[y * stride + x * 4 + 3];
        }
    }
    Ok(Laid {
        picture: colour,
        alpha,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(fill: [u8; 3]) -> ff::frame::Video {
        let mut f = ff::frame::Video::new(ff::format::Pixel::YUV420P, 16, 8);
        for (nth, &value) in fill.iter().enumerate() {
            let stride = f.stride(nth);
            let h = if nth == 0 { 8 } else { 4 };
            let w = if nth == 0 { 16 } else { 8 };
            let data = f.data_mut(nth);
            for y in 0..h {
                for x in 0..w {
                    data[y * stride + x] = value;
                }
            }
        }
        f
    }

    fn sample(f: &ff::frame::Video, nth: usize, x: usize, y: usize) -> u8 {
        f.data(nth)[y * f.stride(nth) + x]
    }

    /// Black is studio black, not nought: the pictures around a fade were
    /// coded in the same range, and a frame darker than they can reach is a
    /// frame that flashes.
    #[test]
    fn a_full_fade_lands_on_studio_black() {
        let mut f = frame([200, 90, 150]);
        tint(&mut f, Shade::Black, 1.0).unwrap();
        assert_eq!(sample(&f, 0, 0, 0), 16);
        assert_eq!(sample(&f, 1, 0, 0), 128);
        assert_eq!(sample(&f, 2, 0, 0), 128);
    }

    /// And a fade that has not started leaves every sample where it was.
    #[test]
    fn no_fade_changes_nothing() {
        let mut f = frame([200, 90, 150]);
        tint(&mut f, Shade::White, 0.0).unwrap();
        assert_eq!(
            (sample(&f, 0, 0, 0), sample(&f, 1, 0, 0), sample(&f, 2, 0, 0)),
            (200, 90, 150)
        );
    }

    /// Halfway through a dissolve every sample is halfway between.
    #[test]
    fn a_dissolve_meets_in_the_middle() {
        let (a, b) = (frame([100, 100, 100]), frame([200, 200, 200]));
        let mut out = frame([0, 0, 0]);
        dissolve(&a, &b, &mut out, 0.5).unwrap();
        for nth in 0..3 {
            assert_eq!(sample(&out, nth, 0, 0), 150);
        }
        dissolve(&a, &b, &mut out, 0.0).unwrap();
        assert_eq!(sample(&out, 0, 0, 0), 100);
        dissolve(&a, &b, &mut out, 1.0).unwrap();
        assert_eq!(sample(&out, 0, 0, 0), 200);
    }

    /// A wipe from the left has the arriving picture on the left of the
    /// edge and the departing one on its right, and the edge is where the
    /// fraction says -- in the chroma plane as well as the luma one.
    #[test]
    fn a_wipe_puts_the_edge_where_it_says() {
        let (a, b) = (frame([100, 100, 100]), frame([200, 200, 200]));
        let mut out = frame([0, 0, 0]);
        wipe(&a, &b, &mut out, Side::Left, 0.5, false).unwrap();
        assert_eq!(sample(&out, 0, 0, 0), 200);
        assert_eq!(sample(&out, 0, 7, 0), 200);
        assert_eq!(sample(&out, 0, 8, 0), 100);
        assert_eq!(sample(&out, 0, 15, 0), 100);
        // Chroma is half as wide, so its edge is at 4.
        assert_eq!(sample(&out, 1, 3, 0), 200);
        assert_eq!(sample(&out, 1, 4, 0), 100);
    }

    /// Both ends of every wipe are the whole of one picture. A transition
    /// that showed a sliver of the other at its ends would be a step at the
    /// join, which is the thing it exists to remove.
    #[test]
    fn a_wipe_ends_on_one_picture_or_the_other() {
        let (a, b) = (frame([100, 100, 100]), frame([200, 200, 200]));
        let mut out = frame([0, 0, 0]);
        for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
            for moving in [false, true] {
                wipe(&a, &b, &mut out, side, 0.0, moving).unwrap();
                assert_eq!(sample(&out, 0, 0, 0), 100, "{side:?} {moving} at 0");
                assert_eq!(sample(&out, 0, 15, 7), 100, "{side:?} {moving} at 0");
                wipe(&a, &b, &mut out, side, 1.0, moving).unwrap();
                assert_eq!(sample(&out, 0, 0, 0), 200, "{side:?} {moving} at 1");
                assert_eq!(sample(&out, 0, 15, 7), 200, "{side:?} {moving} at 1");
            }
        }
    }

    /// A packed or RGB frame is refused rather than written nonsense into.
    #[test]
    fn a_format_that_is_not_planar_yuv_is_refused() {
        assert!(shape_of(ff::format::Pixel::RGB24).is_err());
        assert!(shape_of(ff::format::Pixel::YUYV422).is_err());
        let ten = shape_of(ff::format::Pixel::YUV420P10LE).unwrap();
        assert_eq!((ten.depth, ten.step, ten.log2_w, ten.log2_h), (10, 2, 1, 1));
    }
}
