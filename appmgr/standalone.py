"""
Standalone ELF Binary Package Handler for appmgr.
Installs raw executable binaries (e.g. yt-dlp, jq, kubectl),
validates ELF headers, sets permissions, and creates smart launchers.
"""

import os
import re
import shutil
import stat
import subprocess
from pathlib import Path
from typing import Dict, Any, List, Optional, Tuple, Union

from .config import Config, DEFAULT_CONFIG
from .ui import (
    c,
    Colors,
    apt_prefix,
    PacmanBar,
)


class StandaloneBinaryPackage:
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
        """Inspect binary file and derive package name and version."""
        if not self.file_path.exists():
            raise FileNotFoundError(f"File not found: {self.file_path}")

        file_size = self.file_path.stat().st_size
        filename = self.file_path.name

        # Verify ELF header
        try:
            with open(self.file_path, "rb") as f:
                header = f.read(4)
                if header != b"\x7fELF":
                    raise ValueError(f"'{filename}' is not a valid ELF executable (missing \\x7fELF magic bytes).")
        except Exception as e:
            raise ValueError(f"Failed to read binary header of '{filename}': {e}")

        # Derive clean package name
        if self.repo:
            repo_app = self.repo.split("/")[-1].strip()
            if repo_app:
                pkg_name = repo_app
        else:
            cleaned = filename
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
            "description": f"Standalone ELF binary: {pkg_name}",
            "size": file_size,
            "format": "bin",
        }
        return self.metadata

    def install(self, force: bool = False, env: Optional[Dict[str, str]] = None, args: Optional[List[str]] = None) -> Dict[str, Any]:
        """Deploy binary into isolated app folder and create launcher."""
        if not self.metadata:
            self.read_metadata()

        pkg_id = self.metadata["id"]
        app_dir = self.config.apps_dir / pkg_id
        bin_dir = app_dir / "bin"

        if app_dir.exists():
            if force:
                shutil.rmtree(app_dir, ignore_errors=True)

        bin_dir.mkdir(parents=True, exist_ok=True)

        # Destination binary
        dest_bin = bin_dir / pkg_id

        # Copy with progress
        bar = PacmanBar(total=100, prefix=f"Deploying {pkg_id}", unit="%")
        self._copy_with_progress(self.file_path, dest_bin, bar)
        bar.finish(f"Deployed {dest_bin.name}")

        # Make executable
        dest_bin.chmod(dest_bin.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH | stat.S_IRUSR | stat.S_IRGRP | stat.S_IROTH)

        # Check GUI vs CLI
        is_gui = self._check_is_gui(dest_bin)
        desktop_files = []
        icons = []

        # Create launcher wrapper in bin_dir
        launcher_path = self._setup_launcher(app_dir, dest_bin, pkg_id, env=env, args=args)

        if is_gui:
            dt_file = self._setup_desktop_entry(launcher_path, pkg_id)
            if dt_file:
                desktop_files.append(str(dt_file))
        else:
            print(f" {apt_prefix('OK')} Detected command-line utility (no desktop menu entry needed)")

        total_size = dest_bin.stat().st_size

        record = {
            "id": pkg_id,
            "name": self.metadata.get("name", pkg_id),
            "version": self.metadata.get("version", "1.0.0"),
            "format": "bin",
            "mode": self.config.mode,
            "install_date": subprocess.getoutput("date -Iseconds"),
            "install_dir": str(app_dir),
            "source_file": str(self.file_path),
            "binaries": [str(launcher_path)],
            "desktop_files": desktop_files,
            "icons": icons,
            "installed_files_count": 1,
            "total_size_bytes": total_size,
            "is_gui": is_gui,
            "description": self.metadata.get("description", ""),
            "env": env or {},
            "args": args or [],
        }
        return record

    def _copy_with_progress(self, src: Path, dst: Path, bar: PacmanBar):
        total = src.stat().st_size
        copied = 0
        chunk_size = 512 * 1024

        with open(src, "rb") as fsrc, open(dst, "wb") as fdst:
            while True:
                buf = fsrc.read(chunk_size)
                if not buf:
                    break
                fdst.write(buf)
                copied += len(buf)
                percent = int((copied / total) * 100) if total > 0 else 100
                mb_copied = copied / (1024 * 1024)
                mb_total = total / (1024 * 1024)
                bar.update(percent, extra_text=f"{mb_copied:.1f}/{mb_total:.1f} MB")

    def _check_is_gui(self, binary_path: Path) -> bool:
        gui_keywords = ["libx11", "libwayland", "libgtk", "libqt", "libsdl", "libegl", "libgl.so", "libxcb"]
        try:
            res = subprocess.getoutput(f"ldd '{binary_path}' 2>/dev/null")
            for kw in gui_keywords:
                if kw in res.lower():
                    return True
        except Exception:
            pass

        try:
            out = subprocess.getoutput(f"strings '{binary_path}' 2>/dev/null | grep -Ei 'libgtk|libqt5|libqt6|libsdl2|libwayland-client' | head -n 5")
            if out.strip():
                return True
        except Exception:
            pass

        return False

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
# Wrapper created by appmgr for {pkg_id} (Standalone binary)
APP_ROOT="{app_dir}"
export PATH="$APP_ROOT/bin:$PATH"
export LD_LIBRARY_PATH="$APP_ROOT/lib:${{LD_LIBRARY_PATH}}"{env_block}
{args_block}
exec "$APP_ROOT/{rel_bin}" "${{PERSISTENT_ARGS[@]}}" "$@"
"""
        launcher_path.write_text(script_content)
        launcher_path.chmod(launcher_path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        return launcher_path

    def _setup_desktop_entry(self, launcher_path: Path, pkg_id: str) -> Path:
        dest_desktop = self.config.applications_dir / f"appmgr-{pkg_id}.desktop"
        dt_content = f"""[Desktop Entry]
Name={pkg_id.capitalize()}
Comment=Standalone application {pkg_id} installed via appmgr
Exec="{launcher_path}" %U
Terminal=false
Type=Application
Icon=application-x-executable
Categories=Utility;Application;
StartupWMClass={pkg_id}
"""
        dest_desktop.write_text(dt_content)
        return dest_desktop

    def _sanitize_name(self, name: str) -> str:
        clean = re.sub(r"[^a-zA-Z0-9_-]", "", name.lower())
        return clean or "app"
