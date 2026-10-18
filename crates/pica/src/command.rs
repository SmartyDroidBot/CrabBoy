//! The command processor: command lists and the internal register file
//! (3dbrew, "GPU/Internal Registers").
//!
//! A command is a parameter word and a header word: the register ID, a mask
//! of the parameter's four bytes, a count of further parameters, and whether
//! those go to the following registers or all to the same one. Commands are
//! padded to a whole number of word pairs. A write to `FINALIZE` ends the
//! list and asks for the P3D interrupt.
//!
//! Most registers only hold what is written, for the later stages to read.
//! The shader blocks (geometry at 0x280, vertex at 0x2B0) have FIFOs behind
//! them: code and operand descriptors go to an index that advances by itself,
//! and float uniforms arrive as three words of 24-bit floats or four of
//! single precision, last component first.

use crate::framebuffer::Framebuffer;
use crate::geometry::{Assembler, ScreenVertex, Setup, Topology};
use crate::memory::Memory;
use crate::pipeline::Pipeline;
use crate::raster::Raster;
use crate::regs::{self as reg, sh};
use crate::shader::{from_float24, Program, Uniforms, Unit, Vec4, ZERO};
use crate::vertex::{
    attribute_from_words, semantic, shader_inputs, Attributes, Loader, OutputVertex,
};
use softfloat::F32;

pub const REGISTERS: usize = reg::COUNT;

/// Words of shader code and of operand descriptors a unit holds: the index
/// registers are twelve bits wide.
const BANK: usize = 0x1000;

/// One of the two shader blocks: its program, its uniforms and the state of
/// the three upload FIFOs.
#[derive(Clone)]
pub struct ShaderSetup {
    pub program: Program,
    pub uniforms: Uniforms,
    pub entry: u32,
    float_index: usize,
    float_single: bool,
    float_words: Vec<u32>,
    code_index: usize,
    opdesc_index: usize,
}

impl Default for ShaderSetup {
    fn default() -> Self {
        ShaderSetup {
            program: Program {
                code: vec![0; BANK],
                operands: vec![0; BANK],
            },
            uniforms: Uniforms::default(),
            entry: 0,
            float_index: 0,
            float_single: false,
            float_words: Vec::with_capacity(4),
            code_index: 0,
            opdesc_index: 0,
        }
    }
}

impl ShaderSetup {
    /// A write to the block at `offset`; `value` is the register after
    /// masking.
    fn write(&mut self, offset: u32, value: u32) {
        match offset {
            sh::BOOLUNIFORM => self.uniforms.boolean = value as u16,
            sh::INTUNIFORM_I0..=sh::INTUNIFORM_I3 => {
                self.uniforms.int[(offset - sh::INTUNIFORM_I0) as usize] = value.to_le_bytes();
            }
            sh::ENTRYPOINT => self.entry = value & 0xFFFF,
            sh::FLOATUNIFORM_INDEX => {
                self.float_index = (value & 0xFF) as usize;
                self.float_single = value >> 31 != 0;
                self.float_words.clear();
            }
            sh::FLOATUNIFORM_DATA0..=sh::FLOATUNIFORM_DATA7 => {
                self.float_words.push(value);
                let needed = if self.float_single { 4 } else { 3 };
                if self.float_words.len() == needed {
                    let vector = self.take_vector();
                    if let Some(slot) = self.uniforms.float.get_mut(self.float_index) {
                        *slot = vector;
                    }
                    self.float_index += 1;
                }
            }
            sh::CODETRANSFER_INDEX => self.code_index = (value & 0xFFF) as usize,
            sh::CODETRANSFER_DATA0..=sh::CODETRANSFER_DATA7 => {
                if let Some(slot) = self.program.code.get_mut(self.code_index) {
                    *slot = value;
                }
                self.code_index += 1;
            }
            sh::OPDESCS_INDEX => self.opdesc_index = (value & 0xFFF) as usize,
            sh::OPDESCS_DATA0..=sh::OPDESCS_DATA7 => {
                if let Some(slot) = self.program.operands.get_mut(self.opdesc_index) {
                    *slot = value;
                }
                self.opdesc_index += 1;
            }
            _ => {}
        }
    }

