//! Software-implemented BIOS SWI handlers.
//!
//! Real GBA games call BIOS routines via `SWI #n` (e.g. `CpuSet` for the boot
//! header copy, `VBlankIntrWait` for frame sync). With no BIOS dump these calls
//! would jump to the zeroed vector at `0x00000008` and hang, so we emulate the
//! routines directly. Unimplemented SWIs are recorded by the system root and
//! otherwise act as a return.

use crate::bus::Bus;
use crate::cpu::Cpu;

/// Run a BIOS SWI. Returns `true` if handled, `false` if the routine is not
/// implemented; the caller records it and continues at the next instruction.
pub(crate) fn run(cpu: &mut Cpu, bus: &mut Bus, num: u32) -> bool {
    match num {
        0x00 | 0x26 => soft_reset(cpu, bus),
        0x01 => register_ram_reset(cpu, bus),
        0x02 => halt(cpu),
        0x03 => stop(cpu),
        0x04 => intr_wait(cpu, bus),
        0x05 => vblank_intr_wait(cpu, bus),
        0x06 => div(cpu),
        0x07 => div_arm(cpu),
        0x08 => sqrt(cpu),
        0x09 => arc_tan(cpu),
        0x0A => arc_tan2(cpu),
        0x0B => cpu_set(cpu, bus),
        0x0C => cpu_fast_set(cpu, bus),
        0x0D => get_bios_checksum(cpu),
        0x0E => bg_affine_set(cpu, bus),
        0x0F => obj_affine_set(cpu, bus),
        0x10 => bit_unpack(cpu, bus),
        0x11 => lz77(cpu, bus, Unit::Byte),
        0x12 => lz77(cpu, bus, Unit::Half),
        0x13 => huff(cpu, bus),
        0x14 => rl(cpu, bus, Unit::Byte),
        0x15 => rl(cpu, bus, Unit::Half),
        0x16 => diff8(cpu, bus, Unit::Byte),
        0x17 => diff8(cpu, bus, Unit::Half),
        0x18 => diff16(cpu, bus),
        0x19 => sound_bias(cpu, bus),
        // Sound driver, MultiBoot and debugging entry points that no
        // shipped game relies on: return immediately.
        0x1A..=0x1E | 0x20..=0x25 | 0x28..=0x2A => true,
        0x1F => midi_key_to_freq(cpu, bus),
        0x27 => custom_halt(cpu, bus),
        _ => false,
    }
}

/// 0x00 SoftReset / 0x26 HardReset: clear the BIOS area at the top of IWRAM,
/// zero the registers, set up the three stacks and restart the cartridge in
/// SYS mode (or EWRAM when the return-address flag at 0x03007FFA is set).
fn soft_reset(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let to_ewram = bus.read8(0x0300_7FFA) != 0;
    zero(bus, 0x0300_7E00, 0x200);
    for r in 0..13 {
        cpu.set_reg(r, 0);
    }
    cpu.set_mode_sp(crate::cpu::mode::SVC, 0x0300_7FE0);
    cpu.set_mode_sp(crate::cpu::mode::IRQ, 0x0300_7FA0);
    cpu.set_mode_sp(crate::cpu::mode::USR, 0x0300_7F00);
    cpu.set_cpsr(0x1F);
    cpu.set_reg(14, 0);
    cpu.halted = false;
    cpu.complete_bios_wait();
    bus.io.write16(0x208, 0); // IME off, as after the BIOS boot
    cpu.set_pc(if to_ewram { 0x0200_0000 } else { 0x0800_0000 });
    true
}

