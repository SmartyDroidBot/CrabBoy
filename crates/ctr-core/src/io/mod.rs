//! Memory-mapped I/O.
//!
//! Registers are modelled as 32-bit words. A narrower access arrives as the
//! aligned word address, the value moved to its byte lanes, and a mask of the
//! lanes written, so write-one-to-clear registers behave under byte and
//! halfword writes too.
//!
//! The ARM9 sees its own blocks below 0x10100000 and the shared blocks at
//! 0x101xxxxx; the ARM11 sees the shared blocks and its own from 0x10200000
//! up (GBATEK, "3DS Memory and I/O Map"). An access outside a processor's
//! reach, or into an unused 4 KB block, is a data abort.

pub mod i2c;
pub mod pdc;
pub mod pxi;
pub mod sdmmc;
pub mod spi;
pub mod timer9;
pub mod trace;

use crate::arm11::{irq, Mpcore};
use crate::bus::PhysMem;
use crate::clock::FRAME_CYCLES;
use crate::ctr::{BOTTOM_SCREEN, TOP_SCREEN};
use crate::sched::{Event, Scheduler};
use i2c::I2c;
use pdc::Pdc;
use pxi::{Pxi, Side};
use sdmmc::{Card, CardKind, Sdmmc};
use spi::Spi;
use std::collections::BTreeMap;
use timer9::Timers;

/// ARM9 interrupt sources, as bits of `IRQ_IE` and `IRQ_IF` (3dbrew, "IRQ
/// Registers").
pub mod irq9 {
    pub const TIMER_0: u32 = 1 << 8;
    pub const PXI_SYNC: u32 = 1 << 12;
    pub const PXI_SEND_EMPTY: u32 = 1 << 13;
    pub const PXI_RECV_NOT_EMPTY: u32 = 1 << 14;
    pub const SDIO_1: u32 = 1 << 16;
}

/// The ARM9 interrupt controller at 0x10001000: an enable word and a pending
/// word that is cleared by writing ones.
#[derive(Default)]
pub struct Irq9 {
    pub enable: u32,
    pub pending: u32,
}

impl Irq9 {
    pub fn line(&self) -> bool {
        self.enable & self.pending != 0
    }
}

/// `CFG9_MPCORECFG` and `CFG11_SOCINFO` on an Old 3DS.
const SOCINFO_OLD_3DS: u32 = 1;

/// Sectors of an Old 3DS eMMC (943 MB).
const NAND_SECTORS: u32 = 0x1D_7800;

/// PDC status bit of the vertical blank, in the framebuffer select register.
const PDC_STATUS_VBLANK: u32 = 1 << 17;
/// PDC control: the vertical blank interrupt is masked.
const PDC_MASK_VBLANK: u32 = 1 << 9;

/// Every memory-mapped unit.
pub struct Io {
    pub irq9: Irq9,
    pub timers9: Timers,
    /// `HID_PAD`: a clear bit is a pressed button.
    pub pad: u16,
    /// Top screen, then bottom screen.
    pub pdc: [Pdc; 2],
    pub pxi: Pxi,
    pub i2c: I2c,
    pub spi: Spi,
    pub sdmmc: Sdmmc,
    pub mpcore: Mpcore,
    sysprot9: u8,
    bootenv: u32,
    /// Registers that only hold what was written: configuration blocks whose
    /// effects are not modelled.
    latched: BTreeMap<u32, u32>,
    /// Accesses to registers that are not modelled.
    pub trace: trace::Trace,
}

impl Default for Io {
    fn default() -> Self {
        Self::new()
    }
}

/// Blocks whose registers read back what was written.
fn latches(block: u32) -> bool {
    matches!(
        block,
        0x10140 | 0x10141 | 0x10147 | 0x10202 | 0x10400 | 0x10401
    )
}

