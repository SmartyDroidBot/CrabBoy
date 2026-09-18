//! ARM CPU cores of the 3DS.
//!
//! One decoded-block interpreter serves both processors: the ARM946E-S
//! (ARMv5TE) security processor and the ARM11 MPCore (ARMv6K, VFPv2)
//! application processor. The core is generic over a memory trait, so the
//! MPU, the MMU and the coprocessors live with the machine in `ctr-core`.
//!
//! The ARM7TDMI in `gba-core` is deliberately not shared: its timing is
//! locked by the GBA accuracy baseline and ARMv4T lacks the coprocessor,
//! unaligned-access, saturating, exclusive and mode-change instructions
//! these cores need.
