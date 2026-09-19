//! The RSA engine at 0x1000B000: modular exponentiation with four key slots
//! (3dbrew, "RSA Registers"; the driver GodMode9 and fastboot3DS share).
//!
//! `+0x000` control, `+0x0F0` an unknown latch, `+0x100` four slots of
//! control and size, `+0x200` the exponent FIFO, `+0x400` the modulus and
//! `+0x800` the text, each 0x100 bytes. Setting the start bit replaces the
//! text with `text ^ exponent mod modulus` for the selected slot, over the
//! slot's size in words; shorter numbers sit at the end of their areas.
//! Padding is the software's business.
//!
//! The byte order bits follow the driver, which works on hardware, and not
//! 3dbrew's table, which has bit 8 the other way round: with bit 8 set the
//! bytes of an area, in address order, are the number most significant byte
//! first; clear, each word holds its four bytes reversed. With bit 9 clear
//! the words of the modulus and text are in reverse order as well.
//!
//! An even modulus gives zero, as observed on hardware. The operation
//! completes at once; how long the engine really takes is not known.

const AREA: usize = 0x100;
const SLOTS: usize = 4;

const CNT_START: u32 = 1 << 0;
const CNT_IRQ: u32 = 1 << 1;
const CNT_BIG_ENDIAN: u32 = 1 << 8;
const CNT_NORMAL_ORDER: u32 = 1 << 9;
const CNT_KEPT: u32 = 0x3F2;

const SLOT_KEY_SET: u32 = 1 << 0;
const SLOT_WRITE_PROTECT: u32 = 1 << 1;
const SLOT_READ_PROTECT: u32 = 1 << 2;
const SLOT_LOCK: u32 = 1 << 31;

#[derive(Clone)]
struct Slot {
    control: u32,
    /// The size of the key in words; 0x40 is RSA-2048.
    words: u32,
    /// The exponent as written, a word at a time.
    exponent: [u8; AREA],
    /// Words of the exponent written since the key was last unset.
    filled: usize,
    modulus: [u8; AREA],
}

pub struct RsaEngine {
    control: u32,
    unknown_f0: u32,
    slots: [Slot; SLOTS],
    text: [u8; AREA],
    irq: bool,
}

impl Default for RsaEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn word_at(area: &[u8; AREA], offset: usize) -> u32 {
    let at = offset & (AREA - 4);
    u32::from_le_bytes([area[at], area[at + 1], area[at + 2], area[at + 3]])
}

fn set_word(area: &mut [u8; AREA], offset: usize, value: u32, mask: u32) {
    let at = offset & (AREA - 4);
    let merged = word_at(area, at) & !mask | value & mask;
    area[at..at + 4].copy_from_slice(&merged.to_le_bytes());
}

impl RsaEngine {
    pub fn new() -> Self {
        let slot = Slot {
            control: 0,
            words: 0x40,
            exponent: [0; AREA],
            filled: 0,
            modulus: [0; AREA],
        };
        RsaEngine {
            control: 0,
            unknown_f0: 0,
            slots: [slot.clone(), slot.clone(), slot.clone(), slot],
            text: [0; AREA],
            irq: false,
        }
    }

    /// Bits 6 and 7 of the slot field do not select anything.
    fn selected(&self) -> usize {
        (self.control >> 4 & 3) as usize
    }

    /// Whether the engine has asked for ARM9 interrupt 22 since the last call.
    pub fn take_irq(&mut self) -> bool {
        std::mem::take(&mut self.irq)
    }

