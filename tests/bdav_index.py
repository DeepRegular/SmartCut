#!/usr/bin/env python3
"""Read the index of a disc SmartCut wrote, and check it against the streams.

    bdav_index.py <disc folder>

The reader in `rust/crates/core/src/disc.rs` already says whether a disc can
be opened, and `run_bdav_tests.sh` asks it. What it cannot say is whether the
numbers inside the index are *true* -- it never looks at the entry point map,
the arrival times or the clip index's account of the stream, because nothing
in this program reads those. A player does, and this stands in for one.

What is printed is `key=value` lines, one per recording, for the shell to
compare against what it asked for.
"""
import os
import struct
import sys

SOURCE_PACKET = 192
PACKET = 188
TICK = 45000


def u16(b, at):
    return struct.unpack_from(">H", b, at)[0]


def u32(b, at):
    return struct.unpack_from(">I", b, at)[0]


# --- the stream ----------------------------------------------------------

def arrivals(path):
    """When each packet arrived, and whether that ever goes backwards.

    The field is thirty bits of a 27 MHz clock, so it wraps about every forty
    seconds; a step that is nearly the whole of its range is a wrap and not a
    step backwards.
    """
    size = os.path.getsize(path)
    n = size // SOURCE_PACKET
    total = 0
    back = 0
    prev = None
    with open(path, "rb") as f:
        for i in range(n):
            f.seek(i * SOURCE_PACKET)
            at = struct.unpack(">I", f.read(4))[0] & 0x3FFFFFFF
            if prev is not None:
                step = (at - prev) % (1 << 30)
                if step > (1 << 29):
                    back += 1
                total += step
            prev = at
    return n, total / 27e6, back


def pmt_of(path):
    """The map the stream carries: which PID is what, as the stream says."""
    with open(path, "rb") as f:
        data = f.read(8 << 20)
    pmt_pid = None
    out = {}
    pcr = None
    for i in range(0, len(data) - SOURCE_PACKET, SOURCE_PACKET):
        p = data[i + 4 : i + SOURCE_PACKET]
        if p[0] != 0x47:
            break
        pid = ((p[1] & 0x1F) << 8) | p[2]
        afc = (p[3] >> 4) & 3
        if afc == 2:
            continue
        payload = p[5 + p[4] :] if afc == 3 else p[4:]
        if p[1] & 0x40:
            payload = payload[1 + payload[0] :]
        if pid == 0 and pmt_pid is None:
            n = ((payload[1] & 0xF) << 8) | payload[2]
            body = payload[8 : 3 + n - 4]
            for j in range(0, len(body), 4):
                if body[j] << 8 | body[j + 1]:
                    pmt_pid = ((body[j + 2] & 0x1F) << 8) | body[j + 3]
        elif pmt_pid and pid == pmt_pid and not out:
            n = ((payload[1] & 0xF) << 8) | payload[2]
            pcr = ((payload[8] & 0x1F) << 8) | payload[9]
            j = 12 + (((payload[10] & 0xF) << 8) | payload[11])
            end = 3 + n - 4
            while j < end:
                out[((payload[j + 1] & 0x1F) << 8) | payload[j + 2]] = payload[j]
                j += 5 + (((payload[j + 3] & 0xF) << 8) | payload[j + 4])
            break
    return pcr, out


# --- the clip's own index ------------------------------------------------

def clpi(path):
    raw = open(path, "rb").read()
    assert raw[:4] == b"M2TS", "not a recorder's clip index"
    info = 44
    # The sequence: one arrival clock and one presentation clock, then the
    # PID the clock is on and the first and last moment a picture is shown.
    seq = u32(raw, 8) + 4
    out = {
        "rate": u32(raw, info + 8),
        "packets": u32(raw, info + 12),
        "pcr": u16(raw, seq + 8),
        "start": u32(raw, seq + 14),
        "end": u32(raw, seq + 18),
    }
    # The streams, which is what a chooser reads and what a player maps.
    at = u32(raw, 12) + 4
    streams = {}
    p = at + 10
    for _ in range(raw[at + 8]):
        pid = u16(raw, p)
        length = raw[p + 2]
        streams[pid] = raw[p + 3]
        p += 3 + length
    out["streams"] = streams
    out["ep"] = ep_map(raw)
    return out


def ep_map(raw):
    """Every entry point, as a player reconstructs it: (45 kHz time, packet)."""
    body = u32(raw, 16) + 4
    assert u16(raw, body) & 0xF == 1, "not an entry point map"
    d = body + 2
    counts = int.from_bytes(raw[d + 4 : d + 10], "big")
    coarse_n = (counts >> 18) & 0xFFFF
    fine_n = counts & 0x3FFFF
    at = d + u32(raw, d + 10)
    fine_at = at + u32(raw, at)
    coarse = [(u32(raw, at + 4 + i * 8), u32(raw, at + 4 + i * 8 + 4)) for i in range(coarse_n)]
    out = []
    ci = 0
    for i in range(fine_n):
        w = u32(raw, fine_at + i * 4)
        while ci + 1 < coarse_n and (coarse[ci + 1][0] >> 14) <= i:
            ci += 1
        pts = ((coarse[ci][0] & 0x3FFF) & ~1) << 19 | ((w >> 17) & 0x7FF) << 9
        spn = (coarse[ci][1] & ~0x1FFFF) | (w & 0x1FFFF)
        out.append((pts, spn))
    return out


