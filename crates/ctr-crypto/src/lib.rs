//! The hardware crypto engines of the 3DS.
//!
//! Register-level models of the AES engine (key slots, the keyX/keyY
//! scrambler, CTR/CBC/CCM/ECB, FIFOs and byte-order controls), the SHA engine
//! and the RSA engine. No key material lives in this repository: every key is
//! derived at run time from the boot ROM and OTP the user supplies.
