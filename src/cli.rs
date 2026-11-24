use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

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

    #[arg(
        long = "color",
        value_enum,
        global = true,
        help = "Colorize output (auto, always, never)"
    )]
    pub color: Option<ColorArg>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorArg {
    Auto,
    Always,
    Never,
}

impl From<ColorArg> for crate::ui::ColorMode {
    fn from(value: ColorArg) -> Self {
        match value {
            ColorArg::Auto => crate::ui::ColorMode::Auto,
            ColorArg::Always => crate::ui::ColorMode::Always,
            ColorArg::Never => crate::ui::ColorMode::Never,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(
        alias = "i",
        alias = "in",
        about = "Install application locally, from GitHub (owner/repo[@tag]), or search"
    )]
    Install {
        #[arg(
            required = true,
            num_args = 1..,
            help = "Package paths, GitHub targets (owner/repo[@tag]), registry IDs, or search queries"
        )]
        packages: Vec<String>,

        #[arg(
            short = 'f',
            long = "force",
            help = "Force overwrite existing installation"
        )]
        force: bool,

        #[arg(
            short = 'y',
            long = "yes",
            alias = "noconfirm",
            help = "Assume yes to prompts"
        )]
        yes: bool,

        #[arg(
            long = "repo",
            help = "Associate package with GitHub repository (owner/repo)"
        )]
        repo: Option<String>,

        #[arg(long = "prefer", value_enum, help = "Preferred asset format")]
        prefer: Option<FormatPreference>,

        #[arg(long = "pre", help = "Allow pre-release versions")]
        pre: bool,

        #[arg(
            long = "auto-deps",
            help = "Automatically install missing host dependencies"
        )]
        auto_deps: bool,

        #[arg(
            long = "no-deps",
            help = "Skip dynamic dependency check and resolution"
        )]
        no_deps: bool,

        #[arg(
            long = "rootless-deps",
            help = "Bundle missing libraries into local sandbox (no sudo)"
        )]
        rootless_deps: bool,

        #[arg(
            long = "isolate",
            help = "Isolate application home directory inside a Bubblewrap sandbox"
        )]
        isolate: bool,
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

        #[arg(
            long = "driver",
            help = "Filter by driver type (apt, appimagehub, aur, github)"
        )]
        driver: Option<String>,

        #[arg(long = "offline", help = "Search only local repository cache")]
        offline: bool,

        #[arg(long = "json", help = "Output machine-readable JSON")]
        json: bool,
    },

    #[command(
        alias = "sync",
        about = "Synchronize package databases from configured sources"
    )]
    Update,

    #[command(
        alias = "repo",
        alias = "sources",
        about = "Manage, validate, and test package sources and recipes (Schema v2)"
    )]
    Source {
        #[command(subcommand)]
        command: SourceCommands,
    },

    #[command(
        about = "Diagnose and repair shared library dependencies for installed applications"
    )]
    Deps {
        #[arg(help = "Application ID to check")]
        app: Option<String>,

        #[arg(
            long = "all",
            help = "Check dependencies for all installed applications"
        )]
        all: bool,

        #[arg(long = "fix", help = "Resolve and install missing dependencies")]
        fix: bool,

        #[arg(
            long = "auto-deps",
            help = "Automatically confirm host package installation"
        )]
        auto_deps: bool,

        #[arg(
            long = "rootless-deps",
            help = "Bundle missing libraries into local sandbox (no sudo)"
        )]
        rootless_deps: bool,

        #[arg(long = "json", help = "Output machine-readable JSON")]
        json: bool,
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
        about = "Check and upgrade packages from GitHub Releases, APT, and AppImageHub"
    )]
    Upgrade {
        #[arg(help = "Specific application to upgrade (optional)")]
        app: Option<String>,

        #[arg(
            short = 'c',
            long = "check",
            help = "Check for available updates without installing"
        )]
        check: bool,

        #[arg(
            short = 'y',
            long = "yes",
            alias = "noconfirm",
            help = "Assume yes to prompts"
        )]
        yes: bool,

        #[arg(short = 'f', long = "force", help = "Force upgrade even if pinned")]
        force: bool,

        #[arg(long = "prefer", value_enum, help = "Preferred asset format")]
        prefer: Option<FormatPreference>,

        #[arg(long = "pre", help = "Include pre-releases in update check")]
        pre: bool,

        #[arg(
            long = "notes",
            help = "Show compact release notes preview before confirming upgrade"
        )]
        notes: bool,

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
        #[arg(
            required = true,
            num_args = 1..,
            help = "Names or IDs of installed applications"
        )]
        packages: Vec<String>,

        #[arg(
            short = 'y',
            long = "yes",
            alias = "noconfirm",
            help = "Assume yes to prompts"
        )]
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

        #[arg(long = "json", help = "Output machine-readable JSON")]
        json: bool,
    },

    #[command(
        alias = "check",
        about = "Check system health, FUSE, and broken symlinks"
    )]
    Doctor {
        #[arg(long = "json", help = "Output machine-readable JSON")]
        json: bool,
    },

    #[command(about = "Launch an installed application")]
    Run {
        #[arg(help = "Application name or ID")]
        app: String,

        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            help = "Arguments passed to application"
        )]
        args: Vec<String>,
    },

    #[command(about = "Manage custom environment variables for an application")]
    Env {
        #[arg(help = "Application name or ID")]
        app: String,

        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            help = "Variable pairs to set (e.g. KEY=VALUE)"
        )]
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

        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            help = "Persistent flags to pass"
        )]
        cli_args: Vec<String>,

        #[arg(long = "clear", help = "Clear all persistent launch flags")]
        clear: bool,

        #[arg(long = "list", help = "List configured launch flags")]
        list: bool,
    },

    #[command(about = "Configure Bubblewrap sandbox isolation profile for an application")]
    Sandbox {
        #[arg(help = "Application name or ID")]
        app: String,

        #[arg(long = "profile", value_enum, help = "Sandbox isolation profile")]
        profile: Option<SandboxProfileArg>,

        #[arg(
            long = "no-net",
            conflicts_with = "allow_net",
            help = "Disable network access inside sandbox (--unshare-net)"
        )]
        no_net: bool,

        #[arg(
            long = "allow-net",
            conflicts_with = "no_net",
            help = "Allow network access inside sandbox"
        )]
        allow_net: bool,

        #[arg(
            long = "home-dir",
            help = "Custom directory mounted as $HOME inside sandbox"
        )]
        home_dir: Option<PathBuf>,

        #[arg(long = "reset", help = "Reset sandbox settings to package default")]
        reset: bool,
    },

    #[command(about = "Freeze application version(s) to skip general upgrades")]
    Pin {
        #[arg(required = true, num_args = 1.., help = "Application names or IDs")]
        apps: Vec<String>,
    },

    #[command(about = "Unfreeze application version(s) to allow updates")]
    Unpin {
        #[arg(required = true, num_args = 1.., help = "Application names or IDs")]
        apps: Vec<String>,
    },

    #[command(about = "Restore previous version of an application from cache")]
    Rollback {
        #[arg(help = "Application name or ID")]
        app: String,

        #[arg(
            short = 'y',
            long = "yes",
            alias = "noconfirm",
            help = "Assume yes to prompt"
        )]
        yes: bool,
    },

    #[command(about = "Clean rollback backups and temporary cache")]
    Clean {
        #[arg(
            short = 'y',
            long = "yes",
            alias = "noconfirm",
            help = "Assume yes to prompt"
        )]
        yes: bool,
    },

    #[command(about = "Export installed applications and sources to a manifest (JSON/TOML)")]
    Export {
        #[arg(help = "Destination file (default: appmgr-manifest.json)")]
        file: Option<String>,
    },

    #[command(about = "Sync sources and applications from a manifest (JSON/TOML)")]
    Import {
        #[arg(help = "Path to manifest file (JSON or TOML)")]
        file: String,

        #[arg(
            long = "diff",
            help = "Preview declarative change plan without modifying the system"
        )]
        diff: bool,

        #[arg(long = "apply", help = "Apply manifest changes after preview")]
        apply: bool,

        #[arg(
            short = 'y',
            long = "yes",
            alias = "noconfirm",
            help = "Assume yes to all prompts"
        )]
        yes: bool,
    },

    #[command(about = "Refresh desktop database and icon theme caches")]
    UpdateDb,

    #[cfg(feature = "self-update")]
    #[command(
        name = "self-update",
        about = "Update appmgr itself to the latest version"
    )]
    SelfUpdate {
        #[arg(
            short = 'c',
            long = "check",
            help = "Check for available appmgr updates without installing"
        )]
        check: bool,

        #[arg(long = "pre", help = "Allow pre-release versions")]
        pre: bool,

        #[arg(
            short = 'y',
            long = "yes",
            alias = "noconfirm",
            help = "Assume yes to prompt"
        )]
        yes: bool,
    },

    #[command(about = "Generate shell completions (bash, zsh, fish)")]
    Completions {
        #[arg(value_enum, help = "Shell to generate completions for")]
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand, Debug)]
pub enum SourceCommands {
    #[command(about = "List configured sources in sources.list")]
    List {
        #[arg(long = "json", help = "Output machine-readable JSON")]
        json: bool,
    },
    #[command(about = "Add a source to sources.list")]
    Add {
        #[arg(help = "Driver or registry type (apt, appimagehub, aur, github, registry)")]
        driver: String,
        #[arg(help = "Source identifier (e.g. core, my-apt)")]
        id: String,
        #[arg(help = "Source URL or index endpoint")]
        url: String,
        #[arg(long = "suite", help = "Debian suite / distribution branch (for apt)")]
        suite: Option<String>,
        #[arg(
            long = "components",
            help = "Debian components (comma or space separated)"
        )]
        components: Option<String>,
        #[arg(long = "priority", help = "Priority ranking (0..1000)")]
        priority: Option<i32>,
    },
    #[command(about = "Remove a source from sources.list and delete its cache")]
    Remove {
        #[arg(help = "Source identifier to remove")]
        id: String,
    },
    #[command(about = "Enable a previously disabled source")]
    Enable {
        #[arg(help = "Source identifier to enable")]
        id: String,
    },
    #[command(about = "Disable a source without deleting its configuration")]
    Disable {
        #[arg(help = "Source identifier to disable")]
        id: String,
    },
    #[command(about = "Generate a new Schema v2 source recipe template")]
    New {
        #[arg(help = "Source ID for the new recipe")]
        id: String,
        #[arg(short = 'o', long = "output", help = "Destination TOML file path")]
        output: Option<String>,
    },
    #[command(about = "Lint and validate a source definition TOML against Schema v2")]
    Lint {
        #[arg(help = "Path to source TOML file")]
        file: String,
    },
    #[command(about = "Test source endpoint reachability and index access")]
    Test {
        #[arg(help = "Path to source TOML file")]
        file: String,
        #[arg(
            long = "dry-run",
            help = "Validate recipe and print endpoints without network checks"
        )]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum PkgCommands {
    #[command(about = "Generate a new Schema v2 source recipe template")]
    New {
        #[arg(help = "Source ID for the new recipe")]
        id: String,
        #[arg(short = 'o', long = "output", help = "Destination TOML file path")]
        output: Option<String>,
    },
    #[command(about = "Lint and validate a TOML source recipe against Schema v2")]
    Lint {
        #[arg(help = "Path to TOML recipe file")]
        file: String,
    },
    #[command(about = "Test source resolution and URL reachability for a recipe")]
    Test {
        #[arg(help = "Path to TOML recipe file")]
        file: String,
        #[arg(long = "dry-run", help = "Emulate checks without network requests")]
        dry_run: bool,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxProfileArg {
    None,
    OptOnly,
    IsolatedHome,
    Offline,
}

impl From<SandboxProfileArg> for crate::db::SandboxProfile {
    fn from(value: SandboxProfileArg) -> Self {
        match value {
            SandboxProfileArg::None => crate::db::SandboxProfile::None,
            SandboxProfileArg::OptOnly => crate::db::SandboxProfile::OptOnly,
            SandboxProfileArg::IsolatedHome => crate::db::SandboxProfile::IsolatedHome,
            SandboxProfileArg::Offline => crate::db::SandboxProfile::Offline,
        }
    }
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
