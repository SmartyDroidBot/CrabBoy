//! The interrupt control unit: sixteen request lines routed to the core's
//! three interrupts and to vectored interrupts (Teakra's `icu.h`).

/// Interrupts the core has been sent and not yet taken.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Pending {
    pub int: [bool; 3],
    pub vectored: bool,
    pub vector: u32,
    pub context_switch: bool,
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Icu {
    request: u16,
    enabled: [u16; 3],
    vectored_enabled: u16,
    pub vector_low: [u16; 16],
    pub vector_high: [u16; 16],
    pub vector_context_switch: [u16; 16],
}

impl Icu {
    pub fn request(&self) -> u16 {
        self.request
    }

    pub fn acknowledge(&mut self, bits: u16) {
        self.request &= !bits;
    }

    /// Raise the request lines in `bits`, interrupting the core through
    /// every interrupt that has them enabled.
    pub fn trigger(&mut self, bits: u16, pending: &mut Pending) {
        self.request |= bits;
        for irq in 0..16 {
            if bits >> irq & 1 == 0 {
                continue;
            }
            for (interrupt, enabled) in self.enabled.iter().enumerate() {
                if enabled >> irq & 1 != 0 {
                    pending.int[interrupt] = true;
                }
            }
            if self.vectored_enabled >> irq & 1 != 0 {
                pending.vectored = true;
                pending.vector = self.vector(irq);
                pending.context_switch = self.vector_context_switch[irq] != 0;
            }
        }
    }

    pub fn trigger_single(&mut self, irq: u32, pending: &mut Pending) {
        self.trigger(1 << irq, pending);
    }

    pub fn enable(&self, interrupt: usize) -> u16 {
        self.enabled[interrupt]
    }

    pub fn set_enable(&mut self, interrupt: usize, bits: u16) {
        self.enabled[interrupt] = bits;
    }

    pub fn vectored_enable(&self) -> u16 {
        self.vectored_enabled
    }

    pub fn set_vectored_enable(&mut self, bits: u16) {
        self.vectored_enabled = bits;
    }

    fn vector(&self, irq: usize) -> u32 {
        self.vector_low[irq] as u32 | (self.vector_high[irq] as u32) << 16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_reaches_the_interrupts_that_enable_it() {
        let mut icu = Icu::default();
        let mut pending = Pending::default();
        icu.set_enable(1, 1 << 0xE);
        icu.set_vectored_enable(1 << 0xE);
        icu.vector_low[0xE] = 0x1234;
        icu.vector_high[0xE] = 1;
        icu.vector_context_switch[0xE] = 1;
        icu.trigger_single(0xA, &mut pending);
        assert_eq!(pending, Pending::default());
        icu.trigger_single(0xE, &mut pending);
        assert_eq!(pending.int, [false, true, false]);
        assert!(pending.vectored && pending.context_switch);
        assert_eq!(pending.vector, 0x1_1234);
        assert_eq!(icu.request(), 1 << 0xE | 1 << 0xA);
        icu.acknowledge(1 << 0xA);
        assert_eq!(icu.request(), 1 << 0xE);
    }
}