    pub fn read(&self, offset: u32) -> u32 {
        let offset = offset as usize & 0xFFC;
        let slot = &self.slots[self.selected()];
        match offset {
            0x000 => self.control,
            0x0F0 => self.unknown_f0,
            0x100..=0x13F => {
                let slot = &self.slots[(offset - 0x100) / 0x10];
                match offset & 0xC {
                    0x0 => slot.control,
                    0x4 => slot.words,
                    _ => 0,
                }
            }
            // The exponent cannot be read back.
            0x400..=0x4FF if slot.control & SLOT_READ_PROTECT == 0 => {
                word_at(&slot.modulus, offset)
            }
            0x800..=0x8FF => word_at(&self.text, offset),
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u32, value: u32, mask: u32) {
        let offset = offset as usize & 0xFFC;
        let selected = self.selected();
        match offset {
            0x000 => {
                self.control = self.control & !mask & CNT_KEPT | value & mask & CNT_KEPT;
                if value & mask & CNT_START != 0 {
                    self.run();
                }
            }
            0x0F0 => self.unknown_f0 = self.unknown_f0 & !mask | value & mask,
            0x100..=0x13F => {
                let slot = &mut self.slots[(offset - 0x100) / 0x10];
                if slot.control & SLOT_LOCK != 0 {
                    return;
                }
                match offset & 0xC {
                    0x0 => {
                        let kept =
                            SLOT_LOCK | SLOT_READ_PROTECT | SLOT_WRITE_PROTECT | SLOT_KEY_SET;
                        let new = slot.control & !mask | value & mask;
                        // Software can only unset the key; writing the whole
                        // exponent sets it.
                        let set = new & slot.control & SLOT_KEY_SET;
                        slot.control = new & kept & !SLOT_KEY_SET | set;
                        if set == 0 {
                            slot.filled = 0;
                        }
                    }
                    0x4 => slot.words = (slot.words & !mask | value & mask).clamp(1, 0x40),
                    _ => {}
                }
            }
            0x200..=0x2FF => {
                // A FIFO: the position written does not matter.
                let slot = &mut self.slots[selected];
                let words = slot.words as usize;
                if slot.control & (SLOT_WRITE_PROTECT | SLOT_KEY_SET) != 0 || slot.filled >= words {
                    return;
                }
                let at = AREA - (words - slot.filled) * 4;
                set_word(&mut slot.exponent, at, value, !0);
                slot.filled += 1;
                if slot.filled == words {
                    slot.control |= SLOT_KEY_SET;
                }
            }
            0x400..=0x4FF => {
                let slot = &mut self.slots[selected];
                if slot.control & SLOT_WRITE_PROTECT == 0 {
                    set_word(&mut slot.modulus, offset, value, mask);
                }
            }
            0x800..=0x8FF => set_word(&mut self.text, offset, value, mask),
            _ => {}
        }
    }

    /// The last `words` words of an area as limbs, least significant first.
    fn number(&self, area: &[u8; AREA], words: usize, ordered: bool) -> Vec<u32> {
        let start = AREA - words * 4;
        let big = self.control & CNT_BIG_ENDIAN != 0;
        let normal = !ordered || self.control & CNT_NORMAL_ORDER != 0;
        let mut limbs: Vec<u32> = (0..words)
            .map(|i| {
                let at = start + i * 4;
                let bytes = [area[at], area[at + 1], area[at + 2], area[at + 3]];
                if big {
                    u32::from_be_bytes(bytes)
                } else {
                    u32::from_le_bytes(bytes)
                }
            })
            .collect();
        // In normal order the first word is the most significant.
        if normal {
            limbs.reverse();
        }
        limbs
    }

    fn run(&mut self) {
        let slot = &self.slots[self.selected()];
        let words = slot.words as usize;
        let exponent = self.number(&slot.exponent, words, false);
        let modulus = self.number(&slot.modulus, words, true);
        let text = self.number(&self.text, words, true);
        let mut result = modexp(&text, &exponent, &modulus);

        if self.control & CNT_NORMAL_ORDER != 0 {
            result.reverse();
        }
        let big = self.control & CNT_BIG_ENDIAN != 0;
        let start = AREA - words * 4;
        for (i, limb) in result.into_iter().enumerate() {
            let bytes = if big {
                limb.to_be_bytes()
            } else {
                limb.to_le_bytes()
            };
            self.text[start + i * 4..start + i * 4 + 4].copy_from_slice(&bytes);
        }
        // The start bit never reads as set: the work is done within the write.
        if self.control & CNT_IRQ != 0 {
            self.irq = true;
        }
    }
}

/// `a >= b` for equally long numbers, least significant limb first.
fn not_less(a: &[u32], b: &[u32]) -> bool {
    a.iter().rev().cmp(b.iter().rev()) != std::cmp::Ordering::Less
}

/// `a -= b`, returning the borrow.
fn subtract(a: &mut [u32], b: &[u32]) -> bool {
    let mut borrow = false;
    for (x, &y) in a.iter_mut().zip(b) {
        let (d, b1) = x.overflowing_sub(y);
        let (d, b2) = d.overflowing_sub(borrow as u32);
        *x = d;
        borrow = b1 || b2;
    }
    borrow
}

/// `a = (2a + bit) mod n` for `a < n`: below `2n`, so one subtraction is
/// enough.
fn shift_in(a: &mut [u32], bit: u32, n: &[u32]) {
    let mut carry = bit;
    for x in a.iter_mut() {
        let next = *x >> 31;
        *x = *x << 1 | carry;
        carry = next;
    }
    if carry != 0 || not_less(a, n) {
        subtract(a, n);
    }
}

/// `a = 2a mod n` for `a < n`.
fn double_mod(a: &mut [u32], n: &[u32]) {
    shift_in(a, 0, n);
}

/// `value mod n` by shifting in one bit at a time.
fn reduce(value: &[u32], n: &[u32]) -> Vec<u32> {
    let mut r = vec![0u32; n.len()];
    for limb in value.iter().rev() {
        for bit in (0..32).rev() {
            shift_in(&mut r, limb >> bit & 1, n);
        }
    }
    r
}

/// Montgomery multiplication: `a * b / 2^(32 * len) mod n` for odd `n`, with
/// `n0 = -1 / n mod 2^32`.
fn montgomery(a: &[u32], b: &[u32], n: &[u32], n0: u32) -> Vec<u32> {
    let len = n.len();
    let mut t = vec![0u32; len + 2];
    for &ai in a {
        let mut carry = 0u64;
        for j in 0..len {
            let sum = t[j] as u64 + ai as u64 * b[j] as u64 + carry;
            t[j] = sum as u32;
            carry = sum >> 32;
        }
        let sum = t[len] as u64 + carry;
        t[len] = sum as u32;
        t[len + 1] = (sum >> 32) as u32;

        let m = t[0].wrapping_mul(n0);
        let mut carry = (t[0] as u64 + m as u64 * n[0] as u64) >> 32;
        for j in 1..len {
            let sum = t[j] as u64 + m as u64 * n[j] as u64 + carry;
            t[j - 1] = sum as u32;
            carry = sum >> 32;
        }
        let sum = t[len] as u64 + carry;
        t[len - 1] = sum as u32;
        t[len] = t[len + 1] + (sum >> 32) as u32;
        t[len + 1] = 0;
    }
    let overflow = t[len] != 0;
    t.truncate(len);
    if overflow || not_less(&t, n) {
        subtract(&mut t, n);
    }
    t
}

/// `base ^ exponent mod modulus`, all least significant limb first and of
/// the modulus's length. Zero for an even (or zero) modulus, like the engine.
pub fn modexp(base: &[u32], exponent: &[u32], modulus: &[u32]) -> Vec<u32> {
    let len = modulus.len();
    if len == 0 || modulus[0] & 1 == 0 {
        return vec![0; len];
    }
    // -1/n mod 2^32 by Newton's iteration, which doubles the correct bits.
    let mut inverse = modulus[0];
    for _ in 0..5 {
        inverse = inverse.wrapping_mul(2u32.wrapping_sub(modulus[0].wrapping_mul(inverse)));
    }
    let n0 = inverse.wrapping_neg();

    // R mod n and R^2 mod n, with R = 2^(32 * len), by doubling from one.
    let mut one = vec![0u32; len];
    one[0] = 1;
    let mut r = reduce(&one, modulus);
    for _ in 0..32 * len {
        double_mod(&mut r, modulus);
    }
    let mut r2 = r.clone();
    for _ in 0..32 * len {
        double_mod(&mut r2, modulus);
    }

    let base = montgomery(&reduce(base, modulus), &r2, modulus, n0);
    let mut result = r;
    for limb in exponent.iter().rev() {
        for bit in (0..32).rev() {
            result = montgomery(&result, &result, modulus, n0);
            if limb >> bit & 1 != 0 {
                result = montgomery(&result, &base, modulus, n0);
            }
        }
    }
    montgomery(&result, &one, modulus, n0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slow_modexp(base: u128, exponent: u64, modulus: u128) -> u128 {
        let mut result = 1 % modulus;
        let mut base = base % modulus;
        for bit in 0..64 {
            if exponent >> bit & 1 != 0 {
                result = result * base % modulus;
            }
            base = base * base % modulus;
        }
        result
    }

    #[test]
    fn modexp_matches_plain_arithmetic_on_two_limbs() {
        let mut seed = 0x1234_5678_9ABC_DEF1u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..500 {
            // Keep the modulus below 2^63 so the reference cannot overflow.
            let (base, exponent, modulus) = (next(), next(), next() >> 1 | 1);
            let limbs = |v: u64| vec![v as u32, (v >> 32) as u32];
            let got = modexp(&limbs(base), &limbs(exponent), &limbs(modulus));
            let want = slow_modexp(base as u128, exponent, modulus as u128) as u64;
            assert_eq!(
                got,
                limbs(want),
                "{base:#x} ^ {exponent:#x} mod {modulus:#x}"
            );
        }
    }

    #[test]
    fn an_even_modulus_gives_zero() {
        assert_eq!(modexp(&[5, 0], &[3, 0], &[10, 0]), [0, 0]);
    }

    /// A 2048-bit case that ties the engine to a second implementation:
    /// `pow(m, 65537, n)` from Python, with `n` and `m` made by formula.
    fn vector() -> ([u8; AREA], [u8; AREA]) {
        let mut modulus = [0u8; AREA];
        let mut message = [0u8; AREA];
        for i in 0..AREA {
            modulus[i] = (i * 7 + 3) as u8;
            message[i] = (i * 13 + 5) as u8;
        }
        modulus[0] |= 0x80;
        modulus[AREA - 1] |= 1;
        message[0] = 0;
        (modulus, message)
    }

    fn load(engine: &mut RsaEngine, base: u32, bytes: &[u8; AREA]) {
        for (i, word) in bytes.as_chunks::<4>().0.iter().enumerate() {
            let value = u32::from_le_bytes(*word);
            engine.write(base + i as u32 * 4, value, !0);
        }
    }

    #[test]
    fn the_engine_runs_a_2048_bit_operation_as_the_driver_sets_it_up() {
        let (modulus, message) = vector();
        let mut engine = RsaEngine::new();
        engine.write(0x000, CNT_NORMAL_ORDER | CNT_BIG_ENDIAN | 1 << 4, !0);
        assert_eq!(engine.read(0x114), 0x40);
        for i in 0..0x3F {
            engine.write(0x200 + i * 4, 0, !0);
        }
        assert_eq!(engine.read(0x110) & SLOT_KEY_SET, 0);
        // 65537 as the last word, big-endian in memory.
        engine.write(0x2FC, 0x0100_0100, !0);
        assert_ne!(engine.read(0x110) & SLOT_KEY_SET, 0);
        load(&mut engine, 0x400, &modulus);
        load(&mut engine, 0x800, &message);
        engine.write(0x000, engine.read(0x000) | CNT_START | CNT_IRQ, !0);
        assert_eq!(engine.read(0x000) & CNT_START, 0);
        assert!(engine.take_irq());

        let mut result = [0u8; AREA];
        for (i, word) in result.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            *word = engine.read(0x800 + i as u32 * 4).to_le_bytes();
        }
        assert_eq!(result[..8], EXPECTED_HEAD);
        assert_eq!(result[AREA - 8..], EXPECTED_TAIL);
        let fnv = result.iter().fold(0x811C_9DC5u32, |h, &b| {
            (h ^ b as u32).wrapping_mul(0x0100_0193)
        });
        assert_eq!(fnv, EXPECTED_FNV1A);
    }

    #[test]
    fn little_endian_reversed_input_is_the_same_number() {
        let (modulus, message) = vector();
        let mut big = RsaEngine::new();
        let mut little = RsaEngine::new();
        big.write(0x000, CNT_NORMAL_ORDER | CNT_BIG_ENDIAN, !0);
        little.write(0x000, 0, !0);
        let mut reversed_modulus = modulus;
        let mut reversed_message = message;
        reversed_modulus.reverse();
        reversed_message.reverse();
        for i in 0..0x3F {
            big.write(0x200 + i * 4, 0, !0);
            little.write(0x200 + i * 4, 0, !0);
        }
        big.write(0x2FC, 0x0300_0000, !0);
        little.write(0x2FC, 3, !0);
        load(&mut big, 0x400, &modulus);
        load(&mut big, 0x800, &message);
        load(&mut little, 0x400, &reversed_modulus);
        load(&mut little, 0x800, &reversed_message);
        big.write(0x000, big.read(0) | CNT_START, !0);
        little.write(0x000, little.read(0) | CNT_START, !0);
        for i in 0..0x40u32 {
            let from_big = big.read(0x800 + i * 4).swap_bytes();
            assert_eq!(from_big, little.read(0x800 + (0x3F - i) * 4), "word {i}");
        }
    }

    #[test]
    fn protection_bits_guard_the_key_and_the_lock_freezes_them() {
        let mut engine = RsaEngine::new();
        engine.write(0x400, 0xAABB_CCDD, !0);
        engine.write(0x100, SLOT_READ_PROTECT, !0);
        assert_eq!(engine.read(0x400), 0);
        engine.write(0x100, SLOT_WRITE_PROTECT, !0);
        engine.write(0x400, 0x1122_3344, !0);
        assert_eq!(engine.read(0x400), 0xAABB_CCDD);
        engine.write(0x100, SLOT_LOCK | SLOT_WRITE_PROTECT, !0);
        engine.write(0x100, 0, !0);
        assert_eq!(engine.read(0x100), SLOT_LOCK | SLOT_WRITE_PROTECT);
        // Software cannot set the key-set bit by hand.
        engine.write(0x110, SLOT_KEY_SET, !0);
        assert_eq!(engine.read(0x110), 0);
    }

    /// Of the 256 result bytes: the first and last eight, and FNV-1a over all.
    const EXPECTED_HEAD: [u8; 8] = [0x2F, 0x99, 0x5A, 0x72, 0xEB, 0x87, 0x0A, 0x4D];
    const EXPECTED_TAIL: [u8; 8] = [0x84, 0x21, 0x35, 0x20, 0xCF, 0x2E, 0x1C, 0xF6];
    const EXPECTED_FNV1A: u32 = 0x5063_15ED;
}
