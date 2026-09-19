//! VFPv2 instruction tests. Operands go in through the register file and
//! the encodings are written out by hand.

use super::{arm_on, run, Flat};
use crate::{mode, psr, Arch, Cpu};

const FPEXC_ENABLE: u32 = 1 << 30;

fn vfp(code: &[u32]) -> (Cpu, Flat) {
    let (mut cpu, mut bus) = arm_on(Arch::V6k, code);
    bus.vfp = true;
    cpu.vfp_mut().fpexc = FPEXC_ENABLE;
    (cpu, bus)
}

fn s(cpu: &Cpu, n: usize) -> f32 {
    f32::from_bits(cpu.vfp().s[n])
}

fn d(cpu: &Cpu, n: usize) -> f64 {
    f64::from_bits(cpu.vfp().d(n))
}

#[test]
fn the_unit_is_undefined_until_access_and_enable_are_granted() {
    let fadds = 0xEE31_0A02; // fadds s0, s2, s4
    let (mut cpu, mut bus) = arm_on(Arch::V6k, &[fadds]);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.cpsr() & psr::MODE, mode::UND, "no coprocessor access");

    let (mut cpu, mut bus) = arm_on(Arch::V6k, &[fadds]);
    bus.vfp = true;
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.cpsr() & psr::MODE, mode::UND, "FPEXC.EN is clear");

    let (mut cpu, mut bus) = arm_on(Arch::V5te, &[fadds]);
    bus.vfp = true;
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.cpsr() & psr::MODE, mode::UND, "ARMv5 has no VFP");
}

#[test]
fn system_registers() {
    let (mut cpu, mut bus) = arm_on(
        Arch::V6k,
        &[
            0xE3A0_0101, // mov  r0, #0x40000000
            0xEEE8_0A10, // fmxr fpexc, r0
            0xEEF8_1A10, // fmrx r1, fpexc
            0xEEF0_2A10, // fmrx r2, fpsid
            0xE3A0_0403, // mov  r0, #0x03000000
            0xEEE1_0A10, // fmxr fpscr, r0
            0xEEF1_3A10, // fmrx r3, fpscr
        ],
    );
    bus.vfp = true;
    run(&mut cpu, &mut bus, 7);
    assert_eq!(cpu.reg(1), FPEXC_ENABLE);
    assert_eq!(cpu.reg(2), 0x4101_20B4);
    assert_eq!(cpu.reg(3), 0x0300_0000);
}

#[test]
fn single_precision_arithmetic() {
    let (mut cpu, mut bus) = vfp(&[
        0xEE31_0A02, // fadds s0, s2, s4
        0xEE31_1A42, // fsubs s2, s2, s4    (s2 is the second register: Fd=1)
        0xEE22_3A05, // fmuls s6, s4, s10
        0xEE82_4A05, // fdivs s8, s4, s10
        0xEEB1_5AC2, // fsqrts s10, s4
        0xEEB1_6A42, // fnegs s12, s4
        0xEEB0_7AC6, // fabss s14, s12
    ]);
    cpu.vfp_mut().s[2] = 1.5f32.to_bits();
    cpu.vfp_mut().s[4] = 4.0f32.to_bits();
    cpu.vfp_mut().s[10] = 0.5f32.to_bits();
    run(&mut cpu, &mut bus, 7);
    assert_eq!(s(&cpu, 0), 5.5);
    assert_eq!(s(&cpu, 2), -2.5);
    assert_eq!(s(&cpu, 6), 2.0);
    assert_eq!(s(&cpu, 8), 8.0);
    assert_eq!(s(&cpu, 10), 2.0);
    assert_eq!(s(&cpu, 12), -4.0);
    assert_eq!(s(&cpu, 14), 4.0);
}

#[test]
fn double_precision_and_multiply_accumulate() {
    let (mut cpu, mut bus) = vfp(&[
        0xEE31_0B02, // faddd d0, d1, d2
        0xEE01_3B02, // fmacd d3, d1, d2     d3 += d1 * d2
        0xEE11_4B42, // fnmscd d4, d1, d2    d4 = -d4 - d1 * d2
    ]);
    cpu.vfp_mut().set_d(1, 3.0f64.to_bits());
    cpu.vfp_mut().set_d(2, 0.25f64.to_bits());
    cpu.vfp_mut().set_d(3, 10.0f64.to_bits());
    cpu.vfp_mut().set_d(4, 1.0f64.to_bits());
    run(&mut cpu, &mut bus, 3);
    assert_eq!(d(&cpu, 0), 3.25);
    assert_eq!(d(&cpu, 3), 10.75);
    assert_eq!(d(&cpu, 4), -1.75);
}

#[test]
fn comparison_sets_the_flags_fmstat_copies() {
    let (mut cpu, mut bus) = vfp(&[
        0xEEB4_0A41, // fcmps s0, s2
        0xEEF1_FA10, // fmstat
        0xEEB5_1A40, // fcmpzs s2
        0xEEF1_FA10, // fmstat
        0xEEB4_0AC2, // fcmpes s0, s4       (unordered)
        0xEEF1_FA10, // fmstat
    ]);
    cpu.vfp_mut().s[0] = 1.0f32.to_bits();
    cpu.vfp_mut().s[2] = 2.0f32.to_bits();
    cpu.vfp_mut().s[4] = f32::NAN.to_bits();
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.cpsr() & 0xF000_0000, psr::N, "less than");
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.cpsr() & 0xF000_0000, psr::C, "greater than zero");
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.cpsr() & 0xF000_0000, psr::C | psr::V, "unordered");
    assert_ne!(
        cpu.vfp().fpscr & 1,
        0,
        "the signaling compare raised invalid"
    );
}

