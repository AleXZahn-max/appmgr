use crate::config::Config;
use crate::db::PackageRecord;
use crate::desktop::{
    create_desktop_entry, inspect_elf_headers, install_best_icon, install_patched_desktop_file,
    is_gui_binary,
};
use crate::formats::StagingDirGuard;
use crate::launcher::write_package_launchers;
use crate::ui::apt_prefix;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn has_fuse2_runtime() -> bool {
    let has_fusermount = which::which("fusermount").is_ok() || which::which("fusermount3").is_ok();
    if !has_fusermount {
        return false;
    }

    if let Ok(output) = Command::new("ldconfig").arg("-p").output() {
        if String::from_utf8_lossy(&output.stdout).contains("libfuse.so.2") {
            return true;
        }
    }

    let std_paths = [
        "/lib/libfuse.so.2",
        "/lib64/libfuse.so.2",
        "/usr/lib/libfuse.so.2",
        "/usr/lib64/libfuse.so.2",
        "/usr/lib/x86_64-linux-gnu/libfuse.so.2",
        "/usr/lib/aarch64-linux-gnu/libfuse.so.2",
    ];
    std_paths.iter().any(|p| Path::new(p).exists())
}

fn is_valid_squashfs_superblock(buf: &[u8]) -> bool {
    if buf.len() < 32 || &buf[..4] != b"hsqs" {
        return false;
    }
    let block_size = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
    let s_major = u16::from_le_bytes([buf[28], buf[29]]);
    let s_minor = u16::from_le_bytes([buf[30], buf[31]]);
    s_major == 4
        && s_minor == 0
        && (4096..=1048576).contains(&block_size)
        && block_size.is_power_of_two()
}

pub fn find_squashfs_offset(path: &Path) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    let mut first = [0u8; 32];
    if file.read_exact(&mut first).is_ok() && is_valid_squashfs_superblock(&first) {
        return Some(0);
    }

    if let Some(hdr) = inspect_elf_headers(path) {
        if hdr.e_shoff > 0 && hdr.e_shentsize > 0 && hdr.e_shnum > 0 {
            let elf_end = hdr
                .e_shoff
                .saturating_add((hdr.e_shnum as u64).saturating_mul(hdr.e_shentsize as u64));
            if file.seek(SeekFrom::Start(elf_end)).is_ok() {
                let mut probe = [0u8; 32];
                if file.read_exact(&mut probe).is_ok() && is_valid_squashfs_superblock(&probe) {
                    return Some(elf_end);
                }
            }
        }
    }

    // Bounded scan across the first 8 MiB for embedded SquashFS superblock
    const MAX_SCAN: usize = 8 * 1024 * 1024;
    const CHUNK: usize = 64 * 1024;
    let _ = file.seek(SeekFrom::Start(0));
    let mut buf = vec![0u8; CHUNK + 32];
    let mut scanned = 0usize;
    let mut overlap = 0usize;

    while scanned < MAX_SCAN {
        let n = file.read(&mut buf[overlap..]).ok()?;
        if n == 0 {
            break;
        }
        let valid_len = overlap + n;
        for i in 0..valid_len.saturating_sub(31) {
            if &buf[i..i + 4] == b"hsqs" && is_valid_squashfs_superblock(&buf[i..i + 32]) {
                return Some((scanned.saturating_sub(overlap) + i) as u64);
            }
        }
        if valid_len >= 31 {
            buf.copy_within(valid_len - 31..valid_len, 0);
            overlap = 31;
        } else {
            overlap = 0;
        }
        scanned += n;
    }

    None
}

