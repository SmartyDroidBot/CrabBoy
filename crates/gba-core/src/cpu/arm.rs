//! ARM (32-bit) instruction set decoder/executor for the ARM7TDMI.
//!
//! Correctness-first implementation. Each instruction is dispatched by
//! examining the top bits; condition codes gate every instruction. Cycle
//! accounting uses the classic S/N/I model and is refined once wait states are
//! introduced in the bus layer.

use super::{field, mode, Bus, Cpu};

#[inline]
fn cond_holds(cpu: &Cpu, cond: u32) -> bool {
    let n = cpu.cpsr & super::flag::N != 0;
    let z = cpu.cpsr & super::flag::Z != 0;
    let c = cpu.cpsr & super::flag::C != 0;
    let v = cpu.cpsr & super::flag::V != 0;
    match cond {
        0x0 => z,                            // EQ
        0x1 => !z,                           // NE
        0x2 => c,                            // CS
        0x3 => !c,                           // CC
        0x4 => n,                            // MI
        0x5 => !n,                           // PL
        0x6 => v,                            // VS
        0x7 => !v,                           // VC
        0x8 => c && !z,                      // HI
        0x9 => !c || z,                      // LS
        0xA => n == v,                       // GE
        0xB => n != v,                       // LT
        0xC => !z && n == v,                 // GT
        0xD => z || n != v,                  // LE
        _ => true,                           // AL
    }
}

/// Barrel shifter for a register operand. Returns (shifted value, carry-out).
#[inline]
pub(crate) fn shift_reg(operand: u32, stype: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    match stype {
        0 => {
            // LSL
            if amount == 0 {
                (operand, carry_in)
            } else if amount < 32 {
                (operand.wrapping_shl(amount), operand & (1u32 << (32 - amount)) != 0)
            } else if amount == 32 {
                (0, operand & 1 != 0)
            } else {
                (0, false)
            }
        }
        1 => {
            // LSR
            if amount == 0 {
                (0, operand >> 31 != 0)
            } else if amount < 32 {
                (operand >> amount, operand & (1u32 << (amount - 1)) != 0)
            } else if amount == 32 {
                (0, operand >> 31 != 0)
            } else {
                (0, false)
            }
        }
        2 => {
            // ASR
            if amount == 0 || amount >= 32 {
                (if operand & (1u32 << 31) != 0 { u32::MAX } else { 0 }, operand >> 31 != 0)
            } else {
                let sign = operand >> 31 != 0;
                let c = operand & (1u32 << (amount - 1)) != 0;
                let v = if sign {
                    operand | !((1u32 << (32 - amount)) - 1)
                } else {
                    operand >> amount
                };
                (v, c)
            }
        }
        _ => {
            // ROR (amount 0 => RRX)
            if amount == 0 {
                let v = (operand >> 1) | (if carry_in { 1u32 << 31 } else { 0 });
                (v, operand & 1 != 0)
            } else {
                let amt = amount & 31;
                (operand.rotate_right(amt), operand & (1u32 << (amt - 1)) != 0)
            }
        }
    }
}

#[inline]
fn rotate_imm(v: u32, rot: u32) -> u32 {
    if rot == 0 {
        v
    } else {
        v.rotate_right(rot * 2)
    }
}

/// Carry produced by an immediate rotate (bit31 of the one-less rotation).
#[inline]
fn imm_carry(cpu: &Cpu, imm: u8, rot: u32) -> bool {
    if rot == 0 {
        cpu.cpsr & super::flag::C != 0
    } else {
        rotate_imm(imm as u32, rot - 1) & (1 << 31) != 0
    }
}

#[inline]
pub(crate) fn add(a: u32, b: u32) -> (u32, bool, bool) {
    let r = a.wrapping_add(b);
    let c = (a as u64 + b as u64) > u32::MAX as u64;
    let v = ((a ^ r) & (b ^ r) & (1 << 31)) != 0;
    (r, c, v)
}

#[inline]
pub(crate) fn sub(a: u32, b: u32) -> (u32, bool, bool) {
    let r = a.wrapping_sub(b);
    let c = a >= b;
    let v = ((a ^ b) & (a ^ r) & (1 << 31)) != 0;
    (r, c, v)
}

