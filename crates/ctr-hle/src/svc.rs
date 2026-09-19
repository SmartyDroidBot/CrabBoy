//! Supervisor calls (3dbrew, "SVC"). Arguments arrive in r0-r4 as libctru's
//! veneers leave them (`libctru/source/svc.s`): where a call returns a value
//! through a pointer, the veneer keeps the pointer and the kernel's value
//! comes back in r1. The result code goes in r0.
//!
//! A call that blocks leaves the thread waiting; its results are written to
//! its saved registers by whatever ends the wait.

use crate::horizon::{cycles_of, HleEvent, Horizon};
use crate::kernel::{Object, Reset, INVALID_HANDLE, OUT_OF_RANGE, TIMEOUT};
use crate::memory::{Perm, HEAP_BASE, PAGE};
use crate::result::{self, ResultCode};
use arm_core::{Arch, Cpu};

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

/// `ArbitrateAddress` types; 1 is the plain wait-if-less-than.
mod arbitration {
    pub const SIGNAL: u32 = 0;
    pub const DECREMENT_AND_WAIT_IF_LESS_THAN: u32 = 2;
    pub const WAIT_IF_LESS_THAN_TIMEOUT: u32 = 3;
    pub const DECREMENT_AND_WAIT_IF_LESS_THAN_TIMEOUT: u32 = 4;
}

/// The end of the ordinary heap's address range.
const HEAP_END: u32 = 0x1000_0000;

impl Horizon {
    /// Set register `n` of the calling thread, in its saved registers, which
    /// are loaded again when the call returns.
    fn ret(&mut self, n: usize, value: u32) {
        if let Some(current) = self.kernel.current {
            self.kernel.threads[current].context.r[n] = value;
        }
    }

    fn ret_code(&mut self, code: ResultCode) {
        self.ret(0, code.0);
    }

    /// A 64-bit count of nanoseconds in two registers, negative for "for
    /// ever".
    fn timeout(&self, low: usize, high: usize) -> i64 {
        ((self.reg(high) as u64) << 32 | self.reg(low) as u64) as i64
    }
}

