<p align="center"><img src="assets/logo.png" width="320" alt="CrabBoy"></p>

# CrabBoy

A multi-system emulator in Rust. It hosts a **Game Boy / Game Boy Color**
core and a **Game Boy Advance** core behind one platform-agnostic interface,
with desktop, command-line and WebAssembly frontends. The goal is
hardware-accurate emulation of all three systems in pure Rust.

```
crates/
  emu-core/        Platform-agnostic traits & types (System, Frame, Audio, Button)
  gb-core/         Game Boy / Game Boy Color core, no GUI/OS/wasm deps
  gba-core/        Game Boy Advance core, no GUI/OS/wasm deps
  crab-systems/    Detects the console from a ROM header and builds the core;
                   the only crate frontends depend on
platforms/
  desktop/         egui desktop app ("CrabBoy")
  cli/             `crab` headless runner plus developer tools
                   (accuracy, fetch_test_roms, gba-diag, gba-disasm, ...)
  wasm/            wasm-bindgen bindings and the browser demo (web/)
docs/
  gba/             Verified hardware notes (boot, BIOS, DMA, I/O, RTC) and the
                   pinned verification runs
  releasing.md     How a release is cut
assets/            Logo and the generated icons (see assets/README.md)
```

See [`ROADMAP.md`](ROADMAP.md) for the milestones, [`CHANGELOG.md`](CHANGELOG.md)
for what each release contains and [`docs/accuracy.md`](docs/accuracy.md) for
the current test-suite results.

