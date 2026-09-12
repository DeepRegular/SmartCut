//! What a recording is named, and how that name is opened.
//!
//! Everything in this program is keyed on one string: the list holds it, the
//! seek index and the proxy are cached against it, the output is named beside
//! it, and the demuxer is handed it. That was true when every recording was a
//! file, and it is worth keeping true now that one can be a clip inside a
//! disc image.
//!
//! So a recording inside an image is named as though the image were a
//! directory:
//!
//! ```text
//! /rec/Anime.iso/BDAV/STREAM/00001.m2ts
//! ```
//!
//! Nothing in that path is invented -- the image really does hold a `BDAV`
//! directory with that file in it -- and the one thing that is unusual about
//! it, that `/rec/Anime.iso` is a file rather than a directory, is exactly
//! what this module notices. Splitting there gives three answers at once:
//!
//!   * the **URL** to open, which for a clip inside an image is libavformat's
//!     `subfile` protocol pointed at the bytes the clip occupies -- the
//!     demuxer reads them as a stream and never learns there is a filesystem
//!     around them;
//!   * the **file** to ask the operating system about, which is the image, so
//!     that a cache keyed on size and modification time still has something
//!     to weigh;
//!   * the **range** those bytes occupy, for the passes that read the
//!     transport stream themselves rather than through libavformat.
//!
//! A DVD needs one thing more. Its video is a single program stream that the
//! format made it write in pieces of no more than a gigabyte, and a title is
//! a run of sectors somewhere across those pieces -- so a name has to say
//! which run, and opening it has to put the pieces back together:
//!
//! ```text
//! /rec/Denshi.iso/VIDEO_TS/VTS_01_1.VOB@0-2082359
//! ```
//!
//! The sectors are the ones the disc's own index counts in, from the start of
//! the title set's stream and not from the start of any one file. Inside an
//! image the pieces are laid down end to end, so that is still one byte range
//! and one `subfile`. On a folder they are nine separate files, and
//! libavformat's `concat` protocol joins them -- with `subfile` around it to
//! take the title out of the join.
//!
//! A clip on a disc a recorder wrote needs the same thing for a different
//! reason. Its arrival clock can restart part-way through the file -- the
//! recorder wrote several sequences into one clip, and the playlist plays
//! them as several items -- so what a row in the list offers is one of those
//! sequences and not the file around it:
//!
//! ```text
//! /rec/Recording.iso/BDAV/STREAM/00001.m2ts@8960-4605951
//! ```
//!
//! Counted in source packets, which is the unit the disc's own index counts
//! its clips in, and handed to the demuxer as those bytes alone -- so what it
//! reads is one clock from beginning to end. See [`crate::disc`].
//!
//! That name is still what a playlist names when it plays part of such a
//! clip. What it usually names is the whole of one, and a whole one is
//! offered under the plain name it always had:
//!
//! ```text
//! /rec/Recording.iso/BDAV/STREAM/00001.m2ts
//! ```
//!
//! -- with the clocks inside it put back together as it is read. The name
//! says nothing about that because the disc's own index already does: see
//! [`crate::restamp`], and [`crate::disc::clip_restamp`] for where the
//! correction is worked out. What the demuxer is handed then is not a path
//! at all but a `restamp:` URL carrying the table, because a demuxer in this
//! program is opened from a string and from nothing else.
//!
//! A path that is simply a file comes back unchanged, which is the case that
//! has to cost nothing.

use crate::restamp::Restamp;
use crate::udf;
use anyhow::{anyhow, bail, Context, Result};
use ffmpeg_next as ff;
use std::ffi::{c_int, c_void, CString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::ptr;

/// A DVD sector, and the unit its index addresses the stream in.
const SECTOR: u64 = 2048;

/// A source packet: a transport packet behind the four bytes that say when
/// it arrived, and the unit a Blu-ray's index counts a clip in.
const SOURCE_PACKET: u64 = 192;

/// The most pieces a title set's stream is written in. The format numbers
/// them from one and stops at nine.
const MAX_VOB: usize = 9;

/// What a URL that needs its clocks put back together begins with, and what
/// separates the table from the URL underneath it. Neither is a protocol
/// libavformat knows: this module writes them and this module reads them.
const RESTAMP: &str = "restamp:";
const UNDER: char = '|';

/// A stretch of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub at: u64,
    pub len: u64,
}

#[derive(Debug, Clone)]
pub struct Input {
    /// The name the user, the list and every cache know it by.
    pub spec: String,
    /// What libavformat is given.
    pub url: String,
    /// The file on disk that holds the bytes: the recording itself, or the
    /// image it is inside.
    pub file: PathBuf,
    /// Which bytes of that file, when it is not all of them -- or of the
    /// files in [`Input::parts`] laid end to end, when there are some.
    pub range: Option<Range>,
    /// The files the bytes are spread over, in order, when a recording is
    /// written in more than one -- which on this side means a DVD read out of
    /// a folder rather than an image. Empty for everything else, which is
    /// every case where [`Input::file`] is the whole answer.
    pub parts: Vec<PathBuf>,
}

