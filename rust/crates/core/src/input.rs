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
//! A path that is simply a file comes back unchanged, which is the case that
//! has to cost nothing.

use crate::udf;
use anyhow::{anyhow, bail, Context, Result};
use ffmpeg_next as ff;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// A DVD sector, and the unit its index addresses the stream in.
const SECTOR: u64 = 2048;

/// The most pieces a title set's stream is written in. The format numbers
/// them from one and stops at nine.
const MAX_VOB: usize = 9;

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
            return Ok(Input::plain(spec));
        }
        // A name that says which sectors it plays is a DVD title, and nothing
        // else in this program produces one. Asked after the file test, so
        // that a real file whose name ends that way is still a file.
        if let Some((base, first, last)) = split_at_sectors(spec) {
            return dvd_title(spec, base, first, last);
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
        Ok(Input {
            spec: spec.to_string(),
            url,
            file: image,
            range: Some(Range { at: range.at, len: range.len }),
            parts: Vec::new(),
        })
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
        let names: Vec<&PathBuf> =
            if self.parts.is_empty() { vec![&self.file] } else { self.parts.iter().collect() };
        let mut whole = 0u64;
        for name in names {
            let file =
                File::open(name).with_context(|| format!("cannot open {}", name.display()))?;
            let len = file.metadata()?.len();
            parts.push(Part { file, at: whole, len });
            whole += len;
        }
        let range = self.range.unwrap_or(Range { at: 0, len: whole });
        Ok(Reader { parts, range, pos: 0 })
    }

    /// How long the recording is in bytes.
    pub fn bytes(&self) -> Result<u64> {
        Ok(match self.range {
            Some(r) => r.len,
            None => std::fs::metadata(&self.file)?.len(),
        })
    }
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
        let Some(part) = self.parts.iter_mut().find(|p| at < p.at + p.len && at >= p.at) else {
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
/// The other two things said here are for program streams and for a
/// container that could not say how often pictures arrive; both are below.
pub fn demux(url: &str) -> Result<ff::format::context::Input> {
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
    let (genpts, all_maps) = (is_program_stream(&ictx), !states_frame_rate(&ictx));
    if genpts || all_maps {
        return open_with(url, genpts, all_maps);
    }
    Ok(ictx)
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

fn open(url: &str, generate_pts: bool) -> Result<ff::format::context::Input> {
    open_with(url, generate_pts, false)
}

fn open_with(
    url: &str,
    generate_pts: bool,
    all_maps: bool,
) -> Result<ff::format::context::Input> {
    let nested = url.starts_with("subfile,") || url.starts_with("concat:");
    if !nested && !generate_pts && !all_maps {
        return Ok(ff::format::input(&url)?);
    }
    let mut opts = ff::Dictionary::new();
    if all_maps {
        opts.set("scan_all_pmts", "1");
    }
    if nested {
        opts.set("protocol_whitelist", "file,subfile,concat");
    }
    if generate_pts {
        opts.set("fflags", "+genpts");
    }
    Ok(ff::format::input_with_dictionary(&url, opts)?)
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

/// Where a DVD title's bytes are, and how to hand them to a demuxer.
///
/// The name points at the first of the pieces the title set's stream is
/// written in; the rest are its numbered siblings, and the sectors are
/// counted across all of them together. So the pieces are found first and the
/// range is taken out of the whole.
fn dvd_title(spec: &str, base: &str, first: u64, last: u64) -> Result<Input> {
    let want = Range { at: first * SECTOR, len: (last + 1 - first) * SECTOR };
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
                range: Some(Range { at: at + range.at, len: range.len }),
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
            let joined: Vec<String> =
                parts.iter().map(|(p, _)| p.to_string_lossy().into_owned()).collect();
            let concat = format!("concat:{}", joined.join("|"));
            // A title that is the whole stream needs no window around the
            // join, and one protocol is worth more than two.
            let url = if range.at == 0 && range.len == whole {
                concat
            } else {
                format!("subfile,,start,{},end,{},,:{concat}", range.at, range.at + range.len)
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
    Some(Range { at: want.at, len: want.len.min(whole - want.at) })
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
            anyhow!("{name} is written in pieces on {}, which cannot be read in place", image.display())
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
    let name = first.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut out = Vec::new();
    for want in vob_names(&name) {
        let path = dir.join(&want);
        let path = if path.exists() { path } else { dir.join(want.to_ascii_lowercase()) };
        let Ok(meta) = std::fs::metadata(&path) else { break };
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
    let Some(at) = first.to_ascii_uppercase().rfind(".VOB") else { return Vec::new() };
    // The piece number is the character before the extension.
    let Some(number) = first[..at].chars().last() else { return Vec::new() };
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
        assert_eq!(vob_names("VIDEO_TS/VTS_12_1.VOB")[1], "VIDEO_TS/VTS_12_2.VOB");
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
        assert_eq!(input.range, Some(Range { at: SECTOR, len: SECTOR }));
        assert!(input.url.starts_with("subfile,,start,2048,end,4096,,:concat:"), "{}", input.url);
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
        File::create(&path).unwrap().write_all(b"AAAAhello worldZZZZ").unwrap();

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
