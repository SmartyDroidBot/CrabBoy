//! The ARM9 side of the machine: what the security processor sees when it
//! touches memory or its coprocessor.
//!
//! An access is checked against the protection unit first. It then lands, in
//! order of priority, in the instruction TCM, the data TCM, or on the bus
//! (3dbrew, "Memory layout"): ARM9 work RAM, I/O, the shared memories, and the
//! boot ROM at the top of the address space.

pub mod cp15;

use crate::bus::PhysMem;
use crate::io::Io;
use crate::sched::Scheduler;
use arm_core::{Abort, Bus, CpEffect, CpReg};
use cp15::{perm, Cp15};
use emu_core::Mem;

pub const ITCM_LEN: usize = 0x8000;
pub const DTCM_LEN: usize = 0x4000;
pub const BOOT9_BASE: u32 = 0xFFFF_0000;
pub const BOOT9_LEN: usize = 0x1_0000;

/// State private to the ARM9.
pub struct Arm9 {
    pub cp15: Cp15,
    pub itcm: Mem<u8, ITCM_LEN>,
    pub dtcm: Mem<u8, DTCM_LEN>,
    /// The boot ROM, or the stand-in vectors of the boot shim.
    pub boot9: Mem<u8, BOOT9_LEN>,
}

impl Default for Arm9 {
    fn default() -> Self {
        Self::new()
    }
}

impl Arm9 {
    pub fn new() -> Self {
        Arm9 {
            cp15: Cp15::new(),
            itcm: Mem::zeroed(),
            dtcm: Mem::zeroed(),
            boot9: Mem::zeroed(),
        }
    }
}

/// The ARM9's view of the machine for the duration of a step.
pub struct Arm9Bus<'a> {
    pub arm9: &'a mut Arm9,
    pub mem: &'a mut PhysMem,
    pub io: &'a mut Io,
    pub sched: &'a mut Scheduler,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Read,
    Write,
    Execute,
}

fn get(bytes: &[u8], at: usize, len: usize) -> u32 {
    let mut word = [0u8; 4];
    word[..len].copy_from_slice(&bytes[at..at + len]);
    u32::from_le_bytes(word)
}

fn put(bytes: &mut [u8], at: usize, len: usize, value: u32) {
    bytes[at..at + len].copy_from_slice(&value.to_le_bytes()[..len]);
}

