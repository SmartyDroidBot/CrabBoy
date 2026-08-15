# Plan — CrabBoy

A GitHub-friendly, multi-system emulator framework in Rust. The current hosted
console is a **Game Boy (DMG)** core; the crate layout is designed so more
consoles (e.g. GBA) can be added later behind a uniform interface. Releases are
produced for Windows, Linux, and WASM.

This document is the living roadmap. Feature work continues inside `gb-core`.

## Roadmap (phases)

1. **Phase 0 (current)** — modular workspace restructure into a reusable,
   platform-agnostic core plus per-target frontends; CI/release scaffolding;
   git init. — **mostly complete** (see below).
2. **APU / audio** — NR10–55, four channels, mixing; behind a trait so each
   frontend picks its own audio backend. The `emu_core::AudioBuffer`/`Host`
   plumbing is scaffolded and waiting.
3. **MBC2 + MBC5** — real implementations (currently classified as `Other`).
4. **Accuracy suites** — run blargg `cpu_instrs`/`instr_timing`/`mem_timing`,
   `dmg-acid2`, mooneye via `crab-cli`; investigate the `LY=72` phase offset and
   the title-screen pixel divergence vs PyBoy.
5. **WASM UI** — web frontend on `crab-wasm`.
6. **GBC support** — optional, later (double-speed, HDMA, palettes).
7. **GBA** — a future `gba-core` crate that implements `emu_core::System`.

## Target structure (single repo, Cargo workspace)

```
CrabBoy/
├── Cargo.toml                     # [workspace] members = crates/*, platforms/*
├── .gitignore                     # /target, *.sav, *.srm, ROM binaries
├── README.md  LICENSE  plan.md  summary.md
├── .github/workflows/{ci.yml, release.yml}
├── crates/
│   ├── emu-core/                  # framework traits/types (no UI deps)
│   │   └── src/{lib,bus,device,system,host,video,audio,input}.rs
│   └── gb-core/                   # Game Boy (DMG) core (no UI deps)
│       └── src/{lib,bus,cartridge,cpu,gb}.rs + devices/{mod,joypad,ppu,timer,apu}.rs
├── platforms/
│   ├── desktop/                   # egui app "CrabBoy" (eframe, rfd, emu-core, gb-core)
│   ├── cli/                       # headless tools: test_runner, probe (bins)
│   └── wasm/                      # cdylib, wasm-bindgen wrapper (`wasm` feature-gated)
├── roms/tests/                    # sample ROM + save files (smoke testing)
└── scripts/                       # fetch-roms, packaging (TBD)
```

## Design decisions

- **`emu_core::System`** is the uniform interface every console core implements
  (`name`, `info`, `reset`, `press`/`release(Button)`, `step`, `run_frame`,
  `frame`, `battery_backed`, `save_data`/`load_data`). Frontends host any console
  behind one `Box<dyn System>`.
- **`emu_core::Device`** is the uniform interface for pluggable peripherals
  (PPU, timer, APU, joypad): `kind`, `reset`, `tick(cycles, bus)`. Devices tick
  against an abstract `Bus` and downcast via `as_any_mut()` to the concrete bus.
  The CPU timing model is unchanged (PyBoy-verified, 12 unit tests).
- **`emu_core::Button`** is a shared logical input set (A/B/X/Y/L/R/Start/Select/
  D-pad); each core maps it to its own bitmask (`Gb::button_to_bit`).
- **WASM**: `crab-wasm` `crate-type = ["cdylib", "rlib"]`, built with
  `wasm-pack --target web`. Bindings are gated behind the `wasm` feature so native
  workspace builds don't pull in `wasm-bindgen`.

## Build & test

- Dev happens on **both Windows and WSL**; the repo root is `D:\Projects\CrabBoy`
  (WSL mirror at `/home/eshaa/gb`).
- Windows (msvc): `cargo build --workspace`, `cargo test --workspace`. The dev
  profile keeps `opt-level = 1` (verified OK on Windows).
- WSL/Linux: build natively via `wsl -d Ubuntu -- bash -lc "cd /home/eshaa/gb && cargo ..."`.
- wasm: `wasm-pack build platforms/wasm --target web` (one-time `rustup target
  add wasm32-unknown-unknown`).

## Releases — GitHub Actions on tag

- `release.yml`: on tag, run a matrix of **native runners** (`ubuntu-latest`,
  `windows-latest`) building `crab-desktop` + `crab-cli`, plus a wasm job
  (`wasm-pack --target web`). Upload all artifacts to the GitHub release.
- `ci.yml`: on every push/PR, `cargo test`, `clippy`; optionally run accuracy test
  ROMs via `crab-cli`.

## Phase 0 status

Completed:
- Repo root moved to `D:\Projects\CrabBoy`; old `gb`/`gb-target` deleted;
  `Red.gb`/`Red.sav`/`Red.gb.ram` salvaged to `roms/tests/`.
- Workspace `Cargo.toml` (`crates/emu-core`, `crates/gb-core`,
  `platforms/{desktop,cli,wasm}`), `[profile.dev] opt-level = 1`, workspace deps.
- `emu-core` scaffolded and building.
- `gb-core` refactored: `mmu.rs`→`bus.rs` (type `Bus`), `emulator.rs`→`gb.rs`
  (`Gb` implements `emu_core::System`), devices behind `emu_core::Device`.
- Frontends moved to `platforms/` and adapted: desktop drives `Box<dyn System>` +
  `emu_core::Button`; cli/wasm use `Gb`.
- Windows: workspace builds clean, **12/12 unit tests pass**, Red.gb smoke test OK.
- `git init`, README, MIT LICENSE staged; `plan.md`/`summary.md` rewritten.
- CI + release workflows (`.github/workflows/`) — see status of latest commit.
- WSL mirror refresh + Linux verification — in progress.

## Next concrete actions

1. Refresh the WSL mirror (`/home/eshaa/gb`) and verify a Linux build + tests.
2. Confirm `.github/workflows/{ci.yml, release.yml}` committed and valid.
3. Add `scripts/` (fetch-roms, packaging).
4. Run accuracy suites (Phase 0.4) and record results.