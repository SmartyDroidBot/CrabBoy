//! The instructions ARMv6 and ARMv6K add to the ARM instruction set.

use crate::bus::Bus;
use crate::cpu::{mode, psr, Cpu, Exec, Trap};

/// LDREX, STREX and their byte, halfword and doubleword forms.
pub(crate) fn exclusive<B: Bus>(cpu: &mut Cpu, bus: &mut B, instr: u32) -> Exec {
    let addr = cpu.get(instr >> 16 & 0xF);
    let rd = (instr >> 12 & 0xF) as usize;
    let privileged = cpu.privileged();
    let size = instr >> 21 & 3;
    if instr & 1 << 20 != 0 {
        let value = match size {
            0 => bus.read32(addr & !3, privileged)?,
            1 => {
                let low = bus.read32(addr & !3, privileged)?;
                let high = bus.read32((addr & !3).wrapping_add(4), privileged)?;
                cpu.r[(rd + 1) & 0xF] = high;
                low
            }
            2 => bus.read8(addr, privileged)? as u32,
            _ => bus.read16(addr & !1, privileged)? as u32,
        };
        cpu.r[rd] = value;
        bus.exclusive_load(addr);
    } else {
        let rm = instr & 0xF;
        let value = cpu.get(rm);
        if bus.exclusive_store(addr) {
            match size {
                0 => bus.write32(addr & !3, value, privileged)?,
                1 => {
                    bus.write32(addr & !3, value, privileged)?;
                    let high = cpu.get((rm + 1) & 0xF);
                    bus.write32((addr & !3).wrapping_add(4), high, privileged)?;
                }
                2 => bus.write8(addr, value as u8, privileged)?,
                _ => bus.write16(addr & !1, value as u16, privileged)?,
            }
            cpu.r[rd] = 0;
        } else {
            cpu.r[rd] = 1;
        }
    }
    Ok(2)
}

pub(crate) fn umaal(cpu: &mut Cpu, instr: u32) -> Exec {
    let hi = (instr >> 16 & 0xF) as usize;
    let lo = (instr >> 12 & 0xF) as usize;
    let product = cpu.get(instr & 0xF) as u64 * cpu.get(instr >> 8 & 0xF) as u64;
    let result = product + cpu.r[hi] as u64 + cpu.r[lo] as u64;
    cpu.r[lo] = result as u32;
    cpu.r[hi] = (result >> 32) as u32;
    Ok(4)
}

/// The media space: bits 27:25 = 011 with bit 4 set.
pub(crate) fn media<B: Bus>(cpu: &mut Cpu, _bus: &mut B, instr: u32) -> Exec {
    let hi = instr >> 20 & 0xFF;
    let op = instr >> 5 & 7;
    match hi {
        0x61..=0x63 | 0x65..=0x67 => parallel(cpu, instr),
        0x68 if op == 5 => select(cpu, instr),
        0x68 | 0x6A | 0x6B | 0x6C | 0x6E | 0x6F if op == 3 => extend(cpu, instr),
        0x6A | 0x6B | 0x6E | 0x6F if op & 1 == 0 => saturate(cpu, instr),
        0x6A | 0x6E if op == 1 => saturate16(cpu, instr),
        0x6B if op == 1 => reverse(cpu, instr, |v| v.swap_bytes()),
        0x6B if op == 5 => reverse(cpu, instr, |v| {
            (v >> 8 & 0x00FF_00FF) | (v << 8 & 0xFF00_FF00)
        }),
        0x6F if op == 5 => reverse(cpu, instr, |v| (v as u16).swap_bytes() as i16 as u32),
        0x70 | 0x74 if op <= 3 => dual_multiply(cpu, instr),
        0x75 if matches!(op, 0 | 1 | 6 | 7) => most_significant_multiply(cpu, instr),
        0x78 if op == 0 => sum_of_differences(cpu, instr),
        _ => Err(Trap::Undefined),
    }
}

fn reverse(cpu: &mut Cpu, instr: u32, f: impl Fn(u32) -> u32) -> Exec {
    let rd = (instr >> 12 & 0xF) as usize;
    cpu.r[rd] = f(cpu.get(instr & 0xF));
    Ok(1)
}

