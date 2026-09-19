//! The system control coprocessor of one ARM11 MPCore core.
//!
//! Identification and reset values are those of the 3DS as recorded by the
//! Azahar emulator; the writable control bits are from GBATEK ("ARM CP15
//! System Control Coprocessor"). Caches, branch prediction and the
//! performance monitor are not modelled: their registers accept writes and
//! cache maintenance does nothing, which is coherent because memory is never
//! cached.

use super::mmu::{Fault, Mmu};
use arm_core::{CpEffect, CpReg};

const MAIN_ID: u32 = 0x410F_B024;
const TLB_TYPE: u32 = 0x0000_0800;
/// Harvard caches, 16 KB each, four-way, eight words per line. The encoding
/// follows the ARM ARM; the value a 3DS reports has not been confirmed.
const CACHE_TYPE: u32 = 0x1D15_2152;

pub mod control {
    pub const MMU: u32 = 1 << 0;
    pub const HIGH_VECTORS: u32 = 1 << 13;
    pub const UNALIGNED: u32 = 1 << 22;
    /// Bits 3-6, 14, 16 and 18 always read as one.
    pub const ALWAYS_SET: u32 = 0x0005_4078;
    /// Bits 0-2, 8-9, 11-13, 15, 22-23, 25 and 28-29.
    pub const WRITABLE: u32 = 0x32C0_BB07;
}

pub struct Cp15 {
    core: u32,
    control: u32,
    auxiliary_control: u32,
    coprocessor_access: u32,
    pub mmu: Mmu,
    data_fault_status: u32,
    instruction_fault_status: u32,
    fault_address: u32,
    watchpoint_fault_address: u32,
    fcse_pid: u32,
    context_id: u32,
    /// User read/write, user read-only and privileged-only thread registers.
    thread: [u32; 3],
}

impl Cp15 {
    pub fn new(core: u32) -> Self {
        Cp15 {
            core,
            control: control::ALWAYS_SET,
            auxiliary_control: 0xF,
            coprocessor_access: 0,
            mmu: Mmu::new(),
            data_fault_status: 0,
            instruction_fault_status: 0,
            fault_address: 0,
            watchpoint_fault_address: 0,
            fcse_pid: 0,
            context_id: 0,
            thread: [0; 3],
        }
    }

    pub fn control(&self) -> u32 {
        self.control
    }

    pub fn high_vectors(&self) -> bool {
        self.control & control::HIGH_VECTORS != 0
    }

    pub fn unaligned_access(&self) -> bool {
        self.control & control::UNALIGNED != 0
    }

    /// Whether the coprocessor access register opens coprocessors 10 and 11
    /// to this mode: 0b01 is privileged only, 0b11 everyone.
    pub fn vfp_access(&self, privileged: bool) -> bool {
        match self.coprocessor_access >> 20 & 3 {
            0b11 => true,
            0b01 => privileged,
            _ => false,
        }
    }

    /// Record a data abort for the handler to read.
    pub fn data_fault(&mut self, fault: Fault, addr: u32, write: bool) {
        self.data_fault_status = fault.status | fault.domain << 4 | (write as u32) << 11;
        self.fault_address = addr;
    }

    /// Record a prefetch abort.
    pub fn instruction_fault(&mut self, fault: Fault) {
        self.instruction_fault_status = fault.status;
    }

    pub fn read(&self, reg: CpReg, privileged: bool) -> Option<u32> {
        if reg.cp != 15 || reg.opc1 != 0 {
            return None;
        }
        let key = (reg.crn, reg.crm, reg.opc2);
        if !privileged {
            // User code may read only its two thread registers.
            return match key {
                (13, 0, 2) => Some(self.thread[0]),
                (13, 0, 3) => Some(self.thread[1]),
                _ => None,
            };
        }
        Some(match key {
            (0, 0, 1) => CACHE_TYPE,
            (0, 0, 3) => TLB_TYPE,
            (0, 0, 5) => self.core,
            (0, 0, _) => MAIN_ID,
            (0, 1, 0) => 0x111,
            (0, 1, 1) => 0x1,
            (0, 1, 2) => 0x2,
            (0, 1, 3) => 0,
            (0, 1, 4) => 0x0110_0103,
            (0, 1, 5) => 0x1002_0302,
            (0, 1, 6) => 0x0122_2000,
            (0, 1, 7) => 0,
            (0, 2, 0) => 0x0010_0011,
            (0, 2, 1) => 0x1200_2111,
            (0, 2, 2) => 0x1122_1011,
            (0, 2, 3) => 0x0110_2131,
            (0, 2, 4) => 0x141,
            // The unused slots of the identification block read as zero;
            // Linux reads ID_ISAR5 on anything with this scheme.
            (0, 2..=7, _) => 0,
            (1, 0, 0) => self.control,
            (1, 0, 1) => self.auxiliary_control,
            (1, 0, 2) => self.coprocessor_access,
            (2, 0, 0) => self.mmu.ttbr(0),
            (2, 0, 1) => self.mmu.ttbr(1),
            (2, 0, 2) => self.mmu.ttbcr(),
            (3, 0, 0) => self.mmu.dacr(),
            (5, 0, 0) => self.data_fault_status,
            (5, 0, 1) => self.instruction_fault_status,
            (6, 0, 0) => self.fault_address,
            (6, 0, 1) => self.watchpoint_fault_address,
            (13, 0, 0) => self.fcse_pid,
            (13, 0, 1) => self.context_id,
            (13, 0, n @ 2..=4) => self.thread[n as usize - 2],
            // Cache lockdown, TLB lockdown, remap and performance monitor
            // registers read as zero.
            (9, _, _) | (10, _, _) | (15, _, _) => 0,
            _ => return None,
        })
    }

