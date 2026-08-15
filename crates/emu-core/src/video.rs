//! Framebuffer representation.

/// A console framebuffer of 2-bit shades (`0..=3`) per pixel, with an optional
/// full-colour buffer.
///
/// The core always emits shades; converting to platform-specific RGBA/palettes
/// is the frontend's job (see e.g. the desktop palettes). Systems that render
/// real colours (GBC, GBA) additionally fill [`Frame::rgb`] with RGB888 data;
/// frontends should prefer `rgb` when it is present.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    pub width: u16,
    pub height: u16,
    /// Exactly `width * height` bytes, each `0..=3`.
    pub shades: Vec<u8>,
    /// Optional `width * height * 3` bytes of RGB888 pixel data (present for
    /// colour consoles). When `Some`, its length is exactly
    /// `width * height * 3`.
    pub rgb: Option<Vec<u8>>,
}

impl Frame {
    pub fn new(width: u16, height: u16) -> Self {
        Frame {
            width,
            height,
            shades: vec![0; width as usize * height as usize],
            rgb: None,
        }
    }

    /// Return the RGB888 data if present, otherwise convert `shades` through a
    /// 4-colour palette into RGB888. The palette entries are
    /// `[r, g, b, a]` quads (`alpha` is ignored).
    pub fn to_rgb(&self, palette: &[[u8; 4]]) -> Vec<u8> {
        if let Some(rgb) = &self.rgb {
            return rgb.clone();
        }
        let mut out = vec![0u8; self.shades.len() * 3];
        for (i, s) in self.shades.iter().enumerate() {
            let c = palette[*s as usize & 3];
            out[i * 3] = c[0];
            out[i * 3 + 1] = c[1];
            out[i * 3 + 2] = c[2];
        }
        out
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