//! Software floating point over integers.
//!
//! CrabBoy's emulation path is integer-only so that every host produces the
//! same frames (`guidelines.md`, section 1). The 3DS has two floating-point
//! units, so both are modelled here in integer arithmetic:
//!
//! * IEEE-754 single and double precision with the VFPv2 controls of the
//!   ARM11 (rounding mode, flush-to-zero, default NaN, cumulative exception
//!   flags).
//! * The PICA200's 24-bit, 20-bit and 16-bit shader and register formats,
//!   which are not IEEE.
//!
//! Host floats appear only in `#[cfg(test)]` cross-checks.
