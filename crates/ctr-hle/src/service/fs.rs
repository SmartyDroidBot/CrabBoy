//! The filesystem service, `fs:USER` (3dbrew, "Filesystem services").
//!
//! Archives are opened by id and named afterwards by a 64-bit handle of this
//! service's own; files and directories are sessions of their own, which the
//! service hands out and then answers on. The archives:
//!
//! - the program's own content (SelfNCCH, 3): its RomFS (IVFC level 3, read
//!   from the image where it lies) and the icon, banner and logo of its ExeFS;
//! - its save data (4) and extra data (6, and shared extra data, 7), volumes
//!   of `Horizon::saves` that exist once formatted or created;
//! - the SD card (9, and its write-only view, 10), a volume that always
//!   exists.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::memfs::{self, Entry, Format, FsError};
use crate::result::{self, ResultCode};
use crate::service::Service;
use emu_core::Window;
use std::collections::BTreeMap;

/// Result codes (module FS, 17; 3dbrew, "Filesystem services PXI").
pub const NOT_FOUND: ResultCode = ResultCode(0xC880_4478);
pub const NOT_FORMATTED: ResultCode = ResultCode(0xC8A0_4554);
/// Extra data that was never created, on the SD card.
const EXTDATA_NOT_FOUND: ResultCode = ResultCode(0xC8A0_4478);
const ALREADY_EXISTS: ResultCode = ResultCode(0xC820_44BE);
const NO_PARENT: ResultCode = ResultCode(0xC880_4471);
/// Not on 3dbrew: the FS module's "directory not empty", description 240.
const NOT_EMPTY: ResultCode = ResultCode(0xC860_44F0);
const IS_DIRECTORY: ResultCode = ResultCode(0xC920_44FA);
const INVALID_PATH: ResultCode = ResultCode(0xE0E0_46BE);
const PAST_END: ResultCode = ResultCode(0xE0E0_46C1);
const UNSUPPORTED: ResultCode = ResultCode(0xE0C0_46BE);
const INVALID_ARCHIVE: ResultCode = ResultCode(0xC880_4465);
const INVALID_HANDLE: ResultCode = ResultCode(0xE0E0_47F7);
/// A read of a medium that failed on the host.
const MEDIA_ERROR: ResultCode = ResultCode(0xC8A0_46D0);

/// Archive ids.
const SELF_NCCH: u32 = 3;
const SAVE_DATA: u32 = 4;
const EXT_SAVE_DATA: u32 = 6;
const SHARED_EXT_SAVE_DATA: u32 = 7;
const SDMC: u32 = 9;
const SDMC_WRITE_ONLY: u32 = 10;
/// The media type of the SD card in an extra data path.
const MEDIA_SD: u32 = 1;
/// The SD card: 512-byte sectors, 32 KB clusters, 2 GB.
const SD_SECTOR: u32 = 0x200;
const SD_CLUSTER: u32 = 0x8000;
const SD_CLUSTERS: u32 = 0x1_0000;
/// The size of a directory entry in a listing.
const ENTRY_LEN: usize = 0x228;
/// Save data of a program whose exheader gives no size.
const DEFAULT_SAVE_SIZE: u64 = 0x8_0000;

/// A path as the client gives it: a kind, and bytes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Path {
    Empty,
    Binary(Vec<u8>),
    Text(String),
    Invalid,
}

impl Path {
    /// The normalised path in a volume.
    fn in_volume(&self) -> Result<String, ResultCode> {
        match self {
            Path::Empty => Ok("/".to_string()),
            Path::Text(text) => memfs::normalize(text).ok_or(INVALID_PATH),
            _ => Err(INVALID_PATH),
        }
    }

    /// The words of a binary path.
    fn words(&self) -> Vec<u32> {
        match self {
            Path::Binary(bytes) => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|w| u32::from_le_bytes(*w))
                .collect(),
            _ => Vec::new(),
        }
    }
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

fn code(error: FsError) -> ResultCode {
    match error {
        FsError::NoVolume | FsError::NotFound => NOT_FOUND,
        FsError::NoParent => NO_PARENT,
        FsError::Exists => ALREADY_EXISTS,
        FsError::NotEmpty => NOT_EMPTY,
        FsError::BadPath => INVALID_PATH,
    }
}

