# Nintendo 3DS core: strategy and milestones

The 3DS core lives on the long-lived `3ds` branch and merges into `main` only
when it meets the bar at the end of this note. The goal is the same as for the
other cores: hardware-equivalent emulation in pure Rust.

## Approach

- **Two ways to run the console, one set of hardware models.**
  - *Low level* (`ctr-core`): both processors, the physical bus and the
    memory-mapped units, with firmware running as it does on a console. It
    boots open-source payloads and Linux today. Running Nintendo's own
    firmware this way needs the boot ROMs, OTP and NAND of a console, which
    the project does not have, so that part (M7 and M8 below) is parked, not
    dropped. It stays built and tested, and it remains the reference for how
    the hardware behaves.
  - *High level* (`ctr-hle`, in progress since 2026-09-19): the emulator is
    the operating system. It implements the Horizon kernel's supervisor
    calls and the system services, loads a game or homebrew image directly,
    and needs nothing dumped from a console. This is the path that plays
    games, and the work of the H milestones below. It guesses at what
    Nintendo's software does, so it is never the accuracy reference for the
    operating system; the processor, GPU and DSP underneath are the same
    models either way.
- **Decrypted images only.** The high-level path opens images whose NCCH
  says it is not encrypted and refuses the rest with a message. It derives no
  keys and ships none.
- **What "1:1" means here.** Architecturally exact processors (MMU and TLB,
  MPU, exclusive monitors, VFPv2 rounding and NaN rules, interrupt priority),
  bit-exact GPU and DSP output, and correct ordering of DMA, GPU, DSP and
  interrupt events. Cycle-exact bus contention between the processors is out of
  scope; no public documentation or test suite covers it.
- **Determinism.** Guest floating point (VFPv2 and the PICA200's 24-bit
  floats) is computed in integer arithmetic in `crates/softfloat`. The
  processors take turns in a fixed order for a fixed quantum
  (`ctr_core::clock::QUANTUM`), so every host produces the same frames.
- **Interpreters first.** A JIT comes after the test suites pass, behind a
  cargo feature, never on wasm, and must reproduce the interpreter's frame
  hashes. The software PICA200 pipeline is the reference for any hardware
  renderer.
- **Nothing proprietary in the repository.** No boot ROM, OTP, NAND, key,
  firmware or game image is ever committed. Keys are derived at run time from
  the files the user supplies; `.gitignore` blocks the usual file names.

## Crates

| Crate | Role |
|---|---|
| `softfloat` | IEEE-754 and PICA200 floating point over integers |
| `arm-core` | ARMv5TE (ARM9) and ARMv6K + VFPv2 (ARM11) interpreter |
| `teak-dsp` | CEVA Teak DSP, a port of Teakra (MIT) |
| `pica` | software PICA200 pipeline |
| `ctr-crypto` | AES, SHA and RSA engine models |
| `ctr-fs` | FIRM, NCSD, NCCH, 3DSX and friends |
| `ctr-core` | the machine: bus, MMU/MPU, interrupt controllers, I/O, boot, `System` |
| `gdb-stub` | GDB remote protocol, free of I/O |

`crab-systems` builds the core behind the `ctr` cargo feature, which stays off
by default until the merge:

```sh
cargo run --release -p crab-cli --features crab-systems/ctr --bin crab -- info payload.firm
```

## Status

- **M0: done.**
- **M2 and M3: done but for three units.** Both ARM11 cores run ARMv6K with
  VFPv2, the MMU, the interrupt controller, private timers and watchdogs;
  PXI, I2C with the MCU, SPI with the CODEC, both SD/MMC controllers with SD
  and eMMC cards, NDMA, the AES and SHA engines, PSC fills, the transfer
  engine and the display controllers exist. GodMode9 v2.2.3 browses the SD
  card, fastboot3DS v1.2 and open_agb_firm show their menus on both screens.
  The RSA engine exists and is checked against Python's `pow`, though no
  payload has used it yet. Missing: the OTP model (M2), the DMA330
  controllers (M3). `docs/3ds/arm11.md` and `docs/3ds/io.md` record the hardware facts.
- **M4: done on the native platforms.** Linux 5.11 from the linux-3ds
  project boots on both cores, with `arm9linuxfw` serving virtio over PXI,
  to Buildroot's login prompt; the frame is pinned (`linux-login`) and CI
  compares it across x86_64 and aarch64. The wasm build is compared with
  native on GodMode9 and fastboot3DS (both screens, no card: the wasm
  bindings cannot insert one yet), not on Linux.
- The frontends draw both screens and feed touch and the circle pad.
- **M1: implemented.** `softfloat`, the ARMv5TE
  interpreter, the ARM9 protection unit, TCMs, interrupt controller, timers,
  pad, display scan-out and the boot shim exist and are tested, including two
  hand-assembled end-to-end payloads. The independent references are the
  ARM7TDMI of `gba-core` (800,000 random instructions) and jsmolka's CPU
  suites; GodMode9's ARM9 side is the third-party payload.
- **M5: begun.** `pica` has the transfer engine, the shader unit and the
  command processor with shader uploads (`docs/3ds/gpu.md`); vertex loading
  and everything after it are not started.
- M6 onwards: not started.
- **Ordering note.** fastboot3DS's ARM9 side waits for a PXI handshake from
  its ARM11 side (read in its source), and GodMode9 also ships ARM11 code, so
  the M2 exit test is unlikely to pass before the ARM11 of M3 exists. The M2
  units (crypto engines, SDMMC, FAT, NDMA) are still testable on their own
  with vectors and synthetic images; the GodMode9 check moves to the end of
  M3.