impl Input {
    /// Work out how to open what this names.
    ///
    /// A name that is neither a file nor a path into an image is handed on as
    /// it stands: libavformat opens more than files, and a URL it understands
    /// and this does not is not this module's business to reject.
    pub fn parse(spec: &str) -> Result<Input> {
        let path = Path::new(spec);
        if path.is_file() {
            return Ok(joined(Input::plain(spec)));
        }
        // A name that says which sectors it plays is a DVD title, and nothing
        // else in this program produces one. Asked after the file test, so
        // that a real file whose name ends that way is still a file.
        if let Some((base, first, last)) = split_at_sectors(spec) {
            return dvd_title(spec, base, first, last);
        }
        // And a name that says which source packets it plays is one sequence
        // of a clip. Whatever the clip is named -- a file, or a file inside
        // an image -- is answered by parsing that name and taking the
        // stretch out of whatever comes back.
        if let Some((base, first, last)) = clip_window(spec) {
            return clip_sequence(spec, base, first, last);
        }
        let Some((image, inside)) = split_at_image(path) else {
            return Ok(Input::plain(spec));
        };

        let img = udf::Image::open(&image)?;
        let entry = img
            .find(&inside)
            .ok_or_else(|| anyhow!("{inside} is not on {}", image.display()))?
            .clone();
        let range = entry.contiguous().ok_or_else(|| {
            anyhow!(
                "{inside} is written in pieces on {}, which cannot be read in place",
                image.display()
            )
        })?;
        // A file inside an image is a range of the image, and this is the
        // protocol that says so. The inner name is given as `file:` because
        // the option list ends at the first colon: a Windows path would
        // otherwise be read as the protocol `c`.
        let url = format!(
            "subfile,,start,{},end,{},,:file:{}",
            range.at,
            range.at + range.len,
            image.to_string_lossy()
        );
        Ok(joined(Input {
            spec: spec.to_string(),
            url,
            file: image,
            range: Some(Range {
                at: range.at,
                len: range.len,
            }),
            parts: Vec::new(),
        }))
    }

    /// A file, whole, under the name it is written down as. What every
    /// ordinary recording is, and what a file this program has just written
    /// is when it comes to be read back.
    pub fn plain(spec: &str) -> Input {
        Input {
            spec: spec.to_string(),
            url: spec.to_string(),
            file: PathBuf::from(spec),
            range: None,
            parts: Vec::new(),
        }
    }

    /// Whether the bytes are inside something larger.
    pub fn nested(&self) -> bool {
        self.range.is_some()
    }

    /// Open the bytes for reading directly, as the passes over the transport
    /// stream's own tables do.
    ///
    /// What comes back is positioned and bounded: offset zero is the start of
    /// the recording whether or not there is an image around it, and reading
    /// past the end of it stops, rather than running on into the next clip.
    pub fn open(&self) -> Result<Reader> {
        let mut parts = Vec::new();
        let names: Vec<&PathBuf> = if self.parts.is_empty() {
            vec![&self.file]
        } else {
            self.parts.iter().collect()
        };
        let mut whole = 0u64;
        for name in names {
            let file =
                File::open(name).with_context(|| format!("cannot open {}", name.display()))?;
            let len = file.metadata()?.len();
            parts.push(Part {
                file,
                at: whole,
                len,
            });
            whole += len;
        }
        let range = self.range.unwrap_or(Range { at: 0, len: whole });
        Ok(Reader {
            parts,
            range,
            pos: 0,
        })
    }

    /// How long the recording is in bytes.
    pub fn bytes(&self) -> Result<u64> {
        Ok(match self.range {
            Some(r) => r.len,
            None => std::fs::metadata(&self.file)?.len(),
        })
    }
}

/// Put the clocks of a recorder's clip back together, where it is one of
/// those and the disc's index says how.
///
/// Everything else comes back exactly as it went in, which is every
/// recording that is not a clip on a disc a recorder wrote, and every clip
/// on one whose stream runs on a single clock. The question costs a look at
/// two directory names for a file, and one small index file for a clip that
/// really is on such a disc; see [`crate::disc::clip_restamp`].
fn joined(mut input: Input) -> Input {
    if let Some(table) = crate::disc::clip_restamp(&input.spec) {
        input.url = format!("{RESTAMP}{}{UNDER}{}", table.encode(), input.url);
    }
    input
}

/// One of the files a recording is written in, and where it falls in them.
struct Part {
    file: File,
    /// Byte offset of this file's first byte within all of them laid end to
    /// end.
    at: u64,
    len: u64,
}

/// A window onto a recording, counted from the start of the window.
///
/// The recording is usually one file, and is nine of them for a DVD title
/// read out of a folder. Both are one run of bytes here: the window is
/// measured across the files laid end to end, and a read that reaches the
/// end of one carries on into the next.
pub struct Reader {
    parts: Vec<Part>,
    range: Range,
    pos: u64,
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.range.len.saturating_sub(self.pos);
        if left == 0 {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(left) as usize;
        let at = self.range.at + self.pos;
        let Some(part) = self
            .parts
            .iter_mut()
            .find(|p| at < p.at + p.len && at >= p.at)
        else {
            return Ok(0);
        };
        // A read stops at the end of the file it started in; the caller comes
        // back for the rest, and the next call finds the next file.
        let want = want.min((part.at + part.len - at) as usize);
        part.file.seek(SeekFrom::Start(at - part.at))?;
        let n = part.file.read(&mut buf[..want])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Reader {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::End(n) => self.range.len as i64 + n,
            SeekFrom::Current(n) => self.pos as i64 + n,
        };
        if pos < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek before the start of the recording",
            ));
        }
        self.pos = pos as u64;
        Ok(self.pos)
    }
}

