//! The CRO loader, `ldr:ro` (3dbrew, "RO Services", "CRO0").
//!
//! Not written yet: every command is unhandled. The linker itself lives in
//! `crate::cro`.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::service::Service;

pub struct LdrRo;

/// The service as `service::standard` registers it.
pub fn service() -> Box<dyn Service> {
    Box::new(LdrRo)
}

impl Service for LdrRo {
    fn name(&self) -> &str {
        "ldr:ro"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        h.unhandled("ldr:ro", request)
    }
}
