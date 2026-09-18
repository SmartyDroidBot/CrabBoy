//! What a processor needs from the machine around it.

/// A memory access the protection or translation unit refused. The machine
/// records the fault status and address itself; the processor only needs to
/// know that the access did not happen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Abort;

/// A coprocessor register, as addressed by `MRC` and `MCR`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CpReg {
    pub cp: u8,
    pub opc1: u8,
    pub crn: u8,
    pub crm: u8,
    pub opc2: u8,
}

/// What a coprocessor write asks of the processor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CpEffect {
    None,
    /// Stop executing until an interrupt is pending.
    WaitForInterrupt,
}

/// Memory, coprocessors and configuration as one processor sees them.
///
/// Addresses are virtual: the implementation applies the MPU or MMU.
/// `privileged` is false for user-mode accesses and for the `T` load and
/// store variants. Word and halfword addresses arrive already aligned when
/// the architecture aligns them.
pub trait Bus {
    fn fetch16(&mut self, addr: u32, privileged: bool) -> Result<u16, Abort>;
    fn fetch32(&mut self, addr: u32, privileged: bool) -> Result<u32, Abort>;

    fn read8(&mut self, addr: u32, privileged: bool) -> Result<u8, Abort>;
    fn read16(&mut self, addr: u32, privileged: bool) -> Result<u16, Abort>;
    fn read32(&mut self, addr: u32, privileged: bool) -> Result<u32, Abort>;

    fn write8(&mut self, addr: u32, value: u8, privileged: bool) -> Result<(), Abort>;
    fn write16(&mut self, addr: u32, value: u16, privileged: bool) -> Result<(), Abort>;
    fn write32(&mut self, addr: u32, value: u32, privileged: bool) -> Result<(), Abort>;

    /// Read a coprocessor register. `None` makes the instruction undefined.
    fn coproc_read(&mut self, _reg: CpReg, _privileged: bool) -> Option<u32> {
        None
    }

    /// Write a coprocessor register. `None` makes the instruction undefined.
    fn coproc_write(&mut self, _reg: CpReg, _value: u32, _privileged: bool) -> Option<CpEffect> {
        None
    }

    /// Whether exception vectors sit at 0xFFFF0000 (the `V` bit of the
    /// system control register) instead of 0.
    fn high_vectors(&self) -> bool {
        false
    }
}
