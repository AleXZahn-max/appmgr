use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn default_user() -> String {
    "user".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
            binaries: Vec::new(),
            desktop_files: Vec::new(),
            icons: Vec::new(),
            installed_files_count: None,
            total_size_bytes: None,
            is_gui: None,
            description: None,
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

impl Database {
    pub fn load(path: &Path) -> Self {
        if path.exists() {
            if let Ok(content) = fs::read_to_string(path) {
                if let Ok(data) = serde_json::from_str::<HashMap<String, PackageRecord>>(&content) {
                    return Self {
                        path: path.to_path_buf(),
                        data,
                    };
                }
            }
        }
        Self {
            path: path.to_path_buf(),
            data: HashMap::new(),
        }
    }

    pub fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = self.path.with_extension("tmp");
        let content = serde_json::to_string_pretty(&self.data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        {
            let mut file = fs::File::create(&tmp_path)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
        }
        if fs::rename(&tmp_path, &self.path).is_err() {
            fs::copy(&tmp_path, &self.path)?;
            let _ = fs::remove_file(&tmp_path);
        }
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

    pub fn add(&mut self, record: PackageRecord) -> io::Result<()> {
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
            rec.repo = Some(repo);
            if let Some(t) = tag {
                rec.tag = Some(t);
            }
            if let Some(e) = etag {
                rec.etag = Some(e);
            }
            if let Some(p) = asset_pattern {
                rec.asset_pattern = Some(p);
            }
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
}
