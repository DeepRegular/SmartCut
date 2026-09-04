# Batch processing

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](batch.ja.md)

SmartCut is built around handling a whole evening's recordings in one sitting: drop
twenty files in, press `Ctrl+A` then `Ctrl+D`, work through them one at a time in
the editor, and write the lot out at the end.

This page describes what happens in the background while you do that, and why it
does not get in your way.

## The shape of the work

```
Add files  →  indexed in the background  →  Ctrl+D detects commercials
              →  cut each one in the editor  →  export the whole list
```

Cuts live with the clip, not with the editor window, so you can go down the list
cutting one recording after another and only then write everything out.

## Two queues, running side by side

There are two background queues: **one builds seek indexes, the other detects
commercials.** One pass of each kind runs at a time, and two of different kinds run
at once.

They are separate because they do not weigh the same. Building an index decodes
every key picture and takes all the cores. A commercial detection reads the caption
stream, or the audio and the logo, none of which libavcodec threads at all — that is
one core and a great deal of waiting for the disk. What the index pass loses by
sharing is far less than what the detection gains, and it is what gets an evening's
`Ctrl+D` finished by morning.

There is no third queue. A third pass would be a second decoder on the same cores,
and past that point the disk is the limit anyway. Three answers late is not better
than two answers early with a third behind them.

## The editor never waits for the queues

An index, a detection and an open cut editor all run at the same time. Nothing is
shared between them: indexing, detecting and exporting all reopen the recording from
the seek index on disk, so a long pass over clip 12 costs the clip you are editing
nothing.

While the editor window is up, the two background queues divide only **half the
machine** between them. The picture under your pointer is the one somebody is
waiting for, and a background pass finishing a few seconds later is a good trade for
that.

**You can open a clip the list has not read yet.** The editor makes that pass
itself, showing the recording as far as it has got, so waiting for the queue's turn
buys you nothing. While the editor has the clip, the index queue walks past that row
— reading one file twice over is the one thing worth avoiding, and it is the queue
that gives way. The index the editor writes stays on disk, so when the editor closes
and the queue takes the row up, it costs one read.

## Detecting commercials across the list

`Ctrl+A` then `Ctrl+D` queues a detection for every selected clip. Selecting
eighteen recordings and pressing `Ctrl+D` once is a night's work asked for in one
keystroke, which is what this is for. Meanwhile the clips that have no index yet
carry on being read alongside.

Progress appears on each row (`Detecting commercials 84% — Looking for the logo`),
and rows whose turn has not come say `Commercial detection queued`.

**Stop analysis** stops both queues, and pressing it again resumes them. An index
pass can stop part-way through a file. The three passes that make up a commercial
detection cannot, so a stop lands **between clips** rather than inside one.

Stopping affects what is running and nothing after it, so you can stop a batch and
immediately start a different one.

## Duplicating a clip

**⧉ Duplicate clip** puts the same recording in the list a second time. This is for
the two-hour capture holding two programmes: the same file on two rows, each written
out over a different range.

**A duplicate carries the cuts and the marks over.** The second cut is almost always
the first one moved rather than one begun from nothing, and a copy that dropped the
edit would be useless for the thing duplicates exist for. The index, the length and
whatever commercial detection found come across too — they are all the same file's
answer — so a duplicate costs no extra pass over the disk.

Output filenames gain `_1` and `_2` in list order when a recording appears in the
list more than once; without that, the second cut would land on top of the first.
Remove one copy and the survivor gets its plain name back, because the number is
counted off the list each time rather than stamped on at duplication.

## Exporting the list

The export tab writes the list out from the top down, one clip at a time. Each row
carries its own progress and result; above them are the overall state, the elapsed
time and the time remaining.

`Stop export` finishes writing the clip currently under the head and then stops, so
you never end up with a half-written file.

Because the export order is the list order, dragging a row to the top is how you say
"write this one first".

## Working over a network share

Recordings on an SMB share work as command-line arguments, as drops from a file
manager, and as an output folder. SmartCut translates the share path into wherever
this machine has already mounted it, and after that it is an ordinary path — the
packet scan, the seek index and the output all proceed without knowing a network was
involved.

**SmartCut does not mount anything itself.** Mounting is where the password lives,
and that belongs to your desktop's keyring rather than to a cut editor. A share that
is not connected is refused, with the place to open instead:

```
Not connected to \\nas\rec. Open smb://nas/rec in the file manager and add
it again. (shares connected now: \\nas\録画)
```

## Saving the batch

The whole list — recordings, cuts, track choices and output settings — saves as a
project with `Ctrl+S` and comes back next time. See [projects](projects.md).
