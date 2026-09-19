//! The AES engine at 0x10009000 (3dbrew, "AES Registers"; GBATEK, "3DS Crypto
//! - AES Registers").
//!
//! Software programs a block count, selects a key slot and a mode in
//! `AES_CNT`, then feeds 16-byte blocks as four words through `WRFIFO` and
//! collects them from `RDFIFO`. `AES_CNT` also says how the words relate to
//! the bytes of a block: bits 23 and 22 give the byte order of input and
//! output words, bits 25 and 24 whether the four words of a block arrive in
//! normal or reversed order.
//!
//! Sixty-four key slots each hold a normal key, a keyX and a keyY. Writing
//! the last word of a keyY runs the hardware key generator, which derives the
//! normal key from X and Y and a constant. The constants are not part of
//! this repository: they default to zero, which keeps everything
//! deterministic but derives the wrong keys, and can be supplied at run
//! time. A slot's key only takes effect when the slot is selected through
//! `AES_KEYSEL` and bit 26 of `AES_CNT`.
//!
//! CCM is only partly modelled: the payload is transformed with the CCM
//! counter blocks, but no MAC is computed or verified.

use crate::aes::Aes128;
use std::collections::VecDeque;

const FIFO_WORDS: usize = 16;

const CNT_FLUSH_WRITE: u32 = 1 << 10;
const CNT_FLUSH_READ: u32 = 1 << 11;
const CNT_OUTPUT_BIG: u32 = 1 << 22;
const CNT_INPUT_BIG: u32 = 1 << 23;
const CNT_OUTPUT_NORMAL: u32 = 1 << 24;
const CNT_INPUT_NORMAL: u32 = 1 << 25;
const CNT_SELECT_KEY: u32 = 1 << 26;
const CNT_IRQ: u32 = 1 << 30;
const CNT_BUSY: u32 = 1 << 31;
/// Bits that read back as written.
const CNT_STORED: u32 = 0x7BDF_F000;

const KEYCNT_TWL_GENERATOR: u8 = 1 << 6;

#[derive(Clone, Copy, Default)]
struct Slot {
    normal: [u8; 16],
    x: [u8; 16],
    y: [u8; 16],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KeyPart {
    Normal,
    X,
    Y,
}

#[derive(Default)]
struct KeyFifo {
    words: Vec<u32>,
}

pub struct AesEngine {
    cnt: u32,
    blocks: u16,
    mac_blocks: u16,
    key_select: u8,
    key_count: u8,
    /// The counter, IV or nonce, as the sixteen bytes AES sees.
    ctr: [u8; 16],
    mac: [u8; 16],
    slots: [Slot; 64],
    key_fifos: [KeyFifo; 3],
    cipher: Aes128,
    write_fifo: VecDeque<u32>,
    read_fifo: VecDeque<u32>,
    last_read: u32,
    blocks_left: u32,
    /// The running counter or chaining value of the current operation.
    chain: [u8; 16],
    generator_constants: [u128; 2],
    irq: bool,
}

impl Default for AesEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// The four bytes of a block that `word` carries.
fn word_to_bytes(word: u32, big: bool) -> [u8; 4] {
    if big {
        word.to_le_bytes()
    } else {
        word.to_be_bytes()
    }
}

fn bytes_to_word(bytes: [u8; 4], big: bool) -> u32 {
    if big {
        u32::from_le_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes)
    }
}

/// Assemble a block from four FIFO words.
fn block_from_words(words: &[u32], big: bool, normal_order: bool) -> [u8; 16] {
    let mut block = [0u8; 16];
    for (n, word) in words.iter().enumerate() {
        let position = if normal_order { n } else { 3 - n };
        block[position * 4..position * 4 + 4].copy_from_slice(&word_to_bytes(*word, big));
    }
    block
}

impl AesEngine {
    pub fn new() -> Self {
        AesEngine {
            cnt: 0,
            blocks: 0,
            mac_blocks: 0,
            key_select: 0,
            key_count: 0,
            ctr: [0; 16],
            mac: [0; 16],
            slots: [Slot::default(); 64],
            key_fifos: Default::default(),
            cipher: Aes128::new(&[0; 16]),
            write_fifo: VecDeque::new(),
            read_fifo: VecDeque::new(),
            last_read: 0,
            blocks_left: 0,
            chain: [0; 16],
            generator_constants: [0; 2],
            irq: false,
        }
    }