#[inline]
pub(crate) fn set_flags(cpu: &mut Cpu, result: u32, c: bool, v: bool) {
    let mut f = cpu.cpsr & !field::FLAGS;
    if result & (1 << 31) != 0 {
        f |= super::flag::N;
    }
    if result == 0 {
        f |= super::flag::Z;
    }
    if c {
        f |= super::flag::C;
    }
    if v {
        f |= super::flag::V;
    }
    cpu.cpsr = f;
}

/// Decode and execute a single ARM instruction.
pub fn execute(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    let cond = inst >> 28;
    if !cond_holds(cpu, cond) {
        return;
    }

    // BX
    if inst & 0x0FFF_FFF0 == 0x012F_FF10 {
        let v = cpu.reg(inst & 0xF);
        cpu.bx(v);
        cpu.add_cycles(3);
        return;
    }
    // MRS
    if inst & 0x0FFF_0FF0 == 0x010F_0000 {
        let rd = (inst >> 12) & 0xF;
        let from_spsr = (inst >> 22) & 1 != 0;
        let v = if from_spsr {
            let m = cpu.cpsr & 0x1F;
            cpu.get_spsr(m)
        } else {
            cpu.cpsr
        };
        cpu.set_reg(rd, v);
        cpu.add_cycles(2);
        return;
    }
    // MSR (register)
    if inst & 0x0FB0_FFF0 == 0x0120_F000 {
        let v = cpu.reg(inst & 0xF);
        msr_write(cpu, inst, v);
        cpu.add_cycles(2);
        return;
    }
    // MSR (immediate)
    if inst & 0x0FB0_F000 == 0x0320_F000 {
        let v = rotate_imm(inst & 0xFF, (inst >> 8) & 0xF);
        msr_write(cpu, inst, v);
        cpu.add_cycles(2);
        return;
    }

    // Data-processing family (bits 27:26 = 00).
    if inst & 0x0C00_0000 == 0 {
        data_processing(cpu, bus, inst);
        return;
    }
    // Single data transfer (LDR/STR).
    if inst & 0x0C00_0000 == 0x0400_0000 {
        single_transfer(cpu, bus, inst);
        return;
    }
    // Block transfer (LDM/STM): bits 27:25 = 100.
    if inst & 0x0E00_0000 == 0x0800_0000 {
        block_transfer(cpu, bus, inst);
        return;
    }
    // Branch / branch-with-link: bits 27:25 = 101.
    if inst & 0x0E00_0000 == 0x0A00_0000 {
        branch(cpu, inst);
        return;
    }
    // Coprocessor instructions: no-ops on the GBA.
    if inst & 0x0F00_0010 == 0x0E00_0000 || inst & 0x0F00_0010 == 0x0E00_0010 {
        cpu.add_cycles(2);
        return;
    }
    if inst & 0x0E00_0000 == 0x0C00_0000 {
        cpu.add_cycles(3);
        return;
    }
    // SWI
    if inst & 0x0F00_0000 == 0x0F00_0000 {
        let num = inst & 0x00FF_FFFF;
        if crate::bios::is_known(num) {
            cpu.swi_bios(cpu.pc, num);
        } else {
            cpu.swi(cpu.pc);
        }
        cpu.add_cycles(3);
        return;
    }
    // Undefined / unhandled: leave PC as-is (no-op).
    cpu.add_cycles(2);
}

fn msr_write(cpu: &mut Cpu, inst: u32, value: u32) {
    let to_spsr = (inst >> 22) & 1 != 0;
    let field_mask = (inst >> 16) & 0xF;
    let m = cpu.cpsr & 0x1F;
    let privileged = m != mode::USR && m != 0x1F;
    let mut v = cpu.cpsr;
    if field_mask & 1 != 0 {
        v = (v & !field::FLAGS) | (value & field::FLAGS);
    }
    if field_mask & 2 != 0 && privileged {
        v = (v & !0xFF) | (value & 0xFF);
    }
    if to_spsr {
        if privileged {
            cpu.set_spsr(m, v);
        }
    } else {
        cpu.set_cpsr(v);
    }
}

