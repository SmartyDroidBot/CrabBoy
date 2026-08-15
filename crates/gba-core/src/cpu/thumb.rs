//! Thumb (16-bit) instruction set decoder/executor for the ARM7TDMI.
//!
//! Dispatch is by the top five bits (`inst >> 11`), with a few sub-fields
//! handled by inspecting individual bits. Correctness-first.

use super::arm::{add, set_flags, shift_reg, sub};
use super::{flag, Bus, Cpu};

#[inline]
fn rn(cpu: &Cpu, n: u32) -> u32 {
    cpu.reg(n)
}

/// Execute a single Thumb instruction.
pub fn execute(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    let top = (inst >> 11) & 0x1F;
    match top {
        0..=2 => shift_imm(cpu, inst),
        3 => add_sub(cpu, inst),
        4..=7 => mov_cmp_add_sub_imm(cpu, inst),
        8 => {
            if inst & (1 << 10) != 0 {
                hi_reg_ops(cpu, inst);
            } else {
                alu(cpu, inst);
            }
        }
        9 => ldr_pc_rel(cpu, bus, inst),
        10 => ldr_str_reg(cpu, bus, inst),
        11 => ldrh_signed_reg(cpu, bus, inst),
        12 | 13 => ldr_str_word_imm(cpu, bus, inst),
        14 | 15 => ldr_str_byte_imm(cpu, bus, inst),
        16 | 17 => ldr_str_half_imm(cpu, bus, inst),
        18 => ldr_str_sp_rel(cpu, bus, inst),
        20 | 21 => add_pc_or_sp(cpu, inst),
        22 => {
            if inst & (1 << 10) != 0 {
                push_pop(cpu, bus, inst);
            } else {
                add_sub_sp(cpu, inst);
            }
        }
        24 => ldm_stm(cpu, bus, inst),
        26 | 27 => {
            if inst & 0xFF00 == 0xDF00 {
                cpu.swi(cpu.pc);
                cpu.add_cycles(3);
            } else {
                branch_cond(cpu, inst);
            }
        }
        28 => branch_uncond(cpu, inst),
        30 => bl_upper(cpu, inst),
        31 => bl_lower(cpu, inst),
        _ => {
            cpu.add_cycles(1);
        }
    }
}

fn shift_imm(cpu: &mut Cpu, inst: u32) {
    let stype = (inst >> 11) & 3; // 0=LSL, 1=LSR, 2=ASR
    let imm5 = (inst >> 6) & 0x1F;
    let rs = (inst >> 3) & 7;
    let rd = inst & 7;
    let operand = rn(cpu, rs);
    let carry_in = cpu.cpsr & flag::C != 0;
    let (v, c) = shift_reg(operand, stype, imm5, carry_in);
    cpu.set_reg(rd, v);
    set_flags(cpu, v, c, cpu.cpsr & flag::V != 0);
    cpu.add_cycles(1);
}

fn add_sub(cpu: &mut Cpu, inst: u32) {
    let immediate = inst & (1 << 10) != 0;
    let op = inst & (1 << 9) != 0; // 0=ADD, 1=SUB
    let rn_s = (inst >> 3) & 7;
    let rd = inst & 7;
    if immediate {
        let imm3 = (inst >> 6) & 7;
        let a = rn(cpu, rn_s);
        let (r, c, v) = if op { sub(a, imm3) } else { add(a, imm3) };
        cpu.set_reg(rd, r);
        set_flags(cpu, r, c, v);
    } else {
        let rs = (inst >> 3) & 7;
        let rn_field = (inst >> 6) & 7;
        let a = rn(cpu, rn_field);
        let b = rn(cpu, rs);
        let (r, c, v) = if op { sub(a, b) } else { add(a, b) };
        cpu.set_reg(rd, r);
        set_flags(cpu, r, c, v);
    }
    cpu.add_cycles(1);
}

fn mov_cmp_add_sub_imm(cpu: &mut Cpu, inst: u32) {
    let op = (inst >> 11) & 3; // 0=MOV, 1=CMP, 2=ADD, 3=SUB
    let rd = (inst >> 8) & 7;
    let imm8 = inst & 0xFF;
    let a = rn(cpu, rd);
    let (r, c, v) = match op {
        0 => (imm8, cpu.cpsr & flag::C != 0, cpu.cpsr & flag::V != 0),
        1 => {
            let (r, c, v) = sub(a, imm8);
            (r, c, v)
        }
        2 => add(a, imm8),
        _ => sub(a, imm8),
    };
    if op != 1 {
        cpu.set_reg(rd, r);
    }
    set_flags(cpu, r, c, v);
    cpu.add_cycles(1);
}

