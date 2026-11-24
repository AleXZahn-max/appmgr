use crate::drivers::{read_index_metadata, write_index_metadata};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppImageHubPackage {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub github_repo: Option<String>,
    #[serde(default)]
    pub download_url: Option<String>,
}

type HubStamp = Option<(SystemTime, u64)>;
type HubIndexCacheMap = HashMap<PathBuf, (HubStamp, Arc<HashMap<String, AppImageHubPackage>>)>;

static HUB_INDEX_CACHE: OnceLock<RwLock<HubIndexCacheMap>> = OnceLock::new();

fn hub_cache() -> &'static RwLock<HubIndexCacheMap> {
    HUB_INDEX_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn file_stamp(path: &Path) -> HubStamp {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    Some((mtime, meta.len()))
}

pub fn invalidate_appimagehub_cache(index_file: &Path) {
    if let Ok(mut guard) = hub_cache().write() {
        guard.remove(index_file);
    }
}

pub fn sanitize_app_id(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = clean.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "app".to_string()
    } else {
        trimmed
    }
}

pub fn parse_appimagehub_feed(json_content: &str) -> HashMap<String, AppImageHubPackage> {
    let mut packages = HashMap::new();

    let root: serde_json::Value = match serde_json::from_str(json_content) {
        Ok(v) => v,
        Err(_) => return packages,
    };

    let items = match root.get("items").and_then(|i| i.as_array()) {
        Some(arr) => arr,
        None => return packages,
    };

    for item in items {
        let name = match item.get("name").and_then(|n| n.as_str()) {
            Some(n) if !n.trim().is_empty() => n.trim().to_string(),
            _ => continue,
        };

        let id = sanitize_app_id(&name);
        let description = item
            .get("description")
            .and_then(|d| d.as_str())
            .map(|s| s.trim().to_string());

        let categories = item
            .get("categories")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut github_repo = None;
        let mut download_url = None;

        if let Some(links) = item.get("links").and_then(|l| l.as_array()) {
            for link in links {
                let link_type = link.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let url = link.get("url").and_then(|u| u.as_str()).unwrap_or("");
                if url.is_empty() {
                    continue;
                }

                if link_type.eq_ignore_ascii_case("github") {
                    let clean = url
                        .trim_start_matches("https://github.com/")
                        .trim_matches('/');
                    if clean.contains('/') {
                        github_repo = Some(clean.to_string());
                    }
                } else if link_type.eq_ignore_ascii_case("download") {
                    download_url = Some(url.to_string());
                }
            }
        }

        let pkg = AppImageHubPackage {
            id: id.clone(),
            name,
            description,
            categories,
            github_repo,
            download_url,
        };

        packages.insert(id, pkg);
    }

    packages
}

