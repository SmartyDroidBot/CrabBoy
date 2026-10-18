//! Internal register IDs, named as 3dbrew's "GPU/Internal Registers" names
//! them (without the `GPUREG_` prefix). Registers that come in sets give
//! the first of the set and the stride where it is not one.

pub const FINALIZE: u32 = 0x010;

// Rasteriser.
pub const FACECULLING_CONFIG: u32 = 0x040;
pub const VIEWPORT_WIDTH: u32 = 0x041;
pub const VIEWPORT_INVW: u32 = 0x042;
pub const VIEWPORT_HEIGHT: u32 = 0x043;
pub const VIEWPORT_INVH: u32 = 0x044;
pub const FRAGOP_CLIP: u32 = 0x047;
pub const FRAGOP_CLIP_DATA0: u32 = 0x048;
pub const DEPTHMAP_TRANSFORM_ENABLE: u32 = 0x04C;
pub const DEPTHMAP_SCALE: u32 = 0x04D;
pub const DEPTHMAP_OFFSET: u32 = 0x04E;
pub const SH_OUTMAP_TOTAL: u32 = 0x04F;
pub const SH_OUTMAP_O0: u32 = 0x050;
pub const FLAT_SHADING: u32 = 0x058;
pub const RASTER_PIXEL_MASK: u32 = 0x060;
pub const EARLYDEPTH_FUNC: u32 = 0x061;
pub const EARLYDEPTH_TEST1: u32 = 0x062;
pub const EARLYDEPTH_CLEAR: u32 = 0x063;
pub const TEXUNIT_LOD_ENABLE: u32 = 0x064;
pub const SCISSORTEST_MODE: u32 = 0x065;
pub const SCISSORTEST_POS: u32 = 0x066;
pub const SCISSORTEST_DIM: u32 = 0x067;
pub const VIEWPORT_XY: u32 = 0x068;
pub const EARLYDEPTH_DATA: u32 = 0x06A;
pub const DEPTHMAP_MODE: u32 = 0x06D;
pub const RENDERBUF_DIM: u32 = 0x06E;
pub const SH_OUTATTR_CLOCK: u32 = 0x06F;

// Texturing. Units 1 and 2 have the registers of unit 0 from BORDER_COLOR
// to LOD, then one address and the type.
pub const TEXUNIT_CONFIG: u32 = 0x080;
pub const TEXUNIT0_BORDER_COLOR: u32 = 0x081;
pub const TEXUNIT0_DIM: u32 = 0x082;
pub const TEXUNIT0_PARAM: u32 = 0x083;
pub const TEXUNIT0_LOD: u32 = 0x084;
pub const TEXUNIT0_ADDR1: u32 = 0x085;
pub const TEXUNIT0_SHADOW: u32 = 0x08B;
pub const TEXUNIT0_TYPE: u32 = 0x08E;
pub const LIGHTING_ENABLE0: u32 = 0x08F;
pub const TEXUNIT1_BORDER_COLOR: u32 = 0x091;
pub const TEXUNIT1_ADDR: u32 = 0x095;
pub const TEXUNIT1_TYPE: u32 = 0x096;
pub const TEXUNIT2_BORDER_COLOR: u32 = 0x099;
pub const TEXUNIT2_ADDR: u32 = 0x09D;
pub const TEXUNIT2_TYPE: u32 = 0x09E;
pub const TEXUNIT3_PROCTEX0: u32 = 0x0A8;
pub const PROCTEX_LUT: u32 = 0x0AF;
pub const PROCTEX_LUT_DATA0: u32 = 0x0B0;
pub const PROCTEX_LUT_DATA7: u32 = 0x0B7;

