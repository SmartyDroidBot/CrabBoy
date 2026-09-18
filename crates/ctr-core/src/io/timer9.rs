//! The four ARM9 timers at 0x10003000.
//!
//! Each timer has a 16-bit counter that counts up and reloads on overflow,
//! as on the DS: `+0` reads the counter and writes the reload value, `+2` is
//! the control register (bits 1:0 prescaler, bit 2 count on the previous
//! timer's overflow, bit 6 interrupt on overflow, bit 7 start).
//!
//! Counters are not stepped. A running timer remembers the value it had at a
//! point in time and the scheduler holds its next overflow, so reads compute
//! the counter from the clock.

use crate::clock::ARM9_TIMER_CYCLES;
use crate::sched::{Event, Scheduler, Time};

const PRESCALER_SHIFT: [u32; 4] = [0, 6, 8, 10];

const CASCADE: u16 = 1 << 2;
const IRQ: u16 = 1 << 6;
const START: u16 = 1 << 7;
const CONTROL_MASK: u16 = 0x00C7;

#[derive(Clone, Copy, Default)]
struct Timer {
    reload: u16,
    control: u16,
    /// The counter at `since`.
    counter: u16,
    since: Time,
}

impl Timer {
    fn running(&self) -> bool {
        self.control & START != 0
    }

    /// Counts from the clock rather than from the previous timer.
    fn clocked(&self, index: usize) -> bool {
        self.running() && (index == 0 || self.control & CASCADE == 0)
    }

    /// ARM11 cycles per count.
    fn period(&self) -> u64 {
        ARM9_TIMER_CYCLES << PRESCALER_SHIFT[(self.control & 3) as usize]
    }
}

#[derive(Default)]
pub struct Timers {
    timers: [Timer; 4],
}

impl Timers {
    pub fn new() -> Self {
        Timers::default()
    }

    fn counter(&self, index: usize, now: Time) -> u16 {
        let t = &self.timers[index];
        if t.clocked(index) {
            t.counter
                .wrapping_add(((now - t.since) / t.period()) as u16)
        } else {
            t.counter
        }
    }

    fn schedule(&self, index: usize, sched: &mut Scheduler) {
        let t = &self.timers[index];
        let event = Event::Arm9Timer(index as u8);
        if t.clocked(index) {
            let counts = 0x1_0000 - t.counter as u64;
            sched.schedule(t.since + counts * t.period(), event);
        } else {
            sched.cancel(event);
        }
    }

    /// Read a 16-bit register; `offset` is relative to 0x10003000.
    pub fn read16(&self, offset: u32, now: Time) -> u16 {
        let index = (offset >> 2 & 3) as usize;
        if offset & 2 == 0 {
            self.counter(index, now)
        } else {
            self.timers[index].control
        }
    }

    pub fn write16(&mut self, offset: u32, value: u16, sched: &mut Scheduler) {
        let index = (offset >> 2 & 3) as usize;
        let now = sched.now();
        if offset & 2 == 0 {
            self.timers[index].reload = value;
            return;
        }
        let counter = self.counter(index, now);
        let t = &mut self.timers[index];
        let was_running = t.running();
        t.control = value & CONTROL_MASK;
        // Starting loads the reload value; any other change keeps the count.
        t.counter = if t.running() && !was_running {
            t.reload
        } else {
            counter
        };
        t.since = now;
        self.schedule(index, sched);
    }

    /// Handle an overflow event that fell due at `at`. Returns a bit per timer
    /// that requests an interrupt.
    pub fn overflow(&mut self, index: usize, at: Time, sched: &mut Scheduler) -> u8 {
        let mut irqs = 0;
        let mut index = index;
        loop {
            let t = &mut self.timers[index];
            t.counter = t.reload;
            t.since = at;
            if t.control & IRQ != 0 {
                irqs |= 1 << index;
            }
            self.schedule(index, sched);

            // Carry into the next timer when it counts overflows.
            index += 1;
            let Some(next) = self.timers.get_mut(index) else {
                break;
            };
            if !next.running() || next.control & CASCADE == 0 {
                break;
            }
            next.counter = next.counter.wrapping_add(1);
            if next.counter != 0 {
                break;
            }
        }
        irqs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fire(timers: &mut Timers, sched: &mut Scheduler) -> u8 {
        let mut irqs = 0;
        while let Some((at, Event::Arm9Timer(n))) = sched.pop_due() {
            irqs |= timers.overflow(n as usize, at, sched);
        }
        irqs
    }

    #[test]
    fn a_started_timer_counts_from_its_reload_value() {
        let mut sched = Scheduler::new();
        let mut timers = Timers::new();
        timers.write16(0, 0xFFF0, &mut sched);
        timers.write16(2, START, &mut sched);
        assert_eq!(timers.read16(0, sched.now()), 0xFFF0);
        sched.advance(5 * ARM9_TIMER_CYCLES + 1);
        assert_eq!(timers.read16(0, sched.now()), 0xFFF5);
        assert_eq!(timers.read16(2, sched.now()), START);
    }

    #[test]
    fn overflow_reloads_and_raises_the_interrupt() {
        let mut sched = Scheduler::new();
        let mut timers = Timers::new();
        timers.write16(4, 0xFFFE, &mut sched);
        timers.write16(6, START | IRQ | 1, &mut sched);
        sched.advance(2 * 64 * ARM9_TIMER_CYCLES - 1);
        assert_eq!(fire(&mut timers, &mut sched), 0);
        sched.advance(1);
        assert_eq!(fire(&mut timers, &mut sched), 1 << 1);
        assert_eq!(timers.read16(4, sched.now()), 0xFFFE);
        sched.advance(64 * ARM9_TIMER_CYCLES);
        assert_eq!(timers.read16(4, sched.now()), 0xFFFF);
    }

    #[test]
    fn a_cascaded_timer_counts_overflows() {
        let mut sched = Scheduler::new();
        let mut timers = Timers::new();
        timers.write16(0, 0xFFFF, &mut sched);
        timers.write16(4, 0xFFFE, &mut sched);
        timers.write16(6, START | CASCADE | IRQ, &mut sched);
        timers.write16(2, START, &mut sched);
        sched.advance(ARM9_TIMER_CYCLES);
        assert_eq!(fire(&mut timers, &mut sched), 0);
        assert_eq!(timers.read16(4, sched.now()), 0xFFFF);
        sched.advance(ARM9_TIMER_CYCLES);
        assert_eq!(fire(&mut timers, &mut sched), 1 << 1);
        assert_eq!(timers.read16(4, sched.now()), 0xFFFE);
    }

    #[test]
    fn stopping_freezes_the_counter() {
        let mut sched = Scheduler::new();
        let mut timers = Timers::new();
        timers.write16(2, START, &mut sched);
        sched.advance(10 * ARM9_TIMER_CYCLES);
        timers.write16(2, 0, &mut sched);
        sched.advance(10 * ARM9_TIMER_CYCLES);
        assert_eq!(timers.read16(0, sched.now()), 10);
        assert_eq!(sched.next_due(), None);
    }
}
