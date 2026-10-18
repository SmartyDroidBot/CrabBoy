//! The rasteriser: which pixels a triangle covers, and the depth and
//! varyings at each (3dbrew, "GPU/Internal Registers", rasteriser
//! registers).
//!
//! Coverage is decided by edge functions on window coordinates with four
//! subpixel bits, sampled at pixel centres, with a top-left rule for samples
//! exactly on an edge. Depth is interpolated linearly in window space;
//! varyings perspective-correctly. All of it is integer arithmetic; the
//! scheme is in `docs/3ds/gpu.md`, "Fixed point".

use crate::geometry::{signed_area, varying, ScreenVertex, DEPTH_FRAC, SUBPIXEL_BITS};
use crate::regs as reg;

/// Fractional bits of the interpolation weights.
pub const WEIGHT_FRAC: u32 = 24;
/// The largest depth a buffer holds: depth in 24 bits.
pub const DEPTH_MAX: u32 = 0xFF_FFFF;

/// One covered pixel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fragment {
    /// Window coordinates, y up.
    pub x: u32,
    pub y: u32,
    /// Depth in 24 bits, clamped to [0, 1].
    pub depth: u32,
    /// The varyings, as `geometry::varying` orders them.
    pub varyings: [i32; varying::COUNT],
    /// With texture level of detail on: how texcoord0, 1 and 2 change one
    /// pixel right and one pixel up (du/dx, dv/dx, du/dy, dv/dy).
    pub footprint: [[i32; 4]; 3],
}

/// Whatever takes the fragments: the texture, combiner and output stages.
pub trait FragmentSink {
    fn fragment(&mut self, fragment: &Fragment);
}

/// What the rasteriser reads from the registers.
#[derive(Clone, Copy, Debug)]
pub struct Raster {
    width: i64,
    height: i64,
    /// Inclusive window rectangle, and whether fragments inside it are kept
    /// (mode 3) or dropped (mode 1).
    scissor: Option<(bool, [i64; 4])>,
    /// 1: only even x; 2: even x and odd y.
    pixel_mask: u32,
    w_buffer: bool,
    lod: bool,
}

impl Raster {
    pub fn new(regs: &[u32]) -> Self {
        let r = |id: u32| regs[id as usize];
        let dim = r(reg::FRAMEBUFFER_DIM);
        let (pos, end) = (r(reg::SCISSORTEST_POS), r(reg::SCISSORTEST_DIM));
        let rect = [
            (pos & 0x3FF) as i64,
            (pos >> 16 & 0x3FF) as i64,
            (end & 0x3FF) as i64,
            (end >> 16 & 0x3FF) as i64,
        ];
        Raster {
            width: (dim & 0x7FF) as i64,
            height: (dim >> 12 & 0x3FF) as i64 + 1,
            scissor: match r(reg::SCISSORTEST_MODE) & 3 {
                1 => Some((false, rect)),
                3 => Some((true, rect)),
                _ => None,
            },
            pixel_mask: r(reg::RASTER_PIXEL_MASK) >> 8 & 3,
            w_buffer: r(reg::DEPTHMAP_MODE) & 1 == 0,
            lod: r(reg::TEXUNIT_LOD_ENABLE) & 1 != 0,
        }
    }

    /// Rasterise one triangle; returns the number of fragments sent on.
    pub fn triangle(&self, triangle: &[ScreenVertex; 3], sink: &mut dyn FragmentSink) -> u64 {
        let Some(setup) = Triangle::new(triangle) else {
            return 0;
        };
        let one = 1 << SUBPIXEL_BITS;
        let xs = triangle.map(|v| v.x as i64);
        let ys = triangle.map(|v| v.y as i64);
        let mut x0 = (*xs.iter().min().unwrap()).div_euclid(one).max(0);
        let mut y0 = (*ys.iter().min().unwrap()).div_euclid(one).max(0);
        let mut x1 = (*xs.iter().max().unwrap())
            .div_euclid(one)
            .min(self.width - 1);
        let mut y1 = (*ys.iter().max().unwrap())
            .div_euclid(one)
            .min(self.height - 1);
        if let Some((true, [sx0, sy0, sx1, sy1])) = self.scissor {
            (x0, y0, x1, y1) = (x0.max(sx0), y0.max(sy0), x1.min(sx1), y1.min(sy1));
        }
        let mut count = 0;
        for y in y0..=y1 {
            if self.pixel_mask == 2 && y & 1 == 0 {
                continue;
            }
            for x in x0..=x1 {
                if self.pixel_mask != 0 && self.pixel_mask != 3 && x & 1 != 0 {
                    continue;
                }
                if let Some((false, [sx0, sy0, sx1, sy1])) = self.scissor {
                    if (sx0..=sx1).contains(&x) && (sy0..=sy1).contains(&y) {
                        continue;
                    }
                }
                let centre = (x * one + one / 2, y * one + one / 2);
                let Some(edges) = setup.covers(centre) else {
                    continue;
                };
                let mut fragment = setup.fragment(x as u32, y as u32, edges, self.w_buffer);
                if self.lod {
                    fragment.footprint = setup.footprint(centre, &fragment);
                }
                sink.fragment(&fragment);
                count += 1;
            }
        }
        count
    }
}

