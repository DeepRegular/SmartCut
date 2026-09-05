#!/usr/bin/env python3
"""The small index files a Blu-ray carries, written around streams that exist.

Both halves of the specification are here, because both are read by one
reader and a test that covered one of them would leave the other's dialect to
a real disc nobody can check into a repository.

    disc_index.py bdav <dir> <name>=<clip>,<seconds>[,<in>] ...
    disc_index.py bdmv <dir> <clip>,<seconds>[,<in>] ...

`<dir>` is the folder that holds `BDAV/` or `BDMV/`; the streams are already
in its `STREAM/` directory. `<in>` is where the recording begins on the
stream's own clock -- what `ffprobe` reports as the container's start time. It
matters because a playlist counts in that clock and nothing else: an IN point
that is not in the stream makes a disc whose chapter marks point nowhere,
which is a disc no authoring tool would write.

The offsets here are the ones read off discs written by real tools -- a
recorder's for BDAV, an authoring tool's for BDMV -- and the reader in
`rust/crates/core/src/disc.rs` was made to match those discs rather than this
file; what this catches is a change to either of them.
"""
import os
import struct
import sys

# The clock a playlist counts in.
TICK = 45000
# Where the real files put each part of a BDAV playlist. The reader knows
# these numbers; so does every recorder.
NAME_LEN_AT = 88
MADE_AT = 50
TABLE_AT = 320
MARK_STRIDE = 46
# What a pressed disc writes instead: a mark is fourteen bytes, with the play
# item it belongs to two in and the time four in.
BDMV_MARK_STRIDE = 14


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


# --- what a clip carries -------------------------------------------------

def clpi(magic, streams):
    """A clip's own index, as far as anything reads it: its stream list.

    `streams` is a list of (PID, coding type, attribute bytes). What the
    reader takes from it is the kind of each stream, the PID it sits on and
    what the disc says it is -- which is what a chooser lays out.

    The two dialects sign the file differently and write the same thing after
    it: a pressed disc's opens `HDMV`, a recorder's opens `M2TS`.

    The streams named here are a real disc's, written beside a fixture stream
    that carries fewer of them. What is under test is the reading of the
    index, and an index that described only what ffmpeg happened to mux would
    exercise none of the cases a disc actually presents.
    """
    body = b""
    for pid, coding, attr in streams:
        info = bytes([coding]) + attr
        body += struct.pack(">HB", pid, len(info)) + info
    # One program sequence, starting at packet zero, its map on the PID a
    # Blu-ray always puts it on.
    seq = struct.pack(">IHBB", 0, 0x0100, len(streams), 0) + body
    program = struct.pack(">IBB", len(seq) + 2, 0, 1) + seq
    # The header names where each section starts; only the program info is
    # read, and the sections that follow it are not written at all.
    head = bytearray(40)
    head[0:8] = magic
    struct.pack_into(">I", head, 8, 40)          # sequence info
    struct.pack_into(">I", head, 12, 40)         # program info
    struct.pack_into(">I", head, 16, 0)          # CPI
    struct.pack_into(">I", head, 20, 0)          # clip mark
    struct.pack_into(">I", head, 24, 0)          # extension data
    return bytes(head) + program


# What a Japanese recording carries, and what a pressed disc carries. The
# attribute bytes after the coding type are the ones the specification puts
# there: a shape and a rate for video, a channel arrangement, a rate and a
# language for sound, a language alone for graphics.
# The BDAV list is the one read off a disc TMSR6 wrote, with a second sound
# track added: a recorder cuts the language field short, and its private
# stream -- the captions -- says nothing about itself at all.
BROADCAST = [
    (0x1100, 0x02, bytes([0x44, 0x30])),              # MPEG-2 1080i 29.97
    (0x1101, 0x0F, bytes([0x31, 0x00])),              # AAC stereo 48kHz
    (0x1103, 0x0F, bytes([0x31, 0x00])),              # the dub
    (0x1102, 0x06, b""),                              # the caption stream
]
PRESSED = [
    (0x1011, 0x1B, bytes([0x61, 0x20, 0x00])),        # H.264 1080p 23.976
    (0x1100, 0x83, bytes([0x61, ord("e"), ord("n"), ord("g")])),  # TrueHD 5.1
    (0x1101, 0x83, bytes([0x31, ord("j"), ord("p"), ord("n")])),  # TrueHD 2.0
    (0x1200, 0x90, bytes([ord("e"), ord("n"), ord("g")])),        # subtitles
    (0x1400, 0x91, bytes([ord("e"), ord("n"), ord("g")])),        # the menu
]


