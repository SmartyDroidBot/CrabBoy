# CrabBoy

A multi-system emulator framework in Rust. Currently hosts a **Game Boy / Game
Boy Color** core (a CGB machine that runs both DMG and CGB cartridges); the
crate layout is designed to welcome more consoles (e.g. GBA) later.

```
crates/
  emu-core/        Platform-agnostic traits & types (System, Device, Bus, Host,
                   Frame, Audio, Button)
  gb-core/         Game Boy / Game Boy Color emulator core, no GUI/OS/wasm deps
platforms/
  desktop/         egui desktop app ("CrabBoy") — load a .gb and play
  cli/             Headless tools: test_runner, probe
  wasm/            wasm-bindgen bindings (gated behind the `wasm` feature)
roms/tests/        Sample ROM + save files used for smoke testing
```

## Building

Native desktop (Windows / Linux / macOS):

```sh
cargo build -p crab-desktop --release
cargo run -p crab-desktop --release -- <path-to-rom.gb>
```

Run all unit tests:

```sh
cargo test --workspace
```

## Contributing

All contributors (humans and AI agents) must follow
[`guidelines.md`](guidelines.md) — including the pre-commit testing checklist
and the EU-style commit message format.

## Known Issues

- **Audio playback is usable but not perfect.** The audio driver can introduce
  subtle gaps or stutter at sink boundaries, and the music/menu track can sound
  slightly off (the title "jingle" at the logo is audible but was previously
  inaudible due to a mixer gain bug — now fixed). A more thorough audio-sink
  hardening pass (buffer priming, keep-ahead refill, silence padding on
  starvation) is planned but deferred.
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

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).