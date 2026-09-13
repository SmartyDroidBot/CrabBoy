//! GBA system: the composition root.
//!
//! [`Gba`] owns the CPU, bus, PPU and APU and implements [`emu_core::System`],
//! presenting a uniform, platform-agnostic interface to frontends. It drives
//! the per-frame timing: each `step` runs one CPU instruction and then advances
//! the timers, APU and PPU scanlines by the cycles consumed, firing the
//! appropriate interrupts and DMA triggers.

use crate::apu::Apu;
use crate::bus::Bus;
use crate::cpu::flag;
use crate::cpu::mode;
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
    pub(crate) line_cycles: u32,
    /// Current scanline (0..227).
    pub(crate) line: u32,
    /// Frames completed (for save states / info).
    pub(crate) frame_count: u64,
    /// The first unrecognised BIOS SWI executed (diagnostics).
    pub last_unknown_swi: Option<u32>,
}

impl Gba {
    pub fn new(rom: Vec<u8>) -> Gba {
        let mut cpu = Cpu::new();
        // Power-on state. The real GBA starts in SVC mode at the BIOS reset
        // vector, which copies the cart header to IWRAM and branches to the
        // cartridge entry point. Without a BIOS dump we skip straight to the
        // cartridge at 0x08000000 (the ROM's first word is its entry branch)
        // and give it the stack pointer the BIOS would have set up. The real
        // BIOS enables IRQs before handing control to the game, so clear the
        // CPSR I-flag (the hardware reset sets it).
        cpu.set_cpsr(cpu.cpsr() & !flag::I);
        cpu.set_pc(0x0800_0000);
        cpu.set_reg(13, 0x0300_7F00); // SP_svc (current mode is SVC)
        cpu.set_mode_sp(mode::IRQ, 0x0300_7FA0); // SP_irq (BIOS default)
        let bus = Bus::new(rom);
        Gba {
            cpu,
            bus,
            ppu: Ppu::new(),
            apu: Apu::new(),
            line_cycles: 0,
            line: 0,
            frame_count: 0,
            last_unknown_swi: None,
        }
    }

    /// Construct a `Box<dyn System>` from ROM bytes (frontend convenience).
    pub fn system(rom: Vec<u8>) -> Box<dyn emu_core::System> {
        Box::new(Gba::new(rom))
    }

    /// Construct a system that boots a real BIOS dump from the reset vector
    /// (`0x00000000`). The BIOS runs its POST/intro and launches the cartridge,
    /// setting up the stack pointers and boot RAM that a skip-BIOS start omits.
    ///
    /// When `cold` is true the BIOS performs a full cold boot (logo intro);
    /// when false it sets POSTFLG for a warm boot that skips the intro.
    pub fn with_bios(rom: Vec<u8>, bios: Vec<u8>, cold: bool) -> Gba {
        let mut gba = Gba::new(rom);
        gba.cpu.set_has_bios(true);
        gba.bus.set_bios(bios);
        if !cold {
            // POSTFLG=1 requests a warm boot, skipping the ~2s "Nintendo" logo.
            gba.bus.io.regs[0x300] = 1;
        }
        // The real GBA powers on in SVC mode at the BIOS reset vector.
        gba.cpu.set_pc(0x0000_0000);
        gba
    }

    /// Advance the machine by `cycles` (timers, APU, PPU scanlines, DMA).
    fn advance(&mut self, cycles: u32) {
        self.bus.timers.step(cycles);
        for i in 0..4 {
            if self.bus.timers.just_overflowed(i) {
                self.apu.timer_overflow(i as u8);
                self.check_dma_fifo(i);
            }
        }
        self.apu.step(cycles);
        self.line_cycles += cycles;
        while self.line_cycles >= ppu::CYCLES_PER_LINE {
            self.line_cycles -= ppu::CYCLES_PER_LINE;
            self.tick_line();
        }
    }

