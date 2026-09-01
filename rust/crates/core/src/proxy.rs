//! A small stand-in for the recording, decoded once when the file is opened.
//!
//! Scrubbing a broadcast recording is expensive for reasons that have nothing
//! to do with the picture wanted: a transport stream seeks by byte position,
//! an open GOP cannot be entered anywhere but its I picture, and 1440x1080
//! MPEG-2 costs what it costs. Every one of those is paid again on the next
//! frame the pointer lands on.
//!
//! So the whole thing is decoded once, straight through -- the cheapest way
//! to read it -- and written out small, square-pixel and short-GOP. What the
//! timeline asks about afterwards is answered from that file instead.
//!
//! Two properties make the substitution invisible to everything above:
//!
//!   * **The clock is the same.** Each proxy picture carries its source
//!     picture's presentation time, rebased exactly the way [`crate::scan`]
//!     rebases the source's. Proxy time *is* source time; there is no
//!     mapping to get wrong, and none to keep in step when a cut moves.
//!   * **The entry points are the same.** A keyframe is forced at every one
//!     of the source's access points and, `sc_threshold` permitting, nowhere
//!     else. So the proxy's own index lands on the same instants, and the
//!     thumbnail track built from it sits on the same key pictures as one
//!     built from the recording.
//!
//! What the proxy cannot answer is anything about the *bitstream*: which
//! pictures reference which, where a copy may begin, how long a GOP runs.
//! Planning and cutting therefore keep reading the recording itself. The
//! proxy is for looking at, not for cutting from.

use anyhow::{anyhow, bail, Context, Result};
use ffmpeg_next as ff;
use std::path::{Path, PathBuf};

use crate::{thumbs, Source};

/// Bumped when a change here would make an existing proxy wrong. It goes into
/// the cache key, so old files are simply never looked at again.
pub const VERSION: u32 = 2;

/// Encoders to try, in order, when none is named. Hardware first: the design
/// note in `docs/design.md` keeps x264 at arm's length because it is GPL, and
/// `mpeg4` is the fallback that is always there in any libavcodec build.
pub const ENCODERS: [&str; 6] =
    ["h264_nvenc", "h264_videotoolbox", "h264_amf", "h264_qsv", "libx264", "mpeg4"];

/// Default width. The picture on screen is what this has to hold up at: the
/// preview asks the engine for the stage's own pixels, so a proxy narrower
/// than the stage is a soft picture however well it is encoded, and 960 was
/// narrower than the stage on any screen with a device pixel ratio above 1 --
/// where a full-height stage asks for the whole 1920 and got 960 back.
///
/// 1280 rather than the stage's full width because the build has to be paid
/// for at the moment the file is opened, and past here the wait starts
/// growing faster than the picture improves. Measured on 2.8 minutes of
/// 1440x1080:
///
/// | width | build | size |
/// |---|---|---|
/// | 960 | 7.0s | 48MB |
/// | 1152 | 8.2s | 61MB |
/// | 1280 | 9.1s | 87MB |
///
/// `SMARTCUT_PROXY_WIDTH` is there for a machine that wants to trade the
/// other way -- down to 960 on a slow disk, up towards the 1920 cap on a
/// machine with the cores to spare.
pub const WIDTH: u32 = 1280;

/// The ceiling on the picture, whatever `SMARTCUT_PROXY_WIDTH` asks for.
///
/// Past this there is nothing left for the timeline to show: the stage asks
/// the engine for its own pixels and stops at 1920 (`stageWidth` in the GUI),
/// so a taller proxy is built and stored at a size that is scaled straight
/// back down before anyone looks at it.
///
/// It is a ceiling on the whole picture rather than on the width alone --
/// 4:3 material 1920 across would be 1440 lines tall -- so the width comes
/// down until the height fits inside it too.
pub const MAX_WIDTH: u32 = 1920;
pub const MAX_HEIGHT: u32 = 1080;

/// Default quality, in x264's CRF units -- see [`ProxyOptions::quality`].
pub const QUALITY: f64 = 22.0;

