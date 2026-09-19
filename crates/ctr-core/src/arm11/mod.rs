//! The ARM11 MPCore side of the machine: what each application core sees
//! when it touches memory or a coprocessor.
//!
//! A virtual address goes through the core's MMU, then lands on the physical
//! bus (3dbrew, "Memory layout"): the boot ROM and its mirrors, I/O from
//! 0x10100000 up, the MPCore private region at 0x17E00000, and the shared
//! memories. The ARM11 cannot reach the ARM9's I/O below 0x10100000.

pub mod cp15;
pub mod gic;
pub mod mmu;
pub mod timer;

use crate::bus::PhysMem;
use crate::io::Io;
use crate::sched::Scheduler;
use arm_core::{Abort, Bus, CpEffect, CpReg};
use cp15::Cp15;
use emu_core::Mem;
use gic::{Gic, CORES};
use mmu::{fault, Access, Fault};
use timer::PrivateTimer;

/// ARM11 interrupt numbers (3dbrew, "ARM11 Interrupts").
pub mod irq {
    pub const PSC0: usize = 0x28;
    pub const PSC1: usize = 0x29;
    pub const PDC0: usize = 0x2A;
    pub const PDC1: usize = 0x2B;
    pub const PPF: usize = 0x2C;
    pub const PXI_SYNC: usize = 0x50;
    pub const PXI_SEND_EMPTY: usize = 0x52;
    pub const PXI_RECV_NOT_EMPTY: usize = 0x53;
    pub const I2C_BUS_0: usize = 0x54;
    pub const I2C_BUS_1: usize = 0x55;
    pub const I2C_BUS_2: usize = 0x5C;
    pub const HID_PAD: usize = 0x5B;
    pub const MCU: usize = 0x71;
}

pub const BOOT11_LEN: usize = 0x1_0000;
const PRIVATE_BASE: u32 = 0x17E0_0000;

/// The snoop control unit's configuration on an Old 3DS: two cores.
const SCU_CONFIG: u32 = 0x0000_0011;

/// What the cores share inside the MPCore: the interrupt controller, the
/// private timers and the snoop control unit.
pub struct Mpcore {
    pub gic: Gic,
    pub timers: [PrivateTimer; CORES],
    pub watchdogs: [PrivateTimer; CORES],
    scu_control: u32,
}

impl Default for Mpcore {
    fn default() -> Self {
        Self::new()
    }
}

/// The debug coprocessor, as far as software looks at it at start-up: the
/// debug ID register says ARMv6 debug with six breakpoints and two
/// watchpoints, and the status register reads as zero (monitor debug off).
/// Linux reads the ID unguarded and then leaves hardware breakpoints alone.
/// The layout is the ARMv6 one; the variant and revision fields copy the
/// main ID register and are not confirmed against hardware.
fn debug_read(reg: CpReg, privileged: bool) -> Option<u32> {
    const DIDR: u32 = 0x1501_0024;
    match (reg.opc1, reg.crn, reg.crm, reg.opc2) {
        (0, 0, 0, 0) => Some(DIDR),
        (0, 0, 1, 0) if privileged => Some(0),
        _ => None,
    }
}

impl Mpcore {
    pub fn new() -> Self {
        Mpcore {
            gic: Gic::new(),
            timers: [PrivateTimer::new(0), PrivateTimer::new(1)],
            watchdogs: [PrivateTimer::watchdog(0), PrivateTimer::watchdog(1)],
            scu_control: 0x1FFE,
        }
    }

    /// Read a word of the private region as `core`.
    fn read(&mut self, core: usize, offset: u32, sched: &Scheduler) -> Option<u32> {
        Some(match offset {
            0x000 => self.scu_control,
            0x004 => SCU_CONFIG,
            0x008..=0x0FF => 0,
            0x100..=0x1FF => self.gic.read_interface(core, offset & 0xFF),
            // The per-core aliases of the interface and of the timer.
            0x200..=0x5FF => {
                let target = (offset as usize - 0x200) >> 8;
                if target >= CORES {
                    return Some(0);
                }
                self.gic.read_interface(target, offset & 0xFF)
            }
            0x600..=0x61F => self.timers[core].read(offset & 0x1F, sched.now()),
            0x620..=0x63F => self.watchdogs[core].read(offset & 0x1F, sched.now()),
            0x640..=0x6FF => 0,
            0x700..=0xAFF => {
                let target = (offset as usize - 0x700) >> 8;
                match self.timers.get(target) {
                    Some(timer) if offset & 0xFF < 0x20 => timer.read(offset & 0x1F, sched.now()),
                    _ => 0,
                }
            }
            0x1000..=0x1FFF => self.gic.read_distributor(core, offset & 0xFFF),
            _ => return None,
        })
    }

