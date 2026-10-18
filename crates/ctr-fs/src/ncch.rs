//! Game card images and their contents (3dbrew, "NCSD", "NCCH", "NCCH/Extended
//! Header", "ExeFS", "RomFS").
//!
//! An NCSD image is a table of up to eight partitions; partition 0 is the
//! executable content. An NCCH content has a header, an extended header that
//! says how to lay the program out in memory, an ExeFS holding the program
//! (`.code`) and its icon, and a RomFS holding its data. Offsets in both
//! headers are in media units of 0x200 bytes, scaled by an exponent in the
//! flags.
//!
//! These are parsers over the bytes of each header. A content whose NCCH
//! flags do not say it is unencrypted is refused: nothing here decrypts.

/// The bytes of an NCSD or NCCH header.
pub const HEADER_LEN: usize = 0x200;
/// The extended header: system control information, then access control.
pub const EXHEADER_LEN: usize = 0x400;
const MEDIA_UNIT: u64 = 0x200;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NcchError {
    /// No `NCSD` or `NCCH` magic, or a header too short.
    NotNcch,
    /// The content is encrypted; it has to be decrypted first.
    Encrypted,
    /// A part of the content lies outside it.
    OutOfBounds,
    /// No executable content in partition 0, or it has no extended header.
    NotExecutable,
    /// Compressed code that does not decompress.
    BadCode,
}

impl std::fmt::Display for NcchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NcchError::NotNcch => write!(f, "not an NCSD or NCCH image"),
            NcchError::Encrypted => write!(
                f,
                "the image is encrypted; only decrypted images can be opened"
            ),
            NcchError::OutOfBounds => write!(f, "the image is damaged or cut short"),
            NcchError::NotExecutable => write!(f, "the image holds no program"),
            NcchError::BadCode => write!(f, "the program's code does not decompress"),
        }
    }
}

impl std::error::Error for NcchError {}

fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    u32_at(data, at) as u64 | (u32_at(data, at + 4) as u64) << 32
}

/// A byte range within the image.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Range {
    pub offset: u64,
    pub len: u64,
}

/// The partition table of a card image.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Ncsd {
    pub media_id: u64,
    pub partitions: [Range; 8],
}

impl Ncsd {
    pub fn parse(header: &[u8]) -> Result<Self, NcchError> {
        if header.len() < HEADER_LEN || &header[0x100..0x104] != b"NCSD" {
            return Err(NcchError::NotNcch);
        }
        let unit = MEDIA_UNIT << header[0x188 + 6].min(16);
        let partitions = std::array::from_fn(|n| Range {
            offset: u32_at(header, 0x120 + n * 8) as u64 * unit,
            len: u32_at(header, 0x124 + n * 8) as u64 * unit,
        });
        Ok(Ncsd {
            media_id: u64_at(header, 0x108),
            partitions,
        })
    }
}

/// What an NCCH header says about its content.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Ncch {
    pub program_id: u64,
    pub product_code: String,
    pub exheader_len: u32,
    /// The parts, relative to the start of the content.
    pub exefs: Range,
    pub romfs: Range,
    /// The flags byte at 0x18F: bit 2 means no encryption.
    pub flags: [u8; 8],
}

impl Ncch {
    pub fn parse(header: &[u8]) -> Result<Self, NcchError> {
        if header.len() < HEADER_LEN || &header[0x100..0x104] != b"NCCH" {
            return Err(NcchError::NotNcch);
        }
        let mut flags = [0; 8];
        flags.copy_from_slice(&header[0x188..0x190]);
        let unit = MEDIA_UNIT << flags[6].min(16);
        let range = |at: usize| Range {
            offset: u32_at(header, at) as u64 * unit,
            len: u32_at(header, at + 4) as u64 * unit,
        };
        let code = &header[0x150..0x160];
        let end = code.iter().position(|&b| b == 0).unwrap_or(code.len());
        let ncch = Ncch {
            program_id: u64_at(header, 0x118),
            product_code: String::from_utf8_lossy(&code[..end]).into_owned(),
            exheader_len: u32_at(header, 0x180),
            exefs: range(0x1A0),
            romfs: range(0x1B0),
            flags,
        };
        if !ncch.unencrypted() {
            return Err(NcchError::Encrypted);
        }
        Ok(ncch)
    }

    /// The "NoCrypto" flag. A content with a zero key would also be readable
    /// once decrypted, but it is still encrypted on the medium.
    pub fn unencrypted(&self) -> bool {
        self.flags[7] & 4 != 0
    }
}

/// Where one segment of the program goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CodeSegment {
    pub address: u32,
    /// The pages it occupies in the code image and in memory.
    pub pages: u32,
    /// The bytes that matter.
    pub size: u32,
}

