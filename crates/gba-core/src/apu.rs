//! GBA Audio Processing Unit (APU).
//!
//! The GBA reuses the four Game Boy channels (two squares, one wave, one noise)
//! and adds two 8-bit Direct Sound channels (A and B) with DMA-fed FIFOs. All
//! six channels are mixed into a single stereo pair.
//!
//! The APU is clocked by [`Apu::step`] with CPU cycles. Every 512 cycles (the
//! 32768 Hz sample rate) it emits one stereo sample pair and advances a frame
//! sequencer that drives the length counters, envelopes, and channel-1 sweep.

use emu_core::audio::AudioBuffer;

/// CPU cycles per audio sample (16.78 MHz / 32768 Hz).
pub const CYCLES_PER_SAMPLE: u32 = 512;

const DUTY: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 0, 1, 1],
    [1, 1, 1, 1, 0, 0, 1, 1],
];

#[inline]
fn amp(v: u8) -> f32 {
    v as f32 / 15.0
}

#[derive(Clone, Copy)]
struct Envelope {
    volume: u8,
    up: bool,
    period: u8,
    timer: u8,
}

impl Envelope {
    fn new() -> Self {
        Envelope { volume: 0, up: false, period: 0, timer: 0 }
    }
    /// Update parameters from NRx2 without reloading the timer.
    fn set(&mut self, nr: u8) {
        self.volume = nr >> 4;
        self.up = nr & 0x08 != 0;
        self.period = nr & 0x07;
    }
    fn tick(&mut self) {
        if self.period == 0 {
            return;
        }
        if self.timer > 0 {
            self.timer -= 1;
            return;
        }
        self.timer = self.period;
        if self.up {
            if self.volume < 15 {
                self.volume += 1;
            }
        } else if self.volume > 0 {
            self.volume -= 1;
        }
    }
}

#[derive(Clone, Copy)]
struct Square {
    duty: u8,
    freq_timer: u32,
    freq: u16,
    phase: u8,
    length: u8,
    length_enable: bool,
    env: Envelope,
    sweep_period: u8,
    sweep_negate: bool,
    sweep_shift: u8,
    sweep_timer: u8,
    sweep_enabled: bool,
    on: bool,
}

impl Square {
    fn new() -> Self {
        Square {
            duty: 0,
            freq_timer: 0,
            freq: 0,
            phase: 0,
            length: 0,
            length_enable: false,
            env: Envelope::new(),
            sweep_period: 0,
            sweep_negate: false,
            sweep_shift: 0,
            sweep_timer: 0,
            sweep_enabled: false,
            on: false,
        }
    }
    fn set_duty_len_env(&mut self, value: u16) {
        self.duty = (value >> 14) as u8;
        self.length_enable = value & 0x4000 != 0;
        self.length = (64 - (value & 0x3F) as u8) & 0x3F;
        self.env.set(value as u8);
    }
    fn trigger(&mut self, value: u16, has_sweep: bool) {
        if self.length == 0 {
            self.length = 64;
        }
        self.freq = (self.freq & 0x07FF) | ((value & 0x0700) >> 8);
        self.freq = (value >> 8) & 0x07;
        self.freq_timer = (2048 - self.freq as u32) * 4;
        self.phase = 0;
        self.env.timer = self.env.period;
        if has_sweep {
            self.sweep_timer = if self.sweep_period == 0 { 8 } else { self.sweep_period };
            self.sweep_enabled = self.sweep_period != 0 || self.sweep_shift != 0;
            if self.sweep_shift != 0 {
                self.calc_sweep(true);
            }
        }
        self.on = true;
    }
    fn set_sweep(&mut self, value: u16) {
        self.sweep_period = ((value >> 4) & 0x07) as u8;
        self.sweep_negate = value & 0x08 != 0;
        self.sweep_shift = (value & 0x07) as u8;
    }
    fn calc_sweep(&mut self, load: bool) {
        let delta = self.freq >> self.sweep_shift;
        let new = if self.sweep_negate { self.freq.wrapping_sub(delta) } else { self.freq + delta };
        if new > 0x7FF {
            self.on = false;
        }
        if load {
            self.freq = new & 0x7FF;
            self.freq_timer = (2048 - self.freq as u32) * 4;
        }
    }
    fn tick_freq(&mut self) {
        if self.freq_timer == 0 {
            self.freq_timer = (2048 - self.freq as u32) * 4;
            self.phase = (self.phase + 1) & 7;
        }
        self.freq_timer -= 1;
    }
    fn sample(&self) -> f32 {
        if !self.on {
            return 0.0;
        }
        let a = amp(self.env.volume);
        if DUTY[self.duty as usize][self.phase as usize] == 1 { a } else { -a }
    }
    fn sweep_tick(&mut self) {
        if !self.sweep_enabled {
            return;
        }
        if self.sweep_timer > 0 {
            self.sweep_timer -= 1;
            return;
        }
        self.sweep_timer = if self.sweep_period == 0 { 8 } else { self.sweep_period };
        self.calc_sweep(true);
        self.calc_sweep(false);
    }
}

