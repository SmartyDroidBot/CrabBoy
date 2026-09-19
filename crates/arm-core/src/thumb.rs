//! The 16-bit Thumb instruction set (ARMv5T).

use crate::alu::{add_with_carry, shift_imm, shift_reg, Shift};
use crate::bus::Bus;
use crate::cpu::{psr, Cpu, Exec, Trap};

fn set_arithmetic(cpu: &mut Cpu, (result, carry, overflow): (u32, bool, bool)) -> u32 {
    cpu.set_nz(result);
    cpu.set_flag(psr::C, carry);
    cpu.set_flag(psr::V, overflow);
    result
}

fn set_logical(cpu: &mut Cpu, (result, carry): (u32, bool)) -> u32 {
    cpu.set_nz(result);
    cpu.set_flag(psr::C, carry);
    result
}

pub(crate) fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B, instr: u32) -> Exec {
    let low = |shift: u32| (instr >> shift & 7) as usize;
    let privileged = cpu.privileged();
    // ARMv6 can allow unaligned word and halfword accesses.
    let (w, h) = if bus.unaligned_access() {
        (!0u32, !0u32)
    } else {
        (!3u32, !1u32)
    };

    match instr >> 11 {
        // LSL, LSR, ASR by immediate.
        0b00000..=0b00010 => {
            let kind = Shift::from_bits(instr >> 11);
            let shifted = shift_imm(kind, cpu.r[low(3)], instr >> 6 & 0x1F, cpu.flag(psr::C));
            cpu.r[low(0)] = set_logical(cpu, shifted);
            Ok(1)
        }
        // ADD and SUB with a register or a 3-bit immediate.
        0b00011 => {
            let a = cpu.r[low(3)];
            let b = if instr & 1 << 10 != 0 {
                instr >> 6 & 7
            } else {
                cpu.r[low(6)]
            };
            let sum = if instr & 1 << 9 != 0 {
                add_with_carry(a, !b, true)
            } else {
                add_with_carry(a, b, false)
            };
            cpu.r[low(0)] = set_arithmetic(cpu, sum);
            Ok(1)
        }
        // MOV, CMP, ADD, SUB with an 8-bit immediate.
        0b00100..=0b00111 => {
            let rd = low(8);
            let imm = instr & 0xFF;
            match instr >> 11 & 3 {
                0 => {
                    cpu.r[rd] = imm;
                    cpu.set_nz(imm);
                }
                1 => {
                    set_arithmetic(cpu, add_with_carry(cpu.r[rd], !imm, true));
                }
                2 => cpu.r[rd] = set_arithmetic(cpu, add_with_carry(cpu.r[rd], imm, false)),
                _ => cpu.r[rd] = set_arithmetic(cpu, add_with_carry(cpu.r[rd], !imm, true)),
            }
            Ok(1)
        }
        0b01000 => {
            if instr & 1 << 10 == 0 {
                alu(cpu, instr)
            } else {
                high_register(cpu, instr)
            }
        }
        // LDR from the literal pool.
        0b01001 => {
            let addr = (cpu.pc_read & !3).wrapping_add((instr & 0xFF) << 2);
            cpu.r[low(8)] = bus.read32(addr, privileged)?;
            Ok(2)
        }
        // Load and store with a register offset.
        0b01010 | 0b01011 => {
            let addr = cpu.r[low(3)].wrapping_add(cpu.r[low(6)]);
            let rd = low(0);
            match instr >> 9 & 7 {
                0 => bus.write32(addr & w, cpu.r[rd], privileged)?,
                1 => bus.write16(addr & h, cpu.r[rd] as u16, privileged)?,
                2 => bus.write8(addr, cpu.r[rd] as u8, privileged)?,
                3 => cpu.r[rd] = bus.read8(addr, privileged)? as i8 as u32,
                4 => cpu.r[rd] = bus.read32(addr & w, privileged)?,
                5 => cpu.r[rd] = bus.read16(addr & h, privileged)? as u32,
                6 => cpu.r[rd] = bus.read8(addr, privileged)? as u32,
                _ => cpu.r[rd] = bus.read16(addr & h, privileged)? as i16 as u32,
            }
            Ok(2)
        }
        // STR and LDR with a 5-bit word offset.
        0b01100 | 0b01101 => {
            let addr = cpu.r[low(3)].wrapping_add((instr >> 6 & 0x1F) << 2) & w;
            if instr & 1 << 11 != 0 {
                cpu.r[low(0)] = bus.read32(addr, privileged)?;
            } else {
                bus.write32(addr, cpu.r[low(0)], privileged)?;
            }
            Ok(2)
        }
        // STRB and LDRB.
        0b01110 | 0b01111 => {
            let addr = cpu.r[low(3)].wrapping_add(instr >> 6 & 0x1F);
            if instr & 1 << 11 != 0 {
                cpu.r[low(0)] = bus.read8(addr, privileged)? as u32;
            } else {
                bus.write8(addr, cpu.r[low(0)] as u8, privileged)?;
            }
            Ok(2)
        }
        // STRH and LDRH.
        0b10000 | 0b10001 => {
            let addr = cpu.r[low(3)].wrapping_add((instr >> 6 & 0x1F) << 1) & h;
            if instr & 1 << 11 != 0 {
                cpu.r[low(0)] = bus.read16(addr, privileged)? as u32;
            } else {
                bus.write16(addr, cpu.r[low(0)] as u16, privileged)?;
            }
            Ok(2)
        }
        // STR and LDR relative to the stack pointer.
        0b10010 | 0b10011 => {
            let addr = cpu.r[13].wrapping_add((instr & 0xFF) << 2) & !3;
            if instr & 1 << 11 != 0 {
                cpu.r[low(8)] = bus.read32(addr, privileged)?;
            } else {
                bus.write32(addr, cpu.r[low(8)], privileged)?;
            }
            Ok(2)
        }
        // ADD to the program counter or the stack pointer.
        0b10100 => {
            cpu.r[low(8)] = (cpu.pc_read & !3).wrapping_add((instr & 0xFF) << 2);
            Ok(1)
        }
        0b10101 => {
            cpu.r[low(8)] = cpu.r[13].wrapping_add((instr & 0xFF) << 2);
            Ok(1)
        }
        0b10110 | 0b10111 => miscellaneous(cpu, bus, instr),
        // STMIA and LDMIA.
        0b11000 | 0b11001 => {
            let rn = low(8);
            let list = instr & 0xFF;
            let load = instr & 1 << 11 != 0;
            let mut addr = cpu.r[rn] & !3;
            // As in ARM state, an empty list moves the base by sixteen words.
            let new_base = cpu.r[rn].wrapping_add(if list == 0 {
                0x40
            } else {
                list.count_ones() * 4
            });
            for reg in 0..8 {
                if list & 1 << reg == 0 {
                    continue;
                }
                if load {
                    cpu.r[reg] = bus.read32(addr, privileged)?;
                } else {
                    bus.write32(addr, cpu.r[reg], privileged)?;
                }
                addr = addr.wrapping_add(4);
            }
            if !load || list & 1 << rn == 0 {
                cpu.r[rn] = new_base;
            }
            Ok(list.count_ones() + 1)
        }
        // Conditional branch, undefined, and SWI.
        0b11010 | 0b11011 => match instr >> 8 & 0xF {
            0xE => Err(Trap::Undefined),
            0xF => Err(Trap::Supervisor(instr & 0xFF)),
            cond => {
                if cpu.condition(cond) {
                    let offset = ((instr & 0xFF) as i8 as i32 as u32) << 1;
                    cpu.branch(cpu.pc_read.wrapping_add(offset));
                    Ok(3)
                } else {
                    Ok(1)
                }
            }
        },
        // Unconditional branch.
        0b11100 => {
            let offset = ((instr << 21) as i32 >> 20) as u32;
            cpu.branch(cpu.pc_read.wrapping_add(offset));
            Ok(3)
        }
        // BLX suffix: into ARM state.
        0b11101 => {
            if instr & 1 != 0 {
                return Err(Trap::Undefined);
            }
            let target = cpu.r[14].wrapping_add((instr & 0x7FF) << 1) & !3;
            cpu.r[14] = cpu.r[15] | 1;
            cpu.branch_exchange(target);
            Ok(3)
        }
        // BL prefix: the high part of the offset goes to the link register.
        0b11110 => {
            let offset = ((instr << 21) as i32 >> 9) as u32;
            cpu.r[14] = cpu.pc_read.wrapping_add(offset);
            Ok(1)
        }
        // BL suffix.
        _ => {
            let target = cpu.r[14].wrapping_add((instr & 0x7FF) << 1);
            cpu.r[14] = cpu.r[15] | 1;
            cpu.branch(target);
            Ok(3)
        }
    }
}

