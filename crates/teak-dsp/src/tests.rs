//! The core running programs: Teakra's cycle tests (`tests/interpreter.cpp`)
//! and hand-assembled programs checking instruction behaviour.

use super::*;
use crate::timer::CountMode;

/// A DSP with its own RAM.
struct Dsp {
    teak: Teak,
    ram: Vec<u8>,
}

impl Dsp {
    fn new() -> Self {
        Dsp {
            teak: Teak::new(),
            ram: vec![0; RAM_LEN],
        }
    }

    /// Program words from `addr`.
    fn program(&mut self, addr: u32, words: &[u16]) {
        for (n, word) in words.iter().enumerate() {
            let at = (addr as usize + n) * 2;
            self.ram[at..at + 2].copy_from_slice(&word.to_le_bytes());
        }
    }

    /// A data word.
    fn data(&self, addr: u16) -> u16 {
        let at = 0x4_0000 + addr as usize * 2;
        u16::from_le_bytes([self.ram[at], self.ram[at + 1]])
    }

    fn run(&mut self, cycles: u64) -> u64 {
        self.teak.run(&mut Ram(&mut self.ram), cycles)
    }

    fn regs(&mut self) -> &mut Regs {
        &mut self.teak.regs
    }
}

// ---- Teakra's cycle accuracy tests ------------------------------------------

/// Teakra's set-up: `br` at reset to a counting loop of `inc a0` at
/// 0x1000 (with `brr -1` after sixteen), an interrupt 0 handler of `inc a1`
/// at 0x2000, and interrupt 0 seven cycles in. Teakra's test raises it with
/// a test clock; here timer 0 does, which counts and skips the same way.
fn cycle_test(target: u16) -> Dsp {
    let mut dsp = Dsp::new();
    let r = dsp.regs();
    r.ie = 1;
    r.im[0] = 1;
    dsp.program(0x0000, &[0x4180, target]);
    dsp.program(0x0006, &[0x4180, 0x2000]);
    dsp.program(0x1000, &[0x67D0; 16]);
    dsp.program(0x2000, &[0x77D0; 16]);
    dsp.program(0x1010, &[0x57F0]);
    let io = &mut dsp.teak.io;
    io.icu.set_enable(0, 1 << 0xA);
    let timer = &mut io.timers[0];
    timer.count_mode = CountMode::Single;
    timer.start_low = 7;
    timer.restart();
    dsp
}

fn counts(dsp: &mut Dsp) -> (u64, u64) {
    (dsp.regs().a[0], dsp.regs().a[1])
}

#[test]
fn simple_counting() {
    let mut dsp = cycle_test(0x1000);
    dsp.run(1 + 5);
    assert_eq!(counts(&mut dsp), (5, 0));
}

#[test]
fn counting_with_interrupt() {
    let mut dsp = cycle_test(0x1000);
    dsp.run(1 + 7 + 1 + 3);
    assert_eq!(counts(&mut dsp), (7, 3));
}

#[test]
fn counting_with_idle_and_interrupt() {
    let mut dsp = cycle_test(0x100D);
    dsp.run(1 + 6);
    assert_eq!(counts(&mut dsp), (3, 0));
    dsp.run(3);
    assert_eq!(counts(&mut dsp), (3, 1));

    let mut dsp = cycle_test(0x100D);
    dsp.run(1 + 7);
    assert_eq!(counts(&mut dsp), (3, 0));
    dsp.run(3);
    assert_eq!(counts(&mut dsp), (3, 2));

    let mut dsp = cycle_test(0x100D);
    dsp.run(1 + 7 + 1 + 2);
    assert_eq!(counts(&mut dsp), (3, 2));
}

