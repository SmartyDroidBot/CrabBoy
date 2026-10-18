//! Texture units 0 to 2: formats, tiling, wrapping, filters and mipmaps
//! (3dbrew, "GPU/Internal Registers", texturing registers).
//!
//! A texture is stored like the colour buffer: 8x8 tiles in rows, Z order
//! inside a tile, upside down (memory row 0 is t = 1). ETC1 textures split
//! each tile into four 4x4 blocks in Z order, each a 64-bit little-endian
//! ETC1 block (preceded by 64 bits of 4-bit alpha for ETC1A4). Mipmap
//! levels follow the base level directly, each half as wide and high.
//! Cube maps, shadow textures and the procedural texture unit are not
//! modelled; they sample as transparent black.

use crate::framebuffer::tiled_offset;
use crate::memory::Memory;
use crate::num;
use crate::regs as reg;

/// Fractional bits of texture coordinates (as varyings).
const COORD_FRAC: u32 = crate::geometry::VARYING_FRAC;
/// Fractional bits of filter weights and of the level of detail.
pub const WEIGHT_BITS: u32 = 8;

/// Texel formats, by their `TEXUNITi_TYPE` value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TexFormat {
    Rgba8,
    Rgb8,
    Rgb5a1,
    Rgb565,
    Rgba4,
    Ia8,
    Hilo8,
    I8,
    A8,
    Ia4,
    I4,
    A4,
    Etc1,
    Etc1a4,
}

impl TexFormat {
    pub fn from_bits(bits: u32) -> Option<Self> {
        use TexFormat::*;
        Some(match bits & 0xF {
            0 => Rgba8,
            1 => Rgb8,
            2 => Rgb5a1,
            3 => Rgb565,
            4 => Rgba4,
            5 => Ia8,
            6 => Hilo8,
            7 => I8,
            8 => A8,
            9 => Ia4,
            10 => I4,
            11 => A4,
            12 => Etc1,
            13 => Etc1a4,
            _ => return None,
        })
    }

    pub fn bits_per_texel(self) -> u32 {
        use TexFormat::*;
        match self {
            Rgba8 => 32,
            Rgb8 => 24,
            Rgb5a1 | Rgb565 | Rgba4 | Ia8 | Hilo8 => 16,
            I8 | A8 | Ia4 | Etc1a4 => 8,
            I4 | A4 | Etc1 => 4,
        }
    }
}

fn expand(value: u32, bits: u32) -> u8 {
    (value << (8 - bits) | value >> (2 * bits - 8)) as u8
}

/// Texel `index` (Z order) of an uncompressed 8x8 tile.
fn decode(format: TexFormat, tile: &[u8], index: usize) -> [u8; 4] {
    use TexFormat::*;
    let bytes = format.bits_per_texel() as usize / 8;
    let p = &tile[index * bytes.max(1)..];
    let half = || u16::from_le_bytes([p[0], p[1]]) as u32;
    let nibble = || {
        let byte = tile[index / 2];
        (if index & 1 == 0 {
            byte & 0xF
        } else {
            byte >> 4
        }) as u32
    };
    match format {
        Rgba8 => [p[3], p[2], p[1], p[0]],
        Rgb8 => [p[2], p[1], p[0], 0xFF],
        Rgb5a1 => {
            let v = half();
            let a = if v & 1 != 0 { 0xFF } else { 0 };
            [
                expand(v >> 11, 5),
                expand(v >> 6 & 0x1F, 5),
                expand(v >> 1 & 0x1F, 5),
                a,
            ]
        }
        Rgb565 => {
            let v = half();
            [
                expand(v >> 11, 5),
                expand(v >> 5 & 0x3F, 6),
                expand(v & 0x1F, 5),
                0xFF,
            ]
        }
        Rgba4 => {
            let v = half();
            [
                expand(v >> 12, 4),
                expand(v >> 8 & 0xF, 4),
                expand(v >> 4 & 0xF, 4),
                expand(v & 0xF, 4),
            ]
        }
        Ia8 => [p[1], p[1], p[1], p[0]],
        Hilo8 => [p[1], p[0], 0, 0xFF],
        I8 => [p[0], p[0], p[0], 0xFF],
        A8 => [0, 0, 0, p[0]],
        Ia4 => {
            let i = expand((p[0] >> 4) as u32, 4);
            [i, i, i, expand((p[0] & 0xF) as u32, 4)]
        }
        I4 => {
            let i = expand(nibble(), 4);
            [i, i, i, 0xFF]
        }
        A4 => [0, 0, 0, expand(nibble(), 4)],
        Etc1 | Etc1a4 => unreachable!("decoded by blocks"),
    }
}

