//! The ARMv6 memory management unit of the ARM11 (ARM ARM, part B, "Virtual
//! Memory System Architecture").
//!
//! Translation walks the two-level short-descriptor tables: sections and
//! supersections at the first level, large and small pages at the second,
//! with the ARMv6 `APX` and `XN` bits when the control register's `XP` bit is
//! set and the ARMv5 subpage permissions when it is clear. Results are kept
//! in a direct-mapped software TLB that every TLB maintenance operation and
//! every change of a translation register empties. Address space identifiers
//! are not used to tag entries; a context switch empties the TLB instead,
//! which is slower but never stale.

/// What an access wants to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
    Read,
    Write,
    Execute,
}

/// Fault status codes (the low four bits of DFSR and IFSR).
pub mod fault {
    pub const TRANSLATION_SECTION: u32 = 0b0101;
    pub const TRANSLATION_PAGE: u32 = 0b0111;
    pub const DOMAIN_SECTION: u32 = 0b1001;
    pub const DOMAIN_PAGE: u32 = 0b1011;
    pub const PERMISSION_SECTION: u32 = 0b1101;
    pub const PERMISSION_PAGE: u32 = 0b1111;
    /// An external abort on a translation table walk, first level.
    pub const EXTERNAL_WALK_1: u32 = 0b1100;
    /// The same at the second level.
    pub const EXTERNAL_WALK_2: u32 = 0b1110;
    /// A precise external abort: the bus refused a translated address.
    pub const EXTERNAL: u32 = 0b1000;
}

/// A failed translation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fault {
    pub status: u32,
    pub domain: u32,
}

const CONTROL_MMU: u32 = 1 << 0;
const CONTROL_XP: u32 = 1 << 23;

const PRIV_READ: u8 = 1 << 0;
const PRIV_WRITE: u8 = 1 << 1;
const USER_READ: u8 = 1 << 2;
const USER_WRITE: u8 = 1 << 3;
const NO_EXECUTE: u8 = 1 << 4;
const ALL: u8 = PRIV_READ | PRIV_WRITE | USER_READ | USER_WRITE;

const TLB_ENTRIES: usize = 4096;

#[derive(Clone, Copy)]
struct TlbEntry {
    /// Virtual page number plus one, so zero means empty.
    tag: u32,
    physical_page: u32,
    rights: u8,
    /// For reporting a permission fault found on a TLB hit.
    domain: u8,
    section: bool,
}

const EMPTY: TlbEntry = TlbEntry {
    tag: 0,
    physical_page: 0,
    rights: 0,
    domain: 0,
    section: false,
};

pub struct Mmu {
    control: u32,
    ttbr: [u32; 2],
    ttbcr: u32,
    dacr: u32,
    tlb: Vec<TlbEntry>,
}

impl Default for Mmu {
    fn default() -> Self {
        Self::new()
    }
}

/// Rights from the `APX` and `AP` bits (ARM ARM, table B4-1, with the `S` and
/// `R` bits of the ARMv5 model taken as zero).
fn rights(apx: bool, ap: u32) -> u8 {
    match (apx, ap) {
        (false, 1) => PRIV_READ | PRIV_WRITE,
        (false, 2) => PRIV_READ | PRIV_WRITE | USER_READ,
        (false, 3) => ALL,
        (true, 1) => PRIV_READ,
        (true, 2) | (true, 3) => PRIV_READ | USER_READ,
        _ => 0,
    }
}

impl Mmu {
    pub fn new() -> Self {
        Mmu {
            control: 0,
            ttbr: [0; 2],
            ttbcr: 0,
            dacr: 0,
            tlb: vec![EMPTY; TLB_ENTRIES],
        }
    }

    pub fn enabled(&self) -> bool {
        self.control & CONTROL_MMU != 0
    }

    pub fn flush(&mut self) {
        self.tlb.fill(EMPTY);
    }

    /// The translation-relevant bits of the control register changed.
    pub fn set_control(&mut self, control: u32) {
        self.control = control;
        self.flush();
    }

