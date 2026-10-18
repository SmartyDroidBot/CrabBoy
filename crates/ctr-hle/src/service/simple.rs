//! Services whose every answer is a table (see `table.rs`): what they serve
//! needs hardware or company this console does not have.

use crate::service::table::{ok, words, Answer, Fixed};
use crate::service::Service;

/// `dlp:SRVR`, the Download Play server (3dbrew, "DLP Services"). A game
/// asks it whether it is itself a Download Play child: it is not. The rest
/// is a server nobody joins.
pub fn dlp_server() -> Box<dyn Service> {
    static ANSWERS: &[Answer] = &[
        // Initialize, Finalize.
        ok(0x01),
        ok(0x02),
        // GetServerState: idle.
        words(0x03, &[0]),
        // GetEventDescription: 0x18 bytes, nothing happened.
        words(0x04, &[0; 6]),
        // StartAccepting, EndAccepting, StartDistribution,
        // SendWirelessRebootPassphrase, AcceptClient, DisconnectClient.
        ok(0x05),
        ok(0x06),
        ok(0x07),
        ok(0x08),
        ok(0x09),
        ok(0x0A),
        // GetClientState.
        words(0x0D, &[0; 3]),
        // IsChild(process): no.
        words(0x0E, &[0]),
        // InitializeWithName.
        ok(0x0F),
        // GetDupNoticeNeed: no.
        words(0x10, &[0]),
    ];
    Fixed::boxed("dlp:SRVR", ANSWERS)
}

/// `dlp:FKCL`, the Download Play fake client (3dbrew, "DLP Services"): its
/// scans find no server.
pub fn dlp_fake_client() -> Box<dyn Service> {
    static ANSWERS: &[Answer] = &[
        // Initialize, Finalize.
        ok(0x01),
        ok(0x02),
        // GetEventDesc: 0x18 bytes of nothing.
        words(0x03, &[0; 6]),
        // GetChannels.
        words(0x04, &[0]),
        // StartScan, StopScan.
        ok(0x05),
        ok(0x06),
        // DeleteScanInfo.
        ok(0x0A),
        // GetMyStatus: idle.
        words(0x0C, &[0; 2]),
        // StopSession; Initialize again.
        ok(0x10),
        ok(0x11),
    ];
    Fixed::boxed("dlp:FKCL", ANSWERS)
}

/// `nim:aoc`, the shop's add-on content side (3dbrew, "NIM Services"): set
/// up, with nothing bought.
pub fn nim_aoc() -> Box<dyn Service> {
    static ANSWERS: &[Answer] = &[
        // SetApplicationId, SetTin.
        ok(0x03),
        ok(0x04),
        // GetBalance: none.
        words(0x18, &[0; 2]),
        // GetCustomerSupportCode.
        words(0x1D, &[0]),
        // Initialize, RefreshServerTime.
        ok(0x21),
        ok(0x25),
    ];
    Fixed::boxed("nim:aoc", ANSWERS)
}

/// `qtm:u` (and `:s`, `:sp`), head tracking (3dbrew, "QTM Services"). It
/// belongs to the New 3DS; on this Old 3DS no face is ever tracked and the
/// barrier never moves.
pub fn qtm() -> Box<dyn Service> {
    static ANSWERS: &[Answer] = &[
        // GetRawTrackingDataEx, GetTrackingDataEx: no face, all zeros.
        words(0x01, &[0; 18]),
        words(0x02, &[0; 18]),
        // EnableManualIrLedControl, DisableManualIrLedControl,
        // SetIrLedStatus.
        ok(0x03),
        ok(0x04),
        ok(0x05),
        // IsCurrentAppBlacklisted: no.
        words(0x06, &[0]),
    ];
    Fixed::boxed("qtm", ANSWERS)
}

/// `pxi:dev` (3dbrew, "PXI Services"): the debug unit's link to the ARM9;
/// a retail console's has nothing to publish.
pub fn pxi_dev() -> Box<dyn Service> {
    static ANSWERS: &[Answer] = &[ok(0x01)];
    Fixed::boxed("pxi:dev", ANSWERS)
}

/// `$hioFIO`, `$hostio0` and `$hostio1`: file and console access to a
/// development host, listed by titles built with the debug libraries. No
/// host is attached; a title on a retail console never opens them.
pub fn host_io(name: &'static str) -> Box<dyn Service> {
    static ANSWERS: &[Answer] = &[];
    Fixed::boxed(name, ANSWERS)
}

/// `news:u` (3dbrew, "News Services"): notifications go nowhere, and there
/// are none to read.
pub fn news() -> Box<dyn Service> {
    static ANSWERS: &[Answer] = &[
        // AddNotification, ResetNotifications.
        ok(0x01),
        ok(0x04),
        // GetTotalNotifications.
        words(0x05, &[0]),
        // SetNewsDBHeader, SetNotificationHeader, SetNotificationMessage,
        // SetNotificationImage.
        ok(0x06),
        ok(0x07),
        ok(0x08),
        ok(0x09),
        // SetInfoLEDPattern, SyncArrivedNotifications,
        // SyncOneArrivedNotification, SetAutomaticSyncFlag.
        ok(0x0E),
        ok(0x0F),
        ok(0x10),
        ok(0x11),
        // WriteNewsDBSavedata, GetTotalArrivedNotifications.
        ok(0x13),
        words(0x14, &[0]),
    ];
    Fixed::boxed("news:u", ANSWERS)
}

#[cfg(test)]
mod tests {
    use crate::service::testing::{console, request};

    #[test]
    fn a_game_is_not_a_download_play_child() {
        let mut h = console();
        let reply = request(&mut h, "dlp:SRVR", &[0x000E_0040, 0]);
        assert_eq!(reply, [0x000E_0080, 0, 0]);
        let reply = request(&mut h, "news:u", &[0x0005_0000]);
        assert_eq!(reply, [0x0005_0080, 0, 0]);
    }
}
