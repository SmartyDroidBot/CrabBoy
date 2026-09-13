//! Full-machine save states in a hand-rolled binary format.
//!
//! A state is `b"CBSV"` (4-byte magic) + a little-endian u32 version, followed
//! by the serialized [`Cpu`] and then the [`Bus`]. All lengths are little-endian
//! u32-prefixed so a reader can bounds-check every field; loading validates the
//! magic, version, and every field, and never panics on malformed input.

use crate::bus::Bus;
use crate::cartridge::{Cartridge, MbcType};
use crate::cpu::Cpu;
use crate::devices::apu::{Apu, Envelope, Noise, Square, Wave};
use crate::devices::joypad::Joypad;
use crate::devices::ppu::{Ppu, Sprite, SCREEN_H, SCREEN_W};
use crate::devices::timer::Timer;
use emu_core::audio::AudioBuffer;

/// Magic header identifying a CrabBoy save state.
pub const STATE_MAGIC: &[u8; 4] = b"CBSV";
/// Current save-state format version.
pub const STATE_VERSION: u32 = 2;

/// Encode a cartridge's MBC type as a single byte.
fn mbc_to_u8(mbc: MbcType) -> u8 {
    match mbc {
        MbcType::RomOnly => 0,
        MbcType::Mbc1 => 1,
        MbcType::Mbc2 => 2,
        MbcType::Mbc3 => 3,
        MbcType::Mbc5 => 4,
        MbcType::Other => 5,
    }
}

fn mbc_from_u8(v: u8) -> Result<MbcType, String> {
    Ok(match v {
        0 => MbcType::RomOnly,
        1 => MbcType::Mbc1,
        2 => MbcType::Mbc2,
        3 => MbcType::Mbc3,
        4 => MbcType::Mbc5,
        5 => MbcType::Other,
        _ => return Err(format!("unknown MBC discriminator {v}")),
    })
}

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Writer { buf: Vec::new() }
    }
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }
    fn raw(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }
    fn bytes(&mut self, v: &[u8]) {
        self.u32(v.len() as u32);
        self.raw(v);
    }
    fn str(&mut self, v: &str) {
        self.bytes(v.as_bytes());
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| "save state length overflow".to_string())?;
        let slice = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| "save state truncated".to_string())?;
        self.pos = end;
        Ok(slice)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> Result<bool, String> {
        Ok(self.u8()? != 0)
    }
    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, String> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let b = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(b);
        Ok(a)
    }
    fn bytes(&mut self) -> Result<Vec<u8>, String> {
        let n = self.u32()? as usize;
        if n > self.data.len() - self.pos {
            return Err("save state length prefix exceeds remaining data".to_string());
        }
        Ok(self.take(n)?.to_vec())
    }
    fn str(&mut self) -> Result<String, String> {
        String::from_utf8(self.bytes()?).map_err(|_| "invalid UTF-8 in save state".to_string())
    }
    fn finish(&self) -> Result<(), String> {
        if self.pos == self.data.len() {
            Ok(())
        } else {
            Err(format!(
                "save state has {} trailing bytes",
                self.data.len() - self.pos
            ))
        }
    }
}

fn save_cpu(w: &mut Writer, cpu: &Cpu) {
    w.u8(cpu.a);
    w.u8(cpu.f);
    w.u8(cpu.b);
    w.u8(cpu.c);
    w.u8(cpu.d);
    w.u8(cpu.e);
    w.u8(cpu.h);
    w.u8(cpu.l);
    w.u16(cpu.sp);
    w.u16(cpu.pc);
    w.bool(cpu.ime);
    w.bool(cpu.ei_pending);
    w.bool(cpu.halted);
    w.bool(cpu.stopped);
    w.bool(cpu.halt_bug);
    w.u64(cpu.timer_interrupts);
}

