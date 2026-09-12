"""Report the shape of the UDF image a disc is wrapped in.

Not what the disc says -- `bdav_index.py` reads that -- but what the
filesystem around it looks like: where the descriptors sit, how the partition
is declared, where the metadata lives, and where each file's bytes actually
are. Three layers, and a player that refuses an image usually refuses it for
something in one of them rather than for anything in the recording.

Written to be pointed at real discs as readily as at ours. A recorder's own
image has three anchors where a burner writes two, leaves deleted file
identifiers lying in its directories, and aligns every extent to the cluster
a Blu-ray is written in; none of that is a fault, and a reader that assumes
otherwise is the thing that breaks.

    udf_shape.py <image.iso> [...]     the shape of each, as key=value lines
    udf_shape.py --tree <image.iso>    its files, with where the bytes are

Prints `key=value` lines for a caller to compare.
"""
import os
import struct
import sys

SECTOR = 2048
# The cluster a Blu-ray is written in, in blocks. What both a recorder and
# our own writer align to, so what the report measures against.
CLUSTER = 32

TAG_PRIMARY, TAG_ANCHOR, TAG_PARTITION = 1, 2, 5
TAG_LOGICAL_VOLUME, TAG_TERMINATING, TAG_INTEGRITY = 6, 8, 9
TAG_FILE_ENTRY, TAG_FILE_ID, TAG_EXTENDED_FILE_ENTRY = 261, 257, 266
FILE_TYPE_DIRECTORY = 4
# A file identifier whose file is gone keeps its place in the directory with
# this bit set. Everything behind it is stale, the ICB it points at included.
DELETED = 0x04
PARENT = 0x08

ACCESS = {1: "read-only", 2: "write-once", 3: "rewritable", 4: "overwritable"}


def dstring(raw):
    """A d-string: its last byte is the length, its first says the encoding."""
    n = raw[-1]
    s = raw[:n]
    if not s:
        return ""
    if s[0] == 16:
        return s[1:].decode("utf-16-be", "replace")
    return s[1:].decode("latin1", "replace")


def identifier(raw):
    """The printable part of a 32 byte entity identifier."""
    return raw[1:24].split(b"\x00")[0].decode("latin1", "replace")