    pub fn ttbr(&self, n: usize) -> u32 {
        self.ttbr[n]
    }

    pub fn set_ttbr(&mut self, n: usize, value: u32) {
        self.ttbr[n] = value;
        self.flush();
    }

    pub fn ttbcr(&self) -> u32 {
        self.ttbcr
    }

    pub fn set_ttbcr(&mut self, value: u32) {
        self.ttbcr = value & 0x37;
        self.flush();
    }

    pub fn dacr(&self) -> u32 {
        self.dacr
    }

    pub fn set_dacr(&mut self, value: u32) {
        self.dacr = value;
        self.flush();
    }

    /// Translate `va`. `read_physical` fetches a word of a translation
    /// table; `None` from it is an external abort on the walk.
    pub fn translate(
        &mut self,
        va: u32,
        access: Access,
        privileged: bool,
        read_physical: impl FnMut(u32) -> Option<u32>,
    ) -> Result<u32, Fault> {
        if !self.enabled() {
            return Ok(va);
        }
        let slot = (va >> 12) as usize % TLB_ENTRIES;
        let tag = (va >> 12) + 1;
        if self.tlb[slot].tag != tag {
            self.tlb[slot] = self.walk(va, read_physical)?;
        }
        let entry = self.tlb[slot];
        let needed = match (access, privileged) {
            (Access::Write, true) => PRIV_WRITE,
            (Access::Write, false) => USER_WRITE,
            (_, true) => PRIV_READ,
            (_, false) => USER_READ,
        };
        let forbidden = entry.rights & needed == 0
            || (access == Access::Execute && entry.rights & NO_EXECUTE != 0);
        if forbidden {
            return Err(Fault {
                status: if entry.section {
                    fault::PERMISSION_SECTION
                } else {
                    fault::PERMISSION_PAGE
                },
                domain: entry.domain as u32,
            });
        }
        Ok(entry.physical_page << 12 | va & 0xFFF)
    }

