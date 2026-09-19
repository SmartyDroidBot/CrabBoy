//! The private timer of each ARM11 core (ARM11 MPCore TRM, "Timer and
//! watchdog registers"), at 0x17E00600 as seen by that core.
//!
//! A 32-bit down counter: `+0x00` load, `+0x04` counter, `+0x08` control
//! (bit 0 enable, bit 1 reload on zero, bit 2 interrupt enable, bits 15:8
//! prescaler) and `+0x0C` the event flag, cleared by writing one. It counts
//! once every `(prescaler + 1)` periods of the peripheral clock, half the
//! processor clock. As with the ARM9 timers, the counter is computed from
//! the clock and the scheduler holds the moment it reaches zero.
//!
//! The watchdog at `+0x20` is the same counter with two more registers:
//! `+0x10` the reset flag and `+0x14` the disable register, where writing
//! 0x12345678 and then 0x87654321 leaves watchdog mode (control bit 3, which
//! a control write can only set). In timer mode it raises interrupt 30 where
//! the timer raises 29. Watchdog mode would reset the machine at zero; that
//! is not modelled, and it counts as a timer there too.

use crate::sched::{Event, Scheduler, Time};

/// ARM11 cycles per period of the peripheral clock.
const PERIPHERAL_CYCLES: u64 = 2;

const ENABLE: u32 = 1 << 0;
const RELOAD: u32 = 1 << 1;
const IRQ: u32 = 1 << 2;
const WATCHDOG_MODE: u32 = 1 << 3;
const CONTROL_MASK: u32 = 0xFF07;
const DISABLE_KEYS: [u32; 2] = [0x1234_5678, 0x8765_4321];

#[derive(Default)]
pub struct PrivateTimer {
    core: u8,
    load: u32,
    control: u32,
    /// The counter at `since`.
    counter: u32,
    since: Time,
    event: bool,
    /// This is the core's watchdog, not its timer.
    watchdog: bool,
    /// The first disable key was the last write to the disable register.
    unlocking: bool,
}

impl PrivateTimer {
    pub fn new(core: u8) -> Self {
        PrivateTimer {
            core,
            ..PrivateTimer::default()
        }
    }

    /// The watchdog of `core`.
    pub fn watchdog(core: u8) -> Self {
        PrivateTimer {
            core,
            watchdog: true,
            ..PrivateTimer::default()
        }
    }

    fn period(&self) -> u64 {
        ((self.control >> 8 & 0xFF) as u64 + 1) * PERIPHERAL_CYCLES
    }

    fn running(&self) -> bool {
        self.control & ENABLE != 0 && self.counter != 0
    }

    fn counter_at(&self, now: Time) -> u32 {
        if self.running() {
            let elapsed = (now - self.since) / self.period();
            self.counter
                .saturating_sub(elapsed.min(u32::MAX as u64) as u32)
        } else {
            self.counter
        }
    }

    fn schedule(&self, sched: &mut Scheduler) {
        let event = if self.watchdog {
            Event::Arm11Watchdog(self.core)
        } else {
            Event::Arm11Timer(self.core)
        };
        if self.running() {
            sched.schedule(self.since + self.counter as u64 * self.period(), event);
        } else {
            sched.cancel(event);
        }
    }

