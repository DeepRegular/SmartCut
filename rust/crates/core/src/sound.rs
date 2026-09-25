//! The sound of a cut, written on its own.
//!
//! What this produces is the audio file the cut would have had, with the
//! ranges, the joins and the fades already in it and no pictures anywhere.
//! Somebody keeping a radio programme, or a drama they listen to rather than
//! watch, wants the sound and not a video file to extract it from later.
//!
//! **A writer of its own rather than a switch in [`crate::cut`].** That
//! writer is built around the pictures, and rightly: a range is chosen at an
//! entry point, a segment is copied or re-encoded by what the pictures need,
//! the progress is the writing head moving through them, and the output's
//! clock is a grid of fields. None of that has anything to say here. Asked to
//! write no pictures it would still read them, plan them, and spend the whole
//! of a run's time on material it then throws away -- a two hour recording is
//! ten gigabytes written to keep three hundred megabytes.
//!
//! What it does share is every decision. The track is planned by
//! [`crate::cut::plan_audio`], which is the one place that settles what a
//! track becomes; the frames at a boundary are prepared by
//! [`crate::audio::boundary_patches`], which is the one place that re-encodes
//! them; a whole-track re-encode goes through [`crate::audio::Reencoder`],
//! which is the one place that does that. The sound of an audio-only run and
//! the sound inside an ordinary cut are the same sound because they are made
//! by the same code.
//!
//! What it does not do yet is carry more than one track. A bilingual
//! recording has two, and an audio file with two tracks in it is a thing only
//! some containers hold; the first kept track is written and the run says so.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;
use crate::input::ReadPackets;

use crate::audio::{boundary_patches, fades_for, Patch, Reencoder};
use crate::cut::{AudioMode, CutOptions, Pass, Report};
use crate::{AudioInfo, Source};

/// One recording of an audio-only run, and what is kept of it.
///
/// The same shape [`crate::cut::Reel`] has, less the pictures: the ranges are
/// the kept stretches in the recording's own seconds, in the order they are
/// written, and `after` is the join that follows this piece -- which this
/// module reads for its two fade lengths and for how much of the next
/// piece's head an overlapping crossing takes.
pub struct Piece<'a> {
    pub src: &'a Source,
    pub ranges: &'a [(f64, f64)],
    pub after: crate::transition::Transition,
}

/// Write the sound of a cut, or of a list of them joined, as one file.
///
/// `master` is the piece the file takes its shape from -- its track, its
/// codec, its rate -- as [`crate::cut::join`] takes it; out of range is the
/// last.
pub fn write(pieces: &[Piece], master: usize, output: &str, opts: &CutOptions) -> Result<()> {
    write_with_progress(pieces, master, output, opts, None)
}

/// As [`write`], reporting how far along it is.
///
/// The fraction is ranges finished out of ranges asked for, which is a
/// coarser bar than the picture writer's and is the honest one here: there is
/// no writing head to follow, only a read of the sound of each range, and a
/// range is seconds rather than minutes.
pub fn write_with_progress(
    pieces: &[Piece],
    master: usize,
    output: &str,
    opts: &CutOptions,
    progress: Option<Report>,
) -> Result<()> {
    // See [`crate::input::Input::refuse_as_output`].
    crate::init()?;
    crate::input::refuse_url_output(output)?;
    for piece in pieces {
        piece.src.input.refuse_as_output(output)?;
    }
    let ours = !std::path::Path::new(output).exists();
    let done = run(pieces, master, output, opts, progress);
    if done.is_err() && ours {
        // The same bargain the picture writer makes: a run that failed leaves
        // nothing behind that reads as an answer.
        let _ = std::fs::remove_file(output);
    }
    done
}

/// The track the master supplies, which is its first that the caller has not
/// dropped.
fn track_of<'a>(src: &'a Source, opts: &CutOptions) -> Result<&'a AudioInfo> {
    src.audios
        .iter()
        .find(|a| !opts.drop_streams.contains(&a.stream_index))
        .ok_or_else(|| anyhow!("{} has no sound to write", src.path))
}

