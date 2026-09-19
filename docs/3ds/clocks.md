# 3DS clocks

Constants live in `crates/ctr-core/src/clock.rs`. Any change to them is a
timing change (`guidelines.md`, section 3).

## Processors

| Clock | Rate | Source |
|---|---|---|
| ARM11 MPCore | 268,111,856 Hz | 3dbrew, "Hardware" |
| ARM9 | 134,055,928 Hz, exactly half | 3dbrew, "Hardware" |

The master timeline counts ARM11 cycles.

## LCD

3dbrew ("GPU/External Registers") gives the values `nngxInitialize` programs
for the top screen, `HTotal` = 450 and `VTotal` = 413, and one measured data
point: `VTotal` = 494 lowers the refresh rate to about 50.040660858 Hz.

That data point fixes the model. With a pixel clock of the ARM11 clock divided
by 24, and both counters running through *total + 1* values:

    268,111,856 / (24 x 451 x 495) = 50.0406608... Hz

No other small divider or counting convention reproduces the figure. The
default timing is therefore:

    24 x 451 x 414 = 4,481,136 ARM11 cycles per frame
    268,111,856 / 4,481,136 = 59.8312... Hz

`clock.rs` has a unit test for both numbers.

## Audio

The DSP produces 32,728 Hz stereo (3dbrew, "Hardware").

## Not yet measured

Latencies of GPU command lists, memory fills, display transfers, DMA, PXI and
SDMMC are not publicly documented. They start as coarse constants recorded
here and are refined from measurements when a console is available.

Constants in use, none of them measured:

| What | Cost | Why this value |
|---|---|---|
| GPU memory fill (PSC0, PSC1) | 1 ARM11 cycle per byte filled, at least one | Software starts a fill and only then arms its wait for the completion interrupt (fastboot3DS clears its event flag after the write), so completion has to come later than the starting write. The memory itself is written at once. |
| ARM9 DMA (NDMA) | none: a block moves at once | No payload has needed more yet. |
