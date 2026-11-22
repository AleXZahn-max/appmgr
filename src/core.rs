use crate::config::Config;
use crate::db::{Database, PackageRecord};
use crate::desktop::run_system_triggers;
use crate::formats::appimage::AppImagePackage;
use crate::formats::archive::ArchivePackage;
use crate::formats::binary::BinaryPackage;
use crate::formats::deb::DebPackage;
use crate::formats::{detect_format, PackageFormat};
use crate::github::{CandidateAsset, GitHubClient, Release, ReleaseAsset, RepoItem};
use crate::ui::{
    apt_prefix, apt_preflight_animation, c, confirm_dialog, print_table, Colors,
    Spinner,
};
use reqwest::StatusCode;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Core {
    pub config: Config,
    pub db: Database,
    pub github: GitHubClient,
}

impl Core {
    pub fn new(config: &Config) -> Self {
        let _ = config.ensure_dirs();
        let db = Database::load(&config.db_file);
        Self {
            config: config.clone(),
            db,
            github: GitHubClient::new(),
        }
    }

    pub async fn install(
        &mut self,
        target_str: &str,
        force: bool,
        assume_yes: bool,
        repo: Option<String>,
        prefer: Option<&str>,
        include_pre: bool,
    ) -> Result<bool, String> {
        let local_path = Path::new(target_str);
        if local_path.is_file() {
            return self.install_local_file(
                local_path,
                force,
                assume_yes,
                repo,
                None,
                None,
                None,
                None,
                None,
                false,
            );
        }

        let (parsed_repo, tag) = GitHubClient::parse_repo_and_tag(target_str);
        if target_str.starts_with("github:")
            || target_str.starts_with("http://")
            || target_str.starts_with("https://")
            || (target_str.contains('/') && parsed_repo.is_some())
        {
            let repo_name = parsed_repo
                .ok_or_else(|| format!("Invalid GitHub repository format: {}", target_str))?;
            return self
                .install_from_github(
                    &repo_name,
                    tag.as_deref(),
                    force,
                    assume_yes,
                    prefer,
                    include_pre,
                )
                .await;
        }

        self.search_and_install(target_str, force, assume_yes, prefer, include_pre)
            .await
    }

