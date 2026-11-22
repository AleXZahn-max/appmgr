use crate::config::Config;
use crate::db::PackageRecord;
use crate::desktop::create_desktop_entry;
use crate::sandbox::generate_deb_smart_launcher;
use regex::Regex;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

pub struct DebPackage {
    pub file_path: PathBuf,
    pub config: Config,
    pub repo: Option<String>,
    pub tag: Option<String>,
}

impl DebPackage {
    pub fn new(
        file_path: &Path,
        config: &Config,
        repo: Option<String>,
        tag: Option<String>,
    ) -> Self {
        Self {
            file_path: file_path.to_path_buf(),
            config: config.clone(),
            repo,
            tag,
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

    pub fn read_metadata(&self) -> Result<(String, String, String, u64, String), String> {
        let file_size = fs::metadata(&self.file_path)
            .map_err(|e| format!("Failed to read deb metadata: {}", e))?
            .len();

        let file = File::open(&self.file_path)
            .map_err(|e| format!("Failed to open deb {}: {}", self.file_path.display(), e))?;
        let mut archive =
            ar::Archive::new(file);

        let mut control_tar_data = Vec::new();
        let mut control_tar_name = String::new();

        while let Some(entry_res) = archive.next_entry() {
            let mut entry = entry_res.map_err(|e| format!("Ar entry error: {}", e))?;
            let name = String::from_utf8_lossy(entry.header().identifier()).to_string();
            if name.starts_with("control.tar") {
                control_tar_name = name;
                entry
                    .read_to_end(&mut control_tar_data)
                    .map_err(|e| format!("Failed to read control.tar: {}", e))?;
                break;
            }
        }

        if control_tar_data.is_empty() {
            return Err("Invalid deb package: control.tar not found".to_string());
        }

        let control_content = Self::extract_control_content(&control_tar_name, &control_tar_data)?;

        let mut pkg_name = String::new();
        let mut version = "1.0.0".to_string();
        let mut desc = String::new();
        let mut arch = "all".to_string();

        for line in control_content.lines() {
            if let Some(rest) = line.strip_prefix("Package:") {
                pkg_name = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("Version:") {
                version = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("Architecture:") {
                arch = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("Description:") {
                desc = rest.trim().to_string();
            }
        }

        if pkg_name.is_empty() {
            pkg_name = self
                .file_path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "debapp".to_string());
        }

        let pkg_id = Self::sanitize_name(&pkg_name);
        if desc.is_empty() {
            desc = format!("Debian package application: {}", pkg_name);
        }

        Ok((pkg_id, version, desc, file_size, arch))
    }

    fn extract_control_content(name: &str, data: &[u8]) -> Result<String, String> {
        let cursor = std::io::Cursor::new(data);
        if name.ends_with(".xz") {
            let decoder = xz2::read::XzDecoder::new(cursor);
            Self::find_control_in_tar(decoder)
        } else if name.ends_with(".gz") {
            let decoder = flate2::read::GzDecoder::new(cursor);
            Self::find_control_in_tar(decoder)
        } else if name.ends_with(".zst") {
            let decoder = zstd::Decoder::new(cursor)
                .map_err(|e| format!("zstd init error: {}", e))?;
            Self::find_control_in_tar(decoder)
        } else {
            Self::find_control_in_tar(cursor)
        }
    }

    fn find_control_in_tar<R: Read>(reader: R) -> Result<String, String> {
        let mut tar_archive = tar::Archive::new(reader);
        for entry_res in tar_archive
            .entries()
            .map_err(|e| format!("Tar error: {}", e))?
        {
            let mut entry = entry_res.map_err(|e| format!("Tar entry error: {}", e))?;
            let path = entry.path().map_err(|e| format!("Tar path error: {}", e))?;
            let path_str = path.to_string_lossy();
            if path_str == "control" || path_str.ends_with("/control") {
                let mut content = String::new();
                entry
                    .read_to_string(&mut content)
                    .map_err(|e| format!("Failed to read control: {}", e))?;
                return Ok(content);
            }
        }
        Err("control file not found in control.tar".to_string())
    }

    pub fn install(
        &self,
        force: bool,
        env: Option<HashMap<String, String>>,
        args: Option<Vec<String>>,
    ) -> Result<PackageRecord, String> {
        let (pkg_id, version, desc, file_size, _) = self.read_metadata()?;
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
        fs::create_dir_all(&app_dir)
            .map_err(|e| format!("Failed to create destination dir: {}", e))?;

        // Extract data.tar.*
        let file = File::open(&self.file_path)
            .map_err(|e| format!("Failed to open deb: {}", e))?;
        let mut archive = ar::Archive::new(file);

        let mut data_tar_name = String::new();
        let mut data_tar_data = Vec::new();

        while let Some(entry_res) = archive.next_entry() {
            let mut entry = entry_res.map_err(|e| format!("Ar error: {}", e))?;
            let name = String::from_utf8_lossy(entry.header().identifier()).to_string();
            if name.starts_with("data.tar") {
                data_tar_name = name;
                entry
                    .read_to_end(&mut data_tar_data)
                    .map_err(|e| format!("Failed to read data.tar: {}", e))?;
                break;
            }
        }

        if data_tar_data.is_empty() {
            return Err("Invalid deb package: data.tar not found".to_string());
        }

        let cursor = std::io::Cursor::new(data_tar_data);
        if data_tar_name.ends_with(".xz") {
            let decoder = xz2::read::XzDecoder::new(cursor);
            crate::formats::archive::ArchivePackage::extract_tar_safe(decoder, &app_dir)?;
        } else if data_tar_name.ends_with(".gz") {
            let decoder = flate2::read::GzDecoder::new(cursor);
            crate::formats::archive::ArchivePackage::extract_tar_safe(decoder, &app_dir)?;
        } else if data_tar_name.ends_with(".zst") {
            let decoder = zstd::Decoder::new(cursor)
                .map_err(|e| format!("zstd error: {}", e))?;
            crate::formats::archive::ArchivePackage::extract_tar_safe(decoder, &app_dir)?;
        } else {
            crate::formats::archive::ArchivePackage::extract_tar_safe(cursor, &app_dir)?;
        }

        // Find primary executable
        let primary_exe = Self::discover_primary_exe(&app_dir, &pkg_id)?;

        let launcher_path = self.config.bin_dir.join(&pkg_id);
        let env_map = env.unwrap_or_default();
        let args_vec = args.unwrap_or_default();

        // Generate bwrap launcher
        generate_deb_smart_launcher(
            &app_dir,
            &primary_exe,
            &launcher_path,
            &pkg_id,
            &env_map,
            &args_vec,
        )
        .map_err(|e| format!("Failed to generate launcher: {}", e))?;

        // Setup desktop entries
        let desktop_files = self.setup_desktop_files(&app_dir, &pkg_id, &launcher_path, &desc);
        let icons = self.setup_icons(&app_dir, &pkg_id);

        let now = chrono::Local::now().to_rfc3339();
        Ok(PackageRecord {
            id: pkg_id.clone(),
            name: pkg_id,
            version,
            format: "deb".to_string(),
            mode: self.config.mode.clone(),
            install_date: now,
            install_dir: app_dir,
            source_file: Some(self.file_path.to_string_lossy().to_string()),
            binaries: vec![launcher_path],
            desktop_files,
            icons,
            installed_files_count: None,
            total_size_bytes: Some(file_size),
            is_gui: Some(true),
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

    fn discover_primary_exe(app_dir: &Path, pkg_id: &str) -> Result<PathBuf, String> {
        let mut executables = Vec::new();
        let search_dirs = [
            app_dir.join("usr/bin"),
            app_dir.join("bin"),
            app_dir.join("opt"),
        ];

        for s_dir in &search_dirs {
            if s_dir.exists() {
                for entry in walkdir::WalkDir::new(s_dir).into_iter().flatten() {
                    let p = entry.path();
                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                    if p.is_file() && !name.ends_with(".so") && !name.contains(".so.")
                        && crate::formats::archive::ArchivePackage::is_executable_file(p) {
                            executables.push(p.to_path_buf());
                        }
                }
            }
        }

        if executables.is_empty() {
            // Fallback scan all
            for entry in walkdir::WalkDir::new(app_dir).into_iter().flatten() {
                let p = entry.path();
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                if p.is_file() && !name.ends_with(".so") && !name.contains(".so.")
                    && crate::formats::archive::ArchivePackage::is_executable_file(p) {
                        executables.push(p.to_path_buf());
                    }
            }
        }

        if executables.is_empty() {
            return Err("No executable binary found in deb package".to_string());
        }

        for exe in &executables {
            let name = exe
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            if name == pkg_id.to_lowercase() {
                return Ok(exe.clone());
            }
        }

        Ok(executables[0].clone())
    }

    fn setup_desktop_files(
        &self,
        app_dir: &Path,
        pkg_id: &str,
        launcher_path: &Path,
        desc: &str,
    ) -> Vec<PathBuf> {
        let mut created = Vec::new();
        let mut found_dt = None;

        for entry in walkdir::WalkDir::new(app_dir).into_iter().flatten() {
            let p = entry.path();
            if p.is_file() && p.extension().map(|e| e == "desktop").unwrap_or(false) {
                found_dt = Some(p.to_path_buf());
                break;
            }
        }

        let dest_dt = self
            .config
            .applications_dir
            .join(format!("appmgr-{}.desktop", pkg_id));
        if let Some(parent) = dest_dt.parent() {
            let _ = fs::create_dir_all(parent);
        }

        if let Some(orig_dt) = found_dt {
            if let Ok(content) = fs::read_to_string(&orig_dt) {
                let re = Regex::new(r"(%[a-zA-Z])").unwrap();
                let mut new_lines = Vec::new();
                for line in content.lines() {
                    if line.starts_with("Exec=") {
                        let extra = if let Some(caps) = re.captures(line) {
                            format!(" {}", caps.get(1).unwrap().as_str())
                        } else {
                            " %U".to_string()
                        };
                        new_lines.push(format!("Exec=\"{}\"{}", launcher_path.display(), extra));
                    } else {
                        new_lines.push(line.to_string());
                    }
                }
                if fs::write(&dest_dt, new_lines.join("\n") + "\n").is_ok() {
                    created.push(dest_dt);
                    return created;
                }
            }
        }

        // Fallback desktop creation
        if create_desktop_entry(
            &dest_dt,
            &pkg_id.replace('-', " "),
            desc,
            launcher_path,
            None,
            "Utility;Application;",
        )
        .is_ok()
        {
            created.push(dest_dt);
        }
        created
    }

    fn setup_icons(&self, app_dir: &Path, pkg_id: &str) -> Vec<PathBuf> {
        let mut installed = Vec::new();
        let icon_extensions = [".png", ".svg"];

        let icon_dirs = [
            app_dir.join("usr/share/icons"),
            app_dir.join("usr/share/pixmaps"),
        ];

        for i_dir in &icon_dirs {
            if i_dir.exists() {
                for entry in walkdir::WalkDir::new(i_dir).into_iter().flatten() {
                    let p = entry.path();
                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                    if icon_extensions.iter().any(|ext| name.ends_with(ext)) {
                        let ext = p.extension().unwrap_or_default().to_string_lossy();
                        let target_dir = self.config.icons_dir.join("hicolor/256x256/apps");
                        let _ = fs::create_dir_all(&target_dir);
                        let target = target_dir.join(format!("{}.{}", pkg_id, ext));
                        if fs::copy(p, &target).is_ok() {
                            installed.push(target);
                            return installed;
                        }
                    }
                }
            }
        }
        installed
    }
}