/// The track a piece supplies: the one in the master's track's place among
/// its sound tracks, as the picture writer pairs them (see `cut::Threads`).
///
/// Not the piece's own first undropped one. The streams dropped are the
/// master's numbers, and in another recording the same numbers can be a
/// data stream and the wrong language: an audio-only cut of a join wrote
/// the second recording's Japanese where the cut with pictures had its
/// English.
fn track_for<'a>(pieces: &[Piece<'a>], master: usize, n: usize, opts: &CutOptions) -> Result<&'a AudioInfo> {
    let shape = pieces[master].src;
    let chosen = track_of(shape, opts)?;
    if n == master {
        return Ok(chosen);
    }
    let src = pieces[n].src;
    shape
        .audios
        .iter()
        .position(|a| a.stream_index == chosen.stream_index)
        .and_then(|p| src.audios.get(p))
        .ok_or_else(|| anyhow!("{} has no sound to write", src.path))
}

/// How long the sound takes to leave and to come back at each range's two
/// ends: one pair per range, in the order they are written.
///
/// The same rule as `cut::fade_lengths` and for the same reasons -- a seam
/// inside one recording takes the run's own answer, a join between two takes
/// that join's two. Written twice rather than shared because the two walk
/// different lists: that one has plans, this one has ranges.
fn fade_lengths(pieces: &[Piece], opts: &CutOptions) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for (n, piece) in pieces.iter().enumerate() {
        for k in 0..piece.ranges.len() {
            let head = if k == 0 && n > 0 {
                pieces[n - 1].after.fade_in
            } else {
                opts.audio_fade
            };
            let tail = if k + 1 == piece.ranges.len() && n + 1 < pieces.len() {
                piece.after.fade_out
            } else {
                opts.audio_fade
            };
            out.push((head, tail));
        }
    }
    out
}

/// The ranges as the sound of a joined cut plays them.
///
/// An overlapping crossing -- a dissolve, a wipe, a slide -- shows the first
/// seconds of the clip after it while the clip before is still sounding, so
/// the clip after starts that much later, sound and all, and the output is
/// that much shorter. The same arithmetic as `ranges_with_transitions` in
/// [`crate::cut`], which is what keeps this file as long as the video's own
/// sound: each end gives at most half its range, and the pair is held to the
/// smaller. A fade takes nothing from either side, and nothing follows the
/// last piece.
fn overlapped(pieces: &[Piece]) -> Vec<Vec<(f64, f64)>> {
    let mut out: Vec<Vec<(f64, f64)>> = pieces.iter().map(|p| p.ranges.to_vec()).collect();
    let room = |r: Option<&(f64, f64)>| r.map_or(0.0, |&(a, b)| ((b - a) / 2.0).max(0.0));
    for n in 0..pieces.len().saturating_sub(1) {
        let t = &pieces[n].after;
        if !t.kind.overlaps() {
            continue;
        }
        let take = t
            .takes()
            .1
            .min(room(pieces[n].ranges.last()))
            .min(room(pieces[n + 1].ranges.first()));
        if let Some(first) = out[n + 1].first_mut() {
            if take > 0.0 {
                first.0 += take;
            }
        }
    }
    out
}

/// A range on the recording's own sample clock.
fn window(range: (f64, f64), rate: u32) -> (i64, i64) {
    (
        (range.0 * rate as f64).round() as i64,
        (range.1 * rate as f64).round() as i64,
    )
}