# --- BDAV: a disc of recordings ------------------------------------------

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


def write_bdav(where, specs):
    root = os.path.join(where, "BDAV")
    os.makedirs(os.path.join(root, "PLAYLIST"), exist_ok=True)
    os.makedirs(os.path.join(root, "CLIPINF"), exist_ok=True)

    playlists = []
    for i, spec in enumerate(specs, start=1):
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
            f.write(clpi(b"M2TS0100", BROADCAST))
        playlists.append(number + ".rpls")

    with open(os.path.join(root, "info.bdav"), "wb") as f:
        f.write(info_bdav(playlists, "テストディスク"))
    return root, playlists


# --- BDMV: a pressed disc ------------------------------------------------

def play_item(clip, in_time, out_time, streams):
    """One PlayItem, with the STN table a player picks its tracks from.

    The reader does not read the STN table -- it takes what a clip carries
    from `CLIPINF`, which is the answer for the clip rather than for one way
    of playing it -- but a playlist without one is not a playlist, and writing
    it is what proves the item's length is what carries the reader past it.
    """
    kinds = [0, 0, 0, 0, 0, 0, 0]
    entries = b""
    for pid, coding, attr in streams:
        at = {0x02: 0, 0x1B: 0, 0x83: 1, 0x90: 2, 0x91: 3}.get(coding, 1)
        kinds[at] += 1
        entry = struct.pack(">BH", 1, pid)
        info = bytes([coding]) + attr
        entries += bytes([len(entry)]) + entry + bytes([len(info)]) + info
    stn = struct.pack(">H", 0) + bytes(kinds) + b"\0" * 5 + entries
    stn = struct.pack(">H", len(stn)) + stn

    body = struct.pack(">5s4sHBII", clip.encode(), b"M2TS", 0, 0, in_time, out_time)
    # The UO mask, the random access flag, the still mode, and the two bytes
    # for a still's length -- which a player skips whether or not there is one.
    body += b"\0" * 8 + bytes([0, 0]) + b"\0\0"
    body += stn
    return struct.pack(">H", len(body)) + body


def mpls(items, marks):
    """One `.mpls`: several PlayItems, and marks that name the one they are on.

    `items` is a list of (clip, in, out) in seconds; `marks` a list of
    (which item, seconds on that item's own clock).
    """
    packed = b"".join(
        play_item(clip, int(a * TICK), int(b * TICK), PRESSED) for clip, a, b in items
    )
    play_list = struct.pack(">HHH", 0, len(items), 0) + packed
    play_list = struct.pack(">I", len(play_list)) + play_list

    entries = b""
    for which, at in marks:
        entry = bytearray(BDMV_MARK_STRIDE)
        entry[1] = 1  # an entry mark, which is what a chapter is
        struct.pack_into(">H", entry, 2, which)
        struct.pack_into(">I", entry, 4, int(at * TICK))
        entries += bytes(entry)
    play_marks = struct.pack(">H", len(marks)) + entries
    play_marks = struct.pack(">I", len(play_marks)) + play_marks

    # What a pressed disc says about itself before the playlist proper: the
    # kind of playback, and whether it is a "play all". Nothing a recorder
    # would write, and nothing this reader reads.
    head = bytearray(40)
    head[0:8] = b"MPLS0200"
    app = struct.pack(">IBBHH", 10, 0, 1, 0, 0)
    body = bytes(head) + app
    list_at = (len(body) + 3) & ~3
    body += b"\0" * (list_at - len(body))
    marks_at = list_at + len(play_list)

    raw = bytearray(body + play_list + play_marks)
    struct.pack_into(">I", raw, 8, list_at)
    struct.pack_into(">I", raw, 12, marks_at)
    struct.pack_into(">I", raw, 16, 0)
    return bytes(raw)


