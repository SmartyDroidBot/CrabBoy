//! ARM7TDMI CPU core.
//!
//! The GBA's CPU is an ARM7TDMI running in little-endian mode. It executes the
//! 32-bit ARM instruction set (in `arm` mode) and the 16-bit Thumb instruction
//! set (in `thumb` mode). This module provides the register file, mode/banking
//! logic, CPSR/SPSR handling and exception entry; the two instruction decoders
//! live in [`arm`] and [`thumb`].
//!
//! The core is bus-agnostic in the sense that it talks to memory through the
//! `Bus` passed to [`Cpu::execute`]; the bus owns the address space and returns
//! the number of cycles each access consumed so the CPU can accumulate its
//! cycle count.

pub mod arm;
pub mod thumb;

/// ARM processor modes.
pub mod mode {
    pub const USR: u32 = 0x10;
    pub const FIQ: u32 = 0x11;
    pub const IRQ: u32 = 0x12;
    pub const SVC: u32 = 0x13;
    pub const ABT: u32 = 0x17;
    pub const UND: u32 = 0x1B;
}

pub mod flag {
    pub const N: u32 = 1 << 31;
    pub const Z: u32 = 1 << 30;
    pub const C: u32 = 1 << 29;
    pub const V: u32 = 1 << 28;
    pub const I: u32 = 1 << 7;
    pub const F: u32 = 1 << 6;
    pub const T: u32 = 1 << 5;
}

/// CPSR/SPSR field masks.
pub mod field {
    /// Condition-code flags (bits 31..28).
    pub const FLAGS: u32 = 0xF000_0000;
    /// Interrupt-disable + mode bits that `MSR` with the `fc` mask may write.
    pub const CONTROL: u32 = 0x0000_00FF;
}

/// All instruction addresses are 32-bit; there is no high-VRAM vector.
pub const VECTOR_UND: u32 = 0x0000_0004;
pub const VECTOR_SWI: u32 = 0x0000_0008;
pub const VECTOR_ABT_PREFETCH: u32 = 0x0000_000C;
pub const VECTOR_ABT_DATA: u32 = 0x0000_0010;
pub const VECTOR_IRQ: u32 = 0x0000_0018;
pub const VECTOR_FIQ: u32 = 0x0000_001C;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MemAccess {
    ReadByte,
    ReadHalf,
    ReadWord,
    WriteByte,
    WriteHalf,
    WriteWord,
}

/// Memory interface the CPU decoders use. Implementations own the address
/// space and are responsible for their own timing; the CPU accumulates a cycle
/// count via [`Cpu::cycles`].
pub trait Bus {
    fn read8(&mut self, addr: u32) -> u32;
    fn read16(&mut self, addr: u32) -> u32;
    fn read32(&mut self, addr: u32) -> u32;
    fn write8(&mut self, addr: u32, value: u32);
    fn write16(&mut self, addr: u32, value: u32);
    fn write32(&mut self, addr: u32, value: u32);
}

pub struct Cpu {
    /// Working register file: r0-r15. r15 is the PC; the *value* read from it
    /// is `pc + 8` in ARM mode or `pc + 4` in Thumb mode, but we store the
    /// actual next-instruction address in `pc` and synthesize reads.
    regs: [u32; 16],
    /// Current program counter (the address of the instruction being executed,
    /// or the next one to fetch).
    pc: u32,
    cpsr: u32,
    /// Banked r8-r12 shared by USR/SVC/ABT/UND/IRQ (only FIQ has its own copy).
    base_r8_12: [u32; 5],
    /// Banked r8-r12 for FIQ mode.
    fiq_r8_12: [u32; 5],
    /// Banked SP/LR, indexed by mode: [FIQ, SVC, ABT, UND, IRQ]. USR shares the
    /// SVC slot.
    sp: [u32; 5],
    lr: [u32; 5],
    /// SPSR per exception mode, same indexing as `sp`.
    spsr: [u32; 5],
    /// Cycle counter for the current instruction (reset each [`Cpu::execute`]).
    cycles: u32,
    /// True while the CPU is halted (SWI 0x02 `Halt`).
    pub halted: bool,
    /// HLE BIOS wait mask used by IntrWait and VBlankIntrWait: the IRQ flags
    /// the routine is waiting for. `None` when no wait is in progress.
    bios_wait: Option<u16>,
    /// A BIOS SWI recorded by the decoders, dispatched by the system once the
    /// instruction has finished executing. `None` when no SWI was executed.
    bios_call: Option<u32>,
    /// True when a real BIOS dump is loaded. In that mode SWIs take the real
    /// SVC exception (vector 0x08) so the BIOS dispatcher runs, instead of the
    /// HLE intercept.
    pub has_bios: bool,
}

