//! GBA I/O register model.
//!
//! Provides the registers a game reads and writes on startup and during play
//! that are safe to model before the PPU/timers/DMA exist: the keypad, the
//! interrupt enable/flag/master registers and the display-control reads. The
//! PPU, timers, DMA, serial and APU write their own registers and are added in
//! later phases; for now those accesses are stored as raw bytes and otherwise
//! ignored.

/// Keypad bits (active low: 0 = pressed).
pub mod key {
    pub const A: u16 = 1 << 0;
    pub const B: u16 = 1 << 1;
    pub const SELECT: u16 = 1 << 2;
    pub const START: u16 = 1 << 3;
    pub const RIGHT: u16 = 1 << 4;
    pub const LEFT: u16 = 1 << 5;
    pub const UP: u16 = 1 << 6;
    pub const DOWN: u16 = 1 << 7;
    pub const R: u16 = 1 << 8;
    pub const L: u16 = 1 << 9;
    pub const MASK: u16 = 0x03FF;
}

/// Interrupt flag bit for the keypad.
pub const IRQ_KEYPAD: u16 = 1 << 14;

/// Interrupt flag bits, one per source.
pub mod irq {
    pub const VBLANK: u16 = 1 << 0;
    pub const HBLANK: u16 = 1 << 1;
    pub const VCOUNT: u16 = 1 << 2;
    pub const TIMER0: u16 = 1 << 3;
    pub const TIMER1: u16 = 1 << 4;
    pub const TIMER2: u16 = 1 << 5;
    pub const TIMER3: u16 = 1 << 6;
    pub const SERIAL: u16 = 1 << 7;
    pub const DMA0: u16 = 1 << 8;
    pub const DMA1: u16 = 1 << 9;
    pub const DMA2: u16 = 1 << 10;
    pub const DMA3: u16 = 1 << 11;
    pub const KEYPAD: u16 = 1 << 14;
    pub const GAME_PAK: u16 = 1 << 15;
}

/// I/O offset of `KEYINPUT` within the 0x04000000 region.
const KEYINPUT: usize = 0x130;
/// I/O offset of `KEYCNT`.
const KEYCNT: usize = 0x132;
/// I/O offsets of IE / IF / IME.
const IE: usize = 0x200;
const IF: usize = 0x202;
const IME: usize = 0x208;
/// I/O offset of `DISPSTAT`; bits 0-2 are read-only status flags.
const DISPSTAT: usize = 0x04;
const DISPSTAT_WRITABLE: u16 = 0xFF38;

/// The I/O registers of a GBA.
pub struct Io {
    /// Raw bytes for the whole 0x04000000..0x040003FF window. Registers with
    /// special behaviour are mirrored into fields below and patched on access.
    pub regs: [u8; 0x400],
    /// Keypad state, bits 0-9 active low (0 = pressed).
    keypad: u16,
    /// Keypad interrupt control.
    keycnt: u16,
    /// Interrupt enable / flag / master.
    ie: u16,
    iflags: u16,
    ime: bool,
    /// Latched VCOUNT (driven by the PPU in later phases).
    vcount: u16,
    /// Set by a write to HALTCNT (0x04000301); the system root reads and
    /// clears it to halt the CPU.
    pub halt_requested: bool,
    /// When true, every write is recorded in `write_log` (diagnostics).
    #[cfg(feature = "trace")]
    pub log_writes: bool,
    /// Pending I/O writes since the last drain as `(offset, width, value)`.
    #[cfg(feature = "trace")]
    pub write_log: Vec<(usize, u8, u16)>,
}

impl Default for Io {
    fn default() -> Io {
        let mut io = Io {
            regs: [0; 0x400],
            keypad: key::MASK,
            keycnt: 0,
            ie: 0,
            iflags: 0,
            ime: false,
            vcount: 0,
            halt_requested: false,
            #[cfg(feature = "trace")]
            log_writes: false,
            #[cfg(feature = "trace")]
            write_log: Vec::new(),
        };
        // Power-on register values (GBATEK / mGBA `GBAIOInit`): forced blank,
        // identity affine matrices, SOUNDBIAS mid-level, RCNT general-purpose.
        for (off, v) in [
            (0x000usize, 0x0080u16),
            (0x020, 0x0100),
            (0x026, 0x0100),
            (0x030, 0x0100),
            (0x036, 0x0100),
            (0x088, 0x0200),
            (0x134, 0x8000),
        ] {
            io.regs[off] = v as u8;
            io.regs[off + 1] = (v >> 8) as u8;
        }
        io
    }
}

impl Io {
    pub fn new() -> Io {
        Io::default()
    }

    /// A key was pressed (bit `k`, a `key::*` constant).
    pub fn press(&mut self, k: u16) {
        self.keypad &= !(k & key::MASK);
        self.update_keypad_irq();
    }

    /// A key was released.
    pub fn release(&mut self, k: u16) {
        self.keypad |= k & key::MASK;
        self.update_keypad_irq();
    }

