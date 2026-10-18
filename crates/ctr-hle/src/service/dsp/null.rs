//! The null DSP: no firmware runs.
//!
//! It says the firmware is loaded, answers the audio pipe's start-up command
//! with where the fifteen shared structures lie, and then marks an audio
//! frame every 160 samples, with the frame count in the shared region and the
//! pipe interrupt, as the firmware would. It produces silence.

use super::{DspBackend, AUDIO_PIPE, FRAME_CYCLES, PIPE_INTERRUPT};
use crate::horizon::Horizon;
use std::collections::{BTreeMap, VecDeque};

/// The two shared regions, as the process sees them.
const REGIONS: [u32; 2] = [0x1FF5_0000, 0x1FF7_0000];
/// Where the fifteen structures lie in a region, in bytes, with room for
/// what 3dbrew lists for each: frame count, 24 input configurations, 24
/// input statuses, 24 sets of ADPCM coefficients, DSP configuration and
/// status, output samples, intermediate mix samples, compressor table, debug
/// statistics, three coefficient tables, two biquad filters.
pub(super) const STRUCTURES: [u16; 15] = [
    0x0000, 0x0010, 0x1210, 0x1330, 0x1630, 0x1730, 0x1770, 0x19F0, 0x41F0, 0x45F0, 0x49F0, 0x4DF0,
    0x51F0, 0x55F0, 0x59F0,
];

#[derive(Default)]
pub struct Null {
    loaded: bool,
    running: bool,
    /// Data waiting to be read, by pipe.
    pipes: BTreeMap<u32, VecDeque<u8>>,
    frame: u64,
    interrupts: Vec<(u32, u32)>,
}

impl Null {
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
                self.interrupts.push((PIPE_INTERRUPT, AUDIO_PIPE));
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

impl DspBackend for Null {
    fn load(&mut self, _h: &mut Horizon, _component: &[u8]) -> bool {
        self.loaded = true;
        true
    }

    fn unload(&mut self, _h: &mut Horizon) {
        self.loaded = false;
        self.running = false;
    }

    /// The DSP says it is up once it has firmware.
    fn recv_data(&mut self, _h: &mut Horizon, _register: u32) -> u16 {
        self.loaded as u16
    }

    fn recv_data_is_ready(&mut self, _h: &mut Horizon, _register: u32) -> bool {
        true
    }

    fn write_pipe(&mut self, h: &mut Horizon, channel: u32, data: &[u8]) {
        if channel == AUDIO_PIPE {
            if let Some(&first) = data.first() {
                self.audio_command(h, first);
            }
        }
    }

    fn read_pipe(&mut self, _h: &mut Horizon, channel: u32, len: usize) -> Vec<u8> {
        let queue = self.pipes.entry(channel).or_default();
        let len = len.min(queue.len());
        queue.drain(..len).collect()
    }

    fn pipe_readable(&mut self, _h: &mut Horizon, channel: u32) -> u16 {
        self.pipes.get(&channel).map_or(0, VecDeque::len) as u16
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
        self.interrupts.push((PIPE_INTERRUPT, AUDIO_PIPE));
        h.schedule_service("dsp::DSP", FRAME_CYCLES, 0);
    }

    fn take_interrupts(&mut self) -> Vec<(u32, u32)> {
        std::mem::take(&mut self.interrupts)
    }
}
