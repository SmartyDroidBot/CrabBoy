//! A minimal FAT16 volume builder and reader (Microsoft, "FAT32 File System
//! Specification", which also defines FAT12 and FAT16).
//!
//! It exists to make SD card images for tests and tools: files with 8.3
//! names in the root directory of an unpartitioned volume, laid out in
//! contiguous cluster chains. That is a volume every FAT driver mounts.

const SECTOR: usize = 512;
const SECTORS_PER_CLUSTER: usize = 4;
const RESERVED_SECTORS: usize = 1;
const FATS: usize = 2;
const ROOT_ENTRIES: usize = 512;
const ROOT_SECTORS: usize = ROOT_ENTRIES * 32 / SECTOR;
const END_OF_CHAIN: u16 = 0xFFFF;

/// FAT16 needs between 4085 and 65524 clusters.
const MIN_CLUSTERS: usize = 4085;
const MAX_CLUSTERS: usize = 65524;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FatError {
    /// The volume would have too few or too many clusters for FAT16.
    BadSize,
    /// Not an upper-case 8.3 name.
    BadName(String),
    TooManyFiles,
    /// The files do not fit.
    Full,
    /// Not a FAT16 volume this module wrote or can read.
    NotFat16,
}

impl std::fmt::Display for FatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FatError::BadSize => write!(f, "a FAT16 volume needs 4085 to 65524 clusters"),
            FatError::BadName(name) => write!(f, "{name:?} is not an upper-case 8.3 name"),
            FatError::TooManyFiles => write!(f, "the root directory holds {ROOT_ENTRIES} files"),
            FatError::Full => write!(f, "the files do not fit in the volume"),
            FatError::NotFat16 => write!(f, "not a FAT16 volume"),
        }
    }
}

impl std::error::Error for FatError {}

/// `NAME.EXT` as the eleven space-padded bytes of a directory entry.
fn short_name(name: &str) -> Result<[u8; 11], FatError> {
    let bad = || FatError::BadName(name.to_string());
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    let valid = |part: &str, max: usize| {
        part.len() <= max
            && part
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b"_-~".contains(&b))
    };
    if base.is_empty() || !valid(base, 8) || !valid(ext, 3) {
        return Err(bad());
    }
    let mut out = [b' '; 11];
    out[..base.len()].copy_from_slice(base.as_bytes());
    out[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    Ok(out)
}

struct Layout {
    fat_sectors: usize,
    clusters: usize,
}

fn layout(sectors: usize) -> Result<Layout, FatError> {
    // Each cluster costs its sectors plus two bytes in each FAT.
    let available = sectors
        .checked_sub(RESERVED_SECTORS + ROOT_SECTORS)
        .ok_or(FatError::BadSize)?;
    let clusters = available * SECTOR / (SECTORS_PER_CLUSTER * SECTOR + 2 * FATS);
    let fat_sectors = ((clusters + 2) * 2).div_ceil(SECTOR);
    let clusters = (available - FATS * fat_sectors) / SECTORS_PER_CLUSTER;
    if !(MIN_CLUSTERS..=MAX_CLUSTERS).contains(&clusters) {
        return Err(FatError::BadSize);
    }
    Ok(Layout {
        fat_sectors,
        clusters,
    })
}

