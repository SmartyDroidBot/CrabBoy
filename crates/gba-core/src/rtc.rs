//! Cartridge real-time clock (Seiko S-3511A), bit-banged over the cartridge
//! GPIO port at 0x080000C4 (GBATEK, "GBA Cart Real-Time Clock").
//!
//! Pins on the GPIO data register: bit 0 = SCK (clock), bit 1 = SIO (data),
//! bit 2 = CS (chip select). A transfer starts when CS rises; every SCK rising
//! edge moves one bit. The command byte is sent MSB first and has the form
//! `0110 rrr w`: `rrr` selects the register (0 reset, 1 status, 2 date/time,
//! 3 time, 4 alarm) and the low bit is 1 for a read. Data bytes follow LSB
//! first: 1 byte of status, 7 bytes of BCD date/time (yy mm dd weekday hh mm
//! ss), 3 bytes of time (hh mm ss) or 2 bytes of alarm.
//!
//! The clock counts emulated cycles, so a run is deterministic; frontends seed
//! the wall-clock time through [`Rtc::set_unix_time`].

/// CPU cycles per second (16.78 MHz).
const CYCLES_PER_SECOND: u32 = 16_777_216;
/// 2000-01-01 00:00:00 UTC, the value the chip resets to.
const EPOCH_2000: u64 = 946_684_800;

const STATUS_24HOUR: u8 = 1 << 6;
const STATUS_POWER: u8 = 1 << 7;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// Waiting for the eight command bits.
    Command,
    /// Clocking data bits out to the GBA.
    Read,
    /// Clocking data bits in from the GBA.
    Write,
    /// Transfer finished; nothing more until CS drops.
    Done,
}

/// The cartridge RTC.
pub struct Rtc {
    /// Seconds since the Unix epoch.
    unix: u64,
    /// Cycles accumulated towards the next second.
    sub_cycles: u32,
    status: u8,
    alarm: [u8; 2],
    // Pin state.
    sck: bool,
    cs: bool,
    // Transfer state.
    phase: Phase,
    shift: u8,
    bits: u8,
    register: u8,
    buf: [u8; 8],
    len: usize,
    /// Bit position within `buf` for the data phase.
    pos: usize,
    /// Level currently driven on SIO during a read.
    sio_out: bool,
}

impl Default for Rtc {
    fn default() -> Self {
        Rtc {
            unix: EPOCH_2000,
            sub_cycles: 0,
            status: STATUS_24HOUR,
            alarm: [0; 2],
            sck: false,
            cs: false,
            phase: Phase::Command,
            shift: 0,
            bits: 0,
            register: 0,
            buf: [0; 8],
            len: 0,
            pos: 0,
            sio_out: false,
        }
    }
}

fn to_bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

fn from_bcd(v: u8) -> u8 {
    (v >> 4) * 10 + (v & 0x0F)
}

/// Days since 1970-01-01 to a civil date (year, month, day), proleptic
/// Gregorian; Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Civil date to days since 1970-01-01 (inverse of `civil_from_days`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

impl Rtc {
    pub fn new() -> Rtc {
        Rtc::default()
    }

    /// Seconds since the Unix epoch as the clock currently reads.
    pub fn unix_time(&self) -> u64 {
        self.unix
    }

    /// Set the clock (frontends seed it from the host wall clock).
    pub fn set_unix_time(&mut self, unix: u64) {
        self.unix = unix;
        self.sub_cycles = 0;
    }

    /// Advance the clock by emulated CPU cycles.
    pub fn advance(&mut self, cycles: u32) {
        self.sub_cycles += cycles;
        while self.sub_cycles >= CYCLES_PER_SECOND {
            self.sub_cycles -= CYCLES_PER_SECOND;
            self.unix += 1;
        }
    }

