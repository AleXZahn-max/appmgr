use crate::config::Config;
use crate::db::PackageRecord;
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct AppImagePackage {
    pub file_path: PathBuf,
    pub config: Config,
    pub repo: Option<String>,
    pub tag: Option<String>,
}

impl AppImagePackage {
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

    pub fn read_metadata(&self) -> Result<(String, String, String, u64), String> {
        let file_size = fs::metadata(&self.file_path)
            .map_err(|e| format!("Failed to read metadata: {}", e))?
            .len();

        let stem = self
            .file_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "appimage".to_string());

        let ver_re = Regex::new(r"^([a-zA-Z0-9_\-]+?)[-_](v?\d+\..*)").unwrap();
        let (raw_name, version) = if let Some(caps) = ver_re.captures(&stem) {
            (
                caps.get(1).unwrap().as_str().to_string(),
                caps.get(2).unwrap().as_str().to_string(),
            )
        } else {
            (
                stem.clone(),
                self.tag.clone().unwrap_or_else(|| "1.0.0".to_string()),
            )
        };

        let pkg_id = Self::sanitize_name(&raw_name);
        let desc = format!("AppImage application: {}", raw_name);
        Ok((pkg_id, version, desc, file_size))
    }

    pub fn install(
        &self,
        force: bool,
        env: Option<HashMap<String, String>>,
        args: Option<Vec<String>>,
    ) -> Result<PackageRecord, String> {
        let (pkg_id, version, desc, file_size) = self.read_metadata()?;
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
            .map_err(|e| format!("Failed to create app directory: {}", e))?;

        let target_appimage = app_dir.join(format!("{}.AppImage", pkg_id));
        fs::copy(&self.file_path, &target_appimage)
            .map_err(|e| format!("Failed to copy AppImage: {}", e))?;
        fs::set_permissions(&target_appimage, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to set permissions: {}", e))?;

        // Extract resources (icon and .desktop) using --appimage-extract
        let (icon_installed, desktop_installed) =
            self.extract_resources(&target_appimage, &app_dir, &pkg_id);

        let launcher_path = self.config.bin_dir.join(&pkg_id);
        let env_map = env.unwrap_or_default();
        let args_vec = args.unwrap_or_default();

        let mut env_lines = Vec::new();
        for (k, v) in &env_map {
            env_lines.push(format!("export {}=\"{}\"", k, v));
        }
        let env_block = if env_lines.is_empty() {
            String::new()
        } else {
            format!("\n{}", env_lines.join("\n"))
        };

        let args_block = if args_vec.is_empty() {
            "\nPERSISTENT_ARGS=()\n".to_string()
        } else {
            let quoted: Vec<String> = args_vec.iter().map(|a| format!("    \"{}\"", a)).collect();
            format!("\nPERSISTENT_ARGS=(\n{}\n)\n", quoted.join("\n"))
        };

        let script = format!(
            r#"#!/usr/bin/env bash
# Wrapper created by appmgr for {pkg_id} (AppImage)
APPIMAGE="{target_appimage}"{env_block}
{args_block}
if [ -z "$APPIMAGE_EXTRACT_AND_RUN" ]; then
    if ! command -v fusermount &>/dev/null && ! command -v fusermount3 &>/dev/null; then
        export APPIMAGE_EXTRACT_AND_RUN=1
    fi
fi

exec "$APPIMAGE" "${{PERSISTENT_ARGS[@]}}" "$@"
"#,
            pkg_id = pkg_id,
            target_appimage = target_appimage.display(),
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

        let mut desktop_files = Vec::new();
        if let Some(dt) = desktop_installed {
            desktop_files.push(dt);
        } else {
            let fallback_dt = self
                .config
                .applications_dir
                .join(format!("appmgr-{}.desktop", pkg_id));
            let content = format!(
                r#"[Desktop Entry]
Name={}
Comment={}
Exec="{}" %U
Terminal=false
Type=Application
Icon=application-x-executable
Categories=Utility;Application;
"#,
                pkg_id.replace('-', " "),
                desc,
                launcher_path.display()
            );
            if fs::write(&fallback_dt, content).is_ok() {
                desktop_files.push(fallback_dt);
            }
        }

        let icons = icon_installed.into_iter().collect();
        let now = chrono::Local::now().to_rfc3339();

        Ok(PackageRecord {
            id: pkg_id.clone(),
            name: pkg_id,
            version,
            format: "appimage".to_string(),
            mode: self.config.mode.clone(),
            install_date: now,
            install_dir: app_dir,
            source_file: Some(self.file_path.to_string_lossy().to_string()),
            binaries: vec![launcher_path],
            desktop_files,
            icons,
            installed_files_count: Some(1),
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

    fn extract_resources(
        &self,
        appimage_path: &Path,
        _app_dir: &Path,
        pkg_id: &str,
    ) -> (Option<PathBuf>, Option<PathBuf>) {
        let temp_dir = match tempfile::tempdir() { Ok(d) => d, Err(_) => return (None, None) };
        let output = Command::new(appimage_path)
            .arg("--appimage-extract")
            .current_dir(temp_dir.path())
            .env("APPIMAGE_EXTRACT_AND_RUN", "1")
            .output();

        if output.is_err() {
            return (None, None);
        }

        let squashfs_root = temp_dir.path().join("squashfs-root");
        if !squashfs_root.exists() {
            return (None, None);
        }

        // Find .desktop file
        let mut found_desktop = None;
        if let Ok(entries) = fs::read_dir(&squashfs_root) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file()
                    && p.extension().map(|e| e == "desktop").unwrap_or(false)
                {
                    found_desktop = Some(p);
                    break;
                }
            }
        }

        // Patch and install desktop file
        let mut installed_desktop = None;
        if let Some(dt_path) = found_desktop {
            if let Ok(content) = fs::read_to_string(&dt_path) {
                let launcher = self.config.bin_dir.join(pkg_id);
                let mut new_lines = Vec::new();
                for line in content.lines() {
                    if line.starts_with("Exec=") {
                        new_lines.push(format!("Exec=\"{}\" %U", launcher.display()));
                    } else if line.starts_with("Icon=") {
                        new_lines.push(format!("Icon={}", pkg_id));
                    } else {
                        new_lines.push(line.to_string());
                    }
                }
                let dest_dt = self
                    .config
                    .applications_dir
                    .join(format!("appmgr-{}.desktop", pkg_id));
                if let Some(parent) = dest_dt.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if fs::write(&dest_dt, new_lines.join("\n") + "\n").is_ok() {
                    installed_desktop = Some(dest_dt);
                }
            }
        }

        // Find and install icon
        let mut installed_icon = None;
        let icon_extensions = [".png", ".svg"];
        if let Ok(entries) = fs::read_dir(&squashfs_root) {
            for entry in entries.flatten() {
                let p = entry.path();
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                if icon_extensions.iter().any(|ext| name.ends_with(ext)) && !name.contains(".DirIcon") {
                    let ext = p.extension().unwrap_or_default().to_string_lossy();
                    let dest_icon_dir = self.config.icons_dir.join("hicolor/256x256/apps");
                    let _ = fs::create_dir_all(&dest_icon_dir);
                    let dest_icon = dest_icon_dir.join(format!("{}.{}", pkg_id, ext));
                    if fs::copy(&p, &dest_icon).is_ok() {
                        installed_icon = Some(dest_icon);
                        break;
                    }
                }
            }
        }

        (installed_icon, installed_desktop)
    }
}
