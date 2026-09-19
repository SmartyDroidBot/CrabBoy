//! Register file, modes, exceptions and the fetch loop.

use crate::bus::{Abort, Bus};
use crate::vfp::Vfp;
use crate::{arm, thumb};

/// Program status register bits.
pub mod psr {
    pub const N: u32 = 1 << 31;
    pub const Z: u32 = 1 << 30;
    pub const C: u32 = 1 << 29;
    pub const V: u32 = 1 << 28;
    pub const Q: u32 = 1 << 27;
    /// The four `GE` flags of the ARMv6 parallel arithmetic.
    pub const GE: u32 = 0xF << 16;
    pub const GE0: u32 = 1 << 16;
    /// Data big-endian (ARMv6).
    pub const E: u32 = 1 << 9;
    /// Imprecise abort mask (ARMv6).
    pub const A: u32 = 1 << 8;
    pub const I: u32 = 1 << 7;
    pub const F: u32 = 1 << 6;
    pub const T: u32 = 1 << 5;
    pub const MODE: u32 = 0x1F;
}

/// Processor modes, by their CPSR encoding.
pub mod mode {
    pub const USR: u32 = 0x10;
    pub const FIQ: u32 = 0x11;
    pub const IRQ: u32 = 0x12;
    pub const SVC: u32 = 0x13;
    pub const ABT: u32 = 0x17;
    pub const UND: u32 = 0x1B;
    pub const SYS: u32 = 0x1F;
}

/// The architecture a [`Cpu`] implements.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    /// ARMv5TE: the ARM946E-S of the 3DS.
    V5te,
    /// ARMv6K: the ARM11 MPCore of the 3DS.
    V6k,
}

/// The exceptions, in vector order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Exception {
    Reset,
    Undefined,
    Supervisor,
    PrefetchAbort,
    DataAbort,
    Irq,
    Fiq,
}

impl Exception {
    fn vector(self) -> u32 {
        match self {
            Exception::Reset => 0x00,
            Exception::Undefined => 0x04,
            Exception::Supervisor => 0x08,
            Exception::PrefetchAbort => 0x0C,
            Exception::DataAbort => 0x10,
            Exception::Irq => 0x18,
            Exception::Fiq => 0x1C,
        }
    }

    fn mode(self) -> u32 {
        match self {
            Exception::Reset | Exception::Supervisor => mode::SVC,
            Exception::Undefined => mode::UND,
            Exception::PrefetchAbort | Exception::DataAbort => mode::ABT,
            Exception::Irq => mode::IRQ,
            Exception::Fiq => mode::FIQ,
        }
    }
}

/// Why an instruction did not complete.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Trap {
    Undefined,
    Supervisor,
    Breakpoint,
    DataAbort,
}

impl From<Abort> for Trap {
    fn from(_: Abort) -> Trap {
        Trap::DataAbort
    }
}

pub(crate) type Exec = Result<u32, Trap>;

const BANK_USR: usize = 0;
const BANK_FIQ: usize = 1;

fn bank_of(mode_bits: u32) -> usize {
    match mode_bits & psr::MODE {
        mode::FIQ => BANK_FIQ,
        mode::IRQ => 2,
        mode::SVC => 3,
        mode::ABT => 4,
        mode::UND => 5,
        _ => BANK_USR,
    }
}

/// One ARM processor.
pub struct Cpu {
    arch: Arch,
    /// `r[15]` is the address of the next instruction to fetch.
    pub(crate) r: [u32; 16],
    /// What an instruction reads from `r15`: its own address plus 8 in ARM
    /// state or plus 4 in Thumb state.
    pub(crate) pc_read: u32,
    cpsr: u32,
    banked_sp_lr: [[u32; 2]; 6],
    banked_r8_r12: [[u32; 5]; 2],
    spsr: [u32; 6],
    /// Level of the IRQ input.
    pub irq_line: bool,
    /// Level of the FIQ input.
    pub fiq_line: bool,
    halted: bool,
    /// Exceptions taken so far, in vector order, for diagnostics.
    taken: [u64; 7],
    /// The floating-point unit of an ARMv6K processor.
    pub(crate) vfp: Vfp,
}

impl Cpu {
    /// A processor in its reset state: supervisor mode, interrupts masked,
    /// ARM state, about to fetch the reset vector.
    pub fn new<B: Bus>(arch: Arch, bus: &B) -> Self {
        let mut cpu = Cpu {
            arch,
            r: [0; 16],
            pc_read: 0,
            cpsr: mode::SVC | psr::I | psr::F,
            banked_sp_lr: [[0; 2]; 6],
            banked_r8_r12: [[0; 5]; 2],
            spsr: [0; 6],
            irq_line: false,
            fiq_line: false,
            halted: false,
            taken: [0; 7],
            vfp: Vfp::default(),
        };
        cpu.r[15] = cpu.vector_base(bus);
        cpu
    }

