//! Console detection and construction.
//!
//! Frontends depend on this crate alone: [`detect`] identifies a ROM from its
//! header bytes (the file extension is never consulted) and [`load`] /
//! [`load_with`] build the matching `Box<dyn System>`.

use emu_core::System;

/// A supported console.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Game Boy (DMG).
    Gb,
    /// Game Boy Color, or a cartridge that requests colour mode.
    Gbc,
    /// Game Boy Advance.
    Gba,
    /// Nintendo 3DS.
    #[cfg(feature = "ctr")]
    Ctr,
}

impl Kind {
    /// Stable identifier matching [`System::name`].
    pub fn name(self) -> &'static str {
        match self {
            Kind::Gb => "gb",
            Kind::Gbc => "gbc",
            Kind::Gba => "gba",
            #[cfg(feature = "ctr")]
            Kind::Ctr => "3ds",
        }
    }

    /// The conventional file extension.
    pub fn extension(self) -> &'static str {
        match self {
            Kind::Gb => "gb",
            Kind::Gbc => "gbc",
            Kind::Gba => "gba",
            #[cfg(feature = "ctr")]
            Kind::Ctr => "3ds",
        }
    }
}

/// Options for [`load_with`].
#[derive(Clone, Default, Debug)]
pub struct LoadOptions {
    /// A 16 KB GBA BIOS image. Without one the GBA boots with high-level
    /// BIOS emulation.
    pub gba_bios: Option<Vec<u8>>,
    /// With a BIOS image, run the cold-boot logo intro instead of a warm boot.
    pub cold: bool,
}

/// The first eight bytes of the Nintendo logo every Game Boy cartridge
/// carries at 0x104.
const GB_LOGO: [u8; 8] = [0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B];
/// The first eight bytes of the compressed logo at 0x04 of a GBA header.
const GBA_LOGO: [u8; 8] = [0x24, 0xFF, 0xAE, 0x51, 0x69, 0x9A, 0xA2, 0x21];

/// Identify a ROM image from its header. Returns `None` for data that carries
/// neither a Game Boy nor a GBA header.
pub fn detect(rom: &[u8]) -> Option<Kind> {
    if rom.len() >= 0x150 && rom[0x104..0x10C] == GB_LOGO {
        return Some(if rom[0x143] & 0x80 != 0 {
            Kind::Gbc
        } else {
            Kind::Gb
        });
    }
    if rom.len() >= 0xC0 && rom[0xB2] == 0x96 && rom[0x04..0x0C] == GBA_LOGO {
        return Some(Kind::Gba);
    }
    #[cfg(feature = "ctr")]
    if ctr_fs::ImageKind::detect(rom).is_some() {
        return Some(Kind::Ctr);
    }
    None
}

/// Build the console for `rom` with default options.
pub fn load(rom: Vec<u8>) -> Result<Box<dyn System>, String> {
    load_with(rom, &LoadOptions::default())
}

/// Build the console for `rom`.
pub fn load_with(rom: Vec<u8>, opts: &LoadOptions) -> Result<Box<dyn System>, String> {
    match detect(&rom) {
        Some(Kind::Gb) | Some(Kind::Gbc) => {
            let cart = gb_core::Cartridge::load(&rom)?;
            Ok(gb_core::Gb::system(cart))
        }
        Some(Kind::Gba) => match &opts.gba_bios {
            Some(bios) if bios.len() == 0x4000 => Ok(gba_core::Gba::system_with_bios(
                rom,
                bios.clone(),
                opts.cold,
            )),
            Some(bios) => Err(format!(
                "GBA BIOS must be exactly 16384 bytes, got {}",
                bios.len()
            )),
            None => Ok(gba_core::Gba::system(rom)),
        },
        #[cfg(feature = "ctr")]
        Some(Kind::Ctr) => match ctr_fs::ImageKind::detect(&rom) {
            Some(ctr_fs::ImageKind::Firm) => ctr_core::Ctr::system(rom),
            _ => Err(
                "3DS game and homebrew images need the firmware boot path, which is not                  implemented yet; only FIRM payloads load"
                    .to_string(),
            ),
        },
        None => Err("not a Game Boy, Game Boy Color or Game Boy Advance ROM".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gb_rom(cgb_flag: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x104..0x10C].copy_from_slice(&GB_LOGO);
        rom[0x134..0x138].copy_from_slice(b"TEST");
        rom[0x143] = cgb_flag;
        rom
    }

    fn gba_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x4000];
        rom[0x04..0x0C].copy_from_slice(&GBA_LOGO);
        rom[0xA0..0xA4].copy_from_slice(b"TEST");
        rom[0xB2] = 0x96;
        rom
    }

    #[test]
    fn detects_each_console_from_the_header() {
        assert_eq!(detect(&gb_rom(0x00)), Some(Kind::Gb));
        assert_eq!(detect(&gb_rom(0x80)), Some(Kind::Gbc));
        assert_eq!(detect(&gb_rom(0xC0)), Some(Kind::Gbc));
        assert_eq!(detect(&gba_rom()), Some(Kind::Gba));
    }

    #[test]
    fn rejects_short_and_garbage_buffers() {
        assert_eq!(detect(&[]), None);
        assert_eq!(detect(&[0xFF; 100]), None);
        assert_eq!(detect(&[0u8; 0x8000]), None);
        assert_eq!(detect(&gb_rom(0)[..0x140]), None);
        let mut bad = gba_rom();
        bad[0xB2] = 0;
        assert_eq!(detect(&bad), None);
        assert!(load(vec![0u8; 100]).is_err());
    }

    #[test]
    fn loads_the_matching_system() {
        let gb = load(gb_rom(0)).unwrap();
        assert_eq!(gb.name(), "gb");
        assert_eq!(gb.title(), "TEST");
        assert_eq!(load(gb_rom(0x80)).unwrap().name(), "gbc");
        let gba = load(gba_rom()).unwrap();
        assert_eq!(gba.name(), "gba");
        assert_eq!(gba.screen(), emu_core::Screen::new(240, 160));
    }

    #[cfg(feature = "ctr")]
    #[test]
    fn loads_a_3ds_firm_and_refuses_other_3ds_images() {
        let firm = ctr_fs::firm::build(0, 0x0800_6000, &[(0x0800_6000, &[0; 4])]);
        assert_eq!(detect(&firm), Some(Kind::Ctr));
        let system = load(firm).unwrap();
        assert_eq!(system.name(), "3ds");
        assert_eq!(system.screens().len(), 2);

        let mut ncsd = vec![0u8; 0x200];
        ncsd[0x100..0x104].copy_from_slice(b"NCSD");
        assert_eq!(detect(&ncsd), Some(Kind::Ctr));
        assert!(load(ncsd).is_err());
    }

    #[test]
    fn gba_bios_must_be_16k() {
        let opts = LoadOptions {
            gba_bios: Some(vec![0; 100]),
            cold: false,
        };
        assert!(load_with(gba_rom(), &opts).is_err());
        let opts = LoadOptions {
            gba_bios: Some(vec![0; 0x4000]),
            cold: true,
        };
        assert!(load_with(gba_rom(), &opts).is_ok());
    }
}
