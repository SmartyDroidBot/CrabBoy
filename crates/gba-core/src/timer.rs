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

/// IRQ flags for each timer overflow.
pub const IRQ: [u16; 4] = [1 << 0, 1 << 1, 1 << 2, 1 << 3];

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

    /// Write to the low 16-bit reload register of timer `idx`.
    pub fn write_cnt_l(&mut self, idx: usize, value: u16) {
        self.t[idx].reload = value;
        self.t[idx].counter = value;
    }

    /// Write to the high 16-bit control register of timer `idx`.
    pub fn write_cnt_h(&mut self, idx: usize, value: u16) {
        let t = &mut self.t[idx];
        t.prescaler = (value & 0x03) as u8;
        t.cascade = value & 0x04 != 0;
        t.irq_enable = value & 0x40 != 0;
        t.enabled = value & 0x80 != 0;
        if t.enabled {
            t.ticks = 0;
            if t.counter == 0 && t.reload != 0 {
                t.counter = t.reload;
            }
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

    /// Clear pending overflow IRQ flags (on IF write).
    pub fn clear_irq(&mut self, mask: u16) {
        self.flags &= !(mask & 0x0F);
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

    fn tick(&mut self, i: usize) {
        let t = &mut self.t[i];
        if t.counter == 0xFFFF {
            t.counter = t.reload;
            self.overflowed[i] = true;
            if t.irq_enable {
                self.flags |= IRQ[i];
            }
            // Cascade: clock the next timer if it is in cascade mode.
            if i + 1 < 4 {
                let next = &self.t[i + 1];
                if next.enabled && next.cascade {
                    let irq_enable = self.t[i + 1].irq_enable;
                    let reload = self.t[i + 1].reload;
                    let c = self.t[i + 1].counter;
                    self.t[i + 1].counter = if c == 0xFFFF {
                        self.overflowed[i + 1] = true;
                        if irq_enable {
                            self.flags |= IRQ[i + 1];
                        }
                        reload
                    } else {
                        c + 1
                    };
                }
            }
        } else {
            t.counter = t.counter.wrapping_add(1);
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
