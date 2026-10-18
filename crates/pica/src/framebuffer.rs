//! The output merger: alpha, stencil and depth tests, blending or a logic
//! operation, write masks, and the colour and depth buffers in memory
//! (3dbrew, "GPU/Internal Registers", framebuffer registers).
//!
//! Both buffers are stored in 8x8 tiles, each in Z order, the tiles in rows;
//! the picture is upside down, row 0 in memory being the top of the window
//! (y up), as Citra has it.

use crate::memory::Memory;
use crate::regs as reg;
use crate::transfer::Format;

/// Where pixel `(x, y)` of a `width`-wide picture of 8x8 tiles starts, in
/// pixels (memory row `y`, counted from the start of the buffer).
pub fn tiled_offset(x: u32, y: u32, width: u32) -> u32 {
    let morton = (0..3).fold(0, |m, bit| {
        m | (x >> bit & 1) << (2 * bit) | (y >> bit & 1) << (2 * bit + 1)
    });
    ((y / 8) * (width / 8) + x / 8) * 64 + morton
}

/// A comparison of the alpha, stencil and depth tests: never, always, ==,
/// !=, <, <=, >, >=.
fn compare(function: u32, a: u32, b: u32) -> bool {
    match function & 7 {
        0 => false,
        1 => true,
        2 => a == b,
        3 => a != b,
        4 => a < b,
        5 => a <= b,
        6 => a > b,
        _ => a >= b,
    }
}

/// A depth buffer format: 16 bits, 24 bits, or 24 bits and stencil.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DepthFormat {
    D16,
    D24,
    D24S8,
}

impl DepthFormat {
    pub fn bytes(self) -> usize {
        match self {
            DepthFormat::D16 => 2,
            DepthFormat::D24 => 3,
            DepthFormat::D24S8 => 4,
        }
    }
}

/// What the output merger reads from the registers.
#[derive(Clone, Debug)]
pub struct Framebuffer {
    width: u32,
    height: u32,
    colour_addr: u32,
    colour_format: Format,
    depth_addr: u32,
    depth_format: DepthFormat,
    alpha_test: u32,
    stencil_test: u32,
    stencil_op: u32,
    depth_mask: u32,
    blend: bool,
    blend_func: u32,
    logic_op: u32,
    blend_colour: [u8; 4],
    colour_read: bool,
    colour_write: bool,
    depth_read: bool,
    depth_write: bool,
    stencil_read: bool,
    stencil_write: bool,
}

impl Framebuffer {
    pub fn new(regs: &[u32]) -> Self {
        let r = |id: u32| regs[id as usize];
        let dim = r(reg::FRAMEBUFFER_DIM);
        let colour_format =
            Format::from_bits(r(reg::COLORBUFFER_FORMAT) >> 16).unwrap_or(Format::Rgba8);
        let depth_format = match r(reg::DEPTHBUFFER_FORMAT) & 3 {
            0 | 1 => DepthFormat::D16,
            2 => DepthFormat::D24,
            _ => DepthFormat::D24S8,
        };
        Framebuffer {
            width: dim & 0x7FF,
            height: (dim >> 12 & 0x3FF) + 1,
            colour_addr: (r(reg::COLORBUFFER_LOC) & 0x0FFF_FFFF) << 3,
            colour_format,
            depth_addr: (r(reg::DEPTHBUFFER_LOC) & 0x0FFF_FFFF) << 3,
            depth_format,
            alpha_test: r(reg::FRAGOP_ALPHA_TEST),
            stencil_test: r(reg::STENCIL_TEST),
            stencil_op: r(reg::STENCIL_OP),
            depth_mask: r(reg::DEPTH_COLOR_MASK),
            blend: r(reg::COLOR_OPERATION) >> 8 & 1 != 0,
            blend_func: r(reg::BLEND_FUNC),
            logic_op: r(reg::LOGIC_OP) & 0xF,
            blend_colour: r(reg::BLEND_COLOR).to_le_bytes(),
            colour_read: r(reg::COLORBUFFER_READ) & 0xF != 0,
            colour_write: r(reg::COLORBUFFER_WRITE) & 0xF != 0,
            depth_read: r(reg::DEPTHBUFFER_READ) & 2 != 0,
            depth_write: r(reg::DEPTHBUFFER_WRITE) & 2 != 0,
            stencil_read: r(reg::DEPTHBUFFER_READ) & 1 != 0,
            stencil_write: r(reg::DEPTHBUFFER_WRITE) & 1 != 0,
        }
    }

    /// The memory row of window row `y`, and whether the pixel exists.
    fn locate(&self, x: u32, y: u32) -> Option<u32> {
        (x < self.width && y < self.height)
            .then(|| tiled_offset(x, self.height - 1 - y, self.width))
    }

