//! ARMv6K instruction tests, on the flat memory of the ARMv5 tests.

use super::{arm_on, run, thumb_on, Flat, BASE};
use crate::{mode, psr, Arch, Cpu};

fn arm6(code: &[u32]) -> (Cpu, Flat) {
    arm_on(Arch::V6k, code)
}

#[test]
fn armv5_rejects_the_armv6_instructions() {
    for instr in [0xE6BF_0F31u32, 0xE191_0F9F, 0xE041_0392, 0xF10C_0080] {
        let (mut cpu, mut bus) = arm_on(Arch::V5te, &[instr]);
        run(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.cpsr() & psr::MODE, mode::UND, "{instr:#010x}");
    }
}

#[test]
fn byte_reversal() {
    let (mut cpu, mut bus) = arm6(&[
        0xE6BF_0F31, // rev   r0, r1
        0xE6BF_2FB1, // rev16 r2, r1
        0xE6FF_3FB1, // revsh r3, r1
    ]);
    cpu.set_reg(1, 0x1122_3380);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.reg(0), 0x8033_2211);
    assert_eq!(cpu.reg(2), 0x2211_8033);
    assert_eq!(cpu.reg(3), 0xFFFF_8033);
}

#[test]
fn extension_with_rotation_and_addition() {
    let (mut cpu, mut bus) = arm6(&[
        0xE6EF_0071, // uxtb  r0, r1
        0xE6AF_2071, // sxtb  r2, r1
        0xE6FF_3071, // uxth  r3, r1
        0xE6BF_4071, // sxth  r4, r1
        0xE6EF_5471, // uxtb  r5, r1, ror #8
        0xE6E6_7071, // uxtab r7, r6, r1
        0xE6CF_8071, // uxtb16 r8, r1
    ]);
    cpu.set_reg(1, 0x0081_F280);
    cpu.set_reg(6, 0x1000);
    run(&mut cpu, &mut bus, 7);
    assert_eq!(cpu.reg(0), 0x80);
    assert_eq!(cpu.reg(2), 0xFFFF_FF80);
    assert_eq!(cpu.reg(3), 0xF280);
    assert_eq!(cpu.reg(4), 0xFFFF_F280);
    assert_eq!(cpu.reg(5), 0xF2);
    assert_eq!(cpu.reg(7), 0x1080);
    assert_eq!(cpu.reg(8), 0x0081_0080);
}

#[test]
fn exclusive_access_stores_once() {
    let (mut cpu, mut bus) = arm6(&[
        0xE3A0_1A02, // mov   r1, #0x2000
        0xE191_0F9F, // ldrex r0, [r1]
        0xE280_0001, // add   r0, r0, #1
        0xE181_2F90, // strex r2, r0, [r1]
        0xE181_3F90, // strex r3, r0, [r1]   no longer exclusive
    ]);
    bus.set_word(0x2000, 41);
    run(&mut cpu, &mut bus, 5);
    assert_eq!(bus.word(0x2000), 42);
    assert_eq!((cpu.reg(2), cpu.reg(3)), (0, 1));
}

#[test]
fn processor_state_changes() {
    let (mut cpu, mut bus) = arm6(&[
        0xF10C_0080, // cpsid i
        0xF108_0080, // cpsie i
        0xF102_0013, // cps   #0x13
        0xF101_0200, // setend be
    ]);
    run(&mut cpu, &mut bus, 1);
    assert_ne!(cpu.cpsr() & psr::I, 0);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.cpsr() & psr::I, 0);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.cpsr() & psr::MODE, mode::SVC);
    run(&mut cpu, &mut bus, 1);
    assert_ne!(cpu.cpsr() & psr::E, 0);
}

#[test]
fn user_mode_cannot_change_the_interrupt_masks() {
    let (mut cpu, mut bus) = arm6(&[0xF10C_0080]); // cpsid i
    cpu.set_cpsr(mode::USR);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.cpsr(), mode::USR);
}

#[test]
fn return_state_round_trips_through_another_stack() {
    let (mut cpu, mut bus) = arm6(&[
        0xF96D_0513, // srsdb sp!, #0x13   (from IRQ mode, onto the SVC stack)
        0xF102_0013, // cps   #0x13
        0xF8BD_0A00, // rfeia sp!
    ]);
    cpu.set_cpsr(mode::SVC);
    cpu.set_reg(13, 0x9000);
    // Take an interrupt from system mode in Thumb state, so the SPSR of IRQ
    // mode holds that state and the link register the return address.
    cpu.set_cpsr(mode::SYS | psr::T);
    cpu.enter(&bus, crate::Exception::Irq, 0x4000);
    cpu.jump(BASE);
    let spsr = cpu.spsr();

    run(&mut cpu, &mut bus, 1);
    assert_eq!(bus.word(0x9000 - 8), 0x4000);
    assert_eq!(bus.word(0x9000 - 4), spsr);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(13), 0x9000 - 8);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(15), 0x4000);
    assert_eq!(cpu.cpsr(), spsr);
}

#[test]
fn exception_entry_masks_imprecise_aborts_except_for_und_and_svc() {
    let (mut cpu, mut bus) = arm6(&[0xEF00_0000]); // svc
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.cpsr() & psr::A, 0);

    let (mut cpu, mut bus) = arm6(&[0xE1A0_0000]);
    cpu.irq_line = true;
    run(&mut cpu, &mut bus, 1);
    assert_ne!(cpu.cpsr() & psr::A, 0);
}

