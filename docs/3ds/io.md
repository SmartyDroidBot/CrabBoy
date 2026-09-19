# 3DS memory-mapped I/O

What `crates/ctr-core/src/io` models, with the source of each fact. Anything
not listed reads as zero and ignores writes, and `ctr-diag` reports every such
access. Accesses outside a processor's reach, or into an unused 4 KB block,
are data aborts (GBATEK, "3DS Memory and I/O Map"): the ARM9 sees
0x10000000-0x101FFFFF, the ARM11 0x10100000 and up.

## ARM9 only

| Address | Unit | Notes |
|---|---|---|
| 0x10000000 | CFG9 | `SYSPROT9` bits 0 and 1 are sticky; bit 0 hides the upper half of boot9. `SDMMCCTL` (0x020) bit 9 puts the SD slot on controller 1, clear on controller 3 (3dbrew, "CONFIG9 Registers"); the power bits are latched. `MPCORECFG` (0xFFC) reads 1 on an Old 3DS |
| 0x10001000 | Interrupt controller | `IRQ_IE`, `IRQ_IF` (write one to clear). NDMA channels are bits 0-7, timers 8-11, PXI 12-14, SD/MMC controller 1 bit 16 and controller 3 bit 18 (3dbrew, "IRQ Registers") |
| 0x10002000 | NDMA | See below |
| 0x10003000 | Timers | Four 16-bit timers at 67,027,964 Hz with prescalers 1, 64, 256, 1024 and cascading (3dbrew, "TIMER Registers") |
| 0x10006000, 0x10007000 | SD/MMC controllers 1 and 3 | See below |
| 0x10008000 | PXI | See below |
| 0x10010000 | `CFG9_BOOTENV` | Latched; zero is a cold boot |

## Shared

| Address | Unit | Notes |
|---|---|---|
| 0x10140000 | CFG11 | Latched; `SOCINFO` (0xFFC) reads 1 |
| 0x10141000 | PDN | Latched; the core 0 and 1 clock registers read 0x30 |
| 0x10142000, 0x10143000, 0x10160000 | SPI | FIFO mode at +0x800; the CODEC on bus 1 is a bank of register pages, other chips read 0xFF (GodMode9, `common/spi.c`) |
| 0x10144000, 0x10148000, 0x10161000 | I2C | One byte per command; the MCU on bus 1 at 0x4A, other table devices acknowledge and read zero (3dbrew, "I2C Registers") |
| 0x10146000 | HID | `HID_PAD`, a clear bit is a pressed button |
| 0x10147000 | GPIO | Latched, with the boot ROM's data values |
| 0x10163000 | PXI | The ARM11 end |

### PXI

Both ends have `SYNC`, `CNT`, `SEND` and `RECV`. `SYNC` carries a byte each
way; bit 29 interrupts the ARM11 and bit 30 the ARM9 when the receiver's bit
31 allows it. The FIFOs are 16 words deep (GodMode9 `PXI_FIFO_LEN`); their
interrupts fire on the edge, or when enabled while the condition already
holds, as on the DS. Reading an empty FIFO or writing a full one sets the
error bit.

### MCU

Registers hold what is written except: 0x00-0x13 are read-only; 0x10-0x13 are
the event bits, cleared by reading; 0x18-0x1B mask them. Writing the LCD and
backlight request bits of 0x22 updates the status bits of 0x0F and raises
events 24-29, and the event line (GPIO3_9, ARM11 interrupt 0x71) rises when an
unmasked event is pending. GodMode9 blocks on those events. The clock is fixed
at 2020-01-01 00:00 so that runs are reproducible (an earlier year makes
GodMode9 ask for the date); buttons the MCU owns (HOME,
POWER) and the sliders are not wired to the frontends yet.

### SD/MMC

