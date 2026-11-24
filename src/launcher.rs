use crate::db::PackageRecord;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Wraps `s` in strict POSIX single quotes, escaping embedded single quotes as `'\''`.
pub fn posix_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Validates that `name` matches `^[A-Za-z_][A-Za-z0-9_]*$`.
pub fn is_valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_alphabetic() || b == b'_' => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Formats validated environment exports using strict POSIX single-quoting.
pub fn format_env_exports(env: &HashMap<String, String>) -> Result<String, String> {
    if env.is_empty() {
        return Ok(String::new());
    }
    let sorted: BTreeMap<&String, &String> = env.iter().collect();
    let mut lines = Vec::with_capacity(sorted.len());
    for (k, v) in sorted {
        if !is_valid_env_name(k) {
            return Err(format!(
                "Invalid environment variable name '{}': must match ^[A-Za-z_][A-Za-z0-9_]*$",
                k
            ));
        }
        lines.push(format!("export {}={}", k, posix_quote(v)));
    }
    Ok(format!("\n{}", lines.join("\n")))
}

/// Formats persistent CLI arguments array using strict POSIX single-quoting.
pub fn format_persistent_args(args: &[String]) -> String {
    if args.is_empty() {
        "PERSISTENT_ARGS=()\n".to_string()
    } else {
        let quoted: Vec<String> = args
            .iter()
            .map(|a| format!("    {}", posix_quote(a)))
            .collect();
        format!("PERSISTENT_ARGS=(\n{}\n)\n", quoted.join("\n"))
    }
}

fn effective_env(record: &PackageRecord) -> HashMap<String, String> {
    let mut merged = record.default_env.clone().unwrap_or_default();
    for (k, v) in &record.env {
        merged.insert(k.clone(), v.clone());
    }
    merged
}

fn effective_args(record: &PackageRecord) -> Vec<String> {
    let mut merged = record.default_args.clone().unwrap_or_default();
    merged.extend(record.args.iter().cloned());
    merged
}

fn should_use_bwrap(record: &PackageRecord) -> bool {
    if record.bwrap == Some(false) {
        return false;
    }
    if record.bwrap == Some(true) {
        return true;
    }
    if record.bind_opt.as_ref().is_some_and(|v| !v.is_empty()) {
        return true;
    }
    record.format == "deb" && record.install_dir.join("opt").exists()
}

fn collect_opt_mounts(record: &PackageRecord) -> Vec<String> {
    let mut opt_names = BTreeSet::new();
    let opt_dir = record.install_dir.join("opt");
    if opt_dir.exists() {
        if let Ok(entries) = fs::read_dir(&opt_dir) {
            for entry in entries.flatten() {
                opt_names.insert(entry.file_name().to_string_lossy().to_string());
            }
        }
    }
    if let Some(ref binds) = record.bind_opt {
        for item in binds {
            let trimmed = item
                .trim()
                .trim_start_matches('/')
                .trim_start_matches("opt/");
            if !trimmed.is_empty() {
                opt_names.insert(trimmed.to_string());
            }
        }
    }

    opt_names
        .into_iter()
        .map(|name| {
            let quoted_sub = posix_quote(&format!("opt/{}", name));
            let quoted_target = posix_quote(&format!("/opt/{}", name));
            format!(
                "    [ -d \"$APP_ROOT/\"{} ] && BWRAP_ARGS+=(--ro-bind \"$APP_ROOT/\"{} {})",
                quoted_sub, quoted_sub, quoted_target
            )
        })
        .collect()
}

fn collect_archive_extra_ld(app_dir: &Path) -> String {
    let mut lib_dirs = Vec::new();
    for cand in &[
        "lib64",
        "usr/lib64",
        "usr/lib/x86_64-linux-gnu",
        "lib/x86_64-linux-gnu",
        "usr/lib/aarch64-linux-gnu",
        "lib/aarch64-linux-gnu",
    ] {
        if app_dir.join(cand).exists() {
            lib_dirs.push(format!("$APP_ROOT/{}", cand));
        }
    }
    if lib_dirs.is_empty() {
        String::new()
    } else {
        format!(":{}", lib_dirs.join(":"))
    }
}

