//! What a Blu-ray holds, read from the disc's own index.
//!
//! There are two halves to Blu-ray and this reads both.
//!
//! **BDAV** is the recording half: what a set-top recorder writes, and what
//! an authoring tool writes when it is asked for a disc of recordings rather
//! than a film. One directory called `BDAV` and three inside it.
//!
//! ```text
//! BDAV/
//!   info.bdav              which playlists there are, and in what order
//!   PLAYLIST/00001.rpls    one recording: which clip, from when to when
//!   CLIPINF/00001.clpi     that clip's own index
//!   STREAM/00001.m2ts      the transport stream itself
//! ```
//!
//! **BDMV** is the film half: a pressed disc, or a copy of one. The shape is
//! the same and the names are not, and there is a great deal more of it --
//! menus, a Java application, a second copy of the index under `BACKUP` --
//! none of which is a recording.
//!
//! ```text
//! BDMV/
//!   index.bdmv             the titles, for a player's own menu
//!   PLAYLIST/00009.mpls    one way through the disc: which clips, in order
//!   CLIPINF/00014.clpi     that clip's own index
//!   STREAM/00014.m2ts      the transport stream itself
//!   META/DL/bdmt_eng.xml   what the disc is called
//!   BACKUP/                all of the above again
//! ```
//!
//! The two dialects differ in four places -- the directory's name, the
//! playlist's extension and magic, where the list of playlists comes from,
//! and whether a playlist carries a programme name -- and agree everywhere
//! else, including the byte layout of a play item and of a chapter mark. So
//! they are read by one reader that is told which dialect it is looking at,
//! rather than by two readers that would be the same reader twice.
//!
//! The stream is an ordinary MPEG-2 transport stream in 192 byte packets --
//! 188 of packet behind four bytes of arrival time -- which libavformat reads
//! without being told anything. So the streams were never the difficulty.
//! What was missing was the index: a directory of `00014.m2ts`,
//! `00015.m2ts`, `00016.m2ts` says nothing about which of them is an episode
//! and which is the eight second logo that plays before the menu.
//!
//! Both shapes a disc arrives in are read the same way, because the only
//! thing that differs is how a file is found:
//!
//!   * a **directory** -- a disc copied to a hard disk, or one mounted by the
//!     desktop, which is the same thing to a program reading it;
//!   * an **image** -- an `.iso`, read where it lies by [`crate::udf`],
//!     without mounting it and without unpacking thirty gigabytes to a
//!     temporary file.
//!
//! What comes back names each recording by a path. For a directory that is
//! the path of the stream; for an image it is the path of the stream *inside*
//! the image, written as though the image were a directory:
//! `/rec/Anime.iso/BDAV/STREAM/00001.m2ts`. One string, openable by
//! [`crate::input`], and the rest of the program is none the wiser.
//!
//! **Encrypted discs are not handled and will not be.** AACS is a decryption
//! problem and this program has none of it. A pressed disc copied by a tool
//! that removed the encryption -- which is what the `MAKEMKV/` directory
//! beside `BDMV/` on some images is a sign of -- reads like any other.

use crate::arib;
use crate::udf;
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

/// Playlist and clip times are in 45 kHz ticks -- half the clock the packets
/// carry, which is how a 32 bit field reaches thirteen hours.
const TICK: f64 = 45_000.0;

/// How long a clip has to be before it is offered already ticked.
///
/// A BDMV disc is mostly not the film: the twelve episodes on the disc this
/// was written against sit among fifty other clips that are logos, warnings,
/// menu loops and eight second transitions, and ticking all sixty-two would
/// make the chooser a list to undo rather than a list to choose from. Five
/// minutes is well under the shortest thing anybody keeps a disc for and well
/// over the longest thing a menu is made of.
const WORTH_TICKING: f64 = 300.0;

/// Which half of the Blu-ray specification wrote this disc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A disc of recordings. See the module documentation.
    Bdav,
    /// A film, or a copy of one.
    Bdmv,
}

impl Shape {
    /// The directory the disc keeps everything in.
    fn root(self) -> &'static str {
        match self {
            Shape::Bdav => "BDAV",
            Shape::Bdmv => "BDMV",
        }
    }

    /// What a playlist file is called here.
    fn ext(self) -> &'static str {
        match self {
            Shape::Bdav => ".rpls",
            Shape::Bdmv => ".mpls",
        }
    }

    /// The four bytes a playlist opens with.
    fn magic(self) -> &'static [u8] {
        match self {
            Shape::Bdav => b"PLST",
            Shape::Bdmv => b"MPLS",
        }
    }

    /// What to call it to the user, and to the window.
    pub fn as_str(self) -> &'static str {
        match self {
            Shape::Bdav => "bdav",
            Shape::Bdmv => "bdmv",
        }
    }
}

/// One recording on the disc: what a playlist describes.
#[derive(Debug, Clone)]
pub struct Title {
    /// The playlist file this came from, `00001.rpls` or `00009.mpls`.
    pub playlist: String,
    /// The programme's name, as the broadcaster wrote it. BDMV playlists
    /// carry no name -- a film's titles live in the menu, which is a Java
    /// application -- so this is `None` on a pressed disc.
    pub name: Option<String>,
    /// When the playlist says it was made, `2026-08-17 01:00`. A recorder
    /// writes the moment the recording started; an authoring tool writes the
    /// moment it wrote the disc. `None` on BDMV, which does not record it.
    pub made: Option<String>,
    /// Playing time, which is the IN to OUT of the clips it names.
    pub duration: f64,
    pub clips: Vec<Clip>,
}

