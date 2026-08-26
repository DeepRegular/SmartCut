"""Dump the MPEG-2 sequence headers an elementary stream carries.

An indexer reads the first sequence header it meets and believes it for the
whole file: frame rate, aspect, and whether the sequence is progressive. A cut
that re-encodes its opening frames writes that header itself, so it is the one
place the recording's own numbers can quietly be replaced.
"""
import sys

RATE = ["forbidden", "24000/1001", "24", "25", "30000/1001", "30", "50",
        "60000/1001", "60", "?", "?", "?", "?", "?", "?", "?"]
ASPECT = ["forbidden", "1:1", "4:3", "16:9", "2.21:1"] + ["?"] * 11

data = open(sys.argv[1], "rb").read()
want = 1

seen = 0
i = 0
while i + 12 < len(data) and seen < want:
    if data[i:i + 4] != b"\x00\x00\x01\xb3":
        i += 1
        continue
    h = data[i + 4:i + 12]
    width = (h[0] << 4) | (h[1] >> 4)
    height = ((h[1] & 0xF) << 8) | h[2]
    aspect = h[3] >> 4
    rate = h[3] & 0xF
    bitrate = (h[4] << 10) | (h[5] << 2) | (h[6] >> 6)
    # a sequence extension, if present, follows and refines all of this
    ext = ""
    j = i + 12
    for _ in range(4096):
        if j + 6 >= len(data):
            break
        if data[j:j + 4] == b"\x00\x00\x01\xb5" and (data[j + 4] >> 4) == 1:
            e = data[j + 4:j + 10]
            prog = (e[1] >> 3) & 1
            n = (e[5] >> 5) & 3
            d = e[5] & 0x1F
            ext = f"  拡張: progressive={prog} rate_ext={n}/{d}"
            break
        if data[j:j + 3] == b"\x00\x00\x01" and data[j + 3] == 0x00:
            break
        j += 1
    if seen == 0:
        print(f"width={width}")
        print(f"height={height}")
        print(f"frame_rate_code={rate}")
        print(f"frame_rate={RATE[rate]}")
        print(f"aspect={aspect}")
    seen += 1
    i += 12
if seen == 0:
    print("frame_rate_code=-1")
