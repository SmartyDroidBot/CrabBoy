# 3DS verification runs

Third-party payloads are not part of the repository, so these runs are done
locally with `ctr-diag` and pinned here. Regenerate a hash deliberately when a
change is meant to alter what is on screen, and say so in the commit. The
hash is FNV-1a (32-bit) over the RGBA top screen, or where it says "both"
over the two screens stacked as the frontends draw them (400x480, the bottom
screen centred); `ctr-diag` prints the two side by side.

```sh
cargo build --release -p crab-cli --bin ctr-diag
target/release/ctr-diag <firm> <frames> --every=<frames> [--sd-fat] [--input=...] [--dump=PREFIX]
```

`--sd-fat` inserts a 32 MB FAT16 card holding `HELLO.TXT` (19 bytes), built by
`ctr_fs::fat`. A run must report no undefined instruction, prefetch abort or
data abort on any processor.

## GodMode9 v2.2.3 (`GodMode9.firm` from the release archive)

| Run | Frame | Top hash | What is on screen |
|---|---|---|---|
| no card | 40 | 0fdd798e | Splash: the logo and "10th anniversary" |
| no card | 300 | bc9ff74a | Root without `[0:]`: `[S:] SYSNAND VIRTUAL 943.0 MB`, `[9:] RAMDRIVE`, `[C:] GAMECART`, `[M:] MEMORY VIRTUAL`, `[V:] VRAM VIRTUAL` |
| `--sd-fat` | 1500 | ebc2e71a | Root with `[0:] SDCARD (NOLABEL) 31.9 MB` selected; clock 20-01-01 00:00; help text below |
| `--sd-fat --input=A@1400,!A@1406` | 1600 | fa836c18 | Drive `0:` listing `HELLO.TXT  19 Byte` |

The same four runs are the `godmode9-*` suites of `tests/accuracy/suites.toml`
(kind `ctr-frame`), so CI checks them against the baseline;
`fetch_test_roms --only 3ds` downloads the pinned release archive.

What these runs exercise: both processors and the second ARM11 core's wait
routine, the boot shim and its stand-in boot ROM routines, the MMU, the
interrupt controller, VBlank, PXI (barrier and commands), I2C and the MCU
events, SPI and the CODEC samples, the pad, PSC fills, display scan-out, the
SD/MMC controller with both cards, the FAT volume, and the SHA and AES
engines (the OTP hash and the TWL key setup).

## fastboot3DS v1.2 (`fastboot3DS.firm` from the release archive)

| Run | Frame | Both hash | What is on screen |
|---|---|---|---|
| `--sd-fat` | 300 | ab9baebb | Top: "fastboot3DS v1.2", Model: Old 3DS, Cold boot, Battery: 100 %, `sdmc:/ mounted`, the three NAND drives not mounted, six empty boot slots. Bottom: Main Menu with "Continue boot" selected |
| `--sd-fat --input=DOWN@200,!DOWN@206,DOWN@230,!DOWN@236,DOWN@260,!DOWN@266,A@300,!A@306` | 420 | 37e2056f | "Boot from file...": the browser at `root` listing `sdmc: (SD Card)` |

It adds: NDMA fills, the PSC completion interrupts arriving after the write
that starts them, RGB565 framebuffers swapped through the select register,
texture copies through the transfer engine, and the SD interrupt's edge
latch.

## open_agb_firm beta 2024-12-24 (`open_agb_firm.firm`)

| Run | Frame | Both hash | What is on screen |
|---|---|---|---|
| `--sd-fat` | 300 | f7a376a2 | Top: blank. Bottom: the file browser with `>3ds`, the directory it has just created on the card |

It adds, through libn3ds: the SD slot on controller 3, every block moved by
NDMA on the controller's request (reads and the writes that create
`/3ds/open_agb_firm`), the MPCore watchdog as the sleep timer, and the
interrupt-driven PXI command loop. Its accesses to the DMA330 controllers
(0x1000C000, 0x10200000) are still unmodelled; it only resets them.

These are the `fastboot3ds-*` and `open-agb-firm-browser` suites
(`both_screens = true`). CI also diffs the frame-300 hashes of all three
payloads across the native platforms.