/// Split a path at the file in the middle of it, if there is one.
///
/// Returns the image and the path inside it, forward-slashed the way the
/// image itself spells it.
fn split_at_image(path: &Path) -> Option<(PathBuf, String)> {
    let mut inside: Vec<String> = Vec::new();
    let mut here = path;
    while let Some(parent) = here.parent() {
        inside.push(here.file_name()?.to_string_lossy().into_owned());
        // An empty parent is the end of a relative path, and the root of an
        // absolute one is never a file.
        if parent.as_os_str().is_empty() {
            return None;
        }
        if parent.is_file() {
            inside.reverse();
            return Some((parent.to_path_buf(), inside.join("/")));
        }
        here = parent;
    }
    None
}

/// Open a demuxer on a URL this module produced.
///
/// One protocol wrapping another is refused by libavformat unless it is told
/// otherwise: whatever is allowed at the top level, the protocol *inside* one
/// is checked against a whitelist that holds `file` and nothing else. A DVD
/// title in a folder is `subfile` around `concat`, so it needs saying.
///
/// Said only for the URLs this module writes, and never for a plain path:
/// setting the list at all would narrow what libavformat opens, and a
/// recording named by a URL of somebody else's is none of this module's
/// business -- the same rule [`Input::parse`] follows.
///
/// The other three things said here are for program streams, for a container
/// that could not say how often pictures arrive, and for a recording whose
/// captions the head of the file does not mention; all three are below.
pub fn demux(url: &str) -> Result<Demux> {
    let ictx = open(url, false)?;
    // A program stream -- which on a DVD is every stream -- leaves the
    // presentation time off any picture whose place in the display order can
    // be worked out from the pictures around it, which on the disc this was
    // written against is one picture in four. libavformat will work them out,
    // and will only do it if it is asked before the file is opened, so a
    // program stream is opened again with the flag on.
    //
    // Asked of the format rather than of the name, and asked at all rather
    // than always set, because a transport stream times every picture it
    // carries and a flag that changes nothing is still a flag on the path
    // every recording goes down.
    //
    // The second reopening is for a recording whose frame rate the container
    // could not work out, and reads every program map to get it. Same
    // reasoning: it is asked for where it is needed rather than always. See
    // [`states_frame_rate`].
    //
    // The third is for a recording that began before the broadcaster
    // announced its captions; see [`captions_may_come_later`].
    let (genpts, all_maps) = (is_program_stream(&ictx), !states_frame_rate(&ictx));
    let deeper = captions_may_come_later(&ictx);
    if genpts || all_maps || deeper {
        return open_with(url, genpts, all_maps, deeper);
    }
    Ok(ictx)
}

/// How far into a recording to look for a stream the first program map did
/// not mention. libavformat's own limit is five megabytes.
///
/// A ceiling rather than a read: the analysis stops at five seconds of
/// stream whatever this says, which on a Japanese broadcast is about ten
/// megabytes. So a recording with nothing more to find pays for ten and not
/// for thirty-two -- three tenths of a second, measured, on top of an open
/// that took a quarter.
const DEEP_PROBE: &str = "32000000";

/// Whether this recording might be carrying captions libavformat has not
/// listed.
///
/// **A recorder starts before the programme does, and the captions are
/// announced when the programme starts.** The program map at the head of the
/// file names the video and the sound and nothing else; a few seconds in, it
/// is replaced by one that also names the caption stream. libavformat probes
/// the head of the file and stops after five megabytes -- about two and a
/// half seconds of a broadcast -- so it never sees the second map, lists no
/// subtitle stream, and the recording arrives here with its captions
/// invisible: nothing to draw over the preview, nothing for the commercial
/// detector's caption marks, and nothing named for the cut to carry.
///
/// Measured over 300 recordings, two from each of 32 channels: 279 carry
/// captions and **7 of them are announced late**, between 8.4 and 8.8
/// megabytes in -- just past where the probe stops. The other 20 carry none
/// at all, which is what this asks a second question of, and the answer it
/// costs a deeper probe to get.
///
/// So the question is asked of transport streams that came back with no
/// subtitles at all, which is the only case a deeper probe can change and
/// the only one that pays for it. A recording whose captions are already
/// listed -- nine in ten of them -- reads exactly what it read before.
fn captions_may_come_later(ictx: &ff::format::context::Input) -> bool {
    is_transport_stream(ictx)
        && ictx
            .streams()
            .all(|s| s.parameters().medium() != ff::media::Type::Subtitle)
}

/// Whether what is open is a transport stream: a broadcast recording, or a
/// clip off a Blu-ray.
fn is_transport_stream(ictx: &ff::format::context::Input) -> bool {
    ictx.format()
        .name()
        .split(',')
        .any(|n| n.trim() == "mpegts")
}

/// Did the container come back knowing how often pictures arrive?
///
/// libavformat works this out while it probes, and stops probing at the
/// first program map unless it is told otherwise. On a Blu-ray written in
/// VC-1 that is too early: the answer comes back unset, and a frame rate of
/// "not a number" makes nonsense of every duration derived from it. Reading
/// every map fixes it, but it also turns up the second program a Japanese
/// broadcast carries -- the phone-sized copy of the same material, on its own
/// pids -- which is a different recording than the one that was asked for.
/// So the thorough read is kept for the recordings that need it, which is
/// what this asks.
fn states_frame_rate(ictx: &ff::format::context::Input) -> bool {
    ictx.streams()
        .best(ff::media::Type::Video)
        .map(|s| s.avg_frame_rate())
        .is_some_and(|r| r.numerator() > 0 && r.denominator() > 0)
}

