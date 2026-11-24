use crate::config::Config;
use crate::db::Database;
use crate::ui::{apt_prefix, c, Colors};
use serde::Serialize;
use std::collections::BTreeMap;
use std::process::Command;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolCheck {
    pub name: String,
    pub description: String,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FuseStatus {
    pub fusermount_available: bool,
    pub libfuse2_available: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DoctorReport {
    pub path_in_env: bool,
    pub bin_dir: String,
    pub archive_tools: Vec<ToolCheck>,
    pub fuse: FuseStatus,
    pub bwrap_available: bool,
    pub bwrap_functional: bool,
    pub bwrap_error: Option<String>,
    pub desktop_tools: Vec<ToolCheck>,
    pub installed_packages_count: usize,
    pub broken_launchers: Vec<String>,
    pub missing_libraries: BTreeMap<String, Vec<String>>,
    pub total_disk_bytes: u64,
}

pub struct Doctor {
    pub config: Config,
    pub db: Database,
}

impl Doctor {
    pub fn new(config: &Config) -> Result<Self, String> {
        let db = Database::load(&config.db_file)?;
        Ok(Self {
            config: config.clone(),
            db,
        })
    }

    pub fn collect_report(&self) -> DoctorReport {
        let path_env = std::env::var("PATH").unwrap_or_default();
        let paths: Vec<&str> = path_env.split(':').collect();
        let bin_str = self.config.bin_dir.to_string_lossy().to_string();
        let path_in_env = paths.contains(&bin_str.as_str());

        let archive_tools = [
            ("tar", "Tar archive extraction (required)"),
            ("xz", "XZ decompression (.tar.xz)"),
            ("zstd", "Zstandard decompression (.tar.zst)"),
        ]
        .into_iter()
        .map(|(tool, desc)| ToolCheck {
            name: tool.to_string(),
            description: desc.to_string(),
            available: which::which(tool).is_ok(),
        })
        .collect();

        let fusermount_available =
            which::which("fusermount").is_ok() || which::which("fusermount3").is_ok();
        let libfuse2_available = Self::detect_libfuse2();

        let bwrap_available = which::which("bwrap").is_ok();
        let (bwrap_functional, bwrap_error) = if bwrap_available {
            Self::probe_bwrap_runtime()
        } else {
            (false, None)
        };

        let desktop_tools = [
            ("update-desktop-database", "Desktop database indexing"),
            ("gtk-update-icon-cache", "Icon theme cache updating"),
            ("xdg-desktop-menu", "XDG desktop menu integration"),
        ]
        .into_iter()
        .map(|(tool, desc)| ToolCheck {
            name: tool.to_string(),
            description: desc.to_string(),
            available: which::which(tool).is_ok(),
        })
        .collect();

        let installed = self.db.all();
        let installed_packages_count = installed.len();

        let mut broken_launchers = Vec::new();
        if self.config.bin_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&self.config.bin_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_symlink() && !p.exists() {
                        broken_launchers.push(
                            p.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string(),
                        );
                    }
                }
            }
        }
        broken_launchers.sort();

        let mut missing_libraries = BTreeMap::new();
        for pkg in &installed {
            if pkg.format == "appimage" {
                continue;
            }
            if pkg.install_dir.exists() {
                let scan = crate::deps::scan_app_dependencies(&pkg.install_dir);
                if !scan.missing_libs.is_empty() {
                    let libs = scan.missing_libs.into_iter().map(|m| m.soname).collect();
                    missing_libraries.insert(pkg.id.clone(), libs);
                }
            }
        }

        let mut total_disk_bytes: u64 = 0;
        if self.config.apps_dir.exists() {
            for entry in walkdir::WalkDir::new(&self.config.apps_dir)
                .into_iter()
                .flatten()
            {
                if entry.file_type().is_file() {
                    if let Ok(meta) = entry.metadata() {
                        total_disk_bytes += meta.len();
                    }
                }
            }
        }

        DoctorReport {
            path_in_env,
            bin_dir: bin_str,
            archive_tools,
            fuse: FuseStatus {
                fusermount_available,
                libfuse2_available,
            },
            bwrap_available,
            bwrap_functional,
            bwrap_error,
            desktop_tools,
            installed_packages_count,
            broken_launchers,
            missing_libraries,
            total_disk_bytes,
        }
    }

    fn probe_bwrap_runtime() -> (bool, Option<String>) {
        match Command::new("bwrap")
            .args(["--ro-bind", "/", "/", "true"])
            .output()
        {
            Ok(out) if out.status.success() => (true, None),
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                let lower = stderr.to_lowercase();
                let hint = if lower.contains("apparmor")
                    || lower.contains("permission denied")
                    || lower.contains("operation not permitted")
                    || lower.contains("uid map")
                    || lower.contains("namespace")
                {
                    "Unprivileged user namespaces appear restricted (check kernel.apparmor_restrict_unprivileged_userns or kernel.unprivileged_userns_clone)."
                } else {
                    "bwrap probe 'bwrap --ro-bind / / true' exited with a non-zero status."
                };
                let msg = if stderr.is_empty() {
                    hint.to_string()
                } else {
                    format!("{} ({})", stderr, hint)
                };
                (false, Some(msg))
            }
            Err(e) => (false, Some(format!("Failed to execute bwrap: {}", e))),
        }
    }

    fn detect_libfuse2() -> bool {
        if let Ok(output) = Command::new("ldconfig").arg("-p").output() {
            let out_str = String::from_utf8_lossy(&output.stdout);
            if out_str.contains("libfuse.so.2") {
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
        std_paths.iter().any(|p| std::path::Path::new(p).exists())
    }

    pub fn run_all_checks(&self, as_json: bool) -> Result<(), String> {
        let report = self.collect_report();
        if as_json {
            let json_str = serde_json::to_string_pretty(&report)
                .map_err(|e| format!("Failed to serialize doctor report: {}", e))?;
            println!("{}", json_str);
            return Ok(());
        }

        println!(
            "{} {}\n",
            c("::", &[Colors::BOLD, Colors::CYAN]),
            c(
                "Running appmgr system health diagnostics...",
                &[Colors::BOLD, Colors::WHITE]
            )
        );

        if report.path_in_env {
            println!(
                " {} PATH includes {}",
                apt_prefix("OK"),
                c(&report.bin_dir, &[Colors::DIM])
            );
        } else {
            println!(
                " {} PATH does NOT include {}",
                apt_prefix("WARN"),
                c(&report.bin_dir, &[Colors::BOLD, Colors::YELLOW])
            );
            println!("        To fix, add this to your ~/.bashrc or ~/.zshrc:");
            println!(
                "        {}",
                c(
                    &format!("export PATH=\"{}:$PATH\"", report.bin_dir),
                    &[Colors::DIM]
                )
            );
        }

        for tool in &report.archive_tools {
            if tool.available {
                println!(
                    " {} {} found {}",
                    apt_prefix("OK"),
                    tool.name,
                    c(&format!("({})", tool.description), &[Colors::DIM])
                );
            } else {
                println!(
                    " {} {} missing {}",
                    apt_prefix("WARN"),
                    tool.name,
                    c(&format!("({})", tool.description), &[Colors::DIM])
                );
            }
        }

        if report.fuse.fusermount_available && report.fuse.libfuse2_available {
            println!(
                " {} FUSE runtime and libfuse.so.2 detected (AppImages run natively)",
                apt_prefix("OK")
            );
        } else if report.fuse.libfuse2_available {
            println!(
                " {} libfuse.so.2 detected (AppImages supported)",
                apt_prefix("OK")
            );
        } else {
            println!(
                " {} libfuse.so.2 not found (appmgr will use --appimage-extract-and-run)",
                apt_prefix("WARN")
            );
        }

        if report.bwrap_available && report.bwrap_functional {
            println!(
                " {} bubblewrap detected and functional (userns & sandbox isolation active)",
                apt_prefix("OK")
            );
        } else if report.bwrap_available {
            println!(
                " {} bubblewrap found, but runtime probe (bwrap --ro-bind / / true) failed",
                apt_prefix("WARN")
            );
            if let Some(ref err) = report.bwrap_error {
                println!("        {}", c(err, &[Colors::YELLOW]));
            }
        } else {
            println!(
                " {} bubblewrap not found (hardcoded /opt paths and sandbox profiles require bwrap)",
                apt_prefix("WARN")
            );
        }

        for tool in &report.desktop_tools {
            if tool.available {
                println!(
                    " {} {} found {}",
                    apt_prefix("OK"),
                    tool.name,
                    c(&format!("({})", tool.description), &[Colors::DIM])
                );
            } else {
                println!(
                    " {} {} missing {}",
                    apt_prefix("WARN"),
                    tool.name,
                    c(&format!("({})", tool.description), &[Colors::DIM])
                );
            }
        }

        println!(
            " {} Database tracks {} installed applications",
            apt_prefix("INFO"),
            c(
                &report.installed_packages_count.to_string(),
                &[Colors::BOLD, Colors::GREEN]
            )
        );

        if report.broken_launchers.is_empty() {
            println!(
                " {} No broken launchers found in {}",
                apt_prefix("OK"),
                c(&report.bin_dir, &[Colors::DIM])
            );
        } else {
            println!(
                " {} Found {} broken launchers in {}:",
                apt_prefix("WARN"),
                report.broken_launchers.len(),
                c(&report.bin_dir, &[Colors::DIM])
            );
            for b in &report.broken_launchers {
                println!("        - {}", b);
            }
        }

        if report.missing_libraries.is_empty() {
            println!(
                " {} All inspected application shared libraries are satisfied",
                apt_prefix("OK")
            );
        } else {
            let total_missing: usize = report.missing_libraries.values().map(|v| v.len()).sum();
            println!(
                " {} Found {} missing shared library references across {} installed application(s):",
                apt_prefix("WARN"),
                total_missing,
                report.missing_libraries.len()
            );
            for (app_id, libs) in report.missing_libraries.iter().take(3) {
                let preview = libs.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
                let extra = if libs.len() > 3 {
                    format!(" (+{} more)", libs.len() - 3)
                } else {
                    String::new()
                };
                println!(
                    "        - {}: {}{}",
                    c(app_id, &[Colors::BOLD, Colors::WHITE]),
                    c(&preview, &[Colors::RED]),
                    extra
                );
            }
            println!(
                "        Run {} to automatically diagnose and resolve them.",
                c("appmgr deps --all --fix", &[Colors::BOLD, Colors::CYAN])
            );
        }

        let mb = report.total_disk_bytes as f64 / (1024.0 * 1024.0);
        println!(
            " {} Applications disk usage: {}",
            apt_prefix("INFO"),
            c(&format!("{:.1} MiB", mb), &[Colors::DIM])
        );

        println!(
            "\n{} {}\n",
            c("::", &[Colors::BOLD, Colors::GREEN]),
            c("Diagnostics completed.", &[Colors::BOLD, Colors::WHITE])
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use tempfile::tempdir;

    #[test]
    fn test_doctor_report_json_serialization() {
        let dir = tempdir().unwrap();
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
        cfg.ensure_dirs().unwrap();
        let doc = Doctor::new(&cfg).unwrap();
        let report = doc.collect_report();
        let json_str = serde_json::to_string_pretty(&report).unwrap();
        assert!(!json_str.contains('\x1b'));
        let val: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(val["installed_packages_count"], 0);
        assert!(val["archive_tools"].is_array());
        assert!(val["fuse"]["libfuse2_available"].is_boolean());
        assert!(val["bwrap_available"].is_boolean());
        assert!(val["bwrap_functional"].is_boolean());
        assert!(val["missing_libraries"].is_object());
    }
}
