use gb_core::cartridge::Cartridge;
use gb_core::gb::{Gb, FRAME_CYCLES};
use gb_core::joypad::*;
use std::process::ExitCode;

/// Parse a script like "START@3000,RELEASE@3020;A@4000" into (frame, action, button).
fn parse_input_script(s: &str) -> Vec<(u32, bool, u8)> {
    let mut out = Vec::new();
    for ev in s.split([';', ',']) {
        let ev = ev.trim();
        if ev.is_empty() {
            continue;
        }
        let (name, frame) = ev.rsplit_once('@').expect("GB_INPUT event needs NAME@FRAME");
        let frame: u32 = frame.trim().parse().expect("bad frame");
        let (press, btn) = if let Some(n) = name.strip_prefix("RELEASE") {
            (false, n.trim())
        } else {
            (true, name.trim())
        };
        let button = if btn.is_empty() {
            0 // sentinel: release all held
        } else {
            match btn.to_uppercase().as_str() {
                "A" => BUTTON_A,
                "B" => BUTTON_B,
                "SELECT" => BUTTON_SELECT,
                "START" => BUTTON_START,
                "RIGHT" => BUTTON_RIGHT,
                "LEFT" => BUTTON_LEFT,
                "UP" => BUTTON_UP,
                "DOWN" => BUTTON_DOWN,
                _ => panic!("unknown button {btn}"),
            }
        };
        out.push((frame, press, button));
    }
    out
}


fn shade_to_rgb(shade: u8) -> (u8, u8, u8) {
    match shade & 3 {
        0 => (0xE0, 0xF8, 0xD0),
        1 => (0x88, 0xC0, 0x70),
        2 => (0x34, 0x68, 0x56),
        _ => (0x08, 0x18, 0x20),
    }
}

fn dump_ppm(emu: &Gb, path: &str) {
    let frame = emu.framebuffer();
    let mut s = String::new();
    s.push_str("P3\n160 144\n255\n");
    for y in 0..144usize {
        for x in 0..160usize {
            let (r, g, b) = shade_to_rgb(frame[y * 160 + x]);
            s.push_str(&format!("{r} {g} {b} "));
        }
        s.push('\n');
    }
    std::fs::write(path, s).expect("write ppm");
}

fn main() -> ExitCode {
    let rom = std::env::args()
        .nth(1)
        .expect("usage: test_runner <path-to-test-rom.gb>");
    let data = std::fs::read(&rom).expect("failed to read ROM");
    let cart = Cartridge::load(&data).expect("failed to load cartridge");
    let mut emu = Gb::new(cart);

    let dump_path = std::env::var("GB_DUMP").ok();
    let dump_frame: Option<u32> = std::env::var("GB_DUMP_FRAME")
        .ok()
        .and_then(|v| v.parse().ok());
    let oam_frame: Option<u32> = std::env::var("GB_OAM")
        .ok()
        .and_then(|v| v.parse().ok());
    let cap: u32 = std::env::var("GB_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60 * 120);
    let input_script = std::env::var("GB_INPUT")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| parse_input_script(&s))
        .unwrap_or_default();

    let max_frames: u32 = cap;
    let debug = std::env::var("GB_DEBUG").is_ok();
    let mut held: Vec<u8> = Vec::new();
    for f in 0..max_frames {
        if debug && f % 240 == 0 {
            eprintln!(
                "dbg f={} pc={:04X} IE={:02X} IF={:02X} ime={} halted={} joy={:02X} P1={:02X} LY={}",
                f,
                emu.cpu.pc,
                emu.bus.ie,
                emu.bus.io[0x0F],
                emu.cpu.ime,
                emu.cpu.halted,
                emu.bus.joypad.state,
                emu.bus.io[0x00],
                emu.bus.io[0x44]
            );
        }
        for &(frame, press, button) in &input_script {
            if frame == f {
                if press {
                    emu.press_button(button);
                    held.push(button);
                } else if button == 0 {
                    for &b in &held {
                        emu.release_button(b);
                    }
                    held.clear();
                } else {
                    emu.release_button(button);
                    held.retain(|&b| b != button);
                }
            }
        }
        let mut cycles = 0u32;
        while cycles < FRAME_CYCLES {
            cycles += emu.step();
        }
        if let Some(p) = &dump_path {
            if let Some(frame) = dump_frame {
                if f == frame {
                    dump_ppm(&emu, p);
                }
            }
        }
        let s = String::from_utf8_lossy(&emu.bus.serial_buf);
        if s.contains("Passed") {
            println!("PASSED: {s}");
            return ExitCode::SUCCESS;
        }
        if s.contains("Failed") {
            println!("FAILED: {s}");
            return ExitCode::FAILURE;
        }
        if s.contains("fail") || s.contains("Fail") {
            println!("FAILED: {s}");
            return ExitCode::FAILURE;
        }
    }
    let s = String::from_utf8_lossy(&emu.bus.serial_buf);
    eprintln!("TIMEOUT (no result after 120s of frames)");
    eprintln!("serial so far: {s}");
    let mut hist = [0u32; 4];
    for &sh in emu.framebuffer() {
        hist[(sh & 3) as usize] += 1;
    }
    eprintln!(
        "ppu: LCDC={:02X} LY={} STAT={:02X} mode={} vblank_interrupts={} shades(0..3)={:?}",
        emu.bus.io[0x40],
        emu.bus.io[0x44],
        emu.bus.io[0x41],
        emu.bus.ppu.mode,
        emu.bus.ppu.vblank_interrupts,
        hist
    );
    eprintln!(
        "timer: TIMA={:02X} TMA={:02X} TAC={:02X} timer_interrupts={} frames={}",
        emu.bus.io[0x05], emu.bus.io[0x06], emu.bus.io[0x07], emu.cpu.timer_interrupts, cap
    );
    if oam_frame.is_some() {
        let mut n = 0;
        for i in 0..40 {
            let sy = emu.bus.oam[i * 4];
            let sx = emu.bus.oam[i * 4 + 1];
            let tile = emu.bus.oam[i * 4 + 2];
            let attr = emu.bus.oam[i * 4 + 3];
            if sx != 0 {
                eprintln!("OAM[{i:02}] sy={sy:03} sx={sx:03} tile={tile:02X} attr={attr:02X}");
                n += 1;
            }
        }
        eprintln!("OAM nonzero entries: {n}  LCDC={:02X}", emu.bus.io[0x40]);
        eprintln!("line_sprites count={}", emu.bus.ppu.line_sprite_count);
        for s in emu.bus.ppu
            .line_sprites
            .iter()
            .take(emu.bus.ppu.line_sprite_count)
        {
            eprintln!("  spr x={} y={} tile={:02X} attr={:02X} h={}", s.x, s.y, s.tile, s.attr, s.height);
        }
    }
    ExitCode::FAILURE
}