/// A triangle prepared for rasterising: counter-clockwise, with its edge
/// biases and perspective weights.
struct Triangle {
    v: [ScreenVertex; 3],
    /// Twice the area, positive.
    area: i64,
    /// Subtracted from an edge function: 0 for a top or left edge, whose
    /// samples count, 1 for the others.
    bias: [i64; 3],
    /// `1/w` of each vertex in fixed point with `q_frac` fractional bits,
    /// scaled so the largest is near 2^30.
    q: [i64; 3],
    q_frac: u32,
}

impl Triangle {
    fn new(t: &[ScreenVertex; 3]) -> Option<Self> {
        let mut v = *t;
        let mut area = signed_area(&v);
        if area < 0 {
            v.swap(1, 2);
            area = -area;
        }
        if area == 0 {
            return None;
        }
        // Edge i is the one opposite vertex i, running counter-clockwise.
        let bias = std::array::from_fn(|i| {
            let (a, b) = (&v[(i + 1) % 3], &v[(i + 2) % 3]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let top_left = dy < 0 || (dy == 0 && dx < 0);
            (!top_left) as i64
        });
        let exponent = v
            .iter()
            .map(|v| v.inv_w.0 >> 23 & 0xFF)
            .max()
            .unwrap_or(127) as i32;
        let q_frac = (30 + 127 - exponent).clamp(0, 62) as u32;
        let q = v.map(|v| v.inv_w.to_fixed(q_frac, softfloat::Round::Nearest).max(0));
        Some(Triangle {
            v,
            area,
            bias,
            q,
            q_frac,
        })
    }

    fn edge(&self, i: usize, (px, py): (i64, i64)) -> i64 {
        let (a, b) = (&self.v[(i + 1) % 3], &self.v[(i + 2) % 3]);
        (b.x as i64 - a.x as i64) * (py - a.y as i64)
            - (b.y as i64 - a.y as i64) * (px - a.x as i64)
    }

    /// The edge functions at a sample, if it is inside.
    fn covers(&self, p: (i64, i64)) -> Option<[i64; 3]> {
        let e = [self.edge(0, p), self.edge(1, p), self.edge(2, p)];
        (0..3).all(|i| e[i] - self.bias[i] >= 0).then_some(e)
    }

    /// The texture coordinates' change to the samples one pixel right and
    /// one pixel up, interpolated as if they were inside.
    fn footprint(&self, (cx, cy): (i64, i64), here: &Fragment) -> [[i32; 4]; 3] {
        let one = 1 << SUBPIXEL_BITS;
        let at = |p: (i64, i64)| {
            let e = [self.edge(0, p), self.edge(1, p), self.edge(2, p)];
            self.fragment(0, 0, e, false).varyings
        };
        let (right, up) = (at((cx + one, cy)), at((cx, cy + one)));
        [varying::TEXCOORD0, varying::TEXCOORD1, varying::TEXCOORD2].map(|k| {
            [
                right[k].wrapping_sub(here.varyings[k]),
                right[k + 1].wrapping_sub(here.varyings[k + 1]),
                up[k].wrapping_sub(here.varyings[k]),
                up[k + 1].wrapping_sub(here.varyings[k + 1]),
            ]
        })
    }

    fn fragment(&self, x: u32, y: u32, e: [i64; 3], w_buffer: bool) -> Fragment {
        // Perspective-correct weights: e_i / w_i, normalised.
        let p = [0, 1, 2].map(|i| e[i] as i128 * self.q[i] as i128);
        let sum: i128 = p.iter().sum();
        let weights: [i64; 3] = if sum > 0 {
            let w0 = ((p[0] << WEIGHT_FRAC) / sum) as i64;
            let w1 = ((p[1] << WEIGHT_FRAC) / sum) as i64;
            [w0, w1, (1 << WEIGHT_FRAC) - w0 - w1]
        } else {
            let w0 = ((e[0] as i128) << WEIGHT_FRAC) / self.area as i128;
            let w1 = ((e[1] as i128) << WEIGHT_FRAC) / self.area as i128;
            [
                w0 as i64,
                w1 as i64,
                (1 << WEIGHT_FRAC) - w0 as i64 - w1 as i64,
            ]
        };
        let varyings = std::array::from_fn(|k| {
            let total: i64 = (0..3)
                .map(|i| weights[i] * self.v[i].varyings[k] as i64)
                .sum();
            ((total + (1 << (WEIGHT_FRAC - 1))) >> WEIGHT_FRAC) as i32
        });
        // Depth: linear in window space, times w for a W-buffer.
        let z: i128 = (0..3).map(|i| e[i] as i128 * self.v[i].z as i128).sum();
        let depth = if w_buffer && sum > 0 {
            (z << self.q_frac) / sum
        } else {
            z / self.area as i128
        };
        let depth = (depth >> (DEPTH_FRAC - 24)).clamp(0, DEPTH_MAX as i128) as u32;
        Fragment {
            x,
            y,
            depth,
            varyings,
            footprint: [[0; 4]; 3],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use softfloat::F32;

    struct Record(Vec<Fragment>);

    impl FragmentSink for Record {
        fn fragment(&mut self, fragment: &Fragment) {
            self.0.push(*fragment);
        }
    }

    fn v(x: i32, y: i32, z: i64, w: f32, colour: i32) -> ScreenVertex {
        let mut varyings = [0; varying::COUNT];
        varyings[varying::COLOR] = colour;
        ScreenVertex {
            x: x << SUBPIXEL_BITS,
            y: y << SUBPIXEL_BITS,
            z,
            inv_w: F32((1.0 / w).to_bits()),
            varyings,
        }
    }

    fn regs(width: u32, height: u32) -> Vec<u32> {
        let mut regs = vec![0u32; reg::COUNT];
        regs[reg::FRAMEBUFFER_DIM as usize] = (height - 1) << 12 | width;
        regs[reg::DEPTHMAP_MODE as usize] = 1;
        regs
    }

    fn run(regs: &[u32], t: [ScreenVertex; 3]) -> Vec<Fragment> {
        let mut sink = Record(Vec::new());
        Raster::new(regs).triangle(&t, &mut sink);
        sink.0
    }

    #[test]
    fn two_triangles_sharing_an_edge_cover_each_pixel_once() {
        let regs = regs(16, 16);
        let (a, b, c, d) = (
            v(0, 0, 0, 1.0, 0),
            v(8, 0, 0, 1.0, 0),
            v(0, 8, 0, 1.0, 0),
            v(8, 8, 0, 1.0, 0),
        );
        let mut seen = vec![0; 256];
        for t in [[a, b, c], [c, b, d], [a, c, b]] {
            for f in run(&regs, t) {
                seen[(f.y * 16 + f.x) as usize] += 1;
            }
        }
        // The square is covered once by the first two (the diagonal goes to
        // the second, whose left edge it is), and the reversed copy of the
        // first covers its 28 pixels again.
        let covered = seen.iter().filter(|&&n| n > 0).count();
        assert_eq!(covered, 64);
        assert_eq!(seen.iter().sum::<i32>(), 64 + 28);
    }

    #[test]
    fn colour_interpolates_and_constant_values_stay_exact() {
        let regs = regs(16, 16);
        let t = [
            v(0, 0, 1 << 23, 1.0, 0),
            v(16, 0, 1 << 23, 1.0, 1 << 16),
            v(0, 16, 1 << 23, 1.0, 0),
        ];
        let frags = run(&regs, t);
        let at = |x, y| frags.iter().find(|f| f.x == x && f.y == y).unwrap();
        // At the centre of pixel 7 the weight of the second vertex is 7.5/16.
        assert_eq!(at(7, 0).varyings[varying::COLOR], 30720);
        assert_eq!(at(3, 5).depth, 1 << 23);
    }

    #[test]
    fn perspective_bends_the_interpolation_toward_the_near_vertex() {
        let regs = regs(16, 16);
        let t = [
            v(0, 0, 0, 1.0, 0),
            v(16, 0, 0, 4.0, 1 << 16),
            v(0, 16, 0, 1.0, 0),
        ];
        let frags = run(&regs, t);
        let mid = frags.iter().find(|f| f.x == 7 && f.y == 0).unwrap();
        // Screen weight 7.5/16 of a vertex four times as far: 7.5 / (7.5 + 4 * 8.5).
        let expected = (7.5f64 / (7.5 + 4.0 * 8.5) * 65536.0).round() as i32;
        assert!((mid.varyings[varying::COLOR] - expected).abs() <= 1);
    }

    #[test]
    fn the_scissor_and_the_framebuffer_bound_the_pixels() {
        let mut regs = regs(8, 8);
        let big = [
            v(-20, -20, 0, 1.0, 0),
            v(40, -20, 0, 1.0, 0),
            v(-20, 40, 0, 1.0, 0),
        ];
        assert_eq!(run(&regs, big).len(), 64);
        regs[reg::SCISSORTEST_MODE as usize] = 3;
        regs[reg::SCISSORTEST_POS as usize] = 2 << 16 | 1;
        regs[reg::SCISSORTEST_DIM as usize] = 4 << 16 | 3;
        assert_eq!(run(&regs, big).len(), 9);
        regs[reg::SCISSORTEST_MODE as usize] = 1;
        assert_eq!(run(&regs, big).len(), 55);
        regs[reg::SCISSORTEST_MODE as usize] = 0;
        regs[reg::RASTER_PIXEL_MASK as usize] = 2 << 8;
        assert_eq!(run(&regs, big).len(), 16);
    }

    #[test]
    fn a_w_buffer_scales_depth_by_w() {
        let mut regs = regs(16, 16);
        regs[reg::DEPTHMAP_MODE as usize] = 0;
        let t = [
            v(0, 0, 1 << 22, 2.0, 0),
            v(16, 0, 1 << 22, 2.0, 0),
            v(0, 16, 1 << 22, 2.0, 0),
        ];
        let frags = run(&regs, t);
        assert!(frags.iter().all(|f| f.depth == 1 << 23));
    }
}
