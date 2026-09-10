//! Writing a UDF image around a folder of files.
//!
//! The other direction from [`crate::udf`], and for the same reason
//! [`crate::bdav`] exists: a disc of recordings is not finished as a folder.
//! What a burner wants is an image, and what an image is here is a UDF 2.50
//! or 2.60 filesystem with the `BDAV` directory inside it -- the shape a
//! Blu-ray recorder writes, and the shape ImgBurn produces when it is handed
//! the folder.
//!
//! ## Written against two real images
//!
//! There is no specification in this project. Every offset, every constant
//! and every field this fills in was read off two images written by real
//! tools -- the same two discs [`crate::bdav`] was written against -- and the
//! writer's output is read back by [`crate::udf`], which is the reader those
//! discs are already opened with. Where the two images agree, this writes
//! what they have; where they differ, this writes what the newer one has.
//!
//! ## What the image is made of
//!
//! ```text
//!   0..15   nothing: the system area an ISO9660 volume would use
//!  16..18   BEA01, NSR03, TEA01 -- "there is a UDF volume in here"
//!  32..47   the volume descriptors: primary, implementation use, partition,
//!           logical volume, unallocated space, terminating
//!  64..65   the logical volume integrity descriptor, and a terminator
//!     256   the anchor, which is the one descriptor at a fixed place
//!     288   the partition:
//!             +0    the metadata file's own entry
//!             +1    the mirror's
//!             +32   the metadata partition itself -- every file entry, every
//!                   directory, and the file set descriptor
//!             ...   the files, each starting on a 32 block boundary
//!             end   a second copy of the metadata partition
//!    then   the reserve copy of the volume descriptors, and a second anchor
//!           in the last sector
//! ```
//!
//! ## The metadata partition
//!
//! UDF 2.50 put the file entries somewhere of their own: a *metadata
//! partition*, whose blocks are the contents of an ordinary file on the
//! partition beside it. A player reading a disc then finds the directory
//! tree in one place on the disc rather than spread through it, and the file
//! data stays where a burner laid it down. Both reference images use one, and
//! [`crate::udf`] already reads through it -- see its module documentation.
//!
//! It is what decides how a file entry names its data. A short allocation
//! descriptor means "the partition this descriptor is recorded on", which for
//! an entry living in the metadata partition is the metadata partition -- so
//! a directory, whose contents are there too, uses short descriptors, and a
//! file, whose contents are on the partition beside it, uses long ones and
//! names that partition. Both reference images do exactly this.

use anyhow::{bail, Context, Result};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// A UDF logical block, which is 2048 bytes on every disc that carries video.
const SECTOR: usize = 2048;

/// Where the anchor is required to be, and where the partition begins. Both
/// are what the reference images use.
const ANCHOR: u64 = 256;
const PARTITION: u64 = 288;

/// The volume descriptors, and the integrity descriptor after them.
const MAIN_VDS: u64 = 32;
const VDS_BLOCKS: u64 = 16;
const INTEGRITY: u64 = 64;
const INTEGRITY_BLOCKS: u64 = 2;

/// Where the two entries that describe the metadata partition sit, in blocks
/// from the start of the partition. The first two blocks of it, which is
/// where the reference images have the first of them.
const METADATA_FILE_ENTRY: u32 = 0;
const METADATA_MIRROR_ENTRY: u32 = 1;

/// How the metadata partition and the file data are aligned inside the
/// partition, in blocks. 32 blocks is 64 KB, which is what both reference
/// images use and what the partition map then has to declare.
const ALIGN: u64 = 32;

/// The largest a single allocation descriptor may describe: its length field
/// is 30 bits, and a length that is not the last of a file has to be a whole
/// number of blocks. This is the same 1,073,739,776 the reader sees on discs
/// written by burners.
const EXTENT_MAX: u64 = ((1u64 << 30) - 1) & !(SECTOR as u64 - 1);

// Descriptor tags, from ECMA-167 parts 3 and 4.
const TAG_PRIMARY: u16 = 1;
const TAG_ANCHOR: u16 = 2;
const TAG_IMPL_USE: u16 = 4;
const TAG_PARTITION: u16 = 5;
const TAG_LOGICAL_VOLUME: u16 = 6;
const TAG_UNALLOCATED: u16 = 7;
const TAG_TERMINATING: u16 = 8;
const TAG_INTEGRITY: u16 = 9;
const TAG_FILE_SET: u16 = 256;
const TAG_FILE_ID: u16 = 257;
const TAG_EXTENDED_FILE_ENTRY: u16 = 266;

/// The kinds of file an entry can be. 250 and 251 are the metadata file and
/// its mirror, which is how a reader tells them from the files a person put
/// on the disc.
const FILE_DIRECTORY: u8 = 4;
const FILE_ORDINARY: u8 = 5;
const FILE_METADATA: u8 = 250;
const FILE_METADATA_MIRROR: u8 = 251;

/// Which revision of UDF the image says it is.
///
/// The two differ in one number written into half a dozen places, and in
/// nothing else this writes: 2.60 adds a way of rewriting a disc that has
/// already been written once, which an image on a hard disk has no use for.
/// Both are offered because a burner and a player may each be happier with
/// one of them, and neither is more work than the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revision {
    V250,
    V260,
}

impl Revision {
    /// The number as UDF writes it: 0x0250 for 2.50.
    fn number(self) -> u16 {
        match self {
            Revision::V250 => 0x0250,
            Revision::V260 => 0x0260,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Revision::V250 => "2.50",
            Revision::V260 => "2.60",
        }
    }

    pub fn parse(s: &str) -> Option<Revision> {
        match s.trim() {
            "2.50" | "2.5" | "250" => Some(Revision::V250),
            "2.60" | "2.6" | "260" => Some(Revision::V260),
            _ => None,
        }
    }
}

