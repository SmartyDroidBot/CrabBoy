//! DMG Game Boy system: the composition root.
//!
//! [`Gb`] owns the CPU and bus and implements [`emu_core::System`], presenting
//! a uniform, platform-agnostic interface to frontends. Add `Gb::new` to a
//! system selector to host it.

use crate::bus::{Bus, Model};
use crate::cartridge::Cartridge;
use crate::cpu::Cpu;
use crate::devices::joypad;

pub const FRAME_CYCLES: u32 = 70224;
pub const SCREEN_W: u16 = 160;
pub const SCREEN_H: u16 = 144;

pub struct Gb {
    pub cpu: Cpu,
    pub bus: Bus,
}

impl Gb {
    pub fn new(cart: Cartridge) -> Gb {
        let model = Model::for_cart(&cart);
        Gb::new_with_model(cart, model)
    }

    /// Boot the cartridge on a specific console model instead of the one its
    /// header requests (a CGB running a DMG cart in compatibility mode, or a
    /// DMG ignoring the colour flag).
    pub fn new_with_model(cart: Cartridge, model: Model) -> Gb {
        let mut bus = Bus::new_with_model(cart, model);
        let mut cpu = Cpu::new();
        if bus.is_cgb {
            // The CGB boot ROM leaves the CPU in CGB mode with A=$11 (which
            // games like Crystal probe to detect the console) and the related
            // register state; the DMG defaults (A=$01, F=$B0, ...) apply only
            // to plain Game Boy carts. See SameBoy's cgb_boot.asm.
            cpu.a = 0x11;
            cpu.f = 0x80;
            cpu.b = 0x00;
            cpu.c = 0x00;
            cpu.d = 0xFF;
            cpu.e = 0x00;
            cpu.h = 0x00;
            cpu.l = 0x0D;
            // KEY0: write the cartridge's CGB compatibility flag, as the boot
            // ROM does when leaving full CGB mode.
            bus.io[0x4C] = bus.cart.cgb_flag;
        }
        Gb { cpu, bus }
    }

    /// Construct a `Box<dyn System>` from a cartridge (frontend convenience).
    pub fn system(cart: Cartridge) -> Box<dyn emu_core::System> {
        Box::new(Gb::new(cart))
    }

    /// Whether the CPU executed `ld b,b` (opcode $40) since the last call.
    /// Test suites (mooneye, mealybug, acid2) use it to signal completion.
    pub fn take_breakpoint(&mut self) -> bool {
        std::mem::take(&mut self.cpu.breakpoint)
    }

    pub fn press_button(&mut self, button: u8) {
        if self.bus.joypad.state & button != 0 {
            self.bus.joypad.press(button);
            self.bus.io[0x0F] |= 0x10;
        }
    }

    pub fn release_button(&mut self, button: u8) {
        if self.bus.joypad.state & button == 0 {
            self.bus.joypad.release(button);
            self.bus.io[0x0F] |= 0x10;
        }
    }

    fn button_to_bit(button: emu_core::Button) -> Option<u8> {
        Some(match button {
            emu_core::Button::A => joypad::BUTTON_A,
            emu_core::Button::B => joypad::BUTTON_B,
            emu_core::Button::Start => joypad::BUTTON_START,
            emu_core::Button::Select => joypad::BUTTON_SELECT,
            emu_core::Button::Left => joypad::BUTTON_LEFT,
            emu_core::Button::Right => joypad::BUTTON_RIGHT,
            emu_core::Button::Up => joypad::BUTTON_UP,
            emu_core::Button::Down => joypad::BUTTON_DOWN,
            // GB has no X/Y/L/R.
            _ => return None,
        })
    }

