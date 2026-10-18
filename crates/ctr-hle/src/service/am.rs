//! The application manager's application door, `am:app` (3dbrew,
//! "Application Manager Services").
//!
//! A game asks it about its downloadable content and patches. Nothing is
//! installed besides the game itself: a DLC or patch title is not found, it
//! has no tickets, and nothing is in use.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::result::{self, ResultCode};
use crate::service::Service;

/// A title that is not installed: permanent, not found, module AM (32),
/// description 1018 (not found). Composed from the fields 3dbrew
/// documents.
pub const TITLE_NOT_FOUND: ResultCode = ResultCode(0xD880_83FA);

#[derive(Default)]
pub struct Am;

impl Service for Am {
    fn name(&self) -> &str {
        "am:app"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        let missing = || Reply::new(command, TITLE_NOT_FOUND);
        match command {
            // GetDLCContentInfoCount, FindDLCContentInfos,
            // ListDLCContentInfos, DeleteDLCContents, GetDLCTitleInfos,
            // ListExistingDLCContentInfos, GetPatchTitleInfos: no such
            // title.
            0x1001 | 0x1003 | 0x100C => missing().word(0),
            0x1002 | 0x1004 | 0x1005 | 0x100D => missing(),
            // GetDLCOrLicenseNumTickets, ListDLCOrLicenseTicketInfos: none.
            0x1006 | 0x1007 => ok().word(0),
            // GetDLCOrLicenseItemRights: none.
            0x1008 => missing(),
            // IsDLCTitleInUse: no.
            0x1009 => ok().word(0),
            // QueryAvailableExternalTitleDatabase: there is one, the SD
            // card's.
            0x100A => ok().word(1),
            // GetNumExistingDLCContentInfos.
            0x100B => ok().word(0),
            _ => h.unhandled("am:app", request),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::testing::{console, request};

    #[test]
    fn no_downloadable_content_is_installed() {
        let mut h = console();
        let reply = request(&mut h, "am:app", &[0x1001_00C0, 1, 0x1234, 0x0004_008C]);
        assert_eq!(reply, [0x1001_0080, TITLE_NOT_FOUND.0, 0]);
        let reply = request(&mut h, "am:app", &[0x1006_0080, 0x1234, 0x0004_008C]);
        assert_eq!(reply, [0x1006_0080, 0, 0]);
    }
}
