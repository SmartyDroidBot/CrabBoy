//! Nintendo 3DS core.
//!
//! The machine is emulated at the hardware level: the ARM9 and ARM11
//! processors, the physical bus and the memory-mapped units, with firmware
//! running as it does on a console. Nothing of Nintendo's operating system is
//! reimplemented here: that is the business of `ctr-hle`, which plays games
//! without firmware and shares this crate's memory and GPU models. See
//! `docs/3ds/overview.md` for the strategy and the milestones.
//!
//! This crate has no GUI, OS or WebAssembly dependencies. No boot ROM, key or
//! firmware is part of the repository; whatever a configuration needs is
//! supplied by the user at run time.

pub mod arm11;
pub mod arm9;
pub mod boot;
pub mod bus;
pub mod clock;
pub mod ctr;
pub mod gpu;
pub mod io;
pub mod sched;

pub use ctr::Ctr;