    /// The uniform the FIFO has collected: w arrives first, x last.
    fn take_vector(&mut self) -> Vec4 {
        let w = std::mem::take(&mut self.float_words);
        if self.float_single {
            // Single precision is taken as it is; subnormals and negative
            // zero are dealt with when a shader reads the value.
            [F32(w[3]), F32(w[2]), F32(w[1]), F32(w[0])]
        } else {
            // ZZWWWWWW, YYYYZZZZ, XXXXXXYY.
            [
                from_float24(w[2] >> 8),
                from_float24((w[2] & 0xFF) << 16 | w[1] >> 16),
                from_float24((w[1] & 0xFFFF) << 8 | w[0] >> 24),
                from_float24(w[0] & 0xFF_FFFF),
            ]
        }
    }
}

/// What ended a command list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListEnd {
    /// `FINALIZE` was written: the P3D interrupt is due.
    Finalized,
    /// The words ran out first; the hardware would hang here.
    Exhausted,
    /// A jump register was written: continue with the buffer of that index,
    /// whose address and size registers the caller reads.
    Jump(usize),
}

/// Work the pipeline has done, for cost models and diagnostics.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Stats {
    pub vertices: u64,
    pub triangles: u64,
    pub fragments: u64,
}

pub struct Gpu {
    pub regs: [u32; REGISTERS],
    pub geometry: ShaderSetup,
    pub vertex: ShaderSetup,
    /// The fixed attribute values.
    pub fixed: Attributes,
    /// Words written to `FIXEDATTRIB_DATA0..2` since the last whole value.
    fixed_words: Vec<u32>,
    /// The attributes of the immediate-mode vertex being sent.
    immediate: Vec<Vec4>,
    assembler: Assembler,
    /// Vertex shader outputs waiting for the geometry shader.
    gs_inputs: Vec<Vec4>,
    /// The three vertex slots the geometry shader emits into.
    gs_vertices: [OutputVertex; 3],
    pub stats: Stats,
}

impl Default for Gpu {
    fn default() -> Self {
        let blank = OutputVertex {
            attrs: [ZERO; semantic::COUNT],
        };
        Gpu {
            regs: [0; REGISTERS],
            geometry: ShaderSetup::default(),
            vertex: ShaderSetup::default(),
            fixed: [[ZERO; 4]; 12],
            fixed_words: Vec::with_capacity(3),
            immediate: Vec::with_capacity(16),
            assembler: Assembler::default(),
            gs_inputs: Vec::with_capacity(16),
            gs_vertices: [blank; 3],
            stats: Stats::default(),
        }
    }
}

impl Gpu {
    pub fn new() -> Self {
        Self::default()
    }

    /// The physical address and length in bytes of command buffer `index`.
    pub fn command_buffer(&self, index: usize) -> (u32, u32) {
        let size = self.regs[reg::CMDBUF_SIZE0 as usize + index] & 0x1F_FFFF;
        let addr = self.regs[reg::CMDBUF_ADDR0 as usize + index] & 0x1FFF_FFFF;
        (addr << 3, size << 3)
    }

    /// Write the bytes of `value` that the four-bit `mask` selects to
    /// register `id`. A write that triggers work (a draw) does it at once,
    /// reading and writing `mem`.
    pub fn write_register(&mut self, id: u32, value: u32, mask: u32, mem: &mut dyn Memory) {
        let Some(slot) = self.regs.get_mut(id as usize) else {
            return;
        };
        let bits = (0..4)
            .filter(|byte| mask & 1 << byte != 0)
            .fold(0u32, |bits, byte| bits | 0xFF << (byte * 8));
        *slot = *slot & !bits | value & bits;
        let value = *slot;
        match id {
            reg::DRAWARRAYS => self.draw(false, mem),
            reg::DRAWELEMENTS => self.draw(true, mem),
            reg::RESTART_PRIMITIVE => self.assembler.restart(),
            reg::FIXEDATTRIB_INDEX => {
                self.fixed_words.clear();
                self.immediate.clear();
            }
            reg::FIXEDATTRIB_DATA0..=reg::FIXEDATTRIB_DATA2 => {
                self.fixed_words.push(value);
                if self.fixed_words.len() == 3 {
                    let words = [
                        self.fixed_words[0],
                        self.fixed_words[1],
                        self.fixed_words[2],
                    ];
                    self.fixed_words.clear();
                    self.fixed_attribute(attribute_from_words(words), mem);
                }
            }
            reg::GSH..=0x2AF => self.geometry.write(id - reg::GSH, value),
            reg::VSH..=0x2DF => self.vertex.write(id - reg::VSH, value),
            _ => {}
        }
    }

