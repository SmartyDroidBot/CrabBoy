/// Game Boy timer.
///
/// The DIV register is a 16-bit counter incremented at 16384 Hz (every 256
/// T-cycles). TIMA increments on the falling edge of a bit of DIV selected by
/// TAC: 00 -> bit 9 (4096 Hz), 01 -> bit 3 (262144 Hz), 10 -> bit 5 (65536 Hz),
/// 11 -> bit 7 (16384 Hz). On overflow, TIMA is reloaded from TMA after a
/// one-M-cycle delay, during which it reads 0 and a write to TMA is applied to
/// the pending reload.
pub struct Timer {
    pub div_counter: u16,
    reload_pending: u32,
    reload_value: u8,
}

const CYCLES_PER_DIV: u32 = 256;

impl Timer {
    pub fn new() -> Timer {
        Timer {
            div_counter: 0,
            reload_pending: 0,
            reload_value: 0,
        }
    }

    pub fn on_tima_write(&mut self) {
        // Writing TIMA cancels a pending reload.
        self.reload_pending = 0;
    }

    pub fn on_tac_write(&mut self) {}

    fn inc_tima(&mut self, io: &mut [u8; 0x80]) {
        let tima = io[0x05].wrapping_add(1);
        if tima == 0 {
            io[0x05] = 0;
            self.reload_value = io[0x06];
            self.reload_pending = 4; // one M-cycle before TMA is copied in
            io[0x0F] |= 0x04;
        } else {
            io[0x05] = tima;
        }
    }

    pub fn step(&mut self, cycles: u32, io: &mut [u8; 0x80]) {
        let tac = io[0x07];
        let enabled = tac & 0x04 != 0;
        let bit: u8 = match tac & 0x03 {
            0 => 9,
            1 => 3,
            2 => 5,
            _ => 7,
        };

        let mut div = self.div_counter;
        let mut ticks = cycles / CYCLES_PER_DIV;
        let rem = cycles % CYCLES_PER_DIV;

        while ticks > 0 {
            ticks -= 1;
            let prev = div;
            div = div.wrapping_add(1);
            if enabled {
                if ((prev >> bit) & 1) == 1 && ((div >> bit) & 1) == 0 {
                    self.inc_tima(io);
                }
            }
        }
        self.div_counter = div;
        io[0x04] = (div >> 8) as u8;

        // Finish any pending TMA reload once its delay elapses.
        if self.reload_pending > 0 {
            if rem >= self.reload_pending {
                self.reload_pending = 0;
                io[0x05] = self.reload_value;
            } else {
                self.reload_pending -= rem;
            }
        }
    }
}

impl Default for Timer {
    fn default() -> Self {
        Self::new()
    }
}

impl emu_core::device::Device for Timer {
    fn kind(&self) -> &'static str {
        "Timer"
    }

    fn reset(&mut self) {
        *self = Timer::new();
    }

    fn tick(&mut self, cycles: u32, bus: &mut dyn emu_core::bus::Bus) {
        if let Some(gb) = bus.as_any_mut().downcast_mut::<crate::bus::Bus>() {
            self.step(cycles, &mut gb.io);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tima_overflow_reloads_from_tma_after_delay() {
        let mut timer = Timer::new();
        let mut io = [0u8; 0x80];
        io[0x07] = 0x04; // timer enabled, 4096 Hz (DIV bit 9)
        io[0x06] = 0x42; // TMA
        io[0x05] = 0xFF; // TIMA about to overflow
        timer.div_counter = 1023; // bit 9 set; the next tick makes it fall
        io[0x0F] = 0;

        timer.step(256, &mut io); // DIV -> 1024, TIMA overflows
        assert_eq!(io[0x05], 0x00, "TIMA reads 0 during the one-M-cycle reload delay");
        assert_ne!(io[0x0F] & 0x04, 0, "timer interrupt requested on overflow");

        timer.step(4, &mut io); // reload delay elapses
        assert_eq!(io[0x05], 0x42, "TIMA reloaded from TMA");
    }
}