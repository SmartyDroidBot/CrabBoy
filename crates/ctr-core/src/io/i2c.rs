//! The three I2C controllers and the devices behind them (3dbrew, "I2C
//! Registers").
//!
//! A controller moves one byte per command: `DATA` (+0) holds it and `CNT`
//! (+1) starts the transfer with bit 7, which reads back as busy. Bit 1 sends
//! a start condition first, bit 0 a stop condition afterwards, bit 5 selects
//! reading, bit 6 asks for an interrupt on completion and bit 4 is the
//! acknowledge: reported for a byte written, supplied for a byte read.
//! Transfers complete at once here.
//!
//! The device that matters is the MCU (bus 1, address 0x4A): power, LCD and
//! backlight control, battery, sliders, the real-time clock, and an interrupt
//! line (GPIO3_9, ARM11 interrupt 0x71) for its events. Other devices of the
//! table acknowledge and read as zero.

use crate::arm11::irq;

const BUSES: usize = 3;

const CNT_STOP: u8 = 1 << 0;
const CNT_START: u8 = 1 << 1;
const CNT_ACK: u8 = 1 << 4;
const CNT_READ: u8 = 1 << 5;
const CNT_IRQ: u8 = 1 << 6;
const CNT_BUSY: u8 = 1 << 7;

/// Bus and write address of every device (3dbrew's device table).
const DEVICES: [(u8, u8); 18] = [
    (0, 0x4A),
    (0, 0x7A),
    (0, 0x78),
    (1, 0x4A),
    (1, 0x78),
    (1, 0x2C),
    (1, 0x2E),
    (1, 0x40),
    (1, 0x44),
    (2, 0xD6),
    (2, 0xD0),
    (2, 0xD2),
    (2, 0xA4),
    (2, 0x9A),
    (2, 0xA0),
    (1, 0xEE),
    (0, 0x40),
    (2, 0x54),
];

const MCU: (u8, u8) = (1, 0x4A);

/// MCU registers that do not advance the register pointer.
const NO_AUTO_INCREMENT: [u8; 6] = [0x29, 0x2D, 0x4F, 0x60, 0x61, 0x7F];

/// MCU interrupt events: bits of registers 0x10-0x13.
pub mod mcu_event {
    pub const LCD_OFF: u32 = 1 << 24;
    pub const LCD_ON: u32 = 1 << 25;
    pub const BOTTOM_BACKLIGHT_OFF: u32 = 1 << 26;
    pub const BOTTOM_BACKLIGHT_ON: u32 = 1 << 27;
    pub const TOP_BACKLIGHT_OFF: u32 = 1 << 28;
    pub const TOP_BACKLIGHT_ON: u32 = 1 << 29;
}

/// The microcontroller that runs power, the LCD supplies and the clock.
pub struct Mcu {
    regs: [u8; 0x100],
    events: u32,
    /// A set bit disables that event's interrupt.
    event_mask: u32,
}

impl Default for Mcu {
    fn default() -> Self {
        Self::new()
    }
}

impl Mcu {
    pub fn new() -> Self {
        let mut regs = [0u8; 0x100];
        // The firmware version; an arbitrary value, nothing depends on it.
        regs[0x00] = 0x12;
        regs[0x01] = 0x25;
        regs[0x09] = 0x20; // volume slider, mid travel
        regs[0x0B] = 100; // battery percent
        regs[0x0F] = 1 << 1 | 1 << 3; // shell open, adapter plugged in
                                      // A fixed clock keeps runs reproducible: 2020-01-01 00:00:00, a
                                      // Wednesday, in BCD.
        regs[0x33] = 0x03;
        regs[0x34] = 0x01;
        regs[0x35] = 0x01;
        regs[0x36] = 0x20;
        Mcu {
            regs,
            events: 0,
            event_mask: 0,
        }
    }

    /// Whether the interrupt line is asserted.
    fn interrupting(&self) -> bool {
        self.events & !self.event_mask != 0
    }

    fn read(&mut self, reg: u8) -> u8 {
        match reg {
            0x10..=0x13 => {
                // Reading an event byte clears it.
                let shift = (reg - 0x10) * 8;
                let byte = (self.events >> shift) as u8;
                self.events &= !(0xFF << shift);
                byte
            }
            0x18..=0x1B => (self.event_mask >> ((reg - 0x18) * 8)) as u8,
            _ => self.regs[reg as usize],
        }
    }