    /// A whole value from `FIXEDATTRIB_DATA0..2`: a fixed attribute, or with
    /// index 0xF the next attribute of an immediate-mode vertex, which is
    /// drawn once it has all `VSH_NUM_ATTR` attributes.
    fn fixed_attribute(&mut self, value: Vec4, mem: &mut dyn Memory) {
        let index = (self.regs[reg::FIXEDATTRIB_INDEX as usize] & 0xF) as usize;
        if index != 0xF {
            if let Some(slot) = self.fixed.get_mut(index) {
                *slot = value;
            }
            return;
        }
        self.immediate.push(value);
        let count = (self.regs[reg::VSH_NUM_ATTR as usize] & 0xF) as usize + 1;
        if self.immediate.len() >= count {
            let vertex = self.shade(&self.immediate, count);
            self.immediate.clear();
            self.submit(vertex, mem);
        }
    }

    /// `DRAWARRAYS` or `DRAWELEMENTS`: `NUMVERTICES` vertices through the
    /// vertex shader and on to the primitive assembler.
    fn draw(&mut self, indexed: bool, mem: &mut dyn Memory) {
        let regs = self.regs;
        let loader = Loader::new(&regs);
        let count = loader.attribute_count();
        self.assembler.restart();
        for n in 0..regs[reg::NUMVERTICES as usize] {
            let index = loader.index(n, indexed, mem);
            let attributes = loader.load(index, &self.fixed, mem);
            let vertex = self.shade(&attributes, count);
            self.submit(vertex, mem);
        }
    }

    /// Run the vertex shader on one vertex's attributes: its output
    /// registers.
    pub fn shade(&self, attributes: &[Vec4], count: usize) -> [Vec4; 16] {
        let mut unit = Unit {
            inputs: shader_inputs(&self.regs, reg::VSH, attributes, count),
            ..Unit::default()
        };
        unit.run(
            &self.vertex.program,
            &self.vertex.uniforms,
            self.vertex.entry,
        );
        unit.outputs
    }

    fn reg(&self, id: u32) -> u32 {
        self.regs[id as usize]
    }

    /// A shaded vertex enters primitive assembly, or with a geometry shader
    /// in use (`GEOSTAGE_CONFIG` mode 2) that shader's input buffer: the
    /// enabled outputs in order, until it holds as many as the shader takes.
    fn submit(&mut self, outputs: [Vec4; 16], mem: &mut dyn Memory) {
        self.stats.vertices += 1;
        let mask = self.reg(reg::VSH + sh::OUTMAP_MASK);
        if self.reg(reg::GEOSTAGE_CONFIG) & 3 == 2 {
            let enabled = (0..16).filter(|r| mask >> r & 1 != 0);
            self.gs_inputs.extend(enabled.map(|r| outputs[r]));
            let needed = (self.reg(reg::GSH + sh::INPUTBUFFER_CONFIG) & 0xF) as usize + 1;
            if self.gs_inputs.len() >= needed {
                self.run_geometry_shader(needed, mem);
                self.gs_inputs.clear();
            }
            return;
        }
        let vertex = OutputVertex::from_outputs(&self.regs, mask, &outputs);
        let topology = Topology::from_register(self.reg(reg::PRIMITIVE_CONFIG));
        if let Some(triangle) = self.assembler.push(topology, vertex) {
            self.triangle(triangle, mem);
        }
    }

