"""Walk an ADTS stream frame by frame and say what is in it.

Two questions, and neither can be asked of a decoder -- both are about the
bytes. Is every frame the same kind of AAC the recording carried, MPEG-2 for
a Japanese broadcast? And how many of the frames are the recording's own
bytes, unchanged? A smart-rendered cut has to answer "all of them" and
"all but the handful the boundaries land inside".

    python3 tests/aac_frames.py out.aac [source.aac] [--mpeg 2]
        [--max-reencoded N] [--profile 1] [--payload-only 1] [--hz 48000]

`--hz` requires every frame's header to name that sample rate, which is the
rate a transport stream's decoder actually reads -- an encoder resampled to
44.1 kHz whose frames still say 48 plays the whole track at the wrong speed.

`--payload-only` compares what is inside the frames rather than the frames
themselves, which is the only comparison an MP4 can answer: its muxer keeps
the payloads and throws the ADTS framing away, so the headers read back out
of one were written by the demuxer and say nothing about the cut.
"""
import sys

HEADER = 7

# What a header's four-bit sampling frequency index stands for. 13 is the
# highest one defined; the three above it are reserved and would be a header
# nothing wrote on purpose.
RATES = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050,
         16000, 12000, 11025, 8000, 7350, None, None, None]


def start_of(data):
    """Where the first whole frame begins.

    Usually byte zero. Not when the stream was cut out of a recording with a
    seek, which lands in the middle of a frame and leaves its tail at the
    front of the file. A sync word alone is not proof -- the payload of a
    frame can contain one -- so a candidate has to hold up for several frames
    running.
    """
    for i in range(min(len(data), 1 << 16)):
        if data[i] != 0xFF or data[i + 1] & 0xF0 != 0xF0:
            continue
        at, ok = i, True
        for _ in range(4):
            if at + HEADER > len(data):
                break
            if data[at] != 0xFF or data[at + 1] & 0xF0 != 0xF0:
                ok = False
                break
            at += ((data[at + 3] & 3) << 11) | (data[at + 4] << 3) | (data[at + 5] >> 5)
        if ok:
            return i
    return 0


def frames(path):
    """Every frame in the file, as (header fields, whole frame bytes)."""
    data = open(path, "rb").read()
    out, i = [], start_of(data)
    while i + HEADER <= len(data):
        if data[i] != 0xFF or data[i + 1] & 0xF0 != 0xF0:
            raise SystemExit(f"no sync word at byte {i} of {path}")
        length = ((data[i + 3] & 3) << 11) | (data[i + 4] << 3) | (data[i + 5] >> 5)
        if length < HEADER:
            raise SystemExit(f"frame at byte {i} of {path} claims to be {length} bytes")
        if i + length > len(data):
            # A stream cut out of a recording ends mid-frame as readily as it
            # begins mid-frame; the tail is not a frame, so it is not counted.
            break
        out.append((
            {
                "mpeg": 2 if data[i + 1] >> 3 & 1 else 4,
                "crc": 0 if data[i + 1] & 1 else 1,
                "profile": data[i + 2] >> 6,
                "rate": data[i + 2] >> 2 & 0xF,
                "channels": (data[i + 2] & 1) << 2 | data[i + 3] >> 6,
                "blocks": (data[i + 6] & 3) + 1,
            },
            data[i:i + length],
            # The payload alone, past a header that is two bytes longer when
            # the frame carries a CRC. What survives a trip through a
            # container that strips the framing and writes its own back.
            data[i + HEADER + (0 if data[i + 1] & 1 else 2):i + length],
        ))
        i += length
    return out


def main(argv):
    args, flags = [], {}
    i = 0
    while i < len(argv):
        if argv[i].startswith("--"):
            flags[argv[i][2:]] = argv[i + 1]
            i += 2
        else:
            args.append(argv[i])
            i += 1

    payload_only = "payload-only" in flags
    part = 2 if payload_only else 1
    got = frames(args[0])
    if not got:
        print("  no frames at all")
        return 1

    bad = []
    kinds = {}
    for head, _, _ in got:
        key = tuple(sorted(head.items()))
        kinds[key] = kinds.get(key, 0) + 1
    for key, n in sorted(kinds.items(), key=lambda kv: -kv[1]):
        head = dict(key)
        print("   MPEG-%(mpeg)d  profile %(profile)d  %(hz)s Hz  "
              "%(channels)dch  %(blocks)d block  crc %(crc)d"
              % dict(head, hz=RATES[head["rate"]]), "x%d" % n)
        if "mpeg" in flags and head["mpeg"] != int(flags["mpeg"]):
            bad.append("%d frame(s) are MPEG-%d, wanted MPEG-%s"
                       % (n, head["mpeg"], flags["mpeg"]))
        if "profile" in flags and head["profile"] != int(flags["profile"]):
            bad.append("%d frame(s) are profile %d, wanted %s"
                       % (n, head["profile"], flags["profile"]))
        if "hz" in flags and RATES[head["rate"]] != int(flags["hz"]):
            bad.append("%d frame(s) say %s Hz, wanted %s"
                       % (n, RATES[head["rate"]], flags["hz"]))
        if head["blocks"] != 1:
            bad.append("%d frame(s) carry %d raw data blocks" % (n, head["blocks"]))

    if len(args) > 1:
        source = set(f[part] for f in frames(args[1]))
        same = sum(1 for f in got if f[part] in source)
        new = len(got) - same
        print("   %d/%d frames verbatim from the recording (%.3f%%), %d re-encoded"
              % (same, len(got), 100.0 * same / len(got), new))
        if "max-reencoded" in flags and new > int(flags["max-reencoded"]):
            bad.append("%d frames re-encoded, at most %s expected"
                       % (new, flags["max-reencoded"]))
        if "min-reencoded" in flags and new < int(flags["min-reencoded"]):
            bad.append("%d frames re-encoded, at least %s expected"
                       % (new, flags["min-reencoded"]))

    for line in bad:
        print("   BAD:", line)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
