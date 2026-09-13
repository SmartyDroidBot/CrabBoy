# GBA cartridge RTC (S-3511A)

Pokémon Ruby, Sapphire and Emerald carry a Seiko S-3511A real-time clock
behind the cartridge GPIO port (GBATEK "GBA Cart I/O Port" and "GBA Cart
Real-Time Clock"). `gba-core/src/rtc.rs` implements it; `bus.rs` owns the
port.

## GPIO port

| Address | Register | Notes |
|---|---|---|
| 0x080000C4 | Data | Bits 0-3; bit 0 = SCK, bit 1 = SIO, bit 2 = CS for the RTC |
| 0x080000C6 | Direction | 1 = driven by the GBA, 0 = input |
| 0x080000C8 | Control | Bit 0 = 1 makes the three registers readable; otherwise the addresses read as ROM |

Writes to the data register drive the output pins to the chip; reads of the
data register show the GBA's outputs plus the level the chip drives on any
input pin.

## Protocol

- CS rising starts a transfer; CS low aborts it.
- Every SCK rising edge moves one bit on SIO.
- The command byte is sent MSB first and must be `0110 rrr w` (0x6X):
  register 0 reset, 1 status, 2 date/time, 3 time, 4 alarm; low bit 1 = read.
- Data bytes are LSB first: status 1 byte; date/time 7 bytes (yy mm dd
  weekday hh mm ss, BCD, weekday 0 = Sunday); time 3 bytes; alarm 2 bytes.
- The status register: bit 6 = 24-hour mode, bit 7 = power-failure flag
  (cleared by writing the register or by reset), bits 1/3/5 interrupt
  settings. The core starts in 24-hour mode with no power failure, which the
  games accept as a healthy cartridge.
- Reset returns the clock to 2000-01-01 00:00:00.

The game library (`siirtc.c` in the decompilations) selects the chip with
SCK high and CS low, raises CS, clocks the command out with SIO as an output,
then flips SIO to an input for the reply.

## Time base

The clock counts emulated cycles (16,777,216 per second) from a fixed
2000-01-01 epoch, so a run is reproducible. Frontends can seed it from the
host clock with `Rtc::set_unix_time`; the value is part of the save state.
