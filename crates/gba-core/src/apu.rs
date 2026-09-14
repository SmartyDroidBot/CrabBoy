//! GBA Audio Processing Unit (APU).
//!
//! The GBA reuses the four Game Boy channels (two squares, one wave, one noise)
//! and adds two 8-bit Direct Sound channels (A and B) with DMA-fed FIFOs. All
//! six channels are mixed into a single stereo pair.
//!
//! The APU is clocked by [`Apu::step`] with CPU cycles: channel timers count
//! CPU cycles, a 512 Hz frame sequencer drives the length counters, envelopes
//! and the channel-1 sweep, and one stereo sample is emitted every 512 cycles
//! (32768 Hz). Mixing is integer-only in the 10-bit units of the hardware
//! output; SOUNDBIAS is applied exactly as on the console.
//!
//! Register layouts follow GBATEK ("GBA Sound Controller").

use emu_core::audio::AudioBuffer;

/// CPU cycles per audio sample (16.78 MHz / 32768 Hz).
pub const CYCLES_PER_SAMPLE: u32 = 512;
/// CPU cycles per frame-sequencer step (512 Hz).
const CYCLES_PER_SEQUENCER_STEP: u32 = 32768;

const DUTY: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 0, 1, 1],
    [1, 1, 1, 1, 0, 0, 1, 1],
];

/// Advance a down-counting timer by `cycles`, reloading it with `period`
/// (which must be non-zero) each time it expires. Returns how many times it
/// expired, so a channel can be clocked in bulk instead of per cycle.
fn run_timer(timer: &mut u32, period: u32, cycles: u32) -> u32 {
    if cycles < *timer {
        *timer -= cycles;
        return 0;
    }
    let rest = cycles - *timer;
    *timer = period - rest % period;
    1 + rest / period
}

/// Volume envelope (SOUNDxCNT bits 8-15: initial volume, direction, period).
#[derive(Clone, Copy)]
struct Envelope {
    volume: u8,
    initial: u8,
    up: bool,
    period: u8,
    timer: u8,
}