/// One node of the tree being written.
struct Node {
    name: String,
    /// Where the file's bytes are read from. `None` for a directory.
    source: Option<PathBuf>,
    size: u64,
    children: Vec<usize>,
    /// Block of this node's file entry, in the metadata partition.
    entry: u32,
    /// Block of its contents: in the metadata partition for a directory,
    /// in the partition beside it for a file.
    data: u32,
    /// How many blocks those contents take.
    blocks: u64,
    /// The number this file is known by on the volume. Nought is the root
    /// and the first fifteen are reserved, so everything else counts from
    /// sixteen -- which is where the reference images start too.
    unique: u32,
}

impl Node {
    fn is_dir(&self) -> bool {
        self.source.is_none()
    }
}

/// Write the folder at `from` into a UDF image at `to`.
///
/// `label` is what the volume is called -- what a machine that mounts the
/// image shows. `on` is told how far through the copy it is, because a disc
/// of recordings is twenty gigabytes of file data.
pub fn write(
    from: &Path,
    to: &Path,
    revision: Revision,
    label: &str,
    on: Option<&(dyn Fn(f64) + Sync)>,
) -> Result<u64> {
    let mut left_out = Vec::new();
    let mut tree = read_tree(from, &mut left_out)?;
    say_what_was_left_out(from, &left_out);
    if tree.len() < 2 {
        bail!("{}: there is nothing here to put on a disc", from.display());
    }
    let now = Stamp::now();
    let plan = lay_out(&mut tree);
    let meta = metadata_image(&tree, &plan, revision, &now, label)?;

    let dst =
        std::fs::File::create(to).with_context(|| format!("cannot write {}", to.display()))?;
    let mut out = Writer {
        to: BufWriter::with_capacity(1 << 20, dst),
        at: 0,
    };

    // Everything in front of the partition: the recognition sequence that
    // says there is a volume here at all, the descriptors that describe it,
    // and the anchor that points at them.
    out.pad_to(16)?;
    for name in [b"BEA01", b"NSR03", b"TEA01"] {
        // A structure type of nought, the identifier, and version 1. What is
        // being said is only that a volume of this kind begins here.
        let mut sec = vec![0u8; SECTOR];
        sec[1..6].copy_from_slice(name);
        sec[6] = 1;
        out.sector(&sec)?;
    }
    out.pad_to(MAIN_VDS)?;
    for d in volume_descriptors(&plan, revision, &now, label, MAIN_VDS) {
        out.sector(&sized(&d)?)?;
    }
    out.pad_to(INTEGRITY)?;
    out.sector(&sized(&integrity(&plan, revision, &now))?)?;
    out.sector(&sized(&terminating(INTEGRITY + 1))?)?;
    out.pad_to(ANCHOR)?;
    out.sector(&sized(&anchor(&plan, ANCHOR))?)?;

    // The partition. The two entries that describe the metadata partition
    // come first, then the metadata partition itself, then the files.
    out.pad_to(PARTITION)?;
    out.sector(&sized(&metadata_entry(
        FILE_METADATA,
        plan.meta_at,
        plan.meta_blocks,
        &now,
    ))?)?;
    out.sector(&sized(&metadata_entry(
        FILE_METADATA_MIRROR,
        plan.mirror_at,
        plan.meta_blocks,
        &now,
    ))?)?;
    out.pad_to(PARTITION + plan.meta_at as u64)?;
    out.write_all(&meta)?;

    let total: u64 = tree.iter().map(|n| n.size).sum();
    let mut done = 0u64;
    for node in tree.iter().filter(|n| !n.is_dir()) {
        out.pad_to(PARTITION + node.data as u64)?;
        let path = node.source.as_ref().expect("a file has a path");
        let mut src =
            std::fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
        let mut buf = vec![0u8; 1 << 20];
        let mut left = node.size;
        while left > 0 {
            let want = buf.len().min(left as usize);
            src.read_exact(&mut buf[..want])
                .with_context(|| format!("reading {}", path.display()))?;
            out.write_all(&buf[..want])?;
            left -= want as u64;
            done += want as u64;
            if let Some(f) = on {
                f(done as f64 / total.max(1) as f64);
            }
        }
        out.align()?;
    }

    // The second copy of the metadata partition, and then everything that
    // has to be at the end: the reserve descriptors and the second anchor.
    out.pad_to(PARTITION + plan.mirror_at as u64)?;
    out.write_all(&meta)?;
    out.pad_to(plan.reserve_vds)?;
    for d in volume_descriptors(&plan, revision, &now, label, plan.reserve_vds) {
        out.sector(&sized(&d)?)?;
    }
    out.pad_to(plan.sectors - 1)?;
    out.sector(&sized(&anchor(&plan, plan.sectors - 1))?)?;
    out.to.flush()?;
    Ok(plan.sectors * SECTOR as u64)
}

/// Name what could not go on the volume.
///
/// A folder is whatever the caller named, and an image quietly missing a file
/// out of one is worse than an image that was not made: nothing downstream
/// will ever say which file, or that there was one.
fn say_what_was_left_out(from: &Path, left_out: &[String]) {
    if left_out.is_empty() {
        return;
    }
    // Named the way the folder names them, rather than by the path this
    // program happens to have been handed.
    let named: Vec<String> = left_out
        .iter()
        .take(4)
        .map(|p| {
            Path::new(p)
                .strip_prefix(from)
                .unwrap_or(Path::new(p))
                .to_string_lossy()
                .into_owned()
        })
        .chain((left_out.len() > 4).then(|| "...".to_string()))
        .collect();
    eprintln!(
        "note: {} file(s) under {} are not named the way a disc names its files -- a UDF volume \
         written here carries plain ASCII names of 200 characters or fewer -- and are not in \
         the image: {}",
        left_out.len(),
        from.display(),
        named.join(", "),
    );
}