/// Index of a banked register file within `sp`/`lr`/`spsr`.
#[derive(Clone, Copy)]
pub struct Bank(pub usize);

fn bank_index(mode: u32) -> usize {
    match mode {
        mode::FIQ => 0,
        mode::SVC => 1,
        mode::ABT => 2,
        mode::UND => 3,
        mode::IRQ => 4,
        _ => 0, // USR/SYS share the SVC... handled by caller never asking
    }
}

impl Cpu {
    pub fn new() -> Cpu {
        Cpu {
            regs: [0; 16],
            pc: 0,
            cpsr: mode::SVC | flag::F | flag::I,
            base_r8_12: [0; 5],
            fiq_r8_12: [0; 5],
            sp: [0; 5],
            lr: [0; 5],
            spsr: [0; 5],
            cycles: 0,
            halted: false,
            bios_wait: None,
            bios_call: None,
            has_bios: false,
        }
    }

    /// Enable / disable real-BIOS SWI/exception handling.
    pub fn set_has_bios(&mut self, on: bool) {
        self.has_bios = on;
    }

    #[inline]
    pub fn add_cycles(&mut self, n: u32) {
        self.cycles += n;
    }

    /// Execute a single instruction, returning the cycles consumed. The
    /// instruction is fetched from `self.pc`; the mode (ARM/Thumb) is taken
    /// from the CPSR `T` flag.
    pub fn execute(&mut self, bus: &mut dyn Bus) -> u32 {
        self.cycles = 0;
        if self.halted {
            self.add_cycles(1);
            return self.cycles;
        }
        let pc = self.pc;
        if self.cpsr & flag::T != 0 {
            // Thumb mode: 2-byte fetch.
            let inst = bus.read16(pc);
            self.advance_pc_thumb();
            self.add_cycles(1);
            thumb::execute(self, bus, inst);
        } else {
            // ARM mode: 4-byte fetch.
            let inst = bus.read32(pc);
            self.advance_pc_arm();
            self.add_cycles(1);
            arm::execute(self, bus, inst);
        }
        self.cycles
    }

    #[inline]
    fn advance_pc_arm(&mut self) {
        self.pc = self.pc.wrapping_add(4);
    }

    #[inline]
    fn advance_pc_thumb(&mut self) {
        self.pc = self.pc.wrapping_add(2);
    }

    pub fn cpsr(&self) -> u32 {
        self.cpsr
    }

    pub fn set_cpsr(&mut self, value: u32) {
        self.change_mode(value & 0x1F, false);
        self.cpsr = value;
    }

    pub fn pc(&self) -> u32 {
        self.pc
    }

    pub fn set_pc(&mut self, value: u32) {
        self.pc = value & !1;
        // Branching never changes the T flag via set_pc alone; BX handles that.
    }

    /// Read a general register (r0-r14). r15 is synthesized: the stored `pc`
    /// holds the *next* fetch address, so reading r15 yields `pc+4` (Thumb) or
    /// `pc+8` (ARM), i.e. the current instruction address + 4/+8.
    #[inline]
    pub fn reg(&self, n: u32) -> u32 {
        if n == 15 {
            if self.cpsr & flag::T != 0 {
                self.pc + 2
            } else {
                self.pc + 4
            }
        } else {
            self.regs[n as usize]
        }
    }

    /// Write a general register. Writing r15 is a branch.
    #[inline]
    pub fn set_reg(&mut self, n: u32, value: u32) {
        if n == 15 {
            self.branch(value);
        } else {
            self.regs[n as usize] = value;
        }
    }

    /// Direct read of r15 (actual address, not +8/+4), used by the decoders.
    #[inline]
    pub fn raw_pc(&self) -> u32 {
        self.pc
    }

    /// Branch: clear the bottom bits according to mode, and clear the T flag
    /// (the caller handles setting it for `BX`).
    #[inline]
    pub fn branch(&mut self, value: u32) {
        if self.cpsr & flag::T != 0 {
            self.pc = value & !1;
        } else {
            self.pc = value & !3;
        }
    }

    /// `BX`: branch with possible switch to Thumb mode (value bit 0).
    pub fn bx(&mut self, value: u32) {
        // Switch state first: the target is aligned for the *new* state
        // (halfword for Thumb, word for ARM), not the one we are leaving.
        if value & 1 != 0 {
            self.cpsr |= flag::T;
            self.pc = value & !1;
        } else {
            self.cpsr &= !flag::T;
            self.pc = value & !3;
        }
    }