/// The volume of extra data `id` on `media`.
fn extdata_key(shared: bool, media: u32, id: u64) -> String {
    let kind = if shared { "sharedextdata" } else { "extdata" };
    format!("{kind}/{media}/{id:016x}")
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Archive {
    SelfNcch,
    /// A volume of `Horizon::saves`, by key.
    Volume(String),
}

/// What a file or directory session leads to.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Session {
    /// Read-only bytes of the image.
    Media(Window),
    /// A file in a volume.
    Saved { volume: String, path: String },
    /// A directory listing, and how far it has been read.
    Directory { entries: Vec<Entry>, next: usize },
}

#[derive(Default)]
pub struct Fs {
    archives: BTreeMap<u64, Archive>,
    next_archive: u64,
    sessions: BTreeMap<u32, Session>,
    next_session: u32,
    priority: u32,
}

impl Fs {
    fn open_archive(
        &mut self,
        h: &mut Horizon,
        id: u32,
        path: &Path,
    ) -> Result<Archive, ResultCode> {
        match id {
            SELF_NCCH => Ok(Archive::SelfNcch),
            SAVE_DATA => match h.saves.volume("save") {
                Some(_) => Ok(Archive::Volume("save".into())),
                None => Err(NOT_FORMATTED),
            },
            EXT_SAVE_DATA | SHARED_EXT_SAVE_DATA => {
                let words = path.words();
                let [media, low, high] = words[..] else {
                    return Err(INVALID_PATH);
                };
                let key = extdata_key(
                    id == SHARED_EXT_SAVE_DATA,
                    media,
                    (high as u64) << 32 | low as u64,
                );
                match h.saves.volume(&key) {
                    Some(_) => Ok(Archive::Volume(key)),
                    None if media == MEDIA_SD => Err(EXTDATA_NOT_FOUND),
                    None => Err(NOT_FOUND),
                }
            }
            SDMC | SDMC_WRITE_ONLY => {
                let _ = h.saves.create_volume("sdmc", Format::default());
                Ok(Archive::Volume("sdmc".into()))
            }
            _ => {
                h.note(format!(
                    "fs:USER archive {id:#x} {path:?} is not implemented"
                ));
                Err(NOT_FOUND)
            }
        }
    }

    /// A file of `archive` at `path`, created first if `flags` say so.
    fn open_file(
        &mut self,
        h: &mut Horizon,
        archive: &Archive,
        path: &Path,
        flags: u32,
    ) -> Result<Session, ResultCode> {
        match (archive, path) {
            (Archive::SelfNcch, Path::Binary(bytes)) if bytes.len() >= 4 => {
                let kind = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                match kind {
                    // The RomFS, of the base title or of an update; there is
                    // no update, so both are the base title's.
                    0 | 5 => h.romfs.map(Session::Media).ok_or(NOT_FOUND),
                    // An ExeFS file by name: icon, banner, logo.
                    2 => {
                        let name: String = bytes[4..]
                            .iter()
                            .take(8)
                            .take_while(|&&b| b != 0)
                            .map(|&b| b as char)
                            .collect();
                        h.exefs_file(&name).map(Session::Media).ok_or(NOT_FOUND)
                    }
                    _ => Err(UNSUPPORTED),
                }
            }
            (Archive::SelfNcch, _) => Err(NOT_FOUND),
            (Archive::Volume(volume), path) => {
                let path = path.in_volume()?;
                if h.saves.is_directory(volume, &path) {
                    return Err(IS_DIRECTORY);
                }
                if !h.saves.is_file(volume, &path) {
                    if flags & 4 == 0 {
                        return Err(NOT_FOUND);
                    }
                    h.saves.create_file(volume, &path, 0).map_err(code)?;
                }
                Ok(Session::Saved {
                    volume: volume.clone(),
                    path,
                })
            }
        }
    }

    fn session(&mut self, h: &mut Horizon, session: Session) -> u32 {
        self.next_session += 1;
        self.sessions.insert(self.next_session, session);
        h.sub_session("fs:USER", self.next_session)
    }

    /// The volume and path a request names: archive handle at word `at`,
    /// path type and size at the next two, path address at `addr`.
    fn volume_path(
        &self,
        h: &Horizon,
        request: &Request,
        at: usize,
        addr: usize,
    ) -> Result<(String, String), ResultCode> {
        let Some(Archive::Volume(volume)) = self.archives.get(&request.u64(at)) else {
            return Err(INVALID_ARCHIVE);
        };
        let p = path(
            h,
            request.word(at + 2),
            request.word(at + 3),
            request.word(addr),
        );
        Ok((volume.clone(), p.in_volume()?))
    }

