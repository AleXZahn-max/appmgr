use crate::ui::{apt_prefix, c, Colors};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT_REPO_ID: &str = "core";
pub const DEFAULT_REPO_URL: &str = "https://alexzahn-max.github.io/appmgr-registry/index.json";

// ============================================================================
// Schema v1 Data Models
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Recipe {
    pub schema_version: u32,
    pub package: PackageMeta,
    pub source: SourceConfig,
    #[serde(default)]
    pub runtime: Option<RuntimeConfig>,
    #[serde(default)]
    pub sandbox: Option<SandboxConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageMeta {
    pub id: String,
    pub name: String,
    pub description: String,
    pub homepage: String,
    pub license: String,
    pub categories: Vec<String>,
    pub format: String,
    #[serde(default)]
    pub bin_name: Option<String>,
    #[serde(default)]
    pub gui: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceConfig {
    #[serde(rename = "type")]
    pub source_type: String,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub asset_regex: Option<String>,
    #[serde(default)]
    pub prefer: Option<String>,
    #[serde(default)]
    pub pre: Option<bool>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub version_regex: Option<String>,
    #[serde(default)]
    pub x86_64: Option<String>,
    #[serde(default)]
    pub aarch64: Option<String>,
    #[serde(default)]
    pub version_url: Option<String>,
    #[serde(default)]
    pub sha256_url: Option<String>,
    #[serde(default)]
    pub instance: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RuntimeConfig {
    #[serde(default)]
    pub default_env: Option<HashMap<String, String>>,
    #[serde(default)]
    pub default_args: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SandboxConfig {
    #[serde(default)]
    pub bwrap: Option<bool>,
    #[serde(default)]
    pub bind_opt: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegistryIndex {
    pub schema_version: u32,
    pub registry_name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    pub packages: Vec<RegistryPackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegistryPackage {
    pub id: String,
    pub name: String,
    pub description: String,
    pub homepage: String,
    pub license: String,
    pub categories: Vec<String>,
    pub format: String,
    #[serde(default)]
    pub bin_name: Option<String>,
    #[serde(default)]
    pub gui: Option<bool>,
    pub source: SourceConfig,
    #[serde(default)]
    pub runtime: Option<RuntimeConfig>,
    #[serde(default)]
    pub sandbox: Option<SandboxConfig>,
}

// ============================================================================
// sources.list Repository Manager
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoEntry {
    pub id: String,
    pub url: String,
    pub enabled: bool,
}

pub fn load_sources(path: &Path) -> Result<Vec<RepoEntry>, String> {
    if !path.exists() {
        let fallback = Path::new("/etc/appmgr/sources.list");
        if fallback.exists() {
            return parse_sources_file(fallback);
        }
        return create_default_sources(path);
    }
    parse_sources_file(path)
}

fn create_default_sources(path: &Path) -> Result<Vec<RepoEntry>, String> {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let default_content = format!(
        "# appmgr package repositories list\n# Syntax: <repo_id> <url_or_file_path>\n{} {}\n",
        DEFAULT_REPO_ID, DEFAULT_REPO_URL
    );
    let _ = fs::write(path, default_content);
    Ok(vec![RepoEntry {
        id: DEFAULT_REPO_ID.to_string(),
        url: DEFAULT_REPO_URL.to_string(),
        enabled: true,
    }])
}

fn parse_sources_file(path: &Path) -> Result<Vec<RepoEntry>, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read sources.list at {}: {}", path.display(), e))?;

    let mut entries = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(commented) = trimmed.strip_prefix('#') {
            let inner = commented.trim();
            let parts: Vec<&str> = inner.split_whitespace().collect();
            if parts.len() >= 2 && is_valid_repo_id(parts[0]) && is_valid_repo_url(parts[1]) {
                entries.push(RepoEntry {
                    id: parts[0].to_string(),
                    url: parts[1].to_string(),
                    enabled: false,
                });
            }
        } else {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 2 && is_valid_repo_id(parts[0]) && is_valid_repo_url(parts[1]) {
                entries.push(RepoEntry {
                    id: parts[0].to_string(),
                    url: parts[1].to_string(),
                    enabled: true,
                });
            }
        }
    }

    Ok(entries)
}

pub fn save_sources(path: &Path, entries: &[RepoEntry]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut out = String::from("# appmgr package repositories list\n# Syntax: <repo_id> <url_or_file_path>\n\n");
    for entry in entries {
        if entry.enabled {
            out.push_str(&format!("{} {}\n", entry.id, entry.url));
        } else {
            out.push_str(&format!("# {} {}\n", entry.id, entry.url));
        }
    }
    fs::write(path, out)
        .map_err(|e| format!("Failed to write sources.list at {}: {}", path.display(), e))
}

pub fn add_repo(path: &Path, id: &str, url: &str) -> Result<(), String> {
    if !is_valid_repo_id(id) {
        return Err(format!(
            "Invalid repository ID '{}': must consist of lowercase letters, digits, and hyphens",
            id
        ));
    }
    if !is_valid_repo_url(url) {
        return Err(format!(
            "Invalid repository URL '{}': must start with http://, https://, or file://",
            url
        ));
    }

    let mut entries = load_sources(path)?;
    if entries.iter().any(|e| e.id == id) {
        return Err(format!("Repository '{}' already exists in sources.list", id));
    }

    entries.push(RepoEntry {
        id: id.to_string(),
        url: url.to_string(),
        enabled: true,
    });
    save_sources(path, &entries)
}

pub fn remove_repo(path: &Path, id: &str, cache_dir: &Path) -> Result<(), String> {
    let mut entries = load_sources(path)?;
    let initial_len = entries.len();
    entries.retain(|e| e.id != id);

    if entries.len() == initial_len {
        return Err(format!("Repository '{}' not found in sources.list", id));
    }

    save_sources(path, &entries)?;

    // Remove cache files
    let cache_file = cache_dir.join(format!("{}.json", id));
    let etag_file = cache_dir.join(format!("{}.etag", id));
    let _ = fs::remove_file(cache_file);
    let _ = fs::remove_file(etag_file);

    Ok(())
}

pub fn set_repo_enabled(path: &Path, id: &str, enabled: bool) -> Result<(), String> {
    let mut entries = load_sources(path)?;
    let mut found = false;
    for entry in &mut entries {
        if entry.id == id {
            entry.enabled = enabled;
            found = true;
            break;
        }
    }
    if !found {
        return Err(format!("Repository '{}' not found in sources.list", id));
    }
    save_sources(path, &entries)
}

fn is_valid_repo_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn is_valid_repo_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://") || url.starts_with("file://")
}

// ============================================================================
// Registry Index Synchronization (appmgr update / sync)
// ============================================================================

pub async fn sync_registries(
    client: &reqwest::Client,
    sources_path: &Path,
    cache_dir: &Path,
) -> Result<(), String> {
    let entries = load_sources(sources_path)?;
    let enabled_entries: Vec<RepoEntry> = entries.into_iter().filter(|e| e.enabled).collect();

    if enabled_entries.is_empty() {
        println!(
            "{} No active repositories found in {}",
            apt_prefix("WARN"),
            sources_path.display()
        );
        return Ok(());
    }

    println!(
        "{} {}",
        c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
        c("Synchronizing package databases...", &[Colors::BOLD, Colors::BRIGHT_WHITE])
    );

    let _ = fs::create_dir_all(cache_dir);

    let mut total_packages = 0usize;
    let mut updated_repos = 0usize;

    for (idx, entry) in enabled_entries.iter().enumerate() {
        let repo_num = idx + 1;
        let cache_file = cache_dir.join(format!("{}.json", entry.id));
        let etag_file = cache_dir.join(format!("{}.etag", entry.id));

        if entry.url.starts_with("file://") {
            let local_path = PathBuf::from(entry.url.trim_start_matches("file://"));
            match fs::read_to_string(&local_path) {
                Ok(content) => {
                    let _ = fs::write(&cache_file, &content);
                    if let Ok(idx_data) = serde_json::from_str::<RegistryIndex>(&content) {
                        total_packages += idx_data.packages.len();
                    }
                    updated_repos += 1;
                    println!(
                        "Get:{} {} [{}] (local file)",
                        repo_num,
                        entry.url,
                        c(&entry.id, &[Colors::BOLD, Colors::BRIGHT_CYAN])
                    );
                }
                Err(e) => {
                    println!(
                        "{} Failed to read local repository file {}: {}",
                        apt_prefix("FAIL"),
                        local_path.display(),
                        e
                    );
                }
            }
            continue;
        }

        // HTTP/HTTPS Fetch with ETag caching
        let mut req = client.get(&entry.url);
        let existing_etag = fs::read_to_string(&etag_file).ok().map(|s| s.trim().to_string());
        if let Some(ref etag) = existing_etag {
            if cache_file.exists() {
                req = req.header("If-None-Match", etag.as_str());
            }
        }

        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status == reqwest::StatusCode::NOT_MODIFIED {
                    // 304 Not Modified
                    if let Ok(cached) = fs::read_to_string(&cache_file) {
                        if let Ok(idx_data) = serde_json::from_str::<RegistryIndex>(&cached) {
                            total_packages += idx_data.packages.len();
                        }
                    }
                    updated_repos += 1;
                    println!(
                        "Hit:{} [{}] (cached)",
                        repo_num,
                        c(&entry.id, &[Colors::BOLD, Colors::BRIGHT_GREEN])
                    );
                } else if status.is_success() {
                    // 200 OK
                    let new_etag = resp
                        .headers()
                        .get("etag")
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_string());

                    match resp.bytes().await {
                        Ok(bytes) => {
                            let size_kb = bytes.len() as f64 / 1024.0;
                            // Atomic write
                            let tmp_cache = cache_file.with_extension("tmp");
                            if let Ok(mut f) = fs::File::create(&tmp_cache) {
                                let _ = f.write_all(&bytes);
                                let _ = f.sync_all();
                                let _ = fs::rename(&tmp_cache, &cache_file);
                            }

                            if let Some(etag) = new_etag {
                                let _ = fs::write(&etag_file, etag);
                            }

                            if let Ok(idx_data) = serde_json::from_slice::<RegistryIndex>(&bytes) {
                                total_packages += idx_data.packages.len();
                            }

                            updated_repos += 1;
                            println!(
                                "Get:{} {} [{}] ({:.1} KiB)",
                                repo_num,
                                entry.url,
                                c(&entry.id, &[Colors::BOLD, Colors::BRIGHT_CYAN]),
                                size_kb
                            );
                        }
                        Err(e) => {
                            println!(
                                "{} Failed to read body from {}: {}",
                                apt_prefix("FAIL"),
                                entry.url,
                                e
                            );
                        }
                    }
                } else {
                    println!(
                        "{} HTTP {} from {}",
                        apt_prefix("FAIL"),
                        status,
                        entry.url
                    );
                }
            }
            Err(e) => {
                println!(
                    "{} Failed to connect to {}: {}",
                    apt_prefix("FAIL"),
                    entry.url,
                    e
                );
            }
        }
    }

    println!(
        "{} {} repositories updated, {} packages available.\n",
        c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
        c(&updated_repos.to_string(), &[Colors::BOLD, Colors::BRIGHT_GREEN]),
        c(&total_packages.to_string(), &[Colors::BOLD, Colors::BRIGHT_YELLOW])
    );

    Ok(())
}

