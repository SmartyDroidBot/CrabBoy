//! Cross-checks against the host's IEEE-754 arithmetic (round to nearest
//! only, the one mode a host is sure to be in) and directed tests for what a
//! host cannot show: the other rounding modes, the flags and the ARM modes.

use crate::{Env, Flags, Round, F32, F64};
use std::cmp::Ordering;
use std::ops::Neg;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Bit patterns biased towards the interesting corners: zeros, subnormals,
    /// the largest and smallest normals, infinities, NaNs, and values whose
    /// exponents are close to each other.
    fn f32(&mut self) -> u32 {
        let r = self.next();
        let sign = (r as u32 & 1) << 31;
        let frac = (r >> 8) as u32 & 0x7F_FFFF;
        let exp = match (r >> 1) & 7 {
            0 => 0,
            1 => 0xFF,
            2 => 1,
            3 => 0xFE,
            4 | 5 => 0x70 + ((r >> 32) as u32 & 0x1F),
            _ => (r >> 40) as u32 & 0xFF,
        };
        let frac = match (r >> 4) & 7 {
            0 => 0,
            1 => 0x7F_FFFF,
            2 => 1,
            _ => frac,
        };
        sign | exp << 23 | frac
    }

    fn f64(&mut self) -> u64 {
        let r = self.next();
        let sign = (r & 1) << 63;
        let frac = self.next() & 0xF_FFFF_FFFF_FFFF;
        let exp = match (r >> 1) & 7 {
            0 => 0,
            1 => 0x7FF,
            2 => 1,
            3 => 0x7FE,
            4 | 5 => 0x3F0 + ((r >> 32) & 0x3F),
            _ => (r >> 40) & 0x7FF,
        };
        let frac = match (r >> 4) & 7 {
            0 => 0,
            1 => 0xF_FFFF_FFFF_FFFF,
            2 => 1,
            _ => frac,
        };
        sign | exp << 52 | frac
    }
}

const ROUNDS: u32 = 300_000;

fn same32(ours: F32, host: f32, what: &str, a: u32, b: u32) {
    if host.is_nan() {
        assert!(ours.is_nan(), "{what}({a:#010x}, {b:#010x})");
    } else {
        assert_eq!(ours.0, host.to_bits(), "{what}({a:#010x}, {b:#010x})");
    }
}

fn same64(ours: F64, host: f64, what: &str, a: u64, b: u64) {
    if host.is_nan() {
        assert!(ours.is_nan(), "{what}({a:#018x}, {b:#018x})");
    } else {
        assert_eq!(ours.0, host.to_bits(), "{what}({a:#018x}, {b:#018x})");
    }
}

#[test]
fn single_precision_matches_the_host() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut env = Env::default();
    for _ in 0..ROUNDS {
        let (a, b) = (rng.f32(), rng.f32());
        let (x, y) = (f32::from_bits(a), f32::from_bits(b));
        same32(F32(a).add(F32(b), &mut env), x + y, "add", a, b);
        same32(F32(a).sub(F32(b), &mut env), x - y, "sub", a, b);
        same32(F32(a).mul(F32(b), &mut env), x * y, "mul", a, b);
        same32(F32(a).div(F32(b), &mut env), x / y, "div", a, b);
        same32(F32(a).sqrt(&mut env), x.sqrt(), "sqrt", a, 0);
        same64(F32(a).to_f64(&mut env), x as f64, "widen", a as u64, 0);
        assert_eq!(F32(a).compare(F32(b), &mut env), x.partial_cmp(&y));
    }
}

#[test]
fn double_precision_matches_the_host() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut env = Env::default();
    for _ in 0..ROUNDS {
        let (a, b) = (rng.f64(), rng.f64());
        let (x, y) = (f64::from_bits(a), f64::from_bits(b));
        same64(F64(a).add(F64(b), &mut env), x + y, "add", a, b);
        same64(F64(a).sub(F64(b), &mut env), x - y, "sub", a, b);
        same64(F64(a).mul(F64(b), &mut env), x * y, "mul", a, b);
        same64(F64(a).div(F64(b), &mut env), x / y, "div", a, b);
        same64(F64(a).sqrt(&mut env), x.sqrt(), "sqrt", a, 0);
        let narrowed = F64(a).to_f32(&mut env);
        if x.is_nan() {
            assert!(narrowed.is_nan());
        } else {
            assert_eq!(narrowed.0, (x as f32).to_bits(), "narrow({a:#018x})");
        }
        assert_eq!(F64(a).compare(F64(b), &mut env), x.partial_cmp(&y));
    }
}