fn data_processing(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    // SWP / SWPB
    if inst & 0x0FB0_00F0 == 0x0100_0090 {
        swap(cpu, bus, inst);
        return;
    }
    // Long multiply
    if inst & 0x0F80_00F0 == 0x0080_0090 {
        long_multiply(cpu, inst);
        return;
    }
    // Multiply
    if inst & 0x0FC0_00F0 == 0x0000_0090 {
        multiply(cpu, inst);
        return;
    }
    // Halfword / signed byte transfer
    if inst & 0x0E00_0090 == 0x0000_0090 {
        halfword_transfer(cpu, bus, inst);
        return;
    }

    let i = inst & (1 << 25) != 0;
    let opcode = (inst >> 21) & 0xF;
    let s = inst & (1 << 20) != 0;
    let rn = (inst >> 16) & 0xF;
    let rd = (inst >> 12) & 0xF;

    // Operand 2 and its carry.
    let carry_in = cpu.cpsr & super::flag::C != 0;
    let (operand2, carry_out) = if i {
        let imm = (inst & 0xFF) as u8;
        let rot = (inst >> 8) & 0xF;
        (rotate_imm(imm as u32, rot), imm_carry(cpu, imm, rot))
    } else {
        let rm = inst & 0xF;
        let opv = cpu.reg(rm);
        let stype = (inst >> 5) & 3;
        if inst & (1 << 4) != 0 {
            let rs = (inst >> 8) & 0xF;
            let amount = cpu.reg(rs) & 0xFF;
            shift_reg(opv, stype, amount, carry_in)
        } else {
            let amount = (inst >> 7) & 0x1F;
            shift_reg(opv, stype, amount, carry_in)
        }
    };

    let rn_v = cpu.reg(rn);
    let old_carry = cpu.cpsr & super::flag::C != 0;

    let (result, fc, fv) = match opcode {
        0 => (rn_v & operand2, carry_out, false),                 // AND
        1 => (rn_v ^ operand2, carry_out, false),                 // EOR
        2 => sub(rn_v, operand2),                                 // SUB
        3 => sub(operand2, rn_v),                                 // RSB
        4 => add(rn_v, operand2),                                 // ADD
        5 => {
            let (t, c1, v1) = add(rn_v, operand2);
            let (t2, c2, v2) = add(t, if old_carry { 1 } else { 0 });
            (t2, c1 || c2, v1 || v2)
        }                                                         // ADC
        6 => {
            let (t, c1, v1) = sub(rn_v, operand2);
            let (t2, c2, v2) = sub(t, if old_carry { 0 } else { 1 });
            (t2, c1 || c2, v1 || v2)
        }                                                         // SBC
        7 => {
            let (t, c1, v1) = sub(operand2, rn_v);
            let (t2, c2, v2) = sub(t, if old_carry { 0 } else { 1 });
            (t2, c1 || c2, v1 || v2)
        }                                                         // RSC
        8 => (rn_v & operand2, carry_out, false),                 // TST
        9 => (rn_v ^ operand2, carry_out, false),                 // TEQ
        10 => sub(rn_v, operand2),                                // CMP
        11 => add(rn_v, operand2),                                // CMN
        12 => (rn_v | operand2, carry_out, false),                // ORR
        13 => (operand2, carry_out, false),                       // MOV
        14 => (rn_v & !operand2, carry_out, false),               // BIC
        _ => (!operand2, carry_out, false),                       // MVN
    };

    let is_test = (8..=11).contains(&opcode);

    // TST/TEQ/CMP/CMN always set flags; others set only when S.
    if is_test || (s && !is_test) {
        set_flags(cpu, result, fc, fv);
    }

    if !is_test {
        if rd == 15 {
            if s {
                let m = cpu.cpsr & 0x1F;
                if m != mode::USR && m != 0x1F {
                    // Write flag bits into SPSR (pseudo MSR).
                    let spsr = cpu.get_spsr(m);
                    cpu.set_spsr(m, (spsr & !field::FLAGS) | (result & field::FLAGS));
                }
            } else {
                cpu.branch(result);
            }
        } else {
            cpu.set_reg(rd, result);
        }
    }

    cpu.add_cycles(1);
}

fn multiply(cpu: &mut Cpu, inst: u32) {
    let s = inst & (1 << 20) != 0;
    let rd = (inst >> 16) & 0xF;
    let rn = (inst >> 12) & 0xF;
    let rs = (inst >> 8) & 0xF;
    let rm = inst & 0xF;
    let accumulate = inst & (1 << 21) != 0;
    let mut result = cpu.reg(rm).wrapping_mul(cpu.reg(rs));
    if accumulate {
        result = result.wrapping_add(cpu.reg(rn));
    }
    if s {
        set_flags(cpu, result, cpu.cpsr & super::flag::C != 0, cpu.cpsr & super::flag::V != 0);
    }
    if rd == 15 {
        cpu.branch(result);
    } else {
        cpu.set_reg(rd, result);
    }
    cpu.add_cycles(1);
}