    /// Register read with no PC-synthesis, used internally.
    #[inline]
    pub fn reg_raw(&self, n: u32) -> u32 {
        self.regs[n as usize]
    }

    /// Switch the active mode, banking r8-r14 as required. `via_exception` is
    /// reserved (exceptions write LR/SPSR after this call).
    fn change_mode(&mut self, new_mode: u32, _via_exception: bool) {
        let old = self.cpsr & 0x1F;
        if old == new_mode {
            return;
        }
        // Save outgoing bank.
        if old == mode::FIQ {
            self.fiq_r8_12.copy_from_slice(&self.regs[8..13]);
            self.sp[0] = self.regs[13];
            self.lr[0] = self.regs[14];
        } else {
            let idx = bank_index(old);
            self.base_r8_12.copy_from_slice(&self.regs[8..13]);
            self.sp[idx] = self.regs[13];
            self.lr[idx] = self.regs[14];
        }
        // Load incoming bank.
        if new_mode == mode::FIQ {
            self.regs[8..13].copy_from_slice(&self.fiq_r8_12);
            self.regs[13] = self.sp[0];
            self.regs[14] = self.lr[0];
        } else {
            let idx = bank_index(new_mode);
            self.regs[8..13].copy_from_slice(&self.base_r8_12);
            self.regs[13] = self.sp[idx];
            self.regs[14] = self.lr[idx];
        }
    }

    /// Return a copy of the current (non-PC) registers. Used for tests.
    pub fn dump_regs(&self) -> [u32; 16] {
        let mut r = self.regs;
        r[15] = self.pc;
        r
    }

    /// Stack pointer of the current mode (diagnostics).
    pub fn sp_raw(&self) -> u32 {
        self.regs[13]
    }

    /// Set the banked SP of a non-user mode. Used by the skip-BIOS boot path
    /// to give the exception modes the stacks the BIOS would have set up
    /// (GBATEK: SP_irq=03007FA0h, SP_svc=03007FE0h, SP_usr=03007F00h).
    pub fn set_mode_sp(&mut self, mode: u32, value: u32) {
        if mode == self.cpsr & 0x1F {
            self.regs[13] = value;
        } else {
            self.sp[bank_index(mode)] = value;
        }
    }

    /// Enter an exception. Sets LR, SPSR, mode and PC, disabling the
    /// appropriate interrupts. `lr_value` is the link address to store.
    pub fn take_exception(&mut self, vector: u32, mode: u32, lr_value: u32, disable_fiq: bool) {
        self.change_mode(mode, true);
        let idx = bank_index(mode);
        self.lr[idx] = lr_value;
        // change_mode already loaded regs[14] from the (stale) bank slot.
        self.regs[14] = lr_value;
        self.spsr[idx] = self.cpsr;
        self.cpsr &= !flag::T; // exceptions run in ARM mode
        self.cpsr |= flag::I;
        if disable_fiq {
            self.cpsr |= flag::F;
        }
        self.cpsr = (self.cpsr & !0x1F) | mode;
        self.pc = vector;
    }

    /// SWI exception (called by both ARM and Thumb SWI decoders).
    pub fn swi(&mut self, lr_value: u32) {
        self.take_exception(VECTOR_SWI, mode::SVC, lr_value, false);
    }

    /// Record a BIOS SWI to be dispatched by the system after the instruction
    /// finishes. The PC is already past the SWI, so a handled call returns
    /// directly to the next instruction.
    pub(crate) fn swi_bios(&mut self, num: u32) {
        self.bios_call = Some(num);
    }

    /// Take the recorded BIOS SWI number, clearing the pending flag.
    pub(crate) fn take_bios_call(&mut self) -> Option<u32> {
        self.bios_call.take()
    }

    /// Start an HLE IntrWait: the CPU idles until one of the `mask` IRQ flags
    /// is raised.
    pub(crate) fn begin_bios_wait(&mut self, mask: u16) {
        self.bios_wait = Some(mask);
    }

    /// The IRQ mask an HLE IntrWait is currently waiting for, if any.
    pub fn bios_wait_mask(&self) -> Option<u16> {
        self.bios_wait
    }

    /// Finish the HLE IntrWait in progress.
    pub(crate) fn complete_bios_wait(&mut self) {
        self.bios_wait = None;
    }

    /// IRQ exception.
    pub fn irq(&mut self, lr_value: u32) {
        self.take_exception(VECTOR_IRQ, mode::IRQ, lr_value, false);
    }

