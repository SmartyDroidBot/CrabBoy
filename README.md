# CrabBoy

A multi-system emulator framework in Rust. Currently hosts a **Game Boy (DMG)**
core; the crate layout is designed to welcome more consoles (e.g. GBA) later.

```
crates/
  emu-core/        Platform-agnostic traits & types (System, Device, Bus, Host,
                   Frame, Audio, Button)
  gb-core/         Game Boy (DMG) emulator core, no GUI/OS/wasm deps
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

## Design

- `emu_core::System` is the uniform interface every console core implements, so
  frontends can host any console behind one `Box<dyn System>`.
- `emu_core::Device` is the uniform interface for pluggable peripherals
  (PPU, timer, APU, joypad). Devices tick with an abstract `Bus` and downcast
  via `as_any_mut()` to the concrete bus — the CPU timing model stays unchanged.
- `emu_core::Button` is a shared logical input set that each core maps to its
  own bitmask.

## License

MIT. See [LICENSE](LICENSE).