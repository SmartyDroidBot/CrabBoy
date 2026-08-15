//! Framebuffer representation.

/// A console framebuffer of 2-bit shades (`0..=3`) per pixel.
///
/// The core always emits shades; converting to platform-specific RGBA/palettes
/// is the frontend's job (see e.g. the desktop palettes).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    pub width: u16,
    pub height: u16,
    /// Exactly `width * height` bytes, each `0..=3`.
    pub shades: Vec<u8>,
}

impl Frame {
    pub fn new(width: u16, height: u16) -> Self {
        Frame {
            width,
            height,
            shades: vec![0; width as usize * height as usize],
        }
    }

    /// Fill every pixel with a shade.
    pub fn fill(&mut self, shade: u8) {
        self.shades.fill(shade & 3);
    }

    /// Blit a single row of shades at `y`.
    pub fn set_row(&mut self, y: u16, row: &[u8]) {
        let w = self.width as usize;
        let start = y as usize * w;
        let end = start + row.len().min(w);
        self.shades[start..end].copy_from_slice(&row[..(end - start)]);
    }
}