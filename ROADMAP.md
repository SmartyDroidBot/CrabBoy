# Roadmap

The end goal is hardware-equivalent emulation of the Game Boy, Game Boy Color
and Game Boy Advance in pure Rust. Each milestone ships as a GitHub release
built for Windows and Linux on amd64 and arm64, plus a WebAssembly build with
a minimal browser demo. Accuracy-suite results per release live in
`docs/accuracy.md` once the harness exists.

| Release | Milestone |
|---|---|
| v0.1.0 | GBA reaches the Emerald/Ruby title screen; GB/GBC/GBA on desktop, CLI and wasm; multi-arch release pipeline and browser demo |
| v0.2.0 | GB/GBC accuracy harness (blargg, mooneye, dmg-acid2, cgb-acid2, mealybug, SameSuite) with baseline results |
| v0.3.0 | GB M-cycle CPU: blargg cpu_instrs, instr_timing, mem_timing(-2), halt_bug pass; mooneye acceptance ≥ 90% excluding ppu/ |
| v0.4.0 | GB pixel-FIFO PPU: dmg-acid2 and cgb-acid2 pixel-exact, mealybug ≥ 50% |
| v0.5.0 | GB APU: blargg dmg_sound and cgb_sound 12/12, integer audio path |
| v0.6.0 | GB cartridges and CGB details: mooneye mbc*, rtc3test, bully |
| v0.7.0+ | GBA scheduler, prefetch and wait states, jsmolka gba-tests / FuzzARM / mGBA suite conformance |

## GBA accuracy backlog (after v0.1.0)

Ordered by impact on commercial games:

1. Event scheduler with sub-scanline HBlank (IRQ/DMA at dot 240), timer and
   DMA events; affine reference registers latched when written, not only at
   VBlank.
2. Title-screen and menu rendering bugs seen in Pokémon (background layer
   colours, missing large sprites, garbled text). See `docs/gba/verification.md`.
3. Cartridge prefetch buffer and per-region wait states (WS0/WS1/WS2,
   sequential vs non-sequential, EWRAM), DMA cycle stealing.
4. Save conformance: Flash manufacturer IDs and sector erase per part, SRAM
   8-bit bus replication, EEPROM timing.
5. Memory quirks: VRAM/PALRAM byte-write duplication, OAM byte writes ignored,
   out-of-range ROM reads, open bus from the prefetched opcode.
6. PPU: OBJ cycle budget, mid-scanline register latches, mosaic corner cases.
7. CPU conformance on jsmolka arm.gba/thumb.gba: empty register lists, STM
   with the base in the list, PC-relative reads of +12, MUL flags.
8. BIOS exactness against a real image (oracle tests), STOP wake rules.
9. Serial I/O (link cable, JOYBUS), other GPIO peripherals (solar, gyro, rumble).

## GB/GBC accuracy backlog

Tracked per milestone above; the structural work is stepping the bus per
M-cycle inside instructions, a fetcher/FIFO PPU with mode-3 penalties and a
DIV-clocked frame sequencer with integer mixing in the APU.
