//! Instruction tests on a flat memory. Encodings are written out by hand with
//! the assembly beside them.

use crate::{mode, psr, Abort, Arch, Bus, CpEffect, CpReg, Cpu, Exception};

mod v6;

const MEM: usize = 0x2_0000;

struct Flat {
    mem: Vec<u8>,
    /// Data accesses in this range abort.
    no_access: std::ops::Range<u32>,
    high_vectors: bool,
    control: u32,
    unprivileged_accesses: u32,
    unaligned: bool,
    exclusive: Option<u32>,
}

impl Flat {
    fn new() -> Self {
        Flat {
            mem: vec![0; MEM],
            no_access: 0..0,
            high_vectors: false,
            control: 0x0000_0078,
            unprivileged_accesses: 0,
            unaligned: false,
            exclusive: None,
        }
    }

    fn at(&self, addr: u32) -> usize {
        addr as usize & (MEM - 1)
    }

    fn check(&mut self, addr: u32, privileged: bool) -> Result<(), Abort> {
        if !privileged {
            self.unprivileged_accesses += 1;
        }
        if self.no_access.contains(&addr) {
            Err(Abort)
        } else {
            Ok(())
        }
    }

    fn word(&self, addr: u32) -> u32 {
        let i = self.at(addr);
        u32::from_le_bytes(self.mem[i..i + 4].try_into().unwrap())
    }

    fn set_word(&mut self, addr: u32, value: u32) {
        let i = self.at(addr);
        self.mem[i..i + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn set_half(&mut self, addr: u32, value: u16) {
        let i = self.at(addr);
        self.mem[i..i + 2].copy_from_slice(&value.to_le_bytes());
    }
}

impl Bus for Flat {
    fn fetch16(&mut self, addr: u32, _: bool) -> Result<u16, Abort> {
        let i = self.at(addr);
        Ok(u16::from_le_bytes([self.mem[i], self.mem[i + 1]]))
    }
    fn fetch32(&mut self, addr: u32, _: bool) -> Result<u32, Abort> {
        if addr == 0xDEAD_0000 {
            return Err(Abort);
        }
        Ok(self.word(addr))
    }
    fn read8(&mut self, addr: u32, p: bool) -> Result<u8, Abort> {
        self.check(addr, p)?;
        Ok(self.mem[self.at(addr)])
    }
    fn read16(&mut self, addr: u32, p: bool) -> Result<u16, Abort> {
        self.check(addr, p)?;
        let i = self.at(addr);
        Ok(u16::from_le_bytes([self.mem[i], self.mem[i + 1]]))
    }
    fn read32(&mut self, addr: u32, p: bool) -> Result<u32, Abort> {
        self.check(addr, p)?;
        Ok(self.word(addr))
    }
    fn write8(&mut self, addr: u32, value: u8, p: bool) -> Result<(), Abort> {
        self.check(addr, p)?;
        let i = self.at(addr);
        self.mem[i] = value;
        Ok(())
    }
    fn write16(&mut self, addr: u32, value: u16, p: bool) -> Result<(), Abort> {
        self.check(addr, p)?;
        self.set_half(addr, value);
        Ok(())
    }
    fn write32(&mut self, addr: u32, value: u32, p: bool) -> Result<(), Abort> {
        self.check(addr, p)?;
        self.set_word(addr, value);
        Ok(())
    }
    fn coproc_read(&mut self, reg: CpReg, privileged: bool) -> Option<u32> {
        (privileged && reg.cp == 15 && reg.crn == 1).then_some(self.control)
    }
    fn coproc_write(&mut self, reg: CpReg, value: u32, privileged: bool) -> Option<CpEffect> {
        if !privileged || reg.cp != 15 {
            return None;
        }
        match (reg.crn, reg.crm, reg.opc2) {
            (1, 0, 0) => {
                self.control = value;
                Some(CpEffect::None)
            }
            (7, 0, 4) => Some(CpEffect::WaitForInterrupt),
            _ => None,
        }
    }
    fn high_vectors(&self) -> bool {
        self.high_vectors
    }
    fn unaligned_access(&self) -> bool {
        self.unaligned
    }
    fn exclusive_load(&mut self, addr: u32) {
        self.exclusive = Some(addr);
    }
    fn exclusive_store(&mut self, addr: u32) -> bool {
        self.exclusive.take() == Some(addr)
    }
    fn exclusive_clear(&mut self) {
        self.exclusive = None;
    }
}

const BASE: u32 = 0x1000;

/// A processor in system mode about to run ARM `code` at `BASE`.
fn arm(code: &[u32]) -> (Cpu, Flat) {
    arm_on(Arch::V5te, code)
}

fn arm_on(arch: Arch, code: &[u32]) -> (Cpu, Flat) {
    let mut bus = Flat::new();
    for (i, word) in code.iter().enumerate() {
        bus.set_word(BASE + i as u32 * 4, *word);
    }
    let mut cpu = Cpu::new(arch, &bus);
    cpu.set_cpsr(mode::SYS);
    cpu.set_reg(13, 0x8000);
    cpu.jump(BASE);
    (cpu, bus)
}

/// The same for Thumb `code`.
fn thumb(code: &[u16]) -> (Cpu, Flat) {
    thumb_on(Arch::V5te, code)
}

fn thumb_on(arch: Arch, code: &[u16]) -> (Cpu, Flat) {
    let mut bus = Flat::new();
    for (i, half) in code.iter().enumerate() {
        bus.set_half(BASE + i as u32 * 2, *half);
    }
    let mut cpu = Cpu::new(arch, &bus);
    cpu.set_cpsr(mode::SYS);
    cpu.set_reg(13, 0x8000);
    cpu.jump(BASE | 1);
    (cpu, bus)
}

fn run(cpu: &mut Cpu, bus: &mut Flat, steps: usize) {
    for _ in 0..steps {
        cpu.step(bus);
    }
}

fn flags(cpu: &Cpu) -> (bool, bool, bool, bool) {
    let f = |bit| cpu.cpsr() & bit != 0;
    (f(psr::N), f(psr::Z), f(psr::C), f(psr::V))
}

#[test]
fn reset_state_is_supervisor_with_interrupts_masked() {
    let bus = Flat::new();
    let cpu = Cpu::new(Arch::V5te, &bus);
    assert_eq!(cpu.cpsr(), mode::SVC | psr::I | psr::F);
    assert_eq!(cpu.reg(15), 0);

    let mut high = Flat::new();
    high.high_vectors = true;
    assert_eq!(Cpu::new(Arch::V5te, &high).reg(15), 0xFFFF_0000);
}

#[test]
fn arithmetic_sets_carry_and_overflow() {
    let (mut cpu, mut bus) = arm(&[
        0xE3E0_0000, // mvn  r0, #0
        0xE290_1001, // adds r1, r0, #1      -> 0, Z C
        0xE3A0_2102, // mov  r2, #0x80000000
        0xE252_3001, // subs r3, r2, #1      -> 0x7FFFFFFF, C V
        0xE273_4000, // rsbs r4, r3, #0      -> -0x7FFFFFFF, N
        0xE0B4_5004, // adcs r5, r4, r4      -> carry in 0
    ]);
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.reg(1), 0);
    assert_eq!(flags(&cpu), (false, true, true, false));
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.reg(3), 0x7FFF_FFFF);
    assert_eq!(flags(&cpu), (false, false, true, true));
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(4), 0x8000_0001);
    assert_eq!(flags(&cpu), (true, false, false, false));
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(5), 2);
    assert_eq!(flags(&cpu), (false, false, true, true));
}