    /// Supply the constants of the 3DS and the DSi key generators, which
    /// come from the user's own boot ROM dump.
    pub fn set_generator_constants(&mut self, ctr: u128, twl: u128) {
        self.generator_constants = [ctr, twl];
    }

    /// Whether the engine asked for its interrupt since the last call.
    pub fn take_irq(&mut self) -> bool {
        std::mem::take(&mut self.irq)
    }

    /// The normal key of `slot`, for tests and diagnostics.
    pub fn normal_key(&self, slot: usize) -> [u8; 16] {
        self.slots[slot & 0x3F].normal
    }

    fn generate(&mut self, slot: usize, twl: bool) {
        let x = u128::from_be_bytes(self.slots[slot].x);
        let y = u128::from_be_bytes(self.slots[slot].y);
        let normal = if twl {
            ((x ^ y).wrapping_add(self.generator_constants[1])).rotate_left(42)
        } else {
            ((x.rotate_left(2) ^ y).wrapping_add(self.generator_constants[0])).rotate_right(41)
        };
        self.slots[slot].normal = normal.to_be_bytes();
    }

    /// Read the word at `offset`.
    pub fn read(&mut self, offset: u32) -> u32 {
        match offset {
            0x000 => {
                self.cnt & (CNT_STORED | CNT_BUSY)
                    | self.write_fifo.len() as u32
                    | (self.read_fifo.len() as u32) << 5
            }
            0x00C => {
                if let Some(word) = self.read_fifo.pop_front() {
                    self.last_read = word;
                    self.pump();
                }
                self.last_read
            }
            0x010 => self.key_select as u32 | (self.key_count as u32) << 8,
            _ => 0,
        }
    }

    /// Write the byte lanes of `mask` of the word at `offset`.
    pub fn write(&mut self, offset: u32, value: u32, mask: u32) {
        match offset {
            0x000 => self.write_cnt(value),
            0x004 => {
                if mask & 0xFFFF != 0 {
                    self.mac_blocks = value as u16;
                }
                if mask >> 16 != 0 {
                    self.blocks = (value >> 16) as u16;
                }
            }
            0x008 => {
                if self.write_fifo.len() < FIFO_WORDS {
                    self.write_fifo.push_back(value);
                }
                self.pump();
            }
            0x010 => {
                if mask & 0xFF != 0 {
                    self.key_select = value as u8 & 0x3F;
                }
                if mask & 0xFF00 != 0 {
                    let key_count = (value >> 8) as u8;
                    // Bit 7 flushes the key FIFOs and does not stick.
                    if key_count & 0x80 != 0 {
                        self.flush_key_fifos();
                    }
                    self.key_count = key_count & 0x7F;
                }
            }
            // The counter and the MAC: word 0 is the least significant.
            0x020..=0x02C => self.ctr = self.with_word(self.ctr, (offset - 0x020) / 4, value),
            0x030..=0x03C => self.mac = self.with_word(self.mac, (offset - 0x030) / 4, value),
            0x040..=0x0FC => self.write_twl_key(offset - 0x040, value),
            0x100 => self.write_key_fifo(KeyPart::Normal, value),
            0x104 => self.write_key_fifo(KeyPart::X, value),
            0x108 => self.write_key_fifo(KeyPart::Y, value),
            _ => {}
        }
    }

    /// `bytes` with little-endian word `index` replaced, under the input
    /// byte order.
    fn with_word(&self, mut bytes: [u8; 16], index: u32, value: u32) -> [u8; 16] {
        let at = (3 - index as usize) * 4;
        bytes[at..at + 4].copy_from_slice(&word_to_bytes(value, self.cnt & CNT_INPUT_BIG != 0));
        bytes
    }

    fn flush_key_fifos(&mut self) {
        for fifo in self.key_fifos.iter_mut() {
            fifo.words.clear();
        }
    }

