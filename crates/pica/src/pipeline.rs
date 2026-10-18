//! The per-fragment pipeline after the rasteriser: the fragment's colour,
//! then the output merger.

use crate::framebuffer::Framebuffer;
use crate::geometry::{varying, VARYING_FRAC};
use crate::memory::Memory;
use crate::raster::{Fragment, FragmentSink};

/// A varying in [0, 1] as a colour channel, to nearest.
pub fn channel(value: i32) -> u8 {
    let v = value.clamp(0, 1 << VARYING_FRAC) as i64;
    ((v * 255 + (1 << (VARYING_FRAC - 1))) >> VARYING_FRAC) as u8
}

/// The fragment stages of one draw, over memory.
pub struct Pipeline<'a> {
    pub framebuffer: &'a Framebuffer,
    pub mem: &'a mut dyn Memory,
    /// Fragments that passed every test.
    pub written: u64,
}

impl FragmentSink for Pipeline<'_> {
    fn fragment(&mut self, f: &Fragment) {
        let colour = std::array::from_fn(|i| channel(f.varyings[varying::COLOR + i]));
        if self
            .framebuffer
            .fragment(self.mem, f.x, f.y, f.depth, colour)
        {
            self.written += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_round_to_nearest() {
        assert_eq!(channel(1 << 16), 255);
        assert_eq!(channel(1 << 15), 128);
        assert_eq!(channel(-5), 0);
        assert_eq!(channel(1 << 20), 255);
    }
}
