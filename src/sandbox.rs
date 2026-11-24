use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

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

pub fn generate_deb_smart_launcher(
    app_dir: &Path,
    primary_exe: &Path,
    launcher_path: &Path,
    pkg_id: &str,
    env: &HashMap<String, String>,
    args: &[String],
) -> std::io::Result<()> {
    let rel_target = primary_exe
        .strip_prefix(app_dir)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    let opt_dir = app_dir.join("opt");
    let mut bwrap_mounts = Vec::new();
    if opt_dir.exists() {
        if let Ok(entries) = fs::read_dir(&opt_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let quoted_sub = posix_quote(&format!("opt/{}", name));
                let quoted_target = posix_quote(&format!("/opt/{}", name));
                bwrap_mounts.push(format!(
                    "    [ -d \"$APP_ROOT/\"{} ] && BWRAP_ARGS+=(--ro-bind \"$APP_ROOT/\"{} {})",
                    quoted_sub, quoted_sub, quoted_target
                ));
            }
        }
    }

    let bwrap_mount_block = bwrap_mounts.join("\n");
    let is_under_opt = rel_target.to_string_lossy().starts_with("opt/");
    let quoted_rel_target = posix_quote(&rel_target.to_string_lossy());
    let container_bin_line = if is_under_opt {
        format!(
            "    CONTAINER_BIN={}",
            posix_quote(&format!("/{}", rel_target.display()))
        )
    } else {
        format!("    CONTAINER_BIN=\"$APP_ROOT/\"{}", quoted_rel_target)
    };

    let extra_ld = if opt_dir.exists() {
        if let Some(parent) = rel_target.parent() {
            format!(":$APP_ROOT/{}", parent.display())
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let env_block = format_env_exports(env)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let args_block = format!("\n{}", format_persistent_args(args));

    let script = format!(
        r#"#!/usr/bin/env bash
# Smart Wrapper created by appmgr for {pkg_id}
# Handles hardcoded /opt paths transparently via Bubblewrap user namespaces
APP_ROOT={app_dir}
TARGET_BIN="$APP_ROOT/"{quoted_rel_target}{env_block}
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
        app_dir = posix_quote(&app_dir.to_string_lossy()),
        quoted_rel_target = quoted_rel_target,
        env_block = env_block,
        args_block = args_block,
        bwrap_mount_block = bwrap_mount_block,
        container_bin_line = container_bin_line,
        extra_ld = extra_ld,
    );

    if let Some(parent) = launcher_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(launcher_path, script)?;
    fs::set_permissions(launcher_path, fs::Permissions::from_mode(0o755))?;
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
    fn test_deb_smart_launcher_guards_lib_and_lib64() {
        let dir = tempdir().unwrap();
        let app_dir = dir.path().join("app");
        let bin_path = app_dir.join("usr/bin/demo");
        fs::create_dir_all(bin_path.parent().unwrap()).unwrap();
        fs::write(&bin_path, b"elf").unwrap();
        let launcher = dir.path().join("bin/demo");

        let mut env = HashMap::new();
        env.insert("DEMO_ENV".to_string(), "a'b $(cmd)".to_string());
        let args = vec!["--flag='x'".to_string()];

        generate_deb_smart_launcher(&app_dir, &bin_path, &launcher, "demo", &env, &args).unwrap();
        let content = fs::read_to_string(&launcher).unwrap();

        assert!(content.contains("[ -e /lib ] && BWRAP_ARGS+=(--ro-bind /lib /lib)"));
        assert!(content.contains("[ -e /lib64 ] && BWRAP_ARGS+=(--ro-bind /lib64 /lib64)"));
        assert!(content.contains("export DEMO_ENV='a'\\''b $(cmd)'"));
        assert!(content.contains("'--flag='\\''x'\\'''"));
    }
}
