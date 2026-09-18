//! The FIRM firmware container.
//!
//! | Offset | Size  | Field                                   |
//! |--------|-------|-----------------------------------------|
//! | 0x000  | 4     | magic `FIRM`                            |
//! | 0x004  | 4     | boot priority                           |
//! | 0x008  | 4     | ARM11 entry point                       |
//! | 0x00C  | 4     | ARM9 entry point                        |
//! | 0x010  | 0x30  | reserved                                |
//! | 0x040  | 0xC0  | four section headers, 0x30 bytes each   |
//! | 0x100  | 0x100 | RSA-2048 signature of the header hash   |
//!
//! A section header is: byte offset, physical load address, byte size (zero
//! means the slot is unused), copy method, SHA-256 of the section.

/// Bytes in a FIRM header.
pub const HEADER_LEN: usize = 0x200;
/// Section slots in a FIRM header.
pub const SECTIONS: usize = 4;

const SECTION_HEADERS: usize = 0x40;
const SECTION_HEADER_LEN: usize = 0x30;
const SIGNATURE: usize = 0x100;

/// How the boot ROM copies a section to its load address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CopyMethod {
    Ndma,
    Xdma,
    Cpu,
    /// A value the boot ROM does not define.
    Unknown(u32),
}

impl From<u32> for CopyMethod {
    fn from(v: u32) -> Self {
        match v {
            0 => CopyMethod::Ndma,
            1 => CopyMethod::Xdma,
            2 => CopyMethod::Cpu,
            other => CopyMethod::Unknown(other),
        }
    }
}

/// One firmware section, borrowing its bytes from the image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Section<'a> {
    /// Physical address the section is copied to.
    pub load_address: u32,
    pub copy_method: CopyMethod,
    /// SHA-256 of `data` as recorded in the header (not verified here).
    pub hash: [u8; 32],
    pub data: &'a [u8],
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FirmError {
    /// Shorter than a header, or no `FIRM` magic.
    NotFirm,
    /// A section's offset and size reach outside the image.
    SectionOutOfBounds { index: usize },
    /// A section would wrap around the 32-bit address space.
    SectionWraps { index: usize },
    /// No section is present.
    Empty,
}

impl std::fmt::Display for FirmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FirmError::NotFirm => write!(f, "not a FIRM image"),
            FirmError::SectionOutOfBounds { index } => {
                write!(f, "FIRM section {index} lies outside the image")
            }
            FirmError::SectionWraps { index } => {
                write!(f, "FIRM section {index} wraps the address space")
            }
            FirmError::Empty => write!(f, "FIRM image has no sections"),
        }
    }
}

impl std::error::Error for FirmError {}

/// A parsed FIRM image.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Firm<'a> {
    pub boot_priority: u32,
    pub arm11_entry: u32,
    pub arm9_entry: u32,
    /// The present sections, in header order.
    pub sections: Vec<Section<'a>>,
    pub signature: &'a [u8],
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

impl<'a> Firm<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, FirmError> {
        if data.len() < HEADER_LEN || &data[..4] != b"FIRM" {
            return Err(FirmError::NotFirm);
        }
        let mut sections = Vec::with_capacity(SECTIONS);
        for index in 0..SECTIONS {
            let h = SECTION_HEADERS + index * SECTION_HEADER_LEN;
            let offset = u32_at(data, h) as usize;
            let load_address = u32_at(data, h + 4);
            let size = u32_at(data, h + 8);
            if size == 0 {
                continue;
            }
            let bytes = offset
                .checked_add(size as usize)
                .and_then(|end| data.get(offset..end))
                .ok_or(FirmError::SectionOutOfBounds { index })?;
            if load_address.checked_add(size - 1).is_none() {
                return Err(FirmError::SectionWraps { index });
            }
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&data[h + 0x10..h + 0x30]);
            sections.push(Section {
                load_address,
                copy_method: u32_at(data, h + 0xC).into(),
                hash,
                data: bytes,
            });
        }
        if sections.is_empty() {
            return Err(FirmError::Empty);
        }
        Ok(Firm {
            boot_priority: u32_at(data, 4),
            arm11_entry: u32_at(data, 8),
            arm9_entry: u32_at(data, 0xC),
            sections,
            signature: &data[SIGNATURE..SIGNATURE + 0x100],
        })
    }
}

/// Build a FIRM image from `(load_address, bytes)` sections. For tests and
/// tools; the hashes and the signature are left zeroed.
pub fn build(arm11_entry: u32, arm9_entry: u32, sections: &[(u32, &[u8])]) -> Vec<u8> {
    assert!(sections.len() <= SECTIONS, "a FIRM holds four sections");
    let mut image = vec![0u8; HEADER_LEN];
    image[..4].copy_from_slice(b"FIRM");
    image[8..12].copy_from_slice(&arm11_entry.to_le_bytes());
    image[12..16].copy_from_slice(&arm9_entry.to_le_bytes());
    for (index, (load_address, bytes)) in sections.iter().enumerate() {
        let h = SECTION_HEADERS + index * SECTION_HEADER_LEN;
        let offset = image.len() as u32;
        image[h..h + 4].copy_from_slice(&offset.to_le_bytes());
        image[h + 4..h + 8].copy_from_slice(&load_address.to_le_bytes());
        image[h + 8..h + 12].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        image[h + 12..h + 16].copy_from_slice(&2u32.to_le_bytes());
        image.extend_from_slice(bytes);
        while !image.len().is_multiple_of(0x200) {
            image.push(0);
        }
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_entries_and_sections() {
        let image = build(
            0x1FF8_0000,
            0x0800_6000,
            &[(0x0800_6000, &[1, 2, 3, 4]), (0x1FF8_0000, &[5, 6])],
        );
        let firm = Firm::parse(&image).unwrap();
        assert_eq!(firm.arm11_entry, 0x1FF8_0000);
        assert_eq!(firm.arm9_entry, 0x0800_6000);
        assert_eq!(firm.sections.len(), 2);
        assert_eq!(firm.sections[0].load_address, 0x0800_6000);
        assert_eq!(firm.sections[0].data, [1, 2, 3, 4]);
        assert_eq!(firm.sections[0].copy_method, CopyMethod::Cpu);
        assert_eq!(firm.sections[1].data, [5, 6]);
        assert_eq!(firm.signature.len(), 0x100);
    }

    #[test]
    fn rejects_malformed_images() {
        assert_eq!(Firm::parse(&[0u8; 0x100]), Err(FirmError::NotFirm));
        assert_eq!(Firm::parse(&[0u8; HEADER_LEN]), Err(FirmError::NotFirm));

        let empty = build(0, 0, &[]);
        assert_eq!(Firm::parse(&empty), Err(FirmError::Empty));

        let mut truncated = build(0, 0x0800_0000, &[(0x0800_0000, &[0; 0x400])]);
        truncated.truncate(HEADER_LEN + 0x100);
        assert_eq!(
            Firm::parse(&truncated),
            Err(FirmError::SectionOutOfBounds { index: 0 })
        );

        let wraps = build(0, 0, &[(0xFFFF_FFFF, &[0; 2])]);
        assert_eq!(
            Firm::parse(&wraps),
            Err(FirmError::SectionWraps { index: 0 })
        );

        let mut huge = build(0, 0, &[(0x0800_0000, &[0; 4])]);
        huge[0x40..0x44].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        assert_eq!(
            Firm::parse(&huge),
            Err(FirmError::SectionOutOfBounds { index: 0 })
        );
    }
}