/// Renders a POSIX-safe launcher script declaratively from `PackageRecord`.
pub fn render_launcher_script(record: &PackageRecord, rel_target: &Path) -> Result<String, String> {
    let env_map = effective_env(record);
    let args_vec = effective_args(record);
    let env_block = format_env_exports(&env_map)?;
    let args_block = format!("\n{}", format_persistent_args(&args_vec));
    let app_dir_quoted = posix_quote(&record.install_dir.to_string_lossy());
    let rel_str = rel_target.to_string_lossy();
    let rel_quoted = posix_quote(&rel_str);
    let pkg_id = &record.id;

    if record.format == "appimage" {
        return Ok(format!(
            r#"#!/usr/bin/env bash
# Wrapper created by appmgr for {pkg_id} (AppImage)
APP_ROOT={app_dir}
APPIMAGE="$APP_ROOT/"{rel_quoted}{env_block}
{args_block}
if [ -z "$APPIMAGE_EXTRACT_AND_RUN" ]; then
    HAS_FUSE2=0
    if command -v fusermount >/dev/null 2>&1 || command -v fusermount3 >/dev/null 2>&1; then
        if command -v ldconfig >/dev/null 2>&1 && ldconfig -p 2>/dev/null | grep -q "libfuse\.so\.2"; then
            HAS_FUSE2=1
        elif [ -e /lib/libfuse.so.2 ] || [ -e /lib64/libfuse.so.2 ] || [ -e /usr/lib/libfuse.so.2 ] || [ -e /usr/lib64/libfuse.so.2 ] || [ -e /usr/lib/x86_64-linux-gnu/libfuse.so.2 ] || [ -e /usr/lib/aarch64-linux-gnu/libfuse.so.2 ]; then
            HAS_FUSE2=1
        fi
    fi
    if [ "$HAS_FUSE2" -ne 1 ]; then
        echo "[appmgr] libfuse.so.2 not found; using --appimage-extract-and-run (install fuse2 for native FUSE mounting)." >&2
        export APPIMAGE_EXTRACT_AND_RUN=1
    fi
fi

exec "$APPIMAGE" "${{PERSISTENT_ARGS[@]}}" "$@"
"#,
            pkg_id = pkg_id,
            app_dir = app_dir_quoted,
            rel_quoted = rel_quoted,
            env_block = env_block,
            args_block = args_block,
        ));
    }

    if should_use_bwrap(record) {
        let bwrap_mount_block = collect_opt_mounts(record).join("\n");
        let is_under_opt = rel_str.starts_with("opt/");
        let container_bin_line = if is_under_opt {
            format!(
                "    CONTAINER_BIN={}",
                posix_quote(&format!("/{}", rel_str))
            )
        } else {
            format!("    CONTAINER_BIN=\"$APP_ROOT/\"{}", rel_quoted)
        };
        let extra_ld = if record.install_dir.join("opt").exists() {
            if let Some(parent) = rel_target.parent() {
                if !parent.as_os_str().is_empty() {
                    format!(":$APP_ROOT/{}", parent.display())
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        return Ok(format!(
            r#"#!/usr/bin/env bash
# Smart Wrapper created by appmgr for {pkg_id}
# Handles hardcoded /opt paths transparently via Bubblewrap user namespaces
APP_ROOT={app_dir}
TARGET_BIN="$APP_ROOT/"{rel_quoted}{env_block}
{args_block}
# 1. Bubblewrap (bwrap) rootless virtualization for hardcoded absolute paths (/opt, /usr/share)
if command -v bwrap >/dev/null 2>&1 && [ "$APPMGR_NO_BWRAP" != "1" ]; then
    BWRAP_ARGS=(
        --dev-bind /dev /dev
        --ro-bind /sys /sys
        --proc /proc
        --ro-bind /usr /usr
        --ro-bind /etc /etc
        --bind /run /run
        --bind /tmp /tmp
        --bind "$HOME" "$HOME"
        --tmpfs /opt
    )
    [ -e /bin ] && BWRAP_ARGS+=(--ro-bind /bin /bin)
    [ -e /lib ] && BWRAP_ARGS+=(--ro-bind /lib /lib)
    [ -e /lib64 ] && BWRAP_ARGS+=(--ro-bind /lib64 /lib64)
    if [ -d /opt ]; then
        for d in /opt/*; do
            [ -e "$d" ] && BWRAP_ARGS+=(--ro-bind "$d" "$d")
        done
    fi
{bwrap_mount_block}

{container_bin_line}
    export LD_LIBRARY_PATH="$APP_ROOT/deps/lib:$APP_ROOT/usr/lib:$APP_ROOT/usr/lib/x86_64-linux-gnu:$APP_ROOT/lib:$APP_ROOT/lib/x86_64-linux-gnu{extra_ld}:${{LD_LIBRARY_PATH}}"
    export XDG_DATA_DIRS="$APP_ROOT/usr/share:${{XDG_DATA_DIRS:-/usr/local/share:/usr/share}}"

    exec bwrap "${{BWRAP_ARGS[@]}}" "$CONTAINER_BIN" "${{PERSISTENT_ARGS[@]}}" "$@"
fi

# 2. Standard direct execution fallback
export PATH="$APP_ROOT/usr/bin:$APP_ROOT/bin:$PATH"
export LD_LIBRARY_PATH="$APP_ROOT/deps/lib:$APP_ROOT/usr/lib:$APP_ROOT/usr/lib/x86_64-linux-gnu:$APP_ROOT/lib:$APP_ROOT/lib/x86_64-linux-gnu{extra_ld}:${{LD_LIBRARY_PATH}}"
export XDG_DATA_DIRS="$APP_ROOT/usr/share:${{XDG_DATA_DIRS:-/usr/local/share:/usr/share}}"

exec "$TARGET_BIN" "${{PERSISTENT_ARGS[@]}}" "$@"
"#,
            pkg_id = pkg_id,
            app_dir = app_dir_quoted,
            rel_quoted = rel_quoted,
            env_block = env_block,
            args_block = args_block,
            bwrap_mount_block = bwrap_mount_block,
            container_bin_line = container_bin_line,
            extra_ld = extra_ld,
        ));
    }

    if matches!(record.format.as_str(), "tar" | "zip" | "deb") {
        let extra_ld = collect_archive_extra_ld(&record.install_dir);
        let fmt_label = record.format.as_str();
        return Ok(format!(
            r#"#!/usr/bin/env bash
# Wrapper created by appmgr for {pkg_id} ({fmt_label})
APP_ROOT={app_dir}
export PATH="$APP_ROOT/bin:$APP_ROOT/usr/bin:$APP_ROOT:$PATH"
export LD_LIBRARY_PATH="$APP_ROOT/deps/lib:$APP_ROOT/usr/lib:$APP_ROOT/lib{extra_ld}:${{LD_LIBRARY_PATH}}"
export XDG_DATA_DIRS="$APP_ROOT/share:$APP_ROOT/usr/share:${{XDG_DATA_DIRS:-/usr/local/share:/usr/share}}"{env_block}
{args_block}
exec "$APP_ROOT/"{rel_quoted} "${{PERSISTENT_ARGS[@]}}" "$@"
"#,
            pkg_id = pkg_id,
            fmt_label = fmt_label,
            app_dir = app_dir_quoted,
            rel_quoted = rel_quoted,
            extra_ld = extra_ld,
            env_block = env_block,
            args_block = args_block,
        ));
    }

    Ok(format!(
        r#"#!/usr/bin/env bash
# Wrapper created by appmgr for {pkg_id} (Standalone binary)
APP_ROOT={app_dir}
export PATH="$APP_ROOT/bin:$PATH"
export LD_LIBRARY_PATH="$APP_ROOT/deps/lib:$APP_ROOT/lib:${{LD_LIBRARY_PATH}}"{env_block}
{args_block}
exec "$APP_ROOT/"{rel_quoted} "${{PERSISTENT_ARGS[@]}}" "$@"
"#,
        pkg_id = pkg_id,
        app_dir = app_dir_quoted,
        rel_quoted = rel_quoted,
        env_block = env_block,
        args_block = args_block,
    ))
}

fn write_executable_script(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create launcher dir {}: {}", parent.display(), e))?;
    }
    if fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        let _ = fs::remove_file(path);
    }
    fs::write(path, content)
        .map_err(|e| format!("Failed to write launcher {}: {}", path.display(), e))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("Failed to chmod launcher {}: {}", path.display(), e))?;
    Ok(())
}

/// Resolves the relative target executable inside `record.install_dir` for both new and legacy records.
pub fn resolve_primary_rel_target(record: &PackageRecord) -> Result<PathBuf, String> {
    if let Some(ref ep) = record.entrypoint {
        return Ok(ep.clone());
    }

    let app_dir = &record.install_dir;
    match record.format.as_str() {
        "appimage" => {
            let expected = app_dir.join(format!("{}.AppImage", record.id));
            if expected.exists() {
                return Ok(PathBuf::from(format!("{}.AppImage", record.id)));
            }
            if let Ok(entries) = fs::read_dir(app_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file()
                        && p.extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.eq_ignore_ascii_case("appimage"))
                            .unwrap_or(false)
                    {
                        if let Ok(rel) = p.strip_prefix(app_dir) {
                            return Ok(rel.to_path_buf());
                        }
                    }
                }
            }
            Ok(PathBuf::from(format!("{}.AppImage", record.id)))
        }
        "bin" => {
            let expected = app_dir.join("bin").join(&record.id);
            if expected.exists() {
                return Ok(PathBuf::from("bin").join(&record.id));
            }
            if let Ok(entries) = fs::read_dir(app_dir.join("bin")) {
                if let Some(first) = entries.flatten().find(|e| e.path().is_file()) {
                    if let Ok(rel) = first.path().strip_prefix(app_dir) {
                        return Ok(rel.to_path_buf());
                    }
                }
            }
            Ok(PathBuf::from("bin").join(&record.id))
        }
        "deb" => {
            let primary =
                crate::formats::deb::DebPackage::discover_primary_exe(app_dir, &record.id)?;
            primary
                .strip_prefix(app_dir)
                .map(|p| p.to_path_buf())
                .map_err(|e| format!("Invalid entrypoint path: {}", e))
        }
        _ => {
            let primary = crate::formats::archive::ArchivePackage::discover_and_rank_binaries(
                app_dir, &record.id,
            )?;
            primary
                .strip_prefix(app_dir)
                .map(|p| p.to_path_buf())
                .map_err(|e| format!("Invalid entrypoint path: {}", e))
        }
    }
}

/// Regenerates all launcher scripts and clean-name symlinks declaratively from `PackageRecord`.
pub fn write_package_launchers(record: &PackageRecord, bin_dir: &Path) -> Result<(), String> {
    let rel_primary = resolve_primary_rel_target(record)?;
    let primary_launcher = record
        .binaries
        .first()
        .cloned()
        .unwrap_or_else(|| bin_dir.join(&record.id));
    let effective_bin_dir = primary_launcher.parent().unwrap_or(bin_dir);

    let primary_script = render_launcher_script(record, &rel_primary)?;
    write_executable_script(&primary_launcher, &primary_script)?;

    let mut secondary_paths = BTreeSet::new();
    for (sec_name, rel_sec) in &record.secondary_binaries {
        let sec_launcher = effective_bin_dir.join(sec_name);
        if sec_launcher == primary_launcher {
            continue;
        }
        let sec_script = render_launcher_script(record, rel_sec)?;
        write_executable_script(&sec_launcher, &sec_script)?;
        secondary_paths.insert(sec_launcher);
    }

    // Preserve or restore clean-name alias symlinks recorded in `record.binaries`
    for path in record.binaries.iter().skip(1) {
        if secondary_paths.contains(path) || *path == primary_launcher {
            continue;
        }
        if !path.exists() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::remove_file(path);
            #[cfg(unix)]
            {
                let _ = std::os::unix::fs::symlink(&primary_launcher, path);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_posix_quote_and_env_validation() {
        assert_eq!(posix_quote("simple"), "'simple'");
        assert_eq!(posix_quote("a b"), "'a b'");
        assert_eq!(
            posix_quote("it's $(whoami) `id`"),
            "'it'\\''s $(whoami) `id`'"
        );
        assert_eq!(posix_quote(""), "''");

        assert!(is_valid_env_name("FOO"));
        assert!(is_valid_env_name("_PRIVATE_1"));
        assert!(!is_valid_env_name(""));
        assert!(!is_valid_env_name("1INVALID"));
        assert!(!is_valid_env_name("BAD-NAME"));
        assert!(!is_valid_env_name("BAD=VAL"));
        assert!(!is_valid_env_name("BAD;rm"));

        let mut bad_env = HashMap::new();
        bad_env.insert("1BAD".to_string(), "val".to_string());
        assert!(format_env_exports(&bad_env).is_err());

        let mut good_env = HashMap::new();
        good_env.insert("QT_SCALE".to_string(), "1'2".to_string());
        let formatted = format_env_exports(&good_env).unwrap();
        assert_eq!(formatted, "\nexport QT_SCALE='1'\\''2'");
    }

    #[test]
    fn test_render_all_modes_and_bwrap_escaping() {
        let dir = tempdir().unwrap();
        let mut env = HashMap::new();
        env.insert("DEMO_ENV".to_string(), "a'b $(cmd)".to_string());
        let args = vec!["--flag='x'".to_string(), "--arg)paren".to_string()];

        // 1. Standalone binary mode
        let bin_rec = PackageRecord {
            id: "mybin".to_string(),
            name: "mybin".to_string(),
            version: "1.0.0".to_string(),
            format: "bin".to_string(),
            install_dir: dir.path().join("apps/mybin"),
            entrypoint: Some(PathBuf::from("bin/mybin")),
            env: env.clone(),
            args: args.clone(),
            ..Default::default()
        };
        let bin_script = render_launcher_script(&bin_rec, Path::new("bin/mybin")).unwrap();
        assert!(bin_script.contains("export DEMO_ENV='a'\\''b $(cmd)'"));
        assert!(bin_script.contains("'--flag='\\''x'\\'''"));
        assert!(bin_script.contains("'--arg)paren'"));
        assert!(bin_script.contains("exec \"$APP_ROOT/\"'bin/mybin'"));

        // 2. AppImage mode
        let ai_rec = PackageRecord {
            id: "myai".to_string(),
            name: "myai".to_string(),
            version: "1.0.0".to_string(),
            format: "appimage".to_string(),
            install_dir: dir.path().join("apps/myai"),
            entrypoint: Some(PathBuf::from("myai.AppImage")),
            env: env.clone(),
            args: args.clone(),
            ..Default::default()
        };
        let ai_script = render_launcher_script(&ai_rec, Path::new("myai.AppImage")).unwrap();
        assert!(ai_script.contains("APPIMAGE=\"$APP_ROOT/\"'myai.AppImage'"));
        assert!(ai_script.contains("APPIMAGE_EXTRACT_AND_RUN=1"));
        assert!(ai_script.contains("exec \"$APPIMAGE\" \"${PERSISTENT_ARGS[@]}\" \"$@\""));

        // 3. Archive mode (tar/zip)
        let tar_dir = dir.path().join("apps/myarch");
        fs::create_dir_all(tar_dir.join("lib64")).unwrap();
        let tar_rec = PackageRecord {
            id: "myarch".to_string(),
            name: "myarch".to_string(),
            version: "1.0.0".to_string(),
            format: "tar".to_string(),
            install_dir: tar_dir,
            entrypoint: Some(PathBuf::from("bin/tool")),
            env: env.clone(),
            args: args.clone(),
            ..Default::default()
        };
        let tar_script = render_launcher_script(&tar_rec, Path::new("bin/tool")).unwrap();
        assert!(tar_script.contains("$APP_ROOT/lib64"));
        assert!(tar_script.contains("XDG_DATA_DIRS="));
        assert!(tar_script.contains("exec \"$APP_ROOT/\"'bin/tool'"));

        // 4. Deb + bwrap (/opt) mode
        let deb_dir = dir.path().join("apps/mydeb");
        fs::create_dir_all(deb_dir.join("opt/My App")).unwrap();
        let deb_rec = PackageRecord {
            id: "mydeb".to_string(),
            name: "mydeb".to_string(),
            version: "1.0.0".to_string(),
            format: "deb".to_string(),
            install_dir: deb_dir,
            entrypoint: Some(PathBuf::from("opt/My App/mydeb")),
            bwrap: Some(true),
            env,
            args,
            ..Default::default()
        };
        let deb_script = render_launcher_script(&deb_rec, Path::new("opt/My App/mydeb")).unwrap();
        assert!(deb_script.contains("[ -e /lib ] && BWRAP_ARGS+=(--ro-bind /lib /lib)"));
        assert!(deb_script.contains("[ -e /lib64 ] && BWRAP_ARGS+=(--ro-bind /lib64 /lib64)"));
        assert!(deb_script.contains("--ro-bind \"$APP_ROOT/\"'opt/My App' '/opt/My App'"));
        assert!(deb_script.contains("CONTAINER_BIN='/opt/My App/mydeb'"));
        assert!(deb_script.contains("export DEMO_ENV='a'\\''b $(cmd)'"));
        assert!(deb_script.contains("'--flag='\\''x'\\'''"));
    }

    #[test]
    fn test_write_package_launchers_regenerates_without_regex() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        let app_dir = dir.path().join("apps/suite");
        fs::create_dir_all(app_dir.join("bin")).unwrap();
        fs::write(app_dir.join("bin/suite"), b"\x7fELF").unwrap();
        fs::write(app_dir.join("bin/suite-ctl"), b"\x7fELF").unwrap();

        let mut sec = BTreeMap::new();
        sec.insert("suite-ctl".to_string(), PathBuf::from("bin/suite-ctl"));

        let mut rec = PackageRecord {
            id: "suite".to_string(),
            name: "suite".to_string(),
            version: "1.0.0".to_string(),
            format: "tar".to_string(),
            install_dir: app_dir,
            entrypoint: Some(PathBuf::from("bin/suite")),
            secondary_binaries: sec,
            binaries: vec![bin_dir.join("suite"), bin_dir.join("suite-ctl")],
            ..Default::default()
        };

        rec.env
            .insert("SAFE_VAR".to_string(), "it's $(safe)".to_string());
        rec.args = vec!["--arg)with)paren".to_string(), "two words".to_string()];
        write_package_launchers(&rec, &bin_dir).unwrap();

        let main_content = fs::read_to_string(bin_dir.join("suite")).unwrap();
        let sec_content = fs::read_to_string(bin_dir.join("suite-ctl")).unwrap();
        assert!(main_content.contains("export SAFE_VAR='it'\\''s $(safe)'"));
        assert!(sec_content.contains("export SAFE_VAR='it'\\''s $(safe)'"));
        assert!(main_content.contains("    '--arg)with)paren'\n    'two words'\n)"));
        assert!(sec_content.contains("exec \"$APP_ROOT/\"'bin/suite-ctl'"));

        // Clear env/args and regenerate cleanly
        rec.env.clear();
        rec.args.clear();
        write_package_launchers(&rec, &bin_dir).unwrap();
        let cleared = fs::read_to_string(bin_dir.join("suite")).unwrap();
        assert!(!cleared.contains("SAFE_VAR"));
        assert!(!cleared.contains("--arg)with)paren"));
        assert!(cleared.contains("PERSISTENT_ARGS=()\n"));
    }
}
