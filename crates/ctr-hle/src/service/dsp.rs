//! The DSP service, `dsp::DSP` (3dbrew, "DSP Services", "DSP Memory Region").
//!
//! A game hands the DSP its own firmware with `LoadComponent`, then talks to
//! it through the command/reply registers, the semaphore, pipes, and two
//! shared regions in DSP memory. The service is the same whatever answers
//! behind it; a [`DspBackend`] does the DSP's part. The null backend
//! ([`null::Null`]) runs no firmware and imitates its answers.

mod null;

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::kernel::{Handle, ObjId, Object, Reset};
use crate::result::{self, ResultCode};
use crate::service::Service;
use ctr_core::clock::ARM11_HZ;
use std::collections::BTreeMap;

pub use null::Null;

/// The output rate, and samples in an audio frame.
const SAMPLE_RATE: u64 = 32_728;
const FRAME_SAMPLES: u64 = 160;
/// ARM11 cycles in an audio frame.
pub const FRAME_CYCLES: u64 = ARM11_HZ as u64 * FRAME_SAMPLES / SAMPLE_RATE;
/// The audio pipe, and the interrupt kind of pipes.
const AUDIO_PIPE: u32 = 2;
const PIPE_INTERRUPT: u32 = 2;
/// A pipe that does not exist.
const INVALID_PIPE: ResultCode = ResultCode(0xD8E0_07F6);

/// The DSP's side of the service: what the firmware (or something that
/// imitates it) answers. Interrupts it raises are collected and handed to
/// the service with [`DspBackend::take_interrupts`], as (kind, channel):
/// kind 0 and 1 are the first two reply registers, kind 2 a pipe.
pub trait DspBackend {
    /// `LoadComponent`: the firmware image; whether it is now running.
    fn load(&mut self, h: &mut Horizon, component: &[u8]) -> bool;
    fn unload(&mut self, h: &mut Horizon);

    /// Reply register `register`, waiting for it if the DSP has not
    /// written it yet.
    fn recv_data(&mut self, h: &mut Horizon, register: u32) -> u16;
    fn recv_data_is_ready(&mut self, h: &mut Horizon, register: u32) -> bool;
    /// Command register `register`.
    fn send_data(&mut self, _h: &mut Horizon, _register: u32, _value: u16) {}
    fn send_data_is_empty(&mut self, _h: &mut Horizon, _register: u32) -> bool {
        true
    }

    /// The semaphore the ARM sets for the DSP.
    fn set_semaphore(&mut self, _h: &mut Horizon, _bits: u16) {}
    /// The semaphore the DSP sets for the ARM: its value, clearing bits of
    /// it, and masking bits of it.
    fn semaphore(&mut self, _h: &mut Horizon) -> u16 {
        0
    }
    fn clear_semaphore(&mut self, _h: &mut Horizon, _bits: u16) {}
    fn mask_semaphore(&mut self, _h: &mut Horizon, _bits: u16) {}

    /// Pipes, by channel: write all of `data`, read up to `len` bytes, the
    /// bytes waiting to be read.
    fn write_pipe(&mut self, h: &mut Horizon, channel: u32, data: &[u8]);
    fn read_pipe(&mut self, h: &mut Horizon, channel: u32, len: usize) -> Vec<u8>;
    fn pipe_readable(&mut self, h: &mut Horizon, channel: u32) -> u16;

    /// A clock event the backend scheduled for the service.
    fn event(&mut self, _h: &mut Horizon, _tag: u32) {}

    fn take_interrupts(&mut self) -> Vec<(u32, u32)>;
}

pub struct Dsp {
    backend: Box<dyn DspBackend>,
    /// Events registered for interrupts: (kind, channel) -> event.
    interrupts: BTreeMap<(u32, u32), ObjId>,
    semaphore_event: Option<(Handle, ObjId)>,
}

impl Default for Dsp {
    fn default() -> Self {
        Dsp::null()
    }
}

impl Dsp {
    pub fn with(backend: Box<dyn DspBackend>) -> Self {
        Dsp {
            backend,
            interrupts: BTreeMap::new(),
            semaphore_event: None,
        }
    }