impl Title {
    /// What to call this in a list. The name if there is one, and the file
    /// number when there is not -- never an empty row.
    pub fn label(&self) -> String {
        let stem = self
            .playlist
            .trim_end_matches(".rpls")
            .trim_end_matches(".RPLS")
            .trim_end_matches(".mpls")
            .trim_end_matches(".MPLS");
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
    /// The chapter points the disc set inside this clip, in seconds from
    /// [`Clip::start`]. Empty when the playlist carries none, or carries them
    /// in a shape this could not vouch for.
    ///
    /// Per clip and not per title, because that is where a mark actually is:
    /// a playlist that runs twelve episodes together holds fifty-three marks
    /// and each of them is a time on its own episode's clock, not a time in
    /// the three hours the playlist plays.
    pub marks: Vec<f64>,
}

/// One elementary stream a clip carries, as the disc's own index describes
/// it.
///
/// Read from `CLIPINF` rather than from the stream, because the point of it
/// is to be answerable before anything is opened: a chooser that had to
/// demux thirty gigabytes to draw itself would not be a chooser anybody
/// waited for. What the demuxer says later is the authority at cutting time;
/// this is what the disc says it wrote.
#[derive(Debug, Clone)]
pub struct Track {
    /// `"video"`, `"audio"`, `"subtitle"`, `"menu"` or `"other"`.
    pub kind: &'static str,
    /// The PID it sits on, which is the one name for it that survives
    /// everything: the demuxer reports it, the cut writes it back, and a
    /// stream index does not exist until something is open.
    pub pid: i32,
    /// The codec and shape, `H.264 1080p 23.976fps` or `TrueHD 5.1 48kHz`.
    pub detail: String,
    /// The language the disc declared, when it declared one.
    pub language: Option<String>,
    /// Whether a cut can take it with it.
    ///
    /// The graphics streams a Blu-ray menu is made of cannot go on a cut
    /// timeline -- a presentation graphics stream is its own little display
    /// list, not a run of timed packets -- so they are listed to say they are
    /// being left behind, not to offer a choice about them.
    pub carried: bool,
}

/// One row of a list of recordings: something that opens.
///
/// A title and a row are the same thing until a title plays more than one
/// clip, which is what a recorder writes when a programme ran past the length
/// it splits its streams at, and what an authoring tool writes for a "play
/// all" that runs twelve episodes together. Those become a row each. Joining
/// them into one timeline is a different piece of work -- each clip carries
/// its own clock, and splicing two of them is not the same operation as
/// cutting one -- and until that exists, showing the pieces is the honest
/// thing: every second of the disc is reachable, and the list says plainly
/// that it is in pieces.
///
/// A clip that several playlists play is one row and not several. On a
/// pressed disc that is the difference between sixty-two rows and three
/// hundred and twenty-eight: an episode is named by its own playlist, again
/// by the "play all", and again by the one the menu's chapter list points at,
/// and all three would open the same stream at the same instant.
#[derive(Debug, Clone)]
pub struct Entry {
    /// What to open, which is what everything downstream is keyed on.
    pub path: String,
    /// The clip's number on the disc, `00014`.
    pub clip: String,
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
    /// What to name that cut. The programme's own name where the disc knows
    /// one -- which is the whole reason for reading the index, since
    /// `00014.m2ts` is not a name anybody wants a file called.
    pub stem: String,
    /// How many bytes of disc the stream occupies. Shown in the chooser: on
    /// a disc whose index names everything `000NN`, size and length are what
    /// tell an episode from a logo.
    pub bytes: u64,
    /// What the disc says the clip carries. Empty when the clip has no
    /// `CLIPINF` beside it, or one this could not read -- in which case
    /// nothing is claimed rather than something guessed.
    pub tracks: Vec<Track>,
    /// Whether to offer it already ticked. See [`WORTH_TICKING`].
    pub wanted: bool,
}

/// A disc, as far as its own index describes it.
#[derive(Debug, Clone)]
pub struct Disc {
    /// Which half of the specification wrote it.
    pub shape: Shape,
    /// What the disc calls itself: the name in `META` on a pressed disc, and
    /// the image or folder's own name where there is no such thing.
    pub label: String,
    /// Everything on it that can be opened, in the order its index lists it.
    pub entries: Vec<Entry>,
}

/// Whether this is worth trying to read as a disc.
///
/// Cheap on purpose: it is asked of everything dropped on the window, and the
/// answer for a `.ts` file has to cost nothing.
pub fn looks_like_disc(at: &Path) -> bool {
    if at.is_dir() {
        return disc_dir(at).is_some();
    }
    at.extension().is_some_and(|e| e.eq_ignore_ascii_case("iso"))
}

/// The recordings a disc holds, in the order its own index lists them.
pub fn titles(at: &Path) -> Result<(Shape, Vec<Title>)> {
    let mut vol = Volume::open(at)?;
    let shape = vol.shape();
    let order = vol.playlists();
    let mut out = Vec::new();
    for name in order {
        let raw = match vol.read(&format!("PLAYLIST/{name}")) {
            Ok(raw) => raw,
            // A playlist the index names and the disc does not hold is a
            // damaged disc, not a reason to show none of the others.
            Err(_) => continue,
        };
        match playlist(&raw, &name, shape, &mut vol) {
            Ok(title) => out.push(title),
            Err(_) => continue,
        }
    }
    if out.is_empty() {
        bail!("{}: no playable playlist on this disc", at.display());
    }
    Ok((shape, out))
}

/// Everything on a disc that can be opened, once and not once per playlist.
pub fn read(at: &Path) -> Result<Disc> {
    let mut vol = Volume::open(at)?;
    let shape = vol.shape();
    let label = vol.label(at);
    let home = beside(at);

    let mut entries: Vec<Entry> = Vec::new();
    // What has already been offered, so that the "play all" playlist and the
    // twelve that play one episode each come to twelve rows rather than
    // twenty-four. Keyed on the part of the clip a playlist plays and not on
    // the clip alone: a recorder can write two programmes into one stream and
    // two playlists that each name half of it, and those are two recordings.
    //
    // Each key remembers the row it made and how many clips its playlist
    // played, because the marks are worth taking from the shortest playlist
    // that offers them. A disc names an episode twice -- once in a playlist
    // of its own and once inside the "play all" -- and only the first of
    // those has chapter points this can be sure it has placed correctly.
    let mut seen: Vec<((String, i64, i64), usize, usize)> = Vec::new();

    for name in vol.playlists() {
        let Ok(raw) = vol.read(&format!("PLAYLIST/{name}")) else { continue };
        let Ok(title) = playlist(&raw, &name, shape, &mut vol) else { continue };
        let many = title.clips.len();
        for (i, c) in title.clips.iter().enumerate() {
            let key = (c.name.clone(), ticks(c.start), ticks(c.end));
            if let Some((_, row, from)) = seen.iter_mut().find(|(k, _, _)| *k == key) {
                if many < *from && !c.marks.is_empty() {
                    entries[*row].marks = c.marks.clone();
                    *from = many;
                }
                continue;
            }
            // A recorder splits a long programme across clips, and the
            // pieces are named as pieces so that a list of them reads as one
            // recording in three parts. A pressed disc's "play all" is not
            // that -- it is twelve separate episodes played one after
            // another -- and numbering them (1/15) would be claiming a
            // relationship the disc never asserted.
            let part = match (shape, many > 1) {
                (Shape::Bdav, true) => format!(" ({}/{})", i + 1, many),
                _ => String::new(),
            };
            // A disc whose playlists carry no name -- which is every pressed
            // disc -- still has to name its rows something, and the disc's
            // own name with the clip's number is what there is. It is also
            // what the files a cut writes will be called, so it has to be
            // something a person can tell apart at a glance in a folder.
            let label_row = match &title.name {
                Some(programme) => format!("{programme}{part}"),
                None => format!("{label} {}{part}", c.name),
            };
            seen.push((key, entries.len(), many));
            entries.push(Entry {
                path: c.path.clone(),
                clip: c.name.clone(),
                duration: c.end - c.start,
                marks: c.marks.clone(),
                start: c.start,
                home: home.clone(),
                stem: filename(&label_row),
                label: label_row,
                bytes: vol.bytes(&c.name),
                tracks: vol.tracks(&c.name),
                // Filled in below, once the whole disc is known.
                wanted: false,
            });
        }
    }
    if entries.is_empty() {
        bail!("{}: no playable playlist on this disc", at.display());
    }

    let lengths: Vec<f64> = entries.iter().map(|e| e.duration).collect();
    for (e, wanted) in entries.iter_mut().zip(worth_ticking(shape, &lengths)) {
        e.wanted = wanted;
    }

    Ok(Disc { shape, label, entries })
}

/// Which rows to offer already ticked.
///
/// A disc of recordings is a disc of things somebody chose to record, so all
/// of it is offered. A pressed disc is mostly not the film: see
/// [`WORTH_TICKING`].
fn worth_ticking(shape: Shape, lengths: &[f64]) -> Vec<bool> {
    if shape == Shape::Bdav {
        return vec![true; lengths.len()];
    }
    let mut out: Vec<bool> = lengths.iter().map(|d| *d >= WORTH_TICKING).collect();
    // A disc whose longest clip is four minutes is a disc of four minute
    // clips, and offering none of them ticked would be this reader deciding
    // that a disc it does not understand holds nothing worth opening.
    if !out.contains(&true) {
        let longest = lengths
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i);
        if let Some(i) = longest {
            out[i] = true;
        }
    }
    out
}