pub struct ProxyOptions {
    /// Width in square pixels. The height follows from the source's aspect
    /// ratio, so 1440x1080 broadcast material comes out 16:9.
    pub width: u32,
    /// How good the picture has to be, in x264's CRF units -- lower is
    /// better, and 18 to 24 is the useful span. Every encoder is given the
    /// nearest thing it has to a constant-quality mode and this number
    /// mapped onto its own scale, so the knob means the same thing whichever
    /// one takes the job.
    ///
    /// Constant quality rather than a bit rate, because the proxy is a
    /// scratch file on local disk: it is worth exactly what it looks like
    /// and nothing else. A bit rate would have to be guessed from the frame
    /// size and would then be wrong in both directions at once -- too little
    /// for the busy half of a recording, too much for the still half.
    pub quality: f64,
    /// A bit rate to hold to instead, for an encoder with no constant-quality
    /// mode at all. Nothing sets this now; it is the escape hatch.
    pub bit_rate: Option<usize>,
    /// Encoders to try, in order; the first that opens is used.
    pub encoders: Vec<String>,
    /// Longest run without a keyframe, in seconds. Only a backstop: the
    /// source's own access points are keyframes here whatever this says.
    pub max_gop: f64,
}

impl Default for ProxyOptions {
    fn default() -> Self {
        fn num<T: std::str::FromStr>(key: &str) -> Option<T> {
            std::env::var(key).ok().and_then(|v| v.trim().parse().ok())
        }
        let encoders = match std::env::var("SMARTCUT_PROXY_ENCODER") {
            Ok(v) if !v.trim().is_empty() => {
                v.split(',').map(|s| s.trim().to_string()).collect()
            }
            _ => ENCODERS.iter().map(|s| s.to_string()).collect(),
        };
        Self {
            width: num("SMARTCUT_PROXY_WIDTH").unwrap_or(WIDTH),
            quality: num("SMARTCUT_PROXY_QUALITY").unwrap_or(QUALITY),
            bit_rate: None,
            encoders,
            max_gop: 2.0,
        }
    }
}

/// What kind of picture each of the source's pictures was.
///
/// The proxy is re-encoded, so its own picture types describe the proxy and
/// nothing else -- and the timeline shows the letter beside the frame number.
/// Read here while the recording is being decoded, when it is free, and kept
/// beside the proxy so a cached one can still answer.
#[derive(Default, Clone)]
pub struct Marks {
    /// Presentation times, ascending.
    pub times: Vec<f64>,
    /// b'I', b'P', b'B' or b'-', one per time.
    pub kinds: Vec<u8>,
}

const MARKS_MAGIC: &[u8; 4] = b"SCPM";

impl Marks {
    pub fn len(&self) -> usize {
        self.times.len()
    }

    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }

    fn push(&mut self, time: f64, kind: &str) {
        self.times.push(time);
        self.kinds.push(kind.as_bytes().first().copied().unwrap_or(b'-'));
    }

    /// The kind of the picture nearest `time`, if one is within `slack`.
    pub fn kind_at(&self, time: f64, slack: f64) -> Option<&'static str> {
        if self.times.is_empty() {
            return None;
        }
        let i = self.times.partition_point(|&t| t < time);
        let best = [i.wrapping_sub(1), i]
            .into_iter()
            .filter(|&j| j < self.times.len())
            .min_by(|&a, &b| {
                (self.times[a] - time).abs().total_cmp(&(self.times[b] - time).abs())
            })?;
        if (self.times[best] - time).abs() > slack {
            return None;
        }
        Some(match self.kinds[best] {
            b'I' => "I",
            b'P' => "P",
            b'B' => "B",
            _ => "-",
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut out = Vec::with_capacity(16 + self.times.len() * 9);
        out.extend_from_slice(MARKS_MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(self.times.len() as u64).to_le_bytes());
        for (t, k) in self.times.iter().zip(&self.kinds) {
            out.extend_from_slice(&t.to_le_bytes());
            out.push(*k);
        }
        std::fs::write(path, out).with_context(|| format!("cannot write {}", path.display()))?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Marks> {
        let raw = std::fs::read(path)?;
        if raw.len() < 16 || &raw[0..4] != MARKS_MAGIC {
            bail!("{} is not a picture-kind table", path.display());
        }
        let version = u32::from_le_bytes(raw[4..8].try_into()?);
        if version != VERSION {
            bail!("{} was written by another version", path.display());
        }
        let n = u64::from_le_bytes(raw[8..16].try_into()?) as usize;
        if raw.len() < 16 + n * 9 {
            bail!("{} is truncated", path.display());
        }
        let mut marks = Marks { times: Vec::with_capacity(n), kinds: Vec::with_capacity(n) };
        for i in 0..n {
            let at = 16 + i * 9;
            marks.times.push(f64::from_le_bytes(raw[at..at + 8].try_into()?));
            marks.kinds.push(raw[at + 8]);
        }
        Ok(marks)
    }
}

pub struct Built {
    pub path: String,
    /// Which encoder actually took it, for reporting.
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    pub pictures: usize,
    pub bytes: u64,
    pub seconds: f64,
    pub marks: Marks,
    /// The thumbnail track and scene index, which cost nothing extra: the
    /// pictures they are built from are being decoded here anyway.
    ///
    /// Holds every thumbnail unless a `share` callback took some as they were
    /// collected, in which case it holds only the ones taken since the last
    /// hand-over. The scene index is always whole -- it is made at the end,
    /// from every picture the pass saw.
    pub track: thumbs::Track,
}

/// Where the proxy for `src_path` belongs inside `dir`.
///
/// Keyed by the source's path, size and modification time, so a re-recorded
/// file under the same name gets a new proxy rather than the old one. The
/// width and the quality are in the key too: changing either changes what
/// the file is, and the old one must not be picked up in its place.
pub fn cache_path(dir: &Path, src_path: &str, opts: &ProxyOptions) -> Result<PathBuf> {
    let meta = std::fs::metadata(src_path)
        .with_context(|| format!("cannot stat {src_path}"))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    // FNV-1a, which is plenty for telling one opened file from another and
    // costs no dependency.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x1_0000_01b3);
        }
    };
    eat(src_path.as_bytes());
    eat(&meta.len().to_le_bytes());
    eat(&mtime.to_le_bytes());
    eat(&opts.width.to_le_bytes());
    eat(&opts.quality.to_bits().to_le_bytes());
    eat(&VERSION.to_le_bytes());

    let stem: String = Path::new(src_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .take(40)
        .collect();
    Ok(dir.join(format!("{stem}-{h:016x}.mp4")))
}

