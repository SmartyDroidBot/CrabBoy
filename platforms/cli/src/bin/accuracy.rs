//! Run the open-source accuracy test suites and compare with the recorded
//! baseline.
//!
//! ```text
//! accuracy [--suites tests/accuracy/suites.toml] [--roms roms/test-suites/gb]
//!          [--baseline tests/accuracy/baseline.txt] [--filter TEXT]
//!          [--jobs N] [--ci] [--update-baseline] [--markdown FILE] [--verbose]
//!          [--dump-failures DIR]
//! ```
//!
//! Every ROM runs in-process (a panic counts as a crash) with a frame budget.
//! `--ci` exits non-zero when any result differs from the baseline, in either
//! direction, so improvements are recorded deliberately with
//! `--update-baseline`.

use emu_core::System;
use gb_core::{Cartridge, Gb, Model};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    Pass,
    Fail,
    Timeout,
    Crash,
    Skip,
}

impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Outcome::Pass => "PASS",
            Outcome::Fail => "FAIL",
            Outcome::Timeout => "TIMEOUT",
            Outcome::Crash => "CRASH",
            Outcome::Skip => "SKIP",
        }
    }
    fn parse(s: &str) -> Option<Outcome> {
        Some(match s {
            "PASS" => Outcome::Pass,
            "FAIL" => Outcome::Fail,
            "TIMEOUT" => Outcome::Timeout,
            "CRASH" => Outcome::Crash,
            "SKIP" => Outcome::Skip,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
struct Suite {
    name: String,
    kind: String,
    model: String,
    frames: u64,
    roms: Vec<String>,
    glob: Option<String>,
    reference: Option<String>,
    reference_alt: Option<String>,
    /// `ctr-frame`: the expected top-screen hash, an input script, and
    /// whether a FAT16 SD card is inserted.
    hash: Option<String>,
    input: Option<String>,
    sd_fat: bool,
}

#[derive(Clone, Debug)]
struct Case {
    suite: String,
    kind: String,
    model: String,
    frames: u64,
    /// Path relative to the suites root, as recorded in the baseline.
    rel: String,
    path: PathBuf,
    reference: Option<PathBuf>,
    /// Directory that receives the frame of a failing screenshot test.
    dump_dir: Option<PathBuf>,
    hash: Option<String>,
    input: Option<String>,
    sd_fat: bool,
}

#[derive(Clone, Debug)]
struct Result_ {
    outcome: Outcome,
    detail: String,
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("accuracy: {msg}");
    std::process::exit(2)
}

fn load_suites(path: &Path) -> Vec<Suite> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| fail(format!("read {}: {e}", path.display())));
    let doc: toml::Value =
        toml::from_str(&text).unwrap_or_else(|e| fail(format!("{}: {e}", path.display())));
    let list = doc
        .get("suite")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| fail("no [[suite]] entries"));
    list.iter()
        .map(|s| {
            let str_field = |k: &str| s.get(k).and_then(|v| v.as_str()).map(str::to_string);
            Suite {
                name: str_field("name").unwrap_or_else(|| fail("suite without name")),
                kind: str_field("kind").unwrap_or_else(|| fail("suite without kind")),
                model: str_field("model").unwrap_or_else(|| "auto".to_string()),
                frames: s.get("frames").and_then(|v| v.as_integer()).unwrap_or(600) as u64,
                roms: s
                    .get("roms")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
                glob: str_field("glob"),
                reference: str_field("reference"),
                reference_alt: str_field("reference_alt"),
                hash: str_field("hash"),
                input: str_field("input"),
                sd_fat: s.get("sd_fat").and_then(|v| v.as_bool()).unwrap_or(false),
            }
        })
        .collect()
}

