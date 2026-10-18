//! CRO and CRS modules (3dbrew, "CRO0"): the shared libraries of the 3DS
//! and the symbol table of the program that loads them.
//!
//! A CRO is a file the program reads from its RomFS and hands to `ldr:ro`
//! (`service::ldr_ro`), which maps it into the process and links it in
//! place: the module's own tables are rewritten from file offsets to
//! addresses, its pointers to itself are relocated, and its imports are
//! patched with what other modules export. A CRS has the same format and
//! describes the program itself (its segments are already where they
//! belong), so CROs can import from it and it can import from them.
//!
//! The file starts with four SHA-256 hashes (checked against the CRR by the
//! real module, not here), then a header at 0x80 of offset/count pairs, each
//! naming a table further on. A place in a module is a *segment tag*: the
//! segment's index in bits 0-3 and the offset into it above.
//!
//! Everything here works on the process's memory through [`Memory`], since
//! the module is linked where it was mapped and the program reads the
//! results there.

use crate::memory::AddressSpace;
use crate::result::ResultCode;
use ctr_core::bus::PhysMem;

/// The bytes a header takes, hashes included.
pub const HEADER_LEN: u32 = 0x138;
/// "CRO0", and "FIXD" once a module is fixed (its tail given back).
pub const MAGIC_CRO0: u32 = u32::from_le_bytes(*b"CRO0");
pub const MAGIC_FIXD: u32 = u32::from_le_bytes(*b"FIXD");
/// Header fields, as offsets from the start of the module.
pub mod field {
    pub const MAGIC: u32 = 0x80;
    pub const NAME: u32 = 0x84;
    pub const NEXT: u32 = 0x88;
    pub const PREVIOUS: u32 = 0x8C;
    pub const FILE_SIZE: u32 = 0x90;
    pub const BSS_SIZE: u32 = 0x94;
    pub const FIXED_SIZE: u32 = 0x98;
    pub const CONTROL_OBJECT: u32 = 0xA0;
    pub const ON_LOAD: u32 = 0xA4;
    pub const ON_EXIT: u32 = 0xA8;
    pub const ON_UNRESOLVED: u32 = 0xAC;
    // Tables: an offset, then a count (or a size, for byte strings).
    pub const CODE: u32 = 0xB0;
    pub const DATA: u32 = 0xB8;
    pub const MODULE_NAME: u32 = 0xC0;
    pub const SEGMENTS: u32 = 0xC8;
    pub const EXPORT_NAMED: u32 = 0xD0;
    pub const EXPORT_INDEXED: u32 = 0xD8;
    pub const EXPORT_STRINGS: u32 = 0xE0;
    pub const EXPORT_TREE: u32 = 0xE8;
    pub const IMPORT_MODULES: u32 = 0xF0;
    pub const IMPORT_RELOCATIONS: u32 = 0xF8;
    pub const IMPORT_NAMED: u32 = 0x100;
    pub const IMPORT_INDEXED: u32 = 0x108;
    pub const IMPORT_ANONYMOUS: u32 = 0x110;
    pub const IMPORT_STRINGS: u32 = 0x118;
    pub const STATIC_ANONYMOUS: u32 = 0x120;
    pub const INTERNAL_RELOCATIONS: u32 = 0x128;
    pub const STATIC_RELOCATIONS: u32 = 0x130;
}

/// Every table in header order, with the size of its entries (1 for byte
/// strings and code).
pub const TABLES: [(u32, u32); 17] = [
    (field::CODE, 1),
    (field::DATA, 1),
    (field::MODULE_NAME, 1),
    (field::SEGMENTS, 12),
    (field::EXPORT_NAMED, 8),
    (field::EXPORT_INDEXED, 4),
    (field::EXPORT_STRINGS, 1),
    (field::EXPORT_TREE, 8),
    (field::IMPORT_MODULES, 20),
    (field::IMPORT_RELOCATIONS, 12),
    (field::IMPORT_NAMED, 8),
    (field::IMPORT_INDEXED, 8),
    (field::IMPORT_ANONYMOUS, 8),
    (field::IMPORT_STRINGS, 1),
    (field::STATIC_ANONYMOUS, 8),
    (field::INTERNAL_RELOCATIONS, 12),
    (field::STATIC_RELOCATIONS, 12),
];

/// The order the tables lie in the file, which the loader insists on; the
/// data the program copies out comes last, then the end of the file.
pub const FILE_ORDER: [u32; 18] = [
    field::CODE,
    field::MODULE_NAME,
    field::SEGMENTS,
    field::EXPORT_NAMED,
    field::EXPORT_TREE,
    field::EXPORT_INDEXED,
    field::EXPORT_STRINGS,
    field::IMPORT_MODULES,
    field::IMPORT_RELOCATIONS,
    field::IMPORT_NAMED,
    field::IMPORT_INDEXED,
    field::IMPORT_ANONYMOUS,
    field::IMPORT_STRINGS,
    field::STATIC_ANONYMOUS,
    field::INTERNAL_RELOCATIONS,
    field::STATIC_RELOCATIONS,
    field::DATA,
    field::FILE_SIZE,
];

/// Segment types, which the loader also takes as their indices.
pub mod segment {
    pub const TEXT: u32 = 0;
    pub const RODATA: u32 = 1;
    pub const DATA: u32 = 2;
    pub const BSS: u32 = 3;
}

/// Relocation types: ELF's numbers for ARM (3dbrew, "CRO0").
pub mod reloc {
    pub const NONE: u8 = 0;
    pub const ABS32: u8 = 2;
    pub const REL32: u8 = 3;
    pub const THM_PC22: u8 = 10;
    pub const CALL: u8 = 28;
    pub const JUMP24: u8 = 29;
    pub const TARGET1: u8 = 38;
    pub const PREL31: u8 = 42;
}

/// The process's memory as the loader sees it: the kernel's view, with no
/// permission checks. What is not mapped reads as zero and ignores writes.
pub trait Memory {
    fn read8(&self, va: u32) -> u8;
    fn write8(&mut self, va: u32, value: u8);

    fn read32(&self, va: u32) -> u32 {
        u32::from_le_bytes([0, 1, 2, 3].map(|n| self.read8(va.wrapping_add(n))))
    }

    fn write32(&mut self, va: u32, value: u32) {
        for (n, byte) in value.to_le_bytes().into_iter().enumerate() {
            self.write8(va.wrapping_add(n as u32), byte);
        }
    }

    /// A string of at most `max` bytes, ending at its first zero.
    fn cstring(&self, va: u32, max: u32) -> Vec<u8> {
        (0..max)
            .map(|n| self.read8(va.wrapping_add(n)))
            .take_while(|&b| b != 0)
            .collect()
    }
}

/// A process's memory: its pages over physical memory.
pub struct Guest<'a> {
    pub space: &'a AddressSpace,
    pub mem: &'a mut PhysMem,
}

impl Memory for Guest<'_> {
    fn read8(&self, va: u32) -> u8 {
        self.space
            .read_bytes(self.mem, va, 1)
            .map_or(0, |bytes| bytes[0])
    }

    fn write8(&mut self, va: u32, value: u8) {
        self.space.write_bytes(self.mem, va, &[value]);
    }

    fn read32(&self, va: u32) -> u32 {
        self.space.read32(self.mem, va).unwrap_or(0)
    }

    fn write32(&mut self, va: u32, value: u32) {
        self.space.write32(self.mem, va, value);
    }
}

/// The segment's index and the offset into it.
pub fn split_tag(tag: u32) -> (u32, u32) {
    (tag & 0xF, tag >> 4)
}

pub fn tag(segment: u32, offset: u32) -> u32 {
    offset << 4 | segment & 0xF
}

/// A module mapped at an address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Module(pub u32);

impl Module {
    pub fn get(self, m: &dyn Memory, field: u32) -> u32 {
        m.read32(self.0 + field)
    }

    pub fn set(self, m: &mut dyn Memory, field: u32, value: u32) {
        m.write32(self.0 + field, value);
    }

    /// Where entry `index` of the table at `field` lies, once the header
    /// holds addresses.
    fn entry(self, m: &dyn Memory, field: u32, size: u32, index: u32) -> u32 {
        self.get(m, field) + index * size
    }

    /// The module's name, from its module name table.
    pub fn name(self, m: &dyn Memory) -> String {
        let at = self.get(m, field::MODULE_NAME);
        let len = self.get(m, field::MODULE_NAME + 4);
        String::from_utf8_lossy(&m.cstring(at, len)).into_owned()
    }

    /// Segment `index`: its address (or offset, before rebasing), size and
    /// type.
    pub fn segment(self, m: &dyn Memory, index: u32) -> (u32, u32, u32) {
        let at = self.entry(m, field::SEGMENTS, 12, index);
        (m.read32(at), m.read32(at + 4), m.read32(at + 8))
    }

