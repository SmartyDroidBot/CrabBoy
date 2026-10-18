//! From shaded vertices to screen-space triangles: primitive assembly,
//! clipping, the perspective divide, the viewport and depth map, and face
//! culling (3dbrew, "GPU/Internal Registers", rasteriser and geometry
//! pipeline registers).
//!
//! Clipping is done in clip space with single-precision floats. The screen
//! vertex it hands on is fixed point: see `docs/3ds/gpu.md`, "Fixed point".

use crate::num::{self, add, div, fixed, lerp, less, mul, sub};
use crate::regs as reg;
use crate::shader::{from_float24, ONE, ZERO};
use crate::vertex::{semantic, OutputVertex};
use softfloat::F32;

/// How vertices make triangles (`PRIMITIVE_CONFIG` bits 8-9).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Topology {
    List,
    Strip,
    Fan,
    /// Triangles as a geometry shader emits them; without one, a list.
    Geometry,
}

impl Topology {
    pub fn from_register(value: u32) -> Self {
        match value >> 8 & 3 {
            0 => Topology::List,
            1 => Topology::Strip,
            2 => Topology::Fan,
            _ => Topology::Geometry,
        }
    }
}

/// The primitive assembler: the vertices of the triangle being built.
#[derive(Clone, Default)]
pub struct Assembler {
    buffer: Vec<OutputVertex>,
    /// Which of the two kept vertices of a strip the next one replaces.
    strip_odd: bool,
}

impl Assembler {
    /// `RESTART_PRIMITIVE`: the next vertex starts a new strip or fan.
    pub fn restart(&mut self) {
        self.buffer.clear();
        self.strip_odd = false;
    }

    /// Add a vertex; a triangle when it completes one. A strip keeps its
    /// triangles' winding by replacing its two kept vertices in turn.
    pub fn push(&mut self, topology: Topology, vertex: OutputVertex) -> Option<[OutputVertex; 3]> {
        if self.buffer.len() < 2 {
            self.buffer.push(vertex);
            return None;
        }
        let triangle = [self.buffer[0], self.buffer[1], vertex];
        match topology {
            Topology::Strip => {
                self.buffer[self.strip_odd as usize] = vertex;
                self.strip_odd = !self.strip_odd;
            }
            Topology::Fan => self.buffer[1] = vertex,
            Topology::List | Topology::Geometry => self.buffer.clear(),
        }
        Some(triangle)
    }
}

/// The values the rasteriser interpolates, in fixed point with
/// [`VARYING_FRAC`] fractional bits, in this order.
pub mod varying {
    pub const COLOR: usize = 0;
    pub const TEXCOORD0: usize = 4;
    pub const TEXCOORD0_W: usize = 6;
    pub const TEXCOORD1: usize = 7;
    pub const TEXCOORD2: usize = 9;
    pub const QUATERNION: usize = 11;
    pub const VIEW: usize = 15;
    pub const COUNT: usize = 18;
}

/// Fractional bits of a varying.
pub const VARYING_FRAC: u32 = 16;
/// Fractional bits of screen coordinates: four subpixel bits.
pub const SUBPIXEL_BITS: u32 = 4;
/// Fractional bits of depth.
pub const DEPTH_FRAC: u32 = 24;

/// Where each varying comes from, by semantic.
const SOURCES: [usize; varying::COUNT] = [
    semantic::COLOR,
    semantic::COLOR + 1,
    semantic::COLOR + 2,
    semantic::COLOR + 3,
    semantic::TEXCOORD0,
    semantic::TEXCOORD0 + 1,
    semantic::TEXCOORD0_W,
    semantic::TEXCOORD1,
    semantic::TEXCOORD1 + 1,
    semantic::TEXCOORD2,
    semantic::TEXCOORD2 + 1,
    semantic::QUATERNION,
    semantic::QUATERNION + 1,
    semantic::QUATERNION + 2,
    semantic::QUATERNION + 3,
    semantic::VIEW,
    semantic::VIEW + 1,
    semantic::VIEW + 2,
];

