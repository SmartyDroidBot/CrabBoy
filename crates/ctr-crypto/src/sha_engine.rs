//! The SHA engine at 0x1000A000 (3dbrew, "SHA Registers"; GBATEK, "3DS
//! Crypto - SHA Registers").
//!
//! Writing bit 0 of `SHA_CNT` starts a hash: the byte counter clears and the
//! initial state of the mode in bits 5:4 is loaded (0 SHA-256, 1 SHA-224,
//! 2 and 3 SHA-1). Data written anywhere in the 64-byte FIFO window at +0x80
//! is consumed in order, a block at a time. Bit 1 finishes: the engine
//! appends the padding and the length itself. `SHA_HASH` at +0x40 holds the
//! state; with bit 3 set its words read so that memory holds the digest in
//! the standard byte order. Everything completes at once, so the busy bits
//! always read as clear.

use crate::sha::{compress1, compress256, padding, SHA1_INIT, SHA224_INIT, SHA256_INIT};

const CNT_START: u32 = 1 << 0;
const CNT_FINAL: u32 = 1 << 1;
const CNT_BIG_ENDIAN: u32 = 1 << 3;
/// DMA enables, output byte order, mode and the readback enable.
const CNT_STORED: u32 = 0x0000_053C;

#[derive(Default)]
pub struct ShaEngine {
    cnt: u32,
    /// Bytes hashed so far, not counting what waits in `pending`.
    byte_count: u32,
    state: [u32; 8],
    pending: Vec<u8>,
}

impl ShaEngine {
    pub fn new() -> Self {
        ShaEngine::default()
    }

    fn sha1(&self) -> bool {
        self.cnt >> 4 & 3 >= 2
    }

    fn compress(&mut self, block: &[u8; 64]) {
        if self.sha1() {
            let mut state = [0u32; 5];
            state.copy_from_slice(&self.state[..5]);
            compress1(&mut state, block);
            self.state[..5].copy_from_slice(&state);
        } else {
            compress256(&mut self.state, block);
        }
    }

    pub fn read(&self, offset: u32) -> u32 {
        match offset {
            0x000 => self.cnt & CNT_STORED,
            0x004 => self.byte_count,
            0x040..=0x05C => {
                let word = self.state[((offset - 0x040) / 4) as usize];
                if self.cnt & CNT_BIG_ENDIAN != 0 {
                    word.swap_bytes()
                } else {
                    word
                }
            }
            _ => 0,
        }
    }

    /// Write the byte lanes of `mask` of the word at `offset`.
    pub fn write(&mut self, offset: u32, value: u32, mask: u32) {
        match offset {
            0x000 => {
                self.cnt = value & CNT_STORED;
                if value & CNT_START != 0 {
                    self.byte_count = 0;
                    self.pending.clear();
                    self.state = match value >> 4 & 3 {
                        0 => SHA256_INIT,
                        1 => SHA224_INIT,
                        _ => {
                            let mut state = [0u32; 8];
                            state[..5].copy_from_slice(&SHA1_INIT);
                            state
                        }
                    };
                }
                if value & CNT_FINAL != 0 {
                    let total = self.byte_count as u64 + self.pending.len() as u64;
                    let tail = std::mem::take(&mut self.pending);
                    for block in padding(&tail, total) {
                        self.compress(&block);
                    }
                    self.byte_count = total as u32;
                }
            }
            0x004 => self.byte_count = self.byte_count & !mask | value & mask,
            0x040..=0x05C => {
                let index = ((offset - 0x040) / 4) as usize;
                let big = self.cnt & CNT_BIG_ENDIAN != 0;
                let old = if big {
                    self.state[index].swap_bytes()
                } else {
                    self.state[index]
                };
                let new = old & !mask | value & mask;
                self.state[index] = if big { new.swap_bytes() } else { new };
            }
            0x080..=0x0BC => {
                // The bytes of the lanes written, lowest first.
                for (lane, byte) in value.to_le_bytes().into_iter().enumerate() {
                    if mask >> (lane * 8) & 0xFF != 0 {
                        self.pending.push(byte);
                    }
                }
                while self.pending.len() >= 64 {
                    let mut block = [0u8; 64];
                    block.copy_from_slice(&self.pending[..64]);
                    self.pending.drain(..64);
                    self.compress(&block);
                    self.byte_count = self.byte_count.wrapping_add(64);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::chunks_exact_to_as_chunks)]
mod tests {
    use super::*;

    const MODE_SHA256: u32 = 0 << 4;
    const MODE_SHA224: u32 = 1 << 4;
    const MODE_SHA1: u32 = 2 << 4;

    /// Hash the way a driver does: whole words, then a halfword and a byte
    /// for the tail, then read the digest out of the hash registers.
    fn digest(mode: u32, message: &[u8], len: usize) -> Vec<u8> {
        let mut sha = ShaEngine::new();
        sha.write(0x000, mode | CNT_BIG_ENDIAN | CNT_START, !0);
        let mut chunks = message.chunks_exact(4);
        for (n, chunk) in (&mut chunks).enumerate() {
            let word = u32::from_le_bytes(chunk.try_into().unwrap());
            sha.write(0x080 + ((n as u32 * 4) & 0x3C), word, !0);
        }
        for (n, byte) in chunks.remainder().iter().enumerate() {
            sha.write(0x080, (*byte as u32) << (n * 8), 0xFF << (n * 8));
        }
        let cnt = sha.read(0x000);
        sha.write(0x000, cnt | CNT_FINAL, !0);
        assert_eq!(sha.read(0x000) & 3, 0, "idle");
        assert_eq!(sha.read(0x004) as usize, message.len());
        (0..8)
            .flat_map(|n| sha.read(0x040 + n * 4).to_le_bytes())
            .take(len)
            .collect()
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    /// FIPS 180-4 examples.
    #[test]
    fn digests_in_every_mode() {
        assert_eq!(
            digest(MODE_SHA256, b"abc", 32),
            hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(
            digest(MODE_SHA224, b"abc", 28),
            hex("23097d223405d8228642a477bda255b32aadbce4bda0b3f7e36c9da7")
        );
        assert_eq!(
            digest(MODE_SHA1, b"abc", 20),
            hex("a9993e364706816aba3e25717850c26c9cd0d89d")
        );
    }

    #[test]
    fn a_message_longer_than_a_block() {
        let message = vec![b'a'; 200];
        let expected = {
            let mut state = SHA256_INIT;
            let mut chunks = message.chunks_exact(64);
            for chunk in &mut chunks {
                compress256(&mut state, chunk.try_into().unwrap());
            }
            for block in padding(chunks.remainder(), 200) {
                compress256(&mut state, &block);
            }
            state
                .iter()
                .flat_map(|w| w.to_be_bytes())
                .collect::<Vec<u8>>()
        };
        assert_eq!(digest(MODE_SHA256, &message, 32), expected);
    }

    #[test]
    fn little_endian_readout_gives_the_state_words() {
        let mut sha = ShaEngine::new();
        sha.write(0x000, CNT_START, !0);
        assert_eq!(sha.read(0x040), 0x6A09_E667);
        sha.write(0x000, CNT_BIG_ENDIAN, !0);
        assert_eq!(sha.read(0x040), 0x67E6_096A);
    }
}
