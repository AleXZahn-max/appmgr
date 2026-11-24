use crate::deps::analyzer::{
    inspect_elf_dynamic, is_core_host_only, parse_ldd_output, ElfClass, MissingLib,
};
use crate::ui::{apt_prefix, c, Colors, PacmanBar};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use reqwest::header::IF_NONE_MATCH;
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DEBIAN_MIRROR_ROOT: &str = "https://deb.debian.org/debian";
pub const DEFAULT_DEBIAN_SUITE: &str = "bookworm";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootlessDebInfo {
    pub package: String,
    pub filename: String,
    pub sha256: String,
}

fn path_matches_elf_class(filepath: &str, elf_class: ElfClass) -> bool {
    match elf_class {
        ElfClass::Elf64 => !filepath.contains("i386-linux-gnu") && !filepath.contains("/lib32/"),
        ElfClass::Elf32 => {
            !filepath.contains("x86_64-linux-gnu")
                && !filepath.contains("aarch64-linux-gnu")
                && !filepath.contains("/lib64/")
        }
    }
}

/// Scans a Debian Contents stream in a SINGLE pass for all requested SONAME targets.
pub fn search_contents_file_batch<R: Read>(
    reader: R,
    targets: &[MissingLib],
) -> HashMap<String, String> {
    let mut resolved = HashMap::new();
    if targets.is_empty() {
        return resolved;
    }

    let target_specs: Vec<(String, String, ElfClass)> = targets
        .iter()
        .map(|t| (t.soname.clone(), format!("/{}", t.soname), t.elf_class))
        .collect();
    let unique_count: HashSet<&str> = targets.iter().map(|t| t.soname.as_str()).collect();
    let total_needed = unique_count.len();

    let buf_reader = BufReader::new(reader);
    for line_res in buf_reader.lines() {
        let line = match line_res {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let Some(filepath) = parts.next() else {
            continue;
        };
        let Some(pkg_section) = parts.next() else {
            continue;
        };

        for (soname, suffix, elf_class) in &target_specs {
            if resolved.contains_key(soname) {
                continue;
            }
            if (filepath == soname || filepath.ends_with(suffix))
                && path_matches_elf_class(filepath, *elf_class)
            {
                let first_pkg = pkg_section.split(',').next().unwrap_or(pkg_section);
                let pkg_name = first_pkg.split('/').next_back().unwrap_or(first_pkg);
                resolved.insert(soname.clone(), pkg_name.trim().to_string());
                if resolved.len() == total_needed {
                    return resolved;
                }
            }
        }
    }

    resolved
}

/// Searches Debian Contents stream for a single SONAME and matching ELF architecture.
#[allow(dead_code)]
pub fn search_contents_file<R: Read>(
    reader: R,
    soname: &str,
    elf_class: ElfClass,
) -> Option<String> {
    let target = [MissingLib {
        soname: soname.to_string(),
        elf_class,
    }];
    search_contents_file_batch(reader, &target).remove(soname)
}

/// Streams Packages.gz content in a SINGLE pass looking for all `target_packages`.
pub fn find_debs_in_packages_stream<R: Read>(
    reader: R,
    target_packages: &HashSet<String>,
) -> HashMap<String, RootlessDebInfo> {
    let mut found = HashMap::new();
    if target_packages.is_empty() {
        return found;
    }

    let buf = BufReader::new(reader);
    let mut current_pkg = String::new();
    let mut current_filename = String::new();
    let mut current_sha256 = String::new();
    let mut matching = false;

    for line_res in buf.lines() {
        let line = match line_res {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();

        if trimmed.is_empty() {
            if matching && !current_filename.is_empty() {
                found.insert(
                    current_pkg.clone(),
                    RootlessDebInfo {
                        package: current_pkg.clone(),
                        filename: current_filename.clone(),
                        sha256: current_sha256.clone(),
                    },
                );
                if found.len() == target_packages.len() {
                    return found;
                }
            }
            current_pkg.clear();
            current_filename.clear();
            current_sha256.clear();
            matching = false;
            continue;
        }

        if let Some(pkg) = trimmed.strip_prefix("Package:") {
            let name = pkg.trim();
            if target_packages.contains(name) && !found.contains_key(name) {
                matching = true;
                current_pkg = name.to_string();
            } else {
                matching = false;
            }
        } else if matching {
            if let Some(f) = trimmed.strip_prefix("Filename:") {
                current_filename = f.trim().to_string();
            } else if let Some(s) = trimmed.strip_prefix("SHA256:") {
                current_sha256 = s.trim().to_string();
            }
        }
    }

    if matching && !current_filename.is_empty() {
        found.insert(
            current_pkg.clone(),
            RootlessDebInfo {
                package: current_pkg,
                filename: current_filename,
                sha256: current_sha256,
            },
        );
    }

    found
}

/// Streams Packages.gz content looking for a single package.
#[allow(dead_code)]
pub fn find_deb_in_packages_stream<R: Read>(
    reader: R,
    target_package: &str,
) -> Option<RootlessDebInfo> {
    let mut set = HashSet::new();
    set.insert(target_package.to_string());
    find_debs_in_packages_stream(reader, &set).remove(target_package)
}

/// Unpacks only .so files and symlinks from a tar stream into target_deps_dir.
pub fn extract_so_from_tar<R: Read>(
    reader: R,
    target_deps_dir: &Path,
    extracted_sonames: &mut Vec<String>,
) -> Result<(), String> {
    let mut tar = tar::Archive::new(reader);
    for entry_res in tar.entries().map_err(|e| format!("Tar error: {}", e))? {
        let mut entry = entry_res.map_err(|e| format!("Tar entry error: {}", e))?;
        let path = entry
            .path()
            .map_err(|e| format!("Tar path error: {}", e))?
            .to_path_buf();
        let filename = match path.file_name() {
            Some(n) => n.to_string_lossy().to_string(),
            None => continue,
        };

        if filename.ends_with(".so") || filename.contains(".so.") {
            let dest_file = target_deps_dir.join(&filename);
            let entry_type = entry.header().entry_type();

            if entry_type.is_symlink() {
                if let Ok(Some(link_target)) = entry.link_name() {
                    let _ = fs::remove_file(&dest_file);
                    let target_fn = link_target.file_name().unwrap_or(link_target.as_os_str());
                    #[cfg(unix)]
                    let _ = std::os::unix::fs::symlink(target_fn, &dest_file);
                    extracted_sonames.push(filename);
                }
            } else if entry_type.is_file() {
                let _ = fs::remove_file(&dest_file);
                let mut out_file = File::create(&dest_file)
                    .map_err(|e| format!("Failed to create {}: {}", dest_file.display(), e))?;
                io::copy(&mut entry, &mut out_file)
                    .map_err(|e| format!("Failed to write {}: {}", dest_file.display(), e))?;
                let _ = fs::set_permissions(&dest_file, fs::Permissions::from_mode(0o755));
                extracted_sonames.push(filename);
            }
        }
    }
    Ok(())
}

/// Extracts only *.so* files and symlinks from a downloaded deb package.
pub fn extract_so_files_from_deb(
    deb_path: &Path,
    target_deps_dir: &Path,
) -> Result<Vec<String>, String> {
    fs::create_dir_all(target_deps_dir).map_err(|e| format!("Failed to create deps dir: {}", e))?;

    let file = File::open(deb_path).map_err(|e| format!("Failed to open deb: {}", e))?;
    let mut ar = ar::Archive::new(file);

    let mut data_tar_name = String::new();
    let mut data_tar_data = Vec::new();

    while let Some(entry_res) = ar.next_entry() {
        let mut entry = entry_res.map_err(|e| format!("Ar entry error: {}", e))?;
        let name = String::from_utf8_lossy(entry.header().identifier()).to_string();
        if name.starts_with("data.tar") {
            data_tar_name = name;
            entry
                .read_to_end(&mut data_tar_data)
                .map_err(|e| format!("Failed to read data.tar: {}", e))?;
            break;
        }
    }

    if data_tar_data.is_empty() {
        return Err("data.tar not found in deb".to_string());
    }

    let cursor = std::io::Cursor::new(data_tar_data);
    let mut extracted_sonames = Vec::new();

    if data_tar_name.ends_with(".xz") {
        let dec = xz2::read::XzDecoder::new(cursor);
        extract_so_from_tar(dec, target_deps_dir, &mut extracted_sonames)?;
    } else if data_tar_name.ends_with(".gz") {
        let dec = flate2::read::GzDecoder::new(cursor);
        extract_so_from_tar(dec, target_deps_dir, &mut extracted_sonames)?;
    } else if data_tar_name.ends_with(".zst") {
        let dec = zstd::Decoder::new(cursor).map_err(|e| format!("zstd init error: {}", e))?;
        extract_so_from_tar(dec, target_deps_dir, &mut extracted_sonames)?;
    } else {
        extract_so_from_tar(cursor, target_deps_dir, &mut extracted_sonames)?;
    }

    Ok(extracted_sonames)
}

/// Downloads a remote file with ETag caching and Pacman progress bar.
pub async fn download_file_cached(
    client: &reqwest::Client,
    url: &str,
    dest_path: &Path,
    label: &str,
) -> Result<(), String> {
    if let Some(parent) = dest_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let etag_file = dest_path.with_extension("etag");
    let mut req = client.get(url);

    if dest_path.exists() && etag_file.exists() {
        if let Ok(etag) = fs::read_to_string(&etag_file) {
            let trimmed = etag.trim();
            if !trimmed.is_empty() {
                req = req.header(IF_NONE_MATCH, trimmed);
            }
        }
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("Failed to send request to {}: {}", url, e))?;

    if resp.status() == StatusCode::NOT_MODIFIED && dest_path.exists() {
        return Ok(());
    }

    if !resp.status().is_success() {
        return Err(format!(
            "Download failed with HTTP {} for {}",
            resp.status(),
            url
        ));
    }

    let new_etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let total_bytes = resp.content_length().unwrap_or(0);
    let mut bar = PacmanBar::new(total_bytes, label);

    let temp_dest = dest_path.with_extension("part");
    let mut file =
        File::create(&temp_dest).map_err(|e| format!("Failed to create temp file: {}", e))?;

    let mut downloaded: u64 = 0;
    let mut stream = resp.bytes_stream();

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res.map_err(|e| format!("Download stream error: {}", e))?;
        file.write_all(&chunk)
            .map_err(|e| format!("File write error: {}", e))?;
        downloaded += chunk.len() as u64;
        bar.update(downloaded);
    }
    bar.finish(&format!("Downloaded {}", label));

    if fs::rename(&temp_dest, dest_path).is_err() {
        fs::copy(&temp_dest, dest_path).map_err(|e| e.to_string())?;
        let _ = fs::remove_file(&temp_dest);
    }

    if let Some(et) = new_etag {
        let _ = fs::write(etag_file, et);
    }

    Ok(())
}

/// Ensures Debian library index files (Contents and Packages) are cached locally.
pub async fn ensure_debian_index_cached(
    client: &reqwest::Client,
    cache_dir: &Path,
    arch_str: &str,
    debian_suite: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let suite = if debian_suite.trim().is_empty() {
        DEFAULT_DEBIAN_SUITE
    } else {
        debian_suite.trim()
    };
    let deps_index_dir = cache_dir.join("deps-index").join(suite);
    fs::create_dir_all(&deps_index_dir)
        .map_err(|e| format!("Failed to create cache dir: {}", e))?;

    let contents_filename = format!("Contents-{}.gz", arch_str);
    let packages_filename = format!("Packages-{}.gz", arch_str);

    let contents_path = deps_index_dir.join(&contents_filename);
    let packages_path = deps_index_dir.join(&packages_filename);

    let contents_url = format!(
        "{}/dists/{}/main/{}",
        DEBIAN_MIRROR_ROOT, suite, contents_filename
    );
    let packages_url = format!(
        "{}/dists/{}/main/binary-{}/Packages.gz",
        DEBIAN_MIRROR_ROOT, suite, arch_str
    );

    if !contents_path.exists() {
        println!(
            "{} Initializing Debian ({}) library index cache for {} ...",
            apt_prefix("INFO"),
            suite,
            arch_str
        );
    }

    download_file_cached(
        client,
        &contents_url,
        &contents_path,
        &format!("Debian Contents ({})", arch_str),
    )
    .await?;

    download_file_cached(
        client,
        &packages_url,
        &packages_path,
        &format!("Debian Packages ({})", arch_str),
    )
    .await?;

    Ok((contents_path, packages_path))
}

fn arch_str_for_class(elf_class: ElfClass) -> &'static str {
    match elf_class {
        ElfClass::Elf64 => {
            if std::env::consts::ARCH == "aarch64" {
                "arm64"
            } else {
                "amd64"
            }
        }
        ElfClass::Elf32 => "i386",
    }
}