fn load_cpu(r: &mut Reader) -> Result<Cpu, String> {
    Ok(Cpu {
        a: r.u8()?,
        f: r.u8()?,
        b: r.u8()?,
        c: r.u8()?,
        d: r.u8()?,
        e: r.u8()?,
        h: r.u8()?,
        l: r.u8()?,
        sp: r.u16()?,
        pc: r.u16()?,
        ime: r.bool()?,
        ei_pending: r.bool()?,
        halted: r.bool()?,
        stopped: r.bool()?,
        halt_bug: r.bool()?,
        timer_interrupts: r.u64()?,
    })
}

fn save_cartridge(w: &mut Writer, c: &Cartridge) {
    w.str(&c.title);
    w.u8(mbc_to_u8(c.mbc));
    w.u8(c.cgb_flag);
    w.bytes(&c.rom);
    w.bytes(&c.ram);
    w.u32(c.num_rom_banks as u32);
    w.u32(c.num_ram_banks as u32);
    w.u32(c.rom_bank as u32);
    w.u32(c.ram_bank as u32);
    w.bool(c.bank_mode);
    w.bool(c.ram_enabled);
    w.bool(c.has_battery);
    w.bool(c.has_rtc);
    w.bool(c.rtc_selected);
    w.u8(c.rtc_register);
    w.raw(&c.rtc);
    w.raw(&c.rtc_latched);
    w.u8(c.rtc_latch);
    w.u32(c.rtc_cycles);
    w.bool(c.sram_dirty);
}

fn load_cartridge(r: &mut Reader) -> Result<Cartridge, String> {
    Ok(Cartridge {
        title: r.str()?,
        mbc: mbc_from_u8(r.u8()?)?,
        cgb_flag: r.u8()?,
        rom: r.bytes()?,
        ram: r.bytes()?,
        num_rom_banks: r.u32()? as usize,
        num_ram_banks: r.u32()? as usize,
        rom_bank: r.u32()? as usize,
        ram_bank: r.u32()? as usize,
        bank_mode: r.bool()?,
        ram_enabled: r.bool()?,
        has_battery: r.bool()?,
        has_rtc: r.bool()?,
        rtc_selected: r.bool()?,
        rtc_register: r.u8()?,
        rtc: r.fixed()?,
        rtc_latched: r.fixed()?,
        rtc_latch: r.u8()?,
        rtc_cycles: r.u32()?,
        sram_dirty: r.bool()?,
    })
}

fn save_sprite(w: &mut Writer, s: &Sprite) {
    w.u16(s.x as u16);
    w.u8(s.y);
    w.u8(s.tile);
    w.u8(s.attr);
    w.u8(s.height);
}

fn load_sprite(r: &mut Reader) -> Result<Sprite, String> {
    Ok(Sprite {
        x: r.u16()? as i16,
        y: r.u8()?,
        tile: r.u8()?,
        attr: r.u8()?,
        height: r.u8()?,
    })
}

fn save_ppu(w: &mut Writer, p: &Ppu) {
    w.u8(p.ly);
    w.u8(p.mode);
    w.u32(p.dot);
    w.u32(p.mode3_cycles);
    w.u8(p.prev_mode);
    w.bool(p.prev_coinc);
    for s in &p.line_sprites {
        save_sprite(w, s);
    }
    w.u32(p.line_sprite_count as u32);
    w.u64(p.vblank_interrupts);
    w.raw(&p.frame_buffer);
    w.bool(p.cgb);
    w.raw(&p.bg_pal);
    w.raw(&p.obj_pal);
}

fn load_ppu(r: &mut Reader) -> Result<Ppu, String> {
    let ly = r.u8()?;
    let mode = r.u8()?;
    let dot = r.u32()?;
    let m3 = r.u32()?;
    let pm = r.u8()?;
    let pcs = r.bool()?;
    let mut line_sprites = [Sprite {
        x: 0,
        y: 0,
        tile: 0,
        attr: 0,
        height: 8,
    }; 10];
    for s in line_sprites.iter_mut() {
        *s = load_sprite(r)?;
    }
    Ok(Ppu {
        ly,
        mode,
        dot,
        mode3_cycles: m3,
        prev_mode: pm,
        prev_coinc: pcs,
        line_sprites,
        line_sprite_count: r.u32()? as usize,
        vblank_interrupts: r.u64()?,
        frame_buffer: r.fixed()?,
        cgb: r.bool()?,
        bg_pal: r.fixed()?,
        obj_pal: r.fixed()?,
        rgb_buffer: vec![0; SCREEN_W * SCREEN_H * 3],
    })
}

