//! Addressable memory and bus abstraction.

use std::any::Any;

/// A unit that can be read from / written to over a bus.
///
/// Addresses are `u32` so the trait works for both 16-bit (GB) and 32-bit
/// (GBA) address spaces; 16-bit systems simply cast down.
pub trait Addressable {
    fn read(&self, addr: u32) -> u8;
    fn write(&mut self, addr: u32, value: u8);
}

/// A system memory bus that routes addresses to attached devices and drives
/// their clock.
///
/// A `Bus` is also `Addressable` so a CPU can execute against `&mut dyn Bus`.
pub trait Bus: Addressable {
    /// Advance the whole machine (all attached devices) by `cycles` master
    /// cycles. Returns nothing; state changes happen on the bus itself.
    fn tick(&mut self, cycles: u32);

    /// Request an interrupt by its system-defined bit index.
    fn request_interrupt(&mut self, bit: u32);

    /// Downcast support so concrete systems can reach their own device
    /// internals when needed.
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
