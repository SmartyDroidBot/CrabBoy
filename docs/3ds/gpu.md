# 3DS GPU (PICA200)

What `crates/pica` models, with the source of each fact. The crate works on
register values and on memory through its `Memory` trait (byte ranges by
physical address, `None` for a range that does not lie in one region) and
knows nothing of the machine; `ctr-core` implements the trait for its
physical memory and connects the crate to the registers at 0x10400000. The
memory fill units and display controllers are in `io.md`. Register IDs are
named in `pica::regs` as 3dbrew names them.

## Transfer engine (`pica::transfer`)

Described in `io.md`, "Transfer engine".

## Shader unit (`pica::shader`)

From 3dbrew, "GPU/Shader Instruction Set". One interpreter serves vertex and
geometry shaders: 16 input, 16 temporary and 16 output vectors, 96 float, four
integer and sixteen boolean uniforms, two address registers and the loop
counter, and the two comparison flags.

- Every arithmetic, comparison and move instruction, with operand
  descriptors (destination mask, component selectors, negation) and the
  inverted `-I` forms; `MAD` rounds its product before the sum.
- Relative addressing applies to constants only. An offset outside a signed
  byte is dropped, the sum wraps at seven bits, and indices past `c95` read
  as (1, 1, 1, 1).
- `CALL`, `CALLC`, `CALLU`, `IFC`, `IFU`, `LOOP`, `BREAK`, `BREAKC`, `JMPC`
  and `JMPU` (bit 0 of its count inverts the test). A loop runs its count
  plus one times and steps the loop counter by its increment; the counter
  stays readable afterwards. Comparison operators 6 and 7 are always true.
- `SETEMIT` and `EMIT` record what a geometry shader emits; nothing consumes
  it yet.

Floating point follows 3dbrew's hardware tests: no negative zero, subnormals
are zero, zero times infinity is zero but NaN times zero is NaN, `RCP` and
`RSQ` of either zero are plus infinity, `RSQ` of a negative number is NaN,
and `MAX` and `MIN` return their second operand when the comparison fails,
so a NaN there wins and a NaN in the first place loses.

Open:

- The hardware computes in its 24-bit format (1 sign, 7 exponent, 16 mantissa
  bits). Arithmetic here is single precision with those special cases; how
  the narrower format rounds has not been measured by anyone, so results can
  differ in their low bits.
- `EX2` and `LG2` use `softfloat`'s `exp2` and `log2`, rounded once; the
  hardware's precision is not measured and is likely lower. `LITP` follows
  3dbrew: (max(x, 0), y clamped to ±127.99609375, 0, max(w, 0)), with the
  comparison flags set to x ≥ 0 and w ≥ 0.
- The depth of the hardware's call, if and loop stacks is not modelled; the
  interpreter stops at 32 nested blocks or a million instructions.
- No payload drives the GPU yet. Bare-metal GPU test programs that
  need no system software are not known to exist; the published GPU tests are
  applications for the 3DS's operating system.

## Command processor (`pica::command`)

From 3dbrew, "GPU/Internal Registers". The 0x300 internal registers are
reached two ways: directly at 0x10401000 (four bytes to an ID, a narrow
write touching only its bytes) and through command lists. A command is a
parameter and a header (ID, a mask of the parameter's four bytes, a count of
further parameters, and consecutive mode, which sends those to the following
IDs instead of the same one), padded to a pair of words. Writing a jump
register (0x23C, 0x23D; 0x104018F0 on the bus) runs the buffer whose address
and size registers (in units of eight bytes) go with it; a jump inside a list
moves to the other buffer. `FINALIZE` (0x010) ends a list, and nothing after
it runs.

The list takes effect at once; interrupt 0x2D follows 16 ARM11 cycles per
word later (`clocks.md`; unmeasured, and nothing is drawn yet that would
take longer). A list without `FINALIZE` hangs the hardware; here it ends
without an interrupt.

Behind the geometry (0x280) and vertex (0x2B0) shader blocks are their
upload FIFOs: code and operand descriptors go to a twelve-bit index that
advances with each word, and float uniforms arrive last component first as
three words of 24-bit floats (`ZZWWWWWW YYYYZZZZ XXXXXXYY`) or four of single
precision, the target index advancing after each register; past `c95` they
are dropped. Boolean and integer uniforms and the entry point are plain
registers. Every other register only holds its value so far.

## Vertex loader (`pica::vertex`)

From 3dbrew, "GPU/Internal Registers" (geometry pipeline) and "GPU/Programming
Guide". Writing `DRAWARRAYS` (0x22E) or `DRAWELEMENTS` (0x22F) draws
`NUMVERTICES` vertices at once. Arrays count from `VERTEX_OFFSET`; elements
read unsigned bytes or shorts (bit 31 of `INDEXBUFFER_CONFIG`) at an offset
from the base address, which is `ATTRIBBUFFERS_LOC & 0x1FFFFFFE` times eight.

Each of the twelve attribute buffers lists, in order, the attributes a vertex
holds there and paddings of 4 to 16 bytes (component values 12 to 15, placed
on a word boundary); each attribute starts on a multiple of its element size,
and vertices are `stride` bytes apart. Elements are signed or unsigned bytes,
shorts or floats, converted exactly; elements an attribute lacks are 0, 0, 0
and 1. An attribute no buffer names keeps its fixed value.

`FIXEDATTRIB_DATA0..2` take three words per value, whichever of the three is
written: w in the high 24 bits of the first, then z, y, and x in the low 24
bits of the third (as citro3d packs them; the layout is the mirror of the
float uniforms'). With index 0xF they are immediate-mode attributes, and a
vertex is drawn as soon as `VSH_NUM_ATTR + 1` have arrived; writing the index
starts over.

The vertex shader gets attribute `i` in the input register its permutation
(0x2BB, 0x2BC) names. Of its outputs, the `k`th one enabled in the output
mask (0x2BD) takes the semantics of `SH_OUTMAP_Ok` (0x50 + k), for
`SH_OUTMAP_TOTAL` of them; semantics nothing maps read as 1.0.

## Not started

Primitive assembly, clipping, the rasteriser, texture units and combiners,
fragment lighting, the framebuffer and the geometry stage.
