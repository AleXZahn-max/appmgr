use crate::config::Config;
use crate::db::PackageRecord;
use crate::desktop::{create_desktop_entry, is_gui_binary};
use regex::Regex;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

pub enum ArchiveKind {
    TarGz,
    TarXz,
    TarZst,
    TarBz2,
    TarPlain,
    Zip,
}

pub struct ArchivePackage {
    pub file_path: PathBuf,
    pub config: Config,
    pub kind: ArchiveKind,
    pub repo: Option<String>,
    pub tag: Option<String>,
}

impl ArchivePackage {
    pub fn new(
        file_path: &Path,
        config: &Config,
        repo: Option<String>,
        tag: Option<String>,
    ) -> Result<Self, String> {
        let name_lower = file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        let kind = if name_lower.ends_with(".zip") {
            ArchiveKind::Zip
        } else if name_lower.ends_with(".tar.gz") || name_lower.ends_with(".tgz") {
            ArchiveKind::TarGz
        } else if name_lower.ends_with(".tar.xz") || name_lower.ends_with(".txz") {
            ArchiveKind::TarXz
        } else if name_lower.ends_with(".tar.zst") || name_lower.ends_with(".tzst") {
            ArchiveKind::TarZst
        } else if name_lower.ends_with(".tar.bz2") || name_lower.ends_with(".tbz2") {
            ArchiveKind::TarBz2
        } else if name_lower.ends_with(".tar") {
            ArchiveKind::TarPlain
        } else {
            return Err(format!(
                "Unsupported archive format: {}",
                file_path.display()
            ));
        };

        Ok(Self {
            file_path: file_path.to_path_buf(),
            config: config.clone(),
            kind,
            repo,
            tag,
        })
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
            .map_err(|e| format!("Failed to read archive metadata: {}", e))?
            .len();

        let stem = self
            .file_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "archive".to_string());

        let clean_stem = stem
            .trim_end_matches(".zip")
            .trim_end_matches(".tar.gz")
            .trim_end_matches(".tgz")
            .trim_end_matches(".tar.xz")
            .trim_end_matches(".txz")
            .trim_end_matches(".tar.zst")
            .trim_end_matches(".tzst")
            .trim_end_matches(".tar.bz2")
            .trim_end_matches(".tbz2")
            .trim_end_matches(".tar");

        let ver_re = Regex::new(r"^([a-zA-Z0-9_\-]+?)[-_](v?\d+\..*)").unwrap();
        let (raw_name, version) = if let Some(caps) = ver_re.captures(clean_stem) {
            (
                caps.get(1).unwrap().as_str().to_string(),
                caps.get(2).unwrap().as_str().to_string(),
            )
        } else {
            let parts: Vec<&str> = clean_stem.split('-').collect();
            let name = parts.first().unwrap_or(&clean_stem).to_string();
            let ver = self.tag.clone().unwrap_or_else(|| "1.0.0".to_string());
            (name, ver)
        };

        let pkg_id = Self::sanitize_name(&raw_name);
        let desc = format!("Binary archive application: {}", raw_name);
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
            .map_err(|e| format!("Failed to create destination dir: {}", e))?;

        let temp_extract_dir = tempfile::tempdir()
            .map_err(|e| format!("Failed to create staging directory: {}", e))?;
        let staging_path = temp_extract_dir.path();

        // 1. Safe extraction with Path Traversal (Tar/Zip Slip) Defense
        match self.kind {
            ArchiveKind::Zip => {
                Self::extract_zip_safe(&self.file_path, staging_path)?;
            }
            ArchiveKind::TarGz => {
                let file = File::open(&self.file_path)
                    .map_err(|e| format!("Failed to open file: {}", e))?;
                let decoder = flate2::read::GzDecoder::new(file);
                Self::extract_tar_safe(decoder, staging_path)?;
            }
            ArchiveKind::TarXz => {
                let file = File::open(&self.file_path)
                    .map_err(|e| format!("Failed to open file: {}", e))?;
                let decoder = xz2::read::XzDecoder::new(file);
                Self::extract_tar_safe(decoder, staging_path)?;
            }
            ArchiveKind::TarZst => {
                let file = File::open(&self.file_path)
                    .map_err(|e| format!("Failed to open file: {}", e))?;
                let decoder = zstd::Decoder::new(file)
                    .map_err(|e| format!("Failed to init zstd decoder: {}", e))?;
                Self::extract_tar_safe(decoder, staging_path)?;
            }
            ArchiveKind::TarBz2 => {
                let file = File::open(&self.file_path)
                    .map_err(|e| format!("Failed to open file: {}", e))?;
                let decoder = bzip2::read::BzDecoder::new(file);
                Self::extract_tar_safe(decoder, staging_path)?;
            }
            ArchiveKind::TarPlain => {
                let file = File::open(&self.file_path)
                    .map_err(|e| format!("Failed to open file: {}", e))?;
                Self::extract_tar_safe(file, staging_path)?;
            }
        }

