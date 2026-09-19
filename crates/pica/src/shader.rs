//! The shader unit: the instruction set vertex and geometry shaders share
//! (3dbrew, "GPU/Shader Instruction Set").
//!
//! Arithmetic is done in single precision by `softfloat`, with the PICA's
//! departures from IEEE laid on top as 3dbrew's hardware tests give them:
//! there is no negative zero, subnormals are zero, and zero times anything
//! but NaN is zero. The hardware works in its 24-bit format (one sign bit,
//! seven exponent bits, sixteen mantissa bits); what that does to rounding
//! is not modelled, as no measurements of it are published.
//!
//! Not implemented: `EX2`, `LG2` and `LITP`, which need an exponential and a
//! logarithm in integer arithmetic. They leave their destination alone and
//! are counted in [`Unit::unimplemented`].

use softfloat::{Env, F32};
use std::cmp::Ordering;

pub type Vec4 = [F32; 4];

pub const ZERO: F32 = F32(0);
pub const ONE: F32 = F32(0x3F80_0000);
const INFINITY: F32 = F32(0x7F80_0000);

/// Instructions a run may execute before it is cut off; a shader has at most
/// 512 and loops are bounded, so only a program that never ends gets here.
const STEP_LIMIT: u32 = 1 << 20;
const STACK_DEPTH: usize = 32;

/// A value in the 24-bit format of uniforms and fixed attributes, widened.
/// Subnormals are zero and there is no negative zero.
pub fn from_float24(raw: u32) -> F32 {
    let sign = raw >> 23 & 1;
    let exponent = raw >> 16 & 0x7F;
    let mantissa = raw & 0xFFFF;
    let bits = match exponent {
        0 => return ZERO,
        0x7F => 0x7F80_0000 | mantissa << 7,
        _ => (exponent + 127 - 63) << 23 | mantissa << 7,
    };
    F32(sign << 31 | bits)
}

/// Zero has one sign here, and subnormals do not exist.
fn tidy(value: F32) -> F32 {
    if value.0 & 0x7F80_0000 == 0 {
        ZERO
    } else {
        value
    }
}

fn is_zero(value: F32) -> bool {
    value.0 & 0x7FFF_FFFF == 0
}

fn add(a: F32, b: F32) -> F32 {
    tidy(a.add(b, &mut Env::default()))
}

/// Zero times infinity is zero; only NaN survives a zero.
fn mul(a: F32, b: F32) -> F32 {
    if (is_zero(a) && !b.is_nan()) || (is_zero(b) && !a.is_nan()) {
        return ZERO;
    }
    tidy(a.mul(b, &mut Env::default()))
}

fn less(a: F32, b: F32) -> bool {
    a.compare(b, &mut Env::default()) == Some(Ordering::Less)
}

fn negate(value: F32) -> F32 {
    tidy(F32(value.0 ^ 0x8000_0000))
}

fn floor(value: F32) -> F32 {
    // From 2^23 up every value is whole already (and infinities and NaN
    // pass through).
    if value.0 >> 23 & 0xFF >= 127 + 23 {
        return value;
    }
    let mut env = Env::default();
    let whole = value.to_i32(softfloat::Round::Down, &mut env);
    tidy(F32::from_i32(whole, &mut env))
}

fn reciprocal(value: F32) -> F32 {
    if is_zero(value) {
        return INFINITY;
    }
    tidy(ONE.div(value, &mut Env::default()))
}

fn reciprocal_root(value: F32) -> F32 {
    if is_zero(value) {
        return INFINITY;
    }
    let mut env = Env::default();
    tidy(ONE.div(value.sqrt(&mut env), &mut env))
}

/// The instructions and operand descriptors loaded into a shader unit.
#[derive(Clone, Default)]
pub struct Program {
    pub code: Vec<u32>,
    pub operands: Vec<u32>,
}

/// What the application sets for a shader unit.
#[derive(Clone)]
pub struct Uniforms {
    pub float: [Vec4; 96],
    /// `x` iterations minus one, `y` the first loop counter, `z` its step.
    pub int: [[u8; 4]; 4],
    pub boolean: u16,
}

impl Default for Uniforms {
    fn default() -> Self {
        Uniforms {
            float: [[ZERO; 4]; 96],
            int: [[0; 4]; 4],
            boolean: 0,
        }
    }
}

