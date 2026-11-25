use crate::config::Config;
use crate::db::PackageRecord;
use crate::desktop::{create_desktop_entry, inspect_elf_headers, is_gui_binary};
use crate::formats::StagingDirGuard;
use crate::launcher::write_package_launchers;
use std::collections::HashMap;
use std::fs;
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
        let size = fs::metadata(&self.file_path)
            .map_err(|e| format!("Failed to open binary {}: {}", self.file_path.display(), e))?
            .len();

        let elf_hdr = inspect_elf_headers(&self.file_path)
            .ok_or_else(|| "File is not a valid ELF binary".to_string())?;

        let is_executable = elf_hdr.e_type == goblin::elf::header::ET_EXEC
            || (elf_hdr.e_type == goblin::elf::header::ET_DYN && elf_hdr.entry != 0);

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
        let version = self
            .tag
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(crate::formats::normalize_version)
            .unwrap_or_else(|| "1.0.0".to_string());
        let desc = format!("Standalone binary executable: {}", pkg_id);

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

        if app_dir.exists() && !force {
            return Err(format!(
                "Package directory already exists: {}. Use -f to overwrite.",
                app_dir.display()
            ));
        }

        let mut staging = StagingDirGuard::new(&self.config.apps_dir, &pkg_id)?;
        let stage_path = staging.path.clone();
        let target_bin_dir = stage_path.join("bin");
        fs::create_dir_all(&target_bin_dir)
            .map_err(|e| format!("Failed to create app bin dir: {}", e))?;

        let deployed_bin = target_bin_dir.join(&pkg_id);
        fs::copy(&self.file_path, &deployed_bin)
            .map_err(|e| format!("Failed to copy binary: {}", e))?;
        fs::set_permissions(&deployed_bin, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to set permissions: {}", e))?;

        let is_gui = is_gui_binary(&deployed_bin);

        staging.commit_swap(&stage_path, &app_dir, force)?;

        let launcher_path = self.config.bin_dir.join(&pkg_id);
        let env_map = env.unwrap_or_default();
        let args_vec = args.unwrap_or_default();
        let now = chrono::Local::now().to_rfc3339();

        let mut record = PackageRecord {
            id: pkg_id.clone(),
            name: pkg_id.clone(),
            version,
            format: "bin".to_string(),
            mode: self.config.mode.clone(),
            install_date: now,
            install_dir: app_dir,
            source_file: Some(self.file_path.to_string_lossy().to_string()),
            entrypoint: Some(PathBuf::from("bin").join(&pkg_id)),
            binaries: vec![launcher_path.clone()],
            desktop_files: Vec::new(),
            icons: Vec::new(),
            installed_files_count: Some(1),
            total_size_bytes: Some(size),
            is_gui: Some(is_gui),
            description: Some(desc.clone()),
            repo: self.repo.clone(),
            tag: self.tag.clone(),
            etag: None,
            asset_pattern: None,
            env: env_map,
            args: args_vec,
            pinned: false,
            ..Default::default()
        };

        write_package_launchers(&record, &self.config.bin_dir)?;

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
                record.desktop_files.push(dt_path);
            }
        }

        Ok(record)
    }
}
