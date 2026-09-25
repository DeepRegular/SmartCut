#!/usr/bin/env python3
"""The field sequence of an MPEG-2 elementary stream, as a set-top decoder
would show it.

Prints one line: how many frame pictures there are, how many fields they
stand for, how many of them repeat a field, how many times the parity of
consecutive fields fails to alternate, and which `progressive_sequence`
values the sequence extensions carry. Pictures are taken GOP by GOP in
display order (by temporal reference).

    mpeg2_fields.py <file.m2v>
"""
import sys

data = open(sys.argv[1], "rb").read()
gops, pics, cur, seqs = [], [], None, set()
i = 0
while True:
    j = data.find(b"\x00\x00\x01", i)
    if j < 0 or j + 9 > len(data):
        break
    code = data[j + 3]
    if code == 0xB8:
        if pics:
            gops.append(pics)
        pics = []
    elif code == 0x00:
        cur = (data[j + 4] << 2) | (data[j + 5] >> 6)
    elif code == 0xB5:
        ext = data[j + 4] >> 4
        if ext == 1:
            seqs.add((data[j + 5] >> 3) & 1)
        elif ext == 8 and cur is not None:
            e = data[j + 4 : j + 9]
            pics.append((cur, (e[3] >> 7) & 1, (e[3] >> 1) & 1, e[2] & 3))
            cur = None
    i = j + 3
if pics:
    gops.append(pics)

last = None
pictures = fields = repeats = breaks = 0
for g in gops:
    # By temporal reference alone: the two fields of a pair share one, and
    # the order they were coded in is the order they are shown.
    for _, tff, rff, structure in sorted(g, key=lambda p: p[0]):
        if structure != 3:  # a field picture: one field, top (1) or bottom (2)
            first = 1 if structure == 1 else 0
            count = 1
        else:
            first = tff
            count = 3 if rff else 2
        if last is not None and first == last:
            breaks += 1
        last = first if count % 2 else 1 - first
        pictures += structure == 3
        fields += count
        repeats += rff
print(f"pictures={pictures} fields={fields} repeats={repeats} breaks={breaks} "
      f"progressive_sequence={','.join(map(str, sorted(seqs)))}")
