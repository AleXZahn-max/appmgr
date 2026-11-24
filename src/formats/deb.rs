use crate::config::Config;
use crate::db::PackageRecord;
use crate::desktop::{
    create_desktop_entry, install_best_icon, install_patched_desktop_file, is_gui_binary,
};
use crate::formats::{connect_package_binaries, is_deb_arch_compatible, StagingDirGuard};
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
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
        let mut archive = ar::Archive::new(file);

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
            let decoder =
                zstd::Decoder::new(cursor).map_err(|e| format!("zstd init error: {}", e))?;
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

    #[allow(dead_code)]
    pub fn install(
        &self,
        force: bool,
        env: Option<HashMap<String, String>>,
        args: Option<Vec<String>>,
    ) -> Result<PackageRecord, String> {
        self.install_with_owned(force, env, args, &[])
    }

    pub fn install_with_owned(
        &self,
        force: bool,
        env: Option<HashMap<String, String>>,
        args: Option<Vec<String>>,
        owned_launchers: &[PathBuf],
    ) -> Result<PackageRecord, String> {
        let (pkg_id, version, desc, file_size, arch) = self.read_metadata()?;

        if !is_deb_arch_compatible(&arch, std::env::consts::ARCH) {
            return Err(format!(
                "Incompatible .deb architecture '{}' for host '{}'",
                arch,
                std::env::consts::ARCH
            ));
        }

        let app_dir = self.config.apps_dir.join(&pkg_id);
        if app_dir.exists() && !force {
            return Err(format!(
                "Package directory already exists: {}. Use -f to overwrite.",
                app_dir.display()
            ));
        }

        // Read data.tar.* before touching apps_dir
        let file = File::open(&self.file_path).map_err(|e| format!("Failed to open deb: {}", e))?;
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

        // Stage extraction on the same filesystem
        let mut staging = StagingDirGuard::new(&self.config.apps_dir, &pkg_id)?;
        let stage_path = staging.path.clone();

        let cursor = std::io::Cursor::new(data_tar_data);
        if data_tar_name.ends_with(".xz") {
            let decoder = xz2::read::XzDecoder::new(cursor);
            crate::formats::archive::ArchivePackage::extract_tar_safe(decoder, &stage_path)?;
        } else if data_tar_name.ends_with(".gz") {
            let decoder = flate2::read::GzDecoder::new(cursor);
            crate::formats::archive::ArchivePackage::extract_tar_safe(decoder, &stage_path)?;
        } else if data_tar_name.ends_with(".zst") {
            let decoder = zstd::Decoder::new(cursor).map_err(|e| format!("zstd error: {}", e))?;
            crate::formats::archive::ArchivePackage::extract_tar_safe(decoder, &stage_path)?;
        } else {
            crate::formats::archive::ArchivePackage::extract_tar_safe(cursor, &stage_path)?;
        }

        // Validate executables and metadata BEFORE swapping
        let (primary_exe, secondary_exes) = Self::discover_executables(&stage_path, &pkg_id)?;
        let rel_primary = primary_exe
            .strip_prefix(&stage_path)
            .map_err(|e| format!("Path error: {}", e))?
            .to_path_buf();

        let mut secondary_rels = Vec::new();
        for sec in &secondary_exes {
            if let Ok(rel) = sec.strip_prefix(&stage_path) {
                secondary_rels.push(rel.to_path_buf());
            }
        }

        let has_opt = stage_path.join("opt").exists();
        let bind_opt = if has_opt {
            let mut opts = Vec::new();
            if let Ok(entries) = fs::read_dir(stage_path.join("opt")) {
                for entry in entries.flatten() {
                    opts.push(format!("opt/{}", entry.file_name().to_string_lossy()));
                }
            }
            opts.sort();
            if opts.is_empty() {
                None
            } else {
                Some(opts)
            }
        } else {
            None
        };

        let orig_desktop_rel = Self::find_desktop_file(&stage_path)
            .and_then(|p| p.strip_prefix(&stage_path).ok().map(|r| r.to_path_buf()));
        let binary_is_gui = is_gui_binary(&primary_exe);
        let is_gui = orig_desktop_rel.is_some() || binary_is_gui;

        // Atomic swap into final app_dir
        staging.commit_swap(&stage_path, &app_dir, force)?;

        let launcher_path = self.config.bin_dir.join(&pkg_id);
        let env_map = env.unwrap_or_default();
        let args_vec = args.unwrap_or_default();
        let now = chrono::Local::now().to_rfc3339();

        let mut record = PackageRecord {
            id: pkg_id.clone(),
            name: pkg_id.clone(),
            version,
            format: "deb".to_string(),
            mode: self.config.mode.clone(),
            install_date: now,
            install_dir: app_dir.clone(),
            source_file: Some(self.file_path.to_string_lossy().to_string()),
            entrypoint: Some(rel_primary),
            binaries: vec![launcher_path.clone()],
            desktop_files: Vec::new(),
            icons: Vec::new(),
            installed_files_count: None,
            total_size_bytes: Some(file_size),
            is_gui: Some(is_gui),
            description: Some(desc.clone()),
            repo: self.repo.clone(),
            tag: self.tag.clone(),
            etag: None,
            asset_pattern: None,
            env: env_map,
            args: args_vec,
            pinned: false,
            bwrap: if has_opt { Some(true) } else { None },
            bind_opt,
            ..Default::default()
        };

        connect_package_binaries(
            &mut record,
            &self.config.bin_dir,
            &secondary_rels,
            owned_launchers,
        )?;

        let icons = self.setup_icons(&app_dir, &pkg_id);
        let desktop_files = self.setup_desktop_files(
            &app_dir,
            &pkg_id,
            &launcher_path,
            &desc,
            orig_desktop_rel.as_deref(),
            binary_is_gui,
            !icons.is_empty(),
        );
        record.icons = icons;
        record.desktop_files = desktop_files;

        Ok(record)
    }

    pub fn discover_primary_exe(app_dir: &Path, pkg_id: &str) -> Result<PathBuf, String> {
        let (primary, _) = Self::discover_executables(app_dir, pkg_id)?;
        Ok(primary)
    }

    pub fn discover_executables(
        app_dir: &Path,
        pkg_id: &str,
    ) -> Result<(PathBuf, Vec<PathBuf>), String> {
        let mut executables = Vec::new();
        let mut bin_dir_exes = Vec::new();
        let search_dirs = [
            app_dir.join("usr/bin"),
            app_dir.join("bin"),
            app_dir.join("opt"),
        ];

        for (idx, s_dir) in search_dirs.iter().enumerate() {
            if s_dir.exists() {
                for entry in walkdir::WalkDir::new(s_dir).into_iter().flatten() {
                    let p = entry.path();
                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                    if p.is_file()
                        && !name.ends_with(".so")
                        && !name.contains(".so.")
                        && crate::formats::archive::ArchivePackage::is_executable_file(p)
                    {
                        let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
                        executables.push(p.to_path_buf());
                        if idx <= 1 {
                            bin_dir_exes.push(p.to_path_buf());
                        }
                    }
                }
            }
        }

        if executables.is_empty() {
            for entry in walkdir::WalkDir::new(app_dir).into_iter().flatten() {
                let p = entry.path();
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                if p.is_file()
                    && !name.ends_with(".so")
                    && !name.contains(".so.")
                    && crate::formats::archive::ArchivePackage::is_executable_file(p)
                {
                    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
                    executables.push(p.to_path_buf());
                }
            }
        }

        if executables.is_empty() {
            return Err("No executable binary found in deb package".to_string());
        }

        executables.sort();
        let primary = executables
            .iter()
            .find(|exe| {
                exe.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(pkg_id)
            })
            .cloned()
            .unwrap_or_else(|| executables[0].clone());

        let ignored_helpers = ["crashreporter", "updater", "elevate", "uninstall"];
        let mut seen_names = BTreeSet::new();
        if let Some(p_name) = primary.file_name().and_then(|n| n.to_str()) {
            seen_names.insert(p_name.to_string());
        }
        let mut secondary = Vec::new();
        bin_dir_exes.sort();
        for exe in bin_dir_exes {
            if exe == primary {
                continue;
            }
            let name = exe.file_name().unwrap_or_default().to_string_lossy();
            let lower = name.to_lowercase();
            if ignored_helpers.iter().any(|h| lower.contains(h)) {
                continue;
            }
            if seen_names.insert(name.to_string()) {
                secondary.push(exe);
            }
        }

        Ok((primary, secondary))
    }

    fn find_desktop_file(app_dir: &Path) -> Option<PathBuf> {
        for entry in walkdir::WalkDir::new(app_dir).into_iter().flatten() {
            let p = entry.path();
            if p.is_file() && p.extension().map(|e| e == "desktop").unwrap_or(false) {
                return Some(p.to_path_buf());
            }
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn setup_desktop_files(
        &self,
        app_dir: &Path,
        pkg_id: &str,
        launcher_path: &Path,
        desc: &str,
        orig_desktop_rel: Option<&Path>,
        binary_is_gui: bool,
        has_local_icon: bool,
    ) -> Vec<PathBuf> {
        let mut created = Vec::new();
        let dest_dt = self
            .config
            .applications_dir
            .join(format!("appmgr-{}.desktop", pkg_id));

        let icon_override = if has_local_icon { Some(pkg_id) } else { None };

        if let Some(rel) = orig_desktop_rel {
            let orig_dt = app_dir.join(rel);
            if let Ok(path) =
                install_patched_desktop_file(&orig_dt, &dest_dt, launcher_path, icon_override)
            {
                created.push(path);
                return created;
            }
        }

        if binary_is_gui
            && create_desktop_entry(
                &dest_dt,
                &pkg_id.replace('-', " "),
                desc,
                launcher_path,
                icon_override,
                "Utility;Application;",
            )
            .is_ok()
        {
            created.push(dest_dt);
        }

        created
    }

    fn setup_icons(&self, app_dir: &Path, pkg_id: &str) -> Vec<PathBuf> {
        let icons_root = app_dir.join("usr/share/icons");
        let pixmaps_root = app_dir.join("usr/share/pixmaps");
        install_best_icon(
            &[icons_root.as_path(), pixmaps_root.as_path()],
            &self.config.icons_dir,
            pkg_id,
        )
        .into_iter()
        .collect()
    }
}