The TMIO controller (3dbrew, "EMMC Registers"; bit names from libn3ds):
commands on the selected port, response registers holding card registers
without their low byte, status events that clear where a zero is written, the
interrupt mask, and block data through the 16-bit FIFO at +0x30 or the 32-bit
one at +0x10C. Port 0 is the SD slot (card detect follows the slot whatever
port is selected), port 1 the eMMC, which is always present and blank unless
an image is supplied. Commands complete at once. The SD card is presented as
high capacity and without command class 10, because how the controller ends a
64-byte `SWITCH` read under a 512-byte block length is not documented.

There are two controllers on the ARM9 side. Controller 1 has the eMMC and,
while `CFG9_SDMMCCTL` bit 9 is set, the SD slot; with the bit clear the slot
belongs to controller 3 (libn3ds drives it there, GodMode9 and fastboot3DS on
controller 1). The card keeps its state when the slot moves. Controller 3's
mapping to the ARM11 at 0x10100000 (bit 8) is not modelled.

The ARM9's pending bit latches the rising edge of a controller's interrupt
line, not its level: fastboot3DS unmasks every event and never clears the
status in its handler. While the 32-bit FIFO is enabled and a block is
waiting to be read or wanted for writing, the controller raises a request to
NDMA (startup modes 6 and 7); libn3ds moves every block that way.

### NDMA

Eight channels (3dbrew, "NDMA Registers"; bit names from libn3ds): source,
destination, total count, block count, interval, fill data and control. A
channel moves one block of `WCNT` words per request. The source is memory, a
fixed address or the fill register; both addresses can increment, decrement,
stay, and reload at the end of a block. A request is immediate (control bit
28) or comes from the device the startup mode names; of those only the two
SD/MMC controllers raise requests so far. Outside repeat mode a
device-started channel stops when `TCNT` words have moved. Completion clears
the enable bit and, if asked, raises interrupt 0-7; in repeat mode every
block interrupts. Global control bit 0 makes the address and count registers
read back the working values. The controller reaches physical memory and the
ARM9's registers, not the TCMs. Blocks move at once, the interval register is
latched only, and arbitration is lowest channel first whatever the global
control says.

## ARM11 only

| Address | Unit | Notes |
|---|---|---|
| 0x10202000 | LCD | Latched; the fill colour registers (0x204, 0xA04) reach the panels |
| 0x10400010, 0x10400020 | PSC0, PSC1 | Memory fills of 16, 24 or 32-bit patterns. The memory is written at once; busy clears, finished sets and interrupt 0x28 or 0x29 fires one ARM11 cycle per byte later (see `clocks.md`) |
| 0x10400400, 0x10400500 | PDC0, PDC1 | Framebuffer addresses, format, select and stride drive scan-out; the VBlank status bit and interrupts 0x2A and 0x2B fire every 4,481,136 cycles unless masked (3dbrew, "GPU/External Registers") |
| 0x10400C00 | Transfer engine | Display transfers and texture copies, see below; finished (bit 8) and interrupt 0x2C one ARM11 cycle per byte written later |
| 0x17E00000 | MPCore private region | See `arm11.md` |

Framebuffers are stored a column at a time from the bottom pixel up, with
colour components in reverse byte order.

### Transfer engine

`pica::transfer`, from 3dbrew's "GPU/External Registers". With flag bit 3 it
is a texture copy: the byte count, rounded down to 16, moves between lines of
an input width and gap and an output width and gap (16-byte units; no gap
means one unbroken run). Otherwise it is a display transfer: a conversion
between the five framebuffer formats (RGBA8, RGB8, RGB565, RGB5A1, RGBA4)
with an optional vertical flip and a 2x1 or 2x2 box-filter downscale. A
format value above 4 or scale 3 writes nothing and still completes.

Not known, and chosen: narrow channels widen by repeating their top bits and
narrow by dropping low bits; the box filter truncates its average; an
overlapping input and output behave as a copy through a buffer. Not modelled:
the tiling modes (flag bits 1, 5 and 16), the crop bit (2) beyond using the
output width, and register 0xC14.
