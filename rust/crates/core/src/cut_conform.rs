//! Writing pictures afresh: into another reel's shape, and through a
//! transition.
//!
//! A submodule of [`crate::cut`] rather than a module of its own, because
//! everything it works on is that file's: the writer, the segment context,
//! the per-stream take-ups. What is here is only the one path that
//! [`reencode_segment`] cannot take.
//!
//! **It is not the same job as a seam.** A seam re-encodes a partial GOP to
//! stand among the recording's own copied pictures: same size, same rate,
//! same codec, and the only question is how to make the few frames it writes
//! indistinguishable from the ones on either side. This writes a whole range
//! of a recording that has none of those in common with the file it is going
//! into, and the questions are the other ones -- how to get a picture from
//! one size to another, and what to do when the two recordings do not agree
//! on how often a picture arrives.
//!
//! ## The pictures are placed on the master's grid, not their own
//!
//! [`reencode_segment`] places each decoded picture at its own instant, and
//! that is right where the pictures around it came at the same rate. Here
//! they did not. A 25 fps clip placed by its own instants into a 29.97 file
//! lands between the frames of every second one, and what comes out is a
//! stream whose pictures are neither one rate nor the other -- playable, but
//! not the single shape the file was supposed to have.
//!
//! So the output's own frame instants are counted off from the range's
//! start, and each is given **whichever source picture was on screen at that
//! moment**. Slower in than out and a picture is written twice; faster and
//! one is passed over. That is what the rate conversion is, and it is the
//! one a smart renderer can honestly offer: nothing here invents a picture
//! that was never taken.

use super::*;

/// The shape a reel is being written into: the master's.
pub(super) struct Shaped<'a> {
    pub video: &'a crate::VideoInfo,
    /// The master's own video stream parameters, which is what the encoder
    /// is opened against and what the output stream was declared as.
    pub params: &'a ff::codec::Parameters,
    /// What the master's pictures cost per second, so the ones written here
    /// are worth about what the file's others are.
    pub bit_rate: usize,
}

/// A picture on its way from one shape to the other.
///
/// The scaler is rebuilt whenever the decoder starts handing over a
/// different shape of picture, which is not the paranoia it looks like: a
/// broadcast recording can change frame size mid-file where the station
/// switched feeds, and a scaler built for the first shape silently refuses
/// the second. See [`crate::proxy`], where the same thing is done for the
/// same reason.
struct Rescale {
    /// Size and format of what it was built for, and whether that was full
    /// range: a range to convert is as much a reason to rebuild as a size.
    was: (u32, u32, ff::format::Pixel, bool),
    ctx: crate::blend::Scaler,
}

/// Write one range of a reel that does not match the master.
///
/// The whole range: a clip being written afresh has no copied pictures for a
/// partial GOP to be spliced onto, so there is nothing for the planner to
/// divide and [`crate::plan::reencode_range`] hands over one segment.
pub(super) fn conform_segment(
    src: &Source,
    seg: &Segment,
    ctx: &SegmentCtx,
    opts: &CutOptions,
    into: &Shaped,
    writer: &mut Writer,
) -> Result<Span> {
    pictures_afresh(src, None, seg, None, None, ctx, opts, into, writer)
}

/// Write one range's worth of a transition.
///
/// `next` is the reel that follows, which an overlapping crossing reads
/// alongside this one and a fade does not read at all.
#[allow(clippy::too_many_arguments)]
pub(super) fn crossing_segment(
    src: &Source,
    next: Option<&Source>,
    seg: &Segment,
    retouch: &crate::plan::Retouch,
    overlay: Option<&str>,
    ctx: &SegmentCtx,
    opts: &CutOptions,
    into: &Shaped,
    writer: &mut Writer,
) -> Result<Span> {
    let laid = overlay.map(|path| read_overlay(path, into)).transpose()?;
    pictures_afresh(
        src,
        next,
        seg,
        Some(*retouch),
        laid.as_ref(),
        ctx,
        opts,
        into,
        writer,
    )
}