fn run(
    pieces: &[Piece],
    master: usize,
    output: &str,
    opts: &CutOptions,
    progress: Option<Report>,
) -> Result<()> {
    crate::init()?;
    if pieces.is_empty() {
        return Err(anyhow!("nothing to write"));
    }
    // The piece the file takes its shape from; the tracks it declares are
    // the master's, as the picture writer's are.
    let master = master.min(pieces.len() - 1);
    let shape = &pieces[master];
    let ranges_in_all: usize = pieces.iter().map(|p| p.ranges.len()).sum();
    if ranges_in_all == 0 {
        return Err(anyhow!("nothing is kept, so there is no sound to write"));
    }
    let kept = overlapped(pieces);
    let info = track_of(shape.src, opts)?.clone();
    let tracks = shape
        .src
        .audios
        .iter()
        .filter(|a| !opts.drop_streams.contains(&a.stream_index))
        .count();
    let many = shape.src.audios.len() > 1;
    if tracks > 1 {
        crate::note_once(format!(
            "note: {} carries {tracks} sound tracks and an audio file is written with one. \
             The first is written; the rest are left out.",
            shape.src.path,
        ));
    }
    // What the track becomes. Settled by the picture writer's own planner, so
    // that an audio-only run and an ordinary one cannot disagree about it.
    let mut setup =
        crate::cut::plan_audio(&shape.src.input.url, &info, opts, false, shape.src.on_a_ts, many)?;
    // A recording joined on whose sound is written another way cannot be
    // copied into a stream declared as the master's. The picture writer
    // re-encodes just those reels to the master's shape; with one track and
    // no pictures, re-encoding the whole of it comes to the same file.
    //
    // Framed differently counts as well: a broadcast's ADTS frames and an
    // MP4's raw ones are the same AAC, and one stream cannot hold both.
    let framing = |src: &Source| {
        (info.codec == "aac").then(|| crate::aac::of_source(src).map(|f| f.as_str()))
    };
    let shape_framing = framing(shape.src);
    let unlike = pieces.iter().enumerate().filter(|&(n, _)| n != master).any(|(n, p)| {
        track_for(pieces, master, n, opts).is_ok_and(|t| {
            t.codec != info.codec
                || t.sample_rate != info.sample_rate
                || t.channels != info.channels
                || framing(p.src) != shape_framing
        })
    });
    if unlike && setup.mode != AudioMode::Reencode {
        setup.mode = AudioMode::Reencode;
        // Smart mode frames what it encodes so it can sit among the copied
        // frames; a whole re-encode has none to sit among, and into a file
        // that is not a transport stream it is written raw, as the picture
        // writer writes it. Left set, the ADTS muxer put a second header in
        // front of every frame.
        setup.frame_as = None;
    }
    let fades = fade_lengths(pieces, opts);

    let mut octx = ff::format::output(&output)?;
    // Which way round this container writes samples. `carriage` answers for
    // the containers a cut goes into, where big-endian PCM is what an MP4
    // has a box for; a `.wav` wants them the other way round and says
    // "Function not implemented" about anything else -- after the file has
    // been created, which is the worst moment to find out. Only PCM is
    // second-guessed here: every other codec is a codec, and a container
    // that cannot hold one is a question for `carriage`.
    let guess = octx
        .format()
        .codec(std::path::Path::new(output), ff::media::Type::Audio);
    if crate::audio::uncompressed(setup.target)
        && crate::audio::uncompressed(guess)
        && guess != setup.target
    {
        // The muxer names its sixteen-bit default; a track that is wider
        // keeps its width and changes only its byte order.
        use ff::codec::Id;
        let wanted = match (guess, setup.target) {
            (Id::PCM_S16LE, Id::PCM_S24BE | Id::PCM_S24LE) => Id::PCM_S24LE,
            (Id::PCM_S16LE, Id::PCM_S32BE | Id::PCM_S32LE) => Id::PCM_S32LE,
            // And the containers whose default is big-endian (.aiff, .au,
            // .caf): named sixteen bits, they took a 24-bit track down to 16.
            (Id::PCM_S16BE, Id::PCM_S24BE | Id::PCM_S24LE) => Id::PCM_S24BE,
            (Id::PCM_S16BE, Id::PCM_S32BE | Id::PCM_S32LE) => Id::PCM_S32BE,
            (Id::PCM_F32LE | Id::PCM_S16LE, Id::PCM_F32BE | Id::PCM_F32LE) => Id::PCM_F32LE,
            (Id::PCM_F64LE | Id::PCM_S16LE, Id::PCM_F64BE | Id::PCM_F64LE) => Id::PCM_F64LE,
            _ => guess,
        };
        // A different byte order is a different codec, and copied frames
        // are still in the old one: they have to go through the encoder,
        // which for PCM is a rearrangement and loses nothing. Declared the
        // new way with the old frames, the header write failed.
        if wanted != setup.target {
            setup.target = wanted;
            if setup.mode != AudioMode::Reencode {
                setup.mode = AudioMode::Reencode;
                setup.frame_as = None;
            }
        }
    }

    // A container that has no box for the codec says so only as "Invalid
    // argument", once the header is being written. Asked first, so the run
    // can say which codec and which file.
    let holds = unsafe {
        ff::ffi::avformat_query_codec(
            (*octx.as_ptr()).oformat,
            setup.target.into(),
            ff::ffi::FF_COMPLIANCE_NORMAL,
        )
    };
    if holds == 0 {
        return Err(anyhow!(
            "{output} cannot hold {:?} sound. Name the file for that codec (.mp2, .ac3, \
             .aac and so on), or ask for another with --audio-codec",
            setup.target
        ));
    }

    let ictx = crate::input::demux(&shape.src.input.url)?;
    let params = ictx
        .stream(info.stream_index)
        .ok_or_else(|| anyhow!("audio stream {} vanished", info.stream_index))?
        .parameters();
    drop(ictx);

    let mut recoder = if setup.mode == AudioMode::Reencode {
        Some(Reencoder::new(
            params.clone(),
            setup.target,
            setup.like,
            &setup.info,
            setup.channels,
            setup.sample_rate,
            setup.bit_rate,
            setup.frame_as,
        )?)
    } else {
        None
    };

    {
        let mut ost = octx.add_stream(ff::encoder::find(ff::codec::Id::None))?;
        match recoder.as_ref() {
            Some(re) => ost.set_parameters(re.parameters()),
            None => ost.set_parameters(params.clone()),
        }
        // What the frames are rather than what the head of the recording
        // was, as the picture writer declares it: a broadcast that opens on
        // the mono bulletin before the programme is stereo from then on.
        // See `crate::audio::settled_shape`.
        unsafe {
            let p = ost.parameters().as_mut_ptr();
            if (*p).ch_layout.nb_channels != i32::from(setup.channels) {
                ff::ffi::av_channel_layout_uninit(&mut (*p).ch_layout);
                ff::ffi::av_channel_layout_default(&mut (*p).ch_layout, i32::from(setup.channels));
            }
            if (*p).sample_rate != setup.sample_rate as i32 {
                (*p).sample_rate = setup.sample_rate as i32;
            }
        }
        if let Some(lang) = &setup.info.language {
            let mut meta = ff::Dictionary::new();
            meta.set("language", lang);
            ost.set_metadata(meta);
        }
        // The muxer works the tag out for the container it is writing;
        // carrying the recording's own over means writing a transport
        // stream's idea of the codec into an MP4. Same as `write_audio_es`.
        unsafe {
            (*ost.parameters().as_mut_ptr()).codec_tag = 0;
        }
    }
    let mut muxer_opts = ff::Dictionary::new();
    // An ADTS header says which AAC it is framing, and a broadcast's is
    // MPEG-2. libavformat writes MPEG-4 unless it is told.
    if octx.format().name().contains("adts") && opts.aac == crate::AacVersion::Mpeg2 {
        muxer_opts.set("write_mpeg2", "1");
    }
    // A RIFF header counts its sizes in 32 bits, and uncompressed sound runs
    // past four gigabytes inside a film: 5.1 at 24 bits and 48 kHz does in
    // 83 minutes. The muxer's default is to leave the sizes unwritten there
    // and say the file is broken. `auto` reserves room for the RF64 sizes
    // and uses it only where they are needed, so everything shorter is the
    // plain WAV it always was.
    if octx.format().name() == "wav" {
        muxer_opts.set("rf64", "auto");
    }
    octx.write_header_with(muxer_opts)?;
    let out_tb = f64::from(
        octx.stream(0)
            .ok_or_else(|| anyhow!("no output stream"))?
            .time_base(),
    );
    // The rate a re-encoded packet's own clock counts in, which is the
    // encoder's last word rather than what was asked for.
    let out_rate = recoder.as_ref().map_or(setup.sample_rate, Reencoder::rate) as f64;

    // Where the sound has reached, in seconds. Frames are laid end to end:
    // they are whole, so a range's worth of them is a whole number of frames
    // however the range was chosen, and end to end is the only arrangement
    // that neither overlaps nor drifts. The picture writer lays its sound the
    // same way. See `Writer::push_audio`.
    let mut written = 0.0f64;
    let mut ranges_done = 0usize;
    let say = |done: usize| {
        if let Some(report) = progress.as_ref() {
            let share = done as f64 / ranges_in_all as f64;
            report(Pass::Writing, share, share);
        }
    };
    say(0);

    for (n, piece) in pieces.iter().enumerate() {
        // A clip joined on with no sound has none to give, and failing the
        // whole file over it would lose every other clip's. Its ranges are
        // left out, which makes the file that much shorter than the cut
        // with pictures, and that is said.
        let Ok(track) = track_for(pieces, master, n, opts).cloned() else {
            crate::note_once(format!(
                "note: {} has no sound, so its part of the join is left out of the audio file",
                piece.src.path,
            ));
            ranges_done += piece.ranges.len();
            say(ranges_done);
            continue;
        };
        let base = pieces[..n].iter().map(|p| p.ranges.len()).sum::<usize>();
        let ranges = &kept[n];
        let windows: Vec<(i64, i64)> = ranges
            .iter()
            .map(|&r| window(r, track.sample_rate))
            .collect();
        // The frames the boundaries fall inside, re-encoded with the far side
        // silenced and the fade ridden over the near one. Smart rendering,
        // and the same call the picture writer makes.
        let patches: std::collections::HashMap<i64, Patch> = if setup.mode == AudioMode::Smart {
            boundary_patches(
                piece.src,
                &track,
                &windows,
                setup.bit_rate,
                setup.frame_as,
                &fades,
                base,
                ranges_in_all,
            )?
        } else {
            Default::default()
        };
        // A reel of another recording is another stream: the decoder inside
        // the re-encoder has to be told, or it reads the next recording's
        // frames against the one before it. It was opened on the master's,
        // which need not be the first.
        if let Some(re) = recoder.as_mut() {
            if n > 0 || n != master {
                let probe = crate::input::demux(&piece.src.input.url)?;
                let theirs = probe
                    .stream(track.stream_index)
                    .ok_or_else(|| anyhow!("audio stream {} vanished", track.stream_index))?
                    .parameters();
                drop(probe);
                re.retune(theirs, &track)?;
            }
        }

        let mut prev: Option<i64> = None;
        for (k, &range) in ranges.iter().enumerate() {
            let at = base + k;
            let win = windows[k];
            let ramp = fades_for(
                fades.get(at).copied().unwrap_or((0.0, 0.0)),
                track.sample_rate,
                at,
                ranges_in_all,
                win,
            );
            written = take_range(
                piece.src,
                &track,
                range,
                win,
                ramp,
                &patches,
                &mut prev,
                recoder.as_mut(),
                &mut octx,
                Clock { tb: out_tb, rate: out_rate, frame_secs: setup.frame_secs },
                written,
            )?;
            ranges_done += 1;
            say(ranges_done);
        }
    }

    if let Some(re) = recoder.as_mut() {
        let mut out = Vec::new();
        re.finish(&mut out)?;
        let clock = Clock { tb: out_tb, rate: out_rate, frame_secs: setup.frame_secs };
        for (packet, pts) in out {
            written = write_encoded(&mut octx, packet, pts, clock)?.max(written);
        }
    }
    octx.write_trailer()?;
    say(ranges_in_all);
    Ok(())
}