/// SXTB, SXTH, UXTB, UXTH, their 16-bit pair forms and the adding forms.
fn extend(cpu: &mut Cpu, instr: u32) -> Exec {
    let rd = (instr >> 12 & 0xF) as usize;
    let rn = instr >> 16 & 0xF;
    let rotated = cpu.get(instr & 0xF).rotate_right((instr >> 10 & 3) * 8);
    let unsigned = instr & 1 << 22 != 0;
    let base = if rn == 15 { 0 } else { cpu.get(rn) };
    cpu.r[rd] = match instr >> 20 & 3 {
        // The 16-bit pair forms extend both halves' low bytes.
        0 => {
            let ext = |byte: u32| {
                if unsigned {
                    byte & 0xFF
                } else {
                    byte as u8 as i8 as u32 & 0xFFFF
                }
            };
            let low = (base & 0xFFFF).wrapping_add(ext(rotated)) & 0xFFFF;
            let high = (base >> 16).wrapping_add(ext(rotated >> 16)) & 0xFFFF;
            high << 16 | low
        }
        2 => base.wrapping_add(if unsigned {
            rotated & 0xFF
        } else {
            rotated as u8 as i8 as u32
        }),
        3 => base.wrapping_add(if unsigned {
            rotated & 0xFFFF
        } else {
            rotated as u16 as i16 as u32
        }),
        _ => return Err(Trap::Undefined),
    };
    Ok(1)
}

/// Clamp `value` to `bits` signed bits, or to `bits` unsigned bits.
fn clamp(value: i64, bits: u32, unsigned: bool) -> (i64, bool) {
    let (low, high) = if unsigned {
        (0, (1i64 << bits) - 1)
    } else {
        (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1)
    };
    (value.clamp(low, high), value < low || value > high)
}

/// SSAT and USAT.
fn saturate(cpu: &mut Cpu, instr: u32) -> Exec {
    let rd = (instr >> 12 & 0xF) as usize;
    let unsigned = instr & 1 << 22 != 0;
    let bits = (instr >> 16 & 0x1F) + !unsigned as u32;
    let rm = cpu.get(instr & 0xF);
    let amount = instr >> 7 & 0x1F;
    let operand = if instr & 1 << 6 != 0 {
        // ASR, where zero encodes 32.
        (rm as i32 >> if amount == 0 { 31 } else { amount }) as i64
    } else {
        (rm << amount) as i32 as i64
    };
    let (result, saturated) = clamp(operand, bits, unsigned);
    cpu.r[rd] = result as u32;
    if saturated {
        cpu.set_flag(psr::Q, true);
    }
    Ok(1)
}

/// SSAT16 and USAT16.
fn saturate16(cpu: &mut Cpu, instr: u32) -> Exec {
    let rd = (instr >> 12 & 0xF) as usize;
    let unsigned = instr & 1 << 22 != 0;
    let bits = (instr >> 16 & 0xF) + !unsigned as u32;
    let rm = cpu.get(instr & 0xF);
    let (low, sat_low) = clamp(rm as i16 as i64, bits, unsigned);
    let (high, sat_high) = clamp((rm >> 16) as i16 as i64, bits, unsigned);
    cpu.r[rd] = (high as u32 & 0xFFFF) << 16 | low as u32 & 0xFFFF;
    if sat_low || sat_high {
        cpu.set_flag(psr::Q, true);
    }
    Ok(1)
}

fn select(cpu: &mut Cpu, instr: u32) -> Exec {
    let rd = (instr >> 12 & 0xF) as usize;
    let rn = cpu.get(instr >> 16 & 0xF);
    let rm = cpu.get(instr & 0xF);
    let mut mask = 0;
    for lane in 0..4 {
        if cpu.cpsr() & psr::GE0 << lane != 0 {
            mask |= 0xFF << (lane * 8);
        }
    }
    cpu.r[rd] = rn & mask | rm & !mask;
    Ok(1)
}