    /// The service with the null DSP behind it.
    pub fn null() -> Self {
        Dsp::with(Box::new(Null::default()))
    }

    /// Signal the events registered for the interrupts the backend raised.
    fn deliver_interrupts(&mut self, h: &mut Horizon) {
        for key in self.backend.take_interrupts() {
            if let Some(&event) = self.interrupts.get(&key) {
                h.kernel.signal(event);
            }
        }
    }

    fn answer(&mut self, h: &mut Horizon, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        let backend = &mut self.backend;
        match command {
            // RecvData(register).
            0x01 => ok().word(backend.recv_data(h, request.word(1)) as u32),
            // RecvDataIsReady(register).
            0x02 => ok().word(backend.recv_data_is_ready(h, request.word(1)) as u32),
            // SendData(register, value).
            0x03 => {
                backend.send_data(h, request.word(1), request.word(2) as u16);
                ok()
            }
            // SendDataIsEmpty(register).
            0x04 => ok().word(backend.send_data_is_empty(h, request.word(1)) as u32),
            // SetSemaphore(value).
            0x07 => {
                backend.set_semaphore(h, request.word(1) as u16);
                ok()
            }
            // GetSemaphore.
            0x08 => ok().word(backend.semaphore(h) as u32),
            // ClearSemaphore(value).
            0x09 => {
                backend.clear_semaphore(h, request.word(1) as u16);
                ok()
            }
            // ConvertProcessAddressFromDspDram(address in 16-bit words).
            0x0C => ok().word(0x1FF4_0000 + (request.word(1) << 1)),
            // WriteProcessPipe(channel, size, data).
            0x0D => {
                let (channel, size, addr) = (request.word(1), request.word(2), request.word(4));
                let data = h
                    .process
                    .space
                    .read_bytes(&h.mem, addr, size.min(0x1000) as usize)
                    .unwrap_or_default();
                backend.write_pipe(h, channel, &data);
                ok()
            }
            // ReadPipe, ReadPipeIfPossible(channel, peer, size): what there
            // is, up to the size, in static buffer 0.
            0x0E | 0x10 => {
                let (channel, size) = (request.word(1), request.word(3) as usize);
                if channel > 3 {
                    return Reply::new(command, INVALID_PIPE).word(0);
                }
                let (addr, room) = h.static_buffer(0);
                let data = backend.read_pipe(h, channel, size.min(room as usize));
                h.process.space.write_bytes(&mut h.mem, addr, &data);
                let len = data.len() as u32;
                let reply = if command == 0x10 {
                    ok().word(len)
                } else {
                    ok()
                };
                reply.static_buffer(0, addr, len)
            }
            // GetPipeReadableSize(channel, peer).
            0x0F => ok().word(backend.pipe_readable(h, request.word(1)) as u32),
            // LoadComponent(size, program mask, data mask, firmware).
            0x11 => {
                let (size, addr) = (request.word(1), request.word(5));
                let component = h
                    .process
                    .space
                    .read_bytes(&h.mem, addr, size as usize)
                    .unwrap_or_default();
                let loaded = backend.load(h, &component);
                ok().word(loaded as u32)
                    .mapped_buffer(request.word(4), request.word(5))
            }
            0x12 => {
                backend.unload(h);
                ok()
            }
            // FlushDataCache, InvalidateDCache.
            0x13 | 0x14 => ok(),
            // RegisterInterruptEvents(kind, channel, event): no event
            // unregisters.
            0x15 => {
                let key = (request.word(1), request.word(2));
                match h.kernel.resolve(request.word(4)) {
                    Some(event) => {
                        self.interrupts.insert(key, event);
                    }
                    None => {
                        self.interrupts.remove(&key);
                    }
                }
                ok()
            }
            // GetSemaphoreEventHandle.
            0x16 => {
                let (event, _) = *self.semaphore_event.get_or_insert_with(|| {
                    let handle = h.kernel.create_handle(Object::Event {
                        signalled: false,
                        reset: Reset::OneShot,
                    });
                    (handle, h.kernel.resolve(handle).unwrap_or_default())
                });
                ok().handles(&[event], false)
            }
            // SetSemaphoreMask.
            0x17 => {
                backend.mask_semaphore(h, request.word(1) as u16);
                ok()
            }
            // GetHeadphoneStatus: none plugged in.
            0x1F => ok().word(0),
            // ForceHeadphoneOut.
            0x20 => ok(),
            // GetIsDspOccupied: no.
            0x21 => ok().word(0),
            _ => h.unhandled("dsp::DSP", request),
        }
    }
}

