//! What a BDAV disc holds, read from the disc's own index.
//!
//! BDAV is the recording half of Blu-ray: what a set-top recorder writes, and
//! what an authoring tool writes when it is asked for a disc of recordings
//! rather than a film. It is not BDMV. There are no menus, no `BDMV`
//! directory and no `index.bdmv`; there is one directory called `BDAV` and
//! three inside it.
//!
//! ```text
//! BDAV/
//!   info.bdav              which playlists there are, and in what order
//!   PLAYLIST/00001.rpls    one recording: which clip, from when to when
//!   CLIPINF/00001.clpi     that clip's own index
//!   STREAM/00001.m2ts      the transport stream itself
//! ```
//!
//! The stream is an ordinary MPEG-2 transport stream in 192 byte packets --
//! 188 of packet behind four bytes of arrival time -- which libavformat reads
//! without being told anything. So the streams were never the difficulty.
//! What was missing was the index: a directory of `00001.m2ts`, `00002.m2ts`,
//! `00003.m2ts` says nothing about which programme is which, and a disc of a
//! night's recordings is exactly the case where that matters.
//!
//! This reads the index. A playlist names its clip and the part of it to
//! play, and carries the programme's name and the time it was recorded in
//! ARIB's own text encoding ([`crate::arib`]); the marks beside it are the
//! chapter points the recorder set, which on a Japanese recording are
//! frequently the commercial breaks themselves.
//!
//! Both shapes a disc arrives in are read the same way, because the only
//! thing that differs is how a file is found:
//!
//!   * a **directory** -- a disc copied to a hard disk, or one mounted by the
//!     desktop, which is the same thing to a program reading it;
//!   * an **image** -- an `.iso`, read where it lies by [`crate::udf`],
//!     without mounting it and without unpacking a gigabyte to a temporary
//!     file.
//!
//! What comes back names each recording by a path. For a directory that is
//! the path of the stream; for an image it is the path of the stream *inside*
//! the image, written as though the image were a directory:
//! `/rec/Anime.iso/BDAV/STREAM/00001.m2ts`. One string, openable by
//! [`crate::input`], and the rest of the program is none the wiser.

use crate::arib;
use crate::udf;
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

/// Playlist and clip times are in 45 kHz ticks -- half the clock the packets
/// carry, which is how a 32 bit field reaches thirteen hours.
const TICK: f64 = 45_000.0;

/// One recording on the disc: what a playlist describes.
#[derive(Debug, Clone)]
pub struct Title {
    /// The playlist file this came from, `00001.rpls`.
    pub playlist: String,
    /// The programme's name, as the broadcaster wrote it.
    pub name: Option<String>,
    /// When the playlist says it was made, `2026-08-17 01:00`. A recorder
    /// writes the moment the recording started; an authoring tool writes the
    /// moment it wrote the disc.
    pub made: Option<String>,
    /// Playing time, which is the IN to OUT of the clips it names.
    pub duration: f64,
    pub clips: Vec<Clip>,
    /// The chapter points the recorder set, in seconds from the start of the
    /// title. Empty when the playlist carries none, or carries them in a
    /// shape this could not vouch for.
    pub marks: Vec<f64>,
}

impl Title {
    /// What to call this in a list. The name if there is one, and the file
    /// number when there is not -- never an empty row.
    pub fn label(&self) -> String {
        let stem = self.playlist.trim_end_matches(".rpls").trim_end_matches(".RPLS");
        match (&self.name, &self.made) {
            (Some(name), _) => name.clone(),
            (None, Some(made)) => format!("{stem} {made}"),
            (None, None) => stem.to_string(),
        }
    }
}

/// One clip a title plays, and the part of it the title plays.
#[derive(Debug, Clone)]
pub struct Clip {
    /// `00001`, the number the three files share.
    pub name: String,
    /// The path to open, which for an image is a path through it.
    pub path: String,
    /// Where the title starts and ends in the clip, in seconds on the clip's
    /// own timeline -- the one the demuxer reports, which does not begin at
    /// zero either.
    pub start: f64,
    pub end: f64,
}