#[test]
fn sbc_and_rsc_borrow_through_the_carry_flag() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_0005, // mov  r0, #5
        0xE3A0_1003, // mov  r1, #3
        0xE150_0001, // cmp  r0, r1          -> C set (no borrow)
        0xE0D0_2001, // sbcs r2, r0, r1      -> 2
        0xE151_0000, // cmp  r1, r0          -> C clear
        0xE0D0_3001, // sbcs r3, r0, r1      -> 1
        0xE0F0_4001, // rscs r4, r0, r1      -> 3 - 5 - !C
    ]);
    run(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.reg(2), 2);
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.reg(3), 1);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(4), 0xFFFF_FFFE);
    assert_eq!(flags(&cpu), (true, false, false, false));
}

#[test]
fn logical_operations_take_carry_from_the_shifter() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_1003, // mov  r1, #3
        0xE1B0_00A1, // movs r0, r1, lsr #1  -> 1, C
        0xE1B0_0061, // movs r0, r1, rrx     -> 0x80000001, C
        0xE3A0_2020, // mov  r2, #32
        0xE1B0_0211, // movs r0, r1, lsl r2  -> 0, C = bit 0
        0xE3B0_0102, // movs r0, #0x80000000 -> N, C from the rotation
        0xE1B0_0001, // movs r0, r1          -> C unchanged
        0xE111_0001, // tst  r1, r1
        0xE131_0001, // teq  r1, r1          -> Z
    ]);
    run(&mut cpu, &mut bus, 2);
    assert_eq!((cpu.reg(0), flags(&cpu).2), (1, true));
    run(&mut cpu, &mut bus, 1);
    assert_eq!((cpu.reg(0), flags(&cpu).2), (0x8000_0001, true));
    run(&mut cpu, &mut bus, 2);
    assert_eq!((cpu.reg(0), flags(&cpu)), (0, (false, true, true, false)));
    run(&mut cpu, &mut bus, 1);
    assert_eq!(flags(&cpu), (true, false, true, false));
    run(&mut cpu, &mut bus, 1);
    assert_eq!(flags(&cpu), (false, false, true, false));
    run(&mut cpu, &mut bus, 2);
    assert!(flags(&cpu).1);
}

#[test]
fn bitwise_operations() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_00F0, // mov r0, #0xF0
        0xE3A0_103C, // mov r1, #0x3C
        0xE000_2001, // and r2, r0, r1
        0xE020_3001, // eor r3, r0, r1
        0xE180_4001, // orr r4, r0, r1
        0xE1C0_5001, // bic r5, r0, r1
        0xE1E0_6001, // mvn r6, r1
    ]);
    run(&mut cpu, &mut bus, 7);
    assert_eq!(cpu.reg(2), 0x30);
    assert_eq!(cpu.reg(3), 0xCC);
    assert_eq!(cpu.reg(4), 0xFC);
    assert_eq!(cpu.reg(5), 0xC0);
    assert_eq!(cpu.reg(6), !0x3C);
}

#[test]
fn r15_reads_as_the_instruction_plus_8_and_stores_as_plus_12() {
    let (mut cpu, mut bus) = arm(&[
        0xE1A0_000F, // mov r0, pc
        0xE58D_F000, // str pc, [sp]
        0xE28F_1004, // add r1, pc, #4
    ]);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.reg(0), BASE + 8);
    assert_eq!(bus.word(0x8000), BASE + 4 + 12);
    assert_eq!(cpu.reg(1), BASE + 8 + 8 + 4);
}

