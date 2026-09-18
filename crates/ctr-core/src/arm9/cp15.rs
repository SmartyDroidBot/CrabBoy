//! The system control coprocessor of the ARM946E-S: control register,
//! protection unit and tightly coupled memories (GBATEK, "ARM CP15 System
//! Control Coprocessor"; lioncash's ARMBook for the 3DS identification
//! values). The core has no fault status or fault address registers.

use arm_core::{CpEffect, CpReg};

/// Control register bits.
pub mod control {
    pub const MPU: u32 = 1 << 0;
    pub const HIGH_VECTORS: u32 = 1 << 13;
    pub const DTCM: u32 = 1 << 16;
    pub const DTCM_LOAD: u32 = 1 << 17;
    pub const ITCM: u32 = 1 << 18;
    pub const ITCM_LOAD: u32 = 1 << 19;
    /// Bits that read as written; bits 3-6 always read as one.
    pub const WRITABLE: u32 = 0x000F_F085;
    pub const ALWAYS_SET: u32 = 0x78;
}

const MAIN_ID: u32 = 0x4105_9461;
const CACHE_TYPE: u32 = 0x0F0D_2112;
const TCM_SIZE: u32 = 0x0014_0180;

/// Permission bits of one 4 KB page.
pub mod perm {
    pub const PRIV_READ: u8 = 1 << 0;
    pub const PRIV_WRITE: u8 = 1 << 1;
    pub const USER_READ: u8 = 1 << 2;
    pub const USER_WRITE: u8 = 1 << 3;
    pub const PRIV_EXEC: u8 = 1 << 4;
    pub const USER_EXEC: u8 = 1 << 5;
    pub const ALL: u8 = 0x3F;
}

/// A tightly coupled memory's place in the address space.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TcmWindow {
    pub base: u32,
    /// Last address of the window.
    pub last: u32,
    /// False in load mode, when reads fall through to the bus.
    pub readable: bool,
    enabled: bool,
}

impl TcmWindow {
    #[inline]
    pub fn contains(&self, addr: u32) -> bool {
        self.enabled && addr >= self.base && addr <= self.last
    }
}

pub struct Cp15 {
    control: u32,
    cacheable: [u32; 2],
    bufferable: u32,
    /// Extended access permissions: data, then instruction.
    access: [u32; 2],
    regions: [u32; 8],
    dtcm_region: u32,
    itcm_region: u32,
    /// Permission bits per 4 KB page, rebuilt when a register changes.
    pages: Vec<u8>,
    pub dtcm: TcmWindow,
    pub itcm: TcmWindow,
}

impl Default for Cp15 {
    fn default() -> Self {
        Self::new()
    }
}

/// Decode a 4-bit access permission field into read and write rights for
/// privileged and user modes.
fn rights(ap: u32) -> (bool, bool, bool, bool) {
    match ap {
        1 => (true, true, false, false),
        2 => (true, true, true, false),
        3 => (true, true, true, true),
        5 => (true, false, false, false),
        6 => (true, false, true, false),
        _ => (false, false, false, false),
    }
}

/// Widen a standard register (two bits per region) to the extended layout.
fn widen(standard: u32) -> u32 {
    (0..8).fold(0, |acc, n| acc | (standard >> (n * 2) & 3) << (n * 4))
}

fn narrow(extended: u32) -> u32 {
    (0..8).fold(0, |acc, n| acc | (extended >> (n * 4) & 3) << (n * 2))
}

impl Cp15 {
    pub fn new() -> Self {
        let mut cp15 = Cp15 {
            control: control::ALWAYS_SET,
            cacheable: [0; 2],
            bufferable: 0,
            access: [0; 2],
            regions: [0; 8],
            dtcm_region: 0,
            itcm_region: 0,
            pages: vec![perm::ALL; 1 << 20],
            dtcm: TcmWindow::default(),
            itcm: TcmWindow::default(),
        };
        cp15.rebuild();
        cp15
    }

    pub fn control(&self) -> u32 {
        self.control
    }

    pub fn high_vectors(&self) -> bool {
        self.control & control::HIGH_VECTORS != 0
    }

    /// The permission bits of the page holding `addr`.
    #[inline]
    pub fn page(&self, addr: u32) -> u8 {
        self.pages[(addr >> 12) as usize]
    }

