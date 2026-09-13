# GBA DMA — verified reference

Sources: GBATEK (DMA section) and mGBA (`src/gba/dma.c`, `include/mgba/internal/gba/dma.h`).
These notes describe what the hardware does; `gba-core/src/dma.rs` and
`Bus::run_dma` follow them.

## Register map

Four DMA channels. Each channel is **12 bytes** (`0xC`) wide, starting at its SAD
(source address) register. `BASE` is the SAD base of each channel:

| Channel | SAD   | DAD   | CNT_L | CNT_H |
|---------|-------|-------|-------|-------|
| DMA0    | 0xB0  | 0xB4  | 0xB8  | 0xBA  |
| DMA1    | 0xBC  | 0xC0  | 0xC4  | 0xC6  |
| DMA2    | 0xC8  | 0xCC  | 0xD0  | 0xD2  |
| DMA3    | 0xD4  | 0xD8  | 0xDC  | 0xDE  |

A channel's registers span `+0x0..=+0xA` (12 bytes).

`DMA3CNT_H` is at `0x040000DE`.

## CNT_H (control high, written as the upper 16 bits of the 32-bit control word)

| Bit(s) | Meaning |
|--------|---------|
| 15     | Enable (`DMA_ENABLE`). Start on write when timing = "Now". |
| 14     | IRQ: request an IRQ when the transfer completes. |
| 12-13  | Start timing: 0 = immediately, 1 = VBlank, 2 = HBlank, 3 = Special (only DMA3 valid). |
| 11     | DRQ (only DMA3; game-pak DRQ). Must be 0 for the other channels. |
| 10     | **Width: 1 = 32-bit, 0 = 16-bit.** (`DMA_32 = 1 << 10` in pokeemerald `macro.h`.) |
| 9      | Repeat: re-start the transfer on every frame/HBlank event. Must be 0 when timing = now. |
| 8-7    | Src address adjustment (see below). |
| 6-5    | Dst address adjustment (see below). |

Decodes:

- width  → `(cnt >> 10) & 1`
- timing → `(cnt >> 12) & 3`
- repeat → `1 << 9`

## Address adjustment

mGBA `DMA_OFFSET[] = { 1, -1, 0, 1 }` applied per word/halfword:

| code | adjustment |
|------|-----------|
| 0    | increment (`+1`) |
| 1    | decrement (`-1`) |
| 2    | fixed (`+0`) |
| 3    | increment-reload (`+1`, reload on repeat) |

Address masks (mGBA):
`DMA_SRC_MASK[] = { 0x07FFFFFE, 0x0FFFFFFE, 0x0FFFFFFE, 0x0FFFFFFE }`
`DMA_DST_MASK[] = { 0x07FFFFFE, 0x07FFFFFE, 0x07FFFFFE, 0x0FFFFFFE }`

(DMA0 is restricted to 16MB for src and dst; the others to 32MB.)

## Count

- The count is the low 16 bits (`CNT_L`), plus the upper 16 bits written as part
  of the control word when doing 32-bit writes — i.e. the full 32-bit write of
  `(CNT_H << 16) | CNT_L` supplies both fields.
- DMA0-2: count masked to 14 bits (`0x3FFF`); a value of 0 means 0x4000 words.
- DMA3:   full 16-bit count; a value of 0 means 0x10000 words.

## Start

Writing `CNT_H` with bit 15 set (and timing = "now") begins the transfer
immediately; `DMA3CNT_H` at 0xDE is the last I/O write a game does to kick off a
DMA.

## Emerald uses this to install its IRQ handler

`InitIntrHandlers` (Emerald, ~0x08000684):

```c
DmaCopy32(3, IntrMain, IntrMain_Buffer, sizeof(IntrMain_Buffer));
INTR_VECTOR = IntrMain_Buffer;
```

- `IntrMain_Buffer` is `COMMON_DATA u32[0x200]` in IWRAM at **0x03002750**.
- `sizeof(IntrMain_Buffer)` = 0x800 bytes = 0x200 32-bit words, so the DMA count
  is 0x200 and the control word is `(DMA_ENABLE | DMA_START_NOW | DMA_32 | INC_SRC
  | INC_DST) << 16 | 0x200`.
- `IntrMain` lives in ROM at **0x08000248**; it is **ARM** code.
- `INTR_VECTOR` is at **0x03007FFC** (GBATEK). The IRQ vector 0x18 reads it and
  branches to the copied handler.
- The traced copy: SAD = 0x08000248, DAD = 0x03002750, CNT_L = 0x0200,
  CNT_H = 0x8400 (enable + 32-bit).
## Latching

The source, destination and count registers are latched into internal
pointers on the 0 -> 1 transition of the enable bit (masked to the widths
above), so later register writes do not disturb a running channel. After a
repeating transfer the count is reloaded, and the destination too when the
adjustment mode is increment/reload (3). A one-shot transfer clears the
enable bit in `CNT_H` when it finishes. HBlank-timed channels run on the 160
visible lines only.

## Sound FIFOs

DMA1/DMA2 in special timing mode with the destination at 0x040000A0 or
0x040000A4 feed the DirectSound FIFOs: when the timer selected in SOUNDCNT_H
overflows and the FIFO holds 16 bytes or fewer, four words are transferred
from the channel's current source.

## EEPROM

DMA3 to or from 0x0D000000 talks to the EEPROM one bit per halfword. The
transfer length reveals the part size: 9 or 73 halfwords for the 512-byte
part, 17 or 81 for the 8 KB part (see `save.rs` and `eeprom.rs`).
