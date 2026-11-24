use crate::deps::analyzer::{ElfClass, MissingLib};
use crate::ui::{apt_prefix, c, confirm_dialog, print_table, Colors};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPackageManagerKind {
    Pacman,
    Dnf,
    Apt,
    Zypper,
}

impl HostPackageManagerKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Pacman => "pacman",
            Self::Dnf => "dnf",
            Self::Apt => "apt",
            Self::Zypper => "zypper",
        }
    }

    pub fn build_install_command(&self, packages: &[String]) -> Vec<String> {
        let mut cmd = Vec::new();
        match self {
            Self::Pacman => {
                cmd.push("sudo".to_string());
                cmd.push("pacman".to_string());
                cmd.push("-S".to_string());
                cmd.push("--needed".to_string());
                cmd.extend(packages.iter().cloned());
            }
            Self::Dnf => {
                cmd.push("sudo".to_string());
                cmd.push("dnf".to_string());
                cmd.push("install".to_string());
                cmd.extend(packages.iter().cloned());
            }
            Self::Apt => {
                cmd.push("sudo".to_string());
                cmd.push("apt-get".to_string());
                cmd.push("install".to_string());
                cmd.extend(packages.iter().cloned());
            }
            Self::Zypper => {
                cmd.push("sudo".to_string());
                cmd.push("zypper".to_string());
                cmd.push("install".to_string());
                cmd.extend(packages.iter().cloned());
            }
        }
        cmd
    }
}

pub fn detect_host_package_manager() -> Option<HostPackageManagerKind> {
    if which::which("pacman").is_ok() {
        Some(HostPackageManagerKind::Pacman)
    } else if which::which("dnf").is_ok() {
        Some(HostPackageManagerKind::Dnf)
    } else if which::which("apt-get").is_ok() {
        Some(HostPackageManagerKind::Apt)
    } else if which::which("zypper").is_ok() {
        Some(HostPackageManagerKind::Zypper)
    } else {
        None
    }
}

pub fn check_pacman_files_db_exists() -> bool {
    let sync_dir = Path::new("/var/lib/pacman/sync");
    if let Ok(entries) = fs::read_dir(sync_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".files") {
                return true;
            }
        }
    }
    false
}

/// Parses raw pacman --machinereadable output:
/// format: repository\0pkgname\0pkgver\0path\n
pub fn parse_pacman_machinereadable(raw_output: &[u8], elf_class: ElfClass) -> Option<String> {
    let text = String::from_utf8_lossy(raw_output);
    let mut candidates = Vec::new();

    for line in text.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parts: Vec<&str> = trimmed.split('\0').collect();
        if parts.len() >= 4 {
            let repo = parts[0];
            let pkg = parts[1];
            let path = parts[3];

            candidates.push((repo.to_string(), pkg.to_string(), path.to_string()));
        }
    }

    select_pacman_candidate(&candidates, elf_class)
}

/// Selects the best candidate package from a list of (repo, pkg, path) tuples based on architecture.
pub fn select_pacman_candidate(
    candidates: &[(String, String, String)],
    elf_class: ElfClass,
) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }

    match elf_class {
        ElfClass::Elf64 => {
            // Priority 1: usr/lib/ or lib/ (NOT usr/lib32), repo is not multilib, name does not start with lib32-
            for (repo, pkg, path) in candidates {
                if !path.starts_with("usr/lib32")
                    && !repo.eq_ignore_ascii_case("multilib")
                    && !pkg.starts_with("lib32-")
                {
                    return Some(pkg.clone());
                }
            }
            // Fallback: any candidate that does not start with lib32-
            for (_, pkg, _) in candidates {
                if !pkg.starts_with("lib32-") {
                    return Some(pkg.clone());
                }
            }
        }
        ElfClass::Elf32 => {
            // Priority 1: path is under usr/lib32, or package starts with lib32-
            for (_, pkg, path) in candidates {
                if path.starts_with("usr/lib32") || pkg.starts_with("lib32-") {
                    return Some(pkg.clone());
                }
            }
        }
    }

    candidates.first().map(|(_, p, _)| p.clone())
}

