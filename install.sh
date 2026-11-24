#!/usr/bin/env bash
# Fast user installer for appmgr (Rust edition)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET_BIN="$HOME/.local/bin"
APPMGR_VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$SCRIPT_DIR/Cargo.toml" | head -n 1)"
APPMGR_VERSION="${APPMGR_VERSION:-2.4.0}"

echo -e "\033[1;36m::\033[0m \033[1;37mBuilding and installing appmgr v${APPMGR_VERSION} (Rust)...\033[0m"

# Ensure Rust/Cargo is available
if ! command -v cargo &>/dev/null; then
    if [ -f "$HOME/.cargo/env" ]; then
        # shellcheck source=/dev/null
        source "$HOME/.cargo/env"
    else
        echo -e "\033[1;31m[ FAIL ]\033[0m Cargo/Rust is not found in PATH or ~/.cargo/env."
        echo "        Please install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        exit 1
    fi
fi

cd "$SCRIPT_DIR"
cargo build --release --locked

mkdir -p "$TARGET_BIN"
mkdir -p "$HOME/.local/share/bash-completion/completions"
mkdir -p "$HOME/.local/share/zsh/site-functions"
mkdir -p "$HOME/.local/share/fish/vendor_completions.d"
mkdir -p "$HOME/.local/share/man/man1"

install -m 755 target/release/appmgr "$TARGET_BIN/appmgr"

# Generate and install completions directly from the compiled binary
target/release/appmgr completions bash > "$HOME/.local/share/bash-completion/completions/appmgr" 2>/dev/null || true
target/release/appmgr completions zsh > "$HOME/.local/share/zsh/site-functions/_appmgr" 2>/dev/null || true
target/release/appmgr completions fish > "$HOME/.local/share/fish/vendor_completions.d/appmgr.fish" 2>/dev/null || true

if [ -f "$SCRIPT_DIR/man/man1/appmgr.1" ]; then
    cp "$SCRIPT_DIR/man/man1/appmgr.1" "$HOME/.local/share/man/man1/appmgr.1" 2>/dev/null || true
fi

# Ensure ~/.local/bin is in PATH without duplicates
BIN_DIR="$HOME/.local/bin"
for RC in "$HOME/.bashrc" "$HOME/.zshrc"; do
    if [ -f "$RC" ]; then
        if ! grep -q 'PATH=.*\.local/bin' "$RC" 2>/dev/null; then
            echo "export PATH=\"$BIN_DIR:\$PATH\"" >> "$RC"
        fi
    fi
done

echo -e "\033[1;32m[  OK  ]\033[0m appmgr v${APPMGR_VERSION} binary installed to: \033[1;33m$TARGET_BIN/appmgr\033[0m"
echo -e "\033[1;32m[  OK  ]\033[0m Shell completions (bash, zsh, fish) generated and installed."
echo
echo -e "\033[1;36m::\033[0m Test the installation with: \033[1;32mappmgr --version\033[0m or \033[1;32mappmgr doctor\033[0m"