pub fn call(h: &mut Horizon, number: u32) -> Outcome {
    match number {
        0x01 => {
            let (code, addr) = control_memory(h);
            h.ret_code(code);
            h.ret(1, addr);
        }
        0x03 => {
            h.note("the process exited".to_string());
            for thread in &mut h.kernel.threads {
                thread.state = crate::kernel::State::Dead;
            }
        }
        0x08 => create_thread(h),
        0x09 => h.kernel.exit_current(),
        0x0A => match h.timeout(0, 1) {
            // Zero gives way to the other threads of this priority.
            0 => h.kernel.yield_now(),
            ns => h.kernel.sleep(ns.max(0) as u64),
        },
        0x0B => match h.kernel.object(h.reg(1)) {
            Some(&Object::Thread(id)) => {
                let priority = h.kernel.threads[id].priority;
                h.ret(1, priority as u32);
                h.ret_code(result::SUCCESS);
            }
            _ => h.ret_code(INVALID_HANDLE),
        },
        0x0C => match h.kernel.object(h.reg(0)) {
            Some(&Object::Thread(id)) => {
                h.kernel.threads[id].priority = h.reg(1).min(63) as u8;
                h.ret_code(result::SUCCESS);
            }
            _ => h.ret_code(INVALID_HANDLE),
        },
        0x13 => {
            let handle = h.kernel.create_handle(Object::Mutex {
                owner: None,
                depth: 0,
            });
            if h.reg(1) != 0 {
                // Created locked: the caller takes it, which cannot block.
                let id = h.kernel.resolve(handle).expect("just made");
                h.kernel.wait(vec![id], false, -1);
            }
            h.ret_code(result::SUCCESS);
            h.ret(1, handle);
        }
        0x14 => {
            let code = match h.kernel.resolve(h.reg(0)) {
                Some(id) => h.kernel.release_mutex(id),
                None => INVALID_HANDLE,
            };
            h.ret_code(code);
        }
        0x15 => {
            let (count, max) = (h.reg(1) as i32, h.reg(2) as i32);
            let handle = h.kernel.create_handle(Object::Semaphore { count, max });
            h.ret_code(result::SUCCESS);
            h.ret(1, handle);
        }
        0x16 => {
            let released = match h.kernel.resolve(h.reg(1)) {
                Some(id) => h.kernel.release_semaphore(id, h.reg(2) as i32),
                None => Err(INVALID_HANDLE),
            };
            match released {
                Ok(before) => {
                    h.ret_code(result::SUCCESS);
                    h.ret(1, before as u32);
                }
                Err(code) => h.ret_code(code),
            }
        }
        0x17 => {
            let handle = h.kernel.create_handle(Object::Event {
                signalled: false,
                reset: Reset::from_raw(h.reg(1)),
            });
            h.ret_code(result::SUCCESS);
            h.ret(1, handle);
        }
        0x18 | 0x19 => {
            let code = match h.kernel.resolve(h.reg(0)) {
                Some(id) if matches!(h.kernel.objects[id], Object::Event { .. }) => {
                    if number == 0x18 {
                        h.kernel.signal(id);
                    } else {
                        h.kernel.clear(id);
                    }
                    result::SUCCESS
                }
                _ => INVALID_HANDLE,
            };
            h.ret_code(code);
        }
        0x1A => {
            let handle = h.kernel.create_handle(Object::Timer {
                signalled: false,
                reset: Reset::from_raw(h.reg(1)),
                interval: 0,
            });
            h.ret_code(result::SUCCESS);
            h.ret(1, handle);
        }
        0x1B..=0x1D => timer(h, number),
        0x21 => {
            let handle = h.kernel.create_handle(Object::Arbiter);
            h.ret_code(result::SUCCESS);
            h.ret(1, handle);
        }
        0x22 => arbitrate_address(h),
        0x23 => {
            let code = h.kernel.close(h.reg(0));
            h.ret_code(code);
        }
        0x24 => match h.kernel.resolve(h.reg(0)) {
            Some(id) => {
                let timeout = h.timeout(2, 3);
                if let Some((code, _)) = h.kernel.wait(vec![id], false, timeout) {
                    h.ret_code(code);
                }
            }
            None => h.ret_code(INVALID_HANDLE),
        },
        0x25 => wait_synchronization_n(h),
        0x27 => match h.kernel.resolve(h.reg(1)) {
            Some(id) => {
                let handle = h.kernel.open(id);
                h.ret_code(result::SUCCESS);
                h.ret(1, handle);
            }
            None => h.ret_code(INVALID_HANDLE),
        },
        0x28 => {
            // A tick is an ARM11 cycle.
            let now = h.clock.now();
            h.ret(0, now as u32);
            h.ret(1, (now >> 32) as u32);
        }
        0x35 => {
            // One process, and this is its number.
            h.ret_code(result::SUCCESS);
            h.ret(1, 1);
        }
        0x37 => match h.kernel.object(h.reg(1)) {
            Some(&Object::Thread(id)) => {
                h.ret_code(result::SUCCESS);
                h.ret(1, id as u32);
            }
            _ => h.ret_code(INVALID_HANDLE),
        },
        0x3C => {
            let reason = h.reg(0);
            h.stop(format!("the program called Break (reason {reason})"));
        }
        0x3D => {
            let (addr, len) = (h.reg(0), h.reg(1).min(0x1000) as usize);
            let text = h
                .process
                .space
                .read_bytes(&h.mem, addr, len)
                .map(|bytes| String::from_utf8_lossy(&bytes).trim_end().to_string());
            match text {
                Some(text) => h.note(format!("debug: {text}")),
                None => h.note(format!("debug: unreadable string at {addr:#010x}")),
            }
            h.ret_code(result::SUCCESS);
        }
        _ => return Outcome::Unknown,
    }
    Outcome::Done
}

/// `CreateThread(priority, entry, argument, stack top, processor)`: r0 and r4
/// are swapped in by the veneer. The thread starts with the argument in r0.
fn create_thread(h: &mut Horizon) {
    let (priority, entry, argument, stack) = (h.reg(0), h.reg(1), h.reg(2), h.reg(3));
    let processor = h.reg(4) as i32;
    if priority > 63 {
        return h.ret_code(OUT_OF_RANGE);
    }
    let Some(tls) = h.process.allocate_tls(&mut h.mem) else {
        return h.ret_code(result::OUT_OF_MEMORY);
    };
    let mut context = Cpu::new_user(Arch::V6k, entry, stack & !3).save_context();
    context.r[0] = argument;
    // Returning from the entry function without ExitThread is a fault.
    context.r[14] = 0;
    let (_, object) = h.kernel.spawn(context, priority as u8, processor, tls);
    let handle = h.kernel.open(object);
    h.ret_code(result::SUCCESS);
    h.ret(1, handle);
}