#[test]
fn conditions_follow_the_flags() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_0000, // mov   r0, #0
        0xE350_0000, // cmp   r0, #0
        0x03A0_1001, // moveq r1, #1
        0x13A0_2001, // movne r2, #1
        0xA3A0_3001, // movge r3, #1
        0xB3A0_4001, // movlt r4, #1
        0x83A0_5001, // movhi r5, #1
        0x93A0_6001, // movls r6, #1
    ]);
    run(&mut cpu, &mut bus, 8);
    assert_eq!([1, 2, 3, 4, 5, 6].map(|n| cpu.reg(n)), [1, 0, 1, 0, 0, 1]);
}

#[test]
fn branches_link_and_exchange() {
    let (mut cpu, mut bus) = arm(&[
        0xEB00_0001, // bl   +1 word  (to BASE + 12)
        0xE1A0_0000, // nop
        0xE1A0_0000, // nop
        0xE28F_0005, // add  r0, pc, #5   (BASE + 12 + 8 + 5: odd -> Thumb)
        0xE12F_FF30, // blx  r0
    ]);
    run(&mut cpu, &mut bus, 1);
    assert_eq!((cpu.reg(15), cpu.reg(14)), (BASE + 12, BASE + 4));
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.reg(15), BASE + 0x18);
    assert!(cpu.thumb());
    assert_eq!(cpu.reg(14), BASE + 20);
}

#[test]
fn blx_immediate_enters_thumb_with_the_h_bit() {
    let (mut cpu, mut bus) = arm(&[
        0xFB00_0000, // blx +2 (H set): BASE + 8 + 2
    ]);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(15), BASE + 10);
    assert!(cpu.thumb());
    assert_eq!(cpu.reg(14), BASE + 4);
}

#[test]
fn multiplies() {
    let (mut cpu, mut bus) = arm(&[
        0xE3E0_0000, // mvn   r0, #0          (-1)
        0xE3A0_1003, // mov   r1, #3
        0xE012_0190, // muls  r2, r0, r1      -> -3, N
        0xE023_1190, // mla   r3, r0, r1, r1  -> 0
        0xE085_4190, // umull r4, r5, r0, r1  -> 0x2_FFFFFFFD
        0xE0C7_6190, // smull r6, r7, r0, r1  -> -3
        0xE0A5_4190, // umlal r4, r5, r0, r1  -> 0x5_FFFFFFFA
        0xE0F7_6190, // smlals r6, r7, r0, r1 -> -6, N
    ]);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.reg(2), 0xFFFF_FFFD);
    assert!(flags(&cpu).0);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.reg(3), 0);
    assert_eq!((cpu.reg(5), cpu.reg(4)), (2, 0xFFFF_FFFD));
    assert_eq!((cpu.reg(7), cpu.reg(6)), (0xFFFF_FFFF, 0xFFFF_FFFD));
    run(&mut cpu, &mut bus, 2);
    assert_eq!((cpu.reg(5), cpu.reg(4)), (5, 0xFFFF_FFFA));
    assert_eq!((cpu.reg(7), cpu.reg(6)), (0xFFFF_FFFF, 0xFFFF_FFFA));
    assert!(flags(&cpu).0);
}

#[test]
fn count_leading_zeros() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_1001, // mov r1, #1
        0xE16F_0F11, // clz r0, r1
        0xE3A0_1000, // mov r1, #0
        0xE16F_2F11, // clz r2, r1
    ]);
    run(&mut cpu, &mut bus, 4);
    assert_eq!((cpu.reg(0), cpu.reg(2)), (31, 32));
}

#[test]
fn saturating_arithmetic_sets_q_and_keeps_it() {
    let (mut cpu, mut bus) = arm(&[
        0xE3E0_0102, // mvn   r0, #0x80000000  (0x7FFFFFFF)
        0xE3A0_1001, // mov   r1, #1
        0xE101_2050, // qadd  r2, r0, r1       -> saturates
        0xE3A0_3102, // mov   r3, #0x80000000
        0xE121_4053, // qsub  r4, r3, r1       -> saturates low
        0xE141_5051, // qdadd r5, r1, r1       -> 1 + 2
        0xE160_6051, // qdsub r6, r1, r0       -> 1 - sat(2 * 0x7FFFFFFF)
    ]);
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.cpsr() & psr::Q, 0);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(2), 0x7FFF_FFFF);
    assert_ne!(cpu.cpsr() & psr::Q, 0);
    run(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.reg(4), 0x8000_0000);
    assert_eq!(cpu.reg(5), 3);
    assert_eq!(cpu.reg(6), 0x8000_0002);
    assert_ne!(cpu.cpsr() & psr::Q, 0);
}

#[test]
fn dsp_multiplies_use_signed_halves() {
    let (mut cpu, mut bus) = arm(&[
        0xE59F_0020, // ldr     r0, [pc, #0x20]   = 0x0003FFFE  (hi 3, lo -2)
        0xE59F_1020, // ldr     r1, [pc, #0x20]   = 0xFFFC0005  (hi -4, lo 5)
        0xE162_0180, // smulbb  r2, r0, r1        -> -2 * 5
        0xE163_01E0, // smultt  r3, r0, r1        -> 3 * -4
        0xE164_01A0, // smultb  r4, r0, r1        -> 3 * 5
        0xE165_01C0, // smulbt  r5, r0, r1        -> -2 * -4
        0xE106_5180, // smlabb  r6, r0, r1, r5    -> -10 + 8
        0xE127_01A0, // smulwb  r7, r0, r1        -> (0x3FFFE * 5) >> 16
        0xE148_7180, // smlalbb r7, r8, r0, r1    -> r8:r7 += -10
        0xEAFF_FFFE, // b .
        0x0003_FFFE,
        0xFFFC_0005,
    ]);
    run(&mut cpu, &mut bus, 8);
    assert_eq!(cpu.reg(2) as i32, -10);
    assert_eq!(cpu.reg(3) as i32, -12);
    assert_eq!(cpu.reg(4) as i32, 15);
    assert_eq!(cpu.reg(5) as i32, 8);
    assert_eq!(cpu.reg(6) as i32, -2);
    assert_eq!(cpu.reg(7), ((0x3FFFEu64 * 5) >> 16) as u32);
    let before = (cpu.reg(8) as u64) << 32 | cpu.reg(7) as u64;
    run(&mut cpu, &mut bus, 1);
    let after = (cpu.reg(8) as u64) << 32 | cpu.reg(7) as u64;
    assert_eq!(after, before.wrapping_sub(10));
    assert_eq!(cpu.cpsr() & psr::Q, 0);
}

