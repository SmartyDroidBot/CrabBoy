//! Audio Processing Unit (APU).
//!
//! Stub for now. The real implementation (NR10-55, four channels, mixing) is a
//! future phase; it will produce [`emu_core::audio::Sample`]s pushed into the
//! [`emu_core::Host`].

use emu_core::device::Device;
use emu_core::bus::Bus;

#[derive(Debug)]
pub struct Apu {
    /// Number of audio samples produced (stub).
    pub produced: u64,
}

impl Apu {
    pub fn new() -> Apu {
        Apu { produced: 0 }
    }
}

impl Default for Apu {
    fn default() -> Self {
        Self::new()
    }
}

impl Device for Apu {
    fn kind(&self) -> &'static str {
        "APU"
    }

    fn reset(&mut self) {
        *self = Apu::new();
    }

    fn tick(&mut self, cycles: u32, _bus: &mut dyn Bus) {
        // No-op stub.
        self.produced = self.produced.saturating_add(cycles as u64);
    }
}