#[test]
fn idle_with_interrupt() {
    let mut dsp = cycle_test(0x1010);
    dsp.run(1 + 7 + 1 + 2);
    assert_eq!(counts(&mut dsp), (0, 2));

    let mut dsp = cycle_test(0x1010);
    dsp.run(1 + 7);
    assert_eq!(counts(&mut dsp), (0, 0));
    dsp.run(1 + 2);
    assert_eq!(counts(&mut dsp), (0, 2));

    // Single step on the interrupt.
    let mut dsp = cycle_test(0x1010);
    dsp.run(1 + 6);
    assert_eq!(counts(&mut dsp), (0, 0));
    dsp.run(1);
    assert_eq!(counts(&mut dsp), (0, 0));
    dsp.run(1 + 2);
    assert_eq!(counts(&mut dsp), (0, 2));

    // Run over the interrupt.
    let mut dsp = cycle_test(0x1010);
    dsp.run(1 + 6);
    dsp.run(1 + 1 + 2);
    assert_eq!(counts(&mut dsp), (0, 2));
}

// ---- instructions ----------------------------------------------------------

/// Run `program` from address 0, followed by an idle loop, for `cycles`.
fn run(program: &[u16], cycles: u64) -> Dsp {
    let mut dsp = Dsp::new();
    dsp.program(0, program);
    dsp.program(program.len() as u32, &[0x57F0]);
    dsp.run(cycles);
    assert_eq!(dsp.teak.fault(), None);
    dsp
}

// Encodings, from the pattern table.
const INC_A0: u16 = 0x67D0;
const INC_A1: u16 = 0x77D0;
const BRR_SELF: u16 = 0x57F0;
/// mov #imm16, register (r0 0, y0 7, cfgi 14, a0 24, a0h 28).
const fn mov_imm(reg: u16) -> u16 {
    0x5E00 | reg
}
/// mov #imm16, mod0-mod3 (4-7).
const fn mov_imm_sttmod(reg: u16) -> u16 {
    0x0030 | reg
}
/// mov a0l, [imm16]; mov [imm16], a0.
const MOV_A0L_TO_MEM: u16 = 0xD4BC;
const MOV_MEM_TO_A0: u16 = 0xD4B8;

#[test]
fn add_sign_extends_and_sets_the_flags() {
    // add #0x8000, a0
    let mut dsp = run(&[0x86C0, 0x8000], 2);
    let r = dsp.regs();
    assert_eq!(r.a[0], 0xFFFF_FFFF_FFFF_8000);
    assert_eq!((r.fm, r.fz, r.fc0, r.fv), (1, 0, 0, 0));
    // add #0x8000 twice more, from -0x8000: carry out of bit 40.
    let mut dsp = run(&[0x86C0, 0x8000, 0x86C0, 0x8000, 0x86C0, 0x8000], 3);
    assert_eq!(dsp.regs().a[0], 0xFFFF_FFFF_FFFE_8000);
    assert_eq!(dsp.regs().fc0, 1);
}

#[test]
fn moving_to_an_accumulator_saturates_only_when_asked() {
    // mov #0x7fff, a0h; or #0xffff, a0; inc a0.
    let program = [mov_imm(28), 0x7FFF, 0x80C0, 0xFFFF, INC_A0];
    let mut dsp = run(&program, 3);
    let r = dsp.regs();
    // sata is set at reset: no saturation, the result needs 33 bits.
    assert_eq!(r.a[0], 0x8000_0000);
    assert_eq!((r.fe, r.flm, r.fv, r.fm), (1, 0, 0, 0));

    // mod0 = 0 clears sata: the same sum saturates.
    let mut program = vec![mov_imm_sttmod(4), 0x0000];
    program.extend_from_slice(&[mov_imm(28), 0x7FFF, 0x80C0, 0xFFFF, INC_A0]);
    let mut dsp = run(&program, 4);
    let r = dsp.regs();
    assert_eq!(r.a[0], 0x7FFF_FFFF);
    assert_eq!((r.fe, r.flm), (1, 1));
}