#[test]
fn smlaxy_overflow_sets_q_without_saturating() {
    let (mut cpu, mut bus) = arm(&[
        0xE3E0_0102, // mvn    r0, #0x80000000
        0xE3A0_1002, // mov    r1, #2
        0xE102_0181, // smlabb r2, r1, r1, r0   -> 4 + 0x7FFFFFFF wraps
    ]);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.reg(2), 0x8000_0003);
    assert_ne!(cpu.cpsr() & psr::Q, 0);
}

#[test]
fn status_register_access_switches_banks() {
    let (mut cpu, mut bus) = arm(&[
        0xE10F_0000, // mrs r0, cpsr
        0xE3A0_D0AA, // mov sp, #0xAA          (system/user sp)
        0xE321_F0D2, // msr cpsr_c, #0xD2      -> IRQ mode, I F set
        0xE3A0_D0BB, // mov sp, #0xBB          (irq sp)
        0xE3A0_1011, // mov r1, #0x11
        0xE169_F001, // msr spsr_fc, r1
        0xE14F_2000, // mrs r2, spsr
        0xE321_F0DF, // msr cpsr_c, #0xDF      -> system
    ]);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(0), mode::SYS);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.cpsr() & psr::MODE, mode::IRQ);
    assert_eq!(cpu.reg(13), 0xBB);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.reg(2), 0x11);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(13), 0xAA);
}

#[test]
fn user_mode_cannot_change_the_control_bits() {
    let (mut cpu, mut bus) = arm(&[
        0xE329_F20F, // msr cpsr_fc, #0xF0000000 (writes N Z C V, and tries mode 0)
    ]);
    cpu.set_cpsr(mode::USR);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.cpsr(), mode::USR | psr::N | psr::Z | psr::C | psr::V);
}

#[test]
fn fiq_banks_r8_to_r12() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_8001, // mov r8, #1
        0xE321_F0D1, // msr cpsr_c, #0xD1  -> FIQ
        0xE3A0_8002, // mov r8, #2
        0xE321_F0DF, // msr cpsr_c, #0xDF  -> system
    ]);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.reg(8), 2);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(8), 1);
}

#[test]
fn single_loads_and_stores_index_and_write_back() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_0A02, // mov  r0, #0x2000
        0xE3A0_10AB, // mov  r1, #0xAB
        0xE580_1004, // str  r1, [r0, #4]
        0xE5A0_1008, // str  r1, [r0, #8]!     -> r0 = 0x2008
        0xE400_1004, // str  r1, [r0], #-4     -> r0 = 0x2004
        0xE590_2000, // ldr  r2, [r0]
        0xE3A0_3001, // mov  r3, #1
        0xE790_4103, // ldr  r4, [r0, r3, lsl #2]
        0xE5D0_5000, // ldrb r5, [r0]
        0xE5C0_1001, // strb r1, [r0, #1]
        0xE590_6000, // ldr  r6, [r0]
    ]);
    run(&mut cpu, &mut bus, 5);
    assert_eq!(cpu.reg(0), 0x2004);
    assert_eq!(bus.word(0x2004), 0xAB);
    assert_eq!(bus.word(0x2008), 0xAB);
    run(&mut cpu, &mut bus, 6);
    assert_eq!(cpu.reg(2), 0xAB);
    assert_eq!(cpu.reg(4), 0xAB);
    assert_eq!(cpu.reg(5), 0xAB);
    assert_eq!(cpu.reg(6), 0xABAB);
}

#[test]
fn word_accesses_ignore_the_low_address_bits() {
    let (mut cpu, mut bus) = arm(&[
        0xE59F_0004, // ldr r0, [pc, #4]    = 0x2002
        0xE590_1000, // ldr r1, [r0]
        0xEAFF_FFFE, // b .
        0x0000_2002,
    ]);
    bus.set_word(0x2000, 0x1122_3344);
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.reg(1), 0x1122_3344);
}

#[test]
fn halfword_and_signed_loads() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_0A02, // mov   r0, #0x2000
        0xE1D0_10B0, // ldrh  r1, [r0]
        0xE1D0_20F0, // ldrsh r2, [r0]
        0xE1D0_30D1, // ldrsb r3, [r0, #1]
        0xE1C0_10B4, // strh  r1, [r0, #4]
        0xE3A0_4004, // mov   r4, #4
        0xE190_50B4, // ldrh  r5, [r0, r4]
    ]);
    bus.set_word(0x2000, 0xFFFF_8180);
    run(&mut cpu, &mut bus, 7);
    assert_eq!(cpu.reg(1), 0x8180);
    assert_eq!(cpu.reg(2), 0xFFFF_8180);
    assert_eq!(cpu.reg(3), 0xFFFF_FF81);
    assert_eq!(bus.word(0x2004), 0x8180);
    assert_eq!(cpu.reg(5), 0x8180);
}

