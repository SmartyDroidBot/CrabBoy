//! The 3DSX homebrew executable (3dbrew, "3DSX Format").
//!
//! Three segments (code, read-only data, data with its zero-filled tail) that
//! are linked as if they lay back to back from address zero, each starting on
//! a page. A loader puts them wherever it likes and patches the words the
//! relocation tables point at. A table is a run of pairs: skip this many
//! words, then patch this many. The word to patch holds an address in the
//! file's own address space in its low 28 bits and a kind in the top four:
//! in the absolute tables kind 0 becomes the address; in the relative tables
//! it becomes the distance from the patched word, in 32 bits (kind 0) or 31
//! (kind 1, for `R_ARM_PREL31`).
//!
//! An extended header, if present, points at an icon (SMDH) and a RomFS
//! image at the end of the file.

const PAGE: u32 = 0x1000;
const SEGMENTS: usize = 3;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ThreeDsxError {
    /// No `3DSX` magic, or shorter than its headers say.
    NotThreeDsx,
    /// A segment or a relocation table reaches outside the file.
    Truncated,
    /// A relocation points outside its segment, or at an address outside the
    /// program.
    BadRelocation { segment: usize },
}

impl std::fmt::Display for ThreeDsxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThreeDsxError::NotThreeDsx => write!(f, "not a 3DSX executable"),
            ThreeDsxError::Truncated => write!(f, "the 3DSX file is cut short"),
            ThreeDsxError::BadRelocation { segment } => {
                write!(f, "a relocation of 3DSX segment {segment} is out of range")
            }
        }
    }
}

impl std::error::Error for ThreeDsxError {}

/// One relocation table entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Relocation {
    pub skip: u16,
    pub patch: u16,
}

/// A parsed file. The segments borrow from it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ThreeDsx<'a> {
    /// Code, read-only data, and the initialised part of the data.
    pub segments: [&'a [u8]; SEGMENTS],
    /// Bytes of zero after the data.
    pub bss_len: u32,
    /// For each segment, its absolute and its relative relocations.
    pub relocations: [[Vec<Relocation>; 2]; SEGMENTS],
    pub smdh: Option<&'a [u8]>,
    /// Where the RomFS image starts in the file; it runs to the end.
    pub romfs_offset: Option<u32>,
}

/// A program placed in memory: what to map where.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Placed {
    /// The address of each segment and its contents; the data includes its
    /// zeroed tail. Every segment starts on a page.
    pub segments: [(u32, Vec<u8>); SEGMENTS],
    pub entry: u32,
}

