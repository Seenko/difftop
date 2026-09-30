# Architecture

This document describes how difftop is put together: what each module does,
how data moves through it, and why some things are done the way they are.
Read this before making a non-trivial change.

## The big picture

```text
            watcher thread                                 main thread
 ┌──────────────────────────────────────────────┐   ┌──────────────────────────┐
 │ inotify ─► watch::run ──Change──► Renderer ──┼──►│ ui::run: Msg → Feed      │
 │            (batch, filter,       (diff +     │   │ tick ~30×/s, draw, keys  │
 │             cache, compare)       highlight) │   │                          │
 └──────────────────────────────────────────────┘   └──────────────────────────┘
                      mpsc::channel<ui::Msg> ──────────────►
```

There are two threads and no async runtime.

- **The watcher thread** does everything slow: reading files, diffing, and
  syntax highlighting. It sends the screen finished, colored lines.
- **The main thread** only animates and draws, so the screen stays smooth
  even while a huge file is being highlighted.

## Modules

All code lives in `src/`. Each module has a doc comment at the top saying what
it is for. Start there.

| Module         | Job                                                                 | Knows about terminals? |
| -------------- | ------------------------------------------------------------------- | ---------------------- |
| `main.rs`      | Parses flags, wires the two threads together.                       | yes                    |
| `shell.rs`     | Runs prompt lines in bash, streams output, reports the final cwd.   | no                     |
| `command.rs`   | Tab completion for the prompt.                                      | no                     |
| `cli.rs`       | Every command-line flag. `--help` text comes from its doc comments. | no                     |
| `watch.rs`     | Watcher thread: inotify, batching, file cache, emits `Change`s.     | no                     |
| `filter.rs`    | Which paths are ignored (`.gitignore`, hidden, `.git`).             | no                     |
| `diff.rs`      | Old + new text → hunks of `DiffLine`s.                              | no                     |
| `highlight.rs` | Text → colored spans, using bat's syntax definitions.               | spans only             |
| `theme.rs`     | Finding/loading themes (bundled, `.tmTheme`, base16), `UiColors`.   | colors only            |
| `render.rs`    | A `Change` → the lines shown for it (header, gutter, cutoff).       | yes                    |
| `reload.rs`    | Notices a replaced binary, saves the screen, `exec`s the new one.   | no                     |
| `feed.rs`      | The animation: queue, pacing, scrolling, what's visible. Pure.      | no                     |
| `ui.rs`        | Terminal setup, frame loop, drawing, keys, mouse, toast.            | yes                    |

## How a change travels

1. **inotify fires.** Every watched folder has its own non-recursive watch
   (`watch::State::scan`), and ignored folders never get one. Watching
   recursively would put watches on `target/` and `node_modules/` too, and
   run into the kernel's watch limit on large trees.
2. **Batching** (`watch::next_batch`). Events are collected until the folder
   has been quiet for 50 ms, or 250 ms have passed. Editors and agents
   often write a file in several steps; batching turns those into one
   change. `Access` events are dropped, because reading a file ourselves
   would otherwise trigger a new event and loop.
3. **Compare** (`watch::State::update`). The cache holds a `Snapshot` of
   every file: its size, modification time, and its text if it's small
   UTF-8 text. Text files are compared by content. Everything else is
   compared by size and modification time. The result is a `Created`,
   `Modified` or `Deleted` change, carrying the snapshots from before and
   after.
4. **Render** (`render::Renderer`). If both sides have text, `diff::hunks`
   finds the changed lines, `highlight` colors the new file from the top
   down to the last line needed, and each line gets a `+`, `-` or blank
   gutter. If they don't (binary files, files over 1 MiB, text that wasn't
   in memory yet), the header shows the size change instead, e.g.
   `12.0 KiB → 14.1 KiB  +2.10 KiB`. Nothing that changes goes unshown.
   Every change gets a header and a blank spacer after it.
5. **Animate** (`feed::Feed`). The lines join a queue. On every frame,
   `tick` moves some of them onto the screen. The rate is `speed` plus half
   of the queue length per second, so a small change glides in and a burst
   of 5,000 lines still drains within a few seconds.
6. **Draw** (`ui::App::draw`). The newest lines are pinned to the bottom of
   the screen, and the top third is dimmed so older lines fade out.

**Scrolling.** `Feed` keeps the last 10,000 lines and an `offset`, which is
the number of lines below the view. An offset of 0 means following the
newest line. While the offset is above 0, every released line increases it
by one, so the view holds still as new lines arrive underneath. `ui`
dims nothing while scrolled, and it shows a toast with the number of
newer lines. The toast's position is remembered so a click on it can
resume following.

