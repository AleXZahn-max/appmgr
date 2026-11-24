use crate::drivers::appimagehub::{get_cached_appimagehub_packages, sync_appimagehub_repo};
use crate::drivers::apt::{get_cached_apt_packages, sync_apt_repo};
use crate::drivers::aur::search_aur;
use crate::ui::{apt_prefix, c, Colors, Spinner};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::Ordering;

pub const DEFAULT_REGISTRY_ID: &str = "official";
pub const DEFAULT_REGISTRY_URL: &str =
    "https://alexzahn-max.github.io/appmgr-registry/sources.json";

pub const VALID_RECIPE_DRIVERS: &[&str] = &["apt", "appimagehub", "aur", "github"];
pub const VALID_SOURCE_DRIVERS: &[&str] = &["apt", "appimagehub", "aur", "github", "registry"];

// ============================================================================
// Sources Schema v2 Models
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceRecipe {
    pub schema_version: u32,
    pub source: SourceDefinition,
    #[serde(default)]
    pub apt: Option<AptSourceConfig>,
    #[serde(default)]
    pub api: Option<ApiSourceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceDefinition {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub base_url: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default = "default_architectures")]
    pub architectures: Vec<String>,
    #[serde(default = "default_mode")]
    pub mode: String,
}

fn default_true() -> bool {
    true
}
fn default_priority() -> i32 {
    100
}
fn default_architectures() -> Vec<String> {
    vec!["any".to_string()]
}
fn default_mode() -> String {
    "indexed".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AptSourceConfig {
    pub suite: String,
    pub components: Vec<String>,
    #[serde(default = "default_format_deb")]
    pub format: String,
}

fn default_format_deb() -> String {
    "deb".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ApiSourceConfig {
    #[serde(default)]
    pub feed_url: Option<String>,
    #[serde(default)]
    pub rpc_url: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcesIndex {
    pub schema_version: u32,
    pub registry_name: String,
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub source_count: usize,
    pub sources: Vec<SourceRecipe>,
}

// ============================================================================
// sources.list Config Entry
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceConfigEntry {
    pub driver: String,
    pub id: String,
    pub url: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default)]
    pub suite: Option<String>,
    #[serde(default)]
    pub components: Option<Vec<String>>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Parses comma- and/or whitespace-separated component lists uniformly.
pub fn parse_components_list(raw: &str) -> Vec<String> {
    raw.split(|ch: char| ch == ',' || ch.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

pub fn validate_source_driver(driver: &str) -> Result<String, String> {
    let norm = driver.trim().to_lowercase();
    if VALID_SOURCE_DRIVERS.contains(&norm.as_str()) {
        Ok(norm)
    } else {
        Err(format!(
            "Unsupported source driver '{}'. Valid drivers: {}",
            driver,
            VALID_SOURCE_DRIVERS.join(", ")
        ))
    }
}

pub fn parse_sources_list_content(content: &str) -> Vec<SourceConfigEntry> {
    let mut entries = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let is_disabled = trimmed.starts_with('#');
        let clean_line = if is_disabled {
            let without_hash = trimmed.trim_start_matches('#').trim();
            if let Some(rest) = without_hash.strip_prefix("disabled:") {
                rest.trim()
            } else {
                without_hash
            }
        } else {
            trimmed
        };

        if clean_line.is_empty()
            || clean_line.starts_with("Format:")
            || clean_line.starts_with("Generated at:")
        {
            continue;
        }

        let parts: Vec<&str> = clean_line.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }

        let driver = parts[0].to_lowercase();
        let id = parts[1].to_string();
        let url = parts[2].to_string();

        let mut priority = 100;
        let mut suite = None;
        let mut components = None;
        let mut mode = None;

        for opt in &parts[3..] {
            if let Some(eq_idx) = opt.find('=') {
                let k = &opt[..eq_idx].to_lowercase();
                let v = &opt[eq_idx + 1..];
                match k.as_str() {
                    "priority" => {
                        if let Ok(p) = v.parse::<i32>() {
                            priority = p;
                        }
                    }
                    "suite" => suite = Some(v.to_string()),
                    "components" => {
                        let parsed = parse_components_list(v);
                        if !parsed.is_empty() {
                            components = Some(parsed);
                        }
                    }
                    "mode" => mode = Some(v.to_string()),
                    _ => {}
                }
            }
        }

        let default_mode_val = if driver == "apt" || driver == "appimagehub" {
            "indexed".to_string()
        } else if driver == "registry" {
            "registry".to_string()
        } else {
            "online".to_string()
        };

        entries.push(SourceConfigEntry {
            driver,
            id,
            url,
            enabled: !is_disabled,
            priority,
            suite,
            components,
            mode: Some(mode.unwrap_or(default_mode_val)),
            name: None,
            description: None,
        });
    }

    entries
}

pub fn format_sources_list_content(entries: &[SourceConfigEntry]) -> String {
    let mut lines = Vec::new();
    lines.push("# appmgr official sources.list (managed)".to_string());
    lines.push("# Format: <driver> <id> <base_url> [options...]".to_string());
    lines.push("".to_string());

    for entry in entries {
        let mut opts = Vec::new();
        if let Some(ref s) = entry.suite {
            opts.push(format!("suite={}", s));
        }
        if let Some(ref comps) = entry.components {
            if !comps.is_empty() {
                opts.push(format!("components={}", comps.join(",")));
            }
        }
        opts.push(format!("priority={}", entry.priority));

        let opt_str = if opts.is_empty() {
            String::new()
        } else {
            format!(" {}", opts.join(" "))
        };

        let raw_line = format!("{} {} {}{}", entry.driver, entry.id, entry.url, opt_str);
        if entry.enabled {
            lines.push(raw_line);
        } else {
            lines.push(format!("# disabled: {}", raw_line));
        }
    }
    lines.push("".to_string());
    lines.join("\n")
}

pub fn load_sources(file_path: &Path) -> Result<Vec<SourceConfigEntry>, String> {
    if !file_path.exists() {
        if let Some(parent) = file_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let default_entries = vec![SourceConfigEntry {
            driver: "registry".to_string(),
            id: DEFAULT_REGISTRY_ID.to_string(),
            url: DEFAULT_REGISTRY_URL.to_string(),
            enabled: true,
            priority: 100,
            suite: None,
            components: None,
            mode: Some("registry".to_string()),
            name: Some("Official Sources Registry".to_string()),
            description: None,
        }];
        let _ = save_sources(file_path, &default_entries);
        return Ok(default_entries);
    }

    let content = fs::read_to_string(file_path).map_err(|e| {
        format!(
            "Failed to read sources.list at {}: {}",
            file_path.display(),
            e
        )
    })?;
    Ok(parse_sources_list_content(&content))
}

pub fn save_sources(file_path: &Path, entries: &[SourceConfigEntry]) -> Result<(), String> {
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config dir {}: {}", parent.display(), e))?;
    }
    let content = format_sources_list_content(entries);
    fs::write(file_path, content).map_err(|e| {
        format!(
            "Failed to write sources.list at {}: {}",
            file_path.display(),
            e
        )
    })
}

pub fn add_source(file_path: &Path, new_entry: SourceConfigEntry) -> Result<(), String> {
    let _ = validate_source_driver(&new_entry.driver)?;
    let mut entries = load_sources(file_path).unwrap_or_default();
    entries.retain(|e| e.id != new_entry.id);
    entries.push(new_entry);
    save_sources(file_path, &entries)
}

pub fn remove_source(file_path: &Path, id: &str) -> Result<bool, String> {
    let mut entries = load_sources(file_path)?;
    let initial_len = entries.len();
    entries.retain(|e| e.id != id);
    if entries.len() != initial_len {
        save_sources(file_path, &entries)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn set_source_enabled(file_path: &Path, id: &str, enabled: bool) -> Result<bool, String> {
    let mut entries = load_sources(file_path)?;
    let mut found = false;
    for e in &mut entries {
        if e.id == id {
            e.enabled = enabled;
            found = true;
            break;
        }
    }
    if found {
        save_sources(file_path, &entries)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Resolves all effective sources by merging local sources.list with remote meta-registry caches
pub fn get_effective_sources(
    sources_file: &Path,
    registries_cache_dir: &Path,
) -> Vec<SourceConfigEntry> {
    let local_entries = load_sources(sources_file).unwrap_or_default();
    let mut effective_map: HashMap<String, SourceConfigEntry> = HashMap::new();

    for entry in &local_entries {
        if entry.driver == "registry" && entry.enabled {
            let cache_file = registries_cache_dir.join(format!("{}.json", entry.id));
            if let Ok(bytes) = fs::read(&cache_file) {
                if let Ok(idx) = serde_json::from_slice::<SourcesIndex>(&bytes) {
                    for r in idx.sources {
                        let src = r.source;
                        let apt_suite = r.apt.as_ref().map(|a| a.suite.clone());
                        let apt_comps = r.apt.as_ref().map(|a| a.components.clone());
                        effective_map.insert(
                            src.id.clone(),
                            SourceConfigEntry {
                                driver: src.driver.clone(),
                                id: src.id.clone(),
                                url: src.base_url.clone(),
                                enabled: src.enabled,
                                priority: src.priority,
                                suite: apt_suite,
                                components: apt_comps,
                                mode: Some(src.mode),
                                name: Some(src.name),
                                description: src.description,
                            },
                        );
                    }
                }
            }
        }
    }

    for entry in local_entries {
        if entry.driver != "registry" {
            effective_map.insert(entry.id.clone(), entry);
        }
    }

    let mut result: Vec<SourceConfigEntry> = effective_map.into_values().collect();
    result.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.id.cmp(&b.id)));
    result
}

// ============================================================================
// Multi-threaded Synchronize Engine
// ============================================================================

pub async fn sync_all_sources(
    client: &reqwest::Client,
    sources_file: &Path,
    registries_cache_dir: &Path,
    indexes_cache_dir: &Path,
) -> Result<(usize, usize), String> {
    println!(
        "{} {}",
        c("::", &[Colors::BOLD, Colors::CYAN]),
        c(
            "Synchronizing package sources and databases...",
            &[Colors::BOLD, Colors::WHITE]
        )
    );

    let local_sources = load_sources(sources_file)?;
    let mut updated_sources_count = 0;
    let mut total_packages = 0;
    let mut seq_counter = 1;

    // 1. Update meta-registries (sources.json)
    for entry in &local_sources {
        if entry.driver != "registry" || !entry.enabled {
            continue;
        }

        let num = seq_counter;
        seq_counter += 1;

        let cache_file = registries_cache_dir.join(format!("{}.json", entry.id));
        let etag_file = registries_cache_dir.join(format!("{}.etag", entry.id));

        let mut req = client.get(&entry.url);
        let existing_etag = fs::read_to_string(&etag_file)
            .ok()
            .map(|s| s.trim().to_string());
        if let Some(ref etag) = existing_etag {
            if cache_file.exists() {
                if etag.starts_with('"') || etag.starts_with("W/") {
                    req = req.header("If-None-Match", etag.as_str());
                } else {
                    req = req.header("If-Modified-Since", etag.as_str());
                }
            }
        }

        let spinner = Spinner::new(&format!("Fetching registry [{}]...", entry.id));
        let resp_res = req.send().await;
        spinner.stop();

        match resp_res {
            Ok(resp) if resp.status() == StatusCode::NOT_MODIFIED => {
                println!(
                    "Hit:{} [{}] {}",
                    num,
                    c(&entry.id, &[Colors::BOLD, Colors::GREEN]),
                    c("(304 Not Modified)", &[Colors::DIM])
                );
            }
            Ok(resp) if resp.status().is_success() => {
                let new_etag = resp
                    .headers()
                    .get("etag")
                    .or_else(|| resp.headers().get("last-modified"))
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());

                if let Ok(bytes) = resp.bytes().await {
                    let kb = bytes.len() as f64 / 1024.0;
                    println!(
                        "Get:{} {} [{}] {}",
                        num,
                        c(&entry.url, &[Colors::DIM]),
                        c(&entry.id, &[Colors::BOLD, Colors::CYAN]),
                        c(&format!("({:.1} KiB)", kb), &[Colors::DIM])
                    );
                    let _ = fs::create_dir_all(registries_cache_dir);
                    let _ = fs::write(&cache_file, &bytes);
                    if let Some(tag) = new_etag {
                        let _ = fs::write(&etag_file, tag);
                    }
                    updated_sources_count += 1;
                }
            }
            Ok(resp) => {
                println!(
                    " {} Failed to fetch registry {} [HTTP {}]",
                    apt_prefix("WARN"),
                    c(&entry.url, &[Colors::DIM]),
                    resp.status()
                );
            }
            Err(e) => {
                println!(
                    " {} Network error fetching registry {}: {}",
                    apt_prefix("WARN"),
                    c(&entry.url, &[Colors::DIM]),
                    e
                );
            }
        }
    }

    // 2. Reload effective sources
    let effective = get_effective_sources(sources_file, registries_cache_dir);
    let _ = fs::create_dir_all(indexes_cache_dir);

    // 3. Concurrently sync indexed drivers (apt, appimagehub)
    let mut tasks = Vec::new();
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(6));

    let indexed_sources: Vec<_> = effective
        .into_iter()
        .filter(|src| {
            src.enabled
                && (src.mode.as_deref() == Some("indexed")
                    || src.driver == "apt"
                    || src.driver == "appimagehub")
        })
        .collect();

    let total_indexed = indexed_sources.len();
    let spinner = if total_indexed > 0 {
        Some(Spinner::with_counter(
            "Synchronizing package indexes...",
            total_indexed,
            "checked",
        ))
    } else {
        None
    };
    let counter = spinner.as_ref().map(|s| s.counter());

    for src in indexed_sources {
        let permit = sem.clone();
        let http = client.clone();
        let cache_dir = indexes_cache_dir.to_path_buf();
        let num = seq_counter;
        seq_counter += 1;
        let ctr = counter.clone();

        tasks.push(tokio::spawn(async move {
            let _permit = permit.acquire().await.unwrap();
            let result = match src.driver.as_str() {
                "apt" => {
                    let suite = src.suite.as_deref().unwrap_or("stable");
                    let components = src
                        .components
                        .clone()
                        .filter(|c| !c.is_empty())
                        .unwrap_or_else(|| vec!["main".to_string()]);
                    let res =
                        sync_apt_repo(&http, &cache_dir, &src.id, &src.url, suite, &components)
                            .await;
                    (num, src, res)
                }
                "appimagehub" => {
                    let res = sync_appimagehub_repo(&http, &cache_dir, &src.id, &src.url).await;
                    (num, src, res)
                }
                _ => (num, src, Ok((false, 0, 0))),
            };
            if let Some(ref c_ref) = ctr {
                c_ref.fetch_add(1, Ordering::Relaxed);
            }
            result
        }));
    }

    let mut completed_results = Vec::with_capacity(tasks.len());
    for task in tasks {
        if let Ok(item) = task.await {
            completed_results.push(item);
        }
    }
    if let Some(s) = spinner {
        s.stop();
    }

    for (num, src, res) in completed_results {
        match res {
            Ok((changed, count, bytes)) => {
                total_packages += count;
                if changed {
                    updated_sources_count += 1;
                    let kb = bytes as f64 / 1024.0;
                    println!(
                        "Get:{} {} [{}] {}",
                        num,
                        c(&src.url, &[Colors::DIM]),
                        c(&src.id, &[Colors::BOLD, Colors::CYAN]),
                        c(&format!("({:.1} KiB, {} pkgs)", kb, count), &[Colors::DIM])
                    );
                } else {
                    println!(
                        "Hit:{} [{}] {}",
                        num,
                        c(&src.id, &[Colors::BOLD, Colors::GREEN]),
                        c(&format!("(cached, {} pkgs)", count), &[Colors::DIM])
                    );
                }
            }
            Err(e) => {
                println!(
                    " {} Failed to update source [{}] {}: {}",
                    apt_prefix("WARN"),
                    c(&src.id, &[Colors::BOLD, Colors::YELLOW]),
                    c(&src.url, &[Colors::DIM]),
                    e
                );
            }
        }
    }

    println!(
        "\n{} Sources updated successfully. {} packages available in index.\n",
        c("::", &[Colors::BOLD, Colors::GREEN]),
        c(&total_packages.to_string(), &[Colors::BOLD, Colors::WHITE])
    );

    Ok((updated_sources_count, total_packages))
}

// ============================================================================
// Unified Search Across Indexed and Online Drivers
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct UnifiedSearchResult {
    pub source_id: String,
    pub driver: String,
    pub package_id: String,
    pub name: String,
    pub version: String,
    pub format: String,
    pub description: String,
    pub priority: i32,
}

#[allow(clippy::too_many_arguments)]
pub async fn search_all_sources(
    client: &reqwest::Client,
    sources_file: &Path,
    registries_cache_dir: &Path,
    indexes_cache_dir: &Path,
    query: &str,
    filter_source: Option<&str>,
    filter_driver: Option<&str>,
    offline: bool,
) -> Vec<UnifiedSearchResult> {
    let effective = get_effective_sources(sources_file, registries_cache_dir);
    let mut results = Vec::new();
    let lower_q = query.to_lowercase();

    for src in &effective {
        if !src.enabled {
            continue;
        }
        if let Some(fs) = filter_source {
            if !src.id.eq_ignore_ascii_case(fs) {
                continue;
            }
        }
        if let Some(fd) = filter_driver {
            if !src.driver.eq_ignore_ascii_case(fd) {
                continue;
            }
        }

        let idx_file = indexes_cache_dir.join(format!("{}.json", src.id));
        if !idx_file.exists() {
            continue;
        }

        match src.driver.as_str() {
            "apt" => {
                let pkgs = get_cached_apt_packages(&idx_file);
                for (name, p) in pkgs.iter() {
                    if name.to_lowercase().contains(&lower_q)
                        || p.description
                            .as_ref()
                            .map(|d| d.to_lowercase().contains(&lower_q))
                            .unwrap_or(false)
                    {
                        results.push(UnifiedSearchResult {
                            source_id: src.id.clone(),
                            driver: "apt".to_string(),
                            package_id: p.package.clone(),
                            name: p.package.clone(),
                            version: p.version.clone(),
                            format: "deb".to_string(),
                            description: p.description.clone().unwrap_or_default(),
                            priority: src.priority,
                        });
                    }
                }
            }
            "appimagehub" => {
                let pkgs = get_cached_appimagehub_packages(&idx_file);
                for (id, p) in pkgs.iter() {
                    if id.contains(&lower_q)
                        || p.name.to_lowercase().contains(&lower_q)
                        || p.description
                            .as_ref()
                            .map(|d| d.to_lowercase().contains(&lower_q))
                            .unwrap_or(false)
                    {
                        results.push(UnifiedSearchResult {
                            source_id: src.id.clone(),
                            driver: "appimagehub".to_string(),
                            package_id: p.id.clone(),
                            name: p.name.clone(),
                            version: "latest".to_string(),
                            format: "appimage".to_string(),
                            description: p.description.clone().unwrap_or_default(),
                            priority: src.priority,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    if !offline {
        for src in &effective {
            if !src.enabled {
                continue;
            }
            if let Some(fs) = filter_source {
                if !src.id.eq_ignore_ascii_case(fs) {
                    continue;
                }
            }
            if let Some(fd) = filter_driver {
                if !src.driver.eq_ignore_ascii_case(fd) {
                    continue;
                }
            }

            if src.driver == "aur" {
                if let Ok(aur_pkgs) = search_aur(client, &src.url, query).await {
                    for ap in aur_pkgs {
                        results.push(UnifiedSearchResult {
                            source_id: src.id.clone(),
                            driver: "aur".to_string(),
                            package_id: ap.name.clone(),
                            name: ap.name.clone(),
                            version: ap.version,
                            format: "aur".to_string(),
                            description: ap.description.unwrap_or_default(),
                            priority: src.priority,
                        });
                    }
                }
            }
        }
    }

    results.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.package_id.cmp(&b.package_id))
    });
    results
}

// ============================================================================
// Source Schema v2 Linting and Testing
// ============================================================================

/// Renders a valid Schema v2 TOML source recipe template for `id`.
pub fn render_source_recipe_template(id: &str) -> String {
    format!(
        r#"schema_version = 2

[source]
id = "{id}"
name = "{id}"
description = "Repository source for {id}"
driver = "apt"
base_url = "https://example.com/packages"
priority = 500
enabled = true
architectures = ["amd64", "arm64"]
mode = "indexed"

[apt]
suite = "stable"
components = ["main"]
format = "deb"
"#
    )
}

pub fn source_lint(file_path: &Path) -> Result<SourceRecipe, Vec<String>> {
    let mut errors = Vec::new();

    let content = match fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) => {
            return Err(vec![format!(
                "Failed to read source file {}: {}",
                file_path.display(),
                e
            )])
        }
    };

    let recipe: SourceRecipe = match toml::from_str(&content) {
        Ok(r) => r,
        Err(e) => return Err(vec![format!("TOML syntax error: {}", e)]),
    };

    if recipe.schema_version != 2 {
        errors.push(format!(
            "schema_version must be 2, got {}",
            recipe.schema_version
        ));
    }

    let src = &recipe.source;
    if src.id.is_empty() {
        errors.push("source.id cannot be empty".to_string());
    }
    if src.name.is_empty() {
        errors.push("source.name cannot be empty".to_string());
    }

    if !VALID_RECIPE_DRIVERS.contains(&src.driver.as_str()) {
        errors.push(format!(
            "Invalid source.driver '{}'. Must be one of {:?}",
            src.driver, VALID_RECIPE_DRIVERS
        ));
    }

    if !src.base_url.starts_with("http://") && !src.base_url.starts_with("https://") {
        errors.push(format!(
            "source.base_url must be an HTTP/HTTPS URL, got '{}'",
            src.base_url
        ));
    }

    if src.driver == "apt" {
        match recipe.apt {
            Some(ref apt) => {
                if apt.suite.is_empty() {
                    errors.push("apt.suite cannot be empty".to_string());
                }
                if apt.components.is_empty() {
                    errors.push("apt.components must contain at least one component".to_string());
                }
            }
            None => errors.push("[apt] table is required when source.driver = 'apt'".to_string()),
        }
    }

    if errors.is_empty() {
        Ok(recipe)
    } else {
        Err(errors)
    }
}

pub fn build_source_test_urls(recipe: &SourceRecipe) -> Vec<String> {
    let src = &recipe.source;
    if src.driver == "apt" {
        if let Some(ref apt) = recipe.apt {
            let arch = crate::drivers::apt::get_debian_arch();
            let clean = src.base_url.trim_end_matches('/');
            return apt
                .components
                .iter()
                .map(|comp| {
                    format!(
                        "{}/dists/{}/{}/binary-{}/Packages.gz",
                        clean, apt.suite, comp, arch
                    )
                })
                .collect();
        }
    } else if src.driver == "aur" {
        let clean = src.base_url.trim_end_matches('/');
        return vec![format!("{}/info?arg[]=paru-bin", clean)];
    }
    vec![src.base_url.clone()]
}

pub async fn source_test(
    file_path: &Path,
    client: &reqwest::Client,
    dry_run: bool,
) -> Result<(), String> {
    let recipe = source_lint(file_path).map_err(|errs| errs.join("\n"))?;
    let src = &recipe.source;
    let test_urls = build_source_test_urls(&recipe);

    if dry_run {
        println!(
            "{} [dry-run] Validated Schema v2 source '{}' ({})",
            apt_prefix("OK"),
            src.id,
            src.driver
        );
        for url in &test_urls {
            println!(
                "{} [dry-run] Would check HTTP endpoint: {}",
                apt_prefix("INFO"),
                url
            );
        }
        return Ok(());
    }

    println!(
        "{} Testing endpoint accessibility for source '{}' ({}) ...",
        apt_prefix("INFO"),
        src.id,
        src.driver
    );

    for test_url in &test_urls {
        println!(
            "{} Checking HTTP GET on {} ...",
            apt_prefix("INFO"),
            test_url
        );
        let resp = client
            .get(test_url)
            .send()
            .await
            .map_err(|e| format!("Failed to reach {}: {}", test_url, e))?;

        if resp.status().is_success() {
            println!(
                "{} Endpoint returned HTTP {} OK",
                apt_prefix("OK"),
                resp.status()
            );
        } else {
            return Err(format!(
                "Endpoint {} returned HTTP {}",
                test_url,
                resp.status()
            ));
        }
    }

    Ok(())
}

// ============================================================================
// Clean Binary Name Helper
// ============================================================================

pub fn clean_binary_name(asset_name: &str, fallback_id: &str) -> String {
    let lower_asset = asset_name.to_lowercase();
    let lower_fallback = fallback_id.to_lowercase();

    let clean = lower_asset
        .trim_end_matches(".tar.gz")
        .trim_end_matches(".tgz")
        .trim_end_matches(".tar.xz")
        .trim_end_matches(".txz")
        .trim_end_matches(".tar.zst")
        .trim_end_matches(".tzst")
        .trim_end_matches(".tar.bz2")
        .trim_end_matches(".tbz2")
        .trim_end_matches(".tar")
        .trim_end_matches(".zip")
        .trim_end_matches(".appimage")
        .trim_end_matches(".deb")
        .trim_end_matches(".bin");

    if clean == lower_fallback
        || clean.starts_with(&format!("{}-", lower_fallback))
        || clean.starts_with(&format!("{}_", lower_fallback))
    {
        return fallback_id.to_string();
    }

    let parts: Vec<&str> = clean.split(['-', '_']).collect();
    if let Some(&first) = parts.first() {
        if !first.is_empty() && !first.chars().all(|c| c.is_ascii_digit()) {
            return first.to_string();
        }
    }

    fallback_id.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_binary_name() {
        assert_eq!(clean_binary_name("jq-linux-amd64", "jq"), "jq");
        assert_eq!(
            clean_binary_name("ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz", "ripgrep"),
            "ripgrep"
        );
        assert_eq!(clean_binary_name("gh_2.40.1_linux_amd64.deb", "gh"), "gh");
        assert_eq!(
            clean_binary_name("fd-v8.7.0-x86_64-unknown-linux-gnu.tar.gz", "fd"),
            "fd"
        );
    }

    #[test]
    fn test_parse_components_list_and_driver_validation() {
        assert_eq!(
            parse_components_list("main,contrib non-free"),
            vec!["main", "contrib", "non-free"]
        );
        assert_eq!(
            parse_components_list(" main , universe "),
            vec!["main", "universe"]
        );
        assert!(validate_source_driver("apt").is_ok());
        assert!(validate_source_driver("gitlab").is_err());
    }

    #[test]
    fn test_parse_sources_list_parsing_and_options() {
        let sample = r#"
# appmgr sources.list
registry official https://alexzahn-max.github.io/appmgr-registry/sources.json
apt apt-gh https://cli.github.com/packages suite=stable components=main,contrib priority=100
# disabled: apt apt-vscode https://packages.microsoft.com/repos/code suite=stable components=main priority=90
appimagehub appimagehub https://appimage.github.io/feed.json priority=80
"#;

        let entries = parse_sources_list_content(sample);
        assert_eq!(entries.len(), 4);

        let reg = &entries[0];
        assert_eq!(reg.driver, "registry");
        assert_eq!(reg.id, "official");
        assert!(reg.enabled);

        let gh = &entries[1];
        assert_eq!(gh.driver, "apt");
        assert_eq!(gh.id, "apt-gh");
        assert_eq!(gh.suite.as_deref(), Some("stable"));
        assert_eq!(
            gh.components,
            Some(vec!["main".to_string(), "contrib".to_string()])
        );
        assert_eq!(gh.priority, 100);
        assert!(gh.enabled);

        let vscode = &entries[2];
        assert_eq!(vscode.id, "apt-vscode");
        assert!(!vscode.enabled);

        let appimage = &entries[3];
        assert_eq!(appimage.driver, "appimagehub");
        assert_eq!(appimage.priority, 80);
    }

    #[test]
    fn test_pkg_new_template_passes_source_lint() {
        let dir = tempfile::tempdir().unwrap();
        let recipe_path = dir.path().join("my-source.toml");
        let content = render_source_recipe_template("my-source");
        fs::write(&recipe_path, content).unwrap();

        let parsed = source_lint(&recipe_path).expect("generated template must pass source_lint");
        assert_eq!(parsed.schema_version, 2);
        assert_eq!(parsed.source.id, "my-source");
        assert_eq!(parsed.source.driver, "apt");
        assert!(parsed.apt.is_some());
    }
}
