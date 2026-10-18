//! Power and time, `ptm:u` (3dbrew, "PTM Services").
//!
//! The console here is always on its charger with a full battery and its
//! shell open, and nobody has walked with it: the pedometer reads no steps.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::result;
use crate::service::table::to_mapped;
use crate::service::Service;

/// GetBatteryLevel: 0 (empty) to 5 (full).
const BATTERY_FULL: u32 = 5;

#[derive(Default)]
pub struct Ptm {
    alarm: u64,
    pedometer_recording: u32,
}

impl Service for Ptm {
    fn name(&self) -> &str {
        "ptm:u"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        match command {
            // RegisterAlarmClient.
            0x01 => ok(),
            // SetRtcAlarm(time), GetRtcAlarm, CancelRtcAlarm.
            0x02 => {
                self.alarm = request.u64(1);
                ok()
            }
            0x03 => ok().u64(self.alarm),
            0x04 => {
                self.alarm = 0;
                ok()
            }
            // GetAdapterState: plugged in. GetShellState: open.
            0x05 | 0x06 => ok().word(1),
            0x07 => ok().word(BATTERY_FULL),
            // GetBatteryChargeState: charging, as a console on its charger.
            0x08 => ok().word(1),
            // GetPedometerState: not counting.
            0x09 => ok().word(0),
            // GetStepHistoryEntry, GetStepHistory(hours, start), and
            // GetStepHistoryAll: a step count of zero for every hour, into
            // a mapped buffer.
            0x0A | 0x0B | 0x0F => {
                let (descriptor, addr) = match command {
                    0x0A => (request.word(2), request.word(3)),
                    0x0B => (request.word(4), request.word(5)),
                    _ => (request.word(3), request.word(4)),
                };
                let zeros = vec![0; (descriptor >> 4) as usize];
                to_mapped(h, ok(), descriptor, addr, &zeros)
            }
            // GetTotalStepCount.
            0x0C => ok().word(0),
            // SetPedometerRecordingMode, GetPedometerRecordingMode.
            0x0D => {
                self.pedometer_recording = request.word(1);
                ok()
            }
            0x0E => ok().word(self.pedometer_recording),
            _ => h.unhandled("ptm:u", request),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::service::testing::{connect, console, send};

    #[test]
    fn the_console_is_charging_with_a_full_battery_and_its_shell_open() {
        let mut h = console();
        let ptm = connect(&mut h, "ptm:u");
        assert_eq!(send(&mut h, ptm, &[0x0007_0000]), [0x0007_0080, 0, 5]);
        assert_eq!(send(&mut h, ptm, &[0x0005_0000])[2], 1);
        assert_eq!(send(&mut h, ptm, &[0x0006_0000])[2], 1);
        assert_eq!(send(&mut h, ptm, &[0x0008_0000])[2], 1);
        assert_eq!(send(&mut h, ptm, &[0x000C_0000])[2], 0);
    }
}
