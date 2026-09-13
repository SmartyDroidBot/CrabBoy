# Guidelines

These guidelines apply to **every** change to this project, by every contributor —
humans and AI agents alike, including the assistant driving this repo. They are
binding. Treat anything not covered here by using your best judgement and the
existing code conventions.

Changes to this document itself are made only via a pull request that
references this file.

---

## 1. Approach

- **Platform-agnostic cores.** Everything in `crates/` (`emu-core`, `gb-core`,
  `gba-core`)
  must compile with no GUI, OS, or WebAssembly dependencies. Frontends live only
  under `platforms/` (`desktop`, `cli`, `wasm`).
- **Everything behind a uniform interface.** Each console implements
  `emu_core::System`; pluggable peripherals implement `emu_core::Device`. A new
  console is a new `*-core` crate behind the same traits — frontends stay
  unchanged. This layout intentionally anticipates future consoles (e.g. GBA).
- **The timing model is sacred.** The CPU/PPU/timer timing is verified against
  PyBoy and locked by unit tests. Never change it without re-running the full
  pre-commit checklist and, where timing is touched, the cross-platform
  determinism checks.
- **Determinism.** The emulation path is integer-only (no floats). The same
  number of master cycles must always produce the same state, on every platform
  and in WASM.
- **Respect existing conventions.** Match the style of the code you touch.
  Add no comments unless they genuinely clarify non-obvious logic.
- **Keep docs current.** `ROADMAP.md`, `CHANGELOG.md` and the notes under
  `docs/` are living documents; update them when the layout, design, or
  verification status changes.
- **Record hardware research as notes.** When debugging a core against real
  hardware, consult authoritative references (GBATEK for the GBA, Pan Docs for
  the Game Boy, mGBA/SameBoy as reference emulators) rather than guessing.
  Capture the verified facts as Markdown notes under `docs/<console>/` so the
  knowledge is reused, and delete any downloaded artifacts (reference pages,
  binaries) after extracting what is needed.

## 2. Adding a feature

1. Decide where it belongs: cross-console behaviour goes in `emu-core` (a trait
   or shared type); console-specific behaviour goes in `gb-core`.
2. Implement it in the core crate **with unit tests** covering the new behaviour.
3. Wire it through all three frontends (`desktop`, `cli`, `wasm`) so the whole
   workspace still builds.
4. Run the **pre-commit testing checklist** (section 6) and record the results.
5. Update documentation if the feature changes the public surface or the plan.
6. Commit with an EU-style message (section 7); for anything non-trivial, open a
   pull request (section 5).

## 3. How many at a time

- **One logical change per commit.** Do not bundle a feature, a refactor, and a
  doc fix into a single commit.
- **One feature per pull request.** A PR should describe and solve one thing.
- **Isolate risky changes.** Anything touching CPU/PPU/timer timing, or the
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
- **CI must pass** before merging: `cargo build --workspace`,
  `cargo test --workspace`, `cargo clippy --workspace`, `cargo fmt --all -- --check`
  on both `ubuntu-latest` and `windows-latest`.
- **Never merge with failing checks.** If you are a solo contributor, still
  complete the self-review checklist below before merging.
- **Never commit** ROM binaries, save files (`*.sav`, `*.srm`), generated
  framebuffer dumps, credentials, or API keys.
- Self-review checklist before merging:
  - [ ] Workspace builds and all unit tests pass (baseline 12/12).
  - [ ] No *new* clippy warnings or `rustfmt` diffs.
  - [ ] Red.gb regression passes (markers in section 6).
  - [ ] WASM bindings verified if `platforms/wasm` changed.
  - [ ] Cross-platform determinism re-checked if timing/PPU changed.
  - [ ] `ROADMAP.md`/`CHANGELOG.md` reflect the change if relevant.

## 5. Testing before each commit

Run the relevant checks **before** committing. Exact commands and expected
results:

**Always:**
```sh
cargo build --workspace
cargo test --workspace          # baseline: 12/12 passing
cargo clippy --workspace        # no new warnings beyond the known PPU style lints
cargo fmt --all -- --check      # no diffs
```

**Red.gb regression** (required for any core or CLI change):
```sh
cargo run -p crab-cli --bin probe -- roms/tests/Red.gb 400 560 0 1
# expect: === vblank gap samples: 544 ===  (exit 0)
GB_INPUT="START@400" GB_FRAMES="560" cargo run -p crab-cli --bin test_runner -- roms/tests/Red.gb
# expect final state: LCDC=CB LY=72 vblank=545 shades=[22330, 248, 208, 254]
```

**When `platforms/wasm` is touched:**
```sh
# rebuild bindings, then run the Node smoke test; the framebuffer at frame 559
# (with START@400) must be pixel-identical to native (hash eb53d00e)
node platforms/wasm/tests/run.js roms/tests/Red.gb
```

**When timing or the PPU is touched:** re-verify determinism — dump the
frame-559 PPM on Windows and on the WSL mirror and confirm they are
byte-identical (same SHA256), and confirm WASM output matches native.

**When anything changes:** re-run the checks on the WSL mirror
(`/home/eshaa/gb`) before committing, because this project is developed on both
Windows and Linux.

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
`emu-core`, `gb-core`, `cpu`, `ppu`, `timer`, `apu`, `joypad`, `cartridge`,
`bus`, `desktop`, `cli`, `wasm`, `ci`, `docs`, `roms`, `scripts`.

**Subject rules:**
- imperative, present tense: `change`, not `changed` / `changes`
- don't capitalise the first letter
- no trailing full stop

**Body:** same imperative tense. Include the motivation and contrast with the
previous behaviour.

**Footer:** use `BREAKING CHANGE: <description>` for breaking changes, and
`Closes #<issue>` / `Refs #<issue>` to reference issues.

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

## 7. AI contributors

- The same rules apply to automated agents as to humans. **Never commit without
  running the pre-commit checklist** for the parts you changed.
- Never claim a verification result that you did not actually run. Paste real
  output.
- Do not silently broaden the scope of a task; if a change is needed that was
  not asked for, call it out rather than bundling it in.

---

If any of these rules conflict, prefer (in order): the project's verified timing
model, platform-agnosticism, determinism, and the EU commit format.