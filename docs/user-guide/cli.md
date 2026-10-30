# Using the command line

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](cli.ja.md)

**The same engine that runs inside the GUI** is also available as a command.
The packages install it as `smartcut-cli` — `smartcut-cli.exe` on Windows,
`./smartcut-cli` in the unpacked tar.gz — and `smartcut` is the GUI. Built
from source, the command is called `smartcut`, which is what the examples on
this page use.

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
- **`--analyze` writes no cut.** It prints the plan — what gets copied and what
  gets rebuilt. Running it first is the safe way to check before committing.
  (What other options write is still written: a `--seek-index` file, say.)
- `smartcut --help` lists every option, grouped by what it is for.
- `smartcut --version` prints the version as `smartcut 1.2.3` and nothing else,
  for a script that has to know which one it is running.

## Choosing where to cut

| Option | Meaning |
|---|---|
| `--keep START-END` | A range to keep. Repeatable |
| `--cut START-END` | A range to drop. Repeatable. Not with `--keep` |
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
| `-o OUTPUT` (or `--output`) | Output path. **The extension picks the container**. It has to be a file on this machine, and not the recording being read (or a link to it): either is refused before anything is written. On Linux a share named the Windows way or as a URL (`\\nas\rec\x.ts`, `smb://nas/rec/x.ts`) is written through the place it is mounted, as an input named that way is read; a share that is not mounted is refused. An extension that holds sound alone (`.aac`, `.ac3`, `.eac3`, `.mp2`, `.mp3`, `.dts`, `.wav`, `.aiff`, `.flac`, `.opus`, `.thd`) is accepted only with `--sound-only` |
| `--drop-stream INDEX` | Leave one of the recording's streams out of the output. Repeatable. The same thing the cut editor's **Tracks** menu does. The index is a sound, caption or subtitle stream's; one the recording does not have, or the pictures', is an error. A broadcast's sound track with nothing in the ranges kept (a second track the programme before this one had, say) is left out without being asked, and the run says so |
| `--title N` | Which recording on a disc (a folder or an `.iso`) to open. Part of the programme's name works in place of the number. **Left out, it lists what is on the disc and stops.** If the run also asked for something that needs a recording open (a cut, an output, `--analyze`, a `--seek-index`, `--as-proxy` and the like), the list is followed by an error. **A recorder's title cut with no `--cut` or `--keep` keeps what the disc's playlist plays.** The clip a recorder writes holds more than that: a second or so of the programme before, the moment either side of a break the recorder left out, and the start of the programme after. Those stretches are cut, on the pictures, and the run names them on a `plays :` line. No chapter is put where they were. A pressed disc, a DVD and a disc made with an authoring tool play the whole clip, and are cut as before |

## Joining several recordings

| Option | Meaning |
|---|---|
| `--join RECORDING` | Another recording, written into the same output after this one. **Repeatable**, and they go in the order they are named. Where the master's sound is TrueHD or DTS, a recording whose sound is another codec is refused before anything is written; make it the master with `--master`, or name a codec with `--audio-codec`. A disc, a folder or an `.iso`, is refused at once, before the first recording is read: name one of its recordings by the path `smartcut DISC --title N --analyze` prints on its `input :` line. The refusal gives the first recording's as an example — `DISC/BDAV/STREAM/00001.m2ts` on a recorder's disc, `DISC/VIDEO_TS/VTS_01_1.VOB@…-…` with its stretch on a DVD; `VTS_01_1.VOB` alone would open only the first gigabyte of the title set, not a title. A folder that holds no disc is refused at once as well, and so is an `.iso` that holds no disc that can be read: it is not a recording either |
| `--master N` | Which of them the output takes its shape from — frame size, rate, codec, sound tracks, tables. Counting the first as 1. **Left out, it is the first**. A number past the last recording is refused |

`--keep` and `--cut` belong to the first recording; a joined one is written
whole. A command line giving every file its own ranges would be a project file
with a worse syntax, and the window is where a list with cuts in it belongs.