/// Queries the host package manager for a missing library.
pub fn query_host_for_library(
    pm: HostPackageManagerKind,
    missing: &MissingLib,
) -> Result<Option<String>, String> {
    match pm {
        HostPackageManagerKind::Pacman => {
            if !check_pacman_files_db_exists() {
                return Err(
                    "Pacman files database is not synchronized (/var/lib/pacman/sync/*.files missing). Please run 'sudo pacman -Fy' to synchronize file records.".to_string()
                );
            }

            // Try machinereadable first
            let output = Command::new("pacman")
                .args(["-F", "--machinereadable", &missing.soname])
                .output()
                .map_err(|e| format!("Failed to execute pacman: {}", e))?;

            if output.status.success() && !output.stdout.is_empty() {
                if let Some(pkg) = parse_pacman_machinereadable(&output.stdout, missing.elf_class) {
                    return Ok(Some(pkg));
                }
            }

            // Fallback: query pacman -F
            let human_out = Command::new("pacman")
                .args(["-F", &missing.soname])
                .output()
                .map_err(|e| format!("Failed to execute pacman -F: {}", e))?;

            if human_out.status.success() {
                let text = String::from_utf8_lossy(&human_out.stdout);
                let mut candidates = Vec::new();
                let mut current_pkg = None;

                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if !line.starts_with(' ') && !line.starts_with('\t') {
                        // Header line: repo/pkgname version
                        if let Some((repo_pkg, _)) = trimmed.split_once(' ') {
                            if let Some((repo, pkg)) = repo_pkg.split_once('/') {
                                current_pkg = Some((repo.to_string(), pkg.to_string()));
                            }
                        }
                    } else if let Some((ref r, ref p)) = current_pkg {
                        candidates.push((r.clone(), p.clone(), trimmed.to_string()));
                    }
                }

                if let Some(pkg) = select_pacman_candidate(&candidates, missing.elf_class) {
                    return Ok(Some(pkg));
                }
            }

            Ok(None)
        }
        HostPackageManagerKind::Dnf => {
            let output = Command::new("dnf")
                .args(["provides", "--quiet", &format!("*/{}", missing.soname)])
                .output()
                .map_err(|e| format!("Failed to execute dnf: {}", e))?;

            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    // Sample line: "libfoo-1.0-1.fc38.x86_64 : Library description"
                    let pkg_part = trimmed.split(':').next().unwrap_or("").trim();
                    if let Some((name_ver, arch)) = pkg_part.rsplit_once('.') {
                        let is_match = match missing.elf_class {
                            ElfClass::Elf64 => {
                                arch == "x86_64" || arch == "aarch64" || arch == "noarch"
                            }
                            ElfClass::Elf32 => arch == "i686" || arch == "i386",
                        };
                        if is_match {
                            if let Some((name, _)) = name_ver.rsplit_once('-') {
                                return Ok(Some(name.to_string()));
                            }
                            return Ok(Some(name_ver.to_string()));
                        }
                    }
                }
            }

            Ok(None)
        }
        HostPackageManagerKind::Apt => {
            if which::which("apt-file").is_err() {
                return Err(
                    "Command 'apt-file' is not installed. Run 'sudo apt-get install -y apt-file && sudo apt-file update' to enable native package lookup.".to_string()
                );
            }

            let output = Command::new("apt-file")
                .args(["search", &format!("/{}", missing.soname)])
                .output()
                .map_err(|e| format!("Failed to execute apt-file: {}", e))?;

            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Some((pkg, path)) = trimmed.split_once(':') {
                        let path_trim = path.trim();
                        let is_match = match missing.elf_class {
                            ElfClass::Elf64 => !path_trim.contains("i386-linux-gnu"),
                            ElfClass::Elf32 => {
                                path_trim.contains("i386-linux-gnu") || path_trim.contains("lib32")
                            }
                        };
                        if is_match {
                            return Ok(Some(pkg.trim().to_string()));
                        }
                    }
                }
            }

            Ok(None)
        }
        HostPackageManagerKind::Zypper => {
            let output = Command::new("zypper")
                .args(["--no-refresh", "se", "--provides", &missing.soname])
                .output()
                .map_err(|e| format!("Failed to execute zypper: {}", e))?;

            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with('|') || trimmed.starts_with("---") || trimmed.is_empty()
                    {
                        continue;
                    }
                    let parts: Vec<&str> = trimmed.split('|').map(|s| s.trim()).collect();
                    if parts.len() >= 2 && !parts[1].eq_ignore_ascii_case("name") {
                        return Ok(Some(parts[1].to_string()));
                    }
                }
            }

            Ok(None)
        }
    }
}

#[allow(dead_code)]
pub struct HostResolveResult {
    pub resolved_packages: BTreeMap<String, Vec<String>>, // package -> list of sonames
    pub unresolved_libs: Vec<MissingLib>,
}

/// Resolves missing libraries using the native host package manager.
pub fn resolve_libraries_with_host(
    pm: HostPackageManagerKind,
    missing_libs: &[MissingLib],
) -> Result<HostResolveResult, String> {
    let mut resolved_packages: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut unresolved_libs = Vec::new();

    for lib in missing_libs {
        match query_host_for_library(pm, lib) {
            Ok(Some(pkg)) => {
                resolved_packages
                    .entry(pkg)
                    .or_default()
                    .push(lib.soname.clone());
            }
            Ok(None) => {
                unresolved_libs.push(lib.clone());
            }
            Err(e) => {
                println!("{} {}", apt_prefix("WARN"), e);
                unresolved_libs.push(lib.clone());
            }
        }
    }

    Ok(HostResolveResult {
        resolved_packages,
        unresolved_libs,
    })
}

