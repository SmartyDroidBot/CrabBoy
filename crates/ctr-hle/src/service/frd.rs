//! Friends, `frd:u` (and `frd:a`) (3dbrew, "Friend Services").
//!
//! The friend account of a console that has never been online: no
//! principal id or friend code yet (the server hands those out at the first
//! login), an empty friend list, the screen name "CrabBoy" and the profile
//! of the console's settings. A login attempt ends at once, failed, as on a
//! console with no network.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::result::{self, ResultCode};
use crate::service::table::{signal_handle as signal, to_mapped, to_static, utf16, words_of};
use crate::service::Service;

/// Not online: permanent, invalid state, module Friends (49), description
/// 1016 (not initialized). Composed from the fields 3dbrew documents.
const OFFLINE: ResultCode = ResultCode(0xD8A0_C7F8);
const SCREEN_NAME: &str = "CrabBoy";
/// Profile: region (USA), country (USA), area, language (English),
/// platform (always 2, the 3DS).
const PROFILE: [u8; 8] = [1, 49, 2, 1, 2, 0, 0, 0];
/// The sizes of a friend's records in lists.
const MII_LEN: usize = 0x60;
const PRESENCE_LEN: usize = 0x12C;
const SCREEN_NAME_LEN: usize = 0x16;
const PROFILE_LEN: usize = 8;
const GAME_KEY_LEN: usize = 0x10;
const PLAYING_GAME_LEN: usize = 0x112;
const INFO_LEN: usize = 0xE0;
const COMMENT_LEN: usize = 0x22;

#[derive(Default)]
pub struct Frd {
    /// The result GetLastResponseResult reports.
    last: u32,
    sdk_version: u32,
}

impl Frd {
    /// A failed request to the friend server: its event signalled, its
    /// result kept for GetLastResponseResult.
    fn fail(&mut self, h: &mut Horizon, event: u32) {
        self.last = OFFLINE.0;
        signal(h, event);
    }
}