A **Nintendo 3DS** core is in development on the `3ds` branch (behind the
`crab-systems/ctr` feature); see [Nintendo 3DS](#nintendo-3ds) below and
[`docs/3ds/overview.md`](docs/3ds/overview.md).

## Status

| | Game Boy | Game Boy Color | Game Boy Advance |
|---|---|---|---|
| Desktop | yes | yes | yes |
| `crab` CLI | yes | yes | yes |
| Browser (wasm) | yes | yes | yes |
| Boots commercial games | yes | yes | yes (HLE BIOS, or a user-supplied `gba_bios.bin`) |
| Playable | yes (Pokémon Red) | yes (Pokémon Crystal) | Pokémon Emerald/Ruby intro, title and menus render and play with sound |
| Save types | MBC1/2/3/5 battery RAM, MBC3 RTC | same | SRAM, Flash 64K/128K, EEPROM 512 B/8 KB, cartridge RTC |
| Save states | yes | yes | yes |
| Audio | four channels | four channels | four channels + DirectSound FIFOs |

The GBA core passes the jsmolka arm, thumb, memory, bios and save test
suites and reaches the Pokémon Emerald and Ruby title screens and menus
(`docs/gba/verification.md` lists the pinned frames). Known gaps: cartridge
prefetch and precise wait states, the OBJ cycle budget, mosaic corner cases.

The Game Boy CPU is stepped per M-cycle: blargg's cpu_instrs, instr_timing,
mem_timing, mem_timing-2 and halt_bug pass, as does every mooneye acceptance
test outside `ppu/` (OAM DMA, timer, serial and interrupt timing included).
`docs/accuracy.md` records the full suite results.

Emulation is integer-only and deterministic: CI checks that x86_64, aarch64
and wasm builds produce bit-identical frames.

## Nintendo 3DS

The 3DS core is built only with the `ctr` feature of a frontend (or
`crab-systems/ctr` for the whole workspace) and lives on the `3ds` branch
until it can play a game on every frontend.

It runs in two ways, chosen from the image's header:

- **High-level mode** (`crates/ctr-hle`) for games and homebrew: the
  emulator is the console's operating system. It loads the program straight
  into a process, answers the Horizon kernel's calls and implements the
  system services itself, so nothing dumped from a console is needed. It
  opens **decrypted** card images (`.3ds`, `.cci`), program contents
  (`.cxi`) and 3DSX homebrew (`.3dsx`). CrabBoy contains no keys and never
  decrypts anything: an encrypted image is refused with the reason. Make
  your own decrypted dump of a game you own.
- **Low-level machine** (`crates/ctr-core`) for FIRM payloads (`.firm`):
  the hardware models run bare-metal software on the ARM9 and both ARM11
  cores. GodMode9, fastboot3DS, open_agb_firm, and Linux to its login
  prompt are pinned in the accuracy suites. Booting the console's own
  firmware, and running games on this machine, is future work.

```sh
# Desktop: the top screen above the bottom one; the mouse is the stylus
cargo run --release -p crab-desktop --features ctr -- "path/to/game.3ds"

# Headless, with the stylus and the circle pad scripted
cargo run --release -p crab-cli --features ctr --bin crab -- run game.3dsx \
    --frames 600 --input "TOUCH:160:120@300,!TOUCH@305,CPAD:0:32767@400" --hash

# Diagnostics: the program's log, its threads and the services it uses
cargo run --release -p crab-cli --bin ctr-diag -- "path/to/game.3ds" 600 \
    --every=60 --trace=svc,ipc --dump=frame
```

A card image of several gigabytes is read from disk as the game asks for
it, never whole. The game's save data, extra data and SD card are kept in
one `.sav` file next to the image, written when the game saves. The
browser build holds an image in memory, so it opens images up to 1 GiB.

What works today: card images and homebrew load and start their threads,
the kernel, the file system, APT, HID, configuration and GPU command
services answer them, and both screens show what the program puts in its
framebuffers. Games do not reach their title screens yet; the GPU's
drawing pipeline, the DSP, the CRO linker and many services are in
progress. `docs/3ds/hle.md` records what the high-level mode implements.

## Download

Each release on the [Releases page](https://github.com/SmartyDroidBot/CrabBoy/releases)
ships:

| Platform | Archive |
|---|---|
| Linux x86_64 | `crabboy-<version>-linux-amd64.tar.gz` |
| Linux arm64 | `crabboy-<version>-linux-arm64.tar.gz` |
| Windows x86_64 | `crabboy-<version>-windows-amd64.zip` |
| Windows arm64 | `crabboy-<version>-windows-arm64.zip` |
| Browser | `crabboy-<version>-web.zip` (serve the folder over HTTP) |

Archives contain the `CrabBoy` desktop app and the `crab` command-line
runner. The browser demo of the latest release is also deployed to GitHub
Pages. No ROMs or BIOS images are included or ever will be.

## Building

Rust 1.88 or newer. On Linux the desktop app needs the ALSA headers and
`pkg-config` (`sudo apt install libasound2-dev pkg-config`).

```sh
# Desktop
cargo run --release -p crab-desktop -- path/to/game.gba
cargo run --release -p crab-desktop -- --bios gba_bios.bin path/to/game.gba

# Headless
cargo run --release -p crab-cli --bin crab -- info path/to/game.gb
cargo run --release -p crab-cli --bin crab -- run path/to/game.gb \
    --frames 600 --input START@400 --dump frame.png --wav out.wav --hash

# Browser: see platforms/wasm/web/README.md
cargo build -p crab-wasm --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir platforms/wasm/web/pkg \
    target/wasm32-unknown-unknown/release/crab_wasm.wasm

# Everything CI runs
cargo build --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The console is detected from the ROM header, not the file extension. A GBA
ROM boots through high-level BIOS emulation unless a 16 KB `gba_bios.bin`
is passed with `--bios` or found next to the ROM or the executable.

`cargo run --release -p crab-cli --bin fetch_test_roms` downloads the
open-source accuracy suites (c-sp game-boy-test-roms, jsmolka gba-tests,
the 3DS payloads and the Homebrew Menu) into `roms/test-suites/`, which is
gitignored; `--bin accuracy` runs them and skips any that are absent.

## Controls

| Key | Action |
|---|---|
| Arrows | D-pad |
| Z / X | A / B |
| Enter / Backspace | Start / Select |
| A / S | L / R |
| C / V | X / Y (3DS) |
| Q / W | ZL / ZR (3DS) |
| I / J / K / L | Circle pad (3DS) |
| Mouse on the bottom screen | Stylus (3DS) |
| P / R / F | Pause / Reset / Fast-forward |
| F1–F4, Shift+F1–F4 | Save / load state slots |
| F5 / F9 | Quick save / quick load |

The browser demo adds on-screen touch controls and gamepad support.

## Pure Rust

The emulator cores, `crab-systems`, the CLI runner and the wasm bindings are
pure Rust with no C code. The remaining exceptions are host bindings:

- Desktop audio on Linux links `alsa-sys` (the ALSA C library); on Windows
  it uses WASAPI through the `windows` crate. Graphics go through OpenGL
  loaded at runtime.
- `fetch_test_roms` (a developer tool, not shipped) uses `ureq` with
  `rustls`, whose `ring` backend contains C and assembly.

## Credits

- The 3DS DSP port follows [Teakra](https://github.com/wwylele/teakra) by
  Weiyi Wang (MIT licence), the reference emulation of the Teak DSP.
- Test suites by blargg, Gekkio (mooneye), Matt Currie (mealybug, dmg-acid2,
  cgb-acid2), LIJI (same-suite) and jsmolka, and the 3DS programs of the
  GodMode9, fastboot3DS, open_agb_firm, Linux for the 3DS and Homebrew
  Menu authors; `fetch_test_roms` downloads them, none is redistributed.

## Contributing

All contributors (humans and AI agents) must follow
[`guidelines.md`](guidelines.md), including the pre-commit checklist and the
EU-style commit message format. Parts of this project were written with the
help of Claude (Anthropic), which is credited here rather than in individual
commits.

## Known Issues

- **GBA timing** is approximate: no cartridge prefetch buffer, no
  sequential/non-sequential distinction, DMA does not stall the CPU.
- **Game Boy PPU** renders per scanline (no pixel FIFO yet): dmg-acid2,
  cgb-acid2, the mealybug tests and mooneye's `ppu/` timing tests fail.
- **Audio accuracy**: the Game Boy APU still mixes in floating point and
  the GBA PSG lacks the finer edge cases (sweep and length quirks); both
  are milestones on the roadmap.
- **CGB double-speed audio** is emitted at 16384 Hz (the native CGB APU rate
  when the CPU switches to double speed). Frontends resample to a fixed
  output rate and must honour `System::audio_rate()`.

## Design

- `emu_core::System` is the uniform interface every console core implements, so
  frontends can host any console behind one `Box<dyn System>`.
- `crab_systems::detect` identifies a ROM from its header bytes and
  `crab_systems::load_with` builds the matching system, so no frontend knows
  about individual cores. `crab_systems::load_media` does the same for an
  image read in pieces through `emu_core::Storage`, such as a 3DS card image.
- `emu_core::Device` is the uniform interface for pluggable peripherals
  (PPU, timer, APU, joypad). Devices tick with an abstract `Bus` and downcast
  via `as_any_mut()` to the concrete bus.
- `emu_core::Button` is a shared logical input set that each core maps to its
  own bitmask; cores ignore buttons they do not have.
- The emulation path is integer-only and deterministic: the same number of
  master cycles produces the same state on every platform and in WebAssembly.

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).
