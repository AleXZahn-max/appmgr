use super::Core;
use crate::db::{PackageOrigin, PackageRecord, SandboxConfig};
use crate::launcher::write_package_launchers;
use crate::registry::{add_source, load_sources, sync_all_sources, SourceConfigEntry};
use crate::ui::{apt_prefix, c, confirm_dialog, Colors};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestApp {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_pattern: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub is_local: bool,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<PackageOrigin>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncManifest {
    #[serde(default)]
    pub appmgr_version: String,
    #[serde(default)]
    pub export_date: String,
    #[serde(default)]
    pub sources: Vec<SourceConfigEntry>,
    #[serde(default)]
    pub applications: Vec<ManifestApp>,
}

impl ManifestApp {
    pub fn from_record(p: &PackageRecord) -> Self {
        let origin = p.resolved_origin();
        let (source_type, registry_name, recipe_id, repo, tag, asset_pattern, is_local) =
            match &origin {
                PackageOrigin::GitHub {
                    repo,
                    tag,
                    asset_pattern,
                    ..
                } => (
                    Some("github".to_string()),
                    None,
                    None,
                    Some(repo.clone()),
                    tag.clone().or_else(|| p.tag.clone()),
                    asset_pattern.clone().or_else(|| p.asset_pattern.clone()),
                    false,
                ),
                PackageOrigin::Apt { source_id, package } => (
                    Some("apt".to_string()),
                    Some(source_id.clone()),
                    Some(package.clone()),
                    None,
                    p.tag.clone(),
                    None,
                    false,
                ),
                PackageOrigin::AppImageHub {
                    source_id,
                    app_id,
                    github_repo,
                    tag,
                    asset_pattern,
                    ..
                } => (
                    Some("appimagehub".to_string()),
                    Some(source_id.clone()),
                    Some(app_id.clone()),
                    github_repo.clone().or_else(|| p.repo.clone()),
                    tag.clone().or_else(|| p.tag.clone()),
                    asset_pattern.clone().or_else(|| p.asset_pattern.clone()),
                    false,
                ),
                PackageOrigin::Local { .. } => (
                    p.source_type.clone(),
                    p.registry_name.clone(),
                    p.recipe_id.clone(),
                    p.repo.clone(),
                    p.tag.clone(),
                    p.asset_pattern.clone(),
                    true,
                ),
            };

        let env: BTreeMap<String, String> =
            p.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

        Self {
            id: p.id.clone(),
            name: p.name.clone(),
            version: p.version.clone(),
            format: p.format.clone(),
            source_type,
            registry_name,
            recipe_id,
            repo,
            tag,
            asset_pattern,
            pinned: p.pinned,
            is_local,
            args: p.args.clone(),
            origin: Some(origin),
            env,
            sandbox: p.sandbox.clone(),
        }
    }

    pub fn resolved_origin(&self) -> PackageOrigin {
        if let Some(ref orig) = self.origin {
            if !matches!(orig, PackageOrigin::Local { .. }) {
                return orig.clone();
            }
        }
        if self.source_type.as_deref() == Some("apt") {
            if let (Some(src), Some(pkg)) = (&self.registry_name, &self.recipe_id) {
                return PackageOrigin::Apt {
                    source_id: src.clone(),
                    package: pkg.clone(),
                };
            }
        }
        if self.source_type.as_deref() == Some("appimagehub") {
            if let (Some(src), Some(app_id)) = (&self.registry_name, &self.recipe_id) {
                return PackageOrigin::AppImageHub {
                    source_id: src.clone(),
                    app_id: app_id.clone(),
                    github_repo: self.repo.clone(),
                    tag: self.tag.clone(),
                    etag: None,
                    asset_pattern: self.asset_pattern.clone(),
                    download_url: None,
                };
            }
        }
        if let Some(ref r) = self.repo {
            if !r.is_empty() {
                return PackageOrigin::GitHub {
                    repo: r.clone(),
                    tag: self.tag.clone(),
                    etag: None,
                    asset_pattern: self.asset_pattern.clone(),
                };
            }
        }
        PackageOrigin::Local { source_file: None }
    }

    fn install_target(&self, origin: &PackageOrigin) -> Option<String> {
        match origin {
            PackageOrigin::Apt { source_id, package } => Some(format!("{}:{}", source_id, package)),
            PackageOrigin::AppImageHub {
                source_id, app_id, ..
            } => Some(format!("{}:{}", source_id, app_id)),
            PackageOrigin::GitHub { repo, tag, .. } => {
                let effective_tag = tag.as_deref().or(self.tag.as_deref());
                match effective_tag {
                    Some(t) if !t.is_empty() => Some(format!("{}@{}", repo, t)),
                    _ => Some(repo.clone()),
                }
            }
            PackageOrigin::Local { .. } => {
                self.repo
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(|r| match self.tag.as_deref() {
                        Some(t) if !t.is_empty() => format!("{}@{}", r, t),
                        _ => r.to_string(),
                    })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum SourcePlanItem {
    Add(SourceConfigEntry),
    Update(SourceConfigEntry),
    Keep(String),
}

#[derive(Debug, Clone, PartialEq)]
enum AppPlanItem {
    Install {
        app: ManifestApp,
        target: String,
        origin: PackageOrigin,
    },
    Update {
        app: ManifestApp,
        changes: Vec<&'static str>,
    },
    Keep {
        id: String,
        version: String,
    },
    LocalManual {
        app: ManifestApp,
    },
}

fn sandbox_configs_differ(existing: &PackageRecord, desired: &Option<SandboxConfig>) -> bool {
    if existing.sandbox == *desired {
        return false;
    }
    match (&existing.sandbox, desired) {
        (None, Some(d)) => !d.is_active() && !existing.effective_sandbox().is_active(),
        (Some(e), None) => e.is_active(),
        _ => true,
    }
}

fn sources_differ(a: &SourceConfigEntry, b: &SourceConfigEntry) -> bool {
    a.driver != b.driver
        || a.url != b.url
        || a.enabled != b.enabled
        || a.priority != b.priority
        || a.suite != b.suite
        || a.components != b.components
}

fn parse_manifest_content(content: &str, path: &Path) -> Result<SyncManifest, String> {
    let is_toml = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"));

    if is_toml {
        toml::from_str::<SyncManifest>(content).map_err(|e| format!("Invalid manifest TOML: {}", e))
    } else {
        serde_json::from_str::<SyncManifest>(content).or_else(|json_err| {
            toml::from_str::<SyncManifest>(content)
                .map_err(|_| format!("Invalid manifest JSON: {}", json_err))
        })
    }
}

impl Core {
    pub fn build_sync_manifest(&self) -> SyncManifest {
        let mut sources: Vec<SourceConfigEntry> = load_sources(&self.config.sources_list_file)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.enabled)
            .collect();
        sources.sort_by(|a, b| a.id.cmp(&b.id));

        let mut applications: Vec<ManifestApp> = self
            .db
            .all()
            .into_iter()
            .map(ManifestApp::from_record)
            .collect();
        applications.sort_by(|a, b| a.id.cmp(&b.id));

        SyncManifest {
            appmgr_version: env!("CARGO_PKG_VERSION").to_string(),
            export_date: chrono::Local::now().to_rfc3339(),
            sources,
            applications,
        }
    }

    pub fn export_manifest(&self, output_path: Option<&str>) -> Result<bool, String> {
        let manifest = self.build_sync_manifest();
        let out_file = output_path
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("appmgr-manifest.json"));

        let is_toml = out_file
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("toml"));

        let content = if is_toml {
            toml::to_string_pretty(&manifest)
                .map_err(|e| format!("TOML serialization error: {}", e))?
        } else {
            serde_json::to_string_pretty(&manifest)
                .map_err(|e| format!("JSON serialization error: {}", e))?
                + "\n"
        };

        if let Some(parent) = out_file.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create manifest directory: {}", e))?;
            }
        }
        fs::write(&out_file, content).map_err(|e| format!("Failed to write manifest: {}", e))?;

        println!(
            "{} Exported {} source(s) and {} application(s) to: {}",
            c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
            manifest.sources.len(),
            manifest.applications.len(),
            c(
                &out_file.display().to_string(),
                &[Colors::BOLD, Colors::BRIGHT_YELLOW]
            )
        );
        Ok(true)
    }

    fn plan_manifest_sync(
        &self,
        manifest: &SyncManifest,
    ) -> (Vec<SourcePlanItem>, Vec<AppPlanItem>) {
        let existing_sources: HashMap<String, SourceConfigEntry> =
            load_sources(&self.config.sources_list_file)
                .unwrap_or_default()
                .into_iter()
                .map(|s| (s.id.clone(), s))
                .collect();

        let mut source_plan = Vec::new();
        for src in &manifest.sources {
            match existing_sources.get(&src.id) {
                None => source_plan.push(SourcePlanItem::Add(src.clone())),
                Some(cur) if sources_differ(cur, src) => {
                    source_plan.push(SourcePlanItem::Update(src.clone()))
                }
                Some(_) => source_plan.push(SourcePlanItem::Keep(src.id.clone())),
            }
        }

        let mut app_plan = Vec::new();
        for app in &manifest.applications {
            if let Some(existing) = self.db.get(&app.id) {
                let mut changes = Vec::new();
                let cur_env: BTreeMap<String, String> = existing
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if cur_env != app.env {
                    changes.push("env");
                }
                if existing.args != app.args {
                    changes.push("args");
                }
                if existing.pinned != app.pinned {
                    changes.push("pin");
                }
                if sandbox_configs_differ(existing, &app.sandbox) {
                    changes.push("sandbox");
                }
                if app.tag.is_some() && existing.tag != app.tag {
                    changes.push("tag");
                }

                if changes.is_empty() {
                    app_plan.push(AppPlanItem::Keep {
                        id: existing.id.clone(),
                        version: existing.version.clone(),
                    });
                } else {
                    app_plan.push(AppPlanItem::Update {
                        app: app.clone(),
                        changes,
                    });
                }
            } else {
                let origin = app.resolved_origin();
                if let Some(target) = app.install_target(&origin) {
                    app_plan.push(AppPlanItem::Install {
                        app: app.clone(),
                        target,
                        origin,
                    });
                } else {
                    app_plan.push(AppPlanItem::LocalManual { app: app.clone() });
                }
            }
        }

        (source_plan, app_plan)
    }

    fn print_sync_plan(source_plan: &[SourcePlanItem], app_plan: &[AppPlanItem]) {
        println!(
            "\n{} {}",
            c("::", &[Colors::BOLD, Colors::CYAN]),
            c("Manifest Sync Plan:", &[Colors::BOLD, Colors::WHITE])
        );

        for item in source_plan {
            match item {
                SourcePlanItem::Add(src) => println!(
                    "  {} {} {} {}",
                    c("+", &[Colors::BOLD, Colors::GREEN]),
                    c("source", &[Colors::BOLD, Colors::GREEN]),
                    c(&src.id, &[Colors::BOLD, Colors::WHITE]),
                    c(&format!("({}: {})", src.driver, src.url), &[Colors::DIM])
                ),
                SourcePlanItem::Update(src) => println!(
                    "  {} {} {} {}",
                    c("~", &[Colors::BOLD, Colors::YELLOW]),
                    c("source", &[Colors::BOLD, Colors::YELLOW]),
                    c(&src.id, &[Colors::BOLD, Colors::WHITE]),
                    c(&format!("({}: {})", src.driver, src.url), &[Colors::DIM])
                ),
                SourcePlanItem::Keep(id) => println!(
                    "  {} {} {}",
                    c("=", &[Colors::DIM]),
                    c("keep source", &[Colors::DIM]),
                    id
                ),
            }
        }

        for item in app_plan {
            match item {
                AppPlanItem::Install { app, target, .. } => println!(
                    "  {} {} {} {}",
                    c("+", &[Colors::BOLD, Colors::GREEN]),
                    c("install", &[Colors::BOLD, Colors::GREEN]),
                    c(&app.id, &[Colors::BOLD, Colors::WHITE]),
                    c(&format!("({})", target), &[Colors::DIM])
                ),
                AppPlanItem::Update { app, changes } => println!(
                    "  {} {} {} {}",
                    c("~", &[Colors::BOLD, Colors::YELLOW]),
                    c("update", &[Colors::BOLD, Colors::YELLOW]),
                    c(&app.id, &[Colors::BOLD, Colors::WHITE]),
                    c(&format!("({})", changes.join("/")), &[Colors::CYAN])
                ),
                AppPlanItem::Keep { id, version } => println!(
                    "  {} {} {} {}",
                    c("=", &[Colors::DIM]),
                    c("keep", &[Colors::DIM]),
                    id,
                    c(&format!("({})", version), &[Colors::DIM])
                ),
                AppPlanItem::LocalManual { app } => println!(
                    "  {} {} {} {}",
                    c("!", &[Colors::BOLD, Colors::MAGENTA]),
                    c("manual", &[Colors::BOLD, Colors::MAGENTA]),
                    app.id,
                    c("(local install required)", &[Colors::DIM])
                ),
            }
        }
        println!();
    }

    fn apply_manifest_app_metadata(
        &mut self,
        pkg_id: &str,
        app: &ManifestApp,
        restore_origin: Option<&PackageOrigin>,
    ) -> Result<(), String> {
        let mut need_launcher = false;
        let has_opt_deb = if let Some(rec) = self.db.get_mut(pkg_id) {
            rec.env = app
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            rec.args = app.args.clone();
            rec.pinned = app.pinned;

            if let Some(orig) = restore_origin {
                match orig {
                    PackageOrigin::AppImageHub {
                        source_id, app_id, ..
                    } => {
                        rec.set_origin(PackageOrigin::AppImageHub {
                            source_id: source_id.clone(),
                            app_id: app_id.clone(),
                            github_repo: app.repo.clone().or_else(|| rec.repo.clone()),
                            tag: app.tag.clone().or_else(|| rec.tag.clone()),
                            etag: rec.etag.clone(),
                            asset_pattern: app
                                .asset_pattern
                                .clone()
                                .or_else(|| rec.asset_pattern.clone()),
                            download_url: None,
                        });
                    }
                    PackageOrigin::GitHub {
                        repo,
                        tag,
                        asset_pattern,
                        ..
                    } => {
                        rec.set_origin(PackageOrigin::GitHub {
                            repo: repo.clone(),
                            tag: tag
                                .clone()
                                .or_else(|| app.tag.clone())
                                .or_else(|| rec.tag.clone()),
                            etag: rec.etag.clone(),
                            asset_pattern: asset_pattern
                                .clone()
                                .or_else(|| app.asset_pattern.clone())
                                .or_else(|| rec.asset_pattern.clone()),
                        });
                    }
                    PackageOrigin::Apt { source_id, package } => {
                        rec.set_origin(PackageOrigin::Apt {
                            source_id: source_id.clone(),
                            package: package.clone(),
                        });
                    }
                    PackageOrigin::Local { .. } => {}
                }
            }
            if let Some(ref pat) = app.asset_pattern {
                rec.asset_pattern = Some(pat.clone());
            }
            if let Some(ref t) = app.tag {
                rec.tag = Some(t.clone());
            }
            need_launcher = true;
            rec.install_dir.join("opt").exists()
                || rec.bind_opt.as_ref().is_some_and(|v| !v.is_empty())
        } else {
            false
        };

        if !need_launcher {
            return Ok(());
        }

        if let Some(ref sbx) = app.sandbox {
            self.apply_sandbox_config(pkg_id, sbx.clone())?;
        } else {
            let default_bwrap = if has_opt_deb { Some(true) } else { None };
            self.db
                .set_sandbox(pkg_id, None, default_bwrap)
                .map_err(|e| e.to_string())?;
            if let Some(updated) = self.db.get(pkg_id) {
                write_package_launchers(updated, &self.config.bin_dir)?;
            }
        }
        Ok(())
    }

    pub async fn import_manifest(
        &mut self,
        manifest_path: &str,
        diff_only: bool,
        apply_flag: bool,
        assume_yes: bool,
    ) -> Result<bool, String> {
        let mf_path = Path::new(manifest_path);
        let content = fs::read_to_string(mf_path)
            .map_err(|e| format!("Failed to read manifest {}: {}", manifest_path, e))?;

        let manifest = parse_manifest_content(&content, mf_path)?;
        let (source_plan, app_plan) = self.plan_manifest_sync(&manifest);

        let has_changes = source_plan
            .iter()
            .any(|s| !matches!(s, SourcePlanItem::Keep(_)))
            || app_plan
                .iter()
                .any(|a| matches!(a, AppPlanItem::Install { .. } | AppPlanItem::Update { .. }));

        if diff_only && !apply_flag {
            Self::print_sync_plan(&source_plan, &app_plan);
            return Ok(true);
        }

        if diff_only || !assume_yes {
            Self::print_sync_plan(&source_plan, &app_plan);
        }

        if !has_changes {
            println!(
                "{} Everything in manifest is already in sync. Nothing to do.",
                apt_prefix("OK")
            );
            return Ok(true);
        }

        if !assume_yes && !confirm_dialog("Apply manifest sync plan?", true) {
            println!(
                "{} Import cancelled by user.",
                c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE])
            );
            return Ok(false);
        }

        let mut sources_changed = false;
        let mut needs_index_sync = false;
        for item in &source_plan {
            match item {
                SourcePlanItem::Add(src) | SourcePlanItem::Update(src) => {
                    add_source(&self.config.sources_list_file, src.clone())?;
                    sources_changed = true;
                    if matches!(src.driver.as_str(), "apt" | "appimagehub" | "registry") {
                        needs_index_sync = true;
                    }
                    println!(
                        " {} Configured source '{}' [{}]",
                        apt_prefix("OK"),
                        src.id,
                        src.driver
                    );
                }
                SourcePlanItem::Keep(_) => {}
            }
        }

        if needs_index_sync
            || manifest.sources.iter().any(|s| {
                matches!(s.driver.as_str(), "apt" | "appimagehub")
                    && !self
                        .config
                        .indexes_cache_dir
                        .join(format!("{}.json", s.id))
                        .exists()
            })
        {
            sync_all_sources(
                &self.http_client,
                &self.config.sources_list_file,
                &self.config.registries_cache_dir,
                &self.config.indexes_cache_dir,
            )
            .await?;
        } else if sources_changed {
            // Online-only sources require no index download
        }

        let mut installed_count = 0usize;
        let mut updated_count = 0usize;
        let mut skipped_count = 0usize;
        let mut local_apps = Vec::new();

        for item in app_plan {
            match item {
                AppPlanItem::Install {
                    app,
                    target,
                    origin,
                } => {
                    println!(
                        "\n{} Installing {} from {} ...",
                        c("::", &[Colors::BOLD, Colors::BRIGHT_CYAN]),
                        app.name,
                        target
                    );
                    let prefer_fmt = if app.format.is_empty() {
                        None
                    } else {
                        Some(app.format.as_str())
                    };
                    let install_ok = if let PackageOrigin::AppImageHub {
                        source_id,
                        app_id,
                        github_repo: Some(repo),
                        tag,
                        ..
                    } = &origin
                    {
                        let eff_tag = tag.as_deref().or(app.tag.as_deref());
                        if let Some(t) = eff_tag {
                            let target_gh = format!("{}@{}", repo, t);
                            self.install(
                                &target_gh,
                                true,
                                true,
                                None,
                                Some("appimage"),
                                false,
                                false,
                                false,
                                false,
                                false,
                            )
                            .await?
                        } else {
                            let prefixed = format!("{}:{}", source_id, app_id);
                            self.install(
                                &prefixed, true, true, None, prefer_fmt, false, false, false,
                                false, false,
                            )
                            .await?
                        }
                    } else {
                        self.install(
                            &target, true, true, None, prefer_fmt, false, false, false, false,
                            false,
                        )
                        .await?
                    };

                    if install_ok {
                        let resolved_id = self
                            .db
                            .get(&app.id)
                            .map(|r| r.id.clone())
                            .unwrap_or_else(|| app.id.clone());
                        self.apply_manifest_app_metadata(&resolved_id, &app, Some(&origin))?;
                        installed_count += 1;
                    }
                }
                AppPlanItem::Update { app, changes } => {
                    self.apply_manifest_app_metadata(&app.id, &app, None)?;
                    println!(
                        " {} Updated {} ({})",
                        apt_prefix("OK"),
                        app.id,
                        changes.join(", ")
                    );
                    updated_count += 1;
                }
                AppPlanItem::Keep { .. } => {
                    skipped_count += 1;
                }
                AppPlanItem::LocalManual { app } => {
                    local_apps.push(app);
                }
            }
        }

        println!(
            "\n{} Manifest sync finished: {} installed, {} updated, {} kept.",
            c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
            installed_count,
            updated_count,
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
                    la.name,
                    la.version,
                    la.format.to_uppercase()
                );
            }
            println!();
        }

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::SandboxProfile;
    use tempfile::tempdir;

    #[test]
    fn test_manifest_export_and_diff_planning_json_and_toml() {
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

        add_source(
            &cfg.sources_list_file,
            SourceConfigEntry {
                driver: "apt".to_string(),
                id: "myapt".to_string(),
                url: "https://example.com/debian".to_string(),
                enabled: true,
                priority: 500,
                suite: Some("stable".to_string()),
                components: Some(vec!["main".to_string()]),
                mode: Some("indexed".to_string()),
                name: None,
                description: None,
            },
        )
        .unwrap();

        let mut core = Core::new(&cfg).unwrap();
        let mut rec = PackageRecord {
            id: "demo".to_string(),
            name: "demo".to_string(),
            version: "1.2.3".to_string(),
            format: "tar".to_string(),
            install_dir: cfg.apps_dir.join("demo"),
            pinned: true,
            args: vec!["--flag".to_string()],
            sandbox: Some(SandboxConfig {
                profile: SandboxProfile::IsolatedHome,
                no_net: true,
                home_dir: None,
            }),
            ..Default::default()
        };
        rec.env.insert("FOO".to_string(), "bar".to_string());
        rec.set_origin(PackageOrigin::GitHub {
            repo: "owner/demo".to_string(),
            tag: Some("v1.2.3".to_string()),
            etag: None,
            asset_pattern: Some("^demo-.*\\.tar\\.gz$".to_string()),
        });
        core.db.add(rec).unwrap();

        let json_path = dir.path().join("manifest.json");
        let toml_path = dir.path().join("manifest.toml");
        core.export_manifest(Some(&json_path.to_string_lossy()))
            .unwrap();
        core.export_manifest(Some(&toml_path.to_string_lossy()))
            .unwrap();

        let parsed_json =
            parse_manifest_content(&fs::read_to_string(&json_path).unwrap(), &json_path).unwrap();
        let parsed_toml =
            parse_manifest_content(&fs::read_to_string(&toml_path).unwrap(), &toml_path).unwrap();
        assert_eq!(parsed_json.sources.len(), 2);
        assert!(parsed_json.sources.iter().any(|s| s.id == "myapt"));
        assert_eq!(parsed_json.sources, parsed_toml.sources);
        assert_eq!(parsed_json.applications.len(), 1);
        assert_eq!(parsed_json.applications[0], parsed_toml.applications[0]);
        assert_eq!(
            parsed_json.applications[0].sandbox,
            Some(SandboxConfig {
                profile: SandboxProfile::IsolatedHome,
                no_net: true,
                home_dir: None,
            })
        );

        // Diff on identical state -> all Keep
        let (src_keep, app_keep) = core.plan_manifest_sync(&parsed_json);
        assert!(matches!(src_keep[0], SourcePlanItem::Keep(_)));
        assert!(matches!(app_keep[0], AppPlanItem::Keep { .. }));

        // Modify installed env + unpin -> plan detects ~ update (env, pin)
        core.db.set_pinned("demo", false).unwrap();
        let (src_keep2, app_upd) = core.plan_manifest_sync(&parsed_json);
        assert!(matches!(src_keep2[0], SourcePlanItem::Keep(_)));
        match &app_upd[0] {
            AppPlanItem::Update { changes, .. } => {
                assert!(changes.contains(&"pin"));
            }
            other => panic!("Expected Update plan item, got {:?}", other),
        }
    }
}
