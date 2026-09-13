# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- GBA: EEPROM (512 B / 8 KB) save memory with size detection from the DMA
  transfer length, and save-type detection from the ROM identifier string.
- GBA: the full BIOS SWI table as high-level emulation, integer-only (resets,
  IntrWait, decompression, unfilters, BitUnPack, affine set, ArcTan,
  MidiKey2Freq, sound bias).
- GBA: S-3511A real-time clock on the cartridge GPIO port.
- GBA: save states carry timers, DMA, APU, EEPROM, RTC and GPIO state
  (format version 5).
- CLI: `gba-diag` headless runner (frame sampling, wild-PC detection, PNG
  and VRAM dumps, scripted input, tracing) and `gba-disasm`.
- Desktop: window sized for the 240x160 GBA display.

### Fixed
- GBA CPU: `bx` target alignment, Thumb decoder formats and push order, BL
  offset sign, exception-return CPSR restore, MSR field masks, ARM SWI number
  decode, user/system banked registers, LDRSH at odd addresses, memory-access
  cycle counts.
- GBA I/O and bus: IME at 0x208, IME gating of interrupts, DISPSTAT write
  mask, live byte reads, unaligned load rotation, WAITCNT timing, HALTCNT,
  VRAM mirroring (upper character and screen blocks no longer alias the
  first 32 KB).
- GBA DMA/timers: CNT_H decode, latching, repeat reload, DMA3 16-bit count,
  timer reload semantics, cascade chains, edge-triggered timer/DMA interrupts,
  HBlank DMA restricted to visible lines, FIFO refill destination check.
- GBA PPU: layer priority order, BLDCNT effect bits, second blend target and
  window effect bits.
- GBA boot: skip-BIOS register state matches the BIOS hand-off; IRQ dispatch
  and IntrWait follow the BIOS sequence with a return stub.

### Changed
- License is now AGPL-3.0-or-later (was MIT).
- Workspace formatted with rustfmt; generated wasm output and a save-RAM dump
  are no longer tracked.
