//! GBA memory bus: address space, ROM, and wait-state timing.
//!
//! [`Bus`] implements the [`cpu::Bus`] trait the CPU decoders call. It owns the
//! full address space (BIOS, EWRAM, IWRAM, I/O, PALRAM, VRAM, OAM, cartridge
//! ROM and SRAM) and accounts for memory wait states.
//!
//! The CPU accumulates its *internal* per-instruction cycle count; the bus adds
//! the *external* wait-state cycles for each memory access into its own counter
//! ([`Bus::cycles`]), which the system root sums with the CPU count after each
//! instruction. The wait-state model is intentionally simplified for now (fixed
//! per-region values); the cartridge ROM prefetch buffer is added in a later
//! phase.

use crate::cpu::Bus as CpuBus;
use crate::dma::Dma;
use crate::io::Io;
use crate::rtc::Rtc;
use crate::timer::Timers;

/// BIOS size (16 KB).
pub const BIOS_SIZE: usize = 0x4000;
/// EWRAM size (256 KB).
pub const EWRAM_SIZE: usize = 0x40000;
/// IWRAM size (32 KB).
pub const IWRAM_SIZE: usize = 0x8000;
/// PALRAM size (1 KB).
pub const PALRAM_SIZE: usize = 0x400;
/// VRAM size (96 KB).
pub const VRAM_SIZE: usize = 0x18000;
/// OAM size (1 KB).
pub const OAM_SIZE: usize = 0x400;
/// SRAM size (64 KB).
pub const SRAM_SIZE: usize = 0x10000;

/// Default 32 MB cartridge ROM mask.
const ROM_MASK: usize = 0x1FF_FFFF;

use std::ops::{Deref, DerefMut, Index, IndexMut};

/// A fixed-size memory region, heap-allocated with no stack temporary.
///
/// Large regions (VRAM, EWRAM, save cartridges) must not be embedded inline in
/// the emulator structs: in debug builds Rust materialises a returned value as
/// a temporary on the caller's stack, and a ~670 KB `Gba` overflows the 1 MB
/// default thread stack the moment a ROM is constructed. `Box::new_zeroed`
/// allocates the `[T; N]` directly on the heap, so the region never touches the
/// stack while keeping the exact size baked into the type via const generics.
pub struct Mem<T, const N: usize>(Box<[T; N]>);

impl<T, const N: usize> Mem<T, N> {
    /// Allocate a zero-initialised region directly on the heap.
    pub fn zeroed() -> Self {
        // SAFETY: zero bytes are a valid value for any `T` we instantiate this
        // with (u8/u16), and `assume_init` hands us a fully-owned `[T; N]`.
        Mem(unsafe { Box::<[T; N]>::new_zeroed().assume_init() })
    }

    /// Allocate a region filled with a repeated byte.
    pub fn filled(v: T) -> Self
    where
        T: Copy,
    {
        let mut m = Self::zeroed();
        m.0.iter_mut().for_each(|b| *b = v);
        m
    }
}

impl<T, const N: usize> Index<usize> for Mem<T, N> {
    type Output = T;
    #[inline]
    fn index(&self, i: usize) -> &T {
        &self.0[i]
    }
}

impl<T, const N: usize> IndexMut<usize> for Mem<T, N> {
    #[inline]
    fn index_mut(&mut self, i: usize) -> &mut T {
        &mut self.0[i]
    }
}

macro_rules! impl_index_range {
    ($($r:ty),+ $(,)?) => {$(
        impl<T, const N: usize> Index<$r> for Mem<T, N> {
            type Output = [T];
            #[inline]
            fn index(&self, i: $r) -> &[T] {
                &self.0[i]
            }
        }
        impl<T, const N: usize> IndexMut<$r> for Mem<T, N> {
            #[inline]
            fn index_mut(&mut self, i: $r) -> &mut [T] {
                &mut self.0[i]
            }
        }
    )+};
}

