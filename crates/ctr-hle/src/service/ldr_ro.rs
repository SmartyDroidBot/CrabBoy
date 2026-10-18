//! The CRO loader, `ldr:ro` (3dbrew, "RO Services", "CRO0").
//!
//! A program reads its CRS (`static.crs`, the symbols of the program itself)
//! and its CROs (its shared libraries) from its RomFS into its own heap,
//! then asks RO to map each one at an address of its choosing in the region
//! of the program's image (0x00100000-0x04000000). The mapping is an alias
//! of the heap pages, which are locked until the module is unloaded; the
//! module is then rebased and linked in place by `crate::cro`. Data and bss
//! go in buffers the program gives, and the program copies the module's
//! data there itself once the module is loaded.
//!
//! The CRR (the list of hashes of the modules allowed) is accepted without a
//! signature or hash check, as nothing here is signed.

use crate::cro::{self, Guest, Module};
use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::memory::{MemState, Perm, PAGE};
use crate::result::{self, ResultCode};
use crate::service::Service;
use crate::trace;
use std::collections::BTreeMap;

/// The region modules may be mapped in.
const IMAGE_START: u32 = 0x0010_0000;
const IMAGE_END: u32 = 0x0400_0000;

// RO's own result codes (module 75): 3dbrew's "RO Services" where it lists
// them, the rest as other emulators report them.
const MISALIGNED_ADDRESS: ResultCode = ResultCode(0xD901_2FF1);
const MISALIGNED_SIZE: ResultCode = ResultCode(0xD901_2FF2);
const ALREADY_INITIALIZED: ResultCode = ResultCode(0xE0A1_2FF9);
const NOT_INITIALIZED: ResultCode = ResultCode(0xD961_2FF8);
const INVALID_MEMORY_STATE: ResultCode = ResultCode(0xD8A1_2C08);
const NOT_LOADED: ResultCode = ResultCode(0xD8A1_2C0D);
const ILLEGAL_ADDRESS: ResultCode = ResultCode(0xE161_2C0F);
const NOT_ZERO: ResultCode = ResultCode(0xE161_2C1D);

/// Where a loaded module came from: the program's buffer, and the size it
/// had before the fix gave part of it back.
#[derive(Clone, Copy, Debug)]
struct Loaded {
    buffer: u32,
    size: u32,
}

#[derive(Default)]
pub struct LdrRo {
    /// The CRS once `Initialize` has mapped it: its address, buffer and
    /// size.
    crs: Option<(u32, u32, u32)>,
    /// The modules loaded, by address.
    loaded: BTreeMap<u32, Loaded>,
}

/// The service as `service::standard` registers it.
pub fn service() -> Box<dyn Service> {
    Box::<LdrRo>::default()
}

/// The checks every mapping request goes through: page alignment, a
/// buffer of private read/write memory, a place inside the image region.
fn check_mapping(h: &Horizon, buffer: u32, address: u32, size: u32) -> Result<(), ResultCode> {
    if size < cro::HEADER_LEN {
        return Err(cro::BUFFER_TOO_SMALL);
    }
    if !buffer.is_multiple_of(PAGE) || !address.is_multiple_of(PAGE) {
        return Err(MISALIGNED_ADDRESS);
    }
    if !size.is_multiple_of(PAGE) {
        return Err(MISALIGNED_SIZE);
    }
    if !h
        .process
        .space
        .is_all(buffer, size, Perm::RW, MemState::Private)
    {
        return Err(INVALID_MEMORY_STATE);
    }
    if address < IMAGE_START || address.checked_add(size).is_none_or(|end| end > IMAGE_END) {
        return Err(ILLEGAL_ADDRESS);
    }
    Ok(())
}

/// Map the program's buffer at `address`, read-only, as code.
fn map(h: &mut Horizon, address: u32, buffer: u32, size: u32) -> Result<(), ResultCode> {
    if address == buffer || h.process.space.alias(address, buffer, size, Perm::R, true) {
        Ok(())
    } else {
        Err(INVALID_MEMORY_STATE)
    }
}

fn unmap(h: &mut Horizon, address: u32, buffer: u32, size: u32) {
    if address != buffer {
        h.process.space.unalias(address, buffer, size);
    }
}

fn guest(h: &mut Horizon) -> Guest<'_> {
    Guest {
        space: &h.process.space,
        mem: &mut h.mem,
    }
}

impl LdrRo {
    fn crs(&self) -> Result<Module, ResultCode> {
        self.crs
            .map(|(address, _, _)| Module(address))
            .ok_or(NOT_INITIALIZED)
    }

