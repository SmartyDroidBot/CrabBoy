//! The host-trap path: what a bus that says `hle()` gets instead of vectors.

use super::{Flat, BASE};
use crate::{mode, psr, Arch, Cpu, HostTrap, HostTrapKind};

fn user(entry: u32, bus: &mut Flat) -> Cpu {
    bus.hle = true;
    Cpu::new_user(Arch::V6k, entry, 0x1_0000)
}

#[test]
fn a_supervisor_call_is_parked_with_its_comment_in_both_states() {
    let mut bus = Flat::new();
    bus.set_word(BASE, 0xEF12_3456); // svc 0x123456
    bus.set_word(BASE + 4, 0xE3A0_0007); // mov r0, #7
    let mut cpu = user(BASE, &mut bus);
    cpu.step(&mut bus);
    assert_eq!(
        cpu.take_trap(),
        Some(HostTrap {
            kind: HostTrapKind::Supervisor(0x12_3456),
            pc: BASE
        })
    );
    // Still in user mode, past the call, and nothing was vectored.
    assert_eq!(cpu.cpsr() & psr::MODE, mode::USR);
    assert_eq!(cpu.reg(15), BASE + 4);
    assert_eq!(cpu.exceptions_taken(), [0; 7]);
    assert_eq!(cpu.take_trap(), None);
    cpu.step(&mut bus);
    assert_eq!(cpu.reg(0), 7);

    bus.set_half(BASE + 0x100, 0xDF32); // svc 0x32
    let mut cpu = user((BASE + 0x100) | 1, &mut bus);
    assert_ne!(cpu.cpsr() & psr::T, 0);
    cpu.step(&mut bus);
    assert_eq!(
        cpu.take_trap().map(|t| t.kind),
        Some(HostTrapKind::Supervisor(0x32))
    );
    assert_eq!(cpu.reg(15), BASE + 0x102);
}

#[test]
fn faults_are_parked_too() {
    let mut bus = Flat::new();
    bus.no_access = 0x8000..0x9000;
    bus.set_word(BASE, 0xE7F0_00F0); // udf #0
    bus.set_word(BASE + 4, 0xE590_0000); // ldr r0, [r0]
    bus.set_word(BASE + 8, 0xE120_0070); // bkpt #0
    let mut cpu = user(BASE, &mut bus);
    cpu.set_reg(0, 0x8000);
    let kinds: Vec<HostTrapKind> = (0..3)
        .map(|_| {
            cpu.step(&mut bus);
            cpu.take_trap().unwrap().kind
        })
        .collect();
    assert_eq!(
        kinds,
        [
            HostTrapKind::Undefined,
            HostTrapKind::DataAbort,
            HostTrapKind::Breakpoint
        ]
    );
    assert_eq!(cpu.cpsr() & psr::MODE, mode::USR);
}

#[test]
fn without_hle_the_guest_still_gets_its_vector() {
    let mut bus = Flat::new();
    bus.set_word(BASE, 0xEF00_0001);
    let mut cpu = Cpu::new_user(Arch::V6k, BASE, 0x1_0000);
    cpu.step(&mut bus);
    assert_eq!(cpu.take_trap(), None);
    assert_eq!(cpu.cpsr() & psr::MODE, mode::SVC);
}

#[test]
fn a_context_round_trips_through_another_thread() {
    let mut bus = Flat::new();
    let mut cpu = user(BASE, &mut bus);
    cpu.set_reg(4, 0x1111);
    cpu.vfp_mut().set_d(3, 0x4009_21FB_5444_2D18);
    let first = cpu.save_context();

    let second = Cpu::new_user(Arch::V6k, BASE + 0x201, 0x2_0000).save_context();
    cpu.load_context(&second);
    assert_eq!(
        (cpu.reg(4), cpu.reg(13), cpu.reg(15)),
        (0, 0x2_0000, BASE + 0x200)
    );
    assert_ne!(cpu.cpsr() & psr::T, 0);
    assert_eq!(cpu.vfp().d(3), 0);

    cpu.load_context(&first);
    assert_eq!((cpu.reg(4), cpu.reg(13)), (0x1111, 0x1_0000));
    assert_eq!(cpu.cpsr() & psr::T, 0);
    assert_eq!(cpu.vfp().d(3), 0x4009_21FB_5444_2D18);
}
