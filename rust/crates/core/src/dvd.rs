//! What a DVD-Video holds, read from the disc's own index.
//!
//! A DVD keeps everything in one directory, and unlike a Blu-ray it keeps the
//! index in a format of its own rather than in something resembling the
//! stream:
//!
//! ```text
//! VIDEO_TS/
//!   VIDEO_TS.IFO     the disc's own table: how many title sets, and the
//!                    titles they hold in the order a player numbers them
//!   VIDEO_TS.VOB     the top menu, which is not a recording
//!   VTS_01_0.IFO     one title set's index: its titles, their chapters,
//!                    which sectors each of them plays, and what sound and
//!                    subtitles it carries
//!   VTS_01_0.VOB     that title set's menu, which is also not a recording
//!   VTS_01_1.VOB     the video itself, split at a gigabyte because ISO 9660
//!   VTS_01_2.VOB     could not address more than that
//!   ...
//!   VTS_01_0.BUP     a byte-for-byte copy of the IFO, for a player whose
//!                    read of the first one failed
//! ```
//!
//! Three things about that shape decide how this is written.
//!
//! **The stream is one stream, not nine.** `VTS_01_1.VOB` through
//! `VTS_01_9.VOB` are one MPEG program stream that the format required to be
//! written in pieces of no more than a gigabyte, and the disc addresses it as
//! a single run of sectors numbered from zero -- so a cell that begins at
//! sector 536,000 is 400 megabytes into the *third* file. The pieces are laid
//! down one after another with nothing between them, which is what makes the
//! whole title set one byte range of the image, and [`crate::input`] joins
//! them back for a directory as well.
//!
//! **A title is a run of cells, not a file.** Where a Blu-ray gives an
//! episode a stream of its own, a DVD gives it a stretch of the one stream,
//! and the index is the only thing that says where. So a title's name carries
//! the sectors it plays -- `VTS_01_1.VOB@0-2082359` -- because a name that
//! said only `VTS_01_1.VOB` would name four gigabytes of everything the disc
//! holds and no episode in particular.
//!
//! **The clock is in the stream, not the index.** Cell times are durations
//! counted from the start of the title; the program stream's timestamps begin
//! wherever the author's multiplexer began them. The two are joined by the
//! navigation pack that opens every VOBU: 2048 bytes at a sector this module
//! already knows, holding the presentation time of the picture that follows.
//! One read of it turns a chapter at "twelve minutes in" into a time on the
//! clock the demuxer will report.
//!
//! **Encrypted discs are not handled and will not be.** CSS is a decryption
//! problem and this program has none of it. A disc copied by a tool that
//! removed the encryption reads like any other.

use crate::disc::{Disc, Entry, Shape, Track};
use crate::udf;
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

/// A DVD sector, and the unit every address in the index is counted in.
const SECTOR: u64 = 2048;

/// The clock a program stream's timestamps are on, and the one the navigation
/// packs count in.
const PTM: f64 = 90_000.0;

/// How long a title has to be before it is offered already ticked.
///
/// The same reasoning as a pressed Blu-ray's, and the same number: a DVD's
/// title list holds the film and it also holds the trailers, the "play all"
/// and whatever the menu loops over.
const WORTH_TICKING: f64 = 300.0;

/// How many title sets a disc may have. The format's own limit is 99.
const MAX_TITLE_SETS: usize = 99;

/// One stretch of a title that runs on a single clock.
///
/// Usually the whole title. A DVD is allowed to assemble a title out of
/// pieces that were multiplexed separately, and where it does, the
/// timestamps start again at the seam -- the disc this was written against
/// ends its hour with an eight second cell whose clock begins at nought.
///
/// Splicing two clocks is not the same operation as cutting one, so a title
/// like that becomes a row for each piece, which is what a Blu-ray playlist
/// of several clips already does here. Every second of the disc is
/// reachable and the list says plainly that it is in pieces.
#[derive(Debug, Clone)]
pub struct Title {
    /// The number a player would call the title this came from, counted
    /// across the whole disc.
    pub number: usize,
    /// Which piece of that title this is, and how many there are. `(1, 1)`
    /// for a title that runs on one clock, which is nearly all of them.
    pub part: usize,
    pub parts: usize,
    /// Which title set it lives in, `1` for `VTS_01_*`.
    pub vts: usize,
    /// How long the index says this piece runs.
    pub duration: f64,
    /// Chapter points, in seconds from the start of this piece.
    pub chapters: Vec<f64>,
    /// The first and last sector it plays, counted from the start of the
    /// title set's stream.
    pub first_sector: u64,
    pub last_sector: u64,
    /// Where this piece's first picture sits on the program stream's own
    /// clock, read from the navigation pack at [`Title::first_sector`].
    pub start: f64,
    /// What the index says the title carries.
    pub tracks: Vec<Track>,
}

/// Whether this is worth trying to read as a DVD.
///
/// Cheap on purpose, the same as [`crate::disc::looks_like_disc`]: a
/// directory is asked whether it holds a `VIDEO_TS`, and a file is asked only
/// about its extension. Whether an `.iso` is a DVD or a Blu-ray is a question
/// for [`read`], which has the image open anyway.
pub fn looks_like_dvd(at: &Path) -> bool {
    if at.is_dir() {
        return video_ts(at).is_some();
    }
    at.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("iso"))
}

