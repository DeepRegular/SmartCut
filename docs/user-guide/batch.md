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

**The bar is what you say to the queue as a whole.** `バッチ開始` starts it and
`すべて中止` calls off the job being written and every job behind it. Beside
them are the two things a queue is made of: `ジョブ追加`, which takes projects
saved earlier, several at a time; and `ジョブ削除`, which takes out the row you
have clicked, with the `▾` on the end of it holding the two ways of doing that
in bulk — `出力済のジョブを削除` and `すべて削除`.

**A single job is called off from its own row.** While the queue is running
each row that still has something to do carries a `中止`. On the job being
written it stops the way `出力中止` does — the recording in hand is finished
first, so nothing half-written is left behind, which for a job of one
recording means that one is written anyway — and the queue goes on to the next
job. On a job whose turn has not come, the queue passes over it. Either way it
is waiting again the next time you press `バッチ開始`: calling a job off is
about this run, and `ジョブ削除` is what takes it out for good.

The tool stays on the queue while it works, so **each job is a card rather
than a line**: a picture off its first recording, what it holds and where it
writes, and — underneath — what it is doing this second. **That line is also
the progress bar**: it fills in behind the words as the job is written, so the
sentence and how far it has got are in the same box. The elapsed time, the
percentage and an estimate of what is left sit under it. The bar under the
whole list is the queue itself.

The line under a job's name is what its project says about itself: how many
recordings it holds, whether it writes files or a disc, and the folder it
writes into. Nothing about the output format, because a smart render mostly
has none to state — what comes out is what went in, copied. **That works whether or not the
tool is open**, and while it is running: an added job lands behind the one
being written and the tool picks it up when it gets there.

**The order is the running order**, and a row is moved by dragging it: press
it, carry it to where it belongs, and a line shows the gap it will drop into.
Escape puts it back.

**A right click on a row** is where the rest of what can be done to one job
is: `先頭へ移動` / `上に移動` / `下に移動` / `末尾へ移動` for the order, and then

| | |
|---|---|
| `もう一度出力する` | Puts a job that has been written — or failed, or been called off — back in the queue as one that is waiting, in the place it already holds. Without it the only way to write a job twice is to take it out and add it again |
| `プロジェクトを開く` | Opens that job's `.scproj` in a list window of its own, for when a job needs looking at rather than running |
| `出力先フォルダーを開く` | Shows where it writes, in whatever your desktop uses to show folders |
| `ジョブ削除` | Takes the job out of the queue for good |

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
there to answer a question.

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
