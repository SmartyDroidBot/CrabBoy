//! Differential test against the ARM7TDMI of `gba-core`.
//!
//! That interpreter is an independent implementation which passes the jsmolka
//! ARM and Thumb suites, so it serves as an oracle for the instructions ARMv4T
//! and ARMv5TE execute identically. Random instructions are generated from
//! templates that stay inside that common subset; what the generator avoids,
//! and why:
//!
//! * `r15` as an operand or destination, `SWI`, and anything that interworks
//!   on ARMv5 (loads into `r15`), because the architectures differ there;
//! * unaligned word and halfword loads, which rotate or load a byte on the
//!   ARM7TDMI only;
//! * block transfers with the base register in the list or an empty list;
//! * `MSR` to the control byte.
//!
//! The C flag after a multiply and C and V after a long multiply are
//! unpredictable on ARMv4 and are masked out of the comparison.

use arm_core::{Abort, Arch, Bus, Cpu};

const RAM: usize = 0x1000;
const CODE: u32 = 0x800;

#[derive(Clone)]
struct Ram(Vec<u8>);

impl Ram {
    fn at(addr: u32) -> usize {
        addr as usize & (RAM - 1)
    }
    fn get(&self, addr: u32, len: usize) -> u32 {
        (0..len).fold(0, |acc, i| {
            acc | (self.0[Ram::at(addr.wrapping_add(i as u32))] as u32) << (i * 8)
        })
    }
    fn put(&mut self, addr: u32, len: usize, value: u32) {
        for i in 0..len {
            self.0[Ram::at(addr.wrapping_add(i as u32))] = (value >> (i * 8)) as u8;
        }
    }
}

