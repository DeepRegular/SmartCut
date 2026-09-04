# Projects

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](projects.ja.md)

An evening's work is a list of recordings, what you have cut out of each one, and
where the results are supposed to go. A project file (`.scproj`) holds all of that
so you can come back to it.

Without projects, closing the program threw the work away. For one recording in one
sitting that costs nothing. For twenty recordings over a weekend it costs a great
deal.

## Saving and opening

| | |
|---|---|
| `Ctrl+S` | Save the project |
| `Ctrl+Shift+S` | Save as |
| `Ctrl+O` | Open a project |

The same three items are in the **SmartCut** menu in the corner of the list window,
alongside Preferences and About — that menu is where the things that concern the
program rather than one clip already live.

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
  chapters — so that reopening the list does not need the disc back in the drive
- the output settings

Everything else is left out. A recording's length, shape and frame rate arrive with
its seek index. The seek index and the commercial detections are kept in the
program's own cache directory — not next to the recording, which is usually on a
share that other things read.

So the file holds paths, edits and settings, and nothing else. **A list of twenty
recordings comes to a few hundred bytes**, and opening it re-reads the list exactly
as adding the same files would.

That is the point, rather than a saving. A project opened **on another machine**,
or **after the caches have been cleared**, is a project that still opens. It simply
reads everything again.

## The `*` in the title bar

The title bar names the project that is open, and puts a `*` in front of it while
there is work that has not been written to disk. Closing the window in that state
asks you first.

**Whether there is unsaved work is worked out by comparing, not by remembering.**
SmartCut builds a description of what the project would be if it were written this
instant, and compares it against what was last written or read.

The alternative — raising a flag whenever anything changes — has to be lowered
again by everything that puts the work back where it was: cancelling out of the
editor, adding a clip and then removing it. Miss one of those places and the
program insists there is something to lose when there is not, which is the state in
which people stop reading the question.

Two things are deliberately outside the comparison: the saved timestamp, which
changes every time and says nothing about the work; and the flag recording whether
a detection's marks are still owed to a timeline, which is written to the file but
cannot be lost, since the detection it stands for is in the cache.

There are also two exemptions at the edges:

- **An empty list with no project open is nothing to lose**, whatever it held a
  moment ago. Without this, emptying a list left a `*` that nothing could clear:
  there was nothing to save, so saving could not clear it.
- **A list handed over on the command line is not work anybody did.** Starting the
  program the same way again gives the same list, so that list becomes what the
  title compares against. A program launched on a folder should not open with a `*`
  over a list nobody has touched.

## When something has moved

A recording named in the file that has since moved is not a reason to refuse the
whole project. That row goes up like any other, and the index pass reports what
happened to it, in the row, next to the nineteen that were fine.

## Versions

The format carries its own version number, and a file from the future is refused
rather than read. What a newer file would lose on the way in is exactly the part
this program does not recognise, and quietly dropping somebody's cuts is worse than
not opening at all.

A field being *added* is not a new version — a reader that has never heard of a
field leaves it alone.

Settings are copied key by key rather than wholesale, so a file cannot put anything
into the output settings that the screen has no control for. A drop-down handed a
value it has no option for falls back to its first entry: what is on screen and what
will be written have to be the same answer.
