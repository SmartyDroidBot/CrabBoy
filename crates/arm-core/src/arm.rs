//! The 32-bit ARM instruction set (ARMv5TE).
//!
//! Instructions are classified once, into a table indexed by bits 27-20 and
//! 7-4, which is every bit that distinguishes one encoding from another.

use crate::alu::{add_with_carry, saturate, shift_imm, shift_reg, Shift};
use crate::bus::{Bus, CpEffect, CpReg};
use crate::cpu::{psr, Cpu, Exec, Trap};
use crate::v6;
use crate::vfp;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    DataProcessing,
    Multiply,
    MultiplyLong,
    Swap,
    Exclusive,
    Umaal,
    Media,
    LoadStoreMisc,
    Mrs,
    MsrReg,
    MsrImm,
    Bx,
    BlxReg,
    Clz,
    Saturating,
    DspMultiply,
    Breakpoint,
    LoadStore,
    LoadStoreMultiple,
    Branch,
    CoprocLoadStore,
    CoprocDoubleReg,
    CoprocData,
    CoprocReg,
    Supervisor,
    Undefined,
}

const fn classify(hi: u32, lo: u32) -> Kind {
    match hi >> 5 {
        0b000 => {
            if lo == 0b1001 {
                if hi & 0xFC == 0x00 {
                    Kind::Multiply
                } else if hi & 0xF8 == 0x08 {
                    Kind::MultiplyLong
                } else if hi & 0xFB == 0x10 {
                    Kind::Swap
                } else if hi == 0x04 {
                    Kind::Umaal
                } else if hi & 0xF8 == 0x18 {
                    Kind::Exclusive
                } else {
                    Kind::Undefined
                }
            } else if lo & 0b1001 == 0b1001 {
                Kind::LoadStoreMisc
            } else if hi & 0xF9 == 0x10 {
                // Opcodes TST..CMN without S: the miscellaneous space.
                match lo {
                    0b0000 if hi & 0xFB == 0x10 => Kind::Mrs,
                    0b0000 => Kind::MsrReg,
                    0b0001 | 0b0010 if hi == 0x12 => Kind::Bx,
                    0b0001 if hi == 0x16 => Kind::Clz,
                    0b0011 if hi == 0x12 => Kind::BlxReg,
                    0b0101 => Kind::Saturating,
                    0b0111 if hi == 0x12 => Kind::Breakpoint,
                    0b1000 | 0b1010 | 0b1100 | 0b1110 => Kind::DspMultiply,
                    _ => Kind::Undefined,
                }
            } else {
                Kind::DataProcessing
            }
        }
        0b001 => {
            if hi & 0xFB == 0x32 {
                Kind::MsrImm
            } else if hi & 0xFB == 0x30 {
                Kind::Undefined
            } else {
                Kind::DataProcessing
            }
        }
        0b010 => Kind::LoadStore,
        0b011 => {
            if lo & 1 != 0 {
                Kind::Media
            } else {
                Kind::LoadStore
            }
        }
        0b100 => Kind::LoadStoreMultiple,
        0b101 => Kind::Branch,
        0b110 => {
            if hi & 0xFE == 0xC4 {
                Kind::CoprocDoubleReg
            } else {
                Kind::CoprocLoadStore
            }
        }
        _ => {
            if hi & 0x10 != 0 {
                Kind::Supervisor
            } else if lo & 1 != 0 {
                Kind::CoprocReg
            } else {
                Kind::CoprocData
            }
        }
    }
}

const fn build_table() -> [Kind; 4096] {
    let mut table = [Kind::Undefined; 4096];
    let mut i = 0;
    while i < 4096 {
        table[i] = classify((i >> 4) as u32, (i & 0xF) as u32);
        i += 1;
    }
    table
}

static TABLE: [Kind; 4096] = build_table();

#[inline]
pub(crate) fn kind_of(instr: u32) -> Kind {
    TABLE[(instr >> 16 & 0xFF0 | instr >> 4 & 0xF) as usize]
}