**The shell prompt.** `shell::run` starts the typed line in `bash -c`,
wrapped so that afterwards the shell writes its working directory to a
private file (`pwd -P > $DIFFTOP_PWD_FILE`). That's how `cd` is followed
wherever it appears in the line. stdout and stderr share one pipe, so
errors land in order. A thread reads the pipe and sends each line to the
frame loop as `Msg::Shell`. `Feed::show_now` puts them on screen right away
instead of queueing them behind the animation. The command gets its own
process group, so `Ctrl-C` (`Running::interrupt`) reaches everything it
started. When the command ends with a different working directory, `ui`
calls `App::cd`, which is the switch described next.

**Switching folders.** `watch::spawn` returns a `Handle`. `Handle::watch`
sends new roots over the same channel the file events use, so the watcher
thread only ever waits in one place. On a switch, the thread drops its
whole `State`, which removes every old watch, and starts over with a fresh
scan. Events from the old watches that are still queued fall outside the
new roots, so the filter drops them. `App::cd` also changes the process's
working directory, so the next command, `cd -` (passed as `OLDPWD`) and
the file names on screen all follow it, like in a shell.

**Hot reload.** `reload::Upgrade` checks the running binary's file once a
second. A new build shows up as a new inode, size or modification time.
Once the file has stayed the same for one more check, so it isn't still
being copied, and `--version` runs successfully, `ui` saves the feed, scroll
position, speed, pause state and watched roots (`reload::Saved`) to a 0600
file. It then `exec`s the new binary with the same arguments and
`DIFFTOP_RESUME` pointing at that file. The terminal isn't restored first,
so the screen never flashes back to the shell. The new process reads and
deletes the file in `main`, watches the saved roots instead of its
arguments, and restores the feed. File contents in the watcher's cache
aren't carried over; the new process rescans.
`Saved` uses `#[serde(default)]`, so a version that adds fields can still
read an older save. Keep new fields defaultable.

**Input.** `ui::start` turns on mouse reporting, so the wheel can scroll,
and bracketed paste. Many terminals paste on right-click; without
bracketed paste, the pasted text would arrive as keypresses and could
pause or clear the screen. `ui::stop` undoes both, and a panic hook calls
it too, so a crash never leaves the terminal in mouse mode.

## Decisions and why

- **Syntax is highlighted from the top of the file, not from the hunk.**
  Starting in the middle of a file gets multi-line strings and comments
  wrong. We stop at the last line we need, so the cost is only high when a
  change is near the end of a big file.
- **Every non-ignored file is shown when it changes, even if it can't be
  diffed.** Hiding binary or large files would make the display lie about
  what's happening. Ignore rules (`.gitignore`, hidden files, `.git`) are
  the only thing that keeps a file off the screen.
- **Removed lines are not syntax-highlighted.** They're shown dimmed in the
  "removed" color. This is easier to read at a glance and means only one
  version of the file ever needs highlighting.
- **The `ansi` theme is the default**, because its colors are terminal
  palette indexes rather than RGB values. The terminal draws them with
  whatever palette it currently has, so difftop matches the user's shell
  theme and follows it live without any code for it. bat encodes palette
  indexes in the alpha channel of a syntect color; `theme::to_color`
  explains the details.
- **base16 schemes are parsed by hand** (`theme::from_base16`). Scheme files
  are flat `key: value` lists, so a small parser covers all 561 base16 and
  base24 schemes in tinted-theming without adding a YAML dependency.
- **`Feed` is generic over its line type**, so the animation logic can be
  tested with plain integers and no terminal.
- **Interface colors come from the theme's own scopes**
  (`markup.inserted`, `markup.deleted`, `markup.heading`, ...). Where a
  theme leaves them undefined or makes them clash, the terminal palette is
  used instead. A test checks every bundled theme for clashes.

## Known limits

Known shortcuts are marked in the code with a `ponytail:` comment. Each one
says where the limit is and how it would be lifted. To list them:

```bash
grep -rn "ponytail:" src
```

## Testing

- Every module has unit tests next to its code, in `#[cfg(test)] mod tests`.
- `watch::tests` runs the real watcher on a temp folder, so inotify is
  covered end to end.
- The TUI itself isn't unit-tested. To check it by eye, run it in tmux.
  [AGENTS.md](AGENTS.md#seeing-the-tui) shows how.