        // 2. Check for Single Root Directory (auto-collapse --strip-components=1)
        let root_to_copy = Self::resolve_single_root(staging_path);

        // 3. Move extracted files into app_dir
        Self::copy_dir_all(&root_to_copy, &app_dir)?;

        // 4. Discover Executables and Check Against Source Code Only Archives
        let primary_bin = Self::discover_and_rank_binaries(&app_dir, &pkg_id)?;

        // 5. Setup Launcher Script in PATH
        let launcher_path = self.config.bin_dir.join(&pkg_id);
        let rel_bin = primary_bin
            .strip_prefix(&app_dir)
            .map_err(|e| format!("Path error: {}", e))?;

        let mut lib_dirs = Vec::new();
        for cand in &["lib", "lib64", "usr/lib", "usr/lib64"] {
            if app_dir.join(cand).exists() {
                lib_dirs.push(format!("$APP_ROOT/{}", cand));
            }
        }
        let extra_ld = if lib_dirs.is_empty() {
            String::new()
        } else {
            format!(":{}", lib_dirs.join(":"))
        };

        let env_map = env.unwrap_or_default();
        let args_vec = args.unwrap_or_default();

        let env_block = crate::sandbox::format_env_exports(&env_map)?;
        let args_block = format!("\n{}", crate::sandbox::format_persistent_args(&args_vec));

        let fmt_name = match self.kind {
            ArchiveKind::Zip => "Zip",
            _ => "Tar",
        };

