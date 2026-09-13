//! Headless GBA diagnostic runner.
//!
//! Runs a ROM without a display and prints what the machine is doing so a
//! "black screen" hang can be debugged: the PC, DISPCNT, whether the CPU is
//! halted, and any unrecognised BIOS SWIs, sampled every N frames. A wild PC
//! (outside any executable region) dumps the last 128 instructions.
//!
//! Usage: `gba-diag <rom.gba> [frames] [interval] [trace_steps] [options]`
//!
//!   `--bios=<path>`        boot through a real BIOS image (`--cold` for the
//!                          full power-on path instead of the warm POSTFLG boot)
//!   `--capture-pc=<hex>`   stop when PC equals the address and dump registers
//!   `--dump-iwram=<path>`  write 0x03007E00-0x03007FFF to a file at the end
//!   `--dump-vram=<path>`   write palette RAM, VRAM and OAM to a file at the end
//!   `--dump-frame=<path>`  write the framebuffer as a binary PPM at the end
//!                          (or at `--dump-frame-at=<frame>`)
//!   `--frame-hash`         print an FNV-1a hash of the framebuffer per sample
//!   `--input=SPEC`         press/release buttons on given frames, e.g.
//!                          `START@120,A@300,!A@310` (`!` releases)
//!   `--trace-io`           print every I/O register write while tracing
//!   `--trace-ram`          print writes to the IRQ handler area of IWRAM and to VRAM

use emu_core::{Button, System};
use gba_core::Gba;
use std::time::Instant;

struct Options {
    rom: String,
    total_frames: u64,
    interval: u64,
    trace_steps: u32,
    bios: Option<String>,
    cold: bool,
    capture_pc: Option<u32>,
    dump_iwram: Option<String>,
    dump_vram: Option<String>,
    dump_frame: Option<String>,
    dump_frame_at: Option<u64>,
    frame_hash: bool,
    inputs: Vec<(u64, Button, bool)>,
    trace_io: bool,
    trace_ram: bool,
}

fn usage() -> ! {
    eprintln!(
        "usage: gba-diag <rom.gba> [frames] [interval] [trace_steps] [--bios=<path>] [--cold] \
         [--capture-pc=<hex>] [--dump-iwram=<path>] [--dump-vram=<path>] [--dump-frame=<path.png>] \
         [--dump-frame-at=<frame>] [--frame-hash] [--input=START@120,A@300,!A@310] \
         [--trace-io] [--trace-ram]"
    );
    std::process::exit(2);
}

fn parse_button(name: &str) -> Option<Button> {
    Some(match name.to_ascii_uppercase().as_str() {
        "A" => Button::A,
        "B" => Button::B,
        "START" => Button::Start,
        "SELECT" => Button::Select,
        "UP" => Button::Up,
        "DOWN" => Button::Down,
        "LEFT" => Button::Left,
        "RIGHT" => Button::Right,
        "L" => Button::L,
        "R" => Button::R,
        _ => return None,
    })
}

fn parse_inputs(spec: &str) -> Vec<(u64, Button, bool)> {
    let mut out = Vec::new();
    for item in spec.split(',').filter(|s| !s.is_empty()) {
        let (name, frame) = match item.split_once('@') {
            Some(p) => p,
            None => {
                eprintln!("bad input item {item:?}: expected NAME@FRAME");
                usage();
            }
        };
        let (name, pressed) = match name.strip_prefix('!') {
            Some(n) => (n, false),
            None => (name, true),
        };
        let Some(button) = parse_button(name) else {
            eprintln!("unknown button {name:?}");
            usage();
        };
        let Ok(frame) = frame.parse::<u64>() else {
            eprintln!("bad frame number in {item:?}");
            usage();
        };
        out.push((frame, button, pressed));
    }
    out.sort_by_key(|(f, _, _)| *f);
    out
}