fn alu(cpu: &mut Cpu, inst: u32) {
    let op = (inst >> 6) & 0xF;
    let rs = (inst >> 3) & 7;
    let rd = inst & 7;
    let a = rn(cpu, rd);
    let b = rn(cpu, rs);
    let carry_in = cpu.cpsr & flag::C != 0;
    let (r, c, v) = match op {
        0 => (a & b, cpu.cpsr & flag::C != 0, cpu.cpsr & flag::V != 0),          // AND
        1 => (a ^ b, cpu.cpsr & flag::C != 0, cpu.cpsr & flag::V != 0),          // EOR
        2 => {
            let (v, c) = shift_reg(a, 0, b & 0xFF, carry_in);
            (v, c, cpu.cpsr & flag::V != 0)                                     // LSL
        }
        3 => {
            let (v, c) = shift_reg(a, 1, b & 0xFF, carry_in);
            (v, c, cpu.cpsr & flag::V != 0)                                     // LSR
        }
        4 => {
            let (v, c) = shift_reg(a, 2, b & 0xFF, carry_in);
            (v, c, cpu.cpsr & flag::V != 0)                                     // ASR
        }
        5 => {
            let (t, c1, v1) = add(a, b);
            let (t2, c2, v2) = add(t, if carry_in { 1 } else { 0 });
            (t2, c1 || c2, v1 || v2)                                           // ADC
        }
        6 => {
            let (t, c1, v1) = sub(a, b);
            let (t2, c2, v2) = sub(t, if carry_in { 0 } else { 1 });
            (t2, c1 || c2, v1 || v2)                                           // SBC
        }
        7 => {
            let (v, c) = shift_reg(a, 3, b & 0xFF, carry_in);
            (v, c, cpu.cpsr & flag::V != 0)                                     // ROR
        }
        8 => (a & b, cpu.cpsr & flag::C != 0, cpu.cpsr & flag::V != 0),         // TST
        9 => sub(0, b),                                                          // NEG
        10 => sub(a, b),                                                         // CMP
        11 => add(a, b),                                                         // CMN
        12 => (a | b, cpu.cpsr & flag::C != 0, cpu.cpsr & flag::V != 0),         // ORR
        13 => (a.wrapping_mul(b), cpu.cpsr & flag::C != 0, cpu.cpsr & flag::V != 0), // MUL
        14 => (a & !b, cpu.cpsr & flag::C != 0, cpu.cpsr & flag::V != 0),        // BIC
        _ => (!b, cpu.cpsr & flag::C != 0, cpu.cpsr & flag::V != 0),             // MVN
    };
    let test = op == 8 || op == 10 || op == 11;
    if !test {
        cpu.set_reg(rd, r);
    }
    set_flags(cpu, r, c, v);
    cpu.add_cycles(1);
}

fn hi_reg_ops(cpu: &mut Cpu, inst: u32) {
    let op = (inst >> 8) & 3; // 0=ADD, 1=CMP, 2=MOV, 3=BX
    let h1 = (inst >> 7) & 1 != 0;
    let h2 = (inst >> 6) & 1 != 0;
    let rs = (inst >> 3) & 7;
    let rd = inst & 7;
    let high = h1 || h2;
    if op == 3 {
        // BX
        let rm = rs | if h2 { 8 } else { 0 };
        let v = rn(cpu, rm);
        cpu.bx(v);
        cpu.add_cycles(3);
        return;
    }
    let src = rs | if h2 { 8 } else { 0 };
    let dst = rd | if h1 { 8 } else { 0 };
    let a = rn(cpu, dst);
    let b = rn(cpu, src);
    match op {
        0 => {
            let (r, c, v) = add(a, b);
            cpu.set_reg(dst, r);
            if !high {
                set_flags(cpu, r, c, v);
            }
        }
        1 => {
            let (r, c, v) = sub(a, b);
            set_flags(cpu, r, c, v);
        }
        _ => {
            cpu.set_reg(dst, b);
            if !high {
                set_flags(cpu, b, cpu.cpsr & flag::C != 0, cpu.cpsr & flag::V != 0);
            }
        }
    }
    cpu.add_cycles(1);
}

fn ldr_pc_rel(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    let rd = (inst >> 8) & 7;
    let imm8 = inst & 0xFF;
    let addr = ((cpu.pc + 2) & !3) + (imm8 << 2);
    let v = bus.read32(addr);
    cpu.set_reg(rd, v);
    cpu.add_cycles(1);
}

