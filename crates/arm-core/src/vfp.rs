//! VFPv2, the floating-point coprocessor of the ARM11 (ARM ARM, part C).
//!
//! Coprocessor 10 carries the single-precision forms of every instruction
//! and coprocessor 11 the double-precision ones. Arithmetic is done by
//! `softfloat` under the rounding mode, flush-to-zero and default-NaN
//! controls of the FPSCR, and accumulates the FPSCR's exception flags. Short
//! vectors (the FPSCR's LEN and STRIDE fields) are implemented.
//!
//! Not modelled: the trap-enable bits of the FPSCR, and the VFP11's habit of
//! bouncing the cases its hardware does not finish (subnormals and the like
//! outside RunFast mode) to support code through the undefined-instruction
//! vector. Results are always the IEEE ones.

use crate::bus::Bus;
use crate::cpu::{psr, Cpu, Exec, Trap};
use softfloat::{Env, Flags, Round, F32, F64};
use std::cmp::Ordering;

/// VFP11: implementer ARM, VFPv2, single and double precision.
const FPSID: u32 = 0x4101_20B4;

const FPEXC_ENABLE: u32 = 1 << 30;
const FPSCR_FLAGS: u32 = 0xF000_0000;
/// The cumulative exception bits, in `softfloat::Flags` positions.
const FPSCR_CUMULATIVE: u32 = 0x9F;
/// Flags, DN, FZ, rounding mode, stride, length, trap enables, cumulative.
const FPSCR_WRITABLE: u32 = 0xF3F7_9F9F;

/// The register file and the system registers.
#[derive(Clone, Default)]
pub struct Vfp {
    /// The 32 single registers; double `n` is singles `2n` (low) and `2n+1`.
    pub s: [u32; 32],
    pub fpscr: u32,
    pub fpexc: u32,
}

impl Vfp {
    pub fn d(&self, n: usize) -> u64 {
        self.s[2 * n] as u64 | (self.s[2 * n + 1] as u64) << 32
    }

    pub fn set_d(&mut self, n: usize, value: u64) {
        self.s[2 * n] = value as u32;
        self.s[2 * n + 1] = (value >> 32) as u32;
    }

    fn get(&self, double: bool, n: usize) -> u64 {
        if double {
            self.d(n)
        } else {
            self.s[n] as u64
        }
    }

    fn set(&mut self, double: bool, n: usize, value: u64) {
        if double {
            self.set_d(n, value);
        } else {
            self.s[n] = value as u32;
        }
    }

    fn env(&self) -> Env {
        Env {
            round: match self.fpscr >> 22 & 3 {
                0 => Round::Nearest,
                1 => Round::Up,
                2 => Round::Down,
                _ => Round::Zero,
            },
            flush_to_zero: self.fpscr & 1 << 24 != 0,
            default_nan: self.fpscr & 1 << 25 != 0,
            flags: Flags(0),
        }
    }

    fn accumulate(&mut self, env: Env) {
        self.fpscr |= env.flags.0 as u32 & FPSCR_CUMULATIVE;
    }
}

/// One arithmetic step in either precision, on raw bits.
#[derive(Clone, Copy)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

fn arith(double: bool, op: Op, a: u64, b: u64, env: &mut Env) -> u64 {
    if double {
        let (a, b) = (F64(a), F64(b));
        match op {
            Op::Add => a.add(b, env),
            Op::Sub => a.sub(b, env),
            Op::Mul => a.mul(b, env),
            Op::Div => a.div(b, env),
        }
        .0
    } else {
        let (a, b) = (F32(a as u32), F32(b as u32));
        match op {
            Op::Add => a.add(b, env),
            Op::Sub => a.sub(b, env),
            Op::Mul => a.mul(b, env),
            Op::Div => a.div(b, env),
        }
        .0 as u64
    }
}

fn negate(double: bool, value: u64) -> u64 {
    value ^ if double { 1 << 63 } else { 1 << 31 }
}

fn absolute(double: bool, value: u64) -> u64 {
    value & !if double { 1u64 << 63 } else { 1 << 31 }
}

/// Whether `instr` addresses the VFP.
pub(crate) fn owns(instr: u32) -> bool {
    matches!(instr >> 8 & 0xF, 10 | 11)
}