/// 0x01 RegisterRamReset: r0 bit flags select what to clear. DISPCNT is
/// forced blank unconditionally, and bit 1 spares the top 0x200 bytes of
/// IWRAM that hold the BIOS state (stacks, IF mirror, IRQ vector).
fn register_ram_reset(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let flags = cpu.reg_raw(0);
    bus.io.write16(0x00, 0x0080);
    if flags & 0x01 != 0 {
        zero(bus, 0x0200_0000, crate::bus::EWRAM_SIZE);
    }
    if flags & 0x02 != 0 {
        zero(bus, 0x0300_0000, crate::bus::IWRAM_SIZE - 0x200);
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
    if flags & 0x20 != 0 {
        // Serial: SIOCNT, SIOMLT_SEND, RCNT (start value), JOYCNT, JOY_*.
        for (off, v) in [
            (0x128u32, 0u16),
            (0x12A, 0),
            (0x134, 0x8000),
            (0x140, 0),
            (0x150, 0),
            (0x152, 0),
            (0x154, 0),
            (0x156, 0),
        ] {
            bus.write16(0x0400_0000 + off, v as u32);
        }
    }
    if flags & 0x40 != 0 {
        for off in (0x60..=0x84u32).step_by(2) {
            bus.write16(0x0400_0000 + off, 0);
        }
        bus.write16(0x0400_0088, 0x200); // SOUNDBIAS
        for off in (0x90..=0x9Eu32).step_by(2) {
            bus.write16(0x0400_0000 + off, 0);
        }
    }
    if flags & 0x80 != 0 {
        // Video (except DISPCNT), DMA, timers, interrupt and wait-state
        // registers; the affine matrices return to identity.
        for off in (0x04..=0x56u32).step_by(2) {
            bus.write16(0x0400_0000 + off, 0);
        }
        for off in [0x20u32, 0x26, 0x30, 0x36] {
            bus.write16(0x0400_0000 + off, 0x100);
        }
        for off in (0xB0..=0xDEu32).step_by(2) {
            bus.write16(0x0400_0000 + off, 0);
        }
        for off in (0x100..=0x10Eu32).step_by(2) {
            bus.write16(0x0400_0000 + off, 0);
        }
        bus.write16(0x0400_0200, 0); // IE
        bus.write16(0x0400_0202, 0xFFFF); // IF: acknowledge everything
        bus.write16(0x0400_0204, 0); // WAITCNT
        bus.write16(0x0400_0208, 0); // IME
    }
    true
}

/// 0x03 Stop: enter low-power mode until a keypad, cartridge or serial
/// interrupt. Approximated as a halt.
fn stop(cpu: &mut Cpu) -> bool {
    cpu.halted = true;
    true
}

/// 0x27 CustomHalt: write r2 to HALTCNT (0 = halt, 0x80 = stop).
fn custom_halt(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    bus.write8(0x0400_0301, cpu.reg_raw(2) & 0xFF);
    true
}

/// 0x19 SoundBias: r0 = 0 sets the bias level to 0, otherwise to 0x200.
fn sound_bias(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let bias = if cpu.reg_raw(0) == 0 { 0 } else { 0x200 };
    bus.write16(0x0400_0088, bias);
    true
}

/// 2^31 * 2^(n/12) for the twelve semitones.
const SEMITONE_TABLE: [u32; 12] = [
    2147483648, 2275179671, 2410468894, 2553802834, 2705659852, 2866546760, 3037000500, 3217589947,
    3408917802, 3611622603, 3826380858, 4053909305,
];

/// Frequency multiplier for a MIDI key as a 32-bit fraction of 2^(16+key/12):
/// the semitone table shifted down by the octaves below the 15th.
fn key_scale(key: u32) -> u32 {
    let key = key.min(179);
    SEMITONE_TABLE[(key % 12) as usize] >> (15 - key / 12)
}

/// 0x1F MidiKey2Freq: r0 = WaveData (sample rate at r0+4), r1 = MIDI key,
/// r2 = fine adjust (1/256 semitone) -> r0 = playback rate. Keys above 178
/// clamp to 178 with maximum fine adjust, as in the sound driver source.
fn midi_key_to_freq(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let freq = bus.read32(cpu.reg_raw(0).wrapping_add(4)) as u64;
    let mut key = cpu.reg_raw(1) & 0xFF;
    let mut fine = cpu.reg_raw(2) & 0xFF;
    if key > 178 {
        key = 178;
        fine = 255;
    }
    let lo = key_scale(key) as u64;
    let hi = key_scale(key + 1) as u64;
    let scale = lo + (((hi - lo) * fine as u64) >> 8);
    cpu.set_reg(0, ((freq * scale) >> 32) as u32);
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

/// 0x04 IntrWait: r0 = 1 discards flags already set, r1 = mask of IRQ flags
/// to wait for.
fn intr_wait(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let discard = cpu.reg_raw(0) & 1 != 0;
    let mask = cpu.reg_raw(1) as u16;
    wait_for_irq(cpu, bus, mask, discard)
}

/// 0x05 VBlankIntrWait: `IntrWait(1, 1)`.
fn vblank_intr_wait(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    wait_for_irq(cpu, bus, 1 << 0, true)
}

fn wait_for_irq(cpu: &mut Cpu, bus: &mut Bus, mask: u16, discard: bool) -> bool {
    let cur = bus.read32(BIOS_IF_ADDR);
    if discard {
        bus.write32(BIOS_IF_ADDR, cur & !(mask as u32));
    } else if cur & mask as u32 != 0 {
        bus.write32(BIOS_IF_ADDR, cur & !(mask as u32));
        return true;
    }
    // The BIOS enables IME so the game's handler can run and flag the
    // mirror, then halts until it does.
    bus.io.write16(0x208, 1);
    cpu.begin_bios_wait(mask);
    true
}

/// Signed division as the BIOS performs it. Division by zero cannot be
/// reproduced exactly (the BIOS loops for |num| > 1); follow mGBA's HLE and
/// return the sign of the numerator with the numerator as remainder.
fn divide(num: i32, den: i32) -> (i32, i32) {
    if den == 0 {
        (if num < 0 { -1 } else { 1 }, num)
    } else {
        (num.wrapping_div(den), num.wrapping_rem(den))
    }
}

/// 0x06 Div: r0/r1 -> r0 = quotient, r1 = remainder, r3 = |quotient|.
fn div(cpu: &mut Cpu) -> bool {
    let (q, r) = divide(cpu.reg_raw(0) as i32, cpu.reg_raw(1) as i32);
    cpu.set_reg(0, q as u32);
    cpu.set_reg(1, r as u32);
    cpu.set_reg(3, q.unsigned_abs());
    true
}

/// 0x07 DivARM: r1/r2 -> r0 = quotient, r1 = remainder, r3 = |quotient|.
fn div_arm(cpu: &mut Cpu) -> bool {
    let (q, r) = divide(cpu.reg_raw(1) as i32, cpu.reg_raw(2) as i32);
    cpu.set_reg(0, q as u32);
    cpu.set_reg(1, r as u32);
    cpu.set_reg(3, q.unsigned_abs());
    true
}

/// 0x08 Sqrt: r0 = floor(sqrt(r0)), computed bit by bit.
fn sqrt(cpu: &mut Cpu) -> bool {
    let v = cpu.reg_raw(0);
    cpu.set_reg(0, isqrt(v));
    true
}

fn isqrt(v: u32) -> u32 {
    let mut rem = v;
    let mut root = 0u32;
    let mut bit = 1u32 << 30;
    while bit > rem {
        bit >>= 2;
    }
    while bit != 0 {
        if rem >= root + bit {
            rem -= root + bit;
            root = (root >> 1) + bit;
        } else {
            root >>= 1;
        }
        bit >>= 2;
    }
    root
}

/// The BIOS ArcTan polynomial: `i` is a 1.14 fixed-point tangent, the result
/// an angle with 0x4000 = 90 degrees (only valid for |i| <= 1).
fn bios_arctan(i: i32) -> i32 {
    let a = -((i * i) >> 14);
    let mut b = ((0xA9 * a) >> 14) + 0x390;
    b = ((b * a) >> 14) + 0x91C;
    b = ((b * a) >> 14) + 0xFB6;
    b = ((b * a) >> 14) + 0x16AA;
    b = ((b * a) >> 14) + 0x2081;
    b = ((b * a) >> 14) + 0x3651;
    b = ((b * a) >> 14) + 0xA2F9;
    (i * b) >> 16
}

/// 0x09 ArcTan: r0 = tan (1.14 fixed point) -> r0 = angle (0x4000 = 90 deg).
fn arc_tan(cpu: &mut Cpu) -> bool {
    let i = cpu.reg_raw(0) as i16 as i32;
    cpu.set_reg(0, bios_arctan(i) as u32 & 0xFFFF);
    true
}

/// 0x0A ArcTan2: r0 = x, r1 = y (signed 16-bit) -> r0 = angle 0..0xFFFF
/// (full circle = 0x10000), reduced to the ArcTan polynomial per octant.
fn arc_tan2(cpu: &mut Cpu) -> bool {
    let x = cpu.reg_raw(0) as i16 as i32;
    let y = cpu.reg_raw(1) as i16 as i32;
    let angle = if y == 0 {
        if x >= 0 {
            0
        } else {
            0x8000
        }
    } else if x == 0 {
        if y >= 0 {
            0x4000
        } else {
            0xC000
        }
    } else if y >= 0 {
        if x >= 0 {
            if x >= y {
                bios_arctan((y << 14) / x)
            } else {
                0x4000 - bios_arctan((x << 14) / y)
            }
        } else if -x >= y {
            bios_arctan((y << 14) / x) + 0x8000
        } else {
            0x4000 - bios_arctan((x << 14) / y)
        }
    } else if x <= 0 {
        if -x > -y {
            bios_arctan((y << 14) / x) + 0x8000
        } else {
            0xC000 - bios_arctan((x << 14) / y)
        }
    } else if x >= -y {
        bios_arctan((y << 14) / x) + 0x10000
    } else {
        0xC000 - bios_arctan((x << 14) / y)
    };
    cpu.set_reg(0, angle as u32 & 0xFFFF);
    true
}

/// 0x0B CpuSet: copy or fill r0 -> r1. r2 bits 0-20 = unit count, bit 24 =
/// fill mode (repeat the first source unit), bit 26 = 32-bit units.
fn cpu_set(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let mut src = cpu.reg_raw(0);
    let mut dst = cpu.reg_raw(1);
    let ctrl = cpu.reg_raw(2);
    let count = (ctrl & 0x1F_FFFF) as usize;
    let fill = ctrl & 0x0100_0000 != 0;
    if ctrl & 0x0400_0000 != 0 {
        let fill_value = bus.read32(src);
        for _ in 0..count {
            let v = if fill {
                fill_value
            } else {
                let v = bus.read32(src);
                src = src.wrapping_add(4);
                v
            };
            bus.write32(dst, v);
            dst = dst.wrapping_add(4);
        }
    } else {
        let fill_value = bus.read16(src);
        for _ in 0..count {
            let v = if fill {
                fill_value
            } else {
                let v = bus.read16(src);
                src = src.wrapping_add(2);
                v
            };
            bus.write16(dst, v);
            dst = dst.wrapping_add(2);
        }
    }
    true
}

/// 0x0C CpuFastSet: 32-bit copy or fill in blocks of eight words; the count
/// is rounded up to a multiple of eight as the BIOS's unrolled loop does.
fn cpu_fast_set(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let mut src = cpu.reg_raw(0);
    let mut dst = cpu.reg_raw(1);
    let ctrl = cpu.reg_raw(2);
    let count = ((ctrl & 0x1F_FFFF) as usize).div_ceil(8) * 8;
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

/// 0x0D GetBIOSChecksum: the checksum of the retail GBA BIOS.
fn get_bios_checksum(cpu: &mut Cpu) -> bool {
    cpu.set_reg(0, 0xBAAE_187F);
    true
}

/// sin(i * 2pi / 256) in 2.14 fixed point; cos is the entry 64 further on.
const SIN_LUT: [i16; 256] = [
    0, 402, 804, 1205, 1606, 2006, 2404, 2801, 3196, 3590, 3981, 4370, 4756, 5139, 5520, 5897,
    6270, 6639, 7005, 7366, 7723, 8076, 8423, 8765, 9102, 9434, 9760, 10080, 10394, 10702, 11003,
    11297, 11585, 11866, 12140, 12406, 12665, 12916, 13160, 13395, 13623, 13842, 14053, 14256,
    14449, 14635, 14811, 14978, 15137, 15286, 15426, 15557, 15679, 15791, 15893, 15986, 16069,
    16143, 16207, 16261, 16305, 16340, 16364, 16379, 16384, 16379, 16364, 16340, 16305, 16261,
    16207, 16143, 16069, 15986, 15893, 15791, 15679, 15557, 15426, 15286, 15137, 14978, 14811,
    14635, 14449, 14256, 14053, 13842, 13623, 13395, 13160, 12916, 12665, 12406, 12140, 11866,
    11585, 11297, 11003, 10702, 10394, 10080, 9760, 9434, 9102, 8765, 8423, 8076, 7723, 7366, 7005,
    6639, 6270, 5897, 5520, 5139, 4756, 4370, 3981, 3590, 3196, 2801, 2404, 2006, 1606, 1205, 804,
    402, 0, -402, -804, -1205, -1606, -2006, -2404, -2801, -3196, -3590, -3981, -4370, -4756,
    -5139, -5520, -5897, -6270, -6639, -7005, -7366, -7723, -8076, -8423, -8765, -9102, -9434,
    -9760, -10080, -10394, -10702, -11003, -11297, -11585, -11866, -12140, -12406, -12665, -12916,
    -13160, -13395, -13623, -13842, -14053, -14256, -14449, -14635, -14811, -14978, -15137, -15286,
    -15426, -15557, -15679, -15791, -15893, -15986, -16069, -16143, -16207, -16261, -16305, -16340,
    -16364, -16379, -16384, -16379, -16364, -16340, -16305, -16261, -16207, -16143, -16069, -15986,
    -15893, -15791, -15679, -15557, -15426, -15286, -15137, -14978, -14811, -14635, -14449, -14256,
    -14053, -13842, -13623, -13395, -13160, -12916, -12665, -12406, -12140, -11866, -11585, -11297,
    -11003, -10702, -10394, -10080, -9760, -9434, -9102, -8765, -8423, -8076, -7723, -7366, -7005,
    -6639, -6270, -5897, -5520, -5139, -4756, -4370, -3981, -3590, -3196, -2801, -2404, -2006,
    -1606, -1205, -804, -402,
];

/// Rotation matrix entries (8.8 fixed point) for scales `sx`/`sy` (8.8) and
/// an angle whose top byte indexes the sine table, as the BIOS does.
fn affine_matrix(sx: i32, sy: i32, alpha: u32) -> (i32, i32, i32, i32) {
    let theta = ((alpha >> 8) & 0xFF) as usize;
    let sin = SIN_LUT[theta] as i32;
    let cos = SIN_LUT[(theta + 64) & 0xFF] as i32;
    let pa = (sx * cos) >> 14;
    let pb = -((sx * sin) >> 14);
    let pc = (sy * sin) >> 14;
    let pd = (sy * cos) >> 14;
    (pa, pb, pc, pd)
}

/// 0x0E BgAffineSet: r0 = source (s32 cx, s32 cy, s16 px, s16 py, s16 sx,
/// s16 sy, u16 alpha; 20 bytes), r1 = destination (s16 pa, pb, pc, pd; s32
/// dx, dy; 16 bytes), r2 = count.
fn bg_affine_set(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let mut src = cpu.reg_raw(0);
    let mut dst = cpu.reg_raw(1);
    for _ in 0..cpu.reg_raw(2) {
        let cx = bus.read32(src) as i32;
        let cy = bus.read32(src.wrapping_add(4)) as i32;
        let px = bus.read16(src.wrapping_add(8)) as i16 as i32;
        let py = bus.read16(src.wrapping_add(10)) as i16 as i32;
        let sx = bus.read16(src.wrapping_add(12)) as i16 as i32;
        let sy = bus.read16(src.wrapping_add(14)) as i16 as i32;
        let alpha = bus.read16(src.wrapping_add(16));
        src = src.wrapping_add(20);
        let (pa, pb, pc, pd) = affine_matrix(sx, sy, alpha);
        let dx = cx.wrapping_sub(pa.wrapping_mul(px).wrapping_add(pb.wrapping_mul(py)));
        let dy = cy.wrapping_sub(pc.wrapping_mul(px).wrapping_add(pd.wrapping_mul(py)));
        bus.write16(dst, pa as u32 & 0xFFFF);
        bus.write16(dst.wrapping_add(2), pb as u32 & 0xFFFF);
        bus.write16(dst.wrapping_add(4), pc as u32 & 0xFFFF);
        bus.write16(dst.wrapping_add(6), pd as u32 & 0xFFFF);
        bus.write32(dst.wrapping_add(8), dx as u32);
        bus.write32(dst.wrapping_add(12), dy as u32);
        dst = dst.wrapping_add(16);
    }
    true
}

/// 0x0F ObjAffineSet: r0 = source (s16 sx, s16 sy, u16 alpha; 8 bytes with
/// padding), r1 = destination, r2 = count, r3 = byte offset between the
/// four matrix entries (8 when writing straight into OAM).
fn obj_affine_set(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let mut src = cpu.reg_raw(0);
    let mut dst = cpu.reg_raw(1);
    let stride = cpu.reg_raw(3);
    for _ in 0..cpu.reg_raw(2) {
        let sx = bus.read16(src) as i16 as i32;
        let sy = bus.read16(src.wrapping_add(2)) as i16 as i32;
        let alpha = bus.read16(src.wrapping_add(4));
        src = src.wrapping_add(8);
        let (pa, pb, pc, pd) = affine_matrix(sx, sy, alpha);
        for v in [pa, pb, pc, pd] {
            bus.write16(dst, v as u32 & 0xFFFF);
            dst = dst.wrapping_add(stride);
        }
    }
    true
}

/// 0x10 BitUnPack: r0 = source, r1 = destination (word aligned), r2 = info
/// block (u16 source length in bytes, u8 source unit width, u8 destination
/// unit width, u32 data offset with bit 31 = also add it to zero units).
fn bit_unpack(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let src = cpu.reg_raw(0);
    let mut dst = cpu.reg_raw(1);
    let info = cpu.reg_raw(2);
    let src_len = bus.read16(info) as usize;
    let src_width = bus.read8(info.wrapping_add(2));
    let dst_width = bus.read8(info.wrapping_add(3));
    let bias = bus.read32(info.wrapping_add(4));
    let offset = bias & 0x7FFF_FFFF;
    let zero_too = bias & 0x8000_0000 != 0;
    if !matches!(src_width, 1 | 2 | 4 | 8) || !matches!(dst_width, 1 | 2 | 4 | 8 | 16 | 32) {
        return false;
    }
    let mut out = 0u32;
    let mut bits = 0u32;
    for i in 0..src_len as u32 {
        let byte = bus.read8(src.wrapping_add(i));
        let mut b = 0;
        while b < 8 {
            let mut unit = (byte >> b) & ((1 << src_width) - 1);
            if unit != 0 || zero_too {
                unit = unit.wrapping_add(offset);
            }
            out |= unit.wrapping_shl(bits);
            bits += dst_width;
            if bits >= 32 {
                bus.write32(dst, out);
                dst = dst.wrapping_add(4);
                out = 0;
                bits = 0;
            }
            b += src_width;
        }
    }
    true
}

/// Access width the decompressors use to store their output. The `Wram`
/// variants write bytes; the `Vram` variants write halfwords because VRAM
/// ignores byte writes; Huffman always writes words.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Byte,
    Half,
    Word,
}

/// Decode the 32-bit header shared by the decompression SWIs: bits 4-7 hold
/// the compression type, bits 8-31 the decompressed size. Returns the low
/// nibble (type-specific) and the size.
fn unpack_header(bus: &mut Bus, src: u32, kind: u32) -> Option<(u32, usize)> {
    let header = bus.read32(src);
    if (header >> 4) & 0xF != kind {
        return None;
    }
    Some((header & 0xF, (header >> 8) as usize))
}

/// Store decompressed bytes with the given access width, zero-padding the
/// final unit.
fn write_out(bus: &mut Bus, dst: u32, data: &[u8], unit: Unit) {
    let mut d = dst;
    match unit {
        Unit::Byte => {
            for &b in data {
                bus.write8(d, b as u32);
                d = d.wrapping_add(1);
            }
        }
        Unit::Half => {
            for chunk in data.chunks(2) {
                let lo = chunk[0] as u32;
                let hi = chunk.get(1).copied().unwrap_or(0) as u32;
                bus.write16(d, lo | hi << 8);
                d = d.wrapping_add(2);
            }
        }
        Unit::Word => {
            for chunk in data.chunks(4) {
                let mut w = 0u32;
                for (i, &b) in chunk.iter().enumerate() {
                    w |= (b as u32) << (i * 8);
                }
                bus.write32(d, w);
                d = d.wrapping_add(4);
            }
        }
    }
}

/// 0x11/0x12 LZ77UnComp: flag byte (MSB first) selects literal bytes or
/// (disp+1, len+3) back-references.
fn lz77(cpu: &mut Cpu, bus: &mut Bus, unit: Unit) -> bool {
    let mut src = cpu.reg_raw(0);
    let dst = cpu.reg_raw(1);
    let Some((_, size)) = unpack_header(bus, src, 1) else {
        return false;
    };
    src = src.wrapping_add(4);
    let mut out = Vec::with_capacity(size);
    while out.len() < size {
        let flags = bus.read8(src);
        src = src.wrapping_add(1);
        for i in 0..8 {
            if out.len() >= size {
                break;
            }
            if flags & (0x80 >> i) == 0 {
                out.push(bus.read8(src) as u8);
                src = src.wrapping_add(1);
            } else {
                let first = bus.read8(src) as usize;
                let second = bus.read8(src.wrapping_add(1)) as usize;
                src = src.wrapping_add(2);
                let disp = (((first & 0x0F) << 8) | second) + 1;
                let len = (first >> 4) + 3;
                if disp > out.len() {
                    return false;
                }
                let from = out.len() - disp;
                for k in 0..len {
                    if out.len() >= size {
                        break;
                    }
                    out.push(out[from + k]);
                }
            }
        }
    }
    write_out(bus, dst, &out, unit);
    true
}

/// 0x13 HuffUnComp: 4- or 8-bit symbols encoded with the BIOS Huffman tree
/// (node = offset to the child pair, bits 7/6 flag data children).
fn huff(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let src = cpu.reg_raw(0);
    let dst = cpu.reg_raw(1);
    let Some((bits, size)) = unpack_header(bus, src, 2) else {
        return false;
    };
    if bits != 4 && bits != 8 {
        return false;
    }
    let tree_size = bus.read8(src.wrapping_add(4));
    let root = src.wrapping_add(5);
    let mut data = src.wrapping_add(4 + (tree_size + 1) * 2);
    let mut out = Vec::with_capacity(size);
    let mut word = 0u32;
    let mut remaining = 0u32;
    let mut low_nibble: Option<u8> = None;
    while out.len() < size {
        let mut node = root;
        loop {
            if remaining == 0 {
                word = bus.read32(data);
                data = data.wrapping_add(4);
                remaining = 32;
            }
            let bit = word >> 31;
            word <<= 1;
            remaining -= 1;
            let desc = bus.read8(node);
            // Children live at the next even address plus the offset; the
            // right child (bit 1) follows the left.
            let child = (node & !1).wrapping_add(((desc & 0x3F) + 1) * 2 + bit);
            let leaf = desc & (if bit == 1 { 0x40 } else { 0x80 }) != 0;
            if !leaf {
                node = child;
                continue;
            }
            let value = bus.read8(child) as u8;
            if bits == 8 {
                out.push(value);
            } else if let Some(lo) = low_nibble.take() {
                out.push(lo | (value & 0x0F) << 4);
            } else {
                low_nibble = Some(value & 0x0F);
            }
            break;
        }
    }
    if let Some(lo) = low_nibble {
        out.push(lo);
    }
    write_out(bus, dst, &out, Unit::Word);
    true
}

/// 0x14/0x15 RLUnComp: flag byte bit 7 = run of (n+3) copies, else (n+1)
/// literal bytes.
fn rl(cpu: &mut Cpu, bus: &mut Bus, unit: Unit) -> bool {
    let mut src = cpu.reg_raw(0);
    let dst = cpu.reg_raw(1);
    let Some((_, size)) = unpack_header(bus, src, 3) else {
        return false;
    };
    src = src.wrapping_add(4);
    let mut out = Vec::with_capacity(size);
    while out.len() < size {
        let flag = bus.read8(src) as usize;
        src = src.wrapping_add(1);
        if flag & 0x80 != 0 {
            let n = ((flag & 0x7F) + 3).min(size - out.len());
            let byte = bus.read8(src) as u8;
            src = src.wrapping_add(1);
            out.extend(std::iter::repeat_n(byte, n));
        } else {
            let n = ((flag & 0x7F) + 1).min(size - out.len());
            for _ in 0..n {
                out.push(bus.read8(src) as u8);
                src = src.wrapping_add(1);
            }
        }
    }
    write_out(bus, dst, &out, unit);
    true
}

/// 0x16/0x17 Diff8bitUnFilter: running sum of byte deltas.
fn diff8(cpu: &mut Cpu, bus: &mut Bus, unit: Unit) -> bool {
    let mut src = cpu.reg_raw(0);
    let dst = cpu.reg_raw(1);
    let Some((width, size)) = unpack_header(bus, src, 8) else {
        return false;
    };
    if width != 1 {
        return false;
    }
    src = src.wrapping_add(4);
    let mut out = Vec::with_capacity(size);
    let mut acc = 0u8;
    for _ in 0..size {
        acc = acc.wrapping_add(bus.read8(src) as u8);
        src = src.wrapping_add(1);
        out.push(acc);
    }
    write_out(bus, dst, &out, unit);
    true
}

/// 0x18 Diff16bitUnFilter: running sum of halfword deltas.
fn diff16(cpu: &mut Cpu, bus: &mut Bus) -> bool {
    let mut src = cpu.reg_raw(0);
    let mut dst = cpu.reg_raw(1);
    let Some((width, size)) = unpack_header(bus, src, 8) else {
        return false;
    };
    if width != 2 {
        return false;
    }
    src = src.wrapping_add(4);
    let mut acc = 0u16;
    for _ in 0..size / 2 {
        acc = acc.wrapping_add(bus.read16(src) as u16);
        src = src.wrapping_add(2);
        bus.write16(dst, acc as u32);
        dst = dst.wrapping_add(2);
    }
    true
}

#[cfg(test)]
mod tests {
    use crate::cpu::Cpu;

    /// A bus whose ROM holds `stream` at 0x08000000.
    fn rom_bus(stream: &[u8]) -> crate::bus::Bus {
        let mut rom = vec![0u8; 0x8000];
        rom[..stream.len()].copy_from_slice(stream);
        crate::bus::Bus::new(rom)
    }

    fn run_unpack(bus: &mut crate::bus::Bus, swi: u32) -> bool {
        let mut c = Cpu::new();
        c.set_reg(0, 0x0800_0000);
        c.set_reg(1, 0x0200_0000);
        crate::bios::run(&mut c, bus, swi)
    }

    fn header(kind: u32, low: u32, size: u32) -> [u8; 4] {
        ((kind << 4) | low | (size << 8)).to_le_bytes()
    }

    #[test]
    fn lz77_uncomp_expands_literals_and_back_references() {
        // "AB" literal, then a 3-byte back-reference with displacement 2
        // (repeats "AB" -> "ABA"), then literal "C": ABABAC.
        let mut s = header(1, 0, 6).to_vec();
        s.extend_from_slice(&[0b0010_0000, b'A', b'B', 0x00, 0x01, b'C']);
        let mut bus = rom_bus(&s);
        assert!(run_unpack(&mut bus, 0x11));
        let got: Vec<u8> = (0..6).map(|i| bus.read8(0x0200_0000 + i) as u8).collect();
        assert_eq!(got, b"ABABAC");
        // The VRAM variant produces the same bytes through halfword writes.
        let mut bus = rom_bus(&s);
        let mut c = Cpu::new();
        c.set_reg(0, 0x0800_0000);
        c.set_reg(1, 0x0600_0000);
        assert!(crate::bios::run(&mut c, &mut bus, 0x12));
        assert_eq!(bus.read16(0x0600_0000), u16::from_le_bytes(*b"AB") as u32);
        assert_eq!(bus.read16(0x0600_0004), u16::from_le_bytes(*b"AC") as u32);
    }

    #[test]
    fn rl_uncomp_expands_runs() {
        // Run of 4 x 0x12, then 2 literals.
        let mut s = header(3, 0, 6).to_vec();
        s.extend_from_slice(&[0x81, 0x12, 0x01, 0xAA, 0xBB]);
        let mut bus = rom_bus(&s);
        assert!(run_unpack(&mut bus, 0x14));
        assert_eq!(bus.read32(0x0200_0000), 0x1212_1212);
        assert_eq!(bus.read16(0x0200_0004), 0xBBAA);
    }

    #[test]
    fn huff_uncomp_reads_eight_bit_symbols() {
        // Single node: both children are leaves; 0 -> 'A', 1 -> 'B'.
        let mut s = header(2, 8, 4).to_vec();
        s.push(1); // tree size byte: bitstream at +4+4
        s.push(0xC0); // root: left and right are data
        s.push(b'A');
        s.push(b'B');
        s.extend_from_slice(&0b0101_0000_0000_0000_0000_0000_0000_0000u32.to_le_bytes());
        let mut bus = rom_bus(&s);
        assert!(run_unpack(&mut bus, 0x13));
        assert_eq!(bus.read32(0x0200_0000), u32::from_le_bytes(*b"ABAB"));
    }

    #[test]
    fn huff_uncomp_packs_four_bit_symbols_low_nibble_first() {
        let mut s = header(2, 4, 2).to_vec();
        s.push(1);
        s.push(0xC0);
        s.push(0x1);
        s.push(0x2);
        // Bits 0,1,1,0 -> symbols 1,2,2,1 -> bytes 0x21, 0x12.
        s.extend_from_slice(&0b0110_0000_0000_0000_0000_0000_0000_0000u32.to_le_bytes());
        let mut bus = rom_bus(&s);
        assert!(run_unpack(&mut bus, 0x13));
        assert_eq!(bus.read16(0x0200_0000), 0x1221);
    }

    #[test]
    fn huff_uncomp_walks_a_two_level_tree() {
        // root(+5, offset 0) -> pair at +6/+7: left = node (offset 0), right
        // = leaf 'C'. Left node at +6 -> pair at +8/+9: leaves 'A', 'B'.
        // Codes: A = 00, B = 01, C = 1.
        let mut s = header(2, 8, 4).to_vec();
        s.push(3); // tree table padded to 8 bytes so the bitstream is aligned
        s.push(0x40); // root: right child is data
        s.push(0xC0); // left node: both children data
        s.push(b'C');
        s.push(b'A');
        s.push(b'B');
        s.push(0);
        s.push(0);
        // C A B C -> 1 00 01 1
        s.extend_from_slice(&0b1000_1100_0000_0000_0000_0000_0000_0000u32.to_le_bytes());
        let mut bus = rom_bus(&s);
        assert!(run_unpack(&mut bus, 0x13));
        assert_eq!(bus.read32(0x0200_0000), u32::from_le_bytes(*b"CABC"));
    }

    #[test]
    fn diff8_and_diff16_unfilter() {
        let mut s = header(8, 1, 3).to_vec();
        s.extend_from_slice(&[0x10, 0x02, 0xFF]);
        let mut bus = rom_bus(&s);
        assert!(run_unpack(&mut bus, 0x16));
        assert_eq!(bus.read8(0x0200_0000), 0x10);
        assert_eq!(bus.read8(0x0200_0001), 0x12);
        assert_eq!(bus.read8(0x0200_0002), 0x11);

        let mut s = header(8, 2, 4).to_vec();
        s.extend_from_slice(&0x1234u16.to_le_bytes());
        s.extend_from_slice(&0xFFFEu16.to_le_bytes());
        let mut bus = rom_bus(&s);
        assert!(run_unpack(&mut bus, 0x18));
        assert_eq!(bus.read16(0x0200_0000), 0x1234);
        assert_eq!(bus.read16(0x0200_0002), 0x1232);
        // A byte-width stream is rejected by the 16-bit filter.
        let mut bus = rom_bus(&header(8, 1, 4));
        assert!(!run_unpack(&mut bus, 0x18));
    }

    #[test]
    fn arc_tan2_covers_the_axes_and_diagonals() {
        let mut c = Cpu::new();
        let mut bus = crate::bus::Bus::new(vec![0; 0x8000]);
        for (x, y, expect) in [
            (1i32, 0i32, 0u32),
            (0, 1, 0x4000),
            (-1, 0, 0x8000),
            (0, -1, 0xC000),
            (0, 0, 0),
        ] {
            c.set_reg(0, x as u32);
            c.set_reg(1, y as u32);
            crate::bios::run(&mut c, &mut bus, 0x0A);
            assert_eq!(c.reg_raw(0), expect, "atan2({y}, {x})");
        }
        c.set_reg(0, 100);
        c.set_reg(1, 100);
        crate::bios::run(&mut c, &mut bus, 0x0A);
        assert!((c.reg_raw(0) as i32 - 0x2000).abs() <= 4);
        c.set_reg(0, (-100i32) as u32);
        c.set_reg(1, (-100i32) as u32);
        crate::bios::run(&mut c, &mut bus, 0x0A);
        assert!((c.reg_raw(0) as i32 - 0xA000).abs() <= 4);
        // ArcTan of 1.0 (1.14) is 45 degrees.
        c.set_reg(0, 0x4000);
        crate::bios::run(&mut c, &mut bus, 0x09);
        assert!((c.reg_raw(0) as i32 - 0x2000).abs() <= 4);
    }

    #[test]
    fn div_by_zero_follows_the_hle_convention() {
        let mut c = Cpu::new();
        let mut bus = crate::bus::Bus::new(vec![0; 0x8000]);
        c.set_reg(0, (-7i32) as u32);
        c.set_reg(1, 0);
        crate::bios::run(&mut c, &mut bus, 0x06);
        assert_eq!(c.reg_raw(0), (-1i32) as u32);
        assert_eq!(c.reg_raw(1), (-7i32) as u32);
        assert_eq!(c.reg_raw(3), 1);
    }

    #[test]
    fn sqrt_is_exact_integer_floor() {
        let mut c = Cpu::new();
        let mut bus = crate::bus::Bus::new(vec![0; 0x8000]);
        for (v, r) in [
            (0u32, 0u32),
            (1, 1),
            (2, 1),
            (81, 9),
            (99, 9),
            (u32::MAX, 65535),
        ] {
            c.set_reg(0, v);
            crate::bios::run(&mut c, &mut bus, 0x08);
            assert_eq!(c.reg_raw(0), r, "sqrt({v})");
        }
    }

    #[test]
    fn cpu_set_fill_mode_repeats_the_first_unit() {
        let mut bus = crate::bus::Bus::new(vec![0; 0x8000]);
        bus.write16(0x0300_0000, 0xBEEF);
        let mut c = Cpu::new();
        c.set_reg(0, 0x0300_0000);
        c.set_reg(1, 0x0200_0000);
        c.set_reg(2, 3 | (1 << 24));
        crate::bios::run(&mut c, &mut bus, 0x0B);
        assert_eq!(bus.read16(0x0200_0004), 0xBEEF);
        // CpuFastSet rounds a count of 3 words up to 8.
        c.set_reg(0, 0x0300_0000);
        c.set_reg(1, 0x0200_1000);
        c.set_reg(2, 3 | (1 << 24));
        crate::bios::run(&mut c, &mut bus, 0x0C);
        assert_eq!(bus.read32(0x0200_101C), 0xBEEF);
    }

    #[test]
    fn bg_affine_set_rotates_by_ninety_degrees() {
        let mut bus = crate::bus::Bus::new(vec![0; 0x8000]);
        let src = 0x0300_0000;
        bus.write32(src, 0x1000 << 8); // cx
        bus.write32(src + 4, 0x2000 << 8); // cy
        bus.write16(src + 8, 120); // px
        bus.write16(src + 10, 80); // py
        bus.write16(src + 12, 0x100); // sx = 1.0
        bus.write16(src + 14, 0x100); // sy = 1.0
        bus.write16(src + 16, 0x4000); // 90 degrees
        let mut c = Cpu::new();
        c.set_reg(0, src);
        c.set_reg(1, 0x0300_0100);
        c.set_reg(2, 1);
        crate::bios::run(&mut c, &mut bus, 0x0E);
        let pa = bus.read16(0x0300_0100) as i16;
        let pb = bus.read16(0x0300_0102) as i16;
        let pc = bus.read16(0x0300_0104) as i16;
        let pd = bus.read16(0x0300_0106) as i16;
        assert_eq!((pa, pb, pc, pd), (0, -0x100, 0x100, 0));
        let dx = bus.read32(0x0300_0108) as i32;
        let dy = bus.read32(0x0300_010C) as i32;
        assert_eq!(dx, (0x1000 << 8) + 0x100 * 80);
        assert_eq!(dy, (0x2000 << 8) - 0x100 * 120);
    }

    #[test]
    fn obj_affine_set_writes_with_the_oam_stride() {
        let mut bus = crate::bus::Bus::new(vec![0; 0x8000]);
        bus.write16(0x0300_0000, 0x200); // sx = 2.0
        bus.write16(0x0300_0002, 0x080); // sy = 0.5
        bus.write16(0x0300_0004, 0); // angle 0
        let mut c = Cpu::new();
        c.set_reg(0, 0x0300_0000);
        c.set_reg(1, 0x0700_0006);
        c.set_reg(2, 1);
        c.set_reg(3, 8);
        crate::bios::run(&mut c, &mut bus, 0x0F);
        assert_eq!(bus.read16(0x0700_0006), 0x200);
        assert_eq!(bus.read16(0x0700_000E), 0);
        assert_eq!(bus.read16(0x0700_0016), 0);
        assert_eq!(bus.read16(0x0700_001E), 0x080);
    }

    #[test]
    fn bit_unpack_widens_units_and_applies_the_offset() {
        let mut bus = crate::bus::Bus::new(vec![0; 0x8000]);
        // Two source bytes of 1-bit units -> 4-bit units, offset 5 added to
        // non-zero units only: 0b0000_0011, 0b1000_0000.
        bus.write8(0x0300_0000, 0b0000_0011);
        bus.write8(0x0300_0001, 0b1000_0000);
        let info = 0x0300_0100;
        bus.write16(info, 2);
        bus.write8(info + 2, 1);
        bus.write8(info + 3, 4);
        bus.write32(info + 4, 5);
        let mut c = Cpu::new();
        c.set_reg(0, 0x0300_0000);
        c.set_reg(1, 0x0200_0000);
        c.set_reg(2, info);
        crate::bios::run(&mut c, &mut bus, 0x10);
        assert_eq!(bus.read32(0x0200_0000), 0x0000_0066);
        assert_eq!(bus.read32(0x0200_0004), 0x6000_0000);
    }

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
        assert!(!crate::bios::run(&mut c, &mut bus, 0x2B));
        // Stubbed driver/multiboot entry points report success.
        assert!(crate::bios::run(&mut c, &mut bus, 0x1A));
        assert!(crate::bios::run(&mut c, &mut bus, 0x25));
    }

    #[test]
    fn midi_key_to_freq_scales_by_semitones() {
        let mut bus = crate::bus::Bus::new(vec![0; 0x8000]);
        bus.write32(0x0300_0004, 0x1000_0000); // WaveData.freq
        let run = |c: &mut Cpu, bus: &mut crate::bus::Bus, key: u32, fine: u32| {
            c.set_reg(0, 0x0300_0000);
            c.set_reg(1, key);
            c.set_reg(2, fine);
            crate::bios::run(c, bus, 0x1F);
            c.reg_raw(0)
        };
        let mut c = Cpu::new();
        let at60 = run(&mut c, &mut bus, 60, 0);
        assert_eq!(at60, 0x1000_0000 >> 11);
        assert_eq!(run(&mut c, &mut bus, 72, 0), at60 * 2, "octave doubles");
        let at61 = run(&mut c, &mut bus, 61, 0);
        let between = run(&mut c, &mut bus, 60, 128);
        assert!(at60 < between && between < at61, "fine adjust interpolates");
        assert_eq!(
            run(&mut c, &mut bus, 200, 0),
            run(&mut c, &mut bus, 178, 255),
            "keys above 178 clamp"
        );
    }

    #[test]
    fn soft_reset_restarts_the_cartridge_in_sys_mode() {
        let mut bus = crate::bus::Bus::new(vec![0; 0x8000]);
        bus.write32(0x0300_7FFC, 0x0300_2750);
        let mut c = Cpu::new();
        c.set_reg(0, 0x1234);
        assert!(crate::bios::run(&mut c, &mut bus, 0x00));
        assert_eq!(c.pc(), 0x0800_0000);
        assert_eq!(c.cpsr() & 0x1F, 0x1F);
        assert_eq!(c.reg_raw(13), 0x0300_7F00);
        assert_eq!(c.reg_raw(0), 0);
        assert_eq!(bus.read32(0x0300_7FFC), 0, "BIOS area cleared");
        c.set_cpsr(crate::cpu::mode::IRQ);
        assert_eq!(c.reg_raw(13), 0x0300_7FA0);
        // The return-address flag selects EWRAM.
        bus.write8(0x0300_7FFA, 1);
        assert!(crate::bios::run(&mut c, &mut bus, 0x00));
        assert_eq!(c.pc(), 0x0200_0000);
    }

    #[test]
    fn register_ram_reset_preserves_top_of_iwram_and_blanks_dispcnt() {
        let mut bus = crate::bus::Bus::new(vec![0; 0x8000]);
        bus.write32(0x0300_0000, 0xCAFE_BABE);
        bus.write32(0x0300_7E00, 0x1234_5678);
        bus.write32(0x0300_7FFC, 0xDEAD_BEEF);
        bus.io.write16(0x00, 0x0103);
        let mut c = Cpu::new();
        c.set_reg(0, 0x02);
        crate::bios::run(&mut c, &mut bus, 0x01);
        assert_eq!(bus.read32(0x0300_0000), 0);
        assert_eq!(bus.read32(0x0300_7DFC), 0);
        assert_eq!(bus.read32(0x0300_7E00), 0x1234_5678);
        assert_eq!(bus.read32(0x0300_7FFC), 0xDEAD_BEEF);
        assert_eq!(bus.io.read16(0x00), 0x0080);
    }

    #[test]
    fn register_ram_reset_resets_io_groups() {
        let mut bus = crate::bus::Bus::new(vec![0; 0x8000]);
        bus.write16(0x0400_0200, 0x3FFF); // IE
        bus.io.raise_irq(0xFFFF);
        bus.write16(0x0400_0088, 0xFFFF); // SOUNDBIAS
        bus.write16(0x0400_0134, 0x0000); // RCNT
        bus.write16(0x0400_0020, 0x0000); // BG2PA
        let mut c = Cpu::new();
        c.set_reg(0, 0xE0);
        crate::bios::run(&mut c, &mut bus, 0x01);
        assert_eq!(bus.io.ie(), 0);
        assert_eq!(bus.io.iflags(), 0);
        assert_eq!(bus.read16(0x0400_0088), 0x200);
        assert_eq!(bus.read16(0x0400_0134), 0x8000);
        assert_eq!(bus.read16(0x0400_0020), 0x100);
    }

    #[test]
    fn intr_wait_without_discard_returns_on_a_set_mirror_flag() {
        let mut bus = crate::bus::Bus::new(vec![0; 0x8000]);
        bus.write32(super::BIOS_IF_ADDR, 0x0001);
        let mut c = Cpu::new();
        c.set_reg(0, 0);
        c.set_reg(1, 1);
        assert!(crate::bios::run(&mut c, &mut bus, 0x04));
        assert_eq!(c.bios_wait_mask(), None);
        assert_eq!(bus.read32(super::BIOS_IF_ADDR), 0, "flag consumed");
        // With discard set the stale flag is dropped and the wait begins.
        bus.write32(super::BIOS_IF_ADDR, 0x0001);
        c.set_reg(0, 1);
        assert!(crate::bios::run(&mut c, &mut bus, 0x04));
        assert_eq!(c.bios_wait_mask(), Some(1));
    }
}
