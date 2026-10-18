//! The register file and the status registers built from its bits
//! (Teakra's `register.h`).

use crate::operand::{sext32, Cond};

/// One level of block repeat: first and last address, and loop counter.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct BlockRepeat {
    pub start: u32,
    pub end: u32,
    pub lc: u16,
}

/// Flags a context switch saves and restores.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct ShadowFlags {
    flm: u16,
    fvl: u16,
    fe: u16,
    fc0: u16,
    fc1: u16,
    fv: u16,
    fn_: u16,
    fm: u16,
    fz: u16,
    fr: u16,
}

/// Mode bits a context switch exchanges with their shadows.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct ShadowModes {
    pcmhi: u16,
    sat: u16,
    sata: u16,
    hwm: u16,
    s: u16,
    ps: [u16; 2],
    page: u16,
    stp16: u16,
    cmd: u16,
    m: [u16; 8],
    br: [u16; 8],
    im: [u16; 3],
    imv: u16,
    epi: u16,
    epj: u16,
}

/// The shadow of one address-unit configuration: register, step and
/// offset of the i and j halves.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct ShadowAr {
    rni: u16,
    rnj: u16,
    stepi: u16,
    stepj: u16,
    offseti: u16,
    offsetj: u16,
}

/// Every register of the core. Accumulators are 40 bits, kept in 64 with
/// the upper 24 bits a copy of bit 39.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Regs {
    // Program control.
    /// 18-bit program counter.
    pub pc: u32,
    pub prpage: u16,
    /// Word order of a pushed program counter.
    pub cpc: u16,
    pub repc: u16,
    pub repcs: u16,
    pub rep: bool,
    pub crep: u16,
    /// Block repeat nesting level and in-loop flag.
    pub bcn: u16,
    pub lp: u16,
    pub bkrep_stack: [BlockRepeat; 4],

    // Computation.
    pub a: [u64; 2],
    pub b: [u64; 2],
    pub a1s: u64,
    pub b1s: u64,
    pub ccnta: u16,
    /// Saturation off when moving from (`sat`) and to (`sata`) an
    /// accumulator.
    pub sat: u16,
    pub sata: u16,
    /// Shift mode: 0 arithmetic, 1 logic.
    pub s: u16,
    /// Shift value.
    pub sv: u16,
    pub fz: u16,
    pub fm: u16,
    pub fn_: u16,
    pub fv: u16,
    pub fe: u16,
    pub fc0: u16,
    pub fc1: u16,
    pub flm: u16,
    pub fvl: u16,
    pub fr: u16,
    pub vtr0: u16,
    pub vtr1: u16,

    // Multiplication.
    pub x: [u16; 2],
    pub y: [u16; 2],
    /// Half-word mode of `y`.
    pub hwm: u16,
    pub p: [u32; 2],
    pub pe: [u16; 2],
    /// Product shift mode.
    pub ps: [u16; 2],
    pub p0h_cbs: u16,

    // Addresses.
    pub r: [u16; 8],
    pub mixp: u16,
    pub sp: u16,
    pub page: u16,
    pub pcmhi: u16,
    pub r0b: u16,
    pub r1b: u16,
    pub r4b: u16,
    pub r7b: u16,
    pub stepi: u16,
    pub stepj: u16,
    pub modi: u16,
    pub modj: u16,
    pub stepi0: u16,
    pub stepj0: u16,
    pub stepib: u16,
    pub stepjb: u16,
    pub modib: u16,
    pub modjb: u16,
    pub stepi0b: u16,
    pub stepj0b: u16,
    /// Modulo arithmetic and bit reversal, by address register.
    pub m: [u16; 8],
    pub br: [u16; 8],
    pub stp16: u16,
    /// Step and modulo method: 0 Teak, 1 TeakLite.
    pub cmd: u16,
    pub epi: u16,
    pub epj: u16,

    // Indirect addressing: steps (0 +0, 1 +1, 2 -1, 3 +s, 4 +2, 5 -2, 6 +2*,
    // 7 -2*), offsets (0 +0, 1 +1, 2 -1, 3 -1*) and registers.
    pub arstep: [u16; 4],
    pub arpstepi: [u16; 4],
    pub arpstepj: [u16; 4],
    pub aroffset: [u16; 4],
    pub arpoffseti: [u16; 4],
    pub arpoffsetj: [u16; 4],
    pub arrn: [u16; 4],
    pub arprni: [u16; 4],
    pub arprnj: [u16; 4],

    // Interrupts: pending, enabled, context switching, master enable.
    pub ip: [u16; 3],
    pub ipv: u16,
    pub im: [u16; 3],
    pub imv: u16,
    pub ic: [u16; 3],
    pub nimc: u16,
    pub ie: u16,

    // Extension unit.
    pub ou: [u16; 5],
    pub iu: [u16; 2],
    pub ext: [u16; 4],
    pub mod0_unk_const: u16,

    shadow_flags: ShadowFlags,
    shadow_modes: ShadowModes,
    shadow_ar: [ShadowAr; 2],
    shadow_arp: [ShadowAr; 4],
}