/// Run a coprocessor 10 or 11 instruction of any of the four classes.
pub(crate) fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B, instr: u32) -> Exec {
    if !bus.vfp_access(cpu.privileged()) {
        return Err(Trap::Undefined);
    }
    let double = instr >> 8 & 0xF == 11;
    let class = instr >> 24 & 0xF;
    let register_transfer = class == 0xE && instr & 1 << 4 != 0;

    // With the unit disabled only the system registers answer, to
    // privileged code.
    let enabled = cpu.vfp.fpexc & FPEXC_ENABLE != 0;
    if !enabled && !(register_transfer && instr >> 21 & 7 == 7 && cpu.privileged()) {
        return Err(Trap::Undefined);
    }

    if register_transfer {
        transfer(cpu, instr, double, enabled)
    } else if class == 0xE {
        data_processing(cpu, instr, double)
    } else if instr >> 21 & 0x7F == 0b110_0010 {
        transfer_two(cpu, instr, double)
    } else {
        load_store(cpu, bus, instr, double)
    }
}

/// The register number of a five-bit single field or a four-bit double one.
fn reg(double: bool, field: u32, extra: u32) -> usize {
    if double {
        field as usize
    } else {
        (field << 1 | extra) as usize
    }
}

fn data_processing(cpu: &mut Cpu, instr: u32, double: bool) -> Exec {
    let fd = reg(double, instr >> 12 & 0xF, instr >> 22 & 1);
    let fn_ = reg(double, instr >> 16 & 0xF, instr >> 7 & 1);
    let fm = reg(double, instr & 0xF, instr >> 5 & 1);
    let opcode = (instr >> 23 & 1) << 3 | (instr >> 20 & 3) << 1 | (instr >> 6 & 1);

    if opcode == 0b1111 {
        let extension = (instr >> 16 & 0xF) << 1 | (instr >> 7 & 1);
        if extension >= 0b01000 {
            return scalar_extension(cpu, instr, double, extension);
        }
    }

    // Short vectors: a destination outside the first bank makes the
    // operation run LEN times over registers that wrap within their bank.
    let bank = if double { 4 } else { 8 };
    let len = (cpu.vfp.fpscr >> 16 & 7) as usize + 1;
    let stride = if cpu.vfp.fpscr >> 20 & 3 == 3 { 2 } else { 1 };
    let vector = fd / bank != 0 && len > 1;
    let next = |r: usize| r / bank * bank + (r % bank + stride) % bank;
    let (mut fd, mut fn_, mut fm) = (fd, fn_, fm);
    let scalar_operand = fm / bank == 0;

    let mut env = cpu.vfp.env();
    for _ in 0..if vector { len } else { 1 } {
        let a = cpu.vfp.get(double, fn_);
        let b = cpu.vfp.get(double, fm);
        let d = cpu.vfp.get(double, fd);
        let result = match opcode {
            // The multiply-accumulates round the product, then the sum.
            0b0000 => arith(
                double,
                Op::Add,
                d,
                arith(double, Op::Mul, a, b, &mut env),
                &mut env,
            ),
            0b0001 => {
                let product = negate(double, arith(double, Op::Mul, a, b, &mut env));
                arith(double, Op::Add, d, product, &mut env)
            }
            0b0010 => {
                let product = arith(double, Op::Mul, a, b, &mut env);
                arith(double, Op::Add, negate(double, d), product, &mut env)
            }
            0b0011 => {
                let product = negate(double, arith(double, Op::Mul, a, b, &mut env));
                arith(double, Op::Add, negate(double, d), product, &mut env)
            }
            0b0100 => arith(double, Op::Mul, a, b, &mut env),
            0b0101 => negate(double, arith(double, Op::Mul, a, b, &mut env)),
            0b0110 => arith(double, Op::Add, a, b, &mut env),
            0b0111 => arith(double, Op::Sub, a, b, &mut env),
            0b1000 => arith(double, Op::Div, a, b, &mut env),
            0b1111 => match (instr >> 16 & 0xF) << 1 | (instr >> 7 & 1) {
                0b00000 => b,
                0b00001 => absolute(double, b),
                0b00010 => negate(double, b),
                0b00011 => {
                    if double {
                        F64(b).sqrt(&mut env).0
                    } else {
                        F32(b as u32).sqrt(&mut env).0 as u64
                    }
                }
                _ => return Err(Trap::Undefined),
            },
            _ => return Err(Trap::Undefined),
        };
        cpu.vfp.set(double, fd, result);
        fd = next(fd);
        fn_ = next(fn_);
        if !scalar_operand {
            fm = next(fm);
        }
    }
    cpu.vfp.accumulate(env);
    Ok(1)
}