/// Resolves missing shared libraries rootlessly by downloading required .deb packages
/// using single-pass index scans per architecture and extracting *.so* into app_dir/deps/lib/.
pub async fn resolve_rootless_libraries(
    client: &reqwest::Client,
    cache_dir: &Path,
    app_dir: &Path,
    initial_missing: &[MissingLib],
    debian_suite: &str,
) -> Result<Vec<String>, String> {
    let deps_lib_dir = app_dir.join("deps").join("lib");
    fs::create_dir_all(&deps_lib_dir).map_err(|e| format!("Failed to create deps/lib: {}", e))?;

    let mut visited_sonames = HashSet::new();
    let mut downloaded_packages = HashSet::new();
    let mut cached_indexes: HashMap<String, (PathBuf, PathBuf)> = HashMap::new();
    let mut bundled_so_files = Vec::new();
    let mut queue: Vec<MissingLib> = initial_missing.to_vec();
    let mut depth = 0;

    while !queue.is_empty() && depth < 3 {
        let mut grouped_by_arch: BTreeMap<String, Vec<MissingLib>> = BTreeMap::new();
        for missing in queue {
            if is_core_host_only(&missing.soname) || !visited_sonames.insert(missing.soname.clone())
            {
                continue;
            }
            let arch = arch_str_for_class(missing.elf_class).to_string();
            grouped_by_arch.entry(arch).or_default().push(missing);
        }

        let mut newly_extracted_files = Vec::new();

        for (arch_str, batch) in grouped_by_arch {
            if !cached_indexes.contains_key(&arch_str) {
                let paths =
                    ensure_debian_index_cached(client, cache_dir, &arch_str, debian_suite).await?;
                cached_indexes.insert(arch_str.clone(), paths);
            }
            let (contents_path, packages_path) = &cached_indexes[&arch_str];

            println!(
                "{} Scanning Debian ({}) pool in single pass for {} librar{} ...",
                apt_prefix("INFO"),
                arch_str,
                batch.len(),
                if batch.len() == 1 { "y" } else { "ies" }
            );

            // 1. Single-pass scan of Contents-<arch>.gz for all SONAMEs in batch
            let contents_file = File::open(contents_path)
                .map_err(|e| format!("Failed to open Contents index: {}", e))?;
            let gz_contents = GzDecoder::new(contents_file);
            let soname_to_pkg = search_contents_file_batch(gz_contents, &batch);

            let mut target_packages = HashSet::new();
            for missing in &batch {
                if let Some(pkg) = soname_to_pkg.get(&missing.soname) {
                    if !downloaded_packages.contains(pkg) {
                        target_packages.insert(pkg.clone());
                    }
                } else {
                    println!(
                        "{} Library '{}' not found in Debian {} Contents index.",
                        apt_prefix("WARN"),
                        missing.soname,
                        arch_str
                    );
                }
            }

            if target_packages.is_empty() {
                continue;
            }

            // 2. Single-pass scan of Packages-<arch>.gz for all needed packages
            let packages_file = File::open(packages_path)
                .map_err(|e| format!("Failed to open Packages index: {}", e))?;
            let gz_packages = GzDecoder::new(packages_file);
            let pkg_to_deb = find_debs_in_packages_stream(gz_packages, &target_packages);

            let mut ordered_pkgs: Vec<String> = target_packages.into_iter().collect();
            ordered_pkgs.sort();

            for pkg_name in ordered_pkgs {
                let Some(deb_info) = pkg_to_deb.get(&pkg_name) else {
                    println!(
                        "{} Package metadata for '{}' not found in Packages index.",
                        apt_prefix("WARN"),
                        pkg_name
                    );
                    continue;
                };
                downloaded_packages.insert(pkg_name.clone());

                let deb_url = format!("{}/{}", DEBIAN_MIRROR_ROOT, deb_info.filename);
                let deb_filename = deb_info
                    .filename
                    .split('/')
                    .next_back()
                    .unwrap_or("lib.deb");
                let temp_deb_path = cache_dir.join("downloads").join(deb_filename);

                println!(
                    "{} Downloading dependency package '{}' ({}) ...",
                    apt_prefix("GET"),
                    c(&pkg_name, &[Colors::BOLD, Colors::BRIGHT_GREEN]),
                    deb_filename
                );

                if let Some(p) = temp_deb_path.parent() {
                    let _ = fs::create_dir_all(p);
                }

                let resp = client
                    .get(&deb_url)
                    .send()
                    .await
                    .map_err(|e| format!("Failed to download {}: {}", deb_url, e))?;

                if !resp.status().is_success() {
                    return Err(format!(
                        "Download failed with HTTP {} for {}",
                        resp.status(),
                        deb_url
                    ));
                }

                let total_bytes = resp.content_length().unwrap_or(0);
                let mut bar = PacmanBar::new(total_bytes, deb_filename);
                let mut file = File::create(&temp_deb_path)
                    .map_err(|e| format!("Failed to create {}: {}", temp_deb_path.display(), e))?;

                let mut downloaded: u64 = 0;
                let mut hasher = Sha256::new();
                let mut stream = resp.bytes_stream();

                while let Some(chunk_res) = stream.next().await {
                    let chunk = chunk_res.map_err(|e| format!("Stream error: {}", e))?;
                    file.write_all(&chunk)
                        .map_err(|e| format!("Write error: {}", e))?;
                    hasher.update(&chunk);
                    downloaded += chunk.len() as u64;
                    bar.update(downloaded);
                }
                bar.finish(&format!("Downloaded {}", deb_filename));

                let calculated_hash = hex::encode(hasher.finalize());
                if !deb_info.sha256.is_empty() && calculated_hash != deb_info.sha256.to_lowercase()
                {
                    let _ = fs::remove_file(&temp_deb_path);
                    return Err(format!(
                        "SHA-256 verification failed for {}.\nExpected: {}\nCalculated: {}",
                        deb_filename, deb_info.sha256, calculated_hash
                    ));
                }

                println!(
                    "{} SHA-256 verified ({})",
                    apt_prefix("OK"),
                    c(&calculated_hash[..8], &[Colors::DIM])
                );

                let extracted = extract_so_files_from_deb(&temp_deb_path, &deps_lib_dir)?;
                let _ = fs::remove_file(&temp_deb_path);

                for so in &extracted {
                    if !bundled_so_files.contains(so) {
                        bundled_so_files.push(so.clone());
                    }
                    newly_extracted_files.push(so.clone());
                }

                println!(
                    "{} Extracted {} library files into {}",
                    apt_prefix("OK"),
                    extracted.len(),
                    c(&deps_lib_dir.display().to_string(), &[Colors::DIM])
                );
            }
        }

        // Inspect newly extracted libraries in batches per ElfClass for secondary dependencies
        let mut next_queue = Vec::new();
        if !newly_extracted_files.is_empty() {
            let mut internal_sonames = HashSet::new();
            if let Ok(entries) = fs::read_dir(&deps_lib_dir) {
                for entry in entries.flatten() {
                    internal_sonames.insert(entry.file_name().to_string_lossy().to_string());
                }
            }

            let mut by_class: BTreeMap<ElfClass, Vec<PathBuf>> = BTreeMap::new();
            for so_name in &newly_extracted_files {
                let so_path = deps_lib_dir.join(so_name);
                if so_path.is_file() && !so_path.is_symlink() {
                    if let Some((elf_class, true)) = inspect_elf_dynamic(&so_path) {
                        by_class.entry(elf_class).or_default().push(so_path);
                    }
                }
            }

            for (elf_class, paths) in by_class {
                for chunk in paths.chunks(64) {
                    let mut cmd = Command::new("ldd");
                    cmd.args(chunk);
                    cmd.env("LD_LIBRARY_PATH", &deps_lib_dir);
                    if let Ok(output) = cmd.output() {
                        let combined = format!(
                            "{}\n{}",
                            String::from_utf8_lossy(&output.stdout),
                            String::from_utf8_lossy(&output.stderr)
                        );
                        let sub_missing = parse_ldd_output(&combined, elf_class, &internal_sonames);
                        for m in sub_missing {
                            if !visited_sonames.contains(&m.soname) && !is_core_host_only(&m.soname)
                            {
                                next_queue.push(m);
                            }
                        }
                    }
                }
            }
        }

        queue = next_queue;
        depth += 1;
    }

    Ok(bundled_so_files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;

    struct SinglePassReader<R> {
        inner: R,
        eof_reached: bool,
    }

    impl<R: Read> Read for SinglePassReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.inner.read(buf)?;
            if n == 0 {
                self.eof_reached = true;
            }
            Ok(n)
        }
    }

    #[test]
    fn test_search_contents_file_parsing() {
        let mock_contents = r#"
usr/lib/x86_64-linux-gnu/libayatana-indicator3.so.7.0.0 libs/libayatana-indicator3-7
usr/lib/i386-linux-gnu/libayatana-indicator3.so.7.0.0 libs/lib32-libayatana-indicator3-7
usr/lib/x86_64-linux-gnu/libayatana-ido3-0.4.so.0      libs/libayatana-ido3-0.4-0
"#;

        let res_64 = search_contents_file(
            mock_contents.as_bytes(),
            "libayatana-indicator3.so.7.0.0",
            ElfClass::Elf64,
        );
        assert_eq!(res_64.as_deref(), Some("libayatana-indicator3-7"));

        let res_32 = search_contents_file(
            mock_contents.as_bytes(),
            "libayatana-indicator3.so.7.0.0",
            ElfClass::Elf32,
        );
        assert_eq!(res_32.as_deref(), Some("lib32-libayatana-indicator3-7"));
    }

    #[test]
    fn test_single_pass_soname_and_packages_batch_fixture() {
        let contents_fixture = "\
usr/lib/i386-linux-gnu/libssl.so.3 libs/lib32-libssl3
usr/lib/x86_64-linux-gnu/libssl.so.3 libs/libssl3
usr/lib/x86_64-linux-gnu/libayatana-indicator3.so.7 libs/libayatana-indicator3-7
usr/lib/x86_64-linux-gnu/libcrypto.so.3 libs/libssl3
usr/lib/x86_64-linux-gnu/libpng16.so.16 libs/libpng16-16
";
        let mut gz_encoder = GzEncoder::new(Vec::new(), Compression::default());
        gz_encoder.write_all(contents_fixture.as_bytes()).unwrap();
        let contents_gz = gz_encoder.finish().unwrap();

        let targets = vec![
            MissingLib {
                soname: "libssl.so.3".to_string(),
                elf_class: ElfClass::Elf64,
            },
            MissingLib {
                soname: "libayatana-indicator3.so.7".to_string(),
                elf_class: ElfClass::Elf64,
            },
            MissingLib {
                soname: "libpng16.so.16".to_string(),
                elf_class: ElfClass::Elf64,
            },
        ];

        let single_reader = SinglePassReader {
            inner: GzDecoder::new(&contents_gz[..]),
            eof_reached: false,
        };
        let resolved = search_contents_file_batch(single_reader, &targets);
        assert_eq!(resolved.len(), 3);
        assert_eq!(
            resolved.get("libssl.so.3").map(String::as_str),
            Some("libssl3")
        );
        assert_eq!(
            resolved
                .get("libayatana-indicator3.so.7")
                .map(String::as_str),
            Some("libayatana-indicator3-7")
        );
        assert_eq!(
            resolved.get("libpng16.so.16").map(String::as_str),
            Some("libpng16-16")
        );

        let packages_fixture = "\
Package: libssl3
Filename: pool/main/o/openssl/libssl3_3.0.11_amd64.deb
SHA256: 1111111111111111

Package: unrelated
Filename: pool/main/u/unrelated/unrelated_1.0_amd64.deb
SHA256: 0000000000000000

Package: libpng16-16
Filename: pool/main/libp/libpng1.6/libpng16-16_1.6.39_amd64.deb
SHA256: 2222222222222222
";
        let mut pkg_encoder = GzEncoder::new(Vec::new(), Compression::default());
        pkg_encoder.write_all(packages_fixture.as_bytes()).unwrap();
        let pkg_gz = pkg_encoder.finish().unwrap();

        let needed_pkgs: HashSet<String> = ["libssl3".to_string(), "libpng16-16".to_string()]
            .into_iter()
            .collect();
        let deb_map = find_debs_in_packages_stream(GzDecoder::new(&pkg_gz[..]), &needed_pkgs);
        assert_eq!(deb_map.len(), 2);
        assert_eq!(
            deb_map["libssl3"].filename,
            "pool/main/o/openssl/libssl3_3.0.11_amd64.deb"
        );
        assert_eq!(
            deb_map["libpng16-16"].filename,
            "pool/main/libp/libpng1.6/libpng16-16_1.6.39_amd64.deb"
        );
    }

    #[test]
    fn test_find_deb_in_packages_stream() {
        let mock_packages = r#"
Package: 0ad
Filename: pool/main/0/0ad/0ad_0.0.26-3_amd64.deb
SHA256: 3a2118df47bf3f04285649f0455c2fc6fe2dc7f0b237073038aa00af41f0d5f2

Package: libayatana-indicator3-7
Filename: pool/main/liba/libayatana-indicator/libayatana-indicator3-7_0.9.3-1_amd64.deb
SHA256: a1b2c3d4e5f60718293041526374859607182930415263748596071829304152

Package: other
Filename: pool/main/o/other/other_1.0_amd64.deb
SHA256: 1234567890abcdef
"#;

        let info = find_deb_in_packages_stream(mock_packages.as_bytes(), "libayatana-indicator3-7")
            .expect("Package should be found");
        assert_eq!(info.package, "libayatana-indicator3-7");
        assert_eq!(
            info.filename,
            "pool/main/liba/libayatana-indicator/libayatana-indicator3-7_0.9.3-1_amd64.deb"
        );
        assert_eq!(
            info.sha256,
            "a1b2c3d4e5f60718293041526374859607182930415263748596071829304152"
        );
    }
}
