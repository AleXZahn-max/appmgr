# AppMgr — Universal Linux Application Combine

<div align="center">

[![Version](https://img.shields.io/badge/version-2.1.0-brightgreen?style=for-the-badge)](https://github.com/AleXZahn-max/appmgr/releases)
[![Status](https://img.shields.io/badge/status-active%20development-orange?style=for-the-badge)](#project-status)
[![License](https://img.shields.io/badge/license-MIT-blue?style=for-the-badge)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75+-DEA584?style=for-the-badge&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/platform-linux-FCC624?style=for-the-badge&logo=linux&logoColor=black)](https://www.kernel.org/)
[![Discord](https://img.shields.io/badge/Discord-AleXZahn__-5865F2?style=for-the-badge&logo=discord&logoColor=white)](https://discord.com)
[![Built With](https://img.shields.io/badge/Built%20With-Antigravity%20(Neon%20xHigh)-9B51E0?style=for-the-badge)](https://github.com/AleXZahn-max/appmgr)

**The next-generation Pacman + APT hybrid engine for universal Linux application management, built in blazingly fast Rust.**

</div>

```text
┌─── appmgr v2.1.0 — Universal Linux Application Combine ────────────────────────────────┐
│                                                                                        │
│  :: Initializing transaction for btop (GitHub: aristocratos/btop)...                  │
│  Reading package lists... Done                                                         │
│  Building dependency tree... Done                                                      │
│                                                                                        │
│  Packages (1) btop-1.4.0-x86_64.tar.zst [NEW]                                          │
│                                                                                        │
│  Total Download Size:    1.82 MiB                                                      │
│  Total Installed Size:   4.95 MiB                                                      │
│                                                                                        │
│  :: Proceed with installation? [Y/n] y                                                 │
│  (1/1) Downloading btop-x86_64.tar.zst  (████████████ C • • • • • • •)  62%            │
│  (1/1) Validating SHA-256 checksum...                              [ OK ]              │
│  (1/1) Unpacking application files...                              [ OK ]              │
│  :: Setting up environment wrapper into ~/.local/bin/btop...       [ OK ]              │
│  :: Processing triggers for desktop-file-utils...                  [ OK ]              │
│                                                                                        │
│  [ OK ] btop installed successfully! Run with: btop                                    │
└────────────────────────────────────────────────────────────────────────────────────────┘
```

---

<a id="project-status"></a>
> [!IMPORTANT]
> ### Project Status: Active Development
> **AppMgr v2.0.0** is rewritten entirely in Rust for maximum speed, memory safety, and sub-millisecond cold start times.
> * If you encounter bugs, regressions, or have feature requests, please report them directly via **Discord**: `AleXZahn_`.

> [!NOTE]
> ### Authorship & Development
> This software is developed solo by **AleXZahn** with the assistance of the AI IDE agent **Antigravity (Neon xHigh)**.

---

## Overview

**AppMgr** is an all-in-one application manager and isolation combine designed for Linux systems (Arch Linux, CachyOS, Debian, Ubuntu, Fedora, openSUSE, and more). It allows you to install, isolate, search, configure, upgrade, and manage applications from various formats seamlessly—without root requirements and without cluttering or breaking your system package manager (Pacman, APT, DNF).

### The Pacman + APT Hybrid Experience
AppMgr marries the best visual and operational paradigms of two iconic package managers:
* **From Pacman**: Double-colon prompts (`::`), compact confirmation dialogs (`[Y/n]`), blazing execution speed, and the animated **Candy Pacman progress bar** (`(██████ C • • • •) 45%` — eating dots as files download and unpack).
* **From APT**: Structured pre-flight verification steps (`Reading package lists... Done`), comprehensive transaction summaries (delta sizes, dependency state, action lists), lifecycle trigger processing (`Processing triggers for desktop-file-utils...`), and clean status badges (`[ OK ]`, `[ WORK ]`, `[ FAIL ]`, `[ WARN ]`).

---

## Key Strengths & Architecture

1. **Pure Rust Performance & Instant Startup**:
   Compiled to a single standalone binary with zero runtime dependencies. Sub-millisecond cold start and instant CLI responsiveness.

2. **Zero System Package Manager Conflicts**:
   Applications reside in dedicated rootless directories (`~/.local/share/appmgr/apps/<name>/` or `/opt/appmgr/apps/<name>/` in system mode). Your base system package database remains untouched and pristine.

3. **Transparent `/opt/` Virtualization via Bubblewrap (`bwrap`)**:
   Heavier proprietary `.deb` packages (e.g., Discord, VS Code, Google Chrome) often hardcode absolute file paths to `/opt/appname/` or `/usr/share/`. AppMgr automatically generates a sandbox wrapper that mounts those directories into an unprivileged user namespace, allowing complex binaries to run out of the box without root privileges.

4. **Smart CLI vs GUI Desktop Integration**:
   AppMgr inspects binaries with `goblin` for GUI toolkit linkages (`libX11`, `libwayland`, `libgtk`, `libQt`, `libSDL`, `libGL`).
   * **CLI tools** (e.g., `btop`, `ripgrep`, `jq`, `kubectl`): Only creates a launcher in your `PATH` (`~/.local/bin`), avoiding clutter in desktop app menus.
   * **GUI apps**: Automatically extracts icons into `hicolor` themes, crafts compliant `.desktop` entries, and refreshes the desktop database.

5. **Public GitHub API with Zero Required Keys & ETag Caching**:
   AppMgr operates out of the box using GitHub's public API without requiring any registration or tokens. It leverages `ETag` and `If-None-Match` caching: when checking for updates, HTTP `304 Not Modified` responses consume zero GitHub API rate limits. (An optional `GITHUB_TOKEN` can be supplied to raise limits to 5,000 req/hr).

6. **Multi-Threaded Async Upgrade Checks**:
   Check dozens of installed applications concurrently in parallel with `tokio` (bounded to 8 tasks), outputting synchronized real-time progress.

7. **SHA-256 Integrity Verification**:
   Automatically detects and downloads checksum files (`sha256sum.txt`, `SHA256SUMS`, `checksums.txt`, `*.sha256`) or asset digest hashes, validating downloaded archives before execution.

8. **Persistent Environment Variables & Launch Flags**:
   Assign custom environment variables and runtime flags (such as Ozone hints or toolkit backends) per application. AppMgr preserves these configurations across updates.

9. **Atomic Upgrades & Rollback Cache**:
   During upgrades, AppMgr automatically creates a backup snapshot of the prior working version. If an update fails, AppMgr rolls back instantly. You can also manually revert using `appmgr rollback <app>`.

10. **Built-in Self-Updating (`self-update`)**:
    Automatically checks and updates the `appmgr` binary itself from GitHub Releases, with built-in protection for system package managers (pacman).

---

## Supported Formats

| Format | Type Badge | Description |
| :--- | :---: | :--- |
| **Debian Packages** | `[DEB]` | Extracts data archives (`.tar.xz`, `.tar.gz`, `.tar.zst`) via native `ar`, parses control metadata, sets up dependencies and optional Bubblewrap containers. |
| **AppImage** | `[APPIMAGE]` | Inspects embedded desktop metadata and icons without launching, checks FUSE availability (`libfuse.so.2`), with automatic fallback to `--appimage-extract-and-run`. |
| **Binary Tarballs** | `[TAR]` | Supports `.tar.xz`, `.tar.zst`, `.tar.gz`, `.tgz`, `.tar.bz2`. Features Tar Slip protection, single-root directory auto-collapsing (`--strip-components=1`), and ELF binary discovery. |
| **Zip Archives** | `[ZIP]` | Safe extraction via `zip` with Zip Slip defense, root collapsing, Unix permission restoration, and binary scoring. |
| **Standalone ELF** | `[BIN]` | Raw Linux executables without archive packaging (e.g., `yt-dlp_linux`, `jq-linux-amd64`). Verifies `\x7fELF` magic bytes, sets executable permissions, and wraps into `PATH`. |

---

## Requirements

### Core Dependencies
* **bubblewrap (`bwrap`)**: Rootless `/opt` filesystem virtualization for proprietary `.deb` packages
* **tar**: Standard archive extraction
* **xz & zstd**: Decompressing `.tar.xz` and `.tar.zst` archives
* **desktop-file-utils**: Desktop database indexing (`update-desktop-database`)

### Recommended Tools
* **fuse2 / libfuse2**: Native FUSE mounting for AppImages
* **gtk-update-icon-cache**: Icon theme cache regeneration

### Quick Install Commands

**Arch Linux / CachyOS:**
```bash
sudo pacman -S bubblewrap tar xz zstd desktop-file-utils fuse2
```

**Debian / Ubuntu:**
```bash
sudo apt install bubblewrap tar xz-utils zstd desktop-file-utils libfuse2
```

**Fedora:**
```bash
sudo dnf install bubblewrap tar xz zstd desktop-file-utils fuse
```

---

## Installation & Building

### Option 1: Fast User Installation (Recommended)
Builds and installs AppMgr into `~/.local/bin` (no root/sudo required):
```bash
git clone https://github.com/AleXZahn-max/appmgr.git
cd appmgr
./install.sh
```

### Option 2: Using Makefile
```bash
git clone https://github.com/AleXZahn-max/appmgr.git
cd appmgr

# For current user (~/.local, rootless):
make install-user

# System-wide (/usr/local, requires sudo):
sudo make install
```

### Option 3: Arch Linux / CachyOS Package (PKGBUILD)
Build and install via `PKGBUILD`:
```bash
git clone https://github.com/AleXZahn-max/appmgr.git
cd appmgr
makepkg -si
```

---

## Command Reference & Examples

### 1. Synchronizing Software Sources (`update`, `sync`)
Synchronizes all active sources configured in `~/.config/appmgr/sources.list` (meta-registries, Debian/APT repos, AppImageHub index) using HTTP ETag / Last-Modified caching:
```bash
appmgr update
# or:
appmgr sync
```

### 2. Managing Software Sources & Repositories (`repo`, `sources`)
Manage upstream APT repositories, AppImageHub, AUR endpoints, and meta-registries:
```bash
# List configured sources (tabular overview with package counts):
appmgr repo list

# List configured sources in JSON format:
appmgr repo list --json

# Add a direct APT source:
appmgr repo add apt my-repo https://example.com/debian --suite stable --components main --priority 600

# Temporarily disable or re-enable a source:
appmgr repo disable apt-vscode
appmgr repo enable apt-vscode

# Remove a source and purge its cache:
appmgr repo remove my-repo
```

### 3. Source Recipe & Maintainer Tools (`source`, `pkg`)
Validate and test Schema v2 declarative source definition recipes:
```bash
# Lint a Schema v2 source definition:
appmgr source lint sources/apt-gh.toml

# Test network reachability and index parsing for a source definition:
appmgr source test sources/apt-gh.toml
```

### 4. Searching Applications (`search`)
Search across all configured software sources (APT, AppImageHub, AUR) and GitHub:
```bash
# Search across all sources and GitHub:
appmgr search vscode

# Filter search results by source ID:
appmgr search gh --source apt-gh

# Filter search results by driver type (apt, appimagehub, aur, github, gitlab):
appmgr search obsidian --driver appimagehub
appmgr search paru --driver aur

# Search only local cached repository indexes (offline):
appmgr search ripgrep --offline
```

### 5. Installing Applications (`install`)
Install packages from upstream APT repositories, AppImageHub, AUR, GitHub Releases, or local files:
```bash
# Install with explicit source or driver prefix:
appmgr install apt-gh:gh
appmgr install apt-vscode:code
appmgr install appimagehub:obsidian
appmgr install aur:paru-bin

# Install by application name (searches configured sources by priority, then GitHub):
appmgr install gh
appmgr install code
appmgr install obsidian

# Install specific GitHub repository release:
appmgr install aristocratos/btop

# Install specific version tag:
appmgr install jqlang/jq@jq-1.7.1

# Prefer standalone raw binary over archives:
appmgr install jqlang/jq --prefer bin

# Enable pre-releases:
appmgr install owner/repo --pre

# Install local package files:
appmgr install ~/Downloads/discord-0.0.40.deb
appmgr install ~/Downloads/Obsidian-1.5.8.AppImage
appmgr install ~/Downloads/ripgrep-14.1.0-x86_64-linux.tar.gz
appmgr install ~/Downloads/tool.zip

# Non-interactive mode (assumes yes):
appmgr install apt-gh:gh -y
```

### 6. Linking Applications to GitHub (`link`)
Link an already installed application (e.g. installed from a local file) to a GitHub repository to enable automatic update checking:
```bash
# Link application to GitHub repo:
appmgr link obsidian obsidianmd/obsidian-releases
appmgr link discord discord/discord-linux
```

### 4. Updating AppMgr Itself (`self-update`)
```bash
# Check if a new version of appmgr is available:
appmgr self-update --check

# Update appmgr binary in-place:
appmgr self-update -y
```

### 5. Managing Environment Variables (`env`)
```bash
# Set custom persistent environment variables:
appmgr env obsidian ELECTRON_OZONE_PLATFORM_HINT=auto QT_QPA_PLATFORM=wayland

# View configured variables:
appmgr env obsidian --list

# Unset a variable:
appmgr env obsidian --unset QT_QPA_PLATFORM
```

### 6. Persistent Launch Arguments (`args`)
```bash
# Set permanent CLI flags passed to the binary on every start:
appmgr args btop --utf-force --low-color

# View arguments:
appmgr args btop --list

# Clear arguments:
appmgr args btop --clear
```

### 7. Version Freezing (`pin` / `unpin`)
```bash
# Freeze package version to skip during general upgrades:
appmgr pin btop

# View pinned packages in table:
appmgr list

# Force update a pinned package:
appmgr upgrade btop --force

# Unfreeze package:
appmgr unpin btop
```

### 8. Checking & Upgrading Packages (`upgrade`)
```bash
# Multi-threaded async check for updates without installing:
appmgr upgrade --check

# Machine-readable JSON output for status bars and scripts:
appmgr upgrade --check --json

# Upgrade all linked applications with atomic rollback backup:
appmgr upgrade -y

# Upgrade a specific package:
appmgr upgrade gh
```

### 9. Rollback & Cache Cleanup (`rollback` / `clean`)
```bash
# Restore previous version from rollback backup:
appmgr rollback btop

# Clean up rollback cache and temporary files:
appmgr clean -y
```

### 10. Exporting & Importing Application Manifests (`export` / `import`)
```bash
# Export all installed applications and configurations:
appmgr export my-apps.json

# Restore all applications on a fresh machine:
appmgr import my-apps.json -y
```

### 11. System Diagnostics & Inspection (`doctor` / `info` / `run`)
```bash
# Run comprehensive diagnostic checks (FUSE, bwrap, libraries, PATH):
appmgr doctor

# Inspect package details and library linkage:
appmgr info btop

# Run an installed application:
appmgr run btop
```

### 12. Removing Applications (`remove`)
```bash
appmgr remove btop -y
```

---

## Feedback & Bug Reports

Found an issue or have an idea?
* **Discord**: `AleXZahn_`
* **GitHub Issues**: [Open an issue](https://github.com/AleXZahn-max/appmgr/issues)

---

## License

This project is licensed under the [MIT License](LICENSE).
