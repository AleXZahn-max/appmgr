use crate::drivers::{read_index_metadata, write_index_metadata};
use flate2::read::GzDecoder;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AptPackage {
    pub package: String,
    pub version: String,
    pub architecture: String,
    pub filename: String,
    pub size: u64,
    pub sha256: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

type AptStamp = Option<(SystemTime, u64)>;
type AptIndexCacheMap = HashMap<PathBuf, (AptStamp, Arc<HashMap<String, AptPackage>>)>;

static APT_INDEX_CACHE: OnceLock<RwLock<AptIndexCacheMap>> = OnceLock::new();

fn apt_cache() -> &'static RwLock<AptIndexCacheMap> {
    APT_INDEX_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn file_stamp(path: &Path) -> AptStamp {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    Some((mtime, meta.len()))
}

pub fn invalidate_apt_cache(index_file: &Path) {
    if let Ok(mut guard) = apt_cache().write() {
        guard.remove(index_file);
    }
}

/// Compares Debian package versions (`[epoch:]upstream[-revision]`).
pub fn compare_apt_versions(v1: &str, v2: &str) -> Ordering {
    fn split_deb_ver(v: &str) -> (u64, &str, Option<&str>) {
        let trimmed = v.trim();
        let (epoch, rest) = match trimmed.split_once(':') {
            Some((ep, r)) => (ep.parse::<u64>().unwrap_or(0), r),
            None => (0, trimmed),
        };
        match rest.rsplit_once('-') {
            Some((up, rev)) => (epoch, up, Some(rev)),
            None => (epoch, rest, None),
        }
    }

    let (e1, u1, r1) = split_deb_ver(v1);
    let (e2, u2, r2) = split_deb_ver(v2);

    if e1 != e2 {
        return e1.cmp(&e2);
    }
    let up_ord = crate::github::GitHubClient::compare_versions(u1, u2);
    if up_ord != Ordering::Equal {
        return up_ord;
    }
    match (r1, r2) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => crate::github::GitHubClient::compare_versions(a, b),
    }
}

/// Parses Debian RFC822 format Packages text into `packages` map, keeping highest version.
pub fn parse_packages_stanzas_into(content: &str, packages: &mut HashMap<String, AptPackage>) {
    let stanzas = content.split("\n\n");

    for stanza in stanzas {
        let stanza_trimmed = stanza.trim();
        if stanza_trimmed.is_empty() {
            continue;
        }

        let mut current_key: Option<String> = None;
        let mut fields: HashMap<String, String> = HashMap::new();

        for line in stanza_trimmed.lines() {
            if line.starts_with(' ') || line.starts_with('\t') {
                if let Some(ref k) = current_key {
                    if let Some(v) = fields.get_mut(k) {
                        v.push('\n');
                        v.push_str(line.trim());
                    }
                }
            } else if let Some(colon_idx) = line.find(':') {
                let key = line[..colon_idx].trim().to_lowercase();
                let val = line[colon_idx + 1..].trim().to_string();
                current_key = Some(key.clone());
                fields.insert(key, val);
            }
        }

        let pkg_name = match fields.get("package") {
            Some(p) if !p.is_empty() => p.clone(),
            _ => continue,
        };
        let version = match fields.get("version") {
            Some(v) if !v.is_empty() => v.clone(),
            _ => continue,
        };
        let filename = match fields.get("filename") {
            Some(f) if !f.is_empty() => f.clone(),
            _ => continue,
        };
        let sha256 = fields
            .get("sha256")
            .cloned()
            .unwrap_or_default()
            .to_lowercase();
        let size = fields
            .get("size")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let architecture = fields
            .get("architecture")
            .cloned()
            .unwrap_or_else(|| "all".to_string());
        let homepage = fields.get("homepage").cloned();
        let description = fields.get("description").cloned();

        let apt_pkg = AptPackage {
            package: pkg_name.clone(),
            version,
            architecture,
            filename,
            size,
            sha256,
            homepage,
            description,
        };

        if let Some(existing) = packages.get(&pkg_name) {
            if compare_apt_versions(&apt_pkg.version, &existing.version) == Ordering::Greater {
                packages.insert(pkg_name, apt_pkg);
            }
        } else {
            packages.insert(pkg_name, apt_pkg);
        }
    }
}

