//! CrabBoy Game Boy Advance emulator core.
//!
//! Platform-agnostic: no GUI/OS/wasm dependencies. Frontends interact with the
//! system through the `Gba` type once implemented (see `gba`), which will
//! implement [`emu_core::System`].

pub mod apu;
pub mod bios;
pub mod bus;
pub mod cpu;
pub mod dma;
pub mod gba;
pub mod io;
pub mod ppu;
pub mod rtc;
pub mod save;
pub mod state;
pub mod timer;

pub use apu::Apu;
pub use bus::Bus;
pub use cpu::Cpu;
pub use gba::Gba;
pub use ppu::Ppu;
