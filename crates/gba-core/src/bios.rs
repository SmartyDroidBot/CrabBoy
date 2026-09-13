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
        0x11 => lz77(cpu, bus, Unit::Byte),
        0x12 => lz77(cpu, bus, Unit::Half),
        0x13 => huff(cpu, bus),
        0x14 => rl(cpu, bus, Unit::Byte),
        0x15 => rl(cpu, bus, Unit::Half),
        0x16 => diff8(cpu, bus, Unit::Byte),
        0x17 => diff8(cpu, bus, Unit::Half),
        0x18 => diff16(cpu, bus),
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