impl Io {
    pub fn new() -> Self {
        let mut latched = BTreeMap::new();
        // GPIO data as the boot ROM leaves it (GodMode9 relies on it).
        latched.insert(0x1014_7000, 0x0003);
        latched.insert(0x1014_7010, 0x0002);
        latched.insert(0x1014_7020, 0x0DFB);
        // CFG11: null page and FIQ mask reset values.
        latched.insert(0x1014_0100, 0x0001_0000);
        latched.insert(0x1014_0104, 0x0000_000F);
        // PDN: the clock multiplier registers of cores 0 and 1 are fixed.
        latched.insert(0x1014_1310, 0x0000_3030);
        Io {
            irq9: Irq9::default(),
            timers9: Timers::new(),
            pad: 0x0FFF,
            pdc: [Pdc::new(TOP_SCREEN), Pdc::new(BOTTOM_SCREEN)],
            pxi: Pxi::new(),
            i2c: I2c::new(),
            spi: Spi::new(),
            sdmmc: {
                // A console always has its eMMC; this one is blank.
                let mut sdmmc = Sdmmc::new();
                sdmmc.cards[1] = Some(Card::new(CardKind::Mmc, Vec::new(), NAND_SECTORS));
                sdmmc
            },
            mpcore: Mpcore::new(),
            sysprot9: 0,
            bootenv: 0,
            latched,
            trace: trace::Trace::default(),
        }
    }

    /// Start the events that run from power-on.
    pub fn power_on(&mut self, sched: &mut Scheduler) {
        sched.schedule(FRAME_CYCLES as u64, Event::VBlank);
    }

    fn apply_pxi(&mut self, irqs: pxi::Irqs) {
        let (arm9, arm11) = (Side::Arm9 as usize, Side::Arm11 as usize);
        if irqs.sync[arm9] {
            self.irq9.pending |= irq9::PXI_SYNC;
        }
        if irqs.send_empty[arm9] {
            self.irq9.pending |= irq9::PXI_SEND_EMPTY;
        }
        if irqs.recv_not_empty[arm9] {
            self.irq9.pending |= irq9::PXI_RECV_NOT_EMPTY;
        }
        if irqs.sync[arm11] {
            self.mpcore.gic.raise(irq::PXI_SYNC);
        }
        if irqs.send_empty[arm11] {
            self.mpcore.gic.raise(irq::PXI_SEND_EMPTY);
        }
        if irqs.recv_not_empty[arm11] {
            self.mpcore.gic.raise(irq::PXI_RECV_NOT_EMPTY);
        }
    }

    fn sdmmc_irq(&mut self) {
        if self.sdmmc.interrupting() {
            self.irq9.pending |= irq9::SDIO_1;
        }
    }

    /// Put an SD card holding `image` in the slot. The card is as large as
    /// the image, rounded up to a whole number of 512 KB units.
    pub fn insert_sd(&mut self, image: Vec<u8>) {
        let sectors = (image.len().div_ceil(512 * 1024) * 1024) as u32;
        self.sdmmc.cards[0] = Some(Card::new(CardKind::Sd, image, sectors.max(1024)));
    }

    fn read_pxi(&mut self, side: Side, offset: u32) -> u32 {
        let mut irqs = pxi::Irqs::default();
        let value = self.pxi.read(side, offset, &mut irqs);
        self.apply_pxi(irqs);
        value
    }

    fn write_pxi(&mut self, side: Side, offset: u32, value: u32, mask: u32) {
        let mut irqs = pxi::Irqs::default();
        self.pxi.write(side, offset, value, mask, &mut irqs);
        self.apply_pxi(irqs);
    }

    fn i2c_bus(block: u32) -> Option<usize> {
        match block {
            0x10161 => Some(0),
            0x10144 => Some(1),
            0x10148 => Some(2),
            _ => None,
        }
    }

    /// The FIFO-mode registers of an SPI bus sit at 0x800 of its block.
    fn spi_bus(addr: u32) -> Option<usize> {
        if addr & 0x800 == 0 {
            return None;
        }
        match addr >> 12 {
            0x10160 => Some(0),
            0x10142 => Some(1),
            0x10143 => Some(2),
            _ => None,
        }
    }

    /// Blocks both processors reach, at 0x101xxxxx.
    fn read_shared(&mut self, addr: u32) -> Option<u32> {
        let block = addr >> 12;
        let offset = addr & 0xFFF;
        Some(match block {
            0x10140 if offset == 0xFFC => SOCINFO_OLD_3DS,
            0x10146 => match offset {
                0x000 => self.pad as u32 | self.latched_or_zero(addr) & 0xFFFF_0000,
                _ => self.trace.read(addr),
            },
            _ if Io::spi_bus(addr).is_some() => self
                .spi
                .read(Io::spi_bus(addr).unwrap_or(0), offset & 0x7FF),
            _ if Io::i2c_bus(block).is_some() => {
                self.i2c.read(Io::i2c_bus(block).unwrap_or(0), offset)
            }
            _ if latches(block) => self.latched_or_zero(addr),
            0x10100..=0x1017F => self.trace.read(addr),
            _ => return None,
        })
    }

