//! The SD/MMC host controller at 0x10006000 and the cards behind its two
//! ports: the SD slot (port 0) and the eMMC that holds the NAND (port 1).
//!
//! The controller is a Toshiba "TMIO" block of 16-bit registers (3dbrew,
//! "EMMC Registers"; the register and bit names follow libn3ds's `tmio.h`).
//! A write to `CMD` runs a command on the selected port; the response lands
//! in `RESP0`-`RESP7` and `STATUS` reports the end of the response and of the
//! data. Data moves a block at a time through the 16-bit FIFO at +0x30 or
//! the 32-bit one at +0x10C. Commands complete at once.
//!
//! Cards follow the SD Physical Layer and JEDEC eMMC specifications as far as
//! drivers exercise them: identification, selection, block length, single
//! and multiple block reads and writes, and the few data-bearing queries.

use std::collections::BTreeMap;

const SECTOR: usize = 512;

/// `STATUS` bits, as one 32-bit value.
mod status {
    pub const RESP_END: u32 = 1 << 0;
    pub const DATA_END: u32 = 1 << 2;
    pub const INSERTED: u32 = 1 << 5;
    pub const WRITABLE: u32 = 1 << 7;
    pub const CMD_TIMEOUT: u32 = 1 << 22;
    pub const NOT_BUSY: u32 = 1 << 23;
    pub const RX_READY: u32 = 1 << 24;
    pub const TX_REQUEST: u32 = 1 << 25;
    /// Bits a driver acknowledges by writing zero.
    pub const EVENTS: u32 = 0x837F_031D;
}

const DATACTL32_ENABLE: u32 = 1 << 1;
const DATACTL32_RX_READY: u32 = 1 << 8;
#[cfg(test)]
const DATACTL32_TX_PENDING: u32 = 1 << 9;
const DATACTL32_KEPT: u32 = 0x1802;

/// Card states, as reported in bits 12:9 of the card status.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Idle = 0,
    Ready = 1,
    Identification = 2,
    Standby = 3,
    Transfer = 4,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardKind {
    /// An SD card, presented as high capacity (sector addressed).
    Sd,
    /// The eMMC, byte addressed as on the console.
    Mmc,
}

/// What a command asks the controller to move.
enum Data {
    None,
    /// Bytes from the card to the host.
    Read(Vec<u8>),
    /// Blocks from `sector` on, either way.
    Blocks {
        sector: u32,
        write: bool,
    },
}

struct Reply {
    response: [u32; 4],
    data: Data,
}

/// A card: an optional image plus the sectors written since.
pub struct Card {
    kind: CardKind,
    image: Vec<u8>,
    written: BTreeMap<u32, [u8; SECTOR]>,
    sectors: u32,
    state: State,
    rca: u16,
    app_command: bool,
}

impl Card {
    /// A card of `sectors` 512-byte sectors holding `image` (zero beyond it).
    pub fn new(kind: CardKind, image: Vec<u8>, sectors: u32) -> Self {
        Card {
            kind,
            image,
            written: BTreeMap::new(),
            sectors,
            state: State::Idle,
            rca: 0,
            app_command: false,
        }
    }

    pub fn read_sector(&self, sector: u32) -> [u8; SECTOR] {
        if let Some(data) = self.written.get(&sector) {
            return *data;
        }
        let mut out = [0u8; SECTOR];
        let start = sector as usize * SECTOR;
        if let Some(bytes) = self.image.get(start..) {
            let len = bytes.len().min(SECTOR);
            out[..len].copy_from_slice(&bytes[..len]);
        }
        out
    }

    fn write_sector(&mut self, sector: u32, data: [u8; SECTOR]) {
        if sector < self.sectors {
            self.written.insert(sector, data);
        }
    }

    fn card_status(&self) -> u32 {
        (self.state as u32) << 9 | 1 << 8 | (self.app_command as u32) << 5
    }

