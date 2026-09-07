# Using the GUI

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](gui.ja.md)

A walkthrough of the screens in the order you meet them — **add, cut, write** —
from dropping in a night's recordings to writing out the last file.

Installation is covered in the [README](../../README.md#download), why the cuts
land where they do is in [the algorithm](../technical/algorithm.md), and how the
front end is built is in [the design notes](../developers/design.md).

The screenshots below show a synthetic recording that
[`tests/make_demo_media.sh`](../../tests/make_demo_media.sh) builds from ffmpeg's
own test sources: 1440x1080 interlaced MPEG-2, 3 minutes 45 seconds long, with two
commercial blocks in it. No broadcast material is involved.

## Four screens, two windows

The screens follow the order of the work.

| Screen | Where | What it is for |
|---|---|---|
| **Input** | List window, first tab | Line the recordings up. Seek indexes are built in the order the files arrive |
| **Cut editor** | **Its own window** | Open one recording from the list, cut it, and leave with **OK** |
| **Output settings** | List window, second tab | Where to write, which container, what to do with the audio. **Applies to every clip in the list** |
| **Export** | List window, third tab | Write the list out, top to bottom |

The cut editor is the only screen that is not a tab. The other three are settings
you make once for the whole list, while cutting is done one recording at a time
and needs a clear "I am finished with this one" moment. That moment is the **OK**
button, which is why the editor gets a window of its own.

---

## 1. Line the recordings up

![The input screen on startup](../images/usage-empty.png)

You start with an empty list. A **clip** is one row in that list, and stands for
one recording on disk.

### Adding recordings

| How | |
|---|---|
| **Drag and drop** | Anywhere in the window works. Dropping a folder adds the supported files inside it |
| **＋ Add files** | Top right. You can select several files at once |
| **Command-line arguments** | `smartcut recording1.ts recording2.ts`. A `.scproj` project file opens the same way |

SmartCut reads `.ts` `.m2ts` `.mts` `.m2t` `.mp4` `.mkv` `.mov` `.m4v`, and the
program streams a DVD is made of: `.vob` `.mpg` `.mpeg` `.m2p`. Anything else is
ignored, with a line saying so, and the list is left as it was.

### Discs (BDAV, BDMV and DVD-Video)

**A disc becomes one row per recording on it, not a single row.** A folder holding
a `BDAV`, `BDMV` or `VIDEO_TS` directory, an `.iso`, or a folder holding several of
either all work the same way. An `.iso` is not mounted; the streams inside it are
read where they lie.

Dropping one opens **Reading a disc**, which asks two questions.

![Reading a disc](../images/usage-disc.png)

**Which clips do you want?** Every recording on a recorded disc is offered. On a
pressed disc most of the content is not the programme — a season set holds twelve
episodes among fifty logos, warnings and menu loops, and all of them are called
`000NN` — so anything over five minutes is ticked for you and the rest is behind
*Show the short clips too*. Each row shows how long the clip runs and how much of
the disc it takes, which is usually enough to tell an episode from a logo.

**Which tracks do you want?** Open a row's *tracks* and you see what the disc says
that clip carries: the video, the audio tracks with their languages, and the
subtitles and menus. The video cannot be left out. Subtitles and menus on a pressed
disc are always left out, because a cut cannot carry them; the row says so rather
than dropping them quietly. Everything else is yours to switch off. *Use these
tracks for every clip like this one* copies your answer across the whole disc,
which is what you want for a season set.

Rows from a recorded disc are named after the **programme**, not after
`00001.m2ts`: the disc's own index carries the name, the recording time and the
chapter marks. A pressed disc carries no programme names, so its rows are named
after the disc and the clip number.

Cuts are written **beside the disc** unless the output settings say otherwise,
using that name (`cut_2026年08月17日01時00分-BS11….ts`), because there is nowhere
to write inside a disc. "The same as the input" means `.ts` in this case.

A DVD's rows are named after the disc and the title number, and the chapters the
disc set are already on the timeline. Where a DVD title was assembled from pieces
that were multiplexed separately — which is what it means when the timestamps
start again partway through a title — you get one row per piece, marked `(1/2)`
and `(2/2)`, because joining two clocks is a different operation from cutting one.

**A disc written in VC-1 is cut like any other.** Most Blu-rays pressed before
about 2010 are, and neither FFmpeg nor a graphics card has a VC-1 encoder to
rebuild the ends of a range with, so SmartCut writes those pictures itself. There
is nothing to set for it in the window: cutting works exactly the same, and the few
dozen pictures at the ends of each range are written finely enough that the join
cannot be seen. From the command line, `--vc1-quant` sets how finely. What that
costs is described in
[the Rust core](../developers/rust-core.md#vc-1-the-codec-with-no-encoder).

Encrypted discs cannot be opened, DVD and Blu-ray alike. How disc reading works is
in [Reading a disc](../developers/disc.md).

Network (SMB) shares work as command-line arguments, as drops from a file manager,
and in the output folder box. **SmartCut does not mount anything itself.** If you
hand it a share that is not currently connected, it stops and tells you what to do:
open `smb://…` in your file manager first, then add the file again.

### What happens as soon as a clip lands

**A row is filled in the moment the file lands.** The length, the range, the
resolution, the frame rate, the codec and whether there is audio all come from what
the container says about itself, which takes about thirty milliseconds however long
the file is. The picture on the left arrives at the same time, taken with an
approximate seek. Drop in twenty recordings and the last row is never left as a
path and an empty square for minutes.

Behind that, **three lanes** run at the same time: one walks the packets and builds
the seek index, one decodes the key pictures into thumbnails and scene changes, and
one detects commercials. The index is kept on disk, so it is not built again the
second time you open the same recording.

![The list part-way through reading](../images/usage-loading.png)

The bottom right of each row shows how far it has got. `Reading` is the walk,
`Thumbnails` is the decoding pass that produces the pictures and the scene changes,
and a row whose turn has not come says `Queued`. When the walk finishes, the row
says either `Indexed in 2s` (built just now) or `Index from an earlier run` (the
answer was already on disk). The top right of the window shows which lane is on
which clip: above, the second recording is being walked while the first one's
thumbnails are built, and the last two are still waiting — and all four rows
already show what they are, with a picture.

As the walk finishes, its answer replaces the container's on the row. The two
mainly disagree about length: a DVD's program stream does not record its own, so
the container's answer can be wrong.

**No lane makes the editor wait.** A clip that is still being read opens on a
double-click like any other; [Usable the moment it
opens](#usable-the-moment-it-opens) explains what waits and what does not.

![The input screen with four clips](../images/usage-list.png)

Each row shows the filename; then the length in frames, the time range, the
resolution, the frame rate and the codec; and then whatever commercial detection
and your own cuts have to say. On the right, `Smart` means smart rendering applies
to this material, and `CM 2` means two commercial blocks were found.

**The picture on the left follows the cuts.** Recordings tend to start on black or
on the tail of the previous programme, so the picture is taken a little way in
rather than at the head — and, once there are cuts, a little way into *what
survives*. A row whose commercials have been cut never goes on showing one of them.

**You can drag rows into a different order.** The export runs down the list, so
move whatever you want written first to the top. A multiple selection moves
together.

### Working the list

| | |
|---|---|
| **Double-click** / `Enter` | Open that recording in the cut editor |
| `Ctrl+A` | Select all |
| `Ctrl+D` | Detect commercials in the selection |
| `Delete` | Remove it from the list (the file itself is not touched) |
| `↑` `↓` | Move the selection. Hold `Shift` to extend it |
| **Drag a row** | Reorder. `Esc` cancels |
| **⧉ Duplicate clip** | Put the same recording in the list twice. Cuts and marks come with it |

**Duplicating** is for a two-hour recording that contains two programmes: the same
file on two rows, each written out over a different range. The output filenames get
`_1` and `_2` appended.

**Quick properties**, along the bottom, describes whichever single clip is
selected: the path, the codec, the resolution, the scan type, the audio, the
length, how many lossless points there are, how many scenes, and the state of the
index. With several clips selected, it just says how many.

### Detecting commercials

![Commercial detection running](../images/usage-detect.png)

`Ctrl+A` then `Ctrl+D` (or the **Detect commercials** button on the right) runs
detection over everything selected. Dropping in a night's recordings and pressing
`Ctrl+D` once is exactly what this was built for: detection works through them one
at a time, running alongside the indexing and thumbnail lanes rather than behind
them.

**Detection does not wait for the index.** The three reading passes that look for
the signals — captions, audio, logo — only need the recording read from the start.
The index is needed for the last step alone, which moves each boundary it found
onto a real scene change, so that step asks for the index at the end of the pass.
The reading takes minutes and the walk takes seconds, so by then the walk is long
finished.

Progress appears on the row: `Detecting commercials 84% — Looking for the logo`.
Rows whose turn has not come say `Commercial detection queued`. The audio pass
takes a few seconds and the logo pass takes about ten times as long, so the
progress figure is weighted to match.

Detection finds blocks on the 15-second grid, using three signals: caption resets,
silence, and the station logo. The number of blocks and their total length go on
the row. The details are in [commercial detection](cm-detection.md).

**Detection only places marks. The cutting is up to you.** Open the cut editor and
the start of each commercial block, and of each return to the programme, is already
there as a keyframe.

To stop, press **Stop analysis**; pressing it again picks up where it left off.
Detection stops between clips, never inside one.

There is more about running a whole evening's worth at once in
[batch processing](batch.md).

---

## 2. Cut

Double-click a row and it opens in its own window.

![The cut editor](../images/usage-editor.png)

### Usable the moment it opens

The editor does not wait for the recording to be read. What you can do grows in
three stages.

| When | What becomes available |
|---|---|
| **The moment it opens** (30 ms) | Length, resolution, fps, scan type, audio, codec; the timeline, the scrubber, keyframes, **cutting itself**, tracks, commercial detection. The preview and the filmstrip are pictures found by approximate seek |
| **When the walk finishes** (about 1 s per GB) | How many lossless points there are, `Snap to lossless`, the GOP-by-GOP filmstrip, a frame-accurate preview, the export plan, playback |
| **When the thumbnails are built** (about 4 s per GB) | The pictures in the filmstrip, scene changes, the scrubber's hover preview |

**Only two things wait.** `Snap to lossless` has nothing to snap to yet, and `Play`
needs to decode continuously, which an approximate seek gives it no fixed start
for. Everything else works from the first stage.

A cut made before the walk finishes stays exactly where you put it, and no lossless
point arriving later moves it. The only thing that changes is the answer to **what
it costs**: a join that does not land on an access point shows up in the plan as a
few re-encoded frames, and `Snap to lossless` can take those to zero once it lights
up.

![The cut editor before the recording has been read](../images/usage-stages.png)

The band underneath shows how far it has got. While it reads `Reading the
recording. What copies losslessly is known once it has been read`, you are in the
first stage: `Snap to lossless` is greyed out, but the preview is there, the
filmstrip has pictures in it, and you can already make cuts.

**The first stage's filmstrip is approximate, and says so by what it leaves out.**
With nothing read yet there are no lossless points to divide the strip on, so its cells
are cut on an even grid and filled with pictures found by the same approximate seek the
preview uses. A cell takes a picture only if it belongs to the stretch that cell covers,
so how much of the strip fills depends on how much time a cell covers against how often
the recording carries a picture that can be found this way: at `GOP / 6 s` and wider,
all or nearly all of it; at `GOP / 3 s`, where a cell is shorter than the gap between
two such pictures, about half. The rest stay black until the walk finishes.

Each cell is captioned with the time of the picture actually in it rather than with the
time the cell stands for, so the captions can step unevenly and what you click is what
you were looking at. Both settle down the moment the walk lands.

### What is where

| Where | What |
|---|---|
| Top line | The filename |
| Info bar | Lossless points, resolution, fps, scan type, audio, codec. **Tracks** and **Detect commercials** on the right |
| Left column | The **keyframes** — your marks, each with a thumbnail. Click one to jump there |
| The large picture | The preview. Bottom right: frame number, timecode, what kind of frame it is, and the current selection |
| The band under it | The **filmstrip**. Each cell begins at a lossless point and covers about the width the `View` menu to its right asks for — 3 s to 3 min across the band, or frame by frame |
| The scrubber | Green is the output itself. `▼` are keyframes, a red vertical line is a join left by a cut, and the fine ticks below are scene changes |
| The button row | **Cut** in the middle, `[ IN` to its left and `OUT ]` to its right, and outwards from there: go to, one frame, lossless point, start and end |
| The band and lines below | The engine's **plan**: what will be copied and what will be re-encoded |
| Bottom right | **OK** and **Cancel** |

### Getting about

| | |
|---|---|
| **Click** the filmstrip | Go to that frame |
| **Right-drag** the filmstrip | Search back and forth. Right of centre is forwards, left is back, and further out is faster |
| **Middle-click** the filmstrip | Jump to the next scene change |
| **Wheel** over the filmstrip | One frame per notch. Hold `Shift` to hop from lossless point to lossless point |
| **Drag** the scrubber | Move the playhead. Grab near the IN or OUT mark and you move that mark instead |
| **Hover** the scrubber | Shows the frame at that moment in a small picture |
| `Space` or **▶ Play** | Play from here, picture and sound. Press again to stop |
| `←` `→` | Back and forward one frame. Hold to repeat |
| `Shift+←` `Shift+→` | One second |
| `S` / `Shift+S` | Next / previous scene change |
| `◀\|` `\|▶` | Previous / next **lossless point** |
| `\|◀` `▶\|` | Start / end |

**Every number on this screen is on the output's clock.** What you cut does not go
grey — it disappears. The scrubber shrinks, the filmstrip closes over the hole, and
the frame counter counts the length that will actually be written.

### Marks and edits are two different things

- A **keyframe** is a *mark*, not an edit. `⚑ Keyframe` (or `K`) puts one on the
  frame you are on. Marks are listed down the left with a thumbnail each; click one
  to jump there, or click its `×` to remove it.
- A **cut** is the edit. Set IN and OUT, press `✂ Cut`, and that range leaves the
  output.

Marks can also arrive without your placing any: from a detection, from a
`.keyframe` file next to the recording, and — for a recording opened off a
**disc** — from the chapters the recorder itself set, which on a Japanese recording
are frequently the commercial breaks. Those are read on the first visit only, and
where there is a `.keyframe` file, it wins.

### Selecting with IN and OUT

![A commercial block selected](../images/usage-selection.png)

Click the keyframe at the head of the commercial block and press `I`. Then click
the keyframe where the programme comes back, press `←` to step one frame off it,
and press `O`. That puts the block exactly inside the selection.

- **IN to OUT includes the OUT frame.** Select five frames and five frames go.
- **Setting one end leaves the other alone.** You usually place IN and OUT one at a
  time as you close in, so re-placing IN is no reason to lose OUT. Only when the two
  cross does the one you just placed win, and the other retreats to the end of the
  timeline.
- The selection is shown under the preview and on the status line, as
  `Selection 1800 - 3599 : 00:01:00.06`.

### Cutting

![After the cut](../images/usage-cut.png)

`✂ Cut` takes the selection out of the output. The screen is rebuilt immediately,
and the band and lines below tell you what will be written:

```
Output 00:02:44.94 (2 ranges, 1 cuts) — copied losslessly 164.94s (100%) / re-encoded 0.00s
copy 00:00:00.00 → 00:01:00.05 (1800 frames)
copy 00:02:00.11 → 00:03:45.00 (3143 frames)
```

If the badge at the bottom left reads `Video completely lossless`, not one frame of
this output will be re-encoded. Cut the second commercial block the same way and it
becomes `3 ranges, 2 cuts`.

**A join left by a cut becomes a keyframe of its own**, because that is exactly the
place you will want to check afterwards. The scrubber keeps a red line there.

### Cutting again, and cutting wider

| Button | |
|---|---|
| **Cut outside** | Drop everything outside the selection. One press for lifting a single stretch out |
| **Snap to lossless** | Move both ends of the selection to the nearest lossless point. Press it and the re-encoding goes to zero |
| **↺ Undo** | Take the last cut back (fifty deep) |
| **Clear all** | Remove every cut and every keyframe |

### When re-encoding is needed

![A cut that needs re-encoding](../images/usage-reencode.png)

When a cut point lands inside a GOP, that partial GOP alone is rebuilt. The orange
part of the band and the `re-encode` lines are those frames — above, **14 frames**
out of 461, leaving 97.0% of the length a byte-for-byte copy.

Commercial boundaries in a broadcast recording sit in silence, and silence is
usually a lossless point as well, so removing commercials often comes out
completely lossless. It is cutting at an arbitrary moment that costs those fourteen
frames, and **Snap to lossless** takes them back to zero.

### Choosing which tracks are written

![The track menu](../images/usage-tracks.png)

**Tracks**, on the info bar. A broadcast recording contains more than a picture and
a sound, and this is where you say which of it goes into the output.

**Everything is on by default.** The case this menu exists for is switching off the
second language on a bilingual broadcast. A track nobody asked about is still a
track that was in the recording, and dropping it silently would mean the program
deciding what the recording is for.

Captions can only be kept when writing a `.ts`. Superimposed text and data
broadcasting cannot be carried on a cut timeline at all, so they appear as `not
carried` rather than as choices. Programme information (EIT), the station name and
the broadcast clock are not tracks and so are not listed, but they are carried
across when writing a `.ts`.

This choice is a fact about *this clip*, so it travels back with the edit and is
saved in the project. Two duplicates of one recording can answer it differently.

### Leaving

- **OK** — take the cuts and marks back to the list and close.
- **Cancel** — throw away what was done here and close.

Either way, the list window has stayed where it was, ready for the next recording.

---

## 3. Output settings

![The output settings screen](../images/usage-output-settings.png)

**These settings apply to every clip in the list.** The top half is there to be
read: pick a clip and it shows what that recording will become under the current
settings — video, audio, how many ranges, how long the output is, and the path it
will be written to.

**The audio encoding settings appear only when something is being encoded.** Five
of the rows below — the codec, the channels, the rate, the bit depth and the
bitrate — all describe an encode, and the other two audio modes do not run one over
the whole track. So instead of sitting there greyed out under every mode, they
appear when `Re-encode everything` asks for them:

![The audio settings, under Re-encode everything](../images/usage-output-audio.png)

| Field | |
|---|---|
| **Output folder** | Empty means alongside the input. Use `Browse`, or type a path (an SMB path is fine) |
| **Filename prefix** | `cut_` by default, so `cut_recording.ts` |
| **Container** | `Same as the input`, or a specific one. The extension is what decides the container |
| **Audio** | `Smart rendering (default)` / `Copy through` / `Re-encode everything` |
| **Audio codec** | `Same as the input`, or AAC, AC-3, DTS, linear PCM. Asking for a codec the recording does not carry **re-encodes the whole track** |
| **Audio channels** | `Same as the input`, or 1ch, 2ch, 5.1ch. Asking for a different count is a downmix, and **re-encodes the whole track**. Counts above what the recording carries are greyed out |
| **Sample rate** | `Same as the input`, or 96 / 48 / 44.1 / 32 kHz — 96 kHz being what a Blu-ray's LPCM is carried at. A different rate is a resample, and **re-encodes the whole track**. A rate the codec being written does not support is greyed out rather than offered (AC-3 and DTS have nothing above 48 kHz), and so is any rate above what the recording was sampled at. Where a codec cannot manage the recording's own rate either, the nearest rate it does support is used |
| **Bit depth** | `Same as the input`, 16 or 24 bit; 24 is greyed out for a recording that has 16 bits in it. The whole row is grey unless `Linear PCM` is being written, because every other codec writes a description of the sound rather than the sound itself and has nowhere to put a sample width. For uncompressed audio it decides the track's size outright — channels × width × rate — which is the figure shown in place of the bitrate |
| **Audio bitrate** | For the frames that are rebuilt. `Leave it to the engine` picks what the codec is worth at that channel count. Only rungs the encoder will actually open at are listed; DTS has a floor as well as a ceiling |
| **Write the keyframes to a separate .keyframe file** | Puts a `.keyframe` file next to the video, under the same name |

**How the three audio modes differ.** The default, `Smart rendering`, rebuilds only
the few frames a boundary falls inside, so nothing from the far side of a cut is
heard; cut in silence and the result is byte-identical to a copy. `Copy through` is
lossless to the byte, but a little of the cut-away side survives at the join.
`Re-encode everything` cuts to sample precision, at the price of rebuilding the
whole track. There is more detail in [audio](../technical/audio.md).

**Audio that came off a disc.** Blu-ray LPCM is smart rendered, the same as AAC.
DTS-HD and TrueHD are lossless, and nothing here rebuilds a frame of either: they
are copied whichever mode is set, boundaries and all, and SmartCut says so. MP4 has
no box for Blu-ray LPCM, so when writing MP4 the same samples go in as plain PCM —
nothing is lost — and TrueHD goes in as well, outside the standard and starting a
few milliseconds after the pictures. A `.ts` or an `.m2ts` carries all of it exactly
as the disc had it, which is what `Same as the input` gives you.

**What choosing a codec does.** The only frame that can be copied is a frame that
is already in the codec being written, so asking for another codec means asking for
the whole track to be rebuilt — the same reasoning as a downmix. Choose by what the
file is for afterwards: AAC is what the broadcast carried and what plays on a phone;
AC-3 is what a disc player or an AV receiver decodes without complaint, and DTS is
the other one a receiver knows; linear PCM is uncompressed, which is what to hand to
an editor that will encode it again later.

The bitrates on offer change with the codec. AC-3 and DTS both carry the rate as a
number out of a table the format defines, so only the rates in that table are
offered. **For linear PCM the bitrate is greyed out and shows what the track will
actually cost**: an uncompressed track's size is channels × bit depth × sample rate,
so there is nothing to choose and everything to be told. Where the clips in a list do
not all cost the same, the range is shown; the audio line in the panel above, which
describes one clip, always shows that clip's own figure.

One surprise the figure accounts for: Blu-ray's LPCM writes its channels in pairs,
so a mono track in a `.ts` costs two channels' worth of bytes — 1536 kbit/s, not
768.

Linear PCM written into a `.ts` registers the transport stream itself as HDMV —
Blu-ray's flavour — because that is the only way a transport stream can declare
LPCM at all. In a `.mp4` or `.mkv` it becomes plain big-endian PCM.

**What cannot be written is greyed out.** Not every combination of these five
settings is a file that can be written, and which combinations work depends on the
recording and on where it is going. Blu-ray LPCM — the only linear PCM a transport
stream can declare — is written at 48 kHz, not at 44.1. DTS is written as mono,
stereo, quad, 5.0 or 5.1 and nothing else, and a DTS frame has to be long enough to
describe every channel in it, which puts a floor under the bitrate that moves with
the channel count and the sample rate: 5.1 at 48 kHz starts at 768 kbit/s.

Those options stay on their lists but cannot be chosen, because a list that quietly
shortened itself could not tell you what was missing. Which of them are grey changes
with everything else on the panel: choose 44.1 kHz and `Linear PCM` goes grey; switch
the container to MP4 — where the samples go in as plain big-endian PCM, which takes
any rate — and it comes back. The answers come from the engine, not from a table this
window keeps, so what is offered is what the encoders in this build will actually
open at.

**Nothing above what the recording itself has is offered either.** Three of these
rows describe the samples — how many channels, how often they were taken, how wide
each one is — and a re-encode can write more of all three than came in, but none of
the three brings anything with it. Six channels made out of two are two channels'
worth of sound spread into six; 48 kHz made out of 44.1 is the same curve drawn
through more points; 24-bit samples made out of 16-bit ones are the same numbers with
eight zeroes on the end. All that arrives is the extra size. So each of those three
rows offers what the recording has and everything below it, and greys out the rest.
96 kHz is on the rate list for the recordings that have it — a Blu-ray carrying LPCM
or lossless audio at 96 — and grey under a broadcast, which is sampled at 48.

For a list, "what the recording has" means the least any of them has: the settings
are one answer for every clip in the list and every track in each of them, so a list
holding a stereo recording beside a 5.1 one offers 2ch as its widest. Asking for 5.1
would leave the stereo one spread thin. A recording that has not said yet decides
nothing, and until there is something readable in the list, the whole of each row is
offered.

**The five rows under the mode — codec, channels, sample rate, bit depth and
bitrate — appear only when the mode is `Re-encode everything`**, because they
describe an encode that the other two modes do not run over the whole track. They
keep their values while they are hidden, so they are still there when you switch
back.

The `.keyframe` file contains **line numbers only, CRLF, no header**, and the
numbers are on the written file's clock. A `.keyframe` file sitting next to a video
is read back automatically when that video is opened in SmartCut.

---

## 4. Write

![The export finished](../images/usage-export.png)

`Start export` writes the list out from the top down. Each row carries its own
progress and result, and above them are the overall state, the elapsed time and the
time remaining. At the end it says `4 of 4 written`.

`Stop export` finishes writing the clip currently in progress, then stops.

### This screen shows the frames that get re-encoded

![The frames that get re-encoded](../images/usage-export-reencode.png)

The large picture is not a representative frame. It is a frame that will be
re-encoded — the only place in the output whose quality is this program's doing.
`Re-encode 1 of 2` says how many such places there are, and the line above says how
many frames in total.

For a clip whose cuts all landed on lossless points, you get its representative
frame instead, with `Nothing re-encoded — the whole clip is copied losslessly`
written underneath.

---

## Saving your work

![The SmartCut menu](../images/usage-menu.png)

The **SmartCut** button at the top right saves and opens projects (`Ctrl+S` /
`Ctrl+O`). A project holds the list itself: the paths, the cuts and marks you put
in, the track choices, and the output settings.

A `.scproj` file is only a few hundred bytes, and it opens on another machine or
after the cache has been cleared. See [projects](projects.md) for what is in one and
why.

## Language and version

![Preferences](../images/usage-prefs.png)

**Preferences…**, in the same menu, is where you choose the interface language:
English, Japanese, or follow the system (the default). A change takes effect in both
windows at once and is remembered for next time.

![About](../images/usage-about.png)

**About SmartCut** gives the versions: the program and the cutting engine, the
FFmpeg libraries actually loaded and their licence, and the platform. This is the
one place in either window where text can be selected and copied, so you can quote
it straight into a bug report.

---

## Keyboard reference

### List window

| | |
|---|---|
| `Ctrl+A` | Select all |
| `Ctrl+D` | Detect commercials in the selection |
| `Ctrl+S` / `Ctrl+Shift+S` | Save project / save as |
| `Ctrl+O` | Open project |
| `Enter` / double-click | Open the cut editor |
| `Delete` | Remove from the list |
| `↑` `↓` (with `Shift` to extend) | Move the selection |

### Cut editor

| | |
|---|---|
| `Space` | Play / stop |
| `←` `→` | One frame (hold to repeat) |
| `Shift+←` `Shift+→` | One second |
| `I` / `O` | Start / end the selection here |
| `K` | Mark this frame as a keyframe |
| `S` / `Shift+S` | Next / previous scene change |
| `Ctrl+D` | Detect commercials |

---

## Troubleshooting

| Problem | What to do |
|---|---|
| **Dropping a file does nothing** | Check the extension (`.ts` `.m2ts` `.mts` `.m2t` `.mp4` `.mkv` `.mov` `.m4v` `.vob` `.mpg` `.mpeg` `.m2p`). A folder brings in the supported files inside it, and nothing else |
| **"Not connected to `\\nas\rec`"** | Open that share in your file manager first. SmartCut does not mount shares itself |
| **The captions are not in the output** | Captions survive only into a `.ts`. Check the container in the output settings |
| **The editor's picture is coarse, or slow to arrive** | It is still being read. The first stage's preview and filmstrip come from approximate seeks; they become frame-accurate when the walk finishes, and the filmstrip fills in completely when the thumbnails are built (see [Usable the moment it opens](#usable-the-moment-it-opens)). The index is built the first time only |
| **I want zero re-encoding** | Select the range and press `Snap to lossless`. If that does not do it, this material's cut points do not fall on access points |
| **An unsupported codec or track layout** | See [known limits](../technical/validation.md#known-limitations) |

---

## Command-line reference

The same engine is available as `smartcut` (`smartcut-cli` if you installed the
`.deb`).

```bash
smartcut input.ts --keep 5.3-12.7 -o out.ts   # keep this range
smartcut input.ts --cut 8.0-20.0  -o out.ts   # drop this range
smartcut input.ts --analyze                   # show the plan, write nothing

smartcut input.ts --analyze --detect-cm --logo  # list the commercial candidates
smartcut input.ts --analyze --scenes            # list the scene changes
```

| Option | Meaning |
|---|---|
| `--keep START-END` / `--cut START-END` | Ranges to keep or drop. Repeatable. The `1:23:45.6` form is also accepted |
| `--audio-mode smart\|copy\|reencode` | `smart` (the default) re-encodes only the frames a boundary falls inside, so nothing from the far side of a cut is heard — and nothing at all when the seam falls in silence. `copy` is lossless to the byte; `reencode` is sample-accurate |
| `--audio-codec source\|aac\|lpcm\|ac3\|dts` | What the audio is written as. `source`, the default, is the recording's own codec. Anything else leaves no frame that can be copied, so the whole track is re-encoded whatever `--audio-mode` says. Without `--audio-bitrate` it takes what that codec is worth at that channel count; LPCM has no bitrate at all |
| `--audio-channels N` | Channels to write, 1 to 8. Anything but the recording's own count is a downmix — 5.1 folded to stereo, for players that make a mess of surround — and a downmix has no copy path, so the whole track is re-encoded whatever `--audio-mode` says |
| `--audio-bitrate RATE` | Bits per second for re-encoded audio, as `192k` or `192000`. Left out, it follows the recording, and comes down with the channel count when there is a fold. A figure the encoder will not open at — DTS has a floor that moves with the channels and the rate — is raised to what that codec is ordinarily carried at, and SmartCut says so |
| `--audio-samplerate RATE` | Samples per second for re-encoded audio, as `48k` or `48000`. Left out, it follows the recording. A rate that is not the recording's is a resample, and like a downmix it has no copy path, so the whole track is re-encoded whatever `--audio-mode` says. Not every codec supports every rate — AC-3 has three, Blu-ray LPCM three others — and a rate the codec does not have is taken to the nearest it does, with a note saying which |
| `--audio-bits 16\|24` | How wide the samples are written. This only means anything when linear PCM is being written: a lossy codec takes a float and spends a bitrate, so a width asked of one is declined with a message. Left out, it follows the recording |
| `--aac auto\|mpeg2\|mpeg4` | Which flavour of AAC the frames SmartCut writes announce themselves as. `auto` follows the recording, which for a broadcast means MPEG-2 AAC |
| `--vc1-quant 3..31` | How finely the partial GOPs of a VC-1 recording are written, 3 being the finest. Only VC-1 has this setting, and only because it is the one codec with no encoder in libavcodec: SmartCut writes those pictures itself, and what it writes them at is a quantiser step rather than a bitrate. Left out, it uses 4, which lands around 46 dB against the pictures it replaces |
| `--index scan\|container` | How access points are indexed. `container` is faster but unavailable for TS |
| `--seek-index PATH` | Where to keep the seek index. Written on the first run and read on the next, which skips the walk over the packets |
| `--detect-cm` / `--logo` / `--scenes` | Commercial candidates, logo assist, scene detection |
| `--drop-stream INDEX` | Leave one of the recording's streams out of the output. Repeatable. The same thing the cut editor's **Tracks** menu does |
| `--title N` | Which recording on a disc (a folder or an `.iso`) to open. Part of the programme's name works in place of the number. Without it, the disc's recordings are listed and nothing else happens |
| `--tables partial\|broadcast\|muxer` | How a `.ts` describes itself. The default, `partial`, writes a partial transport stream (one SIT, per DVB EN 300 468 Annex C / ARIB TR-B15); `broadcast` puts the recording's own PMT, SDT, EIT and TOT back; `muxer` leaves the muxer's own tables standing |
| `--no-open-gop` | Never start a copy at an open GOP |
| `-o OUTPUT` | Output path. The extension picks the container |
