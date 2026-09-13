//! Platform-provided services that a core pushes into.

use crate::audio::Sample;
use crate::video::Frame;

/// Implemented by each frontend (desktop, CLI, wasm). The core calls these and
/// never depends on the concrete platform.
pub trait Host {
    /// Receive a block of audio samples (interleaved stereo, `-1.0..=1.0`).
    /// The core may call this zero times if a system has no audio yet.
    fn audio_out(&mut self, samples: &[Sample]);

    /// Receive a finished video frame.
    fn submit_frame(&mut self, frame: &Frame);

    /// Persist save data under a namespaced key (e.g. the ROM basename).
    fn persist(&mut self, key: &str, data: &[u8]);
}
