//! From an executable image to what a process is made of: segments and where
//! they go, the main thread's stack and priority, the size of the memory
//! region, and where the program's RomFS lies on the medium.
//!
//! Homebrew (3DSX) carries little of this and gets the defaults of an
//! application. A game image (NCSD, or a bare NCCH) has it all in its
//! extended header; its RomFS stays on the medium and is read as the program
//! asks, so an image of gigabytes costs a few megabytes of memory.

use crate::memory::{Perm, CODE_BASE, LINEAR_BASE_NEW, LINEAR_BASE_OLD, PAGE};
use ctr_fs::ncch::{self, Exheader, Ncch, Ncsd};
use ctr_fs::{ImageKind, ThreeDsx};
use emu_core::{Storage, Window};

/// The main thread's stack when the image does not say (3DSX).
const DEFAULT_STACK: u32 = 0x4000;
/// The main thread's priority when the image does not say.
const DEFAULT_PRIORITY: u8 = 0x30;
/// The first kernel with the linear heap at 0x30000000 for applications
/// (8.0.0, kernel 2.44).
const NEW_LINEAR_KERNEL: (u8, u8) = (2, 44);

/// A program, ready to be put into a process.
pub struct Program {
    /// Address, contents (zero-filled to whole pages, bss included) and
    /// permissions of each segment.
    pub segments: Vec<(u32, Vec<u8>, Perm)>,
    pub entry: u32,
    pub stack_size: u32,
    pub priority: u8,
    /// The Old 3DS memory mode, which sets the application's region size.
    pub system_mode: u8,
    pub app_memory: u32,
    pub linear_base: u32,
    pub program_id: u64,
    /// The size of its save data, from the exheader; 0 for homebrew.
    pub save_size: u64,
    pub title: String,
    pub info: String,
    /// IVFC level 3 of the RomFS, on the medium.
    pub romfs: Option<Window>,
    /// The ExeFS, on the medium.
    pub exefs: Option<Window>,
}

/// A homebrew executable, laid out at the start of the code region.
pub fn from_3dsx(image: &[u8]) -> Result<Program, String> {
    let file = ThreeDsx::parse(image).map_err(|e| e.to_string())?;
    let placed = file.place(CODE_BASE).map_err(|e| e.to_string())?;
    let perms = [Perm::RX, Perm::R, Perm::RW];
    let segments = placed
        .segments
        .into_iter()
        .zip(perms)
        .map(|((addr, bytes), perm)| (addr, bytes, perm))
        .collect();
    let romfs = file.romfs_offset.map(|offset| Window {
        base: offset as u64,
        len: (image.len() as u64).saturating_sub(offset as u64),
    });
    Ok(Program {
        segments,
        entry: placed.entry,
        stack_size: DEFAULT_STACK,
        priority: DEFAULT_PRIORITY,
        system_mode: 0,
        app_memory: 64 << 20,
        linear_base: LINEAR_BASE_OLD,
        program_id: 0,
        save_size: 0,
        title: String::new(),
        info: "3DS homebrew (3DSX), high-level mode".to_string(),
        romfs,
        exefs: None,
    })
}

fn read(
    media: &mut dyn Storage,
    window: Window,
    offset: u64,
    len: usize,
) -> Result<Vec<u8>, String> {
    window
        .read_vec(media, offset, len)
        .map_err(|e| format!("reading the image: {e}"))
}