pub(crate) fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B, instr: u32) -> Exec {
    let cond = instr >> 28;
    if cond == 0xF {
        if cpu.v6() {
            if let Some(result) = v6::unconditional(cpu, bus, instr) {
                return result;
            }
        }
        return unconditional(cpu, instr);
    }
    if cond != 0xE && !cpu.condition(cond) {
        return Ok(1);
    }
    let kind = kind_of(instr);
    let coprocessor = matches!(
        kind,
        Kind::CoprocReg | Kind::CoprocData | Kind::CoprocLoadStore | Kind::CoprocDoubleReg
    );
    if coprocessor && cpu.v6() && vfp::owns(instr) {
        return vfp::execute(cpu, bus, instr);
    }
    match kind {
        Kind::DataProcessing => data_processing(cpu, instr),
        Kind::Multiply => multiply(cpu, instr),
        Kind::MultiplyLong => multiply_long(cpu, instr),
        Kind::Swap => swap(cpu, bus, instr),
        Kind::Exclusive if cpu.v6() => v6::exclusive(cpu, bus, instr),
        Kind::Umaal if cpu.v6() => v6::umaal(cpu, instr),
        Kind::Media if cpu.v6() => v6::media(cpu, bus, instr),
        Kind::Exclusive | Kind::Umaal | Kind::Media => Err(Trap::Undefined),
        Kind::LoadStoreMisc => load_store_misc(cpu, bus, instr),
        Kind::Mrs => mrs(cpu, instr),
        Kind::MsrReg => {
            let value = cpu.get(instr & 0xF);
            msr(cpu, instr, value)
        }
        // ARMv6K hints share the encoding of an MSR that writes no field.
        // WFE, SEV and YIELD only matter for power and do nothing here.
        Kind::MsrImm if cpu.v6() && instr & 0x000F_0000 == 0 => {
            if instr & 0xFF == 3 {
                cpu.halt();
            }
            Ok(1)
        }
        Kind::MsrImm => {
            let value = (instr & 0xFF).rotate_right((instr >> 8 & 0xF) * 2);
            msr(cpu, instr, value)
        }
        Kind::Bx => {
            cpu.branch_exchange(cpu.get(instr & 0xF));
            Ok(3)
        }
        Kind::BlxReg => {
            let target = cpu.get(instr & 0xF);
            cpu.r[14] = cpu.r[15];
            cpu.branch_exchange(target);
            Ok(3)
        }
        Kind::Clz => {
            let rd = (instr >> 12 & 0xF) as usize;
            cpu.r[rd] = cpu.get(instr & 0xF).leading_zeros();
            Ok(1)
        }
        Kind::Saturating => saturating(cpu, instr),
        Kind::DspMultiply => dsp_multiply(cpu, instr),
        Kind::Breakpoint => Err(Trap::Breakpoint),
        Kind::LoadStore => load_store(cpu, bus, instr),
        Kind::LoadStoreMultiple => load_store_multiple(cpu, bus, instr),
        Kind::Branch => {
            if instr & 1 << 24 != 0 {
                cpu.r[14] = cpu.r[15];
            }
            let offset = ((instr << 8) as i32 >> 6) as u32;
            cpu.branch(cpu.pc_read.wrapping_add(offset));
            Ok(3)
        }
        Kind::CoprocReg => coproc_reg(cpu, bus, instr),
        Kind::CoprocLoadStore | Kind::CoprocDoubleReg | Kind::CoprocData => Err(Trap::Undefined),
        Kind::Supervisor => Err(Trap::Supervisor),
        Kind::Undefined => Err(Trap::Undefined),
    }
}