/// Whether this really is a DVD, as against something that merely arrived by
/// the same door.
///
/// Asked only when a read has already failed, so it may open the image again;
/// what it buys is that the complaint the user sees is the one that belongs
/// to the disc they have.
pub fn is_dvd(at: &Path) -> bool {
    Volume::open(at).is_ok()
}

/// Everything on a DVD that can be opened, in the order the disc numbers it.
pub fn read(at: &Path) -> Result<Disc> {
    let mut vol = Volume::open(at)?;
    let label = vol.label(at);
    let home = beside(at);
    let titles = titles(&mut vol)?;

    let mut entries: Vec<Entry> = Vec::new();
    // What has already been offered. A disc names the same stretch of stream
    // more than once -- this one holds a title of nineteen cells and another
    // of the same nineteen less the last, which between them are two things
    // and not three -- and a row is a stretch of stream, so the second
    // mention of one is not a row.
    let mut seen: Vec<(u64, u64)> = Vec::new();
    for t in titles {
        let key = (t.first_sector, t.last_sector);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let sectors = t.last_sector + 1 - t.first_sector;
        let path = vol.stream(t.vts, t.first_sector, t.last_sector);
        let part = if t.parts > 1 {
            format!(" ({}/{})", t.part, t.parts)
        } else {
            String::new()
        };
        let label_row = format!("{label} {}{part}", t.number);
        entries.push(Entry {
            path,
            // A DVD has no clip numbers; the title set is the nearest thing
            // to one, and nothing outside this crate looks at it.
            clip: format!("{:02}", t.vts),
            duration: t.duration,
            // Marks are carried on the clip's own clock, which for a program
            // stream means the multiplexer's -- and the navigation pack at
            // the first sector is what says where that clock stood.
            marks: t.chapters,
            start: t.start,
            home: home.clone(),
            stem: crate::disc::filename(&label_row),
            label: label_row,
            bytes: sectors * SECTOR,
            // A DVD's index has nowhere to write any of this: the format
            // predates the question.
            made: None,
            description: None,
            channel: None,
            channel_number: 0,
            tracks: t.tracks,
            // Filled in below, once the whole disc is known.
            wanted: false,
        });
    }
    if entries.is_empty() {
        bail!("{}: no playable title on this disc", at.display());
    }

    // Asked of the rows and not of the titles, so that the guarantee that
    // something is ticked is a guarantee about what the chooser shows.
    let lengths: Vec<f64> = entries.iter().map(|e| e.duration).collect();
    for (e, wanted) in entries.iter_mut().zip(worth_ticking(&lengths)) {
        e.wanted = wanted;
    }
    Ok(Disc {
        shape: Shape::Dvd,
        label,
        entries,
    })
}

/// The titles the disc's own tables list, in the order a player numbers them.
///
/// One row per stretch of stream that runs on one clock, so a title
/// assembled out of separately multiplexed pieces comes back as one entry per
/// piece. See [`Title`].
pub fn titles(vol: &mut Volume) -> Result<Vec<Title>> {
    let vmg = vol
        .read("VIDEO_TS.IFO")
        .context("this disc has no VIDEO_TS.IFO")?;
    if !vmg.starts_with(b"DVDVIDEO-VMG") {
        bail!("VIDEO_TS.IFO is not a DVD-Video manager table");
    }

    // The manager's table of titles says which title set each of them is in
    // and which of that set's titles it is; everything else about a title --
    // how long it runs, which sectors it plays, where its chapters are -- is
    // in the set's own index. So the manager is read for the order and the
    // sets are read for the substance.
    let srpt_at = u32be(&vmg, 0xc4) as usize * SECTOR as usize;
    let srpt = vmg.get(srpt_at..).unwrap_or_default();
    if srpt.len() < 8 {
        bail!("VIDEO_TS.IFO holds no table of titles");
    }
    let count = u16be(srpt, 0) as usize;

    let mut out = Vec::new();
    // One title set is read once however many of the disc's titles are in it.
    let mut cache: Vec<(usize, Option<TitleSet>)> = Vec::new();
    for i in 0..count {
        let at = 8 + i * 12;
        if srpt.len() < at + 12 {
            break;
        }
        let vts = srpt[at + 6] as usize;
        let vts_ttn = srpt[at + 7] as usize;
        if vts == 0 || vts > MAX_TITLE_SETS {
            continue;
        }
        if !cache.iter().any(|(n, _)| *n == vts) {
            // A title set the disc names and does not hold is a damaged disc,
            // not a reason to show none of the others.
            cache.push((vts, title_set(vol, vts).ok()));
        }
        let Some((_, Some(set))) = cache.iter().find(|(n, _)| *n == vts) else {
            continue;
        };
        let Some(parts) = set.parts.get(vts_ttn.wrapping_sub(1)) else {
            continue;
        };
        // All of one title's chapters are in one chain on every disc this has
        // met, and a title spread over several is one this does not claim to
        // understand.
        let Some(&(pgcn, _)) = parts.first() else {
            continue;
        };
        if parts.iter().any(|&(n, _)| n != pgcn) {
            continue;
        }
        let Some(pgc) = set.chains.get(pgcn.wrapping_sub(1)) else {
            continue;
        };
        let entries: Vec<usize> = parts.iter().map(|&(_, pgn)| pgn).collect();
        out.extend(pieces(vol, vts, i + 1, pgc, &entries, &set.tracks));
    }
    if out.is_empty() {
        bail!("this disc's table of titles is empty");
    }
    Ok(out)
}

