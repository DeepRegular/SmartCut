//! Where the files are on a UDF image, and nothing more.
//!
//! A recording burnt to Blu-ray -- or written to an `.iso` and never burnt at
//! all, which is what a disc made by an authoring tool usually is -- puts its
//! streams inside a UDF filesystem. Linux can mount one, but mounting wants
//! root and a loop device, and asking a cut editor to hold a mount for the
//! length of an edit is asking for a stale mount after a crash. Windows
//! mounts an `.iso` by double-clicking it, which is a different set of steps
//! for the user to have taken before the program is any use.
//!
//! Nothing is mounted here. The image is read where it lies, far enough to
//! answer one question: **which byte of the image does each file start at,
//! and how many bytes long is it.** A `.m2ts` written by a burner is one
//! unbroken run of bytes, so the answer is a byte range -- and a byte range
//! is something libavformat can be handed directly, through its `subfile`
//! protocol, with the demuxer none the wiser that it is reading out of an
//! image. Everything downstream then works as it does on a plain file: the
//! packet scan, the seek to an access point, the copy.
//!
//! Only the read side exists, and only the parts of UDF that a video disc
//! actually uses:
//!
//!   * the anchor at sector 256 and the volume descriptors it points at,
//!   * the partition, and the *metadata partition* that UDF 2.50 and later
//!     add -- on which the file entries live while the file data stays on the
//!     plain partition beside it,
//!   * the file set, the directory tree, and each file's allocation
//!     descriptors.
//!
//! **Encrypted discs are not handled and will not be.** AACS is a decryption
//! problem and this program has none of it.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// A UDF logical block is 2048 bytes on every disc format that carries video,
/// and every descriptor is aligned to it.
const SECTOR: u64 = 2048;

/// Where the anchor is required to be. Two more copies live at the end of the
/// volume and are tried after it, so that an image whose tail was truncated
/// still opens.
const ANCHOR: u64 = 256;

// Descriptor tag identifiers, from ECMA-167 parts 3 and 4.
const TAG_ANCHOR: u16 = 2;
const TAG_PARTITION: u16 = 5;
const TAG_LOGICAL_VOLUME: u16 = 6;
const TAG_TERMINATING: u16 = 8;
const TAG_FILE_SET: u16 = 256;
const TAG_FILE_ID: u16 = 257;
const TAG_ALLOCATION_EXTENT: u16 = 258;
const TAG_FILE_ENTRY: u16 = 261;
const TAG_EXTENDED_FILE_ENTRY: u16 = 266;

/// Bits of a file identifier descriptor's characteristics field.
const FID_HIDDEN: u8 = 0x01;
const FID_DIRECTORY: u8 = 0x02;
const FID_DELETED: u8 = 0x04;
const FID_PARENT: u8 = 0x08;

/// How deep the directory tree is followed. BDAV is three levels; anything
/// claiming to be deeper than this on a video disc is a loop or a lie.
const MAX_DEPTH: usize = 8;

/// How many entries are listed before the walk gives up. A disc holds
/// hundreds of clips at the very most.
const MAX_ENTRIES: usize = 20_000;

/// The largest directory this will read into memory, so that a corrupt
/// length cannot ask for a gigabyte.
const MAX_DIR: u64 = 16 << 20;

/// How many times a file's allocation descriptors may be continued
/// elsewhere before the chain is called a loop.
const MAX_CONTINUATIONS: usize = 64;

/// One unbroken run of bytes in the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    /// Byte offset from the start of the image.
    pub at: u64,
    pub len: u64,
}

/// Where a file's bytes are.
#[derive(Debug, Clone)]
pub enum Data {
    /// The bytes are held inside the file's own entry. UDF does this for very
    /// small files, and `info.bdav` is small enough to qualify.
    Inline(Vec<u8>),
    Extents(Vec<Extent>),
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Path from the root of the image, forward slashes, no leading one:
    /// `BDAV/STREAM/00001.m2ts`.
    pub path: String,
    pub size: u64,
    pub data: Data,
}

