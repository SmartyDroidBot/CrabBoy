//! The GPU service, `gsp::Gpu` (3dbrew, "GSP Services", "GSP Shared Memory").
//!
//! An application drives the GPU through a page it shares with the service:
//! it queues GX commands there (command lists, memory fills, display
//! transfers, texture copies, DMA), states which framebuffers to show, and
//! reads the interrupts the service relays to it. The service carries the
//! commands out by writing the GPU's registers in `ctr_core::gpu::GpuExt`,
//! the same registers the low-level machine has, and turns each unit's
//! completion into an entry of the interrupt queue and a signal of the
//! application's event. There is one client, at index 0.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::kernel::ObjId;
use crate::result::{self, ResultCode};
use crate::service::Service;
use ctr_core::gpu::{GpuIrq, GpuJob};

/// The tag `Horizon` delivers at each vertical blank.
pub const VBLANK: u32 = 0x100;
const NAME: &str = "gsp::Gpu";
/// The shared page: interrupt queues from 0, framebuffer information from
/// 0x200 and GX command queues from 0x800, each by client.
const SHARED_LEN: u32 = 0x1000;
const FRAMEBUFFER_INFO: u32 = 0x200;
const COMMAND_QUEUE: u32 = 0x800;
const QUEUE_ENTRIES: u32 = 15;
/// The completions a GPU job can report, by interrupt number, then DMA.
const UNITS: [GpuIrq; 6] = [
    GpuIrq::Psc0,
    GpuIrq::Psc1,
    GpuIrq::Pdc0,
    GpuIrq::Pdc1,
    GpuIrq::Ppf,
    GpuIrq::P3d,
];
const DMA: u32 = 6;
/// A completion that reports no interrupt: the second unit of a memory fill
/// that uses both, as GSP only dispatches PSC0 then.
const SILENT: u32 = 0x10;
/// Room in the interrupt queue: PDC interrupts are dropped past 0x20 entries,
/// others past 0x34, the length of the list.
const PDC_ROOM: u8 = 0x20;
const LIST_LEN: u8 = 0x34;
/// Register addresses in requests are from 0x1EB00000; the GPU's block is at
/// 0x1EF00000, and the LCD fill colours at 0x1ED02204 and 0x1ED02A04.
const GPU_REGISTERS: u32 = 0x40_0000;
const REGISTERS_END: u32 = 0x42_0000;
const LCD_FILL: [u32; 2] = [0x20_2204, 0x20_2A04];
/// The first registration's result: success, "first initialization".
const FIRST_INITIALIZATION: ResultCode = ResultCode(0x0000_2A07);
const REGISTERS_OUT_OF_RANGE: ResultCode = ResultCode(0xE0E0_2A01);
const INVALID_ADDRESS: ResultCode = ResultCode(0xE0E0_2BF5);

#[derive(Default)]
pub struct Gsp {
    /// The shared page: its object and physical address.
    shared: Option<(ObjId, u32)>,
    /// The application's event for relayed interrupts.
    event: Option<ObjId>,
    registered: bool,
}

impl Gsp {
    /// Write a GPU register on the application's behalf, and schedule the
    /// completion of the work it starts.
    fn write(&self, h: &mut Horizon, offset: u32, value: u32, mask: u32, tag: u32) {
        if let Some(GpuJob { unit, cost }) = h.gpu.write(offset, value, mask, &mut h.mem) {
            h.schedule_service(NAME, cost, unit as u32 | tag);
        }
    }

    /// A write through WriteHWRegs, at an offset from 0x1EB00000.
    fn write_register(&self, h: &mut Horizon, offset: u32, value: u32, mask: u32) {
        if offset >= GPU_REGISTERS {
            self.write(h, offset - GPU_REGISTERS, value, mask, 0);
        } else if let Some(screen) = LCD_FILL.iter().position(|&at| at == offset) {
            let fill = &mut h.gpu.pdc[screen].fill;
            *fill = *fill & !mask | value & mask;
        }
    }