    /// FIQ exception.
    pub fn fiq(&mut self, lr_value: u32) {
        self.take_exception(VECTOR_FIQ, mode::FIQ, lr_value, true);
    }

    /// Direct register write used by exception return paths (MRS/MSR/LDM^).
    pub fn set_spsr(&mut self, mode: u32, value: u32) {
        self.spsr[bank_index(mode)] = value;
    }

    pub fn get_spsr(&self, mode: u32) -> u32 {
        self.spsr[bank_index(mode)]
    }

    /// Read a register from the user (SYS) bank, used by `LDM^`/`STM^`.
    pub fn usr_reg(&self, n: u32) -> u32 {
        if n < 8 {
            self.regs[n as usize]
        } else if n < 13 {
            self.base_r8_12[(n - 8) as usize]
        } else if n == 13 {
            self.sp[1]
        } else {
            self.lr[1]
        }
    }

    /// Write a register into the user (SYS) bank, used by `LDM^`/`STM^`.
    pub fn set_usr_reg(&mut self, n: u32, v: u32) {
        if n < 8 {
            self.regs[n as usize] = v;
        } else if n < 13 {
            self.base_r8_12[(n - 8) as usize] = v;
        } else if n == 13 {
            self.sp[1] = v;
        } else {
            self.lr[1] = v;
        }
    }

    /// Whether IRQ is masked.
    pub fn irq_masked(&self) -> bool {
        self.cpsr & flag::I != 0
    }

    /// Whether FIQ is masked.
    pub fn fiq_masked(&self) -> bool {
        self.cpsr & flag::F != 0
    }

    pub fn in_thumb(&self) -> bool {
        self.cpsr & flag::T != 0
    }
}

/// Plain snapshot of the full CPU register file for save states.
#[derive(Clone, Copy)]
pub(crate) struct CpuSave {
    pub regs: [u32; 16],
    pub pc: u32,
    pub cpsr: u32,
    pub base_r8_12: [u32; 5],
    pub fiq_r8_12: [u32; 5],
    pub sp: [u32; 5],
    pub lr: [u32; 5],
    pub spsr: [u32; 5],
    pub cycles: u32,
    pub halted: bool,
    pub bios_wait: Option<u16>,
}

impl Cpu {
    pub(crate) fn save(&self) -> CpuSave {
        CpuSave {
            regs: self.regs,
            pc: self.pc,
            cpsr: self.cpsr,
            base_r8_12: self.base_r8_12,
            fiq_r8_12: self.fiq_r8_12,
            sp: self.sp,
            lr: self.lr,
            spsr: self.spsr,
            cycles: self.cycles,
            halted: self.halted,
            bios_wait: self.bios_wait,
        }
    }

    pub(crate) fn restore(&mut self, s: CpuSave) {
        self.regs = s.regs;
        self.pc = s.pc;
        self.cpsr = s.cpsr;
        self.base_r8_12 = s.base_r8_12;
        self.fiq_r8_12 = s.fiq_r8_12;
        self.sp = s.sp;
        self.lr = s.lr;
        self.spsr = s.spsr;
        self.cycles = s.cycles;
        self.halted = s.halted;
        self.bios_wait = s.bios_wait;
        self.bios_call = None;
    }
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

/// A tiny flat-memory bus used by CPU unit tests (no wait states).
#[cfg(test)]
#[derive(Default)]
pub struct TestBus {
    pub mem: Vec<u8>,
}

#[cfg(test)]
impl TestBus {
    pub fn new() -> Self {
        TestBus {
            mem: vec![0; 0x10000],
        }
    }
}

#[cfg(test)]
impl Bus for TestBus {
    fn read8(&mut self, addr: u32) -> u32 {
        self.mem[addr as usize & 0xFFFF] as u32
    }
    fn read16(&mut self, addr: u32) -> u32 {
        let a = addr as usize & 0xFFFF;
        self.mem[a] as u32 | (self.mem[a + 1] as u32) << 8
    }
    fn read32(&mut self, addr: u32) -> u32 {
        let a = addr as usize & 0xFFFF;
        (self.mem[a] as u32)
            | (self.mem[a + 1] as u32) << 8
            | (self.mem[a + 2] as u32) << 16
            | (self.mem[a + 3] as u32) << 24
    }
    fn write8(&mut self, addr: u32, value: u32) {
        let a = addr as usize & 0xFFFF;
        self.mem[a] = value as u8;
    }
    fn write16(&mut self, addr: u32, value: u32) {
        let a = addr as usize & 0xFFFF;
        self.mem[a] = value as u8;
        self.mem[a + 1] = (value >> 8) as u8;
    }
    fn write32(&mut self, addr: u32, value: u32) {
        let a = addr as usize & 0xFFFF;
        self.mem[a] = value as u8;
        self.mem[a + 1] = (value >> 8) as u8;
        self.mem[a + 2] = (value >> 16) as u8;
        self.mem[a + 3] = (value >> 24) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::flag;

    fn arm(bus: &mut TestBus, addr: u32, inst: u32) {
        bus.write32(addr, inst);
    }
    fn thumb(bus: &mut TestBus, addr: u32, inst: u16) {
        bus.write16(addr, inst as u32);
    }

    #[test]
    fn arm_mov_immediate_and_flags() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        let mut bus = TestBus::new();
        arm(&mut bus, 0, 0xE3B0102A); // MOVS r1, #0x2A (S set -> Z cleared)
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[1], 0x2A);
        assert_eq!(cpu.cpsr & flag::Z, 0);

