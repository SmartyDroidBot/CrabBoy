//! GBA battery-backed save memory: SRAM, FLASH (64K/128K), and EEPROM.
//!
//! The save cartridge lives at `0x0E000000` and is auto-detected from the first
//! access: a write to the FLASH command addresses (`0x5555`/`0x2AAA`) selects
//! FLASH, otherwise the region behaves as plain SRAM. FLASH implements the
//! standard command interface (program, sector erase, ID and bank select for
//! 128K parts). EEPROM uses a separate serial protocol and is not implemented
//! here (it reads as 0xFF).

/// Maximum FLASH capacity (128 KB).
pub const FLASH_SIZE: usize = 0x20000;
/// SRAM capacity (64 KB).
pub const SRAM_SIZE: usize = 0x10000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SaveType {
    None,
    Sram,
    Flash,
    Eeprom,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cmd {
    Idle,
    Aa,
    A55,
    Program,
    Erase1,
    Erase2,
    Erase3,
    Bank,
}

use crate::bus::Mem;

/// The battery-backed save cartridge.
pub struct SaveCartridge {
    pub kind: SaveType,
    /// Detected automatically on first access.
    detected: bool,
    flash: Mem<u8, FLASH_SIZE>,
    sram: Mem<u8, SRAM_SIZE>,
    bank: usize,
    cmd: Cmd,
    id_mode: bool,
    /// Dirty flag, cleared by the system after a save flush.
    dirty: bool,
}

impl Default for SaveCartridge {
    fn default() -> Self {
        SaveCartridge {
            kind: SaveType::None,
            detected: false,
            flash: Mem::filled(0xFF),
            sram: Mem::zeroed(),
            bank: 0,
            cmd: Cmd::Idle,
            id_mode: false,
            dirty: false,
        }
    }
}

impl SaveCartridge {
    pub fn new() -> Self {
        SaveCartridge::default()
    }

    /// Whether a battery-backed cartridge is present.
    pub fn battery_backed(&self) -> bool {
        self.kind != SaveType::None
    }

    /// The save region as raw bytes (FLASH or SRAM).
    pub fn raw(&self) -> &[u8] {
        match self.kind {
            SaveType::Flash => self.flash.as_ref(),
            SaveType::Sram => self.sram.as_ref(),
            _ => &[],
        }
    }

    /// Load raw save bytes into the active region, detecting the type from the
/// payload length if it has not been set yet.
    pub fn load(&mut self, data: &[u8]) {
        if self.kind == SaveType::None {
            self.detected = true;
            self.kind = if data.len() > SRAM_SIZE {
                SaveType::Flash
            } else if !data.is_empty() {
                SaveType::Sram
            } else {
                SaveType::None
            };
        }
        match self.kind {
            SaveType::Flash => {
                let n = data.len().min(FLASH_SIZE);
                self.flash[..n].copy_from_slice(&data[..n]);
            }
            SaveType::Sram => {
                let n = data.len().min(SRAM_SIZE);
                self.sram[..n].copy_from_slice(&data[..n]);
            }
            _ => {}
        }
        self.dirty = false;
    }

    /// Read the dirty flag and clear it.
    pub fn take_dirty(&mut self) -> bool {
        let d = self.dirty;
        self.dirty = false;
        d
    }

    /// The detected save type.
    pub fn kind(&self) -> SaveType {
        self.kind
    }

    /// Set the save type and mark it detected (used by save-state restore).
    pub fn set_kind(&mut self, kind: SaveType) {
        self.kind = kind;
        self.detected = true;
    }

    /// The active FLASH bank index.
    pub fn flash_bank(&self) -> usize {
        self.bank
    }

    pub fn set_flash_bank(&mut self, bank: usize) {
        self.bank = bank & 1;
    }

    fn detect_write(&mut self, addr: usize) {
        if self.detected {
            return;
        }
        self.detected = true;
        self.kind = if (addr & 0xFFFF) == 0x5555 || (addr & 0xFFFF) == 0x2AAA {
            SaveType::Flash
        } else {
            SaveType::Sram
        };
    }

    /// Read a byte from the save region.
    pub fn read8(&self, addr: usize) -> u8 {
        let a = addr & 0xFFFF;
        match self.kind {
            SaveType::Flash => {
                if self.id_mode {
                    match a {
                        0x0000 => 0xC2, // manufacturer
                        0x0001 => 0x09, // device: 128K (also used for 64K detection)
                        _ => 0xFF,
                    }
                } else {
                    self.flash[self.bank * 0x10000 + a]
                }
            }
            SaveType::Sram => self.sram[a & (SRAM_SIZE - 1)],
            _ => 0xFF,
        }
    }

    /// Read a 16-bit value from the save region.
    pub fn read16(&self, addr: usize) -> u16 {
        self.read8(addr) as u16 | (self.read8(addr + 1) as u16) << 8
    }

    /// Write a byte to the save region.
    pub fn write8(&mut self, addr: usize, value: u8) {
        self.detect_write(addr);
        let a = addr & 0xFFFF;
        match self.kind {
            SaveType::Flash => self.flash_write(a, value),
            SaveType::Sram => {
                self.sram[a & (SRAM_SIZE - 1)] = value;
                self.dirty = true;
            }
            _ => {}
        }
    }

    /// Write a 16-bit value to the save region.
    pub fn write16(&mut self, addr: usize, value: u16) {
        self.write8(addr, value as u8);
        self.write8(addr + 1, (value >> 8) as u8);
    }

    fn flash_write(&mut self, a: usize, value: u8) {
        match self.cmd {
            Cmd::Idle => {
                if a == 0x5555 && value == 0xAA {
                    self.cmd = Cmd::Aa;
                }
            }
            Cmd::Aa => {
                if a == 0x2AAA && value == 0x55 {
                    self.cmd = Cmd::A55;
                } else {
                    self.cmd = Cmd::Idle;
                }
            }
            Cmd::A55 => {
                if a == 0x5555 {
                    match value {
                        0x90 => {
                            self.id_mode = true;
                            self.cmd = Cmd::Idle;
                        }
                        0xA0 => self.cmd = Cmd::Program,
                        0x80 => self.cmd = Cmd::Erase1,
                        0xF0 => {
                            self.id_mode = false;
                            self.cmd = Cmd::Idle;
                        }
                        0xB0 => self.cmd = Cmd::Bank,
                        _ => self.cmd = Cmd::Idle,
                    }
                } else {
                    self.cmd = Cmd::Idle;
                }
            }
            Cmd::Program => {
                let idx = self.bank * 0x10000 + a;
                self.flash[idx] = value;
                self.dirty = true;
                self.cmd = Cmd::Idle;
            }
            Cmd::Erase1 => {
                if a == 0x5555 && value == 0xAA {
                    self.cmd = Cmd::Erase2;
                } else {
                    self.cmd = Cmd::Idle;
                }
            }
            Cmd::Erase2 => {
                if a == 0x2AAA && value == 0x55 {
                    self.cmd = Cmd::Erase3;
                } else {
                    self.cmd = Cmd::Idle;
                }
            }
            Cmd::Erase3 => {
                if a == 0x5555 && (value == 0x10 || value == 0x30) {
                    // Sector erase; for simplicity erase the whole selected bank.
                    let base = self.bank * 0x10000;
                    self.flash[base..base + 0x10000].fill(0xFF);
                    self.dirty = true;
                }
                self.cmd = Cmd::Idle;
            }
            Cmd::Bank => {
                self.bank = (value as usize) & 1;
                self.cmd = Cmd::Idle;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sram_detection_and_write() {
        let mut s = SaveCartridge::new();
        s.write8(0x0E000000, 0x5A);
        assert_eq!(s.kind, SaveType::Sram);
        assert_eq!(s.read8(0x0E000000), 0x5A);
        assert!(s.take_dirty());
        assert!(!s.take_dirty());
    }

    #[test]
    fn flash_program_and_read() {
        let mut s = SaveCartridge::new();
        // Command sequence to program 0x42 at offset 0x0004.
        s.write8(0x0E005555, 0xAA);
        s.write8(0x0E002AAA, 0x55);
        s.write8(0x0E005555, 0xA0);
        s.write8(0x0E000004, 0x42);
        assert_eq!(s.kind, SaveType::Flash);
        assert_eq!(s.read8(0x0E000004), 0x42);
        assert!(s.take_dirty());
    }

    #[test]
    fn flash_bank_select_128k() {
        let mut s = SaveCartridge::new();
        s.write8(0x0E005555, 0xAA);
        s.write8(0x0E002AAA, 0x55);
        s.write8(0x0E005555, 0xB0);
        s.write8(0x0E000000, 0x01); // select bank 1
        s.write8(0x0E005555, 0xAA);
        s.write8(0x0E002AAA, 0x55);
        s.write8(0x0E005555, 0xA0);
        s.write8(0x0E000004, 0x11); // program bank 1
        assert_eq!(s.read8(0x0E000004), 0x11);
        // Bank 0 still 0xFF.
        s.write8(0x0E005555, 0xAA);
        s.write8(0x0E002AAA, 0x55);
        s.write8(0x0E005555, 0xB0);
        s.write8(0x0E000000, 0x00);
        assert_eq!(s.read8(0x0E000004), 0xFF);
    }
}