    /// A command on a file or directory session.
    fn session_call(&mut self, h: &mut Horizon, id: u32, request: &Request) -> Reply {
        let command = request.command();
        let Some(session) = self.sessions.get(&id).cloned() else {
            return Reply::new(command, INVALID_HANDLE);
        };
        match session {
            Session::Directory { entries, next } => {
                self.directory_call(h, id, entries, next, request)
            }
            file => self.file_call(h, id, file, request),
        }
    }

    fn file_call(&mut self, h: &mut Horizon, id: u32, file: Session, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        match command {
            // Read(offset, size) into a mapped buffer.
            0x0802 => {
                let (offset, size, addr) = (request.u64(1), request.word(3), request.word(5));
                let data = match &file {
                    Session::Media(window) => {
                        let len = (size as u64).min(window.len.saturating_sub(offset)) as usize;
                        let mut data = vec![0; len];
                        if window.read_at(&mut *h.media, offset, &mut data).is_err() {
                            return Reply::new(command, MEDIA_ERROR).word(0);
                        }
                        data
                    }
                    Session::Saved { volume, path } => {
                        match h.saves.size(volume, path) {
                            Ok(len) if offset > len => {
                                return Reply::new(command, PAST_END).word(0)
                            }
                            Ok(_) => {}
                            Err(e) => return Reply::new(command, code(e)).word(0),
                        }
                        h.saves
                            .read(volume, path, offset, size as usize)
                            .unwrap_or_default()
                    }
                    Session::Directory { .. } => unreachable!("directories answer elsewhere"),
                };
                h.process.space.write_bytes(&mut h.mem, addr, &data);
                ok().word(data.len() as u32)
                    .mapped_buffer(request.word(4), addr)
            }
            // Write(offset, size, options) from a mapped buffer.
            0x0803 => {
                let Session::Saved { volume, path } = &file else {
                    return Reply::new(command, UNSUPPORTED).word(0);
                };
                let (offset, size, addr) = (request.u64(1), request.word(3), request.word(6));
                let data = h
                    .process
                    .space
                    .read_bytes(&h.mem, addr, size as usize)
                    .unwrap_or_default();
                match h.saves.write(volume, path, offset, &data) {
                    Ok(()) => ok().word(size).mapped_buffer(request.word(5), addr),
                    Err(e) => Reply::new(command, code(e)).word(0),
                }
            }
            // GetSize.
            0x0804 => match &file {
                Session::Media(window) => ok().u64(window.len),
                Session::Saved { volume, path } => match h.saves.size(volume, path) {
                    Ok(len) => ok().u64(len),
                    Err(e) => Reply::new(command, code(e)).u64(0),
                },
                Session::Directory { .. } => unreachable!("directories answer elsewhere"),
            },
            // SetSize(size).
            0x0805 => match &file {
                Session::Saved { volume, path } => {
                    match h.saves.set_size(volume, path, request.u64(1)) {
                        Ok(()) => ok(),
                        Err(e) => Reply::new(command, code(e)),
                    }
                }
                _ => Reply::new(command, UNSUPPORTED),
            },
            // GetAttributes: none.
            0x0806 => ok().word(0),
            // SetAttributes.
            0x0807 => ok(),
            // Close.
            0x0808 => {
                self.sessions.remove(&id);
                ok()
            }
            // Flush, SetPriority.
            0x0809 | 0x080A => ok(),
            0x080B => ok().word(self.priority),
            // OpenLinkFile: another session to the same file.
            0x080C => {
                let handle = self.session(h, file);
                ok().handles(&[handle], true)
            }
            _ => h.unhandled("fs:USER file", request),
        }
    }

    fn directory_call(
        &mut self,
        h: &mut Horizon,
        id: u32,
        entries: Vec<Entry>,
        next: usize,
        request: &Request,
    ) -> Reply {
        let command = request.command();
        match command {
            // Read(count) into a mapped buffer of 0x228-byte entries.
            0x0801 => {
                let (count, addr) = (request.word(1) as usize, request.word(3));
                let batch: Vec<&Entry> = entries.iter().skip(next).take(count).collect();
                let bytes: Vec<u8> = batch.iter().flat_map(|e| encode_entry(e)).collect();
                h.process.space.write_bytes(&mut h.mem, addr, &bytes);
                let read = batch.len();
                self.sessions.insert(
                    id,
                    Session::Directory {
                        entries,
                        next: next + read,
                    },
                );
                Reply::new(command, result::SUCCESS)
                    .word(read as u32)
                    .mapped_buffer(request.word(2), addr)
            }
            // Close.
            0x0802 => {
                self.sessions.remove(&id);
                Reply::new(command, result::SUCCESS)
            }
            _ => h.unhandled("fs:USER directory", request),
        }
    }

