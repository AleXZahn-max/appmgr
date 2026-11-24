use crate::github::GitHubClient;
use crate::ui::{apt_prefix, c, confirm_dialog, Colors, Spinner};
use reqwest::StatusCode;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

pub struct SelfUpdater {
    pub client: GitHubClient,
}

impl Default for SelfUpdater {
    fn default() -> Self {
        Self::new()
    }
}

impl SelfUpdater {
    pub const REPO: &'static str = "AleXZahn-max/appmgr";
    pub const CURRENT_VERSION: &'static str = env!("CARGO_PKG_VERSION");

    pub fn new() -> Self {
        Self {
            client: GitHubClient::new(),
        }
    }

    /// Quietly checks GitHub Releases for a newer appmgr version. Returns `Some(version)` if newer.
    pub async fn check_latest_version(&self, include_pre: bool) -> Option<String> {
        let (status, release_opt, _, _) = self
            .client
            .get_release(Self::REPO, None, include_pre, None)
            .await
            .ok()?;
        if status != StatusCode::OK {
            return None;
        }
        let release = release_opt?;
        let latest = release.tag_name.trim_start_matches('v');
        if GitHubClient::compare_versions(latest, Self::CURRENT_VERSION)
            == std::cmp::Ordering::Greater
        {
            Some(latest.to_string())
        } else {
            None
        }
    }

