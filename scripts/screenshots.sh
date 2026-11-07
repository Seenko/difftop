#!/usr/bin/env bash
# Regenerates the README screenshots in docs/screenshots/ from a scripted
# demo, so they always show the current difftop. Run it before every
# release (see AGENTS.md) and check the README still matches.
#
# Needs tmux and freeze (https://github.com/charmbracelet/freeze).
# The demo runs in a throwaway home folder with a neutral prompt, so no real
# user name or path ends up in the pictures.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release --quiet
bin="$PWD/target/release/difftop"
out="$PWD/docs/screenshots"
session=difftop-screenshots
demo="$(mktemp -d)"
trap 'tmux kill-session -t "$session" 2>/dev/null || true; rm -rf "$demo"' EXIT
mkdir -p "$out"

home="$demo/home"
project="$home/project"
mkdir -p "$project/src" "$project/assets"
cat > "$home/.bashrc" <<'EOF'
PS1='\[\e[1;38;5;114m\]you\[\e[0m\]:\[\e[1;38;5;75m\]\w\[\e[0m\]$ '
alias ls='ls --color=auto'
export LS_COLORS='di=1;38;5;75:ex=1;38;5;114'
EOF
cat > "$project/src/main.rs" <<'EOF'
use std::env;

/// Greets whoever is named on the command line.
fn main() {
    let name = env::args().nth(1).unwrap_or_default();
    println!("Hello, {name}!");
}
EOF
cat > "$project/README.md" <<'EOF'
# greeter

Says hello.
EOF
head -c 12000 /dev/zero > "$project/assets/logo.png"

# Starts difftop on the demo project in a terminal <rows> tall, with the
# default theme. COLORTERM makes it use its exact RGB colors, so the pictures
# don't depend on freeze's ANSI palette.
start() {
    tmux kill-session -t "$session" 2>/dev/null || true
    tmux new-session -d -s "$session" -x 96 -y "$1" -c "$project" \
        env -i HOME="$home" PATH="$PATH" TERM=xterm-256color COLORTERM=truecolor \
        SHELL=/bin/bash "$bin"
    sleep 1.5
}
# Types a command into difftop's shell prompt and waits for it to finish.
run() {
    tmux send-keys -t "$session" -l "$1"
    tmux send-keys -t "$session" Enter
    sleep 1
}
# Saves what's on screen as docs/screenshots/<name>.png.
shot() {
    tmux capture-pane -t "$session" -p -e > "$demo/$1.ansi"
    freeze "$demo/$1.ansi" --language ansi --window --padding 20 --margin 0 \
        --border.radius 8 --background '#1a1a1a' --output "$out/$1.png" \
        < /dev/null > /dev/null # freeze waits on a non-terminal stdin otherwise
    echo "wrote docs/screenshots/$1.png"
}

# 1. The feed: an edit with syntax highlighting, a new file, a binary change.
start 24
cat > "$project/src/main.rs" <<'EOF'
use std::env;

/// Greets everyone named on the command line.
fn main() {
    let names: Vec<String> = env::args().skip(1).collect();
    for name in &names {
        println!("Hello, {name}!");
    }
}
EOF
sleep 1.5
cat > "$project/src/lib.rs" <<'EOF'
pub fn greeting(name: &str) -> String {
    format!("Hello, {name}!")
}
EOF
sleep 1
head -c 15500 /dev/zero > "$project/assets/logo.png"
sleep 2.5
shot feed

# 2. The shell: commands in boxes, a diff arriving between them, cd followed.
start 22
tmux send-keys -t "$session" ':'
sleep 1
run 'ls'
printf '\nRun it with some names.\n' >> "$project/README.md"
sleep 1.5
run 'cd src && ls'
sleep 1
shot shell