    /// Put interrupt `id` in the client's queue and signal its event.
    fn relay(&self, h: &mut Horizon, id: u8) {
        let Some((_, pa)) = self.shared else {
            return;
        };
        let Some(queue) = h.mem.slice_mut(pa, 0x40) else {
            return;
        };
        let pdc = id == 2 || id == 3;
        if pdc && queue[3] & 1 != 0 {
            return;
        }
        let count = queue[1];
        if count >= if pdc { PDC_ROOM } else { LIST_LEN } {
            if pdc {
                let at = if id == 2 { 4 } else { 8 };
                let missed = u32::from_le_bytes(queue[at..at + 4].try_into().unwrap());
                queue[at..at + 4].copy_from_slice(&missed.wrapping_add(1).to_le_bytes());
            } else {
                queue[2] = 1;
            }
        } else {
            let slot = (queue[0] as usize + count as usize) % LIST_LEN as usize;
            queue[0xC + slot] = id;
            queue[1] = count + 1;
        }
        if let Some(event) = self.event {
            h.kernel.signal(event);
        }
    }

    /// Run the GX commands the client has queued, in order, until none are
    /// left or one asks to stop after it.
    fn run_queue(&self, h: &mut Horizon) {
        let Some((_, pa)) = self.shared else {
            return;
        };
        let base = pa + COMMAND_QUEUE;
        loop {
            let Some(header) = h.mem.slice_mut(base, 8) else {
                return;
            };
            let (index, count) = (header[0] as u32, header[1]);
            if count == 0 || header[3] & 1 != 0 {
                return;
            }
            header[0] = ((index + 1) % QUEUE_ENTRIES) as u8;
            header[1] = count - 1;
            let Some(entry) = h.mem.slice(base + 0x20 + index * 0x20, 0x20) else {
                return;
            };
            let command: [u32; 8] = std::array::from_fn(|n| {
                u32::from_le_bytes(entry[n * 4..n * 4 + 4].try_into().unwrap())
            });
            let code = self.execute(h, &command);
            if code.is_error() {
                if let Some(header) = h.mem.slice_mut(base, 8) {
                    header[2] |= 0x80;
                    header[4..8].copy_from_slice(&code.0.to_le_bytes());
                }
            }
            if command[0] >> 16 & 1 != 0 {
                return;
            }
        }
    }

    /// The physical address of `va`, or 0 where GSP would use 0.
    fn physical(h: &Horizon, va: u32) -> u32 {
        h.process.space.v2p(va).unwrap_or(0)
    }

    /// Carry out one GX command.
    fn execute(&self, h: &mut Horizon, command: &[u32; 8]) -> ResultCode {
        match command[0] & 0xFF {
            // RequestDma(source, destination, size): the copy is done at
            // once and reported after one cycle (not measured).
            0x00 => {
                let (source, target, size) = (command[1], command[2], command[3]);
                if let Some(data) = h.process.space.read_bytes(&h.mem, source, size as usize) {
                    h.process.space.write_bytes(&mut h.mem, target, &data);
                }
                h.schedule_service(NAME, 1, DMA);
            }
            // ProcessCommandList(address, size).
            0x01 => {
                let (pa, size) = (Self::physical(h, command[1]), command[2]);
                self.write(h, 0x18E0, size >> 3, !0, 0);
                self.write(h, 0x18E8, pa >> 3, !0, 0);
                self.write(h, 0x18F0, 1, !0, 0);
            }
            // MemoryFill: start, value, end for each unit, then both
            // controls. A unit whose start is 0 is skipped.
            0x02 => {
                let both = command[1] != 0 && command[4] != 0;
                for unit in 0..2u32 {
                    let (start, value, end) = (
                        command[1 + unit as usize * 3],
                        command[2 + unit as usize * 3],
                        command[3 + unit as usize * 3],
                    );
                    if start == 0 {
                        continue;
                    }
                    let Some(pa) = h.process.space.v2p(start).filter(|_| start < end) else {
                        return INVALID_ADDRESS;
                    };
                    let control = command[7] >> (unit * 16) & 0xFFFF | 1;
                    let registers = 0x10 + unit * 0x10;
                    let tag = if both && unit == 1 { SILENT } else { 0 };
                    self.write(h, registers, pa >> 3, !0, 0);
                    self.write(h, registers + 4, (pa + (end - start)) >> 3, !0, 0);
                    self.write(h, registers + 8, value, !0, 0);
                    self.write(h, registers + 0xC, control, !0, tag);
                }
            }
            // DisplayTransfer(source, destination, source and output
            // dimensions, flags).
            0x03 => {
                let (input, output) =
                    (Self::physical(h, command[1]), Self::physical(h, command[2]));
                self.write(h, 0xC00, input >> 3, !0, 0);
                self.write(h, 0xC04, output >> 3, !0, 0);
                self.write(h, 0xC08, command[4], !0, 0);
                self.write(h, 0xC0C, command[3], !0, 0);
                self.write(h, 0xC10, command[5], !0, 0);
                self.write(h, 0xC14, 0, !0, 0);
                self.write(h, 0xC18, 1, !0, 0);
            }
            // TextureCopy(source, destination, size, input and output line
            // width and gap, flags).
            0x04 => {
                let (input, output) =
                    (Self::physical(h, command[1]), Self::physical(h, command[2]));
                self.write(h, 0xC00, input >> 3, !0, 0);
                self.write(h, 0xC04, output >> 3, !0, 0);
                self.write(h, 0xC20, command[3], !0, 0);
                self.write(h, 0xC24, command[4], !0, 0);
                self.write(h, 0xC28, command[5], !0, 0);
                self.write(h, 0xC10, command[6], !0, 0);
                self.write(h, 0xC18, 1, !0, 0);
            }
            // FlushCacheRegions: caches are not modelled.
            0x05 => {}
            other => h.note(format!("gsp::Gpu GX command {other:#x} is not implemented")),
        }
        result::SUCCESS
    }