#[test]
fn moving_from_an_accumulator_saturates_unless_sat_is_set() {
    // a0 = 0x8000_0000 with sata set, then store its low half.
    let program = [
        mov_imm(28),
        0x7FFF,
        0x80C0,
        0xFFFF,
        INC_A0,
        MOV_A0L_TO_MEM,
        0x0100,
    ];
    let mut dsp = run(&program, 4);
    // sat is clear at reset: the store saturates to 0x7fffffff.
    assert_eq!(dsp.data(0x100), 0xFFFF);
    assert_eq!(dsp.regs().flm, 1);
    // With sat set (mod0 bit 0, keeping sata), it does not.
    let mut program = vec![mov_imm_sttmod(4), 0x0003];
    program.extend_from_slice(&[mov_imm(28), 0x7FFF, 0x80C0, 0xFFFF, INC_A0]);
    program.extend_from_slice(&[MOV_A0L_TO_MEM, 0x0100]);
    let dsp = run(&program, 5);
    assert_eq!(dsp.data(0x100), 0x0000);
}

#[test]
fn mpyi_multiplies_signed_by_y0() {
    // mov #5, y0; mpyi #-3.
    let mut dsp = run(&[mov_imm(7), 5, 0x08FD], 2);
    let r = dsp.regs();
    assert_eq!(r.p[0], 0xFFFF_FFF1);
    assert_eq!(r.pe[0], 1);
    // mov p, a0 (Register 11 to field 24): the product, sign-extended.
    let mut dsp = run(&[mov_imm(7), 5, 0x08FD, 0x5800 | 11 | 24 << 5], 3);
    assert_eq!(dsp.regs().a[0], 0xFFFF_FFFF_FFFF_FFF1);
}

#[test]
fn rep_repeats_the_next_instruction() {
    // rep #3; inc a0.
    let mut dsp = run(&[0x0C03, INC_A0], 10);
    assert_eq!(dsp.regs().a[0], 4);
}

#[test]
fn bkrep_runs_a_block_its_count_plus_one_times() {
    // bkrep #2, 0x0003; inc a0; inc a1.
    let mut dsp = run(&[0x5C02, 0x0003, INC_A0, INC_A1], 20);
    assert_eq!(counts(&mut dsp), (3, 3));
    assert_eq!((dsp.regs().lp, dsp.regs().bcn), (0, 0));
}

#[test]
fn call_and_ret_go_through_the_stack() {
    let mut dsp = Dsp::new();
    // call 0x0010; brr -1; ... 0x10: inc a0; ret.
    dsp.program(0, &[0x41C0, 0x0010, BRR_SELF]);
    dsp.program(0x10, &[INC_A0, 0x4580]);
    dsp.run(10);
    let r = dsp.regs();
    // pc rests on the idle loop.
    assert_eq!((r.a[0], r.pc, r.sp), (1, 2, 0));
    // The return address, high word first below it with cpc set.
    assert_eq!((dsp.data(0xFFFE), dsp.data(0xFFFF)), (2, 0));
}

#[test]
fn shifts_are_arithmetic_or_logical() {
    // mov #-0x10, a0 (via a0 = 0xfff0 sign-extended); shfi a0, a0, #-4.
    let mut dsp = run(&[mov_imm(24), 0xFFF0, 0x9B7C], 2);
    assert_eq!(dsp.regs().a[0], 0xFFFF_FFFF_FFFF_FFFF);
    // The same with s set (mod0 bit 7, keeping sata): zeros come in.
    let mut dsp = run(&[mov_imm_sttmod(4), 0x0082, mov_imm(24), 0xFFF0, 0x9B7C], 3);
    assert_eq!(dsp.regs().a[0], 0x0F_FFFF_FFFF);
}

#[test]
fn modulo_addressing_wraps_an_address_register() {
    // m0 on (mod2 bit 0), modi 3 (cfgi bits 7-15), r0 = 3; modr r0+.
    let program = [
        mov_imm_sttmod(6),
        0x0001,
        mov_imm(14),
        3 << 7,
        mov_imm(0),
        3,
        0x0088,
    ];
    let mut dsp = run(&program, 4);
    assert_eq!(dsp.regs().r[0], 0);
    let mut dsp = run(&[mov_imm(0), 3, 0x0088], 2);
    assert_eq!(dsp.regs().r[0], 4);
}