    /// The card-specific data register, without its checksum byte, as the
    /// controller presents it: bits 127:8 in little-endian order.
    fn csd(&self) -> [u32; 4] {
        let mut csd: u128 = 0;
        match self.kind {
            CardKind::Sd => {
                // Version 2: capacity in units of 512 KB, minus one.
                csd |= 1 << 126;
                // The command classes leave out class 10 (SWITCH), as cards
                // older than SD 1.10 do: how the controller ends a 64-byte
                // read under a 512-byte block length, which GodMode9's driver
                // relies on, is not documented.
                csd |= 0x0E << 112 | 0x32 << 96 | 0x1B5 << 84 | 9 << 80;
                csd |= ((self.sectors as u128 / 1024).saturating_sub(1)) << 48;
                csd |= 1 << 46 | 0x7F << 39 | 0x0A << 22 | 1 << 14;
            }
            CardKind::Mmc => {
                // Version 1 fields: 1 KB blocks times 512 times (C_SIZE + 1).
                csd |= 0b11 << 126 | 4 << 122;
                csd |= 0x0E << 112 | 0x32 << 96 | 0x0F5 << 84 | 10 << 80;
                let units = (self.sectors as u128 * SECTOR as u128).div_ceil(1024 * 512);
                csd |= (units.saturating_sub(1) & 0xFFF) << 62;
                csd |= 7 << 47;
            }
        }
        shifted(csd)
    }

    fn cid(&self) -> [u32; 4] {
        // Manufacturer 0, the name "CRAB", revision 1.0 and a serial number.
        let cid: u128 = (b'C' as u128) << 96
            | (b'R' as u128) << 88
            | (b'A' as u128) << 80
            | (b'B' as u128) << 72
            | 0x10 << 56
            | 0x0000_0001 << 24;
        shifted(cid)
    }

    fn command(&mut self, index: u32, arg: u32) -> Option<Reply> {
        let app = std::mem::take(&mut self.app_command);
        let sd = self.kind == CardKind::Sd;
        let r1 = |card: &Card| [card.card_status(), 0, 0, 0];
        let plain = |response| {
            Some(Reply {
                response,
                data: Data::None,
            })
        };
        // High-capacity SD cards take sector numbers; everything else bytes.
        let sector_of = |arg: u32| if sd { arg } else { arg / SECTOR as u32 };

        if app && sd {
            return match index {
                6 | 42 => plain(r1(self)),
                // SD status: all zero is a valid report.
                13 => Some(Reply {
                    response: r1(self),
                    data: Data::Read(vec![0; 64]),
                }),
                41 => {
                    self.state = State::Ready;
                    // Powered up, high capacity, 2.7-3.6 V.
                    plain([0xC0FF_8000, 0, 0, 0])
                }
                // SCR: SD 2.0, one and four bit buses.
                51 => Some(Reply {
                    response: r1(self),
                    data: Data::Read(vec![0x02, 0x35, 0, 0, 0, 0, 0, 0]),
                }),
                _ => None,
            };
        }
        match index {
            0 => {
                self.state = State::Idle;
                self.rca = 0;
                plain([0; 4])
            }
            1 if !sd => {
                self.state = State::Ready;
                // Powered up, byte addressed, 2.7-3.6 V.
                plain([0x80FF_8080, 0, 0, 0])
            }
            2 => {
                self.state = State::Identification;
                plain(self.cid())
            }
            3 => {
                self.state = State::Standby;
                if sd {
                    self.rca = 1;
                    plain([
                        (self.rca as u32) << 16 | self.card_status() & 0xFFFF,
                        0,
                        0,
                        0,
                    ])
                } else {
                    self.rca = (arg >> 16) as u16;
                    plain(r1(self))
                }
            }
            6 if sd => Some(Reply {
                // Switch function status: nothing but the default supported.
                response: r1(self),
                data: Data::Read(vec![0; 64]),
            }),
            6 => plain(r1(self)),
            7 => {
                self.state = if (arg >> 16) as u16 == self.rca {
                    State::Transfer
                } else {
                    State::Standby
                };
                plain(r1(self))
            }
            8 if sd => plain([arg & 0xFFF, 0, 0, 0]),
            8 => {
                // Extended CSD: only the sector count is filled in.
                let mut ext = vec![0u8; SECTOR];
                ext[212..216].copy_from_slice(&self.sectors.to_le_bytes());
                Some(Reply {
                    response: r1(self),
                    data: Data::Read(ext),
                })
            }
            9 => plain(self.csd()),
            10 => plain(self.cid()),
            12 | 13 | 16 => plain(r1(self)),
            17 | 18 => Some(Reply {
                response: r1(self),
                data: Data::Blocks {
                    sector: sector_of(arg),
                    write: false,
                },
            }),
            24 | 25 => Some(Reply {
                response: r1(self),
                data: Data::Blocks {
                    sector: sector_of(arg),
                    write: true,
                },
            }),
            55 => {
                self.app_command = true;
                plain(r1(self))
            }
            _ => None,
        }
    }
}

