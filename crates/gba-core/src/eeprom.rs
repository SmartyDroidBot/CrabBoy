//! GBA serial EEPROM save memory (512 byte / 8 KB parts).
//!
//! The EEPROM sits on the cartridge bus at `0x0D000000..=0x0DFFFFFF` and is
//! accessed one bit at a time, normally through DMA3 with 16-bit units where
//! only bit 0 of each halfword matters (GBATEK, "GBA Cart Backup EEPROM").
//!
//! Requests start with two command bits: `11` = read, `10` = write. A read
//! request carries the block address (6 bits for the 512 B part, 14 bits for
//! the 8 KB part, of which only the low 10 select one of 1024 blocks) and a
//! stop bit; the reply is 4 dummy bits followed by the 64 data bits of the
//! block, MSB first. A write request carries the address, 64 data bits and a
//! stop bit. The bus width is not encoded anywhere in the ROM, so it is
//! inferred from the length of the first DMA transfer: 9/73 halfwords for the
//! 512 B part, 17/81 for the 8 KB part.

/// Capacity of the largest supported part.
pub const EEPROM_SIZE: usize = 8192;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Idle,
    Command,
    ReadAddress,
    WriteAddress,
    WriteData,
    WriteFinalize,
    ReadDummy,
    ReadData,
}

impl State {
    fn to_u8(self) -> u8 {
        match self {
            State::Idle => 0,
            State::Command => 1,
            State::ReadAddress => 2,
            State::WriteAddress => 3,
            State::WriteData => 4,
            State::WriteFinalize => 5,
            State::ReadDummy => 6,
            State::ReadData => 7,
        }
    }
    fn from_u8(v: u8) -> State {
        match v {
            1 => State::Command,
            2 => State::ReadAddress,
            3 => State::WriteAddress,
            4 => State::WriteData,
            5 => State::WriteFinalize,
            6 => State::ReadDummy,
            7 => State::ReadData,
            _ => State::Idle,
        }
    }
}

pub struct Eeprom {
    data: Box<[u8; EEPROM_SIZE]>,
    /// True for the 8 KB part (14-bit addresses); false for 512 B (6-bit).
    size_8k: bool,
    /// Whether the part size has been established (from a save file, a save
    /// state or the first DMA transfer).
    size_known: bool,
    state: State,
    cmd: u8,
    cmd_bits: u8,
    address: usize,
    addr_bits: u8,
    data_buffer: u64,
    data_bits: u8,
    read_bits: u8,
    /// Set when a block was programmed since the last save flush.
    pub dirty: bool,
}

impl Default for Eeprom {
    fn default() -> Self {
        Eeprom {
            data: Box::new([0xFF; EEPROM_SIZE]),
            size_8k: true,
            size_known: false,
            state: State::Idle,
            cmd: 0,
            cmd_bits: 0,
            address: 0,
            addr_bits: 0,
            data_buffer: 0,
            data_bits: 0,
            read_bits: 0,
            dirty: false,
        }
    }
}

/// Length of the serialised transient state (see `save_state_sm`).
pub const STATE_LEN: usize = 20;

impl Eeprom {
    pub fn new() -> Self {
        Eeprom::default()
    }

    /// Whether the 8 KB part is selected.
    pub fn is_8k(&self) -> bool {
        self.size_8k
    }

    /// Capacity of the selected part in bytes.
    pub fn size(&self) -> usize {
        if self.size_8k {
            EEPROM_SIZE
        } else {
            512
        }
    }

    /// The backing bytes of the selected part.
    pub fn raw(&self) -> &[u8] {
        &self.data[..self.size()]
    }

    /// Load a save file; a 512-byte file selects the small part.
    pub fn load(&mut self, data: &[u8]) {
        let n = data.len().min(EEPROM_SIZE);
        self.data[..n].copy_from_slice(&data[..n]);
        if !data.is_empty() {
            self.size_8k = data.len() > 512;
            self.size_known = true;
        }
    }

