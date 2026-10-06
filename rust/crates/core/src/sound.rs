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
    //
    // The tracks being written, not the recordings' first ones: with that
    // one dropped, the framing compared was a track that is not in the file.
    let framing = |src: &Source, track: &AudioInfo| {
        (info.codec == "aac")
            .then(|| crate::aac::of_track(src, track.stream_index).map(|f| f.as_str()))
    };
    let shape_framing = framing(shape.src, &info);
    let unlike = pieces.iter().enumerate().filter(|&(n, _)| n != master).any(|(n, p)| {
        track_for(pieces, master, n, opts).is_ok_and(|t| {
            t.codec != info.codec
                || t.sample_rate != info.sample_rate
                || t.channels != info.channels
                || framing(p.src, t) != shape_framing
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
    //
    // A range that opens at the head is not seeked for at all: the file was
    // just opened and is standing at its first byte. Any seek lands on a
    // picture, and a broadcast's sound starts before its first one does --
    // half a second of it on an off-air recording, which a whole-recording
    // .wav had as silence and an .aac did not have at all.
    let landing = (range.0 - 0.5).max(0.0);
    if landing > 0.0 {
        let target = ((landing + src.start_time) * f64::from(ff::ffi::AV_TIME_BASE)) as i64;
        ictx.seek(target, ..target)?;
    }
    // After the seek, for the reason [`crate::input::keep_only`] gives, and
    // with the pictures, which say when the range is over where the track
    // has nothing in it. See [`crate::input::keep_with_pictures`].
    crate::input::keep_with_pictures(&mut ictx, &[track.stream_index]);

    let in_tb = track.time_base;
    let mut lengths = Lengths::new(ictx.stream(track.stream_index).map(|s| s.parameters()));
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
        let dur = lengths.of(&packet, in_tb, dur);
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
/// fifth.
struct Lengths {
    par: Option<ff::codec::Parameters>,
    /// libavcodec's Vorbis parser, which keeps the size of the block before:
    /// fed every packet in order from wherever the read began.
    vorbis: *mut ff::ffi::AVVorbisParseContext,
}

impl Lengths {
    fn new(par: Option<ff::codec::Parameters>) -> Self {
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

    fn of(&mut self, packet: &ff::Packet, tb: f64, said: f64) -> f64 {
        let Some(par) = self.par.as_ref() else { return said };
        let (rate, id, fixed) = unsafe {
            let p = par.as_ptr() as *mut ff::ffi::AVCodecParameters;
            let size = i32::try_from(packet.size()).unwrap_or(0);
            ((*p).sample_rate, par.id(), ff::ffi::av_get_audio_frame_duration2(p, size))
        };
        if rate <= 0 || tb * f64::from(rate) <= 1.0 {
            return said;
        }
        let data = packet.data().unwrap_or(&[]);
        // Opus counts its frames at 48 kHz whatever rate the track states.
        let (samples, rate) = match id {
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
        if samples <= 0 {
            return said;
        }
        let exact = samples as f64 / f64::from(rate);
        if (exact - said).abs() < tb {
            exact
        } else {
            said
        }
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
    use super::{flac_block, opus_samples};

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
