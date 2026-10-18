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
}