/// Where everything goes, in blocks.
struct Plan {
    /// Blocks of the metadata partition, and where its two copies sit in the
    /// partition beside it.
    meta_blocks: u64,
    meta_at: u32,
    mirror_at: u32,
    /// How long the partition is, and where the reserve descriptors and the
    /// end of the image are.
    partition_blocks: u64,
    reserve_vds: u64,
    sectors: u64,
    files: u32,
    directories: u32,
    /// The number the next file written onto this volume would take.
    next_unique: u32,
}

/// Read the folder into the tree that will be written.
///
/// Sorted case-insensitively, which is the order both reference images list
/// a directory in -- and the order a person reading `CLIPINF`, `info.bdav`,
/// `PLAYLIST`, `STREAM` expects.
///
/// `left_out` collects what could not go on the volume, so the caller can
/// say so. A disc of recordings has none of it -- the files on one are
/// `00001.m2ts` and `info.bdav` -- but the folder handed over is whatever
/// the caller named, and an image quietly missing a file is worse than one
/// that was not made.
fn read_tree(from: &Path, left_out: &mut Vec<String>) -> Result<Vec<Node>> {
    let mut tree = vec![Node {
        name: String::new(),
        source: None,
        size: 0,
        children: Vec::new(),
        entry: 0,
        data: 0,
        blocks: 0,
        unique: 0,
    }];
    fill(from, 0, &mut tree, left_out)?;
    Ok(tree)
}

fn fill(dir: &Path, parent: usize, tree: &mut Vec<Node>, left_out: &mut Vec<String>) -> Result<()> {
    let mut entries: Vec<(String, PathBuf, bool, u64)> = Vec::new();
    for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        // A name that is not plain ASCII is not something a disc of
        // recordings has: the files on one are `00001.m2ts` and `info.bdav`.
        // A dot-file is not on one either. Both are noted rather than simply
        // skipped -- see `left_out`.
        if name.starts_with('.') || !name.is_ascii() || name.len() > 200 {
            if !name.starts_with('.') {
                left_out.push(dir.join(&name).to_string_lossy().into_owned());
            }
            continue;
        }
        let meta = e.metadata()?;
        entries.push((name, e.path(), meta.is_dir(), meta.len()));
    }
    entries.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    for (name, path, is_dir, size) in entries {
        let at = tree.len();
        tree.push(Node {
            name,
            source: (!is_dir).then(|| path.clone()),
            size: if is_dir { 0 } else { size },
            children: Vec::new(),
            entry: 0,
            data: 0,
            blocks: 0,
            unique: 0,
        });
        tree[parent].children.push(at);
        if is_dir {
            fill(&path, at, tree, left_out)?;
        }
    }
    Ok(())
}

/// Give every node its blocks, and the image its length.
fn lay_out(tree: &mut [Node]) -> Plan {
    // What a directory's contents come to: the entry for its parent, and one
    // for each thing in it.
    for i in 0..tree.len() {
        if !tree[i].is_dir() {
            continue;
        }
        let mut len = fid_len(0);
        for c in tree[i].children.clone() {
            len += fid_len(tree[c].name.len());
        }
        tree[i].size = len as u64;
        tree[i].blocks = blocks(len as u64);
    }

    // Every file is numbered as the tree is walked, the root being nought.
    for (i, node) in tree.iter_mut().enumerate() {
        node.unique = if i == 0 { 0 } else { 15 + i as u32 };
    }

    // The metadata partition: the file set descriptor and a terminator, then
    // every entry and every directory, depth first.
    let mut next = 2u32;
    let mut order = vec![0usize];
    while let Some(i) = order.pop() {
        tree[i].entry = next;
        next += 1;
        if tree[i].is_dir() {
            tree[i].data = next;
            next += tree[i].blocks as u32;
            // Reversed, so that the tree is walked in the order the entries
            // were read rather than backwards.
            for c in tree[i].children.clone().into_iter().rev() {
                order.push(c);
            }
        }
    }
    let meta_blocks = align(next as u64);

    // The partition: the two entries that describe the metadata partition,
    // then the metadata partition, then the files, then its second copy.
    let meta_at = align(2) as u32;
    let mut at = align(meta_at as u64 + meta_blocks);
    for i in 0..tree.len() {
        if tree[i].is_dir() {
            continue;
        }
        tree[i].data = at as u32;
        tree[i].blocks = blocks(tree[i].size);
        at = align(at + tree[i].blocks);
    }
    let mirror_at = at as u32;
    let partition_blocks = at + meta_blocks;
    let reserve_vds = PARTITION + partition_blocks;

    Plan {
        meta_blocks,
        meta_at,
        mirror_at,
        partition_blocks,
        reserve_vds,
        sectors: reserve_vds + VDS_BLOCKS + 1,
        files: tree.iter().filter(|n| !n.is_dir()).count() as u32,
        directories: tree.iter().filter(|n| n.is_dir()).count() as u32,
        next_unique: 15 + tree.len() as u32,
    }
}

fn blocks(bytes: u64) -> u64 {
    bytes.div_ceil(SECTOR as u64)
}

fn align(blocks: u64) -> u64 {
    blocks.div_ceil(ALIGN) * ALIGN
}

/// How long a file identifier descriptor is, name and padding included.
fn fid_len(name: usize) -> usize {
    let len = 38 + if name == 0 { 0 } else { name + 1 };
    (len + 3) & !3
}

