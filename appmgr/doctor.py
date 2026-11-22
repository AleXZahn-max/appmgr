"""
System Diagnostics and Health Check (Doctor) for appmgr.
Audits FUSE, PATH, desktop databases, broken links, and shared library dependencies.
"""

import os
import shutil
import subprocess
from pathlib import Path
from typing import Dict, Any, List

from .config import Config
from .database import Database
from .ui import c, Colors, apt_prefix, print_table


class Doctor:
    def __init__(self, config: Config):
        self.config = config
        self.db = Database(config)

    def run_all_checks(self):
        """Execute full diagnostics suite."""
        print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} {c('Running appmgr system health diagnostics...', Colors.BOLD, Colors.BRIGHT_WHITE)}\n")

        self.check_path_environment()
        self.check_archive_utilities()
        self.check_fuse_support()
        self.check_namespace_virtualization()
        self.check_desktop_utilities()
        self.check_database_integrity()
        self.check_broken_launchers()
        self.check_shared_libraries()
        self.report_disk_usage()

        print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_GREEN)} Diagnostics completed.\n")

    def check_path_environment(self):
        """Verify that bin_dir is in PATH."""
        path_env = os.environ.get("PATH", "").split(":")
        bin_str = str(self.config.bin_dir)

        if bin_str in path_env:
            print(f" {apt_prefix('OK')} PATH includes {c(bin_str, Colors.BOLD, Colors.BRIGHT_CYAN)}")
        else:
            print(f" {apt_prefix('WARN')} PATH does NOT include {c(bin_str, Colors.BOLD, Colors.BRIGHT_YELLOW)}")
            print(f"        To fix, add this to your ~/.bashrc or ~/.zshrc:")
            print(f"        {c(f'export PATH=\"{bin_str}:$PATH\"', Colors.DIM, Colors.WHITE)}")

    def check_archive_utilities(self):
        """Verify decompression utilities for tarball packages (tar, xz, zstd)."""
        tools = [
            ("tar", "Tar archive extraction (required)"),
            ("xz", "XZ decompression (.tar.xz)"),
            ("zstd", "Zstandard decompression (.tar.zst)"),
        ]
        for tool, purpose in tools:
            if shutil.which(tool):
                print(f" {apt_prefix('OK')} {tool} found ({purpose})")
            else:
                print(f" {apt_prefix('WARN')} {tool} missing ({purpose})")

    def check_fuse_support(self):
        """Check FUSE support required for AppImages."""
        has_fusermount = shutil.which("fusermount") or shutil.which("fusermount3")
        
        # Check libfuse.so.2
        has_libfuse2 = False
        try:
            ld_out = subprocess.getoutput("ldconfig -p")
            if "libfuse.so.2" in ld_out:
                has_libfuse2 = True
        except Exception:
            pass

        if has_fusermount and has_libfuse2:
            print(f" {apt_prefix('OK')} FUSE runtime and libfuse.so.2 detected (AppImages run natively)")
        elif has_libfuse2:
            print(f" {apt_prefix('OK')} libfuse.so.2 detected (AppImages supported)")
        else:
            print(f" {apt_prefix('WARN')} libfuse.so.2 not found (appmgr will use --appimage-extract-and-run)")

    def check_namespace_virtualization(self):
        """Check Bubblewrap support for rootless /opt path virtualization."""
        if shutil.which("bwrap"):
            print(f" {apt_prefix('OK')} bubblewrap detected (transparent /opt path virtualization active)")
        else:
            print(f" {apt_prefix('WARN')} bubblewrap not found (hardcoded /opt paths in heavy .deb might require sudo)")

    def check_desktop_utilities(self):
        """Check presence of desktop and icon indexers."""
        tools = [
            ("update-desktop-database", "Desktop database indexing"),
            ("gtk-update-icon-cache", "Icon theme cache updating"),
            ("xdg-desktop-menu", "XDG desktop menu integration"),
        ]
        for tool, purpose in tools:
            if shutil.which(tool):
                print(f" {apt_prefix('OK')} {tool} found ({purpose})")
            else:
                print(f" {apt_prefix('WARN')} {tool} missing ({purpose} will be skipped)")

    def check_database_integrity(self):
        """Verify installed packages registered in DB."""
        installed = self.db.all()
        print(f" {apt_prefix('INFO')} Database tracks {c(str(len(installed)), Colors.BOLD, Colors.BRIGHT_GREEN)} installed applications")

    def check_broken_launchers(self):
        """Detect broken symlinks or orphaned files in bin_dir."""
        broken = []
        if self.config.bin_dir.exists():
            for p in self.config.bin_dir.iterdir():
                if p.is_symlink() and not p.exists():
                    broken.append(p)

        if broken:
            print(f" {apt_prefix('WARN')} Found {len(broken)} broken launchers in {self.config.bin_dir}:")
            for b in broken:
                print(f"        - {b.name}")
        else:
            print(f" {apt_prefix('OK')} No broken launchers found in {self.config.bin_dir}")

    def check_shared_libraries(self):
        """Run ldd check on ELF binaries of installed packages."""
        installed = self.db.all()
        missing_libs: List[str] = []
        static_binaries_count = 0

        for pkg in installed:
            app_dir = Path(pkg.get("install_dir", ""))
            elf_candidates = []

            # Find actual ELF binaries in app_dir
            if app_dir.exists():
                for p in app_dir.glob("**/*"):
                    if p.is_file() and not p.is_symlink():
                        try:
                            with open(p, "rb") as f:
                                if f.read(4) == b"\x7fELF":
                                    elf_candidates.append(p)
                        except Exception:
                            pass

            for elf in elf_candidates:
                try:
                    res = subprocess.getoutput(f"ldd '{elf}' 2>&1")
                    res_lower = res.lower()
                    if "not a dynamic executable" in res_lower or "statically linked" in res_lower:
                        static_binaries_count += 1
                        continue

                    for line in res.splitlines():
                        if "not found" in line:
                            lib = line.strip().split("=>")[0].strip()
                            missing_libs.append(f"{pkg['name']}: {lib}")
                except Exception:
                    pass

        if missing_libs:
            print(f" {apt_prefix('WARN')} Missing shared libraries detected:")
            for m in missing_libs[:5]:
                print(f"        - {m}")
        else:
            static_note = f" ({static_binaries_count} static)" if static_binaries_count > 0 else ""
            print(f" {apt_prefix('OK')} Binary dependencies verified (no missing shared libraries){static_note}")

    def report_disk_usage(self):
        """Calculate and report total disk usage of apps directory."""
        if not self.config.apps_dir.exists():
            return
        total_bytes = sum(f.stat().st_size for f in self.config.apps_dir.glob("**/*") if f.is_file())
        total_mb = total_bytes / (1024 * 1024)
        print(f" {apt_prefix('INFO')} Storage utilized by managed applications: {c(f'{total_mb:.1f} MB', Colors.BOLD, Colors.BRIGHT_CYAN)}")
