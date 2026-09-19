# 3DS memory-mapped I/O

What `crates/ctr-core/src/io` models, with the source of each fact. Anything
not listed reads as zero and ignores writes, and `ctr-diag` reports every such
access. Accesses outside a processor's reach, or into an unused 4 KB block,
are data aborts (GBATEK, "3DS Memory and I/O Map"): the ARM9 sees
0x10000000-0x101FFFFF, the ARM11 0x10100000 and up.

## ARM9 only

| Address | Unit | Notes |
|---|---|---|
| 0x10000000 | CFG9 | `SYSPROT9` bits 0 and 1 are sticky; bit 0 hides the upper half of boot9. `MPCORECFG` (0xFFC) reads 1 on an Old 3DS |
| 0x10001000 | Interrupt controller | `IRQ_IE`, `IRQ_IF` (write one to clear). Timers are bits 8-11, PXI 12-14, SD/MMC 16 (3dbrew, "IRQ Registers") |
| 0x10003000 | Timers | Four 16-bit timers at 67,027,964 Hz with prescalers 1, 64, 256, 1024 and cascading (3dbrew, "TIMER Registers") |
| 0x10006000 | SD/MMC | See below |
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
at 2011-03-27 so that runs are reproducible; buttons the MCU owns (HOME,
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

## ARM11 only

| Address | Unit | Notes |
|---|---|---|
| 0x10202000 | LCD | Latched; the fill colour registers (0x204, 0xA04) reach the panels |
| 0x10400010, 0x10400020 | PSC0, PSC1 | Memory fills of 16, 24 or 32-bit patterns, complete at once, interrupts 0x28 and 0x29 |
| 0x10400400, 0x10400500 | PDC0, PDC1 | Framebuffer addresses, format, select and stride drive scan-out; the VBlank status bit and interrupts 0x2A and 0x2B fire every 4,481,136 cycles unless masked (3dbrew, "GPU/External Registers") |
| 0x10400C00 | Transfer engine | Not modelled: reports completion and raises 0x2C without copying |
| 0x17E00000 | MPCore private region | See `arm11.md` |

Framebuffers are stored a column at a time from the bottom pixel up, with
colour components in reverse byte order.
