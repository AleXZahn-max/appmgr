"""
Debian Package (.deb) Handler for appmgr.
Extracts, sandboxes, fixes desktop entries, sets up wrappers, and integrates with the desktop.
"""

import os
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile
from pathlib import Path
from typing import Dict, Any, List, Optional, Tuple

from .config import Config
from .ui import (
    c,
    Colors,
    apt_prefix,
    PacmanBar,
    Spinner,
)


class DebPackage:
    def __init__(self, file_path: Path, config: Config):
        self.file_path = file_path.resolve()
        self.config = config
        self.metadata: Dict[str, Any] = {}
        self.temp_dir: Optional[Path] = None

    def read_metadata(self) -> Dict[str, Any]:
        """Extract and parse Debian control file without full installation."""
        if not self.file_path.exists():
            raise FileNotFoundError(f"File not found: {self.file_path}")

        file_size = self.file_path.stat().st_size
        control_content = ""

        # Extract control archive
        with tempfile.TemporaryDirectory(prefix="appmgr_ctrl_") as tmp:
            tmp_path = Path(tmp)
            # Try using ar command first
            if shutil.which("ar"):
                subprocess.run(
                    ["ar", "x", str(self.file_path)],
                    cwd=tmp_path,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    check=True,
                )
            else:
                self._ar_extract(self.file_path, tmp_path)

            # Find control.tar.*
            ctrl_tar = None
            for cand in tmp_path.glob("control.tar.*"):
                ctrl_tar = cand
                break

            if not ctrl_tar:
                raise ValueError(f"Invalid debian package (control.tar missing): {self.file_path}")

            # Extract control.tar
            ctrl_extract_dir = tmp_path / "ctrl"
            ctrl_extract_dir.mkdir()
            self._extract_tar(ctrl_tar, ctrl_extract_dir)

            control_file = ctrl_extract_dir / "control"
            if control_file.exists():
                control_content = control_file.read_text(encoding="utf-8", errors="replace")

        # Parse control metadata
        pkg_name = self.file_path.stem.split("_")[0]
        version = "1.0.0"
        desc = ""
        arch = "all"
        depends = ""
        homepage = ""

        for line in control_content.splitlines():
            if line.startswith("Package:"):
                pkg_name = line.split(":", 1)[1].strip()
            elif line.startswith("Version:"):
                version = line.split(":", 1)[1].strip()
            elif line.startswith("Architecture:"):
                arch = line.split(":", 1)[1].strip()
            elif line.startswith("Description:"):
                desc = line.split(":", 1)[1].strip()
            elif line.startswith("Depends:"):
                depends = line.split(":", 1)[1].strip()
            elif line.startswith("Homepage:"):
                homepage = line.split(":", 1)[1].strip()

        self.metadata = {
            "id": self._sanitize_name(pkg_name),
            "name": pkg_name,
            "version": version,
            "architecture": arch,
            "description": desc or f"Application installed from {self.file_path.name}",
            "depends": depends,
            "homepage": homepage,
            "size": file_size,
            "format": "deb",
        }
        return self.metadata

    def install(self, force: bool = False) -> Dict[str, Any]:
        """Perform full installation of Debian package into isolated app directory."""
        if not self.metadata:
            self.read_metadata()

        pkg_id = self.metadata["id"]
        app_dir = self.config.apps_dir / pkg_id

        if app_dir.exists():
            if force:
                shutil.rmtree(app_dir)
            else:
                pass  # Overwrite files

        app_dir.mkdir(parents=True, exist_ok=True)

        with tempfile.TemporaryDirectory(prefix="appmgr_inst_") as tmp:
            tmp_path = Path(tmp)

            # Unpack AR archive
            if shutil.which("ar"):
                subprocess.run(
                    ["ar", "x", str(self.file_path)],
                    cwd=tmp_path,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    check=True,
                )
            else:
                self._ar_extract(self.file_path, tmp_path)

            data_tar = None
            for cand in tmp_path.glob("data.tar.*"):
                data_tar = cand
                break

            if not data_tar:
                raise ValueError(f"No data.tar archive found in {self.file_path}")

            # Extract data.tar with pacman animation
            file_count = self._count_tar_members(data_tar)
            bar = PacmanBar(total=file_count, prefix=f"Unpacking {pkg_id}")

            self._extract_tar_with_progress(data_tar, app_dir, bar)
            bar.finish(f"Unpacked {file_count} files")

        # Post-unpack integration
        created_binaries = self._setup_launchers(app_dir, pkg_id)
        created_desktops = self._setup_desktop_files(app_dir, pkg_id, created_binaries)
        created_icons = self._setup_icons(app_dir, pkg_id)

        # Calculate installed disk size
        total_size = sum(f.stat().st_size for f in app_dir.glob("**/*") if f.is_file())

        record = {
            "id": pkg_id,
            "name": self.metadata.get("name", pkg_id),
            "version": self.metadata.get("version", "1.0.0"),
            "format": "deb",
            "mode": self.config.mode,
            "install_date": subprocess.getoutput("date -Iseconds"),
            "install_dir": str(app_dir),
            "source_file": str(self.file_path),
            "binaries": [str(b) for b in created_binaries],
            "desktop_files": [str(d) for d in created_desktops],
            "icons": [str(i) for i in created_icons],
            "installed_files_count": file_count,
            "total_size_bytes": total_size,
            "description": self.metadata.get("description", ""),
        }
        return record

    def _setup_launchers(self, app_dir: Path, pkg_id: str) -> List[Path]:
        """Detect executables and create smart shell wrapper scripts with optional bwrap path virtualization."""
        created = []
        executables: List[Path] = []

        # Look in standard binary paths first
        standard_search_dirs = [
            app_dir / "usr/bin",
            app_dir / "bin",
            app_dir / "opt",
        ]

        for s_dir in standard_search_dirs:
            if s_dir.exists():
                for p in s_dir.glob("**/*"):
                    if p.is_file() and os.access(p, os.X_OK):
                        # Filter out helper libraries and daemons
                        if not p.name.endswith(".so") and not ".so." in p.name:
                            executables.append(p)

        if not executables:
            # Fallback scan all files
            for p in app_dir.glob("**/*"):
                if p.is_file() and os.access(p, os.X_OK) and not p.name.endswith(".so"):
                    executables.append(p)

        # Prioritize executable matching package name
        primary_exe = None
        for exe in executables:
            if exe.name.lower() == pkg_id.lower():
                primary_exe = exe
                break

        if not primary_exe and executables:
            primary_exe = executables[0]

        if primary_exe:
            launcher_name = pkg_id
            launcher_path = self.config.bin_dir / launcher_name

            rel_target = primary_exe.relative_to(app_dir)

            # Detect if package has files under opt/
            opt_mounts = []
            opt_dir = app_dir / "opt"
            if opt_dir.exists():
                for item in opt_dir.iterdir():
                    opt_mounts.append((f"$APP_ROOT/opt/{item.name}", f"/opt/{item.name}"))

            # Build bwrap mount statements
            bwrap_mount_cmds = []
            for src, dst in opt_mounts:
                bwrap_mount_cmds.append(f'    [ -d "{src}" ] && BWRAP_ARGS+=(--ro-bind "{src}" "{dst}")')

            bwrap_mount_block = "\n".join(bwrap_mount_cmds) if bwrap_mount_cmds else ""

            # Check if primary_exe is inside opt/
            is_under_opt = "opt/" in str(rel_target)
            if is_under_opt:
                container_bin_line = f'    CONTAINER_BIN="/{rel_target}"'
            else:
                container_bin_line = f'    CONTAINER_BIN="$APP_ROOT/{rel_target}"'

            extra_ld = ""
            if opt_dir.exists():
                extra_ld = f":$APP_ROOT/{rel_target.parent}"

            script_content = f"""#!/usr/bin/env bash
# Smart Wrapper created by appmgr for {pkg_id}
# Handles hardcoded /opt paths transparently via Bubblewrap user namespaces
APP_ROOT="{app_dir}"
TARGET_BIN="$APP_ROOT/{rel_target}"

# 1. Bubblewrap (bwrap) rootless virtualization for hardcoded absolute paths (/opt, /usr/share)
if command -v bwrap >/dev/null 2>&1 && [ "$APPMGR_NO_BWRAP" != "1" ]; then
    BWRAP_ARGS=(
        --dev-bind /dev /dev
        --ro-bind /sys /sys
        --proc /proc
        --ro-bind /usr /usr
        --ro-bind /etc /etc
        --ro-bind /bin /bin
        --ro-bind /lib /lib
        --ro-bind /lib64 /lib64
        --bind /run /run
        --bind /tmp /tmp
        --bind "$HOME" "$HOME"
        --tmpfs /opt
    )
    # Mirror host /opt directories into the virtual namespace
    if [ -d /opt ]; then
        for d in /opt/*; do
            [ -e "$d" ] && BWRAP_ARGS+=(--ro-bind "$d" "$d")
        done
    fi
{bwrap_mount_block}

{container_bin_line}
    export LD_LIBRARY_PATH="$APP_ROOT/usr/lib:$APP_ROOT/usr/lib/x86_64-linux-gnu:$APP_ROOT/lib:$APP_ROOT/lib/x86_64-linux-gnu{extra_ld}:${{LD_LIBRARY_PATH}}"
    export XDG_DATA_DIRS="$APP_ROOT/usr/share:${{XDG_DATA_DIRS:-/usr/local/share:/usr/share}}"

    exec bwrap "${{BWRAP_ARGS[@]}}" "$CONTAINER_BIN" "$@"
fi

# 2. Standard direct execution fallback
export PATH="$APP_ROOT/usr/bin:$APP_ROOT/bin:$PATH"
export LD_LIBRARY_PATH="$APP_ROOT/usr/lib:$APP_ROOT/usr/lib/x86_64-linux-gnu:$APP_ROOT/lib:$APP_ROOT/lib/x86_64-linux-gnu{extra_ld}:${{LD_LIBRARY_PATH}}"
export XDG_DATA_DIRS="$APP_ROOT/usr/share:${{XDG_DATA_DIRS:-/usr/local/share:/usr/share}}"

exec "$TARGET_BIN" "$@"
"""
            launcher_path.write_text(script_content)
            launcher_path.chmod(launcher_path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
            created.append(launcher_path)

        return created

    def _setup_desktop_files(self, app_dir: Path, pkg_id: str, binaries: List[Path]) -> List[Path]:
        """Locate, patch, and install desktop entries."""
        created = []
        desktop_candidates = list(app_dir.glob("**/*.desktop"))

        target_bin = binaries[0] if binaries else None

        if desktop_candidates:
            for dt in desktop_candidates:
                content = dt.read_text(encoding="utf-8", errors="replace")
                new_lines = []
                for line in content.splitlines():
                    if line.startswith("Exec=") and target_bin:
                        # Extract flags like %U, %F
                        args_match = re.search(r"(%[a-zA-Z])", line)
                        extra_args = f" {args_match.group(1)}" if args_match else " %U"
                        new_lines.append(f'Exec="{target_bin}"{extra_args}')
                    else:
                        new_lines.append(line)

                dest_dt = self.config.applications_dir / f"appmgr-{pkg_id}.desktop"
                dest_dt.write_text("\n".join(new_lines) + "\n")
                created.append(dest_dt)
                break
        else:
            # Auto-generate desktop entry if package didn't provide one
            dest_dt = self.config.applications_dir / f"appmgr-{pkg_id}.desktop"
            exec_cmd = f'"{target_bin}" %U' if target_bin else pkg_id
            content = f"""[Desktop Entry]
Name={self.metadata.get('name', pkg_id).capitalize()}
Comment={self.metadata.get('description', '')}
Exec={exec_cmd}
Terminal=false
Type=Application
Categories=Utility;Application;
"""
            dest_dt.write_text(content)
            created.append(dest_dt)

        return created

    def _setup_icons(self, app_dir: Path, pkg_id: str) -> List[Path]:
        """Find and install icons to system / user icon directories."""
        created = []
        icon_extensions = [".png", ".svg", ".xpm"]
        icon_candidates = []

        for ext in icon_extensions:
            icon_candidates.extend(list(app_dir.glob(f"**/*{ext}")))

        # Filter candidate icons
        best_icon = None
        for ic in icon_candidates:
            if pkg_id in ic.name.lower() or "icon" in ic.name.lower():
                best_icon = ic
                break

        if not best_icon and icon_candidates:
            best_icon = icon_candidates[0]

        if best_icon:
            dest_icon_dir = self.config.icons_dir / "hicolor" / "256x256" / "apps"
            dest_icon_dir.mkdir(parents=True, exist_ok=True)
            dest_icon = dest_icon_dir / f"{pkg_id}{best_icon.suffix}"
            shutil.copy2(best_icon, dest_icon)
            created.append(dest_icon)

            # Update desktop file icon entry
            dt_path = self.config.applications_dir / f"appmgr-{pkg_id}.desktop"
            if dt_path.exists():
                lines = dt_path.read_text(encoding="utf-8", errors="replace").splitlines()
                has_icon = any(l.startswith("Icon=") for l in lines)
                new_lines = []
                for l in lines:
                    if l.startswith("Icon="):
                        new_lines.append(f"Icon={dest_icon}")
                    else:
                        new_lines.append(l)
                if not has_icon:
                    new_lines.append(f"Icon={dest_icon}")
                dt_path.write_text("\n".join(new_lines) + "\n")

        return created

    def _count_tar_members(self, tar_path: Path) -> int:
        """Count files in tar archive."""
        cmd = ["tar", "-tf", str(tar_path)]
        if str(tar_path).endswith(".zst"):
            cmd = ["tar", "--zstd", "-tf", str(tar_path)]
        try:
            res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, check=True)
            lines = [l for l in res.stdout.splitlines() if l.strip()]
            return len(lines)
        except Exception:
            return 100  # Fallback estimate

    def _extract_tar(self, tar_path: Path, dest_dir: Path):
        """Extract tar archive."""
        cmd = ["tar", "-xf", str(tar_path), "-C", str(dest_dir)]
        if str(tar_path).endswith(".zst"):
            cmd = ["tar", "--zstd", "-xf", str(tar_path), "-C", str(dest_dir)]
        subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)

    def _extract_tar_with_progress(self, tar_path: Path, dest_dir: Path, bar: PacmanBar):
        """Extract tar archive while updating the Pacman progress bar."""
        cmd = ["tar", "-xvf", str(tar_path), "-C", str(dest_dir)]
        if str(tar_path).endswith(".zst"):
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

    def _ar_extract(self, ar_path: Path, dest_dir: Path):
        """Pure-Python AR archive extraction fallback."""
        with open(ar_path, "rb") as f:
            magic = f.read(8)
            if magic != b"!<arch>\n":
                raise ValueError("Not a valid AR/deb archive")

            while True:
                header = f.read(60)
                if len(header) < 60:
                    break
                filename = header[:16].decode("ascii", errors="replace").strip().rstrip("/")
                file_size = int(header[48:58].decode("ascii", errors="replace").strip())

                data = f.read(file_size)
                if file_size % 2 == 1:
                    f.read(1)  # 2-byte padding

                if filename:
                    (dest_dir / filename).write_bytes(data)

    def _sanitize_name(self, name: str) -> str:
        clean = re.sub(r"[^a-zA-Z0-9_-]", "", name.lower())
        return clean or "package"