/// The `cond` = 1111 space.
fn unconditional(cpu: &mut Cpu, instr: u32) -> Exec {
    if instr >> 25 & 7 == 0b101 {
        // BLX <label>: the H bit supplies bit 1 of the Thumb target.
        let offset = ((instr << 8) as i32 >> 6) as u32 | (instr >> 23 & 2);
        cpu.r[14] = cpu.r[15];
        cpu.branch_exchange(cpu.pc_read.wrapping_add(offset) | 1);
        return Ok(3);
    }
    if instr & 0x0D70_F000 == 0x0550_F000 {
        // PLD: a hint.
        return Ok(1);
    }
    Err(Trap::Undefined)
}

/// The shifter operand of a data-processing instruction and its carry out.
fn operand2(cpu: &Cpu, instr: u32) -> (u32, bool) {
    let carry = cpu.flag(psr::C);
    if instr & 1 << 25 != 0 {
        let rotate = (instr >> 8 & 0xF) * 2;
        let value = (instr & 0xFF).rotate_right(rotate);
        let carry = if rotate == 0 { carry } else { value >> 31 != 0 };
        (value, carry)
    } else {
        let value = cpu.get(instr & 0xF);
        let kind = Shift::from_bits(instr >> 5);
        if instr & 1 << 4 != 0 {
            // A register-specified shift takes a cycle before the operand is
            // read, so r15 has moved on to the instruction's address plus 12.
            let value = if instr & 0xF == 15 {
                value.wrapping_add(4)
            } else {
                value
            };
            shift_reg(kind, value, cpu.get(instr >> 8 & 0xF), carry)
        } else {
            shift_imm(kind, value, instr >> 7 & 0x1F, carry)
        }
    }
}

fn data_processing(cpu: &mut Cpu, instr: u32) -> Exec {
    let opcode = instr >> 21 & 0xF;
    let set_flags = instr & 1 << 20 != 0;
    let rd = instr >> 12 & 0xF;
    let register_shift = instr >> 25 & 1 == 0 && instr & 1 << 4 != 0;
    let mut a = cpu.get(instr >> 16 & 0xF);
    if register_shift && instr >> 16 & 0xF == 15 {
        // As for the shifted register: r15 is a cycle further on.
        a = a.wrapping_add(4);
    }
    let (b, shifter_carry) = operand2(cpu, instr);
    let carry_in = cpu.flag(psr::C);

    // Logical operations take C from the shifter; arithmetic ones compute
    // C and V.
    let (result, arithmetic) = match opcode {
        0x0 | 0x8 => (a & b, None),
        0x1 | 0x9 => (a ^ b, None),
        0x2 | 0xA => {
            let (r, c, v) = add_with_carry(a, !b, true);
            (r, Some((c, v)))
        }
        0x3 => {
            let (r, c, v) = add_with_carry(b, !a, true);
            (r, Some((c, v)))
        }
        0x4 | 0xB => {
            let (r, c, v) = add_with_carry(a, b, false);
            (r, Some((c, v)))
        }
        0x5 => {
            let (r, c, v) = add_with_carry(a, b, carry_in);
            (r, Some((c, v)))
        }
        0x6 => {
            let (r, c, v) = add_with_carry(a, !b, carry_in);
            (r, Some((c, v)))
        }
        0x7 => {
            let (r, c, v) = add_with_carry(b, !a, carry_in);
            (r, Some((c, v)))
        }
        0xC => (a | b, None),
        0xD => (b, None),
        0xE => (a & !b, None),
        _ => (!b, None),
    };

    let writes = !(0x8..=0xB).contains(&opcode);
    let mut cycles = 1 + (instr >> 25 & 1 == 0 && instr & 1 << 4 != 0) as u32;

    if set_flags && rd == 15 && writes {
        cpu.exception_return(result);
        return Ok(cycles + 2);
    }
    if set_flags {
        cpu.set_nz(result);
        match arithmetic {
            Some((c, v)) => {
                cpu.set_flag(psr::C, c);
                cpu.set_flag(psr::V, v);
            }
            None => cpu.set_flag(psr::C, shifter_carry),
        }
    }
    if writes {
        if rd == 15 {
            cpu.branch(result);
            cycles += 2;
        } else {
            cpu.r[rd as usize] = result;
        }
    }
    Ok(cycles)
}