// Texture combiners: six stages of five registers, the first four eight
// apart from 0x0C0 and the last two from 0x0F0.
pub const TEXENV0_SOURCE: u32 = 0x0C0;
pub const TEXENV4_SOURCE: u32 = 0x0F0;
pub const TEXENV_UPDATE_BUFFER: u32 = 0x0E0;
pub const FOG_COLOR: u32 = 0x0E1;
pub const FOG_LINEAR_SLOPE: u32 = 0x0E2;
pub const FOG_LINEAR_OFFSET: u32 = 0x0E3;
pub const GAS_ATTENUATION: u32 = 0x0E4;
pub const GAS_ACCMAX: u32 = 0x0E5;
pub const FOG_LUT_INDEX: u32 = 0x0E6;
pub const FOG_LUT_DATA0: u32 = 0x0E8;
pub const FOG_LUT_DATA7: u32 = 0x0EF;
pub const TEXENV_BUFFER_COLOR: u32 = 0x0FD;

/// The first register of combiner stage `stage`.
pub const fn texenv(stage: u32) -> u32 {
    if stage < 4 {
        TEXENV0_SOURCE + stage * 8
    } else {
        TEXENV4_SOURCE + (stage - 4) * 8
    }
}

// Framebuffer.
pub const COLOR_OPERATION: u32 = 0x100;
pub const BLEND_FUNC: u32 = 0x101;
pub const LOGIC_OP: u32 = 0x102;
pub const BLEND_COLOR: u32 = 0x103;
pub const FRAGOP_ALPHA_TEST: u32 = 0x104;
pub const STENCIL_TEST: u32 = 0x105;
pub const STENCIL_OP: u32 = 0x106;
pub const DEPTH_COLOR_MASK: u32 = 0x107;
pub const FRAMEBUFFER_INVALIDATE: u32 = 0x110;
pub const FRAMEBUFFER_FLUSH: u32 = 0x111;
pub const COLORBUFFER_READ: u32 = 0x112;
pub const COLORBUFFER_WRITE: u32 = 0x113;
pub const DEPTHBUFFER_READ: u32 = 0x114;
pub const DEPTHBUFFER_WRITE: u32 = 0x115;
pub const DEPTHBUFFER_FORMAT: u32 = 0x116;
pub const COLORBUFFER_FORMAT: u32 = 0x117;
pub const EARLYDEPTH_TEST2: u32 = 0x118;
pub const FRAMEBUFFER_BLOCK32: u32 = 0x11B;
pub const DEPTHBUFFER_LOC: u32 = 0x11C;
pub const COLORBUFFER_LOC: u32 = 0x11D;
pub const FRAMEBUFFER_DIM: u32 = 0x11E;
pub const GAS_LUT_INDEX: u32 = 0x123;
pub const GAS_LUT_DATA: u32 = 0x124;
pub const FRAGOP_SHADOW: u32 = 0x130;

// Fragment lighting: eight lights of sixteen registers from 0x140.
pub const LIGHT0_SPECULAR0: u32 = 0x140;
pub const LIGHT_STRIDE: u32 = 0x10;
pub const LIGHT_SPECULAR0: u32 = 0x0;
pub const LIGHT_SPECULAR1: u32 = 0x1;
pub const LIGHT_DIFFUSE: u32 = 0x2;
pub const LIGHT_AMBIENT: u32 = 0x3;
pub const LIGHT_XY: u32 = 0x4;
pub const LIGHT_Z: u32 = 0x5;
pub const LIGHT_SPOTDIR_XY: u32 = 0x6;
pub const LIGHT_SPOTDIR_Z: u32 = 0x7;
pub const LIGHT_CONFIG: u32 = 0x9;
pub const LIGHT_ATTENUATION_BIAS: u32 = 0xA;
pub const LIGHT_ATTENUATION_SCALE: u32 = 0xB;
pub const LIGHTING_AMBIENT: u32 = 0x1C0;
pub const LIGHTING_NUM_LIGHTS: u32 = 0x1C2;
pub const LIGHTING_CONFIG0: u32 = 0x1C3;
pub const LIGHTING_CONFIG1: u32 = 0x1C4;
pub const LIGHTING_LUT_INDEX: u32 = 0x1C5;
pub const LIGHTING_ENABLE1: u32 = 0x1C6;
pub const LIGHTING_LUT_DATA0: u32 = 0x1C8;
pub const LIGHTING_LUT_DATA7: u32 = 0x1CF;
pub const LIGHTING_LUTINPUT_ABS: u32 = 0x1D0;
pub const LIGHTING_LUTINPUT_SELECT: u32 = 0x1D1;
pub const LIGHTING_LUTINPUT_SCALE: u32 = 0x1D2;
pub const LIGHTING_LIGHT_PERMUTATION: u32 = 0x1D9;

