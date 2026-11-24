mod cli;
mod config;
mod core;
mod db;
mod deps;
mod desktop;
mod doctor;
mod drivers;
mod formats;
mod github;
mod launcher;
mod registry;
#[cfg(feature = "self-update")]
mod self_update;
mod ui;

use clap::{CommandFactory, Parser};
use cli::{Cli, Commands};
use config::Config;
use core::Core;
use doctor::Doctor;
#[cfg(feature = "self-update")]
use self_update::SelfUpdater;
use std::io;
use ui::{print_banner, set_color_mode, ColorMode};

fn resolve_initial_color_mode(cli_color: Option<cli::ColorArg>) -> ColorMode {
    match cli_color {
        Some(c) => c.into(),
        None => ColorMode::Auto,
    }
}

fn resolve_effective_color_mode(
    cli_color: Option<cli::ColorArg>,
    config_color: ColorMode,
) -> ColorMode {
    match cli_color {
        Some(c) => c.into(),
        None => {
            if ui::env_disables_color() {
                ColorMode::Never
            } else {
                config_color
            }
        }
    }
}

#[tokio::main]
async fn main() {
    tokio::spawn(async {
        let _ = tokio::signal::ctrl_c().await;
        ui::restore_cursor();
        eprintln!(
            "\n{} Operation interrupted by user.",
            ui::apt_prefix_err("WARN")
        );
        std::process::exit(130);
    });

    let args = Cli::parse();
    set_color_mode(resolve_initial_color_mode(args.color));

    if let Some(Commands::Completions { shell }) = args.command {
        let mut cmd = Cli::command();
        clap_complete::generate(shell, &mut cmd, "appmgr", &mut io::stdout());
        return;
    }

    if args.command.is_none() {
        if let Ok(cfg) = Config::new(args.system, args.user) {
            set_color_mode(resolve_effective_color_mode(
                args.color,
                cfg.settings.color.into(),
            ));
        }
        print_banner();
        return;
    }

    let config = match Config::new(args.system, args.user) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} {}", ui::apt_prefix_err("FAIL"), e);
            std::process::exit(1);
        }
    };

    set_color_mode(resolve_effective_color_mode(
        args.color,
        config.settings.color.into(),
    ));

    let cmd = args.command.unwrap();
    let global_yes = args.yes;
    let mut core = match Core::new(&config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} {}", ui::apt_prefix_err("FAIL"), e);
            std::process::exit(1);
        }
    };

    let res = match cmd {
        Commands::Install {
            packages,
            force,
            yes,
            repo,
            prefer,
            pre,
            auto_deps,
            no_deps,
            rootless_deps,
            isolate,
        } => {
            core.install_batch(
                &packages,
                force,
                yes || global_yes,
                repo,
                prefer.map(|p| p.as_str()),
                pre,
                auto_deps,
                no_deps,
                rootless_deps,
                isolate,
            )
            .await
        }
        Commands::Search {
            query,
            source,
            driver,
            offline,
            json,
        } => {
            core.search(&query, source.as_deref(), driver.as_deref(), offline, json)
                .await
        }
        Commands::Update => core.sync_databases().await,
        Commands::Source { command } => core.source_manage(command).await,
        Commands::Deps {
            app,
            all,
            fix,
            auto_deps,
            rootless_deps,
            json,
        } => {
            core.deps_manage(app, all, fix, auto_deps, rootless_deps, json)
                .await
        }
        Commands::Pkg { command } => core.pkg_manage(command).await,
        Commands::Link { app, repo } => core.link(&app, &repo).await.map(|_| ()),
        Commands::Upgrade {
            app,
            check,
            yes,
            force,
            prefer,
            pre,
            notes,
            json,
        } => core
            .upgrade(
                app.as_deref(),
                check,
                yes || global_yes,
                prefer.map(|p| p.as_str()),
                pre,
                force,
                notes,
                json,
            )
            .await
            .map(|_| ()),
        Commands::Remove { packages, yes } => core.remove_batch(&packages, yes || global_yes),
        Commands::List { filter_type, json } => {
            core.list_installed(filter_type.as_deref(), json);
            Ok(())
        }
        Commands::Info { target, json } => core.info(&target, json),
        Commands::Doctor { json } => Doctor::new(&config).and_then(|doc| doc.run_all_checks(json)),
        Commands::Run { app, args } => core.run_app(&app, &args),
        Commands::Env {
            app,
            pairs,
            unset,
            list,
        } => core
            .env_manage(&app, &pairs, unset.as_deref(), list)
            .map(|_| ()),
        Commands::Args {
            app,
            cli_args,
            clear,
            list,
        } => core.args_manage(&app, &cli_args, clear, list).map(|_| ()),
        Commands::Sandbox {
            app,
            profile,
            no_net,
            allow_net,
            home_dir,
            reset,
        } => core
            .sandbox_manage(
                &app,
                profile.map(Into::into),
                no_net,
                allow_net,
                home_dir,
                reset,
            )
            .map(|_| ()),
        Commands::Pin { apps } => core.pin_batch(&apps),
        Commands::Unpin { apps } => core.unpin_batch(&apps),
        Commands::Rollback { app, yes } => core.rollback(&app, yes || global_yes).map(|_| ()),
        Commands::Clean { yes } => core.clean(yes || global_yes).map(|_| ()),
        Commands::Export { file } => core.export_manifest(file.as_deref()).map(|_| ()),
        Commands::Import {
            file,
            diff,
            apply,
            yes,
        } => core
            .import_manifest(&file, diff, apply, yes || global_yes)
            .await
            .map(|_| ()),
        Commands::UpdateDb => {
            desktop::run_system_triggers(&config.applications_dir, &config.icons_dir);
            Ok(())
        }
        #[cfg(feature = "self-update")]
        Commands::SelfUpdate { check, pre, yes } => {
            let updater = SelfUpdater::new();
            updater.run(check, pre, yes || global_yes).await.map(|_| ())
        }
        Commands::Completions { .. } => unreachable!(),
    };

    if let Err(e) = res {
        eprintln!("{} {}", ui::apt_prefix_err("FAIL"), e);
        std::process::exit(1);
    }
}