class Image:
    def __init__(self, path):
        self.path = path
        self.f = open(path, "rb")
        self.sectors = os.path.getsize(path) // SECTOR
        self.anchors = [n for n in (256, self.sectors - 257, self.sectors - 1)
                        if 0 <= n < self.sectors and self.tag_at(n) == TAG_ANCHOR]
        if not self.anchors:
            raise SystemExit(f"{path}: no anchor at 256 or at the end -- not a UDF image")
        anchor = self.block(self.anchors[0])
        self.vds_len, self.vds_at = struct.unpack("<II", anchor[16:24])
        self.reserve_len, self.reserve_at = struct.unpack("<II", anchor[24:32])
        self.read_volume()
        self.read_metadata()

    # --- reading blocks -------------------------------------------------

    def block(self, lba, count=1):
        self.f.seek(lba * SECTOR)
        return self.f.read(count * SECTOR)

    def tag_at(self, lba):
        d = self.block(lba)
        return struct.unpack("<H", d[0:2])[0] if len(d) >= 2 else 0

    def partition_block(self, part, lba):
        """A logical block of a partition, as a block of the image.

        The physical partition is an offset; the metadata partition is a file
        inside it, so a block of it is wherever that file's extents put it.
        """
        if part == self.metadata_part:
            for start, length in self.metadata_extents:
                if lba < length:
                    return self.start + start + lba
                lba -= length
            raise SystemExit(f"{self.path}: metadata partition is shorter than it claims")
        return self.start + lba

    def read(self, part, lba, count=1):
        return self.block(self.partition_block(part, lba), count)

    # --- the volume -----------------------------------------------------

    def read_volume(self):
        self.primary = self.partition = self.volume = None
        for i in range(self.vds_len // SECTOR):
            d = self.block(self.vds_at + i)
            tag = struct.unpack("<H", d[0:2])[0]
            if tag == TAG_PRIMARY:
                self.primary = d
            elif tag == TAG_PARTITION:
                self.partition = d
            elif tag == TAG_LOGICAL_VOLUME:
                self.volume = d
            elif tag == TAG_TERMINATING:
                break
        if not (self.primary and self.partition and self.volume):
            raise SystemExit(f"{self.path}: the volume descriptor sequence is incomplete")
        self.access, self.start, self.blocks = struct.unpack("<III", self.partition[184:196])
        self.revision = self.volume[240] | self.volume[241] << 8
        self.fsd_len, self.fsd_at, self.fsd_part = struct.unpack("<IIH", self.volume[248:258])

    def read_metadata(self):
        """The metadata partition, which is a file the file entries live in."""
        self.metadata_part = -1
        self.metadata_at = self.metadata_mirror = self.metadata_bitmap = None
        self.metadata_align = None
        self.metadata_extents = []
        count, = struct.unpack("<I", self.volume[268:272])
        at = 440
        for index in range(count):
            kind, length = self.volume[at], self.volume[at + 1]
            if kind == 2 and b"Metadata" in self.volume[at + 4:at + 36]:
                self.metadata_part = index
                (self.metadata_at, self.metadata_mirror, bitmap, align,
                 ) = struct.unpack("<IIII", self.volume[at + 40:at + 56])
                self.metadata_bitmap = None if bitmap == 0xFFFFFFFF else bitmap
                self.metadata_align = align
            at += length
        if self.metadata_part >= 0:
            self.metadata_extents = extents(self.block(self.start + self.metadata_at))
            self.mirror_extents = extents(self.block(self.start + self.metadata_mirror))

    def integrity(self):
        """The logical volume integrity descriptor, if it is where it says."""
        length, at = struct.unpack("<II", self.volume[432:440])
        if not length:
            return None
        d = self.block(at)
        return d if struct.unpack("<H", d[0:2])[0] == TAG_INTEGRITY else None

    # --- the files ------------------------------------------------------

    def tree(self):
        """Every file on the image, in the order its directories list them."""
        fsd = self.read(self.fsd_part, self.fsd_at)
        _, root, part = struct.unpack("<IIH", fsd[400:410])
        out = []
        self.deleted = 0
        self.walk(part, root, "/", out)
        return out

    def walk(self, part, lba, path, out):
        entry = self.read(part, lba)
        raw = b""
        for start, length in extents(entry):
            raw += entry_bytes(entry) if start is None else self.read(part, start, length)
        at = 0
        while at + 38 <= len(raw):
            if struct.unpack("<H", raw[at:at + 2])[0] != TAG_FILE_ID:
                break
            characteristics = raw[at + 18]
            name_len = raw[at + 19]
            impl_len, = struct.unpack("<H", raw[at + 36:at + 38])
            _, child, child_part = struct.unpack("<IIH", raw[at + 20:at + 30])
            name = dstring(raw[at + 38 + impl_len:at + 38 + impl_len + name_len]
                           + bytes([name_len])) if name_len else ""
            at += (38 + impl_len + name_len + 3) & ~3
            if characteristics & PARENT or not name:
                continue
            if characteristics & DELETED:
                # Counted and not followed: the entry it points at belongs to
                # whatever was written over it.
                self.deleted += 1
                out.append((path + name, None, []))
                continue
            child_entry = self.read(child_part, child)
            kind, size = describe(child_entry)
            if kind == FILE_TYPE_DIRECTORY:
                self.walk(child_part, child, path + name + "/", out)
            else:
                # A directory's contents are in the metadata partition with
                # its entry; a file's bytes never are. The descriptors in a
                # file entry that lives in the metadata partition still count
                # from the start of the physical one, which is the trap in
                # reading an image that has both.
                out.append((path + name, size,
                            [(self.start + s, n)
                             for s, n in extents(child_entry) if s is not None]))


def entry_bytes(entry):
    """The contents of a file small enough to live inside its own entry."""
    return extents(entry)[0][1]


def describe(entry):
    """What a file entry is and how long the file is."""
    size, = struct.unpack("<Q", entry[56:64])
    return entry[16 + 11], size


def extents(entry):
    """Where a file entry says its bytes are, as (start, blocks) pairs.

    A start of None means the bytes are in the entry itself, and the second
    half of the pair is the bytes rather than a length.
    """
    tag, = struct.unpack("<H", entry[0:2])
    base = 216 if tag == TAG_EXTENDED_FILE_ENTRY else 176
    ea_len, ad_len = struct.unpack("<II", entry[base - 8:base])
    kind = struct.unpack("<H", entry[16 + 18:16 + 20])[0] & 7
    ads = entry[base + ea_len:base + ea_len + ad_len]
    out = []
    if kind == 3:
        return [(None, ads)]
    stride = 8 if kind == 0 else 16
    for at in range(0, len(ads) - stride + 1, stride):
        length, start = struct.unpack("<II", ads[at:at + 8])
        if length == 0:
            break
        # The top two bits say whether the extent is recorded at all; a hole
        # has no place in the layout this reports.
        if length >> 30:
            continue
        out.append((start, (length + SECTOR - 1) // SECTOR))
    return out


def shape(path):
    image = Image(path)
    files = image.tree()
    say = lambda k, v: print(f"{k}={v}")

    # --- the volume
    say("sectors", image.sectors)
    say("anchors", ",".join(str(a) for a in image.anchors))
    say("vds_main", f"{image.vds_at}+{image.vds_len // SECTOR}")
    say("vds_reserve", f"{image.reserve_at}+{image.reserve_len // SECTOR}")
    say("udf_revision", f"{image.revision >> 8:x}.{image.revision & 0xFF:02x}")
    say("volume_id", dstring(image.primary[24:56]))
    say("writer", identifier(image.partition[196:228]))
    say("partition", f"{image.start}+{image.blocks}")
    say("access", ACCESS.get(image.access, image.access))
    say("file_set", f"{image.fsd_at}+{image.fsd_len // SECTOR}")
    if image.metadata_part >= 0:
        say("metadata_entry", image.metadata_at)
        say("metadata", f"{image.metadata_extents[0][0]}+"
                        f"{sum(n for _, n in image.metadata_extents)}")
        say("metadata_mirror_entry", image.metadata_mirror)
        say("metadata_mirror", f"{image.mirror_extents[0][0]}+"
                               f"{sum(n for _, n in image.mirror_extents)}"
            if image.mirror_extents else "none")
        # Both entries in one cluster is both entries lost to one bad one,
        # which is the thing the mirror is there to survive.
        say("mirror_apart", "no" if image.metadata_mirror // CLUSTER
            == image.metadata_at // CLUSTER else "yes")
        say("metadata_bitmap", "none" if image.metadata_bitmap is None
            else image.metadata_bitmap)
        say("metadata_align", image.metadata_align)
    integrity = image.integrity()
    if integrity is not None:
        parts, = struct.unpack("<I", integrity[72:76])
        use = 80 + 8 * parts
        count, dirs, low, write, high = struct.unpack("<IIHHH", integrity[use + 32:use + 46])
        say("integrity", "closed" if struct.unpack("<I", integrity[28:32])[0] else "open")
        say("counted", f"{count} files, {dirs} dirs")
        say("reads_at", f"{low >> 8:x}.{low & 0xFF:02x}")
        say("writes_at", f"{write >> 8:x}.{write & 0xFF:02x}")

    # --- the files
    real = [f for f in files if f[1] is not None]
    say("files", len(real))
    say("deleted_ids", image.deleted)
    say("names", ",".join(sorted(os.path.basename(p) for p, s, _ in real))[:400])
    say("bytes", sum(s for _, s, _ in real))

    # --- where the bytes are
    pieces = [(start, length) for _, _, e in real for start, length in e]
    if pieces:
        say("data_from", min(s for s, _ in pieces))
        say("data_to", max(s + n for s, n in pieces))
        say("longest_extent", max(n for _, n in pieces))
        say("split_files", sum(1 for _, _, e in real if len(e) > 1))
        starts = sum(1 for s, _ in pieces if s % CLUSTER)
        # A recorder aligns the streams and packs the small index files in
        # tight, so it is the streams that say whether a writer aligns at
        # all. A megabyte is well above any index file and well below any
        # recording.
        stream = [(s, n) for _, size, e in real if size >= 1 << 20 for s, n in e]
        say("stream_extents", len(stream))
        say("stream_starts_aligned",
            "yes" if all(s % CLUSTER == 0 for s, _ in stream) else "no")
        say("stream_runs_aligned",
            "yes" if all(n % CLUSTER == 0
                         for _, size, e in real if size >= 1 << 20
                         for n in [x for _, x in e[:-1]]) else "no")
        # The last extent of a file ends wherever the file ends; the ones
        # before it are a choice, and a recorder makes a different one than a
        # burner does.
        runs = [e[:-1] for _, _, e in real if len(e) > 1]
        lengths = sum(1 for run in runs for _, n in run if n % CLUSTER)
        say("unaligned_starts", starts)
        say("unaligned_runs", lengths)
        overlap = 0
        ordered = sorted(pieces)
        for (a_start, a_len), (b_start, _) in zip(ordered, ordered[1:]):
            if a_start + a_len > b_start:
                overlap += 1
        say("overlaps", overlap)


def tree(path):
    image = Image(path)
    for name, size, pieces in image.tree():
        if size is None:
            print(f"{name}  (deleted identifier)")
            continue
        where = " ".join(f"{s}+{n}" for s, n in pieces[:4])
        if len(pieces) > 4:
            where += f" ... {len(pieces)} extents"
        print(f"{name}  {size} bytes  {where}")


if __name__ == "__main__":
    args = sys.argv[1:]
    if not args:
        raise SystemExit(__doc__)
    if args[0] == "--tree":
        for path in args[1:]:
            if len(args) > 2:
                print(f"--- {path}")
            tree(path)
    else:
        for path in args:
            if len(args) > 1:
                print(f"--- {path}")
            shape(path)