// ============================================================================
// Registry Lookup & Search
// ============================================================================

pub fn get_cached_index(cache_dir: &Path, repo_id: &str) -> Option<RegistryIndex> {
    let path = cache_dir.join(format!("{}.json", repo_id));
    if let Ok(content) = fs::read_to_string(&path) {
        if let Ok(idx) = serde_json::from_str::<RegistryIndex>(&content) {
            return Some(idx);
        }
    }
    None
}

pub fn find_package_in_registries(
    cache_dir: &Path,
    sources: &[RepoEntry],
    pkg_id: &str,
) -> Option<(String, RegistryPackage)> {
    let lower_target = pkg_id.to_lowercase();
    for entry in sources {
        if !entry.enabled {
            continue;
        }
        if let Some(idx) = get_cached_index(cache_dir, &entry.id) {
            for pkg in idx.packages {
                if pkg.id.to_lowercase() == lower_target {
                    return Some((entry.id.clone(), pkg));
                }
            }
        }
    }
    None
}

pub fn search_registries(
    cache_dir: &Path,
    sources: &[RepoEntry],
    query: &str,
) -> Vec<(String, RegistryPackage)> {
    let q = query.to_lowercase();
    let mut results = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    for entry in sources {
        if !entry.enabled {
            continue;
        }
        if let Some(idx) = get_cached_index(cache_dir, &entry.id) {
            for pkg in idx.packages {
                if seen_ids.contains(&pkg.id) {
                    continue;
                }
                let id_match = pkg.id.to_lowercase().contains(&q);
                let name_match = pkg.name.to_lowercase().contains(&q);
                let desc_match = pkg.description.to_lowercase().contains(&q);
                let cat_match = pkg.categories.iter().any(|c| c.to_lowercase().contains(&q));

                if id_match || name_match || desc_match || cat_match {
                    seen_ids.insert(pkg.id.clone());
                    results.push((entry.id.clone(), pkg));
                }
            }
        }
    }

    results
}

