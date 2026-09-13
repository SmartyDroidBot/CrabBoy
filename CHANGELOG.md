# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- `crab-systems` crate: detects the console from the ROM header (never the
  file extension) and builds any core behind `Box<dyn System>`, with an
  optional user-supplied GBA BIOS image and cold/warm boot choice.
- `emu-core`: `System::title`, `System::screen`, `System::frame_rate`, the
  `DMG_PALETTE` and `Frame::write_rgba`/`to_rgba` helpers.
- CLI: `crab info` and `crab run` headless runner for GB, GBC and GBA with
  scripted input, PNG/PPM frame dumps, WAV capture, battery saves and an
  FNV-1a-32 frame hash; `fetch_test_roms` downloads the pinned open-source
  accuracy suites (c-sp game-boy-test-roms v7.0, jsmolka gba-tests).
- wasm: system-agnostic `Emulator` binding compiled for every wasm32 build,
  and a browser demo with audio (AudioWorklet), keyboard, gamepad and touch
  input, IndexedDB battery saves and quick-save, drag-and-drop and
  `?rom=<url>` auto-load.
- Desktop: `--bios <path>` (or `gba_bios.bin` next to the ROM/executable)
  boots the GBA through a real BIOS; the console is detected from the header.
- CI on ubuntu-latest, ubuntu-24.04-arm, windows-latest and windows-11-arm
  with clippy `-D warnings`, a wasm job whose frame hashes must match native,
  a cross-architecture determinism job and a 1.88 MSRV check.
- Release workflow: Linux/Windows × amd64/arm64 archives, a web archive,
  `SHA256SUMS`, changelog-driven release notes and GitHub Pages deployment.
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
- Desktop: loading a file shorter than a GBA header no longer panics;
  emulation is paced at 59.7275 Hz instead of 60 Hz.
- GBA: `reset()` keeps a loaded BIOS image instead of falling back to HLE.
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
- Release profile uses fat LTO, one codegen unit, stripped symbols and
  `panic = "abort"`; the workspace declares `rust-version = "1.88"`.
- The wasm crate no longer has a `wasm` feature; bindings compile whenever
  the target is wasm32.

### Removed
- The Game Boy only `wav` bin (use `crab run --wav`) and the Python ROM
  fetcher (use `fetch_test_roms`).
- Workspace formatted with rustfmt; generated wasm output and a save-RAM dump
  are no longer tracked.
