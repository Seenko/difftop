#!/bin/sh
# Installs the latest difftop release:
#
#   curl -fsSL https://raw.githubusercontent.com/Seenko/difftop/main/install.sh | sh
#
# The binary goes to ~/.local/bin, or to $DIFFTOP_DIR if that is set.
# Linux only (x86_64 and arm64), matching the release builds.

set -eu

# Everything runs from main, called on the last line, so a download cut off
# halfway through can't run half a script.
main() {
    repo="https://github.com/Seenko/difftop"
    dir="${DIFFTOP_DIR:-$HOME/.local/bin}"

    [ "$(uname -s)" = Linux ] || fail "difftop only runs on Linux for now"
    case "$(uname -m)" in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) fail "there is no difftop build for $(uname -m) yet" ;;
    esac
    asset="difftop-linux-$arch"

    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    echo "Downloading $asset..."
    curl -fsSL -o "$tmp/$asset" "$repo/releases/latest/download/$asset"
    curl -fsSL -o "$tmp/$asset.sha256" "$repo/releases/latest/download/$asset.sha256"
    (cd "$tmp" && sha256sum -c --quiet "$asset.sha256") || fail "checksum mismatch, not installing"

    mkdir -p "$dir"
    install -m 755 "$tmp/$asset" "$dir/difftop"
    echo "Installed $("$dir/difftop" --version) to $dir/difftop"

    case ":$PATH:" in
        *":$dir:"*) ;;
        *) echo "Note: $dir is not on your PATH. Add it with: export PATH=\"$dir:\$PATH\"" ;;
    esac
}

fail() {
    echo "difftop install: $1" >&2
    exit 1
}

main