/// How the output keeps time.
#[derive(Clone, Copy)]
struct Clock {
    /// The container's time base, which is its own business: MP4 happens to
    /// count in samples and MPEG-TS insists on 90 kHz.
    tb: f64,
    /// The rate a re-encoded packet's own timestamps count in.
    rate: f64,
    /// What one of the recording's frames is worth, for a track whose
    /// packets do not say. See `assumed_frame` in [`crate::cut`].
    frame_secs: Option<f64>,
}

/// Read one range's sound and write it.
///
/// Returns where the output has reached, in seconds.
#[allow(clippy::too_many_arguments)]
fn take_range(
    src: &Source,
    track: &AudioInfo,
    range: (f64, f64),
    win: (i64, i64),
    ramp: crate::audio::Fades,
    patches: &std::collections::HashMap<i64, Patch>,
    prev: &mut Option<i64>,
    mut recoder: Option<&mut Reencoder>,
    octx: &mut ff::format::context::Output,
    clock: Clock,
    mut written: f64,
) -> Result<f64> {
    let mut ictx = crate::input::demux(&src.input.url)?;
    // Half a second before the range, so the frame it opens on has been
    // decoded up to rather than seeked into: a seek in a transport stream
    // lands where it can rather than where it was asked, and an AAC frame
    // decoded straight after one is missing half its window.
    let landing = ((range.0 - 0.5).max(0.0) + src.start_time) * f64::from(ff::ffi::AV_TIME_BASE);
    let target = landing as i64;
    ictx.seek(target, ..target)?;
    // After the seek, for the reason [`crate::input::keep_only`] gives, and
    // with the pictures, which say when the range is over where the track
    // has nothing in it. See [`crate::input::keep_with_pictures`].
    crate::input::keep_with_pictures(&mut ictx, &[track.stream_index]);

    let in_tb = track.time_base;
    let mut packets = ictx.read_packets();
    for (stream, packet) in packets.by_ref() {
        if stream.index() != track.stream_index {
            // Leeway for the pictures running a little ahead of their sound.
            if crate::input::packet_time(&stream, &packet, src.start_time)
                .is_some_and(|t| t >= range.1 + 5.0)
            {
                break;
            }
            continue;
        }
        let Some(pts) = packet.pts() else { continue };
        let t = pts as f64 * in_tb - src.start_time;
        let dur = match packet.duration() {
            own if own > 0 => own as f64 * in_tb,
            _ => clock.frame_secs.unwrap_or(0.0),
        };
        if t >= range.1 {
            break;
        }
        if let Some(re) = recoder.as_deref_mut() {
            // The re-encoder is handed every frame the range touches and
            // trims to the sample: the window says where the range really
            // begins and ends, which is inside the frames at both of them.
            // From the frame before the one the range opens in: the decoder
            // needs it to decode that one whole, and the window drops it.
            if t + 2.0 * dur > range.0 {
                re.take(&packet, track, src.start_time, win, ramp, None)?;
                let mut out = Vec::new();
                re.drain(&mut out)?;
                for (p, at) in out {
                    written = write_encoded(octx, p, at, clock)?.max(written);
                }
            }
            continue;
        }
        // Open on whichever frame sits nearest the range's start, so the
        // error is at most half a frame either way rather than a whole frame
        // late. The picture writer's own rule; see `take_audio` there, which
        // records why being cleverer is not available.
        if t + dur / 2.0 <= range.0 {
            continue;
        }
        if dur <= 0.0 {
            // A frame with no length is a frame there is nowhere to put:
            // what follows would land on top of it.
            continue;
        }
        // The frame a boundary falls inside, re-encoded beforehand with the
        // material outside the range silenced and the fade ridden over what
        // is kept. A guard is only used when it is what came before it.
        let patch = patches
            .get(&pts)
            .filter(|p| p.after.is_none() || p.after == *prev);
        let packet = match patch {
            Some(p) => {
                let mut patched = ff::Packet::copy(&p.bytes);
                patched.set_flags(ff::packet::Flags::KEY);
                patched.set_duration(packet.duration());
                patched
            }
            None => packet,
        };
        *prev = Some(pts);
        write_copied(octx, packet, written, dur, clock)?;
        written += dur;
    }
    packets.finished()?;
    Ok(written)
}