## Milestones

Milestones marked *console* need a 3DS with boot9strap, because they run files
only a console can provide (boot ROMs, OTP, NAND, DSP firmware, games).

| | Scope | Exit criteria |
|---|---|---|
| M0 | Branch, crates, `emu-core` multi-screen and analog/touch/motion hooks, FIRM loading, this note, CI | Builds and tests pass with and without `ctr`; GB/GBA baseline and hashes unchanged |
| M1 | `softfloat`; ARMv5TE; ARM9 MPU, TCM, interrupts, timers; LCD scan-out; HID | An ARM9-only open-source payload draws and reads buttons; instruction tests against an independent reference |
| M2 | Crypto engines, OTP model, SDMMC with SD and NAND images, FAT, NDMA | GodMode9 lists a synthetic SD card; NIST vectors for AES and SHA |
| M3 | ARMv6K, VFPv2, ARM11 MMU, GIC, SCU, private timers, PXI, I2C/MCU, SPI, GPIO, CDMA | fastboot3DS and open_agb_firm menus on both screens |
| M4 | Linux on the ARM11 with its ARM9 helper | Boots to a shell; frame hash pinned and identical on x86_64, aarch64 and wasm |
| M5 | `pica` | Public GPU test binaries match published hardware captures |
| M6 | `teak-dsp` | Teakra's recorded hardware results pass |
| M7 *console* | Boot ROMs, OTP, NAND boot of NATIVE_FIRM, remaining I/O | HOME Menu |
| M8 *console* | DSP firmware, game cards, saves, save states | A commercial game reaches gameplay with audio |
| M9 | JIT (x86_64 and aarch64) | Same hashes as the interpreter on every suite |
| M10 | Hardware renderer in the desktop frontend | Optional |
| M11 | New 3DS: four cores, 804 MHz, L2 cache controller, extra memory | A New 3DS exclusive boots |

## Key-free test targets

These open-source bare-metal programs ship as FIRM payloads and contain no
Nintendo code, so they are what the core runs until a console is available:
`Gruetzig/bmbt3ds` (ARM9 only), `d0k3/GodMode9`, `derrekr/fastboot3DS`,
`profi200/open_agb_firm` with `libn3ds`, and `linux-3ds`
(`firm_linux_loader`, a kernel and `arm9linuxfw`). A FIRM is launched by
`ctr_core::boot::shim`, which does what a chainloader does: copy the sections
and start the processors at the entry points.

## References

Read, never copied: 3dbrew (mirrored at `docs.mikage.app`), GBATEK's 3DS
chapters, the ARM technical reference manuals (DDI0360 for the ARM11 MPCore,
DDI0201 for the ARM946E-S), Corgi3DS (the public low-level emulator), Mikage,
dynarmic (0BSD) for ARMv6K semantics, and Azahar's software rasteriser.
Teakra is MIT-licensed and is ported with attribution.

## High-level milestones

| # | Deliverable | Exit test |
|---|---|---|
| H0 | Host-trap hook in `arm-core`, generic event queue, `GpuExt` shared by both modes, `emu_core::Storage` and `load_media` | Every pinned frame unchanged |
| H1 | 3DSX loader; process, one thread, the supervisor calls of libctru's start-up, `srv:`, minimal APT, gsp and hid | A libctru console program shows its text |
| H2 | Threads and synchronisation objects, IPC translation, fs:USER with SD card, RomFS and save archives, cfg, ptm, ndm, ac, generic stub | devkitPro examples reach their screens |
| H3 | GX command queue; vertex loading to rasteriser, textures, combiners, tests, blending | citro3d examples and the public GPU test programs |
| H4 | Fragment lighting, fog, stencil, ETC1 and the other texture formats | citro3d lighting, fog and stencil examples |
| H5 | NCSD, NCCH, ExeFS, RomFS streamed from the image; null DSP; configuration and shared pages | A commercial title presents a first frame |
| H6 | `ldr:ro` (CRO modules), extra save data, synthesised system data, the remaining stubs | Its title screen, and input moves past it |
| H7 | First performance pass; software keyboard; y2r | In game; speed measured and reported |
| H8 | Save persistence | A save survives quitting |
| H9 | The Teakra port, running the DSP firmware the game supplies | Music |
| H10 | Tile-parallel rasteriser, the JIT decision, CIA, streaming on wasm | Speed against the 30 fps target |

The commercial test title is a cartridge image the user owns. It is never
committed, nor are screenshots of it; its suites are skipped where the file
is absent. Citra, Azahar and Panda3DS are read for behaviour and nothing is
copied from them (they are GPL); Teakra is MIT and is ported with its notice.
System data a game expects (shared font, country list, bad-word list) is
generated in the repository from openly licensed sources.

## Ready to merge

H8 is complete for a commercial game (M8 no longer gates the merge); every 3DS suite is in `tests/accuracy/baseline.txt`; frame
hashes agree on x86_64, aarch64 and wasm; the GB and GBA baseline and hashes
are untouched; the desktop, CLI and wasm frontends show both screens and feed
touch and the circle pad; `README.md`, `ROADMAP.md`, `CHANGELOG.md` and
`docs/accuracy.md` are current; and the `ctr` feature becomes a default in the
final commit.
