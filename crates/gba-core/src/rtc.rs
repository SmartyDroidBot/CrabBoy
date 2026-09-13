//! GBA RTC (Sanyo real-time clock), accessed via the serial I/O port.
//!
//! The RTC is bit-banged through the low three bits of `SIODATA` (0x120):
//! bit 0 = SIO (data), bit 1 = SCI (clock), bit 2 = SIC (chip select).
//!
//! Protocol: on a rising SIC edge the transfer is reset; then 8 command bits
//! (MSB-first) are clocked in on SCI rising edges. The command byte encodes
//! `address << 1 | rw`. Reads then output `length` data bytes on SIO; writes
//! clock in `length` data bytes.

use std::time::SystemTime;

/// RTC register lengths (in bytes) for the supported commands.
fn length_for(address: u8) -> usize {
    match address {
        0x08 => 1, // Status (read)
        0x09 => 7, // Time (read)
        0x0A => 8, // DateTime (read)
        0x0B => 8, // Time (read, alt)
        0x0C => 7, // Time (write)
        0x0D => 8, // DateTime (write)
        _ => 0,
    }
}

/// State of the bit-bang protocol.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Command,
    Data,
    Done,
}

/// The GBA real-time clock.
pub struct Rtc {
    phase: Phase,
    bits: u8,
    command: u8,
    /// Remaining bytes to transfer.
    remaining: usize,
    /// Buffered data bytes (for reads) or received bytes (for writes).
    buf: [u8; 8],
    buf_pos: usize,
    /// When reading, the byte currently being shifted out.
    out: u8,
    /// Bit position within `out` currently being driven out (0..7).
    out_bit: u8,
    /// Whether the game wants a read (R/W=0) or write (R/W=1).
    rw: bool,
    /// The RTC state byte returned by the status register.
    status: u8,
    /// Last known SIC state (for edge detection).
    last_sic: bool,
    /// Last known SCI state (for edge detection).
    sci: bool,
}

impl Default for Rtc {
    fn default() -> Self {
        Rtc {
            phase: Phase::Command,
            bits: 0,
            command: 0,
            remaining: 0,
            buf: [0; 8],
            buf_pos: 0,
            out: 0,
            out_bit: 0,
            rw: false,
            status: 0x40, // initialized, valid time
            last_sic: true,
            sci: false,
        }
    }
}

impl Rtc {
    pub fn new() -> Rtc {
        Rtc::default()
    }

    /// Current time fields (day, hour, minute, second, month, year).
    fn now(&self) -> (u8, u8, u8, u8, u8, u8) {
        let s = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let days = s / 86400;
        let secs = s % 86400;
        let year = (days / 365) as u8;
        let month = 1 + ((days % 365) / 30) as u8;
        let day = ((days % 365) % 30 + 1) as u8;
        let hour = (secs / 3600) as u8;
        let minute = ((secs % 3600) / 60) as u8;
        let second = (secs % 60) as u8;
        (day, hour, minute, second, month, year)
    }

    fn build_response(&mut self, address: u8) {
        let (day, hour, minute, second, month, year) = self.now();
        let buf = &mut self.buf;
        buf[0] = 0;
        buf[1] = 0;
        buf[2] = 0;
        buf[3] = 0;
        buf[4] = 0;
        buf[5] = 0;
        buf[6] = 0;
        buf[7] = 0;
        match address {
            0x08 => buf[0] = self.status,
            0x09 => {
                // Time: hour, min, sec, day, month, year, week.
                buf[0] = hour;
                buf[1] = minute;
                buf[2] = second;
                buf[3] = day;
                buf[4] = month;
                buf[5] = year;
                buf[6] = 0;
            }
            0x0A | 0x0B => {
                // DateTime/Time: sec, min, hour, day, date, month, year, week.
                buf[0] = second;
                buf[1] = minute;
                buf[2] = hour;
                buf[3] = day;
                buf[4] = day;
                buf[5] = month;
                buf[6] = year;
                buf[7] = 0;
            }
            _ => {}
        }
        self.remaining = length_for(address);
        self.buf_pos = 0;
        self.out = buf[0];
    }