#[test]
fn integer_conversions_match_the_host() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let mut env = Env::default();
    for _ in 0..ROUNDS {
        let a = rng.f32();
        let x = f32::from_bits(a);
        // Rust's `as` truncates and saturates, and maps NaN to zero: the
        // behaviour of the round-towards-zero conversions.
        assert_eq!(F32(a).to_i32(Round::Zero, &mut env), x as i32, "{a:#x}");
        assert_eq!(F32(a).to_u32(Round::Zero, &mut env), x as u32, "{a:#x}");
        let d = rng.f64();
        let y = f64::from_bits(d);
        assert_eq!(F64(d).to_i32(Round::Zero, &mut env), y as i32, "{d:#x}");
        assert_eq!(F64(d).to_u32(Round::Zero, &mut env), y as u32, "{d:#x}");

        let i = rng.next() as i32;
        assert_eq!(F32::from_i32(i, &mut env).0, (i as f32).to_bits());
        assert_eq!(
            F32::from_u32(i as u32, &mut env).0,
            (i as u32 as f32).to_bits()
        );
        assert_eq!(F64::from_i32(i, &mut env).0, (i as f64).to_bits());
        assert_eq!(
            F64::from_u32(i as u32, &mut env).0,
            (i as u32 as f64).to_bits()
        );
    }
}

/// Directed rounding brackets round-to-nearest: down <= nearest <= up, the
/// two differ by at most one unit in the last place, all agree when the
/// result is exact, and towards-zero is whichever of the two is nearer zero.
#[test]
fn directed_rounding_brackets_the_nearest_result() {
    let mut rng = Rng(0xA076_1D64_78BD_642F);
    let ops: [fn(F32, F32, &mut Env) -> F32; 4] = [F32::add, F32::sub, F32::mul, F32::div];
    for _ in 0..ROUNDS {
        let (a, b) = (F32(rng.f32()), F32(rng.f32()));
        for op in ops {
            let result = |round| {
                let mut env = Env {
                    round,
                    ..Env::default()
                };
                (op(a, b, &mut env), env.flags)
            };
            let (nearest, flags) = result(Round::Nearest);
            if nearest.is_nan() {
                continue;
            }
            let (up, _) = result(Round::Up);
            let (down, _) = result(Round::Down);
            let (zero, _) = result(Round::Zero);
            let value = |f: F32| f32::from_bits(f.0) as f64;
            if !flags.has(Flags::INEXACT) {
                // Exact results agree, except that an exact zero sum is
                // negative when rounding down.
                let magnitude = |f: F32| f.0 & 0x7FFF_FFFF;
                assert_eq!((up.0, zero.0), (nearest.0, nearest.0));
                assert_eq!(magnitude(down), magnitude(nearest));
                assert!(down == nearest || magnitude(nearest) == 0);
                continue;
            }
            assert!(value(down) < value(up), "{a:?} {b:?}");
            assert!(nearest == up || nearest == down, "{a:?} {b:?}");
            let expected_zero = if value(up) <= 0.0 { up } else { down };
            assert_eq!(zero, expected_zero, "{a:?} {b:?}");
            // Adjacent: one step in the ordered encoding apart.
            let ordinal = |f: F32| {
                let magnitude = (f.0 & 0x7FFF_FFFF) as i64;
                if f.0 >> 31 != 0 {
                    -magnitude
                } else {
                    magnitude
                }
            };
            let gap = ordinal(up) - ordinal(down);
            assert!(gap == 1 || (gap == 0 && up.0 != down.0), "{a:?} {b:?}");
        }
    }
}

