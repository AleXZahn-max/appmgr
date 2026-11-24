use flate2::write::GzEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use tempfile::TempDir;

struct IsolatedEnv {
    _root: TempDir,
    home: PathBuf,
    xdg_data: PathBuf,
    xdg_config: PathBuf,
    xdg_cache: PathBuf,
    fixtures: PathBuf,
}

impl IsolatedEnv {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let xdg_data = root.path().join("xdg_data");
        let xdg_config = root.path().join("xdg_config");
        let xdg_cache = root.path().join("xdg_cache");
        let fixtures = root.path().join("fixtures");
        for d in [&home, &xdg_data, &xdg_config, &xdg_cache, &fixtures] {
            fs::create_dir_all(d).expect("mkdir");
        }
        Self {
            _root: root,
            home,
            xdg_data,
            xdg_config,
            xdg_cache,
            fixtures,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_appmgr"))
            .arg("--user")
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", &self.xdg_data)
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("XDG_CACHE_HOME", &self.xdg_cache)
            .env("NO_COLOR", "1")
            .output()
            .expect("spawn appmgr")
    }

    fn run_ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            out.status.success(),
            "appmgr {:?} failed (code {:?})\nSTDOUT:\n{}\nSTDERR:\n{}",
            args,
            out.status.code(),
            stdout,
            stderr
        );
        stdout
    }

    fn bin_dir(&self) -> PathBuf {
        self.home.join(".local/bin")
    }

    fn sandboxes_dir(&self) -> PathBuf {
        self.xdg_data.join("appmgr/sandboxes")
    }
}

fn build_tar_gz_fixture(dest: &Path, files: &[(&str, &[u8], u32)]) {
    let file = File::create(dest).expect("create tar.gz");
    let enc = GzEncoder::new(file, Compression::default());
    let mut tb = tar::Builder::new(enc);
    for (rel_path, content, mode) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(*mode);
        header.set_cksum();
        tb.append_data(&mut header, rel_path, *content)
            .expect("append tar entry");
    }
    tb.finish().expect("finish tar.gz");
}

fn build_zip_fixture(dest: &Path, files: &[(&str, &[u8], u32)]) {
    let file = File::create(dest).expect("create zip");
    let mut zw = zip::ZipWriter::new(file);
    for (rel_path, content, mode) in files {
        let opts = zip::write::SimpleFileOptions::default().unix_permissions(*mode);
        zw.start_file(*rel_path, opts).expect("start zip entry");
        zw.write_all(content).expect("write zip entry");
    }
    zw.finish().expect("finish zip");
}

fn build_deb_fixture(
    dest: &Path,
    pkg: &str,
    version: &str,
    arch: &str,
    files: &[(&str, &[u8], u32)],
) -> Vec<u8> {
    let mut control_gz = Vec::new();
    {
        let enc = GzEncoder::new(&mut control_gz, Compression::default());
        let mut tb = tar::Builder::new(enc);
        let control_txt = format!(
            "Package: {}\nVersion: {}\nArchitecture: {}\nDescription: Fixture deb {}\n",
            pkg, version, arch, pkg
        );
        let mut header = tar::Header::new_gnu();
        header.set_size(control_txt.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tb.append_data(&mut header, "control", control_txt.as_bytes())
            .expect("append control");
        tb.finish().expect("finish control.tar.gz");
    }

    let mut data_gz = Vec::new();
    {
        let enc = GzEncoder::new(&mut data_gz, Compression::default());
        let mut tb = tar::Builder::new(enc);
        for (rel_path, content, mode) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            tb.append_data(&mut header, rel_path, *content)
                .expect("append data entry");
        }
        tb.finish().expect("finish data.tar.gz");
    }

    let mut deb_bytes = Vec::new();
    {
        let mut ab = ar::Builder::new(&mut deb_bytes);
        let debian_bin = b"2.0\n";
        let mut bh = ar::Header::new(b"debian-binary".to_vec(), debian_bin.len() as u64);
        bh.set_mode(0o644);
        ab.append(&bh, &debian_bin[..])
            .expect("append debian-binary");

        let mut ch = ar::Header::new(b"control.tar.gz".to_vec(), control_gz.len() as u64);
        ch.set_mode(0o644);
        ab.append(&ch, &control_gz[..]).expect("append control");

        let mut dh = ar::Header::new(b"data.tar.gz".to_vec(), data_gz.len() as u64);
        dh.set_mode(0o644);
        ab.append(&dh, &data_gz[..]).expect("append data");
    }

    fs::write(dest, &deb_bytes).expect("write deb");
    deb_bytes
}