    /// Write the low 16 bits of `SIODATA` (0x120); the game drives the pins.
    pub fn write_sio(&mut self, value: u16) {
        let sic = value & 0x04 != 0;
        let sci = value & 0x02 != 0;
        let sio = value & 0x01 != 0;
        let sci_rising = sci && !self.sci;
        self.sci = sci;
        // Chip-select edge (falling or rising) resets the transfer.
        if sic != self.last_sic {
            self.last_sic = sic;
            if sic {
                self.reset_transfer();
            }
            return;
        }
        if !sic {
            return;
        }
        if !sci_rising {
            return;
        }
        match self.phase {
            Phase::Command => {
                self.command = (self.command << 1) | sio as u8;
                self.bits += 1;
                if self.bits == 8 {
                    self.rw = self.command & 1 == 1;
                    let address = self.command >> 1;
                    self.phase = Phase::Data;
                    self.bits = 0;
                    self.remaining = length_for(address);
                    if !self.rw {
                        // Reset / force reset produce no output.
                        if address == 0x06 || address == 0x07 {
                            self.phase = Phase::Done;
                        } else {
                            self.build_response(address);
                        }
                    }
                }
            }
            Phase::Data => {
                if self.remaining == 0 {
                    self.phase = Phase::Done;
                    return;
                }
                if self.rw {
                    // Shift in a data bit into buf[buf_pos].
                    self.buf[self.buf_pos] = (self.buf[self.buf_pos] << 1) | sio as u8;
                    self.bits += 1;
                    if self.bits == 8 {
                        self.bits = 0;
                        self.buf_pos += 1;
                        self.remaining -= 1;
                        if self.remaining == 0 {
                            self.apply_write();
                            self.phase = Phase::Done;
                        }
                    }
                } else {
                    // Clock out a bit; advance every 8 clocks.
                    self.out_bit += 1;
                    if self.out_bit == 8 {
                        self.out_bit = 0;
                        self.buf_pos += 1;
                        self.remaining -= 1;
                        self.out = self.buf.get(self.buf_pos).copied().unwrap_or(0);
                    }
                }
            }
            Phase::Done => {}
        }
    }

    fn apply_write(&mut self) {
        // For simplicity we accept the write and mark time valid.
        self.status = 0x40;
    }

    fn reset_transfer(&mut self) {
        self.phase = Phase::Command;
        self.bits = 0;
        self.command = 0;
        self.buf_pos = 0;
        self.remaining = 0;
    }

    /// The RTC output pin (SIO bit 0) driven back to the game during reads.
    pub fn read_sio_bit(&self) -> bool {
        if self.phase == Phase::Data && !self.rw && self.remaining > 0 {
            ((self.out >> (7 - self.out_bit)) & 1) == 1
        } else {
            false
        }
    }

    /// Current SCI pin state (tracked for edge detection).
    pub fn sci(&self) -> bool {
        self.sci
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_read_returns_initialized() {
        let mut rtc = Rtc::new();
        // Command byte for Status read: address 0x08 << 1 | 0 = 0x10, MSB-first.
        let cmd = 0x10u8;
        // Reset via SIC rising, then clock 8 command bits.
        let mut val = 0u16;
        rtc.write_sio(val); // SIC low initially
        val |= 0x04; // SIC high
        rtc.write_sio(val);
        for i in (0..8).rev() {
            let bit = (cmd >> i) & 1;
            val = (val & !0x03) | (bit as u16) | 0x02;
            rtc.write_sio(val);
            val &= !0x02;
            rtc.write_sio(val);
        }
        // After 8 bits the read is in data phase; remaining=1, out=status(0x40).
        // Clock 8 output bits.
        let mut result = 0u8;
        for _ in 0..8 {
            let bit = rtc.read_sio_bit() as u8;
            result = (result << 1) | bit;
            val |= 0x02;
            rtc.write_sio(val);
            val &= !0x02;
            rtc.write_sio(val);
        }
        assert_eq!(result, 0x40);
    }
}