impl_index_range!(
    std::ops::Range<usize>,
    std::ops::RangeFrom<usize>,
    std::ops::RangeTo<usize>,
    std::ops::RangeFull,
    std::ops::RangeInclusive<usize>,
);

impl<T, const N: usize> Deref for Mem<T, N> {
    type Target = [T; N];
    fn deref(&self) -> &[T; N] {
        &self.0
    }
}

impl<T, const N: usize> DerefMut for Mem<T, N> {
    fn deref_mut(&mut self) -> &mut [T; N] {
        &mut self.0
    }
}

impl<T, const N: usize> AsRef<[T]> for Mem<T, N> {
    fn as_ref(&self) -> &[T] {
        &self.0[..]
    }
}

pub use crate::save::{SaveCartridge, SaveType};

/// The memory regions the bus decodes an address into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Region {
    Bios,
    Ewram,
    Iwram,
    Io,
    Palram,
    Vram,
    Oam,
    Rom,
    Sram,
}

impl Region {
    fn of(addr: u32) -> Option<Region> {
        Some(match addr {
            0x0000_0000..=0x0000_3FFF => Region::Bios,
            0x0200_0000..=0x02FF_FFFF => Region::Ewram,
            0x0300_0000..=0x03FF_FFFF => Region::Iwram,
            0x0400_0000..=0x04FF_FFFF => Region::Io,
            0x0500_0000..=0x05FF_FFFF => Region::Palram,
            0x0600_0000..=0x06FF_FFFF => Region::Vram,
            0x0700_0000..=0x07FF_FFFF => Region::Oam,
            0x0800_0000..=0x0DFF_FFFF => Region::Rom,
            0x0E00_0000..=0x0EFF_FFFF => Region::Sram,
            _ => return None,
        })
    }
}

/// A GBA memory bus.
pub struct Bus {
    /// Optional BIOS image (executed when present; else the region reads as 0).
    pub bios: Vec<u8>,
    pub ewram: Mem<u8, EWRAM_SIZE>,
    pub iwram: Mem<u8, IWRAM_SIZE>,
    pub palram: Mem<u8, PALRAM_SIZE>,
    pub vram: Mem<u8, VRAM_SIZE>,
    pub oam: Mem<u8, OAM_SIZE>,
    /// Battery-backed save cartridge (SRAM/FLASH/EEPROM).
    pub save: SaveCartridge,
    /// Cartridge ROM.
    pub rom: Vec<u8>,
    /// I/O registers.
    pub io: Io,
    /// DMA channels.
    pub dma: Dma,
    /// Timers.
    pub timers: Timers,
    /// Real-time clock (serial I/O).
    pub rtc: Rtc,
    /// Wait-state cycles added by accesses during the current instruction.
    cycles: u32,
    /// If `true`, the previous access was a sequential ROM access (prefetch
    /// approximation, currently unused except for future refinement).
    last_seq: bool,
}

impl Bus {
    pub fn new(rom: Vec<u8>) -> Bus {
        Bus {
            bios: Vec::new(),
            ewram: Mem::zeroed(),
            iwram: Mem::zeroed(),
            palram: Mem::zeroed(),
            vram: Mem::zeroed(),
            oam: Mem::zeroed(),
            save: SaveCartridge::new(),
            rom,
            io: Io::new(),
            dma: Dma::new(),
            timers: Timers::new(),
            rtc: Rtc::new(),
            cycles: 0,
            last_seq: false,
        }
    }

    /// Install a BIOS dump (optional; enables BIOS execution and SWI traps).
    pub fn set_bios(&mut self, bios: Vec<u8>) {
        self.bios = bios;
    }

    /// Whether a full 16 KB BIOS image is loaded (as opposed to the small
    /// IRQ-return stub the skip-BIOS boot installs).
    pub fn has_real_bios(&self) -> bool {
        self.bios.len() >= 0x4000
    }

