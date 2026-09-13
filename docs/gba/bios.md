# GBA BIOS routines (HLE)

`gba-core/src/bios.rs` implements the software interrupts a game can call
when no BIOS image is loaded. The SWI number comes from bits 23-16 of an ARM
`swi` instruction and from the low byte of a Thumb one; every number is routed
to the dispatcher, and unimplemented routines return to the caller and are
recorded in `Gba::last_unknown_swi` (the diagnostic runner prints it).

| SWI | Routine | Notes |
|---|---|---|
| 0x00 / 0x26 | SoftReset / HardReset | Clears 0x03007E00-0x03007FFF, zeroes r0-r12, sets the three stacks, restarts the cartridge (or EWRAM when `[0x03007FFA] != 0`) in SYS mode |
| 0x01 | RegisterRamReset | Forces DISPCNT blank; bit 1 spares the top 0x200 bytes of IWRAM; bits 5/6/7 reset serial, sound and other I/O to their start values |
| 0x02 | Halt | |
| 0x03 | Stop | Modelled as a halt |
| 0x04 / 0x05 | IntrWait / VBlankIntrWait | See `boot.md` |
| 0x06 / 0x07 | Div / DivArm | r0 = quotient, r1 = remainder, r3 = abs(quotient); division by zero follows mGBA's HLE (r0 = sign, r1 = numerator, r3 = 1) |
| 0x08 | Sqrt | Bit-serial integer square root |
| 0x09 | ArcTan | The BIOS's seven-term polynomial on a 1.14 tangent |
| 0x0A | ArcTan2 | Octant reduction onto ArcTan; full circle = 0x10000 |
| 0x0B | CpuSet | 16/32-bit copy or fill (bit 24) |
| 0x0C | CpuFastSet | 32-bit copy or fill in blocks of eight words; the count rounds up |
| 0x0D | GetBiosChecksum | 0xBAAE187F |
| 0x0E / 0x0F | BgAffineSet / ObjAffineSet | 256-entry 2.14 sine table indexed by the top byte of the angle |
| 0x10 | BitUnPack | Parameters from the info block (length, unit widths, data offset, zero flag) |
| 0x11-0x18 | LZ77, Huffman, RL, Diff8/16 | Vram variants store halfwords, Huffman stores words |
| 0x19 | SoundBias | 0 -> bias 0, else 0x200 |
| 0x1A-0x1E, 0x20-0x25, 0x28-0x2A | Sound driver, MultiBoot, debug | No-ops |
| 0x1F | MidiKey2Freq | Sound driver algorithm: 2^(n/12) table shifted per octave, interpolated by the fine adjust, multiplied into the sample rate as a 32.32 fraction |
| 0x27 | CustomHalt | Writes r2 to HALTCNT |

## Determinism

Every routine is integer-only. The sine table and semitone table are
constants; nothing calls into `libm`, so results are identical on every
platform and in WebAssembly.

## Verifying against a real BIOS

A BIOS image (`gba_bios.bin`, never committed) can serve as an oracle: run a
`swi` from IWRAM through `Gba::with_bios` and through the HLE dispatcher with
the same registers and memory, and compare r0-r3 and the destination bytes.
The affine routines, `MidiKey2Freq` and `ArcTan` are the first candidates,
since their tables were reconstructed rather than dumped. These oracle tests
are planned as `#[ignore]`d tests that skip when no image is present.