impl Entry {
    /// The one byte range holding the whole file, when there is one.
    ///
    /// This is what makes a file inside an image openable without unpacking
    /// it. Runs that follow each other exactly are joined, which is the
    /// ordinary case rather than the lucky one: an allocation descriptor
    /// carries 30 bits of length, so no single one of them can describe more
    /// than a gigabyte, and a burner lays the pieces of a larger file down
    /// back to back. A genuinely fragmented file -- which a filesystem
    /// written in place can produce and a burner cannot -- has no single
    /// range, and the caller has to say so rather than read the wrong bytes.
    pub fn contiguous(&self) -> Option<Extent> {
        let Data::Extents(exts) = &self.data else { return None };
        let first = *exts.first()?;
        let mut end = first.at + first.len;
        for e in &exts[1..] {
            if e.at != end {
                return None;
            }
            end += e.len;
        }
        Some(Extent { at: first.at, len: (end - first.at).min(self.size) })
    }

    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// How a logical block number becomes a place in the image.
#[derive(Debug, Clone)]
enum Map {
    /// Blocks sit at a fixed offset into the volume.
    Physical { start: u64 },
    /// Blocks sit inside a file -- the metadata file -- which itself lives on
    /// a physical partition. UDF 2.50 introduced this so that the file
    /// entries of a disc could be kept together and read in one go, away
    /// from the file data; every image written to UDF 2.50 or later has one,
    /// and an image written to 1.02 has none.
    Metadata { extents: Vec<Extent> },
}

/// An opened image: the descriptors have been read, the tree has been walked,
/// and what is left is a list of files and the handle they were found with.
pub struct Image {
    file: File,
    maps: Vec<Map>,
    files: Vec<Entry>,
}

impl Image {
    /// Read the image's directory tree.
    ///
    /// The whole tree is walked once, here, rather than a directory at a time
    /// on demand: a video disc holds tens of files, its descriptors are a few
    /// sectors, and holding the answer means nothing downstream has to think
    /// about UDF again.
    pub fn open(path: &Path) -> Result<Image> {
        let file =
            File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let mut img = Image { file, maps: Vec::new(), files: Vec::new() };
        let root = img
            .read_descriptors()
            .with_context(|| format!("{} is not a UDF image", path.display()))?;
        img.walk(root)?;
        img.files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(img)
    }

    pub fn files(&self) -> &[Entry] {
        &self.files
    }

    /// The entry at that path, matched without regard to case.
    ///
    /// Case is worth ignoring because the discs disagree with each other:
    /// BDAV names the directory `BDAV` and the file `info.bdav`, and
    /// recorders have shipped `INFO.BDAV` for as long as there have been
    /// recorders.
    pub fn find(&self, path: &str) -> Option<&Entry> {
        self.files.iter().find(|e| e.path.eq_ignore_ascii_case(path))
    }

    /// The whole of a small file. For streams, take [`Entry::contiguous`] and
    /// read the range instead of pulling a gigabyte into memory.
    pub fn read(&mut self, entry: &Entry) -> Result<Vec<u8>> {
        self.read_data(&entry.data, entry.size)
    }

    fn read_data(&mut self, data: &Data, size: u64) -> Result<Vec<u8>> {
        match data {
            Data::Inline(bytes) => Ok(bytes.clone()),
            Data::Extents(exts) => {
                let mut out = Vec::with_capacity(size.min(MAX_DIR) as usize);
                for e in exts {
                    if out.len() as u64 >= size {
                        break;
                    }
                    let take = e.len.min(size - out.len() as u64);
                    out.extend(self.read_runs(&[Extent { at: e.at, len: take }])?);
                }
                Ok(out)
            }
        }
    }