const ONE: F32 = F32(0x3F80_0000);
const THREE: F32 = F32(0x4040_0000);
const INF: F32 = F32(0x7F80_0000);
const MAX: F32 = F32(0x7F7F_FFFF);
const MIN_NORMAL: F32 = F32(0x0080_0000);
const MIN_SUBNORMAL: F32 = F32(0x0000_0001);
const QNAN: F32 = F32(0x7FC0_0000);

#[test]
fn flags_accumulate_per_the_standard() {
    let mut env = Env::default();
    ONE.add(ONE, &mut env);
    assert_eq!(env.flags.0, 0, "exact");

    ONE.div(THREE, &mut env);
    assert_eq!(env.flags.0, Flags::INEXACT);

    let mut env = Env::default();
    assert_eq!(ONE.div(F32(0), &mut env), INF);
    assert_eq!(env.flags.0, Flags::DIV_BY_ZERO);

    let mut env = Env::default();
    assert_eq!(MAX.add(MAX, &mut env), INF);
    assert_eq!(env.flags.0, Flags::OVERFLOW | Flags::INEXACT);

    let mut env = Env::default();
    MIN_NORMAL.div(THREE, &mut env);
    assert_eq!(env.flags.0, Flags::UNDERFLOW | Flags::INEXACT);

    // An exact subnormal result is not an underflow.
    let mut env = Env::default();
    assert_eq!(MIN_NORMAL.div(F32(0x4000_0000), &mut env), F32(0x0040_0000));
    assert_eq!(env.flags.0, 0);

    for invalid in [
        INF.sub(INF, &mut Env::default()),
        INF.mul(F32(0), &mut Env::default()),
        F32(0).div(F32(0), &mut Env::default()),
        INF.div(INF, &mut Env::default()),
        ONE.neg().sqrt(&mut Env::default()),
    ] {
        assert_eq!(invalid, QNAN);
    }
    let mut env = Env::default();
    INF.sub(INF, &mut env);
    assert_eq!(env.flags.0, Flags::INVALID);
    assert_eq!(F32(0x8000_0000).sqrt(&mut Env::default()), F32(0x8000_0000));
}

#[test]
fn overflow_respects_the_rounding_direction() {
    let overflow = |round, negative: bool| {
        let mut env = Env {
            round,
            ..Env::default()
        };
        let operand = if negative { MAX.neg() } else { MAX };
        operand.add(operand, &mut env)
    };
    assert_eq!(overflow(Round::Zero, false), MAX);
    assert_eq!(overflow(Round::Zero, true), MAX.neg());
    assert_eq!(overflow(Round::Down, false), MAX);
    assert_eq!(overflow(Round::Down, true), INF.neg());
    assert_eq!(overflow(Round::Up, false), INF);
    assert_eq!(overflow(Round::Up, true), MAX.neg());
}

#[test]
fn exact_zero_sums_are_negative_only_when_rounding_down() {
    let sum = |round| {
        let mut env = Env {
            round,
            ..Env::default()
        };
        ONE.sub(ONE, &mut env)
    };
    assert_eq!(sum(Round::Nearest), F32(0));
    assert_eq!(sum(Round::Down), F32(0x8000_0000));
    let mut env = Env::default();
    assert_eq!(
        F32(0x8000_0000).add(F32(0x8000_0000), &mut env),
        F32(0x8000_0000)
    );
}

#[test]
fn nan_propagation_prefers_signaling_operands() {
    let snan = F32(0x7F80_0001);
    let qnan = F32(0xFFC0_1234);
    let mut env = Env::default();
    assert_eq!(qnan.add(snan, &mut env), F32(0x7FC0_0001));
    assert!(env.flags.has(Flags::INVALID));

    let mut env = Env::default();
    assert_eq!(qnan.mul(ONE, &mut env), qnan);
    assert_eq!(ONE.mul(qnan, &mut env), qnan);
    assert_eq!(env.flags.0, 0, "quiet NaNs raise nothing");

    let mut env = Env {
        default_nan: true,
        ..Env::default()
    };
    assert_eq!(qnan.add(ONE, &mut env), QNAN);
    assert_eq!(env.flags.0, 0);
    assert_eq!(snan.add(ONE, &mut env), QNAN);
    assert!(env.flags.has(Flags::INVALID));
}

