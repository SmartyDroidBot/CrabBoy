# 3DS GPU (PICA200)

What `crates/pica` models, with the source of each fact. The crate works on
register values and on memory through its `Memory` trait (byte ranges by
physical address, `None` for a range that does not lie in one region) and
knows nothing of the machine; `ctr-core` implements the trait for its
physical memory and connects the crate to the registers at 0x10400000. The
memory fill units and display controllers are in `io.md`. Register IDs are
named in `pica::regs` as 3dbrew names them.

## Transfer engine (`pica::transfer`)

Described in `io.md`, "Transfer engine". Tiling (3dbrew, "GPU/External
Registers"): a display transfer reads its input in 8x8 tiles (the layout of
the colour buffer, below) and writes rows, which is how a rendered frame
reaches an LCD framebuffer; flags bit 1 reads rows and writes tiles, bit 5
keeps both tiled. The 32x32 block mode (bit 16) is taken as 8x8.

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

## Primitive assembly and clipping (`pica::geometry`)

`PRIMITIVE_CONFIG` bits 8-9 choose triangles, a strip, a fan or geometry
primitives (a list unless a geometry shader emits them). A strip keeps its
winding by replacing its two kept vertices in turn, so its triangles are
(0, 1, 2), (2, 1, 3), (2, 3, 4)...; a fan keeps its first vertex. Every draw,
and a write to `RESTART_PRIMITIVE`, starts afresh; immediate-mode vertices
continue the primitive until then.

With `GEOSTAGE_CONFIG` mode 2 the vertex shader's enabled outputs, in order,
fill the geometry shader's input buffer; when it holds
`GSH_INPUTBUFFER_CONFIG + 1` vectors the geometry shader runs on them
(through its own permutation). Each `EMIT` stores the outputs, mapped by the
geometry shader's output mask, in the slot `SETEMIT` named; an emit that
ends a primitive draws slots 0, 1, 2, or 1, 0, 2 for the other winding.

Flat shading (0x58) gives the whole triangle its third vertex's colour.
Triangles are clipped in clip space (single precision) against w ≥ 2^-16,
-w ≤ x ≤ w, -w ≤ y ≤ w, -w ≤ z ≤ 0 and, when `FRAGOP_CLIP` is on, the user
plane, interpolating every attribute where an edge crosses. The result is a
fan. Window coordinates are (x/w + 1) times the half width plus the viewport
corner (`VIEWPORT_XY`, signed 10 bits), likewise for y, with y up. The depth
map is `scale * z / w + offset` with `DEPTHMAP_TRANSFORM_ENABLE`, else
`-z / w`. Face culling (0x40) on the window-space winding: mode 1 drops
counter-clockwise triangles, mode 2 clockwise ones; zero-area triangles are
dropped.

## Fixed point

The rasteriser and everything after it work in integers. The scheme fixes
the frame hashes, so changing it changes them.