    /// The address a segment tag names, if its segment has that much.
    pub fn address(self, m: &dyn Memory, tag: u32) -> Option<u32> {
        let (index, offset) = split_tag(tag);
        if index >= self.get(m, field::SEGMENTS + 4) {
            return None;
        }
        let (start, size, _) = self.segment(m, index);
        (offset < size).then_some(start.wrapping_add(offset))
    }

    /// The address of the export called `name`, found through the export
    /// tree: a node tests bit `test & 7` of byte `test >> 3` of the name
    /// (bytes past its end read as zero) and goes right on a one; a child
    /// marked as an end names the export, which must then match in full.
    pub fn find_export(self, m: &dyn Memory, name: &[u8]) -> Option<u32> {
        let nodes = self.get(m, field::EXPORT_TREE + 4);
        if nodes == 0 {
            return None;
        }
        let node = |index: u32| self.entry(m, field::EXPORT_TREE, 8, index);
        // The root is a dummy whose left child is the real tree.
        let mut next = m.read32(node(0) + 2) as u16;
        let mut steps = 0;
        let found = loop {
            let at = node(u32::from(next & 0x7FFF) % nodes);
            if next & 0x8000 != 0 {
                break m.read32(at + 6) as u16;
            }
            let test = m.read32(at) as u16;
            let byte = name.get(usize::from(test >> 3)).copied().unwrap_or(0);
            next = m.read32(at + if byte >> (test & 7) & 1 != 0 { 4 } else { 2 }) as u16;
            steps += 1;
            if steps > nodes {
                return None;
            }
        };
        let count = self.get(m, field::EXPORT_NAMED + 4);
        if u32::from(found) >= count {
            return None;
        }
        let entry = self.entry(m, field::EXPORT_NAMED, 8, u32::from(found));
        let strings = self.get(m, field::EXPORT_STRINGS + 4);
        (m.cstring(m.read32(entry), strings) == name)
            .then(|| self.address(m, m.read32(entry + 4)))
            .flatten()
    }
}

/// A malformed module: RO's codes (module 75) at the permanent level,
/// "wrong argument", with a description saying which check failed
/// (0xD9012C22 and 0xD9012C23 are 3dbrew's; the others follow the same
/// pattern, as other emulators report them).
pub const fn format_error(description: u32) -> ResultCode {
    ResultCode(0xD901_2C00 | description)
}

/// The data or bss buffer a program gives is smaller than the segment.
pub const BUFFER_TOO_SMALL: ResultCode = ResultCode(0xE0E1_2C1F);
const BAD_HEADER: ResultCode = format_error(0x11);
const BAD_RELOCATION: ResultCode = format_error(0x12);
const BAD_INDEXED_IMPORT: ResultCode = format_error(0x14);
const BAD_INTERNAL_RELOCATION: ResultCode = format_error(0x15);
const BAD_STATIC_ANONYMOUS: ResultCode = format_error(0x16);
const BAD_ANONYMOUS_IMPORT: ResultCode = format_error(0x17);
const BAD_IMPORT_MODULE: ResultCode = format_error(0x18);
const BAD_SEGMENT: ResultCode = format_error(0x19);
const BAD_NAMED_IMPORT: ResultCode = format_error(0x1B);
const UNTERMINATED_STRING: ResultCode = format_error(0x0B);
const NO_SYMBOL: ResultCode = format_error(0x10);
const UNKNOWN_RELOCATION: ResultCode = format_error(0x22);
const OUT_OF_RANGE: ResultCode = format_error(0x23);

/// Write relocation `kind` at `target` for the symbol at `symbol` plus
/// `addend`, as 3dbrew gives RO's code. `place` is where the word is when
/// the program runs: `target` itself, except for data relocated in the
/// image before the program copies it to its own buffer.
pub fn relocate(
    m: &mut dyn Memory,
    target: u32,
    kind: u8,
    addend: u32,
    symbol: u32,
    place: u32,
) -> Result<(), ResultCode> {
    let offset = symbol.wrapping_add(addend).wrapping_sub(place);
    let distance = place.wrapping_sub(symbol) as i32;
    let value = match kind {
        reloc::NONE => return Ok(()),
        reloc::ABS32 | reloc::TARGET1 => symbol.wrapping_add(addend),
        reloc::REL32 => offset,
        reloc::THM_PC22 => {
            if !(-0x40_0000 + 1..0x40_0000).contains(&distance) {
                return Err(OUT_OF_RANGE);
            }
            // A BL pair to Thumb code, or a BLX pair to ARM code, whose
            // target must then be a word away from the instruction.
            let (offset, second) = if symbol & 1 != 0 {
                (offset, 0xF800)
            } else {
                (offset.wrapping_add(offset & 2), 0xE800)
            };
            let high = 0xF000 | (offset >> 12 & 0x7FF);
            let low = second | (offset >> 1 & 0x7FF);
            low << 16 | high
        }
        reloc::CALL => {
            if !(-0x200_0000 + 1..0x200_0000).contains(&distance) {
                return Err(OUT_OF_RANGE);
            }
            if symbol & 1 != 0 {
                // BLX to Thumb code: the half-word bit goes into H.
                0xFA00_0000 | (offset & 2) << 23 | (offset >> 2 & 0xFF_FFFF)
            } else {
                0xEB00_0000 | (offset >> 2 & 0xFF_FFFF)
            }
        }
        reloc::JUMP24 => {
            if !(-0x200_0000 + 1..0x200_0000).contains(&distance) || symbol & 1 != 0 {
                return Err(OUT_OF_RANGE);
            }
            // The condition and opcode stay.
            m.read32(target) & 0xFF00_0000 | (offset >> 2 & 0xFF_FFFF)
        }
        reloc::PREL31 => addend
            .wrapping_add(symbol & 0x7FFF_FFFF)
            .wrapping_sub(place),
        _ => return Err(UNKNOWN_RELOCATION),
    };
    m.write32(target, value);
    Ok(())
}

/// Undo a relocation: the word is zero again, as in the file, except that a
/// branch keeps its condition and opcode.
pub fn unrelocate(m: &mut dyn Memory, target: u32, kind: u8) -> Result<(), ResultCode> {
    let value = match kind {
        reloc::NONE => return Ok(()),
        reloc::JUMP24 => m.read32(target) & 0xFF00_0000,
        reloc::ABS32
        | reloc::TARGET1
        | reloc::REL32
        | reloc::THM_PC22
        | reloc::CALL
        | reloc::PREL31 => 0,
        _ => return Err(UNKNOWN_RELOCATION),
    };
    m.write32(target, value);
    Ok(())
}

/// One entry of a relocation table: target tag, type, its fifth and sixth
/// bytes (the end of a batch and whether it is resolved, for imports; the
/// symbol's segment, for internal relocations) and the addend.
#[derive(Clone, Copy, Debug)]
struct Entry {
    target: u32,
    kind: u8,
    byte5: u8,
    addend: u32,
}

fn read_entry(m: &dyn Memory, at: u32) -> Entry {
    Entry {
        target: m.read32(at),
        kind: m.read8(at + 4),
        byte5: m.read8(at + 5),
        addend: m.read32(at + 8),
    }
}

/// Where a batch of import relocations records that it is resolved.
const RESOLVED: u32 = 6;

/// What a link step does to a batch of imports.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Batch {
    /// Point it at a symbol.
    Resolve,
    /// Point it at the module's "unresolved" handler again.
    Reset,
}

impl Module {
    fn count(self, m: &dyn Memory, table: u32) -> u32 {
        self.get(m, table + 4)
    }

    /// The tables a module's header names, as (offset field, entry size).
    fn tables() -> impl Iterator<Item = (u32, u32)> {
        TABLES.into_iter()
    }

    /// The address of the function to call for an import nobody resolved,
    /// or zero.
    fn on_unresolved(self, m: &dyn Memory) -> u32 {
        self.address(m, self.get(m, field::ON_UNRESOLVED))
            .unwrap_or(0)
    }

    /// Whether a module is mapped here: it carries either magic.
    pub fn is_loaded(self, m: &dyn Memory) -> bool {
        matches!(self.get(m, field::MAGIC), MAGIC_CRO0 | MAGIC_FIXD)
    }

    pub fn is_fixed(self, m: &dyn Memory) -> bool {
        self.get(m, field::MAGIC) == MAGIC_FIXD
    }

    /// The pages of its text segment, which become executable.
    pub fn executable_pages(self, m: &dyn Memory) -> Option<(u32, u32)> {
        (0..self.count(m, field::SEGMENTS))
            .map(|n| self.segment(m, n))
            .find(|&(_, size, kind)| kind == segment::TEXT && size != 0)
            .map(|(start, size, _)| {
                let first = start & !0xFFF;
                let end = (start + size).next_multiple_of(0x1000);
                (first, end - first)
            })
    }

