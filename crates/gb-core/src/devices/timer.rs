pub struct Timer {
    pub div_counter: u16,
    tima_counter: u32,
}

impl Timer {
    pub fn new() -> Timer {
        Timer {
            div_counter: 0,
            tima_counter: 0,
        }
    }

    pub fn on_tima_write(&mut self) {
        self.tima_counter = 0;
    }

    pub fn on_tac_write(&mut self) {
        self.tima_counter = 0;
    }

    pub fn step(&mut self, cycles: u32, io: &mut [u8; 0x80]) {
        // DIV is a 16-bit counter incremented at 16384 Hz.
        self.div_counter = self.div_counter.wrapping_add(cycles as u16);
        io[0x04] = (self.div_counter >> 8) as u8;

        let tac = io[0x07];
        if tac & 0x04 != 0 {
            let frequency: u32 = match tac & 0x03 {
                0 => 1024,
                1 => 16,
                2 => 64,
                _ => 256,
            };
            self.tima_counter += cycles;
            while self.tima_counter >= frequency {
                self.tima_counter -= frequency;
                let tima = io[0x05].wrapping_add(1);
                if tima == 0 {
                    io[0x05] = io[0x06];
                    io[0x0F] |= 0x04;
                } else {
                    io[0x05] = tima;
                }
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