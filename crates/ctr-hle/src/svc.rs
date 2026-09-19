//! Supervisor calls (3dbrew, "SVC"): arguments in r0-r4 (r0 and r4 often
//! swapped in by the library's veneer), the result code back in r0 and
//! further results from r1 up.

use crate::horizon::{Horizon, ThreadState};
use crate::memory::{Perm, HEAP_BASE, PAGE};
use crate::result::{self, ResultCode};

/// What became of a call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Done,
    /// No such call is implemented; the process cannot go on.
    Unknown,
}

/// `ControlMemory` operations, in the low byte of the operation word.
mod mem_op {
    pub const FREE: u32 = 1;
    pub const COMMIT: u32 = 3;
    pub const PROTECT: u32 = 6;
    /// Flag: the memory is part of the linear heap.
    pub const LINEAR: u32 = 0x1_0000;
}

/// The end of the ordinary heap's address range.
const HEAP_END: u32 = 0x1000_0000;

pub fn call(h: &mut Horizon, number: u32) -> Outcome {
    match number {
        0x01 => {
            let (result, addr) = control_memory(h);
            h.cpu.set_reg(0, result.0);
            h.cpu.set_reg(1, addr);
        }
        0x03 => {
            h.note("the process exited".to_string());
            h.thread = ThreadState::Exited;
        }
        0x09 => h.thread = ThreadState::Exited,
        0x0A => {
            let nanoseconds = (h.cpu.reg(1) as u64) << 32 | h.cpu.reg(0) as u64;
            h.sleep(nanoseconds);
        }
        0x28 => {
            // A tick is an ARM11 cycle.
            let now = h.clock.now();
            h.cpu.set_reg(0, now as u32);
            h.cpu.set_reg(1, (now >> 32) as u32);
        }
        0x3C => {
            let reason = h.cpu.reg(0);
            h.stop(format!("the program called Break (reason {reason})"));
        }
        0x3D => {
            let (addr, len) = (h.cpu.reg(0), h.cpu.reg(1).min(0x1000) as usize);
            let text = h
                .process
                .space
                .read_bytes(&h.mem, addr, len)
                .map(|bytes| String::from_utf8_lossy(&bytes).trim_end().to_string());
            match text {
                Some(text) => h.note(format!("debug: {text}")),
                None => h.note(format!("debug: unreadable string at {addr:#010x}")),
            }
            h.cpu.set_reg(0, result::SUCCESS.0);
        }
        _ => return Outcome::Unknown,
    }
    Outcome::Done
}

/// `ControlMemory(operation, addr0, addr1, size, permissions)`: grow, shrink
/// or re-protect the heap. Returns the result and the address operated on.
fn control_memory(h: &mut Horizon) -> (ResultCode, u32) {
    let (operation, addr0, size) = (h.cpu.reg(0), h.cpu.reg(1), h.cpu.reg(3));
    let perm = Perm(h.cpu.reg(4) as u8 & 3);
    if size == 0 || size % PAGE != 0 || addr0 % PAGE != 0 {
        return (result::MISALIGNED_SIZE, 0);
    }
    let process = &mut h.process;
    match operation & 0xFF {
        mem_op::COMMIT if operation & mem_op::LINEAR != 0 => {
            // The linear heap grows at its end; the kernel picks the address.
            let addr = process.linear_base + process.linear_len;
            if addr0 != 0 && addr0 != addr {
                return (result::INVALID_ADDRESS, 0);
            }
            match process.space.fcram.take_linear(size) {
                Some(pa) => {
                    process.space.map(addr, pa, size, perm);
                    process.linear_len += size;
                    (result::SUCCESS, addr)
                }
                None => (result::OUT_OF_MEMORY, 0),
            }
        }
        mem_op::COMMIT => {
            let inside =
                addr0 >= HEAP_BASE && addr0.checked_add(size).is_some_and(|e| e <= HEAP_END);
            if !inside {
                return (result::INVALID_ADDRESS, 0);
            }
            match process.space.allocate(addr0, size, perm) {
                Some(_) => {
                    process.heap_len = process.heap_len.max(addr0 + size - HEAP_BASE);
                    (result::SUCCESS, addr0)
                }
                None => (result::OUT_OF_MEMORY, 0),
            }
        }
        mem_op::FREE => {
            // The pages are unmapped; their memory is not handed out again
            // yet, which only matters to a program that frees and regrows a
            // lot.
            process.space.unmap(addr0, size);
            (result::SUCCESS, addr0)
        }
        mem_op::PROTECT => match process.space.v2p(addr0) {
            Some(_) => {
                for page in (addr0..addr0 + size).step_by(PAGE as usize) {
                    if let Some(pa) = process.space.v2p(page) {
                        process.space.map(page, pa, PAGE, perm);
                    }
                }
                (result::SUCCESS, addr0)
            }
            None => (result::INVALID_ADDRESS, 0),
        },
        other => {
            h.note(format!(
                "ControlMemory operation {other} is not implemented"
            ));
            (result::NOT_IMPLEMENTED, 0)
        }
    }
}
