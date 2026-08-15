//! Shared audio types.

/// A single audio sample in `-1.0..=1.0` range.
pub type Sample = f32;

/// Interleaved stereo sample buffer.
pub struct AudioBuffer {
    pub samples: Vec<Sample>,
}

impl AudioBuffer {
    pub fn new() -> Self {
        AudioBuffer { samples: Vec::new() }
    }

    pub fn with_capacity(n: usize) -> Self {
        AudioBuffer { samples: Vec::with_capacity(n) }
    }

    pub fn push(&mut self, left: Sample, right: Sample) {
        self.samples.push(left);
        self.samples.push(right);
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

impl Default for AudioBuffer {
    fn default() -> Self {
        Self::new()
    }
}