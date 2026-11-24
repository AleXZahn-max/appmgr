use crate::config::Config;
use crate::db::PackageRecord;
use crate::desktop::{create_desktop_entry, is_gui_binary};
use goblin::elf::Elf;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub struct BinaryPackage {
    pub file_path: PathBuf,
    pub config: Config,
    pub repo: Option<String>,
    pub tag: Option<String>,
    pub clean_name: Option<String>,
}

impl BinaryPackage {
    pub fn new(
        file_path: &Path,
        config: &Config,
        repo: Option<String>,
        tag: Option<String>,
        clean_name: Option<String>,
    ) -> Self {
        Self {
            file_path: file_path.to_path_buf(),
            config: config.clone(),
            repo,
            tag,
            clean_name,
        }
    }

    pub fn sanitize_name(name: &str) -> String {
        let clean: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        let lower = clean.to_lowercase();
        if lower.is_empty() {
            "app".to_string()
        } else {
            lower
        }
    }

    pub fn read_metadata(&self) -> Result<(String, String, String, u64), String> {
        let mut buffer = Vec::new();
        let mut file = File::open(&self.file_path)
            .map_err(|e| format!("Failed to open binary {}: {}", self.file_path.display(), e))?;
        file.read_to_end(&mut buffer)
            .map_err(|e| format!("Failed to read binary: {}", e))?;

        if buffer.len() < 4 || &buffer[..4] != b"\x7fELF" {
            return Err("File is not a valid ELF binary".to_string());
        }

        let elf = Elf::parse(&buffer).map_err(|e| format!("Corrupt ELF header: {}", e))?;
        let is_executable = elf.header.e_type == goblin::elf::header::ET_EXEC
            || (elf.header.e_type == goblin::elf::header::ET_DYN && elf.entry != 0);

        if !is_executable {
            return Err("ELF file is not an executable binary".to_string());
        }

        let stem = self
            .file_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "binary".to_string());

        let raw_name = stem.trim_end_matches(".bin");
        let pkg_id = self
            .clean_name
            .as_deref()
            .map(Self::sanitize_name)
            .unwrap_or_else(|| Self::sanitize_name(raw_name));
        let version = self.tag.clone().unwrap_or_else(|| "1.0.0".to_string());
        let desc = format!("Standalone binary executable: {}", pkg_id);
        let size = buffer.len() as u64;

        Ok((pkg_id, version, desc, size))
    }

    pub fn install(
        &self,
        force: bool,
        env: Option<HashMap<String, String>>,
        args: Option<Vec<String>>,
    ) -> Result<PackageRecord, String> {
        let (pkg_id, version, desc, size) = self.read_metadata()?;
        let app_dir = self.config.apps_dir.join(&pkg_id);

        if app_dir.exists() {
            if force {
                let _ = fs::remove_dir_all(&app_dir);
            } else {
                return Err(format!(
                    "Package directory already exists: {}. Use -f to overwrite.",
                    app_dir.display()
                ));
            }
        }

        let target_bin_dir = app_dir.join("bin");
        fs::create_dir_all(&target_bin_dir)
            .map_err(|e| format!("Failed to create app bin dir: {}", e))?;

        let deployed_bin = target_bin_dir.join(&pkg_id);
        fs::copy(&self.file_path, &deployed_bin)
            .map_err(|e| format!("Failed to copy binary: {}", e))?;
        fs::set_permissions(&deployed_bin, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to set permissions: {}", e))?;

        let launcher_path = self.config.bin_dir.join(&pkg_id);
        let env_map = env.unwrap_or_default();
        let args_vec = args.unwrap_or_default();

        let env_block = crate::sandbox::format_env_exports(&env_map)?;
        let args_block = format!("\n{}", crate::sandbox::format_persistent_args(&args_vec));

        let script = format!(
            r#"#!/usr/bin/env bash
# Wrapper created by appmgr for {pkg_id} (Standalone binary)
APP_ROOT={app_dir}
export PATH="$APP_ROOT/bin:$PATH"
export LD_LIBRARY_PATH="$APP_ROOT/deps/lib:$APP_ROOT/lib:${{LD_LIBRARY_PATH}}"{env_block}
{args_block}
exec "$APP_ROOT/bin/"{quoted_pkg_id} "${{PERSISTENT_ARGS[@]}}" "$@"
"#,
            pkg_id = pkg_id,
            quoted_pkg_id = crate::sandbox::posix_quote(&pkg_id),
            app_dir = crate::sandbox::posix_quote(&app_dir.to_string_lossy()),
            env_block = env_block,
            args_block = args_block,
        );

        if let Some(parent) = launcher_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(&launcher_path, script)
            .map_err(|e| format!("Failed to write launcher: {}", e))?;
        fs::set_permissions(&launcher_path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to chmod launcher: {}", e))?;

        let is_gui = is_gui_binary(&deployed_bin);
        let mut desktop_files = Vec::new();
        if is_gui {
            let dt_path = self
                .config
                .applications_dir
                .join(format!("appmgr-{}.desktop", pkg_id));
            if create_desktop_entry(
                &dt_path,
                &pkg_id,
                &desc,
                &launcher_path,
                None,
                "Utility;Application;",
            )
            .is_ok()
            {
                desktop_files.push(dt_path);
            }
        }

        let now = chrono::Local::now().to_rfc3339();
        Ok(PackageRecord {
            id: pkg_id.clone(),
            name: pkg_id,
            version,
            format: "bin".to_string(),
            mode: self.config.mode.clone(),
            install_date: now,
            install_dir: app_dir,
            source_file: Some(self.file_path.to_string_lossy().to_string()),
            binaries: vec![launcher_path],
            desktop_files,
            icons: Vec::new(),
            installed_files_count: Some(1),
            total_size_bytes: Some(size),
            is_gui: Some(is_gui),
            description: Some(desc),
            repo: self.repo.clone(),
            tag: self.tag.clone(),
            etag: None,
            asset_pattern: None,
            env: env_map,
            args: args_vec,
            pinned: false,
            ..Default::default()
        })
    }
}
