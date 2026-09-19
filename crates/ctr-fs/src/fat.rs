//! A minimal FAT16 volume builder and reader (Microsoft, "FAT32 File System
//! Specification", which also defines FAT12 and FAT16).
//!
//! It exists to make SD card images for tests and tools: an unpartitioned
//! volume of files and directories laid out in contiguous cluster chains,
//! which every FAT driver mounts. A name that is not upper-case 8.3 gets long
//! name entries and a `NAME~1.EXT` alias, as the specification describes.

const SECTOR: usize = 512;
const SECTORS_PER_CLUSTER: usize = 4;
const CLUSTER_BYTES: usize = SECTORS_PER_CLUSTER * SECTOR;
const RESERVED_SECTORS: usize = 1;
const FATS: usize = 2;
const ROOT_ENTRIES: usize = 512;
const ROOT_SECTORS: usize = ROOT_ENTRIES * 32 / SECTOR;
const END_OF_CHAIN: u16 = 0xFFFF;

const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LONG_NAME: u8 = 0x0F;
const ATTR_VOLUME: u8 = 0x08;
/// Where the thirteen UTF-16 units of a long name entry sit.
const LONG_UNIT_AT: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];

/// FAT16 needs between 4085 and 65524 clusters.
const MIN_CLUSTERS: usize = 4085;
const MAX_CLUSTERS: usize = 65524;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FatError {
    /// The volume would have too few or too many clusters for FAT16.
    BadSize,
    /// Empty, longer than 255 characters, with a character FAT forbids, or a
    /// path through something that is a file.
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
            FatError::BadName(name) => write!(f, "{name:?} is not a valid FAT name"),
            FatError::TooManyFiles => write!(f, "the root directory holds {ROOT_ENTRIES} entries"),
            FatError::Full => write!(f, "the files do not fit in the volume"),
            FatError::NotFat16 => write!(f, "not a FAT16 volume"),
        }
    }
}

impl std::error::Error for FatError {}

/// `NAME.EXT` as the eleven space-padded bytes of a directory entry, if it
/// is an upper-case 8.3 name.
fn short_name(name: &str) -> Option<[u8; 11]> {
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    let valid = |part: &str, max: usize| {
        part.len() <= max
            && part
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b"_-~".contains(&b))
    };
    if base.is_empty() || !valid(base, 8) || !valid(ext, 3) {
        return None;
    }
    let mut out = [b' '; 11];
    out[..base.len()].copy_from_slice(base.as_bytes());
    out[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    Some(out)
}

/// The checksum of a short name that ties long name entries to it.
fn short_name_checksum(short: &[u8]) -> u8 {
    short
        .iter()
        .fold(0u8, |sum, &b| sum.rotate_right(1).wrapping_add(b))
}

