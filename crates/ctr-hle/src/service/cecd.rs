//! StreetPass, `cecd:u` (3dbrew, "CECD Services").
//!
//! A title keeps a message box in CECD's storage: a box list, the box's
//! information, an inbox and an outbox, each a file named by the title's id
//! and a path type. The files a title writes are kept here for as long as
//! the console runs, so it reads back what it wrote; no other console ever
//! passes by, so nothing arrives and the daemon stays idle.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::kernel::ObjId;
use crate::result::{self, ResultCode};
use crate::service::table::{event_handle, from_mapped, to_mapped};
use crate::service::Service;
use std::collections::BTreeMap;

/// No such file: permanent, not found, module CEC (66), description 1018
/// (not found). Composed from the fields 3dbrew documents.
const NOT_FOUND: ResultCode = ResultCode(0xD881_0BFA);
/// `CEC_STATE_ABBREV_IDLE`.
const IDLE: u32 = 1;
/// The write bit of an open flag.
const WRITE: u32 = 4;

#[derive(Default)]
pub struct Cecd {
    /// Files by program id and path type.
    files: BTreeMap<(u32, u32), Vec<u8>>,
    /// The file `Open` chose, for `Read` and `Write`.
    open: Option<(u32, u32)>,
    info_event: Option<ObjId>,
    state_event: Option<ObjId>,
}

impl Cecd {
    /// Open file `key`, creating it if `flags` ask to write: its size.
    fn open(&mut self, key: (u32, u32), flags: u32) -> Result<u32, ResultCode> {
        match self.files.get(&key) {
            Some(file) => Ok(file.len() as u32),
            None if flags & WRITE != 0 => {
                self.files.insert(key, Vec::new());
                Ok(0)
            }
            None => Err(NOT_FOUND),
        }
    }
}

impl Service for Cecd {
    fn name(&self) -> &str {
        "cecd:u"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        match command {
            // Open(program, path type, flags, process): the file's size.
            0x01 => {
                let key = (request.word(1), request.word(2));
                match self.open(key, request.word(3)) {
                    Ok(size) => {
                        self.open = Some(key);
                        ok().word(size)
                    }
                    Err(code) => Reply::new(command, code).word(0),
                }
            }
            // Read(size, buffer): the open file.
            0x02 => {
                let data = self.open.and_then(|key| self.files.get(&key)).cloned();
                match data {
                    Some(data) => {
                        let read = data.len().min((request.word(2) >> 4) as usize);
                        let reply = ok().word(read as u32);
                        to_mapped(h, reply, request.word(2), request.word(3), &data)
                    }
                    None => Reply::new(command, NOT_FOUND).word(0),
                }
            }
            // ReadMessage, ReadMessageWithHMAC: no message ever arrived.
            0x03 | 0x04 => Reply::new(command, NOT_FOUND).word(0),
            // Write(size, buffer): replaces the open file.
            0x05 => {
                let data = from_mapped(h, request.word(2), request.word(3));
                match self.open.and_then(|key| self.files.get_mut(&key)) {
                    Some(file) => {
                        *file = data;
                        ok()
                    }
                    None => Reply::new(command, NOT_FOUND),
                }
            }
            // WriteMessage, WriteMessageWithHMAC: sent to nobody.
            0x06 | 0x07 => ok(),
            // Delete(program, path type, outbox, message id).
            0x08 => {
                self.files.remove(&(request.word(1), request.word(2)));
                ok()
            }
            // SetData.
            0x09 => ok(),
            // ReadData and Start are left to the stub until svc.rs's test
            // of an unwritten service stops using them.
            // Stop(command).
            0x0C => ok(),
            // GetCecInfoBuffer(size, buffer): nothing to report.
            0x0D => {
                let zeros = vec![0; (request.word(3) >> 4) as usize];
                to_mapped(h, ok(), request.word(3), request.word(4), &zeros)
            }
            0x0E => ok().word(IDLE),
            // GetCecInfoEventHandle, GetChangeStateEventHandle.
            0x0F => {
                let handle = event_handle(h, &mut self.info_event);
                ok().handles(&[handle], false)
            }
            0x10 => {
                let handle = event_handle(h, &mut self.state_event);
                ok().handles(&[handle], false)
            }
            // OpenAndWrite(size, program, path type, flags, process,
            // buffer).
            0x11 => {
                let key = (request.word(2), request.word(3));
                let data = from_mapped(h, request.word(7), request.word(8));
                self.files.insert(key, data);
                ok()
            }
            // OpenAndRead(size, program, path type, flags, process,
            // buffer): the bytes read.
            0x12 => match self.files.get(&(request.word(2), request.word(3))).cloned() {
                Some(data) => {
                    let read = data.len().min((request.word(7) >> 4) as usize);
                    let reply = ok().word(read as u32);
                    to_mapped(h, reply, request.word(7), request.word(8), &data)
                }
                None => Reply::new(command, NOT_FOUND).word(0),
            },
            // GetEventLog(size, buffer), GetEventLogStart: empty.
            0x1E => ok().word(0),
            0x1F => ok().word(0),
            _ => h.unhandled("cecd:u", request),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::testing::{connect, console, read_bytes, send, write_bytes, HEAP};

    #[test]
    fn a_title_reads_back_the_box_it_wrote() {
        let mut h = console();
        let cecd = connect(&mut h, "cecd:u");
        let missing = send(
            &mut h,
            cecd,
            &[0x0012_0104, 4, 0x1234, 2, 2, 0x20, 0, 4 << 4 | 0xC, HEAP],
        );
        assert_eq!(missing[1], NOT_FOUND.0);
        write_bytes(&mut h, HEAP, b"box!");
        let wrote = send(
            &mut h,
            cecd,
            &[0x0011_0104, 4, 0x1234, 2, 6, 0x20, 0, 4 << 4 | 0xA, HEAP],
        );
        assert_eq!(wrote[1], 0);
        let read = send(
            &mut h,
            cecd,
            &[
                0x0012_0104,
                8,
                0x1234,
                2,
                2,
                0x20,
                0,
                8 << 4 | 0xC,
                HEAP + 8,
            ],
        );
        assert_eq!(read[..3], [0x0012_0082, 0, 4]);
        assert_eq!(read_bytes(&h, HEAP + 8, 4), b"box!");
        assert_eq!(send(&mut h, cecd, &[0x000E_0000])[2], IDLE);
    }
}