// ============================================================================
// Clean Binary Name Helper
// ============================================================================

pub fn clean_binary_name(raw_name: &str, fallback_id: &str) -> String {
    let lower_raw = raw_name.to_lowercase();
    let lower_fallback = fallback_id.to_lowercase();

    if lower_raw == lower_fallback {
        return lower_fallback;
    }

    if lower_raw.starts_with(&format!("{}-", lower_fallback))
        || lower_raw.starts_with(&format!("{}_", lower_fallback))
    {
        return lower_fallback;
    }

    // Strip file extensions like .bin, .elf
    let base = raw_name.trim_end_matches(".bin").trim_end_matches(".elf");

    // Split on delimiters '-' and '_'
    let parts: Vec<&str> = base.split(['-', '_']).collect();
    if !parts.is_empty() {
        let first = parts[0].trim();
        if !first.is_empty() && first.chars().all(|c| c.is_ascii_alphabetic()) && parts.len() > 1 {
            let second = parts[1];
            let is_version = second.starts_with('v')
                || second.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false);
            let is_arch = second == "linux"
                || second == "x86"
                || second == "x86_64"
                || second == "amd64"
                || second == "arm64"
                || second == "aarch64";
            if is_version || is_arch {
                return first.to_lowercase();
            }
        }
    }

    // Pattern based stripping
    let patterns = [
        "-unknown-linux-gnu",
        "-unknown-linux-musl",
        "-linux-amd64",
        "-linux-arm64",
        "-linux-x86_64",
        "-linux-aarch64",
        "_linux_amd64",
        "_linux_arm64",
        "_linux_x86_64",
        "_linux_aarch64",
        "-x86_64-linux",
        "-aarch64-linux",
        "-linux64",
        "-linux",
        "_linux",
    ];

    let mut s = base;
    for pat in &patterns {
        if let Some(pos) = s.to_lowercase().find(pat) {
            s = &s[..pos];
            break;
        }
    }

    let clean = s.trim_matches(['-', '_']).to_lowercase();
    if clean.is_empty() {
        lower_fallback
    } else {
        clean
    }
}

