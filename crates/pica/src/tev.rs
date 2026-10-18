//! The texture combiners ("TEV"): six stages that each combine three
//! sources into a colour and an alpha, and the combiner buffer between them
//! (3dbrew, "GPU/Internal Registers", `GPUREG_TEXENVi_*` and
//! `GPUREG_TEXENV_UPDATE_BUFFER`).
//!
//! Everything is 8-bit integer arithmetic; products divide by 255,
//! truncating.

use crate::regs as reg;

pub type Rgba = [u8; 4];

/// The inputs a stage can choose from.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sources {
    pub primary: Rgba,
    pub fragment_primary: Rgba,
    pub fragment_secondary: Rgba,
    pub texture: [Rgba; 4],
}

#[derive(Clone, Copy, Debug)]
struct Stage {
    source: u32,
    operand: u32,
    combiner: u32,
    constant: Rgba,
    scale: u32,
}

/// The combiner settings of one draw.
#[derive(Clone, Copy, Debug)]
pub struct Combiners {
    stages: [Stage; 6],
    update: u32,
    buffer_colour: Rgba,
}

fn mul(a: u8, b: u8) -> u32 {
    a as u32 * b as u32 / 255
}

impl Combiners {
    pub fn new(regs: &[u32]) -> Self {
        let stages = std::array::from_fn(|i| {
            let first = reg::texenv(i as u32) as usize;
            Stage {
                source: regs[first],
                operand: regs[first + 1],
                combiner: regs[first + 2],
                constant: regs[first + 3].to_le_bytes(),
                scale: regs[first + 4],
            }
        });
        Combiners {
            stages,
            update: regs[reg::TEXENV_UPDATE_BUFFER as usize],
            buffer_colour: regs[reg::TEXENV_BUFFER_COLOR as usize].to_le_bytes(),
        }
    }

    /// Run the six stages. The buffer a stage reads is what an earlier
    /// stage's update put there one stage late, as Citra has it from
    /// hardware tests: a stage's update is first seen two stages on.
    pub fn combine(&self, sources: &Sources) -> Rgba {
        let mut previous = [0u8; 4];
        let mut buffer = self.buffer_colour;
        let mut next_buffer = self.buffer_colour;
        for (i, stage) in self.stages.iter().enumerate() {
            let get = |n: u32| -> Rgba {
                match n & 0xF {
                    0 => sources.primary,
                    1 => sources.fragment_primary,
                    2 => sources.fragment_secondary,
                    3..=6 => sources.texture[(n & 0xF) as usize - 3],
                    13 => buffer,
                    14 => stage.constant,
                    15 => previous,
                    _ => [0; 4],
                }
            };
            let colour_args: [[u8; 3]; 3] = std::array::from_fn(|k| {
                let c = get(stage.source >> (4 * k));
                rgb_operand(stage.operand >> (4 * k) & 0xF, c)
            });
            let alpha_args: [u8; 3] = std::array::from_fn(|k| {
                let c = get(stage.source >> (16 + 4 * k));
                alpha_operand(stage.operand >> (12 + 4 * k) & 7, c)
            });
            let rgb_mode = stage.combiner & 0xF;
            let rgb = combine_rgb(rgb_mode, colour_args);
            let alpha = if rgb_mode == 7 {
                rgb[0] as u32
            } else {
                combine(stage.combiner >> 16 & 0xF, alpha_args)
            };
            let scale = |v: u32, s: u32| (v << (s & 3).min(2)).min(255) as u8;
            let out = [
                scale(rgb[0], stage.scale),
                scale(rgb[1], stage.scale),
                scale(rgb[2], stage.scale),
                scale(alpha, stage.scale >> 16),
            ];
            buffer = next_buffer;
            if i < 4 && self.update >> (8 + i) & 1 != 0 {
                next_buffer[..3].copy_from_slice(&out[..3]);
            }
            if i < 4 && self.update >> (12 + i) & 1 != 0 {
                next_buffer[3] = out[3];
            }
            previous = out;
        }
        previous
    }
}

fn rgb_operand(operand: u32, c: Rgba) -> [u8; 3] {
    let all = |v: u8| [v; 3];
    let rgb = [c[0], c[1], c[2]];
    match operand {
        0 => rgb,
        1 => rgb.map(|v| 255 - v),
        2 => all(c[3]),
        3 => all(255 - c[3]),
        4 => all(c[0]),
        5 => all(255 - c[0]),
        8 => all(c[1]),
        9 => all(255 - c[1]),
        12 => all(c[2]),
        13 => all(255 - c[2]),
        _ => rgb,
    }
}

fn alpha_operand(operand: u32, c: Rgba) -> u8 {
    match operand {
        0 => c[3],
        1 => 255 - c[3],
        2 => c[0],
        3 => 255 - c[0],
        4 => c[1],
        5 => 255 - c[1],
        6 => c[2],
        _ => 255 - c[2],
    }
}