impl Service for Frd {
    fn name(&self) -> &str {
        "frd:u"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        // A list of `count` (word 1) records of `len` bytes, all zeros:
        // nobody is on the list to ask about.
        let zeros = |len: usize| vec![0u8; request.word(1) as usize * len];
        match command {
            // HasLoggedIn, IsOnline: no.
            0x01 | 0x02 => ok().word(0),
            // Login(event): fails at once.
            0x03 => {
                self.fail(h, request.word(2));
                ok()
            }
            0x04 => ok(),
            // GetMyFriendKey: principal id, padding, friend code; none yet.
            0x05 => ok().words(&[0; 4]),
            // GetMyPreference: public, showing the game and the history.
            0x06 => ok().words(&[1, 1, 1]),
            0x07 => ok().words(&words_of(&PROFILE)),
            // GetMyPresence: nothing to show, into static buffer 0.
            0x08 => to_static(h, ok(), 0, &[0; PRESENCE_LEN]),
            0x09 => ok().words(&words_of(&utf16(SCREEN_NAME, SCREEN_NAME_LEN))),
            // GetMyMii: no Mii chosen.
            0x0A => ok().words(&[0; MII_LEN / 4]),
            // GetMyLocalAccountId: the first.
            0x0B => ok().word(1),
            // GetMyPlayingGame: this program, version 0.
            0x0C => {
                let title = h.program_id;
                ok().u64(title).words(&[0, 0])
            }
            // GetMyFavoriteGame: none.
            0x0D => ok().words(&[0; 4]),
            // GetMyNcPrincipalId.
            0x0E => ok().word(0),
            // GetMyComment: empty.
            0x0F => ok().words(&[0; COMMENT_LEN.div_ceil(4)]),
            // GetMyPassword(size): none.
            0x10 => to_static(h, ok(), 0, &vec![0; request.word(1) as usize]),
            // GetFriendKeyList(offset, count): nobody.
            0x11 => to_static(h, ok().word(0), 0, &[]),
            // GetFriendPresence, GetFriendScreenName, GetFriendProfile,
            // GetFriendRelationship, GetFriendAttributeFlags,
            // GetFriendFavoriteGame, GetFriendComment(size, count): records
            // of nobody in static buffer 0.
            0x12 => to_static(h, ok(), 0, &zeros(PRESENCE_LEN)),
            0x13 => {
                let count = request.word(3) as usize;
                to_static(h, ok(), 0, &vec![0; count * SCREEN_NAME_LEN])
            }
            0x15 => to_static(h, ok(), 0, &zeros(PROFILE_LEN)),
            0x16 => to_static(h, ok(), 0, &zeros(1)),
            0x17 => to_static(h, ok(), 0, &zeros(4)),
            0x19 => to_static(h, ok(), 0, &zeros(GAME_KEY_LEN)),
            0x31 => {
                let count = request.word(2) as usize;
                to_static(h, ok(), 0, &vec![0; count * COMMENT_LEN])
            }
            // GetFriendMii, GetFriendPlayingGame, GetFriendInfo: into a
            // mapped buffer.
            0x14 | 0x18 => {
                let len = if command == 0x14 {
                    MII_LEN
                } else {
                    PLAYING_GAME_LEN
                };
                let bytes = zeros(len);
                to_mapped(h, ok(), request.word(4), request.word(5), &bytes)
            }
            0x1A => {
                let bytes = zeros(INFO_LEN);
                to_mapped(h, ok(), request.word(6), request.word(7), &bytes)
            }
            // IsIncludedInFriendList(friend code): no.
            0x1B => ok().word(0),
            // UpdateGameModeDescription, UpdateMyPresence, SendInvitation,
            // AttachToEventNotification, SetNotificationMask.
            0x1D..=0x21 => ok(),
            // GetEventNotification(count): none.
            0x22 => to_static(h, ok().words(&[0, 0]), 0, &[]),
            0x23 => Reply::new(command, ResultCode(self.last)),
            // IsValidFriendCode: none is, with no server to ask.
            0x26 => ok().word(0),
            // ResultToErrorCode(result): no error number to show.
            0x27 => ok().word(0),
            // RequestGameAuthentication, RequestServiceLocator(..., event):
            // fail at once.
            0x28 | 0x2A => {
                self.fail(h, request.word(request.words.len().saturating_sub(1)));
                ok()
            }
            // GetGameAuthenticationData, GetServiceLocatorData: empty.
            0x29 => to_static(h, ok(), 0, &[0; 0x138]),
            0x2B => to_static(h, ok(), 0, &[0; 0x198]),
            // DetectNatProperties(event): fails at once.
            0x2C => {
                self.fail(h, request.word(2));
                ok()
            }
            // GetNatProperties, GetServerTimeDifference.
            0x2D => ok().words(&[0, 0]),
            0x2E => ok().u64(0),
            // AllowHalfAwake.
            0x2F => ok(),
            // GetServerTypes: the production servers.
            0x30 => ok().words(&[0, 0, 0]),
            // SetClientSdkVersion(version, process).
            0x32 => {
                self.sdk_version = request.word(1);
                ok()
            }
            // GetExtendedNatProperties.
            0x36 => ok().words(&[0, 0, 0]),
            _ => h.unhandled("frd:u", request),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::service::testing::{connect, console, send};

    #[test]
    fn a_console_never_online_has_no_friends_and_its_own_name() {
        let mut h = console();
        let frd = connect(&mut h, "frd:u");
        assert_eq!(
            send(&mut h, frd, &[0x0032_0042, 0x7000_0000, 0x20, 0]),
            [0x0032_0040, 0]
        );
        assert_eq!(
            send(&mut h, frd, &[0x0005_0000]),
            [0x0005_0140, 0, 0, 0, 0, 0]
        );
        let list = send(&mut h, frd, &[0x0011_0080, 0, 100]);
        assert_eq!(list[..3], [0x0011_0082, 0, 0]);
        let name = send(&mut h, frd, &[0x0009_0000]);
        assert_eq!(name[2], u32::from_le_bytes([b'C', 0, b'r', 0]));
        assert_eq!(send(&mut h, frd, &[0x0001_0000])[2], 0);
    }
}