    fn write(&mut self, core: usize, offset: u32, value: u32, sched: &mut Scheduler) -> Option<()> {
        match offset {
            0x000 => self.scu_control = value & 0x3FFF,
            0x004..=0x0FF => {}
            0x100..=0x1FF => self.gic.write_interface(core, offset & 0xFF, value),
            0x200..=0x5FF => {
                let target = (offset as usize - 0x200) >> 8;
                if target < CORES {
                    self.gic.write_interface(target, offset & 0xFF, value);
                }
            }
            0x600..=0x61F => self.timers[core].write(offset & 0x1F, value, sched),
            0x620..=0x63F => self.watchdogs[core].write(offset & 0x1F, value, sched),
            0x640..=0x6FF => {}
            0x700..=0xAFF => {
                let target = (offset as usize - 0x700) >> 8;
                if let Some(timer) = self.timers.get_mut(target) {
                    if offset & 0xFF < 0x20 {
                        timer.write(offset & 0x1F, value, sched);
                    }
                }
            }
            0x1000..=0x1FFF => self.gic.write_distributor(core, offset & 0xFFF, value),
            _ => return None,
        }
        Some(())
    }
}

/// State private to one core.
pub struct Core {
    pub cp15: Cp15,
}

/// State of the ARM11 side that no other part of the machine owns.
pub struct Arm11 {
    pub cores: [Core; CORES],
    /// The boot ROM, or the stand-in routines of the boot shim.
    pub boot11: Mem<u8, BOOT11_LEN>,
    /// The physical word each core has marked for exclusive access.
    exclusive: [Option<u32>; CORES],
}

impl Default for Arm11 {
    fn default() -> Self {
        Self::new()
    }
}

impl Arm11 {
    pub fn new() -> Self {
        Arm11 {
            cores: [Core { cp15: Cp15::new(0) }, Core { cp15: Cp15::new(1) }],
            boot11: Mem::zeroed(),
            exclusive: [None; CORES],
        }
    }
}

/// One core's view of the machine for the duration of a step.
pub struct Arm11Bus<'a> {
    pub core: usize,
    pub arm11: &'a mut Arm11,
    pub mem: &'a mut PhysMem,
    pub io: &'a mut Io,
    pub sched: &'a mut Scheduler,
}

fn get(bytes: &[u8], at: usize, len: usize) -> u32 {
    let mut word = [0u8; 4];
    word[..len].copy_from_slice(&bytes[at..at + len]);
    u32::from_le_bytes(word)
}