/// Where the picture-kind table for a proxy belongs.
pub fn marks_path(proxy: &Path) -> PathBuf {
    proxy.with_extension("marks")
}

/// Is there a complete proxy at `path` already?
///
/// Both files or neither: the marks are written first and the video is only
/// renamed into place once it is whole, so a half-built proxy never looks
/// like a finished one.
pub fn ready(path: &Path) -> bool {
    path.is_file() && marks_path(path).is_file()
}

/// Delete the least recently used proxies in `dir` until at most `keep`
/// remain and they take no more than `budget` bytes between them.
///
/// A count alone is the wrong limit. What a proxy costs depends on how long
/// the recording is and how good the picture was asked to be, and both of
/// those move: eight proxies is a few hundred megabytes of half-hour
/// programmes at one setting and eight gigabytes of feature films at
/// another. The byte budget is the limit that means the same thing whatever
/// is in the cache; the count is only there to stop it filling with
/// thousands of tiny ones.
///
/// The most recent is never deleted, however far over budget it is on its
/// own -- it is almost certainly the recording being edited right now.
pub fn prune(dir: &Path, keep: usize, budget: u64) -> Result<usize> {
    let mut found: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("mp4") {
            continue;
        }
        // A proxy still being built is not one of the finished ones, however
        // old it looks.
        if path.to_string_lossy().ends_with(".part.mp4") {
            continue;
        }
        let meta = path.metadata();
        let when = meta
            .as_ref()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::UNIX_EPOCH);
        let bytes = meta.map(|m| m.len()).unwrap_or(0);
        found.push((when, bytes, path));
    }
    found.sort_by_key(|(when, _, _)| std::cmp::Reverse(*when));

    let mut running = 0u64;
    let mut gone = 0;
    for (i, (_, bytes, path)) in found.into_iter().enumerate() {
        running = running.saturating_add(bytes);
        if i == 0 || (i < keep && running <= budget) {
            continue;
        }
        let _ = std::fs::remove_file(marks_path(&path));
        if std::fs::remove_file(&path).is_ok() {
            gone += 1;
        }
    }
    Ok(gone)
}

