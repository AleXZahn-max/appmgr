use super::Core;
use crate::db::{PackageOrigin, PackageRecord, SandboxConfig, SandboxProfile};
use crate::desktop::run_system_triggers;
use crate::drivers::appimagehub::{
    get_cached_appimagehub_packages, invalidate_appimagehub_cache, load_cached_appimagehub_count,
};
use crate::drivers::apt::{get_cached_apt_packages, invalidate_apt_cache, load_cached_apt_count};
use crate::formats::detect_format;
use crate::github::RepoItem;
use crate::launcher::{is_valid_env_name, write_package_launchers};
use crate::ui::{
    apt_prefix, apt_prefix_err, c, confirm_dialog, format_card_row, format_card_section,
    print_table, should_color_stdout, Colors, Spinner,
};
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Command;

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

fn dir_size_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    if path.exists() {
        for e in walkdir::WalkDir::new(path).into_iter().flatten() {
            if e.file_type().is_file() {
                total += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

impl Core {
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
                        &target, false, false, None, None, false, false, false, false, false,
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

    pub async fn source_manage(&self, cmd: crate::cli::SourceCommands) -> Result<(), String> {
        match cmd {
            crate::cli::SourceCommands::List { json } => {
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
                                format!("{} pkgs", load_cached_apt_count(&idx_file))
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
                                format!("{} pkgs", load_cached_appimagehub_count(&idx_file))
                            } else {
                                "not indexed".to_string()
                            }
                        }
                        "aur" | "github" => "online API".to_string(),
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
            crate::cli::SourceCommands::Add {
                driver,
                id,
                url,
                suite,
                components,
                priority,
            } => {
                let valid_driver = crate::registry::validate_source_driver(&driver)?;
                let comps = components.and_then(|c| {
                    let parsed = crate::registry::parse_components_list(&c);
                    if parsed.is_empty() {
                        None
                    } else {
                        Some(parsed)
                    }
                });
                let entry = crate::registry::SourceConfigEntry {
                    driver: valid_driver.clone(),
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
                    c(&valid_driver, &[Colors::BOLD, Colors::BRIGHT_YELLOW]),
                    url
                );
                println!(
                    "        Run {} to synchronize the package databases.",
                    c("appmgr update", &[Colors::BOLD, Colors::BRIGHT_YELLOW])
                );
                Ok(())
            }
            crate::cli::SourceCommands::Remove { id } => {
                let removed = crate::registry::remove_source(&self.config.sources_list_file, &id)?;
                if removed {
                    let idx_cache = self.config.indexes_cache_dir.join(format!("{}.json", id));
                    invalidate_apt_cache(&idx_cache);
                    invalidate_appimagehub_cache(&idx_cache);
                    let _ = fs::remove_file(&idx_cache);
                    let _ = fs::remove_file(crate::drivers::index_meta_path(&idx_cache));
                    let _ =
                        fs::remove_file(self.config.indexes_cache_dir.join(format!("{}.etag", id)));

                    // Also remove any lingering per-prefix etag files if present
                    let etag_prefix = format!("{}.", id);
                    for dir in [
                        &self.config.indexes_cache_dir,
                        &self.config.registries_cache_dir,
                    ] {
                        if let Ok(entries) = fs::read_dir(dir) {
                            for entry in entries.flatten() {
                                let name = entry.file_name().to_string_lossy().to_string();
                                if name.starts_with(&etag_prefix) && name.ends_with(".etag") {
                                    let _ = fs::remove_file(entry.path());
                                }
                            }
                        }
                    }

                    let reg_cache = self
                        .config
                        .registries_cache_dir
                        .join(format!("{}.json", id));
                    let _ = fs::remove_file(&reg_cache);
                    let _ = fs::remove_file(
                        self.config
                            .registries_cache_dir
                            .join(format!("{}.etag", id)),
                    );
                    let _ = fs::remove_file(crate::drivers::index_meta_path(&reg_cache));

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
            crate::cli::SourceCommands::Enable { id } => {
                if crate::registry::set_source_enabled(&self.config.sources_list_file, &id, true)? {
                    println!(
                        "{} Enabled source '{}'.",
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
            crate::cli::SourceCommands::Disable { id } => {
                if crate::registry::set_source_enabled(&self.config.sources_list_file, &id, false)?
                {
                    println!(
                        "{} Disabled source '{}'.",
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
            crate::cli::SourceCommands::New { id, output } => {
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
            crate::cli::SourceCommands::Test { file, dry_run } => {
                crate::registry::source_test(Path::new(&file), &self.http_client, dry_run).await
            }
        }
    }

    pub async fn pkg_manage(&self, cmd: crate::cli::PkgCommands) -> Result<(), String> {
        match cmd {
            crate::cli::PkgCommands::New { id, output } => {
                self.source_manage(crate::cli::SourceCommands::New { id, output })
                    .await
            }
            crate::cli::PkgCommands::Lint { file } => {
                self.source_manage(crate::cli::SourceCommands::Lint { file })
                    .await
            }
            crate::cli::PkgCommands::Test { file, dry_run } => {
                self.source_manage(crate::cli::SourceCommands::Test { file, dry_run })
                    .await
            }
        }
    }

    pub fn clean(&self, assume_yes: bool) -> Result<bool, String> {
        let backup_bytes = dir_size_bytes(&self.config.backups_dir);
        let downloads_dir = self.config.cache_dir.join("downloads");
        let downloads_bytes = dir_size_bytes(&downloads_dir);

        let mut staging_bytes = 0u64;
        let mut staging_dirs = Vec::new();
        if self.config.apps_dir.exists() {
            if let Ok(entries) = fs::read_dir(&self.config.apps_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with(".staging-") || name.starts_with(".old-") {
                        let p = entry.path();
                        if p.is_dir() {
                            staging_bytes += dir_size_bytes(&p);
                            staging_dirs.push(p);
                        }
                    }
                }
            }
        }

        let mut temp_bytes = staging_bytes;
        let mut tmp_dirs = Vec::new();
        if let Ok(entries) = fs::read_dir("/tmp") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("appmgr_") {
                    let p = entry.path();
                    if p.is_dir() {
                        temp_bytes += dir_size_bytes(&p);
                        tmp_dirs.push(p);
                    }
                }
            }
        }

        let total_bytes = backup_bytes + downloads_bytes + temp_bytes;
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
            "   Downloaded cache:   {:.1} MB",
            downloads_bytes as f64 / (1024.0 * 1024.0)
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
        if downloads_dir.exists() {
            let _ = fs::remove_dir_all(&downloads_dir);
            let _ = fs::create_dir_all(&downloads_dir);
        }
        for sd in staging_dirs {
            let _ = fs::remove_dir_all(sd);
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
                write_package_launchers(updated, &self.config.bin_dir)?;
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
                if !is_valid_env_name(key) {
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
                write_package_launchers(updated, &self.config.bin_dir)?;
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
                write_package_launchers(updated, &self.config.bin_dir)?;
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
                write_package_launchers(updated, &self.config.bin_dir)?;
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

    pub(crate) fn apply_sandbox_config(
        &mut self,
        pkg_id: &str,
        cfg: SandboxConfig,
    ) -> Result<(), String> {
        if cfg.isolates_home() {
            let sbx_home = cfg
                .home_dir
                .clone()
                .unwrap_or_else(|| self.config.sandboxes_dir.join(pkg_id));
            fs::create_dir_all(&sbx_home).map_err(|e| {
                format!(
                    "Failed to create sandbox directory {}: {}",
                    sbx_home.display(),
                    e
                )
            })?;
        }
        let bwrap_flag = if cfg.is_active() {
            Some(true)
        } else {
            Some(false)
        };
        self.db
            .set_sandbox(pkg_id, Some(cfg), bwrap_flag)
            .map_err(|e| e.to_string())?;
        if let Some(updated) = self.db.get(pkg_id) {
            write_package_launchers(updated, &self.config.bin_dir)?;
        }
        Ok(())
    }

    pub fn sandbox_manage(
        &mut self,
        app_id: &str,
        profile: Option<SandboxProfile>,
        no_net: bool,
        allow_net: bool,
        home_dir: Option<PathBuf>,
        reset: bool,
    ) -> Result<bool, String> {
        let pkg = self
            .db
            .get(app_id)
            .ok_or_else(|| format!("Package '{}' is not managed by appmgr.", app_id))?
            .clone();

        if !reset && profile.is_none() && !no_net && !allow_net && home_dir.is_none() {
            let eff = pkg.effective_sandbox();
            println!(
                "\n{} {}:",
                c(
                    "Sandbox configuration for",
                    &[Colors::BOLD, Colors::BRIGHT_WHITE]
                ),
                c(&pkg.name, &[Colors::BOLD, Colors::BRIGHT_CYAN])
            );
            println!(
                "  Profile:   {}",
                c(&eff.profile.to_string(), &[Colors::BOLD, Colors::YELLOW])
            );
            println!(
                "  Network:   {}",
                if eff.disables_net() {
                    "disabled (--unshare-net)"
                } else {
                    "enabled (host)"
                }
            );
            if eff.isolates_home() {
                let home_path = eff
                    .home_dir
                    .unwrap_or_else(|| self.config.sandboxes_dir.join(&pkg.id));
                println!("  Home Dir:  {}", home_path.display());
            }
            println!();
            return Ok(true);
        }

        if reset {
            let has_opt = pkg.install_dir.join("opt").exists()
                || pkg.bind_opt.as_ref().is_some_and(|v| !v.is_empty());
            let default_bwrap = if has_opt { Some(true) } else { None };
            self.db
                .set_sandbox(&pkg.id, None, default_bwrap)
                .map_err(|e| e.to_string())?;
            if let Some(updated) = self.db.get(&pkg.id) {
                write_package_launchers(updated, &self.config.bin_dir)?;
            }
            let default_profile = if has_opt {
                SandboxProfile::OptOnly
            } else {
                SandboxProfile::None
            };
            println!(
                " {} Reset sandbox configuration for {} to default ({}).",
                apt_prefix("OK"),
                pkg.name,
                default_profile
            );
            return Ok(true);
        }

        let mut cfg = pkg.effective_sandbox();
        if let Some(p) = profile {
            let prev = cfg.profile;
            cfg.profile = p;
            match p {
                SandboxProfile::Offline => {
                    cfg.no_net = true;
                }
                SandboxProfile::None => {
                    if !no_net {
                        cfg.no_net = false;
                    }
                    if home_dir.is_none() {
                        cfg.home_dir = None;
                    }
                }
                SandboxProfile::OptOnly => {
                    if prev == SandboxProfile::Offline && !no_net {
                        cfg.no_net = false;
                    }
                    if home_dir.is_none() {
                        cfg.home_dir = None;
                    }
                }
                SandboxProfile::IsolatedHome => {
                    if prev == SandboxProfile::Offline && !no_net {
                        cfg.no_net = false;
                    }
                }
            }
        }

        if no_net {
            cfg.no_net = true;
        }
        if allow_net {
            cfg.no_net = false;
            if cfg.profile == SandboxProfile::Offline {
                let has_opt = pkg.install_dir.join("opt").exists()
                    || pkg.bind_opt.as_ref().is_some_and(|v| !v.is_empty());
                cfg.profile = if has_opt {
                    SandboxProfile::OptOnly
                } else {
                    SandboxProfile::None
                };
            }
        }
        if let Some(hd) = home_dir {
            let abs_hd = if hd.is_absolute() {
                hd
            } else {
                std::env::current_dir()
                    .map_err(|e| format!("Failed to get current directory: {}", e))?
                    .join(hd)
            };
            cfg.home_dir = Some(abs_hd);
            if matches!(cfg.profile, SandboxProfile::None | SandboxProfile::OptOnly) {
                cfg.profile = SandboxProfile::IsolatedHome;
            }
        }

        self.apply_sandbox_config(&pkg.id, cfg.clone())?;
        let net_desc = if cfg.disables_net() { "no-net" } else { "net" };
        println!(
            " {} Updated sandbox for {}: profile={}, network={}",
            apt_prefix("OK"),
            c(&pkg.name, &[Colors::BOLD, Colors::WHITE]),
            c(&cfg.profile.to_string(), &[Colors::BOLD, Colors::CYAN]),
            net_desc
        );
        if cfg.isolates_home() {
            let sbx_home = cfg
                .home_dir
                .unwrap_or_else(|| self.config.sandboxes_dir.join(&pkg.id));
            println!(
                " {} Sandbox home: {}",
                apt_prefix("INFO"),
                c(&sbx_home.display().to_string(), &[Colors::DIM])
            );
        }
        Ok(true)
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
                        "origin": p.resolved_origin(),
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
            let repo_str = match p.resolved_origin() {
                PackageOrigin::GitHub { repo, .. } => repo,
                PackageOrigin::Apt { source_id, .. } => format!("apt:{}", source_id),
                PackageOrigin::AppImageHub {
                    source_id,
                    github_repo,
                    ..
                } => github_repo.unwrap_or_else(|| format!("appimagehub:{}", source_id)),
                PackageOrigin::Local { .. } => "-".to_string(),
            };
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
        if !pkg.secondary_binaries.is_empty() {
            let sec: Vec<String> = pkg
                .secondary_binaries
                .iter()
                .map(|(k, v)| format!("{} -> {}", k, v.display()))
                .collect();
            lines.push(format_card_row(
                "Extra Bins:",
                LABEL_WIDTH,
                &c_if(colorize, &sec.join(", "), &[Colors::DIM]),
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
        let eff_sbx = pkg.effective_sandbox();
        if eff_sbx.is_active() {
            let sbx_desc = match eff_sbx.profile {
                SandboxProfile::OptOnly => {
                    if eff_sbx.disables_net() {
                        "opt-only (Bubblewrap /opt, no-net)".to_string()
                    } else {
                        "opt-only (Bubblewrap /opt isolated)".to_string()
                    }
                }
                SandboxProfile::IsolatedHome => {
                    if eff_sbx.disables_net() {
                        "isolated-home (no-net)".to_string()
                    } else {
                        "isolated-home".to_string()
                    }
                }
                SandboxProfile::Offline => "offline (--unshare-net)".to_string(),
                SandboxProfile::None => "none (no-net)".to_string(),
            };
            lines.push(format_card_row(
                "Sandbox:",
                LABEL_WIDTH,
                &sbx_desc,
                colorize,
            ));
            if let Some(ref custom_home) = eff_sbx.home_dir {
                lines.push(format_card_row(
                    "Sandbox Home:",
                    LABEL_WIDTH,
                    &c_if(colorize, &custom_home.display().to_string(), &[Colors::DIM]),
                    colorize,
                ));
            }
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
            "origin": pkg.resolved_origin(),
            "repo": pkg.repo,
            "tag": pkg.tag,
            "asset_pattern": pkg.asset_pattern,
            "source_type": pkg.source_type,
            "registry_name": pkg.registry_name,
            "recipe_id": pkg.recipe_id,
            "source_file": pkg.source_file,
            "bwrap": pkg.bwrap,
            "sandbox": pkg.sandbox,
            "effective_sandbox": pkg.effective_sandbox(),
            "binaries": pkg.binaries,
            "secondary_binaries": pkg.secondary_binaries,
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
                        let pkgs = get_cached_apt_packages(&idx_file);
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
                        let pkgs = get_cached_appimagehub_packages(&idx_file);
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
        let sbx_dir = self.config.sandboxes_dir.join(&pkg.id);
        if sbx_dir.exists() {
            let _ = fs::remove_dir_all(&sbx_dir);
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