/// One channel of a combine mode.
fn combine(mode: u32, [a, b, c]: [u8; 3]) -> u32 {
    let (wa, wb, wc) = (a as u32, b as u32, c as u32);
    match mode {
        0 => wa,
        1 => mul(a, b),
        2 => (wa + wb).min(255),
        3 => (wa as i32 + wb as i32 - 128).clamp(0, 255) as u32,
        4 => (wa * wc + wb * (255 - wc)) / 255,
        5 => wa.saturating_sub(wb),
        8 => (mul(a, b) + wc).min(255),
        9 => (wa + wb).min(255) * wc / 255,
        _ => wa,
    }
}

fn combine_rgb(mode: u32, args: [[u8; 3]; 3]) -> [u32; 3] {
    if mode == 6 || mode == 7 {
        // (2a − 1)·(2b − 1) per channel in units of 1/256, rounded as
        // Citra's hardware-informed formula has it, summed.
        let dot: i32 = (0..3)
            .map(|i| {
                let (a, b) = (args[0][i] as i32 * 2 - 255, args[1][i] as i32 * 2 - 255);
                (a * b + 128) / 256
            })
            .sum();
        let value = dot.clamp(0, 255) as u32;
        return [value; 3];
    }
    std::array::from_fn(|i| combine(mode, [args[0][i], args[1][i], args[2][i]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stage `stage`: sources, operands, combiners, constant, scale.
    fn set(regs: &mut [u32], stage: u32, values: [u32; 5]) {
        let first = reg::texenv(stage) as usize;
        regs[first..first + 5].copy_from_slice(&values);
    }

    /// Every stage passes "previous" through.
    fn regs() -> Vec<u32> {
        let mut regs = vec![0u32; reg::COUNT];
        for stage in 0..6 {
            set(
                &mut regs,
                stage,
                [0x00F0_00F0 | 0xF | 0xF << 16, 0, 0, 0, 0],
            );
        }
        regs
    }

    #[test]
    fn a_texture_modulated_by_the_vertex_colour() {
        let mut regs = regs();
        // Stage 0: texture 0 times primary colour, for colour and alpha.
        set(&mut regs, 0, [0x0003_0003, 0, 1 | 1 << 16, 0, 0]);
        let sources = Sources {
            primary: [255, 128, 0, 255],
            texture: [[200, 200, 200, 100], [0; 4], [0; 4], [0; 4]],
            ..Sources::default()
        };
        assert_eq!(Combiners::new(&regs).combine(&sources), [200, 100, 0, 100]);
    }

    #[test]
    fn constants_scales_and_interpolation() {
        let mut regs = regs();
        // Stage 0: constant, scaled by 2.
        set(
            &mut regs,
            0,
            [
                0x000E_000E,
                0,
                0,
                u32::from_le_bytes([100, 150, 20, 64]),
                1 | 2 << 16,
            ],
        );
        // Stage 1: lerp(previous, constant, primary alpha).
        set(
            &mut regs,
            1,
            [
                0x0000_0E0F | 0xE0F << 16,
                0x2 << 8,
                4 | 4 << 16,
                0xFF00_0000,
                0,
            ],
        );
        let sources = Sources {
            primary: [0, 0, 0, 255],
            ..Sources::default()
        };
        assert_eq!(Combiners::new(&regs).combine(&sources), [200, 255, 40, 255]);
    }

    #[test]
    fn the_buffer_is_updated_one_stage_late() {
        let mut regs = regs();
        regs[reg::TEXENV_BUFFER_COLOR as usize] = u32::from_le_bytes([1, 2, 3, 4]);
        // Stage 0 writes the constant and updates the buffer.
        set(
            &mut regs,
            0,
            [0x000E_000E, 0, 0, u32::from_le_bytes([9; 4]), 0],
        );
        regs[reg::TEXENV_UPDATE_BUFFER as usize] = 1 << 8 | 1 << 12;
        // Stage 1 still sees the buffer colour; stage 2 sees stage 0's.
        set(&mut regs, 1, [0x000D_000D, 0, 0, 0, 0]);
        let combiners = Combiners::new(&regs);
        assert_eq!(combiners.combine(&Sources::default()), [1, 2, 3, 4]);
        set(&mut regs, 2, [0x000D_000D, 0, 0, 0, 0]);
        assert_eq!(Combiners::new(&regs).combine(&Sources::default()), [9; 4]);
    }

    #[test]
    fn dot3_and_the_signed_and_multiply_add_modes() {
        let a = [[255, 128, 128], [255, 128, 128], [0; 3]];
        assert_eq!(combine_rgb(6, a), [254; 3]);
        assert_eq!(combine(3, [200, 100, 0]), 172);
        assert_eq!(combine(8, [255, 128, 10]), 138);
        assert_eq!(combine(9, [100, 100, 128]), 100);
        assert_eq!(combine(5, [10, 100, 0]), 0);
    }
}
