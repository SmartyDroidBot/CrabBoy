//! Minimal GBA (ARM7TDMI) disassembler.
//!
//! Pure Rust, no dependencies. Used to inspect the boot/init code of a ROM
//! while debugging why a game fails to reach its title. Covers the ARMv4T
//! instruction set used by commercial games and prints a `...` marker for any
//! encoding it does not understand so unsupported opcodes are visibly flagged
//! rather than silently misdecoded.
//!
//! Usage: `gba-disasm <rom.gba> <start-hex> <count> [--thumb]`

const UNKNOWN: &str = "...";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut positional = Vec::new();
    let mut thumb_mode = false;
    for a in &args[1..] {
        if a == "--thumb" {
            thumb_mode = true;
        } else {
            positional.push(a.clone());
        }
    }
    if positional.len() < 3 {
        eprintln!("usage: gba-disasm <rom.gba> <start-hex> <count> [--thumb]");
        std::process::exit(2);
    }
    let rom = std::fs::read(&positional[0]).unwrap_or_else(|e| {
        eprintln!("failed to read {}: {e}", positional[0]);
        std::process::exit(1);
    });
    if rom.is_empty() {
        eprintln!("empty ROM");
        std::process::exit(1);
    }
    let start =
        u32::from_str_radix(positional[1].trim_start_matches("0x"), 16).unwrap_or_else(|_| {
            eprintln!("bad start address: {}", positional[1]);
            std::process::exit(2);
        });
    let count: usize = positional[2].parse().unwrap_or_else(|_| {
        eprintln!("bad count: {}", positional[2]);
        std::process::exit(2);
    });

    let byte = |addr: u32| rom[(addr as usize) % rom.len()];
    let mut addr = start;
    let mut done = 0;
    while done < count {
        if thumb_mode {
            let insn = (byte(addr) as u16) | ((byte(addr + 1) as u16) << 8);
            let next = if insn & 0xF800 == 0xF000 {
                Some((byte(addr + 2) as u16) | ((byte(addr + 3) as u16) << 8))
            } else {
                None
            };
            let (text, width) = thumb(addr, insn, next);
            println!("{addr:08X} {insn:04X}    {text}");
            addr += width;
            done += (width / 2) as usize;
        } else {
            let insn = (byte(addr) as u32)
                | ((byte(addr + 1) as u32) << 8)
                | ((byte(addr + 2) as u32) << 16)
                | ((byte(addr + 3) as u32) << 24);
            println!("{addr:08X} {insn:08X} {}", arm(addr, insn));
            addr += 4;
            done += 1;
        }
    }
}

fn u24_offset(pc: u32, off: u32) -> u32 {
    let sign = if off & 0x80_0000 != 0 { 0xFF00_0000 } else { 0 };
    pc.wrapping_add(8)
        .wrapping_add((sign | (off & 0xFF_FFFF)) << 2)
}

fn u12_imm(raw: u32) -> u32 {
    let r = (raw >> 8) & 0xF;
    (raw & 0xFF).rotate_right(r * 2)
}

fn cond(c: u32) -> &'static str {
    const T: [&str; 16] = [
        "eq", "ne", "cs", "cc", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le", "",
        "nv",
    ];
    T[((c >> 28) & 0xF) as usize]
}

fn reg(r: u32) -> String {
    match r {
        13 => "sp".to_string(),
        14 => "lr".to_string(),
        15 => "pc".to_string(),
        _ => format!("r{r}"),
    }
}