#[allow(clippy::too_many_arguments)]
fn pictures_afresh(
    src: &Source,
    next: Option<&Source>,
    seg: &Segment,
    retouch: Option<crate::plan::Retouch>,
    laid: Option<&crate::blend::Laid>,
    ctx: &SegmentCtx,
    opts: &CutOptions,
    into: &Shaped,
    writer: &mut Writer,
) -> Result<Span> {
    let first_segment = ctx.first;
    let (mut ictx, ist_index) = open_input(&src.input.url)?;
    let stream = ictx
        .stream(ist_index)
        .ok_or_else(|| anyhow!("no video stream in {}", src.path))?;
    let in_tb = f64::from(stream.time_base());
    let params = stream.parameters();

    let mut decoder = crate::video_decoder(params)?;
    let mut encoder = Pictures::open_into(
        into.video,
        into.bit_rate,
        into.params,
        opts,
        ctx.signalling,
    )?;
    // A master whose container reported the field order of a first picture
    // that happened to be film is encoded progressive, and libavcodec then
    // writes `progressive_sequence=1` among the master's own pictures saying
    // 0 -- at every transition of such a recording. Put back the way
    // [`reencode_segment`] puts it back; each picture here is a whole frame
    // shown for two fields, top first. See [`Mpeg2Display`].
    let shown = (into.video.codec == "mpeg2video" && !into.video.interlaced())
        .then(|| stated_sequence(into.params))
        .flatten()
        .filter(|&interlaced| interlaced)
        .map(|_| Mpeg2Display {
            interlaced_sequence: true,
            top_first: Default::default(),
        });

    // The output's own frame, in the units the writer counts in. Two for an
    // ordinary recording, more where the master's timeline is divided
    // finely; see `Grid`.
    let field = ctx.grid.unit();
    let step = into.video.frame_duration();
    let per_frame = ((step / field).round() as i64).max(1);
    // The clip's own field order, where its lines reach the output as they
    // are: an interlaced clip of the master's height is scaled across only,
    // so its two fields are still its two fields -- and stated in the
    // master's order, a bottom-first clip joined onto a top-first master
    // showed every pair of fields in reverse, the motion stepping back
    // between each two. Each picture states its own order, as a copied one
    // does (see [`crate::conform::What::costs_pictures`]). A clip scaled in
    // height has had its fields mixed by the scaler and is left in the
    // master's order, and so is a crossing, which has two clips in it.
    let own_order = (into.video.interlaced()
        && src.video.interlaced()
        && src.video.height == into.video.height
        && src.video.top_field_first() != into.video.top_field_first()
        && !matches!(retouch, Some(crate::plan::Retouch::Cross { .. })))
    .then(|| src.video.top_field_first());

    seek_into(&mut ictx, src, seg.seek_from, None)?;
    // After the seek, for the reason [`crate::input::keep_only`] gives.
    let keep = segment_streams(&ictx, ctx, ist_index);
    crate::input::keep_only(&mut ictx, &keep);

    let mut audio_done = vec![false; ctx.audio.len()];
    let mut caption_done = vec![false; ctx.captions.len()];
    let mut graphics_done = vec![false; ctx.graphics.len()];
    let mut sub_done = !ctx.subpictures
        || src.subpictures.is_empty()
        || (writer.subpictures.is_none() && writer.converted.is_empty());

    // How many output frames the whole segment comes to, which is what a
    // transition's own progress is counted against. At least one: a
    // transition shorter than a frame still has the frame it lands on.
    let of_them = (((seg.end - seg.start) / step).round() as i64).max(1);
    // The clip on the far side of an overlapping crossing, opened here and
    // stepped alongside the near one. A fade has none: there is nothing on
    // the other side of it but the colour.
    let mut far = match retouch {
        Some(crate::plan::Retouch::Cross { theirs, .. }) => {
            let Some(other) = next else {
                bail!(
                    "{}: a crossing was asked for where there is no clip to cross to",
                    src.path
                );
            };
            Some(FarSide::open(other, theirs)?)
        }
        _ => None,
    };
    let mut rescale: Option<Rescale> = None;
    // The picture on screen, already in the master's shape, and the instant
    // the recording put it there. Held rather than written at once: what
    // decides how many times it is written is when the *next* one arrives.
    let mut held: Option<ff::frame::Video> = None;
    // Where this range's first picture landed, which every output instant is
    // counted off from.
    let mut anchor: Option<f64> = None;
    // How many output frames have been written, and what the encoder was
    // told about each.
    let mut made = 0i64;
    let mut placed: std::collections::HashMap<i64, (i64, i64)> = Default::default();
    let mut fed = 0i64;
    let mut span = Span::default();
    let mut damaged = 0usize;
    let mut past_end = false;
    // The instant the range stops asking for pictures. A range may be asked
    // for past where the recording stops, and a last picture written out to
    // an hour is an hour of nothing.
    let until = if src.duration > 0.0 {
        seg.end.min(src.duration)
    } else {
        seg.end
    };

    // Write the held picture at every output instant up to `before`.
    macro_rules! lay {
        ($before:expr) => {{
            let before = $before;
            if let (Some(a), Some(frame)) = (anchor, held.as_mut()) {
                while a + made as f64 * step < before - step * 1e-3 {
                    let display = ctx.display_base + made * per_frame;
                    placed.insert(fed, (display, per_frame));
                    span.fields = span.fields.max(display - ctx.display_base + per_frame);
                    span.pictures += 1;
                    // How far through the transition this frame is, if
                    // this stretch is one.
                    let through = made as f64 / of_them as f64;
                    let elapsed = made as f64 * step;
                    made += 1;
                    let mut own;
                    let picture: &mut ff::frame::Video = match retouch {
                        None => frame,
                        Some(what) => {
                            // Where this segment sits in the whole crossing.
                            // A fade is written as two of them -- the clip
                            // before going down and the clip after coming
                            // up -- and an image over it has to be one image
                            // over the pair.
                            let across = match what {
                                crate::plan::Retouch::Tint { going_in: true, .. } => {
                                    through / 2.0
                                }
                                crate::plan::Retouch::Tint { .. } => 0.5 + through / 2.0,
                                crate::plan::Retouch::Cross { .. } => through,
                            };
                            own = ff::frame::Video::new(
                                frame.format(),
                                frame.width(),
                                frame.height(),
                            );
                            retouched(
                                frame,
                                &mut own,
                                what,
                                through,
                                elapsed,
                                far.as_mut(),
                                into,
                            )?;
                            if let Some(laid) = laid {
                                crate::blend::over(
                                    &mut own,
                                    laid,
                                    crate::transition::showing(across),
                                )?;
                            }
                            &mut own
                        }
                    };
                    picture.set_pts(Some(fed));
                    fed += 1;
                    picture.set_kind(ff::picture::Type::None);
                    mark_interlacing(picture, into.video);
                    if let Some(top_first) = own_order {
                        unsafe {
                            let f = picture.as_mut_ptr();
                            if top_first {
                                (*f).flags |= ff::ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST;
                            } else {
                                (*f).flags &= !ff::ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST;
                            }
                        }
                    }
                    match &mut encoder {
                        Pictures::Libav(enc) => {
                            enc.send_frame(picture)?;
                            drain_encoder_shown(enc, ctx.reframe, &placed, shown.as_ref(), writer)?;
                        }
                        Pictures::Vc1(enc) => {
                            // In fields, as [`encode_vc1`] counts them: every
                            // frame here is one frame of the master's, shown
                            // for two. `per_frame` is in the timeline's
                            // units, which on a variable-rate master are a
                            // thirty-second of a field, and handed over as
                            // fields it said each picture was repeated three
                            // times over.
                            let packet = encode_vc1(enc, picture, 2)?;
                            writer.push(Emitted {
                                packet,
                                display,
                                fields: per_frame,
                            })?;
                        }
                    }
                }
            }
        }};
    }

    macro_rules! feed {
        () => {
            let mut frame = ff::frame::Video::empty();
            while decoder.receive_frame(&mut frame).is_ok() {
                let Some(pts) = frame.pts() else { continue };
                let t = pts as f64 * in_tb - src.start_time;
                if t >= until {
                    past_end = true;
                    break;
                }
                // Everything the picture already on screen still covers is
                // written before it is replaced.
                lay!(t);
                // Nothing before the range's own start is on screen in it --
                // but the picture that *was* on screen when it opened is,
                // and it may have arrived long before. So the last one to
                // arrive early is kept, and the anchor is not set until the
                // range begins.
                let shaped = reshape(&frame, into, &mut rescale)?;
                if anchor.is_none() {
                    anchor = Some(t.max(seg.start));
                }
                held = Some(shaped);
            }
        };
    }

    let mut packets = ictx.read_packets();
    for (stream, packet) in packets.by_ref() {
        let index = stream.index();
        if index != ist_index {
            if let Some(k) = ctx.audio.iter().position(|a| a.in_index == index) {
                if !audio_done[k] {
                    audio_done[k] =
                        take_audio(&ctx.audio[k], src, seg, first_segment, packet, writer)?;
                }
            } else if let Some(k) = ctx.captions.iter().position(|c| c.in_index == index) {
                if !caption_done[k] {
                    caption_done[k] =
                        take_caption(&ctx.captions[k], src, seg, first_segment, packet, writer)?;
                }
            } else if let Some(k) = ctx.graphics.iter().position(|g| g.in_index == index) {
                if !graphics_done[k] {
                    graphics_done[k] = take_graphics(&ctx.graphics[k], src, seg, packet, writer)?;
                }
            } else if !sub_done && stream.parameters().id() == ff::codec::Id::DVD_SUBTITLE {
                let id = stream.id();
                let tb = f64::from(stream.time_base());
                sub_done = take_subpicture(seg, ctx.offset, id, tb, src, &packet, writer);
            }
            continue;
        }
        if past_end {
            if audio_done.iter().all(|&d| d)
                && caption_done.iter().all(|&d| d)
                && graphics_done.iter().all(|&d| d)
                && sub_done
            {
                break;
            }
            if packet
                .pts()
                .is_some_and(|p| p as f64 * in_tb - src.start_time > seg.end + TRAIL)
            {
                break;
            }
            continue;
        }
        // A packet the decoder will not take is not a reason to stop; see
        // [`reencode_segment`], where the same is said at more length.
        if decoder.send_packet(&packet).is_err() {
            damaged += 1;
            continue;
        }
        feed!();
    }
    packets.finished()?;
    if !past_end {
        decoder.send_eof()?;
        // The last pictures the decoder was holding. Nothing reads
        // `past_end` after this -- the loop it belongs to is over -- so what
        // the macro sets here goes nowhere, and saying so keeps the macro
        // one macro rather than two.
        feed!();
        let _ = past_end;
    }
    // And the last picture stands until the range's own end.
    lay!(until);
    if let Pictures::Libav(enc) = &mut encoder {
        enc.send_eof()?;
        drain_encoder_shown(enc, ctx.reframe, &placed, shown.as_ref(), writer)?;
    }

    if span.pictures == 0 {
        if damaged > 0 {
            bail!(
                "{}: {:.3}-{:.3} has no pictures that can be written into the file's own \
                 shape -- {damaged} packet(s) there are damaged",
                src.path,
                seg.start,
                seg.end
            );
        }
        bail!(
            "{}: no pictures decoded between {:.3} and {:.3}",
            src.path,
            seg.start,
            seg.end
        );
    }
    if damaged > 0 {
        crate::say!(
            "note: {damaged} packet(s) between {:.3}s and {:.3}s of {} are damaged and could \
             not be decoded, so the pictures they carried are missing from the {} written \
             there.",
            seg.start,
            seg.end,
            src.path,
            span.pictures,
        );
    }
    Ok(span)
}

