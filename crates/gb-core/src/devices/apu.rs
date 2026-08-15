//! Audio Processing Unit (APU): NR10-52, four channels, and mixing.
//!
//! The DMG APU produces one stereo sample pair every 512 T-cycles (8192 Hz)
//! while a 512 Hz frame sequencer drives the length counters, envelope, and
//! channel-1 sweep. Samples are accumulated into an [`AudioBuffer`] drained by
//! the frontend via [`Gb::take_audio`].

use emu_core::audio::AudioBuffer;
use emu_core::device::Device;
use emu_core::bus::Bus;

const DUTY: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1], // 12.5%
    [1, 0, 0, 0, 0, 0, 0, 1], // 25%
    [1, 0, 0, 0, 0, 0, 1, 1], // 50%
    [1, 1, 1, 1, 0, 0, 1, 1], // 75%
];

/// Convert a 4-bit (or wave) amplitude to a `-1.0..=1.0` sample.
fn dac(v: u8) -> f32 {
    (v as f32) / 7.5 - 1.0
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
    fn reload(&mut self, nr: u8) {
        self.volume = nr >> 4;
        self.up = nr & 0x08 != 0;
        self.period = nr & 0x07;
        self.timer = self.period;
    }
    /// Called on frame-sequencer envelope steps; returns true if a change
    /// happened (used only to gate volume updates).
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
    length: u16,
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

    fn trigger(&mut self, nr1: u8, nr2: u8, nr3: u8, nr4: u8) {
        self.length = 64 - (nr1 & 0x3F) as u16;
        self.duty = nr1 >> 6;
        self.env.reload(nr2);
        self.freq = nr3 as u16 | (((nr4 & 0x07) as u16) << 8);
        self.freq_timer = (2048 - self.freq as u32) * 4;
        self.phase = 0;
        self.sweep_timer = self.sweep_period;
        self.sweep_enabled = self.sweep_period != 0 || self.sweep_shift != 0;
        if self.sweep_shift != 0 {
            self.calc_sweep(true);
        }
        self.on = true;
    }

    fn calc_sweep(&mut self, load: bool) {
        let delta = self.freq >> self.sweep_shift;
        let new_freq = if self.sweep_negate {
            self.freq.wrapping_sub(delta)
        } else {
            self.freq + delta
        };
        if new_freq > 0x7FF {
            self.on = false;
        }
        if load {
            self.freq = new_freq & 0x7FF;
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
        if DUTY[self.duty as usize][self.phase as usize] == 1 {
            dac(self.env.volume)
        } else {
            dac(0)
        }
    }

    fn sweep_tick(&mut self) {
        if !self.sweep_enabled {
            return;
        }
        if self.sweep_timer > 0 {
            self.sweep_timer -= 1;
            return;
        }
        self.sweep_timer = self.sweep_period;
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

    fn trigger(&mut self, nr1: u8, nr3: u8, nr4: u8) {
        self.length = 256 - nr1 as u16;
        self.freq = nr3 as u16 | (((nr4 & 0x07) as u16) << 8);
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
        let nibble = if self.phase & 1 == 0 {
            byte >> 4
        } else {
            byte & 0x0F
        };
        dac(nibble >> (self.volume_shift - 1))
    }
}

#[derive(Clone, Copy)]
struct Noise {
    freq_timer: u32,
    divisor: u8,
    shift: u8,
    width: bool,
    lfsr: u16,
    length: u16,
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

    fn trigger(&mut self, nr1: u8, nr2: u8) {
        self.length = 64 - (nr1 & 0x3F) as u16;
        self.env.reload(nr2);
        self.lfsr = 0x7FFF;
        self.on = true;
    }

    fn tick_freq(&mut self) {
        if self.freq_timer == 0 {
            self.freq_timer = self.clock();
            let xor = (self.lfsr & 1) ^ ((self.lfsr >> 1) & 1);
            self.lfsr >>= 1;
            self.lfsr |= xor << 14;
            if self.width {
                self.lfsr = (self.lfsr & !0x40) | (xor << 6);
            }
        }
        self.freq_timer -= 1;
    }

    fn clock(&self) -> u32 {
        let divisor_clock = if self.divisor == 0 { 8 } else { (self.divisor as u32) << 4 };
        divisor_clock << self.shift
    }

    fn sample(&self) -> f32 {
        if !self.on {
            return 0.0;
        }
        if self.lfsr & 1 == 0 {
            dac(self.env.volume)
        } else {
            dac(0)
        }
    }
}

pub struct Apu {
    pub buffer: AudioBuffer,
    power: bool,
    cycle_accum: u32,
    frame_accum: u32,
    frame_step_n: u32,
    ch1: Square,
    ch2: Square,
    ch3: Wave,
    ch4: Noise,
    /// Number of audio samples produced (for diagnostics).
    pub produced: u64,
}

impl Apu {
    pub fn new() -> Apu {
        Apu {
            buffer: AudioBuffer::new(),
            power: false,
            cycle_accum: 0,
            frame_accum: 0,
            frame_step_n: 0,
            ch1: Square::new(),
            ch2: Square::new(),
            ch3: Wave::new(),
            ch4: Noise::new(),
            produced: 0,
        }
    }

    fn set_power(&mut self, io: &mut [u8; 0x80], on: bool) {
        if self.power == on {
            return;
        }
        self.power = on;
        if !on {
            // Powering off clears all audio registers.
            for r in 0x10..=0x25 {
                io[r] = 0;
            }
            for r in 0x30..=0x3F {
                io[r] = 0;
            }
            io[0x26] = 0;
            self.ch1 = Square::new();
            self.ch2 = Square::new();
            self.ch3 = Wave::new();
            self.ch4 = Noise::new();
        }
    }

    fn channel_status(&self) -> u8 {
        (if self.ch1.on { 1 } else { 0 })
            | (if self.ch2.on { 2 } else { 0 })
            | (if self.ch3.on { 4 } else { 0 })
            | (if self.ch4.on { 8 } else { 0 })
    }

    pub fn read(&self, addr: u16, io: &[u8; 0x80]) -> u8 {
        let off = (addr - 0xFF00) as usize;
        match addr {
            0xFF10 => io[off] | 0x80,
            0xFF11 | 0xFF16 | 0xFF21 => io[off] | 0x3F,
            0xFF12 | 0xFF17 | 0xFF22 => io[off],
            0xFF13 | 0xFF18 | 0xFF1D | 0xFF23 => 0xFF,
            0xFF14 | 0xFF19 | 0xFF1E | 0xFF24 => io[off] | 0xBF,
            0xFF15 | 0xFF1F => 0xFF,
            0xFF1A => io[off] | 0x7F,
            0xFF1B | 0xFF1C => io[off],
            0xFF20 => io[off] | 0x3F,
            0xFF25 => io[off],
            0xFF26 => {
                if self.power {
                    io[off] | 0x70 | self.channel_status()
                } else {
                    0x70
                }
            }
            _ => io[off],
        }
    }

    pub fn write(&mut self, addr: u16, value: u8, io: &mut [u8; 0x80]) {
        let off = (addr - 0xFF00) as usize;
        match addr {
            0xFF10 => {
                io[off] = value;
                self.ch1.sweep_period = (value >> 4) & 0x07;
                self.ch1.sweep_negate = value & 0x08 != 0;
                self.ch1.sweep_shift = value & 0x07;
            }
            0xFF11 => {
                io[off] = value;
                self.ch1.length = 64 - (value & 0x3F) as u16;
                self.ch1.duty = value >> 6;
            }
            0xFF12 => {
                io[off] = value;
                self.ch1.env.reload(value);
            }
            0xFF13 => {
                io[off] = value;
            }
            0xFF14 => {
                io[off] = value;
                self.ch1.length_enable = value & 0x40 != 0;
                self.ch1.freq = self.ch1.freq & 0x700 | io[0x13] as u16;
                self.ch1.freq |= ((value & 0x07) as u16) << 8;
                if value & 0x80 != 0 {
                    self.ch1.trigger(io[0x11], io[0x12], io[0x13], value);
                }
            }
            0xFF16 => {
                io[off] = value;
                self.ch2.length = 64 - (value & 0x3F) as u16;
                self.ch2.duty = value >> 6;
            }
            0xFF17 => {
                io[off] = value;
                self.ch2.env.reload(value);
            }
            0xFF18 => {
                io[off] = value;
            }
            0xFF19 => {
                io[off] = value;
                self.ch2.length_enable = value & 0x40 != 0;
                self.ch2.freq = io[0x18] as u16 | (((value & 0x07) as u16) << 8);
                if value & 0x80 != 0 {
                    self.ch2.trigger(io[0x16], io[0x17], io[0x18], value);
                }
            }
            0xFF1A => {
                io[off] = value;
                self.ch3.dac_on = value & 0x80 != 0;
                if !self.ch3.dac_on {
                    self.ch3.on = false;
                }
            }
            0xFF1B => {
                io[off] = value;
            }
            0xFF1C => {
                io[off] = value;
                self.ch3.volume_shift = (value >> 5) & 0x03;
            }
            0xFF1D => {
                io[off] = value;
            }
            0xFF1E => {
                io[off] = value;
                self.ch3.length_enable = value & 0x40 != 0;
                self.ch3.freq = io[0x1D] as u16 | (((value & 0x07) as u16) << 8);
                if value & 0x80 != 0 {
                    self.ch3.trigger(io[0x1B], io[0x1D], value);
                }
            }
            0xFF20 => {
                io[off] = value;
                self.ch4.length = 64 - (value & 0x3F) as u16;
            }
            0xFF21 => {
                io[off] = value;
                self.ch4.env.reload(value);
            }
            0xFF22 => {
                io[off] = value;
                self.ch4.divisor = value >> 4;
                self.ch4.width = value & 0x08 != 0;
                self.ch4.shift = value & 0x07;
            }
            0xFF23 => {
                io[off] = value;
                self.ch4.length_enable = value & 0x40 != 0;
                if value & 0x80 != 0 {
                    self.ch4.trigger(io[0x20], io[0x21]);
                }
            }
            0xFF24 => {
                io[off] = value;
            }
            0xFF25 => {
                io[off] = value;
            }
            0xFF26 => {
                io[off] = value | 0x70;
                self.set_power(io, value & 0x80 != 0);
            }
            0xFF30..=0xFF3F => {
                io[off] = value;
            }
            _ => {}
        }
    }

    fn frame_step(&mut self) {
        // Step 0..7: length on even steps, sweep on 1/5, envelope on 3/7.
        let step = self.frame_step_n;
        self.frame_step_n = (self.frame_step_n + 1) % 8;
        if step % 2 == 0 {
            self.tick_lengths();
        }
        match step {
            1 | 5 => {
                self.ch1.sweep_tick();
            }
            3 | 7 => {
                self.ch1.env.tick();
                self.ch2.env.tick();
                self.ch4.env.tick();
            }
            _ => {}
        }
    }

    fn tick_lengths(&mut self) {
        if self.ch1.length_enable {
            self.ch1.length = self.ch1.length.saturating_sub(1);
            if self.ch1.length == 0 {
                self.ch1.on = false;
            }
        }
        if self.ch2.length_enable {
            self.ch2.length = self.ch2.length.saturating_sub(1);
            if self.ch2.length == 0 {
                self.ch2.on = false;
            }
        }
        if self.ch3.length_enable {
            self.ch3.length = self.ch3.length.saturating_sub(1);
            if self.ch3.length == 0 {
                self.ch3.on = false;
            }
        }
        if self.ch4.length_enable {
            self.ch4.length = self.ch4.length.saturating_sub(1);
            if self.ch4.length == 0 {
                self.ch4.on = false;
            }
        }
    }

    fn sample(&mut self, io: &[u8; 0x80], wave_ram: &[u8; 16]) {
        let s1 = self.ch1.sample();
        let s2 = self.ch2.sample();
        let s3 = self.ch3.sample(wave_ram);
        let s4 = self.ch4.sample();

        let nr51 = io[0x25];
        let left_vol = (io[0x24] & 0x07) as f32 / 7.0;
        let right_vol = ((io[0x24] >> 4) & 0x07) as f32 / 7.0;

        let l = (if nr51 & 0x10 != 0 { s1 } else { 0.0 })
            + (if nr51 & 0x20 != 0 { s2 } else { 0.0 })
            + (if nr51 & 0x40 != 0 { s3 } else { 0.0 })
            + (if nr51 & 0x80 != 0 { s4 } else { 0.0 });
        let r = (if nr51 & 0x01 != 0 { s1 } else { 0.0 })
            + (if nr51 & 0x02 != 0 { s2 } else { 0.0 })
            + (if nr51 & 0x04 != 0 { s3 } else { 0.0 })
            + (if nr51 & 0x08 != 0 { s4 } else { 0.0 });

        let left = (l * left_vol).clamp(-1.0, 1.0);
        let right = (r * right_vol).clamp(-1.0, 1.0);
        self.buffer.push(left, right);
        self.produced += 1;
    }

    pub fn step(&mut self, cycles: u32, io: &mut [u8; 0x80], wave_ram: &[u8; 16]) {
        if !self.power {
            return;
        }
        for _ in 0..cycles {
            self.ch1.tick_freq();
            self.ch2.tick_freq();
            self.ch3.tick_freq();
            self.ch4.tick_freq();
            self.cycle_accum += 1;
            if self.cycle_accum >= 512 {
                self.cycle_accum -= 512;
                self.sample(io, wave_ram);
            }
        }
        self.frame_accum += cycles;
        while self.frame_accum >= 8192 {
            self.frame_accum -= 8192;
            self.frame_step();
        }
    }
}

impl Default for Apu {
    fn default() -> Self {
        Self::new()
    }
}

impl Device for Apu {
    fn kind(&self) -> &'static str {
        "APU"
    }

    fn reset(&mut self) {
        *self = Apu::new();
    }

    fn tick(&mut self, cycles: u32, bus: &mut dyn Bus) {
        if let Some(gb) = bus.as_any_mut().downcast_mut::<crate::bus::Bus>() {
            let io = &mut gb.io;
            let mut wave_ram = [0u8; 16];
            wave_ram.copy_from_slice(&io[0x30..0x40]);
            self.step(cycles, io, &wave_ram);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regs() -> ([u8; 0x80], [u8; 16]) {
        let io = [0u8; 0x80];
        let wave_ram = [0u8; 16];
        (io, wave_ram)
    }

    #[test]
    fn ch2_square_produces_samples() {
        let (mut io, wave_ram) = regs();
        let mut apu = Apu::new();
        io[0x26] = 0x80;
        apu.write(0xFF26, 0x80, &mut io); // power on
        apu.write(0xFF16, 0x80, &mut io); // duty 50%, length load
        apu.write(0xFF17, 0xF0, &mut io); // volume 15, no envelope
        apu.write(0xFF18, 0x00, &mut io); // freq low
        apu.write(0xFF24, 0x77, &mut io); // max left/right master volume
        apu.write(0xFF25, 0xFF, &mut io); // all channels -> both sides
        apu.write(0xFF19, 0x80, &mut io); // trigger + freq high
        assert!(apu.ch2.on, "CH2 enabled after trigger");

        apu.step(8192, &mut io, &wave_ram);
        assert!(apu.produced >= 8, "samples produced at 8192 Hz: {}", apu.produced);

        // Non-silent: CH2 is a 50% square at max volume, panned to both sides.
        let has_audio = apu.buffer.samples.iter().any(|&s| s.abs() > 0.1);
        assert!(has_audio, "mixed output is non-silent");
    }

    #[test]
    fn power_off_silences_and_clears() {
        let (mut io, wave_ram) = regs();
        let mut apu = Apu::new();
        io[0x26] = 0x80;
        apu.write(0xFF26, 0x80, &mut io);
        apu.write(0xFF26, 0x00, &mut io); // power off
        assert!(!apu.power);
        apu.step(2048, &mut io, &wave_ram);
        assert_eq!(apu.buffer.samples.len(), 0, "no samples while powered off");
        assert_eq!(io[0x24], 0, "NR50 cleared on power-off");
    }
}
