//! Full-machine save states for the GBA.
//!
//! A state is `b"CRGA"` (4-byte magic) + a little-endian u32 version, followed
//! by the serialized CPU, I/O, main memory, save cartridge and frame-timing
//! state. All lengths are little-endian u32-prefixed so a reader can bounds-check
//! every field; loading validates the magic, version and each length.

use crate::cpu::CpuSave;
use crate::gba::Gba;
use crate::io::IoSave;
use crate::save::SaveType;

/// Magic header identifying a CrabBoy GBA save state.
pub const STATE_MAGIC: &[u8; 4] = b"CRGA";
/// Current save-state format version.
pub const STATE_VERSION: u32 = 5;

/// Little-endian serialiser shared by the save-state writers.
pub(crate) struct Writer {
    pub(crate) buf: Vec<u8>,
}

impl Writer {
    pub(crate) fn new() -> Self {
        Writer { buf: Vec::new() }
    }
    pub(crate) fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub(crate) fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.buf.extend_from_slice(b);
    }
    pub(crate) fn bool(&mut self, v: bool) {
        self.u8(v as u8);
    }
}

/// Bounds-checked reader matching [`Writer`].
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }
    pub(crate) fn u8(&mut self) -> Result<u8, String> {
        let v = *self.data.get(self.pos).ok_or("unexpected end of state")?;
        self.pos += 1;
        Ok(v)
    }
    pub(crate) fn u16(&mut self) -> Result<u16, String> {
        let b = self
            .data
            .get(self.pos..self.pos + 2)
            .ok_or("unexpected end of state")?;
        self.pos += 2;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    pub(crate) fn u32(&mut self) -> Result<u32, String> {
        let b = self
            .data
            .get(self.pos..self.pos + 4)
            .ok_or("unexpected end of state")?;
        self.pos += 4;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub(crate) fn u64(&mut self) -> Result<u64, String> {
        let b = self
            .data
            .get(self.pos..self.pos + 8)
            .ok_or("unexpected end of state")?;
        self.pos += 8;
        let mut arr = [0u8; 8];
        arr.copy_from_slice(b);
        Ok(u64::from_le_bytes(arr))
    }
    pub(crate) fn bool(&mut self) -> Result<bool, String> {
        Ok(self.u8()? != 0)
    }
    /// Read exactly `N` raw bytes (no length prefix).
    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let b = self
            .data
            .get(self.pos..self.pos + N)
            .ok_or("unexpected end of state")?;
        self.pos += N;
        let mut out = [0u8; N];
        out.copy_from_slice(b);
        Ok(out)
    }
    pub(crate) fn bytes(&mut self) -> Result<Vec<u8>, String> {
        let len = self.u32()? as usize;
        let b = self
            .data
            .get(self.pos..self.pos + len)
            .ok_or("unexpected end of state")?;
        self.pos += len;
        Ok(b.to_vec())
    }
}

fn save_type_to_u8(t: SaveType) -> u8 {
    match t {
        SaveType::None => 0,
        SaveType::Sram => 1,
        SaveType::Flash => 2,
        SaveType::Eeprom => 3,
    }
}

fn save_type_from_u8(v: u8) -> Result<SaveType, String> {
    Ok(match v {
        0 => SaveType::None,
        1 => SaveType::Sram,
        2 => SaveType::Flash,
        3 => SaveType::Eeprom,
        _ => return Err(format!("unknown save type {v}")),
    })
}

fn save_cpu(w: &mut Writer, gba: &Gba) {
    let s = gba.cpu.save();
    for r in s.regs {
        w.u32(r);
    }
    w.u32(s.pc);
    w.u32(s.cpsr);
    for v in s.base_r8_12 {
        w.u32(v);
    }
    for v in s.fiq_r8_12 {
        w.u32(v);
    }
    for v in s.sp {
        w.u32(v);
    }
    for v in s.lr {
        w.u32(v);
    }
    for v in s.spsr {
        w.u32(v);
    }
    w.u32(s.cycles);
    w.u8(s.halted as u8);
    w.u16(s.bios_wait.unwrap_or(0));
    w.u32(s.bios_wait_pc);
}

fn load_cpu(r: &mut Reader, gba: &mut Gba) -> Result<(), String> {
    let mut s = CpuSave {
        regs: [0; 16],
        pc: 0,
        cpsr: 0,
        base_r8_12: [0; 5],
        fiq_r8_12: [0; 5],
        sp: [0; 6],
        lr: [0; 6],
        spsr: [0; 6],
        cycles: 0,
        halted: false,
        bios_wait: None,
        bios_wait_pc: 0,
    };
    for x in s.regs.iter_mut() {
        *x = r.u32()?;
    }
    s.pc = r.u32()?;
    s.cpsr = r.u32()?;
    for x in s.base_r8_12.iter_mut() {
        *x = r.u32()?;
    }
    for x in s.fiq_r8_12.iter_mut() {
        *x = r.u32()?;
    }
    for x in s.sp.iter_mut() {
        *x = r.u32()?;
    }
    for x in s.lr.iter_mut() {
        *x = r.u32()?;
    }
    for x in s.spsr.iter_mut() {
        *x = r.u32()?;
    }
    s.cycles = r.u32()?;
    s.halted = r.u8()? != 0;
    s.bios_wait = match r.u16()? {
        0 => None,
        mask => Some(mask),
    };
    s.bios_wait_pc = r.u32()?;
    gba.cpu.restore(s);
    // BIOS presence is a static property of the bus, not of the saved CPU.
    gba.cpu.set_has_bios(gba.bus.has_real_bios());
    Ok(())
}