/// The sixteen register-to-register operations of format 4.
fn alu(cpu: &mut Cpu, instr: u32) -> Exec {
    let rd = (instr & 7) as usize;
    let a = cpu.r[rd];
    let b = cpu.r[(instr >> 3 & 7) as usize];
    let carry = cpu.flag(psr::C);
    let mut cycles = 1;
    match instr >> 6 & 0xF {
        0x0 => {
            cpu.r[rd] = a & b;
            cpu.set_nz(a & b);
        }
        0x1 => {
            cpu.r[rd] = a ^ b;
            cpu.set_nz(a ^ b);
        }
        op @ (0x2..=0x4 | 0x7) => {
            let kind = match op {
                0x2 => Shift::Lsl,
                0x3 => Shift::Lsr,
                0x4 => Shift::Asr,
                _ => Shift::Ror,
            };
            cpu.r[rd] = set_logical(cpu, shift_reg(kind, a, b, carry));
            cycles = 2;
        }
        0x5 => cpu.r[rd] = set_arithmetic(cpu, add_with_carry(a, b, carry)),
        0x6 => cpu.r[rd] = set_arithmetic(cpu, add_with_carry(a, !b, carry)),
        0x8 => cpu.set_nz(a & b),
        0x9 => cpu.r[rd] = set_arithmetic(cpu, add_with_carry(0, !b, true)),
        0xA => {
            set_arithmetic(cpu, add_with_carry(a, !b, true));
        }
        0xB => {
            set_arithmetic(cpu, add_with_carry(a, b, false));
        }
        0xC => {
            cpu.r[rd] = a | b;
            cpu.set_nz(a | b);
        }
        0xD => {
            cpu.r[rd] = a.wrapping_mul(b);
            cpu.set_nz(cpu.r[rd]);
            cycles = 3;
        }
        0xE => {
            cpu.r[rd] = a & !b;
            cpu.set_nz(a & !b);
        }
        _ => {
            cpu.r[rd] = !b;
            cpu.set_nz(!b);
        }
    }
    Ok(cycles)
}

