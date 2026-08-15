use gb_core::cartridge::Cartridge;
use gb_core::gb::{Gb, FRAME_CYCLES};
use gb_core::joypad::*;

fn main() {
    let rom = std::env::args().nth(1).expect("rom");
    let press_f: u32 = std::env::args().nth(2).and_then(|v| v.parse().ok()).unwrap_or(400);
    let end_f: u32 = std::env::args().nth(3).and_then(|v| v.parse().ok()).unwrap_or(560);
    let dfrom: u32 = std::env::args().nth(4).and_then(|v| v.parse().ok()).unwrap_or(0);
    let force_f9: i64 = std::env::args().nth(5).and_then(|v| v.parse().ok()).unwrap_or(-1);

    let data = std::fs::read(&rom).expect("read rom");
    let cart = Cartridge::load(&data).expect("load cart");
    let mut emu = Gb::new(cart);

    let mut last_vblank = emu.bus.ppu.vblank_interrupts;
    let mut cyc: u64 = 0;
    let mut stamps: Vec<u64> = Vec::new();
    let mut held = false;
    for f in 0..end_f {
        if f == press_f {
            emu.press_button(BUTTON_START);
            held = true;
        }
        if f == press_f + 1 && held {
            emu.release_button(BUTTON_START);
            held = false;
        }
        let mut cycles = 0u32;
        while cycles < FRAME_CYCLES {
            let c = emu.step();
            cycles += c;
            if force_f9 >= 0 && f as i64 >= force_f9 {
                emu.bus.write(0xFFF9, 1);
            }
            cyc += c as u64;
            if emu.bus.ppu.vblank_interrupts != last_vblank {
                stamps.push(cyc);
                last_vblank = emu.bus.ppu.vblank_interrupts;
            }
        }
        if f < dfrom {
            continue;
        }
        let f8 = emu.bus.read(0xFFF8);
        let f9 = emu.bus.read(0xFFF9);
        let b5 = emu.bus.read(0xFFB5);
        let lcdc = emu.bus.read(0xFF40);
        let ly = emu.bus.read(0xFF44);
        println!(
            "f={} FFF8={:02X} FFF9={:02X} FFB5={:02X} LCDC={:02X} LY={} vblank={}",
            f, f8, f9, b5, lcdc, ly, emu.bus.ppu.vblank_interrupts
        );
    }
    eprintln!("=== vblank gap samples: {} ===", stamps.len().saturating_sub(1));
    for (i, w) in stamps.windows(2).enumerate().take(20) {
        eprintln!("gap[{}] = {} cycles", i, w[1] - w[0]);
    }
    if stamps.len() > 2 {
        eprintln!("last gaps: ...");
        for (i, w) in stamps.windows(2).enumerate().rev().take(6).rev() {
            eprintln!("gap[{}] = {} cycles", i, w[1] - w[0]);
        }
    }
}