/// Build a FAT16 volume of `sectors` 512-byte sectors holding `files` in its
/// root directory.
pub fn build(files: &[(&str, &[u8])], sectors: u32) -> Result<Vec<u8>, FatError> {
    let sectors = sectors as usize;
    let Layout {
        fat_sectors,
        clusters,
    } = layout(sectors)?;
    if files.len() > ROOT_ENTRIES {
        return Err(FatError::TooManyFiles);
    }
    let mut image = vec![0u8; sectors * SECTOR];

    // The boot sector and its BIOS parameter block.
    let boot = &mut image[..SECTOR];
    boot[..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
    boot[3..11].copy_from_slice(b"CRABBOY ");
    boot[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
    boot[13] = SECTORS_PER_CLUSTER as u8;
    boot[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
    boot[16] = FATS as u8;
    boot[17..19].copy_from_slice(&(ROOT_ENTRIES as u16).to_le_bytes());
    if let Ok(small) = u16::try_from(sectors) {
        boot[19..21].copy_from_slice(&small.to_le_bytes());
    } else {
        boot[32..36].copy_from_slice(&(sectors as u32).to_le_bytes());
    }
    boot[21] = 0xF8;
    boot[22..24].copy_from_slice(&(fat_sectors as u16).to_le_bytes());
    boot[24..26].copy_from_slice(&63u16.to_le_bytes());
    boot[26..28].copy_from_slice(&255u16.to_le_bytes());
    boot[36] = 0x80;
    boot[38] = 0x29;
    boot[39..43].copy_from_slice(&0x3D5C_AB01u32.to_le_bytes());
    boot[43..54].copy_from_slice(b"CRABBOY SD ");
    boot[54..62].copy_from_slice(b"FAT16   ");
    boot[510..512].copy_from_slice(&[0x55, 0xAA]);

    let fat_start = RESERVED_SECTORS * SECTOR;
    let root_start = fat_start + FATS * fat_sectors * SECTOR;
    let data_start = root_start + ROOT_SECTORS * SECTOR;
    let cluster_bytes = SECTORS_PER_CLUSTER * SECTOR;

    let mut fat = vec![0u16; clusters + 2];
    fat[0] = 0xFFF8;
    fat[1] = END_OF_CHAIN;
    let mut next_cluster = 2usize;
    for (index, (name, data)) in files.iter().enumerate() {
        let entry = root_start + index * 32;
        image[entry..entry + 11].copy_from_slice(&short_name(name)?);
        image[entry + 11] = 0x20;
        image[entry + 28..entry + 32].copy_from_slice(&(data.len() as u32).to_le_bytes());
        if data.is_empty() {
            continue;
        }
        let needed = data.len().div_ceil(cluster_bytes);
        if next_cluster + needed > clusters + 2 {
            return Err(FatError::Full);
        }
        image[entry + 26..entry + 28].copy_from_slice(&(next_cluster as u16).to_le_bytes());
        for n in 0..needed {
            let cluster = next_cluster + n;
            fat[cluster] = if n + 1 == needed {
                END_OF_CHAIN
            } else {
                cluster as u16 + 1
            };
        }
        let at = data_start + (next_cluster - 2) * cluster_bytes;
        image[at..at + data.len()].copy_from_slice(data);
        next_cluster += needed;
    }
    for copy in 0..FATS {
        let at = fat_start + copy * fat_sectors * SECTOR;
        for (n, entry) in fat.iter().enumerate() {
            image[at + n * 2..at + n * 2 + 2].copy_from_slice(&entry.to_le_bytes());
        }
    }
    Ok(image)
}

/// The files in the root directory of a FAT16 volume, with their contents.
pub fn read_root(image: &[u8]) -> Result<Vec<(String, Vec<u8>)>, FatError> {
    let u16_at = |at: usize| -> Result<usize, FatError> {
        let bytes = image.get(at..at + 2).ok_or(FatError::NotFat16)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]) as usize)
    };
    if image.get(54..62) != Some(b"FAT16   ") || u16_at(11)? != SECTOR {
        return Err(FatError::NotFat16);
    }
    let cluster_bytes = *image.get(13).ok_or(FatError::NotFat16)? as usize * SECTOR;
    let fat_start = u16_at(14)? * SECTOR;
    let root_entries = u16_at(17)?;
    let root_start = fat_start + image[16] as usize * u16_at(22)? * SECTOR;
    let data_start = root_start + root_entries * 32;

    let mut files = Vec::new();
    for index in 0..root_entries {
        let entry = image
            .get(root_start + index * 32..root_start + index * 32 + 32)
            .ok_or(FatError::NotFat16)?;
        match entry[0] {
            0x00 => break,
            0xE5 => continue,
            _ if entry[11] & 0x18 != 0 => continue,
            _ => {}
        }
        let base = String::from_utf8_lossy(&entry[..8]).trim_end().to_string();
        let ext = String::from_utf8_lossy(&entry[8..11])
            .trim_end()
            .to_string();
        let name = if ext.is_empty() {
            base
        } else {
            format!("{base}.{ext}")
        };
        let size = u32::from_le_bytes([entry[28], entry[29], entry[30], entry[31]]) as usize;
        let mut cluster = u16::from_le_bytes([entry[26], entry[27]]) as usize;
        let mut data = Vec::with_capacity(size);
        while data.len() < size && (2..0xFFF8).contains(&cluster) {
            let at = data_start + (cluster - 2) * cluster_bytes;
            let chunk = image
                .get(at..at + cluster_bytes)
                .ok_or(FatError::NotFat16)?;
            data.extend_from_slice(&chunk[..chunk.len().min(size - data.len())]);
            cluster = u16_at(fat_start + cluster * 2)?;
        }
        files.push((name, data));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECTORS_32_MB: u32 = 65536;

    #[test]
    fn files_round_trip() {
        let big: Vec<u8> = (0..10_000u32).map(|n| n as u8).collect();
        let image = build(
            &[("HELLO.TXT", b"hi"), ("BIG.BIN", &big), ("EMPTY", b"")],
            SECTORS_32_MB,
        )
        .unwrap();
        assert_eq!(image.len(), SECTORS_32_MB as usize * SECTOR);
        assert_eq!(image[510..512], [0x55, 0xAA]);
        let files = read_root(&image).unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0], ("HELLO.TXT".to_string(), b"hi".to_vec()));
        assert_eq!(files[1].1, big);
        assert_eq!(files[2], ("EMPTY".to_string(), Vec::new()));
    }

    #[test]
    fn the_two_fats_agree_and_chain_the_clusters() {
        let data = vec![1u8; SECTORS_PER_CLUSTER * SECTOR * 2 + 1];
        let image = build(&[("A.BIN", &data)], SECTORS_32_MB).unwrap();
        let fat_sectors = u16::from_le_bytes([image[22], image[23]]) as usize;
        let first = &image[SECTOR..SECTOR + fat_sectors * SECTOR];
        let second = &image[SECTOR + fat_sectors * SECTOR..SECTOR + 2 * fat_sectors * SECTOR];
        assert_eq!(first, second);
        assert_eq!(
            first[..10],
            [0xF8, 0xFF, 0xFF, 0xFF, 3, 0, 4, 0, 0xFF, 0xFF]
        );
    }

    #[test]
    fn rejects_what_it_cannot_represent() {
        assert_eq!(build(&[], 1000), Err(FatError::BadSize));
        assert_eq!(build(&[], 600_000), Err(FatError::BadSize));
        assert!(matches!(
            build(&[("lower.txt", b"")], SECTORS_32_MB),
            Err(FatError::BadName(_))
        ));
        assert!(matches!(
            build(&[("WAYTOOLONGNAME.TXT", b"")], SECTORS_32_MB),
            Err(FatError::BadName(_))
        ));
        let huge = vec![0u8; 40 << 20];
        assert_eq!(
            build(&[("HUGE.BIN", &huge)], SECTORS_32_MB),
            Err(FatError::Full)
        );
        assert_eq!(read_root(&[0u8; 1024]), Err(FatError::NotFat16));
    }
}