    fn sectors(&mut self, at: u64, count: u64) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; (count * SECTOR) as usize];
        self.file.seek(SeekFrom::Start(at * SECTOR))?;
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Find the anchor, then the volume descriptors it points at, and from
    /// them the partition maps and the root directory.
    fn read_descriptors(&mut self) -> Result<LongAd> {
        let end = self.file.seek(SeekFrom::End(0))? / SECTOR;
        if end < ANCHOR {
            bail!("too short to be an image");
        }
        // The three places the standard allows an anchor. The last two are
        // what a disc written by a drive uses.
        let candidates = [ANCHOR, end.saturating_sub(1), end.saturating_sub(1 + ANCHOR)];
        let anchor = candidates
            .into_iter()
            .filter(|at| *at > 0 && *at < end)
            .find_map(|at| {
                self.sectors(at, 1).ok().filter(|s| tag_id(s) == Some(TAG_ANCHOR))
            })
            .ok_or_else(|| anyhow!("no anchor descriptor"))?;

        let main = extent_ad(&anchor, 16);
        let reserve = extent_ad(&anchor, 24);
        // A main sequence that does not read is exactly what the reserve copy
        // is there for.
        let seq = match self.volume_sequence(main) {
            Ok(seq) if seq.found() => seq,
            _ => {
                let seq = self.volume_sequence(reserve)?;
                if !seq.found() {
                    bail!("no logical volume descriptor");
                }
                seq
            }
        };
        let (parts, lvd) = (seq.parts, seq.lvd);

        if u32le(&lvd, 212) as u64 != SECTOR {
            bail!("logical block size is not {SECTOR}");
        }
        self.maps = self.partition_maps(&lvd, &parts)?;

        let fsd_ad = long_ad(&lvd, 248);
        let fsd = self.read_blocks(fsd_ad.partition, fsd_ad.block, 1)?;
        if tag_id(&fsd) != Some(TAG_FILE_SET) {
            bail!("the file set is not where the volume says it is");
        }
        Ok(long_ad(&fsd, 400))
    }

    /// Read one run of volume descriptors: the partitions and the logical
    /// volume descriptor are all this needs from it.
    fn volume_sequence(&mut self, at: (u64, u64)) -> Result<Sequence> {
        let (len, loc) = at;
        let mut seq = Sequence { parts: Vec::new(), lvd: Vec::new() };
        for i in 0..(len / SECTOR).min(64) {
            let Ok(sec) = self.sectors(loc + i, 1) else { break };
            match tag_id(&sec) {
                Some(TAG_PARTITION) => {
                    seq.parts.push((u16le(&sec, 22), u32le(&sec, 188) as u64))
                }
                Some(TAG_LOGICAL_VOLUME) if seq.lvd.is_empty() => seq.lvd = sec,
                Some(TAG_TERMINATING) | None => break,
                _ => {}
            }
        }
        Ok(seq)
    }

    /// The partition maps, in the order the logical volume lists them --
    /// which is the order a partition reference number counts in.
    fn partition_maps(&mut self, lvd: &[u8], parts: &[(u16, u64)]) -> Result<Vec<Map>> {
        let table_len = u32le(lvd, 264) as usize;
        let count = u32le(lvd, 268) as usize;
        let table = lvd
            .get(440..440 + table_len)
            .ok_or_else(|| anyhow!("partition map table runs past the descriptor"))?
            .to_vec();
        let start_of = |number: u16| -> Result<u64> {
            parts
                .iter()
                .find(|(n, _)| *n == number)
                .map(|(_, s)| *s)
                .ok_or_else(|| anyhow!("the volume names partition {number}, which is not there"))
        };

        let mut maps = Vec::new();
        let mut at = 0usize;
        for _ in 0..count.min(16) {
            let (kind, len) = match (table.get(at), table.get(at + 1)) {
                (Some(k), Some(l)) if *l as usize >= 2 => (*k, *l as usize),
                _ => break,
            };
            match kind {
                // Type 1: the volume sequence number, and then the
                // partition this reference means.
                1 => maps.push(Map::Physical { start: start_of(u16le(&table, at + 4))? }),
                2 => {
                    let ident = &table[at + 4..at + 36];
                    let number = u16le(&table, at + 38);
                    let start = start_of(number)?;
                    if contains(ident, b"*UDF Metadata Partition") {
                        // The metadata file's own entry sits on the physical
                        // partition; everything else on this partition is
                        // found by walking through it.
                        let block = u32le(&table, at + 40) as u64;
                        let fe = self.sectors(start + block, 1)?;
                        let (_, data) = self.file_data(&fe, Map::Physical { start })?;
                        let Data::Extents(extents) = data else {
                            bail!("the metadata file has no extents")
                        };
                        maps.push(Map::Metadata { extents });
                    } else {
                        // Virtual and sparable partitions are for rewritable
                        // and write-once media that this will not be reading:
                        // an image on a disk has neither.
                        bail!(
                            "unsupported partition map: {}",
                            String::from_utf8_lossy(ident).trim_end_matches('\0')
                        );
                    }
                }
                other => bail!("unknown partition map type {other}"),
            }
            at += len;
        }
        if maps.is_empty() {
            bail!("no partition maps");
        }
        Ok(maps)
    }

