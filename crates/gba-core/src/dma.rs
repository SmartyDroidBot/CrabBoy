//! GBA DMA channels (DMA0-3).
//!
//! Four DMA channels at I/O offsets 0xB0..0xDE. Each has a 32-bit source,
//! 32-bit destination, 16-bit transfer count, and a 16-bit control word
//! selecting the start timing, transfer unit (16/32-bit), and source/destination
//! address adjustment. Channels trigger on immediate, VBlank, or HBlank timing
//! (and special game-pak transfers via DMA3).
//!
//! [`Dma::run`] is called by the system root at the appropriate points in the
//! frame with the bus to perform any pending transfers.

use crate::bus::Bus;

/// Base I/O offset of each DMA channel.
const BASE: [usize; 4] = [0xB0, 0xBC, 0xC8, 0xD4];

/// Control-word bit flags (CNT_H).
const EN: u16 = 1 << 15;
const IRQ: u16 = 1 << 14;
const REPEAT: u16 = 1 << 13;

/// Start-timing values.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Timing {
    Immediate = 0,
    VBlank = 1,
    HBlank = 2,
    Special = 3,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Channel {
    pub(crate) src: u32,
    pub(crate) dst: u32,
    pub(crate) count: u16,
    pub(crate) control: u16,
    pub(crate) enabled: bool,
    /// Set once the transfer has been performed for a frame/event (used to
    /// implement repeat).
    pub(crate) done: bool,
}

impl Channel {
    fn new() -> Self {
        Channel {
            src: 0,
            dst: 0,
            count: 0,
            control: 0,
            enabled: false,
            done: false,
        }
    }
    pub(crate) fn timing(&self) -> Timing {
        match (self.control >> 11) & 3 {
            1 => Timing::VBlank,
            2 => Timing::HBlank,
            3 => Timing::Special,
            _ => Timing::Immediate,
        }
    }
    pub(crate) fn unit_32(&self) -> bool {
        ((self.control >> 9) & 3) == 1
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
}

/// Address adjustment applied to a source/destination pointer after a transfer.
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
    /// Pending DMA IRQ flags (bits 4-7).
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
        for (i, base) in BASE.iter().enumerate() {
            if offset >= *base && offset < *base + 0x10 {
                return Some((i, offset - *base));
            }
        }
        None
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
                ch.count = value & 0x3FFF;
                if ch.count == 0 {
                    ch.count = 0x4000;
                }
            }
            0xA => {
                ch.control = value;
                if value & EN != 0 {
                    ch.enabled = true;
                    ch.done = false;
                } else {
                    ch.enabled = false;
                }
            }
            _ => {}
        }
    }

    /// Pending DMA IRQ flags (bits 4-7).
    pub fn irq_flags(&self) -> u16 {
        self.flags
    }

    pub fn clear_irq(&mut self, mask: u16) {
        self.flags &= !(mask & 0x00F0);
    }

    /// Run any enabled channel whose start timing matches `timing`.
    pub fn run(&mut self, bus: &mut Bus, timing: Timing) {
        for i in 0..4 {
            let ch = &self.chans[i];
            if !ch.enabled || ch.done {
                continue;
            }
            if ch.timing() != timing {
                continue;
            }
            // Special transfers (game-pak / VRAM special) are DMA3-only and
            // left unimplemented; skip them.
            if timing == Timing::Special {
                continue;
            }
            let src = ch.src;
            let dst = ch.dst;
            let count = ch.count as usize;
            let unit = ch.unit_32();
            let unit_bytes: u32 = if unit { 4 } else { 2 };
            let src_adj = ch.src_adjust();
            let dst_adj = ch.dst_adjust();
            let irq = ch.irq_enable();
            let repeat = ch.repeat();

            let mut s = src;
            let mut d = dst;
            let n = count * unit_bytes as usize;
            if unit {
                for _ in 0..count {
                    let v = bus.read32(s);
                    bus.write32(d, v);
                    s = adjust(s, unit_bytes, src_adj);
                    d = adjust(d, unit_bytes, dst_adj);
                }
            } else {
                for _ in 0..count {
                    let v = bus.read16(s);
                    bus.write16(d, v);
                    s = adjust(s, unit_bytes, src_adj);
                    d = adjust(d, unit_bytes, dst_adj);
                }
            }
            let _ = n;
            // Update stored addresses unless fixed/src-reload modes reset them.
            let ch = &mut self.chans[i];
            if !ch.repeat() {
                ch.enabled = false;
            }
            if irq {
                self.flags |= 1 << (4 + i);
            }
            let _ = repeat;
            ch.done = !ch.enabled;
            ch.src = s;
            ch.dst = d;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediate_copy() {
        let mut bus = Bus::new(vec![0; 0x4000]);
        for i in 0..4 {
            bus.write16(0x0300_0000 + i * 2, 0x1000 + i);
        }
        let mut dma = Dma::new();
        dma.write16(0xB0, 0x0000);
        dma.write16(0xB2, 0x0300);
        dma.write16(0xB4, 0x1000);
        dma.write16(0xB6, 0x0300);
        dma.write16(0xB8, 4); // count = 4 halves
        dma.write16(0xBA, 0x8000); // enable, immediate, 16-bit
        dma.run(&mut bus, Timing::Immediate);
        for i in 0..4 {
            assert_eq!(bus.read16(0x0300_1000 + i * 2), 0x1000 + i);
        }
        // Non-repeating channel disables itself.
        assert!(!dma.chans[0].enabled);
    }

    #[test]
    fn timed_channel_waits() {
        let mut bus = Bus::new(vec![0; 0x4000]);
        let mut dma = Dma::new();
        dma.write16(0xB0, 0x0000);
        dma.write16(0xB2, 0x0300);
        dma.write16(0xB4, 0x1000);
        dma.write16(0xB6, 0x0300);
        dma.write16(0xB8, 2);
        dma.write16(0xBA, 0x8000 | (1 << 11)); // VBlank timing
        dma.run(&mut bus, Timing::Immediate);
        assert!(dma.chans[0].enabled); // not run yet
        dma.run(&mut bus, Timing::VBlank);
        assert!(!dma.chans[0].enabled);
    }
}
