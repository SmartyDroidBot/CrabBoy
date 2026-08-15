//! GBA system: the composition root.
//!
//! [`Gba`] owns the CPU, bus, PPU and APU and implements [`emu_core::System`],
//! presenting a uniform, platform-agnostic interface to frontends. It drives
//! the per-frame timing: each `step` runs one CPU instruction and then advances
//! the timers, APU and PPU scanlines by the cycles consumed, firing the
//! appropriate interrupts and DMA triggers.

use crate::apu::Apu;
use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::io::irq;
use crate::ppu::Ppu;
use crate::{dma::Timing, ppu};

/// Total master cycles per video frame.
pub const FRAME_CYCLES: u32 = ppu::FRAME_CYCLES;
/// Display width / height.
pub const SCREEN_W: u16 = ppu::SCREEN_W as u16;
pub const SCREEN_H: u16 = ppu::SCREEN_H as u16;

/// The GBA system.
pub struct Gba {
    pub cpu: Cpu,
    pub bus: Bus,
    pub ppu: Ppu,
    pub apu: Apu,
    /// Cycles consumed so far in the current line.
    line_cycles: u32,
    /// Current scanline (0..227).
    line: u32,
    /// Frames completed (for save states / info).
    frame_count: u64,
}

impl Gba {
    pub fn new(rom: Vec<u8>) -> Gba {
        let mut cpu = Cpu::new();
        // Power-on state: the CPU starts in SVC mode at the BIOS reset vector.
        // Without a BIOS dump we jump straight to the cartridge header (which
        // copies itself to IWRAM via the fixed BIOS `CpuSet`/`Copy` SWIs); to
        // let un-BIOS'd games boot we leave PC at 0x0000 and rely on the game's
        // entry. Most games start by branching to their entry point from 0.
        cpu.set_pc(0);
        let bus = Bus::new(rom);
        Gba { cpu, bus, ppu: Ppu::new(), apu: Apu::new(), line_cycles: 0, line: 0, frame_count: 0 }
    }

    /// Construct a `Box<dyn System>` from ROM bytes (frontend convenience).
    pub fn system(rom: Vec<u8>) -> Box<dyn emu_core::System> {
        Box::new(Gba::new(rom))
    }

    /// Advance the machine by `cycles` (timers, APU, PPU scanlines, DMA).
    fn advance(&mut self, cycles: u32) {
        self.bus.timers.step(cycles);
        self.apu.step(cycles);
        self.line_cycles += cycles;
        while self.line_cycles >= ppu::CYCLES_PER_LINE {
            self.line_cycles -= ppu::CYCLES_PER_LINE;
            self.tick_line();
        }
    }

    /// Advance one scanline: update VCOUNT/DISPSTAT, render, raise IRQs and
    /// run VBlank/HBlank DMA.
    fn tick_line(&mut self) {
        self.line += 1;
        if self.line >= ppu::LINES_PER_FRAME {
            self.line = 0;
            self.frame_count += 1;
        }
        let y = self.line;
        self.bus.io.set_vcount(y as u16);

        // DISPSTAT at 0x04: low byte = flags + IRQ enables; high byte = VCOUNT
        // setting.
        let irq_en = self.bus.io.regs[0x05] & 0x07;
        let vcount_setting = self.bus.io.regs[0x05] as u32;
        let in_vblank = y >= ppu::VISIBLE_LINES;
        let vblank_flag = if in_vblank { 1 } else { 0 };

        // Render the visible line.
        if y < ppu::VISIBLE_LINES {
            self.ppu.render_scanline(&self.bus, y);
        }

        // VBlank transition.
        if y == ppu::VISIBLE_LINES {
            self.bus.run_dma(Timing::VBlank);
            if irq_en & 0x01 != 0 {
                self.bus.io.raise_irq(irq::VBLANK);
            }
        }
        // VCOUNT match.
        if y == vcount_setting && irq_en & 0x04 != 0 {
            self.bus.io.raise_irq(irq::VCOUNT);
        }
        // HBlank DMA + IRQ each line.
        self.bus.run_dma(Timing::HBlank);
        if irq_en & 0x02 != 0 {
            self.bus.io.raise_irq(irq::HBLANK);
        }

        // Write the DISPSTAT flags byte (VBlank + HBlank bits).
        self.bus.io.regs[0x04] = vblank_flag | (1 << 1);
    }

    fn press_button(&mut self, button: emu_core::Button) {
        if let Some(bit) = Self::button_to_bit(button) {
            self.bus.press(bit);
        }
    }

    fn release_button(&mut self, button: emu_core::Button) {
        if let Some(bit) = Self::button_to_bit(button) {
            self.bus.release(bit);
        }
    }