    pub fn read_colour(&self, mem: &dyn Memory, x: u32, y: u32) -> Option<[u8; 4]> {
        let bytes = self.colour_format.bytes();
        let at = self.colour_addr + self.locate(x, y)? * bytes as u32;
        mem.read(at, bytes).map(|p| self.colour_format.decode(p))
    }

    /// Depth and stencil at a pixel.
    pub fn read_depth(&self, mem: &dyn Memory, x: u32, y: u32) -> Option<(u32, u8)> {
        let bytes = self.depth_format.bytes();
        let at = self.depth_addr + self.locate(x, y)? * bytes as u32;
        let p = mem.read(at, bytes)?;
        Some(match self.depth_format {
            DepthFormat::D16 => (u16::from_le_bytes([p[0], p[1]]) as u32, 0),
            DepthFormat::D24 => (u32::from_le_bytes([p[0], p[1], p[2], 0]), 0),
            DepthFormat::D24S8 => (u32::from_le_bytes([p[0], p[1], p[2], 0]), p[3]),
        })
    }

    /// The fragment's depth (24 bits) in this buffer's precision.
    fn depth_value(&self, depth: u32) -> u32 {
        match self.depth_format {
            DepthFormat::D16 => depth >> 8,
            _ => depth,
        }
    }

    fn write_depth(
        &self,
        mem: &mut dyn Memory,
        x: u32,
        y: u32,
        depth: Option<u32>,
        stencil: Option<u8>,
    ) {
        let bytes = self.depth_format.bytes();
        let Some(offset) = self.locate(x, y) else {
            return;
        };
        let Some(p) = mem.write(self.depth_addr + offset * bytes as u32, bytes) else {
            return;
        };
        if let Some(depth) = depth {
            let le = depth.to_le_bytes();
            let n = if self.depth_format == DepthFormat::D16 {
                2
            } else {
                3
            };
            p[..n].copy_from_slice(&le[..n]);
        }
        if let (Some(stencil), DepthFormat::D24S8) = (stencil, self.depth_format) {
            p[3] = stencil;
        }
    }

    /// The stencil value an operation leaves.
    fn stencil_result(&self, op: u32, old: u8) -> u8 {
        let reference = (self.stencil_test >> 16) as u8;
        match op & 7 {
            0 => old,
            1 => 0,
            2 => reference,
            3 => old.saturating_add(1),
            4 => old.saturating_sub(1),
            5 => !old,
            6 => old.wrapping_add(1),
            _ => old.wrapping_sub(1),
        }
    }

    /// Put a fragment of colour `colour` and 24-bit depth `depth` through the
    /// tests and into the buffers. Returns whether it passed them.
    pub fn fragment(
        &self,
        mem: &mut dyn Memory,
        x: u32,
        y: u32,
        depth: u32,
        colour: [u8; 4],
    ) -> bool {
        if self.locate(x, y).is_none() {
            return false;
        }
        if self.alpha_test & 1 != 0
            && !compare(
                self.alpha_test >> 4,
                colour[3] as u32,
                self.alpha_test >> 8 & 0xFF,
            )
        {
            return false;
        }
        let (stored_depth, stencil) = self.read_depth(mem, x, y).unwrap_or((0, 0));
        let stencil_on = self.stencil_test & 1 != 0 && self.depth_format == DepthFormat::D24S8;
        let write_stencil = |fb: &Self, mem: &mut dyn Memory, op: u32| {
            if fb.stencil_write {
                let mask = (fb.stencil_test >> 8) as u8;
                let new = fb.stencil_result(op, stencil);
                fb.write_depth(mem, x, y, None, Some(stencil & !mask | new & mask));
            }
        };
        if stencil_on {
            let mask = self.stencil_test >> 24;
            let reference = self.stencil_test >> 16 & 0xFF;
            let value = if self.stencil_read { stencil as u32 } else { 0 };
            if !compare(self.stencil_test >> 4, reference & mask, value & mask) {
                write_stencil(self, mem, self.stencil_op);
                return false;
            }
        }
        let depth = self.depth_value(depth);
        let depth_on = self.depth_mask & 1 != 0 && self.depth_read;
        if depth_on && !compare(self.depth_mask >> 4, depth, stored_depth) {
            if stencil_on {
                write_stencil(self, mem, self.stencil_op >> 4);
            }
            return false;
        }
        if stencil_on {
            write_stencil(self, mem, self.stencil_op >> 8);
        }
        if self.depth_mask & 1 << 12 != 0 && self.depth_write {
            self.write_depth(mem, x, y, Some(depth), None);
        }
        let channels = self.depth_mask >> 8 & 0xF;
        if !self.colour_write || channels == 0 {
            return true;
        }
        let dest = if self.colour_read {
            self.read_colour(mem, x, y).unwrap_or([0; 4])
        } else {
            [0; 4]
        };
        let result = if self.blend {
            self.blend(colour, dest)
        } else {
            self.logic(colour, dest)
        };
        let out: [u8; 4] = std::array::from_fn(|i| {
            if channels >> i & 1 != 0 {
                result[i]
            } else {
                dest[i]
            }
        });
        let bytes = self.colour_format.bytes();
        let at = self.colour_addr + self.locate(x, y).unwrap() * bytes as u32;
        if let Some(p) = mem.write(at, bytes) {
            self.colour_format.encode(out, p);
        }
        true
    }