/// The master's pixel format, where its parameters state one.
///
/// **Not every recording does.** A Blu-ray's VC-1 can be probed without a
/// picture being decoded, and its parameters then say `AV_PIX_FMT_NONE`:
/// handed to the scaler as the format to make, that is an assertion inside
/// libswscale and the whole program aborted -- every transition and every
/// reel written afresh into such a master. `None` then, and the caller
/// keeps the format the pictures decode to.
fn master_pixels(into: &Shaped) -> Option<ff::format::Pixel> {
    if into.video.shape.pix_fmt < 0 {
        return None;
    }
    let raw = unsafe {
        std::mem::transmute::<i32, ff::ffi::AVPixelFormat>(into.video.shape.pix_fmt)
    };
    Some(ff::format::Pixel::from(raw))
}

/// One decoded picture in the master's size and pixel format.
///
/// A picture that is already the right shape is handed back as it is: the
/// reel whose sound alone had to be written afresh decodes and re-encodes
/// its pictures for nothing otherwise, and a scaler that is not needed is a
/// pass over every pixel that buys nothing.
fn reshape(
    frame: &ff::frame::Video,
    into: &Shaped,
    rescale: &mut Option<Rescale>,
) -> Result<ff::frame::Video> {
    let want = master_pixels(into).unwrap_or(frame.format());
    // Whether the samples are full range, the picture's and the master's.
    // A clip on the other side from the master is converted on the way
    // through: `conform` calls that a colour difference, and the reel is
    // written afresh for it rather than copied with its black read wrong.
    let full_in = crate::blend::full_range_frame(frame);
    let full_out = into.video.shape.full_range();
    let have = (frame.width(), frame.height(), frame.format(), full_in);
    if have == (into.video.width, into.video.height, want, full_out) {
        return Ok(frame.clone());
    }
    if rescale.as_ref().is_none_or(|r| r.was != have) {
        *rescale = Some(Rescale {
            was: have,
            ctx: crate::blend::Scaler::new(
                (have.0, have.1, have.2, full_in),
                (into.video.width, into.video.height, want, full_out),
            )?,
        });
    }
    let mut out = ff::frame::Video::new(want, into.video.width, into.video.height);
    rescale
        .as_mut()
        .expect("just built")
        .ctx
        .run(frame, &mut out)?;
    // Carried across rather than left at nothing: an encoder writes what a
    // frame says about itself, and a frame the scaler made says nothing.
    out.set_pts(frame.pts());
    unsafe {
        let o = out.as_mut_ptr();
        let f = frame.as_ptr();
        (*o).sample_aspect_ratio = (*f).sample_aspect_ratio;
        // The range these samples are written out as, which is the master's:
        // the encoder is opened against its parameters. Left at nothing, a
        // fade over a scaled clip in a full-range master read the picture as
        // studio-range and went down to a grey, the fault
        // [`crate::blend::copy_into`] carries the range across for.
        (*o).color_range = (*into.params.as_ptr()).color_range;
    }
    Ok(out)
}

