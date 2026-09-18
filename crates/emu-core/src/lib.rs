//! CrabBoy core framework.
//!
//! This crate defines the traits and shared types that let every console core
//! (GB, GBA and the 3DS) present a uniform, platform-agnostic interface to
//! frontends. It has **no** GUI/OS/wasm dependencies so it compiles unchanged
//! for `wasm32-unknown-unknown`.
//!
//! The three abstraction boundaries are:
//!
//! * [`System`] - what a frontend drives. Every console implements it; a
//!   frontend only ever talks to `Box<dyn System>`.
//! * [`Host`] - what a platform provides (audio, video, persistence). The core
//!   pushes into it; it never imports eframe/winit/wasm-bindgen.
//! * [`Bus`] / [`Device`] / [`Addressable`] - the memory & clock model. Bus
//!   addresses are `u32` so GBA's 32-bit bus needs no framework changes.

pub mod audio;
pub mod bus;
pub mod device;
pub mod host;
pub mod input;
pub mod mem;
pub mod state;
pub mod system;
pub mod video;

pub use audio::Sample;
pub use bus::{Addressable, Bus};
pub use device::Device;
pub use host::Host;
pub use input::{Axis, Button, Motion};
pub use mem::Mem;
pub use system::System;
pub use video::{Frame, DMG_PALETTE};

/// Common error type for core operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidRom(String),
    UnsupportedSystem(String),
    OutOfMemory,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidRom(m) => write!(f, "invalid ROM: {m}"),
            Error::UnsupportedSystem(m) => write!(f, "unsupported system: {m}"),
            Error::OutOfMemory => write!(f, "out of memory"),
        }
    }
}

impl std::error::Error for Error {}

/// The native resolution of a console's framebuffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Screen {
    pub width: u16,
    pub height: u16,
}

impl Screen {
    pub const fn new(width: u16, height: u16) -> Self {
        Screen { width, height }
    }

    pub fn pixels(&self) -> usize {
        self.width as usize * self.height as usize
    }
}