- Window x and y: 4 fractional (subpixel) bits, to nearest.
- Depth (the depth map's output): 24 fractional bits, to nearest, unclamped
  until it is tested.
- Varyings (colour, the three texture coordinates and texcoord0.w, the
  normal quaternion and the view vector): 16 fractional bits, to nearest,
  saturated to 32 bits; colours are first clamped to [0, 1].
- `1/w` stays a float until a triangle is set up. Then the three values are
  put in fixed point with a shared scale that brings the largest near 2^30.
- Coverage: edge functions in 64-bit integers, sampled at pixel centres
  (x·16 + 8). A sample exactly on an edge counts for a top edge (horizontal,
  interior below) or a left edge (in a counter-clockwise triangle, y up,
  running downwards), as Citra's rasteriser has it; clockwise triangles are
  turned around first.
- Varyings: the weights e_i·q_i (edge function times fixed-point 1/w) are
  normalised to 24 fractional bits, truncating, with the third weight
  1 minus the other two so constant values come through exactly; the
  weighted sum is rounded to nearest.
- Depth: the edge-function-weighted sum of the vertex depths over the area,
  truncating; with a W-buffer (`DEPTHMAP_MODE` 0) it is scaled by w
  through the same q values. It is then clamped to 24 bits [0, 0xFFFFFF].

## Rasteriser (`pica::raster`)

The bounding box of a triangle, cut to the framebuffer (`FRAMEBUFFER_DIM`)
and, with `SCISSORTEST_MODE` 3, to the inclusive scissor rectangle (window
coordinates, as Citra has them); mode 1 drops the pixels inside the
rectangle instead. `RASTER_PIXEL_MASK` mode 1 keeps even x, mode 2 even x and
odd y. Each covered pixel becomes a fragment (window x and y, depth, the
varyings) for the stages after it, which sit behind the `FragmentSink` trait.

## Output merger (`pica::framebuffer`)

The colour buffer (`COLORBUFFER_LOC`, formats RGBA8, RGB8, RGB5A1, RGB565
and RGBA4 stored as the display controller reads them) and the depth buffer
(`DEPTHBUFFER_LOC`; 16 bits, 24 bits, or 24 bits with the stencil in the
fourth byte) are `FRAMEBUFFER_DIM` pixels in 8x8 tiles, Z order inside a
tile (x in the even bits), the tiles in rows. Window row y is memory row
height − 1 − y (Citra: "laid out from bottom to top"). Bit 24 of the
dimension register, the 32x32 block mode (0x11B) and early depth are not
modelled.

In order: the alpha test (0x104) on the fragment's alpha; the stencil test
(0x105, only with a D24S8 buffer) comparing reference & mask with stored &
mask, and its fail operation; the depth test (0x107, 24-bit depth shifted
right 8 for a 16-bit buffer; disabled, it passes but depth is still written)
with the z-fail operation; then the z-pass operation, the depth write, and
the colour: blending (`COLOR_OPERATION` bit 8) or a logic operation (0x102),
through the channel write mask. Stencil writes go through the buffer mask.
`COLORBUFFER_READ/WRITE` and `DEPTHBUFFER_READ/WRITE` gate each access: a
test without read access passes (and compares with zero for stencil), a
blend without it reads zeros. Blending is integer: (s·f_s ± d·f_d) / 255,
truncated and clamped; minimum and maximum ignore the factors; equations 5-7
add. The "source alpha saturate" factor is min(A_s, 1 − A_d) for colour and
one for alpha. Gas and shadow modes are not modelled.

Interpolated colour channels become 8 bits as (c·255 + 2^15) >> 16.

## Texture units (`pica::texture`)

Units 0-2 (registers 0x80-0x9E), each on when its `TEXUNIT_CONFIG` bit is.
Textures are laid out like the colour buffer: 8x8 tiles in rows, Z order
inside, upside down (texel row t is memory row height − 1 − t, as Citra has
it). Formats: RGBA8 (A, B, G, R in memory), RGB8, RGB5A1, RGB565, RGBA4,
IA8 (alpha in the first byte), HILO8 (as red and green), I8, A8, IA4
(intensity in the high nibble), I4 and A4 (low nibble first), ETC1 and
ETC1A4. An ETC1 tile is four 4x4 blocks in Z order, each a little-endian
64-bit ETC1 block (texel (x, y) of a block is bit x·4 + y of each index
plane), after 64 bits of 4-bit alpha in the same order for ETC1A4. Narrow
channels widen by repeating their top bits. Mipmap levels follow each other,
each half the size (at least one tile).

Wrap modes: clamp to edge, clamp to border (the unit's border colour),
repeat, mirrored repeat; 4-7 as 0, 1, 2, 2. Coordinates are 16-bit fixed
point; a texel position has 8 fractional bits. Nearest takes the texel the
position falls in; linear blends the four around position − ½ with 8-bit
weights, rounding. With `TEXUNIT_LOD_ENABLE` the level of detail is
log2(largest of |du/dx|·w, |dv/dx|·h, |du/dy|·w, |dv/dy|·h) with 8
fractional bits (the fraction linear between powers of two), plus the
signed 1.4.8 bias, clamped to the unit's level range; at or below zero the
magnification filter samples level 0, above it the minification filter
samples the nearest level, or blends two with the mipmap filter on. Without
it, level 0 with the magnification filter (the minimum level still holds).
Unit 0's projection mode divides u and v by texcoord0.w. Cube maps, shadow
textures and the procedural unit 3 sample as transparent black.

## Not started

Texture combiners and fragment lighting.
