use super::Core;
use crate::db::PackageOrigin;
use crate::ui::{apt_prefix, c, Colors};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

impl Core {
    pub fn export_manifest(&self, output_path: Option<&str>) -> Result<bool, String> {
        let all_pkgs = self.db.all();
        let manifest_data = json!({
            "appmgr_version": env!("CARGO_PKG_VERSION"),
            "export_date": chrono::Local::now().to_rfc3339(),
            "applications": all_pkgs.iter().map(|p| {
                let origin = p.resolved_origin();
                json!({
                    "id": p.id,
                    "name": p.name,
                    "version": p.version,
                    "format": p.format,
                    "origin": origin,
                    "repo": p.repo,
                    "pinned": p.pinned,
                    "env": p.env,
                    "args": p.args,
                    "is_local": matches!(origin, PackageOrigin::Local { .. }),
                })
            }).collect::<Vec<_>>()
        });

        let out_file = output_path
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("appmgr-manifest.json"));

        let content = serde_json::to_string_pretty(&manifest_data)
            .map_err(|e| format!("Serialization error: {}", e))?;
        fs::write(&out_file, content + "\n")
            .map_err(|e| format!("Failed to write manifest: {}", e))?;

        println!(
            "{} Exported {} applications to: {}",
            c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
            all_pkgs.len(),
            c(
                &out_file.display().to_string(),
                &[Colors::BOLD, Colors::BRIGHT_YELLOW]
            )
        );
        Ok(true)
    }

    pub async fn import_manifest(
        &mut self,
        manifest_path: &str,
        assume_yes: bool,
    ) -> Result<bool, String> {
        let mf_path = Path::new(manifest_path);
        let content = fs::read_to_string(mf_path)
            .map_err(|e| format!("Failed to read manifest {}: {}", manifest_path, e))?;

        let data: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| format!("Invalid manifest JSON: {}", e))?;

        let apps = data["applications"]
            .as_array()
            .ok_or_else(|| "Manifest contains no 'applications' array".to_string())?;

        println!(
            "{} Importing {} applications from {}...",
            c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
            apps.len(),
            mf_path.file_name().unwrap_or_default().to_string_lossy()
        );

        let mut installed_count = 0;
        let mut skipped_count = 0;
        let mut local_apps = Vec::new();

        for app in apps {
            let id = app["id"].as_str().unwrap_or_default();
            let name = app["name"].as_str().unwrap_or(id);

            if self.db.get(id).is_some() {
                println!(
                    " {} {} is already installed, skipping.",
                    apt_prefix("OK"),
                    name
                );
                skipped_count += 1;
                continue;
            }

            let origin = serde_json::from_value::<PackageOrigin>(app["origin"].clone()).ok();
            let target = match origin {
                Some(PackageOrigin::Apt { source_id, package }) => {
                    Some(format!("{}:{}", source_id, package))
                }
                Some(PackageOrigin::AppImageHub {
                    source_id, app_id, ..
                }) => Some(format!("{}:{}", source_id, app_id)),
                Some(PackageOrigin::GitHub { repo, .. }) => Some(repo),
                _ => app["repo"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(String::from),
            };

            let Some(target_str) = target else {
                local_apps.push(app);
                continue;
            };

            println!(
                "\n{} Installing {} from {} ...",
                c("::", &[Colors::BOLD, Colors::BRIGHT_CYAN]),
                name,
                target_str
            );

            if self
                .install(
                    &target_str,
                    true,
                    assume_yes,
                    None,
                    None,
                    false,
                    false,
                    false,
                    false,
                )
                .await?
            {
                installed_count += 1;
            }
        }

        println!(
            "\n{} Manifest import finished: {} installed, {} skipped.",
            c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
            installed_count,
            skipped_count
        );

        if !local_apps.is_empty() {
            println!(
                "\n{} The following applications were originally installed from local files and must be installed manually:",
                apt_prefix("INFO")
            );
            for la in local_apps {
                println!(
                    "        - {} ({}) [{}]",
                    la["name"].as_str().unwrap_or(""),
                    la["version"].as_str().unwrap_or(""),
                    la["format"].as_str().unwrap_or("").to_uppercase()
                );
            }
            println!();
        }

        Ok(true)
    }
}
