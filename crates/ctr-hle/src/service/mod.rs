//! System services (3dbrew, "Services API").
//!
//! On a console a service is a process that answers requests on a port. Here
//! it is host code: a session handle names a service and, for services that
//! hand out sessions of their own (files, directories), an object of that
//! service. `SendSyncRequest` reads the request from the caller's command
//! buffer, the service answers it, and the reply is written back.
//!
//! A service that is not written yet is a [`Stub`]: it answers every command
//! with success and zeros and logs each command once, or stops the process
//! when the console is strict.

pub mod apt;
pub mod cfg;
pub mod dsp;
pub mod fs;
pub mod gsp;
pub mod hid;
pub mod ldr_ro;
pub mod ndm;
pub mod simple;
pub mod table;
#[cfg(test)]
pub mod testing;

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::kernel::Object;
use crate::result::{self, ResultCode};

pub trait Service {
    fn name(&self) -> &str;

    /// Answer `request`, sent on a session to `object` of this service (0
    /// for the service itself).
    fn call(&mut self, h: &mut Horizon, object: u32, request: &Request) -> Reply;

    /// A clock event the service scheduled with `Horizon::schedule_service`.
    fn event(&mut self, _h: &mut Horizon, _tag: u32) {}

    /// The vertical blank: every registered service hears of it, in the
    /// order they were registered.
    fn vblank(&mut self, _h: &mut Horizon) {}
}

/// The place a service occupies while it is answering a request.
pub struct Vacant;

impl Service for Vacant {
    fn name(&self) -> &str {
        "(busy)"
    }

    fn call(&mut self, _h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        Reply::new(request.command(), result::NOT_IMPLEMENTED)
    }
}

/// A service nobody has written yet.
pub struct Stub {
    pub name: String,
}

impl Service for Stub {
    fn name(&self) -> &str {
        &self.name
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        h.unhandled(&self.name, request)
    }
}

/// The service manager port, "srv:" (3dbrew, "Services").
#[derive(Default)]
pub struct Srv {
    notification: Option<u32>,
}

impl Service for Srv {
    fn name(&self) -> &str {
        "srv:"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        match command {
            // RegisterClient.
            0x1 => ok(),
            // EnableNotification: the semaphore notifications count on.
            0x2 => {
                let semaphore = *self.notification.get_or_insert_with(|| {
                    h.kernel
                        .create_handle(Object::Semaphore { count: 0, max: 64 })
                });
                ok().handles(&[semaphore], false)
            }
            // GetServiceHandle(name, length, flags).
            0x5 => {
                let name = request.name(1, request.word(3) as usize);
                let session = h.connect_service(&name);
                ok().handles(&[session], true)
            }
            // Subscribe, Unsubscribe, PublishToSubscriber: nothing is ever
            // published.
            0x9 | 0xA | 0xC => ok(),
            // ReceiveNotification: none.
            0xB => ok().word(0),
            // IsServiceRegistered: every service is.
            0xE => ok().word(1),
            _ => h.unhandled("srv:", request),
        }
    }
}

/// The error display port, "err:f" (3dbrew, "ERR:Throw"). A fatal error stops
/// the process with what it reported.
pub struct ErrF;

impl Service for ErrF {
    fn name(&self) -> &str {
        "err:f"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        if request.command() == 1 {
            // The error record starts at word 1: its type in the low byte,
            // then the result code and the program counter.
            let kind = request.word(1) & 0xFF;
            let (code, pc) = (request.word(2), request.word(3));
            // A failure carries a message from byte 0x20 on.
            let text: String = request.words[1..]
                .iter()
                .skip(8)
                .flat_map(|w| w.to_le_bytes())
                .take_while(|&b| b != 0)
                .map(|b| {
                    if b.is_ascii_graphic() || b == b' ' {
                        b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            h.stop(format!(
                "the program reported a fatal error: type {kind}, result {code:#010x}, \
                 pc {pc:#010x}{}",
                if kind == 4 && !text.is_empty() {
                    format!(", \"{text}\"")
                } else {
                    String::new()
                }
            ));
        }
        Reply::new(request.command(), result::SUCCESS)
    }
}

/// `ConnectToPort` with a name nobody has registered.
pub const PORT_NOT_FOUND: ResultCode = ResultCode(0xD88007FA);

/// The ports a process can connect to by name.
pub fn port(name: &str) -> Option<Box<dyn Service>> {
    match name {
        "srv:" => Some(Box::new(Srv::default())),
        "err:f" => Some(Box::new(ErrF)),
        _ => None,
    }
}

/// The one service behind names that are only different doors to it.
pub fn canonical(name: &str) -> &str {
    match name {
        "APT:A" | "APT:S" | "APT:U" => "APT",
        "cfg:u" | "cfg:s" | "cfg:i" => "cfg",
        "hid:USER" | "hid:SPVR" => "hid",
        "qtm:u" | "qtm:s" | "qtm:sp" => "qtm",
        other => other,
    }
}

/// The service registered under `name` (a canonical name), if one is
/// written.
pub fn standard(name: &str) -> Option<Box<dyn Service>> {
    Some(match name {
        "APT" => Box::new(apt::Apt::default()),
        "fs:USER" => Box::new(fs::Fs::default()),
        "cfg" => Box::new(cfg::Cfg),
        "dsp::DSP" => Box::new(dsp::Dsp::default()),
        "gsp::Gpu" => Box::new(gsp::Gsp::default()),
        "gsp::Lcd" => gsp::lcd(),
        "ldr:ro" => ldr_ro::service(),
        "hid" => Box::new(hid::Hid::default()),
        "ndm:u" => Box::new(ndm::Ndm::default()),
        "news:u" => simple::news(),
        "nim:aoc" => simple::nim_aoc(),
        "dlp:SRVR" => simple::dlp_server(),
        "dlp:FKCL" => simple::dlp_fake_client(),
        "qtm" => simple::qtm(),
        "pxi:dev" => simple::pxi_dev(),
        "$hioFIO" => simple::host_io("$hioFIO"),
        "$hostio0" => simple::host_io("$hostio0"),
        "$hostio1" => simple::host_io("$hostio1"),
        _ => return None,
    })
}