    /// Execute one instruction (or idle cycle), returning cycles consumed.
    pub fn step(&mut self) -> u32 {
        // STOP halts the CPU (and LCD) until a button is pressed. On CGB
        // hardware, STOP with KEY1 bit 0 set toggles double speed instead.
        if self.cpu.stopped {
            if self.bus.is_cgb && self.bus.io[0x4D] & 1 != 0 {
                self.bus.double_speed = !self.bus.double_speed;
                self.cpu.stopped = false;
            } else if self.bus.joypad.state != 0xFF {
                self.cpu.stopped = false;
            } else {
                self.bus.step(4);
                return 4;
            }
        }

        if self.cpu.halted {
            let pending = self.bus.ie & self.bus.io[0x0F] & 0x1F;
            if pending != 0 {
                self.cpu.halted = false;
            } else {
                self.bus.step(4);
                return 4;
            }
        }

        let pending = self.bus.ie & self.bus.io[0x0F] & 0x1F;
        if self.cpu.ime && pending != 0 {
            return self.cpu.take_interrupt(&mut self.bus);
        }

        // EI enables interrupts only *after* the instruction following EI runs,
        // so `EI; RET` returns before an interrupt fires. Capture whether an EI
        // executed on the previous step: if it did (and DI has not since cleared
        // the pending flag), the following instruction has now completed and IME
        // may be enabled. This is also why `EI; DI` leaves IME disabled.
        let was_ei = self.cpu.ei_pending;
        // The CPU advances the bus itself, one M-cycle per memory access or
        // internal cycle, so devices see every access at its true time.
        let total = self.cpu.execute(&mut self.bus);

        if was_ei && self.cpu.ei_pending {
            self.cpu.ime = true;
            self.cpu.ei_pending = false;
        }
        total
    }

    /// Current framebuffer as 2-bit shades (`0..=3`) per pixel.
    pub fn framebuffer(&self) -> &[u8] {
        self.bus.frame()
    }

    pub fn battery_backed(&self) -> bool {
        self.bus.cart.has_battery()
    }

    pub fn load_sram(&mut self, path: &str) {
        use std::fs::File;
        use std::io::Read;
        let bytes = self.bus.cart.ram_bytes();
        if let Ok(mut f) = File::open(path) {
            let mut data = vec![0u8; bytes];
            let _ = f.read(&mut data);
            self.bus.cart.ram.copy_from_slice(&data);
            println!("loaded SRAM from {}", path);
        }
    }

    pub fn save_sram(&self, path: &str) {
        use std::fs::File;
        use std::io::Write;
        let bytes = self.bus.cart.ram_bytes();
        if bytes == 0 {
            return;
        }
        if let Ok(mut f) = File::create(path) {
            let _ = f.write_all(&self.bus.cart.ram);
            println!("saved SRAM ({} bytes) to {}", bytes, path);
        }
    }
}