fn multiply(cpu: &mut Cpu, instr: u32) -> Exec {
    let rd = (instr >> 16 & 0xF) as usize;
    let mut result = cpu.get(instr & 0xF).wrapping_mul(cpu.get(instr >> 8 & 0xF));
    if instr & 1 << 21 != 0 {
        result = result.wrapping_add(cpu.get(instr >> 12 & 0xF));
    }
    cpu.r[rd] = result;
    if instr & 1 << 20 != 0 {
        cpu.set_nz(result);
    }
    Ok(3)
}

fn multiply_long(cpu: &mut Cpu, instr: u32) -> Exec {
    let rd_hi = (instr >> 16 & 0xF) as usize;
    let rd_lo = (instr >> 12 & 0xF) as usize;
    let a = cpu.get(instr & 0xF);
    let b = cpu.get(instr >> 8 & 0xF);
    let mut result = if instr & 1 << 22 != 0 {
        (a as i32 as i64).wrapping_mul(b as i32 as i64) as u64
    } else {
        a as u64 * b as u64
    };
    if instr & 1 << 21 != 0 {
        result = result.wrapping_add((cpu.r[rd_hi] as u64) << 32 | cpu.r[rd_lo] as u64);
    }
    cpu.r[rd_lo] = result as u32;
    cpu.r[rd_hi] = (result >> 32) as u32;
    if instr & 1 << 20 != 0 {
        cpu.set_nz64(result);
    }
    Ok(4)
}

/// QADD, QSUB, QDADD, QDSUB.
fn saturating(cpu: &mut Cpu, instr: u32) -> Exec {
    let rd = (instr >> 12 & 0xF) as usize;
    let rm = cpu.get(instr & 0xF) as i32 as i64;
    let mut rn = cpu.get(instr >> 16 & 0xF) as i32 as i64;
    let mut q = false;
    if instr & 1 << 22 != 0 {
        let (doubled, sat) = saturate(rn * 2);
        rn = doubled as i32 as i64;
        q |= sat;
    }
    let (result, sat) = saturate(if instr & 1 << 21 != 0 {
        rm - rn
    } else {
        rm + rn
    });
    cpu.r[rd] = result;
    if q || sat {
        cpu.set_flag(psr::Q, true);
    }
    Ok(1)
}

/// SMLAxy, SMLAWy, SMULWy, SMLALxy, SMULxy.
fn dsp_multiply(cpu: &mut Cpu, instr: u32) -> Exec {
    let half = |value: u32, high: bool| -> i64 {
        let bits = if high { value >> 16 } else { value };
        bits as i16 as i64
    };
    let rd = (instr >> 16 & 0xF) as usize;
    let rn = (instr >> 12 & 0xF) as usize;
    let rs = cpu.get(instr >> 8 & 0xF);
    let rm = cpu.get(instr & 0xF);
    let x = instr & 1 << 5 != 0;
    let y = instr & 1 << 6 != 0;

    match instr >> 21 & 3 {
        0b00 => {
            let product = (half(rm, x) * half(rs, y)) as u32;
            let (result, _, overflow) = add_with_carry(product, cpu.r[rn], false);
            cpu.r[rd] = result;
            if overflow {
                cpu.set_flag(psr::Q, true);
            }
        }
        0b01 => {
            let product = ((rm as i32 as i64 * half(rs, y)) >> 16) as u32;
            if x {
                cpu.r[rd] = product;
            } else {
                let (result, _, overflow) = add_with_carry(product, cpu.r[rn], false);
                cpu.r[rd] = result;
                if overflow {
                    cpu.set_flag(psr::Q, true);
                }
            }
        }
        0b10 => {
            let product = half(rm, x) * half(rs, y);
            let acc = ((cpu.r[rd] as u64) << 32 | cpu.r[rn] as u64) as i64;
            let result = acc.wrapping_add(product) as u64;
            cpu.r[rn] = result as u32;
            cpu.r[rd] = (result >> 32) as u32;
        }
        _ => cpu.r[rd] = (half(rm, x) * half(rs, y)) as u32,
    }
    Ok(2)
}

