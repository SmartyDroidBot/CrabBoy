//! A process's address space, and the bus its threads run on.
//!
//! There are no guest page tables in this mode: the emulator is the kernel,
//! so a flat table of 4 KB pages says where each virtual page lies in
//! physical memory and what may be done to it. The memory itself is the
//! low-level machine's [`PhysMem`], because the GPU works on physical
//! addresses and the windows a process has onto GPU-visible memory are fixed
//! (3dbrew, "Memory layout"):
//!
//! | Virtual | Physical | |
//! |---|---|---|
//! | 0x14000000 | 0x20000000 | linear heap, older applications |
//! | 0x30000000 | 0x20000000 | linear heap, newer applications |
//! | 0x1F000000 | 0x18000000 | VRAM |
//! | 0x1FF00000 | 0x1FF00000 | DSP memory |

use arm_core::{Abort, Bus, CpEffect, CpReg};
use ctr_core::bus::phys::{FCRAM_BASE, VRAM_BASE, VRAM_LEN};
use ctr_core::bus::PhysMem;

pub const PAGE: u32 = 0x1000;
const PAGES: usize = 1 << 20;

/// Where a process's code starts.
pub const CODE_BASE: u32 = 0x0010_0000;
/// The heap a process grows with `ControlMemory`.
pub const HEAP_BASE: u32 = 0x0800_0000;
/// The top of the main thread's stack.
pub const STACK_TOP: u32 = 0x1000_0000;
/// The two virtual homes of the linear heap.
pub const LINEAR_BASE_OLD: u32 = 0x1400_0000;
pub const LINEAR_BASE_NEW: u32 = 0x3000_0000;
/// VRAM as a process sees it.
pub const VRAM_VADDR: u32 = 0x1F00_0000;
/// Thread-local storage: 0x200 bytes a thread, in pages from here.
pub const TLS_BASE: u32 = 0x1FF8_2000;
pub const TLS_LEN: u32 = 0x200;

/// What may be done to a page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Perm(pub u8);

impl Perm {
    pub const NONE: Perm = Perm(0);
    pub const R: Perm = Perm(1);
    pub const W: Perm = Perm(2);
    pub const X: Perm = Perm(4);
    pub const RW: Perm = Perm(3);
    pub const RX: Perm = Perm(5);

    fn allows(self, wanted: Perm) -> bool {
        self.0 & wanted.0 == wanted.0
    }
}

/// A page table entry: the physical page address, with the permissions in
/// the low bits and bit 3 marking it present.
#[derive(Clone, Copy, Default)]
struct Entry(u32);

impl Entry {
    const PRESENT: u32 = 8;

    fn new(pa: u32, perm: Perm) -> Self {
        Entry(pa & !(PAGE - 1) | Entry::PRESENT | perm.0 as u32)
    }

    fn resolve(self, va: u32, wanted: Perm) -> Option<u32> {
        (self.0 & Entry::PRESENT != 0 && Perm(self.0 as u8 & 7).allows(wanted))
            .then_some(self.0 & !(PAGE - 1) | va & (PAGE - 1))
    }
}

/// Physical pages of the application's part of FCRAM, handed out in order:
/// ordinary memory from the top down, the linear heap from the bottom up, so
/// that the linear heap stays one run of physical memory.
pub struct FcramAllocator {
    /// First page not yet given to the linear heap, as an offset into FCRAM.
    low: u32,
    /// First page given to ordinary memory; everything below is free.
    high: u32,
}

impl FcramAllocator {
    /// An application region of `len` bytes at the start of FCRAM.
    pub fn new(len: u32) -> Self {
        FcramAllocator { low: 0, high: len }
    }

    /// `len` bytes (a whole number of pages) of ordinary memory.
    pub fn take(&mut self, len: u32) -> Option<u32> {
        let start = self.high.checked_sub(len).filter(|&s| s >= self.low)?;
        self.high = start;
        Some(FCRAM_BASE + start)
    }

    /// `len` bytes at the end of the linear heap.
    pub fn take_linear(&mut self, len: u32) -> Option<u32> {
        let end = self.low.checked_add(len).filter(|&e| e <= self.high)?;
        let start = self.low;
        self.low = end;
        Some(FCRAM_BASE + start)
    }

    pub fn free_bytes(&self) -> u32 {
        self.high - self.low
    }
}

pub struct AddressSpace {
    pages: Vec<Entry>,
    pub fcram: FcramAllocator,
}