/// Expand a glob with `*`, `**` and `{a,b}` alternatives against the suites
/// root. Returned paths are sorted.
fn expand_glob(root: &Path, pattern: &str) -> Vec<PathBuf> {
    fn alternatives(pattern: &str) -> Vec<String> {
        if let (Some(open), Some(close)) = (pattern.find('{'), pattern.find('}')) {
            let mut out = Vec::new();
            for alt in pattern[open + 1..close].split(',') {
                let expanded = format!("{}{}{}", &pattern[..open], alt, &pattern[close + 1..]);
                out.extend(alternatives(&expanded));
            }
            out
        } else {
            vec![pattern.to_string()]
        }
    }
    fn matches(name: &str, pat: &str) -> bool {
        // Single-segment wildcard match with `*`.
        let parts: Vec<&str> = pat.split('*').collect();
        if parts.len() == 1 {
            return name == pat;
        }
        let mut pos = 0;
        for (i, part) in parts.iter().enumerate() {
            if i == 0 {
                if !name.starts_with(part) {
                    return false;
                }
                pos = part.len();
            } else if i == parts.len() - 1 {
                return name.len() >= pos && name[pos..].ends_with(part);
            } else if let Some(found) = name[pos..].find(part) {
                pos += found + part.len();
            } else {
                return false;
            }
        }
        true
    }
    fn walk(dir: &Path, segs: &[&str], out: &mut Vec<PathBuf>) {
        let Some((seg, rest)) = segs.split_first() else {
            return;
        };
        if *seg == "**" {
            walk(dir, rest, out);
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    if e.path().is_dir() {
                        walk(&e.path(), segs, out);
                    }
                }
            }
            return;
        }
        if !seg.contains('*') {
            // A literal segment (including "..") is joined directly.
            let next = dir.join(seg);
            if rest.is_empty() {
                if next.is_file() {
                    out.push(next);
                }
            } else if next.is_dir() {
                walk(&next, rest, out);
            }
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !matches(&name, seg) {
                continue;
            }
            if rest.is_empty() {
                if e.path().is_file() {
                    out.push(e.path());
                }
            } else if e.path().is_dir() {
                walk(&e.path(), rest, out);
            }
        }
    }
    let mut out = Vec::new();
    for alt in alternatives(pattern) {
        let segs: Vec<&str> = alt.split('/').collect();
        walk(root, &segs, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

fn rel_path(root: &Path, p: &Path) -> String {
    let slash = |p: &Path| p.to_string_lossy().replace('\\', "/");
    // A path that cannot be resolved keeps its manifest spelling so the
    // baseline id stays stable.
    let Ok(canon_root) = root.canonicalize() else {
        return slash(p);
    };
    let Ok(canon) = p.canonicalize() else {
        return slash(p.strip_prefix(root).unwrap_or(p));
    };
    let rel = canon
        .strip_prefix(&canon_root)
        .map(|r| r.to_path_buf())
        .unwrap_or_else(|_| {
            // Paths outside the root (../gba/...) are expressed relative to it.
            pathdiff(&canon_root, &canon)
        });
    slash(&rel)
}

fn pathdiff(base: &Path, p: &Path) -> PathBuf {
    let base: Vec<_> = base.components().collect();
    let p: Vec<_> = p.components().collect();
    let common = base.iter().zip(&p).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..base.len() {
        out.push("..");
    }
    for c in &p[common..] {
        out.push(c);
    }
    out
}

fn reference_for(template: &str, rom: &Path) -> PathBuf {
    let dir = rom
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let stem = rom
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    PathBuf::from(template.replace("{dir}", &dir).replace("{stem}", &stem))
}

fn cases(suites: &[Suite], root: &Path, filter: Option<&str>) -> Vec<Case> {
    let mut out = Vec::new();
    for s in suites {
        let mut paths: Vec<PathBuf> = s.roms.iter().map(|r| root.join(r)).collect();
        if let Some(g) = &s.glob {
            paths.extend(expand_glob(root, g));
        }
        for p in paths {
            let rel = rel_path(root, &p);
            let id = format!("{}/{}", s.name, rel);
            if filter.is_some_and(|f| !id.contains(f)) {
                continue;
            }
            let reference = s
                .reference
                .as_ref()
                .map(|t| reference_for(t, &p))
                .and_then(|r| {
                    if r.exists() {
                        Some(r)
                    } else {
                        s.reference_alt.as_ref().map(|t| reference_for(t, &p))
                    }
                });
            out.push(Case {
                suite: s.name.clone(),
                kind: s.kind.clone(),
                model: s.model.clone(),
                frames: s.frames,
                rel,
                path: p,
                reference,
                dump_dir: None,
                hash: s.hash.clone(),
                input: s.input.clone(),
                sd_fat: s.sd_fat,
            });
        }
    }
    out
}

/// Apply mooneye's filename hints: which model the test targets, or `None`
/// to skip (SGB, DMG0, MGB and AGB variants have no equivalent here).
fn model_from_name(name: &str, default: &str) -> Option<Model> {
    let stem = name.trim_end_matches(".gb").trim_end_matches(".gbc");
    let suffix = stem.rsplit('-').next().filter(|s| *s != stem);
    let model = match suffix {
        Some("S") | Some("sgb") | Some("sgb2") | Some("dmg0") | Some("mgb") | Some("A")
        | Some("cgb0") => return None,
        Some("GS") | Some("dmgABC") | Some("dmgABCmgb") | Some("G") => Model::Dmg,
        Some("C") | Some("cgb") | Some("cgbABCDE") | Some("CE") | Some("cgbABCDE-A") => Model::Cgb,
        _ => match default {
            "cgb" => Model::Cgb,
            _ => Model::Dmg,
        },
    };
    Some(model)
}

fn boot(rom: &[u8], model: &str, name: &str, kind: &str) -> Option<Gb> {
    let cart = Cartridge::load(rom).ok()?;
    let m = match model {
        "dmg" if kind == "ldbb" => model_from_name(name, "dmg")?,
        "cgb" if kind == "ldbb" => model_from_name(name, "cgb")?,
        "dmg" => Model::Dmg,
        "cgb" => Model::Cgb,
        _ => Model::for_cart(&cart),
    };
    Some(Gb::new_with_model(cart, m))
}

fn run_case(case: &Case) -> Result_ {
    let rom = match std::fs::read(&case.path) {
        Ok(r) => r,
        Err(e) => {
            return Result_ {
                outcome: Outcome::Crash,
                detail: format!("read: {e}"),
            }
        }
    };
    if case.kind == "gba-jsmolka" {
        return run_jsmolka(&rom, case.frames);
    }
    if case.kind == "ctr-frame" {
        return run_ctr_frame(rom, case);
    }
    let name = case
        .path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let Some(mut gb) = boot(&rom, &case.model, &name, &case.kind) else {
        return Result_ {
            outcome: Outcome::Skip,
            detail: "model not emulated".into(),
        };
    };
    match case.kind.as_str() {
        "blargg-serial" => {
            for _ in 0..case.frames {
                gb.run_frame();
                gb.take_audio();
                let text = String::from_utf8_lossy(&gb.bus.serial_buf);
                if text.contains("Passed") {
                    return Result_ {
                        outcome: Outcome::Pass,
                        detail: String::new(),
                    };
                }
                if text.contains("Failed") {
                    return Result_ {
                        outcome: Outcome::Fail,
                        detail: last_line(&text),
                    };
                }
            }
            Result_ {
                outcome: Outcome::Timeout,
                detail: last_line(&String::from_utf8_lossy(&gb.bus.serial_buf)),
            }
        }
        "blargg-memory" => {
            let mut signed = false;
            for _ in 0..case.frames {
                gb.run_frame();
                gb.take_audio();
                let sig = [
                    gb.bus.read(0xA001),
                    gb.bus.read(0xA002),
                    gb.bus.read(0xA003),
                ];
                if sig == [0xDE, 0xB0, 0x61] {
                    signed = true;
                    let status = gb.bus.read(0xA000);
                    if status != 0x80 {
                        let mut text = String::new();
                        for a in 0xA004..0xA200u16 {
                            let b = gb.bus.read(a);
                            if b == 0 {
                                break;
                            }
                            text.push(b as char);
                        }
                        return Result_ {
                            outcome: if status == 0 {
                                Outcome::Pass
                            } else {
                                Outcome::Fail
                            },
                            detail: format!("status {status:#04x}: {}", last_line(&text)),
                        };
                    }
                }
            }
            Result_ {
                outcome: Outcome::Timeout,
                detail: if signed {
                    "still running".into()
                } else {
                    "no signature".into()
                },
            }
        }
        "ldbb" => {
            gb.take_breakpoint();
            for _ in 0..case.frames {
                gb.run_frame();
                gb.take_audio();
                if gb.take_breakpoint() {
                    let c = &gb.cpu;
                    let regs = [c.b, c.c, c.d, c.e, c.h, c.l];
                    let ok = regs == [3, 5, 8, 13, 21, 34];
                    return Result_ {
                        outcome: if ok { Outcome::Pass } else { Outcome::Fail },
                        detail: if ok {
                            String::new()
                        } else {
                            format!("registers {regs:?}")
                        },
                    };
                }
            }
            Result_ {
                outcome: Outcome::Timeout,
                detail: "no ld b,b".into(),
            }
        }
        "screenshot" => {
            let Some(reference) = &case.reference else {
                return Result_ {
                    outcome: Outcome::Skip,
                    detail: "no reference image".into(),
                };
            };
            gb.take_breakpoint();
            for _ in 0..case.frames {
                gb.run_frame();
                gb.take_audio();
                if gb.take_breakpoint() {
                    break;
                }
            }
            // One more frame so the last writes are on screen.
            gb.run_frame();
            let result = compare_screenshot(&gb, reference);
            if result.outcome == Outcome::Fail {
                if let Some(dir) = &case.dump_dir {
                    dump_frame(&gb, dir, &case.path);
                }
            }
            result
        }
        other => Result_ {
            outcome: Outcome::Skip,
            detail: format!("unknown kind {other}"),
        },
    }
}

fn last_line(text: &str) -> String {
    text.lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn compare_screenshot(gb: &Gb, reference: &Path) -> Result_ {
    let (w, h, rgb) = match read_png_rgb(reference) {
        Ok(v) => v,
        Err(e) => {
            return Result_ {
                outcome: Outcome::Crash,
                detail: format!("{}: {e}", reference.display()),
            }
        }
    };
    let frame = gb.frame();
    if (w, h) != (frame.width as u32, frame.height as u32) {
        return Result_ {
            outcome: Outcome::Crash,
            detail: format!("reference is {w}x{h}"),
        };
    }
    let mut wrong = 0usize;
    if gb.bus.is_cgb {
        let ours = frame.rgb.as_deref().unwrap_or(&[]);
        for (a, b) in ours.as_chunks::<3>().0.iter().zip(rgb.as_chunks::<3>().0) {
            if a != b {
                wrong += 1;
            }
        }
    } else {
        // DMG shades: 0 lightest -> #FFFFFF, 3 darkest -> #000000.
        const LEVELS: [u8; 4] = [0xFF, 0xAA, 0x55, 0x00];
        for (&s, px) in frame.shades.iter().zip(rgb.as_chunks::<3>().0) {
            if LEVELS[(s & 3) as usize] != px[0] {
                wrong += 1;
            }
        }
    }
    if wrong == 0 {
        Result_ {
            outcome: Outcome::Pass,
            detail: String::new(),
        }
    } else {
        Result_ {
            outcome: Outcome::Fail,
            detail: format!("{wrong} pixels differ"),
        }
    }
}

/// Write the frame the test produced as `<dir>/<rom stem>.png`, in the same
/// encoding the references use (DMG shades as $FF/$AA/$55/$00 grey).
fn dump_frame(gb: &Gb, dir: &Path, rom: &Path) {
    let frame = gb.frame();
    let rgba: Vec<u8> = if gb.bus.is_cgb {
        frame
            .rgb
            .as_deref()
            .unwrap_or(&[])
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|c| [c[0], c[1], c[2], 0xFF])
            .collect()
    } else {
        const LEVELS: [u8; 4] = [0xFF, 0xAA, 0x55, 0x00];
        frame
            .shades
            .iter()
            .flat_map(|&s| {
                let l = LEVELS[(s & 3) as usize];
                [l, l, l, 0xFF]
            })
            .collect()
    };
    let png = crab_cli::encode_png(frame.width as u32, frame.height as u32, &rgba);
    let stem = rom.file_stem().and_then(|s| s.to_str()).unwrap_or("frame");
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join(format!("{stem}.png")), png);
}

fn read_png_rgb(path: &Path) -> std::result::Result<(u32, u32, Vec<u8>), String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let bytes = &buf[..info.buffer_size()];
    let rgb: Vec<u8> = match info.color_type {
        png::ColorType::Rgb => bytes.to_vec(),
        png::ColorType::Rgba => bytes
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
        png::ColorType::Grayscale => bytes.iter().flat_map(|&g| [g, g, g]).collect(),
        png::ColorType::GrayscaleAlpha => bytes
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0]])
            .collect(),
        other => return Err(format!("unsupported colour type {other:?}")),
    };
    Ok((info.width, info.height, rgb))
}

