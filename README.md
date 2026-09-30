# difftop

Watch a folder and see every file change rise up your terminal as a live,
syntax-highlighted diff.

difftop is less a review tool and more an ambient display: keep it open in
a side pane of a tiling window manager and it shows what's happening in your
projects. It doesn't care who made a change, whether that's you, your
editor, a formatter or five coding agents at once.

```text
▍ src/feed.rs  +1 −0
      pub fn clear(&mut self) {
+         self.budget = 0.0;
          self.shown.clear();
      }

▍ hello.py  new +3
+ def hello(name):
+     """Say hi."""
+     return f"hi {name}"
```

New changes enter at the bottom and push older ones up, and the older ones
fade as they reach the top. Changes that arrive in a burst are queued and
released at a steady pace, so they scroll in instead of appearing all at once.

## Install

Linux only for now. You'll need a Rust toolchain.

```bash
cargo install --git https://github.com/Seenko/difftop
```

## Use

```bash
difftop                   # watch the current folder
difftop ~ /etc            # watch several folders
difftop --theme dracula   # pick a theme
difftop --help            # everything else
```

| Key            | Action                  |
| -------------- | ----------------------- |
| `space` / `p`  | pause / resume          |
| `+` / `-`      | faster / slower         |
| `c`            | clear the screen        |
| `q` / `Esc`    | quit                    |

What gets watched:

- Folders are watched recursively, and new folders are picked up as they appear.
- `.gitignore` rules are respected. `--no-ignore` turns that off.
- Hidden files and folders are skipped. `--hidden` includes them.
- `.git` is always skipped.
- Binary files and files over 1 MiB are skipped.

## Themes

The default theme is **`ansi`**. It colors everything with your terminal's own
16-color palette, so it matches your shell theme automatically and follows it
live when you switch terminal themes.

To choose a different theme, run `difftop --list-themes` and pass a name with
`--theme`. Case and punctuation don't matter, so `solarized-dark` finds
`Solarized (dark)`. Besides `ansi` you can use:

- **The themes bundled from [bat]**, for example Dracula, Nord, gruvbox, Catppuccin,
  Solarized, Monokai and OneHalf.
- **Any [base16/base24] scheme.** There are hundreds. To install all of them at once:

  ```bash
  git clone https://github.com/tinted-theming/schemes ~/.config/difftop/themes/tinted
  difftop --theme rose-pine
  ```

- **Any `.tmTheme` file**, the TextMate/Sublime format that bat and delta use.

Theme files dropped anywhere under `~/.config/difftop/themes/` are found by
file name. A theme there with the same name as a bundled one takes priority.
You can also pass a path directly with `--theme path/to/theme.yaml`.

If you use [tinted-shell] or base16-shell to theme your terminal, the
`base16` and `base16-256` themes follow it the same way `ansi` does.

## Limits

- difftop needs one inotify watch per folder. If you watch a huge tree, you
  may hit `fs.inotify.max_user_watches`. difftop shows a warning in the status
  line when that happens.
- It keeps a copy of every tracked text file in memory, capped at 256 MiB
  at startup, so that the first edit to a file can already be shown as a
  diff. A file past that cap shows "changed, diffing from now on" on its
  first change, and after that it's diffed normally.

## Contributing

Contributions are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) covers setup and
the workflow, and [ARCHITECTURE.md](ARCHITECTURE.md) explains how the code
fits together.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT), at your option. Unless you explicitly state otherwise,
any contribution you submit for inclusion is dual licensed the same way,
without any additional terms or conditions.

[bat]: https://github.com/sharkdp/bat
[base16/base24]: https://github.com/tinted-theming/home
[tinted-shell]: https://github.com/tinted-theming/tinted-shell