#[derive(Clone, Copy)]
struct Wave {
    freq_timer: u32,
    freq: u16,
    phase: u8,
    length: u16,
    length_enable: bool,
    volume_shift: u8,
    dac_on: bool,
    on: bool,
}

impl Wave {
    fn new() -> Self {
        Wave {
            freq_timer: 0,
            freq: 0,
            phase: 0,
            length: 0,
            length_enable: false,
            volume_shift: 0,
            dac_on: false,
            on: false,
        }
    }
    fn set_length(&mut self, value: u16) {
        self.length = 256 - (value & 0xFF);
        self.length_enable = value & 0x2000 != 0;
        self.dac_on = value & 0x8000 != 0;
    }
    fn trigger(&mut self, value: u16) {
        if self.length == 0 {
            self.length = 256;
        }
        self.freq = (value >> 8) & 0x07;
        self.freq_timer = (2048 - self.freq as u32) * 4;
        self.phase = 0;
        self.on = self.dac_on;
    }
    fn tick_freq(&mut self) {
        if self.freq_timer == 0 {
            self.freq_timer = (2048 - self.freq as u32) * 4;
            self.phase = (self.phase + 1) & 31;
        }
        self.freq_timer -= 1;
    }
    fn sample(&self, wave_ram: &[u8; 16]) -> f32 {
        if !self.on || self.volume_shift == 0 {
            return 0.0;
        }
        let byte = wave_ram[(self.phase >> 1) as usize];
        let nibble = if self.phase & 1 == 0 { byte >> 4 } else { byte & 0x0F };
        let v = nibble >> (self.volume_shift - 1);
        v as f32 / 7.5 - 1.0
    }
}

#[derive(Clone, Copy)]
struct Noise {
    freq_timer: u32,
    divisor: u8,
    shift: u8,
    width: bool,
    lfsr: u16,
    length: u8,
    length_enable: bool,
    env: Envelope,
    on: bool,
}

impl Noise {
    fn new() -> Self {
        Noise {
            freq_timer: 0,
            divisor: 0,
            shift: 0,
            width: false,
            lfsr: 0x7FFF,
            length: 0,
            length_enable: false,
            env: Envelope::new(),
            on: false,
        }
    }
    fn set_len_env(&mut self, value: u16) {
        self.length_enable = value & 0x4000 != 0;
        self.length = (64 - (value & 0x3F) as u8) & 0x3F;
        self.env.set(value as u8);
    }
    fn trigger(&mut self, value: u16) {
        if self.length == 0 {
            self.length = 64;
        }
        self.env.timer = self.env.period;
        self.divisor = ((value >> 8) & 0x07) as u8;
        self.width = value & 0x0800 != 0;
        self.shift = ((value >> 12) & 0x0F) as u8;
        self.freq_timer = if self.divisor == 0 { 8 } else { self.divisor as u32 * 16 };
        self.lfsr = 0x7FFF;
        self.on = true;
    }
    fn tick_freq(&mut self) {
        if self.freq_timer == 0 {
            self.freq_timer = if self.divisor == 0 { 8 } else { self.divisor as u32 * 16 };
            let xor = (self.lfsr & 1) ^ ((self.lfsr >> 1) & 1);
            self.lfsr >>= 1;
            self.lfsr |= xor << 14;
            if self.width {
                self.lfsr = (self.lfsr & !(1 << 6)) | (xor << 6);
            }
        }
        self.freq_timer -= 1;
    }
    fn sample(&self) -> f32 {
        if !self.on {
            return 0.0;
        }
        let a = amp(self.env.volume);
        if self.lfsr & 1 == 0 { a } else { -a }
    }
}

