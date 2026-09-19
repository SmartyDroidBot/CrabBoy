//! Clock constants. The derivations are in `docs/3ds/clocks.md`.

/// ARM11 MPCore clock in Hz. The master timeline counts these cycles; the
/// ARM9 runs at exactly half this rate.
pub const ARM11_HZ: u32 = 268_111_856;

/// ARM11 cycles per ARM9 cycle.
pub const ARM9_CYCLE: u32 = 2;

/// ARM11 cycles per count of an ARM9 timer with no prescaler: the timers run
/// at 67,027,964 Hz, half the ARM9 clock (3dbrew, "TIMER Registers").
pub const ARM9_TIMER_CYCLES: u64 = 4;

/// ARM11 cycles a GPU memory fill takes per byte. Not measured: a coarse
/// constant that keeps completion after the write that starts the fill.
pub const PSC_FILL_CYCLES_PER_BYTE: u64 = 1;

/// ARM11 cycles per LCD pixel.
pub const CYCLES_PER_PIXEL: u32 = 24;

/// Pixels per scanline with the timing every known firmware programs
/// (`HTotal` = 450, and the counter runs through `HTotal + 1` values).
pub const LINE_PIXELS: u32 = 451;

/// Scanlines per frame (`VTotal` = 413, likewise plus one).
pub const FRAME_LINES: u32 = 414;

/// ARM11 cycles in one video frame: 4,481,136, about 59.831 Hz.
pub const FRAME_CYCLES: u32 = CYCLES_PER_PIXEL * LINE_PIXELS * FRAME_LINES;

/// ARM11 cycles each processor runs before the next one takes its turn.
/// Cross-processor effects become visible at these boundaries, so the value
/// is part of the timing model.
pub const QUANTUM: u32 = 64;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_4481136_cycles() {
        assert_eq!(FRAME_CYCLES, 4_481_136);
    }

    /// 3dbrew records that `VTotal` = 494 gives about 50.040660858 Hz, which
    /// pins both the pixel divider and the plus-one counting.
    #[test]
    fn the_model_reproduces_the_documented_50hz_mode() {
        let cycles = (CYCLES_PER_PIXEL * LINE_PIXELS * 495) as u64;
        let millihertz_x1000 = ARM11_HZ as u64 * 1_000_000 / cycles;
        assert_eq!(millihertz_x1000, 50_040_660);
    }
}