    /// Walk the tables for the 4 KB page holding `va`.
    fn walk(
        &self,
        va: u32,
        mut read_physical: impl FnMut(u32) -> Option<u32>,
    ) -> Result<TlbEntry, Fault> {
        let xp = self.control & CONTROL_XP != 0;
        // With N > 0, addresses whose top N bits are zero use TTBR0 and a
        // table of 2^(12-N) entries; the rest use TTBR1.
        let n = self.ttbcr & 7;
        let (base, index) = if n != 0 && va >> (32 - n) == 0 {
            (self.ttbr[0] & !((1 << (14 - n)) - 1), va >> 20)
        } else if n != 0 {
            (self.ttbr[1] & !0x3FFF, va >> 20)
        } else {
            (self.ttbr[0] & !0x3FFF, va >> 20)
        };
        let no_domain = |status| Fault { status, domain: 0 };
        let first = read_physical(base | index << 2).ok_or(no_domain(fault::EXTERNAL_WALK_1))?;

        let (physical_page, domain, section, apx, ap, xn) = match first & 3 {
            0b10 => {
                let apx = xp && first & 1 << 15 != 0;
                let ap = first >> 10 & 3;
                let xn = xp && first & 1 << 4 != 0;
                if xp && first & 1 << 18 != 0 {
                    // Supersection: 16 MB, always in domain 0.
                    let page = (first & 0xFF00_0000 | va & 0x00FF_F000) >> 12;
                    (page, 0, true, apx, ap, xn)
                } else {
                    let page = (first & 0xFFF0_0000 | va & 0x000F_F000) >> 12;
                    (page, first >> 5 & 0xF, true, apx, ap, xn)
                }
            }
            0b01 => {
                let domain = first >> 5 & 0xF;
                let second =
                    read_physical(first & !0x3FF | (va >> 12 & 0xFF) << 2).ok_or(Fault {
                        status: fault::EXTERNAL_WALK_2,
                        domain,
                    })?;
                let apx = xp && second & 1 << 9 != 0;
                match second & 3 {
                    0b01 => {
                        let page = (second & 0xFFFF_0000 | va & 0x0000_F000) >> 12;
                        let ap = if xp {
                            second >> 4 & 3
                        } else {
                            second >> (4 + (va >> 14 & 3) * 2) & 3
                        };
                        (page, domain, false, apx, ap, xp && second & 1 << 15 != 0)
                    }
                    0b10 | 0b11 if xp => (
                        second >> 12,
                        domain,
                        false,
                        apx,
                        second >> 4 & 3,
                        second & 1 != 0,
                    ),
                    0b10 => {
                        // ARMv5 small page: four subpages with their own AP.
                        let ap = second >> (4 + (va >> 10 & 3) * 2) & 3;
                        (second >> 12, domain, false, false, ap, false)
                    }
                    0b11 => (second >> 12, domain, false, false, second >> 4 & 3, false),
                    _ => {
                        return Err(Fault {
                            status: fault::TRANSLATION_PAGE,
                            domain,
                        })
                    }
                }
            }
            _ => return Err(no_domain(fault::TRANSLATION_SECTION)),
        };

        let rights = match self.dacr >> (domain * 2) & 3 {
            0b01 => rights(apx, ap) | if xn { NO_EXECUTE } else { 0 },
            // Managers bypass the permission bits, including XN.
            0b11 => ALL,
            _ => {
                return Err(Fault {
                    status: if section {
                        fault::DOMAIN_SECTION
                    } else {
                        fault::DOMAIN_PAGE
                    },
                    domain,
                })
            }
        };
        Ok(TlbEntry {
            tag: (va >> 12) + 1,
            physical_page,
            rights,
            domain: domain as u8,
            section,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const TABLE: u32 = 0x2000_0000;
    const COARSE: u32 = 0x2000_4000;

    struct Tables(HashMap<u32, u32>);

    impl Tables {
        fn reader(&self) -> impl FnMut(u32) -> Option<u32> + '_ {
            |addr| Some(self.0.get(&addr).copied().unwrap_or(0))
        }
    }

    fn mmu() -> Mmu {
        let mut mmu = Mmu::new();
        mmu.set_ttbr(0, TABLE);
        mmu.set_dacr(0b01);
        mmu.set_control(CONTROL_MMU | CONTROL_XP);
        mmu
    }

    #[test]
    fn disabled_translation_is_the_identity() {
        let tables = Tables(HashMap::new());
        let mut mmu = Mmu::new();
        assert_eq!(
            mmu.translate(0x1234_5678, Access::Write, false, tables.reader()),
            Ok(0x1234_5678)
        );
    }

    #[test]
    fn sections_and_supersections() {
        let mut tables = Tables(HashMap::new());
        // 0x00100000 -> 0x20300000, privileged read/write.
        tables.0.insert(TABLE + 4, 0x2030_0000 | 1 << 10 | 0b10);
        // 0x01000000 -> 0x28000000 as a supersection, full access.
        // It repeats in all sixteen entries it covers.
        for i in 0x10..0x20 {
            tables
                .0
                .insert(TABLE + i * 4, 0x2800_0000 | 1 << 18 | 3 << 10 | 0b10);
        }
        let mut mmu = mmu();
        assert_eq!(
            mmu.translate(0x0012_3456, Access::Read, true, tables.reader()),
            Ok(0x2032_3456)
        );
        assert_eq!(
            mmu.translate(0x0012_3456, Access::Read, false, tables.reader()),
            Err(Fault {
                status: fault::PERMISSION_SECTION,
                domain: 0
            })
        );
        assert_eq!(
            mmu.translate(0x01AB_CDEF, Access::Write, false, tables.reader()),
            Ok(0x28AB_CDEF)
        );
        assert_eq!(
            mmu.translate(0x0020_0000, Access::Read, true, tables.reader()),
            Err(Fault {
                status: fault::TRANSLATION_SECTION,
                domain: 0
            })
        );
    }

    #[test]
    fn small_and_large_pages_with_apx_and_xn() {
        let mut tables = Tables(HashMap::new());
        tables.0.insert(TABLE, COARSE | 3 << 5 | 0b01);
        // Page 1: small, read-only for everyone, no execute.
        tables
            .0
            .insert(COARSE + 4, 0x2040_0000 | 1 << 9 | 2 << 4 | 0b11);
        // Pages 0x10-0x1F: a large page, user read/write.
        for i in 0x10..0x20 {
            tables.0.insert(COARSE + i * 4, 0x2050_0000 | 3 << 4 | 0b01);
        }
        let mut mmu = mmu();
        mmu.set_dacr(0b01 << 6);
        assert_eq!(
            mmu.translate(0x1ABC, Access::Read, false, tables.reader()),
            Ok(0x2040_0ABC)
        );
        let denied = Err(Fault {
            status: fault::PERMISSION_PAGE,
            domain: 3,
        });
        assert_eq!(
            mmu.translate(0x1ABC, Access::Write, true, tables.reader()),
            denied
        );
        assert_eq!(
            mmu.translate(0x1ABC, Access::Execute, true, tables.reader()),
            denied
        );
        assert_eq!(
            mmu.translate(0x0001_5678, Access::Write, false, tables.reader()),
            Ok(0x2050_5678)
        );
        assert_eq!(
            mmu.translate(0x2000, Access::Read, true, tables.reader()),
            Err(Fault {
                status: fault::TRANSLATION_PAGE,
                domain: 3
            })
        );
    }

    #[test]
    fn domains_gate_and_managers_bypass_permissions() {
        let mut tables = Tables(HashMap::new());
        tables.0.insert(TABLE, 0x2030_0000 | 5 << 5 | 1 << 4 | 0b10);
        let mut mmu = mmu();
        assert_eq!(
            mmu.translate(0, Access::Read, true, tables.reader()),
            Err(Fault {
                status: fault::DOMAIN_SECTION,
                domain: 5
            })
        );
        mmu.set_dacr(0b11 << 10);
        assert_eq!(
            mmu.translate(0, Access::Execute, false, tables.reader()),
            Ok(0x2030_0000)
        );
    }

    #[test]
    fn the_address_space_splits_between_the_two_tables() {
        let mut tables = Tables(HashMap::new());
        let other = 0x2001_0000;
        tables.0.insert(TABLE, 0x2030_0000 | 3 << 10 | 0b10);
        tables
            .0
            .insert(other + 0x800 * 4, 0x2070_0000 | 3 << 10 | 0b10);
        let mut mmu = mmu();
        mmu.set_ttbr(1, other);
        mmu.set_ttbcr(1);
        assert_eq!(
            mmu.translate(0x10, Access::Read, true, tables.reader()),
            Ok(0x2030_0010)
        );
        assert_eq!(
            mmu.translate(0x8000_0010, Access::Read, true, tables.reader()),
            Ok(0x2070_0010)
        );
    }

    #[test]
    fn the_tlb_holds_a_translation_until_it_is_flushed() {
        let mut tables = Tables(HashMap::new());
        tables.0.insert(TABLE, 0x2030_0000 | 3 << 10 | 0b10);
        let mut mmu = mmu();
        mmu.translate(0, Access::Read, true, tables.reader())
            .unwrap();
        tables.0.insert(TABLE, 0x2040_0000 | 3 << 10 | 0b10);
        assert_eq!(
            mmu.translate(0, Access::Read, true, tables.reader()),
            Ok(0x2030_0000)
        );
        mmu.flush();
        assert_eq!(
            mmu.translate(0, Access::Read, true, tables.reader()),
            Ok(0x2040_0000)
        );
    }

    #[test]
    fn a_table_outside_memory_is_an_external_abort_on_the_walk() {
        let mut mmu = mmu();
        assert_eq!(
            mmu.translate(0, Access::Read, true, |_| None),
            Err(Fault {
                status: fault::EXTERNAL_WALK_1,
                domain: 0
            })
        );
    }
}
