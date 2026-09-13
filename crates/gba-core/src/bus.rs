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

/// Map a VRAM address (or offset) onto the 96 KB array. VRAM is mirrored
/// every 128 KB, and within a mirror the upper 32 KB (0x18000-0x1FFFF)
/// repeats the OBJ half (0x10000-0x17FFF). The size is not a power of two,
/// so a plain size mask would drop address bit 15 and alias the upper
/// character and screen blocks onto the first 32 KB.
#[inline]
pub fn vram_index(addr: usize) -> usize {
    let a = addr & 0x1_FFFF;
    if a >= VRAM_SIZE {
        a - 0x8000
    } else {
        a
    }
}

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
    /// Top 16 MB of the cartridge space: the EEPROM chip select on carts
    /// that have one, otherwise a ROM mirror.
    Eeprom,
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
            0x0800_0000..=0x0CFF_FFFF => Region::Rom,
            0x0D00_0000..=0x0DFF_FFFF => Region::Eeprom,
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
    /// Last value driven onto the bus; unmapped reads return it (open bus).
    pub(crate) open_bus: u32,
    /// Wait-state cycles added by accesses during the current instruction.
    cycles: u32,
    /// If `true`, the previous access was a sequential ROM access (prefetch
    /// approximation, currently unused except for future refinement).
    last_seq: bool,
    /// When true, record every RAM write to `ram_write_log` (diagnostics).
    #[cfg(feature = "trace")]
    pub log_ram_writes: bool,
    /// Recent RAM writes as `(addr, width, value)` (diagnostics).
    #[cfg(feature = "trace")]
    pub ram_write_log: Vec<(u32, u8, u32)>,
}

