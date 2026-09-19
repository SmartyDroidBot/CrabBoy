//! Memory-mapped I/O.
//!
//! Registers are modelled as 32-bit words. A narrower access arrives as the
//! aligned word address, the value moved to its byte lanes, and a mask of the
//! lanes written, so write-one-to-clear registers behave under byte and
//! halfword writes too.

pub mod pdc;
pub mod pxi;
pub mod timer9;
pub mod trace;

use crate::ctr::{BOTTOM_SCREEN, TOP_SCREEN};
use crate::sched::{Event, Scheduler};
use pdc::Pdc;
use timer9::Timers;

/// ARM9 interrupt sources, as bits of `IRQ_IE` and `IRQ_IF` (3dbrew, "IRQ
/// Registers").
pub mod irq9 {
    pub const TIMER_0: u32 = 1 << 8;
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

const CFG9_SOCINFO_OLD_3DS: u32 = 1;

/// Every memory-mapped unit.
pub struct Io {
    pub irq9: Irq9,
    pub timers9: Timers,
    /// `HID_PAD`: a clear bit is a pressed button.
    pub pad: u16,
    /// Top screen, then bottom screen.
    pub pdc: [Pdc; 2],
    sysprot9: u8,
    bootenv: u32,
    /// Accesses to registers that are not modelled.
    pub trace: trace::Trace,
}

impl Default for Io {
    fn default() -> Self {
        Self::new()
    }
}

impl Io {
    pub fn new() -> Self {
        Io {
            irq9: Irq9::default(),
            timers9: Timers::new(),
            pad: 0x0FFF,
            pdc: [Pdc::new(TOP_SCREEN), Pdc::new(BOTTOM_SCREEN)],
            sysprot9: 0,
            bootenv: 0,
            trace: trace::Trace::default(),
        }
    }

    /// Read the word at `addr` (aligned) as the ARM9 sees it. `None` is a
    /// data abort: the ARM9 cannot reach 0x10200000 and up, and unused 4 KB
    /// blocks fault (GBATEK, "3DS Memory and I/O Map").
    pub fn read9(&mut self, addr: u32, sched: &Scheduler) -> Option<u32> {
        let offset = addr & 0xFFF;
        Some(match addr >> 12 {
            0x10000 => match offset {
                0x000 => self.sysprot9 as u32,
                0xFFC => CFG9_SOCINFO_OLD_3DS,
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
            0x10010 => match offset {
                0x000 => self.bootenv,
                _ => self.trace.read(addr),
            },
            0x10146 => match offset {
                0x000 => self.pad as u32,
                _ => self.trace.read(addr),
            },
            block if self.mapped9(block) => self.trace.read(addr),
            _ => return None,
        })
    }

    /// Write the lanes of `mask` of the word at `addr`. `None` is a data
    /// abort.
    pub fn write9(
        &mut self,
        addr: u32,
        value: u32,
        mask: u32,
        sched: &mut Scheduler,
    ) -> Option<()> {
        let offset = addr & 0xFFF;
        match addr >> 12 {
            0x10000 => {
                if offset == 0 && mask & 0xFF != 0 {
                    // Both protection bits are sticky.
                    self.sysprot9 |= value as u8 & 3;
                }
            }
            0x10001 => match offset {
                0x000 => self.irq9.enable = self.irq9.enable & !mask | value & mask,
                0x004 => self.irq9.pending &= !(value & mask),
                _ => {}
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
            0x10010 => {
                if offset == 0 {
                    self.bootenv = self.bootenv & !mask | value & mask;
                }
            }
            block if self.mapped9(block) => self.trace.write(addr, value, mask),
            _ => return None,
        }
        Some(())
    }

    /// Blocks the ARM9 can address whose registers are not modelled yet:
    /// reads give zero and writes are dropped.
    fn mapped9(&self, block: u32) -> bool {
        matches!(
            block,
            0x10000..=0x1000D | 0x10010..=0x10012 | 0x10018 | 0x10100..=0x1017F
        )
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
        }
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
    fn the_arm9_cannot_reach_arm11_registers_or_unused_blocks() {
        let mut sched = Scheduler::new();
        let mut io = Io::new();
        assert_eq!(io.read9(0x1040_0468, &sched), None);
        assert_eq!(io.read9(0x1000_E000, &sched), None);
        assert_eq!(io.write9(0x1020_2204, 0, !0, &mut sched), None);
        assert_eq!(io.read9(0x1014_6000, &sched), Some(0x0FFF));
        assert_eq!(io.read9(0x1000_0FFC, &sched), Some(1));
    }

    #[test]
    fn the_boot_rom_protection_bit_is_sticky() {
        let mut sched = Scheduler::new();
        let mut io = Io::new();
        io.write9(0x1000_0000, 1, 0xFF, &mut sched);
        io.write9(0x1000_0000, 0, 0xFF, &mut sched);
        assert!(io.boot9_protected());
    }
}
