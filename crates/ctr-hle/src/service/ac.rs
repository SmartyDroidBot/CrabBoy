//! The connection manager, `ac:u` (and `ac:i`) (3dbrew, "AC Services").
//!
//! An application builds a connection configuration with AC's help, asks
//! for a connection and waits on an event. This console has no wireless
//! network: a connection attempt ends at once, with its event signalled and
//! a failure to collect, and every question about the connected access
//! point fails the same way.

use crate::horizon::Horizon;
use crate::ipc::{self, Reply, Request};
use crate::result::{self, ResultCode};
use crate::service::table::{signal_handle as signal, to_static};
use crate::service::Service;

/// Not connected: usage level, invalid state, module AC (39),
/// description 302. Composed from the fields 3dbrew documents.
pub const NOT_CONNECTED: ResultCode = ResultCode(0xE0A0_9D2E);
/// The size of a connection configuration.
const CONFIG_LEN: usize = 0x200;

#[derive(Default)]
pub struct Ac {
    /// What GetConnectResult answers.
    connect_result: u32,
}

/// The configuration a client passes in a static buffer described at word
/// `at`, handed back changed as the command says (here: unchanged) in its
/// static buffer 0.
fn config_back(h: &mut Horizon, request: &Request, at: usize) -> Reply {
    let len = ipc::static_buffer(request.word(at)).unwrap_or(0) as usize;
    let config = h
        .process
        .space
        .read_bytes(&h.mem, request.word(at + 1), len.min(CONFIG_LEN))
        .unwrap_or_default();
    to_static(
        h,
        Reply::new(request.command(), result::SUCCESS),
        0,
        &config,
    )
}

impl Service for Ac {
    fn name(&self) -> &str {
        "ac:u"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        let offline = || Reply::new(command, NOT_CONNECTED);
        match command {
            // CreateDefaultConfig: a configuration of zeros.
            0x01 => to_static(h, ok(), 0, &[0; CONFIG_LEN]),
            // ConnectAsync(process, event, configuration): over at once,
            // having failed.
            0x04 => {
                self.connect_result = NOT_CONNECTED.0;
                signal(h, request.word(4));
                ok()
            }
            0x05 => Reply::new(command, ResultCode(self.connect_result)),
            // CancelConnectAsync.
            0x07 => ok(),
            // CloseAsync(process, event): nothing to close.
            0x08 => {
                signal(h, request.word(4));
                ok()
            }
            0x09 => ok(),
            // GetLastErrorCode, GetLastDetailErrorCode.
            0x0A | 0x0B => ok().word(0),
            // GetStatus, GetWifiStatus: not connected.
            0x0C | 0x0D => ok().word(0),
            // GetCurrentAPInfo, GetConnectingInfraPriority and the
            // connected hotspot's details.
            0x0E..=0x14 => offline(),
            // SetAllowApType, AddAllowApType, AddDenyApType, SetNetworkArea,
            // SetInfraPriority, SetPowerSaveMode, SetFromApplication(value,
            // configuration).
            0x22..=0x26 | 0x28 | 0x2C => config_back(h, request, 2),
            // GetInfraPriority(configuration), GetPowerSaveMode.
            0x27 | 0x29 => ok().word(0),
            // SetRequestEulaVersion(major, minor, configuration).
            0x2D => config_back(h, request, 3),
            // RegisterDisconnectEvent(process, event): it never comes.
            0x30 => ok(),
            // The connected network's security, name and proxy.
            0x33..=0x3B => offline(),
            // IsConnected: no.
            0x3E => ok().word(0),
            // SetClientVersion(version, process).
            0x40 => ok(),
            _ => h.unhandled("ac:u", request),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::{Object, Reset};
    use crate::service::testing::{connect, console, event, send};

    #[test]
    fn a_connection_fails_at_once_and_says_so() {
        let mut h = console();
        let ac = connect(&mut h, "ac:u");
        assert_eq!(send(&mut h, ac, &[0x000D_0000]), [0x000D_0080, 0, 0]);
        let wait = h.kernel.create_handle(Object::Event {
            signalled: false,
            reset: Reset::OneShot,
        });
        let reply = send(&mut h, ac, &[0x0004_0006, 0x20, 0, 0, wait, 0, 0]);
        assert_eq!(reply, [0x0004_0040, 0]);
        assert_eq!(event(&h, wait), Some(true));
        let reply = send(&mut h, ac, &[0x0005_0002, 0x20, 0]);
        assert_eq!(reply[1], NOT_CONNECTED.0);
    }
}
