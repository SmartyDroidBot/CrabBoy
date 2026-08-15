# Session Summary — CrabBoy

A Rust multi-system emulator framework; the current hosted console is a Game Boy
(DMG) core. This document records what was accomplished, the current state, and
known issues/next steps.

## Environment / layout

- Repo root: `D:\Projects\CrabBoy` (Windows). WSL mirror at `/home/eshaa/gb`
  (on the `Ubuntu` distro; `kali` is the default, so use `-d Ubuntu`).
- Builds: Windows via msvc; Linux natively inside WSL
  (`wsl -d Ubuntu -- bash -lc "cd /home/eshaa/gb && cargo ..."`).
- Now a git repo (initial commit made in the Phase 0 restructure).

## Phase 0 restructure (current)

Goal: a GitHub-friendly Cargo workspace with a reusable platform-agnostic core,
per-target frontends (desktop, CLI, WASM), and a future-ready crate layout for
GBA.

### Completed

- **Repo moved** from the old `gb` tree to `D:\Projects\CrabBoy`; deleted the old
  `gb` and `gb-target` (2.5 GB) directories. Salvaged `Red.gb`, `Red.sav`,
  `Red.gb.ram` into `roms/tests/`.
- **Workspace** `Cargo.toml`: members `crates/emu-core`, `crates/gb-core`,
  `platforms/{desktop,cli,wasm}`; `[workspace.dependencies]` for the core crates;
  `[profile.dev] opt-level = 1`.
- **`crates/emu-core`** (framework): `System`, `Device`, `Bus`/`Addressable`,
  `Host`, `Frame`, `AudioBuffer`/`Sample`, `Button`. Building clean.
- **`crates/gb-core`** refactored:
  - `mmu.rs` → `bus.rs` (type `Mmu` → `Bus`, implements `Addressable` + `Bus`).
  - `emulator.rs` → `gb.rs`: `Emulator` → `Gb`, implements `emu_core::System`
    (press/release via `Button`, `frame()`, `save_data`/`load_data`,
    `run_frame`/`frame_cycles`), plus concrete helpers (`step`, `framebuffer`,
    `load_sram`/`save_sram`, `press_button`/`release_button`).
  - Devices split into `devices/{mod,joypad,ppu,timer,apu}.rs`, each implementing
    `emu_core::Device`; `tick` downcasts the abstract bus to `Bus`.
  - CPU timing model untouched (PyBoy-verified). 12 unit tests.
- **`platforms/desktop`** ("CrabBoy"): rewritten to drive `Box<dyn System>` +
  `emu_core::Button`; unified `run_frame`; keymap/palettes/SRAM preserved.
- **`platforms/cli`**: `test_runner` + `probe` bins updated to `Gb`.
- **`platforms/wasm`**: `wasm`-feature-gated `wasm-bindgen` bindings updated.
- **Verification (Windows)**: workspace builds clean; **12/12 unit tests pass**;
  Red.gb smoke test (probe, 2 frames) exits 0.
- `git init`, README, MIT LICENSE, rewritten `plan.md`/`summary.md`.

### In progress / remaining

- Refresh the WSL mirror and verify a Linux build + tests.
- `.github/workflows/{ci.yml, release.yml}`.
- `scripts/` (fetch-roms, packaging).
- Accuracy suites + results recording.

## Known issues / open items

1. **LY phase offset**: a benign `LY=72` phase offset vs PyBoy at frame
   boundaries.
2. **Title-screen pixel divergence** from PyBoy (~12,960/23,040 px) to
   investigate for pixel-exact PPU.
3. **MBC2/MBC5** classified as `Other` (stubs).
4. **APU/audio** not implemented; `emu_core::AudioBuffer`/`Host` plumbing is in
   place but no audio backend yet.
5. **WSL reliability**: occasionally needs `wsl --shutdown` to recover.

## Prior fixes (kept for reference)

- **Pokémon Red title-screen freeze**: `joypad.rs` had the P1 row-select bits
  inverted (bit4/P14 = D-pad, bit5/P15 = buttons). Swapped the two row-select
  conditions and updated the affected tests. Fixed the START check at `0x12F8`.