    fn button_to_bit(button: emu_core::Button) -> Option<u16> {
        use crate::io::key;
        Some(match button {
            emu_core::Button::A => key::A,
            emu_core::Button::B => key::B,
            emu_core::Button::Select => key::SELECT,
            emu_core::Button::Start => key::START,
            emu_core::Button::Right => key::RIGHT,
            emu_core::Button::Left => key::LEFT,
            emu_core::Button::Up => key::UP,
            emu_core::Button::Down => key::DOWN,
            emu_core::Button::R => key::R,
            emu_core::Button::L => key::L,
            // GBA has no X/Y.
            emu_core::Button::X | emu_core::Button::Y => return None,
        })
    }

    /// Execute one instruction (or idle cycle), returning cycles consumed.
    pub fn step(&mut self) -> u32 {
        self.bus.sync_dev_irq();

        if self.cpu.halted {
            if self.bus.pending_irq() != 0 {
                self.cpu.halted = false;
            } else {
                self.advance(4);
                return 4;
            }
        }

        let pending = self.bus.pending_irq();
        if pending != 0 && !self.cpu.irq_masked() {
            let lr = self.cpu.pc();
            self.cpu.irq(lr);
            self.advance(4);
            return 4;
        }

        self.bus.begin_step();
        let instr = self.cpu.execute(&mut self.bus);
        let total = instr + self.bus.cycles();
        // Immediate DMA fires as soon as its channel is enabled.
        self.bus.run_dma(Timing::Immediate);
        self.advance(total);
        total
    }
}

impl emu_core::System for Gba {
    fn name(&self) -> &'static str {
        "gba"
    }

    fn info(&self) -> String {
        let title = self
            .bus
            .rom
            .get(0xA0..0xB0)
            .map(|s| {
                String::from_utf8_lossy(s)
                    .trim_end_matches('\0')
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();
        format!("{} (GBA)", if title.is_empty() { "unknown" } else { &title })
    }

    fn reset(&mut self) {
        let rom = self.bus.rom.clone();
        *self = Gba::new(rom);
    }

    fn press(&mut self, button: emu_core::Button) {
        self.press_button(button);
    }

    fn release(&mut self, button: emu_core::Button) {
        self.release_button(button);
    }

    fn step(&mut self) -> u32 {
        Gba::step(self)
    }

    fn frame_cycles(&self) -> u32 {
        FRAME_CYCLES
    }

    fn frame(&self) -> emu_core::Frame {
        let w = SCREEN_W as usize;
        let h = SCREEN_H as usize;
        let mut rgb = vec![0u8; w * h * 3];
        for (i, &c) in self.ppu.framebuffer.iter().enumerate() {
            // BGR555: R bits 0-4, G bits 5-9, B bits 10-14.
            let r = ((c & 0x1F) as u32) * 255 / 31;
            let g = (((c >> 5) & 0x1F) as u32) * 255 / 31;
            let b = (((c >> 10) & 0x1F) as u32) * 255 / 31;
            rgb[i * 3] = r as u8;
            rgb[i * 3 + 1] = g as u8;
            rgb[i * 3 + 2] = b as u8;
        }
        emu_core::Frame { width: SCREEN_W, height: SCREEN_H, shades: vec![], rgb: Some(rgb) }
    }

    fn audio_rate(&self) -> u32 {
        32768
    }

    fn take_audio(&mut self) -> emu_core::audio::AudioBuffer {
        self.apu.take_audio()
    }

    fn battery_backed(&self) -> bool {
        false
    }

    fn save_data(&self) -> Vec<u8> {
        Vec::new()
    }

    fn load_data(&mut self, _data: &[u8]) {}

    fn rtc_data(&self) -> Vec<u8> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::System;

    #[test]
    fn boots_and_renders_a_frame() {
        // A tiny ROM that loops forever (B to itself in Thumb).
        let mut rom = vec![0u8; 0x4000];
        // Branch to self: 0xE7FE = B -2 (relative to the fetch address).
        rom[0x0000] = 0xFE;
        rom[0x0001] = 0xE7;
        let mut gba = Gba::new(rom);
        // Enable IRQ-less execution; just run a frame.
        gba.run_frame();
        let f = gba.frame();
        assert_eq!(f.width, 240);
        assert_eq!(f.height, 160);
        assert_eq!(f.rgb.as_ref().unwrap().len(), 240 * 160 * 3);
    }

    #[test]
    fn audio_rate_is_32768() {
        let gba = Gba::new(vec![0; 0x4000]);
        assert_eq!(gba.audio_rate(), 32768);
    }
}