/// One title, cut at the seams where its clock starts again.
fn pieces(
    vol: &mut Volume,
    vts: usize,
    number: usize,
    pgc: &Pgc,
    entries: &[usize],
    tracks: &[Track],
) -> Vec<Title> {
    let runs = runs_of(vol, vts, pgc);
    let parts = runs.len();
    runs.into_iter()
        .enumerate()
        .map(|(k, run)| {
            // A chapter belongs to the piece holding the cell it starts at,
            // and is timed from that piece's own beginning. Which means there
            // is nowhere to put one when that beginning could not be read: a
            // chapter is a time on the stream's clock or it is nothing, and a
            // mark in the wrong place is worse than no mark.
            let chapters = entries
                .iter()
                .filter_map(|&pgn| pgc.programs.get(pgn.checked_sub(1)?).copied())
                .filter(|&cell| cell > run.from && cell <= run.to)
                .map(|cell| {
                    pgc.cells[run.from..cell - 1]
                        .iter()
                        .map(|c| c.length)
                        .sum::<f64>()
                        + 0.0
                })
                .collect();
            Title {
                number,
                part: k + 1,
                parts,
                vts,
                duration: pgc.cells[run.from..run.to].iter().map(|c| c.length).sum(),
                chapters: if run.start.is_some() {
                    chapters
                } else {
                    Vec::new()
                },
                first_sector: pgc.cells[run.from].first,
                last_sector: pgc.cells[run.to - 1].last,
                start: run.start.unwrap_or(0.0),
                tracks: tracks.to_vec(),
            }
        })
        .collect()
}

/// A stretch of a chain's cells that runs on one clock.
struct Run {
    /// Half-open range of cell indices.
    from: usize,
    to: usize,
    /// Where the first of them begins on the stream's own clock, when the
    /// navigation pack at that sector could be read. `None` is a damaged
    /// disc, and costs the chapters -- see [`pieces`].
    start: Option<f64>,
}

/// Where a chain's clock starts again, and so where its rows have to be cut.
///
/// The index does not record this -- cell times are durations, and a duration
/// says nothing about the clock it is counted on. The navigation packs do: the
/// one opening a cell says when its pictures are shown, and the one opening
/// the cell's last VOBU says when that VOBU's pictures stop. Two cells belong
/// to the same run when the second begins where the first left off.
fn runs_of(vol: &mut Volume, vts: usize, pgc: &Pgc) -> Vec<Run> {
    /// How far apart the seam of two cells may be and still be a seam. A
    /// cell's own last VOBU has to run out before the next begins, so the
    /// honest gap is nought; a second of slack is well inside a discontinuity,
    /// which restarts the clock from the beginning.
    const SLACK: f64 = 1.0;

    let mut out: Vec<Run> = Vec::new();
    let mut ends_at = f64::NAN;
    for (i, cell) in pgc.cells.iter().enumerate() {
        let start = vol.vobu_time(vts, cell.first);
        let joins = match (start, out.last()) {
            // A cell whose navigation pack could not be read is left with the
            // run before it. Cutting on a failed read would put a seam where
            // the disc has none.
            (None, Some(_)) => true,
            (None, None) => false,
            // A cell whose predecessor's end could not be read joins it too:
            // there is no evidence of a seam, and inventing one would split a
            // programme in half over a sector that would not read.
            (Some(t), Some(_)) => !ends_at.is_finite() || (t - ends_at).abs() <= SLACK,
            (Some(_), None) => false,
        };
        match out.last_mut() {
            Some(run) if joins => run.to = i + 1,
            _ => out.push(Run {
                from: i,
                to: i + 1,
                start,
            }),
        }
        // Where this cell leaves the clock, for the next one to be judged
        // against. Its last VOBU is the one that knows.
        ends_at = vol
            .vobu_end(vts, cell.last_vobu)
            .or_else(|| start.map(|t| t + cell.length))
            .unwrap_or(f64::NAN);
    }
    out
}

/// What one title set's index says, read once.
struct TitleSet {
    tracks: Vec<Track>,
    chains: Vec<Pgc>,
    /// For each of the set's titles, the parts it is made of.
    parts: Vec<Vec<(usize, usize)>>,
}

fn title_set(vol: &mut Volume, vts: usize) -> Result<TitleSet> {
    let ifo = vol.read(&format!("VTS_{vts:02}_0.IFO"))?;
    if !ifo.starts_with(b"DVDVIDEO-VTS") {
        bail!("VTS_{vts:02}_0.IFO is not a DVD-Video title set table");
    }
    Ok(TitleSet {
        tracks: tracks(&ifo),
        chains: program_chains(&ifo)?,
        parts: parts_of_titles(&ifo),
    })
}

