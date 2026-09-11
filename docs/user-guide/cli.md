# Using the command line

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](cli.ja.md)

**The same engine that runs inside the GUI** is also available as a command
called `smartcut`. (If you installed the `.deb`, it is called `smartcut-cli`
there — `smartcut` is the GUI.)

It is for repeating the same job, for calling from a script, and for machines
with no screen to put a window on. It can do everything the GUI can.

## Start with these six

```bash
smartcut input.ts --keep 5.3-12.7 -o out.ts   # keep only this range
smartcut input.ts --cut 8.0-20.0  -o out.ts   # drop this range
smartcut input.ts --analyze                   # just show what would happen

smartcut input.ts --analyze --detect-cm --logo  # list the commercial candidates
smartcut input.ts --analyze --scenes            # list the scene changes

smartcut input.ts --cut 8.0-20.0 --bdav ~/disc  # onto a disc instead of a file
```

- `--keep` and `--cut` can be given **as many times as you like**.
  `--keep 0-60 --keep 180-240` keeps those two ranges.
- Times can be plain seconds or the `1:23:45.6` form.
- **`--analyze` writes nothing.** It prints the plan — what gets copied and what
  gets rebuilt. Running it first is the safe way to check before committing.

## Choosing where to cut

| Option | Meaning |
|---|---|
| `--keep START-END` | A range to keep. Repeatable |
| `--cut START-END` | A range to drop. Repeatable |
| `--no-open-gop` | Never start a copy at an open GOP |
| `--clean-joins` | Re-encode up to two extra seconds at the start of each range, far enough to reach a point where no picture comes out of the decoder in the wrong order. **Off by default** — see the note below |

> **What `--clean-joins` costs.** At an ordinary join, the pictures a copy makes
> are already exact; the only thing wrong is the order one of them comes out in.
> `--clean-joins` avoids that by rebuilding two seconds, and those two seconds
> stop being a copy of the recording (about 51 dB against the original). That
> trades something certain for something occasional, which is why it is off.

## Choosing where the output goes

| Option | Meaning |
|---|---|
| `-o OUTPUT` | Output path. **The extension picks the container** |
| `--drop-stream INDEX` | Leave one of the recording's streams out of the output. Repeatable. The same thing the cut editor's **Tracks** menu does |
| `--title N` | Which recording on a disc (a folder or an `.iso`) to open. Part of the programme's name works in place of the number. **Left out, it lists what is on the disc and stops** |

## Audio

By default the sound is treated like the video: only **the few frames a join
falls inside** are rebuilt (smart rendering). Give any of the five settings
below a value the recording does not already have and there is no frame left to
copy, so **the whole track is re-encoded**.

| Option | Meaning |
|---|---|
| `--audio-mode smart\|copy\|reencode` | `smart` is the default: it rebuilds only the frames a boundary falls inside, so nothing from the far side of a cut is heard — and nothing at all when the join falls in silence. `copy` is lossless to the byte but leaves a little of the far side audible; `reencode` cuts to the sample at the cost of rebuilding the whole track |
| `--audio-codec source\|aac\|lpcm\|ac3\|dts` | What the audio is written as. `source`, the default, keeps the recording's own. AAC plays straight off a phone, AC-3 and DTS are what an AV receiver handles most reliably, and linear PCM is uncompressed |
| `--audio-channels N` | Channel count, 1 to 8. Anything but the recording's own is a downmix — 5.1 folded to stereo, for instance |
| `--audio-samplerate RATE` | Sample rate, as `48k` or `48000`. Codecs differ in what they support (AC-3 has three rates, Blu-ray LPCM three others); a rate a codec does not have is taken to the nearest one it does, with a note saying which |
| `--audio-bits 16\|24` | How wide each sample is. **Only meaningful for linear PCM** — the other codecs write a description of the sound rather than the sound itself, so there is nowhere to put a width |
| `--audio-bitrate RATE` | Bits per second when re-encoding, as `192k` or `192000`. Left out, it follows the recording. A figure the encoder will not accept is raised to what that codec is ordinarily carried at, with a note saying so |
| `--aac auto\|mpeg2\|mpeg4` | Which flavour of AAC the frames SmartCut writes announce themselves as. `auto`, the default, follows the recording — MPEG-2 AAC for a broadcast |
| `--audio-es` | Also write the sound out as a bare stream beside the output. AAC only |

## Writing a broadcast `.ts`

| Option | Meaning |
|---|---|
| `--tables partial\|broadcast\|muxer` | How the `.ts` describes its own contents. `partial`, the default, writes a partial transport stream (the standard shape for a recording); `broadcast` puts the recording's own programme tables back; `muxer` adds nothing. `--no-tables` is the old name for `muxer` |

