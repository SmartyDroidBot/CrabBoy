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

/// CGB boot-ROM default compatibility palette (combination index 0, used for
/// monochrome games that are not in the boot ROM's title database):
/// OBJ palettes 0 and 1 = palette 4, BG palette 0 = palette 29.
const BOOT_PAL_OBJ: [u8; 8] = [0x7F, 0xFF, 0x1F, 0x42, 0xF2, 0x1C, 0x00, 0x00];
const BOOT_PAL_BG: [u8; 8] = [0x7F, 0xFF, 0xEF, 0x1B, 0x80, 0x61, 0x00, 0x00];

pub struct Bus {
    pub cart: Cartridge,
    /// 8 banks of 8 KiB WRAM (0x8000 bytes). Bank 0 is always at C000–CFFF;
    /// D000–DFFF selects bank 1–7 via SVBK (0xFF70).
    pub wram: [u8; 0x8000],
    /// 2 banks of 8 KiB VRAM (0x4000 bytes); VBK (0xFF4F) selects the bank the
    /// CPU reads/writes. The PPU reads both banks.
    pub vram: [u8; 0x4000],
    pub oam: [u8; 0xA0],
    pub io: [u8; 0x80],
    pub hram: [u8; 0x80],
    pub ie: u8,
    pub joypad: Joypad,
    pub ppu: Ppu,
    pub timer: Timer,
    pub apu: crate::devices::apu::Apu,
    pub serial_buf: Vec<u8>,
    /// Whether the loaded cartridge requests CGB (color) rendering.
    pub is_cgb: bool,
    /// Whether the CPU is in double-speed mode (8.39 MHz).
    pub double_speed: bool,
    /// Carry accumulator for halving device cycles in double-speed mode.
    pub(crate) dev_accum: u32,
    pub(crate) dma_source: u16,
    pub(crate) dma_remaining: u32,
    pub(crate) serial_remaining: u32,
    /// The byte in SB when the transfer started; it is what the other end
    /// (and `serial_buf`) receives.
    pub(crate) serial_out: u8,
    /// HDMA (CGB) state.
    pub(crate) hdma_active: bool,
    pub(crate) hdma_hblank: bool,
    pub(crate) hdma_len: u16,
    pub(crate) hdma_src: usize,
    pub(crate) hdma_dst: usize,
    pub(crate) hdma_done_this_hblank: bool,
}

/// The console a cartridge runs on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Model {
    /// Game Boy (DMG).
    Dmg,
    /// Game Boy Color, including its DMG compatibility mode for plain carts.
    Cgb,
}

impl Model {
    /// The model the cartridge header asks for.
    pub fn for_cart(cart: &Cartridge) -> Model {
        if cart.is_cgb() {
            Model::Cgb
        } else {
            Model::Dmg
        }
    }
}

impl Bus {
    pub fn new(cart: Cartridge) -> Bus {
        let model = Model::for_cart(&cart);
        Bus::new_with_model(cart, model)
    }

    pub fn new_with_model(cart: Cartridge, model: Model) -> Bus {
        let is_cgb = model == Model::Cgb;
        let mut bus = Bus {
            cart,
            wram: [0; 0x8000],
            vram: [0; 0x4000],
            oam: [0; 0xA0],
            io: [0; 0x80],
            hram: [0; 0x80],
            ie: 0x00,
            joypad: Joypad::new(),
            ppu: Ppu::new(),
            timer: Timer::new(),
            apu: crate::devices::apu::Apu::new(),
            serial_buf: Vec::new(),
            is_cgb,
            double_speed: false,
            dev_accum: 0,
            dma_source: 0,
            dma_remaining: 0,
            serial_remaining: 0,
            serial_out: 0,
            hdma_active: false,
            hdma_hblank: false,
            hdma_len: 0,
            hdma_src: 0,
            hdma_dst: 0,
            hdma_done_this_hblank: false,
        };
        bus.io[0x00] = 0xCF;
        bus.io[0x40] = 0x91; // post-boot LCDC: LCD on, BG+OBJ enabled
        bus.ppu.cgb = is_cgb;
        // Simulate the CGB boot ROM's default palette upload for monochrome
        // games (BG palette 0 / OBJ palettes 0–1); harmless for CGB carts too,
        // which overwrite them.
        for p in 0..8 {
            bus.ppu.bg_pal[p * 8..p * 8 + 8].copy_from_slice(&BOOT_PAL_BG);
            bus.ppu.obj_pal[p * 8..p * 8 + 8].copy_from_slice(&BOOT_PAL_OBJ);
        }
        bus
    }

