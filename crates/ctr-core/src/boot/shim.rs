//! Launch a FIRM without the boot ROMs.
//!
//! This does what a chainloader such as boot9strap does once it has a FIRM in
//! hand: copy each section to its physical load address and hand the two
//! entry points to the processors. It models no Nintendo behaviour, verifies
//! no signature and sets up no key, so it suits bare-metal homebrew only;
//! official firmware needs the real boot ROMs.

use crate::bus::PhysMem;
use ctr_fs::Firm;

/// Where each processor starts executing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    pub arm9: u32,
    /// Zero when the FIRM has no ARM11 code; that processor then stays
    /// parked, as it does on a console.
    pub arm11: u32,
}

/// Copy the sections of `image` into `mem`.
pub fn load_firm(mem: &mut PhysMem, image: &[u8]) -> Result<Entry, String> {
    let firm = Firm::parse(image).map_err(|e| e.to_string())?;
    for (index, section) in firm.sections.iter().enumerate() {
        let target = mem
            .slice_mut(section.load_address, section.data.len())
            .ok_or_else(|| {
                format!(
                    "FIRM section {index} loads {} bytes at {:#010x}, which is not RAM",
                    section.data.len(),
                    section.load_address
                )
            })?;
        target.copy_from_slice(section.data);
    }
    Ok(Entry {
        arm9: firm.arm9_entry,
        arm11: firm.arm11_entry,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctr_fs::firm::build;

    #[test]
    fn copies_every_section_to_its_load_address() {
        let image = build(
            0x1FF8_0000,
            0x0800_6000,
            &[(0x0800_6000, &[1, 2, 3, 4]), (0x1FF8_0000, &[5, 6])],
        );
        let mut mem = PhysMem::new();
        let entry = load_firm(&mut mem, &image).unwrap();
        assert_eq!(
            entry,
            Entry {
                arm9: 0x0800_6000,
                arm11: 0x1FF8_0000
            }
        );
        assert_eq!(mem.slice(0x0800_6000, 4), Some(&[1u8, 2, 3, 4][..]));
        assert_eq!(mem.slice(0x1FF8_0000, 2), Some(&[5u8, 6][..]));
    }

    #[test]
    fn refuses_a_section_outside_ram() {
        let image = build(0, 0x1000_0000, &[(0x1000_0000, &[0; 4])]);
        let err = load_firm(&mut PhysMem::new(), &image).unwrap_err();
        assert!(err.contains("0x10000000"), "{err}");

        let straddles = build(0, 0, &[(0x080F_FFFE, &[0; 4])]);
        assert!(load_firm(&mut PhysMem::new(), &straddles).is_err());
    }
}