impl AddressSpace {
    /// An empty address space with VRAM mapped, over an application region
    /// of `app_len` bytes.
    pub fn new(app_len: u32) -> Self {
        let mut space = AddressSpace {
            pages: vec![Entry::default(); PAGES],
            fcram: FcramAllocator::new(app_len),
        };
        space.map(VRAM_VADDR, VRAM_BASE, VRAM_LEN as u32, Perm::RW);
        space
    }

    /// Map `len` bytes at `va` onto physical memory at `pa`. All three are
    /// whole pages; whatever was mapped there is replaced.
    pub fn map(&mut self, va: u32, pa: u32, len: u32, perm: Perm) {
        for n in 0..len.div_ceil(PAGE) {
            let index = (va / PAGE + n) as usize;
            if let Some(entry) = self.pages.get_mut(index) {
                *entry = Entry::new(pa + n * PAGE, perm);
            }
        }
    }

    pub fn unmap(&mut self, va: u32, len: u32) {
        for n in 0..len.div_ceil(PAGE) {
            if let Some(entry) = self.pages.get_mut((va / PAGE + n) as usize) {
                *entry = Entry::default();
            }
        }
    }

    /// Allocate `len` bytes of ordinary memory and map them at `va`.
    pub fn allocate(&mut self, va: u32, len: u32, perm: Perm) -> Option<u32> {
        let len = len.div_ceil(PAGE) * PAGE;
        let pa = self.fcram.take(len)?;
        self.map(va, pa, len, perm);
        Some(pa)
    }

    /// The physical address behind `va`, if the page allows `wanted`.
    pub fn resolve(&self, va: u32, wanted: Perm) -> Option<u32> {
        self.pages[(va / PAGE) as usize].resolve(va, wanted)
    }

    /// The physical address behind `va`, whatever its permissions: what the
    /// kernel and the GPU use.
    pub fn v2p(&self, va: u32) -> Option<u32> {
        self.resolve(va, Perm::NONE)
    }

    /// Copy out of the process, as the kernel: permissions are not checked,
    /// and the range may cross pages that are not adjacent physically.
    pub fn read_bytes(&self, mem: &PhysMem, va: u32, len: usize) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        let mut at = va;
        while out.len() < len {
            let run = ((PAGE - at % PAGE) as usize).min(len - out.len());
            out.extend_from_slice(mem.slice(self.v2p(at)?, run)?);
            at = at.wrapping_add(run as u32);
        }
        Some(out)
    }

    /// Copy into the process, as the kernel.
    pub fn write_bytes(&self, mem: &mut PhysMem, va: u32, bytes: &[u8]) -> Option<()> {
        let mut at = va;
        let mut done = 0;
        while done < bytes.len() {
            let run = ((PAGE - at % PAGE) as usize).min(bytes.len() - done);
            mem.slice_mut(self.v2p(at)?, run)?
                .copy_from_slice(&bytes[done..done + run]);
            at = at.wrapping_add(run as u32);
            done += run;
        }
        Some(())
    }

    pub fn read32(&self, mem: &PhysMem, va: u32) -> Option<u32> {
        let b = self.read_bytes(mem, va, 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn write32(&self, mem: &mut PhysMem, va: u32, value: u32) -> Option<()> {
        self.write_bytes(mem, va, &value.to_le_bytes())
    }
}

/// The physical word each core has marked for exclusive access, and the
/// address of the latest access that faulted.
#[derive(Default)]
pub struct BusState {
    exclusive: [Option<u32>; 2],
    pub fault_address: u32,
}

/// What a thread sees for the duration of a step.
pub struct HleBus<'a> {
    pub space: &'a AddressSpace,
    pub mem: &'a mut PhysMem,
    pub state: &'a mut BusState,
    pub core: usize,
    /// The running thread's thread-local storage.
    pub tls: u32,
}

