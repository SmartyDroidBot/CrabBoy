//! IEEE-754 binary32 and binary64.
//!
//! A finite number is unpacked to `sig * 2^exp` with an integer significand.
//! Every operation produces an exact (or exact-plus-sticky) result in that
//! form and hands it to [`round_pack`], the only place that rounds.

use std::cmp::Ordering;

/// Rounding modes, in FPSCR `RMode` order.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Round {
    /// To nearest, ties to even.
    #[default]
    Nearest,
    /// Towards plus infinity.
    Up,
    /// Towards minus infinity.
    Down,
    /// Towards zero.
    Zero,
}

/// Cumulative exception flags, in FPSCR bit order.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Flags(pub u8);

impl Flags {
    pub const INVALID: u8 = 1 << 0;
    pub const DIV_BY_ZERO: u8 = 1 << 1;
    pub const OVERFLOW: u8 = 1 << 2;
    pub const UNDERFLOW: u8 = 1 << 3;
    pub const INEXACT: u8 = 1 << 4;
    /// A subnormal operand was replaced by zero in flush-to-zero mode.
    pub const INPUT_DENORMAL: u8 = 1 << 7;

    pub fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

/// The floating-point environment: the controls an operation obeys and the
/// flags it accumulates.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Env {
    pub round: Round,
    /// Replace subnormal operands and results by zero.
    pub flush_to_zero: bool,
    /// Return the default NaN instead of propagating an operand.
    pub default_nan: bool,
    pub flags: Flags,
}

impl Env {
    fn raise(&mut self, flag: u8) {
        self.flags.0 |= flag;
    }
}

#[derive(Clone, Copy)]
struct Format {
    exp_bits: u32,
    frac_bits: u32,
}

impl Format {
    const SINGLE: Format = Format {
        exp_bits: 8,
        frac_bits: 23,
    };
    const DOUBLE: Format = Format {
        exp_bits: 11,
        frac_bits: 52,
    };

    fn bias(self) -> i32 {
        (1 << (self.exp_bits - 1)) - 1
    }
    fn exp_max(self) -> u64 {
        (1 << self.exp_bits) - 1
    }
    fn emin(self) -> i32 {
        1 - self.bias()
    }
    fn frac_mask(self) -> u64 {
        (1 << self.frac_bits) - 1
    }
    fn sign_bit(self) -> u64 {
        1 << (self.exp_bits + self.frac_bits)
    }
    fn quiet_bit(self) -> u64 {
        1 << (self.frac_bits - 1)
    }
    fn pack(self, sign: bool, biased_exp: u64, frac: u64) -> u64 {
        (sign as u64) << (self.exp_bits + self.frac_bits) | biased_exp << self.frac_bits | frac
    }
    fn infinity(self, sign: bool) -> u64 {
        self.pack(sign, self.exp_max(), 0)
    }
    fn zero(self, sign: bool) -> u64 {
        self.pack(sign, 0, 0)
    }
    fn default_nan(self) -> u64 {
        self.pack(false, self.exp_max(), self.quiet_bit())
    }
    fn max_finite(self, sign: bool) -> u64 {
        self.pack(sign, self.exp_max() - 1, self.frac_mask())
    }
}

#[derive(Clone, Copy)]
enum Class {
    Zero,
    Infinity,
    Nan {
        signaling: bool,
    },
    /// `sig * 2^exp`, with `sig` non-zero.
    Finite {
        sig: u128,
        exp: i32,
    },
}

#[derive(Clone, Copy)]
struct Unpacked {
    sign: bool,
    class: Class,
    bits: u64,
}

fn unpack(fmt: Format, bits: u64, env: &mut Env) -> Unpacked {
    let sign = bits & fmt.sign_bit() != 0;
    let biased = bits >> fmt.frac_bits & fmt.exp_max();
    let frac = bits & fmt.frac_mask();
    let class = if biased == fmt.exp_max() {
        if frac == 0 {
            Class::Infinity
        } else {
            Class::Nan {
                signaling: frac & fmt.quiet_bit() == 0,
            }
        }
    } else if biased == 0 {
        if frac == 0 {
            Class::Zero
        } else if env.flush_to_zero {
            env.raise(Flags::INPUT_DENORMAL);
            Class::Zero
        } else {
            Class::Finite {
                sig: frac as u128,
                exp: fmt.emin() - fmt.frac_bits as i32,
            }
        }
    } else {
        Class::Finite {
            sig: (frac | 1 << fmt.frac_bits) as u128,
            exp: biased as i32 - fmt.bias() - fmt.frac_bits as i32,
        }
    };
    Unpacked { sign, class, bits }
}