#[test]
fn teak_mode_modulo_wraps_both_ways() {
    // cmd clear (mod1 bit 13), m0 on, modi 3, r0 = 0; modr r0-.
    let setup = [
        mov_imm_sttmod(5),
        0x0000,
        mov_imm_sttmod(6),
        0x0001,
        mov_imm(14),
        3 << 7,
    ];
    let mut program = setup.to_vec();
    program.extend_from_slice(&[mov_imm(0), 0, 0x0090]);
    let mut dsp = run(&program, 5);
    assert_eq!(dsp.regs().r[0], 3);
    let mut program = setup.to_vec();
    program.extend_from_slice(&[mov_imm(0), 3, 0x0088]);
    let mut dsp = run(&program, 5);
    assert_eq!(dsp.regs().r[0], 0);
}

#[test]
fn bit_reversed_addressing_reverses_the_address() {
    // br0 on (mod2 bit 8), r0 = 2: mov [r0], a0 reads 0x4000.
    let mut dsp = Dsp::new();
    dsp.program(
        0,
        &[mov_imm_sttmod(6), 0x0100, mov_imm(0), 2, 0x1F00, BRR_SELF],
    );
    let at = 0x4_0000 + 0x4000 * 2;
    dsp.ram[at..at + 2].copy_from_slice(&0x5A5Au16.to_le_bytes());
    dsp.run(4);
    assert_eq!(dsp.regs().a[0], 0x5A5A);
    // The register itself steps normally.
    assert_eq!(dsp.regs().r[0], 2);
}

#[test]
fn mac_adds_the_last_product_and_multiplies_again() {
    // mov #3, y0; mov #4, r0; mpy y0, r0; mac y0, r0, a0 twice.
    let program = [mov_imm(7), 3, mov_imm(0), 4, 0x8040, 0x8240, 0x8240];
    let mut dsp = run(&program, 5);
    assert_eq!(dsp.regs().a[0], 24);
    assert_eq!(dsp.regs().p[0], 12);
}

#[test]
fn exp_counts_the_redundant_sign_bits() {
    // mov #0x1000, a0; exp a0: 26 copies of the sign below bit 39, less 8.
    let mut dsp = run(&[mov_imm(24), 0x1000, 0x9458], 2);
    assert_eq!(dsp.regs().sv, 18);
    // A negative value counts ones.
    let mut dsp = run(&[mov_imm(24), 0xF000, 0x9458], 2);
    assert_eq!(dsp.regs().sv, 19);
}

#[test]
fn an_undefined_opcode_faults() {
    let mut dsp = Dsp::new();
    dsp.program(0, &[0x0020]);
    dsp.run(10);
    assert!(dsp.teak.fault().unwrap().starts_with("trap"));
    assert_eq!(dsp.run(10), 0);
}

// ---- the ARM side ----------------------------------------------------------

#[test]
fn a_reply_register_write_stops_the_run_for_the_arm() {
    let mut dsp = Dsp::new();
    // mov #0x1234, a0; mov a0l, [0x80c0]; brr -1.
    dsp.program(0, &[mov_imm(24), 0x1234, MOV_A0L_TO_MEM, 0x80C0, BRR_SELF]);
    assert_eq!(dsp.run(100), 2);
    assert_eq!(dsp.teak.take_events(), [ArmEvent::Data(0)]);
    assert!(dsp.teak.recv_data_is_ready(0));
    assert_eq!(dsp.teak.recv_data(0), 0x1234);
    assert!(!dsp.teak.recv_data_is_ready(0));
    assert_eq!(dsp.run(100), 100);
}