    /// The seven BCD date/time bytes: yy mm dd weekday hh mm ss.
    fn date_time_bytes(&self) -> [u8; 7] {
        let days = (self.unix / 86_400) as i64;
        let secs = self.unix % 86_400;
        let (y, m, d) = civil_from_days(days);
        // 1970-01-01 was a Thursday; the chip counts Sunday as 0.
        let weekday = ((days + 4).rem_euclid(7)) as u8;
        let hour24 = (secs / 3600) as u8;
        let hour = if self.status & STATUS_24HOUR != 0 {
            to_bcd(hour24)
        } else {
            to_bcd(hour24 % 12) | if hour24 >= 12 { 0x80 } else { 0 }
        };
        [
            to_bcd((y.rem_euclid(100)) as u8),
            to_bcd(m as u8),
            to_bcd(d as u8),
            weekday,
            hour,
            to_bcd(((secs % 3600) / 60) as u8),
            to_bcd((secs % 60) as u8),
        ]
    }

    fn set_date_time(&mut self, b: &[u8; 7]) {
        let y = 2000 + from_bcd(b[0]) as i64;
        let m = from_bcd(b[1]).clamp(1, 12) as u32;
        let d = from_bcd(b[2]).clamp(1, 31) as u32;
        self.set_time_of_day(&[b[4], b[5], b[6]], days_from_civil(y, m, d));
    }

    fn set_time_of_day(&mut self, b: &[u8; 3], days: i64) {
        let mut hour = from_bcd(b[0] & 0x3F) as u64;
        if self.status & STATUS_24HOUR == 0 && b[0] & 0x80 != 0 {
            hour = (hour % 12) + 12;
        }
        let secs = hour * 3600 + from_bcd(b[1]) as u64 * 60 + from_bcd(b[2]) as u64;
        self.unix = (days.max(0) as u64) * 86_400 + secs;
        self.sub_cycles = 0;
    }

    /// Drive the GPIO pins from the GBA side: bit 0 = SCK, bit 1 = SIO, bit 2
    /// = CS. Only the levels of pins configured as outputs are meaningful.
    pub fn write_pins(&mut self, pins: u8) {
        let sck = pins & 1 != 0;
        let sio = pins & 2 != 0;
        let cs = pins & 4 != 0;
        if cs != self.cs {
            self.cs = cs;
            // A new transfer starts on every CS rise; CS low aborts.
            self.phase = Phase::Command;
            self.shift = 0;
            self.bits = 0;
            self.pos = 0;
            self.sio_out = false;
        }
        let rising = sck && !self.sck;
        self.sck = sck;
        if !cs || !rising {
            return;
        }
        match self.phase {
            Phase::Command => {
                self.shift = (self.shift << 1) | sio as u8;
                self.bits += 1;
                if self.bits == 8 {
                    self.bits = 0;
                    self.start_command(self.shift);
                }
            }
            Phase::Read => {
                let byte = self.pos / 8;
                self.sio_out = (self.buf[byte] >> (self.pos % 8)) & 1 != 0;
                self.pos += 1;
                if self.pos >= self.len * 8 {
                    self.phase = Phase::Done;
                }
            }
            Phase::Write => {
                let byte = self.pos / 8;
                if sio {
                    self.buf[byte] |= 1 << (self.pos % 8);
                }
                self.pos += 1;
                if self.pos >= self.len * 8 {
                    self.finish_write();
                    self.phase = Phase::Done;
                }
            }
            Phase::Done => {}
        }
    }

    fn start_command(&mut self, cmd: u8) {
        if cmd & 0xF0 != 0x60 {
            self.phase = Phase::Done;
            return;
        }
        self.register = (cmd >> 1) & 7;
        let read = cmd & 1 != 0;
        self.len = match self.register {
            1 => 1,
            2 => 7,
            3 => 3,
            4 => 2,
            _ => 0,
        };
        self.buf = [0; 8];
        self.pos = 0;
        if self.register == 0 {
            // Reset: 2000-01-01 00:00:00, 24-hour mode, flags clear.
            self.unix = EPOCH_2000;
            self.sub_cycles = 0;
            self.status = STATUS_24HOUR;
            self.alarm = [0; 2];
            self.phase = Phase::Done;
            return;
        }
        if self.len == 0 {
            self.phase = Phase::Done;
            return;
        }
        if read {
            let dt = self.date_time_bytes();
            match self.register {
                1 => self.buf[0] = self.status,
                2 => self.buf[..7].copy_from_slice(&dt),
                3 => self.buf[..3].copy_from_slice(&dt[4..7]),
                _ => self.buf[..2].copy_from_slice(&self.alarm),
            }
            self.phase = Phase::Read;
        } else {
            self.phase = Phase::Write;
        }
    }

