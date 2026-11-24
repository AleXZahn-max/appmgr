use super::Core;
use crate::config::DepsMode;
use crate::db::{PackageOrigin, PackageRecord};
use crate::desktop::{create_desktop_entry, install_best_icon, run_system_triggers};
use crate::drivers::appimagehub::get_cached_appimagehub_packages;
use crate::drivers::apt::{compare_apt_versions, get_cached_apt_packages, AptPackage};
use crate::formats::archive::ArchivePackage;
use crate::formats::StagingDirGuard;
use crate::github::{CandidateAsset, GitHubClient, ReleaseAsset};
use crate::launcher::write_package_launchers;
use crate::registry::SourceConfigEntry;
use crate::ui::{apt_prefix, c, confirm_dialog, Colors, Spinner};
use reqwest::StatusCode;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
enum UpgradeAction {
    GitHub {
        pkg: PackageRecord,
        repo: String,
        new_tag: String,
        new_etag: Option<String>,
        asset: CandidateAsset,
        release_assets: Vec<ReleaseAsset>,
        pattern: String,
    },
    Apt {
        pkg: PackageRecord,
        source: SourceConfigEntry,
        apt_pkg: AptPackage,
    },
    AppImageHubGitHub {
        pkg: PackageRecord,
        source_id: String,
        app_id: String,
        repo: String,
        download_url: Option<String>,
        new_tag: String,
        new_etag: Option<String>,
        asset: CandidateAsset,
        release_assets: Vec<ReleaseAsset>,
        pattern: String,
    },
    AppImageHubDirect {
        pkg: PackageRecord,
        source_id: String,
        app_id: String,
        download_url: String,
        new_version: String,
        new_etag: Option<String>,
    },
}

impl UpgradeAction {
    fn pkg(&self) -> &PackageRecord {
        match self {
            Self::GitHub { pkg, .. }
            | Self::Apt { pkg, .. }
            | Self::AppImageHubGitHub { pkg, .. }
            | Self::AppImageHubDirect { pkg, .. } => pkg,
        }
    }

    fn target_version_display(&self) -> String {
        match self {
            Self::GitHub { new_tag, .. } | Self::AppImageHubGitHub { new_tag, .. } => {
                new_tag.trim_start_matches('v').to_string()
            }
            Self::Apt { apt_pkg, .. } => apt_pkg.version.clone(),
            Self::AppImageHubDirect { new_version, .. } => new_version.clone(),
        }
    }

    fn source_label(&self) -> String {
        match self {
            Self::GitHub { repo, .. } => repo.clone(),
            Self::Apt { source, .. } => format!("apt:{}", source.id),
            Self::AppImageHubGitHub {
                source_id, repo, ..
            } => format!("appimagehub:{} ({})", source_id, repo),
            Self::AppImageHubDirect { source_id, .. } => format!("appimagehub:{}", source_id),
        }
    }

    fn format_label(&self) -> String {
        match self {
            Self::GitHub { asset, .. } | Self::AppImageHubGitHub { asset, .. } => {
                asset.format.clone()
            }
            Self::Apt { .. } => "deb".to_string(),
            Self::AppImageHubDirect { .. } => "appimage".to_string(),
        }
    }
}

enum UpgradeCheckResult {
    Upgrade(Box<UpgradeAction>),
    UpToDate(PackageRecord, bool),
    Pinned(PackageRecord),
}

impl Core {
    pub async fn link(&mut self, app_id: &str, repo_str: &str) -> Result<bool, String> {
        let pkg = match self.db.get(app_id) {
            Some(p) => p.clone(),
            None => {
                println!(
                    "{} Package '{}' is not managed by appmgr.",
                    apt_prefix("FAIL"),
                    app_id
                );
                return Ok(false);
            }
        };

        let parsed_repo = GitHubClient::parse_repo(repo_str)
            .ok_or_else(|| format!("Invalid repository format: '{}'. Use owner/repo.", repo_str))?;

        let spinner = Spinner::new(&format!("Validating GitHub repository {}...", parsed_repo));
        let (status, release_opt, etag, _) = self
            .github
            .get_release(&parsed_repo, None, false, None)
            .await?;
        spinner.stop();

        if status == StatusCode::NOT_FOUND || release_opt.is_none() {
            println!(
                "{} Repository '{}' has no releases yet.",
                apt_prefix("WARN"),
                parsed_repo
            );
            let _ = self
                .db
                .link_repo(&pkg.id, parsed_repo.clone(), None, None, None);
            println!(
                " {} Linked {} -> {} (pending first release).",
                apt_prefix("OK"),
                pkg.name,
                c(&parsed_repo, &[Colors::BOLD, Colors::BRIGHT_CYAN])
            );
            return Ok(true);
        }

        let release = release_opt.unwrap();
        let candidates = self
            .github
            .filter_assets(&release.assets, Some(&pkg.format), None, None);
        let pattern = candidates
            .first()
            .map(|c| GitHubClient::derive_pattern(&c.name, &release.tag_name));

        self.db
            .link_repo(
                &pkg.id,
                parsed_repo.clone(),
                Some(release.tag_name.clone()),
                etag,
                pattern,
            )
            .map_err(|e| format!("DB error: {}", e))?;

        println!(
            " {} Successfully linked {} to {} ({})",
            apt_prefix("OK"),
            c(&pkg.name, &[Colors::BOLD, Colors::BRIGHT_WHITE]),
            c(&parsed_repo, &[Colors::BOLD, Colors::BRIGHT_CYAN]),
            release.tag_name
        );
        Ok(true)
    }