#[test]
fn doubleword_loads_and_stores() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_0A02, // mov  r0, #0x2000
        0xE1C0_20D0, // ldrd r2, [r0]
        0xE1C0_20F8, // strd r2, [r0, #8]
    ]);
    bus.set_word(0x2000, 0x1111_1111);
    bus.set_word(0x2004, 0x2222_2222);
    run(&mut cpu, &mut bus, 3);
    assert_eq!((cpu.reg(2), cpu.reg(3)), (0x1111_1111, 0x2222_2222));
    assert_eq!(bus.word(0x2008), 0x1111_1111);
    assert_eq!(bus.word(0x200C), 0x2222_2222);
}

#[test]
fn loading_r15_interworks() {
    let (mut cpu, mut bus) = arm(&[
        0xE59F_F000, // ldr pc, [pc]      = 0x3001
        0xE1A0_0000, // nop
        0x0000_3001,
    ]);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(15), 0x3000);
    assert!(cpu.thumb());
}

#[test]
fn block_transfers_in_all_four_directions() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_1001, // mov   r1, #1
        0xE3A0_2002, // mov   r2, #2
        0xE3A0_0A02, // mov   r0, #0x2000
        0xE8A0_0006, // stmia r0!, {r1, r2}   -> 0x2000, 0x2004; r0 = 0x2008
        0xE9A0_0006, // stmib r0!, {r1, r2}   -> 0x200C, 0x2010; r0 = 0x2010
        0xE820_0006, // stmda r0!, {r1, r2}   -> 0x200C, 0x2010; r0 = 0x2008
        0xE920_0006, // stmdb r0!, {r1, r2}   -> 0x2000, 0x2004; r0 = 0x2000
        0xE8B0_0018, // ldmia r0!, {r3, r4}
        0xE930_0060, // ldmdb r0!, {r5, r6}
    ]);
    run(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.reg(0), 0x2008);
    assert_eq!((bus.word(0x2000), bus.word(0x2004)), (1, 2));
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(0), 0x2010);
    assert_eq!((bus.word(0x200C), bus.word(0x2010)), (1, 2));
    bus.set_word(0x200C, 0);
    bus.set_word(0x2010, 0);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(0), 0x2008);
    assert_eq!((bus.word(0x200C), bus.word(0x2010)), (1, 2));
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(0), 0x2000);
    run(&mut cpu, &mut bus, 2);
    assert_eq!((cpu.reg(3), cpu.reg(4)), (1, 2));
    assert_eq!((cpu.reg(5), cpu.reg(6)), (1, 2));
    assert_eq!(cpu.reg(0), 0x2000);
}

#[test]
fn block_transfers_with_the_base_in_the_list() {
    // STM stores the original base wherever it sits in the list.
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_0A02, // mov   r0, #0x2000
        0xE3A0_1007, // mov   r1, #7
        0xE8A0_0003, // stmia r0!, {r0, r1}
    ]);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(bus.word(0x2000), 0x2000);
    assert_eq!(cpu.reg(0), 0x2008);

    // LDM: the base is the last register, so the loaded value stays.
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_1A02, // mov   r1, #0x2000
        0xE8B1_0003, // ldmia r1!, {r0, r1}
    ]);
    bus.set_word(0x2000, 0x11);
    bus.set_word(0x2004, 0x22);
    run(&mut cpu, &mut bus, 2);
    assert_eq!((cpu.reg(0), cpu.reg(1)), (0x11, 0x22));

    // LDM: the base is not last, so the written-back value wins.
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_0A02, // mov   r0, #0x2000
        0xE8B0_0003, // ldmia r0!, {r0, r1}
    ]);
    bus.set_word(0x2000, 0x11);
    bus.set_word(0x2004, 0x22);
    run(&mut cpu, &mut bus, 2);
    assert_eq!((cpu.reg(0), cpu.reg(1)), (0x2008, 0x22));
}

#[test]
fn block_transfer_of_the_user_bank_and_exception_return() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_D0AA, // mov   sp, #0xAA        (system sp)
        0xE321_F0D3, // msr   cpsr_c, #0xD3    -> supervisor
        0xE3A0_D0BB, // mov   sp, #0xBB
        0xE3A0_0A02, // mov   r0, #0x2000
        0xE8C0_2000, // stmia r0, {sp}^        -> stores the user sp
        0xE3A0_1010, // mov   r1, #0x10
        0xE169_F001, // msr   spsr_fc, r1      -> return to user mode
        0xE8D0_8000, // ldmia r0, {pc}^        -> pc = 0xAA & ~3, cpsr = spsr
    ]);
    run(&mut cpu, &mut bus, 5);
    assert_eq!(bus.word(0x2000), 0xAA);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.cpsr(), mode::USR);
    assert_eq!(cpu.reg(15), 0xA8);
    assert_eq!(cpu.reg(13), 0xAA);
}

#[test]
fn swap_is_a_read_then_a_write() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_0A02, // mov  r0, #0x2000
        0xE3A0_1055, // mov  r1, #0x55
        0xE100_2091, // swp  r2, r1, [r0]
        0xE140_3091, // swpb r3, r1, [r0]
    ]);
    bus.set_word(0x2000, 0x1234_5678);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.reg(2), 0x1234_5678);
    assert_eq!(bus.word(0x2000), 0x55);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(3), 0x55);
}

#[test]
fn supervisor_call_enters_and_returns() {
    let (mut cpu, mut bus) = arm(&[
        0xEF00_0042, // svc #0x42
        0xE3A0_0001, // mov r0, #1
    ]);
    bus.set_word(0x08, 0xE1B0_F00E); // movs pc, lr
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(15), 0x08);
    assert_eq!(cpu.cpsr(), mode::SVC | psr::I);
    assert_eq!(cpu.reg(14), BASE + 4);
    assert_eq!(cpu.spsr(), mode::SYS);
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.cpsr(), mode::SYS);
    assert_eq!(cpu.reg(0), 1);
}

