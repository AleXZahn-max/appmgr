use flate2::read::GzDecoder;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;

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

/// Parses Debian RFC822 format Packages text (from Packages or decompressed Packages.gz)
pub fn parse_packages_stanzas(content: &str) -> HashMap<String, AptPackage> {
    let mut packages: HashMap<String, AptPackage> = HashMap::new();
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

        // If package already exists, compare version or keep latest
        if let Some(existing) = packages.get(&pkg_name) {
            if crate::github::GitHubClient::compare_versions(&apt_pkg.version, &existing.version)
                == std::cmp::Ordering::Greater
            {
                packages.insert(pkg_name, apt_pkg);
            }
        } else {
            packages.insert(pkg_name, apt_pkg);
        }
    }

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

/// Synchronizes a Debian/APT repository into ~/.cache/appmgr/indexes/<source_id>.json
pub async fn sync_apt_repo(
    client: &reqwest::Client,
    cache_dir: &Path,
    source_id: &str,
    base_url: &str,
    suite: &str,
    component: &str,
) -> Result<(bool, usize, u64), String> {
    let arch = get_debian_arch();
    let index_file = cache_dir.join(format!("{}.json", source_id));
    let etag_file = cache_dir.join(format!("{}.etag", source_id));

    let clean_base = base_url.trim_end_matches('/');
    let gz_url = format!("{}/dists/{}/{}/binary-{}/Packages.gz", clean_base, suite, component, arch);
    let plain_url = format!("{}/dists/{}/{}/binary-{}/Packages", clean_base, suite, component, arch);

    let existing_etag = fs::read_to_string(&etag_file).ok().map(|s| s.trim().to_string());

    // 1. Try compressed Packages.gz first
    let mut req = client.get(&gz_url);
    if let Some(ref tag) = existing_etag {
        if index_file.exists() {
            if tag.starts_with('"') || tag.starts_with("W/") {
                req = req.header("If-None-Match", tag.as_str());
            } else {
                req = req.header("If-Modified-Since", tag.as_str());
            }
        }
    }

    let resp_res = req.send().await;
    let (target_url, resp) = match resp_res {
        Ok(r) if r.status() == StatusCode::OK || r.status() == StatusCode::NOT_MODIFIED => {
            (gz_url.clone(), r)
        }
        _ => {
            // Fallback to uncompressed Packages
            let mut req2 = client.get(&plain_url);
            if let Some(ref tag) = existing_etag {
                if index_file.exists() {
                    if tag.starts_with('"') || tag.starts_with("W/") {
                        req2 = req2.header("If-None-Match", tag.as_str());
                    } else {
                        req2 = req2.header("If-Modified-Since", tag.as_str());
                    }
                }
            }
            let r2 = req2
                .send()
                .await
                .map_err(|e| format!("Failed to reach {}: {}", plain_url, e))?;
            (plain_url.clone(), r2)
        }
    };

    if resp.status() == StatusCode::NOT_MODIFIED {
        let count = load_cached_apt_count(&index_file);
        return Ok((false, count, 0));
    }

    if !resp.status().is_success() {
        return Err(format!("APT repository returned HTTP {} for {}", resp.status(), target_url));
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

    let text_content = if target_url.ends_with(".gz") || (bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b) {
        let mut gz = GzDecoder::new(&bytes[..]);
        let mut decompressed = String::new();
        gz.read_to_string(&mut decompressed)
            .map_err(|e| format!("Failed to decompress Packages.gz: {}", e))?;
        decompressed
    } else {
        String::from_utf8_lossy(&bytes).to_string()
    };

    let packages = parse_packages_stanzas(&text_content);
    let count = packages.len();

    let json_bytes = serde_json::to_vec(&packages)
        .map_err(|e| format!("Failed to serialize APT package cache: {}", e))?;
    let _ = fs::create_dir_all(cache_dir);
    fs::write(&index_file, json_bytes)
        .map_err(|e| format!("Failed to write APT cache: {}", e))?;

    if let Some(tag) = new_etag {
        let _ = fs::write(&etag_file, tag);
    }

    Ok((true, count, downloaded_bytes))
}

fn load_cached_apt_count(index_file: &Path) -> usize {
    if let Ok(bytes) = fs::read(index_file) {
        if let Ok(pkgs) = serde_json::from_slice::<HashMap<String, AptPackage>>(&bytes) {
            return pkgs.len();
        }
    }
    0
}

pub fn load_cached_apt_packages(index_file: &Path) -> HashMap<String, AptPackage> {
    if let Ok(bytes) = fs::read(index_file) {
        if let Ok(pkgs) = serde_json::from_slice::<HashMap<String, AptPackage>>(&bytes) {
            return pkgs;
        }
    }
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