    /// Wait-state cycles accumulated during the current instruction.
    pub fn cycles(&self) -> u32 {
        self.cycles
    }

    /// Reset the per-instruction wait-state counter.
    pub fn reset_cycles(&mut self) {
        self.cycles = 0;
        self.last_seq = false;
    }

    /// Begin a new instruction: reset the wait-state counter.
    pub fn begin_step(&mut self) {
        self.reset_cycles();
    }

    /// Number of wait-state cycles for an access to `region`.
    #[inline]
    fn wait_for(&self, region: Region, width: u32) -> u32 {
        match region {
            Region::Ewram => 2,
            Region::Iwram => {
                if width == 8 {
                    1
                } else {
                    0
                }
            }
            Region::Vram | Region::Palram => 1,
            Region::Oam => 2,
            Region::Rom => {
                // Simplified cartridge timing (no prefetch yet): a 16-bit
                // access costs 1 + (N-1) where N defaults to 3 for the first
                // non-sequential access and 1 for sequential. Without the
                // prefetch buffer we approximate a steady 1-cycle sequential
                // cost per 16-bit unit.
                if width == 32 { 2 } else { 1 }
            }
            Region::Bios | Region::Sram => 1,
            Region::Io => 0,
        }
    }

    fn index_in(addr: u32, base_mask: usize) -> usize {
        (addr as usize) & base_mask
    }

    /// Read a byte across the memory map, adding wait states.
    pub fn read8(&mut self, addr: u32) -> u32 {
        let Some(region) = Region::of(addr) else {
            return 0;
        };
        self.cycles += self.wait_for(region, 8);
        self.last_seq = false;
        (match region {
            Region::Bios => self.bios.get((addr as usize) & 0x3FFF).copied().unwrap_or(0),
            Region::Ewram => self.ewram[Self::index_in(addr, EWRAM_SIZE - 1)],
            Region::Iwram => self.iwram[Self::index_in(addr, IWRAM_SIZE - 1)],
            Region::Io => {
                let off = Self::index_in(addr, 0x3FF);
                self.io.regs[off]
            }
            Region::Palram => self.palram[Self::index_in(addr, PALRAM_SIZE - 1)],
            Region::Vram => self.vram[Self::index_in(addr, VRAM_SIZE - 1)],
            Region::Oam => self.oam[Self::index_in(addr, OAM_SIZE - 1)],
            Region::Rom => self.rom[(addr as usize & ROM_MASK) % self.rom.len()],
            Region::Sram => self.save.read8(Self::index_in(addr, 0x1FFFF)),
        }) as u32
    }

    /// Read a 16-bit value across the memory map.
    pub fn read16(&mut self, addr: u32) -> u32 {
        let Some(region) = Region::of(addr) else {
            return 0;
        };
        self.cycles += self.wait_for(region, 16);
        self.last_seq = false;
        let base = (addr as usize) & !1;
        match region {
            Region::Bios => {
                let b = self.bios.get(base & 0x3FFF).copied().unwrap_or(0);
                let b2 = self.bios.get((base + 1) & 0x3FFF).copied().unwrap_or(0);
                (b as u32) | (b2 as u32) << 8
            }
            Region::Ewram => {
                let i = base & (EWRAM_SIZE - 1);
                (self.ewram[i] as u32) | (self.ewram[i + 1] as u32) << 8
            }
            Region::Iwram => {
                let i = base & (IWRAM_SIZE - 1);
                (self.iwram[i] as u32) | (self.iwram[i + 1] as u32) << 8
            }
            Region::Io => {
                let off = base & 0x3FF;
                if off == 0x120 {
                    // RTC serial data: bit 0 is the RTC output pin during reads.
                    return (self.io.read16(off) & !1) as u32 | self.rtc.read_sio_bit() as u32;
                }
                self.io.read16(off) as u32
            }
            Region::Palram => {
                let i = base & (PALRAM_SIZE - 1);
                (self.palram[i] as u32) | (self.palram[i + 1] as u32) << 8
            }
            Region::Vram => {
                let i = base & (VRAM_SIZE - 1);
                (self.vram[i] as u32) | (self.vram[i + 1] as u32) << 8
            }
            Region::Oam => {
                let i = base & (OAM_SIZE - 1);
                (self.oam[i] as u32) | (self.oam[i + 1] as u32) << 8
            }
            Region::Rom => {
                let i = (base & ROM_MASK) % self.rom.len();
                (self.rom[i] as u32) | (self.rom[i + 1] as u32) << 8
            }
            Region::Sram => {
                let i = base & 0x1FFFF;
                self.save.read16(i) as u32
            }
        }
    }