    fn write_shared(&mut self, addr: u32, value: u32, mask: u32) -> Option<()> {
        let block = addr >> 12;
        let offset = addr & 0xFFF;
        if let Some(bus) = Io::spi_bus(addr) {
            self.spi.write(bus, offset & 0x7FF, value);
        } else if let Some(bus) = Io::i2c_bus(block) {
            let mut irqs = Vec::new();
            self.i2c.write(bus, offset, value, mask, &mut irqs);
            for id in irqs {
                self.mpcore.gic.raise(id);
            }
        } else if latches(block) || block == 0x10146 {
            self.latch(addr, value, mask);
        } else if (0x10100..=0x1017F).contains(&block) {
            self.trace.write(addr, value, mask);
        } else {
            return None;
        }
        Some(())
    }

    fn latched_or_zero(&self, addr: u32) -> u32 {
        self.latched.get(&addr).copied().unwrap_or(0)
    }

    fn latch(&mut self, addr: u32, value: u32, mask: u32) {
        let word = self.latched.entry(addr).or_insert(0);
        *word = *word & !mask | value & mask;
    }

    /// Read the word at `addr` (aligned) as the ARM9 sees it. `None` is a
    /// data abort.
    pub fn read9(&mut self, addr: u32, sched: &Scheduler) -> Option<u32> {
        self.trace.touch(addr, None);
        let offset = addr & 0xFFF;
        Some(match addr >> 12 {
            0x10000 => match offset {
                0x000 => self.sysprot9 as u32,
                0xFFC => SOCINFO_OLD_3DS,
                _ => self.trace.read(addr),
            },
            0x10001 => match offset {
                0x000 => self.irq9.enable,
                0x004 => self.irq9.pending,
                _ => self.trace.read(addr),
            },
            0x10003 => {
                let counter = self.timers9.read16(offset & 0xC, sched.now()) as u32;
                let control = self.timers9.read16(offset & 0xC | 2, sched.now()) as u32;
                control << 16 | counter
            }
            0x10006 => {
                let value = if offset == 0x10C {
                    self.sdmmc.read_fifo32()
                } else {
                    let low = self.sdmmc.read16(offset as usize) as u32;
                    low | (self.sdmmc.read16(offset as usize + 2) as u32) << 16
                };
                self.sdmmc_irq();
                value
            }
            0x10008 => self.read_pxi(Side::Arm9, offset),
            0x10010 => match offset {
                0x000 => self.bootenv,
                _ => self.trace.read(addr),
            },
            0x10000..=0x1000D | 0x10010..=0x10012 | 0x10018 => self.trace.read(addr),
            // The ARM11's PXI end and everything from 0x10200000 up.
            0x10163 | 0x10200.. => return None,
            _ => return self.read_shared(addr),
        })
    }

    /// Write the lanes of `mask` of the word at `addr` as the ARM9. `None` is
    /// a data abort.
    pub fn write9(
        &mut self,
        addr: u32,
        value: u32,
        mask: u32,
        sched: &mut Scheduler,
    ) -> Option<()> {
        self.trace.touch(addr, Some(value));
        let offset = addr & 0xFFF;
        match addr >> 12 {
            0x10000 if offset == 0 => {
                if mask & 0xFF != 0 {
                    // Both protection bits are sticky.
                    self.sysprot9 |= value as u8 & 3;
                }
            }
            0x10001 => match offset {
                0x000 => self.irq9.enable = self.irq9.enable & !mask | value & mask,
                0x004 => self.irq9.pending &= !(value & mask),
                _ => self.trace.write(addr, value, mask),
            },
            0x10003 => {
                if mask & 0xFFFF != 0 {
                    self.timers9.write16(offset & 0xC, value as u16, sched);
                }
                if mask >> 16 != 0 {
                    self.timers9
                        .write16(offset & 0xC | 2, (value >> 16) as u16, sched);
                }
            }
            0x10006 => {
                if offset == 0x10C {
                    self.sdmmc.write_fifo32(value);
                } else {
                    if mask & 0xFFFF != 0 {
                        self.sdmmc.write16(offset as usize, value as u16);
                    }
                    if mask >> 16 != 0 {
                        self.sdmmc
                            .write16(offset as usize + 2, (value >> 16) as u16);
                    }
                }
                self.sdmmc_irq();
            }
            0x10008 => self.write_pxi(Side::Arm9, offset, value, mask),
            0x10010 if offset == 0 => self.bootenv = self.bootenv & !mask | value & mask,
            0x10000..=0x1000D | 0x10010..=0x10012 | 0x10018 => self.trace.write(addr, value, mask),
            0x10163 | 0x10200.. => return None,
            _ => return self.write_shared(addr, value, mask),
        }
        Some(())
    }

