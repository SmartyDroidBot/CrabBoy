use std::fmt;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MbcType {
    RomOnly,
    Mbc1,
    Mbc3,
    Other,
}

impl fmt::Display for MbcType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MbcType::RomOnly => write!(f, "ROM only"),
            MbcType::Mbc1 => write!(f, "MBC1"),
            MbcType::Mbc3 => write!(f, "MBC3"),
            MbcType::Other => write!(f, "Unknown"),
        }
    }
}

#[derive(Clone)]
pub struct Cartridge {
    pub title: String,
    pub mbc: MbcType,
    pub rom: Vec<u8>,
    pub ram: Vec<u8>,
    num_rom_banks: usize,
    num_ram_banks: usize,
    rom_bank: usize,
    ram_bank: usize,
    bank_mode: bool,
    ram_enabled: bool,
    has_battery: bool,
    rtc_selected: bool,
    rtc_register: u8,
    rtc: [u8; 5],
    rtc_latched: [u8; 5],
    rtc_latch: u8,
    rtc_cycles: u32,
}

impl Cartridge {
    pub fn load(data: &[u8]) -> Result<Cartridge, String> {
        if data.len() < 0x150 {
            return Err("ROM too small to contain a cartridge header".to_string());
        }
        let title = data[0x134..0x143]
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as char)
            .collect();
        let mbc_code = data[0x147];
        let mbc = match mbc_code {
            0x00 => MbcType::RomOnly,
            0x01..=0x03 => MbcType::Mbc1,
            0x0F..=0x13 => MbcType::Mbc3,
            0x05..=0x06 => MbcType::Other, // MBC2
            0x19..=0x1E => MbcType::Other, // MBC5
            _ => MbcType::Other,
        };
        let rom_size_code = data[0x148];
        let num_rom_banks = match rom_size_code {
            0x00 => 2,
            0x01 => 4,
            0x02 => 8,
            0x03 => 16,
            0x04 => 32,
            0x05 => 64,
            0x06 => 128,
            0x07 => 256,
            0x52 => 72,
            0x53 => 80,
            0x54 => 96,
            _ => 2,
        };
        let ram_size_code = data[0x149];
        let num_ram_banks = match ram_size_code {
            0x00 => 0,
            0x01 => 0,
            0x02 => 1,
            0x03 => 4,
            0x04 => 16,
            0x05 => 8,
            _ => 0,
        };
        let ram_bytes = match ram_size_code {
            0x02 => 0x2000,
            0x03 => 0x8000,
            0x04 => 0x20000,
            0x05 => 0x10000,
            _ => 0,
        };
        let has_battery = matches!(
            mbc_code,
            0x03 | 0x06 | 0x09 | 0x0D | 0x0F | 0x10 | 0x13 | 0x1B | 0x1E
        );

