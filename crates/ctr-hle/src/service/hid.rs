//! Input, `hid:USER` (and `hid:SPVR`) (3dbrew, "HID Services", "HID Shared
//! Memory").
//!
//! The HID module writes the buttons, circle pad, touch screen and motion
//! sensors into a page it shares with the application, each section a ring
//! of entries with the tick of its last pass, and signals an event after
//! each update. Here the page is written from what the frontend last
//! reported, on a fixed clock: the rates are those other emulators use, not
//! measured ones.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::kernel::{Handle, ObjId, Object, Reset};
use crate::result;
use crate::service::Service;
use ctr_core::clock::ARM11_HZ;

/// The registry name: `hid:USER` and `hid:SPVR` share it.
const NAME: &str = "hid";
/// Updates a second: pad and touch screen, accelerometer, gyroscope.
const PAD_RATE: u64 = 234;
const ACCELEROMETER_RATE: u64 = 104;
const GYROSCOPE_RATE: u64 = 101;
/// Event tags.
const PAD: u32 = 0;
const ACCELEROMETER: u32 = 1;
const GYROSCOPE: u32 = 2;
/// The sections of the shared page.
const PAD_SECTION: usize = 0x000;
const TOUCH_SECTION: usize = 0x0A8;
const ACCELEROMETER_SECTION: usize = 0x108;
const GYROSCOPE_SECTION: usize = 0x158;
const SHARED_LEN: u32 = 0x1000;
/// The circle pad's direction bits, set past 41 units on an axis.
const CIRCLE_PAD_THRESHOLD: i16 = 41;
/// The gyroscope's raw units per degree a second.
const GYROSCOPE_COEFFICIENT: f32 = 14.375;
/// Its calibration for each axis: zero, the positive and negative readings
/// at the reference rate.
const GYROSCOPE_CALIBRATION: [i16; 3] = [0, 6700, -6700];

#[derive(Default)]
pub struct Hid {
    /// The shared page: its object and physical address.
    shared: Option<(ObjId, u32)>,
    /// Pad, touch screen, accelerometer, gyroscope, debug pad.
    events: Vec<ObjId>,
    accelerometer: bool,
    gyroscope: bool,
    /// Updates so far of the pad, touch, accelerometer and gyroscope rings.
    passes: [u32; 4],
    /// The buttons at the last update, a set bit pressed.
    last_pad: u32,
}

/// Advance the ring at `section` with `entries` entries: the index of the
/// entry to write. The tick fields move when entry 0 is written.
fn advance(page: &mut [u8], section: usize, passes: &mut u32, entries: u32, tick: u64) -> usize {
    let index = *passes % entries;
    *passes = passes.wrapping_add(1);
    if index == 0 {
        page.copy_within(section..section + 8, section + 8);
        page[section..section + 8].copy_from_slice(&tick.to_le_bytes());
    }
    page[section + 0x10..section + 0x14].copy_from_slice(&index.to_le_bytes());
    index as usize
}

fn put(page: &mut [u8], at: usize, bytes: &[u8]) {
    page[at..at + bytes.len()].copy_from_slice(bytes);
}

/// The circle pad as HID's words hold it: x in the low half, y in the high.
fn circle_pad((x, y): (i16, i16)) -> u32 {
    x as u16 as u32 | (y as u16 as u32) << 16
}

impl Hid {
    fn signal(&self, h: &mut Horizon, events: &[usize]) {
        for &n in events {
            if let Some(&event) = self.events.get(n) {
                h.kernel.signal(event);
            }
        }
    }

    /// The buttons and circle pad, then the touch screen.
    fn update_pad(&mut self, h: &mut Horizon) {
        let Some((_, pa)) = self.shared else {
            return;
        };
        let tick = h.clock.now();
        let buttons = !h.pad as u32 & 0x0FFF;
        let (x, y) = h.circle_pad;
        let directions = [
            (x >= CIRCLE_PAD_THRESHOLD, 28),
            (x <= -CIRCLE_PAD_THRESHOLD, 29),
            (y >= CIRCLE_PAD_THRESHOLD, 30),
            (y <= -CIRCLE_PAD_THRESHOLD, 31),
        ];
        let pad = directions
            .iter()
            .filter(|(on, _)| *on)
            .fold(buttons, |pad, (_, bit)| pad | 1 << bit);
        let (pressed, released) = (pad & !self.last_pad, !pad & self.last_pad);
        self.last_pad = pad;
        let touch = h.touch;
        let Some(page) = h.mem.slice_mut(pa, SHARED_LEN as usize) else {
            return;
        };

        let index = advance(page, PAD_SECTION, &mut self.passes[0], 8, tick);
        put(page, PAD_SECTION + 0x18, &0f32.to_bits().to_le_bytes());
        put(page, PAD_SECTION + 0x1C, &buttons.to_le_bytes());
        put(page, PAD_SECTION + 0x20, &circle_pad((x, y)).to_le_bytes());
        let entry = PAD_SECTION + 0x28 + index * 0x10;
        put(page, entry, &pad.to_le_bytes());
        put(page, entry + 4, &pressed.to_le_bytes());
        put(page, entry + 8, &released.to_le_bytes());
        put(page, entry + 0xC, &circle_pad((x, y)).to_le_bytes());

        let index = advance(page, TOUCH_SECTION, &mut self.passes[1], 8, tick);
        // The raw entry has the converter's 12-bit readings.
        let (pixels, raw) = match touch {
            Some((x, y)) => (
                [x, y, 1, 0],
                [
                    (x as u32 * 0x1000 / 320) as u16,
                    (y as u32 * 0x1000 / 240) as u16,
                    1,
                    0,
                ],
            ),
            None => ([0; 4], [0; 4]),
        };
        let bytes = |v: [u16; 4]| -> Vec<u8> { v.iter().flat_map(|w| w.to_le_bytes()).collect() };
        put(page, TOUCH_SECTION + 0x18, &bytes(raw));
        put(page, TOUCH_SECTION + 0x20 + index * 8, &bytes(pixels));
        self.signal(h, &[0, 1]);
    }