/// ADD, CMP and MOV on any register, and BX and BLX.
fn high_register(cpu: &mut Cpu, instr: u32) -> Exec {
    let rd = instr >> 4 & 8 | instr & 7;
    let rm = instr >> 3 & 0xF;
    let value = cpu.get(rm);
    match instr >> 8 & 3 {
        0 | 2 => {
            let result = if instr >> 8 & 3 == 0 {
                cpu.get(rd).wrapping_add(value)
            } else {
                value
            };
            if rd == 15 {
                cpu.branch(result);
                return Ok(3);
            }
            cpu.r[rd as usize] = result;
        }
        1 => {
            set_arithmetic(cpu, add_with_carry(cpu.get(rd), !value, true));
        }
        _ => {
            if instr & 1 << 7 != 0 {
                cpu.r[14] = cpu.r[15] | 1;
            }
            cpu.branch_exchange(value);
            return Ok(3);
        }
    }
    Ok(1)
}

/// The 1011 space: stack adjustment, PUSH, POP and BKPT.
fn miscellaneous<B: Bus>(cpu: &mut Cpu, bus: &mut B, instr: u32) -> Exec {
    let privileged = cpu.privileged();
    match instr >> 8 & 0xF {
        0x0 => {
            let offset = (instr & 0x7F) << 2;
            cpu.r[13] = if instr & 1 << 7 != 0 {
                cpu.r[13].wrapping_sub(offset)
            } else {
                cpu.r[13].wrapping_add(offset)
            };
            Ok(1)
        }
        // PUSH, optionally with the link register.
        0x4 | 0x5 => {
            let list = instr & 0xFF;
            let with_lr = instr & 1 << 8 != 0;
            let count = list.count_ones() + with_lr as u32;
            let base = cpu.r[13].wrapping_sub(count * 4);
            let mut addr = base & !3;
            for reg in 0..8 {
                if list & 1 << reg != 0 {
                    bus.write32(addr, cpu.r[reg], privileged)?;
                    addr = addr.wrapping_add(4);
                }
            }
            if with_lr {
                bus.write32(addr, cpu.r[14], privileged)?;
            }
            cpu.r[13] = base;
            Ok(count + 1)
        }
        // POP, optionally into the program counter, which interworks.
        0xC | 0xD => {
            let list = instr & 0xFF;
            let with_pc = instr & 1 << 8 != 0;
            let count = list.count_ones() + with_pc as u32;
            let mut addr = cpu.r[13] & !3;
            for reg in 0..8 {
                if list & 1 << reg != 0 {
                    cpu.r[reg] = bus.read32(addr, privileged)?;
                    addr = addr.wrapping_add(4);
                }
            }
            let target = if with_pc {
                Some(bus.read32(addr, privileged)?)
            } else {
                None
            };
            cpu.r[13] = cpu.r[13].wrapping_add(count * 4);
            if let Some(target) = target {
                cpu.branch_exchange(target);
                return Ok(count + 4);
            }
            Ok(count + 1)
        }
        0xE => Err(Trap::Breakpoint),
        // SXTH, SXTB, UXTH, UXTB.
        0x2 if cpu.v6() => {
            let value = cpu.r[(instr >> 3 & 7) as usize];
            cpu.r[(instr & 7) as usize] = match instr >> 6 & 3 {
                0 => value as u16 as i16 as u32,
                1 => value as u8 as i8 as u32,
                2 => value & 0xFFFF,
                _ => value & 0xFF,
            };
            Ok(1)
        }
        // SETEND and CPS.
        0x6 if cpu.v6() => {
            if instr & 0xFFF7 == 0xB650 {
                cpu.set_flag(psr::E, instr & 8 != 0);
            } else if instr & 0xFFE8 == 0xB660 {
                if cpu.privileged() {
                    let bits = (instr & 7) << 6;
                    let cpsr = cpu.cpsr();
                    cpu.set_cpsr(if instr & 0x10 != 0 {
                        cpsr | bits
                    } else {
                        cpsr & !bits
                    });
                }
            } else {
                return Err(Trap::Undefined);
            }
            Ok(1)
        }
        // REV, REV16, REVSH.
        0xA if cpu.v6() => {
            let value = cpu.r[(instr >> 3 & 7) as usize];
            cpu.r[(instr & 7) as usize] = match instr >> 6 & 3 {
                0 => value.swap_bytes(),
                1 => (value >> 8 & 0x00FF_00FF) | (value << 8 & 0xFF00_FF00),
                3 => (value as u16).swap_bytes() as i16 as u32,
                _ => return Err(Trap::Undefined),
            };
            Ok(1)
        }
        _ => Err(Trap::Undefined),
    }
}
