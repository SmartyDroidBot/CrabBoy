//! ARM CPU cores of the 3DS.
//!
//! One interpreter serves both processors: the ARM946E-S (ARMv5TE) security
//! processor and, later, the ARM11 MPCore (ARMv6K, VFPv2) application
//! processor. The core is generic over [`Bus`], so the MPU, the MMU and the
//! coprocessors live with the machine in `ctr-core`.
//!
//! Memory accesses can fail with [`Abort`]; the instruction then has no
//! effect on the base register (the base-restored abort model of both
//! processors) and the data-abort exception is taken.
//!
//! The ARM7TDMI in `gba-core` is deliberately not shared: its timing is
//! locked by the GBA accuracy baseline and ARMv4T lacks the coprocessor,
//! interworking, saturating, exclusive and mode-change instructions these
//! cores need.

mod alu;
mod arm;
mod bus;
mod cpu;
mod thumb;

pub use bus::{Abort, Bus, CpEffect, CpReg};
pub use cpu::{mode, psr, Arch, Cpu, Exception};

#[cfg(test)]
mod tests;