    /// Read whole logical blocks of a partition.
    fn read_blocks(&mut self, part: u16, block: u64, count: u64) -> Result<Vec<u8>> {
        let map = self.map(part)?.clone();
        let runs = byte_runs(&map, block, count * SECTOR)?;
        self.read_runs(&runs)
    }

    fn map(&self, part: u16) -> Result<&Map> {
        self.maps
            .get(part as usize)
            .ok_or_else(|| anyhow!("this image has no partition {part}"))
    }

    /// Turn a file entry into the length and the whereabouts of its bytes.
    ///
    /// `home` is the map the entry itself was read from, because a short
    /// allocation descriptor names no partition: it means the one the
    /// descriptor is recorded on. That is what keeps a directory's contents
    /// on the metadata partition while a stream's contents are on the
    /// physical one -- the stream's entry names its partition and the
    /// directory's does not.
    fn file_data(&mut self, fe: &[u8], home: Map) -> Result<(u64, Data)> {
        let (size, ad_at, ad_len, kind) = match tag_id(fe) {
            Some(TAG_FILE_ENTRY) => {
                let ea = u32le(fe, 168) as usize;
                (u64le(fe, 56), 176 + ea, u32le(fe, 172) as usize, u16le(fe, 34) & 7)
            }
            Some(TAG_EXTENDED_FILE_ENTRY) => {
                let ea = u32le(fe, 208) as usize;
                (u64le(fe, 56), 216 + ea, u32le(fe, 212) as usize, u16le(fe, 34) & 7)
            }
            other => bail!("expected a file entry, found tag {other:?}"),
        };
        let ads = fe
            .get(ad_at..ad_at + ad_len)
            .ok_or_else(|| anyhow!("allocation descriptors run past the file entry"))?;

        // Type 3 keeps the bytes in the entry itself, which is how UDF stores
        // a file too small to be worth a block of its own.
        if kind == 3 {
            let mut bytes = ads.to_vec();
            bytes.truncate(size as usize);
            return Ok((size, Data::Inline(bytes)));
        }

        let step = match kind {
            0 => 8,  // short_ad
            1 => 16, // long_ad
            2 => 20, // ext_ad
            other => bail!("unknown allocation descriptor type {other}"),
        };
        // The descriptors can be continued elsewhere -- it takes a very
        // fragmented file, but the format allows it -- so this is a loop over
        // runs of descriptors rather than over one run.
        let mut out = Vec::new();
        let mut chunk = ads.to_vec();
        for _ in 0..MAX_CONTINUATIONS {
            let mut carry_on = None;
            let mut at = 0usize;
            while at + step <= chunk.len() {
                let raw = u32le(&chunk, at);
                let len = (raw & 0x3fff_ffff) as u64;
                if len == 0 {
                    break;
                }
                let block = u32le(&chunk, at + 4) as u64;
                let map = match kind {
                    0 => home.clone(),
                    _ => self.map(u16le(&chunk, at + 8))?.clone(),
                };
                match raw >> 30 {
                    // Recorded and allocated: the only kind that holds bytes.
                    0 => out.extend(byte_runs(&map, block, len)?),
                    // Allocated but not recorded, or neither: a hole. A
                    // stream with one is not a stream to hand to a demuxer,
                    // and saying so is better than reading zeros as pictures.
                    1 | 2 => bail!("the file has an unwritten hole in it"),
                    // More descriptors, over there.
                    _ => {
                        carry_on = Some((map, block, len));
                        break;
                    }
                }
                at += step;
            }
            let Some((map, block, len)) = carry_on else { break };
            let more = self.read_runs(&byte_runs(&map, block, len)?)?;
            // An allocation extent descriptor is a tag, the location of the
            // descriptors that led here, and the length of the ones carried.
            if tag_id(&more) != Some(TAG_ALLOCATION_EXTENT) || more.len() < 24 {
                bail!("the allocation descriptors are continued nowhere");
            }
            let carried = u32le(&more, 20) as usize;
            chunk = more[24..].get(..carried).unwrap_or(&more[24..]).to_vec();
        }
        Ok((size, Data::Extents(out)))
    }