/// A vertex after the viewport transform.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScreenVertex {
    /// Window coordinates, y up, with [`SUBPIXEL_BITS`] fractional bits.
    pub x: i32,
    pub y: i32,
    /// The depth map's output, `scale * z / w + offset`, with
    /// [`DEPTH_FRAC`] fractional bits. Not clamped.
    pub z: i64,
    /// `1 / w` of clip space.
    pub inv_w: F32,
    pub varyings: [i32; varying::COUNT],
}

/// What the clipper and viewport read from the registers.
pub struct Setup {
    half_width: F32,
    half_height: F32,
    x: i32,
    y: i32,
    depth_scale: F32,
    depth_offset: F32,
    user_plane: Option<[F32; 4]>,
    cull: u32,
    flat: bool,
}

/// The smallest `w` that survives clipping.
const W_EPSILON: F32 = F32(0x3780_0000); // 2^-16

impl Setup {
    pub fn new(regs: &[u32]) -> Self {
        let r = |id: u32| regs[id as usize];
        let float24 = |id: u32| from_float24(r(id) & 0xFF_FFFF);
        let xy = r(reg::VIEWPORT_XY);
        let (depth_scale, depth_offset) = if r(reg::DEPTHMAP_TRANSFORM_ENABLE) & 1 != 0 {
            (float24(reg::DEPTHMAP_SCALE), float24(reg::DEPTHMAP_OFFSET))
        } else {
            (num::sub(ZERO, ONE), ZERO)
        };
        Setup {
            half_width: float24(reg::VIEWPORT_WIDTH),
            half_height: float24(reg::VIEWPORT_HEIGHT),
            x: num::signed(xy & 0x3FF, 10),
            y: num::signed(xy >> 16 & 0x3FF, 10),
            depth_scale,
            depth_offset,
            user_plane: (r(reg::FRAGOP_CLIP) & 1 != 0)
                .then(|| std::array::from_fn(|i| float24(reg::FRAGOP_CLIP_DATA0 + i as u32))),
            cull: r(reg::FACECULLING_CONFIG) & 3,
            flat: r(reg::FLAT_SHADING) & 1 != 0,
        }
    }

    /// Clip a triangle, project it, cull it, and hand each triangle of what
    /// is left to `out`. Returns how many triangles that was.
    pub fn triangle(
        &self,
        mut t: [OutputVertex; 3],
        out: &mut dyn FnMut([ScreenVertex; 3]),
    ) -> u32 {
        if self.flat {
            let colour = &t[2].attrs[semantic::COLOR..semantic::COLOR + 4];
            let colour: [F32; 4] = [colour[0], colour[1], colour[2], colour[3]];
            for v in &mut t[..2] {
                v.attrs[semantic::COLOR..semantic::COLOR + 4].copy_from_slice(&colour);
            }
        }
        let polygon = self.clip(&t);
        if polygon.len() < 3 {
            return 0;
        }
        let screen: Vec<ScreenVertex> = polygon.iter().map(|v| self.project(v)).collect();
        let mut drawn = 0;
        for i in 1..screen.len() - 1 {
            let triangle = [screen[0], screen[i], screen[i + 1]];
            let area = signed_area(&triangle);
            let culled = match self.cull {
                1 => area > 0,
                2 => area < 0,
                _ => false,
            };
            if area != 0 && !culled {
                out(triangle);
                drawn += 1;
            }
        }
        drawn
    }

    /// The signed distances of a vertex to the clip planes, positive inside.
    fn distances(&self, v: &OutputVertex) -> [F32; 8] {
        let [x, y, z, w] = v.position();
        let user = self.user_plane.map_or(ONE, |p| {
            add(
                add(mul(p[0], x), mul(p[1], y)),
                add(mul(p[2], z), mul(p[3], w)),
            )
        });
        [
            sub(w, x),
            add(w, x),
            sub(w, y),
            add(w, y),
            num::sub(ZERO, z),
            add(w, z),
            sub(w, W_EPSILON),
            user,
        ]
    }