/// A 128-bit card register as the response registers hold it: without its
/// low byte, least significant word first.
fn shifted(register: u128) -> [u32; 4] {
    let value = register >> 8;
    [
        value as u32,
        (value >> 32) as u32,
        (value >> 64) as u32,
        (value >> 96) as u32,
    ]
}

struct Transfer {
    port: usize,
    write: bool,
    /// The next sector, for block transfers.
    sector: Option<u32>,
    blocks_left: u32,
    buffer: Vec<u8>,
    position: usize,
}

pub struct Sdmmc {
    /// The SD slot, then the eMMC.
    pub cards: [Option<Card>; 2],
    regs: [u16; 0x80],
    status: u32,
    irq_mask: u32,
    response: [u32; 4],
    datactl32: u32,
    block_len32: u16,
    block_count32: u16,
    transfer: Option<Transfer>,
}

impl Default for Sdmmc {
    fn default() -> Self {
        Self::new()
    }
}

const REG_CMD: usize = 0x00;
const REG_PORTSEL: usize = 0x02;
const REG_ARG0: usize = 0x04;
const REG_ARG1: usize = 0x06;
const REG_BLKCOUNT: usize = 0x0A;
const REG_BLKLEN: usize = 0x26;

impl Sdmmc {
    pub fn new() -> Self {
        Sdmmc {
            cards: [None, None],
            regs: [0; 0x80],
            status: 0,
            irq_mask: 0x837F_031D,
            response: [0; 4],
            datactl32: 0,
            block_len32: SECTOR as u16,
            block_count32: 0,
            transfer: None,
        }
    }

    fn port(&self) -> usize {
        (self.regs[REG_PORTSEL / 2] & 1) as usize
    }

    fn fifo32(&self) -> bool {
        self.datactl32 & DATACTL32_ENABLE != 0
    }

    /// Whether the 32-bit FIFO asks the DMA controller for service: a block
    /// is waiting to be read, or one is wanted for writing.
    pub fn dma_request(&self) -> bool {
        self.datactl32 & DATACTL32_ENABLE != 0 && self.transfer.is_some()
    }

    /// Whether an unmasked event is pending, for the ARM9 interrupt.
    pub fn interrupting(&self) -> bool {
        self.status & !self.irq_mask & status::EVENTS != 0
    }

    fn full_status(&self) -> u32 {
        // Card detection belongs to the SD slot, whichever port is selected.
        let card = self.cards[0].is_some();
        self.status
            | status::NOT_BUSY
            | if card {
                status::INSERTED | status::WRITABLE
            } else {
                0
            }
    }

    pub fn read16(&mut self, offset: usize) -> u16 {
        match offset {
            0x0C..=0x1A => (self.response[(offset - 0x0C) / 4] >> ((offset & 2) * 8)) as u16,
            0x1C => self.full_status() as u16,
            0x1E => (self.full_status() >> 16) as u16,
            0x20 => self.irq_mask as u16,
            0x22 => (self.irq_mask >> 16) as u16,
            0x30 => self.pop(2) as u16,
            0x100 => {
                let mut value = self.datactl32 & DATACTL32_KEPT;
                // Bit 9 would say the transmit FIFO still holds data; blocks are
                // taken at once here, so it always reads as empty.
                if self.transfer.as_ref().is_some_and(|t| !t.write) {
                    value |= DATACTL32_RX_READY;
                }
                value as u16
            }
            0x104 => self.block_len32,
            0x108 => self.block_count32,
            _ => self.regs.get(offset / 2).copied().unwrap_or(0),
        }
    }

