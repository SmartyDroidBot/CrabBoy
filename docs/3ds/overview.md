# Nintendo 3DS core: strategy and milestones

The 3DS core lives on the long-lived `3ds` branch and merges into `main` only
when it meets the bar at the end of this note. The goal is the same as for the
other cores: hardware-equivalent emulation in pure Rust.

## Approach

- **Low-level emulation first.** Both processors, the physical bus and the
  memory-mapped units are emulated, and firmware runs as it does on a console.
  Nothing of Nintendo's operating system is reimplemented. A high-level kernel
  may be added later as a convenience for users without a NAND dump; it will
  never be the accuracy reference.
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
- **M1: implemented, one exit criterion open.** `softfloat`, the ARMv5TE
  interpreter, the ARM9 protection unit, TCMs, interrupt controller, timers,
  pad, display scan-out and the boot shim exist and are tested, including two
  hand-assembled end-to-end payloads. The independent reference is the
  ARM7TDMI of `gba-core` (800,000 random instructions). Still open: running a
  third-party ARM9 payload such as `bmbt3ds`, which publishes no binary and so
  needs an ARM toolchain (devkitARM) to build.
- Everything else: not started.

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

## Ready to merge

M8 is complete; every 3DS suite is in `tests/accuracy/baseline.txt`; frame
hashes agree on x86_64, aarch64 and wasm; the GB and GBA baseline and hashes
are untouched; the desktop, CLI and wasm frontends show both screens and feed
touch and the circle pad; `README.md`, `ROADMAP.md`, `CHANGELOG.md` and
`docs/accuracy.md` are current; and the `ctr` feature becomes a default in the
final commit.