#[test]
fn status_register_writes_reach_the_armv6_bits() {
    let (mut cpu, mut bus) = arm6(&[
        0xE3A0_0C03, // mov r0, #0x300        A and E
        0xE122_F000, // msr cpsr_x, r0
        0xE3A0_080F, // mov r0, #0xF0000      GE
        0xE124_F000, // msr cpsr_s, r0
    ]);
    run(&mut cpu, &mut bus, 4);
    assert_eq!(
        cpu.cpsr() & (psr::A | psr::E | psr::GE),
        psr::A | psr::E | psr::GE
    );
}

#[test]
fn unsigned_multiply_accumulate_accumulate() {
    let (mut cpu, mut bus) = arm6(&[0xE041_0392]); // umaal r0, r1, r2, r3
    cpu.set_reg(0, 0xFFFF_FFFF);
    cpu.set_reg(1, 0xFFFF_FFFF);
    cpu.set_reg(2, 0xFFFF_FFFF);
    cpu.set_reg(3, 0xFFFF_FFFF);
    run(&mut cpu, &mut bus, 1);
    assert_eq!((cpu.reg(1), cpu.reg(0)), (0xFFFF_FFFF, 0xFFFF_FFFF));
}

#[test]
fn saturation() {
    let (mut cpu, mut bus) = arm6(&[
        0xE6A7_0011, // ssat r0, #8, r1
        0xE6E8_2011, // usat r2, #8, r1
        0xE6A7_3014, // ssat r3, #8, r4
    ]);
    cpu.set_reg(1, 0x1234);
    cpu.set_reg(4, 0xFFFF_FFF0);
    run(&mut cpu, &mut bus, 2);
    assert_eq!((cpu.reg(0), cpu.reg(2)), (0x7F, 0xFF));
    assert_ne!(cpu.cpsr() & psr::Q, 0);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(3), 0xFFFF_FFF0);
}

#[test]
fn parallel_arithmetic_sets_ge_and_select_uses_it() {
    let (mut cpu, mut bus) = arm6(&[
        0xE651_0F92, // uadd8 r0, r1, r2
        0xE684_3FB5, // sel   r3, r4, r5
        0xE661_6F92, // uqadd8 r6, r1, r2
        0xE631_7F92, // shadd8 r7, r1, r2
        0xE611_8F12, // sadd16 r8, r1, r2
    ]);
    cpu.set_reg(1, 0x80FF_0110);
    cpu.set_reg(2, 0x8001_0220);
    cpu.set_reg(4, 0xAAAA_AAAA);
    cpu.set_reg(5, 0x5555_5555);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(0), 0x0000_0330);
    assert_eq!(
        cpu.cpsr() & psr::GE,
        0xC << 16,
        "carries in the top two lanes"
    );
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(3), 0xAAAA_5555);
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.reg(6), 0xFFFF_0330);
    assert_eq!(cpu.reg(7), 0x8000_0118, "signed halving");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(8), 0x0100_0330);
    assert_eq!(
        cpu.cpsr() & psr::GE,
        0x3 << 16,
        "0x80FF + 0x8001 is negative"
    );
}

#[test]
fn armv6_multiplies() {
    let (mut cpu, mut bus) = arm6(&[
        0xE750_F211, // smmul r0, r1, r2
        0xE703_F211, // smuad r3, r1, r2
        0xE784_F211, // usad8 r4, r1, r2
    ]);
    cpu.set_reg(1, 0x4000_0003);
    cpu.set_reg(2, 0x0002_0004);
    run(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.reg(0), ((0x4000_0003i64 * 0x0002_0004i64) >> 32) as u32);
    assert_eq!(cpu.reg(3), 3 * 4 + 0x4000 * 2);
    assert_eq!(cpu.reg(4), 1 + 2 + 0x40);
}

#[test]
fn wait_for_interrupt_hint_halts_and_the_others_do_nothing() {
    let (mut cpu, mut bus) = arm6(&[
        0xE320_F000, // nop
        0xE320_F002, // wfe
        0xE320_F003, // wfi
    ]);
    run(&mut cpu, &mut bus, 2);
    assert!(!cpu.halted());
    run(&mut cpu, &mut bus, 1);
    assert!(cpu.halted());
}

#[test]
fn unaligned_loads_when_the_bus_allows_them() {
    let (mut cpu, mut bus) = arm6(&[
        0xE59F_0004, // ldr r0, [pc, #4]   = 0x2001
        0xE590_1000, // ldr r1, [r0]
        0xEAFF_FFFE, // b .
        0x0000_2001,
    ]);
    bus.unaligned = true;
    bus.set_word(0x2000, 0x4433_2211);
    bus.set_word(0x2004, 0x0000_0055);
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.reg(1), 0x5544_3322);
}

#[test]
fn thumb_armv6_additions() {
    let (mut cpu, mut bus) = thumb_on(
        Arch::V6k,
        &[
            0xBA08, // rev  r0, r1
            0xB2CA, // uxtb r2, r1
            0xB20B, // sxth r3, r1
            0xB672, // cpsid i
            0xB662, // cpsie i
        ],
    );
    cpu.set_reg(1, 0x1122_F380);
    run(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.reg(0), 0x80F3_2211);
    assert_eq!(cpu.reg(2), 0x80);
    assert_eq!(cpu.reg(3), 0xFFFF_F380);
    assert_ne!(cpu.cpsr() & psr::I, 0);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.cpsr() & psr::I, 0);
}