/// What a geometry shader asked for with `SETEMIT` before an `EMIT`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Emit {
    pub vertex: u8,
    pub primitive: bool,
    pub winding: bool,
}

/// The registers of one run.
#[derive(Clone)]
pub struct Unit {
    pub inputs: [Vec4; 16],
    pub temps: [Vec4; 16],
    pub outputs: [Vec4; 16],
    pub address: [i32; 2],
    pub loop_counter: i32,
    pub cmp: [bool; 2],
    pub emit: Emit,
    /// `EMIT` instructions executed, each with the state it saw.
    pub emitted: Vec<(Emit, [Vec4; 16])>,
    /// `EX2`, `LG2` and `LITP` instructions skipped.
    pub unimplemented: u32,
}

impl Default for Unit {
    fn default() -> Self {
        Unit {
            inputs: [[ZERO; 4]; 16],
            temps: [[ZERO; 4]; 16],
            outputs: [[ZERO; 4]; 16],
            address: [0; 2],
            loop_counter: 0,
            cmp: [false; 2],
            emit: Emit::default(),
            emitted: Vec::new(),
            unimplemented: 0,
        }
    }
}

/// A block being executed: where it ends, where execution goes then, and for
/// a loop how often it still repeats.
struct Frame {
    end: u32,
    next: u32,
    repeats: u8,
    step: u8,
    start: u32,
    is_loop: bool,
}

/// How a run ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stop {
    End,
    /// Ran off the end of the program.
    OutOfCode,
    StepLimit,
    StackOverflow,
}

impl Unit {
    fn source(&self, uniforms: &Uniforms, register: u32, index: u32) -> Vec4 {
        match register {
            0x00..=0x0F => self.inputs[register as usize],
            0x10..=0x1F => self.temps[register as usize - 0x10],
            _ => {
                // Only constants are addressed relatively. An offset that
                // does not fit a signed byte is dropped, the sum wraps at
                // seven bits, and past c95 everything reads as one.
                let offset = match index {
                    1 => self.address[0],
                    2 => self.address[1],
                    3 => self.loop_counter,
                    _ => 0,
                };
                let offset = if (-128..=127).contains(&offset) {
                    offset
                } else {
                    0
                };
                let at = (register as i32 - 0x20 + offset) & 0x7F;
                uniforms.float.get(at as usize).copied().unwrap_or([ONE; 4])
            }
        }
    }

    fn write(&mut self, register: u32, mask: u32, value: Vec4) {
        let target = match register {
            0x00..=0x0F => &mut self.outputs[register as usize],
            _ => &mut self.temps[register as usize & 0xF],
        };
        for (i, component) in value.into_iter().enumerate() {
            if mask & 8 >> i != 0 {
                target[i] = component;
            }
        }
    }

    fn condition(&self, instr: u32) -> bool {
        let x = self.cmp[0] == (instr >> 25 & 1 != 0);
        let y = self.cmp[1] == (instr >> 24 & 1 != 0);
        match instr >> 22 & 3 {
            0 => x || y,
            1 => x && y,
            2 => x,
            _ => y,
        }
    }

