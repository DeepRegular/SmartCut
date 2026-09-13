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

## An overnight queue of projects

One list is one evening's work. The **batch tool** is the other axis: a queue
of saved projects, written out one after another with nobody in the room.

A job is a project file and nothing else. A `.scproj` already holds the
recordings, the cuts, the track choices and the output settings, so the queue
only has to say which files and in what order.

```
①  cut an evening's recordings and save the project (Ctrl+S)
②  出力 tab → バッチに登録
③  do the same for the next evening's work
④  menu → バッチ出力ツール…, press バッチ開始 in the window that opens, and go to bed
```

### The tool is a window of its own

`バッチ出力ツール…` on the SmartCut menu opens **a second window, in a process
of its own**. That is the point of it: closing the main window — or quitting
SmartCut entirely — does not stop a queue that is being written.

The queue lives there and nowhere else. The tool shows it, orders it, runs it
and stops it; the main window's only part in it is `バッチに登録`, which puts
the list on screen at the end of the queue.

**One button runs the whole thing**, at the left of the tool's own bar: it says
`バッチ開始` while the queue is waiting and `バッチ中止` in red while it is being
written. The tool has no tabs — it shows the queue while it is idle and what is
being written while it is not, and moves between the two itself — so that
button is in reach whichever of them is up. Beside it are the two things a
queue is made of: `ジョブ追加…`, which takes projects saved earlier, several at
a time; and `ジョブ削除`, which takes out the row you have clicked, with the
`▾` on the end of it holding the two ways of doing that in bulk —
`出力済のジョブを削除` and `すべて削除`. **That works whether or not the
tool is open**, and while it is running: an added job lands behind the one
being written and the tool picks it up when it gets there.

`上へ` and `下へ` under the bar reorder the queue.

**The queue survives the program.** It is written to a file as it is changed,
so a queue lined up at midnight is still there in the morning — and a job that
has been written stays in the list with what it wrote, and is not written
again. `出力済のジョブを削除` clears out that half of it in one go, leaving
exactly the jobs still to do.

Each job runs exactly as it would by hand: the project is opened, the list is
read, and the 出力 screen writes it — the disc pass, the image and the sidecars
included.

**A job that fails does not stop the queue.** It is marked in red with what went
wrong and the next job starts: a night left to run is left because nobody is
there to answer a question. `バッチ中止` stops both the job under the head and
the queue behind it; the recording being written is finished first, so nothing
half-written is left behind, and the jobs it did not reach go back to waiting.

### Sleeping or shutting down at the end

`完了後` takes `何もしない`, `スリープ` or `シャットダウン`, and is remembered
with the queue. It fires once the queue has run to the end — including a queue
that ended with failures, which are still on the screen when the machine comes
back — and never over a queue somebody stopped.

Before anything happens there is **a minute's countdown with a 中止 button**
next to it. A machine that turns itself off is a machine that should say so
first.

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