fn reglist(list: u32) -> String {
    let mut parts = Vec::new();
    let mut i = 0;
    while i < 16 {
        if list & (1 << i) != 0 {
            let mut j = i;
            while j < 15 && list & (1 << (j + 1)) != 0 {
                j += 1;
            }
            if j == i {
                parts.push(reg(i));
            } else if j == i + 1 {
                parts.push(reg(i));
                parts.push(reg(j));
            } else {
                parts.push(format!("{}-{}", reg(i), reg(j)));
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    parts.join(",")
}

/// Decode one ARM instruction. Dispatch follows the ARM7TDMI encoding map:
/// bits 27:25 select the class, with the `000`/`001` classes further split on
/// bits 7:4 and bits 24:20.
fn arm(pc: u32, w: u32) -> String {
    let c = cond(w);
    if w >> 28 == 0xF {
        return UNKNOWN.to_string(); // no unconditional space on ARMv4T
    }
    match (w >> 25) & 7 {
        0b000 => {
            if w & 0x0FFF_FFF0 == 0x012F_FF10 {
                return format!("bx{c} {}", reg(w & 0xF));
            }
            if w & 0x0FB0_0FF0 == 0x0100_0090 {
                let b = if w & (1 << 22) != 0 { "b" } else { "" };
                return format!(
                    "swp{c}{b} {},{},[{}]",
                    reg((w >> 12) & 0xF),
                    reg(w & 0xF),
                    reg((w >> 16) & 0xF)
                );
            }
            if w & 0x0F00_00F0 == 0x0000_0090 {
                return arm_multiply(w, c);
            }
            if w & 0x0E00_0090 == 0x0000_0090 {
                return arm_halfword(w, c);
            }
            if w & 0x0FBF_0FFF == 0x010F_0000 {
                let psr = if w & (1 << 22) != 0 { "spsr" } else { "cpsr" };
                return format!("mrs{c} {},{psr}", reg((w >> 12) & 0xF));
            }
            if w & 0x0FB0_FFF0 == 0x0120_F000 {
                return format!("msr{c} {},{}", psr_fields(w), reg(w & 0xF));
            }
            arm_data_processing(w, c, false)
        }
        0b001 => {
            if w & 0x0FB0_F000 == 0x0320_F000 {
                return format!("msr{c} {},#0x{:X}", psr_fields(w), u12_imm(w));
            }
            arm_data_processing(w, c, true)
        }
        0b010 | 0b011 => arm_single_transfer(w, c),
        0b100 => arm_block(w, c),
        0b101 => {
            let l = if w & (1 << 24) != 0 { "l" } else { "" };
            format!("b{l}{c} 0x{:08X}", u24_offset(pc, w & 0xFF_FFFF))
        }
        0b111 if w & (1 << 24) != 0 => format!("swi{c} #0x{:06X}", w & 0xFF_FFFF),
        _ => UNKNOWN.to_string(),
    }
}

fn psr_fields(w: u32) -> String {
    let psr = if w & (1 << 22) != 0 { "spsr" } else { "cpsr" };
    let mut f = String::new();
    for (bit, ch) in [(16, 'c'), (17, 'x'), (18, 's'), (19, 'f')] {
        if w & (1 << bit) != 0 {
            f.push(ch);
        }
    }
    format!("{psr}_{f}")
}

fn arm_multiply(w: u32, c: &str) -> String {
    let s = if w & (1 << 20) != 0 { "s" } else { "" };
    let rm = reg(w & 0xF);
    let rs = reg((w >> 8) & 0xF);
    let rn = reg((w >> 12) & 0xF);
    let rd = reg((w >> 16) & 0xF);
    match (w >> 21) & 0xF {
        0b0000 => format!("mul{c}{s} {rd},{rm},{rs}"),
        0b0001 => format!("mla{c}{s} {rd},{rm},{rs},{rn}"),
        0b0100 => format!("umull{c}{s} {rn},{rd},{rm},{rs}"),
        0b0101 => format!("umlal{c}{s} {rn},{rd},{rm},{rs}"),
        0b0110 => format!("smull{c}{s} {rn},{rd},{rm},{rs}"),
        0b0111 => format!("smlal{c}{s} {rn},{rd},{rm},{rs}"),
        _ => UNKNOWN.to_string(),
    }
}

fn arm_halfword(w: u32, c: &str) -> String {
    let l = w & (1 << 20) != 0;
    let kind = match (w >> 5) & 3 {
        0b01 => "h",
        0b10 => "sb",
        0b11 => "sh",
        _ => return UNKNOWN.to_string(),
    };
    if !l && kind != "h" {
        return UNKNOWN.to_string(); // LDRD/STRD are ARMv5
    }
    let op = if l { "ldr" } else { "str" };
    let rd = reg((w >> 12) & 0xF);
    let rn = reg((w >> 16) & 0xF);
    let sign = if w & (1 << 23) != 0 { "" } else { "-" };
    let off = if w & (1 << 22) != 0 {
        format!("#{sign}0x{:X}", ((w >> 4) & 0xF0) | (w & 0xF))
    } else {
        format!("{sign}{}", reg(w & 0xF))
    };
    let wb = if w & (1 << 21) != 0 { "!" } else { "" };
    if w & (1 << 24) != 0 {
        format!("{op}{c}{kind} {rd},[{rn},{off}]{wb}")
    } else {
        format!("{op}{c}{kind} {rd},[{rn}],{off}")
    }
}

fn arm_data_processing(w: u32, c: &str, imm: bool) -> String {
    let opcode = (w >> 21) & 0xF;
    let s = if w & (1 << 20) != 0 { "s" } else { "" };
    let rn = reg((w >> 16) & 0xF);
    let rd = reg((w >> 12) & 0xF);
    let op2 = if imm {
        format!("#0x{:X}", u12_imm(w))
    } else {
        arm_shift((w >> 4) & 0xFF, w & 0xF)
    };
    const NAMES: [&str; 16] = [
        "and", "eor", "sub", "rsb", "add", "adc", "sbc", "rsc", "tst", "teq", "cmp", "cmn", "orr",
        "mov", "bic", "mvn",
    ];
    let name = NAMES[opcode as usize];
    match opcode {
        0x8..=0xB => format!("{name}{c} {rn},{op2}"),
        0xD | 0xF => format!("{name}{c}{s} {rd},{op2}"),
        _ => format!("{name}{c}{s} {rd},{rn},{op2}"),
    }
}

fn arm_shift(shift: u32, rm: u32) -> String {
    let typ = (shift >> 5) & 3;
    let amt = shift & 0x1F;
    let t = ["lsl", "lsr", "asr", "ror"][typ as usize];
    let rm_s = reg(rm);
    if shift & 1 != 0 {
        return format!("{rm_s},{t} {}", reg((shift >> 4) & 0xF));
    }
    match (typ, amt) {
        (0, 0) => rm_s,
        (3, 0) => format!("{rm_s},rrx"),
        (1, 0) | (2, 0) => format!("{rm_s},{t} #32"),
        _ => format!("{rm_s},{t} #{amt}"),
    }
}

fn arm_single_transfer(w: u32, c: &str) -> String {
    let l = w & (1 << 20) != 0;
    let b = if w & (1 << 22) != 0 { "b" } else { "" };
    let pre = w & (1 << 24) != 0;
    let wb = w & (1 << 21) != 0;
    let t = if !pre && wb { "t" } else { "" };
    let sign = if w & (1 << 23) != 0 { "" } else { "-" };
    let rn = reg((w >> 16) & 0xF);
    let rd = reg((w >> 12) & 0xF);
    let op = if l { "ldr" } else { "str" };
    let off = if w & (1 << 25) != 0 {
        format!("{sign}{}", arm_shift((w >> 4) & 0xFF, w & 0xF))
    } else {
        format!("#{sign}0x{:X}", w & 0xFFF)
    };
    if pre {
        let wbs = if wb { "!" } else { "" };
        format!("{op}{c}{b} {rd},[{rn},{off}]{wbs}")
    } else {
        format!("{op}{c}{b}{t} {rd},[{rn}],{off}")
    }
}

fn arm_block(w: u32, c: &str) -> String {
    let l = w & (1 << 20) != 0;
    let base = reg((w >> 16) & 0xF);
    let mode = match ((w >> 24) & 1, (w >> 23) & 1) {
        (1, 1) => "ib",
        (0, 1) => "ia",
        (1, 0) => "db",
        _ => "da",
    };
    let wb = if w & (1 << 21) != 0 { "!" } else { "" };
    let s = if w & (1 << 22) != 0 { "^" } else { "" };
    let list = reglist(w & 0xFFFF);
    let op = if l { "ldm" } else { "stm" };
    format!("{op}{c}{mode} {base}{wb},{{{list}}}{s}")
}

fn thumb(pc: u32, w: u16, next: Option<u16>) -> (String, u32) {
    let h = w as u32;

    // BL: 11110 S imm11 (prefix) then 11111 imm11 (suffix).
    if h & 0xF800 == 0xF000 {
        if let Some(n) = next {
            let n = n as u32;
            if n & 0xF800 != 0xF800 {
                return ("...bl prefix".to_string(), 2);
            }
            let upper = h & 0x7FF;
            let sign = if upper & 0x400 != 0 { 0xFF80_0000 } else { 0 };
            let offset = sign | (upper << 12) | ((n & 0x7FF) << 1);
            let target = pc.wrapping_add(4).wrapping_add(offset);
            return (format!("bl 0x{target:08X}"), 4);
        }
        return ("...bl prefix".to_string(), 2);
    }

    // Format 1: LSL/LSR/ASR immediate (000xx, excluding 00011).
    if h & 0xE000 == 0 && h & 0x1800 != 0x1800 {
        let op = ["lsl", "lsr", "asr"][((h >> 11) & 3) as usize];
        return (
            format!("{op} r{},r{},#{}", h & 7, (h >> 3) & 7, (h >> 6) & 0x1F),
            2,
        );
    }

    // Format 2: add/subtract register or 3-bit immediate.
    if h & 0xF800 == 0x1800 {
        let name = if h & (1 << 9) == 0 { "add" } else { "sub" };
        let rd = h & 7;
        let rs = (h >> 3) & 7;
        let rn = (h >> 6) & 7;
        return if h & (1 << 10) != 0 {
            (format!("{name} r{rd},r{rs},#{rn}"), 2)
        } else {
            (format!("{name} r{rd},r{rs},r{rn}"), 2)
        };
    }

    // Format 3: MOV/CMP/ADD/SUB 8-bit immediate.
    if h & 0xE000 == 0x2000 {
        let op = ["mov", "cmp", "add", "sub"][((h >> 11) & 3) as usize];
        return (format!("{op} r{},#0x{:X}", (h >> 8) & 7, h & 0xFF), 2);
    }

    // Format 4: ALU operations.
    if h & 0xFC00 == 0x4000 {
        const OPS: [&str; 16] = [
            "and", "eor", "lsl", "lsr", "asr", "adc", "sbc", "ror", "tst", "neg", "cmp", "cmn",
            "orr", "mul", "bic", "mvn",
        ];
        let op = OPS[((h >> 6) & 0xF) as usize];
        return (format!("{op} r{},r{}", h & 7, (h >> 3) & 7), 2);
    }

    // Format 5: hi-register operations / BX.
    if h & 0xFC00 == 0x4400 {
        let op = (h >> 8) & 3;
        let rd = (h & 7) | ((h >> 4) & 8);
        let rs = (h >> 3) & 0xF;
        let text = match op {
            0 => format!("add {},{}", reg(rd), reg(rs)),
            1 => format!("cmp {},{}", reg(rd), reg(rs)),
            2 => format!("mov {},{}", reg(rd), reg(rs)),
            _ if h & (1 << 7) == 0 => format!("bx {}", reg(rs)),
            _ => UNKNOWN.to_string(),
        };
        return (text, 2);
    }

    // Format 6: LDR literal.
    if h & 0xF800 == 0x4800 {
        let rd = (h >> 8) & 7;
        let imm = (h & 0xFF) * 4;
        let target = (pc.wrapping_add(4) & !3).wrapping_add(imm);
        return (
            format!("ldr r{rd},[pc,#0x{imm:X}]   ; -> 0x{target:08X}"),
            2,
        );
    }

    // Formats 7 and 8: register-offset transfers.
    if h & 0xF000 == 0x5000 {
        let rd = h & 7;
        let rb = (h >> 3) & 7;
        let ro = (h >> 6) & 7;
        let op = match (h >> 9) & 7 {
            0 => "str",
            1 => "strh",
            2 => "strb",
            3 => "ldrsb",
            4 => "ldr",
            5 => "ldrh",
            6 => "ldrb",
            _ => "ldrsh",
        };
        return (format!("{op} r{rd},[r{rb},r{ro}]"), 2);
    }

    // Format 9: word/byte with 5-bit immediate offset.
    if h & 0xE000 == 0x6000 {
        let rd = h & 7;
        let rb = (h >> 3) & 7;
        let imm5 = (h >> 6) & 0x1F;
        let (op, imm) = match (h >> 11) & 3 {
            0 => ("str", imm5 * 4),
            1 => ("ldr", imm5 * 4),
            2 => ("strb", imm5),
            _ => ("ldrb", imm5),
        };
        return (format!("{op} r{rd},[r{rb},#0x{imm:X}]"), 2);
    }

    // Format 10: halfword with 5-bit immediate offset.
    if h & 0xF000 == 0x8000 {
        let op = if h & (1 << 11) == 0 { "strh" } else { "ldrh" };
        let imm = ((h >> 6) & 0x1F) * 2;
        return (format!("{op} r{},[r{},#0x{imm:X}]", h & 7, (h >> 3) & 7), 2);
    }

    // Format 11: SP-relative load/store.
    if h & 0xF000 == 0x9000 {
        let op = if h & (1 << 11) == 0 { "str" } else { "ldr" };
        return (
            format!("{op} r{},[sp,#0x{:X}]", (h >> 8) & 7, (h & 0xFF) * 4),
            2,
        );
    }

    // Format 12: load address.
    if h & 0xF000 == 0xA000 {
        let base = if h & (1 << 11) == 0 { "pc" } else { "sp" };
        return (
            format!("add r{},{base},#0x{:X}", (h >> 8) & 7, (h & 0xFF) * 4),
            2,
        );
    }

    // Formats 13/14: SP adjust, PUSH/POP.
    if h & 0xF000 == 0xB000 {
        return match (h >> 8) & 0xF {
            0x0 => {
                let op = if h & 0x80 == 0 { "add" } else { "sub" };
                (format!("{op} sp,#0x{:X}", (h & 0x7F) * 4), 2)
            }
            0x4 => (format!("push {{{}}}", reglist(h & 0xFF)), 2),
            0x5 => (format!("push {{{},lr}}", reglist(h & 0xFF)), 2),
            0xC => (format!("pop {{{}}}", reglist(h & 0xFF)), 2),
            0xD => (format!("pop {{{},pc}}", reglist(h & 0xFF)), 2),
            _ => (UNKNOWN.to_string(), 2),
        };
    }

    // Format 15: multiple load/store.
    if h & 0xF000 == 0xC000 {
        let op = if h & (1 << 11) == 0 { "stmia" } else { "ldmia" };
        return (
            format!("{op} r{}!,{{{}}}", (h >> 8) & 7, reglist(h & 0xFF)),
            2,
        );
    }

    // Formats 16/17: conditional branch / SWI.
    if h & 0xF000 == 0xD000 {
        let opcode = (h >> 8) & 0xF;
        if opcode == 0xF {
            return (format!("swi #0x{:X}", h & 0xFF), 2);
        }
        if opcode == 0xE {
            return (UNKNOWN.to_string(), 2);
        }
        let c = cond(opcode << 28);
        let imm = (h & 0xFF) as i32;
        let off = if imm & 0x80 != 0 {
            (imm - 0x100) << 1
        } else {
            imm << 1
        };
        let target = (pc as i32).wrapping_add(4).wrapping_add(off) as u32;
        return (format!("b{c} 0x{target:08X}"), 2);
    }

    // Format 18: unconditional branch.
    if h & 0xF800 == 0xE000 {
        let imm = (h & 0x7FF) as i32;
        let off = if imm & 0x400 != 0 {
            (imm - 0x800) << 1
        } else {
            imm << 1
        };
        let target = (pc as i32).wrapping_add(4).wrapping_add(off) as u32;
        return (format!("b 0x{target:08X}"), 2);
    }

    (UNKNOWN.to_string(), 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_decodes_loads_not_as_block_transfers() {
        assert_eq!(arm(0, 0xE59F_0010), "ldr r0,[pc,#0x10]");
        assert_eq!(arm(0, 0xE8BD_500F), "ldmia sp!,{r0-r3,r12,lr}");
        assert_eq!(arm(0, 0xE25E_F004), "subs pc,lr,#0x4");
        assert_eq!(arm(0, 0xE12F_FF1E), "bx lr");
        assert_eq!(arm(0, 0xE083_2190), "umull r2,r3,r0,r1");
        assert_eq!(arm(0x6C, 0x03A0_E004), "moveq lr,#0x4");
        assert_eq!(arm(0x88, 0x0A00_0000), "beq 0x00000090");
        assert_eq!(arm(0, 0xEF06_0000), "swi #0x060000");
    }

    #[test]
    fn thumb_decodes_register_offset_forms() {
        assert_eq!(thumb(0, 0x5042, None).0, "str r2,[r0,r1]");
        assert_eq!(thumb(0, 0x5E42, None).0, "ldrsh r2,[r0,r1]");
        assert_eq!(thumb(0, 0x4700, None).0, "bx r0");
        assert_eq!(thumb(0, 0xB5F0, None).0, "push {r4-r7,lr}");
        assert_eq!(thumb(0, 0xBD02, None).0, "pop {r1,pc}");
        assert_eq!(thumb(0x082E_001E, 0xF7FF, Some(0xF815)).0, "bl 0x082DF04C");
    }
}