/// Shift right, folding every bit shifted out into the sticky flag.
fn shift_right_sticky(value: u128, amount: u32, sticky: &mut bool) -> u128 {
    if amount == 0 {
        value
    } else if amount >= 128 {
        *sticky |= value != 0;
        0
    } else {
        *sticky |= value & ((1 << amount) - 1) != 0;
        value >> amount
    }
}

/// Round `(-1)^sign * sig * 2^exp` (plus something below the last bit when
/// `sticky`) into the format.
fn round_pack(fmt: Format, sign: bool, sig: u128, exp: i32, sticky: bool, env: &mut Env) -> u64 {
    debug_assert!(sig != 0);
    let precision = fmt.frac_bits as i32 + 1;
    let width = 128 - sig.leading_zeros() as i32;
    let mut e = exp + width - 1;
    let tiny = e < fmt.emin();

    if tiny && env.flush_to_zero {
        env.raise(Flags::UNDERFLOW);
        return fmt.zero(sign);
    }

    // Two extra bits below the result: the rounding bit and a sticky bit.
    let drop = if tiny {
        fmt.emin() - fmt.frac_bits as i32 - exp
    } else {
        width - precision
    } - 2;
    let mut sticky = sticky;
    let extended = if drop > 0 {
        shift_right_sticky(sig, drop as u32, &mut sticky)
    } else {
        sig << -drop
    };
    let mut kept = (extended >> 2) as u64;
    let round_bit = extended & 2 != 0;
    let sticky = sticky || extended & 1 != 0;
    let inexact = round_bit || sticky;

    let increment = match env.round {
        Round::Nearest => round_bit && (sticky || kept & 1 != 0),
        Round::Up => inexact && !sign,
        Round::Down => inexact && sign,
        Round::Zero => false,
    };
    if increment {
        kept += 1;
        if kept == 1 << precision {
            kept >>= 1;
            e += 1;
        }
    }
    if inexact {
        env.raise(Flags::INEXACT);
        if tiny {
            env.raise(Flags::UNDERFLOW);
        }
    }

    if kept >> fmt.frac_bits == 0 {
        // Subnormal, or zero after rounding.
        return fmt.pack(sign, 0, kept);
    }
    let e = if tiny { fmt.emin() } else { e };
    if e > fmt.bias() {
        env.raise(Flags::OVERFLOW | Flags::INEXACT);
        let to_infinity = match env.round {
            Round::Nearest => true,
            Round::Up => !sign,
            Round::Down => sign,
            Round::Zero => false,
        };
        return if to_infinity {
            fmt.infinity(sign)
        } else {
            fmt.max_finite(sign)
        };
    }
    fmt.pack(sign, (e + fmt.bias()) as u64, kept & fmt.frac_mask())
}

/// The NaN result of an operation with at least one NaN operand: the first
/// signaling operand, else the first quiet one, quietened; or the default
/// NaN when that mode is on.
fn propagate_nan(fmt: Format, operands: &[Unpacked], env: &mut Env) -> u64 {
    let signaling = |u: &&Unpacked| matches!(u.class, Class::Nan { signaling: true });
    let any_nan = |u: &&Unpacked| matches!(u.class, Class::Nan { .. });
    let first_signaling = operands.iter().find(signaling);
    if first_signaling.is_some() {
        env.raise(Flags::INVALID);
    }
    if env.default_nan {
        return fmt.default_nan();
    }
    let chosen = first_signaling
        .or_else(|| operands.iter().find(any_nan))
        .expect("an operand is a NaN");
    chosen.bits | fmt.quiet_bit()
}

fn invalid(fmt: Format, env: &mut Env) -> u64 {
    env.raise(Flags::INVALID);
    fmt.default_nan()
}

fn is_nan(u: &Unpacked) -> bool {
    matches!(u.class, Class::Nan { .. })
}

