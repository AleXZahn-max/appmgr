pub mod appimage;
pub mod archive;
pub mod binary;
pub mod deb;

use crate::db::PackageRecord;
use crate::ui::apt_prefix;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageFormat {
    Deb,
    AppImage,
    Tar,
    Zip,
    Binary,
    ArchNative,
}

impl PackageFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Deb => "deb",
            Self::AppImage => "appimage",
            Self::Tar => "tar",
            Self::Zip => "zip",
            Self::Binary => "bin",
            Self::ArchNative => "arch_pkg",
        }
    }
}

pub fn detect_format(path: &Path) -> Option<PackageFormat> {
    let name_lower = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    if name_lower.contains(".pkg.tar.") {
        return Some(PackageFormat::ArchNative);
    }
    if name_lower.ends_with(".deb") {
        return Some(PackageFormat::Deb);
    }
    if name_lower.ends_with(".appimage") {
        return Some(PackageFormat::AppImage);
    }
    if name_lower.ends_with(".zip") {
        return Some(PackageFormat::Zip);
    }

    let tar_extensions = [
        ".tar.xz", ".tar.zst", ".tar.gz", ".tgz", ".tar.bz2", ".tbz2", ".txz", ".tzst", ".tar",
    ];
    if tar_extensions.iter().any(|ext| name_lower.ends_with(ext)) {
        return Some(PackageFormat::Tar);
    }

    // Inspect file header magic bytes
    if let Ok(mut file) = File::open(path) {
        let mut header = [0u8; 1024];
        let bytes_read = file.read(&mut header).unwrap_or(0);
        if bytes_read >= 8 && &header[..7] == b"!<arch>" {
            return Some(PackageFormat::Deb);
        }
        if bytes_read >= 4 && &header[..4] == b"PK\x03\x04" {
            return Some(PackageFormat::Zip);
        }
        if bytes_read >= 4 && &header[..4] == b"\x7fELF" {
            if header[..bytes_read]
                .windows(3)
                .any(|w| w == b"AI\x02" || w == b"App")
            {
                return Some(PackageFormat::AppImage);
            }
            return Some(PackageFormat::Binary);
        }
        if bytes_read >= 2 && &header[..2] == b"\x1f\x8b" {
            return Some(PackageFormat::Tar);
        }
        if bytes_read >= 6 && &header[..6] == b"\xfd7zXZ\x00" {
            return Some(PackageFormat::Tar);
        }
        if bytes_read >= 4 && &header[..4] == b"\x28\xb5\x2f\xfd" {
            return Some(PackageFormat::Tar);
        }
        if bytes_read >= 3 && &header[..3] == b"BZh" {
            return Some(PackageFormat::Tar);
        }
    }

    None
}

/// Validates Debian architecture compatibility with the host architecture.
pub fn is_deb_arch_compatible(deb_arch: &str, host_arch: &str) -> bool {
    let norm = deb_arch.trim().to_lowercase();
    if norm.is_empty() || norm == "all" || norm == "any" {
        return true;
    }
    match host_arch {
        "x86_64" => matches!(norm.as_str(), "amd64" | "x86_64"),
        "x86" => matches!(norm.as_str(), "i386" | "i486" | "i586" | "i686"),
        "aarch64" => matches!(norm.as_str(), "arm64" | "aarch64"),
        "arm" => matches!(norm.as_str(), "armhf" | "armel" | "arm"),
        "riscv64" => norm == "riscv64",
        other => norm == other,
    }
}

/// Atomically swaps `staged` directory into `target` on the same filesystem.
pub fn atomic_swap_dir(staged: &Path, target: &Path, force: bool) -> Result<(), String> {
    let target_exists = fs::symlink_metadata(target).is_ok();
    if target_exists && !force {
        return Err(format!(
            "Package directory already exists: {}. Use -f to overwrite.",
            target.display()
        ));
    }

    let parent = target
        .parent()
        .ok_or_else(|| format!("Invalid target path: {}", target.display()))?;
    fs::create_dir_all(parent)
        .map_err(|e| format!("Failed to create parent dir {}: {}", parent.display(), e))?;

    if target_exists {
        let name = target
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let old_backup = parent.join(format!(".old-{}-{}", name, std::process::id()));
        if fs::symlink_metadata(&old_backup).is_ok() {
            let _ = fs::remove_dir_all(&old_backup);
        }
        fs::rename(target, &old_backup).map_err(|e| {
            format!(
                "Failed to move existing directory {} aside: {}",
                target.display(),
                e
            )
        })?;

        if let Err(e) = fs::rename(staged, target) {
            let _ = fs::rename(&old_backup, target);
            return Err(format!(
                "Failed to swap staged directory {} into {}: {}",
                staged.display(),
                target.display(),
                e
            ));
        }
        let _ = fs::remove_dir_all(&old_backup);
    } else {
        fs::rename(staged, target).map_err(|e| {
            format!(
                "Failed to move staged directory {} into {}: {}",
                staged.display(),
                target.display(),
                e
            )
        })?;
    }

    Ok(())
}

