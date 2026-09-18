//! Getting firmware into the machine.

pub mod shim;

pub use shim::{load_firm, Entry};
