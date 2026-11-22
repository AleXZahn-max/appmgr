pub mod appimage;
pub mod archive;
pub mod binary;
pub mod deb;

use std::fs::File;
use std::io::Read;
use std::path::Path;

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