/// A Direct Sound channel (8-bit signed FIFO sample).
#[derive(Clone, Copy, Default)]
struct DirectSound {
    fifo: [u8; 32],
    fifo_count: usize,
    fifo_read: usize,
}

impl DirectSound {
    fn push(&mut self, v: u8) {
        if self.fifo_count < 32 {
            let idx = (self.fifo_read + self.fifo_count) % 32;
            self.fifo[idx] = v;
            self.fifo_count += 1;
        }
    }
    /// Pop the next signed 8-bit sample; an empty FIFO repeats silence.
    fn sample(&mut self) -> i8 {
        if self.fifo_count == 0 {
            return 0;
        }
        let v = self.fifo[self.fifo_read];
        self.fifo_read = (self.fifo_read + 1) % 32;
        self.fifo_count -= 1;
        v as i8
    }
}

/// The GBA APU.
pub struct Apu {
    sq1: Square,
    sq2: Square,
    wave: Wave,
    noise: Noise,
    dsa: DirectSound,
    dsb: DirectSound,
    /// Sample currently held by each DirectSound DAC; replaced on overflow of
    /// the timer selected in SOUNDCNT_H.
    dsa_current: i8,
    dsb_current: i8,
    /// Timer (0 or 1) that clocks each DirectSound channel.
    dsa_timer: u8,
    dsb_timer: u8,
    wave_ram: [u8; 16],
    cycles: u32,
    fs: u8,
    soundcnt_l: u16,
    soundcnt_h: u16,
    soundcnt_x: u16,
    output: AudioBuffer,
}

impl Default for Apu {
    fn default() -> Self {
        Apu {
            sq1: Square::new(),
            sq2: Square::new(),
            wave: Wave::new(),
            noise: Noise::new(),
            dsa: DirectSound::default(),
            dsb: DirectSound::default(),
            dsa_current: 0,
            dsb_current: 0,
            dsa_timer: 0,
            dsb_timer: 0,
            wave_ram: [0; 16],
            cycles: 0,
            fs: 0,
            soundcnt_l: 0,
            soundcnt_h: 0,
            soundcnt_x: 0,
            output: AudioBuffer::new(),
        }
    }
}

impl Apu {
    pub fn new() -> Apu {
        Apu::default()
    }

    /// Timer overflow notification: each DirectSound channel clocked by this
    /// timer pops its next FIFO sample into its DAC.
    pub fn timer_overflow(&mut self, timer_idx: u8) {
        if self.dsa_timer == timer_idx {
            self.dsa_current = self.dsa.sample();
        }
        if self.dsb_timer == timer_idx {
            self.dsb_current = self.dsb.sample();
        }
    }

    /// Bytes currently queued in FIFO A.
    pub fn fifo_a_count(&self) -> usize {
        self.dsa.fifo_count
    }

    /// Bytes currently queued in FIFO B.
    pub fn fifo_b_count(&self) -> usize {
        self.dsb.fifo_count
    }

    /// Queue a sample byte in FIFO A (DMA refill path).
    pub fn push_fifo_a(&mut self, v: u8) {
        self.dsa.push(v);
    }

    /// Queue a sample byte in FIFO B (DMA refill path).
    pub fn push_fifo_b(&mut self, v: u8) {
        self.dsb.push(v);
    }