    fn write(&mut self, reg: u8, value: u8) {
        match reg {
            // Version, sliders, battery and status are read-only.
            0x00..=0x13 => {}
            0x18..=0x1B => {
                let shift = (reg - 0x18) * 8;
                self.event_mask = self.event_mask & !(0xFF << shift) | (value as u32) << shift;
            }
            0x22 => {
                // Each request bit powers a supply and reports it as an event.
                const REQUESTS: [(u8, u32, u8, bool); 6] = [
                    (1 << 0, mcu_event::LCD_OFF, 1 << 7, false),
                    (1 << 1, mcu_event::LCD_ON, 1 << 7, true),
                    (1 << 2, mcu_event::BOTTOM_BACKLIGHT_OFF, 1 << 5, false),
                    (1 << 3, mcu_event::BOTTOM_BACKLIGHT_ON, 1 << 5, true),
                    (1 << 4, mcu_event::TOP_BACKLIGHT_OFF, 1 << 6, false),
                    (1 << 5, mcu_event::TOP_BACKLIGHT_ON, 1 << 6, true),
                ];
                for (request, event, status, on) in REQUESTS {
                    if value & request != 0 {
                        self.events |= event;
                        if on {
                            self.regs[0x0F] |= status;
                        } else {
                            self.regs[0x0F] &= !status;
                        }
                    }
                }
            }
            _ => self.regs[reg as usize] = value,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Bus {
    data: u8,
    cnt: u8,
    cntex: u16,
    scl: u16,
    /// Write address of the selected device, if it exists.
    device: Option<u8>,
    /// The register pointer, once the first data byte has set it.
    register: Option<u8>,
}

#[derive(Default)]
pub struct I2c {
    buses: [Bus; BUSES],
    pub mcu: Mcu,
    mcu_line: bool,
}

impl I2c {
    pub fn new() -> Self {
        I2c::default()
    }

    /// Read the word at `offset` of bus `bus`.
    pub fn read(&self, bus: usize, offset: u32) -> u32 {
        let b = &self.buses[bus];
        match offset {
            0 => b.data as u32 | (b.cnt as u32) << 8 | (b.cntex as u32) << 16,
            4 => b.scl as u32,
            _ => 0,
        }
    }

    /// Write the byte lanes of `mask`. Interrupts to raise on the ARM11 are
    /// pushed to `irqs`.
    pub fn write(&mut self, bus: usize, offset: u32, value: u32, mask: u32, irqs: &mut Vec<usize>) {
        match offset {
            0 => {
                if mask & 0xFF != 0 {
                    self.buses[bus].data = value as u8;
                }
                if mask & 0xFFFF_0000 != 0 {
                    self.buses[bus].cntex = (value >> 16) as u16;
                }
                if mask & 0xFF00 != 0 {
                    self.command(bus, (value >> 8) as u8, irqs);
                }
            }
            4 if mask & 0xFFFF != 0 => self.buses[bus].scl = value as u16,
            _ => {}
        }
    }

    fn command(&mut self, bus: usize, cnt: u8, irqs: &mut Vec<usize>) {
        if cnt & CNT_BUSY == 0 {
            self.buses[bus].cnt = cnt;
            return;
        }
        let mut ack = true;
        if cnt & CNT_READ != 0 {
            let byte = self.device_read(bus);
            self.buses[bus].data = byte;
        } else if cnt & CNT_START != 0 {
            // The address byte; bit 0 selects reading and keeps the pointer.
            let byte = self.buses[bus].data;
            let address = byte & !1;
            let exists = DEVICES.contains(&(bus as u8, address));
            self.buses[bus].device = exists.then_some(address);
            if byte & 1 == 0 {
                self.buses[bus].register = None;
            }
            ack = exists;
        } else {
            ack = self.device_write(bus);
        }
        if cnt & CNT_STOP != 0 {
            self.buses[bus].device = None;
        }

        // Busy clears at once; a written byte reports its acknowledge.
        let mut done = cnt & !CNT_BUSY;
        if cnt & CNT_READ == 0 {
            done = done & !CNT_ACK | if ack { CNT_ACK } else { 0 };
        }
        self.buses[bus].cnt = done;
        if cnt & CNT_IRQ != 0 {
            irqs.push([irq::I2C_BUS_0, irq::I2C_BUS_1, irq::I2C_BUS_2][bus]);
        }
        self.update_mcu_line(irqs);
    }

    fn device_read(&mut self, bus: usize) -> u8 {
        let b = self.buses[bus];
        let (Some(device), Some(register)) = (b.device, b.register) else {
            return 0;
        };
        let byte = if (bus as u8, device) == MCU {
            self.mcu.read(register)
        } else {
            0
        };
        self.advance(bus, device, register);
        byte
    }

    fn device_write(&mut self, bus: usize) -> bool {
        let b = self.buses[bus];
        let Some(device) = b.device else {
            return false;
        };
        match b.register {
            None => self.buses[bus].register = Some(b.data),
            Some(register) => {
                if (bus as u8, device) == MCU {
                    self.mcu.write(register, b.data);
                }
                self.advance(bus, device, register);
            }
        }
        true
    }

    fn advance(&mut self, bus: usize, device: u8, register: u8) {
        let fixed = (bus as u8, device) == MCU && NO_AUTO_INCREMENT.contains(&register);
        if !fixed {
            self.buses[bus].register = Some(register.wrapping_add(1));
        }
    }

    /// The MCU interrupt is an edge on its line.
    fn update_mcu_line(&mut self, irqs: &mut Vec<usize>) {
        let line = self.mcu.interrupting();
        if line && !self.mcu_line {
            irqs.push(irq::MCU);
        }
        self.mcu_line = line;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A driver's register read: select, register, reselect for reading,
    /// then the bytes with a stop on the last.
    fn read_reg(i2c: &mut I2c, bus: usize, address: u8, reg: u8, out: &mut [u8]) -> bool {
        let mut irqs = Vec::new();
        let mut step = |i2c: &mut I2c, data: Option<u8>, cnt: u8| {
            if let Some(byte) = data {
                i2c.write(bus, 0, byte as u32, 0xFF, &mut irqs);
            }
            i2c.write(bus, 0, (cnt as u32) << 8, 0xFF00, &mut irqs);
            i2c.read(bus, 0)
        };
        if step(i2c, Some(address), 0xC2) >> 8 & CNT_ACK as u32 == 0 {
            return false;
        }
        step(i2c, Some(reg), 0xC0);
        step(i2c, Some(address | 1), 0xC2);
        let last = out.len() - 1;
        for (n, byte) in out.iter_mut().enumerate() {
            *byte = step(i2c, None, if n == last { 0xE1 } else { 0xF0 }) as u8;
        }
        true
    }

    fn write_reg(i2c: &mut I2c, bus: usize, address: u8, reg: u8, bytes: &[u8]) -> Vec<usize> {
        let mut irqs = Vec::new();
        let mut step = |i2c: &mut I2c, data: u8, cnt: u8| {
            i2c.write(bus, 0, data as u32, 0xFF, &mut irqs);
            i2c.write(bus, 0, (cnt as u32) << 8, 0xFF00, &mut irqs);
        };
        step(i2c, address, 0xC2);
        step(i2c, reg, 0xC0);
        let last = bytes.len() - 1;
        for (n, byte) in bytes.iter().enumerate() {
            step(i2c, *byte, if n == last { 0xC1 } else { 0xC0 });
        }
        // Every step also asks for the bus completion interrupt.
        irqs.retain(|id| *id == irq::MCU);
        irqs
    }

    #[test]
    fn reads_auto_increment_through_the_mcu_registers() {
        let mut i2c = I2c::new();
        let mut version = [0u8; 2];
        assert!(read_reg(&mut i2c, 1, 0x4A, 0x00, &mut version));
        assert_eq!(version, [0x12, 0x25]);
        let mut battery = [0u8; 1];
        read_reg(&mut i2c, 1, 0x4A, 0x0B, &mut battery);
        assert_eq!(battery, [100]);
    }

    #[test]
    fn an_absent_device_does_not_acknowledge() {
        let mut i2c = I2c::new();
        assert!(!read_reg(&mut i2c, 0, 0x10, 0, &mut [0]));
        assert!(read_reg(&mut i2c, 1, 0x2C, 0x40, &mut [0, 0]));
    }

    #[test]
    fn lcd_power_requests_become_events_and_an_interrupt() {
        let mut i2c = I2c::new();
        let irqs = write_reg(&mut i2c, 1, 0x4A, 0x22, &[0x02]);
        assert_eq!(irqs, [irq::MCU]);
        let mut events = [0u8; 4];
        read_reg(&mut i2c, 1, 0x4A, 0x10, &mut events);
        assert_eq!(u32::from_le_bytes(events), mcu_event::LCD_ON);
        read_reg(&mut i2c, 1, 0x4A, 0x10, &mut events);
        assert_eq!(events, [0; 4], "reading clears the events");

        let irqs = write_reg(&mut i2c, 1, 0x4A, 0x22, &[0x28]);
        assert_eq!(irqs, [irq::MCU], "the line rose again");
        let mut status = [0u8; 1];
        read_reg(&mut i2c, 1, 0x4A, 0x0F, &mut status);
        assert_eq!(status[0] & 0xE0, 0xE0, "panel and both backlights on");
    }

    #[test]
    fn masked_events_do_not_interrupt() {
        let mut i2c = I2c::new();
        write_reg(&mut i2c, 1, 0x4A, 0x18, &[0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(write_reg(&mut i2c, 1, 0x4A, 0x22, &[0x02]).is_empty());
    }

    #[test]
    fn completion_interrupts_when_asked() {
        let mut i2c = I2c::new();
        let mut irqs = Vec::new();
        i2c.write(2, 0, 0xD0, 0xFF, &mut irqs);
        i2c.write(2, 0, 0xC2 << 8, 0xFF00, &mut irqs);
        assert_eq!(irqs, [irq::I2C_BUS_2]);
    }
}