fn add(fmt: Format, a: u64, b: u64, subtract: bool, env: &mut Env) -> u64 {
    let a = unpack(fmt, a, env);
    let mut b = unpack(fmt, b, env);
    if is_nan(&a) || is_nan(&b) {
        return propagate_nan(fmt, &[a, b], env);
    }
    b.sign ^= subtract;

    // The sign of an exact zero sum: minus only when rounding down, unless
    // both operands agree.
    let zero_sign = |env: &Env| {
        if a.sign == b.sign {
            a.sign
        } else {
            env.round == Round::Down
        }
    };

    match (a.class, b.class) {
        (Class::Infinity, Class::Infinity) => {
            if a.sign == b.sign {
                fmt.infinity(a.sign)
            } else {
                invalid(fmt, env)
            }
        }
        (Class::Infinity, _) => fmt.infinity(a.sign),
        (_, Class::Infinity) => fmt.infinity(b.sign),
        (Class::Zero, Class::Zero) => fmt.zero(zero_sign(env)),
        (Class::Zero, Class::Finite { sig, exp }) => round_pack(fmt, b.sign, sig, exp, false, env),
        (Class::Finite { sig, exp }, Class::Zero) => round_pack(fmt, a.sign, sig, exp, false, env),
        (
            Class::Finite {
                sig: sig_a,
                exp: exp_a,
            },
            Class::Finite {
                sig: sig_b,
                exp: exp_b,
            },
        ) => {
            // Put the operand with the larger exponent first, 64 bits up, and
            // bring the other one to the same scale, keeping a sticky bit.
            let (hi, lo) = if exp_a >= exp_b {
                ((a.sign, sig_a, exp_a), (b.sign, sig_b, exp_b))
            } else {
                ((b.sign, sig_b, exp_b), (a.sign, sig_a, exp_a))
            };
            let exp = hi.2 - 64;
            let big = hi.1 << 64;
            let distance = (hi.2 - lo.2) as u32;
            let mut sticky = false;
            let small = if distance <= 64 {
                lo.1 << (64 - distance)
            } else {
                shift_right_sticky(lo.1, distance - 64, &mut sticky)
            };
            if hi.0 == lo.0 {
                round_pack(fmt, hi.0, big + small, exp, sticky, env)
            } else {
                // The true small operand exceeds its truncation when sticky,
                // so borrow one and keep the sticky bit.
                let small = small + sticky as u128;
                match big.cmp(&small) {
                    Ordering::Equal => fmt.zero(zero_sign(env)),
                    Ordering::Greater => round_pack(fmt, hi.0, big - small, exp, sticky, env),
                    Ordering::Less => round_pack(fmt, lo.0, small - big, exp, sticky, env),
                }
            }
        }
        (Class::Nan { .. }, _) | (_, Class::Nan { .. }) => unreachable!("NaNs returned above"),
    }
}

fn mul(fmt: Format, a: u64, b: u64, env: &mut Env) -> u64 {
    let a = unpack(fmt, a, env);
    let b = unpack(fmt, b, env);
    if is_nan(&a) || is_nan(&b) {
        return propagate_nan(fmt, &[a, b], env);
    }
    let sign = a.sign ^ b.sign;
    match (a.class, b.class) {
        (Class::Infinity, Class::Zero) | (Class::Zero, Class::Infinity) => invalid(fmt, env),
        (Class::Infinity, _) | (_, Class::Infinity) => fmt.infinity(sign),
        (Class::Zero, _) | (_, Class::Zero) => fmt.zero(sign),
        (
            Class::Finite {
                sig: sig_a,
                exp: exp_a,
            },
            Class::Finite {
                sig: sig_b,
                exp: exp_b,
            },
        ) => round_pack(fmt, sign, sig_a * sig_b, exp_a + exp_b, false, env),
        (Class::Nan { .. }, _) | (_, Class::Nan { .. }) => unreachable!("NaNs returned above"),
    }
}

fn div(fmt: Format, a: u64, b: u64, env: &mut Env) -> u64 {
    let a = unpack(fmt, a, env);
    let b = unpack(fmt, b, env);
    if is_nan(&a) || is_nan(&b) {
        return propagate_nan(fmt, &[a, b], env);
    }
    let sign = a.sign ^ b.sign;
    match (a.class, b.class) {
        (Class::Infinity, Class::Infinity) | (Class::Zero, Class::Zero) => invalid(fmt, env),
        (Class::Infinity, _) => fmt.infinity(sign),
        (_, Class::Infinity) | (Class::Zero, _) => fmt.zero(sign),
        (Class::Finite { .. }, Class::Zero) => {
            env.raise(Flags::DIV_BY_ZERO);
            fmt.infinity(sign)
        }
        (
            Class::Finite {
                sig: sig_a,
                exp: exp_a,
            },
            Class::Finite {
                sig: sig_b,
                exp: exp_b,
            },
        ) => {
            // Normalise the dividend to 127 bits so the quotient keeps at
            // least 74 bits whatever the operands.
            let shift = sig_a.leading_zeros() - 1;
            let dividend = sig_a << shift;
            let quotient = dividend / sig_b;
            let sticky = dividend % sig_b != 0;
            round_pack(
                fmt,
                sign,
                quotient,
                exp_a - shift as i32 - exp_b,
                sticky,
                env,
            )
        }
        (Class::Nan { .. }, _) | (_, Class::Nan { .. }) => unreachable!("NaNs returned above"),
    }
}