def entries_land(m2ts, pid, ep):
    """Does every entry point name a packet that starts a picture at that time?

    This is the whole claim the map makes, and the one a chapter skip on a
    player depends on. A map that is a few packets out is a map that lands on
    a packet with no PES header in it, which is what this catches.
    """
    wrong = 0
    size = os.path.getsize(m2ts)
    with open(m2ts, "rb") as f:
        for pts, spn in ep:
            at = spn * SOURCE_PACKET
            if at + SOURCE_PACKET > size:
                wrong += 1
                continue
            f.seek(at)
            p = f.read(SOURCE_PACKET)[4:]
            got = ((p[1] & 0x1F) << 8) | p[2]
            afc = (p[3] >> 4) & 3
            payload = p[5 + p[4] :] if afc == 3 else p[4:]
            if got != pid or not p[1] & 0x40 or payload[:3] != b"\x00\x00\x01":
                wrong += 1
                continue
            if not payload[7] & 0x80:
                wrong += 1
                continue
            b = payload[9:14]
            had = ((b[0] >> 1 & 7) << 30) | (b[1] << 22) | ((b[2] >> 1) << 15) | (b[3] << 7) | (b[4] >> 1)
            # The map keeps the time to the nine bits below 45 kHz, which is
            # a fifth of a millisecond.
            if abs(had // 2 - pts) > 512:
                wrong += 1
    return wrong


# --- the playlist and the disc -------------------------------------------

def rpls(path):
    raw = open(path, "rb").read()
    assert raw[:4] == b"PLST", "not a recorder's playlist"
    marks_at = u32(raw, 12)
    count = u16(raw, marks_at + 4)
    stride = (u32(raw, marks_at) - 2) // max(count, 1)
    item = u32(raw, 8) + 4 + 8
    return {
        "clip": raw[item : item + 5].decode(),
        "in": u32(raw, item + 12),
        "out": u32(raw, item + 16),
        "marks": [u32(raw, marks_at + 6 + i * stride + 6) for i in range(count)],
        # What the playlist says about the recording rather than about the
        # stream. The text is ARIB and is not decoded here -- this is a
        # check that the fields are there and the right size, and the reader
        # in disc.rs is what says they read back as the right words.
        "made": raw[50:57].hex(),
        "channel_number": u16(raw, 64),
        "channel_length": raw[67],
        "name_length": raw[88],
        "description_length": u16(raw, 344),
    }


def main():
    at = os.path.join(sys.argv[1], "BDAV")
    raw = open(os.path.join(at, "info.bdav"), "rb").read()
    table = u32(raw, 8)
    names = [
        raw[table + 6 + i * 10 : table + 16 + i * 10].decode()
        for i in range(u16(raw, table + 4))
    ]
    print(f"playlists={len(names)}")
    for name in names:
        stem = name.split(".")[0]
        play = rpls(os.path.join(at, "PLAYLIST", name))
        m2ts = os.path.join(at, "STREAM", stem + ".m2ts")
        clip = clpi(os.path.join(at, "CLIPINF", stem + ".clpi"))
        packets, seconds, back = arrivals(m2ts)
        pcr, streams = pmt_of(m2ts)
        video = next((pid for pid, kind in streams.items() if kind in (0x02, 0x1B, 0xEA, 0x24)), 0)
        wrong = entries_land(m2ts, video, clip["ep"])
        print(f"{stem}.clip={play['clip']}")
        print(f"{stem}.seconds={(play['out'] - play['in']) / TICK:.3f}")
        print(f"{stem}.marks={len(play['marks'])}")
        print(f"{stem}.made={play['made']}")
        print(f"{stem}.channel_number={play['channel_number']}")
        print(f"{stem}.channel_length={play['channel_length']}")
        print(f"{stem}.name_length={play['name_length']}")
        print(f"{stem}.description_length={play['description_length']}")
        print(f"{stem}.mark_in_range={all(play['in'] <= m <= play['out'] for m in play['marks'])}")
        # What the clip index says has to be what the file and its map say.
        print(f"{stem}.packets_agree={clip['packets'] == packets}")
        print(f"{stem}.times_agree={clip['start'] == play['in'] and clip['end'] == play['out']}")
        print(f"{stem}.pcr_agrees={clip['pcr'] == pcr}")
        print(f"{stem}.streams_agree={clip['streams'] == streams}")
        print(f"{stem}.stream_types=" + ",".join(f"{p:04x}:{t:02x}" for p, t in sorted(streams.items())))
        print(f"{stem}.entry_points={len(clip['ep'])}")
        print(f"{stem}.entry_points_wrong={wrong}")
        print(f"{stem}.arrival_seconds={seconds:.3f}")
        print(f"{stem}.arrival_backwards={back}")


if __name__ == "__main__":
    main()