fn parse_args() -> Options {
    let args: Vec<String> = std::env::args().collect();
    let mut positional = Vec::new();
    let mut o = Options {
        rom: String::new(),
        total_frames: 300,
        interval: 50,
        trace_steps: 0,
        bios: None,
        cold: false,
        capture_pc: None,
        dump_iwram: None,
        dump_vram: None,
        dump_frame: None,
        dump_frame_at: None,
        frame_hash: false,
        inputs: Vec::new(),
        trace_io: false,
        trace_ram: false,
    };
    for a in &args[1..] {
        if a == "--cold" {
            o.cold = true;
        } else if a == "--trace-io" {
            o.trace_io = true;
        } else if a == "--trace-ram" {
            o.trace_ram = true;
        } else if a == "--frame-hash" {
            o.frame_hash = true;
        } else if let Some(v) = a.strip_prefix("--bios=") {
            o.bios = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--capture-pc=") {
            o.capture_pc = u32::from_str_radix(v.trim_start_matches("0x"), 16).ok();
        } else if let Some(v) = a.strip_prefix("--dump-iwram=") {
            o.dump_iwram = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--dump-vram=") {
            o.dump_vram = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--dump-frame=") {
            o.dump_frame = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--dump-frame-at=") {
            o.dump_frame_at = v.parse().ok();
        } else if let Some(v) = a.strip_prefix("--input=") {
            o.inputs = parse_inputs(v);
        } else if a.starts_with("--") {
            eprintln!("unknown option {a}");
            usage();
        } else {
            positional.push(a.clone());
        }
    }
    if positional.is_empty() {
        usage();
    }
    o.rom = positional[0].clone();
    o.total_frames = positional
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    o.interval = positional.get(2).and_then(|s| s.parse().ok()).unwrap_or(50);
    o.trace_steps = positional.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    o
}

