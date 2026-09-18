//! GDB remote serial protocol.
//!
//! A pure state machine over byte slices: packets in, packets out, with the
//! target behind a trait. Sockets belong to the command-line frontend, so
//! this crate stays usable from the platform-agnostic cores.
