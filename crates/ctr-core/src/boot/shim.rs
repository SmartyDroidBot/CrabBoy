//! Launch a FIRM without the boot ROMs.
//!
//! This does what a chainloader such as boot9strap or Luma3DS does once it
//! has a FIRM in hand, and leaves the machine the way those leave it
//! (`docs/3ds/boot.md`): sections copied to their load addresses, the TCMs
//! and high vectors on, the protection unit and caches off, the screens
//! initialised with `RGB8` framebuffers, and `argc`, `argv` and a magic word
//! in `r0`-`r2`. It models no Nintendo behaviour, verifies no signature and
//! sets up no key, so it suits bare-metal homebrew only; official firmware
//! needs the real boot ROMs.

use super::romstubs;
use crate::arm11::gic::CORES;
use crate::arm11::Arm11;
use crate::arm9::Arm9;
use crate::bus::PhysMem;
use crate::io::Io;
use arm_core::{mode, psr, CpReg, Cpu};
use ctr_fs::Firm;

/// Where each processor starts executing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    pub arm9: u32,
    /// Zero when the FIRM has no ARM11 code; that processor then stays
    /// parked, as it does on a console.
    pub arm11: u32,
}

/// Framebuffers as Luma3DS sets them up: two sets, top left, top right and
/// bottom, with the right eye sharing the left one.
pub const FRAMEBUFFERS: [[u32; 3]; 2] = [
    [0x1830_0000, 0x1830_0000, 0x1834_6500],
    [0x1840_0000, 0x1840_0000, 0x1844_6500],
];

/// `r2` at entry: the low half tells a payload it was chainloaded.
pub const MAGIC: u32 = 0x0000_BEEF;

/// The argument block, in the instruction TCM as the ARM9 kernel maps it.
const ARGV: u32 = 0x01FF_F470;
const ARGV_FRAMEBUFFERS: u32 = 0x01FF_F478;
const ARGV_PATH: u32 = 0x01FF_F490;
const PATH: &[u8] = b"sdmc:/boot.firm\0";

/// The exception handlers in ARM9 work RAM that the boot ROM's vectors jump
/// to, in vector order; `None` for reset and the reserved vector.
const RAM_VECTORS: [Option<u32>; 8] = [
    None,
    Some(0x0800_0018),
    Some(0x0800_0010),
    Some(0x0800_0020),
    Some(0x0800_0028),
    None,
    Some(0x0800_0000),
    Some(0x0800_0008),
];

/// Copy the sections of `image` into `mem`.
pub fn load_firm(mem: &mut PhysMem, image: &[u8]) -> Result<Entry, String> {
    let firm = Firm::parse(image).map_err(|e| e.to_string())?;
    for (index, section) in firm.sections.iter().enumerate() {
        let target = mem
            .slice_mut(section.load_address, section.data.len())
            .ok_or_else(|| {
                format!(
                    "FIRM section {index} loads {} bytes at {:#010x}, which is not RAM",
                    section.data.len(),
                    section.load_address
                )
            })?;
        target.copy_from_slice(section.data);
    }
    Ok(Entry {
        arm9: firm.arm9_entry,
        arm11: firm.arm11_entry,
    })
}

fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// The ARM11 handlers, at the end of AXI work RAM, in the same order.
const RAM_VECTORS_11: [Option<u32>; 8] = [
    None,
    Some(0x1FFF_FFB8),
    Some(0x1FFF_FFB0),
    Some(0x1FFF_FFC0),
    Some(0x1FFF_FFC8),
    None,
    Some(0x1FFF_FFA0),
    Some(0x1FFF_FFA8),
];

/// Stand-in for the boot ROM's vector page: each vector loads the address of
/// its handler in work RAM from a table 0x20 bytes on.
fn install_vectors(rom: &mut [u8], handlers: &[Option<u32>; 8]) {
    const LDR_PC_PC_0X18: u32 = 0xE59F_F018;
    const BRANCH_TO_SELF: u32 = 0xEAFF_FFFE;
    for (n, handler) in handlers.iter().enumerate() {
        match handler {
            Some(addr) => {
                put32(rom, n * 4, LDR_PC_PC_0X18);
                put32(rom, 0x20 + n * 4, *addr);
            }
            None => put32(rom, n * 4, BRANCH_TO_SELF),
        }
    }
}