impl Envelope {
    fn new() -> Self {
        Envelope {
            volume: 0,
            initial: 0,
            up: false,
            period: 0,
            timer: 0,
        }
    }
    /// Update the parameters from the register's high byte. The running
    /// volume only changes on the next restart.
    fn set(&mut self, nr: u8) {
        self.initial = nr >> 4;
        self.up = nr & 0x08 != 0;
        self.period = nr & 0x07;
    }
    /// The DAC is powered while the initial volume or direction is non-zero.
    fn dac_on(&self) -> bool {
        self.initial != 0 || self.up
    }
    fn trigger(&mut self) {
        self.volume = self.initial;
        self.timer = if self.period == 0 { 8 } else { self.period };
    }
    /// One 64 Hz envelope clock.
    fn tick(&mut self) {
        if self.period == 0 {
            return;
        }
        self.timer -= 1;
        if self.timer > 0 {
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
            freq_timer: 16,
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
    /// SOUND1CNT_L: sweep shift (0-2), direction (3), period (4-6).
    fn set_sweep(&mut self, value: u16) {
        self.sweep_shift = (value & 0x07) as u8;
        self.sweep_negate = value & 0x08 != 0;
        self.sweep_period = ((value >> 4) & 0x07) as u8;
    }
    /// SOUND1CNT_H / SOUND2CNT_L: length (0-5), duty (6-7), envelope (8-15).
    fn set_len_duty_env(&mut self, value: u16) {
        self.length = 64 - (value & 0x3F) as u8;
        self.duty = ((value >> 6) & 3) as u8;
        self.env.set((value >> 8) as u8);
        if !self.env.dac_on() {
            self.on = false;
        }
    }
    /// SOUND1CNT_X / SOUND2CNT_H: frequency (0-10), length enable (14),
    /// restart (15).
    fn set_freq_ctrl(&mut self, value: u16, has_sweep: bool) {
        self.freq = value & 0x07FF;
        self.length_enable = value & 0x4000 != 0;
        if value & 0x8000 != 0 {
            self.trigger(has_sweep);
        }
    }
    fn trigger(&mut self, has_sweep: bool) {
        if self.length == 0 {
            self.length = 64;
        }
        self.freq_timer = self.period();
        self.phase = 0;
        self.env.trigger();
        if has_sweep {
            self.sweep_timer = if self.sweep_period == 0 {
                8
            } else {
                self.sweep_period
            };
            self.sweep_enabled = self.sweep_period != 0 || self.sweep_shift != 0;
            if self.sweep_shift != 0 {
                self.calc_sweep(false);
            }
        }
        self.on = self.env.dac_on();
    }
    fn calc_sweep(&mut self, load: bool) {
        let delta = self.freq >> self.sweep_shift;
        let new = if self.sweep_negate {
            self.freq.wrapping_sub(delta)
        } else {
            self.freq + delta
        };
        if new > 0x7FF {
            self.on = false;
        } else if load {
            self.freq = new;
        }
    }
    /// CPU cycles per duty step: the Game Boy's `(2048 - f) * 4` at four
    /// times the clock.
    fn period(&self) -> u32 {
        (2048 - self.freq as u32) * 16
    }
    fn clock(&mut self, cycles: u32) {
        let period = self.period();
        let steps = run_timer(&mut self.freq_timer, period, cycles);
        self.phase = (self.phase as u32 + steps) as u8 & 7;
    }
    /// Bipolar 4-bit DAC output: `+volume` on the high part of the duty
    /// cycle, `-volume` on the low part, 0 while the channel is off.
    fn output(&self) -> i32 {
        if !self.on {
            return 0;
        }
        let v = self.env.volume as i32;
        if DUTY[self.duty as usize][self.phase as usize] == 1 {
            v
        } else {
            -v
        }
    }
    fn length_tick(&mut self) {
        if self.length_enable && self.length > 0 {
            self.length -= 1;
            if self.length == 0 {
                self.on = false;
            }
        }
    }
    fn sweep_tick(&mut self) {
        if !self.sweep_enabled {
            return;
        }
        if self.sweep_timer > 0 {
            self.sweep_timer -= 1;
        }
        if self.sweep_timer > 0 {
            return;
        }
        self.sweep_timer = if self.sweep_period == 0 {
            8
        } else {
            self.sweep_period
        };
        if self.sweep_period != 0 {
            self.calc_sweep(true);
            self.calc_sweep(false);
        }
    }
}

#[derive(Clone, Copy)]
struct Wave {
    /// SOUND3CNT_L bit 5: play both banks as one 64-sample wave.
    dimension: bool,
    /// SOUND3CNT_L bit 6: the bank playback starts in; the CPU sees the other.
    bank: u8,
    dac_on: bool,
    length: u16,
    length_enable: bool,
    /// SOUND3CNT_H bits 13-14: 0 mute, 1 100 %, 2 50 %, 3 25 %.
    volume: u8,
    /// SOUND3CNT_H bit 15: force 75 %.
    force75: bool,
    freq_timer: u32,
    freq: u16,
    /// Sample position within the 32 or 64 sample sequence.
    pos: u8,
    on: bool,
    /// Both wave RAM banks (bank 0 in bytes 0-15, bank 1 in 16-31).
    ram: [u8; 32],
}

impl Wave {
    fn new() -> Self {
        Wave {
            dimension: false,
            bank: 0,
            dac_on: false,
            length: 0,
            length_enable: false,
            volume: 0,
            force75: false,
            freq_timer: 8,
            freq: 0,
            pos: 0,
            on: false,
            ram: [0; 32],
        }
    }
    /// SOUND3CNT_L: dimension (5), bank (6), playback enable (7).
    fn set_ctrl_l(&mut self, value: u16) {
        self.dimension = value & 0x20 != 0;
        self.bank = ((value >> 6) & 1) as u8;
        self.dac_on = value & 0x80 != 0;
        if !self.dac_on {
            self.on = false;
        }
    }
    /// SOUND3CNT_H: length (0-7), volume (13-14), force 75 % (15).
    fn set_len_vol(&mut self, value: u16) {
        self.length = 256 - (value & 0xFF);
        self.volume = ((value >> 13) & 3) as u8;
        self.force75 = value & 0x8000 != 0;
    }
    /// SOUND3CNT_X: frequency (0-10), length enable (14), restart (15).
    fn set_freq_ctrl(&mut self, value: u16) {
        self.freq = value & 0x07FF;
        self.length_enable = value & 0x4000 != 0;
        if value & 0x8000 != 0 {
            self.trigger();
        }
    }
    fn trigger(&mut self) {
        if self.length == 0 {
            self.length = 256;
        }
        self.freq_timer = self.period();
        self.pos = 0;
        self.on = self.dac_on;
    }
    /// CPU cycles per wave sample: the Game Boy's `(2048 - f) * 2` at four
    /// times the clock.
    fn period(&self) -> u32 {
        (2048 - self.freq as u32) * 8
    }
    fn samples(&self) -> u32 {
        if self.dimension {
            64
        } else {
            32
        }
    }
    fn clock(&mut self, cycles: u32) {
        let period = self.period();
        let steps = run_timer(&mut self.freq_timer, period, cycles);
        self.pos = ((self.pos as u32 + steps) % self.samples()) as u8;
    }
    /// Index into `ram` (in nibbles) of the sample being played.
    fn play_nibble(&self) -> usize {
        let start = self.bank as usize * 32;
        if self.dimension {
            (start + self.pos as usize) % 64
        } else {
            start + self.pos as usize % 32
        }
    }
    /// The byte offset in `ram` the CPU sees at wave RAM offset `idx`: the
    /// bank that is not selected for playback.
    fn cpu_byte(&self, idx: usize) -> usize {
        (1 - self.bank as usize) * 16 + (idx & 0xF)
    }
    fn read_ram16(&self, idx: usize) -> u16 {
        let i = self.cpu_byte(idx & 0xE);
        u16::from(self.ram[i]) | u16::from(self.ram[i + 1]) << 8
    }
    fn write_ram16(&mut self, idx: usize, value: u16) {
        let i = self.cpu_byte(idx & 0xE);
        self.ram[i] = value as u8;
        self.ram[i + 1] = (value >> 8) as u8;
    }
    /// Bipolar 4-bit DAC output of the current sample after the volume
    /// shift (0 while the channel is off).
    fn output(&self) -> i32 {
        if !self.on {
            return 0;
        }
        let nib = self.play_nibble();
        let byte = self.ram[nib / 2];
        let sample = if nib & 1 == 0 { byte >> 4 } else { byte & 0x0F } as i32;
        let scaled = if self.force75 {
            sample * 3 / 4
        } else {
            match self.volume {
                0 => 0,
                1 => sample,
                2 => sample >> 1,
                _ => sample >> 2,
            }
        };
        scaled * 2 - 15
    }
    fn length_tick(&mut self) {
        if self.length_enable && self.length > 0 {
            self.length -= 1;
            if self.length == 0 {
                self.on = false;
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Noise {
    freq_timer: u32,
    /// SOUND4CNT_H bits 0-2 (dividing ratio r).
    divisor: u8,
    /// SOUND4CNT_H bits 4-7 (shift s).
    shift: u8,
    /// SOUND4CNT_H bit 3: 7-bit counter.
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
            freq_timer: 32,
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
    /// SOUND4CNT_L: length (0-5), envelope (8-15).
    fn set_len_env(&mut self, value: u16) {
        self.length = 64 - (value & 0x3F) as u8;
        self.env.set((value >> 8) as u8);
        if !self.env.dac_on() {
            self.on = false;
        }
    }
    /// SOUND4CNT_H: r (0-2), width (3), s (4-7), length enable (14),
    /// restart (15).
    fn set_ctrl(&mut self, value: u16) {
        self.divisor = (value & 0x07) as u8;
        self.width = value & 0x08 != 0;
        self.shift = ((value >> 4) & 0x0F) as u8;
        self.length_enable = value & 0x4000 != 0;
        if value & 0x8000 != 0 {
            self.trigger();
        }
    }
    fn trigger(&mut self) {
        if self.length == 0 {
            self.length = 64;
        }
        self.env.trigger();
        self.freq_timer = self.period();
        self.lfsr = 0x7FFF;
        self.on = self.env.dac_on();
    }
    /// CPU cycles per LFSR shift: `(r == 0 ? 8 : 16 r) << s` Game Boy cycles
    /// at four times the clock. Shift values 14 and 15 never clock.
    fn period(&self) -> u32 {
        let base = if self.divisor == 0 {
            32
        } else {
            64 * self.divisor as u32
        };
        base << self.shift.min(13)
    }
    fn clock(&mut self, cycles: u32) {
        if self.shift >= 14 {
            return;
        }
        let period = self.period();
        let steps = run_timer(&mut self.freq_timer, period, cycles);
        // The LFSR repeats after at most 32767 steps (127 in 7-bit mode).
        let repeat = if self.width { 127 } else { 32767 };
        for _ in 0..steps % repeat {
            let xor = (self.lfsr & 1) ^ ((self.lfsr >> 1) & 1);
            self.lfsr >>= 1;
            self.lfsr |= xor << 14;
            if self.width {
                self.lfsr = (self.lfsr & !(1 << 6)) | (xor << 6);
            }
        }
    }
    fn output(&self) -> i32 {
        if !self.on {
            return 0;
        }
        let v = self.env.volume as i32;
        if self.lfsr & 1 == 0 {
            v
        } else {
            -v
        }
    }
    fn length_tick(&mut self) {
        if self.length_enable && self.length > 0 {
            self.length -= 1;
            if self.length == 0 {
                self.on = false;
            }
        }
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
    /// CPU cycles into the current sample and sequencer step.
    cycles: u32,
    fs_cycles: u32,
    fs: u8,
    soundcnt_l: u16,
    soundcnt_h: u16,
    soundcnt_x: u16,
    soundbias: u16,
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
            cycles: 0,
            fs_cycles: 0,
            fs: 0,
            soundcnt_l: 0,
            soundcnt_h: 0,
            soundcnt_x: 0,
            soundbias: 0x200,
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

    /// Timer (0 or 1) selected in SOUNDCNT_H to clock DirectSound A.
    pub fn dsa_timer(&self) -> u8 {
        self.dsa_timer
    }

    /// Timer (0 or 1) selected in SOUNDCNT_H to clock DirectSound B.
    pub fn dsb_timer(&self) -> u8 {
        self.dsb_timer
    }

    /// SOUNDCNT_X bit 7.
    pub fn master_enabled(&self) -> bool {
        self.soundcnt_x & 0x80 != 0
    }

    /// SOUNDCNT_X as the CPU reads it: the master enable plus the live
    /// channel-active bits 0-3.
    pub fn read_soundcnt_x(&self) -> u16 {
        (self.soundcnt_x & 0x80)
            | u16::from(self.sq1.on)
            | u16::from(self.sq2.on) << 1
            | u16::from(self.wave.on) << 2
            | u16::from(self.noise.on) << 3
    }

    /// SOUNDBIAS as the CPU reads it.
    pub fn soundbias(&self) -> u16 {
        self.soundbias
    }

    /// Halfword of wave RAM at `idx` (0..16, even), from the bank the CPU
    /// can access (the one not being played).
    pub fn read_wave_ram16(&self, idx: usize) -> u16 {
        self.wave.read_ram16(idx)
    }

    /// A byte store to the FIFO ports 0xA0-0xA7.
    pub fn push_fifo_byte(&mut self, offset: usize, v: u8) {
        if offset & 4 == 0 {
            self.dsa.push(v);
        } else {
            self.dsb.push(v);
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

    pub(crate) fn save(&self, w: &mut crate::state::Writer) {
        self.sq1.save(w);
        self.sq2.save(w);
        self.wave.save(w);
        self.noise.save(w);
        self.dsa.save(w);
        self.dsb.save(w);
        w.u8(self.dsa_current as u8);
        w.u8(self.dsb_current as u8);
        w.u8(self.dsa_timer);
        w.u8(self.dsb_timer);
        w.u32(self.cycles);
        w.u32(self.fs_cycles);
        w.u8(self.fs);
        w.u16(self.soundcnt_l);
        w.u16(self.soundcnt_h);
        w.u16(self.soundcnt_x);
        w.u16(self.soundbias);
    }

    pub(crate) fn load(&mut self, r: &mut crate::state::Reader) -> Result<(), String> {
        self.sq1.load(r)?;
        self.sq2.load(r)?;
        self.wave.load(r)?;
        self.noise.load(r)?;
        self.dsa.load(r)?;
        self.dsb.load(r)?;
        self.dsa_current = r.u8()? as i8;
        self.dsb_current = r.u8()? as i8;
        self.dsa_timer = r.u8()? & 1;
        self.dsb_timer = r.u8()? & 1;
        self.cycles = r.u32()?;
        self.fs_cycles = r.u32()?;
        self.fs = r.u8()?;
        self.soundcnt_l = r.u16()?;
        self.soundcnt_h = r.u16()?;
        self.soundcnt_x = r.u16()?;
        self.soundbias = r.u16()?;
        self.output = AudioBuffer::new();
        Ok(())
    }

    pub fn take_audio(&mut self) -> AudioBuffer {
        std::mem::take(&mut self.output)
    }

    /// Read a 16-bit sound register (offset within 0x04000000).
    pub fn read16(&self, offset: usize) -> u16 {
        match offset {
            0x80 => self.soundcnt_l,
            0x82 => self.soundcnt_h,
            0x84 => self.read_soundcnt_x(),
            0x88 => self.soundbias,
            0x90..=0x9F => self.wave.read_ram16(offset - 0x90),
            _ => 0,
        }
    }

    /// Write a 16-bit sound register. The PSG registers (0x60-0x81) are
    /// ignored while the master enable is off.
    pub fn write16(&mut self, offset: usize, value: u16) {
        if !self.master_enabled() && (0x60..=0x81).contains(&offset) {
            return;
        }
        match offset {
            0x60 => self.sq1.set_sweep(value),
            0x62 => self.sq1.set_len_duty_env(value),
            0x64 => self.sq1.set_freq_ctrl(value, true),
            0x68 => self.sq2.set_len_duty_env(value),
            0x6C => self.sq2.set_freq_ctrl(value, false),
            0x70 => self.wave.set_ctrl_l(value),
            0x72 => self.wave.set_len_vol(value),
            0x74 => self.wave.set_freq_ctrl(value),
            0x78 => self.noise.set_len_env(value),
            0x7C => self.noise.set_ctrl(value),
            0x80 => self.soundcnt_l = value & 0xFF77,
            0x82 => {
                // Bits 11 and 15 are write-only FIFO resets.
                self.soundcnt_h = value & 0x770F;
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
            0x84 => {
                let enable = value & 0x80;
                if enable == 0 && self.master_enabled() {
                    self.power_off();
                }
                self.soundcnt_x = enable;
            }
            0x88 => self.soundbias = value & 0xC3FE,
            0x90..=0x9F => self.wave.write_ram16(offset - 0x90, value),
            0xA0 | 0xA2 => {
                self.dsa.push(value as u8);
                self.dsa.push((value >> 8) as u8);
            }
            0xA4 | 0xA6 => {
                self.dsb.push(value as u8);
                self.dsb.push((value >> 8) as u8);
            }
            _ => {}
        }
    }

    /// Clearing SOUNDCNT_X bit 7 resets the PSG channels and their
    /// registers; the FIFOs, SOUNDCNT_H and wave RAM survive.
    fn power_off(&mut self) {
        let ram = self.wave.ram;
        self.sq1 = Square::new();
        self.sq2 = Square::new();
        self.wave = Wave::new();
        self.wave.ram = ram;
        self.noise = Noise::new();
        self.soundcnt_l = 0;
        self.fs = 0;
        self.fs_cycles = 0;
    }

    /// Advance the APU by `cycles` CPU cycles: clock the channel timers and
    /// the 512 Hz frame sequencer, emitting one sample every 512 cycles.
    pub fn step(&mut self, mut cycles: u32) {
        while cycles > 0 {
            let run = cycles.min(CYCLES_PER_SAMPLE - self.cycles);
            self.clock_channels(run);
            self.fs_cycles += run;
            while self.fs_cycles >= CYCLES_PER_SEQUENCER_STEP {
                self.fs_cycles -= CYCLES_PER_SEQUENCER_STEP;
                self.fs = (self.fs + 1) & 7;
                self.sequencer_step();
            }
            self.cycles += run;
            cycles -= run;
            if self.cycles == CYCLES_PER_SAMPLE {
                self.cycles = 0;
                self.push_sample();
            }
        }
    }

    fn clock_channels(&mut self, cycles: u32) {
        self.sq1.clock(cycles);
        self.sq2.clock(cycles);
        self.wave.clock(cycles);
        self.noise.clock(cycles);
    }

    fn sequencer_step(&mut self) {
        if self.fs & 1 == 0 {
            self.sq1.length_tick();
            self.sq2.length_tick();
            self.wave.length_tick();
            self.noise.length_tick();
        }
        // Sweep (ch1) at steps 2 and 6, envelopes at step 7.
        if self.fs == 2 || self.fs == 6 {
            self.sq1.sweep_tick();
        }
        if self.fs == 7 {
            self.sq1.env.tick();
            self.sq2.env.tick();
            self.noise.env.tick();
        }
    }

    /// Mix one stereo sample in the hardware's 10-bit units and apply
    /// SOUNDBIAS, then scale to `-1.0..1.0` (`(out - 0x200) / 512`).
    fn push_sample(&mut self) {
        if !self.master_enabled() {
            self.output.push(0.0, 0.0);
            return;
        }
        let psg = [
            self.sq1.output(),
            self.sq2.output(),
            self.wave.output(),
            self.noise.output(),
        ];
        let cnt_l = self.soundcnt_l;
        let cnt_h = self.soundcnt_h;
        // SOUNDCNT_H bits 0-1: PSG at 25 / 50 / 100 %.
        let psg_shift = 2 - u32::from(cnt_h & 3).min(2);
        let dsa = i32::from(self.dsa_current) * if cnt_h & 0x04 != 0 { 4 } else { 2 };
        let dsb = i32::from(self.dsb_current) * if cnt_h & 0x08 != 0 { 4 } else { 2 };
        let bias = i32::from(self.soundbias & 0x3FE);
        let resolution = u32::from(self.soundbias >> 14) + 1;

        // side 0 = right (SOUNDCNT_L bits 0-2, 8-11; SOUNDCNT_H bits 8, 12),
        // side 1 = left (bits 4-6, 12-15; bits 9, 13).
        let mut out = [0.0f32; 2];
        for (side, slot) in out.iter_mut().enumerate() {
            let master = i32::from((cnt_l >> (4 * side)) & 7);
            let mut mix = 0;
            for (i, ch) in psg.iter().enumerate() {
                if cnt_l & (1 << (8 + 4 * side + i)) != 0 {
                    mix += ch;
                }
            }
            mix = (mix * master) >> psg_shift;
            if cnt_h & (1 << (8 + side)) != 0 {
                mix += dsa;
            }
            if cnt_h & (1 << (12 + side)) != 0 {
                mix += dsb;
            }
            let v = (mix + bias).clamp(0, 0x3FF) >> resolution << resolution;
            *slot = (v - 0x200) as f32 / 512.0;
        }
        self.output.push(out[1], out[0]);
    }
}

impl std::fmt::Debug for Apu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Apu")
    }
}

// Save-state serialisation of the channel state.
impl Envelope {
    fn save(&self, w: &mut crate::state::Writer) {
        w.u8(self.volume);
        w.u8(self.initial);
        w.bool(self.up);
        w.u8(self.period);
        w.u8(self.timer);
    }
    fn load(&mut self, r: &mut crate::state::Reader) -> Result<(), String> {
        self.volume = r.u8()?;
        self.initial = r.u8()?;
        self.up = r.bool()?;
        self.period = r.u8()?;
        self.timer = r.u8()?;
        Ok(())
    }
}

impl Square {
    fn save(&self, w: &mut crate::state::Writer) {
        w.u8(self.duty);
        w.u32(self.freq_timer);
        w.u16(self.freq);
        w.u8(self.phase);
        w.u8(self.length);
        w.bool(self.length_enable);
        self.env.save(w);
        w.u8(self.sweep_period);
        w.bool(self.sweep_negate);
        w.u8(self.sweep_shift);
        w.u8(self.sweep_timer);
        w.bool(self.sweep_enabled);
        w.bool(self.on);
    }
    fn load(&mut self, r: &mut crate::state::Reader) -> Result<(), String> {
        self.duty = r.u8()? & 3;
        self.freq_timer = r.u32()?.max(1);
        self.freq = r.u16()? & 0x7FF;
        self.phase = r.u8()? & 7;
        self.length = r.u8()?;
        self.length_enable = r.bool()?;
        self.env.load(r)?;
        self.sweep_period = r.u8()?;
        self.sweep_negate = r.bool()?;
        self.sweep_shift = r.u8()?;
        self.sweep_timer = r.u8()?;
        self.sweep_enabled = r.bool()?;
        self.on = r.bool()?;
        Ok(())
    }
}

impl Wave {
    fn save(&self, w: &mut crate::state::Writer) {
        w.bool(self.dimension);
        w.u8(self.bank);
        w.bool(self.dac_on);
        w.u16(self.length);
        w.bool(self.length_enable);
        w.u8(self.volume);
        w.bool(self.force75);
        w.u32(self.freq_timer);
        w.u16(self.freq);
        w.u8(self.pos);
        w.bool(self.on);
        w.buf.extend_from_slice(&self.ram);
    }
    fn load(&mut self, r: &mut crate::state::Reader) -> Result<(), String> {
        self.dimension = r.bool()?;
        self.bank = r.u8()? & 1;
        self.dac_on = r.bool()?;
        self.length = r.u16()?;
        self.length_enable = r.bool()?;
        self.volume = r.u8()? & 3;
        self.force75 = r.bool()?;
        self.freq_timer = r.u32()?.max(1);
        self.freq = r.u16()? & 0x7FF;
        self.pos = r.u8()? & 63;
        self.on = r.bool()?;
        self.ram = r.array()?;
        Ok(())
    }
}

impl Noise {
    fn save(&self, w: &mut crate::state::Writer) {
        w.u32(self.freq_timer);
        w.u8(self.divisor);
        w.u8(self.shift);
        w.bool(self.width);
        w.u16(self.lfsr);
        w.u8(self.length);
        w.bool(self.length_enable);
        self.env.save(w);
        w.bool(self.on);
    }
    fn load(&mut self, r: &mut crate::state::Reader) -> Result<(), String> {
        self.freq_timer = r.u32()?.max(1);
        self.divisor = r.u8()? & 7;
        self.shift = r.u8()? & 0xF;
        self.width = r.bool()?;
        self.lfsr = r.u16()?;
        self.length = r.u8()?;
        self.length_enable = r.bool()?;
        self.env.load(r)?;
        self.on = r.bool()?;
        Ok(())
    }
}

impl DirectSound {
    fn save(&self, w: &mut crate::state::Writer) {
        w.buf.extend_from_slice(&self.fifo);
        w.u8(self.fifo_count as u8);
        w.u8(self.fifo_read as u8);
    }
    fn load(&mut self, r: &mut crate::state::Reader) -> Result<(), String> {
        self.fifo = r.array()?;
        self.fifo_count = (r.u8()? as usize).min(32);
        self.fifo_read = (r.u8()? as usize) % 32;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Master enable on, every PSG channel to both sides at full volume.
    fn enabled() -> Apu {
        let mut apu = Apu::new();
        apu.write16(0x84, 0x80);
        apu.write16(0x80, 0xFF77);
        apu.write16(0x82, 2);
        apu
    }

    fn left_samples(apu: &mut Apu) -> Vec<f32> {
        apu.take_audio()
            .samples
            .iter()
            .step_by(2)
            .copied()
            .collect()
    }

    #[test]
    fn square_trigger_produces_audio() {
        let mut apu = enabled();
        apu.write16(0x62, 0xF080);
        apu.write16(0x64, 0x8000 | 1024);
        apu.step(CYCLES_PER_SAMPLE * 4);
        let audio = apu.take_audio();
        assert_eq!(audio.samples.len(), 8);
        assert!(audio.samples.iter().any(|&s| s != 0.0));
    }

    #[test]
    fn square_channel_frequency_from_register() {
        let mut apu = enabled();
        // f = 1792: period (2048 - 1792) * 16 * 8 = 32768 cycles = 512 Hz,
        // i.e. 64 samples per period; a 50 % duty cycle flips twice.
        apu.write16(0x68, 0xF000 | (2 << 6));
        apu.write16(0x6C, 0x8000 | 1792);
        apu.step(32768);
        let left = left_samples(&mut apu);
        assert_eq!(left.len(), 64);
        let flips = left
            .windows(2)
            .filter(|w| (w[0] > 0.0) != (w[1] > 0.0))
            .count();
        assert_eq!(flips, 2, "{left:?}");
        // The second period repeats the first exactly.
        apu.step(32768);
        assert_eq!(left_samples(&mut apu), left);
    }

    #[test]
    fn noise_period_uses_dividing_ratio_and_shift() {
        let mut n = Noise::new();
        n.set_ctrl(0x8000 | (3 << 4)); // r = 0, s = 3
        assert_eq!(n.period(), 32 << 3);
        n.set_ctrl(0x8000 | 7); // r = 7, s = 0
        assert_eq!(n.period(), 64 * 7);
        n.set_ctrl(0x8000 | 1 | (14 << 4));
        let before = n.lfsr;
        n.clock(1 << 20);
        assert_eq!(n.lfsr, before, "s = 14 never clocks");
    }

    #[test]
    fn wave_length_and_volume_come_from_sound3cnt_h() {
        let mut w = Wave::new();
        w.set_ctrl_l(0x80);
        w.set_len_vol(0xFF | (2 << 13));
        assert_eq!(w.length, 1);
        assert_eq!(w.volume, 2);
        w.set_len_vol(0x8000);
        assert!(w.force75);
        assert_eq!(w.length, 256);
        w.set_freq_ctrl(0x8000 | 2047);
        assert_eq!(w.period(), 8);
        assert!(w.on);
    }

    #[test]
    fn wave_ram_writes_land_in_the_bank_not_playing() {
        let mut apu = enabled();
        apu.write16(0x70, 0x80); // bank 0 plays, CPU sees bank 1
        apu.write16(0x90, 0x1234);
        apu.write16(0x9E, 0xABCD);
        assert_eq!(apu.read16(0x90), 0x1234);
        assert_eq!(apu.read16(0x9E), 0xABCD);
        assert_eq!(apu.wave.ram[16], 0x34);
        assert_eq!(apu.wave.ram[31], 0xAB);
        apu.write16(0x70, 0x80 | 0x40); // bank 1 plays, CPU sees bank 0
        assert_eq!(apu.read16(0x90), 0);
        apu.write16(0x90, 0x5678);
        assert_eq!(apu.wave.ram[0], 0x78);
        assert_eq!(apu.wave.ram[16], 0x34, "bank 1 untouched");
    }

    #[test]
    fn frame_sequencer_runs_at_512_hz() {
        let mut apu = enabled();
        // Envelope: volume 15, decreasing, period 1 -> one step per envelope
        // clock, which is sequencer step 7 (after 7 x 32768 cycles) and then
        // every 262144 cycles.
        apu.write16(0x62, 0xF100);
        apu.write16(0x64, 0x8000);
        assert_eq!(apu.sq1.env.volume, 15);
        apu.step(7 * 32768 - 1);
        assert_eq!(apu.sq1.env.volume, 15);
        apu.step(1);
        assert_eq!(apu.sq1.env.volume, 14);
        apu.step(262144 * 2);
        assert_eq!(apu.sq1.env.volume, 12);
    }

    #[test]
    fn run_timer_counts_expiries_in_bulk() {
        let mut t = 10;
        assert_eq!(run_timer(&mut t, 16, 4), 0);
        assert_eq!(t, 6);
        assert_eq!(run_timer(&mut t, 16, 6), 1);
        assert_eq!(t, 16);
        // Expiries at cycles 16 and 32 of the 40; 8 cycles remain of the next.
        assert_eq!(run_timer(&mut t, 16, 40), 2);
        assert_eq!(t, 8);
    }

    #[test]
    fn directsound_mixes_into_selected_channels() {
        let mut apu = enabled();
        // A at 100 % to the left only, clocked by timer 0.
        apu.write16(0x82, (1 << 2) | (1 << 9));
        apu.write16(0xA0, 0x007F);
        apu.timer_overflow(0);
        apu.step(CYCLES_PER_SAMPLE);
        let s = apu.take_audio().samples;
        assert!(s[0] > 0.9, "left {}", s[0]);
        assert_eq!(s[1], 0.0, "right is silent");
        // Now B at 50 % to the right, clocked by timer 1.
        apu.write16(0x82, (1 << 12) | (1 << 14));
        apu.write16(0xA4, 0x0040);
        apu.timer_overflow(1);
        apu.step(CYCLES_PER_SAMPLE);
        let s = apu.take_audio().samples;
        assert_eq!(s[0], 0.0);
        assert_eq!(s[1], 0.25, "0x40 * 2 / 512");
    }

    #[test]
    fn psg_volume_and_master_volume_scale_the_mix() {
        let mut apu = enabled();
        apu.write16(0x62, 0xF0C0); // volume 15, duty 3 (75 %: starts high)
        apu.write16(0x64, 0x8000);
        apu.step(CYCLES_PER_SAMPLE);
        // 15 * 7 = 105 above the bias; the default 9-bit resolution drops
        // the lowest bit of 617, giving 616 - 512 = 104.
        let full = apu.take_audio().samples[0];
        assert_eq!(full, 104.0 / 512.0);
        apu.write16(0x82, 1); // 50 %: 52 -> 564, even
        apu.step(CYCLES_PER_SAMPLE);
        assert_eq!(apu.take_audio().samples[0], 52.0 / 512.0);
        apu.write16(0x82, 2);
        apu.write16(0x80, 0xFF37); // left master 3 (45 -> 556), right 7
        apu.step(CYCLES_PER_SAMPLE);
        let s = apu.take_audio().samples;
        assert_eq!(s[0], 44.0 / 512.0);
        assert_eq!(s[1], 104.0 / 512.0);
    }

    #[test]
    fn soundbias_offsets_and_reduces_resolution() {
        let mut apu = enabled();
        apu.write16(0x88, 0x100);
        apu.step(CYCLES_PER_SAMPLE);
        assert_eq!(apu.take_audio().samples[0], -0.5);
        apu.write16(0x88, 0x200 | (3 << 14));
        apu.write16(0x62, 0xF0C0);
        apu.write16(0x64, 0x8000);
        apu.step(CYCLES_PER_SAMPLE);
        // 105 + 0x200 = 617 -> 6-bit resolution keeps 608.
        assert_eq!(apu.take_audio().samples[0], (608 - 512) as f32 / 512.0);
    }

    #[test]
    fn master_disable_silences_output() {
        let mut apu = enabled();
        apu.write16(0x62, 0xF0C0);
        apu.write16(0x64, 0x8000);
        apu.step(CYCLES_PER_SAMPLE);
        assert!(apu.take_audio().samples[0] != 0.0);
        apu.write16(0x84, 0);
        assert_eq!(apu.read16(0x80), 0);
        assert_eq!(apu.read_soundcnt_x(), 0);
        apu.step(CYCLES_PER_SAMPLE * 4);
        assert!(apu.take_audio().samples.iter().all(|&s| s == 0.0));
        // PSG registers are ignored while off; SOUNDCNT_H is not.
        apu.write16(0x62, 0xF0C0);
        assert_eq!(apu.sq1.env.initial, 0);
        apu.write16(0x82, 0x0F);
        assert_eq!(apu.read16(0x82), 0x0F);
    }

    #[test]
    fn fifo_produces_output() {
        let mut apu = enabled();
        apu.write16(0x82, (1 << 2) | (1 << 8) | (1 << 9));
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
    fn fifo_halfword_writes_queue_both_bytes() {
        let mut apu = Apu::new();
        apu.write16(0xA0, 0x0201);
        apu.write16(0xA2, 0x0403);
        assert_eq!(apu.fifo_a_count(), 4);
        assert_eq!(apu.dsa.sample(), 1);
        assert_eq!(apu.dsa.sample(), 2);
        assert_eq!(apu.dsa.sample(), 3);
        assert_eq!(apu.dsa.sample(), 4);
    }

    #[test]
    fn fifo_samples_are_signed() {
        let mut apu = enabled();
        apu.write16(0x82, (1 << 2) | (1 << 9));
        apu.write16(0xA0, 0x80);
        apu.timer_overflow(0);
        apu.step(CYCLES_PER_SAMPLE);
        let audio = apu.take_audio();
        assert_eq!(audio.samples[0], -1.0);
    }

    #[test]
    fn fifo_empty_is_silent() {
        let mut apu = enabled();
        apu.write16(0x82, (1 << 2) | (1 << 8) | (1 << 9));
        apu.step(CYCLES_PER_SAMPLE);
        let audio = apu.take_audio();
        assert_eq!(audio.samples, vec![0.0, 0.0]);
    }
}
