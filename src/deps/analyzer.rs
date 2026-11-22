use std::collections::{BTreeSet, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Command;

pub const CORE_HOST_ONLY_PREFIXES: &[&str] = &[
    "libc.so",
    "libm.so",
    "libdl.so",
    "librt.so",
    "libpthread.so",
    "libresolv.so",
    "ld-linux",
    "libstdc++.so",
    "libgcc_s.so",
    "libGL",
    "libEGL",
    "libGLX",
    "libOpenGL",
    "libvulkan",
    "libdrm",
    "libgbm",
    "libwayland",
    "libX11",
    "libxcb",
    "libnvidia",
    "libcuda",
    "libasound",
    "libpulse",
    "libpipewire",
];

pub fn is_core_host_only(soname: &str) -> bool {
    let lower = soname.to_lowercase();
    CORE_HOST_ONLY_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(&prefix.to_lowercase()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ElfClass {
    Elf32,
    Elf64,
}

impl std::fmt::Display for ElfClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ElfClass::Elf32 => write!(f, "32-bit"),
            ElfClass::Elf64 => write!(f, "64-bit"),
        }
    }
}

pub fn detect_elf_class(path: &Path) -> Option<ElfClass> {
    let mut f = File::open(path).ok()?;
    let mut header = [0u8; 5];
    f.read_exact(&mut header).ok()?;
    if &header[0..4] != b"\x7fELF" {
        return None;
    }
    match header[4] {
        1 => Some(ElfClass::Elf32),
        2 => Some(ElfClass::Elf64),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MissingLib {
    pub soname: String,
    pub elf_class: ElfClass,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppDepsAnalysis {
    pub missing_libs: Vec<MissingLib>,
    pub total_elfs: usize,
    pub dynamic_elfs: usize,
    pub app_elf_class: Option<ElfClass>,
}

/// Parses raw ldd output (stdout + stderr) for missing shared libraries.
/// Excludes libraries physically present in internal_sonames.
pub fn parse_ldd_output(
    output: &str,
    elf_class: ElfClass,
    internal_sonames: &HashSet<String>,
) -> Vec<MissingLib> {
    let mut missing = BTreeSet::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains("not found") {
            let soname_raw = if let Some((left, _)) = trimmed.split_once("=>") {
                left.trim()
            } else if let Some((_, right)) = trimmed.split_once("not found:") {
                right.trim()
            } else {
                trimmed.split_whitespace().next().unwrap_or("")
            };

            let soname = soname_raw.trim().trim_matches(':').trim();
            if soname.is_empty() || !soname.contains(".so") {
                continue;
            }

            if internal_sonames.contains(soname) {
                continue;
            }

            missing.insert(MissingLib {
                soname: soname.to_string(),
                elf_class,
            });
        }
    }

    missing.into_iter().collect()
}

/// Collects internal library paths and SONAMEs within app_dir,
/// so internal bundled libraries (e.g. libcef.so, libffmpeg.so) are not reported as missing.
pub fn collect_internal_library_environment(
    app_dir: &Path,
) -> (String, HashSet<String>) {
    let mut lib_dirs = BTreeSet::new();
    let mut sonames = HashSet::new();

    // Standard expected paths
    let standard_subdirs = [
        "deps/lib",
        "usr/lib",
        "usr/lib64",
        "usr/lib/x86_64-linux-gnu",
        "usr/lib/aarch64-linux-gnu",
        "lib",
        "lib64",
        "lib/x86_64-linux-gnu",
        "lib/aarch64-linux-gnu",
    ];

    for sub in &standard_subdirs {
        let d = app_dir.join(sub);
        if d.is_dir() {
            lib_dirs.insert(d);
        }
    }

    if app_dir.exists() {
        for entry in walkdir::WalkDir::new(app_dir).into_iter().flatten() {
            let p = entry.path();
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            if name.ends_with(".so") || name.contains(".so.") {
                sonames.insert(name.to_string());
                if let Some(parent) = p.parent() {
                    lib_dirs.insert(parent.to_path_buf());
                }
            }
        }
    }

    let joined_dirs = lib_dirs
        .into_iter()
        .map(|d| d.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(":");

    let full_ld_path = if let Ok(existing) = std::env::var("LD_LIBRARY_PATH") {
        if joined_dirs.is_empty() {
            existing
        } else if existing.is_empty() {
            joined_dirs
        } else {
            format!("{}:{}", joined_dirs, existing)
        }
    } else {
        joined_dirs
    };

    (full_ld_path, sonames)
}

/// Scans all ELF binaries in an installed application directory, runs ldd with
/// internal library paths configured, and aggregates missing shared libraries.
pub fn scan_app_dependencies(app_dir: &Path) -> AppDepsAnalysis {
    let mut analysis = AppDepsAnalysis::default();

    if !app_dir.exists() {
        return analysis;
    }

    let (ld_library_path, internal_sonames) = collect_internal_library_environment(app_dir);

    let mut candidate_elfs = Vec::new();
    for entry in walkdir::WalkDir::new(app_dir).into_iter().flatten() {
        let p = entry.path();
        if p.is_file() && !p.is_symlink() {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            // Focus on executables and non-shared library ELFs first
            if !name.ends_with(".so") && !name.contains(".so.") {
                if let Some(class) = detect_elf_class(p) {
                    candidate_elfs.push((p.to_path_buf(), class));
                }
            }
        }
    }

    // If no standalone executables found, scan all ELFs
    if candidate_elfs.is_empty() {
        for entry in walkdir::WalkDir::new(app_dir).into_iter().flatten() {
            let p = entry.path();
            if p.is_file() && !p.is_symlink() {
                if let Some(class) = detect_elf_class(p) {
                    candidate_elfs.push((p.to_path_buf(), class));
                }
            }
        }
    }

    analysis.total_elfs = candidate_elfs.len();
    let mut missing_set = BTreeSet::new();

    for (elf_path, elf_class) in candidate_elfs {
        if analysis.app_elf_class.is_none() {
            analysis.app_elf_class = Some(elf_class);
        }

        let mut cmd = Command::new("ldd");
        cmd.arg(&elf_path);
        if !ld_library_path.is_empty() {
            cmd.env("LD_LIBRARY_PATH", &ld_library_path);
        }

        if let Ok(output) = cmd.output() {
            let stdout_str = String::from_utf8_lossy(&output.stdout);
            let stderr_str = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{}\n{}", stdout_str, stderr_str);

            let lower = combined.to_lowercase();
            if lower.contains("not a dynamic executable") || lower.contains("statically linked") {
                continue;
            }

            analysis.dynamic_elfs += 1;
            let missing_for_bin = parse_ldd_output(&combined, elf_class, &internal_sonames);
            for m in missing_for_bin {
                missing_set.insert(m);
            }
        }
    }

    analysis.missing_libs = missing_set.into_iter().collect();
    analysis
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn test_core_host_only_prefixes() {
        assert!(is_core_host_only("libc.so.6"));
        assert!(is_core_host_only("libm.so.6"));
        assert!(is_core_host_only("libpthread.so.0"));
        assert!(is_core_host_only("libGL.so.1"));
        assert!(is_core_host_only("libEGL.so.1"));
        assert!(is_core_host_only("libwayland-client.so.0"));
        assert!(is_core_host_only("libX11.so.6"));
        assert!(is_core_host_only("libxcb.so.1"));
        assert!(is_core_host_only("libasound.so.2"));
        assert!(is_core_host_only("libpulse.so.0"));
        assert!(is_core_host_only("libpipewire-0.3.so.0"));

        // Non-host-only libraries should NOT be blocked
        assert!(!is_core_host_only("libayatana-indicator3.so.7"));
        assert!(!is_core_host_only("libayatana-ido3-0.4.so.0"));
        assert!(!is_core_host_only("libssl.so.3"));
        assert!(!is_core_host_only("libcrypto.so.3"));
        assert!(!is_core_host_only("libpng16.so.16"));
        assert!(!is_core_host_only("libicuuc.so.72"));
    }

    #[test]
    fn test_detect_elf_class() {
        let temp_dir = std::env::temp_dir().join("appmgr_test_elf_class");
        let _ = fs::create_dir_all(&temp_dir);

        let elf64_path = temp_dir.join("test_elf64");
        let mut f = File::create(&elf64_path).unwrap();
        f.write_all(b"\x7fELF\x02\x01\x01\x00").unwrap();

        let elf32_path = temp_dir.join("test_elf32");
        let mut f = File::create(&elf32_path).unwrap();
        f.write_all(b"\x7fELF\x01\x01\x01\x00").unwrap();

        let non_elf_path = temp_dir.join("test_non_elf");
        let mut f = File::create(&non_elf_path).unwrap();
        f.write_all(b"#!/usr/bin/env bash\n").unwrap();

        assert_eq!(detect_elf_class(&elf64_path), Some(ElfClass::Elf64));
        assert_eq!(detect_elf_class(&elf32_path), Some(ElfClass::Elf32));
        assert_eq!(detect_elf_class(&non_elf_path), None);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_parse_ldd_output_with_internal_filter() {
        let sample_output = r#"
        linux-vdso.so.1 (0x00007ffe341f4000)
        libayatana-indicator3.so.7 => not found
        libinternal.so.1 => not found
        libgtk-3.so.0 => /usr/lib/libgtk-3.so.0 (0x00007f9c2d000000)
        libayatana-ido3-0.4.so.0 => not found
        libc.so.6 => /usr/lib/libc.so.6 (0x00007f9c2ce00000)
        /lib64/ld-linux-x86-64.so.2 => /usr/lib64/ld-linux-x86-64.so.2 (0x00007f9c2da00000)
        "#;

        let mut internal_sonames = HashSet::new();
        internal_sonames.insert("libinternal.so.1".to_string());

        let missing = parse_ldd_output(sample_output, ElfClass::Elf64, &internal_sonames);
        assert_eq!(missing.len(), 2);
        assert_eq!(missing[0].soname, "libayatana-ido3-0.4.so.0");
        assert_eq!(missing[0].elf_class, ElfClass::Elf64);
        assert_eq!(missing[1].soname, "libayatana-indicator3.so.7");
        assert_eq!(missing[1].elf_class, ElfClass::Elf64);
    }
}