/// One cell: the smallest thing a chain plays, and the unit its sectors and
/// its seconds are both counted in.
struct Cell {
    length: f64,
    /// Where the cell begins, ends, and where its last VOBU begins -- all
    /// counted in sectors from the start of the title set's stream.
    first: u64,
    last_vobu: u64,
    last: u64,
}

/// One program chain: the thing a title actually plays.
struct Pgc {
    cells: Vec<Cell>,
    /// The cell each program starts at, counting cells from one.
    programs: Vec<usize>,
}

/// Every program chain in a title set's index, in its own numbering.
fn program_chains(ifo: &[u8]) -> Result<Vec<Pgc>> {
    let table_at = u32be(ifo, 0xcc) as usize * SECTOR as usize;
    let table = ifo.get(table_at..).unwrap_or_default();
    if table.len() < 8 {
        bail!("this title set holds no program chains");
    }
    let count = u16be(table, 0) as usize;
    let mut out = Vec::new();
    for i in 0..count {
        let at = 8 + i * 8;
        if table.len() < at + 8 {
            break;
        }
        let start = u32be(table, at + 4) as usize;
        // A chain that will not read still has to occupy its number, or every
        // chain after it is misnamed.
        let empty = Pgc {
            cells: Vec::new(),
            programs: Vec::new(),
        };
        out.push(table.get(start..).and_then(read_pgc).unwrap_or(empty));
    }
    Ok(out)
}

/// One program chain, from the bytes it starts at.
///
/// A chain whose cells do not lie end to end in the stream is not read at
/// all. It is an angle block, or one assembled out of pieces of several
/// titles, and the span its cells happen to lie inside is not the title --
/// saying nothing is better than offering that.
fn read_pgc(g: &[u8]) -> Option<Pgc> {
    if g.len() < 0xec {
        return None;
    }
    let programs_n = g[2] as usize;
    let cells_n = g[3] as usize;
    let map_at = u16be(g, 0xe6) as usize;
    let cells_at = u16be(g, 0xe8) as usize;
    if map_at == 0 || cells_at == 0 {
        return None;
    }

    let mut cells: Vec<Cell> = Vec::with_capacity(cells_n);
    for c in 0..cells_n {
        let at = cells_at + c * 24;
        if g.len() < at + 24 {
            return None;
        }
        // A cell in an angle block or an interleaved unit is one of several
        // the player picks between, and laying them end to end would count
        // the same seconds two or three times over.
        if g[at] & 0xc0 != 0 {
            return None;
        }
        let cell = Cell {
            length: dvd_time(g, at + 4)?,
            first: u32be(g, at + 8) as u64,
            last_vobu: u32be(g, at + 0x10) as u64,
            last: u32be(g, at + 0x14) as u64,
        };
        if cell.last < cell.first || cell.last_vobu < cell.first || cell.last_vobu > cell.last {
            return None;
        }
        if let Some(before) = cells.last() {
            if cell.first != before.last + 1 {
                return None;
            }
        }
        cells.push(cell);
    }

    let mut programs = Vec::with_capacity(programs_n);
    for p in 0..programs_n {
        let &cell = g.get(map_at + p)?;
        let cell = cell as usize;
        if cell == 0 || cell > cells.len() {
            return None;
        }
        programs.push(cell);
    }
    Some(Pgc { cells, programs })
}

/// For each title in the set, the parts it is made of as `(chain, program)`.
///
/// This is the table a player's chapter-skip button walks, and it is the only
/// place a DVD says which of a chain's programs are chapters -- a chain can
/// hold programs that no title lists as a part, and those are not chapters.
fn parts_of_titles(ifo: &[u8]) -> Vec<Vec<(usize, usize)>> {
    let table_at = u32be(ifo, 0xc8) as usize * SECTOR as usize;
    let Some(table) = ifo.get(table_at..) else {
        return Vec::new();
    };
    if table.len() < 8 {
        return Vec::new();
    }
    let count = u16be(table, 0) as usize;
    let last = u32be(table, 4) as usize;
    let mut out = Vec::new();
    for i in 0..count {
        let at = 8 + i * 4;
        if table.len() < at + 4 {
            break;
        }
        let from = u32be(table, at) as usize;
        // A title's parts run to the start of the next title's, and the last
        // one runs to the end of the table.
        let to = if i + 1 < count {
            table
                .get(at + 4..at + 8)
                .map(|b| u32be(b, 0) as usize)
                .unwrap_or(last + 1)
        } else {
            last + 1
        };
        let mut parts = Vec::new();
        let mut p = from;
        while p + 4 <= to.min(table.len()) {
            parts.push((u16be(table, p) as usize, u16be(table, p + 2) as usize));
            p += 4;
        }
        out.push(parts);
    }
    out
}

/// What a title's subtitles need that only the disc's index has.
///
/// The units in the stream say "colour 4" and nothing about what colour four
/// is: a DVD keeps its palette in the index, beside the chain of cells it
/// belongs to. So a cut that means to write the subtitles out has to come
/// back to the disc for this, and it is the one thing here that is read at
/// cutting time rather than when the list was drawn.
pub struct Subtitles {
    /// Every subpicture stream: the substream id the disc names it by, and
    /// the language it declared.
    pub streams: Vec<(u8, Option<String>)>,
    pub palette: crate::vobsub::Palette,
}