    pub async fn install_from_github(
        &mut self,
        repo: &str,
        tag: Option<&str>,
        force: bool,
        assume_yes: bool,
        prefer: Option<&str>,
        include_pre: bool,
    ) -> Result<bool, String> {
        let tag_disp = tag
            .map(|t| format!(" ({})", c(t, &[Colors::BOLD, Colors::BRIGHT_YELLOW])))
            .unwrap_or_default();
        println!(
            "{} Fetching release metadata from GitHub: {}{}",
            c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
            c(repo, &[Colors::BOLD, Colors::BRIGHT_CYAN]),
            tag_disp
        );
        apt_preflight_animation();

        let spinner = Spinner::new(&format!("Querying GitHub API for {}...", repo));
        let (status, release_opt, etag, is_pre) = self
            .github
            .get_release(repo, tag, include_pre, None)
            .await?;
        spinner.stop();

        if status == StatusCode::NOT_FOUND || release_opt.is_none() {
            println!(
                "{} No suitable release found for repository '{}'.",
                apt_prefix("FAIL"),
                repo
            );
            return Ok(false);
        }

        let release = release_opt.unwrap();
        let tag_name = release.tag_name.clone();

        if is_pre && !include_pre && tag.is_none() && !assume_yes {
            println!(
                "\n{} Only pre-release builds found for {} ({}).",
                apt_prefix("INFO"),
                repo,
                c(&tag_name, &[Colors::BOLD, Colors::BRIGHT_YELLOW])
            );
            if !confirm_dialog(&format!("Install pre-release {}?", tag_name), true) {
                println!("{} Installation aborted.", c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]));
                return Ok(false);
            }
        }

        let app_name_hint = repo.split('/').next_back().unwrap_or(repo);
        let candidates = self
            .github
            .filter_assets(&release.assets, prefer, None, Some(app_name_hint));

        if candidates.is_empty() {
            println!(
                "{} No compatible binary assets found in {} ({}) for {}.",
                apt_prefix("FAIL"),
                repo,
                tag_name,
                std::env::consts::ARCH
            );
            return Ok(false);
        }

        self.download_and_install_asset(repo, &release, &candidates, etag, force, assume_yes)
            .await
    }

    pub async fn search_and_install(
        &mut self,
        query: &str,
        force: bool,
        assume_yes: bool,
        prefer: Option<&str>,
        include_pre: bool,
    ) -> Result<bool, String> {
        println!(
            "{} Searching GitHub repositories for '{}'...",
            c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
            c(query, &[Colors::BOLD, Colors::BRIGHT_CYAN])
        );

        let spinner = Spinner::new(&format!("Querying GitHub Search API for '{}'...", query));
        let repos = self.github.search_repositories(query, 8).await?;
        spinner.stop();

        if repos.is_empty() {
            println!(
                "{} No repositories found matching '{}'.",
                apt_prefix("FAIL"),
                query
            );
            return Ok(false);
        }

        let remaining_repos = repos;
        while !remaining_repos.is_empty() {
            let chosen = Self::prompt_select_repository(&remaining_repos, query);
            let repo_item = match chosen {
                Some(r) => r,
                None => {
                    println!("{} Installation cancelled.", c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]));
                    return Ok(false);
                }
            };

            let repo_name = repo_item.full_name.clone();
            println!(
                "\n{} Checking releases for {}...",
                c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
                c(&repo_name, &[Colors::BOLD, Colors::BRIGHT_CYAN])
            );

            let spinner = Spinner::new(&format!("Fetching release for {}...", repo_name));
            let release_res = self
                .github
                .get_release(&repo_name, None, include_pre, None)
                .await;
            spinner.stop();

            let (status, release_opt, etag, _) = match release_res {
                Ok(res) => res,
                Err(e) => {
                    println!(" {} Error checking {}: {}", apt_prefix("WARN"), repo_name, e);
                    (StatusCode::INTERNAL_SERVER_ERROR, None, None, false)
                }
            };

            if status == StatusCode::NOT_FOUND || release_opt.is_none() {
                println!(
                    " {} Repository '{}' has no releases.\n        Please select another repository.\n",
                    apt_prefix("WARN"),
                    c(&repo_name, &[Colors::BOLD, Colors::YELLOW])
                );
                continue;
            }

            let release = release_opt.unwrap();
            let app_hint = repo_name.split('/').next_back().unwrap_or(&repo_name);
            let candidates = self
                .github
                .filter_assets(&release.assets, prefer, None, Some(app_hint));

            if candidates.is_empty() {
                println!(
                    " {} Repository '{}' ({}) has no compatible assets for {}.\n        Please select another repository.\n",
                    apt_prefix("WARN"),
                    c(&repo_name, &[Colors::BOLD, Colors::YELLOW]),
                    release.tag_name,
                    std::env::consts::ARCH
                );
                continue;
            }

            apt_preflight_animation();
            return self
                .download_and_install_asset(&repo_name, &release, &candidates, etag, force, assume_yes)
                .await;
        }

        println!("{} No suitable repositories remaining.", apt_prefix("FAIL"));
        Ok(false)
    }

    async fn download_and_install_asset(
        &mut self,
        repo: &str,
        release: &Release,
        candidates: &[CandidateAsset],
        etag: Option<String>,
        force: bool,
        assume_yes: bool,
    ) -> Result<bool, String> {
        let chosen = if candidates.len() == 1 || assume_yes {
            &candidates[0]
        } else {
            match Self::prompt_select_asset(candidates) {
                Some(c) => c,
                None => {
                    println!("{} Installation aborted.", c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]));
                    return Ok(false);
                }
            }
        };

        let temp_dir = tempfile::tempdir()
            .map_err(|e| format!("Failed to create temporary directory: {}", e))?;
        let local_archive = temp_dir.path().join(&chosen.name);

        println!();
        self.github
            .download_asset(
                &chosen.url,
                &local_archive,
                chosen.size,
                &chosen.name,
                Some(chosen),
                Some(&release.assets),
            )
            .await?;

        let pattern = GitHubClient::derive_pattern(&chosen.name, &release.tag_name);

        self.install_local_file(
            &local_archive,
            force,
            assume_yes,
            Some(repo.to_string()),
            Some(release.tag_name.clone()),
            etag,
            Some(pattern),
            None,
            None,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn install_local_file(
        &mut self,
        path: &Path,
        force: bool,
        assume_yes: bool,
        repo: Option<String>,
        github_tag: Option<String>,
        github_etag: Option<String>,
        asset_pattern: Option<String>,
        env: Option<HashMap<String, String>>,
        args: Option<Vec<String>>,
        pinned: bool,
    ) -> Result<bool, String> {
        let fmt = match detect_format(path) {
            Some(f) => f,
            None => {
                println!(
                    "{} Unsupported package format. Supported: .AppImage, .deb, .tar.*, .zip, standalone ELF",
                    apt_prefix("FAIL")
                );
                return Ok(false);
            }
        };

        if fmt == PackageFormat::ArchNative {
            println!(
                "{} '{}' is an Arch Linux native package (.pkg.tar.*).",
                apt_prefix("INFO"),
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            println!("        To install it into your system, run:");
            println!(
                "        {}\n",
                c(&format!("sudo pacman -U {}", path.display()), &[Colors::BOLD, Colors::BRIGHT_YELLOW])
            );
            return Ok(false);
        }

        apt_preflight_animation();

        let (name, version, _desc, size) = match fmt {
            PackageFormat::Deb => {
                let p = DebPackage::new(path, &self.config, repo.clone(), github_tag.clone());
                let (id, v, d, s, _) = p.read_metadata()?;
                (id, v, d, s)
            }
            PackageFormat::AppImage => {
                let p = AppImagePackage::new(path, &self.config, repo.clone(), github_tag.clone());
                p.read_metadata()?
            }
            PackageFormat::Tar | PackageFormat::Zip => {
                let p = ArchivePackage::new(path, &self.config, repo.clone(), github_tag.clone())?;
                p.read_metadata()?
            }
            PackageFormat::Binary => {
                let p = BinaryPackage::new(path, &self.config, repo.clone(), github_tag.clone());
                p.read_metadata()?
            }
            PackageFormat::ArchNative => unreachable!(),
        };

        // Transaction Summary
        println!(
            "\nPackages (1) {} [{}]",
            c(&format!("{}-{} [{}]", name, version, fmt.as_str().to_uppercase()), &[Colors::BOLD, Colors::BRIGHT_WHITE]),
            c("NEW", &[Colors::BOLD, Colors::BRIGHT_GREEN])
        );
        let mb = size as f64 / (1024.0 * 1024.0);
        println!("Total Installed Size:   {:.2} MiB\n", mb);

        if !assume_yes
            && !confirm_dialog("Proceed with installation?", true) {
                println!("{} Installation aborted by user.", c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]));
                return Ok(false);
            }

        let start_time = std::time::Instant::now();
        println!(
            "\nSelecting previously unselected package {}.",
            c(&name, &[Colors::BOLD, Colors::BRIGHT_WHITE])
        );
        println!(
            "(Reading database ... {} packages currently installed.)",
            self.db.all().len()
        );
        println!(
            "Preparing to unpack {} ...",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        println!(
            "Unpacking {} ({}) ...",
            c(&name, &[Colors::BOLD, Colors::BRIGHT_CYAN]),
            version
        );

        let mut record = match fmt {
            PackageFormat::Deb => {
                let p = DebPackage::new(path, &self.config, repo.clone(), github_tag.clone());
                p.install(force, env, args)?
            }
            PackageFormat::AppImage => {
                let p = AppImagePackage::new(path, &self.config, repo.clone(), github_tag.clone());
                p.install(force, env, args)?
            }
            PackageFormat::Tar | PackageFormat::Zip => {
                let p = ArchivePackage::new(path, &self.config, repo.clone(), github_tag.clone())?;
                p.install(force, env, args)?
            }
            PackageFormat::Binary => {
                let p = BinaryPackage::new(path, &self.config, repo.clone(), github_tag.clone());
                p.install(force, env, args)?
            }
            PackageFormat::ArchNative => unreachable!(),
        };

        if let Some(r) = repo {
            record.repo = GitHubClient::parse_repo(&r).or(Some(r));
            record.tag = github_tag;
            record.etag = github_etag;
            record.asset_pattern = asset_pattern;
        }
        record.pinned = pinned;

        println!(
            "Setting up {} ({}) ...",
            c(&record.name, &[Colors::BOLD, Colors::BRIGHT_GREEN]),
            record.version
        );
        for b in &record.binaries {
            println!(
                " {} Installed launcher: {}",
                apt_prefix("OK"),
                c(&b.display().to_string(), &[Colors::BOLD, Colors::BRIGHT_WHITE])
            );
        }
        for d in &record.desktop_files {
            println!(
                " {} Installed desktop entry: {}",
                apt_prefix("OK"),
                c(&d.display().to_string(), &[Colors::DIM, Colors::WHITE])
            );
        }
        for i in &record.icons {
            println!(
                " {} Installed icon: {}",
                apt_prefix("OK"),
                c(&i.display().to_string(), &[Colors::DIM, Colors::WHITE])
            );
        }

        run_system_triggers(&self.config.applications_dir, &self.config.icons_dir);

        let pkg_id = record.id.clone();
        self.db
            .add(record)
            .map_err(|e| format!("Failed to update database: {}", e))?;

        let elapsed = start_time.elapsed().as_secs_f64();
        println!(
            "\n{} Transaction completed successfully in {:.2}s.",
            c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
            elapsed
        );
        println!(
            "   You can now launch it by typing: {}\n",
            c(&pkg_id, &[Colors::BOLD, Colors::BRIGHT_YELLOW])
        );
        Ok(true)
    }

    pub async fn search(&self, query: &str) -> Result<(), String> {
        println!(
            "{} Searching GitHub for '{}'...",
            c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
            c(query, &[Colors::BOLD, Colors::BRIGHT_CYAN])
        );

        let spinner = Spinner::new(&format!("Querying GitHub Search API for '{}'...", query));
        let repos = self.github.search_repositories(query, 8).await?;
        spinner.stop();

        if repos.is_empty() {
            println!(
                "{} No repositories found matching '{}'.",
                apt_prefix("FAIL"),
                query
            );
            return Ok(());
        }

        println!(
            "\n{} Found {} repositories for '{}':\n",
            c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
            repos.len(),
            c(query, &[Colors::BOLD, Colors::BRIGHT_CYAN])
        );

        for (i, repo) in repos.iter().enumerate() {
            let desc = repo.description.as_deref().unwrap_or("No description provided");
            println!(
                "  {} {} {} {}",
                c(&format!("{}.", i + 1), &[Colors::BOLD, Colors::BRIGHT_YELLOW]),
                c(&repo.full_name, &[Colors::BOLD, Colors::BRIGHT_WHITE]),
                c(&format!("★ {}", repo.stargazers_count), &[Colors::BOLD, Colors::YELLOW]),
                c(&format!("- {}", desc), &[Colors::DIM, Colors::WHITE])
            );
        }
        println!();
        Ok(())
    }

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

        let parsed_repo = GitHubClient::parse_repo(repo_str).ok_or_else(|| {
            format!("Invalid repository format: '{}'. Use owner/repo.", repo_str)
        })?;

        let spinner = Spinner::new(&format!("Validating GitHub repository {}...", parsed_repo));
        let (status, release_opt, etag, _) =
            self.github.get_release(&parsed_repo, None, false, None).await?;
        spinner.stop();

        if status == StatusCode::NOT_FOUND || release_opt.is_none() {
            println!(
                "{} Repository '{}' has no releases yet.",
                apt_prefix("WARN"),
                parsed_repo
            );
            let _ = self.db.link_repo(&pkg.id, parsed_repo.clone(), None, None, None);
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
                    } else {
                        println!(
                            "{} Package '{}' is not managed by appmgr.",
                            apt_prefix("FAIL"),
                            q
                        );
                    }
                    return Ok(false);
                }
            }
        } else {
            self.db.all().into_iter().cloned().collect()
        };

        let linked_pkgs: Vec<PackageRecord> = target_pkgs
            .into_iter()
            .filter(|p| p.repo.is_some())
            .collect();

        if linked_pkgs.is_empty() {
            if as_json {
                println!("{}", json!({"upgrades": [], "up_to_date": [], "pinned": []}));
                return Ok(true);
            }
            if let Some(q) = app_query {
                println!(
                    "{} Package '{}' is not linked to any GitHub repository.",
                    apt_prefix("WARN"),
                    q
                );
            } else {
                println!(
                    "{} No installed packages are linked to GitHub repositories.",
                    c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE])
                );
            }
            return Ok(false);
        }

        if !as_json {
            println!(
                "{} {}",
                c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
                c("Synchronizing package databases...", &[Colors::BOLD, Colors::BRIGHT_WHITE])
            );
            apt_preflight_animation();
        }

        // Concurrent multi-threaded check via tokio tasks (semaphore max 8)
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
        let client = self.github.clone();
        let prefer_owned = prefer.map(String::from);

        let mut tasks = Vec::new();
        for pkg in linked_pkgs {
            let sem = semaphore.clone();
            let gh = client.clone();
            let pref = prefer_owned.clone();
            let force_flag = force;
            let single_app = app_query.is_some();

            tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let repo = pkg.repo.as_ref().unwrap();
                let is_pinned = pkg.pinned;

                if is_pinned && !force_flag && !single_app {
                    return UpgradeCheckResult::Pinned(pkg);
                }

                let etag_ref = pkg.etag.as_deref();
                let release_res = gh.get_release(repo, None, include_pre, etag_ref).await;

                match release_res {
                    Ok((StatusCode::NOT_MODIFIED, _, _, _)) => UpgradeCheckResult::UpToDate(pkg, true),
                    Ok((StatusCode::OK, Some(release), new_etag, _)) => {
                        let new_tag = release.tag_name.clone();
                        let current_tag = pkg.tag.as_deref().unwrap_or("");
                        let current_ver = &pkg.version;

                        let is_newer = GitHubClient::compare_versions(&new_tag, current_ver)
                            == std::cmp::Ordering::Greater
                            || (new_tag != current_tag
                                && !current_tag.is_empty()
                                && GitHubClient::compare_versions(&new_tag, current_ver)
                                    != std::cmp::Ordering::Less);

                        if is_newer {
                            let pref_format = pref.as_deref().unwrap_or(&pkg.format);
                            let candidates = gh.filter_assets(
                                &release.assets,
                                Some(pref_format),
                                pkg.asset_pattern.as_deref(),
                                None,
                            );
                            if let Some(chosen) = candidates.into_iter().next() {
                                let pattern = pkg.asset_pattern.clone().unwrap_or_else(|| {
                                    GitHubClient::derive_pattern(&chosen.name, &new_tag)
                                });
                                return UpgradeCheckResult::Upgrade(
                                    pkg,
                                    new_tag,
                                    new_etag,
                                    chosen,
                                    release.assets,
                                    pattern,
                                );
                            }
                        }
                        UpgradeCheckResult::UpToDate(pkg, false)
                    }
                    _ => UpgradeCheckResult::UpToDate(pkg, false),
                }
            }));
        }

        let mut upgrades = Vec::new();
        let mut up_to_date = Vec::new();
        let mut pinned = Vec::new();

        for task in tasks {
            if let Ok(res) = task.await {
                match res {
                    UpgradeCheckResult::Upgrade(pkg, new_tag, new_etag, asset, assets, pat) => {
                        upgrades.push((pkg, new_tag, new_etag, asset, assets, pat));
                    }
                    UpgradeCheckResult::UpToDate(pkg, is_304) => {
                        up_to_date.push((pkg, is_304));
                    }
                    UpgradeCheckResult::Pinned(pkg) => {
                        pinned.push(pkg);
                    }
                }
            }
        }

        if as_json {
            let json_out = json!({
                "upgrades": upgrades.iter().map(|(p, tag, _, asset, _, _)| {
                    json!({
                        "id": p.id,
                        "name": p.name,
                        "current_version": p.version,
                        "latest_version": tag.trim_start_matches('v'),
                        "repo": p.repo,
                        "format": asset.format,
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
                c(&p.name, &[Colors::BOLD, Colors::BRIGHT_MAGENTA]),
                p.version,
                c("[PINNED]", &[Colors::BOLD, Colors::YELLOW])
            );
        }

        for (ut, is_304) in &up_to_date {
            let note = if *is_304 {
                format!(" {}", c("(304 Not Modified)", &[Colors::DIM, Colors::WHITE]))
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

        for (u, new_tag, _, _, _, _) in &upgrades {
            println!(
                " {} {}: update available ({} -> {}) [{}]",
                apt_prefix("INFO"),
                c(&u.name, &[Colors::BOLD, Colors::BRIGHT_GREEN]),
                u.version,
                c(new_tag.trim_start_matches('v'), &[Colors::BOLD, Colors::BRIGHT_YELLOW]),
                u.repo.as_deref().unwrap_or("-")
            );
        }

        if check_only {
            println!();
            if !upgrades.is_empty() {
                println!(
                    "{} {} package(s) can be upgraded. Run {} to apply updates.\n",
                    c("::", &[Colors::BOLD, Colors::BRIGHT_YELLOW]),
                    upgrades.len(),
                    c("appmgr upgrade", &[Colors::BOLD, Colors::BRIGHT_GREEN])
                );
            } else {
                println!(
                    "{} All packages are up to date.\n",
                    c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN])
                );
            }
            return Ok(true);
        }

        if upgrades.is_empty() {
            println!(
                "\n{} All packages are up to date. Nothing to do.\n",
                c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN])
            );
            return Ok(true);
        }

        if !assume_yes
            && !confirm_dialog("Proceed with upgrade?", true) {
                println!("{} Upgrade cancelled by user.", c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]));
                return Ok(false);
            }

        let mut success_count = 0;
        for (pkg, new_tag, new_etag, asset_info, release_assets, pattern) in upgrades {
            println!(
                "\n{} Upgrading {} ({} -> {}) ...",
                c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
                c(&pkg.name, &[Colors::BOLD, Colors::BRIGHT_CYAN]),
                pkg.version,
                new_tag
            );

            let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
            let downloaded_file = temp_dir.path().join(&asset_info.name);

            // Rollback backup
            let backup_dir = self.config.backups_dir.join(&pkg.id);
            if pkg.install_dir.exists() {
                let _ = fs::remove_dir_all(&backup_dir);
                let _ = fs::create_dir_all(&backup_dir);
                let backup_files = backup_dir.join("files");
                let _ = ArchivePackage::copy_dir_all(&pkg.install_dir, &backup_files);
                let _ = fs::write(
                    backup_dir.join("meta.json"),
                    serde_json::to_string_pretty(&pkg).unwrap(),
                );
            }

            let dl_res = self
                .github
                .download_asset(
                    &asset_info.url,
                    &downloaded_file,
                    asset_info.size,
                    &asset_info.name,
                    Some(&asset_info),
                    Some(&release_assets),
                )
                .await;

            if let Err(e) = dl_res {
                println!("{} Download failed: {}", apt_prefix("FAIL"), e);
                continue;
            }

            let ok = self.install_local_file(
                &downloaded_file,
                true,
                true,
                pkg.repo.clone(),
                Some(new_tag.clone()),
                new_etag,
                Some(pattern),
                Some(pkg.env.clone()),
                Some(pkg.args.clone()),
                pkg.pinned,
            );

            match ok {
                Ok(true) => {
                    success_count += 1;
                    println!(
                        " {} {} upgraded to {}",
                        apt_prefix("OK"),
                        pkg.name,
                        c(&new_tag, &[Colors::BOLD, Colors::BRIGHT_GREEN])
                    );
                }
                _ => {
                    println!(
                        "{} Upgrade failed. Rolling back...",
                        apt_prefix("FAIL")
                    );
                    let backup_files = backup_dir.join("files");
                    if backup_files.exists() {
                        let _ = fs::remove_dir_all(&pkg.install_dir);
                        let _ = ArchivePackage::copy_dir_all(&backup_files, &pkg.install_dir);
                    }
                }
            }
        }

        println!(
            "\n{} Upgrade complete. {} packages upgraded successfully.\n",
            c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
            success_count
        );
        Ok(success_count > 0)
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
            c("::", &[Colors::BOLD, Colors::BRIGHT_YELLOW]),
            c(&pkg.name, &[Colors::BOLD, Colors::BRIGHT_WHITE])
        );
        println!(
            "   Current version:  {}",
            c(&pkg.version, &[Colors::BOLD, Colors::BRIGHT_RED])
        );
        println!(
            "   Rollback version: {}",
            c(&prev_record.version, &[Colors::BOLD, Colors::BRIGHT_GREEN])
        );

        if !assume_yes
            && !confirm_dialog(
                &format!("Are you sure you want to rollback {} to {}?", pkg.name, prev_record.version),
                true,
            ) {
                println!("{} Rollback cancelled.", c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]));
                return Ok(false);
            }

        let _ = fs::remove_dir_all(&pkg.install_dir);
        ArchivePackage::copy_dir_all(&files_dir, &pkg.install_dir)?;

        self.db.add(prev_record.clone()).map_err(|e| e.to_string())?;
        run_system_triggers(&self.config.applications_dir, &self.config.icons_dir);
        let _ = fs::remove_dir_all(&backup_dir);

        println!(
            "\n{} Successfully rolled back {} to version {}.\n",
            c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
            pkg.name,
            prev_record.version
        );
        Ok(true)
    }

    pub fn clean(&self, assume_yes: bool) -> Result<bool, String> {
        let mut backup_bytes = 0;
        if self.config.backups_dir.exists() {
            for e in walkdir::WalkDir::new(&self.config.backups_dir).into_iter().flatten() {
                if e.file_type().is_file() {
                    backup_bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
        }

        let mut temp_bytes = 0;
        let mut tmp_dirs = Vec::new();
        if let Ok(entries) = fs::read_dir("/tmp") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("appmgr_") {
                    let p = entry.path();
                    if p.is_dir() {
                        for se in walkdir::WalkDir::new(&p).into_iter().flatten() {
                            if se.file_type().is_file() {
                                temp_bytes += se.metadata().map(|m| m.len()).unwrap_or(0);
                            }
                        }
                        tmp_dirs.push(p);
                    }
                }
            }
        }

        let total_bytes = backup_bytes + temp_bytes;
        let total_mb = total_bytes as f64 / (1024.0 * 1024.0);

        println!("{} AppMgr Cache Cleanup:", c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]));
        println!("   Rollback backups:   {:.1} MB", backup_bytes as f64 / (1024.0 * 1024.0));
        println!("   Temporary files:    {:.1} MB", temp_bytes as f64 / (1024.0 * 1024.0));
        println!(
            "   Estimated space:    {}",
            c(&format!("{:.1} MB", total_mb), &[Colors::BOLD, Colors::BRIGHT_GREEN])
        );

        if total_bytes == 0 {
            println!(
                "\n{} Cache is already clean. Nothing to do.\n",
                c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN])
            );
            return Ok(true);
        }

        if !assume_yes
            && !confirm_dialog("Proceed with cleanup?", true) {
                println!("{} Cleanup cancelled.", c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]));
                return Ok(false);
            }

        if self.config.backups_dir.exists() {
            let _ = fs::remove_dir_all(&self.config.backups_dir);
            let _ = fs::create_dir_all(&self.config.backups_dir);
        }
        for td in tmp_dirs {
            let _ = fs::remove_dir_all(td);
        }

        println!(
            "\n{} Cache cleaned successfully. Freed {:.1} MB.\n",
            c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
            total_mb
        );
        Ok(true)
    }

    pub fn env_manage(
        &mut self,
        app_id: &str,
        pairs: &[String],
        unset_key: Option<&str>,
        list_only: bool,
    ) -> Result<bool, String> {
        let pkg = self
            .db
            .get(app_id)
            .ok_or_else(|| format!("Package '{}' is not managed by appmgr.", app_id))?
            .clone();

        if list_only || (pairs.is_empty() && unset_key.is_none()) {
            println!(
                "\n{} {}:",
                c("Environment variables for", &[Colors::BOLD, Colors::BRIGHT_WHITE]),
                c(&pkg.name, &[Colors::BOLD, Colors::BRIGHT_CYAN])
            );
            if pkg.env.is_empty() {
                println!("  No custom environment variables set.");
            } else {
                for (k, v) in &pkg.env {
                    println!("  {}={}", c(k, &[Colors::BOLD, Colors::BRIGHT_YELLOW]), v);
                }
            }
            println!();
            return Ok(true);
        }

        if let Some(key) = unset_key {
            self.db.unset_env(&pkg.id, key).map_err(|e| e.to_string())?;
            if let Some(updated) = self.db.get(&pkg.id) {
                if let Some(bin) = updated.binaries.first() {
                    let _ = Self::update_wrapper_script(bin, &updated.env, &updated.args);
                }
            }
            println!(
                " {} Unset {} for {}.",
                apt_prefix("OK"),
                c(key, &[Colors::BOLD, Colors::BRIGHT_YELLOW]),
                pkg.name
            );
            return Ok(true);
        }

        let mut new_vars = HashMap::new();
        for p in pairs {
            if let Some((k, v)) = p.split_once('=') {
                new_vars.insert(k.trim().to_string(), v.trim().to_string());
            } else {
                println!(
                    " {} Ignoring invalid pair: '{}' (expected KEY=VALUE)",
                    apt_prefix("WARN"),
                    p
                );
            }
        }

        if !new_vars.is_empty() {
            let keys: Vec<String> = new_vars.keys().cloned().collect();
            self.db.set_env(&pkg.id, new_vars).map_err(|e| e.to_string())?;
            if let Some(updated) = self.db.get(&pkg.id) {
                if let Some(bin) = updated.binaries.first() {
                    let _ = Self::update_wrapper_script(bin, &updated.env, &updated.args);
                }
            }
            println!(
                " {} Updated environment variables for {}: {}",
                apt_prefix("OK"),
                pkg.name,
                keys.join(", ")
            );
        }
        Ok(true)
    }

    pub fn args_manage(
        &mut self,
        app_id: &str,
        args_list: &[String],
        clear: bool,
        list_only: bool,
    ) -> Result<bool, String> {
        let pkg = self
            .db
            .get(app_id)
            .ok_or_else(|| format!("Package '{}' is not managed by appmgr.", app_id))?
            .clone();

        if list_only || (args_list.is_empty() && !clear) {
            println!(
                "\n{} {}:",
                c("Persistent launch arguments for", &[Colors::BOLD, Colors::BRIGHT_WHITE]),
                c(&pkg.name, &[Colors::BOLD, Colors::BRIGHT_CYAN])
            );
            if pkg.args.is_empty() {
                println!("  No persistent arguments configured.");
            } else {
                println!("  {}", c(&pkg.args.join(" "), &[Colors::BOLD, Colors::BRIGHT_YELLOW]));
            }
            println!();
            return Ok(true);
        }

        if clear {
            self.db.clear_args(&pkg.id).map_err(|e| e.to_string())?;
            if let Some(updated) = self.db.get(&pkg.id) {
                if let Some(bin) = updated.binaries.first() {
                    let _ = Self::update_wrapper_script(bin, &updated.env, &updated.args);
                }
            }
            println!(" {} Cleared persistent arguments for {}.", apt_prefix("OK"), pkg.name);
            return Ok(true);
        }

        if !args_list.is_empty() {
            self.db
                .set_args(&pkg.id, args_list.to_vec())
                .map_err(|e| e.to_string())?;
            if let Some(updated) = self.db.get(&pkg.id) {
                if let Some(bin) = updated.binaries.first() {
                    let _ = Self::update_wrapper_script(bin, &updated.env, &updated.args);
                }
            }
            println!(
                " {} Updated launch arguments for {}: {}",
                apt_prefix("OK"),
                pkg.name,
                c(&args_list.join(" "), &[Colors::BOLD, Colors::BRIGHT_YELLOW])
            );
        }
        Ok(true)
    }

    pub fn update_wrapper_script(
        launcher_path: &Path,
        env: &HashMap<String, String>,
        args: &[String],
    ) -> Result<(), String> {
        if !launcher_path.is_file() {
            return Ok(());
        }

        let mut content = fs::read_to_string(launcher_path).map_err(|e| e.to_string())?;

        let args_block = if args.is_empty() {
            "PERSISTENT_ARGS=()\n".to_string()
        } else {
            let quoted: Vec<String> = args
                .iter()
                .map(|a| format!("    \"{}\"", a.replace('"', "\\\"")))
                .collect();
            format!("PERSISTENT_ARGS=(\n{}\n)\n", quoted.join("\n"))
        };

        let re_args = regex::Regex::new(r"(?s)PERSISTENT_ARGS=\(.*?\)\n?").map_err(|e| e.to_string())?;
        if re_args.is_match(&content) {
            content = re_args.replace(&content, &args_block).to_string();
        } else if let Some(pos) = content.rfind("exec ") {
            content.insert_str(pos, &format!("{}\n", args_block));
        } else {
            content.push_str(&format!("\n{}\n", args_block));
        }

        if !content.contains("${PERSISTENT_ARGS[@]}") {
            content = content.replace("\"$@\"", "\"${PERSISTENT_ARGS[@]}\" \"$@\"");
        }

        let standard_exports = [
            "PATH",
            "LD_LIBRARY_PATH",
            "XDG_DATA_DIRS",
            "APPIMAGE_EXTRACT_AND_RUN",
            "CONTAINER_BIN",
            "TARGET_BIN",
        ];
        let lines: Vec<&str> = content.lines().collect();
        let mut filtered_lines = Vec::new();
        for line in lines {
            let trimmed = line.trim();
            if let Some(var_part) = trimmed.strip_prefix("export ") {
                let var_name = var_part.split('=').next().unwrap_or("").trim();
                if !standard_exports.contains(&var_name) {
                    continue;
                }
            }
            filtered_lines.push(line);
        }

        let mut new_content = filtered_lines.join("\n");
        if !env.is_empty() {
            let mut env_lines = Vec::new();
            for (k, v) in env {
                env_lines.push(format!("export {}=\"{}\"", k, v.replace('"', "\\\"")));
            }
            let env_block = format!("\n{}\n", env_lines.join("\n"));
            if let Some(pos) = new_content.find("PERSISTENT_ARGS=") {
                new_content.insert_str(pos, &env_block);
            } else {
                new_content.push_str(&env_block);
            }
        }

        fs::write(launcher_path, new_content).map_err(|e| e.to_string())?;
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(launcher_path) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o755);
            let _ = fs::set_permissions(launcher_path, perms);
        }

        Ok(())
    }

    pub fn pin_app(&mut self, app_id: &str) -> Result<bool, String> {
        let pkg = self
            .db
            .get(app_id)
            .ok_or_else(|| format!("Package '{}' is not managed by appmgr.", app_id))?
            .clone();

        self.db.set_pinned(&pkg.id, true).map_err(|e| e.to_string())?;
        println!(
            " {} Application '{}' pinned to version {} {}. General upgrades will skip it.",
            apt_prefix("OK"),
            c(&pkg.name, &[Colors::BOLD, Colors::BRIGHT_WHITE]),
            pkg.version,
            c("[PINNED]", &[Colors::BOLD, Colors::BRIGHT_YELLOW])
        );
        Ok(true)
    }

    pub fn unpin_app(&mut self, app_id: &str) -> Result<bool, String> {
        let pkg = self
            .db
            .get(app_id)
            .ok_or_else(|| format!("Package '{}' is not managed by appmgr.", app_id))?
            .clone();

        self.db.set_pinned(&pkg.id, false).map_err(|e| e.to_string())?;
        println!(
            " {} Application '{}' unpinned. Updates enabled.",
            apt_prefix("OK"),
            c(&pkg.name, &[Colors::BOLD, Colors::BRIGHT_WHITE])
        );
        Ok(true)
    }

    pub fn export_manifest(&self, output_path: Option<&str>) -> Result<bool, String> {
        let all_pkgs = self.db.all();
        let manifest_data = json!({
            "appmgr_version": env!("CARGO_PKG_VERSION"),
            "export_date": chrono::Local::now().to_rfc3339(),
            "applications": all_pkgs.iter().map(|p| {
                json!({
                    "id": p.id,
                    "name": p.name,
                    "version": p.version,
                    "format": p.format,
                    "repo": p.repo,
                    "pinned": p.pinned,
                    "env": p.env,
                    "args": p.args,
                    "is_local": p.repo.is_none(),
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
            c(&out_file.display().to_string(), &[Colors::BOLD, Colors::BRIGHT_YELLOW])
        );
        Ok(true)
    }

    pub async fn import_manifest(&mut self, manifest_path: &str, assume_yes: bool) -> Result<bool, String> {
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
            let repo = app["repo"].as_str();

            if self.db.get(id).is_some() {
                println!(" {} {} is already installed, skipping.", apt_prefix("OK"), name);
                skipped_count += 1;
                continue;
            }

            let repo_str = match repo {
                Some(r) if !r.is_empty() => r,
                _ => {
                    local_apps.push(app);
                    continue;
                }
            };

            println!(
                "\n{} Installing {} from {} ...",
                c("::", &[Colors::BOLD, Colors::BRIGHT_CYAN]),
                name,
                repo_str
            );

            let ok = self
                .install_from_github(repo_str, None, true, assume_yes, None, false)
                .await?;
            if ok {
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

    pub fn list_installed(&self, filter_type: Option<&str>, as_json: bool) {
        let mut pkgs: Vec<&PackageRecord> = self.db.all();
        if let Some(ft) = filter_type {
            let lower = ft.to_lowercase();
            pkgs.retain(|p| p.format.to_lowercase() == lower);
        }

        if as_json {
            let json_list: Vec<_> = pkgs
                .iter()
                .map(|p| {
                    json!({
                        "id": p.id,
                        "name": p.name,
                        "version": p.version,
                        "format": p.format,
                        "repo": p.repo,
                        "pinned": p.pinned,
                        "total_size_bytes": p.total_size_bytes.unwrap_or(0),
                        "binaries": p.binaries,
                        "env": p.env,
                        "args": p.args,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&json_list).unwrap());
            return;
        }

        let headers = ["PACKAGE", "VERSION", "TYPE", "REPO", "SIZE", "LAUNCHER"];
        let mut rows = Vec::new();

        for p in &pkgs {
            let size_mb = p.total_size_bytes.unwrap_or(0) as f64 / (1024.0 * 1024.0);
            let size_str = format!("{:.1} MB", size_mb);
            let bin_name = p
                .binaries
                .first()
                .and_then(|b| b.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "-".to_string());
            let pkg_type = format!("[{}]", p.format.to_uppercase());
            let repo_str = p.repo.clone().unwrap_or_else(|| "-".to_string());
            let mut ver_str = p.version.clone();
            if p.pinned {
                ver_str = format!("{} {}", ver_str, c("[PINNED]", &[Colors::BOLD, Colors::BRIGHT_MAGENTA]));
            }

            rows.push(vec![
                p.id.clone(),
                ver_str,
                pkg_type,
                repo_str,
                size_str,
                bin_name,
            ]);
        }

        println!(
            "\n{} Managed Applications ({} total):\n",
            c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
            pkgs.len()
        );
        print_table(&headers, &rows);
    }

    pub fn info(&self, target: &str) {
        let path = Path::new(target);
        if path.is_file() {
            println!("\n{}", c("Package Information (Local File):", &[Colors::BOLD, Colors::BRIGHT_WHITE]));
            println!("  Path: {}", path.display());
            if let Some(fmt) = detect_format(path) {
                println!("  Format: [{}]", fmt.as_str().to_uppercase());
            }
            return;
        }

        let pkg = match self.db.get(target) {
            Some(p) => p,
            None => {
                println!("{} Package '{}' not found in database.", apt_prefix("FAIL"), target);
                return;
            }
        };

        println!("\n{}", c("Package Information:", &[Colors::BOLD, Colors::BRIGHT_WHITE]));
        println!("  {:<16} {}", c("Name:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), pkg.name);
        println!("  {:<16} {}", c("ID:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), pkg.id);
        println!("  {:<16} {}", c("Version:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), pkg.version);
        println!("  {:<16} [{}]", c("Format:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), pkg.format.to_uppercase());
        println!("  {:<16} {}", c("Status:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), c("Installed", &[Colors::BRIGHT_GREEN]));

        if pkg.pinned {
            println!(
                "  {:<16} {}",
                c("Pinned:", &[Colors::BOLD, Colors::BRIGHT_MAGENTA]),
                c("Yes (General upgrades disabled)", &[Colors::BOLD, Colors::YELLOW])
            );
        }

        if let Some(repo) = &pkg.repo {
            println!("  {:<16} {}", c("GitHub Repo:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), repo);
            if let Some(tag) = &pkg.tag {
                println!("  {:<16} {}", c("Release Tag:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), tag);
            }
            if let Some(pat) = &pkg.asset_pattern {
                println!("  {:<16} {}", c("Asset Match:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), pat);
            }
        }

        if let Some(desc) = &pkg.description {
            println!("  {:<16} {}", c("Description:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), desc);
        }

        println!("  {:<16} {}", c("Installed On:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), pkg.install_date);
        println!("  {:<16} {}", c("Location:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), pkg.install_dir.display());

        let size_mb = pkg.total_size_bytes.unwrap_or(0) as f64 / (1024.0 * 1024.0);
        println!("  {:<16} {:.2} MB", c("Disk Usage:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), size_mb);

        if !pkg.binaries.is_empty() {
            let bins: Vec<String> = pkg.binaries.iter().map(|b| b.display().to_string()).collect();
            println!("  {:<16} {}", c("Binaries:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), bins.join(", "));
        }
        if !pkg.desktop_files.is_empty() {
            let dts: Vec<String> = pkg.desktop_files.iter().map(|d| d.display().to_string()).collect();
            println!("  {:<16} {}", c("Desktop:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), dts.join(", "));
        }
        if !pkg.env.is_empty() {
            let env_str = pkg.env.iter().map(|(k, v)| format!("{}={}", k, v)).collect::<Vec<_>>().join(", ");
            println!("  {:<16} {}", c("Environment:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), env_str);
        }
        if !pkg.args.is_empty() {
            println!("  {:<16} {}", c("Launch Args:", &[Colors::BOLD, Colors::BRIGHT_CYAN]), pkg.args.join(" "));
        }

        // Check shared library linkages
        self.check_pkg_ldd(pkg);
        println!();
    }

    fn check_pkg_ldd(&self, pkg: &PackageRecord) {
        let app_dir = &pkg.install_dir;
        let mut target_elfs = Vec::new();

        if app_dir.exists() {
            for entry in walkdir::WalkDir::new(app_dir).into_iter().flatten() {
                let p = entry.path();
                if p.is_file() && !p.is_symlink() && ArchivePackage::is_executable_file(p) {
                    target_elfs.push(p.to_path_buf());
                }
            }
        }

        let mut has_dynamic = false;
        let mut missing_all = Vec::new();

        for elf in &target_elfs {
            if let Ok(output) = Command::new("ldd").arg(elf).output() {
                let combined = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                let lower = combined.to_lowercase();
                if lower.contains("not a dynamic executable") || lower.contains("statically linked") {
                    continue;
                }
                has_dynamic = true;
                for line in combined.lines() {
                    if line.contains("not found") {
                        if let Some((lib, _)) = line.split_once("=>") {
                            missing_all.push(lib.trim().to_string());
                        }
                    }
                }
            }
        }

        if !target_elfs.is_empty() && !has_dynamic {
            println!(
                "  {:<16} [static binary] (All libraries statically linked)",
                c("Dependencies:", &[Colors::BOLD, Colors::BRIGHT_GREEN])
            );
        } else if missing_all.is_empty() {
            println!(
                "  {:<16} All shared libraries satisfied",
                c("Dependencies:", &[Colors::BOLD, Colors::BRIGHT_GREEN])
            );
        } else {
            println!(
                "  {:<16} {} missing libraries:",
                c("Dependencies:", &[Colors::BOLD, Colors::BRIGHT_YELLOW]),
                missing_all.len()
            );
            for m in missing_all.iter().take(3) {
                println!("    - {}", c(m, &[Colors::RED]));
            }
        }
    }

    pub fn run_app(&self, app_id: &str, args: &[String]) {
        let pkg = match self.db.get(app_id) {
            Some(p) => p,
            None => {
                println!("{} Package '{}' is not installed.", apt_prefix("FAIL"), app_id);
                return;
            }
        };

        let bin_path = match pkg.binaries.first() {
            Some(b) => b,
            None => {
                println!("{} No executable launcher found for '{}'.", apt_prefix("FAIL"), app_id);
                return;
            }
        };

        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new(bin_path);
        cmd.args(args);
        let err = cmd.exec();
        println!("{} Failed to execute {}: {}", apt_prefix("FAIL"), bin_path.display(), err);
    }

    pub fn remove(&mut self, app_id: &str, assume_yes: bool) -> Result<bool, String> {
        let pkg = match self.db.get(app_id) {
            Some(p) => p.clone(),
            None => {
                println!("{} Package '{}' is not managed by appmgr.", apt_prefix("FAIL"), app_id);
                return Ok(false);
            }
        };

        println!(
            "\nPackages to be removed (1): {}",
            c(&format!("{}-{}", pkg.name, pkg.version), &[Colors::BOLD, Colors::BRIGHT_RED])
        );

        if !assume_yes
            && !confirm_dialog(&format!("Do you want to completely remove {}?", pkg.name), true) {
                println!("{} Removal aborted.", c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]));
                return Ok(false);
            }

        let start_time = std::time::Instant::now();
        println!(
            "\nRemoving {} ({}) ...",
            c(&pkg.name, &[Colors::BOLD, Colors::BRIGHT_RED]),
            pkg.version
        );

        for b in &pkg.binaries {
            let _ = fs::remove_file(b);
        }
        for d in &pkg.desktop_files {
            let _ = fs::remove_file(d);
        }
        for i in &pkg.icons {
            let _ = fs::remove_file(i);
        }
        if pkg.install_dir.exists() {
            let _ = fs::remove_dir_all(&pkg.install_dir);
        }

        self.db.remove(&pkg.id).map_err(|e| e.to_string())?;
        run_system_triggers(&self.config.applications_dir, &self.config.icons_dir);

        let elapsed = start_time.elapsed().as_secs_f64();
        println!(
            "\n{} Package {} removed in {:.2}s.\n",
            c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
            pkg.name,
            elapsed
        );
        Ok(true)
    }

    fn prompt_select_repository<'a>(repos: &'a [RepoItem], query: &str) -> Option<&'a RepoItem> {
        println!(
            "\n{} Found {} repositories for '{}':\n",
            c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
            repos.len(),
            c(query, &[Colors::BOLD, Colors::BRIGHT_CYAN])
        );

        for (i, repo) in repos.iter().enumerate() {
            let desc = repo.description.as_deref().unwrap_or("No description");
            println!(
                "  {} {} {} {}",
                c(&format!("{:2})", i + 1), &[Colors::BOLD, Colors::BRIGHT_YELLOW]),
                c(&repo.full_name, &[Colors::BOLD, Colors::BRIGHT_WHITE]),
                c(&format!("★ {}", repo.stargazers_count), &[Colors::BOLD, Colors::YELLOW]),
                c(&format!("- {}", desc), &[Colors::DIM, Colors::WHITE])
            );
        }

        print!(
            "\n{} Select repository to install (1-{}, 0 to cancel) [1]: ",
            c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
            repos.len()
        );
        let _ = io::stdout().flush();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            return None;
        }
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return repos.first();
        }
        match trimmed.parse::<usize>() {
            Ok(0) => None,
            Ok(idx) if idx >= 1 && idx <= repos.len() => repos.get(idx - 1),
            _ => repos.first(),
        }
    }

    fn prompt_select_asset(candidates: &[CandidateAsset]) -> Option<&CandidateAsset> {
        println!(
            "\n{} Multiple compatible release assets found:\n",
            c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE])
        );

        for (i, c_asset) in candidates.iter().enumerate() {
            let mb = c_asset.size as f64 / (1024.0 * 1024.0);
            println!(
                "  {} {} [{}] ({:.1} MB)",
                c(&format!("{:2})", i + 1), &[Colors::BOLD, Colors::BRIGHT_YELLOW]),
                c(&c_asset.name, &[Colors::BOLD, Colors::BRIGHT_WHITE]),
                c(&c_asset.format.to_uppercase(), &[Colors::BOLD, Colors::BRIGHT_CYAN]),
                mb
            );
        }

        print!(
            "\n{} Select asset to download (1-{}, 0 to cancel) [1]: ",
            c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
            candidates.len()
        );
        let _ = io::stdout().flush();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            return None;
        }
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return candidates.first();
        }
        match trimmed.parse::<usize>() {
            Ok(0) => None,
            Ok(idx) if idx >= 1 && idx <= candidates.len() => candidates.get(idx - 1),
            _ => candidates.first(),
        }
    }
}

#[allow(clippy::large_enum_variant)]
enum UpgradeCheckResult {
    Upgrade(
        PackageRecord,
        String,
        Option<String>,
        CandidateAsset,
        Vec<ReleaseAsset>,
        String,
    ),
    UpToDate(PackageRecord, bool),
    Pinned(PackageRecord),
}