/// Open a proxy, with its clock put back on the recording's.
///
/// The pictures were written carrying the recording's own timestamps, but a
/// container has opinions about where a timeline starts: MP4 states a start
/// time of its own, may write an edit list, and may quietly shift everything
/// so that the first sample lands on zero. Each of those is undone somewhere
/// on the way back in, and the ones that are not would leave every proxy
/// picture a few hundredths of a second out -- a third of a frame, which is
/// enough to show the wrong picture and never look wrong doing it.
///
/// So the offset is not deduced from the container at all. The recording's
/// time for the proxy's first picture is written down when the proxy is
/// built, in the table beside it, and the whole timeline is shifted to put
/// that picture back where it belongs. What the container thought does not
/// come into it.
pub fn open(path: &str) -> Result<Source> {
    let first = Marks::load(&marks_path(Path::new(path)))
        .ok()
        .and_then(|m| m.times.first().copied());
    open_with(path, first)
}

/// As [`open`], for a caller that already holds the marks.
pub fn open_with(path: &str, first_picture: Option<f64>) -> Result<Source> {
    // MP4 carries its own sync-sample table, so the index is free; the walk
    // is only there for a proxy some other muxer produced.
    let mut src = crate::scan_with(path, &crate::index::ContainerIndex)
        .or_else(|_| crate::scan(path))?;
    let have = src.points.first().map(|p| p.time).unwrap_or(0.0);
    // Without the marks, the container's own start time is the best guess
    // left -- which is right for a muxer that stored the timestamps as given.
    let offset = match first_picture {
        Some(want) => want - have,
        None => src.start_time,
    };
    if offset.abs() > 1e-9 {
        for p in src.points.iter_mut() {
            p.time += offset;
            p.lead_start += offset;
        }
        // Decoding rebases by `start_time`, so moving the timeline forward
        // means moving that back by the same amount.
        src.start_time -= offset;
        // `duration` is read as "where the timeline ends", and the timeline
        // no longer starts at zero.
        src.duration += offset;
    }
    Ok(src)
}

/// Everything the muxing side needs, built once the first picture has said
/// what shape the pictures are.
struct Sink {
    octx: ff::format::context::Output,
    encoder: ff::encoder::video::Encoder,
    name: String,
    tb_enc: ff::Rational,
    tb_out: ff::Rational,
    width: u32,
    height: u32,
}

/// What the write side did, once it has run out of pictures.
struct Wrote {
    encoder: String,
    width: u32,
    height: u32,
    pictures: usize,
}

/// One decoded picture on its way to the write side: the picture itself, its
/// time on the recording's own tick, and whether the recording had an access
/// point there.
type Job = (ff::frame::Video, i64, bool);

/// Take the encoder's packets as they come and mux them.
fn drain(s: &mut Sink) -> Result<()> {
    let mut packet = ff::Packet::empty();
    while s.encoder.receive_packet(&mut packet).is_ok() {
        packet.set_stream(0);
        packet.set_position(-1);
        packet.rescale_ts(s.tb_enc, s.tb_out);
        packet.write_interleaved(&mut s.octx)?;
        packet = ff::Packet::empty();
    }
    Ok(())
}

/// Scale, encode and mux every picture handed over, until the sender hangs up.
///
/// This half runs on its own thread. The two halves are about the same size --
/// on a half hour of 1440x1080 the decode is 18s and the reduction to 720 wide
/// plus the encode another 19s -- and run one after the other on a single
/// thread they simply add up, with the other cores idle for want of anything
/// to do. Handed to each other through a bounded channel they overlap, and the
/// pass takes about as long as its longer half.
///
/// The sink is opened here rather than passed in: it needs a picture to know
/// what shape the pictures are, and the first one to arrive is that picture.
fn write_side(
    src: &Source,
    opts: &ProxyOptions,
    tb_in: ff::Rational,
    part: &Path,
    rx: std::sync::mpsc::Receiver<Job>,
) -> Result<Wrote> {
    let mut sink: Option<Sink> = None;
    let mut scaler: Option<ff::software::scaling::Context> = None;
    let mut pictures = 0usize;

    for (frame, ticks, entry) in rx {
        let s = match sink.as_mut() {
            Some(s) => s,
            None => {
                sink = Some(open_sink(src, &frame, opts, tb_in, part)?);
                sink.as_mut().unwrap()
            }
        };
        let sc = match scaler.as_mut() {
            Some(sc) => sc,
            None => {
                scaler = Some(ff::software::scaling::Context::get(
                    frame.format(),
                    frame.width(),
                    frame.height(),
                    ff::format::Pixel::YUV420P,
                    s.width,
                    s.height,
                    // Averaging is what takes the comb out of interlaced
                    // material: at this reduction a field pair blends rather
                    // than combs, and the proxy needs no deinterlacer of its
                    // own.
                    ff::software::scaling::Flags::AREA,
                )?);
                scaler.as_mut().unwrap()
            }
        };
        // A fresh picture every time: the encoder keeps a reference to what it
        // is handed, so scaling into the same buffer again would rewrite a
        // picture that has not been encoded yet.
        let mut scaled = ff::frame::Video::empty();
        sc.run(&frame, &mut scaled)?;
        scaled.set_pts(Some(rescale(ticks, tb_in, s.tb_enc)));
        scaled.set_kind(if entry { ff::picture::Type::I } else { ff::picture::Type::None });
        s.encoder.send_frame(&scaled)?;
        pictures += 1;
        drain(s)?;
    }

    let mut s = sink.ok_or_else(|| anyhow!("{} decoded to nothing", src.path))?;
    s.encoder.send_eof()?;
    drain(&mut s)?;
    s.octx.write_trailer()?;
    Ok(Wrote { encoder: s.name, width: s.width, height: s.height, pictures })
}

