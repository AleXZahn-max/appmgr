mod cli;
mod config;
mod core;
mod db;
mod desktop;
mod doctor;
mod formats;
mod github;
mod sandbox;
mod self_update;
mod ui;

use clap::{CommandFactory, Parser};
use cli::{Cli, Commands};
use config::Config;
use core::Core;
use doctor::Doctor;
use self_update::SelfUpdater;
use std::io;
use ui::print_banner;

#[tokio::main]
async fn main() {
    let args = Cli::parse();

    let cmd = match args.command {
        Some(c) => c,
        None => {
            print_banner();
            let mut cmd = Cli::command();
            let _ = cmd.print_help();
            println!();
            return;
        }
    };

    if let Commands::Completions { shell } = cmd {
        let mut cmd = Cli::command();
        clap_complete::generate(shell, &mut cmd, "appmgr", &mut io::stdout());
        return;
    }

    let config = match Config::new(args.system, args.user) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error initializing configuration: {}", e);
            std::process::exit(1);
        }
    };

    let global_yes = args.yes;
    let mut core = Core::new(&config);

    let res = match cmd {
        Commands::Install {
            package,
            force,
            yes,
            repo,
            prefer,
            pre,
        } => {
            core.install(
                &package,
                force,
                yes || global_yes,
                repo,
                prefer.map(|p| p.as_str()),
                pre,
            )
            .await
            .map(|_| ())
        }
        Commands::Search { query } => core.search(&query).await,
        Commands::Link { app, repo } => core.link(&app, &repo).await.map(|_| ()),
        Commands::Upgrade {
            app,
            check,
            yes,
            force,
            prefer,
            pre,
            json,
        } => core
            .upgrade(
                app.as_deref(),
                check,
                yes || global_yes,
                prefer.map(|p| p.as_str()),
                pre,
                force,
                json,
            )
            .await
            .map(|_| ()),
        Commands::Remove { package, yes } => {
            core.remove(&package, yes || global_yes).map(|_| ())
        }
        Commands::List { filter_type, json } => {
            core.list_installed(filter_type.as_deref(), json);
            Ok(())
        }
        Commands::Info { target } => {
            core.info(&target);
            Ok(())
        }
        Commands::Doctor => {
            let doc = Doctor::new(&config);
            doc.run_all_checks();
            Ok(())
        }
        Commands::Run { app, args } => {
            core.run_app(&app, &args);
            Ok(())
        }
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
        } => core
            .args_manage(&app, &cli_args, clear, list)
            .map(|_| ()),
        Commands::Pin { app } => core.pin_app(&app).map(|_| ()),
        Commands::Unpin { app } => core.unpin_app(&app).map(|_| ()),
        Commands::Rollback { app, yes } => {
            core.rollback(&app, yes || global_yes).map(|_| ())
        }
        Commands::Clean { yes } => core.clean(yes || global_yes).map(|_| ()),
        Commands::Export { file } => core.export_manifest(file.as_deref()).map(|_| ()),
        Commands::Import { file, yes } => {
            core.import_manifest(&file, yes || global_yes).await.map(|_| ())
        }
        Commands::UpdateDb => {
            desktop::run_system_triggers(&config.applications_dir, &config.icons_dir);
            Ok(())
        }
        Commands::SelfUpdate { check, pre, yes } => {
            let updater = SelfUpdater::new();
            updater
                .run(check, pre, yes || global_yes)
                .await
                .map(|_| ())
        }
        Commands::Completions { .. } => unreachable!(),
    };

    if let Err(e) = res {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}
