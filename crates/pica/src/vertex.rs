//! The vertex loader: attribute buffers, fixed attributes, immediate-mode
//! vertices and index buffers (3dbrew, "GPU/Internal Registers", geometry
//! pipeline registers, and "GPU/Programming Guide").
//!
//! Up to twelve attributes make a vertex. Each comes from one of twelve
//! attribute buffers, whose components name the attributes they hold in
//! order (with padding between), or else from its fixed value. An attribute
//! is one to four elements of bytes, unsigned bytes, shorts or floats;
//! missing elements are 0, 0, 0 and 1. The loader puts attribute `i` in the
//! vertex shader input register its permutation names.

use crate::memory::Memory;
use crate::regs::{self as reg, sh};
use crate::shader::{from_float24, Vec4, ONE, ZERO};
use softfloat::{Env, F32};

/// The attributes of one vertex, by attribute number.
pub type Attributes = [Vec4; 12];

/// Decode a fixed attribute or an immediate-mode attribute from the three
/// words written to `FIXEDATTRIB_DATA0..2`: x in the low 24 bits of the
/// third, w in the high 24 bits of the first.
pub fn attribute_from_words(w: [u32; 3]) -> Vec4 {
    [
        from_float24(w[2] & 0xFF_FFFF),
        from_float24(w[2] >> 24 | (w[1] & 0xFFFF) << 8),
        from_float24(w[1] >> 16 | (w[0] & 0xFF) << 16),
        from_float24(w[0] >> 8),
    ]
}

/// Bytes of one element of each type: byte, unsigned byte, short, float.
fn element_size(kind: u32) -> usize {
    [1, 1, 2, 4][kind as usize & 3]
}

/// The loader's view of the registers for one draw.
pub struct Loader<'a> {
    regs: &'a [u32],
    base: u32,
}

impl<'a> Loader<'a> {
    pub fn new(regs: &'a [u32]) -> Self {
        let base = (regs[reg::ATTRIBBUFFERS_LOC as usize] & 0x1FFF_FFFE) << 3;
        Loader { regs, base }
    }

    fn reg(&self, id: u32) -> u32 {
        self.regs[id as usize]
    }

    /// Type and element count of attribute `n`.
    fn format(&self, n: usize) -> (u32, usize) {
        let word = if n < 8 {
            self.reg(reg::ATTRIBBUFFERS_FORMAT_LOW) >> (n * 4)
        } else {
            self.reg(reg::ATTRIBBUFFERS_FORMAT_HIGH) >> ((n - 8) * 4)
        };
        (word & 3, (word >> 2 & 3) as usize + 1)
    }

    /// How many attributes a vertex has.
    pub fn attribute_count(&self) -> usize {
        (self.reg(reg::ATTRIBBUFFERS_FORMAT_HIGH) >> 28) as usize + 1
    }

    /// The vertex number of the `n`th vertex of a draw: from the index
    /// buffer for `DRAWELEMENTS`, else counted from `VERTEX_OFFSET`.
    pub fn index(&self, n: u32, indexed: bool, mem: &dyn Memory) -> u32 {
        if !indexed {
            return self.reg(reg::VERTEX_OFFSET).wrapping_add(n);
        }
        let config = self.reg(reg::INDEXBUFFER_CONFIG);
        let at = self.base.wrapping_add(config & 0x0FFF_FFFF);
        if config >> 31 != 0 {
            mem.read(at.wrapping_add(n * 2), 2)
                .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]) as u32)
        } else {
            mem.read(at.wrapping_add(n), 1).map_or(0, |b| b[0] as u32)
        }
    }

    /// The attributes of vertex `vertex`: what the buffers hold, the fixed
    /// values for the rest.
    pub fn load(&self, vertex: u32, fixed: &Attributes, mem: &dyn Memory) -> Attributes {
        let mut out = *fixed;
        let mut env = Env::default();
        for buffer in 0..12u32 {
            let first = reg::ATTRIBBUFFER0_OFFSET + buffer * 3;
            let (offset, low, high) = (self.reg(first), self.reg(first + 1), self.reg(first + 2));
            let components = (high >> 28) as usize;
            let stride = high >> 16 & 0xFF;
            let mut at = self
                .base
                .wrapping_add(offset & 0x0FFF_FFFF)
                .wrapping_add(vertex.wrapping_mul(stride));
            for c in 0..components {
                let id = if c < 8 {
                    low >> (c * 4) & 0xF
                } else {
                    high >> ((c - 8) * 4) & 0xF
                } as usize;
                if id >= 12 {
                    // Padding of 4, 8, 12 or 16 bytes, on a word boundary.
                    at = at.next_multiple_of(4) + (id as u32 - 11) * 4;
                    continue;
                }
                let (kind, count) = self.format(id);
                let size = element_size(kind);
                at = at.next_multiple_of(size as u32);
                let mut value = [ZERO, ZERO, ZERO, ONE];
                if let Some(bytes) = mem.read(at, size * count) {
                    for (e, slot) in value.iter_mut().take(count).enumerate() {
                        let b = &bytes[e * size..];
                        *slot = match kind {
                            0 => F32::from_i32(b[0] as i8 as i32, &mut env),
                            1 => F32::from_i32(b[0] as i32, &mut env),
                            2 => F32::from_i32(i16::from_le_bytes([b[0], b[1]]) as i32, &mut env),
                            _ => F32(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                        };
                    }
                }
                out[id] = value;
                at += (size * count) as u32;
            }
        }
        out
    }
}