    /// The address a tag names in the image as loaded: the same as
    /// `address`, except that data is still where the file had it, before
    /// the program copies it out to its buffer.
    fn image_address(self, m: &dyn Memory, tag: u32) -> Option<u32> {
        let place = self.address(m, tag)?;
        let (index, offset) = split_tag(tag);
        Some(if self.segment(m, index).2 == segment::DATA {
            self.get(m, field::DATA) + offset
        } else {
            place
        })
    }

    // ---- rebasing ------------------------------------------------------

    /// Turn the header's offsets into addresses, after checking that the
    /// module is fresh, its tables lie in order and within `size` bytes.
    fn rebase_header(self, m: &mut dyn Memory, size: u32) -> Result<(), ResultCode> {
        let fresh = self.get(m, field::MAGIC) == MAGIC_CRO0
            && self.get(m, field::NEXT) == 0
            && self.get(m, field::PREVIOUS) == 0
            && self.get(m, field::FILE_SIZE) <= 0x1000_0000
            && self.get(m, field::BSS_SIZE) <= 0x1000_0000
            && self.get(m, field::FIXED_SIZE) == 0
            && self.get(m, field::CODE) >= HEADER_LEN;
        let ordered = FILE_ORDER
            .windows(2)
            .all(|pair| self.get(m, pair[0]) <= self.get(m, pair[1]));
        if !fresh || !ordered {
            return Err(BAD_HEADER);
        }
        let name = self.get(m, field::NAME);
        if name != 0 {
            self.set(m, field::NAME, name + self.0);
        }
        for (table, _) in Module::tables() {
            let offset = self.get(m, table);
            if offset != 0 {
                self.set(m, table, offset + self.0);
            }
        }
        let end = self.0 + size;
        for (table, entry) in Module::tables() {
            let last = self.get(m, table) as u64 + self.count(m, table) as u64 * entry as u64;
            if last > end as u64 {
                return Err(BAD_HEADER);
            }
        }
        Ok(())
    }

    /// A string table must end in a zero, if it has anything.
    fn check_strings(self, m: &dyn Memory, table: u32) -> Result<(), ResultCode> {
        let len = self.count(m, table);
        if len != 0 && m.read8(self.get(m, table) + len - 1) != 0 {
            return Err(UNTERMINATED_STRING);
        }
        Ok(())
    }

    /// Segments become addresses: text and read-only data where the module
    /// lies, data and bss in the buffers the program gave. Returns where
    /// the data lies in the image.
    fn rebase_segments(
        self,
        m: &mut dyn Memory,
        size: u32,
        data: (u32, u32),
        bss: (u32, u32),
    ) -> Result<u32, ResultCode> {
        let mut image_data = 0;
        for n in 0..self.count(m, field::SEGMENTS) {
            let at = self.entry(m, field::SEGMENTS, 12, n);
            let (offset, len, kind) = self.segment(m, n);
            let rebased = match kind {
                segment::DATA | segment::BSS if len == 0 => continue,
                segment::DATA if len > data.1 => return Err(BUFFER_TOO_SMALL),
                segment::DATA => {
                    image_data = offset;
                    data.0
                }
                segment::BSS if len > bss.1 => return Err(BUFFER_TOO_SMALL),
                segment::BSS => bss.0,
                _ if offset == 0 => continue,
                _ if offset > size => return Err(BAD_SEGMENT),
                _ => offset + self.0,
            };
            m.write32(at, rebased);
        }
        Ok(image_data + self.0)
    }

    /// Add the module's address to the offsets in a table's entries:
    /// `fields` gives each offset's place in the entry and the range it
    /// must then fall in (start field, entry size of that table), if any.
    fn rebase_entries(
        self,
        m: &mut dyn Memory,
        table: u32,
        entry: u32,
        fields: &[(u32, u32, u32)],
        error: ResultCode,
    ) -> Result<(), ResultCode> {
        let ranges: Vec<(u32, u32)> = fields
            .iter()
            .map(|&(_, within, size)| {
                let start = self.get(m, within);
                (start, start + self.count(m, within) * size)
            })
            .collect();
        for n in 0..self.count(m, table) {
            let at = self.entry(m, table, entry, n);
            for (&(place, _, size), &(start, end)) in fields.iter().zip(&ranges) {
                let offset = m.read32(at + place);
                if offset == 0 {
                    continue;
                }
                let address = offset + self.0;
                // A string must start inside its table; a sub-table may
                // also start at the end of its table, when it is empty.
                let inside = if size == 1 {
                    (start..end).contains(&address)
                } else {
                    (start..=end).contains(&address)
                };
                if !inside {
                    return Err(error);
                }
                m.write32(at + place, address);
            }
        }
        Ok(())
    }

    fn unrebase_entries(self, m: &mut dyn Memory, table: u32, entry: u32, places: &[u32]) {
        for n in 0..self.count(m, table) {
            let at = self.entry(m, table, entry, n);
            for &place in places {
                let address = m.read32(at + place);
                if address != 0 {
                    m.write32(at + place, address - self.0);
                }
            }
        }
    }

    /// Every import relocation points at the "unresolved" handler, and no
    /// batch is resolved.
    fn reset_import_relocations(self, m: &mut dyn Memory) -> Result<(), ResultCode> {
        let count = self.count(m, field::IMPORT_RELOCATIONS);
        let last = self.entry(m, field::IMPORT_RELOCATIONS, 12, count.wrapping_sub(1));
        if count != 0 && m.read8(last + 5) == 0 {
            return Err(BAD_RELOCATION);
        }
        let unresolved = self.on_unresolved(m);
        let mut batch_start = true;
        for n in 0..count {
            let at = self.entry(m, field::IMPORT_RELOCATIONS, 12, n);
            let entry = read_entry(m, at);
            let target = self.address(m, entry.target).ok_or(BAD_RELOCATION)?;
            relocate(m, target, entry.kind, entry.addend, unresolved, target)?;
            if batch_start {
                m.write8(at + RESOLVED, 0);
            }
            batch_start = entry.byte5 != 0;
        }
        Ok(())
    }

    /// Apply a batch of this module's import relocations, which lies at
    /// `batch`, for the symbol at `symbol`, and mark it resolved or not.
    fn apply_batch(
        self,
        m: &mut dyn Memory,
        batch: u32,
        symbol: u32,
        what: Batch,
    ) -> Result<(), ResultCode> {
        if symbol == 0 && what == Batch::Resolve {
            return Err(NO_SYMBOL);
        }
        let mut at = batch;
        loop {
            let entry = read_entry(m, at);
            let target = self.address(m, entry.target).ok_or(BAD_RELOCATION)?;
            relocate(m, target, entry.kind, entry.addend, symbol, target)?;
            if entry.byte5 != 0 {
                break;
            }
            at += 12;
        }
        m.write8(batch + RESOLVED, (what == Batch::Resolve) as u8);
        Ok(())
    }

    fn batch_resolved(m: &dyn Memory, batch: u32) -> bool {
        m.read8(batch + RESOLVED) != 0
    }

    /// The symbols this module gives the program itself, through
    /// relocations of the CRS's segments.
    fn export_static_anonymous(self, m: &mut dyn Memory, crs: Module) -> Result<(), ResultCode> {
        let start = self.get(m, field::STATIC_RELOCATIONS);
        let end = start + self.count(m, field::STATIC_RELOCATIONS) * 12;
        for n in 0..self.count(m, field::STATIC_ANONYMOUS) {
            let at = self.entry(m, field::STATIC_ANONYMOUS, 8, n);
            let batch = m.read32(at + 4) + self.0;
            if !(start..=end).contains(&batch) {
                return Err(BAD_STATIC_ANONYMOUS);
            }
            let symbol = self.address(m, m.read32(at)).unwrap_or(0);
            crs.apply_batch(m, batch, symbol, Batch::Resolve)?;
        }
        Ok(())
    }

    /// Relocate the module's references to itself. Those in data are made
    /// in the image, for where the data will be once copied.
    fn relocate_internal(self, m: &mut dyn Memory, image_data: u32) -> Result<(), ResultCode> {
        let segments = self.count(m, field::SEGMENTS);
        for n in 0..self.count(m, field::INTERNAL_RELOCATIONS) {
            let entry = read_entry(m, self.entry(m, field::INTERNAL_RELOCATIONS, 12, n));
            let place = self
                .address(m, entry.target)
                .ok_or(BAD_INTERNAL_RELOCATION)?;
            let (index, offset) = split_tag(entry.target);
            let target = if self.segment(m, index).2 == segment::DATA {
                image_data + offset
            } else {
                place
            };
            if u32::from(entry.byte5) >= segments {
                return Err(BAD_INTERNAL_RELOCATION);
            }
            let symbol = self.segment(m, u32::from(entry.byte5)).0;
            relocate(m, target, entry.kind, entry.addend, symbol, place)?;
        }
        Ok(())
    }

