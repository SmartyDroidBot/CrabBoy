//! The DMA engine and the AHBM bridge it reaches the ARM's memory through
//! (Teakra's `dma.cpp`, `ahbm.cpp`, `dma.md`, `ahbm.md`). Transfers run to
//! the end as soon as they start.

use crate::Bus;
use std::collections::VecDeque;

/// Where data memory starts in DSP RAM, in words.
const DATA_MEMORY_OFFSET: u32 = 0x2_0000;

fn read_word<B: Bus + ?Sized>(bus: &mut B, word: u32) -> u16 {
    let ram = bus.ram();
    let at = (word as usize * 2) & (ram.len() - 1);
    u16::from_le_bytes([ram[at], ram[at + 1]])
}

fn write_word<B: Bus + ?Sized>(bus: &mut B, word: u32, value: u16) {
    let ram = bus.ram();
    let at = (word as usize * 2) & (ram.len() - 1);
    ram[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct AhbmChannel {
    /// 0 bytes, 1 halfwords, 2 words.
    pub unit_size: u16,
    /// 0 one unit, 1 four, 2 eight.
    pub burst_size: u16,
    /// 0 read, 1 write.
    pub direction: u16,
    /// The DMA channels using this one, a bit each.
    pub dma_channel: u16,
    burst: VecDeque<u32>,
    write_burst_start: u32,
}

impl AhbmChannel {
    fn burst_len(&self) -> usize {
        match self.burst_size {
            1 => 4,
            2 => 8,
            _ => 1,
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Ahbm {
    pub channels: [AhbmChannel; 3],
}

impl Ahbm {
    pub fn busy(&self) -> u16 {
        0
    }

    pub fn read16<B: Bus + ?Sized>(&mut self, bus: &mut B, channel: usize, addr: u32) -> u16 {
        let value = self.read32(bus, channel, addr);
        if addr & 1 == 0 {
            value as u16
        } else {
            (value >> 16) as u16
        }
    }

    pub fn read32<B: Bus + ?Sized>(&mut self, bus: &mut B, channel: usize, addr: u32) -> u32 {
        let c = &mut self.channels[channel];
        if c.burst.is_empty() {
            let mut current = addr;
            for _ in 0..c.burst_len() {
                let value = match c.unit_size {
                    0 => {
                        let mut v = bus.ahb_read8(current) as u32;
                        // Tested on hardware: an odd byte comes in the high half.
                        if current & 1 == 1 {
                            v <<= 8;
                        }
                        current = current.wrapping_add(1);
                        v
                    }
                    1 => {
                        let v = bus.ahb_read16(current & !1) as u32;
                        current = current.wrapping_add(2);
                        v
                    }
                    2 => {
                        let v = bus.ahb_read32(current & !3);
                        current = current.wrapping_add(4);
                        v
                    }
                    _ => 0,
                };
                c.burst.push_back(value);
            }
        }
        c.burst.pop_front().unwrap_or(0)
    }

    pub fn write16<B: Bus + ?Sized>(&mut self, bus: &mut B, channel: usize, addr: u32, v: u16) {
        self.write(bus, channel, addr, v as u32);
    }

    pub fn write32<B: Bus + ?Sized>(&mut self, bus: &mut B, channel: usize, addr: u32, v: u32) {
        // Tested on hardware: an odd address writes the high half.
        let v = if addr & 1 == 1 { v >> 16 } else { v };
        self.write(bus, channel, addr, v);
    }

    fn write<B: Bus + ?Sized>(&mut self, bus: &mut B, channel: usize, addr: u32, value: u32) {
        let c = &mut self.channels[channel];
        if c.burst.is_empty() {
            c.write_burst_start = addr;
        }
        c.burst.push_back(value);
        if c.burst.len() < c.burst_len() {
            return;
        }
        let mut current = c.write_burst_start;
        while let Some(v) = c.burst.pop_front() {
            match c.unit_size {
                0 => {
                    let byte = if current & 1 == 1 {
                        (v >> 8) as u8
                    } else {
                        v as u8
                    };
                    bus.ahb_write8(current, byte);
                    current = current.wrapping_add(1);
                }
                1 => {
                    let c0 = current & !1;
                    if c0 >= current {
                        bus.ahb_write16(c0, v as u16);
                    } else {
                        bus.ahb_write8(c0 + 1, (v >> 8) as u8);
                    }
                    current = current.wrapping_add(2);
                }
                2 => {
                    let c0 = current & !3;
                    let (c1, c2, c3) = (c0 + 1, c0 + 2, c0 + 3);
                    if c0 >= current && c1 >= current && c2 >= current {
                        bus.ahb_write32(c0, v);
                    } else if c2 >= current {
                        if c1 >= current {
                            bus.ahb_write8(c1, (v >> 8) as u8);
                        }
                        bus.ahb_write16(c2, (v >> 16) as u16);
                    } else {
                        bus.ahb_write8(c3, (v >> 24) as u8);
                    }
                    current = current.wrapping_add(4);
                }
                _ => {}
            }
        }
    }

    /// The AHBM channel a DMA channel goes through.
    pub fn channel_for_dma(&self, dma_channel: u16) -> usize {
        self.channels
            .iter()
            .position(|c| c.dma_channel >> (dma_channel & 15) & 1 != 0)
            .unwrap_or(0)
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct DmaChannel {
    pub addr_src_low: u16,
    pub addr_src_high: u16,
    pub addr_dst_low: u16,
    pub addr_dst_high: u16,
    pub size: [u16; 3],
    pub src_step: [u16; 3],
    pub dst_step: [u16; 3],
    /// 0 data memory, 1 MMIO, 7 the ARM's memory through AHBM.
    pub src_space: u16,
    pub dst_space: u16,
    pub dword_mode: u16,
    pub y: u16,
    pub z: u16,
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Dma {
    pub enable_channel: u16,
    active_channel: u16,
    pub channels: [DmaChannel; 8],
}

impl Dma {
    pub fn active_channel(&self) -> u16 {
        self.active_channel
    }

    pub fn set_active_channel(&mut self, value: u16) {
        self.active_channel = value & 7;
    }

    pub fn active(&mut self) -> &mut DmaChannel {
        &mut self.channels[self.active_channel as usize]
    }

    /// Run channel `index` to the end.
    pub fn run<B: Bus + ?Sized>(&mut self, index: usize, ahbm: &mut Ahbm, bus: &mut B) {
        let c = self.channels[index];
        let ahbm_channel = ahbm.channel_for_dma(index as u16);
        let mut src = c.addr_src_low as u32 | (c.addr_src_high as u32) << 16;
        let mut dst = c.addr_dst_low as u32 | (c.addr_dst_high as u32) << 16;
        let mut counter = [0u16; 3];
        loop {
            if c.dword_mode != 0 {
                let value = match c.src_space {
                    0 => {
                        let l = read_word(bus, DATA_MEMORY_OFFSET + (src & !1));
                        let h = read_word(bus, DATA_MEMORY_OFFSET + (src | 1));
                        l as u32 | (h as u32) << 16
                    }
                    7 => ahbm.read32(bus, ahbm_channel, src),
                    _ => 0,
                };
                match c.dst_space {
                    0 => {
                        write_word(bus, DATA_MEMORY_OFFSET + (dst & !1), value as u16);
                        write_word(bus, DATA_MEMORY_OFFSET + (dst | 1), (value >> 16) as u16);
                    }
                    7 => ahbm.write32(bus, ahbm_channel, dst, value),
                    _ => {}
                }
                counter[0] = counter[0].wrapping_add(2);
            } else {
                let value = match c.src_space {
                    0 => read_word(bus, DATA_MEMORY_OFFSET + src),
                    7 => ahbm.read16(bus, ahbm_channel, src),
                    _ => 0,
                };
                match c.dst_space {
                    0 => write_word(bus, DATA_MEMORY_OFFSET + dst, value),
                    7 => ahbm.write16(bus, ahbm_channel, dst, value),
                    _ => {}
                }
                counter[0] = counter[0].wrapping_add(1);
            }

            let level = if counter[0] < c.size[0] {
                0
            } else {
                counter[0] = 0;
                counter[1] += 1;
                if counter[1] < c.size[1] {
                    1
                } else {
                    counter[1] = 0;
                    counter[2] += 1;
                    if counter[2] >= c.size[2] {
                        return;
                    }
                    2
                }
            };
            src = src.wrapping_add(c.src_step[level] as u32);
            dst = dst.wrapping_add(c.dst_step[level] as u32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestBus {
        ram: Vec<u8>,
        fcram: Vec<u8>,
    }

    impl TestBus {
        fn fcram(&mut self, addr: u32) -> &mut u8 {
            let at = addr.checked_sub(0x2000_0000).expect("an FCRAM address");
            &mut self.fcram[at as usize]
        }
    }

    impl Bus for TestBus {
        fn ram(&mut self) -> &mut [u8] {
            &mut self.ram
        }
        fn ahb_read8(&mut self, addr: u32) -> u8 {
            *self.fcram(addr)
        }
        fn ahb_write8(&mut self, addr: u32, value: u8) {
            *self.fcram(addr) = value;
        }
    }

    /// Data memory from word 0 holds bytes 0x00 up, FCRAM 0x80 up; AHBM
    /// channel 0 serves DMA channel 0.
    fn env() -> (Dma, Ahbm, TestBus) {
        let mut bus = TestBus {
            ram: vec![0; 0x8_0000],
            fcram: (0x80..=0xFF).collect(),
        };
        for i in 0..0x80 {
            bus.ram[0x4_0000 + i] = i as u8;
        }
        let mut ahbm = Ahbm::default();
        ahbm.channels[0].dma_channel = 1;
        (Dma::default(), ahbm, bus)
    }

    /// 0x80 bytes: those the hex string gives, then `base + index` where
    /// it says `--` or has ended.
    fn expected(hex: &str, base: u8) -> Vec<u8> {
        let hex = hex.as_bytes();
        (0..0x80usize)
            .map(|i| match hex.get(i * 2..i * 2 + 2) {
                Some(b"--") | None => base.wrapping_add(i as u8),
                Some(pair) => u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap(),
            })
            .collect()
    }

    // Teakra's tests/dma.cpp: configurations and results from hardware.
    #[test]
    fn dma_through_ahbm_matches_hardware() {
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 0;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8000000082000000", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 0;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 1;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081000082830000", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 0;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081828380818283", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 1;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8283808182838485", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 2;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081828384858687", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 3;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8283848586878485", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 0;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("00810000", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 0;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8000000000810000", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 1;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("0081000082000000", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 2;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8200000000830000", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 0;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 1;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081000080810000", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 1;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 1;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081000082830000", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 0;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081828380818283", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 1;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081828380818283", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 2;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081828380818283", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 3;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081828384858687", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 0;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 2;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081828384858687", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("88898A8B8C8D8E8F", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("9091929394959697", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("98999A9B9C9D9E9F", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081828384858687", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 0;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 2;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081868788898E8F", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("9091969798999E9F", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081868788898E8F", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 0;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 1;
            ahbm.channels[0].burst_size = 2;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081000084850000", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("888900008C8D0000", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081000084850000", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 0;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 2;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 1;
            ahbm.channels[0].burst_size = 2;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081828384858687", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("88898A8B8C8D8E8F", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8081828384858687", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 1;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 1;
            ahbm.channels[0].burst_size = 2;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("0000828300008687", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("00008A8B00008E8F", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("0000828300008687", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x2000;
            dma.active().addr_dst_high = 0x0000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 7;
            dma.active().dst_space = 0;
            ahbm.channels[0].direction = 0;
            dma.active().addr_src_low = 0;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 2;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 2;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8000000000810000", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8200000000830000", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8400000000850000", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8600000000870000", 0));
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.ram[0x40000..0x40080], expected("8000000000810000", 0));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 1;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("2021222324252627", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("20--22--24--26--", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("20232427", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 3;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("20----23----24----27", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 4;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("20------22------24------26", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x11;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("22252629", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 1;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("--21222526", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 1;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("20232427", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 1;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 1;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("--2122252627", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 1;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 1;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("--21--23--25--27", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 3;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 1;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("2021--23----2425--27", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 3;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("20210000----0000--270000", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 4;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(
                bus.fcram,
                expected("20210000222300002425000026270000", 0x80)
            );
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("2021000024250000", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("20230000", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 5;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(
                bus.fcram,
                expected("20210000--230000----0000------00", 0x80)
            );
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 1;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 0;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("--21000026270000", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 1;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("2023", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 2;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("2027", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 2;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("20--24", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 2;
            dma.active().dst_step[0] = 3;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("20----27", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x11;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 2;
            dma.active().dst_step[0] = 3;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("20----27", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x12;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 2;
            dma.active().dst_step[0] = 3;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 0;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("24----2B", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 2;
            dma.active().dst_step[0] = 3;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 1;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("2021--27", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 2;
            dma.active().dst_step[0] = 2;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 1;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("20212425", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 2;
            dma.active().dst_step[0] = 1;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 1;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("2027", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().src_step[0] = 2;
            dma.active().dst_step[0] = 4;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 1;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("2021----2425", 0x80));
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().size[0] = 8;
            dma.active().src_step[0] = 2;
            dma.active().dst_step[0] = 4;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(
                bus.fcram,
                expected("202122232425262728292A2B2C2D2E2F", 0x80)
            );
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().size[0] = 8;
            dma.active().src_step[0] = 2;
            dma.active().dst_step[0] = 5;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 0;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(
                bus.fcram,
                expected("20212223--270000----2A2B------00", 0x80)
            );
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().size[0] = 16;
            dma.active().src_step[0] = 2;
            dma.active().dst_step[0] = 4;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 1;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(
                bus.fcram,
                expected(
                    "202122232425262728292A2B2C2D2E2F303132333435363738393A3B3C3D3E3F",
                    0x80
                )
            );
        }
        {
            let (mut dma, mut ahbm, mut bus) = env();
            dma.active().addr_src_high = 0x0000;
            dma.active().addr_dst_high = 0x2000;
            dma.active().size[0] = 4;
            dma.active().size[1] = 1;
            dma.active().size[2] = 1;
            dma.active().src_space = 0;
            dma.active().dst_space = 7;
            ahbm.channels[0].direction = 1;
            dma.active().addr_src_low = 0x10;
            dma.active().addr_dst_low = 0;
            dma.active().size[0] = 16;
            dma.active().src_step[0] = 2;
            dma.active().dst_step[0] = 8;
            dma.active().dword_mode = 1;
            ahbm.channels[0].unit_size = 2;
            ahbm.channels[0].burst_size = 1;
            dma.run(0, &mut ahbm, &mut bus);
            assert_eq!(bus.fcram, expected("202122232425262728292A2B2C2D2E2F--------------------------------303132333435363738393A3B3C3D3E3F", 0x80));
        }
    }
}
