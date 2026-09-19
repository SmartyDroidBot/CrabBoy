//! Read-only media too large to hold in memory.
//!
//! A cartridge image of several gigabytes is read in pieces, where and when
//! the emulated software asks for them. The cores stay free of the operating
//! system: a frontend supplies the [`Storage`] (a file, a browser `File`, a
//! byte vector) and the core only ever calls [`Storage::read_at`]. Reads are
//! part of no timing model, so how long the host takes changes nothing that
//! is emulated.

/// Why a read failed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum StorageError {
    /// The range does not lie inside the medium.
    OutOfRange { offset: u64, len: usize },
    /// The host could not deliver the data.
    Host(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::OutOfRange { offset, len } => {
                write!(f, "read of {len} bytes at {offset:#x} is outside the image")
            }
            StorageError::Host(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for StorageError {}

/// A medium that is read at arbitrary offsets.
pub trait Storage {
    /// The size of the medium in bytes.
    fn len(&self) -> u64;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Fill `buf` from `offset`. A range that is not wholly inside the
    /// medium is an error and leaves `buf` unspecified.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError>;
}

impl Storage for Vec<u8> {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError> {
        let range = usize::try_from(offset)
            .ok()
            .and_then(|start| Some(start..start.checked_add(buf.len())?))
            .and_then(|range| self.get(range))
            .ok_or(StorageError::OutOfRange {
                offset,
                len: buf.len(),
            })?;
        buf.copy_from_slice(range);
        Ok(())
    }
}

impl Storage for Box<dyn Storage> {
    fn len(&self) -> u64 {
        (**self).len()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError> {
        (**self).read_at(offset, buf)
    }
}

/// A part of a medium: a partition, a file system, a file. It holds no
/// reference, so many windows can describe one medium that a single owner
/// reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Window {
    pub base: u64,
    pub len: u64,
}

impl Window {
    /// The whole of `media`.
    pub fn whole(media: &dyn Storage) -> Self {
        Window {
            base: 0,
            len: media.len(),
        }
    }

    /// The part of this window from `offset`, `len` bytes long, or `None` if
    /// it does not fit.
    pub fn sub(&self, offset: u64, len: u64) -> Option<Window> {
        (offset.checked_add(len)? <= self.len).then_some(Window {
            base: self.base + offset,
            len,
        })
    }

    /// Fill `buf` from `offset` within the window.
    pub fn read_at(
        &self,
        media: &mut dyn Storage,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<(), StorageError> {
        match self.sub(offset, buf.len() as u64) {
            Some(part) => media.read_at(part.base, buf),
            None => Err(StorageError::OutOfRange {
                offset,
                len: buf.len(),
            }),
        }
    }

    /// `len` bytes from `offset` as a vector.
    pub fn read_vec(
        &self,
        media: &mut dyn Storage,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, StorageError> {
        let mut buf = vec![0; len];
        self.read_at(media, offset, &mut buf)?;
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vector_is_a_medium_and_refuses_reads_past_its_end() {
        let mut media: Vec<u8> = (0..16).collect();
        let mut buf = [0u8; 4];
        media.read_at(12, &mut buf).unwrap();
        assert_eq!(buf, [12, 13, 14, 15]);
        assert_eq!(
            media.read_at(13, &mut buf),
            Err(StorageError::OutOfRange { offset: 13, len: 4 })
        );
        assert!(media.read_at(u64::MAX, &mut buf).is_err());
        assert_eq!(Storage::len(&media), 16);
    }

    #[test]
    fn windows_nest_and_stay_inside_their_parent() {
        let mut media: Box<dyn Storage> = Box::new((0..32).collect::<Vec<u8>>());
        let partition = Window::whole(&*media).sub(8, 16).unwrap();
        let file = partition.sub(4, 8).unwrap();
        assert_eq!(file, Window { base: 12, len: 8 });
        assert_eq!(file.read_vec(&mut media, 6, 2).unwrap(), [18, 19]);
        assert!(file.read_vec(&mut media, 6, 3).is_err());
        assert_eq!(partition.sub(9, 8), None);
        assert_eq!(partition.sub(u64::MAX, 2), None);
    }
}
