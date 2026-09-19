//! The ARM9's DMA controller ("NDMA") at 0x10002000: a global control word
//! and eight channels of seven registers each.
//!
//! A channel moves `WCNT` words per request ("logical block"), from memory,
//! a fixed address or its fill register. A request is either immediate, made
//! by enabling the channel, or raised by a device. Outside repeat mode a
//! device-started channel stops after `TCNT` words in total. Transfers
//! complete at once; the bus time they take is not modelled yet (see
//! `docs/3ds/clocks.md`).

/// `CNT`: the channel is enabled, and busy while set.
pub const ENABLE: u32 = 1 << 31;
/// `CNT`: raise the channel's interrupt when the transfer ends.
pub const IRQ_ENABLE: u32 = 1 << 30;
/// `CNT`: repeat on every device request, ignoring `TCNT`.
pub const REPEAT: u32 = 1 << 29;
/// `CNT`: start immediately instead of on a device request.
pub const IMMEDIATE: u32 = 1 << 28;
/// `GCNT`: the address and count registers read back the internal state.
const READBACK: u32 = 1;

/// The devices that can request a transfer, by startup mode number.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Device {
    Tmio1 = 6,
    Tmio3 = 7,
    AesIn = 8,
    AesOut = 9,
    ShaIn = 10,
    ShaOut = 11,
}

/// How an address moves after each word.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Update {
    Increment,
    Decrement,
    Fixed,
    /// Source only: there is no address, the data is the fill register.
    Fill,
}

impl Update {
    fn of(bits: u32) -> Self {
        match bits & 3 {
            0 => Update::Increment,
            1 => Update::Decrement,
            2 => Update::Fixed,
            _ => Update::Fill,
        }
    }