/// The ETC1 intensity modifiers by table, small then large.
const ETC1_MODIFIERS: [[i32; 2]; 8] = [
    [2, 8],
    [5, 17],
    [9, 29],
    [13, 42],
    [18, 60],
    [24, 80],
    [33, 106],
    [47, 183],
];

/// Texel `(x, y)` (y down) of a 4x4 ETC1 block.
pub fn etc1(block: u64, x: u32, y: u32) -> [u8; 3] {
    let high = (block >> 32) as u32;
    let flip = high & 1 != 0;
    let differential = high & 2 != 0;
    let second = if flip { y >= 2 } else { x >= 2 };
    let base: [u8; 3] = std::array::from_fn(|c| {
        let shift = 27 - 8 * c as u32;
        if differential {
            let first = high >> shift & 0x1F;
            let delta = num::signed(high >> (shift - 3) & 7, 3);
            let value = if second {
                (first as i32 + delta) as u32 & 0x1F
            } else {
                first
            };
            expand(value, 5)
        } else {
            let value = high >> (if second { shift - 3 } else { shift + 1 }) & 0xF;
            expand(value, 4)
        }
    });
    let table = high >> (if second { 2 } else { 5 }) & 7;
    let i = x * 4 + y;
    let msb = block >> (16 + i) & 1;
    let lsb = block >> i & 1;
    let [small, large] = ETC1_MODIFIERS[table as usize];
    let modifier = match (msb, lsb) {
        (0, 0) => small,
        (0, _) => large,
        (_, 0) => -small,
        _ => -large,
    };
    base.map(|c| (c as i32 + modifier).clamp(0, 255) as u8)
}

/// Texture unit settings for one draw.
#[derive(Clone, Copy, Debug)]
pub struct TextureUnit {
    addr: u32,
    width: u32,
    height: u32,
    format: TexFormat,
    mag_linear: bool,
    min_linear: bool,
    mip_linear: bool,
    wrap_s: u32,
    wrap_t: u32,
    border: [u8; 4],
    /// Level-of-detail bias, with [`WEIGHT_BITS`] fractional bits.
    bias: i32,
    min_level: u32,
    max_level: u32,
    /// Texture unit 0 in projection mode divides by texcoord0.w.
    pub projected: bool,
    /// A type that is not modelled (cube, shadow) or "disabled".
    unsupported: bool,
}

/// Registers of texture unit `unit`: border colour, dimensions, parameters,
/// level of detail, address and type.
fn unit_registers(unit: usize) -> [u32; 6] {
    match unit {
        0 => [
            reg::TEXUNIT0_BORDER_COLOR,
            reg::TEXUNIT0_DIM,
            reg::TEXUNIT0_PARAM,
            reg::TEXUNIT0_LOD,
            reg::TEXUNIT0_ADDR1,
            reg::TEXUNIT0_TYPE,
        ],
        1 => [
            0x91,
            0x92,
            0x93,
            0x94,
            reg::TEXUNIT1_ADDR,
            reg::TEXUNIT1_TYPE,
        ],
        _ => [
            0x99,
            0x9A,
            0x9B,
            0x9C,
            reg::TEXUNIT2_ADDR,
            reg::TEXUNIT2_TYPE,
        ],
    }
}

impl TextureUnit {
    /// Unit `unit`, or `None` when `TEXUNIT_CONFIG` has it off.
    pub fn new(regs: &[u32], unit: usize) -> Option<Self> {
        if regs[reg::TEXUNIT_CONFIG as usize] >> unit & 1 == 0 {
            return None;
        }
        let r = unit_registers(unit).map(|id| regs[id as usize]);
        let [border, dim, param, lod, addr, kind] = r;
        let kind_bits = if unit == 0 { param >> 28 & 7 } else { 0 };
        let lod_enabled = regs[reg::TEXUNIT_LOD_ENABLE as usize] & 1 != 0;
        Some(TextureUnit {
            addr: (addr & 0x0FFF_FFFF) << 3,
            width: (dim >> 16 & 0x7FF).max(1),
            height: (dim & 0x7FF).max(1),
            format: TexFormat::from_bits(kind).unwrap_or(TexFormat::Rgba8),
            mag_linear: param >> 1 & 1 != 0,
            min_linear: param >> 2 & 1 != 0,
            mip_linear: param >> 24 & 1 != 0,
            wrap_s: param >> 12 & 7,
            wrap_t: param >> 8 & 7,
            border: border.to_le_bytes(),
            bias: if lod_enabled {
                num::signed(lod & 0x1FFF, 13)
            } else {
                0
            },
            min_level: lod >> 24 & 0xF,
            max_level: lod >> 16 & 0xF,
            projected: kind_bits == 3,
            unsupported: !matches!(kind_bits, 0 | 3) || TexFormat::from_bits(kind).is_none(),
        })
    }

