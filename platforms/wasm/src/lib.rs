//! WebAssembly bindings for CrabBoy.
//!
//! This crate is a thin, `wasm-bindgen`-exposed wrapper around `gb-core`. It is
//! intentionally gated behind the `wasm` feature so native/workspace builds do
//! not pull in wasm-bindgen.
//!
//! The web UI (canvas, input, audio) lives outside this crate and talks to the
//! emulator through these bindings.

#[cfg(feature = "wasm")]
mod bindings {
    use emu_core::{Button, System};
    use gb_core::cartridge::Cartridge;
    use gb_core::gb::{Gb as GbSystem, FRAME_CYCLES};
    use wasm_bindgen::prelude::*;

    fn button_from_mask(mask: u8) -> Option<Button> {
        Some(match mask {
            gb_core::joypad::BUTTON_A => Button::A,
            gb_core::joypad::BUTTON_B => Button::B,
            gb_core::joypad::BUTTON_START => Button::Start,
            gb_core::joypad::BUTTON_SELECT => Button::Select,
            gb_core::joypad::BUTTON_LEFT => Button::Left,
            gb_core::joypad::BUTTON_RIGHT => Button::Right,
            gb_core::joypad::BUTTON_UP => Button::Up,
            gb_core::joypad::BUTTON_DOWN => Button::Down,
            _ => return None,
        })
    }

    /// A wasm-visible handle to a running emulator.
    #[wasm_bindgen]
    pub struct Gb {
        emu: Box<dyn System>,
    }

    #[wasm_bindgen]
    impl Gb {
        /// Create an emulator from a raw ROM byte slice.
        #[wasm_bindgen(constructor)]
        pub fn new(rom: &[u8]) -> Result<Gb, JsValue> {
            let cart = Cartridge::load(rom).map_err(|e| JsValue::from_str(&e))?;
            Ok(Gb {
                emu: GbSystem::system(cart),
            })
        }

        /// Advance the emulator by one frame (70224 cycles).
        pub fn step_frame(&mut self) {
            let mut cycles = 0u32;
            while cycles < FRAME_CYCLES {
                cycles += self.emu.step();
            }
        }

        /// Current 160x144 framebuffer as 2-bit shades (0..=3) per pixel.
        pub fn framebuffer(&self) -> Box<[u8]> {
            self.emu.frame().shades.into_boxed_slice()
        }

        /// Drain audio produced since the last call as interleaved stereo f32
        /// samples in `-1.0..=1.0` (sample rate 8192 Hz).
        pub fn take_audio(&mut self) -> Vec<f32> {
            self.emu.take_audio().samples
        }

        /// Press (or release) a button by its GB bitmask (see gb_core::joypad).
        pub fn set_button(&mut self, button: u8, pressed: bool) {
            if let Some(b) = button_from_mask(button) {
                if pressed {
                    self.emu.press(b);
                } else {
                    self.emu.release(b);
                }
            }
        }
    }
}