// --- the metadata partition ---------------------------------------------

/// Everything that lives inside the metadata file: the file set descriptor,
/// every file entry, and every directory's contents.
fn metadata_image(
    tree: &[Node],
    plan: &Plan,
    rev: Revision,
    now: &Stamp,
    label: &str,
) -> Result<Vec<u8>> {
    let mut out = vec![0u8; (plan.meta_blocks * SECTOR as u64) as usize];
    let put = |out: &mut Vec<u8>, block: u32, bytes: &[u8]| {
        let at = block as usize * SECTOR;
        out[at..at + bytes.len()].copy_from_slice(bytes);
    };

    put(
        &mut out,
        0,
        &sized(&file_set(tree[0].entry, rev, now, label))?,
    );
    put(&mut out, 1, &sized(&terminating(1))?);

    for (i, node) in tree.iter().enumerate() {
        put(&mut out, node.entry, &sized(&file_entry(node, now))?);
        if node.is_dir() {
            let mut dir = Vec::with_capacity(node.size as usize);
            // The parent comes first and has no name.
            // The root's parent is the root, which is what a filesystem
            // says when it has run out of upwards.
            let parent = tree
                .iter()
                .find(|n| n.children.contains(&i))
                .unwrap_or(&tree[0])
                .entry;
            dir.extend_from_slice(&file_id("", true, true, parent, 0, dir.len(), node.data));
            for &c in &node.children {
                let child = &tree[c];
                dir.extend_from_slice(&file_id(
                    &child.name,
                    child.is_dir(),
                    false,
                    child.entry,
                    child.unique,
                    dir.len(),
                    node.data,
                ));
            }
            put(&mut out, node.data, &dir);
        }
    }
    Ok(out)
}

/// One entry in a directory.
///
/// `at` is how far into the directory it is being written, because the
/// descriptor records the block it is recorded in and a long directory
/// crosses several.
fn file_id(
    name: &str,
    directory: bool,
    parent: bool,
    icb: u32,
    unique: u32,
    at: usize,
    first_block: u32,
) -> Vec<u8> {
    let mut d = vec![0u8; fid_len(if parent { 0 } else { name.len() })];
    le16(&mut d, 16, 1); // file version
    d[18] = if parent {
        0x08 | 0x02
    } else if directory {
        0x02
    } else {
        0x00
    };
    d[19] = if parent { 0 } else { name.len() as u8 + 1 };
    long_ad(&mut d, 20, SECTOR as u32, icb, METADATA_PART);
    // What the six bytes a long descriptor keeps for the implementation are
    // used for here: the number the file it points at is known by, which its
    // own entry carries too. Both reference images write it.
    le32(&mut d, 32, unique);
    if !parent {
        // Eight-bit OSTA compressed unicode: the marker, and then the name.
        d[38] = 8;
        d[39..39 + name.len()].copy_from_slice(name.as_bytes());
    }
    let block = first_block + (at / SECTOR) as u32;
    tag(&mut d, TAG_FILE_ID, block as u64);
    d
}

/// Which partition reference means which. The physical partition is the
/// first map in the logical volume and the metadata partition the second, so
/// a file's data is named as partition 0 and everything in the metadata
/// partition as partition 1.
const PHYSICAL_PART: u16 = 0;
const METADATA_PART: u16 = 1;

/// The entry for one file or directory.
fn file_entry(node: &Node, now: &Stamp) -> Vec<u8> {
    let mut d = vec![0u8; 216];
    let dir = node.is_dir();
    icb(
        &mut d,
        if dir { FILE_DIRECTORY } else { FILE_ORDINARY },
        // A directory's contents are on the metadata partition, which is the
        // partition its own entry is recorded on, so a short descriptor says
        // it. A file's are not, so its entry has to name the partition.
        if dir { 0 } else { 1 },
    );
    le32(&mut d, 36, 0xFFFF_FFFF); // no owner
    le32(&mut d, 40, 0xFFFF_FFFF); // and no group
    le32(&mut d, 44, 0x14A5); // read and execute for everybody: a read-only disc
    le16(&mut d, 48, if dir { 2 } else { 1 });
    le64(&mut d, 56, node.size);
    le64(&mut d, 64, node.size); // object size: the same, there being no streams
    le64(&mut d, 72, node.blocks);
    d[80..92].copy_from_slice(&now.bytes());
    d[92..104].copy_from_slice(&now.bytes());
    d[104..116].copy_from_slice(&now.bytes());
    d[116..128].copy_from_slice(&now.bytes());
    le32(&mut d, 128, 1); // checkpoint
    d[168..200].copy_from_slice(&regid(0, "*SmartCut", &[]));
    le64(&mut d, 200, node.unique as u64);
    le32(&mut d, 208, 0); // no extended attributes

    // The allocation descriptors. A file longer than an extent can describe
    // is written as several, each a whole number of blocks but the last.
    let mut ads = Vec::new();
    let mut left = node.size;
    let mut block = node.data as u64;
    while left > 0 || ads.is_empty() {
        let take = left.min(EXTENT_MAX);
        let mut ad = vec![0u8; if dir { 8 } else { 16 }];
        le32(&mut ad, 0, take as u32);
        if dir {
            le32(&mut ad, 4, block as u32);
        } else {
            le32(&mut ad, 4, block as u32);
            le16(&mut ad, 8, PHYSICAL_PART);
        }
        ads.extend_from_slice(&ad);
        block += blocks(take);
        left -= take;
        if left == 0 {
            break;
        }
    }
    le32(&mut d, 212, ads.len() as u32);
    d.extend_from_slice(&ads);
    tag(&mut d, TAG_EXTENDED_FILE_ENTRY, node.entry as u64);
    d
}

