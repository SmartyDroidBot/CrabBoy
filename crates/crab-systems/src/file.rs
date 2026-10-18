//! Images on the host's file system, for the desktop and command-line
//! frontends. The browser has no files to open and builds without this
//! module (feature `std`).

use emu_core::{Storage, StorageError, System};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// A file read where it lies, for images too large to hold in memory: a 3DS
/// card image is read a piece at a time as the game asks for it.
pub struct FileStorage {
    file: std::fs::File,
    len: u64,
}

impl FileStorage {
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        Ok(FileStorage { file, len })
    }
}

impl Storage for FileStorage {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError> {
        if offset
            .checked_add(buf.len() as u64)
            .is_none_or(|end| end > self.len)
        {
            return Err(StorageError::OutOfRange {
                offset,
                len: buf.len(),
            });
        }
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.read_exact(buf))
            .map_err(|e| StorageError::Host(e.to_string()))
    }
}

/// Identify the image in the file at `path` from its header alone.
pub fn detect_file(path: impl AsRef<Path>) -> Result<Option<crate::Kind>, String> {
    let mut media = FileStorage::open(path).map_err(|e| e.to_string())?;
    crate::detect_media(&mut media)
}

/// Build the console for the image in the file at `path`, reading no more of
/// it than the console needs (see [`crate::load_media`]).
pub fn load_file(
    path: impl AsRef<Path>,
    opts: &crate::LoadOptions,
) -> Result<Box<dyn System>, String> {
    let media = FileStorage::open(path).map_err(|e| e.to_string())?;
    crate::load_media(Box::new(media), opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, data: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("crab-systems-{}-{name}", std::process::id()));
        std::fs::write(&path, data).unwrap();
        path
    }

    #[test]
    fn a_file_is_read_in_pieces_and_refuses_reads_past_its_end() {
        let path = temp_file("pieces", b"0123456789");
        let mut file = FileStorage::open(&path).unwrap();
        assert_eq!(file.len(), 10);
        let mut buf = [0u8; 4];
        file.read_at(3, &mut buf).unwrap();
        assert_eq!(&buf, b"3456");
        assert!(file.read_at(8, &mut buf).is_err());
        assert!(file.read_at(u64::MAX, &mut buf).is_err());
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        assert!(load_file("no/such/rom.gb", &Default::default()).is_err());
        assert!(detect_file("no/such/rom.gb").is_err());
    }
}
