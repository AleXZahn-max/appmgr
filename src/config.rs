use std::path::PathBuf;

pub fn is_root() -> bool {
    unsafe {
        extern "C" {
            fn geteuid() -> u32;
        }
        geteuid() == 0
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
    pub cache_dir: PathBuf,
    pub config_dir: PathBuf,
    pub sources_list_file: PathBuf,
    pub registries_cache_dir: PathBuf,
}

impl Config {
    pub fn new(force_system: bool, force_user: bool) -> Result<Self, String> {
        if force_system && force_user {
            return Err("Cannot specify both force_system and force_user".to_string());
        }

        let mode = if force_system {
            "system".to_string()
        } else if force_user {
            "user".to_string()
        } else if is_root() {
            "system".to_string()
        } else {
            "user".to_string()
        };

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/root"));

        if mode == "system" {
            let db_dir = PathBuf::from("/var/lib/appmgr");
            let config_dir = PathBuf::from("/etc/appmgr");
            let registries_cache_dir = PathBuf::from("/var/cache/appmgr/registries");
            Ok(Self {
                mode,
                prefix: PathBuf::from("/usr/local"),
                apps_dir: PathBuf::from("/opt/appmgr/apps"),
                bin_dir: PathBuf::from("/usr/local/bin"),
                applications_dir: PathBuf::from("/usr/local/share/applications"),
                icons_dir: PathBuf::from("/usr/local/share/icons"),
                db_file: db_dir.join("installed.json"),
                backups_dir: db_dir.join("backups"),
                cache_dir: db_dir.join("cache"),
                db_dir,
                sources_list_file: config_dir.join("sources.list"),
                config_dir,
                registries_cache_dir,
            })
        } else {
            let prefix = home.join(".local");
            let db_dir = prefix.join("share/appmgr");
            let config_dir = home.join(".config/appmgr");
            let registries_cache_dir = home.join(".cache/appmgr/registries");
            Ok(Self {
                mode,
                apps_dir: db_dir.join("apps"),
                bin_dir: prefix.join("bin"),
                applications_dir: prefix.join("share/applications"),
                icons_dir: prefix.join("share/icons"),
                db_file: db_dir.join("installed.json"),
                backups_dir: db_dir.join("backups"),
                cache_dir: db_dir.join("cache"),
                db_dir,
                prefix,
                sources_list_file: config_dir.join("sources.list"),
                config_dir,
                registries_cache_dir,
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
            &self.cache_dir,
            &self.config_dir,
            &self.registries_cache_dir,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}
