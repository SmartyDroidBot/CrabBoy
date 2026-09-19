//! AES-128 (FIPS 197), one block at a time.
//!
//! A plain byte-oriented implementation: the engines of the 3DS move a few
//! megabytes at most, and nothing here handles secrets that timing could
//! leak to anyone but the user's own software.

const fn xtime(x: u8) -> u8 {
    (x << 1) ^ if x & 0x80 != 0 { 0x1B } else { 0 }
}

/// The S-box, generated from the field inverse and the affine map.
const fn build_sbox() -> [u8; 256] {
    let mut sbox = [0u8; 256];
    let (mut p, mut q) = (1u8, 1u8);
    loop {
        // p runs through the field by multiplying by 3; q by dividing by 3,
        // so q is always the inverse of p.
        p = p ^ xtime(p);
        q ^= q << 1;
        q ^= q << 2;
        q ^= q << 4;
        if q & 0x80 != 0 {
            q ^= 0x09;
        }
        let x = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
        sbox[p as usize] = x ^ 0x63;
        if p == 1 {
            break;
        }
    }
    sbox[0] = 0x63;
    sbox
}

const fn invert(sbox: &[u8; 256]) -> [u8; 256] {
    let mut inverse = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        inverse[sbox[i] as usize] = i as u8;
        i += 1;
    }
    inverse
}

static SBOX: [u8; 256] = build_sbox();
static INV_SBOX: [u8; 256] = invert(&SBOX);

fn multiply(mut a: u8, mut b: u8) -> u8 {
    let mut product = 0;
    while b != 0 {
        if b & 1 != 0 {
            product ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    product
}

/// An expanded AES-128 key.
#[derive(Clone)]
pub struct Aes128 {
    round_keys: [[u8; 16]; 11],
}

impl Aes128 {
    pub fn new(key: &[u8; 16]) -> Self {
        let mut words = [[0u8; 4]; 44];
        for (i, word) in words.iter_mut().take(4).enumerate() {
            word.copy_from_slice(&key[i * 4..i * 4 + 4]);
        }
        let mut rcon = 1u8;
        for i in 4..44 {
            let mut temp = words[i - 1];
            if i % 4 == 0 {
                temp.rotate_left(1);
                for byte in temp.iter_mut() {
                    *byte = SBOX[*byte as usize];
                }
                temp[0] ^= rcon;
                rcon = xtime(rcon);
            }
            for (j, byte) in temp.iter().enumerate() {
                words[i][j] = words[i - 4][j] ^ byte;
            }
        }
        let mut round_keys = [[0u8; 16]; 11];
        for (round, key) in round_keys.iter_mut().enumerate() {
            for column in 0..4 {
                key[column * 4..column * 4 + 4].copy_from_slice(&words[round * 4 + column]);
            }
        }
        Aes128 { round_keys }
    }

    fn add_round_key(&self, state: &mut [u8; 16], round: usize) {
        for (byte, key) in state.iter_mut().zip(self.round_keys[round]) {
            *byte ^= key;
        }
    }

    pub fn encrypt_block(&self, block: &[u8; 16]) -> [u8; 16] {
        let mut state = *block;
        self.add_round_key(&mut state, 0);
        for round in 1..=10 {
            for byte in state.iter_mut() {
                *byte = SBOX[*byte as usize];
            }
            // Row r moves left by r; the state is column-major.
            let old = state;
            for column in 0..4 {
                for row in 0..4 {
                    state[column * 4 + row] = old[(column + row) % 4 * 4 + row];
                }
            }
            if round != 10 {
                for column in state.as_chunks_mut::<4>().0 {
                    let [a, b, c, d] = *column;
                    column[0] = xtime(a) ^ xtime(b) ^ b ^ c ^ d;
                    column[1] = a ^ xtime(b) ^ xtime(c) ^ c ^ d;
                    column[2] = a ^ b ^ xtime(c) ^ xtime(d) ^ d;
                    column[3] = xtime(a) ^ a ^ b ^ c ^ xtime(d);
                }
            }
            self.add_round_key(&mut state, round);
        }
        state
    }

    pub fn decrypt_block(&self, block: &[u8; 16]) -> [u8; 16] {
        let mut state = *block;
        self.add_round_key(&mut state, 10);
        for round in (0..10).rev() {
            let old = state;
            for column in 0..4 {
                for row in 0..4 {
                    state[(column + row) % 4 * 4 + row] = old[column * 4 + row];
                }
            }
            for byte in state.iter_mut() {
                *byte = INV_SBOX[*byte as usize];
            }
            self.add_round_key(&mut state, round);
            if round != 0 {
                for column in state.as_chunks_mut::<4>().0 {
                    let [a, b, c, d] = *column;
                    column[0] =
                        multiply(a, 14) ^ multiply(b, 11) ^ multiply(c, 13) ^ multiply(d, 9);
                    column[1] =
                        multiply(a, 9) ^ multiply(b, 14) ^ multiply(c, 11) ^ multiply(d, 13);
                    column[2] =
                        multiply(a, 13) ^ multiply(b, 9) ^ multiply(c, 14) ^ multiply(d, 11);
                    column[3] =
                        multiply(a, 11) ^ multiply(b, 13) ^ multiply(c, 9) ^ multiply(d, 14);
                }
            }
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> [u8; 16] {
        let mut out = [0u8; 16];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn the_sbox_matches_fips_197() {
        assert_eq!(SBOX[..4], [0x63, 0x7C, 0x77, 0x7B]);
        assert_eq!(SBOX[0x53], 0xED);
        assert_eq!(SBOX[0xFF], 0x16);
        assert_eq!(INV_SBOX[0xED], 0x53);
    }

    /// FIPS 197, appendix C.1.
    #[test]
    fn the_example_vector_encrypts_and_decrypts() {
        let aes = Aes128::new(&hex("000102030405060708090a0b0c0d0e0f"));
        let plain = hex("00112233445566778899aabbccddeeff");
        let cipher = hex("69c4e0d86a7b0430d8cdb78070b4c55a");
        assert_eq!(aes.encrypt_block(&plain), cipher);
        assert_eq!(aes.decrypt_block(&cipher), plain);
    }

    /// FIPS 197, appendix B.
    #[test]
    fn the_cipher_example() {
        let aes = Aes128::new(&hex("2b7e151628aed2a6abf7158809cf4f3c"));
        assert_eq!(
            aes.encrypt_block(&hex("3243f6a8885a308d313198a2e0370734")),
            hex("3925841d02dc09fbdc118597196a0b32")
        );
    }

    /// NIST SP 800-38A, F.1.1, the four ECB blocks.
    #[test]
    fn sp_800_38a_ecb() {
        let aes = Aes128::new(&hex("2b7e151628aed2a6abf7158809cf4f3c"));
        let pairs = [
            (
                "6bc1bee22e409f96e93d7e117393172a",
                "3ad77bb40d7a3660a89ecaf32466ef97",
            ),
            (
                "ae2d8a571e03ac9c9eb76fac45af8e51",
                "f5d3d58503b9699de785895a96fdbaaf",
            ),
            (
                "30c81c46a35ce411e5fbc1191a0a52ef",
                "43b1cd7f598ece23881b00e3ed030688",
            ),
            (
                "f69f2445df4f9b17ad2b417be66c3710",
                "7b0c785e27e8ad3f8223207104725dd4",
            ),
        ];
        for (plain, cipher) in pairs {
            assert_eq!(aes.encrypt_block(&hex(plain)), hex(cipher));
            assert_eq!(aes.decrypt_block(&hex(cipher)), hex(plain));
        }
    }
}
