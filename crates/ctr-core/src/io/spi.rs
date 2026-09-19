//! The SPI buses in their FIFO mode (GodMode9, `common/spi.c`; 3dbrew, "SPI
//! Registers"), at 0x10160800, 0x10142800 and 0x10143800.
//!
//! A transfer is programmed with its byte length (`BLKLEN`, +8) and started
//! through `CNT` (+0): bit 15 starts and reads back as busy until the last
//! byte has moved, bit 13 is the direction (set to write) and bits 7:6 select
//! the chip. Data moves through `FIFO` (+0xC) four bytes at a time; bit 0 of
//! `STAT` (+0x10) would hold a driver off while the FIFO is busy, which it
//! never is here. Writing `DONE` (+4) deselects the chip.
//!
//! The only chip modelled is the CODEC on bus 1: a bank of register pages
//! that hold what is written. Other chips read as 0xFF.

const CNT_BUSY: u32 = 1 << 15;
const CNT_WRITE: u32 = 1 << 13;

/// The audio and touch CODEC: pages of 128 registers, selected through
/// register 0. A command byte is the register number shifted left once, with
/// bit 0 set to read; the register pointer then advances per byte.
pub struct Codec {
    pages: Vec<[u8; 128]>,
    page: u8,
    pointer: Option<(u8, bool)>,
    /// The touch position as 12-bit converter readings, while touched.
    pub touch: Option<(u16, u16)>,
    /// The circle pad's deflection in converter units, right and up positive.
    pub circle_pad: (i16, i16),
}

/// The page of converter samples: registers 1 to 0x34 hold five touch X
/// readings, five touch Y readings, eight circle pad Y readings and eight
/// circle pad X readings, each a big-endian halfword (GodMode9,
/// `arm11/source/hw/codec.c`). Bit 12 of a touch reading means "not touched",
/// the circle pad rests at 0x800 and its X axis is inverted.
const SAMPLE_PAGE: u8 = 0xFB;

/// Converter units at full deflection. The real travel is not documented;
/// this clears the thresholds drivers use.
pub const CIRCLE_PAD_RANGE: i16 = 0x600;

impl Default for Codec {
    fn default() -> Self {
        Codec {
            pages: vec![[0; 128]; 256],
            page: 0,
            pointer: None,
            touch: None,
            circle_pad: (0, 0),
        }
    }
}

impl Codec {
    fn deselect(&mut self) {
        self.pointer = None;
    }

    fn write(&mut self, byte: u8) {
        match self.pointer {
            None => self.pointer = Some((byte >> 1, byte & 1 != 0)),
            Some((reg, read)) => {
                if reg == 0 {
                    self.page = byte;
                } else {
                    self.pages[self.page as usize][reg as usize & 0x7F] = byte;
                }
                self.pointer = Some((reg.wrapping_add(1) & 0x7F, read));
            }
        }
    }