/// Whether what is open is an MPEG program stream: a `.vob`, a `.mpg`, or a
/// DVD title read out of either.
fn is_program_stream(ictx: &ff::format::context::Input) -> bool {
    ictx.format().name().split(',').any(|n| n.trim() == "mpeg")
}

fn open(url: &str, generate_pts: bool) -> Result<Demux> {
    open_with(url, generate_pts, false, false)
}

fn open_with(url: &str, generate_pts: bool, all_maps: bool, deep_probe: bool) -> Result<Demux> {
    if let Some((table, under)) = split_restamp(url) {
        return restamped(under, table, generate_pts, all_maps, deep_probe);
    }
    let nested = url.starts_with("subfile,") || url.starts_with("concat:");
    if !nested && !generate_pts && !all_maps && !deep_probe {
        return Ok(Demux::plain(ff::format::input(&url)?));
    }
    let mut opts = ff::Dictionary::new();
    if all_maps {
        opts.set("scan_all_pmts", "1");
    }
    if deep_probe {
        opts.set("probesize", DEEP_PROBE);
    }
    if nested {
        opts.set("protocol_whitelist", "file,subfile,concat");
    }
    if generate_pts {
        opts.set("fflags", "+genpts");
    }
    Ok(Demux::plain(ff::format::input_with_dictionary(&url, opts)?))
}

/// The seams in a recording, in seconds on the clock the demuxer will
/// report. Empty for everything that is one run of time, which is every
/// recording but a recorder's clip written in several stretches.
///
/// Read back out of the name rather than off the disc a second time: the
/// table travelled there when the recording was parsed, and this is the one
/// place downstream that needs it. See [`crate::restamp::Restamp::joins`].
pub fn joins(input: &Input) -> Vec<f64> {
    split_restamp(&input.url).map_or_else(Vec::new, |(table, _)| table.joins())
}

/// Split a `restamp:` URL into the table and the URL it wraps. `None` for
/// every other URL, which is nearly all of them.
fn split_restamp(url: &str) -> Option<(Restamp, &str)> {
    let rest = url.strip_prefix(RESTAMP)?;
    let (table, under) = rest.split_once(UNDER)?;
    Some((Restamp::decode(table)?, under))
}

/// An open demuxer, and whatever had to be built to open it.
///
/// A recording is nearly always opened straight off a URL, and then this is
/// libavformat's own context and nothing else. A recorder's clip whose
/// clocks are being put back together needs a reader of this program's own
/// in front of it -- see [`Bridge`] -- and that reader has to live exactly as
/// long as the demuxer reading through it. Holding both here is what says so.
///
/// It stands in for the context everywhere one was used before, because it
/// hands out the context on demand.
pub struct Demux {
    ictx: std::mem::ManuallyDrop<ff::format::context::Input>,
    /// The reader underneath, when there is one. libavformat is told the i/o
    /// is not its own -- `AVFMT_FLAG_CUSTOM_IO` -- so closing the context
    /// leaves this alone and it is freed here.
    io: Option<*mut ff::ffi::AVIOContext>,
}

// The context is `Send` and so is everything hung off it here: the reader is
// reached only through the context, which is only ever used from one thread
// at a time.
unsafe impl Send for Demux {}

impl Demux {
    fn plain(ictx: ff::format::context::Input) -> Demux {
        Demux {
            ictx: std::mem::ManuallyDrop::new(ictx),
            io: None,
        }
    }
}

impl Deref for Demux {
    type Target = ff::format::context::Input;
    fn deref(&self) -> &Self::Target {
        &self.ictx
    }
}

impl DerefMut for Demux {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ictx
    }
}

impl Drop for Demux {
    fn drop(&mut self) {
        // The context goes first: it may still read while it is being closed,
        // and what it would read through is freed below.
        unsafe { std::mem::ManuallyDrop::drop(&mut self.ictx) };
        let Some(pb) = self.io.take() else { return };
        unsafe {
            // The buffer is freed as it stands rather than as it was handed
            // over, because libavformat is free to have grown it.
            let bridge = (*pb).opaque as *mut Bridge;
            ff::ffi::av_freep(std::ptr::addr_of_mut!((*pb).buffer) as *mut c_void);
            let mut pb = pb;
            ff::ffi::avio_context_free(&mut pb);
            if !bridge.is_null() {
                drop(Box::from_raw(bridge));
            }
        }
    }
}

/// How much of the stream this program's own reader hands over at a time.
/// A multiple of a source packet, so that a read that starts on one ends on
/// one and the next begins where a packet does.
const BRIDGE_BUFFER: usize = crate::restamp::SOURCE_PACKET * 256;

/// The reader that sits between libavformat and the bytes, correcting the
/// times as they go past.
///
/// It reads through an ordinary libavformat i/o context of its own, so the
/// `subfile` and `concat` protocols keep doing what they do and this has no
/// opinion about where the bytes are. What it adds is alignment -- a
/// correction is applied to whole source packets, and libavformat asks for
/// whatever it likes -- and the correction itself.
struct Bridge {
    under: *mut ff::ffi::AVIOContext,
    table: Restamp,
    /// Whole packets, read and corrected before any of them is handed over.
    scratch: Vec<u8>,
}