fn page_up(len: u32) -> u32 {
    len.div_ceil(PAGE) * PAGE
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

impl<'a> ThreeDsx<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, ThreeDsxError> {
        let not = ThreeDsxError::NotThreeDsx;
        if data.get(..4) != Some(b"3DSX") {
            return Err(not);
        }
        let u16_at = |at: usize| {
            data.get(at..at + 2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]) as usize)
        };
        let header_len = u16_at(4).ok_or(not.clone())?;
        let reloc_header_len = u16_at(6).ok_or(not.clone())?;
        if header_len < 0x20 || reloc_header_len < 8 {
            return Err(not);
        }
        let word = |at: usize| u32_at(data, at).ok_or(ThreeDsxError::Truncated);
        let sizes = [word(0x10)?, word(0x14)?, word(0x18)?];
        let bss_len = word(0x1C)?;
        let data_in_file = sizes[2]
            .checked_sub(bss_len)
            .ok_or(ThreeDsxError::Truncated)?;
        let (smdh, romfs_offset) = if header_len >= 0x2C {
            let (offset, len) = (word(0x20)? as usize, word(0x24)? as usize);
            let smdh = (len != 0)
                .then(|| {
                    data.get(offset..offset + len)
                        .ok_or(ThreeDsxError::Truncated)
                })
                .transpose()?;
            (smdh, Some(word(0x28)?).filter(|&o| o != 0))
        } else {
            (None, None)
        };

        let mut counts = [[0usize; 2]; SEGMENTS];
        for (n, count) in counts.iter_mut().enumerate() {
            let at = header_len + n * reloc_header_len;
            *count = [word(at)? as usize, word(at + 4)? as usize];
        }
        let mut at = header_len + SEGMENTS * reloc_header_len;
        let mut take = |len: usize| {
            let part = data.get(at..at.checked_add(len)?)?;
            at += len;
            Some(part)
        };
        let lens = [sizes[0], sizes[1], data_in_file];
        let mut segments: [&[u8]; SEGMENTS] = [&[]; SEGMENTS];
        for (segment, len) in segments.iter_mut().zip(lens) {
            *segment = take(len as usize).ok_or(ThreeDsxError::Truncated)?;
        }
        let mut relocations: [[Vec<Relocation>; 2]; SEGMENTS] = Default::default();
        for (tables, count) in relocations.iter_mut().zip(counts) {
            for (table, count) in tables.iter_mut().zip(count) {
                let raw = take(count.checked_mul(4).ok_or(ThreeDsxError::Truncated)?)
                    .ok_or(ThreeDsxError::Truncated)?;
                *table = raw
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|r| Relocation {
                        skip: u16::from_le_bytes([r[0], r[1]]),
                        patch: u16::from_le_bytes([r[2], r[3]]),
                    })
                    .collect();
            }
        }
        Ok(ThreeDsx {
            segments,
            bss_len,
            relocations,
            smdh,
            romfs_offset,
        })
    }

    /// The bytes the program occupies once placed: each segment rounded up
    /// to a page.
    pub fn placed_len(&self) -> u32 {
        self.segment_lens().into_iter().map(page_up).sum()
    }

    fn segment_lens(&self) -> [u32; SEGMENTS] {
        [
            self.segments[0].len() as u32,
            self.segments[1].len() as u32,
            self.segments[2].len() as u32 + self.bss_len,
        ]
    }

    /// Place the program at `base` (a page address) and apply its
    /// relocations. The entry point is the start of the code.
    pub fn place(&self, base: u32) -> Result<Placed, ThreeDsxError> {
        let lens = self.segment_lens().map(page_up);
        // Where each segment was linked, and where it goes.
        let linked = [0, lens[0], lens[0] + lens[1]];
        let placed = linked.map(|at| base.wrapping_add(at));
        let translate = |addr: u32| -> Option<u32> {
            let segment = (0..SEGMENTS).rev().find(|&n| addr >= linked[n])?;
            (addr - linked[segment] <= lens[segment])
                .then(|| placed[segment] + addr - linked[segment])
        };

        let mut contents: [Vec<u8>; SEGMENTS] = [
            self.segments[0].to_vec(),
            self.segments[1].to_vec(),
            self.segments[2].to_vec(),
        ];
        contents[2].resize(self.segments[2].len() + self.bss_len as usize, 0);

        for (segment, tables) in self.relocations.iter().enumerate() {
            let bad = ThreeDsxError::BadRelocation { segment };
            for (relative, table) in tables.iter().enumerate() {
                // Each table walks its segment from the start.
                let mut at = 0usize;
                for relocation in table {
                    at += relocation.skip as usize * 4;
                    for _ in 0..relocation.patch {
                        let word = u32_at(&contents[segment], at).ok_or(bad.clone())?;
                        let target = translate(word & 0x0FFF_FFFF).ok_or(bad.clone())?;
                        let here = placed[segment] + at as u32;
                        let value = match (relative, word >> 28) {
                            (0, 0) => target,
                            (1, 0) => target.wrapping_sub(here),
                            (1, 1) => target.wrapping_sub(here) & 0x7FFF_FFFF,
                            // Kinds no tool emits leave the word alone.
                            _ => word,
                        };
                        contents[segment][at..at + 4].copy_from_slice(&value.to_le_bytes());
                        at += 4;
                    }
                }
            }
        }
        let [code, rodata, data] = contents;
        Ok(Placed {
            segments: [(placed[0], code), (placed[1], rodata), (placed[2], data)],
            entry: placed[0],
        })
    }
}