impl Bus {
    pub fn new(rom: Vec<u8>) -> Bus {
        let mut save = SaveCartridge::new();
        let kind = SaveCartridge::detect_from_rom(&rom);
        if kind != SaveType::None {
            save.set_kind(kind);
        }
        Bus {
            bios: Vec::new(),
            ewram: Mem::zeroed(),
            iwram: Mem::zeroed(),
            palram: Mem::zeroed(),
            vram: Mem::zeroed(),
            oam: Mem::zeroed(),
            save,
            rom,
            io: Io::new(),
            dma: Dma::new(),
            timers: Timers::new(),
            rtc: Rtc::new(),
            open_bus: 0,
            cycles: 0,
            last_seq: false,
            #[cfg(feature = "trace")]
            log_ram_writes: false,
            #[cfg(feature = "trace")]
            ram_write_log: Vec::new(),
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

    #[cfg(feature = "trace")]
    #[inline]
    fn trace_ram_write(&mut self, addr: u32, width: u8, value: u32) {
        if self.log_ram_writes {
            self.ram_write_log.push((addr, width, value));
        }
    }

    #[cfg(not(feature = "trace"))]
    #[inline(always)]
    fn trace_ram_write(&mut self, _addr: u32, _width: u8, _value: u32) {}

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
            Region::Rom | Region::Eeprom => {
                // Wait-state 0 timing from WAITCNT (no prefetch buffer yet):
                // a 16-bit access costs the non-sequential count, a 32-bit
                // access adds the sequential count for its second halfword.
                let waitcnt = u16::from_le_bytes([self.io.regs[0x204], self.io.regs[0x205]]);
                let ws0_n = [4, 3, 2, 8][((waitcnt >> 2) & 3) as usize];
                let ws0_s = if waitcnt & (1 << 4) != 0 { 1 } else { 2 };
                if width == 32 {
                    ws0_n + ws0_s
                } else {
                    ws0_n
                }
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
            return self.open_bus & 0xFF;
        };
        self.cycles += self.wait_for(region, 8);
        self.last_seq = false;
        let v = (match region {
            Region::Bios => self
                .bios
                .get((addr as usize) & 0x3FFF)
                .copied()
                .unwrap_or(0),
            Region::Ewram => self.ewram[Self::index_in(addr, EWRAM_SIZE - 1)],
            Region::Iwram => self.iwram[Self::index_in(addr, IWRAM_SIZE - 1)],
            Region::Io => {
                // Byte reads of the special registers (IE/IF/IME/VCOUNT,
                // timer counters) must see the live values, not the raw
                // backing bytes.
                let off = Self::index_in(addr, 0x3FF);
                let v = self.io_read16(off & !1);
                (v >> ((off & 1) * 8)) as u8
            }
            Region::Palram => self.palram[Self::index_in(addr, PALRAM_SIZE - 1)],
            Region::Vram => self.vram[vram_index(addr as usize)],
            Region::Oam => self.oam[Self::index_in(addr, OAM_SIZE - 1)],
            Region::Rom => self.rom_byte(addr as usize & ROM_MASK),
            Region::Eeprom => match self.save.eeprom_read_bit() {
                Some(bit) => bit,
                None => self.rom_byte(addr as usize & ROM_MASK),
            },
            Region::Sram => self.save.read8(Self::index_in(addr, 0x1FFFF)),
        }) as u32;
        self.open_bus = v;
        v
    }

    #[inline]
    fn rom_byte(&self, off: usize) -> u8 {
        if self.rom.is_empty() {
            0
        } else {
            self.rom[off % self.rom.len()]
        }
    }

    #[inline]
    fn rom_half(&self, off: usize) -> u32 {
        (self.rom_byte(off) as u32) | (self.rom_byte(off + 1) as u32) << 8
    }

    /// Read an aligned 16-bit I/O register, routing to the device that owns
    /// it.
    fn io_read16(&self, off: usize) -> u32 {
        match off {
            0xB0..=0xDF => self.dma.read16(off) as u32,
            0x100 => self.timers.read_cnt_l(0) as u32,
            0x104 => self.timers.read_cnt_l(1) as u32,
            0x108 => self.timers.read_cnt_l(2) as u32,
            0x10C => self.timers.read_cnt_l(3) as u32,
            // RTC serial data: bit 0 is the RTC output pin during reads.
            0x120 => (self.io.read16(off) & !1) as u32 | self.rtc.read_sio_bit() as u32,
            _ => self.io.read16(off) as u32,
        }
    }

    /// Read a 16-bit value across the memory map.
    /// Read a 16-bit value across the memory map. An odd address returns the
    /// aligned halfword rotated right by 8 within 32 bits, as the ARM7TDMI
    /// does for LDRH.
    pub fn read16(&mut self, addr: u32) -> u32 {
        let Some(region) = Region::of(addr) else {
            return self.open_bus;
        };
        self.cycles += self.wait_for(region, 16);
        self.last_seq = false;
        let base = (addr as usize) & !1;
        let hw = match region {
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
            Region::Io => self.io_read16(base & 0x3FF),
            Region::Palram => {
                let i = base & (PALRAM_SIZE - 1);
                (self.palram[i] as u32) | (self.palram[i + 1] as u32) << 8
            }
            Region::Vram => {
                let i = vram_index(base);
                (self.vram[i] as u32) | (self.vram[i + 1] as u32) << 8
            }
            Region::Oam => {
                let i = base & (OAM_SIZE - 1);
                (self.oam[i] as u32) | (self.oam[i + 1] as u32) << 8
            }
            Region::Rom => self.rom_half(base & ROM_MASK),
            Region::Eeprom => match self.save.eeprom_read_bit() {
                Some(bit) => bit as u32,
                None => self.rom_half(base & ROM_MASK),
            },
            Region::Sram => {
                let i = base & 0x1FFFF;
                self.save.read16(i) as u32
            }
        };
        let v = if addr & 1 != 0 {
            hw.rotate_right(8)
        } else {
            hw
        };
        self.open_bus = v;
        v
    }

    /// Read a 32-bit value across the memory map. An unaligned address reads
    /// the aligned word rotated right by 8 bits per byte of misalignment.
    pub fn read32(&mut self, addr: u32) -> u32 {
        if Region::of(addr).is_none() {
            return self.open_bus;
        }
        let aligned = addr & !3;
        let lo = self.read16(aligned);
        let hi = self.read16(aligned.wrapping_add(2));
        let word = lo | hi << 16;
        let v = word.rotate_right((addr & 3) * 8);
        self.open_bus = v;
        v
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
            Region::Iwram => {
                self.trace_ram_write(addr, 1, value);
                self.iwram[Self::index_in(addr, IWRAM_SIZE - 1)] = value as u8
            }
            Region::Io => {
                let off = Self::index_in(addr, 0x3FF);
                if off == 0x301 {
                    // HALTCNT: bit 7 clear = HALT, set = STOP. Both park the
                    // CPU until an enabled interrupt arrives.
                    self.io.halt_requested = true;
                } else {
                    self.io.write8(off, value as u8);
                }
            }
            Region::Palram => self.palram[Self::index_in(addr, PALRAM_SIZE - 1)] = value as u8,
            Region::Vram => self.vram[vram_index(addr as usize)] = value as u8,
            Region::Oam => self.oam[Self::index_in(addr, OAM_SIZE - 1)] = value as u8,
            Region::Sram => self.save.write8(Self::index_in(addr, 0x1FFFF), value as u8),
            Region::Eeprom => self.save.eeprom_write_bit(value as u8),
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
                self.trace_ram_write(base as u32, 2, value);
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
                let i = vram_index(base);
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
            Region::Eeprom => self.save.eeprom_write_bit(value as u8),
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
        // Edge-triggered: each device flag is moved into IF exactly once, so
        // writing 1 to IF acknowledges it for good.
        let flags = self.timers.take_irq() | self.dma.take_irq();
        if flags != 0 {
            self.io.raise_irq(flags);
        }
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
            // A DMA to or from the EEPROM chip select reveals the part size
            // through its length (9/73 halfwords = 512 B, 17/81 = 8 KB).
            if i == 3 && matches!(Region::of(ch.cur_dst), Some(Region::Eeprom))
                || matches!(Region::of(ch.cur_src), Some(Region::Eeprom))
            {
                self.save.eeprom.set_size_from_dma_count(ch.cur_count);
            }
            let unit = ch.unit_32();
            let ub = if unit { 4u32 } else { 2u32 };
            let sa = ch.src_adjust();
            let da = ch.dst_adjust();
            // The low address bits are ignored for the selected unit size.
            let mut s = ch.cur_src & !(ub - 1);
            let mut d = ch.cur_dst & !(ub - 1);
            for _ in 0..ch.cur_count {
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
                flags |= 1 << (8 + i);
            }
            ch.cur_src = s;
            ch.cur_dst = d;
            if ch.repeat() {
                ch.reload_for_repeat();
            } else {
                ch.enabled = false;
                ch.control &= !(1 << 15);
            }
            ch.done = !ch.enabled;
            self.dma.chans[i] = ch;
        }
        self.dma.flags = flags;
    }

