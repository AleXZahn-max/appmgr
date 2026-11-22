"""
Core Controller and Installation Engine for appmgr.
Implements APT+Pacman hybrid UI, transaction orchestration,
safety checks, rollback, isolation, environment management, and multi-threaded upgrades.
"""

import concurrent.futures
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple, Union

from .appimage import AppImagePackage
from .config import Config, DEFAULT_CONFIG
from .database import Database
from .deb import DebPackage
from .github import GitHubClient, GitHubRateLimitError
from .standalone import StandaloneBinaryPackage
from .tarball import TarballPackage
from .ui import (
    Colors,
    PacmanBar,
    Spinner,
    apt_prefix,
    apt_preflight_animation,
    c,
    confirm_dialog,
    pacman_prompt,
    print_apt_transaction_summary,
    print_apt_upgrade_summary,
    print_banner,
    print_search_results,
    print_table,
    prompt_select_asset,
    prompt_select_repository,
)
from .zipball import ZipPackage


class Core:
    def __init__(self, config: Optional[Config] = None):
        self.config = config or DEFAULT_CONFIG
        self.config.ensure_dirs()
        self.db = Database(self.config)
        self.github = GitHubClient()

    def install(
        self,
        target_str: str,
        force: bool = False,
        assume_yes: bool = False,
        repo: Optional[str] = None,
        prefer: Optional[str] = None,
        include_pre: bool = False,
    ) -> bool:
        """
        Install package with target auto-detection:
        - Local file -> install locally (.deb, .AppImage, .tar.*, .zip, standalone ELF)
        - URL or owner/repo[@tag] format -> install directly from GitHub release
        - Application name / query -> search GitHub repositories and interactively install
        """
        # 1. Local file installation
        local_path = Path(target_str).expanduser()
        if local_path.exists() and local_path.is_file():
            return self._install_local_file(
                local_path.resolve(), force=force, assume_yes=assume_yes, repo=repo
            )

        # 2. Direct GitHub repository or URL (with optional @tag)
        parsed_repo, tag = GitHubClient.parse_repo_and_tag(target_str)
        if (
            target_str.startswith("github:")
            or target_str.startswith("http://")
            or target_str.startswith("https://")
            or ("/" in target_str and parsed_repo)
        ):
            if not parsed_repo:
                print(f"{apt_prefix('FAIL')} Invalid GitHub repository format: {c(target_str, Colors.BOLD, Colors.BRIGHT_RED)}")
                return False
            return self._install_from_github(
                parsed_repo,
                tag=tag,
                force=force,
                assume_yes=assume_yes,
                prefer=prefer,
                include_pre=include_pre,
            )

        # 3. Application name / search query via GitHub Search API
        return self._search_and_install(
            target_str, force=force, assume_yes=assume_yes, prefer=prefer, include_pre=include_pre
        )

    def _install_from_github(
        self,
        repo_str: str,
        tag: Optional[str] = None,
        force: bool = False,
        assume_yes: bool = False,
        prefer: Optional[str] = None,
        include_pre: bool = False,
    ) -> bool:
        """Fetch release from GitHub and install matching asset."""
        tag_disp = f" ({c(tag, Colors.BOLD, Colors.BRIGHT_YELLOW)})" if tag else ""
        print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} Fetching release metadata from GitHub: {c(repo_str, Colors.BOLD, Colors.BRIGHT_CYAN)}{tag_disp}")
        apt_preflight_animation()

        with Spinner(f"Querying GitHub API for {repo_str}..."):
            try:
                status, release, etag, is_pre = self.github.get_release(
                    repo_str, tag=tag, include_pre=include_pre
                )
            except GitHubRateLimitError as e:
                print(f"{apt_prefix('FAIL')} {e}")
                return False
            except Exception as e:
                print(f"{apt_prefix('FAIL')} Failed to reach GitHub: {e}")
                return False

        if status == 404 or not release:
            print(f"{apt_prefix('FAIL')} No suitable release found for repository '{repo_str}'.")
            return False

        tag_name = release.get("tag_name", "unknown")

        # Prompt if we fell back to a pre-release without explicit --pre
        if is_pre and not include_pre and not tag and not assume_yes:
            print(f"\n{apt_prefix('INFO')} Only pre-release builds found for {repo_str} ({c(tag_name, Colors.BOLD, Colors.BRIGHT_YELLOW)}).")
            if not confirm_dialog(f"Install pre-release {tag_name}?", default=True):
                print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} Installation aborted.")
                return False

        raw_assets = release.get("assets", [])
        if not raw_assets:
            print(f"{apt_prefix('FAIL')} Release {tag_name} has no binary assets.")
            return False

        # Filter assets for host architecture and OS
        app_name_hint = repo_str.split("/")[-1]
        candidates = self.github.filter_assets(raw_assets, prefer=prefer, app_name_hint=app_name_hint)
        if not candidates:
            import platform
            host_arch = platform.machine()
            print(f"{apt_prefix('FAIL')} No compatible binary assets found in {repo_str} ({tag_name}) for {host_arch}.")
            return False

        return self._download_and_install_asset(
            repo_str, release, candidates, etag, force=force, assume_yes=assume_yes
        )

    def _search_and_install(
        self,
        query: str,
        force: bool = False,
        assume_yes: bool = False,
        prefer: Optional[str] = None,
        include_pre: bool = False,
    ) -> bool:
        """Search GitHub for repositories matching query and interactively install chosen one."""
        print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} Searching GitHub repositories for '{c(query, Colors.BOLD, Colors.BRIGHT_CYAN)}'...")
        with Spinner(f"Querying GitHub Search API for '{query}'..."):
            try:
                repos = self.github.search_repositories(query, per_page=8)
            except GitHubRateLimitError as e:
                print(f"{apt_prefix('FAIL')} {e}")
                return False
            except Exception as e:
                print(f"{apt_prefix('FAIL')} Search failed: {e}")
                return False

        if not repos:
            print(f"{apt_prefix('FAIL')} No repositories found matching '{query}'.")
            return False

        # Interactive repository selection loop
        remaining_repos = list(repos)
        while remaining_repos:
            chosen = prompt_select_repository(remaining_repos, query)
            if not chosen:
                print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} Installation cancelled.")
                return False

            repo_name = chosen["full_name"]
            print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} Checking releases for {c(repo_name, Colors.BOLD, Colors.BRIGHT_CYAN)}...")

            with Spinner(f"Fetching release for {repo_name}..."):
                try:
                    status, release, etag, is_pre = self.github.get_release(
                        repo_name, include_pre=include_pre
                    )
                except GitHubRateLimitError as e:
                    print(f"{apt_prefix('FAIL')} {e}")
                    return False
                except Exception as e:
                    print(f" {apt_prefix('WARN')} Error checking {repo_name}: {e}")
                    status, release, etag, is_pre = 500, None, None, False

            if status == 404 or not release:
                print(f" {apt_prefix('WARN')} Repository '{c(repo_name, Colors.BOLD, Colors.YELLOW)}' has no releases.")
                print(f"        Please select another repository from the list.\n")
                continue

            tag_name = release.get("tag_name", "unknown")
            raw_assets = release.get("assets", [])
            app_hint = repo_name.split("/")[-1]
            candidates = self.github.filter_assets(raw_assets, prefer=prefer, app_name_hint=app_hint)

            if not candidates:
                import platform
                host_arch = platform.machine()
                print(f" {apt_prefix('WARN')} Repository '{c(repo_name, Colors.BOLD, Colors.YELLOW)}' ({tag_name}) has no compatible assets for {host_arch}.")
                print(f"        Please select another repository from the list.\n")
                continue

            apt_preflight_animation()
            return self._download_and_install_asset(
                repo_name, release, candidates, etag, force=force, assume_yes=assume_yes
            )

        print(f"{apt_prefix('FAIL')} No suitable repositories remaining.")
        return False

    def _download_and_install_asset(
        self,
        repo_str: str,
        release: Dict[str, Any],
        candidates: List[Dict[str, Any]],
        etag: Optional[str],
        force: bool = False,
        assume_yes: bool = False,
    ) -> bool:
        """Download chosen release asset and trigger local installation with SHA-256 validation."""
        tag_name = release.get("tag_name", "unknown")

        if len(candidates) == 1 or assume_yes:
            chosen = candidates[0]
        else:
            chosen = prompt_select_asset(candidates)

        if not chosen:
            print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} Installation aborted.")
            return False

        asset_name = chosen["name"]
        asset_url = chosen["url"]
        asset_size = chosen["size"]

        temp_dir = Path(tempfile.mkdtemp(prefix="appmgr_gh_"))
        local_archive = temp_dir / asset_name

        try:
            print()
            self.github.download_asset(
                asset_url,
                local_archive,
                expected_size=asset_size,
                asset_name=asset_name,
                chosen_asset=chosen,
                release_assets=release.get("assets", []),
            )

            pattern = self.github.derive_pattern(asset_name, tag_name)

            success = self._install_local_file(
                local_archive,
                force=force,
                assume_yes=assume_yes,
                repo=repo_str,
                github_tag=tag_name,
                github_etag=etag,
                asset_pattern=pattern,
            )
            return success
        finally:
            shutil.rmtree(temp_dir, ignore_errors=True)

    def _install_local_file(
        self,
        path: Path,
        force: bool = False,
        assume_yes: bool = False,
        repo: Optional[str] = None,
        github_tag: Optional[str] = None,
        github_etag: Optional[str] = None,
        asset_pattern: Optional[str] = None,
        env: Optional[Dict[str, str]] = None,
        args: Optional[List[str]] = None,
        pinned: bool = False,
    ) -> bool:
        """Internal helper for extracting and registering package files."""
        pkg_type = self._detect_package_type(path)

        if pkg_type == "arch_pkg":
            print(f"{apt_prefix('INFO')} '{path.name}' is an Arch Linux native package (.pkg.tar.*).")
            print(f"        To install it into your system, run:")
            print(f"        {c(f'sudo pacman -U {path}', Colors.BOLD, Colors.BRIGHT_YELLOW)}\n")
            return False

        if not pkg_type:
            print(f"{apt_prefix('FAIL')} Unsupported package format. Supported: .AppImage, .deb, .tar.*, .zip, standalone ELF")
            return False

        # 1. APT Preflight simulation
        apt_preflight_animation()

        # 2. Extract metadata
        with Spinner("Analyzing package metadata and architecture..."):
            try:
                if pkg_type == "deb":
                    pkg_handler = DebPackage(path, self.config)
                elif pkg_type == "appimage":
                    pkg_handler = AppImagePackage(path, self.config)
                elif pkg_type == "zip":
                    pkg_handler = ZipPackage(path, self.config, repo=repo, tag=github_tag)
                elif pkg_type == "bin":
                    pkg_handler = StandaloneBinaryPackage(path, self.config, repo=repo, tag=github_tag)
                else:
                    pkg_handler = TarballPackage(path, self.config, repo=repo, tag=github_tag)
                meta = pkg_handler.read_metadata()
            except Exception as e:
                print(f"{apt_prefix('FAIL')} Error reading package: {e}")
                return False

        # 3. APT Transaction Summary
        print_apt_transaction_summary([meta])

        # 4. Pacman Confirmation
        if not assume_yes:
            if not confirm_dialog("Proceed with installation?", default=True):
                print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} Installation aborted by user.")
                return False

        start_time = time.time()
        print()
        print(f"Selecting previously unselected package {c(meta['name'], Colors.BOLD, Colors.BRIGHT_WHITE)}.")
        installed_count = len(self.db.all())
        print(f"(Reading database ... {installed_count} packages currently installed.)")
        print(f"Preparing to unpack {path.name} ...")
        print(f"Unpacking {c(meta['name'], Colors.BOLD, Colors.BRIGHT_CYAN)} ({meta['version']}) ...")

        # 5. Execute Installation with Pacman progress bar
        try:
            record = pkg_handler.install(force=force, env=env, args=args)
        except Exception as e:
            print(f"\n{apt_prefix('FAIL')} Installation failed: {e}")
            return False

        # Attach GitHub metadata if supplied
        if repo:
            clean_repo = GitHubClient.parse_repo(repo) or repo
            record["repo"] = clean_repo
            if github_tag:
                record["tag"] = github_tag
            if github_etag:
                record["etag"] = github_etag
            if asset_pattern:
                record["asset_pattern"] = asset_pattern
            elif not asset_pattern and github_tag:
                record["asset_pattern"] = self.github.derive_pattern(path.name, github_tag)

        if env:
            record["env"] = env
        if args:
            record["args"] = args
        if pinned:
            record["pinned"] = pinned

        # 6. Setting up step
        print(f"Setting up {c(record['name'], Colors.BOLD, Colors.BRIGHT_GREEN)} ({record['version']}) ...")
        for b in record.get("binaries", []):
            print(f" {apt_prefix('OK')} Installed launcher: {c(b, Colors.BOLD, Colors.BRIGHT_WHITE)}")
        for d in record.get("desktop_files", []):
            print(f" {apt_prefix('OK')} Installed desktop entry: {c(d, Colors.DIM, Colors.WHITE)}")
        for i in record.get("icons", []):
            print(f" {apt_prefix('OK')} Installed icon: {c(i, Colors.DIM, Colors.WHITE)}")

        # 7. Processing triggers (APT style)
        self.run_triggers()

        # 8. Register in database
        self.db.add(record)

        elapsed = time.time() - start_time
        print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_GREEN)} Transaction completed successfully in {elapsed:.2f}s.")
        print(f"   You can now launch it by typing: {c(record['id'], Colors.BOLD, Colors.BRIGHT_YELLOW)}\n")
        return True

    def link(self, app_query: str, repo_str: str) -> bool:
        """Associate an installed application with a GitHub repository."""
        pkg = self.db.get(app_query)
        if not pkg:
            print(f"{apt_prefix('FAIL')} Package '{app_query}' is not managed by appmgr.")
            return False

        parsed_repo = GitHubClient.parse_repo(repo_str)
        if not parsed_repo:
            print(f"{apt_prefix('FAIL')} Invalid repository format: '{repo_str}'. Use owner/repo or github:owner/repo.")
            return False

        with Spinner(f"Validating GitHub repository {parsed_repo}..."):
            try:
                status, release, etag, _ = self.github.get_release(parsed_repo)
            except Exception as e:
                print(f"{apt_prefix('FAIL')} Failed to connect to GitHub: {e}")
                return False

        if status == 404 or not release:
            print(f"{apt_prefix('WARN')} Repository '{parsed_repo}' has no releases yet.")
            self.db.link_repo(pkg["id"], parsed_repo)
            print(f" {apt_prefix('OK')} Linked {pkg['name']} -> {c(parsed_repo, Colors.BOLD, Colors.BRIGHT_CYAN)} (pending first release).")
            return True

        tag_name = release.get("tag_name", "")
        raw_assets = release.get("assets", [])
        candidates = self.github.filter_assets(raw_assets, prefer=pkg.get("format"))
        pattern = None
        if candidates:
            pattern = self.github.derive_pattern(candidates[0]["name"], tag_name)

        self.db.link_repo(pkg["id"], parsed_repo, tag=tag_name, etag=etag, asset_pattern=pattern)
        print(f" {apt_prefix('OK')} Successfully linked {c(pkg['name'], Colors.BOLD, Colors.BRIGHT_WHITE)} to {c(parsed_repo, Colors.BOLD, Colors.BRIGHT_CYAN)} ({tag_name})")
        return True

    def upgrade(
        self,
        app_query: Optional[str] = None,
        check_only: bool = False,
        assume_yes: bool = False,
        prefer: Optional[str] = None,
        include_pre: bool = False,
        force: bool = False,
        as_json: bool = False,
    ) -> bool:
        """
        Check and upgrade packages from GitHub Releases.
        Uses multithreaded checks, ETag caching, preserved env/args, version pinning,
        and automatic rollback cache.
        """
        if app_query:
            target_pkgs = [self.db.get(app_query)]
            if not target_pkgs[0]:
                if as_json:
                    print(json.dumps({"error": f"Package '{app_query}' not found"}))
                else:
                    print(f"{apt_prefix('FAIL')} Package '{app_query}' is not managed by appmgr.")
                return False
        else:
            target_pkgs = self.db.all()

        linked_pkgs = [p for p in target_pkgs if p.get("repo")]
        if not linked_pkgs:
            if as_json:
                print(json.dumps({"upgrades": [], "up_to_date": [], "pinned": []}))
                return True
            if app_query:
                print(f"{apt_prefix('WARN')} Package '{app_query}' is not linked to any GitHub repository.")
            else:
                print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} No installed packages are linked to GitHub repositories.")
            return False

        if not as_json:
            print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} {c('Synchronizing package databases...', Colors.BOLD, Colors.BRIGHT_WHITE)}")
            apt_preflight_animation()

        # Multithreaded check using ThreadPoolExecutor
        def check_single_package(pkg):
            name = pkg.get("name", pkg["id"])
            repo = pkg["repo"]
            saved_etag = pkg.get("etag")
            saved_tag = pkg.get("tag", "")
            current_ver = pkg.get("version", "")
            saved_pattern = pkg.get("asset_pattern")
            is_pinned = pkg.get("pinned", False)

            if is_pinned and not force and not app_query:
                return {"type": "pinned", "pkg": pkg, "name": name, "version": current_ver}

            try:
                status, release, new_etag, is_pre = self.github.get_release(
                    repo, include_pre=include_pre, etag=saved_etag
                )
            except Exception as e:
                return {"type": "error", "pkg": pkg, "name": name, "error": str(e)}

            if status == 304:
                return {"type": "up_to_date", "pkg": pkg, "name": name, "version": current_ver, "etag304": True}

            if status == 200 and release:
                new_tag = release.get("tag_name", "")
                raw_assets = release.get("assets", [])
                is_newer = (new_tag != saved_tag) and (new_tag.lstrip("v") != current_ver.lstrip("v"))

                if is_newer:
                    candidates = self.github.filter_assets(
                        raw_assets, prefer=prefer or pkg.get("format"), saved_pattern=saved_pattern
                    )
                    if candidates:
                        chosen_asset = candidates[0]
                        return {
                            "type": "upgrade",
                            "pkg": pkg,
                            "name": name,
                            "id": pkg["id"],
                            "old_version": current_ver,
                            "new_version": new_tag.lstrip("v"),
                            "repo": repo,
                            "new_tag": new_tag,
                            "new_etag": new_etag,
                            "format": chosen_asset["format"],
                            "asset": chosen_asset,
                            "release_assets": raw_assets,
                            "size": chosen_asset["size"],
                            "asset_pattern": saved_pattern or self.github.derive_pattern(chosen_asset["name"], new_tag),
                        }

            return {"type": "up_to_date", "pkg": pkg, "name": name, "version": current_ver, "etag304": False}

        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
            check_results = list(executor.map(check_single_package, linked_pkgs))

        upgrades = []
        up_to_date = []
        pinned = []

        for r in check_results:
            rtype = r.get("type")
            if rtype == "upgrade":
                upgrades.append(r)
            elif rtype == "pinned":
                pinned.append(r)
            elif rtype == "up_to_date":
                up_to_date.append(r)

        # JSON mode
        if as_json:
            json_summary = {
                "upgrades": [
                    {
                        "id": u["id"],
                        "name": u["name"],
                        "current_version": u["old_version"],
                        "latest_version": u["new_version"],
                        "repo": u["repo"],
                        "format": u["format"],
                    }
                    for u in upgrades
                ],
                "up_to_date": [{"name": ut["name"], "version": ut["version"]} for ut in up_to_date],
                "pinned": [{"name": p["name"], "version": p["version"]} for p in pinned],
            }
            print(json.dumps(json_summary, indent=2))
            return True

        # CLI Display
        for p in pinned:
            print(f" {apt_prefix('INFO')} {c(p['name'], Colors.BOLD, Colors.BRIGHT_MAGENTA)} is pinned to {p['version']} {c('[PINNED]', Colors.BOLD, Colors.YELLOW)} (use --force to upgrade)")

        for ut in up_to_date:
            note = f" {c('(304 Not Modified)', Colors.DIM, Colors.WHITE)}" if ut.get("etag304") else ""
            if check_only or app_query:
                print(f" {apt_prefix('OK')} {c(ut['name'], Colors.BOLD, Colors.WHITE)} is up to date ({ut['version']}){note}")

        for u in upgrades:
            print(f" {apt_prefix('INFO')} {c(u['name'], Colors.BOLD, Colors.BRIGHT_GREEN)}: update available ({u['old_version']} -> {c(u['new_version'], Colors.BOLD, Colors.BRIGHT_YELLOW)}) [{u['repo']}]")

        if check_only:
            print()
            if upgrades:
                print(f"{c('::', Colors.BOLD, Colors.BRIGHT_YELLOW)} {len(upgrades)} package(s) can be upgraded. Run {c('appmgr upgrade', Colors.BOLD, Colors.BRIGHT_GREEN)} to apply updates.\n")
            else:
                print(f"{c('::', Colors.BOLD, Colors.BRIGHT_GREEN)} All packages are up to date.\n")
            return True

        if not upgrades:
            print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_GREEN)} All packages are up to date. Nothing to do.\n")
            return True

        # Print APT Upgrade Summary
        print_apt_upgrade_summary(upgrades)

        if not assume_yes:
            if not confirm_dialog("Proceed with upgrade?", default=True):
                print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} Upgrade cancelled by user.")
                return False

        # Execute upgrades with automatic rollback and persistent cache
        success_count = 0
        for item in upgrades:
            pkg = item["pkg"]
            name = item["name"]
            pkg_id = item["id"]
            asset_info = item["asset"]
            asset_name = asset_info["name"]
            asset_url = asset_info["url"]
            asset_size = asset_info["size"]
            new_tag = item["new_tag"]
            new_etag = item["new_etag"]
            pattern = item["asset_pattern"]
            release_assets = item.get("release_assets", [])

            print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} Upgrading {c(name, Colors.BOLD, Colors.BRIGHT_CYAN)} ({item['old_version']} -> {item['new_version']}) ...")

            temp_dir = Path(tempfile.mkdtemp(prefix="appmgr_upg_"))
            downloaded_file = temp_dir / asset_name

            # Prepare persistent rollback cache
            backup_pkg_dir = self.config.backups_dir / pkg_id
            install_dir = Path(pkg.get("install_dir", ""))

            try:
                # 1. Download asset with SHA-256 verification
                self.github.download_asset(
                    asset_url,
                    downloaded_file,
                    expected_size=asset_size,
                    asset_name=asset_name,
                    chosen_asset=asset_info,
                    release_assets=release_assets,
                )

                # 2. Backup current version into persistent rollback cache
                if install_dir.exists():
                    if backup_pkg_dir.exists():
                        shutil.rmtree(backup_pkg_dir, ignore_errors=True)
                    backup_pkg_dir.mkdir(parents=True, exist_ok=True)
                    backup_files_dir = backup_pkg_dir / "files"
                    shutil.copytree(install_dir, backup_files_dir)
                    # Save metadata
                    (backup_pkg_dir / "meta.json").write_text(json.dumps(pkg, indent=2))

                # 3. Detect format and install
                pkg_type = self._detect_package_type(downloaded_file)
                saved_env = pkg.get("env", {})
                saved_args = pkg.get("args", [])
                saved_pinned = pkg.get("pinned", False)

                if pkg_type == "deb":
                    handler = DebPackage(downloaded_file, self.config)
                elif pkg_type == "appimage":
                    handler = AppImagePackage(downloaded_file, self.config)
                elif pkg_type == "zip":
                    handler = ZipPackage(downloaded_file, self.config, repo=item["repo"], tag=new_tag)
                elif pkg_type == "bin":
                    handler = StandaloneBinaryPackage(downloaded_file, self.config, repo=item["repo"], tag=new_tag)
                elif pkg_type == "tar":
                    handler = TarballPackage(downloaded_file, self.config, repo=item["repo"], tag=new_tag)
                else:
                    raise ValueError(f"Unsupported package format during upgrade: {downloaded_file.name}")

                new_record = handler.install(force=True, env=saved_env, args=saved_args)

                # 4. Attach GitHub metadata, env, args, pin
                new_record["repo"] = item["repo"]
                new_record["tag"] = new_tag
                if new_etag:
                    new_record["etag"] = new_etag
                new_record["asset_pattern"] = pattern
                new_record["env"] = saved_env
                new_record["args"] = saved_args
                new_record["pinned"] = saved_pinned

                # Update database
                self.db.add(new_record)
                self.run_triggers()

                print(f" {apt_prefix('OK')} {name} upgraded to {c(item['new_version'], Colors.BOLD, Colors.BRIGHT_GREEN)}")
                success_count += 1

            except Exception as e:
                print(f"\n{apt_prefix('FAIL')} Upgrade failed for {name}: {e}")
                # Atomic Rollback
                backup_files = backup_pkg_dir / "files"
                if backup_files.exists():
                    print(f" {apt_prefix('WORK')} Initiating rollback to previous version...")
                    if install_dir.exists():
                        shutil.rmtree(install_dir, ignore_errors=True)
                    shutil.copytree(backup_files, install_dir)
                    self._regenerate_launcher(pkg)
                    print(f" {apt_prefix('DONE')} Rollback successful. Previous version restored.")
            finally:
                shutil.rmtree(temp_dir, ignore_errors=True)

        print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_GREEN)} Upgrade complete. {success_count}/{len(upgrades)} packages upgraded successfully.\n")
        return success_count > 0

    def rollback(self, app_id: str, assume_yes: bool = False) -> bool:
        """Rollback an application to its saved previous version."""
        pkg = self.db.get(app_id)
        if not pkg:
            print(f"{apt_prefix('FAIL')} Package '{app_id}' is not managed by appmgr.")
            return False

        real_id = pkg["id"]
        backup_pkg_dir = self.config.backups_dir / real_id
        meta_file = backup_pkg_dir / "meta.json"
        files_dir = backup_pkg_dir / "files"

        if not backup_pkg_dir.exists() or not meta_file.exists() or not files_dir.exists():
            print(f"{apt_prefix('FAIL')} No rollback backup found for '{pkg['name']}'.")
            return False

        try:
            prev_meta = json.loads(meta_file.read_text())
        except Exception as e:
            print(f"{apt_prefix('FAIL')} Corrupted rollback metadata: {e}")
            return False

        prev_version = prev_meta.get("version", "previous")
        current_version = pkg.get("version", "current")

        print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_YELLOW)} Rollback application: {c(pkg['name'], Colors.BOLD, Colors.BRIGHT_WHITE)}")
        print(f"   Current version:  {c(current_version, Colors.BOLD, Colors.BRIGHT_RED)}")
        print(f"   Rollback version: {c(prev_version, Colors.BOLD, Colors.BRIGHT_GREEN)}")

        if not assume_yes:
            if not confirm_dialog(f"Are you sure you want to rollback {pkg['name']} to {prev_version}?", default=True):
                print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} Rollback cancelled.")
                return False

        install_dir = Path(pkg.get("install_dir", ""))
        if install_dir.exists():
            shutil.rmtree(install_dir, ignore_errors=True)

        # Restore files
        shutil.copytree(files_dir, install_dir)

        # Regenerate launcher
        self._regenerate_launcher(prev_meta)

        # Restore database record
        self.db.add(prev_meta)
        self.run_triggers()

        # Remove used rollback backup
        shutil.rmtree(backup_pkg_dir, ignore_errors=True)

        print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_GREEN)} Successfully rolled back {pkg['name']} to version {prev_version}.\n")
        return True

    def clean(self, assume_yes: bool = False) -> bool:
        """Clean up rollback backups, temporary caches, and orphaned files."""
        freed_bytes = 0

        # 1. Backups
        backup_bytes = 0
        if self.config.backups_dir.exists():
            for p in self.config.backups_dir.glob("**/*"):
                if p.is_file():
                    backup_bytes += p.stat().st_size

        # 2. Temp directories
        temp_bytes = 0
        tmp_candidates = list(Path("/tmp").glob("appmgr_*"))
        for td in tmp_candidates:
            if td.is_dir():
                for p in td.glob("**/*"):
                    if p.is_file():
                        temp_bytes += p.stat().st_size

        total_bytes = backup_bytes + temp_bytes
        total_mb = total_bytes / (1024 * 1024)

        print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} AppMgr Cache Cleanup:")
        print(f"   Rollback backups:   {backup_bytes / (1024*1024):.1f} MB")
        print(f"   Temporary files:    {temp_bytes / (1024*1024):.1f} MB")
        print(f"   Estimated space:    {c(f'{total_mb:.1f} MB', Colors.BOLD, Colors.BRIGHT_GREEN)}")

        if total_bytes == 0:
            print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_GREEN)} Cache is already clean. Nothing to do.\n")
            return True

        if not assume_yes:
            if not confirm_dialog("Proceed with cleanup?", default=True):
                print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} Cleanup cancelled.")
                return False

        if self.config.backups_dir.exists():
            shutil.rmtree(self.config.backups_dir, ignore_errors=True)
            self.config.backups_dir.mkdir(parents=True, exist_ok=True)

        for td in tmp_candidates:
            shutil.rmtree(td, ignore_errors=True)

        print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_GREEN)} Cache cleaned successfully. Freed {total_mb:.1f} MB.\n")
        return True

    def export_manifest(self, output_path: Optional[str] = None) -> bool:
        """Export installed packages into a JSON manifest file."""
        all_pkgs = self.db.all()
        manifest_data = {
            "appmgr_version": "1.4.0",
            "export_date": subprocess.getoutput("date -Iseconds"),
            "applications": [],
        }

        for p in all_pkgs:
            manifest_data["applications"].append(
                {
                    "id": p.get("id"),
                    "name": p.get("name"),
                    "version": p.get("version"),
                    "format": p.get("format"),
                    "repo": p.get("repo"),
                    "pinned": p.get("pinned", False),
                    "env": p.get("env", {}),
                    "args": p.get("args", []),
                    "is_local": not bool(p.get("repo")),
                }
            )

        out_file = Path(output_path).expanduser().resolve() if output_path else Path("appmgr-manifest.json")
        try:
            out_file.write_text(json.dumps(manifest_data, indent=2, ensure_ascii=False) + "\n")
            print(f"{c('::', Colors.BOLD, Colors.BRIGHT_GREEN)} Exported {len(all_pkgs)} applications to: {c(str(out_file), Colors.BOLD, Colors.BRIGHT_YELLOW)}")
            return True
        except Exception as e:
            print(f"{apt_prefix('FAIL')} Failed to export manifest: {e}")
            return False

    def import_manifest(self, manifest_path: str, assume_yes: bool = False) -> bool:
        """Import applications from a JSON manifest file."""
        mf_path = Path(manifest_path).expanduser().resolve()
        if not mf_path.exists():
            print(f"{apt_prefix('FAIL')} Manifest file not found: {manifest_path}")
            return False

        try:
            data = json.loads(mf_path.read_text(encoding="utf-8"))
        except Exception as e:
            print(f"{apt_prefix('FAIL')} Failed to parse manifest JSON: {e}")
            return False

        apps = data.get("applications", [])
        if not apps:
            print(f"{apt_prefix('WARN')} Manifest contains no applications.")
            return False

        print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} Importing {len(apps)} applications from {mf_path.name}...")

        installed_count = 0
        skipped_count = 0
        local_apps = []

        for app in apps:
            app_id = app.get("id")
            repo = app.get("repo")
            name = app.get("name", app_id)

            if self.db.get(app_id):
                print(f" {apt_prefix('OK')} {name} is already installed, skipping.")
                skipped_count += 1
                continue

            if not repo:
                local_apps.append(app)
                continue

            print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_CYAN)} Installing {name} from {repo} ...")
            prefer = app.get("format")
            env = app.get("env", {})
            args = app.get("args", [])
            pinned = app.get("pinned", False)

            ok = self._install_from_github(
                repo, force=True, assume_yes=assume_yes, prefer=prefer
            )
            if ok:
                installed_count += 1
                rec = self.db.get(app_id)
                if rec:
                    rec["env"] = env
                    rec["args"] = args
                    rec["pinned"] = pinned
                    self.db.add(rec)
                    self._regenerate_launcher(rec)

        print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_GREEN)} Manifest import finished: {installed_count} installed, {skipped_count} skipped.")
        if local_apps:
            print(f"\n{apt_prefix('INFO')} The following applications were originally installed from local files and must be installed manually:")
            for la in local_apps:
                print(f"        - {la.get('name')} ({la.get('version')}) [{la.get('format', '').upper()}]")
            print()

        return True

    def env_manage(
        self,
        app_id: str,
        pairs: Optional[List[str]] = None,
        unset_key: Optional[str] = None,
        list_only: bool = False,
    ) -> bool:
        """Manage environment variables for an application."""
        pkg = self.db.get(app_id)
        if not pkg:
            print(f"{apt_prefix('FAIL')} Package '{app_id}' is not managed by appmgr.")
            return False

        current_env = pkg.get("env", {})

        if list_only or (not pairs and not unset_key):
            print(f"\n{c('Environment variables for', Colors.BOLD, Colors.BRIGHT_WHITE)} {c(pkg['name'], Colors.BOLD, Colors.BRIGHT_CYAN)}:")
            if not current_env:
                print("  No custom environment variables set.")
            else:
                for k, v in current_env.items():
                    print(f"  {c(k, Colors.BOLD, Colors.BRIGHT_YELLOW)}={v}")
            print()
            return True

        if unset_key:
            rec = self.db.unset_env(pkg["id"], unset_key)
            if rec:
                self._regenerate_launcher(rec)
            print(f" {apt_prefix('OK')} Unset {c(unset_key, Colors.BOLD, Colors.BRIGHT_YELLOW)} for {pkg['name']}.")
            return True

        if pairs:
            new_vars = {}
            for p in pairs:
                if "=" in p:
                    k, v = p.split("=", 1)
                    new_vars[k.strip()] = v.strip()
                else:
                    print(f" {apt_prefix('WARN')} Ignoring invalid pair: '{p}' (expected KEY=VALUE)")

            if new_vars:
                rec = self.db.set_env(pkg["id"], new_vars)
                if rec:
                    self._regenerate_launcher(rec)
                print(f" {apt_prefix('OK')} Updated environment variables for {pkg['name']}: {', '.join(new_vars.keys())}")
            return True

        return True

    def args_manage(
        self,
        app_id: str,
        args_list: Optional[List[str]] = None,
        clear: bool = False,
        list_only: bool = False,
    ) -> bool:
        """Manage persistent runtime arguments for an application."""
        pkg = self.db.get(app_id)
        if not pkg:
            print(f"{apt_prefix('FAIL')} Package '{app_id}' is not managed by appmgr.")
            return False

        current_args = pkg.get("args", [])

        if list_only or (not args_list and not clear):
            print(f"\n{c('Persistent launch arguments for', Colors.BOLD, Colors.BRIGHT_WHITE)} {c(pkg['name'], Colors.BOLD, Colors.BRIGHT_CYAN)}:")
            if not current_args:
                print("  No persistent arguments configured.")
            else:
                print(f"  {c(' '.join(current_args), Colors.BOLD, Colors.BRIGHT_YELLOW)}")
            print()
            return True

        if clear:
            rec = self.db.clear_args(pkg["id"])
            if rec:
                self._regenerate_launcher(rec)
            print(f" {apt_prefix('OK')} Cleared persistent arguments for {pkg['name']}.")
            return True

        if args_list:
            rec = self.db.set_args(pkg["id"], args_list)
            if rec:
                self._regenerate_launcher(rec)
            print(f" {apt_prefix('OK')} Updated launch arguments for {pkg['name']}: {c(' '.join(args_list), Colors.BOLD, Colors.BRIGHT_YELLOW)}")
            return True

        return True

    def pin_app(self, app_id: str) -> bool:
        """Pin application to prevent general upgrades."""
        pkg = self.db.get(app_id)
        if not pkg:
            print(f"{apt_prefix('FAIL')} Package '{app_id}' is not managed by appmgr.")
            return False

        self.db.set_pinned(pkg["id"], True)
        print(f" {apt_prefix('OK')} Application '{c(pkg['name'], Colors.BOLD, Colors.BRIGHT_WHITE)}' pinned to version {pkg.get('version')} {c('[PINNED]', Colors.BOLD, Colors.BRIGHT_YELLOW)}. General upgrades will skip it.")
        return True

    def unpin_app(self, app_id: str) -> bool:
        """Unpin application to allow updates."""
        pkg = self.db.get(app_id)
        if not pkg:
            print(f"{apt_prefix('FAIL')} Package '{app_id}' is not managed by appmgr.")
            return False

        self.db.set_pinned(pkg["id"], False)
        print(f" {apt_prefix('OK')} Application '{c(pkg['name'], Colors.BOLD, Colors.BRIGHT_WHITE)}' unpinned. Updates enabled.")
        return True

    def _regenerate_launcher(self, pkg: Dict[str, Any]):
        """Regenerate wrapper script with updated env vars and persistent args."""
        pkg_id = pkg["id"]
        app_dir = Path(pkg.get("install_dir", ""))
        binaries = pkg.get("binaries", [])
        if not binaries:
            return

        launcher_path = Path(binaries[0])
        fmt = pkg.get("format", "")

        env = pkg.get("env", {})
        args = pkg.get("args", [])

        env_lines = []
        for k, v in env.items():
            env_lines.append(f'export {k}="{v}"')
        env_block = ("\n" + "\n".join(env_lines)) if env_lines else ""

        args_lines = []
        for a in args:
            args_lines.append(f'    "{a}"')
        args_block = "\nPERSISTENT_ARGS=(\n" + "\n".join(args_lines) + "\n)\n" if args_lines else "\nPERSISTENT_ARGS=()\n"

        if fmt == "appimage":
            appimage_files = list(app_dir.glob("*.AppImage"))
            if not appimage_files:
                return
            target_appimage = appimage_files[0]
            script_content = f"""#!/usr/bin/env bash
# Wrapper created by appmgr for {pkg_id} (AppImage)
APPIMAGE="{target_appimage}"{env_block}
{args_block}
if [ -z "$APPIMAGE_EXTRACT_AND_RUN" ]; then
    if ! command -v fusermount &>/dev/null && ! command -v fusermount3 &>/dev/null; then
        export APPIMAGE_EXTRACT_AND_RUN=1
    fi
fi

exec "$APPIMAGE" "${{PERSISTENT_ARGS[@]}}" "$@"
"""
        elif fmt == "bin":
            bin_files = list((app_dir / "bin").glob("*"))
            if not bin_files:
                return
            rel_bin = bin_files[0].relative_to(app_dir)
            script_content = f"""#!/usr/bin/env bash
# Wrapper created by appmgr for {pkg_id} (Standalone binary)
APP_ROOT="{app_dir}"
export PATH="$APP_ROOT/bin:$PATH"
export LD_LIBRARY_PATH="$APP_ROOT/lib:${{LD_LIBRARY_PATH}}"{env_block}
{args_block}
exec "$APP_ROOT/{rel_bin}" "${{PERSISTENT_ARGS[@]}}" "$@"
"""
        elif fmt in ["tar", "zip"]:
            # Find primary binary
            primary_candidates = []
            for p in app_dir.glob("**/*"):
                if p.is_file() and not p.is_symlink() and not p.name.endswith(".so"):
                    try:
                        with open(p, "rb") as f:
                            if f.read(4) == b"\x7fELF" or f.read(2) == b"#!":
                                primary_candidates.append(p)
                    except Exception:
                        pass
            if not primary_candidates:
                return
            # Match package id
            chosen_bin = primary_candidates[0]
            for c_bin in primary_candidates:
                if c_bin.name.lower() == pkg_id.lower():
                    chosen_bin = c_bin
                    break
            rel_bin = chosen_bin.relative_to(app_dir)

            lib_dirs = []
            for cand in ["lib", "lib64", "usr/lib", "usr/lib64"]:
                if (app_dir / cand).exists():
                    lib_dirs.append(f"$APP_ROOT/{cand}")
            extra_ld = (":" + ":".join(lib_dirs)) if lib_dirs else ""

            script_content = f"""#!/usr/bin/env bash
# Wrapper created by appmgr for {pkg_id} ({fmt.capitalize()} binary)
APP_ROOT="{app_dir}"
export PATH="$APP_ROOT/bin:$APP_ROOT/usr/bin:$APP_ROOT:$PATH"
export LD_LIBRARY_PATH="$APP_ROOT/usr/lib:$APP_ROOT/lib{extra_ld}:${{LD_LIBRARY_PATH}}"
export XDG_DATA_DIRS="$APP_ROOT/share:$APP_ROOT/usr/share:${{XDG_DATA_DIRS:-/usr/local/share:/usr/share}}"{env_block}
{args_block}
exec "$APP_ROOT/{rel_bin}" "${{PERSISTENT_ARGS[@]}}" "$@"
"""
        else:
            # deb format: update existing wrapper script
            if launcher_path.exists():
                old_content = launcher_path.read_text()
                # Inject or update env and args block
                if "PERSISTENT_ARGS=" in old_content:
                    old_content = re.sub(r"PERSISTENT_ARGS=\(.*?\)\n", "", old_content, flags=re.DOTALL)
                # Ensure exec uses PERSISTENT_ARGS
                if "${PERSISTENT_ARGS[@]}" not in old_content:
                    old_content = old_content.replace('"$@"', '"${PERSISTENT_ARGS[@]}" "$@"')
                script_content = f"{env_block}\n{args_block}\n" + old_content
            else:
                return

        launcher_path.write_text(script_content)
        launcher_path.chmod(launcher_path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    def remove(self, query: str, assume_yes: bool = False) -> bool:
        """Remove an application cleanly."""
        pkg = self.db.get(query)
        if not pkg:
            print(f"{apt_prefix('FAIL')} Package '{query}' is not managed by appmgr.")
            return False

        print_apt_transaction_summary([], [pkg])

        if not assume_yes:
            if not confirm_dialog(f"Do you want to completely remove {pkg['name']}?", default=True):
                print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} Removal aborted.")
                return False

        start_time = time.time()
        print(f"\nRemoving {c(pkg['name'], Colors.BOLD, Colors.BRIGHT_RED)} ({pkg['version']}) ...")

        for b in pkg.get("binaries", []):
            b_path = Path(b)
            if b_path.exists() or b_path.is_symlink():
                b_path.unlink()

        for d in pkg.get("desktop_files", []):
            d_path = Path(d)
            if d_path.exists():
                d_path.unlink()

        for i in pkg.get("icons", []):
            i_path = Path(i)
            if i_path.exists():
                i_path.unlink()

        app_dir = Path(pkg.get("install_dir", ""))
        if app_dir.exists() and app_dir.is_dir():
            shutil.rmtree(app_dir, ignore_errors=True)

        self.db.remove(pkg["id"])
        self.run_triggers()

        elapsed = time.time() - start_time
        print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_GREEN)} Package {pkg['name']} removed in {elapsed:.2f}s.\n")
        return True

    def list_installed(self, filter_type: Optional[str] = None, as_json: bool = False):
        """Display table of all installed packages or print clean JSON."""
        all_pkgs = self.db.all()
        if filter_type:
            all_pkgs = [p for p in all_pkgs if p.get("format") == filter_type.lower()]

        if as_json:
            json_list = []
            for p in all_pkgs:
                json_list.append(
                    {
                        "id": p.get("id"),
                        "name": p.get("name"),
                        "version": p.get("version"),
                        "format": p.get("format"),
                        "repo": p.get("repo"),
                        "pinned": p.get("pinned", False),
                        "total_size_bytes": p.get("total_size_bytes", 0),
                        "binaries": p.get("binaries", []),
                        "env": p.get("env", {}),
                        "args": p.get("args", []),
                    }
                )
            print(json.dumps(json_list, indent=2))
            return

        headers = ["PACKAGE", "VERSION", "TYPE", "REPO", "SIZE", "LAUNCHER"]
        rows = []
        for p in all_pkgs:
            size_mb = p.get("total_size_bytes", 0) / (1024 * 1024)
            size_str = f"{size_mb:.1f} MB"
            binaries = p.get("binaries", [])
            bin_name = Path(binaries[0]).name if binaries else "-"
            pkg_type = f"[{p.get('format', '').upper()}]"
            repo_str = p.get("repo") or "-"
            ver_str = p.get("version", "")
            if p.get("pinned"):
                ver_str = f"{ver_str} {c('[PINNED]', Colors.BOLD, Colors.BRIGHT_MAGENTA)}"
            rows.append(
                [
                    p.get("id", ""),
                    ver_str,
                    pkg_type,
                    repo_str,
                    size_str,
                    bin_name,
                ]
            )

        print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} Managed Applications ({len(all_pkgs)} total):\n")
        print_table(headers, rows)

    def info(self, target: str):
        """Show detailed package metadata."""
        path = Path(target).expanduser().resolve()
        if path.exists() and path.is_file():
            pkg_type = self._detect_package_type(path)
            if pkg_type == "deb":
                pkg = DebPackage(path, self.config).read_metadata()
            elif pkg_type == "appimage":
                pkg = AppImagePackage(path, self.config).read_metadata()
            elif pkg_type == "zip":
                pkg = ZipPackage(path, self.config).read_metadata()
            elif pkg_type == "bin":
                pkg = StandaloneBinaryPackage(path, self.config).read_metadata()
            elif pkg_type == "tar":
                pkg = TarballPackage(path, self.config).read_metadata()
            else:
                print(f"{apt_prefix('FAIL')} Unknown package format.")
                return
            is_installed = False
        else:
            pkg = self.db.get(target)
            if not pkg:
                print(f"{apt_prefix('FAIL')} Package '{target}' not found.")
                return
            is_installed = True

        print(f"\n{c('Package Information:', Colors.BOLD, Colors.BRIGHT_WHITE)}")
        print(f"  {c('Name:', Colors.BOLD, Colors.BRIGHT_CYAN)}         {pkg.get('name')}")
        print(f"  {c('ID:', Colors.BOLD, Colors.BRIGHT_CYAN)}           {pkg.get('id')}")
        print(f"  {c('Version:', Colors.BOLD, Colors.BRIGHT_CYAN)}      {pkg.get('version')}")
        print(f"  {c('Format:', Colors.BOLD, Colors.BRIGHT_CYAN)}       {pkg.get('format', '').upper()}")
        print(f"  {c('Status:', Colors.BOLD, Colors.BRIGHT_CYAN)}       {c('Installed', Colors.BRIGHT_GREEN) if is_installed else c('Local Archive', Colors.BRIGHT_YELLOW)}")

        if pkg.get("pinned"):
            print(f"  {c('Pinned:', Colors.BOLD, Colors.BRIGHT_MAGENTA)}       {c('Yes (General upgrades disabled)', Colors.BOLD, Colors.YELLOW)}")

        if "repo" in pkg and pkg["repo"]:
            print(f"  {c('GitHub Repo:', Colors.BOLD, Colors.BRIGHT_CYAN)}   {pkg['repo']}")
            if pkg.get("tag"):
                print(f"  {c('Release Tag:', Colors.BOLD, Colors.BRIGHT_CYAN)}   {pkg['tag']}")
            if pkg.get("asset_pattern"):
                print(f"  {c('Asset Match:', Colors.BOLD, Colors.BRIGHT_CYAN)}   {pkg['asset_pattern']}")

        print(f"  {c('Description:', Colors.BOLD, Colors.BRIGHT_CYAN)}  {pkg.get('description')}")

        if is_installed:
            print(f"  {c('Installed On:', Colors.BOLD, Colors.BRIGHT_CYAN)} {pkg.get('install_date', 'N/A')}")
            print(f"  {c('Location:', Colors.BOLD, Colors.BRIGHT_CYAN)}     {pkg.get('install_dir')}")
            size_mb = pkg.get("total_size_bytes", 0) / (1024 * 1024)
            print(f"  {c('Disk Usage:', Colors.BOLD, Colors.BRIGHT_CYAN)}   {size_mb:.2f} MB")

            binaries = pkg.get("binaries", [])
            if binaries:
                print(f"  {c('Binaries:', Colors.BOLD, Colors.BRIGHT_CYAN)}     {', '.join(binaries)}")
            desktop_files = pkg.get("desktop_files", [])
            if desktop_files:
                print(f"  {c('Desktop:', Colors.BOLD, Colors.BRIGHT_CYAN)}      {', '.join(desktop_files)}")

            env = pkg.get("env", {})
            if env:
                env_str = ", ".join(f"{k}={v}" for k, v in env.items())
                print(f"  {c('Environment:', Colors.BOLD, Colors.BRIGHT_CYAN)}  {env_str}")

            args = pkg.get("args", [])
            if args:
                print(f"  {c('Launch Args:', Colors.BOLD, Colors.BRIGHT_CYAN)}  {' '.join(args)}")

            self._check_pkg_ldd(pkg)
        else:
            size_mb = pkg.get("size", 0) / (1024 * 1024)
            print(f"  {c('Archive Size:', Colors.BOLD, Colors.BRIGHT_CYAN)} {size_mb:.2f} MB")
            if "depends" in pkg and pkg["depends"]:
                print(f"  {c('Deb Depends:', Colors.BOLD, Colors.BRIGHT_CYAN)}  {pkg['depends']}")

        print()

    def run_app(self, app_id: str, args: List[str]):
        """Run an installed application."""
        pkg = self.db.get(app_id)
        if not pkg:
            print(f"{apt_prefix('FAIL')} Package '{app_id}' is not installed.")
            return

        binaries = pkg.get("binaries", [])
        if not binaries:
            print(f"{apt_prefix('FAIL')} No executable binary found for '{app_id}'.")
            return

        bin_path = binaries[0]
        cmd = [bin_path] + args
        try:
            os.execv(bin_path, cmd)
        except Exception as e:
            print(f"{apt_prefix('FAIL')} Failed to execute {bin_path}: {e}")

    def search(self, query: str):
        """Search GitHub for repositories matching query and display formatted results."""
        print(f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} Searching GitHub for '{c(query, Colors.BOLD, Colors.BRIGHT_CYAN)}'...")
        with Spinner(f"Querying GitHub Search API for '{query}'..."):
            try:
                repos = self.github.search_repositories(query, per_page=8)
            except GitHubRateLimitError as e:
                print(f"{apt_prefix('FAIL')} {e}")
                return
            except Exception as e:
                print(f"{apt_prefix('FAIL')} Search failed: {e}")
                return

        if not repos:
            print(f"{apt_prefix('FAIL')} No repositories found matching '{query}'.")
            return

        print_search_results(repos, query)

    def run_triggers(self):
        """Execute desktop and icon database update triggers."""
        print("Processing triggers for desktop-file-utils ...")
        if shutil.which("update-desktop-database"):
            subprocess.run(
                ["update-desktop-database", str(self.config.applications_dir)],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )

        print("Processing triggers for hicolor-icon-theme ...")
        if shutil.which("gtk-update-icon-cache"):
            theme_dir = self.config.icons_dir / "hicolor"
            if theme_dir.exists():
                subprocess.run(
                    ["gtk-update-icon-cache", "-q", "-t", "-f", str(theme_dir)],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )

    def _detect_package_type(self, path: Path) -> Optional[str]:
        """
        Detect whether a file is .deb, .AppImage, tarball, zip, or standalone ELF binary.
        """
        name_lower = path.name.lower()

        if ".pkg.tar." in name_lower:
            return "arch_pkg"

        if name_lower.endswith(".deb"):
            return "deb"
        if name_lower.endswith(".appimage"):
            return "appimage"
        if name_lower.endswith(".zip"):
            return "zip"

        tar_extensions = (
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
        if any(name_lower.endswith(ext) for ext in tar_extensions):
            return "tar"

        try:
            with open(path, "rb") as f:
                header = f.read(16)
                if header.startswith(b"!<arch>\n"):
                    return "deb"
                if header.startswith(b"PK\x03\x04"):
                    return "zip"
                if header.startswith(b"\x7fELF"):
                    sample = header + f.read(1024)
                    if b"AI\x02" in sample or b"AppImage" in sample:
                        return "appimage"
                    return "bin"
                if header.startswith(b"\x1f\x8b"):
                    return "tar"
                if header.startswith(b"\xfd7zXZ\x00"):
                    return "tar"
                if header.startswith(b"\x28\xb5\x2f\xfd"):
                    return "tar"
                if header.startswith(b"BZh"):
                    return "tar"
        except Exception:
            pass

        return None

    def _check_pkg_ldd(self, pkg: Dict[str, Any]):
        """Inspect dynamic libraries for installed package, handling static binaries cleanly."""
        app_dir = Path(pkg.get("install_dir", ""))
        target_elfs = []

        if app_dir.exists():
            for p in app_dir.glob("**/*"):
                if p.is_file() and not p.is_symlink():
                    try:
                        with open(p, "rb") as f:
                            if f.read(4) == b"\x7fELF":
                                target_elfs.append(p)
                    except Exception:
                        pass

        if not target_elfs:
            for b in pkg.get("binaries", []):
                p = Path(b)
                if p.exists() and p.is_file():
                    target_elfs.append(p)

        all_static = True
        missing_all = []
        has_dynamic = False

        for elf in target_elfs:
            try:
                out = subprocess.getoutput(f"ldd '{elf}' 2>&1")
                out_lower = out.lower()
                if "not a dynamic executable" in out_lower or "statically linked" in out_lower:
                    continue
                has_dynamic = True
                all_static = False
                missing = [l.strip().split("=>")[0].strip() for l in out.splitlines() if "not found" in l]
                missing_all.extend(missing)
            except Exception:
                pass

        if target_elfs and not has_dynamic:
            print(f"  {c('Dependencies:', Colors.BOLD, Colors.BRIGHT_GREEN)} [static binary] (All libraries statically linked)")
        elif missing_all:
            print(f"  {c('Dependencies:', Colors.BOLD, Colors.BRIGHT_YELLOW)} {len(missing_all)} missing libraries:")
            for m in missing_all[:3]:
                print(f"    - {c(m, Colors.RED)}")
        else:
            print(f"  {c('Dependencies:', Colors.BOLD, Colors.BRIGHT_GREEN)} All shared libraries satisfied")
