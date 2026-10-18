//! The memory the GPU reads and writes: vertex arrays, index buffers,
//! textures, command lists, and the colour and depth buffers. Everything is
//! addressed by physical address; the machine decides what lies there.

/// Byte access to physical memory. A range that does not lie wholly in one
/// region of memory is `None`, and the GPU skips the access.
pub trait Memory {
    fn read(&self, pa: u32, len: usize) -> Option<&[u8]>;
    fn write(&mut self, pa: u32, len: usize) -> Option<&mut [u8]>;
}

/// One region of memory at `base`: the stand-in for VRAM and FCRAM in tests
/// and tools.
#[derive(Clone, Debug, Default)]
pub struct VecMemory {
    pub base: u32,
    pub bytes: Vec<u8>,
}

impl VecMemory {
    pub fn new(base: u32, len: usize) -> Self {
        VecMemory {
            base,
            bytes: vec![0; len],
        }
    }

    fn range(&self, pa: u32, len: usize) -> Option<std::ops::Range<usize>> {
        let start = pa.checked_sub(self.base)? as usize;
        let end = start.checked_add(len)?;
        (end <= self.bytes.len()).then_some(start..end)
    }
}

impl Memory for VecMemory {
    fn read(&self, pa: u32, len: usize) -> Option<&[u8]> {
        let range = self.range(pa, len)?;
        Some(&self.bytes[range])
    }

    fn write(&mut self, pa: u32, len: usize) -> Option<&mut [u8]> {
        let range = self.range(pa, len)?;
        Some(&mut self.bytes[range])
    }
}

/// The little-endian word at `pa`.
pub fn read_u32(mem: &dyn Memory, pa: u32) -> Option<u32> {
    mem.read(pa, 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_region_answers_only_inside_itself() {
        let mut mem = VecMemory::new(0x1800_0000, 16);
        mem.write(0x1800_0004, 4)
            .unwrap()
            .copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(read_u32(&mem, 0x1800_0004), Some(0x0403_0201));
        assert!(mem.read(0x1800_000D, 4).is_none());
        assert!(mem.read(0x17FF_FFFF, 1).is_none());
        assert!(mem.read(0xFFFF_FFFF, 2).is_none());
    }
}
