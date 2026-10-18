//! The filesystem service, `fs:USER` (3dbrew, "Filesystem services").
//!
//! Archives are opened by id and named afterwards by a 64-bit handle of this
//! service's own; files are sessions of their own, which the service hands
//! out and then answers on. So far there is one archive kind: the program's
//! own content (SelfNCCH, id 3), which holds its RomFS (IVFC level 3, read
//! from the image where it lies) and the icon, banner and logo of its ExeFS.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::result::{self, ResultCode};
use crate::service::Service;
use emu_core::Window;
use std::collections::BTreeMap;

/// Result codes (module FS, 17).
pub const NOT_FOUND: ResultCode = ResultCode(0xC880_4478);
pub const NOT_FORMATTED: ResultCode = ResultCode(0xC8A0_4554);
const UNSUPPORTED: ResultCode = ResultCode(0xE0C0_46BE);
const INVALID_ARCHIVE: ResultCode = ResultCode(0xC880_4465);
const INVALID_HANDLE: ResultCode = ResultCode(0xE0E0_47F7);
/// A read of a medium that failed on the host.
const MEDIA_ERROR: ResultCode = ResultCode(0xC8A0_46D0);

/// Archive ids.
const SELF_NCCH: u32 = 3;

/// A path as the client gives it: a kind, and bytes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Path {
    Empty,
    Binary(Vec<u8>),
    Text(String),
    Invalid,
}

