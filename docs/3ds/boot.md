# Booting a FIRM

## What the boot ROMs do (3dbrew, "Bootloader" and "FIRM")

boot9 and boot11 initialise the hardware, load a FIRM from NAND, verify its
RSA signature and section hashes, copy each section to its physical load
address and jump to the two entry points. boot9 runs with its stacks in the
data TCM, places the instruction TCM at 0 (mirrored every 0x8000 up to 128 MB)
and the data TCM at 0xFFF00000, and turns on the protection unit, both caches
and the high exception vectors (it clears 0x000F9005 in the CP15 control
register and then sets 0x0005707D).

The ARM9 exception vectors live in the boot ROM at 0xFFFF0000 and jump to
fixed handlers in ARM9 work RAM, eight bytes each (GBATEK, "BIOS 3DS Exception
Vectors"):

| Exception | ARM9 | ARM11 |
|---|---|---|
| IRQ | 0x08000000 | 0x1FFFFFA0 |
| FIQ | 0x08000008 | 0x1FFFFFA8 |
| Supervisor call | 0x08000010 | 0x1FFFFFB0 |
| Undefined | 0x08000018 | 0x1FFFFFB8 |
| Prefetch abort | 0x08000020 | 0x1FFFFFC0 |
| Data abort | 0x08000028 | 0x1FFFFFC8 |

## What a chainloader hands to a payload

Read from the boot9strap, Luma3DS and fastboot3DS sources:

- boot9strap clears the protection unit and both cache enables (control bits
  0, 2 and 12) and leaves the TCMs and the high vectors on.
- `r0` = `argc`, `r1` = `argv`, `r2` = a magic word whose low half is 0xBEEF
  (boot9strap puts its version in the high half, Luma3DS passes 0x4BEEF,
  fastboot3DS 0x3BEEF).
- `argv[0]` is the path of the payload. When `argc` is 2, `argv[1]` points to
  two sets of `{top left, top right, bottom}` framebuffer addresses. Luma3DS
  uses 0x18300000, 0x18300000, 0x18346500 and 0x18400000, 0x18400000,
  0x18446500, in `RGB8` (blue, green, red in memory; 0x46500 = 400 x 240 x 3),
  and programs the display controllers accordingly: format 0x80341 (top) and
  0x80301 (bottom), stride 0x2D0.
- A payload's first instruction is normally `msr cpsr_cxsf, #0xD3`.

The screens must be set up by the chainloader because the LCD and display
controller registers (0x10202000, 0x10400000) are ARM11-only; an ARM9 access
there aborts (GBATEK, "3DS Memory and I/O Map").

## The boot shim (`ctr_core::boot::shim`)

Without boot ROMs the core reproduces that hand-off and nothing else:

- copies the FIRM sections to their load addresses, refusing any that is not
  RAM; hashes and the signature are not checked;
- fills 0xFFFF0000 with stand-in vectors, `ldr pc, [pc, #0x18]` and a table of
  the work-RAM handler addresses above, which is what the real vectors do;
- programs the TCM regions as boot9 does and sets the control register to
  0x00056078 (boot9's value minus bits 0, 2 and 12);
- initialises both display controllers as Luma3DS does;
- passes `argc` = 2, `argv` at 0x01FFF470 (path at 0x01FFF490, framebuffers at
  0x01FFF478, the layout fastboot3DS uses) and the magic 0x0000BEEF;
- enters the ARM9 entry point in supervisor mode with interrupts masked.

No key, OTP or boot ROM data exists in this state, so anything that needs the
crypto engines keyed needs the real boot ROMs (milestone M7).
