# Compatibility

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](compatibility.ja.md)

What SmartCut keeps working from one version to the next, and what it does not.
This is written for 1.0. The 0.8 series changed week by week without promising
any of it, and up to 0.8.21 it did not do all of what this page says; what those
versions do differently is [at the end](#versions-up-to-0821).

Everything SmartCut leaves on disk, and everything another program can lean on,
falls into one of three kinds:

| | Kept within 1.x | Examples |
|---|---|---|
| [**Kept**](#kept) | Yes. Breaking one needs 2.0 | `.scproj`, CLI options and exit codes, the mark files read back, writing into a BDAV folder |
| [**Carried over**](#carried-over) | Yes, by migrating it; a change is in the release notes | Preferences, named output presets, the batch queue, the environment variables in [Building](building.md) |
| [**Not kept**](#not-kept) | No. SmartCut makes it again or does without | Caches, the CLI's wording, log files, the output's exact bytes |

## Version numbers

From 1.0 a version is `MAJOR.MINOR.PATCH`:

| | Goes up when | Example |
|---|---|---|
| `PATCH` | Something that was wrong is put right | A cut that dropped a caption now keeps it |
| `MINOR` | Something new can be done, or a default changes | A new output option; `.ts` output writing broadcast tables by default |
| `MAJOR` | Something in [Kept](#kept) stops working the way it did | A `.scproj` that 1.x writes and 2.0 reads differently |

The 0.8 series did not follow this: new features have gone out in patch
releases throughout, because nothing on this page had been promised yet.

A change of default is a `MINOR` change and not a `MAJOR` one even when it changes
what a run with the same options writes. What is kept is that every option still
means what it meant and still asks for what it asked for; a run that depends on
a default it never wrote down is told about the change in the release notes.

## Kept

### Project files (`.scproj`)

A `.scproj` carries its own format number in the field `smartcut`, which is
[`PROJECT_VERSION`](../../gui/src/app.js) and is not the program's version.
It has been 1 since projects were first saved.

- **Every 1.x opens every `.scproj` that any 0.8 or 1.x wrote.**
- **A file with a larger number than this build knows is refused, not opened.**
  This is already so ([Projects](../user-guide/projects.md#files-from-a-newer-version)).
  Opening it would drop what this build does not know, and saving would make that
  final.
- **Within 1.x the number stays at 1.** Something new is written as a new field.
  A build that has never heard of the field leaves it as it is and writes it back
  out when it saves.
- **A new field never changes what an old one means.** If a field has to start
  meaning something else, that is a new field, and the old one keeps being written
  for the builds that read it.
- The number goes up only with `MAJOR`, and the build that raises it still opens
  the files before it.

A field this build does not know is kept at these places, and only these:

| | |
|---|---|
| The top of the file | Beside `smartcut`, `saved`, `settings` and `clips` |
| The output settings | `settings` |
| A row | Each entry of `clips` |
| A row's edit | `edit` |
| A row's transition | `after` |

Two things are not kept, and a later version is written so that neither is needed:

- **A field inside an item of a list** -- a cut range in `cuts`, a block in
  `cmBlocks`, the answers in `detectedWith`. The editor writes those lists again
  from what it knows, so a field added to one item is gone once the row has been
  opened and saved. Something new about a cut belongs in a field of the edit, not
  in each range.
- **A value this build does not know, in a field it does.** A transition `kind`
  added later is read as no transition, and the seam window writes it back as
  none once its join is opened there and confirmed. A new kind of something is a
  new field, with the old one written as the nearest thing this build knows.

A crash recovery copy is a `.scproj` wrapped in `{recovered, project, doc}` and
follows the same rule: one written by a newer build is left where it is and not
offered ([Projects](../user-guide/projects.md#autosave-and-recovery)).

A queued job is a `.scproj` too, so a queue written by one version can be worked
through by the next.

### Mark files read back

| File | Kept |
|---|---|
| `.keyframe` | One frame number per line. Lines that are not a number are passed over, so a header another program writes is fine |
| `.trim.avs` | `Trim(a,b)` joined with `++` |
| `.cm.json` | The fields in [Design notes](design.md#saved-detections); `smartcut_cm` is its format number, now 1 |
| `.cue` | Read only. `INDEX 01` of each `TRACK` |

The same rule as `.scproj` applies to `.cm.json`: fields are added rather than
changed, and a file with a `smartcut_cm` larger than this build knows is refused.
A file with no `smartcut_cm` at all, written by hand or by another program, is
read as it always was.

### The command line

- **An option keeps its name and its meaning through 1.x.** One that is replaced
  is still accepted under the old name until 2.0, as `--no-tables` is today for
  `--tables muxer`.
- **An option that is not known is an error**, not passed over, so a script
  written for a newer version fails rather than doing something else.
- **The exit code is 0 on success and 1 on failure.** `--verify` finding
  pictures that differ is a failure. A sound length that differs is reported and
  is not. Any other code (101 is Rust's for a panic) is a bug.
- **`--version` prints one line, `smartcut X.Y.Z`**, and nothing else.
- **The window takes recordings and projects as its arguments**, and `--batch`
  for the batch tool. `--queued` is how the batch tool talks to the window it
  opens, and is not for anyone else.

What the CLI prints is [not kept](#not-kept).

### Writing into a disc folder

`--bdav` writes into a folder that already holds a BDAV disc, numbering on from
what is there rather than over it. A folder written by any earlier version, or by
a recorder, can be added to by every later one.

### The place SmartCut keeps its own files

The app's identifier is `dev.smartcut.app`, and every folder SmartCut keeps its
own files in is named after it: the cache, the recovery copies, the batch queue,
the window positions, the run logs. The webview's storage, where the preferences
are, is keyed on it as well. Changing it would leave all of that behind in one
step, so it does not change.

## Carried over

### Preferences

The preferences are held in the window's storage, one key per setting, as
`smartcut.<name>`. There is no format number; each is read on its own and checked
for its type and, where there is a list of choices, for being one of them. A value
that does not fit is replaced by the default, which is what happens when a
setting is new.

When a setting is renamed or comes to be measured in something else, the next
version reads the old key, writes the new one and removes the old. This has been
done once already: `blankBlackLevel` and `blankWhiteLevel`, which were a share of
0 to 255, became `blankBlack` and `blankWhite`, which are a percentage
(`carryLevels` in [prefs.js](../../gui/src/prefs.js)).

A preference that is removed is ignored. It is not reused for something else.

### Named output presets

Held in the same storage under `smartcut.outputPresets`, as a list of a name and
the output settings. A setting a preset does not mention is the default when it
is applied, so a preset saved before a setting existed still applies. A value of
the wrong kind is read the way a `.scproj` reads it: a number where the screen
holds text is taken as that text, and anything else is the default.

### The batch queue

`batch.json` in the app's config folder: the jobs, the state of each, and what to
do when the queue runs out. A queue left by one version is picked up by the next.
A field the version does not know is kept, on the queue and on each job, and
written back out.

### Window positions

`windows.json` in the app's config folder. A file that cannot be read means each
window opens where it would on a first start.

### Environment variables

The ones in [Building](building.md) (`SMARTCUT_PROXY`, `SMARTCUT_PROXY_WIDTH`,
`SMARTCUT_PROXY_QUALITY`, `SMARTCUT_PROXY_ENCODER`, `SMARTCUT_BYTE_SEEK`, and
`SMARTCUT_FFMPEG_LOG` and `SMARTCUT_CLEAN_JOINS` which seed the window's
preferences) are carried over like a preference. Every other `SMARTCUT_*`
variable is for debugging or the test scripts, and may go without notice.

## Not kept

### Caches

The seek index (`.scix`), the proxy, and the saved commercial, black-and-white,
silence and waveform results are each written with a version number of their
own, and that number is part of the file's name. A version that reads them
differently looks for a different name, finds nothing and makes the file again.
The old one is left until the cache's own limit or `Delete all` removes it.
Nothing in the cache is ever the only copy of anything.

| Cache | Number | Where |
|---|---|---|
| Seek index | `seek_index::VERSION` | [seek_index.rs](../../rust/crates/core/src/seek_index.rs) |
| Proxy and its picture types | `proxy::VERSION` | [proxy.rs](../../rust/crates/core/src/proxy.rs) |
| Commercial detection | `CM_VERSION` | [lib.rs](../../gui/src-tauri/src/lib.rs) |
| Waveform | `WAVE_VERSION` | same |
| Black and white | `FLAT_VERSION` | same |
| Silence | `QUIET_VERSION` | same |

A number goes up when the file would otherwise be read wrongly. Raising one costs
everyone who updates the time it takes to make that file again for every
recording in their list, so it is not raised for a change that reading the old
file would survive.

### What the command line prints

What the CLI prints is for a person. Its wording, its layout and the language of
its labels can change in any version. Nothing it writes is meant to be parsed,
`--analyze` and `--detect-cm` included. The exit code and `--version` are what
a script should look at.

### Logs

The run logs in the app's log folder and the file `--log` writes have a line at
the top naming the version, and nothing about the rest is kept.

### The output's exact bytes

The same recording cut the same way by two versions is not promised to be the
same file. What is kept is what makes it a cut:

- The part of the recording that is copied is the recording's own bytes.
- What plays is what was asked for, in the same place, with the same number of
  pictures.

What can differ between versions, and between two runs of the same one:

| | Why |
|---|---|
| A re-encoded seam | Written by the libavcodec encoder the build carries, and that changes with FFmpeg |
| An `.iso` | The UDF image is stamped with the time it was written |
| An `.mp4` or `.mkv` | libavformat writes its own version into it, and Matroska a new segment ID each time |
| Anything a fix touches | A `PATCH` that puts a cut right changes what the cut writes |

A change that alters what a cut writes, other than a seam's bytes, is in the
release notes. Before each release the CLI's output from a fixed set of
recordings is compared against the previous release's; an output that differs and
is not explained by a change in the notes is held back.

## Versions up to 0.8.21

What this page says was put in place after 0.8.21. Those versions are still in
use, and differ in these ways:

| | 0.8.21 and earlier | After |
|---|---|---|
| Fields a `.scproj` does not know | Dropped on load and gone from the file at the next save, except inside a row's edit until it is opened. A file from a later version opened and saved there loses them | Kept at the five places listed above and written back out |
| `smartcut_cm` in `.cm.json` | Written as 1 and never read | A larger number is refused |
| Fields `batch.json` does not know | Passed over when read and gone at the next write | Kept and written back out |
| A number where a preset's setting is text | The default, where a `.scproj` turned it into text | Turned into text |
| `--version` | None. The version is only in the first line of a `--log` file | `smartcut --version` prints `smartcut X.Y.Z` and exits with 0 |
