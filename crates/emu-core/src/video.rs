//! Framebuffer representation.

/// The classic green-tinted DMG palette (`[r, g, b, a]` per shade, lightest
/// first), the default frontends use for monochrome frames.
pub const DMG_PALETTE: [[u8; 4]; 4] = [
    [0xE0, 0xF8, 0xD0, 0xFF],
    [0x88, 0xC0, 0x70, 0xFF],
    [0x34, 0x68, 0x56, 0xFF],
    [0x08, 0x18, 0x20, 0xFF],
];

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

    /// Write the frame as RGBA8888 into `out`, which must hold exactly
    /// `width * height * 4` bytes. Uses the colour buffer when present,
    /// otherwise maps the shades through `palette`.
    pub fn write_rgba(&self, palette: &[[u8; 4]; 4], out: &mut [u8]) {
        let pixels = self.width as usize * self.height as usize;
        assert_eq!(out.len(), pixels * 4, "rgba buffer size");
        match &self.rgb {
            Some(rgb) => {
                for (dst, src) in out.chunks_exact_mut(4).zip(rgb.chunks_exact(3)) {
                    dst[..3].copy_from_slice(src);
                    dst[3] = 0xFF;
                }
            }
            None => {
                for (dst, &s) in out.chunks_exact_mut(4).zip(&self.shades) {
                    dst.copy_from_slice(&palette[s as usize & 3]);
                }
            }
        }
    }

    /// The frame as a new RGBA8888 buffer (see [`Frame::write_rgba`]).
    pub fn to_rgba(&self, palette: &[[u8; 4]; 4]) -> Vec<u8> {
        let mut out = vec![0u8; self.width as usize * self.height as usize * 4];
        self.write_rgba(palette, &mut out);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_rgba_maps_shades_through_the_palette() {
        let mut f = Frame::new(2, 1);
        f.shades = vec![0, 3];
        let out = f.to_rgba(&DMG_PALETTE);
        assert_eq!(&out[..4], &DMG_PALETTE[0]);
        assert_eq!(&out[4..], &DMG_PALETTE[3]);
    }

    #[test]
    fn write_rgba_prefers_the_colour_buffer() {
        let mut f = Frame::new(1, 2);
        f.rgb = Some(vec![1, 2, 3, 4, 5, 6]);
        let out = f.to_rgba(&DMG_PALETTE);
        assert_eq!(out, [1, 2, 3, 0xFF, 4, 5, 6, 0xFF]);
    }

    #[test]
    #[should_panic(expected = "rgba buffer size")]
    fn write_rgba_rejects_a_wrong_sized_buffer() {
        let f = Frame::new(2, 2);
        let mut out = [0u8; 8];
        f.write_rgba(&DMG_PALETTE, &mut out);
    }
}