// ============================================================================
// Maintainer Package Tools (appmgr pkg new / lint / test)
// ============================================================================

pub fn pkg_new(id: &str, output_path: Option<&Path>) -> Result<PathBuf, String> {
    if !is_valid_repo_id(id) {
        return Err(format!(
            "Invalid package ID '{}': must consist of lowercase letters, digits, and hyphens",
            id
        ));
    }

    let target_path = match output_path {
        Some(p) => p.to_path_buf(),
        None => PathBuf::from(format!("{}.toml", id)),
    };

    let template = format!(
        r#"schema_version = 1

[package]
id = "{id}"
name = "{name}"
description = "Fast, universal command-line utility"
homepage = "https://example.com"
license = "MIT"
categories = ["cli", "utilities"]
format = "auto"
# bin_name = "{id}"
# gui = false

[source]
type = "github"
repo = "owner/{id}"
# asset_regex = ".*linux.*x86_64.*"
# prefer = "tar"
# pre = false

# [runtime]
# default_args = []
# [runtime.default_env]
# KEY = "VALUE"

# [sandbox]
# bwrap = false
# bind_opt = []
"#,
        id = id,
        name = title_case(id)
    );

    fs::write(&target_path, template)
        .map_err(|e| format!("Failed to write recipe file at {}: {}", target_path.display(), e))?;

    Ok(target_path)
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

pub fn pkg_lint(file_path: &Path) -> Result<Recipe, Vec<String>> {
    let content = fs::read_to_string(file_path).map_err(|e| {
        vec![format!(
            "Failed to read file {}: {}",
            file_path.display(),
            e
        )]
    })?;

    let recipe: Recipe = toml::from_str(&content)
        .map_err(|e| vec![format!("TOML syntax error: {}", e)])?;

    let mut errors = Vec::new();

    if recipe.schema_version != 1 {
        errors.push(format!(
            "Unsupported schema_version {}: must be 1",
            recipe.schema_version
        ));
    }

    let pkg = &recipe.package;
    if !Regex::new(r"^[a-z0-9][a-z0-9-]*$")
        .unwrap()
        .is_match(&pkg.id)
    {
        errors.push(format!("package.id '{}' must match ^[a-z0-9][a-z0-9-]*$", pkg.id));
    }
    if pkg.name.trim().is_empty() {
        errors.push("package.name cannot be empty".to_string());
    }
    if pkg.description.trim().is_empty() {
        errors.push("package.description cannot be empty".to_string());
    }
    if pkg.homepage.trim().is_empty() {
        errors.push("package.homepage cannot be empty".to_string());
    }
    if pkg.license.trim().is_empty() {
        errors.push("package.license cannot be empty".to_string());
    }
    if pkg.categories.is_empty() {
        errors.push("package.categories must contain at least one category".to_string());
    }

    let valid_formats = ["deb", "appimage", "tar", "zip", "bin", "auto"];
    if !valid_formats.contains(&pkg.format.as_str()) {
        errors.push(format!(
            "Invalid package.format '{}'. Allowed: {}",
            pkg.format,
            valid_formats.join(", ")
        ));
    }

    let src = &recipe.source;
    match src.source_type.as_str() {
        "github" => {
            if let Some(ref r) = src.repo {
                if !Regex::new(r"^[^/]+/[^/]+$").unwrap().is_match(r) {
                    errors.push(format!("source.repo '{}' must be in 'owner/repo' format", r));
                }
            } else {
                errors.push("source.repo is required for type = 'github'".to_string());
            }
        }
        "redirect" => {
            if let Some(ref u) = src.url {
                if !u.starts_with("http://") && !u.starts_with("https://") {
                    errors.push("source.url must start with http:// or https://".to_string());
                }
            } else {
                errors.push("source.url is required for type = 'redirect'".to_string());
            }
        }
        "direct" => {
            if let Some(ref x) = src.x86_64 {
                if !x.starts_with("http://") && !x.starts_with("https://") {
                    errors.push("source.x86_64 must start with http:// or https://".to_string());
                }
            } else {
                errors.push("source.x86_64 is required for type = 'direct'".to_string());
            }
        }
        "gitlab" => {
            if src.instance.is_none() {
                errors.push("source.instance is required for type = 'gitlab'".to_string());
            }
            if src.project.is_none() {
                errors.push("source.project is required for type = 'gitlab'".to_string());
            }
        }
        other => {
            errors.push(format!(
                "Invalid source.type '{}'. Allowed: github, redirect, direct, gitlab",
                other
            ));
        }
    }

    if errors.is_empty() {
        Ok(recipe)
    } else {
        Err(errors)
    }
}

pub async fn pkg_test(
    client: &reqwest::Client,
    file_path: &Path,
    dry_run: bool,
) -> Result<(), String> {
    println!(
        "{} Linting package recipe at {} ...",
        apt_prefix("INFO"),
        file_path.display()
    );

    let recipe = match pkg_lint(file_path) {
        Ok(r) => {
            println!(
                "{} Schema v1 validation passed for '{}' ({})",
                apt_prefix("OK"),
                r.package.name,
                r.package.id
            );
            r
        }
        Err(errs) => {
            for err in &errs {
                println!("{} {}", apt_prefix("FAIL"), err);
            }
            return Err(format!("Recipe validation failed with {} error(s)", errs.len()));
        }
    };

    println!(
        "{} Testing upstream source resolution (type = '{}')...",
        apt_prefix("WORK"),
        recipe.source.source_type
    );

    match recipe.source.source_type.as_str() {
        "github" => {
            let repo = recipe.source.repo.as_ref().unwrap();
            let url = format!("https://api.github.com/repos/{}/releases/latest", repo);
            let resp = client
                .get(&url)
                .header("User-Agent", "appmgr")
                .send()
                .await
                .map_err(|e| format!("Failed to query GitHub API: {}", e))?;

            if !resp.status().is_success() {
                return Err(format!(
                    "GitHub API returned HTTP {} for {}",
                    resp.status(),
                    repo
                ));
            }

            let release: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| format!("Failed to parse GitHub JSON: {}", e))?;

            let tag = release["tag_name"].as_str().unwrap_or("unknown");
            let assets = release["assets"].as_array().map(|a| a.len()).unwrap_or(0);
            println!(
                "{} Found GitHub release: {} with {} asset(s)",
                apt_prefix("OK"),
                tag,
                assets
            );
        }
        "redirect" => {
            let url = recipe.source.url.as_ref().unwrap();
            // Test no-redirect client
            let no_redirect_client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

            let resp = no_redirect_client
                .head(url)
                .send()
                .await
                .map_err(|e| format!("Failed to connect to redirect URL: {}", e))?;

            let status = resp.status();
            if status.is_redirection() {
                let loc = resp
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("unknown");
                println!(
                    "{} HTTP {} -> redirects to: {}",
                    apt_prefix("OK"),
                    status,
                    loc
                );
            } else if status.is_success() {
                println!(
                    "{} HTTP {} (direct response)",
                    apt_prefix("OK"),
                    status
                );
            } else {
                return Err(format!("Redirect probe returned HTTP {}", status));
            }
        }
        "direct" => {
            let x86 = recipe.source.x86_64.as_ref().unwrap();
            let resp = client
                .head(x86)
                .send()
                .await
                .map_err(|e| format!("Failed to probe direct URL: {}", e))?;

            if !resp.status().is_success() {
                return Err(format!("Direct URL returned HTTP {}", resp.status()));
            }

            let len = resp
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown");
            println!(
                "{} Direct URL reachable: HTTP {} (Content-Length: {} bytes)",
                apt_prefix("OK"),
                resp.status(),
                len
            );
        }
        "gitlab" => {
            let instance = recipe
                .source
                .instance
                .as_deref()
                .unwrap_or("https://gitlab.com");
            let project = recipe.source.project.as_ref().unwrap();
            let encoded_project = urlencoding_simple(project);
            let url = format!("{}/api/v4/projects/{}/releases", instance, encoded_project);

            let resp = client
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("Failed to query GitLab API: {}", e))?;

            if !resp.status().is_success() {
                return Err(format!(
                    "GitLab API returned HTTP {} for {}",
                    resp.status(),
                    project
                ));
            }

            let releases: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| format!("Failed to parse GitLab JSON: {}", e))?;

            let first = releases.as_array().and_then(|a| a.first());
            if let Some(r) = first {
                let tag = r["tag_name"].as_str().unwrap_or("unknown");
                println!(
                    "{} Found GitLab release: {}",
                    apt_prefix("OK"),
                    tag
                );
            } else {
                println!("{} No GitLab releases found for this project", apt_prefix("WARN"));
            }
        }
        _ => {}
    }

    if dry_run {
        println!(
            "{} Dry-run test completed successfully. No changes made to system.",
            apt_prefix("DONE")
        );
    } else {
        println!(
            "{} Recipe test completed successfully.",
            apt_prefix("DONE")
        );
    }

    Ok(())
}

