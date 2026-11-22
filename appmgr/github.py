"""
GitHub Releases API Integration and Asset Matcher for appmgr.
Supports public API without token, ETag caching (304 Not Modified),
optional GITHUB_TOKEN, specific version tags (@v1.2.3), pre-release handling,
SHA-256 verification, architecture/format filtering (.AppImage, .deb, .tar.*, .zip, standalone ELF),
and streaming downloads.
"""

import hashlib
import json
import os
import platform
import re
import shutil
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

from .ui import Colors, PacmanBar, apt_prefix, c, pacman_prompt


class GitHubRateLimitError(Exception):
    pass


class GitHubClient:
    API_BASE = "https://api.github.com"

    def __init__(self, token: Optional[str] = None):
        self.token = token or os.environ.get("GITHUB_TOKEN")

    def _headers(self, etag: Optional[str] = None) -> Dict[str, str]:
        headers = {
            "User-Agent": "appmgr/1.4.0",
            "Accept": "application/vnd.github.v3+json",
        }
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        if etag:
            headers["If-None-Match"] = etag
        return headers

    def search_repositories(self, query: str, per_page: int = 8) -> List[Dict[str, Any]]:
        """
        Search GitHub repositories via /search/repositories?q=<query>&sort=stars&order=desc&per_page=8
        """
        clean_q = urllib.parse.quote(query.strip())
        url = f"{self.API_BASE}/search/repositories?q={clean_q}&sort=stars&order=desc&per_page={per_page}"
        req = urllib.request.Request(url, headers=self._headers())

        try:
            with urllib.request.urlopen(req, timeout=15) as resp:
                body = resp.read().decode("utf-8")
                data = json.loads(body)
                return data.get("items", [])
        except urllib.error.HTTPError as e:
            if e.code == 403:
                remaining = e.headers.get("X-RateLimit-Remaining")
                if remaining == "0":
                    raise GitHubRateLimitError(
                        "GitHub API rate limit exceeded. Set GITHUB_TOKEN environment variable to get 5,000 requests/hr."
                    )
                raise GitHubRateLimitError(f"GitHub API HTTP 403: {e.reason}")
            raise RuntimeError(f"GitHub Search API error (HTTP {e.code}): {e.reason}")
        except urllib.error.URLError as e:
            raise RuntimeError(f"Network error accessing GitHub API: {e.reason}")

    def get_release(
        self,
        repo: str,
        tag: Optional[str] = None,
        include_pre: bool = False,
        etag: Optional[str] = None,
    ) -> Tuple[int, Optional[Dict[str, Any]], Optional[str], bool]:
        """
        Fetch release from GitHub API.
        Returns: (status_code, release_data, new_etag, is_prerelease)
        """
        clean_repo = repo.strip().strip("/")

        if tag:
            clean_tag = tag.strip()
            url = f"{self.API_BASE}/repos/{clean_repo}/releases/tags/{clean_tag}"
            req = urllib.request.Request(url, headers=self._headers(etag=etag))
            try:
                with urllib.request.urlopen(req, timeout=15) as resp:
                    new_etag = resp.headers.get("ETag")
                    data = json.loads(resp.read().decode("utf-8"))
                    return 200, data, new_etag, data.get("prerelease", False)
            except urllib.error.HTTPError as e:
                if e.code == 304:
                    return 304, None, etag, False
                if e.code == 404:
                    return 404, None, None, False
                if e.code == 403:
                    self._check_rate_limit(e)
                raise RuntimeError(f"GitHub API error (HTTP {e.code}): {e.reason}")
            except urllib.error.URLError as e:
                raise RuntimeError(f"Network error accessing GitHub API: {e.reason}")

        if include_pre:
            url = f"{self.API_BASE}/repos/{clean_repo}/releases?per_page=10"
            req = urllib.request.Request(url, headers=self._headers(etag=etag))
            try:
                with urllib.request.urlopen(req, timeout=15) as resp:
                    new_etag = resp.headers.get("ETag")
                    data = json.loads(resp.read().decode("utf-8"))
                    if isinstance(data, list) and len(data) > 0:
                        chosen = data[0]
                        return 200, chosen, new_etag, chosen.get("prerelease", False)
                    return 404, None, None, False
            except urllib.error.HTTPError as e:
                if e.code == 304:
                    return 304, None, etag, False
                if e.code == 404:
                    return 404, None, None, False
                if e.code == 403:
                    self._check_rate_limit(e)
                raise RuntimeError(f"GitHub API error (HTTP {e.code}): {e.reason}")
            except urllib.error.URLError as e:
                raise RuntimeError(f"Network error accessing GitHub API: {e.reason}")

        # Default: /releases/latest
        url = f"{self.API_BASE}/repos/{clean_repo}/releases/latest"
        req = urllib.request.Request(url, headers=self._headers(etag=etag))

        try:
            with urllib.request.urlopen(req, timeout=15) as resp:
                new_etag = resp.headers.get("ETag")
                data = json.loads(resp.read().decode("utf-8"))
                return 200, data, new_etag, False
        except urllib.error.HTTPError as e:
            if e.code == 304:
                return 304, None, etag, False
            if e.code == 403:
                self._check_rate_limit(e)
            if e.code == 404:
                # Fallback: check if repository only has pre-releases
                try:
                    fb_url = f"{self.API_BASE}/repos/{clean_repo}/releases?per_page=5"
                    fb_req = urllib.request.Request(fb_url, headers=self._headers())
                    with urllib.request.urlopen(fb_req, timeout=10) as fb_resp:
                        fb_data = json.loads(fb_resp.read().decode("utf-8"))
                        if isinstance(fb_data, list) and len(fb_data) > 0:
                            chosen = fb_data[0]
                            return 200, chosen, fb_resp.headers.get("ETag"), True
                except Exception:
                    pass
                return 404, None, None, False
            raise RuntimeError(f"GitHub API error (HTTP {e.code}): {e.reason}")
        except urllib.error.URLError as e:
            raise RuntimeError(f"Network error accessing GitHub API: {e.reason}")

    def get_latest_release(
        self, repo: str, etag: Optional[str] = None
    ) -> Tuple[int, Optional[Dict[str, Any]], Optional[str]]:
        code, data, new_etag, _ = self.get_release(repo, etag=etag)
        return code, data, new_etag

    def _check_rate_limit(self, e: urllib.error.HTTPError):
        remaining = e.headers.get("X-RateLimit-Remaining")
        if remaining == "0":
            raise GitHubRateLimitError(
                "GitHub API rate limit exceeded. Set GITHUB_TOKEN environment variable to get 5,000 requests/hr."
            )
        raise GitHubRateLimitError(f"GitHub API HTTP 403: {e.reason}")

    @staticmethod
    def parse_repo_and_tag(target: str) -> Tuple[Optional[str], Optional[str]]:
        """
        Parse repo and optional tag from target string:
        - 'owner/repo@v1.2.3' -> ('owner/repo', 'v1.2.3')
        - 'github:owner/repo@tag' -> ('owner/repo', 'tag')
        - 'owner/repo' -> ('owner/repo', None)
        """
        s = target.strip()
        tag = None
        if "@" in s:
            s, tag = s.split("@", 1)
            tag = tag.strip() or None

        repo = GitHubClient.parse_repo(s)
        return repo, tag

    @staticmethod
    def parse_repo(target: str) -> Optional[str]:
        s = target.strip()
        if "@" in s:
            s = s.split("@", 1)[0]
        if s.startswith("github:"):
            s = s[7:]
        elif "github.com/" in s:
            parts = s.split("github.com/", 1)[1].split("/")
            if len(parts) >= 2:
                return f"{parts[0]}/{parts[1]}"
            return None

        parts = s.split("/")
        if len(parts) == 2 and parts[0] and parts[1]:
            owner = parts[0]
            repo = parts[1].split("#")[0].split("?")[0]
            if re.match(r"^[a-zA-Z0-9_\-\.]+$", owner) and re.match(r"^[a-zA-Z0-9_\-\.]+$", repo):
                return f"{owner}/{repo}"
        return None

    def filter_assets(
        self,
        assets: List[Dict[str, Any]],
        prefer: Optional[str] = None,
        saved_pattern: Optional[str] = None,
        app_name_hint: Optional[str] = None,
    ) -> List[Dict[str, Any]]:
        """
        Filter assets by architecture, exclude win/mac, prioritize:
        .AppImage -> .deb -> .tar.* -> .zip -> raw standalone binary.
        """
        host_arch = platform.machine().lower()
        is_x86_64 = host_arch in ["x86_64", "amd64"]
        is_aarch64 = host_arch in ["aarch64", "arm64"]

        # Blacklisted platform/OS tokens
        os_blacklist = [
            ".msi",
            ".dmg",
            ".apk",
            ".rpm",
            ".pkg.tar.",
            "-src.",
            "_src.",
            "-source.",
            "_source.",
            "-sources.",
            "_sources.",
            "src.tar",
            "source.tar",
            "sources.tar",
            "debug",
            "dbg",
            "symbols",
            "dsym",
            "windows",
            "win32",
            "win64",
            "-win-",
            "_win_",
            "-win.",
            "_win.",
            "darwin",
            "macos",
            "mac-",
            "mac_",
            ".sha256",
            ".sha256sum",
            ".sha512",
            ".sig",
            ".asc",
            ".txt",
            ".md5",
            ".json",
            ".yaml",
            ".yml",
            ".sbom",
            ".pem",
            ".crt",
        ]

        if is_x86_64:
            arch_blacklist = [
                "arm64",
                "aarch64",
                "armv",
                "armhf",
                "armel",
                "arm-",
                "arm_",
                "-arm.",
                "i386",
                "i486",
                "i586",
                "i686",
                "x86-32",
                "x86_32",
                "s390",
                "s390x",
                "ppc",
                "powerpc",
                "riscv",
                "mips",
                "m68k",
                "sparc",
                "alpha",
                "sh4",
                "loongarch",
                "wasm",
            ]
        elif is_aarch64:
            arch_blacklist = [
                "x86_64",
                "amd64",
                "x64",
                "x86-64",
                "i386",
                "i486",
                "i586",
                "i686",
                "s390",
                "s390x",
                "ppc",
                "powerpc",
                "riscv",
                "mips",
                "m68k",
                "sparc",
                "alpha",
                "sh4",
                "loongarch",
                "wasm",
            ]
        else:
            arch_blacklist = []

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

        valid_assets = []
        for asset in assets:
            name = asset.get("name", "")
            lower_name = name.lower()

            # Check OS & source blacklist
            if any(b in lower_name for b in os_blacklist):
                continue

            # Check arch blacklist
            if any(b in lower_name for b in arch_blacklist):
                continue

            is_deb = lower_name.endswith(".deb")
            is_appimage = lower_name.endswith(".appimage")
            is_tar = any(lower_name.endswith(ext) for ext in tar_extensions)
            is_zip = lower_name.endswith(".zip")

            # Check if this could be a standalone binary (e.g. yt-dlp, jq, kubectl)
            is_bin = False
            if not (is_deb or is_appimage or is_tar or is_zip):
                # Must not end in .exe or generic doc extensions
                if not lower_name.endswith(".exe"):
                    if is_x86_64:
                        if any(t in lower_name for t in ["linux", "amd64", "x86_64", "x64"]):
                            is_bin = True
                        elif app_name_hint and lower_name == app_name_hint.lower():
                            is_bin = True
                    elif is_aarch64:
                        if any(t in lower_name for t in ["linux", "aarch64", "arm64"]):
                            is_bin = True
                        elif app_name_hint and lower_name == app_name_hint.lower():
                            is_bin = True

            if not (is_deb or is_appimage or is_tar or is_zip or is_bin):
                continue

            # Match saved pattern if specified
            matches_pattern = False
            if saved_pattern:
                try:
                    if re.search(saved_pattern, name, re.IGNORECASE):
                        matches_pattern = True
                except Exception:
                    pass

            format_score = 0
            if prefer == "deb":
                if is_deb:
                    format_score += 50
                elif is_appimage:
                    format_score += 20
                elif is_tar:
                    format_score += 15
                elif is_zip:
                    format_score += 10
                elif is_bin:
                    format_score += 5
            elif prefer == "appimage":
                if is_appimage:
                    format_score += 50
                elif is_deb:
                    format_score += 20
                elif is_tar:
                    format_score += 15
                elif is_zip:
                    format_score += 10
                elif is_bin:
                    format_score += 5
            elif prefer == "tar":
                if is_tar:
                    format_score += 50
                elif is_appimage:
                    format_score += 20
                elif is_deb:
                    format_score += 15
                elif is_zip:
                    format_score += 10
                elif is_bin:
                    format_score += 5
            elif prefer == "zip":
                if is_zip:
                    format_score += 50
                elif is_appimage:
                    format_score += 20
                elif is_deb:
                    format_score += 15
                elif is_tar:
                    format_score += 10
                elif is_bin:
                    format_score += 5
            elif prefer == "bin":
                if is_bin:
                    format_score += 50
                elif is_appimage:
                    format_score += 20
                elif is_deb:
                    format_score += 15
                elif is_tar:
                    format_score += 10
                elif is_zip:
                    format_score += 5
            else:
                # Default priority: .AppImage -> .deb -> .tar.* -> .zip -> raw binary
                if is_appimage:
                    format_score += 30
                elif is_deb:
                    format_score += 20
                elif is_tar:
                    format_score += 10
                elif is_zip:
                    format_score += 5
                elif is_bin:
                    format_score += 1

            # Architecture boost for exact host match
            arch_score = 0
            if is_x86_64:
                if any(t in lower_name for t in ["x86_64", "amd64", "x86-64", "linux64", "x64"]):
                    arch_score += 40
            elif is_aarch64:
                if any(t in lower_name for t in ["aarch64", "arm64", "armv8"]):
                    arch_score += 40

            format_score += arch_score

            if matches_pattern:
                format_score += 100

            fmt_label = "deb" if is_deb else ("appimage" if is_appimage else ("tar" if is_tar else ("zip" if is_zip else "bin")))

            valid_assets.append(
                {
                    "asset": asset,
                    "name": name,
                    "format": fmt_label,
                    "size": asset.get("size", 0),
                    "url": asset.get("browser_download_url", ""),
                    "score": format_score,
                    "matches_pattern": matches_pattern,
                }
            )

        valid_assets.sort(key=lambda x: (x["score"], x["size"]), reverse=True)
        return valid_assets

    @staticmethod
    def derive_pattern(asset_name: str, version: str) -> str:
        escaped_version = re.escape(version.lstrip("v"))
        pattern_str = re.sub(escaped_version, r".*", asset_name)
        parts = pattern_str.split(r".*")
        escaped_parts = [re.escape(p) for p in parts]
        return "^" + r".*".join(escaped_parts) + "$"

    def download_asset(
        self,
        url: str,
        dest_path: Path,
        expected_size: int = 0,
        asset_name: str = "asset",
        chosen_asset: Optional[Dict[str, Any]] = None,
        release_assets: Optional[List[Dict[str, Any]]] = None,
    ) -> bool:
        """
        Stream download an asset with a Pacman animated progress bar
        and verify its SHA-256 checksum if available.
        """
        req = urllib.request.Request(url, headers=self._headers())

        dest_path.parent.mkdir(parents=True, exist_ok=True)
        temp_dest = dest_path.with_name(f"{dest_path.name}.part")

        # 1. Determine expected SHA-256 if available
        expected_sha256 = self._find_expected_sha256(asset_name, chosen_asset, release_assets)

        bar = PacmanBar(total=100, prefix=f"Downloading {asset_name[:20]}", unit="%")

        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                total_bytes = int(resp.headers.get("Content-Length") or expected_size or 0)
                downloaded = 0
                chunk_size = 128 * 1024  # 128 KB
                sha256_hash = hashlib.sha256()

                with open(temp_dest, "wb") as f:
                    while True:
                        chunk = resp.read(chunk_size)
                        if not chunk:
                            break
                        f.write(chunk)
                        sha256_hash.update(chunk)
                        downloaded += len(chunk)

                        if total_bytes > 0:
                            percent = int((downloaded / total_bytes) * 100)
                            mb_done = downloaded / (1024 * 1024)
                            mb_total = total_bytes / (1024 * 1024)
                            bar.update(percent, extra_text=f"{mb_done:.1f}/{mb_total:.1f} MB")
                        else:
                            mb_done = downloaded / (1024 * 1024)
                            bar.update(50, extra_text=f"{mb_done:.1f} MB")

            bar.finish(f"Downloaded {asset_name}")

            computed_sha256 = sha256_hash.hexdigest().lower()

            # 2. Verify SHA-256
            if expected_sha256:
                if computed_sha256 == expected_sha256.lower():
                    print(f" {apt_prefix('OK')} SHA-256 verified ({c(computed_sha256[:12] + '...', Colors.BOLD, Colors.BRIGHT_GREEN)})")
                else:
                    if temp_dest.exists():
                        temp_dest.unlink()
                    print(f" {apt_prefix('FAIL')} SHA-256 checksum mismatch for {asset_name}!")
                    print(f"        Expected: {c(expected_sha256, Colors.BOLD, Colors.BRIGHT_RED)}")
                    print(f"        Computed: {c(computed_sha256, Colors.BOLD, Colors.BRIGHT_YELLOW)}")
                    raise ValueError(f"SHA-256 checksum mismatch for {asset_name}")

            shutil.move(temp_dest, dest_path)
            return True
        except Exception as e:
            if temp_dest.exists():
                temp_dest.unlink()
            raise

    def _find_expected_sha256(
        self,
        asset_name: str,
        chosen_asset: Optional[Dict[str, Any]] = None,
        release_assets: Optional[List[Dict[str, Any]]] = None,
    ) -> Optional[str]:
        # A) Check in chosen asset metadata
        if chosen_asset:
            asset_meta = chosen_asset.get("asset", {})
            digest = asset_meta.get("digest") or asset_meta.get("sha256")
            if digest and digest.startswith("sha256:"):
                return digest.split("sha256:", 1)[1].strip().lower()

        # B) Check in release assets for checksum files
        if not release_assets:
            return None

        checksum_patterns = [
            f"{asset_name}.sha256",
            f"{asset_name}.sha256sum",
            "sha256sums.txt",
            "sha256sums",
            "sha256sum.txt",
            "sha256sum",
            "sha256.txt",
            "sha256",
            "checksums.txt",
            "checksums_sha256.txt",
            "checksums",
            "checksums.sha256",
        ]

        # Prioritize exact file match first, then multi-file checksum manifests
        target_asset = None
        for cand in release_assets:
            c_name = cand.get("name", "").lower()
            if c_name == f"{asset_name.lower()}.sha256" or c_name == f"{asset_name.lower()}.sha256sum":
                target_asset = cand
                break

        if not target_asset:
            for cand in release_assets:
                c_name = cand.get("name", "").lower()
                for pat in checksum_patterns:
                    if c_name == pat or c_name.endswith(pat):
                        target_asset = cand
                        break
                if target_asset:
                    break

        if not target_asset:
            for cand in release_assets:
                c_name = cand.get("name", "").lower()
                if any(kw in c_name for kw in ["sha256", "checksum"]) and any(c_name.endswith(ext) for ext in [".txt", ".sha256", ".sha256sum", ".sum", ".sums", ""]):
                    target_asset = cand
                    break

        if not target_asset:
            return None

        chk_url = target_asset.get("browser_download_url")
        if not chk_url:
            return None

        try:
            req = urllib.request.Request(chk_url, headers=self._headers())
            with urllib.request.urlopen(req, timeout=10) as resp:
                content = resp.read().decode("utf-8", errors="ignore")

                # Parse lines
                for line in content.splitlines():
                    line = line.strip()
                    if not line:
                        continue
                    # Match 64-char hex string
                    hex_match = re.search(r"\b([a-fA-F0-9]{64})\b", line)
                    if hex_match:
                        found_hash = hex_match.group(1).lower()
                        # If single hash file, return it
                        if target_asset.get("name", "").lower().startswith(asset_name.lower()):
                            return found_hash
                        # If manifest file, check filename on the line
                        if asset_name in line or asset_name.lower() in line.lower():
                            return found_hash
        except Exception:
            pass

        return None