    /// Read the word at `addr` as an ARM11 core.
    pub fn read11(&mut self, addr: u32, _sched: &Scheduler) -> Option<u32> {
        self.trace.touch(addr, None);
        let offset = addr & 0xFFF;
        Some(match addr >> 12 {
            0x10163 => self.read_pxi(Side::Arm11, offset),
            0x10400 => match offset {
                0x400..=0x4FF => self.pdc[0].read32(offset),
                0x500..=0x5FF => self.pdc[1].read32(offset),
                _ => self.latched_or_zero(addr),
            },
            0x10200..=0x10203 | 0x1020F | 0x10401 => {
                if latches(addr >> 12) {
                    self.latched_or_zero(addr)
                } else {
                    self.trace.read(addr)
                }
            }
            _ => return self.read_shared(addr),
        })
    }

    /// Write the lanes of `mask` of the word at `addr` as an ARM11 core.
    pub fn write11(
        &mut self,
        addr: u32,
        value: u32,
        mask: u32,
        mem: &mut PhysMem,
        _sched: &mut Scheduler,
    ) -> Option<()> {
        self.trace.touch(addr, Some(value));
        let offset = addr & 0xFFF;
        match addr >> 12 {
            0x10163 => self.write_pxi(Side::Arm11, offset, value, mask),
            0x10202 => {
                self.latch(addr, value, mask);
                // The fill colour of each panel.
                match offset {
                    0x204 => self.pdc[0].fill = self.latched_or_zero(addr),
                    0xA04 => self.pdc[1].fill = self.latched_or_zero(addr),
                    _ => {}
                }
            }
            0x10400 => self.write_gpu(addr, value, mask, mem),
            0x10200..=0x10203 | 0x1020F | 0x10401 => {
                if latches(addr >> 12) {
                    self.latch(addr, value, mask);
                } else {
                    self.trace.write(addr, value, mask);
                }
            }
            _ => return self.write_shared(addr, value, mask),
        }
        Some(())
    }

    /// The GPU's external registers: memory fills, the display controllers
    /// and the transfer engine (3dbrew, "GPU/External Registers").
    fn write_gpu(&mut self, addr: u32, value: u32, mask: u32, mem: &mut PhysMem) {
        let offset = addr & 0xFFF;
        match offset {
            0x400..=0x5FF => {
                let pdc = &mut self.pdc[(offset >> 8 & 1) as usize];
                let reg = offset & 0xFF;
                let old = pdc.read32(reg);
                let written = old & !mask | value & mask;
                if reg == 0x78 {
                    // Status bits 16-18 clear when written as one.
                    const STATUS: u32 = 0x0007_0000;
                    let status = old & STATUS & !(value & mask);
                    pdc.write32(reg, written & !STATUS | status);
                } else {
                    pdc.write32(reg, written);
                }
            }
            // Memory fill units PSC0 and PSC1.
            0x01C | 0x02C => {
                self.latch(addr, value, mask);
                let control = self.latched_or_zero(addr);
                if control & 1 != 0 {
                    let unit = addr & !0xF;
                    let start = self.latched_or_zero(unit) << 3;
                    let end = self.latched_or_zero(unit + 4) << 3;
                    let pattern = self.latched_or_zero(unit + 8);
                    fill(mem, start, end, pattern, control >> 8 & 3);
                    // Done: busy clears, finished sets, the interrupt fires.
                    self.latched.insert(addr, control & !1 | 2);
                    let id = if offset == 0x01C {
                        irq::PSC0
                    } else {
                        irq::PSC1
                    };
                    self.mpcore.gic.raise(id);
                }
            }
            // The transfer engine is not modelled: it reports completion and
            // the trace records that it was asked.
            0xC18 => {
                self.latch(addr, value, mask);
                if self.latched_or_zero(addr) & 1 != 0 {
                    self.trace.write(addr, value, mask);
                    let control = self.latched_or_zero(addr);
                    self.latched.insert(addr, control & !1 | 1 << 8);
                    self.mpcore.gic.raise(irq::PPF);
                }
            }
            _ => self.latch(addr, value, mask),
        }
    }