#[test]
fn a_command_from_the_arm_interrupts_the_dsp() {
    let mut dsp = Dsp::new();
    // Past the interrupt vectors: ICU request 0xE to interrupt 0, im0 in
    // mod3, eint, idle.
    dsp.program(0, &[0x4180, 0x0010]);
    dsp.program(
        0x0010,
        &[
            mov_imm(24),
            0x4000,
            MOV_A0L_TO_MEM,
            0x8206,
            mov_imm_sttmod(7),
            0xE100,
            0x4380,
            BRR_SELF,
        ],
    );
    dsp.program(0x0006, &[0x4180, 0x0020]);
    dsp.program(
        0x0020,
        &[MOV_MEM_TO_A0, 0x80C2, MOV_A0L_TO_MEM, 0x80C0, 0x45C0],
    );
    dsp.run(1000);
    assert!(dsp.teak.take_events().is_empty());
    assert!(dsp.teak.send_data_is_empty(0));
    dsp.teak.send_data(0, 0x0055);
    assert!(!dsp.teak.send_data_is_empty(0));
    dsp.run(1000);
    assert_eq!(dsp.teak.take_events(), [ArmEvent::Data(0)]);
    assert_eq!(dsp.teak.recv_data(0), 0x0055);
    assert!(dsp.teak.send_data_is_empty(0));
    // Back in the idle loop with interrupts on.
    dsp.run(1000);
    assert_eq!(dsp.regs().pc, 0x17);
    assert_eq!(dsp.regs().ie, 1);
}

#[test]
fn the_semaphore_tells_the_arm_unless_masked() {
    let mut dsp = Dsp::new();
    // mov #0x8001, a0; mov a0l, [0x80cc].
    dsp.program(0, &[mov_imm(24), 0x8001, MOV_A0L_TO_MEM, 0x80CC, BRR_SELF]);
    dsp.teak.mask_semaphore(0x8001);
    dsp.run(10);
    assert!(dsp.teak.take_events().is_empty());
    assert_eq!(dsp.teak.semaphore(), 0x8001);
    dsp.teak.clear_semaphore(0x8001);
    dsp.teak.mask_semaphore(0);
    dsp.regs().pc = 0;
    dsp.run(10);
    assert_eq!(dsp.teak.take_events(), [ArmEvent::Semaphore]);
}

#[test]
fn the_audio_port_plays_a_sample_pair_every_period() {
    let mut dsp = Dsp::new();
    dsp.program(
        0,
        &[
            // Two sample words into port 0, then transmit on.
            mov_imm(24),
            0x1111,
            MOV_A0L_TO_MEM,
            0x82C6,
            mov_imm(24),
            0x2222,
            MOV_A0L_TO_MEM,
            0x82C6,
            mov_imm(24),
            1,
            MOV_A0L_TO_MEM,
            0x82BE,
            BRR_SELF,
        ],
    );
    dsp.run(SAMPLE_PERIOD);
    assert!(dsp.teak.take_samples().is_empty());
    dsp.run(SAMPLE_PERIOD * 3);
    assert_eq!(dsp.teak.take_samples(), [[0x1111, 0x2222], [0, 0], [0, 0]]);
}

// ---- every opcode ------------------------------------------------------------

/// Every opcode, from a few register states drawn from a fixed sequence,
/// runs without a panic: it executes or faults.
#[test]
fn every_opcode_runs_or_faults() {
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut dsp = Dsp::new();
    for opcode in 0..=u16::MAX {
        for _ in 0..3 {
            dsp.teak = Teak::new();
            let r = dsp.regs();
            for n in 0..8 {
                r.r[n] = next() as u16;
            }
            r.a = [operand::sext64(next(), 40), operand::sext64(next(), 40)];
            r.b = [operand::sext64(next(), 40), operand::sext64(next(), 40)];
            r.set_mod0(next() as u16);
            r.set_mod1(next() as u16 & 0x30FF);
            r.set_mod2(next() as u16);
            r.set_cfgi(next() as u16);
            r.set_cfgj(next() as u16);
            r.set_ar(0, next() as u16);
            r.set_ar(1, next() as u16);
            r.sv = next() as u16;
            r.sp = 0x4000;
            r.pc = 0x100;
            dsp.program(0x100, &[opcode, next() as u16]);
            dsp.run(1);
        }
    }
}
