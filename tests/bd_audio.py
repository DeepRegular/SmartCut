"""What a cut did to a Blu-ray's sound, in the two ways it can be checked.

`types` reads the programme map and prints what each stream is declared as.
A Blu-ray's sound lives in stream types libavformat's own muxer has no
mapping for -- LPCM is 0x80, TrueHD 0x83, E-AC-3 0x84 -- so this is where a
cut that dropped one to "private data" shows up.

`align` decodes a cut and the recording it came from and lines the two up,
because a cut of a transport stream starts on a whole audio frame and not on
the sample the range asked for. What comes back is how much of the cut is the
recording's own samples and how much is not.

`peak` is the loudest sample in a stretch of a cut, which is how the frame a
boundary lands inside is checked: the kept half is loud and the half on the
far side of the cut is silence.
"""
import array
import subprocess
import sys


def packets(data):
    """Every 188 byte packet, whichever framing the file uses.

    A Blu-ray writes 192 byte packets: the same packet behind four bytes
    saying when it arrived.
    """
    stride = 188 if data[:1] == b"\x47" else 192
    base = 0 if stride == 188 else 4
    at = base
    while at + 188 <= len(data):
        if data[at] == 0x47:
            yield data[at : at + 188]
        at += stride


def payload(p):
    """A packet's section payload, or None when it carries none."""
    if not p[1] & 0x40:
        return None
    af = (p[3] >> 4) & 3
    at = 4
    if af in (2, 3):
        at += 1 + p[at]
    if af == 2:
        return None
    return p[at + 1 + p[at] :]


def parse_pmt(path):
    """The programme's own descriptors, and `(pid, stream_type)` per stream."""
    data = open(path, "rb").read(4 << 20)
    pmt_pid = None
    for p in packets(data):
        pid = ((p[1] & 0x1F) << 8) | p[2]
        body = payload(p)
        if body is None:
            continue
        if pid == 0 and pmt_pid is None:
            slen = ((body[1] & 0x0F) << 8) | body[2]
            for i in range(8, slen - 1, 4):
                if ((body[i] << 8) | body[i + 1]) != 0:
                    pmt_pid = ((body[i + 2] & 0x1F) << 8) | body[i + 3]
                    break
        elif pid == pmt_pid:
            slen = ((body[1] & 0x0F) << 8) | body[2]
            pil = ((body[10] & 0x0F) << 8) | body[11]
            at, end = 12 + pil, 3 + slen - 4
            out = []
            while at + 5 <= end:
                out.append((((body[at + 1] & 0x1F) << 8) | body[at + 2], body[at]))
                at += 5 + (((body[at + 3] & 0x0F) << 8) | body[at + 4])
            return bytes(body[12 : 12 + pil]), out
    return b"", []


def stream_types(path):
    """`pid stream_type` for every stream the map names."""
    return parse_pmt(path)[1]


def registrations(path):
    """The four-character names the programme registers itself under.

    A format identifier (descriptor 0x05) is how a transport stream says
    whose meaning to read its stream types by. It is the difference between
    stream type 0x80 meaning HDMV LPCM and 0x80 meaning something else
    entirely, so a cut that writes LPCM into a transport stream has to write
    one of these as well.
    """
    info = parse_pmt(path)[0]
    out, at = [], 0
    while at + 2 <= len(info):
        length = info[at + 1]
        body = info[at + 2 : at + 2 + length]
        if info[at] == 0x05:
            out.append(body.decode("ascii", "replace"))
        at += 2 + length
    return out


def decode(path, channels):
    """The file's sound as 16 bit samples, channels interleaved."""
    raw = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", path, "-vn", "-f", "s16le",
         "-acodec", "pcm_s16le", "-ac", str(channels), "-ar", "48000", "-"],
        capture_output=True, check=True,
    ).stdout
    out = array.array("h")
    out.frombytes(raw)
    return out


def align(src_path, cut_path, channels):
    """Where the cut's sound begins in the recording, and what differs after.

    Searched rather than worked out: where a cut's audio starts depends on
    the framing of the track, and the point of the check is that everything
    from there on is the recording's own bytes.
    """
    src, cut = decode(src_path, channels), decode(cut_path, channels)
    begins, best = 0, None
    for at in range(0, max(len(src) - len(cut), 0) + 1, channels):
        d = sum(1 for i in range(0, min(len(cut), 40000), 7) if src[at + i] != cut[i])
        if best is None or d < best:
            begins, best = at, d
        if d == 0:
            break
    n = min(len(cut), len(src) - begins)
    differing = sum(1 for i in range(n) if src[begins + i] != cut[i])
    first = next((i for i in range(n) if src[begins + i] != cut[i]), -1)
    return begins // channels, len(cut) // channels, differing, first // channels


if __name__ == "__main__":
    what = sys.argv[1]
    if what == "types":
        for pid, st in stream_types(sys.argv[2]):
            print("0x%04x 0x%02x" % (pid, st))
    elif what == "registrations":
        for name in registrations(sys.argv[2]):
            print(name)
    elif what == "align":
        begins, length, differing, first = align(sys.argv[2], sys.argv[3], int(sys.argv[4]))
        print("begins=%d length=%d differing=%d first=%d" % (begins, length, differing, first))
    elif what == "peak":
        cut = decode(sys.argv[2], int(sys.argv[3]))
        lo, hi = int(sys.argv[4]), int(sys.argv[5])
        window = cut[lo * int(sys.argv[3]) : hi * int(sys.argv[3])]
        print(max((abs(v) for v in window), default=0))
    else:
        sys.exit("unknown: " + what)
