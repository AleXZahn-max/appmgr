use crate::ui::{apt_prefix, c, Colors, PacmanBar};
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, IF_NONE_MATCH, USER_AGENT};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoItem {
    pub full_name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub stargazers_count: u64,
    pub html_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    pub browser_download_url: String,
    pub digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone)]
pub struct CandidateAsset {
    pub asset: ReleaseAsset,
    pub name: String,
    pub format: String,
    pub size: u64,
    pub url: String,
    pub score: i32,
    #[allow(dead_code)]
    pub matches_pattern: bool,
}

#[derive(Debug, Clone)]
pub struct GitHubClient {
    token: Option<String>,
    client: reqwest::Client,
}

impl Default for GitHubClient {
    fn default() -> Self {
        Self::new()
    }
}

impl GitHubClient {
    pub const API_BASE: &'static str = "https://api.github.com";

    pub fn new() -> Self {
        let token = std::env::var("GITHUB_TOKEN").ok().filter(|s| !s.is_empty());
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { token, client }
    }

    fn headers(&self, etag: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(concat!("appmgr/", env!("CARGO_PKG_VERSION"))),
        );
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github.v3+json"),
        );
        if let Some(token) = &self.token {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {}", token)) {
                headers.insert(AUTHORIZATION, val);
            }
        }
        if let Some(et) = etag {
            if let Ok(val) = HeaderValue::from_str(et) {
                headers.insert(IF_NONE_MATCH, val);
            }
        }
        headers
    }

    pub async fn search_repositories(
        &self,
        query: &str,
        per_page: usize,
    ) -> Result<Vec<RepoItem>, String> {
        let clean_q = query.trim();
        let url = format!(
            "{}/search/repositories?q={}&sort=stars&order=desc&per_page={}",
            Self::API_BASE,
            urlencoding::encode(clean_q),
            per_page
        );

        let resp = self
            .client
            .get(&url)
            .headers(self.headers(None))
            .send()
            .await
            .map_err(|e| format!("Network error: {}", e))?;

        let status = resp.status();
        if status == StatusCode::FORBIDDEN {
            return Err("GitHub API rate limit exceeded. Set GITHUB_TOKEN environment variable to get 5,000 req/hr.".to_string());
        }
        if !status.is_success() {
            return Err(format!("GitHub Search API error (HTTP {})", status));
        }

        #[derive(Deserialize)]
        struct SearchResponse {
            #[serde(default)]
            items: Vec<RepoItem>,
        }

        let body = resp
            .json::<SearchResponse>()
            .await
            .map_err(|e| format!("Failed to parse JSON: {}", e))?;
        Ok(body.items)
    }

    pub async fn get_release(
        &self,
        repo: &str,
        tag: Option<&str>,
        include_pre: bool,
        etag: Option<&str>,
    ) -> Result<(StatusCode, Option<Release>, Option<String>, bool), String> {
        let clean_repo = repo.trim().trim_matches('/');

        if let Some(t) = tag {
            let url = format!("{}/repos/{}/releases/tags/{}", Self::API_BASE, clean_repo, t.trim());
            let resp = self
                .client
                .get(&url)
                .headers(self.headers(etag))
                .send()
                .await
                .map_err(|e| format!("Network error: {}", e))?;

            let status = resp.status();
            if status == StatusCode::NOT_MODIFIED {
                return Ok((StatusCode::NOT_MODIFIED, None, etag.map(String::from), false));
            }
            if status == StatusCode::NOT_FOUND {
                return Ok((StatusCode::NOT_FOUND, None, None, false));
            }
            if status == StatusCode::FORBIDDEN {
                return Err("GitHub API rate limit exceeded. Set GITHUB_TOKEN environment variable.".to_string());
            }

            let new_etag = resp
                .headers()
                .get("ETag")
                .and_then(|h| h.to_str().ok())
                .map(String::from);
            let release = resp
                .json::<Release>()
                .await
                .map_err(|e| format!("Failed to parse release: {}", e))?;
            let is_pre = release.prerelease;
            return Ok((StatusCode::OK, Some(release), new_etag, is_pre));
        }

        if include_pre {
            let url = format!("{}/repos/{}/releases?per_page=10", Self::API_BASE, clean_repo);
            let resp = self
                .client
                .get(&url)
                .headers(self.headers(etag))
                .send()
                .await
                .map_err(|e| format!("Network error: {}", e))?;

            let status = resp.status();
            if status == StatusCode::NOT_MODIFIED {
                return Ok((StatusCode::NOT_MODIFIED, None, etag.map(String::from), false));
            }
            if status == StatusCode::NOT_FOUND {
                return Ok((StatusCode::NOT_FOUND, None, None, false));
            }

            let new_etag = resp
                .headers()
                .get("ETag")
                .and_then(|h| h.to_str().ok())
                .map(String::from);
            let releases = resp
                .json::<Vec<Release>>()
                .await
                .map_err(|e| format!("Failed to parse releases: {}", e))?;

            if let Some(first) = releases.into_iter().next() {
                let is_pre = first.prerelease;
                return Ok((StatusCode::OK, Some(first), new_etag, is_pre));
            }
            return Ok((StatusCode::NOT_FOUND, None, None, false));
        }

        // Default: /releases/latest
        let url = format!("{}/repos/{}/releases/latest", Self::API_BASE, clean_repo);
        let resp = self
            .client
            .get(&url)
            .headers(self.headers(etag))
            .send()
            .await
            .map_err(|e| format!("Network error: {}", e))?;

        let status = resp.status();
        if status == StatusCode::NOT_MODIFIED {
            return Ok((StatusCode::NOT_MODIFIED, None, etag.map(String::from), false));
        }
        if status == StatusCode::NOT_FOUND {
            // Fallback: check if repo has only pre-releases
            let fb_url = format!("{}/repos/{}/releases?per_page=5", Self::API_BASE, clean_repo);
            if let Ok(fb_resp) = self.client.get(&fb_url).headers(self.headers(None)).send().await {
                if let Ok(fb_list) = fb_resp.json::<Vec<Release>>().await {
                    if let Some(first) = fb_list.into_iter().next() {
                        return Ok((StatusCode::OK, Some(first), None, true));
                    }
                }
            }
            return Ok((StatusCode::NOT_FOUND, None, None, false));
        }
        if status == StatusCode::FORBIDDEN {
            return Err("GitHub API rate limit exceeded. Set GITHUB_TOKEN environment variable.".to_string());
        }

        let new_etag = resp
            .headers()
            .get("ETag")
            .and_then(|h| h.to_str().ok())
            .map(String::from);
        let release = resp
            .json::<Release>()
            .await
            .map_err(|e| format!("Failed to parse release: {}", e))?;
        Ok((StatusCode::OK, Some(release), new_etag, false))
    }

    pub fn parse_repo_and_tag(target: &str) -> (Option<String>, Option<String>) {
        let s = target.trim();
        let (repo_part, tag) = if let Some((r, t)) = s.split_once('@') {
            (r, Some(t.trim().to_string()).filter(|x| !x.is_empty()))
        } else {
            (s, None)
        };
        (Self::parse_repo(repo_part), tag)
    }

    pub fn parse_repo(target: &str) -> Option<String> {
        let mut s = target.trim();
        if let Some((r, _)) = s.split_once('@') {
            s = r;
        }
        if let Some(stripped) = s.strip_prefix("github:") {
            s = stripped;
        } else if let Some((_, rest)) = s.split_once("github.com/") {
            s = rest;
        }

        let parts: Vec<&str> = s.split('/').collect();
        if parts.len() >= 2 {
            let owner = parts[0];
            let repo = parts[1].split(['#', '?']).next().unwrap_or("");
            if !owner.is_empty() && !repo.is_empty() {
                let re = Regex::new(r"^[a-zA-Z0-9_\-\.]+$").unwrap();
                if re.is_match(owner) && re.is_match(repo) {
                    return Some(format!("{}/{}", owner, repo));
                }
            }
        }
        None
    }

    pub fn filter_assets(
        &self,
        assets: &[ReleaseAsset],
        prefer: Option<&str>,
        saved_pattern: Option<&str>,
        app_name_hint: Option<&str>,
    ) -> Vec<CandidateAsset> {
        let host_arch = std::env::consts::ARCH;
        let is_x86_64 = host_arch == "x86_64";
        let is_aarch64 = host_arch == "aarch64";

        let os_blacklist = [
            ".msi", ".dmg", ".apk", ".rpm", ".pkg.tar.", "-src.", "_src.", "-source.", "_source.",
            "-sources.", "_sources.", "src.tar", "source.tar", "sources.tar", "debug", "dbg",
            "symbols", "dsym", "windows", "win32", "win64", "-win-", "_win_", "-win.", "_win.",
            "darwin", "macos", "mac-", "mac_", ".sha256", ".sha256sum", ".sha512", ".sig",
            ".asc", ".txt", ".md5", ".json", ".yaml", ".yml", ".sbom", ".pem", ".crt",
        ];

        let x86_blacklist = [
            "arm64", "aarch64", "armv", "armhf", "armel", "arm-", "arm_", "-arm.", "i386", "i486",
            "i586", "i686", "x86-32", "x86_32", "s390", "s390x", "ppc", "powerpc", "riscv",
            "mips", "m68k", "sparc", "alpha", "sh4", "loongarch", "wasm",
        ];

        let arm_blacklist = [
            "x86_64", "amd64", "x64", "x86-64", "i386", "i486", "i586", "i686", "s390", "s390x",
            "ppc", "powerpc", "riscv", "mips", "m68k", "sparc", "alpha", "sh4", "loongarch",
            "wasm",
        ];

        let arch_blacklist = if is_x86_64 {
            &x86_blacklist[..]
        } else if is_aarch64 {
            &arm_blacklist[..]
        } else {
            &[][..]
        };

        let tar_extensions = [
            ".tar.xz", ".tar.zst", ".tar.gz", ".tgz", ".tar.bz2", ".tbz2", ".txz", ".tzst", ".tar",
        ];

        let saved_re = saved_pattern.and_then(|p| Regex::new(p).ok());
        let mut candidates = Vec::new();

        for asset in assets {
            let name = &asset.name;
            let lower = name.to_lowercase();

            if os_blacklist.iter().any(|b| lower.contains(b)) {
                continue;
            }
            if arch_blacklist.iter().any(|b| lower.contains(b)) {
                continue;
            }

            let is_deb = lower.ends_with(".deb");
            let is_appimage = lower.ends_with(".appimage");
            let is_tar = tar_extensions.iter().any(|ext| lower.ends_with(ext));
            let is_zip = lower.ends_with(".zip");

            let mut is_bin = false;
            if !is_deb && !is_appimage && !is_tar && !is_zip && !lower.ends_with(".exe") {
                if is_x86_64 {
                    if ["linux", "amd64", "x86_64", "x64"].iter().any(|t| lower.contains(t)) {
                        is_bin = true;
                    } else if let Some(hint) = app_name_hint {
                        if lower == hint.to_lowercase() {
                            is_bin = true;
                        }
                    }
                } else if is_aarch64 {
                    if ["linux", "aarch64", "arm64"].iter().any(|t| lower.contains(t)) {
                        is_bin = true;
                    } else if let Some(hint) = app_name_hint {
                        if lower == hint.to_lowercase() {
                            is_bin = true;
                        }
                    }
                }
            }

            if !is_deb && !is_appimage && !is_tar && !is_zip && !is_bin {
                continue;
            }

            let matches_pattern = saved_re.as_ref().map(|r| r.is_match(name)).unwrap_or(false);

            let mut format_score = match prefer {
                Some("deb") => {
                    if is_deb { 50 } else if is_appimage { 20 } else if is_tar { 15 } else if is_zip { 10 } else { 5 }
                }
                Some("appimage") => {
                    if is_appimage { 50 } else if is_deb { 20 } else if is_tar { 15 } else if is_zip { 10 } else { 5 }
                }
                Some("tar") => {
                    if is_tar { 50 } else if is_appimage { 20 } else if is_deb { 15 } else if is_zip { 10 } else { 5 }
                }
                Some("zip") => {
                    if is_zip { 50 } else if is_appimage { 20 } else if is_deb { 15 } else if is_tar { 10 } else { 5 }
                }
                Some("bin") => {
                    if is_bin { 50 } else if is_appimage { 20 } else if is_deb { 15 } else if is_tar { 10 } else { 5 }
                }
                _ => {
                    // Default priority: .AppImage -> .deb -> .tar.* -> .zip -> raw binary
                    if is_appimage { 30 } else if is_deb { 20 } else if is_tar { 10 } else if is_zip { 5 } else { 1 }
                }
            };

            let arch_score = if is_x86_64 {
                if ["x86_64", "amd64", "x86-64", "linux64", "x64"].iter().any(|t| lower.contains(t)) { 40 } else { 0 }
            } else if is_aarch64 {
                if ["aarch64", "arm64", "armv8"].iter().any(|t| lower.contains(t)) { 40 } else { 0 }
            } else {
                0
            };
            format_score += arch_score;

            if matches_pattern {
                format_score += 100;
            }

            let fmt_label = if is_deb {
                "deb"
            } else if is_appimage {
                "appimage"
            } else if is_tar {
                "tar"
            } else if is_zip {
                "zip"
            } else {
                "bin"
            };

            candidates.push(CandidateAsset {
                asset: asset.clone(),
                name: name.clone(),
                format: fmt_label.to_string(),
                size: asset.size,
                url: asset.browser_download_url.clone(),
                score: format_score,
                matches_pattern,
            });
        }

        candidates.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.size.cmp(&a.size)));
        candidates
    }

    pub fn derive_pattern(asset_name: &str, version: &str) -> String {
        let clean_ver = version.trim_start_matches('v');
        let escaped_ver = regex::escape(clean_ver);
        let replaced = Regex::new(&escaped_ver)
            .map(|r| r.replace_all(asset_name, ".*").to_string())
            .unwrap_or_else(|_| asset_name.to_string());

        let parts: Vec<String> = replaced
            .split(".*")
            .map(regex::escape)
            .collect();
        format!("^{}$", parts.join(".*"))
    }

    pub async fn download_asset(
        &self,
        url: &str,
        dest_path: &Path,
        expected_size: u64,
        asset_name: &str,
        chosen_asset: Option<&CandidateAsset>,
        release_assets: Option<&[ReleaseAsset]>,
    ) -> Result<(), String> {
        if let Some(parent) = dest_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let temp_dest = dest_path.with_extension("part");

        let expected_sha256 = self
            .find_expected_sha256(asset_name, chosen_asset, release_assets)
            .await;

        let resp = self
            .client
            .get(url)
            .headers(self.headers(None))
            .send()
            .await
            .map_err(|e| format!("Failed to initiate download: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("Download failed with HTTP {}", resp.status()));
        }

        let total_bytes = resp.content_length().unwrap_or(expected_size);
        let mut bar = PacmanBar::new(
            100,
            &format!("Downloading {}", &asset_name[..asset_name.len().min(20)]),
            "%",
        );

        let mut file = File::create(&temp_dest)
            .map_err(|e| format!("Failed to create temp file: {}", e))?;
        let mut downloaded: u64 = 0;
        let mut hasher = Sha256::new();
        let mut stream = resp.bytes_stream();

        use futures_util::StreamExt;
        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res.map_err(|e| format!("Error streaming chunk: {}", e))?;
            file.write_all(&chunk)
                .map_err(|e| format!("Error writing chunk: {}", e))?;
            hasher.update(&chunk);
            downloaded += chunk.len() as u64;

            if total_bytes > 0 {
                let percent = ((downloaded as f64 / total_bytes as f64) * 100.0) as usize;
                let mb_done = (downloaded as f64) / (1024.0 * 1024.0);
                let mb_total = (total_bytes as f64) / (1024.0 * 1024.0);
                bar.update(percent, &format!("{:.1}/{:.1} MB", mb_done, mb_total));
            } else {
                let mb_done = (downloaded as f64) / (1024.0 * 1024.0);
                bar.update(50, &format!("{:.1} MB", mb_done));
            }
        }
        bar.finish(&format!("Downloaded {}", asset_name));

        let computed_sha256 = hex::encode(hasher.finalize()).to_lowercase();

        if let Some(expected) = expected_sha256 {
            if computed_sha256 == expected.to_lowercase() {
                println!(
                    " {} SHA-256 verified ({})",
                    apt_prefix("OK"),
                    c(
                        &format!("{}...", &computed_sha256[..12]),
                        &[Colors::BOLD, Colors::BRIGHT_GREEN]
                    )
                );
            } else {
                let _ = std::fs::remove_file(&temp_dest);
                println!(
                    " {} SHA-256 checksum mismatch for {}!",
                    apt_prefix("FAIL"),
                    asset_name
                );
                println!(
                    "        Expected: {}",
                    c(&expected, &[Colors::BOLD, Colors::BRIGHT_RED])
                );
                println!(
                    "        Computed: {}",
                    c(&computed_sha256, &[Colors::BOLD, Colors::BRIGHT_YELLOW])
                );
                return Err(format!("SHA-256 checksum mismatch for {}", asset_name));
            }
        }

        if std::fs::rename(&temp_dest, dest_path).is_err() {
            std::fs::copy(&temp_dest, dest_path)
                .map_err(|e| format!("Failed to move finished download: {}", e))?;
            let _ = std::fs::remove_file(&temp_dest);
        }
        Ok(())
    }

    pub async fn find_expected_sha256(
        &self,
        asset_name: &str,
        chosen_asset: Option<&CandidateAsset>,
        release_assets: Option<&[ReleaseAsset]>,
    ) -> Option<String> {
        if let Some(ca) = chosen_asset {
            if let Some(digest) = &ca.asset.digest {
                if let Some(hash) = digest.strip_prefix("sha256:") {
                    return Some(hash.trim().to_lowercase());
                }
            }
        }

        let r_assets = release_assets?;
        let asset_lower = asset_name.to_lowercase();

        let mut target_asset = None;
        for cand in r_assets {
            let c_name = cand.name.to_lowercase();
            if c_name == format!("{}.sha256", asset_lower)
                || c_name == format!("{}.sha256sum", asset_lower)
            {
                target_asset = Some(cand);
                break;
            }
        }

        if target_asset.is_none() {
            let manifest_patterns = [
                "sha256sums.txt", "sha256sums", "sha256sum.txt", "sha256sum", "sha256.txt",
                "sha256", "checksums.txt", "checksums_sha256.txt", "checksums", "checksums.sha256",
            ];
            for cand in r_assets {
                let c_name = cand.name.to_lowercase();
                if manifest_patterns.iter().any(|pat| c_name == *pat || c_name.ends_with(pat)) {
                    target_asset = Some(cand);
                    break;
                }
            }
        }

        if target_asset.is_none() {
            for cand in r_assets {
                let c_name = cand.name.to_lowercase();
                if (c_name.contains("sha256") || c_name.contains("checksum"))
                    && [".txt", ".sha256", ".sha256sum", ".sum", ".sums", ""].iter().any(|ext| c_name.ends_with(ext))
                {
                    target_asset = Some(cand);
                    break;
                }
            }
        }

        let chk_asset = target_asset?;
        let resp = self
            .client
            .get(&chk_asset.browser_download_url)
            .headers(self.headers(None))
            .send()
            .await
            .ok()?;

        let content = resp.text().await.ok()?;
        let hex_re = Regex::new(r"\b([a-fA-F0-9]{64})\b").ok()?;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(caps) = hex_re.captures(trimmed) {
                let hash = caps.get(1).unwrap().as_str().to_lowercase();
                if chk_asset.name.to_lowercase().starts_with(&asset_lower) {
                    return Some(hash);
                }
                if trimmed.to_lowercase().contains(&asset_lower) {
                    return Some(hash);
                }
            }
        }

        None
    }

    /// Compare two version strings (handles semver, prefixes like 'v', 'jq-', and prerelease tags)
    pub fn compare_versions(v1: &str, v2: &str) -> std::cmp::Ordering {
        fn extract_parts(v: &str) -> (Vec<u64>, Option<&str>) {
            let start = v.find(|c: char| c.is_ascii_digit()).unwrap_or(v.len());
            let s = &v[start..];

            let (num_part, pre_part) = if let Some(idx) = s.find(['-', '+', '~']) {
                (&s[..idx], Some(&s[idx + 1..]))
            } else {
                (s, None)
            };

            let nums: Vec<u64> = num_part
                .split('.')
                .filter_map(|p| p.parse::<u64>().ok())
                .collect();

            (nums, pre_part)
        }

        let (n1, p1) = extract_parts(v1);
        let (n2, p2) = extract_parts(v2);

        let max_len = n1.len().max(n2.len());
        for i in 0..max_len {
            let a = n1.get(i).copied().unwrap_or(0);
            let b = n2.get(i).copied().unwrap_or(0);
            if a != b {
                return a.cmp(&b);
            }
        }

        match (p1, p2) {
            (None, None) => std::cmp::Ordering::Equal,
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(a), Some(b)) => a.cmp(b),
        }
    }
}