    /// Read a 32-bit value across the memory map.
    pub fn read32(&mut self, addr: u32) -> u32 {
        let lo = self.read16(addr);
        let hi = self.read16(addr.wrapping_add(2));
        lo | hi << 16
    }

    /// Write a byte across the memory map.
    pub fn write8(&mut self, addr: u32, value: u32) {
        let Some(region) = Region::of(addr) else {
            return;
        };
        self.cycles += self.wait_for(region, 8);
        self.last_seq = false;
        match region {
            Region::Ewram => self.ewram[Self::index_in(addr, EWRAM_SIZE - 1)] = value as u8,
            Region::Iwram => self.iwram[Self::index_in(addr, IWRAM_SIZE - 1)] = value as u8,
            Region::Io => {
                let off = Self::index_in(addr, 0x3FF);
                self.io.write8(off, value as u8);
            }
            Region::Palram => self.palram[Self::index_in(addr, PALRAM_SIZE - 1)] = value as u8,
            Region::Vram => self.vram[Self::index_in(addr, VRAM_SIZE - 1)] = value as u8,
            Region::Oam => self.oam[Self::index_in(addr, OAM_SIZE - 1)] = value as u8,
            Region::Sram => self.save.write8(Self::index_in(addr, 0x1FFFF), value as u8),
            Region::Rom | Region::Bios => {}
        }
    }

    /// Write a 16-bit value across the memory map.
    pub fn write16(&mut self, addr: u32, value: u32) {
        let Some(region) = Region::of(addr) else {
            return;
        };
        self.cycles += self.wait_for(region, 16);
        self.last_seq = false;
        let base = (addr as usize) & !1;
        match region {
            Region::Ewram => {
                let i = base & (EWRAM_SIZE - 1);
                self.ewram[i] = value as u8;
                self.ewram[i + 1] = (value >> 8) as u8;
            }
            Region::Iwram => {
                let i = base & (IWRAM_SIZE - 1);
                self.iwram[i] = value as u8;
                self.iwram[i + 1] = (value >> 8) as u8;
            }
            Region::Io => {
                let off = base & 0x3FF;
                self.io.write16(off, value as u16);
                match off {
                    0xB0..=0xDF => self.dma.write16(off, value as u16),
                    0x100..=0x110 => self.write_timer(off, value as u16),
                    0x120 | 0x122 => self.rtc.write_sio(value as u16),
                    _ => {}
                }
            }
            Region::Palram => {
                let i = base & (PALRAM_SIZE - 1);
                self.palram[i] = value as u8;
                self.palram[i + 1] = (value >> 8) as u8;
            }
            Region::Vram => {
                let i = base & (VRAM_SIZE - 1);
                self.vram[i] = value as u8;
                self.vram[i + 1] = (value >> 8) as u8;
            }
            Region::Oam => {
                let i = base & (OAM_SIZE - 1);
                self.oam[i] = value as u8;
                self.oam[i + 1] = (value >> 8) as u8;
            }
            Region::Sram => {
                let i = base & 0x1FFFF;
                self.save.write16(i, value as u16);
            }
            Region::Rom | Region::Bios => {}
        }
    }