/// The entry for the metadata file itself, or for its mirror.
///
/// It lives on the partition beside the one it describes, so its own
/// descriptors are short ones and mean that partition.
fn metadata_entry(kind: u8, at: u32, blocks: u64, now: &Stamp) -> Vec<u8> {
    let mut d = vec![0u8; 216];
    icb(&mut d, kind, 0);
    le32(&mut d, 36, 0xFFFF_FFFF);
    le32(&mut d, 40, 0xFFFF_FFFF);
    le32(&mut d, 44, 0x14A5);
    le16(&mut d, 48, 1);
    let size = blocks * SECTOR as u64;
    le64(&mut d, 56, size);
    le64(&mut d, 64, size);
    le64(&mut d, 72, blocks);
    for at in [80, 92, 104, 116] {
        d[at..at + 12].copy_from_slice(&now.bytes());
    }
    le32(&mut d, 128, 1);
    d[168..200].copy_from_slice(&regid(0, "*SmartCut", &[]));
    le64(&mut d, 200, 0);
    le32(&mut d, 208, 0);
    let mut ad = vec![0u8; 8];
    le32(&mut ad, 0, size as u32);
    le32(&mut ad, 4, at);
    le32(&mut d, 212, ad.len() as u32);
    d.extend_from_slice(&ad);
    tag(
        &mut d,
        TAG_EXTENDED_FILE_ENTRY,
        if kind == FILE_METADATA { 0 } else { 1 },
    );
    d
}

/// The file set: which directory is the root of it.
fn file_set(root: u32, rev: Revision, now: &Stamp, label: &str) -> Vec<u8> {
    let mut d = vec![0u8; 512];
    d[16..28].copy_from_slice(&now.bytes());
    le16(&mut d, 28, 3); // interchange level
    le16(&mut d, 30, 3);
    le32(&mut d, 32, 1); // one character set
    le32(&mut d, 36, 1);
    charspec(&mut d, 48);
    dstring(&mut d, 112, 128, label);
    charspec(&mut d, 240);
    dstring(&mut d, 304, 32, label);
    long_ad(&mut d, 400, SECTOR as u32, root, METADATA_PART);
    d[416..448].copy_from_slice(&regid(0, "*OSTA UDF Compliant", &domain_suffix(rev)));
    tag(&mut d, TAG_FILE_SET, 0);
    d
}

// --- the volume ----------------------------------------------------------

/// The six descriptors that describe the volume, written twice: once where
/// the anchor's main extent points and once where its reserve does. `at` is
/// the block the first of them is written at, because each records its own.
fn volume_descriptors(
    plan: &Plan,
    rev: Revision,
    now: &Stamp,
    label: &str,
    at: u64,
) -> Vec<Vec<u8>> {
    vec![
        primary(now, label, at),
        implementation_use(rev, label, at + 1),
        partition(plan, at + 2),
        logical_volume(rev, label, at + 3),
        unallocated(at + 4),
        terminating(at + 5),
    ]
}

fn primary(now: &Stamp, label: &str, at: u64) -> Vec<u8> {
    let mut d = vec![0u8; 512];
    // Every descriptor of the sequence is numbered, in the order they are
    // written: a reader takes the highest-numbered of any it sees twice.
    le32(&mut d, 16, 0);
    le32(&mut d, 20, 0);
    dstring(&mut d, 24, 32, label);
    le16(&mut d, 56, 1); // one volume in the set
    le16(&mut d, 58, 1);
    le16(&mut d, 60, 2); // interchange level: one volume, one file set
    le16(&mut d, 62, 2);
    le32(&mut d, 64, 1);
    le32(&mut d, 68, 1);
    // The volume set is named after when it was made, which is what a tool
    // with nothing else to go on does: it only has to be unique.
    dstring(&mut d, 72, 128, &now.set_id());
    charspec(&mut d, 200);
    charspec(&mut d, 264);
    d[344..376].copy_from_slice(&regid(0, "*SmartCut", &[]));
    d[376..388].copy_from_slice(&now.bytes());
    d[388..420].copy_from_slice(&regid(0, "*SmartCut", &[]));
    tag(&mut d, TAG_PRIMARY, at);
    d
}

fn implementation_use(rev: Revision, label: &str, at: u64) -> Vec<u8> {
    let mut d = vec![0u8; 512];
    le32(&mut d, 16, 1);
    d[20..52].copy_from_slice(&regid(0, "*UDF LV Info", &udf_suffix(rev)));
    charspec(&mut d, 52);
    dstring(&mut d, 116, 128, label);
    dstring(&mut d, 244, 36, "");
    dstring(&mut d, 280, 36, "");
    dstring(&mut d, 316, 36, "");
    d[352..384].copy_from_slice(&regid(0, "*SmartCut", &[]));
    tag(&mut d, TAG_IMPL_USE, at);
    d
}

fn partition(plan: &Plan, at: u64) -> Vec<u8> {
    let mut d = vec![0u8; 512];
    le32(&mut d, 16, 2);
    le16(&mut d, 20, 1); // allocated
    le16(&mut d, 22, 0); // partition number
    d[24..56].copy_from_slice(&regid(0, "+NSR03", &[]));
    le32(&mut d, 184, 1); // read only
    le32(&mut d, 188, PARTITION as u32);
    le32(&mut d, 192, plan.partition_blocks as u32);
    d[196..228].copy_from_slice(&regid(0, "*SmartCut", &[]));
    tag(&mut d, TAG_PARTITION, at);
    d
}

