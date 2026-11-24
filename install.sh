#!/usr/bin/env bash
# Binary release installer for appmgr (with SHA-256 verification, no Rust required)

set -euo pipefail

REPO="AleXZahn-max/appmgr"
DEFAULT_VERSION="v2.5.0"
TARGET_BIN="${APPMGR_INSTALL_DIR:-$HOME/.local/bin}"
DATA_SHARE="$HOME/.local/share"

if [ "$(uname -s)" != "Linux" ]; then
    echo "[ FAIL ] appmgr only supports Linux hosts." >&2
    exit 1
fi

RAW_ARCH="$(uname -m)"
case "$RAW_ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *)
        echo "[ FAIL ] Unsupported architecture: $RAW_ARCH (supported: x86_64, aarch64)." >&2
        exit 1
        ;;
esac

download_file() {
    local url="$1"
    local dest="$2"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 "$url" -o "$dest"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$dest" "$url"
    else
        echo "[ FAIL ] Neither curl nor wget is installed." >&2
        exit 1
    fi
}

compute_sha256() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print tolower($1)}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | awk '{print tolower($1)}'
    else
        echo "[ FAIL ] sha256sum (or shasum) is required for checksum verification." >&2
        exit 1
    fi
}

VERSION="${APPMGR_VERSION:-$DEFAULT_VERSION}"
if [ "$VERSION" = "latest" ]; then
    if command -v curl >/dev/null 2>&1; then
        RESOLVED="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest" || true)"
        TAG_PART="${RESOLVED##*/}"
        if [ -n "$TAG_PART" ] && [ "$TAG_PART" != "latest" ]; then
            VERSION="$TAG_PART"
        else
            VERSION="$DEFAULT_VERSION"
        fi
    else
        VERSION="$DEFAULT_VERSION"
    fi
fi

case "$VERSION" in
    v*) TAG="$VERSION" ;;
    *) TAG="v$VERSION" ;;
esac

ARCHIVE_NAME="appmgr-${TAG}-${ARCH}-linux.tar.gz"
BASE_URL="https://github.com/${REPO}/releases/download/${TAG}"
ARCHIVE_URL="${BASE_URL}/${ARCHIVE_NAME}"
CHECKSUMS_URL="${BASE_URL}/SHA256SUMS"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo ":: Installing appmgr ${TAG} (${ARCH}-linux) from GitHub Releases..."
download_file "$CHECKSUMS_URL" "$TMP_DIR/SHA256SUMS"
download_file "$ARCHIVE_URL" "$TMP_DIR/$ARCHIVE_NAME"

EXPECTED_SHA="$(awk -v target="$ARCHIVE_NAME" '$2 == target || $2 == ("./" target) || $2 == ("*" target) {print tolower($1); exit}' "$TMP_DIR/SHA256SUMS")"
if [ -z "$EXPECTED_SHA" ]; then
    echo "[ FAIL ] Checksum entry for ${ARCHIVE_NAME} not found in SHA256SUMS." >&2
    exit 1
fi

ACTUAL_SHA="$(compute_sha256 "$TMP_DIR/$ARCHIVE_NAME")"
if [ "$ACTUAL_SHA" != "$EXPECTED_SHA" ]; then
    echo "[ FAIL ] SHA-256 verification failed for ${ARCHIVE_NAME}!" >&2
    echo "        Expected: $EXPECTED_SHA" >&2
    echo "        Computed: $ACTUAL_SHA" >&2
    exit 1
fi
echo "[ OK ] SHA-256 verified (${ACTUAL_SHA:0:12}...)"

tar -xzf "$TMP_DIR/$ARCHIVE_NAME" -C "$TMP_DIR"
EXTRACTED_DIR="$TMP_DIR/appmgr-${TAG}-${ARCH}-linux"
if [ ! -f "$EXTRACTED_DIR/appmgr" ]; then
    echo "[ FAIL ] Extracted archive missing appmgr binary." >&2
    exit 1
fi

mkdir -p "$TARGET_BIN"
mkdir -p "$DATA_SHARE/bash-completion/completions"
mkdir -p "$DATA_SHARE/zsh/site-functions"
mkdir -p "$DATA_SHARE/fish/vendor_completions.d"
mkdir -p "$DATA_SHARE/man/man1"

install -m 755 "$EXTRACTED_DIR/appmgr" "$TARGET_BIN/appmgr"

"$TARGET_BIN/appmgr" completions bash > "$DATA_SHARE/bash-completion/completions/appmgr" 2>/dev/null || true
"$TARGET_BIN/appmgr" completions zsh > "$DATA_SHARE/zsh/site-functions/_appmgr" 2>/dev/null || true
"$TARGET_BIN/appmgr" completions fish > "$DATA_SHARE/fish/vendor_completions.d/appmgr.fish" 2>/dev/null || true

if [ -f "$EXTRACTED_DIR/appmgr.1" ]; then
    install -m 644 "$EXTRACTED_DIR/appmgr.1" "$DATA_SHARE/man/man1/appmgr.1" 2>/dev/null || true
elif [ -f "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/man/man1/appmgr.1" ]; then
    install -m 644 "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/man/man1/appmgr.1" "$DATA_SHARE/man/man1/appmgr.1" 2>/dev/null || true
fi

for RC in "$HOME/.bashrc" "$HOME/.zshrc"; do
    if [ -f "$RC" ]; then
        if ! grep -q 'PATH=.*\.local/bin' "$RC" 2>/dev/null; then
            echo "export PATH=\"$TARGET_BIN:\$PATH\"" >> "$RC"
        fi
    fi
done

echo "[ OK ] appmgr ${TAG} installed to: $TARGET_BIN/appmgr"
echo "[ OK ] Shell completions (bash, zsh, fish) installed."
echo ":: Test with: appmgr --version && appmgr doctor"