    pub async fn run(
        &self,
        check_only: bool,
        include_pre: bool,
        assume_yes: bool,
    ) -> Result<bool, String> {
        println!(
            "{} Checking for appmgr updates from {}...",
            c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
            c(Self::REPO, &[Colors::BOLD, Colors::BRIGHT_CYAN])
        );

        let spinner = Spinner::new("Querying GitHub Releases for appmgr updates...");
        let (status, release_opt, _, _) = self
            .client
            .get_release(Self::REPO, None, include_pre, None)
            .await?;
        spinner.stop();

        if status != StatusCode::OK {
            println!(
                " {} No release found for {}",
                apt_prefix("WARN"),
                Self::REPO
            );
            return Ok(false);
        }

        let release = match release_opt {
            Some(r) => r,
            None => {
                println!(" {} No release data returned.", apt_prefix("WARN"));
                return Ok(false);
            }
        };

        let latest_tag = release.tag_name.trim_start_matches('v');
        let current_ver = Self::CURRENT_VERSION;

        println!(
            "  Current version: {}",
            c(current_ver, &[Colors::BOLD, Colors::BRIGHT_WHITE])
        );
        println!(
            "  Latest version:  {}",
            c(latest_tag, &[Colors::BOLD, Colors::BRIGHT_GREEN])
        );

        if GitHubClient::compare_versions(latest_tag, current_ver) != std::cmp::Ordering::Greater {
            println!(
                "\n{} appmgr is already up to date ({})\n",
                apt_prefix("OK"),
                current_ver
            );
            return Ok(true);
        }

        if check_only {
            println!(
                "\n{} A newer version of appmgr is available: {} -> {}\n",
                c("::", &[Colors::BOLD, Colors::BRIGHT_YELLOW]),
                current_ver,
                c(latest_tag, &[Colors::BOLD, Colors::BRIGHT_GREEN])
            );
            return Ok(true);
        }

        let current_exe = std::env::current_exe()
            .map_err(|e| format!("Failed to determine current executable path: {}", e))?;

        let pacman_check = Command::new("pacman")
            .args(["-Qo", &current_exe.to_string_lossy()])
            .output();

        if let Ok(output) = pacman_check {
            if output.status.success() {
                let stdout_str = String::from_utf8_lossy(&output.stdout);
                println!(
                    "\n{} appmgr is managed by the system package manager (pacman):",
                    apt_prefix("WARN")
                );
                println!(
                    "        {}",
                    c(stdout_str.trim(), &[Colors::DIM, Colors::WHITE])
                );
                println!(
                    "        Automatic binary replacement is disabled to prevent database divergence."
                );
                println!("        To update, please use:");
                println!(
                    "        {}",
                    c(
                        "cd appmgr && makepkg -si",
                        &[Colors::BOLD, Colors::BRIGHT_YELLOW]
                    )
                );
                println!();
                return Ok(true);
            }
        }

        if !assume_yes
            && !confirm_dialog(&format!("Update appmgr to version {}?", latest_tag), true)
        {
            println!(
                "{} Update cancelled by user.",
                c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE])
            );
            return Ok(false);
        }

        let host_arch = std::env::consts::ARCH;
        let candidates =
            self.client
                .filter_assets(&release.assets, Some("bin"), None, Some("appmgr"));

        let chosen = candidates.iter().find(|c| {
            let lower = c.name.to_lowercase();
            if host_arch == "x86_64" {
                (lower.contains("x86_64") || lower.contains("amd64")) && !lower.contains("aarch64")
            } else if host_arch == "aarch64" {
                lower.contains("aarch64") || lower.contains("arm64")
            } else {
                false
            }
        });

        let chosen_asset = match chosen {
            Some(a) => a,
            None => {
                return Err(format!(
                    "No matching appmgr binary asset found in release for architecture {}",
                    host_arch
                ));
            }
        };

        println!(
            "\n{} Downloading new binary: {} ...",
            c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
            c(&chosen_asset.name, &[Colors::BOLD, Colors::BRIGHT_CYAN])
        );

        let temp_dir = tempfile::tempdir()
            .map_err(|e| format!("Failed to create temporary directory: {}", e))?;
        let temp_new_bin = temp_dir.path().join("appmgr.new");

        self.client
            .download_asset(
                &chosen_asset.url,
                &temp_new_bin,
                chosen_asset.size,
                &chosen_asset.name,
                Some(chosen_asset),
                Some(&release.assets),
            )
            .await?;

        let parent_dir = current_exe.parent().ok_or_else(|| {
            "Failed to determine parent directory of current executable".to_string()
        })?;

        let pid = std::process::id();
        let staged_bin = parent_dir.join(format!(".appmgr.new.{}", pid));
        let backup_bin = parent_dir.join(format!(".appmgr.bak.{}", pid));

        if let Err(e) = fs::copy(&temp_new_bin, &staged_bin) {
            let _ = fs::remove_file(&staged_bin);
            return Err(format!(
                "Failed to stage new binary in {}: {}",
                parent_dir.display(),
                e
            ));
        }

        if let Err(e) = fs::set_permissions(&staged_bin, fs::Permissions::from_mode(0o755)) {
            let _ = fs::remove_file(&staged_bin);
            return Err(format!("Failed to set permissions on staged binary: {}", e));
        }

        if let Err(e) = fs::rename(&current_exe, &backup_bin) {
            let _ = fs::remove_file(&staged_bin);
            return Err(format!("Failed to backup current executable: {}", e));
        }

        if let Err(e) = fs::rename(&staged_bin, &current_exe) {
            let _ = fs::rename(&backup_bin, &current_exe);
            let _ = fs::remove_file(&staged_bin);
            let _ = fs::remove_file(&backup_bin);
            return Err(format!(
                "Failed to install new executable (rolled back): {}",
                e
            ));
        }

        let _ = fs::remove_file(&backup_bin);
        let _ = fs::remove_file(&staged_bin);

        println!(
            "\n{} Successfully updated appmgr to {}! ({})\n",
            c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
            c(latest_tag, &[Colors::BOLD, Colors::BRIGHT_GREEN]),
            current_exe.display()
        );

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_atomic_binary_replacement() {
        let dir = tempdir().unwrap();
        let current_exe = dir.path().join("appmgr");
        fs::write(&current_exe, b"old_version_binary").unwrap();

        let external_dir = tempdir().unwrap();
        let downloaded = external_dir.path().join("downloaded.bin");
        fs::write(&downloaded, b"new_version_binary").unwrap();

        let parent_dir = current_exe.parent().unwrap();
        let pid = std::process::id();
        let staged_bin = parent_dir.join(format!(".appmgr.new.{}", pid));
        let backup_bin = parent_dir.join(format!(".appmgr.bak.{}", pid));

        fs::copy(&downloaded, &staged_bin).unwrap();
        fs::set_permissions(&staged_bin, fs::Permissions::from_mode(0o755)).unwrap();

        fs::rename(&current_exe, &backup_bin).unwrap();
        fs::rename(&staged_bin, &current_exe).unwrap();
        fs::remove_file(&backup_bin).unwrap();

        assert_eq!(fs::read(&current_exe).unwrap(), b"new_version_binary");
        assert!(!staged_bin.exists());
        assert!(!backup_bin.exists());
    }
}
