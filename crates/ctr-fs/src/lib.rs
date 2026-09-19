//! 3DS container formats.
//!
//! Parsers only: nothing here decrypts, and nothing here knows about the
//! machine. Layouts follow 3dbrew (`FIRM`, `NCSD`, `NCCH`, `3DSX Format`).

pub mod fat;
pub mod firm;
pub mod threedsx;

pub use firm::{CopyMethod, Firm, FirmError, Section};
pub use threedsx::{ThreeDsx, ThreeDsxError};

/// The kind of 3DS image a byte buffer holds, judged by its magic alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImageKind {
    /// A firmware container: boot ROM payloads, NATIVE_FIRM, bare-metal
    /// homebrew. Magic `FIRM` at 0.
    Firm,
    /// A game card or NAND image. Magic `NCSD` at 0x100.
    Ncsd,
    /// A single content container (CXI/CFA). Magic `NCCH` at 0x100.
    Ncch,
    /// A relocatable homebrew executable. Magic `3DSX` at 0.
    ThreeDsx,
}

impl ImageKind {
    /// Identify an image from its magic. The file extension is never
    /// consulted.
    pub fn detect(data: &[u8]) -> Option<ImageKind> {
        let magic_at = |offset: usize| data.get(offset..offset + 4);
        match magic_at(0) {
            Some(b"FIRM") if data.len() >= firm::HEADER_LEN => return Some(ImageKind::Firm),
            Some(b"3DSX") => return Some(ImageKind::ThreeDsx),
            _ => {}
        }
        match magic_at(0x100) {
            Some(b"NCSD") => Some(ImageKind::Ncsd),
            Some(b"NCCH") => Some(ImageKind::Ncch),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_each_container_by_magic() {
        let mut firm = vec![0u8; firm::HEADER_LEN];
        firm[..4].copy_from_slice(b"FIRM");
        assert_eq!(ImageKind::detect(&firm), Some(ImageKind::Firm));

        let mut ncsd = vec![0u8; 0x200];
        ncsd[0x100..0x104].copy_from_slice(b"NCSD");
        assert_eq!(ImageKind::detect(&ncsd), Some(ImageKind::Ncsd));

        let mut ncch = vec![0u8; 0x200];
        ncch[0x100..0x104].copy_from_slice(b"NCCH");
        assert_eq!(ImageKind::detect(&ncch), Some(ImageKind::Ncch));

        assert_eq!(ImageKind::detect(b"3DSX...."), Some(ImageKind::ThreeDsx));
    }

    #[test]
    fn rejects_short_and_foreign_data() {
        assert_eq!(ImageKind::detect(&[]), None);
        assert_eq!(ImageKind::detect(b"FIRM"), None);
        assert_eq!(ImageKind::detect(&[0u8; 0x8000]), None);
        assert_eq!(ImageKind::detect(&[0xFF; 0x103]), None);
    }
}