fn long_multiply(cpu: &mut Cpu, inst: u32) {
    let s = inst & (1 << 20) != 0;
    let signed = inst & (1 << 22) != 0;
    let accumulate = inst & (1 << 21) != 0;
    let rdhi = (inst >> 16) & 0xF;
    let rdlo = (inst >> 12) & 0xF;
    let rs = (inst >> 8) & 0xF;
    let rm = inst & 0xF;
    let op1 = cpu.reg(rm);
    let op2 = cpu.reg(rs);
    let r: i128 = if signed {
        (op1 as i32 as i128) * (op2 as i32 as i128)
    } else {
        (op1 as i128) * (op2 as i128)
    };
    let mut result = r as u128;
    if accumulate {
        let acc = ((cpu.reg(rdhi) as u128) << 32) | (cpu.reg(rdlo) as u128);
        result = result.wrapping_add(acc);
    }
    let lo = result & 0xFFFF_FFFF;
    let hi = (result >> 32) & 0xFFFF_FFFF;
    if s {
        let combined = result as u32;
        let n = hi & (1 << 31) != 0;
        let z = (hi | lo) == 0;
        let mut f = cpu.cpsr & !field::FLAGS;
        if n {
            f |= super::flag::N;
        }
        if z {
            f |= super::flag::Z;
        }
        cpu.cpsr = f;
        let _ = combined;
    }
    cpu.set_reg(rdlo, lo as u32);
    cpu.set_reg(rdhi, hi as u32);
    cpu.add_cycles(1);
}

fn swap(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    let byte = inst & (1 << 22) != 0;
    let rn = (inst >> 16) & 0xF;
    let rd = (inst >> 12) & 0xF;
    let rm = inst & 0xF;
    let addr = cpu.reg(rn);
    let tmp = if byte {
        bus.read8(addr)
    } else {
        bus.read32(addr)
    };
    let data = cpu.reg(rm);
    if byte {
        bus.write8(addr, data);
    } else {
        bus.write32(addr, data);
    }
    if rd == 15 {
        cpu.branch(tmp);
    } else {
        cpu.set_reg(rd, tmp);
    }
    cpu.add_cycles(1);
}

fn halfword_transfer(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    let p = inst & (1 << 24) != 0;
    let u = inst & (1 << 23) != 0;
    let i = inst & (1 << 22) != 0;
    let w = inst & (1 << 21) != 0;
    let l = inst & (1 << 20) != 0;
    let rn = (inst >> 16) & 0xF;
    let rd = (inst >> 12) & 0xF;
    let sh = ((inst >> 6) & 1) << 1 | (inst >> 5) & 1; // bit6=S, bit5=H

    let offset = if i {
        ((inst >> 4) & 0xF0) | (inst & 0xF)
    } else {
        cpu.reg(inst & 0xF)
    };
    let base = cpu.reg(rn);
    let delta = if u { offset } else { offset.wrapping_neg() };
    let (addr, wb) = if p {
        (base.wrapping_add(delta), if w { Some(base.wrapping_add(delta)) } else { None })
    } else {
        (base, if w { Some(base.wrapping_add(delta)) } else { None })
    };

    if l {
        let value = match sh {
            0b01 => bus.read16(addr),
            0b10 => (bus.read8(addr) as i8 as i32) as u32,
            0b11 => (bus.read16(addr) as i16 as i32) as u32,
            _ => 0,
        };
        if rd == 15 {
            cpu.branch(value);
        } else {
            cpu.set_reg(rd, value);
        }
    } else if sh == 0b01 {
        bus.write16(addr, cpu.reg(rd));
    }
    if let Some(v) = wb {
        if rn != 15 && rn != rd {
            cpu.set_reg(rn, v);
        }
    }
    cpu.add_cycles(1);
}