        // MOVS r0, #0 sets Z.
        cpu.regs[0] = 0;
        arm(&mut bus, 0, 0xE3B00000);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.cpsr & flag::Z, flag::Z);
    }

    #[test]
    fn arm_add_carry_and_zero() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        let mut bus = TestBus::new();
        cpu.regs[0] = 0xFFFF_FFFF;
        cpu.regs[1] = 1;
        arm(&mut bus, 0, 0xE0900001); // ADDS r0, r0, r1
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[0], 0);
        assert_ne!(cpu.cpsr & flag::C, 0);
        assert_ne!(cpu.cpsr & flag::Z, 0);
    }

    #[test]
    fn arm_branch_relative() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        let mut bus = TestBus::new();
        arm(&mut bus, 0, 0xEA000002); // B +8 -> target 0x10
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.pc, 0x10);
    }

    #[test]
    fn arm_branch_link_sets_lr() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        let mut bus = TestBus::new();
        arm(&mut bus, 0, 0xEB000000); // BL +0
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[14], 4); // return address = A+4
        assert_eq!(cpu.pc, 8);
    }

    #[test]
    fn arm_bx_to_thumb_and_back() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        let mut bus = TestBus::new();
        cpu.regs[0] = 0x200 | 1;
        arm(&mut bus, 0, 0xE12FFF10); // BX r0
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_ne!(cpu.cpsr & flag::T, 0);
        assert_eq!(cpu.pc, 0x200);
        // Back to ARM via a Thumb BX (0x4700 = BX r0).
        cpu.regs[0] = 0x400;
        thumb(&mut bus, 0, 0x4700);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.cpsr & flag::T, 0);
        assert_eq!(cpu.pc, 0x400);
    }

    #[test]
    fn arm_bx_keeps_halfword_aligned_thumb_target() {
        // An ARM-state `BX` to a Thumb address that is 2 mod 4 must land on
        // that halfword, not be word-aligned by the state being left.
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        let mut bus = TestBus::new();
        cpu.regs[14] = 0x082E_0023;
        arm(&mut bus, 0, 0xE12FFF1E); // BX lr
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert!(cpu.in_thumb());
        assert_eq!(cpu.pc, 0x082E_0022);
    }

    #[test]
    fn thumb_bx_to_arm_word_aligns_target() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR | flag::T);
        let mut bus = TestBus::new();
        cpu.regs[0] = 0x402;
        thumb(&mut bus, 0, 0x4700); // BX r0
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert!(!cpu.in_thumb());
        assert_eq!(cpu.pc, 0x400);
    }

    #[test]
    fn arm_swi_enters_svc() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        let mut bus = TestBus::new();
        arm(&mut bus, 0, 0xEF000000); // SWI 0
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.pc, VECTOR_SWI);
        assert_eq!(cpu.cpsr & 0x1F, mode::SVC);
        assert_ne!(cpu.cpsr & flag::I, 0);
        assert_eq!(cpu.lr[1], 4); // SVC LR = return address
                                  // SPSR_SVC saved USR mode + no T.
        assert_eq!(cpu.spsr[1] & 0x1F, mode::USR);
    }

    #[test]
    fn arm_ldr_str() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        let mut bus = TestBus::new();
        cpu.regs[1] = 0x100;
        cpu.regs[0] = 0xDEADBEEF;
        arm(&mut bus, 0, 0xE5810000); // STR r0, [r1]
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(bus.read32(0x100), 0xDEADBEEF);
        arm(&mut bus, 0, 0xE5912000); // LDR r2, [r1]
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[2], 0xDEADBEEF);
    }

    #[test]
    fn arm_ldm_stm() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        let mut bus = TestBus::new();
        cpu.regs[1] = 0x100; // base
        cpu.regs[2] = 0x22;
        cpu.regs[3] = 0x33;
        arm(&mut bus, 0, 0xE88A000C); // STMIA r2!, {r3,r4}? -> use r1 base, r2,r3
                                      // STMIA r1!, {r2,r3} = 0xE8A1000C
        arm(&mut bus, 0, 0xE8A1000C);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(bus.read32(0x100), 0x22);
        assert_eq!(bus.read32(0x104), 0x33);
        assert_eq!(cpu.regs[1], 0x108);
        // LDMIA r1!, {r4,r5}
        cpu.regs[1] = 0x100;
        arm(&mut bus, 0, 0xE8B10030);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[4], 0x22);
        assert_eq!(cpu.regs[5], 0x33);
        assert_eq!(cpu.regs[1], 0x108);
    }

    #[test]
    fn arm_multiply() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        let mut bus = TestBus::new();
        cpu.regs[0] = 7;
        cpu.regs[1] = 6;
        arm(&mut bus, 0, 0xE0000190); // MUL r0, r1, r0
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[0], 42);
    }

    #[test]
    fn arm_mode_banking_fiq() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        cpu.regs[8] = 0x1111;
        cpu.regs[9] = 0x2222;
        // Enter FIQ via exception.
        cpu.take_exception(VECTOR_FIQ, mode::FIQ, 0x99, true);
        assert_eq!(cpu.cpsr & 0x1F, mode::FIQ);
        assert_ne!(cpu.cpsr & flag::F, 0);
        // FIQ r8 is now a separate bank (0 initially).
        assert_eq!(cpu.regs[8], 0);
        cpu.regs[8] = 0xAAAA;
        cpu.regs[9] = 0xBBBB;
        // Return to USR by restoring CPSR.
        let saved_usr = cpu.spsr[0];
        cpu.set_cpsr(saved_usr);
        assert_eq!(cpu.cpsr & 0x1F, mode::USR);
        assert_eq!(cpu.regs[8], 0x1111);
        assert_eq!(cpu.regs[9], 0x2222);
    }

    #[test]
    fn thumb_add_immediate() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR | flag::T);
        let mut bus = TestBus::new();
        cpu.regs[1] = 5;
        thumb(&mut bus, 0, 0x1D09); // ADD r1, r1, #4
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[1], 9);
        assert_eq!(cpu.pc, 2);
    }

    #[test]
    fn thumb_branch_link() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR | flag::T);
        let mut bus = TestBus::new();
        thumb(&mut bus, 0, 0xF000); // BL upper, offset 0
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[14], 4);
        assert_eq!(cpu.pc, 2);
        thumb(&mut bus, 2, 0xF800); // BL lower, offset 0
        cpu.pc = 2;
        cpu.execute(&mut bus);
        assert_eq!(cpu.pc, 4);
        assert_eq!(cpu.regs[14], 5); // return address | 1
    }

    #[test]
    fn thumb_ldr_str_register() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR | flag::T);
        let mut bus = TestBus::new();
        cpu.regs[0] = 0x200;
        cpu.regs[1] = 0x10;
        cpu.regs[2] = 0xCAFEBABE;
        // STR r2, [r0, r1]: format 7, L=0 B=0, Ro=001 Rb=000 Rd=010 = 0x5042.
        thumb(&mut bus, 0, 0x5042);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(bus.read32(0x210), 0xCAFEBABE);
        // LDR r3, [r0, r1]: format 7, L=1 B=0 = 0x5843.
        thumb(&mut bus, 0, 0x5843);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[3], 0xCAFEBABE);
        // LDRSH r4, [r0, r1]: format 8, H=1 S=1 = 0x5E44.
        bus.write16(0x210, 0x8001);
        thumb(&mut bus, 0, 0x5E44);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[4], 0xFFFF_8001);
    }

    #[test]
    fn thumb_push_pop_round_trip() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR | flag::T);
        let mut bus = TestBus::new();
        cpu.regs[4] = 0x4444_4444;
        cpu.regs[5] = 0x5555_5555;
        cpu.regs[6] = 0x6666_6666;
        cpu.regs[7] = 0x7777_7777;
        cpu.regs[14] = 0x0800_1234;
        cpu.regs[13] = 0x1000;
        thumb(&mut bus, 0, 0xB5F0); // push {r4-r7, lr}
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[13], 0x1000 - 20);
        // Lowest register at the lowest address, LR on top.
        assert_eq!(bus.read32(0x1000 - 20), 0x4444_4444);
        assert_eq!(bus.read32(0x1000 - 16), 0x5555_5555);
        assert_eq!(bus.read32(0x1000 - 12), 0x6666_6666);
        assert_eq!(bus.read32(0x1000 - 8), 0x7777_7777);
        assert_eq!(bus.read32(0x1000 - 4), 0x0800_1234);
        for r in 4..8 {
            cpu.regs[r] = 0;
        }
        thumb(&mut bus, 2, 0xBCF0); // pop {r4-r7}
        cpu.pc = 2;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[13], 0x1000 - 4);
        assert_eq!(cpu.regs[4], 0x4444_4444);
        assert_eq!(cpu.regs[7], 0x7777_7777);
    }

    #[test]
    fn thumb_pop_pc_branches_via_stack() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR | flag::T);
        let mut bus = TestBus::new();
        cpu.regs[13] = 0x1000;
        bus.write32(0x1000, 0xAAAA_AAAA);
        bus.write32(0x1004, 0x0800_2000 | 1);
        thumb(&mut bus, 0, 0xBD02); // pop {r1, pc}
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[1], 0xAAAA_AAAA);
        assert_eq!(cpu.pc, 0x0800_2000);
        assert_eq!(cpu.regs[13], 0x1008);
        assert!(cpu.in_thumb());
    }

    #[test]
    fn thumb_add_sub_register_uses_rs_as_left_operand() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR | flag::T);
        let mut bus = TestBus::new();
        cpu.regs[0] = 3;
        cpu.regs[1] = 10;
        thumb(&mut bus, 0, 0x1842); // add r2, r0, r1
        thumb(&mut bus, 2, 0x1A43); // sub r3, r0, r1
        cpu.pc = 0;
        cpu.execute(&mut bus);
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[2], 13);
        assert_eq!(cpu.regs[3], (-7i32) as u32);
    }

    #[test]
    fn thumb_ldr_sp_relative_and_ldmia_decode() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR | flag::T);
        let mut bus = TestBus::new();
        cpu.regs[13] = 0x1000;
        bus.write32(0x1008, 0x1234_5678);
        thumb(&mut bus, 0, 0x9A02); // ldr r2, [sp, #8]
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[2], 0x1234_5678);
        cpu.regs[0] = 0x2000;
        bus.write32(0x2000, 1);
        bus.write32(0x2004, 2);
        thumb(&mut bus, 2, 0xC806); // ldmia r0!, {r1, r2}
        cpu.pc = 2;
        cpu.execute(&mut bus);
        assert_eq!((cpu.regs[1], cpu.regs[2], cpu.regs[0]), (1, 2, 0x2008));
    }

    #[test]
    fn thumb_bl_negative_offset() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR | flag::T);
        let mut bus = TestBus::new();
        // bl 0x80 from 0x100: offset -0x84 -> prefix F7FF, suffix FFBE.
        thumb(&mut bus, 0x100, 0xF7FF);
        thumb(&mut bus, 0x102, 0xFFBE);
        cpu.pc = 0x100;
        cpu.execute(&mut bus);
        cpu.execute(&mut bus);
        assert_eq!(cpu.pc, 0x80);
        assert_eq!(cpu.regs[14], 0x105);
    }

    #[test]
    fn condition_codes() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR | flag::Z);
        let mut bus = TestBus::new();
        // ADDEQ r0, r0, #1 (EQ executes, Z set)
        arm(&mut bus, 0, 0x02800001);
        cpu.regs[0] = 0;
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[0], 1);
        // ADDNE r0, r0, #1 (NE fails, no exec)
        cpu.regs[0] = 0;
        arm(&mut bus, 0, 0x12800001);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[0], 0);
    }

    #[test]
    fn arm_mrs_msr_register() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR | flag::Z | flag::C);
        let mut bus = TestBus::new();
        // MRS r1, cpsr
        arm(&mut bus, 0, 0xE10F1000);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[1], mode::USR | flag::Z | flag::C);
        // MSR cpsr_f, r1: the flags field is bit 3 of the field mask (0x8).
        cpu.regs[1] = flag::V | mode::USR;
        arm(&mut bus, 0, 0xE128F001);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.cpsr & field::FLAGS, flag::V);
        assert_eq!(cpu.cpsr & 0x1F, mode::USR);
        // MRS r2, spsr must decode (bit 22 set) and read the SPSR of the
        // current mode.
        cpu.set_cpsr(mode::IRQ);
        cpu.set_spsr(mode::IRQ, 0xF000_0010);
        arm(&mut bus, 0, 0xE14F2000);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[2], 0xF000_0010);
    }

    #[test]
    fn arm_subs_pc_lr_restores_spsr() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR | flag::T);
        cpu.regs[13] = 0x0300_7F00;
        cpu.irq(0x0800_0102 + 4);
        assert_eq!(cpu.cpsr & 0x1F, mode::IRQ);
        assert_eq!(cpu.regs[14], 0x0800_0106);
        assert!(!cpu.in_thumb());
        let mut bus = TestBus::new();
        arm(&mut bus, VECTOR_IRQ, 0xE25EF004); // subs pc, lr, #4
        cpu.execute(&mut bus);
        assert_eq!(cpu.cpsr & 0x1F, mode::USR);
        assert!(cpu.in_thumb());
        assert_eq!(cpu.pc, 0x0800_0102);
        // The user-mode stack pointer is back in r13.
        assert_eq!(cpu.regs[13], 0x0300_7F00);
    }

    #[test]
    fn arm_ldm_user_bank_without_pc() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        cpu.regs[13] = 0x1111;
        cpu.set_cpsr(mode::IRQ);
        cpu.regs[13] = 0x2222;
        let mut bus = TestBus::new();
        cpu.regs[0] = 0x400;
        bus.write32(0x400, 0xABCD);
        arm(&mut bus, 0, 0xE8D0_2000); // ldmia r0, {r13}^
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[13], 0x2222, "IRQ sp untouched");
        assert_eq!(cpu.usr_reg(13), 0xABCD, "user sp loaded");
    }

    #[test]
    fn arm_strh_post_index_same_reg_writes_back() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        let mut bus = TestBus::new();
        cpu.regs[0] = 0x200;
        arm(&mut bus, 0, 0xE0C0_00B2); // strh r0, [r0], #2
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(bus.read16(0x200), 0x200);
        assert_eq!(cpu.regs[0], 0x202);
    }

    #[test]
    fn arm_ldm_writeback_loaded_base_wins() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        let mut bus = TestBus::new();
        cpu.regs[0] = 0x400;
        bus.write32(0x400, 0x1234);
        bus.write32(0x404, 0x5678);
        arm(&mut bus, 0, 0xE8B0_0003); // ldmia r0!, {r0, r1}
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[0], 0x1234);
        assert_eq!(cpu.regs[1], 0x5678);
    }

    #[test]
    fn arm_swp_and_signed_half() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        let mut bus = TestBus::new();
        bus.write32(0x100, 0xDEADBEEF);
        cpu.regs[0] = 0x100;
        cpu.regs[1] = 0x11111111;
        // SWP r2, r1, [r0]
        arm(&mut bus, 0, 0xE1002091);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[2], 0xDEADBEEF);
        assert_eq!(bus.read32(0x100), 0x11111111);
        // LDRSH r3, [r0, #2] (sign-extend signed halfword)
        bus.write16(0x102, 0xFFF2);
        cpu.regs[0] = 0x100;
        arm(&mut bus, 0, 0xE1D030F2);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[3], 0xFFFF_FFF2);
    }

    #[test]
    fn arm_long_multiply() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        let mut bus = TestBus::new();
        // UMAAL/SMULL: SMULL r2, r3, r0, r1 (signed, no acc)
        cpu.regs[0] = 0x8000_0000; // -2147483648
        cpu.regs[1] = 2;
        arm(&mut bus, 0, 0xE0C32091);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[2], 0);
        assert_eq!(cpu.regs[3], 0xFFFF_FFFF);
    }

    #[test]
    fn arm_ldm_stm_with_base_writeback() {
        let mut cpu = Cpu::new();
        cpu.set_cpsr(mode::USR);
        let mut bus = TestBus::new();
        cpu.regs[0] = 1;
        cpu.regs[1] = 2;
        cpu.regs[2] = 3;
        cpu.regs[10] = 0x100;
        // STMDB r10!, {r0, r1, r2}
        arm(&mut bus, 0, 0xE92A0007);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[10], 0x0F4);
        assert_eq!(bus.read32(0x0F4), 1);
        assert_eq!(bus.read32(0x0F8), 2);
        assert_eq!(bus.read32(0x0FC), 3);
        // LDMIA r10!, {r4, r5, r6}
        arm(&mut bus, 0, 0xE8BA0070);
        cpu.pc = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.regs[10], 0x100);
        assert_eq!(cpu.regs[4], 1);
        assert_eq!(cpu.regs[5], 2);
        assert_eq!(cpu.regs[6], 3);
    }
}
