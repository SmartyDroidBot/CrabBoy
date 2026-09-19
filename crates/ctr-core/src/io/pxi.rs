//! PXI: the link between the two processors (3dbrew, "PXI Registers").
//!
//! Each side has the same four registers, at 0x10008000 for the ARM9 and
//! 0x10163000 for the ARM11: `SYNC` (+0) carries a byte each way and can
//! interrupt the other side, `CNT` (+4) controls the two 16-word FIFOs, and
//! `SEND` (+8) and `RECV` (+0xC) are their ends. The FIFO interrupts fire on
//! the edge, or when enabled while their condition already holds, as on the
//! DS.

use std::collections::VecDeque;

const FIFO_DEPTH: usize = 16;

/// An end of the link.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Arm9 = 0,
    Arm11 = 1,
}

impl Side {
    fn other(self) -> Side {
        match self {
            Side::Arm9 => Side::Arm11,
            Side::Arm11 => Side::Arm9,
        }
    }
}

/// Interrupts a register access raised, as a set per side.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Irqs {
    pub sync: [bool; 2],
    pub send_empty: [bool; 2],
    pub recv_not_empty: [bool; 2],
}

#[derive(Default)]
struct End {
    /// The byte this side presents to the other.
    sync_out: u8,
    sync_irq: bool,
    /// Words this side has sent and the other has not read yet.
    sent: VecDeque<u32>,
    send_irq: bool,
    recv_irq: bool,
    error: bool,
    enabled: bool,
    /// What an empty `RECV` reads: the last word received.
    last_received: u32,
}

#[derive(Default)]
pub struct Pxi {
    ends: [End; 2],
}

impl Pxi {
    pub fn new() -> Self {
        Pxi::default()
    }

    /// Read the word at `offset` as `side`.
    pub fn read(&mut self, side: Side, offset: u32, irqs: &mut Irqs) -> u32 {
        let (me, other) = (side as usize, side.other() as usize);
        match offset {
            0x0 => {
                let end = &self.ends[me];
                self.ends[other].sync_out as u32
                    | (end.sync_out as u32) << 8
                    | (end.sync_irq as u32) << 31
            }
            0x4 => {
                let end = &self.ends[me];
                let incoming = &self.ends[other].sent;
                end.sent.is_empty() as u32
                    | ((end.sent.len() == FIFO_DEPTH) as u32) << 1
                    | (end.send_irq as u32) << 2
                    | (incoming.is_empty() as u32) << 8
                    | ((incoming.len() == FIFO_DEPTH) as u32) << 9
                    | (end.recv_irq as u32) << 10
                    | (end.error as u32) << 14
                    | (end.enabled as u32) << 15
            }
            0xC => match self.ends[other].sent.pop_front() {
                Some(word) => {
                    self.ends[me].last_received = word;
                    if self.ends[other].sent.is_empty() && self.ends[other].send_irq {
                        irqs.send_empty[other] = true;
                    }
                    word
                }
                None => {
                    self.ends[me].error = true;
                    self.ends[me].last_received
                }
            },
            _ => 0,
        }
    }

