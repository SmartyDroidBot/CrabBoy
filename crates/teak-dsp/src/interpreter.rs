//! The instruction set: what each instruction does to the registers and
//! memory, and the loop that fetches, repeats and takes interrupts
//! (Teakra's `interpreter.h`).
//!
//! Accumulators hold 40-bit values sign-extended into 64 bits. Arithmetic
//! wraps as the C++ original's unsigned arithmetic does. Behaviour Teakra
//! leaves unimplemented, or asserts never happens, stops the core with a
//! fault instead.

use crate::decoder;
use crate::mmio::Io;
use crate::operand::*;
use crate::register::Regs;
use crate::Bus;

/// How a second address is found from the first in two-word accesses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Offset {
    Zero,
    PlusOne,
    MinusOne,
    MinusOneDmod,
}

fn offset_of(v: u16) -> Offset {
    match v & 3 {
        0 => Offset::Zero,
        1 => Offset::PlusOne,
        2 => Offset::MinusOne,
        _ => Offset::MinusOneDmod,
    }
}

fn step_of(v: u16) -> StepValue {
    match v & 7 {
        0 => StepValue::Zero,
        1 => StepValue::Increase,
        2 => StepValue::Decrease,
        3 => StepValue::PlusStep,
        4 => StepValue::Increase2Mode1,
        5 => StepValue::Decrease2Mode1,
        6 => StepValue::Increase2Mode2,
        _ => StepValue::Decrease2Mode2,
    }
}

/// The accumulator a register name belongs to: a 0, a 1, b 2, b 3.
fn acc_index(name: RegName) -> Option<usize> {
    use RegName::*;
    Some(match name {
        A0 | A0l | A0h | A0e => 0,
        A1 | A1l | A1h | A1e => 1,
        B0 | B0l | B0h | B0e => 2,
        B1 | B1l | B1h | B1e => 3,
        _ => return None,
    })
}

/// The other accumulator of the same pair, in the same part.
fn counter_acc(name: RegName) -> RegName {
    use RegName::*;
    match name {
        A0 => A1,
        A1 => A0,
        B0 => B1,
        B1 => B0,
        A0l => A1l,
        A1l => A0l,
        B0l => B1l,
        B1l => B0l,
        A0h => A1h,
        A1h => A0h,
        B0h => B1h,
        B1h => B0h,
        A0e => A1e,
        A1e => A0e,
        B0e => B1e,
        B1e => B0e,
        other => other,
    }
}

/// The bit length of `x`: C++20's `log2p1`.
fn log2p1(x: u16) -> u32 {
    16 - x.leading_zeros()
}

/// The core while it runs: registers, the memory it sees, and its
/// peripherals.
pub struct Interp<'a, B: Bus + ?Sized> {
    pub regs: &'a mut Regs,
    pub io: &'a mut Io,
    pub bus: &'a mut B,
    pub fault: &'a mut Option<String>,
    idle: bool,
}

impl<'a, B: Bus + ?Sized> Interp<'a, B> {
    pub fn new(
        regs: &'a mut Regs,
        io: &'a mut Io,
        bus: &'a mut B,
        fault: &'a mut Option<String>,
    ) -> Self {
        Interp {
            regs,
            io,
            bus,
            fault,
            idle: false,
        }
    }

    /// Stop the core: something Teakra does not model happened.
    fn stop(&mut self, why: impl Into<String>) {
        if self.fault.is_none() {
            *self.fault = Some(format!("{} at pc {:#07x}", why.into(), self.regs.pc));
        }
    }

    // ---- memory ----------------------------------------------------------

    pub fn program_read(&mut self, addr: u32) -> u16 {
        let ram = self.bus.ram();
        let at = (addr as usize * 2) & (ram.len() - 1);
        u16::from_le_bytes([ram[at], ram[at + 1]])
    }

