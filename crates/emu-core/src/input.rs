//! Shared logical button identifiers, used across consoles.
//!
//! Each `System` maps these to its own bitmask internally, so a frontend can
//! use one uniform set of inputs for every console it hosts.

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Button {
    A,
    B,
    X,
    Y,
    L,
    R,
    ZL,
    ZR,
    Start,
    Select,
    Up,
    Down,
    Left,
    Right,
}

impl Button {
    /// All buttons, in a stable order (useful for iterating a keymap).
    pub const ALL: [Button; 14] = [
        Button::A,
        Button::B,
        Button::X,
        Button::Y,
        Button::L,
        Button::R,
        Button::ZL,
        Button::ZR,
        Button::Start,
        Button::Select,
        Button::Up,
        Button::Down,
        Button::Left,
        Button::Right,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Button::A => "A",
            Button::B => "B",
            Button::X => "X",
            Button::Y => "Y",
            Button::L => "L",
            Button::R => "R",
            Button::ZL => "ZL",
            Button::ZR => "ZR",
            Button::Start => "Start",
            Button::Select => "Select",
            Button::Up => "Up",
            Button::Down => "Down",
            Button::Left => "Left",
            Button::Right => "Right",
        }
    }
}

/// A two-axis analog control. Cores without one ignore it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Axis {
    /// The primary analog pad (the 3DS circle pad).
    CirclePad,
    /// The secondary analog nub (the New 3DS C-stick).
    CStick,
}

/// Accelerometer and gyroscope readings in raw sensor units, `[x, y, z]`.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Motion {
    pub accel: [i16; 3],
    pub gyro: [i16; 3],
}
