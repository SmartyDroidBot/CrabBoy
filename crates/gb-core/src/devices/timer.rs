/// Game Boy timer.
///
/// An internal 16-bit counter advances once per T-cycle. The DIV register is
/// the upper byte of that counter, so it changes every 256 T-cycles (16384 Hz).
/// Writing DIV resets the counter to 0. TIMA increments on the falling edge of
/// a bit of the counter selected by TAC: 00 -> bit 9 (4096 Hz),
/// 01 -> bit 3 (262144 Hz), 10 -> bit 5 (65536 Hz), 11 -> bit 7 (16384 Hz).
///
/// On overflow TIMA reads 0 for one M-cycle while a reload from TMA is
/// pending; a write to TIMA in that cycle cancels the reload (and the
/// interrupt), a write in the cycle the reload happens is ignored, and a write
/// to TMA in either cycle is what gets loaded. Disabling the timer, or changing
/// its clock, while the selected counter bit is set counts as a falling edge
/// (the TAC glitch).
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
    /// Absolute cycle at which the last reload was applied.
    pub(crate) reloaded_at: u64,
}

impl Timer {
    pub fn new() -> Timer {
        Timer {
            div_counter: 0,
            abs_cycles: 0,
            reload_deadline: 0,
            reload_value: 0,
            reload_pending: false,
            reloaded_at: u64::MAX,
        }
    }

    fn selected_bit(tac: u8) -> u8 {
        match tac & 0x03 {
            0 => 9,
            1 => 3,
            2 => 5,
            _ => 7,
        }
    }

    /// The signal feeding TIMA's increment: the selected counter bit ANDed
    /// with the enable bit.
    fn signal(&self, tac: u8) -> bool {
        tac & 0x04 != 0 && (self.div_counter >> Self::selected_bit(tac)) & 1 == 1
    }

    /// Whether the current CPU access falls in the M-cycle that applied the
    /// reload (accesses land at the end of their cycle).
    fn in_reload_cycle(&self) -> bool {
        self.abs_cycles >= self.reloaded_at && self.abs_cycles < self.reloaded_at.wrapping_add(4)
    }

    /// Writing DIV (0xFF04) resets the counter. If the previously selected bit
    /// was 1, the reset to 0 is itself a falling edge, so TIMA is incremented.
    pub fn on_div_write(&mut self, io: &mut [u8; 0x80]) {
        if self.signal(io[0x07]) {
            self.inc_tima(io, self.abs_cycles);
        }
        self.div_counter = 0;
        io[0x04] = 0;
    }

    /// Write TIMA (0xFF05): cancels a pending reload, is ignored in the cycle
    /// the reload happens, and simply stores the value otherwise.
    pub fn write_tima(&mut self, io: &mut [u8; 0x80], value: u8) {
        if self.reload_pending {
            self.reload_pending = false;
            io[0x05] = value;
        } else if self.in_reload_cycle() {
            // TMA won this cycle.
        } else {
            io[0x05] = value;
        }
    }

    /// Write TMA (0xFF06): a pending or just-applied reload takes the new
    /// value.
    pub fn write_tma(&mut self, io: &mut [u8; 0x80], value: u8) {
        io[0x06] = value;
        if self.reload_pending {
            self.reload_value = value;
        } else if self.in_reload_cycle() {
            io[0x05] = value;
        }
    }

    /// Write TAC (0xFF07): if the increment signal falls because the timer is
    /// disabled or re-clocked, TIMA increments (the DMG "TAC glitch").
    pub fn write_tac(&mut self, io: &mut [u8; 0x80], value: u8) {
        let before = self.signal(io[0x07]);
        io[0x07] = value | 0xF8;
        if before && !self.signal(io[0x07]) {
            self.inc_tima(io, self.abs_cycles);
        }
    }

