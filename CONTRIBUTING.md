# Contributing to difftop

Thanks for helping out! Bug reports, themes, docs and code are all welcome.

## Before you start

- **Bugs:** open an issue using the bug template. Include your terminal,
  your theme, and what you were watching.
- **Features:** open an issue first so we can agree on the shape before
  you spend time on code. difftop is meant to stay small and ambient;
  features that make it a review tool will probably be declined.
- **Small fixes** (typos, docs, obvious bugs): just send a pull request.

## Setup

You'll need stable Rust (install via [rustup](https://rustup.rs)) on Linux.

```bash
git clone https://github.com/Seenko/difftop
cd difftop
cargo test
cargo run -- --help
```

[ARCHITECTURE.md](ARCHITECTURE.md) explains how the code fits together.
The doc comment at the top of each file in `src/` explains that file.

## Making a change

1. Branch off `main`.
2. Make the change, and add a test if it adds or changes logic.
3. Run the same checks as CI:

   ```bash
   cargo fmt
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```

4. Add a line under `Unreleased` in [CHANGELOG.md](CHANGELOG.md) if users
   would notice the change.
5. Open a pull request. Explain what changed and why. Screenshots or
   recordings help a lot for anything visual. (The README's pictures are
   regenerated with `scripts/screenshots.sh` at each release.)

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/),
e.g. `feat: add --no-fade flag` or `fix: keep watching after a folder is renamed`.

## Style

- Prefer the simplest code that works. Don't add an abstraction until
  there's a second use for it.
- Comments explain *why*. Names should explain *what*.
- Keep terminal code in `ui.rs`, `render.rs` and `main.rs`. Everything else
  should be testable without a terminal.

## Themes

You don't need to change any code to use a theme. Drop a `.tmTheme` or
base16 `.yaml` file into `~/.config/difftop/themes/`. To bundle a theme
with difftop, contribute it upstream to [bat] or [tinted-theming], and
difftop picks it up from there.

[bat]: https://github.com/sharkdp/bat
[tinted-theming]: https://github.com/tinted-theming/schemes

## Licensing

By contributing you agree that your contributions are dual licensed under
[MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE), like the rest of the project.

## Conduct

Everyone taking part is expected to follow the [code of conduct](CODE_OF_CONDUCT.md).
