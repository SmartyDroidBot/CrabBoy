//! Run every test ROM in a directory through `test_runner` and summarize.
//!
//! Each ROM is executed in a subprocess so a hang or crash in one test cannot
//! affect the others. Exit codes: 0 = PASS, 1 = FAIL. Anything else (timeout,
//! no result) is INCONCLUSIVE.
//!
//! Usage: run_accuracy <dir> [runner-path]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn collect_roms(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_roms(&p, out);
        } else if p.extension().map(|x| x == "gb").unwrap_or(false) {
            out.push(p);
        }
    }
}

fn main() -> ExitCode {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: run_accuracy <dir> [runner-path]");
    let runner = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "test_runner".to_string());

    let mut roms: Vec<PathBuf> = Vec::new();
    collect_roms(Path::new(&dir), &mut roms);
    roms.sort();

    if roms.is_empty() {
        eprintln!("no .gb files under {dir}");
        return ExitCode::FAILURE;
    }

    let mut pass = 0;
    let mut fail = 0;
    let mut inconclusive = 0;
    for rom in &roms {
        let name = Path::new(rom)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| rom.to_string_lossy().to_string());
        let out = match Command::new(&runner).arg(rom).output() {
            Ok(o) => o,
            Err(e) => {
                eprintln!("  {name:<48} ERROR ({e})");
                continue;
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stdout = stdout.trim();
        let result = match out.status.code() {
            Some(0) => {
                pass += 1;
                "PASS"
            }
            Some(_) => {
                fail += 1;
                "FAIL"
            }
            None => {
                inconclusive += 1;
                "??"
            }
        };
        eprintln!("  {name:<48} {result}  {stdout}");
    }

    println!(
        "SUMMARY: {} passed, {} failed, {} inconclusive ({} total)",
        pass,
        fail,
        inconclusive,
        roms.len()
    );
    if fail == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}