    pub fn dma_active(&self) -> bool {
        self.dma_remaining > 0
    }

    /// Offset into `wram` for a CPU access to C000–FDFF. Bank 0 always covers
    /// C000–CFFF; D000–DFFF selects SVBK bank 1–7 (SVBK value 0 acts as bank
    /// 1). E000–FDFF mirrors C000–DFFF.
    fn wram_offset(&self, addr: u16) -> usize {
        let a = if addr >= 0xE000 { addr - 0x2000 } else { addr };
        if a < 0xD000 {
            (a - 0xC000) as usize
        } else {
            let s = self.io[0x70] & 0x07;
            let bank = if s == 0 { 1 } else { s as usize };
            bank * 0x1000 + (a - 0xD000) as usize
        }
    }

    fn vram_offset(&self, addr: u16) -> usize {
        (self.io[0x4F] as usize & 1) * 0x2000 + (addr - 0x8000) as usize
    }

    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x7FFF => self.cart.read(addr),
            0x8000..=0x9FFF => self.vram[self.vram_offset(addr)],
            0xA000..=0xBFFF => self.cart.read_ram(addr),
            0xC000..=0xDFFF => self.wram[self.wram_offset(addr)],
            0xE000..=0xFDFF => self.wram[self.wram_offset(addr)],
            0xFE00..=0xFE9F => self.oam[(addr - 0xFE00) as usize],
            0xFEA0..=0xFEFF => 0x00,
            0xFF00 => self.joypad.read(self.io[0x00]),
            0xFF01 => self.io[0x01],
            0xFF02 => self.io[0x02] | 0x7E,
            0xFF04..=0xFF07 => self.io[(addr - 0xFF00) as usize],
            0xFF0F => self.io[0x0F] | 0xE0,
            0xFF10..=0xFF3F => self.apu.read(addr, &self.io),
            0xFF4D => {
                let speed = if self.double_speed { 0x80 } else { 0 };
                (self.io[0x4D] & 0x01) | speed
            }
            0xFF68..=0xFF6B => self.ppu.read_pal_reg(addr, &self.io),
            0xFF70 => self.io[0x70] | 0xF8,
            0xFF40..=0xFF7F => self.io[(addr - 0xFF00) as usize],
            0xFF80..=0xFFFE => self.hram[(addr - 0xFF80) as usize],
            0xFFFF => self.ie,
            _ => 0x00,
        }
    }

    pub fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x7FFF => self.cart.write(addr, value),
            0x8000..=0x9FFF => {
                let off = self.vram_offset(addr);
                self.vram[off] = value;
            }
            0xA000..=0xBFFF => self.cart.write_ram(addr, value),
            0xC000..=0xDFFF => {
                let off = self.wram_offset(addr);
                self.wram[off] = value;
            }
            0xE000..=0xFDFF => {
                let off = self.wram_offset(addr);
                self.wram[off] = value;
            }
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
                    self.serial_out = self.io[0x01];
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
            0xFF4D => self.io[0x4D] = value & 0x01,
            0xFF4F => self.io[0x4F] = value & 0x01,
            0xFF51..=0xFF54 => self.io[(addr - 0xFF00) as usize] = value,
            0xFF55 => self.hdma_start(value),
            0xFF68 => self.io[0x68] = value,
            0xFF69 => self.ppu.write_bg_pal(value, &mut self.io),
            0xFF6A => self.io[0x6A] = value,
            0xFF6B => self.ppu.write_obj_pal(value, &mut self.io),
            0xFF70 => self.io[0x70] = value & 0x07,
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
            0x8000..=0x9FFF => {
                let bank = (self.io[0x4F] as usize & 1) * 0x2000;
                self.vram[bank + addr - 0x8000]
            }
            0xA000..=0xBFFF => self.cart.read_ram(addr as u16),
            0xC000..=0xFDFF => self.wram[self.wram_offset(addr as u16)],
            _ => 0,
        }
    }

    /// Start an HDMA transfer (write to 0xFF55). General-purpose transfers
    /// (bit 7 clear) and transfers started while the LCD is off complete
    /// immediately; HBlank DMA (bit 7 set, LCD on) copies 0x10 bytes per line
    /// in [`Bus::step`].
    fn hdma_start(&mut self, value: u8) {
        self.io[0x55] = value;
        self.hdma_src = ((self.io[0x51] as usize) << 8 | self.io[0x52] as usize) & 0xFFF0;
        self.hdma_dst =
            (((self.io[0x53] as usize) << 8 | self.io[0x54] as usize) & 0x1FF0) + 0x8000;
        let len = ((value & 0x7F) as usize + 1) * 0x10;
        let lcd_on = self.io[0x40] & 0x80 != 0;
        if value & 0x80 == 0 || !lcd_on {
            self.hdma_transfer(len);
            self.io[0x55] = 0xFF;
            self.hdma_active = false;
            self.hdma_hblank = false;
        } else {
            self.hdma_active = true;
            self.hdma_hblank = true;
            self.hdma_len = len as u16;
            self.hdma_done_this_hblank = false;
        }
    }

    fn hdma_transfer(&mut self, len: usize) {
        for i in 0..len {
            let byte = self.read_transfer(self.hdma_src + i);
            let dst = self.hdma_dst + i;
            if dst <= 0x9FFF {
                // HDMA always writes VRAM bank 0.
                self.vram[dst - 0x8000] = byte;
            }
        }
        self.hdma_src += len;
        self.hdma_dst += len;
    }

    fn hdma_step(&mut self) {
        if !self.hdma_active || !self.hdma_hblank {
            return;
        }
        if self.ppu.mode != 0 {
            self.hdma_done_this_hblank = false;
            return;
        }
        if self.hdma_done_this_hblank {
            return;
        }
        self.hdma_done_this_hblank = true;
        let n = self.hdma_len.min(0x10) as usize;
        self.hdma_transfer(n);
        self.hdma_len -= n as u16;
        if self.hdma_len == 0 {
            self.io[0x55] = 0xFF;
            self.hdma_active = false;
            self.hdma_hblank = false;
        }
    }

    /// Number of master cycles in one full video frame.
    pub fn frame_cycles(&self) -> u32 {
        if self.double_speed {
            140448
        } else {
            70224
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
                // No link partner: the transmitted byte leaves through
                // `serial_buf` and $FF shifts in.
                self.serial_buf.push(self.serial_out);
                self.io[0x01] = 0xFF;
                self.io[0x02] &= !0x80; // transfer complete
                self.io[0x0F] |= 0x08; // serial interrupt
            }
        }

        // The LCD, timer, and RTC are clocked at fixed absolute rates, so in
        // double-speed mode they receive half the raw cycles (with carry). The
        // APU keeps the raw clock so its sample rate doubles (8192 -> 16384).
        let mut dev = cycles;
        if self.double_speed {
            self.dev_accum += cycles;
            dev = self.dev_accum / 2;
            self.dev_accum -= dev * 2;
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
        timer.step(dev, io);
        self.cart.rtc_tick(dev);
        ppu.step(dev, io, vram, oam);
        apu.step(cycles, io, &wave_ram);
        self.hdma_step();
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
        assert_ne!(
            bus.io[0x0F] & 0x08,
            0,
            "serial interrupt raised at 4096 cycles"
        );
        assert_eq!(bus.io[0x02] & 0x80, 0, "transfer-complete bit cleared");
        assert_eq!(bus.io[0x01], 0xFF, "SB reflects received byte");
    }
}