impl Default for Regs {
    fn default() -> Self {
        Regs {
            pc: 0,
            prpage: 0,
            cpc: 1,
            repc: 0,
            repcs: 0,
            rep: false,
            crep: 1,
            bcn: 0,
            lp: 0,
            bkrep_stack: [BlockRepeat::default(); 4],
            a: [0; 2],
            b: [0; 2],
            a1s: 0,
            b1s: 0,
            ccnta: 1,
            sat: 0,
            sata: 1,
            s: 0,
            sv: 0,
            fz: 0,
            fm: 0,
            fn_: 0,
            fv: 0,
            fe: 0,
            fc0: 0,
            fc1: 0,
            flm: 0,
            fvl: 0,
            fr: 0,
            vtr0: 0,
            vtr1: 0,
            x: [0; 2],
            y: [0; 2],
            hwm: 0,
            p: [0; 2],
            pe: [0; 2],
            ps: [0; 2],
            p0h_cbs: 0,
            r: [0; 8],
            mixp: 0,
            sp: 0,
            page: 0,
            pcmhi: 0,
            r0b: 0,
            r1b: 0,
            r4b: 0,
            r7b: 0,
            stepi: 0,
            stepj: 0,
            modi: 0,
            modj: 0,
            stepi0: 0,
            stepj0: 0,
            stepib: 0,
            stepjb: 0,
            modib: 0,
            modjb: 0,
            stepi0b: 0,
            stepj0b: 0,
            m: [0; 8],
            br: [0; 8],
            stp16: 0,
            cmd: 1,
            epi: 0,
            epj: 0,
            arstep: [1, 4, 5, 3],
            arpstepi: [1, 4, 5, 3],
            arpstepj: [1, 4, 5, 3],
            aroffset: [0, 1, 2, 0],
            arpoffseti: [0, 1, 2, 0],
            arpoffsetj: [0, 1, 2, 0],
            arrn: [0, 4, 2, 6],
            arprni: [0, 1, 2, 3],
            arprnj: [0, 1, 2, 3],
            ip: [0; 3],
            ipv: 0,
            im: [0; 3],
            imv: 0,
            ic: [0; 3],
            nimc: 0,
            ie: 0,
            ou: [0; 5],
            iu: [0; 2],
            ext: [0; 4],
            mod0_unk_const: 1,
            shadow_flags: ShadowFlags::default(),
            shadow_modes: ShadowModes::default(),
            shadow_ar: [ShadowAr::default(); 2],
            shadow_arp: [ShadowAr::default(); 4],
        }
    }
}

/// Bit `pos` of `value`, as 0 or 1.
fn bit(value: u16, pos: u32) -> u16 {
    value >> pos & 1
}

impl Regs {
    /// The loop counter of the innermost block repeat.
    pub fn lc(&self) -> u16 {
        if self.lp != 0 {
            self.bkrep_stack[self.bcn as usize - 1].lc
        } else {
            self.bkrep_stack[0].lc
        }
    }

    pub fn lc_mut(&mut self) -> &mut u16 {
        if self.lp != 0 {
            &mut self.bkrep_stack[self.bcn as usize - 1].lc
        } else {
            &mut self.bkrep_stack[0].lc
        }
    }