def index_bdmv():
    """The titles, for a player's own menu. Not read; a disc without one is
    not a disc."""
    raw = bytearray(40)
    raw[0:8] = b"INDX0200"
    struct.pack_into(">I", raw, 8, 40)
    struct.pack_into(">I", raw, 12, 0)
    return bytes(raw)


def bdmt(name):
    """What the disc calls itself."""
    return (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<disclib xmlns="urn:BDA:bdmv;disclib">\n'
        '  <di:discinfo xmlns:di="urn:BDA:bdmv;discinfo">\n'
        f"    <di:title><di:name>{name}</di:name></di:title>\n"
        "    <di:description><di:tableOfContents>\n"
        '      <di:titleName titleNumber="1">T01 Feature</di:titleName>\n'
        "    </di:tableOfContents></di:description>\n"
        "  </di:discinfo>\n</disclib>\n"
    ).encode()


def write_bdmv(where, specs):
    """A pressed disc: an episode each, a "play all" over the lot, and a logo.

    Which is the shape the chooser exists for. The logo is short and the
    episodes are not, so a reader that offers only what is worth a look has
    something to be right or wrong about.
    """
    root = os.path.join(where, "BDMV")
    for part in ("PLAYLIST", "CLIPINF", "META/DL"):
        os.makedirs(os.path.join(root, part), exist_ok=True)

    clips = []
    for i, spec in enumerate(specs, start=1):
        clip, seconds, *rest = spec.split(",")
        seconds = float(seconds)
        start = float(rest[0]) if rest else 0.463
        number = f"{i:05d}"
        os.replace(
            os.path.join(root, "STREAM", clip),
            os.path.join(root, "STREAM", number + ".m2ts"),
        )
        with open(os.path.join(root, "CLIPINF", number + ".clpi"), "wb") as f:
            f.write(clpi(b"HDMV0200", PRESSED))
        clips.append((number, start, start + seconds))

    # The "play all" first, because that is where a disc puts it and because
    # a reader that takes the first playlist's answer for the marks would be
    # taking the wrong one: only the playlists below name one clip each, and
    # only they can say where a chapter is without being asked which episode.
    names = []
    play_all = "00001.mpls"
    marks = [(i, a) for i, (_, a, _) in enumerate(clips)]
    with open(os.path.join(root, "PLAYLIST", play_all), "wb") as f:
        f.write(mpls(clips, marks))
    names.append(play_all)
    for i, (number, a, b) in enumerate(clips, start=2):
        name = f"{i:05d}.mpls"
        with open(os.path.join(root, "PLAYLIST", name), "wb") as f:
            f.write(mpls([(number, a, b)], [(0, a), (0, (a + b) / 2)]))
        names.append(name)

    with open(os.path.join(root, "index.bdmv"), "wb") as f:
        f.write(index_bdmv())
    with open(os.path.join(root, "META/DL/bdmt_eng.xml"), "wb") as f:
        f.write(bdmt("Smartcut Test Disc"))
    return root, names


def main(argv):
    if len(argv) < 4:
        raise SystemExit(__doc__)
    kind, where, specs = argv[1], argv[2], argv[3:]
    if kind == "bdav":
        root, names = write_bdav(where, specs)
    elif kind == "bdmv":
        root, names = write_bdmv(where, specs)
    else:
        raise SystemExit(f"{kind}: want bdav or bdmv")
    print(f"wrote {len(names)} playlist(s) under {root}")


if __name__ == "__main__":
    main(sys.argv)
