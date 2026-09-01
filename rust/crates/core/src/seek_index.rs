//! What is written down about a recording so that opening it again is cheap.
//!
//! Two passes stand between opening a file and being able to work on it, and
//! neither depends on anything but the file itself:
//!
//!   * the **access-point index** -- every packet read once, in decode order,
//!     to find the entry points and the leading pictures that hang off them.
//!     About a second per gigabyte from cache, far worse from a disc.
//!   * the **thumbnail track** -- every key picture decoded once, for the
//!     pictures the scrubber and the film strip show and for the scene index
//!     that falls out of comparing them. Around ten seconds for half an hour
//!     of 1440x1080 MPEG-2.
//!
//! Both answers are the same every time the same file is opened, so they are
//! kept: this is the seek index. What it buys is the second open and every
//! one after it, which for a recording being cut over several sittings is
//! most of them.
//!
//! It is deliberately not a proxy. Nothing here is re-encoded and nothing
//! stands in for the recording -- the pictures still come from the recording
//! itself. The index only says *where in the file* they are, which is the
//! part that was being worked out again from scratch every time.
//!
//! The file is keyed on the recording's path, size and modification time, so
//! a re-recording under the same name gets its own and never the old one.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::{index, thumbs, AccessPoint};

/// Bumped when a change here would make an existing file wrong. It goes into
/// the cache key, so older ones are simply never looked at again.
pub const VERSION: u32 = 1;

const MAGIC: &[u8; 4] = b"SCIX";

const FLAG_LEADING_KNOWN: u32 = 1 << 0;
const FLAG_PULLDOWN_KNOWN: u32 = 1 << 1;
const FLAG_PULLDOWN: u32 = 1 << 2;
const FLAG_HAS_TRACK: u32 = 1 << 3;

/// Everything a previous open worked out about a recording.
pub struct SeekIndex {
    pub points: Vec<AccessPoint>,
    /// Whether the leading-picture fields were measured or merely assumed.
    pub leading_known: bool,
    /// Whether the stream uses 2:3 pulldown, when the pass that made this
    /// could tell.
    pub pulldown: Option<bool>,
    /// The thumbnail track and scene index, when one was built.
    ///
    /// Optional because the index is worth keeping on its own: a file whose
    /// track failed to build, or was never asked for, still saves the pass
    /// over its packets next time.
    pub track: Option<thumbs::Track>,
}

/// A held index drops straight into [`crate::scan_with`] in place of the
/// walk it came from.
///
/// The seam is [`index::IndexSource`] itself: what the walk produced is
/// exactly what this hands back, so nothing downstream can tell which one
/// answered.
impl index::IndexSource for SeekIndex {
    fn name(&self) -> &'static str {
        "seek index"
    }

    fn build(&self, _input: index::IndexInput) -> Result<index::Index> {
        if self.points.is_empty() {
            bail!("the seek index holds no access points");
        }
        Ok(index::Index {
            points: self.points.clone(),
            leading_known: self.leading_known,
            pulldown: self.pulldown,
        })
    }
}

