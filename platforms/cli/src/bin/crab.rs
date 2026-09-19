//! `crab`: the headless CrabBoy runner for every supported console.
//!
//! ```text
//! crab info <rom>
//! crab run <rom> [--frames N] [--input SPEC] [--dump-frame N] [--dump PATH]
//!                [--wav PATH] [--hash] [--sav PATH] [--bios PATH] [--cold]
//! ```

use crab_cli::{encode_png, encode_ppm, encode_wav, fnv1a32, parse_input_script, InputEvent};
use emu_core::{Layout, System, DMG_PALETTE};

const USAGE: &str = "\
usage:
  crab info <rom>
  crab run <rom> [options]

options for run:
  --frames N        frames to emulate (default 600)
  --input SPEC      scripted input, e.g. START@400,!START@410
  --dump PATH       write the frame reached at --dump-frame (or the last frame)
                    as .png or .ppm
  --dump-frame N    frame to dump (frames completed; default: --frames)
  --wav PATH        write all audio produced as a 16-bit stereo WAV
  --hash            print the FNV-1a-32 hash of the final RGBA frame
  --sav PATH        battery save to load before and write after the run
  --bios PATH       GBA BIOS image (16 KB); default is high-level emulation
  --cold            with --bios, run the cold-boot logo intro";

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("crab: {msg}");
    std::process::exit(1)
}

struct RunArgs {
    rom: String,
    frames: u64,
    input: Vec<InputEvent>,
    dump: Option<String>,
    dump_frame: Option<u64>,
    wav: Option<String>,
    hash: bool,
    sav: Option<String>,
    bios: Option<String>,
    cold: bool,
}

fn parse_run_args(args: &[String]) -> RunArgs {
    let mut out = RunArgs {
        rom: String::new(),
        frames: 600,
        input: Vec::new(),
        dump: None,
        dump_frame: None,
        wav: None,
        hash: false,
        sav: None,
        bios: None,
        cold: false,
    };
    let mut it = args.iter();
    let value = |flag: &str, it: &mut std::slice::Iter<String>| -> String {
        it.next()
            .cloned()
            .unwrap_or_else(|| fail(format!("{flag} needs a value")))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--frames" => {
                out.frames = value(a, &mut it)
                    .parse()
                    .unwrap_or_else(|_| fail("--frames needs a number"));
            }
            "--input" => {
                out.input = parse_input_script(&value(a, &mut it)).unwrap_or_else(|e| fail(e));
            }
            "--dump" => out.dump = Some(value(a, &mut it)),
            "--dump-frame" => {
                out.dump_frame = Some(
                    value(a, &mut it)
                        .parse()
                        .unwrap_or_else(|_| fail("--dump-frame needs a number")),
                );
            }
            "--wav" => out.wav = Some(value(a, &mut it)),
            "--hash" => out.hash = true,
            "--sav" => out.sav = Some(value(a, &mut it)),
            "--bios" => out.bios = Some(value(a, &mut it)),
            "--cold" => out.cold = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ if a.starts_with('-') => fail(format!("unknown option {a}")),
            _ if out.rom.is_empty() => out.rom = a.clone(),
            _ => fail(format!("unexpected argument {a}")),
        }
    }
    if out.rom.is_empty() {
        fail("run needs a ROM path");
    }
    out
}

fn load_system(rom_path: &str, bios: Option<&str>, cold: bool) -> Box<dyn System> {
    let rom = std::fs::read(rom_path).unwrap_or_else(|e| fail(format!("read {rom_path}: {e}")));
    let gba_bios =
        bios.map(|p| std::fs::read(p).unwrap_or_else(|e| fail(format!("read {p}: {e}"))));
    let opts = crab_systems::LoadOptions { gba_bios, cold };
    crab_systems::load_with(rom, &opts).unwrap_or_else(|e| fail(format!("{rom_path}: {e}")))
}

/// Every display of the system in one image (the 3DS has two).
fn rgba(system: &dyn System) -> (u32, u32, Vec<u8>) {
    let frame = Layout::of(system).compose(system, &DMG_PALETTE);
    (
        frame.width as u32,
        frame.height as u32,
        frame.to_rgba(&DMG_PALETTE),
    )
}

