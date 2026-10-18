//! The PICA200 GPU.
//!
//! A software pipeline that is the reference for every other renderer:
//! the shader instruction set (24-bit floats from `softfloat`), clipping,
//! rasterisation, the texture-environment combiners, fragment lighting
//! look-up tables, texture decoding, framebuffer formats, memory fills and
//! display transfers. It reaches memory through the [`memory::Memory`]
//! trait and has no dependency on the machine.
//!
//! So far: the transfer engine, the shader unit and the command processor.

pub mod command;
pub mod framebuffer;
pub mod geometry;
pub mod memory;
pub mod num;
pub mod pipeline;
pub mod raster;
pub mod regs;
pub mod shader;
pub mod texture;
pub mod transfer;
pub mod vertex;