/// Comparisons and conversions, which never operate on vectors.
fn scalar_extension(cpu: &mut Cpu, instr: u32, double: bool, extension: u32) -> Exec {
    let mut env = cpu.vfp.env();
    let single_d = (instr >> 12 & 0xF) << 1 | (instr >> 22 & 1);
    let single_m = (instr & 0xF) << 1 | (instr >> 5 & 1);
    let fd = reg(double, instr >> 12 & 0xF, instr >> 22 & 1);
    let fm = reg(double, instr & 0xF, instr >> 5 & 1);
    match extension {
        // FCMP, FCMPE, FCMPZ, FCMPEZ.
        0b01000..=0b01011 => {
            let a = cpu.vfp.get(double, fd);
            let b = if extension & 0b10 != 0 {
                0
            } else {
                cpu.vfp.get(double, fm)
            };
            let signaling = extension & 1 != 0;
            let order = match (double, signaling) {
                (true, false) => F64(a).compare(F64(b), &mut env),
                (true, true) => F64(a).compare_signaling(F64(b), &mut env),
                (false, false) => F32(a as u32).compare(F32(b as u32), &mut env),
                (false, true) => F32(a as u32).compare_signaling(F32(b as u32), &mut env),
            };
            let flags = match order {
                Some(Ordering::Equal) => psr::Z | psr::C,
                Some(Ordering::Less) => psr::N,
                Some(Ordering::Greater) => psr::C,
                None => psr::C | psr::V,
            };
            cpu.vfp.fpscr = cpu.vfp.fpscr & !FPSCR_FLAGS | flags;
        }
        // FCVT: the coprocessor number names the source precision.
        0b01111 => {
            if double {
                let value = F64(cpu.vfp.d(fm)).to_f32(&mut env);
                cpu.vfp.s[single_d as usize] = value.0;
            } else {
                let value = F32(cpu.vfp.s[fm]).to_f64(&mut env);
                cpu.vfp.set_d((instr >> 12 & 0xF) as usize, value.0);
            }
        }
        // FUITO and FSITO: the integer sits in a single register.
        0b10000 | 0b10001 => {
            let integer = cpu.vfp.s[single_m as usize];
            let signed = extension & 1 != 0;
            let value = match (double, signed) {
                (true, true) => F64::from_i32(integer as i32, &mut env).0,
                (true, false) => F64::from_u32(integer, &mut env).0,
                (false, true) => F32::from_i32(integer as i32, &mut env).0 as u64,
                (false, false) => F32::from_u32(integer, &mut env).0 as u64,
            };
            cpu.vfp.set(double, fd, value);
        }
        // FTOUI, FTOUIZ, FTOSI, FTOSIZ: the integer lands in a single register.
        0b11000..=0b11011 => {
            let signed = extension & 0b10 != 0;
            let round = if extension & 1 != 0 {
                Round::Zero
            } else {
                env.round
            };
            let source = cpu.vfp.get(double, fm);
            let integer = match (double, signed) {
                (true, true) => F64(source).to_i32(round, &mut env) as u32,
                (true, false) => F64(source).to_u32(round, &mut env),
                (false, true) => F32(source as u32).to_i32(round, &mut env) as u32,
                (false, false) => F32(source as u32).to_u32(round, &mut env),
            };
            cpu.vfp.s[single_d as usize] = integer;
        }
        _ => return Err(Trap::Undefined),
    }
    cpu.vfp.accumulate(env);
    Ok(1)
}