/// RAII staging directory on the same filesystem as `apps_dir`.
/// Automatically deletes staging files if dropped before `commit_swap` succeeds.
pub struct StagingDirGuard {
    pub path: PathBuf,
    committed: bool,
}

impl StagingDirGuard {
    pub fn new(apps_dir: &Path, pkg_id: &str) -> Result<Self, String> {
        fs::create_dir_all(apps_dir).map_err(|e| {
            format!(
                "Failed to create apps directory {}: {}",
                apps_dir.display(),
                e
            )
        })?;
        let path = apps_dir.join(format!(".staging-{}-{}", pkg_id, std::process::id()));
        if fs::symlink_metadata(&path).is_ok() {
            let _ = fs::remove_dir_all(&path);
        }
        fs::create_dir_all(&path).map_err(|e| {
            format!(
                "Failed to create staging directory {}: {}",
                path.display(),
                e
            )
        })?;
        Ok(Self {
            path,
            committed: false,
        })
    }

    pub fn commit_swap(
        &mut self,
        staged_root: &Path,
        target_dir: &Path,
        force: bool,
    ) -> Result<(), String> {
        atomic_swap_dir(staged_root, target_dir, force)?;
        self.committed = true;
        if staged_root != self.path && fs::symlink_metadata(&self.path).is_ok() {
            let _ = fs::remove_dir_all(&self.path);
        }
        Ok(())
    }
}

impl Drop for StagingDirGuard {
    fn drop(&mut self) {
        if !self.committed && fs::symlink_metadata(&self.path).is_ok() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Connects primary and secondary executables in `bin_dir` without clobbering unrelated binaries.
pub fn connect_package_binaries(
    record: &mut PackageRecord,
    bin_dir: &Path,
    secondary_rel_paths: &[PathBuf],
    owned_launchers: &[PathBuf],
) -> Result<(), String> {
    let primary_launcher = bin_dir.join(&record.id);
    record.binaries = vec![primary_launcher.clone()];
    record.secondary_binaries.clear();

    let mut claimed = BTreeSet::new();
    claimed.insert(record.id.clone());

    for rel in secondary_rel_paths {
        let Some(raw_name) = rel.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if raw_name.is_empty() || claimed.contains(raw_name) {
            continue;
        }

        let cand_path = bin_dir.join(raw_name);
        let collides =
            fs::symlink_metadata(&cand_path).is_ok() && !owned_launchers.contains(&cand_path);

        let chosen_name = if !collides {
            raw_name.to_string()
        } else {
            let fallback_name = format!("{}-{}", record.id, raw_name);
            let fallback_path = bin_dir.join(&fallback_name);
            let fallback_collides = (fs::symlink_metadata(&fallback_path).is_ok()
                && !owned_launchers.contains(&fallback_path))
                || claimed.contains(&fallback_name);
            if fallback_collides {
                println!(
                    " {} Skipping secondary binary '{}' (collision at {})",
                    apt_prefix("WARN"),
                    raw_name,
                    cand_path.display()
                );
                continue;
            }
            println!(
                " {} Binary '{}' already exists in {}; installing secondary launcher as '{}'",
                apt_prefix("WARN"),
                raw_name,
                bin_dir.display(),
                fallback_name
            );
            fallback_name
        };

        claimed.insert(chosen_name.clone());
        record
            .secondary_binaries
            .insert(chosen_name.clone(), rel.clone());
        record.binaries.push(bin_dir.join(&chosen_name));
    }

    crate::launcher::write_package_launchers(record, bin_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deb_arch_compatibility() {
        assert!(is_deb_arch_compatible("all", "x86_64"));
        assert!(is_deb_arch_compatible("any", "aarch64"));
        assert!(is_deb_arch_compatible("amd64", "x86_64"));
        assert!(is_deb_arch_compatible("x86_64", "x86_64"));
        assert!(!is_deb_arch_compatible("arm64", "x86_64"));
        assert!(!is_deb_arch_compatible("armhf", "x86_64"));
        assert!(!is_deb_arch_compatible("i386", "x86_64"));

        assert!(is_deb_arch_compatible("arm64", "aarch64"));
        assert!(!is_deb_arch_compatible("amd64", "aarch64"));
    }
}