fn urlencoding_simple(s: &str) -> String {
    s.replace('/', "%2F")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_sources_list_parsing_and_priority() {
        let dir = tempdir().unwrap();
        let sources_file = dir.path().join("sources.list");

        let content = r#"
# Primary repository (priority 1)
core https://example.com/core/index.json

# Disabled community repository
# community https://example.com/community/index.json

# Local custom repo (priority 2)
custom file:///opt/custom/index.json
"#;
        fs::write(&sources_file, content).unwrap();

        let entries = load_sources(&sources_file).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].id, "core");
        assert!(entries[0].enabled);
        assert_eq!(entries[1].id, "community");
        assert!(!entries[1].enabled);
        assert_eq!(entries[2].id, "custom");
        assert!(entries[2].enabled);
    }

    #[test]
    fn test_sources_list_add_remove_toggle() {
        let dir = tempdir().unwrap();
        let sources_file = dir.path().join("sources.list");
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&cache_dir).unwrap();

        // 1. Initial default creation
        let initial = load_sources(&sources_file).unwrap();
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].id, "core");

        // 2. Add repo
        add_repo(&sources_file, "community", "https://example.com/community/index.json").unwrap();
        let after_add = load_sources(&sources_file).unwrap();
        assert_eq!(after_add.len(), 2);
        assert_eq!(after_add[1].id, "community");
        assert!(after_add[1].enabled);

        // 3. Disable repo
        set_repo_enabled(&sources_file, "community", false).unwrap();
        let after_disable = load_sources(&sources_file).unwrap();
        assert!(!after_disable[1].enabled);

        // 4. Remove repo
        remove_repo(&sources_file, "community", &cache_dir).unwrap();
        let after_remove = load_sources(&sources_file).unwrap();
        assert_eq!(after_remove.len(), 1);
        assert_eq!(after_remove[0].id, "core");
    }

    #[test]
    fn test_clean_binary_name() {
        assert_eq!(clean_binary_name("jq-linux-amd64", "jq"), "jq");
        assert_eq!(clean_binary_name("gh_2.40.1_linux_amd64", "gh"), "gh");
        assert_eq!(clean_binary_name("btop-x86_64-linux-musl", "btop"), "btop");
        assert_eq!(clean_binary_name("ripgrep-14.1.0-x86_64-unknown-linux-musl", "ripgrep"), "ripgrep");
        assert_eq!(clean_binary_name("bat-v0.24.0-x86_64-unknown-linux-musl", "bat"), "bat");
        assert_eq!(clean_binary_name("fd-v10.1.0-x86_64-unknown-linux-gnu", "fd"), "fd");
        assert_eq!(clean_binary_name("fzf-0.56.3-linux_amd64", "fzf"), "fzf");
        assert_eq!(clean_binary_name("myapp.bin", "myapp"), "myapp");
        assert_eq!(clean_binary_name("simple", "simple"), "simple");
    }

    #[test]
    fn test_pkg_lint_valid_and_invalid() {
        let dir = tempdir().unwrap();
        let valid_file = dir.path().join("valid.toml");
        let invalid_file = dir.path().join("invalid.toml");

        let valid_toml = r#"
schema_version = 1

[package]
id = "ripgrep"
name = "ripgrep"
description = "Fast line-oriented search tool"
homepage = "https://github.com/BurntSushi/ripgrep"
license = "MIT"
categories = ["cli", "search"]
format = "tar"
bin_name = "rg"

[source]
type = "github"
repo = "BurntSushi/ripgrep"
asset_regex = "x86_64.*linux.*tar\\.gz"
"#;
        fs::write(&valid_file, valid_toml).unwrap();
        let recipe = pkg_lint(&valid_file).unwrap();
        assert_eq!(recipe.package.id, "ripgrep");
        assert_eq!(recipe.package.bin_name.as_deref(), Some("rg"));
        assert_eq!(recipe.source.source_type, "github");

        // Invalid: bad id format, missing required fields
        let invalid_toml = r#"
schema_version = 1

[package]
id = "Invalid_Name!"
name = ""
description = ""
homepage = ""
license = ""
categories = []
format = "unknown_format"

[source]
type = "github"
"#;
        fs::write(&invalid_file, invalid_toml).unwrap();
        let errs = pkg_lint(&invalid_file).unwrap_err();
        assert!(!errs.is_empty());
        assert!(errs.iter().any(|e| e.contains("must match")));
        assert!(errs.iter().any(|e| e.contains("Invalid package.format")));
        assert!(errs.iter().any(|e| e.contains("source.repo is required")));
    }

    #[test]
    fn test_redirect_version_regex() {
        let regex_str = r"discord-([0-9.]+)\.deb";
        let re = Regex::new(regex_str).unwrap();

        let target_url = "https://dl.discordapp.net/apps/linux/0.0.78/discord-0.0.78.deb";
        let caps = re.captures(target_url).unwrap();
        let version = caps.get(1).unwrap().as_str();
        assert_eq!(version, "0.0.78");
    }
}
