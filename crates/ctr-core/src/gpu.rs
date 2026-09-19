//! The GPU's register block at 0x10400000: the memory fill units, the two
//! display controllers, the transfer engine, and the internal registers with
//! their command lists (3dbrew, "GPU/External Registers").
//!
//! It stands apart from the rest of the I/O so that both ways of running the
//! console share it. On the low-level machine the ARM11 writes these
//! registers and gets interrupts from the GIC; in the high-level mode the
//! `gsp::Gpu` service writes the same registers on a game's behalf and turns
//! the same completions into entries of its interrupt queue. A write that
//! starts work returns a [`GpuJob`]: the work is done at once, and the caller
//! delivers the completion after the job's cost (see `docs/3ds/clocks.md`).

use crate::bus::PhysMem;
use crate::clock::{P3D_CYCLES_PER_WORD, PPF_CYCLES_PER_BYTE, PSC_FILL_CYCLES_PER_BYTE};
use crate::ctr::{BOTTOM_SCREEN, TOP_SCREEN};
use crate::io::pdc::Pdc;
use std::collections::BTreeMap;

/// PDC status bit of the vertical blank, in the framebuffer select register.
pub const PDC_STATUS_VBLANK: u32 = 1 << 17;
/// PDC control: the vertical blank interrupt is masked.
const PDC_MASK_VBLANK: u32 = 1 << 9;

/// The units that report completion, in the order of their ARM11 interrupts
/// (0x28 to 0x2D) and of the identifiers `gsp::Gpu` hands to applications.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum GpuIrq {
    Psc0,
    Psc1,
    Pdc0,
    Pdc1,
    Ppf,
    P3d,
}

/// Work a register write started: which unit, and how many ARM11 cycles
/// until it reports.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GpuJob {
    pub unit: GpuIrq,
    pub cost: u64,
}

pub struct GpuExt {
    /// Top screen, then bottom screen.
    pub pdc: [Pdc; 2],
    /// The command processor and the internal registers.
    pub pica: pica::command::Gpu,
    /// Registers that hold what was written, by offset.
    latched: BTreeMap<u32, u32>,
}

impl Default for GpuExt {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuExt {
    pub fn new() -> Self {
        GpuExt {
            pdc: [Pdc::new(TOP_SCREEN), Pdc::new(BOTTOM_SCREEN)],
            pica: pica::command::Gpu::new(),
            latched: BTreeMap::new(),
        }
    }

    fn latched(&self, offset: u32) -> u32 {
        self.latched.get(&offset).copied().unwrap_or(0)
    }

    fn latch(&mut self, offset: u32, value: u32, mask: u32) {
        let merged = self.latched(offset) & !mask | value & mask;
        self.latched.insert(offset, merged);
    }

    /// Read the word at `offset` from 0x10400000.
    pub fn read(&self, offset: u32) -> u32 {
        match offset {
            0x400..=0x4FF => self.pdc[0].read32(offset & 0xFFF),
            0x500..=0x5FF => self.pdc[1].read32(offset & 0xFFF),
            // The internal registers, four bytes to an ID.
            0x1000..=0x1BFF => self.pica.regs[(offset as usize - 0x1000) / 4],
            _ => self.latched(offset),
        }
    }

    /// Write the lanes of `mask` of the word at `offset` from 0x10400000.
    pub fn write(
        &mut self,
        offset: u32,
        value: u32,
        mask: u32,
        mem: &mut PhysMem,
    ) -> Option<GpuJob> {
        match offset {
            0x400..=0x5FF => {
                let pdc = &mut self.pdc[(offset >> 8 & 1) as usize];
                let reg = offset & 0xFF;
                let old = pdc.read32(reg);
                let written = old & !mask | value & mask;
                if reg == 0x78 {
                    // Status bits 16-18 clear when written as one.
                    const STATUS: u32 = 0x0007_0000;
                    let status = old & STATUS & !(value & mask);
                    pdc.write32(reg, written & !STATUS | status);
                } else {
                    pdc.write32(reg, written);
                }
                None
            }
            // Memory fill units PSC0 and PSC1.
            0x01C | 0x02C => {
                self.latch(offset, value, mask);
                let control = self.latched(offset);
                if control & 1 == 0 {
                    return None;
                }
                let unit = offset & !0xF;
                let start = self.latched(unit) << 3;
                let end = self.latched(unit + 4) << 3;
                let pattern = self.latched(unit + 8);
                fill(mem, start, end, pattern, control >> 8 & 3);
                // The memory is written at once; the unit stays busy for
                // the time the fill takes and then interrupts.
                self.latched.insert(offset, control & !2);
                let cost = end.saturating_sub(start) as u64 * PSC_FILL_CYCLES_PER_BYTE;
                Some(GpuJob {
                    unit: if offset == 0x02C {
                        GpuIrq::Psc1
                    } else {
                        GpuIrq::Psc0
                    },
                    cost: cost.max(1),
                })
            }
            // The transfer engine. Like a fill, the memory is written at
            // once and completion follows after the time the work takes.
            0xC18 => {
                self.latch(offset, value, mask);
                let control = self.latched(offset);
                if control & 1 == 0 {
                    return None;
                }
                let written = self.transfer(mem);
                self.latched.insert(offset, control & !(1 << 8));
                Some(GpuJob {
                    unit: GpuIrq::Ppf,
                    cost: (written as u64 * PPF_CYCLES_PER_BYTE).max(1),
                })
            }
            0x1000..=0x1BFF => {
                use pica::command::reg;
                let id = (offset - 0x1000) / 4;
                let bytes = (0..4)
                    .filter(|byte| mask >> (byte * 8) & 0xFF != 0)
                    .fold(0, |bytes, byte| bytes | 1 << byte);
                self.pica.write_register(id, value, bytes);
                match id {
                    // Written by hand, it interrupts like the end of a list.
                    reg::FINALIZE => Some(GpuJob {
                        unit: GpuIrq::P3d,
                        cost: 1,
                    }),
                    reg::CMDBUF_JUMP0 | 0x23D => {
                        self.run_commands((id - reg::CMDBUF_JUMP0) as usize, mem)
                    }
                    _ => None,
                }
            }
            _ => {
                self.latch(offset, value, mask);
                None
            }
        }
    }