/// The shader input registers of a vertex: attribute `i` goes to the
/// register the shader block's permutation names for it.
pub fn shader_inputs(regs: &[u32], block: u32, attributes: &[Vec4], count: usize) -> [Vec4; 16] {
    let permutation = regs[(block + sh::ATTRIBUTES_PERMUTATION_LOW) as usize] as u64
        | (regs[(block + sh::ATTRIBUTES_PERMUTATION_HIGH) as usize] as u64) << 32;
    let mut inputs = [[ZERO; 4]; 16];
    for (i, attribute) in attributes.iter().take(count.min(16)).enumerate() {
        inputs[(permutation >> (i * 4) & 0xF) as usize] = *attribute;
    }
    inputs
}

/// Semantics of shader outputs (3dbrew, `GPUREG_SH_OUTMAP_Oi`): the slots of
/// [`OutputVertex::attrs`].
pub mod semantic {
    pub const POSITION: usize = 0x00;
    pub const QUATERNION: usize = 0x04;
    pub const COLOR: usize = 0x08;
    pub const TEXCOORD0: usize = 0x0C;
    pub const TEXCOORD1: usize = 0x0E;
    pub const TEXCOORD0_W: usize = 0x10;
    pub const VIEW: usize = 0x12;
    pub const TEXCOORD2: usize = 0x16;
    pub const COUNT: usize = 0x18;
}

/// A shaded vertex: every semantic the rasteriser uses, as floats. Those
/// no output register maps are one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OutputVertex {
    pub attrs: [F32; semantic::COUNT],
}

impl OutputVertex {
    /// Map the output registers of a shader run. The `k`th register enabled
    /// in `mask` takes the semantics of `SH_OUTMAP_Ok`, for as many as
    /// `SH_OUTMAP_TOTAL` gives (at most seven).
    pub fn from_outputs(regs: &[u32], mask: u32, outputs: &[Vec4; 16]) -> Self {
        let mut attrs = [ONE; semantic::COUNT];
        let total = (regs[reg::SH_OUTMAP_TOTAL as usize] & 7) as usize;
        let enabled = (0..16).filter(|r| mask >> r & 1 != 0);
        for (k, register) in enabled.take(total).enumerate() {
            let map = regs[reg::SH_OUTMAP_O0 as usize + k];
            for (component, value) in outputs[register].iter().enumerate() {
                let semantic = (map >> (component * 8) & 0x1F) as usize;
                if let Some(slot) = attrs.get_mut(semantic) {
                    *slot = *value;
                }
            }
        }
        OutputVertex { attrs }
    }