    fn check_dma_fifo(&mut self, timer_idx: usize) {
        let cnt_h = self.bus.io.read16(0x82);
        let dsa_timer = ((cnt_h >> 10) & 1) as usize;
        let dsb_timer = ((cnt_h >> 14) & 1) as usize;

        // DMA1 feeds FIFO A if dsa_timer matches and FIFO A has <= 16 bytes
        if dsa_timer == timer_idx && self.apu.fifo_a_count() <= 16 {
            let mut ch = self.bus.dma.chans[1];
            if ch.enabled && ch.timing() == Timing::Special {
                let s = ch.src;
                for _ in 0..4 {
                    let w = self.bus.read32(s);
                    self.apu.push_fifo_a(w as u8);
                    self.apu.push_fifo_a((w >> 8) as u8);
                    self.apu.push_fifo_a((w >> 16) as u8);
                    self.apu.push_fifo_a((w >> 24) as u8);
                }
                ch.src = crate::dma::adjust(s, 16, ch.src_adjust());
                if ch.irq_enable() {
                    self.bus.dma.flags |= 1 << (8 + 1);
                }
                self.bus.dma.chans[1] = ch;
            }
        }

        // DMA2 feeds FIFO B if dsb_timer matches and FIFO B has <= 16 bytes
        if dsb_timer == timer_idx && self.apu.fifo_b_count() <= 16 {
            let mut ch = self.bus.dma.chans[2];
            if ch.enabled && ch.timing() == Timing::Special {
                let s = ch.src;
                for _ in 0..4 {
                    let w = self.bus.read32(s);
                    self.apu.push_fifo_b(w as u8);
                    self.apu.push_fifo_b((w >> 8) as u8);
                    self.apu.push_fifo_b((w >> 16) as u8);
                    self.apu.push_fifo_b((w >> 24) as u8);
                }
                ch.src = crate::dma::adjust(s, 16, ch.src_adjust());
                if ch.irq_enable() {
                    self.bus.dma.flags |= 1 << (8 + 2);
                }
                self.bus.dma.chans[2] = ch;
            }
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
        let irq_en = (self.bus.io.regs[0x04] >> 3) & 0x07;
        let vcount_setting = self.bus.io.regs[0x05] as u32;
        let in_vblank = y >= ppu::VISIBLE_LINES;
        let vblank_flag = if in_vblank { 1 } else { 0 };

        // Render the visible line.
        if y < ppu::VISIBLE_LINES {
            self.ppu.render_scanline(&self.bus, y);
        }

        // VBlank transition.
        if y == ppu::VISIBLE_LINES {
            self.ppu.reload_affine_refs(&self.bus);
            self.bus.run_dma(Timing::VBlank);
            if self
                .cpu
                .bios_wait_mask()
                .is_some_and(|mask| mask & irq::VBLANK != 0)
            {
                self.cpu.complete_bios_wait();
                self.dispatch_bios_irq(irq::VBLANK);
            }
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

        // Write the DISPSTAT flags byte (VBlank + HBlank bits), preserving the
        // game-written IRQ-enable bits (3-5).
        // HBlank is set during the H-blank period of every line (visible and VBlank).
        // In our scanline model, once tick_line runs the HBlank DMA/IRQ it is the
        // H-blank period, so the bit stays asserted until the next line begins.
        let enables = self.bus.io.regs[0x04] & 0x38;
        self.bus.io.regs[0x04] = vblank_flag | (1 << 1) | enables;
    }

    /// Dispatch the game's IRQ handler as the real BIOS would: enter IRQ mode,
    /// branch to `[0x03007FFC]`. Called when a bios_wait completes.
    fn dispatch_bios_irq(&mut self, mask: u16) {
        let cur = self.bus.read32(crate::bios::BIOS_IF_ADDR);
        self.bus
            .write32(crate::bios::BIOS_IF_ADDR, cur | mask as u32);
        let handler = self.bus.read32(0x0300_7FFC);
        if handler == 0 || handler == 0xFFFF_FFFF {
            return;
        }
        let lr = self.cpu.pc();
        self.cpu.irq(lr);
        self.cpu.set_pc(handler);
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

        // HALTCNT (0x04000301) write requests CPU halt.
        if self.bus.io.halt_requested {
            self.bus.io.halt_requested = false;
            self.cpu.halted = true;
        }

        if let Some(mask) = self.cpu.bios_wait_mask() {
            if self.bus.io.iflags() & mask != 0 {
                self.bus.io.acknowledge(mask);
                self.cpu.complete_bios_wait();
                self.dispatch_bios_irq(mask);
            } else {
                self.advance(4);
                return 4;
            }
        }

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
            // Without a BIOS, dispatch the IRQ straight to the game's handler
            // (the real BIOS's `ldr pc, [pc, #-4]` at vector 0x18 jumps through
            // the handler pointer the game stores at 0x03007FFC).
            if self.bus.bios.is_empty() {
                self.cpu.set_pc(self.bus.read32(0x0300_7FFC));
            }
            self.advance(4);
            return 4;
        }

        self.bus.begin_step();
        let instr = self.cpu.execute(&mut self.bus);
        let total = instr + self.bus.cycles();
        // Dispatch a BIOS SWI (if any) now that the instruction has finished.
        if let Some(num) = self.cpu.take_bios_call() {
            if !crate::bios::run(&mut self.cpu, &mut self.bus, num) {
                // Unrecognised SWI: the real BIOS dispatcher would still handle
                // it, so no-op (return) rather than hanging on the zeroed SVC
                // vector. Record it so we know which routines to implement.
                if self.last_unknown_swi != Some(num) {
                    self.last_unknown_swi = Some(num);
                    eprintln!("unimplemented BIOS SWI 0x{num:02X}");
                }
            }
        }
        // Immediate DMA fires as soon as its channel is enabled.
        self.bus.run_dma(Timing::Immediate);
        self.advance(total);
        total
    }
    /// Current DISPCNT register value (read diagnostics).
    pub fn dispcnt(&self) -> u16 {
        self.bus.io.read16(0)
    }

    /// Frames completed (diagnostics).
    pub fn frames(&self) -> u64 {
        self.frame_count
    }

    /// Current program counter (diagnostics).
    pub fn pc(&self) -> u32 {
        self.cpu.pc()
    }

    /// Whether the CPU is halted (waiting for an IRQ).
    pub fn halted(&self) -> bool {
        self.cpu.halted
    }

    /// Read the instruction word at `pc` for tracing (diagnostics).
    pub fn peek16(&mut self, pc: u32) -> u32 {
        self.bus.read16(pc)
    }

    /// CPU registers (diagnostics).
    pub fn regs(&self) -> [u32; 16] {
        self.cpu.dump_regs()
    }

    /// Current stack pointer (SVC/USR, diagnostics).
    pub fn sp(&self) -> u32 {
        self.cpu.sp_raw()
    }

    /// Read a 32-bit word (diagnostics).
    pub fn peek32(&mut self, addr: u32) -> u32 {
        self.bus.read32(addr)
    }

    /// Number of cycles in one full frame.
    pub fn frame_cycles(&self) -> u32 {
        FRAME_CYCLES
    }

    /// IRQ/exception diagnostics: (IME, IE, IF, handler pointer at 0x03007FFC).
    pub fn irq_debug(&mut self) -> (bool, u16, u16, u32) {
        (
            self.bus.io.ime(),
            self.bus.io.ie(),
            self.bus.io.iflags(),
            self.bus.read32(0x0300_7FFC),
        )
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
        format!(
            "{} (GBA)",
            if title.is_empty() { "unknown" } else { &title }
        )
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
        emu_core::Frame {
            width: SCREEN_W,
            height: SCREEN_H,
            shades: vec![],
            rgb: Some(rgb),
        }
    }

    fn audio_rate(&self) -> u32 {
        32768
    }

    fn take_audio(&mut self) -> emu_core::audio::AudioBuffer {
        let mut buf = self.apu.take_audio();
        // Cap samples per frame to prevent surplus accumulation in the audio
        // sink, which would cause ever-growing latency.
        let max_samples = (self.audio_rate() as usize / 60 + 1) * 2;
        buf.samples.truncate(max_samples);
        buf
    }

    fn battery_backed(&self) -> bool {
        self.bus.save.battery_backed()
    }

    fn save_data(&self) -> Vec<u8> {
        self.bus.save.raw().to_vec()
    }

    fn load_data(&mut self, data: &[u8]) {
        self.bus.save.load(data);
    }

    fn sram_changed(&mut self) -> bool {
        self.bus.save.take_dirty()
    }

    fn rtc_data(&self) -> Vec<u8> {
        Vec::new()
    }

    fn save_state(&self) -> Vec<u8> {
        crate::state::save_state(self)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        crate::state::load_state(self, data)
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

    #[test]
    fn save_state_round_trip() {
        let mut gba = Gba::new(vec![0; 0x4000]);
        gba.run_frame();
        let saved = gba.save_state();
        assert_eq!(&saved[0..4], crate::state::STATE_MAGIC);
        // Run a bit further, then restore.
        gba.run_frame();
        gba.load_state(&saved).unwrap();
        let again = gba.save_state();
        assert_eq!(saved, again, "save(load(save())) must equal save()");
    }

    #[test]
    fn save_state_rejects_bad_input() {
        let mut gba = Gba::new(vec![0; 0x4000]);
        assert!(gba.load_state(b"").is_err());
        assert!(gba.load_state(b"NOT_A_STATE").is_err());
        let mut good = gba.save_state();
        good[4] = 0xFF;
        assert!(gba.load_state(&good).is_err(), "bad version rejected");
    }

    #[test]
    fn boots_from_cartridge_entry() {
        // A ROM whose entry is an unconditional branch to itself (B .).
        let mut rom = vec![0u8; 0x2000];
        rom[0..4].copy_from_slice(&0xEAFFFFFEu32.to_le_bytes());
        let mut gba = Gba::new(rom);
        // The CPU must begin executing at the cartridge base.
        assert_eq!(gba.cpu.pc(), 0x0800_0000);
        gba.step();
        // The branch at 0x08000000 loops back to itself (B .).
        assert_eq!(gba.cpu.pc(), 0x0800_0000);
    }

    #[test]
    fn vblank_intr_wait_completes_at_vblank_without_ie() {
        let mut gba = Gba::new(vec![0; 0x4000]);
        assert!(crate::bios::run(&mut gba.cpu, &mut gba.bus, 0x05));
        assert_eq!(gba.cpu.bios_wait_mask(), Some(irq::VBLANK));
        gba.line = ppu::VISIBLE_LINES - 1;
        gba.tick_line();
        assert_eq!(gba.cpu.bios_wait_mask(), None);
    }

    #[test]
    fn bios_if_flag_cleared_on_entry_and_set_at_vblank() {
        let mut gba = Gba::new(vec![0; 0x4000]);
        crate::bios::run(&mut gba.cpu, &mut gba.bus, 0x05);
        // Flag cleared on IntrWait entry.
        assert_eq!(gba.peek32(0x0300_7FF8) & 1, 0);
        // Advance to VBlank.
        gba.line = ppu::VISIBLE_LINES - 1;
        gba.tick_line();
        // Flag set, wait cleared.
        assert_eq!(gba.cpu.bios_wait_mask(), None);
        assert_eq!(gba.peek32(0x0300_7FF8) & 1, 1);
    }

    #[test]
    fn bios_irq_dispatch_enters_irq_mode() {
        let mut gba = Gba::new(vec![0; 0x4000]);
        // Install a stub handler at 0x03007FFC pointing to IWRAM.
        gba.bus.write32(0x0300_7FFC, 0x0300_0100);
        // Install a NOP (MOV r0,r0) at that address.
        gba.bus.write32(0x0300_0100, 0xE1A0_0000);
        crate::bios::run(&mut gba.cpu, &mut gba.bus, 0x05);
        gba.line = ppu::VISIBLE_LINES - 1;
        gba.tick_line();
        // After dispatch, CPU should be in IRQ mode at handler address.
        assert_eq!(gba.cpu.pc(), 0x0300_0100);
        assert_eq!(gba.cpu.cpsr() & 0x1F, crate::cpu::mode::IRQ);
    }
}
