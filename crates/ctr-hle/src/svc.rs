//! Supervisor calls (3dbrew, "SVC"). Arguments arrive in r0-r4 as libctru's
//! veneers leave them (`libctru/source/svc.s`): where a call returns a value
//! through a pointer, the veneer keeps the pointer and the kernel's value
//! comes back in r1. The result code goes in r0.
//!
//! A call that blocks leaves the thread waiting; its results are written to
//! its saved registers by whatever ends the wait.

use crate::horizon::{cycles_of, thread_cpu, HleEvent, Horizon};
use crate::kernel::{Object, Reset, INVALID_HANDLE, OUT_OF_RANGE, TIMEOUT};
use crate::memory::{MemState, Perm, HEAP_BASE, PAGE};
use crate::result::{self, ResultCode};
use crate::service::PORT_NOT_FOUND;

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
        0x02 => {
            let info = h.process.space.query(h.reg(2));
            h.ret_code(result::SUCCESS);
            h.ret(1, info.base);
            h.ret(2, info.size);
            h.ret(3, info.perm.0 as u32);
            h.ret(4, info.state as u32);
            // Page flags: none locked or changed.
            h.ret(5, 0);
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
        0x1E => create_memory_block(h),
        0x1F => map_memory_block(h),
        0x20 => {
            let code = match h.kernel.object(h.reg(0)) {
                Some(Object::SharedMemory { pages, .. }) => {
                    let len = pages.len() as u32 * PAGE;
                    h.process.space.unmap(h.reg(1), len);
                    result::SUCCESS
                }
                _ => INVALID_HANDLE,
            };
            h.ret_code(code);
        }
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
        0x2A => {
            // GetSystemInfo(type, parameter). Only the application's
            // region is in use: services take no memory here.
            let used = h.process.space.fcram.used_bytes();
            let value = match (h.reg(1), h.reg(2)) {
                (0, 0 | 1) => used,
                // System and base regions; memory the kernel took.
                (0, _) | (2, _) => 0,
                // Processes the kernel starts by itself: fs, sm, pm, pxi, loader.
                (26, _) => 5,
                (kind, param) => {
                    h.note(format!("GetSystemInfo({kind}, {param}) is not implemented"));
                    0
                }
            };
            h.ret_code(result::SUCCESS);
            h.ret(1, value);
            h.ret(2, 0);
        }
        0x2B => get_process_info(h),
        0x2D => {
            let name = h
                .process
                .space
                .read_bytes(&h.mem, h.reg(1), 12)
                .map(|bytes| {
                    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                    String::from_utf8_lossy(&bytes[..end]).into_owned()
                })
                .unwrap_or_default();
            match h.connect_port(&name) {
                Some(handle) => {
                    h.ret_code(result::SUCCESS);
                    h.ret(1, handle);
                }
                None => {
                    h.note(format!("no port is called {name:?}"));
                    h.ret_code(PORT_NOT_FOUND);
                }
            }
        }
        0x32 => {
            let code = h.send_request(h.reg(0));
            h.ret_code(code);
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
        0x38 => {
            if !matches!(h.kernel.object(h.reg(1)), Some(Object::Process)) {
                return {
                    h.ret_code(INVALID_HANDLE);
                    Outcome::Done
                };
            }
            let handle = h.kernel.create_handle(Object::ResourceLimit);
            h.ret_code(result::SUCCESS);
            h.ret(1, handle);
        }
        0x39 | 0x3A => resource_limit_values(h, number == 0x39),
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

/// `GetProcessInfo(process, type)`, for the calling process.
fn get_process_info(h: &mut Horizon) {
    if !matches!(h.kernel.object(h.reg(1)), Some(Object::Process)) {
        return h.ret_code(INVALID_HANDLE);
    }
    let value = match h.reg(2) {
        // Memory used, with and without shared memory: the same here.
        0 | 2 => h.process.space.fcram.used_bytes(),
        1 | 3 => 0,
        4 | 5 => h.kernel.open_handles() as u32,
        7 => h.kernel.threads.len() as u32,
        // What turns a linear heap address into a physical one.
        20 => ctr_core::bus::phys::FCRAM_BASE.wrapping_sub(h.process.linear_base),
        _ => return h.ret_code(NOT_AVAILABLE),
    };
    h.ret_code(result::SUCCESS);
    h.ret(1, value);
    h.ret(2, 0);
}

/// `GetResourceLimitLimitValues` and `...CurrentValues(values, limit, names,
/// count)`: one 64-bit value per category named. The limits are those of
/// the application category as Citra sets them; the current values count
/// what the process has.
fn resource_limit_values(h: &mut Horizon, limits: bool) {
    let (out, names, count) = (h.reg(0), h.reg(2), h.reg(3));
    if !matches!(h.kernel.object(h.reg(1)), Some(Object::ResourceLimit)) {
        return h.ret_code(INVALID_HANDLE);
    }
    for n in 0..count.min(32) {
        let Some(name) = h.process.space.read32(&h.mem, names + n * 4) else {
            return h.ret_code(result::INVALID_ADDRESS);
        };
        let count_of =
            |kind: fn(&Object) -> bool| h.kernel.objects.iter().filter(|o| kind(o)).count() as u64;
        let value: u64 = match (name, limits) {
            // The most urgent priority a thread may have.
            (0, _) => 0x18,
            (1, true) => {
                h.process.space.fcram.used_bytes() as u64
                    + h.process.space.fcram.free_bytes() as u64
            }
            (1, false) => h.process.space.fcram.used_bytes() as u64,
            (2, true) => 32,
            (2, false) => h.kernel.threads.len() as u64,
            (3, true) => 32,
            (3, false) => count_of(|o| matches!(o, Object::Event { .. })),
            (4, true) => 32,
            (4, false) => count_of(|o| matches!(o, Object::Mutex { .. })),
            (5, true) => 8,
            (5, false) => count_of(|o| matches!(o, Object::Semaphore { .. })),
            (6, true) => 8,
            (6, false) => count_of(|o| matches!(o, Object::Timer { .. })),
            (7, true) => 16,
            (7, false) => count_of(|o| matches!(o, Object::SharedMemory { .. })),
            (8, true) => 2,
            (8, false) => count_of(|o| matches!(o, Object::Arbiter)),
            _ => 0,
        };
        let addr = out + n * 8;
        h.process.space.write32(&mut h.mem, addr, value as u32);
        h.process
            .space
            .write32(&mut h.mem, addr + 4, (value >> 32) as u32);
    }
    h.ret_code(result::SUCCESS);
}

/// What the kernel answers for an information type it does not provide.
const NOT_AVAILABLE: ResultCode = ResultCode(0xD8E0_07ED);

/// `CreateMemoryBlock(other permissions, address, size, own permissions)`,
/// as the veneer leaves the registers: memory the process already has, or
/// fresh memory if the address is zero.
fn create_memory_block(h: &mut Horizon) {
    let (other_perm, addr, size) = (h.reg(0), h.reg(1), h.reg(2));
    if size == 0 || !size.is_multiple_of(PAGE) || !addr.is_multiple_of(PAGE) {
        return h.ret_code(result::MISALIGNED_SIZE);
    }
    let pages: Option<Vec<u32>> = if addr == 0 {
        h.process.space.fcram.take(size).map(|pa| {
            if let Some(bytes) = h.mem.slice_mut(pa, size as usize) {
                bytes.fill(0);
            }
            (0..size / PAGE).map(|n| pa + n * PAGE).collect()
        })
    } else {
        (0..size / PAGE)
            .map(|n| h.process.space.v2p(addr + n * PAGE))
            .collect()
    };
    let Some(pages) = pages else {
        return h.ret_code(if addr == 0 {
            result::OUT_OF_MEMORY
        } else {
            result::INVALID_ADDRESS
        });
    };
    let handle = h.kernel.create_handle(Object::SharedMemory {
        pages,
        other_perm: other_perm as u8 & 3,
    });
    h.ret_code(result::SUCCESS);
    h.ret(1, handle);
}

/// `MapMemoryBlock(handle, address, own permissions, other permissions)`.
/// Address zero lets the kernel choose; the address chosen comes back in r1.
fn map_memory_block(h: &mut Horizon) {
    let (addr, perm) = (h.reg(1), h.reg(2));
    let Some(Object::SharedMemory { pages, other_perm }) = h.kernel.object(h.reg(0)).cloned()
    else {
        return h.ret_code(INVALID_HANDLE);
    };
    let len = pages.len() as u32 * PAGE;
    let addr = if addr == 0 {
        h.process.place_shared(len)
    } else {
        addr
    };
    // "Don't care" leaves it to what the creator allows.
    let perm = if perm & 0x1000_0000 != 0 {
        other_perm
    } else {
        perm as u8 & 3
    };
    for (n, pa) in pages.into_iter().enumerate() {
        h.process.space.map_as(
            addr + n as u32 * PAGE,
            pa,
            PAGE,
            Perm(perm),
            MemState::Shared,
        );
    }
    h.ret_code(result::SUCCESS);
    h.ret(1, addr);
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
    let mut context = thread_cpu(entry, stack & !3).save_context();
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
                    process
                        .space
                        .map_as(addr, pa, size, perm, MemState::Continuous);
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
                process.space.protect(addr0, size, perm);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::COMMAND_BUFFER;
    use ctr_fs::threedsx::build;

    /// A console whose program has not run: its one thread is current.
    fn console() -> Horizon {
        let code = 0xEF00_0003u32.to_le_bytes();
        Horizon::from_3dsx(&build([&code, &[], &[]], 0, &Default::default())).unwrap()
    }

    /// Make supervisor call `number` with `regs` in r0 up, as the trap path
    /// does, and return the caller's registers afterwards.
    fn call(h: &mut Horizon, number: u32, regs: &[u32]) -> [u32; 6] {
        for (n, &value) in regs.iter().enumerate() {
            h.cpu.set_reg(n, value);
        }
        let current = h.kernel.current.unwrap();
        h.kernel.threads[current].context = h.cpu.save_context();
        assert_eq!(super::call(h, number), Outcome::Done);
        let r = &h.kernel.threads[current].context.r;
        [r[0], r[1], r[2], r[3], r[4], r[5]]
    }

    fn command_buffer(h: &Horizon) -> u32 {
        h.kernel.threads[h.kernel.current.unwrap()].tls + COMMAND_BUFFER
    }

    fn write_words(h: &mut Horizon, addr: u32, words: &[u32]) {
        for (n, &word) in words.iter().enumerate() {
            h.process
                .space
                .write32(&mut h.mem, addr + n as u32 * 4, word);
        }
    }

    fn read_words(h: &Horizon, addr: u32, len: u32) -> Vec<u32> {
        (0..len)
            .map(|n| h.process.space.read32(&h.mem, addr + n * 4).unwrap())
            .collect()
    }

    /// Connect to a port whose name is put in the heap.
    fn port(h: &mut Horizon, name: &[u8; 8]) -> [u32; 6] {
        if h.process.space.v2p(HEAP_BASE).is_none() {
            call(h, 0x01, &[3, HEAP_BASE, 0, PAGE, 3]);
        }
        let words = [
            u32::from_le_bytes(name[..4].try_into().unwrap()),
            u32::from_le_bytes(name[4..].try_into().unwrap()),
            0,
        ];
        write_words(h, HEAP_BASE, &words);
        call(h, 0x2D, &[0, HEAP_BASE])
    }

    #[test]
    fn a_service_handle_comes_from_srv_and_an_unwritten_service_is_a_stub() {
        let mut h = console();
        let srv = port(&mut h, b"srv:\0\0\0\0")[1];
        let buffer = command_buffer(&h);
        write_words(&mut h, buffer, &[0x0001_0002, 0x20, 0]);
        assert_eq!(call(&mut h, 0x32, &[srv])[0], 0);
        assert_eq!(read_words(&h, buffer, 2), [0x0001_0040, 0]);

        let name = [u32::from_le_bytes(*b"hid:"), u32::from_le_bytes(*b"USER")];
        write_words(&mut h, buffer, &[0x0005_0100, name[0], name[1], 8, 0]);
        call(&mut h, 0x32, &[srv]);
        let reply = read_words(&h, buffer, 4);
        assert_eq!(reply[..3], [0x0005_0042, 0, 0x10]);
        let hid = reply[3];
        assert!(matches!(h.kernel.object(hid), Some(Object::Session { .. })));

        // The stub answers with success and zeros, and says so once.
        for _ in 0..2 {
            write_words(&mut h, buffer, &[0x000A_0000, 0x1234]);
            assert_eq!(call(&mut h, 0x32, &[hid])[0], 0);
        }
        assert_eq!(read_words(&h, buffer, 3), [0x000A_0240, 0, 0]);
        let notes: Vec<_> = h.log.iter().filter(|l| l.contains("hid:USER")).collect();
        assert_eq!(notes.len(), 1, "{notes:?}");

        // A second handle reaches the same service.
        write_words(&mut h, buffer, &[0x0005_0100, name[0], name[1], 8, 0]);
        call(&mut h, 0x32, &[srv]);
        let again = read_words(&h, buffer, 4)[3];
        assert_ne!(again, hid);
        assert_eq!(h.kernel.object(again), h.kernel.object(hid));

        // Strict, the stub stops the process instead.
        h.strict = true;
        write_words(&mut h, buffer, &[0x000B_0000]);
        call(&mut h, 0x32, &[hid]);
        let why = h.fatal.clone().unwrap();
        assert!(why.contains("hid:USER command 0x000b"), "{why}");
    }

    #[test]
    fn requests_need_a_session_and_ports_need_a_name_that_exists() {
        let mut h = console();
        assert_eq!(call(&mut h, 0x32, &[0x1234])[0], INVALID_HANDLE.0);
        assert_eq!(port(&mut h, b"nope\0\0\0\0")[0], PORT_NOT_FOUND.0);
    }

    #[test]
    fn a_fatal_error_report_stops_the_process() {
        let mut h = console();
        let errf = port(&mut h, b"err:f\0\0\0")[1];
        let buffer = command_buffer(&h);
        let mut message = [0u32; 33];
        message[0] = 0x0001_0800;
        message[1] = 4;
        message[2] = 0xC8A0_4554;
        message[3] = 0x0010_0040;
        message[9] = u32::from_le_bytes(*b"oops");
        write_words(&mut h, buffer, &message);
        call(&mut h, 0x32, &[errf]);
        let why = h.fatal.clone().unwrap();
        assert!(
            why.contains("result 0xc8a04554, pc 0x00100040, \"oops\""),
            "{why}"
        );
        assert!(!h.running());
    }

    #[test]
    fn a_memory_block_maps_the_same_pages_twice() {
        let mut h = console();
        let r = call(&mut h, 0x1E, &[1, 0, 2 * PAGE, 3]);
        assert_eq!(r[0], 0);
        let block = r[1];
        let first = call(&mut h, 0x1F, &[block, 0, 3, 1]);
        assert_eq!(first[..2], [0, 0x1000_0000]);
        let second = call(&mut h, 0x1F, &[block, 0x1100_0000, 0x1000_0000, 0]);
        assert_eq!(second[1], 0x1100_0000);

        h.process.space.write32(&mut h.mem, 0x1000_1FFC, 0xCAFE);
        assert_eq!(h.process.space.read32(&h.mem, 0x1100_1FFC), Some(0xCAFE));
        let info = call(&mut h, 0x02, &[0, 0, 0x1100_0010]);
        assert_eq!(
            info[1..5],
            [0x1100_0000, 2 * PAGE, 1, MemState::Shared as u32]
        );
        assert_eq!(
            h.process.space.resolve(0x1100_0000, Perm::W),
            None,
            "the creator allowed others to read only"
        );

        assert_eq!(call(&mut h, 0x20, &[block, 0x1000_0000])[0], 0);
        assert_eq!(h.process.space.v2p(0x1000_0000), None);
        assert_eq!(h.process.space.read32(&h.mem, 0x1100_1FFC), Some(0xCAFE));
    }
}
