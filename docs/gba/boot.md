# GBA boot and interrupt entry

How `gba-core` starts a cartridge and delivers interrupts, with the hardware
facts (GBATEK, mGBA) each choice follows.

## Skip-BIOS boot (`Gba::new`)

Without a BIOS image the core hands control to the cartridge the way the BIOS
does after its intro (mGBA `GBASkipBIOS`, GBATEK "BIOS RAM usage"):

| State | Value |
|---|---|
| PC | `0x08000000` (the cartridge entry branch) |
| Mode | SYS (0x1F), IRQ and FIQ enabled, ARM state |
| SP_svc | `0x03007FE0` |
| SP_irq | `0x03007FA0` |
| SP_usr / SP_sys | `0x03007F00` |
| POSTFLG (0x04000300) | 1 (warm boot) |
| VCOUNT | 0x7E (the LCD is mid-frame when the BIOS finishes) |
| DISPCNT | 0x0080 (forced blank); BG2/BG3 affine matrices identity; SOUNDBIAS 0x200; RCNT 0x8000 |

The top 0x200 bytes of IWRAM (0x03007E00-0x03007FFF) belong to the BIOS: the
three stacks, the IRQ handler pointer at `0x03007FFC`, the IntrWait flag word
at `0x03007FF8` and the soft-reset flag at `0x03007FFA`. `RegisterRamReset`
spares this area when it clears IWRAM.

## IRQ delivery

Hardware takes the IRQ exception at vector `0x18`, where the BIOS runs:

```
stmfd sp!, {r0-r3, r12, lr}   ; on SP_irq
mov   r0, #0x04000000
add   lr, pc, #0              ; return address inside the BIOS
ldr   pc, [r0, #-4]           ; jump to [0x03007FFC]
ldmfd sp!, {r0-r3, r12, lr}
subs  pc, lr, #4              ; restore CPSR from SPSR_irq and resume
```

`Gba::enter_irq` replicates that without a BIOS image: it enters IRQ mode with
`LR_irq = interrupted PC + 4`, pushes the six-register frame on the IRQ stack,
sets r0 to the I/O base, points LR at a two-instruction return stub planted at
BIOS address `0x20` (`ldmfd sp!, {r0-r3, r12, lr}; subs pc, lr, #4`) and jumps
to the handler the game registered at `0x03007FFC`. The game's `bx lr`
therefore lands on the same return sequence the BIOS provides, and the
exception return restores the interrupted mode and Thumb state.

An IRQ is taken when `IME` is set, the CPU's I flag is clear and `IE & IF` is
non-zero. Timer and DMA interrupts are edge-triggered: each device hands its
flags to `IF` once, so writing 1 to `IF` acknowledges them for good. `HALT`
ends on `IE & IF` regardless of `IME`.

With a real 16 KB BIOS image (`Gba::with_bios`) none of this is emulated: the
CPU starts in SVC mode at the reset vector and the BIOS code handles SWIs,
interrupts and the boot sequence itself. A BIOS image is recognised by its
size; the 40-byte stub does not count.

## IntrWait / VBlankIntrWait

The BIOS routines (SWI 4/5) enable `IME`, halt, and loop until the game's IRQ
handler ORs the awaited flag into the mirror word at `0x03007FF8`; a game whose
handler never sets the mirror, or whose `IE` excludes the interrupt, waits
forever. `bios::wait_for_irq` records the mask and the return address; each
`Gba::step` checks the mirror only while the PC is back at that address, so a
nested handler runs normally, and clears the flag from the mirror when the
wait completes. With `r0 = 0` a flag already present in the mirror returns at
once.

## Real-BIOS boot

`Gba::with_bios(rom, bios, cold)` boots from `0x00000000` in SVC mode with
IRQs masked. `cold = false` sets POSTFLG so the BIOS skips its logo. The
real-BIOS path is the reference for the HLE routines (see `bios.md`); it is
not yet verified to reach the cartridge on the cold path.
