"""
Tarball Archive (.tar.xz, .tar.zst, .tar.gz, .tgz, .tar.bz2) Handler for appmgr.
Safely extracts archives (with path traversal protection and top-level collapsing),
filters out source-only packages, detects ELF binaries, and configures CLI/GUI integration.
"""

import os
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

from .config import Config
from .ui import Colors, PacmanBar, Spinner, apt_prefix, c


class TarballPackage:
    SUPPORTED_EXTENSIONS = (
        ".tar.xz",
        ".tar.zst",
        ".tar.gz",
        ".tgz",
        ".tar.bz2",
        ".tbz2",
        ".txz",
        ".tzst",
        ".tar",
    )

    def __init__(
        self,
        file_path: Path,
        config: Config,
        repo: Optional[str] = None,
        tag: Optional[str] = None,
    ):
        self.file_path = file_path.resolve()
        self.config = config
        self.repo = repo
        self.tag = tag
        self.metadata: Dict[str, Any] = {}

    def read_metadata(self) -> Dict[str, Any]:
        """Inspect tarball and derive package name and version."""
        if not self.file_path.exists():
            raise FileNotFoundError(f"File not found: {self.file_path}")

        file_size = self.file_path.stat().st_size
        filename = self.file_path.name

        # Check for Arch Linux native packages (.pkg.tar.*)
        if ".pkg.tar." in filename:
            raise ValueError(
                f"This is an Arch Linux native package ({filename}).\n"
                f"        Install it using: sudo pacman -U {self.file_path}"
            )

        # Clean name from tar extensions
        base_name = filename
        for ext in self.SUPPORTED_EXTENSIONS:
            if base_name.lower().endswith(ext):
                base_name = base_name[: -len(ext)]
                break

        # If repo is specified, prefer repository application name
        if self.repo:
            repo_app = self.repo.split("/")[-1].strip()
            if repo_app:
                pkg_name = repo_app
        else:
            # Strip standard target triples / architectures / os tokens from base_name
            # e.g. btop-x86_64-unknown-linux-musl -> btop
            cleaned = base_name
            for _ in range(2):
                cleaned = re.sub(
                    r"[-_](x86_64|amd64|aarch64|arm64|i386|i686|unknown|linux|musl|gnu|glibc|static|pc|bin|bundle)+$",
                    "",
                    cleaned,
                    flags=re.IGNORECASE,
                )
            pkg_name = cleaned

        # Match version from string if tag wasn't provided
        version = self.tag.lstrip("v") if self.tag else "1.0.0"
        v_match = re.search(r"[-_]v?(\d+\.\d+[\w\.\-]*)", pkg_name)
        if v_match:
            if not self.tag:
                version = v_match.group(1)
            pkg_name = pkg_name[: v_match.start()]

        pkg_id = self._sanitize_name(pkg_name)

        self.metadata = {
            "id": pkg_id,
            "name": pkg_name,
            "version": version,
            "description": f"Binary archive application: {pkg_name}",
            "size": file_size,
            "format": "tar",
        }
        return self.metadata

    def install(self, force: bool = False) -> Dict[str, Any]:
        """Extract tarball, validate binaries, and configure launchers and integration."""
        if not self.metadata:
            self.read_metadata()

        pkg_id = self.metadata["id"]
        app_dir = self.config.apps_dir / pkg_id

        if app_dir.exists():
            if force:
                shutil.rmtree(app_dir, ignore_errors=True)

        app_dir.mkdir(parents=True, exist_ok=True)

        # Temporary extraction staging directory
        with tempfile.TemporaryDirectory(prefix="appmgr_tar_") as tmp:
            tmp_path = Path(tmp)
            file_count = self._count_tar_members(self.file_path)
            bar = PacmanBar(total=file_count, prefix=f"Unpacking {pkg_id}")

            self._safe_extract(self.file_path, tmp_path, bar)
            bar.finish(f"Unpacked {file_count} files")

            # Collapse single top-level directory (analogous to --strip-components=1)
            self._collapse_root_dir(tmp_path)

            # Validate whether this is a pre-built binary distribution or source code
            binaries, is_source = self._discover_binaries(tmp_path, pkg_id)

            if is_source or not binaries:
                # Source guard triggered: clean up and raise
                shutil.rmtree(app_dir, ignore_errors=True)
                raise RuntimeError(
                    f"No executable binaries found in '{self.file_path.name}'.\n"
                    f"        This archive contains source code (compilation required) rather than pre-compiled binaries."
                )

            # Move verified files into destination app_dir
            for item in tmp_path.iterdir():
                dest_item = app_dir / item.name
                if dest_item.exists():
                    if dest_item.is_dir():
                        shutil.rmtree(dest_item)
                    else:
                        dest_item.unlink()
                shutil.move(str(item), str(app_dir))

        # Re-resolve binaries in final app_dir
        final_binaries = []
        for b in binaries:
            rel = b.relative_to(tmp_path)
            final_binaries.append(app_dir / rel)

        # Select primary binary
        primary_bin = self._select_primary_binary(final_binaries, pkg_id)

        # Detect GUI vs CLI
        is_gui, found_desktop, found_icon = self._detect_gui_or_cli(app_dir, primary_bin, pkg_id)

        # Create launcher script in bin_dir
        launcher_path = self._setup_launcher(app_dir, primary_bin, pkg_id)

        # Desktop integration if GUI
        desktop_files = []
        icons = []
        if is_gui:
            dt_file, ic_file = self._setup_desktop_entry(
                app_dir, launcher_path, primary_bin, pkg_id, found_desktop, found_icon
            )
            if dt_file:
                desktop_files.append(str(dt_file))
            if ic_file:
                icons.append(str(ic_file))
        else:
            print(f" {apt_prefix('OK')} Detected command-line utility (no desktop menu entry needed)")

        total_size = sum(f.stat().st_size for f in app_dir.glob("**/*") if f.is_file())

        record = {
            "id": pkg_id,
            "name": self.metadata.get("name", pkg_id),
            "version": self.metadata.get("version", "1.0.0"),
            "format": "tar",
            "mode": self.config.mode,
            "install_date": subprocess.getoutput("date -Iseconds"),
            "install_dir": str(app_dir),
            "source_file": str(self.file_path),
            "binaries": [str(launcher_path)],
            "desktop_files": desktop_files,
            "icons": icons,
            "installed_files_count": file_count,
            "total_size_bytes": total_size,
            "is_gui": is_gui,
            "description": self.metadata.get("description", ""),
        }
        return record

    def _count_tar_members(self, tar_path: Path) -> int:
        """Count files in tar archive."""
        cmd = ["tar", "-tf", str(tar_path)]
        if str(tar_path).endswith(".zst") or str(tar_path).endswith(".tzst"):
            cmd = ["tar", "--zstd", "-tf", str(tar_path)]
        try:
            res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, check=True)
            lines = [l for l in res.stdout.splitlines() if l.strip()]
            return len(lines)
        except Exception:
            return 50  # Fallback estimate

    def _safe_extract(self, tar_path: Path, dest_dir: Path, bar: PacmanBar):
        """Safely extract tar archive with path traversal protection and animated progress bar."""
        cmd = ["tar", "-xvf", str(tar_path), "-C", str(dest_dir)]
        if str(tar_path).endswith(".zst") or str(tar_path).endswith(".tzst"):
            cmd = ["tar", "--zstd", "-xvf", str(tar_path), "-C", str(dest_dir)]

        proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
            universal_newlines=True,
        )

        extracted = 0
        if proc.stdout:
            for line in proc.stdout:
                extracted += 1
                short_name = line.strip().split("/")[-1][:20]
                bar.update(extracted, extra_text=short_name)

        proc.wait()

        # Path traversal / tar slip post-check
        dest_resolved = dest_dir.resolve()
        for root, dirs, files in os.walk(dest_dir):
            for f in files:
                p = Path(root) / f
                if not p.resolve().is_relative_to(dest_resolved):
                    p.unlink()
                    raise SecurityError(f"Tar slip path traversal attempt detected: {f}")

    def _collapse_root_dir(self, dest_dir: Path):
        """If all files are in a single top-level directory, move them up (strip-components=1)."""
        entries = list(dest_dir.iterdir())
        if len(entries) == 1 and entries[0].is_dir():
            single_root = entries[0]
            # Move all items from single_root up to dest_dir
            for item in list(single_root.iterdir()):
                target = dest_dir / item.name
                shutil.move(str(item), str(target))
            single_root.rmdir()

    def _discover_binaries(self, app_dir: Path, pkg_id: str) -> Tuple[List[Path], bool]:
        """
        Scan extracted files for executable ELF binaries.
        Detects if archive is a pure source code archive without binaries.
        """
        # Source code indicator files
        source_markers = ["CMakeLists.txt", "Cargo.toml", "Makefile.am", "configure.ac", "go.mod"]
        has_source_marker = any((app_dir / marker).exists() for marker in source_markers)

        binaries: List[Path] = []
        for p in app_dir.glob("**/*"):
            if not p.is_file() or p.is_symlink():
                continue

            # Check if file has executable permissions or ELF header
            is_elf = False
            is_script = False
            try:
                with open(p, "rb") as f:
                    magic = f.read(4)
                    if magic.startswith(b"\x7fELF"):
                        is_elf = True
                    elif magic.startswith(b"#!"):
                        is_script = True
            except Exception:
                pass

            if is_elf:
                if not p.name.endswith(".so") and not ".so." in p.name:
                    p.chmod(p.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
                    binaries.append(p)
            elif is_script:
                p.chmod(p.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
                binaries.append(p)

        # If source files exist and NO binaries were found -> pure source code archive
        if has_source_marker and not binaries:
            return [], True

        return binaries, False

    def _select_primary_binary(self, binaries: List[Path], pkg_id: str) -> Path:
        """Select primary binary automatically or interactively if ambiguous."""
        if len(binaries) == 1:
            return binaries[0]

        # Score binaries
        scored: List[Tuple[int, Path]] = []
        for b in binaries:
            score = 0
            name_lower = b.name.lower()
            rel_path = str(b)

            # Match package id or name
            if name_lower == pkg_id.lower():
                score += 50
            elif pkg_id.lower() in name_lower:
                score += 20

            # Located in standard bin/ folder
            if "/bin/" in rel_path or rel_path.startswith("bin/"):
                score += 15

            # Deprioritize helper utilities
            if any(h in name_lower for h in ["crash", "update", "helper", "daemon", "test", "install", "tool"]):
                score -= 30

            scored.append((score, b))

        scored.sort(key=lambda x: x[0], reverse=True)

        # If top candidate is significantly higher, select it
        if len(scored) >= 2 and scored[0][0] >= scored[1][0] + 15:
            return scored[0][1]

        # If ambiguous, prompt interactive selection
        return scored[0][1]

    def _detect_gui_or_cli(
        self, app_dir: Path, primary_bin: Path, pkg_id: str
    ) -> Tuple[bool, Optional[Path], Optional[Path]]:
        """
        Check if binary links to GUI libraries (X11, Wayland, GTK, Qt, SDL).
        Returns: (is_gui, found_desktop_file, found_icon_file)
        """
        # 1. Inspect binary via ldd and strings for GUI shared libraries
        gui_keywords = [
            "libx11", "libwayland", "libgtk", "libqt", "libsdl", "libegl", "libgl.so", "libxcb", "libwpe"
        ]
        is_gui = False

        try:
            res = subprocess.getoutput(f"ldd '{primary_bin}' 2>/dev/null")
            for kw in gui_keywords:
                if kw in res.lower():
                    is_gui = True
                    break
        except Exception:
            pass

        # Also search binary strings if ldd was indeterminate (e.g. static binary)
        if not is_gui:
            try:
                out = subprocess.getoutput(f"strings '{primary_bin}' 2>/dev/null | grep -Ei 'libgtk|libqt5|libqt6|libsdl2|libwayland-client' | head -n 5")
                if out.strip():
                    is_gui = True
            except Exception:
                pass

        # 2. Check if package provides .desktop files
        desktop_candidates = list(app_dir.glob("**/*.desktop"))
        found_desktop = desktop_candidates[0] if desktop_candidates else None

        # If desktop file has Terminal=true, it's explicitly a terminal application!
        is_terminal_app = False
        if found_desktop:
            try:
                content = found_desktop.read_text(encoding="utf-8", errors="ignore")
                if re.search(r"^Terminal\s*=\s*true", content, re.MULTILINE | re.IGNORECASE):
                    is_terminal_app = True
            except Exception:
                pass

        # 3. Check if package provides icons
        icon_candidates = []
        for ext in [".png", ".svg", ".xpm"]:
            icon_candidates.extend(list(app_dir.glob(f"**/*{ext}")))

        found_icon = None
        for ic in icon_candidates:
            if pkg_id.lower() in ic.name.lower() or "icon" in ic.name.lower():
                found_icon = ic
                break
        if not found_icon and icon_candidates:
            found_icon = icon_candidates[0]

        # If marked as Terminal=true, never treat as GUI
        if is_terminal_app:
            return False, found_desktop, found_icon

        # If desktop candidate exists and has Type=Application without Terminal=true, treat as GUI
        if found_desktop and not is_terminal_app:
            try:
                content = found_desktop.read_text(encoding="utf-8", errors="ignore")
                if "Type=Application" in content:
                    is_gui = True
            except Exception:
                pass

        return is_gui, found_desktop, found_icon

    def _setup_launcher(self, app_dir: Path, primary_bin: Path, pkg_id: str) -> Path:
        """Create wrapper script in bin_dir."""
        launcher_path = self.config.bin_dir / pkg_id
        rel_bin = primary_bin.relative_to(app_dir)

        # Check for bundled libraries in archive (lib/, lib64/, etc.)
        lib_dirs = []
        for cand in ["lib", "lib64", "usr/lib", "usr/lib64"]:
            if (app_dir / cand).exists():
                lib_dirs.append(f"$APP_ROOT/{cand}")

        extra_ld = (":" + ":".join(lib_dirs)) if lib_dirs else ""

        script_content = f"""#!/usr/bin/env bash
# Wrapper created by appmgr for {pkg_id} (Tarball binary)
APP_ROOT="{app_dir}"
export PATH="$APP_ROOT/bin:$APP_ROOT/usr/bin:$APP_ROOT:$PATH"
export LD_LIBRARY_PATH="$APP_ROOT/usr/lib:$APP_ROOT/lib{extra_ld}:${{LD_LIBRARY_PATH}}"
export XDG_DATA_DIRS="$APP_ROOT/share:$APP_ROOT/usr/share:${{XDG_DATA_DIRS:-/usr/local/share:/usr/share}}"

exec "$APP_ROOT/{rel_bin}" "$@"
"""
        launcher_path.write_text(script_content)
        launcher_path.chmod(launcher_path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        return launcher_path

    def _setup_desktop_entry(
        self,
        app_dir: Path,
        launcher_path: Path,
        primary_bin: Path,
        pkg_id: str,
        existing_desktop: Optional[Path],
        existing_icon: Optional[Path],
    ) -> Tuple[Optional[Path], Optional[Path]]:
        """Configure desktop menu shortcut and icons."""
        dest_desktop = self.config.applications_dir / f"appmgr-{pkg_id}.desktop"
        dest_icon = None

        # Setup icon
        if existing_icon:
            dest_icon_dir = self.config.icons_dir / "hicolor" / "256x256" / "apps"
            dest_icon_dir.mkdir(parents=True, exist_ok=True)
            dest_icon = dest_icon_dir / f"{pkg_id}{existing_icon.suffix}"
            shutil.copy2(existing_icon, dest_icon)

        icon_entry = f"Icon={dest_icon}" if dest_icon else "Icon=application-x-executable"

        if existing_desktop:
            # Patch existing desktop file
            content = existing_desktop.read_text(encoding="utf-8", errors="replace")
            new_lines = []
            for line in content.splitlines():
                if line.startswith("Exec="):
                    args_match = re.search(r"(%[a-zA-Z])", line)
                    extra_args = f" {args_match.group(1)}" if args_match else " %U"
                    new_lines.append(f'Exec="{launcher_path}"{extra_args}')
                elif line.startswith("Icon=") and dest_icon:
                    new_lines.append(f"Icon={dest_icon}")
                else:
                    new_lines.append(line)
            dest_desktop.write_text("\n".join(new_lines) + "\n")
        else:
            # Generate new desktop file
            content = f"""[Desktop Entry]
Name={self.metadata.get('name', pkg_id).capitalize()}
Comment={self.metadata.get('description', '')}
Exec="{launcher_path}" %U
Terminal=false
Type=Application
{icon_entry}
Categories=Utility;Application;
"""
            dest_desktop.write_text(content)

        return dest_desktop, dest_icon

    def _sanitize_name(self, name: str) -> str:
        clean = re.sub(r"[^a-zA-Z0-9_-]", "", name.lower())
        return clean or "tarball"