/// libavformat's two flags for a seek that is not a seek: one asks how long
/// the stream is, the other insists on moving even where moving is dear.
const AVSEEK_SIZE: c_int = 0x10000;
const AVSEEK_FORCE: c_int = 0x20000;

/// Seek from where the reader stands, and seek from the start.
const SEEK_CUR: c_int = 1;
const SEEK_SET: c_int = 0;

impl Bridge {
    fn read(&mut self, out: *mut u8, size: c_int) -> c_int {
        let size = size.max(0) as usize;
        // Where the reader underneath stands. `avio_tell` is a header's
        // one-liner rather than a function to call, so it is written out.
        let at = unsafe { ff::ffi::avio_seek(self.under, 0, SEEK_CUR) };
        if at < 0 || size == 0 {
            return ff::ffi::AVERROR_EOF;
        }
        let packet = crate::restamp::SOURCE_PACKET;
        // Where the packet this read starts inside begins, and how much of
        // it has already gone by.
        let past = at as usize % packet;
        let want = (past + size).div_ceil(packet) * packet;
        if self.scratch.len() < want {
            self.scratch.resize(want, 0);
        }
        if past != 0 {
            unsafe { ff::ffi::avio_seek(self.under, at - past as i64, SEEK_SET) };
        }
        let mut got = 0usize;
        while got < want {
            let n = unsafe {
                ff::ffi::avio_read(
                    self.under,
                    self.scratch[got..].as_mut_ptr(),
                    (want - got) as c_int,
                )
            };
            if n <= 0 {
                break;
            }
            got += n as usize;
        }
        if got <= past {
            return ff::ffi::AVERROR_EOF;
        }
        self.table.apply(at as u64 - past as u64, &mut self.scratch[..got]);
        let mut n = (got - past).min(size);
        // Whole packets wherever there are any, so that the next read starts
        // where a packet does and nothing has to be read twice.
        if n >= packet {
            n -= n % packet;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(self.scratch[past..].as_ptr(), out, n);
            ff::ffi::avio_seek(self.under, at + n as i64, SEEK_SET);
        }
        n as c_int
    }

    fn seek(&mut self, to: i64, whence: c_int) -> i64 {
        if whence & AVSEEK_SIZE != 0 {
            return unsafe { ff::ffi::avio_size(self.under) };
        }
        unsafe { ff::ffi::avio_seek(self.under, to, whence & !AVSEEK_FORCE) }
    }
}

impl Drop for Bridge {
    /// The reader underneath is this one's to close, and closing it is what
    /// gives the file handle back.
    fn drop(&mut self) {
        unsafe { ff::ffi::avio_closep(&mut self.under) };
    }
}

unsafe extern "C" fn bridge_read(opaque: *mut c_void, out: *mut u8, size: c_int) -> c_int {
    (*(opaque as *mut Bridge)).read(out, size)
}

unsafe extern "C" fn bridge_seek(opaque: *mut c_void, to: i64, whence: c_int) -> i64 {
    (*(opaque as *mut Bridge)).seek(to, whence)
}

/// Open `under` with the clocks in it corrected as it is read.
///
/// The same three questions the ordinary path answers are answered here --
/// whether to invent presentation times, whether to read every program map,
/// how far to probe -- because the caller has already asked them of a first
/// open and is asking for a second.
fn restamped(
    under: &str,
    table: Restamp,
    generate_pts: bool,
    all_maps: bool,
    deep_probe: bool,
) -> Result<Demux> {
    let nested = under.starts_with("subfile,") || under.starts_with("concat:");
    unsafe {
        let mut below: *mut ff::ffi::AVIOContext = ptr::null_mut();
        let name = CString::new(under).context("that name cannot be opened")?;
        let mut opts: *mut ff::ffi::AVDictionary = ptr::null_mut();
        if nested {
            set(&mut opts, "protocol_whitelist", "file,subfile,concat");
        }
        let err = ff::ffi::avio_open2(
            &mut below,
            name.as_ptr(),
            ff::ffi::AVIO_FLAG_READ as c_int,
            ptr::null(),
            &mut opts,
        );
        ff::ffi::av_dict_free(&mut opts);
        if err < 0 {
            bail!("cannot open {under}: {}", ff::Error::from(err));
        }

        let buffer = ff::ffi::av_malloc(BRIDGE_BUFFER) as *mut u8;
        let bridge = Box::into_raw(Box::new(Bridge {
            under: below,
            table,
            scratch: Vec::new(),
        }));
        let pb = ff::ffi::avio_alloc_context(
            buffer,
            BRIDGE_BUFFER as c_int,
            0,
            bridge as *mut c_void,
            Some(bridge_read),
            None,
            Some(bridge_seek),
        );

        let mut ctx = ff::ffi::avformat_alloc_context();
        (*ctx).pb = pb;
        (*ctx).flags |= ff::ffi::AVFMT_FLAG_CUSTOM_IO as c_int;
        let mut opts: *mut ff::ffi::AVDictionary = ptr::null_mut();
        if all_maps {
            set(&mut opts, "scan_all_pmts", "1");
        }
        if deep_probe {
            set(&mut opts, "probesize", DEEP_PROBE);
        }
        if generate_pts {
            set(&mut opts, "fflags", "+genpts");
        }
        let err = ff::ffi::avformat_open_input(&mut ctx, ptr::null(), ptr::null_mut(), &mut opts);
        ff::ffi::av_dict_free(&mut opts);
        if err < 0 {
            // `avformat_open_input` has freed the context and left the i/o
            // alone, so what this module made is what this module takes back.
            ff::ffi::av_freep(std::ptr::addr_of_mut!((*pb).buffer) as *mut c_void);
            let mut pb = pb;
            ff::ffi::avio_context_free(&mut pb);
            drop(Box::from_raw(bridge));
            bail!("cannot open {under}: {}", ff::Error::from(err));
        }
        let demux = Demux {
            ictx: std::mem::ManuallyDrop::new(ff::format::context::Input::wrap(ctx)),
            io: Some(pb),
        };
        let err = ff::ffi::avformat_find_stream_info(ctx, ptr::null_mut());
        if err < 0 {
            bail!("cannot read {under}: {}", ff::Error::from(err));
        }
        Ok(demux)
    }
}