/// Two playlists play the same part of a clip when they agree to the tick.
/// Comparing the seconds themselves would be comparing two divisions by
/// 45000 for equality, which is a thing that happens to work.
fn ticks(seconds: f64) -> i64 {
    (seconds * TICK).round() as i64
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

/// A disc, whichever of its two shapes it arrived in and whichever dialect
/// wrote it.
enum Volume {
    /// A directory, and the path of the `BDAV` or `BDMV` directory inside it.
    Dir { shape: Shape, dir: PathBuf },
    /// An image, the path it was opened from, and the prefix that directory
    /// sits at inside it.
    Image { shape: Shape, image: Box<udf::Image>, path: PathBuf, prefix: String },
}

impl Volume {
    fn open(at: &Path) -> Result<Volume> {
        if at.is_dir() {
            let (shape, dir) = disc_dir(at)
                .ok_or_else(|| anyhow!("{}: no BDMV or BDAV directory here", at.display()))?;
            return Ok(Volume::Dir { shape, dir });
        }
        let image = udf::Image::open(at)?;
        let (shape, prefix) = prefix_of(&image)
            .ok_or_else(|| anyhow!("{}: no BDMV or BDAV directory on this image", at.display()))?;
        Ok(Volume::Image { shape, image: Box::new(image), path: at.to_path_buf(), prefix })
    }

    fn shape(&self) -> Shape {
        match self {
            Volume::Dir { shape, .. } | Volume::Image { shape, .. } => *shape,
        }
    }

    /// Read one of the small index files, named relative to `BDAV` or `BDMV`.
    fn read(&mut self, rel: &str) -> Result<Vec<u8>> {
        match self {
            Volume::Dir { dir, .. } => {
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
            Volume::Dir { dir, .. } => at_name(dir, &rel).to_string_lossy().into_owned(),
            Volume::Image { path, prefix, .. } => {
                format!("{}/{prefix}{rel}", path.to_string_lossy())
            }
        }
    }

    /// How large that stream is. Zero where it cannot be asked, which is a
    /// disc missing the file the playlist names -- the row still opens as far
    /// as this module is concerned, and fails where it is opened.
    fn bytes(&self, clip: &str) -> u64 {
        let rel = format!("STREAM/{clip}.m2ts");
        match self {
            Volume::Dir { dir, .. } => {
                std::fs::metadata(at_name(dir, &rel)).map(|m| m.len()).unwrap_or(0)
            }
            Volume::Image { image, prefix, .. } => {
                image.find(&format!("{prefix}{rel}")).map(|e| e.size).unwrap_or(0)
            }
        }
    }

    /// What the disc says one clip carries, out of `CLIPINF`.
    fn tracks(&mut self, clip: &str) -> Vec<Track> {
        self.read(&format!("CLIPINF/{clip}.clpi")).ok().map(|raw| tracks(&raw)).unwrap_or_default()
    }

    /// Everything in one of the disc's directories, whichever case it spells
    /// its names in.
    fn names_in(&self, dir: &str, ext: &str) -> Vec<String> {
        let ext = ext.to_ascii_lowercase();
        let mut names: Vec<String> = match self {
            Volume::Dir { dir: root, .. } => std::fs::read_dir(at_name(root, dir))
                .map(|d| {
                    d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()
                })
                .unwrap_or_default(),
            Volume::Image { image, prefix, .. } => {
                let want = format!("{prefix}{dir}/").to_ascii_uppercase();
                image
                    .files()
                    .iter()
                    .filter(|e| e.path.to_ascii_uppercase().starts_with(&want))
                    .map(|e| e.name().to_string())
                    .collect()
            }
        };
        names.retain(|n| n.to_ascii_lowercase().ends_with(&ext));
        names.sort();
        names
    }

    /// The playlists to offer, in the order the disc would show them.
    ///
    /// On BDAV that is what `info.bdav` says: the index is what a recorder's
    /// own list would show, and it is also what says which playlists are real
    /// -- a disc can hold a `.rpls` that its index does not name.
    ///
    /// On BDMV there is no such list to read. `index.bdmv` names *titles*,
    /// and a title is a Java or navigation program rather than a playlist:
    /// working out which playlist a title plays means running the disc's own
    /// menu code, which is a Blu-ray player and not this. So the directory
    /// stands in, sorted -- which is the order the authoring tool numbered
    /// them in, and on every disc looked at that is the order a person would
    /// have chosen anyway.
    fn playlists(&mut self) -> Vec<String> {
        if self.shape() == Shape::Bdav {
            if let Ok(raw) = self.read("info.bdav") {
                let names = table_of_playlists(&raw);
                if !names.is_empty() {
                    return names;
                }
            }
        }
        self.names_in("PLAYLIST", self.shape().ext())
    }

    /// What the disc calls itself.
    ///
    /// A pressed disc writes it down in `META`, for the player that shows a
    /// shelf of covers. Nothing else does, and the file or folder the disc
    /// arrived as is then the only name there is -- which is not a bad one,
    /// since somebody chose it.
    fn label(&mut self, at: &Path) -> String {
        let fallback = at
            .file_stem()
            .or_else(|| at.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.shape().root().to_string());
        if self.shape() != Shape::Bdmv {
            return fallback;
        }
        // The disc may carry the same file in several languages. Japanese
        // first, because this is a tool for Japanese recordings and a
        // Japanese disc that also ships an English name means the Japanese
        // one; English next, because it is the one a disc almost always has.
        let mut names = self.names_in("META/DL", ".xml");
        names.sort_by_key(|n| {
            let n = n.to_ascii_lowercase();
            match () {
                _ if n.contains("jpn") || n.contains("ja") => 0,
                _ if n.contains("eng") => 1,
                _ => 2,
            }
        });
        for name in names {
            let Ok(raw) = self.read(&format!("META/DL/{name}")) else { continue };
            if let Some(found) = disc_name(&String::from_utf8_lossy(&raw)) {
                return found;
            }
        }
        fallback
    }
}

/// The disc's own directory at or under a path, whichever the user named, and
/// which dialect it speaks.
///
/// Both are worth taking: a disc mounted at `/media/disc` is named by its
/// mount point, and a disc copied to a hard disk is as likely to be named by
/// the `BDMV` directory itself as by the folder holding it -- or by whatever
/// the person copying it typed, which is why the name is a hint and the
/// contents are the answer.
fn disc_dir(at: &Path) -> Option<(Shape, PathBuf)> {
    // The directory inside first, and BDMV before BDAV: a folder holding both
    // is a folder somebody put two things in, and the film is the larger
    // claim.
    for name in ["BDMV", "bdmv", "BDAV", "bdav"] {
        let dir = at.join(name);
        if let Some(shape) = dialect_of(&dir) {
            return Some((shape, dir));
        }
    }
    dialect_of(at).map(|shape| (shape, at.to_path_buf()))
}

/// Which dialect a directory holding a `PLAYLIST` speaks, by what is in it.
///
/// The extension is what says: `.rpls` is a recording's playlist and `.mpls`
/// is a film's. Asking the directory rather than its name is what lets a disc
/// copied under somebody's own name -- `Anime 2026-08-17/`, holding
/// `PLAYLIST`, `CLIPINF` and `STREAM` -- still open.
fn dialect_of(dir: &Path) -> Option<Shape> {
    let playlists = ["PLAYLIST", "playlist"].iter().map(|n| dir.join(n)).find(|p| p.is_dir())?;
    let (mut rpls, mut mpls) = (0usize, 0usize);
    for e in std::fs::read_dir(&playlists).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".rpls") {
            rpls += 1;
        } else if name.ends_with(".mpls") {
            mpls += 1;
        }
    }
    if rpls > mpls {
        return Some(Shape::Bdav);
    }
    if mpls > 0 {
        return Some(Shape::Bdmv);
    }
    // A disc's three directories with no playlist in them is a copy that went
    // wrong, and the only thing left to go on is what it was called.
    match dir.file_name()?.to_string_lossy().to_ascii_uppercase().as_str() {
        "BDMV" => Some(Shape::Bdmv),
        "BDAV" => Some(Shape::Bdav),
        _ => None,
    }
}