fn isqrt(value: u128) -> u128 {
    let mut remainder = value;
    let mut root = 0u128;
    let mut bit = 1u128 << 126;
    while bit > value {
        bit >>= 2;
    }
    while bit != 0 {
        if remainder >= root + bit {
            remainder -= root + bit;
            root = (root >> 1) + bit;
        } else {
            root >>= 1;
        }
        bit >>= 2;
    }
    root
}

fn sqrt(fmt: Format, a: u64, env: &mut Env) -> u64 {
    let a = unpack(fmt, a, env);
    match a.class {
        Class::Nan { .. } => propagate_nan(fmt, &[a], env),
        Class::Zero => fmt.zero(a.sign),
        _ if a.sign => invalid(fmt, env),
        Class::Infinity => fmt.infinity(false),
        Class::Finite { sig, exp } => {
            // Scale to an even exponent with the significand near 2^126, so
            // the root has 63 bits.
            let mut shift = sig.leading_zeros() - 1;
            if (exp - shift as i32) % 2 != 0 {
                shift -= 1;
            }
            let scaled = sig << shift;
            let root = isqrt(scaled);
            let sticky = root * root != scaled;
            round_pack(fmt, false, root, (exp - shift as i32) / 2, sticky, env)
        }
    }
}

fn compare(fmt: Format, a: u64, b: u64, signaling: bool, env: &mut Env) -> Option<Ordering> {
    let a = unpack(fmt, a, env);
    let b = unpack(fmt, b, env);
    if is_nan(&a) || is_nan(&b) {
        let any_signaling = [a, b]
            .iter()
            .any(|u| matches!(u.class, Class::Nan { signaling: true }));
        if signaling || any_signaling {
            env.raise(Flags::INVALID);
        }
        return None;
    }
    // Order by sign, then by magnitude through the biased encoding, which is
    // monotonic. Flushed subnormals compare as zeros.
    let key = |u: &Unpacked| -> (bool, u64) {
        match u.class {
            Class::Zero => (false, 0),
            _ => (u.sign, u.bits & !fmt.sign_bit()),
        }
    };
    let (sign_a, mag_a) = key(&a);
    let (sign_b, mag_b) = key(&b);
    Some(match (sign_a, sign_b) {
        (false, false) => mag_a.cmp(&mag_b),
        (true, true) => mag_b.cmp(&mag_a),
        (false, true) => Ordering::Greater,
        (true, false) => Ordering::Less,
    })
}

/// Round to an integer magnitude under `round`; the flag reports inexactness.
fn to_integer(sign: bool, sig: u128, exp: i32, round: Round) -> (u128, bool) {
    if exp >= 0 {
        // Saturate the shift: anything this large is out of every range.
        return (sig << exp.min(64), false);
    }
    let mut sticky = false;
    let extended = shift_right_sticky(sig, (-exp - 1) as u32, &mut sticky);
    let kept = extended >> 1;
    let round_bit = extended & 1 != 0;
    let inexact = round_bit || sticky;
    let increment = match round {
        Round::Nearest => round_bit && (sticky || kept & 1 != 0),
        Round::Up => inexact && !sign,
        Round::Down => inexact && sign,
        Round::Zero => false,
    };
    (kept + increment as u128, inexact)
}

fn to_int(fmt: Format, a: u64, signed: bool, round: Round, env: &mut Env) -> u32 {
    let a = unpack(fmt, a, env);
    let (low, high): (i64, i64) = if signed {
        (i32::MIN as i64, i32::MAX as i64)
    } else {
        (0, u32::MAX as i64)
    };
    let saturated = |negative: bool| (if negative { low } else { high }) as u32;
    match a.class {
        Class::Nan { .. } => {
            env.raise(Flags::INVALID);
            0
        }
        Class::Zero => 0,
        Class::Infinity => {
            env.raise(Flags::INVALID);
            saturated(a.sign)
        }
        Class::Finite { sig, exp } => {
            let (magnitude, inexact) = to_integer(a.sign, sig, exp, round);
            let in_range = if a.sign {
                magnitude <= low.unsigned_abs() as u128
            } else {
                magnitude <= high as u128
            };
            if !in_range {
                env.raise(Flags::INVALID);
                return saturated(a.sign);
            }
            if inexact {
                env.raise(Flags::INEXACT);
            }
            if a.sign {
                (magnitude as u32).wrapping_neg()
            } else {
                magnitude as u32
            }
        }
    }
}