    /// The address of the word after the one at `addr`.
    pub fn step(self, addr: u32) -> u32 {
        match self {
            Update::Increment => addr.wrapping_add(4),
            Update::Decrement => addr.wrapping_sub(4),
            Update::Fixed | Update::Fill => addr,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Channel {
    sad: u32,
    dad: u32,
    tcnt: u32,
    wcnt: u32,
    bcnt: u32,
    fdata: u32,
    cnt: u32,
    /// The working copies, loaded when the address registers are written.
    cur_sad: u32,
    cur_dad: u32,
    /// Words left of `TCNT` for a device-started transfer.
    remaining: u32,
}

/// One logical block for the bus to carry out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Block {
    pub channel: usize,
    pub src: u32,
    pub dst: u32,
    pub src_update: Update,
    pub dst_update: Update,
    pub fill: u32,
    pub words: u32,
}

#[derive(Default)]
pub struct Ndma {
    gcnt: u32,
    channels: [Channel; 8],
    /// Channels with a request outstanding, one bit each.
    requests: u8,
}

impl Ndma {
    pub fn read(&self, offset: u32) -> u32 {
        let offset = offset & 0xFF;
        if offset == 0 {
            return self.gcnt;
        }
        let (index, reg) = ((offset - 4) / 0x1C, (offset - 4) % 0x1C);
        let Some(ch) = self.channels.get(index as usize) else {
            return 0;
        };
        let live = self.gcnt & READBACK != 0;
        match reg {
            0x00 if live => ch.cur_sad,
            0x00 => ch.sad,
            0x04 if live => ch.cur_dad,
            0x04 => ch.dad,
            0x08 if live => ch.remaining,
            0x08 => ch.tcnt,
            0x0C => ch.wcnt,
            0x10 => ch.bcnt,
            0x14 => ch.fdata,
            _ => ch.cnt,
        }
    }

    pub fn write(&mut self, offset: u32, value: u32, mask: u32) {
        let offset = offset & 0xFF;
        let merge = |old: u32, keep: u32| (old & !mask | value & mask) & keep;
        if offset == 0 {
            self.gcnt = merge(self.gcnt, 0x800F_0001);
            return;
        }
        let (index, reg) = ((offset - 4) / 0x1C, (offset - 4) % 0x1C);
        let Some(ch) = self.channels.get_mut(index as usize) else {
            return;
        };
        match reg {
            0x00 => {
                ch.sad = merge(ch.sad, !3);
                ch.cur_sad = ch.sad;
            }
            0x04 => {
                ch.dad = merge(ch.dad, !3);
                ch.cur_dad = ch.dad;
            }
            0x08 => ch.tcnt = merge(ch.tcnt, 0x0FFF_FFFF),
            0x0C => ch.wcnt = merge(ch.wcnt, 0x00FF_FFFF),
            0x10 => ch.bcnt = merge(ch.bcnt, 0x0003_FFFF),
            0x14 => ch.fdata = merge(ch.fdata, !0),
            _ => {
                let was = ch.cnt & ENABLE != 0;
                ch.cnt = merge(ch.cnt, 0xFF0F_FC1F);
                if !was && ch.cnt & ENABLE != 0 {
                    ch.remaining = ch.tcnt;
                    if ch.cnt & IMMEDIATE != 0 {
                        self.requests |= 1 << index;
                    }
                }
                if ch.cnt & ENABLE == 0 {
                    self.requests &= !(1 << index);
                }
            }
        }
    }

    /// A device asks for data to be moved: every enabled channel started by
    /// it gets a request.
    pub fn request(&mut self, device: Device) {
        for (i, ch) in self.channels.iter().enumerate() {
            let waiting = ch.cnt & (ENABLE | IMMEDIATE) == ENABLE;
            if waiting && ch.cnt >> 24 & 0xF == device as u32 {
                self.requests |= 1 << i;
            }
        }
    }

    pub fn pending(&self) -> bool {
        self.requests != 0
    }

    /// The next logical block to move, lowest channel first.
    pub fn next_block(&mut self) -> Option<Block> {
        let index = self.requests.trailing_zeros() as usize;
        let ch = self.channels.get(index)?;
        self.requests &= !(1 << index);
        let words = if ch.wcnt == 0 { 0x0100_0000 } else { ch.wcnt };
        Some(Block {
            channel: index,
            src: ch.cur_sad,
            dst: ch.cur_dad,
            src_update: Update::of(ch.cnt >> 13),
            dst_update: Update::of(ch.cnt >> 10),
            fill: ch.fdata,
            words,
        })
    }

    /// The bus moved `block`, ending at the addresses given. Returns whether
    /// the channel's interrupt fires.
    pub fn finish(&mut self, block: Block, src: u32, dst: u32) -> bool {
        let ch = &mut self.channels[block.channel];
        // The reload flags rewind an address at the end of a logical block.
        ch.cur_sad = if ch.cnt & 1 << 15 != 0 { ch.sad } else { src };
        ch.cur_dad = if ch.cnt & 1 << 12 != 0 { ch.dad } else { dst };
        let done = if ch.cnt & IMMEDIATE != 0 {
            true
        } else if ch.cnt & REPEAT != 0 {
            false
        } else {
            ch.remaining = ch.remaining.saturating_sub(block.words);
            ch.remaining == 0
        };
        if done {
            ch.cnt &= !ENABLE;
        }
        // In repeat mode the interrupt marks every logical block.
        (done || ch.cnt & REPEAT != 0) && ch.cnt & IRQ_ENABLE != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(channel: u32, reg: u32) -> u32 {
        4 + channel * 0x1C + reg
    }

    #[test]
    fn an_immediate_fill_is_one_block_and_clears_the_enable_bit() {
        let mut ndma = Ndma::default();
        ndma.write(reg(7, 0x04), 0x1812_EE00, !0);
        ndma.write(reg(7, 0x0C), 0x9600, !0);
        ndma.write(reg(7, 0x14), 0xF800_F800, !0);
        ndma.write(reg(7, 0x18), 0xD000_6000, !0);
        let block = ndma.next_block().unwrap();
        assert_eq!(block.channel, 7);
        assert_eq!((block.dst, block.words), (0x1812_EE00, 0x9600));
        assert_eq!(block.src_update, Update::Fill);
        assert_eq!(block.dst_update, Update::Increment);
        assert_eq!(block.fill, 0xF800_F800);
        assert!(ndma.read(reg(7, 0x18)) & ENABLE != 0);
        assert!(ndma.finish(block, 0, 0x1812_EE00 + 0x9600 * 4));
        assert_eq!(ndma.read(reg(7, 0x18)) & ENABLE, 0);
        assert!(!ndma.pending());
    }

    #[test]
    fn a_device_channel_runs_per_request_until_the_total_is_moved() {
        let mut ndma = Ndma::default();
        ndma.write(reg(2, 0x00), 0x1000_900C, !0);
        ndma.write(reg(2, 0x04), 0x0800_0000, !0);
        ndma.write(reg(2, 0x08), 8, !0);
        ndma.write(reg(2, 0x0C), 4, !0);
        // AES output FIFO, fixed source, no interrupt.
        ndma.write(reg(2, 0x18), ENABLE | 9 << 24 | 2 << 13, !0);
        assert!(!ndma.pending());
        ndma.request(Device::ShaIn);
        assert!(!ndma.pending());

        ndma.request(Device::AesOut);
        let first = ndma.next_block().unwrap();
        assert_eq!(
            (first.src, first.dst, first.words),
            (0x1000_900C, 0x0800_0000, 4)
        );
        assert!(!ndma.finish(first, 0x1000_900C, 0x0800_0010));
        assert!(ndma.read(reg(2, 0x18)) & ENABLE != 0);

        ndma.request(Device::AesOut);
        let second = ndma.next_block().unwrap();
        assert_eq!(second.dst, 0x0800_0010);
        ndma.finish(second, 0x1000_900C, 0x0800_0020);
        assert_eq!(ndma.read(reg(2, 0x18)) & ENABLE, 0);
        ndma.request(Device::AesOut);
        assert!(!ndma.pending());
    }

    #[test]
    fn readback_shows_the_working_addresses() {
        let mut ndma = Ndma::default();
        ndma.write(reg(0, 0x04), 0x2000_0000, !0);
        ndma.write(reg(0, 0x0C), 2, !0);
        ndma.write(reg(0, 0x18), ENABLE | IMMEDIATE | 3 << 13, !0);
        let block = ndma.next_block().unwrap();
        ndma.finish(block, 0, 0x2000_0008);
        assert_eq!(ndma.read(reg(0, 0x04)), 0x2000_0000);
        ndma.write(0, READBACK, !0);
        assert_eq!(ndma.read(reg(0, 0x04)), 0x2000_0008);
    }

    #[test]
    fn the_destination_reload_flag_rewinds_the_address() {
        let mut ndma = Ndma::default();
        ndma.write(reg(1, 0x04), 0x2000_0000, !0);
        ndma.write(reg(1, 0x0C), 1, !0);
        ndma.write(
            reg(1, 0x18),
            ENABLE | REPEAT | IRQ_ENABLE | 6 << 24 | 1 << 12,
            !0,
        );
        ndma.request(Device::Tmio1);
        let block = ndma.next_block().unwrap();
        assert!(ndma.finish(block, 4, 0x2000_0004));
        ndma.request(Device::Tmio1);
        assert_eq!(ndma.next_block().unwrap().dst, 0x2000_0000);
    }
}