    /// The modules that link with every other, the program's first: the
    /// CRS, then the chain from its `NEXT`.
    fn auto_link(m: &dyn Memory, crs: Module) -> Vec<Module> {
        let mut list = vec![crs];
        let mut next = crs.get(m, field::NEXT);
        while next != 0 && list.len() < 0x1000 {
            list.push(Module(next));
            next = Module(next).get(m, field::NEXT);
        }
        list
    }

    /// `__aeabi_atexit` comes from whichever module exports
    /// `nnroAeabiAtexit_` (3dbrew, "CRO0").
    fn import_atexit(self, m: &mut dyn Memory, crs: Module) -> Result<(), ResultCode> {
        let strings = self.count(m, field::IMPORT_STRINGS);
        for n in 0..self.count(m, field::IMPORT_NAMED) {
            let at = self.entry(m, field::IMPORT_NAMED, 8, n);
            if m.cstring(m.read32(at), strings) != b"__aeabi_atexit" {
                continue;
            }
            let batch = m.read32(at + 4);
            let found = Module::auto_link(m, crs)
                .into_iter()
                .find_map(|source| source.find_export(m, b"nnroAeabiAtexit_"));
            if let Some(symbol) = found {
                self.apply_batch(m, batch, symbol, Batch::Resolve)?;
            }
        }
        Ok(())
    }

    /// Make a module mapped here usable: its tables become addresses, its
    /// segments are placed (text and read-only data where it lies, data and
    /// bss in the program's buffers), its imports point at its handler for
    /// unresolved symbols, and its references to itself are relocated. A
    /// CRS (`crs` is then unused) keeps its segments, which describe the
    /// program where it already is.
    pub fn rebase(
        self,
        m: &mut dyn Memory,
        crs: Module,
        size: u32,
        data: (u32, u32),
        bss: (u32, u32),
        is_crs: bool,
    ) -> Result<(), ResultCode> {
        self.rebase_header(m, size)?;
        self.check_strings(m, field::MODULE_NAME)?;
        let image_data = if is_crs {
            0
        } else {
            self.rebase_segments(m, size, data, bss)?
        };
        self.rebase_entries(
            m,
            field::EXPORT_NAMED,
            8,
            &[(0, field::EXPORT_STRINGS, 1)],
            BAD_HEADER,
        )?;
        let nodes = self.count(m, field::EXPORT_TREE);
        for n in 0..nodes {
            let at = self.entry(m, field::EXPORT_TREE, 8, n);
            let children = [m.read32(at + 2) as u16, m.read32(at + 4) as u16];
            if children.iter().any(|&c| u32::from(c & 0x7FFF) >= nodes) {
                return Err(BAD_HEADER);
            }
        }
        self.check_strings(m, field::EXPORT_STRINGS)?;
        self.rebase_entries(
            m,
            field::IMPORT_MODULES,
            20,
            &[
                (0, field::IMPORT_STRINGS, 1),
                (4, field::IMPORT_INDEXED, 8),
                (12, field::IMPORT_ANONYMOUS, 8),
            ],
            BAD_IMPORT_MODULE,
        )?;
        self.reset_import_relocations(m)?;
        let relocations = (4, field::IMPORT_RELOCATIONS, 12);
        self.rebase_entries(
            m,
            field::IMPORT_NAMED,
            8,
            &[(0, field::IMPORT_STRINGS, 1), relocations],
            BAD_NAMED_IMPORT,
        )?;
        self.rebase_entries(
            m,
            field::IMPORT_INDEXED,
            8,
            &[relocations],
            BAD_INDEXED_IMPORT,
        )?;
        self.rebase_entries(
            m,
            field::IMPORT_ANONYMOUS,
            8,
            &[relocations],
            BAD_ANONYMOUS_IMPORT,
        )?;
        self.check_strings(m, field::IMPORT_STRINGS)?;
        if !is_crs {
            self.export_static_anonymous(m, crs)?;
        }
        self.relocate_internal(m, image_data)?;
        if !is_crs {
            self.import_atexit(m, crs)?;
        }
        Ok(())
    }

    /// Undo `rebase`'s address arithmetic: the tables hold offsets again
    /// and the module is in no list. The data segment gets back its offset
    /// in the file.
    pub fn unrebase(self, m: &mut dyn Memory, is_crs: bool) {
        self.unrebase_entries(m, field::IMPORT_ANONYMOUS, 8, &[4]);
        self.unrebase_entries(m, field::IMPORT_INDEXED, 8, &[4]);
        self.unrebase_entries(m, field::IMPORT_NAMED, 8, &[0, 4]);
        self.unrebase_entries(m, field::IMPORT_MODULES, 20, &[0, 4, 12]);
        self.unrebase_entries(m, field::EXPORT_NAMED, 8, &[0]);
        if !is_crs {
            let image_data = self.get(m, field::DATA);
            for n in 0..self.count(m, field::SEGMENTS) {
                let at = self.entry(m, field::SEGMENTS, 12, n);
                let (address, len, kind) = self.segment(m, n);
                let offset = match kind {
                    segment::DATA | segment::BSS if len == 0 => continue,
                    segment::DATA => image_data.wrapping_sub(self.0),
                    segment::BSS => 0,
                    _ if address == 0 => continue,
                    _ => address - self.0,
                };
                m.write32(at, offset);
            }
        }
        self.set(m, field::NEXT, 0);
        self.set(m, field::PREVIOUS, 0);
        self.set(m, field::FIXED_SIZE, 0);
        let name = self.get(m, field::NAME);
        if name != 0 {
            self.set(m, field::NAME, name - self.0);
        }
        for (table, _) in Module::tables() {
            let address = self.get(m, table);
            if address != 0 {
                self.set(m, table, address - self.0);
            }
        }
    }

    // ---- linking -------------------------------------------------------

    /// Resolve this module's named imports that are not yet, from the
    /// auto-link modules, the first that exports each.
    fn import_named(self, m: &mut dyn Memory, crs: Module) -> Result<(), ResultCode> {
        let strings = self.count(m, field::IMPORT_STRINGS);
        let sources = Module::auto_link(m, crs);
        for n in 0..self.count(m, field::IMPORT_NAMED) {
            let at = self.entry(m, field::IMPORT_NAMED, 8, n);
            let batch = m.read32(at + 4);
            if Module::batch_resolved(m, batch) {
                continue;
            }
            let name = m.cstring(m.read32(at), strings);
            if let Some(symbol) = sources.iter().find_map(|s| s.find_export(m, &name)) {
                self.apply_batch(m, batch, symbol, Batch::Resolve)?;
            }
        }
        Ok(())
    }

    /// The indexed and anonymous imports `importer` makes from `self`:
    /// each one's batch in the importer and the symbol in `self`.
    fn module_imports(self, m: &dyn Memory, importer: Module) -> Vec<(u32, u32)> {
        let name = self.name(m).into_bytes();
        let strings = importer.count(m, field::IMPORT_STRINGS);
        let mut found = Vec::new();
        for n in 0..importer.count(m, field::IMPORT_MODULES) {
            let at = importer.entry(m, field::IMPORT_MODULES, 20, n);
            if m.cstring(m.read32(at), strings) != name {
                continue;
            }
            let (indexed, indexed_count) = (m.read32(at + 4), m.read32(at + 8));
            for k in 0..indexed_count {
                let entry = indexed + k * 8;
                let index = m.read32(entry);
                let export = self.entry(m, field::EXPORT_INDEXED, 4, index);
                let symbol = self.address(m, m.read32(export)).unwrap_or(0);
                found.push((m.read32(entry + 4), symbol));
            }
            let (anonymous, anonymous_count) = (m.read32(at + 12), m.read32(at + 16));
            for k in 0..anonymous_count {
                let entry = anonymous + k * 8;
                let symbol = self.address(m, m.read32(entry)).unwrap_or(0);
                found.push((m.read32(entry + 4), symbol));
            }
        }
        found
    }