    /// Write the byte lanes of `mask` of the word at `offset` as `side`.
    pub fn write(&mut self, side: Side, offset: u32, value: u32, mask: u32, irqs: &mut Irqs) {
        let (me, other) = (side as usize, side.other() as usize);
        match offset {
            0x0 => {
                if mask & 0xFF00 != 0 {
                    self.ends[me].sync_out = (value >> 8) as u8;
                }
                if mask >> 24 != 0 {
                    self.ends[me].sync_irq = value & 1 << 31 != 0;
                    // Bit 29 interrupts the ARM11 and bit 30 the ARM9.
                    let trigger = match side {
                        Side::Arm9 => 1 << 29,
                        Side::Arm11 => 1 << 30,
                    };
                    if value & trigger != 0 && self.ends[other].sync_irq {
                        irqs.sync[other] = true;
                    }
                }
            }
            0x4 => {
                if mask & 0xFF != 0 {
                    let had = self.ends[me].send_irq;
                    self.ends[me].send_irq = value & 1 << 2 != 0;
                    if value & 1 << 3 != 0 {
                        self.ends[me].sent.clear();
                    }
                    if !had && self.ends[me].send_irq && self.ends[me].sent.is_empty() {
                        irqs.send_empty[me] = true;
                    }
                }
                if mask & 0xFF00 != 0 {
                    let had = self.ends[me].recv_irq;
                    let end = &mut self.ends[me];
                    end.recv_irq = value & 1 << 10 != 0;
                    end.enabled = value & 1 << 15 != 0;
                    if value & 1 << 14 != 0 {
                        end.error = false;
                    }
                    let enabled_now = !had && end.recv_irq;
                    if enabled_now && !self.ends[other].sent.is_empty() {
                        irqs.recv_not_empty[me] = true;
                    }
                }
            }
            0x8 => {
                if self.ends[me].sent.len() == FIFO_DEPTH {
                    self.ends[me].error = true;
                    return;
                }
                let was_empty = self.ends[me].sent.is_empty();
                self.ends[me].sent.push_back(value);
                if was_empty && self.ends[other].recv_irq {
                    irqs.recv_not_empty[other] = true;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARM9: usize = 0;
    const ARM11: usize = 1;

    #[test]
    fn sync_bytes_cross_and_interrupt_the_other_side_when_it_allows() {
        let mut pxi = Pxi::new();
        let mut irqs = Irqs::default();
        pxi.write(Side::Arm9, 0, 0x2000_AB00, !0, &mut irqs);
        assert_eq!(irqs, Irqs::default(), "the ARM11 has not enabled it");
        assert_eq!(pxi.read(Side::Arm11, 0, &mut irqs) & 0xFF, 0xAB);
        assert_eq!(pxi.read(Side::Arm9, 0, &mut irqs), 0xAB00);

        pxi.write(Side::Arm11, 0, 0x8000_0000, 0xFF00_0000, &mut irqs);
        pxi.write(Side::Arm9, 0, 0x2000_0000, 0xFF00_0000, &mut irqs);
        assert!(irqs.sync[ARM11]);
        assert_eq!(
            pxi.read(Side::Arm9, 0, &mut irqs),
            0xAB00,
            "a byte write elsewhere keeps the sync byte"
        );

        let mut irqs = Irqs::default();
        pxi.write(Side::Arm9, 0, 0x8000_0000, 0xFF00_0000, &mut irqs);
        pxi.write(Side::Arm11, 0, 0x4000_0000, 0xFF00_0000, &mut irqs);
        assert!(irqs.sync[ARM9] && !irqs.sync[ARM11]);
    }

    #[test]
    fn words_arrive_in_order_with_the_status_bits_following() {
        let mut pxi = Pxi::new();
        let mut irqs = Irqs::default();
        assert_eq!(pxi.read(Side::Arm9, 4, &mut irqs), 0x0101, "both empty");
        pxi.write(Side::Arm9, 8, 1, !0, &mut irqs);
        pxi.write(Side::Arm9, 8, 2, !0, &mut irqs);
        assert_eq!(pxi.read(Side::Arm9, 4, &mut irqs), 0x0100);
        assert_eq!(pxi.read(Side::Arm11, 4, &mut irqs), 0x0001);
        assert_eq!(pxi.read(Side::Arm11, 0xC, &mut irqs), 1);
        assert_eq!(pxi.read(Side::Arm11, 0xC, &mut irqs), 2);
        assert_eq!(pxi.read(Side::Arm11, 4, &mut irqs), 0x0101);
    }

    #[test]
    fn fifo_interrupts_fire_on_the_edges() {
        let mut pxi = Pxi::new();
        let mut irqs = Irqs::default();
        pxi.write(Side::Arm11, 4, 1 << 10 | 1 << 15, !0, &mut irqs);
        pxi.write(Side::Arm9, 4, 1 << 2 | 1 << 15, !0, &mut irqs);
        assert!(irqs.send_empty[ARM9], "enabled while already empty");

        let mut irqs = Irqs::default();
        pxi.write(Side::Arm9, 8, 7, !0, &mut irqs);
        assert!(irqs.recv_not_empty[ARM11]);
        let mut irqs = Irqs::default();
        pxi.write(Side::Arm9, 8, 8, !0, &mut irqs);
        assert!(
            !irqs.recv_not_empty[ARM11],
            "only the first word interrupts"
        );
        pxi.read(Side::Arm11, 0xC, &mut irqs);
        assert!(!irqs.send_empty[ARM9]);
        pxi.read(Side::Arm11, 0xC, &mut irqs);
        assert!(irqs.send_empty[ARM9], "the last word left the FIFO");
    }

    #[test]
    fn overflow_and_underflow_set_the_error_flag() {
        let mut pxi = Pxi::new();
        let mut irqs = Irqs::default();
        assert_eq!(pxi.read(Side::Arm9, 0xC, &mut irqs), 0);
        assert_ne!(pxi.read(Side::Arm9, 4, &mut irqs) & 1 << 14, 0);
        pxi.write(Side::Arm9, 4, 1 << 14, !0, &mut irqs);
        assert_eq!(pxi.read(Side::Arm9, 4, &mut irqs) & 1 << 14, 0);

        for word in 0..17 {
            pxi.write(Side::Arm9, 8, word, !0, &mut irqs);
        }
        let cnt = pxi.read(Side::Arm9, 4, &mut irqs);
        assert_ne!(cnt & 1 << 1, 0, "full");
        assert_ne!(cnt & 1 << 14, 0, "the seventeenth word was dropped");
        pxi.write(Side::Arm9, 4, 1 << 3, 0xFF, &mut irqs);
        assert_ne!(pxi.read(Side::Arm9, 4, &mut irqs) & 1, 0, "flushed");
    }
}