/// The parts of the extended header a loader needs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Exheader {
    pub title: String,
    /// The `.code` file in ExeFS is compressed.
    pub compressed: bool,
    pub text: CodeSegment,
    pub rodata: CodeSegment,
    pub data: CodeSegment,
    pub bss_size: u32,
    pub stack_size: u32,
    pub dependencies: Vec<u64>,
    pub save_data_size: u64,
    pub program_id: u64,
    /// The main thread's priority, and the core it prefers.
    pub priority: u8,
    pub ideal_processor: u8,
    /// The Old 3DS memory mode: 0 is 64 MB for the application, 2 is 96,
    /// 3 is 80, 4 is 72, 5 is 32.
    pub system_mode: u8,
    /// The services the program may use.
    pub services: Vec<String>,
    /// The kernel version the program was built for, `(major, minor)`.
    pub kernel_version: Option<(u8, u8)>,
}

impl Exheader {
    pub fn parse(data: &[u8]) -> Result<Self, NcchError> {
        if data.len() < EXHEADER_LEN {
            return Err(NcchError::NotExecutable);
        }
        let segment = |at: usize| CodeSegment {
            address: u32_at(data, at),
            pages: u32_at(data, at + 4),
            size: u32_at(data, at + 8),
        };
        let name = &data[..8];
        let end = name.iter().position(|&b| b == 0).unwrap_or(8);
        let services = (0..34)
            .map(|n| &data[0x250 + n * 8..0x258 + n * 8])
            .filter(|name| name[0] != 0)
            .map(|name| {
                let end = name.iter().position(|&b| b == 0).unwrap_or(8);
                String::from_utf8_lossy(&name[..end]).into_owned()
            })
            .collect();
        // Kernel capabilities: a descriptor whose top seven bits are 1111110
        // holds the version.
        let kernel_version = (0..28)
            .map(|n| u32_at(data, 0x370 + n * 4))
            .find(|d| d >> 25 == 0b111_1110)
            .map(|d| ((d >> 8) as u8, d as u8));
        Ok(Exheader {
            title: String::from_utf8_lossy(&name[..end]).into_owned(),
            compressed: data[0xD] & 1 != 0,
            text: segment(0x10),
            stack_size: u32_at(data, 0x1C),
            rodata: segment(0x20),
            data: segment(0x30),
            bss_size: u32_at(data, 0x3C),
            dependencies: (0..48)
                .map(|n| u64_at(data, 0x40 + n * 8))
                .filter(|&id| id != 0)
                .collect(),
            save_data_size: u64_at(data, 0x1C0),
            program_id: u64_at(data, 0x200),
            ideal_processor: data[0x20E] & 3,
            system_mode: data[0x20E] >> 4,
            priority: data[0x20F],
            services,
            kernel_version,
        })
    }

    /// The size of the application's memory region for the system mode.
    pub fn application_memory(&self) -> u32 {
        match self.system_mode {
            2 => 96 << 20,
            3 => 80 << 20,
            4 => 72 << 20,
            5 => 32 << 20,
            _ => 64 << 20,
        }
    }
}

/// One file of an ExeFS.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExeFsFile {
    pub name: String,
    /// From the start of the ExeFS.
    pub offset: u32,
    pub size: u32,
}

/// The file table at the start of an ExeFS.
pub fn exefs_files(header: &[u8]) -> Result<Vec<ExeFsFile>, NcchError> {
    if header.len() < HEADER_LEN {
        return Err(NcchError::OutOfBounds);
    }
    Ok((0..10)
        .map(|n| &header[n * 16..n * 16 + 16])
        .filter(|entry| entry[0] != 0)
        .map(|entry| {
            let end = entry[..8].iter().position(|&b| b == 0).unwrap_or(8);
            ExeFsFile {
                name: String::from_utf8_lossy(&entry[..end]).into_owned(),
                // Files follow the 0x200-byte header.
                offset: u32_at(entry, 8) + HEADER_LEN as u32,
                size: u32_at(entry, 12),
            }
        })
        .collect())
}

/// Lay `.code` out as the extended header asks: each segment from a page,
/// for as many pages as it says, text then read-only data then data. The
/// zeroed tail is not included.
pub fn code_segments(code: &[u8], exheader: &Exheader) -> Result<[Vec<u8>; 3], NcchError> {
    let mut at = 0usize;
    let mut take = |segment: &CodeSegment| {
        let len = segment.pages as usize * 0x1000;
        let end = (at + len).min(code.len());
        let mut bytes = code.get(at..end).ok_or(NcchError::BadCode)?.to_vec();
        bytes.resize(len, 0);
        at += len;
        Ok(bytes)
    };
    Ok([
        take(&exheader.text)?,
        take(&exheader.rodata)?,
        take(&exheader.data)?,
    ])
}