    /// Run the geometry shader on its input buffer. Each `EMIT` stores a
    /// vertex in the slot `SETEMIT` chose; one that also ends a primitive
    /// draws the three slots, the first two swapped for the other winding.
    fn run_geometry_shader(&mut self, count: usize, mem: &mut dyn Memory) {
        let mut unit = Unit {
            inputs: shader_inputs(&self.regs, reg::GSH, &self.gs_inputs, count),
            ..Unit::default()
        };
        unit.run(
            &self.geometry.program,
            &self.geometry.uniforms,
            self.geometry.entry,
        );
        let mask = self.reg(reg::GSH + sh::OUTMAP_MASK);
        for (emit, outputs) in std::mem::take(&mut unit.emitted) {
            let vertex = OutputVertex::from_outputs(&self.regs, mask, &outputs);
            self.gs_vertices[(emit.vertex as usize).min(2)] = vertex;
            if emit.primitive {
                let [a, b, c] = self.gs_vertices;
                let triangle = if emit.winding { [b, a, c] } else { [a, b, c] };
                self.triangle(triangle, mem);
            }
        }
    }

    /// Clip, project and cull a triangle, and rasterise what is left.
    fn triangle(&mut self, triangle: [OutputVertex; 3], mem: &mut dyn Memory) {
        let setup = Setup::new(&self.regs);
        let mut screen = Vec::with_capacity(4);
        setup.triangle(triangle, &mut |t| screen.push(t));
        for t in screen {
            self.rasterize(t, mem);
        }
    }

    fn rasterize(&mut self, triangle: [ScreenVertex; 3], mem: &mut dyn Memory) {
        let framebuffer = Framebuffer::new(&self.regs);
        let mut pipeline = Pipeline {
            framebuffer: &framebuffer,
            mem,
            written: 0,
        };
        self.stats.triangles += 1;
        self.stats.fragments += Raster::new(&self.regs).triangle(&triangle, &mut pipeline);
    }