    /// Initialize(buffer, size, address, 0, process): map and rebase the
    /// CRS.
    fn initialize(&mut self, h: &mut Horizon, request: &Request) -> Result<(), ResultCode> {
        let (buffer, size, address) = (request.word(1), request.word(2), request.word(3));
        if self.crs.is_some() {
            return Err(ALREADY_INITIALIZED);
        }
        check_mapping(h, buffer, address, size)?;
        map(h, address, buffer, size)?;
        let crs = Module(address);
        let m = &mut guest(h);
        crs.set(m, cro::field::NEXT, 0);
        crs.set(m, cro::field::PREVIOUS, 0);
        if let Err(code) = crs.rebase(m, crs, size, (0, 0), (0, 0), true) {
            unmap(h, address, buffer, size);
            return Err(code);
        }
        self.crs = Some((address, buffer, size));
        if h.trace & trace::RO != 0 {
            let line = format!(
                "ldr:ro: the CRS is at {address:#010x} ({size:#x} bytes from {buffer:#010x})"
            );
            h.note(line);
        }
        Ok(())
    }

    /// LoadCRO and LoadCRO_New(buffer, address, size, data, 0, data size,
    /// bss, bss size, auto-link, fix level, CRR, 0, process): map, rebase,
    /// link and register a module, give back what the fix level lets go and
    /// make its text executable. Returns the size kept.
    fn load(&mut self, h: &mut Horizon, request: &Request) -> Result<u32, ResultCode> {
        let crs = self.crs()?;
        let [buffer, address, size, data, zero, data_size, bss, bss_size, auto_link, level] =
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10].map(|n| request.word(n));
        check_mapping(h, buffer, address, size)?;
        if zero != 0 {
            return Err(NOT_ZERO);
        }
        map(h, address, buffer, size)?;
        let module = Module(address);
        let fix_data = request.command() == 0x9;
        let linked = {
            let m = &mut guest(h);
            module
                .rebase(m, crs, size, (data, data_size), (bss, bss_size), false)
                .and_then(|()| module.link(m, crs, fix_data))
        };
        if let Err(code) = linked {
            unmap(h, address, buffer, size);
            return Err(code);
        }
        let m = &mut guest(h);
        module.register(m, crs, auto_link & 0xFF != 0);
        let kept = module.fix(m, level).min(size);
        let pages = module.executable_pages(m);
        let name = module.name(m);
        let (named, others) = module.unresolved(m);
        if kept < size {
            unmap(h, address + kept, buffer + kept, size - kept);
        }
        if let Some((start, len)) = pages {
            h.process.space.protect(start, len, Perm::RX);
        }
        self.loaded.insert(address, Loaded { buffer, size });
        if h.trace & trace::RO != 0 {
            let missing = if named.is_empty() && others == 0 {
                "everything resolved".to_string()
            } else {
                format!(
                    "unresolved: {} named [{}], {others} by index or position",
                    named.len(),
                    named.join(", ")
                )
            };
            let line = format!(
                "ldr:ro: loaded \"{name}\" at {address:#010x} (data {data:#010x}, bss \
                 {bss:#010x}, fix level {level}, {kept:#x} of {size:#x} bytes kept); {missing}"
            );
            h.note(line);
        }
        Ok(kept)
    }

    /// UnloadCRO(address, 0, buffer, 0, process): undo everything `load`
    /// did. The part the fix gave back is mapped again for the while, so
    /// that an unfixed module's image returns to its bytes as loaded.
    fn unload(&mut self, h: &mut Horizon, request: &Request) -> Result<(), ResultCode> {
        let crs = self.crs()?;
        let (address, buffer) = (request.word(1), request.word(3));
        let module = self.loaded_module(h, address)?;
        let kept = module.get(&guest(h), cro::field::FIXED_SIZE);
        let size = match self.loaded.get(&address) {
            Some(loaded) if loaded.buffer == buffer && loaded.size > kept => {
                let tail = (address + kept, buffer + kept, loaded.size - kept);
                if map(h, tail.0, tail.1, tail.2).is_ok() {
                    loaded.size
                } else {
                    kept
                }
            }
            _ => kept,
        };
        let m = &mut guest(h);
        let name = module.name(m);
        module.unregister(m, crs);
        module.unlink(m, crs)?;
        if !module.is_fixed(m) {
            module.clear_relocations(m)?;
        }
        module.unrebase(m, false);
        unmap(h, address, buffer, size);
        self.loaded.remove(&address);
        if h.trace & trace::RO != 0 {
            h.note(format!("ldr:ro: unloaded \"{name}\" from {address:#010x}"));
        }
        Ok(())
    }

    /// The module at `address`, if one is loaded there.
    fn loaded_module(&self, h: &mut Horizon, address: u32) -> Result<Module, ResultCode> {
        if !address.is_multiple_of(PAGE) {
            return Err(MISALIGNED_ADDRESS);
        }
        let module = Module(address);
        if !module.is_loaded(&guest(h)) {
            return Err(NOT_LOADED);
        }
        Ok(module)
    }

    /// LinkCRO and UnlinkCRO(address, 0, process).
    fn relink(&mut self, h: &mut Horizon, request: &Request, link: bool) -> Result<(), ResultCode> {
        let crs = self.crs()?;
        let module = self.loaded_module(h, request.word(1))?;
        let m = &mut guest(h);
        if link {
            module.link(m, crs, false)
        } else {
            module.unlink(m, crs)
        }
    }

    /// Shutdown(buffer, 0, process): the CRS goes.
    fn shutdown(&mut self, h: &mut Horizon) -> Result<(), ResultCode> {
        let crs = self.crs()?;
        let (address, buffer, size) = self.crs.take().expect("initialized");
        crs.unrebase(&mut guest(h), true);
        unmap(h, address, buffer, size);
        Ok(())
    }
}

