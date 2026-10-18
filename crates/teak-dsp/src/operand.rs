//! Instruction operands: the registers an operand field names, and the
//! enumerations of operations and conditions (Teakra's `operand.h`).

/// Every register an operand can name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RegName {
    A0,
    A0l,
    A0h,
    A0e,
    A1,
    A1l,
    A1h,
    A1e,
    B0,
    B0l,
    B0h,
    B0e,
    B1,
    B1l,
    B1h,
    B1e,
    R0,
    R1,
    R2,
    R3,
    R4,
    R5,
    R6,
    R7,
    Y0,
    P,
    Pc,
    Sp,
    Sv,
    Lc,
    Ar0,
    Ar1,
    Arp0,
    Arp1,
    Arp2,
    Arp3,
    Ext0,
    Ext1,
    Ext2,
    Ext3,
    Stt0,
    Stt1,
    Stt2,
    St0,
    St1,
    St2,
    Cfgi,
    Cfgj,
    Mod0,
    Mod1,
    Mod2,
    Mod3,
    Undefined,
}

use RegName::*;

/// The general register field: `p`, `a0` and `a1` are special as sources.
pub const REGISTER: [RegName; 32] = [
    R0, R1, R2, R3, R4, R5, R7, Y0, St0, St1, St2, P, Pc, Sp, Cfgi, Cfgj, B0h, B1h, B0l, B1l, Ext0,
    Ext1, Ext2, Ext3, A0, A1, A0l, A1l, A0h, A1h, Lc, Sv,
];
pub const AX: [RegName; 2] = [A0, A1];
pub const AXL: [RegName; 2] = [A0l, A1l];
pub const AXH: [RegName; 2] = [A0h, A1h];
pub const BX: [RegName; 2] = [B0, B1];
pub const BXL: [RegName; 2] = [B0l, B1l];
pub const BXH: [RegName; 2] = [B0h, B1h];
pub const AB: [RegName; 4] = [B0, B1, A0, A1];
pub const ABL: [RegName; 4] = [B0l, B1l, A0l, A1l];
pub const ABH: [RegName; 4] = [B0h, B1h, A0h, A1h];
pub const ABE: [RegName; 4] = [B0e, B1e, A0e, A1e];
pub const ABLH: [RegName; 8] = [B0l, B0h, B1l, B1h, A0l, A0h, A1l, A1h];
pub const RN_OLD: [RegName; 8] = [R0, R1, R2, R3, R4, R5, R7, Y0];
pub const AR_ARP_STT_MOD: [RegName; 16] = [
    Ar0, Ar1, Arp0, Arp1, Arp2, Arp3, Undefined, Undefined, Stt0, Stt1, Stt2, Undefined, Mod0,
    Mod1, Mod2, Mod3,
];
pub const AR_ARP: [RegName; 8] = [Ar0, Ar1, Arp0, Arp1, Arp2, Arp3, Undefined, Undefined];
pub const STT_MOD: [RegName; 8] = [Stt0, Stt1, Stt2, Undefined, Mod0, Mod1, Mod2, Mod3];

/// How an address register moves after it is used.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepValue {
    Zero,
    Increase,
    Decrease,
    PlusStep,
    Increase2Mode1,
    Decrease2Mode1,
    Increase2Mode2,
    Decrease2Mode2,
}