    /// Carry out a command list.
    pub fn run_list(&mut self, words: &[u32], mem: &mut dyn Memory) -> ListEnd {
        let mut at = 0;
        while at + 1 < words.len() {
            let header = words[at + 1];
            let mut id = header & 0xFFFF;
            let mask = header >> 16 & 0xF;
            let extra = (header >> 20 & 0xFF) as usize;
            let consecutive = header >> 31 != 0;
            let parameters = std::iter::once(words[at]).chain(
                words
                    .get(at + 2..)
                    .unwrap_or(&[])
                    .iter()
                    .copied()
                    .take(extra),
            );
            for value in parameters {
                self.write_register(id, value, mask, mem);
                match id {
                    reg::FINALIZE => return ListEnd::Finalized,
                    reg::CMDBUF_JUMP0 | reg::CMDBUF_JUMP1 => {
                        return ListEnd::Jump((id - reg::CMDBUF_JUMP0) as usize)
                    }
                    _ => {}
                }
                if consecutive {
                    id += 1;
                }
            }
            // The header pair, the extra words, and padding to a pair.
            at += 2 + extra + (extra & 1);
        }
        ListEnd::Exhausted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::VecMemory;
    use crate::shader::{Unit, ONE};

    fn header(id: u32, mask: u32, extra: u32, consecutive: bool) -> u32 {
        id | mask << 16 | extra << 20 | (consecutive as u32) << 31
    }

    #[test]
    fn the_documented_example_writes_three_registers() {
        let mut gpu = Gpu::new();
        let list = [0xAAAA_AAAA, 0x802F_011C, 0xBBBB_BBBB, 0xCCCC_CCCC];
        assert_eq!(
            gpu.run_list(&list, &mut VecMemory::default()),
            ListEnd::Exhausted
        );
        assert_eq!(
            gpu.regs[0x11C..0x11F],
            [0xAAAA_AAAA, 0xBBBB_BBBB, 0xCCCC_CCCC]
        );

        // Without consecutive mode they all land on the first.
        let mut gpu = Gpu::new();
        let list = [0xAAAA_AAAA, 0x002F_011C, 0xBBBB_BBBB, 0xCCCC_CCCC];
        gpu.run_list(&list, &mut VecMemory::default());
        assert_eq!(gpu.regs[0x11C..0x11F], [0xCCCC_CCCC, 0, 0]);
    }

    #[test]
    fn the_mask_selects_bytes_and_odd_extras_are_padded() {
        let mut gpu = Gpu::new();
        let list = [
            0x1122_3344,
            header(0x100, 0xF, 0, false),
            0xAABB_CCDD,
            header(0x100, 0b0101, 1, true),
            0x5566_7788,
            0xDEAD_BEEF, // padding
            0x0000_0001,
            header(0x102, 0xF, 0, false),
        ];
        gpu.run_list(&list, &mut VecMemory::default());
        assert_eq!(gpu.regs[0x100], 0x11BB_33DD);
        assert_eq!(gpu.regs[0x101], 0x0066_0088);
        assert_eq!(gpu.regs[0x102], 1);
    }

    #[test]
    fn finalize_ends_the_list_and_a_jump_names_its_buffer() {
        let mut gpu = Gpu::new();
        let list = [
            0x1234_5678,
            header(reg::FINALIZE, 0xF, 0, false),
            7,
            header(0x100, 0xF, 0, false),
        ];
        assert_eq!(
            gpu.run_list(&list, &mut VecMemory::default()),
            ListEnd::Finalized
        );
        assert_eq!(gpu.regs[0x100], 0, "nothing runs after FINALIZE");

        let list = [
            0x0010_0000 >> 3,
            header(reg::CMDBUF_ADDR0 + 1, 0xF, 0, false),
            0x40 >> 3,
            header(reg::CMDBUF_SIZE0 + 1, 0xF, 0, false),
            1,
            header(reg::CMDBUF_JUMP0 + 1, 0xF, 0, false),
        ];
        assert_eq!(
            gpu.run_list(&list, &mut VecMemory::default()),
            ListEnd::Jump(1)
        );
        assert_eq!(gpu.command_buffer(1), (0x0010_0000, 0x40));
    }

    #[test]
    fn a_vertex_shader_is_uploaded_and_runs() {
        // mov o0, c5 ; end, with c5 sent as 24-bit floats and c6 as singles.
        let mov = 0x13 << 26 | 0x25 << 12;
        let end = 0x22 << 26;
        let plain = 0x1B << 5 | 0xF;
        // (x, y, z, w) = (1.0, -1.0, 3.0, 0.5) in the 24-bit format.
        let (x, y, z, w) = (0x3F_0000u32, 0xBF_0000u32, 0x40_8000u32, 0x3E_0000u32);
        let list = [
            0,
            header(reg::VSH + sh::CODETRANSFER_INDEX, 0xF, 2, true),
            mov,
            end,
            0,
            header(reg::VSH + sh::OPDESCS_INDEX, 0xF, 1, true),
            plain,
            0xDEAD_BEEF, // padding
            5,
            header(reg::VSH + sh::FLOATUNIFORM_INDEX, 0xF, 3, true),
            z << 24 | w,
            y << 16 | z >> 8,
            x << 8 | y >> 16,
            0xDEAD_BEEF, // padding
            1 << 31 | 6,
            header(reg::VSH + sh::FLOATUNIFORM_INDEX, 0xF, 4, true),
            2.0f32.to_bits(),
            0.0f32.to_bits(),
            0.0f32.to_bits(),
            4.0f32.to_bits(),
            0x0000_0B0B,
            header(reg::VSH + sh::BOOLUNIFORM, 0xF, 0, false),
            0x0403_0201,
            header(reg::VSH + 2, 0xF, 0, false),
            0,
            header(reg::VSH + sh::ENTRYPOINT, 0xF, 0, false),
            1,
            header(reg::FINALIZE, 0xF, 0, false),
        ];
        let mut gpu = Gpu::new();
        assert_eq!(
            gpu.run_list(&list, &mut VecMemory::default()),
            ListEnd::Finalized
        );
        let vertex = &gpu.vertex;
        assert_eq!(vertex.program.code[..2], [mov, end]);
        assert_eq!(vertex.uniforms.boolean, 0x0B0B);
        assert_eq!(vertex.uniforms.int[1], [1, 2, 3, 4]);
        let single = |v: f32| F32(v.to_bits());
        assert_eq!(vertex.uniforms.float[6], [4.0, 0.0, 0.0, 2.0].map(single));

        let mut unit = Unit::default();
        unit.run(&vertex.program, &vertex.uniforms, vertex.entry);
        assert_eq!(
            unit.outputs[0],
            [ONE, single(-1.0), single(3.0), single(0.5)]
        );
        assert_eq!(
            gpu.geometry.uniforms.boolean, 0,
            "the geometry block is separate"
        );
    }

    /// A vertex shader `mov o0, v0 ; mov o1, v1 ; end`, o0 the position and
    /// o1 the colour, reading attribute 0 into v0 and 1 into v1.
    fn passthrough(gpu: &mut Gpu) {
        let mem = &mut VecMemory::default();
        let mov = |dst: u32, src: u32| 0x13 << 26 | dst << 21 | src << 12;
        let code = [mov(0, 0), mov(1, 1), 0x22 << 26];
        gpu.write_register(reg::VSH + sh::CODETRANSFER_INDEX, 0, 0xF, mem);
        for word in code {
            gpu.write_register(reg::VSH + sh::CODETRANSFER_DATA0, word, 0xF, mem);
        }
        gpu.write_register(reg::VSH + sh::OPDESCS_INDEX, 0, 0xF, mem);
        gpu.write_register(reg::VSH + sh::OPDESCS_DATA0, 0x1B << 5 | 0xF, 0xF, mem);
        gpu.write_register(reg::VSH + sh::ATTRIBUTES_PERMUTATION_LOW, 0x10, 0xF, mem);
        gpu.write_register(reg::VSH + sh::OUTMAP_MASK, 0b11, 0xF, mem);
        gpu.write_register(reg::SH_OUTMAP_TOTAL, 2, 0xF, mem);
        gpu.write_register(reg::SH_OUTMAP_O0, 0x0302_0100, 0xF, mem);
        gpu.write_register(reg::SH_OUTMAP_O0 + 1, 0x0B0A_0908, 0xF, mem);
    }

    #[test]
    fn a_draw_shades_each_vertex_of_its_buffers() {
        let mut gpu = Gpu::new();
        passthrough(&mut gpu);
        let mut mem = VecMemory::new(0x1800_0000, 0x100);
        // Three vertices of two unsigned-byte pairs each, four bytes apart.
        for (n, bytes) in [[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12]]
            .iter()
            .enumerate()
        {
            mem.bytes[n * 4..n * 4 + 4].copy_from_slice(bytes);
        }
        let m = &mut mem;
        gpu.write_register(reg::ATTRIBBUFFERS_LOC, 0x1800_0000 >> 3, 0xF, m);
        gpu.write_register(reg::ATTRIBBUFFERS_FORMAT_LOW, 0x55, 0xF, m);
        gpu.write_register(reg::ATTRIBBUFFERS_FORMAT_HIGH, 1 << 28, 0xF, m);
        gpu.write_register(reg::ATTRIBBUFFER0_OFFSET + 1, 0x10, 0xF, m);
        gpu.write_register(reg::ATTRIBBUFFER0_OFFSET + 2, 2 << 28 | 4 << 16, 0xF, m);
        gpu.write_register(reg::NUMVERTICES, 3, 0xF, m);
        gpu.write_register(reg::DRAWARRAYS, 1, 0xF, m);
        assert_eq!(gpu.stats.vertices, 3);

        let loader = Loader::new(&gpu.regs);
        let attributes = loader.load(2, &gpu.fixed, &mem);
        let outputs = gpu.shade(&attributes, 2);
        let vertex = OutputVertex::from_outputs(&gpu.regs, 0b11, &outputs);
        let f = |v: f32| F32(v.to_bits());
        assert_eq!(
            vertex.position(),
            [f(9.0), f(10.0), ZERO, crate::shader::ONE]
        );
        assert_eq!(vertex.attrs[8..10], [f(11.0), f(12.0)]);
    }

    #[test]
    fn immediate_mode_draws_a_vertex_per_whole_set_of_attributes() {
        let mut gpu = Gpu::new();
        passthrough(&mut gpu);
        let m = &mut VecMemory::default();
        gpu.write_register(reg::VSH_NUM_ATTR, 1, 0xF, m);
        gpu.write_register(reg::FIXEDATTRIB_INDEX, 0xF, 0xF, m);
        for _ in 0..5 {
            for word in [0x3E00_0040, 0x8000_BF00, 0x003F_0000] {
                gpu.write_register(reg::FIXEDATTRIB_DATA0, word, 0xF, m);
            }
        }
        assert_eq!(gpu.stats.vertices, 2, "the fifth attribute waits");

        // A fixed attribute is stored, not drawn.
        gpu.write_register(reg::FIXEDATTRIB_INDEX, 3, 0xF, m);
        for (n, word) in [0x3E00_0040, 0x8000_BF00, 0x003F_0000]
            .into_iter()
            .enumerate()
        {
            gpu.write_register(reg::FIXEDATTRIB_DATA0 + n as u32, word, 0xF, m);
        }
        assert_eq!(gpu.stats.vertices, 2);
        assert_eq!(gpu.fixed[3][0], crate::shader::ONE);
    }

    /// `v` in the 24-bit float format (truncated; zero for zero).
    fn f24(v: f32) -> u32 {
        let bits = v.to_bits();
        if bits & 0x7FFF_FFFF == 0 {
            return 0;
        }
        let exponent = (bits >> 23 & 0xFF) + 63 - 127;
        (bits >> 31) << 23 | exponent << 16 | (bits >> 7 & 0xFFFF)
    }

    /// Send one immediate-mode attribute.
    fn send(gpu: &mut Gpu, v: [f32; 4], mem: &mut dyn Memory) {
        let [x, y, z, w] = v.map(f24);
        for word in [
            w << 8 | z >> 16,
            (z & 0xFFFF) << 16 | y >> 8,
            (y & 0xFF) << 24 | x,
        ] {
            gpu.write_register(reg::FIXEDATTRIB_DATA0, word, 0xF, mem);
        }
    }

    /// The pass-through shader, a 240x400 viewport and immediate mode with
    /// two attributes a vertex.
    fn immediate_setup(gpu: &mut Gpu, mem: &mut dyn Memory) {
        passthrough(gpu);
        gpu.write_register(reg::VIEWPORT_WIDTH, f24(120.0), 0xF, mem);
        gpu.write_register(reg::VIEWPORT_HEIGHT, f24(200.0), 0xF, mem);
        gpu.write_register(reg::VSH_NUM_ATTR, 1, 0xF, mem);
        gpu.write_register(reg::FIXEDATTRIB_INDEX, 0xF, 0xF, mem);
    }

    #[test]
    fn a_strip_of_four_vertices_is_two_triangles() {
        let mut gpu = Gpu::new();
        let m = &mut VecMemory::default();
        immediate_setup(&mut gpu, m);
        gpu.write_register(reg::PRIMITIVE_CONFIG, 1 << 8, 0xF, m);
        for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            send(&mut gpu, [x, y, -0.5, 1.0], m);
            send(&mut gpu, [1.0, 0.0, 0.0, 1.0], m);
        }
        assert_eq!((gpu.stats.vertices, gpu.stats.triangles), (4, 2));
        // A restart drops the strip so far.
        gpu.write_register(reg::RESTART_PRIMITIVE, 1, 0xF, m);
        send(&mut gpu, [0.0, 0.0, -0.5, 1.0], m);
        send(&mut gpu, [1.0; 4], m);
        assert_eq!(gpu.stats.triangles, 2);
    }

