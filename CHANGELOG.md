# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Nintendo 3DS core scaffolding on the `3ds` branch, behind the
  `crab-systems/ctr` feature: `ctr-core` loads a FIRM payload into physical
  memory, keeps LCD frame time (4,481,136 ARM11 cycles) and reports both
  screens; `ctr-fs` parses FIRM images and detects NCSD, NCCH and 3DSX. See
  `docs/3ds/overview.md`.
- 3DS ARM9: `arm-core` interprets ARMv5TE (ARM and Thumb), checked by unit
  tests and by 800,000 random instructions against the ARM7TDMI of
  `gba-core`. `ctr-core` runs it behind the ARM946E-S protection unit and
  TCMs with the ARM9 interrupt controller, timers, the pad register, an event
  scheduler and display scan-out, and starts FIRM payloads the way a
  chainloader does (`docs/3ds/boot.md`). The ARM11, GPU and DSP do not exist
  yet.
- 3DS ARM11: `arm-core` adds the ARMv6K integer instruction set; `ctr-core`
  runs both cores behind the ARMv6 MMU with the MPCore interrupt controller
  and private timers, PXI, I2C and the MCU, SPI and the CODEC, PSC fills,
  VBlank interrupts and the SD/MMC controller with SD and eMMC cards. The
  boot shim stands in for the boot ROM routines homebrew calls. GodMode9
  boots to its splash screen. `ctr-fs` builds and reads FAT16 volumes, and
  `ctr-diag` runs a FIRM headlessly and reports the processors, the I/O and
  unmodelled registers.
- `softfloat`: IEEE-754 single and double precision in integer arithmetic with
  the VFP rounding modes, flush-to-zero, default NaN and cumulative flags,
  bit-exact against the host in round-to-nearest.
- `emu_core::System` gains `screens`, `frame_at`, `set_axis`, `set_touch` and
  `set_motion` with defaults that leave existing cores unchanged, `Button`
  gains `ZL` and `ZR`, and `emu_core::{mem, state}` share the heap region
  type and the save-state reader and writer.
- 3DS: fastboot3DS v1.2 and open_agb_firm run to their menus and are pinned
  beside GodMode9 (`fastboot3ds-*`, `open-agb-firm-browser`; suites can hash
  both screens). Behind them: the ARM9's DMA controller with requests from
  the SD/MMC controllers, the second SD/MMC controller and the slot routing
  of `CFG9_SDMMCCTL`, the MPCore watchdog as a timer, the GPU's transfer
  engine in the new `pica::transfer` (display transfers between the five
  framebuffer formats, texture copies), GPU fills and transfers that take
  time before they interrupt, and an SD interrupt that latches on the edge.
  `fetch_test_roms --only 3ds` unpacks `.7z` releases; `ctr-diag` gains
  `--watch=LO-HI`, the ARM9 interrupt state, fault origins and a both-screens
  hash.
- 3DS: Linux 5.11 (linux-3ds) boots on both ARM11 cores to Buildroot's login
  prompt, with the ARM9 serving virtio over PXI; pinned as `linux-login`.
  It needed: other cores' exclusive reservations cleared by any store (a
  lost spinlock release hung SMP start-up), the GIC configuration
  registers, the debug ID register on coprocessor 14, and the unused
  identification registers reading as zero. `ctr_fs::fat` builds
  directories and long file names and reads files by path; suites take
  `sd_files`; `ctr-diag` gains `--sd-dir`, `--mem`, `--save` and `--regs`.
- `emu_core::Layout` stacks a console's displays into one image. The desktop
  app, `crab run` and the wasm bindings draw through it, so a 3DS payload
  shows both screens; the pointer held on the bottom screen is the stylus,
  I/J/K/L move the circle pad, and C, V, Q and W are X, Y, ZL and ZR. Each
  frontend has a `ctr` feature that turns the 3DS core on. Single-display
  consoles produce the same frames and hashes as before.
- `accuracy --dump-failures DIR` writes the frame of every failing screenshot
  test in the reference encoding for diffing.

## [0.3.0] - 2026-09-14

Game Boy CPU timing release: the CPU is stepped per M-cycle and every
blargg CPU test, halt_bug and every mooneye acceptance test outside `ppu/`
pass. Accuracy baseline: 111 of 313.

### Added
- OAM DMA warm-up cycle, closed OAM for one cycle after the last byte, and
  bus conflicts: CPU reads on the bus the DMA engine drives return the byte
  it transferred last (mooneye `oam_dma/sources`).
- The timer's TAC glitch (disabling or re-clocking the timer while the
  selected DIV bit is set increments TIMA) and the exact TIMA/TMA write rules
  around a reload.
- Post-boot register state: DIV phase ($ABCC DMG, $2678 CGB), IF with the
  VBlank flag, TAC/P1/STAT/KEY1/VBK unused bits, the sound registers after
  the start-up chime, the LCD at the top of the frame.

### Changed
- The GB CPU ticks the bus at every memory access (M-cycle stepping);
  internal cycles of `INC rr`, `ADD HL,rr`, `LD SP,HL`, taken `JR`/`JP`/
  `CALL`/`RET`, `RET cc`, `PUSH`, `ADD SP,e` and `LD HL,SP+e` land where
  the hardware puts them. Accesses happen at the start of their M-cycle so
  an interrupt raised by the last access of an instruction is dispatched
  before the next one.
- Interrupt dispatch takes five M-cycles with a discarded opcode fetch; IE
  is sampled after the high push and IF after the low push (`PUSH` onto IE
  can cancel the dispatch to vector $0000).