pub const STEP_ZIDS: [StepValue; 4] = [
    StepValue::Zero,
    StepValue::Increase,
    StepValue::Decrease,
    StepValue::PlusStep,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlmOp {
    Or,
    And,
    Xor,
    Add,
    Tst0,
    Tst1,
    Cmp,
    Sub,
    Msu,
    Addh,
    Addl,
    Subh,
    Subl,
    Sqr,
    Sqra,
    Cmpu,
    Reserved,
}

pub const ALM: [AlmOp; 16] = [
    AlmOp::Or,
    AlmOp::And,
    AlmOp::Xor,
    AlmOp::Add,
    AlmOp::Tst0,
    AlmOp::Tst1,
    AlmOp::Cmp,
    AlmOp::Sub,
    AlmOp::Msu,
    AlmOp::Addh,
    AlmOp::Addl,
    AlmOp::Subh,
    AlmOp::Subl,
    AlmOp::Sqr,
    AlmOp::Sqra,
    AlmOp::Cmpu,
];

pub const ALU: [AlmOp; 8] = [
    AlmOp::Or,
    AlmOp::And,
    AlmOp::Xor,
    AlmOp::Add,
    AlmOp::Reserved,
    AlmOp::Reserved,
    AlmOp::Cmp,
    AlmOp::Sub,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlbOp {
    Set,
    Rst,
    Chng,
    Addv,
    Tst0,
    Tst1,
    Cmpv,
    Subv,
}

pub const ALB: [AlbOp; 8] = [
    AlbOp::Set,
    AlbOp::Rst,
    AlbOp::Chng,
    AlbOp::Addv,
    AlbOp::Tst0,
    AlbOp::Tst1,
    AlbOp::Cmpv,
    AlbOp::Subv,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MulOp {
    Mpy,
    Mpysu,
    Mac,
    Macus,
    Maa,
    Macuu,
    Macsu,
    Maasu,
}

pub const MUL3: [MulOp; 8] = [
    MulOp::Mpy,
    MulOp::Mpysu,
    MulOp::Mac,
    MulOp::Macus,
    MulOp::Maa,
    MulOp::Macuu,
    MulOp::Macsu,
    MulOp::Maasu,
];
pub const MUL2: [MulOp; 4] = [MulOp::Mpy, MulOp::Mac, MulOp::Maa, MulOp::Macsu];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModaOp {
    Shr,
    Shr4,
    Shl,
    Shl4,
    Ror,
    Rol,
    Clr,
    Reserved,
    Not,
    Neg,
    Rnd,
    Pacr,
    Clrr,
    Inc,
    Dec,
    Copy,
}

pub const MODA4: [ModaOp; 16] = [
    ModaOp::Shr,
    ModaOp::Shr4,
    ModaOp::Shl,
    ModaOp::Shl4,
    ModaOp::Ror,
    ModaOp::Rol,
    ModaOp::Clr,
    ModaOp::Reserved,
    ModaOp::Not,
    ModaOp::Neg,
    ModaOp::Rnd,
    ModaOp::Pacr,
    ModaOp::Clrr,
    ModaOp::Inc,
    ModaOp::Dec,
    ModaOp::Copy,
];
pub const MODA3: [ModaOp; 8] = [
    ModaOp::Shr,
    ModaOp::Shr4,
    ModaOp::Shl,
    ModaOp::Shl4,
    ModaOp::Ror,
    ModaOp::Rol,
    ModaOp::Clr,
    ModaOp::Clrr,
];

/// Branch and operation conditions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cond {
    True,
    Eq,
    Neq,
    Gt,
    Ge,
    Lt,
    Le,
    Nn,
    C,
    V,
    E,
    L,
    Nr,
    Niu0,
    Iu0,
    Iu1,
}

pub const COND: [Cond; 16] = [
    Cond::True,
    Cond::Eq,
    Cond::Neq,
    Cond::Gt,
    Cond::Ge,
    Cond::Lt,
    Cond::Le,
    Cond::Nn,
    Cond::C,
    Cond::V,
    Cond::E,
    Cond::L,
    Cond::Nr,
    Cond::Niu0,
    Cond::Iu0,
    Cond::Iu1,
];

/// What a product sum starts from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SumBase {
    Zero,
    Acc,
    Sv,
    SvRnd,
}

/// Bits `pos..pos + len` of an opcode.
#[inline]
pub fn f(opcode: u16, pos: u32, len: u32) -> u16 {
    (opcode >> pos) & ((1u32 << len) - 1) as u16
}

/// The low `bits` bits of `value`, sign-extended to 16 bits.
#[inline]
pub fn sext16(value: u16, bits: u32) -> u16 {
    let shift = 16 - bits;
    (((value << shift) as i16) >> shift) as u16
}

/// The low `bits` bits of `value`, sign-extended to 32 bits.
#[inline]
pub fn sext32(value: u32, bits: u32) -> u32 {
    let shift = 32 - bits;
    (((value << shift) as i32) >> shift) as u32
}

/// The low `bits` bits of `value`, sign-extended to 64 bits.
#[inline]
pub fn sext64(value: u64, bits: u32) -> u64 {
    let shift = 64 - bits;
    (((value << shift) as i64) >> shift) as u64
}

pub fn bit_reverse(value: u16) -> u16 {
    value.reverse_bits()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_extension_keeps_the_sign_bit() {
        assert_eq!(sext16(0x7F, 7), 0xFFFF);
        assert_eq!(sext16(0x3F, 7), 0x003F);
        assert_eq!(sext32(0x8, 4), 0xFFFF_FFF8);
        assert_eq!(sext64(0x80_0000_0000, 40), 0xFFFF_FF80_0000_0000);
        assert_eq!(sext64(0x7F_FFFF_FFFF, 40), 0x7F_FFFF_FFFF);
        assert_eq!(f(0xA5C3, 4, 8), 0x5C);
        assert_eq!(bit_reverse(0x0001), 0x8000);
    }
}
