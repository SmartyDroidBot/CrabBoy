//! GBA DMA channels (DMA0-3).
//!
//! Four DMA channels at I/O offsets 0xB0..0xDE, 12 bytes each: a 32-bit source
//! address, a 32-bit destination address, a 16-bit transfer count and a 16-bit
//! control word selecting the start timing, transfer unit (16/32-bit), repeat
//! mode and source/destination address adjustment.
//!
//! The registers are latched into internal pointers when a channel is enabled
//! (0 -> 1 transition of the enable bit); the transfer itself is performed by
//! `Bus::run_dma`, which the system root calls for immediate, VBlank and
//! HBlank timings. The sound FIFO (special timing on DMA1/2) is fed from the
//! timer overflow path.

/// Base I/O offset of each DMA channel.
const BASE: [usize; 4] = [0xB0, 0xBC, 0xC8, 0xD4];
/// Register block size of one channel.
const STRIDE: usize = 0xC;

/// Control-word bit flags (CNT_H).
const EN: u16 = 1 << 15;
const IRQ: u16 = 1 << 14;
const REPEAT: u16 = 1 << 9;

/// Start-timing values.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Timing {
    Immediate = 0,
    VBlank = 1,
    HBlank = 2,
    Special = 3,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Channel {
    /// Source address register (SAD).
    pub(crate) src: u32,
    /// Destination address register (DAD).
    pub(crate) dst: u32,
    /// Word count register (CNT_L).
    pub(crate) count: u32,
    /// Control register (CNT_H).
    pub(crate) control: u16,
    /// Internal source pointer, latched from `src` on enable.
    pub(crate) cur_src: u32,
    /// Internal destination pointer, latched from `dst` on enable and again
    /// after each repeat when the increment/reload mode is selected.
    pub(crate) cur_dst: u32,
    /// Internal transfer count, latched on enable and after each repeat.
    pub(crate) cur_count: u32,
    pub(crate) enabled: bool,
    /// Set once the transfer has been performed for the current trigger, so
    /// a repeating channel runs once per VBlank/HBlank event.
    pub(crate) done: bool,
}

impl Channel {
    fn new() -> Self {
        Channel {
            src: 0,
            dst: 0,
            count: 0,
            control: 0,
            cur_src: 0,
            cur_dst: 0,
            cur_count: 0,
            enabled: false,
            done: false,
        }
    }
    pub(crate) fn timing(&self) -> Timing {
        match (self.control >> 12) & 3 {
            1 => Timing::VBlank,
            2 => Timing::HBlank,
            3 => Timing::Special,
            _ => Timing::Immediate,
        }
    }
    pub(crate) fn unit_32(&self) -> bool {
        self.control & (1 << 10) != 0
    }
    pub(crate) fn dst_adjust(&self) -> u8 {
        ((self.control >> 5) & 3) as u8
    }
    pub(crate) fn src_adjust(&self) -> u8 {
        ((self.control >> 7) & 3) as u8
    }
    pub(crate) fn irq_enable(&self) -> bool {
        self.control & IRQ != 0
    }
    pub(crate) fn repeat(&self) -> bool {
        self.control & REPEAT != 0
    }

    /// Reload the internal pointers after a transfer of a repeating channel.
    pub(crate) fn reload_for_repeat(&mut self) {
        self.cur_count = self.count;
        if self.dst_adjust() == 3 {
            self.cur_dst = self.dst;
        }
    }
}

/// Address adjustment applied to a source/destination pointer after each unit.
/// Mode 3 (increment + reload) behaves as increment during the transfer.
pub(crate) fn adjust(addr: u32, unit: u32, mode: u8) -> u32 {
    match mode {
        1 => addr.wrapping_sub(unit),
        2 => addr,
        _ => addr.wrapping_add(unit),
    }
}

/// The four GBA DMA channels.
pub struct Dma {
    pub(crate) chans: [Channel; 4],
    /// Pending DMA IRQ flags (IF bits 8-11).
    pub(crate) flags: u16,
}

impl Default for Dma {
    fn default() -> Self {
        Dma {
            chans: [Channel::new(); 4],
            flags: 0,
        }
    }
}

impl Dma {
    pub fn new() -> Dma {
        Dma::default()
    }

    fn chan_index(offset: usize) -> Option<(usize, usize)> {
        BASE.iter()
            .enumerate()
            .find(|(_, base)| (**base..**base + STRIDE).contains(&offset))
            .map(|(i, base)| (i, offset - *base))
    }

