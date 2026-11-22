# ⚡ AppMgr — Universal Linux Application Combine

<div align="center">

[![Version](https://img.shields.io/badge/version-1.4.0--beta-brightgreen?style=for-the-badge)](https://github.com/AleXZahn-max/appmgr/releases)
[![Status](https://img.shields.io/badge/status-active%20development-orange?style=for-the-badge)](#-project-status)
[![License](https://img.shields.io/badge/license-MIT-blue?style=for-the-badge)](LICENSE)
[![Python](https://img.shields.io/badge/python-3.8+-3776AB?style=for-the-badge&logo=python&logoColor=white)](https://www.python.org/)
[![Platform](https://img.shields.io/badge/platform-linux-FCC624?style=for-the-badge&logo=linux&logoColor=black)](https://www.kernel.org/)
[![Discord](https://img.shields.io/badge/Discord-AleXZahn__-5865F2?style=for-the-badge&logo=discord&logoColor=white)](https://discord.com)
[![Built With](https://img.shields.io/badge/Built%20With-Antigravity%20(Neon%20xHigh)-9B51E0?style=for-the-badge)](https://github.com/AleXZahn-max/appmgr)

**The next-generation Pacman + APT hybrid engine for universal Linux application management.**

</div>

---

> [!IMPORTANT]
> ### 🚧 Project Status: Active Beta Development
> **AppMgr** is currently under active development. Features are rapidly evolving, and new capabilities are being continuously added.
> * If you encounter bugs, regressions, or have feature requests, please report them directly via **Discord**: `AleXZahn_`.

> [!NOTE]
> ### 🤖 Authorship & Development
> This software is developed solo by **AleXZahn** with the assistance of the AI IDE agent **Antigravity (Neon xHigh)**.

---

## 🌟 What is AppMgr?

**AppMgr** is an all-in-one application manager and isolation combine designed for Linux systems (Arch Linux, CachyOS, Debian, Ubuntu, Fedora, openSUSE, and more). It allows you to install, isolate, search, configure, upgrade, and manage applications from various formats seamlessly—without root requirements and without cluttering or breaking your system package manager (Pacman, APT, DNF).

### The Pacman + APT Hybrid Experience
AppMgr marries the best visual and operational paradigms of two iconic package managers:
* **From Pacman**: Double-colon prompts (`::`), compact confirmation dialogs (`[Y/n]`), blazing execution speed, and the animated **Candy Pacman progress bar** (`(██████ C • • • •) 45%` — eating dots as files download and unpack).
* **From APT**: Structured pre-flight verification steps (`Reading package lists... Done`), comprehensive transaction summaries (delta sizes, dependency state, action lists), lifecycle trigger processing (`Processing triggers for desktop-file-utils...`), and clean status badges (`[ OK ]`, `[ WORK ]`, `[ FAIL ]`, `[ WARN ]`).

---

## 💎 Key Strengths & Architectural Advantages

1. **Zero System Package Manager Conflicts**:
   Applications reside in dedicated rootless directories (`~/.local/share/appmgr/apps/<name>/` or `/opt/appmgr/apps/<name>/` in system mode). Your base system package database remains untouched and pristine.

2. **Transparent `/opt/` Virtualization via Bubblewrap (`bwrap`)**:
   Heavier proprietary `.deb` packages (e.g., Discord, VS Code, Google Chrome) often hardcode absolute file paths to `/opt/appname/` or `/usr/share/`. AppMgr automatically generates a sandbox wrapper that mounts those directories into an unprivileged user namespace, allowing complex binaries to run out of the box without root privileges.

3. **Smart CLI vs GUI Desktop Integration**:
   AppMgr inspects binaries for GUI toolkit linkages (`libX11`, `libwayland`, `libgtk`, `libQt`, `libSDL`, `libGL`).
   * **CLI tools** (e.g., `btop`, `ripgrep`, `jq`, `kubectl`): Only creates a launcher in your `PATH` (`~/.local/bin`), avoiding clutter in desktop app menus.
   * **GUI apps**: Automatically extracts icons into `hicolor` themes, crafts compliant `.desktop` entries, and refreshes the desktop database.

4. **Public GitHub API with Zero Required Keys & ETag Caching**:
   AppMgr operates out of the box using GitHub's public API without requiring any registration or tokens. It leverages `ETag` and `If-None-Match` caching: when checking for updates, HTTP `304 Not Modified` responses consume zero GitHub API rate limits. (An optional `GITHUB_TOKEN` can be supplied to raise limits to 5,000 req/hr).

5. **Multi-Threaded Concurrent Upgrade Checks**:
   Check dozens of installed applications concurrently in seconds via Python's `ThreadPoolExecutor`, outputting synchronized real-time progress.

6. **SHA-256 Integrity Verification**:
   Automatically detects and downloads checksum files (`sha256sum.txt`, `SHA256SUMS`, `checksums.txt`, `*.sha256`) or asset digest hashes, validating downloaded archives before execution.

7. **Persistent Environment Variables & Launch Flags**:
   Assign custom environment variables and runtime flags (such as Wayland or Ozone parameters) per application. AppMgr preserves these configurations across updates.

8. **Atomic Upgrades & Rollback Cache**:
   During upgrades, AppMgr automatically creates an archive backup of the prior working version. If an update fails, AppMgr rolls back instantly. You can also manually revert using `appmgr rollback <app>`.

9. **Machine-Readable JSON Mode & Automation**:
   With `--json`, commands like `appmgr list --json` and `appmgr upgrade --check --json` output clean JSON for scripts, status bars, and automated setups. Manifest `export` and `import` allow reproducing entire app environments with one command.

---

## 📦 Supported Formats

| Format | Type Badge | Description |
| :--- | :---: | :--- |
| **Debian Packages** | `[DEB]` | Extracts data archives (`.tar.xz`, `.tar.gz`, `.tar.zst`), parses control metadata, sets up dependencies and optional Bubblewrap containers. |
| **AppImage** | `[APPIMAGE]` | Inspects embedded desktop metadata and icons, checks FUSE availability (`libfuse.so.2`), with automatic fallback to `--appimage-extract-and-run`. |
| **Binary Tarballs** | `[TAR]` | Supports `.tar.xz`, `.tar.zst`, `.tar.gz`, `.tgz`, `.tar.bz2`. Features Tar Slip protection, single-root directory auto-collapsing (`--strip-components=1`), and ELF binary discovery. |
| **Zip Archives** | `[ZIP]` | Safe extraction via `zipfile` with Zip Slip defense, root collapsing, Unix permission restoration, and binary scoring. |
| **Standalone ELF** | `[BIN]` | Raw Linux executables without archive packaging (e.g., `yt-dlp_linux`, `jq-linux-amd64`). Verifies `\x7fELF` magic bytes, sets executable permissions, and wraps into `PATH`. |

---

## 🛠️ Installation & Building

### Option 1: Fast User Installation (Recommended)
Installs AppMgr into `~/.local/bin` and user data directories (no sudo required):
```bash
git clone https://github.com/AleXZahn-max/appmgr.git
cd appmgr
./install.sh
```

### Option 2: Using Makefile
```bash
# For current user (~/.local):
make install-user

# System-wide (/usr/local, requires sudo):
sudo make install
```

### Option 3: Arch Linux / CachyOS Package
Build and install via `PKGBUILD`:
```bash
cd appmgr
makepkg -si
```

---

## 📖 Command Reference & Examples

### 1. Installing Applications
```bash
# Search and install interactively by app name:
appmgr install btop

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
appmgr install btop -y
```

### 2. Managing Environment Variables (`env`)
```bash
# Set custom environment variables (e.g., Wayland flags):
appmgr env obsidian WAYLAND_DISPLAY=wayland-0 ELECTRON_OZONE_PLATFORM_HINT=auto

# View configured variables:
appmgr env obsidian --list

# Unset a variable:
appmgr env obsidian --unset WAYLAND_DISPLAY
```

### 3. Persistent Launch Arguments (`args`)
```bash
# Set permanent CLI flags passed to the binary on every start:
appmgr args btop --utf-force --low-color

# View arguments:
appmgr args btop --list

# Clear arguments:
appmgr args btop --clear
```

### 4. Version Freezing (Pinning)
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

### 5. Checking & Upgrading Packages
```bash
# Multi-threaded check for updates without installing:
appmgr upgrade --check

# Machine-readable JSON output for scripts:
appmgr upgrade --check --json

# Upgrade all linked applications with atomic rollback backup:
appmgr upgrade -y

# Upgrade a specific package:
appmgr upgrade gh
```

### 6. Rollback & Cache Cleanup
```bash
# Restore previous version from rollback backup:
appmgr rollback btop

# Clean up rollback cache and temporary files:
appmgr clean -y
```

### 7. Exporting & Importing Application Manifests
```bash
# Export all installed applications and configurations:
appmgr export my-apps.json

# Restore all applications on a fresh machine:
appmgr import my-apps.json -y
```

### 8. System Diagnostics & Info
```bash
# Run comprehensive diagnostic checks (FUSE, bwrap, libraries, PATH):
appmgr doctor

# Inspect package details and library linkage:
appmgr info btop

# Run an installed application:
appmgr run btop
```

### 9. Removing Applications
```bash
appmgr remove btop -y
```

---

## 🤝 Feedback & Bug Reports

Found an issue or have an idea?
* **Discord**: `AleXZahn_`
* **GitHub Issues**: [Open an issue](https://github.com/AleXZahn-max/appmgr/issues)

---

## 📄 License

This project is licensed under the [MIT License](LICENSE).
