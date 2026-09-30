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
- The main shortcuts (pause, speed, clear, quit) are listed at the bottom of
  the screen, wrapping in narrow panes.

- A shell prompt (`:` or `!`): run any command, see its output in the
  feed as it runs, stop it with `Ctrl-C`. When a command changes directory,
  difftop follows and watches the new folder. History with `↑`/`↓`, Tab
  completion, paste. The prompt is your own shell's (PS1, starship, ...);
  commands run in your shell with your aliases on a pseudo-terminal, so
  colors and columns match your terminal. A frame marks shell mode.

- Hot reload: replacing the difftop binary makes every running difftop
  switch to the new version within seconds, keeping history, scroll
  position, speed, pause and the watched folder.

### Changed

- Much lower memory use: file text kept for diffing is capped at 16 MiB,
  most recently modified files first (watching a home folder went from
  ~290 MB to ~50 MB; a project stays around 10 MB).
- Softer colors for the quit warning.

- Quitting with `Esc` takes two presses; the first shows "Press Esc again
  to quit difftop". `q` still quits at once.

### Fixed

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
- Keys: pause, change speed, clear, quit.
- Binary files and files over 1 MiB show their size change instead of a diff.
- Static Linux binaries (x86_64, arm64) attached to each GitHub release.