    /// Show the framebuffers `info` describes on `screen`: the active one,
    /// the left and right addresses, stride and format.
    fn swap(&self, h: &mut Horizon, screen: u32, info: &[u32]) {
        let pdc = 0x400 + screen * 0x100;
        let active = info[0] & 1;
        let left = Self::physical(h, info[1]);
        self.write(h, pdc + 0x68 + active * 4, left, !0, 0);
        if screen == 0 {
            let right = Self::physical(h, info[2]);
            self.write(h, pdc + 0x94 + active * 4, right, !0, 0);
        }
        self.write(h, pdc + 0x90, info[3], !0, 0);
        self.write(h, pdc + 0x70, info[4], !0, 0);
        self.write(h, pdc + 0x78, active, 1, 0);
    }

    /// Take up framebuffer information the client has marked as new.
    fn update_framebuffers(&self, h: &mut Horizon) {
        let Some((_, pa)) = self.shared else {
            return;
        };
        for screen in 0..2 {
            let at = pa + FRAMEBUFFER_INFO + screen * 0x40;
            let Some(header) = h.mem.slice_mut(at, 4) else {
                return;
            };
            if header[1] & 1 == 0 {
                continue;
            }
            header[1] = 0;
            let entry = at + 4 + (header[0] as u32 & 1) * 0x1C;
            let Some(bytes) = h.mem.slice(entry, 0x1C) else {
                return;
            };
            let info: Vec<u32> = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|w| u32::from_le_bytes(*w))
                .collect();
            self.swap(h, screen, &info);
        }
    }

    /// The words of `len` bytes at `va` in the process.
    fn words(h: &Horizon, va: u32, len: u32) -> Vec<u32> {
        h.process
            .space
            .read_bytes(&h.mem, va, len as usize)
            .unwrap_or_default()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect()
    }
}

/// A register range a request may name: word-aligned, at most 0x80 bytes,
/// below 0x420000.
fn in_range(offset: u32, size: u32) -> bool {
    offset.is_multiple_of(4)
        && size.is_multiple_of(4)
        && size <= 0x80
        && offset
            .checked_add(size)
            .is_some_and(|end| end <= REGISTERS_END)
}

