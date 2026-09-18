//! The two display controllers (3dbrew, "GPU/External Registers").
//!
//! PDC0 at 0x10400400 drives the top screen and PDC1 at 0x10400500 the bottom
//! one. A framebuffer is stored a column at a time, each column from the
//! bottom pixel up, so with the console held normally the first pixel is the
//! bottom-left one. Colour components sit in reverse byte order: `RGB8` is
//! blue, green, red in memory.

use crate::bus::PhysMem;
use emu_core::{Frame, Screen};

const FRAMEBUFFER_A: [usize; 2] = [0x68 / 4, 0x6C / 4];
const FORMAT: usize = 0x70 / 4;
const CONTROL: usize = 0x74 / 4;
const SELECT: usize = 0x78 / 4;
const STRIDE: usize = 0x90 / 4;

/// One display controller: 0x100 bytes of registers.
pub struct Pdc {
    screen: Screen,
    regs: [u32; 0x40],
    /// The LCD fill colour register: bits 23:0 are `0x00BBGGRR`, bit 24 makes
    /// the panel show the colour instead of the framebuffer.
    pub fill: u32,
}

impl Pdc {
    pub fn new(screen: Screen) -> Self {
        Pdc {
            screen,
            regs: [0; 0x40],
            fill: 0,
        }
    }

    pub fn read32(&self, offset: u32) -> u32 {
        self.regs[(offset as usize & 0xFF) >> 2]
    }

    pub fn write32(&mut self, offset: u32, value: u32) {
        self.regs[(offset as usize & 0xFF) >> 2] = value;
    }

    /// Point the controller at two `RGB8` framebuffers and enable it, as a
    /// chainloader leaves it for the payload it starts.
    pub fn init_rgb8(&mut self, first: u32, second: u32) {
        self.regs[FRAMEBUFFER_A[0]] = first;
        self.regs[FRAMEBUFFER_A[1]] = second;
        self.regs[FORMAT] = 0x0008_0301;
        self.regs[STRIDE] = self.screen.height as u32 * 3;
        self.regs[SELECT] = 0;
        self.regs[CONTROL] = 1;
    }

    /// What the panel shows.
    pub fn frame(&self, mem: &PhysMem) -> Frame {
        let (width, height) = (self.screen.width as usize, self.screen.height as usize);
        let mut frame = Frame::new(self.screen.width, self.screen.height);
        let mut rgb = vec![0u8; width * height * 3];

        if self.fill & 1 << 24 != 0 {
            let [r, g, b, _] = self.fill.to_le_bytes();
            rgb.as_chunks_mut::<3>().0.fill([r, g, b]);
        } else if self.regs[CONTROL] & 1 != 0 {
            let base = self.regs[FRAMEBUFFER_A[(self.regs[SELECT] & 1) as usize]];
            let format = self.regs[FORMAT] & 7;
            let bytes = match format {
                1 => 3,
                2..=4 => 2,
                _ => 4,
            };
            let stride = self.regs[STRIDE] as usize;
            let needed = stride * (width - 1) + height * bytes;
            if let Some(source) = mem.slice(base, needed) {
                for x in 0..width {
                    for y in 0..height {
                        let at = x * stride + (height - 1 - y) * bytes;
                        let out = (y * width + x) * 3;
                        rgb[out..out + 3].copy_from_slice(&decode(format, &source[at..]));
                    }
                }
            }
        }
        frame.rgb = Some(rgb);
        frame
    }
}

/// Expand an n-bit component to eight bits by replicating its high bits.
fn expand(value: u32, bits: u32) -> u8 {
    (value << (8 - bits) | value >> (2 * bits - 8)) as u8
}

/// One pixel as red, green, blue.
fn decode(format: u32, p: &[u8]) -> [u8; 3] {
    let half = || u16::from_le_bytes([p[0], p[1]]) as u32;
    match format {
        1 => [p[2], p[1], p[0]],
        2 => {
            let v = half();
            [
                expand(v >> 11, 5),
                expand(v >> 5 & 0x3F, 6),
                expand(v & 0x1F, 5),
            ]
        }
        3 => {
            let v = half();
            [
                expand(v >> 11, 5),
                expand(v >> 6 & 0x1F, 5),
                expand(v >> 1 & 0x1F, 5),
            ]
        }
        4 => {
            let v = half();
            [
                expand(v >> 12, 4),
                expand(v >> 8 & 0xF, 4),
                expand(v >> 4 & 0xF, 4),
            ]
        }
        // RGBA8: alpha first in memory, then blue, green, red.
        _ => [p[3], p[2], p[1]],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOP: Screen = Screen::new(400, 240);

    #[test]
    fn a_disabled_controller_shows_black_and_the_fill_colour_overrides() {
        let mem = PhysMem::new();
        let mut pdc = Pdc::new(TOP);
        assert!(pdc.frame(&mem).rgb.unwrap().iter().all(|b| *b == 0));
        pdc.fill = 1 << 24 | 0x0030_2010;
        assert_eq!(pdc.frame(&mem).rgb.unwrap()[..3], [0x10, 0x20, 0x30]);
    }

    #[test]
    fn the_first_pixel_in_memory_is_the_bottom_left_one() {
        let mut mem = PhysMem::new();
        let mut pdc = Pdc::new(TOP);
        pdc.init_rgb8(0x1830_0000, 0x1840_0000);
        // Blue, green, red in memory.
        mem.slice_mut(0x1830_0000, 3)
            .unwrap()
            .copy_from_slice(&[3, 2, 1]);
        // The second column's top pixel.
        let top_of_column_1 = 0x1830_0000 + (240 + 239) * 3;
        mem.slice_mut(top_of_column_1, 3)
            .unwrap()
            .copy_from_slice(&[6, 5, 4]);
        let rgb = pdc.frame(&mem).rgb.unwrap();
        let bottom_left = 239 * 400 * 3;
        assert_eq!(rgb[bottom_left..bottom_left + 3], [1, 2, 3]);
        assert_eq!(rgb[3..6], [4, 5, 6]);
    }

    #[test]
    fn the_select_register_picks_the_second_framebuffer() {
        let mut mem = PhysMem::new();
        let mut pdc = Pdc::new(TOP);
        pdc.init_rgb8(0x1830_0000, 0x1840_0000);
        mem.slice_mut(0x1840_0000, 3)
            .unwrap()
            .copy_from_slice(&[9, 9, 9]);
        pdc.write32(0x78, 1);
        let rgb = pdc.frame(&mem).rgb.unwrap();
        assert_eq!(rgb[239 * 400 * 3], 9);
    }

    #[test]
    fn sixteen_bit_formats_expand_to_full_range() {
        assert_eq!(decode(2, &0xFFFFu16.to_le_bytes()), [255, 255, 255]);
        assert_eq!(decode(2, &0xF800u16.to_le_bytes()), [255, 0, 0]);
        assert_eq!(decode(3, &0x07C0u16.to_le_bytes()), [0, 255, 0]);
        assert_eq!(decode(4, &0x00F0u16.to_le_bytes()), [0, 0, 255]);
        assert_eq!(decode(0, &[0xFF, 3, 2, 1]), [1, 2, 3]);
    }

    #[test]
    fn a_framebuffer_outside_ram_shows_black() {
        let mem = PhysMem::new();
        let mut pdc = Pdc::new(TOP);
        pdc.init_rgb8(0x1000_0000, 0);
        assert!(pdc.frame(&mem).rgb.unwrap().iter().all(|b| *b == 0));
    }
}
