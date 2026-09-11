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

You can also open a saved file by dropping it on the window, or by passing it on
the command line.

```bash
smartcut friday.scproj
```

## What is saved, and what is not

A project saves **the things you decided**:

- where each recording is (and the order you put them in)
- the cuts and the keyframes (marks) you made in each one
- which tracks you chose to write
- the programme names and chapters read from a disc, and what a disc written
  from the list will say about each recording — the name, the channel, when it
  was recorded and what it was about, including anything typed over
- the output settings

It does not save **anything it can work out again**. Length, resolution and
frame rate come straight from the file next time it is opened. The seek index
and the commercial-detection results live in the program's own cache folder, and
are read back from there.

That keeps the file tiny: **a few hundred bytes even for a list of twenty**.
It is only text, so it opens on another machine, and it opens after you have
cleared the cache. Whatever is missing simply gets read again.

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
