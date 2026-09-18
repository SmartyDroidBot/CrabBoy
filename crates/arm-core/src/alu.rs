//! Shifts and arithmetic with the flag results the ARM ARM defines.

/// The four barrel-shifter operations, in encoding order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shift {
    Lsl,
    Lsr,
    Asr,
    Ror,
}

impl Shift {
    pub fn from_bits(bits: u32) -> Shift {
        match bits & 3 {
            0 => Shift::Lsl,
            1 => Shift::Lsr,
            2 => Shift::Asr,
            _ => Shift::Ror,
        }
    }
}

/// Shift by a 5-bit immediate, where zero encodes the special forms
/// (`LSR #32`, `ASR #32`, `RRX`). Returns the result and the carry out.
pub fn shift_imm(kind: Shift, value: u32, amount: u32, carry: bool) -> (u32, bool) {
    match (kind, amount) {
        (Shift::Lsl, 0) => (value, carry),
        (Shift::Lsl, n) => (value << n, value >> (32 - n) & 1 != 0),
        (Shift::Lsr, 0) => (0, value >> 31 != 0),
        (Shift::Lsr, n) => (value >> n, value >> (n - 1) & 1 != 0),
        (Shift::Asr, 0) => (((value as i32) >> 31) as u32, value >> 31 != 0),
        (Shift::Asr, n) => (((value as i32) >> n) as u32, value >> (n - 1) & 1 != 0),
        (Shift::Ror, 0) => ((carry as u32) << 31 | value >> 1, value & 1 != 0),
        (Shift::Ror, n) => (value.rotate_right(n), value >> (n - 1) & 1 != 0),
    }
}

/// Shift by the low byte of a register, where zero leaves value and carry
/// alone and amounts of 32 and more are defined.
pub fn shift_reg(kind: Shift, value: u32, amount: u32, carry: bool) -> (u32, bool) {
    let amount = amount & 0xFF;
    if amount == 0 {
        return (value, carry);
    }
    match kind {
        Shift::Lsl => match amount {
            1..=31 => (value << amount, value >> (32 - amount) & 1 != 0),
            32 => (0, value & 1 != 0),
            _ => (0, false),
        },
        Shift::Lsr => match amount {
            1..=31 => (value >> amount, value >> (amount - 1) & 1 != 0),
            32 => (0, value >> 31 != 0),
            _ => (0, false),
        },
        Shift::Asr => match amount {
            1..=31 => (
                ((value as i32) >> amount) as u32,
                value >> (amount - 1) & 1 != 0,
            ),
            _ => (((value as i32) >> 31) as u32, value >> 31 != 0),
        },
        Shift::Ror => match amount & 31 {
            0 => (value, value >> 31 != 0),
            n => (value.rotate_right(n), value >> (n - 1) & 1 != 0),
        },
    }
}

/// `a + b + carry_in`, with the carry and signed-overflow flags.
pub fn add_with_carry(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
    let wide = a as u64 + b as u64 + carry_in as u64;
    let result = wide as u32;
    let carry = wide >> 32 != 0;
    let overflow = (!(a ^ b) & (a ^ result)) >> 31 != 0;
    (result, carry, overflow)
}

/// Saturate a 64-bit signed value to 32 bits; the flag reports saturation.
pub fn saturate(value: i64) -> (u32, bool) {
    if value > i32::MAX as i64 {
        (i32::MAX as u32, true)
    } else if value < i32::MIN as i64 {
        (i32::MIN as u32, true)
    } else {
        (value as u32, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediate_shifts_use_the_special_zero_forms() {
        assert_eq!(
            shift_imm(Shift::Lsl, 0x8000_0001, 0, true),
            (0x8000_0001, true)
        );
        assert_eq!(shift_imm(Shift::Lsl, 0x8000_0001, 1, false), (2, true));
        assert_eq!(shift_imm(Shift::Lsr, 0x8000_0000, 0, false), (0, true));
        assert_eq!(
            shift_imm(Shift::Asr, 0x8000_0000, 0, false),
            (0xFFFF_FFFF, true)
        );
        assert_eq!(shift_imm(Shift::Asr, 0x4000_0000, 0, true), (0, false));
        assert_eq!(shift_imm(Shift::Ror, 3, 0, false), (1, true));
        assert_eq!(shift_imm(Shift::Ror, 2, 0, true), (0x8000_0001, false));
        assert_eq!(
            shift_imm(Shift::Ror, 0x0000_00F0, 4, false),
            (0x0000_000F, false)
        );
        assert_eq!(
            shift_imm(Shift::Ror, 0x0000_0008, 4, false),
            (0x8000_0000, true)
        );
    }

    #[test]
    fn register_shifts_are_defined_for_large_amounts() {
        assert_eq!(shift_reg(Shift::Lsl, 5, 0, true), (5, true));
        assert_eq!(shift_reg(Shift::Lsl, 5, 0x100, true), (5, true));
        assert_eq!(shift_reg(Shift::Lsl, 1, 32, false), (0, true));
        assert_eq!(shift_reg(Shift::Lsl, 1, 33, true), (0, false));
        assert_eq!(shift_reg(Shift::Lsr, 0x8000_0000, 32, false), (0, true));
        assert_eq!(shift_reg(Shift::Lsr, 0x8000_0000, 33, true), (0, false));
        assert_eq!(
            shift_reg(Shift::Asr, 0x8000_0000, 40, false),
            (0xFFFF_FFFF, true)
        );
        assert_eq!(shift_reg(Shift::Asr, 0x7000_0000, 32, true), (0, false));
        assert_eq!(
            shift_reg(Shift::Ror, 0x8000_0000, 32, false),
            (0x8000_0000, true)
        );
        assert_eq!(
            shift_reg(Shift::Ror, 0x0000_0001, 33, false),
            (0x8000_0000, true)
        );
    }

    #[test]
    fn addition_reports_carry_and_overflow() {
        assert_eq!(add_with_carry(1, 2, false), (3, false, false));
        assert_eq!(add_with_carry(0xFFFF_FFFF, 1, false), (0, true, false));
        assert_eq!(
            add_with_carry(0x7FFF_FFFF, 1, false),
            (0x8000_0000, false, true)
        );
        assert_eq!(
            add_with_carry(0x8000_0000, 0x8000_0000, false),
            (0, true, true)
        );
        assert_eq!(add_with_carry(5, !3, true), (2, true, false));
        assert_eq!(add_with_carry(3, !5, true), (0xFFFF_FFFE, false, false));
        assert_eq!(
            add_with_carry(0xFFFF_FFFF, 0xFFFF_FFFF, true),
            (0xFFFF_FFFF, true, false)
        );
    }

    #[test]
    fn saturation_clamps_to_32_bits() {
        assert_eq!(saturate(5), (5, false));
        assert_eq!(saturate(i32::MAX as i64 + 1), (0x7FFF_FFFF, true));
        assert_eq!(saturate(i32::MIN as i64 - 1), (0x8000_0000, true));
        assert_eq!(saturate(-1), (0xFFFF_FFFF, false));
    }
}
