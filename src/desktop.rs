use goblin::container::{Container, Ctx, Endian};
use goblin::elf::{dynamic, header, program_header, section_header, Elf};
use regex::Regex;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_ELF_TABLE_READ: usize = 4 * 1024 * 1024;
const MAX_ELF_STRTAB_READ: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ElfHeaderSummary {
    pub is_64: bool,
    pub little_endian: bool,
    pub e_type: u16,
    pub entry: u64,
    pub has_pt_interp: bool,
    pub has_pt_dynamic: bool,
    pub pt_dynamic_offset: u64,
    pub pt_dynamic_filesz: u64,
    pub pt_loads: Vec<(u64, u64, u64)>,
    pub e_shoff: u64,
    pub e_shentsize: u16,
    pub e_shnum: u16,
}

fn read_exact_at(file: &mut File, offset: u64, len: usize) -> Option<Vec<u8>> {
    if len == 0 || len > MAX_ELF_STRTAB_READ {
        return None;
    }
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn elf_ctx(header: &header::Header) -> Option<(Ctx, bool, bool)> {
    let is_64 = match header.e_ident[header::EI_CLASS] {
        header::ELFCLASS64 => true,
        header::ELFCLASS32 => false,
        _ => return None,
    };
    let little_endian = match header.e_ident[header::EI_DATA] {
        header::ELFDATA2LSB => true,
        header::ELFDATA2MSB => false,
        _ => return None,
    };
    let container = if is_64 {
        Container::Big
    } else {
        Container::Little
    };
    let endian = if little_endian {
        Endian::Little
    } else {
        Endian::Big
    };
    Some((Ctx::new(container, endian), is_64, little_endian))
}

/// Reads only the ELF header and program headers without loading the whole file into memory.
pub fn inspect_elf_headers(path: &Path) -> Option<ElfHeaderSummary> {
    let mut file = File::open(path).ok()?;
    let mut hdr_buf = [0u8; 64];
    let n = file.read(&mut hdr_buf).ok()?;
    if n < 52 || &hdr_buf[..4] != b"\x7fELF" {
        return None;
    }
    let hdr = Elf::parse_header(&hdr_buf[..n]).ok()?;
    let (ctx, is_64, little_endian) = elf_ctx(&hdr)?;

    let mut has_pt_interp = false;
    let mut has_pt_dynamic = false;
    let mut pt_dynamic_offset = 0u64;
    let mut pt_dynamic_filesz = 0u64;
    let mut pt_loads = Vec::new();

    if hdr.e_phnum > 0 && hdr.e_phentsize > 0 {
        let ph_size = (hdr.e_phnum as usize).saturating_mul(hdr.e_phentsize as usize);
        if ph_size <= MAX_ELF_TABLE_READ {
            if let Some(ph_bytes) = read_exact_at(&mut file, hdr.e_phoff, ph_size) {
                if let Ok(phs) =
                    program_header::ProgramHeader::parse(&ph_bytes, 0, hdr.e_phnum as usize, ctx)
                {
                    for ph in phs {
                        match ph.p_type {
                            program_header::PT_INTERP => has_pt_interp = true,
                            program_header::PT_DYNAMIC => {
                                has_pt_dynamic = true;
                                pt_dynamic_offset = ph.p_offset;
                                pt_dynamic_filesz = ph.p_filesz;
                            }
                            program_header::PT_LOAD => {
                                pt_loads.push((ph.p_vaddr, ph.p_filesz, ph.p_offset));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    Some(ElfHeaderSummary {
        is_64,
        little_endian,
        e_type: hdr.e_type,
        entry: hdr.e_entry,
        has_pt_interp,
        has_pt_dynamic,
        pt_dynamic_offset,
        pt_dynamic_filesz,
        pt_loads,
        e_shoff: hdr.e_shoff,
        e_shentsize: hdr.e_shentsize,
        e_shnum: hdr.e_shnum,
    })
}

fn read_u32(slice: &[u8], little_endian: bool) -> u32 {
    let arr: [u8; 4] = [slice[0], slice[1], slice[2], slice[3]];
    if little_endian {
        u32::from_le_bytes(arr)
    } else {
        u32::from_be_bytes(arr)
    }
}

fn read_u64(slice: &[u8], little_endian: bool) -> u64 {
    let arr: [u8; 8] = [
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ];
    if little_endian {
        u64::from_le_bytes(arr)
    } else {
        u64::from_be_bytes(arr)
    }
}

fn vaddr_to_file_offset(vaddr: u64, pt_loads: &[(u64, u64, u64)]) -> Option<u64> {
    for &(load_vaddr, load_filesz, load_offset) in pt_loads {
        if vaddr >= load_vaddr && vaddr < load_vaddr.saturating_add(load_filesz) {
            return Some(load_offset.saturating_add(vaddr - load_vaddr));
        }
    }
    None
}

/// Streaming reader for ELF `DT_NEEDED` shared library names without loading the entire file.
pub fn read_elf_needed_libs(binary_path: &Path) -> Option<Vec<String>> {
    let info = inspect_elf_headers(binary_path)?;
    let mut file = File::open(binary_path).ok()?;

    let mut dyn_bytes: Option<Vec<u8>> = None;
    let mut section_strtab: Option<(u64, u64)> = None;

    if info.has_pt_dynamic
        && info.pt_dynamic_filesz > 0
        && info.pt_dynamic_filesz <= MAX_ELF_TABLE_READ as u64
    {
        dyn_bytes = read_exact_at(
            &mut file,
            info.pt_dynamic_offset,
            info.pt_dynamic_filesz as usize,
        );
    }

    let load_shdrs = |f: &mut File| -> Option<Vec<section_header::SectionHeader>> {
        if info.e_shoff == 0 || info.e_shnum == 0 || info.e_shentsize == 0 {
            return None;
        }
        let sh_size = (info.e_shnum as usize).saturating_mul(info.e_shentsize as usize);
        if sh_size > MAX_ELF_TABLE_READ {
            return None;
        }
        let bytes = read_exact_at(f, info.e_shoff, sh_size)?;
        let container = if info.is_64 {
            Container::Big
        } else {
            Container::Little
        };
        let endian = if info.little_endian {
            Endian::Little
        } else {
            Endian::Big
        };
        section_header::SectionHeader::parse(
            &bytes,
            0,
            info.e_shnum as usize,
            Ctx::new(container, endian),
        )
        .ok()
    };

    if dyn_bytes.is_none() {
        if let Some(shdrs) = load_shdrs(&mut file) {
            if let Some(dyn_sh) = shdrs
                .iter()
                .find(|s| s.sh_type == section_header::SHT_DYNAMIC)
            {
                if dyn_sh.sh_size > 0 && dyn_sh.sh_size <= MAX_ELF_TABLE_READ as u64 {
                    dyn_bytes = read_exact_at(&mut file, dyn_sh.sh_offset, dyn_sh.sh_size as usize);
                }
                if let Some(str_sh) = shdrs.get(dyn_sh.sh_link as usize) {
                    section_strtab = Some((str_sh.sh_offset, str_sh.sh_size));
                }
            }
        }
    }

    let dyn_buf = dyn_bytes?;
    let entry_size = if info.is_64 { 16usize } else { 8usize };
    let mut needed_offsets = Vec::new();
    let mut strtab_vaddr: Option<u64> = None;
    let mut strtab_sz: Option<u64> = None;

    for chunk in dyn_buf.chunks_exact(entry_size) {
        let (tag, val) = if info.is_64 {
            (
                read_u64(&chunk[..8], info.little_endian),
                read_u64(&chunk[8..16], info.little_endian),
            )
        } else {
            (
                read_u32(&chunk[..4], info.little_endian) as u64,
                read_u32(&chunk[4..8], info.little_endian) as u64,
            )
        };

        if tag == dynamic::DT_NULL {
            break;
        } else if tag == dynamic::DT_NEEDED {
            needed_offsets.push(val as usize);
        } else if tag == dynamic::DT_STRTAB {
            strtab_vaddr = Some(val);
        } else if tag == dynamic::DT_STRSZ {
            strtab_sz = Some(val);
        }
    }

    if needed_offsets.is_empty() {
        return Some(Vec::new());
    }

    let (str_off, str_len) = match (strtab_vaddr, strtab_sz) {
        (Some(vaddr), Some(sz)) if sz > 0 => {
            if let Some(off) = vaddr_to_file_offset(vaddr, &info.pt_loads) {
                (off, sz)
            } else {
                section_strtab.or_else(|| {
                    load_shdrs(&mut file).and_then(|shdrs| {
                        shdrs
                            .iter()
                            .find(|s| s.sh_type == section_header::SHT_DYNAMIC)
                            .and_then(|d| shdrs.get(d.sh_link as usize))
                            .map(|s| (s.sh_offset, s.sh_size))
                    })
                })?
            }
        }
        _ => section_strtab.or_else(|| {
            load_shdrs(&mut file).and_then(|shdrs| {
                shdrs
                    .iter()
                    .find(|s| s.sh_type == section_header::SHT_DYNAMIC)
                    .and_then(|d| shdrs.get(d.sh_link as usize))
                    .map(|s| (s.sh_offset, s.sh_size))
            })
        })?,
    };

    if str_len == 0 || str_len > MAX_ELF_STRTAB_READ as u64 {
        return None;
    }

    let strtab = read_exact_at(&mut file, str_off, str_len as usize)?;
    let mut libs = Vec::with_capacity(needed_offsets.len());
    for off in needed_offsets {
        if off >= strtab.len() {
            continue;
        }
        let tail = &strtab[off..];
        let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
        if let Ok(name) = std::str::from_utf8(&tail[..end]) {
            if !name.is_empty() {
                libs.push(name.to_string());
            }
        }
    }
    Some(libs)
}

pub fn is_gui_binary(binary_path: &Path) -> bool {
    let Some(libs) = read_elf_needed_libs(binary_path) else {
        return false;
    };
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
    for lib in libs {
        let lower = lib.to_lowercase();
        if gui_libs.iter().any(|g| lower.contains(g)) {
            return true;
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

/// Patches an existing `.desktop` entry to point `Exec=` to `launcher_path` and optionally sets `Icon=`.
pub fn install_patched_desktop_file(
    orig_desktop: &Path,
    dest_desktop: &Path,
    launcher_path: &Path,
    icon_name: Option<&str>,
) -> std::io::Result<PathBuf> {
    let content = fs::read_to_string(orig_desktop)?;
    let re = Regex::new(r"(%[a-zA-Z])").unwrap();
    let mut new_lines = Vec::new();
    let mut replaced_icon = false;

    for line in content.lines() {
        if line.starts_with("Exec=") {
            let extra = if let Some(caps) = re.captures(line) {
                format!(" {}", caps.get(1).unwrap().as_str())
            } else {
                " %U".to_string()
            };
            new_lines.push(format!("Exec=\"{}\"{}", launcher_path.display(), extra));
        } else if line.starts_with("Icon=") {
            if let Some(icon) = icon_name {
                new_lines.push(format!("Icon={}", icon));
                replaced_icon = true;
            } else {
                new_lines.push(line.to_string());
            }
        } else {
            new_lines.push(line.to_string());
        }
    }

    if let Some(icon) = icon_name {
        if !replaced_icon {
            new_lines.push(format!("Icon={}", icon));
        }
    }

    if let Some(parent) = dest_desktop.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(dest_desktop, new_lines.join("\n") + "\n")?;
    Ok(dest_desktop.to_path_buf())
}

fn read_png_dimensions(path: &Path) -> Option<(u32, u32)> {
    let mut file = File::open(path).ok()?;
    let mut header = [0u8; 24];
    file.read_exact(&mut header).ok()?;
    if &header[..8] != b"\x89PNG\r\n\x1a\n" || &header[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
    let h = u32::from_be_bytes([header[20], header[21], header[22], header[23]]);
    Some((w, h))
}

fn parse_size_from_path(path: &Path) -> Option<(u32, u32)> {
    let re = Regex::new(r"(\d+)x(\d+)").ok()?;
    let s = path.to_string_lossy();
    let caps = re.captures(&s)?;
    let w = caps.get(1)?.as_str().parse::<u32>().ok()?;
    let h = caps.get(2)?.as_str().parse::<u32>().ok()?;
    Some((w, h))
}

/// Finds the best `.svg` (installed into `hicolor/scalable/apps`) or highest-resolution `.png` icon.
pub fn install_best_icon(
    search_roots: &[&Path],
    icons_dir: &Path,
    pkg_id: &str,
) -> Option<PathBuf> {
    let mut svg_candidates: Vec<PathBuf> = Vec::new();
    let mut png_candidates: Vec<(u64, u32, u32, PathBuf)> = Vec::new();

    for root in search_roots {
        if !root.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            if name.contains(".DirIcon") {
                continue;
            }
            let file_len = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            if file_len == 0 {
                continue;
            }
            let ext_lower = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_lowercase())
                .unwrap_or_default();

            if ext_lower == "svg" {
                svg_candidates.push(p.to_path_buf());
            } else if ext_lower == "png" {
                let (w, h) = read_png_dimensions(p)
                    .or_else(|| parse_size_from_path(p))
                    .unwrap_or((1, 1));
                let area = (w as u64).saturating_mul(h as u64);
                png_candidates.push((area, w, h, p.to_path_buf()));
            }
        }
    }

    if let Some(svg_path) = svg_candidates.first() {
        let target_dir = icons_dir.join("hicolor/scalable/apps");
        if fs::create_dir_all(&target_dir).is_ok() {
            let target = target_dir.join(format!("{}.svg", pkg_id));
            if fs::copy(svg_path, &target).is_ok() {
                return Some(target);
            }
        }
    }

    png_candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.3.cmp(&b.3)));
    if let Some((_, w, h, png_path)) = png_candidates.first() {
        let size_dir = if *w == *h && *w >= 16 {
            format!("{}x{}", w, h)
        } else {
            "256x256".to_string()
        };
        let target_dir = icons_dir.join(format!("hicolor/{}/apps", size_dir));
        if fs::create_dir_all(&target_dir).is_ok() {
            let target = target_dir.join(format!("{}.png", pkg_id));
            if fs::copy(png_path, &target).is_ok() {
                return Some(target);
            }
        }
    }

    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn make_minimal_png(path: &Path, width: u32, height: u32) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut data = Vec::new();
        data.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        data.extend_from_slice(&13u32.to_be_bytes());
        data.extend_from_slice(b"IHDR");
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&height.to_be_bytes());
        data.extend_from_slice(&[8, 6, 0, 0, 0]);
        fs::write(path, data).unwrap();
    }

    #[test]
    fn test_install_best_icon_svg_and_max_res_png() {
        let dir = tempdir().unwrap();
        let src_png_only = dir.path().join("src_png");
        make_minimal_png(&src_png_only.join("16x16/apps/demo.png"), 16, 16);
        make_minimal_png(&src_png_only.join("512x512/apps/demo.png"), 512, 512);
        make_minimal_png(&src_png_only.join("128x128/apps/demo.png"), 128, 128);

        let icons_dir = dir.path().join("icons");
        let installed_png =
            install_best_icon(&[src_png_only.as_path()], &icons_dir, "demo").unwrap();
        assert_eq!(
            installed_png,
            icons_dir.join("hicolor/512x512/apps/demo.png")
        );
        assert_eq!(read_png_dimensions(&installed_png), Some((512, 512)));

        let src_with_svg = dir.path().join("src_svg");
        make_minimal_png(&src_with_svg.join("256x256/apps/demo.png"), 256, 256);
        fs::create_dir_all(src_with_svg.join("scalable/apps")).unwrap();
        fs::write(src_with_svg.join("scalable/apps/demo.svg"), b"<svg/>").unwrap();

        let installed_svg =
            install_best_icon(&[src_with_svg.as_path()], &icons_dir, "demo").unwrap();
        assert_eq!(
            installed_svg,
            icons_dir.join("hicolor/scalable/apps/demo.svg")
        );
    }

    #[test]
    fn test_streaming_elf_reader_on_system_binary() {
        let cat_path = Path::new("/usr/bin/cat");
        if cat_path.exists() {
            let libs = read_elf_needed_libs(cat_path).expect("cat is ELF");
            assert!(libs.iter().any(|l| l.starts_with("libc.so")));
            assert!(!is_gui_binary(cat_path));
        }

        let dir = tempdir().unwrap();
        let non_elf = dir.path().join("script.sh");
        let mut f = File::create(&non_elf).unwrap();
        f.write_all(b"#!/bin/sh\necho hi\n").unwrap();
        assert!(read_elf_needed_libs(&non_elf).is_none());
        assert!(!is_gui_binary(&non_elf));
    }
}