fn logical_volume(rev: Revision, label: &str, at: u64) -> Vec<u8> {
    let mut d = vec![0u8; 512];
    le32(&mut d, 16, 3);
    charspec(&mut d, 20);
    dstring(&mut d, 84, 128, label);
    le32(&mut d, 212, SECTOR as u32);
    d[216..248].copy_from_slice(&regid(0, "*OSTA UDF Compliant", &domain_suffix(rev)));
    // Where the file set is: the first two blocks of the metadata partition.
    long_ad(&mut d, 248, 2 * SECTOR as u32, 0, METADATA_PART);
    le32(&mut d, 264, 6 + 64); // the two maps below
    le32(&mut d, 268, 2);
    d[272..304].copy_from_slice(&regid(0, "*SmartCut", &[]));
    le32(&mut d, 432, (INTEGRITY_BLOCKS * SECTOR as u64) as u32);
    le32(&mut d, 436, INTEGRITY as u32);

    // The physical partition, and the metadata partition whose blocks are
    // the contents of a file on it.
    d[440] = 1;
    d[441] = 6;
    le16(&mut d, 442, 1); // volume sequence number
    le16(&mut d, 444, 0); // partition number
    let m = 446;
    d[m] = 2;
    d[m + 1] = 64;
    d[m + 4..m + 36].copy_from_slice(&regid(0, "*UDF Metadata Partition", &udf_suffix(rev)));
    le16(&mut d, m + 36, 1);
    le16(&mut d, m + 38, 0);
    // Where the two *entries* are, not where their contents are: a reader
    // finds the metadata partition by reading the file that holds it, and a
    // file is found by its entry. Both are at the front of the partition.
    le32(&mut d, m + 40, METADATA_FILE_ENTRY);
    le32(&mut d, m + 44, METADATA_MIRROR_ENTRY);
    le32(&mut d, m + 48, 0xFFFF_FFFF); // no bitmap: nothing here is allocated later
    le32(&mut d, m + 52, ALIGN as u32);
    le16(&mut d, m + 56, ALIGN as u16);
    d[m + 58] = 1; // the mirror is a copy of the metadata, not a spare
    tag(&mut d, TAG_LOGICAL_VOLUME, at);
    d
}

fn unallocated(at: u64) -> Vec<u8> {
    let mut d = vec![0u8; 512];
    le32(&mut d, 16, 4);
    le32(&mut d, 20, 0); // nothing is unallocated: the image is written once
    tag(&mut d, TAG_UNALLOCATED, at);
    d
}

/// The descriptor that says a sequence has ended. Like every other one, it
/// records the block it is written in: a reader that finds a descriptor
/// claiming to be somewhere else is right to stop.
fn terminating(at: u64) -> Vec<u8> {
    let mut d = vec![0u8; 512];
    tag(&mut d, TAG_TERMINATING, at);
    d
}

fn anchor(plan: &Plan, at: u64) -> Vec<u8> {
    let mut d = vec![0u8; 512];
    le32(&mut d, 16, (VDS_BLOCKS * SECTOR as u64) as u32);
    le32(&mut d, 20, MAIN_VDS as u32);
    le32(&mut d, 24, (VDS_BLOCKS * SECTOR as u64) as u32);
    le32(&mut d, 28, plan.reserve_vds as u32);
    tag(&mut d, TAG_ANCHOR, at);
    d
}

/// What the volume says about itself once it is finished: that it is closed,
/// how much of it is free, and how many files are on it.
fn integrity(plan: &Plan, rev: Revision, now: &Stamp) -> Vec<u8> {
    let mut d = vec![0u8; 512];
    d[16..28].copy_from_slice(&now.bytes());
    le32(&mut d, 28, 1); // closed
    le64(&mut d, 40, plan.next_unique as u64);
    le32(&mut d, 72, 2); // two partitions
    le32(&mut d, 76, 46); // the implementation use below
    le32(&mut d, 80, 0); // free on the physical partition
    le32(&mut d, 84, 0); // and on the metadata partition
    le32(&mut d, 88, plan.partition_blocks as u32);
    le32(&mut d, 92, plan.meta_blocks as u32);
    let iu = 96;
    d[iu..iu + 32].copy_from_slice(&regid(0, "*SmartCut", &[]));
    le32(&mut d, iu + 32, plan.files);
    le32(&mut d, iu + 36, plan.directories);
    // What a reader has to understand to read this, and what a writer would
    // have to understand to add to it. 2.60 is readable by a 2.50 reader,
    // which is what both reference images say of themselves.
    le16(&mut d, iu + 40, 0x0250);
    le16(&mut d, iu + 42, rev.number());
    le16(&mut d, iu + 44, rev.number());
    tag(&mut d, TAG_INTEGRITY, INTEGRITY);
    d
}

// --- the pieces every descriptor is made of ------------------------------

/// Fill in the tag: its identifier, where it is recorded, and the two sums
/// that say it arrived intact.
fn tag(d: &mut [u8], id: u16, location: u64) {
    le16(d, 0, id);
    le16(d, 2, 3); // descriptor version, 3 for UDF 2.00 and later
    le16(d, 6, 1); // tag serial number
    let crc_len = d.len() - 16;
    le16(d, 8, crc16(&d[16..]));
    le16(d, 10, crc_len as u16);
    le32(d, 12, location as u32);
    // The checksum is of the tag itself, and of every byte of it but its own.
    let sum: u8 = d[..16]
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 4)
        .map(|(_, b)| *b)
        .fold(0, u8::wrapping_add);
    d[4] = sum;
}

