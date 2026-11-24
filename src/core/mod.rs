pub mod install;
pub mod manifest;
pub mod ops;
pub mod upgrade;

#[allow(unused_imports)]
pub use ops::{DepsMissingLibJson, DepsPackageJson};

use crate::config::Config;
use crate::db::Database;
use crate::github::GitHubClient;
use crate::ui::{apt_prefix, c, Colors};
use std::io::{self, IsTerminal, Write};

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

    pub(crate) fn print_batch_summary(
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

    pub(crate) fn prompt_select_index(prompt_label: &str, count: usize) -> Option<usize> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{PackageOrigin, PackageRecord};
    use crate::drivers::apt::AptPackage;
    use crate::formats::archive::ArchivePackage;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

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

        let tmp_b = Core::prepare_upgrade_backup(&cfg.backups_dir, &pkg, 0).unwrap();
        assert!(tmp_b.exists());
        Core::finalize_upgrade_backup(&cfg.backups_dir, &pkg.id, &tmp_b, 0);
        assert!(!tmp_b.exists());
        assert!(!cfg.backups_dir.join("app1").exists());

        let mut core = Core::new(&cfg).unwrap();
        core.db.add(pkg).unwrap();
        let res = core.pin_batch(&["app1".to_string(), "missing_app".to_string()]);
        assert!(res.is_err());
        assert!(core.db.get("app1").unwrap().pinned);
    }

    #[cfg(unix)]
    #[test]
    fn test_staging_atomic_failure_and_full_rollback_with_symlinks() {
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

        // Create a valid v1 tarball with a binary, secondary binary, and symlinked library
        let src_v1 = dir.path().join("src_v1");
        fs::create_dir_all(src_v1.join("bin")).unwrap();
        fs::create_dir_all(src_v1.join("lib")).unwrap();
        fs::write(src_v1.join("bin/demo"), b"#!/bin/sh\necho v1\n").unwrap();
        fs::write(src_v1.join("bin/demo-ctl"), b"#!/bin/sh\necho ctl1\n").unwrap();
        fs::write(src_v1.join("lib/libdemo.so.1.0"), b"elf").unwrap();
        std::os::unix::fs::symlink("libdemo.so.1.0", src_v1.join("lib/libdemo.so.1")).unwrap();

        let tar_v1 = dir.path().join("demo-1.0.0.tar.gz");
        {
            let file = fs::File::create(&tar_v1).unwrap();
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            tar.follow_symlinks(false);
            tar.append_dir_all(".", &src_v1).unwrap();
            tar.finish().unwrap();
        }

        let mut core = Core::new(&cfg).unwrap();
        let installed = core
            .install_local_file(
                &tar_v1, false, true, None, None, None, None, None, None, false, None,
            )
            .unwrap();
        assert_eq!(installed.as_deref(), Some("demo"));

        // Attach a desktop file and icon to v1 record to verify full desktop/icon rollback
        let desktop_path = cfg.applications_dir.join("appmgr-demo.desktop");
        fs::write(&desktop_path, "[Desktop Entry]\nName=Demo v1\n").unwrap();
        let icon_path = cfg.icons_dir.join("hicolor/scalable/apps/demo.svg");
        fs::create_dir_all(icon_path.parent().unwrap()).unwrap();
        fs::write(&icon_path, b"<svg>v1</svg>").unwrap();

        {
            let rec = core.db.get_mut("demo").unwrap();
            rec.desktop_files.push(desktop_path.clone());
            rec.icons.push(icon_path.clone());
            rec.env.insert("DEMO_V".to_string(), "one".to_string());
            rec.set_origin(PackageOrigin::GitHub {
                repo: "owner/demo".to_string(),
                tag: Some("v1.0.0".to_string()),
                etag: None,
                asset_pattern: None,
            });
        }
        core.db.save().unwrap();
        crate::launcher::write_package_launchers(core.db.get("demo").unwrap(), &cfg.bin_dir)
            .unwrap();

        // Failing install with invalid source-only archive MUST NOT overwrite working v1 or leave staging!
        let bad_src = dir.path().join("bad_src");
        fs::create_dir_all(&bad_src).unwrap();
        fs::write(bad_src.join("Cargo.toml"), b"[package]\nname=\"demo\"\n").unwrap();
        let bad_tar = dir.path().join("demo-9.9.9.tar.gz");
        {
            let file = fs::File::create(&bad_tar).unwrap();
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            tar.append_dir_all(".", &bad_src).unwrap();
            tar.finish().unwrap();
        }
        let bad_res = core.install_local_file(
            &bad_tar, true, true, None, None, None, None, None, None, false, None,
        );
        assert!(bad_res.is_err());
        assert!(cfg.apps_dir.join("demo/bin/demo").exists());
        assert_eq!(
            fs::read_to_string(cfg.apps_dir.join("demo/bin/demo")).unwrap(),
            "#!/bin/sh\necho v1\n"
        );
        let staging_leftovers: Vec<_> = fs::read_dir(&cfg.apps_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".staging-"))
            .collect();
        assert!(staging_leftovers.is_empty());

        // Prepare backup of v1, then install v2 (without secondary demo-ctl and modified desktop/icon)
        let v1_snapshot = core.db.get("demo").unwrap().clone();
        let backup_dir = Core::prepare_upgrade_backup(&cfg.backups_dir, &v1_snapshot, 1).unwrap();
        assert!(backup_dir.join("meta.json").exists());

        let src_v2 = dir.path().join("src_v2");
        fs::create_dir_all(src_v2.join("bin")).unwrap();
        fs::write(src_v2.join("bin/demo"), b"#!/bin/sh\necho v2\n").unwrap();
        let tar_v2 = dir.path().join("demo-2.0.0.tar.gz");
        {
            let file = fs::File::create(&tar_v2).unwrap();
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            tar.append_dir_all(".", &src_v2).unwrap();
            tar.finish().unwrap();
        }

        core.install_local_file(
            &tar_v2, true, true, None, None, None, None, None, None, false, None,
        )
        .unwrap();
        fs::write(&desktop_path, "[Desktop Entry]\nName=Demo v2\n").unwrap();
        fs::write(&icon_path, b"<svg>v2</svg>").unwrap();
        assert_eq!(core.db.get("demo").unwrap().version, "2.0.0");

        // Full rollback restores v1 files, symlink, launchers, desktop file, icon, and DB record!
        assert!(core.rollback("demo", true).unwrap());
        let restored = core.db.get("demo").unwrap();
        assert_eq!(restored.version, "1.0.0");
        assert_eq!(
            fs::read_to_string(cfg.apps_dir.join("demo/bin/demo")).unwrap(),
            "#!/bin/sh\necho v1\n"
        );
        let sym_meta = fs::symlink_metadata(cfg.apps_dir.join("demo/lib/libdemo.so.1")).unwrap();
        assert!(sym_meta.file_type().is_symlink());
        assert_eq!(
            fs::read_link(cfg.apps_dir.join("demo/lib/libdemo.so.1")).unwrap(),
            PathBuf::from("libdemo.so.1.0")
        );
        assert!(cfg.bin_dir.join("demo").exists());
        assert!(cfg.bin_dir.join("demo-ctl").exists());
        let launcher_txt = fs::read_to_string(cfg.bin_dir.join("demo")).unwrap();
        assert!(launcher_txt.contains("export DEMO_V='one'"));
        assert_eq!(
            fs::read_to_string(&desktop_path).unwrap(),
            "[Desktop Entry]\nName=Demo v1\n"
        );
        assert_eq!(fs::read(&icon_path).unwrap(), b"<svg>v1</svg>");
        assert!(!ArchivePackage::is_path_safe(
            Path::new("/tmp"),
            Path::new("../bad")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_multi_binary_archive_and_collision_avoidance() {
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

        // Pre-populate a colliding binary owned by another app
        let existing_helper = cfg.bin_dir.join("shared-cli");
        fs::write(&existing_helper, b"other-app").unwrap();

        let src = dir.path().join("suite_src");
        fs::create_dir_all(src.join("bin")).unwrap();
        fs::write(src.join("bin/suite"), b"#!/bin/sh\necho main\n").unwrap();
        fs::write(src.join("bin/suite-adm"), b"#!/bin/sh\necho adm\n").unwrap();
        fs::write(src.join("bin/shared-cli"), b"#!/bin/sh\necho shared\n").unwrap();

        let tar_path = dir.path().join("suite-1.0.0.tar.gz");
        {
            let file = fs::File::create(&tar_path).unwrap();
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            tar.append_dir_all(".", &src).unwrap();
            tar.finish().unwrap();
        }

        let mut core = Core::new(&cfg).unwrap();
        let installed = core
            .install_local_file(
                &tar_path, false, true, None, None, None, None, None, None, false, None,
            )
            .unwrap();
        assert_eq!(installed.as_deref(), Some("suite"));

        let rec = core.db.get("suite").unwrap();
        assert!(cfg.bin_dir.join("suite").exists());
        assert!(cfg.bin_dir.join("suite-adm").exists());
        // Original colliding file must not be overwritten; prefixed fallback is used
        assert_eq!(fs::read(&existing_helper).unwrap(), b"other-app");
        assert!(cfg.bin_dir.join("suite-shared-cli").exists());
        assert!(rec.binaries.contains(&cfg.bin_dir.join("suite-adm")));
        assert!(rec.binaries.contains(&cfg.bin_dir.join("suite-shared-cli")));
    }

    fn build_test_deb(dest: &Path, pkg: &str, arch: &str, files: &[(&str, &[u8])]) {
        let mut control_tar_bytes = Vec::new();
        {
            let mut tb = tar::Builder::new(&mut control_tar_bytes);
            let control_txt = format!(
                "Package: {}\nVersion: 1.2.3\nArchitecture: {}\nDescription: Test pkg\n",
                pkg, arch
            );
            let mut header = tar::Header::new_gnu();
            header.set_size(control_txt.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tb.append_data(&mut header, "control", control_txt.as_bytes())
                .unwrap();
            tb.finish().unwrap();
        }

        let mut data_tar_bytes = Vec::new();
        {
            let mut tb = tar::Builder::new(&mut data_tar_bytes);
            for (rel_path, content) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                tb.append_data(&mut header, rel_path, *content).unwrap();
            }
            tb.finish().unwrap();
        }

        let file = fs::File::create(dest).unwrap();
        let mut ab = ar::Builder::new(file);

        let mut ch = ar::Header::new(b"control.tar".to_vec(), control_tar_bytes.len() as u64);
        ch.set_mode(0o644);
        ab.append(&ch, &control_tar_bytes[..]).unwrap();

        let mut dh = ar::Header::new(b"data.tar".to_vec(), data_tar_bytes.len() as u64);
        dh.set_mode(0o644);
        ab.append(&dh, &data_tar_bytes[..]).unwrap();
    }

    #[test]
    fn test_deb_arch_validation_cli_skip_desktop_and_icon_patching() {
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
        let mut core = Core::new(&cfg).unwrap();

        // 1. Incompatible arch must fail before touching apps_dir
        let bad_arch = if std::env::consts::ARCH == "aarch64" {
            "amd64"
        } else {
            "arm64"
        };
        let bad_deb = dir.path().join("badarch.deb");
        build_test_deb(
            &bad_deb,
            "badarch",
            bad_arch,
            &[("usr/bin/badarch", b"#!/bin/sh\necho hi\n")],
        );
        let bad_res = core.install_local_file(
            &bad_deb, false, true, None, None, None, None, None, None, false, None,
        );
        assert!(bad_res.is_err());
        assert!(!cfg.apps_dir.join("badarch").exists());

        // 2. Pure CLI .deb without source .desktop must NOT generate a .desktop file
        let cli_deb = dir.path().join("clitool.deb");
        build_test_deb(
            &cli_deb,
            "clitool",
            "all",
            &[("usr/bin/clitool", b"#!/bin/sh\necho cli\n")],
        );
        core.install_local_file(
            &cli_deb, false, true, None, None, None, None, None, None, false, None,
        )
        .unwrap();
        let cli_rec = core.db.get("clitool").unwrap();
        assert_eq!(cli_rec.is_gui, Some(false));
        assert!(cli_rec.desktop_files.is_empty());

        // 3. GUI .deb with .desktop + SVG icon installs SVG to scalable and updates Icon=pkg_id
        let gui_deb = dir.path().join("guiapp.deb");
        build_test_deb(
            &gui_deb,
            "guiapp",
            "all",
            &[
                ("usr/bin/guiapp", b"#!/bin/sh\necho gui\n"),
                (
                    "usr/share/applications/guiapp.desktop",
                    b"[Desktop Entry]\nName=GuiApp\nExec=/usr/bin/guiapp %F\nIcon=original-name\n",
                ),
                (
                    "usr/share/icons/hicolor/scalable/apps/original-name.svg",
                    b"<svg/>",
                ),
            ],
        );
        core.install_local_file(
            &gui_deb, false, true, None, None, None, None, None, None, false, None,
        )
        .unwrap();
        let gui_rec = core.db.get("guiapp").unwrap();
        assert_eq!(gui_rec.is_gui, Some(true));
        assert_eq!(
            gui_rec.icons,
            vec![cfg.icons_dir.join("hicolor/scalable/apps/guiapp.svg")]
        );
        let dt_content = fs::read_to_string(&gui_rec.desktop_files[0]).unwrap();
        assert!(dt_content.contains("Icon=guiapp"));
        assert!(dt_content.contains(&format!(
            "Exec=\"{}\" %F",
            cfg.bin_dir.join("guiapp").display()
        )));
    }

    #[tokio::test]
    async fn test_clean_downloads_source_remove_and_dry_run_and_apt_upgrade_check() {
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

        let mut core = Core::new(&cfg).unwrap();

        // Populate downloads cache + staging leftover + backups
        let dl_dir = cfg.cache_dir.join("downloads");
        fs::create_dir_all(&dl_dir).unwrap();
        fs::write(dl_dir.join("old.deb"), vec![1u8; 4096]).unwrap();
        fs::create_dir_all(cfg.apps_dir.join(".staging-stale-1")).unwrap();
        fs::write(cfg.apps_dir.join(".staging-stale-1/file"), b"junk").unwrap();
        fs::create_dir_all(cfg.backups_dir.join("oldapp")).unwrap();
        fs::write(cfg.backups_dir.join("oldapp/file"), b"bak").unwrap();

        assert!(core.clean(true).unwrap());
        assert!(!dl_dir.join("old.deb").exists());
        assert!(!cfg.apps_dir.join(".staging-stale-1").exists());
        assert!(!cfg.backups_dir.join("oldapp").exists());

        // Test source add/remove purges .json, .etag, and .meta.json + rejects gitlab
        assert!(core
            .source_manage(crate::cli::SourceCommands::Add {
                driver: "gitlab".to_string(),
                id: "gl".to_string(),
                url: "https://gitlab.com".to_string(),
                suite: None,
                components: None,
                priority: None,
            })
            .await
            .is_err());

        core.source_manage(crate::cli::SourceCommands::Add {
            driver: "apt".to_string(),
            id: "myapt".to_string(),
            url: "https://example.com/deb".to_string(),
            suite: Some("stable".to_string()),
            components: Some("main,contrib non-free".to_string()),
            priority: Some(500),
        })
        .await
        .unwrap();

        let idx_json = cfg.indexes_cache_dir.join("myapt.json");
        let idx_etag = cfg.indexes_cache_dir.join("myapt.etag");
        let idx_meta = cfg.indexes_cache_dir.join("myapt.meta.json");
        let mut apt_map = std::collections::HashMap::new();
        apt_map.insert(
            "demo".to_string(),
            AptPackage {
                package: "demo".to_string(),
                version: "2.0.0".to_string(),
                architecture: "amd64".to_string(),
                filename: "pool/main/d/demo_2.0.0_amd64.deb".to_string(),
                size: 100,
                sha256: "deadbeef".to_string(),
                homepage: None,
                description: None,
            },
        );
        fs::write(&idx_json, serde_json::to_vec(&apt_map).unwrap()).unwrap();
        fs::write(&idx_etag, "\"abc\"").unwrap();
        crate::drivers::write_index_metadata(&idx_json, 1);

        // Verify unified upgrade check detects APT update from cached index
        let mut installed_apt = PackageRecord {
            id: "demo".to_string(),
            name: "demo".to_string(),
            version: "1.0.0".to_string(),
            format: "deb".to_string(),
            install_dir: cfg.apps_dir.join("demo"),
            ..Default::default()
        };
        installed_apt.set_origin(PackageOrigin::Apt {
            source_id: "myapt".to_string(),
            package: "demo".to_string(),
        });
        core.db.add(installed_apt).unwrap();
        assert!(core
            .upgrade(Some("demo"), true, true, None, false, false, true, true)
            .await
            .unwrap());

        // Test source remove deletes .json, .etag, and .meta.json
        core.source_manage(crate::cli::SourceCommands::Remove {
            id: "myapt".to_string(),
        })
        .await
        .unwrap();
        assert!(!idx_json.exists());
        assert!(!idx_etag.exists());
        assert!(!idx_meta.exists());

        // Test source test --dry-run succeeds offline
        let recipe_file = dir.path().join("demo.toml");
        fs::write(
            &recipe_file,
            crate::registry::render_source_recipe_template("demo"),
        )
        .unwrap();
        core.source_manage(crate::cli::SourceCommands::Test {
            file: recipe_file.to_string_lossy().to_string(),
            dry_run: true,
        })
        .await
        .unwrap();
    }

    #[test]
    fn test_format_release_notes_preview_cleaning_and_truncation() {
        let raw_md = r#"<!-- auto-generated -->
<details><summary>Details</summary></details>
![badge](https://img.shields.io/badge/build-passing-green)

## What's Changed
* **Feature**: Added [streaming parser](https://example.com/pr/1) with `zero-copy` mode and very long explanation text that should wrap or truncate cleanly
* Fix crash on startup

## Breaking Changes
* Removed deprecated `--legacy-config` flag

**Full Changelog**: https://github.com/owner/repo/compare/v1.0...v2.0
"#;
        let preview = crate::core::upgrade::format_release_notes_preview(raw_md, 48, 6);
        assert!(!preview.is_empty());
        assert_eq!(preview[0], "Breaking Changes:");
        assert!(preview[1].contains("Removed deprecated --legacy-config flag"));
        assert!(!preview.iter().any(|l| l.contains("<!--")));
        assert!(!preview.iter().any(|l| l.contains("<details>")));
        assert!(!preview.iter().any(|l| l.contains("https://")));
        assert!(!preview.iter().any(|l| l.contains("**")));
        assert!(!preview.iter().any(|l| l.contains("Full Changelog")));
        for line in &preview {
            assert!(
                line.chars().count() <= 48,
                "Line exceeded width 48: '{}' ({})",
                line,
                line.chars().count()
            );
        }
    }
}
