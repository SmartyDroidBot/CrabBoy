//! Download the open-source accuracy test suites into `roms/test-suites/`
//! (gitignored). Everything is pinned: the Game Boy bundle by release and
//! SHA-256, the GBA tests by commit.
//!
//! ```text
//! fetch_test_roms [--dest DIR] [--only gb|gba] [--force]
//! ```

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// c-sp/game-boy-test-roms: blargg, mooneye, mealybug, acid2, ... in one zip.
const GB_URL: &str =
    "https://github.com/c-sp/game-boy-test-roms/releases/download/v7.0/game-boy-test-roms-v7.0.zip";
const GB_SHA256: &str = "b9a9d7a1075aa35a3d07c07c34974048672d8520dca9e07a50178f5860c3832c";
const GB_MARKER: &str = "game-boy-test-roms-v7.0";

/// jsmolka/gba-tests prebuilt ROMs at a fixed commit.
const GBA_COMMIT: &str = "a7113b67e63f83a9b321696ddd7042ccfad6c881";
const GBA_FILES: &[&str] = &[
    "arm/arm.gba",
    "thumb/thumb.gba",
    "memory/memory.gba",
    "bios/bios.gba",
    "nes/nes.gba",
    "ppu/hello.gba",
    "ppu/shades.gba",
    "ppu/stripes.gba",
    "save/none.gba",
    "save/sram.gba",
    "save/flash64.gba",
    "save/flash128.gba",
    "unsafe/unsafe.gba",
];

/// A bare-metal 3DS payload from a public release, used to pin frames of the
/// 3DS core: an archive, its SHA-256, and the FIRM inside it.
struct CtrPayload {
    /// Directory under `3ds/`, and with the version the marker file's name.
    dir: &'static str,
    version: &'static str,
    url: &'static str,
    sha256: &'static str,
    member: &'static str,
}

const CTR_PAYLOADS: [CtrPayload; 3] = [
    // GPL-2.0-or-later.
    CtrPayload {
        dir: "godmode9",
        version: "v2.2.3",
        url: "https://github.com/d0k3/GodMode9/releases/download/v2.2.3/GodMode9-v2.2.3-20260331144941.zip",
        sha256: "3673b86240efa4b47769d2d22e1c9e234a906c40163ed54c821164143501beca",
        member: "GodMode9.firm",
    },
    // GPL-3.0-or-later.
    CtrPayload {
        dir: "fastboot3ds",
        version: "v1.2",
        url: "https://github.com/derrekr/fastboot3DS/releases/download/v1.2/fastboot3DSv1.2.7z",
        sha256: "f4e82308627950cdbd9b4fb7910209d0593a58c7d30ec419063d34b6b4dcc53e",
        member: "fastboot3DS.firm",
    },
    // GPL-3.0-or-later.
    CtrPayload {
        dir: "open_agb_firm",
        version: "beta-2024-12-24",
        url: "https://github.com/profi200/open_agb_firm/releases/download/beta_2024-12-24/open_agb_firm_beta_20241224.7z",
        sha256: "d8105ed0ac877bac99618b2664a656f19ab7ce67a55a696e5598ceeae6fa0cd9",
        member: "open_agb_firm.firm",
    },
];

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("fetch_test_roms: {msg}");
    std::process::exit(1)
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    ureq::get(url)
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?
        .body_mut()
        .as_reader()
        .read_to_end(&mut body)
        .map_err(|e| format!("reading {url}: {e}"))?;
    Ok(body)
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn fetch_gb(dest: &Path, force: bool) -> Result<usize, String> {
    let dir = dest.join("gb");
    let marker = dir.join(format!("{GB_MARKER}.ok"));
    if marker.exists() && !force {
        println!("gb: {GB_MARKER} already present in {}", dir.display());
        return Ok(0);
    }
    println!("gb: downloading {GB_URL}");
    let zip_bytes = download(GB_URL)?;
    let actual = sha256_hex(&zip_bytes);
    if actual != GB_SHA256 {
        return Err(format!(
            "SHA-256 mismatch for {GB_URL}\n  expected {GB_SHA256}\n  actual   {actual}"
        ));
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| format!("opening zip: {e}"))?;
    let mut count = 0;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("zip entry {i}: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let keep = matches!(
            rel.extension().and_then(|e| e.to_str()),
            Some("gb" | "gbc" | "png" | "md" | "txt" | "toml" | "json")
        );
        if !keep {
            continue;
        }
        let out = dir.join(rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let mut data = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut data)
            .map_err(|e| format!("extracting {}: {e}", out.display()))?;
        std::fs::write(&out, data).map_err(|e| format!("write {}: {e}", out.display()))?;
        count += 1;
    }
    std::fs::write(&marker, format!("{GB_URL}\n{GB_SHA256}\n"))
        .map_err(|e| format!("write {}: {e}", marker.display()))?;
    println!("gb: extracted {count} files to {}", dir.display());
    Ok(count)
}