/// The 16 bit cyclic redundancy check ECMA-167 specifies, which is CCITT's
/// with no initial value and no final inversion.
fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for b in data {
        crc ^= (*b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// An ICB tag: what kind of thing this entry is, and how its contents are
/// described.
fn icb(d: &mut [u8], file_type: u8, ad_type: u16) {
    le16(d, 16 + 4, 4); // strategy 4: one entry, no continuation
    le16(d, 16 + 8, 1); // and that is the whole of it
    d[16 + 11] = file_type;
    // The archive bit, which every tool sets on a file it has just written,
    // and the low bits that say short or long allocation descriptors.
    le16(d, 16 + 18, 0x0020 | ad_type);
}

/// A long allocation descriptor: how much, where, and on which partition.
fn long_ad(d: &mut [u8], at: usize, len: u32, block: u32, part: u16) {
    le32(d, at, len);
    le32(d, at + 4, block);
    le16(d, at + 8, part);
}

/// The character set every string in a UDF volume is written in.
fn charspec(d: &mut [u8], at: usize) {
    d[at] = 0; // CS0
    let name = b"OSTA Compressed Unicode";
    d[at + 1..at + 1 + name.len()].copy_from_slice(name);
}

/// A name, as a volume descriptor holds one: the compression marker, the
/// characters, and the length in the last byte of the field.
fn dstring(d: &mut [u8], at: usize, len: usize, text: &str) {
    let field = &mut d[at..at + len];
    field.fill(0);
    if text.is_empty() {
        return;
    }
    // Eight bits a character where the name is Latin-1, which is what a disc
    // label written by a burner is, and sixteen where it is not.
    let wide = text.chars().any(|c| c as u32 > 0xFF);
    // What does not fit is left out, a whole character at a time: a
    // character outside the basic plane is written as two units, and half of
    // one is not a shorter name but a broken one. The marker stays either
    // way, and the last byte of the field is the length.
    let room = len - 1;
    let mut out = Vec::with_capacity(len);
    out.push(if wide { 16 } else { 8 });
    let mut buf = [0u16; 2];
    for c in text.chars() {
        let piece: Vec<u8> = if wide {
            c.encode_utf16(&mut buf)
                .iter()
                .flat_map(|u| u.to_be_bytes())
                .collect()
        } else {
            vec![c as u8]
        };
        if out.len() + piece.len() > room {
            break;
        }
        out.extend_from_slice(&piece);
    }
    field[..out.len()].copy_from_slice(&out);
    field[len - 1] = out.len() as u8;
}

/// An entity identifier: who wrote this, or what standard it follows.
fn regid(flags: u8, id: &str, suffix: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[0] = flags;
    let id = id.as_bytes();
    let take = id.len().min(23);
    out[1..1 + take].copy_from_slice(&id[..take]);
    let take = suffix.len().min(8);
    out[24..24 + take].copy_from_slice(&suffix[..take]);
    out
}

/// The suffix on the identifiers that name UDF itself: which revision the
/// image is written to, and -- on the two that name the *domain* -- that the
/// volume is not to be written to again. Both reference images say so, which
/// is what an image of a disc is.
fn udf_suffix(rev: Revision) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..2].copy_from_slice(&rev.number().to_le_bytes());
    out
}

fn domain_suffix(rev: Revision) -> [u8; 8] {
    let mut out = udf_suffix(rev);
    out[2] = 0x03; // hard and soft write protected
    out
}