    /// Infer the part size from the halfword count of a DMA transfer to the
    /// EEPROM region: 9 (read request) or 73 (write) for the 512 B part, 17
    /// or 81 for the 8 KB part. Only the first transfer decides.
    pub fn set_size_from_dma_count(&mut self, count: u32) {
        if self.size_known {
            return;
        }
        match count {
            9 | 73 => {
                self.size_8k = false;
                self.size_known = true;
            }
            17 | 81 => {
                self.size_8k = true;
                self.size_known = true;
            }
            _ => {}
        }
    }

    fn addr_bits_needed(&self) -> u8 {
        if self.size_8k {
            14
        } else {
            6
        }
    }

    /// The selected block's byte offset; the 8 KB part ignores the top four
    /// address bits.
    fn block_offset(&self) -> usize {
        let mask = if self.size_8k { 0x3FF } else { 0x3F };
        (self.address & mask) * 8
    }

    /// Clock one bit in from the cartridge bus (bit 0 of a halfword write).
    pub fn write_bit(&mut self, bit: u8) {
        let bit = bit & 1;
        match self.state {
            State::Idle => {
                self.cmd = bit;
                self.cmd_bits = 1;
                self.state = State::Command;
            }
            State::Command => {
                self.cmd = (self.cmd << 1) | bit;
                self.cmd_bits += 1;
                if self.cmd_bits == 2 {
                    self.address = 0;
                    self.addr_bits = 0;
                    self.state = match self.cmd {
                        0b11 => State::ReadAddress,
                        0b10 => State::WriteAddress,
                        _ => State::Idle,
                    };
                }
            }
            State::ReadAddress => {
                self.address = (self.address << 1) | bit as usize;
                self.addr_bits += 1;
                if self.addr_bits == self.addr_bits_needed() {
                    self.state = State::ReadDummy;
                    self.read_bits = 0;
                }
            }
            State::WriteAddress => {
                self.address = (self.address << 1) | bit as usize;
                self.addr_bits += 1;
                if self.addr_bits == self.addr_bits_needed() {
                    self.state = State::WriteData;
                    self.data_buffer = 0;
                    self.data_bits = 0;
                }
            }
            State::WriteData => {
                self.data_buffer = (self.data_buffer << 1) | bit as u64;
                self.data_bits += 1;
                if self.data_bits == 64 {
                    self.state = State::WriteFinalize;
                }
            }
            State::WriteFinalize => {
                // The stop bit commits the block.
                let off = self.block_offset();
                self.data[off..off + 8].copy_from_slice(&self.data_buffer.to_be_bytes());
                self.dirty = true;
                self.state = State::Idle;
            }
            // The stop bit of a read request and any stray writes during the
            // reply are ignored.
            State::ReadDummy | State::ReadData => {}
        }
    }

    /// Clock one bit out to the cartridge bus (bit 0 of a halfword read).
    /// Outside a read reply the line idles high, which games poll to detect
    /// the end of a write.
    pub fn read_bit(&mut self) -> u8 {
        match self.state {
            State::ReadDummy => {
                self.read_bits += 1;
                if self.read_bits >= 4 {
                    self.state = State::ReadData;
                    self.read_bits = 0;
                    let off = self.block_offset();
                    let mut b = [0u8; 8];
                    b.copy_from_slice(&self.data[off..off + 8]);
                    self.data_buffer = u64::from_be_bytes(b);
                }
                0
            }
            State::ReadData => {
                let bit = ((self.data_buffer >> (63 - self.read_bits)) & 1) as u8;
                self.read_bits += 1;
                if self.read_bits == 64 {
                    self.state = State::Idle;
                }
                bit
            }
            _ => 1,
        }
    }

    /// Serialise the transient protocol state for save states.
    pub fn save_state_sm(&self) -> [u8; STATE_LEN] {
        let mut out = [0u8; STATE_LEN];
        out[0] = self.state.to_u8();
        out[1] = self.cmd;
        out[2] = self.cmd_bits;
        out[3] = self.addr_bits;
        out[4..8].copy_from_slice(&(self.address as u32).to_le_bytes());
        out[8] = self.data_bits;
        out[9] = self.read_bits;
        out[10] = self.size_8k as u8;
        out[11] = self.size_known as u8;
        out[12..20].copy_from_slice(&self.data_buffer.to_le_bytes());
        out
    }