    // ---- context switching ---------------------------------------------

    pub fn shadow_store(&mut self) {
        self.shadow_flags = ShadowFlags {
            flm: self.flm,
            fvl: self.fvl,
            fe: self.fe,
            fc0: self.fc0,
            fc1: self.fc1,
            fv: self.fv,
            fn_: self.fn_,
            fm: self.fm,
            fz: self.fz,
            fr: self.fr,
        };
    }

    pub fn shadow_restore(&mut self) {
        let s = self.shadow_flags;
        self.flm = s.flm;
        self.fvl = s.fvl;
        self.fe = s.fe;
        self.fc0 = s.fc0;
        self.fc1 = s.fc1;
        self.fv = s.fv;
        self.fn_ = s.fn_;
        self.fm = s.fm;
        self.fz = s.fz;
        self.fr = s.fr;
    }

    pub fn shadow_swap(&mut self) {
        let s = &mut self.shadow_modes;
        std::mem::swap(&mut self.pcmhi, &mut s.pcmhi);
        std::mem::swap(&mut self.sat, &mut s.sat);
        std::mem::swap(&mut self.sata, &mut s.sata);
        std::mem::swap(&mut self.hwm, &mut s.hwm);
        std::mem::swap(&mut self.s, &mut s.s);
        std::mem::swap(&mut self.ps, &mut s.ps);
        std::mem::swap(&mut self.page, &mut s.page);
        std::mem::swap(&mut self.stp16, &mut s.stp16);
        std::mem::swap(&mut self.cmd, &mut s.cmd);
        std::mem::swap(&mut self.m, &mut s.m);
        std::mem::swap(&mut self.br, &mut s.br);
        std::mem::swap(&mut self.im, &mut s.im);
        std::mem::swap(&mut self.imv, &mut s.imv);
        std::mem::swap(&mut self.epi, &mut s.epi);
        std::mem::swap(&mut self.epj, &mut s.epj);
        self.swap_all_ar_arp();
    }

    pub fn swap_all_ar_arp(&mut self) {
        self.swap_ar(0);
        self.swap_ar(1);
        for i in 0..4 {
            self.swap_arp(i);
        }
    }

    pub fn swap_ar(&mut self, index: u16) {
        let i = index as usize & 1;
        let s = &mut self.shadow_ar[i];
        std::mem::swap(&mut self.arrn[i * 2], &mut s.rni);
        std::mem::swap(&mut self.arrn[i * 2 + 1], &mut s.rnj);
        std::mem::swap(&mut self.arstep[i * 2], &mut s.stepi);
        std::mem::swap(&mut self.arstep[i * 2 + 1], &mut s.stepj);
        std::mem::swap(&mut self.aroffset[i * 2], &mut s.offseti);
        std::mem::swap(&mut self.aroffset[i * 2 + 1], &mut s.offsetj);
    }

    pub fn swap_arp(&mut self, index: u16) {
        let i = index as usize & 3;
        let s = &mut self.shadow_arp[i];
        std::mem::swap(&mut self.arprni[i], &mut s.rni);
        std::mem::swap(&mut self.arprnj[i], &mut s.rnj);
        std::mem::swap(&mut self.arpstepi[i], &mut s.stepi);
        std::mem::swap(&mut self.arpstepj[i], &mut s.stepj);
        std::mem::swap(&mut self.arpoffseti[i], &mut s.offseti);
        std::mem::swap(&mut self.arpoffsetj[i], &mut s.offsetj);
    }

    pub fn condition(&self, cond: Cond) -> bool {
        match cond {
            Cond::True => true,
            Cond::Eq => self.fz == 1,
            Cond::Neq => self.fz == 0,
            Cond::Gt => self.fz == 0 && self.fm == 0,
            Cond::Ge => self.fm == 0,
            Cond::Lt => self.fm == 1,
            Cond::Le => self.fm == 1 || self.fz == 1,
            Cond::Nn => self.fn_ == 0,
            Cond::C => self.fc0 == 1,
            Cond::V => self.fv == 1,
            Cond::E => self.fe == 1,
            Cond::L => self.flm == 1 || self.fvl == 1,
            Cond::Nr => self.fr == 0,
            Cond::Niu0 => self.iu[0] == 0,
            Cond::Iu0 => self.iu[0] == 1,
            Cond::Iu1 => self.iu[1] == 1,
        }
    }

