"""
Configuration and Path Management for appmgr.
Supports both user-level (rootless) and system-wide installation modes.
"""

import os
import sys
from pathlib import Path


def is_root() -> bool:
    """Return True if running with root privileges."""
    return os.geteuid() == 0


class Config:
    def __init__(self, force_system: bool = False, force_user: bool = False):
        if force_system and force_user:
            raise ValueError("Cannot specify both force_system and force_user")

        if force_system:
            self.mode = "system"
        elif force_user:
            self.mode = "user"
        else:
            self.mode = "system" if is_root() else "user"

        home = Path.home()

        if self.mode == "system":
            self.prefix = Path("/usr/local")
            self.apps_dir = Path("/opt/appmgr/apps")
            self.bin_dir = Path("/usr/local/bin")
            self.applications_dir = Path("/usr/local/share/applications")
            self.icons_dir = Path("/usr/local/share/icons")
            self.db_dir = Path("/var/lib/appmgr")
            self.db_file = self.db_dir / "installed.json"
            self.backups_dir = self.db_dir / "backups"
            self.cache_dir = self.db_dir / "cache"
        else:
            self.prefix = home / ".local"
            self.apps_dir = home / ".local/share/appmgr/apps"
            self.bin_dir = home / ".local/bin"
            self.applications_dir = home / ".local/share/applications"
            self.icons_dir = home / ".local/share/icons"
            self.db_dir = home / ".local/share/appmgr"
            self.db_file = self.db_dir / "installed.json"
            self.backups_dir = self.db_dir / "backups"
            self.cache_dir = self.db_dir / "cache"

    def ensure_dirs(self):
        """Ensure all required directories exist."""
        for path in [
            self.apps_dir,
            self.bin_dir,
            self.applications_dir,
            self.icons_dir,
            self.db_dir,
            self.backups_dir,
            self.cache_dir,
        ]:
            path.mkdir(parents=True, exist_ok=True)


# Default config instance based on current execution context
DEFAULT_CONFIG = Config()
