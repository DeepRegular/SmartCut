"""Shared parts of the diagrams in this folder.

The colours are the editor's own, from gui/src/styles.css: --copy for what is
copied, --encode for what has to be rebuilt, --cm for a commercial block. Each
diagram is drawn four times — light and dark, English and Japanese — and the
README picks between them with <picture> and prefers-color-scheme.
"""

import os

LIGHT = dict(
    text="#1f2328", dim="#636c76",
    prog="#eaeef2", progline="#d0d7de", frame="#ccd3da", key="#a8b1bb",
    cm="#a95c2e", cmtext="#fff3ec", cmnote="#9c5528",
    copy="#0d8ba3", copytext="#ffffff",
    encode="#d98410", enctext="#3a2400",
)
DARK = dict(
    text="#e6edf3", dim="#9198a1",
    prog="#21262d", progline="#30363d", frame="#2c333c", key="#4b5563",
    cm="#b06030", cmtext="#ffeade", cmnote="#d08c5e",
    copy="#14b8d4", copytext="#062831",
    encode="#f0a020", enctext="#2a1a00",
)

SANS = ("-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Noto Sans', "
        "Helvetica, Arial, sans-serif")
JSANS = ("-apple-system, BlinkMacSystemFont, 'Hiragino Sans', "
         "'Hiragino Kaku Gothic ProN', 'Yu Gothic', YuGothic, 'Noto Sans JP', "
         "'Noto Sans CJK JP', Meiryo, sans-serif")
MONO = "ui-monospace, SFMono-Regular, Menlo, Consolas, 'Liberation Mono', monospace"

# Type that has to hold in both languages: Japanese sits a little larger and
# wants none of the tracking.
EN = dict(font=SANS, track="1.4", labelsize="12.5")
JA = dict(font=JSANS, track="0.6", labelsize="13.5")


def ticks(x0, x1, y, h, colour, width, step, opacity):
    """A column of hairlines across one stretch of a bar."""
    out, x = [], x0 + step
    while x < x1 - 0.5:
        out.append(f'<line x1="{x:.1f}" y1="{y:.1f}" x2="{x:.1f}" '
                   f'y2="{y + h:.1f}" stroke="{colour}" stroke-width="{width}" '
                   f'opacity="{opacity}"/>')
        x += step
    return out


def write(stem, draw, en, ja):
    """Write <stem>{,-dark}{,.ja}.svg beside this file."""
    here = os.path.dirname(os.path.abspath(__file__))
    for suffix, txt in ((".svg", en), (".ja.svg", ja)):
        for name, pal in ((stem, LIGHT), (stem + "-dark", DARK)):
            path = os.path.join(here, name + suffix)
            with open(path, "w", encoding="utf-8") as fh:
                fh.write(draw(pal, txt))
            print(path)