impl emu_core::System for Gb {
    fn name(&self) -> &'static str {
        if self.bus.is_cgb {
            "gbc"
        } else {
            "gb"
        }
    }

    fn info(&self) -> String {
        format!(
            "{} ({}, battery={})",
            self.bus.cart.title,
            self.bus.cart.mbc,
            self.bus.cart.has_battery()
        )
    }

    fn title(&self) -> String {
        self.bus.cart.title.trim().to_string()
    }

    fn screen(&self) -> emu_core::Screen {
        emu_core::Screen::new(SCREEN_W, SCREEN_H)
    }

    fn reset(&mut self) {
        let cart = self.bus.cart.clone();
        *self = Gb::new(cart);
    }

    fn press(&mut self, button: emu_core::Button) {
        if let Some(bit) = Self::button_to_bit(button) {
            self.press_button(bit);
        }
    }

    fn release(&mut self, button: emu_core::Button) {
        if let Some(bit) = Self::button_to_bit(button) {
            self.release_button(bit);
        }
    }

    fn step(&mut self) -> u32 {
        Gb::step(self)
    }

    fn frame_cycles(&self) -> u32 {
        self.bus.frame_cycles()
    }

    fn frame(&self) -> emu_core::Frame {
        let shades = self.framebuffer();
        let mut f = emu_core::Frame {
            width: SCREEN_W,
            height: SCREEN_H,
            shades: shades.to_vec(),
            rgb: None,
        };
        f.rgb = Some(self.bus.ppu.rgb_buffer.clone());
        f
    }

    fn framebuffer(&self) -> &[u8] {
        Gb::framebuffer(self)
    }

    fn audio_rate(&self) -> u32 {
        if self.bus.double_speed {
            16384
        } else {
            8192
        }
    }

    fn take_audio(&mut self) -> emu_core::audio::AudioBuffer {
        let mut buf = std::mem::take(&mut self.bus.apu.buffer);
        // Frontends bound their own latency; only guard against a caller that
        // never drains by keeping the newest second of audio.
        let cap = self.audio_rate() as usize * 2;
        if buf.samples.len() > cap {
            let excess = buf.samples.len() - cap;
            buf.samples.drain(..excess);
        }
        buf
    }

    fn battery_backed(&self) -> bool {
        self.battery_backed()
    }

    fn save_data(&self) -> Vec<u8> {
        self.bus.cart.ram.clone()
    }

    fn load_data(&mut self, data: &[u8]) {
        let bytes = self.bus.cart.ram.len();
        if bytes == 0 {
            return;
        }
        let n = data.len().min(bytes);
        self.bus.cart.ram[..n].copy_from_slice(&data[..n]);
    }

    fn sram_changed(&mut self) -> bool {
        self.bus.cart.sram_changed()
    }

    fn rtc_data(&self) -> Vec<u8> {
        self.bus.cart.rtc_data()
    }

    fn load_rtc(&mut self, data: &[u8]) {
        self.bus.cart.load_rtc(data);
    }

    fn save_state(&self) -> Vec<u8> {
        crate::state::save_state(&self.cpu, &self.bus)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        crate::state::load_state(&mut self.cpu, &mut self.bus, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::joypad::*;

    #[test]
    fn serial_buffer_receives_the_transmitted_byte() {
        use emu_core::System;
        let mut emu = Gb::new(Cartridge::load(&[0u8; 0x8000]).unwrap());
        emu.bus.write(0xFF01, b'P');
        emu.bus.write(0xFF02, 0x81);
        emu.run_frame();
        assert_eq!(emu.bus.serial_buf, vec![b'P']);
        assert_eq!(
            emu.bus.read(0xFF01),
            0xFF,
            "$FF shifts in without a partner"
        );
        assert_eq!(emu.bus.read(0xFF02) & 0x80, 0);
    }

    #[test]
    fn model_selection_overrides_the_header() {
        let cart = Cartridge::load(&[0u8; 0x8000]).unwrap();
        let emu = Gb::new_with_model(cart, Model::Cgb);
        assert!(emu.bus.is_cgb);
        assert_eq!(emu.cpu.a, 0x11);
        let mut rom = vec![0u8; 0x8000];
        rom[0x143] = 0x80;
        let emu = Gb::new_with_model(Cartridge::load(&rom).unwrap(), Model::Dmg);
        assert!(!emu.bus.is_cgb);
        assert_eq!(emu.cpu.a, 0x01);
    }

    #[test]
    fn ld_b_b_raises_the_breakpoint_flag() {
        let mut rom = vec![0u8; 0x8000];
        rom[0x100] = 0x00; // nop
        rom[0x101] = 0x40; // ld b,b
        rom[0x102] = 0x18; // jr -2
        rom[0x103] = 0xFE;
        let mut emu = Gb::new(Cartridge::load(&rom).unwrap());
        assert!(!emu.take_breakpoint());
        emu.step();
        assert!(!emu.take_breakpoint());
        emu.step();
        assert!(emu.take_breakpoint());
        assert!(!emu.take_breakpoint(), "the flag is consumed");
    }

    #[test]
    fn take_audio_returns_the_whole_frame() {
        use emu_core::System;
        let mut emu = Gb::new(Cartridge::load(&[0u8; 0x8000]).unwrap());
        emu.bus.write(0xFF26, 0x80);
        emu.run_frame();
        let pairs = emu.take_audio().samples.len() / 2;
        // 70224 cycles / 512 cycles per sample = 137.16, so 137 or 138.
        assert!(pairs == 137 || pairs == 138, "{pairs} samples");
        assert!(emu.take_audio().samples.is_empty(), "drained");
    }

    #[test]
    fn title_and_screen_come_from_the_header() {
        use emu_core::System;
        let mut rom = vec![0u8; 0x8000];
        rom[0x134..0x134 + 7].copy_from_slice(b"POKEMON");
        let emu = Gb::new(Cartridge::load(&rom).unwrap());
        assert_eq!(emu.title(), "POKEMON");
        assert_eq!(emu.screen(), emu_core::Screen::new(160, 144));
        assert_eq!(emu.frame_rate(), 59.7275);
    }

    #[test]
    fn p1_read_reflects_pressed_buttons() {
        let cart = Cartridge::load(&[0u8; 0x8000]).unwrap();
        let mut emu = Gb::new(cart);

        // Select the "buttons" row (bit5/P15 low). Nothing pressed yet.
        emu.bus.write(0xFF00, 0x10);
        let before = emu.bus.read(0xFF00);
        assert_eq!(before & 0x0F, 0x0F, "all buttons idle = 1");

        // Press START (bit3 of buttons row).
        emu.press_button(BUTTON_START);
        let after = emu.bus.read(0xFF00);
        assert_eq!(after & 0x08, 0x00, "START bit should read 0 (pressed)");
        assert_eq!(after & 0x07, 0x07, "other buttons stay 1");

        emu.release_button(BUTTON_START);
        assert_eq!(emu.bus.read(0xFF00) & 0x08, 0x08, "released = 1");
    }

    #[test]
    fn p1_read_reflects_dpad() {
        let cart = Cartridge::load(&[0u8; 0x8000]).unwrap();
        let mut emu = Gb::new(cart);

        // Select the dpad row (bit4/P14 low).
        emu.bus.write(0xFF00, 0x20);
        emu.press_button(BUTTON_DOWN);
        assert_eq!(emu.bus.read(0xFF00) & 0x0F, 0x07, "DOWN reads as dpad bit3");
        emu.press_button(BUTTON_RIGHT);
        assert_eq!(emu.bus.read(0xFF00) & 0x0F, 0x06, "DOWN+RIGHT");
    }

    #[test]
    fn ei_enables_interrupts_after_next_instruction() {
        let cart = Cartridge::load(&[0u8; 0x8000]).unwrap();
        let mut emu = Gb::new(cart);
        emu.bus.ie = 0x01;
        emu.bus.io[0x0F] = 0x01; // vblank pending
        emu.bus.cart.rom[0x0100] = 0xFB; // EI
        emu.bus.cart.rom[0x0101] = 0x00; // NOP

        emu.step(); // EI
        assert!(!emu.cpu.ime, "IME not yet enabled right after EI");
        assert_eq!(emu.cpu.pc, 0x0101);

        emu.step(); // NOP (the instruction following EI)
        assert!(
            emu.cpu.ime,
            "IME enabled after the instruction following EI"
        );
        assert_eq!(
            emu.cpu.pc, 0x0102,
            "NOP ran; interrupt not serviced before it"
        );

        emu.step(); // now the pending interrupt fires
        assert_eq!(emu.cpu.pc, 0x0040, "interrupt serviced once IME is set");
    }

    #[test]
    fn di_cancels_pending_ei() {
        let cart = Cartridge::load(&[0u8; 0x8000]).unwrap();
        let mut emu = Gb::new(cart);
        emu.bus.ie = 0x01;
        emu.bus.io[0x0F] = 0x01;
        emu.bus.cart.rom[0x0100] = 0xFB; // EI
        emu.bus.cart.rom[0x0101] = 0xF3; // DI
        emu.step(); // EI
        emu.step(); // DI
        assert!(!emu.cpu.ime, "DI cancels the pending EI");
    }

    #[test]
    fn stopped_cpu_wakes_on_button_press() {
        let cart = Cartridge::load(&[0u8; 0x8000]).unwrap();
        let mut emu = Gb::new(cart);
        emu.bus.cart.rom[0x0100] = 0x10; // STOP
        emu.bus.cart.rom[0x0101] = 0x00; // padding
        emu.step();
        assert!(emu.cpu.stopped);
        emu.step();
        assert!(emu.cpu.stopped, "still stopped with no button held");
        emu.press_button(BUTTON_A);
        emu.step();
        assert!(!emu.cpu.stopped, "STOP exits on a button press");
    }

    fn hash(buf: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in buf {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    fn run(n: usize, emu: &mut Gb) {
        use emu_core::System;
        for _ in 0..n {
            emu.run_frame();
        }
    }

    #[test]
    fn save_state_round_trip_is_exact() {
        use emu_core::System;
        let cart = Cartridge::load(&[0u8; 0x8000]).unwrap();

        // Control: 60 + 37 + 37 frames with no save/load.
        let mut control = Gb::new(cart.clone());
        run(60, &mut control);
        run(37, &mut control);
        run(37, &mut control);

        // Test: save at 60, run 37, restore, run 37 more (then 37 more).
        let mut test = Gb::new(cart);
        run(60, &mut test);
        let saved = test.save_state();
        run(37, &mut test);
        test.load_state(&saved).unwrap();
        run(37, &mut test);
        run(37, &mut test);

        assert_eq!(
            control.framebuffer(),
            test.framebuffer(),
            "framebuffer differs"
        );
        assert_eq!(
            hash(&control.bus.serial_buf),
            hash(&test.bus.serial_buf),
            "serial buffer differs"
        );
        assert_eq!(control.cpu.pc, test.cpu.pc, "PC differs");
        assert_eq!(control.cpu.sp, test.cpu.sp, "SP differs");
        assert_eq!(control.bus.io, test.bus.io, "IO registers differ");
        assert_eq!(control.bus.vram, test.bus.vram, "VRAM differs");
        assert_eq!(control.bus.wram, test.bus.wram, "WRAM differs");
        assert_eq!(control.bus.oam, test.bus.oam, "OAM differs");
        assert_eq!(control.bus.hram, test.bus.hram, "HRAM differs");
        assert_eq!(
            control.bus.ppu.frame_buffer, test.bus.ppu.frame_buffer,
            "PPU frame buffer differs"
        );
        assert_eq!(control.bus.ppu.ly, test.bus.ppu.ly, "LY differs");
        assert_eq!(control.bus.ppu.dot, test.bus.ppu.dot, "PPU dot differs");
    }

    #[test]
    fn save_state_rejects_garbage_and_unknown_version() {
        use emu_core::System;
        let cart = Cartridge::load(&[0u8; 0x8000]).unwrap();
        let mut emu = Gb::new(cart);
        run(5, &mut emu);

        assert!(emu.load_state(b"").is_err(), "empty input rejected");
        assert!(
            emu.load_state(b"NOT_A_STATE").is_err(),
            "bad magic rejected"
        );
        assert!(
            emu.load_state(&[0u8; 64]).is_err(),
            "all-zero payload rejected"
        );

        // Corrupt the version byte of an otherwise valid state.
        let mut good = emu.save_state();
        good[4] = 0xFF;
        assert!(emu.load_state(&good).is_err(), "unknown version rejected");

        // Truncate a valid state at every length below full; all must fail.
        let full = emu.save_state();
        for len in 0..full.len() {
            assert!(
                emu.load_state(&full[..len]).is_err(),
                "truncated state (len {len}) must be rejected"
            );
        }
        // Trailing garbage must also be rejected.
        let mut padded = full.clone();
        padded.push(0xAA);
        assert!(emu.load_state(&padded).is_err(), "trailing bytes rejected");
    }

    #[test]
    fn save_state_idempotent_round_trip() {
        use emu_core::System;
        let cart = Cartridge::load(&[0u8; 0x8000]).unwrap();
        let mut emu = Gb::new(cart);
        run(10, &mut emu);

        let s1 = emu.save_state();
        emu.load_state(&s1).unwrap();
        let s2 = emu.save_state();
        assert_eq!(s1, s2, "save(load(save())) must equal save()");
    }

    #[test]
    fn battery_save_tracks_game_writes() {
        use emu_core::System;
        // MBC3 + RAM + battery (4KB SRAM) via a minimal header.
        let mut data = vec![0u8; 0x8000];
        data[0x147] = 0x13;
        data[0x149] = 0x02; // 1 SRAM bank
        let mut emu = Gb::new(Cartridge::load(&data).unwrap());

        assert!(emu.battery_backed());
        assert!(!emu.sram_changed(), "clean at start");
        assert_eq!(emu.save_data().len(), 0x2000, "SRAM buffer exists");
        assert_eq!(emu.save_data()[0], 0, "SRAM starts zeroed");

        // Enable RAM and have the "game" write a save byte.
        emu.bus.write(0x0000, 0x0A);
        emu.bus.write(0xA000, 0x5A);

        assert!(emu.sram_changed(), "write marks dirty");
        assert!(!emu.sram_changed(), "flag clears after check");
        assert_eq!(emu.save_data()[0], 0x5A, "save reflects the write");

        // Round-trip into a fresh machine.
        let saved = emu.save_data();
        let mut emu2 = Gb::new(Cartridge::load(&data).unwrap());
        emu2.load_data(&saved);
        assert_eq!(emu2.save_data()[0], 0x5A, "loaded save matches");
    }

    fn cgb_cart() -> Cartridge {
        let mut data = vec![0u8; 0x8000];
        data[0x143] = 0x80; // CGB-only
        Cartridge::load(&data).unwrap()
    }

    #[test]
    fn cgb_boot_registers_match_cgb_boot_rom() {
        let emu = Gb::new(cgb_cart());
        assert!(emu.bus.is_cgb, "0x80 header -> CGB mode");
        assert_eq!(emu.cpu.a, 0x11, "A=$11 marks CGB hardware to the game");
        assert_eq!(emu.cpu.f, 0x80, "CGB boot leaves F=$80");
        assert_eq!(emu.cpu.c, 0x00);
        assert_eq!(emu.cpu.d, 0xFF);
        assert_eq!(emu.cpu.e, 0x00);
        assert_eq!(emu.cpu.h, 0x00);
        assert_eq!(emu.cpu.l, 0x0D);
        assert_eq!(emu.bus.io[0x4C], 0x80, "KEY0 = cart CGB flag");
    }

    #[test]
    fn dmg_boot_registers_stay_dmg() {
        let emu = Gb::new(Cartridge::load(&[0u8; 0x8000]).unwrap());
        assert!(!emu.bus.is_cgb, "plain cart stays in DMG mode");
        assert_eq!(emu.cpu.a, 0x01, "DMG boot leaves A=$01");
        assert_eq!(emu.cpu.f, 0xB0, "DMG boot leaves F=$B0");
        assert_eq!(emu.bus.io[0x4C], 0x00);
    }

    #[test]
    fn cgb_vram_banking() {
        let mut emu = Gb::new(cgb_cart());
        emu.bus.write(0x8000, 0x11); // bank 0
        emu.bus.write(0xFF4F, 0x01); // VBK = 1
        emu.bus.write(0x8000, 0x22); // bank 1
        assert_eq!(emu.bus.vram[0x0000], 0x11, "bank 0 stored");
        assert_eq!(emu.bus.vram[0x2000], 0x22, "bank 1 stored");
        emu.bus.write(0xFF4F, 0x00);
        assert_eq!(emu.bus.read(0x8000), 0x11, "bank 0 read");
        emu.bus.write(0xFF4F, 0x01);
        assert_eq!(emu.bus.read(0x8000), 0x22, "bank 1 read");
    }

    #[test]
    fn cgb_wram_banking() {
        let mut emu = Gb::new(cgb_cart());
        emu.bus.write(0xD000, 0xAA); // default SVBK -> bank 1
        emu.bus.write(0xFF70, 0x02);
        emu.bus.write(0xD000, 0xBB);
        emu.bus.write(0xFF70, 0x01);
        assert_eq!(emu.bus.read(0xD000), 0xAA);
        emu.bus.write(0xFF70, 0x02);
        assert_eq!(emu.bus.read(0xD000), 0xBB);
        // C000–CFFF is always bank 0.
        emu.bus.write(0xFF70, 0x05);
        emu.bus.write(0xC000, 0xCC);
        assert_eq!(emu.bus.read(0xC000), 0xCC);
        // E000 mirrors the active bank, F000 mirrors the banked D000 region.
        emu.bus.write(0xFF70, 0x03);
        emu.bus.write(0xD000, 0x12);
        assert_eq!(emu.bus.read(0xF000), 0x12, "F000 mirrors D000 bank");
        assert_eq!(
            emu.bus.read(0xE000),
            emu.bus.read(0xC000),
            "E000 mirrors C000"
        );
        // SVBK 0 acts as bank 1.
        emu.bus.write(0xFF70, 0x00);
        assert_eq!(emu.bus.read(0xD000), 0xAA);
    }

    #[test]
    fn cgb_key1_double_speed_switch_on_stop() {
        use emu_core::System;
        let mut emu = Gb::new(cgb_cart());
        emu.bus.cart.rom[0x0100] = 0x10; // STOP
        emu.bus.cart.rom[0x0101] = 0x00; // padding
        assert_eq!(emu.frame_cycles(), 70224);
        assert_eq!(emu.audio_rate(), 8192);
        emu.bus.write(0xFF4D, 0x01); // request double speed
        emu.step(); // STOP executes
        emu.step(); // STOP -> speed switch, no button wait
        assert!(
            !emu.cpu.stopped,
            "STOP with KEY1 toggles speed instead of halting"
        );
        assert!(emu.bus.double_speed);
        assert_eq!(emu.frame_cycles(), 140448);
        assert_eq!(emu.audio_rate(), 16384);
        assert_eq!(
            emu.bus.read(0xFF4D) & 0x80,
            0x80,
            "bit 7 reflects current speed"
        );
        // Toggle back.
        emu.bus.cart.rom[0x0100] = 0x10;
        emu.cpu.pc = 0x0100;
        emu.bus.write(0xFF4D, 0x01);
        emu.step();
        emu.step();
        assert!(!emu.bus.double_speed, "second STOP returns to normal speed");
        assert_eq!(emu.frame_cycles(), 70224);
    }

    #[test]
    fn cgb_palette_write_auto_increment_and_read() {
        let mut emu = Gb::new(cgb_cart());
        emu.bus.write(0xFF68, 0x80); // BG index 0, auto-increment
        emu.bus.write(0xFF69, 0x34);
        emu.bus.write(0xFF69, 0x56);
        assert_eq!(
            emu.bus.read(0xFF68) & 0x3F,
            2,
            "auto-increment advances index"
        );
        assert_eq!(emu.bus.ppu.bg_pal[0], 0x34);
        assert_eq!(emu.bus.ppu.bg_pal[1], 0x56);
        emu.bus.write(0xFF68, 0x01); // index 1, no auto-increment
        assert_eq!(emu.bus.read(0xFF69), 0x56, "BCPD reads the indexed byte");
        emu.bus.write(0xFF6A, 0x84); // OBJ index 4, auto-increment
        emu.bus.write(0xFF6B, 0xAB);
        assert_eq!(emu.bus.ppu.obj_pal[4], 0xAB);
        assert_eq!(emu.bus.read(0xFF6A) & 0x3F, 5);
    }

    #[test]
    fn cgb_hdma_general_purpose_transfer() {
        let mut emu = Gb::new(cgb_cart());
        for i in 0..0x20 {
            emu.bus.wram[i] = i as u8 + 1;
        }
        emu.bus.write(0xFF51, 0xC0);
        emu.bus.write(0xFF52, 0x00);
        emu.bus.write(0xFF53, 0x80);
        emu.bus.write(0xFF54, 0x00);
        emu.bus.write(0xFF55, 0x01); // 32 bytes, general-purpose
        for i in 0..0x20 {
            assert_eq!(emu.bus.vram[i], i as u8 + 1, "HDMA copied byte {i}");
        }
        assert_eq!(emu.bus.read(0xFF55), 0xFF, "HDMA5 reads FF when complete");
    }

    #[test]
    fn cgb_hdma_hblank_transfer() {
        let mut emu = Gb::new(cgb_cart());
        for i in 0..0x40 {
            emu.bus.wram[i] = i as u8;
        }
        emu.bus.write(0xFF51, 0xC0);
        emu.bus.write(0xFF52, 0x00);
        emu.bus.write(0xFF53, 0x80);
        emu.bus.write(0xFF54, 0x00);
        emu.bus.write(0xFF55, 0x83); // HBlank DMA, 64 bytes
        run(2, &mut emu);
        assert_eq!(emu.bus.read(0xFF55), 0xFF, "HBlank HDMA completed");
        for i in 0..0x40 {
            assert_eq!(emu.bus.vram[i], i as u8);
        }
    }

    #[test]
    fn cgb_dmg_game_uses_boot_default_palette() {
        use emu_core::System;
        // A plain DMG cart still renders colour through the boot palettes.
        let mut emu = Gb::new(Cartridge::load(&[0u8; 0x8000]).unwrap());
        assert!(!emu.bus.is_cgb, "plain cart stays in DMG mode");
        // Tile 0 (LCDC bit 4 set -> base 0x8000, tile 0 at 0x0000) = colour 3.
        for i in 0..0x10 {
            emu.bus.vram[i] = 0xFF;
        }
        emu.bus.vram[0x1800] = 0;
        emu.bus.io[0x47] = 0xFF; // BGP maps colour 3 -> shade 3
        run(1, &mut emu);
        let rgb = emu.frame().rgb.expect("DMG-mode-on-CGB yields colour");
        // Row 1 is the first rendered row (line 0 is the boot HBlank); shade 3
        // -> BG palette 0 entry 3 = boot palette 29 (black).
        assert_eq!(
            &rgb[SCREEN_W as usize * 3..SCREEN_W as usize * 3 + 3],
            &[0, 0, 0]
        );
    }

    #[test]
    fn cgb_cart_renders_from_palette_ram() {
        use emu_core::System;
        let mut emu = Gb::new(cgb_cart());
        for i in 0..0x10 {
            emu.bus.vram[i] = 0xFF;
        }
        emu.bus.vram[0x1800] = 0;
        // BG palette 0 colour 3 = pure red ($001F).
        emu.bus.write(0xFF68, 0x86);
        emu.bus.write(0xFF69, 0x1F);
        emu.bus.write(0xFF69, 0x00);
        assert_eq!(emu.bus.ppu.bg_pal[6], 0x1F, "palette RAM write landed");
        run(1, &mut emu);
        let rgb = emu.frame().rgb.unwrap();
        // Row 1 is the first rendered row; colour 3 -> BG palette 0 entry 3 (red).
        assert_eq!(
            &rgb[SCREEN_W as usize * 3..SCREEN_W as usize * 3 + 3],
            &[255, 0, 0],
            "CGB renders palette RAM colour"
        );
    }

    #[test]
    fn cgb_save_state_round_trip() {
        use emu_core::System;
        let mut control = Gb::new(cgb_cart());
        let mut test = Gb::new(cgb_cart());
        run(5, &mut control);
        run(5, &mut test);
        let saved = test.save_state();
        run(3, &mut test);
        test.load_state(&saved).unwrap();
        assert_eq!(control.bus.vram, test.bus.vram, "VRAM matches");
        assert_eq!(control.bus.wram, test.bus.wram, "WRAM matches");
        assert_eq!(
            control.bus.ppu.bg_pal, test.bus.ppu.bg_pal,
            "BG palettes match"
        );
        assert_eq!(
            control.bus.ppu.obj_pal, test.bus.ppu.obj_pal,
            "OBJ palettes match"
        );
        assert_eq!(
            control.bus.double_speed, test.bus.double_speed,
            "speed matches"
        );
    }
}