    fn read_runs(&mut self, runs: &[Extent]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for run in runs {
            self.file.seek(SeekFrom::Start(run.at))?;
            let mut buf = vec![0u8; run.len as usize];
            self.file.read_exact(&mut buf)?;
            out.extend_from_slice(&buf);
        }
        Ok(out)
    }

    /// Walk the tree from the root, listing every file under it.
    fn walk(&mut self, root: LongAd) -> Result<()> {
        let mut seen: HashSet<(u16, u64)> = HashSet::new();
        let mut queue = vec![(String::new(), root, 0usize)];
        while let Some((prefix, icb, depth)) = queue.pop() {
            if depth > MAX_DEPTH || self.files.len() >= MAX_ENTRIES {
                continue;
            }
            if !seen.insert((icb.partition, icb.block)) {
                continue;
            }
            let fe = self.read_blocks(icb.partition, icb.block, 1)?;
            let home = self.map(icb.partition)?.clone();
            let (size, data) = self.file_data(&fe, home)?;
            if size > MAX_DIR {
                bail!("a directory of {size} bytes is not a directory");
            }
            let dir = self.read_data(&data, size)?;

            for (name, chars, child) in file_ids(&dir) {
                if chars & (FID_PARENT | FID_DELETED) != 0 {
                    continue;
                }
                let path =
                    if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
                if chars & FID_DIRECTORY != 0 {
                    queue.push((path, child, depth + 1));
                    continue;
                }
                // A hidden file on a video disc is a recorder's own
                // bookkeeping, and listing it invites opening it.
                if chars & FID_HIDDEN != 0 {
                    continue;
                }
                let fe = self.read_blocks(child.partition, child.block, 1)?;
                let home = self.map(child.partition)?.clone();
                let (size, data) = self.file_data(&fe, home)?;
                self.files.push(Entry { path, size, data });
            }
        }
        Ok(())
    }
}

/// The runs of bytes `len` bytes of a partition, starting at `block`, occupy
/// in the image.
///
/// One run, on a plain partition. On a metadata partition it can be several,
/// because the metadata file is itself a file and may be laid down in pieces.
fn byte_runs(map: &Map, block: u64, len: u64) -> Result<Vec<Extent>> {
    match map {
        Map::Physical { start } => Ok(vec![Extent { at: (start + block) * SECTOR, len }]),
        Map::Metadata { extents } => {
            let mut want = block * SECTOR;
            let mut left = len;
            let mut out = Vec::new();
            for e in extents {
                if want >= e.len {
                    want -= e.len;
                    continue;
                }
                let take = (e.len - want).min(left);
                out.push(Extent { at: e.at + want, len: take });
                left -= take;
                want = 0;
                if left == 0 {
                    return Ok(out);
                }
            }
            Err(anyhow!("block {block} is past the end of the metadata file"))
        }
    }
}

/// The entries of one directory: name, characteristics, and where the entry
/// for it is.
fn file_ids(dir: &[u8]) -> Vec<(String, u8, LongAd)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 38 <= dir.len() {
        if tag_id(&dir[at..]) != Some(TAG_FILE_ID) {
            break;
        }
        let chars = dir[at + 18];
        let name_len = dir[at + 19] as usize;
        let icb = long_ad(dir, at + 20);
        let iu_len = u16le(dir, at + 36) as usize;
        let name_at = at + 38 + iu_len;
        let Some(raw) = dir.get(name_at..name_at + name_len) else { break };
        out.push((dstring(raw), chars, icb));
        // Every descriptor starts on a four byte boundary.
        at = (name_at + name_len + 3) & !3;
    }
    out
}

