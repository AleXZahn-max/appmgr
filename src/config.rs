use crate::ui::ColorMode;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

pub fn is_root() -> bool {
    unsafe {
        extern "C" {
            fn geteuid() -> u32;
        }
        geteuid() == 0
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum DepsMode {
    #[default]
    Ask,
    Host,
    Rootless,
    None,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ColorModeConfig {
    #[default]
    Auto,
    Always,
    Never,
}

impl From<ColorModeConfig> for ColorMode {
    fn from(value: ColorModeConfig) -> Self {
        match value {
            ColorModeConfig::Auto => ColorMode::Auto,
            ColorModeConfig::Always => ColorMode::Always,
            ColorModeConfig::Never => ColorMode::Never,
        }
    }
}

fn default_keep_backups() -> usize {
    1
}

fn default_debian_suite() -> String {
    "bookworm".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct UserSettings {
    pub default_prefer: Option<String>,
    pub deps_mode: DepsMode,
    #[serde(default = "default_keep_backups")]
    pub keep_backups: usize,
    pub color: ColorModeConfig,
    #[serde(default = "default_debian_suite")]
    pub debian_suite: String,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            default_prefer: None,
            deps_mode: DepsMode::Ask,
            keep_backups: default_keep_backups(),
            color: ColorModeConfig::Auto,
            debian_suite: default_debian_suite(),
        }
    }
}

impl UserSettings {
    pub fn parse_str(content: &str) -> Result<Self, String> {
        let mut settings: UserSettings =
            toml::from_str(content).map_err(|e| format!("Invalid TOML: {}", e))?;

        if let Some(ref pref) = settings.default_prefer {
            let norm = pref.trim().to_lowercase();
            if norm.is_empty() {
                settings.default_prefer = None;
            } else {
                let valid = ["appimage", "deb", "tar", "zip", "bin"];
                if !valid.contains(&norm.as_str()) {
                    return Err(format!(
                        "Invalid default_prefer '{}' in config.toml (expected one of: {})",
                        pref,
                        valid.join(", ")
                    ));
                }
                settings.default_prefer = Some(norm);
            }
        }

        let suite_trimmed = settings.debian_suite.trim().to_string();
        if suite_trimmed.is_empty() {
            return Err("Invalid debian_suite in config.toml: cannot be empty".to_string());
        }
        settings.debian_suite = suite_trimmed;

        Ok(settings)
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read config file '{}': {}", path.display(), e))?;
        Self::parse_str(&content)
            .map_err(|e| format!("Error in config file '{}': {}", path.display(), e))
    }
}

fn resolve_xdg_dir(env_val: Option<OsString>, fallback: PathBuf) -> PathBuf {
    match env_val {
        Some(v) if !v.is_empty() => {
            let p = PathBuf::from(v);
            if p.is_absolute() {
                p
            } else {
                fallback
            }
        }
        _ => fallback,
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub mode: String,
    #[allow(dead_code)]
    pub prefix: PathBuf,
    pub apps_dir: PathBuf,
    pub bin_dir: PathBuf,
    pub applications_dir: PathBuf,
    pub icons_dir: PathBuf,
    pub db_dir: PathBuf,
    pub db_file: PathBuf,
    pub backups_dir: PathBuf,
    pub sandboxes_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub config_dir: PathBuf,
    #[allow(dead_code)]
    pub config_file: PathBuf,
    pub sources_list_file: PathBuf,
    pub registries_cache_dir: PathBuf,
    pub indexes_cache_dir: PathBuf,
    pub settings: UserSettings,
}

impl Config {
    pub fn new(force_system: bool, force_user: bool) -> Result<Self, String> {
        let home = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/root"));

        Self::resolve_with_env(
            force_system,
            force_user,
            is_root(),
            home,
            std::env::var_os("XDG_DATA_HOME"),
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("XDG_CACHE_HOME"),
        )
    }

    pub fn resolve_with_env(
        force_system: bool,
        force_user: bool,
        running_as_root: bool,
        home: PathBuf,
        xdg_data_home: Option<OsString>,
        xdg_config_home: Option<OsString>,
        xdg_cache_home: Option<OsString>,
    ) -> Result<Self, String> {
        if force_system && force_user {
            return Err("Cannot specify both force_system and force_user".to_string());
        }

        let mode = if force_system {
            "system".to_string()
        } else if force_user {
            "user".to_string()
        } else if running_as_root {
            "system".to_string()
        } else {
            "user".to_string()
        };

        if mode == "system" {
            let db_dir = PathBuf::from("/var/lib/appmgr");
            let config_dir = PathBuf::from("/etc/appmgr");
            let config_file = config_dir.join("config.toml");
            let cache_dir = PathBuf::from("/var/cache/appmgr");
            let registries_cache_dir = cache_dir.join("registries");
            let indexes_cache_dir = cache_dir.join("indexes");
            let settings = UserSettings::load(&config_file)?;
            Ok(Self {
                mode,
                prefix: PathBuf::from("/usr/local"),
                apps_dir: PathBuf::from("/opt/appmgr/apps"),
                bin_dir: PathBuf::from("/usr/local/bin"),
                applications_dir: PathBuf::from("/usr/local/share/applications"),
                icons_dir: PathBuf::from("/usr/local/share/icons"),
                db_file: db_dir.join("installed.json"),
                backups_dir: db_dir.join("backups"),
                sandboxes_dir: db_dir.join("sandboxes"),
                cache_dir,
                db_dir,
                sources_list_file: config_dir.join("sources.list"),
                config_file,
                config_dir,
                registries_cache_dir,
                indexes_cache_dir,
                settings,
            })
        } else {
            let data_home = resolve_xdg_dir(xdg_data_home, home.join(".local/share"));
            let config_home = resolve_xdg_dir(xdg_config_home, home.join(".config"));
            let cache_home = resolve_xdg_dir(xdg_cache_home, home.join(".cache"));

            let prefix = home.join(".local");
            let db_dir = data_home.join("appmgr");
            let config_dir = config_home.join("appmgr");
            let config_file = config_dir.join("config.toml");
            let cache_dir = cache_home.join("appmgr");
            let registries_cache_dir = cache_dir.join("registries");
            let indexes_cache_dir = cache_dir.join("indexes");
            let settings = UserSettings::load(&config_file)?;

            Ok(Self {
                mode,
                apps_dir: db_dir.join("apps"),
                bin_dir: prefix.join("bin"),
                applications_dir: data_home.join("applications"),
                icons_dir: data_home.join("icons"),
                db_file: db_dir.join("installed.json"),
                backups_dir: db_dir.join("backups"),
                sandboxes_dir: db_dir.join("sandboxes"),
                cache_dir,
                db_dir,
                prefix,
                sources_list_file: config_dir.join("sources.list"),
                config_file,
                config_dir,
                registries_cache_dir,
                indexes_cache_dir,
                settings,
            })
        }
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in &[
            &self.apps_dir,
            &self.bin_dir,
            &self.applications_dir,
            &self.icons_dir,
            &self.db_dir,
            &self.backups_dir,
            &self.sandboxes_dir,
            &self.cache_dir,
            &self.config_dir,
            &self.registries_cache_dir,
            &self.indexes_cache_dir,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_xdg_paths_fallback_and_overrides() {
        let home = PathBuf::from("/tmp/fake-home-appmgr");
        let cfg = Config::resolve_with_env(false, true, false, home.clone(), None, None, None)
            .expect("default fallback config");

        assert_eq!(cfg.db_dir, home.join(".local/share/appmgr"));
        assert_eq!(cfg.apps_dir, home.join(".local/share/appmgr/apps"));
        assert_eq!(cfg.applications_dir, home.join(".local/share/applications"));
        assert_eq!(cfg.icons_dir, home.join(".local/share/icons"));
        assert_eq!(cfg.config_dir, home.join(".config/appmgr"));
        assert_eq!(cfg.config_file, home.join(".config/appmgr/config.toml"));
        assert_eq!(cfg.cache_dir, home.join(".cache/appmgr"));
        assert_eq!(
            cfg.registries_cache_dir,
            home.join(".cache/appmgr/registries")
        );
        assert_eq!(cfg.indexes_cache_dir, home.join(".cache/appmgr/indexes"));

        // With explicit XDG overrides (and ignoring empty/relative values)
        let dir = tempdir().unwrap();
        let data_xdg = dir.path().join("xdg-data");
        let cfg_xdg = dir.path().join("xdg-cfg");
        let cache_xdg = dir.path().join("xdg-cache");

        let cfg2 = Config::resolve_with_env(
            false,
            true,
            false,
            home.clone(),
            Some(OsString::from(&data_xdg)),
            Some(OsString::from(&cfg_xdg)),
            Some(OsString::from(&cache_xdg)),
        )
        .expect("xdg override config");

        assert_eq!(cfg2.db_dir, data_xdg.join("appmgr"));
        assert_eq!(cfg2.applications_dir, data_xdg.join("applications"));
        assert_eq!(cfg2.config_dir, cfg_xdg.join("appmgr"));
        assert_eq!(cfg2.config_file, cfg_xdg.join("appmgr/config.toml"));
        assert_eq!(cfg2.cache_dir, cache_xdg.join("appmgr"));
        assert_eq!(
            cfg2.registries_cache_dir,
            cache_xdg.join("appmgr/registries")
        );

        // Relative or empty XDG env vars must fall back safely
        let cfg3 = Config::resolve_with_env(
            false,
            true,
            false,
            home.clone(),
            Some(OsString::from("")),
            Some(OsString::from("relative/path")),
            None,
        )
        .unwrap();
        assert_eq!(cfg3.db_dir, home.join(".local/share/appmgr"));
        assert_eq!(cfg3.config_dir, home.join(".config/appmgr"));
    }

    #[test]
    fn test_config_toml_defaults_and_overrides() {
        let dir = tempdir().unwrap();
        let cfg_home = dir.path().join("config");
        let appmgr_cfg_dir = cfg_home.join("appmgr");
        fs::create_dir_all(&appmgr_cfg_dir).unwrap();

        // Missing config.toml uses safe defaults
        let cfg_missing = Config::resolve_with_env(
            false,
            true,
            false,
            dir.path().join("home"),
            None,
            Some(OsString::from(&cfg_home)),
            None,
        )
        .unwrap();
        assert_eq!(cfg_missing.settings.default_prefer, None);
        assert_eq!(cfg_missing.settings.deps_mode, DepsMode::Ask);
        assert_eq!(cfg_missing.settings.keep_backups, 1);
        assert_eq!(cfg_missing.settings.color, ColorModeConfig::Auto);
        assert_eq!(cfg_missing.settings.debian_suite, "bookworm");

        // Partial override preserves defaults (including debian_suite = "bookworm")
        let cfg_file = appmgr_cfg_dir.join("config.toml");
        fs::write(
            &cfg_file,
            r#"
default_prefer = "DEB"
keep_backups = 3
"#,
        )
        .unwrap();
        let cfg_partial = Config::resolve_with_env(
            false,
            true,
            false,
            dir.path().join("home"),
            None,
            Some(OsString::from(&cfg_home)),
            None,
        )
        .unwrap();
        assert_eq!(cfg_partial.settings.default_prefer.as_deref(), Some("deb"));
        assert_eq!(cfg_partial.settings.deps_mode, DepsMode::Ask);
        assert_eq!(cfg_partial.settings.keep_backups, 3);
        assert_eq!(cfg_partial.settings.color, ColorModeConfig::Auto);
        assert_eq!(cfg_partial.settings.debian_suite, "bookworm");

        // Full override
        fs::write(
            &cfg_file,
            r#"
default_prefer = "appimage"
deps_mode = "rootless"
keep_backups = 0
color = "never"
debian_suite = "trixie"
"#,
        )
        .unwrap();
        let cfg_full = Config::resolve_with_env(
            false,
            true,
            false,
            dir.path().join("home"),
            None,
            Some(OsString::from(&cfg_home)),
            None,
        )
        .unwrap();
        assert_eq!(
            cfg_full.settings.default_prefer.as_deref(),
            Some("appimage")
        );
        assert_eq!(cfg_full.settings.deps_mode, DepsMode::Rootless);
        assert_eq!(cfg_full.settings.keep_backups, 0);
        assert_eq!(cfg_full.settings.color, ColorModeConfig::Never);
        assert_eq!(cfg_full.settings.debian_suite, "trixie");

        // Invalid value returns Err
        fs::write(&cfg_file, "default_prefer = \"rpm\"\n").unwrap();
        assert!(Config::resolve_with_env(
            false,
            true,
            false,
            dir.path().join("home"),
            None,
            Some(OsString::from(&cfg_home)),
            None,
        )
        .is_err());
    }
}