/// The subtitles of the title a recording's own name points at.
///
/// `None` for a recording that is not a DVD title, which is every recording
/// whose name does not carry the sectors it plays. See
/// [`crate::input::Input`] for the shape of one.
pub fn subtitles_of(spec: &str) -> Option<Subtitles> {
    let (base, first) = spec.rsplit_once('@').map(|(b, r)| {
        let first = r.split('-').next().and_then(|n| n.parse::<u64>().ok());
        (b, first)
    })?;
    let (root, vts) = title_set_of(base)?;
    let mut vol = Volume::open(Path::new(root)).ok()?;
    let ifo = vol.read(&format!("VTS_{vts:02}_0.IFO")).ok()?;
    if !ifo.starts_with(b"DVDVIDEO-VTS") {
        return None;
    }
    let streams = tracks(&ifo)
        .into_iter()
        .filter(|t| t.kind == "subtitle")
        .map(|t| (t.pid as u8, t.language))
        .collect();
    Some(Subtitles {
        streams,
        palette: palette_of(&ifo, first).unwrap_or_else(crate::vobsub::Palette::grey),
    })
}

/// Split `<disc>/VIDEO_TS/VTS_02_1.VOB` into the disc and the title set
/// number.
///
/// The disc is whatever the path says before `VIDEO_TS`, which is a
/// directory on one and an image on the other; [`Volume::open`] takes either.
fn title_set_of(base: &str) -> Option<(&str, usize)> {
    let upper = base.to_ascii_uppercase();
    let at = upper.rfind("/VIDEO_TS/")?;
    let file = &upper[at + "/VIDEO_TS/".len()..];
    let number = file.strip_prefix("VTS_")?.get(..2)?.parse().ok()?;
    Some((&base[..at], number))
}

/// The palette of the chain that plays the sectors this title starts at.
///
/// A title set can hold several chains and each carries its own sixteen
/// colours, so the one that matters is the one whose cells the title is cut
/// out of. Where the sectors say nothing -- a name with no range on it --
/// the first chain answers, which is right on the great majority of discs
/// and is a colour rather than a failure on the rest.
fn palette_of(ifo: &[u8], first: Option<u64>) -> Option<crate::vobsub::Palette> {
    let table_at = u32be(ifo, 0xcc) as usize * SECTOR as usize;
    let table = ifo.get(table_at..)?;
    let count = u16be(table, 0) as usize;
    let mut fallback = None;
    for i in 0..count {
        let at = 8 + i * 8;
        let start = u32be(table.get(..at + 8)?, at + 4) as usize;
        let g = table.get(start..)?;
        let clut = g.get(0xa4..0xa4 + 64)?;
        let palette = crate::vobsub::Palette::from_clut(clut);
        fallback.get_or_insert(palette);
        let Some(first) = first else { break };
        if read_pgc(g).is_some_and(|pgc| {
            pgc.cells
                .iter()
                .any(|c| c.first <= first && first <= c.last)
        }) {
            return Some(palette);
        }
    }
    fallback
}

/// What a title set says it carries.
///
/// Read from the index and not from the stream, for the same reason a
/// Blu-ray's tracks are: a chooser that had to demux four gigabytes to draw
/// itself is not a chooser anybody waits for.
fn tracks(ifo: &[u8]) -> Vec<Track> {
    let mut out = Vec::new();
    if ifo.len() < 0x256 {
        return out;
    }
    out.push(Track {
        kind: "video",
        // A program stream names its video 0x1E0, and that is the id
        // libavformat reports for it.
        pid: 0x1e0,
        detail: video_detail(&ifo[0x200..0x202]),
        language: None,
        // A DVD has no coding byte to give: its streams are named by the
        // substream id in front of each packet and by nothing else.
        coding: 0,
        carried: true,
    });

    let audios = (ifo[0x203] as usize).min(8);
    for i in 0..audios {
        let a = &ifo[0x204 + i * 8..0x204 + i * 8 + 8];
        let format = a[0] >> 5;
        let (name, base) = match format {
            0 => ("AC-3", 0x80),
            2 => ("MPEG-1 audio", 0xc0),
            3 => ("MPEG-2 audio", 0xc0),
            4 => ("LPCM", 0xa0),
            6 => ("DTS", 0x88),
            _ => ("audio", 0xc0),
        };
        let channels = (a[1] & 7) + 1;
        let rate = if (a[1] >> 4) & 3 == 0 {
            "48kHz"
        } else {
            "96kHz"
        };
        out.push(Track {
            kind: "audio",
            pid: base + i as i32,
            detail: format!("{name} {channels}ch {rate}"),
            language: language(&a[2..4]),
            coding: 0,
            // Everything a DVD calls sound is a stream of timed packets, and
            // a cut carries it the same way it carries a broadcast's.
            carried: true,
        });
    }

    let subs = (ifo[0x255] as usize).min(32);
    for i in 0..subs {
        let s = &ifo[0x256 + i * 6..0x256 + i * 6 + 6];
        out.push(Track {
            kind: "subtitle",
            pid: 0x20 + i as i32,
            detail: "subpicture".to_string(),
            language: language(&s[2..4]),
            coding: 0,
            // A DVD subtitle is a run-length coded picture with its own
            // display commands, the same kind of thing a Blu-ray's graphics
            // are -- but where those have a stream type a transport stream
            // can carry, these have none. So they travel beside the cut
            // instead of inside it; see [`crate::vobsub`].
            carried: true,
        });
    }
    out
}