    /// Bytes of mipmap level `level`.
    fn level_size(&self, level: u32) -> u32 {
        let (w, h) = ((self.width >> level).max(8), (self.height >> level).max(8));
        w * h * self.format.bits_per_texel() / 8
    }

    /// One texel of `level` at wrapped texel coordinates (`t` up).
    fn texel(&self, mem: &dyn Memory, level: u32, s: u32, t: u32) -> [u8; 4] {
        let addr = self.addr + (0..level).map(|l| self.level_size(l)).sum::<u32>();
        let (w, h) = ((self.width >> level).max(1), (self.height >> level).max(1));
        let row = h - 1 - t;
        let tile_texels = tiled_offset(s & !7, row & !7, w.max(8));
        let bits = self.format.bits_per_texel();
        let tile_addr = addr + tile_texels * bits / 8;
        let Some(tile) = mem.read(tile_addr, (64 * bits / 8) as usize) else {
            return [0; 4];
        };
        let (x, y) = (s & 7, row & 7);
        match self.format {
            TexFormat::Etc1 | TexFormat::Etc1a4 => {
                let alpha = self.format == TexFormat::Etc1a4;
                let block_bytes = if alpha { 16 } else { 8 };
                let block = &tile[((x / 4 + 2 * (y / 4)) * block_bytes) as usize..];
                let word = |b: &[u8]| u64::from_le_bytes(b[..8].try_into().unwrap());
                let (x, y) = (x % 4, y % 4);
                let (alpha, colour) = if alpha {
                    let a = word(block) >> ((x * 4 + y) * 4) & 0xF;
                    (expand(a as u32, 4), word(&block[8..]))
                } else {
                    (0xFF, word(block))
                };
                let [r, g, b] = etc1(colour, x, y);
                [r, g, b, alpha]
            }
            format => {
                let morton = tiled_offset(x, y, 8) as usize;
                decode(format, tile, morton)
            }
        }
    }

    /// Wrap a texel coordinate; `None` is the border.
    fn wrap(mode: u32, coord: i64, size: u32) -> Option<u32> {
        let size = size as i64;
        Some(match mode {
            1 | 5 => {
                if !(0..size).contains(&coord) {
                    return None;
                }
                coord
            }
            2 | 6 | 7 => coord.rem_euclid(size),
            3 => {
                let m = coord.rem_euclid(2 * size);
                if m >= size {
                    2 * size - 1 - m
                } else {
                    m
                }
            }
            _ => coord.clamp(0, size - 1),
        } as u32)
    }

    fn fetch(&self, mem: &dyn Memory, level: u32, s: i64, t: i64) -> [u8; 4] {
        let (w, h) = ((self.width >> level).max(1), (self.height >> level).max(1));
        match (Self::wrap(self.wrap_s, s, w), Self::wrap(self.wrap_t, t, h)) {
            (Some(s), Some(t)) => self.texel(mem, level, s, t),
            _ => self.border,
        }
    }

    /// Sample `level` at texture coordinates (u, v) with [`COORD_FRAC`]
    /// fractional bits, nearest or bilinear with 8-bit weights.
    fn sample_level(&self, mem: &dyn Memory, level: u32, u: i32, v: i32, linear: bool) -> [u8; 4] {
        let (w, h) = (
            (self.width >> level).max(1) as i64,
            (self.height >> level).max(1) as i64,
        );
        // Texel space with WEIGHT_BITS fractional bits.
        let shift = COORD_FRAC - WEIGHT_BITS;
        let s = (u as i64 * w) >> shift;
        let t = (v as i64 * h) >> shift;
        if !linear {
            return self.fetch(mem, level, s >> WEIGHT_BITS, t >> WEIGHT_BITS);
        }
        let half = 1 << (WEIGHT_BITS - 1);
        let (s, t) = (s - half, t - half);
        let (s0, t0) = (s >> WEIGHT_BITS, t >> WEIGHT_BITS);
        let one = 1i64 << WEIGHT_BITS;
        let (fs, ft) = (s & (one - 1), t & (one - 1));
        let texels = [
            (self.fetch(mem, level, s0, t0), (one - fs) * (one - ft)),
            (self.fetch(mem, level, s0 + 1, t0), fs * (one - ft)),
            (self.fetch(mem, level, s0, t0 + 1), (one - fs) * ft),
            (self.fetch(mem, level, s0 + 1, t0 + 1), fs * ft),
        ];
        std::array::from_fn(|c| {
            let sum: i64 = texels.iter().map(|(t, w)| t[c] as i64 * w).sum();
            ((sum + (1 << (2 * WEIGHT_BITS - 1))) >> (2 * WEIGHT_BITS)) as u8
        })
    }