impl HleBus<'_> {
    /// Horizon runs applications with unaligned access allowed, so an
    /// access may straddle two pages that are apart physically; only then is
    /// it done a byte at a time.
    fn read<const N: usize>(&mut self, va: u32, wanted: Perm) -> Result<[u8; N], Abort> {
        let mut out = [0u8; N];
        let fits = (va % PAGE) as usize + N <= PAGE as usize;
        for (n, byte) in out.iter_mut().enumerate().take(if fits { 1 } else { N }) {
            let at = va.wrapping_add(n as u32);
            let len = if fits { N } else { 1 };
            let bytes = self
                .space
                .resolve(at, wanted)
                .and_then(|pa| self.mem.slice(pa, len));
            match bytes {
                Some(bytes) if fits => {
                    out.copy_from_slice(bytes);
                    break;
                }
                Some(bytes) => *byte = bytes[0],
                None => {
                    self.state.fault_address = at;
                    return Err(Abort);
                }
            }
        }
        Ok(out)
    }

    fn write<const N: usize>(&mut self, va: u32, bytes: [u8; N]) -> Result<(), Abort> {
        // Both ends are checked before anything is written.
        let last = va.wrapping_add(N as u32 - 1);
        let ends = [va, last].map(|at| self.space.resolve(at, Perm::W).ok_or(at));
        let [first_pa, last_pa] = match ends {
            [Ok(first), Ok(last)] => [first, last],
            [Err(at), _] | [_, Err(at)] => {
                self.state.fault_address = at;
                return Err(Abort);
            }
        };
        // Any store ends the other core's exclusive access to its words.
        let other = &mut self.state.exclusive[1 - self.core];
        if *other == Some(first_pa & !3) || *other == Some(last_pa & !3) {
            *other = None;
        }
        let fits = (va % PAGE) as usize + N <= PAGE as usize;
        let stored = if fits {
            self.mem
                .slice_mut(first_pa, N)
                .map(|target| target.copy_from_slice(&bytes))
        } else {
            bytes.into_iter().enumerate().try_for_each(|(n, byte)| {
                let pa = self.space.resolve(va.wrapping_add(n as u32), Perm::W)?;
                self.mem.slice_mut(pa, 1).map(|target| target[0] = byte)
            })
        };
        stored.ok_or_else(|| {
            self.state.fault_address = va;
            Abort
        })
    }

    fn word_of(&self, va: u32) -> u32 {
        self.space.v2p(va).unwrap_or(va) & !3
    }
}