/// Read a path of `kind` and `len` bytes at `addr` in the client.
fn path(h: &Horizon, kind: u32, len: u32, addr: u32) -> Path {
    let bytes = || {
        h.process
            .space
            .read_bytes(&h.mem, addr, len.min(0x1000) as usize)
    };
    match kind {
        1 => Path::Empty,
        2 => bytes().map_or(Path::Invalid, Path::Binary),
        // ASCII, then UTF-16; both carry their terminator.
        3 => bytes().map_or(Path::Invalid, |b| {
            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            Path::Text(String::from_utf8_lossy(&b[..end]).into_owned())
        }),
        4 => bytes().map_or(Path::Invalid, |b| {
            let units: Vec<u16> = b
                .as_chunks::<2>()
                .0
                .iter()
                .map(|u| u16::from_le_bytes(*u))
                .take_while(|&u| u != 0)
                .collect();
            Path::Text(String::from_utf16_lossy(&units))
        }),
        _ => Path::Invalid,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Archive {
    SelfNcch,
}

/// What a file session leads to.
#[derive(Clone, PartialEq, Eq, Debug)]
enum File {
    /// Read-only bytes of the image.
    Media(Window),
}

#[derive(Default)]
pub struct Fs {
    archives: BTreeMap<u64, Archive>,
    next_archive: u64,
    files: BTreeMap<u32, File>,
    next_file: u32,
    priority: u32,
}

impl Fs {
    fn open_archive(&mut self, id: u32) -> Option<Archive> {
        match id {
            SELF_NCCH => Some(Archive::SelfNcch),
            _ => None,
        }
    }

    /// A file of `archive` at `path`.
    fn open_file(
        &mut self,
        h: &mut Horizon,
        archive: Archive,
        path: &Path,
    ) -> Result<File, ResultCode> {
        match (archive, path) {
            (Archive::SelfNcch, Path::Binary(bytes)) if bytes.len() >= 4 => {
                let kind = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                match kind {
                    // The RomFS, of the base title or of an update; there is
                    // no update, so both are the base title's.
                    0 | 5 => h.romfs.map(File::Media).ok_or(NOT_FOUND),
                    // An ExeFS file by name: icon, banner, logo.
                    2 => {
                        let name: String = bytes[4..]
                            .iter()
                            .take(8)
                            .take_while(|&&b| b != 0)
                            .map(|&b| b as char)
                            .collect();
                        h.exefs_file(&name).map(File::Media).ok_or(NOT_FOUND)
                    }
                    _ => Err(UNSUPPORTED),
                }
            }
            _ => Err(NOT_FOUND),
        }
    }

    fn file_session(&mut self, h: &mut Horizon, file: File) -> u32 {
        self.next_file += 1;
        self.files.insert(self.next_file, file);
        h.sub_session("fs:USER", self.next_file)
    }

    /// A command on a file session.
    fn file_call(&mut self, h: &mut Horizon, id: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        let Some(file) = self.files.get(&id).cloned() else {
            return Reply::new(command, INVALID_HANDLE);
        };
        let File::Media(window) = file;
        match command {
            // Read(offset, size) into a mapped buffer.
            0x0802 => {
                let (offset, size, addr) = (request.u64(1), request.word(3), request.word(5));
                let len = (size as u64).min(window.len.saturating_sub(offset)) as usize;
                let mut data = vec![0; len];
                if window.read_at(&mut *h.media, offset, &mut data).is_err() {
                    return Reply::new(command, MEDIA_ERROR).word(0);
                }
                h.process.space.write_bytes(&mut h.mem, addr, &data);
                ok().word(len as u32).mapped_buffer(request.word(4), addr)
            }
            // Write, SetSize: the image is read only.
            0x0803 | 0x0805 => Reply::new(command, UNSUPPORTED).word(0),
            // GetSize.
            0x0804 => ok().u64(window.len),
            // GetAttributes: none.
            0x0806 => ok().word(0),
            // Close.
            0x0808 => {
                self.files.remove(&id);
                ok()
            }
            // Flush, SetPriority.
            0x0809 | 0x080A => ok(),
            0x080B => ok().word(self.priority),
            // OpenLinkFile: another session to the same file.
            0x080C => {
                let handle = self.file_session(h, File::Media(window));
                ok().handles(&[handle], true)
            }
            _ => h.unhandled("fs:USER file", request),
        }
    }
}

impl Service for Fs {
    fn name(&self) -> &str {
        "fs:USER"
    }

    fn call(&mut self, h: &mut Horizon, object: u32, request: &Request) -> Reply {
        if object != 0 {
            return self.file_call(h, object, request);
        }
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        match command {
            // Initialize, InitializeWithSdkVersion.
            0x0801 | 0x0861 => ok(),
            // OpenFile(transaction, archive, path type, path size, flags,
            // attributes, path).
            0x0802 => {
                let Some(&archive) = self.archives.get(&request.u64(2)) else {
                    return Reply::new(command, INVALID_ARCHIVE).words(&[0, 0]);
                };
                let path = path(h, request.word(4), request.word(5), request.word(9));
                match self.open_file(h, archive, &path) {
                    Ok(file) => {
                        let handle = self.file_session(h, file);
                        ok().handles(&[handle], true)
                    }
                    Err(code) => {
                        h.note(format!("fs:USER OpenFile {archive:?} {path:?}: {code:#x?}"));
                        Reply::new(command, code).words(&[0, 0])
                    }
                }
            }
            // OpenFileDirectly(transaction, archive id, archive path type
            // and size, file path type and size, flags, attributes, archive
            // path, file path).
            0x0803 => {
                let id = request.word(2);
                let Some(archive) = self.open_archive(id) else {
                    h.note(format!(
                        "fs:USER OpenFileDirectly: archive {id:#x} is not implemented"
                    ));
                    return Reply::new(command, NOT_FOUND).words(&[0, 0]);
                };
                let path = path(h, request.word(5), request.word(6), request.word(12));
                match self.open_file(h, archive, &path) {
                    Ok(file) => {
                        let handle = self.file_session(h, file);
                        ok().handles(&[handle], true)
                    }
                    Err(code) => {
                        h.note(format!(
                            "fs:USER OpenFileDirectly {archive:?} {path:?}: {code:#x?}"
                        ));
                        Reply::new(command, code).words(&[0, 0])
                    }
                }
            }
            // OpenArchive(archive id, path type, path size, path).
            0x080C => {
                let id = request.word(1);
                match self.open_archive(id) {
                    Some(archive) => {
                        self.next_archive += 1;
                        self.archives.insert(self.next_archive, archive);
                        ok().u64(self.next_archive)
                    }
                    None => {
                        let p = path(h, request.word(2), request.word(3), request.word(5));
                        h.note(format!(
                            "fs:USER OpenArchive {id:#x} {p:?} is not implemented"
                        ));
                        Reply::new(command, NOT_FOUND).words(&[0, 0])
                    }
                }
            }
            // CloseArchive(archive).
            0x080E => {
                self.archives.remove(&request.u64(1));
                ok()
            }
            // IsSdmcDetected, IsSdmcWritable: there is a card, writable.
            0x0817 | 0x0818 => ok().word(1),
            // CardSlotIsInserted: no game card besides this one.
            0x0821 => ok().word(0),
            // SetPriority, GetPriority.
            0x0862 => {
                self.priority = request.word(1);
                ok()
            }
            0x0863 => ok().word(self.priority),
            _ => h.unhandled("fs:USER", request),
        }
    }
}
