# Extending to BDMV / BDAV (research notes)

[← Documentation](README.md) ・ [← smartcut](../README.md) ・ [日本語](bdmv.ja.md)

Conclusion: **it is feasible, and the first stage already works today.** What was
verified:

| Item | State |
|---|---|
| **Reading M2TS (192-byte packets)** | **Works unmodified.** Real material re-muxed to M2TS gave 450/450, 96.2 % lossless, A/V 17.7 ms |
| The `bluray:` protocol | This ffmpeg is built `--enable-libbluray`, and it is reachable from our own binary (with `-playlist`, `-angle` and `-chapter` options) |
| AC-3 audio (the BDMV staple) | Goes through as far as MP4 output |
| **BD LPCM (`pcm_bluray`)** | **Not possible in MP4 or MKV**; in TS it degrades to `bin_data`. Conversion to standard PCM is required |
| BDAV playlists (`.rpls`) | libbluray is BDMV-only. A parser of our own is needed |

## The work, stage by stage

1. **Cutting a single `.m2ts`** — done. No further work.
2. **Cutting BDMV by playlist** — switch to `ff::format::input_with()` and pass
   the `playlist` option. Small.
3. **BDAV playlist support** — read `.rpls` ourselves. The format is
   straightforward, a list of PlayItems (clip reference plus IN/OUT times), but it
   still has to be written.
4. **BD-specific streams** — LPCM conversion, PGS subtitles, TrueHD/DTS-HD, VC-1.
   Right now only "one video track plus one audio track" is handled, so BD's
   multiple audio tracks and subtitles presuppose generalising track selection.
5. **Writing BDMV/BDAV out** — that is authoring, a different problem. Large.

## The index source is swappable (implemented)

How the access point index is built is factored out into the `IndexSource` trait
in [`index.rs`](../rust/crates/core/src/index.rs). An implementation that reads
BD's CLPI EP map only has to be added there.

| Implementation | What it does |
|---|---|
| `PacketScan` | Scans every packet. Exact, and works on any container |
| `ContainerIndex` | Reads the container's seek table (MP4's `stss`, and so on) |
| (future) `ClpiIndex` | Reads BD's CLIPINF EP map |

`--index scan|container` switches between them. Measured on a real 654 MB MP4:

```
container    2.73s   430 access points
scan        56.44s   430 access points   <- the plans are byte-identical
```

**Why "times alone" were made sufficient.** Copy regions used to be delimited by
"packet count in decode order", which only a full packet scan can give you. What
an external index has is times, so the scheme was changed to **stop on reaching
the terminating access point's time**. Leading-picture removal likewise became a
time-based rule: "drop packets that display before the entry point". The result
is a cutter that does not care where its index came from.

**What an external index cannot answer.** An index only knows where the entry
points are. Whether a GOP is open, and whether its leading pictures are
referenced, cannot be known without looking at the bitstream. So the index
declares whether it could answer, and `refine_leading()` fills in the gaps by
reading **only around the access points the cut actually uses**. The file is
never read through again.

Two traps along the way:

- **Container seek tables are keyed by DTS.** With B pyramids the difference from
  PTS is not constant, so a simple correction does not line up. It became: match
  by DTS within a window, then read the PTS back.
- Once matching is done by DTS, **the target keyframe's own PTS already satisfies
  the "next GOP" stop condition**, so the window closes immediately and not a
  single leading picture is seen. The stop condition has to be evaluated on the
  next access point, not on the target itself.

Verification: 9 of the 13 cases (everything but TS) give results identical to the
scan under `--index container`. MPEG-TS has no seek table, so it returns an
explicit error — **precisely the hole that BD's CLPI fills**.

Note that **encrypted discs are out of scope**. Nothing that presupposes
decryption is handled here.