/// `SetTimer(handle, initial, interval)` with the two 64-bit times in r2:r3
/// and r1:r4, `CancelTimer` and `ClearTimer`.
fn timer(h: &mut Horizon, number: u32) {
    let Some(id) = h
        .kernel
        .resolve(h.reg(0))
        .filter(|&id| matches!(h.kernel.objects[id], Object::Timer { .. }))
    else {
        return h.ret_code(INVALID_HANDLE);
    };
    let event = HleEvent::Timer(id as u32);
    match number {
        0x1B => {
            let initial = h.timeout(2, 3).max(0) as u64;
            let repeat = h.timeout(1, 4).max(0) as u64;
            if let Object::Timer { interval, .. } = &mut h.kernel.objects[id] {
                *interval = cycles_of(repeat);
            }
            h.clock
                .schedule(h.clock.now() + cycles_of(initial).max(1), event);
        }
        0x1C => h.clock.cancel(event),
        _ => h.kernel.clear(id),
    }
    h.ret_code(result::SUCCESS);
}

/// `WaitSynchronizationN(out, handles, count, wait for all, timeout)`: the
/// veneer keeps `out` and passes the timeout in r0 and r4.
fn wait_synchronization_n(h: &mut Horizon) {
    let (handles, count, all) = (h.reg(1), h.reg(2), h.reg(3) != 0);
    let timeout = h.timeout(0, 4);
    let mut ids = Vec::new();
    for n in 0..count.min(0x100) {
        let handle = h.process.space.read32(&h.mem, handles + n * 4);
        match handle.and_then(|handle| h.kernel.resolve(handle)) {
            Some(id) => ids.push(id),
            None => return h.ret_code(INVALID_HANDLE),
        }
    }
    if ids.is_empty() {
        // Nothing to wait for: only the time passes.
        if timeout > 0 {
            h.kernel.sleep(timeout as u64);
        }
        h.ret_code(if timeout == 0 {
            result::SUCCESS
        } else {
            TIMEOUT
        });
        return;
    }
    if let Some((code, index)) = h.kernel.wait(ids, all, timeout) {
        h.ret_code(code);
        h.ret(1, index);
    }
}

/// `ArbitrateAddress(arbiter, address, type, value, timeout)`: the primitive
/// under libctru's light locks and condition variables.
fn arbitrate_address(h: &mut Horizon) {
    if !matches!(h.kernel.object(h.reg(0)), Some(Object::Arbiter)) {
        return h.ret_code(INVALID_HANDLE);
    }
    let (address, kind, value) = (h.reg(1), h.reg(2), h.reg(3) as i32);
    let timeout = match kind {
        arbitration::WAIT_IF_LESS_THAN_TIMEOUT
        | arbitration::DECREMENT_AND_WAIT_IF_LESS_THAN_TIMEOUT => h.timeout(4, 5),
        _ => -1,
    };
    h.ret_code(result::SUCCESS);
    if kind == arbitration::SIGNAL {
        return h.kernel.arbiter_signal(address, value);
    }
    if kind > arbitration::DECREMENT_AND_WAIT_IF_LESS_THAN_TIMEOUT {
        return h.ret_code(OUT_OF_RANGE);
    }
    let Some(word) = h.process.space.read32(&h.mem, address) else {
        return h.ret_code(result::INVALID_ADDRESS);
    };
    let word = word as i32;
    if word >= value {
        return;
    }
    if matches!(
        kind,
        arbitration::DECREMENT_AND_WAIT_IF_LESS_THAN
            | arbitration::DECREMENT_AND_WAIT_IF_LESS_THAN_TIMEOUT
    ) {
        h.process
            .space
            .write32(&mut h.mem, address, (word - 1) as u32);
    }
    h.kernel.arbiter_wait(address, timeout);
}

/// `ControlMemory(operation, addr0, addr1, size, permissions)`: grow, shrink
/// or re-protect the heap. Returns the result and the address operated on.
fn control_memory(h: &mut Horizon) -> (ResultCode, u32) {
    let (operation, addr0, size) = (h.reg(0), h.reg(1), h.reg(3));
    let perm = Perm(h.reg(4) as u8 & 3);
    if size == 0 || !size.is_multiple_of(PAGE) || !addr0.is_multiple_of(PAGE) {
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