/// Parses Debian RFC822 format Packages text (from Packages or decompressed Packages.gz)
#[allow(dead_code)]
pub fn parse_packages_stanzas(content: &str) -> HashMap<String, AptPackage> {
    let mut packages = HashMap::new();
    parse_packages_stanzas_into(content, &mut packages);
    packages
}

/// Architecture mapping: x86_64 -> amd64, aarch64 -> arm64
pub fn get_debian_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

fn load_component_etags(etag_file: &Path, default_component: &str) -> BTreeMap<String, String> {
    let Ok(raw) = fs::read_to_string(etag_file) else {
        return BTreeMap::new();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return BTreeMap::new();
    }
    if trimmed.starts_with('{') {
        if let Ok(map) = serde_json::from_str::<BTreeMap<String, String>>(trimmed) {
            return map;
        }
    }
    let mut map = BTreeMap::new();
    map.insert(default_component.to_string(), trimmed.to_string());
    map
}

fn save_component_etags(etag_file: &Path, etags: &BTreeMap<String, String>) {
    if etags.is_empty() {
        return;
    }
    if etags.len() == 1 {
        if let Some(val) = etags.values().next() {
            let _ = fs::write(etag_file, val);
        }
    } else if let Ok(json_str) = serde_json::to_string(etags) {
        let _ = fs::write(etag_file, json_str);
    }
}

enum ComponentFetch {
    NotModified,
    Fetched {
        text: String,
        etag: Option<String>,
        bytes: u64,
    },
}