```bash
smartcut part1.ts --join part2.ts --join part3.ts -o whole.ts
```

A recording that is not the master's shape is decoded and written afresh to
fit it — scaled, rate-converted, re-encoded — and the run says which and why
before it writes a byte. Everything that matches is smart-rendered as usual.

## Transitions at the joins

| Option | Meaning |
|---|---|
| `--transition KIND` | `none`, `fade-black`, `fade-white`, `dissolve`, `wipe-left\|right\|top\|bottom`, `slide-left\|right\|top\|bottom`. Applied at **every** join between two clips |
| `--transition-seconds S` | How long it runs. Up to 30; 1 when left out |
| `--transition-easing C[:M]` | The curve and the end it is applied at: `none`, `back`, `bounce`, `circle`, `elastic`, `exponential`, `power`, `sine`, `quadratic`, `cubic`, `quartic`, `quintic`, each with `:in`, `:out`, `:in-out` or `:out-in`. Default `none:in`, which is a straight line. A name not on the list is refused |
| `--transition-image FILE` | A still laid over each crossing, coming up and going down with it |

All four are about the joins, and so are `--join-fade-out` and
`--join-fade-in`: given without a `--join`, they stop the run rather than
being ignored.

```bash
smartcut a.ts --join b.ts --transition dissolve --transition-seconds 2 \
  --transition-easing sine:in-out -o joined.ts
```

**A fade keeps the output's length** — it takes half its time from each side.
**Everything else takes its own seconds off** the output, because both clips
are on screen at once for the whole of it.

Every frame a transition covers is written afresh: it is pictures that are in
neither recording. Two seconds is two seconds of encoding per join and nothing
anywhere else. The sound is not mixed. Where the two clips are on screen
together, the clip before plays through the crossing and the clip after starts
where it ends; through a fade, the sound changes over at the colour, in the
middle. `--audio-fade` is what softens the change if it needs softening.