- HALT with an interrupt already pending and IME set returns to HALT after
  the handler; the HALT bug fetches the byte after HALT twice.
- The serial port is clocked from DIV's bit 8 (bit 3 for the CGB's fast
  clock); an external-clock transfer waits forever without a partner.
- Unmapped I/O reads $FF; the CGB register block is hidden on the DMG.
- Save state format version 3 (OAM DMA engine state, timer reload cycle,
  serial bit counter).

### Fixed
- `cpu_instrs` no longer hangs on the DMG: the ROM detected a CGB through
  the readable KEY1 register and entered STOP for a speed switch.
- ROR by a multiple of 32 in the GBA core underflowed the carry index in
  debug builds.
- The `accuracy` runner expands `..` path segments and runs mem_timing-2 as
  a memory-result test.

## [0.2.0] - 2026-09-14

Accuracy harness release: every open-source test suite runs in CI against a
recorded baseline.

### Added
- `accuracy` runner (`platforms/cli`) driven by `tests/accuracy/suites.toml`:
  blargg (serial and memory-signature tests), mooneye and SameSuite
  (`ld b,b` register signature), dmg-acid2, cgb-acid2, mealybug and blargg
  screenshot comparisons, and the jsmolka GBA suites, run in parallel with
  per-test frame budgets. Results are compared with a committed baseline;
  CI fails on any change so improvements are recorded deliberately.
  Baseline results live in `docs/accuracy.md` (77 of 313 pass).
- gb-core: `Gb::new_with_model` boots a cartridge as DMG or CGB regardless
  of its header; `Gb::take_breakpoint` reports `ld b,b`.

### Fixed
- gb-core: the serial port now transmits the byte written to SB (blargg's
  "Passed"/"Failed" text was never visible); CGB colours expand with
  `(x << 3) | (x >> 2)` as the reference screenshots do.

### Removed
- The `probe`, `test_runner` and `run_accuracy` bins, replaced by `crab run`
  and `accuracy`.

## [0.1.2] - 2026-09-14

GBA conformance release: the jsmolka CPU, memory, BIOS and save suites
pass, Pokémon Emerald and Ruby render and sound correctly.

### Fixed
- GBA CPU: the jsmolka arm and thumb suites pass. SBC/RSC/ADC carry and
  overflow, ASR of negative values, register-specified shifts by zero,
  rotated-immediate carry, r15 as a register-shifted operand and in STR/STM
  (+12), the "P" forms of TST/TEQ/CMP/CMN, empty LDM/STM register lists,
  STM with the base in the list, and unaligned block transfers. Pokémon's
  software audio mixer produced white noise because of these.
- GBA memory: the jsmolka memory, bios and save suites pass. Byte stores to
  palette RAM / VRAM / OAM follow the hardware rules, word stores align, the
  save region is an 8-bit bus mirrored at 0x0F000000, BIOS reads from
  outside the BIOS return the last prefetched opcode, SRAM powers up as
  0xFF and flash erases by sector.
- GBA PPU: text-background palette banks came from the flip bits, sprite
  shapes had the wrong heights (rows of text drawn twice), disabled sprites
  were drawn, 256-colour sprites read their tiles from the wrong place
  (Rayquaza and Groudon were missing), mode 1 drew a BG3 and mode 2 drew
  BG0/BG1, and affine reference points only latched at VBlank. Emerald and
  Ruby now render their intros, title screens and menus correctly.
- Desktop: the FPS display counts emulated frames, not repaints.

### Added
- `gba-diag --dump-vram` also writes the I/O registers, so a dump can be
  rendered layer by layer offline.

## [0.1.1] - 2026-09-14

Polish release: project branding, a rewritten desktop audio path and
audible GBA sound.

### Added
- Project logo (`assets/`), used in the README, as the desktop window icon
  and Windows executable icon, and as the browser demo's favicon, home-screen
  icon and web manifest; `render_logo` dev tool regenerates every icon from
  the SVG in pure Rust.
- Desktop: the status bar shows how much audio is queued.
- GBA: DirectSound integration test through DMA, timers and `run_frame`.

### Fixed
- Desktop audio: playback goes through one fixed-rate ring buffer instead
  of a new rodio source per frame, so there are no gaps at frame boundaries,
  the queue is bounded to 120 ms, and loading a ROM, reset, pause,
  fast-forward and state loads clear it instead of letting the previous
  ROM's music play on. The output device is no longer reopened when the
  sample rate changes.
- Desktop pacing: a stall (file dialog, window drag, slow ROM read) no longer
  replays as a burst of frames; at most four frames catch up per update.
- Desktop: the screen texture is converted and uploaded only when a new
  frame was emulated.
- Both cores return every audio sample a frame produced instead of dropping
  the fractional sample every few frames (0.1-0.3 % deficit, audible as
  periodic clicks).
- GBA audio was silent: the bus never routed sound-register writes to the
  APU. The APU now lives on the bus; PSG registers are decoded per GBATEK
  (frequency, duty, length, envelope, noise divider and shift, wave RAM
  banks and 64-sample mode, SOUNDCNT_L/H routing and volumes, SOUNDCNT_X
  master enable, SOUNDBIAS), channels and the frame sequencer are clocked
  in CPU cycles, and mixing is integer-only in the hardware's 10-bit units.
  Save-state format is now version 6.

## [0.1.0] - 2026-09-14

First release: GB, GBC and GBA on desktop, command line and in the
browser, built for Linux and Windows on amd64 and arm64.

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
