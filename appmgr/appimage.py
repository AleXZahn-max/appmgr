"""
AppImage Package Handler for appmgr.
Validates, extracts metadata, installs executable, integrates desktop menu and icons.
"""

import os
import re
import shutil
import stat
import subprocess
import tempfile
from pathlib import Path
from typing import Dict, Any, List, Optional

from .config import Config
from .ui import (
    c,
    Colors,
    apt_prefix,
    PacmanBar,
    Spinner,
)


class AppImagePackage:
    def __init__(self, file_path: Path, config: Config):
        self.file_path = file_path.resolve()
        self.config = config
        self.metadata: Dict[str, Any] = {}

    def read_metadata(self) -> Dict[str, Any]:
        """Inspect and parse AppImage metadata."""
        if not self.file_path.exists():
            raise FileNotFoundError(f"File not found: {self.file_path}")

        file_size = self.file_path.stat().st_size
        raw_name = self.file_path.stem
        # Clean common version patterns from name (e.g. Obsidian-1.5.3 -> Obsidian)
        match = re.match(r"^([a-zA-Z0-9_\-]+?)[-_](\d+\..*)", raw_name)
        if match:
            pkg_name = match.group(1)
            version = match.group(2)
        else:
            pkg_name = raw_name
            version = "1.0.0"

        pkg_id = self._sanitize_name(pkg_name)

        # Try to extract embedded desktop file
        desktop_data = self._probe_desktop_info()
        name = desktop_data.get("name") or pkg_name.capitalize()
        version = desktop_data.get("version") or version
        comment = desktop_data.get("comment") or f"AppImage application: {name}"
        categories = desktop_data.get("categories") or "Utility;Application;"

        self.metadata = {
            "id": pkg_id,
            "name": name,
            "version": version,
            "description": comment,
            "categories": categories,
            "size": file_size,
            "format": "appimage",
            "icon_source": desktop_data.get("icon_source"),
        }
        return self.metadata

    def install(self, force: bool = False) -> Dict[str, Any]:
        """Install AppImage, configure wrapper launcher, icon and desktop entry."""
        if not self.metadata:
            self.read_metadata()

        pkg_id = self.metadata["id"]
        app_dir = self.config.apps_dir / pkg_id

        if app_dir.exists() and force:
            shutil.rmtree(app_dir)

        app_dir.mkdir(parents=True, exist_ok=True)

        # Target AppImage binary destination
        target_appimage = app_dir / f"{pkg_id}.AppImage"

        # Copy file with animated pacman progress bar
        bar = PacmanBar(total=100, prefix=f"Deploying {pkg_id}", unit="%")
        self._copy_with_progress(self.file_path, target_appimage, bar)
        bar.finish(f"Deployed {target_appimage.name}")

        # Ensure executable
        target_appimage.chmod(target_appimage.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

        # Extract icon and desktop file from the deployed AppImage
        icon_path, desktop_info = self._extract_resources(target_appimage, app_dir, pkg_id)

        # Create launcher wrapper in PATH
        launcher_path = self.config.bin_dir / pkg_id
        launcher_script = f"""#!/usr/bin/env bash
# Wrapper created by appmgr for {pkg_id} (AppImage)
APPIMAGE="{target_appimage}"

# Automatically handle systems without native FUSE
if [ -z "$APPIMAGE_EXTRACT_AND_RUN" ]; then
    if ! command -v fusermount &>/dev/null && ! command -v fusermount3 &>/dev/null; then
        export APPIMAGE_EXTRACT_AND_RUN=1
    fi
fi

exec "$APPIMAGE" "$@"
"""
        launcher_path.write_text(launcher_script)
        launcher_path.chmod(launcher_path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

        # Create desktop entry
        desktop_file = self.config.applications_dir / f"appmgr-{pkg_id}.desktop"
        categories = desktop_info.get("categories", self.metadata.get("categories", "Utility;Application;"))
        name = desktop_info.get("name", self.metadata.get("name", pkg_id))
        comment = desktop_info.get("comment", self.metadata.get("description", ""))

        icon_entry = f"Icon={icon_path}" if icon_path else "Icon=application-x-executable"

        desktop_content = f"""[Desktop Entry]
Name={name}
Comment={comment}
Exec="{launcher_path}" %U
Terminal=false
Type=Application
{icon_entry}
Categories={categories}
StartupWMClass={pkg_id}
"""
        desktop_file.write_text(desktop_content)

        total_size = target_appimage.stat().st_size

        record = {
            "id": pkg_id,
            "name": name,
            "version": self.metadata.get("version", "1.0.0"),
            "format": "appimage",
            "mode": self.config.mode,
            "install_date": subprocess.getoutput("date -Iseconds"),
            "install_dir": str(app_dir),
            "source_file": str(self.file_path),
            "binaries": [str(launcher_path)],
            "desktop_files": [str(desktop_file)],
            "icons": [str(icon_path)] if icon_path else [],
            "installed_files_count": 1,
            "total_size_bytes": total_size,
            "description": comment,
        }
        return record

    def _probe_desktop_info(self) -> Dict[str, Any]:
        """Try to quickly peek into AppImage desktop information."""
        info: Dict[str, Any] = {}
        # Make temporary executable copy if needed
        is_exec = os.access(self.file_path, os.X_OK)
        if not is_exec:
            try:
                self.file_path.chmod(self.file_path.stat().st_mode | stat.S_IXUSR)
            except Exception:
                pass

        with tempfile.TemporaryDirectory(prefix="appmgr_peek_") as tmp:
            tmp_path = Path(tmp)
            # Try appimage extract with offset or direct call
            try:
                res = subprocess.run(
                    [str(self.file_path), "--appimage-extract", "*.desktop"],
                    cwd=tmp_path,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=5,
                )
                extracted_dt = list(tmp_path.glob("squashfs-root/**/*.desktop"))
                if extracted_dt:
                    dt = extracted_dt[0]
                    content = dt.read_text(encoding="utf-8", errors="replace")
                    for line in content.splitlines():
                        if line.startswith("Name="):
                            info["name"] = line.split("=", 1)[1].strip()
                        elif line.startswith("Comment="):
                            info["comment"] = line.split("=", 1)[1].strip()
                        elif line.startswith("Categories="):
                            info["categories"] = line.split("=", 1)[1].strip()
                        elif line.startswith("X-AppImage-Version="):
                            info["version"] = line.split("=", 1)[1].strip()
            except Exception:
                pass
        return info

    def _extract_resources(
        self, appimage_path: Path, app_dir: Path, pkg_id: str
    ) -> tuple[Optional[Path], Dict[str, str]]:
        """Extract icon and desktop details from AppImage."""
        desktop_info: Dict[str, str] = {}
        icon_path: Optional[Path] = None

        with tempfile.TemporaryDirectory(prefix="appmgr_res_") as tmp:
            tmp_path = Path(tmp)
            try:
                subprocess.run(
                    [str(appimage_path), "--appimage-extract"],
                    cwd=tmp_path,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=10,
                )
                squash_dir = tmp_path / "squashfs-root"
                if squash_dir.exists():
                    # Find desktop file
                    dt_files = list(squash_dir.glob("*.desktop"))
                    if dt_files:
                        dt = dt_files[0]
                        for line in dt.read_text(encoding="utf-8", errors="replace").splitlines():
                            if line.startswith("Name=") and "name" not in desktop_info:
                                desktop_info["name"] = line.split("=", 1)[1].strip()
                            elif line.startswith("Comment=") and "comment" not in desktop_info:
                                desktop_info["comment"] = line.split("=", 1)[1].strip()
                            elif line.startswith("Categories=") and "categories" not in desktop_info:
                                desktop_info["categories"] = line.split("=", 1)[1].strip()

                    # Find icon (.DirIcon or *.png / *.svg)
                    icon_candidates = []
                    dir_icon = squash_dir / ".DirIcon"
                    if dir_icon.exists() and dir_icon.is_file():
                        icon_candidates.append(dir_icon)

                    icon_candidates.extend(list(squash_dir.glob("*.png")))
                    icon_candidates.extend(list(squash_dir.glob("*.svg")))

                    if icon_candidates:
                        chosen_icon = icon_candidates[0]
                        dest_icon_dir = self.config.icons_dir / "hicolor" / "256x256" / "apps"
                        dest_icon_dir.mkdir(parents=True, exist_ok=True)
                        dest_icon = dest_icon_dir / f"{pkg_id}{chosen_icon.suffix}"
                        shutil.copy2(chosen_icon, dest_icon)
                        icon_path = dest_icon
            except Exception:
                pass

        return icon_path, desktop_info

    def _copy_with_progress(self, src: Path, dst: Path, bar: PacmanBar):
        """Copy file chunk by chunk to update progress bar."""
        total = src.stat().st_size
        copied = 0
        chunk_size = 1024 * 1024  # 1 MB

        with open(src, "rb") as fsrc, open(dst, "wb") as fdst:
            while True:
                buf = fsrc.read(chunk_size)
                if not buf:
                    break
                fdst.write(buf)
                copied += len(buf)
                percent = int((copied / total) * 100)
                mb_copied = copied / (1024 * 1024)
                mb_total = total / (1024 * 1024)
                bar.update(percent, extra_text=f"{mb_copied:.1f}/{mb_total:.1f} MB")

    def _sanitize_name(self, name: str) -> str:
        clean = re.sub(r"[^a-zA-Z0-9_-]", "", name.lower())
        return clean or "appimage"