/// `MRC`/`MCR`: one ARM register to or from a VFP register or system register.
fn transfer(cpu: &mut Cpu, instr: u32, double: bool, enabled: bool) -> Exec {
    let rd = instr >> 12 & 0xF;
    let load = instr & 1 << 20 != 0;
    let field = instr >> 16 & 0xF;
    match (instr >> 21 & 7, double) {
        (0, false) => {
            let sn = (field << 1 | (instr >> 7 & 1)) as usize;
            if load {
                cpu.r[rd as usize] = cpu.vfp.s[sn];
            } else {
                cpu.vfp.s[sn] = cpu.get(rd);
            }
        }
        // FMDLR/FMRDL and FMDHR/FMRDH: a half of a double register.
        (half @ (0 | 1), true) => {
            let index = field as usize * 2 + half as usize;
            if load {
                cpu.r[rd as usize] = cpu.vfp.s[index];
            } else {
                cpu.vfp.s[index] = cpu.get(rd);
            }
        }
        (7, false) => {
            let privileged = cpu.privileged();
            match (field, load) {
                (0, true) => cpu.r[rd as usize] = FPSID,
                (0, false) => {}
                (1, true) if enabled => {
                    if rd == 15 {
                        // FMSTAT: the comparison flags go to the CPSR.
                        let cpsr = cpu.cpsr() & !FPSCR_FLAGS | cpu.vfp.fpscr & FPSCR_FLAGS;
                        cpu.set_cpsr(cpsr);
                    } else {
                        cpu.r[rd as usize] = cpu.vfp.fpscr;
                    }
                }
                (1, false) if enabled => cpu.vfp.fpscr = cpu.get(rd) & FPSCR_WRITABLE,
                (8, true) if privileged => cpu.r[rd as usize] = cpu.vfp.fpexc,
                (8, false) if privileged => cpu.vfp.fpexc = cpu.get(rd) & 0xC000_0000,
                _ => return Err(Trap::Undefined),
            }
        }
        _ => return Err(Trap::Undefined),
    }
    Ok(1)
}

/// `MRRC`/`MCRR`: two ARM registers to or from a double or two singles.
fn transfer_two(cpu: &mut Cpu, instr: u32, double: bool) -> Exec {
    let rd = (instr >> 12 & 0xF) as usize;
    let rn = (instr >> 16 & 0xF) as usize;
    let first = if double {
        (instr & 0xF) as usize * 2
    } else {
        ((instr & 0xF) << 1 | (instr >> 5 & 1)) as usize
    };
    if first + 1 > 31 {
        return Err(Trap::Undefined);
    }
    if instr & 1 << 20 != 0 {
        cpu.r[rd] = cpu.vfp.s[first];
        cpu.r[rn] = cpu.vfp.s[first + 1];
    } else {
        cpu.vfp.s[first] = cpu.get(rd as u32);
        cpu.vfp.s[first + 1] = cpu.get(rn as u32);
    }
    Ok(1)
}

/// FLDS, FSTS, FLDD, FSTD and the multiple forms.
fn load_store<B: Bus>(cpu: &mut Cpu, bus: &mut B, instr: u32, double: bool) -> Exec {
    let pre = instr & 1 << 24 != 0;
    let up = instr & 1 << 23 != 0;
    let writeback = instr & 1 << 21 != 0;
    let load = instr & 1 << 20 != 0;
    let rn = instr >> 16 & 0xF;
    let first = reg(double, instr >> 12 & 0xF, instr >> 22 & 1);
    let offset = (instr & 0xFF) << 2;
    let privileged = cpu.privileged();
    let base = if rn == 15 {
        cpu.pc_read & !3
    } else {
        cpu.get(rn)
    };

    // Words to move and where they start.
    let (start, words) = match (pre, up, writeback) {
        // A single register at an offset.
        (true, _, false) => {
            let addr = if up {
                base.wrapping_add(offset)
            } else {
                base.wrapping_sub(offset)
            };
            (addr, if double { 2 } else { 1 })
        }
        // Multiple, increment after or decrement before.
        (false, true, _) => (base, (instr & 0xFF) as usize),
        (true, false, true) => (base.wrapping_sub(offset), (instr & 0xFF) as usize),
        _ => return Err(Trap::Undefined),
    };
    // Doubles move as two words, low first; an odd count is the FLDMX form
    // with a trailing format word.
    let registers = if double { words / 2 } else { words };
    let last = first + registers;
    if registers == 0 || last > if double { 16 } else { 32 } {
        return Err(Trap::Undefined);
    }

    let mut addr = start & !3;
    for n in first..last {
        let parts = if double { 2 } else { 1 };
        for part in 0..parts {
            let index = if double { n * 2 + part } else { n };
            if load {
                cpu.vfp.s[index] = bus.read32(addr, privileged)?;
            } else {
                bus.write32(addr, cpu.vfp.s[index], privileged)?;
            }
            addr = addr.wrapping_add(4);
        }
    }
    if writeback {
        cpu.r[rn as usize] = if up {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
    }
    Ok(registers as u32 + 1)
}
