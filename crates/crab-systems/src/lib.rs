//! Console detection and construction.
//!
//! Frontends depend on this crate alone: [`detect`] identifies a ROM from its
//! header bytes (the file extension is never consulted) and [`load`] /
//! [`load_with`] build the matching `Box<dyn System>`. [`load_media`] does the
//! same for an image that is read in pieces, which is how a 3DS cartridge of
//! several gigabytes is opened. With the `std` feature, [`file`] opens
//! images on the host's file system.

use emu_core::{Storage, System};

#[cfg(feature = "std")]
pub mod file;
#[cfg(feature = "std")]
pub use file::{detect_file, load_file, FileStorage};

/// Enough of an image to identify it: the 3DS magics sit at 0x100.
const HEADER_LEN: u64 = 0x200;

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
        self.extensions()[0]
    }

    /// Every file extension images of the console carry, the conventional
    /// one first, for file pickers. Detection never looks at them.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Kind::Gb => &["gb"],
            Kind::Gbc => &["gbc"],
            Kind::Gba => &["gba"],
            // Card images, program contents, homebrew and firmware payloads.
            #[cfg(feature = "ctr")]
            Kind::Ctr => &["3ds", "cci", "cxi", "3dsx", "firm"],
        }
    }

    /// The consoles this build supports.
    pub const ALL: &'static [Kind] = &[
        Kind::Gb,
        Kind::Gbc,
        Kind::Gba,
        #[cfg(feature = "ctr")]
        Kind::Ctr,
    ];
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

/// The header of a medium: enough of it to identify the image.
fn header_of(media: &mut dyn Storage) -> Result<Vec<u8>, String> {
    let mut header = vec![0u8; media.len().min(HEADER_LEN) as usize];
    media.read_at(0, &mut header).map_err(|e| e.to_string())?;
    Ok(header)
}

/// Identify the image on a medium from its header alone, as [`detect`] does
/// for one held in memory.
pub fn detect_media(media: &mut dyn Storage) -> Result<Option<Kind>, String> {
    Ok(detect(&header_of(media)?))
}

