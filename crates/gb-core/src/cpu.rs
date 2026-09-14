use crate::bus::Bus;

const VECTORS: [u16; 5] = [0x40, 0x48, 0x50, 0x58, 0x60];

#[derive(Clone, Copy)]
pub struct Cpu {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub sp: u16,
    pub pc: u16,
    pub ime: bool,
    pub ei_pending: bool,
    /// Set when `ld b,b` executes; test ROMs use the opcode as a breakpoint.
    pub breakpoint: bool,
    /// T-cycles the bus was advanced during the current instruction.
    pub(crate) ticked: u32,
    pub halted: bool,
    pub stopped: bool,
    pub(crate) halt_bug: bool,
    pub timer_interrupts: u64,
}

impl Cpu {
    pub fn new() -> Cpu {
        Cpu {
            a: 0x01,
            f: 0xB0,
            b: 0x00,
            c: 0x13,
            d: 0x00,
            e: 0xD8,
            h: 0x01,
            l: 0x4D,
            sp: 0xFFFE,
            pc: 0x0100,
            ime: false,
            ei_pending: false,
            halted: false,
            stopped: false,
            halt_bug: false,
            breakpoint: false,
            ticked: 0,
            timer_interrupts: 0,
        }
    }

    #[inline]
    fn z(&self) -> bool {
        self.f & 0x80 != 0
    }
    #[inline]
    fn n(&self) -> bool {
        self.f & 0x40 != 0
    }
    #[inline]
    fn h(&self) -> bool {
        self.f & 0x20 != 0
    }
    #[inline]
    fn c(&self) -> bool {
        self.f & 0x10 != 0
    }

    #[inline]
    fn set_z(&mut self, v: bool) {
        self.f = (self.f & !0x80) | if v { 0x80 } else { 0 };
    }
    #[inline]
    fn set_n(&mut self, v: bool) {
        self.f = (self.f & !0x40) | if v { 0x40 } else { 0 };
    }
    #[inline]
    fn set_h(&mut self, v: bool) {
        self.f = (self.f & !0x20) | if v { 0x20 } else { 0 };
    }
    #[inline]
    fn set_c(&mut self, v: bool) {
        self.f = (self.f & !0x10) | if v { 0x10 } else { 0 };
    }

    #[inline]
    fn bc(&self) -> u16 {
        (self.b as u16) << 8 | self.c as u16
    }
    #[inline]
    fn set_bc(&mut self, v: u16) {
        self.b = (v >> 8) as u8;
        self.c = v as u8;
    }
    #[inline]
    fn de(&self) -> u16 {
        (self.d as u16) << 8 | self.e as u16
    }
    #[inline]
    fn set_de(&mut self, v: u16) {
        self.d = (v >> 8) as u8;
        self.e = v as u8;
    }
    #[inline]
    fn hl(&self) -> u16 {
        (self.h as u16) << 8 | self.l as u16
    }
    #[inline]
    fn set_hl(&mut self, v: u16) {
        self.h = (v >> 8) as u8;
        self.l = v as u8;
    }
    #[inline]
    fn af(&self) -> u16 {
        (self.a as u16) << 8 | self.f as u16
    }
    #[inline]
    fn set_af(&mut self, v: u16) {
        self.a = (v >> 8) as u8;
        self.f = (v & 0xF0) as u8;
    }

    /// Advance the machine by one M-cycle (4 T-cycles) without a bus access.
    #[inline]
    fn internal(&mut self, bus: &mut Bus) {
        bus.step(4);
        self.ticked += 4;
    }

    /// One M-cycle that reads `addr` at its end.
    #[inline]
    fn read8(&mut self, bus: &mut Bus, addr: u16) -> u8 {
        self.internal(bus);
        bus.read(addr)
    }

    /// One M-cycle that writes `addr` at its end.
    #[inline]
    fn write8(&mut self, bus: &mut Bus, addr: u16, v: u8) {
        self.internal(bus);
        bus.write(addr, v);
    }