The captions change over where the pictures do. Through a dissolve, a wipe or
a slide, a broadcast's captions and a 4K recording's subtitles are the clip
after's from the crossing's first frame: the line it has up there comes up at
once, what it says inside the crossing comes at its own time, and the clip
before's line goes when the clip after's first one comes. Where the clip after
says nothing inside the crossing, the clip before's lines run on as they would.
Through a fade the two follow one another, as the pictures do. Up to 0.8.35
the clip after's lines waited for the crossing to end, and one said and gone
inside it was lost. A disc's subtitles, which only the master carries, do the
same where the master is the clip after (up to 0.8.36 they waited for the
crossing to end as well). See
[The captions at a crossing are the clip after's](../technical/joining.md#the-captions-at-a-crossing-are-the-clip-afters).

## Audio

By default the sound is treated like the video: only **the few frames a join
falls inside** are rebuilt (smart rendering). Give any of the five settings
below a value the recording does not already have and there is no frame left to
copy, so **the whole track is re-encoded**.

Before it starts, the run prints a `render:` line saying what is done with the
sound (`audio smart`, or `audio reencode` and how), on the main track -- or,
where `--drop-stream` takes that away and always with `--sound-only`, the first
track kept, which is the one a file of sound alone is written from. It says what
the cut will really do, not what was asked: a width asked of AAC, or the track's
own codec named with `--audio-codec`, leaves it smart; a DTS or TrueHD track,
which is carried through whole unless another codec is named, is not shown as
downmixed or resampled, and asked to be re-encoded with no other codec named it
says `copy`, which is what it gets; a Blu-ray's or a DVD's LPCM going into a
`.mkv` or an `.mp4`, which hold it only as plain PCM, says `reencode` and what
it is written as (`as pcm_s16be`, for one); `--audio-codec lpcm` on a Blu-ray's
own LPCM into a `.ts` or `.m2ts` stays smart; and a `.wav` of sound alone says
it is re-encoded as linear PCM.

| Option | Meaning |
|---|---|
| `--audio-mode smart\|copy\|reencode` | `smart` is the default: it rebuilds only the frames a boundary falls inside, so nothing from the far side of a cut is heard — and nothing at all when the join falls in silence. `copy` is lossless to the byte but leaves a little of the far side audible; `reencode` cuts to the sample at the cost of rebuilding the whole track |
| `--audio-codec source\|aac\|lpcm\|ac3\|dts` | What the audio is written as. `source`, the default, keeps the recording's own. AAC plays straight off a phone, AC-3 and DTS are what an AV receiver handles most reliably, and linear PCM is uncompressed |
| `--audio-channels N` | Channel count, 1 to 8. Anything but the recording's own is a downmix — 5.1 folded to stereo, for instance |
| `--audio-samplerate RATE` | Sample rate, as `48k` or `48000`. Codecs differ in what they support (AC-3 has three rates, Blu-ray LPCM three others); a rate a codec does not have is taken to the nearest one it does, with a note saying which |
| `--audio-bits 16\|24` | How wide each sample is. **Only meaningful for linear PCM** — the other codecs write a description of the sound rather than the sound itself, so there is nowhere to put a width. A recording's own plain PCM written into a `.mkv` or `.mov` is written at the width asked for, float and 8-bit samples included. The signed 8-bit PCM an old `.mov` can carry goes into a `.mkv` or an `.mp4` only as 24 bit; otherwise the run is refused before anything is written, and the message says so |
| `--audio-bitrate RATE` | Bits per second when re-encoding, as `192k` or `192000`. Left out, it follows the recording. A figure the encoder will not accept is raised to what that codec is ordinarily carried at, with a note saying so |
| `--aac auto\|mpeg2\|mpeg4` | Which flavour of AAC the frames SmartCut writes announce themselves as. `auto`, the default, follows the recording — MPEG-2 AAC for a broadcast |
| `--audio-es` | Also write the sound out as a bare stream beside the output. AAC only |
| `--poster TIME` | Carry the picture at `TIME` into the recording as the file's cover (thumbnail): `covr` in `.mp4` and `.m4v`, an attachment named `cover.jpg` in `.mkv`. Other containers have no place for one (FFmpeg writes no `covr` into a `.mov`), and the run says it is ignored. A time past the end takes the last picture, with a note. An interlaced recording's cover is taken from one field. Not with `--sound-only` |
| `--no-chapters` | Leave the chapter list out. Otherwise an `.mp4`, `.m4v`, `.mov`, `.mkv` or `.webm` gets one chapter where each kept range begins (and where each `--join` recording begins), plus the chapters the recording came with that survive the cut — the disc's own for a recording off one, otherwise the chapter list an `.mp4` or `.mkv` input already has — the same points `--bdav` writes into a playlist, which this does not change. A cut with only one range and nothing else gets no chapter list. Not with `--sound-only` |
| `--sound-only` | Write the sound and no pictures: the ranges, the joins and the fades exactly as they would be inside the video, and nothing read or written for the frames. `-o` names the file and its extension picks the container — `.aac`, `.ac3`, `.eac3`, `.mp2`, `.mp3`, `.dts`, `.m4a`, `.mka`, or `.wav`, which is written as linear PCM unless `--audio-codec` names another codec. `.aiff` also takes linear PCM, and `.flac`, `.opus` and `.thd` take a recording's own FLAC, Opus or TrueHD. A sound the container has no room for stops the run before anything is written. A 4K broadcast's LATM-framed AAC asked for as `.aac` or `.m4a` is re-encoded as plain AAC, and the run says so; `.mka` keeps its own frames. One sound track, taken from the recording `--master` names, and not with `--bdav`, `--video-share` or `--audio-es`, nor with what only a file with pictures has: `--tables`, `--data-broadcast`, `--no-data-broadcast`, `--subtitles`, `--drop-subpicture`, `--vc1-quant`, `--clean-joins`, `--no-open-gop`, `--transition-image` or `--no-chapters`. A recording joined on with no sound is left out of the file, and the run says so. On a recorder's title, the time at a seam that no picture is written for is left out of the sound too, so the file is as long as the video would be. With several ranges, each one lines up within a frame with the sound of the video cut of the same ranges |
| `--audio-fade SECONDS` | Take the level down into each seam and bring it back out over that many seconds. 0 to 10; 0, no fade, is the default |
| `--join-fade-out SECONDS` | How long the sound takes to leave at the end of each clip, where `--join` writes several into one file. 0 to 10; 0 is the default |
| `--join-fade-in SECONDS` | ...and how long it takes to come back at the start of the next one |

> **A join between two clips is asked for at each end.** How the programme that
> is ending should end and how the one that is starting should start are two
> questions: `--join-fade-out 3 --join-fade-in 0` takes three seconds to leave
> and starts the next one at full level. The seams *inside* one recording take
> `--audio-fade`, which is one answer for the run.
>
> **What these reach.** Only sound this program writes: `smart` and
> `reencode` fade, `copy` does not, and neither does sound carried through whole
> because re-encoding it would lose what makes it lossless (TrueHD, DTS-HD MA).
> Where it cannot be applied the cut says so. The beginning and the end of the
> output are left alone: a fade is for a place where two pieces meet.

## Writing a broadcast `.ts`

| Option | Meaning |
|---|---|
| `--tables partial\|broadcast\|muxer` | How a transport stream describes its own contents. Unsaid, a `.ts` gets `broadcast` — the recording's own SDT, EIT and TOT, which is where a player reads the programme name, the station and the clock — and a `.m2ts` gets `partial`, the shape a disc's stream is written in. `muxer` adds nothing. `--no-tables` is the old name for `muxer`. With `--bdav` only `partial` is taken: a disc's stream is written in that shape, and `muxer` or `broadcast` stops the run before anything is read |
| `--no-data-broadcast` | Leave out the recording's data broadcast — what is behind the d button. It is otherwise carried into any `.ts` whose tables are not left to the muxer (`broadcast` or `partial`); a `.m2ts` has nowhere to put one: the modules come out whole and byte for byte, so a receiver draws the same pages. What turning it down buys is size — a carousel is between a hundredth and a fifth of what a broadcast multiplex spends. `--data-broadcast` asks for it outright, which only changes what is said when it cannot be carried. With `--join`, every recording's carousel goes in over its own stretch and is listed as `data   : carousel` among that recording's lines; one whose data component is described otherwise than the same component already in the output (another station's profile or BML version), or whose entry component cannot be announced, is left out whole with a note and listed as `not carried`; this takes a master that is itself a transport stream, since the tables, the programme guide included, are put back from that recording |