/// Write one of the recording's own frames where the sound has reached.
fn write_copied(
    octx: &mut ff::format::context::Output,
    mut packet: ff::Packet,
    at: f64,
    dur: f64,
    clock: Clock,
) -> Result<()> {
    let pts = (at.max(0.0) / clock.tb).round() as i64;
    packet.set_stream(0);
    packet.set_pts(Some(pts));
    packet.set_dts(Some(pts));
    packet.set_duration((dur / clock.tb).round() as i64);
    packet.set_position(-1);
    packet.write_interleaved(octx)?;
    Ok(())
}

/// ...and one the re-encoder made, which carries its own clock.
///
/// `at` counts the encoder's samples, because that is the clock it keeps.
/// The container keeps whatever time it likes: an MP4 counts in samples,
/// which hid this for a long time, and MPEG-TS insists on 90 kHz.
fn write_encoded(
    octx: &mut ff::format::context::Output,
    mut packet: ff::Packet,
    at: i64,
    clock: Clock,
) -> Result<f64> {
    let seconds = at as f64 / clock.rate.max(1.0);
    let pts = (seconds / clock.tb).round() as i64;
    packet.set_stream(0);
    packet.set_pts(Some(pts));
    packet.set_dts(Some(pts));
    packet.set_position(-1);
    packet.write_interleaved(octx)?;
    Ok(seconds)
}
