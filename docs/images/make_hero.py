#!/usr/bin/env python3
"""Draw the README hero: the recording on top, the export under it.

Two commercial blocks fall out, the parts you keep close up, and the export
comes out as one file that is a byte-for-byte copy of the input everywhere
except at the seams.

Writes hero.svg, hero-dark.svg, hero.ja.svg and hero-dark.ja.svg beside this
file. Run it from anywhere:

    python3 docs/images/make_hero.py
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from diagram import JA as JA_TYPE, EN as EN_TYPE, ticks, write  # noqa: E402

W, H = 1000, 356
BAR_X, BAR_W, BAR_H = 40.0, 920.0, 48.0
IN_Y, OUT_Y = 64.0, 244.0
KEY = 46.0          # a key frame every this many pixels
FRAME = KEY / 4.0   # ...and the frames between them

# What the recording is made of, as a fraction of its running time.
SEGMENTS = [("prog", 0.00, 0.24), ("cm", 0.24, 0.35), ("prog", 0.35, 0.68),
            ("cm", 0.68, 0.80), ("prog", 0.80, 1.00)]

EN = dict(
    EN_TYPE,
    recording="THE RECORDING", export="THE EXPORT", cm="CM",
    copied="copied byte for byte",
    gone="23% removed",
    seam="rebuilt at the seams — a few frames, often none",
    caption="More than 99% of the output is an exact copy of the input.",
)
JA = dict(
    JA_TYPE,
    recording="録画", export="書き出し", cm="CM",
    copied="バイト単位でそのままコピー",
    gone="CM 23% を削除",
    seam="作り直すのは継ぎ目の数コマだけ。"
         "0 コマのことも多い",
    caption="出力の 99% 以上が入力と同一のコピー。",
)


def draw(pal, txt):
    kept_w = sum(b - a for kind, a, b in SEGMENTS if kind == "prog") * BAR_W
    out_x = BAR_X

    # Where each kept stretch of the recording lands once the gaps close up.
    kept, cursor = [], out_x
    for kind, a, b in SEGMENTS:
        if kind != "prog":
            continue
        x0, x1 = BAR_X + a * BAR_W, BAR_X + b * BAR_W
        kept.append((x0, x1, cursor, cursor + (x1 - x0)))
        cursor += x1 - x0
    seams = [u1 for _, _, _, u1 in kept[:-1]]

    s = []
    s.append(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" '
             f'width="{W}" height="{H}" font-family="{txt["font"]}">')
    s.append('<defs>')
    s.append(f'<clipPath id="cin"><rect x="{BAR_X}" y="{IN_Y}" width="{BAR_W}" '
             f'height="{BAR_H}" rx="4"/></clipPath>')
    s.append(f'<clipPath id="cout"><rect x="{out_x:.1f}" y="{OUT_Y}" '
             f'width="{kept_w:.1f}" height="{BAR_H}" rx="4"/></clipPath>')
    band = f'x1="0" y1="{IN_Y + BAR_H:.0f}" x2="0" y2="{OUT_Y:.0f}" ' \
           f'gradientUnits="userSpaceOnUse"'
    s.append(f'<linearGradient id="keep" {band}>'
             f'<stop offset="0" stop-color="{pal["copy"]}" stop-opacity="0.05"/>'
             f'<stop offset="1" stop-color="{pal["copy"]}" stop-opacity="0.30"/>'
             f'</linearGradient>')
    s.append(f'<linearGradient id="drop" {band}>'
             f'<stop offset="0" stop-color="{pal["cm"]}" stop-opacity="0.40"/>'
             f'<stop offset="1" stop-color="{pal["cm"]}" stop-opacity="0.03"/>'
             f'</linearGradient>')
    s.append('</defs>')

    # --- the recording -------------------------------------------------
    s.append(f'<text x="{BAR_X}" y="{IN_Y - 14:.0f}" font-size="{txt["labelsize"]}" '
             f'font-weight="600" letter-spacing="{txt["track"]}" '
             f'fill="{pal["dim"]}">{txt["recording"]}</text>')
    s.append('<g clip-path="url(#cin)">')
    for kind, a, b in SEGMENTS:
        x0, x1 = BAR_X + a * BAR_W, BAR_X + b * BAR_W
        if kind == "prog":
            s.append(f'<rect x="{x0:.1f}" y="{IN_Y}" width="{x1 - x0:.1f}" '
                     f'height="{BAR_H}" fill="{pal["prog"]}"/>')
            s += ticks(x0, x1, IN_Y, BAR_H, pal["frame"], "1", FRAME, "0.55")
            s += ticks(x0, x1, IN_Y, BAR_H, pal["key"], "1.6", KEY, "0.75")
        else:
            s.append(f'<rect x="{x0:.1f}" y="{IN_Y}" width="{x1 - x0:.1f}" '
                     f'height="{BAR_H}" fill="{pal["cm"]}"/>')
            s.append(f'<text x="{(x0 + x1) / 2:.1f}" y="{IN_Y + BAR_H / 2 + 5:.1f}" '
                     f'text-anchor="middle" font-size="14" font-weight="700" '
                     f'letter-spacing="1" fill="{pal["cmtext"]}">{txt["cm"]}</text>')
    s.append('</g>')
    s.append(f'<rect x="{BAR_X}" y="{IN_Y}" width="{BAR_W}" height="{BAR_H}" '
             f'rx="4" fill="none" stroke="{pal["progline"]}"/>')

    # --- the commercial blocks collapsing to nothing ------------------
    gone = []
    cursor = out_x
    for kind, a, b in SEGMENTS:
        x0, x1 = BAR_X + a * BAR_W, BAR_X + b * BAR_W
        if kind == "prog":
            cursor += x1 - x0
        else:
            gone.append((x0, x1, cursor))
    for x0, x1, u in gone:
        s.append(f'<path d="M{x0:.1f} {IN_Y + BAR_H:.0f} L{x1:.1f} {IN_Y + BAR_H:.0f} '
                 f'L{u:.1f} {OUT_Y:.0f} Z" fill="url(#drop)"/>')

    # --- what is kept, closing up --------------------------------------
    for x0, x1, u0, u1 in kept:
        s.append(f'<path d="M{x0:.1f} {IN_Y + BAR_H:.0f} L{x1:.1f} {IN_Y + BAR_H:.0f} '
                 f'L{u1:.1f} {OUT_Y:.0f} L{u0:.1f} {OUT_Y:.0f} Z" '
                 f'fill="url(#keep)" stroke="{pal["copy"]}" '
                 f'stroke-opacity="0.22" stroke-width="1"/>')

    # --- the export ----------------------------------------------------
    s.append(f'<text x="{out_x:.1f}" y="{OUT_Y - 14:.0f}" font-size="{txt["labelsize"]}" '
             f'font-weight="600" letter-spacing="{txt["track"]}" '
             f'fill="{pal["dim"]}">{txt["export"]}</text>')
    s.append('<g clip-path="url(#cout)">')
    s.append(f'<rect x="{out_x:.1f}" y="{OUT_Y}" width="{kept_w:.1f}" '
             f'height="{BAR_H}" fill="{pal["copy"]}"/>')
    s += ticks(out_x, out_x + kept_w, OUT_Y, BAR_H, "#ffffff", "1.6", KEY, "0.13")
    for x in seams:
        s.append(f'<rect x="{x - 2:.1f}" y="{OUT_Y}" width="4" height="{BAR_H}" '
                 f'fill="{pal["encode"]}"/>')
    s.append('</g>')
    mid = kept[1]
    s.append(f'<text x="{(mid[2] + mid[3]) / 2:.1f}" y="{OUT_Y + BAR_H / 2 + 5:.1f}" '
             f'text-anchor="middle" font-size="15.5" font-weight="600" '
             f'fill="{pal["copytext"]}">{txt["copied"]}</text>')

    s.append(f'<rect x="{out_x + kept_w + 5:.1f}" y="{OUT_Y}" '
             f'width="{BAR_W - kept_w - 5:.1f}" height="{BAR_H}" rx="4" fill="none" '
             f'stroke="{pal["cm"]}" stroke-width="1.2" stroke-dasharray="4 4" '
             f'opacity="0.75"/>')
    s.append(f'<text x="{out_x + kept_w + (BAR_W - kept_w) / 2:.1f}" '
             f'y="{OUT_Y + BAR_H / 2 + 4:.1f}" text-anchor="middle" font-size="12" '
             f'fill="{pal["cmnote"]}">{txt["gone"]}</text>')

    # --- the seam note -------------------------------------------------
    leader, bottom = seams[0], OUT_Y + BAR_H + 20
    s.append(f'<path d="M{leader:.1f} {OUT_Y + BAR_H:.0f} V{bottom:.0f} '
             f'H{leader + 13:.1f}" fill="none" stroke="{pal["encode"]}" '
             f'stroke-width="1.5"/>')
    s.append(f'<text x="{leader + 19:.1f}" y="{bottom + 4:.0f}" font-size="13" '
             f'fill="{pal["encode"]}">{txt["seam"]}</text>')

    s.append(f'<text x="{W / 2:.0f}" y="{H - 16:.0f}" text-anchor="middle" '
             f'font-size="17" font-weight="600" fill="{pal["text"]}">'
             f'{txt["caption"]}</text>')
    s.append('</svg>')
    return "\n".join(s) + "\n"


if __name__ == "__main__":
    write("hero", draw, EN, JA)
