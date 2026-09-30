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
| `cli.rs`       | Every command-line flag. `--help` text comes from its doc comments. | no                     |
| `watch.rs`     | inotify watches, event batching, file cache, emits `Change`s.       | no                     |
| `filter.rs`    | Which paths are ignored (`.gitignore`, hidden, `.git`).             | no                     |
| `diff.rs`      | Old + new text → hunks of `DiffLine`s.                              | no                     |
| `highlight.rs` | Text → colored spans, using bat's syntax definitions.               | spans only             |
| `theme.rs`     | Finding/loading themes (bundled, `.tmTheme`, base16), `UiColors`.   | colors only            |
| `render.rs`    | A `Change` → the lines shown for it (header, gutter, cutoff).       | yes                    |
| `feed.rs`      | The animation: queue, pacing, what's visible. Generic, pure.        | no                     |
| `ui.rs`        | Frame loop, drawing, fade, footer, keys.                            | yes                    |

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
3. **Compare** (`watch::State::update`). The file is read again and compared
   with the copy in the cache. The result is one of four `ChangeKind`s:
   - `Created`: the file is new.
   - `Modified`: the file changed since the cached copy.
   - `Deleted`: the file is gone.
   - `FirstSeen`: the file changed, but there was no cached copy to diff
     against.
4. **Render** (`render::Renderer`). `diff::hunks` finds the changed lines,
   `highlight` colors the new file from the top down to the last line needed,
   and each line gets a `+`, `-` or blank gutter. Every change gets a header
   and a blank spacer after it.
5. **Animate** (`feed::Feed`). The lines join a queue. On every frame,
   `tick` moves some of them onto the screen. The rate is `speed` plus half
   of the queue length per second, so a small change glides in and a burst
   of 5,000 lines still drains within a few seconds.
6. **Draw** (`ui::App::draw`). The newest lines are pinned to the bottom of
   the screen, and the top third is dimmed so older lines fade out.

## Decisions and why

- **Syntax is highlighted from the top of the file, not from the hunk.**
  Starting in the middle of a file gets multi-line strings and comments
  wrong. We stop at the last line we need, so the cost is only high when a
  change is near the end of a big file.
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
