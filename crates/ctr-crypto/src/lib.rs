//! The hardware crypto engines of the 3DS.
//!
//! Register-level models of the AES engine and the SHA engine, on primitives
//! implemented here and checked against the FIPS vectors, so the crate has no
//! dependencies. The RSA engine is still to come. No key material lives in
//! this repository: keys come from what the user's software writes, and the
//! constants of the hardware key generator from the user's own boot ROM.

pub mod aes;
pub mod aes_engine;
pub mod rsa_engine;
pub mod sha;
pub mod sha_engine;

pub use aes_engine::AesEngine;
pub use rsa_engine::RsaEngine;
pub use sha_engine::ShaEngine;
