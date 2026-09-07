# Projects

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](projects.ja.md)

An evening's work is a list of recordings, what you have cut out of each one, and
where the results should go. A project file (`.scproj`) holds all of that so you
can come back to it later.

Before projects existed, closing the program threw the work away. For one recording
in one sitting that costs nothing; for twenty recordings over a weekend it costs a
great deal.

## Saving and opening

| | |
|---|---|
| `Ctrl+N` | New project |
| `Ctrl+S` | Save the project |
| `Ctrl+Shift+S` | Save as |
| `Ctrl+O` | Open a project |

The same four items are in the **SmartCut** menu in the corner of the list window,
alongside Preferences and About. That menu is where everything concerning the
program as a whole, rather than one clip, already lives.

**New project** empties the list, puts the output settings back where they started,
and forgets the project file the work was in — the state the program opens in. If
there is work you have not saved, it asks first.

A `.scproj` also opens if you drop it on the window, or pass it on the command
line:

```bash
smartcut friday-night.scproj
```

## What is in the file

A project holds only what could not be worked out again:

- the path of each recording, in list order
- the cuts and the keyframes you placed in each one
- which tracks you chose to write
- what a disc's index said about a recording on it — its programme name and its
  chapters — so that reopening the list does not require the disc to be back in the
  drive
- the output settings

Everything else is left out. A recording's length, shape and frame rate arrive with
its seek index. The seek index and the commercial detections live in the program's
own cache directory, rather than next to the recording — recordings usually sit on
a share that other programs read.

So the file holds paths, edits and settings, and nothing else. **A list of twenty
recordings comes to a few hundred bytes**, and opening it re-reads the list exactly
as adding the same files would.

The point is not the disk space. It is that a project still opens **on another
machine**, or **after the caches have been cleared**: it simply reads everything
again.

## The `*` in the title bar

The title bar names the project that is open, and puts a `*` in front of it while
there is work that has not been written to disk. Closing the window in that state
asks you first.

**Whether there is unsaved work is worked out by comparing, not by remembering.**
SmartCut builds a description of what the project would be if it were written this
instant, and compares it against what was last written or read.

The alternative — raising a flag whenever anything changes — means that every
action which puts the work back as it was has to lower the flag again: cancelling
out of the editor, adding a clip and then removing it, and so on. Miss one of those
places and the program insists there is something to lose when there is not, and
that is when people stop reading the dialog.

Two things are deliberately outside the comparison. One is the saved timestamp,
which changes every time and says nothing about the work. The other is the flag
that records whether a detection's marks have yet to be applied to a timeline; it
is written to the file, but it cannot be lost, because the detection it stands for
is in the cache.

There are also two exemptions at the edges:

- **An empty list with no project open counts as nothing to lose**, whatever it
  held a moment ago. Without this, emptying a list left a `*` that nothing could
  clear: there was nothing to save, so saving could not clear it.
- **A list handed over on the command line is not work anybody did.** Starting the
  program the same way again gives the same list, so that list becomes what the
  title compares against. A program launched on a folder should not open with a `*`
  over a list nobody has touched.

## When something has moved

A recording named in the file that has since moved is no reason to refuse the whole
project. That row appears like any other, and the indexing pass reports on the row
itself what happened to it, next to the nineteen that were fine.

## Versions

The format carries its own version number, and a file from the future is refused
rather than read. Whatever such a file would lose on the way in is precisely the
part this program does not understand, and quietly dropping somebody's cuts is
worse than not opening the file at all.

*Adding* a field is not a new version: a reader that has never heard of a field
leaves it alone.

Settings are copied key by key rather than wholesale, so a file cannot put anything
into the output settings that the screen has no control for. A drop-down handed a
value it has no option for falls back to its first entry, because what is on screen
and what will be written have to agree.