/// Decode the recording once, writing a small copy of it.
///
/// `stop` is asked between packets; answering true abandons the build and
/// leaves nothing behind. `progress` is fed the fraction done.
///
/// `share` is handed the thumbnails as they are collected, with how far into
/// the recording they speak for -- see [`thumbs::Collector::take_new`]. A
/// build over a half-hour recording runs for a minute or two, and until it
/// finishes there is nothing but the recording itself to answer the film
/// strip from; these are what the strip is waiting for, and they exist long
/// before the file does. **They are moved, not copied**: what
/// [`Built::track`] comes back holding is only the tail, and a caller that
/// takes them owns the rest.
pub fn build(
    src: &Source,
    out: &str,
    opts: &ProxyOptions,
    thumb_opts: &thumbs::ThumbOptions,
    mut progress: Option<Box<dyn FnMut(f64) + Send>>,
    mut share: Option<Box<dyn FnMut(thumbs::Batch) + Send>>,
    stop: Option<Box<dyn Fn() -> bool + Send>>,
) -> Result<Built> {
    crate::init()?;
    let began = std::time::Instant::now();
    let out_path = PathBuf::from(out);
    if let Some(dir) = out_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Built under another name and renamed at the end: a proxy that was
    // interrupted must not be picked up next time as if it were whole.
    let part = out_path.with_extension("part.mp4");

    let mut ictx = ff::format::input(&src.path)?;
    let idx = src.video.stream_index;
    let stream = ictx.stream(idx).ok_or_else(|| anyhow!("video stream vanished"))?;
    let tb_in = stream.time_base();
    let params = stream.parameters();
    let mut decoder = crate::video_decoder(params)?;

    let fd = src.video.frame_duration();
    // The source's clock, in its own ticks, so a picture's time survives the
    // trip without going through a float.
    let start_ticks = (src.start_time / f64::from(tb_in)).round() as i64;

    let mut collector = thumbs::Collector::new(src, thumb_opts);
    let mut marks = Marks::default();
    let mut next_point = 0usize;
    let mut told = -1.0f64;
    let mut shared = std::time::Instant::now();

    // Deep enough that neither half waits on the other for long, shallow
    // enough that the pictures in flight are a few megabytes rather than the
    // recording.
    let (tx, rx) = std::sync::mpsc::sync_channel::<Job>(8);
    let mut cancelled = false;

    let wrote = std::thread::scope(|scope| -> Result<Wrote> {
        let writer = scope.spawn(|| write_side(src, opts, tb_in, &part, rx));

        // Anything but `Ok(true)` means stop reading: the write side has hung
        // up, and its own error is the one worth reporting.
        let mut hand_over = |frame: ff::frame::Video,
                             tx: &std::sync::mpsc::SyncSender<Job>|
         -> Result<bool> {
            let Some(pts) = frame.pts() else { return Ok(true) };
            let ticks = pts - start_ticks;
            let t = ticks as f64 * f64::from(tb_in);
            // Pictures before the container's own start belong to no moment
            // the rest of the app can name.
            if ticks < 0 {
                return Ok(true);
            }
            marks.push(t, crate::preview::kind_of(&frame));

            // Is this one of the recording's access points? The proxy is given
            // a keyframe exactly there, and the thumbnail track is built from
            // exactly those pictures -- the same ones a pass over the recording
            // itself would have used.
            while next_point < src.points.len() && src.points[next_point].time < t - fd / 2.0 {
                next_point += 1;
            }
            let entry =
                src.points.get(next_point).is_some_and(|p| (p.time - t).abs() <= fd / 2.0);
            if entry {
                collector.feed(t, &frame)?;
                if let Some(f) = share.as_mut() {
                    if shared.elapsed() >= thumbs::SHARE_EVERY {
                        shared = std::time::Instant::now();
                        f(collector.take_new());
                    }
                }
            }

            if tx.send((frame, ticks, entry)).is_err() {
                return Ok(false);
            }
            if let Some(f) = progress.as_mut() {
                let done = (t / src.duration.max(1e-9)).clamp(0.0, 1.0);
                if done - told >= 0.005 {
                    told = done;
                    f(done);
                }
            }
            Ok(true)
        };

        'read: {
            for (stream, packet) in ictx.packets() {
                if stream.index() != idx {
                    continue;
                }
                if stop.as_ref().is_some_and(|s| s()) {
                    cancelled = true;
                    break 'read;
                }
                if decoder.send_packet(&packet).is_err() {
                    continue;
                }
                // A picture of its own each time round: the one just filled is
                // on its way to the other thread, and refilling it there would
                // be writing into a picture that is being encoded.
                loop {
                    let mut frame = ff::frame::Video::empty();
                    if decoder.receive_frame(&mut frame).is_err() {
                        break;
                    }
                    if !hand_over(frame, &tx)? {
                        break 'read;
                    }
                }
            }
            if !cancelled {
                let _ = decoder.send_eof();
                loop {
                    let mut frame = ff::frame::Video::empty();
                    if decoder.receive_frame(&mut frame).is_err() {
                        break;
                    }
                    if !hand_over(frame, &tx)? {
                        break 'read;
                    }
                }
            }
        }

        // Hanging up is how the write side is told there is no more; without
        // this the join below would wait for a sender that never goes away.
        drop(tx);
        match writer.join() {
            Ok(r) => r,
            Err(_) => Err(anyhow!("the proxy's write side panicked")),
        }
    });

    if cancelled {
        // Whatever the write side made of the few pictures it had is not a
        // proxy of anything, and the error it may have returned on the way out
        // is not worth reporting over the cancellation.
        let _ = std::fs::remove_file(&part);
        bail!("cancelled");
    }
    let wrote = match wrote {
        Ok(w) => w,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };

    if let Some(f) = progress.as_mut() {
        f(1.0);
    }
    // The marks go down first: `ready` asks for both, so the proxy only
    // counts as finished once the thing beside it is already there.
    marks.save(&marks_path(&out_path))?;
    // Windows will not rename onto an existing file, and one can be sitting
    // there: a proxy whose marks never got written is not `ready`, so it gets
    // built again over the top of itself.
    let _ = std::fs::remove_file(&out_path);
    std::fs::rename(&part, &out_path)
        .with_context(|| format!("cannot put the proxy at {}", out_path.display()))?;

    Ok(Built {
        path: out.to_string(),
        encoder: wrote.encoder,
        width: wrote.width,
        height: wrote.height,
        pictures: wrote.pictures,
        bytes: std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0),
        seconds: began.elapsed().as_secs_f64(),
        marks,
        track: collector.finish(),
    })
}

