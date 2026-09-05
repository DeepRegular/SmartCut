#!/usr/bin/env python3
"""The tables a DVD-Video carries, written around a stream that exists.

    dvd_index.py <VIDEO_TS dir> <cells> <title> [<title> ...]

`<cells>` is how many cells to cut the title set's stream into; `<title>` is
the run of them a title plays, `1-4`, counting cells from one. One program
starts at each cell, so a title of four cells has four chapters -- which is
how the disc this was written against is laid out, and the only shape a
chapter point can be checked against.

The stream is already in the directory, as `VTS_01_1.VOB` and however many
more the caller split it into. Two things are done to it and around it.

**The navigation packs are filled in.** Every VOBU opens with one, and
ffmpeg's DVD muxer writes them with the presentation times left at zero --
which is legal enough for a player that never seeks and useless for a reader
that has to put a chapter on the stream's own clock. So each pack is found,
the pictures behind it are read for when they are shown, and `VOBU_S_PTM` and
`VOBU_E_PTM` are written where a real authoring tool would have written them.
Nothing else in the pack is touched and no byte moves.

**The `.IFO` tables are written.** Only the parts a reader needs: the
manager's table of titles, and the title set's parts-of-title table, program
chain table and attributes. The offsets are the ones read off a disc mastered
by Sonic Scenarist in 2009, and `rust/crates/core/src/dvd.rs` was made to
match that disc rather than this file; what this catches is a change to
either of them.
"""
import os
import struct
import sys

SECTOR = 2048
# The clock a program stream's timestamps are on.
PTM = 90000

# Where the reader looks in a manager table.
VMG_TITLE_SETS_AT = 0x3E
VMG_TT_SRPT_AT = 0xC4
# ... and in a title set's.
VTS_VOBS_AT = 0xC4
VTS_PTT_SRPT_AT = 0xC8
VTS_PGCIT_AT = 0xCC
VTS_VIDEO_ATTR_AT = 0x200
VTS_AUDIO_COUNT_AT = 0x203
VTS_AUDIO_ATTR_AT = 0x204
VTS_SUBP_COUNT_AT = 0x255

# What the index claims the picture is: MPEG-2, 525/60, 4:3, 720x480. The
# fixture really is that, which is the only reason it can be claimed.
VIDEO_ATTR = bytes([0x43, 0x00])
# One MPEG-1 audio track in Japanese, 48 kHz, stereo -- what the fixture
# carries, described the way a disc describes it.
AUDIO_ATTR = bytes([0x44, 0x01]) + b"ja" + bytes(4)


def be32(n):
    return struct.pack(">I", n)


def be16(n):
    return struct.pack(">H", n)


def put(buf, at, raw):
    buf[at:at + len(raw)] = raw