    pub fn write(&mut self, reg: CpReg, value: u32, privileged: bool) -> Option<CpEffect> {
        if reg.cp != 15 || reg.opc1 != 0 {
            return None;
        }
        let key = (reg.crn, reg.crm, reg.opc2);
        if !privileged {
            // The thread register and the three barriers.
            return match key {
                (13, 0, 2) => {
                    self.thread[0] = value;
                    Some(CpEffect::None)
                }
                (7, 5, 4) | (7, 10, 4) | (7, 10, 5) => Some(CpEffect::None),
                _ => None,
            };
        }
        match key {
            (1, 0, 0) => {
                self.control = value & control::WRITABLE | control::ALWAYS_SET;
                self.mmu.set_control(self.control);
            }
            (1, 0, 1) => self.auxiliary_control = value & 0x7F,
            (1, 0, 2) => self.coprocessor_access = value & 0x00F0_0000,
            (2, 0, 0) => self.mmu.set_ttbr(0, value),
            (2, 0, 1) => self.mmu.set_ttbr(1, value),
            (2, 0, 2) => self.mmu.set_ttbcr(value),
            (3, 0, 0) => self.mmu.set_dacr(value),
            (5, 0, 0) => self.data_fault_status = value,
            (5, 0, 1) => self.instruction_fault_status = value,
            (6, 0, 0) => self.fault_address = value,
            (6, 0, 1) => self.watchpoint_fault_address = value,
            (7, 0, 4) | (7, 8, 2) => return Some(CpEffect::WaitForInterrupt),
            (7, _, _) => {}
            (8, _, _) => self.mmu.flush(),
            (13, 0, 0) => self.fcse_pid = value & 0xFE00_0000,
            (13, 0, 1) => {
                self.context_id = value;
                self.mmu.flush();
            }
            (13, 0, n @ 2..=4) => self.thread[n as usize - 2] = value,
            (9, _, _) | (10, _, _) | (15, _, _) => {}
            _ => return None,
        }
        Some(CpEffect::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(crn: u8, crm: u8, opc2: u8) -> CpReg {
        CpReg {
            cp: 15,
            opc1: 0,
            crn,
            crm,
            opc2,
        }
    }

    #[test]
    fn identifies_the_core() {
        let cp15 = Cp15::new(1);
        assert_eq!(cp15.read(reg(0, 0, 0), true), Some(0x410F_B024));
        assert_eq!(cp15.read(reg(0, 0, 5), true), Some(1));
        assert_eq!(cp15.read(reg(1, 0, 0), true), Some(0x0005_4078));
        assert_eq!(cp15.read(reg(1, 0, 1), true), Some(0xF));
    }

    #[test]
    fn the_control_register_keeps_its_fixed_bits_and_drives_the_mmu() {
        let mut cp15 = Cp15::new(0);
        cp15.write(reg(1, 0, 0), 0xFFFF_FFFF, true);
        assert_eq!(cp15.control(), 0x32C0_BB07 | 0x0005_4078);
        assert!(cp15.mmu.enabled() && cp15.high_vectors() && cp15.unaligned_access());
        cp15.write(reg(1, 0, 0), 0, true);
        assert_eq!(cp15.control(), 0x0005_4078);
        assert!(!cp15.mmu.enabled());
    }

    #[test]
    fn user_mode_reaches_only_the_thread_registers_and_barriers() {
        let mut cp15 = Cp15::new(0);
        cp15.write(reg(13, 0, 3), 0x1234, true);
        assert_eq!(cp15.read(reg(13, 0, 3), false), Some(0x1234));
        assert_eq!(cp15.write(reg(13, 0, 3), 0, false), None);
        assert_eq!(cp15.write(reg(13, 0, 2), 7, false), Some(CpEffect::None));
        assert_eq!(cp15.read(reg(13, 0, 2), false), Some(7));
        assert_eq!(cp15.read(reg(13, 0, 4), false), None);
        assert_eq!(cp15.read(reg(1, 0, 0), false), None);
        assert_eq!(cp15.write(reg(7, 10, 4), 0, false), Some(CpEffect::None));
        assert_eq!(cp15.write(reg(7, 0, 4), 0, false), None);
    }

    #[test]
    fn faults_are_recorded_for_the_handler() {
        let mut cp15 = Cp15::new(0);
        let fault = Fault {
            status: 0b1111,
            domain: 3,
        };
        cp15.data_fault(fault, 0xDEAD_BEEF, true);
        assert_eq!(
            cp15.read(reg(5, 0, 0), true),
            Some(0b1111 | 3 << 4 | 1 << 11)
        );
        assert_eq!(cp15.read(reg(6, 0, 0), true), Some(0xDEAD_BEEF));
    }

    #[test]
    fn coprocessor_access_gates_the_vfp_by_mode() {
        let mut cp15 = Cp15::new(0);
        assert!(!cp15.vfp_access(true));
        cp15.write(reg(1, 0, 2), 0x0050_0000, true);
        assert!(cp15.vfp_access(true) && !cp15.vfp_access(false));
        cp15.write(reg(1, 0, 2), 0x00F0_0000, true);
        assert!(cp15.vfp_access(false));
    }
}
