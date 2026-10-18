//! Decoding: which of the instruction patterns an opcode is, looked up in a
//! table of all 65,536 opcodes built once (Teakra's `decoder.h`,
//! `matcher.h`).

mod table;

pub use table::{dispatch, PATTERNS};

use std::sync::OnceLock;

/// One instruction pattern: an opcode matches when `opcode & mask ==
/// expected` and no exception `(mask, value)` matches; an `expanded`
/// pattern takes a second word.
pub struct Pattern {
    pub name: &'static str,
    pub mask: u16,
    pub expected: u16,
    pub expanded: bool,
    pub except: &'static [(u16, u16)],
}

impl Pattern {
    pub fn matches(&self, opcode: u16) -> bool {
        opcode & self.mask == self.expected
            && !self
                .except
                .iter()
                .any(|&(mask, value)| opcode & mask == value)
    }
}

/// The pattern index of an opcode that matches none.
pub const UNDEFINED: u16 = u16::MAX;

/// The pattern of every opcode, and whether it takes a second word.
pub struct Table {
    pub entries: Vec<u16>,
    pub expanded: Vec<bool>,
}

fn build() -> Table {
    let mut entries = vec![UNDEFINED; 0x1_0000];
    let mut expanded = vec![false; 0x1_0000];
    for opcode in 0..=u16::MAX {
        if let Some(index) = PATTERNS.iter().position(|p| p.matches(opcode)) {
            entries[opcode as usize] = index as u16;
            expanded[opcode as usize] = PATTERNS[index].expanded;
        }
    }
    Table { entries, expanded }
}

pub fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(build)
}

/// The name of the instruction an opcode is, for diagnostics.
pub fn name(opcode: u16) -> &'static str {
    match table().entries[opcode as usize] {
        UNDEFINED => "undefined",
        index => PATTERNS[index as usize].name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_opcode_matches_at_most_one_pattern() {
        let mut defined = 0;
        for opcode in 0..=u16::MAX {
            let matching = PATTERNS.iter().filter(|p| p.matches(opcode)).count();
            assert!(matching <= 1, "{opcode:#06x} matches {matching} patterns");
            defined += matching;
        }
        // Seventy opcodes are no instruction.
        assert_eq!(defined, 65_466);
    }

    #[test]
    fn known_opcodes_decode_to_their_instructions() {
        assert_eq!(name(0x0000), "nop");
        assert_eq!(name(0x4180), "br");
        assert_eq!(name(0x67D0), "moda4");
        assert_eq!(name(0x57F0), "brr");
        assert_eq!(name(0xD4BC), "mov");
        assert!(table().expanded[0x4180]);
        assert!(!table().expanded[0x57F0]);
        // tstb with r6 overrides the general form at register field 24.
        assert_eq!(name(0x9018), "tstb_r6");
    }
}
