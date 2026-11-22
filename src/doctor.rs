use crate::config::Config;
use crate::db::Database;
use crate::ui::{apt_prefix, c, Colors};
use std::process::Command;

pub struct Doctor {
    pub config: Config,
    pub db: Database,
}

impl Doctor {
    pub fn new(config: &Config) -> Self {
        let db = Database::load(&config.db_file);
        Self {
            config: config.clone(),
            db,
        }
    }

    pub fn run_all_checks(&self) {
        println!(
            "{} {}\n",
            c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
            c(
                "Running appmgr system health diagnostics...",
                &[Colors::BOLD, Colors::BRIGHT_WHITE]
            )
        );

        self.check_path_environment();
        self.check_archive_utilities();
        self.check_fuse_support();
        self.check_namespace_virtualization();
        self.check_desktop_utilities();
        self.check_database_integrity();
        self.check_broken_launchers();
        self.check_shared_libraries();
        self.report_disk_usage();

        println!(
            "\n{} {}\n",
            c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
            c("Diagnostics completed.", &[Colors::BOLD, Colors::BRIGHT_WHITE])
        );
    }

    pub fn check_path_environment(&self) {
        let path_env = std::env::var("PATH").unwrap_or_default();
        let paths: Vec<&str> = path_env.split(':').collect();
        let bin_str = self.config.bin_dir.to_string_lossy();

        if paths.contains(&bin_str.as_ref()) {
            println!(
                " {} PATH includes {}",
                apt_prefix("OK"),
                c(&bin_str, &[Colors::BOLD, Colors::BRIGHT_CYAN])
            );
        } else {
            println!(
                " {} PATH does NOT include {}",
                apt_prefix("WARN"),
                c(&bin_str, &[Colors::BOLD, Colors::BRIGHT_YELLOW])
            );
            println!("        To fix, add this to your ~/.bashrc or ~/.zshrc:");
            println!(
                "        {}",
                c(
                    &format!("export PATH=\"{}:$PATH\"", bin_str),
                    &[Colors::DIM, Colors::WHITE]
                )
            );
        }
    }

    pub fn check_archive_utilities(&self) {
        let tools = [
            ("tar", "Tar archive extraction (required)"),
            ("xz", "XZ decompression (.tar.xz)"),
            ("zstd", "Zstandard decompression (.tar.zst)"),
        ];
        for (tool, desc) in tools {
            if which::which(tool).is_ok() {
                println!(" {} {} found ({})", apt_prefix("OK"), tool, desc);
            } else {
                println!(" {} {} missing ({})", apt_prefix("WARN"), tool, desc);
            }
        }
    }

    pub fn check_fuse_support(&self) {
        let has_fusermount =
            which::which("fusermount").is_ok() || which::which("fusermount3").is_ok();
        let mut has_libfuse2 = false;

        if let Ok(output) = Command::new("ldconfig").arg("-p").output() {
            let out_str = String::from_utf8_lossy(&output.stdout);
            if out_str.contains("libfuse.so.2") {
                has_libfuse2 = true;
            }
        }

        if has_fusermount && has_libfuse2 {
            println!(
                " {} FUSE runtime and libfuse.so.2 detected (AppImages run natively)",
                apt_prefix("OK")
            );
        } else if has_libfuse2 {
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
    }

    pub fn check_namespace_virtualization(&self) {
        if which::which("bwrap").is_ok() {
            println!(
                " {} bubblewrap detected (transparent /opt path virtualization active)",
                apt_prefix("OK")
            );
        } else {
            println!(
                " {} bubblewrap not found (hardcoded /opt paths in heavy .deb might require sudo)",
                apt_prefix("WARN")
            );
        }
    }

    pub fn check_desktop_utilities(&self) {
        let tools = [
            ("update-desktop-database", "Desktop database indexing"),
            ("gtk-update-icon-cache", "Icon theme cache updating"),
            ("xdg-desktop-menu", "XDG desktop menu integration"),
        ];
        for (tool, desc) in tools {
            if which::which(tool).is_ok() {
                println!(" {} {} found ({})", apt_prefix("OK"), tool, desc);
            } else {
                println!(" {} {} missing ({})", apt_prefix("WARN"), tool, desc);
            }
        }
    }

    pub fn check_database_integrity(&self) {
        let count = self.db.all().len();
        println!(
            " {} Database tracks {} installed applications",
            apt_prefix("INFO"),
            c(&count.to_string(), &[Colors::BOLD, Colors::BRIGHT_GREEN])
        );
    }

    pub fn check_broken_launchers(&self) {
        let mut broken = Vec::new();
        if self.config.bin_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&self.config.bin_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_symlink() && !p.exists() {
                        broken.push(p);
                    }
                }
            }
        }

        if broken.is_empty() {
            println!(
                " {} No broken launchers found in {}",
                apt_prefix("OK"),
                self.config.bin_dir.display()
            );
        } else {
            println!(
                " {} Found {} broken launchers in {}:",
                apt_prefix("WARN"),
                broken.len(),
                self.config.bin_dir.display()
            );
            for b in broken {
                println!("        - {}", b.file_name().unwrap_or_default().to_string_lossy());
            }
        }
    }

    pub fn check_shared_libraries(&self) {
        let installed = self.db.all();
        let mut missing_map: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();

        for pkg in &installed {
            if pkg.format == "appimage" {
                continue;
            }
            if pkg.install_dir.exists() {
                let scan = crate::deps::scan_app_dependencies(&pkg.install_dir);
                if !scan.missing_libs.is_empty() {
                    let libs = scan.missing_libs.into_iter().map(|m| m.soname).collect();
                    missing_map.insert(pkg.id.clone(), libs);
                }
            }
        }

        if missing_map.is_empty() {
            println!(
                " {} All inspected application shared libraries are satisfied",
                apt_prefix("OK")
            );
        } else {
            let total_missing: usize = missing_map.values().map(|v| v.len()).sum();
            println!(
                " {} Found {} missing shared library references across {} installed application(s):",
                apt_prefix("WARN"),
                total_missing,
                missing_map.len()
            );
            for (app_id, libs) in missing_map.iter().take(3) {
                let preview = libs.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
                let extra = if libs.len() > 3 {
                    format!(" (+{} more)", libs.len() - 3)
                } else {
                    String::new()
                };
                println!(
                    "        - {}: {}{}",
                    c(app_id, &[Colors::BOLD, Colors::BRIGHT_WHITE]),
                    c(&preview, &[Colors::RED]),
                    extra
                );
            }
            println!(
                "        Run {} to automatically diagnose and resolve them.",
                c("appmgr deps --all --fix", &[Colors::BOLD, Colors::BRIGHT_CYAN])
            );
        }
    }

    pub fn report_disk_usage(&self) {
        let mut total_bytes: u64 = 0;
        if self.config.apps_dir.exists() {
            for entry in walkdir::WalkDir::new(&self.config.apps_dir).into_iter().flatten() {
                if entry.file_type().is_file() {
                    if let Ok(meta) = entry.metadata() {
                        total_bytes += meta.len();
                    }
                }
            }
        }
        let mb = total_bytes as f64 / (1024.0 * 1024.0);
        println!(
            " {} Applications disk usage: {}",
            apt_prefix("INFO"),
            c(
                &format!("{:.1} MB", mb),
                &[Colors::BOLD, Colors::BRIGHT_GREEN]
            )
        );
    }
}