/// Undo the compression of `.code`: an LZSS variant that works from the end
/// of the file towards its start (3dbrew, "ExeFS"). The last eight bytes say
/// how long the compressed part is, where its header begins, and how much
/// longer the result is than the input.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>, NcchError> {
    let bad = NcchError::BadCode;
    if data.len() < 8 {
        return Err(bad);
    }
    let footer = data.len() - 8;
    let buffer_top_and_bottom = u32_at(data, footer);
    let extra = u32_at(data, footer + 4) as usize;
    let compressed_len = (buffer_top_and_bottom & 0x00FF_FFFF) as usize;
    let header_len = (buffer_top_and_bottom >> 24) as usize;
    if compressed_len > data.len() || header_len > compressed_len {
        return Err(bad);
    }
    let mut out = data.to_vec();
    out.resize(data.len() + extra, 0);
    // Bytes before the compressed part are stored as they are.
    let start = data.len() - compressed_len;
    let mut read = data.len() - header_len;
    let mut write = out.len();
    while read > start {
        read -= 1;
        let control = data[read];
        for bit in (0..8).rev() {
            if read <= start {
                break;
            }
            if control >> bit & 1 == 0 {
                read -= 1;
                write = write.checked_sub(1).ok_or(bad.clone())?;
                out[write] = data[read];
            } else {
                if read < start + 2 {
                    return Err(bad);
                }
                read -= 2;
                let pair = u16_at(data, read) as usize;
                let len = (pair >> 12) + 3;
                let distance = (pair & 0xFFF) + 3;
                for _ in 0..len {
                    write = write.checked_sub(1).ok_or(bad.clone())?;
                    out[write] = *out.get(write + distance).ok_or(bad.clone())?;
                }
            }
        }
    }
    Ok(out)
}

