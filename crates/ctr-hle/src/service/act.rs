//! Accounts, `act:u` (and `act:a`) (3dbrew, "ACT Services").
//!
//! The console has one console account, in slot 1, never linked to a
//! Nintendo Network ID: its account id is empty and its principal id 0, its
//! Mii name is "CrabBoy" and its country that of the settings. A request to
//! the account server ends at once, failed, as on a console with no
//! network.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::result::{self, ResultCode};
use crate::service::table::{signal_handle, to_mapped, utf16};
use crate::service::Service;

/// Not online: permanent, invalid state, module ACT (83), description 1016
/// (not initialized). Composed from the fields 3dbrew documents.
const OFFLINE: ResultCode = ResultCode(0xD8A1_4FF8);
/// The account's persistent id, the first a console hands out.
const PERSISTENT_ID: u32 = 0x8000_0001;
/// Its transferable id base: the device's half and the account's.
const TRANSFERABLE_ID_BASE: u64 = 0x0000_0001_0000_0001;
/// A UUID for the console account.
const UUID: [u8; 16] = *b"CrabBoyConsole01";
const MII_NAME: &str = "CrabBoy";
/// The country, as cfg's CountryInfo word: state 2, the United States.
const SIMPLE_ADDRESS_ID: u32 = 0x3102_0000;

/// GetCommonInfo's data blocks.
fn common_info(block: u32) -> Option<Vec<u8>> {
    Some(match block {
        // Number of accounts, the current slot, the default slot.
        0x1..=0x3 => vec![1],
        // NetworkTimeDifference.
        0x4 => vec![0; 8],
        0x6 => TRANSFERABLE_ID_BASE.to_le_bytes().to_vec(),
        // IsApplicationUpdateRequired: no.
        0x22 => vec![0],
        // Default account server types: production.
        0x23 => vec![0; 4],
        // The default account's server environment: production, no
        // subdomain.
        0x29 => vec![0; 0x24],
        // DeviceHash.
        0x2A => vec![0; 8],
        _ => return None,
    })
}

/// GetAccountDataBlock's data blocks.
fn account_info(block: u32) -> Option<Vec<u8>> {
    let birth_date = || {
        let mut b = 2000u16.to_le_bytes().to_vec();
        b.extend([1, 1]);
        b
    };
    Some(match block {
        0x5 => PERSISTENT_ID.to_le_bytes().to_vec(),
        0x6 => TRANSFERABLE_ID_BASE.to_le_bytes().to_vec(),
        // Mii: none set.
        0x7 => vec![0; 0x60],
        // AccountId: no Nintendo Network ID.
        0x8 | 0x15 => vec![0; 0x11],
        0xA => birth_date(),
        0xB => b"US\0".to_vec(),
        // PrincipalId: none.
        0xC => vec![0; 4],
        0xE => vec![0],
        0x11 => {
            let mut b = PERSISTENT_ID.to_le_bytes().to_vec();
            b.extend([0; 4]);
            b.extend(TRANSFERABLE_ID_BASE.to_le_bytes());
            b.extend([0; 0x60]);
            b.extend(utf16(MII_NAME, 0x16));
            b.extend([0; 0x12]);
            b.extend(birth_date());
            b.extend([0; 4]);
            b
        }
        // Server types; gender, last authentication result.
        0x12 => vec![0; 4],
        0x13 | 0x14 | 0x16 => vec![0],
        0x17 | 0x2F => SIMPLE_ADDRESS_ID.to_le_bytes().to_vec(),
        0x19 => vec![0; 8],
        // IsCommitted.
        0x1A => vec![1],
        0x1B => utf16(MII_NAME, 0x16),
        0x1C => vec![0; 0x11],
        0x1D | 0x1F | 0x20 | 0x24 | 0x27 | 0x2D | 0x2E => vec![0],
        0x1E => vec![0; 0x41],
        0x25 => vec![0; 0x101],
        0x26 => vec![0; 4],
        0x28 => vec![0; 0x24],
        // FpLocalAccountId: friends' first account.
        0x2B => vec![1],
        // Age.
        0x2C => 20u16.to_le_bytes().to_vec(),
        _ => return None,
    })
}