    fn inc_tima(&mut self, io: &mut [u8; 0x80], overflow_abs: u64) {
        let tima = io[0x05].wrapping_add(1);
        if tima == 0 {
            io[0x05] = 0;
            self.reload_value = io[0x06];
            self.reload_pending = true;
            self.reload_deadline = overflow_abs + 4;
        } else {
            io[0x05] = tima;
        }
    }

    /// Apply a due reload: TIMA takes TMA and the interrupt is requested.
    fn apply_reload(&mut self, io: &mut [u8; 0x80]) {
        self.reload_pending = false;
        self.reloaded_at = self.reload_deadline;
        io[0x05] = self.reload_value;
        io[0x0F] |= 0x04;
    }

    pub fn step(&mut self, cycles: u32, io: &mut [u8; 0x80]) {
        let tac = io[0x07];
        let enabled = tac & 0x04 != 0;
        let bit = Self::selected_bit(tac);
        let mut t = self.abs_cycles;
        for _ in 0..cycles {
            if self.reload_pending && t >= self.reload_deadline {
                self.apply_reload(io);
            }
            let prev = self.div_counter;
            self.div_counter = self.div_counter.wrapping_add(1);
            if enabled && ((prev >> bit) & 1) == 1 && ((self.div_counter >> bit) & 1) == 0 {
                self.inc_tima(io, t);
            }
            t += 1;
        }
        if self.reload_pending && t >= self.reload_deadline {
            self.apply_reload(io);
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
        assert_eq!(io[0x0F] & 0x04, 0, "the interrupt comes with the reload");

        timer.step(4, &mut io); // reload delay elapses
        assert_eq!(io[0x05], 0x42, "TIMA reloaded from TMA");
        assert_ne!(io[0x0F] & 0x04, 0, "timer interrupt requested on reload");
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
        timer.write_tma(&mut io, 0x77);
        timer.step(4, &mut io);
        assert_eq!(io[0x05], 0x77, "TMA written during reload is loaded");
    }

    #[test]
    fn tima_write_in_the_pending_cycle_cancels_and_in_the_reload_cycle_loses() {
        let mut timer = Timer::new();
        let mut io = [0u8; 0x80];
        io[0x07] = 0x04;
        io[0x06] = 0x42;
        io[0x05] = 0xFF;
        timer.div_counter = 1020;
        timer.step(4, &mut io); // overflow at the end of this cycle; reload pending
        timer.write_tima(&mut io, 0x7F);
        timer.step(4, &mut io);
        assert_eq!(io[0x05], 0x7F, "the write cancelled the reload");
        assert_eq!(io[0x0F] & 0x04, 0, "no interrupt when cancelled");

        let mut timer = Timer::new();
        io = [0u8; 0x80];
        io[0x07] = 0x04;
        io[0x06] = 0x42;
        io[0x05] = 0xFF;
        timer.div_counter = 1020;
        timer.step(4, &mut io); // overflow
        timer.step(4, &mut io); // reload applied during this cycle
        timer.write_tima(&mut io, 0x7F); // same cycle: ignored
        assert_eq!(io[0x05], 0x42);
        timer.step(4, &mut io);
        timer.write_tima(&mut io, 0x7F);
        assert_eq!(io[0x05], 0x7F, "a later write is normal");
    }

    #[test]
    fn disabling_the_timer_while_the_selected_bit_is_set_increments_tima() {
        let mut timer = Timer::new();
        let mut io = [0u8; 0x80];
        io[0x07] = 0x04; // enabled, bit 9
        io[0x05] = 0x10;
        timer.div_counter = 1 << 9;
        timer.write_tac(&mut io, 0x00);
        assert_eq!(io[0x05], 0x11, "falling edge from the enable bit");
        timer.write_tac(&mut io, 0x04);
        assert_eq!(io[0x05], 0x11, "enabling is a rising edge");
        timer.write_tac(&mut io, 0x05); // switch to bit 3 (clear): falls again
        assert_eq!(io[0x05], 0x12);
    }
}
