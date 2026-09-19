# ARM11 MPCore

Two ARMv6K cores on an Old 3DS. `crates/arm-core` interprets the instruction
set; `crates/ctr-core/src/arm11` holds what surrounds a core.

## Instruction set

ARMv6K adds to ARMv5TE: `LDREX`/`STREX` in word, byte, halfword and doubleword
forms, `CPS`, `SETEND`, `SRS`, `RFE`, `REV`, `REV16`, `REVSH`, the sign and
zero extensions, `SSAT`/`USAT` and their 16-bit forms, the parallel additions
and subtractions with the `GE` flags, `SEL`, `UMAAL`, the dual and
most-significant-word multiplies, `USAD8`/`USADA8` and the hints. `WFI`
halts the core; `WFE`, `SEV` and `YIELD` do nothing, which is functionally
safe and only costs host time in spin loops.

Not implemented yet: VFPv2 data processing (the system registers exist, so
software can enable the unit), big-endian data (`CPSR.E` is recorded only),
and alignment faults (`SCTLR.A`).

Exclusive access uses one reservation per core, kept by the bus as a physical
word address. Any store by a core, exclusive or plain, clears the other
cores' reservations on that word: Linux releases a spinlock with a plain
`STRH`, and a core that loaded the lock word before that must fail its
`STREX`, or it writes the old owner back and the lock is never free again
(this hung the kernel's SMP start-up).

## System control coprocessor

Identification and reset values as recorded by Azahar's `armstate.cpp`: main
ID 0x410FB024, TLB type 0x00000800, control 0x00054078, auxiliary control 0xF,
and the feature registers. The cache type value of a 3DS is not confirmed.
Writable control bits are 0-2, 8-9, 11-13, 15, 22-23, 25 and 28-29, while
3-6, 14, 16 and 18 always read as one (GBATEK). `c0,c0,5` gives the core
number. User mode may read the two thread registers, write the first, and
issue the three barriers; everything else is privileged.

Caches, branch prediction and the performance monitor are not modelled.
Cache maintenance is accepted and does nothing, which is coherent because
memory is never cached, but software that depends on stale cache contents
would behave differently.

## MMU (`arm11/mmu.rs`)

The two-level short-descriptor walk: sections and supersections, large and
small pages, `APX` and `XN` when `XP` is set and subpage permissions when it
is clear, domains (client, manager), the `TTBR0`/`TTBR1` split by `TTBCR.N`,
and fault status codes 5, 7, 9, 11, 13 and 15 with the domain and the write
bit in `DFSR`. Translations are cached in a 4096-entry direct-mapped software
TLB that any TLB operation and any change of a translation register empties.
ASIDs do not tag entries; a context switch empties the TLB instead.

## Private region at 0x17E00000 (GBATEK, "ARM11 MPCore Private Memory Region")

| Offset | Block |
|---|---|
| 0x000 | Snoop control unit; configuration reads 0x11 on an Old 3DS |
| 0x100 | Interrupt interface of the accessing core |
| 0x200-0x5FF | Interrupt interfaces of cores 0-3 |
| 0x600 | Timer and watchdog of the accessing core |
| 0x700-0xAFF | Timers of cores 0-3 |
| 0x1000 | Interrupt distributor |

The distributor's priority, target and configuration registers are byte
arrays, and drivers write them a byte at a time; the other registers act on
the bits written as one. The priority field implements its top four bits; a
pending interrupt is delivered when it is strictly more urgent than the
priority mask and than the running priority; ties go to the lower number.
Hardware lines are modelled as pulses: a raised line stays pending until it is
acknowledged. The private timer counts at half the core clock divided by its
prescaler plus one and raises interrupt 29. The watchdog at +0x20 is the same
counter and raises interrupt 30; libn3ds uses it as its sleep timer. Its
watchdog mode (control bit 3, left only by writing 0x12345678 and 0x87654321
to the disable register) would reset the machine at zero, which is not
modelled: it counts as a timer there too. Only the accessing core's watchdog
is mapped, at 0x620.

The configuration registers (0xC00, two bits a line: edge-triggered and the
1-N model) hold what is written, because Linux reads them back and calls an
interrupt "secure or misconfigured" otherwise; software interrupts always
read as edge-triggered. This follows QEMU's 11MPCore model, the TRM not being
at hand; lines are pulses whatever is written.

Interrupt numbers are in `arm11::irq` (3dbrew, "ARM11 Interrupts").

## Debug coprocessor

Coprocessor 14 answers two reads: the debug ID register (ARMv6 debug, six
breakpoints, two watchpoints; the variant and revision fields are unconfirmed)
and the status register, zero. Linux reads the ID unguarded during start-up
and an undefined instruction there is fatal; it then finds this debug
architecture unsupported and leaves the rest alone. Everything else on
coprocessor 14 is undefined.

## Start-up

Without boot11, `boot::shim` starts core 0 at the FIRM's ARM11 entry point in
supervisor mode with interrupts masked, the MMU off and low vectors, and
parks the other core in a wait routine at 0x0001004C, the address of the boot
ROM routine that "waits for IPI + branches to word @ 0x1FFFFFDC" (3dbrew).
The interrupt controller starts enabled so that the sleeping core can be
woken. Exception vectors jump to the handlers at 0x1FFFFFA0 onwards.

### Boot ROM routines used as a library

Bare-metal software calls a few boot ROM functions on both processors;
GodMode9's `common/bfn.h` lists their addresses and signatures. The shim
provides routines of its own at those addresses (`boot/romstubs.rs`), written
from the signatures: barriers and cache maintenance return at once, the cache
enable and disable calls report that the cache was off, the critical-section
pair masks and restores IRQs, the MPU calls toggle the control bit, and
`waitCycles` loops for roughly the requested cycles. `cpuSet` and `cpuCpy`
are deliberately left undefined, because their length unit is not documented.

## Verification

- 16 ARMv6K instruction tests and the unit tests of the MMU, the interrupt
  controller, the timer and the coprocessor.
- A hand-assembled two-processor payload that sends a word over PXI.
- GodMode9 v2.2.3 (not part of the repository) boots to its splash screen,
  takes VBlank interrupts and initialises both storage cards:
  `ctr-diag GodMode9.firm 600 --sd-fat --dump=out`.
