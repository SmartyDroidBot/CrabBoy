# GBA verification runs

Commercial ROMs are not part of the repository, so these runs are done
locally with the headless runner and the results are pinned here. Regenerate
the hashes deliberately when a change is meant to alter rendering, and say so
in the commit.

```sh
cargo build --release -p crab-cli --bin gba-diag
target/release/gba-diag "<rom>" <frames> <interval> --frame-hash [--input=START@F,!START@F+20]
target/release/gba-diag "<rom>" <frames> <frames> --dump-frame=out.png --dump-frame-at=<frame>
```

The frame hash is FNV-1a (32-bit) over the RGB framebuffer at the sampled
frame. A run must never report `WILD PC` or an `unknown_swi`.

## Pokémon Emerald (U), skip-BIOS, START pressed at frame 4700

| Frame | DISPCNT | Hash | What is on screen |
|---|---|---|---|
| 600 | 0x1F40 | 0xFCB37DE1 | Game Freak intro (dark, leaves) |
| 1200 | 0x1F40 | 0xA4D0D902 | Intro cutscene, field |
| 1800 | 0x1E40 | 0x2DEE9E12 | Intro cutscene, road with trees |
| 2400 | 0x3641 | 0xD8CA4C3D | Intro cutscene |
| 3000 | 0x3540 | 0xD70783EE | Intro, Rayquaza scene (affine layer still wrong) |
| 3600 | 0x1441 | 0xF53B26AE | Title: logo drop |
| 4200 | 0x1741 | 0x07E2CDE1 | Title screen (Rayquaza silhouette, logo, "EMERALD VERSION" sprites, press start) |
| 5000 | 0x3140 | 0x92812A89 | After START at 4700: main menu (NEW GAME / OPTION) |

`crab run` hashes (FNV-1a-32 of the RGBA frame): frame 1800 intro cutscene
`2367a19b`, frame 4200 title `07e2cde1`, frame 5000 with `--input START@4700`
`92812a89`. These replaced the v0.1.1 values when the jsmolka CPU suites and
the PPU screen-entry, sprite-size and 256-colour-sprite fixes landed.

## Pokémon Ruby (U) v1.1, skip-BIOS, START pressed at frame 3600

| Frame | DISPCNT | Hash | What is on screen |
|---|---|---|---|
| 600 | 0x1F40 | 0xFCB37DE1 | Game Freak intro |
| 1200 | 0x1F40 | 0x96ED530E | Intro cutscene |
| 1800 | 0x1E40 | 0x8467397E | Intro cutscene, road with trees |
| 2400 | 0x3940 | 0x362CB7C5 | Intro |
| 3000 | 0x3D40 | 0x93616EB5 | Intro |
| 3600 | 0x1441 | 0xF53B26AE | Title: logo drop |
| 4200 | 0x1741 | 0xBA3D678E | Title screen (Groudon, logo, "RUBY VERSION") |

`crab run` hashes: frame 4200 title `ba3d678e`, frame 4400 with
`--input START@3600` `6d1b63f2`.

## Audio (v0.1.1)

`crab run <rom> --frames 900 --wav out.wav` at 32768 Hz. FNV-1a-32 of the
WAV data bytes (everything after the 44-byte header); the jsmolka video
hashes above are unaffected by the APU.

| ROM | Stereo frames | Data bytes | FNV-1a-32 | Notes |
|---|---|---|---|---|
| Emerald | 493766 | 1975064 | 5dbd36c9 | silent until the Game Freak jingle at ~frame 203, peaks clip at the 10-bit limit |
| Ruby | 493765 | 1975060 | 2a1b5415 | RMS ≈ 13774 |

## jsmolka gba-tests

`roms/test-suites/gba/jsmolka/<suite>/<suite>.gba`, `crab run ... --frames 300
--hash` (2000 frames for the flash tests, which erase the chip). The frame hash
`a313c705` is the "All tests passed" screen.

| Suite | Result |
|---|---|
| arm, thumb, memory, bios | all tests passed |
| save/none, save/sram, save/flash64, save/flash128 | all tests passed |
| ppu/hello, ppu/shades, ppu/stripes | render (`2a1d35dc`, `f723a9c5`, `31ce03c5`) |

## Known rendering gaps

- Frames 300-700 of Emerald's Game Freak intro are mostly black.
- Affine sprite and background edge cases (mosaic, wrapping) and the OBJ
  cycle budget are not modelled yet.

These are tracked in `ROADMAP.md` under the GBA accuracy milestone.
