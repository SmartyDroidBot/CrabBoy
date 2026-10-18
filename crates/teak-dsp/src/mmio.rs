//! The DSP's peripherals and the 2 KB window of data memory they answer in
//! (Teakra's `mmio.cpp`; GBATEK, "DSi Teak I/O Ports"), and the memory
//! interface unit that places that window and pages data memory.

use crate::apbp::Apbp;
use crate::btdmp::Btdmp;
use crate::dma::{Ahbm, Dma};
use crate::icu::{Icu, Pending};
use crate::timer::{CountMode, Timer};
use crate::Bus;

/// The size of the MMIO window, in words.
pub const MMIO_SIZE: u16 = 0x800;
/// Where data memory starts in DSP RAM, and the size of one of its banks, in
/// words.
pub const DATA_MEMORY_OFFSET: u32 = 0x2_0000;
pub const DATA_MEMORY_BANK: u32 = 0x1_0000;

/// Something the DSP did that the ARM side hears of at once: a reply
/// register written (with its interrupt enabled), or a semaphore bit set
/// that is not masked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArmEvent {
    Data(u8),
    Semaphore,
}

/// The memory interface unit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Miu {
    pub x_page: u16,
    pub y_page: u16,
    pub z_page: u16,
    pub x_size: [u16; 2],
    pub y_size: [u16; 2],
    pub page_mode: u16,
    pub mmio_base: u16,
}

impl Default for Miu {
    fn default() -> Self {
        Miu {
            x_page: 0,
            y_page: 0,
            z_page: 0,
            x_size: [0x20; 2],
            y_size: [0x1E; 2],
            page_mode: 0,
            mmio_base: 0x8000,
        }
    }
}

impl Miu {
    pub fn in_mmio(&self, addr: u16) -> bool {
        addr >= self.mmio_base && (addr as u32) < self.mmio_base as u32 + MMIO_SIZE as u32
    }

    /// The register an address in the window reaches; the window mirrors.
    pub fn to_mmio(&self, addr: u16) -> u16 {
        addr.wrapping_sub(self.mmio_base) & (MMIO_SIZE - 1)
    }

    /// The word of DSP RAM a data address reaches.
    pub fn data_word(&self, addr: u16) -> u32 {
        let page = if self.page_mode == 0 {
            self.z_page
        } else if addr as u32 <= self.x_size[0] as u32 * 0x400 {
            self.x_page
        } else {
            self.y_page
        };
        DATA_MEMORY_OFFSET + addr as u32 + (page as u32 & 1) * DATA_MEMORY_BANK
    }
}

/// Every peripheral, the interrupts they have raised for the core, and
/// what the ARM side has yet to hear of.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Io {
    pub miu: Miu,
    pub icu: Icu,
    /// Data and semaphore from the ARM to the DSP, and back.
    pub apbp_from_cpu: Apbp,
    pub apbp_from_dsp: Apbp,
    pub timers: [Timer; 2],
    pub dma: Dma,
    pub ahbm: Ahbm,
    pub btdmp: [Btdmp; 2],
    pub pending: Pending,
    /// Samples from the first audio port, left and right.
    pub samples: Vec<[i16; 2]>,
    pub events: Vec<ArmEvent>,
    /// Registers with nothing behind them keep what was written.
    cells: Vec<u16>,
    /// Samples of the second audio port, which goes nowhere.
    discard: Vec<[i16; 2]>,
}

impl Default for Io {
    fn default() -> Self {
        Io {
            miu: Miu::default(),
            icu: Icu::default(),
            apbp_from_cpu: Apbp::default(),
            apbp_from_dsp: Apbp::default(),
            timers: Default::default(),
            dma: Dma::default(),
            ahbm: Ahbm::default(),
            btdmp: Default::default(),
            pending: Pending::default(),
            samples: Vec::new(),
            events: Vec::new(),
            cells: vec![0; MMIO_SIZE as usize],
            discard: Vec::new(),
        }
    }
}

/// Replace bits `pos..pos + len` of `value` with `field`.
fn put(value: u16, pos: u32, len: u32, field: u16) -> u16 {
    let mask = ((1u32 << len) - 1) as u16;
    value & !(mask << pos) | (field & mask) << pos
}

fn get(value: u16, pos: u32, len: u32) -> u16 {
    value >> pos & ((1u32 << len) - 1) as u16
}