/// Build a 3DSX from its segments and relocation tables, for tests and tools.
/// `relocations[segment][0]` is the absolute table, `[1]` the relative one.
pub fn build(
    segments: [&[u8]; SEGMENTS],
    bss_len: u32,
    relocations: &[[Vec<Relocation>; 2]; SEGMENTS],
) -> Vec<u8> {
    let mut image = Vec::new();
    image.extend_from_slice(b"3DSX");
    image.extend_from_slice(&0x20u16.to_le_bytes());
    image.extend_from_slice(&8u16.to_le_bytes());
    image.extend_from_slice(&0u32.to_le_bytes());
    image.extend_from_slice(&0u32.to_le_bytes());
    let lens = [
        segments[0].len() as u32,
        segments[1].len() as u32,
        segments[2].len() as u32 + bss_len,
        bss_len,
    ];
    for len in lens {
        image.extend_from_slice(&len.to_le_bytes());
    }
    for tables in relocations {
        for table in tables {
            image.extend_from_slice(&(table.len() as u32).to_le_bytes());
        }
    }
    for segment in segments {
        image.extend_from_slice(segment);
    }
    for table in relocations.iter().flatten() {
        for relocation in table {
            image.extend_from_slice(&relocation.skip.to_le_bytes());
            image.extend_from_slice(&relocation.patch.to_le_bytes());
        }
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn segments_are_placed_on_pages_and_patched() {
        // Code: a word, then a pointer to data+4; one more word, then the
        // distance to rodata+0 in both relative kinds.
        let code = words(&[0xE1A0_0000, 0x2000 + 4, 0xE1A0_0000, 0x1000, 0x1000_1000]);
        let rodata = words(&[0x1234_5678, 0]);
        let data = words(&[1, 2]);
        let relocations = [
            [
                vec![Relocation { skip: 1, patch: 1 }],
                vec![Relocation { skip: 3, patch: 2 }],
            ],
            // Rodata's second word points at the code's third.
            [vec![Relocation { skip: 1, patch: 1 }], vec![]],
            [vec![], vec![]],
        ];
        let mut rodata = rodata;
        rodata[4..8].copy_from_slice(&8u32.to_le_bytes());
        let image = build([&code, &rodata, &data], 0x10, &relocations);

        let parsed = ThreeDsx::parse(&image).unwrap();
        assert_eq!(parsed.segments[0], &code[..]);
        assert_eq!(parsed.bss_len, 0x10);
        assert_eq!(parsed.placed_len(), 0x3000);
        assert_eq!((parsed.smdh, parsed.romfs_offset), (None, None));

        let placed = parsed.place(0x0010_0000).unwrap();
        assert_eq!(placed.entry, 0x0010_0000);
        let [(code_at, code), (rodata_at, rodata), (data_at, data)] = placed.segments;
        assert_eq!(
            (code_at, rodata_at, data_at),
            (0x10_0000, 0x10_1000, 0x10_2000)
        );
        assert_eq!(
            code,
            words(&[
                0xE1A0_0000,
                0x0010_2004,
                0xE1A0_0000,
                0x0010_1000 - 0x0010_000C,
                (0x0010_1000 - 0x0010_0010) & 0x7FFF_FFFF,
            ])
        );
        assert_eq!(rodata, words(&[0x1234_5678, 0x0010_0008]));
        assert_eq!(data.len(), 8 + 0x10, "the zeroed tail is part of the data");
        assert_eq!(data[8..], [0; 0x10]);
    }

    #[test]
    fn the_extended_header_finds_the_icon_and_the_romfs() {
        let mut image = build([&[0; 4], &[], &[]], 0, &Default::default());
        // Widen the header by twelve bytes and point them at a tail.
        image[4..6].copy_from_slice(&0x2Cu16.to_le_bytes());
        let tail = image.len() as u32 + 12;
        let extended: Vec<u8> = [tail, 4, tail + 4]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        image.splice(0x20..0x20, extended);
        image.extend_from_slice(b"SMDHIVFC");
        let parsed = ThreeDsx::parse(&image).unwrap();
        assert_eq!(parsed.smdh, Some(&b"SMDH"[..]));
        assert_eq!(parsed.romfs_offset, Some(tail + 4));
    }

    #[test]
    fn damaged_files_are_refused() {
        assert_eq!(ThreeDsx::parse(b"NOPE"), Err(ThreeDsxError::NotThreeDsx));
        let image = build([&[0; 8], &[], &[]], 0, &Default::default());
        assert_eq!(
            ThreeDsx::parse(&image[..image.len() - 1]),
            Err(ThreeDsxError::Truncated)
        );

        // A patch past the end of the segment, and one to nowhere.
        let past = [
            [vec![Relocation { skip: 2, patch: 1 }], vec![]],
            Default::default(),
            Default::default(),
        ];
        let image = build([&[0; 8], &[], &[]], 0, &past);
        assert_eq!(
            ThreeDsx::parse(&image).unwrap().place(0x10_0000),
            Err(ThreeDsxError::BadRelocation { segment: 0 })
        );
        let nowhere = [
            [vec![Relocation { skip: 0, patch: 1 }], vec![]],
            Default::default(),
            Default::default(),
        ];
        let image = build([&words(&[0x0FFF_FFFF]), &[], &[]], 0, &nowhere);
        assert_eq!(
            ThreeDsx::parse(&image).unwrap().place(0x10_0000),
            Err(ThreeDsxError::BadRelocation { segment: 0 })
        );
    }
}