/// One output picture, with whatever the transition does to it.
///
/// Written into a frame of its own rather than over the one the decoder
/// handed back. The decoded picture stands for as many output instants as
/// the two rates come to, so retouching it in place would apply the ramp
/// again to the picture the next instant is about to read -- and the
/// encoder is still holding the frame from the instant before. See
/// [`crate::blend::copy_into`].
#[allow(clippy::too_many_arguments)]
fn retouched(
    from: &ff::frame::Video,
    out: &mut ff::frame::Video,
    what: crate::plan::Retouch,
    through: f64,
    elapsed: f64,
    far: Option<&mut FarSide>,
    into: &Shaped,
) -> Result<()> {
    use crate::transition::Crossing;
    match what {
        crate::plan::Retouch::Tint {
            shade,
            going_in,
            easing,
        } => {
            crate::blend::copy_into(from, out)?;
            let a = easing.at(through);
            crate::blend::tint(out, shade, if going_in { a } else { 1.0 - a })
        }
        crate::plan::Retouch::Cross { kind, easing, .. } => {
            let a = easing.at(through);
            let Some(far) = far else {
                return crate::blend::copy_into(from, out);
            };
            let Some(theirs) = far.at(into, elapsed)? else {
                // The clip on the far side has run out of pictures before
                // the crossing has run out of time. Nothing is invented for
                // it: what is on screen is the clip this side, which is
                // what would have been there with no transition at all.
                return crate::blend::copy_into(from, out);
            };
            match kind {
                Crossing::Dissolve => crate::blend::dissolve(from, theirs, out, a),
                Crossing::Wipe(side) => crate::blend::wipe(from, theirs, out, side, a, false),
                Crossing::Slide(side) => crate::blend::wipe(from, theirs, out, side, a, true),
                // A fade never reaches here -- it has no far side -- and
                // neither does `None`, which is not a transition.
                _ => crate::blend::copy_into(from, out),
            }
        }
    }
}

