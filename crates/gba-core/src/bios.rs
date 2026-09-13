//! Software-implemented BIOS SWI handlers.
//!
//! Real GBA games call BIOS routines via `SWI #n` (e.g. `CpuSet` for the boot
//! header copy, `VBlankIntrWait` for frame sync). With no BIOS dump these calls
//! would jump to the zeroed vector at `0x00000008` and hang, so we emulate the
//! common routines directly. Unrecognised SWIs fall through to a normal SVC
//! exception.

use crate::bus::Bus;
use crate::cpu::Cpu;

/// The BIOS SWI numbers we implement. Anything else takes a normal SVC
/// exception.
pub(crate) fn is_known(num: u32) -> bool {
    matches!(
        num,
        0x01 | 0x02 | 0x05 | 0x06 | 0x07 | 0x08 | 0x0A | 0x0B | 0x0C | 0x0D
    )
}

/// Run a BIOS SWI. Returns `true` if handled, `false` if the caller should
/// fall back to the SVC exception.
pub(crate) fn run(cpu: &mut Cpu, bus: &mut Bus, num: u32) -> bool {
    match num {
        0x01 => register_ram_reset(cpu, bus),
        0x02 => halt(cpu),
        0x05 => vblank_intr_wait(cpu, bus),
        0x06 => div(cpu),
        0x07 => div_arm(cpu),
        0x08 => sqrt(cpu),
        0x0A => arc_tan2(cpu),
        0x0B => cpu_set(cpu, bus),
        0x0C => cpu_fast_set(cpu, bus),
        0x0D => get_bios_checksum(cpu),
        _ => false,
    }
}

/// 0x01 RegisterRamReset: r0 bit flags select regions to zero.
fn register_ram_reset(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let flags = cpu.reg_raw(0);
    if flags & 0x01 != 0 {
        zero(bus, 0x0200_0000, crate::bus::EWRAM_SIZE);
    }
    if flags & 0x02 != 0 {
        zero(bus, 0x0300_0000, crate::bus::IWRAM_SIZE);
    }
    if flags & 0x04 != 0 {
        zero(bus, 0x0500_0000, crate::bus::PALRAM_SIZE);
    }
    if flags & 0x08 != 0 {
        zero(bus, 0x0600_0000, crate::bus::VRAM_SIZE);
    }
    if flags & 0x10 != 0 {
        zero(bus, 0x0700_0000, crate::bus::OAM_SIZE);
    }
    true
}

fn zero(bus: &mut Bus, base: u32, len: usize) {
    let mut i = 0usize;
    while i < len {
        bus.write32(base + i as u32, 0);
        i += 4;
    }
}

/// 0x02 Halt: halt until an IRQ wakes the CPU.
fn halt(cpu: &mut Cpu) -> bool {
    cpu.halted = true;
    true
}

/// BIOS internal IF mirror at the top of IWRAM. The game's IRQ handler ORs the
/// serviced flags into it and IntrWait/VBlankIntrWait poll it (GBATEK, "BIOS
/// Interrupt Functions").
pub(crate) const BIOS_IF_ADDR: u32 = 0x0300_7FF8;

/// 0x05 VBlankIntrWait: clear the VBlank flag and wait until the next one.
fn vblank_intr_wait(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    const VBLANK: u16 = 1 << 0;
    let cur = bus.read32(BIOS_IF_ADDR);
    bus.write32(BIOS_IF_ADDR, cur & !(VBLANK as u32));
    if bus.io.iflags() & VBLANK != 0 {
        bus.io.acknowledge(VBLANK);
    } else {
        cpu.begin_bios_wait(VBLANK);
    }
    true
}

/// 0x06 Div: r0/r1 -> r0 = quotient, r1 = remainder, r3 = |quotient|.
fn div(cpu: &mut Cpu) -> bool {
    let num = cpu.reg_raw(0) as i32;
    let den = cpu.reg_raw(1) as i32;
    if den == 0 {
        cpu.set_reg(0, 0);
        cpu.set_reg(1, 0);
        cpu.set_reg(3, 0);
    } else {
        let q = (num as i64 / den as i64) as i32;
        let r = (num as i64 % den as i64) as i32;
        cpu.set_reg(0, q as u32);
        cpu.set_reg(1, r as u32);
        cpu.set_reg(3, q.unsigned_abs());
    }
    true
}

/// 0x07 DivARM: r1/r2 -> r0 = quotient, r1 = remainder.
fn div_arm(cpu: &mut Cpu) -> bool {
    let num = cpu.reg_raw(1) as i32;
    let den = cpu.reg_raw(2) as i32;
    if den == 0 {
        cpu.set_reg(0, 0);
        cpu.set_reg(1, 0);
    } else {
        let q = (num as i64 / den as i64) as i32;
        let r = (num as i64 % den as i64) as i32;
        cpu.set_reg(0, q as u32);
        cpu.set_reg(1, r as u32);
    }
    true
}

/// 0x08 Sqrt: r0 = isqrt(r0).
fn sqrt(cpu: &mut Cpu) -> bool {
    let v = cpu.reg_raw(0);
    cpu.set_reg(0, (v as f64).sqrt() as u32);
    true
}

