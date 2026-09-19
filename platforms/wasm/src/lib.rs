//! WebAssembly bindings for CrabBoy.
//!
//! A thin `wasm-bindgen` wrapper over `crab-systems`, so the browser runs the
//! same `Box<dyn System>` as the desktop and cli frontends for every console.
//! The bindings are compiled only for `wasm32`; on native targets this crate
//! is empty so it can live in the workspace without pulling in wasm-bindgen.
//!
//! The web UI (canvas, input, audio, saves) lives in `web/` and talks to the
//! emulator only through [`bindings::Emulator`].

#[cfg(target_arch = "wasm32")]
pub mod bindings {
    use emu_core::{Axis, Button, Layout, System, DMG_PALETTE};
    use wasm_bindgen::prelude::*;

    /// Button order used by [`Emulator::set_button`]; also returned by
    /// [`button_names`] so the JS side never hard-codes it.
    const BUTTONS: [Button; 14] = [
        Button::A,
        Button::B,
        Button::Select,
        Button::Start,
        Button::Right,
        Button::Left,
        Button::Up,
        Button::Down,
        Button::R,
        Button::L,
        Button::X,
        Button::Y,
        Button::ZL,
        Button::ZR,
    ];

    /// Comma-separated button names, indexed as [`Emulator::set_button`]
    /// expects: `A,B,Select,Start,Right,Left,Up,Down,R,L,X,Y,ZL,ZR`.
    #[wasm_bindgen]
    pub fn button_names() -> String {
        "A,B,Select,Start,Right,Left,Up,Down,R,L,X,Y,ZL,ZR".to_string()
    }

    /// Identify a ROM from its header: `"gb"`, `"gbc"`, `"gba"` or `""`.
    #[wasm_bindgen]
    pub fn detect(rom: &[u8]) -> String {
        crab_systems::detect(rom)
            .map(|k| k.name())
            .unwrap_or("")
            .to_string()
    }

    /// A running console.
    #[wasm_bindgen]
    pub struct Emulator {
        sys: Box<dyn System>,
        rgba: Vec<u8>,
        layout: Layout,
    }

    #[wasm_bindgen]
    impl Emulator {
        /// Create an emulator from ROM bytes, detecting the console from
        /// the header. GBA ROMs boot with high-level BIOS emulation.
        #[wasm_bindgen(constructor)]
        pub fn new(rom: &[u8]) -> Result<Emulator, JsValue> {
            Self::build(rom, None, false)
        }

        /// Create an emulator that boots a GBA ROM through a real 16 KB
        /// BIOS image (`cold` runs the logo intro). Ignored for Game Boy ROMs.
        pub fn with_bios(rom: &[u8], bios: &[u8], cold: bool) -> Result<Emulator, JsValue> {
            Self::build(rom, Some(bios.to_vec()), cold)
        }

        fn build(rom: &[u8], gba_bios: Option<Vec<u8>>, cold: bool) -> Result<Emulator, JsValue> {
            let opts = crab_systems::LoadOptions { gba_bios, cold };
            let sys =
                crab_systems::load_with(rom.to_vec(), &opts).map_err(|e| JsValue::from_str(&e))?;
            let layout = Layout::of(sys.as_ref());
            let mut emu = Emulator {
                sys,
                rgba: vec![0; layout.width as usize * layout.height as usize * 4],
                layout,
            };
            emu.render();
            Ok(emu)
        }

        fn render(&mut self) {
            self.layout
                .compose(self.sys.as_ref(), &DMG_PALETTE)
                .write_rgba(&DMG_PALETTE, &mut self.rgba);
        }

        /// Console identifier: `"gb"`, `"gbc"` or `"gba"`.
        pub fn name(&self) -> String {
            self.sys.name().to_string()
        }

        /// Cartridge title from the header (may be empty).
        pub fn title(&self) -> String {
            self.sys.title()
        }

