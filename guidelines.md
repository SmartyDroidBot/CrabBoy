# Guidelines

These guidelines apply to **every** change to this project, by every contributor —
humans and AI agents alike. They are binding. Treat anything not covered here
by using your best judgement and the existing code conventions.

Changes to this document itself are made only via a pull request that
references this file.

---

## 1. Approach

- **Platform-agnostic cores.** Everything in `crates/` (`emu-core`, `gb-core`,
  `gba-core`, `crab-systems`) must compile with no GUI, OS, or WebAssembly
  dependencies. Frontends live only under `platforms/` (`desktop`, `cli`,
  `wasm`) and depend on `crab-systems`, never on a core directly.
- **Everything behind a uniform interface.** Each console implements
  `emu_core::System`; pluggable peripherals implement `emu_core::Device`.
  `crab_systems::detect`/`load_with` pick the console from the ROM header. A
  new console is a new `*-core` crate behind the same traits plus one arm in
  `crab-systems` — frontends stay unchanged.
- **Determinism.** The emulation path is integer-only (no floats). The same
  number of master cycles must always produce the same state on every platform
  and in WASM. CI diffs frame hashes from x86_64, aarch64 and wasm builds.
- **Timing changes are isolated.** CPU/PPU/timer/DMA timing is locked by unit
  tests and, once the accuracy harness exists (`ROADMAP.md` v0.2.0), by golden
  frames. Never mix a timing change with an unrelated feature.
- **Pure Rust.** No C or C++ in anything that ships. Host bindings that are
  unavoidable (ALSA on Linux, WASAPI on Windows, OpenGL loading) are listed in
  the README; anything new goes on that list in the same change.
- **Respect existing conventions.** Match the style of the code you touch.
  Add no comments unless they genuinely clarify non-obvious logic.
- **Keep docs current.** `README.md`, `ROADMAP.md`, `CHANGELOG.md` and the
  notes under `docs/` are living documents; update them in the change that
  makes them stale.
- **Record hardware research as notes.** When debugging a core against real
  hardware, consult authoritative references (GBATEK for the GBA, Pan Docs for
  the Game Boy, mGBA/SameBoy as reference emulators) rather than guessing.
  Capture the verified facts as Markdown notes under `docs/<console>/` and
  delete any downloaded artifacts after extracting what is needed.

## 2. Adding a feature

1. Decide where it belongs: cross-console behaviour goes in `emu-core`;
   console-specific behaviour goes in the matching core crate.
2. Implement it in the core crate **with unit tests** covering the new behaviour.
3. Wire it through all three frontends (`desktop`, `cli`, `wasm`) so the whole
   workspace still builds and the feature is usable everywhere.
4. Run the **pre-commit checklist** (section 5) and record the results.
5. Update documentation if the feature changes the public surface or the plan,
   and add a line under `[Unreleased]` in `CHANGELOG.md`.
6. Commit with an EU-style message (section 6); for anything non-trivial, open a
   pull request (section 4).

## 3. How many at a time

- **One logical change per commit.** Do not bundle a feature, a refactor, and a
  doc fix into a single commit.
- **One feature per pull request.** A PR should describe and solve one thing.
- **Isolate risky changes.** Anything touching CPU/PPU/timer/DMA timing, or the
  framing/step loop, must be its own PR and its own commit — never mixed with an
  unrelated feature.
- Keep PRs small enough to review comfortably. If a PR grows past ~400 changed
  lines, split it unless there is a strong reason not to (say so in the PR).

## 4. Pull requests

- **Title** follows the EU commit format: `type(scope): subject`.
- **Description** explains:
  - the motivation (why),
  - what changed (and how it differs from the previous behaviour),
  - how it was verified — paste the actual command output, not "it works".
- **CI must pass** before merging: build, `clippy -D warnings`, tests and
  `rustfmt` on `ubuntu-latest`, `ubuntu-24.04-arm`, `windows-latest` and
  `windows-11-arm`, the wasm job, the determinism job and the MSRV check.
- **Never merge with failing checks.** If you are a solo contributor, still
  complete the self-review checklist below before merging.
- **Never commit** ROM images or ROM bundles, BIOS dumps, save files
  (`*.sav`, `*.srm`, `*.ram`, `*.state*`), generated framebuffer dumps,
  generated `pkg/` output, credentials, or API keys. `.gitignore` enforces
  most of this; do not weaken it.