/// The clip on the far side of a crossing, read alongside the near one.
///
/// **Read forwards and one picture ahead, never seeked.** A crossing asks
/// for the far clip at the same instants the near one is being written at,
/// and those instants march forwards -- so what is needed is the picture
/// that was on screen at each of them, which is exactly what reading
/// forwards and holding the last one gives. The one read ahead is what says
/// when the held one stops being the answer.
struct FarSide {
    ictx: crate::input::Demux,
    ist: usize,
    decoder: ff::decoder::Video,
    in_tb: f64,
    start_time: f64,
    /// Where the clip's own pictures start, on its own clock.
    from: f64,
    /// The picture on screen, already in the master's shape.
    held: Option<ff::frame::Video>,
    /// The next one, and when it arrives.
    ahead: Option<(f64, ff::frame::Video)>,
    rescale: Option<Rescale>,
    /// Set when the clip has no more pictures at all. The held one then
    /// stands for the rest of the crossing.
    spent: bool,
    /// Set once the reader has run out of packets and the decoder has been
    /// told so.
    ///
    /// Not the same as [`Self::spent`], and the two were one field until a
    /// crossing that ran to the end of the far clip froze several frames
    /// early: a decoder handed the end of the stream still has every
    /// reordered picture it was holding, and stopping at the first of them
    /// leaves the rest unread.
    drained: bool,
}