fn cmd_info(rom_path: &str) {
    let data = std::fs::read(rom_path).unwrap_or_else(|e| fail(format!("read {rom_path}: {e}")));
    let size = data.len();
    let Some(kind) = crab_systems::detect(&data) else {
        fail(format!(
            "{rom_path}: not a Game Boy, Game Boy Color or Game Boy Advance ROM"
        ));
    };
    let system = crab_systems::load(data).unwrap_or_else(|e| fail(format!("{rom_path}: {e}")));
    let screen = system.screen();
    println!("file:     {rom_path}");
    println!("size:     {size} bytes");
    println!("system:   {}", kind.name());
    println!("title:    {}", system.title());
    println!("info:     {}", system.info());
    println!("screen:   {}x{}", screen.width, screen.height);
    println!(
        "rate:     {} Hz video, {} Hz audio",
        system.frame_rate(),
        system.audio_rate()
    );
    println!("battery:  {}", system.battery_backed());
}

fn cmd_run(args: RunArgs) {
    let mut system = load_system(&args.rom, args.bios.as_deref(), args.cold);
    if let Some(sav) = &args.sav {
        match std::fs::read(sav) {
            Ok(d) => system.load_data(&d),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => fail(format!("read {sav}: {e}")),
        }
    }
    let dump_at = args.dump_frame.unwrap_or(args.frames);
    let mut audio: Vec<f32> = Vec::new();
    let mut audio_rate = system.audio_rate();
    let mut next_event = 0;
    let mut dumped = false;

    let maybe_dump = |system: &dyn System, frame: u64, dumped: &mut bool| {
        if *dumped || frame != dump_at {
            return;
        }
        if let Some(path) = &args.dump {
            let (w, h, px) = rgba(system);
            let bytes = if path.to_ascii_lowercase().ends_with(".png") {
                encode_png(w, h, &px)
            } else {
                encode_ppm(w, h, &px)
            };
            std::fs::write(path, bytes).unwrap_or_else(|e| fail(format!("write {path}: {e}")));
            println!("dumped frame {frame} to {path}");
        }
        *dumped = true;
    };

    for frame in 0..args.frames {
        maybe_dump(system.as_ref(), frame, &mut dumped);
        while next_event < args.input.len() && args.input[next_event].frame <= frame {
            let ev = args.input[next_event];
            if ev.pressed {
                system.press(ev.button);
            } else {
                system.release(ev.button);
            }
            next_event += 1;
        }
        system.run_frame();
        if args.wav.is_some() {
            let rate = system.audio_rate();
            if rate != audio_rate {
                eprintln!("crab: audio rate changed {audio_rate} -> {rate} Hz mid-run; wav keeps the first rate");
                audio_rate = rate;
            }
            audio.extend(system.take_audio().samples);
        } else {
            system.take_audio();
        }
    }
    maybe_dump(system.as_ref(), args.frames, &mut dumped);
    if args.dump.is_some() && !dumped {
        fail(format!(
            "--dump-frame {dump_at} is beyond --frames {}",
            args.frames
        ));
    }

    if args.hash {
        let (_, _, px) = rgba(system.as_ref());
        println!("hash @f{}: {:08x}", args.frames, fnv1a32(&px));
    }
    if let Some(path) = &args.wav {
        let rate = system.audio_rate().min(audio_rate).max(1);
        std::fs::write(path, encode_wav(&audio, rate))
            .unwrap_or_else(|e| fail(format!("write {path}: {e}")));
        println!(
            "wrote {} samples ({:.2} s) to {path} at {rate} Hz",
            audio.len() / 2,
            audio.len() as f64 / 2.0 / rate as f64
        );
    }
    if let Some(sav) = &args.sav {
        if system.battery_backed() {
            let data = system.save_data();
            if !data.is_empty() {
                std::fs::write(sav, &data).unwrap_or_else(|e| fail(format!("write {sav}: {e}")));
                println!("wrote {} bytes of save data to {sav}", data.len());
            }
        }
    }
    println!(
        "ran {} frames of {} ({})",
        args.frames,
        system.title(),
        system.name()
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("info") => match args.get(1) {
            Some(rom) if args.len() == 2 => cmd_info(rom),
            _ => fail("info takes exactly one ROM path"),
        },
        Some("run") => cmd_run(parse_run_args(&args[1..])),
        Some("-h") | Some("--help") => println!("{USAGE}"),
        Some(other) => fail(format!("unknown command {other}\n{USAGE}")),
        None => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}
