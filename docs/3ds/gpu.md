# 3DS GPU (PICA200)

What `crates/pica` models, with the source of each fact. The crate works on
byte slices and register values and knows nothing of the machine; `ctr-core`
connects it to the registers at 0x10400000. The memory fill units and display
controllers are in `io.md`.

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
- `EX2`, `LG2` and `LITP` are not implemented (they need an exponential and a
  logarithm in integer arithmetic). They leave their destination unchanged
  and are counted.
- The depth of the hardware's call, if and loop stacks is not modelled; the
  interpreter stops at 32 nested blocks or a million instructions.
- No payload drives the shader unit yet. Bare-metal GPU test programs that
  need no system software are not known to exist; the published GPU tests are
  applications for the 3DS's operating system.

## Not started

The command processor for GPU register writes and command lists, vertex
loading, primitive assembly, clipping, the rasteriser, texture units and
combiners, fragment lighting, the framebuffer and the geometry stage.