fn build_synthetic_elf(dest: &Path) {
    let mut hdr = [0u8; 64];
    hdr[0..4].copy_from_slice(b"\x7fELF");
    hdr[4] = 2; // ELFCLASS64
    hdr[5] = 1; // ELFDATA2LSB
    hdr[6] = 1; // EV_CURRENT
    hdr[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    let machine: u16 = match std::env::consts::ARCH {
        "aarch64" => 183,
        _ => 62,
    };
    hdr[18..20].copy_from_slice(&machine.to_le_bytes());
    hdr[20..24].copy_from_slice(&1u32.to_le_bytes());
    hdr[24..32].copy_from_slice(&0x401000u64.to_le_bytes());
    hdr[52..54].copy_from_slice(&64u16.to_le_bytes());
    fs::write(dest, hdr).expect("write synthetic elf");
}

fn host_deb_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        _ => "amd64",
    }
}

fn start_mock_http_server(
    routes: Arc<RwLock<HashMap<String, Vec<u8>>>>,
) -> (u16, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock http");
    listener.set_nonblocking(true).expect("set_nonblocking");
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();

    let handle = std::thread::spawn(move || {
        while !stop_flag.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let first_line = req.lines().next().unwrap_or("");
                    let mut parts = first_line.split_whitespace();
                    let method = parts.next().unwrap_or("GET");
                    let path = parts.next().unwrap_or("/");

                    let guard = routes.read().unwrap();
                    if let Some(body) = guard.get(path) {
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        if method != "HEAD" {
                            let _ = stream.write_all(body);
                        }
                    } else {
                        let msg = b"Not Found";
                        let header = format!(
                            "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            msg.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        if method != "HEAD" {
                            let _ = stream.write_all(msg);
                        }
                    }
                    let _ = stream.flush();
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    (port, stop, handle)
}

#[test]
fn e2e_local_fixtures_install_info_env_args_sandbox_export_import_diff_remove() {
    let env = IsolatedEnv::new();

    // Create a canary file in fake real $HOME to verify isolated-home hides it
    let secret_canary = env.home.join("secret-canary.txt");
    fs::write(&secret_canary, "SENSITIVE_CANARY").unwrap();

    // 1. Local .tar.gz fixture
    let tar_fixture = env.fixtures.join("demoapp-1.0.0.tar.gz");
    let demo_script = b"#!/bin/sh\necho \"DEMO_ENV=${DEMO_ENV:-unset}\"\necho \"ARGS=$*\"\necho \"HOME=$HOME\"\nif [ -e \"$CANARY_FILE\" ]; then echo \"CANARY=EXPOSED\"; else echo \"CANARY=HIDDEN\"; fi\n";
    build_tar_gz_fixture(&tar_fixture, &[("bin/demoapp", demo_script, 0o755)]);

    // 2. Local .zip fixture
    let zip_fixture = env.fixtures.join("zipcli-1.2.0.zip");
    build_zip_fixture(
        &zip_fixture,
        &[("bin/zipcli", b"#!/bin/sh\necho zip_ok\n", 0o755)],
    );

    // 3. Minimal .deb fixture (ar + control.tar.gz + data.tar.gz)
    let deb_fixture = env.fixtures.join("debtool_1.0.0_all.deb");
    build_deb_fixture(
        &deb_fixture,
        "debtool",
        "1.0.0",
        "all",
        &[("usr/bin/debtool", b"#!/bin/sh\necho deb_ok\n", 0o755)],
    );

    // 4. Synthetic standalone ELF binary fixture
    let elf_fixture = env.fixtures.join("rawelf");
    build_synthetic_elf(&elf_fixture);

    // Install all four fixtures (demoapp with --isolate)
    env.run_ok(&[
        "install",
        tar_fixture.to_str().unwrap(),
        "-y",
        "--isolate",
        "--no-deps",
    ]);
    env.run_ok(&["install", zip_fixture.to_str().unwrap(), "-y", "--no-deps"]);
    env.run_ok(&["install", deb_fixture.to_str().unwrap(), "-y", "--no-deps"]);
    env.run_ok(&["install", elf_fixture.to_str().unwrap(), "-y", "--no-deps"]);

    // Verify info --json and sandbox state
    let info_json: serde_json::Value =
        serde_json::from_str(&env.run_ok(&["info", "demoapp", "--json"])).unwrap();
    assert_eq!(info_json["id"], "demoapp");
    assert_eq!(info_json["format"], "tar");
    assert_eq!(info_json["effective_sandbox"]["profile"], "isolated-home");
    assert!(env.sandboxes_dir().join("demoapp").is_dir());

    // Configure env & args and verify generated launcher
    env.run_ok(&["env", "demoapp", "DEMO_ENV=stage4_ok"]);
    env.run_ok(&["args", "demoapp", "--alpha", "--beta"]);

    let doctor_json: serde_json::Value =
        serde_json::from_str(&env.run_ok(&["doctor", "--json"])).unwrap();
    assert_eq!(doctor_json["installed_packages_count"], 4);
    assert!(doctor_json["bwrap_available"].is_boolean());
    assert!(doctor_json["bwrap_functional"].is_boolean());
    let bwrap_functional = doctor_json["bwrap_functional"].as_bool().unwrap_or(false);

    if bwrap_functional {
        let run_out = Command::new(env.bin_dir().join("demoapp"))
            .arg("--gamma")
            .env("HOME", &env.home)
            .env("CANARY_FILE", &secret_canary)
            .output()
            .expect("run sandboxed demoapp");
        let stdout = String::from_utf8_lossy(&run_out.stdout);
        assert!(
            run_out.status.success(),
            "demoapp launcher failed: {}",
            String::from_utf8_lossy(&run_out.stderr)
        );
        assert!(stdout.contains("DEMO_ENV=stage4_ok"));
        assert!(stdout.contains("ARGS=--alpha --beta --gamma"));
        assert!(stdout.contains(&format!("HOME={}", env.home.display())));
        assert!(stdout.contains("CANARY=HIDDEN"));
    }

    // Test sandbox profile switching, custom home-dir, and reset
    let custom_home = env._root.path().join("custom-demo-home");
    env.run_ok(&[
        "sandbox",
        "demoapp",
        "--profile",
        "offline",
        "--home-dir",
        custom_home.to_str().unwrap(),
    ]);
    assert!(custom_home.is_dir());
    let sbx_info: serde_json::Value =
        serde_json::from_str(&env.run_ok(&["info", "demoapp", "--json"])).unwrap();
    assert_eq!(sbx_info["effective_sandbox"]["profile"], "offline");
    assert_eq!(sbx_info["effective_sandbox"]["no_net"], true);

    env.run_ok(&["sandbox", "demoapp", "--reset"]);
    let reset_info: serde_json::Value =
        serde_json::from_str(&env.run_ok(&["info", "demoapp", "--json"])).unwrap();
    assert!(reset_info["sandbox"].is_null());

    // Set final sandbox profile for export/import --diff verification
    env.run_ok(&[
        "sandbox",
        "demoapp",
        "--profile",
        "isolated-home",
        "--no-net",
    ]);

    // Export both JSON and TOML manifests
    let manifest_json_path = env.fixtures.join("sync.json");
    let manifest_toml_path = env.fixtures.join("sync.toml");
    env.run_ok(&["export", manifest_json_path.to_str().unwrap()]);
    env.run_ok(&["export", manifest_toml_path.to_str().unwrap()]);
    assert!(manifest_json_path.is_file());
    assert!(manifest_toml_path.is_file());

    let manifest_val: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest_json_path).unwrap()).unwrap();
    assert_eq!(manifest_val["applications"].as_array().unwrap().len(), 4);
    assert!(manifest_val["sources"].is_array());

    // Mutate local state and verify import --diff (dry-run) vs import --diff --apply -y
    env.run_ok(&["pin", "demoapp"]);
    env.run_ok(&["env", "demoapp", "DEMO_ENV=mutated"]);
    env.run_ok(&["sandbox", "demoapp", "--allow-net"]);

    let diff_out = env.run_ok(&["import", manifest_json_path.to_str().unwrap(), "--diff"]);
    assert!(diff_out.contains("~ update demoapp"));
    assert!(diff_out.contains("env"));
    assert!(diff_out.contains("pin"));
    assert!(diff_out.contains("sandbox"));
    assert!(diff_out.contains("= keep zipcli"));

    // Dry-run --diff must NOT mutate state
    let still_mutated: serde_json::Value =
        serde_json::from_str(&env.run_ok(&["info", "demoapp", "--json"])).unwrap();
    assert_eq!(still_mutated["pinned"], true);
    assert_eq!(still_mutated["env"]["DEMO_ENV"], "mutated");

    // Apply via --diff --apply -y
    env.run_ok(&[
        "import",
        manifest_json_path.to_str().unwrap(),
        "--diff",
        "--apply",
        "-y",
    ]);
    let restored: serde_json::Value =
        serde_json::from_str(&env.run_ok(&["info", "demoapp", "--json"])).unwrap();
    assert_eq!(restored["pinned"], false);
    assert_eq!(restored["env"]["DEMO_ENV"], "stage4_ok");
    assert_eq!(restored["effective_sandbox"]["no_net"], true);

    // Verify clean removal of all packages and sandbox dir
    env.run_ok(&["remove", "demoapp", "zipcli", "debtool", "rawelf", "-y"]);
    let list_after: serde_json::Value =
        serde_json::from_str(&env.run_ok(&["list", "--json"])).unwrap();
    assert_eq!(list_after.as_array().unwrap().len(), 0);
    assert!(!env.sandboxes_dir().join("demoapp").exists());
}