/// Whether this is worth trying to read as a disc.
///
/// Cheap on purpose: it is asked of everything dropped on the window, and the
/// answer for a `.ts` file has to cost nothing.
pub fn looks_like_bdav(at: &Path) -> bool {
    if at.is_dir() {
        return bdav_dir(at).is_some();
    }
    at.extension().is_some_and(|e| e.eq_ignore_ascii_case("iso"))
}

/// The recordings a disc holds, in the order its own index lists them.
pub fn titles(at: &Path) -> Result<Vec<Title>> {
    let mut vol = Volume::open(at)?;
    let order = vol.playlist_order();
    let mut out = Vec::new();
    for name in order {
        let raw = match vol.read(&format!("PLAYLIST/{name}")) {
            Ok(raw) => raw,
            // A playlist the index names and the disc does not hold is a
            // damaged disc, not a reason to show none of the others.
            Err(_) => continue,
        };
        match playlist(&raw, &name, &mut vol) {
            Ok(title) => out.push(title),
            Err(_) => continue,
        }
    }
    if out.is_empty() {
        bail!("{}: no playable playlist on this disc", at.display());
    }
    Ok(out)
}

/// One row of a list of recordings: something that opens.
///
/// A title and a row are the same thing until a title plays more than one
/// clip, which is what a recorder writes when a programme ran past the length
/// it splits its streams at. Those become a row each, named `(1/3)`,
/// `(2/3)`, `(3/3)`. Joining them into one timeline is a different piece of
/// work -- each clip carries its own clock, and splicing two of them is not
/// the same operation as cutting one -- and until that exists, showing the
/// pieces is the honest thing: every second of the recording is reachable,
/// and the list says plainly that it is in pieces.
#[derive(Debug, Clone)]
pub struct Entry {
    /// What to open, which is what everything downstream is keyed on.
    pub path: String,
    /// What to call it in a list.
    pub label: String,
    pub duration: f64,
    /// Chapter marks, in seconds from the start of this clip.
    pub marks: Vec<f64>,
    /// Where that start is on the clip's own clock -- the playlist's IN
    /// point, in the same seconds the demuxer reports.
    ///
    /// A mark is only a number until something says which clock it is on. The
    /// stream is opened whole, and its times are rebased to the container's
    /// start, so a mark becomes a time in the recording as
    /// `start + mark - container start time`. Nothing on this side knows the
    /// last of those three, which is why the first is carried rather than
    /// folded in.
    pub start: f64,
    /// Where a cut of it belongs when no output folder has been chosen.
    ///
    /// Beside the disc rather than in it: a cut of `Anime.iso` is written in
    /// the folder the image is in, because inside the image there is nowhere
    /// to write, and inside a copied disc there is nowhere that belongs to
    /// anything but the disc.
    pub home: String,
    /// What to name that cut. The programme's own name, which is the whole
    /// reason for reading the index -- `00001.m2ts` is not a name anybody
    /// wants a file called.
    pub stem: String,
}

/// Everything on a disc that can be opened, in the order its index lists it.
pub fn entries(at: &Path) -> Result<Vec<Entry>> {
    let home = beside(at);
    let disc = at
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "BDAV".to_string());
    let mut out = Vec::new();
    for t in titles(at)? {
        let many = t.clips.len();
        let mut offset = 0.0;
        for (i, c) in t.clips.iter().enumerate() {
            let duration = c.end - c.start;
            let part =
                if many > 1 { format!(" ({}/{})", i + 1, many) } else { String::new() };
            let label = format!("{}{part}", t.label());
            let marks = marks_in(&t.marks, offset, duration);
            // A disc whose playlists carry no name still has to name its
            // files something, and the disc and the clip number are what
            // there is.
            let stem = match &t.name {
                Some(name) => filename(&format!("{name}{part}")),
                None => filename(&format!("{disc}_{}{part}", c.name)),
            };
            out.push(Entry {
                path: c.path.clone(),
                label,
                duration,
                marks,
                start: c.start,
                home: home.clone(),
                stem,
            });
            offset += duration;
        }
    }
    Ok(out)
}