/// Build the console for an image that is read where it lies. Images of the
/// cartridge consoles, 3DS firmware payloads and 3DSX homebrew are small and
/// are read whole; a 3DS game (a card image or a program content) keeps the
/// medium and reads it as the game asks.
pub fn load_media(
    mut media: Box<dyn Storage>,
    opts: &LoadOptions,
) -> Result<Box<dyn System>, String> {
    #[cfg(feature = "ctr")]
    if matches!(
        ctr_fs::ImageKind::detect(&header_of(&mut *media)?),
        Some(ctr_fs::ImageKind::Ncsd | ctr_fs::ImageKind::Ncch)
    ) {
        return ctr_hle::Horizon::system_from_media(media);
    }
    // Anything else is small enough to hold.
    const LIMIT: u64 = 1 << 30;
    if media.len() > LIMIT {
        return Err(format!(
            "an image of {} bytes is not one of a supported console",
            media.len()
        ));
    }
    let mut rom = vec![0u8; media.len() as usize];
    media.read_at(0, &mut rom).map_err(|e| e.to_string())?;
    load_with(rom, opts)
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
        // Firmware runs on the low-level machine; games and homebrew in the
        // high-level mode.
        Some(Kind::Ctr) => match ctr_fs::ImageKind::detect(&rom) {
            Some(ctr_fs::ImageKind::Firm) => ctr_core::Ctr::system(rom),
            _ => ctr_hle::Horizon::system(rom),
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
    fn loads_a_3ds_firm_and_refuses_a_broken_card() {
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

    /// A medium that says it is `len` bytes long, holds `data` at its start
    /// and zeros after it, and counts what is read from it.
    struct Huge {
        data: Vec<u8>,
        len: u64,
        bytes_read: std::rc::Rc<std::cell::Cell<u64>>,
    }

    impl Huge {
        fn new(data: Vec<u8>) -> (Huge, std::rc::Rc<std::cell::Cell<u64>>) {
            let bytes_read = std::rc::Rc::new(std::cell::Cell::new(0));
            let huge = Huge {
                data,
                len: 4 << 30,
                bytes_read: bytes_read.clone(),
            };
            (huge, bytes_read)
        }
    }

    impl Storage for Huge {
        fn len(&self) -> u64 {
            self.len
        }
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), emu_core::StorageError> {
            self.bytes_read
                .set(self.bytes_read.get() + buf.len() as u64);
            for (i, byte) in buf.iter_mut().enumerate() {
                *byte = self.data.get(offset as usize + i).copied().unwrap_or(0);
            }
            Ok(())
        }
    }

    #[test]
    fn media_are_identified_from_their_header_alone() {
        let system = load_media(Box::new(gb_rom(0x00)), &LoadOptions::default()).unwrap();
        assert_eq!(system.name(), "gb");
        assert_eq!(detect_media(&mut gba_rom()), Ok(Some(Kind::Gba)));

        // Four gigabytes of nothing known: refused after reading the header.
        let (huge, bytes_read) = Huge::new(vec![0; 0x200]);
        assert!(load_media(Box::new(huge), &LoadOptions::default()).is_err());
        assert_eq!(bytes_read.get(), HEADER_LEN);
    }

    #[test]
    fn every_console_names_its_file_extensions() {
        for kind in Kind::ALL {
            assert_eq!(kind.extension(), kind.extensions()[0]);
        }
        #[cfg(feature = "ctr")]
        assert!(Kind::Ctr.extensions().contains(&"3dsx"));
    }

    #[cfg(feature = "ctr")]
    #[test]
    fn homebrew_runs_in_the_high_level_mode() {
        // svc 0x03: the program exits at once.
        let code = 0xEF00_0003u32.to_le_bytes();
        let image = ctr_fs::threedsx::build([&code, &[], &[]], 0, &Default::default());
        assert_eq!(detect(&image), Some(Kind::Ctr));
        let mut system = load(image.clone()).unwrap();
        assert_eq!(system.name(), "3ds");
        assert!(system.info().contains("high-level"));
        system.run_frame();
        assert_eq!(system.screens().len(), 2);
        assert!(load_media(Box::new(image), &LoadOptions::default()).is_ok());
    }

    #[cfg(feature = "ctr")]
    fn put32(image: &mut [u8], at: usize, value: u32) {
        image[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// A decrypted card image whose program is `code` at 0x100000, with
    /// `save_size` bytes of save data and a RomFS of `romfs_len` zeros.
    #[cfg(feature = "ctr")]
    fn card(code: &[u8], save_size: u32, romfs_len: usize) -> Vec<u8> {
        let mut image = vec![0u8; 0x200];
        image[0x100..0x104].copy_from_slice(b"NCSD");
        // Partition 0 from media unit 1 (0x200).
        put32(&mut image, 0x120, 1);

        let mut ncch = vec![0u8; 0x200];
        ncch[0x100..0x104].copy_from_slice(b"NCCH");
        ncch[0x150..0x15A].copy_from_slice(b"CTR-P-TEST");
        // Extended header length; flags[7] bit 2: no encryption.
        put32(&mut ncch, 0x180, 0x400);
        ncch[0x18F] = 4;
        let mut exheader = vec![0u8; 0x400];
        exheader[..4].copy_from_slice(b"test");
        let pages = code.len().div_ceil(0x1000) as u32;
        for (at, addr, pages, size) in [
            (0x10, 0x10_0000, pages, code.len() as u32),
            (0x20, 0x10_0000 + pages * 0x1000, 0, 0),
            (0x30, 0x10_0000 + pages * 0x1000, 0, 0),
        ] {
            put32(&mut exheader, at, addr);
            put32(&mut exheader, at + 4, pages);
            put32(&mut exheader, at + 8, size);
        }
        put32(&mut exheader, 0x1C, 0x1000);
        put32(&mut exheader, 0x1C0, save_size);
        exheader[0x20F] = 0x30;

        let mut exefs = vec![0u8; 0x200];
        exefs[..5].copy_from_slice(b".code");
        let mut program = code.to_vec();
        program.resize(pages as usize * 0x1000, 0);
        put32(&mut exefs, 12, program.len() as u32);
        exefs.extend_from_slice(&program);

        let mut ivfc = vec![0u8; 0x1000];
        ivfc[..4].copy_from_slice(b"IVFC");
        put32(&mut ivfc, 0x08, 0x20);
        put32(&mut ivfc, 0x44, romfs_len as u32);
        put32(&mut ivfc, 0x4C, 12);
        ivfc.resize((0x1000 + romfs_len).div_ceil(0x200) * 0x200, 0);

        let exefs_unit = 3u32;
        put32(&mut ncch, 0x1A0, exefs_unit);
        put32(&mut ncch, 0x1A4, exefs.len() as u32 / 0x200);
        let romfs_unit = exefs_unit + exefs.len() as u32 / 0x200;
        put32(&mut ncch, 0x1B0, romfs_unit);
        put32(&mut ncch, 0x1B4, ivfc.len() as u32 / 0x200);
        ncch.extend_from_slice(&exheader);
        ncch.extend_from_slice(&exefs);
        ncch.extend_from_slice(&ivfc);
        put32(&mut image, 0x124, ncch.len() as u32 / 0x200);
        image.extend_from_slice(&ncch);
        image
    }

    /// `svc 0x03`: the program exits at once.
    #[cfg(feature = "ctr")]
    const EXIT: [u8; 4] = 0xEF00_0003u32.to_le_bytes();

    #[cfg(feature = "ctr")]
    #[test]
    fn a_card_image_is_read_where_it_lies() {
        // Sixteen megabytes of RomFS in an image that says it is 4 GB.
        let (huge, bytes_read) = Huge::new(card(&EXIT, 0, 16 << 20));
        let mut system = load_media(Box::new(huge), &LoadOptions::default()).unwrap();
        assert_eq!(system.name(), "3ds");
        assert!(system.info().contains("CTR-P-TEST"), "{}", system.info());
        assert!(!system.battery_backed(), "no save data in the exheader");
        assert!(
            bytes_read.get() < 64 << 10,
            "read {} bytes to start the game",
            bytes_read.get()
        );
        system.run_frame();

        // The same image held in memory, as the browser has it.
        let system = load(card(&EXIT, 0, 0x200)).unwrap();
        assert_eq!(system.name(), "3ds");

        let firm = ctr_fs::firm::build(0, 0x0800_6000, &[(0x0800_6000, &[0; 4])]);
        let system = load_media(Box::new(firm), &LoadOptions::default()).unwrap();
        assert_eq!(system.name(), "3ds");
    }

    /// What a frontend does with a battery-backed 3DS title: it writes
    /// `save_data` to a file and hands it back with `load_data` on the next
    /// start.
    #[cfg(feature = "ctr")]
    #[test]
    fn the_saves_of_a_card_title_survive_a_restart() {
        let image = card(&EXIT, 0x8000, 0x200);
        let mut first = load(image.clone()).unwrap();
        assert!(first.battery_backed(), "the exheader asks for save data");
        assert!(first.save_data().is_empty(), "nothing saved yet");

        // What the game's fs:USER calls leave behind.
        let mut store = ctr_hle::memfs::Store::default();
        store.format("save", ctr_hle::memfs::Format::default());
        store.create_file("save", "/main", 5).unwrap();
        store.write("save", "/main", 0, b"crabs").unwrap();
        first.load_data(&store.encode());
        let written = first.save_data();
        assert_eq!(written, store.encode());

        let mut second = load(image).unwrap();
        second.load_data(&written);
        assert_eq!(second.save_data(), written);
        // Garbage is ignored, not loaded.
        second.load_data(b"not a save");
        assert_eq!(second.save_data(), written);
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