#[test]
fn e2e_apt_http_install_upgrade_notes_rollback_and_fresh_import_apply() {
    let env = IsolatedEnv::new();
    let arch = host_deb_arch();

    let v1_deb_path = env.fixtures.join("aptdemo_1.0.0_all.deb");
    let v1_bytes = build_deb_fixture(
        &v1_deb_path,
        "aptdemo",
        "1.0.0",
        "all",
        &[("usr/bin/aptdemo", b"#!/bin/sh\necho version_1\n", 0o755)],
    );
    let v1_sha = hex::encode(Sha256::digest(&v1_bytes));

    let v2_deb_path = env.fixtures.join("aptdemo_2.0.0_all.deb");
    let v2_bytes = build_deb_fixture(
        &v2_deb_path,
        "aptdemo",
        "2.0.0",
        "all",
        &[("usr/bin/aptdemo", b"#!/bin/sh\necho version_2\n", 0o755)],
    );
    let v2_sha = hex::encode(Sha256::digest(&v2_bytes));

    let pkg_index_path = format!("/apt/dists/stable/main/binary-{}/Packages", arch);
    let v1_pool_path = "/apt/pool/main/aptdemo_1.0.0_all.deb";
    let v2_pool_path = "/apt/pool/main/aptdemo_2.0.0_all.deb";

    let v1_stanza = format!(
        "Package: aptdemo\nVersion: 1.0.0\nArchitecture: all\nFilename: pool/main/aptdemo_1.0.0_all.deb\nSize: {}\nSHA256: {}\nDescription: Demo APT app v1\n\n",
        v1_bytes.len(),
        v1_sha
    );
    let v2_stanza = format!(
        "Package: aptdemo\nVersion: 2.0.0\nArchitecture: all\nFilename: pool/main/aptdemo_2.0.0_all.deb\nSize: {}\nSHA256: {}\nHomepage: https://example.org/aptdemo\nDescription: Demo APT app v2 release notes summary\n\n",
        v2_bytes.len(),
        v2_sha
    );

    let routes = Arc::new(RwLock::new(HashMap::new()));
    {
        let mut r = routes.write().unwrap();
        r.insert(pkg_index_path.clone(), v1_stanza.into_bytes());
        r.insert(v1_pool_path.to_string(), v1_bytes);
        r.insert(v2_pool_path.to_string(), v2_bytes);
    }

    let (port, stop_flag, server_handle) = start_mock_http_server(routes.clone());
    let apt_base_url = format!("http://127.0.0.1:{}/apt", port);

    // Disable default official registry so tests stay 100% offline
    env.run_ok(&["source", "list"]);
    env.run_ok(&["source", "disable", "official"]);
    env.run_ok(&[
        "source",
        "add",
        "apt",
        "local-apt",
        &apt_base_url,
        "--suite",
        "stable",
        "--components",
        "main",
    ]);
    env.run_ok(&["update"]);

    // Install v1 from local APT server with --isolate, customize env/args
    env.run_ok(&[
        "install",
        "local-apt:aptdemo",
        "-y",
        "--isolate",
        "--no-deps",
    ]);
    env.run_ok(&["env", "aptdemo", "KEEP_ME=yes"]);
    env.run_ok(&["args", "aptdemo", "--persisted"]);

    let v1_info: serde_json::Value =
        serde_json::from_str(&env.run_ok(&["info", "aptdemo", "--json"])).unwrap();
    assert_eq!(v1_info["version"], "1.0.0");
    assert_eq!(v1_info["origin"]["kind"], "apt");

    // Publish v2 in mock APT repo and sync
    {
        let mut r = routes.write().unwrap();
        r.insert(pkg_index_path, v2_stanza.into_bytes());
    }
    env.run_ok(&["update"]);

    // Check upgrade --check --notes output
    let notes_check = env.run_ok(&["upgrade", "aptdemo", "--check", "--notes"]);
    assert!(notes_check.contains("1.0.0 -> 2.0.0"));
    assert!(notes_check.contains("Demo APT app v2 release notes summary"));

    // Upgrade to v2 and verify env/args/sandbox preserved
    env.run_ok(&["upgrade", "aptdemo", "-y"]);
    let v2_info: serde_json::Value =
        serde_json::from_str(&env.run_ok(&["info", "aptdemo", "--json"])).unwrap();
    assert_eq!(v2_info["version"], "2.0.0");
    assert_eq!(v2_info["env"]["KEEP_ME"], "yes");
    assert_eq!(v2_info["args"][0], "--persisted");
    assert_eq!(v2_info["effective_sandbox"]["profile"], "isolated-home");

    // Export manifest at v2 (with source + env + args + sandbox)
    let export_path = env.fixtures.join("apt-sync.json");
    env.run_ok(&["export", export_path.to_str().unwrap()]);

    // Rollback v2 -> v1
    env.run_ok(&["rollback", "aptdemo", "-y"]);
    let rolled_info: serde_json::Value =
        serde_json::from_str(&env.run_ok(&["info", "aptdemo", "--json"])).unwrap();
    assert_eq!(rolled_info["version"], "1.0.0");
    assert_eq!(rolled_info["env"]["KEEP_ME"], "yes");

    // Verify fresh machine import --apply -y restores source + package + env + args + sandbox
    let fresh_env = IsolatedEnv::new();
    fresh_env.run_ok(&["source", "list"]);
    fresh_env.run_ok(&["source", "disable", "official"]);
    let diff_fresh = fresh_env.run_ok(&["import", export_path.to_str().unwrap(), "--diff"]);
    assert!(diff_fresh.contains("+ source local-apt"));
    assert!(diff_fresh.contains("+ install aptdemo"));

    fresh_env.run_ok(&["import", export_path.to_str().unwrap(), "--apply", "-y"]);
    let imported_info: serde_json::Value =
        serde_json::from_str(&fresh_env.run_ok(&["info", "aptdemo", "--json"])).unwrap();
    assert_eq!(imported_info["version"], "2.0.0");
    assert_eq!(imported_info["env"]["KEEP_ME"], "yes");
    assert_eq!(imported_info["args"][0], "--persisted");
    assert_eq!(
        imported_info["effective_sandbox"]["profile"],
        "isolated-home"
    );

    // Remove
    env.run_ok(&["remove", "aptdemo", "-y"]);
    fresh_env.run_ok(&["remove", "aptdemo", "-y"]);

    stop_flag.store(true, Ordering::Relaxed);
    let _ = server_handle.join();
}