impl Io {
    /// Request line `irq` of the interrupt unit.
    pub fn interrupt(&mut self, irq: u32) {
        self.icu.trigger_single(irq, &mut self.pending);
    }

    // ---- the ARM's side of the mailboxes ---------------------------------

    pub fn arm_send_data(&mut self, channel: usize, value: u16) {
        if self.apbp_from_cpu.send(channel, value) {
            self.interrupt(0xE);
        }
    }

    pub fn arm_set_semaphore(&mut self, bits: u16) {
        if self.apbp_from_cpu.set_semaphore(bits) {
            self.interrupt(0xE);
        }
    }

    // ---- time ----------------------------------------------------------

    /// One DSP cycle for the timers and audio ports.
    pub fn tick(&mut self) {
        for (i, irq) in [0xA, 0x9].into_iter().enumerate() {
            if self.timers[i].tick() {
                self.interrupt(irq);
            }
        }
        if self.btdmp[0].tick(&mut self.samples) {
            self.interrupt(0xB);
        }
        if self.btdmp[1].tick(&mut self.discard) {
            self.interrupt(0xB);
        }
        self.discard.clear();
    }

    /// Let up to `maximum` cycles pass while nothing would happen; how many
    /// did.
    pub fn skip(&mut self, maximum: u64) -> u64 {
        let ticks = self
            .timers
            .iter()
            .map(Timer::max_skip)
            .chain(self.btdmp.iter().map(Btdmp::max_skip))
            .fold(maximum, u64::min);
        for timer in &mut self.timers {
            timer.skip(ticks);
        }
        self.btdmp[0].skip(ticks, &mut self.samples);
        self.btdmp[1].skip(ticks, &mut self.discard);
        self.discard.clear();
        ticks
    }

    // ---- the registers ---------------------------------------------------

