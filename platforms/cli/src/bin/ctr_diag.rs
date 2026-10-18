//! Headless 3DS diagnostics: run a FIRM, report where the processors are and
//! which unmodelled registers they touched, and dump the screens.
//!
//! ```text
//! ctr-diag <firm> <frames> [--dump=PREFIX] [--input=SCRIPT] [--every=N]
//!          [--sd=IMAGE | --sd-fat | --sd-dir=DIR] [--watch=LO-HI]
//!          [--mem=ADDR,WORDS] [--save=ADDR,BYTES,FILE] [--regs]
//! ```
//!
//! `--dump` writes `PREFIX-top.png` and `PREFIX-bottom.png` after the last
//! frame. `--sd-dir` makes a 64 MB FAT16 card holding the files under a
//! directory. `--input` takes the same script as `crab run`. `--mem` (hex
//! physical address, repeatable) prints words of memory, `--save` writes a
//! stretch of it to a file, and `--regs` prints the registers of every
//! processor, all after the last frame. `--watch` (hex, ends
//! included, repeatable) prints the latest accesses to a register range in
//! the order they happened.
//!
//! Any other image (a card image, an NCCH, a 3DSX) runs in the high-level
//! mode, which also takes `--strict` (stop at the first unwritten service
//! command), `--watch=ADDR` (one virtual word),
//! `--trace=svc,ipc,gx,irq,dsp,ro,fs,service,all` (what to log),
//! `--sav=FILE` (the program's saves, read before the run and written after
//! it if the program saved) and `--dump-every` (with `--dump`, also write
//! `PREFIX-fNNNNN-top.png` and `-bottom.png` at every report).
//!
//! The input script is the grammar of `crab run --input`, which also holds
//! the stylus (`TOUCH:X:Y@F`, `!TOUCH@F`) and moves the circle pad
//! (`CPAD:X:Y@F`, `!CPAD@F`).

use crab_cli::{encode_png, fnv1a32, parse_input_script};
use ctr_core::Ctr;
use emu_core::{Layout, System, DMG_PALETTE};

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("ctr-diag: {message}");
    std::process::exit(1);
}

/// Every file under `dir` with its path relative to `base`, `/` between parts,
/// in name order so that the card is the same on every host.
fn collect(base: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
    let read = std::fs::read_dir(dir).unwrap_or_else(|e| fail(format!("{}: {e}", dir.display())));
    let mut paths: Vec<_> = read.filter_map(|entry| Some(entry.ok()?.path())).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect(base, &path, out);
        } else {
            let data =
                std::fs::read(&path).unwrap_or_else(|e| fail(format!("{}: {e}", path.display())));
            let relative = path.strip_prefix(base).expect("found under the base");
            let name: Vec<_> = relative
                .iter()
                .map(|part| part.to_string_lossy().into_owned())
                .collect();
            out.push((name.join("/"), data));
        }
    }
}

