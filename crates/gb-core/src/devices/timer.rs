/// Game Boy timer.
///
/// An internal 16-bit counter advances once per T-cycle. The DIV register is
/// the upper byte of that counter, so it changes every 256 T-cycles (16384 Hz).
/// Writing DIV resets the counter to 0. TIMA increments on the falling edge of
/// a bit of the counter selected by TAC: 00 -> bit 9 (4096 Hz),
/// 01 -> bit 3 (262144 Hz), 10 -> bit 5 (65536 Hz), 11 -> bit 7 (16384 Hz).
///
/// On overflow TIMA reads 0 for four T-cycles while a reload from TMA is
/// pending; a write to TMA during that window changes the value loaded, and a
/// write to TIMA cancels the pending reload.
pub struct Timer {
    /// 16-bit counter, advanced once per T-cycle.
    pub div_counter: u16,
    /// Monotonic absolute T-cycle count, used to time the reload deadline.
    pub(crate) abs_cycles: u64,
    /// Absolute cycle at which the pending TMA reload is applied.
    pub(crate) reload_deadline: u64,
    /// Value to load into TIMA when the pending reload fires.
    pub(crate) reload_value: u8,
    /// Whether a reload is pending (TIMA reads 0 and the reload window is open).
    pub(crate) reload_pending: bool,
}

impl Timer {
    pub fn new() -> Timer {
        Timer {
            div_counter: 0,
            abs_cycles: 0,
            reload_deadline: 0,
            reload_value: 0,
            reload_pending: false,
        }
    }

    /// Writing DIV (0xFF04) resets the counter. If the previously selected bit
    /// was 1, the reset to 0 is itself a falling edge, so TIMA is incremented.
    pub fn on_div_write(&mut self, io: &mut [u8; 0x80]) {
        let tac = io[0x07];
        let enabled = tac & 0x04 != 0;
        let bit: u8 = match tac & 0x03 {
            0 => 9,
            1 => 3,
            2 => 5,
            _ => 7,
        };
        if enabled && ((self.div_counter >> bit) & 1) == 1 {
            self.inc_tima(io, self.abs_cycles);
        }
        self.div_counter = 0;
    }

    /// Writing TIMA (0xFF05) cancels a pending reload.
    pub fn on_tima_write(&mut self) {
        self.reload_pending = false;
    }

    /// Writing TMA (0xFF06) while a reload is pending updates the loaded value.
    pub fn on_tma_write(&mut self, value: u8) {
        if self.reload_pending {
            self.reload_value = value;
        }
    }

    /// Writing TAC (0xFF07) only affects future edges, so nothing to do.
    pub fn on_tac_write(&mut self) {}

    fn inc_tima(&mut self, io: &mut [u8; 0x80], overflow_abs: u64) {
        let tima = io[0x05].wrapping_add(1);
        if tima == 0 {
            io[0x05] = 0;
            self.reload_value = io[0x06];
            self.reload_pending = true;
            self.reload_deadline = overflow_abs + 4;
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
        let mut t = self.abs_cycles;
        for _ in 0..cycles {
            if self.reload_pending && t >= self.reload_deadline {
                self.reload_pending = false;
                io[0x05] = self.reload_value;
            }
            let prev = self.div_counter;
            self.div_counter = self.div_counter.wrapping_add(1);
            if enabled && ((prev >> bit) & 1) == 1 && ((self.div_counter >> bit) & 1) == 0 {
                self.inc_tima(io, t);
            }
            t += 1;
        }
        if self.reload_pending && t >= self.reload_deadline {
            self.reload_pending = false;
            io[0x05] = self.reload_value;
        }
        io[0x04] = (self.div_counter >> 8) as u8;
        self.abs_cycles = t;
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
        io[0x07] = 0x04; // timer enabled, 4096 Hz (bit 9)
        io[0x06] = 0x42; // TMA
        io[0x05] = 0xFF; // TIMA about to overflow
        timer.div_counter = 1023; // bit 9 set; the next tick makes it fall
        io[0x0F] = 0;

        timer.step(1, &mut io); // counter -> 1024, bit 9 falls, TIMA overflows
        assert_eq!(io[0x05], 0x00, "TIMA reads 0 during the reload delay");
        assert_ne!(io[0x0F] & 0x04, 0, "timer interrupt requested on overflow");

        timer.step(4, &mut io); // reload delay elapses
        assert_eq!(io[0x05], 0x42, "TIMA reloaded from TMA");
    }

    #[test]
    fn div_register_is_upper_byte_of_per_cycle_counter() {
        let mut timer = Timer::new();
        let mut io = [0u8; 0x80];
        timer.step(255, &mut io);
        assert_eq!(io[0x04], 0, "DIV unchanged before counter crosses 256");
        timer.step(1, &mut io);
        assert_eq!(io[0x04], 1, "DIV is the upper byte of the 16-bit counter");
    }

    #[test]
    fn div_write_resets_counter() {
        let mut timer = Timer::new();
        let mut io = [0u8; 0x80];
        timer.step(600, &mut io); // DIV = 2
        assert_eq!(io[0x04], 2);
        timer.on_div_write(&mut io);
        timer.step(1, &mut io);
        assert_eq!(io[0x04], 0, "counter reset by DIV write");
        assert_eq!(timer.div_counter, 1);
    }

    #[test]
    fn tma_write_during_reload_window_is_loaded() {
        let mut timer = Timer::new();
        let mut io = [0u8; 0x80];
        io[0x07] = 0x04;
        io[0x05] = 0xFF;
        io[0x06] = 0x42;
        timer.div_counter = 1023;
        timer.step(1, &mut io); // overflow, reload 0x42 pending
        assert_eq!(io[0x05], 0x00);
        // Write TMA within the 4-cycle window: the new value should be loaded.
        timer.on_tma_write(0x77);
        timer.step(4, &mut io);
        assert_eq!(io[0x05], 0x77, "TMA written during reload is loaded");
    }
}