    /// Run `program` from `entry` until `END`.
    pub fn run(&mut self, program: &Program, uniforms: &Uniforms, entry: u32) -> Stop {
        let swizzle = |v: Vec4, selector: u32, negated: bool| -> Vec4 {
            std::array::from_fn(|i| {
                let component = v[(selector >> (6 - 2 * i) & 3) as usize];
                if negated {
                    negate(component)
                } else {
                    tidy(component)
                }
            })
        };
        let mut stack: Vec<Frame> = Vec::new();
        let mut pc = entry;
        for _ in 0..STEP_LIMIT {
            // Leave, or repeat, every block that ends here.
            while let Some(frame) = stack.last_mut() {
                if pc != frame.end {
                    break;
                }
                if frame.repeats == 0 {
                    pc = frame.next;
                    stack.pop();
                } else {
                    frame.repeats -= 1;
                    self.loop_counter += frame.step as i32;
                    pc = frame.start;
                }
            }
            let Some(&instr) = program.code.get(pc as usize) else {
                return Stop::OutOfCode;
            };
            let opcode = instr >> 26;
            let mut next = pc + 1;
            let push = |stack: &mut Vec<Frame>, frame: Frame| {
                stack.push(frame);
                stack.len() <= STACK_DEPTH
            };

            let num = instr & 0xFF;
            let dst = instr >> 10 & 0xFFF;
            let block = |end: u32, after: u32| Frame {
                end,
                next: after,
                repeats: 0,
                step: 0,
                start: 0,
                is_loop: false,
            };
            match opcode {
                0x21 => {}
                0x22 => return Stop::End,
                0x20 | 0x23 => {
                    if opcode == 0x20 || self.condition(instr) {
                        // Out of the innermost loop, dropping what is nested.
                        while let Some(frame) = stack.pop() {
                            if frame.is_loop {
                                next = frame.next;
                                break;
                            }
                        }
                    }
                }
                0x24..=0x26 => {
                    let taken = match opcode {
                        0x24 => true,
                        0x25 => self.condition(instr),
                        _ => uniforms.boolean >> (instr >> 22 & 0xF) & 1 != 0,
                    };
                    if taken {
                        if !push(&mut stack, block(dst + num, pc + 1)) {
                            return Stop::StackOverflow;
                        }
                        next = dst;
                    }
                }
                0x27 | 0x28 => {
                    let taken = if opcode == 0x28 {
                        self.condition(instr)
                    } else {
                        uniforms.boolean >> (instr >> 22 & 0xF) & 1 != 0
                    };
                    if !taken {
                        next = dst;
                    } else if !push(&mut stack, block(dst, dst + num)) {
                        return Stop::StackOverflow;
                    }
                }
                0x29 => {
                    let [count, first, step, _] = uniforms.int[(instr >> 22 & 3) as usize];
                    self.loop_counter = first as i32;
                    let frame = Frame {
                        end: dst + 1,
                        next: dst + 1,
                        repeats: count,
                        step,
                        start: pc + 1,
                        is_loop: true,
                    };
                    if !push(&mut stack, frame) {
                        return Stop::StackOverflow;
                    }
                }
                0x2A => self.emitted.push((self.emit, self.outputs)),
                0x2B => {
                    self.emit = Emit {
                        vertex: (instr >> 24 & 3) as u8,
                        primitive: instr >> 23 & 1 != 0,
                        winding: instr >> 22 & 1 != 0,
                    }
                }
                0x2C => {
                    if self.condition(instr) {
                        next = dst;
                    }
                }
                0x2D => {
                    // Bit 0 of the count inverts the test.
                    let set = uniforms.boolean >> (instr >> 22 & 0xF) & 1 != 0;
                    if set != (num & 1 != 0) {
                        next = dst;
                    }
                }
                0x30..=0x3F => {
                    // MAD and MADI: a rounded product, then the sum.
                    let descriptor = program
                        .operands
                        .get((instr & 0x1F) as usize)
                        .copied()
                        .unwrap_or(0);
                    let inverted = opcode < 0x38;
                    let (r1, r2, r3, index) = if inverted {
                        (instr >> 17 & 0x1F, instr >> 12 & 0x1F, instr >> 5 & 0x7F, 3)
                    } else {
                        (instr >> 17 & 0x1F, instr >> 10 & 0x7F, instr >> 5 & 0x1F, 2)
                    };
                    let idx = instr >> 22 & 3;
                    let pick = |n: u32| if n == index { idx } else { 0 };
                    let s1 = swizzle(
                        self.source(uniforms, r1, pick(1)),
                        descriptor >> 5 & 0xFF,
                        descriptor >> 4 & 1 != 0,
                    );
                    let s2 = swizzle(
                        self.source(uniforms, r2, pick(2)),
                        descriptor >> 14 & 0xFF,
                        descriptor >> 13 & 1 != 0,
                    );
                    let s3 = swizzle(
                        self.source(uniforms, r3, pick(3)),
                        descriptor >> 23 & 0xFF,
                        descriptor >> 22 & 1 != 0,
                    );
                    let value = std::array::from_fn(|i| add(mul(s1[i], s2[i]), s3[i]));
                    self.write(instr >> 24 & 0x1F, descriptor & 0xF, value);
                }
                _ => {
                    // Formats 1, 1i, 1u and 1c.
                    let descriptor = program
                        .operands
                        .get((instr & 0x7F) as usize)
                        .copied()
                        .unwrap_or(0);
                    let inverted = (0x18..=0x1B).contains(&opcode);
                    let idx = instr >> 19 & 3;
                    let (r1, r2, idx1, idx2) = if inverted {
                        (instr >> 14 & 0x1F, instr >> 7 & 0x7F, 0, idx)
                    } else {
                        (instr >> 12 & 0x7F, instr >> 7 & 0x1F, idx, 0)
                    };
                    let s1 = swizzle(
                        self.source(uniforms, r1, idx1),
                        descriptor >> 5 & 0xFF,
                        descriptor >> 4 & 1 != 0,
                    );
                    let s2 = swizzle(
                        self.source(uniforms, r2, idx2),
                        descriptor >> 14 & 0xFF,
                        descriptor >> 13 & 1 != 0,
                    );
                    let mask = descriptor & 0xF;
                    let dot = |n: usize| (0..n).fold(ZERO, |sum, i| add(sum, mul(s1[i], s2[i])));
                    let flag = |set: bool| if set { ONE } else { ZERO };
                    let value: Option<Vec4> = match opcode {
                        0x00 => Some(std::array::from_fn(|i| add(s1[i], s2[i]))),
                        0x01 => Some([dot(3); 4]),
                        0x02 => Some([dot(4); 4]),
                        // The first source with its w taken as one.
                        0x03 | 0x18 => Some([add(dot(3), s2[3]); 4]),
                        0x04 | 0x19 => Some([ONE, mul(s1[1], s2[1]), s1[2], s2[3]]),
                        0x05..=0x07 => {
                            self.unimplemented += 1;
                            None
                        }
                        0x08 => Some(std::array::from_fn(|i| mul(s1[i], s2[i]))),
                        0x09 | 0x1A => Some(std::array::from_fn(|i| {
                            flag(!less(s1[i], s2[i]) && !s1[i].is_nan() && !s2[i].is_nan())
                        })),
                        0x0A | 0x1B => Some(std::array::from_fn(|i| flag(less(s1[i], s2[i])))),
                        0x0B => Some(s1.map(floor)),
                        // A NaN in the second place wins, in the first it
                        // loses: `a > b ? a : b`, and the like for MIN.
                        0x0C => Some(std::array::from_fn(|i| {
                            if less(s2[i], s1[i]) {
                                s1[i]
                            } else {
                                s2[i]
                            }
                        })),
                        0x0D => Some(std::array::from_fn(|i| {
                            if less(s1[i], s2[i]) {
                                s1[i]
                            } else {
                                s2[i]
                            }
                        })),
                        0x0E => Some([reciprocal(s1[0]); 4]),
                        0x0F => Some([reciprocal_root(s1[0]); 4]),
                        0x12 => {
                            let mut env = Env::default();
                            for (i, address) in self.address.iter_mut().enumerate() {
                                if mask & 8 >> i != 0 {
                                    *address = s1[i].to_i32(softfloat::Round::Zero, &mut env);
                                }
                            }
                            None
                        }
                        0x13 => Some(s1),
                        0x2E | 0x2F => {
                            let mut env = Env::default();
                            for (i, shift) in [(0, 24), (1, 21)] {
                                let order = s1[i].compare(s2[i], &mut env);
                                self.cmp[i] = match instr >> shift & 7 {
                                    0 => order == Some(Ordering::Equal),
                                    1 => order != Some(Ordering::Equal),
                                    2 => order == Some(Ordering::Less),
                                    3 => matches!(order, Some(Ordering::Less | Ordering::Equal)),
                                    4 => order == Some(Ordering::Greater),
                                    5 => matches!(order, Some(Ordering::Greater | Ordering::Equal)),
                                    _ => true,
                                };
                            }
                            None
                        }
                        // The unknown opcodes do nothing here.
                        _ => None,
                    };
                    if let Some(value) = value {
                        self.write(instr >> 21 & 0x1F, mask, value);
                    }
                }
            }
            pc = next;
        }
        Stop::StepLimit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(value: f32) -> F32 {
        F32(value.to_bits())
    }

    fn vec(values: [f32; 4]) -> Vec4 {
        values.map(f)
    }

    /// `.xyzw`, nothing negated, every destination component.
    const PLAIN: u32 = 0x1B << 23 | 0x1B << 14 | 0x1B << 5 | 0xF;
    const END: u32 = 0x22 << 26;

    fn op(opcode: u32, dst: u32, src1: u32, src2: u32, descriptor: u32) -> u32 {
        opcode << 26 | dst << 21 | src1 << 12 | src2 << 7 | descriptor
    }

    fn run(code: &[u32], operands: &[u32], uniforms: &Uniforms, unit: &mut Unit) -> Stop {
        let program = Program {
            code: code.to_vec(),
            operands: operands.to_vec(),
        };
        unit.run(&program, uniforms, 0)
    }

    #[test]
    fn float24_widens_and_has_no_subnormals_or_negative_zero() {
        assert_eq!(from_float24(0x3F_0000), ONE);
        assert_eq!(from_float24(0xBF_0000), f(-1.0));
        assert_eq!(from_float24(0x40_8000), f(3.0));
        assert_eq!(from_float24(0x00_FFFF), ZERO);
        assert_eq!(from_float24(0x80_0000), ZERO);
        assert_eq!(from_float24(0x7F_0000), INFINITY);
        assert!(from_float24(0x7F_0001).is_nan());
    }

    #[test]
    fn arithmetic_follows_the_hardware_tests() {
        let nan = F32(0x7FC0_0000);
        assert_eq!(mul(INFINITY, ZERO), ZERO);
        assert!(mul(nan, ZERO).is_nan());
        assert!(add(INFINITY, negate(INFINITY)).is_nan());
        assert_eq!(reciprocal(F32(0x8000_0000)), INFINITY);
        assert_eq!(reciprocal(INFINITY), ZERO);
        assert_eq!(reciprocal_root(reciprocal(negate(INFINITY))), INFINITY);
        assert!(reciprocal_root(f(-2.0)).is_nan());
        assert_eq!(reciprocal_root(INFINITY), ZERO);
        assert_eq!(floor(f(-1.5)), f(-2.0));
        assert_eq!(floor(f(2.75)), f(2.0));
        assert_eq!(floor(f(-0.25)), f(-1.0));
        assert_eq!(floor(f(1e30)), f(1e30));
    }

    #[test]
    fn a_transform_is_four_dot_products_with_masks() {
        // o0.x = dot(c0, v0) ... o0.w = dot(c3, v0), then o1 = v0.wzyx negated.
        let code: Vec<u32> = (0..4)
            .map(|row| op(0x02, 0, 0x20 + row, 0, row))
            .chain([op(0x13, 1, 0, 0, 4), END])
            .collect();
        let operands: Vec<u32> = (0..4)
            .map(|row| PLAIN & !0xF | 8 >> row)
            .chain([0xE4 << 5 | 1 << 4 | 0xF])
            .collect();
        let mut uniforms = Uniforms::default();
        uniforms.float[0] = vec([2.0, 0.0, 0.0, 1.0]);
        uniforms.float[1] = vec([0.0, 3.0, 0.0, 2.0]);
        uniforms.float[2] = vec([0.0, 0.0, 4.0, 3.0]);
        uniforms.float[3] = vec([0.0, 0.0, 0.0, 1.0]);
        let mut unit = Unit::default();
        unit.inputs[0] = vec([1.0, 2.0, 3.0, 1.0]);
        assert_eq!(run(&code, &operands, &uniforms, &mut unit), Stop::End);
        assert_eq!(unit.outputs[0], vec([3.0, 8.0, 15.0, 1.0]));
        assert_eq!(unit.outputs[1], vec([-1.0, -3.0, -2.0, -1.0]));
    }

    #[test]
    fn max_and_min_let_a_nan_in_the_second_place_through() {
        let nan = F32(0x7FC0_0000);
        let code = [op(0x0C, 0, 0, 1, 0), op(0x0D, 1, 0, 1, 0), END];
        let mut unit = Unit::default();
        unit.inputs[0] = [ZERO, nan, f(1.0), f(5.0)];
        unit.inputs[1] = [nan, ZERO, f(2.0), INFINITY];
        run(&code, &[PLAIN], &Uniforms::default(), &mut unit);
        assert!(unit.outputs[0][0].is_nan());
        assert_eq!(unit.outputs[0][1..], [ZERO, f(2.0), INFINITY]);
        assert!(unit.outputs[1][0].is_nan());
        assert_eq!(unit.outputs[1][1..], [ZERO, f(1.0), f(5.0)]);
    }

    #[test]
    fn relative_addressing_wraps_and_reads_one_past_the_constants() {
        // mova a0.xy, v0 ; mov o0, c8[a0.x] ; mov o1, c90[a0.y] ; mov o2, v1[a0.x]
        let code = [
            op(0x12, 0, 0, 0, 0),
            op(0x13, 0, 0x28, 0, 0) | 1 << 19,
            op(0x13, 1, 0x7A, 0, 0) | 2 << 19,
            op(0x13, 2, 0x01, 0, 0) | 1 << 19,
            END,
        ];
        let mut uniforms = Uniforms::default();
        uniforms.float[11] = vec([11.0; 4]);
        let mut unit = Unit::default();
        unit.inputs[0] = vec([3.9, 10.0, 0.0, 0.0]);
        unit.inputs[1] = vec([7.0; 4]);
        run(&code, &[PLAIN], &uniforms, &mut unit);
        assert_eq!(unit.address, [3, 10]);
        assert_eq!(unit.outputs[0], vec([11.0; 4]));
        assert_eq!(unit.outputs[1], [ONE; 4], "c100 does not exist");
        assert_eq!(unit.outputs[2], vec([7.0; 4]), "inputs ignore the index");
    }

    #[test]
    fn compare_and_the_conditional_forms() {
        // cmp v0 (x: EQ, y: GT) v1 ; ifc x&&y { mov o0, v0 } else { mov o0, v1 }
        let cmp = 0x2E << 26 | 1 << 7 | 4 << 21;
        let ifc = 0x28 << 26 | 1 << 22 | 1 << 25 | 1 << 24 | 3 << 10 | 1;
        let code = [cmp, ifc, op(0x13, 0, 0, 0, 0), op(0x13, 0, 1, 0, 0), END];
        let mut unit = Unit::default();
        unit.inputs[0] = vec([1.0, 5.0, 0.0, 0.0]);
        unit.inputs[1] = vec([1.0, 2.0, 9.0, 9.0]);
        run(&code, &[PLAIN], &Uniforms::default(), &mut unit);
        assert_eq!(unit.cmp, [true, true]);
        assert_eq!(unit.outputs[0], unit.inputs[0], "the else block is skipped");

        unit.inputs[0] = vec([1.0, 1.0, 0.0, 0.0]);
        run(&code, &[PLAIN], &Uniforms::default(), &mut unit);
        assert_eq!(unit.cmp, [true, false]);
        assert_eq!(unit.outputs[0], unit.inputs[1]);
    }

    #[test]
    fn a_loop_runs_count_plus_one_times_and_steps_its_counter() {
        // loop i0 { mov r1, c0[aL] ; add r0, r0, r1 } ; mov o0, r0
        let code = [
            0x29 << 26 | 2 << 10,
            op(0x13, 0x11, 0x20, 0, 0) | 3 << 19,
            op(0x00, 0x10, 0x10, 0x11, 0),
            op(0x13, 0, 0x10, 0, 0),
            END,
        ];
        let mut uniforms = Uniforms::default();
        uniforms.int[0] = [2, 1, 2, 0];
        for i in 0..8 {
            uniforms.float[i] = vec([i as f32; 4]);
        }
        let mut unit = Unit::default();
        assert_eq!(run(&code, &[PLAIN], &uniforms, &mut unit), Stop::End);
        // c1 + c3 + c5.
        assert_eq!(unit.outputs[0], vec([9.0; 4]));
        assert_eq!(unit.loop_counter, 5, "still readable after the loop");
    }

    #[test]
    fn call_returns_and_mad_rounds_its_product_first() {
        // call 3, 1 ; end ; nop ; mad o0, v0, v1, v2
        let mad = 0x38 << 26 | 2 << 5 | 1 << 10;
        let code = [0x24 << 26 | 3 << 10 | 1, END, 0x21 << 26, mad];
        let mut unit = Unit::default();
        unit.inputs[0] = vec([2.0, 3.0, 4.0, 0.0]);
        unit.inputs[1] = vec([5.0, 6.0, 7.0, f32::INFINITY]);
        unit.inputs[2] = vec([1.0, 1.0, 1.0, 1.0]);
        assert_eq!(
            run(&code, &[PLAIN], &Uniforms::default(), &mut unit),
            Stop::End
        );
        assert_eq!(unit.outputs[0], vec([11.0, 19.0, 29.0, 1.0]));
    }

    #[test]
    fn a_program_without_an_end_is_cut_off() {
        let forever = [0x2C << 26 | 2 << 22];
        let mut unit = Unit::default();
        // cmp.x is false and the reference is false: the jump is always taken.
        assert_eq!(
            run(&forever, &[], &Uniforms::default(), &mut unit),
            Stop::StepLimit
        );
        assert_eq!(
            run(&[0x21 << 26], &[], &Uniforms::default(), &mut unit),
            Stop::OutOfCode
        );
    }
}