    // ---- registers made of other registers' bits -------------------------

    fn set_lp(&mut self, value: u16) {
        if value != 0 {
            self.lp = 0;
            self.bcn = 0;
        }
    }

    /// Bits 32-35 of accumulator a`index`, read as 4 bits and written
    /// sign-extended.
    fn acc_e(&self, index: usize) -> u16 {
        (self.a[index] >> 32 & 0xF) as u16
    }

    fn set_acc_e(&mut self, index: usize, value: u16) {
        let value32 = sext32(value as u32, 4);
        self.a[index] = self.a[index] & 0xFFFF_FFFF | (value32 as u64) << 32;
    }

    pub fn cfgi(&self) -> u16 {
        self.stepi & 0x7F | (self.modi & 0x1FF) << 7
    }

    pub fn set_cfgi(&mut self, v: u16) {
        self.stepi = v & 0x7F;
        self.modi = v >> 7 & 0x1FF;
    }

    pub fn cfgj(&self) -> u16 {
        self.stepj & 0x7F | (self.modj & 0x1FF) << 7
    }

    pub fn set_cfgj(&mut self, v: u16) {
        self.stepj = v & 0x7F;
        self.modj = v >> 7 & 0x1FF;
    }

    pub fn stt0(&self) -> u16 {
        self.flm
            | self.fvl << 1
            | self.fe << 2
            | self.fc0 << 3
            | self.fv << 4
            | self.fn_ << 5
            | self.fm << 6
            | self.fz << 7
            | self.fc1 << 11
    }

    pub fn set_stt0(&mut self, v: u16) {
        self.flm = bit(v, 0);
        self.fvl = bit(v, 1);
        self.fe = bit(v, 2);
        self.fc0 = bit(v, 3);
        self.fv = bit(v, 4);
        self.fn_ = bit(v, 5);
        self.fm = bit(v, 6);
        self.fz = bit(v, 7);
        self.fc1 = bit(v, 11);
    }

    pub fn stt1(&self) -> u16 {
        self.fr << 4 | self.iu[0] << 10 | self.iu[1] << 11 | self.pe[0] << 14 | self.pe[1] << 15
    }

    pub fn set_stt1(&mut self, v: u16) {
        self.fr = bit(v, 4);
        self.pe[0] = bit(v, 14);
        self.pe[1] = bit(v, 15);
    }

    pub fn stt2(&self) -> u16 {
        self.ip[0]
            | self.ip[1] << 1
            | self.ip[2] << 2
            | self.ipv << 3
            | (self.pcmhi & 3) << 6
            | (self.bcn & 7) << 12
            | self.lp << 15
    }

    pub fn set_stt2(&mut self, v: u16) {
        self.pcmhi = v >> 6 & 3;
        self.set_lp(bit(v, 15));
    }

    pub fn mod0(&self) -> u16 {
        self.sat
            | self.sata << 1
            | (self.mod0_unk_const & 7) << 2
            | (self.hwm & 3) << 5
            | self.s << 7
            | self.ou[0] << 8
            | self.ou[1] << 9
            | (self.ps[0] & 3) << 10
            | (self.ps[1] & 3) << 13
    }

    pub fn set_mod0(&mut self, v: u16) {
        self.sat = bit(v, 0);
        self.sata = bit(v, 1);
        self.hwm = v >> 5 & 3;
        self.s = bit(v, 7);
        self.ou[0] = bit(v, 8);
        self.ou[1] = bit(v, 9);
        self.ps[0] = v >> 10 & 3;
        self.ps[1] = v >> 13 & 3;
    }

    pub fn mod1(&self) -> u16 {
        (self.page & 0xFF) | self.stp16 << 12 | self.cmd << 13 | self.epi << 14 | self.epj << 15
    }