    /// Read register `addr` of the window (an offset in words).
    pub fn read(&mut self, addr: u16) -> u16 {
        let addr = addr & (MMIO_SIZE - 1);
        let cell = self.cells[addr as usize];
        match addr {
            // Chip detect.
            0x01A => 0xC902,

            // Timers: configuration, event, start and counter values.
            0x020 | 0x030 => {
                let t = &self.timers[(addr as usize - 0x20) / 0x10];
                let v = put(cell, 0, 2, t.scale);
                let v = put(v, 2, 3, t.count_mode.bits());
                let v = put(v, 8, 1, t.pause);
                let v = put(v, 9, 1, t.update_mmio);
                put(v, 10, 1, 0)
            }
            0x022 | 0x032 => 0,
            0x024 | 0x034 => self.timers[(addr as usize - 0x24) / 0x10].start_low,
            0x026 | 0x036 => self.timers[(addr as usize - 0x26) / 0x10].start_high,
            0x028 | 0x038 => self.timers[(addr as usize - 0x28) / 0x10].counter_low,
            0x02A | 0x03A => self.timers[(addr as usize - 0x2A) / 0x10].counter_high,

            // APBP: reply registers (the DSP's own, read back), command
            // registers (reading empties them), semaphores and status.
            0x0C0 | 0x0C4 | 0x0C8 => self.apbp_from_dsp.peek((addr as usize - 0xC0) / 4),
            0x0C2 | 0x0C6 | 0x0CA => self.apbp_from_cpu.recv((addr as usize - 0xC2) / 4),
            0x0CC => self.apbp_from_dsp.semaphore(),
            0x0CE => self.apbp_from_cpu.semaphore_mask(),
            0x0D0 => 0,
            0x0D2 => self.apbp_from_cpu.semaphore(),
            0x0D4 => {
                let c = &self.apbp_from_cpu;
                let v = put(cell, 8, 1, c.disable_interrupt(0));
                let v = put(v, 12, 1, c.disable_interrupt(1));
                put(v, 13, 1, c.disable_interrupt(2))
            }
            0x0D6 => {
                let (c, d) = (&self.apbp_from_cpu, &self.apbp_from_dsp);
                let v = put(cell, 5, 1, d.is_ready(0) as u16);
                let v = put(v, 6, 1, d.is_ready(1) as u16);
                let v = put(v, 7, 1, d.is_ready(2) as u16);
                let v = put(v, 8, 1, c.is_ready(0) as u16);
                let v = put(v, 9, 1, c.semaphore_signaled() as u16);
                let v = put(v, 12, 1, c.is_ready(1) as u16);
                put(v, 13, 1, c.is_ready(2) as u16)
            }
            // A mirror of the ARM's DSP_PSTS.
            0x0D8 => {
                let (c, d) = (&self.apbp_from_cpu, &self.apbp_from_dsp);
                let v = put(cell, 9, 1, c.semaphore_signaled() as u16);
                let v = put(v, 10, 1, d.is_ready(0) as u16);
                let v = put(v, 11, 1, d.is_ready(1) as u16);
                let v = put(v, 12, 1, d.is_ready(2) as u16);
                let v = put(v, 13, 1, c.is_ready(0) as u16);
                let v = put(v, 14, 1, c.is_ready(1) as u16);
                put(v, 15, 1, c.is_ready(2) as u16)
            }

            // AHBM.
            0x0E0 => self.ahbm.busy(),
            0x0E2 | 0x0E8 | 0x0EE => {
                let c = &self.ahbm.channels[(addr as usize - 0xE2) / 6];
                put(put(cell, 1, 2, c.burst_size), 4, 2, c.unit_size)
            }
            0x0E4 | 0x0EA | 0x0F0 => put(
                cell,
                8,
                1,
                self.ahbm.channels[(addr as usize - 0xE4) / 6].direction,
            ),
            0x0E6 | 0x0EC | 0x0F2 => self.ahbm.channels[(addr as usize - 0xE6) / 6].dma_channel,

            // MIU.
            0x10E => self.miu.x_page,
            0x110 => self.miu.y_page,
            0x112 => self.miu.z_page,
            0x114 | 0x116 => {
                let i = (addr as usize - 0x114) / 2;
                put(
                    put(cell, 0, 6, self.miu.x_size[i]),
                    8,
                    6,
                    self.miu.y_size[i],
                )
            }
            0x11A => put(cell, 6, 1, self.miu.page_mode),
            0x11E => self.miu.mmio_base,

            // DMA.
            0x184 => self.dma.enable_channel,
            0x18C => 0xFFFF,
            0x1BE => self.dma.active_channel(),
            0x1C0..=0x1DE => {
                let c = self.dma.active();
                match addr {
                    0x1C0 => c.addr_src_low,
                    0x1C2 => c.addr_src_high,
                    0x1C4 => c.addr_dst_low,
                    0x1C6 => c.addr_dst_high,
                    0x1C8 => c.size[0],
                    0x1CA => c.size[1],
                    0x1CC => c.size[2],
                    0x1CE => c.src_step[0],
                    0x1D0 => c.dst_step[0],
                    0x1D2 => c.src_step[1],
                    0x1D4 => c.dst_step[1],
                    0x1D6 => c.src_step[2],
                    0x1D8 => c.dst_step[2],
                    0x1DA => {
                        let v = put(cell, 0, 4, c.src_space);
                        put(put(v, 4, 4, c.dst_space), 10, 1, c.dword_mode)
                    }
                    0x1DC => c.y,
                    0x1DE => c.z,
                    _ => cell,
                }
            }

            // ICU.
            0x200 => self.icu.request(),
            0x202 | 0x204 => 0,
            0x206 | 0x208 | 0x20A => self.icu.enable((addr as usize - 0x206) / 2),
            0x20C => self.icu.vectored_enable(),
            0x212..=0x250 if addr & 3 == 2 => {
                let i = (addr as usize - 0x212) / 4;
                let v = put(cell, 0, 2, self.icu.vector_high[i]);
                put(v, 15, 1, self.icu.vector_context_switch[i])
            }
            0x214..=0x252 if addr & 3 == 0 => self.icu.vector_low[(addr as usize - 0x214) / 4],

            // BTDMP.
            0x2A2 | 0x322 => self.btdmp[(addr as usize - 0x2A2) / 0x80].transmit_clock_config,
            0x2BE | 0x33E => self.btdmp[(addr as usize - 0x2BE) / 0x80].transmit_enable,
            0x2C2 | 0x342 => {
                let b = &self.btdmp[(addr as usize - 0x2C2) / 0x80];
                put(
                    put(cell, 3, 1, b.transmit_full() as u16),
                    4,
                    1,
                    b.transmit_empty() as u16,
                )
            }
            0x2C6 | 0x346 => 0,
            0x2CA | 0x34A => 0,

            _ => cell,
        }
    }

