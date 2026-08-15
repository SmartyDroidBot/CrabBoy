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
    dma_source: u16,
    dma_remaining: u32,
    serial_remaining: u32,
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
            dma_source: 0,
            dma_remaining: 0,
            serial_remaining: 0,
        };
        bus.io[0x00] = 0xCF;
        bus.io[0x40] = 0x91; // post-boot LCDC: LCD on, BG+OBJ enabled
        bus
    }

    pub fn dma_active(&self) -> bool {
        self.dma_remaining > 0
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
            0xFF10..=0xFF3F => self.apu.read(addr, &self.io),
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
                    // Start a serial transfer: 8 bits at 8192 Hz (normal) or
                    // 16384 Hz (fast), i.e. 4096 or 2048 T-cycles per byte.
                    self.serial_remaining = if value & 0x01 != 0 { 2048 } else { 4096 };
                }
            }
            0xFF04 => {
                self.io[0x04] = 0;
                self.timer.on_div_write(&mut self.io);
            }
            0xFF05 => {
                self.io[0x05] = value;
                self.timer.on_tima_write();
            }
            0xFF06 => {
                self.io[0x06] = value;
                self.timer.on_tma_write(value);
            }
            0xFF07 => {
                self.io[0x07] = value;
                self.timer.on_tac_write();
            }
            0xFF0F => self.io[0x0F] = value | 0xE0,
            0xFF10..=0xFF3F => {
                let io = &mut self.io;
                self.apu.write(addr, value, io);
            }
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
        // The DMA copies 0xA0 bytes from the source page to OAM over 160
        // M-cycles, during which the CPU is held. The copy runs in `step()`.
        self.dma_source = (value as u16) << 8;
        self.dma_remaining = 160;
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

    /// Advance the clocked devices by `cycles` T-cycles.
    pub fn step(&mut self, cycles: u32) {
        // DMA: one OAM byte is transferred per M-cycle (4 T-cycles) while active.
        if self.dma_remaining > 0 {
            let transferred = self.dma_remaining;
            for _ in 0..cycles.min(transferred * 4) / 4 {
                let idx = (160 - self.dma_remaining) as usize;
                self.oam[idx] = self.read_transfer(self.dma_source as usize + idx);
                self.dma_remaining -= 1;
            }
        }

        // Serial: transfer completes when its cycle budget is exhausted.
        if self.serial_remaining > 0 {
            self.serial_remaining = self.serial_remaining.saturating_sub(cycles);
            if self.serial_remaining == 0 {
                self.io[0x01] = 0xFF; // received byte
                self.io[0x02] &= !0x80; // transfer complete
                self.io[0x0F] |= 0x08; // serial interrupt
                self.serial_buf.push(self.io[0x01]);
            }
        }

        let Bus {
            timer,
            ppu,
            apu,
            io,
            vram,
            oam,
            ..
        } = self;
        let mut wave_ram = [0u8; 16];
        wave_ram.copy_from_slice(&io[0x30..0x40]);
        timer.step(cycles, io);
        self.cart.rtc_tick(cycles);
        ppu.step(cycles, io, vram, oam);
        apu.step(cycles, io, &wave_ram);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::Cartridge;

    fn bus_with_rom() -> Bus {
        let mut rom = vec![0u8; 0x8000];
        rom[0x147] = 0x00;
        Bus::new(Cartridge::load(&rom).unwrap())
    }

    #[test]
    fn dma_copies_oam_over_160_mcycles() {
        let mut bus = bus_with_rom();
        for i in 0..0xA0 {
            bus.wram[i] = (i % 256) as u8;
        }
        bus.write(0xFF46, 0xC0); // source = 0xC000 (WRAM)
        assert!(bus.dma_active());
        bus.step(640); // 160 M-cycles * 4
        assert!(!bus.dma_active(), "DMA completes after 640 T-cycles");
        for i in 0..0xA0 {
            assert_eq!(bus.oam[i], (i % 256) as u8, "byte {i} transferred");
        }
    }

    #[test]
    fn serial_transfer_completes_and_interrupts() {
        let mut bus = bus_with_rom();
        bus.io[0x01] = 0xAB;
        bus.io[0x0F] = 0;
        bus.write(0xFF02, 0x80); // start transfer (normal speed)
        bus.step(4095);
        assert_eq!(bus.io[0x0F] & 0x08, 0, "not done yet before 4096 cycles");
        bus.step(1);
        assert_ne!(bus.io[0x0F] & 0x08, 0, "serial interrupt raised at 4096 cycles");
        assert_eq!(bus.io[0x02] & 0x80, 0, "transfer-complete bit cleared");
        assert_eq!(bus.io[0x01], 0xFF, "SB reflects received byte");
    }
}