impl Bus for Ram {
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

/// The oracle aligns word and halfword addresses itself, as the ARM7TDMI
/// does on the bus.
impl gba_core::cpu::Bus for Ram {
    fn read8(&mut self, addr: u32) -> u32 {
        self.get(addr, 1)
    }
    fn read16(&mut self, addr: u32) -> u32 {
        self.get(addr & !1, 2)
    }
    fn read32(&mut self, addr: u32) -> u32 {
        self.get(addr & !3, 4)
    }
    fn write8(&mut self, addr: u32, value: u32) {
        self.put(addr, 1, value);
    }
    fn write16(&mut self, addr: u32, value: u32) {
        self.put(addr & !1, 2, value);
    }
    fn write32(&mut self, addr: u32, value: u32) {
        self.put(addr & !3, 4, value);
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
    fn bit(&mut self) -> u32 {
        self.next() & 1
    }
    /// Register values that exercise the flag and shift edge cases.
    fn value(&mut self) -> u32 {
        match self.below(8) {
            0 => 0,
            1 => 0xFFFF_FFFF,
            2 => 0x8000_0000,
            3 => 0x7FFF_FFFF,
            4 => self.below(64),
            _ => self.next(),
        }
    }
    /// A register number that is not `r15`.
    fn reg(&mut self) -> u32 {
        self.below(15)
    }
    fn reg_except(&mut self, other: u32) -> u32 {
        loop {
            let r = self.reg();
            if r != other {
                return r;
            }
        }
    }
}

/// What to leave out of the comparison for one instruction.
#[derive(Clone, Copy, Default)]
struct Ignore {
    carry: bool,
    overflow: bool,
}

struct Case {
    instr: u32,
    /// Registers must hold multiples of four (word loads must be aligned).
    aligned: bool,
    ignore: Ignore,
}

const AL: u32 = 0xE000_0000;

fn arm_case(rng: &mut Rng) -> Case {
    let cond = if rng.below(4) == 0 {
        rng.below(14) << 28
    } else {
        AL
    };
    let mut ignore = Ignore::default();
    let mut aligned = false;
    let body = match rng.below(10) {
        // Data processing, immediate or register operand, every shift form.
        0..=3 => {
            let opcode = rng.below(16);
            // The oracle clears V on the logical operations, which the ARM
            // ARM leaves unaffected; a defect of gba-core, not of ARMv4T.
            ignore.overflow = matches!(opcode, 0 | 1 | 8 | 9 | 12..=15);
            let compare = (8..12).contains(&opcode);
            let s = if compare { 1 } else { rng.bit() };
            let operand = match rng.below(3) {
                0 => 1 << 25 | rng.below(0x1000),
                1 => rng.below(32) << 7 | rng.below(4) << 5 | rng.reg(),
                _ => rng.reg() << 8 | rng.below(4) << 5 | 1 << 4 | rng.reg(),
            };
            opcode << 21 | s << 20 | rng.reg() << 16 | rng.reg() << 12 | operand
        }
        4 => {
            ignore.carry = true;
            let rd = rng.reg();
            rng.bit() << 21
                | rng.bit() << 20
                | rd << 16
                | rng.reg() << 12
                | rng.reg() << 8
                | 0x90
                | rng.reg_except(rd)
        }
        5 => {
            ignore.carry = true;
            ignore.overflow = true;
            let hi = rng.reg();
            let lo = rng.reg_except(hi);
            let rm = loop {
                let r = rng.reg();
                if r != hi && r != lo {
                    break r;
                }
            };
            1 << 23
                | rng.below(4) << 21
                | rng.bit() << 20
                | hi << 16
                | lo << 12
                | rng.reg() << 8
                | 0x90
                | rm
        }
        // Word and byte loads and stores, immediate offset.
        6 => {
            aligned = true;
            let rn = rng.reg();
            let load = rng.bit();
            let byte = rng.bit();
            let pre = rng.bit();
            let writeback = if pre == 1 { rng.bit() } else { 0 };
            let offset = if byte == 1 {
                rng.below(0x100)
            } else {
                rng.below(0x40) << 2
            };
            1 << 26
                | pre << 24
                | rng.bit() << 23
                | byte << 22
                | writeback << 21
                | load << 20
                | rn << 16
                | rng.reg_except(rn) << 12
                | offset
        }
        // Halfword and signed loads and stores, kept aligned: an unaligned
        // LDRSH loads a sign-extended byte on the ARM7TDMI only.
        7 => {
            aligned = true;
            let rn = rng.reg();
            let load = rng.bit();
            let kind = if load == 1 { 1 + rng.below(3) } else { 1 };
            let pre = rng.bit();
            let writeback = if pre == 1 { rng.bit() } else { 0 };
            let offset = rng.below(0x100) & if kind == 2 { !0 } else { !1 };
            pre << 24
                | rng.bit() << 23
                | 1 << 22
                | writeback << 21
                | load << 20
                | rn << 16
                | rng.reg_except(rn) << 12
                | (offset >> 4) << 8
                | 0x90
                | kind << 5
                | offset & 0xF
        }
        // Block transfers.
        8 => {
            aligned = true;
            let rn = rng.reg();
            let list = loop {
                let l = rng.below(0x8000) & !(1 << rn);
                if l != 0 {
                    break l;
                }
            };
            0b100 << 25
                | rng.bit() << 24
                | rng.bit() << 23
                | rng.bit() << 21
                | rng.bit() << 20
                | rn << 16
                | list
        }
        // Flag-only status register writes, and reads.
        _ => match rng.below(3) {
            0 => 0x010F_0000 | rng.reg() << 12,
            1 => 0x0128_F000 | rng.reg(),
            _ => 0x0328_F000 | rng.below(0x1000),
        },
    };
    Case {
        instr: cond | body,
        aligned,
        ignore,
    }
}

fn thumb_case(rng: &mut Rng) -> Case {
    let low = |rng: &mut Rng| rng.below(8);
    let mut ignore = Ignore::default();
    let mut aligned = false;
    let instr = match rng.below(12) {
        0 => rng.below(3) << 11 | rng.below(32) << 6 | low(rng) << 3 | low(rng),
        1 => 0b00011 << 11 | rng.below(4) << 9 | low(rng) << 6 | low(rng) << 3 | low(rng),
        2 => 0b001 << 13 | rng.below(4) << 11 | low(rng) << 8 | rng.below(256),
        3 | 4 => {
            let op = rng.below(16);
            ignore.carry = op == 0xD;
            0b010000 << 10 | op << 6 | low(rng) << 3 | low(rng)
        }
        // ADD, CMP, MOV with at least one high register (two low ones are
        // unpredictable on ARMv4T), never r15.
        5 => {
            let (rd, rm) = loop {
                let pair = (rng.reg(), rng.reg());
                if pair.0 >= 8 || pair.1 >= 8 {
                    break pair;
                }
            };
            0b010001 << 10 | rng.below(3) << 8 | (rd >> 3) << 7 | rm << 3 | rd & 7
        }
        6 => {
            aligned = true;
            0b0101 << 12 | rng.below(8) << 9 | low(rng) << 6 | low(rng) << 3 | low(rng)
        }
        7 => {
            aligned = true;
            0b011 << 13 | rng.below(4) << 11 | rng.below(32) << 6 | low(rng) << 3 | low(rng)
        }
        8 => 0b1000 << 12 | rng.bit() << 11 | rng.below(32) << 6 | low(rng) << 3 | low(rng),
        9 => {
            aligned = true;
            match rng.below(3) {
                0 => 0b1001 << 12 | rng.bit() << 11 | low(rng) << 8 | rng.below(256),
                1 => 0b1010 << 12 | rng.bit() << 11 | low(rng) << 8 | rng.below(256),
                _ => 0b1011_0000 << 8 | rng.below(256),
            }
        }
        // PUSH with or without lr, POP without pc, STMIA and LDMIA.
        10 => {
            aligned = true;
            match rng.below(3) {
                0 => 0b1011_0100 << 8 | rng.bit() << 8 | (1 + rng.below(255)),
                1 => 0b1011_1100 << 8 | (1 + rng.below(255)),
                _ => {
                    let rn = low(rng);
                    let list = loop {
                        let l = rng.below(256) & !(1 << rn);
                        if l != 0 {
                            break l;
                        }
                    };
                    0b1100 << 12 | rng.bit() << 11 | rn << 8 | list
                }
            }
        }
        // Branches: conditional, unconditional, and both halves of BL.
        _ => match rng.below(4) {
            0 => 0b1101 << 12 | rng.below(14) << 8 | rng.below(256),
            1 => 0b11100 << 11 | rng.below(0x800),
            2 => 0b11110 << 11 | rng.below(0x800),
            _ => 0b11111 << 11 | rng.below(0x800),
        },
    };
    Case {
        instr,
        aligned,
        ignore,
    }
}

const MODES: [u32; 6] = [0x10, 0x11, 0x12, 0x13, 0x17, 0x1F];

fn run(thumb: bool, iterations: u32, seed: u64) {
    let mut rng = Rng(seed);
    let mut ram = Ram(vec![0; RAM]);
    for byte in ram.0.iter_mut() {
        *byte = rng.next() as u8;
    }

    for iteration in 0..iterations {
        let case = if thumb {
            thumb_case(&mut rng)
        } else {
            arm_case(&mut rng)
        };
        let mut regs = [0u32; 15];
        for r in regs.iter_mut() {
            *r = rng.value();
            if case.aligned {
                *r &= !3;
            }
        }
        let cpsr =
            rng.next() & 0xF000_0000 | MODES[rng.below(6) as usize] | if thumb { 0x20 } else { 0 };

        ram.put(CODE, if thumb { 2 } else { 4 }, case.instr);
        let mut ours_ram = ram.clone();
        let mut theirs_ram = ram.clone();

        let mut ours = Cpu::new(Arch::V5te, &ours_ram);
        ours.set_cpsr(cpsr);
        let mut theirs = gba_core::Cpu::new();
        theirs.set_cpsr(cpsr);
        for (n, value) in regs.iter().enumerate() {
            ours.set_reg(n, *value);
            theirs.set_reg(n as u32, *value);
        }
        ours.set_reg(15, CODE);
        theirs.set_pc(CODE);

        ours.step(&mut ours_ram);
        theirs.execute(&mut theirs_ram);

        let mut mask = 0xF000_00FF;
        if case.ignore.carry {
            mask &= !(1 << 29);
        }
        if case.ignore.overflow {
            mask &= !(1 << 28);
        }
        let context = || {
            format!(
                "iteration {iteration}, {} {:#010x}, cpsr {cpsr:#010x}, regs {regs:08x?}",
                if thumb { "thumb" } else { "arm" },
                case.instr
            )
        };
        for n in 0..15 {
            assert_eq!(ours.reg(n), theirs.reg(n as u32), "r{n}: {}", context());
        }
        let align = if ours.thumb() { !1 } else { !3 };
        assert_eq!(
            ours.reg(15) & align,
            theirs.pc() & align,
            "pc: {}",
            context()
        );
        assert_eq!(
            ours.cpsr() & mask,
            theirs.cpsr() & mask,
            "cpsr: {}",
            context()
        );
        assert!(ours_ram.0 == theirs_ram.0, "memory: {}", context());
        ram = ours_ram;
    }
}

#[test]
fn arm_state_matches_the_arm7tdmi_on_the_common_subset() {
    run(false, 400_000, 0x9E37_79B9_7F4A_7C15);
}

#[test]
fn thumb_state_matches_the_arm7tdmi_on_the_common_subset() {
    run(true, 400_000, 0xD1B5_4A32_D192_ED03);
}