/// Where inside an image that directory sits, or `None` when it does not.
///
/// The case a disc spells its directories in is its own business, and so is
/// whether it puts them at the root -- a burner asked for a subdirectory
/// obliges.
fn prefix_of(image: &udf::Image) -> Option<(Shape, String)> {
    for shape in [Shape::Bdmv, Shape::Bdav] {
        let want = format!("{}/PLAYLIST/", shape.root());
        let ext = shape.ext().to_ascii_uppercase();
        let found = image.files().iter().find_map(|e| {
            let upper = e.path.to_ascii_uppercase();
            if !upper.ends_with(&ext) {
                return None;
            }
            let at = upper.find(&want)?;
            // A pressed disc keeps a second copy of its whole index under
            // `BACKUP`, for a player whose read of the first one failed.
            // Reading it here would be reading the disc twice.
            if upper[..at].contains("BACKUP/") {
                return None;
            }
            Some(e.path[..at + shape.root().len() + 1].to_string())
        });
        if let Some(prefix) = found {
            return Some((shape, prefix));
        }
    }
    // An image whose disc was written without its own directory around it.
    // The extension says which dialect, the same as it does for a folder.
    for shape in [Shape::Bdmv, Shape::Bdav] {
        let ext = shape.ext().to_ascii_uppercase();
        let found = image.files().iter().find_map(|e| {
            let upper = e.path.to_ascii_uppercase();
            if !upper.ends_with(&ext) {
                return None;
            }
            let at = upper.find("PLAYLIST/")?;
            (!upper[..at].contains("BACKUP/")).then(|| e.path[..at].to_string())
        });
        if let Some(prefix) = found {
            return Some((shape, prefix));
        }
    }
    None
}

