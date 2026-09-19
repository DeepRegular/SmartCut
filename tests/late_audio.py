"""Hide a recording's second sound track from the head of the file.

A broadcast announces what it is sending when the programme starts, and a
recorder starts before that: the map at the head of the file describes
whatever was on before, and the second sound track is named only once the
programme begins. libavformat stops probing after five megabytes and so never
sees it.

There is no way to ask ffmpeg for that shape, so it is made here. Two things
can be held back, separately.

`--hide-data N` replaces every packet of the second sound track in the first
N bytes with a null packet: the track is named by the map from the first
frame, but there is nothing on its pid yet. libavformat then lists a stream
it cannot describe -- no sample rate, no channel count -- and a reader that
believes the description drops it. Only a probe deep enough to reach the
track's first frames describes it.

`--hide-map N` rewrites every copy of the program map in the first N bytes to
name one sound track instead of two.

Both maps gain the component tag ARIB names a stream by -- 0x10 for the main
sound, 0x11 for the second -- which ffmpeg does not write and which is what
says which of the two a viewer is meant to hear.

    late_audio.py in.ts out.ts --hide-data 7000000

Hiding the track in the map alone does not hide it from libavformat, which
registers a stream for any pid carrying recognisable PES whether or not a map
mentions it. What makes a second track invisible is that it is not there yet.

Only maps that fit in one packet are rewritten, which is every map ffmpeg
writes: the section is replaced in place and what is left of the payload is
stuffed with 0xFF, as a transport stream pads any short table.
"""
import argparse

SZ = 188
AUDIO_TYPES = (0x03, 0x04, 0x0F, 0x11)


def crc32(data):
    """The CRC a section carries, MPEG-2's own."""
    crc = 0xFFFFFFFF
    for b in data:
        crc ^= b << 24
        for _ in range(8):
            crc = ((crc << 1) ^ 0x04C11DB7) & 0xFFFFFFFF if crc & 0x80000000 else (crc << 1) & 0xFFFFFFFF
    return crc


def pmt_pid_of(packet):
    """The map's PID, out of a program association table packet."""
    payload = packet[4 + 1 + packet[4]:] if packet[3] & 0x20 else packet[4:]
    sec = payload[1 + payload[0]:]
    if not sec or sec[0] != 0x00:
        return None
    n = ((sec[1] & 0x0F) << 8 | sec[2]) + 3
    for i in range(8, min(n - 4, len(sec)), 4):
        if sec[i] << 8 | sec[i + 1]:
            return (sec[i + 2] & 0x1F) << 8 | sec[i + 3]
    return None


def second_sound_pid(sec):
    """The pid of the second sound track a map names, where it names two."""
    n = ((sec[1] & 0x0F) << 8 | sec[2]) + 3
    if n > len(sec):
        return None
    i, seen = 12 + ((sec[10] & 0x0F) << 8 | sec[11]), 0
    while i + 5 <= n - 4:
        el = (sec[i + 3] & 0x0F) << 8 | sec[i + 4]
        if sec[i] in AUDIO_TYPES:
            seen += 1
            if seen == 2:
                return (sec[i + 1] & 0x1F) << 8 | sec[i + 2]
        i += 5 + el
    return None


def rewrite(sec, keep_second):
    """One map, with the component tags added and maybe a track taken out.

    The two halves are given different version numbers, because that is what
    a demuxer reads: a map whose version has not changed is the map it
    already has, and it will not look at the streams in it however deeply it
    is told to probe. A broadcast bumps the version when it replaces a map,
    and so does this.
    """
    n = ((sec[1] & 0x0F) << 8 | sec[2]) + 3
    if n > len(sec):
        return None
    head = bytearray(sec[:12 + ((sec[10] & 0x0F) << 8 | sec[11])])
    head[5] = 0xC1 | (int(keep_second) << 1)
    out = bytearray()
    i, seen = len(head), 0
    while i + 5 <= n - 4:
        stype = sec[i]
        el = (sec[i + 3] & 0x0F) << 8 | sec[i + 4]
        entry = bytearray(sec[i:i + 5 + el])
        i += 5 + el
        if stype in AUDIO_TYPES:
            seen += 1
            if seen == 2 and not keep_second:
                continue
            # The component tag, which ffmpeg does not write.
            tag = 0x10 + seen - 1
            entry += bytes([0x52, 0x01, tag])
            el += 3
            entry[3] = 0xF0 | ((el >> 8) & 0x0F)
            entry[4] = el & 0xFF
        out += entry
    whole = head + out
    length = len(whole) - 3 + 4
    whole[1] = 0xB0 | ((length >> 8) & 0x0F)
    whole[2] = length & 0xFF
    return bytes(whole) + crc32(whole).to_bytes(4, "big")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("src")
    ap.add_argument("dst")
    ap.add_argument("--hide-data", type=int, default=0,
                    help="bytes of the head with none of the second track in them")
    ap.add_argument("--hide-map", type=int, default=0,
                    help="bytes of the head whose maps name one track only")
    args = ap.parse_args()

    data = bytearray(open(args.src, "rb").read())
    start = data.find(b"\x47")
    pmt_pid, second_pid, rewritten, hidden, dropped = None, None, 0, 0, 0
    started = args.hide_data == 0
    # A null packet: the pid reserved for padding, which every demuxer skips.
    null = bytes([0x47, 0x1F, 0xFF, 0x10]) + b"\xff" * (SZ - 4)
    for at in range(start, len(data) - SZ + 1, SZ):
        p = data[at:at + SZ]
        if p[0] != 0x47:
            break
        pid = ((p[1] & 0x1F) << 8) | p[2]
        if pid == 0x00 and pmt_pid is None:
            pmt_pid = pmt_pid_of(p)
            continue
        if pid == second_pid and (at < args.hide_data or not started):
            # Past the hidden region, keep nulling until a packet that opens
            # a PES: a track that starts late starts at the beginning of one,
            # and half of a PES is not something a decoder can read the
            # track's shape out of.
            if at >= args.hide_data and p[1] & 0x40:
                started = True
            else:
                data[at:at + SZ] = null
                dropped += 1
                continue
        if pid != pmt_pid or not (p[1] & 0x40):
            continue
        j = 4 + (1 + p[4] if p[3] & 0x20 else 0)
        sec = p[j + 1 + p[j]:]
        if not sec or sec[0] != 0x02:
            continue
        keep = at >= args.hide_map
        if second_pid is None:
            second_pid = second_sound_pid(bytes(sec))
        made = rewrite(bytes(sec), keep)
        if made is None or j + 1 + len(made) > SZ:
            continue
        body = bytes([0x00]) + made
        data[at + j:at + SZ] = body + b"\xff" * (SZ - j - len(body))
        rewritten += 1
        hidden += 0 if keep else 1
    open(args.dst, "wb").write(data)
    print(f"{rewritten} map(s) rewritten, {hidden} of them naming one sound track; "
          f"{dropped} packet(s) of that track taken out of the head")


main()
