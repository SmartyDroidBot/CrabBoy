//! Audio output through a fixed-rate ring buffer.
//!
//! One [`RingSource`] is appended to the rodio sink exactly once and never
//! ends: it pulls interleaved stereo samples from a shared queue and plays
//! silence when the queue runs dry. The emulator side converts whatever
//! sample rate the running console produces to [`OUTPUT_RATE`] and pushes
//! into that queue, dropping the oldest audio if playback falls more than
//! [`MAX_QUEUED_MS`] behind. Loading a ROM, resetting or pausing simply clears
//! the queue, so no audio from a previous state can bleed through and no
//! device is ever reopened.

use rodio::{OutputStream, Sink, Source};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Rate of the source handed to rodio, which resamples it to the device.
/// The GBA is native at this rate; the Game Boy's 8192/16384 Hz output is
/// upsampled by linear interpolation.
pub const OUTPUT_RATE: u32 = 32768;
const CHANNELS: u16 = 2;
/// Upper bound on queued audio; older samples are dropped past this.
const MAX_QUEUED_MS: usize = 120;
const MAX_QUEUED: usize = OUTPUT_RATE as usize * MAX_QUEUED_MS / 1000 * CHANNELS as usize;
/// Stereo frames the playback thread pulls per lock.
const PULL_FRAMES: usize = 256;

type Ring = Arc<Mutex<VecDeque<f32>>>;

/// The emulator-facing side of the audio output.
pub struct AudioOutput {
    /// Dropping the stream ends playback, so it lives as long as the app.
    _stream: OutputStream,
    _sink: Sink,
    writer: RingWriter,
}

impl AudioOutput {
    /// Open the default output device. `None` when there is no usable device.
    pub fn open() -> Option<AudioOutput> {
        let (stream, handle) = OutputStream::try_default().ok()?;
        let sink = Sink::try_new(&handle).ok()?;
        let writer = RingWriter::new();
        sink.append(RingSource {
            ring: Arc::clone(&writer.ring),
            staging: Vec::with_capacity(PULL_FRAMES * CHANNELS as usize),
            pos: 0,
        });
        sink.play();
        Some(AudioOutput {
            _stream: stream,
            _sink: sink,
            writer,
        })
    }

    /// Queue interleaved stereo `samples` produced at `rate` Hz.
    pub fn push(&mut self, samples: &[f32], rate: u32) {
        self.writer.push(samples, rate);
    }

    /// Discard everything queued; playback continues with silence.
    pub fn clear(&mut self) {
        self.writer.clear();
    }

    /// Milliseconds of audio waiting to be played.
    pub fn queued_ms(&self) -> u32 {
        self.writer.queued_ms()
    }
}

/// Producer side of the ring: rate conversion and the latency cap.
struct RingWriter {
    ring: Ring,
    /// The previous core stereo pair, so interpolation continues across pushes.
    last: [f32; 2],
}

impl RingWriter {
    fn new() -> RingWriter {
        RingWriter {
            ring: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_QUEUED))),
            last: [0.0; 2],
        }
    }

    fn push(&mut self, samples: &[f32], rate: u32) {
        let ratio = (OUTPUT_RATE / rate.max(1)).max(1) as usize;
        let mut q = self.ring.lock().unwrap();
        for pair in samples.as_chunks::<2>().0 {
            for k in 1..=ratio {
                let t = k as f32 / ratio as f32;
                q.push_back(self.last[0] + (pair[0] - self.last[0]) * t);
                q.push_back(self.last[1] + (pair[1] - self.last[1]) * t);
            }
            self.last = *pair;
        }
        if q.len() > MAX_QUEUED {
            let excess = q.len() - MAX_QUEUED;
            q.drain(..excess);
        }
    }

    fn clear(&mut self) {
        self.ring.lock().unwrap().clear();
        self.last = [0.0; 2];
    }

    fn queued_ms(&self) -> u32 {
        let frames = self.ring.lock().unwrap().len() / CHANNELS as usize;
        (frames * 1000 / OUTPUT_RATE as usize) as u32
    }
}

/// The rodio-facing side: an endless stereo source at [`OUTPUT_RATE`].
struct RingSource {
    ring: Ring,
    staging: Vec<f32>,
    pos: usize,
}

impl Iterator for RingSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.pos == self.staging.len() {
            self.staging.clear();
            self.pos = 0;
            let mut q = self.ring.lock().unwrap();
            let n = q.len().min(PULL_FRAMES * CHANNELS as usize);
            self.staging.extend(q.drain(..n));
        }
        if self.pos < self.staging.len() {
            let s = self.staging[self.pos];
            self.pos += 1;
            Some(s)
        } else {
            // Underrun: play silence but never end the source.
            Some(0.0)
        }
    }
}

impl Source for RingSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        CHANNELS
    }

    fn sample_rate(&self) -> u32 {
        OUTPUT_RATE
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The producer side needs no audio device.
    fn writer() -> (RingWriter, Ring) {
        let w = RingWriter::new();
        let ring = Arc::clone(&w.ring);
        (w, ring)
    }

    #[test]
    fn upsamples_by_the_integer_ratio_and_interpolates() {
        let (mut out, ring) = writer();
        out.push(&[1.0, -1.0], 8192);
        let q: Vec<f32> = ring.lock().unwrap().iter().copied().collect();
        assert_eq!(q.len(), 8, "one 8192 Hz pair becomes four 32768 Hz pairs");
        assert_eq!(q, [0.25, -0.25, 0.5, -0.5, 0.75, -0.75, 1.0, -1.0]);
        out.push(&[1.0, -1.0], 32768);
        assert_eq!(ring.lock().unwrap().len(), 10);
    }

    #[test]
    fn drops_the_oldest_audio_past_the_cap() {
        let (mut out, ring) = writer();
        let long: Vec<f32> = (0..MAX_QUEUED + 200).map(|i| i as f32).collect();
        out.push(&long, OUTPUT_RATE);
        let q = ring.lock().unwrap();
        assert_eq!(q.len(), MAX_QUEUED);
        assert_eq!(q[0], 200.0, "oldest samples were dropped");
        drop(q);
        out.clear();
        assert_eq!(out.queued_ms(), 0);
    }

    #[test]
    fn ring_source_plays_silence_on_underrun() {
        let ring: Ring = Arc::new(Mutex::new(VecDeque::from(vec![0.5, -0.5])));
        let mut src = RingSource {
            ring,
            staging: Vec::new(),
            pos: 0,
        };
        assert_eq!(src.next(), Some(0.5));
        assert_eq!(src.next(), Some(-0.5));
        assert_eq!(src.next(), Some(0.0));
        assert_eq!(src.channels(), 2);
        assert_eq!(src.sample_rate(), OUTPUT_RATE);
        assert_eq!(src.current_frame_len(), None);
    }
}