fn mrs(cpu: &mut Cpu, instr: u32) -> Exec {
    let rd = (instr >> 12 & 0xF) as usize;
    cpu.r[rd] = if instr & 1 << 22 != 0 {
        cpu.spsr()
    } else {
        cpu.cpsr()
    };
    Ok(2)
}

fn msr(cpu: &mut Cpu, instr: u32, value: u32) -> Exec {
    // ARMv6 adds the GE flags and E for everyone and A for privileged
    // modes.
    let (user, privileged_bits) = if cpu.v6() {
        (0xF80F_0200, 0x0000_01DF)
    } else {
        (0xF800_0000, 0x0000_00DF)
    };
    const STATE: u32 = 0x0100_0020;

    let mut byte_mask = 0;
    for field in 0..4 {
        if instr & 1 << (16 + field) != 0 {
            byte_mask |= 0xFF << (field * 8);
        }
    }
    if instr & 1 << 22 != 0 {
        if cpu.has_spsr() {
            let mask = byte_mask & (user | privileged_bits | STATE);
            let spsr = cpu.spsr();
            cpu.set_spsr(spsr & !mask | value & mask);
        }
    } else {
        let mut mask = byte_mask & user;
        if cpu.privileged() {
            mask |= byte_mask & privileged_bits;
        }
        let cpsr = cpu.cpsr();
        cpu.set_cpsr(cpsr & !mask | value & mask);
    }
    Ok(1)
}

fn swap<B: Bus>(cpu: &mut Cpu, bus: &mut B, instr: u32) -> Exec {
    let addr = cpu.get(instr >> 16 & 0xF);
    let rd = (instr >> 12 & 0xF) as usize;
    let source = cpu.get(instr & 0xF);
    let privileged = cpu.privileged();
    let old = if instr & 1 << 22 != 0 {
        let old = bus.read8(addr, privileged)? as u32;
        bus.write8(addr, source as u8, privileged)?;
        old
    } else {
        let old = bus.read32(addr & !3, privileged)?;
        bus.write32(addr & !3, source, privileged)?;
        old
    };
    cpu.r[rd] = old;
    Ok(4)
}

/// Pre- or post-indexed addressing: the address to access and the value to
/// write back to the base, if any.
fn index(instr: u32, base: u32, offset: u32) -> (u32, Option<u32>) {
    let offset_addr = if instr & 1 << 23 != 0 {
        base.wrapping_add(offset)
    } else {
        base.wrapping_sub(offset)
    };
    if instr & 1 << 24 == 0 {
        (base, Some(offset_addr))
    } else if instr & 1 << 21 != 0 {
        (offset_addr, Some(offset_addr))
    } else {
        (offset_addr, None)
    }
}