/// The parallel additions and subtractions: a prefix (signed, saturating,
/// halving, and their unsigned forms) times an operation on bytes or halves.
fn parallel(cpu: &mut Cpu, instr: u32) -> Exec {
    let rd = (instr >> 12 & 0xF) as usize;
    let rn = cpu.get(instr >> 16 & 0xF);
    let rm = cpu.get(instr & 0xF);
    let prefix = instr >> 20 & 7;
    let unsigned = prefix & 4 != 0;
    let op = instr >> 5 & 7;
    let (lanes, width): (u32, u32) = if op >= 4 { (4, 8) } else { (2, 16) };
    let lane_mask = (1u32 << width) - 1;
    let field = |value: u32, lane: u32| -> i64 {
        let bits = value >> (lane * width) & lane_mask;
        if unsigned {
            bits as i64
        } else if width == 8 {
            bits as u8 as i8 as i64
        } else {
            bits as u16 as i16 as i64
        }
    };

    let mut result = 0u32;
    let mut ge = 0u32;
    for lane in 0..lanes {
        // The exchanging forms cross the halves of Rm and mix the operations.
        let (b, subtract) = match op {
            0 | 4 => (field(rm, lane), false),
            3 | 7 => (field(rm, lane), true),
            1 => (field(rm, 1 - lane), lane == 0),
            2 => (field(rm, 1 - lane), lane == 1),
            _ => return Err(Trap::Undefined),
        };
        let a = field(rn, lane);
        let exact = if subtract { a - b } else { a + b };
        let (value, flag) = match prefix & 3 {
            1 => {
                let flag = if unsigned && !subtract {
                    exact > lane_mask as i64
                } else {
                    exact >= 0
                };
                (exact, Some(flag))
            }
            2 => (clamp(exact, width, unsigned).0, None),
            _ => (exact >> 1, None),
        };
        result |= (value as u32 & lane_mask) << (lane * width);
        if flag == Some(true) {
            ge |= if width == 8 {
                1 << lane
            } else {
                3 << (lane * 2)
            };
        }
    }
    cpu.r[rd] = result;
    if prefix & 3 == 1 {
        let cpsr = cpu.cpsr() & !psr::GE | (ge * psr::GE0);
        cpu.set_cpsr(cpsr);
    }
    Ok(1)
}

/// SMLAD, SMLSD, SMUAD, SMUSD, SMLALD and SMLSLD.
fn dual_multiply(cpu: &mut Cpu, instr: u32) -> Exec {
    let rd = (instr >> 16 & 0xF) as usize;
    let ra = (instr >> 12 & 0xF) as usize;
    let rm = cpu.get(instr & 0xF);
    let mut rs = cpu.get(instr >> 8 & 0xF);
    if instr & 1 << 5 != 0 {
        rs = rs.rotate_right(16);
    }
    let low = rm as i16 as i64 * rs as i16 as i64;
    let high = (rm >> 16) as i16 as i64 * (rs >> 16) as i16 as i64;
    let products = if instr & 1 << 6 != 0 {
        low - high
    } else {
        low + high
    };
    if instr & 1 << 22 != 0 {
        let acc = ((cpu.r[rd] as u64) << 32 | cpu.r[ra] as u64) as i64;
        let result = acc.wrapping_add(products) as u64;
        cpu.r[ra] = result as u32;
        cpu.r[rd] = (result >> 32) as u32;
    } else {
        let acc = if ra == 15 { 0 } else { cpu.r[ra] as i32 as i64 };
        let result = products + acc;
        cpu.r[rd] = result as u32;
        if result != result as i32 as i64 {
            cpu.set_flag(psr::Q, true);
        }
    }
    Ok(2)
}

/// SMMUL, SMMLA and SMMLS, with optional rounding.
fn most_significant_multiply(cpu: &mut Cpu, instr: u32) -> Exec {
    let rd = (instr >> 16 & 0xF) as usize;
    let ra = (instr >> 12 & 0xF) as usize;
    let product = cpu.get(instr & 0xF) as i32 as i64 * cpu.get(instr >> 8 & 0xF) as i32 as i64;
    let acc = if ra == 15 {
        0
    } else {
        (cpu.r[ra] as i64) << 32
    };
    let mut result = if instr & 1 << 6 != 0 {
        acc.wrapping_sub(product)
    } else {
        acc.wrapping_add(product)
    };
    if instr & 1 << 5 != 0 {
        result = result.wrapping_add(0x8000_0000);
    }
    cpu.r[rd] = (result >> 32) as u32;
    Ok(2)
}