    /// Sutherland-Hodgman against each plane in turn; every attribute is
    /// interpolated at the crossing.
    fn clip(&self, t: &[OutputVertex; 3]) -> Vec<OutputVertex> {
        let inside = |d: F32| !less(d, ZERO) && !d.is_nan();
        let mut polygon = t.to_vec();
        if t.iter().all(|v| self.distances(v).into_iter().all(inside)) {
            return polygon;
        }
        for plane in 0..8 {
            if polygon.is_empty() {
                break;
            }
            let mut next = Vec::with_capacity(polygon.len() + 1);
            for i in 0..polygon.len() {
                let a = &polygon[i];
                let b = &polygon[(i + 1) % polygon.len()];
                let (da, db) = (self.distances(a)[plane], self.distances(b)[plane]);
                if inside(da) {
                    next.push(*a);
                }
                if inside(da) != inside(db) {
                    let t = div(da, sub(da, db));
                    next.push(OutputVertex {
                        attrs: std::array::from_fn(|k| lerp(a.attrs[k], b.attrs[k], t)),
                    });
                }
            }
            polygon = next;
        }
        polygon
    }

    /// The perspective divide, the viewport and the depth map.
    fn project(&self, v: &OutputVertex) -> ScreenVertex {
        let [x, y, z, w] = v.position();
        let inv_w = div(ONE, w);
        let screen = |c: F32, half: F32, corner: i32| {
            let s = mul(add(mul(c, inv_w), ONE), half);
            fixed(s, SUBPIXEL_BITS).saturating_add(corner << SUBPIXEL_BITS)
        };
        let depth = add(mul(mul(z, inv_w), self.depth_scale), self.depth_offset);
        let varyings = std::array::from_fn(|i| {
            let value = v.attrs[SOURCES[i]];
            let value = if i < 4 { clamp01(value) } else { value };
            fixed(value, VARYING_FRAC)
        });
        ScreenVertex {
            x: screen(x, self.half_width, self.x),
            y: screen(y, self.half_height, self.y),
            z: depth.to_fixed(DEPTH_FRAC, softfloat::Round::Nearest),
            inv_w,
            varyings,
        }
    }
}

fn clamp01(v: F32) -> F32 {
    if less(v, ZERO) || v.is_nan() {
        ZERO
    } else if less(ONE, v) {
        ONE
    } else {
        v
    }
}