    /// Pending interrupt flags (IF & IE).
    pub fn pending_irq(&self) -> u16 {
        self.io.pending_irq()
    }

    /// Interrupts that end a HALT (IF & IE, regardless of IME).
    pub fn wake_irq(&self) -> u16 {
        self.io.wake_irq()
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
        // Enable the keypad IRQ in IE: it wakes a HALT but is not taken
        // until IME is set.
        b.write16(0x0400_0200, crate::io::IRQ_KEYPAD as u32);
        assert_ne!(b.wake_irq() & crate::io::IRQ_KEYPAD, 0);
        assert_eq!(b.pending_irq() & crate::io::IRQ_KEYPAD, 0);
        b.write16(0x0400_0208, 1);
        assert_ne!(b.pending_irq() & crate::io::IRQ_KEYPAD, 0);
        // Acknowledge: writing 1 to IF clears.
        b.write16(0x0400_0202, crate::io::IRQ_KEYPAD as u32);
        assert_eq!(b.io.iflags() & crate::io::IRQ_KEYPAD, 0);
    }

    fn setup_dma(b: &mut Bus, ch: u32, src: u32, dst: u32, count: u32, cnt_h: u32) {
        let base = 0x0400_00B0 + ch * 0xC;
        b.write32(base, src);
        b.write32(base + 4, dst);
        b.write16(base + 8, count);
        b.write16(base + 10, cnt_h);
    }

