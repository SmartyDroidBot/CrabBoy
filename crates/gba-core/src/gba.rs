//! GBA system: the composition root.
//!
//! [`Gba`] owns the CPU, bus, PPU and APU and implements [`emu_core::System`],
//! presenting a uniform, platform-agnostic interface to frontends. It drives
//! the per-frame timing: each `step` runs one CPU instruction and then advances
//! the timers, APU and PPU scanlines by the cycles consumed, firing the
//! appropriate interrupts and DMA triggers.

use crate::apu::Apu;
use crate::bus::Bus;
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
        // Skip-BIOS boot: leave the machine as the BIOS does when it hands
        // control to the cartridge (GBATEK; mGBA `GBASkipBIOS`): the three
        // stacks set up, SYS mode with IRQs enabled, POSTFLG marking a warm
        // boot and the LCD partway through its first frame (VCOUNT = 0x7E).
        cpu.set_mode_sp(mode::SVC, 0x0300_7FE0);
        cpu.set_mode_sp(mode::IRQ, 0x0300_7FA0);
        cpu.set_mode_sp(mode::USR, 0x0300_7F00);
        cpu.set_cpsr(0x1F);
        cpu.set_pc(0x0800_0000);
        let mut bus = Bus::new(rom);
        Self::install_irq_stub(&mut bus);
        bus.io.regs[0x300] = 1;
        bus.io.set_vcount(0x7E);
        Gba {
            cpu,
            bus,
            ppu: Ppu::new(),
            apu: Apu::new(),
            line_cycles: 0,
            line: 0x7E,
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
        // The real GBA powers on in SVC mode with IRQs masked at the reset
        // vector; the BIOS sets up everything else itself.
        gba.cpu = Cpu::new();
        gba.cpu.set_has_bios(true);
        gba.bus.set_bios(bios);
        // POSTFLG=1 requests a warm boot, skipping the ~2s "Nintendo" logo.
        gba.bus.io.regs[0x300] = u8::from(!cold);
        gba.bus.io.set_vcount(0);
        gba.line = 0;
        gba
    }

    /// BIOS address of the IRQ return stub used by the skip-BIOS boot.
    const IRQ_RETURN_STUB: u32 = 0x20;

    /// The real BIOS returns from the game's IRQ handler through
    /// `ldmfd sp!, {r0-r3, r12, lr}; subs pc, lr, #4`. Without a BIOS image,
    /// plant those two instructions at 0x20 so that `bx lr` from the handler
    /// pops the frame `enter_irq` pushed and resumes the interrupted code.
    fn install_irq_stub(bus: &mut Bus) {
        if bus.has_real_bios() {
            return;
        }
        let mut stub = vec![0u8; 0x28];
        stub[0x20..0x24].copy_from_slice(&0xE8BD_500Fu32.to_le_bytes());
        stub[0x24..0x28].copy_from_slice(&0xE25E_F004u32.to_le_bytes());
        bus.set_bios(stub);
    }

    /// Take a pending IRQ. With a real BIOS the vector at 0x18 runs its
    /// dispatcher; otherwise replicate it: push {r0-r3, r12, lr} on the IRQ
    /// stack, point LR at the return stub, pass the I/O base in r0 (the BIOS
    /// leaves it there) and jump to the handler registered at 0x03007FFC.
    fn enter_irq(&mut self) {
        let lr = self.cpu.pc().wrapping_add(4);
        self.cpu.irq(lr);
        self.cpu.halted = false;
        if self.cpu.has_bios {
            return;
        }
        let handler = self.bus.read32(0x0300_7FFC);
        let sp = self.cpu.sp_raw().wrapping_sub(24);
        for (i, r) in [0u32, 1, 2, 3, 12, 14].into_iter().enumerate() {
            let v = self.cpu.reg_raw(r);
            self.bus.write32(sp.wrapping_add(i as u32 * 4), v);
        }
        self.cpu.set_reg(13, sp);
        self.cpu.set_reg(14, Self::IRQ_RETURN_STUB);
        self.cpu.set_reg(0, 0x0400_0000);
        self.cpu.set_pc(handler);
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
                let s = ch.cur_src;
                for _ in 0..4 {
                    let w = self.bus.read32(s);
                    self.apu.push_fifo_a(w as u8);
                    self.apu.push_fifo_a((w >> 8) as u8);
                    self.apu.push_fifo_a((w >> 16) as u8);
                    self.apu.push_fifo_a((w >> 24) as u8);
                }
                ch.cur_src = crate::dma::adjust(s, 16, ch.src_adjust());
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
                let s = ch.cur_src;
                for _ in 0..4 {
                    let w = self.bus.read32(s);
                    self.apu.push_fifo_b(w as u8);
                    self.apu.push_fifo_b((w >> 8) as u8);
                    self.apu.push_fifo_b((w >> 16) as u8);
                    self.apu.push_fifo_b((w >> 24) as u8);
                }
                ch.cur_src = crate::dma::adjust(s, 16, ch.src_adjust());
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
        let in_vblank = (ppu::VISIBLE_LINES..ppu::LINES_PER_FRAME - 1).contains(&y);
        let vblank_flag = if in_vblank { 1 } else { 0 };

        // Render the visible line.
        if y < ppu::VISIBLE_LINES {
            self.ppu.render_scanline(&self.bus, y);
        }

        // VBlank transition.
        if y == ppu::VISIBLE_LINES {
            self.ppu.reload_affine_refs(&self.bus);
            self.bus.run_dma(Timing::VBlank);
            if irq_en & 0x01 != 0 {
                self.bus.io.raise_irq(irq::VBLANK);
            }
        }
        // VCOUNT match.
        if y == vcount_setting && irq_en & 0x04 != 0 {
            self.bus.io.raise_irq(irq::VCOUNT);
        }
        // HBlank DMA runs on visible lines only; the HBlank IRQ fires on every
        // line, including those of VBlank.
        if y < ppu::VISIBLE_LINES {
            self.bus.run_dma(Timing::HBlank);
        }
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

        // HLE IntrWait: the BIOS routine halts until the game's IRQ handler
        // flags the awaited interrupt in the IF mirror at 0x03007FF8, taking
        // interrupts normally in the meantime. The check only applies while
        // the PC is back at the wait loop, not while a handler runs.
        if self.cpu.at_bios_wait() {
            let mask = self.cpu.bios_wait_mask().unwrap_or(0);
            let mirror = self.bus.read16(crate::bios::BIOS_IF_ADDR) as u16;
            if mirror & mask != 0 {
                self.bus
                    .write16(crate::bios::BIOS_IF_ADDR, (mirror & !mask) as u32);
                self.cpu.complete_bios_wait();
                self.cpu.halted = false;
            } else {
                self.cpu.halted = true;
            }
        }

        if self.bus.pending_irq() != 0 && !self.cpu.irq_masked() {
            self.enter_irq();
            self.advance(4);
            return 4;
        }

        if self.cpu.halted {
            // HALT ends on any enabled interrupt even while IME is clear; an
            // IntrWait only ends through its mirror flag.
            if self.cpu.bios_wait_mask().is_none() && self.bus.wake_irq() != 0 {
                self.cpu.halted = false;
            } else {
                self.advance(4);
                return 4;
            }
        }

        self.bus.begin_step();
        let instr = self.cpu.execute(&mut self.bus);
        let total = instr + self.bus.cycles();
        // Dispatch a BIOS SWI (if any) now that the instruction has finished.
        if let Some(num) = self.cpu.take_bios_call() {
            if !crate::bios::run(&mut self.cpu, &mut self.bus, num) {
                // Unimplemented routine: return to the caller and record the
                // number so the diagnostic tools can report it.
                self.last_unknown_swi = Some(num);
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
    fn skip_bios_boot_state() {
        let mut gba = Gba::new(vec![0; 0x4000]);
        assert_eq!(gba.cpu.cpsr() & 0x1F, 0x1F, "SYS mode");
        assert_eq!(gba.cpu.cpsr() & (1 << 7), 0, "IRQs enabled");
        assert_eq!(gba.cpu.sp_raw(), 0x0300_7F00);
        gba.cpu.set_cpsr(mode::IRQ);
        assert_eq!(gba.cpu.sp_raw(), 0x0300_7FA0);
        gba.cpu.set_cpsr(mode::SVC);
        assert_eq!(gba.cpu.sp_raw(), 0x0300_7FE0);
        assert_eq!(gba.bus.io.regs[0x300], 1, "POSTFLG");
        assert_eq!(gba.bus.io.vcount(), 0x7E);
        assert!(!gba.cpu.has_bios);
    }

    /// A cartridge that calls VBlankIntrWait and then spins, with an ARM IRQ
    /// handler in IWRAM that acknowledges IF and returns with `bx lr`.
    fn vblank_wait_machine(ie: u16) -> Gba {
        let mut rom = vec![0u8; 0x4000];
        rom[0..4].copy_from_slice(&0xEF05_0000u32.to_le_bytes()); // swi VBlankIntrWait
        rom[4..8].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes()); // b .
        let mut gba = Gba::new(rom);
        gba.bus.write32(0x0300_7FFC, 0x0300_0100);
        for (i, w) in [
            0xE3A0_0404u32, // mov r0, #0x04000000
            0xE380_0C02,    // orr r0, r0, #0x200
            0xE3A0_1001,    // mov r1, #1
            0xE1C0_10B2,    // strh r1, [r0, #2]   (acknowledge VBlank in IF)
            0xE12F_FF1E,    // bx lr
        ]
        .iter()
        .enumerate()
        {
            gba.bus.write32(0x0300_0100 + i as u32 * 4, *w);
        }
        gba.bus.write16(0x0400_0200, ie as u32);
        gba.bus.write16(0x0400_0004, 0x0008); // DISPSTAT: VBlank IRQ enable
        gba
    }

    fn step_until(gba: &mut Gba, cond: impl Fn(&Gba) -> bool, max_steps: u32) -> bool {
        for _ in 0..max_steps {
            if cond(gba) {
                return true;
            }
            gba.step();
        }
        cond(gba)
    }

    #[test]
    fn vblank_intr_wait_returns_after_handler_sets_mirror() {
        let mut gba = vblank_wait_machine(irq::VBLANK);
        gba.step(); // swi
        assert_eq!(gba.cpu.bios_wait_mask(), Some(irq::VBLANK));
        assert!(gba.bus.io.ime(), "IntrWait enables IME");
        assert_eq!(gba.cpu.pc(), 0x0800_0004);

        // The VBlank IRQ enters the handler through the BIOS-style frame.
        assert!(step_until(&mut gba, |g| g.cpu.pc() == 0x0300_0100, 400_000));
        assert_eq!(gba.cpu.cpsr() & 0x1F, mode::IRQ);
        assert_eq!(gba.cpu.reg_raw(0), 0x0400_0000);
        assert_eq!(gba.cpu.reg_raw(14), Gba::IRQ_RETURN_STUB);
        assert_eq!(gba.cpu.sp_raw(), 0x0300_7FA0 - 24);
        assert_eq!(gba.peek32(0x0300_7FA0 - 4), 0x0800_0008, "saved LR");

        // Handler (5) + stub (2) return to the wait in SYS mode; the mirror
        // is still clear, so the wait continues.
        for _ in 0..7 {
            gba.step();
        }
        assert_eq!(gba.cpu.cpsr() & 0x1F, 0x1F);
        assert_eq!(gba.cpu.pc(), 0x0800_0004);
        assert_eq!(gba.cpu.bios_wait_mask(), Some(irq::VBLANK));
        assert_eq!(gba.bus.io.iflags() & irq::VBLANK, 0, "handler acked IF");

        // Once a handler flags the mirror the wait completes and the game
        // continues with the next instruction.
        gba.bus
            .write16(crate::bios::BIOS_IF_ADDR, irq::VBLANK as u32);
        gba.step();
        assert_eq!(gba.cpu.bios_wait_mask(), None);
        assert_eq!(gba.bus.read16(crate::bios::BIOS_IF_ADDR), 0);
        gba.step();
        assert_eq!(gba.cpu.pc(), 0x0800_0004, "b . keeps spinning");
    }

    #[test]
    fn vblank_intr_wait_stays_halted_without_ie() {
        let mut gba = vblank_wait_machine(0);
        gba.step();
        for _ in 0..3 {
            gba.run_frame();
        }
        assert_eq!(gba.cpu.bios_wait_mask(), Some(irq::VBLANK));
        assert_eq!(gba.cpu.pc(), 0x0800_0004, "never reached the handler");
        assert!(gba.cpu.halted);
    }

    #[test]
    fn irq_return_stub_is_installed_only_without_a_real_bios() {
        let gba = Gba::new(vec![0; 0x4000]);
        assert!(!gba.bus.has_real_bios());
        assert_eq!(gba.bus.bios.len(), 0x28);
        let real = Gba::with_bios(vec![0; 0x4000], vec![0; 0x4000], false);
        assert!(real.bus.has_real_bios());
        assert!(real.cpu.has_bios);
    }
}