    /// The 32-bit FIFO.
    pub fn read_fifo32(&mut self) -> u32 {
        self.pop(4)
    }

    pub fn write_fifo32(&mut self, value: u32) {
        self.push(value, 4);
    }

    pub fn write16(&mut self, offset: usize, value: u16) {
        match offset {
            // Events clear where a zero is written.
            0x1C => self.status &= value as u32 | 0xFFFF_0000 | !status::EVENTS,
            0x1E => self.status &= (value as u32) << 16 | 0x0000_FFFF | !status::EVENTS,
            0x20 => self.irq_mask = self.irq_mask & 0xFFFF_0000 | value as u32,
            0x22 => self.irq_mask = self.irq_mask & 0xFFFF | (value as u32) << 16,
            0x30 => self.push(value as u32, 2),
            0x100 => self.datactl32 = value as u32 & DATACTL32_KEPT,
            0x104 => self.block_len32 = value & 0x3FF,
            0x108 => self.block_count32 = value,
            _ => {
                if let Some(reg) = self.regs.get_mut(offset / 2) {
                    *reg = value;
                }
                if offset == REG_CMD {
                    self.run(value as u32);
                }
            }
        }
    }

    fn run(&mut self, command: u32) {
        let port = self.port();
        let arg = self.regs[REG_ARG0 / 2] as u32 | (self.regs[REG_ARG1 / 2] as u32) << 16;
        self.status &= !status::EVENTS;
        self.transfer = None;
        let reply = self.cards[port]
            .as_mut()
            .and_then(|card| card.command(command & 0x3F, arg));
        let Some(reply) = reply else {
            // No card, or a command it does not answer.
            self.status |= status::CMD_TIMEOUT | status::RESP_END;
            return;
        };
        self.response = reply.response;
        self.status |= status::RESP_END;

        let block_len = if self.fifo32() {
            self.block_len32
        } else {
            self.regs[REG_BLKLEN / 2]
        } as usize;
        match reply.data {
            Data::None => {}
            Data::Read(mut bytes) => {
                bytes.resize(block_len.max(1), 0);
                self.begin(Transfer {
                    port,
                    write: false,
                    sector: None,
                    blocks_left: 1,
                    buffer: bytes,
                    position: 0,
                });
            }
            Data::Blocks { sector, write } => {
                let multiple = command & 1 << 13 != 0;
                let count = if !multiple {
                    1
                } else if self.fifo32() {
                    self.block_count32
                } else {
                    self.regs[REG_BLKCOUNT / 2]
                };
                self.begin(Transfer {
                    port,
                    write,
                    sector: Some(sector),
                    blocks_left: count.max(1) as u32,
                    buffer: vec![0; SECTOR],
                    position: 0,
                });
            }
        }
    }

    /// Start a transfer: load the first block to read, or ask for one.
    fn begin(&mut self, mut transfer: Transfer) {
        if transfer.write {
            self.status |= status::TX_REQUEST;
        } else {
            if let (Some(sector), Some(card)) = (transfer.sector, &self.cards[transfer.port]) {
                transfer.buffer = card.read_sector(sector).to_vec();
            }
            self.status |= status::RX_READY;
        }
        self.transfer = Some(transfer);
    }

    /// The host finished a block: move on to the next one or finish.
    fn block_done(&mut self) {
        let Some(mut transfer) = self.transfer.take() else {
            return;
        };
        if transfer.write {
            if let (Some(sector), Some(card)) = (transfer.sector, &mut self.cards[transfer.port]) {
                let mut data = [0u8; SECTOR];
                let len = transfer.buffer.len().min(SECTOR);
                data[..len].copy_from_slice(&transfer.buffer[..len]);
                card.write_sector(sector, data);
            }
        }
        transfer.blocks_left -= 1;
        if transfer.blocks_left == 0 {
            self.status |= status::DATA_END;
            return;
        }
        transfer.sector = transfer.sector.map(|s| s.wrapping_add(1));
        transfer.position = 0;
        self.begin(transfer);
    }