fn rescale(ticks: i64, from: ff::Rational, to: ff::Rational) -> i64 {
    if from == to {
        return ticks;
    }
    unsafe { ff::ffi::av_rescale_q(ticks, from.into(), to.into()) }
}

/// Open the proxy file and an encoder to fill it, once the shape of the
/// pictures is known.
fn open_sink(
    src: &Source,
    picture: &ff::frame::Video,
    opts: &ProxyOptions,
    tb_in: ff::Rational,
    part: &Path,
) -> Result<Sink> {
    // Square pixels: broadcast 1440x1080 is 16:9 on screen, and a proxy that
    // ignored that would hand the timeline a squashed picture.
    let sar = src.video.sample_aspect_ratio.max(0.01);
    let height_for = |w: u32| {
        (((w as f64 * picture.height() as f64) / (picture.width() as f64 * sar)).round() as u32)
            .max(90)
            & !1
    };
    // What the recording is worth in square pixels -- not its coded width, the
    // same reasoning as `preview::encode_jpeg`: 1440 samples across shown at
    // 16:9 need 1920 to keep all 1080 of their lines, and stopping at 1440
    // would throw a quarter of them away. Above that there are no more samples
    // to ask for; above [`MAX_WIDTH`] x [`MAX_HEIGHT`] there is no one to show
    // them to.
    let native = ((picture.width() as f64 * sar).round() as u32).max(160);
    let mut width = opts.width.min(native).clamp(160, MAX_WIDTH) & !1;
    let mut height = height_for(width);
    if height > MAX_HEIGHT {
        // Taller than 16:9, so the height is what binds: take the width back
        // down to the one whose picture fits.
        width = ((width as f64 * MAX_HEIGHT as f64 / height as f64).round() as u32).max(160) & !1;
        height = height_for(width).min(MAX_HEIGHT);
    }

    let fps = if src.video.frame_rate > 0.0 { src.video.frame_rate } else { 30.0 };
    let rate = ff::Rational::from(fps).reduce();
    let gop = (fps * opts.max_gop).round().clamp(1.0, 600.0) as u32;

    let mut octx = ff::format::output(&part)
        .with_context(|| format!("cannot write {}", part.display()))?;

    // MPEG-4 part 2 states its own time base in the bitstream as a 16-bit
    // `vop_time_increment_resolution`, so a transport stream's 90kHz tick is
    // more than it can say and its encoder refuses to open at all. Pictures
    // are therefore handed over on a tick the encoder can hold, and the
    // packets are put back on the recording's tick on the way out. 60kHz
    // divides the 90kHz one evenly for the timestamps broadcast material
    // actually uses, and is under a microsecond out for any it does not.
    let tb_enc = if tb_in.denominator() > 65535 || tb_in.denominator() <= 0 {
        ff::Rational::new(1, 60000)
    } else {
        tb_in
    };
    let settings =
        EncoderSettings { width, height, tb_enc, rate, gop, quality: opts.quality, bit_rate: opts.bit_rate };
    let mut chosen: Option<(ff::encoder::video::Encoder, String)> = None;
    let mut tried: Vec<String> = Vec::new();
    for name in &opts.encoders {
        let Some(codec) = ff::encoder::find_by_name(name) else {
            continue;
        };
        // Opening is not proof of working. A hardware encoder can be present
        // in the build, open on a machine with no hardware behind it, and
        // only then refuse the first picture -- by which point the file is
        // half written and there is no going back to the next candidate. So
        // one throwaway picture is put through first, and the encoder that
        // takes it is opened again for the real run.
        match open_encoder(codec, name, &settings).and_then(|mut e| {
            let mut blank = ff::frame::Video::new(ff::format::Pixel::YUV420P, width, height);
            blank.set_pts(Some(0));
            e.send_frame(&blank)?;
            Ok(())
        }) {
            Ok(()) => {}
            Err(e) => {
                tried.push(format!("{name} ({e})"));
                continue;
            }
        }
        chosen = Some((open_encoder(codec, name, &settings)?, name.clone()));
        break;
    }
    let (encoder, name) = chosen.ok_or_else(|| {
        anyhow!("no proxy encoder would take a picture -- tried {}", tried.join(", "))
    })?;

    {
        let mut ost = octx.add_stream(ff::encoder::find(ff::codec::Id::None))?;
        ost.set_parameters(&encoder);
        ost.set_time_base(tb_in);
        ost.set_avg_frame_rate(rate);
        unsafe {
            (*ost.parameters().as_mut_ptr()).codec_tag = 0;
        }
    }
    octx.write_header()?;
    let tb_out = octx.stream(0).ok_or_else(|| anyhow!("no output stream"))?.time_base();

    Ok(Sink { octx, encoder, name, tb_enc, tb_out, width, height })
}