    fn rebuild(&mut self) {
        let window = |region: u32, enabled: bool, load: bool, fixed_base: Option<u32>| {
            if !enabled {
                return TcmWindow::default();
            }
            // Size is 512 << N bytes, N from 3 (4 KB) to 23 (4 GB).
            let n = (region >> 1 & 0x1F).clamp(3, 23);
            let size = 512u64 << n;
            let base = fixed_base.unwrap_or(region & !0xFFF) & !(size - 1) as u32;
            TcmWindow {
                base,
                last: (base as u64 + size - 1).min(u32::MAX as u64) as u32,
                readable: !load,
                enabled: true,
            }
        };
        let c = self.control;
        self.dtcm = window(
            self.dtcm_region,
            c & control::DTCM != 0,
            c & control::DTCM_LOAD != 0,
            None,
        );
        // The instruction TCM always starts at zero.
        self.itcm = window(
            self.itcm_region,
            c & control::ITCM != 0,
            c & control::ITCM_LOAD != 0,
            Some(0),
        );

        if c & control::MPU == 0 {
            self.pages.fill(perm::ALL);
            return;
        }
        // Higher-numbered regions take priority; outside every region there
        // is no access.
        self.pages.fill(0);
        for (n, region) in self.regions.iter().enumerate() {
            if region & 1 == 0 {
                continue;
            }
            let size = 2u64 << (region >> 1 & 0x1F).max(11);
            let base = (region & !0xFFF) as u64 & !(size - 1);
            let (pr, pw, ur, uw) = rights(self.access[0] >> (n * 4) & 0xF);
            let (px, _, ux, _) = rights(self.access[1] >> (n * 4) & 0xF);
            let bits = [
                (pr, perm::PRIV_READ),
                (pw, perm::PRIV_WRITE),
                (ur, perm::USER_READ),
                (uw, perm::USER_WRITE),
                (px, perm::PRIV_EXEC),
                (ux, perm::USER_EXEC),
            ]
            .iter()
            .fold(0, |acc, (on, bit)| if *on { acc | bit } else { acc });
            let first = (base >> 12) as usize;
            let last = (((base + size - 1).min(u32::MAX as u64)) >> 12) as usize;
            self.pages[first..=last].fill(bits);
        }
    }

    pub fn read(&self, reg: CpReg) -> Option<u32> {
        if reg.cp != 15 || reg.opc1 != 0 {
            return None;
        }
        Some(match (reg.crn, reg.crm, reg.opc2) {
            (0, 0, 1) => CACHE_TYPE,
            (0, 0, 2) => TCM_SIZE,
            (0, 0, _) => MAIN_ID,
            (1, 0, 0) => self.control,
            (2, 0, n @ 0..=1) => self.cacheable[n as usize],
            (3, 0, 0) => self.bufferable,
            (5, 0, n @ 0..=1) => narrow(self.access[n as usize]),
            (5, 0, n @ 2..=3) => self.access[n as usize - 2],
            (6, n @ 0..=7, 0..=1) => self.regions[n as usize],
            (9, 1, 0) => self.dtcm_region,
            (9, 1, 1) => self.itcm_region,
            (9, 0, 0..=1) => 0,
            _ => return None,
        })
    }