/// LDR, STR, LDRB, STRB and their user-mode `T` forms.
fn load_store<B: Bus>(cpu: &mut Cpu, bus: &mut B, instr: u32) -> Exec {
    let rn = instr >> 16 & 0xF;
    let rd = instr >> 12 & 0xF;
    let load = instr & 1 << 20 != 0;
    let byte = instr & 1 << 22 != 0;
    let offset = if instr & 1 << 25 != 0 {
        let kind = Shift::from_bits(instr >> 5);
        shift_imm(
            kind,
            cpu.get(instr & 0xF),
            instr >> 7 & 0x1F,
            cpu.flag(psr::C),
        )
        .0
    } else {
        instr & 0xFFF
    };
    let (addr, writeback) = index(instr, cpu.get(rn), offset);
    // Post-indexed with W set is the `T` form: a user-mode access.
    let translate = instr & 1 << 24 == 0 && instr & 1 << 21 != 0;
    let privileged = cpu.privileged() && !translate;

    let word = if bus.unaligned_access() {
        addr
    } else {
        addr & !3
    };
    if load {
        let value = if byte {
            bus.read8(addr, privileged)? as u32
        } else {
            bus.read32(word, privileged)?
        };
        if let Some(base) = writeback {
            cpu.r[rn as usize] = base;
        }
        if rd == 15 {
            cpu.branch_exchange(value);
            return Ok(5);
        }
        cpu.r[rd as usize] = value;
        Ok(2)
    } else {
        // A stored r15 is the instruction's address plus 12.
        let value = if rd == 15 {
            cpu.pc_read.wrapping_add(4)
        } else {
            cpu.r[rd as usize]
        };
        if byte {
            bus.write8(addr, value as u8, privileged)?;
        } else {
            bus.write32(word, value, privileged)?;
        }
        if let Some(base) = writeback {
            cpu.r[rn as usize] = base;
        }
        Ok(2)
    }
}

/// LDRH, STRH, LDRSB, LDRSH, LDRD, STRD.
fn load_store_misc<B: Bus>(cpu: &mut Cpu, bus: &mut B, instr: u32) -> Exec {
    let rn = instr >> 16 & 0xF;
    let rd = instr >> 12 & 0xF;
    let load = instr & 1 << 20 != 0;
    let offset = if instr & 1 << 22 != 0 {
        instr >> 4 & 0xF0 | instr & 0xF
    } else {
        cpu.get(instr & 0xF)
    };
    let (addr, writeback) = index(instr, cpu.get(rn), offset);
    let privileged = cpu.privileged();
    let half = if bus.unaligned_access() {
        addr
    } else {
        addr & !1
    };

    match (load, instr >> 5 & 3) {
        (false, 0b01) => bus.write16(half, cpu.get(rd) as u16, privileged)?,
        (true, 0b01) => {
            let value = bus.read16(half, privileged)? as u32;
            finish_load(cpu, rn, rd, writeback, value);
            return Ok(2);
        }
        (true, 0b10) => {
            let value = bus.read8(addr, privileged)? as i8 as u32;
            finish_load(cpu, rn, rd, writeback, value);
            return Ok(2);
        }
        (true, _) => {
            let value = bus.read16(half, privileged)? as i16 as u32;
            finish_load(cpu, rn, rd, writeback, value);
            return Ok(2);
        }
        (false, 0b10) => {
            // LDRD: two words into Rd and Rd+1.
            let first = bus.read32(addr & !3, privileged)?;
            let second = bus.read32((addr & !3).wrapping_add(4), privileged)?;
            if let Some(base) = writeback {
                cpu.r[rn as usize] = base;
            }
            cpu.r[rd as usize] = first;
            if rd == 14 {
                cpu.branch_exchange(second);
            } else {
                cpu.r[(rd + 1) as usize & 0xF] = second;
            }
            return Ok(3);
        }
        (false, _) => {
            // STRD.
            bus.write32(addr & !3, cpu.get(rd), privileged)?;
            bus.write32(
                (addr & !3).wrapping_add(4),
                cpu.get((rd + 1) & 0xF),
                privileged,
            )?;
        }
    }
    if let Some(base) = writeback {
        cpu.r[rn as usize] = base;
    }
    Ok(2)
}

/// Writeback first, so a loaded base register keeps the loaded value.
fn finish_load(cpu: &mut Cpu, rn: u32, rd: u32, writeback: Option<u32>, value: u32) {
    if let Some(base) = writeback {
        cpu.r[rn as usize] = base;
    }
    if rd == 15 {
        cpu.branch(value);
    } else {
        cpu.r[rd as usize] = value;
    }
}