    pub fn position(&self) -> Vec4 {
        let p = &self.attrs[semantic::POSITION..];
        [p[0], p[1], p[2], p[3]]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::VecMemory;

    fn f(v: f32) -> F32 {
        F32(v.to_bits())
    }

    #[test]
    fn fixed_attribute_words_unpack_as_citro3d_packs_them() {
        // (1, -1, 3, 0.5) in the 24-bit format: 3F0000 BF0000 408000 3E0000.
        let words = [0x3E00_0040, 0x8000_BF00, 0x003F_0000];
        assert_eq!(
            attribute_from_words(words),
            [f(1.0), f(-1.0), f(3.0), f(0.5)]
        );
    }

    #[test]
    fn buffers_interleave_attributes_with_padding_and_alignment() {
        let mut regs = vec![0u32; reg::COUNT];
        let base = 0x2000_0000u32;
        regs[reg::ATTRIBBUFFERS_LOC as usize] = base >> 3;
        // Attribute 0: three floats. Attribute 1: two shorts. Attribute 2:
        // four unsigned bytes. Attribute 3: one byte (signed), fixed bit set.
        regs[reg::ATTRIBBUFFERS_FORMAT_LOW as usize] =
            3 | 2 << 2 | (2 | 1 << 2) << 4 | (1 | 3 << 2) << 8;
        regs[reg::ATTRIBBUFFERS_FORMAT_HIGH as usize] = 3 << 28 | 1 << 19;
        // Buffer 0 at +0x10: attribute 2, then attribute 0 (aligned to four
        // bytes), 4 bytes of padding, attribute 1. 28 bytes a vertex.
        regs[reg::ATTRIBBUFFER0_OFFSET as usize] = 0x10;
        regs[reg::ATTRIBBUFFER0_OFFSET as usize + 1] = 2 | 12 << 8 | 1 << 12;
        regs[reg::ATTRIBBUFFER0_OFFSET as usize + 2] = 4 << 28 | 28 << 16;
        let mut mem = VecMemory::new(base, 0x100);
        let vertex = 0x10 + 28;
        let bytes = &mut mem.bytes[vertex..vertex + 28];
        bytes[0..4].copy_from_slice(&[1, 2, 3, 255]);
        bytes[4..8].copy_from_slice(&1.5f32.to_le_bytes());
        bytes[8..12].copy_from_slice(&(-2.0f32).to_le_bytes());
        bytes[12..16].copy_from_slice(&4.0f32.to_le_bytes());
        bytes[20..22].copy_from_slice(&(-300i16).to_le_bytes());
        bytes[22..24].copy_from_slice(&7i16.to_le_bytes());
        let mut fixed = [[ZERO; 4]; 12];
        fixed[3] = [f(9.0); 4];
        let loader = Loader::new(&regs);
        assert_eq!(loader.attribute_count(), 4);
        let attrs = loader.load(1, &fixed, &mem);
        assert_eq!(attrs[0], [f(1.5), f(-2.0), f(4.0), ONE]);
        assert_eq!(attrs[1], [f(-300.0), f(7.0), ZERO, ONE]);
        assert_eq!(attrs[2], [f(1.0), f(2.0), f(3.0), f(255.0)]);
        assert_eq!(attrs[3], [f(9.0); 4], "a fixed attribute");
    }

    #[test]
    fn indices_come_from_bytes_or_shorts_and_arrays_count_from_the_offset() {
        let mut regs = vec![0u32; reg::COUNT];
        regs[reg::ATTRIBBUFFERS_LOC as usize] = 0x100 >> 3;
        regs[reg::VERTEX_OFFSET as usize] = 5;
        let mut mem = VecMemory::new(0x100, 0x40);
        mem.bytes[0x20..0x24].copy_from_slice(&[7, 0, 9, 1]);
        let loader = Loader::new(&regs);
        assert_eq!(loader.index(2, false, &mem), 7);
        regs[reg::INDEXBUFFER_CONFIG as usize] = 0x20;
        let loader = Loader::new(&regs);
        assert_eq!(loader.index(2, true, &mem), 9);
        regs[reg::INDEXBUFFER_CONFIG as usize] = 1 << 31 | 0x20;
        let loader = Loader::new(&regs);
        assert_eq!(loader.index(1, true, &mem), 0x109);
    }

    #[test]
    fn outputs_map_by_enabled_register_and_unmapped_semantics_are_one() {
        let mut regs = vec![0u32; reg::COUNT];
        regs[reg::SH_OUTMAP_TOTAL as usize] = 2;
        // o0 is position, o2 colour (o1 is disabled).
        regs[reg::SH_OUTMAP_O0 as usize] = 0x0302_0100;
        regs[reg::SH_OUTMAP_O0 as usize + 1] = 0x0B0A_0908;
        let mut outputs = [[ZERO; 4]; 16];
        outputs[0] = [f(1.0), f(2.0), f(3.0), f(4.0)];
        outputs[2] = [f(0.25); 4];
        let v = OutputVertex::from_outputs(&regs, 0b101, &outputs);
        assert_eq!(v.position(), outputs[0]);
        assert_eq!(v.attrs[semantic::COLOR..semantic::COLOR + 4], [f(0.25); 4]);
        assert_eq!(v.attrs[semantic::TEXCOORD0], ONE);

        let inputs = shader_inputs(
            &{
                let mut r = vec![0u32; reg::COUNT];
                r[(reg::VSH + sh::ATTRIBUTES_PERMUTATION_LOW) as usize] = 0x0000_0053;
                r
            },
            reg::VSH,
            &[[f(1.0); 4], [f(2.0); 4]],
            2,
        );
        assert_eq!((inputs[3], inputs[5]), ([f(1.0); 4], [f(2.0); 4]));
    }
}