fn ldr_str_reg(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    let i = inst & (1 << 10) != 0; // 0=word, 1=byte
    let l = inst & (1 << 9) != 0;
    let rb = (inst >> 6) & 7;
    let ro = (inst >> 3) & 7;
    let rd = inst & 7;
    let addr = rn(cpu, rb).wrapping_add(rn(cpu, ro));
    if l {
        let v = if i {
            bus.read8(addr)
        } else {
            bus.read32(addr)
        };
        cpu.set_reg(rd, v);
    } else {
        let v = rn(cpu, rd);
        if i {
            bus.write8(addr, v);
        } else {
            bus.write32(addr, v);
        }
    }
    cpu.add_cycles(1);
}

fn ldrh_signed_reg(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    let rb = (inst >> 6) & 7;
    let ro = (inst >> 3) & 7;
    let rd = inst & 7;
    let addr = rn(cpu, rb).wrapping_add(rn(cpu, ro));
    if inst & (1 << 10) != 0 {
        // Sign-extended.
        let l = inst & (1 << 9) != 0;
        let v = if l {
            (bus.read16(addr) as i16) as u32
        } else {
            (bus.read8(addr) as i8) as u32
        };
        cpu.set_reg(rd, v);
    } else {
        let l = inst & (1 << 9) != 0;
        if l {
            cpu.set_reg(rd, bus.read16(addr));
        } else {
            bus.write16(addr, rn(cpu, rd));
        }
    }
    cpu.add_cycles(1);
}

fn ldr_str_word_imm(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    let l = inst & (1 << 11) != 0;
    let imm5 = (inst >> 6) & 0x1F;
    let rb = (inst >> 3) & 7;
    let rd = inst & 7;
    let addr = rn(cpu, rb).wrapping_add(imm5 << 2);
    transfer_imm(cpu, bus, l, addr, rd, 4);
}

fn ldr_str_byte_imm(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    let l = inst & (1 << 11) != 0;
    let imm5 = (inst >> 6) & 0x1F;
    let rb = (inst >> 3) & 7;
    let rd = inst & 7;
    let addr = rn(cpu, rb).wrapping_add(imm5);
    transfer_imm(cpu, bus, l, addr, rd, 1);
}

fn ldr_str_half_imm(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    let l = inst & (1 << 11) != 0;
    let imm5 = (inst >> 6) & 0x1F;
    let rb = (inst >> 3) & 7;
    let rd = inst & 7;
    let addr = rn(cpu, rb).wrapping_add(imm5 << 1);
    transfer_imm(cpu, bus, l, addr, rd, 2);
}

#[inline]
fn transfer_imm(cpu: &mut Cpu, bus: &mut dyn Bus, load: bool, addr: u32, rd: u32, width: u32) {
    if load {
        let v = match width {
            4 => bus.read32(addr),
            2 => bus.read16(addr),
            _ => bus.read8(addr),
        };
        cpu.set_reg(rd, v);
    } else {
        let v = rn(cpu, rd);
        match width {
            4 => bus.write32(addr, v),
            2 => bus.write16(addr, v),
            _ => bus.write8(addr, v),
        }
    }
    cpu.add_cycles(1);
}

fn ldr_str_sp_rel(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    let l = inst & (1 << 11) != 0;
    let rd = (inst >> 8) & 7;
    let imm8 = inst & 0xFF;
    let addr = rn(cpu, 13).wrapping_add(imm8 << 2);
    transfer_imm(cpu, bus, l, addr, rd, 4);
}

fn add_pc_or_sp(cpu: &mut Cpu, inst: u32) {
    let rd = (inst >> 8) & 7;
    let imm8 = inst & 0xFF;
    let base = if inst & (1 << 11) != 0 {
        rn(cpu, 13)
    } else {
        (cpu.pc + 2) & !3
    };
    cpu.set_reg(rd, base.wrapping_add(imm8 << 2));
    cpu.add_cycles(1);
}

fn add_sub_sp(cpu: &mut Cpu, inst: u32) {
    let s = inst & (1 << 7) != 0;
    let imm7 = inst & 0x7F;
    let delta = imm7 << 2;
    let sp = rn(cpu, 13);
    cpu.set_reg(13, if s { sp.wrapping_sub(delta) } else { sp.wrapping_add(delta) });
    cpu.add_cycles(1);
}