/// FNV-1a over the RGB framebuffer.
fn frame_hash(gba: &Gba) -> u32 {
    let frame = gba.frame();
    let bytes = frame.rgb.as_deref().unwrap_or(&frame.shades);
    let mut h: u32 = 0x811C_9DC5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Write the framebuffer as a PNG (`.png` extension) or binary PPM (P6).
fn dump_frame(gba: &Gba, path: &str) {
    let frame = gba.frame();
    let Some(rgb) = frame.rgb else {
        eprintln!("no RGB framebuffer to dump");
        return;
    };
    let out = if path.ends_with(".png") {
        encode_png(frame.width as u32, frame.height as u32, &rgb)
    } else {
        let mut out = format!("P6\n{} {}\n255\n", frame.width, frame.height).into_bytes();
        out.extend_from_slice(&rgb);
        out
    };
    match std::fs::write(path, &out) {
        Ok(()) => println!("dumped frame to {path}"),
        Err(e) => eprintln!("failed to write {path}: {e}"),
    }
}

/// Minimal PNG encoder (8-bit RGB, no filtering, stored deflate blocks).
fn encode_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }

    let stride = width as usize * 3;
    let mut raw = Vec::with_capacity(height as usize * (stride + 1));
    for row in rgb.chunks(stride) {
        raw.push(0); // filter type: none
        raw.extend_from_slice(row);
    }
    // zlib stream: header, stored blocks of at most 65535 bytes, Adler-32.
    let mut zlib = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, block) in blocks.iter().enumerate() {
        zlib.push((i + 1 == blocks.len()) as u8);
        let len = block.len() as u16;
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Write the preserved top of IWRAM (0x03007E00-0x03007FFF) to `path`.
fn dump_top_iwram(gba: &mut Gba, path: Option<&str>) {
    if let Some(p) = path {
        let mut out = Vec::with_capacity(0x200);
        for a in (0x0300_7E00..0x0300_8000).step_by(4) {
            let w = gba.peek32(a);
            out.extend_from_slice(&w.to_le_bytes());
        }
        match std::fs::write(p, &out) {
            Ok(()) => println!("dumped 0x200 bytes of top IWRAM to {p}"),
            Err(e) => eprintln!("failed to write {p}: {e}"),
        }
    }
}

fn dump_capture(gba: &mut Gba, target: u32, steps: u64) {
    let r = gba.regs();
    println!("reached pc=0x{target:08X} after {steps} steps");
    println!(
        "  r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X}",
        r[0], r[1], r[2], r[3]
    );
    println!(
        "  r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X}",
        r[4], r[5], r[6], r[7]
    );
    println!(
        "  r8=0x{:08X} r9=0x{:08X} r10=0x{:08X} r11=0x{:08X}",
        r[8], r[9], r[10], r[11]
    );
    println!(
        "  r12=0x{:08X} sp=0x{:08X} lr=0x{:08X}",
        r[12], r[13], r[14]
    );
    for i in (0..64).step_by(4) {
        let addr = target.wrapping_add(i);
        let w = gba.peek32(addr);
        println!("  0x{addr:08X}: 0x{w:08X}");
    }
    let sp = r[13];
    for i in (0..64).step_by(4) {
        let addr = sp.wrapping_add(i);
        println!("  [SP+0x{i:02X}] 0x{addr:08X} = 0x{:08X}", gba.peek32(addr));
    }
}

fn pc_is_valid(pc: u32, real_bios: bool) -> bool {
    let bios_ok = if real_bios {
        (0x0000_0000..=0x0000_3FFF).contains(&pc)
    } else {
        // Skip-BIOS: only the IRQ return stub is valid in BIOS space.
        (0x0000_0020..=0x0000_0027).contains(&pc)
    };
    bios_ok
        || matches!(
            pc,
            0x0200_0000..=0x0203_FFFF
                | 0x0300_0000..=0x0300_7FFF
                | 0x0800_0000..=0x09FF_FFFF
                | 0x0A00_0000..=0x0BFF_FFFF
                | 0x0C00_0000..=0x0DFF_FFFF
        )
}

fn main() {
    let o = parse_args();

    let rom = std::fs::read(&o.rom).unwrap_or_else(|e| {
        eprintln!("failed to read {}: {e}", o.rom);
        std::process::exit(1);
    });

    let mut gba = match &o.bios {
        Some(p) => {
            let bios = std::fs::read(p).unwrap_or_else(|e| {
                eprintln!("failed to read BIOS {p}: {e}");
                std::process::exit(1);
            });
            println!("booting with real BIOS ({} bytes)", bios.len());
            Gba::with_bios(rom, bios, o.cold)
        }
        None => Gba::new(rom),
    };

    if let Some(target) = o.capture_pc {
        let mut steps: u64 = 0;
        loop {
            if gba.pc() == target {
                dump_capture(&mut gba, target, steps);
                dump_top_iwram(&mut gba, o.dump_iwram.as_deref());
                return;
            }
            gba.step();
            steps += 1;
            if steps > 200_000_000 {
                println!("capture target 0x{target:08X} not reached after {steps} steps");
                dump_top_iwram(&mut gba, o.dump_iwram.as_deref());
                return;
            }
        }
    }

    let frame_cycles = gba.frame_cycles();
    let start = Instant::now();
    let mut cycles: u64 = 0;
    let mut target = frame_cycles as u64;
    let mut prev_frame: u64 = 0;
    let mut traced: u32 = 0;
    let mut trace_pc: u32 = 0;
    let mut total_steps: u64 = 0;
    let mut next_input = 0usize;
    gba.bus.io.log_writes = o.trace_io;
    gba.bus.log_ram_writes = o.trace_ram;

    // Ring buffer to catch the last N instructions before a wild PC.
    const RING_SIZE: usize = 128;
    let mut ring: Vec<String> = Vec::with_capacity(RING_SIZE);
    let mut ring_idx: usize = 0;
    let mut wild_detected = false;

    loop {
        let pc = gba.pc();
        let cpsr = gba.cpu.cpsr();
        let thumb = cpsr & (1 << 5) != 0;
        let mode = cpsr & 0x1F;
        let i_flag = (cpsr >> 7) & 1;

        if traced < o.trace_steps {
            trace_pc = pc;
            let r = gba.regs();
            let sp = gba.sp();
            let f_flag = (cpsr >> 6) & 1;
            let op_str = if thumb {
                format!("0x{:04X}", gba.peek16(pc))
            } else {
                format!("0x{:08X}", gba.peek32(pc))
            };
            println!(
                "step {traced:>5}  pc=0x{pc:08X}  op={op_str}  T={t} sp=0x{sp:08X}  cpsr_i={i_flag} \
                 cpsr_f={f_flag} mode=0x{mode:02X}  r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} \
                 r3=0x{:08X} r4=0x{:08X} r7=0x{:08X} r12=0x{:08X} lr=0x{:08X}",
                r[0],
                r[1],
                r[2],
                r[3],
                r[4],
                r[7],
                r[12],
                r[14],
                t = thumb as u8
            );
            traced += 1;
        }

        if !wild_detected {
            let r = gba.regs();
            let sp = gba.sp();
            let op_str = if thumb {
                format!("0x{:04X}", gba.peek16(pc))
            } else {
                format!("0x{:08X}", gba.peek32(pc))
            };
            let entry = format!(
                "step {total_steps:>8}  pc=0x{pc:08X}  op={op_str}  T={t} sp=0x{sp:08X}  i={i_flag} \
                 mode=0x{mode:02X}  r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} lr=0x{:08X}",
                r[0],
                r[1],
                r[2],
                r[3],
                r[14],
                t = thumb as u8
            );
            if ring.len() < RING_SIZE {
                ring.push(entry);
            } else {
                ring[ring_idx] = entry;
            }
            ring_idx = (ring_idx + 1) % RING_SIZE;
        }

        cycles += gba.step() as u64;
        total_steps += 1;

        if !wild_detected {
            let new_pc = gba.pc();
            if !pc_is_valid(new_pc, o.bios.is_some()) {
                wild_detected = true;
                println!("\n*** WILD PC DETECTED: 0x{new_pc:08X} at step {total_steps} ***");
                println!("--- last {} instructions ---", ring.len());
                for i in 0..ring.len() {
                    let idx = (ring_idx + i) % ring.len();
                    println!("  {}", ring[idx]);
                }
                println!("--- end ring ---\n");
            }
        }
        if o.trace_io && !gba.bus.io.write_log.is_empty() {
            for (off, w, val) in std::mem::take(&mut gba.bus.io.write_log) {
                println!(
                    "  io@step {traced:>5} pc=0x{trace_pc:08X}  wr{w} 0x{off:03X} = 0x{val:04X}"
                );
            }
        }
        if o.trace_ram && !gba.bus.ram_write_log.is_empty() {
            for (addr, w, val) in std::mem::take(&mut gba.bus.ram_write_log) {
                if (0x0300_2700..0x0300_2760).contains(&addr)
                    || (0x0600_0000..0x0700_0000).contains(&addr)
                {
                    println!(
                        "  ram@step {total_steps:>8} pc=0x{pc:08X}  wr{w} 0x{addr:08X} = 0x{val:08X}"
                    );
                }
            }
        }
        let frame = gba.frames();
        if frame > prev_frame {
            prev_frame = frame;
            while next_input < o.inputs.len() && o.inputs[next_input].0 <= frame {
                let (_, button, pressed) = o.inputs[next_input];
                if pressed {
                    gba.press(button);
                } else {
                    gba.release(button);
                }
                next_input += 1;
            }
            if frame % o.interval == 0 || frame == o.total_frames {
                let (ime, ie, ifl, hptr) = gba.irq_debug();
                let hash = if o.frame_hash {
                    format!("  hash=0x{:08X}", frame_hash(&gba))
                } else {
                    String::new()
                };
                println!(
                    "frame {frame:>4}  pc=0x{:08X}  dispcnt=0x{:04X}  halted={}  bios_wait={:?}  \
                     unknown_swi={:?}  ime={} ie=0x{:04X} if=0x{:04X} handler=0x{:08X}{hash}",
                    gba.pc(),
                    gba.dispcnt(),
                    gba.halted(),
                    gba.cpu.bios_wait_mask(),
                    gba.last_unknown_swi.map(|n| format!("0x{n:02X}")),
                    ime,
                    ie,
                    ifl,
                    hptr,
                );
            }
            if o.dump_frame_at == Some(frame) {
                if let Some(p) = &o.dump_frame {
                    dump_frame(&gba, p);
                }
            }
            if frame >= o.total_frames {
                break;
            }
            target = (frame + 1) * frame_cycles as u64;
        }
        if cycles >= target + frame_cycles as u64 * 100 {
            eprintln!("warning: no frame boundary reached; pc stuck");
            break;
        }
        if start.elapsed().as_secs() > 120 {
            eprintln!("warning: 120s timeout hit at frame {frame}");
            break;
        }
    }

    println!(
        "done: {} frames in {:.2}s ({:.0} fps)",
        prev_frame,
        start.elapsed().as_secs_f64(),
        prev_frame as f64 / start.elapsed().as_secs_f64()
    );
    if o.dump_frame_at.is_none() {
        if let Some(p) = &o.dump_frame {
            dump_frame(&gba, p);
        }
    }
    if let Some(p) = &o.dump_vram {
        // Palette RAM (1 KB), VRAM (96 KB) and OAM (1 KB), back to back.
        let mut out = Vec::with_capacity(0x18800);
        out.extend_from_slice(gba.bus.palram.as_ref());
        out.extend_from_slice(gba.bus.vram.as_ref());
        out.extend_from_slice(gba.bus.oam.as_ref());
        match std::fs::write(p, &out) {
            Ok(()) => println!("dumped palette, vram and oam to {p}"),
            Err(e) => eprintln!("failed to write {p}: {e}"),
        }
    }
    dump_top_iwram(&mut gba, o.dump_iwram.as_deref());
}
