//! The two DSP timers (Teakra's `timer.cpp`, `timer.md`).

/// No limit on how far time can be skipped.
pub const INFINITY: u64 = u64::MAX;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum CountMode {
    #[default]
    Single,
    AutoRestart,
    FreeRunning,
    EventCount,
}

impl CountMode {
    pub fn from_bits(bits: u16) -> Self {
        match bits {
            1 => CountMode::AutoRestart,
            2 => CountMode::FreeRunning,
            3 => CountMode::EventCount,
            // 0, and the undocumented 4-7.
            _ => CountMode::Single,
        }
    }

    pub fn bits(self) -> u16 {
        self as u16
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Timer {
    pub update_mmio: u16,
    pub pause: u16,
    pub count_mode: CountMode,
    /// The prescaler; only 1:1 (0) is modelled.
    pub scale: u16,
    pub start_high: u16,
    pub start_low: u16,
    pub counter: u32,
    pub counter_high: u16,
    pub counter_low: u16,
}

impl Timer {
    fn start(&self) -> u32 {
        (self.start_high as u32) << 16 | self.start_low as u32
    }

    pub fn restart(&mut self) {
        if self.count_mode != CountMode::FreeRunning {
            self.counter = self.start();
            self.update_mmio();
        }
    }

    /// One cycle; whether the timer fired.
    pub fn tick(&mut self) -> bool {
        if self.pause != 0 || self.count_mode == CountMode::EventCount {
            return false;
        }
        if self.counter == 0 {
            match self.count_mode {
                CountMode::AutoRestart => self.restart(),
                CountMode::FreeRunning => {
                    self.counter = 0xFFFF_FFFF;
                    self.update_mmio();
                }
                _ => {}
            }
            false
        } else {
            self.counter -= 1;
            self.update_mmio();
            self.counter == 0
        }
    }

    /// An event to count; whether the timer fired.
    pub fn tick_event(&mut self) -> bool {
        if self.pause != 0 || self.count_mode != CountMode::EventCount || self.counter == 0 {
            return false;
        }
        self.counter -= 1;
        self.update_mmio();
        self.counter == 0
    }

    fn update_mmio(&mut self) {
        if self.update_mmio != 0 {
            self.counter_high = (self.counter >> 16) as u16;
            self.counter_low = self.counter as u16;
        }
    }

    /// Cycles that can pass with nothing to see before the one that fires.
    pub fn max_skip(&self) -> u64 {
        if self.pause != 0 || self.count_mode == CountMode::EventCount {
            return INFINITY;
        }
        if self.counter == 0 {
            return match self.count_mode {
                CountMode::AutoRestart => self.start() as u64,
                CountMode::FreeRunning => 0xFFFF_FFFF,
                _ => INFINITY,
            };
        }
        self.counter as u64 - 1
    }

    pub fn skip(&mut self, ticks: u64) {
        if self.pause != 0 || self.count_mode == CountMode::EventCount || ticks == 0 {
            return;
        }
        if self.counter == 0 {
            let reset = match self.count_mode {
                CountMode::AutoRestart => self.start(),
                CountMode::FreeRunning => 0xFFFF_FFFF,
                _ => return,
            };
            self.counter = reset.wrapping_sub(ticks as u32 - 1);
        } else {
            self.counter = self.counter.wrapping_sub(ticks as u32);
        }
        self.update_mmio();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timer(mode: CountMode, start: u32) -> Timer {
        let mut t = Timer {
            count_mode: mode,
            update_mmio: 1,
            start_low: start as u16,
            start_high: (start >> 16) as u16,
            ..Timer::default()
        };
        t.restart();
        t
    }

    fn counter(t: &Timer) -> u32 {
        (t.counter_high as u32) << 16 | t.counter_low as u32
    }

    // Teakra's tests/timer.cpp.
    #[test]
    fn single_mode() {
        let mut t = timer(CountMode::Single, 5);
        assert_eq!((counter(&t), t.max_skip()), (5, 4));
        assert!(!t.tick() && !t.tick());
        assert_eq!((counter(&t), t.max_skip()), (3, 2));
        t.skip(2);
        assert_eq!((counter(&t), t.max_skip()), (1, 0));
        t.pause = 1;
        assert!(!t.tick());
        assert_eq!((counter(&t), t.max_skip()), (1, INFINITY));
        t.pause = 0;
        assert!(t.tick());
        assert_eq!((counter(&t), t.max_skip()), (0, INFINITY));
        assert!(!t.tick());
        assert_eq!((counter(&t), t.max_skip()), (0, INFINITY));
    }

    #[test]
    fn auto_restart_mode() {
        let mut t = timer(CountMode::AutoRestart, 0x1234_0005);
        assert_eq!((counter(&t), t.max_skip()), (0x1234_0005, 0x1234_0004));
        assert!(!t.tick() && !t.tick());
        assert_eq!((counter(&t), t.max_skip()), (0x1234_0003, 0x1234_0002));
        t.skip(0x1234_0002);
        assert_eq!((counter(&t), t.max_skip()), (1, 0));
        t.pause = 1;
        assert!(!t.tick());
        assert_eq!((counter(&t), t.max_skip()), (1, INFINITY));
        t.pause = 0;
        assert!(t.tick());
        assert_eq!((counter(&t), t.max_skip()), (0, 0x1234_0005));
        assert!(!t.tick());
        assert_eq!((counter(&t), t.max_skip()), (0x1234_0005, 0x1234_0004));
    }

    #[test]
    fn free_running_mode() {
        let mut t = timer(CountMode::Single, 0x1234_0005);
        t.count_mode = CountMode::FreeRunning;
        assert_eq!((counter(&t), t.max_skip()), (0x1234_0005, 0x1234_0004));
        // Restarting does nothing while free running.
        t.restart();
        assert_eq!((counter(&t), t.max_skip()), (0x1234_0005, 0x1234_0004));
        assert!(!t.tick() && !t.tick());
        assert_eq!((counter(&t), t.max_skip()), (0x1234_0003, 0x1234_0002));
        t.skip(0x1234_0002);
        assert_eq!((counter(&t), t.max_skip()), (1, 0));
        t.pause = 1;
        assert!(!t.tick());
        assert_eq!((counter(&t), t.max_skip()), (1, INFINITY));
        t.pause = 0;
        assert!(t.tick());
        assert_eq!((counter(&t), t.max_skip()), (0, 0xFFFF_FFFF));
        assert!(!t.tick());
        assert_eq!((counter(&t), t.max_skip()), (0xFFFF_FFFF, 0xFFFF_FFFE));
    }

    #[test]
    fn event_count_mode() {
        let mut t = timer(CountMode::EventCount, 5);
        assert_eq!((counter(&t), t.max_skip()), (5, INFINITY));
        assert!(!t.tick());
        assert_eq!(counter(&t), 5);
        assert!(!t.tick_event());
        assert_eq!(counter(&t), 4);
        t.pause = 1;
        assert!(!t.tick_event());
        assert_eq!(counter(&t), 4);
        t.pause = 0;
        assert!(!t.tick_event() && !t.tick_event() && !t.tick_event());
        assert_eq!((counter(&t), t.max_skip()), (1, INFINITY));
        assert!(t.tick_event());
        assert_eq!(counter(&t), 0);
        assert!(!t.tick_event());
        assert_eq!((counter(&t), t.max_skip()), (0, INFINITY));
    }
}