/// What the index says the picture is, from the two bytes it says it in.
fn video_detail(attr: &[u8]) -> String {
    let mpeg = if attr[0] >> 6 == 0 {
        "MPEG-1"
    } else {
        "MPEG-2"
    };
    let ntsc = (attr[0] >> 4) & 3 == 0;
    let (lines, system) = if ntsc { (480, "NTSC") } else { (576, "PAL") };
    let width = match (attr[1] >> 2) & 3 {
        0 => 720,
        1 => 704,
        _ => 352,
    };
    let height = if (attr[1] >> 2) & 3 == 3 {
        lines / 2
    } else {
        lines
    };
    let aspect = match (attr[0] >> 2) & 3 {
        0 => "4:3",
        3 => "16:9",
        _ => "",
    };
    format!("{mpeg} {width}x{height} {system} {aspect}")
        .trim_end()
        .to_string()
}

/// A two letter language code, when the index wrote one.
fn language(raw: &[u8]) -> Option<String> {
    let code: String = raw.iter().map(|&b| b as char).collect();
    code.chars().all(|c| c.is_ascii_lowercase()).then_some(code)
}

/// A `dvd_time_t`: hours, minutes and seconds in binary coded decimal, and a
/// frame count with the frame rate in its top two bits.
fn dvd_time(b: &[u8], at: usize) -> Option<f64> {
    let t = b.get(at..at + 4)?;
    let bcd = |x: u8| -> Option<f64> {
        let (tens, units) = (x >> 4, x & 0xf);
        (tens <= 9 && units <= 9).then(|| (tens * 10 + units) as f64)
    };
    let rate = match t[3] >> 6 {
        1 => 25.0,
        3 => 30000.0 / 1001.0,
        // A time with no frame rate in it is a time this cannot place to
        // better than a second, and the seconds are still worth having.
        _ => 0.0,
    };
    let frames = bcd(t[3] & 0x3f)?;
    let seconds = bcd(t[0])? * 3600.0 + bcd(t[1])? * 60.0 + bcd(t[2])?;
    Some(if rate > 0.0 {
        seconds + frames / rate
    } else {
        seconds
    })
}

/// Which rows to offer already ticked. See [`WORTH_TICKING`].
fn worth_ticking(lengths: &[f64]) -> Vec<bool> {
    let mut out: Vec<bool> = lengths.iter().map(|d| *d >= WORTH_TICKING).collect();
    if !out.contains(&true) {
        if let Some(i) = lengths
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
        {
            out[i] = true;
        }
    }
    out
}

/// The folder a disc is in, which is where a cut of it can be written.
fn beside(at: &Path) -> String {
    at.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(at)
        .to_string_lossy()
        .into_owned()
}

/// A DVD, whichever of the two shapes it arrived in.
pub enum Volume {
    /// A directory, and the path of the `VIDEO_TS` directory inside it.
    Dir(PathBuf),
    /// An image, the path it was opened from, and the prefix `VIDEO_TS` sits
    /// at inside it.
    Image {
        image: Box<udf::Image>,
        path: PathBuf,
        prefix: String,
    },
}

impl Volume {
    pub fn open(at: &Path) -> Result<Volume> {
        if at.is_dir() {
            let dir = video_ts(at)
                .ok_or_else(|| anyhow!("{}: no VIDEO_TS directory here", at.display()))?;
            return Ok(Volume::Dir(dir));
        }
        let image = udf::Image::open(at)?;
        let prefix = prefix_of(&image)
            .ok_or_else(|| anyhow!("{}: no VIDEO_TS directory on this image", at.display()))?;
        Ok(Volume::Image {
            image: Box::new(image),
            path: at.to_path_buf(),
            prefix,
        })
    }

    /// Read one of the small index files, named relative to `VIDEO_TS`.
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

    /// What to hand [`crate::input`] for a stretch of one title set's stream.
    fn stream(&self, vts: usize, first: u64, last: u64) -> String {
        let rel = format!("VTS_{vts:02}_1.VOB");
        let file = match self {
            Volume::Dir(dir) => at_name(dir, &rel).to_string_lossy().into_owned(),
            Volume::Image { path, prefix, .. } => {
                format!("{}/{prefix}{rel}", path.to_string_lossy())
            }
        };
        format!("{file}@{first}-{last}")
    }

    /// The presentation time of the picture a VOBU begins with.
    ///
    /// Every VOBU opens with a navigation pack: 2048 bytes that a player
    /// reads and does not show, holding among other things the time the
    /// pictures behind it are to be presented at. That is the one thing the
    /// index does not carry and the whole of what is needed to put a chapter
    /// on the clock the demuxer reports.
    fn vobu_time(&mut self, vts: usize, sector: u64) -> Option<f64> {
        let pack = self.at(vts, sector)?;
        vobu_ptm(&pack, 0x0c).map(|t| t as f64 / PTM)
    }

