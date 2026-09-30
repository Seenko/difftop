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
  completion, paste.

- Hot reload: replacing the difftop binary makes every running difftop
  switch to the new version within seconds, keeping history, scroll
  position, speed, pause and the watched folder.

### Changed

- `Esc` no longer quits (it closes the prompt); `q` and `Ctrl-C` do.

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