    #[test]
    fn dma_immediate_copy() {
        let mut b = bus();
        for i in 0..4 {
            b.write16(0x0300_0000 + i * 2, 0x1000 + i);
        }
        setup_dma(&mut b, 0, 0x0300_0000, 0x0300_1000, 4, 0x8000);
        b.run_dma(crate::dma::Timing::Immediate);
        for i in 0..4 {
            assert_eq!(b.read16(0x0300_1000 + i * 2), 0x1000 + i);
        }
        // A non-repeating channel disables itself and clears CNT_H bit 15.
        assert!(!b.dma.chans[0].enabled);
        assert_eq!(b.read16(0x0400_00BA) & 0x8000, 0);
    }

    #[test]
    fn dma_timed_channel_waits_for_its_trigger() {
        let mut b = bus();
        setup_dma(&mut b, 0, 0x0300_0000, 0x0300_1000, 2, 0x8000 | (1 << 12));
        b.run_dma(crate::dma::Timing::Immediate);
        assert!(b.dma.chans[0].enabled, "VBlank channel must not run yet");
        b.run_dma(crate::dma::Timing::VBlank);
        assert!(!b.dma.chans[0].enabled);
    }

    #[test]
    fn dma_repeat_reloads_destination_in_mode_3() {
        let mut b = bus();
        b.write32(0x0300_0000, 0x1111_1111);
        b.write32(0x0300_0004, 0x2222_2222);
        // Repeat, 32-bit, HBlank, dst increment+reload, src increment.
        setup_dma(
            &mut b,
            1,
            0x0300_0000,
            0x0300_2000,
            2,
            0x8000 | (1 << 9) | (1 << 10) | (2 << 12) | (3 << 5),
        );
        b.run_dma(crate::dma::Timing::HBlank);
        assert_eq!(b.read32(0x0300_2004), 0x2222_2222);
        let ch = b.dma.chans[1];
        assert!(ch.enabled, "repeating channel stays enabled");
        assert_eq!(ch.cur_dst, 0x0300_2000, "destination reloaded");
        assert_eq!(ch.cur_src, 0x0300_0008, "source keeps advancing");
        assert_eq!(ch.cur_count, 2, "count reloaded");
        assert_eq!(b.dma.irq_flags(), 0, "no IRQ requested");
    }

    #[test]
    fn dma_irq_flags_use_if_bits_8_to_11() {
        let mut b = bus();
        setup_dma(&mut b, 3, 0x0300_0000, 0x0300_1000, 1, 0x8000 | (1 << 14));
        b.run_dma(crate::dma::Timing::Immediate);
        assert_eq!(b.dma.irq_flags(), 1 << 11);
    }

    #[test]
    fn dma3_count_zero_means_0x10000_units() {
        let mut b = bus();
        setup_dma(&mut b, 3, 0x0300_0000, 0x0200_0000, 0, 0x8000 | (1 << 10));
        assert_eq!(b.dma.chans[3].cur_count, 0x10000);
        b.run_dma(crate::dma::Timing::Immediate);
        assert_eq!(b.dma.chans[3].cur_dst, 0x0200_0000 + 0x10000 * 4);
    }

    #[test]
    fn timer_irq_acknowledge_is_not_reasserted() {
        let mut b = bus();
        b.write16(0x0400_0100, 0xFFFF); // TM0 reload
        b.write16(0x0400_0102, 0x80 | 0x40); // enable + IRQ
        b.timers.step(1);
        b.sync_dev_irq();
        assert_ne!(b.io.iflags() & crate::io::irq::TIMER0, 0);
        b.write16(0x0400_0202, crate::io::irq::TIMER0 as u32);
        assert_eq!(b.io.iflags() & crate::io::irq::TIMER0, 0);
        b.sync_dev_irq();
        assert_eq!(b.io.iflags() & crate::io::irq::TIMER0, 0, "stale flag");
    }