    #[inline]
    fn fetch8(&mut self, bus: &mut Bus) -> u8 {
        let v = self.read8(bus, self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }

    #[inline]
    fn fetch16(&mut self, bus: &mut Bus) -> u16 {
        let lo = self.fetch8(bus);
        let hi = self.fetch8(bus);
        (hi as u16) << 8 | lo as u16
    }

    /// PUSH: the internal cycle precedes the two writes.
    #[inline]
    fn push16(&mut self, bus: &mut Bus, v: u16) {
        self.internal(bus);
        self.sp = self.sp.wrapping_sub(1);
        self.write8(bus, self.sp, (v >> 8) as u8);
        self.sp = self.sp.wrapping_sub(1);
        self.write8(bus, self.sp, v as u8);
    }

    #[inline]
    fn pop16(&mut self, bus: &mut Bus) -> u16 {
        let lo = self.read8(bus, self.sp);
        self.sp = self.sp.wrapping_add(1);
        let hi = self.read8(bus, self.sp);
        self.sp = self.sp.wrapping_add(1);
        (hi as u16) << 8 | lo as u16
    }

    /// Execute one instruction, returning the number of machine cycles used.
    pub fn execute(&mut self, bus: &mut Bus) -> u32 {
        // If a bugged HALT ran on the previous step, the byte after it must be
        // fetched again: rewind PC by one after this instruction completes.
        let was_bugged = self.halt_bug;
        self.halt_bug = false;
        let pc = self.pc;
        self.ticked = 0;
        let op = self.fetch8(bus);
        if op == 0x40 {
            self.breakpoint = true;
        }
        // Only query the environment once; tracing is a debug-only aid.
        static TRACE_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *TRACE_ENABLED.get_or_init(|| std::env::var("GB_TRACE").is_ok()) {
            use std::sync::atomic::{AtomicU32, Ordering};
            static TRACE_N: AtomicU32 = AtomicU32::new(0);
            let n = TRACE_N.fetch_add(1, Ordering::Relaxed);
            let start: u32 = std::env::var("GB_TRACE_START")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let count: u32 = std::env::var("GB_TRACE_COUNT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(u32::MAX);
            if n >= start && n < start + count {
                eprintln!(
                    "T{} {:04X} {:02X} A={:02X} F={:02X} BC={:04X} DE={:04X} HL={:04X} SP={:04X}",
                    n,
                    pc,
                    op,
                    self.a,
                    self.f,
                    self.bc(),
                    self.de(),
                    self.hl(),
                    self.sp
                );
            }
        }
        let expected = self.exec_opcode(op, bus);
        debug_assert_eq!(
            self.ticked, expected,
            "opcode {op:02X} at {pc:04X} ticked {} cycles, expected {expected}",
            self.ticked
        );
        // HALT bug: the instruction following a bugged HALT runs twice, so PC is
        // rewound after it (the flag was captured at the top of this call).
        if was_bugged {
            self.pc = self.pc.wrapping_sub(1);
        }
        self.ticked
    }

    #[allow(unused_assignments)]
    fn exec_opcode(&mut self, op: u8, bus: &mut Bus) -> u32 {
        match op {
            0x00 => 4,
            0x01 => {
                let v = self.fetch16(bus);
                self.set_bc(v);
                12
            }
            0x02 => {
                let v = self.a;
                let a = self.bc();
                self.write8(bus, a, v);
                8
            }
            0x03 => {
                self.set_bc(self.bc().wrapping_add(1));
                self.internal(bus);
                8
            }
            0x04 => self.inc_b(),
            0x05 => self.dec_b(),
            0x06 => {
                self.b = self.fetch8(bus);
                8
            }
            0x07 => {
                self.rot_rlc_a();
                4
            }
            0x08 => {
                let addr = self.fetch16(bus);
                self.write8(bus, addr, self.sp as u8);
                self.write8(bus, addr.wrapping_add(1), (self.sp >> 8) as u8);
                20
            }
            0x09 => {
                self.internal(bus);
                self.add_hl_bc()
            }
            0x0A => {
                self.a = self.read8(bus, self.bc());
                8
            }
            0x0B => {
                self.set_bc(self.bc().wrapping_sub(1));
                self.internal(bus);
                8
            }
            0x0C => self.inc_c(),
            0x0D => self.dec_c(),
            0x0E => {
                self.c = self.fetch8(bus);
                8
            }
            0x0F => {
                self.rot_rrc_a();
                4
            }
            0x10 => {
                // STOP: consume the padding byte, then stop until a button press.
                self.fetch8(bus);
                self.stopped = true;
                8
            }
            0x11 => {
                let v = self.fetch16(bus);
                self.set_de(v);
                12
            }
            0x12 => {
                let v = self.a;
                let a = self.de();
                self.write8(bus, a, v);
                8
            }
            0x13 => {
                self.set_de(self.de().wrapping_add(1));
                self.internal(bus);
                8
            }
            0x14 => self.inc_d(),
            0x15 => self.dec_d(),
            0x16 => {
                self.d = self.fetch8(bus);
                8
            }
            0x17 => {
                self.rot_rl_a();
                4
            }
            0x18 => {
                let n = self.fetch8(bus) as i8;
                self.internal(bus);
                self.pc = self.pc.wrapping_add_signed(n as i16);
                12
            }
            0x19 => {
                self.internal(bus);
                self.add_hl_de()
            }
            0x1A => {
                self.a = self.read8(bus, self.de());
                8
            }
            0x1B => {
                self.set_de(self.de().wrapping_sub(1));
                self.internal(bus);
                8
            }
            0x1C => self.inc_e(),
            0x1D => self.dec_e(),
            0x1E => {
                self.e = self.fetch8(bus);
                8
            }
            0x1F => {
                self.rot_rr_a();
                4
            }
            0x20 => {
                let n = self.fetch8(bus) as i8;
                if !self.z() {
                    self.pc = self.pc.wrapping_add_signed(n as i16);
                    self.internal(bus);
                    12
                } else {
                    8
                }
            }
            0x21 => {
                let v = self.fetch16(bus);
                self.set_hl(v);
                12
            }
            0x22 => {
                let v = self.a;
                let a = self.hl();
                self.write8(bus, a, v);
                self.set_hl(a.wrapping_add(1));
                8
            }
            0x23 => {
                self.set_hl(self.hl().wrapping_add(1));
                self.internal(bus);
                8
            }
            0x24 => self.inc_h(),
            0x25 => self.dec_h(),
            0x26 => {
                self.h = self.fetch8(bus);
                8
            }
            0x27 => self.daa(),
            0x28 => {
                let n = self.fetch8(bus) as i8;
                if self.z() {
                    self.pc = self.pc.wrapping_add_signed(n as i16);
                    self.internal(bus);
                    12
                } else {
                    8
                }
            }
            0x29 => {
                self.internal(bus);
                self.add_hl_hl()
            }
            0x2A => {
                let a = self.hl();
                self.a = self.read8(bus, a);
                self.set_hl(a.wrapping_add(1));
                8
            }
            0x2B => {
                self.set_hl(self.hl().wrapping_sub(1));
                self.internal(bus);
                8
            }
            0x2C => self.inc_l(),
            0x2D => self.dec_l(),
            0x2E => {
                self.l = self.fetch8(bus);
                8
            }
            0x2F => {
                self.a = !self.a;
                self.set_n(true);
                self.set_h(true);
                4
            }
            0x30 => {
                let n = self.fetch8(bus) as i8;
                if !self.c() {
                    self.pc = self.pc.wrapping_add_signed(n as i16);
                    self.internal(bus);
                    12
                } else {
                    8
                }
            }
            0x31 => {
                self.sp = self.fetch16(bus);
                12
            }
            0x32 => {
                let v = self.a;
                let a = self.hl();
                self.write8(bus, a, v);
                self.set_hl(a.wrapping_sub(1));
                8
            }
            0x33 => {
                self.sp = self.sp.wrapping_add(1);
                self.internal(bus);
                8
            }
            0x34 => self.inc_hl(bus),
            0x35 => self.dec_hl(bus),
            0x36 => {
                let v = self.fetch8(bus);
                let a = self.hl();
                self.write8(bus, a, v);
                12
            }
            0x37 => {
                self.set_c(true);
                self.set_n(false);
                self.set_h(false);
                4
            }
            0x38 => {
                let n = self.fetch8(bus) as i8;
                if self.c() {
                    self.pc = self.pc.wrapping_add_signed(n as i16);
                    self.internal(bus);
                    12
                } else {
                    8
                }
            }
            0x39 => {
                self.internal(bus);
                self.add_hl_sp()
            }
            0x3A => {
                let a = self.hl();
                self.a = self.read8(bus, a);
                self.set_hl(a.wrapping_sub(1));
                8
            }
            0x3B => {
                self.sp = self.sp.wrapping_sub(1);
                self.internal(bus);
                8
            }
            0x3C => self.inc_a(),
            0x3D => self.dec_a(),
            0x3E => {
                self.a = self.fetch8(bus);
                8
            }
            0x3F => {
                let c = self.c();
                self.set_c(!c);
                self.set_n(false);
                self.set_h(false);
                4
            }
            0x40 => 4,
            0x41 => {
                self.b = self.c;
                4
            }
            0x42 => {
                self.b = self.d;
                4
            }
            0x43 => {
                self.b = self.e;
                4
            }
            0x44 => {
                self.b = self.h;
                4
            }
            0x45 => {
                self.b = self.l;
                4
            }
            0x46 => {
                self.b = self.read8(bus, self.hl());
                8
            }
            0x47 => {
                self.b = self.a;
                4
            }
            0x48 => {
                self.c = self.b;
                4
            }
            0x49 => 4,
            0x4A => {
                self.c = self.d;
                4
            }
            0x4B => {
                self.c = self.e;
                4
            }
            0x4C => {
                self.c = self.h;
                4
            }
            0x4D => {
                self.c = self.l;
                4
            }
            0x4E => {
                self.c = self.read8(bus, self.hl());
                8
            }
            0x4F => {
                self.c = self.a;
                4
            }
            0x50 => {
                self.d = self.b;
                4
            }
            0x51 => {
                self.d = self.c;
                4
            }
            0x52 => 4,
            0x53 => {
                self.d = self.e;
                4
            }
            0x54 => {
                self.d = self.h;
                4
            }
            0x55 => {
                self.d = self.l;
                4
            }
            0x56 => {
                self.d = self.read8(bus, self.hl());
                8
            }
            0x57 => {
                self.d = self.a;
                4
            }
            0x58 => {
                self.e = self.b;
                4
            }
            0x59 => {
                self.e = self.c;
                4
            }
            0x5A => {
                self.e = self.d;
                4
            }
            0x5B => 4,
            0x5C => {
                self.e = self.h;
                4
            }
            0x5D => {
                self.e = self.l;
                4
            }
            0x5E => {
                self.e = self.read8(bus, self.hl());
                8
            }
            0x5F => {
                self.e = self.a;
                4
            }
            0x60 => {
                self.h = self.b;
                4
            }
            0x61 => {
                self.h = self.c;
                4
            }
            0x62 => {
                self.h = self.d;
                4
            }
            0x63 => {
                self.h = self.e;
                4
            }
            0x64 => 4,
            0x65 => {
                self.h = self.l;
                4
            }
            0x66 => {
                self.h = self.read8(bus, self.hl());
                8
            }
            0x67 => {
                self.h = self.a;
                4
            }
            0x68 => {
                self.l = self.b;
                4
            }
            0x69 => {
                self.l = self.c;
                4
            }
            0x6A => {
                self.l = self.d;
                4
            }
            0x6B => {
                self.l = self.e;
                4
            }
            0x6C => {
                self.l = self.h;
                4
            }
            0x6D => 4,
            0x6E => {
                self.l = self.read8(bus, self.hl());
                8
            }
            0x6F => {
                self.l = self.a;
                4
            }
            0x70 => {
                let v = self.b;
                self.write8(bus, self.hl(), v);
                8
            }
            0x71 => {
                let v = self.c;
                self.write8(bus, self.hl(), v);
                8
            }
            0x72 => {
                let v = self.d;
                self.write8(bus, self.hl(), v);
                8
            }
            0x73 => {
                let v = self.e;
                self.write8(bus, self.hl(), v);
                8
            }
            0x74 => {
                let v = self.h;
                self.write8(bus, self.hl(), v);
                8
            }
            0x75 => {
                let v = self.l;
                self.write8(bus, self.hl(), v);
                8
            }
            0x76 => {
                let pending = bus.io[0x0F] & bus.ie & 0x1F;
                if !self.ime && !self.ei_pending && pending != 0 {
                    // HALT bug: with IME disabled and an interrupt pending, the
                    // CPU does not halt; the byte after HALT is executed twice.
                    // An EI immediately before HALT suppresses the bug: HALT then
                    // halts and enables IME, so the pending interrupt is serviced.
                    self.halt_bug = true;
                    4
                } else {
                    self.halted = true;
                    4
                }
            }
            0x77 => {
                let v = self.a;
                self.write8(bus, self.hl(), v);
                8
            }
            0x78 => {
                self.a = self.b;
                4
            }
            0x79 => {
                self.a = self.c;
                4
            }
            0x7A => {
                self.a = self.d;
                4
            }
            0x7B => {
                self.a = self.e;
                4
            }
            0x7C => {
                self.a = self.h;
                4
            }
            0x7D => {
                self.a = self.l;
                4
            }
            0x7E => {
                self.a = self.read8(bus, self.hl());
                8
            }
            0x7F => 4,
            0x80 => self.add_a(self.b),
            0x81 => self.add_a(self.c),
            0x82 => self.add_a(self.d),
            0x83 => self.add_a(self.e),
            0x84 => self.add_a(self.h),
            0x85 => self.add_a(self.l),
            0x86 => {
                let v = self.read8(bus, self.hl());
                self.add_a(v);
                8
            }
            0x87 => self.add_a(self.a),
            0x88 => self.adc_a(self.b),
            0x89 => self.adc_a(self.c),
            0x8A => self.adc_a(self.d),
            0x8B => self.adc_a(self.e),
            0x8C => self.adc_a(self.h),
            0x8D => self.adc_a(self.l),
            0x8E => {
                let v = self.read8(bus, self.hl());
                self.adc_a(v);
                8
            }
            0x8F => self.adc_a(self.a),
            0x90 => self.sub_a(self.b),
            0x91 => self.sub_a(self.c),
            0x92 => self.sub_a(self.d),
            0x93 => self.sub_a(self.e),
            0x94 => self.sub_a(self.h),
            0x95 => self.sub_a(self.l),
            0x96 => {
                let v = self.read8(bus, self.hl());
                self.sub_a(v);
                8
            }
            0x97 => self.sub_a(self.a),
            0x98 => self.sbc_a(self.b),
            0x99 => self.sbc_a(self.c),
            0x9A => self.sbc_a(self.d),
            0x9B => self.sbc_a(self.e),
            0x9C => self.sbc_a(self.h),
            0x9D => self.sbc_a(self.l),
            0x9E => {
                let v = self.read8(bus, self.hl());
                self.sbc_a(v);
                8
            }
            0x9F => self.sbc_a(self.a),
            0xA0 => self.and_a(self.b),
            0xA1 => self.and_a(self.c),
            0xA2 => self.and_a(self.d),
            0xA3 => self.and_a(self.e),
            0xA4 => self.and_a(self.h),
            0xA5 => self.and_a(self.l),
            0xA6 => {
                let v = self.read8(bus, self.hl());
                self.and_a(v);
                8
            }
            0xA7 => self.and_a(self.a),
            0xA8 => self.xor_a(self.b),
            0xA9 => self.xor_a(self.c),
            0xAA => self.xor_a(self.d),
            0xAB => self.xor_a(self.e),
            0xAC => self.xor_a(self.h),
            0xAD => self.xor_a(self.l),
            0xAE => {
                let v = self.read8(bus, self.hl());
                self.xor_a(v);
                8
            }
            0xAF => self.xor_a(self.a),
            0xB0 => self.or_a(self.b),
            0xB1 => self.or_a(self.c),
            0xB2 => self.or_a(self.d),
            0xB3 => self.or_a(self.e),
            0xB4 => self.or_a(self.h),
            0xB5 => self.or_a(self.l),
            0xB6 => {
                let v = self.read8(bus, self.hl());
                self.or_a(v);
                8
            }
            0xB7 => self.or_a(self.a),
            0xB8 => self.cp_a(self.b),
            0xB9 => self.cp_a(self.c),
            0xBA => self.cp_a(self.d),
            0xBB => self.cp_a(self.e),
            0xBC => self.cp_a(self.h),
            0xBD => self.cp_a(self.l),
            0xBE => {
                let v = self.read8(bus, self.hl());
                self.cp_a(v);
                8
            }
            0xBF => self.cp_a(self.a),
            0xC0 => {
                self.internal(bus);
                if !self.z() {
                    self.pc = self.pop16(bus);
                    self.internal(bus);
                    20
                } else {
                    8
                }
            }
            0xC1 => {
                let v = self.pop16(bus);
                self.set_bc(v);
                12
            }
            0xC2 => {
                let n = self.fetch16(bus);
                if !self.z() {
                    self.pc = n;
                    self.internal(bus);
                    16
                } else {
                    12
                }
            }
            0xC3 => {
                self.pc = self.fetch16(bus);
                self.internal(bus);
                16
            }
            0xC4 => {
                let n = self.fetch16(bus);
                if !self.z() {
                    self.push16(bus, self.pc);
                    self.pc = n;
                    24
                } else {
                    12
                }
            }
            0xC5 => {
                let v = self.bc();
                self.push16(bus, v);
                16
            }
            0xC6 => {
                let v = self.fetch8(bus);
                self.add_a(v);
                8
            }
            0xC7 => {
                self.rst(bus, 0x00);
                16
            }
            0xC8 => {
                self.internal(bus);
                if self.z() {
                    self.pc = self.pop16(bus);
                    self.internal(bus);
                    20
                } else {
                    8
                }
            }
            0xC9 => {
                self.pc = self.pop16(bus);
                self.internal(bus);
                16
            }
            0xCA => {
                let n = self.fetch16(bus);
                if self.z() {
                    self.pc = n;
                    self.internal(bus);
                    16
                } else {
                    12
                }
            }
            0xCC => {
                let n = self.fetch16(bus);
                if self.z() {
                    self.push16(bus, self.pc);
                    self.pc = n;
                    24
                } else {
                    12
                }
            }
            0xCD => {
                let n = self.fetch16(bus);
                self.push16(bus, self.pc);
                self.pc = n;
                24
            }
            0xCE => {
                let v = self.fetch8(bus);
                self.adc_a(v);
                8
            }
            0xCF => {
                self.rst(bus, 0x08);
                16
            }
            0xD0 => {
                self.internal(bus);
                if !self.c() {
                    self.pc = self.pop16(bus);
                    self.internal(bus);
                    20
                } else {
                    8
                }
            }
            0xD1 => {
                let v = self.pop16(bus);
                self.set_de(v);
                12
            }
            0xD2 => {
                let n = self.fetch16(bus);
                if !self.c() {
                    self.pc = n;
                    self.internal(bus);
                    16
                } else {
                    12
                }
            }
            0xD4 => {
                let n = self.fetch16(bus);
                if !self.c() {
                    self.push16(bus, self.pc);
                    self.pc = n;
                    24
                } else {
                    12
                }
            }
            0xD5 => {
                let v = self.de();
                self.push16(bus, v);
                16
            }
            0xD6 => {
                let v = self.fetch8(bus);
                self.sub_a(v);
                8
            }
            0xD7 => {
                self.rst(bus, 0x10);
                16
            }
            0xD8 => {
                self.internal(bus);
                if self.c() {
                    self.pc = self.pop16(bus);
                    self.internal(bus);
                    20
                } else {
                    8
                }
            }
            0xD9 => {
                self.pc = self.pop16(bus);
                self.internal(bus);
                self.ime = true;
                16
            }
            0xDA => {
                let n = self.fetch16(bus);
                if self.c() {
                    self.pc = n;
                    self.internal(bus);
                    16
                } else {
                    12
                }
            }
            0xDC => {
                let n = self.fetch16(bus);
                if self.c() {
                    self.push16(bus, self.pc);
                    self.pc = n;
                    24
                } else {
                    12
                }
            }
            0xDE => {
                let v = self.fetch8(bus);
                self.sbc_a(v);
                8
            }
            0xDF => {
                self.rst(bus, 0x18);
                16
            }
            0xE0 => {
                let a = 0xFF00 | self.fetch8(bus) as u16;
                self.write8(bus, a, self.a);
                12
            }
            0xE1 => {
                let v = self.pop16(bus);
                self.set_hl(v);
                12
            }
            0xE2 => {
                let a = 0xFF00 | self.c as u16;
                self.write8(bus, a, self.a);
                8
            }
            0xE5 => {
                let v = self.hl();
                self.push16(bus, v);
                16
            }
            0xE6 => {
                let v = self.fetch8(bus);
                self.and_a(v);
                8
            }
            0xE7 => {
                self.rst(bus, 0x20);
                16
            }
            0xE8 => {
                let n = self.fetch8(bus) as i8;
                self.internal(bus);
                self.internal(bus);
                let sp = self.sp;
                let r = (sp as i32).wrapping_add(n as i32) as u16;
                let a = (sp & 0xFF) as u8;
                let b = n as u8;
                self.sp = r;
                self.set_z(false);
                self.set_n(false);
                self.set_h(((a & 0xF) + (b & 0xF)) > 0xF);
                self.set_c(((a as u16) + (b as u16)) > 0xFF);
                16
            }
            0xE9 => {
                self.pc = self.hl();
                4
            }
            0xEA => {
                let a = self.fetch16(bus);
                self.write8(bus, a, self.a);
                16
            }
            0xEE => {
                let v = self.fetch8(bus);
                self.xor_a(v);
                8
            }
            0xEF => {
                self.rst(bus, 0x28);
                16
            }
            0xF0 => {
                let a = 0xFF00 | self.fetch8(bus) as u16;
                self.a = self.read8(bus, a);
                12
            }
            0xF1 => {
                let v = self.pop16(bus);
                self.set_af(v);
                12
            }
            0xF2 => {
                let a = 0xFF00 | self.c as u16;
                self.a = self.read8(bus, a);
                8
            }
            0xF3 => {
                self.ime = false;
                self.ei_pending = false;
                4
            }
            0xF5 => {
                let v = self.af();
                self.push16(bus, v);
                16
            }
            0xF6 => {
                let v = self.fetch8(bus);
                self.or_a(v);
                8
            }
            0xF7 => {
                self.rst(bus, 0x30);
                16
            }
            0xF8 => {
                let n = self.fetch8(bus) as i8;
                self.internal(bus);
                let sp = self.sp;
                let r = (sp as i32).wrapping_add(n as i32) as u16;
                let a = (sp & 0xFF) as u8;
                let b = n as u8;
                self.set_hl(r);
                self.set_z(false);
                self.set_n(false);
                self.set_h(((a & 0xF) + (b & 0xF)) > 0xF);
                self.set_c(((a as u16) + (b as u16)) > 0xFF);
                12
            }
            0xF9 => {
                self.sp = self.hl();
                self.internal(bus);
                8
            }
            0xFA => {
                let a = self.fetch16(bus);
                self.a = self.read8(bus, a);
                16
            }
            0xFB => {
                self.ei_pending = true;
                4
            }
            0xFE => {
                let v = self.fetch8(bus);
                self.cp_a(v);
                8
            }
            0xFF => {
                self.rst(bus, 0x38);
                16
            }
            0xCB => {
                let sub = self.fetch8(bus);
                self.exec_cb(sub, bus)
            }
            // Undefined opcodes act as 2-byte NOPs on real DMG hardware: they
            // consume the following byte and do nothing else.
            _ => {
                self.fetch8(bus);
                8
            }
        }
    }

    fn rst(&mut self, bus: &mut Bus, addr: u16) {
        self.push16(bus, self.pc);
        self.pc = addr;
    }

    #[inline]
    fn inc_b(&mut self) -> u32 {
        let v = self.b.wrapping_add(1);
        self.b = v;
        self.set_z(v == 0);
        self.set_n(false);
        self.set_h(self.b.wrapping_sub(1) & 0x0F == 0x0F);
        4
    }
    #[inline]
    fn dec_b(&mut self) -> u32 {
        let v = self.b.wrapping_sub(1);
        self.b = v;
        self.set_z(v == 0);
        self.set_n(true);
        self.set_h(self.b.wrapping_add(1) & 0x0F == 0x00);
        4
    }
    #[inline]
    fn inc_c(&mut self) -> u32 {
        let v = self.c.wrapping_add(1);
        self.c = v;
        self.set_z(v == 0);
        self.set_n(false);
        self.set_h(self.c.wrapping_sub(1) & 0x0F == 0x0F);
        4
    }
    #[inline]
    fn dec_c(&mut self) -> u32 {
        let v = self.c.wrapping_sub(1);
        self.c = v;
        self.set_z(v == 0);
        self.set_n(true);
        self.set_h(self.c.wrapping_add(1) & 0x0F == 0x00);
        4
    }
    #[inline]
    fn inc_d(&mut self) -> u32 {
        let v = self.d.wrapping_add(1);
        self.d = v;
        self.set_z(v == 0);
        self.set_n(false);
        self.set_h(self.d.wrapping_sub(1) & 0x0F == 0x0F);
        4
    }
    #[inline]
    fn dec_d(&mut self) -> u32 {
        let v = self.d.wrapping_sub(1);
        self.d = v;
        self.set_z(v == 0);
        self.set_n(true);
        self.set_h(self.d.wrapping_add(1) & 0x0F == 0x00);
        4
    }
    #[inline]
    fn inc_e(&mut self) -> u32 {
        let v = self.e.wrapping_add(1);
        self.e = v;
        self.set_z(v == 0);
        self.set_n(false);
        self.set_h(self.e.wrapping_sub(1) & 0x0F == 0x0F);
        4
    }
    #[inline]
    fn dec_e(&mut self) -> u32 {
        let v = self.e.wrapping_sub(1);
        self.e = v;
        self.set_z(v == 0);
        self.set_n(true);
        self.set_h(self.e.wrapping_add(1) & 0x0F == 0x00);
        4
    }
    #[inline]
    fn inc_h(&mut self) -> u32 {
        let v = self.h.wrapping_add(1);
        self.h = v;
        self.set_z(v == 0);
        self.set_n(false);
        self.set_h(self.h.wrapping_sub(1) & 0x0F == 0x0F);
        4
    }
    #[inline]
    fn dec_h(&mut self) -> u32 {
        let v = self.h.wrapping_sub(1);
        self.h = v;
        self.set_z(v == 0);
        self.set_n(true);
        self.set_h(self.h.wrapping_add(1) & 0x0F == 0x00);
        4
    }
    #[inline]
    fn inc_l(&mut self) -> u32 {
        let v = self.l.wrapping_add(1);
        self.l = v;
        self.set_z(v == 0);
        self.set_n(false);
        self.set_h(self.l.wrapping_sub(1) & 0x0F == 0x0F);
        4
    }
    #[inline]
    fn dec_l(&mut self) -> u32 {
        let v = self.l.wrapping_sub(1);
        self.l = v;
        self.set_z(v == 0);
        self.set_n(true);
        self.set_h(self.l.wrapping_add(1) & 0x0F == 0x00);
        4
    }
    #[inline]
    fn inc_a(&mut self) -> u32 {
        let v = self.a.wrapping_add(1);
        self.a = v;
        self.set_z(v == 0);
        self.set_n(false);
        self.set_h(self.a.wrapping_sub(1) & 0x0F == 0x0F);
        4
    }
    #[inline]
    fn dec_a(&mut self) -> u32 {
        let v = self.a.wrapping_sub(1);
        self.a = v;
        self.set_z(v == 0);
        self.set_n(true);
        self.set_h(self.a.wrapping_add(1) & 0x0F == 0x00);
        4
    }
    #[inline]
    fn inc_hl(&mut self, bus: &mut Bus) -> u32 {
        let a = self.hl();
        let v = self.read8(bus, a).wrapping_add(1);
        self.write8(bus, a, v);
        self.set_z(v == 0);
        self.set_n(false);
        self.set_h(v & 0x0F == 0x00);
        12
    }
    #[inline]
    fn dec_hl(&mut self, bus: &mut Bus) -> u32 {
        let a = self.hl();
        let v = self.read8(bus, a).wrapping_sub(1);
        self.write8(bus, a, v);
        self.set_z(v == 0);
        self.set_n(true);
        self.set_h(v & 0x0F == 0x0F);
        12
    }

    fn add_a(&mut self, v: u8) -> u32 {
        let a = self.a;
        let r = a.wrapping_add(v);
        self.set_z(r == 0);
        self.set_n(false);
        self.set_h((a & 0x0F) + (v & 0x0F) > 0x0F);
        self.set_c((a as u16) + (v as u16) > 0xFF);
        self.a = r;
        4
    }
    fn adc_a(&mut self, v: u8) -> u32 {
        let a = self.a;
        let c = self.c() as u16;
        let r = (a as u16).wrapping_add(v as u16).wrapping_add(c);
        self.set_z(r as u8 == 0);
        self.set_n(false);
        self.set_h((a & 0x0F) + (v & 0x0F) + c as u8 > 0x0F);
        self.set_c(r > 0xFF);
        self.a = r as u8;
        4
    }
    fn sub_a(&mut self, v: u8) -> u32 {
        let a = self.a;
        let r = a.wrapping_sub(v);
        self.set_z(r == 0);
        self.set_n(true);
        self.set_h((a & 0x0F) < (v & 0x0F));
        self.set_c(a < v);
        self.a = r;
        4
    }
    fn sbc_a(&mut self, v: u8) -> u32 {
        let a = self.a;
        let c = self.c() as u16;
        let r = (a as i16) - (v as i16) - c as i16;
        self.set_z(r as u8 == 0);
        self.set_n(true);
        self.set_h((a & 0x0F) < (v & 0x0F) + c as u8);
        self.set_c((a as u16) < (v as u16) + c);
        self.a = r as u8;
        4
    }
    fn and_a(&mut self, v: u8) -> u32 {
        self.a &= v;
        self.set_z(self.a == 0);
        self.set_n(false);
        self.set_h(true);
        self.set_c(false);
        4
    }
    fn xor_a(&mut self, v: u8) -> u32 {
        self.a ^= v;
        self.set_z(self.a == 0);
        self.set_n(false);
        self.set_h(false);
        self.set_c(false);
        4
    }
    fn or_a(&mut self, v: u8) -> u32 {
        self.a |= v;
        self.set_z(self.a == 0);
        self.set_n(false);
        self.set_h(false);
        self.set_c(false);
        4
    }
    fn cp_a(&mut self, v: u8) -> u32 {
        let a = self.a;
        let r = a.wrapping_sub(v);
        self.set_z(r == 0);
        self.set_n(true);
        self.set_h((a & 0x0F) < (v & 0x0F));
        self.set_c(a < v);
        4
    }

    fn add_hl_bc(&mut self) -> u32 {
        self.add_hl(self.bc())
    }
    fn add_hl_de(&mut self) -> u32 {
        self.add_hl(self.de())
    }
    fn add_hl_hl(&mut self) -> u32 {
        self.add_hl(self.hl())
    }
    fn add_hl_sp(&mut self) -> u32 {
        self.add_hl(self.sp)
    }
    fn add_hl(&mut self, v: u16) -> u32 {
        let hl = self.hl();
        let r = hl.wrapping_add(v);
        self.set_n(false);
        self.set_h((hl & 0x0FFF) + (v & 0x0FFF) > 0x0FFF);
        self.set_c((hl as u32) + (v as u32) > 0xFFFF);
        self.set_hl(r);
        8
    }

    fn daa(&mut self) -> u32 {
        let mut a = self.a as u16;
        let mut adjust = 0u16;
        if self.h() || (!self.n() && (a & 0x0F) > 0x09) {
            adjust |= 0x06;
        }
        if self.c() || (!self.n() && a > 0x99) {
            adjust |= 0x60;
            self.set_c(true);
        } else {
            self.set_c(false);
        }
        if self.n() {
            a = a.wrapping_sub(adjust);
        } else {
            a = a.wrapping_add(adjust);
        }
        self.a = (a & 0xFF) as u8;
        self.set_z(self.a == 0);
        self.set_h(false);
        4
    }

    fn rot_rlc_a(&mut self) {
        let c = self.a >> 7;
        self.a = (self.a << 1) | c;
        self.set_z(false);
        self.set_n(false);
        self.set_h(false);
        self.set_c(c == 1);
    }
    fn rot_rrc_a(&mut self) {
        let c = self.a & 1;
        self.a = (self.a >> 1) | (c << 7);
        self.set_z(false);
        self.set_n(false);
        self.set_h(false);
        self.set_c(c == 1);
    }
    fn rot_rl_a(&mut self) {
        let c = self.a >> 7;
        self.a = (self.a << 1) | (self.c() as u8);
        self.set_z(false);
        self.set_n(false);
        self.set_h(false);
        self.set_c(c == 1);
    }
    fn rot_rr_a(&mut self) {
        let c = self.a & 1;
        self.a = (self.a >> 1) | ((self.c() as u8) << 7);
        self.set_z(false);
        self.set_n(false);
        self.set_h(false);
        self.set_c(c == 1);
    }

    fn exec_cb(&mut self, op: u8, bus: &mut Bus) -> u32 {
        // registers for reg ops, index 0..7 = B,C,D,E,H,L,(HL),A
        match op {
            0x00..=0x07 => self.cb_rlc(op & 7, bus),
            0x08..=0x0F => self.cb_rrc(op & 7, bus),
            0x10..=0x17 => self.cb_rl(op & 7, bus),
            0x18..=0x1F => self.cb_rr(op & 7, bus),
            0x20..=0x27 => self.cb_sla(op & 7, bus),
            0x28..=0x2F => self.cb_sra(op & 7, bus),
            0x30..=0x37 => self.cb_swap(op & 7, bus),
            0x38..=0x3F => self.cb_srl(op & 7, bus),
            0x40..=0x7F => self.cb_bit(op, bus),
            0x80..=0xBF => self.cb_res(op, bus),
            0xC0..=0xFF => self.cb_set(op, bus),
        }
    }

    fn read_reg_or_hl(&mut self, bus: &mut Bus, idx: u8) -> u8 {
        match idx {
            0 => self.b,
            1 => self.c,
            2 => self.d,
            3 => self.e,
            4 => self.h,
            5 => self.l,
            6 => self.read8(bus, self.hl()),
            7 => self.a,
            _ => unreachable!(),
        }
    }

    fn write_reg_or_hl(&mut self, bus: &mut Bus, idx: u8, v: u8) {
        match idx {
            0 => self.b = v,
            1 => self.c = v,
            2 => self.d = v,
            3 => self.e = v,
            4 => self.h = v,
            5 => self.l = v,
            6 => self.write8(bus, self.hl(), v),
            7 => self.a = v,
            _ => unreachable!(),
        }
    }

    fn cb_rotate(&mut self, bus: &mut Bus, idx: u8, rot: u8) -> u32 {
        let v = self.read_reg_or_hl(bus, idx);
        let (r, c) = match rot {
            0 => {
                let c = v >> 7;
                ((v << 1) | c, c == 1)
            }
            1 => {
                let c = v & 1;
                ((v >> 1) | (c << 7), c == 1)
            }
            2 => {
                let c = v >> 7;
                ((v << 1) | self.c() as u8, c == 1)
            }
            3 => {
                let c = v & 1;
                ((v >> 1) | ((self.c() as u8) << 7), c == 1)
            }
            _ => unreachable!(),
        };
        self.set_c(c);
        self.set_z(r == 0);
        self.set_n(false);
        self.set_h(false);
        self.write_reg_or_hl(bus, idx, r);
        if idx == 6 {
            16
        } else {
            8
        }
    }

    fn cb_rlc(&mut self, idx: u8, bus: &mut Bus) -> u32 {
        self.cb_rotate(bus, idx, 0)
    }
    fn cb_rrc(&mut self, idx: u8, bus: &mut Bus) -> u32 {
        self.cb_rotate(bus, idx, 1)
    }
    fn cb_rl(&mut self, idx: u8, bus: &mut Bus) -> u32 {
        self.cb_rotate(bus, idx, 2)
    }
    fn cb_rr(&mut self, idx: u8, bus: &mut Bus) -> u32 {
        self.cb_rotate(bus, idx, 3)
    }

    fn cb_shift(&mut self, bus: &mut Bus, idx: u8, shift: u8) -> u32 {
        let v = self.read_reg_or_hl(bus, idx);
        let (r, c) = match shift {
            0 => {
                // SLA
                let c = v >> 7;
                (v << 1, c == 1)
            }
            1 => {
                // SRA
                let c = v & 1;
                ((v >> 1) | (v & 0x80), c == 1)
            }
            2 => {
                // SRL
                let c = v & 1;
                (v >> 1, c == 1)
            }
            _ => unreachable!(),
        };
        self.set_c(c);
        self.set_z(r == 0);
        self.set_n(false);
        self.set_h(false);
        self.write_reg_or_hl(bus, idx, r);
        if idx == 6 {
            16
        } else {
            8
        }
    }

    fn cb_sla(&mut self, idx: u8, bus: &mut Bus) -> u32 {
        self.cb_shift(bus, idx, 0)
    }
    fn cb_sra(&mut self, idx: u8, bus: &mut Bus) -> u32 {
        self.cb_shift(bus, idx, 1)
    }
    fn cb_srl(&mut self, idx: u8, bus: &mut Bus) -> u32 {
        self.cb_shift(bus, idx, 2)
    }

    fn cb_swap(&mut self, idx: u8, bus: &mut Bus) -> u32 {
        let v = self.read_reg_or_hl(bus, idx);
        let r = v.rotate_right(4);
        self.set_z(r == 0);
        self.set_n(false);
        self.set_h(false);
        self.set_c(false);
        self.write_reg_or_hl(bus, idx, r);
        if idx == 6 {
            16
        } else {
            8
        }
    }

    fn cb_bit(&mut self, op: u8, bus: &mut Bus) -> u32 {
        let bit = (op >> 3) & 7;
        let idx = op & 7;
        let v = self.read_reg_or_hl(bus, idx);
        self.set_z(v & (1 << bit) == 0);
        self.set_n(false);
        self.set_h(true);
        if idx == 6 {
            12
        } else {
            8
        }
    }

    fn cb_res(&mut self, op: u8, bus: &mut Bus) -> u32 {
        let bit = (op >> 3) & 7;
        let idx = op & 7;
        let v = self.read_reg_or_hl(bus, idx);
        self.write_reg_or_hl(bus, idx, v & !(1 << bit));
        if idx == 6 {
            16
        } else {
            8
        }
    }

    fn cb_set(&mut self, op: u8, bus: &mut Bus) -> u32 {
        let bit = (op >> 3) & 7;
        let idx = op & 7;
        let v = self.read_reg_or_hl(bus, idx);
        self.write_reg_or_hl(bus, idx, v | (1 << bit));
        if idx == 6 {
            16
        } else {
            8
        }
    }

    pub fn take_interrupt(&mut self, bus: &mut Bus) -> u32 {
        let pending = bus.io[0x0F] & bus.ie & 0x1F;
        if pending == 0 {
            return 0;
        }
        self.ticked = 0;
        self.ime = false;
        self.halted = false;
        // Two internal cycles, then PC is pushed. The push of the high byte
        // can overwrite IE (SP = $0000), so the vector is chosen from the
        // flags as they stand after it; nothing left selects vector $0000.
        self.internal(bus);
        self.internal(bus);
        self.sp = self.sp.wrapping_sub(1);
        self.write8(bus, self.sp, (self.pc >> 8) as u8);
        let pending = bus.io[0x0F] & bus.ie & 0x1F;
        self.sp = self.sp.wrapping_sub(1);
        self.write8(bus, self.sp, self.pc as u8);
        self.internal(bus);
        if pending == 0 {
            self.pc = 0;
        } else {
            let bit = pending.trailing_zeros() as usize;
            if bit == 2 {
                self.timer_interrupts += 1;
            }
            bus.io[0x0F] &= !(1 << bit);
            self.pc = VECTORS[bit];
        }
        self.ticked
    }
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::Cartridge;

    fn cpu_with_bus() -> (Cpu, Bus) {
        let mut rom = vec![0u8; 0x8000];
        rom[0x147] = 0x00;
        let cart = Cartridge::load(&rom).unwrap();
        let bus = Bus::new(cart);
        (Cpu::new(), bus)
    }

    #[test]
    fn nop() {
        let (mut cpu, mut bus) = cpu_with_bus();
        assert_eq!(cpu.execute(&mut bus), 4);
        assert_eq!(cpu.pc, 0x0101);
    }

    #[test]
    fn ld_bc_d16() {
        let (mut cpu, mut bus) = cpu_with_bus();
        bus.cart.rom[0x0100] = 0x01;
        bus.cart.rom[0x0101] = 0x34;
        bus.cart.rom[0x0102] = 0x12;
        cpu.execute(&mut bus);
        assert_eq!(cpu.bc(), 0x1234);
        assert_eq!(cpu.pc, 0x0103);
    }

    #[test]
    fn add_a() {
        let (mut cpu, mut bus) = cpu_with_bus();
        cpu.a = 0x0F;
        cpu.b = 0x01;
        bus.cart.rom[0x0100] = 0x80;
        cpu.execute(&mut bus);
        assert_eq!(cpu.a, 0x10);
        assert!(!cpu.z());
        assert!(!cpu.n());
        assert!(cpu.h());
        assert!(!cpu.c());
    }

    #[test]
    fn add_a_carry() {
        let (mut cpu, mut bus) = cpu_with_bus();
        cpu.a = 0xFF;
        cpu.b = 0x01;
        bus.cart.rom[0x0100] = 0x80;
        cpu.execute(&mut bus);
        assert_eq!(cpu.a, 0x00);
        assert!(cpu.z());
        assert!(cpu.c());
    }

    #[test]
    fn inc_b_half_carry() {
        let (mut cpu, mut bus) = cpu_with_bus();
        cpu.b = 0x0F;
        bus.cart.rom[0x0100] = 0x04;
        cpu.execute(&mut bus);
        assert_eq!(cpu.b, 0x10);
        assert!(cpu.h());
        assert!(!cpu.z());
    }

    #[test]
    fn push_pop() {
        let (mut cpu, mut bus) = cpu_with_bus();
        bus.cart.rom[0x0100] = 0xC5; // push bc
        cpu.b = 0xAB;
        cpu.c = 0xCD;
        cpu.execute(&mut bus);
        assert_eq!(cpu.sp, 0xFFFC);
        bus.cart.rom[0x0100] = 0xC1; // pop bc
        cpu.pc = 0x0100;
        cpu.b = 0;
        cpu.c = 0;
        cpu.execute(&mut bus);
        assert_eq!(cpu.bc(), 0xABCD);
    }

    #[test]
    fn cb_swap() {
        let (mut cpu, mut bus) = cpu_with_bus();
        bus.cart.rom[0x0100] = 0xCB;
        bus.cart.rom[0x0101] = 0x37; // swap a
        cpu.a = 0xAB;
        cpu.execute(&mut bus);
        assert_eq!(cpu.a, 0xBA);
        assert_eq!(cpu.pc, 0x0102);
    }

    #[test]
    fn undefined_opcode_is_two_byte_nop() {
        let (mut cpu, mut bus) = cpu_with_bus();
        bus.cart.rom[0x0100] = 0xD3; // undefined
        bus.cart.rom[0x0101] = 0xAA; // consumed padding
        bus.cart.rom[0x0102] = 0x00; // nop
        assert_eq!(cpu.execute(&mut bus), 8);
        assert_eq!(cpu.pc, 0x0102, "undefined opcode consumes the padding byte");
        assert_eq!(cpu.execute(&mut bus), 4);
        assert_eq!(cpu.pc, 0x0103);
    }

    #[test]
    fn halt_bug_executes_following_instruction_twice() {
        let (mut cpu, mut bus) = cpu_with_bus();
        bus.ie = 0x01;
        bus.io[0x0F] = 0x01; // vblank pending, IME off -> HALT bug
        cpu.ime = false;
        bus.cart.rom[0x0100] = 0x76; // halt
        bus.cart.rom[0x0101] = 0x04; // inc b
        bus.cart.rom[0x0102] = 0x00; // nop
        cpu.b = 0x05;

        cpu.execute(&mut bus); // halt (bug branch)
        assert!(!cpu.halted);
        assert!(cpu.halt_bug);

        cpu.execute(&mut bus); // inc b, first run
        assert_eq!(cpu.b, 0x06);

        cpu.execute(&mut bus); // inc b, second run (PC rewound)
        assert_eq!(cpu.b, 0x07);
        assert_eq!(
            cpu.pc, 0x0102,
            "continues at the byte after the doubled instruction"
        );
    }

    #[test]
    fn halt_without_pending_interrupt_halts() {
        let (mut cpu, mut bus) = cpu_with_bus();
        bus.cart.rom[0x0100] = 0x76; // halt
        cpu.execute(&mut bus);
        assert!(cpu.halted);
        assert!(!cpu.halt_bug);
    }

    #[test]
    fn stop_sets_stopped_and_consumes_padding() {
        let (mut cpu, mut bus) = cpu_with_bus();
        bus.cart.rom[0x0100] = 0x10; // stop
        bus.cart.rom[0x0101] = 0x00; // padding
        bus.cart.rom[0x0102] = 0x00; // nop
        cpu.execute(&mut bus);
        assert!(cpu.stopped);
        assert_eq!(cpu.pc, 0x0102, "STOP consumes its padding byte");
    }
}