    fn write_cnt(&mut self, value: u32) {
        if self.cnt & CNT_BUSY != 0 {
            // Locked while busy; clearing the start bit stops the engine.
            if value & CNT_BUSY == 0 {
                self.cnt &= !CNT_BUSY;
                self.blocks_left = 0;
            }
            return;
        }
        if (self.cnt ^ value) & CNT_INPUT_NORMAL != 0 {
            self.flush_key_fifos();
        }
        self.cnt = value & CNT_STORED;
        if value & CNT_FLUSH_WRITE != 0 {
            self.write_fifo.clear();
        }
        if value & CNT_FLUSH_READ != 0 {
            self.read_fifo.clear();
        }
        if value & CNT_SELECT_KEY != 0 {
            self.cipher = Aes128::new(&self.slots[self.key_select as usize].normal);
        }
        if value & CNT_BUSY != 0 {
            self.blocks_left = self.blocks as u32;
            self.chain = self.ctr;
            if self.mode() < 2 {
                // CCM: the counter block is flags, the 12-byte nonce and a
                // three-byte counter that starts at one.
                let mut first = [0u8; 16];
                first[0] = 2;
                first[1..13].copy_from_slice(&self.ctr[..12]);
                first[15] = 1;
                self.chain = first;
            }
            if self.blocks_left != 0 {
                self.cnt |= CNT_BUSY;
                self.pump();
            } else {
                self.finish();
            }
        }
    }

    fn mode(&self) -> u32 {
        self.cnt >> 27 & 7
    }

    fn finish(&mut self) {
        self.cnt &= !CNT_BUSY;
        if self.cnt & CNT_IRQ != 0 {
            self.irq = true;
        }
    }

    /// Process blocks while there is input, room for output, and work left.
    fn pump(&mut self) {
        while self.cnt & CNT_BUSY != 0
            && self.write_fifo.len() >= 4
            && self.read_fifo.len() + 4 <= FIFO_WORDS
        {
            let words: Vec<u32> = self.write_fifo.drain(..4).collect();
            let input = block_from_words(
                &words,
                self.cnt & CNT_INPUT_BIG != 0,
                self.cnt & CNT_INPUT_NORMAL != 0,
            );
            let output = self.transform(input);
            let big = self.cnt & CNT_OUTPUT_BIG != 0;
            let normal = self.cnt & CNT_OUTPUT_NORMAL != 0;
            for n in 0..4 {
                let position = if normal { n } else { 3 - n };
                let mut bytes = [0u8; 4];
                bytes.copy_from_slice(&output[position * 4..position * 4 + 4]);
                self.read_fifo.push_back(bytes_to_word(bytes, big));
            }
            self.blocks_left -= 1;
            if self.blocks_left == 0 {
                self.finish();
            }
        }
    }

    fn transform(&mut self, input: [u8; 16]) -> [u8; 16] {
        let xor = |a: [u8; 16], b: [u8; 16]| -> [u8; 16] {
            let mut out = [0u8; 16];
            for (n, byte) in out.iter_mut().enumerate() {
                *byte = a[n] ^ b[n];
            }
            out
        };
        match self.mode() {
            // Counter modes, CCM included: the counter is a big-endian number.
            0..=3 => {
                let pad = self.cipher.encrypt_block(&self.chain);
                let next = u128::from_be_bytes(self.chain).wrapping_add(1);
                self.chain = next.to_be_bytes();
                xor(input, pad)
            }
            4 => {
                let output = xor(self.cipher.decrypt_block(&input), self.chain);
                self.chain = input;
                output
            }
            5 => {
                let output = self.cipher.encrypt_block(&xor(input, self.chain));
                self.chain = output;
                output
            }
            6 => self.cipher.decrypt_block(&input),
            _ => self.cipher.encrypt_block(&input),
        }
    }

    /// Slots 0-3 are written in place, a word at a time, and always use the
    /// DSi key generator; any write takes effect at once.
    fn write_twl_key(&mut self, offset: u32, value: u32) {
        let slot = (offset / 0x30) as usize;
        let part = offset % 0x30 / 0x10;
        let word = offset % 0x10 / 4;
        let target = match part {
            0 => self.slots[slot].normal,
            1 => self.slots[slot].x,
            _ => self.slots[slot].y,
        };
        let updated = self.with_word(target, word, value);
        match part {
            0 => self.slots[slot].normal = updated,
            1 => self.slots[slot].x = updated,
            _ => self.slots[slot].y = updated,
        }
        if part != 0 {
            self.generate(slot, true);
        }
    }