fn save_timer(w: &mut Writer, t: &Timer) {
    w.u16(t.div_counter);
    w.u64(t.abs_cycles);
    w.u64(t.reload_deadline);
    w.u8(t.reload_value);
    w.bool(t.reload_pending);
}

fn load_timer(r: &mut Reader) -> Result<Timer, String> {
    Ok(Timer {
        div_counter: r.u16()?,
        abs_cycles: r.u64()?,
        reload_deadline: r.u64()?,
        reload_value: r.u8()?,
        reload_pending: r.bool()?,
    })
}

fn save_envelope(w: &mut Writer, e: &Envelope) {
    w.u8(e.volume);
    w.bool(e.up);
    w.u8(e.period);
    w.u8(e.timer);
}

fn load_envelope(r: &mut Reader) -> Result<Envelope, String> {
    Ok(Envelope {
        volume: r.u8()?,
        up: r.bool()?,
        period: r.u8()?,
        timer: r.u8()?,
    })
}

fn save_square(w: &mut Writer, s: &Square) {
    w.u8(s.duty);
    w.u32(s.freq_timer);
    w.u16(s.freq);
    w.u8(s.phase);
    w.u16(s.length);
    w.bool(s.length_enable);
    save_envelope(w, &s.env);
    w.u8(s.sweep_period);
    w.bool(s.sweep_negate);
    w.u8(s.sweep_shift);
    w.u8(s.sweep_timer);
    w.bool(s.sweep_enabled);
    w.bool(s.on);
}

fn load_square(r: &mut Reader) -> Result<Square, String> {
    Ok(Square {
        duty: r.u8()?,
        freq_timer: r.u32()?,
        freq: r.u16()?,
        phase: r.u8()?,
        length: r.u16()?,
        length_enable: r.bool()?,
        env: load_envelope(r)?,
        sweep_period: r.u8()?,
        sweep_negate: r.bool()?,
        sweep_shift: r.u8()?,
        sweep_timer: r.u8()?,
        sweep_enabled: r.bool()?,
        on: r.bool()?,
    })
}

fn save_wave(w: &mut Writer, s: &Wave) {
    w.u32(s.freq_timer);
    w.u16(s.freq);
    w.u8(s.phase);
    w.u16(s.length);
    w.bool(s.length_enable);
    w.u8(s.volume_shift);
    w.bool(s.dac_on);
    w.bool(s.on);
}

fn load_wave(r: &mut Reader) -> Result<Wave, String> {
    Ok(Wave {
        freq_timer: r.u32()?,
        freq: r.u16()?,
        phase: r.u8()?,
        length: r.u16()?,
        length_enable: r.bool()?,
        volume_shift: r.u8()?,
        dac_on: r.bool()?,
        on: r.bool()?,
    })
}

fn save_noise(w: &mut Writer, s: &Noise) {
    w.u32(s.freq_timer);
    w.u8(s.divisor);
    w.u8(s.shift);
    w.bool(s.width);
    w.u16(s.lfsr);
    w.u16(s.length);
    w.bool(s.length_enable);
    save_envelope(w, &s.env);
    w.bool(s.on);
}

fn load_noise(r: &mut Reader) -> Result<Noise, String> {
    Ok(Noise {
        freq_timer: r.u32()?,
        divisor: r.u8()?,
        shift: r.u8()?,
        width: r.bool()?,
        lfsr: r.u16()?,
        length: r.u16()?,
        length_enable: r.bool()?,
        env: load_envelope(r)?,
        on: r.bool()?,
    })
}

fn save_apu(w: &mut Writer, a: &Apu) {
    w.bool(a.power);
    w.u32(a.cycle_accum);
    w.u32(a.frame_accum);
    w.u32(a.frame_step_n);
    save_square(w, &a.ch1);
    save_square(w, &a.ch2);
    save_wave(w, &a.ch3);
    save_noise(w, &a.ch4);
}