    fn read(&mut self) -> u8 {
        let Some((reg, read)) = self.pointer else {
            return 0;
        };
        self.pointer = Some((reg.wrapping_add(1) & 0x7F, read));
        if reg == 0 {
            self.page
        } else if self.page == SAMPLE_PAGE && (1..=0x34).contains(&reg) {
            let index = (reg - 1) as usize;
            let sample: u16 = match index / 2 {
                0..=4 => self.touch.map_or(0x1000, |(x, _)| x & 0xFFF),
                5..=9 => self.touch.map_or(0x1000, |(_, y)| y & 0xFFF),
                10..=17 => (0x800 + self.circle_pad.1) as u16 & 0xFFF,
                _ => (0x800 - self.circle_pad.0) as u16 & 0xFFF,
            };
            sample.to_be_bytes()[index % 2]
        } else {
            self.pages[self.page as usize][reg as usize & 0x7F]
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Bus {
    control: u32,
    block_len: u32,
    remaining: u32,
}

#[derive(Default)]
pub struct Spi {
    buses: [Bus; 3],
    pub codec: Codec,
}

impl Spi {
    pub fn new() -> Self {
        Spi::default()
    }

    fn is_codec(bus: usize, control: u32) -> bool {
        bus == 1 && control >> 6 & 3 == 0
    }

    /// Read the word at `offset` (from the bus's 0x800) of bus `bus`.
    pub fn read(&mut self, bus: usize, offset: u32) -> u32 {
        let b = self.buses[bus];
        match offset {
            0x00 => b.control,
            0x08 => b.block_len,
            0x0C => {
                let count = b.remaining.min(4);
                let mut bytes = [0u8; 4];
                for byte in bytes.iter_mut().take(count as usize) {
                    *byte = if Spi::is_codec(bus, b.control) {
                        self.codec.read()
                    } else {
                        0xFF
                    };
                }
                self.moved(bus, count);
                u32::from_le_bytes(bytes)
            }
            _ => 0,
        }
    }

    pub fn write(&mut self, bus: usize, offset: u32, value: u32) {
        match offset {
            0x00 => {
                let b = &mut self.buses[bus];
                b.control = value;
                if value & CNT_BUSY != 0 {
                    b.remaining = b.block_len;
                    if b.remaining == 0 {
                        b.control &= !CNT_BUSY;
                    }
                }
            }
            0x04 => {
                if Spi::is_codec(bus, self.buses[bus].control) {
                    self.codec.deselect();
                }
            }
            0x08 => self.buses[bus].block_len = value & 0x1F_FFFF,
            0x0C => {
                let b = self.buses[bus];
                if b.control & CNT_WRITE == 0 {
                    return;
                }
                let count = b.remaining.min(4);
                if Spi::is_codec(bus, b.control) {
                    for byte in value.to_le_bytes().iter().take(count as usize) {
                        self.codec.write(*byte);
                    }
                }
                self.moved(bus, count);
            }
            _ => {}
        }
    }

    fn moved(&mut self, bus: usize, count: u32) {
        let b = &mut self.buses[bus];
        b.remaining -= count;
        if b.remaining == 0 {
            b.control &= !CNT_BUSY;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODEC_BUS: usize = 1;

    fn transfer(spi: &mut Spi, write: &[u8], read: usize, done: bool) -> Vec<u8> {
        let mut word = [0u8; 4];
        word[..write.len()].copy_from_slice(write);
        spi.write(CODEC_BUS, 0x08, write.len() as u32);
        spi.write(CODEC_BUS, 0x00, 5 | CNT_WRITE | CNT_BUSY);
        spi.write(CODEC_BUS, 0x0C, u32::from_le_bytes(word));
        assert_eq!(spi.read(CODEC_BUS, 0x00) & CNT_BUSY, 0);
        let mut out = Vec::new();
        if read > 0 {
            spi.write(CODEC_BUS, 0x08, read as u32);
            spi.write(CODEC_BUS, 0x00, 5 | CNT_BUSY);
            assert_ne!(spi.read(CODEC_BUS, 0x00) & CNT_BUSY, 0);
            while out.len() < read {
                out.extend(spi.read(CODEC_BUS, 0x0C).to_le_bytes());
            }
            out.truncate(read);
            assert_eq!(spi.read(CODEC_BUS, 0x00) & CNT_BUSY, 0);
        }
        if done {
            spi.write(CODEC_BUS, 0x04, 0);
        }
        out
    }

    #[test]
    fn codec_registers_hold_their_values_per_page() {
        let mut spi = Spi::new();
        transfer(&mut spi, &[0, 0x67], 0, true); // select page 0x67
        transfer(&mut spi, &[0x24 << 1, 0x98], 0, true);
        assert_eq!(transfer(&mut spi, &[0x24 << 1 | 1], 1, true), [0x98]);
        transfer(&mut spi, &[0, 0x01], 0, true);
        assert_eq!(transfer(&mut spi, &[0x24 << 1 | 1], 1, true), [0]);
        assert_eq!(transfer(&mut spi, &[1], 1, true), [0x01], "the page");
    }

    #[test]
    fn the_sample_page_reports_the_touch_screen_and_the_circle_pad() {
        let mut spi = Spi::new();
        transfer(&mut spi, &[0, SAMPLE_PAGE], 0, true);
        let idle = transfer(&mut spi, &[1 << 1 | 1], 0x34, true);
        assert_eq!(idle[0] & 0x10, 0x10, "not touched");
        assert_eq!(idle[0x14..0x16], [0x08, 0x00], "circle pad Y at rest");
        assert_eq!(idle[0x24..0x26], [0x08, 0x00], "circle pad X at rest");

        spi.codec.touch = Some((0x123, 0x456));
        spi.codec.circle_pad = (0x100, -0x200);
        let held = transfer(&mut spi, &[1 << 1 | 1], 0x34, true);
        assert_eq!(held[0..2], [0x01, 0x23]);
        assert_eq!(held[10..12], [0x04, 0x56]);
        assert_eq!(held[0x14..0x16], [0x06, 0x00], "down");
        assert_eq!(held[0x24..0x26], [0x07, 0x00], "right reads lower");
    }

    #[test]
    fn reads_advance_through_the_registers() {
        let mut spi = Spi::new();
        transfer(&mut spi, &[0x10 << 1, 1, 2, 3], 0, true);
        assert_eq!(
            transfer(&mut spi, &[0x10 << 1 | 1], 5, true),
            [1, 2, 3, 0, 0]
        );
    }

    #[test]
    fn a_bus_without_a_chip_reads_high() {
        let mut spi = Spi::new();
        spi.write(0, 0x08, 2);
        spi.write(0, 0x00, 1 << 6 | CNT_BUSY);
        assert_eq!(spi.read(0, 0x0C), 0x0000_FFFF);
    }
}