/// The directory entries of one name: long name entries, if it needs them,
/// then the short entry with attribute, first cluster and size left blank.
/// `taken` holds the short names already in the directory.
fn name_entries(name: &str, taken: &mut Vec<[u8; 11]>) -> Result<Vec<[u8; 32]>, FatError> {
    let bad = || FatError::BadName(name.to_string());
    let mut entry = [0u8; 32];
    if let Some(short) = short_name(name) {
        entry[..11].copy_from_slice(&short);
        taken.push(short);
        return Ok(vec![entry]);
    }
    let units: Vec<u16> = name.encode_utf16().collect();
    let forbidden = |c: char| c.is_control() || "\"*/:<>?\\|".contains(c);
    if units.is_empty() || units.len() > 255 || name.chars().any(forbidden) || name.ends_with('.') {
        return Err(bad());
    }

    // The alias: what survives of the base and of the last extension, upper
    // case, up to six characters, a tilde and the first free number.
    let keep = |part: &str, max: usize| -> Vec<u8> {
        part.bytes()
            .filter(|b| b.is_ascii_alphanumeric() || b"_-~".contains(b))
            .map(|b| b.to_ascii_uppercase())
            .take(max)
            .collect()
    };
    let (base, ext) = match name.trim_start_matches('.').rsplit_once('.') {
        Some((base, ext)) => (keep(base, 6), keep(ext, 3)),
        None => (keep(name, 6), Vec::new()),
    };
    let short = (1..1_000_000u32)
        .map(|n| {
            let tail = format!("~{n}");
            let mut short = [b' '; 11];
            let base = &base[..base.len().min(8 - tail.len())];
            short[..base.len()].copy_from_slice(base);
            short[base.len()..base.len() + tail.len()].copy_from_slice(tail.as_bytes());
            short[8..8 + ext.len()].copy_from_slice(&ext);
            short
        })
        .find(|short| !taken.contains(short))
        .ok_or_else(bad)?;
    taken.push(short);

    // Thirteen UTF-16 units per entry, last part first, ended by a zero and
    // padded with 0xFFFF.
    let mut padded = units;
    if !padded.len().is_multiple_of(13) {
        padded.push(0);
        padded.resize(padded.len().div_ceil(13) * 13, 0xFFFF);
    }
    let count = padded.len() / 13;
    let mut entries = Vec::with_capacity(count + 1);
    for part in (0..count).rev() {
        let mut long = [0u8; 32];
        long[0] = (part + 1) as u8 | if part + 1 == count { 0x40 } else { 0 };
        long[11] = ATTR_LONG_NAME;
        long[13] = short_name_checksum(&short);
        for (unit, at) in padded[part * 13..(part + 1) * 13].iter().zip(LONG_UNIT_AT) {
            long[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        entries.push(long);
    }
    entry[..11].copy_from_slice(&short);
    entries.push(entry);
    Ok(entries)
}

/// A directory being built: files and directories in the order given.
#[derive(Default)]
struct Dir<'a> {
    children: Vec<(&'a str, Node<'a>)>,
}

enum Node<'a> {
    File(&'a [u8]),
    Dir(Dir<'a>),
}

impl<'a> Dir<'a> {
    fn insert(&mut self, path: &'a str, data: &'a [u8]) -> Result<(), FatError> {
        let Some((first, rest)) = path.split_once('/') else {
            self.children.push((path, Node::File(data)));
            return Ok(());
        };
        let at = match self.children.iter().position(|(name, _)| *name == first) {
            Some(at) => at,
            None => {
                self.children.push((first, Node::Dir(Dir::default())));
                self.children.len() - 1
            }
        };
        match &mut self.children[at].1 {
            Node::Dir(dir) => dir.insert(rest, data),
            Node::File(_) => Err(FatError::BadName(path.to_string())),
        }
    }

    /// The directory entries of every child's name.
    fn names(&self) -> Result<Vec<Vec<[u8; 32]>>, FatError> {
        let mut taken = Vec::new();
        self.children
            .iter()
            .map(|(name, _)| name_entries(name, &mut taken))
            .collect()
    }
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
    let clusters = available * SECTOR / (CLUSTER_BYTES + 2 * FATS);
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

/// The volume while it is filled: clusters are handed out in order.
struct Volume {
    image: Vec<u8>,
    fat: Vec<u16>,
    next_cluster: usize,
    data_start: usize,
}

impl Volume {
    /// A contiguous chain holding `bytes`; its first cluster.
    fn allocate(&mut self, bytes: usize) -> Result<usize, FatError> {
        let needed = bytes.div_ceil(CLUSTER_BYTES);
        let first = self.next_cluster;
        if first + needed > self.fat.len() {
            return Err(FatError::Full);
        }
        for cluster in first..first + needed {
            self.fat[cluster] = if cluster + 1 == first + needed {
                END_OF_CHAIN
            } else {
                cluster as u16 + 1
            };
        }
        self.next_cluster += needed;
        Ok(first)
    }

    fn cluster_at(&self, cluster: usize) -> usize {
        self.data_start + (cluster - 2) * CLUSTER_BYTES
    }

    /// Write `dir`, whose names are `names`, with its entries at `at`; the
    /// clusters of each child follow in order. `own` is its first cluster
    /// and `parent` that of its parent, zero standing for the root, which
    /// has no dot entries.
    fn write_dir(
        &mut self,
        dir: &Dir<'_>,
        names: Vec<Vec<[u8; 32]>>,
        mut at: usize,
        own: usize,
        parent: Option<usize>,
    ) -> Result<(), FatError> {
        if let Some(parent) = parent {
            for (name, cluster) in [(b".          ", own), (b"..         ", parent)] {
                self.image[at..at + 11].copy_from_slice(name);
                self.image[at + 11] = ATTR_DIRECTORY;
                self.image[at + 26..at + 28].copy_from_slice(&(cluster as u16).to_le_bytes());
                at += 32;
            }
        }
        for ((_, node), mut entries) in dir.children.iter().zip(names) {
            let short = entries.last_mut().expect("a name has a short entry");
            match node {
                Node::File(data) => {
                    short[11] = ATTR_ARCHIVE;
                    short[28..32].copy_from_slice(&(data.len() as u32).to_le_bytes());
                    if !data.is_empty() {
                        let first = self.allocate(data.len())?;
                        short[26..28].copy_from_slice(&(first as u16).to_le_bytes());
                        let to = self.cluster_at(first);
                        self.image[to..to + data.len()].copy_from_slice(data);
                    }
                }
                Node::Dir(child) => {
                    let names = child.names()?;
                    let slots = 2 + names.iter().map(Vec::len).sum::<usize>();
                    let first = self.allocate(slots * 32)?;
                    short[11] = ATTR_DIRECTORY;
                    short[26..28].copy_from_slice(&(first as u16).to_le_bytes());
                    let to = self.cluster_at(first);
                    self.write_dir(child, names, to, first, Some(own))?;
                }
            }
            for entry in entries {
                self.image[at..at + 32].copy_from_slice(&entry);
                at += 32;
            }
        }
        Ok(())
    }
}

/// Build a FAT16 volume of `sectors` 512-byte sectors holding `files`. A name
/// is a path with `/` between directories, which are made where they are
/// first named.
pub fn build(files: &[(&str, &[u8])], sectors: u32) -> Result<Vec<u8>, FatError> {
    let sectors = sectors as usize;
    let Layout {
        fat_sectors,
        clusters,
    } = layout(sectors)?;
    let mut root = Dir::default();
    for (path, data) in files {
        root.insert(path, data)?;
    }
    let names = root.names()?;
    if names.iter().map(Vec::len).sum::<usize>() > ROOT_ENTRIES {
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
    let mut fat = vec![0u16; clusters + 2];
    fat[0] = 0xFFF8;
    fat[1] = END_OF_CHAIN;
    let mut volume = Volume {
        image,
        fat,
        next_cluster: 2,
        data_start: root_start + ROOT_SECTORS * SECTOR,
    };
    volume.write_dir(&root, names, root_start, 0, None)?;

    let Volume { mut image, fat, .. } = volume;
    for copy in 0..FATS {
        let at = fat_start + copy * fat_sectors * SECTOR;
        for (n, entry) in fat.iter().enumerate() {
            image[at + n * 2..at + n * 2 + 2].copy_from_slice(&entry.to_le_bytes());
        }
    }
    Ok(image)
}

/// One entry of a directory as the reader sees it.
struct Found {
    /// The long name if the entry has one, else the short name.
    name: String,
    directory: bool,
    cluster: usize,
    size: usize,
}

struct Reader<'a> {
    image: &'a [u8],
    cluster_bytes: usize,
    fat_start: usize,
    root_start: usize,
    root_entries: usize,
    data_start: usize,
}

impl<'a> Reader<'a> {
    fn u16_at(&self, at: usize) -> Result<usize, FatError> {
        let bytes = self.image.get(at..at + 2).ok_or(FatError::NotFat16)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]) as usize)
    }

    fn open(image: &'a [u8]) -> Result<Self, FatError> {
        let mut reader = Reader {
            image,
            cluster_bytes: 0,
            fat_start: 0,
            root_start: 0,
            root_entries: 0,
            data_start: 0,
        };
        if image.get(54..62) != Some(b"FAT16   ") || reader.u16_at(11)? != SECTOR {
            return Err(FatError::NotFat16);
        }
        reader.cluster_bytes = image[13] as usize * SECTOR;
        reader.fat_start = reader.u16_at(14)? * SECTOR;
        reader.root_entries = reader.u16_at(17)?;
        reader.root_start = reader.fat_start + image[16] as usize * reader.u16_at(22)? * SECTOR;
        reader.data_start = reader.root_start + reader.root_entries * 32;
        Ok(reader)
    }

    /// The bytes of a cluster chain, cut to `size` if given.
    fn chain(&self, first: usize, size: Option<usize>) -> Result<Vec<u8>, FatError> {
        let mut data = Vec::new();
        let mut cluster = first;
        while (2..0xFFF8).contains(&cluster) && size.is_none_or(|size| data.len() < size) {
            let at = self.data_start + (cluster - 2) * self.cluster_bytes;
            let chunk = self
                .image
                .get(at..at + self.cluster_bytes)
                .ok_or(FatError::NotFat16)?;
            data.extend_from_slice(chunk);
            cluster = self.u16_at(self.fat_start + cluster * 2)?;
            if data.len() > self.image.len() {
                return Err(FatError::NotFat16);
            }
        }
        if let Some(size) = size {
            data.truncate(size);
        }
        Ok(data)
    }

    /// The entries of the root (`None`) or of the directory at a cluster,
    /// without dot entries and volume labels.
    fn list(&self, dir: Option<usize>) -> Result<Vec<Found>, FatError> {
        let raw = match dir {
            None => self
                .image
                .get(self.root_start..self.root_start + self.root_entries * 32)
                .ok_or(FatError::NotFat16)?
                .to_vec(),
            Some(cluster) => self.chain(cluster, None)?,
        };
        let mut found = Vec::new();
        let mut long: Vec<u16> = Vec::new();
        for entry in raw.as_chunks::<32>().0 {
            match entry[0] {
                0x00 => break,
                0xE5 => {
                    long.clear();
                    continue;
                }
                _ => {}
            }
            if entry[11] == ATTR_LONG_NAME {
                // Parts come last first: each goes in front of the rest.
                let part = LONG_UNIT_AT.map(|at| u16::from_le_bytes([entry[at], entry[at + 1]]));
                long.splice(0..0, part);
                continue;
            }
            let units = std::mem::take(&mut long);
            if entry[11] & ATTR_VOLUME != 0 || entry[0] == b'.' {
                continue;
            }
            let base = String::from_utf8_lossy(&entry[..8]).trim_end().to_string();
            let ext = String::from_utf8_lossy(&entry[8..11])
                .trim_end()
                .to_string();
            let short = if ext.is_empty() {
                base
            } else {
                format!("{base}.{ext}")
            };
            let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
            found.push(Found {
                name: if units.is_empty() {
                    short
                } else {
                    String::from_utf16_lossy(&units[..end])
                },
                directory: entry[11] & ATTR_DIRECTORY != 0,
                cluster: u16::from_le_bytes([entry[26], entry[27]]) as usize,
                size: u32::from_le_bytes([entry[28], entry[29], entry[30], entry[31]]) as usize,
            });
        }
        Ok(found)
    }
}