        Ok(Cartridge {
            title,
            mbc,
            rom: data.to_vec(),
            ram: vec![0; ram_bytes],
            num_rom_banks,
            num_ram_banks,
            rom_bank: 1,
            ram_bank: 0,
            bank_mode: false,
            ram_enabled: false,
            has_battery,
            rtc_selected: false,
            rtc_register: 0,
            rtc: [0; 5],
            rtc_latched: [0; 5],
            rtc_latch: 0,
            rtc_cycles: 0,
        })
    }

    pub fn has_battery(&self) -> bool {
        self.has_battery
    }

    /// Advance the MBC3 real-time clock by `cycles` T-cycles (4.19 MHz).
    pub fn rtc_tick(&mut self, cycles: u32) {
        if self.mbc != MbcType::Mbc3 {
            return;
        }
        const CYCLES_PER_SECOND: u32 = 4_194_304;
        self.rtc_cycles += cycles;
        while self.rtc_cycles >= CYCLES_PER_SECOND {
            self.rtc_cycles -= CYCLES_PER_SECOND;
            self.rtc[0] += 1;
            if self.rtc[0] >= 60 {
                self.rtc[0] = 0;
                self.rtc[1] += 1;
                if self.rtc[1] >= 60 {
                    self.rtc[1] = 0;
                    self.rtc[2] += 1;
                    if self.rtc[2] >= 24 {
                        self.rtc[2] = 0;
                        // Day counter spans 9 bits across rtc[3] and bit 0 of rtc[4].
                        let mut day = (self.rtc[4] & 0x01) as u16 * 256 | self.rtc[3] as u16;
                        day = day.wrapping_add(1);
                        if day > 0x1FF {
                            day = 0;
                            self.rtc[4] |= 0x80; // day counter carry
                        }
                        self.rtc[3] = (day & 0xFF) as u8;
                        self.rtc[4] = (self.rtc[4] & 0xFE) | ((day >> 8) & 1) as u8;
                    }
                }
            }
        }
    }

    pub fn ram_bytes(&self) -> usize {
        self.ram.len()
    }

    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x3FFF => self.rom[addr as usize],
            0x4000..=0x7FFF => {
                let bank = self.rom_bank % self.num_rom_banks.max(1);
                let offset = bank * 0x4000 + (addr as usize - 0x4000);
                self.rom.get(offset).copied().unwrap_or(0xFF)
            }
            _ => unreachable!(),
        }
    }

    pub fn read_ram(&self, addr: u16) -> u8 {
        let _ = addr;
        if !self.ram_enabled || self.ram.is_empty() {
            return 0xFF;
        }
        if self.mbc == MbcType::Mbc3 && self.rtc_selected {
            return self.rtc_latched[self.rtc_register as usize];
        }
        let bank = match self.mbc {
            MbcType::Mbc1 => {
                if self.bank_mode {
                    self.ram_bank
                } else {
                    0
                }
            }
            MbcType::Mbc3 => self.ram_bank,
            _ => 0,
        };
        let offset = bank * 0x2000 + (addr as usize - 0xA000);
        self.ram.get(offset).copied().unwrap_or(0xFF)
    }

    pub fn write(&mut self, addr: u16, value: u8) {
        match self.mbc {
            MbcType::RomOnly => {}
            MbcType::Mbc1 => self.write_mbc1(addr, value),
            MbcType::Mbc3 => self.write_mbc3(addr, value),
            MbcType::Other => {}
        }
    }

    fn write_mbc1(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram_enabled = (value & 0x0F) == 0x0A,
            0x2000..=0x3FFF => {
                let mut bank = (value & 0x1F) as usize;
                if bank == 0 {
                    bank = 1;
                }
                if self.bank_mode {
                    self.rom_bank = ((self.rom_bank & 0x60) | (bank & 0x1F)) & 0x7F;
                } else {
                    self.rom_bank = ((self.rom_bank & 0xE0) | bank) & 0x7F;
                }
                self.rom_bank %= self.num_rom_banks.max(1);
            }
            0x4000..=0x5FFF => {
                if self.bank_mode {
                    self.ram_bank = (value & 0x03) as usize % self.num_ram_banks.max(1);
                } else {
                    self.rom_bank = ((self.rom_bank & 0x1F) | ((value & 0x03) as usize) << 5) & 0x7F;
                    self.rom_bank %= self.num_rom_banks.max(1);
                }
            }
            0x6000..=0x7FFF => {
                self.bank_mode = value & 0x01 != 0;
            }
            _ => {}
        }
    }

    fn write_mbc3(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram_enabled = (value & 0x0F) == 0x0A,
            0x2000..=0x3FFF => {
                let mut bank = value as usize;
                if bank == 0 {
                    bank = 1;
                }
                self.rom_bank = bank % self.num_rom_banks.max(1);
            }
            0x4000..=0x5FFF => {
                if value <= 0x03 {
                    self.rtc_selected = false;
                    self.ram_bank = value as usize % self.num_ram_banks.max(1);
                } else if (0x08..=0x0C).contains(&value) {
                    self.rtc_selected = true;
                    self.rtc_register = value - 0x08;
                }
            }
            0x6000..=0x7FFF => {
                if value == 0x00 && self.rtc_latch == 0x01 {
                    self.rtc_latched = self.rtc;
                }
                self.rtc_latch = value;
            }
            _ => {}
        }
    }

    pub fn write_ram(&mut self, addr: u16, value: u8) {
        if !self.ram_enabled || self.ram.is_empty() {
            return;
        }
        if self.mbc == MbcType::Mbc3 && self.rtc_selected {
            self.rtc[self.rtc_register as usize] = value;
            return;
        }
        let bank = match self.mbc {
            MbcType::Mbc1 => {
                if self.bank_mode {
                    self.ram_bank
                } else {
                    0
                }
            }
            MbcType::Mbc3 => self.ram_bank,
            _ => 0,
        };
        let offset = bank * 0x2000 + (addr as usize - 0xA000);
        if let Some(slot) = self.ram.get_mut(offset) {
            *slot = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rom(mbc_code: u8, ram_size_code: u8) -> Vec<u8> {
        let mut data = vec![0u8; 0x8000];
        data[0x147] = mbc_code;
        data[0x148] = 0x05; // 64 banks (enough)
        data[0x149] = ram_size_code;
        data
    }

    #[test]
    fn detects_battery() {
        let rom = make_rom(0x13, 0x03); // MBC3+RAM+BATTERY
        let cart = Cartridge::load(&rom).unwrap();
        assert!(cart.has_battery());
        assert_eq!(cart.ram_bytes(), 0x8000);
        assert_eq!(cart.mbc, MbcType::Mbc3);

        let rom = make_rom(0x00, 0x00);
        let cart = Cartridge::load(&rom).unwrap();
        assert!(!cart.has_battery());
        assert_eq!(cart.ram_bytes(), 0);
    }

    #[test]
    fn mbc3_sram_banking() {
        let rom = make_rom(0x13, 0x03); // 4 banks * 0x2000
        let mut cart = Cartridge::load(&rom).unwrap();

        // writes are gated until RAM enabled
        cart.write_ram(0xA000, 0xAA);
        assert_eq!(cart.read_ram(0xA000), 0xFF);

        // enable RAM
        cart.write(0x0000, 0x0A);
        assert!(cart.ram_enabled);

        // write distinct bytes to each bank
        for bank in 0..4u8 {
            cart.write(0x4000, bank);
            cart.write_ram(0xA123, 0x40 + bank);
        }

        // verify isolation
        for bank in 0..4u8 {
            cart.write(0x4000, bank);
            assert_eq!(cart.read_ram(0xA123), 0x40 + bank);
        }

        // bank 0 unchanged elsewhere
        cart.write(0x4000, 0);
        assert_eq!(cart.read_ram(0xA000), 0);
    }

    #[test]
    fn mbc3_rtc_register_vs_ram() {
        let rom = make_rom(0x0F, 0x02); // MBC3+TIMER+BATTERY with 1 ram bank
        let mut cart = Cartridge::load(&rom).unwrap();
        cart.write(0x0000, 0x0A);

        // select RTC register 1 and write to it
        cart.write(0x4000, 0x09);
        assert!(cart.rtc_selected);
        cart.write_ram(0xA000, 0x77);
        assert_eq!(cart.rtc[1], 0x77);

        // latch to make rtc visible via reads
        cart.write(0x6000, 0x01);
        cart.write(0x6000, 0x00);
        assert_eq!(cart.read_ram(0xA000), 0x77);

        // back to RAM select (value <= 3)
        cart.write(0x4000, 0x00);
        assert!(!cart.rtc_selected);
    }
}