/// One option, said the way libavformat wants to hear it.
unsafe fn set(opts: *mut *mut ff::ffi::AVDictionary, key: &str, value: &str) {
    let (Ok(key), Ok(value)) = (CString::new(key), CString::new(value)) else {
        return;
    };
    ff::ffi::av_dict_set(opts, key.as_ptr(), value.as_ptr(), 0);
}

/// Split a DVD title's name into the stream it plays and the sectors of it.
///
/// `…/VTS_01_1.VOB@0-2082359` becomes the path and the pair. Anything else --
/// including a real file whose name happens to hold an `@` -- comes back
/// `None`, because the suffix only counts on a name that ends in a `.VOB`
/// with two sector numbers behind it.
fn split_at_sectors(spec: &str) -> Option<(&str, u64, u64)> {
    let (base, range) = spec.rsplit_once('@')?;
    if !base.to_ascii_uppercase().ends_with(".VOB") {
        return None;
    }
    let (first, last) = range.split_once('-')?;
    let (first, last) = (first.parse().ok()?, last.parse().ok()?);
    (first <= last).then_some((base, first, last))
}

/// The clip a name plays a stretch of, and which source packets those are.
///
/// `None` for every other name, the bare clip included: a clip whose clock
/// never restarts is played whole and named whole.
pub fn clip_window(spec: &str) -> Option<(&str, u64, u64)> {
    let (base, range) = spec.rsplit_once('@')?;
    if !base.to_ascii_uppercase().ends_with(".M2TS") {
        return None;
    }
    let (first, last) = range.split_once('-')?;
    let (first, last) = (first.parse().ok()?, last.parse().ok()?);
    (first <= last).then_some((base, first, last))
}

/// One sequence of a clip: the bytes its source packets occupy.
///
/// The clip is parsed first and the stretch taken out of the answer, so that
/// a clip inside an image and a clip beside one are the same case here --
/// the first arrives as a range of the image and the second as a whole file,
/// and a stretch of either is a range of one file.
fn clip_sequence(spec: &str, base: &str, first: u64, last: u64) -> Result<Input> {
    let clip = Input::parse(base)?;
    if !clip.parts.is_empty() {
        bail!("{spec}: a clip written in pieces cannot be played a sequence at a time");
    }
    let whole = match clip.range {
        Some(r) => r.len,
        None => std::fs::metadata(&clip.file)
            .with_context(|| format!("cannot measure {}", clip.file.display()))?
            .len(),
    };
    let at = clip.range.map_or(0, |r| r.at);
    let want = Range {
        at: first * SOURCE_PACKET,
        len: (last + 1 - first) * SOURCE_PACKET,
    };
    let range = clamp(want, whole)
        .ok_or_else(|| anyhow!("{spec}: those packets are not in this clip"))?;
    let url = format!(
        "subfile,,start,{},end,{},,:file:{}",
        at + range.at,
        at + range.at + range.len,
        clip.file.to_string_lossy()
    );
    Ok(Input {
        spec: spec.to_string(),
        url,
        file: clip.file,
        range: Some(Range {
            at: at + range.at,
            len: range.len,
        }),
        parts: Vec::new(),
    })
}

