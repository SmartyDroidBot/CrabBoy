//! Answers given from a table, and what services share to fill replies.
//!
//! Many commands of a service only need to succeed with fixed values: a
//! state nobody changes here, an event that never fires because what it
//! announces (a network connection, a StreetPass) never happens. A [`Table`]
//! answers those. An event a table hands out is a real kernel object, made
//! the first time its command is asked and handed out again after that, so
//! a client can wait on it (with a timeout) and close it like any other.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::kernel::{Handle, ObjId, Object, Reset};
use crate::result;
use crate::service::Service;
use std::collections::BTreeMap;

/// How a table answers a command: success, then `words`, then copies of
/// `events` event handles.
#[derive(Clone, Copy, Debug)]
pub struct Answer {
    pub command: u16,
    pub words: &'static [u32],
    pub events: u8,
}

/// Success and nothing else.
pub const fn ok(command: u16) -> Answer {
    Answer {
        command,
        words: &[],
        events: 0,
    }
}

/// Success and `words`.
pub const fn words(command: u16, words: &'static [u32]) -> Answer {
    Answer {
        command,
        words,
        events: 0,
    }
}

/// Success and `count` events.
pub const fn events(command: u16, count: u8) -> Answer {
    Answer {
        command,
        words: &[],
        events: count,
    }
}

/// Answers by command, with the events they hand out.
pub struct Table {
    answers: &'static [Answer],
    events: BTreeMap<u16, Vec<ObjId>>,
}

impl Table {
    pub fn new(answers: &'static [Answer]) -> Self {
        Table {
            answers,
            events: BTreeMap::new(),
        }
    }

    /// The reply to `request` if the table has one.
    pub fn answer(&mut self, h: &mut Horizon, request: &Request) -> Option<Reply> {
        let command = request.command();
        let answer = self.answers.iter().find(|a| a.command == command)?;
        let reply = Reply::new(command, result::SUCCESS).words(answer.words);
        if answer.events == 0 {
            return Some(reply);
        }
        let made = self
            .events
            .entry(command)
            .or_insert_with(|| (0..answer.events).map(|_| new_event(h)).collect());
        let handles: Vec<Handle> = made.iter().map(|&e| h.kernel.open(e)).collect();
        Some(reply.handles(&handles, false))
    }
}

/// A service that is only a table: what is not in it is unhandled.
pub struct Fixed {
    name: &'static str,
    table: Table,
}

impl Fixed {
    pub fn boxed(name: &'static str, answers: &'static [Answer]) -> Box<dyn Service> {
        Box::new(Fixed {
            name,
            table: Table::new(answers),
        })
    }
}

impl Service for Fixed {
    fn name(&self) -> &str {
        self.name
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        match self.table.answer(h, request) {
            Some(reply) => reply,
            None => h.unhandled(self.name, request),
        }
    }
}

/// A one-shot event, not signalled.
pub fn new_event(h: &mut Horizon) -> ObjId {
    h.kernel.create(Object::Event {
        signalled: false,
        reset: Reset::OneShot,
    })
}

/// A handle for the event in `slot`, made on first use.
pub fn event_handle(h: &mut Horizon, slot: &mut Option<ObjId>) -> Handle {
    let id = *slot.get_or_insert_with(|| new_event(h));
    h.kernel.open(id)
}

/// Signal the event a client handed over as `handle`, if it is one.
pub fn signal_handle(h: &mut Horizon, handle: Handle) {
    if let Some(id) = h.kernel.resolve(handle) {
        if matches!(h.kernel.objects[id], Object::Event { .. }) {
            h.kernel.signal(id);
        }
    }
}

/// Copy `bytes` to the client's static buffer `id`, cut to its room, and
/// add the descriptor that tells it so.
pub fn to_static(h: &mut Horizon, reply: Reply, id: u32, bytes: &[u8]) -> Reply {
    let (addr, room) = h.static_buffer(id);
    let len = bytes.len().min(room as usize);
    h.process.space.write_bytes(&mut h.mem, addr, &bytes[..len]);
    reply.static_buffer(id, addr, len as u32)
}

/// Copy `bytes` to the buffer the client mapped with `descriptor` at
/// `addr`, cut to its length, and hand the buffer back.
pub fn to_mapped(h: &mut Horizon, reply: Reply, descriptor: u32, addr: u32, bytes: &[u8]) -> Reply {
    let len = bytes.len().min((descriptor >> 4) as usize);
    h.process.space.write_bytes(&mut h.mem, addr, &bytes[..len]);
    reply.mapped_buffer(descriptor, addr)
}

/// The bytes of the buffer the client mapped with `descriptor` at `addr`.
pub fn from_mapped(h: &Horizon, descriptor: u32, addr: u32) -> Vec<u8> {
    h.process
        .space
        .read_bytes(&h.mem, addr, (descriptor >> 4) as usize)
        .unwrap_or_default()
}

/// UTF-16 of `text`, `len` bytes, zero-filled.
pub fn utf16(text: &str, len: usize) -> Vec<u8> {
    let mut out: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    out.resize(len, 0);
    out
}

/// Little-endian words of `bytes`, the last one zero-filled.
pub fn words_of(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks(4)
        .map(|c| {
            let mut w = [0; 4];
            w[..c.len()].copy_from_slice(c);
            u32::from_le_bytes(w)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::testing::{console, event};

    #[test]
    fn a_table_answers_with_its_words_and_the_same_events_each_time() {
        static ANSWERS: [Answer; 2] = [words(1, &[7, 8]), events(2, 2)];
        let mut h = console();
        let mut table = Table::new(&ANSWERS);
        let request = |command: u16| Request {
            words: vec![crate::ipc::header(command, 0, 0)],
        };
        let reply = table.answer(&mut h, &request(1)).unwrap();
        assert_eq!(reply.encode(), [0x0001_00C0, 0, 7, 8]);
        let first = table.answer(&mut h, &request(2)).unwrap().encode();
        assert_eq!(first[..3], [0x0002_0043, 0, 1 << 26]);
        assert_eq!(event(&h, first[3]), Some(false));
        let again = table.answer(&mut h, &request(2)).unwrap().encode();
        assert_ne!(first[3], again[3], "a new handle");
        assert_eq!(h.kernel.resolve(first[3]), h.kernel.resolve(again[3]));
        assert!(table.answer(&mut h, &request(3)).is_none());
        assert_eq!(words_of(&[1, 2, 3, 4, 5]), [0x0403_0201, 5]);
    }
}
