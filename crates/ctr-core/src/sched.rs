//! The event scheduler.
//!
//! Time is counted in ARM11 cycles since power-on. Events that fall due at the
//! same time fire in the order they were scheduled, so a run is reproducible
//! on every host.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// ARM11 cycles since power-on.
pub type Time = u64;

/// Something that happens at a point in time.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Event {
    /// An ARM9 timer reaches 0x10000.
    Arm9Timer(u8),
    /// The private timer of an ARM11 core reaches zero.
    Arm11Timer(u8),
    /// The watchdog of an ARM11 core reaches zero.
    Arm11Watchdog(u8),
    /// The display controllers reach the end of a frame.
    VBlank,
    /// A memory fill unit of the GPU finishes.
    PscDone(u8),
    /// The GPU's transfer engine finishes.
    PpfDone,
    /// The GPU reaches the end of a command list.
    P3dDone,
}

/// A clock and the events waiting on it. The machine's scheduler is a queue
/// of its hardware [`Event`]s; the high-level 3DS mode keeps one of its own
/// kind of event.
pub struct Queue<E> {
    now: Time,
    sequence: u64,
    queue: BinaryHeap<Reverse<(Time, u64, E)>>,
}

/// The scheduler of the low-level machine.
pub type Scheduler = Queue<Event>;

impl<E: Copy + Ord> Default for Queue<E> {
    fn default() -> Self {
        Queue {
            now: 0,
            sequence: 0,
            queue: BinaryHeap::new(),
        }
    }
}

impl<E: Copy + Ord> Queue<E> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn now(&self) -> Time {
        self.now
    }

    /// Set the clock. Within a quantum each processor runs on its own clock,
    /// so time steps back when the next processor takes its turn; events are
    /// only drained once the quantum is over.
    pub fn set_now(&mut self, now: Time) {
        self.now = now;
    }

    /// Move time forward. Events are not fired; drain them with
    /// [`Scheduler::pop_due`].
    pub fn advance(&mut self, cycles: u64) {
        self.now += cycles;
    }

    /// Fire `event` at `at`, replacing any pending instance of it.
    pub fn schedule(&mut self, at: Time, event: E) {
        self.cancel(event);
        self.queue.push(Reverse((at, self.sequence, event)));
        self.sequence += 1;
    }

    pub fn cancel(&mut self, event: E) {
        self.queue.retain(|Reverse((_, _, e))| *e != event);
    }

    /// When the next event falls due, if any is pending.
    pub fn next_due(&self) -> Option<Time> {
        self.queue.peek().map(|Reverse((at, _, _))| *at)
    }

    /// The earliest event that is due by now, with the time it was due.
    pub fn pop_due(&mut self) -> Option<(Time, E)> {
        match self.queue.peek() {
            Some(Reverse((at, _, _))) if *at <= self.now => {
                let Reverse((at, _, event)) = self.queue.pop()?;
                Some((at, event))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_fire_in_time_order_then_in_scheduling_order() {
        let mut s = Scheduler::new();
        s.schedule(20, Event::Arm9Timer(2));
        s.schedule(10, Event::Arm9Timer(1));
        s.schedule(10, Event::Arm9Timer(0));
        assert_eq!(s.next_due(), Some(10));
        assert_eq!(s.pop_due(), None);
        s.advance(15);
        assert_eq!(s.pop_due(), Some((10, Event::Arm9Timer(1))));
        assert_eq!(s.pop_due(), Some((10, Event::Arm9Timer(0))));
        assert_eq!(s.pop_due(), None);
        s.advance(5);
        assert_eq!(s.pop_due(), Some((20, Event::Arm9Timer(2))));
        assert_eq!(s.next_due(), None);
    }

    #[test]
    fn scheduling_again_replaces_and_cancel_removes() {
        let mut s = Scheduler::new();
        s.schedule(10, Event::Arm9Timer(0));
        s.schedule(30, Event::Arm9Timer(0));
        s.schedule(20, Event::Arm9Timer(3));
        s.cancel(Event::Arm9Timer(3));
        s.advance(100);
        assert_eq!(s.pop_due(), Some((30, Event::Arm9Timer(0))));
        assert_eq!(s.pop_due(), None);
    }
}
