# Batch processing

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](batch.ja.md)

SmartCut is built around handling a whole evening's recordings in one sitting: drop
twenty files in, press `Ctrl+A` then `Ctrl+D`, work through them one at a time in
the editor, and write the lot out at the end.

This page describes what happens in the background while you do that, and why it
does not get in your way.

## The shape of the work

```
Add files  →  indexed and pictured in the background  →  Ctrl+D detects commercials
              →  cut each one in the editor  →  export the whole list
```

Cuts live with the clip, not with the editor window, so you can go down the list
cutting one recording after another and only then write everything out.

## Three lanes, running side by side

There are three background lanes: **one walks the packets and builds the seek index,
one decodes the key pictures into thumbnails and scene changes, and one detects
commercials.** One pass of each kind runs at a time, and three of different kinds
run at once.

They are separate because their costs are different in kind. The walk is
disk-bound: one core reading about a gigabyte a second, touching no decoder at all.
The thumbnails are CPU-bound: every key picture through libavcodec, around four
seconds a gigabyte. A commercial detection reads the caption stream, or the audio
and the logo, and libavcodec threads none of that: one core and a great deal of
waiting for the disk. The background loses far less by sharing than `Ctrl+D` gains,
and that is what gets an evening's detection finished by morning.

**Running the walk and the thumbnails side by side, rather than one after the other,
is why the list is quick.** In series they simply added up: the walk read a
recording end to end, and then the thumbnails read the same recording end to end
again, neither of them waiting on anything the other did. Side by side, the walk
runs ahead through the list while the thumbnails follow a clip behind, so **every
row is filled in at disk speed** and the decoding happens during reads it is not
holding up.

The thumbnails follow the walk closely on purpose: they read a recording the machine
has just pulled through, so the second read comes back from the page cache rather
than off the disk. That holds on a share too — a read that hits the cache never
reaches the network either.

There is no fourth lane. A fourth pass would be a second decoder on the same cores,
and past that point the disk is the limit anyway.

## The editor never waits for the lanes

The walk, the thumbnails, a detection and an open cut editor all run at the same
time. Nothing is shared between them: the lanes and the export alike reopen the
recording from the seek index on disk, so a long pass over clip 12 costs the clip
you are editing nothing.

While the editor window is up, the three background lanes share only **half the
machine** between them. The picture under your pointer is the one somebody is
waiting for, and a background pass finishing a few seconds later is a good trade for
that.

**You can open a clip the list has not read yet.** The editor makes that pass
itself and becomes usable as far as it has got (see [Usable the moment it
opens](gui.md#usable-the-moment-it-opens)), so waiting for the lane's turn buys you
nothing.

**Opening the editor stops nothing.** A pass already running on that clip runs on to
the end: what it has read stays read, and the row keeps the progress, the picture and
the scene marks it had rather than falling back to `Analysis queued` because a window
was opened. The editor's own read of the same file follows the lane's through the page
cache instead of going back to the disk for it. What the lanes do skip is *starting* a
fresh pass on the clip the editor has, which would only repeat work that window is
already doing -- and the index the editor writes stays on disk, so when the editor
closes and the lane picks the row up, it costs one read.

## Detecting commercials across the list

`Ctrl+A` then `Ctrl+D` queues a detection for every selected clip. Selecting
eighteen recordings and pressing `Ctrl+D` once starts a night's work with one
keystroke, which is exactly what this is for. Meanwhile the clips that have no index
yet carry on being read alongside.

Progress appears on each row (`Detecting commercials 84% — Looking for the logo`),
and rows whose turn has not come say `Commercial detection queued`.

**Stop analysis** stops all three lanes, and pressing it again resumes them. The
walk and the thumbnails can stop part-way through a file. The three passes that make
up a commercial detection cannot, so a stop lands **between clips** rather than
inside one.

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

**Rows that would be written to the same file gain `_1` and `_2` in list order.**
Duplicates are the obvious case and not the only one: two recordings of the same
programme in different folders share a name, and every recording read off a disc
is called `00001`. Without the number the second cut would land on top of the
first, and the run would report two files written with one of them gone. Remove
one and the survivor gets its plain name back, because the number is counted off
the list each time rather than stamped on at duplication.

## Exporting the list

The export tab writes the list out from the top down, one clip at a time. Each row
carries its own progress and result; above them are the overall state, the elapsed
time and the time remaining.

`Stop export` finishes writing the clip currently in progress and then stops, so you
never end up with a half-written file. **Stopping part way through a disc takes back
what was written to it**: a stopped run does not write the index, and a stream no
playlist names is one the disc does not know it has. See the
[GUI guide](gui.md#4-write).

Because the export order is the list order, dragging a row to the top is how you say
"write this one first".

## Working over a network share

Recordings on an SMB share work as command-line arguments, as drops from a file
manager, and as an output folder. SmartCut translates the share path into wherever
this machine has already mounted it, and after that it is an ordinary path — the
packet scan, the seek index and the output all proceed without knowing a network was
involved.

**SmartCut does not mount anything itself.** Mounting means handling a password,
which belongs in your desktop's keyring rather than in a cut editor. A share that is
not connected is refused, and SmartCut tells you where to connect it:

```
Not connected to \\nas\rec. Open smb://nas/rec in the file manager and add
it again. (shares connected now: \\nas\録画)
```

## Saving the batch

The whole list — recordings, cuts, track choices and output settings — saves as a
project with `Ctrl+S` and comes back next time. See [projects](projects.md).