#[test]
fn conversions() {
    let (mut cpu, mut bus) = vfp(&[
        0xEEB8_0AC1, // fsitos s0, s2
        0xEEB8_1B41, // fuitod d1, s2
        0xEEBD_2AC3, // ftosizs s4, s6      (round towards zero)
        0xEEBD_2A43, // ftosis  s4, s6      (FPSCR rounding: to nearest)
        0xEEB7_4AC3, // fcvtds d4, s6
        0xEEB7_5BC4, // fcvtsd s10, d4
    ]);
    cpu.vfp_mut().s[2] = (-7i32) as u32;
    cpu.vfp_mut().s[6] = 2.75f32.to_bits();
    run(&mut cpu, &mut bus, 3);
    assert_eq!(s(&cpu, 0), -7.0);
    assert_eq!(d(&cpu, 1), 4_294_967_289.0);
    assert_eq!(cpu.vfp().s[4], 2);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.vfp().s[4], 3);
    run(&mut cpu, &mut bus, 2);
    assert_eq!(d(&cpu, 4), 2.75);
    assert_eq!(s(&cpu, 10), 2.75);
    assert_ne!(cpu.vfp().fpscr & 1 << 4, 0, "inexact accumulated");
}

#[test]
fn register_transfers() {
    let (mut cpu, mut bus) = vfp(&[
        0xEE00_0A90, // fmsr  s1, r0
        0xEE10_1A90, // fmrs  r1, s1
        0xEC43_2B12, // fmdrr d2, r2, r3
        0xEC55_4B12, // fmrrd r4, r5, d2
        0xEE32_6B10, // fmrdh r6, d2
    ]);
    cpu.set_reg(0, 0x1234_5678);
    cpu.set_reg(2, 0xAAAA_AAAA);
    cpu.set_reg(3, 0xBBBB_BBBB);
    run(&mut cpu, &mut bus, 5);
    assert_eq!(cpu.reg(1), 0x1234_5678);
    assert_eq!(cpu.vfp().d(2), 0xBBBB_BBBB_AAAA_AAAA);
    assert_eq!((cpu.reg(4), cpu.reg(5)), (0xAAAA_AAAA, 0xBBBB_BBBB));
    assert_eq!(cpu.reg(6), 0xBBBB_BBBB);
}

#[test]
fn loads_and_stores() {
    let (mut cpu, mut bus) = vfp(&[
        0xE3A0_0A02, // mov   r0, #0x2000
        0xED90_0A01, // flds  s0, [r0, #4]
        0xED90_1B02, // fldd  d1, [r0, #8]
        0xED80_0A08, // fsts  s0, [r0, #32]
        0xECA0_1B04, // fstmiad r0!, {d1-d2}
        0xED30_3B02, // fldmdbd r0!, {d3}
    ]);
    bus.set_word(0x2004, 0x3FC0_0000);
    bus.set_word(0x2008, 0x1111_1111);
    bus.set_word(0x200C, 0x2222_2222);
    run(&mut cpu, &mut bus, 4);
    assert_eq!(s(&cpu, 0), 1.5);
    assert_eq!(cpu.vfp().d(1), 0x2222_2222_1111_1111);
    assert_eq!(bus.word(0x2020), 0x3FC0_0000);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(0), 0x2010);
    assert_eq!(
        (bus.word(0x2000), bus.word(0x2004)),
        (0x1111_1111, 0x2222_2222)
    );
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.reg(0), 0x2008);
    assert_eq!(cpu.vfp().d(3), cpu.vfp().d(2), "loaded what d2 stored");
}

#[test]
fn short_vectors_step_within_a_bank() {
    let (mut cpu, mut bus) = vfp(&[
        0xEE34_4A06, // fadds s8, s8, s12
    ]);
    // LEN = 4 (field 3), stride 1.
    cpu.vfp_mut().fpscr = 3 << 16;
    for n in 0..4 {
        cpu.vfp_mut().s[8 + n] = (n as f32).to_bits();
        cpu.vfp_mut().s[12 + n] = 10.0f32.to_bits();
    }
    run(&mut cpu, &mut bus, 1);
    assert_eq!([8, 9, 10, 11].map(|n| s(&cpu, n)), [10.0, 11.0, 12.0, 13.0]);

    // A destination in the first bank stays scalar.
    let (mut cpu, mut bus) = vfp(&[0xEE30_0A06]); // fadds s0, s0, s12
    cpu.vfp_mut().fpscr = 3 << 16;
    cpu.vfp_mut().s[1] = 5.0f32.to_bits();
    cpu.vfp_mut().s[12] = 1.0f32.to_bits();
    run(&mut cpu, &mut bus, 1);
    assert_eq!((s(&cpu, 0), s(&cpu, 1)), (1.0, 5.0));
}

#[test]
fn run_fast_mode_flushes_and_defaults() {
    let (mut cpu, mut bus) = vfp(&[
        0xEE80_0A01, // fdivs s0, s0, s2
        0xEE32_2A03, // fadds s4, s4, s6
    ]);
    cpu.vfp_mut().fpscr = 0x0300_0000;
    cpu.vfp_mut().s[0] = f32::MIN_POSITIVE.to_bits();
    cpu.vfp_mut().s[2] = 3.0f32.to_bits();
    cpu.vfp_mut().s[4] = 0xFFC0_1234;
    cpu.vfp_mut().s[6] = 1.0f32.to_bits();
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.vfp().s[0], 0, "flushed to zero");
    assert_eq!(cpu.vfp().s[4], 0x7FC0_0000, "the default NaN");
    assert_ne!(cpu.vfp().fpscr & 1 << 3, 0, "underflow accumulated");
}
