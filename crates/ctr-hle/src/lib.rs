//! Nintendo 3DS, high-level mode.
//!
//! Here the emulator is the console's operating system. It loads a game or
//! homebrew image straight into a process, answers the Horizon kernel's
//! supervisor calls itself, and implements the system services a program
//! talks to, so nothing dumped from a console is needed. The processor, the
//! memory and the GPU underneath are the models of `arm-core`, `ctr-core` and
//! `pica`, shared with the low-level machine.
//!
//! What the operating system does is reconstructed from public documentation
//! (3dbrew) and from how other emulators behave; it is a best account, not a
//! reference. `docs/3ds/hle.md` records it, and `docs/3ds/overview.md` has
//! the milestones.
//!
//! This crate has no GUI, OS or WebAssembly dependencies. It opens decrypted
//! images only and contains no keys.

pub mod horizon;
pub mod kernel;
pub mod memory;
pub mod result;
pub mod svc;

pub use horizon::Horizon;