/// Write both screens of `system` as `PREFIX-top.png` and
/// `PREFIX-bottom.png`.
fn dump_screens(system: &dyn System, prefix: &str) {
    for (index, name) in ["top", "bottom"].iter().enumerate() {
        let frame = system.frame_at(index);
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

/// The high-level mode: the program's log as it runs, its threads, and why
/// it stopped.
fn run_hle(
    path: &str,
    media: crab_cli::FileStorage,
    frames: u64,
    every: u64,
    script: &[crab_cli::InputEvent],
    args: &[String],
) {
    use ctr_hle::kernel::State;
    use ctr_hle::Horizon;
    let option = |name: &str| {
        args.iter()
            .find_map(|a| a.strip_prefix(&format!("--{name}=")).map(str::to_string))
    };
    let homebrew = std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("3dsx"));
    let console = if homebrew {
        let image = std::fs::read(path).unwrap_or_else(|e| fail(format!("{path}: {e}")));
        Horizon::from_3dsx(&image)
    } else {
        Horizon::from_media(Box::new(media))
    };
    let mut h = console.unwrap_or_else(|e| fail(e));
    h.strict = args.iter().any(|a| a == "--strict");
    if let Some(names) = option("trace") {
        for name in names.split(',') {
            h.trace |= ctr_hle::trace::flag(name)
                .unwrap_or_else(|| fail(format!("--trace: no area called {name}")));
        }
    }
    if let Some(addr) =
        option("watch").and_then(|a| u32::from_str_radix(a.trim_start_matches("0x"), 16).ok())
    {
        let now = h.process.space.read32(&h.mem, addr).unwrap_or(0);
        h.watch = Some((addr, now));
    }
    let sav = option("sav");
    if let Some(sav) = &sav {
        match std::fs::read(sav) {
            Ok(data) => {
                h.load_data(&data);
                println!("read {} bytes of saves from {sav}", data.len());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => fail(format!("{sav}: {e}")),
        }
    }
    let dump_every = args.iter().any(|a| a == "--dump-every");
    println!("{}", h.info());
    let started = std::time::Instant::now();
    for frame in 0..frames {
        for event in script.iter().filter(|e| e.frame == frame) {
            event.apply(&mut h);
        }
        h.run_frame();
        for line in h.log.drain(..) {
            println!("  [{:5}] {line}", frame + 1);
        }
        let last = frame + 1 == frames || !h.running();
        if (frame + 1) % every == 0 || last {
            let top = h.frame().to_rgba(&DMG_PALETTE);
            let both = Layout::of(&h)
                .compose(&h, &DMG_PALETTE)
                .to_rgba(&DMG_PALETTE);
            let ready = h
                .kernel
                .threads
                .iter()
                .filter(|t| t.state == State::Ready)
                .count();
            let waiting = h
                .kernel
                .threads
                .iter()
                .filter(|t| matches!(t.state, State::Waiting(_)))
                .count();
            println!(
                "frame {:5}  top {:08x}  both {:08x}  threads {} ({ready} ready, {waiting} waiting)  {:.1}s",
                frame + 1,
                fnv1a32(&top),
                fnv1a32(&both),
                h.kernel.threads.len(),
                started.elapsed().as_secs_f64()
            );
            if let (true, Some(prefix)) = (dump_every, option("dump")) {
                dump_screens(&h, &format!("{prefix}-f{:05}", frame + 1));
            }
        }
        if !h.running() {
            break;
        }
    }
    if let Some(why) = &h.fatal {
        println!("stopped: {why}");
    }
    println!("threads:");
    for (id, thread) in h.kernel.threads.iter().enumerate() {
        let regs = if h.kernel.current == Some(id) {
            h.cpu.save_context().r
        } else {
            thread.context.r
        };
        println!(
            "  {id:2}  priority {:2}  core {:2}  pc {:#010x}  lr {:#010x}  sp {:#010x}  {:?}",
            thread.priority, thread.processor, regs[15], regs[14], regs[13], thread.state
        );
    }
    println!("services: {}", h.service_names().join(", "));
    if args.iter().any(|a| a == "--regs") {
        for (id, thread) in h.kernel.threads.iter().enumerate() {
            let regs = if h.kernel.current == Some(id) {
                h.cpu.save_context().r
            } else {
                thread.context.r
            };
            let regs: Vec<String> = regs.iter().map(|r| format!("{r:08x}")).collect();
            println!("  {id:2}  {}", regs.join(" "));
        }
    }
    // Words of the process's memory, by virtual address.
    for spec in args.iter().filter_map(|a| a.strip_prefix("--mem=")) {
        let parsed = spec.split_once(',').and_then(|(addr, words)| {
            let addr = u32::from_str_radix(addr.trim_start_matches("0x"), 16).ok()?;
            Some((addr, words.parse::<u32>().ok()?))
        });
        let Some((addr, words)) = parsed else {
            fail(format!("--mem={spec}: expected ADDR,WORDS"));
        };
        for n in 0..words {
            let at = addr.wrapping_add(n * 4);
            match h.process.space.read32(&h.mem, at) {
                Some(word) => println!("  {at:08x}: {word:08x}"),
                None => println!("  {at:08x}: not mapped"),
            }
        }
    }
    if let Some(prefix) = option("dump") {
        dump_screens(&h, &prefix);
    }
    if let Some(sav) = &sav {
        if h.battery_backed() {
            let data = h.save_data();
            if !data.is_empty() {
                std::fs::write(sav, &data).unwrap_or_else(|e| fail(format!("{sav}: {e}")));
                println!("wrote {} bytes of saves to {sav}", data.len());
            }
        }
    }
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

    // Firmware runs on the low-level machine; games and homebrew in the
    // high-level mode.
    let mut media =
        crab_cli::FileStorage::open(path).unwrap_or_else(|e| fail(format!("{path}: {e}")));
    let mut header = vec![0u8; emu_core::Storage::len(&media).min(0x200) as usize];
    emu_core::Storage::read_at(&mut media, 0, &mut header).unwrap_or_else(|e| fail(e));
    if ctr_fs::ImageKind::detect(&header) != Some(ctr_fs::ImageKind::Firm) {
        return run_hle(path, media, frames, every, &script, &args);
    }

    let firm = std::fs::read(path).unwrap_or_else(|e| fail(format!("{path}: {e}")));
    let mut ctr = Ctr::from_firm(firm).unwrap_or_else(|e| fail(e));
    if let Some(sd) = option("sd") {
        ctr.insert_sd(std::fs::read(&sd).unwrap_or_else(|e| fail(format!("{sd}: {e}"))));
    }
    if args.iter().any(|a| a == "--sd-fat") {
        // A 32 MB FAT16 card with one file, made on the spot.
        let files: [(&str, &[u8]); 1] = [("HELLO.TXT", b"Hello from CrabBoy\n")];
        ctr.insert_sd(ctr_fs::fat::build(&files, 65536).unwrap_or_else(|e| fail(e)));
    }
    if let Some(dir) = option("sd-dir") {
        let mut files = Vec::new();
        let base = std::path::Path::new(&dir);
        collect(base, base, &mut files);
        let files: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(name, data)| (name.as_str(), data.as_slice()))
            .collect();
        ctr.insert_sd(ctr_fs::fat::build(&files, 131072).unwrap_or_else(|e| fail(e)));
    }
    for range in args.iter().filter_map(|a| a.strip_prefix("--watch=")) {
        let parse = |v: &str| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok();
        match range.split_once('-').map(|(lo, hi)| (parse(lo), parse(hi))) {
            Some((Some(lo), Some(hi))) => ctr.io_mut().trace.watch(lo, hi),
            _ => fail(format!("--watch={range}: expected LO-HI in hex")),
        }
    }
    println!("{}", ctr.info());

    for frame in 0..frames {
        for event in script.iter().filter(|e| e.frame == frame) {
            event.apply(&mut ctr);
        }
        ctr.run_frame();
        if (frame + 1) % every == 0 || frame + 1 == frames {
            let top = ctr.frame().to_rgba(&DMG_PALETTE);
            let both = Layout::of(&ctr)
                .compose(&ctr, &DMG_PALETTE)
                .to_rgba(&DMG_PALETTE);
            println!(
                "frame {:5}  top {:08x}  both {:08x}  {}",
                frame + 1,
                fnv1a32(&top),
                fnv1a32(&both),
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

    println!(
        "irq9: enabled {:08x} pending {:08x}",
        ctr.io().irq9.enable,
        ctr.io().irq9.pending
    );
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

    if args.iter().any(|a| a == "--regs") {
        let cpus = [
            ("arm9", ctr.cpu9()),
            ("arm11/0", ctr.cpu11(0)),
            ("arm11/1", ctr.cpu11(1)),
        ];
        for (name, cpu) in cpus {
            let regs: Vec<String> = (0..16).map(|r| format!("{:08x}", cpu.reg(r))).collect();
            println!("{name:8} {}", regs.join(" "));
        }
    }
    for spec in args.iter().filter_map(|a| a.strip_prefix("--mem=")) {
        let parsed = spec.split_once(',').and_then(|(addr, words)| {
            let addr = u32::from_str_radix(addr.trim_start_matches("0x"), 16).ok()?;
            Some((addr, words.parse::<u32>().ok()?))
        });
        let Some((addr, words)) = parsed else {
            fail(format!("--mem={spec}: expected ADDR,WORDS"));
        };
        for n in 0..words {
            let at = addr.wrapping_add(n * 4);
            match ctr.mem().slice(at, 4) {
                Some(b) => println!(
                    "  {at:08x}: {:08x}",
                    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
                ),
                None => println!("  {at:08x}: not memory"),
            }
        }
    }

    for spec in args.iter().filter_map(|a| a.strip_prefix("--save=")) {
        let mut parts = spec.splitn(3, ',');
        let hex = |v: Option<&str>| u32::from_str_radix(v?.trim_start_matches("0x"), 16).ok();
        let (Some(addr), Some(len), Some(file)) =
            (hex(parts.next()), hex(parts.next()), parts.next())
        else {
            fail(format!(
                "--save={spec}: expected ADDR,BYTES,FILE with hex numbers"
            ));
        };
        let Some(bytes) = ctr.mem().slice(addr, len as usize) else {
            fail(format!("--save={spec}: not memory"));
        };
        std::fs::write(file, bytes).unwrap_or_else(|e| fail(format!("{file}: {e}")));
        println!("wrote {file}");
    }

    for access in ctr.io().trace.logged() {
        println!(
            "  {} {:#010x} {:#010x}",
            if access.write { "w" } else { "r" },
            access.addr,
            access.value
        );
    }

    if let Some(prefix) = option("dump") {
        dump_screens(&ctr, &prefix);
    }
}
