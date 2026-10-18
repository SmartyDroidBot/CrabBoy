//! IPC messages (3dbrew, "IPC").
//!
//! A request is written to the calling thread's command buffer, at offset
//! 0x80 of its thread-local storage, and sent with `SendSyncRequest`; the
//! reply replaces it. A message starts with a header: the command in bits
//! 16-31, the count of normal parameters in bits 6-11 and the length in
//! words of the translate parameters in bits 0-5. Translate parameters carry
//! handles and buffers, each behind a descriptor.
//!
//! Services here are part of the kernel and share the process's handle
//! table and memory, so nothing needs translating on the way: a handle in a
//! request is the client's own, and a buffer descriptor is followed by the
//! client's own address, which a service reads or writes in place.

use crate::kernel::Handle;
use crate::result::ResultCode;

/// Where the command buffer lies in thread-local storage.
pub const COMMAND_BUFFER: u32 = 0x80;
/// Where a thread describes its static buffers: sixteen pairs of descriptor
/// and address.
pub const STATIC_BUFFERS: u32 = 0x180;
/// The most words a message can hold.
pub const MAX_WORDS: usize = 64;

pub fn header(command: u16, normal: usize, translate: usize) -> u32 {
    (command as u32) << 16 | (normal as u32 & 0x3F) << 6 | translate as u32 & 0x3F
}

/// A request as the client wrote it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Request {
    /// The header, then the parameters: `words[n]` is `cmdbuf[n]` in
    /// libctru's terms.
    pub words: Vec<u32>,
}

impl Request {
    pub fn header(&self) -> u32 {
        self.words[0]
    }

    pub fn command(&self) -> u16 {
        (self.header() >> 16) as u16
    }

    /// Word `n` of the message, zero past its end.
    pub fn word(&self, n: usize) -> u32 {
        self.words.get(n).copied().unwrap_or(0)
    }

    /// Two words as a 64-bit value, low word first.
    pub fn u64(&self, n: usize) -> u64 {
        self.word(n) as u64 | (self.word(n + 1) as u64) << 32
    }

    /// The number of words a header says its message has.
    pub fn len_of(header: u32) -> usize {
        (1 + (header >> 6 & 0x3F) + (header & 0x3F)) as usize
    }

    /// Up to eight bytes of a name stored in two words, cut at its length.
    pub fn name(&self, first: usize, len: usize) -> String {
        let bytes: Vec<u8> = [self.word(first), self.word(first + 1)]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .take(len.min(8))
            .take_while(|&b| b != 0)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// A reply being put together: the result code first among the normal
/// parameters, then whatever the command returns.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Reply {
    command: u16,
    normal: Vec<u32>,
    translate: Vec<u32>,
}

impl Reply {
    pub fn new(command: u16, result: ResultCode) -> Self {
        Reply {
            command,
            normal: vec![result.0],
            translate: Vec::new(),
        }
    }

    /// One more normal parameter.
    pub fn word(mut self, value: u32) -> Self {
        self.normal.push(value);
        self
    }

    pub fn u64(self, value: u64) -> Self {
        self.word(value as u32).word((value >> 32) as u32)
    }

    pub fn words(mut self, values: &[u32]) -> Self {
        self.normal.extend_from_slice(values);
        self
    }

    /// Handles for the client: copies, or moved out of the service.
    pub fn handles(mut self, handles: &[Handle], moved: bool) -> Self {
        let count = handles.len().max(1) as u32 - 1;
        self.translate
            .push(count << 26 | if moved { 0x10 } else { 0 });
        self.translate.extend_from_slice(handles);
        self
    }

    /// Data placed in the client's static buffer `id` at `addr`.
    pub fn static_buffer(mut self, id: u32, addr: u32, len: u32) -> Self {
        self.translate.push(len << 14 | (id & 0xF) << 10 | 2);
        self.translate.push(addr);
        self
    }

    /// A buffer the client mapped, handed back as the kernel does.
    pub fn mapped_buffer(mut self, descriptor: u32, addr: u32) -> Self {
        self.translate.push(descriptor);
        self.translate.push(addr);
        self
    }

    pub fn result(&self) -> u32 {
        self.normal[0]
    }

    /// The message as it is written back to the command buffer.
    pub fn encode(&self) -> Vec<u32> {
        let mut words = vec![header(
            self.command,
            self.normal.len(),
            self.translate.len(),
        )];
        words.extend_from_slice(&self.normal);
        words.extend_from_slice(&self.translate);
        words
    }
}

/// The access a mapped buffer descriptor grants, and its length; `None`
/// for any other descriptor.
pub fn mapped_buffer(descriptor: u32) -> Option<u32> {
    (descriptor & 0x8 != 0 && descriptor & 6 != 0).then_some(descriptor >> 4)
}

/// The length of a static buffer descriptor; `None` for any other.
pub fn static_buffer(descriptor: u32) -> Option<u32> {
    (descriptor & 0xE == 2).then_some(descriptor >> 14)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result;

    #[test]
    fn headers_carry_the_command_and_the_two_counts() {
        assert_eq!(header(5, 4, 0), 0x0005_0100);
        assert_eq!(Request::len_of(0x0001_0002), 3);
        let request = Request {
            words: vec![
                0x0005_0100,
                u32::from_le_bytes(*b"hid:"),
                u32::from_le_bytes(*b"USER"),
                8,
                0,
            ],
        };
        assert_eq!(request.command(), 5);
        assert_eq!(request.name(1, request.word(3) as usize), "hid:USER");
        assert_eq!(request.word(9), 0);
    }

    #[test]
    fn a_reply_puts_the_result_first_and_counts_what_follows() {
        let reply = Reply::new(5, result::SUCCESS).handles(&[7], true);
        assert_eq!(reply.encode(), [0x0005_0042, 0, 0x10, 7]);
        let reply = Reply::new(2, result::SUCCESS)
            .word(9)
            .static_buffer(1, 0x1000, 0x20);
        assert_eq!(
            reply.encode(),
            [0x0002_0082, 0, 9, 0x20 << 14 | 1 << 10 | 2, 0x1000]
        );
        assert_eq!(mapped_buffer(0x0000_000C | 0x40 << 4), Some(0x40));
        assert_eq!(mapped_buffer(0x20), None);
        assert_eq!(static_buffer(0x20 << 14 | 2), Some(0x20));
    }
}