    pub fn write(&mut self, reg: CpReg, value: u32) -> Option<CpEffect> {
        if reg.cp != 15 || reg.opc1 != 0 {
            return None;
        }
        match (reg.crn, reg.crm, reg.opc2) {
            (1, 0, 0) => {
                self.control = value & control::WRITABLE | control::ALWAYS_SET;
            }
            (2, 0, n @ 0..=1) => self.cacheable[n as usize] = value & 0xFF,
            (3, 0, 0) => self.bufferable = value & 0xFF,
            (5, 0, n @ 0..=1) => self.access[n as usize] = widen(value),
            (5, 0, n @ 2..=3) => self.access[n as usize - 2] = value,
            (6, n @ 0..=7, 0..=1) => self.regions[n as usize] = value & 0xFFFF_F03F,
            (9, 1, 0) => self.dtcm_region = value & 0xFFFF_F03E,
            (9, 1, 1) => self.itcm_region = value & 0xFFFF_F03E,
            (9, 0, 0..=1) => {}
            // Wait for interrupt, in both encodings.
            (7, 0, 4) | (7, 8, 2) => return Some(CpEffect::WaitForInterrupt),
            // Cache and write-buffer maintenance: there are no caches to
            // maintain.
            (7, _, _) => {}
            _ => return None,
        }
        if matches!(reg.crn, 1 | 5 | 6 | 9) {
            self.rebuild();
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
    fn identification_matches_the_3ds() {
        let cp15 = Cp15::new();
        assert_eq!(cp15.read(reg(0, 0, 0)), Some(0x4105_9461));
        assert_eq!(cp15.read(reg(0, 0, 1)), Some(0x0F0D_2112));
        assert_eq!(cp15.read(reg(0, 0, 2)), Some(0x0014_0180));
        assert_eq!(cp15.read(reg(0, 0, 5)), Some(0x4105_9461));
        assert_eq!(cp15.read(reg(1, 0, 0)), Some(0x78));
        assert_eq!(cp15.read(reg(4, 0, 0)), None);
    }

    #[test]
    fn the_boot_rom_tcm_setup_mirrors_the_itcm_below_128_mb() {
        let mut cp15 = Cp15::new();
        cp15.write(reg(9, 1, 1), 0x24);
        cp15.write(reg(9, 1, 0), 0xFFF0_000A);
        cp15.write(reg(1, 0, 0), 0x0005_0078);
        assert_eq!((cp15.itcm.base, cp15.itcm.last), (0, 0x07FF_FFFF));
        assert_eq!((cp15.dtcm.base, cp15.dtcm.last), (0xFFF0_0000, 0xFFF0_3FFF));
        assert!(cp15.itcm.readable && cp15.dtcm.contains(0xFFF0_3FFF));
        assert!(!cp15.dtcm.contains(0xFFF0_4000));

        cp15.write(reg(1, 0, 0), 0x000D_0078);
        assert!(!cp15.itcm.readable, "load mode makes the TCM write-only");
        cp15.write(reg(1, 0, 0), 0x78);
        assert!(!cp15.itcm.contains(0));
    }

    #[test]
    fn regions_grant_access_by_priority() {
        let mut cp15 = Cp15::new();
        // Region 0: 4 GB background, privileged read/write. Region 1: the
        // 1 MB at 0x08000000, read-only for everyone.
        cp15.write(reg(6, 0, 0), 0x0000_003F);
        cp15.write(reg(6, 1, 0), 0x0800_0027);
        cp15.write(reg(5, 0, 2), 0x0000_0061);
        cp15.write(reg(5, 0, 3), 0x0000_0001);
        assert_eq!(cp15.page(0x0800_0000), perm::ALL, "protection unit off");

        cp15.write(reg(1, 0, 0), 0x79);
        assert_eq!(
            cp15.page(0x2000_0000),
            perm::PRIV_READ | perm::PRIV_WRITE | perm::PRIV_EXEC
        );
        assert_eq!(cp15.page(0x080F_FFFF), perm::PRIV_READ | perm::USER_READ);
        assert_eq!(cp15.page(0x0810_0000) & perm::PRIV_WRITE, perm::PRIV_WRITE);

        // Without the background region nothing else is accessible.
        cp15.write(reg(6, 0, 0), 0);
        assert_eq!(cp15.page(0x2000_0000), 0);
    }

    #[test]
    fn standard_permission_registers_alias_the_extended_ones() {
        let mut cp15 = Cp15::new();
        cp15.write(reg(5, 0, 0), 0b11_10_01);
        assert_eq!(cp15.read(reg(5, 0, 2)), Some(0x321));
        assert_eq!(cp15.read(reg(5, 0, 0)), Some(0b11_10_01));
    }

    #[test]
    fn wait_for_interrupt_has_two_encodings() {
        let mut cp15 = Cp15::new();
        assert_eq!(
            cp15.write(reg(7, 0, 4), 0),
            Some(CpEffect::WaitForInterrupt)
        );
        assert_eq!(
            cp15.write(reg(7, 8, 2), 0),
            Some(CpEffect::WaitForInterrupt)
        );
        assert_eq!(cp15.write(reg(7, 10, 4), 0), Some(CpEffect::None));
    }
}