async fn fetch_apt_component(
    client: &reqwest::Client,
    clean_base: &str,
    suite: &str,
    component: &str,
    arch: &str,
    existing_etag: Option<&str>,
    index_exists: bool,
) -> Result<ComponentFetch, String> {
    let gz_url = format!(
        "{}/dists/{}/{}/binary-{}/Packages.gz",
        clean_base, suite, component, arch
    );
    let plain_url = format!(
        "{}/dists/{}/{}/binary-{}/Packages",
        clean_base, suite, component, arch
    );

    let apply_etag = |mut req: reqwest::RequestBuilder| -> reqwest::RequestBuilder {
        if index_exists {
            if let Some(tag) = existing_etag {
                if tag.starts_with('"') || tag.starts_with("W/") {
                    req = req.header("If-None-Match", tag);
                } else {
                    req = req.header("If-Modified-Since", tag);
                }
            }
        }
        req
    };

    let resp_res = apply_etag(client.get(&gz_url)).send().await;
    let (target_url, resp) = match resp_res {
        Ok(r) if r.status() == StatusCode::OK || r.status() == StatusCode::NOT_MODIFIED => {
            (gz_url.clone(), r)
        }
        _ => {
            let r2 = apply_etag(client.get(&plain_url))
                .send()
                .await
                .map_err(|e| format!("Failed to reach {}: {}", plain_url, e))?;
            (plain_url.clone(), r2)
        }
    };

    if resp.status() == StatusCode::NOT_MODIFIED {
        return Ok(ComponentFetch::NotModified);
    }

    if !resp.status().is_success() {
        return Err(format!(
            "APT repository returned HTTP {} for {}",
            resp.status(),
            target_url
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
        .map_err(|e| format!("Failed to download APT packages index: {}", e))?;
    let downloaded_bytes = bytes.len() as u64;

    let text_content = if target_url.ends_with(".gz")
        || (bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b)
    {
        let mut gz = GzDecoder::new(&bytes[..]);
        let mut decompressed = String::new();
        gz.read_to_string(&mut decompressed)
            .map_err(|e| format!("Failed to decompress Packages.gz: {}", e))?;
        decompressed
    } else {
        String::from_utf8_lossy(&bytes).to_string()
    };

    Ok(ComponentFetch::Fetched {
        text: text_content,
        etag: new_etag,
        bytes: downloaded_bytes,
    })
}

/// Synchronizes all configured components of a Debian/APT repository into `cache_dir/<source_id>.json`.
pub async fn sync_apt_repo(
    client: &reqwest::Client,
    cache_dir: &Path,
    source_id: &str,
    base_url: &str,
    suite: &str,
    components: &[String],
) -> Result<(bool, usize, u64), String> {
    let arch = get_debian_arch();
    let index_file = cache_dir.join(format!("{}.json", source_id));
    let etag_file = cache_dir.join(format!("{}.etag", source_id));
    let clean_base = base_url.trim_end_matches('/');

    let effective_components: Vec<String> = if components.is_empty() {
        vec!["main".to_string()]
    } else {
        components.to_vec()
    };

    let existing_etags = load_component_etags(&etag_file, &effective_components[0]);
    let index_exists = index_file.exists();

    struct ComponentSyncPart {
        comp: String,
        etag: Option<String>,
        text: Option<String>,
    }

    let mut fetched_parts: Vec<ComponentSyncPart> = Vec::new();
    let mut any_changed = false;
    let mut total_downloaded_bytes: u64 = 0;

    for comp in &effective_components {
        let etag = existing_etags.get(comp).map(String::as_str);
        match fetch_apt_component(client, clean_base, suite, comp, arch, etag, index_exists).await?
        {
            ComponentFetch::NotModified => {
                fetched_parts.push(ComponentSyncPart {
                    comp: comp.clone(),
                    etag: etag.map(String::from),
                    text: None,
                });
            }
            ComponentFetch::Fetched {
                text,
                etag: new_etag,
                bytes,
            } => {
                any_changed = true;
                total_downloaded_bytes += bytes;
                fetched_parts.push(ComponentSyncPart {
                    comp: comp.clone(),
                    etag: new_etag,
                    text: Some(text),
                });
            }
        }
    }

    if !any_changed && index_exists {
        let count = load_cached_apt_count(&index_file);
        return Ok((false, count, 0));
    }

    // If some components changed and others returned 304, fetch the 304'd components unconditionally
    // so the merged package index remains complete across all components.
    let mut packages = HashMap::new();
    let mut new_etags = BTreeMap::new();

    for part in fetched_parts {
        match part.text {
            Some(text) => {
                parse_packages_stanzas_into(&text, &mut packages);
                if let Some(tag) = part.etag {
                    new_etags.insert(part.comp, tag);
                }
            }
            None => {
                if let ComponentFetch::Fetched {
                    text,
                    etag: fresh_etag,
                    bytes,
                } = fetch_apt_component(client, clean_base, suite, &part.comp, arch, None, false)
                    .await?
                {
                    total_downloaded_bytes += bytes;
                    parse_packages_stanzas_into(&text, &mut packages);
                    if let Some(tag) = fresh_etag.or(part.etag) {
                        new_etags.insert(part.comp, tag);
                    }
                }
            }
        }
    }

    let count = packages.len();
    let json_bytes = serde_json::to_vec(&packages)
        .map_err(|e| format!("Failed to serialize APT package cache: {}", e))?;
    let _ = fs::create_dir_all(cache_dir);
    fs::write(&index_file, json_bytes).map_err(|e| format!("Failed to write APT cache: {}", e))?;
    write_index_metadata(&index_file, count);
    save_component_etags(&etag_file, &new_etags);
    invalidate_apt_cache(&index_file);

    Ok((true, count, total_downloaded_bytes))
}

pub fn load_cached_apt_count(index_file: &Path) -> usize {
    if let Some(meta) = read_index_metadata(index_file) {
        return meta.package_count;
    }
    let pkgs = get_cached_apt_packages(index_file);
    let count = pkgs.len();
    if index_file.exists() {
        write_index_metadata(index_file, count);
    }
    count
}

pub fn get_cached_apt_packages(index_file: &Path) -> Arc<HashMap<String, AptPackage>> {
    let stamp = file_stamp(index_file);
    if let Ok(guard) = apt_cache().read() {
        if let Some((cached_stamp, arc)) = guard.get(index_file) {
            if *cached_stamp == stamp && stamp.is_some() {
                return Arc::clone(arc);
            }
        }
    }

    let parsed = if let Ok(bytes) = fs::read(index_file) {
        serde_json::from_slice::<HashMap<String, AptPackage>>(&bytes).unwrap_or_default()
    } else {
        HashMap::new()
    };
    let arc = Arc::new(parsed);
    if stamp.is_some() {
        if let Ok(mut guard) = apt_cache().write() {
            guard.insert(index_file.to_path_buf(), (stamp, Arc::clone(&arc)));
        }
    }
    arc
}

#[allow(dead_code)]
pub fn load_cached_apt_packages(index_file: &Path) -> HashMap<String, AptPackage> {
    (*get_cached_apt_packages(index_file)).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_parse_apt_packages_stanzas() {
        let sample = r#"
Package: gh
Version: 2.105.0
Architecture: amd64
License: MIT
Maintainer: GitHub
Installed-Size: 40663
Depends: git
Homepage: https://github.com/cli/cli
Priority: optional
Section: 
Filename: pool/main/g/gh/gh_2.105.0_amd64.deb
Size: 15121824
SHA256: 1dac939af3e84fb3861927c984b357531341be9403b889e4aa86e8822145a09d
Description: GitHub’s official command line tool.
 A tool to manage GitHub repositories, issues,
 and pull requests.

Package: code
Version: 1.97.0
Architecture: amd64
Filename: pool/main/c/code/code_1.97.0_amd64.deb
Size: 88538392
SHA256: fba25ea5af751561dd4ce5ed7d972f8ed142ce3f24da2ed4b852b515819ee97c
Description: Visual Studio Code
"#;

        let pkgs = parse_packages_stanzas(sample);
        assert_eq!(pkgs.len(), 2);

        let gh = pkgs.get("gh").expect("gh should exist");
        assert_eq!(gh.version, "2.105.0");
        assert_eq!(gh.architecture, "amd64");
        assert_eq!(gh.filename, "pool/main/g/gh/gh_2.105.0_amd64.deb");
        assert_eq!(gh.size, 15121824);
        assert_eq!(
            gh.sha256,
            "1dac939af3e84fb3861927c984b357531341be9403b889e4aa86e8822145a09d"
        );
        assert_eq!(gh.homepage.as_deref(), Some("https://github.com/cli/cli"));

        let code = pkgs.get("code").expect("code should exist");
        assert_eq!(code.version, "1.97.0");
        assert_eq!(
            code.sha256,
            "fba25ea5af751561dd4ce5ed7d972f8ed142ce3f24da2ed4b852b515819ee97c"
        );
    }

    #[test]
    fn test_apt_index_meta_and_process_cache() {
        let dir = tempdir().unwrap();
        let idx = dir.path().join("demo.json");
        let mut map = HashMap::new();
        map.insert(
            "gh".to_string(),
            AptPackage {
                package: "gh".to_string(),
                version: "2.40.0".to_string(),
                architecture: "amd64".to_string(),
                filename: "pool/main/g/gh/gh.deb".to_string(),
                size: 100,
                sha256: "abc".to_string(),
                homepage: None,
                description: None,
            },
        );
        fs::write(&idx, serde_json::to_vec(&map).unwrap()).unwrap();
        write_index_metadata(&idx, 1);

        assert_eq!(load_cached_apt_count(&idx), 1);
        let arc1 = get_cached_apt_packages(&idx);
        let arc2 = get_cached_apt_packages(&idx);
        assert!(Arc::ptr_eq(&arc1, &arc2));
        invalidate_apt_cache(&idx);

        assert_eq!(compare_apt_versions("1:1.0.0", "2.0.0"), Ordering::Greater);
        assert_eq!(
            compare_apt_versions("1.97.0-2", "1.97.0-10"),
            Ordering::Less
        );
    }
}