    /// Whether the protected half of the ARM9 boot ROM is hidden.
    pub fn boot9_protected(&self) -> bool {
        self.sysprot9 & 1 != 0
    }

    /// Handle a scheduler event.
    pub fn fire(&mut self, at: u64, event: Event, sched: &mut Scheduler) {
        match event {
            Event::Arm9Timer(n) => {
                let irqs = self.timers9.overflow(n as usize, at, sched);
                self.irq9.pending |= irqs as u32 * irq9::TIMER_0;
            }
            Event::Arm11Timer(core) => {
                if self.mpcore.timers[core as usize].expire(at, sched) {
                    self.mpcore
                        .gic
                        .raise_private(core as usize, crate::arm11::gic::IRQ_TIMER);
                }
            }
            Event::VBlank => {
                for (n, id) in [irq::PDC0, irq::PDC1].into_iter().enumerate() {
                    let pdc = &mut self.pdc[n];
                    let control = pdc.read32(0x74);
                    if control & 1 == 0 {
                        continue;
                    }
                    let status = pdc.read32(0x78);
                    pdc.write32(0x78, status | PDC_STATUS_VBLANK);
                    if control & PDC_MASK_VBLANK == 0 {
                        self.mpcore.gic.raise(id);
                    }
                }
                sched.schedule(at + FRAME_CYCLES as u64, Event::VBlank);
            }
        }
    }
}