    pub fn read(&self, offset: u32, now: Time) -> u32 {
        match offset {
            0x00 => self.load,
            0x04 => self.counter_at(now),
            0x08 => self.control,
            0x0C => self.event as u32,
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u32, value: u32, sched: &mut Scheduler) {
        let now = sched.now();
        match offset {
            // Writing the load register also sets the counter.
            0x00 => {
                self.load = value;
                self.counter = value;
            }
            0x04 => self.counter = value,
            0x08 => {
                self.counter = self.counter_at(now);
                let mode = if self.watchdog {
                    (self.control | value) & WATCHDOG_MODE
                } else {
                    0
                };
                self.control = value & CONTROL_MASK | mode;
            }
            0x14 if self.watchdog => {
                if self.unlocking && value == DISABLE_KEYS[1] {
                    self.control &= !WATCHDOG_MODE;
                }
                self.unlocking = value == DISABLE_KEYS[0];
                return;
            }
            0x0C => {
                if value & 1 != 0 {
                    self.event = false;
                }
                return;
            }
            _ => return,
        }
        self.since = now;
        self.schedule(sched);
    }

    /// The counter reached zero at `at`. Returns whether the interrupt is
    /// requested.
    pub fn expire(&mut self, at: Time, sched: &mut Scheduler) -> bool {
        self.event = true;
        self.counter = if self.control & RELOAD != 0 {
            self.load
        } else {
            0
        };
        self.since = at;
        self.schedule(sched);
        self.control & IRQ != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_watchdog_counts_as_a_timer_with_its_own_event() {
        let mut sched = Scheduler::new();
        let mut dog = PrivateTimer::watchdog(1);
        dog.write(0x00, 10, &mut sched);
        dog.write(0x08, ENABLE | IRQ, &mut sched);
        sched.advance(20);
        let (at, event) = sched.pop_due().unwrap();
        assert_eq!(event, Event::Arm11Watchdog(1));
        assert!(dog.expire(at, &mut sched));
        assert_eq!(dog.read(0x0C, sched.now()), 1);
        assert_eq!(dog.read(0x04, sched.now()), 0);
    }

    #[test]
    fn watchdog_mode_is_left_only_through_the_disable_keys() {
        let mut sched = Scheduler::new();
        let mut dog = PrivateTimer::watchdog(0);
        dog.write(0x08, WATCHDOG_MODE, &mut sched);
        dog.write(0x08, 0, &mut sched);
        assert_ne!(dog.read(0x08, 0) & WATCHDOG_MODE, 0);
        dog.write(0x14, DISABLE_KEYS[1], &mut sched);
        assert_ne!(dog.read(0x08, 0) & WATCHDOG_MODE, 0);
        dog.write(0x14, DISABLE_KEYS[0], &mut sched);
        dog.write(0x14, DISABLE_KEYS[1], &mut sched);
        assert_eq!(dog.read(0x08, 0) & WATCHDOG_MODE, 0);

        // The plain timer has no such bit.
        let mut timer = PrivateTimer::new(0);
        timer.write(0x08, WATCHDOG_MODE, &mut sched);
        assert_eq!(timer.read(0x08, 0), 0);
    }

    #[test]
    fn counts_down_at_the_prescaled_rate_and_reloads() {
        let mut sched = Scheduler::new();
        let mut timer = PrivateTimer::new(0);
        timer.write(0x00, 100, &mut sched);
        timer.write(0x08, ENABLE | RELOAD | IRQ | 1 << 8, &mut sched);
        sched.advance(10 * 4);
        assert_eq!(timer.read(0x04, sched.now()), 90);
        sched.advance(90 * 4);
        let (at, event) = sched.pop_due().unwrap();
        assert_eq!(event, Event::Arm11Timer(0));
        assert!(timer.expire(at, &mut sched));
        assert_eq!(timer.read(0x0C, sched.now()), 1);
        assert_eq!(timer.read(0x04, sched.now()), 100);
        timer.write(0x0C, 1, &mut sched);
        assert_eq!(timer.read(0x0C, sched.now()), 0);
        assert_eq!(sched.next_due(), Some(400 + 400));
    }

    #[test]
    fn a_single_shot_timer_stops_at_zero() {
        let mut sched = Scheduler::new();
        let mut timer = PrivateTimer::new(1);
        timer.write(0x00, 5, &mut sched);
        timer.write(0x08, ENABLE, &mut sched);
        sched.advance(100);
        let (at, _) = sched.pop_due().unwrap();
        assert!(
            !timer.expire(at, &mut sched),
            "the interrupt is not enabled"
        );
        assert_eq!(timer.read(0x04, sched.now()), 0);
        assert_eq!(sched.next_due(), None);
    }

    #[test]
    fn disabling_freezes_the_counter() {
        let mut sched = Scheduler::new();
        let mut timer = PrivateTimer::new(0);
        timer.write(0x00, 1000, &mut sched);
        timer.write(0x08, ENABLE, &mut sched);
        sched.advance(200);
        timer.write(0x08, 0, &mut sched);
        sched.advance(200);
        assert_eq!(timer.read(0x04, sched.now()), 900);
        assert_eq!(sched.next_due(), None);
    }
}