/// What every candidate encoder is set up with, so that the one that is
/// probed and the one that does the work are the same encoder twice.
struct EncoderSettings {
    width: u32,
    height: u32,
    tb_enc: ff::Rational,
    rate: ff::Rational,
    gop: u32,
    /// In x264's CRF units; [`quality_for`] puts it on each encoder's scale.
    quality: f64,
    /// Set only for an encoder that has no constant-quality mode.
    bit_rate: Option<usize>,
}

/// x264 is told only not to place keyframes of its own; the forced ones must
/// stay IDRs. An open GOP would compress a little better, but MP4 marks a
/// sample as a sync sample only when it is an IDR, and the proxy's whole
/// arrangement rests on its keyframes being findable in that table: the
/// thumbnail track built from a cached proxy reads nothing else.
const X264_PARAMS: &str = "scenecut=0";

/// One quality number, on the scale the named encoder actually has.
///
/// x264's CRF is the scale it is quoted in, because it is the one with a
/// published meaning. The MPEG family's qscale runs 1..31 over roughly the
/// same useful span in a quarter of the steps, and NVENC's and QSV's
/// constant quantiser sit close enough to CRF to pass it straight through.
fn quality_for(name: &str, crf: f64) -> f64 {
    match name {
        "mpeg4" | "mpeg2video" | "mjpeg" => (crf / 4.0).clamp(1.0, 31.0),
        _ => crf.clamp(0.0, 51.0),
    }
}

