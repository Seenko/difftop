# Agent guide

Read this first if you are an AI agent (or a human who likes checklists)
picking up work on difftop.

## Orientation, in order

1. [README.md](README.md): what difftop is, from a user's point of view.
2. [ARCHITECTURE.md](ARCHITECTURE.md): the modules, how a change travels,
   and why things are the way they are.
3. The doc comment at the top of the module you're about to touch.
4. Open issues on GitHub: `gh issue list`.

## Commands

```bash
cargo build                                  # debug build
cargo test                                   # all tests, including the real inotify one
cargo clippy --all-targets -- -D warnings    # lints, must be clean
cargo fmt                                    # formatting, must be clean
cargo run -- --theme dracula ~/some/folder   # try it
```

CI runs `fmt --check`, `clippy -D warnings` and `test`. Run all three before
you call something done.

## Conventions

- **Keep it small.** No abstraction until there are two real uses. No
  config options for things that never change. Use the standard library
  before adding a crate.
- **Comments say why, not what.** Every module starts with a `//!` doc
  comment explaining its job. Keep it true when you change the module.
- **Mark deliberate shortcuts** with a `// ponytail:` comment that names the
  limit and how to lift it.
- **Tests sit next to the code** in `#[cfg(test)] mod tests`. Any new branch,
  loop or parser gets a test that would fail if it broke.
- **Nothing but `ui.rs`, `render.rs` and `main.rs` touches the terminal.**
  Keep new logic out of them where you can, so it stays testable.
- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/)
  (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`).
- Record user-visible changes under `Unreleased` in [CHANGELOG.md](CHANGELOG.md).

## Seeing the TUI

You can't look at a terminal directly, but tmux can show it to you:

```bash
# from the repo root
cargo build
DEMO=$(mktemp -d)
tmux new-session -d -s dt -x 80 -y 30 -c "$DEMO" "$PWD/target/debug/difftop"
sleep 1                                  # let the startup scan finish
printf 'fn main() {}\n' > "$DEMO/main.rs"
sleep 1.5                                # let the lines rise in
tmux capture-pane -t dt -p               # plain text
tmux capture-pane -t dt -p -e            # with colors as escape codes
tmux kill-session -t dt
```

## Releasing

1. Bump `version` in `Cargo.toml`, run `cargo build` so `Cargo.lock` follows.
2. In `CHANGELOG.md`, rename `Unreleased` to the version and date, and start
   a fresh `Unreleased` section above it.
3. Commit (`chore: release v0.2.0`), then tag and push:
   `git tag v0.2.0 && git push origin main v0.2.0`.

`.github/workflows/release.yml` builds static Linux binaries (x86_64 and
arm64), refuses to publish if the tag doesn't match `Cargo.toml`, and
attaches the bare binaries (`difftop-linux-x86_64`, `difftop-linux-aarch64`)
to a GitHub release. `install.sh` and the README depend on those names, so keep them in sync. To test the workflow without
publishing, run it by hand: `gh workflow run release.yml`.

## Where things usually go

| You want to...                  | Look at                                     |
| ------------------------------- | ------------------------------------------- |
| add a flag                      | `cli.rs`, then wire it in `main.rs`         |
| change what's ignored           | `filter.rs`                                 |
| change how a change looks       | `render.rs`                                 |
| change the motion or pacing     | `feed.rs`                                   |
| add a key or change the footer  | `ui.rs`                                     |
| support another theme format    | `theme.rs::load_file`                       |
| react to a new kind of fs event | `watch.rs::next_batch` and `State::update`  |