#[test]
fn undefined_and_breakpoint_exceptions() {
    let (mut cpu, mut bus) = arm(&[0xE7F0_00F0]); // udf
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(15), 0x04);
    assert_eq!(cpu.cpsr() & psr::MODE, mode::UND);
    assert_eq!(cpu.reg(14), BASE + 4);

    let (mut cpu, mut bus) = arm(&[0xE120_0070]); // bkpt
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(15), 0x0C);
    assert_eq!(cpu.cpsr() & psr::MODE, mode::ABT);
    assert_eq!(cpu.reg(14), BASE + 4);
}

#[test]
fn coprocessor_instructions_without_a_coprocessor_are_undefined() {
    let (mut cpu, mut bus) = arm(&[0xEE11_0E10]); // mrc p14, 0, r0, c1, c0, 0
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.cpsr() & psr::MODE, mode::UND);
}

#[test]
fn data_abort_restores_the_base_and_links_to_plus_8() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_0A03, // mov r0, #0x3000
        0xE5B0_1004, // ldr r1, [r0, #4]!
    ]);
    bus.no_access = 0x3000..0x4000;
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.reg(15), 0x10);
    assert_eq!(cpu.cpsr() & psr::MODE, mode::ABT);
    assert_eq!(cpu.reg(14), BASE + 4 + 8);
    assert_eq!(cpu.reg(0), 0x3000);
}

#[test]
fn block_transfer_abort_leaves_the_base_alone() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_0A03, // mov   r0, #0x3000
        0xE920_0006, // stmdb r0!, {r1, r2}  -> 0x2FF8 is fine, 0x2FFC aborts
    ]);
    bus.no_access = 0x2FFC..0x3000;
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.cpsr() & psr::MODE, mode::ABT);
    assert_eq!(cpu.reg(0), 0x3000);
}

#[test]
fn prefetch_abort_links_to_plus_4() {
    let (mut cpu, mut bus) = arm(&[
        0xE59F_F000, // ldr pc, [pc]
        0xE1A0_0000, // nop
        0xDEAD_0000,
    ]);
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.reg(15), 0x0C);
    assert_eq!(cpu.reg(14), 0xDEAD_0004);
}

#[test]
fn irq_is_taken_between_instructions_when_unmasked() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_0001, // mov r0, #1
        0xE3A0_0002, // mov r0, #2
    ]);
    bus.set_word(0x18, 0xE25E_F004); // subs pc, lr, #4
    cpu.set_cpsr(mode::SYS | psr::I);
    cpu.irq_line = true;
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(0), 1, "masked: the instruction runs");

    cpu.set_cpsr(mode::SYS);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(15), 0x18);
    assert_eq!(cpu.cpsr(), mode::IRQ | psr::I);
    assert_eq!(cpu.reg(14), BASE + 4 + 4);
    cpu.irq_line = false;
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.cpsr(), mode::SYS);
    assert_eq!(cpu.reg(0), 2);
}

#[test]
fn fiq_masks_both_interrupts_and_uses_high_vectors() {
    let (mut cpu, mut bus) = arm(&[0xE1A0_0000]);
    bus.high_vectors = true;
    cpu.fiq_line = true;
    cpu.irq_line = true;
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(15), 0xFFFF_001C);
    assert_eq!(cpu.cpsr(), mode::FIQ | psr::I | psr::F);
}

#[test]
fn wait_for_interrupt_halts_until_a_line_rises() {
    let (mut cpu, mut bus) = arm(&[
        0xEE07_0F90, // mcr p15, 0, r0, c7, c0, 4
        0xE3A0_0001, // mov r0, #1
    ]);
    cpu.set_cpsr(mode::SYS | psr::I);
    run(&mut cpu, &mut bus, 4);
    assert!(cpu.halted());
    assert_eq!(cpu.reg(0), 0);
    // A masked interrupt still wakes the processor, which carries on.
    cpu.irq_line = true;
    run(&mut cpu, &mut bus, 1);
    assert!(!cpu.halted());
    assert_eq!(cpu.reg(0), 1);
}

#[test]
fn control_register_round_trip_and_flag_transfer() {
    let (mut cpu, mut bus) = arm(&[
        0xEE11_0F10, // mrc p15, 0, r0, c1, c0, 0
        0xE380_0A02, // orr r0, r0, #0x2000
        0xEE01_0F10, // mcr p15, 0, r0, c1, c0, 0
        0xE3A0_020F, // mov r0, #0xF0000000
        0xEE01_0F10, // mcr p15, 0, r0, c1, c0, 0
        0xEE11_FF10, // mrc p15, 0, pc, c1, c0, 0  -> flags only
    ]);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(bus.control, 0x2078);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(flags(&cpu), (true, true, true, true));
    assert_eq!(cpu.reg(15), BASE + 24);
}

#[test]
fn translated_stores_are_unprivileged() {
    let (mut cpu, mut bus) = arm(&[
        0xE3A0_0A02, // mov  r0, #0x2000
        0xE420_1004, // strt r1, [r0], #-4
        0xE580_1000, // str  r1, [r0]
    ]);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(bus.unprivileged_accesses, 1);
}

#[test]
fn preload_is_a_hint_and_other_unconditional_encodings_are_undefined() {
    let (mut cpu, mut bus) = arm(&[
        0xF5D0_F000, // pld [r0]
        0xF000_0000,
    ]);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(15), BASE + 4);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.cpsr() & psr::MODE, mode::UND);
}