fn open_encoder(
    codec: ff::Codec,
    name: &str,
    s: &EncoderSettings,
) -> Result<ff::encoder::video::Encoder> {
    let mut enc = ff::codec::context::Context::new_with_codec(codec).encoder().video()?;
    enc.set_width(s.width);
    enc.set_height(s.height);
    enc.set_format(ff::format::Pixel::YUV420P);
    // The recording's own tick where the encoder will take it, so a
    // picture's timestamp can be carried over rather than recomputed.
    enc.set_time_base(s.tb_enc);
    enc.set_frame_rate(Some(s.rate));
    enc.set_gop(s.gop);
    // No reordering. Nothing here needs the compression, and decode order
    // that matches display order is one less thing between the pointer and
    // the picture.
    enc.set_max_b_frames(0);
    enc.set_aspect_ratio(ff::Rational::new(1, 1));

    let q = quality_for(name, s.quality);
    let mut eopts = ff::Dictionary::new();
    let mut qscale = false;
    match name {
        // Left to itself an encoder puts keyframes where *it* sees a cut, and
        // the proxy's entry points would then no longer be the recording's.
        // The forced ones are enough.
        "mpeg4" | "mpeg2video" => {
            eopts.set("sc_threshold", "1000000000");
            qscale = true;
        }
        "libx264" => {
            // `ultrafast` rather than `veryfast`. What the slower presets buy
            // is bits, by looking harder for the motion between two pictures
            // -- and looking is most of what an encode costs. A proxy is a
            // scratch file that will be deleted before the week is out, so
            // the trade runs the other way here than it does for a delivery
            // encode: spend the disk, keep the minute.
            eopts.set("preset", "ultrafast");
            eopts.set("crf", &format!("{q:.1}"));
            eopts.set("x264-params", X264_PARAMS);
        }
        // Constant quantiser, for the same reason x264 gets CRF: what is
        // wanted is a picture that holds up everywhere, not a file of a
        // particular size.
        "h264_nvenc" => {
            eopts.set("preset", "p1");
            eopts.set("tune", "ull");
            eopts.set("rc", "constqp");
            eopts.set("qp", &format!("{}", q.round() as i32));
        }
        "h264_qsv" => {
            eopts.set("preset", "veryfast");
            eopts.set("global_quality", &format!("{}", q.round() as i32));
        }
        "h264_amf" => {
            eopts.set("quality", "speed");
            eopts.set("rc", "cqp");
            let qp = format!("{}", q.round() as i32);
            eopts.set("qp_i", &qp);
            eopts.set("qp_p", &qp);
        }
        "h264_videotoolbox" => {
            eopts.set("realtime", "1");
            qscale = true;
        }
        _ => qscale = true,
    }
    if qscale {
        // The generic constant-quality lever: libavcodec reads
        // `global_quality` as a quantiser when this flag is up, and the
        // MPEG-family encoders are driven entirely by it.
        unsafe {
            (*enc.as_mut_ptr()).flags |= ff::ffi::AV_CODEC_FLAG_QSCALE as i32;
            (*enc.as_mut_ptr()).global_quality = (ff::ffi::FF_QP2LAMBDA as f64 * q) as i32;
        }
    }
    // Zero unless a bit rate was actually asked for. libavcodec does not
    // start a context at zero -- the `b` option's own default is 200kbps --
    // and a bit rate sitting there beside a quality is how x264 ends up in
    // average-bit-rate mode with the CRF quietly ignored. Which of the two
    // wins is a matter of the order the wrapper happens to read them in, and
    // that is not a thing to leave the picture resting on.
    enc.set_bit_rate(s.bit_rate.unwrap_or(0));

    enc.open_as_with(codec, eopts).map_err(|e| anyhow!("{e}"))
}