    pub fn take_audio(&mut self) -> AudioBuffer {
        std::mem::take(&mut self.output)
    }

    /// Read a 16-bit sound register (offset within 0x04000000).
    pub fn read16(&self, offset: usize) -> u16 {
        match offset {
            0x80 => self.soundcnt_l,
            0x82 => self.soundcnt_h,
            0x84 => self.soundcnt_x,
            0x88 => 0x200,
            _ => 0,
        }
    }

    /// Write a 16-bit sound register.
    pub fn write16(&mut self, offset: usize, value: u16) {
        match offset {
            0x60 => self.sq1.set_sweep(value),
            0x62 => self.sq1.set_duty_len_env(value),
            0x64 => {
                self.sq1.freq = (self.sq1.freq & 0x07FF) | ((value >> 8) & 0x07);
                if value & 0x8000 != 0 {
                    self.sq1.trigger(value, true);
                }
            }
            0x68 => self.sq2.set_duty_len_env(value),
            0x6C => {
                self.sq2.freq = (value >> 8) & 0x07;
                if value & 0x8000 != 0 {
                    self.sq2.trigger(value, false);
                }
            }
            0x70 => self.wave.set_length(value),
            0x72 => self.wave.volume_shift = ((value >> 13) & 3) as u8,
            0x74 => {
                self.wave.freq = (value >> 8) & 0x07;
                if value & 0x8000 != 0 {
                    self.wave.trigger(value);
                }
            }
            0x78 => self.noise.set_len_env(value),
            0x7C => {
                if value & 0x8000 != 0 {
                    self.noise.trigger(value);
                } else {
                    self.noise.divisor = ((value >> 8) & 0x07) as u8;
                    self.noise.width = value & 0x0800 != 0;
                    self.noise.shift = ((value >> 12) & 0x0F) as u8;
                }
            }
            0x80 => self.soundcnt_l = value,
            0x82 => {
                self.soundcnt_h = value;
                self.dsa_timer = ((value >> 10) & 1) as u8;
                self.dsb_timer = ((value >> 14) & 1) as u8;
                if value & (1 << 11) != 0 {
                    self.dsa = DirectSound::default();
                    self.dsa_current = 0;
                }
                if value & (1 << 15) != 0 {
                    self.dsb = DirectSound::default();
                    self.dsb_current = 0;
                }
            }
            0x84 => self.soundcnt_x = value,
            0x90..=0x9C => {
                let idx = offset - 0x90;
                self.wave_ram[idx] = value as u8;
                self.wave_ram[idx + 1] = (value >> 8) as u8;
            }
            0xA0 => self.dsa.push(value as u8),
            0xA2 => self.dsa.push((value >> 8) as u8),
            0xA4 => self.dsb.push(value as u8),
            0xA6 => self.dsb.push((value >> 8) as u8),
            _ => {}
        }
    }

    /// Advance the APU by `cycles` CPU cycles, generating samples as needed.
    pub fn step(&mut self, cycles: u32) {
        self.cycles += cycles;
        while self.cycles >= CYCLES_PER_SAMPLE {
            self.cycles -= CYCLES_PER_SAMPLE;
            self.fs = (self.fs + 1) & 7;
            self.sequencer_step();
            self.tick_freqs();
            self.push_sample();
        }
    }

    fn sequencer_step(&mut self) {
        match self.fs {
            0 | 2 | 4 | 6 => {
                Self::length_tick_square(&mut self.sq1);
                Self::length_tick_square(&mut self.sq2);
                Self::length_tick_noise(&mut self.noise);
            }
            1 | 3 | 5 => {}
            _ => {}
        }
        // Sweep (ch1) at step 6, envelope at step 7.
        if self.fs == 6 {
            self.sq1.sweep_tick();
        }
        if self.fs == 7 {
            self.sq1.env.tick();
            self.sq2.env.tick();
            self.noise.env.tick();
        }
    }

