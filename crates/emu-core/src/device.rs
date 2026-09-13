//! Clocked hardware device abstraction.

use crate::bus::Bus;

/// A clocked hardware unit attached to a bus (PPU, APU, timer, joypad, ...).
///
/// Devices are advanced by the bus's `tick`, which supplies the shared memory
/// context so devices can read/write system state.
pub trait Device {
    /// Short human-readable device name, e.g. `"PPU"`, `"APU"`, `"Timer"`.
    fn kind(&self) -> &'static str;

    /// Reset the device to its power-on state.
    fn reset(&mut self);

    /// Advance the device by `cycles` master cycles.
    fn tick(&mut self, cycles: u32, bus: &mut dyn Bus);
}