// Geometry pipeline. Each of the twelve attribute buffers has three
// registers from 0x203.
pub const ATTRIBBUFFERS_LOC: u32 = 0x200;
pub const ATTRIBBUFFERS_FORMAT_LOW: u32 = 0x201;
pub const ATTRIBBUFFERS_FORMAT_HIGH: u32 = 0x202;
pub const ATTRIBBUFFER0_OFFSET: u32 = 0x203;
pub const INDEXBUFFER_CONFIG: u32 = 0x227;
pub const NUMVERTICES: u32 = 0x228;
pub const GEOSTAGE_CONFIG: u32 = 0x229;
pub const VERTEX_OFFSET: u32 = 0x22A;
pub const POST_VERTEX_CACHE_NUM: u32 = 0x22D;
pub const DRAWARRAYS: u32 = 0x22E;
pub const DRAWELEMENTS: u32 = 0x22F;
pub const VTX_FUNC: u32 = 0x231;
pub const FIXEDATTRIB_INDEX: u32 = 0x232;
pub const FIXEDATTRIB_DATA0: u32 = 0x233;
pub const FIXEDATTRIB_DATA2: u32 = 0x235;
pub const CMDBUF_SIZE0: u32 = 0x238;
pub const CMDBUF_ADDR0: u32 = 0x23A;
pub const CMDBUF_JUMP0: u32 = 0x23C;
pub const CMDBUF_JUMP1: u32 = 0x23D;
pub const VSH_NUM_ATTR: u32 = 0x242;
pub const VSH_COM_MODE: u32 = 0x244;
pub const START_DRAW_FUNC0: u32 = 0x245;
pub const VSH_OUTMAP_TOTAL1: u32 = 0x24A;
pub const VSH_OUTMAP_TOTAL2: u32 = 0x251;
pub const GSH_MISC0: u32 = 0x252;
pub const GEOSTAGE_CONFIG2: u32 = 0x253;
pub const GSH_MISC1: u32 = 0x254;
pub const PRIMITIVE_CONFIG: u32 = 0x25E;
pub const RESTART_PRIMITIVE: u32 = 0x25F;

// Shader blocks: the geometry shader's at 0x280, the vertex shader's at
// 0x2B0, with the same layout.
pub const GSH: u32 = 0x280;
pub const VSH: u32 = 0x2B0;

/// Offsets within a shader block.
pub mod sh {
    pub const BOOLUNIFORM: u32 = 0x00;
    pub const INTUNIFORM_I0: u32 = 0x01;
    pub const INTUNIFORM_I3: u32 = 0x04;
    pub const INPUTBUFFER_CONFIG: u32 = 0x09;
    pub const ENTRYPOINT: u32 = 0x0A;
    pub const ATTRIBUTES_PERMUTATION_LOW: u32 = 0x0B;
    pub const ATTRIBUTES_PERMUTATION_HIGH: u32 = 0x0C;
    pub const OUTMAP_MASK: u32 = 0x0D;
    pub const CODETRANSFER_END: u32 = 0x0F;
    pub const FLOATUNIFORM_INDEX: u32 = 0x10;
    pub const FLOATUNIFORM_DATA0: u32 = 0x11;
    pub const FLOATUNIFORM_DATA7: u32 = 0x18;
    pub const CODETRANSFER_INDEX: u32 = 0x1B;
    pub const CODETRANSFER_DATA0: u32 = 0x1C;
    pub const CODETRANSFER_DATA7: u32 = 0x23;
    pub const OPDESCS_INDEX: u32 = 0x25;
    pub const OPDESCS_DATA0: u32 = 0x26;
    pub const OPDESCS_DATA7: u32 = 0x2D;
}

/// Register count.
pub const COUNT: usize = 0x300;