    /// A job's time is up: the unit's busy bit clears and its finished bit
    /// sets. The caller delivers the interrupt.
    pub fn complete(&mut self, unit: GpuIrq) {
        match unit {
            GpuIrq::Psc0 | GpuIrq::Psc1 => {
                let offset = if unit == GpuIrq::Psc0 { 0x01C } else { 0x02C };
                let control = self.latched(offset);
                self.latched.insert(offset, control & !1 | 2);
            }
            GpuIrq::Ppf => {
                let control = self.latched(0xC18);
                self.latched.insert(0xC18, control & !1 | 1 << 8);
            }
            GpuIrq::P3d | GpuIrq::Pdc0 | GpuIrq::Pdc1 => {}
        }
    }

    /// The end of a frame: each enabled display controller sets its VBlank
    /// flag. Returns which of them interrupt (top, bottom).
    pub fn vblank(&mut self) -> [bool; 2] {
        let mut interrupts = [false; 2];
        for (pdc, interrupt) in self.pdc.iter_mut().zip(&mut interrupts) {
            let control = pdc.read32(0x74);
            if control & 1 == 0 {
                continue;
            }
            let status = pdc.read32(0x78);
            pdc.write32(0x78, status | PDC_STATUS_VBLANK);
            *interrupt = control & PDC_MASK_VBLANK == 0;
        }
        interrupts
    }

    /// Run the command list of buffer `index`, following jumps to the other
    /// buffer. The list takes effect at once; the P3D interrupt follows after
    /// a time in proportion to its length. A list that never writes
    /// `FINALIZE` hangs the GPU, so nothing follows it.
    fn run_commands(&mut self, mut index: usize, mem: &PhysMem) -> Option<GpuJob> {
        use pica::command::ListEnd;
        let mut words_run = 0u64;
        // Two buffers can jump to each other for ever.
        for _ in 0..64 {
            let (addr, len) = self.pica.command_buffer(index);
            let bytes = mem.slice(addr, len as usize)?;
            let words: Vec<u32> = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|word| u32::from_le_bytes(*word))
                .collect();
            words_run += words.len() as u64;
            match self.pica.run_list(&words) {
                ListEnd::Finalized => {
                    return Some(GpuJob {
                        unit: GpuIrq::P3d,
                        cost: (words_run * P3D_CYCLES_PER_WORD).max(1),
                    })
                }
                ListEnd::Jump(next) => index = next,
                ListEnd::Exhausted => return None,
            }
        }
        None
    }

    /// Run the transfer engine as its registers describe, returning the
    /// number of bytes it writes. A transfer that does not lie in memory, or
    /// whose settings do not exist, writes nothing and still completes.
    fn transfer(&mut self, mem: &mut PhysMem) -> usize {
        use pica::transfer::{DisplayTransfer, TextureCopy};
        let reg = |offset: u32| self.latched(0xC00 + offset);
        let (input, output) = (reg(0x00) << 3, reg(0x04) << 3);
        let flags = reg(0x10);
        let mut run = |input_len: usize, output_len: usize, go: &dyn Fn(&[u8], &mut [u8])| {
            let Some(source) = mem.slice(input, input_len).map(<[u8]>::to_vec) else {
                return 0;
            };
            match mem.slice_mut(output, output_len) {
                Some(target) => {
                    go(&source, target);
                    output_len
                }
                None => 0,
            }
        };
        if flags & 1 << 3 != 0 {
            let copy = TextureCopy::from_registers(reg(0x20), reg(0x24), reg(0x28));
            run(copy.input_len(), copy.output_len(), &|i, o| copy.run(i, o))
        } else {
            match DisplayTransfer::from_registers(reg(0x08), reg(0x0C), flags) {
                Some(t) => run(t.input_len(), t.output_len(), &|i, o| t.run(i, o)),
                None => 0,
            }
        }
    }
}

/// A PSC memory fill of `[start, end)` with a 16-, 24- or 32-bit pattern.
fn fill(mem: &mut PhysMem, start: u32, end: u32, pattern: u32, width: u32) {
    let Some(len) = end.checked_sub(start) else {
        return;
    };
    let Some(target) = mem.slice_mut(start, len as usize) else {
        return;
    };
    let bytes = pattern.to_le_bytes();
    let unit = match width {
        0 => 2,
        2 => 4,
        _ => 3,
    };
    for (n, byte) in target.iter_mut().enumerate() {
        *byte = bytes[n % unit];
    }
}