impl Arm11Bus<'_> {
    /// Read `len` bytes at the physical address `pa`, aligned to `len`.
    fn read_physical(&mut self, pa: u32, len: usize) -> Option<u32> {
        match pa >> 24 {
            0x00 if pa < 0x2_0000 => Some(get(&self.arm11.boot11[..], pa as usize & 0xFFFF, len)),
            0xFF if pa >= 0xFFFF_0000 => {
                Some(get(&self.arm11.boot11[..], pa as usize & 0xFFFF, len))
            }
            0x10..=0x17 => {
                let word = if pa >= PRIVATE_BASE {
                    self.io.trace.touch(pa & !3, None);
                    self.io
                        .mpcore
                        .read(self.core, (pa - PRIVATE_BASE) & !3, self.sched)?
                } else {
                    self.io.read11(pa & !3, self.sched)?
                };
                let shifted = word >> ((pa & 3) * 8);
                Some(if len == 4 {
                    shifted
                } else {
                    shifted & ((1 << (len * 8)) - 1)
                })
            }
            _ => self.mem.slice(pa, len).map(|bytes| get(bytes, 0, len)),
        }
    }

    /// The word of a virtual address, as the exclusive monitor names it. The
    /// access it belongs to has just been translated, so this cannot fault.
    fn monitored_word(&mut self, va: u32) -> u32 {
        self.translate(va, Access::Read, true).unwrap_or(va) & !3
    }

    fn write_physical(&mut self, pa: u32, len: usize, value: u32) -> Option<()> {
        // Any store to a word ends the other cores' exclusive access to it:
        // Linux releases a spinlock with a plain store, and a core that read
        // the lock before that must not get to write the old owner back.
        for (core, mark) in self.arm11.exclusive.iter_mut().enumerate() {
            if core != self.core && *mark == Some(pa & !3) {
                *mark = None;
            }
        }
        match pa >> 24 {
            0x00 if pa < 0x2_0000 => Some(()),
            0xFF if pa >= 0xFFFF_0000 => Some(()),
            0x10..=0x17 => {
                let shift = (pa & 3) * 8;
                let mask = if len == 4 {
                    !0
                } else {
                    ((1u32 << (len * 8)) - 1) << shift
                };
                if pa >= PRIVATE_BASE {
                    self.io.trace.touch(pa & !3, Some(value));
                    let offset = (pa - PRIVATE_BASE) & !3;
                    // Priorities, targets and configuration are byte arrays
                    // that drivers write a byte at a time; the other
                    // registers act on the bits written as one, so the
                    // untouched lanes can go in as zero.
                    let lanes = value << shift & mask;
                    let word = if (0x1400..0x1D00).contains(&offset) && len != 4 {
                        let old = self.io.mpcore.read(self.core, offset, self.sched)?;
                        old & !mask | lanes
                    } else {
                        lanes
                    };
                    self.io.mpcore.write(self.core, offset, word, self.sched)
                } else {
                    self.io
                        .write11(pa & !3, value << shift & mask, mask, self.mem, self.sched)
                }
            }
            _ => {
                let bytes = self.mem.slice_mut(pa, len)?;
                bytes.copy_from_slice(&value.to_le_bytes()[..len]);
                Some(())
            }
        }
    }

    fn translate(&mut self, va: u32, access: Access, privileged: bool) -> Result<u32, Fault> {
        // Split the borrows: the walk reads tables from RAM only.
        let mem = &*self.mem;
        self.arm11.cores[self.core]
            .cp15
            .mmu
            .translate(va, access, privileged, |pa| {
                mem.slice(pa, 4).map(|bytes| get(bytes, 0, 4))
            })
    }

    /// One aligned access, or the abort with the fault recorded.
    fn access(
        &mut self,
        va: u32,
        len: usize,
        kind: Access,
        privileged: bool,
        value: u32,
    ) -> Result<u32, Abort> {
        let outcome = self.translate(va, kind, privileged).and_then(|pa| {
            let done = if kind == Access::Write {
                self.write_physical(pa, len, value).map(|()| 0)
            } else {
                self.read_physical(pa, len)
            };
            done.ok_or(Fault {
                status: fault::EXTERNAL,
                domain: 0,
            })
        });
        outcome.map_err(|fault| {
            let cp15 = &mut self.arm11.cores[self.core].cp15;
            if kind == Access::Execute {
                cp15.instruction_fault(fault);
            } else {
                cp15.data_fault(fault, va, kind == Access::Write);
            }
            Abort
        })
    }

    /// An access of any alignment: byte by byte when it is unaligned, since
    /// it may then cross a page.
    fn transfer(
        &mut self,
        va: u32,
        len: usize,
        kind: Access,
        privileged: bool,
        value: u32,
    ) -> Result<u32, Abort> {
        if va & (len as u32 - 1) == 0 {
            return self.access(va, len, kind, privileged, value);
        }
        let mut result = 0;
        for i in 0..len {
            let byte = self.access(
                va.wrapping_add(i as u32),
                1,
                kind,
                privileged,
                value >> (i * 8) & 0xFF,
            )?;
            result |= byte << (i * 8);
        }
        Ok(result)
    }
}

