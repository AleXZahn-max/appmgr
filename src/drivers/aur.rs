use crate::ui::{apt_prefix, c, confirm_dialog, Colors};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AurResponse {
    pub version: u32,
    #[serde(rename = "type")]
    pub response_type: String,
    pub resultcount: usize,
    pub results: Vec<AurPackageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AurPackageInfo {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Version")]
    pub version: String,
    #[serde(rename = "Description")]
    pub description: Option<String>,
    #[serde(rename = "Maintainer")]
    pub maintainer: Option<String>,
    #[serde(rename = "PackageBase")]
    pub package_base: String,
    #[serde(rename = "URL")]
    pub url: Option<String>,
    #[serde(rename = "URLPath")]
    pub url_path: Option<String>,
    #[serde(rename = "Depends", default)]
    pub depends: Vec<String>,
    #[serde(rename = "MakeDepends", default)]
    pub make_depends: Vec<String>,
}

pub async fn search_aur(
    client: &reqwest::Client,
    rpc_url: &str,
    query: &str,
) -> Result<Vec<AurPackageInfo>, String> {
    let clean_rpc = rpc_url.trim_end_matches('/');
    let url = format!("{}/search/{}", clean_rpc, urlencoding(query));

    let resp = client
        .get(&url)
        .header("User-Agent", "appmgr/2.1.0")
        .send()
        .await
        .map_err(|e| format!("Failed to query AUR RPC: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("AUR RPC returned HTTP {}", resp.status()));
    }

    let parsed: AurResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse AUR RPC JSON: {}", e))?;

    Ok(parsed.results)
}

pub async fn get_aur_info(
    client: &reqwest::Client,
    rpc_url: &str,
    pkg_name: &str,
) -> Result<Option<AurPackageInfo>, String> {
    let clean_rpc = rpc_url.trim_end_matches('/');
    let url = format!("{}/info?arg[]={}", clean_rpc, urlencoding(pkg_name));

    let resp = client
        .get(&url)
        .header("User-Agent", "appmgr/2.1.0")
        .send()
        .await
        .map_err(|e| format!("Failed to query AUR info: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("AUR info returned HTTP {}", resp.status()));
    }

    let parsed: AurResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse AUR info JSON: {}", e))?;

    Ok(parsed.results.into_iter().next())
}

pub async fn install_aur_package(
    client: &reqwest::Client,
    info: &AurPackageInfo,
    assume_yes: bool,
) -> Result<bool, String> {
    // Check if makepkg is available
    if which::which("makepkg").is_err() {
        return Err(
            "Building AUR packages requires 'makepkg' on Arch Linux. Please install base-devel or run on an Arch-based distribution.".to_string(),
        );
    }

    println!(
        "\n{} Preparing AUR package '{}' ({}) ...",
        c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
        c(&info.name, &[Colors::BOLD, Colors::BRIGHT_CYAN]),
        info.version
    );
    if let Some(ref desc) = info.description {
        println!("   Description: {}", desc);
    }
    if let Some(ref m) = info.maintainer {
        println!("   Maintainer:  {}", m);
    }
    if !info.depends.is_empty() {
        println!("   Depends:     {}", info.depends.join(", "));
    }

    if !assume_yes && !confirm_dialog("Proceed with building and installing from AUR?", true) {
        println!("{} Installation aborted by user.", c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]));
        return Ok(false);
    }

    let temp_dir = tempfile::tempdir()
        .map_err(|e| format!("Failed to create temporary directory for AUR build: {}", e))?;

    let snapshot_url = format!(
        "https://aur.archlinux.org/cgit/aur.git/snapshot/{}.tar.gz",
        info.package_base
    );
    let tar_path = temp_dir.path().join(format!("{}.tar.gz", info.package_base));

    println!("{} Downloading snapshot from AUR...", apt_prefix("INFO"));
    let resp = client
        .get(&snapshot_url)
        .send()
        .await
        .map_err(|e| format!("Failed to download AUR snapshot from {}: {}", snapshot_url, e))?;

    if !resp.status().is_success() {
        return Err(format!("AUR snapshot returned HTTP {}", resp.status()));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read snapshot body: {}", e))?;
    std::fs::write(&tar_path, bytes)
        .map_err(|e| format!("Failed to save snapshot archive: {}", e))?;

    // Unpack tarball
    let tar_file = File::open(&tar_path)
        .map_err(|e| format!("Failed to open snapshot archive: {}", e))?;
    let tar = flate2::read::GzDecoder::new(tar_file);
    let mut archive = tar::Archive::new(tar);
    archive
        .unpack(temp_dir.path())
        .map_err(|e| format!("Failed to unpack AUR snapshot: {}", e))?;

    let pkg_dir = temp_dir.path().join(&info.package_base);
    let pkgbuild_file = pkg_dir.join("PKGBUILD");
    if !pkgbuild_file.exists() {
        return Err(format!("PKGBUILD not found in AUR snapshot: {}", pkgbuild_file.display()));
    }

    println!(
        "{} Building and installing package via makepkg in {} ...\n",
        apt_prefix("INFO"),
        pkg_dir.display()
    );

    let mut cmd = Command::new("makepkg");
    cmd.current_dir(&pkg_dir);
    cmd.arg("-si");
    if assume_yes {
        cmd.arg("--noconfirm");
    }

    let status = cmd
        .status()
        .map_err(|e| format!("Failed to execute makepkg: {}", e))?;

    if !status.success() {
        return Err(format!("makepkg exited with code: {:?}", status.code()));
    }

    println!(
        "\n{} AUR package '{}' successfully installed.",
        c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
        info.name
    );
    Ok(true)
}

fn urlencoding(input: &str) -> String {
    let mut encoded = String::new();
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{:02X}", b));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_aur_json() {
        let sample = r#"{
            "version": 5,
            "type": "search",
            "resultcount": 1,
            "results": [
                {
                    "Name": "paru-bin",
                    "Version": "2.0.4-1",
                    "Description": "Feature packed AUR helper",
                    "Maintainer": "morganamilo",
                    "PackageBase": "paru-bin",
                    "URL": "https://github.com/Morganamilo/paru",
                    "URLPath": "/cgit/aur.git/snapshot/paru-bin.tar.gz",
                    "Depends": ["pacman", "git"],
                    "MakeDepends": []
                }
            ]
        }"#;

        let parsed: AurResponse = serde_json::from_str(sample).expect("valid AUR json");
        assert_eq!(parsed.resultcount, 1);
        let pkg = &parsed.results[0];
        assert_eq!(pkg.name, "paru-bin");
        assert_eq!(pkg.package_base, "paru-bin");
        assert_eq!(pkg.version, "2.0.4-1");
        assert_eq!(pkg.depends, vec!["pacman", "git"]);
    }
}
