use goblin::elf::Elf;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::process::Command;

pub fn is_gui_binary(binary_path: &Path) -> bool {
    if let Ok(mut file) = File::open(binary_path) {
        let mut buffer = Vec::new();
        if file.read_to_end(&mut buffer).is_ok() {
            if let Ok(elf) = Elf::parse(&buffer) {
                let gui_libs = [
                    "libx11",
                    "libwayland",
                    "libgtk",
                    "libqt5",
                    "libqt6",
                    "libsdl",
                    "libgl",
                    "libcairo",
                    "libpango",
                ];
                for lib in &elf.libraries {
                    let lower = lib.to_lowercase();
                    if gui_libs.iter().any(|g| lower.contains(g)) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

pub fn create_desktop_entry(
    dest_path: &Path,
    name: &str,
    comment: &str,
    exec_path: &Path,
    icon: Option<&str>,
    categories: &str,
) -> std::io::Result<()> {
    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let icon_str = icon.unwrap_or("application-x-executable");
    let content = format!(
        r#"[Desktop Entry]
Name={}
Comment={}
Exec="{}" %U
Terminal=false
Type=Application
Icon={}
Categories={}
"#,
        name,
        comment,
        exec_path.display(),
        icon_str,
        categories
    );

    fs::write(dest_path, content)?;
    Ok(())
}

pub fn run_system_triggers(applications_dir: &Path, icons_dir: &Path) {
    if which::which("update-desktop-database").is_ok() && applications_dir.exists() {
        println!(
            " {} Processing triggers for desktop-file-utils ...",
            crate::ui::apt_prefix("TRIG")
        );
        let _ = Command::new("update-desktop-database")
            .arg(applications_dir)
            .output();
    }

    if which::which("gtk-update-icon-cache").is_ok() {
        let theme_dir = icons_dir.join("hicolor");
        if theme_dir.exists() {
            println!(
                " {} Processing triggers for hicolor-icon-theme ...",
                crate::ui::apt_prefix("TRIG")
            );
            let _ = Command::new("gtk-update-icon-cache")
                .args(["-q", "-t", "-f"])
                .arg(&theme_dir)
                .output();
        }
    }
}
