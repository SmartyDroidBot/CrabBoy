# GBA I/O — interrupt, key and system registers

Source: GBATEK. Offsets are within the 0x04000000 window.

## Interrupt registers

| Register | Offset | Notes |
|---|---|---|
| IE | 0x200 | Interrupt enable mask (bits 0-13: VBlank, HBlank, VCount, Timer0-3, Serial, DMA0-3, Keypad, GamePak) |
| IF | 0x202 | Interrupt flags; set by hardware, cleared by writing a 1 to a bit |
| WAITCNT | 0x204 | Wait-state control (WS0 non-sequential/sequential bits 2-4 drive the ROM timing) |
| IME | 0x208 | Master enable |

Byte reads of these registers, of VCOUNT and of the timer counters see the
live values; the raw register file is only the backing store for registers
without special behaviour.

## Display status

`DISPSTAT` (0x004): bits 0-2 (VBlank, HBlank, VCount match) are read-only
status flags and are masked out of writes; bits 3-5 enable the three LCD
interrupts, bits 8-15 hold the VCount setting. The VBlank flag is set on lines
160-226 and clear on line 227.

## Key input

| Register | Offset | Notes |
|---|---|---|
| KEYINPUT | 0x130 | Current button state, active low (bit 0 A, 1 B, 2 SELECT, 3 START, 4 RIGHT, 5 LEFT, 6 UP, 7 DOWN, 8 R, 9 L) |
| KEYCNT | 0x132 | Keypad IRQ selection (bit 14 enable, bit 15 AND mode) |

## System

| Register | Offset | Notes |
|---|---|---|
| POSTFLG | 0x300 | 1 after the first boot; the skip-BIOS boot sets it |
| HALTCNT | 0x301 | A write parks the CPU until an enabled interrupt (bit 7 selects STOP, treated the same) |

## Power-on values

The BIOS leaves DISPCNT = 0x0080, BG2PA/PD = BG3PA/PD = 0x100, SOUNDBIAS =
0x200 and RCNT = 0x8000; `Io::default` reproduces them.