fn load_store_multiple<B: Bus>(cpu: &mut Cpu, bus: &mut B, instr: u32) -> Exec {
    let rn = instr >> 16 & 0xF;
    let list = instr & 0xFFFF;
    let load = instr & 1 << 20 != 0;
    let writeback = instr & 1 << 21 != 0;
    let s_bit = instr & 1 << 22 != 0;
    let up = instr & 1 << 23 != 0;
    let pre = instr & 1 << 24 != 0;
    let privileged = cpu.privileged();

    let base = cpu.get(rn);
    // An empty list transfers nothing on the ARM9 but still moves the base
    // by sixteen words.
    let bytes = if list == 0 {
        0x40
    } else {
        list.count_ones() * 4
    };
    let (lowest, new_base) = if up {
        (base, base.wrapping_add(bytes))
    } else {
        (base.wrapping_sub(bytes), base.wrapping_sub(bytes))
    };
    let mut addr = (if pre == up {
        lowest.wrapping_add(4)
    } else {
        lowest
    }) & !3;

    // With S and no r15 in a load list, the user-mode registers transfer.
    let user_bank = s_bit && !(load && list & 0x8000 != 0);

    if load {
        let mut loaded_pc = None;
        for reg in 0..16 {
            if list & 1 << reg == 0 {
                continue;
            }
            let value = bus.read32(addr, privileged)?;
            addr = addr.wrapping_add(4);
            if reg == 15 {
                loaded_pc = Some(value);
            } else if user_bank {
                cpu.set_user_reg(reg, value);
            } else {
                cpu.r[reg as usize] = value;
            }
        }
        // ARMv5 writes the base back unless it is in the list as the last of
        // several registers; the written-back value then replaces the loaded
        // one (GBATEK, "ARM Opcodes: Memory: Block Data Transfer").
        let base_in_list = list & 1 << rn != 0;
        let base_is_last = list >> rn == 1;
        if writeback && (!base_in_list || list.count_ones() == 1 || !base_is_last) {
            cpu.r[rn as usize] = new_base;
        }
        if let Some(target) = loaded_pc {
            if s_bit {
                cpu.exception_return(target);
            } else {
                cpu.branch_exchange(target);
            }
            return Ok(list.count_ones() + 4);
        }
    } else {
        for reg in 0..16 {
            if list & 1 << reg == 0 {
                continue;
            }
            // ARMv5 always stores the original base.
            let value = if reg == 15 {
                cpu.pc_read.wrapping_add(4)
            } else if user_bank {
                cpu.user_reg(reg)
            } else {
                cpu.r[reg as usize]
            };
            bus.write32(addr, value, privileged)?;
            addr = addr.wrapping_add(4);
        }
        if writeback {
            cpu.r[rn as usize] = new_base;
        }
    }
    Ok(list.count_ones().max(1) + 1)
}

/// MRC and MCR.
fn coproc_reg<B: Bus>(cpu: &mut Cpu, bus: &mut B, instr: u32) -> Exec {
    let reg = CpReg {
        cp: (instr >> 8 & 0xF) as u8,
        opc1: (instr >> 21 & 7) as u8,
        crn: (instr >> 16 & 0xF) as u8,
        crm: (instr & 0xF) as u8,
        opc2: (instr >> 5 & 7) as u8,
    };
    let rd = instr >> 12 & 0xF;
    let privileged = cpu.privileged();
    if instr & 1 << 20 != 0 {
        let value = bus.coproc_read(reg, privileged).ok_or(Trap::Undefined)?;
        if rd == 15 {
            let flags = psr::N | psr::Z | psr::C | psr::V;
            let cpsr = cpu.cpsr() & !flags | value & flags;
            cpu.set_cpsr(cpsr);
        } else {
            cpu.r[rd as usize] = value;
        }
    } else {
        let value = if rd == 15 {
            cpu.pc_read.wrapping_add(4)
        } else {
            cpu.r[rd as usize]
        };
        match bus
            .coproc_write(reg, value, privileged)
            .ok_or(Trap::Undefined)?
        {
            CpEffect::None => {}
            CpEffect::WaitForInterrupt => cpu.halt(),
        }
    }
    Ok(2)
}