fn single_transfer(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    let i = inst & (1 << 25) != 0;
    let p = inst & (1 << 24) != 0;
    let u = inst & (1 << 23) != 0;
    let b = inst & (1 << 22) != 0;
    let w = inst & (1 << 21) != 0;
    let l = inst & (1 << 20) != 0;
    let rn = (inst >> 16) & 0xF;
    let rd = (inst >> 12) & 0xF;

    let offset = if i {
        let rm = inst & 0xF;
        let stype = (inst >> 5) & 3;
        let amount = (inst >> 7) & 0x1F;
        let carry_in = cpu.cpsr & super::flag::C != 0;
        shift_reg(cpu.reg(rm), stype, amount, carry_in).0
    } else {
        inst & 0xFFF
    };
    let base = cpu.reg(rn);
    let delta = if u { offset } else { offset.wrapping_neg() };
    let (addr, wb) = if p {
        (base.wrapping_add(delta), if w { Some(base.wrapping_add(delta)) } else { None })
    } else {
        (base, if w { Some(base.wrapping_add(delta)) } else { None })
    };

    if l {
        let value = if b {
            bus.read8(addr)
        } else {
            bus.read32(addr)
        };
        if rd == 15 {
            cpu.branch(value);
        } else {
            cpu.set_reg(rd, value);
        }
    } else {
        let value = cpu.reg(rd);
        if b {
            bus.write8(addr, value);
        } else {
            bus.write32(addr, value);
        }
    }
    // Writeback: for a load with rn==rd the loaded value wins.
    if let Some(v) = wb {
        if rn != 15 && !(l && rn == rd) {
            cpu.set_reg(rn, v);
        }
    }
    cpu.add_cycles(1);
}

fn block_transfer(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    let p = inst & (1 << 24) != 0;
    let u = inst & (1 << 23) != 0;
    let s = inst & (1 << 22) != 0;
    let w = inst & (1 << 21) != 0;
    let l = inst & (1 << 20) != 0;
    let rn = (inst >> 16) & 0xF;
    let list = inst & 0xFFFF;
    let count = list.count_ones();
    let base = cpu.reg(rn);

    let mut addr = if u {
        if p {
            base.wrapping_add(4)
        } else {
            base
        }
    } else if p {
        base.wrapping_sub(4 * count)
    } else {
        base.wrapping_sub(4 * (count - 1))
    };
    let wb_val = if u {
        base.wrapping_add(4 * count)
    } else {
        base.wrapping_sub(4 * count)
    };

    let mut lr_value: Option<u32> = None;
    if l {
        for r in 0..16u32 {
            if list & (1 << r) != 0 {
                let v = bus.read32(addr);
                addr = addr.wrapping_add(4);
                if r == 15 {
                    lr_value = Some(v);
                } else {
                    cpu.set_reg(r, v);
                }
            }
        }
        // ^ with r15 in list: load CPSR from SPSR.
        if s {
            let m = cpu.cpsr & 0x1F;
            if m != mode::USR && m != 0x1F && list & (1 << 15) != 0 {
                let spsr = cpu.get_spsr(m);
                cpu.cpsr = spsr;
            }
        }
        if let Some(v) = lr_value {
            cpu.branch(v);
        }
    } else {
        // Store. r15 stores pc+8; ^ with r15 stores SPSR instead (in exc. modes).
        let store_usr = s;
        for r in 0..16u32 {
            if list & (1 << r) != 0 {
                let v = if store_usr {
                    if r == 15 {
                        let m = cpu.cpsr & 0x1F;
                        if m != mode::USR && m != 0x1F {
                            cpu.get_spsr(m)
                        } else {
                            cpu.reg(15)
                        }
                    } else {
                        cpu.usr_reg(r)
                    }
                } else if r == 15 {
                    cpu.reg(15)
                } else {
                    cpu.reg(r)
                };
                bus.write32(addr, v);
                addr = addr.wrapping_add(4);
            }
        }
    }
    if w && !(rn == 15 && list & (1 << 15) != 0) {
        cpu.set_reg(rn, wb_val);
    }
    cpu.add_cycles(1);
}

fn branch(cpu: &mut Cpu, inst: u32) {
    let l = inst & (1 << 24) != 0;
    let off = inst & 0x00FF_FFFF;
    let signed_off = if off & 0x0080_0000 != 0 {
        (off | 0xFF00_0000) as i32
    } else {
        off as i32
    };
    let target = (cpu.reg(15) as i64) + ((signed_off as i64) << 2);
    if l {
        cpu.set_reg(14, cpu.pc);
    }
    cpu.branch(target as u32);
    cpu.add_cycles(2);
}