/// A game image: a card image (NCSD), whose first partition is the program,
/// or a bare program content (NCCH).
pub fn from_media(media: &mut dyn Storage) -> Result<Program, String> {
    let whole = Window::whole(media);
    let header = read(media, whole, 0, ncch::HEADER_LEN)?;
    let (content, media_id) = match ImageKind::detect(&header) {
        Some(ImageKind::Ncsd) => {
            let ncsd = Ncsd::parse(&header).map_err(|e| e.to_string())?;
            let part = ncsd.partitions[0];
            let content = whole
                .sub(part.offset, part.len)
                .ok_or("the image's first partition lies outside it")?;
            (content, ncsd.media_id)
        }
        Some(ImageKind::Ncch) => (whole, 0),
        _ => return Err("not a 3DS game image".to_string()),
    };
    let ncch =
        Ncch::parse(&read(media, content, 0, ncch::HEADER_LEN)?).map_err(|e| e.to_string())?;
    if ncch.exheader_len == 0 {
        return Err(ncch::NcchError::NotExecutable.to_string());
    }
    let exheader = Exheader::parse(&read(
        media,
        content,
        ncch::HEADER_LEN as u64,
        ncch::EXHEADER_LEN,
    )?)
    .map_err(|e| e.to_string())?;

    // The program is `.code` in ExeFS.
    let exefs = content
        .sub(ncch.exefs.offset, ncch.exefs.len)
        .ok_or("the ExeFS lies outside the image")?;
    let files =
        ncch::exefs_files(&read(media, exefs, 0, ncch::HEADER_LEN)?).map_err(|e| e.to_string())?;
    let code_file = files
        .iter()
        .find(|f| f.name == ".code")
        .ok_or("the ExeFS has no .code")?;
    let mut code = read(
        media,
        exefs,
        code_file.offset as u64,
        code_file.size as usize,
    )?;
    if exheader.compressed {
        code = ncch::decompress(&code).map_err(|e| e.to_string())?;
    }
    let [text, rodata, mut data] =
        ncch::code_segments(&code, &exheader).map_err(|e| e.to_string())?;
    data.resize(
        data.len() + (exheader.bss_size.div_ceil(PAGE) * PAGE) as usize,
        0,
    );

    let romfs = if ncch.romfs.len == 0 {
        None
    } else {
        let region = content
            .sub(ncch.romfs.offset, ncch.romfs.len)
            .ok_or("the RomFS lies outside the image")?;
        let level3 =
            ncch::romfs_level3(&read(media, region, 0, 0x60)?).map_err(|e| e.to_string())?;
        Some(
            region
                .sub(level3.offset, level3.len)
                .ok_or("the RomFS is cut short")?,
        )
    };

    let new_linear = exheader
        .kernel_version
        .is_some_and(|version| version >= NEW_LINEAR_KERNEL);
    Ok(Program {
        segments: vec![
            (exheader.text.address, text, Perm::RX),
            (exheader.rodata.address, rodata, Perm::R),
            (exheader.data.address, data, Perm::RW),
        ],
        entry: exheader.text.address,
        stack_size: exheader.stack_size.max(PAGE),
        priority: exheader.priority,
        system_mode: exheader.system_mode,
        app_memory: exheader.application_memory(),
        linear_base: if new_linear {
            LINEAR_BASE_NEW
        } else {
            LINEAR_BASE_OLD
        },
        program_id: exheader.program_id,
        save_size: exheader.save_data_size,
        title: exheader.title.clone(),
        info: format!(
            "3DS game {} (program {:016X}{}), high-level mode",
            ncch.product_code,
            ncch.program_id,
            if media_id != 0 { ", card image" } else { "" }
        ),
        romfs,
        exefs: Some(exefs),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn put32(image: &mut [u8], at: usize, value: u32) {
        image[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// A card image whose program is `code` at 0x100000, with a RomFS whose
    /// level 3 holds `romfs`.
    pub(crate) fn card(code: &[u8], romfs: &[u8]) -> Vec<u8> {
        let mut image = vec![0u8; 0x200];
        image[0x100..0x104].copy_from_slice(b"NCSD");
        // Partition 0 from media unit 1 (0x200).
        put32(&mut image, 0x120, 1);

        let mut ncch = vec![0u8; 0x200];
        ncch[0x100..0x104].copy_from_slice(b"NCCH");
        ncch[0x150..0x15A].copy_from_slice(b"CTR-P-TEST");
        put32(&mut ncch, 0x180, 0x400);
        ncch[0x18F] = 4;
        let mut exheader = vec![0u8; 0x400];
        exheader[..4].copy_from_slice(b"test");
        let pages = code.len().div_ceil(0x1000) as u32;
        for (at, addr, pages, size) in [
            (0x10, 0x10_0000, pages, code.len() as u32),
            (0x20, 0x10_0000 + pages * 0x1000, 0, 0),
            (0x30, 0x10_0000 + pages * 0x1000, 1, 4),
        ] {
            put32(&mut exheader, at, addr);
            put32(&mut exheader, at + 4, pages);
            put32(&mut exheader, at + 8, size);
        }
        put32(&mut exheader, 0x1C, 0x2000);
        put32(&mut exheader, 0x3C, 0x1800);
        exheader[0x20F] = 0x2C;
        put32(&mut exheader, 0x370, 0xFC00_0000 | 2 << 8 | 50);

        let mut exefs = vec![0u8; 0x200];
        exefs[..5].copy_from_slice(b".code");
        let mut program = code.to_vec();
        program.resize(pages as usize * 0x1000, 0);
        // The data segment's first word.
        program.extend_from_slice(&0xDA7Au32.to_le_bytes());
        put32(&mut exefs, 12, program.len() as u32);
        exefs.extend_from_slice(&program);
        exefs.resize(exefs.len().div_ceil(0x200) * 0x200, 0);

        let mut ivfc = vec![0u8; 0x1000];
        ivfc[..4].copy_from_slice(b"IVFC");
        put32(&mut ivfc, 0x08, 0x20);
        put32(&mut ivfc, 0x44, romfs.len() as u32);
        put32(&mut ivfc, 0x4C, 12);
        ivfc.extend_from_slice(romfs);
        ivfc.resize(ivfc.len().div_ceil(0x200) * 0x200, 0);

        // ExeFS after header and extended header, RomFS after the ExeFS.
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

    #[test]
    fn a_card_image_gives_the_program_its_layout_and_its_romfs() {
        let code = [0xEF00_0003u32.to_le_bytes(), [1, 2, 3, 4]].concat();
        let mut image = card(&code, b"level three");
        let program = from_media(&mut image).unwrap();
        assert_eq!(program.entry, 0x10_0000);
        assert_eq!(program.segments[0].0, 0x10_0000);
        assert_eq!(program.segments[0].1[..8], code[..]);
        assert_eq!(program.segments[2].0, 0x10_1000);
        assert_eq!(program.segments[2].1.len(), 0x1000 + 0x2000, "data and bss");
        assert_eq!(program.segments[2].1[..4], 0xDA7Au32.to_le_bytes());
        assert_eq!((program.stack_size, program.priority), (0x2000, 0x2C));
        assert_eq!(program.linear_base, LINEAR_BASE_NEW, "kernel 2.50");
        assert_eq!(program.title, "test");
        assert!(program.info.contains("CTR-P-TEST"), "{}", program.info);
        let romfs = program.romfs.unwrap();
        assert_eq!(romfs.len, 11);
        assert_eq!(romfs.read_vec(&mut image, 0, 11).unwrap(), b"level three");
    }

    #[test]
    fn an_encrypted_image_is_refused_with_a_reason() {
        let mut image = card(&[0; 4], b"");
        image[0x200 + 0x18F] = 0;
        let error = from_media(&mut image).err().unwrap();
        assert!(error.contains("decrypted"), "{error}");
        assert!(from_media(&mut vec![0u8; 0x400]).is_err());
    }
}