    pub fn arch(&self) -> Arch {
        self.arch
    }

    pub fn vfp(&self) -> &Vfp {
        &self.vfp
    }

    pub fn vfp_mut(&mut self) -> &mut Vfp {
        &mut self.vfp
    }

    pub(crate) fn v6(&self) -> bool {
        self.arch == Arch::V6k
    }

    /// The stack pointer of `mode_bits`, whatever the current mode.
    pub(crate) fn banked_sp(&self, mode_bits: u32) -> u32 {
        if bank_of(mode_bits) == bank_of(self.cpsr) {
            self.r[13]
        } else {
            self.banked_sp_lr[bank_of(mode_bits)][0]
        }
    }

    pub(crate) fn set_banked_sp(&mut self, mode_bits: u32, value: u32) {
        if bank_of(mode_bits) == bank_of(self.cpsr) {
            self.r[13] = value;
        } else {
            self.banked_sp_lr[bank_of(mode_bits)][0] = value;
        }
    }

    pub fn cpsr(&self) -> u32 {
        self.cpsr
    }

    pub fn thumb(&self) -> bool {
        self.cpsr & psr::T != 0
    }

    pub fn privileged(&self) -> bool {
        self.cpsr & psr::MODE != mode::USR
    }

    pub fn halted(&self) -> bool {
        self.halted
    }

    /// How many times each exception was taken, in vector order: reset,
    /// undefined, supervisor call, prefetch abort, data abort, IRQ, FIQ.
    pub fn exceptions_taken(&self) -> [u64; 7] {
        self.taken
    }

    /// Stop executing until an interrupt line is raised.
    pub fn halt(&mut self) {
        self.halted = true;
    }

    /// A general register as the debugger sees it; `15` is the address of the
    /// next instruction.
    pub fn reg(&self, n: usize) -> u32 {
        self.r[n]
    }

    pub fn set_reg(&mut self, n: usize, value: u32) {
        self.r[n] = value;
    }

    /// Start executing at `addr`; bit 0 selects Thumb state.
    pub fn jump(&mut self, addr: u32) {
        self.branch_exchange(addr);
    }

    pub fn spsr(&self) -> u32 {
        self.spsr[bank_of(self.cpsr)]
    }

    pub(crate) fn set_spsr(&mut self, value: u32) {
        let bank = bank_of(self.cpsr);
        if bank != BANK_USR {
            self.spsr[bank] = value;
        }
    }

    pub(crate) fn has_spsr(&self) -> bool {
        bank_of(self.cpsr) != BANK_USR
    }

    /// A register as an executing instruction reads it.
    #[inline]
    pub(crate) fn get(&self, n: u32) -> u32 {
        if n == 15 {
            self.pc_read
        } else {
            self.r[n as usize]
        }
    }

    #[inline]
    pub(crate) fn flag(&self, bit: u32) -> bool {
        self.cpsr & bit != 0
    }

    #[inline]
    pub(crate) fn set_flag(&mut self, bit: u32, on: bool) {
        if on {
            self.cpsr |= bit;
        } else {
            self.cpsr &= !bit;
        }
    }

    #[inline]
    pub(crate) fn set_nz(&mut self, result: u32) {
        self.cpsr =
            self.cpsr & !(psr::N | psr::Z) | result & psr::N | if result == 0 { psr::Z } else { 0 };
    }

    #[inline]
    pub(crate) fn set_nz64(&mut self, result: u64) {
        self.set_flag(psr::N, result >> 63 != 0);
        self.set_flag(psr::Z, result == 0);
    }

    /// Replace the whole CPSR, switching register banks as needed.
    pub fn set_cpsr(&mut self, value: u32) {
        let old = bank_of(self.cpsr);
        let new = bank_of(value);
        if old != new {
            self.banked_sp_lr[old] = [self.r[13], self.r[14]];
            [self.r[13], self.r[14]] = self.banked_sp_lr[new];
            if old == BANK_FIQ || new == BANK_FIQ {
                let (save, load) = if old == BANK_FIQ { (1, 0) } else { (0, 1) };
                self.banked_r8_r12[save].copy_from_slice(&self.r[8..13]);
                let loaded = self.banked_r8_r12[load];
                self.r[8..13].copy_from_slice(&loaded);
            }
        }
        self.cpsr = value;
    }

    /// A user-mode register, whatever the current mode (`LDM`/`STM` with `S`).
    pub(crate) fn user_reg(&self, n: u32) -> u32 {
        let bank = bank_of(self.cpsr);
        match n {
            8..=12 if bank == BANK_FIQ => self.banked_r8_r12[0][n as usize - 8],
            13 | 14 if bank != BANK_USR => self.banked_sp_lr[BANK_USR][n as usize - 13],
            _ => self.get(n),
        }
    }