#[derive(Default)]
pub struct Act {
    last: u32,
}

impl Act {
    /// The block a GetCommonInfo or GetAccountDataBlock asks for, into the
    /// mapped buffer described at word `at`.
    fn block(&self, h: &mut Horizon, request: &Request, data: Option<Vec<u8>>, at: usize) -> Reply {
        let command = request.command();
        let data = data.unwrap_or_else(|| {
            h.note(format!(
                "act:u data block {:#x} is not written out; it reads as zeros",
                request.word(at - 1)
            ));
            Vec::new()
        });
        let mut bytes = data;
        bytes.resize((request.word(at) >> 4) as usize, 0);
        let reply = Reply::new(command, result::SUCCESS);
        to_mapped(h, reply, request.word(at), request.word(at + 1), &bytes)
    }
}

impl Service for Act {
    fn name(&self) -> &str {
        "act:u"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        match command {
            // Initialize(SDK version, shared memory size, process, shared
            // memory).
            0x01 => ok(),
            // ResultToErrorCode(result): no number to show.
            0x02 => ok().word(0),
            0x03 => Reply::new(command, ResultCode(self.last)),
            // Cancel.
            0x04 => ok(),
            // GetCommonInfo(size, block, buffer).
            0x05 => {
                let data = common_info(request.word(2));
                self.block(h, request, data, 3)
            }
            // GetAccountDataBlock(slot, size, block, buffer).
            0x06 => {
                let data = account_info(request.word(3));
                self.block(h, request, data, 4)
            }
            // GetAsyncResult(size, ..., buffer): nothing read.
            0x07 => ok().word(0),
            // SetIsApplicationUpdateRequired, Save.
            0x0A | 0x10 => ok(),
            // GenerateUuid(unique id), GetUuid(slot, unique id).
            0x0D | 0x0E => ok().words(&crate::service::table::words_of(&UUID)),
            // FindSlotNoByUuid: the console account.
            0x0F => ok().word(1),
            // GetTransferableId(slot, unique id).
            0x11 => ok().u64(TRANSFERABLE_ID_BASE),
            // The server requests: account information, tokens, Miis,
            // EULAs and time zones, each with an event last. They fail at
            // once.
            0x0B | 0x0C | 0x12 | 0x14 | 0x16..=0x1A | 0x1D | 0x1E | 0x1F | 0x20 => {
                self.last = OFFLINE.0;
                let event = request.word(request.words.len().saturating_sub(1));
                signal_handle(h, event);
                ok()
            }
            _ => h.unhandled("act:u", request),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::testing::{connect, console, read_bytes, send, HEAP};

    #[test]
    fn the_console_account_has_no_network_id() {
        let mut h = console();
        let act = connect(&mut h, "act:u");
        // GetCommonInfo: the number of accounts.
        send(&mut h, act, &[0x0005_0082, 1, 1, 1 << 4 | 0xC, HEAP]);
        assert_eq!(read_bytes(&h, HEAP, 1), [1]);
        // GetAccountDataBlock: the principal id and the Mii name.
        send(
            &mut h,
            act,
            &[0x0006_00C2, 0xFE, 4, 0xC, 4 << 4 | 0xC, HEAP],
        );
        assert_eq!(read_bytes(&h, HEAP, 4), [0; 4]);
        send(
            &mut h,
            act,
            &[0x0006_00C2, 0xFE, 0x16, 0x1B, 0x16 << 4 | 0xC, HEAP],
        );
        assert_eq!(read_bytes(&h, HEAP, 4), [b'C', 0, b'r', 0]);
        assert_eq!(account_info(0x11).unwrap().len(), 0xA0);
        assert!(h.log.is_empty(), "{:?}", h.log);
    }
}