impl Service for LdrRo {
    fn name(&self) -> &str {
        "ldr:ro"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let outcome = match command {
            0x1 => self.initialize(h, request).map(|()| None),
            // LoadCRR and UnloadCRR: nothing is signed, so every module is
            // allowed.
            0x2 | 0x3 => {
                if h.trace & trace::RO != 0 {
                    let what = if command == 2 { "accepted" } else { "dropped" };
                    h.note(format!("ldr:ro: CRR at {:#010x} {what}", request.word(1)));
                }
                Ok(None)
            }
            0x4 | 0x9 => self.load(h, request).map(Some),
            0x5 => self.unload(h, request).map(|()| None),
            0x6 | 0x7 => self.relink(h, request, command == 0x6).map(|()| None),
            0x8 => self.shutdown(h).map(|()| None),
            _ => return h.unhandled("ldr:ro", request),
        };
        match outcome {
            Ok(None) => Reply::new(command, result::SUCCESS),
            Ok(Some(size)) => Reply::new(command, result::SUCCESS).word(size),
            Err(code) => {
                h.note(format!(
                    "ldr:ro command {command:#06x} failed with {:#010x} ({:#x?})",
                    code.0,
                    &request.words[1..]
                ));
                let reply = Reply::new(command, code);
                if matches!(command, 0x4 | 0x9) {
                    reply.word(0)
                } else {
                    reply
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cro::testing::{build, Patch, Spec};
    use crate::cro::{reloc, segment, tag};
    use crate::memory::HEAP_BASE;
    use ctr_fs::threedsx;

    const CRS_AT: u32 = 0x0020_0000;
    const LIB_AT: u32 = 0x0030_0000;
    const LIB_BUFFER: u32 = HEAP_BASE + 0x10_0000;
    const DATA: u32 = HEAP_BASE + 0x20_0000;

    /// A console whose program has a page each of text, read-only data
    /// and data, and the CRS and a library in its heap.
    fn console() -> (Horizon, Vec<u8>, Vec<u8>) {
        let code = 0xEF00_0003u32.to_le_bytes();
        let image = threedsx::build([&code, &[0; 4], &[0; 4]], 0, &Default::default());
        let mut h = Horizon::from_3dsx(&image).unwrap();
        let crs = build(&Spec {
            name: "|static|".into(),
            program: Some([
                (0x10_0000, 0x1000, segment::TEXT),
                (0x10_1000, 0x1000, segment::RODATA),
                (0x10_2000, 0x1000, segment::DATA),
                (0, 0, segment::BSS),
            ]),
            exports: vec![("crs_func".into(), tag(0, 0x10))],
            ..Spec::default()
        });
        let lib = build(&Spec {
            name: "Lib".into(),
            text: vec![0; 0x40],
            data: vec![0; 0x1000],
            imports: vec![(
                "crs_func".into(),
                vec![Patch {
                    target: tag(0, 0),
                    kind: reloc::ABS32,
                    addend: 0,
                }],
            )],
            internal: vec![(tag(2, 0), reloc::ABS32, 0, 4)],
            ..Spec::default()
        });
        for (at, bytes) in [(HEAP_BASE, &crs), (LIB_BUFFER, &lib)] {
            h.process.space.allocate(at, bytes.len() as u32, Perm::RW);
            h.process.space.write_bytes(&mut h.mem, at, bytes);
        }
        h.process.space.allocate(DATA, 2 * PAGE, Perm::RW);
        (h, crs, lib)
    }

    fn call(ro: &mut LdrRo, h: &mut Horizon, words: &[u32]) -> Vec<u32> {
        let request = Request {
            words: words.to_vec(),
        };
        ro.call(h, 0, &request).encode()
    }

    fn load_words(lib: &[u8], level: u32) -> Vec<u32> {
        let size = lib.len() as u32;
        let (data, bss) = (DATA, DATA + 0x1000);
        vec![
            0x0009_02C2,
            LIB_BUFFER,
            LIB_AT,
            size,
            data,
            0,
            0x1000,
            bss,
            0x100,
            1,
            level,
            0,
            0,
            0xFFFF_8001,
        ]
    }

    #[test]
    fn a_module_loads_links_and_unloads_to_the_bytes_it_came_as() {
        let (mut h, crs, lib) = console();
        let mut ro = LdrRo::default();
        assert_eq!(
            call(&mut ro, &mut h, &load_words(&lib, 0))[1],
            NOT_INITIALIZED.0
        );
        let size = crs.len() as u32;
        let init = [0x0001_00C2, HEAP_BASE, size, CRS_AT, 0, 0xFFFF_8001];
        assert_eq!(call(&mut ro, &mut h, &init), [0x0001_0040, 0]);
        assert_eq!(call(&mut ro, &mut h, &init)[1], ALREADY_INITIALIZED.0);
        let space = &h.process.space;
        assert_eq!(space.query(CRS_AT).state, MemState::AliasCode);
        assert!(space.is_all(HEAP_BASE, size, Perm::NONE, MemState::Locked));

        let reply = call(&mut ro, &mut h, &load_words(&lib, 0));
        assert_eq!(reply[..2], [0x0009_0080, 0]);
        assert!(reply[2] <= lib.len() as u32);
        let space = &h.process.space;
        let text = LIB_AT + 0x180;
        assert_eq!(space.read32(&h.mem, text), Some(0x10_0010), "imported");
        assert!(space.resolve(text, Perm::X).is_some(), "text runs");
        assert_eq!(space.resolve(text, Perm::W), None);
        // The data the program copies, relocated, in its buffer's tail.
        let data = Module(LIB_AT).get(&guest(&mut h), cro::field::DATA);
        assert_eq!(h.process.space.read32(&h.mem, data), Some(text + 4));

        let unload = [0x0005_00C2, LIB_AT, 0, LIB_BUFFER, 0, 0xFFFF_8001];
        assert_eq!(call(&mut ro, &mut h, &unload), [0x0005_0040, 0]);
        let space = &h.process.space;
        assert_eq!(space.v2p(LIB_AT), None);
        let back = space.read_bytes(&h.mem, LIB_BUFFER, lib.len()).unwrap();
        assert!(back == lib, "the image is as it was");
        assert!(space.is_all(LIB_BUFFER, lib.len() as u32, Perm::RW, MemState::Private));
        assert_eq!(call(&mut ro, &mut h, &unload)[1], NOT_LOADED.0);

        let shutdown = [0x0008_0042, HEAP_BASE, 0, 0xFFFF_8001];
        assert_eq!(call(&mut ro, &mut h, &shutdown), [0x0008_0040, 0]);
        let space = &h.process.space;
        assert_eq!(space.v2p(CRS_AT), None);
        let back = space.read_bytes(&h.mem, HEAP_BASE, crs.len()).unwrap();
        assert!(back == crs, "the CRS is as it was");
    }

    #[test]
    fn a_fixed_module_gives_back_its_tail_and_bad_requests_are_refused() {
        let (mut h, crs, lib) = console();
        let mut ro = LdrRo::default();
        let size = crs.len() as u32;
        let init = [0x0001_00C2, HEAP_BASE, size, CRS_AT, 0, 0xFFFF_8001];
        call(&mut ro, &mut h, &init);

        let mut words = load_words(&lib, 0);
        words[2] = LIB_AT + 0x10;
        assert_eq!(call(&mut ro, &mut h, &words)[1], MISALIGNED_ADDRESS.0);
        words[2] = IMAGE_END;
        assert_eq!(call(&mut ro, &mut h, &words)[1], ILLEGAL_ADDRESS.0);
        words[2] = CRS_AT;
        assert_eq!(
            call(&mut ro, &mut h, &words)[1],
            INVALID_MEMORY_STATE.0,
            "the CRS is there"
        );

        let reply = call(&mut ro, &mut h, &load_words(&lib, 3));
        assert_eq!(reply[1], 0);
        let kept = reply[2];
        assert!(kept < lib.len() as u32, "{kept:#x}");
        let space = &h.process.space;
        assert_eq!(space.v2p(LIB_AT + kept), None);
        assert!(space.is_all(LIB_BUFFER + kept, PAGE, Perm::RW, MemState::Private));
        assert!(space.is_all(LIB_BUFFER, kept, Perm::NONE, MemState::Locked));
        let unload = [0x0005_00C2, LIB_AT, 0, LIB_BUFFER, 0, 0xFFFF_8001];
        assert_eq!(call(&mut ro, &mut h, &unload)[1], 0);
        let space = &h.process.space;
        assert!(space.is_all(LIB_BUFFER, lib.len() as u32, Perm::RW, MemState::Private));
    }
}