    /// Resolve this module's imports from the auto-link modules, then the
    /// imports the auto-link modules make from it. `fix_data` is the
    /// correction of `LoadCRO_New`: while loading, imports into data are
    /// made in the image, which the program then copies, rather than in its
    /// buffer, which the copy would overwrite.
    pub fn link(self, m: &mut dyn Memory, crs: Module, fix_data: bool) -> Result<(), ResultCode> {
        let moved = fix_data && self.count(m, field::SEGMENTS) > segment::DATA;
        let buffer = self.segment(m, segment::DATA).0;
        let at = self.entry(m, field::SEGMENTS, 12, segment::DATA);
        if moved {
            let image = self.get(m, field::DATA);
            m.write32(at, image);
        }
        let imported = self.import_named(m, crs).and_then(|()| {
            for source in Module::auto_link(m, crs) {
                for (batch, symbol) in source.module_imports(m, self) {
                    self.apply_batch(m, batch, symbol, Batch::Resolve)?;
                }
            }
            Ok(())
        });
        if moved {
            m.write32(at, buffer);
        }
        imported?;
        for target in Module::auto_link(m, crs) {
            self.export_named_to(m, target, Batch::Resolve)?;
            for (batch, symbol) in self.module_imports(m, target) {
                target.apply_batch(m, batch, symbol, Batch::Resolve)?;
            }
        }
        Ok(())
    }

    /// Resolve (or reset) the named imports of `target` that this module
    /// exports.
    fn export_named_to(
        self,
        m: &mut dyn Memory,
        target: Module,
        what: Batch,
    ) -> Result<(), ResultCode> {
        let strings = target.count(m, field::IMPORT_STRINGS);
        let unresolved = target.on_unresolved(m);
        for n in 0..target.count(m, field::IMPORT_NAMED) {
            let at = target.entry(m, field::IMPORT_NAMED, 8, n);
            let batch = m.read32(at + 4);
            if Module::batch_resolved(m, batch) == (what == Batch::Resolve) {
                continue;
            }
            let name = m.cstring(m.read32(at), strings);
            if let Some(symbol) = self.find_export(m, &name) {
                let symbol = if what == Batch::Resolve {
                    symbol
                } else {
                    unresolved
                };
                target.apply_batch(m, batch, symbol, what)?;
            }
        }
        Ok(())
    }

    /// Point every import of this module at its handler for unresolved
    /// symbols again, and every import the auto-link modules make from it
    /// at theirs.
    pub fn unlink(self, m: &mut dyn Memory, crs: Module) -> Result<(), ResultCode> {
        let unresolved = self.on_unresolved(m);
        for (table, place) in [
            (field::IMPORT_NAMED, 4),
            (field::IMPORT_INDEXED, 4),
            (field::IMPORT_ANONYMOUS, 4),
        ] {
            for n in 0..self.count(m, table) {
                let batch = m.read32(self.entry(m, table, 8, n) + place);
                self.apply_batch(m, batch, unresolved, Batch::Reset)?;
            }
        }
        for target in Module::auto_link(m, crs) {
            self.export_named_to(m, target, Batch::Reset)?;
            let unresolved = target.on_unresolved(m);
            for (batch, _) in self.module_imports(m, target) {
                target.apply_batch(m, batch, unresolved, Batch::Reset)?;
            }
        }
        Ok(())
    }

    /// Zero every relocated word, internal and imported, in the image, so
    /// that an unfixed module returns to the bytes it was loaded from.
    pub fn clear_relocations(self, m: &mut dyn Memory) -> Result<(), ResultCode> {
        let mut batch_start = true;
        for n in 0..self.count(m, field::IMPORT_RELOCATIONS) {
            let at = self.entry(m, field::IMPORT_RELOCATIONS, 12, n);
            let entry = read_entry(m, at);
            let target = self.image_address(m, entry.target).ok_or(BAD_RELOCATION)?;
            unrelocate(m, target, entry.kind)?;
            if batch_start {
                m.write8(at + RESOLVED, 0);
            }
            batch_start = entry.byte5 != 0;
        }
        for n in 0..self.count(m, field::INTERNAL_RELOCATIONS) {
            let entry = read_entry(m, self.entry(m, field::INTERNAL_RELOCATIONS, 12, n));
            let target = self
                .image_address(m, entry.target)
                .ok_or(BAD_INTERNAL_RELOCATION)?;
            unrelocate(m, target, entry.kind)?;
        }
        Ok(())
    }

    /// The batches of imports not resolved: the names of the named ones,
    /// and how many indexed and anonymous ones.
    pub fn unresolved(self, m: &dyn Memory) -> (Vec<String>, u32) {
        let strings = self.count(m, field::IMPORT_STRINGS);
        let named = (0..self.count(m, field::IMPORT_NAMED))
            .map(|n| self.entry(m, field::IMPORT_NAMED, 8, n))
            .filter(|&at| !Module::batch_resolved(m, m.read32(at + 4)))
            .map(|at| String::from_utf8_lossy(&m.cstring(m.read32(at), strings)).into_owned())
            .collect();
        let others = [field::IMPORT_INDEXED, field::IMPORT_ANONYMOUS]
            .into_iter()
            .flat_map(|table| (0..self.count(m, table)).map(move |n| (table, n)))
            .filter(|&(table, n)| {
                !Module::batch_resolved(m, m.read32(self.entry(m, table, 8, n) + 4))
            })
            .count() as u32;
        (named, others)
    }

    // ---- the module lists ----------------------------------------------

    /// Add the module at the end of the CRS's auto-link list (from the
    /// CRS's `NEXT`) or of its other list (from its `PREVIOUS`). The head
    /// of a list has the tail as its `PREVIOUS`.
    pub fn register(self, m: &mut dyn Memory, crs: Module, auto_link: bool) {
        let list = if auto_link {
            field::NEXT
        } else {
            field::PREVIOUS
        };
        let head = crs.get(m, list);
        if head == 0 {
            crs.set(m, list, self.0);
            self.set(m, field::PREVIOUS, self.0);
        } else {
            let tail = Module(head).get(m, field::PREVIOUS);
            self.set(m, field::PREVIOUS, tail);
            Module(tail).set(m, field::NEXT, self.0);
            Module(head).set(m, field::PREVIOUS, self.0);
        }
        self.set(m, field::NEXT, 0);
    }

    /// Take the module out of whichever list it is in.
    pub fn unregister(self, m: &mut dyn Memory, crs: Module) {
        let next = self.get(m, field::NEXT);
        let previous = self.get(m, field::PREVIOUS);
        let list = [field::NEXT, field::PREVIOUS]
            .into_iter()
            .find(|&list| crs.get(m, list) == self.0);
        if let Some(list) = list {
            // The head: the next one heads the list and keeps the tail.
            if next != 0 {
                Module(next).set(m, field::PREVIOUS, previous);
            }
            crs.set(m, list, next);
        } else if next != 0 {
            Module(previous).set(m, field::NEXT, next);
            Module(next).set(m, field::PREVIOUS, previous);
        } else {
            // The tail: the head learns of the new tail.
            Module(previous).set(m, field::NEXT, 0);
            for list in [field::NEXT, field::PREVIOUS] {
                let head = crs.get(m, list);
                if head != 0 && Module(head).get(m, field::PREVIOUS) == self.0 {
                    Module(head).set(m, field::PREVIOUS, previous);
                }
            }
        }
        self.set(m, field::NEXT, 0);
        self.set(m, field::PREVIOUS, 0);
    }

    // ---- fixing --------------------------------------------------------

    /// Give up the tables the program no longer needs: at level 1 the
    /// static and internal relocations, at 2 also the imports, at 3 also
    /// the exports (0 gives up nothing). Their offsets then point at the
    /// end of what is kept, the magic becomes "FIXD", and the page-rounded
    /// size kept is returned and recorded.
    pub fn fix(self, m: &mut dyn Memory, level: u32) -> u32 {
        // Where each level's cut starts, in `TABLES`.
        let barrier = [TABLES.len(), 14, 8, 4][level.min(3) as usize];
        let code_end = self.get(m, field::CODE) + self.count(m, field::CODE);
        let end = TABLES[2..barrier]
            .iter()
            .map(|&(table, size)| self.get(m, table) + self.count(m, table) * size)
            .fold((self.0 + HEADER_LEN).max(code_end), u32::max);
        if level != 0 {
            self.set(m, field::MAGIC, MAGIC_FIXD);
            for &(table, _) in &TABLES[barrier..] {
                self.set(m, table, end);
                self.set(m, table + 4, 0);
            }
        }
        let fixed = end.next_multiple_of(0x1000) - self.0;
        self.set(m, field::FIXED_SIZE, fixed);
        fixed
    }
}

