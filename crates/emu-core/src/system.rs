//! The frontend-facing system abstraction.

use crate::{audio::AudioBuffer, input::Button, video::Frame};

/// A complete console as seen by a frontend.
///
/// Frontends should hold a `Box<dyn System>` and nothing else from the core
/// crate, so adding a new console (GBA) requires no frontend changes.
pub trait System {
    /// Stable console identifier, e.g. `"gb"`, `"gba"`.
    fn name(&self) -> &'static str;

    /// Human-readable description of the loaded cartridge.
    fn info(&self) -> String;

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

    /// Current framebuffer as 2-bit shades (`0..=3`) per pixel.
    fn frame(&self) -> Frame;

    /// Zero-copy view of the current framebuffer (`0..=3` per pixel). Avoids the
    /// allocation in [`System::frame`] on the hot path. Returns `&[]` when the
    /// system exposes no framebuffer here.
    fn framebuffer(&self) -> &[u8] {
        &[]
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
}