/// Serialize the whole machine.
pub fn save_state(gba: &Gba) -> Vec<u8> {
    let mut w = Writer::new();
    w.buf.extend_from_slice(STATE_MAGIC);
    w.u32(STATE_VERSION);

    save_cpu(&mut w, gba);

    // I/O.
    let io = gba.bus.io.snapshot();
    w.bytes(&io.regs);
    w.u16(io.keypad);
    w.u16(io.keycnt);
    w.u16(io.ie);
    w.u16(io.iflags);
    w.u8(io.ime as u8);
    w.u16(io.vcount);

    // Main memory.
    w.bytes(gba.bus.ewram.as_ref());
    w.bytes(gba.bus.iwram.as_ref());
    w.bytes(gba.bus.vram.as_ref());
    w.bytes(gba.bus.palram.as_ref());
    w.bytes(gba.bus.oam.as_ref());

    // Save cartridge.
    w.u8(save_type_to_u8(gba.bus.save.kind()));
    w.u8(gba.bus.save.flash_bank() as u8);
    w.bytes(gba.bus.save.raw());

    // Frame timing.
    w.u32(gba.line_cycles);
    w.u32(gba.line);
    w.u64(gba.frame_count);

    // Peripherals (format version 5).
    gba.bus.timers.save(&mut w);
    gba.bus.dma.save(&mut w);
    gba.bus.apu.save(&mut w);
    w.buf
        .extend_from_slice(&gba.bus.save.eeprom.save_state_sm());
    w.buf.extend_from_slice(&gba.bus.rtc.save_state());
    w.u8(gba.bus.gpio_data);
    w.u8(gba.bus.gpio_dir);
    w.bool(gba.bus.gpio_readable);
    w.u32(gba.bus.open_bus);
    w.u32(gba.ppu.bg2_ref_x as u32);
    w.u32(gba.ppu.bg2_ref_y as u32);
    w.u32(gba.ppu.bg3_ref_x as u32);
    w.u32(gba.ppu.bg3_ref_y as u32);
    w.u32(gba.last_unknown_swi.unwrap_or(u32::MAX));

    w.buf
}

/// Restore a machine from a serialized state.
pub fn load_state(gba: &mut Gba, data: &[u8]) -> Result<(), String> {
    let mut r = Reader::new(data);
    let magic = data.get(0..4).ok_or("state too short")?;
    if magic != STATE_MAGIC {
        return Err(format!("bad magic: {:?}", magic));
    }
    r.pos = 4;
    let version = r.u32()?;
    if version != STATE_VERSION {
        return Err(format!("unsupported version {version}"));
    }

    load_cpu(&mut r, gba)?;

    let regs = r.bytes()?;
    let keypad = r.u16()?;
    let keycnt = r.u16()?;
    let ie = r.u16()?;
    let iflags = r.u16()?;
    let ime = r.u8()? != 0;
    let vcount = r.u16()?;
    gba.bus.io.restore(IoSave {
        regs: regs.try_into().map_err(|_| "IO regs wrong size")?,
        keypad,
        keycnt,
        ie,
        iflags,
        ime,
        vcount,
    });

    let ewram = r.bytes()?;
    let iwram = r.bytes()?;
    let vram = r.bytes()?;
    let palram = r.bytes()?;
    let oam = r.bytes()?;
    gba.bus.ewram.copy_from_slice(&ewram);
    gba.bus.iwram.copy_from_slice(&iwram);
    gba.bus.vram.copy_from_slice(&vram);
    gba.bus.palram.copy_from_slice(&palram);
    gba.bus.oam.copy_from_slice(&oam);

    let save_type = save_type_from_u8(r.u8()?)?;
    let bank = r.u8()? as usize;
    let save_data = r.bytes()?;
    gba.bus.save.set_kind(save_type);
    gba.bus.save.set_flash_bank(bank);
    gba.bus.save.load(&save_data);

    gba.line_cycles = r.u32()?;
    gba.line = r.u32()?;
    gba.frame_count = r.u64()?;

    gba.bus.timers.load(&mut r)?;
    gba.bus.dma.load(&mut r)?;
    gba.bus.apu.load(&mut r)?;
    gba.bus.save.eeprom.load_state_sm(&r.array()?);
    gba.bus.rtc.load_state(&r.array()?);
    gba.bus.gpio_data = r.u8()? & 0x0F;
    gba.bus.gpio_dir = r.u8()? & 0x0F;
    gba.bus.gpio_readable = r.bool()?;
    gba.bus.open_bus = r.u32()?;
    gba.ppu.bg2_ref_x = r.u32()? as i32;
    gba.ppu.bg2_ref_y = r.u32()? as i32;
    gba.ppu.bg3_ref_x = r.u32()? as i32;
    gba.ppu.bg3_ref_y = r.u32()? as i32;
    gba.last_unknown_swi = match r.u32()? {
        u32::MAX => None,
        n => Some(n),
    };

    if r.pos != data.len() {
        return Err("trailing bytes in state".to_string());
    }
    Ok(())
}
