pub mod appimagehub;
pub mod apt;
pub mod aur;

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexMetadata {
    pub package_count: usize,
    pub updated_at: String,
}

pub fn index_meta_path(index_file: &Path) -> PathBuf {
    let mut os = index_file.as_os_str().to_os_string();
    let s = index_file.to_string_lossy();
    if let Some(stem) = s.strip_suffix(".json") {
        return PathBuf::from(format!("{}.meta.json", stem));
    }
    os.push(".meta.json");
    PathBuf::from(os)
}

pub fn write_index_metadata(index_file: &Path, package_count: usize) {
    let meta = IndexMetadata {
        package_count,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Ok(bytes) = serde_json::to_vec(&meta) {
        let _ = fs::write(index_meta_path(index_file), bytes);
    }
}

pub fn read_index_metadata(index_file: &Path) -> Option<IndexMetadata> {
    let meta_path = index_meta_path(index_file);
    let bytes = fs::read(meta_path).ok()?;
    serde_json::from_slice::<IndexMetadata>(&bytes).ok()
}
