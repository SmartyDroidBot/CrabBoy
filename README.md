# CrabBoy

A multi-system emulator framework in Rust. It hosts a **Game Boy / Game Boy
Color** core (a CGB machine that runs both DMG and CGB cartridges) and a
**Game Boy Advance** core, behind one platform-agnostic interface, with
desktop, command-line and WebAssembly frontends. The goal is hardware-accurate
emulation of all three systems in pure Rust.

```
crates/
  emu-core/        Platform-agnostic traits & types (System, Device, Bus, Host,
                   Frame, Audio, Button)
  gb-core/         Game Boy / Game Boy Color emulator core, no GUI/OS/wasm deps
  gba-core/        Game Boy Advance emulator core, no GUI/OS/wasm deps
platforms/
  desktop/         egui desktop app ("CrabBoy") — load a .gb/.gbc/.gba and play
  cli/             Headless tools: test_runner, probe, gba-diag, gba-disasm
  wasm/            wasm-bindgen bindings and a minimal browser demo
docs/
  gba/             Verified hardware notes (boot, BIOS, DMA, I/O, RTC) and the
                   pinned verification runs
```

See [`ROADMAP.md`](ROADMAP.md) for the milestones and [`CHANGELOG.md`](CHANGELOG.md)
for what each release contains.

## Status

| | Game Boy / Color | Game Boy Advance |
|---|---|---|
| Boots commercial games | yes | yes (skip-BIOS, or with a user-supplied BIOS image) |
| Playable | yes (Pokémon Red/Crystal) | intro and title screen; menus have rendering bugs |
| Save types | MBC1/2/3/5 battery RAM, MBC3 RTC | SRAM, Flash 64K/128K, EEPROM 512 B/8 KB, cartridge RTC |
| Save states | yes | yes |
| Audio | four channels | four channels + DirectSound FIFOs |

The GBA core reaches the Pokémon Emerald and Ruby title screens
(`docs/gba/verification.md` lists the pinned frames). Known gaps: the title
screens' background layers and sprites, garbled menu text, per-scanline affine
register effects, cartridge prefetch and precise wait states.

## Building

Native desktop (Windows / Linux / macOS):

```sh
cargo build -p crab-desktop --release
cargo run -p crab-desktop --release -- <path-to-rom>
```

On Linux the desktop build needs the ALSA headers (`libasound2-dev`) and
`pkg-config`; everything else is pure Rust.

Run all unit tests:

```sh
cargo test --workspace
```

Headless GBA diagnostics (frame sampling, wild-PC detection, PNG frame dumps,
VRAM dumps, scripted input):

```sh
cargo run --release -p crab-cli --bin gba-diag -- "<rom.gba>" 600 60 --frame-hash
cargo run --release -p crab-cli --bin gba-disasm -- "<rom.gba>" 0x08000000 32
```

## Contributing

All contributors (humans and AI agents) must follow
[`guidelines.md`](guidelines.md) — including the pre-commit testing checklist
and the EU-style commit message format. Parts of this project were written
with the help of Claude (Anthropic), which is credited here rather than in
individual commits.

## Known Issues

- **GBA rendering**: title-screen background layers show wrong colours, the
  Rayquaza/Groudon sprite is missing, and menu text is garbled. Affine
  reference registers are only latched at VBlank.
- **GBA timing** is approximate: no cartridge prefetch buffer, no
  sequential/non-sequential distinction, DMA does not stall the CPU.
- **Audio playback is usable but not perfect.** The desktop audio sink can
  introduce gaps at buffer boundaries; a ring-buffered sink is planned.
- **CGB double-speed audio** is emitted at 16384 Hz (the native CGB APU rate
  when the CPU switches to double speed). The desktop app recreates its audio
  sink when the rate changes; other frontends must honour `System::audio_rate()`.

## Design

- `emu_core::System` is the uniform interface every console core implements, so
  frontends can host any console behind one `Box<dyn System>`.
- `emu_core::Device` is the uniform interface for pluggable peripherals
  (PPU, timer, APU, joypad). Devices tick with an abstract `Bus` and downcast
  via `as_any_mut()` to the concrete bus — the CPU timing model stays unchanged.
- `emu_core::Button` is a shared logical input set that each core maps to its
  own bitmask.
- The emulation path is integer-only and deterministic: the same number of
  master cycles produces the same state on every platform and in WebAssembly.

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).