    /// The archive a handle names, for GetFreeBytes.
    fn free_bytes(&self, h: &Horizon, archive: &Archive) -> u64 {
        match archive {
            Archive::SelfNcch => 0,
            Archive::Volume(key) => {
                let volume = h.saves.volume(key);
                let used = volume.map_or(0, |v| v.used_bytes());
                match volume.map(|v| v.format.size) {
                    Some(size) if key == "save" => size.saturating_sub(used),
                    _ => (SD_CLUSTERS as u64 * SD_CLUSTER as u64).saturating_sub(used),
                }
            }
        }
    }

    fn sd_resource(&self, h: &Horizon) -> [u32; 4] {
        let used = h.saves.volume("sdmc").map_or(0, |v| v.used_bytes());
        let used_clusters = used.div_ceil(SD_CLUSTER as u64) as u32;
        [
            SD_SECTOR,
            SD_CLUSTER,
            SD_CLUSTERS,
            SD_CLUSTERS.saturating_sub(used_clusters),
        ]
    }

    fn format_save(h: &mut Horizon, directories: u32, files: u32, duplicate: bool) {
        let size = if h.save_size == 0 {
            DEFAULT_SAVE_SIZE
        } else {
            h.save_size
        };
        h.saves.format(
            "save",
            Format {
                size,
                directories,
                files,
                duplicate,
            },
        );
    }
}

/// A directory entry as a listing holds it: the name in UTF-16, an 8.3
/// short name, attributes and size.
fn encode_entry(entry: &Entry) -> Vec<u8> {
    let mut out = vec![0u8; ENTRY_LEN];
    for (n, unit) in entry.name.encode_utf16().take(0x105).enumerate() {
        out[n * 2..n * 2 + 2].copy_from_slice(&unit.to_le_bytes());
    }
    let (stem, extension) = match entry.name.rsplit_once('.') {
        Some((stem, ext)) if !entry.directory => (stem, ext),
        _ => (entry.name.as_str(), ""),
    };
    let short = |text: &str, len: usize| -> Vec<u8> {
        text.bytes()
            .filter(|b| b.is_ascii_graphic() && *b != b'.')
            .map(|b| b.to_ascii_uppercase())
            .take(len)
            .collect()
    };
    let name = short(stem, 8);
    out[0x20C..0x20C + name.len()].copy_from_slice(&name);
    let ext = short(extension, 3);
    out[0x216..0x216 + ext.len()].copy_from_slice(&ext);
    out[0x21A] = 1;
    out[0x21C] = entry.directory as u8;
    out[0x220..0x228].copy_from_slice(&entry.size.to_le_bytes());
    out
}

impl Service for Fs {
    fn name(&self) -> &str {
        "fs:USER"
    }