    fn length_tick_square(ch: &mut Square) {
        if ch.length_enable && ch.on {
            ch.length -= 1;
            if ch.length == 0 {
                ch.on = false;
            }
        }
    }
    fn length_tick_noise(ch: &mut Noise) {
        if ch.length_enable && ch.on {
            ch.length -= 1;
            if ch.length == 0 {
                ch.on = false;
            }
        }
    }

    fn tick_freqs(&mut self) {
        self.sq1.tick_freq();
        self.sq2.tick_freq();
        self.wave.tick_freq();
        self.noise.tick_freq();
    }

    fn push_sample(&mut self) {
        let s1 = self.sq1.sample();
        let s2 = self.sq2.sample();
        let s3 = self.wave.sample(&self.wave_ram);
        let s4 = self.noise.sample();
        let dsa = self.dsa_current as f32 / 128.0;
        let dsb = self.dsb_current as f32 / 128.0;

        let cnt_l = self.soundcnt_l;
        let cnt_h = self.soundcnt_h;
        let dmg_vol = match (cnt_h >> 2) & 3 {
            0 => 0.25,
            1 => 0.5,
            _ => 1.0,
        };

        // SOUNDCNT_L: bits 0-3 = ch1-4 -> A, bits 4-7 = ch1-4 -> B.
        let dmgs = [s1, s2, s3, s4];
        let mut a = 0.0f32;
        let mut b = 0.0f32;
        for (i, s) in dmgs.iter().enumerate() {
            let bit = 1 << i;
            if cnt_l & bit != 0 {
                a += s * dmg_vol;
            }
            if cnt_l & (bit << 4) != 0 {
                b += s * dmg_vol;
            }
        }
        if cnt_h & (1 << 6) != 0 {
            a += dsa;
        }
        if cnt_h & (1 << 7) != 0 {
            b += dsb;
        }

        let l = a.clamp(-1.0, 1.0);
        let r = b.clamp(-1.0, 1.0);
        self.output.push(l, r);
    }
}

impl std::fmt::Debug for Apu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Apu")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_trigger_produces_audio() {
        let mut apu = Apu::new();
        apu.write16(0x60, 0);
        apu.write16(0x62, 0x8F0 | 0x00F0);
        apu.write16(0x64, 0x8000 | (1 << 8));
        apu.write16(0x80, 0x00FF);
        apu.write16(0x82, (2 << 2) | (1 << 6) | (1 << 7));
        apu.step(CYCLES_PER_SAMPLE * 4);
        let audio = apu.take_audio();
        assert_eq!(audio.samples.len(), 8);
        assert!(audio.samples.iter().any(|&s| s != 0.0));
    }

    #[test]
    fn fifo_produces_output() {
        let mut apu = Apu::new();
        apu.write16(0x82, (1 << 6) | (1 << 7));
        apu.write16(0xA0, 0x80 | 0x20);
        // Nothing reaches the DAC until the selected timer (0) overflows.
        apu.step(CYCLES_PER_SAMPLE);
        assert_eq!(apu.take_audio().samples, vec![0.0, 0.0]);
        apu.timer_overflow(0);
        apu.step(CYCLES_PER_SAMPLE);
        let audio = apu.take_audio();
        assert_eq!(audio.samples.len(), 2);
        assert!(audio.samples[0] != 0.0 || audio.samples[1] != 0.0);
    }

    #[test]
    fn fifo_samples_are_signed() {
        let mut apu = Apu::new();
        apu.write16(0x82, 1 << 6);
        apu.write16(0xA0, 0x80);
        apu.timer_overflow(0);
        apu.step(CYCLES_PER_SAMPLE);
        let audio = apu.take_audio();
        assert_eq!(audio.samples[0], -1.0);
    }

    #[test]
    fn fifo_empty_is_silent() {
        let mut apu = Apu::new();
        apu.write16(0x82, (1 << 6) | (1 << 7));
        apu.step(CYCLES_PER_SAMPLE);
        let audio = apu.take_audio();
        assert_eq!(audio.samples, vec![0.0, 0.0]);
    }
}