fn load_apu(r: &mut Reader) -> Result<Apu, String> {
    // The audio buffer is deliberately not serialized; a fresh empty buffer is
    // installed on load so stale audio never leaks into the restored stream.
    Ok(Apu {
        buffer: AudioBuffer::new(),
        power: r.bool()?,
        cycle_accum: r.u32()?,
        frame_accum: r.u32()?,
        frame_step_n: r.u32()?,
        ch1: load_square(r)?,
        ch2: load_square(r)?,
        ch3: load_wave(r)?,
        ch4: load_noise(r)?,
        produced: 0,
    })
}

fn save_bus(w: &mut Writer, b: &Bus) {
    save_cartridge(w, &b.cart);
    w.raw(&b.wram);
    w.raw(&b.vram);
    w.raw(&b.oam);
    w.raw(&b.io);
    w.raw(&b.hram);
    w.u8(b.ie);
    w.u8(b.joypad.state);
    w.bool(b.is_cgb);
    w.bool(b.double_speed);
    w.u32(b.dev_accum);
    save_ppu(w, &b.ppu);
    save_timer(w, &b.timer);
    save_apu(w, &b.apu);
    w.bytes(&b.serial_buf);
    w.u16(b.dma_source);
    w.u32(b.dma_remaining);
    w.u32(b.serial_remaining);
    w.bool(b.hdma_active);
    w.bool(b.hdma_hblank);
    w.u16(b.hdma_len);
    w.u32(b.hdma_src as u32);
    w.u32(b.hdma_dst as u32);
    w.bool(b.hdma_done_this_hblank);
}

fn load_bus(r: &mut Reader) -> Result<Bus, String> {
    Ok(Bus {
        cart: load_cartridge(r)?,
        wram: r.fixed()?,
        vram: r.fixed()?,
        oam: r.fixed()?,
        io: r.fixed()?,
        hram: r.fixed()?,
        ie: r.u8()?,
        joypad: Joypad { state: r.u8()? },
        is_cgb: r.bool()?,
        double_speed: r.bool()?,
        dev_accum: r.u32()?,
        ppu: load_ppu(r)?,
        timer: load_timer(r)?,
        apu: load_apu(r)?,
        serial_buf: r.bytes()?,
        dma_source: r.u16()?,
        dma_remaining: r.u32()?,
        serial_remaining: r.u32()?,
        hdma_active: r.bool()?,
        hdma_hblank: r.bool()?,
        hdma_len: r.u16()?,
        hdma_src: r.u32()? as usize,
        hdma_dst: r.u32()? as usize,
        hdma_done_this_hblank: r.bool()?,
    })
}

/// Serialize the full CPU + bus state into a self-contained byte vector.
pub(crate) fn save_state(cpu: &Cpu, bus: &Bus) -> Vec<u8> {
    let mut w = Writer::new();
    w.raw(STATE_MAGIC);
    w.u32(STATE_VERSION);
    save_cpu(&mut w, cpu);
    save_bus(&mut w, bus);
    w.buf
}

/// Validate and restore a state produced by [`save_state`], replacing the CPU
/// and bus in place. Returns an error on any malformed, truncated, or
/// oversize input without modifying either system.
pub(crate) fn load_state(cpu: &mut Cpu, bus: &mut Bus, data: &[u8]) -> Result<(), String> {
    if data.len() < 8 {
        return Err("save state too short".to_string());
    }
    if &data[0..4] != STATE_MAGIC {
        return Err("bad save state magic".to_string());
    }
    let version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    if version != STATE_VERSION {
        return Err(format!("unsupported save state version {version}"));
    }
    let mut r = Reader::new(&data[8..]);
    let new_cpu = load_cpu(&mut r)?;
    let new_bus = load_bus(&mut r)?;
    r.finish()?;
    *cpu = new_cpu;
    *bus = new_bus;
    Ok(())
}