impl Bus for Arm11Bus<'_> {
    fn fetch16(&mut self, addr: u32, privileged: bool) -> Result<u16, Abort> {
        Ok(self.access(addr, 2, Access::Execute, privileged, 0)? as u16)
    }
    fn fetch32(&mut self, addr: u32, privileged: bool) -> Result<u32, Abort> {
        self.access(addr, 4, Access::Execute, privileged, 0)
    }
    fn read8(&mut self, addr: u32, privileged: bool) -> Result<u8, Abort> {
        Ok(self.access(addr, 1, Access::Read, privileged, 0)? as u8)
    }
    fn read16(&mut self, addr: u32, privileged: bool) -> Result<u16, Abort> {
        Ok(self.transfer(addr, 2, Access::Read, privileged, 0)? as u16)
    }
    fn read32(&mut self, addr: u32, privileged: bool) -> Result<u32, Abort> {
        self.transfer(addr, 4, Access::Read, privileged, 0)
    }
    fn write8(&mut self, addr: u32, value: u8, privileged: bool) -> Result<(), Abort> {
        self.access(addr, 1, Access::Write, privileged, value as u32)?;
        Ok(())
    }
    fn write16(&mut self, addr: u32, value: u16, privileged: bool) -> Result<(), Abort> {
        self.transfer(addr, 2, Access::Write, privileged, value as u32)?;
        Ok(())
    }
    fn write32(&mut self, addr: u32, value: u32, privileged: bool) -> Result<(), Abort> {
        self.transfer(addr, 4, Access::Write, privileged, value)?;
        Ok(())
    }

    fn coproc_read(&mut self, reg: CpReg, privileged: bool) -> Option<u32> {
        if reg.cp == 14 {
            return debug_read(reg, privileged);
        }
        self.arm11.cores[self.core].cp15.read(reg, privileged)
    }
    fn coproc_write(&mut self, reg: CpReg, value: u32, privileged: bool) -> Option<CpEffect> {
        self.arm11.cores[self.core]
            .cp15
            .write(reg, value, privileged)
    }

    fn vfp_access(&self, privileged: bool) -> bool {
        self.arm11.cores[self.core].cp15.vfp_access(privileged)
    }

    fn unaligned_access(&self) -> bool {
        self.arm11.cores[self.core].cp15.unaligned_access()
    }

    fn exclusive_load(&mut self, addr: u32) {
        let word = self.monitored_word(addr);
        self.arm11.exclusive[self.core] = Some(word);
    }

    fn exclusive_store(&mut self, addr: u32) -> bool {
        // The store that follows a success goes through `write_physical`,
        // which breaks every other reservation on the word.
        let word = self.monitored_word(addr);
        self.arm11.exclusive[self.core].take() == Some(word)
    }

    fn exclusive_clear(&mut self) {
        self.arm11.exclusive[self.core] = None;
    }

    fn high_vectors(&self) -> bool {
        self.arm11.cores[self.core].cp15.high_vectors()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rig {
        arm11: Arm11,
        mem: PhysMem,
        io: Io,
        sched: Scheduler,
    }

    impl Rig {
        fn new() -> Self {
            Rig {
                arm11: Arm11::new(),
                mem: PhysMem::new(),
                io: Io::new(),
                sched: Scheduler::new(),
            }
        }

        fn core(&mut self, core: usize) -> Arm11Bus<'_> {
            Arm11Bus {
                core,
                arm11: &mut self.arm11,
                mem: &mut self.mem,
                io: &mut self.io,
                sched: &mut self.sched,
            }
        }
    }

    const LOCK: u32 = 0x2000_0100;

    #[test]
    fn a_plain_store_by_another_core_ends_exclusive_access() {
        let mut rig = Rig::new();
        rig.core(0).exclusive_load(LOCK);
        // The other core releases the lock: a halfword store into the word.
        rig.core(1).write16(LOCK + 2, 1, true).unwrap();
        assert!(!rig.core(0).exclusive_store(LOCK));
    }

    #[test]
    fn a_core_keeps_its_mark_over_its_own_and_unrelated_stores() {
        let mut rig = Rig::new();
        rig.core(0).exclusive_load(LOCK);
        rig.core(0).write32(LOCK, 5, true).unwrap();
        rig.core(1).write32(LOCK + 4, 5, true).unwrap();
        assert!(rig.core(0).exclusive_store(LOCK));
        assert!(!rig.core(0).exclusive_store(LOCK), "the mark is used up");
    }

    #[test]
    fn an_exclusive_store_by_one_core_fails_the_other() {
        let mut rig = Rig::new();
        rig.core(0).exclusive_load(LOCK);
        rig.core(1).exclusive_load(LOCK);
        assert!(rig.core(1).exclusive_store(LOCK));
        rig.core(1).write32(LOCK, 1, true).unwrap();
        assert!(!rig.core(0).exclusive_store(LOCK));
    }

    #[test]
    fn the_debug_id_register_reads_and_the_rest_of_cp14_is_undefined() {
        let mut rig = Rig::new();
        let reg = |crm, opc2| CpReg {
            cp: 14,
            opc1: 0,
            crn: 0,
            crm,
            opc2,
        };
        let didr = rig.core(0).coproc_read(reg(0, 0), true).unwrap();
        assert_eq!(didr >> 16 & 0xF, 1, "ARMv6 debug");
        assert_eq!(rig.core(0).coproc_read(reg(1, 0), true), Some(0));
        assert_eq!(rig.core(0).coproc_read(reg(1, 0), false), None);
        assert_eq!(rig.core(0).coproc_read(reg(0, 4), true), None);
    }
}