/// jsmolka gba-tests: the "All tests passed" screen, else the failed test
/// number printed on screen is reported via the frame hash.
fn run_jsmolka(rom: &[u8], frames: u64) -> Result_ {
    let mut gba = gba_core::Gba::new(rom.to_vec());
    let pass_hash = 0xA313_C705u32;
    let mut last = 0;
    for f in 0..frames {
        gba.run_frame();
        gba.take_audio();
        if f % 30 == 29 {
            let h = crab_cli::fnv1a32(&gba.frame().to_rgba(&emu_core::DMG_PALETTE));
            if h == pass_hash {
                return Result_ {
                    outcome: Outcome::Pass,
                    detail: String::new(),
                };
            }
            last = h;
        }
    }
    Result_ {
        outcome: Outcome::Fail,
        detail: format!("frame hash {last:08x}"),
    }
}

/// A 3DS payload: run it for the frame budget, with an optional SD card and
/// input script, and compare the hash of the top screen with the pinned one.
fn run_ctr_frame(firm: Vec<u8>, case: &Case) -> Result_ {
    use emu_core::System;
    let crash = |detail: String| Result_ {
        outcome: Outcome::Crash,
        detail,
    };
    let mut ctr = match ctr_core::Ctr::from_firm(firm) {
        Ok(ctr) => ctr,
        Err(e) => return crash(e),
    };
    if case.sd_fat {
        let files: [(&str, &[u8]); 1] = [("HELLO.TXT", b"Hello from CrabBoy\n")];
        match ctr_fs::fat::build(&files, 65536) {
            Ok(image) => ctr.insert_sd(image),
            Err(e) => return crash(e.to_string()),
        }
    }
    let script = match case.input.as_deref().map(crab_cli::parse_input_script) {
        Some(Ok(script)) => script,
        Some(Err(e)) => return crash(e),
        None => Vec::new(),
    };
    for frame in 0..case.frames {
        for event in script.iter().filter(|e| e.frame == frame) {
            if event.pressed {
                ctr.press(event.button);
            } else {
                ctr.release(event.button);
            }
        }
        ctr.run_frame();
    }
    let hash = crab_cli::fnv1a32(&ctr.frame().to_rgba(&emu_core::DMG_PALETTE));
    let expected = case.hash.as_deref().unwrap_or("");
    if format!("{hash:08x}") == expected {
        Result_ {
            outcome: Outcome::Pass,
            detail: String::new(),
        }
    } else {
        Result_ {
            outcome: Outcome::Fail,
            detail: format!("frame hash {hash:08x}, expected {expected}"),
        }
    }
}