    /// The presentation time a VOBU's pictures stop at.
    ///
    /// The field beside the one [`Volume::vobu_time`] reads, and what says
    /// where a cell leaves the clock -- asked of the cell's last VOBU, which
    /// is the one the index points at for exactly this sort of question.
    fn vobu_end(&mut self, vts: usize, sector: u64) -> Option<f64> {
        let pack = self.at(vts, sector)?;
        vobu_ptm(&pack, 0x10).map(|t| t as f64 / PTM)
    }

    /// One sector of a title set's stream, counting from the stream's start
    /// and not from any one file's.
    fn at(&mut self, vts: usize, sector: u64) -> Option<Vec<u8>> {
        let want = sector * SECTOR;
        let mut before = 0u64;
        for n in 1..=9 {
            let rel = format!("VTS_{vts:02}_{n}.VOB");
            let size = self.size(&rel)?;
            if want < before + size {
                return self.bytes_at(&rel, want - before, SECTOR as usize);
            }
            before += size;
        }
        None
    }

    fn size(&self, rel: &str) -> Option<u64> {
        match self {
            Volume::Dir(dir) => std::fs::metadata(at_name(dir, rel)).ok().map(|m| m.len()),
            Volume::Image { image, prefix, .. } => {
                image.find(&format!("{prefix}{rel}")).map(|e| e.size)
            }
        }
    }

    fn bytes_at(&mut self, rel: &str, at: u64, len: usize) -> Option<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let (path, base) = match self {
            Volume::Dir(dir) => (at_name(dir, rel), 0),
            Volume::Image {
                image,
                path,
                prefix,
            } => {
                let entry = image.find(&format!("{prefix}{rel}"))?;
                (path.clone(), entry.contiguous()?.at)
            }
        };
        let mut f = std::fs::File::open(path).ok()?;
        f.seek(SeekFrom::Start(base + at)).ok()?;
        let mut buf = vec![0u8; len];
        f.read_exact(&mut buf).ok()?;
        Some(buf)
    }

    /// What to call the disc.
    ///
    /// The image or folder's own name, and not the volume identifier inside
    /// it: `DENSHI_RIKKOKU_01` is what a mastering tool was told to write in
    /// 2009, and the file the user has since named the disc after is the name
    /// they will recognise the rows by.
    fn label(&self, at: &Path) -> String {
        at.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "DVD".to_string())
    }
}

/// One of the two times in a navigation pack's general information: the
/// `VOBU_S_PTM` at `0x0C`, or the `VOBU_E_PTM` at `0x10`.
///
/// The pack is a pack header, sometimes a system header behind it -- the
/// first pack of a stream carries one, and this is the first pack of a title
/// -- and then a PES packet on stream `0xBF`, the one the format reserves for
/// navigation. Its first byte says which of the two kinds it is: `0x00` is
/// the presentation control information, and the time the pictures behind it
/// are to be shown at sits twelve bytes into that.
fn vobu_ptm(pack: &[u8], field: usize) -> Option<u32> {
    if pack.len() < SECTOR as usize || pack[..4] != [0x00, 0x00, 0x01, 0xba] {
        return None;
    }
    // A pack header is fourteen bytes and then as much stuffing as its last
    // three bits ask for.
    let mut at = 14 + (pack[13] & 7) as usize;
    // A system header, when there is one, is six bytes of header and as many
    // more as it says.
    if pack.get(at..at + 4)? == [0x00, 0x00, 0x01, 0xbb] {
        at += 6 + u16be(pack.get(at + 4..at + 6)?, 0) as usize;
    }
    if pack.get(at..at + 4)? != [0x00, 0x00, 0x01, 0xbf] {
        return None;
    }
    // Two bytes of PES length, then the substream byte.
    let pci = at + 6;
    if *pack.get(pci)? != 0x00 {
        return None;
    }
    let gi = pci + 1;
    Some(u32be(pack.get(gi..gi + 0x14)?, field))
}

/// The `VIDEO_TS` directory inside a folder, or the folder itself when that
/// is what was pointed at.
fn video_ts(at: &Path) -> Option<PathBuf> {
    for name in ["VIDEO_TS", "video_ts"] {
        let dir = at.join(name);
        if dir.join("VIDEO_TS.IFO").is_file() || dir.join("video_ts.ifo").is_file() {
            return Some(dir);
        }
    }
    let here = at.join("VIDEO_TS.IFO").is_file() || at.join("video_ts.ifo").is_file();
    here.then(|| at.to_path_buf())
}

/// Where inside an image the `VIDEO_TS` directory sits, or `None` when there
/// is none -- which is how an `.iso` that is a Blu-ray is told from one that
/// is a DVD.
fn prefix_of(image: &udf::Image) -> Option<String> {
    image.files().iter().find_map(|e| {
        let upper = e.path.to_ascii_uppercase();
        let at = upper.rfind("VIDEO_TS/VIDEO_TS.IFO")?;
        Some(e.path[..at + "VIDEO_TS/".len()].to_string())
    })
}

/// A file inside `VIDEO_TS`, found whichever case the disc spells it in.
fn at_name(dir: &Path, rel: &str) -> PathBuf {
    let direct = dir.join(rel);
    if direct.exists() {
        return direct;
    }
    let lower = dir.join(rel.to_ascii_lowercase());
    if lower.exists() {
        return lower;
    }
    direct
}