fn from_int(fmt: Format, sign: bool, magnitude: u64, env: &mut Env) -> u64 {
    if magnitude == 0 {
        fmt.zero(false)
    } else {
        round_pack(fmt, sign, magnitude as u128, 0, false, env)
    }
}

fn convert(from: Format, to: Format, a: u64, env: &mut Env) -> u64 {
    let a = unpack(from, a, env);
    match a.class {
        Class::Nan { signaling } => {
            if signaling {
                env.raise(Flags::INVALID);
            }
            if env.default_nan {
                return to.default_nan();
            }
            // Keep the payload's high bits, as the hardware does.
            let payload = a.bits & from.frac_mask();
            let frac = if to.frac_bits >= from.frac_bits {
                payload << (to.frac_bits - from.frac_bits)
            } else {
                payload >> (from.frac_bits - to.frac_bits)
            };
            to.pack(a.sign, to.exp_max(), frac | to.quiet_bit())
        }
        Class::Zero => to.zero(a.sign),
        Class::Infinity => to.infinity(a.sign),
        Class::Finite { sig, exp } => round_pack(to, a.sign, sig, exp, false, env),
    }
}

macro_rules! float {
    ($name:ident, $bits:ty, $fmt:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
        pub struct $name(pub $bits);

        impl $name {
            pub fn add(self, other: Self, env: &mut Env) -> Self {
                $name(add($fmt, self.0 as u64, other.0 as u64, false, env) as $bits)
            }
            pub fn sub(self, other: Self, env: &mut Env) -> Self {
                $name(add($fmt, self.0 as u64, other.0 as u64, true, env) as $bits)
            }
            pub fn mul(self, other: Self, env: &mut Env) -> Self {
                $name(mul($fmt, self.0 as u64, other.0 as u64, env) as $bits)
            }
            pub fn div(self, other: Self, env: &mut Env) -> Self {
                $name(div($fmt, self.0 as u64, other.0 as u64, env) as $bits)
            }
            pub fn sqrt(self, env: &mut Env) -> Self {
                $name(sqrt($fmt, self.0 as u64, env) as $bits)
            }
            /// Clear the sign bit. Never raises a flag, even for a NaN.
            pub fn abs(self) -> Self {
                $name(self.0 & !($fmt.sign_bit() as $bits))
            }
            pub fn is_nan(self) -> bool {
                self.abs().0 as u64 > $fmt.infinity(false)
            }
            /// Compare, raising invalid only for a signaling NaN. `None` is
            /// unordered.
            pub fn compare(self, other: Self, env: &mut Env) -> Option<Ordering> {
                compare($fmt, self.0 as u64, other.0 as u64, false, env)
            }
            /// Compare, raising invalid for any NaN.
            pub fn compare_signaling(self, other: Self, env: &mut Env) -> Option<Ordering> {
                compare($fmt, self.0 as u64, other.0 as u64, true, env)
            }
            /// Convert to a signed integer under `round`, saturating with the
            /// invalid flag; a NaN gives zero.
            pub fn to_i32(self, round: Round, env: &mut Env) -> i32 {
                to_int($fmt, self.0 as u64, true, round, env) as i32
            }
            /// Convert to an unsigned integer, likewise.
            pub fn to_u32(self, round: Round, env: &mut Env) -> u32 {
                to_int($fmt, self.0 as u64, false, round, env)
            }
            pub fn from_i32(value: i32, env: &mut Env) -> Self {
                let magnitude = value.unsigned_abs() as u64;
                $name(from_int($fmt, value < 0, magnitude, env) as $bits)
            }
            pub fn from_u32(value: u32, env: &mut Env) -> Self {
                $name(from_int($fmt, false, value as u64, env) as $bits)
            }
        }
        /// Flips the sign bit. Never raises a flag, even for a NaN.
        impl std::ops::Neg for $name {
            type Output = Self;
            fn neg(self) -> Self {
                $name(self.0 ^ $fmt.sign_bit() as $bits)
            }
        }
    };
}

float!(
    F32,
    u32,
    Format::SINGLE,
    "An IEEE-754 binary32 value, as its bits."
);
float!(
    F64,
    u64,
    Format::DOUBLE,
    "An IEEE-754 binary64 value, as its bits."
);

impl F32 {
    /// Widen; exact except that a signaling NaN is quietened and raises
    /// invalid.
    pub fn to_f64(self, env: &mut Env) -> F64 {
        F64(convert(Format::SINGLE, Format::DOUBLE, self.0 as u64, env))
    }
}

impl F64 {
    /// Narrow, rounding under the environment.
    pub fn to_f32(self, env: &mut Env) -> F32 {
        F32(convert(Format::DOUBLE, Format::SINGLE, self.0, env) as u32)
    }
}
