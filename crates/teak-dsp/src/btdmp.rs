//! The BTDMP audio ports: a 16-word transmit queue the DSP fills and the
//! codec drains, a left and a right sample every period (Teakra's
//! `btdmp.cpp`, `btdmp.md`).

use crate::timer::INFINITY;
use std::collections::VecDeque;

const QUEUE_LEN: usize = 16;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Btdmp {
    pub transmit_clock_config: u16,
    /// Cycles between two samples. How it follows from the clock
    /// configuration is not known; every game leaves 4096, which at the
    /// DSP's 134 MHz is the codec's 32,728 Hz.
    pub transmit_period: u16,
    transmit_timer: u16,
    pub transmit_enable: u16,
    transmit_empty: bool,
    transmit_full: bool,
    queue: VecDeque<u16>,
}

impl Default for Btdmp {
    fn default() -> Self {
        Btdmp {
            transmit_clock_config: 0,
            transmit_period: 4096,
            transmit_timer: 0,
            transmit_enable: 0,
            transmit_empty: true,
            transmit_full: false,
            queue: VecDeque::with_capacity(QUEUE_LEN),
        }
    }
}

impl Btdmp {
    pub fn transmit_empty(&self) -> bool {
        self.transmit_empty
    }

    pub fn transmit_full(&self) -> bool {
        self.transmit_full
    }

    /// Queue a sample word; one that does not fit is lost.
    pub fn send(&mut self, value: u16) {
        if self.queue.len() < QUEUE_LEN {
            self.queue.push_back(value);
            self.transmit_empty = false;
            self.transmit_full = self.queue.len() == QUEUE_LEN;
        }
    }

    pub fn flush(&mut self) {
        self.queue.clear();
        self.transmit_empty = true;
        self.transmit_full = false;
    }

    /// One cycle. A sample pair goes to `out` every period; whether the
    /// queue just ran empty, which interrupts.
    pub fn tick(&mut self, out: &mut Vec<[i16; 2]>) -> bool {
        if self.transmit_enable == 0 {
            return false;
        }
        self.transmit_timer += 1;
        if self.transmit_timer < self.transmit_period {
            return false;
        }
        self.transmit_timer = 0;
        let mut interrupt = false;
        let mut sample = [0i16; 2];
        for s in &mut sample {
            if let Some(value) = self.queue.pop_front() {
                *s = value as i16;
                self.transmit_empty = self.queue.is_empty();
                self.transmit_full = false;
                if self.transmit_empty {
                    interrupt = true;
                }
            }
        }
        out.push(sample);
        interrupt
    }

    /// Cycles that can pass before the tick that empties the queue.
    pub fn max_skip(&self) -> u64 {
        if self.transmit_enable == 0 || self.queue.is_empty() {
            return INFINITY;
        }
        let period = self.transmit_period as u64;
        let mut ticks = 0;
        if self.transmit_timer < self.transmit_period {
            ticks += period - self.transmit_timer as u64 - 1;
        }
        ticks + ((self.queue.len() as u64).div_ceil(2) - 1) * period
    }

    pub fn skip(&mut self, ticks: u64, out: &mut Vec<[i16; 2]>) {
        if self.transmit_enable == 0 {
            return;
        }
        if self.transmit_timer >= self.transmit_period {
            self.transmit_timer = 0;
        }
        let period = self.transmit_period.max(1) as u64;
        let future = self.transmit_timer as u64 + ticks;
        self.transmit_timer = (future % period) as u16;
        for _ in 0..future / period {
            let mut sample = [0i16; 2];
            for s in &mut sample {
                if let Some(value) = self.queue.pop_front() {
                    *s = value as i16;
                    self.transmit_full = false;
                }
            }
            out.push(sample);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Teakra's tests/btdmp.cpp.
    #[test]
    fn btdmp_queueing() {
        let mut b = Btdmp::default();
        let mut out = Vec::new();
        let mut interrupts = 0;
        let mut tick = |b: &mut Btdmp, out: &mut Vec<[i16; 2]>, n: u32| {
            for _ in 0..n {
                interrupts += b.tick(out) as u32;
            }
            interrupts
        };
        assert_eq!(b.max_skip(), INFINITY);
        b.transmit_enable = 1;
        b.transmit_period = 1000;
        assert!(b.transmit_empty() && !b.transmit_full());
        assert_eq!(b.max_skip(), INFINITY);

        b.skip(3050, &mut out);
        assert_eq!(std::mem::take(&mut out), [[0, 0]; 3]);
        b.skip(949, &mut out);
        assert!(out.is_empty());
        assert_eq!(tick(&mut b, &mut out, 1), 0);
        assert_eq!(std::mem::take(&mut out), [[0, 0]]);
        b.skip(999, &mut out);
        assert!(out.is_empty());
        b.skip(1, &mut out);
        assert_eq!(std::mem::take(&mut out), [[0, 0]]);

        b.skip(100, &mut out);
        b.send(0x1234);
        assert_eq!(b.max_skip(), 899);
        assert!(!b.transmit_empty() && !b.transmit_full());
        b.skip(899, &mut out);
        assert_eq!(b.max_skip(), 0);
        assert!(out.is_empty());
        assert_eq!(tick(&mut b, &mut out, 1), 1);
        assert_eq!(b.max_skip(), INFINITY);
        assert!(b.transmit_empty());
        assert_eq!(std::mem::take(&mut out), [[0x1234, 0]]);

        b.skip(100, &mut out);
        for v in [11, 22, 33, 44, 55] {
            b.send(v);
        }
        assert_eq!(b.max_skip(), 900 + 1000 + 1000 - 1);
        b.skip(1500, &mut out);
        assert_eq!(b.max_skip(), 900 + 1000 + 1000 - 1 - 1500);
        assert_eq!(std::mem::take(&mut out), [[11, 22]]);

        for v in 0..13 {
            assert!(!b.transmit_full());
            b.send(v);
        }
        assert_eq!(b.max_skip(), 1000 * 8 - 600 - 1);
        assert!(b.transmit_full());
        assert_eq!(tick(&mut b, &mut out, 4567), 1);
        assert_eq!(b.max_skip(), 1000 * 8 - 600 - 1 - 4567);
        assert_eq!(out.len(), 5);
        assert!(!b.transmit_empty() && !b.transmit_full());
        b.skip(1000 * 7 - 600 - 1 - 4567, &mut out);
        assert_eq!(b.max_skip(), 1000);
        assert_eq!(out.len(), 6);
        assert_eq!(tick(&mut b, &mut out, 1), 1);
        assert_eq!(b.max_skip(), 999);
        assert_eq!(
            std::mem::take(&mut out),
            [[33, 44], [55, 0], [1, 2], [3, 4], [5, 6], [7, 8], [9, 10]]
        );
        b.skip(999, &mut out);
        assert_eq!(b.max_skip(), 0);
        assert!(out.is_empty());
        assert_eq!(tick(&mut b, &mut out, 1), 2);
        assert_eq!(b.max_skip(), INFINITY);
        assert!(b.transmit_empty());
        assert_eq!(std::mem::take(&mut out), [[11, 12]]);
    }
}