    pub(crate) fn set_user_reg(&mut self, n: u32, value: u32) {
        let bank = bank_of(self.cpsr);
        match n {
            8..=12 if bank == BANK_FIQ => self.banked_r8_r12[0][n as usize - 8] = value,
            13 | 14 if bank != BANK_USR => self.banked_sp_lr[BANK_USR][n as usize - 13] = value,
            _ => self.r[n as usize] = value,
        }
    }

    /// Branch without changing state.
    #[inline]
    pub(crate) fn branch(&mut self, addr: u32) {
        self.r[15] = if self.thumb() { addr & !1 } else { addr & !3 };
    }

    /// Branch, taking the state from bit 0 of the target.
    #[inline]
    pub(crate) fn branch_exchange(&mut self, addr: u32) {
        if addr & 1 != 0 {
            self.cpsr |= psr::T;
            self.r[15] = addr & !1;
        } else {
            self.cpsr &= !psr::T;
            self.r[15] = addr & !3;
        }
    }

    /// Return from an exception: `CPSR = SPSR`, then branch in the restored
    /// state.
    pub(crate) fn exception_return(&mut self, addr: u32) {
        if self.has_spsr() {
            let spsr = self.spsr();
            self.set_cpsr(spsr);
        }
        self.branch(addr);
    }

    fn vector_base<B: Bus>(&self, bus: &B) -> u32 {
        if bus.high_vectors() {
            0xFFFF_0000
        } else {
            0
        }
    }

    /// Enter `exception`. `return_addr` goes to the link register of the new
    /// mode, already adjusted the way that exception's handler expects.
    pub fn enter<B: Bus>(&mut self, bus: &B, exception: Exception, return_addr: u32) {
        self.taken[exception as usize] += 1;
        let old = self.cpsr;
        let mut new = old & !(psr::MODE | psr::T) | exception.mode() | psr::I;
        if matches!(exception, Exception::Reset | Exception::Fiq) {
            new |= psr::F;
        }
        if self.v6() {
            // ARMv6 masks imprecise aborts on everything but undefined
            // instructions and supervisor calls, and enters little-endian.
            if !matches!(exception, Exception::Undefined | Exception::Supervisor) {
                new |= psr::A;
            }
            new &= !psr::E;
        }
        self.set_cpsr(new);
        self.set_spsr(old);
        self.r[14] = return_addr;
        self.r[15] = self.vector_base(bus) + exception.vector();
        self.halted = false;
    }

    pub(crate) fn condition(&self, cond: u32) -> bool {
        let n = self.flag(psr::N);
        let z = self.flag(psr::Z);
        let c = self.flag(psr::C);
        let v = self.flag(psr::V);
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

    /// Run one instruction, or take one exception, and return the cycles it
    /// cost (see `docs/3ds/clocks.md` for the cost model).
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> u32 {
        if self.fiq_line && !self.flag(psr::F) {
            self.enter(bus, Exception::Fiq, self.r[15] + 4);
            return 3;
        }
        if self.irq_line && !self.flag(psr::I) {
            self.enter(bus, Exception::Irq, self.r[15] + 4);
            return 3;
        }
        if self.halted {
            if !(self.irq_line || self.fiq_line) {
                return 1;
            }
            // A masked interrupt still ends the wait; execution continues.
            self.halted = false;
        }

        let addr = self.r[15];
        let privileged = self.privileged();
        let result = if self.thumb() {
            match bus.fetch16(addr, privileged) {
                Ok(instr) => {
                    self.r[15] = addr.wrapping_add(2);
                    self.pc_read = addr.wrapping_add(4);
                    thumb::execute(self, bus, instr as u32)
                }
                Err(Abort) => {
                    self.enter(bus, Exception::PrefetchAbort, addr.wrapping_add(4));
                    return 3;
                }
            }
        } else {
            match bus.fetch32(addr, privileged) {
                Ok(instr) => {
                    self.r[15] = addr.wrapping_add(4);
                    self.pc_read = addr.wrapping_add(8);
                    arm::execute(self, bus, instr)
                }
                Err(Abort) => {
                    self.enter(bus, Exception::PrefetchAbort, addr.wrapping_add(4));
                    return 3;
                }
            }
        };

        match result {
            Ok(cycles) => cycles,
            Err(trap) => {
                // The link register conventions of the ARM ARM: the next
                // instruction for undefined and supervisor calls, the
                // faulting one plus four for a prefetch abort, plus eight for
                // a data abort.
                let next = self.r[15];
                let (exception, lr) = match trap {
                    Trap::Undefined => (Exception::Undefined, next),
                    Trap::Supervisor => (Exception::Supervisor, next),
                    Trap::Breakpoint => (Exception::PrefetchAbort, addr.wrapping_add(4)),
                    Trap::DataAbort => (Exception::DataAbort, addr.wrapping_add(8)),
                };
                self.enter(bus, exception, lr);
                3
            }
        }
    }
}