fn fetch_gba(dest: &Path, force: bool) -> Result<usize, String> {
    let dir = dest.join("gba").join("jsmolka");
    let mut count = 0;
    for rel in GBA_FILES {
        let out = dir.join(rel);
        if out.exists() && !force {
            continue;
        }
        let url = format!("https://raw.githubusercontent.com/jsmolka/gba-tests/{GBA_COMMIT}/{rel}");
        println!("gba: downloading {rel}");
        let data = download(&url)?;
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&out, data).map_err(|e| format!("write {}: {e}", out.display()))?;
        count += 1;
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    std::fs::write(dir.join("COMMIT"), format!("{GBA_COMMIT}\n"))
        .map_err(|e| format!("write COMMIT: {e}"))?;
    println!(
        "gba: {count} new files, {} total in {}",
        GBA_FILES.len(),
        dir.display()
    );
    Ok(count)
}

/// One file out of a `.zip` or `.7z` archive held in memory.
fn extract(archive: Vec<u8>, url: &str, member: &str) -> Result<Vec<u8>, String> {
    let cursor = std::io::Cursor::new(archive);
    if url.ends_with(".7z") {
        let mut reader = sevenz_rust2::ArchiveReader::new(cursor, sevenz_rust2::Password::empty())
            .map_err(|e| format!("opening 7z: {e}"))?;
        return reader
            .read_file(member)
            .map_err(|e| format!("extracting {member}: {e}"));
    }
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| format!("opening zip: {e}"))?;
    let mut entry = zip.by_name(member).map_err(|e| format!("{member}: {e}"))?;
    let mut data = Vec::with_capacity(entry.size() as usize);
    entry
        .read_to_end(&mut data)
        .map_err(|e| format!("extracting {member}: {e}"))?;
    Ok(data)
}

fn fetch_ctr(dest: &Path, force: bool) -> Result<usize, String> {
    let mut count = 0;
    for payload in &CTR_PAYLOADS {
        let CtrPayload {
            dir: name,
            version,
            url,
            sha256,
            member,
        } = *payload;
        let dir = dest.join("3ds").join(name);
        let marker = dir.join(format!("{name}-{version}.ok"));
        if marker.exists() && !force {
            println!("3ds: {name} {version} already present in {}", dir.display());
            continue;
        }
        println!("3ds: downloading {url}");
        let archive = download(url)?;
        let actual = sha256_hex(&archive);
        if actual != sha256 {
            return Err(format!(
                "SHA-256 mismatch for {url}
  expected {sha256}
  actual   {actual}"
            ));
        }
        let data = extract(archive, url, member)?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        let out = dir.join(member);
        std::fs::write(&out, data).map_err(|e| format!("write {}: {e}", out.display()))?;
        std::fs::write(
            &marker,
            format!(
                "{url}
{sha256}
"
            ),
        )
        .map_err(|e| format!("write {}: {e}", marker.display()))?;
        println!("3ds: extracted {member} to {}", dir.display());
        count += 1;
    }
    Ok(count)
}

fn main() {
    let mut dest = PathBuf::from("roms/test-suites");
    let mut only: Option<String> = None;
    let mut force = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dest" => {
                dest = args
                    .next()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| fail("--dest needs a path"))
            }
            "--only" => {
                only = Some(
                    args.next()
                        .unwrap_or_else(|| fail("--only needs gb, gba or 3ds")),
                )
            }
            "--force" => force = true,
            "-h" | "--help" => {
                println!("usage: fetch_test_roms [--dest DIR] [--only gb|gba|3ds] [--force]");
                return;
            }
            other => fail(format!("unknown argument {other}")),
        }
    }
    std::fs::create_dir_all(&dest)
        .unwrap_or_else(|e| fail(format!("mkdir {}: {e}", dest.display())));
    if let Some(o) = &only {
        if !["gb", "gba", "3ds"].contains(&o.as_str()) {
            fail(format!("--only expects gb, gba or 3ds, got {o}"));
        }
    }
    let want = |k: &str| only.as_deref().is_none_or(|o| o == k);
    if want("gb") {
        fetch_gb(&dest, force).unwrap_or_else(|e| fail(e));
    }
    if want("gba") {
        fetch_gba(&dest, force).unwrap_or_else(|e| fail(e));
    }
    if want("3ds") {
        fetch_ctr(&dest, force).unwrap_or_else(|e| fail(e));
    }
}
