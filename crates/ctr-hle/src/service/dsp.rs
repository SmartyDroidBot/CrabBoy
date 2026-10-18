//! The DSP service, `dsp::DSP` (3dbrew, "DSP Services", "DSP Memory Region"),
//! with a null DSP behind it.
//!
//! A game hands the DSP its own firmware with `LoadComponent`, then talks to
//! it through pipes and through two shared regions in DSP memory. The null
//! DSP runs no firmware: it says the firmware is loaded, answers the audio
//! pipe's start-up command with where the fifteen shared structures lie, and
//! then marks an audio frame every 160 samples, with the frame count in the
//! shared region and the pipe interrupt, as the firmware would. It produces
//! silence. The Teak DSP running the game's own firmware will take its place.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::kernel::{Handle, ObjId, Object, Reset};
use crate::result::{self, ResultCode};
use crate::service::Service;
use ctr_core::clock::ARM11_HZ;
use std::collections::{BTreeMap, VecDeque};

/// The output rate, and samples in an audio frame.
const SAMPLE_RATE: u64 = 32_728;
const FRAME_SAMPLES: u64 = 160;
/// ARM11 cycles in an audio frame.
pub const FRAME_CYCLES: u64 = ARM11_HZ as u64 * FRAME_SAMPLES / SAMPLE_RATE;
/// The two shared regions, as the process sees them.
const REGIONS: [u32; 2] = [0x1FF5_0000, 0x1FF7_0000];
/// Where the fifteen structures lie in a region, in bytes, with room for
/// what 3dbrew lists for each: frame count, 24 input configurations, 24
/// input statuses, 24 sets of ADPCM coefficients, DSP configuration and
/// status, output samples, intermediate mix samples, compressor table, debug
/// statistics, three coefficient tables, two biquad filters.
const STRUCTURES: [u16; 15] = [
    0x0000, 0x0010, 0x1210, 0x1330, 0x1630, 0x1730, 0x1770, 0x19F0, 0x41F0, 0x45F0, 0x49F0, 0x4DF0,
    0x51F0, 0x55F0, 0x59F0,
];
/// The audio pipe, and the interrupt kind of pipes.
const AUDIO_PIPE: u32 = 2;
const PIPE_INTERRUPT: u32 = 2;
/// A pipe that does not exist.
const INVALID_PIPE: ResultCode = ResultCode(0xD8E0_07F6);

#[derive(Default)]
pub struct Dsp {
    loaded: bool,
    running: bool,
    /// Data waiting to be read, by pipe.
    pipes: BTreeMap<u32, VecDeque<u8>>,
    /// Events registered for interrupts: (kind, channel) -> event.
    interrupts: BTreeMap<(u32, u32), ObjId>,
    semaphore_event: Option<Handle>,
    semaphore_mask: u32,
    frame: u64,
}

impl Dsp {
    fn interrupt(&self, h: &mut Horizon, kind: u32, channel: u32) {
        if let Some(&event) = self.interrupts.get(&(kind, channel)) {
            h.kernel.signal(event);
        }
    }

    /// Start marking audio frames, if not already.
    fn start(&mut self, h: &mut Horizon) {
        if !self.running {
            self.running = true;
            h.schedule_service("dsp::DSP", FRAME_CYCLES, 0);
        }
    }

    /// A command written to the audio pipe: its first byte says what.
    fn audio_command(&mut self, h: &mut Horizon, command: u8) {
        match command {
            // Initialize: the number of structures and their addresses, in
            // 16-bit words of DSP memory from 0x8000.
            0 => {
                let queue = self.pipes.entry(AUDIO_PIPE).or_default();
                queue.extend((STRUCTURES.len() as u16).to_le_bytes());
                for offset in STRUCTURES {
                    queue.extend((0x8000 + offset / 2).to_le_bytes());
                }
                self.interrupt(h, PIPE_INTERRUPT, AUDIO_PIPE);
                self.start(h);
            }
            // Shutdown, sleep.
            1 | 3 => self.running = false,
            // Wake up.
            2 => self.start(h),
            _ => {}
        }
    }
}

impl Service for Dsp {
    fn name(&self) -> &str {
        "dsp::DSP"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        match command {
            // RecvData(register): the DSP says it is up once it has firmware.
            0x01 => ok().word(self.loaded as u32),
            // RecvDataIsReady, SendDataIsEmpty.
            0x02 | 0x04 => ok().word(1),
            // SendData.
            0x03 => ok(),
            // SetSemaphore, ClearSemaphore.
            0x07 | 0x09 => ok(),
            // GetSemaphore.
            0x08 => ok().word(0),
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
                if channel == AUDIO_PIPE {
                    if let Some(&first) = data.first() {
                        self.audio_command(h, first);
                    }
                }
                ok()
            }
            // ReadPipe, ReadPipeIfPossible(channel, peer, size): what there
            // is, up to the size, in static buffer 0.
            0x0E | 0x10 => {
                let (channel, size) = (request.word(1), request.word(3) as usize);
                if channel > 3 {
                    return Reply::new(command, INVALID_PIPE).word(0);
                }
                let queue = self.pipes.entry(channel).or_default();
                let len = size.min(queue.len());
                let data: Vec<u8> = queue.drain(..len).collect();
                let (addr, room) = h.static_buffer(0);
                let len = len.min(room as usize);
                h.process.space.write_bytes(&mut h.mem, addr, &data[..len]);
                let reply = if command == 0x10 {
                    ok().word(len as u32)
                } else {
                    ok()
                };
                reply.static_buffer(0, addr, len as u32)
            }
            // GetPipeReadableSize(channel, peer).
            0x0F => {
                let len = self.pipes.get(&request.word(1)).map_or(0, VecDeque::len);
                ok().word(len as u32)
            }
            // LoadComponent(size, program mask, data mask, firmware): loaded.
            0x11 => {
                self.loaded = true;
                ok().word(1).mapped_buffer(request.word(4), request.word(5))
            }
            0x12 => {
                self.loaded = false;
                self.running = false;
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
                let event = *self.semaphore_event.get_or_insert_with(|| {
                    h.kernel.create_handle(Object::Event {
                        signalled: false,
                        reset: Reset::OneShot,
                    })
                });
                ok().handles(&[event], false)
            }
            // SetSemaphoreMask.
            0x17 => {
                self.semaphore_mask = request.word(1);
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

    /// An audio frame is over: the next frame count goes to the region whose
    /// parity it has, and the audio pipe interrupts.
    fn event(&mut self, h: &mut Horizon, _tag: u32) {
        if !self.running {
            return;
        }
        self.frame += 1;
        let region = REGIONS[(self.frame % 2) as usize];
        h.process
            .space
            .write_bytes(&mut h.mem, region, &(self.frame as u16).to_le_bytes());
        self.interrupt(h, PIPE_INTERRUPT, AUDIO_PIPE);
        h.schedule_service("dsp::DSP", FRAME_CYCLES, 0);
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
        assert!(STRUCTURES.windows(2).all(|w| w[0] < w[1]));
        assert!(*STRUCTURES.last().unwrap() as u32 + 0x400 <= 0x8000);
    }
}