    fn finish_write(&mut self) {
        match self.register {
            1 => {
                // Only the mode/interrupt bits are writable; a write also
                // acknowledges the power-failure flag.
                self.status = (self.buf[0] & 0x6A) | (self.status & !(0x6A | STATUS_POWER));
            }
            2 => {
                let mut b = [0u8; 7];
                b.copy_from_slice(&self.buf[..7]);
                self.set_date_time(&b);
            }
            3 => {
                let days = (self.unix / 86_400) as i64;
                let mut b = [0u8; 3];
                b.copy_from_slice(&self.buf[..3]);
                self.set_time_of_day(&b, days);
            }
            4 => self.alarm.copy_from_slice(&self.buf[..2]),
            _ => {}
        }
    }

    /// Level the chip drives on SIO (valid while the GBA reads the pin).
    pub fn sio_out(&self) -> bool {
        self.sio_out
    }

    /// Transient protocol state and clock for save states.
    pub fn save_state(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[0..8].copy_from_slice(&self.unix.to_le_bytes());
        out[8..12].copy_from_slice(&self.sub_cycles.to_le_bytes());
        out[12] = self.status;
        out[13..15].copy_from_slice(&self.alarm);
        out[15] = (self.sck as u8) | (self.cs as u8) << 1 | (self.sio_out as u8) << 2;
        out[16] = match self.phase {
            Phase::Command => 0,
            Phase::Read => 1,
            Phase::Write => 2,
            Phase::Done => 3,
        };
        out[17] = self.shift;
        out[18] = self.bits;
        out[19] = self.register;
        out[20..28].copy_from_slice(&self.buf);
        out[28] = self.len as u8;
        out[29] = self.pos as u8;
        out
    }

    pub fn load_state(&mut self, b: &[u8; 32]) {
        self.unix = u64::from_le_bytes(b[0..8].try_into().unwrap());
        self.sub_cycles = u32::from_le_bytes(b[8..12].try_into().unwrap());
        self.status = b[12];
        self.alarm.copy_from_slice(&b[13..15]);
        self.sck = b[15] & 1 != 0;
        self.cs = b[15] & 2 != 0;
        self.sio_out = b[15] & 4 != 0;
        self.phase = match b[16] {
            1 => Phase::Read,
            2 => Phase::Write,
            3 => Phase::Done,
            _ => Phase::Command,
        };
        self.shift = b[17];
        self.bits = b[18];
        self.register = b[19];
        self.buf.copy_from_slice(&b[20..28]);
        self.len = (b[28] as usize).min(8);
        self.pos = (b[29] as usize).min(64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the pins the way the game's `siirtc` library does.
    struct Driver<'a>(&'a mut Rtc);

    impl Driver<'_> {
        fn select(&mut self) {
            self.0.write_pins(0b001); // SCK high, CS low
            self.0.write_pins(0b101); // CS high
        }
        fn deselect(&mut self) {
            self.0.write_pins(0b001);
        }
        fn write_command(&mut self, cmd: u8) {
            for i in (0..8).rev() {
                let sio = ((cmd >> i) & 1) << 1;
                self.0.write_pins(0b100 | sio);
                self.0.write_pins(0b101 | sio);
            }
        }
        fn write_data(&mut self, value: u8) {
            for i in 0..8 {
                let sio = ((value >> i) & 1) << 1;
                self.0.write_pins(0b100 | sio);
                self.0.write_pins(0b101 | sio);
            }
        }
        fn read_data(&mut self) -> u8 {
            let mut v = 0u8;
            for i in 0..8 {
                self.0.write_pins(0b100);
                self.0.write_pins(0b101);
                v |= (self.0.sio_out() as u8) << i;
            }
            v
        }
        fn read(&mut self, cmd: u8, n: usize) -> Vec<u8> {
            self.select();
            self.write_command(cmd);
            let v = (0..n).map(|_| self.read_data()).collect();
            self.deselect();
            v
        }
        fn write(&mut self, cmd: u8, data: &[u8]) {
            self.select();
            self.write_command(cmd);
            for &b in data {
                self.write_data(b);
            }
            self.deselect();
        }
    }

