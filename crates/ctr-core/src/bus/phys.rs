//! Physical memory shared by both processors (3dbrew, "Memory layout").

use emu_core::Mem;

pub const ARM9_RAM_BASE: u32 = 0x0800_0000;
pub const ARM9_RAM_LEN: usize = 0x10_0000;
pub const VRAM_BASE: u32 = 0x1800_0000;
pub const VRAM_LEN: usize = 0x60_0000;
pub const DSP_RAM_BASE: u32 = 0x1FF0_0000;
pub const DSP_RAM_LEN: usize = 0x8_0000;
pub const AXI_WRAM_BASE: u32 = 0x1FF8_0000;
pub const AXI_WRAM_LEN: usize = 0x8_0000;
pub const FCRAM_BASE: u32 = 0x2000_0000;
pub const FCRAM_LEN: usize = 0x800_0000;

/// A RAM region on the physical bus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Region {
    /// ARM9-private work RAM.
    Arm9Ram,
    Vram,
    DspRam,
    AxiWram,
    Fcram,
}

impl Region {
    const ALL: [Region; 5] = [
        Region::Arm9Ram,
        Region::Vram,
        Region::DspRam,
        Region::AxiWram,
        Region::Fcram,
    ];

    pub const fn base(self) -> u32 {
        match self {
            Region::Arm9Ram => ARM9_RAM_BASE,
            Region::Vram => VRAM_BASE,
            Region::DspRam => DSP_RAM_BASE,
            Region::AxiWram => AXI_WRAM_BASE,
            Region::Fcram => FCRAM_BASE,
        }
    }

    pub const fn size(self) -> usize {
        match self {
            Region::Arm9Ram => ARM9_RAM_LEN,
            Region::Vram => VRAM_LEN,
            Region::DspRam => DSP_RAM_LEN,
            Region::AxiWram => AXI_WRAM_LEN,
            Region::Fcram => FCRAM_LEN,
        }
    }

    /// The region holding physical address `addr`, and the offset into it.
    pub fn of(addr: u32) -> Option<(Region, usize)> {
        Region::ALL.into_iter().find_map(|r| {
            let offset = addr.checked_sub(r.base())? as usize;
            (offset < r.size()).then_some((r, offset))
        })
    }
}

/// Every RAM on the physical bus.
pub struct PhysMem {
    arm9_ram: Mem<u8, ARM9_RAM_LEN>,
    vram: Mem<u8, VRAM_LEN>,
    dsp_ram: Mem<u8, DSP_RAM_LEN>,
    axi_wram: Mem<u8, AXI_WRAM_LEN>,
    fcram: Mem<u8, FCRAM_LEN>,
}

impl Default for PhysMem {
    fn default() -> Self {
        Self::new()
    }
}

impl PhysMem {
    pub fn new() -> Self {
        PhysMem {
            arm9_ram: Mem::zeroed(),
            vram: Mem::zeroed(),
            dsp_ram: Mem::zeroed(),
            axi_wram: Mem::zeroed(),
            fcram: Mem::zeroed(),
        }
    }

    pub fn region(&self, region: Region) -> &[u8] {
        match region {
            Region::Arm9Ram => &self.arm9_ram[..],
            Region::Vram => &self.vram[..],
            Region::DspRam => &self.dsp_ram[..],
            Region::AxiWram => &self.axi_wram[..],
            Region::Fcram => &self.fcram[..],
        }
    }

    pub fn region_mut(&mut self, region: Region) -> &mut [u8] {
        match region {
            Region::Arm9Ram => &mut self.arm9_ram[..],
            Region::Vram => &mut self.vram[..],
            Region::DspRam => &mut self.dsp_ram[..],
            Region::AxiWram => &mut self.axi_wram[..],
            Region::Fcram => &mut self.fcram[..],
        }
    }

    /// `len` bytes of RAM at physical address `addr`, if one region holds
    /// all of them.
    pub fn slice(&self, addr: u32, len: usize) -> Option<&[u8]> {
        let (region, offset) = Region::of(addr)?;
        self.region(region).get(offset..offset.checked_add(len)?)
    }

    pub fn slice_mut(&mut self, addr: u32, len: usize) -> Option<&mut [u8]> {
        let (region, offset) = Region::of(addr)?;
        self.region_mut(region)
            .get_mut(offset..offset.checked_add(len)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_edges_of_every_region() {
        assert_eq!(Region::of(0x0800_0000), Some((Region::Arm9Ram, 0)));
        assert_eq!(Region::of(0x080F_FFFF), Some((Region::Arm9Ram, 0xF_FFFF)));
        assert_eq!(Region::of(0x0810_0000), None);
        assert_eq!(Region::of(0x1800_0000), Some((Region::Vram, 0)));
        assert_eq!(Region::of(0x185F_FFFF), Some((Region::Vram, 0x5F_FFFF)));
        assert_eq!(Region::of(0x1860_0000), None);
        assert_eq!(Region::of(0x1FF0_0000), Some((Region::DspRam, 0)));
        assert_eq!(Region::of(0x1FF8_0000), Some((Region::AxiWram, 0)));
        assert_eq!(Region::of(0x1FFF_FFFF), Some((Region::AxiWram, 0x7_FFFF)));
        assert_eq!(Region::of(0x2000_0000), Some((Region::Fcram, 0)));
        assert_eq!(Region::of(0x27FF_FFFF), Some((Region::Fcram, 0x7FF_FFFF)));
        assert_eq!(Region::of(0x2800_0000), None);
        assert_eq!(Region::of(0), None);
    }

    #[test]
    fn slices_never_span_two_regions() {
        let mut mem = PhysMem::new();
        mem.slice_mut(0x1FF8_0000, 4)
            .unwrap()
            .copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(mem.slice(0x1FF8_0002, 2), Some(&[3u8, 4][..]));
        assert!(mem.slice(0x1FF7_FFFE, 4).is_none());
        assert!(mem.slice(0x27FF_FFFF, 2).is_none());
        assert!(mem.slice(0x1000_0000, 1).is_none());
    }
}
