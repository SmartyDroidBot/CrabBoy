//! Numbers outside the shader: single-precision arithmetic in the default
//! environment, the GPU's short float register formats, and fixed point.

use softfloat::{Env, Round, F32};

pub fn add(a: F32, b: F32) -> F32 {
    a.add(b, &mut Env::default())
}

pub fn sub(a: F32, b: F32) -> F32 {
    a.sub(b, &mut Env::default())
}

pub fn mul(a: F32, b: F32) -> F32 {
    a.mul(b, &mut Env::default())
}

pub fn div(a: F32, b: F32) -> F32 {
    a.div(b, &mut Env::default())
}

/// `a < b`; false when either is NaN.
pub fn less(a: F32, b: F32) -> bool {
    a.compare(b, &mut Env::default()) == Some(std::cmp::Ordering::Less)
}

/// `a + t * (b - a)`.
pub fn lerp(a: F32, b: F32, t: F32) -> F32 {
    add(a, mul(t, sub(b, a)))
}

/// A float with one sign bit, `exponent` exponent bits and `mantissa`
/// mantissa bits in the low bits of `raw`, widened. A zero exponent is zero
/// (no subnormals); the largest is infinity or NaN.
pub fn from_short_float(raw: u32, exponent: u32, mantissa: u32) -> F32 {
    let sign = raw >> (exponent + mantissa) & 1;
    let e = raw >> mantissa & ((1 << exponent) - 1);
    let m = raw & ((1 << mantissa) - 1);
    let bias = (1 << (exponent - 1)) - 1;
    let bits = if e == 0 {
        return F32(0);
    } else if e == (1 << exponent) - 1 {
        0x7F80_0000 | m << (23 - mantissa)
    } else {
        (e + 127 - bias) << 23 | m << (23 - mantissa)
    };
    F32(sign << 31 | bits)
}

/// The 1.5.10 format of the lighting and fog registers.
pub fn from_float16(raw: u32) -> F32 {
    from_short_float(raw & 0xFFFF, 5, 10)
}

/// The 1.7.12 format of the attenuation registers.
pub fn from_float20(raw: u32) -> F32 {
    from_short_float(raw & 0xF_FFFF, 7, 12)
}

/// `value` in fixed point with `frac` fractional bits, to nearest,
/// saturated to `i32`.
pub fn fixed(value: F32, frac: u32) -> i32 {
    value
        .to_fixed(frac, Round::Nearest)
        .clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// A two's complement field of `bits` bits, sign-extended.
pub fn signed(value: u32, bits: u32) -> i32 {
    ((value << (32 - bits)) as i32) >> (32 - bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_floats_widen() {
        let f = |v: f32| F32(v.to_bits());
        // 1.5.10: 1.0 is 0x3C00, -2.5 is 0xC100.
        assert_eq!(from_float16(0x3C00), f(1.0));
        assert_eq!(from_float16(0xC100), f(-2.5));
        assert_eq!(from_float16(0x0001), F32(0));
        // 1.7.12: 1.0 is 63 << 12.
        assert_eq!(from_float20(63 << 12 | 0x800), f(1.5));
        assert_eq!(fixed(f(-1.25), 4), -20);
        assert_eq!(fixed(f(1e20), 4), i32::MAX);
        assert_eq!(signed(0x3FF, 10), -1);
        assert_eq!(signed(0x1FF, 10), 511);
    }
}