    /// Restore the transient protocol state from a save state.
    pub fn load_state_sm(&mut self, b: &[u8; STATE_LEN]) {
        self.state = State::from_u8(b[0]);
        self.cmd = b[1];
        self.cmd_bits = b[2];
        self.addr_bits = b[3];
        self.address = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
        self.data_bits = b[8];
        self.read_bits = b[9];
        self.size_8k = b[10] != 0;
        self.size_known = b[11] != 0;
        let mut db = [0u8; 8];
        db.copy_from_slice(&b[12..20]);
        self.data_buffer = u64::from_le_bytes(db);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send_bits(e: &mut Eeprom, value: u64, count: u8) {
        for i in (0..count).rev() {
            e.write_bit(((value >> i) & 1) as u8);
        }
    }

    fn read_block(e: &mut Eeprom, block: u64, addr_bits: u8) -> u64 {
        send_bits(e, 0b11, 2);
        send_bits(e, block, addr_bits);
        e.write_bit(0); // stop bit
        for _ in 0..4 {
            assert_eq!(e.read_bit(), 0, "dummy bits read as zero");
        }
        let mut v = 0u64;
        for _ in 0..64 {
            v = (v << 1) | e.read_bit() as u64;
        }
        v
    }

    fn write_block(e: &mut Eeprom, block: u64, addr_bits: u8, value: u64) {
        send_bits(e, 0b10, 2);
        send_bits(e, block, addr_bits);
        send_bits(e, value, 64);
        e.write_bit(0); // stop bit
    }

    #[test]
    fn eeprom_write_then_read_8k_roundtrip() {
        let mut e = Eeprom::new();
        e.set_size_from_dma_count(81);
        assert!(e.is_8k());
        write_block(&mut e, 0x3FF, 14, 0x0123_4567_89AB_CDEF);
        assert!(e.dirty);
        assert_eq!(read_block(&mut e, 0x3FF, 14), 0x0123_4567_89AB_CDEF);
        assert_eq!(e.read_bit(), 1, "idle line reads high");
        // The top four address bits are ignored on the 8 KB part.
        assert_eq!(read_block(&mut e, 0x3FFF, 14), 0x0123_4567_89AB_CDEF);
    }

    #[test]
    fn eeprom_512b_uses_6_address_bits() {
        let mut e = Eeprom::new();
        e.set_size_from_dma_count(9);
        assert!(!e.is_8k());
        assert_eq!(e.raw().len(), 512);
        write_block(&mut e, 63, 6, 0xFEED_FACE_CAFE_BEEF);
        assert_eq!(read_block(&mut e, 63, 6), 0xFEED_FACE_CAFE_BEEF);
        assert_eq!(
            &e.raw()[63 * 8..64 * 8],
            &0xFEED_FACE_CAFE_BEEFu64.to_be_bytes()
        );
    }

    #[test]
    fn eeprom_size_autodetect_from_dma_count() {
        let mut e = Eeprom::new();
        e.set_size_from_dma_count(100); // not a request length: undecided
        e.set_size_from_dma_count(17);
        assert!(e.is_8k());
        e.set_size_from_dma_count(9); // later transfers do not flip it
        assert!(e.is_8k());
        let mut small = Eeprom::new();
        small.load(&[0u8; 512]);
        assert!(!small.is_8k());
    }

    #[test]
    fn state_round_trip_preserves_a_pending_read() {
        let mut e = Eeprom::new();
        e.set_size_from_dma_count(9);
        write_block(&mut e, 5, 6, 0xAA55_AA55_AA55_AA55);
        send_bits(&mut e, 0b11, 2);
        send_bits(&mut e, 5, 6);
        e.write_bit(0);
        let sm = e.save_state_sm();
        let mut f = Eeprom::new();
        f.load(e.raw());
        f.load_state_sm(&sm);
        for _ in 0..4 {
            f.read_bit();
        }
        let mut v = 0u64;
        for _ in 0..64 {
            v = (v << 1) | f.read_bit() as u64;
        }
        assert_eq!(v, 0xAA55_AA55_AA55_AA55);
    }
}
