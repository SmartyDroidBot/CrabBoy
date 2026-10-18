//! System settings, `cfg:u` (and `cfg:s`, `cfg:i`) (3dbrew, "Config
//! Services", "Config Savegame").
//!
//! The settings are those of a console bought in the United States: English,
//! the USA region, stereo sound, the EULA agreed, parental controls off, an
//! Old 3DS. A block nobody has written out here reads as zeros and is noted.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::result::{self, ResultCode};
use crate::service::Service;

/// Region and language codes.
const REGION_USA: u32 = 1;
const LANGUAGE_ENGLISH: u8 = 1;
const COUNTRY_USA: u8 = 49;
/// `GetCountryCodeID` with a code nobody knows.
const NO_SUCH_COUNTRY: ResultCode = ResultCode(0xD8A1_03F9);
const USER_NAME: &str = "CrabBoy";
const COUNTRY_NAME: &str = "United States";

/// UTF-16 of `text`, `len` bytes, zero-filled.
fn utf16(text: &str, len: usize) -> Vec<u8> {
    let mut out: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    out.resize(len, 0);
    out
}

/// The contents of a configuration block, if it is written out here.
fn block(id: u32) -> Option<Vec<u8>> {
    Some(match id {
        // Sound output: stereo.
        0x0007_0001 => vec![1],
        // Backlight: power saving off, brightness level 2.
        0x0005_0001 => vec![0, 2],
        // The user's name (ten UTF-16 units), then two words of zero.
        0x000A_0000 => utf16(USER_NAME, 0x1C),
        // Birthday: 1 January.
        0x000A_0001 => vec![1, 1],
        0x000A_0002 => vec![LANGUAGE_ENGLISH],
        // Country information: two unknown bytes, the state, the country.
        0x000B_0000 => vec![0, 0, 2, COUNTRY_USA],
        // The country's name for each of sixteen languages.
        0x000B_0001 => utf16(COUNTRY_NAME, 0x80).repeat(16),
        0x000B_0002 => vec![0; 0x800],
        // Coordinates.
        0x000B_0003 => vec![0; 4],
        // Parental controls: none.
        0x000C_0000 => vec![0; 0xC0],
        0x000C_0001 => vec![0; 0x14],
        0x000C_0002 => vec![0; 0x200],
        // The EULA agreed to, and the latest: the highest versions.
        0x000D_0000 => vec![0x7F, 0x7F, 0x7F, 0x7F],
        // System model: an Old 3DS.
        0x000F_0004 => vec![0, 0, 0, 0],
        // Stereo camera and display calibration, user time offset.
        0x0005_0005 => vec![0; 0x20],
        0x0003_0001 => vec![0; 8],
        // TWL (DSi) EULA: agreed.
        0x0010_0000 => vec![1, 0x7F],
        _ => return None,
    })
}

#[derive(Default)]
pub struct Cfg;

impl Service for Cfg {
    fn name(&self) -> &str {
        "cfg"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        match command {
            // GetConfigInfoBlk2(size, block, output).
            0x0001 => {
                let (size, id, descriptor, addr) = (
                    request.word(1),
                    request.word(2),
                    request.word(3),
                    request.word(4),
                );
                let mut data = block(id).unwrap_or_else(|| {
                    h.note(format!(
                        "cfg block {id:#010x} is not written out; it reads as zeros"
                    ));
                    Vec::new()
                });
                data.resize(size.min(0x1000) as usize, 0);
                h.process.space.write_bytes(&mut h.mem, addr, &data);
                ok().mapped_buffer(descriptor, addr)
            }
            // SecureInfoGetRegion.
            0x0002 => ok().word(REGION_USA),
            // GenHashConsoleUnique(salt): two words of a hash that stays the
            // same for this console.
            0x0003 => {
                let salt = request.word(1) & 0xF_FFFF;
                ok().word(0x2E5C_A3B1 ^ salt).word(0x7F1A_0C45)
            }
            // GetRegionCanadaUSA: yes.
            0x0004 => ok().word(1),
            // GetSystemModel: the original 3DS.
            0x0005 => ok().word(0),
            // GetModelNintendo2DS: 1 for anything but a 2DS.
            0x0006 => ok().word(1),
            // GetCountryCodeString(country): two letters.
            0x0009 => match request.word(1) as u8 {
                COUNTRY_USA => ok().word(u16::from_le_bytes(*b"US") as u32),
                _ => Reply::new(command, NO_SUCH_COUNTRY).word(0),
            },
            // GetCountryCodeID(two letters).
            0x000A => match (request.word(1) as u16).to_le_bytes() {
                [b'U', b'S'] => ok().word(COUNTRY_USA as u32),
                _ => Reply::new(command, NO_SUCH_COUNTRY).word(0),
            },
            _ => h.unhandled("cfg", request),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_have_the_sizes_the_savegame_lists() {
        for (id, len) in [
            (0x000A_0000, 0x1C),
            (0x000A_0001, 2),
            (0x000A_0002, 1),
            (0x000B_0000, 4),
            (0x000B_0001, 0x800),
            (0x000C_0000, 0xC0),
            (0x000D_0000, 4),
        ] {
            assert_eq!(block(id).unwrap().len(), len, "{id:#x}");
        }
        assert_eq!(&block(0x000A_0000).unwrap()[..4], [b'C', 0, b'r', 0]);
        assert_eq!(block(0x1234_5678), None);
    }
}
