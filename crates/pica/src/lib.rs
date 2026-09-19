//! The PICA200 GPU.
//!
//! A software pipeline that is the reference for every other renderer:
//! the shader instruction set (24-bit floats from `softfloat`), clipping,
//! rasterisation, the texture-environment combiners, fragment lighting
//! look-up tables, texture decoding, framebuffer formats, memory fills and
//! display transfers. It works on plain byte slices of VRAM and FCRAM and has
//! no dependency on the machine.
//!
//! So far: the transfer engine and the shader unit.

pub mod shader;
pub mod transfer;