/// A PSC memory fill of `[start, end)` with a 16-, 24- or 32-bit pattern.
fn fill(mem: &mut PhysMem, start: u32, end: u32, pattern: u32, width: u32) {
    let Some(len) = end.checked_sub(start) else {
        return;
    };
    let Some(target) = mem.slice_mut(start, len as usize) else {
        return;
    };
    let bytes = pattern.to_le_bytes();
    let unit = match width {
        0 => 2,
        2 => 4,
        _ => 3,
    };
    for (n, byte) in target.iter_mut().enumerate() {
        *byte = bytes[n % unit];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_interrupts_clear_by_writing_ones_in_any_lane() {
        let mut sched = Scheduler::new();
        let mut io = Io::new();
        io.irq9.pending = 0x0000_0301;
        io.write9(0x1000_1004, 0x0000_0100, 0x0000_FF00, &mut sched);
        assert_eq!(io.irq9.pending, 0x0000_0201);
        io.write9(0x1000_1000, 0x0000_0200, 0xFFFF_FFFF, &mut sched);
        assert!(io.irq9.line());
        io.write9(0x1000_1004, 0xFFFF_FFFF, 0xFFFF_FFFF, &mut sched);
        assert!(!io.irq9.line());
    }

    #[test]
    fn a_timer_overflow_raises_its_interrupt() {
        let mut sched = Scheduler::new();
        let mut io = Io::new();
        // Timer 1: reload 0xFFFF, start with the interrupt enabled.
        io.write9(0x1000_3004, 0x00C0_FFFF, 0xFFFF_FFFF, &mut sched);
        assert_eq!(
            io.read9(0x1000_3004, &sched),
            Some(0x00C0_FFFF),
            "control in the high half, counter in the low half"
        );
        sched.advance(4);
        while let Some((at, event)) = sched.pop_due() {
            io.fire(at, event, &mut sched);
        }
        assert_eq!(io.irq9.pending, irq9::TIMER_0 << 1);
    }

    #[test]
    fn each_processor_reaches_only_its_own_blocks() {
        let mut sched = Scheduler::new();
        let mut mem = PhysMem::new();
        let mut io = Io::new();
        assert_eq!(io.read9(0x1040_0468, &sched), None);
        assert_eq!(io.read9(0x1000_E000, &sched), None);
        assert_eq!(io.read9(0x1016_3000, &sched), None);
        assert_eq!(io.write9(0x1020_2204, 0, !0, &mut sched), None);
        assert_eq!(io.read11(0x1000_1000, &sched), None);
        assert_eq!(io.write11(0x1000_8000, 0, !0, &mut mem, &mut sched), None);
        assert_eq!(io.read9(0x1014_6000, &sched), Some(0x0FFF));
        assert_eq!(io.read11(0x1014_6000, &sched), Some(0x0FFF));
        assert_eq!(io.read9(0x1000_0FFC, &sched), Some(1));
        assert_eq!(io.read11(0x1014_0FFC, &sched), Some(1));
    }

    #[test]
    fn the_boot_rom_protection_bit_is_sticky() {
        let mut sched = Scheduler::new();
        let mut io = Io::new();
        io.write9(0x1000_0000, 1, 0xFF, &mut sched);
        io.write9(0x1000_0000, 0, 0xFF, &mut sched);
        assert!(io.boot9_protected());
    }

    #[test]
    fn pxi_words_cross_and_interrupt_the_receiver() {
        let mut sched = Scheduler::new();
        let mut mem = PhysMem::new();
        let mut io = Io::new();
        io.write11(0x1016_3004, 1 << 10 | 1 << 15, !0, &mut mem, &mut sched);
        io.write9(0x1000_8004, 1 << 15, !0, &mut sched);
        io.write9(0x1000_8008, 0xCAFE, !0, &mut sched);
        assert_ne!(io.mpcore.gic.read_distributor(0, 0x208) & 1 << 0x13, 0);
        assert_eq!(io.read11(0x1016_300C, &sched), Some(0xCAFE));
    }

    #[test]
    fn a_memory_fill_runs_and_reports_completion() {
        let mut sched = Scheduler::new();
        let mut mem = PhysMem::new();
        let mut io = Io::new();
        io.write11(0x1040_0010, 0x1830_0000 >> 3, !0, &mut mem, &mut sched);
        io.write11(0x1040_0014, 0x1830_0010 >> 3, !0, &mut mem, &mut sched);
        io.write11(0x1040_0018, 0x00AA_BBCC, !0, &mut mem, &mut sched);
        io.write11(0x1040_001C, 1 << 8 | 1, !0, &mut mem, &mut sched);
        assert_eq!(
            mem.slice(0x1830_0000, 6),
            Some(&[0xCC, 0xBB, 0xAA, 0xCC, 0xBB, 0xAA][..])
        );
        assert_eq!(mem.slice(0x1830_0010, 1), Some(&[0u8][..]));
        assert_eq!(io.read11(0x1040_001C, &sched), Some(1 << 8 | 2));
        assert_ne!(io.mpcore.gic.read_distributor(0, 0x204) & 1 << 8, 0);
    }

    #[test]
    fn vertical_blank_sets_the_status_and_interrupts_unless_masked() {
        let mut sched = Scheduler::new();
        let mut mem = PhysMem::new();
        let mut io = Io::new();
        io.power_on(&mut sched);
        io.write11(0x1040_0474, 0x0001_0501, !0, &mut mem, &mut sched);
        io.write11(0x1040_0574, 0x0001_0701, !0, &mut mem, &mut sched);
        sched.advance(FRAME_CYCLES as u64);
        while let Some((at, event)) = sched.pop_due() {
            io.fire(at, event, &mut sched);
        }
        let pending = io.mpcore.gic.read_distributor(0, 0x204);
        assert_ne!(pending & 1 << 0xA, 0, "top screen");
        assert_eq!(pending & 1 << 0xB, 0, "bottom screen is masked");
        assert_eq!(io.read11(0x1040_0478, &sched), Some(PDC_STATUS_VBLANK));
        io.write11(0x1040_0478, PDC_STATUS_VBLANK, !0, &mut mem, &mut sched);
        assert_eq!(io.read11(0x1040_0478, &sched), Some(0));
        assert_eq!(sched.next_due(), Some(2 * FRAME_CYCLES as u64));
    }

    #[test]
    fn the_lcd_fill_colour_reaches_the_panel() {
        let mut sched = Scheduler::new();
        let mut mem = PhysMem::new();
        let mut io = Io::new();
        io.write11(0x1020_2204, 0x0100_00FF, !0, &mut mem, &mut sched);
        assert_eq!(io.pdc[0].frame(&mem).rgb.unwrap()[..3], [0xFF, 0, 0]);
    }
}
