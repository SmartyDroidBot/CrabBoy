//! DMG Game Boy system: the composition root.
//!
//! [`Gb`] owns the CPU and bus and implements [`emu_core::System`], presenting
//! a uniform, platform-agnostic interface to frontends. Add `Gb::new` to a
//! system selector to host it.

use crate::bus::Bus;
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
        let bus = Bus::new(cart);
        let cpu = Cpu::new();
        Gb { cpu, bus }
    }

    /// Construct a `Box<dyn System>` from a cartridge (frontend convenience).
    pub fn system(cart: Cartridge) -> Box<dyn emu_core::System> {
        Box::new(Gb::new(cart))
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
        // STOP halts the CPU (and LCD) until a button is pressed.
        if self.cpu.stopped {
            if self.bus.joypad.state != 0xFF {
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
            let cycles = self.cpu.take_interrupt(&mut self.bus);
            self.bus.step(cycles);
            return cycles;
        }

        // EI enables interrupts only *after* the instruction following EI runs,
        // so `EI; RET` returns before an interrupt fires. Capture whether an EI
        // executed on the previous step: if it did (and DI has not since cleared
        // the pending flag), the following instruction has now completed and IME
        // may be enabled. This is also why `EI; DI` leaves IME disabled.
        let was_ei = self.cpu.ei_pending;
        let cycles = self.cpu.execute(&mut self.bus);
        self.bus.step(cycles);

        // DMA holds the CPU for 160 M-cycles; drain any transfer that started
        // during this instruction (or is still in flight) before continuing.
        let mut total = cycles;
        while self.bus.dma_active() {
            self.bus.step(4);
            total += 4;
        }

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
        "gb"
    }

    fn info(&self) -> String {
        format!(
            "{} ({}, battery={})",
            self.bus.cart.title,
            self.bus.cart.mbc,
            self.bus.cart.has_battery()
        )
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
        FRAME_CYCLES
    }

    fn frame(&self) -> emu_core::Frame {
        let shades = self.framebuffer();
        emu_core::Frame {
            width: SCREEN_W,
            height: SCREEN_H,
            shades: shades.to_vec(),
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::joypad::*;

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
        assert!(emu.cpu.ime, "IME enabled after the instruction following EI");
        assert_eq!(emu.cpu.pc, 0x0102, "NOP ran; interrupt not serviced before it");

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
}