    /// Source addresses are 27 bits on DMA0 (internal memory only) and 28
    /// bits elsewhere; destinations are 27 bits except on DMA3.
    fn src_mask(i: usize) -> u32 {
        if i == 0 {
            0x07FF_FFFF
        } else {
            0x0FFF_FFFF
        }
    }
    fn dst_mask(i: usize) -> u32 {
        if i == 3 {
            0x0FFF_FFFF
        } else {
            0x07FF_FFFF
        }
    }

    /// Write a 16-bit register in the DMA region.
    pub fn write16(&mut self, offset: usize, value: u16) {
        let Some((i, sub)) = Self::chan_index(offset) else {
            return;
        };
        let ch = &mut self.chans[i];
        match sub {
            0 => ch.src = (ch.src & 0xFFFF_0000) | value as u32,
            2 => ch.src = (ch.src & 0x0000_FFFF) | (value as u32) << 16,
            4 => ch.dst = (ch.dst & 0xFFFF_0000) | value as u32,
            6 => ch.dst = (ch.dst & 0x0000_FFFF) | (value as u32) << 16,
            8 => {
                // DMA3 counts up to 0x10000 units; the others up to 0x4000.
                ch.count = if i == 3 {
                    value as u32
                } else {
                    (value & 0x3FFF) as u32
                };
                if ch.count == 0 {
                    ch.count = if i == 3 { 0x10000 } else { 0x4000 };
                }
            }
            0xA => {
                let was_enabled = ch.enabled;
                ch.control = value;
                ch.enabled = value & EN != 0;
                if ch.enabled && !was_enabled {
                    ch.cur_src = ch.src & Self::src_mask(i);
                    ch.cur_dst = ch.dst & Self::dst_mask(i);
                    ch.cur_count = ch.count;
                    ch.done = false;
                }
            }
            _ => {}
        }
    }

    /// Read a 16-bit register in the DMA region (CNT_H only; the address and
    /// count registers are write-only and read as zero).
    pub fn read16(&self, offset: usize) -> u16 {
        match Self::chan_index(offset) {
            Some((i, 0xA)) => self.chans[i].control,
            _ => 0,
        }
    }

    /// Pending DMA IRQ flags (IF bits 8-11).
    pub fn irq_flags(&self) -> u16 {
        self.flags
    }

    /// Take the completion IRQ flags raised since the last call. The bus ORs
    /// them into IF once, so an acknowledged flag is not re-asserted.
    pub fn take_irq(&mut self) -> u16 {
        std::mem::take(&mut self.flags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_word_fields_follow_cnt_h_layout() {
        let mut dma = Dma::new();
        // Enable, repeat, 32-bit, HBlank timing, IRQ, dst reload, src fixed.
        dma.write16(
            0xD2,
            EN | IRQ | REPEAT | (1 << 10) | (2 << 12) | (3 << 5) | (2 << 7),
        );
        let ch = &dma.chans[2];
        assert!(ch.enabled && ch.repeat() && ch.unit_32() && ch.irq_enable());
        assert_eq!(ch.timing(), Timing::HBlank);
        assert_eq!(ch.dst_adjust(), 3);
        assert_eq!(ch.src_adjust(), 2);
        assert_eq!(dma.read16(0xD2), ch.control);
    }

    #[test]
    fn enable_edge_latches_masked_addresses_and_count() {
        let mut dma = Dma::new();
        dma.write16(0xB0, 0xFFFE);
        dma.write16(0xB2, 0xFFFF); // 0xFFFFFFFE -> masked to 27 bits on DMA0
        dma.write16(0xB4, 0x0000);
        dma.write16(0xB6, 0x0600);
        dma.write16(0xB8, 0);
        dma.write16(0xBA, EN);
        let ch = &dma.chans[0];
        assert_eq!(ch.cur_src, 0x07FF_FFFE);
        assert_eq!(ch.cur_dst, 0x0600_0000);
        assert_eq!(ch.cur_count, 0x4000, "count 0 means 0x4000 on DMA0-2");
        // Writing the registers while enabled does not disturb the pointers.
        dma.write16(0xB4, 0x1234);
        assert_eq!(dma.chans[0].cur_dst, 0x0600_0000);
        // DMA3: 16-bit count, 0 -> 0x10000.
        dma.write16(0xDC, 0);
        dma.write16(0xDE, EN);
        assert_eq!(dma.chans[3].cur_count, 0x10000);
    }
}