    fn write_key_fifo(&mut self, part: KeyPart, value: u32) {
        let fifo = &mut self.key_fifos[part as usize];
        fifo.words.push(value);
        if fifo.words.len() < 4 {
            return;
        }
        let words = std::mem::take(&mut fifo.words);
        let key = block_from_words(
            &words,
            self.cnt & CNT_INPUT_BIG != 0,
            self.cnt & CNT_INPUT_NORMAL != 0,
        );
        let slot = (self.key_count & 0x3F) as usize;
        if slot < 4 {
            // These slots only take the direct registers.
            return;
        }
        match part {
            KeyPart::Normal => self.slots[slot].normal = key,
            KeyPart::X => self.slots[slot].x = key,
            KeyPart::Y => {
                self.slots[slot].y = key;
                self.generate(slot, self.key_count & KEYCNT_TWL_GENERATOR != 0);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::chunks_exact_to_as_chunks)]
mod tests {
    use super::*;

    const ALL_NORMAL: u32 = CNT_INPUT_BIG | CNT_INPUT_NORMAL | CNT_OUTPUT_BIG | CNT_OUTPUT_NORMAL;
    const MODE_CTR: u32 = 2 << 27;
    const MODE_CBC_DECRYPT: u32 = 4 << 27;
    const MODE_CBC_ENCRYPT: u32 = 5 << 27;
    const MODE_ECB_ENCRYPT: u32 = 7 << 27;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    /// What GodMode9's driver does: little-endian loads of the byte stream.
    fn words(bytes: &[u8]) -> Vec<u32> {
        bytes
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect()
    }

    fn set_normal_key(aes: &mut AesEngine, slot: u8, key: &[u8]) {
        aes.write(0x000, ALL_NORMAL, !0);
        aes.write(0x010, (slot as u32 | 0x80) << 8, 0xFF00);
        for word in words(key) {
            aes.write(0x100, word, !0);
        }
        aes.write(0x010, slot as u32, 0xFF);
        aes.write(0x000, ALL_NORMAL | CNT_SELECT_KEY, !0);
    }

    fn set_ctr(aes: &mut AesEngine, iv: &[u8]) {
        let iv = words(iv);
        for n in 0..4 {
            aes.write(0x020 + n as u32 * 4, iv[3 - n], !0);
        }
    }

    fn run(aes: &mut AesEngine, mode: u32, input: &[u8]) -> Vec<u8> {
        aes.write(0x000, 0, !0);
        aes.write(0x004, (input.len() as u32 / 16) << 16, !0);
        aes.write(
            0x000,
            mode | ALL_NORMAL | CNT_BUSY | CNT_FLUSH_READ | CNT_FLUSH_WRITE,
            !0,
        );
        let mut output = Vec::new();
        for block in input.chunks_exact(16) {
            for word in words(block) {
                aes.write(0x008, word, !0);
            }
            assert_eq!(aes.read(0x000) >> 5 & 0x1F, 4, "a block is ready");
            for _ in 0..4 {
                output.extend(aes.read(0x00C).to_le_bytes());
            }
        }
        assert_eq!(aes.read(0x000) & CNT_BUSY, 0);
        output
    }

    const KEY: &str = "2b7e151628aed2a6abf7158809cf4f3c";
    const PLAIN: &str = "6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51";

    /// NIST SP 800-38A, F.1.1.
    #[test]
    fn ecb_through_the_fifos() {
        let mut aes = AesEngine::new();
        set_normal_key(&mut aes, 0x11, &hex(KEY));
        assert_eq!(
            run(&mut aes, MODE_ECB_ENCRYPT, &hex(PLAIN)),
            hex("3ad77bb40d7a3660a89ecaf32466ef97f5d3d58503b9699de785895a96fdbaaf")
        );
    }

    /// NIST SP 800-38A, F.5.1.
    #[test]
    fn ctr_counts_as_a_big_endian_number() {
        let mut aes = AesEngine::new();
        set_normal_key(&mut aes, 0x2C, &hex(KEY));
        set_ctr(&mut aes, &hex("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"));
        assert_eq!(
            run(&mut aes, MODE_CTR, &hex(PLAIN)),
            hex("874d6191b620e3261bef6864990db6ce9806f66b7970fdff8617187bb9fffdff")
        );
    }

    /// NIST SP 800-38A, F.2.1 and F.2.2.
    #[test]
    fn cbc_both_ways() {
        let mut aes = AesEngine::new();
        set_normal_key(&mut aes, 0x30, &hex(KEY));
        let cipher = hex("7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2");
        set_ctr(&mut aes, &hex("000102030405060708090a0b0c0d0e0f"));
        assert_eq!(run(&mut aes, MODE_CBC_ENCRYPT, &hex(PLAIN)), cipher);
        set_ctr(&mut aes, &hex("000102030405060708090a0b0c0d0e0f"));
        assert_eq!(run(&mut aes, MODE_CBC_DECRYPT, &cipher), hex(PLAIN));
    }

    #[test]
    fn a_key_takes_effect_only_when_its_slot_is_selected() {
        let mut aes = AesEngine::new();
        set_normal_key(&mut aes, 0x11, &hex(KEY));
        let expected = run(&mut aes, MODE_ECB_ENCRYPT, &hex(PLAIN));
        // Overwrite the slot without selecting it again.
        aes.write(0x010, 0x91 << 8, 0xFF00);
        for word in [0u32; 4] {
            aes.write(0x100, word, !0);
        }
        assert_eq!(run(&mut aes, MODE_ECB_ENCRYPT, &hex(PLAIN)), expected);
        aes.write(0x000, ALL_NORMAL | CNT_SELECT_KEY, !0);
        assert_ne!(run(&mut aes, MODE_ECB_ENCRYPT, &hex(PLAIN)), expected);
    }

    #[test]
    fn the_key_generator_runs_when_key_y_completes() {
        let mut aes = AesEngine::new();
        aes.set_generator_constants(5, 0);
        aes.write(0x000, ALL_NORMAL, !0);
        aes.write(0x010, 0x84 << 8, 0xFF00);
        for word in words(&[0u8; 12])
            .into_iter()
            .chain([u32::from_le_bytes([0, 0, 0, 1])])
        {
            aes.write(0x104, word, !0);
        }
        assert_eq!(aes.normal_key(4), [0; 16], "only key Y triggers it");
        for word in [0u32; 4] {
            aes.write(0x108, word, !0);
        }
        // ((1 rol 2) xor 0) + 5 = 9, rotated right by 41.
        assert_eq!(
            u128::from_be_bytes(aes.normal_key(4)),
            9u128.rotate_right(41)
        );
    }

    #[test]
    fn reversed_word_order_and_little_endian_words() {
        let mut aes = AesEngine::new();
        set_normal_key(&mut aes, 0x11, &hex(KEY));
        let block = hex(&PLAIN[..32]);
        aes.write(0x000, 0, !0);
        aes.write(0x004, 1 << 16, !0);
        aes.write(0x000, MODE_ECB_ENCRYPT | CNT_BUSY, !0);
        // Reversed order, little-endian words: the last four bytes first.
        for chunk in block.chunks_exact(4).rev() {
            aes.write(0x008, u32::from_be_bytes(chunk.try_into().unwrap()), !0);
        }
        let mut output = [0u8; 16];
        for chunk in output.chunks_exact_mut(4).rev() {
            chunk.copy_from_slice(&aes.read(0x00C).to_be_bytes());
        }
        assert_eq!(output.to_vec(), hex("3ad77bb40d7a3660a89ecaf32466ef97"));
    }

    #[test]
    fn completion_interrupts_when_enabled_and_the_engine_locks_while_busy() {
        let mut aes = AesEngine::new();
        aes.write(0x004, 1 << 16, !0);
        aes.write(0x000, MODE_ECB_ENCRYPT | CNT_BUSY | CNT_IRQ, !0);
        aes.write(0x000, MODE_CTR | CNT_BUSY, !0);
        assert_eq!(aes.read(0x000) >> 27 & 7, 7, "locked");
        for _ in 0..4 {
            aes.write(0x008, 0, !0);
        }
        assert!(aes.take_irq());
        assert!(!aes.take_irq());
        assert_eq!(aes.read(0x000) & CNT_BUSY, 0);
    }
}
