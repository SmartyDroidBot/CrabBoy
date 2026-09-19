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

use crate::shader::{from_float24, Program, Uniforms, Vec4};
use softfloat::F32;

pub const REGISTERS: usize = 0x300;

/// Register IDs.
pub mod reg {
    pub const FINALIZE: u32 = 0x010;
    pub const CMDBUF_SIZE0: u32 = 0x238;
    pub const CMDBUF_ADDR0: u32 = 0x23A;
    pub const CMDBUF_JUMP0: u32 = 0x23C;
    pub const GSH: u32 = 0x280;
    pub const VSH: u32 = 0x2B0;
}

/// Offsets within a shader block.
mod sh {
    pub const BOOL: u32 = 0x00;
    pub const INT: std::ops::RangeInclusive<u32> = 0x01..=0x04;
    pub const ENTRY: u32 = 0x0A;
    pub const FLOAT_INDEX: u32 = 0x10;
    pub const FLOAT_DATA: std::ops::RangeInclusive<u32> = 0x11..=0x18;
    pub const CODE_INDEX: u32 = 0x1B;
    pub const CODE_DATA: std::ops::RangeInclusive<u32> = 0x1C..=0x23;
    pub const OPDESC_INDEX: u32 = 0x25;
    pub const OPDESC_DATA: std::ops::RangeInclusive<u32> = 0x26..=0x2D;
}

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
            sh::BOOL => self.uniforms.boolean = value as u16,
            o if sh::INT.contains(&o) => {
                self.uniforms.int[(o - 1) as usize] = value.to_le_bytes();
            }
            sh::ENTRY => self.entry = value & 0xFFFF,
            sh::FLOAT_INDEX => {
                self.float_index = (value & 0xFF) as usize;
                self.float_single = value >> 31 != 0;
                self.float_words.clear();
            }
            o if sh::FLOAT_DATA.contains(&o) => {
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
            sh::CODE_INDEX => self.code_index = (value & 0xFFF) as usize,
            o if sh::CODE_DATA.contains(&o) => {
                if let Some(slot) = self.program.code.get_mut(self.code_index) {
                    *slot = value;
                }
                self.code_index += 1;
            }
            sh::OPDESC_INDEX => self.opdesc_index = (value & 0xFFF) as usize,
            o if sh::OPDESC_DATA.contains(&o) => {
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

pub struct Gpu {
    pub regs: [u32; REGISTERS],
    pub geometry: ShaderSetup,
    pub vertex: ShaderSetup,
}

impl Default for Gpu {
    fn default() -> Self {
        Gpu {
            regs: [0; REGISTERS],
            geometry: ShaderSetup::default(),
            vertex: ShaderSetup::default(),
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
    /// register `id`.
    pub fn write_register(&mut self, id: u32, value: u32, mask: u32) {
        let Some(slot) = self.regs.get_mut(id as usize) else {
            return;
        };
        let bits = (0..4)
            .filter(|byte| mask & 1 << byte != 0)
            .fold(0u32, |bits, byte| bits | 0xFF << (byte * 8));
        *slot = *slot & !bits | value & bits;
        let value = *slot;
        match id {
            reg::GSH..=0x2AF => self.geometry.write(id - reg::GSH, value),
            reg::VSH..=0x2DF => self.vertex.write(id - reg::VSH, value),
            _ => {}
        }
    }

    /// Carry out a command list.
    pub fn run_list(&mut self, words: &[u32]) -> ListEnd {
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
                self.write_register(id, value, mask);
                match id {
                    reg::FINALIZE => return ListEnd::Finalized,
                    reg::CMDBUF_JUMP0 | 0x23D => {
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
    use crate::shader::{Unit, ONE};

    fn header(id: u32, mask: u32, extra: u32, consecutive: bool) -> u32 {
        id | mask << 16 | extra << 20 | (consecutive as u32) << 31
    }

    #[test]
    fn the_documented_example_writes_three_registers() {
        let mut gpu = Gpu::new();
        let list = [0xAAAA_AAAA, 0x802F_011C, 0xBBBB_BBBB, 0xCCCC_CCCC];
        assert_eq!(gpu.run_list(&list), ListEnd::Exhausted);
        assert_eq!(
            gpu.regs[0x11C..0x11F],
            [0xAAAA_AAAA, 0xBBBB_BBBB, 0xCCCC_CCCC]
        );

        // Without consecutive mode they all land on the first.
        let mut gpu = Gpu::new();
        let list = [0xAAAA_AAAA, 0x002F_011C, 0xBBBB_BBBB, 0xCCCC_CCCC];
        gpu.run_list(&list);
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
        gpu.run_list(&list);
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
        assert_eq!(gpu.run_list(&list), ListEnd::Finalized);
        assert_eq!(gpu.regs[0x100], 0, "nothing runs after FINALIZE");

        let list = [
            0x0010_0000 >> 3,
            header(reg::CMDBUF_ADDR0 + 1, 0xF, 0, false),
            0x40 >> 3,
            header(reg::CMDBUF_SIZE0 + 1, 0xF, 0, false),
            1,
            header(reg::CMDBUF_JUMP0 + 1, 0xF, 0, false),
        ];
        assert_eq!(gpu.run_list(&list), ListEnd::Jump(1));
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
            header(reg::VSH + sh::CODE_INDEX, 0xF, 2, true),
            mov,
            end,
            0,
            header(reg::VSH + sh::OPDESC_INDEX, 0xF, 1, true),
            plain,
            0xDEAD_BEEF, // padding
            5,
            header(reg::VSH + sh::FLOAT_INDEX, 0xF, 3, true),
            z << 24 | w,
            y << 16 | z >> 8,
            x << 8 | y >> 16,
            0xDEAD_BEEF, // padding
            1 << 31 | 6,
            header(reg::VSH + sh::FLOAT_INDEX, 0xF, 4, true),
            2.0f32.to_bits(),
            0.0f32.to_bits(),
            0.0f32.to_bits(),
            4.0f32.to_bits(),
            0x0000_0B0B,
            header(reg::VSH + sh::BOOL, 0xF, 0, false),
            0x0403_0201,
            header(reg::VSH + 2, 0xF, 0, false),
            0,
            header(reg::VSH + sh::ENTRY, 0xF, 0, false),
            1,
            header(reg::FINALIZE, 0xF, 0, false),
        ];
        let mut gpu = Gpu::new();
        assert_eq!(gpu.run_list(&list), ListEnd::Finalized);
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

    #[test]
    fn the_uniform_fifo_moves_on_to_the_next_register() {
        let mut gpu = Gpu::new();
        gpu.write_register(reg::VSH + sh::FLOAT_INDEX, 94, 0xF);
        for _ in 0..3 {
            for word in [0x3F_0000, 0, 0] {
                gpu.write_register(reg::VSH + 0x11, word, 0xF);
            }
        }
        // c94 and c95 are set; the third lands past the end and is dropped.
        assert_eq!(gpu.vertex.uniforms.float[94][3], ONE);
        assert_eq!(gpu.vertex.uniforms.float[95][3], ONE);
    }
}