    /// Write register `addr` of the window.
    pub fn write<B: Bus + ?Sized>(&mut self, bus: &mut B, addr: u16, value: u16) {
        let addr = addr & (MMIO_SIZE - 1);
        self.cells[addr as usize] = value;
        match addr {
            0x01A => {}

            0x020 | 0x030 => {
                let t = &mut self.timers[(addr as usize - 0x20) / 0x10];
                t.scale = get(value, 0, 2);
                t.count_mode = CountMode::from_bits(get(value, 2, 3));
                t.pause = get(value, 8, 1);
                t.update_mmio = get(value, 9, 1);
                if get(value, 10, 1) != 0 {
                    t.restart();
                }
            }
            0x022 | 0x032 => {
                if value != 0 && self.timers[(addr as usize - 0x22) / 0x10].tick_event() {
                    self.interrupt(if addr == 0x022 { 0xA } else { 0x9 });
                }
            }
            0x024 | 0x034 => self.timers[(addr as usize - 0x24) / 0x10].start_low = value,
            0x026 | 0x036 => self.timers[(addr as usize - 0x26) / 0x10].start_high = value,
            0x028 | 0x038 => self.timers[(addr as usize - 0x28) / 0x10].counter_low = value,
            0x02A | 0x03A => self.timers[(addr as usize - 0x2A) / 0x10].counter_high = value,

            0x0C0 | 0x0C4 | 0x0C8 => {
                let channel = (addr as usize - 0xC0) / 4;
                if self.apbp_from_dsp.send(channel, value) {
                    self.events.push(ArmEvent::Data(channel as u8));
                }
            }
            0x0C2 | 0x0C6 | 0x0CA => {}
            0x0CC => {
                if self.apbp_from_dsp.set_semaphore(value) {
                    self.events.push(ArmEvent::Semaphore);
                }
            }
            0x0CE => self.apbp_from_cpu.mask_semaphore(value),
            0x0D0 => self.apbp_from_cpu.clear_semaphore(value),
            0x0D2 => {}
            0x0D4 => {
                let c = &mut self.apbp_from_cpu;
                c.set_disable_interrupt(0, get(value, 8, 1));
                c.set_disable_interrupt(1, get(value, 12, 1));
                c.set_disable_interrupt(2, get(value, 13, 1));
            }

            0x0E0 => {}
            0x0E2 | 0x0E8 | 0x0EE => {
                let c = &mut self.ahbm.channels[(addr as usize - 0xE2) / 6];
                c.burst_size = get(value, 1, 2);
                c.unit_size = get(value, 4, 2);
            }
            0x0E4 | 0x0EA | 0x0F0 => {
                self.ahbm.channels[(addr as usize - 0xE4) / 6].direction = get(value, 8, 1)
            }
            0x0E6 | 0x0EC | 0x0F2 => {
                self.ahbm.channels[(addr as usize - 0xE6) / 6].dma_channel = value
            }

            0x10E => self.miu.x_page = value,
            0x110 => self.miu.y_page = value,
            0x112 => self.miu.z_page = value,
            0x114 | 0x116 => {
                let i = (addr as usize - 0x114) / 2;
                self.miu.x_size[i] = get(value, 0, 6);
                self.miu.y_size[i] = get(value, 8, 6);
            }
            0x11A => self.miu.page_mode = get(value, 6, 1),
            0x11E => self.miu.mmio_base = value,

            0x184 => self.dma.enable_channel = value,
            0x1BE => self.dma.set_active_channel(value),
            0x1C0..=0x1DE => {
                let c = self.dma.active();
                match addr {
                    0x1C0 => c.addr_src_low = value,
                    0x1C2 => c.addr_src_high = value,
                    0x1C4 => c.addr_dst_low = value,
                    0x1C6 => c.addr_dst_high = value,
                    0x1C8 => c.size[0] = value,
                    0x1CA => c.size[1] = value,
                    0x1CC => c.size[2] = value,
                    0x1CE => c.src_step[0] = value,
                    0x1D0 => c.dst_step[0] = value,
                    0x1D2 => c.src_step[1] = value,
                    0x1D4 => c.dst_step[1] = value,
                    0x1D6 => c.src_step[2] = value,
                    0x1D8 => c.dst_step[2] = value,
                    0x1DA => {
                        c.src_space = get(value, 0, 4);
                        c.dst_space = get(value, 4, 4);
                        c.dword_mode = get(value, 10, 1);
                    }
                    0x1DC => c.y = value,
                    0x1DE => {
                        c.z = value;
                        // This value starts the transfer.
                        if value == 0x40C0 {
                            let channel = self.dma.active_channel() as usize;
                            self.dma.run(channel, &mut self.ahbm, bus);
                            self.interrupt(0xF);
                        }
                    }
                    _ => {}
                }
            }

            0x200 => {}
            0x202 => self.icu.acknowledge(value),
            0x204 => self.icu.trigger(value, &mut self.pending),
            0x206 | 0x208 | 0x20A => self.icu.set_enable((addr as usize - 0x206) / 2, value),
            0x20C => self.icu.set_vectored_enable(value),
            0x212..=0x250 if addr & 3 == 2 => {
                let i = (addr as usize - 0x212) / 4;
                self.icu.vector_high[i] = get(value, 0, 2);
                self.icu.vector_context_switch[i] = get(value, 15, 1);
            }
            0x214..=0x252 if addr & 3 == 0 => {
                self.icu.vector_low[(addr as usize - 0x214) / 4] = value
            }

            0x2A2 | 0x322 => {
                self.btdmp[(addr as usize - 0x2A2) / 0x80].transmit_clock_config = value
            }
            0x2BE | 0x33E => self.btdmp[(addr as usize - 0x2BE) / 0x80].transmit_enable = value,
            0x2C6 | 0x346 => self.btdmp[(addr as usize - 0x2C6) / 0x80].send(value),
            0x2CA | 0x34A => self.btdmp[(addr as usize - 0x2CA) / 0x80].flush(),

            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ram;

    #[test]
    fn the_window_sits_where_the_miu_puts_it() {
        let mut miu = Miu::default();
        assert!(miu.in_mmio(0x8000) && miu.in_mmio(0x87FF) && !miu.in_mmio(0x8800));
        assert_eq!(miu.to_mmio(0x80C0), 0xC0);
        assert_eq!(miu.data_word(0x1234), 0x2_1234);
        miu.z_page = 1;
        assert_eq!(miu.data_word(0x1234), 0x3_1234);
        miu.page_mode = 1;
        miu.x_page = 0;
        miu.y_page = 1;
        assert_eq!(miu.data_word(0x8000), 0x2_8000);
        assert_eq!(miu.data_word(0x8001), 0x3_8001);
    }

    #[test]
    fn the_dsp_reply_registers_tell_the_arm() {
        let mut io = Io::default();
        let mut ram = vec![0u8; 0x8_0000];
        let bus = &mut Ram(&mut ram);
        io.write(bus, 0x0C4, 0x1234);
        assert_eq!(io.events, [ArmEvent::Data(1)]);
        assert!(io.apbp_from_dsp.is_ready(1));
        assert_eq!(io.read(0x0D6) & 0xE0, 0x40);
        assert_eq!(io.read(0x0D8) & 0x1C00, 0x0800);
        // A masked semaphore bit is set but not heard of.
        io.apbp_from_dsp.mask_semaphore(0x0001);
        io.write(bus, 0x0CC, 0x0001);
        assert_eq!(io.events.len(), 1);
        io.write(bus, 0x0CC, 0x8000);
        assert_eq!(io.events[1], ArmEvent::Semaphore);
        assert_eq!(io.read(0x0CC), 0x8001);
    }

    #[test]
    fn arm_commands_interrupt_through_the_icu() {
        let mut io = Io::default();
        let mut ram = vec![0u8; 0x8_0000];
        let bus = &mut Ram(&mut ram);
        io.write(bus, 0x206, 1 << 0xE);
        io.arm_send_data(2, 7);
        assert_eq!(io.pending.int, [true, false, false]);
        assert_eq!(io.read(0x0D6) >> 13 & 1, 1);
        assert_eq!(io.read(0x0CA), 7);
        assert_eq!(io.read(0x0D6) >> 13 & 1, 0);
    }

    #[test]
    fn a_timer_counts_down_to_its_interrupt() {
        let mut io = Io::default();
        let mut ram = vec![0u8; 0x8_0000];
        let bus = &mut Ram(&mut ram);
        io.write(bus, 0x206, 1 << 0xA);
        io.write(bus, 0x024, 3);
        // Single mode, MMIO updates, restart.
        io.write(bus, 0x020, 1 << 9 | 1 << 10);
        assert_eq!(io.read(0x028), 3);
        io.tick();
        io.tick();
        assert!(!io.pending.int[0]);
        io.tick();
        assert!(io.pending.int[0]);
        assert_eq!(io.read(0x028), 0);
    }
}