fn push_pop(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    let l = inst & (1 << 11) != 0;
    let rlist = inst & 0xFF;
    let has_lr = inst & (1 << 8) != 0;
    let count = rlist.count_ones() + if has_lr { 1 } else { 0 };
    let sp = rn(cpu, 13);
    if !l {
        // PUSH (STMDB, descending, with LR).
        let mut addr = sp.wrapping_sub(4);
        for r in 0..8u32 {
            if rlist & (1 << r) != 0 {
                bus.write32(addr, rn(cpu, r));
                addr = addr.wrapping_sub(4);
            }
        }
        if has_lr {
            bus.write32(addr, cpu.reg(14));
        }
        cpu.set_reg(13, sp.wrapping_sub(4 * count));
    } else {
        // POP (LDMIA, ascending, with PC).
        let mut addr = sp;
        let mut pc_val: Option<u32> = None;
        for r in 0..8u32 {
            if rlist & (1 << r) != 0 {
                let v = bus.read32(addr);
                addr = addr.wrapping_add(4);
                cpu.set_reg(r, v);
            }
        }
        if has_lr {
            pc_val = Some(bus.read32(addr));
        }
        cpu.set_reg(13, sp.wrapping_add(4 * count));
        if let Some(v) = pc_val {
            cpu.branch(v);
        }
    }
    cpu.add_cycles(1);
}

fn ldm_stm(cpu: &mut Cpu, bus: &mut dyn Bus, inst: u32) {
    let l = inst & (1 << 11) != 0;
    let rb = (inst >> 8) & 7;
    let rlist = inst & 0xFF;
    let base = rn(cpu, rb);
    let count = rlist.count_ones();
    let mut addr = base;
    if l {
        for r in 0..8u32 {
            if rlist & (1 << r) != 0 {
                let v = bus.read32(addr);
                addr = addr.wrapping_add(4);
                cpu.set_reg(r, v);
            }
        }
    } else {
        for r in 0..8u32 {
            if rlist & (1 << r) != 0 {
                bus.write32(addr, rn(cpu, r));
                addr = addr.wrapping_add(4);
            }
        }
    }
    // Writeback unless rn is in the register list for LDM.
    if !(l && rlist & (1 << rb) != 0) {
        cpu.set_reg(rb, base.wrapping_add(4 * count));
    }
    cpu.add_cycles(1);
}

fn branch_cond(cpu: &mut Cpu, inst: u32) {
    let cond = (inst >> 8) & 0xF;
    if cond_holds(cpu, cond) {
        let imm8 = inst & 0xFF;
        let off = (imm8 as i8 as i32) << 1;
        let target = (cpu.reg(15) as i32 + off) as u32;
        cpu.branch(target);
        cpu.add_cycles(2);
    } else {
        cpu.add_cycles(1);
    }
}

fn branch_uncond(cpu: &mut Cpu, inst: u32) {
    let imm11 = inst & 0x7FF;
    let off = if imm11 & 0x400 != 0 {
        ((imm11 | 0xF800) as i16 as i32) << 1
    } else {
        (imm11 as i32) << 1
    };
    let target = (cpu.reg(15) as i32 + off) as u32;
    cpu.branch(target);
    cpu.add_cycles(2);
}

fn bl_upper(cpu: &mut Cpu, inst: u32) {
    // LR = PC + 4 + (offset_top << 12)
    let off = inst & 0x7FF;
    let lr = cpu.pc.wrapping_add(2).wrapping_add(off << 12);
    cpu.set_reg(14, lr);
    cpu.add_cycles(1);
}

fn bl_lower(cpu: &mut Cpu, inst: u32) {
    let off = inst & 0x7FF;
    let target = cpu.reg(14).wrapping_add(off << 1);
    // Return address = address after the first half (self.pc), with bit 0 set.
    cpu.set_reg(14, cpu.pc | 1);
    cpu.branch(target);
    cpu.add_cycles(1);
}

fn cond_holds(cpu: &Cpu, cond: u32) -> bool {
    let n = cpu.cpsr & flag::N != 0;
    let z = cpu.cpsr & flag::Z != 0;
    let c = cpu.cpsr & flag::C != 0;
    let v = cpu.cpsr & flag::V != 0;
    match cond {
        0x0 => z,
        0x1 => !z,
        0x2 => c,
        0x3 => !c,
        0x4 => n,
        0x5 => !n,
        0x6 => v,
        0x7 => !v,
        0x8 => c && !z,
        0x9 => !c || z,
        0xA => n == v,
        0xB => n != v,
        0xC => !z && n == v,
        0xD => z || n != v,
        _ => true,
    }
}