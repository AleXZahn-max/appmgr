use crate::config::{Config, DepsMode};
use crate::db::{Database, PackageRecord};
use crate::desktop::run_system_triggers;
use crate::formats::appimage::AppImagePackage;
use crate::formats::archive::ArchivePackage;
use crate::formats::binary::BinaryPackage;
use crate::formats::deb::DebPackage;
use crate::formats::{detect_format, PackageFormat};
use crate::github::{CandidateAsset, GitHubClient, Release, ReleaseAsset, RepoItem};
use crate::ui::{
    apt_prefix, apt_prefix_err, c, confirm_dialog, format_card_row, format_card_section,
    print_table, should_color_stdout, truncate_chars, Colors, Spinner,
};
use reqwest::StatusCode;
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;

pub struct Core {
    pub config: Config,
    pub db: Database,
    pub github: GitHubClient,
    pub http_client: reqwest::Client,
}

impl Core {
    pub fn new(config: &Config) -> Result<Self, String> {
        let _ = config.ensure_dirs();
        let db = Database::load(&config.db_file)?;
        let http_client = reqwest::Client::builder()
            .user_agent("appmgr")
            .build()
            .unwrap_or_default();
        Ok(Self {
            config: config.clone(),
            db,
            github: GitHubClient::new(),
            http_client,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn install_batch(
        &mut self,
        packages: &[String],
        force: bool,
        assume_yes: bool,
        repo: Option<String>,
        prefer: Option<&str>,
        include_pre: bool,
        auto_deps: bool,
        no_deps: bool,
        rootless_deps: bool,
    ) -> Result<(), String> {
        if packages.len() <= 1 {
            let pkg = packages
                .first()
                .ok_or_else(|| "No package target specified.".to_string())?;
            return self
                .install(
                    pkg,
                    force,
                    assume_yes,
                    repo,
                    prefer,
                    include_pre,
                    auto_deps,
                    no_deps,
                    rootless_deps,
                )
                .await
                .map(|_| ());
        }

        if repo.is_some() {
            return Err("--repo cannot be combined with multiple install targets.".to_string());
        }

        let total = packages.len();
        let mut succeeded = Vec::new();
        let mut skipped = Vec::new();
        let mut failed = Vec::new();

        for (idx, pkg) in packages.iter().enumerate() {
            println!(
                "\n{} [{}/{}] Installing {}...",
                c("::", &[Colors::BOLD, Colors::CYAN]),
                idx + 1,
                total,
                c(pkg, &[Colors::BOLD, Colors::WHITE])
            );
            match self
                .install(
                    pkg,
                    force,
                    assume_yes,
                    None,
                    prefer,
                    include_pre,
                    auto_deps,
                    no_deps,
                    rootless_deps,
                )
                .await
            {
                Ok(true) => succeeded.push(pkg.clone()),
                Ok(false) => skipped.push(pkg.clone()),
                Err(e) => {
                    eprintln!("{} {}: {}", apt_prefix_err("FAIL"), pkg, e);
                    failed.push((pkg.clone(), e));
                }
            }
        }

        Self::print_batch_summary("install", &succeeded, &skipped, &failed)
    }

    fn print_batch_summary(
        action: &str,
        succeeded: &[String],
        skipped: &[String],
        failed: &[(String, String)],
    ) -> Result<(), String> {
        let mut parts = vec![format!("{} succeeded", succeeded.len())];
        if !skipped.is_empty() {
            parts.push(format!("{} skipped", skipped.len()));
        }
        parts.push(format!("{} failed", failed.len()));

        let marker_color = if failed.is_empty() {
            Colors::GREEN
        } else {
            Colors::YELLOW
        };
        println!(
            "\n{} Batch {} summary: {}",
            c("::", &[Colors::BOLD, marker_color]),
            action,
            parts.join(", ")
        );

        if !failed.is_empty() {
            let names: Vec<&str> = failed.iter().map(|(n, _)| n.as_str()).collect();
            return Err(format!(
                "Batch {} failed for {} item(s): {}",
                action,
                failed.len(),
                names.join(", ")
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn install(
        &mut self,
        target_str: &str,
        force: bool,
        assume_yes: bool,
        repo: Option<String>,
        prefer: Option<&str>,
        include_pre: bool,
        auto_deps: bool,
        no_deps: bool,
        rootless_deps: bool,
    ) -> Result<bool, String> {
        let effective_prefer: Option<String> = prefer
            .map(String::from)
            .or_else(|| self.config.settings.default_prefer.clone());
        let local_path = Path::new(target_str);
        if local_path.is_file() {
            if target_str.ends_with(".toml") {
                if let Ok(recipe) = crate::registry::source_lint(local_path) {
                    println!(
                        "{} '{}' is a valid Source Definition (Schema v2).",
                        apt_prefix("INFO"),
                        recipe.source.id
                    );
                    println!(
                        "   To add it to sources: appmgr repo add {} {} {}",
                        recipe.source.driver, recipe.source.id, recipe.source.base_url
                    );
                    return Ok(false);
                }
            }
            let res = self.install_local_file(
                local_path, force, assume_yes, repo, None, None, None, None, None, false, None,
            )?;
            if let Some(ref id) = res {
                self.resolve_package_dependencies(id, auto_deps, no_deps, rootless_deps)
                    .await?;
                return Ok(true);
            }
            return Ok(false);
        }

        let effective_sources = crate::registry::get_effective_sources(
            &self.config.sources_list_file,
            &self.config.registries_cache_dir,
        );

        // Check for explicit prefix: <source_id>:<pkg> or <driver>:<pkg>
        if let Some((prefix, pkg_name)) = target_str.split_once(':') {
            if !target_str.starts_with("http://") && !target_str.starts_with("https://") {
                if prefix == "aur" {
                    let rpc_url = effective_sources
                        .iter()
                        .find(|s| s.driver == "aur")
                        .map(|s| s.url.as_str())
                        .unwrap_or("https://aur.archlinux.org/rpc/v5");
                    if let Some(info) =
                        crate::drivers::aur::get_aur_info(&self.http_client, rpc_url, pkg_name)
                            .await?
                    {
                        return crate::drivers::aur::install_aur_package(
                            &self.http_client,
                            &info,
                            assume_yes,
                        )
                        .await;
                    } else {
                        return Err(format!("Package '{}' not found in AUR.", pkg_name));
                    }
                }

                // Match specific source by ID or by driver
                for src in &effective_sources {
                    if !src.enabled {
                        continue;
                    }
                    if src.id.eq_ignore_ascii_case(prefix)
                        || src.driver.eq_ignore_ascii_case(prefix)
                    {
                        match src.driver.as_str() {
                            "apt" => {
                                let idx_file = self
                                    .config
                                    .indexes_cache_dir
                                    .join(format!("{}.json", src.id));
                                let pkgs = crate::drivers::apt::load_cached_apt_packages(&idx_file);
                                if let Some(p) = pkgs.get(pkg_name) {
                                    let res = self
                                        .install_from_apt_source(src, p, force, assume_yes)
                                        .await?;
                                    if let Some(ref id) = res {
                                        self.resolve_package_dependencies(
                                            id,
                                            auto_deps,
                                            no_deps,
                                            rootless_deps,
                                        )
                                        .await?;
                                        return Ok(true);
                                    }
                                    return Ok(false);
                                }
                            }
                            "appimagehub" => {
                                let idx_file = self
                                    .config
                                    .indexes_cache_dir
                                    .join(format!("{}.json", src.id));
                                let pkgs =
                                    crate::drivers::appimagehub::load_cached_appimagehub_packages(
                                        &idx_file,
                                    );
                                if let Some(p) = pkgs.get(pkg_name).or_else(|| {
                                    pkgs.values()
                                        .find(|v| v.name.eq_ignore_ascii_case(pkg_name))
                                }) {
                                    let res = self
                                        .install_from_appimagehub(src, p, force, assume_yes)
                                        .await?;
                                    if let Some(ref id) = res {
                                        self.resolve_package_dependencies(
                                            id,
                                            auto_deps,
                                            no_deps,
                                            rootless_deps,
                                        )
                                        .await?;
                                        return Ok(true);
                                    }
                                    return Ok(false);
                                }
                            }
                            _ => {}
                        }
                    }
                }

                return Err(format!(
                    "Package '{}' not found under source prefix '{}'.",
                    pkg_name, prefix
                ));
            }
        }

        let (parsed_repo, tag) = GitHubClient::parse_repo_and_tag(target_str);
        if target_str.starts_with("github:")
            || target_str.starts_with("http://")
            || target_str.starts_with("https://")
            || (target_str.contains('/') && parsed_repo.is_some())
        {
            let repo_name = parsed_repo
                .ok_or_else(|| format!("Invalid GitHub repository format: {}", target_str))?;
            let res = self
                .install_from_github(
                    &repo_name,
                    tag.as_deref(),
                    force,
                    assume_yes,
                    effective_prefer.as_deref(),
                    include_pre,
                )
                .await?;
            if let Some(ref id) = res {
                self.resolve_package_dependencies(id, auto_deps, no_deps, rootless_deps)
                    .await?;
                return Ok(true);
            }
            return Ok(false);
        }

        // Search local indexed packages (APT, AppImageHub) by priority
        for src in &effective_sources {
            if !src.enabled {
                continue;
            }
            match src.driver.as_str() {
                "apt" => {
                    let idx_file = self
                        .config
                        .indexes_cache_dir
                        .join(format!("{}.json", src.id));
                    let pkgs = crate::drivers::apt::load_cached_apt_packages(&idx_file);
                    if let Some(p) = pkgs.get(target_str) {
                        let res = self
                            .install_from_apt_source(src, p, force, assume_yes)
                            .await?;
                        if let Some(ref id) = res {
                            self.resolve_package_dependencies(
                                id,
                                auto_deps,
                                no_deps,
                                rootless_deps,
                            )
                            .await?;
                            return Ok(true);
                        }
                        return Ok(false);
                    }
                }
                "appimagehub" => {
                    let idx_file = self
                        .config
                        .indexes_cache_dir
                        .join(format!("{}.json", src.id));
                    let pkgs =
                        crate::drivers::appimagehub::load_cached_appimagehub_packages(&idx_file);
                    if let Some(p) = pkgs.get(target_str).or_else(|| {
                        pkgs.values()
                            .find(|v| v.name.eq_ignore_ascii_case(target_str))
                    }) {
                        let res = self
                            .install_from_appimagehub(src, p, force, assume_yes)
                            .await?;
                        if let Some(ref id) = res {
                            self.resolve_package_dependencies(
                                id,
                                auto_deps,
                                no_deps,
                                rootless_deps,
                            )
                            .await?;
                            return Ok(true);
                        }
                        return Ok(false);
                    }
                }
                _ => {}
            }
        }

        // Online search fallback (AUR)
        let aur_src = effective_sources
            .iter()
            .find(|s| s.enabled && s.driver == "aur");
        if let Some(asrc) = aur_src {
            if let Ok(Some(ainfo)) =
                crate::drivers::aur::get_aur_info(&self.http_client, &asrc.url, target_str).await
            {
                return crate::drivers::aur::install_aur_package(
                    &self.http_client,
                    &ainfo,
                    assume_yes,
                )
                .await;
            }
        }

        let res = self
            .search_and_install(
                target_str,
                force,
                assume_yes,
                effective_prefer.as_deref(),
                include_pre,
            )
            .await?;
        if let Some(ref id) = res {
            self.resolve_package_dependencies(id, auto_deps, no_deps, rootless_deps)
                .await?;
            return Ok(true);
        }
        Ok(false)
    }

    pub async fn install_from_github(
        &mut self,
        repo: &str,
        tag: Option<&str>,
        force: bool,
        assume_yes: bool,
        prefer: Option<&str>,
        include_pre: bool,
    ) -> Result<Option<String>, String> {
        let tag_disp = tag
            .map(|t| format!(" ({})", c(t, &[Colors::BOLD, Colors::BRIGHT_YELLOW])))
            .unwrap_or_default();
        println!(
            "{} Fetching release metadata from GitHub: {}{}",
            c("::", &[Colors::BOLD, Colors::CYAN]),
            c(repo, &[Colors::BOLD, Colors::CYAN]),
            tag_disp
        );

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
            return Ok(None);
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
                println!(
                    "{} Installation aborted.",
                    c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE])
                );
                return Ok(None);
            }
        }

        let app_name_hint = repo.split('/').next_back().unwrap_or(repo);
        let candidates =
            self.github
                .filter_assets(&release.assets, prefer, None, Some(app_name_hint));

        if candidates.is_empty() {
            println!(
                "{} No compatible binary assets found in {} ({}) for {}.",
                apt_prefix("FAIL"),
                repo,
                tag_name,
                std::env::consts::ARCH
            );
            return Ok(None);
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
    ) -> Result<Option<String>, String> {
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
            return Ok(None);
        }

        let remaining_repos = repos;
        while !remaining_repos.is_empty() {
            let chosen = Self::prompt_select_repository(&remaining_repos, query);
            let repo_item = match chosen {
                Some(r) => r,
                None => {
                    println!(
                        "{} Installation cancelled.",
                        c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE])
                    );
                    return Ok(None);
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
                    println!(
                        " {} Error checking {}: {}",
                        apt_prefix("WARN"),
                        repo_name,
                        e
                    );
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
            let candidates =
                self.github
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

            return self
                .download_and_install_asset(
                    &repo_name,
                    &release,
                    &candidates,
                    etag,
                    force,
                    assume_yes,
                )
                .await;
        }

        println!("{} No suitable repositories remaining.", apt_prefix("FAIL"));
        Ok(None)
    }

    async fn download_and_install_asset(
        &mut self,
        repo: &str,
        release: &Release,
        candidates: &[CandidateAsset],
        etag: Option<String>,
        force: bool,
        assume_yes: bool,
    ) -> Result<Option<String>, String> {
        let chosen = if candidates.len() == 1 || assume_yes {
            &candidates[0]
        } else {
            match Self::prompt_select_asset(candidates) {
                Some(c) => c,
                None => {
                    println!(
                        "{} Installation aborted.",
                        c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE])
                    );
                    return Ok(None);
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
        let app_hint = repo.split('/').next_back().unwrap_or(repo);
        let clean_bin = Some(crate::registry::clean_binary_name(&chosen.name, app_hint));

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
            clean_bin,
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
        clean_bin_name: Option<String>,
    ) -> Result<Option<String>, String> {
        let fmt = match detect_format(path) {
            Some(f) => f,
            None => {
                println!(
                    "{} Unsupported package format. Supported: .AppImage, .deb, .tar.*, .zip, standalone ELF",
                    apt_prefix("FAIL")
                );
                return Ok(None);
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
                c(
                    &format!("sudo pacman -U {}", path.display()),
                    &[Colors::BOLD, Colors::BRIGHT_YELLOW]
                )
            );
            return Ok(None);
        }

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
                let p = BinaryPackage::new(
                    path,
                    &self.config,
                    repo.clone(),
                    github_tag.clone(),
                    clean_bin_name.clone(),
                );
                p.read_metadata()?
            }
            PackageFormat::ArchNative => unreachable!(),
        };

        // Transaction Summary
        println!(
            "\nPackages (1) {} {}",
            c(
                &format!("{}-{} [{}]", name, version, fmt.as_str().to_uppercase()),
                &[Colors::BOLD, Colors::WHITE]
            ),
            c("[NEW]", &[Colors::BOLD, Colors::GREEN])
        );
        let mb = size as f64 / (1024.0 * 1024.0);
        println!(
            "Total Installed Size:   {}\n",
            c(&format!("{:.2} MiB", mb), &[Colors::DIM])
        );

        if !assume_yes && !confirm_dialog("Proceed with installation?", true) {
            println!(
                "{} Installation aborted by user.",
                c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE])
            );
            return Ok(None);
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
                let p = BinaryPackage::new(
                    path,
                    &self.config,
                    repo.clone(),
                    github_tag.clone(),
                    clean_bin_name.clone(),
                );
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

        // Clean binary launcher handling
        let desired_clean_name = clean_bin_name
            .or_else(|| Some(crate::registry::clean_binary_name(&record.id, &record.id)));

        if let Some(clean_name) = desired_clean_name {
            let clean_launcher_path = self.config.bin_dir.join(&clean_name);
            if let Some(primary_bin) = record.binaries.first().cloned() {
                if primary_bin != clean_launcher_path
                    && !record.binaries.contains(&clean_launcher_path)
                {
                    let _ = fs::remove_file(&clean_launcher_path);
                    #[cfg(unix)]
                    {
                        if std::os::unix::fs::symlink(&primary_bin, &clean_launcher_path).is_ok() {
                            record.binaries.push(clean_launcher_path);
                        }
                    }
                }
            }
        }

        println!(
            "Setting up {} ({}) ...",
            c(&record.name, &[Colors::BOLD, Colors::BRIGHT_GREEN]),
            record.version
        );
        for b in &record.binaries {
            println!(
                " {} Installed launcher: {}",
                apt_prefix("OK"),
                c(&b.display().to_string(), &[Colors::DIM])
            );
        }
        for d in &record.desktop_files {
            println!(
                " {} Installed desktop entry: {}",
                apt_prefix("OK"),
                c(&d.display().to_string(), &[Colors::DIM])
            );
        }
        for i in &record.icons {
            println!(
                " {} Installed icon: {}",
                apt_prefix("OK"),
                c(&i.display().to_string(), &[Colors::DIM])
            );
        }

        run_system_triggers(&self.config.applications_dir, &self.config.icons_dir);

        let pkg_id = record.id.clone();
        self.db
            .add(record)
            .map_err(|e| format!("Failed to update database: {}", e))?;

        let elapsed = start_time.elapsed().as_secs_f64();
        println!(
            "\n{} Transaction completed successfully in {}.\n   You can now launch it by typing: {}\n",
            c("::", &[Colors::BOLD, Colors::GREEN]),
            c(&format!("{:.2}s", elapsed), &[Colors::DIM]),
            c(&pkg_id, &[Colors::BOLD, Colors::YELLOW])
        );
        Ok(Some(pkg_id))
    }

    pub async fn search(
        &mut self,
        query: &str,
        filter_source: Option<&str>,
        filter_driver: Option<&str>,
        offline: bool,
        as_json: bool,
    ) -> Result<(), String> {
        let matches = crate::registry::search_all_sources(
            &self.http_client,
            &self.config.sources_list_file,
            &self.config.registries_cache_dir,
            &self.config.indexes_cache_dir,
            query,
            filter_source,
            filter_driver,
            offline,
        )
        .await;

        if as_json {
            let github_repos = if matches.is_empty() && !offline {
                self.github.search_repositories(query, 8).await?
            } else {
                Vec::new()
            };
            let json_out = Self::build_search_json(query, &matches, &github_repos);
            println!("{}", serde_json::to_string_pretty(&json_out).unwrap());
            return Ok(());
        }

        let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();

        if !matches.is_empty() {
            println!(
                "\n{} Found {} package(s) matching '{}':\n",
                c("::", &[Colors::BOLD, Colors::CYAN]),
                matches.len(),
                c(query, &[Colors::BOLD, Colors::CYAN])
            );

            let headers = [
                "#",
                "SOURCE",
                "DRIVER",
                "PACKAGE ID",
                "FORMAT",
                "VERSION",
                "DESCRIPTION",
            ];
            let mut rows = Vec::new();

            for (idx, m) in matches.iter().enumerate() {
                let pkg_display =
                    if !m.name.is_empty() && !m.name.eq_ignore_ascii_case(&m.package_id) {
                        format!("{} ({})", m.package_id, m.name)
                    } else {
                        m.package_id.clone()
                    };
                let clean_desc = m
                    .description
                    .lines()
                    .next()
                    .unwrap_or("")
                    .replace('\t', " ")
                    .trim()
                    .to_string();
                rows.push(vec![
                    c(&(idx + 1).to_string(), &[Colors::BOLD, Colors::YELLOW]),
                    format!("[{}]", m.source_id),
                    format!("[{}]", m.driver.to_uppercase()),
                    c(&pkg_display, &[Colors::BOLD, Colors::CYAN]),
                    format!("[{}]", m.format.to_uppercase()),
                    m.version.clone(),
                    clean_desc,
                ]);
            }

            print_table(&headers, &rows);

            if interactive {
                if let Some(selected_idx) =
                    Self::prompt_select_index("Enter number to install", matches.len())
                {
                    let chosen = &matches[selected_idx];
                    let target = if chosen.driver == "aur" {
                        format!("aur:{}", chosen.package_id)
                    } else {
                        format!("{}:{}", chosen.source_id, chosen.package_id)
                    };
                    self.install(
                        &target, false, false, None, None, false, false, false, false,
                    )
                    .await?;
                }
            }
            return Ok(());
        }

        if offline {
            println!(
                "{} No packages matching '{}' found in configured sources (offline mode).",
                apt_prefix("INFO"),
                query
            );
            return Ok(());
        }

        println!(
            "{} Searching GitHub for '{}'...",
            c("::", &[Colors::BOLD, Colors::CYAN]),
            c(query, &[Colors::BOLD, Colors::CYAN])
        );

        let spinner = Spinner::new(&format!("Querying GitHub Search API for '{}'...", query));
        let repos_res = self.github.search_repositories(query, 8).await;
        spinner.stop();

        match repos_res {
            Ok(repos) => {
                if repos.is_empty() {
                    println!(
                        "{} No repositories or packages found matching '{}'.",
                        apt_prefix("FAIL"),
                        query
                    );
                } else {
                    println!(
                        "\n{} Found {} GitHub repositories for '{}':\n",
                        c("::", &[Colors::BOLD, Colors::CYAN]),
                        repos.len(),
                        c(query, &[Colors::BOLD, Colors::CYAN])
                    );

                    for (i, repo) in repos.iter().enumerate() {
                        let desc = repo
                            .description
                            .as_deref()
                            .unwrap_or("No description provided");
                        println!(
                            "  {} {} {} {}",
                            c(&format!("{:2}.", i + 1), &[Colors::BOLD, Colors::YELLOW]),
                            c(&repo.full_name, &[Colors::BOLD, Colors::WHITE]),
                            c(&format!("★ {}", repo.stargazers_count), &[Colors::YELLOW]),
                            c(&format!("- {}", desc), &[Colors::DIM])
                        );
                    }

                    if interactive {
                        if let Some(selected_idx) =
                            Self::prompt_select_index("Enter number to install", repos.len())
                        {
                            let repo_name = repos[selected_idx].full_name.clone();
                            let installed_id = self
                                .install_from_github(&repo_name, None, false, false, None, false)
                                .await?;
                            if let Some(ref id) = installed_id {
                                self.resolve_package_dependencies(id, false, false, false)
                                    .await?;
                            }
                        }
                    } else {
                        println!();
                    }
                }
            }
            Err(e) => {
                println!(" {} GitHub search failed: {}", apt_prefix("WARN"), e);
            }
        }

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

        let linked_pkgs: Vec<PackageRecord> = target_pkgs
            .into_iter()
            .filter(|p| p.repo.is_some())
            .collect();

        if linked_pkgs.is_empty() {
            if let Some(q) = app_query {
                if as_json {
                    println!(
                        "{}",
                        json!({"error": format!("Package '{}' is not linked to any GitHub repository", q)})
                    );
                }
                return Err(format!(
                    "Package '{}' is not linked to any GitHub repository.",
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
                "{} No installed packages are linked to GitHub repositories.",
                c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE])
            );
            return Ok(false);
        }

        if !as_json {
            println!(
                "{} Checking {} linked package(s) for updates...",
                c("::", &[Colors::BOLD, Colors::CYAN]),
                linked_pkgs.len()
            );
        }

        // Concurrent multi-threaded check via tokio tasks (semaphore max 8)
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
        let client = self.github.clone();
        let prefer_owned = prefer
            .or(self.config.settings.default_prefer.as_deref())
            .map(String::from);

        let total_to_check = linked_pkgs.len();
        let spinner = if !as_json && total_to_check > 0 {
            Some(Spinner::with_counter(
                "Checking GitHub Releases...",
                total_to_check,
                "checked",
            ))
        } else {
            None
        };
        let counter = spinner.as_ref().map(|s| s.counter());

        let mut tasks = Vec::new();
        for pkg in linked_pkgs {
            let sem = semaphore.clone();
            let gh = client.clone();
            let pref = prefer_owned.clone();
            let force_flag = force;
            let single_app = app_query.is_some();
            let ctr = counter.clone();

            tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let repo = pkg.repo.as_ref().unwrap();
                let is_pinned = pkg.pinned;

                let outcome = if is_pinned && !force_flag && !single_app {
                    UpgradeCheckResult::Pinned(pkg)
                } else {
                    let etag_ref = pkg.etag.as_deref();
                    let release_res = gh.get_release(repo, None, include_pre, etag_ref).await;

                    match release_res {
                        Ok((StatusCode::NOT_MODIFIED, _, _, _)) => {
                            UpgradeCheckResult::UpToDate(pkg, true)
                        }
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
                                    UpgradeCheckResult::Upgrade(
                                        pkg,
                                        new_tag,
                                        new_etag,
                                        chosen,
                                        release.assets,
                                        pattern,
                                    )
                                } else {
                                    UpgradeCheckResult::UpToDate(pkg, false)
                                }
                            } else {
                                UpgradeCheckResult::UpToDate(pkg, false)
                            }
                        }
                        _ => UpgradeCheckResult::UpToDate(pkg, false),
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
        if let Some(s) = spinner {
            s.stop();
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

        for (u, new_tag, _, _, _, _) in &upgrades {
            println!(
                " {} {}: update available ({} -> {}) {}",
                apt_prefix("INFO"),
                c(&u.name, &[Colors::BOLD, Colors::GREEN]),
                u.version,
                c(
                    new_tag.trim_start_matches('v'),
                    &[Colors::BOLD, Colors::YELLOW]
                ),
                c(
                    &format!("[{}]", u.repo.as_deref().unwrap_or("-")),
                    &[Colors::DIM]
                )
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
        for (pkg, new_tag, new_etag, asset_info, release_assets, pattern) in upgrades {
            println!(
                "\n{} Upgrading {} ({} -> {}) ...",
                c("::", &[Colors::BOLD, Colors::CYAN]),
                c(&pkg.name, &[Colors::BOLD, Colors::CYAN]),
                pkg.version,
                c(&new_tag, &[Colors::BOLD, Colors::YELLOW])
            );

            let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
            let downloaded_file = temp_dir.path().join(&asset_info.name);

            let backup_dir =
                Self::prepare_upgrade_backup(&self.config.backups_dir, &pkg, keep_backups)?;

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
                if keep_backups == 0 {
                    let _ = fs::remove_dir_all(&backup_dir);
                }
                failed_count += 1;
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
                None,
            );

            match ok {
                Ok(Some(ref upgraded_id)) => {
                    Self::finalize_upgrade_backup(
                        &self.config.backups_dir,
                        &pkg.id,
                        &backup_dir,
                        keep_backups,
                    );
                    let upgrade_auto_deps =
                        assume_yes || matches!(self.config.settings.deps_mode, DepsMode::Host);
                    let _ = self
                        .resolve_package_dependencies(upgraded_id, upgrade_auto_deps, false, false)
                        .await;
                    success_count += 1;
                    println!(
                        " {} {} upgraded to {}",
                        apt_prefix("OK"),
                        pkg.name,
                        c(&new_tag, &[Colors::BOLD, Colors::GREEN])
                    );
                }
                _ => {
                    failed_count += 1;
                    println!("{} Upgrade failed. Rolling back...", apt_prefix("FAIL"));
                    let backup_files = backup_dir.join("files");
                    if backup_files.exists() {
                        let _ = fs::remove_dir_all(&pkg.install_dir);
                        let _ = ArchivePackage::copy_dir_all(&backup_files, &pkg.install_dir);
                    }
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
            // Rotate existing snapshots: slot 0 -> prev-1, prev-1 -> prev-2, ..., prune >= keep_backups - 1
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
            } else {
                let _ = fs::remove_file(&cur_meta);
                let _ = fs::remove_dir_all(&cur_files);
            }
        }

        if pkg.install_dir.exists() {
            fs::create_dir_all(&backup_dir).map_err(|e| e.to_string())?;
            let backup_files = backup_dir.join("files");
            ArchivePackage::copy_dir_all(&pkg.install_dir, &backup_files)?;
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

        let prev1 = backup_dir.join("prev-1");
        if prev1.exists() {
            if prev1.join("meta.json").exists() {
                let _ = fs::rename(prev1.join("meta.json"), backup_dir.join("meta.json"));
            }
            if prev1.join("files").exists() {
                let _ = fs::rename(prev1.join("files"), backup_dir.join("files"));
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

        let _ = fs::remove_dir_all(&pkg.install_dir);
        ArchivePackage::copy_dir_all(&files_dir, &pkg.install_dir)?;

        self.db
            .add(prev_record.clone())
            .map_err(|e| e.to_string())?;
        run_system_triggers(&self.config.applications_dir, &self.config.icons_dir);
        Self::promote_previous_backup(&backup_dir);

        println!(
            "\n{} Successfully rolled back {} to version {}.\n",
            c("::", &[Colors::BOLD, Colors::GREEN]),
            pkg.name,
            prev_record.version
        );
        Ok(true)
    }

    pub fn clean(&self, assume_yes: bool) -> Result<bool, String> {
        let mut backup_bytes = 0;
        if self.config.backups_dir.exists() {
            for e in walkdir::WalkDir::new(&self.config.backups_dir)
                .into_iter()
                .flatten()
            {
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

        println!(
            "{} AppMgr Cache Cleanup:",
            c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE])
        );
        println!(
            "   Rollback backups:   {:.1} MB",
            backup_bytes as f64 / (1024.0 * 1024.0)
        );
        println!(
            "   Temporary files:    {:.1} MB",
            temp_bytes as f64 / (1024.0 * 1024.0)
        );
        println!(
            "   Estimated space:    {}",
            c(
                &format!("{:.1} MB", total_mb),
                &[Colors::BOLD, Colors::BRIGHT_GREEN]
            )
        );

        if total_bytes == 0 {
            println!(
                "\n{} Cache is already clean. Nothing to do.\n",
                c("::", &[Colors::BOLD, Colors::BRIGHT_GREEN])
            );
            return Ok(true);
        }

        if !assume_yes && !confirm_dialog("Proceed with cleanup?", true) {
            println!(
                "{} Cleanup cancelled.",
                c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE])
            );
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
                c(
                    "Environment variables for",
                    &[Colors::BOLD, Colors::BRIGHT_WHITE]
                ),
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
                    Self::update_wrapper_script(bin, &updated.env, &updated.args)?;
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
                let key = k.trim();
                if !crate::sandbox::is_valid_env_name(key) {
                    return Err(format!(
                        "Invalid environment variable name '{}': must match ^[A-Za-z_][A-Za-z0-9_]*$",
                        key
                    ));
                }
                new_vars.insert(key.to_string(), v.to_string());
            } else {
                return Err(format!(
                    "Invalid variable pair '{}' (expected KEY=VALUE)",
                    p
                ));
            }
        }

        if !new_vars.is_empty() {
            let keys: Vec<String> = new_vars.keys().cloned().collect();
            self.db
                .set_env(&pkg.id, new_vars)
                .map_err(|e| e.to_string())?;
            if let Some(updated) = self.db.get(&pkg.id) {
                if let Some(bin) = updated.binaries.first() {
                    Self::update_wrapper_script(bin, &updated.env, &updated.args)?;
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
                c(
                    "Persistent launch arguments for",
                    &[Colors::BOLD, Colors::BRIGHT_WHITE]
                ),
                c(&pkg.name, &[Colors::BOLD, Colors::BRIGHT_CYAN])
            );
            if pkg.args.is_empty() {
                println!("  No persistent arguments configured.");
            } else {
                println!(
                    "  {}",
                    c(&pkg.args.join(" "), &[Colors::BOLD, Colors::BRIGHT_YELLOW])
                );
            }
            println!();
            return Ok(true);
        }

        if clear {
            self.db.clear_args(&pkg.id).map_err(|e| e.to_string())?;
            if let Some(updated) = self.db.get(&pkg.id) {
                if let Some(bin) = updated.binaries.first() {
                    Self::update_wrapper_script(bin, &updated.env, &updated.args)?;
                }
            }
            println!(
                " {} Cleared persistent arguments for {}.",
                apt_prefix("OK"),
                pkg.name
            );
            return Ok(true);
        }

        if !args_list.is_empty() {
            self.db
                .set_args(&pkg.id, args_list.to_vec())
                .map_err(|e| e.to_string())?;
            if let Some(updated) = self.db.get(&pkg.id) {
                if let Some(bin) = updated.binaries.first() {
                    Self::update_wrapper_script(bin, &updated.env, &updated.args)?;
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

        let args_block = crate::sandbox::format_persistent_args(args);
        let re_args = regex::Regex::new(r"(?ms)^PERSISTENT_ARGS=\((?:\)|.*?\n\))\n?")
            .map_err(|e| e.to_string())?;
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
        if content.ends_with('\n') {
            new_content.push('\n');
        }
        if !env.is_empty() {
            let env_exports = crate::sandbox::format_env_exports(env)?;
            let env_block = format!("{}\n", env_exports);
            if let Some(pos) = new_content.find("PERSISTENT_ARGS=") {
                new_content.insert_str(pos, env_block.trim_start_matches('\n'));
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

    pub fn pin_batch(&mut self, apps: &[String]) -> Result<(), String> {
        if apps.len() <= 1 {
            let app = apps
                .first()
                .ok_or_else(|| "No application target specified.".to_string())?;
            return self.pin_app(app).map(|_| ());
        }
        let mut succeeded = Vec::new();
        let mut failed = Vec::new();
        for app in apps {
            match self.pin_app(app) {
                Ok(_) => succeeded.push(app.clone()),
                Err(e) => {
                    eprintln!("{} {}: {}", apt_prefix_err("FAIL"), app, e);
                    failed.push((app.clone(), e));
                }
            }
        }
        Self::print_batch_summary("pin", &succeeded, &[], &failed)
    }

    pub fn unpin_batch(&mut self, apps: &[String]) -> Result<(), String> {
        if apps.len() <= 1 {
            let app = apps
                .first()
                .ok_or_else(|| "No application target specified.".to_string())?;
            return self.unpin_app(app).map(|_| ());
        }
        let mut succeeded = Vec::new();
        let mut failed = Vec::new();
        for app in apps {
            match self.unpin_app(app) {
                Ok(_) => succeeded.push(app.clone()),
                Err(e) => {
                    eprintln!("{} {}: {}", apt_prefix_err("FAIL"), app, e);
                    failed.push((app.clone(), e));
                }
            }
        }
        Self::print_batch_summary("unpin", &succeeded, &[], &failed)
    }

    pub fn pin_app(&mut self, app_id: &str) -> Result<bool, String> {
        let pkg = self
            .db
            .get(app_id)
            .ok_or_else(|| format!("Package '{}' is not managed by appmgr.", app_id))?
            .clone();

        self.db
            .set_pinned(&pkg.id, true)
            .map_err(|e| e.to_string())?;
        println!(
            " {} Application '{}' pinned to version {} {}. General upgrades will skip it.",
            apt_prefix("OK"),
            c(&pkg.name, &[Colors::BOLD, Colors::WHITE]),
            pkg.version,
            c("[PINNED]", &[Colors::BOLD, Colors::MAGENTA])
        );
        Ok(true)
    }

    pub fn unpin_app(&mut self, app_id: &str) -> Result<bool, String> {
        let pkg = self
            .db
            .get(app_id)
            .ok_or_else(|| format!("Package '{}' is not managed by appmgr.", app_id))?
            .clone();

        self.db
            .set_pinned(&pkg.id, false)
            .map_err(|e| e.to_string())?;
        println!(
            " {} Application '{}' unpinned. Updates enabled.",
            apt_prefix("OK"),
            c(&pkg.name, &[Colors::BOLD, Colors::WHITE])
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
            let repo = app["repo"].as_str();

            if self.db.get(id).is_some() {
                println!(
                    " {} {} is already installed, skipping.",
                    apt_prefix("OK"),
                    name
                );
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
            if let Some(ref id) = ok {
                let _ = self
                    .resolve_package_dependencies(id, false, false, false)
                    .await;
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
                ver_str = format!(
                    "{} {}",
                    ver_str,
                    c("[PINNED]", &[Colors::BOLD, Colors::MAGENTA])
                );
            }

            rows.push(vec![
                p.id.clone(),
                ver_str,
                pkg_type,
                repo_str,
                c(&size_str, &[Colors::DIM]),
                bin_name,
            ]);
        }

        println!(
            "\n{} Managed Applications ({} total):\n",
            c("::", &[Colors::BOLD, Colors::CYAN]),
            pkgs.len()
        );
        print_table(&headers, &rows);
    }

    pub fn build_installed_info_card_lines(
        pkg: &PackageRecord,
        missing_libs: &[String],
        cols: usize,
        colorize: bool,
    ) -> Vec<String> {
        use crate::ui::c_if;

        const LABEL_WIDTH: usize = 15;
        let section_width = cols.saturating_sub(4).clamp(40, 72);
        let mut lines = Vec::new();

        lines.push(format!(
            "\n{} {} {}",
            c_if(colorize, "::", &[Colors::BOLD, Colors::CYAN]),
            c_if(colorize, &pkg.name, &[Colors::BOLD, Colors::WHITE]),
            c_if(colorize, &format!("v{}", pkg.version), &[Colors::DIM])
        ));

        // 1. Package
        lines.push(format_card_section("Package", section_width, colorize));
        lines.push(format_card_row("Name:", LABEL_WIDTH, &pkg.name, colorize));
        lines.push(format_card_row("ID:", LABEL_WIDTH, &pkg.id, colorize));

        let ver_disp = if pkg.pinned {
            format!(
                "{} {}",
                pkg.version,
                c_if(colorize, "[PINNED]", &[Colors::BOLD, Colors::MAGENTA])
            )
        } else {
            pkg.version.clone()
        };
        lines.push(format_card_row(
            "Version:",
            LABEL_WIDTH,
            &ver_disp,
            colorize,
        ));
        lines.push(format_card_row(
            "Format:",
            LABEL_WIDTH,
            &format!("[{}]", pkg.format.to_uppercase()),
            colorize,
        ));
        lines.push(format_card_row(
            "Status:",
            LABEL_WIDTH,
            &c_if(colorize, "Installed", &[Colors::GREEN]),
            colorize,
        ));
        if pkg.pinned {
            lines.push(format_card_row(
                "Pinned:",
                LABEL_WIDTH,
                &c_if(
                    colorize,
                    "Yes (general upgrades disabled)",
                    &[Colors::BOLD, Colors::MAGENTA],
                ),
                colorize,
            ));
        }
        if let Some(desc) = &pkg.description {
            lines.push(format_card_row("Description:", LABEL_WIDTH, desc, colorize));
        }
        lines.push(format_card_row(
            "Installed On:",
            LABEL_WIDTH,
            &c_if(colorize, &pkg.install_date, &[Colors::DIM]),
            colorize,
        ));

        // 2. Runtime & Paths
        lines.push(String::new());
        lines.push(format_card_section(
            "Runtime & Paths",
            section_width,
            colorize,
        ));
        lines.push(format_card_row("Mode:", LABEL_WIDTH, &pkg.mode, colorize));
        lines.push(format_card_row(
            "Location:",
            LABEL_WIDTH,
            &c_if(
                colorize,
                &pkg.install_dir.display().to_string(),
                &[Colors::DIM],
            ),
            colorize,
        ));
        let size_mib = pkg.total_size_bytes.unwrap_or(0) as f64 / (1024.0 * 1024.0);
        lines.push(format_card_row(
            "Disk Usage:",
            LABEL_WIDTH,
            &c_if(colorize, &format!("{:.2} MiB", size_mib), &[Colors::DIM]),
            colorize,
        ));
        if !pkg.binaries.is_empty() {
            let bins: Vec<String> = pkg
                .binaries
                .iter()
                .map(|b| b.display().to_string())
                .collect();
            lines.push(format_card_row(
                "Binaries:",
                LABEL_WIDTH,
                &c_if(colorize, &bins.join(", "), &[Colors::DIM]),
                colorize,
            ));
        }
        if !pkg.desktop_files.is_empty() {
            let dts: Vec<String> = pkg
                .desktop_files
                .iter()
                .map(|d| d.display().to_string())
                .collect();
            lines.push(format_card_row(
                "Desktop:",
                LABEL_WIDTH,
                &c_if(colorize, &dts.join(", "), &[Colors::DIM]),
                colorize,
            ));
        }
        if pkg.bwrap == Some(true) {
            lines.push(format_card_row(
                "Sandbox:",
                LABEL_WIDTH,
                "Bubblewrap (/opt isolated)",
                colorize,
            ));
        }
        if !pkg.env.is_empty() {
            let mut pairs: Vec<String> = pkg
                .env
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect();
            pairs.sort();
            lines.push(format_card_row(
                "Environment:",
                LABEL_WIDTH,
                &pairs.join(", "),
                colorize,
            ));
        }
        if let Some(ref def_env) = pkg.default_env {
            let mut pairs: Vec<String> = def_env
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect();
            pairs.sort();
            lines.push(format_card_row(
                "Default Env:",
                LABEL_WIDTH,
                &pairs.join(", "),
                colorize,
            ));
        }
        if !pkg.args.is_empty() {
            lines.push(format_card_row(
                "Launch Args:",
                LABEL_WIDTH,
                &pkg.args.join(" "),
                colorize,
            ));
        }
        if let Some(ref def_args) = pkg.default_args {
            lines.push(format_card_row(
                "Default Args:",
                LABEL_WIDTH,
                &def_args.join(" "),
                colorize,
            ));
        }

        // 3. Source & Deps
        lines.push(String::new());
        lines.push(format_card_section(
            "Source & Deps",
            section_width,
            colorize,
        ));
        if let Some(ref rname) = pkg.registry_name {
            lines.push(format_card_row(
                "Registry:",
                LABEL_WIDTH,
                &format!("[{}]", rname),
                colorize,
            ));
        }
        if let Some(ref stype) = pkg.source_type {
            lines.push(format_card_row(
                "Source Type:",
                LABEL_WIDTH,
                stype,
                colorize,
            ));
        }
        if let Some(ref recipe_id) = pkg.recipe_id {
            lines.push(format_card_row(
                "Recipe ID:",
                LABEL_WIDTH,
                recipe_id,
                colorize,
            ));
        }
        if let Some(repo) = &pkg.repo {
            lines.push(format_card_row(
                "GitHub Repo:",
                LABEL_WIDTH,
                &c_if(colorize, repo, &[Colors::DIM]),
                colorize,
            ));
            if let Some(tag) = &pkg.tag {
                lines.push(format_card_row("Release Tag:", LABEL_WIDTH, tag, colorize));
            }
            if let Some(pat) = &pkg.asset_pattern {
                lines.push(format_card_row(
                    "Asset Match:",
                    LABEL_WIDTH,
                    &c_if(colorize, pat, &[Colors::DIM]),
                    colorize,
                ));
            }
        }
        if let Some(ref sf) = pkg.source_file {
            lines.push(format_card_row(
                "Source File:",
                LABEL_WIDTH,
                &c_if(colorize, sf, &[Colors::DIM]),
                colorize,
            ));
        }

        if pkg.format == "appimage" {
            lines.push(format_card_row(
                "Dependencies:",
                LABEL_WIDTH,
                &c_if(
                    colorize,
                    "AppImage bundled runtime (self-contained)",
                    &[Colors::GREEN],
                ),
                colorize,
            ));
        } else if missing_libs.is_empty() {
            lines.push(format_card_row(
                "Dependencies:",
                LABEL_WIDTH,
                &c_if(colorize, "All shared libraries satisfied", &[Colors::GREEN]),
                colorize,
            ));
        } else {
            lines.push(format_card_row(
                "Dependencies:",
                LABEL_WIDTH,
                &c_if(
                    colorize,
                    &format!("{} missing shared libraries", missing_libs.len()),
                    &[Colors::YELLOW],
                ),
                colorize,
            ));
            for m in missing_libs.iter().take(5) {
                lines.push(format!("    - {}", c_if(colorize, m, &[Colors::RED])));
            }
            if missing_libs.len() > 5 {
                lines.push(format!("    ... and {} more", missing_libs.len() - 5));
            }
            lines.push(format!(
                "    Tip: Run {} to diagnose and fix.",
                c_if(
                    colorize,
                    &format!("appmgr deps {} --fix", pkg.id),
                    &[Colors::BOLD, Colors::CYAN]
                )
            ));
        }

        if !pkg.bundled_deps.is_empty() {
            let sample: Vec<String> = pkg.bundled_deps.iter().take(5).cloned().collect();
            let more = if pkg.bundled_deps.len() > 5 {
                format!(" (+{} more)", pkg.bundled_deps.len() - 5)
            } else {
                String::new()
            };
            lines.push(format_card_row(
                "Bundled Deps:",
                LABEL_WIDTH,
                &format!("{} ({}{})", pkg.bundled_deps.len(), sample.join(", "), more),
                colorize,
            ));
        }

        lines
    }

    pub fn build_installed_info_json(
        pkg: &PackageRecord,
        missing_libs: &[String],
    ) -> serde_json::Value {
        json!({
            "kind": "installed",
            "id": pkg.id,
            "name": pkg.name,
            "version": pkg.version,
            "format": pkg.format,
            "status": "installed",
            "mode": pkg.mode,
            "pinned": pkg.pinned,
            "install_date": pkg.install_date,
            "install_dir": pkg.install_dir,
            "total_size_bytes": pkg.total_size_bytes,
            "description": pkg.description,
            "repo": pkg.repo,
            "tag": pkg.tag,
            "asset_pattern": pkg.asset_pattern,
            "source_type": pkg.source_type,
            "registry_name": pkg.registry_name,
            "recipe_id": pkg.recipe_id,
            "source_file": pkg.source_file,
            "bwrap": pkg.bwrap,
            "binaries": pkg.binaries,
            "desktop_files": pkg.desktop_files,
            "icons": pkg.icons,
            "env": pkg.env,
            "default_env": pkg.default_env,
            "args": pkg.args,
            "default_args": pkg.default_args,
            "bundled_deps": pkg.bundled_deps,
            "missing_libs": missing_libs,
        })
    }

    pub fn info(&self, target: &str, as_json: bool) -> Result<(), String> {
        let colorize = should_color_stdout();
        let cols = terminal_size::terminal_size()
            .map(|(w, _)| w.0 as usize)
            .unwrap_or(80);
        let section_width = cols.saturating_sub(4).clamp(40, 72);
        const LABEL_WIDTH: usize = 15;

        let path = Path::new(target);
        if path.is_file() {
            let fmt = detect_format(path).map(|f| f.as_str().to_string());
            let size_bytes = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            if as_json {
                let json_out = json!({
                    "kind": "local_file",
                    "path": path.display().to_string(),
                    "format": fmt,
                    "size_bytes": size_bytes,
                });
                println!("{}", serde_json::to_string_pretty(&json_out).unwrap());
                return Ok(());
            }

            println!(
                "\n{} {}",
                c("::", &[Colors::BOLD, Colors::CYAN]),
                c(
                    "Package Information (Local File)",
                    &[Colors::BOLD, Colors::WHITE]
                )
            );
            println!(
                "{}",
                format_card_section("Package", section_width, colorize)
            );
            println!(
                "{}",
                format_card_row(
                    "Path:",
                    LABEL_WIDTH,
                    &c(&path.display().to_string(), &[Colors::DIM]),
                    colorize
                )
            );
            if let Some(f) = fmt {
                println!(
                    "{}",
                    format_card_row(
                        "Format:",
                        LABEL_WIDTH,
                        &format!("[{}]", f.to_uppercase()),
                        colorize
                    )
                );
            }
            println!(
                "{}",
                format_card_row(
                    "Size:",
                    LABEL_WIDTH,
                    &c(
                        &format!("{:.2} MiB", size_bytes as f64 / (1024.0 * 1024.0)),
                        &[Colors::DIM]
                    ),
                    colorize
                )
            );
            println!();
            return Ok(());
        }

        let pkg = match self.db.get(target) {
            Some(p) => p.clone(),
            None => {
                let effective_sources = crate::registry::get_effective_sources(
                    &self.config.sources_list_file,
                    &self.config.registries_cache_dir,
                );

                for src in &effective_sources {
                    if !src.enabled {
                        continue;
                    }
                    if src.driver == "apt" {
                        let idx_file = self
                            .config
                            .indexes_cache_dir
                            .join(format!("{}.json", src.id));
                        let pkgs = crate::drivers::apt::load_cached_apt_packages(&idx_file);
                        if let Some(p) = pkgs.get(target) {
                            if as_json {
                                let json_out = json!({
                                    "kind": "source_apt",
                                    "source_id": src.id,
                                    "source_url": src.url,
                                    "status": "available",
                                    "package": p.package,
                                    "version": p.version,
                                    "format": "deb",
                                    "architecture": p.architecture,
                                    "size_bytes": p.size,
                                    "sha256": p.sha256,
                                    "homepage": p.homepage,
                                    "description": p.description,
                                });
                                println!("{}", serde_json::to_string_pretty(&json_out).unwrap());
                                return Ok(());
                            }

                            println!(
                                "\n{} {}",
                                c("::", &[Colors::BOLD, Colors::CYAN]),
                                c(
                                    "Package Information (APT Source)",
                                    &[Colors::BOLD, Colors::WHITE]
                                )
                            );
                            println!(
                                "{}",
                                format_card_section("Package", section_width, colorize)
                            );
                            println!(
                                "{}",
                                format_card_row("Name:", LABEL_WIDTH, &p.package, colorize)
                            );
                            println!(
                                "{}",
                                format_card_row("Version:", LABEL_WIDTH, &p.version, colorize)
                            );
                            println!(
                                "{}",
                                format_card_row("Format:", LABEL_WIDTH, "[DEB]", colorize)
                            );
                            println!(
                                "{}",
                                format_card_row(
                                    "Status:",
                                    LABEL_WIDTH,
                                    &c("Available in Source (Not Installed)", &[Colors::YELLOW]),
                                    colorize
                                )
                            );
                            println!(
                                "{}",
                                format_card_row(
                                    "Architecture:",
                                    LABEL_WIDTH,
                                    &p.architecture,
                                    colorize
                                )
                            );
                            if let Some(ref desc) = p.description {
                                println!(
                                    "{}",
                                    format_card_row("Description:", LABEL_WIDTH, desc, colorize)
                                );
                            }

                            println!();
                            println!(
                                "{}",
                                format_card_section("Runtime & Paths", section_width, colorize)
                            );
                            let mb = p.size as f64 / (1024.0 * 1024.0);
                            println!(
                                "{}",
                                format_card_row(
                                    "Package Size:",
                                    LABEL_WIDTH,
                                    &c(&format!("{:.2} MiB", mb), &[Colors::DIM]),
                                    colorize
                                )
                            );
                            println!(
                                "{}",
                                format_card_row(
                                    "SHA256:",
                                    LABEL_WIDTH,
                                    &c(&p.sha256, &[Colors::DIM]),
                                    colorize
                                )
                            );

                            println!();
                            println!(
                                "{}",
                                format_card_section("Source & Deps", section_width, colorize)
                            );
                            println!(
                                "{}",
                                format_card_row(
                                    "Source:",
                                    LABEL_WIDTH,
                                    &format!("[{}] {}", src.id, c(&src.url, &[Colors::DIM])),
                                    colorize
                                )
                            );
                            if let Some(ref hp) = p.homepage {
                                println!(
                                    "{}",
                                    format_card_row(
                                        "Homepage:",
                                        LABEL_WIDTH,
                                        &c(hp, &[Colors::DIM]),
                                        colorize
                                    )
                                );
                            }
                            println!(
                                "\n  Run {} to install this package.\n",
                                c(
                                    &format!("appmgr install {}:{}", src.id, p.package),
                                    &[Colors::BOLD, Colors::YELLOW]
                                )
                            );
                            return Ok(());
                        }
                    } else if src.driver == "appimagehub" {
                        let idx_file = self
                            .config
                            .indexes_cache_dir
                            .join(format!("{}.json", src.id));
                        let pkgs = crate::drivers::appimagehub::load_cached_appimagehub_packages(
                            &idx_file,
                        );
                        if let Some(p) = pkgs
                            .get(target)
                            .or_else(|| pkgs.values().find(|v| v.name.eq_ignore_ascii_case(target)))
                        {
                            if as_json {
                                let json_out = json!({
                                    "kind": "source_appimagehub",
                                    "source_id": src.id,
                                    "status": "available",
                                    "id": p.id,
                                    "name": p.name,
                                    "format": "appimage",
                                    "github_repo": p.github_repo,
                                    "download_url": p.download_url,
                                    "categories": p.categories,
                                    "description": p.description,
                                });
                                println!("{}", serde_json::to_string_pretty(&json_out).unwrap());
                                return Ok(());
                            }

                            println!(
                                "\n{} {}",
                                c("::", &[Colors::BOLD, Colors::CYAN]),
                                c(
                                    "Package Information (AppImageHub)",
                                    &[Colors::BOLD, Colors::WHITE]
                                )
                            );
                            println!(
                                "{}",
                                format_card_section("Package", section_width, colorize)
                            );
                            println!(
                                "{}",
                                format_card_row("Name:", LABEL_WIDTH, &p.name, colorize)
                            );
                            println!("{}", format_card_row("ID:", LABEL_WIDTH, &p.id, colorize));
                            println!(
                                "{}",
                                format_card_row("Format:", LABEL_WIDTH, "[APPIMAGE]", colorize)
                            );
                            println!(
                                "{}",
                                format_card_row(
                                    "Status:",
                                    LABEL_WIDTH,
                                    &c("Available in Source (Not Installed)", &[Colors::YELLOW]),
                                    colorize
                                )
                            );
                            if !p.categories.is_empty() {
                                println!(
                                    "{}",
                                    format_card_row(
                                        "Categories:",
                                        LABEL_WIDTH,
                                        &p.categories.join(", "),
                                        colorize
                                    )
                                );
                            }
                            if let Some(ref desc) = p.description {
                                println!(
                                    "{}",
                                    format_card_row("Description:", LABEL_WIDTH, desc, colorize)
                                );
                            }

                            println!();
                            println!(
                                "{}",
                                format_card_section("Runtime & Paths", section_width, colorize)
                            );
                            println!(
                                "{}",
                                format_card_row(
                                    "Runtime:",
                                    LABEL_WIDTH,
                                    "AppImage (self-contained)",
                                    colorize
                                )
                            );

                            println!();
                            println!(
                                "{}",
                                format_card_section("Source & Deps", section_width, colorize)
                            );
                            println!(
                                "{}",
                                format_card_row(
                                    "Source:",
                                    LABEL_WIDTH,
                                    &format!("[{}]", src.id),
                                    colorize
                                )
                            );
                            if let Some(ref r) = p.github_repo {
                                println!(
                                    "{}",
                                    format_card_row(
                                        "GitHub Repo:",
                                        LABEL_WIDTH,
                                        &c(r, &[Colors::DIM]),
                                        colorize
                                    )
                                );
                            }
                            if let Some(ref u) = p.download_url {
                                println!(
                                    "{}",
                                    format_card_row(
                                        "Download URL:",
                                        LABEL_WIDTH,
                                        &c(u, &[Colors::DIM]),
                                        colorize
                                    )
                                );
                            }
                            println!(
                                "\n  Run {} to install this package.\n",
                                c(
                                    &format!("appmgr install {}:{}", src.id, p.id),
                                    &[Colors::BOLD, Colors::YELLOW]
                                )
                            );
                            return Ok(());
                        }
                    }
                }

                return Err(format!(
                    "Package '{}' not found in database or configured sources.",
                    target
                ));
            }
        };

        let missing_libs: Vec<String> = if pkg.format == "appimage" {
            Vec::new()
        } else {
            crate::deps::scan_app_dependencies(&pkg.install_dir)
                .missing_libs
                .into_iter()
                .map(|m| m.soname)
                .collect()
        };

        if as_json {
            let json_out = Self::build_installed_info_json(&pkg, &missing_libs);
            println!("{}", serde_json::to_string_pretty(&json_out).unwrap());
            return Ok(());
        }

        for line in Self::build_installed_info_card_lines(&pkg, &missing_libs, cols, colorize) {
            println!("{}", line);
        }
        println!();
        Ok(())
    }

    pub fn run_app(&self, app_id: &str, args: &[String]) -> Result<(), String> {
        let pkg = self
            .db
            .get(app_id)
            .ok_or_else(|| format!("Package '{}' is not installed.", app_id))?;

        let bin_path = pkg
            .binaries
            .first()
            .ok_or_else(|| format!("No executable launcher found for '{}'.", app_id))?;

        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new(bin_path);
        cmd.args(args);
        let err = cmd.exec();
        Err(format!("Failed to execute {}: {}", bin_path.display(), err))
    }

    pub fn remove_batch(&mut self, packages: &[String], assume_yes: bool) -> Result<(), String> {
        if packages.len() <= 1 {
            let pkg = packages
                .first()
                .ok_or_else(|| "No package target specified.".to_string())?;
            return self.remove(pkg, assume_yes).map(|_| ());
        }
        let mut succeeded = Vec::new();
        let mut skipped = Vec::new();
        let mut failed = Vec::new();
        for pkg in packages {
            match self.remove(pkg, assume_yes) {
                Ok(true) => succeeded.push(pkg.clone()),
                Ok(false) => skipped.push(pkg.clone()),
                Err(e) => {
                    eprintln!("{} {}: {}", apt_prefix_err("FAIL"), pkg, e);
                    failed.push((pkg.clone(), e));
                }
            }
        }
        Self::print_batch_summary("remove", &succeeded, &skipped, &failed)
    }

    pub fn remove(&mut self, app_id: &str, assume_yes: bool) -> Result<bool, String> {
        let pkg = self
            .db
            .get(app_id)
            .ok_or_else(|| format!("Package '{}' is not managed by appmgr.", app_id))?
            .clone();

        println!(
            "\nPackages to be removed (1): {}",
            c(
                &format!("{}-{}", pkg.name, pkg.version),
                &[Colors::BOLD, Colors::BRIGHT_RED]
            )
        );

        if !assume_yes
            && !confirm_dialog(
                &format!("Do you want to completely remove {}?", pkg.name),
                true,
            )
        {
            println!(
                "{} Removal aborted.",
                c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE])
            );
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

    fn prompt_select_index(prompt_label: &str, count: usize) -> Option<usize> {
        if count == 0 || !io::stdin().is_terminal() {
            return None;
        }

        loop {
            print!(
                "\n{} {} (1-{}, Enter or 'q' to cancel): ",
                c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
                prompt_label,
                count
            );
            let _ = io::stdout().flush();

            let mut input = String::new();
            match io::stdin().read_line(&mut input) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }

            let trimmed = input.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("q") || trimmed == "0" {
                return None;
            }

            if let Ok(idx) = trimmed.parse::<usize>() {
                if idx >= 1 && idx <= count {
                    return Some(idx - 1);
                }
            }

            println!(
                " {} Invalid selection '{}'. Enter a number between 1 and {}, or press Enter/'q' to cancel.",
                apt_prefix("WARN"),
                trimmed,
                count
            );
        }
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
                c(
                    &format!("{:2})", i + 1),
                    &[Colors::BOLD, Colors::BRIGHT_YELLOW]
                ),
                c(&repo.full_name, &[Colors::BOLD, Colors::BRIGHT_WHITE]),
                c(
                    &format!("★ {}", repo.stargazers_count),
                    &[Colors::BOLD, Colors::YELLOW]
                ),
                c(&format!("- {}", desc), &[Colors::DIM, Colors::WHITE])
            );
        }

        Self::prompt_select_index("Select repository to install", repos.len())
            .and_then(|idx| repos.get(idx))
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
                c(
                    &format!("{:2})", i + 1),
                    &[Colors::BOLD, Colors::BRIGHT_YELLOW]
                ),
                c(&c_asset.name, &[Colors::BOLD, Colors::BRIGHT_WHITE]),
                c(
                    &c_asset.format.to_uppercase(),
                    &[Colors::BOLD, Colors::BRIGHT_CYAN]
                ),
                mb
            );
        }

        Self::prompt_select_index("Select asset to download", candidates.len())
            .and_then(|idx| candidates.get(idx))
    }

    pub async fn sync_databases(&self) -> Result<(), String> {
        crate::registry::sync_all_sources(
            &self.http_client,
            &self.config.sources_list_file,
            &self.config.registries_cache_dir,
            &self.config.indexes_cache_dir,
        )
        .await
        .map(|_| ())
    }

    pub fn repo_manage(&self, cmd: crate::cli::RepoCommands) -> Result<(), String> {
        match cmd {
            crate::cli::RepoCommands::List { json } => {
                let effective_sources = crate::registry::get_effective_sources(
                    &self.config.sources_list_file,
                    &self.config.registries_cache_dir,
                );

                if json {
                    let json_str = serde_json::to_string_pretty(&effective_sources)
                        .map_err(|e| format!("Serialization error: {}", e))?;
                    println!("{}", json_str);
                    return Ok(());
                }

                if effective_sources.is_empty() {
                    println!(
                        "{} No sources configured in sources.list",
                        apt_prefix("WARN")
                    );
                    return Ok(());
                }

                println!(
                    "{} {}",
                    c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
                    c(
                        "Configured Package Sources:",
                        &[Colors::BOLD, Colors::BRIGHT_WHITE]
                    )
                );

                let headers = [
                    "SOURCE ID",
                    "DRIVER",
                    "STATUS",
                    "PRIO",
                    "URL / ENDPOINT",
                    "PACKAGES",
                ];
                let mut rows = Vec::new();

                for entry in &effective_sources {
                    let status = if entry.enabled {
                        c("enabled", &[Colors::BOLD, Colors::BRIGHT_GREEN])
                    } else {
                        c("disabled", &[Colors::DIM, Colors::WHITE])
                    };

                    let pkg_count = match entry.driver.as_str() {
                        "apt" => {
                            let idx_file = self
                                .config
                                .indexes_cache_dir
                                .join(format!("{}.json", entry.id));
                            if idx_file.exists() {
                                let pkgs = crate::drivers::apt::load_cached_apt_packages(&idx_file);
                                format!("{} pkgs", pkgs.len())
                            } else {
                                "not indexed".to_string()
                            }
                        }
                        "appimagehub" => {
                            let idx_file = self
                                .config
                                .indexes_cache_dir
                                .join(format!("{}.json", entry.id));
                            if idx_file.exists() {
                                let pkgs =
                                    crate::drivers::appimagehub::load_cached_appimagehub_packages(
                                        &idx_file,
                                    );
                                format!("{} pkgs", pkgs.len())
                            } else {
                                "not indexed".to_string()
                            }
                        }
                        "aur" | "github" | "gitlab" => "online API".to_string(),
                        "registry" => {
                            let reg_file = self
                                .config
                                .registries_cache_dir
                                .join(format!("{}.json", entry.id));
                            if reg_file.exists() {
                                "synced".to_string()
                            } else {
                                "not synced".to_string()
                            }
                        }
                        _ => "-".to_string(),
                    };

                    rows.push(vec![
                        c(&entry.id, &[Colors::BOLD, Colors::BRIGHT_CYAN]),
                        format!("[{}]", entry.driver.to_uppercase()),
                        status,
                        entry.priority.to_string(),
                        entry.url.clone(),
                        pkg_count,
                    ]);
                }

                print_table(&headers, &rows);
                Ok(())
            }
            crate::cli::RepoCommands::Add {
                driver,
                id,
                url,
                suite,
                components,
                priority,
            } => {
                let comps =
                    components.map(|c| c.split_whitespace().map(|s| s.to_string()).collect());
                let entry = crate::registry::SourceConfigEntry {
                    driver: driver.clone(),
                    id: id.clone(),
                    url: url.clone(),
                    enabled: true,
                    priority: priority.unwrap_or(500),
                    suite,
                    components: comps,
                    mode: None,
                    name: None,
                    description: None,
                };
                crate::registry::add_source(&self.config.sources_list_file, entry)?;
                println!(
                    "{} Added source '{}' [driver: {}] ({}) to sources.list",
                    apt_prefix("OK"),
                    c(&id, &[Colors::BOLD, Colors::BRIGHT_CYAN]),
                    c(&driver, &[Colors::BOLD, Colors::BRIGHT_YELLOW]),
                    url
                );
                println!(
                    "        Run {} to synchronize the package databases.",
                    c("appmgr update", &[Colors::BOLD, Colors::BRIGHT_YELLOW])
                );
                Ok(())
            }
            crate::cli::RepoCommands::Remove { id } => {
                let removed = crate::registry::remove_source(&self.config.sources_list_file, &id)?;
                if removed {
                    let idx_cache = self.config.indexes_cache_dir.join(format!("{}.json", id));
                    let _ = fs::remove_file(idx_cache);
                    let reg_cache = self
                        .config
                        .registries_cache_dir
                        .join(format!("{}.json", id));
                    let _ = fs::remove_file(reg_cache);
                    println!(
                        "{} Removed source '{}' and purged cache.",
                        apt_prefix("OK"),
                        c(&id, &[Colors::BOLD, Colors::BRIGHT_CYAN])
                    );
                } else {
                    println!(
                        "{} Source '{}' was not found in sources.list.",
                        apt_prefix("WARN"),
                        id
                    );
                }
                Ok(())
            }
            crate::cli::RepoCommands::Enable { id } => {
                crate::registry::set_source_enabled(&self.config.sources_list_file, &id, true)?;
                println!(
                    "{} Enabled source '{}'.",
                    apt_prefix("OK"),
                    c(&id, &[Colors::BOLD, Colors::BRIGHT_CYAN])
                );
                Ok(())
            }
            crate::cli::RepoCommands::Disable { id } => {
                crate::registry::set_source_enabled(&self.config.sources_list_file, &id, false)?;
                println!(
                    "{} Disabled source '{}'.",
                    apt_prefix("OK"),
                    c(&id, &[Colors::BOLD, Colors::BRIGHT_CYAN])
                );
                Ok(())
            }
        }
    }

    pub async fn source_manage(&self, cmd: crate::cli::SourceCommands) -> Result<(), String> {
        match cmd {
            crate::cli::SourceCommands::Lint { file } => {
                let path = Path::new(&file);
                match crate::registry::source_lint(path) {
                    Ok(recipe) => {
                        println!(
                            "{} Source definition '{}' ({}) is valid Schema v2.",
                            apt_prefix("OK"),
                            c(&recipe.source.name, &[Colors::BOLD, Colors::BRIGHT_GREEN]),
                            recipe.source.id
                        );
                        Ok(())
                    }
                    Err(errs) => {
                        for e in errs {
                            println!("{} {}", apt_prefix("FAIL"), e);
                        }
                        Err("Source linting failed".to_string())
                    }
                }
            }
            crate::cli::SourceCommands::Test { file } => {
                crate::registry::source_test(Path::new(&file), &self.http_client).await
            }
        }
    }

    pub async fn pkg_manage(&self, cmd: crate::cli::PkgCommands) -> Result<(), String> {
        match cmd {
            crate::cli::PkgCommands::New { id, output } => {
                let out_path = match output {
                    Some(o) if o.ends_with(".toml") => {
                        let p = PathBuf::from(o);
                        if let Some(parent) = p.parent() {
                            if !parent.as_os_str().is_empty() {
                                fs::create_dir_all(parent).map_err(|e| {
                                    format!("Failed to create output directory: {}", e)
                                })?;
                            }
                        }
                        p
                    }
                    Some(dir) => {
                        let out_dir = PathBuf::from(dir);
                        fs::create_dir_all(&out_dir)
                            .map_err(|e| format!("Failed to create output directory: {}", e))?;
                        out_dir.join(format!("{}.toml", id))
                    }
                    None => {
                        let out_dir = PathBuf::from("sources");
                        fs::create_dir_all(&out_dir)
                            .map_err(|e| format!("Failed to create output directory: {}", e))?;
                        out_dir.join(format!("{}.toml", id))
                    }
                };
                let template = crate::registry::render_source_recipe_template(&id);
                fs::write(&out_path, template)
                    .map_err(|e| format!("Failed to create template: {}", e))?;
                println!(
                    "{} Created new source recipe template: {}",
                    apt_prefix("OK"),
                    c(
                        &out_path.display().to_string(),
                        &[Colors::BOLD, Colors::BRIGHT_GREEN]
                    )
                );
                Ok(())
            }
            crate::cli::PkgCommands::Lint { file } => {
                self.source_manage(crate::cli::SourceCommands::Lint { file })
                    .await
            }
            crate::cli::PkgCommands::Test { file, dry_run: _ } => {
                self.source_manage(crate::cli::SourceCommands::Test { file })
                    .await
            }
        }
    }

    pub async fn download_direct_url(
        &self,
        url: &str,
        dest_path: &Path,
        asset_name: &str,
        expected_sha256: Option<&str>,
    ) -> Result<(), String> {
        if let Some(parent) = dest_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let temp_dest = dest_path.with_extension("part");

        let resp = self
            .http_client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Failed to initiate download from {}: {}", url, e))?;

        if !resp.status().is_success() {
            return Err(format!("Download failed with HTTP {}", resp.status()));
        }

        let total_bytes = resp.content_length().unwrap_or(0);
        let mut bar = crate::ui::PacmanBar::new(
            total_bytes,
            &format!("Downloading {}", truncate_chars(asset_name, 20)),
        );

        let mut file = fs::File::create(&temp_dest)
            .map_err(|e| format!("Failed to create temp file: {}", e))?;
        let mut downloaded: u64 = 0;
        let mut hasher = sha2::Sha256::new();
        let mut stream = resp.bytes_stream();

        use futures_util::StreamExt;
        use sha2::Digest;

        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res.map_err(|e| format!("Error during download: {}", e))?;
            file.write_all(&chunk)
                .map_err(|e| format!("Write error: {}", e))?;
            hasher.update(&chunk);
            downloaded += chunk.len() as u64;
            bar.update(downloaded);
        }
        bar.finish(&format!("Downloaded {}", truncate_chars(asset_name, 24)));

        let calculated_hash = hex::encode(hasher.finalize());
        if let Some(expected) = expected_sha256 {
            let exp_clean = expected.trim().to_lowercase();
            if !exp_clean.is_empty() && calculated_hash != exp_clean {
                let _ = fs::remove_file(&temp_dest);
                return Err(format!(
                    "SHA-256 verification failed for {}.\nExpected: {}\nCalculated: {}",
                    asset_name, exp_clean, calculated_hash
                ));
            }
            println!(
                "{} SHA-256 verified successfully ({})",
                apt_prefix("OK"),
                c(&calculated_hash[..8], &[Colors::DIM])
            );
        }

        if fs::rename(&temp_dest, dest_path).is_err() {
            fs::copy(&temp_dest, dest_path).map_err(|e| e.to_string())?;
            let _ = fs::remove_file(&temp_dest);
        }

        Ok(())
    }

    pub async fn install_from_apt_source(
        &mut self,
        source: &crate::registry::SourceConfigEntry,
        pkg: &crate::drivers::apt::AptPackage,
        force: bool,
        assume_yes: bool,
    ) -> Result<Option<String>, String> {
        println!(
            "{} Resolving package '{}' from APT source [{}]...",
            apt_prefix("INFO"),
            c(&pkg.package, &[Colors::BOLD, Colors::BRIGHT_CYAN]),
            c(&source.id, &[Colors::BOLD, Colors::BRIGHT_GREEN])
        );

        let cache_download_dir = self.config.cache_dir.join("downloads");
        let _ = fs::create_dir_all(&cache_download_dir);

        let deb_url = if pkg.filename.starts_with("http://") || pkg.filename.starts_with("https://")
        {
            pkg.filename.clone()
        } else {
            format!(
                "{}/{}",
                source.url.trim_end_matches('/'),
                pkg.filename.trim_start_matches('/')
            )
        };

        let file_stem = pkg.filename.split('/').next_back().unwrap_or(&pkg.package);
        let deb_file_name = if file_stem.ends_with(".deb") {
            file_stem.to_string()
        } else {
            format!("{}_{}_{}.deb", pkg.package, pkg.version, pkg.architecture)
        };
        let target_path = cache_download_dir.join(&deb_file_name);

        let expected_sha256 = if pkg.sha256.is_empty() {
            None
        } else {
            Some(pkg.sha256.as_str())
        };

        self.download_direct_url(&deb_url, &target_path, &deb_file_name, expected_sha256)
            .await?;

        let installed_id = self.install_local_file(
            &target_path,
            force,
            assume_yes,
            None,
            Some(pkg.version.clone()),
            None,
            None,
            None,
            None,
            false,
            Some(pkg.package.clone()),
        )?;

        if let Some(ref id) = installed_id {
            if let Some(record) = self.db.get_mut(id) {
                record.source_type = Some("apt".to_string());
                record.registry_name = Some(source.id.clone());
                record.recipe_id = Some(pkg.package.clone());
                if let Some(ref desc) = pkg.description {
                    record.description = Some(desc.clone());
                }
            }
            let _ = self.db.save();
        }

        Ok(installed_id)
    }

    pub async fn install_from_appimagehub(
        &mut self,
        source: &crate::registry::SourceConfigEntry,
        app: &crate::drivers::appimagehub::AppImageHubPackage,
        force: bool,
        assume_yes: bool,
    ) -> Result<Option<String>, String> {
        println!(
            "{} Resolving package '{}' from AppImageHub [{}]...",
            apt_prefix("INFO"),
            c(&app.name, &[Colors::BOLD, Colors::BRIGHT_CYAN]),
            c(&source.id, &[Colors::BOLD, Colors::BRIGHT_GREEN])
        );

        if let Some(ref repo) = app.github_repo {
            println!(
                "{} Package '{}' has upstream GitHub repository: {}",
                apt_prefix("INFO"),
                c(&app.id, &[Colors::BOLD, Colors::BRIGHT_CYAN]),
                c(repo, &[Colors::BOLD, Colors::BRIGHT_GREEN])
            );
            let installed_id = self
                .install_from_github(repo, None, force, assume_yes, Some("appimage"), false)
                .await?;
            if let Some(ref id) = installed_id {
                if let Some(record) = self.db.get_mut(id) {
                    record.source_type = Some("appimagehub".to_string());
                    record.registry_name = Some(source.id.clone());
                    record.recipe_id = Some(app.id.clone());
                    if let Some(ref desc) = app.description {
                        record.description = Some(desc.clone());
                    }
                    let _ = self.db.save();
                }
            }
            return Ok(installed_id);
        }

        if let Some(ref dl_url) = app.download_url {
            let cache_download_dir = self.config.cache_dir.join("downloads");
            let _ = fs::create_dir_all(&cache_download_dir);

            let filename = format!("{}.AppImage", app.id);
            let target_path = cache_download_dir.join(&filename);

            self.download_direct_url(dl_url, &target_path, &filename, None)
                .await?;

            let installed_id = self.install_local_file(
                &target_path,
                force,
                assume_yes,
                None,
                None,
                None,
                None,
                None,
                None,
                false,
                Some(app.id.clone()),
            )?;

            if let Some(ref id) = installed_id {
                if let Some(record) = self.db.get_mut(id) {
                    record.source_type = Some("appimagehub".to_string());
                    record.registry_name = Some(source.id.clone());
                    record.recipe_id = Some(app.id.clone());
                    if let Some(ref desc) = app.description {
                        record.description = Some(desc.clone());
                    }
                    let _ = self.db.save();
                }
            }
            return Ok(installed_id);
        }

        Err(format!("Package '{}' on AppImageHub does not have an upstream GitHub repo or direct download URL.", app.id))
    }

    pub async fn resolve_package_dependencies(
        &mut self,
        app_id: &str,
        auto_deps: bool,
        no_deps: bool,
        rootless_deps: bool,
    ) -> Result<(), String> {
        self.resolve_package_dependencies_with_policy(
            app_id,
            auto_deps,
            no_deps,
            rootless_deps,
            true,
        )
        .await
    }

    async fn resolve_package_dependencies_with_policy(
        &mut self,
        app_id: &str,
        cli_auto_deps: bool,
        cli_no_deps: bool,
        cli_rootless_deps: bool,
        allow_config_none: bool,
    ) -> Result<(), String> {
        let (effective_auto, effective_no_deps, effective_rootless, allow_rootless_fallback) =
            if cli_no_deps {
                (false, true, false, false)
            } else if cli_rootless_deps {
                (false, false, true, true)
            } else if cli_auto_deps {
                (true, false, false, true)
            } else {
                match self.config.settings.deps_mode {
                    DepsMode::Ask => (false, false, false, true),
                    DepsMode::Host => (true, false, false, false),
                    DepsMode::Rootless => (false, false, true, true),
                    DepsMode::None => {
                        if allow_config_none {
                            (false, true, false, false)
                        } else {
                            (false, false, false, true)
                        }
                    }
                }
            };

        if effective_no_deps {
            return Ok(());
        }

        let pkg = match self.db.get(app_id) {
            Some(p) => p.clone(),
            None => return Ok(()),
        };

        // AppImages are self-contained with bundled libc/runtime
        if pkg.format == "appimage" {
            return Ok(());
        }

        println!(
            "{} Scanning shared library dependencies for {}...",
            c("::", &[Colors::BOLD, Colors::CYAN]),
            c(&pkg.name, &[Colors::BOLD, Colors::CYAN])
        );

        let scan = crate::deps::scan_app_dependencies(&pkg.install_dir);
        if scan.missing_libs.is_empty() {
            println!(
                " {} All shared library dependencies are satisfied.",
                apt_prefix("OK")
            );
            return Ok(());
        }

        println!(
            " {} Found {} missing shared librar{}:",
            apt_prefix("WARN"),
            scan.missing_libs.len(),
            if scan.missing_libs.len() == 1 {
                "y"
            } else {
                "ies"
            }
        );
        for m in &scan.missing_libs {
            println!("    - {}", c(&m.soname, &[Colors::BOLD, Colors::RED]));
        }

        let mut remaining_missing = scan.missing_libs;

        if !effective_rootless {
            if let Some(pm) = crate::deps::host::detect_host_package_manager() {
                if let Ok(host_res) =
                    crate::deps::host::resolve_libraries_with_host(pm, &remaining_missing)
                {
                    if !host_res.resolved_packages.is_empty() {
                        let installed = crate::deps::host::prompt_and_install_host_packages(
                            pm,
                            &host_res.resolved_packages,
                            effective_auto,
                        )?;
                        if installed {
                            // Rescan
                            let rescan = crate::deps::scan_app_dependencies(&pkg.install_dir);
                            remaining_missing = rescan.missing_libs;
                        }
                    }
                }
            }
        }

        if !remaining_missing.is_empty() && allow_rootless_fallback {
            // Tier 2: Rootless Debian deb bundle resolver
            let rootless_candidates: Vec<_> = remaining_missing
                .iter()
                .filter(|m| !crate::deps::is_core_host_only(&m.soname))
                .cloned()
                .collect();

            if !rootless_candidates.is_empty() {
                println!(
                    "\n{} Resolving {} remaining librar{} via rootless sandbox pool ({})...",
                    c("::", &[Colors::BOLD, Colors::CYAN]),
                    rootless_candidates.len(),
                    if rootless_candidates.len() == 1 {
                        "y"
                    } else {
                        "ies"
                    },
                    self.config.settings.debian_suite
                );

                let bundled_so_files = crate::deps::rootless::resolve_rootless_libraries(
                    &self.http_client,
                    &self.config.cache_dir,
                    &pkg.install_dir,
                    &rootless_candidates,
                    &self.config.settings.debian_suite,
                )
                .await?;

                if !bundled_so_files.is_empty() {
                    // Update launcher scripts to include LD_LIBRARY_PATH
                    for b in &pkg.binaries {
                        let _ = crate::deps::rootless::update_launcher_ld_path(b, &pkg.install_dir);
                    }

                    // Update package record bundled_deps in DB
                    if let Some(record) = self.db.get_mut(app_id) {
                        for so in &bundled_so_files {
                            if !record.bundled_deps.contains(so) {
                                record.bundled_deps.push(so.clone());
                            }
                        }
                        let _ = self.db.save();
                    }
                }
            }

            // Final check
            let final_scan = crate::deps::scan_app_dependencies(&pkg.install_dir);
            if final_scan.missing_libs.is_empty() {
                println!(
                    "\n {} All dependencies successfully resolved.",
                    apt_prefix("OK")
                );
            } else {
                let host_only_missing: Vec<_> = final_scan
                    .missing_libs
                    .iter()
                    .filter(|m| crate::deps::is_core_host_only(&m.soname))
                    .collect();

                if !host_only_missing.is_empty() {
                    println!(
                        "\n {} Core host librar{} must be installed via your system package manager:",
                        apt_prefix("FAIL"),
                        if host_only_missing.len() == 1 { "y" } else { "ies" }
                    );
                    for m in &host_only_missing {
                        println!("    - {}", c(&m.soname, &[Colors::BOLD, Colors::RED]));
                    }
                }
            }
        } else if remaining_missing.is_empty() {
            println!(
                "\n {} All dependencies successfully resolved.",
                apt_prefix("OK")
            );
        } else {
            println!(
                "\n {} Remaining missing libraries were not bundled (deps_mode = \"host\").",
                apt_prefix("WARN")
            );
        }

        Ok(())
    }

    pub fn build_search_json(
        query: &str,
        matches: &[crate::registry::UnifiedSearchResult],
        github_repos: &[RepoItem],
    ) -> serde_json::Value {
        json!({
            "query": query,
            "sources": matches,
            "github": github_repos,
        })
    }

    pub fn build_deps_all_json(pkgs: &[PackageRecord]) -> serde_json::Value {
        let mut reports: Vec<DepsPackageJson> =
            pkgs.iter().map(Self::build_single_deps_report).collect();
        reports.sort_by(|a, b| a.id.cmp(&b.id));
        let total_missing: usize = reports.iter().map(|r| r.missing_libs.len()).sum();
        json!({
            "packages": reports,
            "total_missing": total_missing,
        })
    }

    pub fn build_single_deps_report(pkg: &PackageRecord) -> DepsPackageJson {
        if pkg.format == "appimage" {
            return DepsPackageJson {
                id: pkg.id.clone(),
                name: pkg.name.clone(),
                format: pkg.format.clone(),
                elf_class: None,
                total_elfs: 0,
                dynamic_elfs: 0,
                bundled_deps: pkg.bundled_deps.clone(),
                missing_libs: Vec::new(),
                satisfied: true,
            };
        }

        let scan = crate::deps::scan_app_dependencies(&pkg.install_dir);
        let missing: Vec<DepsMissingLibJson> = scan
            .missing_libs
            .iter()
            .map(|m| DepsMissingLibJson {
                soname: m.soname.clone(),
                elf_class: m.elf_class.to_string(),
            })
            .collect();
        let satisfied = missing.is_empty();
        DepsPackageJson {
            id: pkg.id.clone(),
            name: pkg.name.clone(),
            format: pkg.format.clone(),
            elf_class: scan.app_elf_class.map(|c| c.to_string()),
            total_elfs: scan.total_elfs,
            dynamic_elfs: scan.dynamic_elfs,
            bundled_deps: pkg.bundled_deps.clone(),
            missing_libs: missing,
            satisfied,
        }
    }

    pub async fn deps_manage(
        &mut self,
        app_id: Option<String>,
        all: bool,
        fix: bool,
        auto_deps: bool,
        rootless_deps: bool,
        as_json: bool,
    ) -> Result<(), String> {
        if as_json && fix {
            return Err(
                "--json is only supported for dependency inspection (cannot combine with --fix)."
                    .to_string(),
            );
        }

        if all {
            let pkgs: Vec<PackageRecord> = self.db.all().into_iter().cloned().collect();
            if as_json {
                let json_out = Self::build_deps_all_json(&pkgs);
                println!("{}", serde_json::to_string_pretty(&json_out).unwrap());
                return Ok(());
            }

            if pkgs.is_empty() {
                println!("{} No packages installed.", apt_prefix("INFO"));
                return Ok(());
            }

            println!(
                "{} Checking shared library dependencies for all installed packages...\n",
                c("::", &[Colors::BOLD, Colors::CYAN])
            );

            let mut any_missing = false;
            for p in &pkgs {
                if p.format == "appimage" {
                    continue;
                }
                let scan = crate::deps::scan_app_dependencies(&p.install_dir);
                if !scan.missing_libs.is_empty() {
                    any_missing = true;
                    println!(
                        "{} {} ({}) - {} missing librar{}:",
                        apt_prefix("WARN"),
                        c(&p.name, &[Colors::BOLD, Colors::WHITE]),
                        p.id,
                        scan.missing_libs.len(),
                        if scan.missing_libs.len() == 1 {
                            "y"
                        } else {
                            "ies"
                        }
                    );
                    for m in &scan.missing_libs {
                        println!("    - {}", c(&m.soname, &[Colors::RED]));
                    }
                    if fix {
                        self.resolve_package_dependencies_with_policy(
                            &p.id,
                            auto_deps,
                            false,
                            rootless_deps,
                            false,
                        )
                        .await?;
                        println!();
                    }
                }
            }

            if !any_missing {
                println!(
                    "{} All installed application shared libraries are satisfied.\n",
                    apt_prefix("OK")
                );
            } else if !fix {
                println!(
                    "\nTip: Run {} to automatically resolve missing dependencies.\n",
                    c("appmgr deps --all --fix", &[Colors::BOLD, Colors::CYAN])
                );
            }
            return Ok(());
        }

        let id = app_id.ok_or_else(|| "Please specify a package ID or use --all.".to_string())?;

        let pkg = self
            .db
            .get(&id)
            .ok_or_else(|| format!("Package '{}' is not installed.", id))?
            .clone();

        if as_json {
            let report = Self::build_single_deps_report(&pkg);
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            return Ok(());
        }

        if pkg.format == "appimage" {
            println!(
                "{} Package '{}' is an AppImage (self-contained runtime).",
                apt_prefix("INFO"),
                pkg.id
            );
            return Ok(());
        }

        let scan = crate::deps::scan_app_dependencies(&pkg.install_dir);
        println!(
            "{} Dependency report for {}:\n",
            c("::", &[Colors::BOLD, Colors::CYAN]),
            c(&pkg.name, &[Colors::BOLD, Colors::CYAN])
        );

        if !pkg.bundled_deps.is_empty() {
            println!(
                "  Bundled rootless dependencies in deps/lib/ ({}):",
                pkg.bundled_deps.len()
            );
            for b in &pkg.bundled_deps {
                println!("    {} {}", c("•", &[Colors::GREEN]), b);
            }
            println!();
        }

        if scan.missing_libs.is_empty() {
            println!(
                " {} All dynamic shared library dependencies are satisfied.\n",
                apt_prefix("OK")
            );
            return Ok(());
        }

        println!(
            " {} Found {} missing shared librar{}:",
            apt_prefix("WARN"),
            scan.missing_libs.len(),
            if scan.missing_libs.len() == 1 {
                "y"
            } else {
                "ies"
            }
        );
        for m in &scan.missing_libs {
            println!("    - {}", c(&m.soname, &[Colors::BOLD, Colors::RED]));
        }
        println!();

        if fix {
            self.resolve_package_dependencies_with_policy(
                &pkg.id,
                auto_deps,
                false,
                rootless_deps,
                false,
            )
            .await?;
        } else {
            println!(
                "Tip: Run {} to automatically resolve missing dependencies.\n",
                c(
                    &format!("appmgr deps {} --fix", pkg.id),
                    &[Colors::BOLD, Colors::CYAN]
                )
            );
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DepsMissingLibJson {
    pub soname: String,
    pub elf_class: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DepsPackageJson {
    pub id: String,
    pub name: String,
    pub format: String,
    pub elf_class: Option<String>,
    pub total_elfs: usize,
    pub dynamic_elfs: usize,
    pub bundled_deps: Vec<String>,
    pub missing_libs: Vec<DepsMissingLibJson>,
    pub satisfied: bool,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_update_wrapper_script_with_special_chars() {
        let dir = tempdir().unwrap();
        let wrapper = dir.path().join("myapp");
        fs::write(
            &wrapper,
            "#!/usr/bin/env bash\nAPP_ROOT='/tmp/app'\nexport PATH=\"$APP_ROOT/bin:$PATH\"\nexport OLD_VAR='old'\n\nPERSISTENT_ARGS=(\n    '--old'\n)\n\nexec \"$APP_ROOT/bin/myapp\" \"${PERSISTENT_ARGS[@]}\" \"$@\"\n",
        )
        .unwrap();

        let mut env = HashMap::new();
        env.insert("SAFE_VAR".to_string(), "it's $(safe)".to_string());
        let args = vec!["--arg)with)paren".to_string(), "two words".to_string()];

        Core::update_wrapper_script(&wrapper, &env, &args).unwrap();
        let updated = fs::read_to_string(&wrapper).unwrap();

        assert!(!updated.contains("OLD_VAR"));
        assert!(!updated.contains("--old"));
        assert!(updated.contains("export PATH=\"$APP_ROOT/bin:$PATH\""));
        assert!(updated.contains("export SAFE_VAR='it'\\''s $(safe)'"));
        assert!(updated.contains("    '--arg)with)paren'\n    'two words'\n)"));

        // Second update clears args and replaces env cleanly even with ')' in args
        let env2 = HashMap::new();
        Core::update_wrapper_script(&wrapper, &env2, &[]).unwrap();
        let cleared = fs::read_to_string(&wrapper).unwrap();
        assert!(!cleared.contains("SAFE_VAR"));
        assert!(!cleared.contains("--arg)with)paren"));
        assert!(cleared.contains("PERSISTENT_ARGS=()\n"));
    }

    #[test]
    fn test_info_card_sections_and_pre_ansi_alignment() {
        let pkg = PackageRecord {
            id: "demo".to_string(),
            name: "DemoApp".to_string(),
            version: "1.2.3".to_string(),
            format: "appimage".to_string(),
            mode: "user".to_string(),
            install_date: "2026-11-24T10:00:00+01:00".to_string(),
            install_dir: PathBuf::from("/tmp/apps/demo"),
            binaries: vec![PathBuf::from("/tmp/bin/demo")],
            repo: Some("owner/demo".to_string()),
            tag: Some("v1.2.3".to_string()),
            pinned: true,
            description: Some("Demo description".to_string()),
            total_size_bytes: Some(2 * 1024 * 1024),
            ..Default::default()
        };

        let lines = Core::build_installed_info_card_lines(&pkg, &[], 80, true);
        let plain: Vec<String> = lines.iter().map(|l| crate::ui::strip_ansi(l)).collect();

        assert!(plain.iter().any(|l| l.contains("── Package ─")));
        assert!(plain.iter().any(|l| l.contains("── Runtime & Paths ─")));
        assert!(plain.iter().any(|l| l.contains("── Source & Deps ─")));

        let name_row = plain.iter().find(|l| l.contains("Name:")).unwrap();
        let desc_row = plain.iter().find(|l| l.contains("Description:")).unwrap();
        let loc_row = plain.iter().find(|l| l.contains("Location:")).unwrap();
        let repo_row = plain.iter().find(|l| l.contains("GitHub Repo:")).unwrap();

        assert_eq!(name_row.find("DemoApp"), desc_row.find("Demo description"));
        assert_eq!(name_row.find("DemoApp"), loc_row.find("/tmp/apps/demo"));
        assert_eq!(name_row.find("DemoApp"), repo_row.find("owner/demo"));
    }

    #[test]
    fn test_json_serialization_for_search_info_and_deps() {
        let pkg = PackageRecord {
            id: "demo".to_string(),
            name: "DemoApp".to_string(),
            version: "1.2.3".to_string(),
            format: "appimage".to_string(),
            install_dir: PathBuf::from("/tmp/nonexistent-demo"),
            ..Default::default()
        };

        let info_json = Core::build_installed_info_json(&pkg, &["libfoo.so.1".to_string()]);
        let info_str = serde_json::to_string_pretty(&info_json).unwrap();
        assert!(!info_str.contains('\x1b'));
        let info_parsed: serde_json::Value = serde_json::from_str(&info_str).unwrap();
        assert_eq!(info_parsed["kind"], "installed");
        assert_eq!(info_parsed["id"], "demo");
        assert_eq!(info_parsed["missing_libs"][0], "libfoo.so.1");

        let deps_json = Core::build_deps_all_json(&[pkg]);
        let deps_str = serde_json::to_string_pretty(&deps_json).unwrap();
        assert!(!deps_str.contains('\x1b'));
        let deps_parsed: serde_json::Value = serde_json::from_str(&deps_str).unwrap();
        assert_eq!(deps_parsed["total_missing"], 0);
        assert_eq!(deps_parsed["packages"][0]["satisfied"], true);

        let search_matches = vec![crate::registry::UnifiedSearchResult {
            source_id: "core-apt".to_string(),
            driver: "apt".to_string(),
            package_id: "gh".to_string(),
            name: "gh".to_string(),
            version: "2.40.0".to_string(),
            format: "deb".to_string(),
            description: "GitHub CLI".to_string(),
            priority: 100,
        }];
        let search_json = Core::build_search_json("gh", &search_matches, &[]);
        let search_str = serde_json::to_string_pretty(&search_json).unwrap();
        assert!(!search_str.contains('\x1b'));
        let search_parsed: serde_json::Value = serde_json::from_str(&search_str).unwrap();
        assert_eq!(search_parsed["query"], "gh");
        assert_eq!(search_parsed["sources"][0]["package_id"], "gh");
        assert_eq!(search_parsed["github"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_keep_backups_rotation_and_batch_pin() {
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

        let install_dir = cfg.apps_dir.join("app1");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(install_dir.join("bin"), b"v1").unwrap();

        let mut pkg = PackageRecord {
            id: "app1".to_string(),
            name: "app1".to_string(),
            version: "1.0.0".to_string(),
            format: "bin".to_string(),
            install_dir: install_dir.clone(),
            ..Default::default()
        };

        // keep_backups = 2 retains slot 0 (v2) + prev-1 (v1)
        let b1 = Core::prepare_upgrade_backup(&cfg.backups_dir, &pkg, 2).unwrap();
        Core::finalize_upgrade_backup(&cfg.backups_dir, &pkg.id, &b1, 2);

        pkg.version = "2.0.0".to_string();
        fs::write(install_dir.join("bin"), b"v2").unwrap();
        let b2 = Core::prepare_upgrade_backup(&cfg.backups_dir, &pkg, 2).unwrap();
        Core::finalize_upgrade_backup(&cfg.backups_dir, &pkg.id, &b2, 2);

        let cur_meta: PackageRecord = serde_json::from_str(
            &fs::read_to_string(cfg.backups_dir.join("app1/meta.json")).unwrap(),
        )
        .unwrap();
        let prev_meta: PackageRecord = serde_json::from_str(
            &fs::read_to_string(cfg.backups_dir.join("app1/prev-1/meta.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(cur_meta.version, "2.0.0");
        assert_eq!(prev_meta.version, "1.0.0");

        Core::promote_previous_backup(&cfg.backups_dir.join("app1"));
        let promoted: PackageRecord = serde_json::from_str(
            &fs::read_to_string(cfg.backups_dir.join("app1/meta.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(promoted.version, "1.0.0");

        // keep_backups = 0 cleans backup on finalize
        let tmp_b = Core::prepare_upgrade_backup(&cfg.backups_dir, &pkg, 0).unwrap();
        assert!(tmp_b.exists());
        Core::finalize_upgrade_backup(&cfg.backups_dir, &pkg.id, &tmp_b, 0);
        assert!(!tmp_b.exists());
        assert!(!cfg.backups_dir.join("app1").exists());

        // Batch pin: pins valid app1 and reports error for missing app
        let mut core = Core::new(&cfg).unwrap();
        core.db.add(pkg).unwrap();
        let res = core.pin_batch(&["app1".to_string(), "missing_app".to_string()]);
        assert!(res.is_err());
        assert!(core.db.get("app1").unwrap().pinned);
    }
}