    #[test]
    fn status_read_reports_24_hour_mode_and_no_power_failure() {
        let mut rtc = Rtc::new();
        let mut d = Driver(&mut rtc);
        assert_eq!(d.read(0x63, 1), vec![0x40]);
    }

    #[test]
    fn date_time_reads_back_in_bcd_and_advances_with_cycles() {
        let mut rtc = Rtc::new();
        rtc.set_unix_time(1_136_239_445); // 2006-01-02 22:04:05 UTC, a Monday
        rtc.advance(CYCLES_PER_SECOND * 2 + 5);
        let mut d = Driver(&mut rtc);
        assert_eq!(d.read(0x65, 7), vec![0x06, 0x01, 0x02, 1, 0x22, 0x04, 0x07]);
        assert_eq!(d.read(0x67, 3), vec![0x22, 0x04, 0x07]);
    }

    #[test]
    fn time_and_date_writes_set_the_clock() {
        let mut rtc = Rtc::new();
        let mut d = Driver(&mut rtc);
        d.write(0x64, &[0x24, 0x02, 0x29, 4, 0x23, 0x59, 0x58]); // 2024-02-29 23:59:58
        assert_eq!(rtc.unix_time(), 1_709_251_198);
        rtc.advance(CYCLES_PER_SECOND * 3);
        let mut d = Driver(&mut rtc);
        assert_eq!(d.read(0x65, 7), vec![0x24, 0x03, 0x01, 5, 0x00, 0x00, 0x01]);
        d.write(0x66, &[0x12, 0x30, 0x00]);
        assert_eq!(d.read(0x67, 3), vec![0x12, 0x30, 0x00]);
    }

    #[test]
    fn reset_command_returns_to_the_epoch() {
        let mut rtc = Rtc::new();
        rtc.set_unix_time(1_700_000_000);
        let mut d = Driver(&mut rtc);
        d.write(0x60, &[]);
        assert_eq!(rtc.unix_time(), EPOCH_2000);
        let mut d = Driver(&mut rtc);
        assert_eq!(d.read(0x65, 7), vec![0x00, 0x01, 0x01, 6, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn invalid_command_and_cs_drop_are_ignored() {
        let mut rtc = Rtc::new();
        let mut d = Driver(&mut rtc);
        d.select();
        d.write_command(0x12);
        assert_eq!(d.read_data(), 0);
        d.deselect();
        // A transfer aborted by CS starts over cleanly.
        d.select();
        d.write_command(0x63);
        d.deselect();
        assert_eq!(d.read(0x63, 1), vec![0x40]);
    }

    #[test]
    fn state_round_trip_keeps_the_clock_and_transfer() {
        let mut rtc = Rtc::new();
        rtc.set_unix_time(1_234_567_890);
        let mut d = Driver(&mut rtc);
        d.select();
        d.write_command(0x65);
        let first = d.read_data();
        let saved = rtc.save_state();
        let mut other = Rtc::new();
        other.load_state(&saved);
        let mut d1 = Driver(&mut rtc);
        let mut d2 = Driver(&mut other);
        assert_eq!(d1.read_data(), d2.read_data());
        assert_eq!(first, 0x09);
    }
}
