# Changelog

All notable changes to difftop are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Watch one or more folders recursively and show each change as a
  syntax-highlighted diff that rises up the screen, with older changes
  fading out at the top.
- Respects `.gitignore` and skips hidden files; `--no-ignore` and
  `--hidden` turn those off.
- Themes: `ansi` (the default, follows the terminal's palette live), the
  themes bundled from bat, any `.tmTheme` file, and any base16/base24 scheme.
- Keys: pause, change speed, clear, quit.
