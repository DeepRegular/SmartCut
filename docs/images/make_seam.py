#!/usr/bin/env python3
"""Draw what happens at one seam: head, body and tail.

For a kept interval [t_in, t_out), the pictures between t_in and the first
access point after it are rebuilt, and so are the ones between the last access
point the copy reaches and t_out. Everything between the two is the input's own
bytes. This is the diagram of that, the same thing README.md used to show as
ASCII.

Writes seam.svg, seam-dark.svg, seam.ja.svg and seam-dark.ja.svg beside this
file. Run it from anywhere:

    python3 docs/images/make_seam.py
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from diagram import JA as JA_TYPE, EN as EN_TYPE, MONO, write  # noqa: E402

W, H = 1000, 254
X0, PITCH, CELL, N = 40.0, 20.0, 17.0, 46
STRIP_Y, STRIP_H = 58.0, 54.0
GOP = 8                 # an access point every this many pictures
IN_F, OUT_F = 4, 38     # the range runs from picture IN_F up to, but not into, OUT_F
K_FIRST, K_TERM = 8, 32  # where the copy can start, and where its coverage ends

EN = dict(
    EN_TYPE,
    kept="ONE RANGE YOU KEEP", legend="I = key frame",
    reencode="re-encoded", copy="copied byte for byte",
    caption="Cut exactly on a key frame and the head and the tail disappear: "
            "nothing is rebuilt at all.",
)
JA = dict(
    JA_TYPE,
    kept="残す区間 1 つ", legend="I = キーフレーム",
    reencode="再エンコード", copy="バイト単位でコピー",
    caption="キーフレームちょうどで切れば head も tail も消え、"
            "作り直しは 1 フレームも起きません。",
)


def x_of(i):
    return X0 + i * PITCH


def cut_at(i):
    """The seam in front of picture i, down the middle of the gap."""
    return x_of(i) - (PITCH - CELL) / 2


def draw(pal, txt):
    t_in, t_out = cut_at(IN_F), cut_at(OUT_F)
    bottom = STRIP_Y + STRIP_H
    s = []
    s.append(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" '
             f'width="{W}" height="{H}" font-family="{txt["font"]}">')

    s.append(f'<text x="{X0:.0f}" y="30" font-size="{txt["labelsize"]}" '
             f'font-weight="600" letter-spacing="{txt["track"]}" '
             f'fill="{pal["dim"]}">{txt["kept"]}</text>')
    s.append(f'<text x="{x_of(N - 1) + CELL:.1f}" y="30" text-anchor="end" '
             f'font-size="12.5" fill="{pal["dim"]}">{txt["legend"]}</text>')

    # --- the pictures, coloured by what becomes of each ------------------
    for i in range(N):
        if i < IN_F or i >= OUT_F:
            fill, ink, op = pal["prog"], pal["dim"], "0.55"
        elif i < K_FIRST or i >= K_TERM:
            fill, ink, op = pal["encode"], pal["enctext"], "1"
        else:
            fill, ink, op = pal["copy"], pal["copytext"], "1"
        x = x_of(i)
        s.append(f'<rect x="{x:.1f}" y="{STRIP_Y:.0f}" width="{CELL:.0f}" '
                 f'height="{STRIP_H:.0f}" rx="2" fill="{fill}" opacity="{op}"/>')
        if i % GOP == 0:
            s.append(f'<text x="{x + CELL / 2:.1f}" y="{STRIP_Y + STRIP_H / 2 + 5:.0f}" '
                     f'text-anchor="middle" font-size="14" font-weight="700" '
                     f'fill="{ink}" opacity="{op}">I</text>')
        else:
            s.append(f'<rect x="{x + CELL / 2 - 2.5:.1f}" '
                     f'y="{STRIP_Y + STRIP_H / 2 - 1:.0f}" width="5" height="2" '
                     f'fill="{ink}" opacity="0.4"/>')

    # --- where you asked for the cut -------------------------------------
    for x in (t_in, t_out):
        s.append(f'<path d="M{x - 5:.1f} 40 L{x + 5:.1f} 40 L{x:.1f} 48 Z" '
                 f'fill="{pal["text"]}"/>')
        s.append(f'<line x1="{x:.1f}" y1="44" x2="{x:.1f}" y2="124" '
                 f'stroke="{pal["text"]}" stroke-width="1.6"/>')
    for i in (K_FIRST, K_TERM):
        x = x_of(i) + CELL / 2
        s.append(f'<line x1="{x:.1f}" y1="{bottom:.0f}" x2="{x:.1f}" y2="124" '
                 f'stroke="{pal["dim"]}" stroke-width="1"/>')

    for x, name in ((t_in, "t_in"), (x_of(K_FIRST) + CELL / 2, "k_first"),
                    (x_of(K_TERM) + CELL / 2, "k_term"), (t_out, "t_out")):
        s.append(f'<text x="{x:.1f}" y="138" text-anchor="middle" font-size="12.5" '
                 f'font-family="{MONO}" fill="{pal["dim"]}">{name}</text>')

    # --- what each stretch costs -----------------------------------------
    spans = [(t_in, x_of(K_FIRST), "head", pal["encode"], txt["reencode"]),
             (x_of(K_FIRST), x_of(K_TERM), "body", pal["copy"], txt["copy"]),
             (x_of(K_TERM), t_out, "tail", pal["encode"], txt["reencode"])]
    for a, b, name, colour, what in spans:
        s.append(f'<path d="M{a:.1f} 156 V162 H{b:.1f} V156" fill="none" '
                 f'stroke="{colour}" stroke-width="1.3" stroke-linecap="square"/>')
        s.append(f'<text x="{(a + b) / 2:.1f}" y="184" text-anchor="middle" '
                 f'font-size="14.5" font-weight="700" font-family="{MONO}" '
                 f'fill="{pal["text"]}">{name}</text>')
        s.append(f'<text x="{(a + b) / 2:.1f}" y="203" text-anchor="middle" '
                 f'font-size="12.5" fill="{colour}">{what}</text>')

    s.append(f'<text x="{W / 2:.0f}" y="{H - 18:.0f}" text-anchor="middle" '
             f'font-size="16" font-weight="600" fill="{pal["text"]}">'
             f'{txt["caption"]}</text>')
    s.append('</svg>')
    return "\n".join(s) + "\n"


if __name__ == "__main__":
    write("seam", draw, EN, JA)