    /// The level of detail for a footprint of `texels` texels per pixel
    /// (with [`COORD_FRAC`] fractional bits): log2 with [`WEIGHT_BITS`]
    /// fractional bits (the fraction linear between powers of two), plus
    /// the bias.
    pub fn lod(&self, footprint: [i32; 4]) -> i32 {
        let du =
            footprint[0].unsigned_abs().max(footprint[2].unsigned_abs()) as u64 * self.width as u64;
        let dv = footprint[1].unsigned_abs().max(footprint[3].unsigned_abs()) as u64
            * self.height as u64;
        let rho = du.max(dv);
        let log = if rho == 0 {
            -16 << WEIGHT_BITS
        } else {
            let whole = 63 - rho.leading_zeros() as i32;
            let fraction = if whole >= WEIGHT_BITS as i32 {
                (rho >> (whole - WEIGHT_BITS as i32)) as i32 & ((1 << WEIGHT_BITS) - 1)
            } else {
                ((rho << (WEIGHT_BITS as i32 - whole)) as i32) & ((1 << WEIGHT_BITS) - 1)
            };
            ((whole - COORD_FRAC as i32) << WEIGHT_BITS) + fraction
        };
        log + self.bias
    }

    /// Sample at (u, v); `lod` from [`TextureUnit::lod`], or `None` when
    /// level-of-detail calculation is off.
    pub fn sample(&self, mem: &dyn Memory, u: i32, v: i32, lod: Option<i32>) -> [u8; 4] {
        if self.unsupported {
            return [0; 4];
        }
        let lod = lod.unwrap_or(0);
        let min_level = self.min_level as i32;
        if lod <= 0 && min_level == 0 {
            return self.sample_level(mem, 0, u, v, self.mag_linear);
        }
        let max_level = (self.max_level as i32).max(min_level);
        let lod = lod.clamp(min_level << WEIGHT_BITS, max_level << WEIGHT_BITS);
        if !self.mip_linear {
            let level =
                ((lod + (1 << (WEIGHT_BITS - 1))) >> WEIGHT_BITS).clamp(min_level, max_level);
            return self.sample_level(mem, level as u32, u, v, self.min_linear);
        }
        let low = (lod >> WEIGHT_BITS) as u32;
        let high = (low + 1).min(max_level as u32);
        let f = (lod & ((1 << WEIGHT_BITS) - 1)) as u32;
        let a = self.sample_level(mem, low, u, v, self.min_linear);
        let b = self.sample_level(mem, high, u, v, self.min_linear);
        let one = 1 << WEIGHT_BITS;
        std::array::from_fn(|c| {
            ((a[c] as u32 * (one - f) + b[c] as u32 * f + one / 2) >> WEIGHT_BITS) as u8
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::VecMemory;

    const ADDR: u32 = 0x1800_0000;

    /// Unit 0 enabled with an 8x8 texture of `format`, nearest, clamped.
    fn regs(format: u32, width: u32, height: u32, param: u32) -> Vec<u32> {
        let mut regs = vec![0u32; reg::COUNT];
        regs[reg::TEXUNIT_CONFIG as usize] = 1;
        regs[reg::TEXUNIT0_DIM as usize] = width << 16 | height;
        regs[reg::TEXUNIT0_PARAM as usize] = param;
        regs[reg::TEXUNIT0_ADDR1 as usize] = ADDR >> 3;
        regs[reg::TEXUNIT0_TYPE as usize] = format;
        regs
    }

    /// u or v at the centre of texel `n` of `size`.
    fn centre(n: i32, size: i32) -> i32 {
        ((2 * n + 1) << 16) / (2 * size)
    }

    #[test]
    fn rgba8_texels_come_from_their_tile_upside_down() {
        let mut mem = VecMemory::new(ADDR, 0x1000);
        // Texel (s, t) = (1, 7) is memory row 0, Z index 1.
        mem.bytes[4..8].copy_from_slice(&[0x44, 0x33, 0x22, 0x11]);
        // Texel (8, 0) is in the second tile, memory row 7: index 42.
        let at = (64 + tiled_offset(0, 7, 8)) as usize * 4;
        mem.bytes[at..at + 4].copy_from_slice(&[1, 2, 3, 4]);
        let unit = TextureUnit::new(&regs(0, 16, 8, 0), 0).unwrap();
        assert_eq!(
            unit.sample(&mem, centre(1, 16), centre(7, 8), None),
            [0x11, 0x22, 0x33, 0x44]
        );
        assert_eq!(
            unit.sample(&mem, centre(8, 16), centre(0, 8), None),
            [4, 3, 2, 1]
        );
    }

    #[test]
    fn small_formats_decode() {
        let tile = [0x34u8, 0x12, 0xF0, 0x00];
        assert_eq!(decode(TexFormat::Ia8, &tile, 0), [0x12, 0x12, 0x12, 0x34]);
        assert_eq!(decode(TexFormat::Hilo8, &tile, 0), [0x12, 0x34, 0, 0xFF]);
        assert_eq!(decode(TexFormat::I4, &tile, 0), [0x44, 0x44, 0x44, 0xFF]);
        assert_eq!(decode(TexFormat::A4, &tile, 1), [0, 0, 0, 0x33]);
        assert_eq!(decode(TexFormat::Ia4, &tile, 2), [0xFF, 0xFF, 0xFF, 0]);
        assert_eq!(
            decode(TexFormat::Rgb565, &tile, 0),
            [0x10, 0x45, 0xA5, 0xFF]
        );
    }

    #[test]
    fn etc1_blocks_decode_in_both_modes() {
        // Individual mode, no flip: left half base (0x8, 0x8, 0x8) = 0x88,
        // right half (0x0, 0xF, 0x0); table 0 left, 7 right; all indices 0
        // except texel (0, 1) with msb set (-small).
        let high: u64 = 0x80 << 24 | 0x8F << 16 | 0x80 << 8 | (7 << 2);
        let low: u64 = 1 << (16 + 1);
        let block = high << 32 | low;
        assert_eq!(etc1(block, 0, 0), [0x8A; 3]);
        assert_eq!(etc1(block, 0, 1), [0x86; 3]);
        assert_eq!(etc1(block, 3, 3), [47, 0xFF, 47]);
        // Differential mode, flipped: base red 16 (0x84), delta -1 below.
        let high: u64 = (16 << 3 | 7) << 24 | 2 | 1;
        let block = high << 32;
        assert_eq!(etc1(block, 0, 0)[0], 0x86);
        assert_eq!(etc1(block, 0, 3)[0], 0x7D);
    }

    #[test]
    fn wrapping_modes() {
        assert_eq!(TextureUnit::wrap(0, -3, 8), Some(0));
        assert_eq!(TextureUnit::wrap(0, 9, 8), Some(7));
        assert_eq!(TextureUnit::wrap(1, 9, 8), None);
        assert_eq!(TextureUnit::wrap(2, -1, 8), Some(7));
        assert_eq!(TextureUnit::wrap(3, 8, 8), Some(7));
        assert_eq!(TextureUnit::wrap(3, -1, 8), Some(0));
    }

    #[test]
    fn bilinear_filtering_averages_neighbours() {
        let mut mem = VecMemory::new(ADDR, 0x1000);
        // Texels (0, 7) and (1, 7): red 0 and 200.
        mem.bytes[3] = 0;
        mem.bytes[7] = 200;
        let unit = TextureUnit::new(&regs(0, 8, 8, 1 << 1), 0).unwrap();
        // Halfway between the two centres.
        let u = 1 << 13;
        assert_eq!(unit.sample(&mem, u, centre(7, 8), None)[0], 100);
    }

    #[test]
    fn mipmap_levels_follow_the_base_level() {
        let mut mem = VecMemory::new(ADDR, 0x1000);
        // Level 1 of a 16x16 RGBA8 texture starts 1024 bytes in.
        mem.bytes[1024 + 3] = 77;
        let mut regs = regs(0, 16, 16, 0);
        regs[reg::TEXUNIT0_LOD as usize] = 1 << 16;
        regs[reg::TEXUNIT_LOD_ENABLE as usize] = 1;
        let unit = TextureUnit::new(&regs, 0).unwrap();
        // Two texels per pixel: level 1. Texel (0, 7) of level 1.
        let lod = unit.lod([2 << 16 >> 4, 0, 0, 2 << 16 >> 4]);
        assert_eq!(lod, 1 << WEIGHT_BITS);
        assert_eq!(
            unit.sample(&mem, centre(0, 8), centre(7, 8), Some(lod))[0],
            77
        );
    }
}