/// Where a DVD title's bytes are, and how to hand them to a demuxer.
///
/// The name points at the first of the pieces the title set's stream is
/// written in; the rest are its numbered siblings, and the sectors are
/// counted across all of them together. So the pieces are found first and the
/// range is taken out of the whole.
fn dvd_title(spec: &str, base: &str, first: u64, last: u64) -> Result<Input> {
    let want = Range {
        at: first * SECTOR,
        len: (last + 1 - first) * SECTOR,
    };
    let path = Path::new(base);
    match split_at_image(path) {
        // Inside an image the pieces are laid down end to end, so the title
        // is one range of the image and the demuxer never learns there was a
        // filesystem, let alone nine files.
        Some((image, inside)) => {
            let img = udf::Image::open(&image)?;
            let (at, whole) = vobs_in_image(&img, &inside, &image)?;
            let range = clamp(want, whole)
                .ok_or_else(|| anyhow!("{spec}: those sectors are not on this disc"))?;
            let url = format!(
                "subfile,,start,{},end,{},,:file:{}",
                at + range.at,
                at + range.at + range.len,
                image.to_string_lossy()
            );
            Ok(Input {
                spec: spec.to_string(),
                url,
                file: image,
                range: Some(Range {
                    at: at + range.at,
                    len: range.len,
                }),
                parts: Vec::new(),
            })
        }
        // On a folder they are nine files, and putting them back together is
        // libavformat's `concat`.
        None => {
            let parts = vobs_beside(path)?;
            let whole: u64 = parts.iter().map(|(_, len)| len).sum();
            let range = clamp(want, whole)
                .ok_or_else(|| anyhow!("{spec}: those sectors are not on this disc"))?;
            let joined: Vec<String> = parts
                .iter()
                .map(|(p, _)| p.to_string_lossy().into_owned())
                .collect();
            let concat = format!("concat:{}", joined.join("|"));
            // A title that is the whole stream needs no window around the
            // join, and one protocol is worth more than two.
            let url = if range.at == 0 && range.len == whole {
                concat
            } else {
                format!(
                    "subfile,,start,{},end,{},,:{concat}",
                    range.at,
                    range.at + range.len
                )
            };
            Ok(Input {
                spec: spec.to_string(),
                url,
                file: parts[0].0.clone(),
                range: Some(range),
                parts: parts.into_iter().map(|(p, _)| p).collect(),
            })
        }
    }
}

/// A range held to what is actually there.
///
/// The last cell of a title runs to the end of the last VOBU, and an index
/// that rounds that up past the end of the stream is a disc that was written
/// slightly wrong rather than one that cannot be played.
fn clamp(want: Range, whole: u64) -> Option<Range> {
    if want.at >= whole {
        return None;
    }
    Some(Range {
        at: want.at,
        len: want.len.min(whole - want.at),
    })
}

/// Where a title set's stream begins inside an image, and how long it is.
///
/// The pieces have to be one unbroken run for this to be a byte range at all.
/// The format makes them one -- a title set's stream is written in one go,
/// and the gigabyte boundary is a limit on file size and not on layout -- so
/// an image where they are not is one that was assembled by something that
/// took the files apart, and saying so is better than reading the pieces in
/// whatever order they happen to lie in.
fn vobs_in_image(img: &udf::Image, inside: &str, image: &Path) -> Result<(u64, u64)> {
    let mut at = 0u64;
    let mut whole = 0u64;
    for (n, name) in vob_names(inside).into_iter().enumerate() {
        let Some(entry) = img.find(&name) else { break };
        let run = entry.contiguous().ok_or_else(|| {
            anyhow!(
                "{name} is written in pieces on {}, which cannot be read in place",
                image.display()
            )
        })?;
        if n == 0 {
            at = run.at;
        } else if run.at != at + whole {
            bail!(
                "{name} does not follow the piece before it on {}",
                image.display()
            );
        }
        whole += run.len;
    }
    if whole == 0 {
        bail!("{inside} is not on {}", image.display());
    }
    Ok((at, whole))
}

/// The pieces of a title set's stream in a folder, in order, with their
/// lengths.
fn vobs_beside(first: &Path) -> Result<Vec<(PathBuf, u64)>> {
    let dir = first.parent().unwrap_or(Path::new("."));
    let name = first
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = Vec::new();
    for want in vob_names(&name) {
        let path = dir.join(&want);
        let path = if path.exists() {
            path
        } else {
            dir.join(want.to_ascii_lowercase())
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            break;
        };
        out.push((path, meta.len()));
    }
    if out.is_empty() {
        bail!("cannot open {}", first.display());
    }
    Ok(out)
}