    async fn print_self_update_hint_if_available(&self, include_pre: bool) {
        #[cfg(feature = "self-update")]
        {
            if let Some(new_ver) = crate::self_update::SelfUpdater::new()
                .check_latest_version(include_pre)
                .await
            {
                println!(
                    " {} A new version of appmgr is available ({} -> {}); run '{}' to update.\n",
                    apt_prefix("INFO"),
                    env!("CARGO_PKG_VERSION"),
                    c(&new_ver, &[Colors::BOLD, Colors::YELLOW]),
                    c("appmgr self-update", &[Colors::BOLD, Colors::CYAN])
                );
            }
        }
        #[cfg(not(feature = "self-update"))]
        {
            let _ = include_pre;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn upgrade(
        &mut self,
        app_query: Option<&str>,
        check_only: bool,
        assume_yes: bool,
        prefer: Option<&str>,
        include_pre: bool,
        force: bool,
        as_json: bool,
    ) -> Result<bool, String> {
        let target_pkgs: Vec<PackageRecord> = if let Some(q) = app_query {
            match self.db.get(q) {
                Some(p) => vec![p.clone()],
                None => {
                    if as_json {
                        println!("{}", json!({"error": format!("Package '{}' not found", q)}));
                    }
                    return Err(format!("Package '{}' is not managed by appmgr.", q));
                }
            }
        } else {
            self.db.all().into_iter().cloned().collect()
        };

        let tracked_pkgs: Vec<PackageRecord> = target_pkgs
            .into_iter()
            .filter(|p| !matches!(p.resolved_origin(), PackageOrigin::Local { .. }))
            .collect();

        if tracked_pkgs.is_empty() {
            if let Some(q) = app_query {
                if as_json {
                    println!(
                        "{}",
                        json!({"error": format!("Package '{}' has no tracked remote source", q)})
                    );
                }
                return Err(format!(
                    "Package '{}' has no tracked remote source (use 'appmgr link' to attach a GitHub repository).",
                    q
                ));
            }
            if as_json {
                println!(
                    "{}",
                    json!({"upgrades": [], "up_to_date": [], "pinned": []})
                );
                return Ok(true);
            }
            println!(
                "{} No installed packages track an upstream source (GitHub, APT, or AppImageHub).",
                c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE])
            );
            if check_only {
                println!();
                self.print_self_update_hint_if_available(include_pre).await;
            }
            return Ok(false);
        }

        if !as_json {
            println!(
                "{} Checking {} tracked package(s) across configured origins...",
                c("::", &[Colors::BOLD, Colors::CYAN]),
                tracked_pkgs.len()
            );
        }

        let effective_sources = crate::registry::get_effective_sources(
            &self.config.sources_list_file,
            &self.config.registries_cache_dir,
        );
        let source_map: std::collections::HashMap<String, SourceConfigEntry> = effective_sources
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect();

        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
        let client = self.github.clone();
        let http = self.http_client.clone();
        let indexes_dir = self.config.indexes_cache_dir.clone();
        let prefer_owned = prefer
            .or(self.config.settings.default_prefer.as_deref())
            .map(String::from);

        let total_to_check = tracked_pkgs.len();
        let spinner = if !as_json && total_to_check > 0 {
            Some(Spinner::with_counter(
                "Checking upstream package sources...",
                total_to_check,
                "checked",
            ))
        } else {
            None
        };
        let counter = spinner.as_ref().map(|s| s.counter());

        let mut tasks = Vec::new();
        for pkg in tracked_pkgs {
            let sem = semaphore.clone();
            let gh = client.clone();
            let http_cl = http.clone();
            let idx_dir = indexes_dir.clone();
            let sources = source_map.clone();
            let pref = prefer_owned.clone();
            let force_flag = force;
            let single_app = app_query.is_some();
            let ctr = counter.clone();

            tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let outcome = if pkg.pinned && !force_flag && !single_app {
                    UpgradeCheckResult::Pinned(pkg)
                } else {
                    match pkg.resolved_origin() {
                        PackageOrigin::GitHub {
                            repo,
                            tag,
                            etag,
                            asset_pattern,
                        } => {
                            Self::check_github_upgrade(
                                &gh,
                                pkg,
                                &repo,
                                tag.as_deref(),
                                etag.as_deref(),
                                asset_pattern.as_deref(),
                                pref.as_deref(),
                                include_pre,
                            )
                            .await
                        }
                        PackageOrigin::Apt { source_id, package } => {
                            Self::check_apt_upgrade(pkg, &source_id, &package, &sources, &idx_dir)
                        }
                        PackageOrigin::AppImageHub {
                            source_id,
                            app_id,
                            github_repo,
                            tag,
                            etag,
                            asset_pattern,
                            download_url,
                        } => {
                            Self::check_appimagehub_upgrade(
                                &gh,
                                &http_cl,
                                pkg,
                                &source_id,
                                &app_id,
                                github_repo,
                                tag,
                                etag,
                                asset_pattern,
                                download_url,
                                pref.as_deref(),
                                include_pre,
                                &idx_dir,
                            )
                            .await
                        }
                        PackageOrigin::Local { .. } => UpgradeCheckResult::UpToDate(pkg, false),
                    }
                };

                if let Some(ref c_ref) = ctr {
                    c_ref.fetch_add(1, Ordering::Relaxed);
                }
                outcome
            }));
        }

        let mut upgrades = Vec::new();
        let mut up_to_date = Vec::new();
        let mut pinned = Vec::new();

        for task in tasks {
            if let Ok(res) = task.await {
                match res {
                    UpgradeCheckResult::Upgrade(action) => upgrades.push(*action),
                    UpgradeCheckResult::UpToDate(pkg, is_304) => up_to_date.push((pkg, is_304)),
                    UpgradeCheckResult::Pinned(pkg) => pinned.push(pkg),
                }
            }
        }
        if let Some(s) = spinner {
            s.stop();
        }

        if as_json {
            let json_out = json!({
                "upgrades": upgrades.iter().map(|act| {
                    let p = act.pkg();
                    json!({
                        "id": p.id,
                        "name": p.name,
                        "current_version": p.version,
                        "latest_version": act.target_version_display(),
                        "source": act.source_label(),
                        "repo": p.repo,
                        "format": act.format_label(),
                    })
                }).collect::<Vec<_>>(),
                "up_to_date": up_to_date.iter().map(|(p, _)| {
                    json!({ "name": p.name, "version": p.version })
                }).collect::<Vec<_>>(),
                "pinned": pinned.iter().map(|p| {
                    json!({ "name": p.name, "version": p.version })
                }).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&json_out).unwrap());
            return Ok(true);
        }

        for p in &pinned {
            println!(
                " {} {} is pinned to {} {} (use --force to upgrade)",
                apt_prefix("INFO"),
                c(&p.name, &[Colors::BOLD, Colors::MAGENTA]),
                p.version,
                c("[PINNED]", &[Colors::BOLD, Colors::MAGENTA])
            );
        }

        for (ut, is_304) in &up_to_date {
            let note = if *is_304 {
                format!(" {}", c("(304 Not Modified)", &[Colors::DIM]))
            } else {
                String::new()
            };
            if check_only || app_query.is_some() {
                println!(
                    " {} {} is up to date ({}){}",
                    apt_prefix("OK"),
                    c(&ut.name, &[Colors::BOLD, Colors::WHITE]),
                    ut.version,
                    note
                );
            }
        }

        for act in &upgrades {
            let u = act.pkg();
            println!(
                " {} {}: update available ({} -> {}) {}",
                apt_prefix("INFO"),
                c(&u.name, &[Colors::BOLD, Colors::GREEN]),
                u.version,
                c(
                    &act.target_version_display(),
                    &[Colors::BOLD, Colors::YELLOW]
                ),
                c(&format!("[{}]", act.source_label()), &[Colors::DIM])
            );
        }

        if check_only {
            println!();
            if !upgrades.is_empty() {
                println!(
                    "{} {} package(s) can be upgraded. Run {} to apply updates.\n",
                    c("::", &[Colors::BOLD, Colors::YELLOW]),
                    upgrades.len(),
                    c("appmgr upgrade", &[Colors::BOLD, Colors::GREEN])
                );
            } else {
                println!(
                    "{} All packages are up to date.\n",
                    c("::", &[Colors::BOLD, Colors::GREEN])
                );
            }
            self.print_self_update_hint_if_available(include_pre).await;
            return Ok(true);
        }

        if upgrades.is_empty() {
            println!(
                "\n{} All packages are up to date. Nothing to do.\n",
                c("::", &[Colors::BOLD, Colors::GREEN])
            );
            return Ok(true);
        }

        if !assume_yes && !confirm_dialog("Proceed with upgrade?", true) {
            println!(
                "{} Upgrade cancelled by user.",
                c("::", &[Colors::BOLD, Colors::CYAN])
            );
            return Ok(false);
        }

        let keep_backups = self.config.settings.keep_backups;
        let mut success_count = 0;
        let mut failed_count = 0;

        for action in upgrades {
            let pkg = action.pkg().clone();
            let target_ver = action.target_version_display();
            println!(
                "\n{} Upgrading {} ({} -> {}) ...",
                c("::", &[Colors::BOLD, Colors::CYAN]),
                c(&pkg.name, &[Colors::BOLD, Colors::CYAN]),
                pkg.version,
                c(&target_ver, &[Colors::BOLD, Colors::YELLOW])
            );

            let backup_dir =
                Self::prepare_upgrade_backup(&self.config.backups_dir, &pkg, keep_backups)?;

            let apply_res = self.apply_upgrade_action(&action, &pkg).await;
            match apply_res {
                Ok(upgraded_id) => {
                    Self::finalize_upgrade_backup(
                        &self.config.backups_dir,
                        &pkg.id,
                        &backup_dir,
                        keep_backups,
                    );
                    let upgrade_auto_deps =
                        assume_yes || matches!(self.config.settings.deps_mode, DepsMode::Host);
                    let _ = self
                        .resolve_package_dependencies(&upgraded_id, upgrade_auto_deps, false, false)
                        .await;
                    success_count += 1;
                    println!(
                        " {} {} upgraded to {}",
                        apt_prefix("OK"),
                        pkg.name,
                        c(&target_ver, &[Colors::BOLD, Colors::GREEN])
                    );
                }
                Err(e) => {
                    failed_count += 1;
                    println!(
                        "{} Upgrade failed ({}). Rolling back...",
                        apt_prefix("FAIL"),
                        e
                    );
                    let _ = self.restore_from_backup_dir(&pkg, &backup_dir);
                    if keep_backups == 0 {
                        let _ = fs::remove_dir_all(&backup_dir);
                    }
                }
            }
        }

        if failed_count > 0 {
            return Err(format!(
                "{} package upgrade(s) failed ({} succeeded).",
                failed_count, success_count
            ));
        }

        println!(
            "\n{} Upgrade complete. {} packages upgraded successfully.\n",
            c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
            success_count
        );
        Ok(success_count > 0)
    }

    #[allow(clippy::too_many_arguments)]
    async fn check_github_upgrade(
        gh: &GitHubClient,
        pkg: PackageRecord,
        repo: &str,
        current_tag: Option<&str>,
        etag: Option<&str>,
        asset_pattern: Option<&str>,
        prefer: Option<&str>,
        include_pre: bool,
    ) -> UpgradeCheckResult {
        let release_res = gh.get_release(repo, None, include_pre, etag).await;
        match release_res {
            Ok((StatusCode::NOT_MODIFIED, _, _, _)) => UpgradeCheckResult::UpToDate(pkg, true),
            Ok((StatusCode::OK, Some(release), new_etag, _)) => {
                let new_tag = release.tag_name.clone();
                let cur_tag = current_tag.unwrap_or("");
                let is_newer = GitHubClient::compare_versions(&new_tag, &pkg.version)
                    == std::cmp::Ordering::Greater
                    || (new_tag != cur_tag
                        && !cur_tag.is_empty()
                        && GitHubClient::compare_versions(&new_tag, &pkg.version)
                            != std::cmp::Ordering::Less);

                if is_newer {
                    let pref_format = prefer.unwrap_or(&pkg.format);
                    let candidates =
                        gh.filter_assets(&release.assets, Some(pref_format), asset_pattern, None);
                    if let Some(chosen) = candidates.into_iter().next() {
                        let pattern = asset_pattern.map(String::from).unwrap_or_else(|| {
                            GitHubClient::derive_pattern(&chosen.name, &new_tag)
                        });
                        return UpgradeCheckResult::Upgrade(Box::new(UpgradeAction::GitHub {
                            pkg,
                            repo: repo.to_string(),
                            new_tag,
                            new_etag,
                            asset: chosen,
                            release_assets: release.assets,
                            pattern,
                        }));
                    }
                }
                UpgradeCheckResult::UpToDate(pkg, false)
            }
            _ => UpgradeCheckResult::UpToDate(pkg, false),
        }
    }

    fn check_apt_upgrade(
        pkg: PackageRecord,
        source_id: &str,
        package: &str,
        sources: &std::collections::HashMap<String, SourceConfigEntry>,
        indexes_dir: &Path,
    ) -> UpgradeCheckResult {
        let Some(source) = sources.get(source_id) else {
            return UpgradeCheckResult::UpToDate(pkg, false);
        };
        if !source.enabled {
            return UpgradeCheckResult::UpToDate(pkg, false);
        }
        let idx_file = indexes_dir.join(format!("{}.json", source_id));
        if !idx_file.exists() {
            return UpgradeCheckResult::UpToDate(pkg, false);
        }
        let pkgs = get_cached_apt_packages(&idx_file);
        let Some(apt_pkg) = pkgs.get(package) else {
            return UpgradeCheckResult::UpToDate(pkg, false);
        };
        if compare_apt_versions(&apt_pkg.version, &pkg.version) == std::cmp::Ordering::Greater {
            UpgradeCheckResult::Upgrade(Box::new(UpgradeAction::Apt {
                pkg,
                source: source.clone(),
                apt_pkg: apt_pkg.clone(),
            }))
        } else {
            UpgradeCheckResult::UpToDate(pkg, false)
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn check_appimagehub_upgrade(
        gh: &GitHubClient,
        http: &reqwest::Client,
        pkg: PackageRecord,
        source_id: &str,
        app_id: &str,
        stored_repo: Option<String>,
        stored_tag: Option<String>,
        stored_etag: Option<String>,
        stored_pattern: Option<String>,
        stored_url: Option<String>,
        prefer: Option<&str>,
        include_pre: bool,
        indexes_dir: &Path,
    ) -> UpgradeCheckResult {
        let idx_file = indexes_dir.join(format!("{}.json", source_id));
        let idx_entry = if idx_file.exists() {
            get_cached_appimagehub_packages(&idx_file)
                .get(app_id)
                .cloned()
        } else {
            None
        };

        let eff_repo = idx_entry
            .as_ref()
            .and_then(|e| e.github_repo.clone())
            .or(stored_repo);
        let eff_url = idx_entry
            .as_ref()
            .and_then(|e| e.download_url.clone())
            .or(stored_url);

        if let Some(repo) = eff_repo {
            let release_res = gh
                .get_release(&repo, None, include_pre, stored_etag.as_deref())
                .await;
            return match release_res {
                Ok((StatusCode::NOT_MODIFIED, _, _, _)) => UpgradeCheckResult::UpToDate(pkg, true),
                Ok((StatusCode::OK, Some(release), new_etag, _)) => {
                    let new_tag = release.tag_name.clone();
                    let cur_tag = stored_tag.as_deref().unwrap_or("");
                    let is_newer = GitHubClient::compare_versions(&new_tag, &pkg.version)
                        == std::cmp::Ordering::Greater
                        || (new_tag != cur_tag
                            && !cur_tag.is_empty()
                            && GitHubClient::compare_versions(&new_tag, &pkg.version)
                                != std::cmp::Ordering::Less);
                    if is_newer {
                        let pref_fmt = prefer.unwrap_or("appimage");
                        let candidates = gh.filter_assets(
                            &release.assets,
                            Some(pref_fmt),
                            stored_pattern.as_deref(),
                            None,
                        );
                        if let Some(chosen) = candidates.into_iter().next() {
                            let pattern = stored_pattern.unwrap_or_else(|| {
                                GitHubClient::derive_pattern(&chosen.name, &new_tag)
                            });
                            return UpgradeCheckResult::Upgrade(Box::new(
                                UpgradeAction::AppImageHubGitHub {
                                    pkg,
                                    source_id: source_id.to_string(),
                                    app_id: app_id.to_string(),
                                    repo,
                                    download_url: eff_url,
                                    new_tag,
                                    new_etag,
                                    asset: chosen,
                                    release_assets: release.assets,
                                    pattern,
                                },
                            ));
                        }
                    }
                    UpgradeCheckResult::UpToDate(pkg, false)
                }
                _ => UpgradeCheckResult::UpToDate(pkg, false),
            };
        }

        if let Some(url) = eff_url {
            let mut req = http.head(&url);
            if let Some(ref tag) = stored_etag {
                if tag.starts_with('"') || tag.starts_with("W/") {
                    req = req.header("If-None-Match", tag.as_str());
                } else {
                    req = req.header("If-Modified-Since", tag.as_str());
                }
            }
            if let Ok(resp) = req.send().await {
                if resp.status() == StatusCode::NOT_MODIFIED {
                    return UpgradeCheckResult::UpToDate(pkg, true);
                }
                if resp.status().is_success() {
                    let new_etag = resp
                        .headers()
                        .get("etag")
                        .or_else(|| resp.headers().get("last-modified"))
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_string());
                    let remote_len = resp.content_length();
                    let changed = match (&stored_etag, &new_etag) {
                        (Some(old), Some(new)) => old != new,
                        (None, _) => remote_len.is_some() && remote_len != pkg.total_size_bytes,
                        _ => false,
                    };
                    if changed {
                        return UpgradeCheckResult::Upgrade(Box::new(
                            UpgradeAction::AppImageHubDirect {
                                pkg,
                                source_id: source_id.to_string(),
                                app_id: app_id.to_string(),
                                download_url: url,
                                new_version: "latest".to_string(),
                                new_etag,
                            },
                        ));
                    }
                }
            }
        }

        UpgradeCheckResult::UpToDate(pkg, false)
    }

    async fn apply_upgrade_action(
        &mut self,
        action: &UpgradeAction,
        old_pkg: &PackageRecord,
    ) -> Result<String, String> {
        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let env_opt = Some(old_pkg.env.clone());
        let args_opt = Some(old_pkg.args.clone());

        match action {
            UpgradeAction::GitHub {
                repo,
                new_tag,
                new_etag,
                asset,
                release_assets,
                pattern,
                ..
            } => {
                let downloaded_file = temp_dir.path().join(&asset.name);
                self.github
                    .download_asset(
                        &asset.url,
                        &downloaded_file,
                        asset.size,
                        &asset.name,
                        Some(asset),
                        Some(release_assets),
                    )
                    .await?;

                let id = self
                    .install_local_file(
                        &downloaded_file,
                        true,
                        true,
                        Some(repo.clone()),
                        Some(new_tag.clone()),
                        new_etag.clone(),
                        Some(pattern.clone()),
                        env_opt,
                        args_opt,
                        old_pkg.pinned,
                        None,
                    )?
                    .ok_or_else(|| "Installer returned no package record".to_string())?;
                self.preserve_defaults_on_upgrade(&id, old_pkg);
                Ok(id)
            }
            UpgradeAction::Apt {
                source, apt_pkg, ..
            } => {
                let deb_url = if apt_pkg.filename.starts_with("http://")
                    || apt_pkg.filename.starts_with("https://")
                {
                    apt_pkg.filename.clone()
                } else {
                    format!(
                        "{}/{}",
                        source.url.trim_end_matches('/'),
                        apt_pkg.filename.trim_start_matches('/')
                    )
                };
                let file_stem = apt_pkg
                    .filename
                    .split('/')
                    .next_back()
                    .unwrap_or(&apt_pkg.package);
                let deb_name = if file_stem.ends_with(".deb") {
                    file_stem.to_string()
                } else {
                    format!(
                        "{}_{}_{}.deb",
                        apt_pkg.package, apt_pkg.version, apt_pkg.architecture
                    )
                };
                let target_path = self.config.cache_dir.join("downloads").join(&deb_name);
                let expected_sha = if apt_pkg.sha256.is_empty() {
                    None
                } else {
                    Some(apt_pkg.sha256.as_str())
                };
                self.download_direct_url(&deb_url, &target_path, &deb_name, expected_sha)
                    .await?;

                let id = self
                    .install_local_file(
                        &target_path,
                        true,
                        true,
                        None,
                        Some(apt_pkg.version.clone()),
                        None,
                        None,
                        env_opt,
                        args_opt,
                        old_pkg.pinned,
                        Some(apt_pkg.package.clone()),
                    )?
                    .ok_or_else(|| "APT upgrade returned no package record".to_string())?;

                if let Some(rec) = self.db.get_mut(&id) {
                    rec.set_origin(PackageOrigin::Apt {
                        source_id: source.id.clone(),
                        package: apt_pkg.package.clone(),
                    });
                    if let Some(ref desc) = apt_pkg.description {
                        rec.description = Some(desc.clone());
                    }
                }
                self.preserve_defaults_on_upgrade(&id, old_pkg);
                Ok(id)
            }
            UpgradeAction::AppImageHubGitHub {
                source_id,
                app_id,
                repo,
                download_url,
                new_tag,
                new_etag,
                asset,
                release_assets,
                pattern,
                ..
            } => {
                let downloaded_file = temp_dir.path().join(&asset.name);
                self.github
                    .download_asset(
                        &asset.url,
                        &downloaded_file,
                        asset.size,
                        &asset.name,
                        Some(asset),
                        Some(release_assets),
                    )
                    .await?;

                let id = self
                    .install_local_file(
                        &downloaded_file,
                        true,
                        true,
                        Some(repo.clone()),
                        Some(new_tag.clone()),
                        new_etag.clone(),
                        Some(pattern.clone()),
                        env_opt,
                        args_opt,
                        old_pkg.pinned,
                        None,
                    )?
                    .ok_or_else(|| "AppImageHub upgrade returned no package record".to_string())?;

                if let Some(rec) = self.db.get_mut(&id) {
                    rec.set_origin(PackageOrigin::AppImageHub {
                        source_id: source_id.clone(),
                        app_id: app_id.clone(),
                        github_repo: Some(repo.clone()),
                        tag: Some(new_tag.clone()),
                        etag: new_etag.clone(),
                        asset_pattern: Some(pattern.clone()),
                        download_url: download_url.clone(),
                    });
                }
                self.preserve_defaults_on_upgrade(&id, old_pkg);
                Ok(id)
            }
            UpgradeAction::AppImageHubDirect {
                source_id,
                app_id,
                download_url,
                new_etag,
                ..
            } => {
                let filename = format!("{}.AppImage", app_id);
                let target_path = self.config.cache_dir.join("downloads").join(&filename);
                let fresh_etag = self
                    .download_direct_url_with_meta(download_url, &target_path, &filename, None)
                    .await?;

                let id = self
                    .install_local_file(
                        &target_path,
                        true,
                        true,
                        None,
                        None,
                        None,
                        None,
                        env_opt,
                        args_opt,
                        old_pkg.pinned,
                        Some(app_id.clone()),
                    )?
                    .ok_or_else(|| "AppImageHub upgrade returned no package record".to_string())?;

                if let Some(rec) = self.db.get_mut(&id) {
                    rec.set_origin(PackageOrigin::AppImageHub {
                        source_id: source_id.clone(),
                        app_id: app_id.clone(),
                        github_repo: None,
                        tag: rec.tag.clone(),
                        etag: fresh_etag.or_else(|| new_etag.clone()),
                        asset_pattern: None,
                        download_url: Some(download_url.clone()),
                    });
                }
                self.preserve_defaults_on_upgrade(&id, old_pkg);
                Ok(id)
            }
        }
    }

    fn preserve_defaults_on_upgrade(&mut self, pkg_id: &str, old_pkg: &PackageRecord) {
        if let Some(rec) = self.db.get_mut(pkg_id) {
            if rec.default_env.is_none() {
                rec.default_env = old_pkg.default_env.clone();
            }
            if rec.default_args.is_none() {
                rec.default_args = old_pkg.default_args.clone();
            }
            let _ = write_package_launchers(rec, &self.config.bin_dir);
            let _ = self.db.save();
        }
    }

    pub fn prepare_upgrade_backup(
        backups_dir: &Path,
        pkg: &PackageRecord,
        keep_backups: usize,
    ) -> Result<PathBuf, String> {
        let backup_dir = if keep_backups == 0 {
            backups_dir.join(format!(".tmp-{}", pkg.id))
        } else {
            backups_dir.join(&pkg.id)
        };

        if keep_backups <= 1 {
            let _ = fs::remove_dir_all(&backup_dir);
        } else {
            let max_prev = keep_backups - 1;
            if let Ok(entries) = fs::read_dir(&backup_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if let Some(num_str) = name.strip_prefix("prev-") {
                        if let Ok(num) = num_str.parse::<usize>() {
                            if num >= max_prev {
                                let _ = fs::remove_dir_all(entry.path());
                            }
                        }
                    }
                }
            }
            for k in (1..max_prev).rev() {
                let src = backup_dir.join(format!("prev-{}", k));
                let dst = backup_dir.join(format!("prev-{}", k + 1));
                if src.exists() {
                    let _ = fs::remove_dir_all(&dst);
                    let _ = fs::rename(&src, &dst);
                }
            }
            let cur_meta = backup_dir.join("meta.json");
            let cur_files = backup_dir.join("files");
            if cur_meta.exists() && cur_files.exists() {
                let prev1 = backup_dir.join("prev-1");
                let _ = fs::remove_dir_all(&prev1);
                fs::create_dir_all(&prev1).map_err(|e| e.to_string())?;
                fs::rename(&cur_meta, prev1.join("meta.json")).map_err(|e| e.to_string())?;
                fs::rename(&cur_files, prev1.join("files")).map_err(|e| e.to_string())?;
                for extra in ["desktop", "icons"] {
                    let cur_extra = backup_dir.join(extra);
                    if cur_extra.exists() {
                        let _ = fs::rename(&cur_extra, prev1.join(extra));
                    }
                }
            } else {
                let _ = fs::remove_file(&cur_meta);
                let _ = fs::remove_dir_all(&cur_files);
                let _ = fs::remove_dir_all(backup_dir.join("desktop"));
                let _ = fs::remove_dir_all(backup_dir.join("icons"));
            }
        }

        if pkg.install_dir.exists() {
            fs::create_dir_all(&backup_dir).map_err(|e| e.to_string())?;
            let backup_files = backup_dir.join("files");
            ArchivePackage::copy_dir_all(&pkg.install_dir, &backup_files)?;

            if !pkg.desktop_files.is_empty() {
                let desktop_bak = backup_dir.join("desktop");
                fs::create_dir_all(&desktop_bak).map_err(|e| e.to_string())?;
                for (idx, dt) in pkg.desktop_files.iter().enumerate() {
                    if dt.is_file() {
                        let fname = dt.file_name().unwrap_or_default().to_string_lossy();
                        let _ = fs::copy(dt, desktop_bak.join(format!("{}_{}", idx, fname)));
                    }
                }
            }

            if !pkg.icons.is_empty() {
                let icons_bak = backup_dir.join("icons");
                fs::create_dir_all(&icons_bak).map_err(|e| e.to_string())?;
                for (idx, ic) in pkg.icons.iter().enumerate() {
                    if ic.is_file() {
                        let fname = ic.file_name().unwrap_or_default().to_string_lossy();
                        let _ = fs::copy(ic, icons_bak.join(format!("{}_{}", idx, fname)));
                    }
                }
            }

            fs::write(
                backup_dir.join("meta.json"),
                serde_json::to_string_pretty(pkg).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(backup_dir)
    }

    pub fn finalize_upgrade_backup(
        backups_dir: &Path,
        pkg_id: &str,
        active_backup_dir: &Path,
        keep_backups: usize,
    ) {
        if keep_backups == 0 {
            let _ = fs::remove_dir_all(active_backup_dir);
            let _ = fs::remove_dir_all(backups_dir.join(pkg_id));
        }
    }

    pub fn promote_previous_backup(backup_dir: &Path) {
        let _ = fs::remove_file(backup_dir.join("meta.json"));
        let _ = fs::remove_dir_all(backup_dir.join("files"));
        let _ = fs::remove_dir_all(backup_dir.join("desktop"));
        let _ = fs::remove_dir_all(backup_dir.join("icons"));

        let prev1 = backup_dir.join("prev-1");
        if prev1.exists() {
            if prev1.join("meta.json").exists() {
                let _ = fs::rename(prev1.join("meta.json"), backup_dir.join("meta.json"));
            }
            if prev1.join("files").exists() {
                let _ = fs::rename(prev1.join("files"), backup_dir.join("files"));
            }
            for extra in ["desktop", "icons"] {
                if prev1.join(extra).exists() {
                    let _ = fs::rename(prev1.join(extra), backup_dir.join(extra));
                }
            }
            let _ = fs::remove_dir_all(&prev1);

            let mut idx = 2usize;
            while backup_dir.join(format!("prev-{}", idx)).exists() {
                let src = backup_dir.join(format!("prev-{}", idx));
                let dst = backup_dir.join(format!("prev-{}", idx - 1));
                let _ = fs::rename(src, dst);
                idx += 1;
            }
        } else {
            let _ = fs::remove_dir_all(backup_dir);
        }
    }

    fn restore_from_backup_dir(
        &mut self,
        current_pkg: &PackageRecord,
        backup_dir: &Path,
    ) -> Result<PackageRecord, String> {
        let meta_file = backup_dir.join("meta.json");
        let files_dir = backup_dir.join("files");

        let meta_content = fs::read_to_string(&meta_file)
            .map_err(|e| format!("Failed to read rollback metadata: {}", e))?;
        let prev_record: PackageRecord = serde_json::from_str(&meta_content)
            .map_err(|e| format!("Corrupt rollback metadata: {}", e))?;

        // 1. Restore install_dir (preserving symlinks) atomically via staging
        let apps_parent = prev_record
            .install_dir
            .parent()
            .unwrap_or(&self.config.apps_dir);
        let mut staging =
            StagingDirGuard::new(apps_parent, &format!("rollback-{}", prev_record.id))?;
        ArchivePackage::copy_dir_all(&files_dir, &staging.path)?;
        let stage_path = staging.path.clone();
        staging.commit_swap(&stage_path, &prev_record.install_dir, true)?;

        if current_pkg.install_dir != prev_record.install_dir && current_pkg.install_dir.exists() {
            let _ = fs::remove_dir_all(&current_pkg.install_dir);
        }

        // 2. Remove current launchers, desktop files, and icons before regenerating/restoring
        for b in &current_pkg.binaries {
            let _ = fs::remove_file(b);
        }
        for d in &current_pkg.desktop_files {
            let _ = fs::remove_file(d);
        }
        for i in &current_pkg.icons {
            let _ = fs::remove_file(i);
        }

        // 3. Regenerate launchers declaratively via launcher.rs
        write_package_launchers(&prev_record, &self.config.bin_dir)?;

        // 4. Restore icons from backup (or fallback to re-extracting from restored install_dir)
        let icons_bak = backup_dir.join("icons");
        let mut restored_any_icon = false;
        if icons_bak.exists() {
            for (idx, dest_icon) in prev_record.icons.iter().enumerate() {
                let fname = dest_icon.file_name().unwrap_or_default().to_string_lossy();
                let src = icons_bak.join(format!("{}_{}", idx, fname));
                if src.is_file() {
                    if let Some(parent) = dest_icon.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    if fs::copy(&src, dest_icon).is_ok() {
                        restored_any_icon = true;
                    }
                }
            }
        }
        if !restored_any_icon && !prev_record.icons.is_empty() {
            let icons_root = prev_record.install_dir.join("usr/share/icons");
            let pixmaps_root = prev_record.install_dir.join("usr/share/pixmaps");
            let _ = install_best_icon(
                &[icons_root.as_path(), pixmaps_root.as_path()],
                &self.config.icons_dir,
                &prev_record.id,
            );
        }

        // 5. Restore .desktop files from backup (or fallback regenerate if GUI)
        let desktop_bak = backup_dir.join("desktop");
        let mut restored_any_dt = false;
        if desktop_bak.exists() {
            for (idx, dest_dt) in prev_record.desktop_files.iter().enumerate() {
                let fname = dest_dt.file_name().unwrap_or_default().to_string_lossy();
                let src = desktop_bak.join(format!("{}_{}", idx, fname));
                if src.is_file() {
                    if let Some(parent) = dest_dt.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    if fs::copy(&src, dest_dt).is_ok() {
                        restored_any_dt = true;
                    }
                }
            }
        }
        if !restored_any_dt && prev_record.is_gui == Some(true) {
            if let Some(primary_launcher) = prev_record.binaries.first() {
                let dest_dt = prev_record
                    .desktop_files
                    .first()
                    .cloned()
                    .unwrap_or_else(|| {
                        self.config
                            .applications_dir
                            .join(format!("appmgr-{}.desktop", prev_record.id))
                    });
                let icon_name = if prev_record.icons.is_empty() {
                    None
                } else {
                    Some(prev_record.id.as_str())
                };
                let _ = create_desktop_entry(
                    &dest_dt,
                    &prev_record.name,
                    prev_record
                        .description
                        .as_deref()
                        .unwrap_or(&prev_record.name),
                    primary_launcher,
                    icon_name,
                    "Utility;Application;",
                );
            }
        }

        // 6. Restore database record
        self.db
            .add(prev_record.clone())
            .map_err(|e| e.to_string())?;
        run_system_triggers(&self.config.applications_dir, &self.config.icons_dir);

        Ok(prev_record)
    }

    pub fn rollback(&mut self, app_id: &str, assume_yes: bool) -> Result<bool, String> {
        let pkg = self
            .db
            .get(app_id)
            .ok_or_else(|| format!("Package '{}' is not managed by appmgr.", app_id))?
            .clone();

        let backup_dir = self.config.backups_dir.join(&pkg.id);
        let meta_file = backup_dir.join("meta.json");
        let files_dir = backup_dir.join("files");

        if !backup_dir.exists() || !meta_file.exists() || !files_dir.exists() {
            println!(
                "{} No rollback backup found for '{}'.",
                apt_prefix("FAIL"),
                pkg.name
            );
            return Ok(false);
        }

        let meta_content = fs::read_to_string(&meta_file)
            .map_err(|e| format!("Failed to read rollback metadata: {}", e))?;
        let prev_record: PackageRecord = serde_json::from_str(&meta_content)
            .map_err(|e| format!("Corrupt rollback metadata: {}", e))?;

        println!(
            "\n{} Rollback application: {}",
            c("::", &[Colors::BOLD, Colors::YELLOW]),
            c(&pkg.name, &[Colors::BOLD, Colors::WHITE])
        );
        println!(
            "   Current version:  {}",
            c(&pkg.version, &[Colors::BOLD, Colors::RED])
        );
        println!(
            "   Rollback version: {}",
            c(&prev_record.version, &[Colors::BOLD, Colors::GREEN])
        );

        if !assume_yes
            && !confirm_dialog(
                &format!(
                    "Are you sure you want to rollback {} to {}?",
                    pkg.name, prev_record.version
                ),
                true,
            )
        {
            println!(
                "{} Rollback cancelled.",
                c("::", &[Colors::BOLD, Colors::CYAN])
            );
            return Ok(false);
        }

        let restored = self.restore_from_backup_dir(&pkg, &backup_dir)?;
        Self::promote_previous_backup(&backup_dir);

        println!(
            "\n{} Successfully rolled back {} to version {}.\n",
            c("::", &[Colors::BOLD, Colors::GREEN]),
            pkg.name,
            restored.version
        );
        Ok(true)
    }
}
