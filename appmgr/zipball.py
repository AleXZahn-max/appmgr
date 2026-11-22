"""
Zip Archive Package Handler for appmgr.
Extracts .zip archives securely, detects binaries, configures launchers, and integrates with desktop.
"""

import os
import re
import shutil
import stat
import subprocess
import tempfile
import zipfile
from pathlib import Path
from typing import Dict, Any, List, Optional, Tuple, Union

from .config import Config, DEFAULT_CONFIG
from .ui import (
    c,
    Colors,
    apt_prefix,
    PacmanBar,
    Spinner,
)


class SecurityError(Exception):
    pass


class ZipPackage:
    SUPPORTED_EXTENSIONS = (".zip",)

    def __init__(
        self,
        file_path: Union[str, Path],
        config: Optional[Config] = None,
        repo: Optional[str] = None,
        tag: Optional[str] = None,
    ):
        self.file_path = Path(file_path).resolve()
        self.config = config or DEFAULT_CONFIG
        self.repo = repo
        self.tag = tag
        self.metadata: Dict[str, Any] = {}

    def read_metadata(self) -> Dict[str, Any]:
        """Inspect zip archive and derive package name and version."""
        if not self.file_path.exists():
            raise FileNotFoundError(f"File not found: {self.file_path}")

        file_size = self.file_path.stat().st_size
        filename = self.file_path.name

        base_name = filename
        if base_name.lower().endswith(".zip"):
            base_name = base_name[:-4]

        # If repo is specified, prefer repository application name
        if self.repo:
            repo_app = self.repo.split("/")[-1].strip()
            if repo_app:
                pkg_name = repo_app
        else:
            # Strip standard target triples / architectures / os tokens from base_name
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
            "description": f"Zip archive application: {pkg_name}",
            "size": file_size,
            "format": "zip",
        }
        return self.metadata

    def install(self, force: bool = False, env: Optional[Dict[str, str]] = None, args: Optional[List[str]] = None) -> Dict[str, Any]:
        """Extract zip archive, validate binaries, and configure launchers and integration."""
        if not self.metadata:
            self.read_metadata()

        pkg_id = self.metadata["id"]
        app_dir = self.config.apps_dir / pkg_id

        if app_dir.exists():
            if force:
                shutil.rmtree(app_dir, ignore_errors=True)

        app_dir.mkdir(parents=True, exist_ok=True)

        with tempfile.TemporaryDirectory(prefix="appmgr_zip_") as tmp_dir:
            tmp_path = Path(tmp_dir)

            total_files = self._count_zip_members(self.file_path)
            bar = PacmanBar(total=total_files, prefix=f"Unpacking {pkg_id}", unit="files")

            self._safe_extract(self.file_path, tmp_path, bar)
            bar.finish(f"Unpacked {total_files} files")

            # Collapse single root folder if all files were wrapped in one
            self._collapse_root_dir(tmp_path)

            # Discover and score executable binaries
            binaries, is_pure_source = self._discover_binaries(tmp_path, pkg_id)

            if is_pure_source:
                shutil.rmtree(app_dir, ignore_errors=True)
                raise ValueError(
                    f"No executable binaries found in '{self.file_path.name}'.\n"
                    f"        This archive contains source code (compilation required) rather than pre-compiled binaries."
                )

            if not binaries:
                shutil.rmtree(app_dir, ignore_errors=True)
                raise ValueError(
                    f"No executable ELF binaries or scripts found in '{self.file_path.name}'."
                )

            # Copy extracted files to permanent app_dir
            file_count = 0
            for item in tmp_path.iterdir():
                file_count += 1
                dest_item = app_dir / item.name
                if dest_item.exists():
                    if dest_item.is_dir():
                        shutil.rmtree(dest_item, ignore_errors=True)
                    else:
                        dest_item.unlink()
                shutil.move(str(item), str(app_dir))

        # Re-resolve binaries in final app_dir
        final_binaries = []
        for b in binaries:
            rel = b.relative_to(tmp_path)
            final_binaries.append(app_dir / rel)

        primary_bin = self._select_primary_binary(final_binaries, pkg_id)
        is_gui, found_desktop, found_icon = self._detect_gui_or_cli(app_dir, primary_bin, pkg_id)
        launcher_path = self._setup_launcher(app_dir, primary_bin, pkg_id, env=env, args=args)

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
            "format": "zip",
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
            "env": env or {},
            "args": args or [],
        }
        return record

    def _count_zip_members(self, zip_path: Path) -> int:
        try:
            with zipfile.ZipFile(zip_path, "r") as zf:
                return len(zf.infolist())
        except Exception:
            return 50

    def _safe_extract(self, zip_path: Path, dest_dir: Path, bar: PacmanBar):
        """Extract zip archive with path traversal protection and animated progress bar."""
        dest_resolved = dest_dir.resolve()
        with zipfile.ZipFile(zip_path, "r") as zf:
            members = zf.infolist()
            for idx, member in enumerate(members, start=1):
                target_path = (dest_dir / member.filename).resolve()
                if not target_path.is_relative_to(dest_resolved):
                    raise SecurityError(f"Zip slip path traversal attempt detected: {member.filename}")
                zf.extract(member, dest_dir)

                # Restore Unix permissions if present in external_attr
                if member.external_attr:
                    mode = (member.external_attr >> 16) & 0o7777
                    if mode != 0 and target_path.exists():
                        try:
                            target_path.chmod(mode)
                        except Exception:
                            pass

                short_name = member.filename.split("/")[-1][:20]
                bar.update(idx, extra_text=short_name)

    def _collapse_root_dir(self, dest_dir: Path):
        """If all files are in a single top-level directory, move them up (strip-components=1)."""
        entries = list(dest_dir.iterdir())
        if len(entries) == 1 and entries[0].is_dir():
            single_root = entries[0]
            for item in list(single_root.iterdir()):
                target = dest_dir / item.name
                shutil.move(str(item), str(target))
            single_root.rmdir()

    def _discover_binaries(self, app_dir: Path, pkg_id: str) -> Tuple[List[Path], bool]:
        source_markers = ["CMakeLists.txt", "Cargo.toml", "Makefile.am", "configure.ac", "go.mod"]
        has_source_marker = any((app_dir / marker).exists() for marker in source_markers)

        binaries: List[Path] = []
        for p in app_dir.glob("**/*"):
            if not p.is_file() or p.is_symlink():
                continue

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

        if has_source_marker and not binaries:
            return [], True

        return binaries, False

    def _select_primary_binary(self, binaries: List[Path], pkg_id: str) -> Path:
        if len(binaries) == 1:
            return binaries[0]

        scored: List[Tuple[int, Path]] = []
        for b in binaries:
            score = 0
            name_lower = b.name.lower()
            rel_path = str(b)

            if name_lower == pkg_id.lower():
                score += 50
            elif pkg_id.lower() in name_lower:
                score += 20

            if "/bin/" in rel_path or rel_path.startswith("bin/"):
                score += 15

            if any(h in name_lower for h in ["crash", "update", "helper", "daemon", "test", "install", "tool"]):
                score -= 30

            scored.append((score, b))

        scored.sort(key=lambda x: x[0], reverse=True)
        return scored[0][1]

    def _detect_gui_or_cli(
        self, app_dir: Path, primary_bin: Path, pkg_id: str
    ) -> Tuple[bool, Optional[Path], Optional[Path]]:
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

        if not is_gui:
            try:
                out = subprocess.getoutput(f"strings '{primary_bin}' 2>/dev/null | grep -Ei 'libgtk|libqt5|libqt6|libsdl2|libwayland-client' | head -n 5")
                if out.strip():
                    is_gui = True
            except Exception:
                pass

        desktop_candidates = list(app_dir.glob("**/*.desktop"))
        found_desktop = desktop_candidates[0] if desktop_candidates else None

        is_terminal_app = False
        if found_desktop:
            try:
                content = found_desktop.read_text(encoding="utf-8", errors="ignore")
                if re.search(r"^Terminal\s*=\s*true", content, re.MULTILINE | re.IGNORECASE):
                    is_terminal_app = True
            except Exception:
                pass

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

        if is_terminal_app:
            return False, found_desktop, found_icon

        if found_desktop and not is_terminal_app:
            try:
                content = found_desktop.read_text(encoding="utf-8", errors="ignore")
                if "Type=Application" in content:
                    is_gui = True
            except Exception:
                pass

        return is_gui, found_desktop, found_icon

    def _setup_launcher(
        self,
        app_dir: Path,
        primary_bin: Path,
        pkg_id: str,
        env: Optional[Dict[str, str]] = None,
        args: Optional[List[str]] = None,
    ) -> Path:
        launcher_path = self.config.bin_dir / pkg_id
        rel_bin = primary_bin.relative_to(app_dir)

        lib_dirs = []
        for cand in ["lib", "lib64", "usr/lib", "usr/lib64"]:
            if (app_dir / cand).exists():
                lib_dirs.append(f"$APP_ROOT/{cand}")

        extra_ld = (":" + ":".join(lib_dirs)) if lib_dirs else ""

        env_lines = []
        if env:
            for k, v in env.items():
                env_lines.append(f'export {k}="{v}"')
        env_block = ("\n" + "\n".join(env_lines)) if env_lines else ""

        args_lines = []
        if args:
            for a in args:
                args_lines.append(f'    "{a}"')
        args_block = "\nPERSISTENT_ARGS=(\n" + "\n".join(args_lines) + "\n)\n" if args_lines else "\nPERSISTENT_ARGS=()\n"

        script_content = f"""#!/usr/bin/env bash
# Wrapper created by appmgr for {pkg_id} (Zip binary)
APP_ROOT="{app_dir}"
export PATH="$APP_ROOT/bin:$APP_ROOT/usr/bin:$APP_ROOT:$PATH"
export LD_LIBRARY_PATH="$APP_ROOT/usr/lib:$APP_ROOT/lib{extra_ld}:${{LD_LIBRARY_PATH}}"
export XDG_DATA_DIRS="$APP_ROOT/share:$APP_ROOT/usr/share:${{XDG_DATA_DIRS:-/usr/local/share:/usr/share}}"{env_block}
{args_block}
exec "$APP_ROOT/{rel_bin}" "${{PERSISTENT_ARGS[@]}}" "$@"
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
        found_desktop: Optional[Path],
        found_icon: Optional[Path],
    ) -> Tuple[Optional[Path], Optional[Path]]:
        dest_icon = None
        if found_icon:
            icon_ext = found_icon.suffix.lower()
            dest_dir = self.config.icons_dir / "hicolor" / ("scalable" if icon_ext == ".svg" else "256x256") / "apps"
            dest_dir.mkdir(parents=True, exist_ok=True)
            dest_icon = dest_dir / f"{pkg_id}{icon_ext}"
            shutil.copy2(found_icon, dest_icon)

        dest_desktop = self.config.applications_dir / f"appmgr-{pkg_id}.desktop"

        if found_desktop:
            try:
                content = found_desktop.read_text(encoding="utf-8", errors="ignore")
                lines = []
                for line in content.splitlines():
                    if line.startswith("Exec="):
                        lines.append(f'Exec="{launcher_path}" %U')
                    elif line.startswith("Icon=") and dest_icon:
                        lines.append(f"Icon={dest_icon}")
                    elif line.startswith("TryExec="):
                        lines.append(f'TryExec="{launcher_path}"')
                    else:
                        lines.append(line)
                dest_desktop.write_text("\n".join(lines) + "\n")
                return dest_desktop, dest_icon
            except Exception:
                pass

        icon_line = f"Icon={dest_icon}" if dest_icon else "Icon=application-x-executable"
        dt_content = f"""[Desktop Entry]
Name={pkg_id.capitalize()}
Comment=Application {pkg_id} installed via appmgr
Exec="{launcher_path}" %U
Terminal=false
Type=Application
{icon_line}
Categories=Utility;Application;
StartupWMClass={pkg_id}
"""
        dest_desktop.write_text(dt_content)
        return dest_desktop, dest_icon

    def _sanitize_name(self, name: str) -> str:
        clean = re.sub(r"[^a-zA-Z0-9_-]", "", name.lower())
        return clean or "app"