/// Prompts user and installs resolved packages through the host package manager.
pub fn prompt_and_install_host_packages(
    pm: HostPackageManagerKind,
    resolved_packages: &BTreeMap<String, Vec<String>>,
    auto_yes: bool,
) -> Result<bool, String> {
    if resolved_packages.is_empty() {
        return Ok(true);
    }

    println!(
        "\n{} {}",
        c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
        c(
            &format!(
                "Host packages required to satisfy shared library dependencies ({})",
                pm.name()
            ),
            &[Colors::BOLD, Colors::BRIGHT_WHITE]
        )
    );

    let headers = ["SHARED LIBRARY", "RESOLVED HOST PACKAGE"];
    let mut rows = Vec::new();

    for (pkg, libs) in resolved_packages {
        for lib in libs {
            rows.push(vec![
                lib.clone(),
                format!(
                    "{} ({})",
                    c(pkg, &[Colors::BOLD, Colors::BRIGHT_CYAN]),
                    pm.name()
                ),
            ]);
        }
    }

    print_table(&headers, &rows);

    let unique_pkgs: Vec<String> = resolved_packages.keys().cloned().collect();
    let cmd_vec = pm.build_install_command(&unique_pkgs);
    let cmd_str = cmd_vec.join(" ");

    println!(
        "\n    Recommended command:\n    {}",
        c(&cmd_str, &[Colors::BOLD, Colors::BRIGHT_YELLOW])
    );

    if !auto_yes && !confirm_dialog("Proceed with host dependency installation?", true) {
        println!(
            "{} Host dependency installation skipped.",
            c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE])
        );
        return Ok(false);
    }

    println!(
        "{} Executing: {}",
        c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
        cmd_str
    );
    let mut child = Command::new(&cmd_vec[0])
        .args(&cmd_vec[1..])
        .spawn()
        .map_err(|e| format!("Failed to spawn {}: {}", cmd_vec[0], e))?;

    let status = child
        .wait()
        .map_err(|e| format!("Failed to wait on installation process: {}", e))?;

    if status.success() {
        println!(
            "{} Host dependencies installed successfully.",
            apt_prefix("OK")
        );
        Ok(true)
    } else {
        Err(format!(
            "Host dependency installation failed with exit code: {:?}",
            status.code()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select_pacman_candidate_arch_filtering() {
        let candidates = vec![
            (
                "extra".to_string(),
                "libayatana-indicator".to_string(),
                "usr/lib/libayatana-indicator3.so.7".to_string(),
            ),
            (
                "multilib".to_string(),
                "lib32-libayatana-indicator".to_string(),
                "usr/lib32/libayatana-indicator3.so.7".to_string(),
            ),
        ];

        // 64-bit ELF must select 64-bit libayatana-indicator
        let chosen_64 = select_pacman_candidate(&candidates, ElfClass::Elf64);
        assert_eq!(chosen_64.as_deref(), Some("libayatana-indicator"));

        // 32-bit ELF must select lib32-libayatana-indicator
        let chosen_32 = select_pacman_candidate(&candidates, ElfClass::Elf32);
        assert_eq!(chosen_32.as_deref(), Some("lib32-libayatana-indicator"));
    }

    #[test]
    fn test_parse_pacman_machinereadable() {
        let raw = b"extra\x00libayatana-indicator\x000.9.4-1\x00usr/lib/libayatana-indicator3.so.7\nmultilib\x00lib32-libayatana-indicator\x000.9.4-1\x00usr/lib32/libayatana-indicator3.so.7\n";
        let chosen = parse_pacman_machinereadable(raw, ElfClass::Elf64);
        assert_eq!(chosen.as_deref(), Some("libayatana-indicator"));

        let chosen_32 = parse_pacman_machinereadable(raw, ElfClass::Elf32);
        assert_eq!(chosen_32.as_deref(), Some("lib32-libayatana-indicator"));
    }

    #[test]
    fn test_build_install_command() {
        let pkgs = vec!["libfoo".to_string(), "libbar".to_string()];
        let pacman_cmd = HostPackageManagerKind::Pacman.build_install_command(&pkgs);
        assert_eq!(
            pacman_cmd,
            vec!["sudo", "pacman", "-S", "--needed", "libfoo", "libbar"]
        );

        let apt_cmd = HostPackageManagerKind::Apt.build_install_command(&pkgs);
        assert_eq!(
            apt_cmd,
            vec!["sudo", "apt-get", "install", "libfoo", "libbar"]
        );
    }
}