impl Service for Dsp {
    fn name(&self) -> &str {
        "dsp::DSP"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let reply = self.answer(h, request);
        self.deliver_interrupts(h);
        reply
    }

    fn event(&mut self, h: &mut Horizon, tag: u32) {
        self.backend.event(h, tag);
        self.deliver_interrupts(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_audio_frame_is_160_samples() {
        // 268,111,856 Hz * 160 / 32,728 Hz, rounded down.
        assert_eq!(FRAME_CYCLES, 1_310_739);
        // The structures fit a region, in order.
        let structures = null::STRUCTURES;
        assert!(structures.windows(2).all(|w| w[0] < w[1]));
        assert!(*structures.last().unwrap() as u32 + 0x400 <= 0x8000);
    }

    /// A console running a program that does nothing, whose thread-local
    /// storage describes a static buffer 0 of 0x100 bytes just after it.
    pub(super) fn console() -> Horizon {
        let code = 0xEF00_0003u32.to_le_bytes();
        let image = ctr_fs::threedsx::build([&code, &[], &[]], 0, &Default::default());
        let mut h = Horizon::from_3dsx(&image).unwrap();
        let tls = h.kernel.threads[h.kernel.current.unwrap()].tls;
        let space = &h.process.space;
        space.write32(&mut h.mem, tls + 0x180, 0x100 << 14 | 2);
        space.write32(&mut h.mem, tls + 0x184, tls + 0x100);
        h
    }

    pub(super) fn request(words: &[u32]) -> Request {
        Request {
            words: words.to_vec(),
        }
    }

    /// Bytes the process reads at `addr`.
    pub(super) fn bytes(h: &Horizon, addr: u32, len: usize) -> Vec<u8> {
        h.process.space.read_bytes(&h.mem, addr, len).unwrap()
    }

    #[test]
    fn the_null_dsp_answers_the_audio_start_up() {
        let mut h = console();
        let mut dsp = Dsp::null();
        let tls = h.kernel.threads[h.kernel.current.unwrap()].tls;
        let reply = dsp.call(
            &mut h,
            0,
            &request(&[0x0011_00C2, 0x300, 0xFF, 0xFF, 0x3000A, tls]),
        );
        assert_eq!(reply.encode()[1..3], [0, 1]);
        let reply = dsp.call(&mut h, 0, &request(&[0x0001_0040, 0]));
        assert_eq!(reply.encode()[2], 1);

        // The game waits on an event for the audio pipe, then starts it up.
        let event = h.kernel.create_handle(Object::Event {
            signalled: false,
            reset: Reset::OneShot,
        });
        dsp.call(&mut h, 0, &request(&[0x0015_0082, 2, 2, 0, event]));
        h.process.space.write32(&mut h.mem, tls + 0x200, 0);
        dsp.call(&mut h, 0, &request(&[0x000D_0082, 2, 4, 0x4C, tls + 0x200]));
        let id = h.kernel.resolve(event).unwrap();
        assert!(matches!(
            h.kernel.objects[id],
            Object::Event {
                signalled: true,
                ..
            }
        ));

        // The count of structures, then their addresses.
        let reply = dsp.call(&mut h, 0, &request(&[0x0010_00C0, 2, 0, 2]));
        assert_eq!(reply.encode()[2], 2);
        assert_eq!(bytes(&h, tls + 0x100, 2), [15, 0]);
        let reply = dsp.call(&mut h, 0, &request(&[0x0010_00C0, 2, 0, 30]));
        assert_eq!(reply.encode()[2], 30);
        assert_eq!(bytes(&h, tls + 0x100, 4), [0x00, 0x80, 0x08, 0x80]);
    }
}
