//! GBA timers (TM0-3).
//!
//! Four 16-bit timers at I/O offsets 0x100..0x110. TM0-2 count up and may be
//! cascaded (clocked by the previous timer's overflow); TM3 can count up as a
//! normal timer or serve as a prescaler counter for TM0. On overflow a timer
//! raises its IRQ (flags 0-3) and reloads from its reload value.
//!
//! `step` is called with CPU cycles from the system root.

/// Prescaler dividers for the two-bit prescaler field.
const DIVIDERS: [u32; 4] = [1, 64, 256, 1024];

/// IRQ flags for each timer overflow (IF bits 3-6).
pub const IRQ: [u16; 4] = [1 << 3, 1 << 4, 1 << 5, 1 << 6];

#[derive(Clone, Copy, Debug)]
struct Timer {
    counter: u16,
    reload: u16,
    prescaler: u8,
    cascade: bool,
    irq_enable: bool,
    enabled: bool,
    ticks: u32,
}

impl Timer {
    fn new() -> Self {
        Timer {
            counter: 0,
            reload: 0,
            prescaler: 0,
            cascade: false,
            irq_enable: false,
            enabled: false,
            ticks: 0,
        }
    }
}

/// The four GBA timers.
pub struct Timers {
    t: [Timer; 4],
    /// Pending overflow IRQ flags (bits 0-3).
    flags: u16,
    /// Which timers overflowed during the most recent `step`.
    overflowed: [bool; 4],
}

impl Default for Timers {
    fn default() -> Self {
        Timers {
            t: [Timer::new(); 4],
            flags: 0,
            overflowed: [false; 4],
        }
    }
}

impl Timers {
    pub fn new() -> Timers {
        Timers::default()
    }

    /// Whether timer `i` overflowed during the most recent `step`.
    pub fn just_overflowed(&self, i: usize) -> bool {
        self.overflowed[i]
    }

    /// Write to the low 16-bit reload register of timer `idx`. This only
    /// sets the reload value; the counter picks it up on the next overflow or
    /// when the timer is (re-)enabled.
    pub fn write_cnt_l(&mut self, idx: usize, value: u16) {
        self.t[idx].reload = value;
    }

    /// Write to the high 16-bit control register of timer `idx`. A 0 -> 1
    /// transition of the enable bit loads the counter from the reload value
    /// and restarts the prescaler.
    pub fn write_cnt_h(&mut self, idx: usize, value: u16) {
        let t = &mut self.t[idx];
        let was_enabled = t.enabled;
        t.prescaler = (value & 0x03) as u8;
        t.cascade = value & 0x04 != 0;
        t.irq_enable = value & 0x40 != 0;
        t.enabled = value & 0x80 != 0;
        if t.enabled && !was_enabled {
            t.counter = t.reload;
            t.ticks = 0;
        }
    }

    /// Read the current counter value.
    pub fn read_cnt_l(&self, idx: usize) -> u16 {
        self.t[idx].counter
    }

    /// Pending overflow IRQ flags (bits 0-3).
    pub fn irq_flags(&self) -> u16 {
        self.flags
    }

    /// Take the overflow IRQ flags raised since the last call. The bus ORs
    /// them into IF once, so an acknowledged flag is not re-asserted.
    pub fn take_irq(&mut self) -> u16 {
        std::mem::take(&mut self.flags)
    }

    /// Advance the timers by `cycles` CPU cycles.
    pub fn step(&mut self, cycles: u32) {
        self.overflowed = [false; 4];
        for i in 0..4 {
            let t = &mut self.t[i];
            if !t.enabled {
                continue;
            }
            if t.cascade {
                // Cascade timers are clocked by the previous timer's overflow;
                // overflow is handled in `on_overflow`.
                continue;
            }
            let div = DIVIDERS[t.prescaler as usize];
            t.ticks += cycles;
            let steps = t.ticks / div;
            t.ticks %= div;
            for _ in 0..steps.min(0x10000) {
                self.tick(i);
            }
        }
    }

    /// Clock timer `i` once.
    fn tick(&mut self, i: usize) {
        if self.t[i].counter == 0xFFFF {
            self.overflow(i);
        } else {
            self.t[i].counter += 1;
        }
    }

    /// Timer `i` overflowed: reload, flag the IRQ and clock a cascaded
    /// successor, which may itself overflow and continue the chain.
    fn overflow(&mut self, i: usize) {
        self.t[i].counter = self.t[i].reload;
        self.overflowed[i] = true;
        if self.t[i].irq_enable {
            self.flags |= IRQ[i];
        }
        if i + 1 < 4 && self.t[i + 1].enabled && self.t[i + 1].cascade {
            self.tick(i + 1);
        }
    }

    /// Whether timer `i` is currently running.
    pub fn enabled(&self, i: usize) -> bool {
        self.t[i].enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_counts_and_overflows() {
        let mut tm = Timers::new();
        tm.write_cnt_l(0, 0xFFFE);
        // Prescaler 0 = /1, IRQ on, start.
        tm.write_cnt_h(0, 0x40 | 0x80);
        tm.step(1);
        assert_eq!(tm.read_cnt_l(0), 0xFFFF);
        tm.step(1);
        // Overflow: reload to 0xFFFE, raise IRQ.
        assert_eq!(tm.read_cnt_l(0), 0xFFFE);
        assert_eq!(tm.irq_flags() & IRQ[0], IRQ[0]);
    }

    #[test]
    fn reload_write_does_not_touch_a_running_counter() {
        let mut tm = Timers::new();
        tm.write_cnt_l(0, 0x1000);
        tm.write_cnt_h(0, 0x80);
        assert_eq!(tm.read_cnt_l(0), 0x1000, "enable loads the reload value");
        tm.step(4);
        tm.write_cnt_l(0, 0x2000);
        assert_eq!(tm.read_cnt_l(0), 0x1004, "reload is latched, not loaded");
        // Re-writing CNT_H with enable still set does not reload either.
        tm.write_cnt_h(0, 0x80);
        assert_eq!(tm.read_cnt_l(0), 0x1004);
    }

    #[test]
    fn three_timer_cascade_chain_overflows() {
        let mut tm = Timers::new();
        for i in 0..3 {
            tm.write_cnt_l(i, 0xFFFF);
        }
        tm.write_cnt_h(0, 0x80); // free-running, overflows every cycle
        tm.write_cnt_h(1, 0x80 | 0x04); // cascade
        tm.write_cnt_h(2, 0x80 | 0x04 | 0x40); // cascade + IRQ
        tm.step(1);
        assert!(tm.just_overflowed(0));
        assert!(tm.just_overflowed(1));
        assert!(tm.just_overflowed(2), "overflow propagates two levels");
        assert_eq!(tm.irq_flags(), IRQ[2]);
    }

    #[test]
    fn timer_prescaler_divides() {
        let mut tm = Timers::new();
        tm.write_cnt_l(1, 0);
        tm.write_cnt_h(1, 0x01 | 0x80); // /64
        tm.step(64);
        assert_eq!(tm.read_cnt_l(1), 1);
        tm.step(63);
        assert_eq!(tm.read_cnt_l(1), 1);
        tm.step(1);
        assert_eq!(tm.read_cnt_l(1), 2);
    }
}
