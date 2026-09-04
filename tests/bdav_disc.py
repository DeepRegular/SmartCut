#!/usr/bin/env python3
"""Write the index files of a BDAV disc around streams that already exist.

A disc is a directory of `.m2ts` files and three small files that say what
they are: `info.bdav` lists the playlists, each `.rpls` names one recording --
its clip, its IN and OUT, its chapter marks and, in ARIB's own text encoding,
the programme's name.

This writes those, so that the reader in `rust/crates/core/src/bdav.rs` can be
tested against a disc that fits in a few megabytes. The offsets here are the
ones read off a disc written by an authoring tool, and the reader was made to
match that disc rather than this file; what this catches is a change to either
of them.

    bdav_disc.py <dir> <name>=<clip>,<seconds>[,<in>] ...

`<dir>` is the folder to hold `BDAV/`; the streams are already in
`<dir>/BDAV/STREAM/`. `<in>` is where the recording begins on the stream's own
clock -- what `ffprobe` reports as the container's start time. It matters
because a playlist counts in that clock and nothing else: an IN point that is
not in the stream makes a disc whose chapter marks point nowhere, which is a
disc no recorder would write.
"""
import os
import struct
import sys

# The clock a playlist counts in.
TICK = 45000
# Where the real files put each part. The reader knows these numbers; so does
# every disc.
NAME_LEN_AT = 88
MADE_AT = 50
TABLE_AT = 320
MARK_STRIDE = 46


def arib(text):
    """UTF-8 to an ARIB eight-unit string.

    Two graphic sets are enough for a programme name: the alphanumerics for
    ASCII and the kanji set -- which holds the kana as well -- for everything
    else. JIS X 0208 is EUC-JP with the high bits taken off.
    """
    out = bytearray()
    mode = None
    for ch in text:
        if ch.isascii():
            if mode != "alnum":
                out.append(0x0E)  # LS1
                mode = "alnum"
            out.append(ord(ch))
            continue
        try:
            wide = ch.encode("euc-jp")
        except UnicodeEncodeError:
            continue
        if len(wide) != 2:
            continue
        if mode != "kanji":
            out.append(0x0F)  # LS0
            mode = "kanji"
        out += bytes([wide[0] & 0x7F, wide[1] & 0x7F])
    return bytes(out)


def bcd(*parts):
    return bytes((p // 10) * 16 + p % 10 for p in parts)


def rpls(clip, seconds, name, made, marks, start):
    """One playlist: one PlayItem, and the marks that fall inside it."""
    in_time = int(start * TICK)
    out_time = in_time + int(seconds * TICK)

    item = struct.pack(">5s4sHBII", clip.encode(), b"M2TS", 1, 0, in_time, out_time)
    play_list = struct.pack(">HHH", 0, 1, 0) + struct.pack(">H", len(item)) + item
    play_list = struct.pack(">I", len(play_list)) + play_list

    entries = b""
    for at in marks:
        entry = bytearray(MARK_STRIDE)
        struct.pack_into(">I", entry, 6, in_time + int(at * TICK))
        entries += bytes(entry)
    play_marks = struct.pack(">H", len(marks)) + entries
    # The length counts what follows it: the count and the entries.
    play_marks = struct.pack(">I", len(play_marks)) + play_marks

    head = bytearray(NAME_LEN_AT + 1)
    head[0:8] = b"PLST0100"
    head[MADE_AT:MADE_AT + 7] = bcd(*made)
    text = arib(name)
    head[NAME_LEN_AT] = len(text)
    body = bytes(head) + text
    # The playlist follows the description of itself, on a four byte boundary.
    list_at = (len(body) + 3) & ~3
    body += b"\0" * (list_at - len(body))
    marks_at = list_at + len(play_list)

    raw = bytearray(body + play_list + play_marks)
    struct.pack_into(">I", raw, 8, list_at)
    struct.pack_into(">I", raw, 12, marks_at)
    struct.pack_into(">I", raw, 16, 0)
    struct.pack_into(">I", raw, 40, list_at - 44)
    return bytes(raw)


def info_bdav(names, volume):
    """The index: which playlists there are, and what the disc is called."""
    table = struct.pack(">H", len(names)) + b"".join(n.encode() for n in names)
    raw = bytearray(TABLE_AT)
    raw[0:8] = b"BDAV0100"
    struct.pack_into(">I", raw, 8, TABLE_AT)
    struct.pack_into(">I", raw, 12, 0)
    struct.pack_into(">I", raw, 40, TABLE_AT - 44)
    text = arib(volume)
    raw[64:64 + len(text)] = text
    return bytes(raw) + struct.pack(">I", len(table)) + table


def clpi(clip):
    """A stub. Nothing reads it yet; a disc without one is not a disc."""
    return b"HDPV0200" + b"\0" * 56


def main(argv):
    if len(argv) < 3:
        raise SystemExit(__doc__)
    root = os.path.join(argv[1], "BDAV")
    os.makedirs(os.path.join(root, "PLAYLIST"), exist_ok=True)
    os.makedirs(os.path.join(root, "CLIPINF"), exist_ok=True)

    playlists = []
    for i, spec in enumerate(argv[2:], start=1):
        name, rest = spec.split("=", 1)
        clip, seconds, *rest = rest.split(",")
        seconds = float(seconds)
        # Where the stream starts, when the caller knows. The default is the
        # value a real disc happened to carry, for a caller that does not.
        start = float(rest[0]) if rest else 0.463
        number = f"{i:05d}"
        marks = [0.0, seconds / 2]
        made = (20, 26, 8, 17 + i, 1, 0, 0)
        os.replace(
            os.path.join(root, "STREAM", clip),
            os.path.join(root, "STREAM", number + ".m2ts"),
        )
        with open(os.path.join(root, "PLAYLIST", number + ".rpls"), "wb") as f:
            f.write(rpls(number, seconds, name, made, marks, start))
        with open(os.path.join(root, "CLIPINF", number + ".clpi"), "wb") as f:
            f.write(clpi(number))
        playlists.append(number + ".rpls")

    with open(os.path.join(root, "info.bdav"), "wb") as f:
        f.write(info_bdav(playlists, "テストディスク"))
    print(f"wrote {len(playlists)} playlist(s) under {root}")


if __name__ == "__main__":
    main(sys.argv)
