# ARM9 (ARM946E-S, ARMv5TE)

Behaviour `crates/arm-core` implements where ARMv5TE differs from the
ARM7TDMI in `gba-core`, with the source of each fact. "ARM ARM" is the ARM
Architecture Reference Manual (DDI 0100), whose instruction pages are the source
unless another one is named.

## Interworking

- `LDR`, `LDM` and Thumb `POP` into `r15` take the Thumb state from bit 0 of
  the loaded value. Data-processing writes
  to `r15` do not interwork.
- `BLX` exists in both register and immediate forms; the immediate form's `H`
  bit supplies bit 1 of the Thumb target.
- Thumb `BL` is two halves; the suffix `11101` is `BLX` and clears bits 1:0 of
  the target.

## Block transfers (GBATEK, "ARM Opcodes: Memory: Block Data Transfer")

- `STM` with the base register in the list always stores the original base.
  The ARM7TDMI stores the updated base unless the base is the first register.
- `LDM` with the base register in the list writes the base back unless the
  base is the last of several registers loaded; the written-back value then
  replaces the loaded one. The ARM7TDMI never writes back in that case.
- An empty register list transfers nothing and moves the base by 0x40.

## Alignment

Word accesses ignore bits 1:0 of the address and halfword accesses bit 0. No
rotation is applied to an unaligned `LDR`, and an unaligned `LDRSH` loads an
aligned halfword; both differ from the ARM7TDMI, which rotates the word and
loads a sign-extended byte.

## Status registers

`MSR` can write `N Z C V Q` from any mode and `I F` and the mode bits from a
privileged one. It can never change `T`. Multiplies leave
`C` and `V` alone.

## Exceptions

The link register holds the next instruction for undefined instructions and
supervisor calls, the faulting instruction plus 4 for a prefetch abort
(including `BKPT`) and plus 8 for a data abort, and the next instruction to
run plus 4 for interrupts. Aborts follow the base-restored
model: a faulting access leaves the base register unchanged.

## r15 in register-specified shifts

`r15` as the shifted register or as the first operand of a data-processing
instruction with a register-specified shift reads as the instruction's address
plus 12, not plus 8. The form is unpredictable in the ARM ARM; the value is
what jsmolka's `arm.gba` tests 224 and 225 measure on an ARM7TDMI. The ARM9
is assumed to behave the same, because the extra cycle of a register-specified
shift exists there too. Not verified on a 3DS.

## Verification

- `crates/arm-core/tests/jsmolka.rs`: jsmolka's `arm.gba` and `thumb.gba`
  run on a GBA-shaped flat memory, resuming after each failure. Everything
  passes except tests of ARM7TDMI behaviour that ARMv5 changed, which the test
  pins: rotated misaligned loads and swaps and the byte-loading `LDRSH` (ARM
  355, 408, 409, 452; Thumb 204, 211, 212, 216, 219, 221), a compare with
  `Rd` = `r15` restoring the CPSR (234), empty register lists transferring
  `r15` (513, 515, 530-532), `LDM` writeback with the base first in the list
  (516) and `STM` storing the updated base (522-529). The Thumb suite cannot
  run past test 223, which pops an even address into `r15` and so enters ARM
  state on ARMv5.
- `crates/arm-core/src/tests.rs`: hand-encoded tests per instruction class.
- `crates/arm-core/tests/differential.rs`: 800,000 random ARM and Thumb
  instructions compared against the ARM7TDMI of `gba-core` on the subset both
  architectures execute identically. It found two defects in `gba-core`, which
  the comparison masks: logical data-processing instructions with `S` clear
  `V`, and the Thumb `MOV Rd, Rm` with two low registers sets flags.

## Cycle costs

Not verified against hardware. The interpreter charges the counts of the
ARM9E-S instruction-cycle tables approximately: 1 for data processing (+1 for
a register-specified shift, +2 when `r15` is written), 3 for a branch or a
multiply, 4 for a long multiply, 2 for a single load or store (5 into `r15`),
one per register plus one for block transfers, and 3 for exception entry.
Memory wait states are not modelled yet.