/// `VTS_01_1.VOB` and the eight names that may follow it, keeping whatever
/// path was in front.
fn vob_names(first: &str) -> Vec<String> {
    let Some(at) = first.to_ascii_uppercase().rfind(".VOB") else {
        return Vec::new();
    };
    // The piece number is the character before the extension.
    let Some(number) = first[..at].chars().last() else {
        return Vec::new();
    };
    if !number.is_ascii_digit() {
        return Vec::new();
    }
    let stem = &first[..at - 1];
    let ext = &first[at..];
    (1..=MAX_VOB).map(|n| format!("{stem}{n}{ext}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn an_ordinary_path_is_left_alone() {
        let dir = std::env::temp_dir().join("smartcut-input-plain");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("rec.ts");
        let mut f = File::create(&path).unwrap();
        f.write_all(b"0123456789").unwrap();

        let spec = path.to_string_lossy().into_owned();
        let input = Input::parse(&spec).unwrap();
        assert_eq!(input.url, spec);
        assert!(!input.nested());
        assert_eq!(input.bytes().unwrap(), 10);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn one_sequence_of_a_clip_names_the_packets_it_plays() {
        assert_eq!(
            clip_window("/rec/R.iso/BDAV/STREAM/00001.m2ts@8960-4605951"),
            Some(("/rec/R.iso/BDAV/STREAM/00001.m2ts", 8960, 4_605_951))
        );
        // A clip named whole, a title of a DVD, and a recording whose own
        // name holds an at-sign are none of them this.
        assert_eq!(clip_window("/rec/R.iso/BDAV/STREAM/00001.m2ts"), None);
        assert_eq!(clip_window("/rec/D.iso/VIDEO_TS/VTS_01_1.VOB@0-2267"), None);
        assert_eq!(clip_window("/rec/2026-08-17@22.ts"), None);
        // Backwards, and not a pair of numbers at all.
        assert_eq!(clip_window("/x/00001.m2ts@900-100"), None);
        assert_eq!(clip_window("/x/00001.m2ts@first-last"), None);
    }

    #[test]
    fn a_sequence_is_read_as_the_bytes_it_occupies() {
        let dir = std::env::temp_dir().join("smartcut-input-sequence");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("00001.m2ts");
        let mut f = File::create(&path).unwrap();
        f.write_all(&vec![0u8; 10 * SOURCE_PACKET as usize]).unwrap();

        let input = Input::parse(&format!("{}@2-5", path.to_string_lossy())).unwrap();
        assert!(input.nested());
        let range = input.range.unwrap();
        assert_eq!(range.at, 2 * SOURCE_PACKET);
        assert_eq!(range.len, 4 * SOURCE_PACKET);
        // A stretch that runs off the end is cut to what is there rather
        // than asked for past it.
        let input = Input::parse(&format!("{}@8-99", path.to_string_lossy())).unwrap();
        assert_eq!(input.range.unwrap().len, 2 * SOURCE_PACKET);
        // And one that begins past the end is refused.
        assert!(Input::parse(&format!("{}@20-30", path.to_string_lossy())).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_dvd_title_names_the_sectors_it_plays() {
        assert_eq!(
            split_at_sectors("/rec/Denshi.iso/VIDEO_TS/VTS_01_1.VOB@0-2082359"),
            Some(("/rec/Denshi.iso/VIDEO_TS/VTS_01_1.VOB", 0, 2082359))
        );
        // A recording whose own name holds an at-sign is not one of these.
        assert_eq!(split_at_sectors("/rec/2026-08-17@22.ts"), None);
        assert_eq!(split_at_sectors("/rec/VTS_01_1.VOB@nine-ten"), None);
        // Backwards is not a range.
        assert_eq!(split_at_sectors("/rec/VTS_01_1.VOB@9-8"), None);
    }

    #[test]
    fn a_title_sets_stream_is_nine_files_at_the_most() {
        let names = vob_names("VTS_01_1.VOB");
        assert_eq!(names.len(), 9);
        assert_eq!(names[0], "VTS_01_1.VOB");
        assert_eq!(names[8], "VTS_01_9.VOB");
        // The path in front is kept, and the piece number is the one digit
        // that changes.
        assert_eq!(
            vob_names("VIDEO_TS/VTS_12_1.VOB")[1],
            "VIDEO_TS/VTS_12_2.VOB"
        );
        assert!(vob_names("recording.ts").is_empty());
    }

    #[test]
    fn a_dvd_title_in_a_folder_is_the_pieces_joined_and_a_window_on_them() {
        let dir = std::env::temp_dir().join("smartcut-input-dvd");
        let _ = std::fs::create_dir_all(&dir);
        // Two pieces of one sector each, so that a title of the second sector
        // is a range that begins inside the second file.
        for (n, fill) in [(1u8, b'A'), (2, b'B')] {
            let mut f = File::create(dir.join(format!("VTS_01_{n}.VOB"))).unwrap();
            f.write_all(&vec![fill; SECTOR as usize]).unwrap();
        }
        let first = dir.join("VTS_01_1.VOB");
        let spec = format!("{}@1-1", first.to_string_lossy());
        let input = Input::parse(&spec).unwrap();

        assert_eq!(input.parts.len(), 2);
        assert_eq!(
            input.range,
            Some(Range {
                at: SECTOR,
                len: SECTOR
            })
        );
        assert!(
            input
                .url
                .starts_with("subfile,,start,2048,end,4096,,:concat:"),
            "{}",
            input.url
        );
        assert_eq!(input.bytes().unwrap(), SECTOR);

        // And the window reads out of the second file, which is the whole
        // point of joining them.
        let mut buf = vec![0u8; SECTOR as usize + 1];
        let n = input.open().unwrap().read(&mut buf).unwrap();
        assert_eq!(&buf[..n], &vec![b'B'; n][..]);

        // A title that is the whole stream needs no window around the join.
        let spec = format!("{}@0-1", first.to_string_lossy());
        assert!(Input::parse(&spec).unwrap().url.starts_with("concat:"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_that_is_nothing_yet_is_handed_on() {
        // libavformat opens more than files; this is not the place to say no.
        let input = Input::parse("http://example.invalid/a.ts").unwrap();
        assert_eq!(input.url, "http://example.invalid/a.ts");
    }

    #[test]
    fn a_window_reads_and_seeks_inside_its_own_bounds() {
        let dir = std::env::temp_dir().join("smartcut-input-window");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("image.bin");
        File::create(&path)
            .unwrap()
            .write_all(b"AAAAhello worldZZZZ")
            .unwrap();

        let input = Input {
            spec: "x".into(),
            url: "x".into(),
            file: path.clone(),
            range: Some(Range { at: 4, len: 11 }),
            parts: Vec::new(),
        };
        let mut r = input.open().unwrap();
        let mut buf = [0u8; 32];
        let n = r.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello world");
        // The window ends where it ends: the Zs are another clip's.
        assert_eq!(r.read(&mut buf).unwrap(), 0);
        r.seek(SeekFrom::Start(6)).unwrap();
        let n = r.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"world");
        let _ = std::fs::remove_file(&path);
    }
}