/// 0x0A ArcTan2: r0 = atan2(r1, r0) in 0..0xFFFF (full circle = 0x10000).
fn arc_tan2(cpu: &mut Cpu) -> bool {
    let x = cpu.reg_raw(0) as i32;
    let y = cpu.reg_raw(1) as i32;
    if x == 0 && y == 0 {
        cpu.set_reg(0, 0);
        return true;
    }
    let ang = (y as f64).atan2(x as f64);
    let mut norm = ((ang / (2.0 * std::f64::consts::PI)) * 65536.0).round() as i32;
    if norm < 0 {
        norm += 65536;
    }
    cpu.set_reg(0, norm as u32);
    true
}

/// 0x0B CpuSet: copy r0[src] -> r1[dst]. r2 bit 26 selects 32-bit mode.
fn cpu_set(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let mut src = cpu.reg_raw(0);
    let mut dst = cpu.reg_raw(1);
    let ctrl = cpu.reg_raw(2);
    let count = (ctrl & 0x1FFFFF) as usize;
    if ctrl & 0x0400_0000 != 0 {
        let n = count.min(0x8000);
        for _ in 0..n {
            let v = bus.read32(src);
            bus.write32(dst, v);
            src = src.wrapping_add(4);
            dst = dst.wrapping_add(4);
        }
    } else {
        let n = count.min(0x10000);
        for _ in 0..n {
            let v = bus.read16(src);
            bus.write16(dst, v);
            src = src.wrapping_add(2);
            dst = dst.wrapping_add(2);
        }
    }
    true
}

/// 0x0C CpuFastSet: 32-bit copy r0[r1]; r2 bit 24 enables 32-bit fill.
fn cpu_fast_set(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let mut src = cpu.reg_raw(0);
    let mut dst = cpu.reg_raw(1);
    let ctrl = cpu.reg_raw(2);
    let count = ((ctrl & 0x1FFFFF) as usize).min(0x8000);
    if ctrl & 0x0100_0000 != 0 {
        let v = bus.read32(src);
        for _ in 0..count {
            bus.write32(dst, v);
            dst = dst.wrapping_add(4);
        }
    } else {
        for _ in 0..count {
            let v = bus.read32(src);
            bus.write32(dst, v);
            src = src.wrapping_add(4);
            dst = dst.wrapping_add(4);
        }
    }
    true
}

/// 0x0D GetBIOSChecksum: return a checksum (games only check it is non-zero).
fn get_bios_checksum(cpu: &mut Cpu) -> bool {
    cpu.set_reg(0, 0x1234_5678);
    true
}

#[cfg(test)]
mod tests {
    use crate::cpu::Cpu;

    #[test]
    fn sqrt_handles_zero_and_squares() {
        let mut c = Cpu::new();
        c.set_reg(0, 0);
        crate::bios::run(&mut c, &mut crate::bus::Bus::new(vec![0; 0x8000]), 0x08);
        assert_eq!(c.reg_raw(0), 0);
        c.set_reg(0, 81);
        crate::bios::run(&mut c, &mut crate::bus::Bus::new(vec![0; 0x8000]), 0x08);
        assert_eq!(c.reg_raw(0), 9);
    }

    #[test]
    fn div_sets_quotient_remainder_and_abs() {
        let mut c = Cpu::new();
        c.set_reg(0, 17);
        c.set_reg(1, 5);
        crate::bios::run(&mut c, &mut crate::bus::Bus::new(vec![0; 0x8000]), 0x06);
        assert_eq!(c.reg_raw(0), 3);
        assert_eq!(c.reg_raw(1), 2);
        assert_eq!(c.reg_raw(3), 3);
        // Negative numerator -> |quotient|.
        c.set_reg(0, (-17i32) as u32);
        c.set_reg(1, 5);
        crate::bios::run(&mut c, &mut crate::bus::Bus::new(vec![0; 0x8000]), 0x06);
        assert_eq!(c.reg_raw(0), (-3i32) as u32);
        assert_eq!(c.reg_raw(3), 3);
    }

    #[test]
    fn cpu_set_copies_words() {
        let mut bus = crate::bus::Bus::new(vec![0; 0x8000]);
        let mut c = Cpu::new();
        // source in IWRAM, destination in EWRAM
        bus.write32(0x0300_0000, 0xDEADBEEF);
        bus.write32(0x0300_0004, 0x12345678);
        c.set_reg(0, 0x0300_0000);
        c.set_reg(1, 0x0200_0000);
        c.set_reg(2, 0x0400_0000 | 2); // 32-bit, 2 words
        crate::bios::run(&mut c, &mut bus, 0x0B);
        assert_eq!(bus.read32(0x0200_0000), 0xDEADBEEF);
        assert_eq!(bus.read32(0x0200_0004), 0x12345678);
    }

    #[test]
    fn unknown_swi_is_not_handled() {
        let mut bus = crate::bus::Bus::new(vec![0; 0x8000]);
        let mut c = Cpu::new();
        assert!(!crate::bios::run(&mut c, &mut bus, 0x1F));
    }
}