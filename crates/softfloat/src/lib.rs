//! Software floating point over integers.
//!
//! CrabBoy's emulation path is integer-only so that every host produces the
//! same frames (`guidelines.md`, section 1). The 3DS has two floating-point
//! units, so both are modelled here in integer arithmetic:
//!
//! * IEEE-754 single and double precision with the VFPv2 controls of the
//!   ARM11: rounding mode, flush-to-zero, default NaN and the cumulative
//!   exception flags. NaN propagation and tininess detection (before
//!   rounding) follow the ARM rules.
//! * The PICA200's shader and register formats, which are not IEEE (to come
//!   with the GPU).
//!
//! Host floats appear only in `#[cfg(test)]` cross-checks.

mod ieee;

pub use ieee::{Env, Flags, Round, F32, F64};

#[cfg(test)]
mod tests;
