# Working through a batch

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](batch.ja.md)

SmartCut is not built around opening one recording at a time. It is built for
**dropping in a whole evening's recordings and working through them**.

```
①  drop the recordings in together
②  Ctrl+A, then Ctrl+D, to run commercial detection on all of them
③  double-click them one at a time and cut
④  export the whole list at the end
```

Cuts belong to **the row in the list**, not to the editor window. So you can cut
your way down the list and leave the exporting until last.

## 1. Preparation starts the moment you drop them

![The list while recordings are loading](../images/usage-loading.png)

The rows fill in immediately. Behind them, SmartCut is doing three jobs at once.

| Background job | What it does | How long it takes |
|---|---|---|
| **Loading** | builds the seek index, used for seeking and cutting | about 1 second per GB |
| **Thumbnails** | makes the filmstrip pictures and finds scene changes | about 4 seconds per GB |
| **Commercial detection** | looks for the commercial breaks (started by `Ctrl+D`) | 10–60 seconds for a 30-minute recording |

Two jobs of the same kind run one after another; jobs of different kinds run at
the same time. Progress appears at the bottom right of each row. Rows whose turn
has not come say `Queued`.

**The index is built once and kept.** Open the same recording again and the row
says `Index from an earlier run`, skipping the loading pass entirely.

**You do not have to wait.** A recording that has not finished loading still
opens on a double-click. What is available immediately and what arrives later is
in [Using the GUI](gui.md#usable-from-the-moment-it-opens).

## 2. Detect commercials across the whole list

![The list during commercial detection](../images/usage-detect.png)

`Ctrl+A` (select all), then `Ctrl+D` (detect commercials). That queues detection
for every selected recording. **Eighteen recordings, one keystroke** — that is
what the feature is for. Press it in the evening, look at the results in the
morning.

Progress appears on the row: `Detecting commercials 84% — Looking for the logo`.
Rows still queued say `Commercial detection queued`.

**Detection only places marks; it does not cut.** You decide what to remove,
later, in the editor. See [Commercial detection](cm-detection.md).

**To stop, press "Stop analysis".** Press it again to resume. Loading and
thumbnails can stop in the middle of a file, but commercial detection cannot, so
it stops **between** recordings.

Only what is running stops. You can stop a batch and immediately start detection
on something else.

## 3. Edit without waiting for the background

Loading, thumbnails, commercial detection and cutting all run at the same time.
**A long job on the twelfth recording does not affect the one you have open.**

While the editor window is open, the three background jobs share **half** the
machine. The picture you are looking at comes first.

**Opening the editor does not stop the background.** Work already started on
that clip runs to the end. Its progress, its pictures and its scene marks never
go backwards because you opened a window.

## 4. Splitting one recording across two rows

**⧉ Duplicate clip** adds the same recording to the list a second time. It is
there for the two-hour recording that contains two programmes: put the same file
on two rows and keep a different part in each.

**A duplicate keeps the cuts and the marks.** The second programme's cuts are
usually the first one's, moved. The index and the detection results come along
too, so nothing is read from disk again.

**Rows that would write the same filename get `_1`, `_2` in list order.** That
happens with duplicates, but also with same-named programmes from different
folders, and with recordings read off a disc (they are all called `00001`).
Without the numbers the second one would overwrite the first, and the program
would report two exports while leaving one file. Delete one of them and the
other goes back to its unnumbered name.

## 5. Export the list

The output tab writes the list **from the top down**. Each row shows its
progress and result, with the overall state, elapsed time and time remaining
above.

To export something first, **drag its row upwards**. The export order is the
list order.

`Stop export` finishes the recording it is on and then stops. It never leaves a
half-written file behind. (If you stop it while it is writing a BDAV disc, what
it has written is taken back off the disc as well — the reason is in
[Using the GUI](gui.md#4-export).)

## Recordings on a network share (NAS)

Recordings on a NAS can be dropped in like any other, and a share can be used as
the output folder.

But **SmartCut does not mount shares.** Mounting needs a password, and that
belongs to the file manager or the keyring, not to a video editor. Given a share
that is not connected, it stops and says where to connect it.

```
\\nas\rec is not connected. Open smb://nas/rec in your file manager and add
the files again. (Connected shares: \\nas\video)
```

## Saving part-way through

The whole list — recordings, cuts, track choices, output settings — saves with
`Ctrl+S` and comes back next time. See [Projects](projects.md).

---

Why the three jobs run in parallel, and how that was tuned, is in the
[design notes](../technical/design.md#three-lanes-and-an-editor-that-stays-open).