impl SeekIndex {
    /// The index a finished scan and thumbnail pass amount to.
    pub fn of(src: &crate::Source, track: Option<&thumbs::Track>) -> SeekIndex {
        SeekIndex {
            points: src.points.clone(),
            leading_known: src.leading_known,
            pulldown: Some(src.video.pulldown),
            track: track.map(clone_track),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut w = Writer(Vec::with_capacity(16 + self.points.len() * 40));
        w.0.extend_from_slice(MAGIC);
        w.u32(VERSION);

        let mut flags = 0;
        if self.leading_known {
            flags |= FLAG_LEADING_KNOWN;
        }
        if let Some(p) = self.pulldown {
            flags |= FLAG_PULLDOWN_KNOWN;
            if p {
                flags |= FLAG_PULLDOWN;
            }
        }
        if self.track.is_some() {
            flags |= FLAG_HAS_TRACK;
        }
        w.u32(flags);

        w.u64(self.points.len() as u64);
        for p in &self.points {
            w.f64(p.time);
            w.f64(p.lead_start);
            w.i64(p.pos);
            w.u8(u8::from(p.droppable));
            w.u32(p.lead_indices.len() as u32);
            for i in &p.lead_indices {
                w.u32(*i as u32);
            }
        }

        if let Some(t) = &self.track {
            w.u32(t.width);
            w.f64(t.interval);
            w.f64(t.covered);
            w.f64(t.threshold);
            w.f64(t.typical);
            w.u64(t.scenes.len() as u64);
            for s in &t.scenes {
                w.f64(*s);
            }
            w.u64(t.thumbs.len() as u64);
            for th in &t.thumbs {
                w.f64(th.time);
                w.bytes(&th.jpeg);
            }
        }

        // Through a temporary and renamed into place. This runs to tens of
        // megabytes and the process can be closed while it is writing; a
        // half-written index that still had the right name would be found
        // and trusted on the next open.
        let part = path.with_extension("part.scix");
        std::fs::write(&part, &w.0)
            .with_context(|| format!("cannot write {}", part.display()))?;
        std::fs::rename(&part, path)
            .with_context(|| format!("cannot put {} in place", path.display()))?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<SeekIndex> {
        let raw = std::fs::read(path)?;
        let mut r = Reader { raw: &raw, at: 0 };
        if r.take(4)? != MAGIC {
            bail!("{} is not a seek index", path.display());
        }
        if r.u32()? != VERSION {
            bail!("{} was written by another version", path.display());
        }
        let flags = r.u32()?;

        let n = r.u64()? as usize;
        let mut points = Vec::with_capacity(n);
        for _ in 0..n {
            let time = r.f64()?;
            let lead_start = r.f64()?;
            let pos = r.i64()?;
            let droppable = r.u8()? != 0;
            let leads = r.u32()? as usize;
            let mut lead_indices = Vec::with_capacity(leads);
            for _ in 0..leads {
                lead_indices.push(r.u32()? as usize);
            }
            points.push(AccessPoint { time, lead_start, lead_indices, droppable, pos });
        }

        let track = if flags & FLAG_HAS_TRACK != 0 {
            let width = r.u32()?;
            // Read, but only as the fallback below. The spacing is a
            // property of the pictures themselves, so it is taken from them
            // rather than trusted -- an index written by a version that
            // meant the *floor* by this field then still loads with the
            // right answer, instead of every recording already cached
            // having to be read again to correct one number.
            let stored_interval = r.f64()?;
            let covered = r.f64()?;
            let threshold = r.f64()?;
            let typical = r.f64()?;
            let n = r.u64()? as usize;
            let mut scenes = Vec::with_capacity(n);
            for _ in 0..n {
                scenes.push(r.f64()?);
            }
            let n = r.u64()? as usize;
            let mut list = Vec::with_capacity(n);
            for _ in 0..n {
                let time = r.f64()?;
                let jpeg = r.bytes()?.to_vec();
                list.push(thumbs::Thumb { time, jpeg });
            }
            Some(thumbs::Track {
                width,
                interval: thumbs::spacing(&list).unwrap_or(stored_interval),
                covered,
                thumbs: list,
                scenes,
                threshold,
                typical,
            })
        } else {
            None
        };

        Ok(SeekIndex {
            points,
            leading_known: flags & FLAG_LEADING_KNOWN != 0,
            pulldown: (flags & FLAG_PULLDOWN_KNOWN != 0)
                .then_some(flags & FLAG_PULLDOWN != 0),
            track,
        })
    }
}

/// `Track` is not `Clone`: it is tens of megabytes of JPEG and copying one by
/// accident is not a mistake worth making easy. Saying it here is the one
/// place it is meant.
fn clone_track(t: &thumbs::Track) -> thumbs::Track {
    thumbs::Track {
        width: t.width,
        interval: t.interval,
        covered: t.covered,
        thumbs: t
            .thumbs
            .iter()
            .map(|th| thumbs::Thumb { time: th.time, jpeg: th.jpeg.clone() })
            .collect(),
        scenes: t.scenes.clone(),
        threshold: t.threshold,
        typical: t.typical,
    }
}

/// Where the seek index for `src_path` belongs inside `dir`.
///
/// Keyed by the recording's path, size and modification time, so a file
/// re-recorded under the same name gets a new index rather than the old one.
pub fn cache_path(dir: &Path, src_path: &str) -> Result<PathBuf> {
    let meta = std::fs::metadata(src_path)
        .with_context(|| format!("cannot stat {src_path}"))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    // FNV-1a, as in `proxy::cache_path`: plenty for telling one opened file
    // from another, and it costs no dependency.
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
    eat(&VERSION.to_le_bytes());

    let stem: String = Path::new(src_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .take(40)
        .collect();
    Ok(dir.join(format!("{stem}-{h:016x}.scix")))
}

/// Delete the least recently used indexes in `dir` until at most `keep`
/// remain and they take no more than `budget` bytes between them.
///
/// Same shape as [`crate::proxy::prune`] and for the same reason: what one
/// costs depends on how long the recording is, so a count alone is the wrong
/// limit. These are an order of magnitude smaller than a proxy -- the
/// pictures are 192 pixels wide and there is no video -- so the budget can be
/// smaller and the count much larger, which is the point: the index for a
/// recording finished last week is worth keeping.
///
/// The most recent is never deleted, however far over budget it is on its
/// own: it is almost certainly the recording being cut right now.
pub fn prune(dir: &Path, keep: usize, budget: u64) -> Result<usize> {
    let mut found: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("scix") {
            continue;
        }
        // One still being written is not one of the finished ones.
        if path.to_string_lossy().ends_with(".part.scix") {
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
        if std::fs::remove_file(&path).is_ok() {
            gone += 1;
        }
    }
    Ok(gone)
}

/// Mark a file as used just now, so that the least-recently-used pruning is
/// about use and not about when the file happened to be written.
pub fn touch(path: &Path) {
    let _ = std::fs::OpenOptions::new().write(true).open(path).and_then(|f| {
        f.set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now()))
    });
}

// --- the little-endian plumbing -----------------------------------------

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
    }
}

struct Reader<'a> {
    raw: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or_else(|| anyhow!("seek index is corrupt"))?;
        if end > self.raw.len() {
            bail!("the seek index is truncated");
        }
        let out = &self.raw[self.at..end];
        self.at = end;
        Ok(out)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
}