    /// Write a 32-bit value across the memory map.
    pub fn write32(&mut self, addr: u32, value: u32) {
        self.write16(addr, value);
        self.write16(addr.wrapping_add(2), value >> 16);
    }

    /// Current keypad state bits (active low).
    pub fn press(&mut self, k: u16) {
        self.io.press(k);
    }

    pub fn release(&mut self, k: u16) {
        self.io.release(k);
    }

    /// Forward a timer register write.
    fn write_timer(&mut self, offset: usize, value: u16) {
        match offset {
            0x100 => self.timers.write_cnt_l(0, value),
            0x102 => self.timers.write_cnt_h(0, value),
            0x104 => self.timers.write_cnt_l(1, value),
            0x106 => self.timers.write_cnt_h(1, value),
            0x108 => self.timers.write_cnt_l(2, value),
            0x10A => self.timers.write_cnt_h(2, value),
            0x10C => self.timers.write_cnt_l(3, value),
            0x10E => self.timers.write_cnt_h(3, value),
            _ => {}
        }
    }

    /// Raise timer/DMA overflow IRQs into the IO flags.
    pub fn sync_dev_irq(&mut self) {
        self.io.raise_irq(self.timers.irq_flags());
        self.io.raise_irq(self.dma.irq_flags());
    }

    /// Run any enabled DMA channel whose start timing matches `timing`.
    pub fn run_dma(&mut self, timing: crate::dma::Timing) {
        let chans = self.dma.chans;
        let mut flags = self.dma.flags;
        for (i, mut ch) in chans.iter().copied().enumerate() {
            if !ch.enabled || ch.done {
                continue;
            }
            if ch.timing() != timing {
                continue;
            }
            if timing == crate::dma::Timing::Special {
                continue;
            }
            let unit = ch.unit_32();
            let ub = if unit { 4u32 } else { 2u32 };
            let sa = ch.src_adjust();
            let da = ch.dst_adjust();
            let mut s = ch.src;
            let mut d = ch.dst;
            for _ in 0..ch.count as usize {
                if unit {
                    let v = self.read32(s);
                    self.write32(d, v);
                } else {
                    let v = self.read16(s);
                    self.write16(d, v);
                }
                s = crate::dma::adjust(s, ub, sa);
                d = crate::dma::adjust(d, ub, da);
            }
            if ch.irq_enable() {
                flags |= 1 << (4 + i);
            }
            if !ch.repeat() {
                ch.enabled = false;
            }
            ch.src = s;
            ch.dst = d;
            ch.done = !ch.enabled;
            self.dma.chans[i] = ch;
        }
        self.dma.flags = flags;
    }

    /// Pending interrupt flags (IF & IE).
    pub fn pending_irq(&self) -> u16 {
        self.io.pending_irq()
    }
}

