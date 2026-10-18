//! The APBP mailboxes between the ARM and the DSP: three data registers and
//! a 16-bit semaphore in each direction (Teakra's `apbp.cpp`; 3dbrew, "DSP
//! Registers").

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct DataChannel {
    ready: bool,
    data: u16,
    disable_interrupt: u16,
}

/// One direction of the mailbox.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Apbp {
    channels: [DataChannel; 3],
    semaphore: u16,
    semaphore_mask: u16,
    semaphore_signal: bool,
}

impl Apbp {
    /// Write data register `channel`; whether the receiver is interrupted.
    pub fn send(&mut self, channel: usize, data: u16) -> bool {
        let c = &mut self.channels[channel];
        c.ready = true;
        c.data = data;
        c.disable_interrupt == 0
    }

    /// Read data register `channel`, emptying it.
    pub fn recv(&mut self, channel: usize) -> u16 {
        let c = &mut self.channels[channel];
        c.ready = false;
        c.data
    }

    pub fn peek(&self, channel: usize) -> u16 {
        self.channels[channel].data
    }

    pub fn is_ready(&self, channel: usize) -> bool {
        self.channels[channel].ready
    }

    pub fn disable_interrupt(&self, channel: usize) -> u16 {
        self.channels[channel].disable_interrupt
    }

    pub fn set_disable_interrupt(&mut self, channel: usize, value: u16) {
        self.channels[channel].disable_interrupt = value;
    }

    /// Set semaphore bits; whether the receiver is interrupted, as it is
    /// whenever an unmasked bit is set.
    pub fn set_semaphore(&mut self, bits: u16) -> bool {
        self.semaphore |= bits;
        let signal = self.semaphore & !self.semaphore_mask != 0;
        self.semaphore_signal |= signal;
        signal
    }

    pub fn clear_semaphore(&mut self, bits: u16) {
        self.semaphore &= !bits;
        self.semaphore_signal = self.semaphore & !self.semaphore_mask != 0;
    }

    pub fn semaphore(&self) -> u16 {
        self.semaphore
    }

    pub fn mask_semaphore(&mut self, bits: u16) {
        self.semaphore_mask = bits;
    }

    pub fn semaphore_mask(&self) -> u16 {
        self.semaphore_mask
    }

    pub fn semaphore_signaled(&self) -> bool {
        self.semaphore_signal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_waits_until_it_is_read() {
        let mut apbp = Apbp::default();
        assert!(!apbp.is_ready(1));
        assert!(apbp.send(1, 0x1234));
        assert!(apbp.is_ready(1));
        assert_eq!(apbp.peek(1), 0x1234);
        assert!(apbp.is_ready(1));
        assert_eq!(apbp.recv(1), 0x1234);
        assert!(!apbp.is_ready(1));
        apbp.set_disable_interrupt(2, 1);
        assert!(!apbp.send(2, 1));
    }

    #[test]
    fn masked_semaphore_bits_do_not_signal() {
        let mut apbp = Apbp::default();
        apbp.mask_semaphore(0x2000);
        assert!(!apbp.set_semaphore(0x2000));
        assert!(!apbp.semaphore_signaled());
        assert!(apbp.set_semaphore(0x8000));
        assert!(apbp.semaphore_signaled());
        assert_eq!(apbp.semaphore(), 0xA000);
        apbp.clear_semaphore(0x8000);
        assert!(!apbp.semaphore_signaled());
        assert_eq!(apbp.semaphore(), 0x2000);
    }
}
