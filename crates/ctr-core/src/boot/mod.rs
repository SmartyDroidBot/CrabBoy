//! Getting firmware into the machine.

pub mod shim;

pub use shim::{hand_off, load_firm, Entry};