## Writing a disc (BDAV)

`--bdav` writes **the same shape a recorder writes to a BD-RE** — a folder, not a
file. The disc decides the filenames, so `-o` is not used. If a disc is already
there, the recording is **added** to it.

| Option | Meaning |
|---|---|
| `--bdav FOLDER` | Write a BDAV disc into this folder (one recording becomes `BDAV/STREAM/00001.m2ts`). The index is built afterwards |
| `--iso 2.50\|2.60` | Wrap the finished disc in an `.iso` beside it: `--bdav ~/disc` writes `~/disc/BDAV` and `~/disc.iso`. The folder stays |
| `--disc-title NAME` | What the disc is called. Left out, the series the recording is an episode of: the programme name with the episode number, the episode's own title and the broadcast's marks taken off it |
| `--programme NAME` | What this recording is called in the disc's index. Left out, the name its playlist gave it if it came off a disc, otherwise the programme name the broadcast carries |
| `--channel NAME[,N]` | The channel, and optionally the three digits a viewer knows it by (`--channel "衛星第一,161"`). Left out, both come from the recording |
| `--about TEXT` | What the disc's index says the programme was. Left out, the description the recording carries |
| `--made "Y-M-D H:M:S"` | When the recording was made. Left out, when the programme went out, where the recording still says |

## Looking before you write

| Option | Meaning |
|---|---|
| `--analyze` | Work out the plan and print it. **Writes nothing** — not even into a `--bdav` folder |
| `--detect-cm` | Look for the commercial breaks |
| `--logo` | Let commercial detection use the station logo as well |
| `--scenes` | List the scene changes |
| `--preview TIME` | Decode one picture at `TIME` and write it as a JPEG (`preview.jpg`, or `-o`). It prints the time actually decoded beside the time asked for |
| `--cut-near TIME` | Print where the nearest picture-to-picture change is to `TIME`, in windows of ±0.5, ±1 and ±2 seconds |

## Index and proxy

| Option | Meaning |
|---|---|
| `--seek-index PATH` | Where to keep the seek index. Written on the first run and read on the next, which saves walking the recording again |
| `--index auto\|disc\|scan\|container` | Where the entry points come from. `auto`, the default, asks whoever already knows and falls back: the disc's own map, then the container's seek table, then the walk over the packets. `disc` and `container` ask one of those and stop; `scan` always walks, which is what a transport stream needs — it has neither table. A container table is taken only where it covers the whole recording |
| `--proxy` | Build the editing proxy (a light stand-in file) beside the recording and stop (`.proxy.mp4`, or `-o`) |
| `--as-proxy` | Treat the input as a proxy rather than a recording — how a proxy is checked against what it stands in for |

## A disc's subtitles

| Option | Meaning |
|---|---|
| `--subtitles pgs\|beside\|sup` | Where the subtitles a disc draws go. `pgs`, the default, puts them **inside the cut** as the kind a transport stream carries — a Blu-ray's own untouched, a DVD's converted to it. `beside` writes them **next to the cut** as the `.idx` and `.sub` pair every player and subtitle tool reads — a DVD's own untouched, a Blu-ray's read back out of its display sets. `sup` writes those display sets themselves, into a **`.sup` next to the cut** — a Blu-ray's subtitles byte for byte, and what BDSup2Sub and Subtitle Edit read |
| `--drop-subpicture ID` | Leave one of a DVD's subtitle streams out, by the id the disc knows it by (`0x20`). Repeatable |

Only a `.ts` or an `.m2ts` can hold subtitles inside it. Asked for an `.mp4`,
SmartCut says so and writes the pair beside it instead — beside the cut is the
one way a cut of a disc in that container keeps its subtitles. A `.sup` holds
one stream, so a recording with two of them is written as two files, named by
language (`cut_title.eng.sup`). See
[a disc's subtitles](../technical/disc.md#a-discs-subtitles-inside-the-cut-or-beside-it).

## VC-1 discs

| Option | Meaning |
|---|---|
| `--vc1-quant 3..31` | How finely the join pictures of VC-1 material are written; 3 is the finest. The default is 4, which lands around 46 dB against the pictures it replaces |

This setting exists only for VC-1 because **there is no VC-1 encoder anywhere** —
not in FFmpeg, not on a graphics card. SmartCut writes those join pictures
itself, and this is how finely. The details are in
[the Rust core](../technical/rust-core.md#vc-1-the-codec-with-no-encoder).
