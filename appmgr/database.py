"""
State Database for appmgr.
Tracks all installed packages, their files, desktop shortcuts, and symlinks.
"""

import json
import os
from pathlib import Path
from typing import Dict, Any, List, Optional
from .config import DEFAULT_CONFIG, Config


class Database:
    def __init__(self, config: Optional[Config] = None):
        self.config = config or DEFAULT_CONFIG
        self.db_path = self.config.db_file
        self._data: Dict[str, Any] = {}
        self.load()

    def load(self):
        """Load state database from disk."""
        if self.db_path.exists():
            try:
                with open(self.db_path, "r", encoding="utf-8") as f:
                    self._data = json.load(f)
            except Exception:
                self._data = {}
        else:
            self._data = {}

    def save(self):
        """Persist state database to disk."""
        self.config.db_dir.mkdir(parents=True, exist_ok=True)
        with open(self.db_path, "w", encoding="utf-8") as f:
            json.dump(self._data, f, indent=2, ensure_ascii=False)

    def add(self, pkg_record: Dict[str, Any]):
        """Add or update an application record."""
        pkg_id = pkg_record["id"]
        self._data[pkg_id] = pkg_record
        self.save()

    def remove(self, pkg_id: str) -> Optional[Dict[str, Any]]:
        """Remove a package record by ID."""
        if pkg_id in self._data:
            rec = self._data.pop(pkg_id)
            self.save()
            return rec
        return None

    def get(self, pkg_id: str) -> Optional[Dict[str, Any]]:
        """Get record by exact ID or case-insensitive match."""
        if pkg_id in self._data:
            return self._data[pkg_id]
        lower_id = pkg_id.lower()
        for k, v in self._data.items():
            if k.lower() == lower_id or v.get("name", "").lower() == lower_id:
                return v
        return None

    def all(self) -> List[Dict[str, Any]]:
        """Return list of all registered applications."""
        return list(self._data.values())

    def search(self, query: str) -> List[Dict[str, Any]]:
        """Search registered packages by name or description."""
        q = query.lower()
        results = []
        for pkg in self._data.values():
            name = pkg.get("name", "").lower()
            pkg_id = pkg.get("id", "").lower()
            desc = pkg.get("description", "").lower()
            if q in name or q in pkg_id or q in desc:
                results.append(pkg)
        return results

    def link_repo(
        self,
        pkg_id: str,
        repo: str,
        tag: Optional[str] = None,
        etag: Optional[str] = None,
        asset_pattern: Optional[str] = None,
    ) -> bool:
        """Associate an installed application with a GitHub repository."""
        rec = self.get(pkg_id)
        if not rec:
            return False
        rec["repo"] = repo
        if tag:
            rec["tag"] = tag
        if etag:
            rec["etag"] = etag
        if asset_pattern:
            rec["asset_pattern"] = asset_pattern
        self._data[rec["id"]] = rec
        self.save()
        return True

    def set_env(self, pkg_id: str, env_vars: Dict[str, str]) -> Optional[Dict[str, Any]]:
        """Update environment variables for an application."""
        rec = self.get(pkg_id)
        if not rec:
            return None
        if "env" not in rec or not isinstance(rec["env"], dict):
            rec["env"] = {}
        rec["env"].update(env_vars)
        self._data[rec["id"]] = rec
        self.save()
        return rec

    def unset_env(self, pkg_id: str, key: str) -> Optional[Dict[str, Any]]:
        """Unset an environment variable for an application."""
        rec = self.get(pkg_id)
        if not rec:
            return None
        if "env" in rec and isinstance(rec["env"], dict):
            rec["env"].pop(key, None)
        self._data[rec["id"]] = rec
        self.save()
        return rec

    def set_args(self, pkg_id: str, args_list: List[str]) -> Optional[Dict[str, Any]]:
        """Set persistent CLI arguments for an application."""
        rec = self.get(pkg_id)
        if not rec:
            return None
        rec["args"] = args_list
        self._data[rec["id"]] = rec
        self.save()
        return rec

    def clear_args(self, pkg_id: str) -> Optional[Dict[str, Any]]:
        """Clear persistent CLI arguments for an application."""
        rec = self.get(pkg_id)
        if not rec:
            return None
        rec["args"] = []
        self._data[rec["id"]] = rec
        self.save()
        return rec

    def set_pinned(self, pkg_id: str, pinned: bool) -> Optional[Dict[str, Any]]:
        """Set or unset the pinned status of an application."""
        rec = self.get(pkg_id)
        if not rec:
            return None
        rec["pinned"] = pinned
        self._data[rec["id"]] = rec
        self.save()
        return rec