/// A file inside the disc's directory, found whichever case the disc spells
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

/// The name a pressed disc gives itself, out of `META/DL/bdmt_*.xml`.
///
/// Scraped rather than parsed. The file is a document with a dozen
/// namespaces declared and one interesting element in it, and pulling in an
/// XML parser to reach `<di:name>` would be the largest dependency in the
/// program by some way. What is taken is the first `name` element that is not
/// a `titleName` -- the table of contents beside it is a list of the disc's
/// menu titles, which is not what the disc is called.
fn disc_name(xml: &str) -> Option<String> {
    let mut rest = xml;
    while let Some(at) = rest.find("name>") {
        // `<di:name>` and `<name>` both end in `name>`; `<di:titleName>` ends
        // in `Name>` and is passed over by the case-sensitive match above.
        let after = &rest[at + "name>".len()..];
        let open = rest[..at].ends_with('<') || rest[..at].ends_with(':');
        if open {
            if let Some(end) = after.find("</") {
                let text = after[..end].trim();
                if !text.is_empty() {
                    return Some(arib::one_line(text));
                }
            }
        }
        rest = after;
    }
    None
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

/// Read one `.rpls` or `.mpls`.
///
/// The two are the same file with different magic and a different idea of
/// what belongs in the header: the play items and the marks sit at addresses
/// the header gives, in the same layout, and are read by the same code.
fn playlist(raw: &[u8], file: &str, shape: Shape, vol: &mut Volume) -> Result<Title> {
    if raw.len() < 48 || &raw[..4] != shape.magic() {
        bail!("{file} is not a playlist");
    }
    let list_at = u32be(raw, 8) as usize;
    let marks_at = u32be(raw, 12) as usize;

    let mut clips = play_items(raw, list_at, vol)?;
    if clips.is_empty() {
        bail!("{file} plays nothing");
    }
    let duration = clips.iter().map(|c| c.end - c.start).sum();
    let found = play_marks(raw, marks_at, &clips);
    for (clip, marks) in clips.iter_mut().zip(found) {
        clip.marks = marks;
    }

    // Only a recording carries these. A pressed disc's header holds a
    // playback type and a play-all flag where a recorder writes the name of
    // the programme and the night it went out, so reading them there would be
    // reading two numbers as a sentence.
    let (name, made) = match shape {
        Shape::Bdav => (app_info_name(raw, list_at), app_info_made(raw)),
        Shape::Bdmv => (None, None),
    };

    Ok(Title { playlist: file.to_string(), name, made, duration, clips })
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
        out.push(Clip { path: vol.stream(&name), name, start, end, marks: Vec::new() });
        item += 2 + len;
    }
    Ok(out)
}