mod urlencoding {
    pub fn encode(s: &str) -> String {
        let mut encoded = String::with_capacity(s.len());
        for b in s.as_bytes() {
            match b {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    encoded.push(*b as char);
                }
                _ => {
                    encoded.push_str(&format!("%{:02X}", b));
                }
            }
        }
        encoded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_tag_comparison() {
        let (repo, tag) = GitHubClient::parse_repo_and_tag("owner/repo@v2.0.0");
        assert_eq!(repo, Some("owner/repo".to_string()));
        assert_eq!(tag, Some("v2.0.0".to_string()));

        let (repo2, tag2) = GitHubClient::parse_repo_and_tag("owner/repo@1.4.0-beta");
        assert_eq!(repo2, Some("owner/repo".to_string()));
        assert_eq!(tag2, Some("1.4.0-beta".to_string()));

        assert_eq!(
            GitHubClient::compare_versions("v2.0.0", "1.4.0-beta"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            GitHubClient::compare_versions("1.4.0-beta", "2.0.0"),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            GitHubClient::compare_versions("v2.0.0", "2.0.0"),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            GitHubClient::compare_versions("1.4.0", "1.4.0-beta"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn test_parse_repo_and_tag() {
        assert_eq!(
            GitHubClient::parse_repo_and_tag("aristocratos/btop"),
            (Some("aristocratos/btop".to_string()), None)
        );
        assert_eq!(
            GitHubClient::parse_repo_and_tag("jqlang/jq@jq-1.7.1"),
            (Some("jqlang/jq".to_string()), Some("jq-1.7.1".to_string()))
        );
        assert_eq!(
            GitHubClient::parse_repo_and_tag("github:cli/cli@v2.40.0"),
            (Some("cli/cli".to_string()), Some("v2.40.0".to_string()))
        );
        assert_eq!(
            GitHubClient::parse_repo_and_tag("https://github.com/fastfetch-cli/fastfetch"),
            (Some("fastfetch-cli/fastfetch".to_string()), None)
        );
    }

    #[test]
    fn test_derive_pattern() {
        let pat = GitHubClient::derive_pattern("btop-x86_64-linux-musl.tar.gz", "v1.4.8");
        assert_eq!(pat, "^btop\\-x86_64\\-linux\\-musl\\.tar\\.gz$");

        let pat2 = GitHubClient::derive_pattern("ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz", "14.1.0");
        assert_eq!(pat2, "^ripgrep\\-.*\\-x86_64\\-unknown\\-linux\\-musl\\.tar\\.gz$");
    }

    #[test]
    fn test_asset_prioritization() {
        let client = GitHubClient::new();
        let assets = vec![
            ReleaseAsset {
                name: "app_1.0_amd64.deb".to_string(),
                size: 1000,
                browser_download_url: "url1".to_string(),
                digest: None,
            },
            ReleaseAsset {
                name: "app-1.0-x86_64.AppImage".to_string(),
                size: 2000,
                browser_download_url: "url2".to_string(),
                digest: None,
            },
            ReleaseAsset {
                name: "app-x86_64.tar.gz".to_string(),
                size: 800,
                browser_download_url: "url3".to_string(),
                digest: None,
            },
        ];

        // Default: AppImage > deb > tar
        let filtered = client.filter_assets(&assets, None, None, None);
        assert!(!filtered.is_empty());
        assert_eq!(filtered[0].format, "appimage");

        // Prefer deb: deb > AppImage
        let prefer_deb = client.filter_assets(&assets, Some("deb"), None, None);
        assert_eq!(prefer_deb[0].format, "deb");
    }
}