fn read_baseline(path: &Path) -> BTreeMap<String, Outcome> {
    let mut out = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((id, res)) = line.rsplit_once(' ') {
                if let Some(o) = Outcome::parse(res.trim()) {
                    out.insert(id.trim().to_string(), o);
                }
            }
        }
    }
    out
}

fn main() {
    let mut suites_path = PathBuf::from("tests/accuracy/suites.toml");
    let mut roms_root = PathBuf::from("roms/test-suites/gb");
    let mut baseline_path = PathBuf::from("tests/accuracy/baseline.txt");
    let mut filter: Option<String> = None;
    let mut jobs = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mut ci = false;
    let mut update = false;
    let mut markdown: Option<PathBuf> = None;
    let mut verbose = false;
    let mut dump_dir: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || {
            args.next()
                .unwrap_or_else(|| fail(format!("{a} needs a value")))
        };
        match a.as_str() {
            "--suites" => suites_path = value().into(),
            "--roms" => roms_root = value().into(),
            "--baseline" => baseline_path = value().into(),
            "--filter" => filter = Some(value()),
            "--jobs" => {
                jobs = value()
                    .parse()
                    .unwrap_or_else(|_| fail("--jobs needs a number"))
            }
            "--ci" => ci = true,
            "--update-baseline" => update = true,
            "--markdown" => markdown = Some(value().into()),
            "--verbose" | "-v" => verbose = true,
            "--dump-failures" => dump_dir = Some(value().into()),
            "-h" | "--help" => {
                println!("usage: accuracy [--suites F] [--roms DIR] [--baseline F] [--filter TEXT] [--jobs N] [--ci] [--update-baseline] [--markdown F] [--verbose] [--dump-failures DIR]");
                return;
            }
            other => fail(format!("unknown argument {other}")),
        }
    }

    let suites = load_suites(&suites_path);
    let mut all = cases(&suites, &roms_root, filter.as_deref());
    for case in &mut all {
        case.dump_dir = dump_dir.clone();
    }
    if all.is_empty() {
        fail(
            "no test ROMs found; run `cargo run --release -p crab-cli --bin fetch_test_roms` first",
        );
    }
    eprintln!("running {} tests on {jobs} threads", all.len());

    // Panics inside a core are reported as CRASH, not printed.
    std::panic::set_hook(Box::new(|_| {}));
    let next = Mutex::new(0usize);
    let results: Mutex<Vec<Option<Result_>>> = Mutex::new(vec![None; all.len()]);
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| loop {
                let i = {
                    let mut n = next.lock().unwrap();
                    let i = *n;
                    *n += 1;
                    i
                };
                if i >= all.len() {
                    break;
                }
                let case = &all[i];
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_case(case)))
                    .unwrap_or_else(|_| Result_ {
                        outcome: Outcome::Crash,
                        detail: "panic".into(),
                    });
                if verbose {
                    eprintln!(
                        "{:8} {}/{} {}",
                        r.outcome.name(),
                        case.suite,
                        case.rel,
                        r.detail
                    );
                }
                results.lock().unwrap()[i] = Some(r);
            });
        }
    });
    let _ = std::panic::take_hook();
    let results: Vec<Result_> = results
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    // Per-suite summary.
    let mut per_suite: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
    for (case, r) in all.iter().zip(&results) {
        let e = per_suite.entry(&case.suite).or_default();
        match r.outcome {
            Outcome::Pass => e.0 += 1,
            Outcome::Skip => e.2 += 1,
            _ => e.1 += 1,
        }
    }
    let mut md = String::from("| Suite | Passed | Failed | Skipped |\n|---|---|---|---|\n");
    for (name, (p, f, s)) in &per_suite {
        println!("{name:26} {p:4} passed {f:4} failed {s:3} skipped");
        md.push_str(&format!("| {name} | {p} | {f} | {s} |\n"));
    }
    let total_pass = per_suite.values().map(|v| v.0).sum::<usize>();
    let total = all.len();
    println!("total {total_pass}/{total} passed");

    if let Some(path) = &markdown {
        let mut doc = String::from("# Accuracy suite results\n\nGenerated by `cargo run --release -p crab-cli --bin accuracy -- --markdown docs/accuracy.md`.\n\n");
        doc.push_str(&md);
        doc.push_str("\n## Failing tests\n\n");
        for (case, r) in all.iter().zip(&results) {
            if !matches!(r.outcome, Outcome::Pass | Outcome::Skip) {
                doc.push_str(&format!(
                    "- `{}/{}`: {} {}\n",
                    case.suite,
                    case.rel,
                    r.outcome.name(),
                    r.detail
                ));
            }
        }
        std::fs::write(path, doc)
            .unwrap_or_else(|e| fail(format!("write {}: {e}", path.display())));
        println!("wrote {}", path.display());
    }

    let baseline = read_baseline(&baseline_path);
    if update {
        let mut text =
            String::from("# accuracy baseline: <suite>/<rom> <PASS|FAIL|TIMEOUT|CRASH|SKIP>\n");
        for (case, r) in all.iter().zip(&results) {
            text.push_str(&format!(
                "{}/{} {}\n",
                case.suite,
                case.rel,
                r.outcome.name()
            ));
        }
        std::fs::write(&baseline_path, text)
            .unwrap_or_else(|e| fail(format!("write {}: {e}", baseline_path.display())));
        println!("wrote {}", baseline_path.display());
        return;
    }
    if ci || !baseline.is_empty() {
        let mut regressions = 0;
        let mut improvements = 0;
        let mut unknown = 0;
        for (case, r) in all.iter().zip(&results) {
            let id = format!("{}/{}", case.suite, case.rel);
            match baseline.get(&id) {
                None => unknown += 1,
                Some(&b) if b == r.outcome => {}
                Some(&b) => {
                    let regression = b == Outcome::Pass || r.outcome == Outcome::Crash;
                    if regression {
                        regressions += 1;
                    } else {
                        improvements += 1;
                    }
                    println!(
                        "{} {id}: baseline {} now {} {}",
                        if regression {
                            "REGRESSION"
                        } else {
                            "IMPROVEMENT"
                        },
                        b.name(),
                        r.outcome.name(),
                        r.detail
                    );
                }
            }
        }
        println!(
            "{regressions} regressions, {improvements} improvements, {unknown} not in baseline"
        );
        if ci && (regressions > 0 || improvements > 0 || unknown > 0) {
            eprintln!(
                "results differ from {}; run with --update-baseline to record them",
                baseline_path.display()
            );
            std::process::exit(1);
        }
    }
}
