//! The transfer engine ("PPF"): display transfers and texture copies.
//!
//! Both work on plain byte slices. The caller snapshots the input, so an
//! input and an output that overlap in memory behave as a copy through a
//! buffer; what the hardware does with overlapping buffers is not known.
//!
//! Register semantics follow 3dbrew, "GPU/External Registers", section
//! "Transfer Engine". The tiling modes (flags bits 1, 5 and 16) rearrange
//! pixels in 8x8 or 32x32 blocks and are not modelled: data is taken as rows.

/// A framebuffer colour format, by its three-bit register value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Rgba8,
    Rgb8,
    Rgb565,
    Rgb5a1,
    Rgba4,
}

fn expand(value: u32, bits: u32) -> u8 {
    (value << (8 - bits) | value >> (2 * bits - 8)) as u8
}

impl Format {
    /// Values 5 to 7 are not formats.
    pub fn from_bits(bits: u32) -> Option<Self> {
        Some(match bits & 7 {
            0 => Format::Rgba8,
            1 => Format::Rgb8,
            2 => Format::Rgb565,
            3 => Format::Rgb5a1,
            4 => Format::Rgba4,
            _ => return None,
        })
    }

    pub fn bytes(self) -> usize {
        match self {
            Format::Rgba8 => 4,
            Format::Rgb8 => 3,
            _ => 2,
        }
    }

    /// One pixel as red, green, blue, alpha. Narrow channels widen by
    /// repeating their top bits; a format without alpha is opaque.
    pub fn decode(self, p: &[u8]) -> [u8; 4] {
        let half = || u16::from_le_bytes([p[0], p[1]]) as u32;
        match self {
            // Alpha first in memory, then blue, green, red.
            Format::Rgba8 => [p[3], p[2], p[1], p[0]],
            Format::Rgb8 => [p[2], p[1], p[0], 0xFF],
            Format::Rgb565 => {
                let v = half();
                [
                    expand(v >> 11, 5),
                    expand(v >> 5 & 0x3F, 6),
                    expand(v & 0x1F, 5),
                    0xFF,
                ]
            }
            Format::Rgb5a1 => {
                let v = half();
                [
                    expand(v >> 11, 5),
                    expand(v >> 6 & 0x1F, 5),
                    expand(v >> 1 & 0x1F, 5),
                    if v & 1 != 0 { 0xFF } else { 0 },
                ]
            }
            Format::Rgba4 => {
                let v = half();
                [
                    expand(v >> 12, 4),
                    expand(v >> 8 & 0xF, 4),
                    expand(v >> 4 & 0xF, 4),
                    expand(v & 0xF, 4),
                ]
            }
        }
    }

    /// Store one pixel. Channels narrow by dropping their low bits; whether
    /// the hardware rounds instead is not known.
    pub fn encode(self, [r, g, b, a]: [u8; 4], out: &mut [u8]) {
        let (r, g, b, a) = (r as u16, g as u16, b as u16, a as u16);
        let half = match self {
            Format::Rgba8 => {
                out[..4].copy_from_slice(&[a as u8, b as u8, g as u8, r as u8]);
                return;
            }
            Format::Rgb8 => {
                out[..3].copy_from_slice(&[b as u8, g as u8, r as u8]);
                return;
            }
            Format::Rgb565 => (r >> 3) << 11 | (g >> 2) << 5 | b >> 3,
            Format::Rgb5a1 => (r >> 3) << 11 | (g >> 3) << 6 | (b >> 3) << 1 | a >> 7,
            Format::Rgba4 => (r >> 4) << 12 | (g >> 4) << 8 | (b >> 4) << 4 | a >> 4,
        };
        out[..2].copy_from_slice(&half.to_le_bytes());
    }
}

/// A display transfer: a format conversion of a picture, optionally flipped
/// and halved in width or in both directions with a box filter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DisplayTransfer {
    pub input_width: usize,
    pub input_height: usize,
    pub output_width: usize,
    pub output_height: usize,
    pub input_format: Format,
    pub output_format: Format,
    pub flip: bool,
    pub halve_width: bool,
    pub halve_height: bool,
}

impl DisplayTransfer {
    /// Decode the dimension registers (width in bits 0-15, height above) and
    /// the flags. `None` for a colour format or a scale that does not exist.
    pub fn from_registers(output_dim: u32, input_dim: u32, flags: u32) -> Option<Self> {
        let scale = flags >> 24 & 3;
        if scale == 3 {
            return None;
        }
        Some(DisplayTransfer {
            input_width: (input_dim & 0xFFFF) as usize,
            input_height: (input_dim >> 16) as usize,
            output_width: (output_dim & 0xFFFF) as usize,
            output_height: (output_dim >> 16) as usize,
            input_format: Format::from_bits(flags >> 8)?,
            output_format: Format::from_bits(flags >> 12)?,
            flip: flags & 1 != 0,
            halve_width: scale != 0,
            halve_height: scale == 2,
        })
    }