/// A UDF file name, which is one byte of encoding followed by the name.
///
/// Two encodings exist and both turn up: 8 is one byte a character and 16 is
/// UTF-16 big endian, which is what a name outside ASCII arrives in. BDAV's
/// own names are digits and a dot, but the volume can hold anything.
fn dstring(raw: &[u8]) -> String {
    match raw.first() {
        Some(8) => raw[1..].iter().map(|b| *b as char).collect(),
        Some(16) => {
            let body = &raw[1..];
            let mut units = Vec::with_capacity(body.len() / 2);
            let mut at = 0;
            while at + 1 < body.len() {
                units.push(u16::from_be_bytes([body[at], body[at + 1]]));
                at += 2;
            }
            String::from_utf16_lossy(&units)
        }
        _ => String::new(),
    }
}

/// What one run of volume descriptors said: the partitions, by number and
/// starting sector, and the logical volume descriptor itself.
struct Sequence {
    parts: Vec<(u16, u64)>,
    lvd: Vec<u8>,
}

impl Sequence {
    fn found(&self) -> bool {
        !self.parts.is_empty() && !self.lvd.is_empty()
    }
}

/// A long allocation descriptor: 30 bits of length, a block, a partition.
#[derive(Debug, Clone, Copy)]
struct LongAd {
    block: u64,
    partition: u16,
}

fn long_ad(b: &[u8], at: usize) -> LongAd {
    LongAd { block: u32le(b, at + 4) as u64, partition: u16le(b, at + 8) }
}

fn extent_ad(b: &[u8], at: usize) -> (u64, u64) {
    (u32le(b, at) as u64, u32le(b, at + 4) as u64)
}

fn tag_id(b: &[u8]) -> Option<u16> {
    (b.len() >= 16).then(|| u16le(b, 0)).filter(|id| *id != 0)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u64le(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(runs: &[(u64, u64)], size: u64) -> Entry {
        Entry {
            path: "BDAV/STREAM/00001.m2ts".into(),
            size,
            data: Data::Extents(runs.iter().map(|(at, len)| Extent { at: *at, len: *len }).collect()),
        }
    }

    #[test]
    fn joins_the_pieces_a_gigabyte_limit_forces() {
        // What a burner writes for a 1.15 GB stream: one full descriptor and
        // the remainder, laid down back to back.
        let e = entry(&[(747_520, 1_073_739_776), (1_074_487_296, 158_107_648)], 1_231_847_424);
        assert_eq!(e.contiguous(), Some(Extent { at: 747_520, len: 1_231_847_424 }));
    }

    #[test]
    fn refuses_a_file_written_in_pieces() {
        let e = entry(&[(2048, 4096), (65536, 4096)], 8192);
        assert_eq!(e.contiguous(), None);
    }

    #[test]
    fn a_metadata_partition_maps_through_its_own_file() {
        let map = Map::Metadata {
            extents: vec![Extent { at: 655_360, len: 4096 }, Extent { at: 1_000_000, len: 4096 }],
        };
        // Second block of the metadata file.
        assert_eq!(byte_runs(&map, 1, SECTOR).unwrap(), vec![Extent { at: 657_408, len: 2048 }]);
        // Third block, which is where the file's own second piece begins.
        assert_eq!(byte_runs(&map, 2, SECTOR).unwrap(), vec![Extent { at: 1_000_000, len: 2048 }]);
        // A run that crosses the join comes back as the two pieces it is.
        assert_eq!(
            byte_runs(&map, 1, 4096).unwrap(),
            vec![Extent { at: 657_408, len: 2048 }, Extent { at: 1_000_000, len: 2048 }]
        );
    }

    #[test]
    fn reads_both_spellings_of_a_name() {
        assert_eq!(dstring(&[8, b'B', b'D', b'A', b'V']), "BDAV");
        assert_eq!(dstring(&[16, 0x30, 0xa2]), "ア");
        assert_eq!(dstring(&[]), "");
    }
}