    /// A blend factor for each channel.
    fn factor(&self, function: u32, src: [u8; 4], dst: [u8; 4]) -> [u32; 4] {
        let c = self.blend_colour;
        let all = |v: u8| [v as u32; 4];
        let each = |v: [u8; 4]| v.map(|v| v as u32);
        let inv = |v: [u32; 4]| v.map(|v| 255 - v);
        match function & 0xF {
            0 => [0; 4],
            1 => [255; 4],
            2 => each(src),
            3 => inv(each(src)),
            4 => each(dst),
            5 => inv(each(dst)),
            6 => all(src[3]),
            7 => inv(all(src[3])),
            8 => all(dst[3]),
            9 => inv(all(dst[3])),
            10 => each(c),
            11 => inv(each(c)),
            12 => all(c[3]),
            13 => inv(all(c[3])),
            14 => {
                let f = src[3].min(255 - dst[3]) as u32;
                [f, f, f, 255]
            }
            _ => [255; 4],
        }
    }

    /// Blending: the two products are added, subtracted or compared per
    /// equation, divided by 255 (truncating) and clamped.
    fn blend(&self, src: [u8; 4], dst: [u8; 4]) -> [u8; 4] {
        let f = self.blend_func;
        let src_rgb = self.factor(f >> 16, src, dst);
        let dst_rgb = self.factor(f >> 20, src, dst);
        let src_a = self.factor(f >> 24, src, dst);
        let dst_a = self.factor(f >> 28, src, dst);
        std::array::from_fn(|i| {
            let (sf, df, eq) = if i < 3 {
                (src_rgb[i], dst_rgb[i], f & 7)
            } else {
                (src_a[3], dst_a[3], f >> 8 & 7)
            };
            let (s, d) = (src[i] as i32 * sf as i32, dst[i] as i32 * df as i32);
            let value = match eq {
                1 => (s - d) / 255,
                2 => (d - s) / 255,
                3 => src[i].min(dst[i]) as i32,
                4 => src[i].max(dst[i]) as i32,
                _ => (s + d) / 255,
            };
            value.clamp(0, 255) as u8
        })
    }