    #[test]
    fn dma_irq_clears_on_if_write() {
        let mut b = bus();
        setup_dma(&mut b, 3, 0x0300_0000, 0x0300_1000, 1, 0x8000 | (1 << 14));
        b.run_dma(crate::dma::Timing::Immediate);
        b.sync_dev_irq();
        assert_ne!(b.io.iflags() & crate::io::irq::DMA3, 0);
        b.write16(0x0400_0202, crate::io::irq::DMA3 as u32);
        b.sync_dev_irq();
        assert_eq!(b.io.iflags() & crate::io::irq::DMA3, 0);
    }

    #[test]
    fn eeprom_type_is_detected_from_the_rom_identifier() {
        let mut rom = vec![0u8; 0x1000];
        rom[0x100..0x10B].copy_from_slice(b"EEPROM_V124");
        let b = Bus::new(rom);
        assert_eq!(b.save.kind, SaveType::Eeprom);
        let mut rom = vec![0u8; 0x1000];
        rom[0x200..0x20A].copy_from_slice(b"FLASH1M_V1");
        assert_eq!(Bus::new(rom).save.kind, SaveType::Flash);
        assert_eq!(Bus::new(vec![0u8; 0x1000]).save.kind, SaveType::None);
    }

    #[test]
    fn eeprom_dma_sizes_the_part_and_transfers_bits() {
        let mut rom = vec![0u8; 0x1000];
        rom[0x100..0x108].copy_from_slice(b"EEPROM_V");
        let mut b = Bus::new(rom);
        // A 9-halfword read request (512 B part): "11", address 3, stop.
        let bits: [u16; 9] = [1, 1, 0, 0, 0, 0, 1, 1, 0];
        for (i, bit) in bits.iter().enumerate() {
            b.write16(0x0300_0000 + i as u32 * 2, *bit as u32);
        }
        setup_dma(&mut b, 3, 0x0300_0000, 0x0D00_0000, 9, 0x8000);
        b.run_dma(crate::dma::Timing::Immediate);
        assert!(!b.save.eeprom.is_8k());
        // Read the 68-bit reply back through DMA: 4 dummy + 64 data (0xFF fill).
        setup_dma(&mut b, 3, 0x0D00_0000, 0x0300_1000, 68, 0x8000);
        b.run_dma(crate::dma::Timing::Immediate);
        assert_eq!(b.read16(0x0300_1000), 0, "dummy bit");
        assert_eq!(b.read16(0x0300_1000 + 4 * 2), 1, "first data bit of 0xFF");
    }

    #[test]
    fn eeprom_region_mirrors_rom_without_an_eeprom() {
        let mut rom = vec![0u8; 0x1000];
        rom[0x100..0x108].copy_from_slice(b"FLASH_V1");
        rom[0x10] = 0xAB;
        let mut b = Bus::new(rom);
        assert_eq!(b.read8(0x0D00_0010), 0xAB);
        b.write16(0x0D00_0000, 1);
        assert_eq!(b.save.kind, SaveType::Flash, "flash cart stays flash");
    }

    #[test]
    fn ime_lives_at_0x208_and_waitcnt_at_0x204() {
        let mut b = bus();
        b.write16(0x0400_0204, 0x4317); // WAITCNT
        assert!(!b.io.ime());
        assert_eq!(b.read16(0x0400_0204), 0x4317);
        b.write16(0x0400_0208, 1);
        assert!(b.io.ime());
        assert_eq!(b.read16(0x0400_0208), 1);
    }

    #[test]
    fn dispstat_status_bits_are_read_only() {
        let mut b = bus();
        b.write16(0x0400_0004, 0xFFFF);
        assert_eq!(b.read16(0x0400_0004), 0xFF38);
    }