    pub fn program_write(&mut self, addr: u32, value: u16) {
        let ram = self.bus.ram();
        let at = (addr as usize * 2) & (ram.len() - 1);
        ram[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    pub fn data_read(&mut self, addr: u16) -> u16 {
        if self.io.miu.in_mmio(addr) {
            let reg = self.io.miu.to_mmio(addr);
            return self.io.read(reg);
        }
        let word = self.io.miu.data_word(addr);
        self.program_read(word)
    }

    pub fn data_write(&mut self, addr: u16, value: u16) {
        if self.io.miu.in_mmio(addr) {
            let reg = self.io.miu.to_mmio(addr);
            self.io.write(self.bus, reg, value);
            return;
        }
        let word = self.io.miu.data_word(addr);
        self.program_write(word, value);
    }

    fn push(&mut self, value: u16) {
        self.regs.sp = self.regs.sp.wrapping_sub(1);
        self.data_write(self.regs.sp, value);
    }

    fn pop(&mut self) -> u16 {
        let value = self.data_read(self.regs.sp);
        self.regs.sp = self.regs.sp.wrapping_add(1);
        value
    }

    fn push_pc(&mut self) {
        let (l, h) = (self.regs.pc as u16, (self.regs.pc >> 16) as u16);
        if self.regs.cpc == 1 {
            self.push(h);
            self.push(l);
        } else {
            self.push(l);
            self.push(h);
        }
    }

    fn pop_pc(&mut self) {
        let (h, l) = if self.regs.cpc == 1 {
            let l = self.pop();
            (self.pop(), l)
        } else {
            let h = self.pop();
            (h, self.pop())
        };
        self.set_pc(l as u32 | (h as u32) << 16);
    }

    fn set_pc(&mut self, pc: u32) {
        if pc >= 0x4_0000 {
            self.stop(format!("jump to {pc:#x}"));
            return;
        }
        self.regs.pc = pc;
    }

    // ---- the loop --------------------------------------------------------

    /// Run up to `cycles` cycles, one instruction each. Stops early, after
    /// the instruction, when the ARM side has something to hear or the core
    /// faults. Returns the cycles that passed.
    pub fn run(&mut self, cycles: u64) -> u64 {
        let table = decoder::table();
        self.idle = false;
        let mut i = 0;
        while i < cycles {
            if self.idle {
                i += self.io.skip(cycles - i - 1);
                // One more tick, so that whatever ends the wait fires.
                if i < cycles - 1 {
                    i += 1;
                    self.io.tick();
                }
            }

            for n in 0..3 {
                if std::mem::take(&mut self.io.pending.int[n]) {
                    self.regs.ip[n] = 1;
                }
            }
            if std::mem::take(&mut self.io.pending.vectored) {
                self.regs.ipv = 1;
            }

            let fetch = self.regs.pc | (self.regs.prpage as u32) << 18;
            self.regs.pc = self.regs.pc.wrapping_add(1);
            let opcode = self.program_read(fetch);
            let entry = table.entries[opcode as usize];
            let mut expansion = 0;
            if table.expanded[opcode as usize] {
                let fetch = self.regs.pc | (self.regs.prpage as u32) << 18;
                self.regs.pc = self.regs.pc.wrapping_add(1);
                expansion = self.program_read(fetch);
            }

            if self.regs.rep {
                if self.regs.repc == 0 {
                    self.regs.rep = false;
                } else {
                    self.regs.repc -= 1;
                    self.regs.pc = self.regs.pc.wrapping_sub(1);
                }
            }

            if self.regs.lp != 0 && self.regs.bcn > 0 {
                let level = self.regs.bcn as usize - 1;
                let frame = self.regs.bkrep_stack[level];
                if frame.end.wrapping_add(1) == self.regs.pc {
                    if frame.lc == 0 {
                        self.regs.bcn -= 1;
                        self.regs.lp = (self.regs.bcn != 0) as u16;
                    } else {
                        self.regs.bkrep_stack[level].lc -= 1;
                        self.regs.pc = frame.start;
                    }
                }
            }

            decoder::dispatch(self, entry, opcode, expansion);

            // Whether a single-instruction repeat can be interrupted is not
            // known; it is not.
            if self.regs.ie != 0 && !self.regs.rep {
                let taken = (0..3).find(|&n| self.regs.im[n] != 0 && self.regs.ip[n] != 0);
                if let Some(n) = taken {
                    self.regs.ip[n] = 0;
                    self.regs.ie = 0;
                    self.push_pc();
                    self.regs.pc = 0x0006 + n as u32 * 8;
                    self.idle = false;
                    if self.regs.ic[n] != 0 {
                        self.context_store();
                    }
                } else if self.regs.imv != 0 && self.regs.ipv != 0 {
                    self.regs.ipv = 0;
                    self.regs.ie = 0;
                    self.push_pc();
                    self.regs.pc = self.io.pending.vector;
                    self.idle = false;
                    if self.io.pending.context_switch {
                        self.context_store();
                    }
                }
            }

            self.io.tick();
            i += 1;
            if self.fault.is_some() || !self.io.events.is_empty() {
                break;
            }
        }
        i.min(cycles)
    }

    pub fn undefined(&mut self, opcode: u16) {
        self.stop(format!("undefined opcode {opcode:#06x}"));
    }

    // ---- accumulators and the buses --------------------------------------

    fn get_acc(&self, name: RegName) -> u64 {
        match acc_index(name) {
            Some(0) => self.regs.a[0],
            Some(1) => self.regs.a[1],
            Some(2) => self.regs.b[0],
            Some(3) => self.regs.b[1],
            _ => 0,
        }
    }

    fn set_acc(&mut self, name: RegName, value: u64) {
        match acc_index(name) {
            Some(0) => self.regs.a[0] = value,
            Some(1) => self.regs.a[1] = value,
            Some(2) => self.regs.b[0] = value,
            Some(3) => self.regs.b[1] = value,
            _ => {}
        }
    }

    fn saturate_acc_no_flag(value: u64) -> u64 {
        if value != sext64(value, 32) {
            if value >> 39 != 0 {
                0xFFFF_FFFF_8000_0000
            } else {
                0x7FFF_FFFF
            }
        } else {
            value
        }
    }

    fn saturate_acc(&mut self, value: u64) -> u64 {
        if value != sext64(value, 32) {
            self.regs.flm = 1;
        }
        Self::saturate_acc_no_flag(value)
    }

    fn get_and_sat_acc(&mut self, name: RegName) -> u64 {
        let value = self.get_acc(name);
        if self.regs.sat == 0 {
            self.saturate_acc(value)
        } else {
            value
        }
    }

    fn get_and_sat_acc_no_flag(&self, name: RegName) -> u64 {
        let value = self.get_acc(name);
        if self.regs.sat == 0 {
            Self::saturate_acc_no_flag(value)
        } else {
            value
        }
    }

    fn set_acc_flag(&mut self, value: u64) {
        let r = &mut *self.regs;
        r.fz = (value == 0) as u16;
        r.fm = (value >> 39 != 0) as u16;
        r.fe = (value != sext64(value, 32)) as u16;
        let bit31 = value >> 31 & 1;
        let bit30 = value >> 30 & 1;
        r.fn_ = (r.fz != 0 || (r.fe == 0 && bit31 ^ bit30 != 0)) as u16;
    }

    fn sat_and_set_acc_and_flag(&mut self, name: RegName, value: u64) {
        self.set_acc_flag(value);
        let value = if self.regs.sata == 0 {
            self.saturate_acc(value)
        } else {
            value
        };
        self.set_acc(name, value);
    }

    fn set_acc_and_flag(&mut self, name: RegName, value: u64) {
        self.set_acc_flag(value);
        self.set_acc(name, value);
    }

    /// A register as the 16-bit bus sees it. `sat` saturates the low and
    /// high parts of accumulators, as moves do.
    fn reg_to_bus16(&mut self, reg: RegName, sat: bool) -> u16 {
        use RegName::*;
        let r = &*self.regs;
        match reg {
            // The low part; unlike aXl, never saturated.
            A0 | A1 | B0 | B1 => self.get_acc(reg) as u16,
            A0l | A1l | B0l | B1l => {
                if sat {
                    self.get_and_sat_acc(reg) as u16
                } else {
                    self.get_acc(reg) as u16
                }
            }
            A0h | A1h | B0h | B1h => {
                if sat {
                    (self.get_and_sat_acc(reg) >> 16) as u16
                } else {
                    (self.get_acc(reg) >> 16) as u16
                }
            }
            R0 => r.r[0],
            R1 => r.r[1],
            R2 => r.r[2],
            R3 => r.r[3],
            R4 => r.r[4],
            R5 => r.r[5],
            R6 => r.r[6],
            R7 => r.r[7],
            Y0 => r.y[0],
            P => (self.product_to_bus40(0) >> 16) as u16,
            Sp => r.sp,
            Sv => r.sv,
            Lc => r.lc(),
            Ar0 => r.ar(0),
            Ar1 => r.ar(1),
            Arp0 => r.arp(0),
            Arp1 => r.arp(1),
            Arp2 => r.arp(2),
            Arp3 => r.arp(3),
            Ext0 => r.ext[0],
            Ext1 => r.ext[1],
            Ext2 => r.ext[2],
            Ext3 => r.ext[3],
            Stt0 => r.stt0(),
            Stt1 => r.stt1(),
            Stt2 => r.stt2(),
            St0 => r.st0(),
            St1 => r.st1(),
            St2 => r.st2(),
            Cfgi => r.cfgi(),
            Cfgj => r.cfgj(),
            Mod0 => r.mod0(),
            Mod1 => r.mod1(),
            Mod2 => r.mod2(),
            Mod3 => r.mod3(),
            A0e | A1e | B0e | B1e | Pc | Undefined => {
                self.stop(format!("read of {reg:?} from the bus"));
                0
            }
        }
    }

    fn reg_from_bus16(&mut self, reg: RegName, value: u16) {
        use RegName::*;
        match reg {
            A0 | A1 | B0 | B1 => self.sat_and_set_acc_and_flag(reg, sext64(value as u64, 16)),
            A0l | A1l | B0l | B1l => self.sat_and_set_acc_and_flag(reg, value as u64),
            A0h | A1h | B0h | B1h => {
                self.sat_and_set_acc_and_flag(reg, sext64((value as u64) << 16, 32))
            }
            R0 => self.regs.r[0] = value,
            R1 => self.regs.r[1] = value,
            R2 => self.regs.r[2] = value,
            R3 => self.regs.r[3] = value,
            R4 => self.regs.r[4] = value,
            R5 => self.regs.r[5] = value,
            R6 => self.regs.r[6] = value,
            R7 => self.regs.r[7] = value,
            Y0 => self.regs.y[0] = value,
            // p0h.
            P => {
                self.regs.pe[0] = (value > 0x7FFF) as u16;
                self.regs.p[0] = self.regs.p[0] & 0xFFFF | (value as u32) << 16;
            }
            Sp => self.regs.sp = value,
            Sv => self.regs.sv = value,
            Lc => *self.regs.lc_mut() = value,
            Ar0 => self.regs.set_ar(0, value),
            Ar1 => self.regs.set_ar(1, value),
            Arp0 => self.regs.set_arp(0, value),
            Arp1 => self.regs.set_arp(1, value),
            Arp2 => self.regs.set_arp(2, value),
            Arp3 => self.regs.set_arp(3, value),
            Ext0 => self.regs.ext[0] = value,
            Ext1 => self.regs.ext[1] = value,
            Ext2 => self.regs.ext[2] = value,
            Ext3 => self.regs.ext[3] = value,
            Stt0 => self.regs.set_stt0(value),
            Stt1 => self.regs.set_stt1(value),
            Stt2 => self.regs.set_stt2(value),
            St0 => self.regs.set_st0(value),
            St1 => self.regs.set_st1(value),
            St2 => self.regs.set_st2(value),
            Cfgi => self.regs.set_cfgi(value),
            Cfgj => self.regs.set_cfgj(value),
            Mod0 => self.regs.set_mod0(value),
            Mod1 => self.regs.set_mod1(value),
            Mod2 => self.regs.set_mod2(value),
            Mod3 => self.regs.set_mod3(value),
            A0e | A1e | B0e | B1e | Pc | Undefined => {
                self.stop(format!("write of {reg:?} from the bus"))
            }
        }
    }

    // ---- products --------------------------------------------------------

    fn product_to_bus40(&self, unit: usize) -> u64 {
        let value = self.regs.p[unit] as u64 | (self.regs.pe[unit] as u64) << 32;
        match self.regs.ps[unit] & 3 {
            0 => sext64(value, 33),
            1 => sext64(value >> 1, 32),
            2 => sext64(value << 1, 34),
            _ => sext64(value << 2, 35),
        }
    }

    fn product_from_bus32(&mut self, unit: usize, value: u32) {
        self.regs.p[unit] = value;
        self.regs.pe[unit] = (value >> 31) as u16;
    }

    fn do_multiplication(&mut self, unit: usize, x_sign: bool, y_sign: bool) {
        let mut x = self.regs.x[unit] as u32;
        let mut y = self.regs.y[unit] as u32;
        let hwm = self.regs.hwm;
        if hwm == 1 || (hwm == 3 && unit == 0) {
            y >>= 8;
        } else if hwm == 2 || (hwm == 3 && unit == 1) {
            y &= 0xFF;
        }
        if x_sign {
            x = sext32(x, 16);
        }
        if y_sign {
            y = sext32(y, 16);
        }
        self.regs.p[unit] = x.wrapping_mul(y);
        self.regs.pe[unit] = if x_sign || y_sign {
            (self.regs.p[unit] >> 31) as u16
        } else {
            0
        };
    }

    fn add_sub40(&mut self, a: u64, b: u64, sub: bool) -> u64 {
        let a = a & 0xFF_FFFF_FFFF;
        let mut b = b & 0xFF_FFFF_FFFF;
        let result = if sub {
            a.wrapping_sub(b)
        } else {
            a.wrapping_add(b)
        };
        self.regs.fc0 = (result >> 40 & 1) as u16;
        if sub {
            b = !b;
        }
        self.regs.fv = ((!(a ^ b) & (a ^ result)) >> 39 & 1) as u16;
        if self.regs.fv != 0 {
            self.regs.fvl = 1;
        }
        sext64(result, 40)
    }

    fn product_sum(
        &mut self,
        base: SumBase,
        acc: RegName,
        sub_p0: bool,
        p0_align: bool,
        sub_p1: bool,
        p1_align: bool,
    ) {
        let mut value_a = self.product_to_bus40(0);
        let mut value_b = self.product_to_bus40(1);
        if p0_align {
            value_a = sext64(value_a >> 16, 24);
        }
        if p1_align {
            value_b = sext64(value_b >> 16, 24);
        }
        let value_c = match base {
            SumBase::Zero => 0,
            SumBase::Acc => self.get_acc(acc),
            SumBase::Sv => sext64((self.regs.sv as u64) << 16, 32),
            SumBase::SvRnd => sext64((self.regs.sv as u64) << 16, 32) | 0x8000,
        };
        let result = self.add_sub40(value_c, value_a, sub_p0);
        let (temp_c, temp_v) = (self.regs.fc0, self.regs.fv);
        let result = self.add_sub40(result, value_b, sub_p1);
        if sub_p0 == sub_p1 {
            self.regs.fc0 |= temp_c;
            self.regs.fv |= temp_v;
        } else {
            self.regs.fc0 ^= temp_c;
            self.regs.fv ^= temp_v;
        }
        self.sat_and_set_acc_and_flag(acc, result);
    }

    // ---- address registers -----------------------------------------------

    fn ar_rn_unit(&self, index: u16) -> usize {
        self.regs.arrn[index as usize & 3] as usize & 7
    }

    fn arp_rn_unit(&self, index: u16) -> (usize, usize) {
        let i = index as usize & 3;
        (
            self.regs.arprni[i] as usize & 3,
            (self.regs.arprnj[i] as usize & 3) + 4,
        )
    }

    fn ar_step(&self, index: u16) -> StepValue {
        step_of(self.regs.arstep[index as usize & 3])
    }

    fn arp_step(&self, i: u16, j: u16) -> (StepValue, StepValue) {
        (
            step_of(self.regs.arpstepi[i as usize & 3]),
            step_of(self.regs.arpstepj[j as usize & 3]),
        )
    }

    fn ar_offset(&self, index: u16) -> Offset {
        offset_of(self.regs.aroffset[index as usize & 3])
    }

    fn arp_offset(&self, i: u16, j: u16) -> (Offset, Offset) {
        (
            offset_of(self.regs.arpoffseti[i as usize & 3]),
            offset_of(self.regs.arpoffsetj[j as usize & 3]),
        )
    }

    fn rn_address(&self, unit: usize, value: u16) -> u16 {
        if self.regs.br[unit] != 0 && self.regs.m[unit] == 0 {
            bit_reverse(value)
        } else {
            value
        }
    }

    fn rn_address_and_modify(&mut self, unit: usize, step: StepValue, dmod: bool) -> u16 {
        let value = self.rn_and_modify(unit, step, dmod);
        self.rn_address(unit, value)
    }

    fn offset_address(&mut self, unit: usize, address: u16, offset: Offset, dmod: bool) -> u16 {
        match offset {
            Offset::Zero => return address,
            Offset::MinusOneDmod => return address.wrapping_sub(1),
            _ => {}
        }
        let emod = self.regs.m[unit] != 0 && self.regs.br[unit] == 0 && !dmod;
        let modulo = if unit < 4 {
            self.regs.modi
        } else {
            self.regs.modj
        };
        // Modulo zero still has a one-bit mask.
        let mut mask = 1;
        for i in 0..9 {
            mask |= modulo >> i;
        }
        if offset == Offset::PlusOne {
            if !emod {
                return address.wrapping_add(1);
            }
            if address & mask == modulo {
                return address & !mask;
            }
            address.wrapping_add(1)
        } else {
            if !emod {
                return address.wrapping_sub(1);
            }
            self.stop("modulo offset -1");
            address
        }
    }

    fn step_address(&self, unit: usize, address: u16, step: StepValue, dmod: bool) -> u16 {
        let r = &*self.regs;
        let legacy = r.cmd != 0;
        let mut step2_mode1 = false;
        let mut step2_mode2 = false;
        let mut s: u16 = match step {
            StepValue::Zero => 0,
            StepValue::Increase => 1,
            StepValue::Decrease => 0xFFFF,
            StepValue::Increase2Mode1 => {
                step2_mode1 = !legacy;
                2
            }
            StepValue::Decrease2Mode1 => {
                step2_mode1 = !legacy;
                0xFFFE
            }
            StepValue::Increase2Mode2 => {
                step2_mode2 = !legacy;
                2
            }
            StepValue::Decrease2Mode2 => {
                step2_mode2 = !legacy;
                0xFFFE
            }
            StepValue::PlusStep => {
                let mut s = if r.br[unit] != 0 && r.m[unit] == 0 {
                    if unit < 4 {
                        r.stepi0
                    } else {
                        r.stepj0
                    }
                } else {
                    sext16(if unit < 4 { r.stepi } else { r.stepj }, 7)
                };
                if r.stp16 == 1 && !legacy {
                    s = if unit < 4 { r.stepi0 } else { r.stepj0 };
                    if r.m[unit] != 0 {
                        s = sext16(s, 9);
                    }
                }
                s
            }
        };

        if s == 0 {
            return address;
        }
        if dmod || r.br[unit] != 0 || r.m[unit] == 0 {
            return address.wrapping_add(s);
        }

        let modulo = if unit < 4 { r.modi } else { r.modj };
        if modulo == 0 || (modulo == 1 && step2_mode2) {
            return address;
        }
        let mut iterations = 1;
        if step2_mode1 {
            iterations = 2;
            s = sext16(s >> 1, 15);
        }
        let mut address = address;
        for _ in 0..iterations {
            if legacy || step2_mode2 {
                let negative = s >> 15 != 0;
                let m = if negative { modulo | !s } else { modulo | s };
                let mask = ((1u32 << log2p1(m)) - 1) as u16;
                let next = if !negative {
                    if address & mask == modulo && (!step2_mode2 || modulo != mask) {
                        0
                    } else {
                        address.wrapping_add(s) & mask
                    }
                } else if address & mask == 0 && (!step2_mode2 || modulo != mask) {
                    modulo
                } else {
                    address.wrapping_add(s) & mask
                };
                address = address & !mask | next;
            } else {
                let mask = ((1u32 << log2p1(modulo)) - 1) as u16;
                let next = if s < 0x8000 {
                    let next = address.wrapping_add(s) & mask;
                    if next == modulo.wrapping_add(1) & mask {
                        0
                    } else {
                        next
                    }
                } else {
                    let mut next = address & mask;
                    if next == 0 {
                        next = modulo.wrapping_add(1);
                    }
                    next.wrapping_add(s) & mask
                };
                address = address & !mask | next;
            }
        }
        address
    }

    /// Step address register `unit`; its value before.
    fn rn_and_modify(&mut self, unit: usize, step: StepValue, dmod: bool) -> u16 {
        let value = self.regs.r[unit];
        let by_two = matches!(
            step,
            StepValue::Increase2Mode1
                | StepValue::Decrease2Mode1
                | StepValue::Increase2Mode2
                | StepValue::Decrease2Mode2
        );
        if ((unit == 3 && self.regs.epi != 0) || (unit == 7 && self.regs.epj != 0)) && !by_two {
            self.regs.r[unit] = 0;
            return value;
        }
        self.regs.r[unit] = self.step_address(unit, value, step, dmod);
        value
    }

    // ---- memory operands -------------------------------------------------

    fn load_mem8(&mut self, a: u16) -> u16 {
        let addr = a.wrapping_add(self.regs.page << 8);
        self.data_read(addr)
    }

    fn store_mem8(&mut self, a: u16, value: u16) {
        let addr = a.wrapping_add(self.regs.page << 8);
        self.data_write(addr, value);
    }

    fn load_r7(&mut self, a: u16) -> u16 {
        let addr = a.wrapping_add(self.regs.r[7]);
        self.data_read(addr)
    }

    fn store_r7(&mut self, a: u16, value: u16) {
        let addr = a.wrapping_add(self.regs.r[7]);
        self.data_write(addr, value);
    }

    fn read_rn(&mut self, unit: u16, step: StepValue) -> u16 {
        let addr = self.rn_address_and_modify(unit as usize, step, false);
        self.data_read(addr)
    }

    fn read_ar(&mut self, index: u16, step: u16) -> u16 {
        let unit = self.ar_rn_unit(index);
        let step = self.ar_step(step);
        let addr = self.rn_address_and_modify(unit, step, false);
        self.data_read(addr)
    }

    // ---- context ---------------------------------------------------------

    fn context_store(&mut self) {
        self.regs.shadow_store();
        self.regs.shadow_swap();
        if self.regs.crep == 0 {
            self.regs.repcs = self.regs.repc;
        }
        if self.regs.ccnta == 0 {
            self.regs.a1s = self.regs.a[1];
            self.regs.b1s = self.regs.b[1];
        } else {
            let (a, b) = (self.regs.a[1], self.regs.b[1]);
            self.regs.b[1] = a;
            // Flags follow b1 into a1.
            self.set_acc_and_flag(RegName::A1, b);
        }
    }

    fn context_restore(&mut self) {
        self.regs.shadow_restore();
        self.regs.shadow_swap();
        if self.regs.crep == 0 {
            self.regs.repc = self.regs.repcs;
        }
        if self.regs.ccnta == 0 {
            self.regs.a[1] = self.regs.a1s;
            self.regs.b[1] = self.regs.b1s;
        } else {
            std::mem::swap(&mut self.regs.a[1], &mut self.regs.b[1]);
        }
    }
}

// The instructions, by the handler names the decoder table uses.
#[allow(clippy::too_many_arguments)]
impl<B: Bus + ?Sized> Interp<'_, B> {
    pub fn nop(&mut self) {}

    pub fn norm(&mut self, a: RegName, b: u16, bs: StepValue) {
        if self.regs.fn_ == 0 {
            let mut value = self.get_acc(a);
            self.regs.fv = (value != sext64(value, 39)) as u16;
            if self.regs.fv != 0 {
                self.regs.fvl = 1;
            }
            value <<= 1;
            self.regs.fc0 = (value & 1 << 40 != 0) as u16;
            value = sext64(value, 40);
            self.set_acc_and_flag(a, value);
            let unit = b as usize;
            self.rn_and_modify(unit, bs, false);
            self.regs.fr = (self.regs.r[unit] == 0) as u16;
        }
    }

    pub fn swap(&mut self, swap: u16) {
        use RegName::*;
        let (s0, d0, s1, d1) = match swap {
            0 => (A0, B0, B0, A0),
            1 => (A0, B1, B1, A0),
            2 => (A1, B0, B0, A1),
            3 => (A1, B1, B1, A1),
            4 => {
                let (u, v) = (self.get_acc(A1), self.get_acc(B1));
                self.sat_and_set_acc_and_flag(A1, v);
                self.sat_and_set_acc_and_flag(B1, u);
                (A0, B0, B0, A0)
            }
            5 => {
                let (u, v) = (self.get_acc(A1), self.get_acc(B0));
                self.sat_and_set_acc_and_flag(A1, v);
                self.sat_and_set_acc_and_flag(B0, u);
                (A0, B1, B1, A0)
            }
            6 => (A0, B0, B0, A1),
            7 => (A0, B1, B1, A1),
            8 => (A1, B0, B0, A0),
            9 => (A1, B1, B1, A0),
            10 => (A0, B1, B0, A0),
            11 => (A1, B1, B0, A1),
            12 => (A0, B0, B1, A0),
            13 => (A1, B0, B1, A1),
            _ => {
                self.stop("reserved swap");
                return;
            }
        };
        let (u, v) = (self.get_acc(s0), self.get_acc(s1));
        self.sat_and_set_acc_and_flag(d0, u);
        // Only this one sets the flags (but for fl).
        self.sat_and_set_acc_and_flag(d1, v);
    }

    pub fn trap(&mut self) {
        self.stop("trap");
    }

    fn alm_generic(&mut self, op: AlmOp, a: u64, b: RegName) {
        match op {
            AlmOp::Or => {
                let value = sext64(self.get_acc(b) | a, 40);
                self.set_acc_and_flag(b, value);
            }
            AlmOp::And => {
                let value = sext64(self.get_acc(b) & a, 40);
                self.set_acc_and_flag(b, value);
            }
            AlmOp::Xor => {
                let value = sext64(self.get_acc(b) ^ a, 40);
                self.set_acc_and_flag(b, value);
            }
            AlmOp::Tst0 => {
                let value = self.get_acc(b) & 0xFFFF;
                self.regs.fz = (value & a == 0) as u16;
            }
            AlmOp::Tst1 => {
                let value = self.get_acc(b) & 0xFFFF;
                self.regs.fz = (value & !a == 0) as u16;
            }
            AlmOp::Cmp
            | AlmOp::Cmpu
            | AlmOp::Sub
            | AlmOp::Subl
            | AlmOp::Subh
            | AlmOp::Add
            | AlmOp::Addl
            | AlmOp::Addh => {
                let value = self.get_acc(b);
                let sub = !matches!(op, AlmOp::Add | AlmOp::Addl | AlmOp::Addh);
                let result = self.add_sub40(value, a, sub);
                if matches!(op, AlmOp::Cmp | AlmOp::Cmpu) {
                    self.set_acc_flag(result);
                } else {
                    self.sat_and_set_acc_and_flag(b, result);
                }
            }
            AlmOp::Msu => {
                let value = self.get_acc(b);
                let product = self.product_to_bus40(0);
                let result = self.add_sub40(value, product, true);
                self.sat_and_set_acc_and_flag(b, result);
                self.regs.x[0] = a as u16;
                self.do_multiplication(0, true, true);
            }
            AlmOp::Sqra | AlmOp::Sqr => {
                if op == AlmOp::Sqra {
                    let value = self.get_acc(b);
                    let product = self.product_to_bus40(0);
                    let result = self.add_sub40(value, product, false);
                    self.sat_and_set_acc_and_flag(b, result);
                }
                self.regs.x[0] = a as u16;
                self.regs.y[0] = a as u16;
                self.do_multiplication(0, true, true);
            }
            AlmOp::Reserved => self.stop("reserved alu operation"),
        }
    }

    fn extend_for_alm(op: AlmOp, a: u16) -> u64 {
        match op {
            AlmOp::Cmp | AlmOp::Sub | AlmOp::Add => sext64(a as u64, 16),
            AlmOp::Addh | AlmOp::Subh => sext64((a as u64) << 16, 32),
            _ => a as u64,
        }
    }

    pub fn alm_memimm8_ax(&mut self, op: AlmOp, a: u16, b: RegName) {
        let value = self.load_mem8(a);
        self.alm_generic(op, Self::extend_for_alm(op, value), b);
    }

    pub fn alm_rn_stepzids_ax(&mut self, op: AlmOp, a: u16, s: StepValue, b: RegName) {
        let value = self.read_rn(a, s);
        self.alm_generic(op, Self::extend_for_alm(op, value), b);
    }

    pub fn alm_register_ax(&mut self, op: AlmOp, a: RegName, b: RegName) {
        let bus40 = matches!(
            op,
            AlmOp::Or | AlmOp::And | AlmOp::Xor | AlmOp::Add | AlmOp::Cmp | AlmOp::Sub
        );
        let value = match a {
            RegName::P | RegName::A0 | RegName::A1 if !bus40 => {
                self.stop(format!("{op:?} with {a:?}"));
                return;
            }
            RegName::P => self.product_to_bus40(0),
            RegName::A0 | RegName::A1 => self.get_acc(a),
            _ => {
                let v = self.reg_to_bus16(a, false);
                Self::extend_for_alm(op, v)
            }
        };
        self.alm_generic(op, value, b);
    }

    pub fn alm_r6(&mut self, op: AlmOp, b: RegName) {
        let value = self.regs.r[6];
        self.alm_generic(op, Self::extend_for_alm(op, value), b);
    }

    pub fn alu_memimm16_ax(&mut self, op: AlmOp, a: u16, b: RegName) {
        let value = self.data_read(a);
        self.alm_generic(op, Self::extend_for_alm(op, value), b);
    }

    pub fn alu_memr7imm16_ax(&mut self, op: AlmOp, a: u16, b: RegName) {
        let value = self.load_r7(a);
        self.alm_generic(op, Self::extend_for_alm(op, value), b);
    }

    pub fn alu_imm16_ax(&mut self, op: AlmOp, a: u16, b: RegName) {
        self.alm_generic(op, Self::extend_for_alm(op, a), b);
    }

    pub fn alu_imm8_ax(&mut self, op: AlmOp, a: u16, b: RegName) {
        // AND leaves bits 8-15 alone, though the flags are set as if not.
        let and_backup = if op == AlmOp::And {
            self.get_acc(b) & 0xFF00
        } else {
            0
        };
        self.alm_generic(op, Self::extend_for_alm(op, a), b);
        if op == AlmOp::And {
            let and_new = self.get_acc(b) & 0xFFFF_FFFF_FFFF_00FF;
            self.set_acc(b, and_backup | and_new);
        }
    }

    pub fn alu_memr7imm7s_ax(&mut self, op: AlmOp, a: u16, b: RegName) {
        let value = self.load_r7(a);
        self.alm_generic(op, Self::extend_for_alm(op, value), b);
    }

    fn or_generic(&mut self, a: RegName, b: RegName, c: RegName) {
        let value = self.get_acc(a) | self.get_acc(b);
        self.set_acc_and_flag(c, value);
    }

    pub fn or_ab_ax_ax(&mut self, a: RegName, b: RegName, c: RegName) {
        self.or_generic(a, b, c);
    }

    pub fn or_ax_bx_ax(&mut self, a: RegName, b: RegName, c: RegName) {
        self.or_generic(a, b, c);
    }

    pub fn or_bx_bx_ax(&mut self, a: RegName, b: RegName, c: RegName) {
        self.or_generic(a, b, c);
    }

    fn generic_alb(&mut self, op: AlbOp, a: u16, b: u16) -> u16 {
        let result = match op {
            AlbOp::Set => {
                let r = a | b;
                self.regs.fm = r >> 15;
                r
            }
            AlbOp::Rst => {
                let r = !a & b;
                self.regs.fm = r >> 15;
                r
            }
            AlbOp::Chng => {
                let r = a ^ b;
                self.regs.fm = r >> 15;
                r
            }
            AlbOp::Addv => {
                let r = a as u32 + b as u32;
                self.regs.fc0 = (r >> 16 != 0) as u16;
                self.regs.fm =
                    (sext32(b as u32, 16).wrapping_add(sext32(a as u32, 16)) >> 31) as u16;
                r as u16
            }
            AlbOp::Tst0 => (a & b != 0) as u16,
            AlbOp::Tst1 => (a & !b != 0) as u16,
            AlbOp::Cmpv | AlbOp::Subv => {
                let r = (b as u32).wrapping_sub(a as u32);
                self.regs.fc0 = (r >> 16 != 0) as u16;
                self.regs.fm =
                    (sext32(b as u32, 16).wrapping_sub(sext32(a as u32, 16)) >> 31) as u16;
                r as u16
            }
        };
        self.regs.fz = (result == 0) as u16;
        result
    }

    fn alb_modifies(op: AlbOp) -> bool {
        !matches!(op, AlbOp::Tst0 | AlbOp::Tst1 | AlbOp::Cmpv)
    }

    pub fn alb_imm16_memimm8(&mut self, op: AlbOp, a: u16, b: u16) {
        let bv = self.load_mem8(b);
        let result = self.generic_alb(op, a, bv);
        if Self::alb_modifies(op) {
            self.store_mem8(b, result);
        }
    }

    pub fn alb_imm16_rn_stepzids(&mut self, op: AlbOp, a: u16, b: u16, bs: StepValue) {
        let addr = self.rn_address_and_modify(b as usize, bs, false);
        let bv = self.data_read(addr);
        let result = self.generic_alb(op, a, bv);
        if Self::alb_modifies(op) {
            self.data_write(addr, result);
        }
    }

    pub fn alb_imm16_register(&mut self, op: AlbOp, a: u16, b: RegName) {
        use RegName::*;
        let bv = match b {
            P => (self.product_to_bus40(0) >> 16) as u16,
            A0 | A1 => {
                self.stop(format!("{op:?} on {b:?}"));
                return;
            }
            _ => self.reg_to_bus16(b, false),
        };
        let result = self.generic_alb(op, a, bv);
        if !Self::alb_modifies(op) {
            return;
        }
        // Accumulators change without the bus's flags and saturation.
        let low = |v: u64| v & 0xFFFF_FFFF_FFFF_0000 | result as u64;
        let high = |v: u64| v & 0xFFFF_FFFF_0000_FFFF | (result as u64) << 16;
        match b {
            A0l => self.regs.a[0] = low(self.regs.a[0]),
            A1l => self.regs.a[1] = low(self.regs.a[1]),
            B0l => self.regs.b[0] = low(self.regs.b[0]),
            B1l => self.regs.b[1] = low(self.regs.b[1]),
            A0h => self.regs.a[0] = high(self.regs.a[0]),
            A1h => self.regs.a[1] = high(self.regs.a[1]),
            B0h => self.regs.b[0] = high(self.regs.b[0]),
            B1h => self.regs.b[1] = high(self.regs.b[1]),
            _ => self.reg_from_bus16(b, result),
        }
    }

    pub fn alb_r6(&mut self, op: AlbOp, a: u16) {
        let bv = self.regs.r[6];
        let result = self.generic_alb(op, a, bv);
        if Self::alb_modifies(op) {
            self.regs.r[6] = result;
        }
    }

    pub fn alb_imm16_sttmod(&mut self, op: AlbOp, a: u16, b: RegName) {
        let bv = self.reg_to_bus16(b, false);
        let result = self.generic_alb(op, a, bv);
        if Self::alb_modifies(op) {
            self.reg_from_bus16(b, result);
        }
    }

    fn add_acc(&mut self, a: RegName, b: RegName, sub: bool) {
        let (va, vb) = (self.get_acc(a), self.get_acc(b));
        let result = self.add_sub40(vb, va, sub);
        self.sat_and_set_acc_and_flag(b, result);
    }

    fn add_product(&mut self, unit: usize, b: RegName, sub: bool) {
        let va = self.product_to_bus40(unit);
        let vb = self.get_acc(b);
        let result = self.add_sub40(vb, va, sub);
        self.sat_and_set_acc_and_flag(b, result);
    }

    pub fn add_ab_bx(&mut self, a: RegName, b: RegName) {
        self.add_acc(a, b, false);
    }

    pub fn add_bx_ax(&mut self, a: RegName, b: RegName) {
        self.add_acc(a, b, false);
    }

    pub fn add_p1(&mut self, b: RegName) {
        self.add_product(1, b, false);
    }

    pub fn add_px_bx(&mut self, a: u16, b: RegName) {
        self.add_product(a as usize, b, false);
    }

    pub fn sub_ab_bx(&mut self, a: RegName, b: RegName) {
        self.add_acc(a, b, true);
    }

    pub fn sub_bx_ax(&mut self, a: RegName, b: RegName) {
        self.add_acc(a, b, true);
    }

    pub fn sub_p1(&mut self, b: RegName) {
        self.add_product(1, b, true);
    }

    pub fn sub_px_bx(&mut self, a: u16, b: RegName) {
        self.add_product(a as usize, b, true);
    }

    pub fn app(&mut self, c: RegName, base: SumBase, sp0: bool, a0: bool, sp1: bool, a1: bool) {
        self.product_sum(base, c, sp0, a0, sp1, a1);
    }

    /// The two addresses of an arp pair, stepped, and their offsets.
    fn arp_pair(&mut self, a: u16, asi: u16, asj: u16) -> (usize, usize, u16, u16) {
        let (ui, uj) = self.arp_rn_unit(a);
        let (si, sj) = self.arp_step(asi, asj);
        let i = self.rn_address_and_modify(ui, si, false);
        let j = self.rn_address_and_modify(uj, sj, false);
        (ui, uj, i, j)
    }

    /// The add/sub pair family: high halves from j and i, low halves from
    /// their offset addresses.
    fn add_sub_pair(
        &mut self,
        a: u16,
        asi: u16,
        asj: u16,
        b: RegName,
        high_sub: bool,
        low_sub: bool,
    ) {
        let (oi, oj) = self.arp_offset(asi, asj);
        let (ui, uj, i, j) = self.arp_pair(a, asi, asj);
        let hj = sext64(self.data_read(j) as u64, 16);
        let hi = sext64(self.data_read(i) as u64, 16);
        let high = if high_sub {
            hj.wrapping_sub(hi)
        } else {
            hj.wrapping_add(hi)
        };
        let lj_addr = self.offset_address(uj, j, oj, false);
        let lj = self.data_read(lj_addr);
        let li_addr = self.offset_address(ui, i, oi, false);
        let li = self.data_read(li_addr);
        let low = if low_sub {
            lj.wrapping_sub(li)
        } else {
            lj.wrapping_add(li)
        };
        self.set_acc(b, high << 16 | low as u64);
    }

    pub fn add_add(&mut self, a: u16, asi: u16, asj: u16, b: RegName) {
        self.add_sub_pair(a, asi, asj, b, false, false);
    }

    pub fn add_sub(&mut self, a: u16, asi: u16, asj: u16, b: RegName) {
        self.add_sub_pair(a, asi, asj, b, false, true);
    }

    pub fn sub_add(&mut self, a: u16, asi: u16, asj: u16, b: RegName) {
        self.add_sub_pair(a, asi, asj, b, true, false);
    }

    pub fn sub_sub(&mut self, a: u16, asi: u16, asj: u16, b: RegName) {
        self.add_sub_pair(a, asi, asj, b, true, true);
    }

    /// The sv family: high half of the word +/- sv, low half of the
    /// offset word -/+ sv.
    fn sv_pair(&mut self, unit: usize, address: u16, offset: Offset, high_add: bool) -> u64 {
        let sv = self.regs.sv;
        let h = sext64(self.data_read(address) as u64, 16);
        let high = if high_add {
            h.wrapping_add(sext64(sv as u64, 16))
        } else {
            h.wrapping_sub(sext64(sv as u64, 16))
        };
        let low_addr = self.offset_address(unit, address, offset, false);
        let l = self.data_read(low_addr);
        let low = if high_add {
            l.wrapping_sub(sv)
        } else {
            l.wrapping_add(sv)
        };
        high << 16 | low as u64
    }

    pub fn add_sub_sv(&mut self, a: u16, as_: u16, b: RegName) {
        let unit = self.ar_rn_unit(a);
        let (s, o) = (self.ar_step(as_), self.ar_offset(as_));
        let address = self.rn_address_and_modify(unit, s, false);
        let result = self.sv_pair(unit, address, o, true);
        self.set_acc(b, result);
    }

    pub fn sub_add_sv(&mut self, a: u16, as_: u16, b: RegName) {
        let unit = self.ar_rn_unit(a);
        let (s, o) = (self.ar_step(as_), self.ar_offset(as_));
        let address = self.rn_address_and_modify(unit, s, false);
        let result = self.sv_pair(unit, address, o, false);
        self.set_acc(b, result);
    }

    pub fn sub_add_i_mov_j_sv(&mut self, a: u16, asi: u16, asj: u16, b: RegName) {
        let (oi, _) = self.arp_offset(asi, asj);
        let (ui, _, i, j) = self.arp_pair(a, asi, asj);
        let result = self.sv_pair(ui, i, oi, false);
        self.set_acc(b, result);
        self.regs.sv = self.data_read(j);
    }

    pub fn sub_add_j_mov_i_sv(&mut self, a: u16, asi: u16, asj: u16, b: RegName) {
        let (_, oj) = self.arp_offset(asi, asj);
        let (_, uj, i, j) = self.arp_pair(a, asi, asj);
        let result = self.sv_pair(uj, j, oj, false);
        self.set_acc(b, result);
        self.regs.sv = self.data_read(i);
    }

    pub fn add_sub_i_mov_j(&mut self, a: u16, asi: u16, asj: u16, b: RegName) {
        let (oi, _) = self.arp_offset(asi, asj);
        let (ui, _, i, j) = self.arp_pair(a, asi, asj);
        let result = self.sv_pair(ui, i, oi, true);
        let exchange = self.get_and_sat_acc_no_flag(b) as u16;
        self.set_acc(b, result);
        self.data_write(j, exchange);
    }

    pub fn add_sub_j_mov_i(&mut self, a: u16, asi: u16, asj: u16, b: RegName) {
        let (_, oj) = self.arp_offset(asi, asj);
        let (_, uj, i, j) = self.arp_pair(a, asi, asj);
        let result = self.sv_pair(uj, j, oj, true);
        let exchange = self.get_and_sat_acc_no_flag(b) as u16;
        self.set_acc(b, result);
        self.data_write(i, exchange);
    }

    fn moda(&mut self, op: ModaOp, a: RegName, cond: Cond) {
        if !self.regs.condition(cond) {
            return;
        }
        match op {
            ModaOp::Shr => self.shift_bus40(self.get_acc(a), 0xFFFF, a),
            ModaOp::Shr4 => self.shift_bus40(self.get_acc(a), 0xFFFC, a),
            ModaOp::Shl => self.shift_bus40(self.get_acc(a), 1, a),
            ModaOp::Shl4 => self.shift_bus40(self.get_acc(a), 4, a),
            ModaOp::Ror => {
                let mut value = self.get_acc(a) & 0xFF_FFFF_FFFF;
                let old_fc = self.regs.fc0 as u64;
                self.regs.fc0 = (value & 1) as u16;
                value >>= 1;
                value |= old_fc << 39;
                self.set_acc_and_flag(a, sext64(value, 40));
            }
            ModaOp::Rol => {
                let mut value = self.get_acc(a);
                let old_fc = self.regs.fc0 as u64;
                self.regs.fc0 = (value >> 39 & 1) as u16;
                value <<= 1;
                value |= old_fc;
                self.set_acc_and_flag(a, sext64(value, 40));
            }
            ModaOp::Clr => self.sat_and_set_acc_and_flag(a, 0),
            ModaOp::Not => {
                let result = !self.get_acc(a);
                self.set_acc_and_flag(a, result);
            }
            ModaOp::Neg => {
                let value = self.get_acc(a);
                self.regs.fc0 = (value != 0) as u16;
                self.regs.fv = (value == 0xFFFF_FF80_0000_0000) as u16;
                if self.regs.fv != 0 {
                    self.regs.fvl = 1;
                }
                let result = sext64((!value).wrapping_add(1), 40);
                self.sat_and_set_acc_and_flag(a, result);
            }
            ModaOp::Rnd => {
                let value = self.get_acc(a);
                let result = self.add_sub40(value, 0x8000, false);
                self.sat_and_set_acc_and_flag(a, result);
            }
            ModaOp::Pacr => {
                let value = self.product_to_bus40(0);
                let result = self.add_sub40(value, 0x8000, false);
                self.sat_and_set_acc_and_flag(a, result);
            }
            ModaOp::Clrr => self.sat_and_set_acc_and_flag(a, 0x8000),
            ModaOp::Inc => {
                let value = self.get_acc(a);
                let result = self.add_sub40(value, 1, false);
                self.sat_and_set_acc_and_flag(a, result);
            }
            ModaOp::Dec => {
                let value = self.get_acc(a);
                let result = self.add_sub40(value, 1, true);
                self.sat_and_set_acc_and_flag(a, result);
            }
            ModaOp::Copy => {
                // Not for b accumulators.
                let from = if a == RegName::A0 {
                    RegName::A1
                } else {
                    RegName::A0
                };
                let value = self.get_acc(from);
                self.sat_and_set_acc_and_flag(a, value);
            }
            ModaOp::Reserved => self.stop("reserved moda"),
        }
    }

    pub fn moda4(&mut self, op: ModaOp, a: RegName, cond: Cond) {
        self.moda(op, a, cond);
    }

    pub fn moda3(&mut self, op: ModaOp, a: RegName, cond: Cond) {
        self.moda(op, a, cond);
    }

    pub fn pacr1(&mut self, a: RegName) {
        let value = self.product_to_bus40(1);
        let result = self.add_sub40(value, 0x8000, false);
        self.sat_and_set_acc_and_flag(a, result);
    }

    fn filter_double_clr(a: RegName, b: RegName) -> RegName {
        use RegName::*;
        match a {
            B0 => B1,
            B1 => B0,
            A0 if b == A0 => A1,
            A0 => b,
            _ => {
                if b == B1 {
                    B1
                } else {
                    B0
                }
            }
        }
    }

    pub fn clr(&mut self, a: RegName, b: RegName) {
        let b = Self::filter_double_clr(a, b);
        self.sat_and_set_acc_and_flag(a, 0);
        self.sat_and_set_acc_and_flag(b, 0);
    }

    pub fn clrr(&mut self, a: RegName, b: RegName) {
        let b = Self::filter_double_clr(a, b);
        self.sat_and_set_acc_and_flag(a, 0x8000);
        self.sat_and_set_acc_and_flag(b, 0x8000);
    }

    fn block_repeat(&mut self, lc: u16, address: u32) {
        if self.regs.bcn > 3 {
            self.stop("block repeat nested too deep");
            return;
        }
        let level = self.regs.bcn as usize;
        self.regs.bkrep_stack[level].start = self.regs.pc;
        self.regs.bkrep_stack[level].end = address;
        self.regs.bkrep_stack[level].lc = lc;
        self.regs.lp = 1;
        self.regs.bcn += 1;
    }

    pub fn bkrep_imm8_address16(&mut self, a: u16, addr: u32) {
        let address = addr | (self.regs.pc & 0x3_0000);
        self.block_repeat(a, address);
    }

    pub fn bkrep_register_address18_16_address18_2(&mut self, a: RegName, low: u32, high: u32) {
        let lc = self.reg_to_bus16(a, false);
        self.block_repeat(lc, low | high);
    }

    pub fn bkrep_r6(&mut self, low: u32, high: u32) {
        self.block_repeat(self.regs.r[6], low | high);
    }

    /// Restore a block repeat level from memory at `address`, counting up;
    /// the address after it.
    fn restore_block_repeat(&mut self, mut address: u16) -> u16 {
        if self.regs.lp != 0 {
            if self.regs.bcn > 3 {
                self.stop("block repeat nested too deep");
                return address;
            }
            let n = self.regs.bcn as usize;
            self.regs.bkrep_stack.copy_within(0..n, 1);
            self.regs.bcn += 1;
        }
        let flag = self.data_read(address);
        address = address.wrapping_add(1);
        let valid = flag >> 15;
        if self.regs.lp == 0 && valid != 0 {
            self.regs.lp = 1;
            self.regs.bcn = 1;
        }
        let end = self.data_read(address) as u32 | ((flag >> 8 & 3) as u32) << 16;
        address = address.wrapping_add(1);
        let start = self.data_read(address) as u32 | ((flag & 3) as u32) << 16;
        address = address.wrapping_add(1);
        let lc = self.data_read(address);
        address = address.wrapping_add(1);
        self.regs.bkrep_stack[0].end = end;
        self.regs.bkrep_stack[0].start = start;
        self.regs.bkrep_stack[0].lc = lc;
        address
    }

    /// Store the outer block repeat level below `address`, counting down;
    /// the address it ends at.
    fn store_block_repeat(&mut self, mut address: u16) -> u16 {
        let frame = self.regs.bkrep_stack[0];
        let mut put = |it: &mut Self, v: u16| {
            address = address.wrapping_sub(1);
            it.data_write(address, v);
        };
        put(self, frame.lc);
        put(self, frame.start as u16);
        put(self, frame.end as u16);
        let flag =
            self.regs.lp << 15 | (frame.start >> 16) as u16 | ((frame.end >> 16) as u16) << 8;
        put(self, flag);
        if self.regs.lp != 0 && self.regs.bcn > 0 {
            let n = self.regs.bcn as usize;
            self.regs.bkrep_stack.copy_within(1..n, 0);
            self.regs.bcn -= 1;
            if self.regs.bcn == 0 {
                self.regs.lp = 0;
            }
        }
        address
    }

    pub fn bkreprst(&mut self, a: u16) {
        let unit = self.ar_rn_unit(a);
        self.regs.r[unit] = self.restore_block_repeat(self.regs.r[unit]);
    }

    pub fn bkreprst_memsp(&mut self) {
        self.regs.sp = self.restore_block_repeat(self.regs.sp);
    }

    pub fn bkrepsto(&mut self, a: u16) {
        let unit = self.ar_rn_unit(a);
        self.regs.r[unit] = self.store_block_repeat(self.regs.r[unit]);
    }

    pub fn bkrepsto_memsp(&mut self) {
        self.regs.sp = self.store_block_repeat(self.regs.sp);
    }

    pub fn banke(&mut self, flags: u16) {
        let r = &mut *self.regs;
        if flags & 1 != 0 {
            std::mem::swap(&mut r.stepi, &mut r.stepib);
            std::mem::swap(&mut r.modi, &mut r.modib);
            if r.stp16 != 0 {
                std::mem::swap(&mut r.stepi0, &mut r.stepi0b);
            }
        }
        if flags & 2 != 0 {
            std::mem::swap(&mut r.r[4], &mut r.r4b);
        }
        if flags & 4 != 0 {
            std::mem::swap(&mut r.r[1], &mut r.r1b);
        }
        if flags & 8 != 0 {
            std::mem::swap(&mut r.r[0], &mut r.r0b);
        }
        if flags & 16 != 0 {
            std::mem::swap(&mut r.r[7], &mut r.r7b);
        }
        if flags & 32 != 0 {
            std::mem::swap(&mut r.stepj, &mut r.stepjb);
            std::mem::swap(&mut r.modj, &mut r.modjb);
            if r.stp16 != 0 {
                std::mem::swap(&mut r.stepj0, &mut r.stepj0b);
            }
        }
    }

    pub fn bankr(&mut self) {
        self.regs.swap_all_ar_arp();
    }

    pub fn bankr_ar(&mut self, a: u16) {
        self.regs.swap_ar(a);
    }

    pub fn bankr_ar_arp(&mut self, a: u16, b: u16) {
        self.regs.swap_ar(a);
        self.regs.swap_arp(b);
    }

    pub fn bankr_arp(&mut self, a: u16) {
        self.regs.swap_arp(a);
    }

    pub fn bitrev(&mut self, a: u16) {
        let unit = a as usize;
        self.regs.r[unit] = bit_reverse(self.regs.r[unit]);
    }

    pub fn bitrev_dbrv(&mut self, a: u16) {
        self.bitrev(a);
        self.regs.br[a as usize] = 0;
    }

    pub fn bitrev_ebrv(&mut self, a: u16) {
        self.bitrev(a);
        self.regs.br[a as usize] = 1;
    }

    pub fn br(&mut self, low: u32, high: u32, cond: Cond) {
        if self.regs.condition(cond) {
            self.set_pc(low | high);
        }
    }

    pub fn brr(&mut self, addr: u32, cond: Cond) {
        if self.regs.condition(cond) {
            // pc is already the address of the next instruction.
            self.regs.pc = self.regs.pc.wrapping_add(addr);
            if addr == 0xFFFF_FFFF {
                self.idle = true;
            }
        }
    }

    pub fn break_(&mut self) {
        if self.regs.lp == 0 || self.regs.bcn == 0 {
            self.stop("break outside a block repeat");
            return;
        }
        // It ends the loop, but does not jump out of it.
        self.regs.bcn -= 1;
        self.regs.lp = (self.regs.bcn != 0) as u16;
    }

    pub fn call(&mut self, low: u32, high: u32, cond: Cond) {
        if self.regs.condition(cond) {
            self.push_pc();
            self.set_pc(low | high);
        }
    }

    pub fn calla_axl(&mut self, a: RegName) {
        self.push_pc();
        let target = self.reg_to_bus16(a, false);
        self.set_pc(target as u32);
    }

    pub fn calla_ax(&mut self, a: RegName) {
        self.push_pc();
        let target = self.get_acc(a) & 0x3_FFFF;
        self.set_pc(target as u32);
    }

    pub fn callr(&mut self, addr: u32, cond: Cond) {
        if self.regs.condition(cond) {
            self.push_pc();
            self.regs.pc = self.regs.pc.wrapping_add(addr);
        }
    }

    pub fn cntx_s(&mut self) {
        self.context_store();
    }

    pub fn cntx_r(&mut self) {
        self.context_restore();
    }

    pub fn ret(&mut self, c: Cond) {
        if self.regs.condition(c) {
            self.pop_pc();
        }
    }

    pub fn retd(&mut self) {
        self.stop("retd");
    }

    pub fn reti(&mut self, c: Cond) {
        if self.regs.condition(c) {
            self.pop_pc();
            self.regs.ie = 1;
        }
    }

    pub fn retic(&mut self, c: Cond) {
        if self.regs.condition(c) {
            self.pop_pc();
            self.regs.ie = 1;
            self.context_restore();
        }
    }

    pub fn retid(&mut self) {
        self.stop("retid");
    }

    pub fn retidc(&mut self) {
        self.stop("retidc");
    }

    pub fn rets(&mut self, a: u16) {
        self.pop_pc();
        self.regs.sp = self.regs.sp.wrapping_add(a);
    }

    pub fn load_ps(&mut self, a: u16) {
        self.regs.ps[0] = a;
    }

    pub fn load_stepi(&mut self, a: u16) {
        // Signed, but only the low 7 bits are kept.
        self.regs.stepi = a & 0x7F;
    }

    pub fn load_stepj(&mut self, a: u16) {
        self.regs.stepj = a & 0x7F;
    }

    pub fn load_page(&mut self, a: u16) {
        self.regs.page = a;
    }

    pub fn load_modi(&mut self, a: u16) {
        self.regs.modi = a;
    }

    pub fn load_modj(&mut self, a: u16) {
        self.regs.modj = a;
    }

    pub fn load_movpd(&mut self, a: u16) {
        self.regs.pcmhi = a;
    }

    pub fn load_ps01(&mut self, a: u16) {
        self.regs.ps[0] = a & 3;
        self.regs.ps[1] = a >> 2;
    }

    pub fn push_imm16(&mut self, a: u16) {
        self.push(a);
    }

    pub fn push_register(&mut self, a: RegName) {
        let value = self.reg_to_bus16(a, true);
        self.push(value);
    }

    pub fn push_abe(&mut self, a: RegName) {
        let value = (self.get_and_sat_acc(a) >> 32) as u16;
        self.push(value);
    }

    pub fn push_ararpsttmod(&mut self, a: RegName) {
        let value = self.reg_to_bus16(a, false);
        self.push(value);
    }

    pub fn push_prpage(&mut self) {
        self.push(self.regs.prpage);
    }

    pub fn push_px(&mut self, a: u16) {
        let value = self.product_to_bus40(a as usize) as u32;
        self.push(value as u16);
        self.push((value >> 16) as u16);
    }

    pub fn push_r6(&mut self) {
        self.push(self.regs.r[6]);
    }

    pub fn push_repc(&mut self) {
        self.push(self.regs.repc);
    }

    pub fn push_x0(&mut self) {
        self.push(self.regs.x[0]);
    }

    pub fn push_x1(&mut self) {
        self.push(self.regs.x[1]);
    }

    pub fn push_y1(&mut self) {
        self.push(self.regs.y[1]);
    }

    pub fn pusha_ax(&mut self, a: RegName) {
        let value = self.get_and_sat_acc(a) as u32;
        self.push(value as u16);
        self.push((value >> 16) as u16);
    }

    pub fn pusha_bx(&mut self, a: RegName) {
        self.pusha_ax(a);
    }

    pub fn pop_register(&mut self, a: RegName) {
        let value = self.pop();
        self.reg_from_bus16(a, value);
    }

    pub fn pop_abe(&mut self, a: RegName) {
        let value32 = sext32(self.pop() as u32 & 0xFF, 8);
        let acc = self.get_acc(a);
        self.set_acc_and_flag(a, acc & 0xFFFF_FFFF | (value32 as u64) << 32);
    }

    pub fn pop_ararpsttmod(&mut self, a: RegName) {
        let value = self.pop();
        self.reg_from_bus16(a, value);
    }

    pub fn pop_bx(&mut self, a: RegName) {
        let value = self.pop();
        self.reg_from_bus16(a, value);
    }

    pub fn pop_prpage(&mut self) {
        self.regs.prpage = self.pop();
    }

    pub fn pop_px(&mut self, a: u16) {
        let h = self.pop();
        let l = self.pop();
        self.product_from_bus32(a as usize, (h as u32) << 16 | l as u32);
    }

    pub fn pop_r6(&mut self) {
        self.regs.r[6] = self.pop();
    }

    pub fn pop_repc(&mut self) {
        self.regs.repc = self.pop();
    }

    pub fn pop_x0(&mut self) {
        self.regs.x[0] = self.pop();
    }

    pub fn pop_x1(&mut self) {
        self.regs.x[1] = self.pop();
    }

    pub fn pop_y1(&mut self) {
        self.regs.y[1] = self.pop();
    }

    pub fn popa(&mut self, a: RegName) {
        let h = self.pop();
        let l = self.pop();
        let value = sext64((h as u64) << 16 | l as u64, 32);
        self.set_acc_and_flag(a, value);
    }

    fn repeat(&mut self, repc: u16) {
        self.regs.repc = repc;
        self.regs.rep = true;
    }

    pub fn rep_imm8(&mut self, a: u16) {
        self.repeat(a);
    }

    pub fn rep_register(&mut self, a: RegName) {
        let value = self.reg_to_bus16(a, false);
        self.repeat(value);
    }

    pub fn rep_r6(&mut self) {
        self.repeat(self.regs.r[6]);
    }

    pub fn shfc(&mut self, a: RegName, b: RegName, cond: Cond) {
        if self.regs.condition(cond) {
            let value = self.get_acc(a);
            self.shift_bus40(value, self.regs.sv, b);
        }
    }

    pub fn shfi(&mut self, a: RegName, b: RegName, s: u16) {
        let value = self.get_acc(a);
        self.shift_bus40(value, s, b);
    }

    pub fn tst4b_arrn2_arstep2(&mut self, b: u16, bs: u16) {
        let value = self.read_ar(b, bs);
        let bit = self.get_acc(RegName::A0) & 0xF;
        let v = value >> bit & 1;
        self.regs.fz = v;
        self.regs.fc0 = v;
    }

    pub fn tst4b_arrn2_arstep2_ax(&mut self, b: u16, bs: u16, c: RegName) {
        let a = self.get_acc(RegName::A0);
        let bit = a & 0xF;
        let r = &*self.regs;
        let (fv, fvl, fm, fn_, fe) = (r.fv, r.fvl, r.fm, r.fn_, r.fe);
        self.shift_bus40(a, self.regs.sv, c);
        let r = &mut *self.regs;
        r.fc1 = r.fc0;
        r.fv = fv;
        r.fvl = fvl;
        r.fm = fm;
        r.fn_ = fn_;
        r.fe = fe;
        let value = self.read_ar(b, bs);
        let v = value >> bit & 1;
        self.regs.fz = v;
        self.regs.fc0 = v;
    }

    pub fn tstb_memimm8_imm4(&mut self, a: u16, b: u16) {
        let value = self.load_mem8(a);
        self.regs.fz = value >> b & 1;
    }

    pub fn tstb_rn_stepzids_imm4(&mut self, a: u16, s: StepValue, b: u16) {
        let value = self.read_rn(a, s);
        self.regs.fz = value >> b & 1;
    }

    pub fn tstb_register_imm4(&mut self, a: RegName, b: u16) {
        let value = self.reg_to_bus16(a, false);
        self.regs.fz = value >> b & 1;
    }

    pub fn tstb_r6(&mut self, b: u16) {
        self.regs.fz = self.regs.r[6] >> b & 1;
    }

    pub fn tstb_sttmod_imm16(&mut self, a: RegName, b: u16) {
        let value = self.reg_to_bus16(a, false);
        self.regs.fz = value.checked_shr(b as u32).unwrap_or(0) & 1;
    }

    pub fn and(&mut self, a: RegName, b: RegName, c: RegName) {
        let value = self.get_acc(a) & self.get_acc(b);
        self.set_acc_and_flag(c, value);
    }

    pub fn dint(&mut self) {
        self.regs.ie = 0;
    }

    pub fn eint(&mut self) {
        self.regs.ie = 1;
    }

    fn mul_generic(&mut self, op: MulOp, a: RegName) {
        if op != MulOp::Mpy && op != MulOp::Mpysu {
            let value = self.get_acc(a);
            let mut product = self.product_to_bus40(0);
            if op == MulOp::Maa || op == MulOp::Maasu {
                product = sext64(product >> 16, 24);
            }
            let result = self.add_sub40(value, product, false);
            self.sat_and_set_acc_and_flag(a, result);
        }
        match op {
            MulOp::Mpy | MulOp::Mac | MulOp::Maa => self.do_multiplication(0, true, true),
            // "mpysu" multiplies a signed y by an unsigned x.
            MulOp::Mpysu | MulOp::Macsu | MulOp::Maasu => self.do_multiplication(0, false, true),
            MulOp::Macus => self.do_multiplication(0, true, false),
            MulOp::Macuu => self.do_multiplication(0, false, false),
        }
    }

    pub fn mul_rn_stepzids_imm16_ax(
        &mut self,
        op: MulOp,
        y: u16,
        ys: StepValue,
        x: u16,
        a: RegName,
    ) {
        self.regs.y[0] = self.read_rn(y, ys);
        self.regs.x[0] = x;
        self.mul_generic(op, a);
    }

    pub fn mul_y0_rn_stepzids_ax(&mut self, op: MulOp, x: u16, xs: StepValue, a: RegName) {
        self.regs.x[0] = self.read_rn(x, xs);
        self.mul_generic(op, a);
    }

    pub fn mul_y0_register_ax(&mut self, op: MulOp, x: RegName, a: RegName) {
        self.regs.x[0] = self.reg_to_bus16(x, false);
        self.mul_generic(op, a);
    }

    pub fn mul_r45_stepzids_r0123_stepzids_ax(
        &mut self,
        op: MulOp,
        y: u16,
        ys: StepValue,
        x: u16,
        xs: StepValue,
        a: RegName,
    ) {
        let address_y = self.rn_address_and_modify(y as usize, ys, false);
        let address_x = self.rn_address_and_modify(x as usize, xs, false);
        self.regs.y[0] = self.data_read(address_y);
        self.regs.x[0] = self.data_read(address_x);
        self.mul_generic(op, a);
    }

    pub fn mul_y0_r6(&mut self, op: MulOp, a: RegName) {
        self.regs.x[0] = self.regs.r[6];
        self.mul_generic(op, a);
    }

    pub fn mul_y0_memimm8_ax(&mut self, op: MulOp, x: u16, a: RegName) {
        self.regs.x[0] = self.load_mem8(x);
        self.mul_generic(op, a);
    }

    pub fn mpyi(&mut self, x: u16) {
        self.regs.x[0] = x;
        self.do_multiplication(0, true, true);
    }

    fn msu_accumulate(&mut self, a: RegName) {
        let value = self.get_acc(a);
        let product = self.product_to_bus40(0);
        let result = self.add_sub40(value, product, true);
        self.sat_and_set_acc_and_flag(a, result);
    }

    pub fn msu_r45_stepzids_r0123_stepzids_ax(
        &mut self,
        y: u16,
        ys: StepValue,
        x: u16,
        xs: StepValue,
        a: RegName,
    ) {
        let yi = self.rn_address_and_modify(y as usize, ys, false);
        let xi = self.rn_address_and_modify(x as usize, xs, false);
        self.msu_accumulate(a);
        self.regs.y[0] = self.data_read(yi);
        self.regs.x[0] = self.data_read(xi);
        self.do_multiplication(0, true, true);
    }

    pub fn msu_rn_stepzids_imm16_ax(&mut self, y: u16, ys: StepValue, x: u16, a: RegName) {
        let yi = self.rn_address_and_modify(y as usize, ys, false);
        self.msu_accumulate(a);
        self.regs.y[0] = self.data_read(yi);
        self.regs.x[0] = x;
        self.do_multiplication(0, true, true);
    }

    pub fn msusu(&mut self, x: u16, xs: u16, a: RegName) {
        let unit = self.ar_rn_unit(x);
        let step = self.ar_step(xs);
        let xi = self.rn_address_and_modify(unit, step, false);
        self.msu_accumulate(a);
        self.regs.x[0] = self.data_read(xi);
        self.do_multiplication(0, false, true);
    }

    pub fn mac_x1to0(&mut self, a: RegName) {
        let value = self.get_acc(a);
        let product = self.product_to_bus40(0);
        let result = self.add_sub40(value, product, false);
        self.sat_and_set_acc_and_flag(a, result);
        self.regs.x[0] = self.regs.x[1];
        self.do_multiplication(0, true, true);
    }

    pub fn mac1(&mut self, xy: u16, xis: u16, yjs: u16, a: RegName) {
        let (_, _, i, j) = self.arp_pair(xy, xis, yjs);
        let value = self.get_acc(a);
        let product = self.product_to_bus40(1);
        let result = self.add_sub40(value, product, false);
        self.sat_and_set_acc_and_flag(a, result);
        self.regs.x[1] = self.data_read(i);
        self.regs.y[1] = self.data_read(j);
        self.do_multiplication(1, true, true);
    }

    fn modr_generic(&mut self, a: u16, step: StepValue, dmod: bool) {
        let unit = a as usize;
        self.rn_and_modify(unit, step, dmod);
        self.regs.fr = (self.regs.r[unit] == 0) as u16;
    }

    pub fn modr(&mut self, a: u16, s: StepValue) {
        self.modr_generic(a, s, false);
    }

    pub fn modr_dmod(&mut self, a: u16, s: StepValue) {
        self.modr_generic(a, s, true);
    }

    pub fn modr_i2(&mut self, a: u16) {
        self.modr_generic(a, StepValue::Increase2Mode1, false);
    }

    pub fn modr_i2_dmod(&mut self, a: u16) {
        self.modr_generic(a, StepValue::Increase2Mode1, true);
    }

    pub fn modr_d2(&mut self, a: u16) {
        self.modr_generic(a, StepValue::Decrease2Mode1, false);
    }

    pub fn modr_d2_dmod(&mut self, a: u16) {
        self.modr_generic(a, StepValue::Decrease2Mode1, true);
    }

    fn modr_pair(&mut self, a: u16, asi: u16, asj: u16, dmodi: bool, dmodj: bool) {
        let (ui, uj) = self.arp_rn_unit(a);
        let (si, sj) = self.arp_step(asi, asj);
        self.rn_and_modify(ui, si, dmodi);
        self.rn_and_modify(uj, sj, dmodj);
    }

    pub fn modr_eemod(&mut self, a: u16, asi: u16, asj: u16) {
        self.modr_pair(a, asi, asj, false, false);
    }

    pub fn modr_edmod(&mut self, a: u16, asi: u16, asj: u16) {
        self.modr_pair(a, asi, asj, false, true);
    }

    pub fn modr_demod(&mut self, a: u16, asi: u16, asj: u16) {
        self.modr_pair(a, asi, asj, true, false);
    }

    pub fn modr_ddmod(&mut self, a: u16, asi: u16, asj: u16) {
        self.modr_pair(a, asi, asj, true, true);
    }

    pub fn movd(&mut self, a: u16, as_: StepValue, b: u16, bs: StepValue) {
        let address_s = self.rn_address_and_modify(a as usize, as_, false);
        let address_d = self.rn_address_and_modify(b as usize, bs, false) as u32
            | (self.regs.pcmhi as u32) << 16;
        let value = self.data_read(address_s);
        self.program_write(address_d, value);
    }

    pub fn movp_axl_register(&mut self, a: RegName, b: RegName) {
        let address = self.reg_to_bus16(a, false) as u32 | (self.regs.pcmhi as u32) << 16;
        let value = self.program_read(address);
        self.reg_from_bus16(b, value);
    }

    pub fn movp_ax_register(&mut self, a: RegName, b: RegName) {
        let address = (self.get_acc(a) & 0x3_FFFF) as u32;
        let value = self.program_read(address);
        self.reg_from_bus16(b, value);
    }

    pub fn movp_rn_stepzids_r0123_stepzids(
        &mut self,
        a: u16,
        as_: StepValue,
        b: u16,
        bs: StepValue,
    ) {
        let address_s = self.rn_address_and_modify(a as usize, as_, false) as u32
            | (self.regs.pcmhi as u32) << 16;
        let address_d = self.rn_address_and_modify(b as usize, bs, false);
        let value = self.program_read(address_s);
        self.data_write(address_d, value);
    }

    pub fn movpdw(&mut self, a: RegName) {
        let address = (self.get_acc(a) & 0x3_FFFF) as u32;
        // The word order does not follow cpc.
        let h = self.program_read(address);
        let l = self.program_read(address + 1);
        self.set_pc(l as u32 | (h as u32) << 16);
    }

    pub fn mov_ab_ab(&mut self, a: RegName, b: RegName) {
        let value = self.get_acc(a);
        self.sat_and_set_acc_and_flag(b, value);
    }

    pub fn mov_dvm(&mut self, _a: RegName) {
        self.stop("mov to dvm");
    }

    pub fn mov_x0(&mut self, a: RegName) {
        self.regs.x[0] = self.reg_to_bus16(a, true);
    }

    pub fn mov_x1(&mut self, a: RegName) {
        self.regs.x[1] = self.reg_to_bus16(a, true);
    }

    pub fn mov_y1(&mut self, a: RegName) {
        self.regs.y[1] = self.reg_to_bus16(a, true);
    }

    pub fn mov_ablh_memimm8(&mut self, a: RegName, b: u16) {
        let value = self.reg_to_bus16(a, true);
        self.store_mem8(b, value);
    }

    pub fn mov_axl_memimm16(&mut self, a: RegName, b: u16) {
        let value = self.reg_to_bus16(a, true);
        self.data_write(b, value);
    }

    pub fn mov_axl_memr7imm16(&mut self, a: RegName, b: u16) {
        let value = self.reg_to_bus16(a, true);
        self.store_r7(b, value);
    }

    pub fn mov_axl_memr7imm7s(&mut self, a: RegName, b: u16) {
        let value = self.reg_to_bus16(a, true);
        self.store_r7(b, value);
    }

    pub fn mov_memimm16_ax(&mut self, a: u16, b: RegName) {
        let value = self.data_read(a);
        self.reg_from_bus16(b, value);
    }

    pub fn mov_memimm8_ab(&mut self, a: u16, b: RegName) {
        let value = self.load_mem8(a);
        self.reg_from_bus16(b, value);
    }

    pub fn mov_memimm8_ablh(&mut self, a: u16, b: RegName) {
        let value = self.load_mem8(a);
        self.reg_from_bus16(b, value);
    }

    pub fn mov_eu(&mut self, a: u16, b: RegName) {
        let value = self.load_mem8(a);
        let acc = self.get_acc(b) & 0xFFFF_FFFF_0000_0000 | (value as u64) << 16;
        self.set_acc_and_flag(b, acc);
    }

    pub fn mov_memimm8_rnold(&mut self, a: u16, b: RegName) {
        let value = self.load_mem8(a);
        self.reg_from_bus16(b, value);
    }

    pub fn mov_sv_memimm8(&mut self, a: u16) {
        self.regs.sv = self.load_mem8(a);
    }

    pub fn mov_dvm_to(&mut self, _b: RegName) {
        self.stop("mov from dvm");
    }

    pub fn mov_icr_to(&mut self, b: RegName) {
        let value = self.regs.icr();
        self.reg_from_bus16(b, value);
    }

    pub fn mov_imm16_bx(&mut self, a: u16, b: RegName) {
        self.reg_from_bus16(b, a);
    }

    pub fn mov_imm16_register(&mut self, a: u16, b: RegName) {
        self.reg_from_bus16(b, a);
    }

    pub fn mov_icr_imm5(&mut self, a: u16) {
        let value = self.regs.icr() & !0x1F | a;
        self.regs.set_icr(value);
    }

    pub fn mov_imm8s_axh(&mut self, a: u16, b: RegName) {
        self.reg_from_bus16(b, a);
    }

    pub fn mov_ext0(&mut self, a: u16) {
        self.regs.ext[0] = a;
    }

    pub fn mov_ext1(&mut self, a: u16) {
        self.regs.ext[1] = a;
    }

    pub fn mov_ext2(&mut self, a: u16) {
        self.regs.ext[2] = a;
    }

    pub fn mov_ext3(&mut self, a: u16) {
        self.regs.ext[3] = a;
    }

    pub fn mov_imm8s_rnold(&mut self, a: u16, b: RegName) {
        self.reg_from_bus16(b, a);
    }

    pub fn mov_sv_imm8s(&mut self, a: u16) {
        self.regs.sv = a;
    }

    pub fn mov_imm8_axl(&mut self, a: u16, b: RegName) {
        self.reg_from_bus16(b, a);
    }

    pub fn mov_memr7imm16_ax(&mut self, a: u16, b: RegName) {
        let value = self.load_r7(a);
        self.reg_from_bus16(b, value);
    }

    pub fn mov_memr7imm7s_ax(&mut self, a: u16, b: RegName) {
        let value = self.load_r7(a);
        self.reg_from_bus16(b, value);
    }

    pub fn mov_rn_stepzids_bx(&mut self, a: u16, s: StepValue, b: RegName) {
        let value = self.read_rn(a, s);
        self.reg_from_bus16(b, value);
    }

    pub fn mov_rn_stepzids_register(&mut self, a: u16, s: StepValue, b: RegName) {
        let value = self.read_rn(a, s);
        self.reg_from_bus16(b, value);
    }

    pub fn mov_memsp_to(&mut self, b: RegName) {
        let value = self.data_read(self.regs.sp);
        self.reg_from_bus16(b, value);
    }

    pub fn mov_mixp_to_register(&mut self, b: RegName) {
        self.reg_from_bus16(b, self.regs.mixp);
    }

    pub fn mov_rnold_memimm8(&mut self, a: RegName, b: u16) {
        let value = self.reg_to_bus16(a, false);
        self.store_mem8(b, value);
    }

    pub fn mov_icr_register(&mut self, a: RegName) {
        let value = self.reg_to_bus16(a, true);
        self.regs.set_icr(value);
    }

    pub fn mov_mixp(&mut self, a: RegName) {
        self.regs.mixp = self.reg_to_bus16(a, true);
    }

    pub fn mov_register_rn_stepzids(&mut self, a: RegName, b: u16, bs: StepValue) {
        let value = self.reg_to_bus16(a, true);
        let address = self.rn_address_and_modify(b as usize, bs, false);
        self.data_write(address, value);
    }

    pub fn mov_register_bx(&mut self, a: RegName, b: RegName) {
        match a {
            RegName::P => {
                let value = self.product_to_bus40(0);
                self.sat_and_set_acc_and_flag(b, value);
            }
            RegName::A0 | RegName::A1 => {
                let value = self.get_acc(a);
                self.sat_and_set_acc_and_flag(b, value);
            }
            _ => {
                let value = self.reg_to_bus16(a, true);
                self.reg_from_bus16(b, value);
            }
        }
    }

    pub fn mov_register_register(&mut self, a: RegName, b: RegName) {
        match a {
            RegName::P => {
                // The destination field then picks a0 or a1 by its low bit.
                let index = REGISTER.iter().position(|&r| r == b).unwrap_or(0);
                let dest = if index & 1 != 0 {
                    RegName::A1
                } else {
                    RegName::A0
                };
                let value = self.product_to_bus40(0);
                self.sat_and_set_acc_and_flag(dest, value);
            }
            RegName::Pc => {
                if b == RegName::A0 || b == RegName::A1 {
                    self.sat_and_set_acc_and_flag(b, self.regs.pc as u64);
                } else {
                    self.reg_from_bus16(b, self.regs.pc as u16);
                }
            }
            _ => {
                let value = self.reg_to_bus16(a, true);
                self.reg_from_bus16(b, value);
            }
        }
    }

    pub fn mov_repc_to_ab(&mut self, b: RegName) {
        self.reg_from_bus16(b, self.regs.repc);
    }

    pub fn mov_sv_to(&mut self, b: u16) {
        self.store_mem8(b, self.regs.sv);
    }

    pub fn mov_x0_to(&mut self, b: RegName) {
        self.reg_from_bus16(b, self.regs.x[0]);
    }

    pub fn mov_x1_to(&mut self, b: RegName) {
        self.reg_from_bus16(b, self.regs.x[1]);
    }

    pub fn mov_y1_to(&mut self, b: RegName) {
        self.reg_from_bus16(b, self.regs.y[1]);
    }

    pub fn mov_imm16_ararp(&mut self, a: u16, b: RegName) {
        self.reg_from_bus16(b, a);
    }

    pub fn mov_r6_imm16(&mut self, a: u16) {
        self.regs.r[6] = a;
    }

    pub fn mov_repc_imm16(&mut self, a: u16) {
        self.regs.repc = a;
    }

    pub fn mov_stepi0(&mut self, a: u16) {
        self.regs.stepi0 = a;
    }

    pub fn mov_stepj0(&mut self, a: u16) {
        self.regs.stepj0 = a;
    }

    pub fn mov_imm16_sttmod(&mut self, a: u16, b: RegName) {
        self.reg_from_bus16(b, a);
    }

    pub fn mov_prpage_imm4(&mut self, a: u16) {
        self.regs.prpage = a;
    }

    pub fn mov_a0h_stepi0(&mut self) {
        self.regs.stepi0 = self.reg_to_bus16(RegName::A0h, true);
    }

    pub fn mov_a0h_stepj0(&mut self) {
        self.regs.stepj0 = self.reg_to_bus16(RegName::A0h, true);
    }

    pub fn mov_stepi0_a0h(&mut self) {
        self.reg_from_bus16(RegName::A0h, self.regs.stepi0);
    }

    pub fn mov_stepj0_a0h(&mut self) {
        self.reg_from_bus16(RegName::A0h, self.regs.stepj0);
    }

    pub fn mov_prpage_abl(&mut self, a: RegName) {
        self.regs.prpage = (self.get_acc(a) & 0xF) as u16;
    }

    pub fn mov_repc_abl(&mut self, a: RegName) {
        self.regs.repc = self.reg_to_bus16(a, true);
    }

    pub fn mov_abl_ararp(&mut self, a: RegName, b: RegName) {
        let value = self.reg_to_bus16(a, true);
        self.reg_from_bus16(b, value);
    }

    pub fn mov_abl_sttmod(&mut self, a: RegName, b: RegName) {
        let value = self.reg_to_bus16(a, true);
        self.reg_from_bus16(b, value);
    }

    pub fn mov_prpage_to(&mut self, b: RegName) {
        self.reg_from_bus16(b, self.regs.prpage);
    }

    pub fn mov_repc_to_abl(&mut self, b: RegName) {
        self.reg_from_bus16(b, self.regs.repc);
    }

    pub fn mov_ararp_abl(&mut self, a: RegName, b: RegName) {
        let value = self.reg_to_bus16(a, false);
        self.reg_from_bus16(b, value);
    }

    pub fn mov_sttmod_abl(&mut self, a: RegName, b: RegName) {
        let value = self.reg_to_bus16(a, false);
        self.reg_from_bus16(b, value);
    }

    fn ar_address(&mut self, b: u16, bs: u16) -> u16 {
        let unit = self.ar_rn_unit(b);
        let step = self.ar_step(bs);
        self.rn_address_and_modify(unit, step, false)
    }

    pub fn mov_repc_to_arrn1_arstep1(&mut self, b: u16, bs: u16) {
        let address = self.ar_address(b, bs);
        self.data_write(address, self.regs.repc);
    }

    pub fn mov_ararp_arrn1_arstep1(&mut self, a: RegName, b: u16, bs: u16) {
        let address = self.ar_address(b, bs);
        let value = self.reg_to_bus16(a, false);
        self.data_write(address, value);
    }

    pub fn mov_sttmod_arrn1_arstep1(&mut self, a: RegName, b: u16, bs: u16) {
        self.mov_ararp_arrn1_arstep1(a, b, bs);
    }

    pub fn mov_repc_arrn1_arstep1(&mut self, a: u16, as_: u16) {
        self.regs.repc = self.read_ar(a, as_);
    }

    pub fn mov_arrn1_arstep1_ararp(&mut self, a: u16, as_: u16, b: RegName) {
        let value = self.read_ar(a, as_);
        self.reg_from_bus16(b, value);
    }

    pub fn mov_arrn1_arstep1_sttmod(&mut self, a: u16, as_: u16, b: RegName) {
        self.mov_arrn1_arstep1_ararp(a, as_, b);
    }

    pub fn mov_repc_to_memr7imm16(&mut self, b: u16) {
        self.store_r7(b, self.regs.repc);
    }

    pub fn mov_ararpsttmod_memr7imm16(&mut self, a: RegName, b: u16) {
        let value = self.reg_to_bus16(a, false);
        self.store_r7(b, value);
    }

    pub fn mov_repc_memr7imm16(&mut self, a: u16) {
        self.regs.repc = self.load_r7(a);
    }

    pub fn mov_memr7imm16_ararpsttmod(&mut self, a: u16, b: RegName) {
        let value = self.load_r7(a);
        self.reg_from_bus16(b, value);
    }

    pub fn mov_pc_ax(&mut self, a: RegName) {
        let value = self.get_acc(a) & 0xFFFF_FFFF;
        self.set_pc(value as u32);
    }

    pub fn mov_pc_bx(&mut self, a: RegName) {
        self.mov_pc_ax(a);
    }

    pub fn mov_mixp_to_bx(&mut self, b: RegName) {
        self.reg_from_bus16(b, self.regs.mixp);
    }

    pub fn mov_mixp_r6(&mut self) {
        self.regs.r[6] = self.regs.mixp;
    }

    pub fn mov_p0h_to_bx(&mut self, b: RegName) {
        let value = (self.product_to_bus40(0) >> 16) as u16;
        self.reg_from_bus16(b, value);
    }

    pub fn mov_p0h_r6(&mut self) {
        self.regs.r[6] = (self.product_to_bus40(0) >> 16) as u16;
    }

    pub fn mov_p0h_to_register(&mut self, b: RegName) {
        self.mov_p0h_to_bx(b);
    }

    pub fn mov_p0(&mut self, a: RegName) {
        let value = self.get_and_sat_acc(a) as u32;
        self.product_from_bus32(0, value);
    }

    pub fn mov_p1_to(&mut self, b: RegName) {
        let value = self.product_to_bus40(1);
        self.sat_and_set_acc_and_flag(b, value);
    }

    /// Write a word pair: `l` at the offset address, then `h` at the
    /// address itself, in that order (the second wins when they meet).
    fn write_pair_ar(&mut self, b: u16, bs: u16, h: u16, l: u16) {
        let unit = self.ar_rn_unit(b);
        let step = self.ar_step(bs);
        let address = self.rn_address_and_modify(unit, step, false);
        let offset = self.ar_offset(bs);
        let address2 = self.offset_address(unit, address, offset, false);
        self.data_write(address2, l);
        self.data_write(address, h);
    }

    /// Read a word pair: (the address, the offset address).
    fn read_pair_ar(&mut self, a: u16, as_: u16) -> (u16, u16) {
        let unit = self.ar_rn_unit(a);
        let step = self.ar_step(as_);
        let address = self.rn_address_and_modify(unit, step, false);
        let offset = self.ar_offset(as_);
        let address2 = self.offset_address(unit, address, offset, false);
        let l = self.data_read(address2);
        let h = self.data_read(address);
        (h, l)
    }

    pub fn mov2_px_arrn2_arstep2(&mut self, a: u16, b: u16, bs: u16) {
        let value = self.regs.p[a as usize];
        self.write_pair_ar(b, bs, (value >> 16) as u16, value as u16);
    }

    pub fn mov2s(&mut self, a: u16, b: u16, bs: u16) {
        let value = self.product_to_bus40(a as usize);
        self.write_pair_ar(b, bs, (value >> 16) as u16, value as u16);
    }

    pub fn mov2_arrn2_arstep2_px(&mut self, a: u16, as_: u16, b: u16) {
        let (h, l) = self.read_pair_ar(a, as_);
        self.product_from_bus32(b as usize, (h as u32) << 16 | l as u32);
    }

    pub fn mova_ab_arrn2_arstep2(&mut self, a: RegName, b: u16, bs: u16) {
        let value = self.get_and_sat_acc(a);
        self.write_pair_ar(b, bs, (value >> 16) as u16, value as u16);
    }

    pub fn mova_arrn2_arstep2_ab(&mut self, a: u16, as_: u16, b: RegName) {
        let (h, l) = self.read_pair_ar(a, as_);
        let value = sext64((h as u64) << 16 | l as u64, 32);
        self.sat_and_set_acc_and_flag(b, value);
    }

    pub fn mov_r6_to_bx(&mut self, b: RegName) {
        self.reg_from_bus16(b, self.regs.r[6]);
    }

    pub fn mov_r6_mixp(&mut self) {
        self.regs.mixp = self.regs.r[6];
    }

    pub fn mov_r6_to_register(&mut self, b: RegName) {
        self.reg_from_bus16(b, self.regs.r[6]);
    }

    pub fn mov_r6_register(&mut self, a: RegName) {
        self.regs.r[6] = self.reg_to_bus16(a, true);
    }

    pub fn mov_memsp_r6(&mut self) {
        self.regs.r[6] = self.data_read(self.regs.sp);
    }

    pub fn mov_r6_to_rn_stepzids(&mut self, b: u16, bs: StepValue) {
        let address = self.rn_address_and_modify(b as usize, bs, false);
        self.data_write(address, self.regs.r[6]);
    }

    pub fn mov_r6_rn_stepzids(&mut self, a: u16, as_: StepValue) {
        self.regs.r[6] = self.read_rn(a, as_);
    }

    pub fn mov2_axh_m_y0_m(&mut self, a: RegName, b: u16, bs: u16) {
        let u = (self.get_and_sat_acc_no_flag(a) >> 16) as u16;
        let v = self.regs.y[0];
        self.write_pair_ar(b, bs, u, v);
    }

    pub fn mov2_ax_mij(&mut self, a: RegName, b: u16, bsi: u16, bsj: u16) {
        let (_, _, i, j) = self.arp_pair(b, bsi, bsj);
        let value = self.get_and_sat_acc_no_flag(a);
        self.data_write(i, (value >> 16) as u16);
        self.data_write(j, value as u16);
    }

    pub fn mov2_ax_mji(&mut self, a: RegName, b: u16, bsi: u16, bsj: u16) {
        let (_, _, i, j) = self.arp_pair(b, bsi, bsj);
        let value = self.get_and_sat_acc_no_flag(a);
        self.data_write(j, (value >> 16) as u16);
        self.data_write(i, value as u16);
    }

    pub fn mov2_mij_ax(&mut self, a: u16, asi: u16, asj: u16, b: RegName) {
        let (ui, uj) = self.arp_rn_unit(a);
        let (si, sj) = self.arp_step(asi, asj);
        let addr = self.rn_address_and_modify(ui, si, false);
        let h = self.data_read(addr);
        let addr = self.rn_address_and_modify(uj, sj, false);
        let l = self.data_read(addr);
        self.set_acc(b, sext64((h as u64) << 16 | l as u64, 32));
    }

    pub fn mov2_mji_ax(&mut self, a: u16, asi: u16, asj: u16, b: RegName) {
        let (ui, uj) = self.arp_rn_unit(a);
        let (si, sj) = self.arp_step(asi, asj);
        let addr = self.rn_address_and_modify(ui, si, false);
        let l = self.data_read(addr);
        let addr = self.rn_address_and_modify(uj, sj, false);
        let h = self.data_read(addr);
        self.set_acc(b, sext64((h as u64) << 16 | l as u64, 32));
    }

    pub fn mov2_abh_m(&mut self, ax: RegName, ay: RegName, b: u16, bs: u16) {
        let u = (self.get_and_sat_acc_no_flag(ax) >> 16) as u16;
        let v = (self.get_and_sat_acc_no_flag(ay) >> 16) as u16;
        self.write_pair_ar(b, bs, u, v);
    }

    /// Exchange the high half of `a` with memory: it goes to `to_j` (j or
    /// i) and `a` loads from the other, rounded when `round`.
    fn exchange(&mut self, a: RegName, b: u16, bsi: u16, bsj: u16, to_j: bool, round: bool) {
        let (_, _, i, j) = self.arp_pair(b, bsi, bsj);
        let value = self.get_and_sat_acc_no_flag(a);
        let (to, from) = if to_j { (j, i) } else { (i, j) };
        self.data_write(to, (value >> 16) as u16);
        let loaded = (self.data_read(from) as u64) << 16 | if round { 0x8000 } else { 0 };
        self.set_acc(a, sext64(loaded, 32));
    }

    pub fn exchange_iaj(&mut self, a: RegName, b: u16, bsi: u16, bsj: u16) {
        self.exchange(a, b, bsi, bsj, true, false);
    }

    pub fn exchange_riaj(&mut self, a: RegName, b: u16, bsi: u16, bsj: u16) {
        self.exchange(a, b, bsi, bsj, true, true);
    }

    pub fn exchange_jai(&mut self, a: RegName, b: u16, bsi: u16, bsj: u16) {
        self.exchange(a, b, bsi, bsj, false, false);
    }

    pub fn exchange_rjai(&mut self, a: RegName, b: u16, bsi: u16, bsj: u16) {
        self.exchange(a, b, bsi, bsj, false, true);
    }

    /// Shift a 40-bit value by `sv` (positive left, negative right) into
    /// `dest`, arithmetic or logic as `s` says.
    fn shift_bus40(&mut self, value: u64, sv: u16, dest: RegName) {
        let mut value = value & 0xFF_FFFF_FFFF;
        let original_sign = value >> 39;
        if sv >> 15 == 0 {
            if sv >= 40 {
                if self.regs.s == 0 {
                    self.regs.fv = (value != 0) as u16;
                    if self.regs.fv != 0 {
                        self.regs.fvl = 1;
                    }
                }
                value = 0;
                self.regs.fc0 = 0;
            } else {
                if self.regs.s == 0 {
                    let fv = sext64(value, 40) != sext64(value, 40 - sv as u32);
                    self.regs.fv = fv as u16;
                    if fv {
                        self.regs.fvl = 1;
                    }
                }
                value <<= sv;
                self.regs.fc0 = (value & 1 << 40 != 0) as u16;
            }
        } else {
            let nsv = (!sv).wrapping_add(1);
            if nsv >= 40 {
                if self.regs.s == 0 {
                    self.regs.fc0 = (value >> 39 & 1) as u16;
                    value = if self.regs.fc0 != 0 {
                        0xFF_FFFF_FFFF
                    } else {
                        0
                    };
                } else {
                    value = 0;
                    self.regs.fc0 = 0;
                }
            } else {
                self.regs.fc0 = (value & 1 << (nsv - 1) != 0) as u16;
                value >>= nsv;
                if self.regs.s == 0 {
                    value = sext64(value, 40 - nsv as u32);
                }
            }
            if self.regs.s == 0 {
                self.regs.fv = 0;
            }
        }

        value = sext64(value, 40);
        self.set_acc_flag(value);
        if self.regs.s == 0
            && self.regs.sata == 0
            && (self.regs.fv != 0 || sext64(value, 32) != value)
        {
            self.regs.flm = 1;
            value = if original_sign == 1 {
                0xFFFF_FFFF_8000_0000
            } else {
                0x7FFF_FFFF
            };
        }
        self.set_acc(dest, value);
    }

    pub fn movs_memimm8_ab(&mut self, a: u16, b: RegName) {
        let value = sext64(self.load_mem8(a) as u64, 16);
        self.shift_bus40(value, self.regs.sv, b);
    }

    pub fn movs_rn_stepzids_ab(&mut self, a: u16, as_: StepValue, b: RegName) {
        let value = sext64(self.read_rn(a, as_) as u64, 16);
        self.shift_bus40(value, self.regs.sv, b);
    }

    pub fn movs_register_ab(&mut self, a: RegName, b: RegName) {
        let value = sext64(self.reg_to_bus16(a, false) as u64, 16);
        self.shift_bus40(value, self.regs.sv, b);
    }

    pub fn movs_r6_to(&mut self, b: RegName) {
        let value = sext64(self.regs.r[6] as u64, 16);
        self.shift_bus40(value, self.regs.sv, b);
    }

    pub fn movsi(&mut self, a: RegName, b: RegName, s: u16) {
        let value = sext64(self.reg_to_bus16(a, false) as u64, 16);
        self.shift_bus40(value, s, b);
    }

    /// A 16-bit value plus 0x8000 the odd way some movr forms do it: carry
    /// from bit 16, overflow always clear.
    fn round16(&mut self, value16: u16) -> u64 {
        let result = value16 as u64 + 0x8000;
        self.regs.fc0 = (result >> 16) as u16;
        self.regs.fv = 0;
        result & 0xFFFF
    }

    pub fn movr_arrn2_arstep2_abh(&mut self, a: u16, as_: u16, b: RegName) {
        let value16 = self.read_ar(a, as_);
        let value = sext64((value16 as u64) << 16, 32);
        let result = self.add_sub40(value, 0x8000, false);
        self.sat_and_set_acc_and_flag(b, result);
    }

    pub fn movr_rn_stepzids_ax(&mut self, a: u16, as_: StepValue, b: RegName) {
        let value16 = self.read_rn(a, as_);
        let result = self.round16(value16);
        self.sat_and_set_acc_and_flag(b, result);
    }

    pub fn movr_register_ax(&mut self, a: RegName, b: RegName) {
        let result = match a {
            RegName::A0 | RegName::A1 => {
                let value = self.get_acc(a);
                self.add_sub40(value, 0x8000, false)
            }
            RegName::P => {
                let value = self.product_to_bus40(0);
                self.add_sub40(value, 0x8000, false)
            }
            _ => {
                let value16 = self.reg_to_bus16(a, false);
                self.round16(value16)
            }
        };
        self.sat_and_set_acc_and_flag(b, result);
    }

    pub fn movr_bx_ax(&mut self, a: RegName, b: RegName) {
        let value = self.get_acc(a);
        let result = self.add_sub40(value, 0x8000, false);
        self.sat_and_set_acc_and_flag(b, result);
    }

    pub fn movr_r6_to(&mut self, b: RegName) {
        let result = self.round16(self.regs.r[6]);
        self.sat_and_set_acc_and_flag(b, result);
    }

    fn exp_of(value: u64) -> u16 {
        let sign = value >> 39 & 1;
        let mut count: u16 = 0;
        for bit in (0..=38).rev() {
            if value >> bit & 1 != sign {
                break;
            }
            count += 1;
        }
        count.wrapping_sub(8)
    }

    fn exp_store(&mut self, b: RegName) {
        self.set_acc(b, sext64(self.regs.sv as u64, 16));
    }

    pub fn exp_bx(&mut self, a: RegName) {
        self.regs.sv = Self::exp_of(self.get_acc(a));
    }

    pub fn exp_bx_ax(&mut self, a: RegName, b: RegName) {
        self.exp_bx(a);
        self.exp_store(b);
    }

    pub fn exp_rn_stepzids(&mut self, a: u16, as_: StepValue) {
        let value = sext64((self.read_rn(a, as_) as u64) << 16, 32);
        self.regs.sv = Self::exp_of(value);
    }

    pub fn exp_rn_stepzids_ax(&mut self, a: u16, as_: StepValue, b: RegName) {
        self.exp_rn_stepzids(a, as_);
        self.exp_store(b);
    }

    pub fn exp_register(&mut self, a: RegName) {
        let value = match a {
            RegName::A0 | RegName::A1 => self.get_acc(a),
            // p follows the usual rule.
            _ => sext64((self.reg_to_bus16(a, false) as u64) << 16, 32),
        };
        self.regs.sv = Self::exp_of(value);
    }

    pub fn exp_register_ax(&mut self, a: RegName, b: RegName) {
        self.exp_register(a);
        self.exp_store(b);
    }

    pub fn exp_r6(&mut self) {
        let value = sext64((self.regs.r[6] as u64) << 16, 32);
        self.regs.sv = Self::exp_of(value);
    }

    pub fn exp_r6_ax(&mut self, b: RegName) {
        self.exp_r6();
        self.exp_store(b);
    }

    pub fn lim(&mut self, a: RegName, b: RegName) {
        let value = self.get_acc(a);
        let value = self.saturate_acc(value);
        self.set_acc_and_flag(b, value);
    }

    pub fn vtrclr0(&mut self) {
        self.regs.vtr0 = 0;
    }

    pub fn vtrclr1(&mut self) {
        self.regs.vtr1 = 0;
    }

    pub fn vtrclr(&mut self) {
        self.regs.vtr0 = 0;
        self.regs.vtr1 = 0;
    }

    pub fn vtrmov0(&mut self, a: RegName) {
        self.sat_and_set_acc_and_flag(a, self.regs.vtr0 as u64);
    }

    pub fn vtrmov1(&mut self, a: RegName) {
        self.sat_and_set_acc_and_flag(a, self.regs.vtr1 as u64);
    }

    pub fn vtrmov(&mut self, a: RegName) {
        let value = self.regs.vtr1 & 0xFF00 | self.regs.vtr0 >> 8;
        self.sat_and_set_acc_and_flag(a, value as u64);
    }

    pub fn vtrshr(&mut self) {
        // On hardware vtr0 changes a cycle later; not modelled.
        self.regs.vtr0 = self.regs.vtr0 >> 1 | self.regs.fc0 << 15;
        self.regs.vtr1 = self.regs.vtr1 >> 1 | self.regs.fc1 << 15;
    }

    pub fn clrp0(&mut self) {
        self.product_from_bus32(0, 0);
    }

    pub fn clrp1(&mut self) {
        self.product_from_bus32(1, 0);
    }

    pub fn clrp(&mut self) {
        self.product_from_bus32(0, 0);
        self.product_from_bus32(1, 0);
    }

    /// The min/max family: compare `a` with `v`, keeping `v` and r0 in mixp
    /// when `keep` says so of the difference v - a.
    fn min_max(&mut self, a: RegName, v: u64, r0: u16, keep: impl Fn(u64) -> bool) {
        let u = self.get_acc(a);
        let d = v.wrapping_sub(u);
        if keep(d) {
            self.regs.fm = 1;
            self.regs.mixp = r0;
            self.set_acc(a, v);
        } else {
            self.regs.fm = 0;
        }
    }

    fn min_max_acc(&mut self, a: RegName, bs: StepValue, keep: impl Fn(u64) -> bool) {
        let v = self.get_acc(counter_acc(a));
        let r0 = self.rn_and_modify(0, bs, false);
        self.min_max(a, v, r0, keep);
    }

    fn min_max_r0(&mut self, a: RegName, bs: StepValue, keep: impl Fn(u64) -> bool) {
        let r0 = self.rn_and_modify(0, bs, false);
        let addr = self.rn_address(0, r0);
        let v = sext64(self.data_read(addr) as u64, 16);
        self.min_max(a, v, r0, keep);
    }

    pub fn max_ge(&mut self, a: RegName, bs: StepValue) {
        self.min_max_acc(a, bs, |d| d >> 63 == 0);
    }

    pub fn max_gt(&mut self, a: RegName, bs: StepValue) {
        self.min_max_acc(a, bs, |d| d >> 63 == 0 && d != 0);
    }

    pub fn min_le(&mut self, a: RegName, bs: StepValue) {
        self.min_max_acc(a, bs, |d| d >> 63 == 1 || d == 0);
    }

    pub fn min_lt(&mut self, a: RegName, bs: StepValue) {
        self.min_max_acc(a, bs, |d| d >> 63 == 1);
    }

    pub fn max_ge_r0(&mut self, a: RegName, bs: StepValue) {
        self.min_max_r0(a, bs, |d| d >> 63 == 0);
    }

    pub fn max_gt_r0(&mut self, a: RegName, bs: StepValue) {
        self.min_max_r0(a, bs, |d| d >> 63 == 0 && d != 0);
    }

    pub fn min_le_r0(&mut self, a: RegName, bs: StepValue) {
        self.min_max_r0(a, bs, |d| d >> 63 == 1 || d == 0);
    }

    pub fn min_lt_r0(&mut self, a: RegName, bs: StepValue) {
        self.min_max_r0(a, bs, |d| d >> 63 == 1);
    }

    pub fn divs(&mut self, a: u16, b: RegName) {
        let da = self.load_mem8(a) as u64;
        let db = self.get_acc(b);
        let value = db.wrapping_sub(da << 15);
        if value >> 63 != 0 {
            self.set_acc_and_flag(b, sext64(db << 1, 40));
        } else {
            self.set_acc_and_flag(b, sext64((value << 1).wrapping_add(1), 40));
        }
    }

    pub fn sqr_sqr_add3_ab_ab(&mut self, a: RegName, b: RegName) {
        let value = self.get_acc(a);
        self.product_sum(SumBase::Acc, b, false, false, false, false);
        self.regs.x[0] = (value >> 16) as u16;
        self.regs.y[0] = (value >> 16) as u16;
        self.regs.x[1] = value as u16;
        self.regs.y[1] = value as u16;
        self.do_multiplication(0, true, true);
        self.do_multiplication(1, true, true);
    }

    pub fn sqr_sqr_add3_arrn2_arstep2_ab(&mut self, a: u16, as_: u16, b: RegName) {
        self.product_sum(SumBase::Acc, b, false, false, false, false);
        let unit = self.ar_rn_unit(a);
        let step = self.ar_step(as_);
        let address0 = self.rn_address_and_modify(unit, step, false);
        let offset = self.ar_offset(as_);
        let address1 = self.offset_address(unit, address0, offset, false);
        let v0 = self.data_read(address0);
        let v1 = self.data_read(address1);
        self.regs.x[0] = v0;
        self.regs.y[0] = v0;
        self.regs.x[1] = v1;
        self.regs.y[1] = v1;
        self.do_multiplication(0, true, true);
        self.do_multiplication(1, true, true);
    }

    pub fn sqr_mpysu_add3a(&mut self, a: RegName, b: RegName) {
        let value = self.get_acc(a);
        self.product_sum(SumBase::Acc, b, false, false, false, true);
        let h = (value >> 16) as u16;
        self.regs.x[0] = h;
        self.regs.y[0] = h;
        self.regs.y[1] = h;
        self.regs.x[1] = value as u16;
        self.do_multiplication(0, true, true);
        self.do_multiplication(1, false, true);
    }

    fn compare(&mut self, va: u64, vb: u64) {
        let result = self.add_sub40(vb, va, true);
        self.set_acc_flag(result);
    }

    pub fn cmp_ax_bx(&mut self, a: RegName, b: RegName) {
        self.compare(self.get_acc(a), self.get_acc(b));
    }

    pub fn cmp_b0_b1(&mut self) {
        self.compare(self.regs.b[0], self.regs.b[1]);
    }

    pub fn cmp_b1_b0(&mut self) {
        self.compare(self.regs.b[1], self.regs.b[0]);
    }

    pub fn cmp_bx_ax(&mut self, a: RegName, b: RegName) {
        self.compare(self.get_acc(a), self.get_acc(b));
    }

    pub fn cmp_p1_to(&mut self, b: RegName) {
        self.compare(self.product_to_bus40(1), self.get_acc(b));
    }

    fn min_max_vtr(&mut self, a: RegName, b: RegName, min: bool) {
        let u = self.get_acc(a);
        let v = self.get_acc(b);
        let uh = sext64(u >> 16, 24);
        let ul = sext64(u & 0xFFFF, 16);
        let vh = sext64(v >> 16, 24);
        let vl = sext64(v & 0xFFFF, 16);
        let wh = if min {
            uh.wrapping_sub(vh)
        } else {
            vh.wrapping_sub(uh)
        };
        let wl = if min {
            ul.wrapping_sub(vl)
        } else {
            vl.wrapping_sub(ul)
        };
        self.regs.fc0 = (wh >> 63 == 0) as u16;
        self.regs.fc1 = (wl >> 63 == 0) as u16;
        let wh = if self.regs.fc0 != 0 { vh } else { uh };
        let wl = if self.regs.fc1 != 0 { vl } else { ul };
        self.set_acc(a, wh << 16 | wl & 0xFFFF);
        self.vtrshr();
    }

    pub fn max2_vtr_ax(&mut self, a: RegName) {
        self.min_max_vtr(a, counter_acc(a), false);
    }

    pub fn min2_vtr_ax(&mut self, a: RegName) {
        self.min_max_vtr(a, counter_acc(a), true);
    }

    pub fn max2_vtr_ax_bx(&mut self, a: RegName, b: RegName) {
        self.min_max_vtr(a, b, false);
    }

    pub fn min2_vtr_ax_bx(&mut self, a: RegName, b: RegName) {
        self.min_max_vtr(a, b, true);
    }

    /// min/max then store a half of the other accumulator.
    fn min_max_vtr_mov(&mut self, a: RegName, b: RegName, min: bool, c: u16, cs: u16, high: bool) {
        self.min_max_vtr(a, b, min);
        let value = self.get_and_sat_acc_no_flag(counter_acc(a));
        let address = self.ar_address(c, cs);
        let value16 = if high { value >> 16 } else { value } as u16;
        self.data_write(address, value16);
    }

    pub fn max2_vtr_movl_ax_bx_arrn1_arstep1(&mut self, a: RegName, b: RegName, c: u16, cs: u16) {
        self.min_max_vtr_mov(a, b, false, c, cs, false);
    }

    pub fn max2_vtr_movh_ax_bx_arrn1_arstep1(&mut self, a: RegName, b: RegName, c: u16, cs: u16) {
        self.min_max_vtr_mov(a, b, false, c, cs, true);
    }

    pub fn max2_vtr_movl_bx_ax_arrn1_arstep1(&mut self, a: RegName, b: RegName, c: u16, cs: u16) {
        self.min_max_vtr_mov(a, b, false, c, cs, false);
    }

    pub fn max2_vtr_movh_bx_ax_arrn1_arstep1(&mut self, a: RegName, b: RegName, c: u16, cs: u16) {
        self.min_max_vtr_mov(a, b, false, c, cs, true);
    }

    pub fn min2_vtr_movl_ax_bx_arrn1_arstep1(&mut self, a: RegName, b: RegName, c: u16, cs: u16) {
        self.min_max_vtr_mov(a, b, true, c, cs, false);
    }

    pub fn min2_vtr_movh_ax_bx_arrn1_arstep1(&mut self, a: RegName, b: RegName, c: u16, cs: u16) {
        self.min_max_vtr_mov(a, b, true, c, cs, true);
    }

    pub fn min2_vtr_movl_bx_ax_arrn1_arstep1(&mut self, a: RegName, b: RegName, c: u16, cs: u16) {
        self.min_max_vtr_mov(a, b, true, c, cs, false);
    }

    pub fn min2_vtr_movh_bx_ax_arrn1_arstep1(&mut self, a: RegName, b: RegName, c: u16, cs: u16) {
        self.min_max_vtr_mov(a, b, true, c, cs, true);
    }

    /// min/max then store the other accumulator through an arp pair, high
    /// half to i (`ij`) or to j.
    fn min_max_vtr_pair(
        &mut self,
        a: RegName,
        b: RegName,
        min: bool,
        c: u16,
        csi: u16,
        csj: u16,
        ij: bool,
    ) {
        self.min_max_vtr(a, b, min);
        let value = self.get_and_sat_acc_no_flag(counter_acc(a));
        let (h, l) = ((value >> 16) as u16, value as u16);
        let (_, _, i, j) = self.arp_pair(c, csi, csj);
        if ij {
            self.data_write(i, h);
            self.data_write(j, l);
        } else {
            self.data_write(i, l);
            self.data_write(j, h);
        }
    }

    pub fn max2_vtr_movij(&mut self, a: RegName, b: RegName, c: u16, csi: u16, csj: u16) {
        self.min_max_vtr_pair(a, b, false, c, csi, csj, true);
    }

    pub fn max2_vtr_movji(&mut self, a: RegName, b: RegName, c: u16, csi: u16, csj: u16) {
        self.min_max_vtr_pair(a, b, false, c, csi, csj, false);
    }

    pub fn min2_vtr_movij(&mut self, a: RegName, b: RegName, c: u16, csi: u16, csj: u16) {
        self.min_max_vtr_pair(a, b, true, c, csi, csj, true);
    }

    pub fn min2_vtr_movji(&mut self, a: RegName, b: RegName, c: u16, csi: u16, csj: u16) {
        self.min_max_vtr_pair(a, b, true, c, csi, csj, false);
    }

    pub fn mov_sv_app_arrn1_arstep1_bx(
        &mut self,
        a: u16,
        as_: u16,
        b: RegName,
        base: SumBase,
        sp0: bool,
        a0: bool,
        sp1: bool,
        a1: bool,
    ) {
        self.regs.sv = self.read_ar(a, as_);
        self.product_sum(base, b, sp0, a0, sp1, a1);
    }

    pub fn mov_sv_app_arrn1_arstep1alt_bx(
        &mut self,
        a: u16,
        as_: u16,
        b: RegName,
        base: SumBase,
        sp0: bool,
        a0: bool,
        sp1: bool,
        a1: bool,
    ) {
        self.mov_sv_app_arrn1_arstep1_bx(a, as_, b, base, sp0, a0, sp1, a1);
    }

    fn codebook_search(&mut self, u: u16, v: u16, r: u16, gt: bool) {
        let diff = self
            .product_to_bus40(0)
            .wrapping_sub(self.product_to_bus40(1));
        let cond = diff >> 63 == 0 && (!gt || diff != 0);
        if cond {
            self.regs.x[1] = self.regs.p0h_cbs;
            self.regs.x[0] = self.regs.y[1];
            self.regs.mixp = r;
        }
        self.regs.y[0] = u;
        let x0 = std::mem::replace(&mut self.regs.x[0], self.regs.y[0]);
        self.do_multiplication(0, true, true);
        let p0h = (self.product_to_bus40(0) >> 16) as u16;
        self.regs.p0h_cbs = p0h;
        self.regs.y[0] = p0h;
        self.regs.x[0] = x0;
        self.regs.y[1] = v;
        self.do_multiplication(0, true, true);
        self.do_multiplication(1, true, true);
    }

    pub fn cbs_axh(&mut self, a: RegName, gt: bool) {
        let u = (self.get_acc(a) >> 16) as u16;
        let v = (self.get_acc(counter_acc(a)) >> 16) as u16;
        self.codebook_search(u, v, self.regs.r[0], gt);
    }

    pub fn cbs_axh_bxh(&mut self, a: RegName, b: RegName, gt: bool) {
        let u = (self.get_acc(a) >> 16) as u16;
        let v = (self.get_acc(b) >> 16) as u16;
        self.codebook_search(u, v, self.regs.r[0], gt);
    }

    pub fn cbs_arprn1_arpstep1_arpstep1(&mut self, a: u16, asi: u16, asj: u16, gt: bool) {
        let (ui, uj) = self.arp_rn_unit(a);
        let (si, sj) = self.arp_step(asi, asj);
        let aip = self.rn_and_modify(ui, si, false);
        let ai = self.rn_address(ui, aip);
        let aj = self.rn_address_and_modify(uj, sj, false);
        let u = self.data_read(ai);
        let v = self.data_read(aj);
        self.codebook_search(u, v, aip, gt);
    }

    fn mma_tail(&mut self, x0_sign: bool, y0_sign: bool, x1_sign: bool, y1_sign: bool) {
        self.do_multiplication(0, x0_sign, y0_sign);
        self.do_multiplication(1, x1_sign, y1_sign);
    }

    fn mma_reg(
        &mut self,
        a: RegName,
        signs: [bool; 4],
        base: SumBase,
        sp0: bool,
        a0: bool,
        sp1: bool,
        a1: bool,
    ) {
        self.product_sum(base, a, sp0, a0, sp1, a1);
        self.regs.x.swap(0, 1);
        self.mma_tail(signs[0], signs[1], signs[2], signs[3]);
    }

    pub fn mma_ax(
        &mut self,
        a: RegName,
        x0s: bool,
        y0s: bool,
        x1s: bool,
        y1s: bool,
        base: SumBase,
        sp0: bool,
        a0: bool,
        sp1: bool,
        a1: bool,
    ) {
        self.mma_reg(a, [x0s, y0s, x1s, y1s], base, sp0, a0, sp1, a1);
    }

    pub fn mma_bx(
        &mut self,
        a: RegName,
        x0s: bool,
        y0s: bool,
        x1s: bool,
        y1s: bool,
        base: SumBase,
        sp0: bool,
        a0: bool,
        sp1: bool,
        a1: bool,
    ) {
        self.mma_reg(a, [x0s, y0s, x1s, y1s], base, sp0, a0, sp1, a1);
    }

    pub fn mma_ab(
        &mut self,
        a: RegName,
        x0s: bool,
        y0s: bool,
        x1s: bool,
        y1s: bool,
        base: SumBase,
        sp0: bool,
        a0: bool,
        sp1: bool,
        a1: bool,
    ) {
        self.mma_reg(a, [x0s, y0s, x1s, y1s], base, sp0, a0, sp1, a1);
    }

    fn mma_arp(
        &mut self,
        xy: u16,
        i: u16,
        j: u16,
        dmodi: bool,
        dmodj: bool,
        a: RegName,
        signs: [bool; 4],
        sum: (SumBase, bool, bool, bool, bool),
    ) {
        let (base, sp0, a0, sp1, a1) = sum;
        self.product_sum(base, a, sp0, a0, sp1, a1);
        let (ui, uj) = self.arp_rn_unit(xy);
        let (si, sj) = self.arp_step(i, j);
        let (oi, oj) = self.arp_offset(i, j);
        let x = self.rn_address_and_modify(ui, si, dmodi);
        let y = self.rn_address_and_modify(uj, sj, dmodj);
        self.regs.x[0] = self.data_read(x);
        self.regs.y[0] = self.data_read(y);
        let x1 = self.offset_address(ui, x, oi, dmodi);
        self.regs.x[1] = self.data_read(x1);
        let y1 = self.offset_address(uj, y, oj, dmodj);
        self.regs.y[1] = self.data_read(y1);
        self.mma_tail(signs[0], signs[1], signs[2], signs[3]);
    }

    pub fn mma_arprn1_arpstep1_arpstep1_ab(
        &mut self,
        xy: u16,
        i: u16,
        j: u16,
        dmodi: bool,
        dmodj: bool,
        a: RegName,
        x0s: bool,
        y0s: bool,
        x1s: bool,
        y1s: bool,
        base: SumBase,
        sp0: bool,
        a0: bool,
        sp1: bool,
        a1: bool,
    ) {
        let signs = [x0s, y0s, x1s, y1s];
        self.mma_arp(xy, i, j, dmodi, dmodj, a, signs, (base, sp0, a0, sp1, a1));
    }

    pub fn mma_arprn1_arpstep1_arpstep1_ax(
        &mut self,
        xy: u16,
        i: u16,
        j: u16,
        dmodi: bool,
        dmodj: bool,
        a: RegName,
        x0s: bool,
        y0s: bool,
        x1s: bool,
        y1s: bool,
        base: SumBase,
        sp0: bool,
        a0: bool,
        sp1: bool,
        a1: bool,
    ) {
        let signs = [x0s, y0s, x1s, y1s];
        self.mma_arp(xy, i, j, dmodi, dmodj, a, signs, (base, sp0, a0, sp1, a1));
    }

    pub fn mma_arprn2_arpstep2_arpstep2_ab(
        &mut self,
        xy: u16,
        i: u16,
        j: u16,
        dmodi: bool,
        dmodj: bool,
        a: RegName,
        x0s: bool,
        y0s: bool,
        x1s: bool,
        y1s: bool,
        base: SumBase,
        sp0: bool,
        a0: bool,
        sp1: bool,
        a1: bool,
    ) {
        let signs = [x0s, y0s, x1s, y1s];
        self.mma_arp(xy, i, j, dmodi, dmodj, a, signs, (base, sp0, a0, sp1, a1));
    }

    pub fn mma_mx_xy(
        &mut self,
        y: u16,
        ys: u16,
        a: RegName,
        x0s: bool,
        y0s: bool,
        x1s: bool,
        y1s: bool,
        base: SumBase,
        sp0: bool,
        a0: bool,
        sp1: bool,
        a1: bool,
    ) {
        self.product_sum(base, a, sp0, a0, sp1, a1);
        self.regs.x.swap(0, 1);
        self.regs.y[0] = self.read_ar(y, ys);
        self.mma_tail(x0s, y0s, x1s, y1s);
    }

    pub fn mma_xy_mx(
        &mut self,
        y: u16,
        ys: u16,
        a: RegName,
        x0s: bool,
        y0s: bool,
        x1s: bool,
        y1s: bool,
        base: SumBase,
        sp0: bool,
        a0: bool,
        sp1: bool,
        a1: bool,
    ) {
        self.product_sum(base, a, sp0, a0, sp1, a1);
        self.regs.x.swap(0, 1);
        self.regs.y[1] = self.read_ar(y, ys);
        self.mma_tail(x0s, y0s, x1s, y1s);
    }

    pub fn mma_my_my(
        &mut self,
        x: u16,
        xs: u16,
        a: RegName,
        x0s: bool,
        y0s: bool,
        x1s: bool,
        y1s: bool,
        base: SumBase,
        sp0: bool,
        a0: bool,
        sp1: bool,
        a1: bool,
    ) {
        self.product_sum(base, a, sp0, a0, sp1, a1);
        let (h, l) = {
            let unit = self.ar_rn_unit(x);
            let step = self.ar_step(xs);
            let address = self.rn_address_and_modify(unit, step, false);
            let offset = self.ar_offset(xs);
            let address2 = self.offset_address(unit, address, offset, false);
            (self.data_read(address), self.data_read(address2))
        };
        self.regs.x[0] = h;
        self.regs.x[1] = l;
        self.mma_tail(x0s, y0s, x1s, y1s);
    }

    fn mma_mov_generic(
        &mut self,
        u: RegName,
        v: RegName,
        w: u16,
        ws: u16,
        a: RegName,
        signs: [bool; 4],
        sum: (SumBase, bool, bool, bool, bool),
    ) {
        let unit = self.ar_rn_unit(w);
        let step = self.ar_step(ws);
        let address = self.rn_address_and_modify(unit, step, false);
        let u_value = (self.get_and_sat_acc_no_flag(u) >> 16) as u16;
        let v_value = (self.get_and_sat_acc_no_flag(v) >> 16) as u16;
        let offset = self.ar_offset(ws);
        let address2 = self.offset_address(unit, address, offset, false);
        self.data_write(address2, v_value);
        self.data_write(address, u_value);
        let (base, sp0, a0, sp1, a1) = sum;
        self.product_sum(base, a, sp0, a0, sp1, a1);
        self.regs.x.swap(0, 1);
        self.mma_tail(signs[0], signs[1], signs[2], signs[3]);
    }

    pub fn mma_mov_axh_bxh_arrn1_arstep1_ab(
        &mut self,
        u: RegName,
        v: RegName,
        w: u16,
        ws: u16,
        a: RegName,
        x0s: bool,
        y0s: bool,
        x1s: bool,
        y1s: bool,
        base: SumBase,
        sp0: bool,
        a0: bool,
        sp1: bool,
        a1: bool,
    ) {
        let signs = [x0s, y0s, x1s, y1s];
        self.mma_mov_generic(u, v, w, ws, a, signs, (base, sp0, a0, sp1, a1));
    }

    pub fn mma_mov_axh_bxh_arrn1_arstep1_ax(
        &mut self,
        u: RegName,
        v: RegName,
        w: u16,
        ws: u16,
        a: RegName,
        x0s: bool,
        y0s: bool,
        x1s: bool,
        y1s: bool,
        base: SumBase,
        sp0: bool,
        a0: bool,
        sp1: bool,
        a1: bool,
    ) {
        let signs = [x0s, y0s, x1s, y1s];
        self.mma_mov_generic(u, v, w, ws, a, signs, (base, sp0, a0, sp1, a1));
    }

    pub fn mma_mov_arrn2_arstep1_ax(
        &mut self,
        w: u16,
        ws: u16,
        a: RegName,
        x0s: bool,
        y0s: bool,
        x1s: bool,
        y1s: bool,
        base: SumBase,
        sp0: bool,
        a0: bool,
        sp1: bool,
        a1: bool,
    ) {
        let signs = [x0s, y0s, x1s, y1s];
        self.mma_mov_generic(a, counter_acc(a), w, ws, a, signs, (base, sp0, a0, sp1, a1));
    }

    pub fn addhp(&mut self, a: u16, as_: u16, b: u16, c: RegName) {
        let word = self.read_ar(a, as_);
        let value = sext64((word as u64) << 16 | 0x8000, 32);
        let p = self.product_to_bus40(b as usize);
        let result = self.add_sub40(value, p, false);
        self.sat_and_set_acc_and_flag(c, result);
    }
}
