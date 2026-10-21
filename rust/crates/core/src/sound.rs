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

/// The ranges less what no cut of the recording writes: the stretches at a
/// recorder's seams, from where the pictures before one stop to the first
/// entry point after it. See [`Source::unwritten`].
///
/// **The picture writer plans a range across a seam as two**
/// ([`crate::plan::plan_on`]), and its sound is cut against those two. Taken
/// whole here, an audio-only run kept what lies between: a recorder BD-RE
/// title kept across a seam whose clock runs on for 18.9 s with no picture
/// in it came out as a `.wav` 35.0 s long, 18.9 s of it silence laid in by
/// the re-encoder, where the cut with pictures is 16.1 s.
///
/// The piece after a hole is kept only where it is more than half a picture
/// long, as the planner keeps it only where an entry point lies inside it.
pub(crate) fn written(src: &Source, ranges: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let holes = src.unwritten();
    if holes.is_empty() {
        return ranges.to_vec();
    }
    less(ranges, &holes, src.video.frame_duration() / 2.0)
}

/// `ranges` with `holes` (in order) taken out of them. See [`written`].
fn less(ranges: &[(f64, f64)], holes: &[(f64, f64)], eps: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(ranges.len() + holes.len());
    for &(a, b) in ranges {
        let mut at = a;
        let mut after_hole = false;
        for &(from, to) in holes {
            if to <= at || from >= b {
                continue;
            }
            if from > at {
                out.push((at, from));
            }
            at = at.max(to);
            after_hole = true;
            if at >= b {
                break;
            }
        }
        if at < b && (!after_hole || b - at > eps) {
            out.push((at, b));
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
    // The ranges as the picture writer cuts them at a recorder's seams, which
    // every count below -- ranges, fades, windows -- is then taken from. See
    // [`written`].
    let split: Vec<Vec<(f64, f64)>> = pieces.iter().map(|p| written(p.src, p.ranges)).collect();
    let pieces: Vec<Piece> = pieces
        .iter()
        .zip(&split)
        .map(|(p, ranges)| Piece { src: p.src, ranges, after: p.after.clone() })
        .collect();
    let pieces = &pieces[..];
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
    //
    // The tracks being written, not the recordings' first ones: with that
    // one dropped, the framing compared was a track that is not in the file.
    let framing = |src: &Source, track: &AudioInfo| {
        (info.codec == "aac")
            .then(|| crate::aac::of_track(src, track.stream_index).map(|f| f.as_str()))
    };
    let shape_framing = framing(shape.src, &info);
    // Lossless sound carried through as it is takes a recording of its own
    // codec in another shape -- a TrueHD 2.0 one joined onto a TrueHD 5.1
    // one, a DTS core onto DTS-HD Master Audio -- as the picture writer
    // takes it: frame for frame, those frames saying what they are. Counted
    // unlike, it was refused here while the cut with pictures wrote it, or
    // (before the refusal) DTS went through the encoder whole, the master's
    // own sound included, and TrueHD failed on an experimental encoder.
    let carried = setup.mode != AudioMode::Reencode && crate::audio::carried_whole(setup.target);
    let unlike = pieces.iter().enumerate().filter(|&(n, _)| n != master).any(|(n, p)| {
        track_for(pieces, master, n, opts).is_ok_and(|t| {
            t.codec != info.codec
                || (!carried
                    && (t.sample_rate != info.sample_rate
                        || t.channels != info.channels
                        || framing(p.src, t) != shape_framing))
        })
    });
    // Unless the master's is lossless sound carried through as it is, which
    // nothing here re-encodes: the recording whose sound is another codec
    // would be silence. Refused as the picture writer refuses it, in its
    // words.
    if unlike && carried {
        let found = pieces.iter().enumerate().filter(|&(n, _)| n != master).find_map(|(n, p)| {
            let t = track_for(pieces, master, n, opts).ok()?;
            if t.codec == info.codec {
                return None;
            }
            let why: Vec<String> = [
                (t.codec != info.codec).then(|| format!("audio codec: {} vs {}", info.codec, t.codec)),
                (t.sample_rate != info.sample_rate).then(|| {
                    format!("sample rate: {} Hz vs {} Hz", info.sample_rate, t.sample_rate)
                }),
                (t.channels != info.channels)
                    .then(|| format!("channels: {} vs {}", info.channels, t.channels)),
            ]
            .into_iter()
            .flatten()
            .collect();
            (!why.is_empty()).then(|| (n, p.src, t.codec.clone(), why.join("; ")))
        });
        if let Some((n, reel, codec, why)) = found {
            return Err(crate::cut::lossless_master_refusal(
                &reel.path,
                n,
                &codec,
                &shape.src.path,
                setup.target,
                &why,
            ));
        }
    }
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
    // TrueHD (and MLP) frames are read against the last major sync, so a
    // range copied on from anywhere else is decoded against a header from
    // another part of the recording: noise, or frames thrown away. The
    // picture writer waits for a sync at the head of every range after the
    // first (`need_sync` in [`crate::cut`]); without the same wait here a
    // sound-only .mka of a Blu-ray's TrueHD failed to decode for a stretch
    // at every seam.
    let joins_at_sync = setup.mode != AudioMode::Reencode
        && matches!(setup.target, ff::codec::Id::TRUEHD | ff::codec::Id::MLP);

    let mut octx = ff::format::output(&*crate::input::as_output(output)).map_err(|e| anyhow!("{output}: {e}"))?;
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
            // And floats, which they hold as floats (AIFF-C's `fl32`):
            // named sixteen bits, a float track came out 16 bit even with
            // 24 asked for.
            (Id::PCM_S16BE, Id::PCM_F32BE | Id::PCM_F32LE) => Id::PCM_F32BE,
            (Id::PCM_S16BE, Id::PCM_F64BE | Id::PCM_F64LE) => Id::PCM_F64BE,
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
    // A FLAC frame numbers its own first sample, and a `.flac` opens on the
    // stream's STREAMINFO, which states the recording's length and the
    // checksum of all its samples. Copied, ten seconds out of a forty second
    // recording were a file that said it was forty seconds long, began ten
    // seconds in and failed its own checksum. FLAC re-encoded loses nothing,
    // and the encoder numbers its frames from nought and states what it wrote.
    if setup.target == ff::codec::Id::FLAC
        && octx.format().name() == "flac"
        && setup.mode != AudioMode::Reencode
    {
        setup.mode = AudioMode::Reencode;
        setup.frame_as = None;
    }

    // A container that has no box for the codec says so only as "Invalid
    // argument", once the header is being written. Asked first, so the run
    // can say which codec and which file.
    //
    // The codec the stream is declared as, which for a whole-track re-encode
    // is the encoder's: a 4K recording's LATM is re-encoded to raw AAC, and
    // asked about as LATM an .m4a was refused for a track it holds.
    let mut declared = if setup.mode == AudioMode::Reencode && setup.frame_as.is_none() {
        crate::audio::encoder_for(setup.target)
    } else {
        setup.target
    };
    let query = |id: ff::codec::Id| unsafe {
        ff::ffi::avformat_query_codec(
            (*octx.as_ptr()).oformat,
            id.into(),
            ff::ffi::FF_COMPLIANCE_NORMAL,
        )
    };
    let mut holds = query(declared);
    // A muxer with no table of its own answers "cannot say" (below nought)
    // for anything but its own codec, and the FLAC and Opus ones then turn
    // the rest away at the header as "Invalid argument" -- AC-3 asked for as
    // `.flac` or AAC as `.opus` ended on that and nothing else. They take
    // their own codec and only that. `.ogg`, `.oga` and `.spx` answer the
    // same and take Vorbis, Speex, FLAC and Opus alike (libavformat's
    // `ogg_init`), and turned MP2 or AAC away just as bare; the LATM muxer
    // takes AAC either way it is framed, and AMR's its two kinds.
    let own = unsafe { (*(*octx.as_ptr()).oformat).audio_codec };
    if holds < 0 {
        use ff::codec::Id;
        let takes: Option<&[Id]> = match octx.format().name() {
            "flac" | "opus" => Some(&[]),
            "ogg" | "oga" | "spx" => Some(&[Id::VORBIS, Id::SPEEX, Id::FLAC, Id::OPUS]),
            "latm" => Some(&[Id::AAC, Id::AAC_LATM]),
            "amr" => Some(&[Id::AMR_NB, Id::AMR_WB]),
            _ => None,
        };
        if let Some(takes) = takes {
            holds = i32::from(Id::from(own) == declared || takes.contains(&declared));
        }
    }
    // LATM is a framing and has no file of its own that anything plays:
    // a 4K recording's sound asked for as `.aac`, `.m4a` or `.mp4` -- the
    // names the window gives it -- was refused with advice to name the file
    // `.aac`, which is what it was already called. Where the container
    // holds plain AAC the track is re-encoded into it, as a join of unlike
    // tracks is above, and said.
    if holds == 0
        && setup.target == ff::codec::Id::AAC_LATM
        && declared != ff::codec::Id::AAC
        && query(ff::codec::Id::AAC) != 0
    {
        setup.mode = AudioMode::Reencode;
        setup.frame_as = None;
        declared = ff::codec::Id::AAC;
        holds = 1;
        crate::note_once(format!(
            "note: {output} cannot hold LATM-framed AAC as it stands, so the whole track is \
             re-encoded as plain AAC. A .mka keeps the recording's own frames."
        ));
    }
    // And the one container that does not answer: a transport stream takes
    // any codec and declares the ones it has no type for as private data, so
    // linear PCM written into a `.ts` came out as `bin_data` that nothing
    // plays -- and was reported as written. The picture writer asks
    // [`crate::carry`] for the same reason.
    let family = crate::carry::family(output);
    if family == "ts" && !crate::carry::holds(family, &crate::carry::name_of(declared)) {
        return Err(anyhow!(
            "{output} cannot carry {:?} sound: a transport stream would declare it as private \
             data, which no player reads. Name the file for the codec, or .wav or .mka",
            declared
        ));
    }
    // TrueHD has a box in an MP4 that the muxer writes only when told the
    // file may be outside the standard, and then only from one of the
    // stream's own sync points. The picture writer does both (see `outside`
    // and `need_sync` in [`crate::cut`]); this one does neither, and the
    // run ended on the muxer's "Experimental feature" (a .mov on "truehd
    // only supported in MP4").
    if declared == ff::codec::Id::TRUEHD && matches!(family, "mp4" | "mov") {
        return Err(anyhow!(
            "{output}: TrueHD sound on its own goes into a .mka (or a bare .thd), not an MP4 \
             or a QuickTime file"
        ));
    }
    if holds == 0 {
        // Signed eight bit samples are the one PCM a Matroska file turns
        // away, and the same samples at 24 bit are not.
        let wide = if declared == ff::codec::Id::PCM_S8 && octx.format().name() == "matroska" {
            ". --audio-bits 24 writes it as 24 bit PCM, which it holds"
        } else {
            ""
        };
        return Err(anyhow!(
            "{output} cannot hold {:?} sound. Name the file for that codec (.mp2, .ac3, \
             .aac and so on), or ask for another with --audio-codec{wide}",
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
    // Where the ranges written so far would have brought the sound had each
    // been laid to the sample: their lengths, end to end. See `drift` below.
    let mut laid = 0.0f64;
    // Whether the track's first frame has been laid yet. Until it has,
    // nothing is owed: see `head_owed`.
    let mut opened = false;
    let mut ranges_done = 0usize;
    // Whether the boundaries of a track carried whole have been asked about
    // yet. See `patches` below.
    let mut asked_whole = false;
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
        let mut ranges = kept[n].clone();
        if let Some(head) = ranges.first_mut() {
            head.0 = primed_from(piece.src, &track, *head)?;
        }
        let ranges = &ranges;
        let windows: Vec<(i64, i64)> = ranges
            .iter()
            .map(|&r| window(r, track.sample_rate))
            .collect();
        // The frames the boundaries fall inside, re-encoded with the far side
        // silenced and the fade ridden over the near one. Smart rendering,
        // and the same call the picture writer makes.
        //
        // Not asked again of a reel where the master's track is carried
        // whole: the answer is that nothing is patched (every reel is of the
        // master's codec, or the join was refused above), and asking printed
        // the note that says so once per recording of the join. A reel that
        // keeps no range is not an answer: the call returns before the note
        // when there are no windows, so it does not count as asked.
        let patches: std::collections::HashMap<i64, Patch> = if setup.mode == AudioMode::Smart
            && !(carried && asked_whole)
        {
            asked_whole = !windows.is_empty();
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
            // How far the sound laid so far runs ahead of (or behind) where
            // this range belongs in the file, which is the ranges before it
            // end to end. Every range keeps a whole number of frames, so each
            // ends up to half a frame off; not carried, the errors added up
            // -- eleven ranges of an AC-3 track ended 129 ms late, where the
            // cut with pictures, which carries it (`pick_from` in
            // [`crate::cut`]), stays within half a frame. Carried, a range
            // opens a frame earlier or later to take it back. Nought for the
            // first range, which is where nothing has been laid yet.
            //
            // And started over where the sound is well behind, as the picture
            // writer starts over: a range whose track ran out before its end
            // (a broadcast's sound stops before its pictures do) is not owed
            // the next range's opening seconds, which is sound the cut took
            // away.
            //
            // Counted from the track's first frame, as the picture writer
            // counts where its sound has reached (`end`) from the first frame
            // it lays: ranges before it that held no sound owe nothing.
            if !opened {
                laid = 0.0;
            }
            let drift = drift_from(written, &mut laid);
            let ramp = fades_for(
                fades.get(at).copied().unwrap_or((0.0, 0.0)),
                track.sample_rate,
                at,
                ranges_in_all,
                win,
            );
            let (reached, first, holes) = take_range(
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
                range.0 + drift,
                joins_at_sync && at > 0,
            )?;
            written = reached;
            if std::env::var("SMARTCUT_DEBUG").is_ok() {
                crate::say!(
                    "  sound range {:.4}-{:.4} laid={:.4} written={:.4} drift={:+.4} holes={:.4}",
                    range.0,
                    range.1,
                    laid,
                    written,
                    drift,
                    holes
                );
            }
            // Not only the first: a range whose own head has no sound (a hole
            // in the track mid-recording, a reel or a recorder stretch whose
            // sound starts after its pictures) is laid short by the hole as
            // well. The picture writer puts that range's frames at their own
            // instants and leaves out, as clashing, what the next range then
            // opens early on; laid end to end here it would be written -- a
            // fifth of a second of the sound the cut took away, in front of
            // the next range.
            if let Some((t, dur, before)) = first {
                opened = true;
                laid += head_owed(range.0 + drift, t, dur, before);
            }
            // And a hole further into the range, for the same reason: the
            // picture writer leaves it as silence and the frames after it at
            // their own instants, so the next range's early frames clash
            // and are left out there. Counted as drift here, the next range
            // opened that much early on the sound the cut took away -- and,
            // where a recorder stretch's sound breaks off a moment before
            // the seam and resumes, on the stretch at the seam that no cut
            // writes (0.28 and 0.44 s on a BD-RE title kept across two).
            laid -= holes;
            laid += range.1 - range.0;
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
    // AIFF counts its chunk sizes in 32 bits as RIFF does, and has no RF64 to
    // fall back on: past four gigabytes libavformat writes the sizes wrapped
    // and says nothing, and what is left is a file every reader takes to be a
    // few hundred megabytes long. Refused rather than written that way.
    if octx.format().name() == "aiff" {
        const SEEK_CUR: std::os::raw::c_int = 1;
        let size = unsafe { ff::ffi::avio_seek((*octx.as_ptr()).pb, 0, SEEK_CUR) };
        if size > i64::from(u32::MAX) {
            return Err(anyhow!(
                "{output} would be over 4 GB, which an AIFF file cannot state. Name it .wav \
                 (written as RF64 where it needs to be) or .caf instead"
            ));
        }
    }
    octx.write_trailer()?;
    say(ranges_in_all);
    Ok(())
}

/// Where a range read from the head of a recording starts, where the cut
/// with pictures leaves out the frame the track opens on: where the cut
/// starts it.
///
/// **An encoder's priming.** An `ffmpeg`-made .mkv stamps its AAC's first
/// frame -- the encoder's priming, 1024 samples of it -- at -0.021 s, ahead
/// of a first picture at 0 (a remux of an .mp4 shifts both, and the frame is
/// at 0 with the first picture a frame after it). The cut with pictures
/// starts the range on that picture and claims the frame nearest it
/// ([`crate::cut`]'s `take_audio`), so the priming frame is not in its
/// sound; read from the range's own start, this writer opened on it, and the
/// whole of a sound-only file of such a recording ran one frame (21 ms) late
/// against the cut's sound (282 frames for 281 in six seconds). Started
/// where the cut starts, it claims the same first frame, copied or
/// re-encoded, and lays its ranges against the same start.
///
/// Only that frame. The sound a broadcast sends ahead of its first picture
/// -- half a second of it, stamped after the recording's zero like all of it
/// -- is the recording's own and is kept, as it always was; what is left out
/// is a frame stamped before the recording's zero, and a track that opens
/// less than one frame ahead of the cut's start on a frame the cut leaves
/// out (more than half of it ahead). A track that opens on a frame the cut
/// claims, or opens after the cut's start, keeps its range as it was.
///
/// Not the cut's other rule at the head, that its first range opens on no
/// frame before its start (`min_start`): an AC-3 or Opus priming frame mostly
/// after the start is left out there and the cut's sound opens a frame in,
/// on a gap the container keeps. This file is laid from its first frame with
/// no gap to keep, so the same frame left out here would put the sound a
/// frame early; kept, it is within half a frame, as every range is.
fn primed_from(src: &Source, track: &AudioInfo, range: (f64, f64)) -> Result<f64> {
    let cut_from = src.points.first().map_or(range.0, |p| p.time).max(range.0).min(range.1);
    if cut_from <= range.0 {
        return Ok(range.0);
    }
    let ictx = crate::input::demux(&src.input.url)?;
    let Some(stream) = ictx.stream(track.stream_index) else {
        return Ok(range.0);
    };
    let at = stream.start_time();
    if at == ff::ffi::AV_NOPTS_VALUE {
        return Ok(range.0);
    }
    // SAFETY: the parameters belong to a stream of a context still open.
    let size = unsafe { (*stream.parameters().as_ptr()).frame_size };
    let frame = if size > 0 && track.sample_rate > 0 {
        f64::from(size) / f64::from(track.sample_rate)
    } else {
        0.0
    };
    // A 33-bit clock (a transport or program stream) is stamped before its
    // zero only where it wraps: a broadcast recorded in the last minute
    // before its clock turns over has every frame up to the turn stamped
    // negative by libavformat, and the sound it sends ahead of its pictures
    // was taken for priming and left out -- up to all of it, where the cut
    // with pictures in the same file is laid as in any other recording.
    // SAFETY: the stream belongs to a context still open.
    let wraps = unsafe { (*stream.as_ptr()).pts_wrap_bits } < 63;
    let raw = at as f64 * track.time_base;
    Ok(cut_start(range.0, cut_from, src.start_time, raw, frame, !wraps))
}

/// The arithmetic of [`primed_from`]: `from` is the range's own start,
/// `cut_from` the cut's, `start_time` the recording's, `raw` where the track's
/// first frame is stamped (on the recording's own clock, before `start_time`
/// is taken off), `frame` how long one frame lasts (0 where it varies) and
/// `primes` whether a stamp before the zero can be priming at all (not on a
/// 33-bit clock, where it is the clock wrapping).
fn cut_start(from: f64, cut_from: f64, start_time: f64, raw: f64, frame: f64, primes: bool) -> f64 {
    // Stamped before the recording's zero: an encoder's priming, left out up
    // to the zero (or the cut's start, where that comes first).
    let mut from = from;
    if primes && raw < 0.0 {
        from = from.max(cut_from.min(-start_time));
    }
    // And where what is left opens on a frame the cut leaves out, less than
    // one frame ahead of its start -- give or take the millisecond a
    // Matroska clock rounds the picture's time by.
    let opens = (raw - start_time).max(from);
    let ahead = cut_from - opens;
    if frame > 0.0 && ahead >= frame / 2.0 && ahead < frame + 1e-3 {
        from = cut_from;
    }
    from
}

/// How far the sound written (`written`) has drifted from where the ranges
/// before put the next one (`laid`), which is what the next range opens
/// against. Where it is more than half a second behind, the track had a hole
/// rather than a drift, and the count starts over from where the sound is.
/// The rule `pick_from` has in [`crate::cut`].
fn drift_from(written: f64, laid: &mut f64) -> f64 {
    let drift = written - *laid;
    if drift < -0.5 {
        *laid = written;
        return 0.0;
    }
    drift
}

/// What a range is owed before its own length is counted (added to `laid`):
/// nothing, unless its sound starts later than the range does -- a file
/// whose sound begins a few tenths of a second after its pictures, kept from
/// the head, or a hole in the track at a range's head further on. The audio
/// file has no silence to put there, so the first frame goes where the sound
/// has reached; counted as owed, the next range read that lateness of the
/// sound the cut took away and laid it in front of its own start. The picture
/// writer places that frame at its own instant and leaves out what would
/// land on it, so it writes none of that sound either.
///
/// A frame opening up to half its length late is the ordinary rounding of
/// the opening rule and stays counted as such, and so is one the frame
/// before it runs on into (`before`, where that frame ended): frames of
/// Vorbis or Opus differ in length, and after a long one a short one can
/// open more than half its own length after the start with nothing missing.
///
/// `start` is the instant the range's first frame was chosen against.
fn head_owed(start: f64, opened_at: f64, dur: f64, before: Option<f64>) -> f64 {
    let late = opened_at - start;
    let runs_on = before.is_some_and(|end| end >= opened_at - 2e-3);
    if late > dur / 2.0 + 1e-3 && !runs_on {
        -late
    } else {
        0.0
    }
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

/// Whether a read from the head of a recording just opened has to seek
/// there all the same rather than read on from the open.
///
/// **Only where the open has already lost something.** A Matroska file with a
/// WebVTT stream: the open's look ahead reads with every stream switched on,
/// and a WebVTT block libavformat cannot parse -- a video site's, see
/// [`crate::input::keep_only`] -- makes it drop the rest of the cluster the
/// block is in, sound and pictures alike: read from the open, a cut of such a
/// file from its head lost its first two seconds (527 pictures of 584 in
/// `tests/run_demux_tests.sh`, 826 of 915 frames of sound). Sought, the read
/// starts again with the subtitles switched off, and nothing is lost.
///
/// But the seek costs something too, so it is not made on every such file.
/// It flushes what libavformat knows of the timestamps, and a key picture's
/// open-GOP leading pictures in front of 0 -- which Matroska cannot state, so
/// libavformat works them out -- come back from it worked out wrong, behind
/// the key picture instead of before it: a remuxed recording with a WebVTT
/// track lost a picture from its head (143 of 144, 10 dB). So the open's
/// packets are checked against the sought read's, see [`open_lost`], and the
/// seek is made only where the open is missing some of them. Every other
/// recording is read on from the open, without looking.
///
/// The one rule for both writers: `seek_to` in [`crate::cut`] asks it as well,
/// and the two disagreeing put a different first frame at the head of the
/// sound-only file than at the head of the cut's.
pub(crate) fn sought_at_head(ictx: &ff::format::context::Input, url: &str) -> bool {
    let matroska = ictx.format().name().split(',').any(|n| n.trim() == "matroska");
    let webvtt = ictx.streams().any(|s| {
        let par = s.parameters();
        par.medium() == ff::media::Type::Subtitle && par.id() == ff::codec::Id::WEBVTT
    });
    // A check that cannot be made seeks, as every such file did before.
    matroska && webvtt && open_lost(url).unwrap_or(true)
}

/// Whether a fresh open of `url` hands over fewer of the head's pictures and
/// sound than a read sought to the head with the subtitles switched off.
///
/// What the open can lose, it loses while it looks ahead: once it is open the
/// readers switch the subtitles off, and a block of a stream switched off is
/// never parsed. So the loss lies inside the bytes the look ahead went
/// through, and where the demuxer stands just after the open bounds it.
/// Every packet of the sought read from before there has to be among the
/// open's, by stream and byte position -- both reads hand over the same
/// blocks of the same file, so they are named the same. A packet that does
/// not say where it was read is a check that cannot be made.
fn open_lost(url: &str) -> Result<bool> {
    // How far past the bound each read goes on: a parser can hand a packet
    // from before it over after one from beyond.
    const PAST: usize = 64;
    const SEEK_CUR: std::os::raw::c_int = 1;
    // Not a cover picture: an attachment Matroska shows as a picture stream
    // is one packet handed over at the open and again after every seek, and
    // never read from a block, so it says nowhere that it was read -- and a
    // video site's recording saved with its thumbnail and its subtitles was
    // sought for that alone, and lost the picture the seek costs (145
    // packets for 144, the copy failing its check).
    let carried = |ictx: &ff::format::context::Input| -> Vec<usize> {
        ictx.streams()
            .filter(|s| {
                matches!(s.parameters().medium(), ff::media::Type::Video | ff::media::Type::Audio)
                    && !s.disposition().contains(ff::format::stream::Disposition::ATTACHED_PIC)
            })
            .map(|s| s.index())
            .collect()
    };
    let mut open = crate::input::demux(url)?;
    // SAFETY: the context is open and its reader is only read here.
    let bound = unsafe {
        let pb = (*open.as_ptr()).pb;
        if pb.is_null() {
            return Ok(true);
        }
        ff::ffi::avio_seek(pb, 0, SEEK_CUR)
    };
    if bound <= 0 {
        return Ok(true);
    }
    let keep = carried(&open);
    crate::input::keep_only(&mut open, &keep);
    let mut had = std::collections::HashSet::new();
    let mut past = 0;
    for (stream, packet) in open.read_packets() {
        if !keep.contains(&stream.index()) {
            continue;
        }
        let at = packet.position() as i64;
        if at < 0 {
            return Ok(true);
        }
        if at >= bound {
            past += 1;
            if past > PAST {
                break;
            }
        } else {
            had.insert((stream.index(), at));
        }
    }
    drop(open);
    let mut sought = crate::input::demux(url)?;
    // As `seek_to` in [`crate::cut`] seeks to the head, before the discards.
    let target = i64::MIN / 2;
    sought.seek(target, ..target)?;
    crate::input::keep_only(&mut sought, &keep);
    let mut past = 0;
    for (stream, packet) in sought.read_packets() {
        if !keep.contains(&stream.index()) {
            continue;
        }
        let at = packet.position() as i64;
        if at < 0 {
            return Ok(true);
        }
        if at >= bound {
            past += 1;
            if past > PAST {
                break;
            }
        } else if !had.contains(&(stream.index(), at)) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The first frame a range laid: its instant, its length, and where the
/// track's frame before it ended, where the read passed one over.
type Opening = (f64, f64, Option<f64>);

/// Read one range's sound and write it.
///
/// `pick_from` is the instant the range's first frame is chosen against:
/// its start, moved by however far the sound before it has drifted.
///
/// Returns where the output has reached, in seconds, and the instant and
/// length of the first of the recording's frames the range laid (copied,
/// or left out waiting for a sync), where it laid one, with where the
/// track's frame before that one ended, where the read passed one over;
/// and how much of the range after that first frame the track had no sound
/// for (see `hole_before`).
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
    pick_from: f64,
    mut need_sync: bool,
) -> Result<(f64, Option<Opening>, f64)> {
    let mut ictx = crate::input::demux(&src.input.url)?;
    // Half a second before the range, so the frame it opens on has been
    // decoded up to rather than seeked into: a seek in a transport stream
    // lands where it can rather than where it was asked, and an AAC frame
    // decoded straight after one is missing half its window.
    //
    // A range that opens at the head is not seeked for at all: the file was
    // just opened and is standing at its first byte. Any seek lands on a
    // picture, and a broadcast's sound starts before its first one does --
    // half a second of it on an off-air recording, which a whole-recording
    // .wav had as silence and an .aac did not have at all.
    //
    // Except where the cut with pictures seeks at the head too: a Matroska
    // file with WebVTT in it whose open, read on, has already lost whole
    // clusters to a block libavformat cannot parse -- 826 of 915 frames of
    // sound, the first two seconds gone, where the cut with pictures had them
    // all. See [`sought_at_head`].
    let landing = (range.0 - 0.5).max(0.0);
    if landing > 0.0 {
        let target = ((landing + src.start_time) * f64::from(ff::ffi::AV_TIME_BASE)) as i64;
        ictx.seek(target, ..target)?;
    } else if sought_at_head(&ictx, &src.input.url) {
        let target = i64::MIN / 2;
        ictx.seek(target, ..target)?;
    }
    // After the seek, for the reason [`crate::input::keep_only`] gives, and
    // with the pictures, which say when the range is over where the track
    // has nothing in it. See [`crate::input::keep_with_pictures`].
    crate::input::keep_with_pictures(&mut ictx, &[track.stream_index]);

    let in_tb = track.time_base;
    let mut lengths = Lengths::new(ictx.stream(track.stream_index).map(|s| s.parameters()));
    let mut first: Option<Opening> = None;
    // Where the last of the track's frames passed over before the range
    // opens ended. See `head_owed`.
    let mut before: Option<f64> = None;
    // Where the last frame the range laid ended, where the latest of them
    // started, and the holes between its frames so far.
    let mut reached: Option<f64> = None;
    let mut latest: Option<f64> = None;
    let mut holes = 0.0f64;
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
        // A frame that comes with no length at all, on a track whose
        // packets otherwise carry theirs: the first sound block of an
        // `ffmpeg -c copy` .mkv of a broadcast, read as the open read it.
        // Of no length it was passed over as sound before the range, the
        // range opened a frame late and every frame after went down 21 ms
        // early -- where the picture writer, which gives such a frame its
        // own length (`take_audio` in [`crate::cut`]), keeps it. Read once,
        // as there: Vorbis's parser keeps the block before, and asked twice
        // of one packet it answered the second time as if the packet had
        // followed itself.
        let dur = match (packet.duration(), clock.frame_secs) {
            (own, _) if own > 0 => lengths.of(&packet, in_tb, own as f64 * in_tb),
            (_, Some(frame)) if frame > 0.0 => lengths.of(&packet, in_tb, frame),
            _ => lengths.own(&packet).unwrap_or(0.0),
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
            // A frame of no stated length is taken whatever its time: its
            // length unknown, the one the range opens in looked like one
            // that ends where it begins, and the opening of every range was
            // left out -- a whole second of WavPack in a short .mkv. The
            // window trims what is early.
            if dur <= 0.0 || t + 2.0 * dur > range.0 {
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
        // late -- the start as the sound so far has drifted from it, so the
        // errors do not add up. The picture writer's own rule; see
        // `take_audio` there, which records why being cleverer is not
        // available.
        if t + dur / 2.0 <= pick_from {
            if dur > 0.0 {
                before = Some(t + dur);
            }
            continue;
        }
        if dur <= 0.0 {
            // A frame with no length is a frame there is nowhere to put:
            // what follows would land on top of it.
            continue;
        }
        first.get_or_insert((t, dur, before));
        // A frame that starts before one the range has laid already: the
        // track's timestamps stepped back (a second or so before some
        // recorder seams, by a fifth or a third of a second), and what
        // follows is sound for instants already written. The picture writer
        // leaves it out as clashing with the last frame it wrote. Laid end to
        // end here, it put everything after it that much later -- and,
        // carried as drift, the next range opened that much late, on the
        // sound the cut kept. A millisecond's leeway for a clock that keeps
        // whole ones.
        if behind(latest, t) {
            if std::env::var("SMARTCUT_DEBUG").is_ok() {
                crate::say!("  sound behind {:.4}-{:.4} in range {:.4}-{:.4}", t, t + dur, range.0, range.1);
            }
            continue;
        }
        let hole = hole_before(reached, t, dur);
        if hole > 0.0 && std::env::var("SMARTCUT_DEBUG").is_ok() {
            crate::say!("  sound hole {:.4}-{:.4} in range {:.4}-{:.4}", t - hole, t, range.0, range.1);
        }
        holes += hole;
        reached = Some(reached.map_or(t + dur, |r| r.max(t + dur)));
        latest = Some(latest.map_or(t, |l| l.max(t)));
        if need_sync {
            if !opens_on_sync(packet.data()) {
                // Left out, but its instant is still spent, so what follows
                // stays where the recording had it -- as the picture writer
                // counts it.
                written += dur;
                continue;
            }
            need_sync = false;
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
    Ok((written, first, holes))
}

/// How much sound the track is missing just before a frame of a range,
/// after the frame before it in the same range ended at `reached`: nothing
/// where it runs on, within the rounding of a clock that keeps whole
/// milliseconds and of timestamps that wander a little; the gap where it is
/// more than half the frame (the bound `head_owed` keeps at a range's head).
fn hole_before(reached: Option<f64>, t: f64, dur: f64) -> f64 {
    match reached {
        Some(end) if t - end > dur / 2.0 + 1e-3 => t - end,
        _ => 0.0,
    }
}

/// Whether a frame of a range starts before the latest one the range laid
/// (`latest`) by more than a millisecond's rounding: a frame for instants
/// already written, which the picture writer leaves out as clashing. One
/// that starts after it is laid, as it is there, even where it overlaps.
/// TrueHD in Matroska hands frames over sharing a millisecond, or one
/// rounded a fraction of one before the frame ahead of it.
fn behind(latest: Option<f64>, t: f64) -> bool {
    latest.is_some_and(|l| t < l - 1e-3)
}

/// How long a frame really lasts, where the container's clock is too coarse
/// to say.
///
/// The frames are laid end to end by their lengths, so a length that is out
/// is out at every frame. Matroska keeps milliseconds: an AAC frame of 1024
/// samples at 48 kHz, 21.33 ms, comes back as 21, and a hundred seconds of
/// an .mkv's sound written as an .m4a said it was 98.4 -- every frame
/// stamped 1.6 % early, which is a track that runs ahead of any picture it
/// is put back beside. The codec knows the length from the frame itself; it
/// is taken where it rounds to what the container said, and only where the
/// container counts more coarsely than one sample, so a transport stream or
/// an MP4 is laid exactly as it was.
///
/// libavcodec answers for the codecs whose frames are all one length. FLAC,
/// Vorbis and Opus frames are not, and it answers nought for them: the
/// length is read from the frame here instead -- FLAC's header states its
/// block size, Opus's first byte its frame size and count, and a Vorbis
/// packet's mode names one of the two block sizes in the stream's setup
/// header, overlapped with the block before it. Without, an .mkv's FLAC came
/// out 0.5 % short of its samples, Vorbis 0.9 % and Opus in 2.5 ms frames a
/// fifth. DTS is read the same way, from its core header's block count and
/// rate: libavcodec answers nought for it too, and a DTS-HD Master Audio
/// track it states at 96 kHz carries 48 kHz core frames.
///
/// The picture writer asks the same question of the same frames -- see
/// `take_audio` in [`crate::cut`] -- and asks it here.
pub(crate) struct Lengths {
    par: Option<ff::codec::Parameters>,
    /// libavcodec's Vorbis parser, which keeps the size of the block before:
    /// fed every packet in order from wherever the read began.
    vorbis: *mut ff::ffi::AVVorbisParseContext,
}

impl Lengths {
    pub(crate) fn new(par: Option<ff::codec::Parameters>) -> Self {
        let vorbis = par
            .as_ref()
            .filter(|p| p.id() == ff::codec::Id::VORBIS)
            .map_or(std::ptr::null_mut(), |p| unsafe {
                let p = p.as_ptr();
                if (*p).extradata.is_null() || (*p).extradata_size <= 0 {
                    std::ptr::null_mut()
                } else {
                    ff::ffi::av_vorbis_parse_init((*p).extradata, (*p).extradata_size)
                }
            });
        Lengths { par, vorbis }
    }

    pub(crate) fn of(&mut self, packet: &ff::Packet, tb: f64, said: f64) -> f64 {
        let Some(par) = self.par.as_ref() else { return said };
        let rate = unsafe { (*par.as_ptr()).sample_rate };
        if rate <= 0 || tb * f64::from(rate) <= 1.0 {
            return said;
        }
        let Some(exact) = self.own(packet) else { return said };
        if (exact - said).abs() < tb {
            exact
        } else {
            said
        }
    }

    /// The frame's length as the frame itself has it, whatever the
    /// container says -- or does not: a packet that arrives with no length
    /// at all (the first sound block of an `ffmpeg -c copy` .mkv, handed
    /// over as the open read it) has one all the same. `None` where the
    /// frame does not say.
    pub(crate) fn own(&mut self, packet: &ff::Packet) -> Option<f64> {
        let par = self.par.as_ref()?;
        let (rate, id, fixed) = unsafe {
            let p = par.as_ptr() as *mut ff::ffi::AVCodecParameters;
            let size = i32::try_from(packet.size()).unwrap_or(0);
            ((*p).sample_rate, par.id(), ff::ffi::av_get_audio_frame_duration2(p, size))
        };
        if rate <= 0 {
            return None;
        }
        let data = packet.data().unwrap_or(&[]);
        // A DTS frame by its core, before anything libavcodec may know of the
        // track's frame size: that is counted at the rate the track states,
        // and a Master Audio track states its extension's. Opus counts its
        // frames at 48 kHz whatever rate the track states.
        let core = if id == ff::codec::Id::DTS { dts_core(data) } else { None };
        let (samples, rate) = match id {
            _ if core.is_some() => core.unwrap_or((0, rate)),
            _ if fixed > 0 => (i64::from(fixed), rate),
            ff::codec::Id::FLAC => (flac_block(data).unwrap_or(0), rate),
            ff::codec::Id::OPUS => (opus_samples(data).unwrap_or(0), 48_000),
            ff::codec::Id::VORBIS if !self.vorbis.is_null() && !data.is_empty() => {
                let mut flags = 0;
                let n = unsafe {
                    ff::ffi::av_vorbis_parse_frame_flags(
                        self.vorbis,
                        data.as_ptr(),
                        i32::try_from(data.len()).unwrap_or(i32::MAX),
                        &mut flags,
                    )
                };
                (i64::from(n), rate)
            }
            _ => (0, rate),
        };
        (samples > 0).then(|| samples as f64 / f64::from(rate))
    }
}

impl Drop for Lengths {
    fn drop(&mut self) {
        if !self.vorbis.is_null() {
            unsafe { ff::ffi::av_vorbis_parse_free(&mut self.vorbis) };
        }
    }
}

/// The samples in a FLAC frame, from its header's block size code.
fn flac_block(data: &[u8]) -> Option<i64> {
    // The sync code, fixed or variable blocking.
    if data.len() < 5 || data[0] != 0xFF || data[1] & 0xFE != 0xF8 {
        return None;
    }
    let code = data[2] >> 4;
    match code {
        1 => Some(192),
        2..=5 => Some(576 << (code - 2)),
        8..=15 => Some(256 << (code - 8)),
        6 | 7 => {
            // Stated after the frame (or sample) number, which is coded the
            // way UTF-8 codes a character, in one to seven bytes.
            let lead = data[4];
            let coded = match lead.leading_ones() {
                0 => 1,
                n @ 2..=7 => n as usize,
                _ => return None,
            };
            let at = 4 + coded;
            if code == 6 {
                data.get(at).map(|&b| i64::from(b) + 1)
            } else {
                let b = data.get(at..at + 2)?;
                Some(i64::from(u16::from_be_bytes([b[0], b[1]])) + 1)
            }
        }
        _ => None,
    }
}

/// The samples in a DTS frame and the rate they are counted at, from its
/// core header: a block count of 32 samples each, and the core's own rate,
/// which is what the frame lasts by whatever an extension raises the rate
/// to. Only the ordinary 16-bit big-endian sync.
fn dts_core(data: &[u8]) -> Option<(i64, i32)> {
    if data.len() < 9 || data[..4] != [0x7F, 0xFE, 0x80, 0x01] {
        return None;
    }
    // FTYPE 1, SHORT 5, CPF 1, NBLKS 7, FSIZE 14, AMODE 6, SFREQ 4.
    let bits = u64::from_be_bytes([data[4], data[5], data[6], data[7], data[8], 0, 0, 0]);
    let nblks = (bits >> (64 - 14)) & 0x7F;
    let sfreq = (bits >> (64 - 38)) & 0x0F;
    let rate = [0, 8000, 16000, 32000, 0, 0, 11025, 22050, 44100, 0, 0, 12000, 24000, 48000, 0, 0]
        [sfreq as usize];
    // At least six blocks in a normal frame (5 is the least NBLKS allows).
    (nblks >= 5 && rate > 0).then_some(((nblks as i64 + 1) * 32, rate))
}

/// The samples in an Opus packet at 48 kHz, from its table-of-contents byte
/// (RFC 6716, 3.1).
fn opus_samples(data: &[u8]) -> Option<i64> {
    let toc = *data.first()?;
    let config = toc >> 3;
    let frame: i64 = match config {
        0..=11 => [480, 960, 1920, 2880][usize::from(config & 3)],
        12..=15 => [480, 960][usize::from(config & 1)],
        _ => [120, 240, 480, 960][usize::from(config & 3)],
    };
    let frames = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => i64::from(*data.get(1)? & 0x3F),
    };
    // A packet holds at most 120 ms.
    (frames > 0 && frame * frames <= 5760).then_some(frame * frames)
}

/// Whether a TrueHD or MLP frame opens on a major sync, which a range may
/// open on. See `opens_a_truehd_track` in [`crate::cut`].
fn opens_on_sync(data: Option<&[u8]>) -> bool {
    data.is_some_and(|d| d.len() >= 8 && d[4..7] == [0xF8, 0x72, 0x6F] && matches!(d[7], 0xBA | 0xBB))
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
    match packet.write_interleaved(octx) {
        // A frame whose ADTS header is damaged, which the MP4 and Matroska
        // muxers' aac_adtstoasc turns away. One such frame used to fail the
        // whole output; it is left out and the sound goes on after it.
        Err(ff::Error::PatchWelcome | ff::Error::InvalidData) => {
            crate::say!("note: a frame of the sound at {at:.3}s has a damaged header and was left out.");
            Ok(())
        }
        r => Ok(r?),
    }
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
    // The length as well, which the encoder also counts in samples. Left
    // so, a Matroska file -- counting in milliseconds -- took its last frame
    // of 1024 samples to last 1.024 s, and stated the track a second longer
    // than it is.
    if packet.duration() > 0 {
        let secs = packet.duration() as f64 / clock.rate.max(1.0);
        packet.set_duration((secs / clock.tb).round() as i64);
    }
    packet.set_position(-1);
    packet.write_interleaved(octx)?;
    Ok(seconds)
}

#[cfg(test)]
mod tests {
    use super::{behind, cut_start, drift_from, dts_core, flac_block, head_owed, hole_before, less, opus_samples};

    #[test]
    fn ranges_lose_the_unwritten_stretches() {
        let holes = [(10.0, 12.0), (20.0, 20.5)];
        let eps = 0.0167;
        // Across both, as the planner cuts it: up to each, on from its end.
        assert_eq!(less(&[(5.0, 25.0)], &holes, eps), vec![(5.0, 10.0), (12.0, 20.0), (20.5, 25.0)]);
        // Opening or ending inside one; wholly inside one.
        assert_eq!(less(&[(11.0, 15.0)], &holes, eps), vec![(12.0, 15.0)]);
        assert_eq!(less(&[(8.0, 11.0)], &holes, eps), vec![(8.0, 10.0)]);
        assert_eq!(less(&[(10.5, 11.5)], &holes, eps), Vec::<(f64, f64)>::new());
        // Less than half a picture past one is not a piece.
        assert_eq!(less(&[(8.0, 12.01)], &holes, eps), vec![(8.0, 10.0)]);
        // Clear of them, however short.
        assert_eq!(less(&[(1.0, 1.01), (13.0, 19.0)], &holes, eps), vec![(1.0, 1.01), (13.0, 19.0)]);
    }

    #[test]
    fn a_late_sound_owes_nothing_at_the_head() {
        // Within half a frame of the start: the opening rule's own rounding.
        assert_eq!(head_owed(0.0, 0.0107, 0.0213, None), 0.0);
        assert_eq!(head_owed(5.0, 4.99, 0.032, Some(4.99)), 0.0);
        // The track has nothing there: the lateness is not owed.
        assert!((head_owed(0.0, 0.278, 0.0213, None) + 0.278).abs() < 1e-9);
        assert!((head_owed(1.0, 1.0485, 0.0213, None) + 0.0485).abs() < 1e-9);
        // A hole mid-track: the frame before ends well short of this one.
        assert!((head_owed(10.1, 10.3253, 0.0213, Some(10.0267)) + 0.2253).abs() < 1e-9);
        // A short Vorbis frame after a long one, on a millisecond clock:
        // more than half its own length late, but nothing is missing.
        assert_eq!(head_owed(3.0, 3.0105, 0.012, Some(3.0098)), 0.0);
    }

    #[test]
    fn a_hole_inside_a_range_is_not_drift() {
        // The range's first frame, and frames that run on: within a
        // millisecond clock's rounding, either way.
        assert_eq!(hole_before(None, 10.0, 0.0213), 0.0);
        assert_eq!(hole_before(Some(10.0213), 10.0213, 0.0213), 0.0);
        assert_eq!(hole_before(Some(10.0213), 10.022, 0.0213), 0.0);
        assert_eq!(hole_before(Some(10.0213), 10.020, 0.0213), 0.0);
        assert_eq!(hole_before(Some(3.0103), 3.012, 0.0027), 0.0);
        // A frame missing, and a stretch of them.
        assert!((hole_before(Some(10.0213), 10.0426, 0.0213) - 0.0213).abs() < 1e-9);
        assert!((hole_before(Some(10.0267), 10.3253, 0.0213) - 0.2986).abs() < 1e-9);
    }

    #[test]
    fn a_frame_behind_what_was_laid_is_left_out() {
        // Running on, overlapping by a millisecond clock's rounding (TrueHD
        // frames sharing a timestamp), or by part of a frame: laid.
        assert!(!behind(None, 10.0));
        assert!(!behind(Some(10.0), 10.0213));
        assert!(!behind(Some(10.002), 10.002));
        assert!(!behind(Some(10.002), 10.0018));
        // Short frames starting inside a long one: laid, as the picture
        // writer lays them.
        assert!(!behind(Some(18.005), 18.012));
        // Timestamps stepped back by a fifth of a second: left out until
        // they pass the latest frame laid.
        assert!(behind(Some(11.384), 11.2193));
        assert!(behind(Some(11.384), 11.3687));
        assert!(!behind(Some(11.384), 11.39));
    }

    #[test]
    fn the_drift_is_carried_and_a_hole_starts_over() {
        let mut laid = 0.0;
        assert_eq!(drift_from(0.0, &mut laid), 0.0);
        laid = 1.0;
        assert!((drift_from(1.0107, &mut laid) - 0.0107).abs() < 1e-9);
        assert!((drift_from(0.7, &mut laid) + 0.3).abs() < 1e-9);
        assert_eq!(laid, 1.0);
        assert_eq!(drift_from(0.4, &mut laid), 0.0);
        assert_eq!(laid, 0.4);
    }

    #[test]
    fn the_head_starts_where_the_cut_does_only_past_a_priming_frame() {
        let aac = 1024.0 / 48000.0;
        // ffmpeg's .mkv: AAC priming at -0.021, the first picture at 0.
        assert!((cut_start(0.0, 0.021, -0.021, -0.021, aac, true) - 0.021).abs() < 1e-9);
        // The same remuxed from an .mp4: everything shifted, the frame at 0.
        let aac441 = 1024.0 / 44100.0;
        assert_eq!(cut_start(0.0, 0.023, 0.0, 0.0, aac441, true), 0.023);
        // A broadcast's sound half a second ahead of its pictures is kept.
        assert_eq!(cut_start(0.0, 0.624, 1.4, 1.4, aac, true), 0.0);
        // A priming frame ahead of sound that runs on before the pictures:
        // only the priming goes.
        assert!((cut_start(0.0, 1.323, -0.021, -0.021, aac, true) - 0.021).abs() < 1e-9);
        // A first frame the cut claims (less than half of it ahead) or one
        // after the cut's start: unchanged.
        assert_eq!(cut_start(0.0, 0.008, 0.0, 0.0, aac, true), 0.0);
        assert_eq!(cut_start(0.0, 0.0, 0.0, 0.004, aac, true), 0.0);
        // A broadcast recorded just before its 33-bit clock wraps: the sound
        // ahead of its pictures is stamped before the zero and still kept.
        assert_eq!(cut_start(0.0, 0.624, -0.3, -0.3, aac, false), 0.0);
        // Opus priming that the nearest-frame rule keeps: up to its zero.
        assert!((cut_start(0.0, 0.007, -0.007, -0.007, 0.0, true) - 0.007).abs() < 1e-9);
    }

    #[test]
    fn flac_block_sizes() {
        // 4096 (code 12), 44.1 kHz, frame number 0.
        assert_eq!(flac_block(&[0xFF, 0xF8, 0xC9, 0x18, 0x00, 0xC2]), Some(4096));
        // 1152 (code 3), variable blocking.
        assert_eq!(flac_block(&[0xFF, 0xF9, 0x39, 0x18, 0x00, 0x00]), Some(1152));
        assert_eq!(flac_block(&[0xFF, 0xF8, 0x19, 0x18, 0x00, 0x00]), Some(192));
        assert_eq!(flac_block(&[0xFF, 0xF8, 0x89, 0x18, 0x00, 0x00]), Some(256));
        // 8-bit size after a one-byte frame number: the short last block.
        assert_eq!(flac_block(&[0xFF, 0xF8, 0x69, 0x18, 0x05, 0x9F, 0x00]), Some(160));
        // 16-bit size after a two-byte frame number (0xC2 0x80 = 128).
        assert_eq!(flac_block(&[0xFF, 0xF8, 0x79, 0x18, 0xC2, 0x80, 0x0F, 0xFF, 0x00]), Some(4096));
        // Reserved size, no sync, too short, a stray continuation byte.
        assert_eq!(flac_block(&[0xFF, 0xF8, 0x09, 0x18, 0x00]), None);
        assert_eq!(flac_block(&[0xFF, 0xF0, 0xC9, 0x18, 0x00]), None);
        assert_eq!(flac_block(&[0xFF, 0xF8, 0xC9]), None);
        assert_eq!(flac_block(&[0xFF, 0xF8, 0x69, 0x18, 0x80, 0x00]), None);
        assert_eq!(flac_block(&[0xFF, 0xF8, 0x79, 0x18, 0x00, 0x01]), None);
    }

    #[test]
    fn dts_core_frames() {
        // 512 samples (NBLKS 15) at 48 kHz (SFREQ 13): FTYPE 1, SHORT 31,
        // CPF 0, NBLKS 15, FSIZE 1023, AMODE 9, SFREQ 13.
        let head = |nblks: u64, sfreq: u64| {
            let bits: u64 = (1 << 63) | (31 << 58) | (nblks << 50) | (1023 << 36) | (9 << 30) | (sfreq << 26);
            let b = bits.to_be_bytes();
            vec![0x7F, 0xFE, 0x80, 0x01, b[0], b[1], b[2], b[3], b[4], 0]
        };
        assert_eq!(dts_core(&head(15, 13)), Some((512, 48000)));
        assert_eq!(dts_core(&head(15, 8)), Some((512, 44100)));
        assert_eq!(dts_core(&head(7, 13)), Some((256, 48000)));
        // A reserved rate, too few blocks, no sync, too short.
        assert_eq!(dts_core(&head(15, 4)), None);
        assert_eq!(dts_core(&head(4, 13)), None);
        assert_eq!(dts_core(&[0x7F, 0xFE, 0x80, 0x00, 0xFC, 0x3C, 0, 0, 0, 0]), None);
        assert_eq!(dts_core(&[0x7F, 0xFE, 0x80, 0x01, 0xFC]), None);
    }

    #[test]
    fn opus_packet_samples() {
        // CELT 20 ms, one frame.
        assert_eq!(opus_samples(&[31 << 3]), Some(960));
        // CELT 2.5 ms, one frame.
        assert_eq!(opus_samples(&[16 << 3]), Some(120));
        // SILK 60 ms, two frames is 120 ms: the most a packet holds.
        assert_eq!(opus_samples(&[(3 << 3) | 1]), Some(5760));
        // Hybrid 10 ms, code 3 with four frames.
        assert_eq!(opus_samples(&[(12 << 3) | 3, 4]), Some(1920));
        // Over 120 ms, no frames, nothing at all.
        assert_eq!(opus_samples(&[(3 << 3) | 3, 3]), None);
        assert_eq!(opus_samples(&[(31 << 3) | 3, 0]), None);
        assert_eq!(opus_samples(&[(31 << 3) | 3]), None);
        assert_eq!(opus_samples(&[]), None);
    }
}