        let script = format!(
            r#"#!/usr/bin/env bash
# Wrapper created by appmgr for {pkg_id} ({fmt_name} binary)
APP_ROOT={app_dir}
export PATH="$APP_ROOT/bin:$APP_ROOT/usr/bin:$APP_ROOT:$PATH"
export LD_LIBRARY_PATH="$APP_ROOT/deps/lib:$APP_ROOT/usr/lib:$APP_ROOT/lib{extra_ld}:${{LD_LIBRARY_PATH}}"
export XDG_DATA_DIRS="$APP_ROOT/share:$APP_ROOT/usr/share:${{XDG_DATA_DIRS:-/usr/local/share:/usr/share}}"{env_block}
{args_block}
exec "$APP_ROOT/"{rel_bin} "${{PERSISTENT_ARGS[@]}}" "$@"
"#,
            pkg_id = pkg_id,
            fmt_name = fmt_name,
            app_dir = crate::sandbox::posix_quote(&app_dir.to_string_lossy()),
            rel_bin = crate::sandbox::posix_quote(&rel_bin.to_string_lossy()),
            extra_ld = extra_ld,
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

        // 6. GUI Desktop & Icon Setup
        let is_gui = is_gui_binary(&primary_bin);
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

        let fmt_str = match self.kind {
            ArchiveKind::Zip => "zip",
            _ => "tar",
        };

        let now = chrono::Local::now().to_rfc3339();
        Ok(PackageRecord {
            id: pkg_id.clone(),
            name: pkg_id,
            version,
            format: fmt_str.to_string(),
            mode: self.config.mode.clone(),
            install_date: now,
            install_dir: app_dir,
            source_file: Some(self.file_path.to_string_lossy().to_string()),
            binaries: vec![launcher_path],
            desktop_files,
            icons: Vec::new(),
            installed_files_count: None,
            total_size_bytes: Some(file_size),
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

    pub fn is_path_safe(dest: &Path, target: &Path) -> bool {
        // Prevent Path Traversal (Tar Slip / Zip Slip)
        for comp in target.components() {
            match comp {
                Component::ParentDir => return false,
                Component::Prefix(_) | Component::RootDir => return false,
                _ => {}
            }
        }
        let full_path = dest.join(target);
        full_path.starts_with(dest)
    }

    pub fn extract_tar_safe<R: Read>(reader: R, dest_dir: &Path) -> Result<(), String> {
        let mut archive = tar::Archive::new(reader);
        for entry_res in archive.entries().map_err(|e| format!("Tar error: {}", e))? {
            let mut entry = entry_res.map_err(|e| format!("Tar entry error: {}", e))?;
            let path = entry
                .path()
                .map_err(|e| format!("Tar path error: {}", e))?
                .to_path_buf();
            if !Self::is_path_safe(dest_dir, &path) {
                return Err(format!(
                    "Tar Slip / Path traversal attempt detected: {}",
                    path.display()
                ));
            }
            entry
                .unpack_in(dest_dir)
                .map_err(|e| format!("Failed to unpack {}: {}", path.display(), e))?;
        }
        Ok(())
    }

    pub fn extract_zip_safe(zip_path: &Path, dest_dir: &Path) -> Result<(), String> {
        let file = File::open(zip_path)
            .map_err(|e| format!("Failed to open zip {}: {}", zip_path.display(), e))?;
        let mut archive =
            zip::ZipArchive::new(file).map_err(|e| format!("Corrupt zip file: {}", e))?;

        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| format!("Zip index error: {}", e))?;
            let enclosed = match file.enclosed_name() {
                Some(p) => p.to_owned(),
                None => {
                    return Err(format!(
                        "Zip Slip / Path traversal attempt detected: {}",
                        file.name()
                    ));
                }
            };

            if !Self::is_path_safe(dest_dir, &enclosed) {
                return Err(format!(
                    "Zip Slip / Path traversal attempt detected: {}",
                    enclosed.display()
                ));
            }

            let outpath = dest_dir.join(&enclosed);
            if file.is_dir() {
                fs::create_dir_all(&outpath)
                    .map_err(|e| format!("Failed to create dir {}: {}", outpath.display(), e))?;
            } else {
                if let Some(p) = outpath.parent() {
                    fs::create_dir_all(p).map_err(|e| {
                        format!("Failed to create parent dir {}: {}", p.display(), e)
                    })?;
                }
                let mut outfile = File::create(&outpath)
                    .map_err(|e| format!("Failed to create file {}: {}", outpath.display(), e))?;
                io::copy(&mut file, &mut outfile)
                    .map_err(|e| format!("Failed to write file {}: {}", outpath.display(), e))?;

                if let Some(mode) = file.unix_mode() {
                    let _ = fs::set_permissions(&outpath, fs::Permissions::from_mode(mode));
                }
            }
        }
        Ok(())
    }

    pub fn resolve_single_root(staging_dir: &Path) -> PathBuf {
        if let Ok(entries) = fs::read_dir(staging_dir) {
            let valid_entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            if valid_entries.len() == 1 && valid_entries[0].is_dir() {
                return valid_entries[0].clone();
            }
        }
        staging_dir.to_path_buf()
    }

    pub fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), String> {
        fs::create_dir_all(dst)
            .map_err(|e| format!("Failed to create dest dir {}: {}", dst.display(), e))?;
        for entry in walkdir::WalkDir::new(src) {
            let entry = entry.map_err(|e| format!("Walkdir error: {}", e))?;
            let rel = entry
                .path()
                .strip_prefix(src)
                .map_err(|e| format!("Strip prefix error: {}", e))?;
            let target = dst.join(rel);
            if entry.file_type().is_dir() {
                fs::create_dir_all(&target).map_err(|e| format!("Failed to create dir: {}", e))?;
            } else if entry.file_type().is_symlink() {
                #[cfg(unix)]
                {
                    let link_target = fs::read_link(entry.path()).map_err(|e| {
                        format!("Failed to read symlink {}: {}", entry.path().display(), e)
                    })?;
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|e| format!("Failed to create parent dir: {}", e))?;
                    }
                    let _ = fs::remove_file(&target);
                    std::os::unix::fs::symlink(&link_target, &target).map_err(|e| {
                        format!("Failed to create symlink {}: {}", target.display(), e)
                    })?;
                }
            } else if entry.file_type().is_file() {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("Failed to create parent dir: {}", e))?;
                }
                fs::copy(entry.path(), &target)
                    .map_err(|e| format!("Failed to copy file: {}", e))?;
                if let Ok(meta) = entry.metadata() {
                    let _ = fs::set_permissions(&target, meta.permissions());
                }
            }
        }
        Ok(())
    }

    pub fn discover_and_rank_binaries(app_dir: &Path, pkg_id: &str) -> Result<PathBuf, String> {
        let mut candidates = Vec::new();
        let mut has_source_indicators = false;

        let source_files = [
            "Makefile",
            "Cargo.toml",
            "CMakeLists.txt",
            "go.mod",
            "pom.xml",
        ];

        for entry in walkdir::WalkDir::new(app_dir).into_iter().flatten() {
            let p = entry.path();
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            if source_files.contains(&name.as_ref()) {
                has_source_indicators = true;
            }
            if p.is_file() && !name.ends_with(".so") && !name.contains(".so.") {
                let is_exec = Self::is_executable_file(p);
                if is_exec {
                    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
                    candidates.push(p.to_path_buf());
                }
            }
        }

        if candidates.is_empty() {
            if has_source_indicators {
                return Err("Source code archive detected without pre-compiled binaries. Compilation is not supported.".to_string());
            }
            return Err(
                "No executable ELF binaries or runnable scripts found in archive".to_string(),
            );
        }

        // Rank binaries: prefer exact pkg_id match, ignore helpers (updater, crashreporter)
        let ignored_helpers = ["crashreporter", "updater", "elevate", "uninstall"];
        let mut best_cand = candidates[0].clone();
        let mut best_score = -100;

        for cand in &candidates {
            let name = cand
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            let mut score = 0;
            if name == pkg_id.to_lowercase() {
                score += 100;
            } else if name.contains(&pkg_id.to_lowercase()) {
                score += 50;
            }
            if ignored_helpers.iter().any(|h| name.contains(h)) {
                score -= 60;
            }
            if cand.to_string_lossy().contains("/bin/") {
                score += 20;
            }
            if score > best_score {
                best_score = score;
                best_cand = cand.clone();
            }
        }

        Ok(best_cand)
    }

    pub fn is_executable_file(path: &Path) -> bool {
        if let Ok(mut file) = File::open(path) {
            let mut magic = [0u8; 4];
            if file.read_exact(&mut magic).is_ok() {
                if &magic == b"\x7fELF" {
                    return true;
                }
                if &magic[..2] == b"#!" {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_path_safety_check() {
        let dest = Path::new("/home/user/apps/myapp");
        assert!(ArchivePackage::is_path_safe(dest, Path::new("bin/app")));
        assert!(ArchivePackage::is_path_safe(
            dest,
            Path::new("lib/libfoo.so")
        ));
        assert!(!ArchivePackage::is_path_safe(
            dest,
            Path::new("../../../etc/passwd")
        ));
        assert!(!ArchivePackage::is_path_safe(
            dest,
            Path::new("/etc/shadow")
        ));
        assert!(!ArchivePackage::is_path_safe(
            dest,
            Path::new("foo/../../../../root/.bashrc")
        ));
        assert!(!ArchivePackage::is_path_safe(dest, Path::new("..")));
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_all_preserves_symlinks() {
        let src = tempdir().unwrap();
        let dst = tempdir().unwrap();

        let lib_dir = src.path().join("lib");
        fs::create_dir_all(&lib_dir).unwrap();
        fs::write(lib_dir.join("libdemo.so.1.2"), b"elf").unwrap();
        std::os::unix::fs::symlink("libdemo.so.1.2", lib_dir.join("libdemo.so.1")).unwrap();
        std::os::unix::fs::symlink("libdemo.so.1", lib_dir.join("libdemo.so")).unwrap();
        std::os::unix::fs::symlink("/opt/nonexistent/target", lib_dir.join("abs_broken")).unwrap();

        ArchivePackage::copy_dir_all(src.path(), dst.path()).unwrap();

        let dst_lib = dst.path().join("lib");
        let meta1 = fs::symlink_metadata(dst_lib.join("libdemo.so.1")).unwrap();
        assert!(meta1.file_type().is_symlink());
        assert_eq!(
            fs::read_link(dst_lib.join("libdemo.so.1")).unwrap(),
            PathBuf::from("libdemo.so.1.2")
        );

        let meta2 = fs::symlink_metadata(dst_lib.join("libdemo.so")).unwrap();
        assert!(meta2.file_type().is_symlink());
        assert_eq!(
            fs::read_link(dst_lib.join("libdemo.so")).unwrap(),
            PathBuf::from("libdemo.so.1")
        );

        let meta_abs = fs::symlink_metadata(dst_lib.join("abs_broken")).unwrap();
        assert!(meta_abs.file_type().is_symlink());
        assert_eq!(
            fs::read_link(dst_lib.join("abs_broken")).unwrap(),
            PathBuf::from("/opt/nonexistent/target")
        );
    }
}
