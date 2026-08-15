pub const BUTTON_A: u8 = 0x01;
pub const BUTTON_B: u8 = 0x02;
pub const BUTTON_SELECT: u8 = 0x04;
pub const BUTTON_START: u8 = 0x08;
pub const BUTTON_RIGHT: u8 = 0x10;
pub const BUTTON_LEFT: u8 = 0x20;
pub const BUTTON_UP: u8 = 0x40;
pub const BUTTON_DOWN: u8 = 0x80;

/// Each bit is 1 when the button is NOT pressed.
pub struct Joypad {
    pub state: u8,
}

impl Joypad {
    pub fn new() -> Joypad {
        Joypad { state: 0xFF }
    }

    pub fn press(&mut self, button: u8) {
        self.state &= !button;
    }

    pub fn release(&mut self, button: u8) {
        self.state |= button;
    }

    /// Read the P1 register given the current select bits.
    pub fn read(&self, p1: u8) -> u8 {
        // bit 4 (P14) = 0 selects the D-pad row, bit 5 (P15) = 0 selects the
        // buttons row. Output lines are shared (open-collector, active low),
        // so when both groups are selected the result is the AND of both rows.
        let buttons = if p1 & 0x20 == 0 { self.state & 0x0F } else { 0x0F };
        let dpad = if p1 & 0x10 == 0 { (self.state >> 4) & 0x0F } else { 0x0F };
        0xC0 | (buttons & dpad)
    }
}

impl Default for Joypad {
    fn default() -> Self {
        Self::new()
    }
}

impl emu_core::device::Device for Joypad {
    fn kind(&self) -> &'static str {
        "Joypad"
    }

    fn reset(&mut self) {
        *self = Joypad::new();
    }

    fn tick(&mut self, _cycles: u32, _bus: &mut dyn emu_core::bus::Bus) {
        // Joypad has no clocked behaviour.
    }
}