    #[test]
    fn io_powers_on_with_bios_register_values() {
        let mut b = bus();
        assert_eq!(b.read16(0x0400_0000), 0x0080); // DISPCNT forced blank
        assert_eq!(b.read16(0x0400_0020), 0x0100); // BG2PA identity
        assert_eq!(b.read16(0x0400_0088), 0x0200); // SOUNDBIAS
        assert_eq!(b.read16(0x0400_0134), 0x8000); // RCNT
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
    fn unmapped_region_reads_open_bus() {
        let mut b = bus();
        assert_eq!(b.read8(0x0FFF_0000), 0);
        assert_eq!(b.read32(0x0FFF_0000), 0);
        b.write32(0x0300_0000, 0xDEAD_BEEF);
        b.read32(0x0300_0000);
        assert_eq!(b.read32(0x0FFF_0000), 0xDEAD_BEEF);
        assert_eq!(b.read8(0x0FFF_0000), 0xEF);
    }

    #[test]
    fn vram_keeps_bit_15_and_mirrors_every_128k() {
        let mut b = bus();
        b.write16(0x0600_8020, 0x3000); // screen block 16
        b.write16(0x0600_0020, 0x4444); // char block 0
        assert_eq!(b.read16(0x0600_8020), 0x3000, "bit 15 must survive");
        assert_eq!(b.read16(0x0600_0020), 0x4444);
        assert_eq!(b.read16(0x0602_0020), 0x4444, "128 KB mirror");
        b.write16(0x0601_0000, 0xBEEF); // OBJ VRAM
        assert_eq!(
            b.read16(0x0601_8000),
            0xBEEF,
            "upper 32 KB mirrors OBJ VRAM"
        );
        assert_eq!(super::vram_index(0x1_7FFF), 0x1_7FFF);
        assert_eq!(super::vram_index(0x1_8000), 0x1_0000);
    }

    #[test]
    fn misaligned_read32_rotates() {
        let mut b = bus();
        b.write32(0x0300_0000, 0x1234_5678);
        assert_eq!(b.read32(0x0300_0000), 0x1234_5678);
        assert_eq!(b.read32(0x0300_0001), 0x7812_3456);
        assert_eq!(b.read32(0x0300_0002), 0x5678_1234);
        assert_eq!(b.read32(0x0300_0003), 0x3456_7812);
    }

    #[test]
    fn read16_odd_address_rotates_in_32_bits() {
        let mut b = bus();
        b.write16(0x0300_0000, 0x1234);
        assert_eq!(b.read16(0x0300_0001), 0x3400_0012);
    }

    #[test]
    fn waitcnt_changes_rom_wait_states() {
        let mut b = Bus::new(vec![0; 0x100]);
        b.begin_step();
        b.read16(0x0800_0000); // default WS0: 4 non-sequential
        assert_eq!(b.cycles(), 4);
        b.write16(0x0400_0204, 3 << 2); // WS0 N = 8
        b.begin_step();
        b.read16(0x0800_0000);
        assert_eq!(b.cycles(), 8);
    }

    #[test]
    fn haltcnt_write_requests_halt() {
        let mut b = bus();
        assert!(!b.io.halt_requested);
        b.write8(0x0400_0301, 0);
        assert!(b.io.halt_requested);
    }

    #[test]
    fn io_byte_reads_see_live_registers() {
        let mut b = bus();
        b.write16(0x0400_0200, 0x1234); // IE
        assert_eq!(b.read8(0x0400_0200), 0x34);
        assert_eq!(b.read8(0x0400_0201), 0x12);
        b.write16(0x0400_0100, 0x00FE); // TM0 reload
        b.write16(0x0400_0102, 0x80);
        b.timers.step(1);
        assert_eq!(b.read16(0x0400_0100), 0x00FF, "live counter");
        assert_eq!(b.read8(0x0400_0100), 0xFF);
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