    fn call(&mut self, h: &mut Horizon, object: u32, request: &Request) -> Reply {
        if object != 0 {
            return self.session_call(h, object, request);
        }
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        let status = |result: Result<(), ResultCode>| {
            Reply::new(command, result.err().unwrap_or(result::SUCCESS))
        };
        match command {
            // Initialize, InitializeWithSdkVersion.
            0x0801 | 0x0861 => ok(),
            // OpenFile(transaction, archive, path type, path size, flags,
            // attributes, path).
            0x0802 => {
                let Some(archive) = self.archives.get(&request.u64(2)).cloned() else {
                    return Reply::new(command, INVALID_ARCHIVE).words(&[0, 0]);
                };
                let path = path(h, request.word(4), request.word(5), request.word(9));
                match self.open_file(h, &archive, &path, request.word(6)) {
                    Ok(file) => {
                        let handle = self.session(h, file);
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
                let archive_path = path(h, request.word(3), request.word(4), request.word(10));
                let archive = match self.open_archive(h, id, &archive_path) {
                    Ok(archive) => archive,
                    Err(code) => return Reply::new(command, code).words(&[0, 0]),
                };
                let path = path(h, request.word(5), request.word(6), request.word(12));
                match self.open_file(h, &archive, &path, request.word(7)) {
                    Ok(file) => {
                        let handle = self.session(h, file);
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
            // DeleteFile(transaction, archive, path type and size, path).
            0x0804 => status(
                self.volume_path(h, request, 2, 7)
                    .and_then(|(v, p)| h.saves.delete_file(&v, &p).map_err(code)),
            ),
            // RenameFile, RenameDirectory(transaction, source archive and
            // path, destination archive and path): within one archive.
            0x0805 | 0x080A => {
                let from = self.volume_path(h, request, 2, 11);
                let to = self.volume_path(h, request, 6, 13);
                status(from.and_then(|(v, from)| {
                    let (w, to) = to?;
                    if v != w {
                        return Err(ResultCode(0xE0C0_46F8));
                    }
                    h.saves
                        .rename(&v, &from, &to, command == 0x080A)
                        .map_err(code)
                }))
            }
            // DeleteDirectory, DeleteDirectoryRecursively.
            0x0806 | 0x0807 => status(self.volume_path(h, request, 2, 7).and_then(|(v, p)| {
                h.saves
                    .delete_directory(&v, &p, command == 0x0807)
                    .map_err(code)
            })),
            // CreateFile(transaction, archive, path type and size,
            // attributes, size, path).
            0x0808 => {
                let size = request.u64(7);
                status(
                    self.volume_path(h, request, 2, 10)
                        .and_then(|(v, p)| h.saves.create_file(&v, &p, size).map_err(code)),
                )
            }
            // CreateDirectory(transaction, archive, path type and size,
            // attributes, path).
            0x0809 => status(
                self.volume_path(h, request, 2, 8)
                    .and_then(|(v, p)| h.saves.create_directory(&v, &p).map_err(code)),
            ),
            // OpenDirectory(archive, path type and size, path).
            0x080B => {
                let listing = self
                    .volume_path(h, request, 1, 6)
                    .and_then(|(v, p)| h.saves.list(&v, &p).map_err(code));
                match listing {
                    Ok(entries) => {
                        let handle = self.session(h, Session::Directory { entries, next: 0 });
                        ok().handles(&[handle], true)
                    }
                    Err(code) => Reply::new(command, code).words(&[0, 0]),
                }
            }
            // OpenArchive(archive id, path type, path size, path).
            0x080C => {
                let id = request.word(1);
                let p = path(h, request.word(2), request.word(3), request.word(5));
                match self.open_archive(h, id, &p) {
                    Ok(archive) => {
                        self.next_archive += 1;
                        self.archives.insert(self.next_archive, archive);
                        ok().u64(self.next_archive)
                    }
                    Err(code) => Reply::new(command, code).words(&[0, 0]),
                }
            }
            // ControlArchive(archive, action, sizes, input, output): commit
            // (0) has nothing to do; a time stamp (1) is 0.
            0x080D => {
                if request.word(3) == 1 {
                    let output = request.word(9);
                    h.process
                        .space
                        .write_bytes(&mut h.mem, output, &0u64.to_le_bytes());
                }
                ok()
            }
            // CloseArchive(archive).
            0x080E => {
                self.archives.remove(&request.u64(1));
                ok()
            }
            // FormatThisUserSaveData(block size, directories, files, bucket
            // counts, duplicate).
            0x080F => {
                Self::format_save(
                    h,
                    request.word(2),
                    request.word(3),
                    request.word(6) & 0xFF != 0,
                );
                ok()
            }
            // GetFreeBytes(archive).
            0x0812 => match self.archives.get(&request.u64(1)) {
                Some(archive) => ok().u64(self.free_bytes(h, archive)),
                None => Reply::new(command, INVALID_ARCHIVE).u64(0),
            },
            // GetSdmcArchiveResource; GetArchiveResource(media type).
            0x0814 | 0x0849 => ok().words(&self.sd_resource(h)),
            // IsSdmcDetected, IsSdmcWritable: there is a card, writable.
            0x0817 | 0x0818 => ok().word(1),
            // CardSlotIsInserted: no game card besides this one.
            0x0821 => ok().word(0),
            // GetFormatInfo(archive id, path): total size, directories,
            // files, duplicate.
            0x0845 => {
                let id = request.word(1);
                let p = path(h, request.word(2), request.word(3), request.word(5));
                let key = match id {
                    SAVE_DATA => Ok("save".to_string()),
                    EXT_SAVE_DATA | SHARED_EXT_SAVE_DATA => match p.words()[..] {
                        [media, low, high] => Ok(extdata_key(
                            id == SHARED_EXT_SAVE_DATA,
                            media,
                            (high as u64) << 32 | low as u64,
                        )),
                        _ => Err(INVALID_PATH),
                    },
                    _ => Err(UNSUPPORTED),
                };
                let format = key.and_then(|key| {
                    h.saves.volume(&key).map(|v| v.format).ok_or(match id {
                        SAVE_DATA => NOT_FORMATTED,
                        _ => EXTDATA_NOT_FOUND,
                    })
                });
                match format {
                    Ok(f) => {
                        ok().words(&[f.size as u32, f.directories, f.files, f.duplicate as u32])
                    }
                    Err(code) => Reply::new(command, code).words(&[0; 4]),
                }
            }
            // FormatSaveData(archive id, path type and size, block size,
            // directories, files, bucket counts, duplicate, path).
            0x084C => match request.word(1) {
                SAVE_DATA | 0x5678_90B2 => {
                    Self::format_save(
                        h,
                        request.word(5),
                        request.word(6),
                        request.word(9) & 0xFF != 0,
                    );
                    ok()
                }
                _ => Reply::new(command, INVALID_PATH),
            },
            // CreateExtSaveData(info, directories, files, size limit, icon).
            0x0851 => {
                let key = extdata_key(false, request.word(1) & 0xFF, request.u64(2));
                let format = Format {
                    size: request.u64(7),
                    directories: request.word(5),
                    files: request.word(6),
                    duplicate: false,
                };
                status(h.saves.create_volume(&key, format).map_err(code))
            }
            // DeleteExtSaveData(info).
            0x0852 => {
                let key = extdata_key(false, request.word(1) & 0xFF, request.u64(2));
                status(h.saves.delete_volume(&key).map_err(|_| EXTDATA_NOT_FOUND))
            }
            // SetPriority, GetPriority.
            0x0862 => {
                self.priority = request.word(1);
                ok()
            }
            0x0863 => ok().word(self.priority),
            // SetSaveDataSecureValue(value, slot, title, variation).
            0x0865 => {
                h.saves.set_secure_value(request.word(3), request.u64(1));
                ok()
            }
            // GetSaveDataSecureValue(slot, title, variation): exists, value.
            0x0866 => {
                let value = h.saves.secure_values.get(&request.word(1)).copied();
                ok().word(value.is_some() as u32).u64(value.unwrap_or(0))
            }
            // ControlSecureSave(action, sizes, input, output): delete a
            // secure value (0), saying whether it existed; format (1).
            0x0867 => {
                if request.word(1) == 0 {
                    let input = h
                        .process
                        .space
                        .read_bytes(&h.mem, request.word(5), 8)
                        .unwrap_or_else(|| vec![0; 8]);
                    let slot = u64::from_le_bytes(input.try_into().unwrap()) >> 32;
                    let existed = h.saves.secure_values.remove(&(slot as u32)).is_some();
                    h.process
                        .space
                        .write_bytes(&mut h.mem, request.word(7), &[existed as u8]);
                }
                ok()
            }
            // SetThisSaveDataSecureValue(slot, value).
            0x086E => {
                h.saves.set_secure_value(request.word(1), request.u64(2));
                ok()
            }
            // GetThisSaveDataSecureValue(slot), and of a save archive
            // (archive, slot): found, on a game card, value.
            0x086F | 0x0876 => {
                let slot = request.word(if command == 0x086F { 1 } else { 3 });
                let value = h.saves.secure_values.get(&slot).copied();
                let card = h.exefs.is_some();
                ok().word(value.is_some() as u32)
                    .word(card as u32)
                    .u64(value.unwrap_or(0))
            }
            // SetSaveArchiveSecureValue(archive, slot, value).
            0x0875 => {
                h.saves.set_secure_value(request.word(3), request.u64(4));
                ok()
            }
            _ => h.unhandled("fs:USER", request),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_entry_has_its_long_and_short_names() {
        let entry = encode_entry(&Entry {
            name: "main.sav".into(),
            directory: false,
            size: 0x1234,
        });
        assert_eq!(entry.len(), ENTRY_LEN);
        assert_eq!(&entry[..4], [b'm', 0, b'a', 0]);
        assert_eq!(&entry[0x20C..0x210], b"MAIN");
        assert_eq!(&entry[0x216..0x219], b"SAV");
        assert_eq!((entry[0x21A], entry[0x21C]), (1, 0));
        assert_eq!(
            u64::from_le_bytes(entry[0x220..].try_into().unwrap()),
            0x1234
        );
        assert_eq!(extdata_key(false, 1, 0x1B50), "extdata/1/0000000000001b50");
    }
}
