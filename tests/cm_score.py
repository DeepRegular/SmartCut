#!/usr/bin/env python3
"""Score detected commercial blocks against what a recording actually contains.

Block counts and per-boundary tolerances answer "did it find them", which is
not the question the tool is judged on. Acting on a block removes it, so the
two errors cost different amounts and are budgeted separately:

  本編を誤削除  programme swallowed by a block -- gone once the cut is made
  CM 残り       commercial left outside one -- a nuisance, and visible

One accuracy number would let a regression in the expensive direction hide
behind an improvement in the cheap one.

Not every second is one or the other. A broadcast puts its own material
around the breaks -- block idents, 番宣, the sponsor credit -- and whether
those should go is the viewer's taste, not a fact about the recording.
Forcing a verdict on them would make the metric lie in whichever direction
it was forced, so they are marked grey (`~START-END`) and cost nothing
either way.

  usage: cm_score.py MAX_OVERCUT MAX_UNDERCUT DURATION [~]START-END ...
  stdin: one "HH:MM:SS.mmm HH:MM:SS.mmm" per detected block

Prints a summary on the first line and one reason per line after it; no
reasons means the case passed.
"""
import sys

# How close to either end of the recording still counts as its edge. Mirrors
# cm.rs, which uses the same figure for the same reason.
EDGE = 1.5


def secs(s):
    h, m, rest = s.split(":")
    return int(h) * 3600 + int(m) * 60 + float(rest)


def uncovered(spans, others):
    """Seconds of `spans` not covered by `others`."""
    total = 0.0
    for a, b in spans:
        cur = a
        for x, y in sorted(others):
            if y <= cur or x >= b:
                continue
            if x > cur:
                total += x - cur
            cur = max(cur, y)
            if cur >= b:
                break
        total += max(0.0, b - cur)
    return total


def overlaps(a, b, spans):
    return any(max(a, x) < min(b, y) for x, y in spans)


def hms(t):
    return f"{int(t)//3600}:{int(t)//60%60:02d}:{t%60:06.3f}"


def main():
    max_over, max_under, duration = (float(a) for a in sys.argv[1:4])
    cm, grey = [], []
    for span in sys.argv[4:]:
        target, text = (grey, span[1:]) if span.startswith("~") else (cm, span)
        a, b = text.split("-")
        target.append((float(a), float(b)))

    got = []
    for line in sys.stdin:
        if not line.strip():
            continue
        a, b = line.split()
        got.append((secs(a), secs(b)))

    over = uncovered(got, cm + grey)  # detected, and neither commercial nor grey
    under = uncovered(cm, got)        # commercial, and not detected

    print(f"{len(got)} ブロック / 本編を誤削除 {over:.1f}s / CM 残り {under:.1f}s")

    why = []
    # Named separately from the budgets: a block sitting entirely in programme
    # is the failure this tool exists to avoid, and saying "3.2s over budget"
    # would not convey that.
    for i, (a, b) in enumerate(got, 1):
        if not overlaps(a, b, cm + grey):
            why.append(f"第{i}ブロックは本編のみ: {hms(a)} → {hms(b)}")
    for a, b in cm:
        if not overlaps(a, b, got):
            why.append(f"CM を丸ごと取りこぼした: {hms(a)} → {hms(b)}")
    if over > max_over:
        why.append(f"本編を誤削除 {over:.1f}s > {max_over:.1f}s")
    if under > max_under:
        why.append(f"CM 残り {under:.1f}s > {max_under:.1f}s")

    # Commercials are sold in fifteen-second units, so a block that really is
    # one is a whole number of them long. Nothing in the detector enforces
    # this -- it comes out that way only if the boundaries landed on the real
    # cuts, which makes it the sharpest check available on that, and an
    # independent one. Keep it that way: do not feed the 15-second grid back
    # into block placement without finding another check to replace it.
    #
    # A block touching either end of the recording is exempt: the recorder
    # started or stopped partway through it, so its length is whatever was
    # caught, not what was sold.
    for i, (a, b) in enumerate(got, 1):
        span = b - a
        if span < 30 or a <= EDGE or b >= duration - EDGE:
            continue
        off = abs(span - round(span / 15) * 15)
        if off > 0.25:
            why.append(f"第{i}ブロック {span:.1f}s が 15 秒の倍数でない（{off:.2f}s ずれ）")

    for w in why:
        print(w)


main()