pub async fn sync_appimagehub_repo(
    client: &reqwest::Client,
    cache_dir: &Path,
    source_id: &str,
    feed_url: &str,
) -> Result<(bool, usize, u64), String> {
    let index_file = cache_dir.join(format!("{}.json", source_id));
    let etag_file = cache_dir.join(format!("{}.etag", source_id));

    let existing_etag = fs::read_to_string(&etag_file)
        .ok()
        .map(|s| s.trim().to_string());
    let mut req = client.get(feed_url);

    if let Some(ref tag) = existing_etag {
        if index_file.exists() {
            if tag.starts_with('"') || tag.starts_with("W/") {
                req = req.header("If-None-Match", tag.as_str());
            } else {
                req = req.header("If-Modified-Since", tag.as_str());
            }
        }
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("Failed to fetch AppImageHub feed {}: {}", feed_url, e))?;

    if resp.status() == StatusCode::NOT_MODIFIED {
        let count = load_cached_appimagehub_count(&index_file);
        return Ok((false, count, 0));
    }

    if !resp.status().is_success() {
        return Err(format!(
            "AppImageHub returned HTTP {} for {}",
            resp.status(),
            feed_url
        ));
    }

    let new_etag = resp
        .headers()
        .get("etag")
        .or_else(|| resp.headers().get("last-modified"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read AppImageHub feed body: {}", e))?;
    let downloaded_bytes = bytes.len() as u64;

    let json_text = String::from_utf8_lossy(&bytes);
    let packages = parse_appimagehub_feed(&json_text);
    let count = packages.len();

    let json_bytes = serde_json::to_vec(&packages)
        .map_err(|e| format!("Failed to serialize AppImageHub cache: {}", e))?;
    let _ = fs::create_dir_all(cache_dir);
    fs::write(&index_file, json_bytes)
        .map_err(|e| format!("Failed to write AppImageHub index: {}", e))?;
    write_index_metadata(&index_file, count);
    invalidate_appimagehub_cache(&index_file);

    if let Some(tag) = new_etag {
        let _ = fs::write(&etag_file, tag);
    }

    Ok((true, count, downloaded_bytes))
}

pub fn load_cached_appimagehub_count(index_file: &Path) -> usize {
    if let Some(meta) = read_index_metadata(index_file) {
        return meta.package_count;
    }
    let pkgs = get_cached_appimagehub_packages(index_file);
    let count = pkgs.len();
    if index_file.exists() {
        write_index_metadata(index_file, count);
    }
    count
}

pub fn get_cached_appimagehub_packages(
    index_file: &Path,
) -> Arc<HashMap<String, AppImageHubPackage>> {
    let stamp = file_stamp(index_file);
    if let Ok(guard) = hub_cache().read() {
        if let Some((cached_stamp, arc)) = guard.get(index_file) {
            if *cached_stamp == stamp && stamp.is_some() {
                return Arc::clone(arc);
            }
        }
    }

    let parsed = if let Ok(bytes) = fs::read(index_file) {
        serde_json::from_slice::<HashMap<String, AppImageHubPackage>>(&bytes).unwrap_or_default()
    } else {
        HashMap::new()
    };
    let arc = Arc::new(parsed);
    if stamp.is_some() {
        if let Ok(mut guard) = hub_cache().write() {
            guard.insert(index_file.to_path_buf(), (stamp, Arc::clone(&arc)));
        }
    }
    arc
}

#[allow(dead_code)]
pub fn load_cached_appimagehub_packages(index_file: &Path) -> HashMap<String, AppImageHubPackage> {
    (*get_cached_appimagehub_packages(index_file)).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_appimagehub_feed() {
        let sample = r#"{
            "version": "1.0",
            "items": [
                {
                    "name": "Obsidian",
                    "description": "Markdown-based knowledge base",
                    "categories": ["Office", "Utility"],
                    "links": [
                        { "type": "GitHub", "url": "obsidianmd/obsidian-releases" },
                        { "type": "Download", "url": "https://github.com/obsidianmd/obsidian-releases/releases/download/v1.5.0/Obsidian.AppImage" }
                    ]
                },
                {
                    "name": "Krita",
                    "description": "Digital painting program",
                    "categories": ["Graphics"],
                    "links": [
                        { "type": "Download", "url": "https://download.kde.org/krita.AppImage" }
                    ]
                }
            ]
        }"#;

        let pkgs = parse_appimagehub_feed(sample);
        assert_eq!(pkgs.len(), 2);

        let obs = pkgs.get("obsidian").expect("obsidian should exist");
        assert_eq!(obs.name, "Obsidian");
        assert_eq!(
            obs.github_repo.as_deref(),
            Some("obsidianmd/obsidian-releases")
        );
        assert_eq!(obs.categories, vec!["Office", "Utility"]);

        let krita = pkgs.get("krita").expect("krita should exist");
        assert_eq!(krita.name, "Krita");
        assert_eq!(krita.github_repo, None);
        assert_eq!(
            krita.download_url.as_deref(),
            Some("https://download.kde.org/krita.AppImage")
        );
    }
}