fn u32be(b: &[u8], at: usize) -> u32 {
    let mut v = [0u8; 4];
    v.copy_from_slice(&b[at..at + 4]);
    u32::from_be_bytes(v)
}

fn u16be(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dvd_time_is_binary_coded_decimal_with_the_rate_on_top() {
        // 00:59:59.22 at 30000/1001
        let t = [0x00, 0x59, 0x59, 0xc0 | 0x22];
        let secs = dvd_time(&t, 0).unwrap();
        assert!(
            (secs - (3599.0 + 22.0 / (30000.0 / 1001.0))).abs() < 1e-6,
            "{secs}"
        );
        // 25 fps carries a different pair of top bits.
        let t = [0x01, 0x00, 0x00, 0x40 | 0x12];
        assert!((dvd_time(&t, 0).unwrap() - (3600.0 + 12.0 / 25.0)).abs() < 1e-6);
        // Anything that is not decimal is not a time.
        assert!(dvd_time(&[0x0a, 0x00, 0x00, 0xc0], 0).is_none());
    }

    /// A chain of cells laid out the way the disc this was written against
    /// lays out its first three, with a program starting at each.
    fn chain(spans: &[(u64, u64)]) -> Vec<u8> {
        let mut g = vec![0u8; 0x100 + spans.len() * 24];
        g[2] = spans.len() as u8; // programs
        g[3] = spans.len() as u8; // cells
        g[0xe6..0xe8].copy_from_slice(&0xf0u16.to_be_bytes()); // program map
        g[0xe8..0xea].copy_from_slice(&0x100u16.to_be_bytes()); // cell playback
        for (i, &(first, last)) in spans.iter().enumerate() {
            g[0xf0 + i] = i as u8 + 1;
            let at = 0x100 + i * 24;
            g[at + 4..at + 8].copy_from_slice(&[0x00, 0x01, 0x00, 0xc0]); // one minute
            g[at + 8..at + 12].copy_from_slice(&(first as u32).to_be_bytes());
            g[at + 0x10..at + 0x14].copy_from_slice(&(last as u32).to_be_bytes());
            g[at + 0x14..at + 0x18].copy_from_slice(&(last as u32).to_be_bytes());
        }
        g
    }

    #[test]
    fn a_chain_that_plays_its_cells_in_order_reads() {
        let g = chain(&[(0, 59808), (59809, 170543), (170544, 229979)]);
        let pgc = read_pgc(&g).expect("three cells end to end");
        assert_eq!(pgc.cells.len(), 3);
        assert_eq!(pgc.cells[0].first, 0);
        assert_eq!(pgc.cells[2].last, 229979);
        assert_eq!(pgc.programs, vec![1, 2, 3]);
    }

    #[test]
    fn a_chain_that_jumps_about_is_not_offered_at_all() {
        // A gap between two cells is a chain assembled out of pieces, and the
        // span it lies inside is not the title.
        let g = chain(&[(0, 59808), (170544, 229979)]);
        assert!(read_pgc(&g).is_none());
    }

    #[test]
    fn the_index_reads_the_picture_out_of_two_bytes() {
        // MPEG-2, NTSC, 4:3, 720x480 -- what the disc this was written
        // against says.
        assert_eq!(video_detail(&[0x43, 0x00]), "MPEG-2 720x480 NTSC 4:3");
        // MPEG-2, PAL, 16:9.
        assert_eq!(video_detail(&[0x5d, 0x00]), "MPEG-2 720x576 PAL 16:9");
    }

    #[test]
    fn a_navigation_pack_gives_up_its_start_time() {
        let mut pack = vec![0u8; SECTOR as usize];
        pack[..4].copy_from_slice(&[0x00, 0x00, 0x01, 0xba]);
        pack[13] = 0xf8; // no stuffing
        pack[14..18].copy_from_slice(&[0x00, 0x00, 0x01, 0xbf]);
        pack[20] = 0x00; // presentation control information
        pack[21 + 0x0c..21 + 0x10].copy_from_slice(&31263u32.to_be_bytes());
        assert_eq!(vobu_ptm(&pack, 0x0c), Some(31263));
        // Anything that is not a pack is not a navigation pack.
        assert_eq!(vobu_ptm(&[0u8; 2048], 0x0c), None);
    }

    #[test]
    fn the_first_pack_of_a_title_has_a_system_header_in_the_way() {
        // What the disc this was written against actually holds at sector
        // zero, down to the eighteen byte system header that stands between
        // the pack header and the navigation packet.
        let mut pack = vec![0u8; SECTOR as usize];
        pack[..4].copy_from_slice(&[0x00, 0x00, 0x01, 0xba]);
        pack[13] = 0xf8;
        pack[14..20].copy_from_slice(&[0x00, 0x00, 0x01, 0xbb, 0x00, 0x12]);
        pack[38..42].copy_from_slice(&[0x00, 0x00, 0x01, 0xbf]);
        pack[44] = 0x00;
        pack[45 + 0x0c..45 + 0x10].copy_from_slice(&25257u32.to_be_bytes());
        assert_eq!(vobu_ptm(&pack, 0x0c), Some(25257));
    }
}
