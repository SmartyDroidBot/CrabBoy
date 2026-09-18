//! Run the jsmolka gba-tests CPU suites on the interpreter.
//!
//! The suites target the ARM7TDMI, but they only need memory shaped like a
//! GBA's to run, and they leave the number of the first failing test in `r12`
//! (zero when all pass) before parking in a loop. The ROMs are not part of
//! the repository; `fetch_test_roms` downloads them, and the tests skip when
//! they are absent.
//!
//! ARMv5TE differs from ARMv4T in places the suites probe, so a fixed list of
//! expected failures is compared instead of demanding zero. Each entry is
//! explained in `docs/3ds/arm9.md`.

use arm_core::{Abort, Arch, Bus, Cpu};
use std::path::PathBuf;

struct Gba {
    rom: Vec<u8>,
    ewram: Vec<u8>,
    iwram: Vec<u8>,
    /// Reads of DISPSTAT, whose vertical-blank flag the suites poll.
    dispstat_reads: u32,
}

impl Gba {
    fn region(&mut self, addr: u32) -> Option<(&mut Vec<u8>, usize)> {
        match addr >> 24 {
            0x02 => Some((&mut self.ewram, addr as usize & 0x3_FFFF)),
            0x03 => Some((&mut self.iwram, addr as usize & 0x7FFF)),
            0x08..=0x0D => {
                let at = addr as usize & 0x1FF_FFFF;
                (at + 4 <= self.rom.len()).then_some((&mut self.rom, at))
            }
            _ => None,
        }
    }

    fn get(&mut self, addr: u32, len: usize) -> u32 {
        if addr == 0x0400_0004 {
            // Flip the vertical-blank flag every few reads.
            self.dispstat_reads += 1;
            return self.dispstat_reads >> 2 & 1;
        }
        match self.region(addr) {
            Some((bytes, at)) if at + len <= bytes.len() => {
                let mut word = [0u8; 4];
                word[..len].copy_from_slice(&bytes[at..at + len]);
                u32::from_le_bytes(word)
            }
            _ => 0,
        }
    }

    fn put(&mut self, addr: u32, len: usize, value: u32) {
        if addr >> 24 >= 0x08 {
            return;
        }
        if let Some((bytes, at)) = self.region(addr) {
            if at + len <= bytes.len() {
                bytes[at..at + len].copy_from_slice(&value.to_le_bytes()[..len]);
            }
        }
    }
}

impl Bus for Gba {
    fn fetch16(&mut self, addr: u32, _: bool) -> Result<u16, Abort> {
        Ok(self.get(addr, 2) as u16)
    }
    fn fetch32(&mut self, addr: u32, _: bool) -> Result<u32, Abort> {
        Ok(self.get(addr, 4))
    }
    fn read8(&mut self, addr: u32, _: bool) -> Result<u8, Abort> {
        Ok(self.get(addr, 1) as u8)
    }
    fn read16(&mut self, addr: u32, _: bool) -> Result<u16, Abort> {
        Ok(self.get(addr, 2) as u16)
    }
    fn read32(&mut self, addr: u32, _: bool) -> Result<u32, Abort> {
        Ok(self.get(addr, 4))
    }
    fn write8(&mut self, addr: u32, value: u8, _: bool) -> Result<(), Abort> {
        self.put(addr, 1, value as u32);
        Ok(())
    }
    fn write16(&mut self, addr: u32, value: u16, _: bool) -> Result<(), Abort> {
        self.put(addr, 2, value as u32);
        Ok(())
    }
    fn write32(&mut self, addr: u32, value: u32, _: bool) -> Result<(), Abort> {
        self.put(addr, 4, value);
        Ok(())
    }
}

