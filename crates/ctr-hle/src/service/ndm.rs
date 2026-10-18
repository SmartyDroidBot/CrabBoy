//! The network daemon manager, `ndm:u` (3dbrew, "NDM Services").
//!
//! NDM starts and stops the background daemons (StreetPass, SpotPass, the
//! shop, friends) around what an application does with the network. There
//! is no network here, so it never leaves its first state; what it keeps is
//! what applications set, so they read back what they asked for.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::result;
use crate::service::Service;

/// Every daemon: CEC, BOSS, NIM, friends.
const ALL_DAEMONS: u32 = 0xF;
/// `DAEMON_STATUS_IDLE` and `DAEMON_STATUS_SUSPENDED`.
const IDLE: u32 = 1;
const SUSPENDED: u32 = 3;
/// The scan interval a console starts with, in seconds.
const SCAN_INTERVAL: u32 = 30;

pub struct Ndm {
    exclusive: u32,
    default_daemons: u32,
    /// Daemons suspended, a bit each.
    suspended: u32,
    scheduler_suspended: bool,
    scan_interval: u32,
    retry_interval: u32,
}

impl Default for Ndm {
    fn default() -> Self {
        Ndm {
            exclusive: 0,
            default_daemons: ALL_DAEMONS,
            suspended: 0,
            scheduler_suspended: false,
            scan_interval: SCAN_INTERVAL,
            retry_interval: 0,
        }
    }
}

impl Service for Ndm {
    fn name(&self) -> &str {
        "ndm:u"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        match command {
            // EnterExclusiveState(state, process), LeaveExclusiveState.
            0x01 => {
                self.exclusive = request.word(1);
                ok()
            }
            0x02 => {
                self.exclusive = 0;
                ok()
            }
            // QueryExclusiveMode.
            0x03 => ok().word(self.exclusive),
            // LockState, UnlockState.
            0x04 | 0x05 => ok(),
            // SuspendDaemons(mask), ResumeDaemons(mask).
            0x06 => {
                self.suspended |= request.word(1) & ALL_DAEMONS;
                ok()
            }
            0x07 => {
                self.suspended &= !request.word(1);
                ok()
            }
            // SuspendScheduler(asynchronous), ResumeScheduler.
            0x08 | 0x09 => {
                self.scheduler_suspended = command == 0x08;
                ok()
            }
            // GetCurrentState, GetTargetState: the first state, initial.
            0x0A | 0x0B => ok().word(0),
            0x0C => ok(),
            // QueryStatus(daemon).
            0x0D => {
                let suspended = self.suspended >> (request.word(1) & 3) & 1 != 0;
                ok().word(if suspended { SUSPENDED } else { IDLE })
            }
            // GetDaemonDisableCount(daemon), GetSchedulerDisableCount: the
            // count for this process and for all.
            0x0E => {
                let n = self.suspended >> (request.word(1) & 3) & 1;
                ok().words(&[n, n])
            }
            0x0F => {
                let n = self.scheduler_suspended as u32;
                ok().words(&[n, n])
            }
            // SetScanInterval, GetScanInterval, SetRetryInterval,
            // GetRetryInterval.
            0x10 => {
                self.scan_interval = request.word(1);
                ok()
            }
            0x11 => ok().word(self.scan_interval),
            0x12 => {
                self.retry_interval = request.word(1);
                ok()
            }
            0x13 => ok().word(self.retry_interval),
            // OverrideDefaultDaemons(mask), ResetDefaultDaemons,
            // GetDefaultDaemons.
            0x14 => {
                self.default_daemons = request.word(1) & ALL_DAEMONS;
                ok()
            }
            0x15 => {
                self.default_daemons = ALL_DAEMONS;
                ok()
            }
            0x16 => ok().word(self.default_daemons),
            // ClearHalfAwakeMacFilter.
            0x17 => ok(),
            _ => h.unhandled("ndm:u", request),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::service::testing::{connect, console, send};

    #[test]
    fn daemons_read_back_as_they_were_left() {
        let mut h = console();
        let ndm = connect(&mut h, "ndm:u");
        assert_eq!(send(&mut h, ndm, &[0x0014_0040, 0x9]), [0x0014_0040, 0]);
        assert_eq!(send(&mut h, ndm, &[0x0016_0000]), [0x0016_0080, 0, 0x9]);
        send(&mut h, ndm, &[0x0006_0040, 0x2]);
        assert_eq!(send(&mut h, ndm, &[0x000D_0040, 1])[2], 3);
        send(&mut h, ndm, &[0x0007_0040, 0x2]);
        assert_eq!(send(&mut h, ndm, &[0x000D_0040, 1])[2], 1);
        assert!(h.log.is_empty(), "{:?}", h.log);
    }
}
