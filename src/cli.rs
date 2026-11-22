use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "appmgr",
    version = env!("CARGO_PKG_VERSION"),
    about = "Universal Linux Application Combine (Pacman + APT hybrid)",
    long_about = "The next-generation Pacman + APT hybrid engine for universal Linux application management.\nSupports .deb, .AppImage, .tar.*, .zip, and Standalone raw ELF binaries with rootless isolation."
)]
pub struct Cli {
    #[arg(
        short = 'y',
        long = "yes",
        alias = "noconfirm",
        global = true,
        help = "Automatic yes to prompts"
    )]
    pub yes: bool,

    #[arg(
        long = "system",
        global = true,
        help = "Force system-wide installation mode (/opt, /usr/local)"
    )]
    pub system: bool,

    #[arg(
        long = "user",
        global = true,
        help = "Force user-level rootless mode (~/.local)"
    )]
    pub user: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(
        alias = "i",
        alias = "in",
        about = "Install application locally, from GitHub (owner/repo[@tag]), or search"
    )]
    Install {
        #[arg(help = "Path to package, GitHub target (owner/repo[@tag]), or search query")]
        package: String,

        #[arg(short = 'f', long = "force", help = "Force overwrite existing installation")]
        force: bool,

        #[arg(short = 'y', long = "yes", alias = "noconfirm", help = "Assume yes to prompts")]
        yes: bool,

        #[arg(long = "repo", help = "Associate package with GitHub repository (owner/repo)")]
        repo: Option<String>,

        #[arg(long = "prefer", value_enum, help = "Preferred asset format")]
        prefer: Option<FormatPreference>,

        #[arg(long = "pre", help = "Allow pre-release versions")]
        pre: bool,

        #[arg(long = "auto-deps", help = "Automatically install missing host dependencies")]
        auto_deps: bool,

        #[arg(long = "no-deps", help = "Skip dynamic dependency check and resolution")]
        no_deps: bool,

        #[arg(long = "rootless-deps", help = "Bundle missing libraries into local sandbox (no sudo)")]
        rootless_deps: bool,
    },

    #[command(
        alias = "s",
        alias = "find",
        about = "Search package sources and online registries for applications"
    )]
    Search {
        #[arg(help = "Application name or search query")]
        query: String,

        #[arg(long = "source", help = "Filter by specific source ID")]
        source: Option<String>,

        #[arg(long = "driver", help = "Filter by driver type (apt, appimagehub, aur, github, gitlab)")]
        driver: Option<String>,

        #[arg(long = "offline", help = "Search only local repository cache")]
        offline: bool,
    },

    #[command(
        alias = "sync",
        about = "Synchronize package databases from configured sources"
    )]
    Update,

    #[command(alias = "sources", about = "Manage software sources and repositories in sources.list")]
    Repo {
        #[command(subcommand)]
        command: RepoCommands,
    },

    #[command(about = "Validate and test source definitions (Schema v2)")]
    Source {
        #[command(subcommand)]
        command: SourceCommands,
    },

    #[command(about = "Diagnose and repair shared library dependencies for installed applications")]
    Deps {
        #[arg(help = "Application ID to check")]
        app: Option<String>,

        #[arg(long = "all", help = "Check dependencies for all installed applications")]
        all: bool,

        #[arg(long = "fix", help = "Resolve and install missing dependencies")]
        fix: bool,

        #[arg(long = "auto-deps", help = "Automatically confirm host package installation")]
        auto_deps: bool,

        #[arg(long = "rootless-deps", help = "Bundle missing libraries into local sandbox (no sudo)")]
        rootless_deps: bool,
    },

    #[command(about = "Maintainer tools for recipe creation, linting, and testing")]
    Pkg {
        #[command(subcommand)]
        command: PkgCommands,
    },

    #[command(about = "Link an installed application to a GitHub repository for updates")]
    Link {
        #[arg(help = "Installed application name or ID")]
        app: String,

        #[arg(help = "GitHub repository (owner/repo or URL)")]
        repo: String,
    },

    #[command(
        alias = "up",
        about = "Check and upgrade packages from GitHub Releases and registries"
    )]
    Upgrade {
        #[arg(help = "Specific application to upgrade (optional)")]
        app: Option<String>,

        #[arg(short = 'c', long = "check", help = "Check for available updates without installing")]
        check: bool,

        #[arg(short = 'y', long = "yes", alias = "noconfirm", help = "Assume yes to prompts")]
        yes: bool,

        #[arg(short = 'f', long = "force", help = "Force upgrade even if pinned")]
        force: bool,

        #[arg(long = "prefer", value_enum, help = "Preferred asset format")]
        prefer: Option<FormatPreference>,

        #[arg(long = "pre", help = "Include pre-releases in update check")]
        pre: bool,

        #[arg(long = "json", help = "Output machine-readable JSON")]
        json: bool,
    },

    #[command(
        alias = "rm",
        alias = "uninstall",
        alias = "purge",
        about = "Remove an installed application"
    )]
    Remove {
        #[arg(help = "Name or ID of installed application")]
        package: String,

        #[arg(short = 'y', long = "yes", alias = "noconfirm", help = "Assume yes to prompts")]
        yes: bool,
    },

    #[command(alias = "ls", about = "List installed applications")]
    List {
        #[arg(short = 't', long = "type", help = "Filter by package format")]
        filter_type: Option<String>,

        #[arg(long = "json", help = "Output machine-readable JSON")]
        json: bool,
    },

    #[command(about = "Display details about a package or file")]
    Info {
        #[arg(help = "Package ID or file path to inspect")]
        target: String,
    },

    #[command(alias = "check", about = "Check system health, FUSE, and broken symlinks")]
    Doctor,

    #[command(about = "Launch an installed application")]
    Run {
        #[arg(help = "Application name or ID")]
        app: String,

        #[arg(trailing_var_arg = true, allow_hyphen_values = true, help = "Arguments passed to application")]
        args: Vec<String>,
    },

    #[command(about = "Manage custom environment variables for an application")]
    Env {
        #[arg(help = "Application name or ID")]
        app: String,

        #[arg(trailing_var_arg = true, allow_hyphen_values = true, help = "Variable pairs to set (e.g. KEY=VALUE)")]
        pairs: Vec<String>,

        #[arg(long = "unset", help = "Unset an environment variable")]
        unset: Option<String>,

        #[arg(long = "list", help = "List configured environment variables")]
        list: bool,
    },

    #[command(about = "Manage persistent launch flags for an application")]
    Args {
        #[arg(help = "Application name or ID")]
        app: String,

        #[arg(trailing_var_arg = true, allow_hyphen_values = true, help = "Persistent flags to pass")]
        cli_args: Vec<String>,

        #[arg(long = "clear", help = "Clear all persistent launch flags")]
        clear: bool,

        #[arg(long = "list", help = "List configured launch flags")]
        list: bool,
    },

    #[command(about = "Freeze an application version to skip general upgrades")]
    Pin {
        #[arg(help = "Application name or ID")]
        app: String,
    },

    #[command(about = "Unfreeze an application version to allow updates")]
    Unpin {
        #[arg(help = "Application name or ID")]
        app: String,
    },

    #[command(about = "Restore previous version of an application from cache")]
    Rollback {
        #[arg(help = "Application name or ID")]
        app: String,

        #[arg(short = 'y', long = "yes", alias = "noconfirm", help = "Assume yes to prompt")]
        yes: bool,
    },

    #[command(about = "Clean rollback backups and temporary cache")]
    Clean {
        #[arg(short = 'y', long = "yes", alias = "noconfirm", help = "Assume yes to prompt")]
        yes: bool,
    },

    #[command(about = "Export installed applications to a JSON manifest")]
    Export {
        #[arg(help = "Destination file (default: appmgr-manifest.json)")]
        file: Option<String>,
    },

    #[command(about = "Install applications from a JSON manifest")]
    Import {
        #[arg(help = "Path to manifest JSON file")]
        file: String,

        #[arg(short = 'y', long = "yes", alias = "noconfirm", help = "Assume yes to all prompts")]
        yes: bool,
    },

    #[command(about = "Refresh desktop database and icon theme caches")]
    UpdateDb,

    #[cfg(feature = "self-update")]
    #[command(name = "self-update", about = "Update appmgr itself to the latest version")]
    SelfUpdate {
        #[arg(short = 'c', long = "check", help = "Check for available appmgr updates without installing")]
        check: bool,

        #[arg(long = "pre", help = "Allow pre-release versions")]
        pre: bool,

        #[arg(short = 'y', long = "yes", alias = "noconfirm", help = "Assume yes to prompt")]
        yes: bool,
    },

    #[command(about = "Generate shell completions (bash, zsh, fish)")]
    Completions {
        #[arg(value_enum, help = "Shell to generate completions for")]
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand, Debug)]
pub enum RepoCommands {
    #[command(about = "List configured repositories in sources.list")]
    List {
        #[arg(long = "json", help = "Output machine-readable JSON")]
        json: bool,
    },
    #[command(about = "Add a repository to sources.list")]
    Add {
        #[arg(help = "Driver or registry type (apt, appimagehub, aur, github, gitlab, registry)")]
        driver: String,
        #[arg(help = "Repository identifier (e.g. core, my-apt)")]
        id: String,
        #[arg(help = "Repository URL or index endpoint")]
        url: String,
        #[arg(long = "suite", help = "Debian suite / distribution branch (for apt)")]
        suite: Option<String>,
        #[arg(long = "components", help = "Debian components (for apt)")]
        components: Option<String>,
        #[arg(long = "priority", help = "Priority ranking (0..1000)")]
        priority: Option<i32>,
    },
    #[command(about = "Remove a repository from sources.list and delete its cache")]
    Remove {
        #[arg(help = "Repository identifier to remove")]
        id: String,
    },
    #[command(about = "Enable a previously disabled repository")]
    Enable {
        #[arg(help = "Repository identifier to enable")]
        id: String,
    },
    #[command(about = "Disable a repository without deleting its configuration")]
    Disable {
        #[arg(help = "Repository identifier to disable")]
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum SourceCommands {
    #[command(about = "Lint and validate a source definition TOML against Schema v2")]
    Lint {
        #[arg(help = "Path to source TOML file")]
        file: String,
    },
    #[command(about = "Test source endpoint reachability and index access")]
    Test {
        #[arg(help = "Path to source TOML file")]
        file: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum PkgCommands {
    #[command(about = "Generate a new Schema v1 package recipe template")]
    New {
        #[arg(help = "Package ID for the new recipe")]
        id: String,
        #[arg(short = 'o', long = "output", help = "Destination TOML file path")]
        output: Option<String>,
    },
    #[command(about = "Lint and validate a TOML package recipe against Schema v1")]
    Lint {
        #[arg(help = "Path to TOML recipe file")]
        file: String,
    },
    #[command(about = "Test source resolution and URL reachability for a recipe")]
    Test {
        #[arg(help = "Path to TOML recipe file")]
        file: String,
        #[arg(long = "dry-run", help = "Emulate checks without modifying system")]
        dry_run: bool,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatPreference {
    Appimage,
    Deb,
    Tar,
    Zip,
    Bin,
}

impl FormatPreference {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Appimage => "appimage",
            Self::Deb => "deb",
            Self::Tar => "tar",
            Self::Zip => "zip",
            Self::Bin => "bin",
        }
    }
}
