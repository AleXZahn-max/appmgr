use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

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
                bwrap_mounts.push(format!(
                    "    [ -d \"$APP_ROOT/opt/{}\" ] && BWRAP_ARGS+=(--ro-bind \"$APP_ROOT/opt/{}\" \"/opt/{}\")",
                    name, name, name
                ));
            }
        }
    }

    let bwrap_mount_block = bwrap_mounts.join("\n");
    let is_under_opt = rel_target.to_string_lossy().starts_with("opt/");
    let container_bin_line = if is_under_opt {
        format!("    CONTAINER_BIN=\"/{}\"", rel_target.display())
    } else {
        format!("    CONTAINER_BIN=\"$APP_ROOT/{}\"", rel_target.display())
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

    let mut env_lines = Vec::new();
    for (k, v) in env {
        env_lines.push(format!("export {}=\"{}\"", k, v));
    }
    let env_block = if env_lines.is_empty() {
        String::new()
    } else {
        format!("\n{}", env_lines.join("\n"))
    };

    let args_block = if args.is_empty() {
        "\nPERSISTENT_ARGS=()\n".to_string()
    } else {
        let quoted: Vec<String> = args.iter().map(|a| format!("    \"{}\"", a)).collect();
        format!("\nPERSISTENT_ARGS=(\n{}\n)\n", quoted.join("\n"))
    };

    let script = format!(
        r#"#!/usr/bin/env bash
# Smart Wrapper created by appmgr for {pkg_id}
# Handles hardcoded /opt paths transparently via Bubblewrap user namespaces
APP_ROOT="{app_dir}"
TARGET_BIN="$APP_ROOT/{rel_target}"{env_block}
{args_block}
# 1. Bubblewrap (bwrap) rootless virtualization for hardcoded absolute paths (/opt, /usr/share)
if command -v bwrap >/dev/null 2>&1 && [ "$APPMGR_NO_BWRAP" != "1" ]; then
    BWRAP_ARGS=(
        --dev-bind /dev /dev
        --ro-bind /sys /sys
        --proc /proc
        --ro-bind /usr /usr
        --ro-bind /etc /etc
        --ro-bind /bin /bin
        --ro-bind /lib /lib
        --ro-bind /lib64 /lib64
        --bind /run /run
        --bind /tmp /tmp
        --bind "$HOME" "$HOME"
        --tmpfs /opt
    )
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
        app_dir = app_dir.display(),
        rel_target = rel_target.display(),
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
