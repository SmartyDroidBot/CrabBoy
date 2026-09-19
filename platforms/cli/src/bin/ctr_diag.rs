//! Headless 3DS diagnostics: run a FIRM, report where the processors are and
//! which unmodelled registers they touched, and dump the screens.
//!
//! ```text
//! ctr-diag <firm> <frames> [--dump=PREFIX] [--input=SCRIPT] [--every=N]
//! ```
//!
//! `--dump` writes `PREFIX-top.png` and `PREFIX-bottom.png` after the last
//! frame. `--input` takes the same script as `crab run`.

use crab_cli::{encode_png, fnv1a32, parse_input_script};
use ctr_core::Ctr;
use emu_core::{System, DMG_PALETTE};

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("ctr-diag: {message}");
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut positional = args.iter().filter(|a| !a.starts_with("--"));
    let (Some(path), Some(frames)) = (positional.next(), positional.next()) else {
        fail("usage: ctr-diag <firm> <frames> [--dump=PREFIX] [--input=SCRIPT] [--every=N]");
    };
    let frames: u64 = frames
        .parse()
        .unwrap_or_else(|e| fail(format!("frames: {e}")));
    let option = |name: &str| {
        args.iter()
            .find_map(|a| a.strip_prefix(&format!("--{name}=")).map(str::to_string))
    };
    let every: u64 = option("every").map_or(60, |v| v.parse().unwrap_or(60));
    let script = option("input")
        .map(|s| parse_input_script(&s).unwrap_or_else(|e| fail(e)))
        .unwrap_or_default();

    let firm = std::fs::read(path).unwrap_or_else(|e| fail(format!("{path}: {e}")));
    let mut ctr = Ctr::from_firm(firm).unwrap_or_else(|e| fail(e));
    println!("{}", ctr.info());

    for frame in 0..frames {
        for event in script.iter().filter(|e| e.frame == frame) {
            if event.pressed {
                ctr.press(event.button);
            } else {
                ctr.release(event.button);
            }
        }
        ctr.run_frame();
        if (frame + 1) % every == 0 || frame + 1 == frames {
            let top = ctr.frame().to_rgba(&DMG_PALETTE);
            println!(
                "frame {:5}  top {:08x}  {}",
                frame + 1,
                fnv1a32(&top),
                ctr.describe()
            );
        }
    }

    // One more frame, to show what the software is busy with at the end.
    ctr.io_mut().trace.take_recent();
    ctr.run_frame();
    println!("i/o in the last frame:");
    for (addr, entry) in ctr.io_mut().trace.take_recent() {
        println!(
            "  {addr:#010x}  reads {:8}  writes {:8}  last write {:#010x}",
            entry.reads, entry.writes, entry.last_write
        );
    }

    let gic = &ctr.io().mpcore.gic;
    let words = |base: u32| -> Vec<String> {
        (0..4)
            .map(|n| format!("{:08x}", gic.read_distributor(0, base + n * 4)))
            .collect()
    };
    println!(
        "gic: control {} enabled {:?} pending {:?} active {:?}",
        gic.read_distributor(0, 0),
        words(0x100),
        words(0x200),
        words(0x300)
    );

    println!("unmodelled registers:");
    for (addr, entry) in ctr.io().trace.entries() {
        println!(
            "  {addr:#010x}  reads {:8}  writes {:8}  last write {:#010x}",
            entry.reads, entry.writes, entry.last_write
        );
    }

    if let Some(prefix) = option("dump") {
        for (index, name) in ["top", "bottom"].iter().enumerate() {
            let frame = ctr.frame_at(index);
            let png = encode_png(
                frame.width as u32,
                frame.height as u32,
                &frame.to_rgba(&DMG_PALETTE),
            );
            let file = format!("{prefix}-{name}.png");
            std::fs::write(&file, png).unwrap_or_else(|e| fail(format!("{file}: {e}")));
            println!("wrote {file}");
        }
    }
}