impl FarSide {
    fn open(src: &Source, from: f64) -> Result<FarSide> {
        let (mut ictx, ist) = open_input(&src.input.url)?;
        let stream = ictx
            .stream(ist)
            .ok_or_else(|| anyhow!("no video stream in {}", src.path))?;
        let in_tb = f64::from(stream.time_base());
        let params = stream.parameters();
        let decoder = crate::video_decoder(params)?;
        // Back a little, because the picture wanted at the crossing's first
        // instant is the one standing there, and it may have been coded
        // before the range opens.
        seek_to(&mut ictx, src, (from - src.seek_margin).max(0.0))?;
        crate::input::keep_only(&mut ictx, &[ist]);
        Ok(FarSide {
            ictx,
            ist,
            decoder,
            in_tb,
            start_time: src.start_time,
            from,
            held: None,
            ahead: None,
            rescale: None,
            spent: false,
            drained: false,
        })
    }

    /// The picture this clip has on screen `elapsed` seconds into the
    /// crossing.
    fn at(&mut self, into: &Shaped, elapsed: f64) -> Result<Option<&ff::frame::Video>> {
        let want = self.from + elapsed;
        loop {
            match self.ahead.take() {
                Some((t, frame)) if t <= want + 1e-9 => self.held = Some(frame),
                Some(pair) => {
                    self.ahead = Some(pair);
                    break;
                }
                None if self.spent => break,
                None => {
                    self.ahead = self.pull(into)?;
                    if self.ahead.is_none() {
                        self.spent = true;
                    }
                }
            }
        }
        Ok(self.held.as_ref())
    }

    /// The next picture the clip has, in the master's shape.
    fn pull(&mut self, into: &Shaped) -> Result<Option<(f64, ff::frame::Video)>> {
        let mut frame = ff::frame::Video::empty();
        loop {
            if self.decoder.receive_frame(&mut frame).is_ok() {
                let Some(pts) = frame.pts() else { continue };
                let t = pts as f64 * self.in_tb - self.start_time;
                return Ok(Some((t, reshape(&frame, into, &mut self.rescale)?)));
            }
            let (read, ended) = {
                let mut packets = self.ictx.read_packets();
                let read = packets.next().map(|(stream, packet)| (stream.index(), packet));
                (read, packets.finished())
            };
            let Some((index, packet)) = read else {
                // A read that failed is not the end of the clip: taken for
                // one, the crossing froze on the last picture it had and the
                // cut said it had succeeded. See [`crate::input::Packets`].
                ended?;
                // Nothing left to read. Whatever the decoder is still
                // holding comes out now, and after that there is nothing.
                if !self.drained {
                    self.drained = true;
                    self.decoder.send_eof()?;
                    continue;
                }
                return Ok(None);
            };
            if index != self.ist {
                continue;
            }
            // A packet the decoder will not take is not a reason to stop
            // the cut; see [`reencode_segment`].
            let _ = self.decoder.send_packet(&packet);
        }
    }
}

/// The still laid over a transition, brought to the shape these frames are
/// in. The reading of it is in [`crate::blend`], where the window that sets
/// the transition reads it too.
fn read_overlay(path: &str, into: &Shaped) -> Result<crate::blend::Laid> {
    // A master that never stated its format is decoded as 4:2:0, which is
    // what every one met so far turned out to be; a picture that is not
    // meets [`crate::blend::over`]'s refusal rather than a scaler's abort.
    let want = master_pixels(into).unwrap_or(ff::format::Pixel::YUV420P);
    crate::blend::read_laid(
        path,
        into.video.width,
        into.video.height,
        want,
        into.video.shape.full_range(),
    )
}
