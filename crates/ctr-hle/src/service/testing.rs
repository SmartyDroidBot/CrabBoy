//! A console to try services on: a program that only spins, so its one
//! thread stays current and its command buffer is free, with heap for
//! buffers and two static buffers described.

use crate::horizon::Horizon;
use crate::ipc::{COMMAND_BUFFER, STATIC_BUFFERS};
use crate::kernel::{Handle, Object};
use crate::memory::{Perm, HEAP_BASE, PAGE};
use ctr_fs::threedsx::build;

/// Where the heap the tests use starts, and how long it is.
pub const HEAP: u32 = HEAP_BASE;
pub const HEAP_LEN: u32 = 16 * PAGE;
/// Static buffer 0 of the thread: the last page of the heap.
pub const STATIC: u32 = HEAP_BASE + HEAP_LEN - PAGE;

pub fn console() -> Horizon {
    let code = 0xEAFF_FFFEu32.to_le_bytes(); // b .
    let mut h = Horizon::from_3dsx(&build([&code, &[], &[]], 0, &Default::default())).unwrap();
    h.process.space.allocate(HEAP, HEAP_LEN, Perm::RW).unwrap();
    let tls = h.kernel.threads[h.kernel.current.unwrap()].tls;
    for id in 0..2 {
        let at = tls + STATIC_BUFFERS + id * 8;
        let descriptor = (PAGE / 2) << 14 | id << 10 | 2;
        write_words(&mut h, at, &[descriptor, STATIC + id * PAGE / 2]);
    }
    h
}

pub fn write_words(h: &mut Horizon, addr: u32, words: &[u32]) {
    for (n, &word) in words.iter().enumerate() {
        h.process
            .space
            .write32(&mut h.mem, addr + n as u32 * 4, word);
    }
}

pub fn read_words(h: &Horizon, addr: u32, len: u32) -> Vec<u32> {
    (0..len)
        .map(|n| h.process.space.read32(&h.mem, addr + n * 4).unwrap())
        .collect()
}

pub fn read_bytes(h: &Horizon, addr: u32, len: usize) -> Vec<u8> {
    h.process.space.read_bytes(&h.mem, addr, len).unwrap()
}

pub fn write_bytes(h: &mut Horizon, addr: u32, bytes: &[u8]) {
    h.process
        .space
        .write_bytes(&mut h.mem, addr, bytes)
        .unwrap();
}

/// A session with the service called `name`.
pub fn connect(h: &mut Horizon, name: &str) -> Handle {
    h.connect_service(name)
}

/// Send `words` on `session` and return the reply, as many words as its
/// header says.
pub fn send(h: &mut Horizon, session: Handle, words: &[u32]) -> Vec<u32> {
    let buffer = h.kernel.threads[h.kernel.current.unwrap()].tls + COMMAND_BUFFER;
    write_words(h, buffer, words);
    assert_eq!(h.send_request(session).0, 0);
    let header = read_words(h, buffer, 1)[0];
    read_words(h, buffer, crate::ipc::Request::len_of(header) as u32)
}

/// Connect to `name` and send it one request.
pub fn request(h: &mut Horizon, name: &str, words: &[u32]) -> Vec<u32> {
    let session = connect(h, name);
    send(h, session, words)
}

/// Whether `handle` names an event, and if so whether it is signalled.
pub fn event(h: &Horizon, handle: Handle) -> Option<bool> {
    match h.kernel.object(handle) {
        Some(Object::Event { signalled, .. }) => Some(*signalled),
        _ => None,
    }
}

/// Run the clock to `cycles` from now, delivering what falls due. The
/// program only spins, so its thread stays current.
pub fn run_for(h: &mut Horizon, cycles: u64) {
    use emu_core::System;
    let end = h.clock.now() + cycles;
    while h.clock.now() < end {
        h.step();
    }
}