    /// A motion sensor's ring: its raw entry and the next of `entries`.
    fn update_motion(&mut self, h: &mut Horizon, sensor: usize) {
        let Some((_, pa)) = self.shared else {
            return;
        };
        let tick = h.clock.now();
        let (section, entries, values) = if sensor == 0 {
            (ACCELEROMETER_SECTION, 8, h.motion.accel)
        } else {
            (GYROSCOPE_SECTION, 32, h.motion.gyro)
        };
        let Some(page) = h.mem.slice_mut(pa, SHARED_LEN as usize) else {
            return;
        };
        let index = advance(page, section, &mut self.passes[2 + sensor], entries, tick);
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        put(page, section + 0x18, &bytes);
        put(page, section + 0x20 + index * 6, &bytes);
        self.signal(h, &[2 + sensor]);
    }

    fn start(&self, h: &mut Horizon, tag: u32, rate: u64) {
        h.schedule_service(NAME, ARM11_HZ as u64 / rate, tag);
    }
}

impl Service for Hid {
    fn name(&self) -> &str {
        NAME
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        match command {
            // GetIPCHandles: the shared page and the five events.
            0x000A => {
                let shared = match self.shared {
                    Some((object, _)) => Some(h.kernel.open(object)),
                    None => h.system_block(SHARED_LEN).map(|(handle, pa)| {
                        let object = h.kernel.resolve(handle).expect("a new handle resolves");
                        self.shared = Some((object, pa));
                        self.start(h, PAD, PAD_RATE);
                        handle
                    }),
                };
                let Some(shared) = shared else {
                    return Reply::new(command, result::OUT_OF_MEMORY);
                };
                if self.events.is_empty() {
                    for _ in 0..5 {
                        let object = h.kernel.create(Object::Event {
                            signalled: false,
                            reset: Reset::OneShot,
                        });
                        self.events.push(object);
                    }
                }
                let mut handles: Vec<Handle> = vec![shared];
                handles.extend(self.events.iter().map(|&event| h.kernel.open(event)));
                ok().handles(&handles, false)
            }
            // EnableAccelerometer, DisableAccelerometer.
            0x0011 | 0x0012 => {
                let on = command == 0x0011;
                if on && !self.accelerometer {
                    self.start(h, ACCELEROMETER, ACCELEROMETER_RATE);
                }
                self.accelerometer = on;
                ok()
            }
            // EnableGyroscopeLow, DisableGyroscopeLow.
            0x0013 | 0x0014 => {
                let on = command == 0x0013;
                if on && !self.gyroscope {
                    self.start(h, GYROSCOPE, GYROSCOPE_RATE);
                }
                self.gyroscope = on;
                ok()
            }
            // GetGyroscopeLowRawToDpsCoefficient: an f32.
            0x0015 => ok().word(GYROSCOPE_COEFFICIENT.to_bits()),
            // GetGyroscopeLowCalibrateParam: zero, positive and negative
            // readings for each of the three axes, as s16.
            0x0016 => {
                let halves: Vec<u16> = (0..3)
                    .flat_map(|_| GYROSCOPE_CALIBRATION.map(|v| v as u16))
                    .chain([0])
                    .collect();
                let words: Vec<u32> = halves
                    .chunks(2)
                    .map(|pair| pair[0] as u32 | (pair[1] as u32) << 16)
                    .collect();
                ok().words(&words)
            }
            // GetSoundVolume: the slider all the way up.
            0x0017 => ok().word(0x3F),
            _ => h.unhandled(NAME, request),
        }
    }

    fn event(&mut self, h: &mut Horizon, tag: u32) {
        match tag {
            PAD => {
                self.update_pad(h);
                self.start(h, PAD, PAD_RATE);
            }
            ACCELEROMETER if self.accelerometer => {
                self.update_motion(h, 0);
                self.start(h, ACCELEROMETER, ACCELEROMETER_RATE);
            }
            GYROSCOPE if self.gyroscope => {
                self.update_motion(h, 1);
                self.start(h, GYROSCOPE, GYROSCOPE_RATE);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_moves_its_tick_when_entry_zero_is_written() {
        let mut page = vec![0u8; 0x100];
        let mut passes = 0;
        assert_eq!(advance(&mut page, 0, &mut passes, 8, 5), 0);
        assert_eq!(page[0], 5);
        for n in 1..8 {
            assert_eq!(advance(&mut page, 0, &mut passes, 8, 9), n);
        }
        assert_eq!(page[0], 5);
        assert_eq!(advance(&mut page, 0, &mut passes, 8, 7), 0);
        assert_eq!((page[0], page[8]), (7, 5));
        assert_eq!(page[0x10], 0);
        assert_eq!(circle_pad((-1, 2)), 0x0002_FFFF);
    }
}
