use super::Core;
use crate::config::DepsMode;
use crate::db::PackageOrigin;
use crate::desktop::run_system_triggers;
use crate::drivers::appimagehub::get_cached_appimagehub_packages;
use crate::drivers::apt::get_cached_apt_packages;
use crate::formats::appimage::AppImagePackage;
use crate::formats::archive::ArchivePackage;
use crate::formats::binary::BinaryPackage;
use crate::formats::deb::DebPackage;
use crate::formats::{detect_format, PackageFormat};
use crate::github::{CandidateAsset, GitHubClient, Release, RepoItem};
use crate::launcher::write_package_launchers;
use crate::ui::{apt_prefix, apt_prefix_err, c, confirm_dialog, truncate_chars, Colors, Spinner};
use reqwest::StatusCode;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;

impl Core {
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
                        "   To add it to sources: appmgr source add {} {} {}",
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
                                let pkgs = get_cached_apt_packages(&idx_file);
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
                                let pkgs = get_cached_appimagehub_packages(&idx_file);
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
                    let pkgs = get_cached_apt_packages(&idx_file);
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
                    let pkgs = get_cached_appimagehub_packages(&idx_file);
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

        let owned_launchers = self
            .db
            .get(&name)
            .map(|rec| rec.binaries.clone())
            .unwrap_or_default();

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
                p.install_with_owned(force, env, args, &owned_launchers)?
            }
            PackageFormat::AppImage => {
                let p = AppImagePackage::new(path, &self.config, repo.clone(), github_tag.clone());
                p.install(force, env, args)?
            }
            PackageFormat::Tar | PackageFormat::Zip => {
                let p = ArchivePackage::new(path, &self.config, repo.clone(), github_tag.clone())?;
                p.install_with_owned(force, env, args, &owned_launchers)?
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
            let norm_repo = GitHubClient::parse_repo(&r).unwrap_or(r);
            record.set_origin(PackageOrigin::GitHub {
                repo: norm_repo,
                tag: github_tag,
                etag: github_etag,
                asset_pattern,
            });
        } else {
            record.set_origin(PackageOrigin::Local {
                source_file: record.source_file.clone(),
            });
        }
        record.pinned = pinned;

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

    pub async fn download_direct_url(
        &self,
        url: &str,
        dest_path: &Path,
        asset_name: &str,
        expected_sha256: Option<&str>,
    ) -> Result<(), String> {
        let _ = self
            .download_direct_url_with_meta(url, dest_path, asset_name, expected_sha256)
            .await?;
        Ok(())
    }

    pub async fn download_direct_url_with_meta(
        &self,
        url: &str,
        dest_path: &Path,
        asset_name: &str,
        expected_sha256: Option<&str>,
    ) -> Result<Option<String>, String> {
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

        let new_etag = resp
            .headers()
            .get("etag")
            .or_else(|| resp.headers().get("last-modified"))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

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

        Ok(new_etag)
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
                record.set_origin(PackageOrigin::Apt {
                    source_id: source.id.clone(),
                    package: pkg.package.clone(),
                });
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
                    record.set_origin(PackageOrigin::AppImageHub {
                        source_id: source.id.clone(),
                        app_id: app.id.clone(),
                        github_repo: Some(repo.clone()),
                        tag: record.tag.clone(),
                        etag: record.etag.clone(),
                        asset_pattern: record.asset_pattern.clone(),
                        download_url: app.download_url.clone(),
                    });
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

            let etag = self
                .download_direct_url_with_meta(dl_url, &target_path, &filename, None)
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
                    record.set_origin(PackageOrigin::AppImageHub {
                        source_id: source.id.clone(),
                        app_id: app.id.clone(),
                        github_repo: None,
                        tag: record.tag.clone(),
                        etag,
                        asset_pattern: None,
                        download_url: Some(dl_url.clone()),
                    });
                    if let Some(ref desc) = app.description {
                        record.description = Some(desc.clone());
                    }
                    let _ = self.db.save();
                }
            }
            return Ok(installed_id);
        }

        Err(format!(
            "Package '{}' on AppImageHub does not have an upstream GitHub repo or direct download URL.",
            app.id
        ))
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

    pub(crate) async fn resolve_package_dependencies_with_policy(
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
                            let rescan = crate::deps::scan_app_dependencies(&pkg.install_dir);
                            remaining_missing = rescan.missing_libs;
                        }
                    }
                }
            }
        }

        if !remaining_missing.is_empty() && allow_rootless_fallback {
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
                    if let Some(record) = self.db.get_mut(app_id) {
                        for so in &bundled_so_files {
                            if !record.bundled_deps.contains(so) {
                                record.bundled_deps.push(so.clone());
                            }
                        }
                        let _ = write_package_launchers(record, &self.config.bin_dir);
                        let _ = self.db.save();
                    }
                }
            }

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

    pub(crate) fn prompt_select_repository<'a>(
        repos: &'a [RepoItem],
        query: &str,
    ) -> Option<&'a RepoItem> {
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

    pub(crate) fn prompt_select_asset(candidates: &[CandidateAsset]) -> Option<&CandidateAsset> {
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
}
