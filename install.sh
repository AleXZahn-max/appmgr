#!/usr/bin/env bash
# Quick installer for appmgr

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET_BIN="$HOME/.local/bin"
TARGET_SHARE="$HOME/.local/share/appmgr"

echo -e "\033[1;36m::\033[0m \033[1;37mInstalling appmgr to user environment (~/.local)...\033[0m"

mkdir -p "$TARGET_BIN"
mkdir -p "$TARGET_SHARE/appmgr"
mkdir -p "$HOME/.local/share/bash-completion/completions"
mkdir -p "$HOME/.local/share/zsh/site-functions"
mkdir -p "$HOME/.local/share/fish/vendor_completions.d"
mkdir -p "$HOME/.local/share/man/man1"

# Copy python code
cp -r "$SCRIPT_DIR/appmgr/"* "$TARGET_SHARE/appmgr/"

# Create executable in ~/.local/bin
cat << 'EOF' > "$TARGET_BIN/appmgr"
#!/usr/bin/env python3
import sys
from pathlib import Path

share_dir = Path.home() / ".local/share/appmgr"
if str(share_dir) not in sys.path:
    sys.path.insert(0, str(share_dir))

from appmgr.cli import main

if __name__ == "__main__":
    main()
EOF

chmod +x "$TARGET_BIN/appmgr"

# Install completions
cp "$SCRIPT_DIR/completions/bash/appmgr" "$HOME/.local/share/bash-completion/completions/appmgr" 2>/dev/null || true
cp "$SCRIPT_DIR/completions/zsh/_appmgr" "$HOME/.local/share/zsh/site-functions/_appmgr" 2>/dev/null || true
cp "$SCRIPT_DIR/completions/fish/appmgr.fish" "$HOME/.local/share/fish/vendor_completions.d/appmgr.fish" 2>/dev/null || true
cp "$SCRIPT_DIR/man/man1/appmgr.1" "$HOME/.local/share/man/man1/appmgr.1" 2>/dev/null || true

echo -e "\033[1;32m[  OK  ]\033[0m appmgr binary installed to: \033[1;33m$TARGET_BIN/appmgr\033[0m"
echo -e "\033[1;32m[  OK  ]\033[0m Shell completions and man pages configured."
echo
echo -e "\033[1;36m::\033[0m Test the installation with: \033[1;32mappmgr --version\033[0m or \033[1;32mappmgr doctor\033[0m"
