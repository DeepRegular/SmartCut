# Projects (saving your work)

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](projects.ja.md)

You line up twenty recordings and start cutting the commercials out of them one
by one. Then something else comes up. Closing the window does not throw the
work away.

**A project file (`.scproj`) saves the whole list.** Which recordings you added,
where you cut each one, and where the results should go. Open it next time and
you are back where you left off.

## Saving and opening

![The SmartCut menu](../images/usage-menu.png)

The **SmartCut** button in the top right of the list window has four items.
The keyboard does the same things.

| Key | Button | What it does |
|---|---|---|
| `Ctrl+S` | Save project | Saves over the file you opened |
| `Ctrl+Shift+S` | Save as | Saves under a new name |
| `Ctrl+O` | Open project | Loads one you saved earlier |
| `Ctrl+N` | New | Empties the list and starts over |

**New** clears the list and puts the output settings back to their defaults —
exactly the state you get when you start the program. If you have unsaved work,
it asks first.

At the foot of the menu is **Quit**, which is the window's own cross by another
route: unsaved work stops it the same way. The batch tool's menu has it too.

You can also open a saved file by dropping it on the window, or by passing it on
the command line.

```bash
smartcut friday.scproj
```

## What is saved, and what is not

A project saves **the things you decided**:

- where each recording is (and the order you put them in)
- any clip you renamed
- the cuts and the keyframes (marks) you made in each one
- for a recording you have had open in the cut editor, what the detections
  found there: the commercial blocks, and the black, white and silent
  stretches. The bands under the timeline come back with the project
- which tracks you chose to write
- whether the list is written as one file, which clip is the master, and
  what happens at each join between two clips
- the programme names and chapters read from a disc, and what a disc written
  from the list will say about each recording — the name, the channel, when it
  was recorded and what it was about, including anything typed over
- the output settings, once you have settled any of them

It does not save **anything it can work out again**. Length, resolution and
frame rate come straight from the file next time it is opened. The seek index
and the thumbnails live in the program's own cache folder, and are read back
from there.

A detection is kept in that cache too, but only on the machine it was made on.
For a recording you have had open in the cut editor, the stretches it found are
in the project as well — so the bands are there again after you clear the cache,
and on another machine.

That keeps the file small: **a few hundred bytes for a list of twenty with cuts
and marks alone, a few kilobytes with the detections in it**. It is only text,
so it opens on another machine, and it opens after you have cleared the cache.
Whatever is missing simply gets read again.

### The output settings go in once you have settled them

A list saved from the Input screen has not been given an output yet. What the
program is holding at that moment is its own defaults, whatever Preferences says
a cut is called, and whatever the last session was carrying — none of it an
answer you gave about this work. So none of it is written down, and opening
that project later asks those standing answers again. Which is what you want
when you have since changed them.

Use one control on the Output settings screen and the whole panel becomes this
project's own answer: saved with it, and put back exactly as it was when it
is opened. Merely walking onto that screen is not using it. The folder name
and the disc title it fills in for you are worked out from the recordings,
and are worked out again next time.

A batch job is the exception. `Add to batch` writes the output settings into
the queue's copy whether or not you have been to that screen, because a job is
to be written the way this window would write it now — not the way another
process would work it out hours later.

## The `*` in the title bar means "not saved yet"

The title bar shows the name of the project you have open. While there are
changes you have not saved, a `*` appears in front of the name. Close the
window in that state and it asks whether to save first.

The `*` is decided by **comparing the current list against the last saved
version**, every time something changes — not by "you touched something, so
flag it". So if you make a cut and immediately undo it, the `*` goes away
again.

There are two exceptions, both cases where a `*` would be wrong:

- **An empty list with no project open.** There is nothing to lose, so no `*`.
- **A list you passed on the command line.** You did not build that list; the
  same command would produce it again. So until you touch it, there is no `*`.

## When a recording has moved

A recording named in the project may have been moved to another folder, or
deleted. The **project still opens normally.** Only the row for that recording
says what happened. The other nineteen work as usual.

## A project with recordings on a share

A project that names recordings in a shared folder — `\\nas\rec\…` or
`smb://nas/rec/…` — asks before it opens, if it names a computer SmartCut has
not been used with yet.

An open project starts reading every row at once, and on Windows reading a file
on a share sends your sign-in details to the computer it is on. A project from
someone else that names a computer you do not know would hand them over just by
being opened. The question is there to stop that.

| When | Asked again |
|---|---|
| Recordings were added to the list from that share | No |
| You answered Yes | No |
| You answered No | The project does not open, and is asked about next time too |

The batch tool asks the same when it starts a job. A folder mapped as a
network drive (`Z:\` and so on) is not asked about.

## A saved project is also a batch job

`Add to batch` on the Export screen saves the list, puts it at the end of the
queue and opens the batch tool over it, with no picker in the way. The tool —
a window in a process of its own — writes the queue out one job after another
overnight.

What is queued is a copy. The queue writes the list as it stands into a folder
of its own and runs that, under the name of the project you have open or of the
list's first row where there is none. The copy is the queue's, and goes when the
job is taken out of it; to change it, open the job from its row in the tool with
`Open the project`.

**Registering is also saving.** The project you have open is written as well,
the way `Ctrl+S` writes it, and an untitled list counts as saved by the copy —
either way the `*` comes off the title bar. Note what that means for an
untitled one: taking the job out of the queue takes the copy with it, so save
it yourself with `Save project as…` if you will want it again. A job is a `.scproj` and nothing
besides, because the file already holds everything a job is. See
[Working through a batch](batch.md#an-overnight-queue-of-projects).

## Files from a newer version

A `.scproj` carries a version number. **A file saved by a newer version than
the one you are running is refused rather than opened.**

Reading it anyway would silently drop everything this version does not
recognise, and saving over it would make that loss permanent. Refusing to open
is better than quietly throwing away someone's cuts.

Note that **adding** fields does not count as a new version. Fields it does not
recognise are kept as they are and written back out.

---

The file format itself, and how the `*` comparison is implemented, are in the
[design notes](../technical/design.md#projects-saved-or-not-worked-out-rather-than-remembered).