/// The marks of a title that belong to one of its clips, on that clip's own
/// count.
///
/// A title's marks are seconds from where the title begins, and a title that
/// plays three clips is three rows: `offset` is where this clip starts in
/// that count. What comes back is seconds from the head of this clip's
/// material, which with [`Entry::start`] beside it is enough to say where a
/// mark is in the stream.
fn marks_in(marks: &[f64], offset: f64, duration: f64) -> Vec<f64> {
    marks
        .iter()
        .filter(|m| **m >= offset - 0.001 && **m < offset + duration)
        .map(|m| (m - offset).max(0.0))
        .collect()
}

/// The folder a disc is in, which is where a cut of it can be written.
fn beside(at: &Path) -> String {
    at.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(at)
        .to_string_lossy()
        .into_owned()
}

/// A programme name as a file name.
///
/// A broadcaster writes titles with the characters it likes, and some of them
/// are characters a filesystem will not take. They are turned into their full
/// width forms rather than dropped, which is what a Japanese recorder does
/// with the same problem: `?` becomes `？` and the name still reads.
fn filename(name: &str) -> String {
    /// What a name is cut to. The limit is on bytes rather than characters
    /// because that is what a filesystem counts, and a title in Japanese is
    /// three bytes a character.
    const LIMIT: usize = 180;
    let mut out = String::new();
    for c in name.chars() {
        let c = match c {
            '\\' => '＼',
            '/' => '／',
            ':' => '：',
            '*' => '＊',
            '?' => '？',
            '"' => '＂',
            '<' => '＜',
            '>' => '＞',
            '|' => '｜',
            c if (c as u32) < 0x20 => ' ',
            c => c,
        };
        if out.len() + c.len_utf8() > LIMIT {
            break;
        }
        out.push(c);
    }
    // Windows will not have a name that ends in a dot or a space, and no
    // filesystem is improved by one.
    let out = out.trim_matches([' ', '.', '\u{3000}']).to_string();
    if out.is_empty() {
        "recording".to_string()
    } else {
        out
    }
}

/// A BDAV volume, whichever of its two shapes it arrived in.
enum Volume {
    /// A directory, and the path of the `BDAV` directory inside it.
    Dir(PathBuf),
    /// An image, the path it was opened from, and the prefix the `BDAV`
    /// directory sits at inside it.
    Image { image: Box<udf::Image>, path: PathBuf, prefix: String },
}

impl Volume {
    fn open(at: &Path) -> Result<Volume> {
        if at.is_dir() {
            let dir = bdav_dir(at)
                .ok_or_else(|| anyhow!("{}: no BDAV directory here", at.display()))?;
            return Ok(Volume::Dir(dir));
        }
        let image = udf::Image::open(at)?;
        // Where the playlists are is where BDAV is, and the case a disc
        // spells its directories in is its own business.
        let prefix = image
            .files()
            .iter()
            .find_map(|e| {
                let upper = e.path.to_ascii_uppercase();
                upper.find("PLAYLIST/").map(|at| e.path[..at].to_string())
            })
            .ok_or_else(|| anyhow!("{}: no BDAV directory on this image", at.display()))?;
        Ok(Volume::Image { image: Box::new(image), path: at.to_path_buf(), prefix })
    }

    /// Read one of the small index files, named relative to `BDAV`.
    fn read(&mut self, rel: &str) -> Result<Vec<u8>> {
        match self {
            Volume::Dir(dir) => {
                let path = at_name(dir, rel);
                std::fs::read(&path).with_context(|| format!("cannot read {}", path.display()))
            }
            Volume::Image { image, prefix, .. } => {
                let want = format!("{prefix}{rel}");
                let entry = image
                    .find(&want)
                    .ok_or_else(|| anyhow!("{want} is not on this image"))?
                    .clone();
                image.read(&entry)
            }
        }
    }

    /// What to hand [`crate::input`] for one clip's stream.
    fn stream(&self, clip: &str) -> String {
        let rel = format!("STREAM/{clip}.m2ts");
        match self {
            Volume::Dir(dir) => at_name(dir, &rel).to_string_lossy().into_owned(),
            Volume::Image { path, prefix, .. } => {
                format!("{}/{prefix}{rel}", path.to_string_lossy())
            }
        }
    }

