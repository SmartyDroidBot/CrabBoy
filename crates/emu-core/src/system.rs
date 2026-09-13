//! The frontend-facing system abstraction.

use crate::{audio::AudioBuffer, input::Button, video::Frame, Screen};

/// A complete console as seen by a frontend.
///
/// Frontends should hold a `Box<dyn System>` and nothing else from the core
/// crate, so adding a new console (GBA) requires no frontend changes.
pub trait System {
    /// Stable console identifier, e.g. `"gb"`, `"gbc"`, `"gba"`.
    fn name(&self) -> &'static str;

    /// Human-readable description of the loaded cartridge.
    fn info(&self) -> String;

    /// The cartridge title from its header, trimmed (may be empty).
    fn title(&self) -> String;

    /// Native display resolution, without allocating a frame.
    fn screen(&self) -> Screen;

    /// Nominal video frame rate in Hz, for host pacing only (both the Game
    /// Boy and the GBA refresh at 59.7275 Hz).
    fn frame_rate(&self) -> f64 {
        59.7275
    }

    /// Reset the system to power-on state (keeps loaded cartridge).
    fn reset(&mut self);

    /// Press or release a logical button (see [`Button`]).
    fn press(&mut self, button: Button);
    fn release(&mut self, button: Button);

    /// Execute one instruction (or idle cycle), returning cycles consumed.
    fn step(&mut self) -> u32;

    /// Number of master cycles in one full video frame.
    fn frame_cycles(&self) -> u32;

    /// Run exactly one full video frame.
    fn run_frame(&mut self) {
        let total = self.frame_cycles();
        let mut cycles = 0u32;
        while cycles < total {
            cycles += self.step();
        }
    }

    /// Current framebuffer. Colour consoles fill [`Frame::rgb`]; monochrome
    /// ones only the 2-bit shades. Frontends should render through
    /// [`Frame::write_rgba`], which handles both.
    fn frame(&self) -> Frame;

    /// Zero-copy view of the current framebuffer as 2-bit shades (`0..=3` per
    /// pixel), a fast path for the Game Boy family. Colour-only consoles
    /// return `&[]`; use [`System::frame`] for them.
    fn framebuffer(&self) -> &[u8] {
        &[]
    }

    /// Audio output sample rate in Hz. Frontends should feed `take_audio()`
    /// samples to their sink at this rate. The rate can change between frames
    /// (e.g. GBC double-speed mode raises the APU rate from 8192 to 16384).
    fn audio_rate(&self) -> u32 {
        8192
    }

    /// Drain audio samples produced since the last call (interleaved stereo).
    /// Returns an empty buffer for systems without audio.
    fn take_audio(&mut self) -> AudioBuffer {
        AudioBuffer::new()
    }

    /// Whether the cartridge has battery-backed save RAM.
    fn battery_backed(&self) -> bool;

    /// The raw save-data bytes (empty if none).
    fn save_data(&self) -> Vec<u8>;

    /// Load raw save-data bytes (must match `save_data` shape).
    fn load_data(&mut self, data: &[u8]);

    /// Returns `true` once if battery-backed state (SRAM or a clock) changed
    /// since the last call, clearing the flag. Frontends use this to flush
    /// `.sav`/`.rtc` files exactly when the game writes its save data, instead
    /// of on a wall-clock timer. Default: `false` (no dirty tracking).
    fn sram_changed(&mut self) -> bool {
        false
    }

    /// Raw bytes of a real-time clock (empty if the system has none).
    fn rtc_data(&self) -> Vec<u8> {
        Vec::new()
    }

    /// Load raw real-time-clock bytes (see [`System::rtc_data`]).
    fn load_rtc(&mut self, _data: &[u8]) {}

    /// Serialize the entire machine state (save state). Empty when unsupported.
    fn save_state(&self) -> Vec<u8> {
        Vec::new()
    }

    /// Restore a machine state produced by [`System::save_state`].
    fn load_state(&mut self, _data: &[u8]) -> Result<(), String> {
        Err("save states not supported by this system".to_string())
    }
}
