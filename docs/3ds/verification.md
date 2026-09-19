# 3DS verification runs

Third-party payloads are not part of the repository, so these runs are done
locally with `ctr-diag` and pinned here. Regenerate a hash deliberately when a
change is meant to alter what is on screen, and say so in the commit. The
hash is FNV-1a (32-bit) over the RGBA top screen.

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
| no card | 300 | 0fdd798e | Splash: the logo and "10th anniversary" |
| `--sd-fat` | 1500 | 68e57cf5 | Root: `[0:] SDCARD (NOLABEL) 31.9 MB`, `[S:] SYSNAND VIRTUAL 943.0 MB`, `[9:] RAMDRIVE`, `[C:] GAMECART`, `[M:] MEMORY VIRTUAL`, `[V:] VRAM VIRTUAL`; clock 20-01-01 00:00; help text below |
| `--sd-fat`, five `UP` presses from 1300 then `A` at 1400 | 1600 | fa836c18 | Drive `0:` listing `HELLO.TXT  19 Byte` |

The input script of the third run:
`UP@1300,!UP@1306,UP@1320,!UP@1326,UP@1340,!UP@1346,UP@1360,!UP@1366,UP@1380,!UP@1386,A@1400,!A@1406`.

What these runs exercise: both processors and the second ARM11 core's wait
routine, the boot shim and its stand-in boot ROM routines, the MMU, the
interrupt controller, VBlank, PXI (barrier and commands), I2C and the MCU
events, SPI and the CODEC samples, the pad, PSC fills, display scan-out, the
SD/MMC controller with both cards, the FAT volume, and the SHA and AES
engines (the OTP hash and the TWL key setup).