/// The chapter marks, when they can be believed, each on the clock of the
/// clip it belongs to.
///
/// The entries are a fixed size that the section's own length gives away, but
/// what sits where inside one is not the same on both dialects: BDMV's mark
/// is fourteen bytes -- a byte reserved, the mark's kind, the play item it
/// belongs to, then the time -- and the marks a BDAV recorder writes are
/// longer and carry a name and a thumbnail beside the time. So the layout is
/// not assumed. Each candidate is tried, and the one whose times all land
/// inside the clip they claim is the one that is right -- and when none of
/// them does, the marks are left out rather than guessed at, because a
/// chapter point in the wrong place is worse than no chapter point.
///
/// Reading which play item a mark belongs to is what makes a playlist of
/// twelve episodes usable. Without it every mark is a number with no clock
/// under it: the fifth episode's chapter points are on the fifth episode's
/// own timeline, which shares nothing with the first's beyond both starting
/// near eleven seconds.
fn play_marks(raw: &[u8], at: usize, clips: &[Clip]) -> Vec<Vec<f64>> {
    let none = || vec![Vec::new(); clips.len()];
    if clips.is_empty() {
        return Vec::new();
    }
    let Some(count) = raw.get(at + 4..at + 6).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
    else {
        return none();
    };
    if count == 0 || count > 10_000 {
        return none();
    }
    let len = u32be(raw, at) as usize;
    if len < 2 {
        return none();
    }
    let stride = (len - 2) / count;
    if stride < 8 {
        return none();
    }
    let body_at = at + 6;

    // Where the time sits, and where the play item's number sits when the
    // layout is one that carries it. The pair that reads a BDMV mark is
    // first: it is the only one that can place a mark in a playlist of more
    // than one clip, and on a playlist of exactly one it agrees with the
    // others anyway.
    for (time_at, item_at) in [(4usize, Some(2usize)), (4, None), (6, Some(2)), (6, None)] {
        if item_at.is_some_and(|o| o + 2 > time_at) {
            continue;
        }
        let mut out = none();
        let mut sound = true;
        for i in 0..count {
            let entry = body_at + i * stride;
            if raw.get(entry..entry + stride).is_none() {
                sound = false;
                break;
            }
            // A layout that does not say which clip a mark is on can only be
            // believed on a playlist that plays one.
            let which = match item_at {
                Some(o) => u16be(raw, entry + o) as usize,
                None => 0,
            };
            let (Some(clip), Some(marks)) = (clips.get(which), out.get_mut(which)) else {
                sound = false;
                break;
            };
            let time = u32be(raw, entry + time_at) as f64 / TICK;
            // A mark belongs to a play item and cannot fall outside it. That
            // is the whole of the check, and it is enough: a field read from
            // the wrong offset lands outside a ten minute window
            // immediately.
            if time < clip.start - 0.001 || time > clip.end + 0.001 {
                sound = false;
                break;
            }
            marks.push(time - clip.start);
        }
        if !sound || out.iter().all(Vec::is_empty) {
            continue;
        }
        // A playlist of several clips whose marks all landed on the first of
        // them has been read by a layout that cannot say which clip it meant.
        // Believing it would put every episode's chapter points on episode
        // one.
        if item_at.is_none() && clips.len() > 1 {
            continue;
        }
        for marks in out.iter_mut() {
            marks.sort_by(f64::total_cmp);
            marks.dedup_by(|a, b| (*a - *b).abs() < 0.001);
        }
        return out;
    }
    none()
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

fn u16be(b: &[u8], at: usize) -> u16 {
    b.get(at..at + 2).map(|v| u16::from_be_bytes([v[0], v[1]])).unwrap_or(0)
}

// --- what a clip carries, out of CLIPINF ---------------------------------
//
// A `.clpi` is the clip's own index: where its program sequences begin, what
// streams each of them carries, and -- in the part this does not read -- a
// map of every entry point in the video. Only the stream list is wanted here,
// and only to draw a list somebody chooses from.

/// The streams `raw`, one `.clpi` file, says its clip carries.
///
/// Empty for anything this cannot make sense of. A chooser that listed
/// nothing is a chooser that offers the clip whole, which is the right answer
/// when the disc will not say what is in it; a chooser that listed a guess
/// would have somebody switch off a track that was never there.
fn tracks(raw: &[u8]) -> Vec<Track> {
    // The two dialects sign the file differently and write the same thing
    // after it: a pressed disc's clip index opens `HDMV`, a recorder's opens
    // `M2TS`.
    if raw.len() < 24 || !matches!(&raw[..4], b"HDMV" | b"M2TS") {
        return Vec::new();
    }
    let at = u32be(raw, 12) as usize;
    let Some(&sequences) = raw.get(at + 5) else { return Vec::new() };
    let mut out: Vec<Track> = Vec::new();
    let mut p = at + 6;
    for _ in 0..sequences.min(64) {
        let Some(&count) = raw.get(p + 6) else { break };
        p += 8;
        for _ in 0..count {
            let pid = u16be(raw, p) as i32;
            let Some(&len) = raw.get(p + 2) else { return out };
            let Some(attr) = raw.get(p + 3..p + 3 + len as usize) else { return out };
            p += 3 + len as usize;
            // A clip whose program map changes part-way through is written as
            // several sequences naming the same streams. One row each would
            // be the same track listed twice.
            if out.iter().any(|t| t.pid == pid) {
                continue;
            }
            if let Some(track) = track_of(pid, attr) {
                out.push(track);
            }
        }
    }
    out
}

/// One entry of a `.clpi` stream list.
fn track_of(pid: i32, attr: &[u8]) -> Option<Track> {
    let coding = *attr.first()?;
    let lang = |at: usize| -> Option<String> {
        let raw = attr.get(at..at + 3)?;
        let text: String =
            raw.iter().take_while(|b| b.is_ascii_alphabetic()).map(|b| *b as char).collect();
        (text.len() == 3).then_some(text.to_lowercase())
    };
    let (kind, detail, language, carried) = match coding {
        // Video. The second byte is the frame's shape and its rate, four bits
        // each.
        0x01 | 0x02 | 0x1B | 0x20 | 0x24 | 0xEA => {
            let name = match coding {
                0x01 => "MPEG-1",
                0x02 => "MPEG-2",
                0x1B => "H.264",
                0x20 => "MVC",
                0x24 => "HEVC",
                _ => "VC-1",
            };
            let b = attr.get(1).copied().unwrap_or(0);
            let shape = match b >> 4 {
                1 => "480i",
                2 => "576i",
                3 => "480p",
                4 => "1080i",
                5 => "720p",
                6 => "1080p",
                7 => "576p",
                8 => "2160p",
                _ => "",
            };
            let rate = match b & 0x0F {
                1 => "23.976",
                2 => "24",
                3 => "25",
                4 => "29.97",
                6 => "50",
                7 => "59.94",
                _ => "",
            };
            let mut detail = name.to_string();
            if !shape.is_empty() {
                detail.push(' ');
                detail.push_str(shape);
            }
            if !rate.is_empty() {
                detail.push(' ');
                detail.push_str(rate);
                detail.push_str("fps");
            }
            ("video", detail, None, true)
        }
        // Sound. The second byte is the channel arrangement and the sample
        // rate, four bits each; the language follows it.
        0x03 | 0x04 | 0x0F | 0x11 | 0x80..=0x86 | 0xA1 | 0xA2 => {
            let name = match coding {
                0x03 => "MPEG-1 audio",
                0x04 => "MPEG-2 audio",
                0x0F | 0x11 => "AAC",
                0x80 => "LPCM",
                0x81 => "AC-3",
                0x82 => "DTS",
                0x83 => "TrueHD",
                0x84 => "E-AC-3",
                0x85 => "DTS-HD",
                0x86 => "DTS-HD MA",
                0xA1 => "E-AC-3",
                _ => "DTS-HD",
            };
            let b = attr.get(1).copied().unwrap_or(0);
            let channels = match b >> 4 {
                1 => "mono",
                3 => "stereo",
                6 => "multi",
                12 => "stereo+multi",
                _ => "",
            };
            let rate = match b & 0x0F {
                1 => "48kHz",
                4 => "96kHz",
                5 => "192kHz",
                12 => "48/192kHz",
                14 => "48/96kHz",
                _ => "",
            };
            let mut detail = name.to_string();
            for bit in [channels, rate] {
                if !bit.is_empty() {
                    detail.push(' ');
                    detail.push_str(bit);
                }
            }
            ("audio", detail, lang(2), true)
        }
        // The graphics a Blu-ray's subtitles and menus are made of. Each is a
        // little display list rather than a run of timed packets, and there
        // is nowhere on a cut timeline to put one. See [`Track::carried`].
        0x90 => ("subtitle", "PGS".to_string(), lang(1), false),
        0x91 => ("menu", "IGS".to_string(), lang(1), false),
        0x92 => ("subtitle", "TextST".to_string(), lang(1), false),
        // Anything else, which on a Japanese recording is the broadcast's own
        // private streams -- the captions among them. Named by its number
        // rather than described, because what a private stream holds is not
        // something the disc's index says.
        c => ("other", format!("stream type 0x{c:02x}"), None, true),
    };
    Some(Track { kind, pid, detail, language, carried })
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
        Clip {
            name: "00001".into(),
            path: "/x/00001.m2ts".into(),
            start,
            end,
            marks: Vec::new(),
        }
    }

    /// A mark section: one entry of `stride` bytes each, carrying the play
    /// item it belongs to at `item_at` and its time at `time_at`.
    fn mark_section(
        stride: usize,
        entries: &[(u16, u32)],
        item_at: usize,
        time_at: usize,
    ) -> Vec<u8> {
        let mut raw = vec![0u8; 6 + entries.len() * stride];
        raw[..4].copy_from_slice(&((2 + entries.len() * stride) as u32).to_be_bytes());
        raw[4..6].copy_from_slice(&(entries.len() as u16).to_be_bytes());
        for (i, (item, ticks)) in entries.iter().enumerate() {
            let at = 6 + i * stride;
            raw[at + item_at..at + item_at + 2].copy_from_slice(&item.to_be_bytes());
            raw[at + time_at..at + time_at + 4].copy_from_slice(&ticks.to_be_bytes());
        }
        raw
    }

    #[test]
    fn takes_the_marks_that_land_inside_the_title() {
        // Two marks, 46 bytes each, with the time six bytes in: what a
        // recorder writes.
        let raw = mark_section(46, &[(0, 20_842), (0, 22_876_605)], 2, 6);
        let marks = play_marks(&raw, 0, &[clip(20_842.0 / TICK, 22_971_270.0 / TICK)]);
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].len(), 2);
        assert!(marks[0][0].abs() < 0.001);
        assert!((marks[0][1] - 507.905).abs() < 0.01);
    }

    #[test]
    fn takes_the_fourteen_byte_marks_a_pressed_disc_writes() {
        // BDMV: fourteen bytes an entry, the play item two in and the time
        // four in.
        let raw = mark_section(14, &[(0, 524_280), (0, 10_000_000), (0, 30_000_000)], 2, 4);
        let marks = play_marks(&raw, 0, &[clip(524_280.0 / TICK, 32_565_437.0 / TICK)]);
        assert_eq!(marks[0].len(), 3);
        assert!(marks[0][0].abs() < 0.001);
        assert!((marks[0][1] - (10_000_000.0 - 524_280.0) / TICK).abs() < 0.001);
    }

    #[test]
    fn offers_the_part_of_a_pressed_disc_that_is_the_film() {
        // Twelve minutes is an episode; ten seconds is the transition
        // between two menus.
        let disc = [10.0, 712.0, 10.0, 712.0, 47.0];
        assert_eq!(
            worth_ticking(Shape::Bdmv, &disc),
            [false, true, false, true, false]
        );
        // A disc of recordings is all of it.
        assert_eq!(worth_ticking(Shape::Bdav, &disc), [true; 5]);
        // And a disc that is all short clips is not a disc holding nothing.
        assert_eq!(worth_ticking(Shape::Bdmv, &[10.0, 47.0, 3.0]), [false, true, false]);
        assert!(worth_ticking(Shape::Bdmv, &[]).is_empty());
    }

    #[test]
    fn a_mark_lands_on_the_clip_it_belongs_to() {
        // What a "play all" is: three episodes, each on its own clock, each
        // starting where a Blu-ray always starts. A mark in the third
        // episode is a small number, not a number near three hours.
        let clips: Vec<Clip> = (0..3).map(|_| clip(11.651, 723.695)).collect();
        let ticks = |seconds: f64| (seconds * TICK) as u32;
        let raw = mark_section(
            14,
            &[
                (0, ticks(11.651)),
                (0, ticks(100.0)),
                (1, ticks(11.651)),
                (2, ticks(11.651)),
                (2, ticks(600.0)),
            ],
            2,
            4,
        );
        let marks = play_marks(&raw, 0, &clips);
        assert_eq!(marks.len(), 3);
        assert_eq!(marks[0].len(), 2);
        assert_eq!(marks[1].len(), 1);
        assert_eq!(marks[2].len(), 2);
        assert!((marks[2][1] - (600.0 - 11.651)).abs() < 0.01);
    }

    #[test]
    fn leaves_a_multi_clip_playlist_unmarked_rather_than_marked_wrongly() {
        // A layout with no play item number in it says nothing about which of
        // three clips a mark is on, and putting all three episodes' chapter
        // points on episode one would be worse than putting down none.
        let clips: Vec<Clip> = (0..3).map(|_| clip(11.651, 723.695)).collect();
        // 46 byte entries with the time six in, and something at offset two
        // that is not a play item of this playlist -- so the layout that
        // reads one there is rejected as well.
        let raw = mark_section(46, &[(40_000, 524_280), (40_000, 10_000_000)], 2, 6);
        assert!(play_marks(&raw, 0, &clips).iter().all(Vec::is_empty));
    }

    #[test]
    fn leaves_out_marks_it_cannot_place() {
        // Times that fall nowhere near the title, at every candidate offset.
        let raw = mark_section(46, &[(0, 900_000_000), (0, 900_000_000)], 2, 6);
        assert!(play_marks(&raw, 0, &[clip(0.4, 510.0)]).iter().all(Vec::is_empty));
    }

    /// One `.clpi`, with the stream list of an episode off a pressed disc:
    /// H.264 1080p 23.976, two TrueHD tracks, a subtitle and a menu.
    fn clpi() -> Vec<u8> {
        let streams: Vec<(u16, Vec<u8>)> = vec![
            (0x1011, vec![0x1B, 0x61, 0x20, 0x00]),
            (0x1100, vec![0x83, 0x61, b'e', b'n', b'g']),
            (0x1101, vec![0x83, 0x31, b'j', b'p', b'n']),
            (0x1200, vec![0x90, b'e', b'n', b'g']),
            (0x1400, vec![0x91, b'e', b'n', b'g']),
        ];
        let at = 40usize;
        let mut raw = vec![0u8; at];
        raw[..8].copy_from_slice(b"HDMV0200");
        raw[12..16].copy_from_slice(&(at as u32).to_be_bytes());
        raw.extend_from_slice(&0u32.to_be_bytes()); // length, unread
        raw.push(0); // reserved
        raw.push(1); // one program sequence
        raw.extend_from_slice(&0u32.to_be_bytes()); // SPN of its start
        raw.extend_from_slice(&0x0100u16.to_be_bytes()); // program map PID
        raw.push(streams.len() as u8);
        raw.push(0); // reserved
        for (pid, attr) in &streams {
            raw.extend_from_slice(&pid.to_be_bytes());
            raw.push(attr.len() as u8);
            raw.extend_from_slice(attr);
        }
        raw
    }

    #[test]
    fn reads_what_a_clip_carries() {
        let found = tracks(&clpi());
        assert_eq!(found.len(), 5);
        assert_eq!(found[0].kind, "video");
        assert_eq!(found[0].pid, 0x1011);
        assert_eq!(found[0].detail, "H.264 1080p 23.976fps");
        assert_eq!(found[1].kind, "audio");
        assert_eq!(found[1].detail, "TrueHD multi 48kHz");
        assert_eq!(found[1].language.as_deref(), Some("eng"));
        assert_eq!(found[2].detail, "TrueHD stereo 48kHz");
        assert_eq!(found[2].language.as_deref(), Some("jpn"));
        // The graphics are listed, and listed as things a cut leaves behind.
        assert_eq!(found[3].kind, "subtitle");
        assert!(!found[3].carried);
        assert_eq!(found[4].kind, "menu");
        assert!(!found[4].carried);
        assert!(found[..3].iter().all(|t| t.carried));
    }

    #[test]
    fn says_nothing_about_a_clip_index_it_cannot_read() {
        assert!(tracks(b"not a clpi").is_empty());
        assert!(tracks(&[]).is_empty());
    }

    /// What a recorder writes, read off a real disc: a different magic, three
    /// streams, a language field cut short, and a private stream that says
    /// nothing about itself.
    #[test]
    fn reads_what_a_recorder_wrote() {
        let mut raw = vec![0u8; 40];
        raw[..8].copy_from_slice(b"M2TS0100");
        raw[12..16].copy_from_slice(&40u32.to_be_bytes());
        raw.extend_from_slice(&26u32.to_be_bytes());
        raw.extend_from_slice(&[0, 1]);
        raw.extend_from_slice(&0u32.to_be_bytes());
        raw.extend_from_slice(&0x0100u16.to_be_bytes());
        raw.extend_from_slice(&[3, 0]);
        for (pid, attr) in [
            (0x1100u16, vec![0x02u8, 0x44, 0x30]),
            (0x1101, vec![0x0F, 0x31, 0x00]),
            (0x1102, vec![0x06]),
        ] {
            raw.extend_from_slice(&pid.to_be_bytes());
            raw.push(attr.len() as u8);
            raw.extend_from_slice(&attr);
        }
        let found = tracks(&raw);
        assert_eq!(found.len(), 3);
        assert_eq!((found[0].kind, found[0].detail.as_str()), ("video", "MPEG-2 1080i 29.97fps"));
        assert_eq!((found[1].kind, found[1].detail.as_str()), ("audio", "AAC stereo 48kHz"));
        // Three bytes of attributes leave no room for a language, and a
        // language guessed at is worse than none.
        assert_eq!(found[1].language, None);
        // A broadcast's private stream -- the captions, on this disc. The
        // index does not say what is in one, so neither does this.
        assert_eq!((found[2].kind, found[2].detail.as_str()), ("other", "stream type 0x06"));
        assert!(found.iter().all(|t| t.carried));
    }

    #[test]
    fn takes_the_disc_its_own_name() {
        let xml = r#"<disclib xmlns="urn:BDA:bdmv;disclib">
          <di:discinfo xmlns:di="urn:BDA:bdmv;discinfo">
            <di:title><di:name>Isekai Quartet Season 1</di:name></di:title>
            <di:description><di:tableOfContents>
              <di:titleName titleNumber="1">T01 Feature</di:titleName>
            </di:tableOfContents></di:description>
          </di:discinfo></disclib>"#;
        assert_eq!(disc_name(xml).as_deref(), Some("Isekai Quartet Season 1"));
        // A table of contents on its own is not a name for the disc.
        assert_eq!(disc_name("<di:titleName>T01 Feature</di:titleName>"), None);
        assert_eq!(disc_name(""), None);
    }
}