impl CpuBus for Bus {
    fn read8(&mut self, addr: u32) -> u32 {
        Bus::read8(self, addr)
    }
    fn read16(&mut self, addr: u32) -> u32 {
        Bus::read16(self, addr)
    }
    fn read32(&mut self, addr: u32) -> u32 {
        Bus::read32(self, addr)
    }
    fn write8(&mut self, addr: u32, value: u32) {
        Bus::write8(self, addr, value)
    }
    fn write16(&mut self, addr: u32, value: u32) {
        Bus::write16(self, addr, value)
    }
    fn write32(&mut self, addr: u32, value: u32) {
        Bus::write32(self, addr, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::key;

    fn bus() -> Bus {
        Bus::new(vec![0; 0x4000])
    }

    #[test]
    fn iwram_round_trip() {
        let mut b = bus();
        b.write32(0x0300_0000, 0xDEADBEEF);
        assert_eq!(b.read32(0x0300_0000), 0xDEADBEEF);
        b.write16(0x0300_0004, 0x1234);
        assert_eq!(b.read16(0x0300_0004), 0x1234);
        b.write8(0x0300_0006, 0xAB);
        assert_eq!(b.read8(0x0300_0006), 0xAB);
    }

    #[test]
    fn ewram_mirrors() {
        let mut b = bus();
        b.write16(0x0200_0000, 0x1234);
        assert_eq!(b.read16(0x0200_0000), 0x1234);
        // Mirror within EWRAM range.
        assert_eq!(b.read16(0x0200_0000 + EWRAM_SIZE as u32), 0x1234);
    }

    #[test]
    fn rom_read_and_mirror() {
        let mut b = Bus::new(vec![0x11, 0x22, 0x33, 0x44]);
        assert_eq!(b.read8(0x0800_0000), 0x11);
        assert_eq!(b.read16(0x0800_0000), 0x2211);
        assert_eq!(b.read32(0x0800_0000), 0x4433_2211);
        // 32 MB mirroring wraps into the 4-byte ROM.
        assert_eq!(b.read8(0x0900_0000), 0x11);
    }

    #[test]
    fn keypad_active_low() {
        let mut b = bus();
        assert_eq!(b.read16(0x0400_0130) as u16 & key::MASK, key::MASK);
        b.press(key::A);
        assert_eq!(b.read16(0x0400_0130) as u16 & key::A, 0);
        b.release(key::A);
        assert_eq!(b.read16(0x0400_0130) as u16 & key::A, key::A);
    }

    #[test]
    fn keypad_irq_and_if_clear() {
        let mut b = bus();
        // Enable keypad IRQ in OR mode, watch A.
        b.write16(0x0400_0132, (1 << 14) as u32 | key::A as u32);
        b.press(key::A);
        assert_ne!(b.io.iflags() & crate::io::IRQ_KEYPAD, 0);
        // Without IE the flag is set but nothing is pending.
        assert_eq!(b.pending_irq() & crate::io::IRQ_KEYPAD, 0);
        // Enable the keypad IRQ in IE, then it becomes pending.
        b.write16(0x0400_0200, crate::io::IRQ_KEYPAD as u32);
        assert_ne!(b.pending_irq() & crate::io::IRQ_KEYPAD, 0);
        // Acknowledge: writing 1 to IF clears.
        b.write16(0x0400_0202, crate::io::IRQ_KEYPAD as u32);
        assert_eq!(b.io.iflags() & crate::io::IRQ_KEYPAD, 0);
    }

    #[test]
    fn wait_states_accumulate() {
        let mut b = bus();
        b.begin_step();
        b.read32(0x0300_0000); // IWRAM: 0 wait (word)
        b.read32(0x0200_0000); // EWRAM: 2 wait per 16-bit half => 4 total
        assert_eq!(b.cycles(), 4);
    }

    #[test]
    fn unimplemented_region_reads_zero() {
        let mut b = bus();
        assert_eq!(b.read8(0x0FFF_0000), 0);
        assert_eq!(b.read32(0x0FFF_0000), 0);
    }

    #[test]
    fn cpu_executes_program_from_iwram() {
        let mut b = bus();
        let mut cpu = crate::Cpu::new();
        // MOV r0, #0; ADD r1, r0, #5; B back to 0x03000000 (all ARM).
        b.write32(0x0300_0000, 0xE3A0_0000);
        b.write32(0x0300_0004, 0xE280_1005);
        b.write32(0x0300_0008, 0xEAFF_FFFD);
        cpu.set_pc(0x0300_0000);
        cpu.set_cpsr(0x13); // SVC mode, ARM.
        b.begin_step();
        cpu.execute(&mut b);
        b.begin_step();
        cpu.execute(&mut b);
        assert_eq!(cpu.reg_raw(1), 5);
        assert_eq!(cpu.reg_raw(0), 0);
        // Both instructions consumed internal cycles (CPU returns the count).
        assert!(cpu.execute(&mut b) > 0);
    }
}