#[test]
fn exception_entry_by_hand() {
    let mut bus = Flat::new();
    let mut cpu = Cpu::new(Arch::V5te, &bus);
    cpu.set_cpsr(mode::USR | psr::T);
    cpu.enter(&bus, Exception::Irq, 0x1234);
    assert_eq!(cpu.cpsr(), mode::IRQ | psr::I);
    assert_eq!(cpu.spsr(), mode::USR | psr::T);
    assert_eq!(cpu.reg(14), 0x1234);
    bus.high_vectors = true;
    cpu.enter(&bus, Exception::Reset, 0);
    assert_eq!(cpu.reg(15), 0xFFFF_0000);
    assert_eq!(cpu.cpsr(), mode::SVC | psr::I | psr::F);
}

// Thumb.

#[test]
fn thumb_shifts_and_small_arithmetic() {
    let (mut cpu, mut bus) = thumb(&[
        0x2103, // mov r1, #3
        0x0048, // lsl r0, r1, #1   -> 6
        0x0849, // lsr r1, r1, #1   -> 1, C
        0x2280, // mov r2, #0x80
        0x0612, // lsl r2, r2, #24  -> 0x80000000
        0x1053, // asr r3, r2, #1   -> 0xC0000000
        0x18C4, // add r4, r0, r3
        0x1A45, // sub r5, r0, r1   -> 5
        0x1CC6, // add r6, r0, #3   -> 9
        0x1EC7, // sub r7, r0, #3   -> 3
    ]);
    run(&mut cpu, &mut bus, 3);
    assert_eq!((cpu.reg(0), cpu.reg(1)), (6, 1));
    assert!(flags(&cpu).2);
    run(&mut cpu, &mut bus, 7);
    assert_eq!(cpu.reg(3), 0xC000_0000);
    assert_eq!(cpu.reg(4), 0xC000_0006);
    assert_eq!((cpu.reg(5), cpu.reg(6), cpu.reg(7)), (5, 9, 3));
}

#[test]
fn thumb_immediate_operations_set_flags() {
    let (mut cpu, mut bus) = thumb(&[
        0x20FF, // mov r0, #0xFF
        0x28FF, // cmp r0, #0xFF  -> Z C
        0x3001, // add r0, #1
        0x3802, // sub r0, #2
        0x2000, // mov r0, #0     -> Z
    ]);
    run(&mut cpu, &mut bus, 2);
    assert_eq!(flags(&cpu), (false, true, true, false));
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.reg(0), 0xFE);
    run(&mut cpu, &mut bus, 1);
    assert!(flags(&cpu).1);
}

#[test]
fn thumb_register_operations() {
    let (mut cpu, mut bus) = thumb(&[
        0x20F0, // mov r0, #0xF0
        0x213C, // mov r1, #0x3C
        0x4008, // and r0, r1   -> 0x30
        0x4048, // eor r0, r1   -> 0x0C
        0x4308, // orr r0, r1   -> 0x3C
        0x2204, // mov r2, #4
        0x4090, // lsl r0, r2   -> 0x3C0
        0x40D0, // lsr r0, r2   -> 0x3C
        0x4348, // mul r0, r1   -> 0xE10
        0x4388, // bic r0, r1
        0x43C8, // mvn r0, r1
        0x4248, // neg r0, r1
        0x4288, // cmp r0, r1
        0x42C8, // cmn r0, r1   -> Z
        0x4208, // tst r0, r1
    ]);
    run(&mut cpu, &mut bus, 5);
    assert_eq!(cpu.reg(0), 0x3C);
    run(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.reg(0), 0xE10);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(0), 0xE00);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(0), !0x3C);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(0), 0x3Cu32.wrapping_neg());
    run(&mut cpu, &mut bus, 2);
    assert!(flags(&cpu).1);
}

#[test]
fn thumb_carry_operations_and_rotate() {
    let (mut cpu, mut bus) = thumb(&[
        0x2001, // mov r0, #1
        0x2101, // mov r1, #1
        0x0840, // lsr r0, r0, #1  -> 0, C
        0x4148, // adc r0, r1      -> 2
        0x4188, // sbc r0, r1      -> 2 - 1 - !C(0) = 0 with C clear from adc
        0x2003, // mov r0, #3
        0x41C8, // ror r0, r1      -> 0x80000001, C
        0x4108, // asr r0, r1      -> 0xC0000000, C
    ]);
    run(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.reg(0), 2);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(0), 0);
    run(&mut cpu, &mut bus, 2);
    assert_eq!((cpu.reg(0), flags(&cpu).2), (0x8000_0001, true));
    run(&mut cpu, &mut bus, 1);
    assert_eq!((cpu.reg(0), flags(&cpu).2), (0xC000_0000, true));
}

#[test]
fn thumb_high_registers_and_exchange() {
    let (mut cpu, mut bus) = thumb(&[
        0x2005, // mov r0, #5
        0x4680, // mov r8, r0
        0x4440, // add r0, r8   -> 10
        0x4540, // cmp r0, r8   -> C
        0x4679, // mov r1, pc   -> BASE + 8 + 4
        0x4788, // blx r1       -> ARM state at BASE + 12
    ]);
    run(&mut cpu, &mut bus, 4);
    assert_eq!((cpu.reg(0), cpu.reg(8)), (10, 5));
    assert!(flags(&cpu).2);
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.reg(1), BASE + 12);
    assert!(!cpu.thumb());
    assert_eq!(cpu.reg(15), BASE + 12);
    assert_eq!(cpu.reg(14), (BASE + 12) | 1);
}

