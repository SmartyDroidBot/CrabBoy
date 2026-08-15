//! CrabBoy Game Boy Advance emulator core.
//!
//! Platform-agnostic: no GUI/OS/wasm dependencies. Frontends interact with the
//! system through the `Gba` type once implemented (see `gba`), which will
//! implement [`emu_core::System`].

pub mod cpu;

pub use cpu::Cpu;