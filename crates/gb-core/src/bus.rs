//! Game Boy memory bus.
//!
//! Routes reads/writes across the full 16-bit address space and drives the
//! clocked devices (timer, PPU). This type implements [`emu_core::Bus`] so it
//! can be driven generically; the CPU executes against it as the concrete
//! `gb_core::bus::Bus`.

use crate::cartridge::Cartridge;
use crate::devices::joypad::Joypad;
use crate::devices::ppu::Ppu;
use crate::devices::timer::Timer;

pub const INTERRUPT_VBLANK: u8 = 0x01;
pub const INTERRUPT_LCD: u8 = 0x02;
pub const INTERRUPT_TIMER: u8 = 0x04;
pub const INTERRUPT_SERIAL: u8 = 0x08;
pub const INTERRUPT_JOYPAD: u8 = 0x10;

pub struct Bus {
    pub cart: Cartridge,
    pub wram: [u8; 0x2000],
    pub vram: [u8; 0x2000],
    pub oam: [u8; 0xA0],
    pub io: [u8; 0x80],
    pub hram: [u8; 0x80],
    pub ie: u8,
    pub joypad: Joypad,
    pub ppu: Ppu,
    pub timer: Timer,
    pub apu: crate::devices::apu::Apu,
    pub serial_buf: Vec<u8>,
}

impl Bus {
    pub fn new(cart: Cartridge) -> Bus {
        let mut bus = Bus {
            cart,
            wram: [0; 0x2000],
            vram: [0; 0x2000],
            oam: [0; 0xA0],
            io: [0; 0x80],
            hram: [0; 0x80],
            ie: 0x00,
            joypad: Joypad::new(),
            ppu: Ppu::new(),
            timer: Timer::new(),
            apu: crate::devices::apu::Apu::new(),
            serial_buf: Vec::new(),
        };
        bus.io[0x00] = 0xCF;
        bus
    }

    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x7FFF => self.cart.read(addr),
            0x8000..=0x9FFF => self.vram[(addr - 0x8000) as usize],
            0xA000..=0xBFFF => self.cart.read_ram(addr),
            0xC000..=0xDFFF => self.wram[(addr - 0xC000) as usize],
            0xE000..=0xFDFF => self.wram[(addr - 0xE000) as usize],
            0xFE00..=0xFE9F => self.oam[(addr - 0xFE00) as usize],
            0xFEA0..=0xFEFF => 0x00,
            0xFF00 => self.joypad.read(self.io[0x00]),
            0xFF01 => self.io[0x01],
            0xFF02 => self.io[0x02] | 0x7E,
            0xFF04..=0xFF07 => self.io[(addr - 0xFF00) as usize],
            0xFF0F => self.io[0x0F] | 0xE0,
            0xFF10..=0xFF3F => 0x00,
            0xFF40..=0xFF7F => self.io[(addr - 0xFF00) as usize],
            0xFF80..=0xFFFE => self.hram[(addr - 0xFF80) as usize],
            0xFFFF => self.ie,
            _ => 0x00,
        }
    }

    pub fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x7FFF => self.cart.write(addr, value),
            0x8000..=0x9FFF => self.vram[(addr - 0x8000) as usize] = value,
            0xA000..=0xBFFF => self.cart.write_ram(addr, value),
            0xC000..=0xDFFF => self.wram[(addr - 0xC000) as usize] = value,
            0xE000..=0xFDFF => self.wram[(addr - 0xE000) as usize] = value,
            0xFE00..=0xFE9F => self.oam[(addr - 0xFE00) as usize] = value,
            0xFEA0..=0xFEFF => {}
            0xFF00 => self.io[0x00] = value | 0xC0,
            0xFF01 => self.io[0x01] = value,
            0xFF02 => {
                self.io[0x02] = value;
                if value & 0x80 != 0 {
                    self.serial_buf.push(self.io[0x01]);
                }
            }
            0xFF04 => {
                self.io[0x04] = 0;
                self.timer.div_counter = 0;
            }
            0xFF05 => {
                self.io[0x05] = value;
                self.timer.on_tima_write();
            }
            0xFF06 => self.io[0x06] = value,
            0xFF07 => {
                self.io[0x07] = value;
                self.timer.on_tac_write();
            }
            0xFF0F => self.io[0x0F] = value | 0xE0,
            0xFF10..=0xFF3F => {}
            0xFF46 => self.dma(value),
            0xFF44 => { /* LY is read-only on real hardware; writes ignored */ }
            0xFF40..=0xFF4B => self.io[(addr - 0xFF00) as usize] = value,
            0xFF4C..=0xFF7F => self.io[(addr - 0xFF00) as usize] = value,
            0xFF80..=0xFFFE => self.hram[(addr - 0xFF80) as usize] = value,
            0xFFFF => self.ie = value,
            _ => {}
        }
    }

    fn dma(&mut self, value: u8) {
        self.io[0x46] = value;
        let src = (value as usize) << 8;
        for i in 0..0xA0 {
            self.oam[i] = self.read_transfer(src + i);
        }
    }

    fn read_transfer(&self, addr: usize) -> u8 {
        match addr {
            0x0000..=0x7FFF => self.cart.read(addr as u16),
            0x8000..=0x9FFF => self.vram[addr - 0x8000],
            0xA000..=0xBFFF => self.cart.read_ram(addr as u16),
            0xC000..=0xDFFF => self.wram[addr - 0xC000],
            0xE000..=0xFDFF => self.wram[addr - 0xE000],
            _ => 0,
        }
    }

    /// Advance the clocked devices by `cycles`.
    pub fn step(&mut self, cycles: u32) {
        let Bus {
            timer,
            ppu,
            io,
            vram,
            oam,
            ..
        } = self;
        timer.step(cycles, io);
        ppu.step(cycles, io, vram, oam);
    }

    pub fn frame(&self) -> &[u8] {
        &self.ppu.frame_buffer
    }
}

impl emu_core::Addressable for Bus {
    fn read(&self, addr: u32) -> u8 {
        self.read(addr as u16)
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.write(addr as u16, value)
    }
}

impl emu_core::Bus for Bus {
    fn tick(&mut self, cycles: u32) {
        self.step(cycles);
    }

    fn request_interrupt(&mut self, bit: u32) {
        if bit < 5 {
            self.io[0x0F] |= 1 << bit;
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}