#[test]
fn thumb_loads_and_stores() {
    let (mut cpu, mut bus) = thumb(&[
        0x4806, // ldr  r0, [pc, #24]   -> literal at BASE + 0x1C
        0x2120, // mov  r1, #0x20
        0x0209, // lsl  r1, r1, #8      -> 0x2000
        0x6048, // str  r0, [r1, #4]
        0x684A, // ldr  r2, [r1, #4]
        0x2304, // mov  r3, #4
        0x5ACC, // ldrh r4, [r1, r3]
        0x56CD, // ldrsb r5, [r1, r3]
        0x5ECE, // ldrsh r6, [r1, r3]
        0x7949, // ldrb r1, [r1, #5]
        0x9001, // str  r0, [sp, #4]
        0x9F01, // ldr  r7, [sp, #4]
        0xE7FE, // b .
        0x0000, 0x8182, // literal 0xFFFF8182
        0xFFFF,
    ]);
    run(&mut cpu, &mut bus, 12);
    assert_eq!(cpu.reg(0), 0xFFFF_8182);
    assert_eq!(cpu.reg(2), 0xFFFF_8182);
    assert_eq!(cpu.reg(4), 0x8182);
    assert_eq!(cpu.reg(5), 0xFFFF_FF82);
    assert_eq!(cpu.reg(6), 0xFFFF_8182);
    assert_eq!(cpu.reg(1), 0x81);
    assert_eq!(bus.word(0x8004), 0xFFFF_8182);
    assert_eq!(cpu.reg(7), 0xFFFF_8182);
}

#[test]
fn thumb_address_generation_and_stack_adjustment() {
    let (mut cpu, mut bus) = thumb(&[
        0x46C0, // nop (mov r8, r8)
        0xA001, // add r0, pc, #4   -> ((BASE + 2 + 4) & ~3) + 4
        0xA902, // add r1, sp, #8
        0xB082, // sub sp, #8
        0xB001, // add sp, #4
    ]);
    run(&mut cpu, &mut bus, 5);
    assert_eq!(cpu.reg(0), ((BASE + 6) & !3) + 4);
    assert_eq!(cpu.reg(1), 0x8008);
    assert_eq!(cpu.reg(13), 0x7FFC);
}

#[test]
fn thumb_push_pop_and_interworking_return() {
    let (mut cpu, mut bus) = thumb(&[
        0x2011, // mov  r0, #0x11
        0x2122, // mov  r1, #0x22
        0xB503, // push {r0, r1, lr}
        0x2000, // mov  r0, #0
        0xBD03, // pop  {r0, r1, pc}
    ]);
    cpu.set_reg(14, 0x3000);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.reg(13), 0x8000 - 12);
    assert_eq!(bus.word(0x8000 - 4), 0x3000);
    run(&mut cpu, &mut bus, 2);
    assert_eq!((cpu.reg(0), cpu.reg(1)), (0x11, 0x22));
    assert_eq!(cpu.reg(13), 0x8000);
    assert_eq!(cpu.reg(15), 0x3000);
    assert!(!cpu.thumb(), "bit 0 clear returns to ARM state");
}

#[test]
fn thumb_multiple_loads_and_stores() {
    let (mut cpu, mut bus) = thumb(&[
        0x2020, // mov   r0, #0x20
        0x0200, // lsl   r0, r0, #8
        0x2101, // mov   r1, #1
        0x2202, // mov   r2, #2
        0xC006, // stmia r0!, {r1, r2}
        0x3808, // sub   r0, #8
        0xC818, // ldmia r0!, {r3, r4}
        0x3808, // sub   r0, #8
        0xC801, // ldmia r0!, {r0}       -> no writeback
    ]);
    run(&mut cpu, &mut bus, 5);
    assert_eq!(cpu.reg(0), 0x2008);
    run(&mut cpu, &mut bus, 2);
    assert_eq!((cpu.reg(3), cpu.reg(4), cpu.reg(0)), (1, 2, 0x2008));
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.reg(0), 1);
}

#[test]
fn thumb_branches() {
    let (mut cpu, mut bus) = thumb(&[
        0x2000, // mov r0, #0
        0xD001, // beq +1      -> BASE + 2 + 4 + 2 = BASE + 8
        0x2001, // mov r0, #1  (skipped)
        0x2002, // mov r0, #2  (skipped)
        0xD100, // bne +0      (not taken)
        0xE7FA, // b   -6      -> BASE + 10 + 4 - 12 = BASE + 2
    ]);
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.reg(15), BASE + 8);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(15), BASE + 10);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(15), BASE + 2);
}

#[test]
fn thumb_long_branches_with_link() {
    let (mut cpu, mut bus) = thumb(&[
        0xF000, // bl prefix, high offset 0
        0xF802, // bl suffix, +4       -> BASE + 4 + 4
        0x0000, 0x0000, //
        0xF7FF, // bl prefix, high offset -1
        0xEFFC, // blx suffix          -> ARM at (BASE + 12 - 0x1000 + 0xFF8) & ~3
    ]);
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.reg(15), BASE + 8);
    assert_eq!(cpu.reg(14), (BASE + 4) | 1);
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.reg(15), BASE + 4);
    assert!(!cpu.thumb());
    assert_eq!(cpu.reg(14), (BASE + 12) | 1);
}

#[test]
fn thumb_exceptions_return_to_the_next_instruction() {
    let (mut cpu, mut bus) = thumb(&[0xDF05]); // svc 5
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(15), 0x08);
    assert!(!cpu.thumb());
    assert_eq!(cpu.reg(14), BASE + 2);
    assert_eq!(cpu.spsr(), mode::SYS | psr::T);

    let (mut cpu, mut bus) = thumb(&[0xBE00]); // bkpt
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(15), 0x0C);
    assert_eq!(cpu.reg(14), BASE + 4);

    let (mut cpu, mut bus) = thumb(&[0xDE00]); // udf
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(15), 0x04);
    assert_eq!(cpu.reg(14), BASE + 2);
}