    /// The size of the picture written, after scaling.
    fn written(&self) -> (usize, usize) {
        (
            self.output_width >> self.halve_width as u32,
            self.output_height >> self.halve_height as u32,
        )
    }

    pub fn input_len(&self) -> usize {
        self.input_width * self.input_height * self.input_format.bytes()
    }

    pub fn output_len(&self) -> usize {
        let (w, h) = self.written();
        w * h * self.output_format.bytes()
    }

    /// Convert `input` into `output`. Pixels whose source or destination
    /// falls outside the slices are skipped.
    pub fn run(&self, input: &[u8], output: &mut [u8]) {
        let (width, height) = self.written();
        let (src_bytes, dst_bytes) = (self.input_format.bytes(), self.output_format.bytes());
        let (dx, dy) = (self.halve_width as usize, self.halve_height as usize);
        for y in 0..height {
            let out_y = if self.flip { height - 1 - y } else { y };
            for x in 0..width {
                // The box: one pixel, two side by side, or two by two.
                let mut sum = [0u32; 4];
                let mut count = 0;
                for by in 0..=dy {
                    for bx in 0..=dx {
                        let (sx, sy) = ((x << dx) + bx, (y << dy) + by);
                        let at = (sy * self.input_width + sx) * src_bytes;
                        if sx >= self.input_width {
                            continue;
                        }
                        if let Some(p) = input.get(at..at + src_bytes) {
                            let c = self.input_format.decode(p);
                            for (s, c) in sum.iter_mut().zip(c) {
                                *s += c as u32;
                            }
                            count += 1;
                        }
                    }
                }
                if count == 0 {
                    continue;
                }
                let colour = sum.map(|s| (s / count) as u8);
                let at = (out_y * width + x) * dst_bytes;
                if let Some(p) = output.get_mut(at..at + dst_bytes) {
                    self.output_format.encode(colour, p);
                }
            }
        }
    }
}

/// A texture copy: bytes moved from lines of one width and gap to lines of
/// another. All quantities are in bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextureCopy {
    pub size: usize,
    pub input_width: usize,
    pub input_gap: usize,
    pub output_width: usize,
    pub output_gap: usize,
}

impl TextureCopy {
    /// Decode the size register and the two line registers (width in bits
    /// 0-15, gap above, both in units of 16 bytes). The size rounds down to
    /// 16 bytes. A line register with no gap means one unbroken run.
    pub fn from_registers(size: u32, input: u32, output: u32) -> Self {
        let size = (size & !15) as usize;
        let line = |reg: u32| {
            let (width, gap) = ((reg & 0xFFFF) as usize * 16, (reg >> 16) as usize * 16);
            if gap == 0 {
                (size, 0)
            } else {
                (width, gap)
            }
        };
        let ((input_width, input_gap), (output_width, output_gap)) = (line(input), line(output));
        TextureCopy {
            size,
            input_width,
            input_gap,
            output_width,
            output_gap,
        }
    }

    fn span(&self, width: usize, gap: usize) -> usize {
        if self.size == 0 || width == 0 {
            return 0;
        }
        let lines = self.size.div_ceil(width);
        self.size + (lines - 1) * gap
    }

    /// Bytes from the input address to the end of the last byte read.
    pub fn input_len(&self) -> usize {
        self.span(self.input_width, self.input_gap)
    }

    /// Bytes from the output address to the end of the last byte written.
    pub fn output_len(&self) -> usize {
        self.span(self.output_width, self.output_gap)
    }