pub fn has_valid_appimage_signature(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut head = [0u8; 32 * 1024];
    let n = file.read(&mut head).unwrap_or(0);
    if n < 4 {
        return false;
    }
    if &head[..4] == b"hsqs" {
        return true;
    }
    if &head[..4] != b"\x7fELF" {
        return false;
    }
    if find_squashfs_offset(path).is_some() {
        return true;
    }
    head[..n].windows(3).any(|w| w == b"AI\x02" || w == b"App")
}

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

        let (raw_name, version) = crate::formats::parse_stem_name_and_version(
            &stem,
            self.repo.as_deref(),
            self.tag.as_deref(),
            false,
        );

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
        if file_size == 0 {
            return Err("Invalid AppImage file: file is empty".to_string());
        }

        let app_dir = self.config.apps_dir.join(&pkg_id);
        if app_dir.exists() && !force {
            return Err(format!(
                "Package directory already exists: {}. Use -f to overwrite.",
                app_dir.display()
            ));
        }

        // Stage on the same filesystem
        let mut staging = StagingDirGuard::new(&self.config.apps_dir, &pkg_id)?;
        let stage_path = staging.path.clone();

        let staged_appimage = stage_path.join(format!("{}.AppImage", pkg_id));
        fs::copy(&self.file_path, &staged_appimage)
            .map_err(|e| format!("Failed to copy AppImage: {}", e))?;
        fs::set_permissions(&staged_appimage, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to set permissions: {}", e))?;

        let extracted_tmp = tempfile::tempdir().ok();
        let squashfs_root = extracted_tmp
            .as_ref()
            .and_then(|d| Self::extract_squashfs_root(&staged_appimage, d.path()));

        let orig_desktop_path = squashfs_root
            .as_deref()
            .and_then(Self::find_desktop_in_root);
        let is_gui = orig_desktop_path.is_some()
            || squashfs_root
                .as_deref()
                .map(Self::detect_gui_in_extracted)
                .unwrap_or(false);

        if !has_fuse2_runtime() {
            println!(
                " {} libfuse.so.2 not found; wrapper will fall back to --appimage-extract-and-run (install fuse2 for native mounting).",
                apt_prefix("WARN")
            );
        }

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
            format: "appimage".to_string(),
            mode: self.config.mode.clone(),
            install_date: now,
            install_dir: app_dir,
            source_file: Some(self.file_path.to_string_lossy().to_string()),
            entrypoint: Some(PathBuf::from(format!("{}.AppImage", pkg_id))),
            binaries: vec![launcher_path.clone()],
            desktop_files: Vec::new(),
            icons: Vec::new(),
            installed_files_count: Some(1),
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
            ..Default::default()
        };

        write_package_launchers(&record, &self.config.bin_dir)?;

        let icon_installed = squashfs_root
            .as_deref()
            .and_then(|root| install_best_icon(&[root], &self.config.icons_dir, &pkg_id));

        if let Some(dt_path) = orig_desktop_path {
            let dest_dt = self
                .config
                .applications_dir
                .join(format!("appmgr-{}.desktop", pkg_id));
            if let Ok(installed_dt) =
                install_patched_desktop_file(&dt_path, &dest_dt, &launcher_path, Some(&pkg_id))
            {
                record.desktop_files.push(installed_dt);
            }
        } else if is_gui {
            let fallback_dt = self
                .config
                .applications_dir
                .join(format!("appmgr-{}.desktop", pkg_id));
            let icon_name = icon_installed.as_ref().map(|_| pkg_id.as_str());
            if create_desktop_entry(
                &fallback_dt,
                &pkg_id.replace('-', " "),
                &desc,
                &launcher_path,
                icon_name,
                "Utility;Application;",
            )
            .is_ok()
            {
                record.desktop_files.push(fallback_dt);
            }
        }

        record.icons = icon_installed.into_iter().collect();
        Ok(record)
    }

    fn extract_squashfs_root(appimage_path: &Path, work_dir: &Path) -> Option<PathBuf> {
        let squashfs_root = work_dir.join("squashfs-root");

        // 1. Prefer unsquashfs to extract resources without executing untrusted binaries
        if which::which("unsquashfs").is_ok() {
            if let Some(offset) = find_squashfs_offset(appimage_path) {
                let status = Command::new("unsquashfs")
                    .args([
                        "-f",
                        "-o",
                        &offset.to_string(),
                        "-d",
                        &squashfs_root.to_string_lossy(),
                        &appimage_path.to_string_lossy(),
                    ])
                    .env_clear()
                    .env("PATH", "/usr/bin:/bin")
                    .output();
                if status.map(|o| o.status.success()).unwrap_or(false) && squashfs_root.exists() {
                    return Some(squashfs_root);
                }
            }
        }

        // 2. Fallback to --appimage-extract only after verifying ELF/SquashFS header,
        //    running with env_clear() and minimal environment
        if !has_valid_appimage_signature(appimage_path) {
            return None;
        }

        let output = Command::new(appimage_path)
            .arg("--appimage-extract")
            .current_dir(work_dir)
            .env_clear()
            .env("APPIMAGE_EXTRACT_AND_RUN", "1")
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", work_dir)
            .env("TMPDIR", work_dir)
            .output()
            .ok()?;

        if output.status.success() && squashfs_root.exists() {
            Some(squashfs_root)
        } else {
            None
        }
    }

    fn find_desktop_in_root(squashfs_root: &Path) -> Option<PathBuf> {
        if let Ok(entries) = fs::read_dir(squashfs_root) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file() && p.extension().map(|e| e == "desktop").unwrap_or(false) {
                    return Some(p);
                }
            }
        }
        let apps_dir = squashfs_root.join("usr/share/applications");
        if apps_dir.exists() {
            if let Ok(entries) = fs::read_dir(&apps_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file() && p.extension().map(|e| e == "desktop").unwrap_or(false) {
                        return Some(p);
                    }
                }
            }
        }
        None
    }

    fn detect_gui_in_extracted(squashfs_root: &Path) -> bool {
        let apprun = squashfs_root.join("AppRun");
        if apprun.is_file() && is_gui_binary(&apprun) {
            return true;
        }
        for sub in ["usr/bin", "bin"] {
            let dir = squashfs_root.join(sub);
            if dir.is_dir() {
                if let Ok(entries) = fs::read_dir(&dir) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if p.is_file() && is_gui_binary(&p) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_appimage_read_metadata_version_normalization() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::resolve_with_env(
            false,
            true,
            false,
            dir.path().to_path_buf(),
            None,
            None,
            None,
        )
        .unwrap();

        let cases = [
            ("hibiki-v3.3.0-linux-x64.AppImage", "hibiki", "3.3.0"),
            ("Mornye-v5.4.5-linux-x86_64.AppImage", "mornye", "5.4.5"),
            (
                "tool-v1.2.0-rc.1-linux-amd64.AppImage",
                "tool",
                "1.2.0-rc.1",
            ),
            ("app-2.0.0-beta.2.AppImage", "app", "2.0.0-beta.2"),
        ];

        for (filename, expected_id, expected_ver) in cases {
            let file_path = dir.path().join(filename);
            fs::write(&file_path, b"dummy").unwrap();
            let pkg = AppImagePackage::new(&file_path, &cfg, None, None);
            let (id, ver, _, _) = pkg.read_metadata().unwrap();
            assert_eq!(id, expected_id, "unexpected id for {}", filename);
            assert_eq!(ver, expected_ver, "unexpected version for {}", filename);
        }

        let gh_file = dir.path().join("hibiki-v3.3.0-linux-x64.AppImage");
        let gh_pkg = AppImagePackage::new(
            &gh_file,
            &cfg,
            Some("owner/hibiki".to_string()),
            Some("v3.4.0".to_string()),
        );
        let (id, ver, _, _) = gh_pkg.read_metadata().unwrap();
        assert_eq!(id, "hibiki");
        assert_eq!(ver, "3.4.0");
    }
}
