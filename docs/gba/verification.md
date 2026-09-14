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
| 4200 | 0x1741 | 0x771885DD | Title screen (background colours wrong, Rayquaza sprite missing) |
| 4800 | 0x3140 | 0xAE7E9FC0 | After START: main menu / clock message (text garbled) |
| 5400 | 0x3140 | 0x01FDE534 | Main menu / clock message |

## Pokémon Ruby (U) v1.1, skip-BIOS, START pressed at frame 3600

| Frame | DISPCNT | Hash | What is on screen |
|---|---|---|---|
| 600 | 0x1F40 | 0xFCB37DE1 | Game Freak intro |
| 1200 | 0x1F40 | 0x96ED530E | Intro cutscene |
| 1800 | 0x1E40 | 0x8467397E | Intro cutscene, road with trees |
| 2400 | 0x3940 | 0x362CB7C5 | Intro |
| 3000 | 0x3D40 | 0x93616EB5 | Intro |
| 3600 | 0x1441 | 0xF53B26AE | Title: logo drop |
| 4200 | 0x1741 | 0x3D6DC2D5 | Title screen (background layers wrong) |

## Audio (v0.1.1)

`crab run <rom> --frames 900 --wav out.wav` at 32768 Hz. FNV-1a-32 of the
WAV data bytes (everything after the 44-byte header); the jsmolka video
hashes above are unaffected by the APU.

| ROM | Stereo frames | Data bytes | FNV-1a-32 | Notes |
|---|---|---|---|---|
| Emerald | 493766 | 1975064 | 5dbd36c9 | silent until the Game Freak jingle at ~frame 203, peaks clip at the 10-bit limit |
| Ruby | 493765 | 1975060 | 2a1b5415 | RMS ≈ 13774 |

## Known rendering gaps at these points

- The title screens' background layers show wrong colours and the Rayquaza /
  Groudon sprite is missing; the logo (an affine 256-colour layer) is right.
- Text in menus is garbled.
- Frames 300-700 of Emerald's Game Freak intro are mostly black.

These are tracked in `ROADMAP.md` under the GBA accuracy milestone.