    pub fn set_mod1(&mut self, v: u16) {
        self.page = v & 0xFF;
        self.stp16 = bit(v, 12);
        self.cmd = bit(v, 13);
        self.epi = bit(v, 14);
        self.epj = bit(v, 15);
    }

    pub fn mod2(&self) -> u16 {
        (0..8).fold(0, |v, i| v | self.m[i] << i | self.br[i] << (i + 8))
    }

    pub fn set_mod2(&mut self, v: u16) {
        for i in 0..8 {
            self.m[i] = bit(v, i as u32);
            self.br[i] = bit(v, i as u32 + 8);
        }
    }

    pub fn mod3(&self) -> u16 {
        self.nimc
            | self.ic[0] << 1
            | self.ic[1] << 2
            | self.ic[2] << 3
            | self.ou[2] << 4
            | self.ou[3] << 5
            | self.ou[4] << 6
            | self.ie << 7
            | self.im[0] << 8
            | self.im[1] << 9
            | self.im[2] << 10
            | self.imv << 11
            | self.ccnta << 13
            | self.cpc << 14
            | self.crep << 15
    }

    pub fn set_mod3(&mut self, v: u16) {
        self.nimc = bit(v, 0);
        self.ic[0] = bit(v, 1);
        self.ic[1] = bit(v, 2);
        self.ic[2] = bit(v, 3);
        self.ou[2] = bit(v, 4);
        self.ou[3] = bit(v, 5);
        self.ou[4] = bit(v, 6);
        self.ie = bit(v, 7);
        self.im[0] = bit(v, 8);
        self.im[1] = bit(v, 9);
        self.im[2] = bit(v, 10);
        self.imv = bit(v, 11);
        self.ccnta = bit(v, 13);
        self.cpc = bit(v, 14);
        self.crep = bit(v, 15);
    }

    pub fn st0(&self) -> u16 {
        self.sat
            | self.ie << 1
            | self.im[0] << 2
            | self.im[1] << 3
            | self.fr << 4
            | (self.flm | self.fvl) << 5
            | self.fe << 6
            | self.fc0 << 7
            | self.fv << 8
            | self.fn_ << 9
            | self.fm << 10
            | self.fz << 11
            | self.acc_e(0) << 12
    }

    pub fn set_st0(&mut self, v: u16) {
        self.sat = bit(v, 0);
        self.ie = bit(v, 1);
        self.im[0] = bit(v, 2);
        self.im[1] = bit(v, 3);
        self.fr = bit(v, 4);
        self.flm = bit(v, 5);
        self.fvl = bit(v, 5);
        self.fe = bit(v, 6);
        self.fc0 = bit(v, 7);
        self.fv = bit(v, 8);
        self.fn_ = bit(v, 9);
        self.fm = bit(v, 10);
        self.fz = bit(v, 11);
        self.set_acc_e(0, v >> 12);
    }

    pub fn st1(&self) -> u16 {
        (self.page & 0xFF) | (self.ps[0] & 3) << 10 | self.acc_e(1) << 12
    }

    pub fn set_st1(&mut self, v: u16) {
        self.page = v & 0xFF;
        self.ps[0] = v >> 10 & 3;
        self.set_acc_e(1, v >> 12);
    }

    pub fn st2(&self) -> u16 {
        (0..6).fold(0, |v, i| v | self.m[i] << i)
            | self.im[2] << 6
            | self.s << 7
            | self.ou[0] << 8
            | self.ou[1] << 9
            | self.iu[0] << 10
            | self.iu[1] << 11
            | self.ip[2] << 13
            | self.ip[0] << 14
            | self.ip[1] << 15
    }

    pub fn set_st2(&mut self, v: u16) {
        for i in 0..6 {
            self.m[i] = bit(v, i as u32);
        }
        self.im[2] = bit(v, 6);
        self.s = bit(v, 7);
        self.ou[0] = bit(v, 8);
        self.ou[1] = bit(v, 9);
    }

    pub fn icr(&self) -> u16 {
        self.nimc
            | self.ic[0] << 1
            | self.ic[1] << 2
            | self.ic[2] << 3
            | self.lp << 4
            | (self.bcn & 7) << 5
    }