- Self-review checklist before merging:
  - [ ] Workspace builds and all unit tests pass.
  - [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean, and
        so is `cargo clippy -p crab-wasm --target wasm32-unknown-unknown`.
  - [ ] No `rustfmt` diffs.
  - [ ] `accuracy --ci` passes, or the baseline and `docs/accuracy.md` are
        updated in the same change with the reason.
  - [ ] Frame hashes unchanged (section 5) unless the change intends to alter
        output, in which case the new hashes are stated in the PR.
  - [ ] WASM bindings and browser demo verified if `platforms/wasm` changed.
  - [ ] `CHANGELOG.md`/`ROADMAP.md`/`README.md` reflect the change if relevant.

## 5. Testing before each commit

Run the relevant checks **before** committing. Exact commands and expected
results:

**Always:**
```sh
cargo build --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

**Accuracy suites** (required for any core change). Fetch the suites once
with `cargo run --release -p crab-cli --bin fetch_test_roms`, then:
```sh
cargo run --release -p crab-cli --bin accuracy -- --ci
```
Every result must match `tests/accuracy/baseline.txt`; CI runs the same
command. A change that fixes or breaks a test updates the baseline in the
same commit (`--update-baseline`, then `--markdown docs/accuracy.md`) and
says so in the commit body. Never regress a passing test without an
explicit reason recorded there.

**Frame-hash regression** (required for any core, `crab-systems` or CLI
change):
```sh
cargo run --release -p crab-cli --bin crab -- run roms/test-suites/gb/dmg-acid2/dmg-acid2.gb --frames 120 --hash
cargo run --release -p crab-cli --bin crab -- run roms/test-suites/gb/cgb-acid2/cgb-acid2.gbc --frames 120 --hash
cargo run --release -p crab-cli --bin crab -- run roms/test-suites/gba/jsmolka/arm/arm.gba --frames 120 --hash
```
The hashes must match the ones CI last recorded on `main` (see the
`hashes-*` artifacts of the latest run) unless the change deliberately
alters output. Commercial-ROM checks used during development are recorded
in `docs/gba/verification.md`.

**When `platforms/wasm` is touched:**
```sh
cargo clippy -p crab-wasm --target wasm32-unknown-unknown -- -D warnings
cargo build -p crab-wasm --release --target wasm32-unknown-unknown
wasm-bindgen --target nodejs --out-dir target/wasm-node target/wasm32-unknown-unknown/release/crab_wasm.wasm
node platforms/wasm/tests/run.js <rom> --frames 120 --hash   # must equal `crab run --hash`
```
For `web/` changes, also serve the directory and load a ROM in a browser.

**When timing or a PPU is touched:** state the before/after hashes of the
three suite ROMs above in the commit body, and run the GBA verification
runs in `docs/gba/verification.md`.

## 6. Commit messages (EU guidelines)

Follow the **EU System / European Commission Git Commit Guidelines**:
`<type>(<scope>): <subject>`, a blank line, then a `<body>`, a blank line, then
a `<footer>`. No line may exceed **100 characters**.

**Types** (exactly these):
- `feat` — a new feature
- `fix` — a bug fix
- `docs` — documentation only
- `style` — formatting/whitespace with no behaviour change
- `refactor` — code change that neither fixes a bug nor adds a feature
- `perf` — a performance improvement
- `test` — adding or updating tests
- `chore` — build process / auxiliary tools / dependencies
  (workflow and CI configuration changes are written as `chore(ci)` because the
  EU list has no separate `ci` type)

**Scope** — optional, but recommended. Use one of:
`emu-core`, `gb-core`, `gba-core`, `systems`, `cpu`, `ppu`, `timer`, `apu`,
`joypad`, `cartridge`, `bus`, `bios`, `dma`, `eeprom`, `rtc`, `state`, `io`,
`irq`, `save`, `gba`, `desktop`, `cli`, `wasm`, `accuracy`, `release`, `ci`,
`docs`, `roms`, `scripts`, `git`.

**Subject rules:**
- imperative, present tense: `change`, not `changed` / `changes`
- don't capitalise the first letter
- no trailing full stop

**Body:** same imperative tense. Include the motivation and contrast with the
previous behaviour.

**Footer:** use `BREAKING CHANGE: <description>` for breaking changes, and
`Closes #<issue>` / `Refs #<issue>` to reference issues. No other trailers.

**Revert:** a revert starts with `revert: ` followed by the header of the
reverted commit; the body says `This reverts commit <hash>.`.

**Examples:**

```
feat(apu): add wave channel mixing

Fills in NR30/NR31/NR32/NR33/NR34 and mixes the wave channel into the
output buffer, matching the volume-envelope behaviour already used by
the square channels. Audio now renders on all three frontends.

Closes #42
```

```
fix(joypad): correct P1 row-select bits for the d-pad row

bit4/P14 selects the d-pad row and bit5/P15 the buttons row; these were
swapped, which made Pokémon Red ignore the START press on the title
screen.

Closes #17
```

```
chore(ci): build desktop and cli binaries on the release tag
```

## 7. Releases

Follow `docs/releasing.md`: bump the workspace version, move the changelog
section, commit `chore: release vX.Y.Z`, tag, push with `--follow-tags`.
Never move a published tag.

## 8. AI contributors

- The same rules apply to automated agents as to humans. **Never commit without
  running the pre-commit checklist** for the parts you changed.
- Never claim a verification result that you did not actually run. Paste real
  output.
- Do not silently broaden the scope of a task; if a change is needed that was
  not asked for, call it out rather than bundling it in.
- AI assistance is credited once, in `README.md`; commits carry no
  co-author or session trailers.

---

If any of these rules conflict, prefer (in order): determinism and the verified
timing model, platform-agnosticism, pure Rust, and the EU commit format.