    fn update_keypad_irq(&mut self) {
        if self.keycnt & (1 << 14) == 0 {
            return;
        }
        let watched = self.keycnt & key::MASK;
        if watched == 0 {
            return;
        }
        let pressed = !self.keypad & key::MASK & watched;
        let and = self.keycnt & (1 << 15) != 0;
        let hit = if and {
            pressed == watched
        } else {
            pressed != 0
        };
        if hit {
            self.iflags |= IRQ_KEYPAD;
        }
    }

    /// Read a 16-bit I/O register (offset within the 0x04000000 region).
    pub fn read16(&self, offset: usize) -> u16 {
        match offset {
            KEYINPUT => self.keypad | 0xFC00,
            KEYCNT => self.keycnt,
            IE => self.ie,
            IF => self.iflags,
            IME => self.ime as u16,
            0x06 => self.vcount,
            _ => u16::from_le_bytes([self.regs[offset], self.regs[offset + 1]]),
        }
    }

    #[cfg(feature = "trace")]
    #[inline]
    fn trace_write(&mut self, offset: usize, width: u8, value: u16) {
        if self.log_writes {
            self.write_log.push((offset, width, value));
        }
    }

    #[cfg(not(feature = "trace"))]
    #[inline(always)]
    fn trace_write(&mut self, _offset: usize, _width: u8, _value: u16) {}

    /// Write a 16-bit I/O register.
    pub fn write16(&mut self, offset: usize, value: u16) {
        self.trace_write(offset, 2, value);
        match offset {
            KEYINPUT | 0x06 => {}
            DISPSTAT => {
                let cur = u16::from_le_bytes([self.regs[offset], self.regs[offset + 1]]);
                let v = (cur & !DISPSTAT_WRITABLE) | (value & DISPSTAT_WRITABLE);
                self.regs[offset] = v as u8;
                self.regs[offset + 1] = (v >> 8) as u8;
            }
            KEYCNT => {
                self.keycnt = value;
                self.regs[offset] = value as u8;
                self.regs[offset + 1] = (value >> 8) as u8;
            }
            IE => {
                self.ie = value;
                self.regs[offset] = value as u8;
                self.regs[offset + 1] = (value >> 8) as u8;
            }
            // IF is cleared by writing 1s.
            IF => {
                self.iflags &= !value;
                self.regs[offset] = 0;
                self.regs[offset + 1] = 0;
            }
            IME => {
                self.ime = value & 1 != 0;
                self.regs[offset] = self.ime as u8;
                self.regs[offset + 1] = 0;
            }
            _ => {
                self.regs[offset] = value as u8;
                self.regs[offset + 1] = (value >> 8) as u8;
            }
        }
    }

    /// A 16-bit write that may split across a special register boundary is not
    /// supported; treat any non-16-bit access as a raw byte store.
    pub fn write8(&mut self, offset: usize, value: u8) {
        self.trace_write(offset, 1, value as u16);
        self.regs[offset] = value;
    }

    /// Set the latched VCOUNT (called by the PPU each scanline).
    pub fn set_vcount(&mut self, v: u16) {
        self.vcount = v;
    }

    /// Latched VCOUNT.
    pub fn vcount(&self) -> u16 {
        self.vcount
    }

    /// Interrupts that will be taken by the CPU: IF & IE, gated by IME.
    pub fn pending_irq(&self) -> u16 {
        if self.ime {
            self.iflags & self.ie
        } else {
            0
        }
    }

    /// Interrupts that end a HALT: IF & IE regardless of IME.
    pub fn wake_irq(&self) -> u16 {
        self.iflags & self.ie
    }

    /// Clear a pending interrupt flag (writing 1 to IF clears it).
    pub fn acknowledge(&mut self, flags: u16) {
        self.iflags &= !flags;
    }

    /// Set an interrupt flag (from timers/DMA/PPU in later phases).
    pub fn raise_irq(&mut self, flags: u16) {
        self.iflags |= flags;
    }

    /// Raw IF value.
    pub fn iflags(&self) -> u16 {
        self.iflags
    }

    /// IE register value.
    pub fn ie(&self) -> u16 {
        self.ie
    }

    /// Whether the master interrupt enable is set.
    pub fn ime(&self) -> bool {
        self.ime
    }
}

/// Plain snapshot of the IO registers for save states.
#[derive(Clone, Copy)]
pub(crate) struct IoSave {
    pub regs: [u8; 0x400],
    pub keypad: u16,
    pub keycnt: u16,
    pub ie: u16,
    pub iflags: u16,
    pub ime: bool,
    pub vcount: u16,
}

impl Io {
    pub(crate) fn snapshot(&self) -> IoSave {
        IoSave {
            regs: self.regs,
            keypad: self.keypad,
            keycnt: self.keycnt,
            ie: self.ie,
            iflags: self.iflags,
            ime: self.ime,
            vcount: self.vcount,
        }
    }

    pub(crate) fn restore(&mut self, s: IoSave) {
        self.regs = s.regs;
        self.keypad = s.keypad;
        self.keycnt = s.keycnt;
        self.ie = s.ie;
        self.iflags = s.iflags;
        self.ime = s.ime;
        self.vcount = s.vcount;
    }
}
