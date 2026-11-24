use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn default_user() -> String {
    "user".to_string()
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackageOrigin {
    #[serde(rename = "github")]
    GitHub {
        repo: String,
        #[serde(default)]
        tag: Option<String>,
        #[serde(default)]
        etag: Option<String>,
        #[serde(default)]
        asset_pattern: Option<String>,
    },
    #[serde(rename = "apt")]
    Apt { source_id: String, package: String },
    #[serde(rename = "appimagehub")]
    AppImageHub {
        source_id: String,
        app_id: String,
        #[serde(default)]
        github_repo: Option<String>,
        #[serde(default)]
        tag: Option<String>,
        #[serde(default)]
        etag: Option<String>,
        #[serde(default)]
        asset_pattern: Option<String>,
        #[serde(default)]
        download_url: Option<String>,
    },
    #[serde(rename = "local")]
    Local {
        #[serde(default)]
        source_file: Option<String>,
    },
}

impl Default for PackageOrigin {
    fn default() -> Self {
        Self::Local { source_file: None }
    }
}

impl<'de> Deserialize<'de> for PackageOrigin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum TaggedOrigin {
            #[serde(rename = "github", alias = "GitHub")]
            GitHub {
                repo: String,
                #[serde(default)]
                tag: Option<String>,
                #[serde(default)]
                etag: Option<String>,
                #[serde(default)]
                asset_pattern: Option<String>,
            },
            #[serde(rename = "apt", alias = "Apt")]
            Apt { source_id: String, package: String },
            #[serde(rename = "appimagehub", alias = "AppImageHub", alias = "app_image_hub")]
            AppImageHub {
                source_id: String,
                app_id: String,
                #[serde(default)]
                github_repo: Option<String>,
                #[serde(default)]
                tag: Option<String>,
                #[serde(default)]
                etag: Option<String>,
                #[serde(default)]
                asset_pattern: Option<String>,
                #[serde(default)]
                download_url: Option<String>,
            },
            #[serde(rename = "local", alias = "Local")]
            Local {
                #[serde(default)]
                source_file: Option<String>,
            },
        }

        #[derive(Deserialize)]
        enum ExternalOrigin {
            #[serde(alias = "github")]
            GitHub {
                repo: String,
                #[serde(default)]
                tag: Option<String>,
                #[serde(default)]
                etag: Option<String>,
                #[serde(default)]
                asset_pattern: Option<String>,
            },
            #[serde(alias = "apt")]
            Apt { source_id: String, package: String },
            #[serde(alias = "appimagehub")]
            AppImageHub {
                source_id: String,
                app_id: String,
                #[serde(default)]
                github_repo: Option<String>,
                #[serde(default)]
                tag: Option<String>,
                #[serde(default)]
                etag: Option<String>,
                #[serde(default)]
                asset_pattern: Option<String>,
                #[serde(default)]
                download_url: Option<String>,
            },
            #[serde(alias = "local")]
            Local {
                #[serde(default)]
                source_file: Option<String>,
            },
        }

        let val = serde_json::Value::deserialize(deserializer)?;
        if let Ok(tagged) = serde_json::from_value::<TaggedOrigin>(val.clone()) {
            return Ok(match tagged {
                TaggedOrigin::GitHub {
                    repo,
                    tag,
                    etag,
                    asset_pattern,
                } => PackageOrigin::GitHub {
                    repo,
                    tag,
                    etag,
                    asset_pattern,
                },
                TaggedOrigin::Apt { source_id, package } => {
                    PackageOrigin::Apt { source_id, package }
                }
                TaggedOrigin::AppImageHub {
                    source_id,
                    app_id,
                    github_repo,
                    tag,
                    etag,
                    asset_pattern,
                    download_url,
                } => PackageOrigin::AppImageHub {
                    source_id,
                    app_id,
                    github_repo,
                    tag,
                    etag,
                    asset_pattern,
                    download_url,
                },
                TaggedOrigin::Local { source_file } => PackageOrigin::Local { source_file },
            });
        }

        if let Some(s) = val.as_str() {
            if s.eq_ignore_ascii_case("local") {
                return Ok(PackageOrigin::Local { source_file: None });
            }
        }

        let ext =
            serde_json::from_value::<ExternalOrigin>(val).map_err(serde::de::Error::custom)?;
        Ok(match ext {
            ExternalOrigin::GitHub {
                repo,
                tag,
                etag,
                asset_pattern,
            } => PackageOrigin::GitHub {
                repo,
                tag,
                etag,
                asset_pattern,
            },
            ExternalOrigin::Apt { source_id, package } => PackageOrigin::Apt { source_id, package },
            ExternalOrigin::AppImageHub {
                source_id,
                app_id,
                github_repo,
                tag,
                etag,
                asset_pattern,
                download_url,
            } => PackageOrigin::AppImageHub {
                source_id,
                app_id,
                github_repo,
                tag,
                etag,
                asset_pattern,
                download_url,
            },
            ExternalOrigin::Local { source_file } => PackageOrigin::Local { source_file },
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(from = "PackageRecordRaw")]
pub struct PackageRecord {
    pub id: String,
    pub name: String,
    pub version: String,
    pub format: String,
    #[serde(default = "default_user")]
    pub mode: String,
    #[serde(default)]
    pub install_date: String,
    pub install_dir: PathBuf,
    #[serde(default)]
    pub source_file: Option<String>,
    #[serde(default)]
    pub entrypoint: Option<PathBuf>,
    #[serde(default)]
    pub secondary_binaries: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub binaries: Vec<PathBuf>,
    #[serde(default)]
    pub desktop_files: Vec<PathBuf>,
    #[serde(default)]
    pub icons: Vec<PathBuf>,
    #[serde(default)]
    pub installed_files_count: Option<usize>,
    #[serde(default)]
    pub total_size_bytes: Option<u64>,
    #[serde(default)]
    pub is_gui: Option<bool>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub origin: PackageOrigin,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub asset_pattern: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub source_type: Option<String>,
    #[serde(default)]
    pub recipe_id: Option<String>,
    #[serde(default)]
    pub registry_name: Option<String>,
    #[serde(default)]
    pub default_env: Option<HashMap<String, String>>,
    #[serde(default)]
    pub default_args: Option<Vec<String>>,
    #[serde(default)]
    pub bwrap: Option<bool>,
    #[serde(default)]
    pub bind_opt: Option<Vec<String>>,
    #[serde(default)]
    pub bundled_deps: Vec<String>,
}

#[derive(Deserialize)]
struct PackageRecordRaw {
    id: String,
    name: String,
    version: String,
    format: String,
    #[serde(default = "default_user")]
    mode: String,
    #[serde(default)]
    install_date: String,
    install_dir: PathBuf,
    #[serde(default)]
    source_file: Option<String>,
    #[serde(default)]
    entrypoint: Option<PathBuf>,
    #[serde(default)]
    secondary_binaries: BTreeMap<String, PathBuf>,
    #[serde(default)]
    binaries: Vec<PathBuf>,
    #[serde(default)]
    desktop_files: Vec<PathBuf>,
    #[serde(default)]
    icons: Vec<PathBuf>,
    #[serde(default)]
    installed_files_count: Option<usize>,
    #[serde(default)]
    total_size_bytes: Option<u64>,
    #[serde(default)]
    is_gui: Option<bool>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    origin: Option<PackageOrigin>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    asset_pattern: Option<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    source_type: Option<String>,
    #[serde(default)]
    recipe_id: Option<String>,
    #[serde(default)]
    registry_name: Option<String>,
    #[serde(default)]
    default_env: Option<HashMap<String, String>>,
    #[serde(default)]
    default_args: Option<Vec<String>>,
    #[serde(default)]
    bwrap: Option<bool>,
    #[serde(default)]
    bind_opt: Option<Vec<String>>,
    #[serde(default)]
    bundled_deps: Vec<String>,
}

impl From<PackageRecordRaw> for PackageRecord {
    fn from(raw: PackageRecordRaw) -> Self {
        let inferred_origin = raw.origin.unwrap_or_else(|| {
            infer_legacy_origin(
                raw.source_type.as_deref(),
                raw.registry_name.as_deref(),
                raw.recipe_id.as_deref(),
                raw.repo.as_deref(),
                raw.tag.as_deref(),
                raw.etag.as_deref(),
                raw.asset_pattern.as_deref(),
                raw.source_file.as_deref(),
            )
        });

        let mut rec = Self {
            id: raw.id,
            name: raw.name,
            version: raw.version,
            format: raw.format,
            mode: raw.mode,
            install_date: raw.install_date,
            install_dir: raw.install_dir,
            source_file: raw.source_file,
            entrypoint: raw.entrypoint,
            secondary_binaries: raw.secondary_binaries,
            binaries: raw.binaries,
            desktop_files: raw.desktop_files,
            icons: raw.icons,
            installed_files_count: raw.installed_files_count,
            total_size_bytes: raw.total_size_bytes,
            is_gui: raw.is_gui,
            description: raw.description,
            origin: inferred_origin,
            repo: raw.repo,
            tag: raw.tag,
            etag: raw.etag,
            asset_pattern: raw.asset_pattern,
            env: raw.env,
            args: raw.args,
            pinned: raw.pinned,
            source_type: raw.source_type,
            recipe_id: raw.recipe_id,
            registry_name: raw.registry_name,
            default_env: raw.default_env,
            default_args: raw.default_args,
            bwrap: raw.bwrap,
            bind_opt: raw.bind_opt,
            bundled_deps: raw.bundled_deps,
        };
        rec.sync_legacy_from_origin_if_missing();
        rec
    }
}

#[allow(clippy::too_many_arguments)]
fn infer_legacy_origin(
    source_type: Option<&str>,
    registry_name: Option<&str>,
    recipe_id: Option<&str>,
    repo: Option<&str>,
    tag: Option<&str>,
    etag: Option<&str>,
    asset_pattern: Option<&str>,
    source_file: Option<&str>,
) -> PackageOrigin {
    if source_type == Some("apt") {
        if let (Some(src), Some(pkg)) = (registry_name, recipe_id) {
            return PackageOrigin::Apt {
                source_id: src.to_string(),
                package: pkg.to_string(),
            };
        }
    }
    if source_type == Some("appimagehub") {
        if let (Some(src), Some(app_id)) = (registry_name, recipe_id) {
            return PackageOrigin::AppImageHub {
                source_id: src.to_string(),
                app_id: app_id.to_string(),
                github_repo: repo.map(String::from),
                tag: tag.map(String::from),
                etag: etag.map(String::from),
                asset_pattern: asset_pattern.map(String::from),
                download_url: None,
            };
        }
    }
    if let Some(r) = repo.filter(|s| !s.is_empty()) {
        return PackageOrigin::GitHub {
            repo: r.to_string(),
            tag: tag.map(String::from),
            etag: etag.map(String::from),
            asset_pattern: asset_pattern.map(String::from),
        };
    }
    PackageOrigin::Local {
        source_file: source_file.map(String::from),
    }
}

impl PackageRecord {
    pub fn resolved_origin(&self) -> PackageOrigin {
        if !matches!(self.origin, PackageOrigin::Local { .. }) {
            return self.origin.clone();
        }
        infer_legacy_origin(
            self.source_type.as_deref(),
            self.registry_name.as_deref(),
            self.recipe_id.as_deref(),
            self.repo.as_deref(),
            self.tag.as_deref(),
            self.etag.as_deref(),
            self.asset_pattern.as_deref(),
            self.source_file.as_deref(),
        )
    }

    pub fn set_origin(&mut self, origin: PackageOrigin) {
        self.origin = origin;
        match &self.origin {
            PackageOrigin::GitHub {
                repo,
                tag,
                etag,
                asset_pattern,
            } => {
                self.repo = Some(repo.clone());
                self.tag = tag.clone();
                self.etag = etag.clone();
                self.asset_pattern = asset_pattern.clone();
                self.source_type = None;
                self.registry_name = None;
                self.recipe_id = None;
            }
            PackageOrigin::Apt { source_id, package } => {
                self.source_type = Some("apt".to_string());
                self.registry_name = Some(source_id.clone());
                self.recipe_id = Some(package.clone());
                self.repo = None;
            }
            PackageOrigin::AppImageHub {
                source_id,
                app_id,
                github_repo,
                tag,
                etag,
                asset_pattern,
                ..
            } => {
                self.source_type = Some("appimagehub".to_string());
                self.registry_name = Some(source_id.clone());
                self.recipe_id = Some(app_id.clone());
                self.repo = github_repo.clone();
                self.tag = tag.clone();
                self.etag = etag.clone();
                self.asset_pattern = asset_pattern.clone();
            }
            PackageOrigin::Local { source_file } => {
                if source_file.is_some() {
                    self.source_file = source_file.clone();
                }
                self.repo = None;
                self.source_type = None;
                self.registry_name = None;
                self.recipe_id = None;
            }
        }
    }

    fn sync_legacy_from_origin_if_missing(&mut self) {
        match &self.origin {
            PackageOrigin::GitHub {
                repo,
                tag,
                etag,
                asset_pattern,
            } => {
                if self.repo.is_none() {
                    self.repo = Some(repo.clone());
                }
                if self.tag.is_none() {
                    self.tag = tag.clone();
                }
                if self.etag.is_none() {
                    self.etag = etag.clone();
                }
                if self.asset_pattern.is_none() {
                    self.asset_pattern = asset_pattern.clone();
                }
            }
            PackageOrigin::Apt { source_id, package } => {
                if self.source_type.is_none() {
                    self.source_type = Some("apt".to_string());
                }
                if self.registry_name.is_none() {
                    self.registry_name = Some(source_id.clone());
                }
                if self.recipe_id.is_none() {
                    self.recipe_id = Some(package.clone());
                }
            }
            PackageOrigin::AppImageHub {
                source_id,
                app_id,
                github_repo,
                tag,
                etag,
                asset_pattern,
                ..
            } => {
                if self.source_type.is_none() {
                    self.source_type = Some("appimagehub".to_string());
                }
                if self.registry_name.is_none() {
                    self.registry_name = Some(source_id.clone());
                }
                if self.recipe_id.is_none() {
                    self.recipe_id = Some(app_id.clone());
                }
                if self.repo.is_none() {
                    self.repo = github_repo.clone();
                }
                if self.tag.is_none() {
                    self.tag = tag.clone();
                }
                if self.etag.is_none() {
                    self.etag = etag.clone();
                }
                if self.asset_pattern.is_none() {
                    self.asset_pattern = asset_pattern.clone();
                }
            }
            PackageOrigin::Local { source_file } => {
                if self.source_file.is_none() {
                    self.source_file = source_file.clone();
                }
            }
        }
    }
}

impl Default for PackageRecord {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            version: String::new(),
            format: String::new(),
            mode: default_user(),
            install_date: String::new(),
            install_dir: PathBuf::new(),
            source_file: None,
            entrypoint: None,
            secondary_binaries: BTreeMap::new(),
            binaries: Vec::new(),
            desktop_files: Vec::new(),
            icons: Vec::new(),
            installed_files_count: None,
            total_size_bytes: None,
            is_gui: None,
            description: None,
            origin: PackageOrigin::default(),
            repo: None,
            tag: None,
            etag: None,
            asset_pattern: None,
            env: HashMap::new(),
            args: Vec::new(),
            pinned: false,
            source_type: None,
            recipe_id: None,
            registry_name: None,
            default_env: None,
            default_args: None,
            bwrap: None,
            bind_opt: None,
            bundled_deps: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Database {
    pub path: PathBuf,
    pub data: HashMap<String, PackageRecord>,
}

fn append_path_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

fn atomic_write_bytes(target: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = append_path_suffix(target, &format!(".tmp.{}", std::process::id()));
    let write_res = (|| -> io::Result<()> {
        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp_path, target) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

impl Database {
    pub fn backup_path(path: &Path) -> PathBuf {
        append_path_suffix(path, ".bak")
    }

    pub fn corrupt_path(path: &Path) -> PathBuf {
        append_path_suffix(path, ".corrupt")
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self {
                path: path.to_path_buf(),
                data: HashMap::new(),
            });
        }

        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read database at '{}': {}", path.display(), e))?;

        match serde_json::from_str::<HashMap<String, PackageRecord>>(&content) {
            Ok(data) => Ok(Self {
                path: path.to_path_buf(),
                data,
            }),
            Err(e) => {
                let corrupt_path = Self::corrupt_path(path);
                let bak_path = Self::backup_path(path);
                if !corrupt_path.exists() {
                    let _ = fs::copy(path, &corrupt_path);
                }
                Err(format!(
                    "Database file '{}' is corrupted ({}). Corrupt snapshot preserved at '{}'. Restore from backup '{}' (e.g., cp '{}' '{}') before retrying.",
                    path.display(),
                    e,
                    corrupt_path.display(),
                    bak_path.display(),
                    bak_path.display(),
                    path.display()
                ))
            }
        }
    }

    pub fn save(&self) -> io::Result<()> {
        let content = serde_json::to_string_pretty(&self.data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let bytes = content.as_bytes();
        atomic_write_bytes(&self.path, bytes)?;
        let bak_path = Self::backup_path(&self.path);
        atomic_write_bytes(&bak_path, bytes)?;
        Ok(())
    }

    pub fn get(&self, query: &str) -> Option<&PackageRecord> {
        if let Some(rec) = self.data.get(query) {
            return Some(rec);
        }
        let lower = query.to_lowercase();
        for (k, v) in &self.data {
            if k.to_lowercase() == lower || v.name.to_lowercase() == lower {
                return Some(v);
            }
        }
        None
    }

    pub fn get_mut(&mut self, query: &str) -> Option<&mut PackageRecord> {
        if self.data.contains_key(query) {
            return self.data.get_mut(query);
        }
        let lower = query.to_lowercase();
        let mut found_key = None;
        for (k, v) in &self.data {
            if k.to_lowercase() == lower || v.name.to_lowercase() == lower {
                found_key = Some(k.clone());
                break;
            }
        }
        if let Some(key) = found_key {
            self.data.get_mut(&key)
        } else {
            None
        }
    }

    pub fn all(&self) -> Vec<&PackageRecord> {
        self.data.values().collect()
    }

    pub fn add(&mut self, mut record: PackageRecord) -> io::Result<()> {
        let resolved = record.resolved_origin();
        record.set_origin(resolved);
        self.data.insert(record.id.clone(), record);
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> io::Result<Option<PackageRecord>> {
        let removed = if let Some(rec) = self.get(id) {
            let key = rec.id.clone();
            self.data.remove(&key)
        } else {
            None
        };
        if removed.is_some() {
            self.save()?;
        }
        Ok(removed)
    }

    pub fn link_repo(
        &mut self,
        pkg_id: &str,
        repo: String,
        tag: Option<String>,
        etag: Option<String>,
        asset_pattern: Option<String>,
    ) -> io::Result<bool> {
        if let Some(rec) = self.get_mut(pkg_id) {
            let new_tag = tag.or_else(|| rec.tag.clone());
            let new_etag = etag.or_else(|| rec.etag.clone());
            let new_pat = asset_pattern.or_else(|| rec.asset_pattern.clone());
            rec.set_origin(PackageOrigin::GitHub {
                repo,
                tag: new_tag,
                etag: new_etag,
                asset_pattern: new_pat,
            });
            self.save()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn set_env(&mut self, pkg_id: &str, env_vars: HashMap<String, String>) -> io::Result<bool> {
        if let Some(rec) = self.get_mut(pkg_id) {
            for (k, v) in env_vars {
                rec.env.insert(k, v);
            }
            self.save()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn unset_env(&mut self, pkg_id: &str, key: &str) -> io::Result<bool> {
        if let Some(rec) = self.get_mut(pkg_id) {
            rec.env.remove(key);
            self.save()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn set_args(&mut self, pkg_id: &str, args_list: Vec<String>) -> io::Result<bool> {
        if let Some(rec) = self.get_mut(pkg_id) {
            rec.args = args_list;
            self.save()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn clear_args(&mut self, pkg_id: &str) -> io::Result<bool> {
        if let Some(rec) = self.get_mut(pkg_id) {
            rec.args.clear();
            self.save()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn set_pinned(&mut self, pkg_id: &str, pinned: bool) -> io::Result<bool> {
        if let Some(rec) = self.get_mut(pkg_id) {
            rec.pinned = pinned;
            self.save()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_v140_backward_compatibility() {
        let sample_json = r#"{
          "btop": {
            "id": "btop",
            "name": "btop",
            "version": "1.4.8",
            "format": "tar",
            "mode": "user",
            "install_date": "2026-11-22T11:06:04+01:00",
            "install_dir": "/home/alexzahn/.local/share/appmgr/apps/btop",
            "source_file": "/tmp/btop.tar.gz",
            "binaries": ["/home/alexzahn/.local/bin/btop"],
            "desktop_files": [],
            "icons": [],
            "installed_files_count": 56,
            "total_size_bytes": 2558640,
            "is_gui": false,
            "description": "Binary archive application: btop",
            "repo": "aristocratos/btop",
            "tag": "v1.4.8",
            "etag": "W/\"37e416c010b9d0dafbd614b374d75f1390b55a3043e54a521ce040bacff2d912\"",
            "asset_pattern": "^btop\\-x86_64\\-unknown\\-linux\\-musl\\.tar\\.gz$",
            "env": {
              "FOO": "bar"
            },
            "args": ["--utf-force", "--low-color"],
            "pinned": false
          }
        }"#;

        let parsed: HashMap<String, PackageRecord> = serde_json::from_str(sample_json).unwrap();
        let btop = parsed.get("btop").unwrap();
        assert_eq!(btop.id, "btop");
        assert_eq!(btop.name, "btop");
        assert_eq!(btop.version, "1.4.8");
        assert_eq!(btop.format, "tar");
        assert_eq!(btop.repo, Some("aristocratos/btop".to_string()));
        assert_eq!(
            btop.origin,
            PackageOrigin::GitHub {
                repo: "aristocratos/btop".to_string(),
                tag: Some("v1.4.8".to_string()),
                etag: Some(
                    "W/\"37e416c010b9d0dafbd614b374d75f1390b55a3043e54a521ce040bacff2d912\""
                        .to_string()
                ),
                asset_pattern: Some(
                    "^btop\\-x86_64\\-unknown\\-linux\\-musl\\.tar\\.gz$".to_string()
                ),
            }
        );
        assert_eq!(btop.env.get("FOO").unwrap(), "bar");
        assert_eq!(btop.args, vec!["--utf-force", "--low-color"]);
        assert!(!btop.pinned);
        assert_eq!(btop.source_type, None);
        assert_eq!(btop.recipe_id, None);
        assert_eq!(btop.registry_name, None);
        assert_eq!(btop.default_env, None);
        assert_eq!(btop.default_args, None);
        assert_eq!(btop.bwrap, None);
        assert_eq!(btop.bind_opt, None);
        assert_eq!(btop.bundled_deps, Vec::<String>::new());
    }

    #[test]
    fn test_package_origin_backward_compatibility_all_variants() {
        let legacy_db_json = r#"{
          "gh-cli": {
            "id": "gh",
            "name": "gh",
            "version": "2.40.0",
            "format": "deb",
            "install_dir": "/tmp/apps/gh",
            "source_type": "apt",
            "registry_name": "apt-gh",
            "recipe_id": "gh"
          },
          "obsidian": {
            "id": "obsidian",
            "name": "obsidian",
            "version": "1.5.0",
            "format": "appimage",
            "install_dir": "/tmp/apps/obsidian",
            "source_type": "appimagehub",
            "registry_name": "hub",
            "recipe_id": "obsidian",
            "repo": "obsidianmd/obsidian-releases",
            "tag": "v1.5.0"
          },
          "localtool": {
            "id": "localtool",
            "name": "localtool",
            "version": "1.0.0",
            "format": "bin",
            "install_dir": "/tmp/apps/localtool",
            "source_file": "/tmp/localtool"
          }
        }"#;

        let parsed: HashMap<String, PackageRecord> = serde_json::from_str(legacy_db_json).unwrap();
        assert_eq!(
            parsed["gh-cli"].origin,
            PackageOrigin::Apt {
                source_id: "apt-gh".to_string(),
                package: "gh".to_string(),
            }
        );
        assert_eq!(
            parsed["obsidian"].origin,
            PackageOrigin::AppImageHub {
                source_id: "hub".to_string(),
                app_id: "obsidian".to_string(),
                github_repo: Some("obsidianmd/obsidian-releases".to_string()),
                tag: Some("v1.5.0".to_string()),
                etag: None,
                asset_pattern: None,
                download_url: None,
            }
        );
        assert_eq!(
            parsed["localtool"].origin,
            PackageOrigin::Local {
                source_file: Some("/tmp/localtool".to_string()),
            }
        );

        // Round-trip new tagged format and legacy external format
        let roundtrip_json = serde_json::to_string(&parsed).unwrap();
        let reparsed: HashMap<String, PackageRecord> =
            serde_json::from_str(&roundtrip_json).unwrap();
        assert_eq!(reparsed["gh-cli"].origin, parsed["gh-cli"].origin);
        assert_eq!(reparsed["obsidian"].origin, parsed["obsidian"].origin);
        assert_eq!(reparsed["localtool"].origin, parsed["localtool"].origin);

        let ext_origin: PackageOrigin =
            serde_json::from_str(r#"{"Apt":{"source_id":"deb","package":"fd"}}"#).unwrap();
        assert_eq!(
            ext_origin,
            PackageOrigin::Apt {
                source_id: "deb".to_string(),
                package: "fd".to_string()
            }
        );
    }

    #[test]
    fn test_corrupt_db_protection_and_atomic_backup() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("installed.json");

        let mut db = Database::load(&db_path).unwrap();
        assert!(db.all().is_empty());

        let rec = PackageRecord {
            id: "demo".to_string(),
            name: "demo".to_string(),
            version: "1.0.0".to_string(),
            format: "bin".to_string(),
            install_dir: dir.path().join("apps/demo"),
            ..Default::default()
        };
        db.add(rec).unwrap();

        let bak_path = Database::backup_path(&db_path);
        assert!(bak_path.exists());
        let loaded_ok = Database::load(&db_path).unwrap();
        assert!(loaded_ok.get("demo").is_some());

        let corrupt_payload = "{ not valid json";
        fs::write(&db_path, corrupt_payload).unwrap();

        let err = Database::load(&db_path).unwrap_err();
        assert!(err.contains("installed.json.bak"));

        let corrupt_path = Database::corrupt_path(&db_path);
        assert!(corrupt_path.exists());
        assert_eq!(fs::read_to_string(&corrupt_path).unwrap(), corrupt_payload);
        assert_eq!(fs::read_to_string(&db_path).unwrap(), corrupt_payload);

        fs::write(&db_path, "{ newer broken").unwrap();
        let _ = Database::load(&db_path).unwrap_err();
        assert_eq!(fs::read_to_string(&corrupt_path).unwrap(), corrupt_payload);
    }
}