/// USAD8 and USADA8.
fn sum_of_differences(cpu: &mut Cpu, instr: u32) -> Exec {
    let rd = (instr >> 16 & 0xF) as usize;
    let ra = (instr >> 12 & 0xF) as usize;
    let rm = cpu.get(instr & 0xF).to_le_bytes();
    let rs = cpu.get(instr >> 8 & 0xF).to_le_bytes();
    let sum: u32 = rm.iter().zip(rs).map(|(a, b)| a.abs_diff(b) as u32).sum();
    let acc = if ra == 15 { 0 } else { cpu.r[ra] };
    cpu.r[rd] = acc.wrapping_add(sum);
    Ok(2)
}

/// The ARMv6 part of the `cond` = 1111 space. `None` when `instr` is not one
/// of these.
pub(crate) fn unconditional<B: Bus>(cpu: &mut Cpu, bus: &mut B, instr: u32) -> Option<Exec> {
    if instr & 0x0FF1_FE20 == 0x0100_0000 {
        return Some(change_processor_state(cpu, instr));
    }
    if instr & 0x0FFF_FDFF == 0x0101_0000 {
        cpu.set_flag(psr::E, instr & 1 << 9 != 0);
        return Some(Ok(1));
    }
    if instr == 0xF57F_F01F {
        bus.exclusive_clear();
        return Some(Ok(1));
    }
    if instr & 0x0E5F_FFE0 == 0x084D_0500 {
        return Some(store_return_state(cpu, bus, instr));
    }
    if instr & 0x0E50_FFFF == 0x0810_0A00 {
        return Some(return_from_exception(cpu, bus, instr));
    }
    None
}

/// CPS: mask or unmask interrupts and optionally change mode.
fn change_processor_state(cpu: &mut Cpu, instr: u32) -> Exec {
    if !cpu.privileged() {
        return Ok(1);
    }
    let mut cpsr = cpu.cpsr();
    let bits = instr & (psr::A | psr::I | psr::F);
    match instr >> 18 & 3 {
        2 => cpsr &= !bits,
        3 => cpsr |= bits,
        _ => {}
    }
    if instr & 1 << 17 != 0 {
        cpsr = cpsr & !psr::MODE | instr & psr::MODE;
    }
    cpu.set_cpsr(cpsr);
    Ok(1)
}

/// The lower address of the two words SRS and RFE transfer, and the new
/// base.
fn pair_address(instr: u32, base: u32) -> (u32, u32) {
    let up = instr & 1 << 23 != 0;
    let pre = instr & 1 << 24 != 0;
    let low = match (pre, up) {
        (false, true) => base,
        (true, true) => base.wrapping_add(4),
        (false, false) => base.wrapping_sub(4),
        (true, false) => base.wrapping_sub(8),
    };
    let new_base = if up {
        base.wrapping_add(8)
    } else {
        base.wrapping_sub(8)
    };
    (low & !3, new_base)
}

/// SRS: store the link register and SPSR on the stack of another mode.
fn store_return_state<B: Bus>(cpu: &mut Cpu, bus: &mut B, instr: u32) -> Exec {
    if !cpu.has_spsr() {
        return Err(Trap::Undefined);
    }
    let target = instr & psr::MODE;
    let (addr, new_base) = pair_address(instr, cpu.banked_sp(target));
    let privileged = cpu.privileged();
    bus.write32(addr, cpu.r[14], privileged)?;
    bus.write32(addr.wrapping_add(4), cpu.spsr(), privileged)?;
    if instr & 1 << 21 != 0 {
        cpu.set_banked_sp(target, new_base);
    }
    Ok(3)
}

/// RFE: load the program counter and the CPSR from memory.
fn return_from_exception<B: Bus>(cpu: &mut Cpu, bus: &mut B, instr: u32) -> Exec {
    if cpu.cpsr() & psr::MODE == mode::USR {
        return Err(Trap::Undefined);
    }
    let rn = (instr >> 16 & 0xF) as usize;
    let (addr, new_base) = pair_address(instr, cpu.r[rn]);
    let privileged = cpu.privileged();
    let pc = bus.read32(addr, privileged)?;
    let cpsr = bus.read32(addr.wrapping_add(4), privileged)?;
    if instr & 1 << 21 != 0 {
        cpu.r[rn] = new_base;
    }
    cpu.set_cpsr(cpsr);
    cpu.branch(pc);
    bus.exclusive_clear();
    Ok(5)
}
