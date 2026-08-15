//! CrabBoy Game Boy (DMG) emulator core.
//!
//! Platform-agnostic: no GUI/OS/wasm dependencies. Frontends interact with the
//! system through [`gb::Gb`], which implements [`emu_core::System`].

pub mod bus;
pub mod cartridge;
pub mod cpu;
pub mod devices;
pub mod gb;
mod state;

pub use bus::Bus;
pub use cartridge::Cartridge;
pub use cpu::Cpu;
pub use devices::joypad;
pub use gb::{Gb, FRAME_CYCLES};