        /// Human-readable cartridge description.
        pub fn info(&self) -> String {
            self.sys.info()
        }

        /// Width of the framebuffer: every display of the console, stacked.
        pub fn width(&self) -> u32 {
            self.layout.width as u32
        }

        pub fn height(&self) -> u32 {
            self.layout.height as u32
        }

        /// Nominal video frame rate in Hz.
        pub fn frame_rate(&self) -> f64 {
            self.sys.frame_rate()
        }

        /// Audio sample rate in Hz (may change between frames on the GBC).
        pub fn audio_rate(&self) -> u32 {
            self.sys.audio_rate()
        }

        /// Run one video frame and refresh the RGBA framebuffer.
        pub fn run_frame(&mut self) {
            self.sys.run_frame();
            self.render();
        }

        /// Pointer into wasm memory of the `width * height * 4` byte RGBA
        /// framebuffer, valid until the next call into the emulator. Read it
        /// as `new Uint8ClampedArray(memory.buffer, ptr, len)`.
        pub fn frame_ptr(&self) -> *const u8 {
            self.rgba.as_ptr()
        }

        pub fn frame_len(&self) -> usize {
            self.rgba.len()
        }

        /// A copy of the RGBA framebuffer.
        pub fn frame_rgba(&self) -> Vec<u8> {
            self.rgba.clone()
        }

        /// Drain audio produced since the last call as interleaved stereo
        /// `f32` samples in `-1.0..=1.0` at [`Emulator::audio_rate`].
        pub fn take_audio(&mut self) -> Vec<f32> {
            self.sys.take_audio().samples
        }

        /// Press or release a button by its index in [`button_names`].
        pub fn set_button(&mut self, index: u8, pressed: bool) {
            if let Some(&b) = BUTTONS.get(index as usize) {
                if pressed {
                    self.sys.press(b);
                } else {
                    self.sys.release(b);
                }
            }
        }

        /// Hold the stylus at a framebuffer pixel. Points outside a
        /// touch-sensitive display (the second one of the 3DS) lift it.
        pub fn set_touch(&mut self, x: u32, y: u32) {
            let point = self
                .layout
                .locate(x.min(u16::MAX as u32) as u16, y.min(u16::MAX as u32) as u16)
                .and_then(|(index, x, y)| (index == 1).then_some((x, y)));
            self.sys.set_touch(point);
        }

        pub fn clear_touch(&mut self) {
            self.sys.set_touch(None);
        }

        /// Move the circle pad; both axes span `-32767..=32767`, `y` up.
        pub fn set_circle_pad(&mut self, x: i16, y: i16) {
            self.sys.set_axis(Axis::CirclePad, x, y);
        }

        /// Reset to power-on state, keeping the cartridge.
        pub fn reset(&mut self) {
            self.sys.reset();
            self.render();
        }

        pub fn battery_backed(&self) -> bool {
            self.sys.battery_backed()
        }

        /// Raw battery save bytes (empty when the cartridge has none).
        pub fn save_data(&self) -> Vec<u8> {
            self.sys.save_data()
        }

        pub fn load_data(&mut self, data: &[u8]) {
            self.sys.load_data(data);
        }

        /// True once when the game wrote its save data since the last call.
        pub fn sram_changed(&mut self) -> bool {
            self.sys.sram_changed()
        }

        pub fn rtc_data(&self) -> Vec<u8> {
            self.sys.rtc_data()
        }

        pub fn load_rtc(&mut self, data: &[u8]) {
            self.sys.load_rtc(data);
        }

        /// Serialise the whole machine (empty when unsupported).
        pub fn save_state(&self) -> Vec<u8> {
            self.sys.save_state()
        }

        pub fn load_state(&mut self, data: &[u8]) -> Result<(), JsValue> {
            self.sys
                .load_state(data)
                .map_err(|e| JsValue::from_str(&e))?;
            self.render();
            Ok(())
        }
    }
}
