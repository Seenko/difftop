# Changelog

All notable changes to difftop are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `install.sh`: install the latest release with one `curl ... | sh`.
- Scroll back through history (arrow keys, PgUp/PgDn, Home, mouse wheel)
  while changes keep arriving. A toast shows how many newer lines there
  are; press `End` or click it to follow again.
- Scrolling the mouse wheel over the speed display (`30/s`) changes the speed.
- One status line at the bottom: what's watched, the `:` and `c` keys, and
  the file count and speed.

- A shell prompt (`:` or `!`): run any command, see its output in the
  feed as it runs, stop it with `Ctrl-C`. When a command changes directory,
  difftop follows and watches the new folder. History with `↑`/`↓`, Tab
  completion, paste. The prompt is your own shell's (PS1, starship, ...);
  commands run in your shell with your aliases on a pseudo-terminal, so
  colors and columns match your terminal. Each command's output is
  boxed, with diffs flowing around the boxes; in shell mode the prompt
  sits in its own box at the bottom.

- Hot reload: replacing the difftop binary makes every running difftop
  switch to the new version within seconds, keeping history, scroll
  position, speed and the watched folder.

### Changed

- Much lower memory use: file text kept for diffing is capped at 16 MiB,
  most recently modified files first (watching a home folder went from
  ~290 MB to ~50 MB; a project stays around 10 MB).
- Softer colors for the quit warning.

- Quitting with `Esc` takes two presses; the first shows "Press Esc again
  to quit difftop". `Ctrl-C` quits at once.
- Removed pause (space) and `q`; quit with `Ctrl-C` like any other program.

### Fixed

- The screen could get scrambled (typed keys echoed, rows shifted) while
  the shell prompt was fetched: the prompt's shell took over difftop's
  terminal. It now runs in its own session. `Ctrl-L` redraws the screen.
- In Windows Terminal the shell-mode frame could push the whole screen up
  a line (the frame's corner filled the last cell, which made the terminal
  wrap). difftop now turns line wrapping off while it runs. The frame is
  also redrawn btop-style, with the title and key hints cut into the
  border.
- Right-clicking (which pastes in many terminals) could pause or clear
  the screen, because pasted text was read as keypresses.

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