/// The files in the root directory of a FAT16 volume, with their contents.
/// Directories are left out.
pub fn read_root(image: &[u8]) -> Result<Vec<(String, Vec<u8>)>, FatError> {
    let reader = Reader::open(image)?;
    reader
        .list(None)?
        .into_iter()
        .filter(|found| !found.directory)
        .map(|found| Ok((found.name, reader.chain(found.cluster, Some(found.size))?)))
        .collect()
}

/// The contents of the file at `path` (`/` between directories, names
/// compared without regard to ASCII case), or `None` if there is none.
pub fn read_path(image: &[u8], path: &str) -> Result<Option<Vec<u8>>, FatError> {
    let reader = Reader::open(image)?;
    let mut dir = None;
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        let Some(found) = reader
            .list(dir)?
            .into_iter()
            .find(|found| found.name.eq_ignore_ascii_case(part))
        else {
            return Ok(None);
        };
        match (parts.peek().is_some(), found.directory) {
            (true, true) => dir = Some(found.cluster),
            (false, false) => return reader.chain(found.cluster, Some(found.size)).map(Some),
            _ => return Ok(None),
        }
    }
    Ok(None)
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
        let data = vec![1u8; CLUSTER_BYTES * 2 + 1];
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
    fn an_upper_case_short_name_is_a_single_entry() {
        let image = build(&[("HELLO.TXT", b"hi")], SECTORS_32_MB).unwrap();
        let reader = Reader::open(&image).unwrap();
        let root = &image[reader.root_start..reader.root_start + 64];
        assert_eq!(&root[..11], b"HELLO   TXT");
        assert_eq!(root[11], ATTR_ARCHIVE);
        assert_eq!(root[32], 0, "no second entry");
    }

    #[test]
    fn long_names_get_entries_a_checksum_and_a_numbered_alias() {
        let image = build(
            &[
                ("nintendo3ds_ctr.dtb", b"a"),
                ("nintendo3ds_ktr.dtb", b"b"),
                ("zImage", b"c"),
            ],
            SECTORS_32_MB,
        )
        .unwrap();
        let reader = Reader::open(&image).unwrap();
        let root = &image[reader.root_start..];
        // Nineteen characters: two long entries, the last part first.
        assert_eq!((root[0], root[11]), (0x42, ATTR_LONG_NAME));
        assert_eq!((root[32], root[32 + 11]), (0x01, ATTR_LONG_NAME));
        let short = &root[64..64 + 11];
        assert_eq!(short, b"NINTEN~1DTB");
        assert_eq!(root[13], short_name_checksum(short));
        assert_eq!(&root[32 + 1..32 + 5], [b'n', 0, b'i', 0]);
        // The second name collides with the first alias.
        assert_eq!(&root[5 * 32..5 * 32 + 11], b"NINTEN~2DTB");
        // Mixed case alone also needs a long entry.
        assert_eq!(&root[7 * 32..7 * 32 + 11], b"ZIMAGE~1   ");

        let names: Vec<String> = read_root(&image)
            .unwrap()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            names,
            ["nintendo3ds_ctr.dtb", "nintendo3ds_ktr.dtb", "zImage"]
        );
        assert_eq!(
            read_path(&image, "NINTENDO3DS_KTR.DTB").unwrap(),
            Some(b"b".to_vec())
        );
    }

    #[test]
    fn directories_nest_and_carry_dot_entries() {
        let kernel = vec![7u8; CLUSTER_BYTES + 5];
        let image = build(
            &[
                ("HELLO.TXT", b"hi"),
                ("linux/zImage", &kernel),
                ("linux/initramfs.cpio.gz", b"rootfs"),
                ("linux/more/DEEP.BIN", b"deep"),
                ("LAST.TXT", b"last"),
            ],
            SECTORS_32_MB,
        )
        .unwrap();
        assert_eq!(read_path(&image, "linux/zImage").unwrap(), Some(kernel));
        assert_eq!(
            read_path(&image, "linux/initramfs.cpio.gz").unwrap(),
            Some(b"rootfs".to_vec())
        );
        assert_eq!(
            read_path(&image, "linux/more/DEEP.BIN").unwrap(),
            Some(b"deep".to_vec())
        );
        assert_eq!(
            read_path(&image, "LAST.TXT").unwrap(),
            Some(b"last".to_vec())
        );
        assert_eq!(read_path(&image, "linux/absent").unwrap(), None);
        assert_eq!(read_path(&image, "linux").unwrap(), None);
        assert_eq!(read_path(&image, "HELLO.TXT/x").unwrap(), None);
        // Only files are listed at the root.
        assert_eq!(read_root(&image).unwrap().len(), 2);

        // HELLO.TXT has cluster 2, so the directory is at 3 with its dots.
        let reader = Reader::open(&image).unwrap();
        let dir = &image[reader.data_start + CLUSTER_BYTES..];
        assert_eq!(&dir[..11], b".          ");
        assert_eq!(u16::from_le_bytes([dir[26], dir[27]]), 3);
        assert_eq!(&dir[32..43], b"..         ");
        assert_eq!(u16::from_le_bytes([dir[58], dir[59]]), 0, "the root");
    }

    #[test]
    fn rejects_what_it_cannot_represent() {
        assert_eq!(build(&[], 1000), Err(FatError::BadSize));
        assert_eq!(build(&[], 600_000), Err(FatError::BadSize));
        for name in ["", "a*b", "dir//x", "ends.", "A.TXT/x"] {
            let files: [(&str, &[u8]); 2] = [("A.TXT", b""), (name, b"")];
            assert!(
                matches!(build(&files, SECTORS_32_MB), Err(FatError::BadName(_))),
                "{name:?}"
            );
        }
        let huge = vec![0u8; 40 << 20];
        assert_eq!(
            build(&[("HUGE.BIN", &huge)], SECTORS_32_MB),
            Err(FatError::Full)
        );
        assert_eq!(read_root(&[0u8; 1024]), Err(FatError::NotFat16));
    }
}