/// The numbers of the failing tests, or `None` when the ROM is not there.
///
/// A suite stops at its first failure: the failing test branches to a stub
/// that loads its number (`mov r12, #n` in the ARM suite, `mov r7, #n` in the
/// Thumb one) and enters the evaluation routine. The instruction before that
/// stub is the passing path's branch to the next test, so the harness records
/// the number and resumes there.
fn failures(suite: &str) -> Option<Vec<u32>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../roms/test-suites/gba/jsmolka")
        .join(suite)
        .join(format!("{suite}.gba"));
    let rom = std::fs::read(path).ok()?;
    let mut bus = Gba {
        rom,
        ewram: vec![0; 0x4_0000],
        iwram: vec![0; 0x8000],
        dispstat_reads: 0,
    };
    let mut cpu = Cpu::new(Arch::V5te, &bus);
    // What the GBA BIOS leaves: system mode, the stacks set up.
    cpu.set_cpsr(0x1F);
    cpu.set_reg(13, 0x0300_7F00);
    cpu.jump(0x0800_0000);

    // The evaluation routine starts by testing the failed-test register,
    // `movs r12, r12` (ARM suite) or `movs r12, r7` (Thumb suite), before it
    // draws the verdict with GBA video hardware.
    const EVALUATE: [(u32, usize); 2] = [(0xE1B0_C00C, 12), (0xE1B0_C007, 7)];
    let mut failed = Vec::new();
    let mut stub = 0;
    for _ in 0..20_000_000 {
        let pc = cpu.reg(15);
        if cpu.thumb() {
            if bus.get(pc, 2) & 0xFF00 == 0x2700 {
                stub = pc | 1;
            }
        } else {
            let next = bus.get(pc, 4);
            if next & 0xFFFF_F000 == 0xE3A0_C000 {
                stub = pc;
            }
            if let Some((_, reg)) = EVALUATE.iter().find(|(word, _)| *word == next) {
                match cpu.reg(*reg) {
                    0 => return Some(failed),
                    // The last test of a suite has nothing to resume into.
                    number if failed.last() == Some(&number) => return Some(failed),
                    number => failed.push(number),
                }
                assert!(failed.len() < 100, "{suite}: runaway, {failed:?}");
                cpu.set_cpsr(0x1F);
                // Resume at the branch to the next test. A test that ends by
                // falling into its stub has none; the next test then follows
                // the stub (three ARM instructions, or `mov` and `bl`).
                let resume = if stub & 1 != 0 {
                    match bus.get(stub - 3, 2) >> 11 {
                        0b11100 => stub - 2,
                        _ => stub + 6,
                    }
                } else {
                    match bus.get(stub - 4, 4) >> 24 {
                        0xEA => stub - 4,
                        _ => stub + 12,
                    }
                };
                cpu.jump(resume);
                continue;
            }
        }
        if !(0x0800_0000..0x0A00_0000).contains(&pc) {
            // Execution left the ROM: ARMv5 interworking took a test
            // somewhere the ARM7TDMI would not go.
            failed.push(DERAILED);
            return Some(failed);
        }
        cpu.step(&mut bus);
    }
    panic!(
        "{suite}: never finished, pc {:#010x}, failed {failed:?}",
        cpu.reg(15)
    );
}

/// Marks the point where a suite could not continue.
const DERAILED: u32 = u32::MAX;

/// ARMv5TE differs from the ARM7TDMI here; see `docs/3ds/arm9.md`.
const EXPECTED_ARM: &[u32] = &[
    234, // a compare with Rd = r15 restores the CPSR on the ARM7TDMI
    355, 408, 409, 452, // misaligned loads and swaps rotate, LDRSH loads a byte
    513, 515, 530, 531, 532, // an empty register list transfers r15
    516, // LDM with the base first in the list does not write back
    522, 523, 524, 525, 526, 527, 528, 529, // STM stores the updated base
];
const EXPECTED_THUMB: &[u32] = &[
    204, 211, 212, 216, 219, 221, // misaligned loads, as above
    // Test 223 pops an even address into r15, which enters ARM state on
    // ARMv5, so the rest of the suite cannot run.
    DERAILED,
];

fn check(suite: &str, expected: &[u32]) {
    let Some(mut failed) = failures(suite) else {
        return;
    };
    let mut expected = expected.to_vec();
    failed.sort_unstable();
    expected.sort_unstable();
    assert_eq!(failed, expected, "failing tests of {suite}.gba");
}

#[test]
fn arm_suite() {
    check("arm", EXPECTED_ARM);
}

#[test]
fn thumb_suite() {
    check("thumb", EXPECTED_THUMB);
}