fn le16(d: &mut [u8], at: usize, v: u16) {
    d[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn le32(d: &mut [u8], at: usize, v: u32) {
    d[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn le64(d: &mut [u8], at: usize, v: u64) {
    d[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// A descriptor padded out to the block it is written in.
///
/// One that does not fit is refused rather than cut down. `resize` shortens
/// as readily as it lengthens, so a file entry with more allocation
/// descriptors than a block holds -- which takes a single file of about
/// 114 GB, an extent being a gigabyte and a long descriptor sixteen bytes --
/// used to be silently truncated and written as though it were whole. An
/// image that says it is finished and is not is the worst of the answers
/// available here.
fn sized(d: &[u8]) -> Result<Vec<u8>> {
    if d.len() > SECTOR {
        bail!(
            "a descriptor of {} bytes does not fit in a {SECTOR} byte block: this folder holds \
             a file too large to describe in one entry",
            d.len()
        );
    }
    let mut out = d.to_vec();
    out.resize(SECTOR, 0);
    Ok(out)
}

// --- when ---------------------------------------------------------------

/// The moment the image was made, in the fields a UDF timestamp has.
struct Stamp {
    /// Seconds since 1970, which is what the fields below were worked out
    /// from and what names the volume set.
    stamp: i64,
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

impl Stamp {
    fn now() -> Stamp {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        // Days since 1970, and the civil date they come to. The arithmetic
        // is the usual one -- shift the year to start in March so that the
        // leap day is the last day of it -- and is exact for every date this
        // will ever be handed.
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        Stamp {
            stamp: secs,
            year: (y + i64::from(m <= 2)) as u16,
            month: m as u8,
            day: d as u8,
            hour: (rem / 3600) as u8,
            minute: (rem % 3600 / 60) as u8,
            second: (rem % 60) as u8,
        }
    }

    /// The twelve bytes a descriptor carries. Written as coordinated
    /// universal time, which is what an image made on one machine and read on
    /// another should say.
    fn bytes(&self) -> [u8; 12] {
        let mut out = [0u8; 12];
        // Type 1 -- local time -- with an offset of zero.
        out[..2].copy_from_slice(&0x1000u16.to_le_bytes());
        out[2..4].copy_from_slice(&self.year.to_le_bytes());
        out[4] = self.month;
        out[5] = self.day;
        out[6] = self.hour;
        out[7] = self.minute;
        out[8] = self.second;
        out
    }

    /// What the volume set is called.
    ///
    /// Sixteen hexadecimal digits, the first eight of which UDF asks to be
    /// unique among the sets a machine writes -- so they are the second the
    /// image was made in. A burner writes the same shape for the same
    /// reason: there is nothing else to tell one set from another by.
    fn set_id(&self) -> String {
        let day = (self.year as u64) << 32
            | (self.month as u64) << 24
            | (self.day as u64) << 16
            | (self.hour as u64) << 8
            | self.minute as u64;
        format!(
            "{:08X}{:08X}",
            self.stamp as u32,
            (day ^ self.stamp as u64) as u32
        )
    }
}

// --- writing it out ------------------------------------------------------

/// A file being written sector by sector, which knows how far in it is.
struct Writer {
    to: BufWriter<std::fs::File>,
    at: u64,
}

impl Writer {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.to.write_all(bytes)?;
        self.at += bytes.len() as u64;
        Ok(())
    }

    fn sector(&mut self, bytes: &[u8]) -> Result<()> {
        self.write_all(bytes)?;
        self.align()
    }

    /// Zeros up to the next block boundary.
    fn align(&mut self) -> Result<()> {
        let over = self.at % SECTOR as u64;
        if over != 0 {
            let pad = vec![0u8; (SECTOR as u64 - over) as usize];
            self.write_all(&pad)?;
        }
        Ok(())
    }

    /// Zeros up to a block, which is how everything between the descriptors
    /// is written.
    fn pad_to(&mut self, block: u64) -> Result<()> {
        let want = block * SECTOR as u64;
        if self.at > want {
            bail!("the image is already past block {block}");
        }
        // A run of zeros can be a gigabyte where a file was aligned, so it is
        // written as a hole rather than as bytes.
        if want > self.at {
            self.to.flush()?;
            self.to.get_mut().seek(SeekFrom::Start(want))?;
            // The last sector is written for real, so the file is the length
            // it claims even where nothing follows.
            self.at = want;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    /// A descriptor larger than the block it goes in is refused, not cut
    /// down. `resize` shortens as readily as it lengthens, and an image
    /// written from a truncated file entry is one that says it is finished
    /// and is not.
    #[test]
    fn a_descriptor_too_large_for_its_block_is_refused() {
        assert!(sized(&vec![0u8; 216]).is_ok());
        assert!(sized(&vec![0u8; SECTOR]).is_ok());
        assert!(sized(&vec![0u8; SECTOR + 1]).is_err());
    }
    use super::*;

    /// The check sums in a descriptor tag, against a tag read off a real
    /// image: the file identifier for `BDAV` in the root of a disc an
    /// authoring tool wrote.
    #[test]
    fn the_sums_are_the_ones_a_real_image_has() {
        let mut d = file_id("BDAV", true, false, 4, 16, 40, 3);
        assert_eq!(d.len(), 44);
        tag(&mut d, TAG_FILE_ID, 3);
        // Tag, version, checksum, reserved, serial, CRC, CRC length, block.
        assert_eq!(d[0..2], [0x01, 0x01]);
        assert_eq!(d[2..4], [0x03, 0x00]);
        assert_eq!(d[4], 0x22);
        assert_eq!(d[8..10], [0xf1, 0x0c]);
        assert_eq!(d[10..12], [0x1c, 0x00]);
        assert_eq!(d[12..16], [0x03, 0x00, 0x00, 0x00]);
        // The name, with the marker that says how it is compressed.
        assert_eq!(&d[38..43], b"\x08BDAV");
    }

    #[test]
    fn a_name_is_cut_at_a_character() {
        let mut d = vec![0u8; 32];
        dstring(&mut d, 0, 32, "テストディスク");
        // Sixteen bits a character, so the marker and fifteen characters fit.
        assert_eq!(d[0], 16);
        assert_eq!(d[31], 15);
        assert_eq!(&d[1..3], &[0x30, 0xc6]); // テ
        let mut d = vec![0u8; 8];
        dstring(&mut d, 0, 8, "ABCDEFGHIJ");
        assert_eq!(d[0], 8);
        assert_eq!(&d[1..7], b"ABCDEF");
        assert_eq!(d[7], 7);
        // A character outside the basic plane is two units, and the field
        // stops in front of it rather than keeping half of one.
        let mut d = vec![0u8; 8];
        dstring(&mut d, 0, 8, "アい\u{1F600}");
        assert_eq!(d[0], 16);
        assert_eq!(d[7], 5);
        assert_eq!(&d[1..5], &[0x30, 0xa2, 0x30, 0x44]);
        assert_eq!(&d[5..7], &[0, 0]);
    }

    /// A file longer than an extent can describe is written as several, and
    /// all but the last are a whole number of blocks.
    #[test]
    fn a_long_file_is_split_where_the_field_runs_out() {
        let node = Node {
            name: "00001.m2ts".into(),
            source: Some(PathBuf::from("/x")),
            size: EXTENT_MAX + 12345,
            children: Vec::new(),
            entry: 9,
            data: 64,
            blocks: blocks(EXTENT_MAX + 12345),
            unique: 16,
        };
        let d = file_entry(&node, &Stamp::now());
        let len = u32::from_le_bytes(d[212..216].try_into().unwrap()) as usize;
        assert_eq!(len, 32); // two long descriptors
        let first = u32::from_le_bytes(d[216..220].try_into().unwrap()) as u64;
        let second = u32::from_le_bytes(d[232..236].try_into().unwrap()) as u64;
        assert_eq!(first, EXTENT_MAX);
        assert_eq!(second, 12345);
        assert_eq!(first % SECTOR as u64, 0);
        // The second extent picks up where the first left off.
        assert_eq!(
            u32::from_le_bytes(d[236..240].try_into().unwrap()) as u64,
            64 + EXTENT_MAX / SECTOR as u64
        );
    }
}