#[test]
fn flush_to_zero_drops_subnormal_operands_and_results() {
    let mut env = Env {
        flush_to_zero: true,
        ..Env::default()
    };
    assert_eq!(MIN_SUBNORMAL.add(ONE, &mut env), ONE);
    assert_eq!(env.flags.0, Flags::INPUT_DENORMAL);

    let mut env = Env {
        flush_to_zero: true,
        ..Env::default()
    };
    assert_eq!(MIN_NORMAL.div(THREE, &mut env), F32(0));
    assert_eq!(env.flags.0, Flags::UNDERFLOW);
    assert_eq!(MIN_NORMAL.neg().div(THREE, &mut env), F32(0x8000_0000));
    assert_eq!(
        MIN_SUBNORMAL.compare(F32(0), &mut env),
        Some(Ordering::Equal)
    );
}

#[test]
fn comparisons_signal_per_their_kind() {
    let mut env = Env::default();
    assert_eq!(QNAN.compare(ONE, &mut env), None);
    assert_eq!(env.flags.0, 0);
    assert_eq!(QNAN.compare_signaling(ONE, &mut env), None);
    assert_eq!(env.flags.0, Flags::INVALID);

    let mut env = Env::default();
    assert_eq!(F32(0x7F80_0001).compare(ONE, &mut env), None);
    assert_eq!(env.flags.0, Flags::INVALID);
    assert_eq!(
        F32(0).compare(F32(0x8000_0000), &mut Env::default()),
        Some(Ordering::Equal)
    );
    assert_eq!(
        ONE.neg().compare(MIN_SUBNORMAL, &mut Env::default()),
        Some(Ordering::Less)
    );
}

#[test]
fn integer_conversion_rounds_and_saturates() {
    let half = F32(0x3F00_0000);
    let one_and_half = F32(0x3FC0_0000);
    let two_and_half = F32(0x4020_0000);
    let convert = |value: F32, round| value.to_i32(round, &mut Env::default());
    assert_eq!(convert(half, Round::Nearest), 0);
    assert_eq!(convert(one_and_half, Round::Nearest), 2);
    assert_eq!(convert(two_and_half, Round::Nearest), 2);
    assert_eq!(convert(two_and_half, Round::Up), 3);
    assert_eq!(convert(two_and_half.neg(), Round::Up), -2);
    assert_eq!(convert(two_and_half.neg(), Round::Down), -3);
    assert_eq!(convert(two_and_half.neg(), Round::Zero), -2);

    let mut env = Env::default();
    assert_eq!(half.to_u32(Round::Nearest, &mut env), 0);
    assert_eq!(env.flags.0, Flags::INEXACT);

    let mut env = Env::default();
    assert_eq!(ONE.neg().to_u32(Round::Zero, &mut env), 0);
    assert_eq!(env.flags.0, Flags::INVALID);

    let mut env = Env::default();
    assert_eq!(F32(0x4F00_0000).to_i32(Round::Zero, &mut env), i32::MAX);
    assert_eq!(env.flags.0, Flags::INVALID);
    let mut env = Env::default();
    assert_eq!(F32(0xCF00_0000).to_i32(Round::Zero, &mut env), i32::MIN);
    assert_eq!(env.flags.0, 0, "-2^31 is representable");
    let mut env = Env::default();
    assert_eq!(QNAN.to_i32(Round::Zero, &mut env), 0);
    assert_eq!(env.flags.0, Flags::INVALID);
}

#[test]
fn narrowing_rounds_and_keeps_nan_payloads() {
    let mut env = Env::default();
    // 1 + 2^-24 is a tie between 1 and 1 + 2^-23: to even.
    assert_eq!(F64(0x3FF0_0000_1000_0000).to_f32(&mut env), ONE);
    assert_eq!(env.flags.0, Flags::INEXACT);
    assert_eq!(
        F32(0x7FA0_0000).to_f64(&mut Env::default()),
        F64(0x7FFC_0000_0000_0000)
    );
    assert_eq!(
        F64(0xFFF8_0000_2000_0000).to_f32(&mut Env::default()),
        F32(0xFFC0_0001)
    );
}