impl Bus for HleBus<'_> {
    fn fetch16(&mut self, addr: u32, _privileged: bool) -> Result<u16, Abort> {
        self.read::<2>(addr, Perm::X).map(u16::from_le_bytes)
    }
    fn fetch32(&mut self, addr: u32, _privileged: bool) -> Result<u32, Abort> {
        self.read::<4>(addr, Perm::X).map(u32::from_le_bytes)
    }

    fn read8(&mut self, addr: u32, _privileged: bool) -> Result<u8, Abort> {
        self.read::<1>(addr, Perm::R).map(|b| b[0])
    }
    fn read16(&mut self, addr: u32, _privileged: bool) -> Result<u16, Abort> {
        self.read::<2>(addr, Perm::R).map(u16::from_le_bytes)
    }
    fn read32(&mut self, addr: u32, _privileged: bool) -> Result<u32, Abort> {
        self.read::<4>(addr, Perm::R).map(u32::from_le_bytes)
    }

    fn write8(&mut self, addr: u32, value: u8, _privileged: bool) -> Result<(), Abort> {
        self.write(addr, [value])
    }
    fn write16(&mut self, addr: u32, value: u16, _privileged: bool) -> Result<(), Abort> {
        self.write(addr, value.to_le_bytes())
    }
    fn write32(&mut self, addr: u32, value: u32, _privileged: bool) -> Result<(), Abort> {
        self.write(addr, value.to_le_bytes())
    }

    /// User code reads one coprocessor register: the address of its
    /// thread-local storage.
    fn coproc_read(&mut self, reg: CpReg, _privileged: bool) -> Option<u32> {
        (reg.cp == 15 && (reg.opc1, reg.crn, reg.crm, reg.opc2) == (0, 13, 0, 3))
            .then_some(self.tls)
    }

    /// The cache and barrier operations user code may issue do nothing here.
    fn coproc_write(&mut self, reg: CpReg, _value: u32, _privileged: bool) -> Option<CpEffect> {
        (reg.cp == 15 && reg.crn == 7).then_some(CpEffect::None)
    }

    fn unaligned_access(&self) -> bool {
        true
    }

    fn hle(&self) -> bool {
        true
    }

    fn vfp_access(&self, _privileged: bool) -> bool {
        true
    }

    fn exclusive_load(&mut self, addr: u32) {
        self.state.exclusive[self.core] = Some(self.word_of(addr));
    }

    fn exclusive_store(&mut self, addr: u32) -> bool {
        let word = self.word_of(addr);
        self.state.exclusive[self.core].take() == Some(word)
    }

    fn exclusive_clear(&mut self) {
        self.state.exclusive[self.core] = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_map_with_permissions_and_the_kernel_sees_through_them() {
        let mut mem = PhysMem::new();
        let mut space = AddressSpace::new(64 << 20);
        let pa = space.allocate(CODE_BASE, 0x1800, Perm::RX).unwrap();
        assert_eq!(
            pa,
            FCRAM_BASE + (64 << 20) - 0x2000,
            "two pages from the top"
        );
        assert_eq!(
            space.resolve(CODE_BASE + 0x1234, Perm::R),
            Some(pa + 0x1234)
        );
        assert_eq!(space.resolve(CODE_BASE, Perm::W), None);
        assert_eq!(space.resolve(CODE_BASE + 0x2000, Perm::R), None);

        // The kernel writes code the process may only read and run.
        space
            .write_bytes(&mut mem, CODE_BASE + 0xFFE, &[1, 2, 3, 4])
            .unwrap();
        assert_eq!(space.read32(&mem, CODE_BASE + 0xFFE), Some(0x0403_0201));
        space.unmap(CODE_BASE, 0x1000);
        assert_eq!(space.v2p(CODE_BASE), None);
        assert!(space.v2p(CODE_BASE + 0x1000).is_some());
    }

    #[test]
    fn the_linear_heap_is_one_run_and_meets_ordinary_memory() {
        let mut fcram = FcramAllocator::new(0x4000);
        assert_eq!(fcram.take_linear(0x1000), Some(FCRAM_BASE));
        assert_eq!(fcram.take_linear(0x1000), Some(FCRAM_BASE + 0x1000));
        assert_eq!(fcram.take(0x1000), Some(FCRAM_BASE + 0x3000));
        assert_eq!(fcram.free_bytes(), 0x1000);
        assert_eq!(fcram.take(0x2000), None);
        assert_eq!(fcram.take_linear(0x2000), None);
    }

    #[test]
    fn the_bus_checks_permissions_and_records_the_faulting_address() {
        let mut mem = PhysMem::new();
        let mut space = AddressSpace::new(64 << 20);
        space.allocate(CODE_BASE, 0x1000, Perm::RX);
        space.allocate(HEAP_BASE, 0x1000, Perm::RW);
        let mut state = BusState::default();
        let mut bus = HleBus {
            space: &space,
            mem: &mut mem,
            state: &mut state,
            core: 0,
            tls: TLS_BASE,
        };
        bus.write32(HEAP_BASE + 8, 0xDEAD_BEEF, false).unwrap();
        assert_eq!(bus.read16(HEAP_BASE + 10, false), Ok(0xDEAD));
        assert_eq!(
            bus.fetch32(HEAP_BASE, false),
            Err(Abort),
            "data is not code"
        );
        assert_eq!(bus.write8(CODE_BASE + 5, 1, false), Err(Abort));
        assert_eq!(bus.state.fault_address, CODE_BASE + 5);
        assert_eq!(bus.read32(0x0400_0000, false), Err(Abort));
        // VRAM is there from the start.
        bus.write32(VRAM_VADDR + 0x30_0000, 0x11, false).unwrap();
        assert_eq!(bus.mem.slice(VRAM_BASE + 0x30_0000, 1), Some(&[0x11][..]));

        let tls = CpReg {
            cp: 15,
            opc1: 0,
            crn: 13,
            crm: 0,
            opc2: 3,
        };
        assert_eq!(bus.coproc_read(tls, false), Some(TLS_BASE));
        assert_eq!(bus.coproc_read(CpReg { opc2: 2, ..tls }, false), None);
    }

    #[test]
    fn a_store_by_the_other_core_ends_exclusive_access() {
        let mut mem = PhysMem::new();
        let mut space = AddressSpace::new(64 << 20);
        space.allocate(HEAP_BASE, 0x1000, Perm::RW);
        let mut state = BusState::default();
        let mut on = |core: usize, state: &mut BusState, f: &mut dyn FnMut(&mut HleBus)| {
            f(&mut HleBus {
                space: &space,
                mem: &mut mem,
                state,
                core,
                tls: 0,
            })
        };
        on(0, &mut state, &mut |bus| bus.exclusive_load(HEAP_BASE));
        on(1, &mut state, &mut |bus| {
            bus.write16(HEAP_BASE + 2, 1, false).unwrap()
        });
        on(0, &mut state, &mut |bus| {
            assert!(!bus.exclusive_store(HEAP_BASE))
        });
        on(0, &mut state, &mut |bus| bus.exclusive_load(HEAP_BASE));
        on(0, &mut state, &mut |bus| {
            bus.write32(HEAP_BASE, 1, false).unwrap()
        });
        on(0, &mut state, &mut |bus| {
            assert!(bus.exclusive_store(HEAP_BASE))
        });
    }
}