## Writing a disc (BDAV)

`--bdav` writes **the same shape a recorder writes to a BD-RE** — a folder, not a
file. The disc decides the filenames, so `-o` is not used, and giving both is an
error. If a disc is already there, the recording is **added** to it. `--iso` and
everything below it in the table describe a disc, and without `--bdav` they are
refused rather than quietly ignored.

| Option | Meaning |
|---|---|
| `--bdav FOLDER` | Write a BDAV disc into this folder (one recording becomes `BDAV/STREAM/00001.m2ts`). The index is built afterwards. Pictures or a kept sound track that a disc's index has no coding type for (Opus, for one) stop the run before anything is cut or a place on the disc is taken; `--audio-codec` or `--drop-stream` gets past the sound. A run that stops before the recording is written removes the stream file it set aside on the disc, and takes away the folders it made for it (`BDAV` and the ones in it, and the folder itself and its parents where the run made them) as long as they are still empty. So does a run whose recording was written but could not be put in the disc's index: that recording is taken off the disc, and the folders go with it the same way, while a disc that was there before keeps its `info.bdav` as it was. None is taken away while another run's recording is in that disc's `STREAM` folder, and a folder that was there before stays, empty or not, and so does every folder where the path given goes through `..`. The folder has to be on this machine, as `-o` does: one named as an address (`http://…`) is refused at once, before the recording is read, and by `--analyze` too |
| `--fit bd25\|bd50\|bd100\|bd128\|BYTES` | Write the pictures back smaller, by as much as it takes for the output to fit that much room. Where it already fits, nothing is done. The sound, the subtitles and the programme information are untouched; only the pictures give anything up. The sound is counted as it will be written, so `--audio-codec lpcm` or a track left out with `--drop-stream` is in the sum. MPEG-2 only -- anything else is written at its full size and says so, and where that does not fit the run stops. What share they will be written at is printed before the writing starts. The rewrite does not always get there -- short ranges at a hard share can stay several points above it -- and where it stops short, a `note:` says how far, and how many MB more than planned the pictures it rewrote came to (the cut as a whole may still fit). Once the run is over, the output (with `--bdav`, the whole disc, what was on it already included; with a read-only `--iso`, the image itself) is weighed against the size `--fit` priced against (with `--iso-access overwritable`, what the recorder's layout holds), and where it came out larger a `note:` says by how much and why -- where the pictures were written smaller, what the run was priced at with them at that share and how far past that it came (the recording weighed more than its index measured, or the pictures would not shrink as far as asked, which a `note:` above says), or the run was priced to fit as it was, or, for an image whose files alone are under the size, the image's own file system, which `--fit` does not price; the output is left as it is. Each range kept is priced from the bytes the recording holds there, read off its index, rather than from the recording's average, the way the window's disc gauge prices it. On a disc the stream's own tables are priced by the second, since they go out at a fixed rate however small the pictures become; an AAC track in a transport stream (`.ts`, `.m2ts`, a disc) whose stated rate is less than half of what AAC usually takes at that many channels is priced at that usual figure, since there the rate is not recorded and what is stated is the average over the opening libav reads, which a quiet opening makes far too low -- a track really carried that low is priced high, and the pictures come out a little smaller than they needed to. A rate an MP4 or Matroska file states is believed however low. With `--bdav` naming a folder that already holds a disc, what is already written there is measured and the output is fitted into the room left; the `fit` line says how much that is. With `--iso-access overwritable` the output is fitted into what a recorder's layout holds of files -- 24.12 GB on a single layer, 48.34 GB on a dual -- and the `fit` line says so too. Not with `--join` or `--sound-only`. See [Fitting a disc](../technical/transrate.md) |
| `--video-share 0.35..1` | The share itself, where you would rather name it than have a size worked out into one. Not with `--fit`, which works one out. MPEG-2 only, as with `--fit`: other pictures are copied at their full size, and the run says so. The pictures re-encoded at the ends of each range follow the ones around them -- written at the share where those are written smaller, at the full rate where they are copied. Up to 0.8.35 an H.264 cut's seams were re-encoded at the share all the same, the only pictures in it that came out worse |
| `--iso 2.50\|2.60` | Wrap the finished disc in an `.iso` beside it: `--bdav ~/disc` writes `~/disc/BDAV` and `~/disc.iso`. The image holds the disc, `BDAV`, and nothing else in the folder. The folder stays. Refused before anything is written where that `.iso` is the image the recording is being read out of |
| `--iso-access read-only\|overwritable` | What the image says may be done to the disc it is burned onto. `read-only`, the default, is the truth about a disc nothing will write to again — a BD-R, or a BD-RE you only play. `overwritable` is what a recorder writes on a BD-RE, and what it wants to see before it will add a recording to the disc or take one off; the image is then laid out the way a recorder lays out a BD-RE, and is the size of the whole disc (25 GB, or 50 GB where one layer will not hold it — the part with nothing in it takes no room on the disk it is written to). An image that outgrows one layer and comes out the dual-layer size says so. Needs `--iso` |
| `--iso-only` | And then take the folder away, leaving the image on its own. Needs `--iso`: the image is made *of* the folder, so the folder is written first and goes once the image holds it. What goes is `BDAV`, and the folder above it only where that leaves it empty — a disc written into a folder of your own leaves everything else in it alone. A file in `BDAV` that the image could not carry keeps the folder where it is. Where a `.iso` of that name is already there and the `--bdav` folder holds no disc to add to, the run is refused before anything is cut (exit code 1), since that image would be replaced by the new disc's; move it away to keep it, or give `--bdav` another folder. Where that `.iso` is the image the recording is read out of, the run is refused as that instead, as `--iso` says |
| `--disc-title NAME` | What the disc is called. Left out, a disc that already has a name keeps it, and one with no name — a recorder's disc nobody named — stays without one; a new disc is named after the series the recording is an episode of: the programme name with the episode number, the episode's own title and the broadcast's marks taken off it |
| `--programme NAME` | What this recording is called in the disc's index. Left out, the name its playlist gave it if it came off a disc, otherwise the programme name the broadcast carries |
| `--channel NAME[,N]` | The channel, and optionally the three digits a viewer knows it by (`--channel "衛星第一,161"`). Left out, both come from the recording. A number that is not one is refused |
| `--about TEXT` | What the disc's index says the programme was. Left out, the description the recording carries |
| `--made "Y-M-D H:M:S"` | When the recording was made. Left out, when the programme went out, where the recording still says |

## Looking before you write

| Option | Meaning |
|---|---|
| `--analyze` | Work out the plan and print it. **Writes no cut** — not even into a `--bdav` folder. A `--seek-index` file, a `--proxy` or a `--preview` asked for alongside is still written. What the run would refuse before cutting it refuses as well: an `--iso` that is the image the recording is read out of, an `--iso-only` image in the way, an `-o` that is not a file on this machine (`http://…`) or a `--bdav` that is not a folder on it, an `-o` that is a recording of the run or the `--transition-image`, the `.aac` that `--audio-es` would write beside `-o` where that is a recording of the run, and — only where `-o` or `--bdav` says the run would write — a `--drop-stream` number or a `--drop-subpicture` id that names nothing. Those two are checked against the first recording, since `--analyze` does not open the `--join` ones: not at all with `--master` past the first for `--drop-stream`, or with any `--join` for `--drop-subpicture` |
| `--detect-cm` | Look for the commercial breaks |
| `--logo` | Let commercial detection use the station logo as well. Given with `--detect-cm` |
| `--inserts` | ...and report the few seconds a subscription channel drops into a programme — its own animated ident. Off by default: the same test catches a programme's own caption card. Given with `--detect-cm`. See the [commercial detection guide](cm-detection.md) |
| `--scenes` | List the scene changes |
| `--preview TIME` | Decode one picture at `TIME` and write it as a JPEG (`preview.jpg`, or `-o`). It prints the time actually decoded beside the time asked for |
| `--still TIME` | Save the frame at `TIME` at the size the recording is shown at, as the PNG, JPEG or BMP that `-o` names (`still.png` without one). An interlaced recording is saved from one field. A time past the end is an error, as for `--cut-near`. The same as **Save this frame as an image** in the cut editor |
| `--cut-near TIME` | Print where the nearest picture-to-picture change is to `TIME`, in windows of ±0.5, ±1 and ±2 seconds. A time past the end of the recording is an error (exit code 1); the length the `input` line prints is the end, and typed back it is answered. On a recorder's title, a change found at a seam is the first picture after it |
| `--verify` | Once the cut is written, decode it from end to end and line it up against the recording frame by frame: the frame count, that every copied frame is the recording's own bit for bit, the re-encoded frames' quality, decode errors and the length of the sound. Each finding is a line starting `verify:`. A file whose pictures do not match ends the run with exit code 1; sound that runs long or short, or is laid out more than half a second past the last picture, is only reported. Not with anything that writes no cut, such as `--analyze`, and refused when there is neither `-o` nor `--bdav`. It is the same check as [the GUI's](gui.md#checking-what-was-written) |
| `--log FILE` | Also write what the run prints into FILE, headed by a line giving the version and the command line. An existing file is added to. FFmpeg's lines go in as `SMARTCUT_FFMPEG_LOG` lets them. A file the run reads or writes — the recording, `-o` and so on — is refused, and so is a file inside the folders of a disc it reads or adds to (the `BDMV`, `BDAV` or `VIDEO_TS` of a disc opened by its folder, the `BDMV` or `BDAV` holding a disc clip given as the input or to `--join`, the `VIDEO_TS` of a DVD title given by its stretch as `VTS_01_1.VOB@…` the same way, and the `--bdav` target's `BDAV`). With `--iso`, the image beside the disc and the `.part` it is written under are refused too. A refused log takes away the folders the run made for it, and only those |

## Index and proxy

| Option | Meaning |
|---|---|
| `--seek-index PATH` | Where to keep the seek index. Written on the first run and read on the next, which saves walking the recording again. One that cannot be read — another version's, or a damaged one — is made again. A file that is not a seek index at all is left untouched and the run stops, so a mistyped path cannot write over a recording |
| `--index auto\|disc\|scan\|container` | Where the entry points come from. `auto`, the default, asks whoever already knows and falls back: the disc's own map, then the container's seek table, then the walk over the packets. `disc` and `container` ask one of those and stop; `scan` always walks, which is what a transport stream needs — it has neither table. A container table is taken only where it covers the whole recording. `disc`, `container` and `scan` hold for every `--join` recording too: one that cannot be read that way stops the run with an error naming it, where up to 0.8.29 it fell back on the other table or the walk |
| `--proxy` | Build the editing proxy (a light stand-in file) beside the recording and stop (`.proxy.mp4`, or `-o`) |
| `--as-proxy` | Treat the input as a proxy rather than a recording — how a proxy is checked against what it stands in for |

## A disc's subtitles

| Option | Meaning |
|---|---|
| `--subtitles pgs\|beside\|sup` | Where the subtitles a disc draws go. `pgs`, the default, puts them **inside the cut** as the kind a transport stream carries — a Blu-ray's own untouched, a DVD's converted to it. `beside` writes them **next to the cut** as the `.idx` and `.sub` pair every player and subtitle tool reads — a DVD's own untouched, a Blu-ray's read back out of its display sets. `sup` writes those display sets themselves, into a **`.sup` next to the cut** — a Blu-ray's subtitles byte for byte, and what BDSup2Sub and Subtitle Edit read. `beside` and `sup` are refused with `--bdav`: a file next to the clip does not go onto the disc |
| `--drop-subpicture ID` | Leave one of a DVD's subtitle streams out, by the id the disc knows it by: one byte in hex, `0x20` or `20`. Repeatable |

Only a `.ts` or an `.m2ts` can hold subtitles inside it. A DVD's subtitles
cut to an `.mp4` or an `.mkv` are written as the pair beside it instead, and
the run says so. A Blu-ray's are not: cut to anything but a transport stream with the
default `pgs`, they are left out, and the run says so and names the two ways to
keep them, a `.ts` or `.m2ts`, or `beside` or `sup`, which write them next to
any output. A `.sup` holds
one stream, so a recording with two of them is written as two files, named by
language (`cut_title.eng.sup`), or by number where the language does not tell
them apart. See
[a disc's subtitles](../technical/disc.md#a-discs-subtitles-inside-the-cut-or-beside-it).

Some kinds of subtitles are not carried into any output at all: SubRip, ASS,
`mov_text` or WebVTT in an `.mkv` or an `.mp4`, a DVB broadcast's subtitles, and
a DVD's subtitles remuxed into an `.mkv`. The run names each of them on a line
of its own, `not carried: a subtitle stream (its format is not one a cut
carries)`. A `--join` recording's are named as well, each under the `join  :`
line that names its recording.

## VC-1 discs

| Option | Meaning |
|---|---|
| `--vc1-quant 3..31` | How finely the join pictures of VC-1 material are written; 3 is the finest. The default is 4, which lands around 46 dB against the pictures it replaces. On material that uses overlap smoothing, anything coarser than 8 is written at 8 |

This setting exists only for VC-1 because **there is no VC-1 encoder anywhere** —
not in FFmpeg, not on a graphics card. SmartCut writes those join pictures
itself, and this is how finely. The details are in
[the Rust core](../technical/rust-core.md#vc-1-the-codec-with-no-encoder).