/// Twice the signed area in subpixels squared: positive when the vertices
/// run counter-clockwise (y up).
pub fn signed_area(t: &[ScreenVertex; 3]) -> i64 {
    let (x0, y0) = (t[0].x as i64, t[0].y as i64);
    (t[1].x as i64 - x0) * (t[2].y as i64 - y0) - (t[2].x as i64 - x0) * (t[1].y as i64 - y0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(v: f32) -> F32 {
        F32(v.to_bits())
    }

    fn vertex(x: f32, y: f32, z: f32, w: f32) -> OutputVertex {
        let mut attrs = [ONE; semantic::COUNT];
        attrs[..4].copy_from_slice(&[f(x), f(y), f(z), f(w)]);
        OutputVertex { attrs }
    }

    /// A 240x400 viewport at the origin with the default depth map.
    fn regs() -> Vec<u32> {
        let mut regs = vec![0u32; reg::COUNT];
        regs[reg::VIEWPORT_WIDTH as usize] = 0x45_E000; // 120
        regs[reg::VIEWPORT_HEIGHT as usize] = 0x46_9000; // 200
        regs
    }

    fn run(setup: &Setup, t: [OutputVertex; 3]) -> Vec<[ScreenVertex; 3]> {
        let mut out = Vec::new();
        setup.triangle(t, &mut |s| out.push(s));
        out
    }

    #[test]
    fn strips_keep_their_winding_and_fans_their_centre() {
        let v: Vec<OutputVertex> = (0..5).map(|n| vertex(n as f32, 0.0, 0.0, 1.0)).collect();
        let x = |t: [OutputVertex; 3]| t.map(|v| v.attrs[0]);
        let mut a = Assembler::default();
        let strip: Vec<_> = v
            .iter()
            .filter_map(|&v| a.push(Topology::Strip, v))
            .map(x)
            .collect();
        let n = |i: f32| f(i);
        assert_eq!(
            strip,
            [
                [n(0.0), n(1.0), n(2.0)],
                [n(2.0), n(1.0), n(3.0)],
                [n(2.0), n(3.0), n(4.0)]
            ]
        );
        a.restart();
        let fan: Vec<_> = v
            .iter()
            .filter_map(|&v| a.push(Topology::Fan, v))
            .map(x)
            .collect();
        assert_eq!(fan[2], [n(0.0), n(3.0), n(4.0)]);
        a.restart();
        let list: Vec<_> = v
            .iter()
            .filter_map(|&v| a.push(Topology::List, v))
            .collect();
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn the_viewport_maps_clip_space_to_window_subpixels() {
        let setup = Setup::new(&regs());
        let t = [
            vertex(-1.0, -1.0, 0.0, 1.0),
            vertex(1.0, -1.0, -0.5, 1.0),
            vertex(0.0, 2.0, -2.0, 2.0),
        ];
        let out = run(&setup, t);
        assert_eq!(out.len(), 1);
        let [a, b, c] = out[0];
        assert_eq!((a.x, a.y, a.z), (0, 0, 0));
        assert_eq!((b.x, b.y, b.z), (240 * 16, 0, 1 << 23));
        assert_eq!((c.x, c.y, c.z), (120 * 16, 400 * 16, 1 << 24));
        assert_eq!(c.inv_w, f(0.5));
        assert_eq!(a.varyings[varying::COLOR], 1 << 16);
    }

    #[test]
    fn a_triangle_through_the_near_plane_is_cut_in_two() {
        let setup = Setup::new(&regs());
        // One vertex in front of the near plane (z > 0).
        let t = [
            vertex(-0.5, -0.5, -0.5, 1.0),
            vertex(0.5, -0.5, -0.5, 1.0),
            vertex(0.0, 0.5, 0.5, 1.0),
        ];
        let out = run(&setup, t);
        assert_eq!(out.len(), 2, "a quadrilateral");
        for t in &out {
            for v in t {
                assert!(v.z >= 0 && v.z <= 1 << 23);
            }
        }
        // Wholly outside: nothing.
        let t = [
            vertex(2.0, 0.0, -0.5, 1.0),
            vertex(3.0, 0.0, -0.5, 1.0),
            vertex(2.0, 1.0, -0.5, 1.0),
        ];
        assert!(run(&setup, t).is_empty());
    }

    #[test]
    fn culling_drops_one_winding() {
        let mut regs = regs();
        let ccw = [
            vertex(-0.5, -0.5, -0.5, 1.0),
            vertex(0.5, -0.5, -0.5, 1.0),
            vertex(0.0, 0.5, -0.5, 1.0),
        ];
        let cw = [ccw[0], ccw[2], ccw[1]];
        regs[reg::FACECULLING_CONFIG as usize] = 1;
        let setup = Setup::new(&regs);
        assert_eq!((run(&setup, ccw).len(), run(&setup, cw).len()), (0, 1));
        regs[reg::FACECULLING_CONFIG as usize] = 2;
        let setup = Setup::new(&regs);
        assert_eq!((run(&setup, ccw).len(), run(&setup, cw).len()), (1, 0));
    }

    #[test]
    fn flat_shading_takes_the_last_colour_and_colours_clamp() {
        let mut regs = regs();
        regs[reg::FLAT_SHADING as usize] = 1;
        let setup = Setup::new(&regs);
        let mut t = [
            vertex(-0.5, -0.5, -0.5, 1.0),
            vertex(0.5, -0.5, -0.5, 1.0),
            vertex(0.0, 0.5, -0.5, 1.0),
        ];
        t[2].attrs[semantic::COLOR] = f(0.5);
        t[0].attrs[semantic::COLOR + 1] = f(-3.0);
        t[2].attrs[semantic::COLOR + 1] = f(7.0);
        let out = run(&setup, t);
        for v in out[0] {
            assert_eq!(v.varyings[varying::COLOR], 1 << 15);
            assert_eq!(v.varyings[varying::COLOR + 1], 1 << 16);
        }
    }
}