/// Bring the machine to the state a chainloader hands to a payload.
pub fn hand_off(
    arm9: &mut Arm9,
    arm11: &mut Arm11,
    io: &mut Io,
    cpu: &mut Cpu,
    cpu11: &mut [Cpu; CORES],
    entry: Entry,
) {
    install_vectors(&mut arm9.boot9[..], &RAM_VECTORS);
    install_vectors(&mut arm11.boot11[..], &RAM_VECTORS_11);
    romstubs::install(&mut arm9.boot9[..], romstubs::ARM9);
    romstubs::install(&mut arm11.boot11[..], romstubs::ARM11);

    // The interrupt controller as a boot ROM that has used it leaves it, so
    // that a sleeping core can be woken: distributor and interfaces on.
    io.mpcore.gic.write_distributor(0, 0x000, 1);
    for core in 0..CORES {
        io.mpcore.gic.write_interface(core, 0x00, 1);
        io.mpcore.gic.write_interface(core, 0x04, 0xF0);
    }
    // Core 0 starts the payload's ARM11 code, if it has any; the others wait
    // to be sent an entry point.
    for (core, cpu) in cpu11.iter_mut().enumerate() {
        cpu.set_cpsr(mode::SVC | psr::A | psr::I | psr::F);
        if core == 0 && entry.arm11 != 0 {
            cpu.jump(entry.arm11);
        } else {
            cpu.jump(romstubs::SECONDARY_WAIT_ADDR);
        }
    }

    let cp15 = |crn, crm, opc2| CpReg {
        cp: 15,
        opc1: 0,
        crn,
        crm,
        opc2,
    };
    // The boot ROM's TCM placement, then its control value with the
    // protection unit and both caches switched off again.
    arm9.cp15.write(cp15(9, 1, 1), 0x0000_0024);
    arm9.cp15.write(cp15(9, 1, 0), 0xFFF0_000A);
    arm9.cp15.write(cp15(1, 0, 0), 0x0005_6078);

    let [top, bottom] = &mut io.gpu.pdc;
    top.init_rgb8(FRAMEBUFFERS[0][0], FRAMEBUFFERS[1][0]);
    bottom.init_rgb8(FRAMEBUFFERS[0][2], FRAMEBUFFERS[1][2]);

    let itcm = |addr: u32| (addr & 0x7FFF) as usize;
    put32(&mut arm9.itcm[..], itcm(ARGV), ARGV_PATH);
    put32(&mut arm9.itcm[..], itcm(ARGV) + 4, ARGV_FRAMEBUFFERS);
    for (n, addr) in FRAMEBUFFERS.iter().flatten().enumerate() {
        put32(&mut arm9.itcm[..], itcm(ARGV_FRAMEBUFFERS) + n * 4, *addr);
    }
    let path = itcm(ARGV_PATH);
    arm9.itcm[path..path + PATH.len()].copy_from_slice(PATH);

    cpu.set_cpsr(mode::SVC | psr::I | psr::F);
    cpu.set_reg(0, 2);
    cpu.set_reg(1, ARGV);
    cpu.set_reg(2, MAGIC);
    cpu.jump(entry.arm9);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctr_fs::firm::build;

    #[test]
    fn copies_every_section_to_its_load_address() {
        let image = build(
            0x1FF8_0000,
            0x0800_6000,
            &[(0x0800_6000, &[1, 2, 3, 4]), (0x1FF8_0000, &[5, 6])],
        );
        let mut mem = PhysMem::new();
        let entry = load_firm(&mut mem, &image).unwrap();
        assert_eq!(
            entry,
            Entry {
                arm9: 0x0800_6000,
                arm11: 0x1FF8_0000
            }
        );
        assert_eq!(mem.slice(0x0800_6000, 4), Some(&[1u8, 2, 3, 4][..]));
        assert_eq!(mem.slice(0x1FF8_0000, 2), Some(&[5u8, 6][..]));
    }

    #[test]
    fn refuses_a_section_outside_ram() {
        let image = build(0, 0x1000_0000, &[(0x1000_0000, &[0; 4])]);
        let err = load_firm(&mut PhysMem::new(), &image).unwrap_err();
        assert!(err.contains("0x10000000"), "{err}");

        let straddles = build(0, 0, &[(0x080F_FFFE, &[0; 4])]);
        assert!(load_firm(&mut PhysMem::new(), &straddles).is_err());
    }
}