impl Service for Gsp {
    fn name(&self) -> &str {
        NAME
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        match command {
            // WriteHWRegs(offset, size, data), WriteHWRegsWithMask(offset,
            // size, data, mask), WriteHWRegRepeat(offset, size, data): the
            // last writes every word to the one register.
            0x01..=0x03 => {
                let (offset, size) = (request.word(1), request.word(2));
                if !in_range(offset, size) {
                    return Reply::new(command, REGISTERS_OUT_OF_RANGE);
                }
                let data = Self::words(h, request.word(4), size);
                let masks = if command == 0x02 {
                    Self::words(h, request.word(6), size)
                } else {
                    vec![!0; data.len()]
                };
                for (n, (&value, &mask)) in data.iter().zip(&masks).enumerate() {
                    let at = if command == 0x03 {
                        offset
                    } else {
                        offset + n as u32 * 4
                    };
                    self.write_register(h, at, value, mask);
                }
                ok()
            }
            // ReadHWRegs(offset, size), into static buffer 0.
            0x04 => {
                let (offset, size) = (request.word(1), request.word(2));
                if !in_range(offset, size) {
                    return Reply::new(command, REGISTERS_OUT_OF_RANGE);
                }
                let (addr, room) = h.static_buffer(0);
                let size = size.min(room);
                let data: Vec<u8> = (0..size / 4)
                    .flat_map(|n| {
                        let at = offset + n * 4;
                        let value = if at >= GPU_REGISTERS {
                            h.gpu.read(at - GPU_REGISTERS)
                        } else {
                            0
                        };
                        value.to_le_bytes()
                    })
                    .collect();
                h.process.space.write_bytes(&mut h.mem, addr, &data);
                ok().static_buffer(0, addr, size)
            }
            // SetBufferSwap(screen, framebuffer information): at once.
            0x05 => {
                let info: Vec<u32> = (2..9).map(|n| request.word(n)).collect();
                self.swap(h, request.word(1) & 1, &info);
                ok()
            }
            // FlushDataCache, InvalidateDataCache, StoreDataCache: caches
            // are not modelled.
            0x08 | 0x09 | 0x1F => ok(),
            // RegisterInterruptEvents: stubbed by GSP itself.
            0x0A => ok(),
            // SetLcdForceBlack(flag): both panels show black, or not.
            0x0B => {
                let fill = if request.word(1) & 0xFF != 0 {
                    1 << 24
                } else {
                    0
                };
                for pdc in &mut h.gpu.pdc {
                    pdc.fill = fill;
                }
                ok()
            }
            // TriggerCmdReqQueue.
            0x0C => {
                self.run_queue(h);
                ok()
            }
            // SetAxiConfigQoSMode, SetPerfLogMode.
            0x10 | 0x11 => ok(),
            // RegisterInterruptRelayQueue(flags, event): the client's index
            // and the shared page.
            0x13 => {
                let shared = match self.shared {
                    Some((object, _)) => Some(h.kernel.open(object)),
                    None => h.system_block(SHARED_LEN).map(|(handle, pa)| {
                        let object = h.kernel.resolve(handle).expect("a new handle resolves");
                        self.shared = Some((object, pa));
                        handle
                    }),
                };
                let Some(shared) = shared else {
                    return Reply::new(command, result::OUT_OF_MEMORY);
                };
                self.event = h.kernel.resolve(request.word(3));
                let code = if self.registered {
                    result::SUCCESS
                } else {
                    FIRST_INITIALIZATION
                };
                self.registered = true;
                Reply::new(command, code).word(0).handles(&[shared], false)
            }
            // UnregisterInterruptRelayQueue.
            0x14 => {
                self.event = None;
                ok()
            }
            // TryAcquireRight, AcquireRight, ReleaseRight: the application is
            // the only process that draws.
            0x15..=0x17 => ok(),
            // SaveVramSysArea, RestoreVramSysArea, ResetGpuCore,
            // SetLedForceOff, SetInternalPriorities.
            0x19..=0x1C | 0x1E => ok(),
            _ => h.unhandled(NAME, request),
        }
    }

    fn event(&mut self, h: &mut Horizon, tag: u32) {
        match tag {
            VBLANK => {
                self.update_framebuffers(h);
                self.relay(h, 2);
                self.relay(h, 3);
            }
            DMA => self.relay(h, DMA as u8),
            _ => {
                let unit = UNITS[(tag & 0xF) as usize % UNITS.len()];
                h.gpu.complete(unit);
                // The framebuffers are taken up when a transfer finishes.
                if unit == GpuIrq::Ppf {
                    self.update_framebuffers(h);
                }
                if tag & SILENT == 0 {
                    self.relay(h, unit as u8);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_ranges_are_word_aligned_short_and_below_the_end() {
        assert!(in_range(0x40_0000, 0x80));
        assert!(!in_range(0x40_0002, 4));
        assert!(!in_range(0x40_0000, 0x84));
        assert!(!in_range(REGISTERS_END - 4, 8));
        // The interrupt numbers are the units' order.
        assert_eq!(GpuIrq::P3d as u32, 5);
        assert_eq!(UNITS[GpuIrq::Ppf as usize], GpuIrq::Ppf);
    }
}