/// Where the file system proper (IVFC level 3) begins in a RomFS: after the
/// IVFC header and the master hash, on a block of level 3.
pub fn romfs_level3(header: &[u8]) -> Result<Range, NcchError> {
    if header.len() < 0x60 || &header[..4] != b"IVFC" {
        return Err(NcchError::OutOfBounds);
    }
    let master_hash = u32_at(header, 0x08) as u64;
    let block = 1u64 << u32_at(header, 0x4C).min(31);
    let offset = (0x60 + master_hash).div_ceil(block) * block;
    Ok(Range {
        offset,
        len: u64_at(header, 0x44),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(magic: &[u8; 4]) -> Vec<u8> {
        let mut h = vec![0u8; HEADER_LEN];
        h[0x100..0x104].copy_from_slice(magic);
        h
    }

    #[test]
    fn the_partition_table_is_in_media_units() {
        let mut h = header(b"NCSD");
        h[0x108..0x110].copy_from_slice(&0x0004_0000_001B_5000u64.to_le_bytes());
        h[0x120..0x128].copy_from_slice(&[0x20, 0, 0, 0, 0x10, 0, 0, 0]);
        let ncsd = Ncsd::parse(&h).unwrap();
        assert_eq!(ncsd.media_id, 0x0004_0000_001B_5000);
        assert_eq!(
            ncsd.partitions[0],
            Range {
                offset: 0x4000,
                len: 0x2000
            }
        );
        assert_eq!(ncsd.partitions[1], Range::default());
        assert_eq!(Ncsd::parse(&header(b"NCCH")), Err(NcchError::NotNcch));
    }

    #[test]
    fn an_encrypted_content_is_refused() {
        let mut h = header(b"NCCH");
        h[0x150..0x15A].copy_from_slice(b"CTR-P-TEST");
        h[0x1A0..0x1A8].copy_from_slice(&[1, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(Ncch::parse(&h), Err(NcchError::Encrypted));
        h[0x18F] = 4;
        let ncch = Ncch::parse(&h).unwrap();
        assert_eq!(ncch.product_code, "CTR-P-TEST");
        assert_eq!(
            ncch.exefs,
            Range {
                offset: 0x200,
                len: 0x400
            }
        );
    }

    #[test]
    fn the_extended_header_gives_the_layout_and_the_process_settings() {
        let mut x = vec![0u8; EXHEADER_LEN];
        x[..6].copy_from_slice(b"momiji");
        x[0xD] = 1;
        let segment = |a: u32, p: u32, s: u32| -> Vec<u8> {
            [a, p, s].iter().flat_map(|v| v.to_le_bytes()).collect()
        };
        x[0x10..0x1C].copy_from_slice(&segment(0x10_0000, 2, 0x1800));
        x[0x1C..0x20].copy_from_slice(&0x4000u32.to_le_bytes());
        x[0x20..0x2C].copy_from_slice(&segment(0x10_2000, 1, 0x10));
        x[0x30..0x3C].copy_from_slice(&segment(0x10_3000, 1, 0x20));
        x[0x3C..0x40].copy_from_slice(&0x3000u32.to_le_bytes());
        x[0x40..0x48].copy_from_slice(&0x0004_0130_0000_1C02u64.to_le_bytes());
        x[0x20E] = 0x30 | 1;
        x[0x20F] = 0x30;
        x[0x250..0x258].copy_from_slice(b"fs:USER\0");
        x[0x258..0x260].copy_from_slice(b"gsp::Gpu");
        x[0x374..0x378].copy_from_slice(&(0xFC00_0000u32 | 2 << 8 | 0x33).to_le_bytes());

        let ex = Exheader::parse(&x).unwrap();
        assert_eq!(ex.title, "momiji");
        assert!(ex.compressed);
        assert_eq!(
            ex.text,
            CodeSegment {
                address: 0x10_0000,
                pages: 2,
                size: 0x1800
            }
        );
        assert_eq!((ex.stack_size, ex.bss_size), (0x4000, 0x3000));
        assert_eq!(ex.dependencies, [0x0004_0130_0000_1C02]);
        assert_eq!(
            (ex.priority, ex.ideal_processor, ex.system_mode),
            (0x30, 1, 3)
        );
        assert_eq!(ex.application_memory(), 80 << 20);
        assert_eq!(ex.services, ["fs:USER", "gsp::Gpu"]);
        assert_eq!(ex.kernel_version, Some((2, 0x33)));

        let code: Vec<u8> = (0..0x3800u32).map(|n| n as u8).collect();
        let [text, rodata, data] = code_segments(&code, &ex).unwrap();
        assert_eq!(
            (text.len(), rodata.len(), data.len()),
            (0x2000, 0x1000, 0x1000)
        );
        assert_eq!(rodata[0], code[0x2000]);
        assert_eq!(data[0x7FF], code[0x37FF]);
        assert_eq!(data[0x800..], [0; 0x800], "the image ends early");
    }

    #[test]
    fn the_exefs_table_lists_its_files_after_the_header() {
        let mut h = vec![0u8; HEADER_LEN];
        h[..5].copy_from_slice(b".code");
        h[12..16].copy_from_slice(&0x100u32.to_le_bytes());
        h[16..20].copy_from_slice(b"icon");
        h[24..28].copy_from_slice(&0x100u32.to_le_bytes());
        h[28..32].copy_from_slice(&0x36C0u32.to_le_bytes());
        let files = exefs_files(&h).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(
            files[0],
            ExeFsFile {
                name: ".code".into(),
                offset: 0x200,
                size: 0x100
            }
        );
        assert_eq!((files[1].offset, files[1].size), (0x300, 0x36C0));
    }

    #[test]
    fn decompression_runs_from_the_end_with_literals_and_back_references() {
        // "abc" eight times. Read from the end: a control byte saying
        // literal, literal, literal, reference, literal, literal, literal;
        // the last three bytes; a copy of 18 bytes from three further on;
        // the first three bytes.
        let mut packed = b"abc".to_vec();
        packed.extend_from_slice(&0xF000u16.to_le_bytes());
        packed.extend_from_slice(b"abc");
        packed.push(0b0001_0000);
        // The compressed part is the whole file, its header the footer.
        let len = packed.len() as u32 + 8;
        packed.extend_from_slice(&(8u32 << 24 | len).to_le_bytes());
        packed.extend_from_slice(&(24 - len).to_le_bytes());
        assert_eq!(decompress(&packed).unwrap(), b"abc".repeat(8));

        // A reference past the end of what is written.
        packed[3..5].copy_from_slice(&0xF0FFu16.to_le_bytes());
        assert_eq!(decompress(&packed), Err(NcchError::BadCode));
        assert_eq!(decompress(&[0; 4]), Err(NcchError::BadCode));
    }

    #[test]
    fn level_3_follows_the_master_hash_on_a_block() {
        let mut h = vec![0u8; 0x60];
        h[..4].copy_from_slice(b"IVFC");
        h[0x08..0x0C].copy_from_slice(&0x20u32.to_le_bytes());
        h[0x44..0x4C].copy_from_slice(&0x1234u64.to_le_bytes());
        h[0x4C..0x50].copy_from_slice(&12u32.to_le_bytes());
        assert_eq!(
            romfs_level3(&h).unwrap(),
            Range {
                offset: 0x1000,
                len: 0x1234
            }
        );
        h[0x08..0x0C].copy_from_slice(&0x1000u32.to_le_bytes());
        assert_eq!(romfs_level3(&h).unwrap().offset, 0x2000);
        assert!(romfs_level3(&h[..0x40]).is_err());
    }
}