    pub fn set_icr(&mut self, v: u16) {
        self.nimc = bit(v, 0);
        self.ic[0] = bit(v, 1);
        self.ic[1] = bit(v, 2);
        self.ic[2] = bit(v, 3);
        self.set_lp(bit(v, 4));
    }

    /// Address configuration register ar0 or ar1.
    pub fn ar(&self, index: usize) -> u16 {
        let (i, j) = (index * 2, index * 2 + 1);
        (self.arstep[j] & 7)
            | (self.aroffset[j] & 3) << 3
            | (self.arstep[i] & 7) << 5
            | (self.aroffset[i] & 3) << 8
            | (self.arrn[j] & 7) << 10
            | (self.arrn[i] & 7) << 13
    }

    pub fn set_ar(&mut self, index: usize, v: u16) {
        let (i, j) = (index * 2, index * 2 + 1);
        self.arstep[j] = v & 7;
        self.aroffset[j] = v >> 3 & 3;
        self.arstep[i] = v >> 5 & 7;
        self.aroffset[i] = v >> 8 & 3;
        self.arrn[j] = v >> 10 & 7;
        self.arrn[i] = v >> 13 & 7;
    }

    /// Address configuration register arp0 to arp3.
    pub fn arp(&self, i: usize) -> u16 {
        (self.arpstepi[i] & 7)
            | (self.arpoffseti[i] & 3) << 3
            | (self.arpstepj[i] & 7) << 5
            | (self.arpoffsetj[i] & 3) << 8
            | (self.arprni[i] & 3) << 10
            | (self.arprnj[i] & 3) << 13
    }

    pub fn set_arp(&mut self, i: usize, v: u16) {
        self.arpstepi[i] = v & 7;
        self.arpoffseti[i] = v >> 3 & 3;
        self.arpstepj[i] = v >> 5 & 7;
        self.arpoffsetj[i] = v >> 8 & 3;
        self.arprni[i] = v >> 10 & 3;
        self.arprnj[i] = v >> 13 & 3;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_registers_round_trip_their_bits() {
        let mut regs = Regs::default();
        regs.set_stt0(0x08FF);
        assert_eq!(regs.stt0(), 0x08FF);
        assert_eq!((regs.fz, regs.fc1, regs.flm), (1, 1, 1));
        regs.set_mod0(0xFFFF);
        // Bits 2-4 read a constant 1, bit 12 is not there.
        assert_eq!(regs.mod0(), 0x6FE7);
        regs.set_mod1(0xF0AB);
        assert_eq!(regs.mod1(), 0xF0AB);
        regs.set_mod2(0xA55A);
        assert_eq!(regs.mod2(), 0xA55A);
        regs.set_ar(1, 0xBEEF);
        assert_eq!(regs.ar(1), 0xBEEF);
        regs.set_arp(2, 0x6FFF);
        assert_eq!(regs.arp(2), 0x6FFF);
        regs.set_cfgi(0xFFFF);
        assert_eq!((regs.stepi, regs.modi), (0x7F, 0x1FF));
    }

    #[test]
    fn st0_writes_the_accumulator_extension_sign_extended() {
        let mut regs = Regs::default();
        regs.a[0] = 0x1234_5678;
        regs.set_st0(0x8000);
        assert_eq!(regs.a[0], 0xFFFF_FFF8_1234_5678);
        assert_eq!(regs.st0() >> 12, 8);
        regs.set_st0(0x7000);
        assert_eq!(regs.a[0], 0x0000_0007_1234_5678);
    }

    #[test]
    fn a_context_switch_exchanges_modes_with_their_shadows() {
        let mut regs = Regs {
            page: 0x12,
            arrn: [5, 4, 2, 6],
            ..Regs::default()
        };
        regs.shadow_swap();
        assert_eq!((regs.page, regs.arrn[0], regs.sata), (0, 0, 0));
        regs.shadow_swap();
        assert_eq!((regs.page, regs.arrn[0], regs.sata), (0x12, 5, 1));
    }
}