    #[test]
    fn a_full_screen_quad_fills_the_colour_buffer() {
        let mut gpu = Gpu::new();
        let mut mem = VecMemory::new(0x1800_0000, 0x1000);
        let m = &mut mem;
        immediate_setup(&mut gpu, m);
        gpu.write_register(reg::VIEWPORT_WIDTH, f24(4.0), 0xF, m);
        gpu.write_register(reg::VIEWPORT_HEIGHT, f24(4.0), 0xF, m);
        gpu.write_register(reg::FRAMEBUFFER_DIM, 7 << 12 | 8, 0xF, m);
        gpu.write_register(reg::COLORBUFFER_LOC, 0x1800_0000 >> 3, 0xF, m);
        gpu.write_register(reg::COLORBUFFER_WRITE, 0xF, 0xF, m);
        gpu.write_register(reg::DEPTH_COLOR_MASK, 0xF << 8, 0xF, m);
        gpu.write_register(reg::COLOR_OPERATION, 1 << 8, 0xF, m);
        gpu.write_register(reg::BLEND_FUNC, 1 << 16 | 1 << 24, 0xF, m);
        gpu.write_register(reg::PRIMITIVE_CONFIG, 1 << 8, 0xF, m);
        for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            send(&mut gpu, [x, y, -0.5, 1.0], m);
            send(&mut gpu, [1.0, 0.5, 0.0, 1.0], m);
        }
        assert_eq!(gpu.stats.fragments, 64);
        // RGBA8 is stored alpha, blue, green, red.
        for pixel in mem.bytes[..256].chunks(4) {
            assert_eq!(pixel, [255, 0, 128, 255]);
        }
        assert_eq!(mem.bytes[256], 0);
    }

    #[test]
    fn a_geometry_shader_emits_triangles_from_its_inputs() {
        let mut gpu = Gpu::new();
        let m = &mut VecMemory::default();
        immediate_setup(&mut gpu, m);
        gpu.write_register(reg::GEOSTAGE_CONFIG, 2, 0xF, m);
        // Three vertices of one output (the position) per invocation.
        gpu.write_register(reg::VSH + sh::OUTMAP_MASK, 1, 0xF, m);
        gpu.write_register(reg::GSH + sh::INPUTBUFFER_CONFIG, 2, 0xF, m);
        gpu.write_register(reg::GSH + sh::ATTRIBUTES_PERMUTATION_LOW, 0x210, 0xF, m);
        gpu.write_register(reg::GSH + sh::OUTMAP_MASK, 1, 0xF, m);
        let mov = |src: u32| 0x13 << 26 | src << 12;
        let setemit = |vertex: u32, primitive: u32| 0x2B << 26 | vertex << 24 | primitive << 23;
        let emit = 0x2A << 26;
        let code = [
            setemit(0, 0),
            mov(0),
            emit,
            setemit(1, 0),
            mov(1),
            emit,
            setemit(2, 1),
            mov(2),
            emit,
            0x22 << 26,
        ];
        gpu.write_register(reg::GSH + sh::CODETRANSFER_INDEX, 0, 0xF, m);
        for word in code {
            gpu.write_register(reg::GSH + sh::CODETRANSFER_DATA0, word, 0xF, m);
        }
        gpu.write_register(reg::GSH + sh::OPDESCS_INDEX, 0, 0xF, m);
        gpu.write_register(reg::GSH + sh::OPDESCS_DATA0, 0x1B << 5 | 0xF, 0xF, m);
        for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0)] {
            send(&mut gpu, [x, y, -0.5, 1.0], m);
            send(&mut gpu, [1.0; 4], m);
        }
        assert_eq!((gpu.stats.vertices, gpu.stats.triangles), (3, 1));
    }

    #[test]
    fn the_uniform_fifo_moves_on_to_the_next_register() {
        let mut gpu = Gpu::new();
        gpu.write_register(
            reg::VSH + sh::FLOATUNIFORM_INDEX,
            94,
            0xF,
            &mut VecMemory::default(),
        );
        for _ in 0..3 {
            for word in [0x3F_0000, 0, 0] {
                gpu.write_register(reg::VSH + 0x11, word, 0xF, &mut VecMemory::default());
            }
        }
        // c94 and c95 are set; the third lands past the end and is dropped.
        assert_eq!(gpu.vertex.uniforms.float[94][3], ONE);
        assert_eq!(gpu.vertex.uniforms.float[95][3], ONE);
    }
}