/// A flat test memory, and a builder of modules for tests.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::collections::BTreeMap;

    /// Sparse memory: every address exists and starts at zero.
    #[derive(Default)]
    pub struct Flat(BTreeMap<u32, [u8; 0x1000]>);

    impl Memory for Flat {
        fn read8(&self, va: u32) -> u8 {
            self.0
                .get(&(va >> 12))
                .map_or(0, |page| page[va as usize & 0xFFF])
        }

        fn write8(&mut self, va: u32, value: u8) {
            self.0.entry(va >> 12).or_insert([0; 0x1000])[va as usize & 0xFFF] = value;
        }
    }

    impl Flat {
        pub fn load(&mut self, va: u32, bytes: &[u8]) {
            for (n, &b) in bytes.iter().enumerate() {
                self.write8(va + n as u32, b);
            }
        }

        pub fn bytes(&self, va: u32, len: u32) -> Vec<u8> {
            (0..len).map(|n| self.read8(va + n)).collect()
        }
    }

    /// One relocation: where (a segment tag of the module being built),
    /// its type and its addend.
    #[derive(Clone, Copy, Debug)]
    pub struct Patch {
        pub target: u32,
        pub kind: u8,
        pub addend: u32,
    }

    /// Imports from one module by index and by anonymous position.
    #[derive(Clone, Default, Debug)]
    pub struct ModuleImport {
        pub name: String,
        pub indexed: Vec<(u32, Vec<Patch>)>,
        pub anonymous: Vec<(u32, Vec<Patch>)>,
    }

    /// A module to build. Segments are text, read-only data, data and bss,
    /// at indices 0-3 (the loader takes the index for the type).
    #[derive(Clone, Default, Debug)]
    pub struct Spec {
        pub name: String,
        pub text: Vec<u8>,
        pub rodata: Vec<u8>,
        pub data: Vec<u8>,
        pub bss: u32,
        pub exports: Vec<(String, u32)>,
        pub indexed_exports: Vec<u32>,
        pub imports: Vec<(String, Vec<Patch>)>,
        pub modules: Vec<ModuleImport>,
        /// Target tag, type, the segment of the symbol, addend.
        pub internal: Vec<(u32, u8, u8, u32)>,
        pub on_unresolved: Option<u32>,
        /// For a CRS: the program's segments as addresses, in place of
        /// the module's own.
        pub program: Option<[(u32, u32, u32); 4]>,
    }

    fn put(image: &mut [u8], at: u32, value: u32) {
        image[at as usize..at as usize + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn align(image: &mut Vec<u8>, to: usize) {
        image.resize(image.len().div_ceil(to) * to, 0);
    }

    /// An export tree over `names` that `Module::find_export` walks: each
    /// internal node splits on the first bit some names differ in.
    fn tree(names: &[Vec<u8>]) -> Vec<[u16; 4]> {
        fn bit(name: &[u8], test: u16) -> bool {
            name.get(usize::from(test >> 3)).copied().unwrap_or(0) >> (test & 7) & 1 != 0
        }
        fn grow(names: &[Vec<u8>], set: &[u16], nodes: &mut Vec<[u16; 4]>) -> u16 {
            if set.len() == 1 {
                nodes.push([0, 0, 0, set[0]]);
                return (nodes.len() - 1) as u16 | 0x8000;
            }
            let test = (0..0x7FFF)
                .find(|&t| {
                    let ones = set.iter().filter(|&&i| bit(&names[usize::from(i)], t));
                    (1..set.len()).contains(&ones.count())
                })
                .expect("export names differ");
            let index = nodes.len();
            nodes.push([test, 0, 0, 0]);
            let (right, left): (Vec<u16>, Vec<u16>) = set
                .iter()
                .partition(|&&i| bit(&names[usize::from(i)], test));
            nodes[index][1] = grow(names, &left, nodes);
            nodes[index][2] = grow(names, &right, nodes);
            index as u16
        }
        if names.is_empty() {
            return Vec::new();
        }
        let mut nodes = vec![[0u16; 4]];
        let all: Vec<u16> = (0..names.len() as u16).collect();
        nodes[0][1] = grow(names, &all, &mut nodes);
        nodes
    }

    /// The file `spec` describes, laid out in the order of a real one and
    /// padded to a page with 0xCC.
    pub fn build(spec: &Spec) -> Vec<u8> {
        let mut out = vec![0u8; HEADER_LEN as usize];
        let mut header: Vec<(u32, u32, u32)> = Vec::new();
        let mut table = |out: &mut Vec<u8>, field: u32, bytes: &[u8], count: u32| {
            align(out, 4);
            header.push((field, out.len() as u32, count));
            out.extend_from_slice(bytes);
        };
        align(&mut out, 0x80);
        let text_at = out.len() as u32;
        table(&mut out, field::CODE, &spec.text, spec.text.len() as u32);
        let rodata_at = {
            align(&mut out, 4);
            let at = out.len() as u32;
            out.extend_from_slice(&spec.rodata);
            at
        };
        let mut name = spec.name.as_bytes().to_vec();
        name.push(0);
        table(&mut out, field::MODULE_NAME, &name, name.len() as u32);

        // Data goes last; its offset is known once the tables are laid out,
        // so the segment table is written afterwards.
        let segments_at = {
            align(&mut out, 4);
            out.len() as u32
        };
        table(&mut out, field::SEGMENTS, &[0; 48], 4);

        let mut export_strings = Vec::new();
        let mut named = Vec::new();
        let mut name_offsets = Vec::new();
        for (name, _) in &spec.exports {
            name_offsets.push(export_strings.len() as u32);
            export_strings.extend_from_slice(name.as_bytes());
            export_strings.push(0);
        }
        let named_at = out.len() as u32;
        let tree_names: Vec<Vec<u8>> = spec
            .exports
            .iter()
            .map(|(n, _)| n.clone().into_bytes())
            .collect();
        let nodes = tree(&tree_names);
        let tree_len = nodes.len() as u32 * 8;
        let indexed_len = spec.indexed_exports.len() as u32 * 4;
        let strings_at = named_at + spec.exports.len() as u32 * 8 + tree_len + indexed_len;
        for ((_, tag), offset) in spec.exports.iter().zip(&name_offsets) {
            named.extend_from_slice(&(strings_at + offset).to_le_bytes());
            named.extend_from_slice(&tag.to_le_bytes());
        }
        table(
            &mut out,
            field::EXPORT_NAMED,
            &named,
            spec.exports.len() as u32,
        );
        let tree_bytes: Vec<u8> = nodes
            .iter()
            .flatten()
            .flat_map(|h| h.to_le_bytes())
            .collect();
        table(
            &mut out,
            field::EXPORT_TREE,
            &tree_bytes,
            nodes.len() as u32,
        );
        let indexed: Vec<u8> = spec
            .indexed_exports
            .iter()
            .flat_map(|t| t.to_le_bytes())
            .collect();
        table(
            &mut out,
            field::EXPORT_INDEXED,
            &indexed,
            spec.indexed_exports.len() as u32,
        );
        assert_eq!(out.len() as u32, strings_at);
        table(
            &mut out,
            field::EXPORT_STRINGS,
            &export_strings,
            export_strings.len() as u32,
        );

        // Imports: every batch of patches goes into one relocation table.
        let mut relocations: Vec<u8> = Vec::new();
        let mut batch = |patches: &[Patch]| -> u32 {
            let at = relocations.len() as u32;
            for (n, patch) in patches.iter().enumerate() {
                relocations.extend_from_slice(&patch.target.to_le_bytes());
                relocations.push(patch.kind);
                relocations.push((n + 1 == patches.len()) as u8);
                relocations.extend_from_slice(&[0, 0]);
                relocations.extend_from_slice(&patch.addend.to_le_bytes());
            }
            at
        };
        let named_batches: Vec<u32> = spec.imports.iter().map(|(_, p)| batch(p)).collect();
        let indexed_batches: Vec<Vec<u32>> = spec
            .modules
            .iter()
            .map(|m| m.indexed.iter().map(|(_, p)| batch(p)).collect())
            .collect();
        let anonymous_batches: Vec<Vec<u32>> = spec
            .modules
            .iter()
            .map(|m| m.anonymous.iter().map(|(_, p)| batch(p)).collect())
            .collect();

        let modules_at = {
            align(&mut out, 4);
            out.len() as u32
        };
        let relocations_at = modules_at + spec.modules.len() as u32 * 20;
        let import_named_at = relocations_at + relocations.len() as u32;
        let import_indexed_at = import_named_at + spec.imports.len() as u32 * 8;
        let indexed_count: usize = spec.modules.iter().map(|m| m.indexed.len()).sum();
        let anonymous_at = import_indexed_at + indexed_count as u32 * 8;
        let anonymous_count: usize = spec.modules.iter().map(|m| m.anonymous.len()).sum();
        let import_strings_at = anonymous_at + anonymous_count as u32 * 8;

        let mut import_strings = Vec::new();
        let mut string = |s: &str| {
            let at = import_strings_at + import_strings.len() as u32;
            import_strings.extend_from_slice(s.as_bytes());
            import_strings.push(0);
            at
        };
        let mut modules = Vec::new();
        let (mut indexed_next, mut anonymous_next) = (import_indexed_at, anonymous_at);
        for module in &spec.modules {
            for value in [
                string(&module.name),
                indexed_next,
                module.indexed.len() as u32,
                anonymous_next,
                module.anonymous.len() as u32,
            ] {
                modules.extend_from_slice(&value.to_le_bytes());
            }
            indexed_next += module.indexed.len() as u32 * 8;
            anonymous_next += module.anonymous.len() as u32 * 8;
        }
        let mut import_named = Vec::new();
        for ((name, _), at) in spec.imports.iter().zip(&named_batches) {
            import_named.extend_from_slice(&string(name).to_le_bytes());
            import_named.extend_from_slice(&(relocations_at + at).to_le_bytes());
        }
        let pairs = |list: Vec<(u32, u32)>| -> Vec<u8> {
            list.into_iter()
                .flat_map(|(a, b)| [a.to_le_bytes(), (relocations_at + b).to_le_bytes()])
                .flatten()
                .collect()
        };
        let import_indexed = pairs(
            spec.modules
                .iter()
                .zip(&indexed_batches)
                .flat_map(|(m, b)| m.indexed.iter().map(|(i, _)| *i).zip(b.iter().copied()))
                .collect(),
        );
        let import_anonymous = pairs(
            spec.modules
                .iter()
                .zip(&anonymous_batches)
                .flat_map(|(m, b)| m.anonymous.iter().map(|(t, _)| *t).zip(b.iter().copied()))
                .collect(),
        );
        table(
            &mut out,
            field::IMPORT_MODULES,
            &modules,
            spec.modules.len() as u32,
        );
        table(
            &mut out,
            field::IMPORT_RELOCATIONS,
            &relocations,
            relocations.len() as u32 / 12,
        );
        table(
            &mut out,
            field::IMPORT_NAMED,
            &import_named,
            spec.imports.len() as u32,
        );
        table(
            &mut out,
            field::IMPORT_INDEXED,
            &import_indexed,
            indexed_count as u32,
        );
        table(
            &mut out,
            field::IMPORT_ANONYMOUS,
            &import_anonymous,
            anonymous_count as u32,
        );
        assert_eq!(out.len() as u32, import_strings_at);
        table(
            &mut out,
            field::IMPORT_STRINGS,
            &import_strings,
            import_strings.len() as u32,
        );
        table(&mut out, field::STATIC_ANONYMOUS, &[], 0);
        let internal: Vec<u8> = spec
            .internal
            .iter()
            .flat_map(|&(target, kind, segment, addend)| {
                let mut entry = target.to_le_bytes().to_vec();
                entry.extend_from_slice(&[kind, segment, 0, 0]);
                entry.extend_from_slice(&addend.to_le_bytes());
                entry
            })
            .collect();
        table(
            &mut out,
            field::INTERNAL_RELOCATIONS,
            &internal,
            spec.internal.len() as u32,
        );
        table(&mut out, field::STATIC_RELOCATIONS, &[], 0);
        let data_at = {
            align(&mut out, 4);
            out.len() as u32
        };
        table(&mut out, field::DATA, &spec.data, spec.data.len() as u32);
        align(&mut out, 0x1000);
        let end = out.len();
        out[data_at as usize + spec.data.len()..end].fill(0xCC);

        let segments = [
            (text_at, spec.text.len() as u32, segment::TEXT),
            (rodata_at, spec.rodata.len() as u32, segment::RODATA),
            (data_at, spec.data.len() as u32, segment::DATA),
            (0, spec.bss, segment::BSS),
        ];
        let segments = spec.program.unwrap_or(segments);
        for (n, (offset, size, kind)) in segments.into_iter().enumerate() {
            let at = segments_at + n as u32 * 12;
            put(&mut out, at, offset);
            put(&mut out, at + 4, size);
            put(&mut out, at + 8, kind);
        }
        put(&mut out, field::MAGIC, MAGIC_CRO0);
        put(&mut out, field::NAME, header[1].1);
        put(&mut out, field::FILE_SIZE, end as u32);
        put(&mut out, field::BSS_SIZE, spec.bss);
        for at in [field::CONTROL_OBJECT, field::ON_LOAD, field::ON_EXIT] {
            put(&mut out, at, u32::MAX);
        }
        put(
            &mut out,
            field::ON_UNRESOLVED,
            spec.on_unresolved.unwrap_or(u32::MAX),
        );
        for (field, offset, count) in header {
            put(&mut out, field, offset);
            put(&mut out, field + 4, count);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn exporter() -> Spec {
        Spec {
            name: "Lib".into(),
            text: vec![0xAA; 0x40],
            rodata: vec![0xBB; 0x10],
            data: vec![1, 2, 3, 4],
            bss: 0x20,
            exports: ["alpha", "beta", "betamax", "gamma", "a"]
                .iter()
                .enumerate()
                .map(|(n, s)| (s.to_string(), tag(0, n as u32 * 4)))
                .collect(),
            ..Spec::default()
        }
    }

    #[test]
    fn a_built_module_has_its_header_tables_and_segments() {
        let image = build(&exporter());
        assert_eq!(image.len() % 0x1000, 0);
        let mut m = Flat::default();
        m.load(0, &image);
        let module = Module(0);
        assert_eq!(module.get(&m, field::MAGIC), MAGIC_CRO0);
        assert_eq!(module.get(&m, field::FILE_SIZE), image.len() as u32);
        assert_eq!(module.name(&m), "Lib");
        assert_eq!(module.segment(&m, 0), (0x180, 0x40, segment::TEXT));
        assert_eq!(module.segment(&m, 3), (0, 0x20, segment::BSS));
        assert_eq!(*image.last().unwrap(), 0xCC);
        // The tables lie in the order the loader checks.
        let starts: Vec<u32> = FILE_ORDER.iter().map(|&f| module.get(&m, f)).collect();
        assert!(starts.windows(2).all(|w| w[0] <= w[1]), "{starts:x?}");
        assert_eq!(m.bytes(module.get(&m, field::DATA), 4), [1, 2, 3, 4]);
    }

    #[test]
    fn exports_are_found_through_the_tree_and_must_match_in_full() {
        let mut m = Flat::default();
        m.load(0, &build(&exporter()));
        let module = Module(0);
        for (n, name) in ["alpha", "beta", "betamax", "gamma", "a"]
            .iter()
            .enumerate()
        {
            // Offsets, until the module is rebased.
            assert_eq!(
                module.find_export(&m, name.as_bytes()),
                Some(0x180 + n as u32 * 4),
                "{name}"
            );
        }
        assert_eq!(module.find_export(&m, b"bet"), None);
        assert_eq!(module.find_export(&m, b"delta"), None);
        assert_eq!(module.address(&m, tag(1, 0x10)), None, "past the segment");
        assert_eq!(module.address(&m, tag(5, 0)), None, "no such segment");
    }

    #[test]
    fn relocations_encode_what_3dbrew_gives() {
        let mut m = Flat::default();
        let mut at = |kind, addend, symbol, place, before| {
            m.write32(0x1000, before);
            relocate(&mut m, 0x1000, kind, addend, symbol, place).map(|()| m.read32(0x1000))
        };
        let minus = |n: u32| n.wrapping_neg();
        assert_eq!(at(reloc::ABS32, 4, 0x2000, 0x1000, 0), Ok(0x2004));
        assert_eq!(at(reloc::REL32, 0, 0x2000, 0x1800, 0), Ok(0x800));
        // BL from 0x1000 to 0x2000: the PC is 8 ahead.
        assert_eq!(
            at(reloc::CALL, minus(8), 0x2000, 0x1000, 0),
            Ok(0xEB00_03FE)
        );
        // BLX to Thumb code at 0x2002.
        assert_eq!(
            at(reloc::CALL, minus(8), 0x2003, 0x1000, 0),
            Ok(0xFB00_03FE)
        );
        assert_eq!(
            at(reloc::JUMP24, minus(8), 0x2000, 0x1000, 0x1A00_0000),
            Ok(0x1A00_03FE),
            "the condition stays"
        );
        // A Thumb BL pair from 0x1000 to 0x2000, first half-word first.
        assert_eq!(
            at(reloc::THM_PC22, minus(4), 0x2001, 0x1000, 0),
            Ok(0xFFFE_F000)
        );
        assert_eq!(at(reloc::PREL31, 0, 0x2000, 0x1000, 0), Ok(0x1000));
        assert_eq!(
            at(reloc::CALL, 0, 0x0400_0000, 0x1000, 0),
            Err(OUT_OF_RANGE)
        );
        assert_eq!(at(7, 0, 0, 0, 0), Err(UNKNOWN_RELOCATION));
    }

    const CRS: Module = Module(0x0080_0000);
    const LIB: Module = Module(0x0090_0000);
    const USER: Module = Module(0x00A0_0000);
    const DATA_BUFFER: u32 = 0x0800_0000;
    const BSS_BUFFER: u32 = 0x0800_1000;

    fn abs(target: u32) -> Patch {
        Patch {
            target,
            kind: reloc::ABS32,
            addend: 0,
        }
    }

    /// The program: text at 0x100000, read-only data and data a page
    /// apart. It exports one function, and takes one function of "Lib"
    /// into its data.
    fn crs() -> Spec {
        Spec {
            name: "|static|".into(),
            program: Some([
                (0x10_0000, 0x1000, segment::TEXT),
                (0x10_1000, 0x1000, segment::RODATA),
                (0x10_2000, 0x1000, segment::DATA),
                (0, 0, segment::BSS),
            ]),
            exports: vec![("crs_func".into(), tag(0, 0x10))],
            modules: vec![ModuleImport {
                name: "Lib".into(),
                anonymous: vec![(tag(0, 8), vec![abs(tag(2, 0x40))])],
                ..Default::default()
            }],
            on_unresolved: Some(tag(0, 0x100)),
            ..Spec::default()
        }
    }

    /// A library that imports from the program, exports to others and
    /// points at itself.
    fn lib() -> Spec {
        Spec {
            name: "Lib".into(),
            text: vec![0; 0x40],
            rodata: vec![0; 0x10],
            data: vec![0; 0x10],
            bss: 0x20,
            exports: vec![("lib_func".into(), tag(0, 8))],
            indexed_exports: vec![tag(0, 0xC)],
            imports: vec![
                (
                    "crs_func".into(),
                    vec![
                        Patch {
                            addend: 4,
                            ..abs(tag(1, 0))
                        },
                        abs(tag(2, 4)),
                    ],
                ),
                ("missing".into(), vec![abs(tag(1, 4))]),
            ],
            internal: vec![
                (tag(2, 0), reloc::ABS32, 0, 4),
                (tag(0, 0x20), reloc::REL32, 1, 0),
            ],
            on_unresolved: Some(tag(0, 0x30)),
            ..Spec::default()
        }
    }

    /// A module that imports "Lib"'s function by name, its first indexed
    /// export and its read-only data by position.
    fn user() -> Spec {
        Spec {
            name: "User".into(),
            text: vec![0; 0x20],
            imports: vec![("lib_func".into(), vec![abs(tag(0, 0))])],
            modules: vec![ModuleImport {
                name: "Lib".into(),
                indexed: vec![(0, vec![abs(tag(0, 4))])],
                anonymous: vec![(tag(1, 0), vec![abs(tag(0, 8))])],
            }],
            on_unresolved: Some(tag(0, 0x10)),
            ..Spec::default()
        }
    }

    /// Map, rebase and link a module as `LoadCRO_New` does.
    fn load(m: &mut Flat, module: Module, spec: &Spec) -> Result<(), ResultCode> {
        let image = build(spec);
        m.load(module.0, &image);
        let data = (DATA_BUFFER, 0x100);
        let bss = (BSS_BUFFER, 0x100);
        module.rebase(m, CRS, image.len() as u32, data, bss, false)?;
        module.link(m, CRS, true)?;
        module.register(m, CRS, true);
        Ok(())
    }

    fn console() -> Flat {
        let mut m = Flat::default();
        let image = build(&crs());
        m.load(CRS.0, &image);
        CRS.rebase(&mut m, CRS, image.len() as u32, (0, 0), (0, 0), true)
            .unwrap();
        m
    }

    #[test]
    fn rebasing_places_segments_and_relocates_a_module_to_itself() {
        let mut m = console();
        // The program's import from "Lib" waits on the program's handler.
        assert_eq!(m.read32(0x10_2040), 0x10_0100);

        let image = build(&lib());
        m.load(LIB.0, &image);
        let size = image.len() as u32;
        let data = (DATA_BUFFER, 0x100);
        assert_eq!(
            LIB.rebase(
                &mut m,
                CRS,
                size,
                (DATA_BUFFER, 8),
                (BSS_BUFFER, 0x100),
                false
            ),
            Err(BUFFER_TOO_SMALL)
        );
        m.load(LIB.0, &image);
        LIB.rebase(&mut m, CRS, size, data, (BSS_BUFFER, 0x100), false)
            .unwrap();
        let text = LIB.0 + 0x180;
        assert_eq!(LIB.segment(&m, 0), (text, 0x40, segment::TEXT));
        assert_eq!(LIB.segment(&m, 2).0, DATA_BUFFER);
        assert_eq!(LIB.segment(&m, 3).0, BSS_BUFFER);
        assert_eq!(LIB.find_export(&m, b"lib_func"), Some(text + 8));
        // The data is relocated in the image, which the program copies.
        assert_eq!(m.read32(LIB.get(&m, field::DATA)), text + 4);
        let rodata = LIB.segment(&m, 1).0;
        assert_eq!(m.read32(text + 0x20), rodata.wrapping_sub(text + 0x20));
        // Imports wait on the module's handler.
        assert_eq!(m.read32(rodata), text + 0x30 + 4);
        assert_eq!(
            LIB.unresolved(&m),
            (vec!["crs_func".into(), "missing".into()], 0)
        );

        // A second rebase finds the header already rebased.
        assert_eq!(
            LIB.rebase(&mut m, CRS, size, data, (BSS_BUFFER, 0x100), false),
            Err(BAD_HEADER)
        );
    }

    #[test]
    fn modules_link_both_ways_and_unlink() {
        let mut m = console();
        // The user comes first: what it wants from "Lib" is not there yet.
        load(&mut m, USER, &user()).unwrap();
        let user_text = USER.segment(&m, 0).0;
        assert_eq!(m.read32(user_text), user_text + 0x10);
        assert_eq!(USER.unresolved(&m), (vec!["lib_func".into()], 2));

        load(&mut m, LIB, &lib()).unwrap();
        let text = LIB.segment(&m, 0).0;
        let rodata = LIB.segment(&m, 1).0;
        // Lib imported from the program: in place, and into the image's
        // data rather than the buffer (`LoadCRO_New`).
        assert_eq!(m.read32(rodata), 0x10_0010 + 4);
        assert_eq!(m.read32(LIB.get(&m, field::DATA) + 4), 0x10_0010);
        assert_eq!(m.read32(DATA_BUFFER + 4), text + 0x30);
        assert_eq!(LIB.unresolved(&m), (vec!["missing".into()], 0));
        // And exported to the program and to the user.
        assert_eq!(m.read32(0x10_2040), text + 8);
        assert_eq!(m.read32(user_text), text + 8);
        assert_eq!(m.read32(user_text + 4), text + 0xC);
        assert_eq!(m.read32(user_text + 8), rodata);
        assert_eq!(USER.unresolved(&m), (vec![], 0));

        // The auto-link list: the program, then the user, then Lib; the
        // head knows the tail.
        assert_eq!(CRS.get(&m, field::NEXT), USER.0);
        assert_eq!(USER.get(&m, field::NEXT), LIB.0);
        assert_eq!(USER.get(&m, field::PREVIOUS), LIB.0);
        assert_eq!(LIB.get(&m, field::PREVIOUS), USER.0);

        LIB.unlink(&mut m, CRS).unwrap();
        assert_eq!(m.read32(user_text), user_text + 0x10);
        assert_eq!(m.read32(user_text + 8), user_text + 0x10);
        assert_eq!(m.read32(0x10_2040), 0x10_0100);
        assert_eq!(m.read32(rodata), text + 0x30 + 4);
        assert_eq!(USER.unresolved(&m), (vec!["lib_func".into()], 2));
        LIB.link(&mut m, CRS, false).unwrap();
        assert_eq!(USER.unresolved(&m), (vec![], 0));
    }

    #[test]
    fn fixing_gives_up_the_tables_a_level_names() {
        let mut m = console();
        load(&mut m, LIB, &lib()).unwrap();
        let size = LIB.get(&m, field::FILE_SIZE);
        let kept = LIB.fix(&mut m, 0);
        assert!(kept <= size && kept.is_multiple_of(0x1000), "{kept:#x}");
        assert!(!LIB.is_fixed(&m));
        LIB.set(&mut m, field::FIXED_SIZE, 0);
        LIB.fix(&mut m, 3);
        assert!(LIB.is_fixed(&m) && LIB.is_loaded(&m));
        assert_eq!(LIB.count(&m, field::EXPORT_NAMED), 0);
        assert_eq!(LIB.count(&m, field::IMPORT_NAMED), 0);
        assert_eq!(LIB.find_export(&m, b"lib_func"), None);
        assert_eq!(LIB.executable_pages(&m), Some((LIB.0, 0x1000)));
    }
}
