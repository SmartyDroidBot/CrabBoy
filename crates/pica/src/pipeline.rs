//! The per-fragment pipeline after the rasteriser: textures, the
//! combiners, then the output merger.

use crate::framebuffer::Framebuffer;
use crate::geometry::{varying, VARYING_FRAC};
use crate::memory::Memory;
use crate::raster::{Fragment, FragmentSink};
use crate::regs as reg;
use crate::tev::{Combiners, Sources};
use crate::texture::TextureUnit;

/// A varying in [0, 1] as a colour channel, to nearest.
pub fn channel(value: i32) -> u8 {
    let v = value.clamp(0, 1 << VARYING_FRAC) as i64;
    ((v * 255 + (1 << (VARYING_FRAC - 1))) >> VARYING_FRAC) as u8
}

/// The fragment stages of one draw.
pub struct Stages {
    pub framebuffer: Framebuffer,
    textures: [Option<TextureUnit>; 3],
    /// Texture unit 2 reads texcoord1 instead of texcoord2.
    unit2_texcoord1: bool,
    lod: bool,
    combiners: Combiners,
}

impl Stages {
    pub fn new(regs: &[u32]) -> Self {
        let config = regs[reg::TEXUNIT_CONFIG as usize];
        Stages {
            framebuffer: Framebuffer::new(regs),
            textures: std::array::from_fn(|unit| TextureUnit::new(regs, unit)),
            unit2_texcoord1: config >> 13 & 1 != 0,
            lod: regs[reg::TEXUNIT_LOD_ENABLE as usize] & 1 != 0,
            combiners: Combiners::new(regs),
        }
    }

    /// The colour a fragment leaves the combiners with.
    pub fn colour(&self, mem: &dyn Memory, f: &Fragment) -> [u8; 4] {
        let vars = &f.varyings;
        let mut sources = Sources {
            primary: std::array::from_fn(|i| channel(vars[varying::COLOR + i])),
            ..Sources::default()
        };
        for (unit, texture) in self.textures.iter().enumerate() {
            let Some(texture) = texture else {
                continue;
            };
            let coords = match unit {
                0 => 0,
                1 => 1,
                _ if self.unit2_texcoord1 => 1,
                _ => 2,
            };
            let first = [varying::TEXCOORD0, varying::TEXCOORD1, varying::TEXCOORD2][coords];
            let (mut u, mut v) = (vars[first], vars[first + 1]);
            if texture.projected {
                let w = vars[varying::TEXCOORD0_W] as i64;
                if w != 0 {
                    u = ((u as i64) << VARYING_FRAC).checked_div(w).unwrap_or(0) as i32;
                    v = ((v as i64) << VARYING_FRAC).checked_div(w).unwrap_or(0) as i32;
                }
            }
            let lod = self.lod.then(|| texture.lod(f.footprint[coords]));
            sources.texture[unit] = texture.sample(mem, u, v, lod);
        }
        self.combiners.combine(&sources)
    }
}

/// The fragment stages of one draw, over memory.
pub struct Pipeline<'a> {
    pub stages: &'a Stages,
    pub mem: &'a mut dyn Memory,
    /// Fragments that passed every test.
    pub written: u64,
}

impl FragmentSink for Pipeline<'_> {
    fn fragment(&mut self, f: &Fragment) {
        let colour = self.stages.colour(self.mem, f);
        if self
            .stages
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

    use crate::memory::VecMemory;

    #[test]
    fn the_combiners_see_the_texture_at_the_fragments_coordinates() {
        let mut regs = vec![0u32; reg::COUNT];
        regs[reg::TEXUNIT_CONFIG as usize] = 1;
        regs[reg::TEXUNIT0_DIM as usize] = 8 << 16 | 8;
        regs[reg::TEXUNIT0_ADDR1 as usize] = 0x1800_0000 >> 3;
        regs[reg::TEXUNIT0_TYPE as usize] = 7; // I8
                                               // Stage 0: texture 0 times the primary colour.
        regs[reg::TEXENV0_SOURCE as usize] = 0x03 | 0x03 << 16;
        regs[reg::TEXENV0_SOURCE as usize + 2] = 1 | 1 << 16;
        for stage in 1..6 {
            regs[reg::texenv(stage) as usize] = 0xF | 0xF << 16;
        }
        let mut mem = VecMemory::new(0x1800_0000, 0x100);
        // Texel (2, 7): memory row 0, Z index 4.
        mem.bytes[4] = 200;
        let stages = Stages::new(&regs);
        let mut varyings = [0; varying::COUNT];
        varyings[varying::COLOR..varying::COLOR + 4].copy_from_slice(&[
            1 << 16,
            1 << 15,
            0,
            1 << 16,
        ]);
        varyings[varying::TEXCOORD0] = (5 << 16) / 16;
        varyings[varying::TEXCOORD0 + 1] = (15 << 16) / 16;
        let f = Fragment {
            x: 0,
            y: 0,
            depth: 0,
            varyings,
            footprint: [[0; 4]; 3],
        };
        assert_eq!(stages.colour(&mem, &f), [200, 100, 0, 255]);
    }

    #[test]
    fn channels_round_to_nearest() {
        assert_eq!(channel(1 << 16), 255);
        assert_eq!(channel(1 << 15), 128);
        assert_eq!(channel(-5), 0);
        assert_eq!(channel(1 << 20), 255);
    }
}