    fn pop(&mut self, width: usize) -> u32 {
        let Some(transfer) = self.transfer.as_mut().filter(|t| !t.write) else {
            return 0;
        };
        let mut bytes = [0u8; 4];
        for byte in bytes.iter_mut().take(width) {
            *byte = transfer.buffer.get(transfer.position).copied().unwrap_or(0);
            transfer.position += 1;
        }
        if transfer.position >= transfer.buffer.len() {
            self.block_done();
        }
        u32::from_le_bytes(bytes)
    }

    fn push(&mut self, value: u32, width: usize) {
        let Some(transfer) = self.transfer.as_mut().filter(|t| t.write) else {
            return;
        };
        for byte in value.to_le_bytes().iter().take(width) {
            if let Some(slot) = transfer.buffer.get_mut(transfer.position) {
                *slot = *byte;
            }
            transfer.position += 1;
        }
        if transfer.position >= transfer.buffer.len() {
            self.block_done();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(sd: &mut Sdmmc, command: u16, arg: u32) -> [u32; 4] {
        sd.write16(0x1C, 0);
        sd.write16(0x1E, 0);
        sd.write16(REG_ARG0, arg as u16);
        sd.write16(REG_ARG1, (arg >> 16) as u16);
        sd.write16(REG_CMD, command);
        assert_ne!(sd.read16(0x1C) as u32 & status::RESP_END, 0);
        let mut out = [0u32; 4];
        for (n, word) in out.iter_mut().enumerate() {
            *word = sd.read16(0x0C + n * 4) as u32 | (sd.read16(0x0E + n * 4) as u32) << 16;
        }
        out
    }

    /// GodMode9's capacity calculation on the response bytes.
    fn capacity_in_sectors(response: [u32; 4], forced_v1: bool) -> u32 {
        let csd: Vec<u8> = response.iter().flat_map(|w| w.to_le_bytes()).collect();
        let version = if forced_v1 { 0 } else { csd[14] >> 6 };
        if version == 0 {
            let block_len = 1u32 << (csd[9] & 0xF);
            let mult = 1u32 << (((csd[4] >> 7) | (csd[5] & 3) << 1) + 2);
            let c_size = ((csd[8] as u32 & 3) << 8 | csd[7] as u32) << 2 | (csd[6] >> 6) as u32;
            (c_size + 1) * mult * block_len / 512
        } else {
            let c_size = (csd[7] as u32 & 0x3F) << 16 | (csd[6] as u32) << 8 | csd[5] as u32;
            (c_size + 1) * 1024
        }
    }

    fn sd_card(image: Vec<u8>, sectors: u32) -> Sdmmc {
        let mut sd = Sdmmc::new();
        sd.cards[0] = Some(Card::new(CardKind::Sd, image, sectors));
        sd
    }

    fn init_sd(sd: &mut Sdmmc) {
        command(sd, 0, 0);
        assert_eq!(command(sd, 0x0408, 0x1AA)[0], 0x1AA);
        command(sd, 0x0437, 0);
        let ocr = command(sd, 0x0769, 0x5010_0000)[0];
        assert_eq!(ocr >> 30, 0b11, "powered up and high capacity");
        command(sd, 0x0602, 0);
        let rca = command(sd, 0x0403, 0)[0] >> 16;
        command(sd, 0x0507, rca << 16);
    }

    #[test]
    fn an_empty_slot_times_out_and_reports_no_card() {
        let mut sd = Sdmmc::new();
        command(&mut sd, 0, 0);
        let status = sd.read16(0x1C) as u32 | (sd.read16(0x1E) as u32) << 16;
        assert_ne!(status & status::CMD_TIMEOUT, 0);
        assert_eq!(status & status::INSERTED, 0);
    }

    #[test]
    fn identification_reports_the_capacity_a_driver_computes() {
        let mut sd = sd_card(Vec::new(), 64 * 1024 * 2);
        init_sd(&mut sd);
        command(&mut sd, 0x0407, 0);
        let csd = command(&mut sd, 0x0609, 1 << 16);
        assert_eq!(capacity_in_sectors(csd, false), 64 * 1024 * 2);

        let mut mmc = Sdmmc::new();
        mmc.cards[1] = Some(Card::new(CardKind::Mmc, Vec::new(), 0x1D_7800));
        mmc.write16(REG_PORTSEL, 1);
        command(&mut mmc, 0, 0);
        assert_ne!(command(&mut mmc, 0x0701, 0x10_0000)[0] & 1 << 31, 0);
        command(&mut mmc, 0x0602, 0);
        command(&mut mmc, 0x0403, 1 << 16);
        let csd = command(&mut mmc, 0x0609, 1 << 16);
        assert_eq!(capacity_in_sectors(csd, true), 0x1D_7800);
    }

    #[test]
    fn multiple_block_reads_through_the_32_bit_fifo() {
        let mut image = vec![0u8; 3 * SECTOR];
        image[SECTOR] = 0x11;
        image[2 * SECTOR + 511] = 0x22;
        let mut sd = sd_card(image, 1 << 20);
        init_sd(&mut sd);
        sd.write16(0x100, 0x0002);
        sd.write16(0x104, 512);
        sd.write16(0x108, 2);
        command(&mut sd, 0x3C12, 1);
        let mut data = Vec::new();
        for block in 0..2 {
            assert_ne!(sd.read16(0x100) as u32 & DATACTL32_RX_READY, 0, "{block}");
            for _ in 0..128 {
                data.extend(sd.read_fifo32().to_le_bytes());
            }
        }
        assert_eq!(data[0], 0x11);
        assert_eq!(data[1023], 0x22);
        assert_ne!(sd.read16(0x1C) as u32 & status::DATA_END, 0);
        assert_eq!(sd.read16(0x100) as u32 & DATACTL32_RX_READY, 0);
    }

    #[test]
    fn writes_land_in_the_overlay_and_read_back() {
        let mut sd = sd_card(Vec::new(), 1 << 20);
        init_sd(&mut sd);
        sd.write16(0x100, 0x0002);
        sd.write16(0x108, 1);
        command(&mut sd, 0x2C19, 7);
        assert_eq!(sd.read16(0x100) as u32 & DATACTL32_TX_PENDING, 0);
        for n in 0..128u32 {
            sd.write_fifo32(n);
        }
        assert_ne!(sd.read16(0x1C) as u32 & status::DATA_END, 0);
        let sector = sd.cards[0].as_ref().unwrap().read_sector(7);
        assert_eq!(sector[4..8], 1u32.to_le_bytes());
        assert_eq!(sd.cards[0].as_ref().unwrap().read_sector(8), [0; SECTOR]);
    }

    #[test]
    fn the_emmc_is_byte_addressed() {
        let mut image = vec![0u8; 2 * SECTOR];
        image[SECTOR] = 0x5A;
        let mut mmc = Sdmmc::new();
        mmc.cards[1] = Some(Card::new(CardKind::Mmc, image, 0x1D_7800));
        mmc.write16(REG_PORTSEL, 1);
        command(&mut mmc, 0, 0);
        command(&mut mmc, 0x0701, 0x10_0000);
        command(&mut mmc, 0x0602, 0);
        command(&mut mmc, 0x0403, 1 << 16);
        command(&mut mmc, 0x0407, 1 << 16);
        mmc.write16(REG_BLKLEN, 512);
        command(&mut mmc, 0x1C11, 512);
        assert_eq!(mmc.read16(0x30), 0x005A);
    }

    #[test]
    fn events_acknowledge_by_writing_zero_and_drive_the_interrupt() {
        let mut sd = sd_card(Vec::new(), 1 << 20);
        command(&mut sd, 0, 0);
        assert!(!sd.interrupting(), "masked after reset");
        sd.write16(0x20, 0);
        assert!(sd.interrupting());
        sd.write16(0x1C, !(status::RESP_END as u16));
        assert!(!sd.interrupting());
    }
}