    fn logic(&self, s: [u8; 4], d: [u8; 4]) -> [u8; 4] {
        std::array::from_fn(|i| {
            let (s, d) = (s[i], d[i]);
            match self.logic_op {
                0 => 0,
                1 => s & d,
                2 => s & !d,
                3 => s,
                4 => 0xFF,
                5 => !s,
                6 => d,
                7 => !d,
                8 => !(s & d),
                9 => s | d,
                10 => !(s | d),
                11 => s ^ d,
                12 => !(s ^ d),
                13 => !s & d,
                14 => s | !d,
                _ => !s | d,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::VecMemory;

    const COLOUR: u32 = 0x1800_0000;
    const DEPTH: u32 = 0x1800_1000;

    /// An 8x8 RGBA8 buffer with a D24S8 depth buffer, every test off and
    /// every write on.
    fn regs() -> Vec<u32> {
        let mut regs = vec![0u32; reg::COUNT];
        regs[reg::FRAMEBUFFER_DIM as usize] = 7 << 12 | 8;
        regs[reg::COLORBUFFER_LOC as usize] = COLOUR >> 3;
        regs[reg::DEPTHBUFFER_LOC as usize] = DEPTH >> 3;
        regs[reg::DEPTHBUFFER_FORMAT as usize] = 3;
        regs[reg::DEPTH_COLOR_MASK as usize] = 0x1F << 8 | 1 << 4;
        regs[reg::COLOR_OPERATION as usize] = 1 << 8;
        regs[reg::BLEND_FUNC as usize] = 1 << 16 | 1 << 24;
        for id in [reg::COLORBUFFER_READ, reg::COLORBUFFER_WRITE] {
            regs[id as usize] = 0xF;
        }
        for id in [reg::DEPTHBUFFER_READ, reg::DEPTHBUFFER_WRITE] {
            regs[id as usize] = 3;
        }
        regs
    }

    #[test]
    fn tiles_are_z_ordered_and_the_picture_upside_down() {
        assert_eq!(tiled_offset(1, 0, 16), 1);
        assert_eq!(tiled_offset(0, 1, 16), 2);
        assert_eq!(tiled_offset(7, 7, 16), 63);
        assert_eq!(tiled_offset(8, 0, 16), 64);
        assert_eq!(tiled_offset(0, 8, 16), 128);
        let mut mem = VecMemory::new(COLOUR, 0x2000);
        let fb = Framebuffer::new(&regs());
        assert!(fb.fragment(&mut mem, 1, 7, 0, [1, 2, 3, 4]));
        // Window row 7 is memory row 0: pixel 1 of the first tile.
        assert_eq!(mem.bytes[4..8], [4, 3, 2, 1]);
        assert_eq!(fb.read_colour(&mem, 1, 7), Some([1, 2, 3, 4]));
    }

    #[test]
    fn the_depth_test_keeps_the_nearer_fragment_and_writes_depth() {
        let mut regs = regs();
        regs[reg::DEPTH_COLOR_MASK as usize] |= 1 | 4 << 4; // test <
        let fb = Framebuffer::new(&regs);
        let mut mem = VecMemory::new(COLOUR, 0x2000);
        fb.write_depth(&mut mem, 0, 0, Some(0xFF_FFFF), Some(0));
        assert!(fb.fragment(&mut mem, 0, 0, 0x80_0000, [9; 4]));
        assert!(!fb.fragment(&mut mem, 0, 0, 0x90_0000, [7; 4]));
        assert_eq!(fb.read_depth(&mem, 0, 0), Some((0x80_0000, 0)));
        assert_eq!(fb.read_colour(&mem, 0, 0), Some([9; 4]));
    }

    #[test]
    fn stencil_operations_follow_the_test_results() {
        let mut regs = regs();
        // Pass where stencil == 1; replace with the reference (1) on depth
        // pass, increment on fail.
        regs[reg::STENCIL_TEST as usize] = 1 | 2 << 4 | 0xFF << 8 | 1 << 16 | 0xFF << 24;
        regs[reg::STENCIL_OP as usize] = 3 | 2 << 8;
        let fb = Framebuffer::new(&regs);
        let mut mem = VecMemory::new(COLOUR, 0x2000);
        assert!(!fb.fragment(&mut mem, 2, 2, 0, [1; 4]));
        assert_eq!(fb.read_depth(&mem, 2, 2).unwrap().1, 1, "incremented");
        assert!(fb.fragment(&mut mem, 2, 2, 0, [1; 4]));
    }

    #[test]
    fn alpha_blending_and_logic_ops_and_write_masks() {
        let mut regs = regs();
        // src * src alpha + dst * (1 - src alpha), alpha likewise.
        regs[reg::BLEND_FUNC as usize] = 6 << 16 | 7 << 20 | 6 << 24 | 7 << 28;
        let fb = Framebuffer::new(&regs);
        let mut mem = VecMemory::new(COLOUR, 0x2000);
        fb.fragment(&mut mem, 0, 0, 0, [200, 0, 100, 255]);
        fb.fragment(&mut mem, 0, 0, 0, [0, 200, 0, 51]);
        // 200 * 204 / 255 = 160; 200 * 51 / 255 = 40; 100 * 204 / 255 = 80.
        assert_eq!(fb.read_colour(&mem, 0, 0), Some([160, 40, 80, 214]));

        regs[reg::COLOR_OPERATION as usize] = 0;
        regs[reg::LOGIC_OP as usize] = 11; // xor
        regs[reg::DEPTH_COLOR_MASK as usize] = 0b0011 << 8;
        let fb = Framebuffer::new(&regs);
        fb.fragment(&mut mem, 0, 0, 0, [0xFF; 4]);
        assert_eq!(fb.read_colour(&mem, 0, 0), Some([95, 215, 80, 214]));
    }

    #[test]
    fn the_alpha_test_drops_fragments() {
        let mut regs = regs();
        regs[reg::FRAGOP_ALPHA_TEST as usize] = 1 | 6 << 4 | 0x80 << 8;
        let fb = Framebuffer::new(&regs);
        let mut mem = VecMemory::new(COLOUR, 0x2000);
        assert!(!fb.fragment(&mut mem, 0, 0, 0, [1, 1, 1, 0x80]));
        assert!(fb.fragment(&mut mem, 0, 0, 0, [1, 1, 1, 0x81]));
    }
}
