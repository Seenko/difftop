# Changelog

All notable changes to difftop are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Binary files get a bar under their header that shows where in the file
  the bytes changed, where it grew and where it was cut off, with offsets.
  It works for files of any size in constant memory.
- A built-in `matrix` theme: green CRT phosphor on near-black, with a random
  flicker. Pick it with `--theme matrix`.
- Diffs show line numbers, and added and removed lines get a tinted row
  background that keeps their syntax colors. Removed lines are now syntax
  highlighted too. Turn these off with `--no-line-numbers` and
  `--no-change-background`.

### Changed

- Unchanged lines around a change are no longer dimmed. On dark terminal
  palettes they were nearly unreadable, and the row background now marks
  what changed.
- Change headers read like Claude Code's: `Updated src/main.rs (+5 −4)`,
  `Created logo.png (300 KiB)`, `Deleted notes.txt (2 B)`, with the numbers
  in grey. The path uses the normal text color instead of the accent
  color, and everything under a header is indented.
- Frames are sent as synchronized updates, so terminals that support it
  never show a half-drawn frame while the screen scrolls. Frames are timed
  on a steady beat, and an idle screen sends nothing to the terminal.
- Palette themes (the default `ansi`) use the same dark green and red row
  backgrounds as Claude Code (#005f00, #5f0000).

### Fixed

- Quitting difftop after it had reloaded itself (a new build replaced its
  binary) no longer leaves the terminal in raw mode, which showed up as no
  echo and broken line breaks until `reset`.
- difftop no longer crashes on text files with lone carriage returns, such
  as logs with progress bars. Those `\r` are now shown as `␍` instead of
  disappearing.
- The built-in shell no longer hangs when the shell's config prints something
  without a final newline, such as fastfetch.
- Text files saved as UTF-16 (Visual Studio's "Unicode") or in a legacy
  code page like Windows-1252 are diffed again instead of shown as binary.

## [0.2.0] - 2026-09-29

### Added

- A built-in shell: press `:` (or `!`) for your own prompt (PS1, starship,
  ...) and run any command. It runs in your shell with your aliases on a
  pseudo-terminal, so colors and columns match your terminal, and its
  output appears in its own box, with diffs flowing around it. When a
  command changes directory, difftop follows and watches the new folder.
  History with `↑`/`↓`, Tab completion, paste, `Ctrl-C` to stop a command.
  A shell is kept warm, so commands start in ~50 ms even with a heavy
  `.bashrc`.
- Hot reload: replacing the difftop binary makes every running difftop
  switch to the new version within seconds, keeping history, scroll
  position, speed and the watched folder.
- Scroll back through history (arrow keys, PgUp/PgDn, Home, mouse wheel)
  while changes keep arriving. A toast shows how many newer lines there
  are; press `End` or click it to follow again.
- Scrolling the mouse wheel over the speed display (`30/s`) changes the speed.
- `Ctrl-L` redraws the screen; `Esc` `Esc` quits (the first press asks).
- `install.sh`: install the latest release with one `curl ... | sh`.

### Changed

- One status line at the bottom: what's watched, the `:` and `c` keys, and
  the file count and speed.
- Brightness means one thing: changed lines (added and removed) are bright,
  unchanged context is dim. The fade at the top of the screen is gone.
- Much lower memory use: file text kept for diffing is capped at 16 MiB,
  most recently modified files first (watching a home folder went from
  ~290 MB to ~50 MB; a project stays around 10 MB).
- Removed pause (space) and `q`; quit with `Ctrl-C` like any other program.
- Line wrapping is turned off while difftop runs, so drawing the screen's
  last cell can't make terminals like Windows Terminal scroll.

### Fixed

- Right-clicking (which pastes in many terminals) could clear the screen,
  because pasted text was read as keypresses.

## [0.1.0] - 2026-09-29

### Added

- Watch one or more folders recursively and show each change as a
  syntax-highlighted diff that rises up the screen, with older changes
  fading out at the top.
- Respects `.gitignore` and skips hidden files; `--no-ignore` and
  `--hidden` turn those off.
- Themes: `ansi` (the default, follows the terminal's palette live), the
  themes bundled from bat, any `.tmTheme` file, and any base16/base24 scheme.
- Keys: change speed, clear, quit.
- Binary files and files over 1 MiB show their size change instead of a diff.
- Static Linux binaries (x86_64, arm64) attached to each GitHub release.