    /// Copy. A gap with no line width is not a valid setting and copies
    /// nothing; bytes outside the slices are skipped.
    pub fn run(&self, input: &[u8], output: &mut [u8]) {
        if self.input_width == 0 || self.output_width == 0 {
            return;
        }
        let (mut src, mut dst) = (0, 0);
        let (mut in_left, mut out_left) = (self.input_width, self.output_width);
        let mut left = self.size;
        while left > 0 {
            let n = left.min(in_left).min(out_left);
            if let (Some(from), Some(to)) = (input.get(src..src + n), output.get_mut(dst..dst + n))
            {
                to.copy_from_slice(from);
            }
            src += n;
            dst += n;
            left -= n;
            in_left -= n;
            out_left -= n;
            if in_left == 0 {
                src += self.input_gap;
                in_left = self.input_width;
            }
            if out_left == 0 {
                dst += self.output_gap;
                out_left = self.output_width;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_format_round_trips_its_own_pixels() {
        for (format, pixel) in [
            (Format::Rgba8, &[0x12, 0x34, 0x56, 0x78][..]),
            (Format::Rgb8, &[0x12, 0x34, 0x56]),
            (Format::Rgb565, &[0xCD, 0xAB]),
            (Format::Rgb5a1, &[0xCD, 0xAB]),
            (Format::Rgba4, &[0xCD, 0xAB]),
        ] {
            let mut out = [0u8; 4];
            format.encode(format.decode(pixel), &mut out);
            assert_eq!(&out[..format.bytes()], pixel, "{format:?}");
        }
    }

    #[test]
    fn channels_sit_where_the_display_controller_reads_them() {
        assert_eq!(Format::Rgba8.decode(&[0xAA, 3, 2, 1]), [1, 2, 3, 0xAA]);
        assert_eq!(Format::Rgb8.decode(&[3, 2, 1]), [1, 2, 3, 0xFF]);
        assert_eq!(Format::Rgb565.decode(&[0x00, 0xF8]), [0xFF, 0, 0, 0xFF]);
        assert_eq!(Format::Rgb5a1.decode(&[0x3F, 0x00]), [0, 0, 0xFF, 0xFF]);
        assert_eq!(Format::Rgba4.decode(&[0xF0, 0x00]), [0, 0, 0xFF, 0]);
    }

    #[test]
    fn a_display_transfer_converts_and_flips() {
        // Two rows of two RGB565 pixels to RGB8, flipped.
        let t = DisplayTransfer::from_registers(2 << 16 | 2, 2 << 16 | 2, 1 << 12 | 2 << 8 | 1)
            .unwrap();
        assert_eq!((t.input_len(), t.output_len()), (8, 12));
        let input = [0x00, 0xF8, 0xE0, 0x07, 0x1F, 0x00, 0xFF, 0xFF];
        let mut output = [0u8; 12];
        t.run(&input, &mut output);
        assert_eq!(
            output,
            [0xFF, 0, 0, 0xFF, 0xFF, 0xFF, 0, 0, 0xFF, 0, 0xFF, 0]
        );
    }

    #[test]
    fn downscaling_averages_a_box() {
        let flags = 2 << 24;
        let t = DisplayTransfer::from_registers(2 << 16 | 2, 2 << 16 | 2, flags).unwrap();
        assert_eq!(t.output_len(), 4);
        let mut input = [0u8; 16];
        for (i, red) in [10u8, 20, 30, 40].into_iter().enumerate() {
            input[i * 4 + 3] = red;
        }
        let mut output = [0u8; 4];
        t.run(&input, &mut output);
        assert_eq!(output, [0, 0, 0, 25]);

        let t = DisplayTransfer::from_registers(2 << 16 | 2, 2 << 16 | 2, 1 << 24).unwrap();
        let mut output = [0u8; 8];
        t.run(&input, &mut output);
        assert_eq!((output[3], output[7]), (15, 35));
    }

    #[test]
    fn unknown_formats_and_scales_are_refused() {
        assert_eq!(DisplayTransfer::from_registers(0, 0, 5 << 8), None);
        assert_eq!(DisplayTransfer::from_registers(0, 0, 3 << 24), None);
    }

    #[test]
    fn a_texture_copy_without_gaps_is_one_run() {
        let t = TextureCopy::from_registers(37, 0, 0);
        assert_eq!((t.size, t.input_len(), t.output_len()), (32, 32, 32));
        let input: Vec<u8> = (0..40).collect();
        let mut output = [0xEEu8; 40];
        t.run(&input, &mut output);
        assert_eq!(output[..32], input[..32]);
        assert_eq!(output[32], 0xEE);
    }

    #[test]
    fn a_texture_copy_skips_the_gaps_of_both_sides() {
        // Input lines of 16 bytes 16 apart, packed into the output.
        let t = TextureCopy::from_registers(32, 1 << 16 | 1, 0);
        assert_eq!((t.input_len(), t.output_len()), (48, 32));
        let input: Vec<u8> = (0..48).collect();
        let mut output = [0u8; 32];
        t.run(&input, &mut output);
        assert_eq!(output[..16], input[..16]);
        assert_eq!(output[16..], input[32..48]);

        // And the other way round.
        let t = TextureCopy::from_registers(32, 0, 1 << 16 | 1);
        let mut output = [0xEEu8; 48];
        t.run(&input, &mut output);
        assert_eq!(output[..16], input[..16]);
        assert_eq!(output[16..32], [0xEE; 16]);
        assert_eq!(output[32..], input[16..32]);
    }
}