    /// The playlists to offer, in the order `info.bdav` puts them.
    ///
    /// The index is what says which order a recorder's own list would show,
    /// and it is also what says which playlists are real: a disc can hold a
    /// `.rpls` that its index does not name. When the index cannot be read
    /// the directory stands in for it, sorted, because a list in file order
    /// is better than no list.
    fn playlist_order(&mut self) -> Vec<String> {
        if let Ok(raw) = self.read("info.bdav") {
            let names = table_of_playlists(&raw);
            if !names.is_empty() {
                return names;
            }
        }
        let mut names = match self {
            Volume::Dir(dir) => std::fs::read_dir(at_name(dir, "PLAYLIST"))
                .map(|d| {
                    d.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .filter(|n| n.to_ascii_lowercase().ends_with(".rpls"))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            Volume::Image { image, prefix, .. } => {
                let want = format!("{prefix}PLAYLIST/").to_ascii_uppercase();
                image
                    .files()
                    .iter()
                    .filter(|e| e.path.to_ascii_uppercase().starts_with(&want))
                    .filter(|e| e.path.to_ascii_lowercase().ends_with(".rpls"))
                    .map(|e| e.name().to_string())
                    .collect()
            }
        };
        names.sort();
        names
    }
}

/// The `BDAV` directory at or under a path, whichever the user named.
///
/// Both are worth taking: a disc mounted at `/media/disc` is named by its
/// mount point, and a disc copied to a hard disk is as likely to be named by
/// the `BDAV` directory itself as by the folder holding it.
fn bdav_dir(at: &Path) -> Option<PathBuf> {
    let here = at.join("PLAYLIST");
    if here.is_dir() {
        return Some(at.to_path_buf());
    }
    for name in ["BDAV", "bdav"] {
        let dir = at.join(name);
        if dir.join("PLAYLIST").is_dir() || dir.join("playlist").is_dir() {
            return Some(dir);
        }
    }
    None
}

/// A file inside the `BDAV` directory, found whichever case the disc spells
/// its directories in.
fn at_name(dir: &Path, rel: &str) -> PathBuf {
    let direct = dir.join(rel);
    if direct.exists() {
        return direct;
    }
    let lower = dir.join(rel.to_lowercase());
    if lower.exists() {
        return lower;
    }
    // Ask for the name as given, so that what cannot be found is reported
    // under the name the disc ought to have used.
    direct
}

/// The playlist names `info.bdav` lists.
fn table_of_playlists(raw: &[u8]) -> Vec<String> {
    if raw.len() < 12 || &raw[..4] != b"BDAV" {
        return Vec::new();
    }
    let at = u32be(raw, 8) as usize;
    let Some(count) = raw.get(at + 4..at + 6).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 0..count.min(2000) {
        let name_at = at + 6 + i * 10;
        let Some(name) = raw.get(name_at..name_at + 10) else { break };
        if !name.iter().all(|b| b.is_ascii_graphic()) {
            break;
        }
        out.push(String::from_utf8_lossy(name).into_owned());
    }
    out
}

/// Read one `.rpls`.
fn playlist(raw: &[u8], file: &str, vol: &mut Volume) -> Result<Title> {
    if raw.len() < 48 || &raw[..4] != b"PLST" {
        bail!("{file} is not a playlist");
    }
    let list_at = u32be(raw, 8) as usize;
    let marks_at = u32be(raw, 12) as usize;

    let clips = play_items(raw, list_at, vol)?;
    if clips.is_empty() {
        bail!("{file} plays nothing");
    }
    let duration = clips.iter().map(|c| c.end - c.start).sum();
    let marks = play_marks(raw, marks_at, &clips);

    Ok(Title {
        playlist: file.to_string(),
        name: app_info_name(raw, list_at),
        made: app_info_made(raw),
        duration,
        clips,
        marks,
    })
}

/// The PlayItems: which clip, and the part of it to play.
fn play_items(raw: &[u8], at: usize, vol: &mut Volume) -> Result<Vec<Clip>> {
    let count = raw
        .get(at + 6..at + 8)
        .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
        .ok_or_else(|| anyhow!("the playlist ends where its items should start"))?;
    let mut out = Vec::new();
    let mut item = at + 10;
    for _ in 0..count.min(1000) {
        let Some(len) = raw.get(item..item + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
        else {
            break;
        };
        let Some(body) = raw.get(item + 2..item + 2 + len) else { break };
        if body.len() < 20 {
            break;
        }
        let name = String::from_utf8_lossy(&body[..5]).into_owned();
        let codec = &body[5..9];
        if codec != b"M2TS" {
            bail!("a clip in {name} is {}, which is not a stream this reads",
                  String::from_utf8_lossy(codec));
        }
        let start = u32be(body, 12) as f64 / TICK;
        let end = u32be(body, 16) as f64 / TICK;
        if end <= start {
            bail!("a clip in the playlist ends before it begins");
        }
        out.push(Clip { path: vol.stream(&name), name, start, end });
        item += 2 + len;
    }
    Ok(out)
}

/// The chapter marks, when they can be believed.
///
/// The entries are a fixed size that the section's own length gives away, but
/// where the timestamp sits inside one is not the same on every disc: BDMV's
/// mark is fourteen bytes with the time four in, and the marks a BDAV
/// recorder writes are longer and carry a name and a thumbnail beside the
/// time. So the offset is not assumed. Each candidate is tried, and the one
/// whose times all land inside the title is the one that is right -- and when
/// none of them does, the marks are left out rather than guessed at, because
/// a chapter point in the wrong place is worse than no chapter point.
fn play_marks(raw: &[u8], at: usize, clips: &[Clip]) -> Vec<f64> {
    let (Some(first), Some(last)) = (clips.first(), clips.last()) else { return Vec::new() };
    let Some(count) = raw.get(at + 4..at + 6).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
    else {
        return Vec::new();
    };
    if count == 0 || count > 10_000 {
        return Vec::new();
    }
    let len = u32be(raw, at) as usize;
    if len < 2 {
        return Vec::new();
    }
    let stride = (len - 2) / count;
    if stride < 8 {
        return Vec::new();
    }
    let body_at = at + 6;

    for time_at in [4usize, 6] {
        let mut times = Vec::with_capacity(count);
        let mut sound = true;
        for i in 0..count {
            let entry = body_at + i * stride;
            let Some(_) = raw.get(entry..entry + stride) else {
                sound = false;
                break;
            };
            let time = u32be(raw, entry + time_at) as f64 / TICK;
            // A mark belongs to a PlayItem and cannot fall outside it. That
            // is the whole of the check, and it is enough: a field read from
            // the wrong offset lands outside a ten minute window
            // immediately.
            if time < first.start - 0.001 || time > last.end + 0.001 {
                sound = false;
                break;
            }
            times.push(time - first.start);
        }
        if sound && !times.is_empty() {
            times.sort_by(f64::total_cmp);
            times.dedup_by(|a, b| (*a - *b).abs() < 0.001);
            return times;
        }
    }
    Vec::new()
}

/// The programme name, which sits after everything the playlist says about
/// itself and before the items it plays.
///
/// A length byte and then ARIB text. The length is checked against where the
/// items begin, so that a field this has misread cannot swallow the file.
fn app_info_name(raw: &[u8], list_at: usize) -> Option<String> {
    const NAME_LEN_AT: usize = 88;
    let len = *raw.get(NAME_LEN_AT)? as usize;
    let at = NAME_LEN_AT + 1;
    if len == 0 || at + len > list_at.min(raw.len()) {
        return None;
    }
    let text = arib::one_line(&arib::decode(&raw[at..at + len]));
    (!text.is_empty()).then_some(text)
}

/// When the playlist says it was made: seven bytes of binary coded decimal,
/// century first.
fn app_info_made(raw: &[u8]) -> Option<String> {
    const MADE_AT: usize = 50;
    let b = raw.get(MADE_AT..MADE_AT + 7)?;
    let d = |i: usize| -> Option<u32> {
        let (hi, lo) = (b[i] >> 4, b[i] & 0x0F);
        (hi <= 9 && lo <= 9).then_some(hi as u32 * 10 + lo as u32)
    };
    let year = d(0)? * 100 + d(1)?;
    let (month, day, hour, min) = (d(2)?, d(3)?, d(4)?, d(5)?);
    if !(1970..=2200).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || min > 59
    {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02} {hour:02}:{min:02}"))
}

fn u32be(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .map(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_index_of_a_disc() {
        let mut raw = vec![0u8; 356];
        raw[..8].copy_from_slice(b"BDAV0100");
        raw[8..12].copy_from_slice(&320u32.to_be_bytes());
        raw[320..324].copy_from_slice(&32u32.to_be_bytes());
        raw[324..326].copy_from_slice(&3u16.to_be_bytes());
        for (i, name) in ["00001.rpls", "00002.rpls", "00003.rpls"].iter().enumerate() {
            raw[326 + i * 10..336 + i * 10].copy_from_slice(name.as_bytes());
        }
        assert_eq!(table_of_playlists(&raw), ["00001.rpls", "00002.rpls", "00003.rpls"]);
    }

    #[test]
    fn a_programme_name_becomes_a_file_name() {
        assert_eq!(
            filename("#09「わたしのラッキーアイテム?」"),
            "#09「わたしのラッキーアイテム？」"
        );
        assert_eq!(filename("a/b:c*d"), "a／b：c＊d");
        assert_eq!(filename("  "), "recording");
        // Cut on a character boundary, never through one.
        let long = "あ".repeat(200);
        let cut = filename(&long);
        assert!(cut.len() <= 180 && cut.chars().all(|c| c == 'あ'));
    }

    #[test]
    fn a_date_that_is_not_one_is_left_out() {
        let mut raw = vec![0u8; 100];
        assert_eq!(app_info_made(&raw), None);
        raw[50..57].copy_from_slice(&[0x20, 0x26, 0x09, 0x04, 0x18, 0x33, 0x57]);
        assert_eq!(app_info_made(&raw).as_deref(), Some("2026-09-04 18:33"));
        // A month of 19 is a field read from the wrong place.
        raw[52] = 0x19;
        assert_eq!(app_info_made(&raw), None);
    }

    fn clip(start: f64, end: f64) -> Clip {
        Clip { name: "00001".into(), path: "/x/00001.m2ts".into(), start, end }
    }

    #[test]
    fn takes_the_marks_that_land_inside_the_title() {
        // Two marks, 46 bytes each, with the time six bytes in: what a
        // recorder writes.
        let count = 2usize;
        let stride = 46usize;
        let at = 0usize;
        let mut raw = vec![0u8; 6 + count * stride];
        raw[..4].copy_from_slice(&((2 + count * stride) as u32).to_be_bytes());
        raw[4..6].copy_from_slice(&(count as u16).to_be_bytes());
        let put = |raw: &mut Vec<u8>, i: usize, ticks: u32| {
            let entry = 6 + i * stride + 6;
            raw[entry..entry + 4].copy_from_slice(&ticks.to_be_bytes());
        };
        put(&mut raw, 0, 20_842);
        put(&mut raw, 1, 22_876_605);
        let marks = play_marks(&raw, at, &[clip(20_842.0 / TICK, 22_971_270.0 / TICK)]);
        assert_eq!(marks.len(), 2);
        assert!(marks[0].abs() < 0.001);
        assert!((marks[1] - 507.905).abs() < 0.01);
    }

    #[test]
    fn a_clip_takes_the_marks_that_fall_in_it() {
        // A title of two ten minute clips, marked every five.
        let marks = [0.0, 300.0, 600.0, 900.0];
        assert_eq!(marks_in(&marks, 0.0, 600.0), vec![0.0, 300.0]);
        assert_eq!(marks_in(&marks, 600.0, 600.0), vec![0.0, 300.0]);
        // The mark on the join belongs to the clip it opens, once.
        assert_eq!(marks_in(&marks, 0.0, 600.0).len() + marks_in(&marks, 600.0, 600.0).len(), 4);
    }

    #[test]
    fn leaves_out_marks_it_cannot_place() {
        let mut raw = vec![0u8; 6 + 2 * 46];
        raw[..4].copy_from_slice(&(2u32 + 2 * 46).to_be_bytes());
        raw[4..6].copy_from_slice(&2u16.to_be_bytes());
        // Times that fall nowhere near the title, at either candidate offset.
        for i in 0..2 {
            let entry = 6 + i * 46 + 6;
            raw[entry..entry + 4].copy_from_slice(&900_000_000u32.to_be_bytes());
        }
        assert!(play_marks(&raw, 0, &[clip(0.4, 510.0)]).is_empty());
    }
}