impl Arm9Bus<'_> {
    fn check(&self, addr: u32, access: Access, privileged: bool) -> Result<(), Abort> {
        let needed = match (access, privileged) {
            (Access::Read, true) => perm::PRIV_READ,
            (Access::Read, false) => perm::USER_READ,
            (Access::Write, true) => perm::PRIV_WRITE,
            (Access::Write, false) => perm::USER_WRITE,
            (Access::Execute, true) => perm::PRIV_EXEC,
            (Access::Execute, false) => perm::USER_EXEC,
        };
        if self.arm9.cp15.page(addr) & needed != 0 {
            Ok(())
        } else {
            Err(Abort)
        }
    }

    /// Read `len` bytes (1, 2 or 4) at an address aligned to `len`.
    fn read(&mut self, addr: u32, len: usize) -> Result<u32, Abort> {
        let cp15 = &self.arm9.cp15;
        if cp15.itcm.contains(addr) && cp15.itcm.readable {
            return Ok(get(&self.arm9.itcm[..], addr as usize % ITCM_LEN, len));
        }
        if cp15.dtcm.contains(addr) && cp15.dtcm.readable {
            let at = (addr - cp15.dtcm.base) as usize % DTCM_LEN;
            return Ok(get(&self.arm9.dtcm[..], at, len));
        }
        match addr >> 24 {
            0x10..=0x17 => {
                let word = self.io.read9(addr & !3, self.sched).ok_or(Abort)?;
                let shifted = word >> ((addr & 3) * 8);
                Ok(if len == 4 {
                    shifted
                } else {
                    shifted & ((1 << (len * 8)) - 1)
                })
            }
            0xFF if addr >= BOOT9_BASE => {
                let at = (addr - BOOT9_BASE) as usize;
                if at >= 0x8000 && self.io.boot9_protected() {
                    return Err(Abort);
                }
                Ok(get(&self.arm9.boot9[..], at, len))
            }
            _ => self
                .mem
                .slice(addr, len)
                .map(|bytes| get(bytes, 0, len))
                .ok_or(Abort),
        }
    }

    fn write(&mut self, addr: u32, len: usize, value: u32) -> Result<(), Abort> {
        let cp15 = &self.arm9.cp15;
        if cp15.itcm.contains(addr) {
            put(
                &mut self.arm9.itcm[..],
                addr as usize % ITCM_LEN,
                len,
                value,
            );
            return Ok(());
        }
        if cp15.dtcm.contains(addr) {
            let at = (addr - cp15.dtcm.base) as usize % DTCM_LEN;
            put(&mut self.arm9.dtcm[..], at, len, value);
            return Ok(());
        }
        match addr >> 24 {
            0x10..=0x17 => {
                let shift = (addr & 3) * 8;
                let mask = if len == 4 {
                    !0
                } else {
                    ((1u32 << (len * 8)) - 1) << shift
                };
                self.io
                    .write9(addr & !3, value << shift & mask, mask, self.sched)
                    .ok_or(Abort)?;
                if self.io.ndma.pending() {
                    self.io.run_ndma(self.mem, self.sched);
                }
                Ok(())
            }
            // The boot ROM ignores writes.
            0xFF if addr >= BOOT9_BASE => Ok(()),
            _ => {
                let bytes = self.mem.slice_mut(addr, len).ok_or(Abort)?;
                put(bytes, 0, len, value);
                Ok(())
            }
        }
    }
}

impl Bus for Arm9Bus<'_> {
    fn fetch16(&mut self, addr: u32, privileged: bool) -> Result<u16, Abort> {
        self.check(addr, Access::Execute, privileged)?;
        Ok(self.read(addr, 2)? as u16)
    }
    fn fetch32(&mut self, addr: u32, privileged: bool) -> Result<u32, Abort> {
        self.check(addr, Access::Execute, privileged)?;
        self.read(addr, 4)
    }
    fn read8(&mut self, addr: u32, privileged: bool) -> Result<u8, Abort> {
        self.check(addr, Access::Read, privileged)?;
        Ok(self.read(addr, 1)? as u8)
    }
    fn read16(&mut self, addr: u32, privileged: bool) -> Result<u16, Abort> {
        self.check(addr, Access::Read, privileged)?;
        Ok(self.read(addr, 2)? as u16)
    }
    fn read32(&mut self, addr: u32, privileged: bool) -> Result<u32, Abort> {
        self.check(addr, Access::Read, privileged)?;
        self.read(addr, 4)
    }
    fn write8(&mut self, addr: u32, value: u8, privileged: bool) -> Result<(), Abort> {
        self.check(addr, Access::Write, privileged)?;
        self.write(addr, 1, value as u32)
    }
    fn write16(&mut self, addr: u32, value: u16, privileged: bool) -> Result<(), Abort> {
        self.check(addr, Access::Write, privileged)?;
        self.write(addr, 2, value as u32)
    }
    fn write32(&mut self, addr: u32, value: u32, privileged: bool) -> Result<(), Abort> {
        self.check(addr, Access::Write, privileged)?;
        self.write(addr, 4, value)
    }
    fn coproc_read(&mut self, reg: CpReg, privileged: bool) -> Option<u32> {
        if !privileged {
            return None;
        }
        self.arm9.cp15.read(reg)
    }
    fn coproc_write(&mut self, reg: CpReg, value: u32, privileged: bool) -> Option<CpEffect> {
        if !privileged {
            return None;
        }
        self.arm9.cp15.write(reg, value)
    }
    fn high_vectors(&self) -> bool {
        self.arm9.cp15.high_vectors()
    }
}