def bcd(n):
    return ((n // 10) << 4) | (n % 10)


def dvd_time(seconds):
    """A `dvd_time_t`: hours, minutes and seconds in binary coded decimal,
    and a frame count with the rate in its top two bits. 30000/1001 here,
    which is what the fixture runs at."""
    rate = 30000 / 1001
    whole = int(seconds)
    frames = min(int(round((seconds - whole) * rate)), 29)
    h, m, s = whole // 3600, (whole // 60) % 60, whole % 60
    return bytes([bcd(h), bcd(m), bcd(s), 0xC0 | bcd(frames)])


def seconds_of(raw):
    """A `dvd_time_t` back into seconds, so that what is printed for the
    tests to check is what the reader will read rather than what was meant."""
    un = lambda x: (x >> 4) * 10 + (x & 0xF)
    return (
        un(raw[0]) * 3600.0
        + un(raw[1]) * 60.0
        + un(raw[2])
        + un(raw[3] & 0x3F) / (30000 / 1001)
    )


# --- the stream ----------------------------------------------------------


def pack_payload_at(sector):
    """Where the first PES packet in a pack starts: past the fourteen byte
    pack header, its stuffing, and a system header if there is one."""
    at = 14 + (sector[13] & 7)
    if sector[at:at + 4] == b"\x00\x00\x01\xbb":
        at += 6 + struct.unpack(">H", sector[at + 4:at + 6])[0]
    return at


def is_nav(sector):
    if sector[:4] != b"\x00\x00\x01\xba":
        return False
    at = pack_payload_at(sector)
    return sector[at:at + 4] == b"\x00\x00\x01\xbf" and sector[at + 6] == 0x00


def video_pts(sector):
    """Every video presentation time in one pack, in the order they appear."""
    out = []
    at = pack_payload_at(sector)
    while at + 6 <= len(sector) and sector[at:at + 3] == b"\x00\x00\x01":
        sid = sector[at + 3]
        length = struct.unpack(">H", sector[at + 4:at + 6])[0]
        if 0xE0 <= sid <= 0xEF and length >= 5:
            flags = sector[at + 7]
            if flags & 0x80:  # a presentation time is present
                b = sector[at + 9:at + 14]
                out.append(
                    ((b[0] >> 1 & 0x07) << 30)
                    | (b[1] << 22)
                    | ((b[2] >> 1 & 0x7F) << 15)
                    | (b[3] << 7)
                    | (b[4] >> 1)
                )
        at += 6 + length
    return out


def audio_streams(data):
    """Which MPEG audio streams the stream carries, by their PES id.

    A DVD's index has to declare them, and declaring one where there are two
    would make a test that agreed with itself rather than with the stream."""
    ids = set()
    for s in range(min(len(data) // SECTOR, 512)):
        sector = memoryview(data)[s * SECTOR:(s + 1) * SECTOR]
        if sector[:4] != b"\x00\x00\x01\xba":
            continue
        at = pack_payload_at(sector)
        while at + 6 <= SECTOR and bytes(sector[at:at + 3]) == b"\x00\x00\x01":
            sid = sector[at + 3]
            if 0xC0 <= sid <= 0xC7:
                ids.add(sid)
            at += 6 + struct.unpack(">H", bytes(sector[at + 4:at + 6]))[0]
    return sorted(ids)


def read_stream(video_ts):
    """The title set's stream, and the files it is written in."""
    names = []
    for n in range(1, 10):
        path = os.path.join(video_ts, "VTS_01_%d.VOB" % n)
        if not os.path.exists(path):
            break
        names.append(path)
    if not names:
        sys.exit("no VTS_01_n.VOB in %s" % video_ts)
    return bytearray(b"".join(open(p, "rb").read() for p in names)), names


def write_stream(data, names):
    """Back into the same files, at the same lengths."""
    at = 0
    for path in names:
        size = os.path.getsize(path)
        with open(path, "wb") as f:
            f.write(data[at:at + size])
        at += size


def fill_nav_packs(data):
    """Give every navigation pack the times its VOBU is shown at.

    Returns the sector each VOBU begins at, and when it begins and ends, so
    that the cells written below can be cut on a VOBU and timed off the
    stream rather than off an assumption about it."""
    sectors = len(data) // SECTOR
    navs = [s for s in range(sectors) if is_nav(memoryview(data)[s * SECTOR:(s + 1) * SECTOR])]
    if not navs:
        sys.exit("this stream has no navigation packs: mux it with -f dvd")

    out = []
    for i, s in enumerate(navs):
        stop = navs[i + 1] if i + 1 < len(navs) else sectors
        times = []
        for k in range(s, stop):
            times += video_pts(memoryview(data)[k * SECTOR:(k + 1) * SECTOR])
        if not times:
            continue
        start, end = min(times), max(times)
        at = s * SECTOR + pack_payload_at(memoryview(data)[s * SECTOR:(s + 1) * SECTOR])
        gi = at + 7  # past the PES header and the substream byte
        put(data, gi, be32(s))              # NV_PCK_LBN
        put(data, gi + 0x0C, be32(start))   # VOBU_S_PTM
        put(data, gi + 0x10, be32(end))     # VOBU_E_PTM
        out.append((s, start, end))
    return out


# --- the tables ----------------------------------------------------------


def tt_srpt(titles):
    """The manager's table of titles: which set each is in, and which of that
    set's titles it is."""
    body = b""
    for i, chapters in enumerate(titles):
        body += bytes([0x3C, 0x01])       # playback type, one angle
        body += be16(chapters)
        body += be16(1)                   # parental id
        body += bytes([1, i + 1])         # title set 1, its title i+1
        body += be32(0)                   # where the set starts; unread here
    return be16(len(titles)) + be16(0) + be32(7 + len(body)) + body


def vts_ptt_srpt(titles):
    """For each of the set's titles, the `(chain, program)` pairs that are
    its chapters."""
    heads = 8 + 4 * len(titles)
    offsets, body = b"", b""
    for i, chapters in enumerate(titles):
        offsets += be32(heads + len(body))
        for pgn in range(1, chapters + 1):
            body += be16(i + 1) + be16(pgn)
    table = be16(len(titles)) + be16(0) + be32(heads + len(body) - 1) + offsets + body
    return table


def pgc(cells, first, last):
    """One program chain: the cells it plays, and a program starting at each.

    `first` and `last` count cells from one, inclusive."""
    mine = cells[first - 1:last]
    length = sum(c[2] - c[1] for c in mine) / PTM
    n = len(mine)
    map_at = 0xEC
    cells_at = map_at + n + (-(map_at + n) % 2)

    g = bytearray(cells_at + n * 24)
    g[2] = n                                   # programs
    g[3] = n                                   # cells
    put(g, 4, dvd_time(length))
    put(g, 0xE6, be16(map_at))
    put(g, 0xE8, be16(cells_at))
    put(g, 0xEA, be16(0))
    for i in range(n):
        g[map_at + i] = i + 1                  # program i starts at cell i+1
        at = cells_at + i * 24
        first_sector, start, end, last_vobu, last_sector = mine[i]
        put(g, at + 4, dvd_time((end - start) / PTM))
        put(g, at + 8, be32(first_sector))
        put(g, at + 0x10, be32(last_vobu))     # last VOBU's first sector
        put(g, at + 0x14, be32(last_sector))
    return bytes(g), length


def vtsi(cells, titles, vobs_sectors, audios):
    """`VTS_01_0.IFO`: the parts-of-title table at sector 1, the program
    chains at sector 2, and the attributes in the header."""
    ptt = vts_ptt_srpt(titles)

    chains, lengths = [], []
    for first, last in titles_as_runs(titles):
        raw, length = pgc(cells, first, last)
        chains.append(raw)
        lengths.append(length)
    heads = 8 + 8 * len(chains)
    table = bytearray(be16(len(chains)) + be16(0))
    body = b""
    entries = b""
    for i, raw in enumerate(chains):
        entries += be32(0x80000000 | (i + 1)) + be32(heads + len(body))
        body += raw
    pgcit = bytes(table) + be32(heads + len(body) - 1) + entries + body

    sectors = 2 + (len(pgcit) + SECTOR - 1) // SECTOR
    mat = bytearray(SECTOR)
    put(mat, 0, b"DVDVIDEO-VTS")
    put(mat, 0x0C, be32(sectors + vobs_sectors - 1))
    put(mat, 0x1C, be32(sectors - 1))
    mat[0x21] = 0x11
    put(mat, 0x80, be32(sectors * SECTOR - 1))
    put(mat, VTS_VOBS_AT, be32(sectors))
    put(mat, VTS_PTT_SRPT_AT, be32(1))
    put(mat, VTS_PGCIT_AT, be32(2))
    put(mat, VTS_VIDEO_ATTR_AT, VIDEO_ATTR)
    mat[VTS_AUDIO_COUNT_AT] = len(audios)
    for i in range(len(audios)):
        put(mat, VTS_AUDIO_ATTR_AT + i * 8, AUDIO_ATTR)
    mat[VTS_SUBP_COUNT_AT] = 0

    raw = bytes(mat)
    raw += ptt.ljust(SECTOR, b"\0")
    raw += pgcit.ljust((len(pgcit) + SECTOR - 1) // SECTOR * SECTOR, b"\0")
    return raw, lengths


def titles_as_runs(titles):
    """Each title as the run of cells it plays. A title of `n` chapters plays
    the first `n` cells, which is the shape the caller asks for -- and is what
    a disc holding a programme and the same programme without its ending
    looks like."""
    return [(1, n) for n in titles]


def vmgi(titles):
    mat = bytearray(SECTOR)
    put(mat, 0, b"DVDVIDEO-VMG")
    mat[0x21] = 0x11
    put(mat, VMG_TITLE_SETS_AT, be16(1))
    put(mat, VMG_TT_SRPT_AT, be32(1))
    put(mat, 0x0C, be32(1))
    put(mat, 0x1C, be32(1))
    put(mat, 0x80, be32(2 * SECTOR - 1))
    srpt = tt_srpt(titles)
    return bytes(mat) + srpt.ljust(SECTOR, b"\0")


def main():
    if len(sys.argv) < 4:
        sys.exit(__doc__)
    video_ts, want_cells = sys.argv[1], int(sys.argv[2])
    titles = [int(a) for a in sys.argv[3:]]

    data, names = read_stream(video_ts)
    vobus = fill_nav_packs(data)
    write_stream(data, names)
    sectors = len(data) // SECTOR

    if want_cells > len(vobus):
        sys.exit("only %d VOBUs in this stream; asked for %d cells" % (len(vobus), want_cells))
    if max(titles) > want_cells:
        sys.exit("a title of %d chapters needs %d cells" % (max(titles), max(titles)))

    # Cells of a whole number of VOBUs, as near equal as they divide.
    edges = [round(i * len(vobus) / want_cells) for i in range(want_cells)] + [len(vobus)]
    cells = []
    for i in range(want_cells):
        mine = vobus[edges[i]:edges[i + 1]]
        last = vobus[edges[i + 1]][0] - 1 if edges[i + 1] < len(vobus) else sectors - 1
        # (first sector, start time, end time, last VOBU's first sector, last sector)
        cells.append((mine[0][0], mine[0][1], mine[-1][2], mine[-1][0], last))

    ifo, _ = vtsi(cells, titles, sectors, audio_streams(data))
    open(os.path.join(video_ts, "VTS_01_0.IFO"), "wb").write(ifo)
    open(os.path.join(video_ts, "VTS_01_0.BUP"), "wb").write(ifo)
    mgr = vmgi(titles)
    open(os.path.join(video_ts, "VIDEO_TS.IFO"), "wb").write(mgr)
    open(os.path.join(video_ts, "VIDEO_TS.BUP"), "wb").write(mgr)

    # What the shell needs to know to check the answers against. Every
    # duration here is a sum of cell lengths *as written* -- binary coded
    # decimal down to the frame -- because that is what the reader adds up,
    # and a check against the timestamps they were quantised from would be a
    # check against a number nothing produces.
    quantised = [seconds_of(dvd_time((c[2] - c[1]) / PTM)) for c in cells]
    print("start %.6f" % (cells[0][1] / PTM))
    for i, chapters in enumerate(titles):
        print("title %d %.3f" % (i + 1, sum(quantised[:chapters])))
    for i, c in enumerate(cells):
        print("cell %d %d %d %.3f" % (i + 1, c[0], c[4], (c[2] - c[1]) / PTM))
    print("chapters " + " ".join(
        "%.3f" % sum(quantised[:i]) for i in range(len(cells))
    ))


if __name__ == "__main__":
    main()
