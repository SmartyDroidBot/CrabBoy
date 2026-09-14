# Assets

- `logo.svg` — the CrabBoy logo (source of truth, 1254×1254).
- `logo.png` — the full logo at 512×512, used by the README.
- `icon-256.png`, `icon-64.png` — square crop of the crab and console
  (no wordmark) for the desktop window icon and Linux desktop entries.
- `icon.ico` — the same crop at 16/24/32/48/64/128/256 px, embedded in the
  Windows executable by `platforms/desktop/build.rs`.

The web icons (`favicon-32.png`, `icon-192.png`, `apple-touch-icon.png`)
live next to the browser demo in `platforms/wasm/web/`.

Everything except `logo.svg` is generated. After changing the SVG, run from
the repository root:

```sh
cargo run --release -p crab-cli --features logo-tools --bin render_logo
```

The tool is pure Rust (resvg + ico) and its output is byte-for-byte
reproducible, so `git status` stays clean when the SVG has not changed.
