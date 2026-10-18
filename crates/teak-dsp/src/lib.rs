//! The CEVA TeakLite II DSP of the 3DS.
//!
//! A Rust port of [Teakra](https://github.com/wwylele/teakra), copyright
//! (c) 2018 Weiyi Wang, MIT licence (the text is in `LICENSE.teakra`): the
//! only public low-level model of the DSP. It covers the instruction set,
//! the register file, the interrupt control unit, the timers, the APBP
//! mailboxes to the ARM, the DMA engine with its AHBM bridge to the ARM's
//! memory, the BTDMP audio ports and the memory interface unit. Behaviour
//! Teakra leaves unimplemented stops the core with a fault.
//!
//! The DSP has 512 KB of RAM (3dbrew, "DSP Memory Region"): program memory
//! in the first 256 KB, data memory in the second, both addressed in 16-bit
//! words. The RAM is the caller's, reached through a [`Bus`], so that the
//! ARM side can share it; so is the ARM memory DMA reaches. Everything is
//! integer arithmetic and runs the same on every host.

mod apbp;
mod btdmp;
pub mod decoder;
mod dma;
mod icu;
mod interpreter;
mod mmio;
pub mod operand;
mod register;
mod timer;

pub use mmio::ArmEvent;
pub use register::Regs;

use interpreter::Interp;
use mmio::Io;

/// The size of DSP RAM.
pub const RAM_LEN: usize = 0x8_0000;
/// DSP cycles between two audio samples: the codec's 32,728 Hz at the DSP's
/// 134,055,928 Hz.
pub const SAMPLE_PERIOD: u64 = 4096;

/// What the DSP reaches outside itself: its 512 KB of RAM, and the ARM's
/// memory through AHBM, by physical address. Memory that is not there
/// reads as zero.
pub trait Bus {
    /// DSP RAM, [`RAM_LEN`] bytes, 16-bit words little-endian.
    fn ram(&mut self) -> &mut [u8];

    fn ahb_read8(&mut self, _addr: u32) -> u8 {
        0
    }
    fn ahb_write8(&mut self, _addr: u32, _value: u8) {}

    fn ahb_read16(&mut self, addr: u32) -> u16 {
        u16::from_le_bytes([self.ahb_read8(addr), self.ahb_read8(addr.wrapping_add(1))])
    }
    fn ahb_write16(&mut self, addr: u32, value: u16) {
        let [l, h] = value.to_le_bytes();
        self.ahb_write8(addr, l);
        self.ahb_write8(addr.wrapping_add(1), h);
    }

    fn ahb_read32(&mut self, addr: u32) -> u32 {
        let l = self.ahb_read16(addr) as u32;
        let h = self.ahb_read16(addr.wrapping_add(2)) as u32;
        l | h << 16
    }
    fn ahb_write32(&mut self, addr: u32, value: u32) {
        self.ahb_write16(addr, value as u16);
        self.ahb_write16(addr.wrapping_add(2), (value >> 16) as u16);
    }
}

/// DSP RAM and nothing else.
pub struct Ram<'a>(pub &'a mut [u8]);

impl Bus for Ram<'_> {
    fn ram(&mut self) -> &mut [u8] {
        self.0
    }
}

/// The whole DSP: the core and its peripherals. The RAM is not here; each
/// call that needs it is given it.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Teak {
    regs: Regs,
    io: Io,
    fault: Option<String>,
}

impl Teak {
    pub fn new() -> Self {
        Teak::default()
    }

    /// The state after reset: registers, peripherals and RAM cleared.
    pub fn reset(&mut self, ram: &mut [u8]) {
        *self = Teak::default();
        ram.fill(0);
    }

    pub fn regs(&self) -> &Regs {
        &self.regs
    }

    pub fn regs_mut(&mut self) -> &mut Regs {
        &mut self.regs
    }

    /// Why the core stopped, if it did: an undefined or unmodelled
    /// instruction.
    pub fn fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }

    /// Run up to `cycles` DSP cycles. It returns early, after the
    /// instruction that caused it, once there is an [`ArmEvent`] to take
    /// with [`Teak::take_events`], or when the core faults. Returns the
    /// cycles that passed.
    pub fn run<B: Bus + ?Sized>(&mut self, bus: &mut B, cycles: u64) -> u64 {
        if self.fault.is_some() || cycles == 0 {
            return 0;
        }
        if bus.ram().len() != RAM_LEN {
            self.fault = Some(format!("DSP RAM of {} bytes", bus.ram().len()));
            return 0;
        }
        Interp::new(&mut self.regs, &mut self.io, bus, &mut self.fault).run(cycles)
    }

    /// What the DSP has done that the ARM side should hear of, in order.
    pub fn take_events(&mut self) -> Vec<ArmEvent> {
        std::mem::take(&mut self.io.events)
    }

    /// The samples of the audio output so far, left and right.
    pub fn take_samples(&mut self) -> Vec<[i16; 2]> {
        std::mem::take(&mut self.io.samples)
    }

    // ---- the ARM's side of the mailboxes (3dbrew, "DSP Registers") -------

    /// Whether command register `channel` has been read by the DSP.
    pub fn send_data_is_empty(&self, channel: usize) -> bool {
        !self.io.apbp_from_cpu.is_ready(channel)
    }

    pub fn send_data(&mut self, channel: usize, value: u16) {
        self.io.arm_send_data(channel, value);
    }

    pub fn recv_data_is_ready(&self, channel: usize) -> bool {
        self.io.apbp_from_dsp.is_ready(channel)
    }

    pub fn recv_data(&mut self, channel: usize) -> u16 {
        self.io.apbp_from_dsp.recv(channel)
    }

    pub fn peek_recv_data(&self, channel: usize) -> u16 {
        self.io.apbp_from_dsp.peek(channel)
    }

    /// Set bits of the semaphore the DSP reads.
    pub fn set_semaphore(&mut self, bits: u16) {
        self.io.arm_set_semaphore(bits);
    }

    /// The semaphore the DSP sets for the ARM.
    pub fn semaphore(&self) -> u16 {
        self.io.apbp_from_dsp.semaphore()
    }

    pub fn clear_semaphore(&mut self, bits: u16) {
        self.io.apbp_from_dsp.clear_semaphore(bits);
    }

    pub fn mask_semaphore(&mut self, bits: u16) {
        self.io.apbp_from_dsp.mask_semaphore(bits);
    }
}

#[cfg(test)]
mod tests;
