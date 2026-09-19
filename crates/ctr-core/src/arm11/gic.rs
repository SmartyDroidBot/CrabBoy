//! The interrupt controller of the ARM11 MPCore: one distributor and a CPU
//! interface per core (ARM11 MPCore TRM, "Distributed Interrupt Controller").
//!
//! Interrupts 0-15 are software generated and 16-31 private to a core (29 is
//! its timer, 30 its watchdog); both kinds have state per core. The rest are
//! shared hardware lines, which the machine raises as pulses: a raised line
//! stays pending until a core acknowledges it.

/// Cores with an interface. The Old 3DS has two.
pub const CORES: usize = 2;
/// Interrupt lines the distributor implements.
pub const LINES: usize = 128;
const PRIVATE: usize = 32;

/// The private timer interrupt of each core.
pub const IRQ_TIMER: usize = 29;
/// The private watchdog interrupt of each core.
pub const IRQ_WATCHDOG: usize = 30;

/// No interrupt to acknowledge.
const SPURIOUS: u32 = 1023;
/// The priority field implements its top four bits.
const PRIORITY_MASK: u8 = 0xF0;
const IDLE_PRIORITY: u8 = 0xFF;

#[derive(Clone, Copy, Default)]
struct Line {
    enabled: bool,
    pending: bool,
    active: bool,
    priority: u8,
    /// Cores a shared interrupt goes to.
    targets: u8,
    /// For a software interrupt, the core that sent it.
    source: u8,
}

#[derive(Clone, Default)]
struct Interface {
    enabled: bool,
    priority_mask: u8,
    binary_point: u8,
    /// Acknowledged interrupts not yet ended, innermost last.
    running: Vec<(u16, u8)>,
}

pub struct Gic {
    enabled: bool,
    /// Software and private interrupts, per core.
    private: [[Line; PRIVATE]; CORES],
    shared: [Line; LINES - PRIVATE],
    interfaces: [Interface; CORES],
}

impl Default for Gic {
    fn default() -> Self {
        Self::new()
    }
}

impl Gic {
    pub fn new() -> Self {
        let mut gic = Gic {
            enabled: false,
            private: [[Line::default(); PRIVATE]; CORES],
            shared: [Line::default(); LINES - PRIVATE],
            interfaces: Default::default(),
        };
        // Software interrupts are always enabled.
        for core in gic.private.iter_mut() {
            for line in core[..16].iter_mut() {
                line.enabled = true;
            }
        }
        gic
    }

    fn line(&self, core: usize, irq: usize) -> &Line {
        if irq < PRIVATE {
            &self.private[core][irq]
        } else {
            &self.shared[irq - PRIVATE]
        }
    }

    fn line_mut(&mut self, core: usize, irq: usize) -> &mut Line {
        if irq < PRIVATE {
            &mut self.private[core][irq]
        } else {
            &mut self.shared[irq - PRIVATE]
        }
    }

    /// Pulse a shared hardware interrupt line.
    pub fn raise(&mut self, irq: usize) {
        debug_assert!((PRIVATE..LINES).contains(&irq));
        self.shared[irq - PRIVATE].pending = true;
    }

    /// Pulse a private interrupt of one core.
    pub fn raise_private(&mut self, core: usize, irq: usize) {
        debug_assert!((16..PRIVATE).contains(&irq));
        self.private[core][irq].pending = true;
    }

    /// The highest-priority interrupt `core` could take now, if any.
    fn best(&self, core: usize) -> Option<(usize, u8)> {
        let interface = &self.interfaces[core];
        if !self.enabled || !interface.enabled {
            return None;
        }
        let running = interface.running.last().map_or(IDLE_PRIORITY, |r| r.1);
        (0..LINES)
            .filter_map(|irq| {
                let line = self.line(core, irq);
                let routed = irq < PRIVATE || line.targets & 1 << core != 0;
                (line.pending && line.enabled && routed).then_some((irq, line.priority))
            })
            // Lower values are more urgent; ties go to the lower number.
            .min_by_key(|(irq, priority)| (*priority, *irq))
            .filter(|(_, priority)| *priority < interface.priority_mask && *priority < running)
    }

    /// Level of the IRQ input of `core`.
    pub fn irq_line(&self, core: usize) -> bool {
        self.best(core).is_some()
    }

    fn acknowledge(&mut self, core: usize) -> u32 {
        let Some((irq, priority)) = self.best(core) else {
            return SPURIOUS;
        };
        let line = self.line_mut(core, irq);
        line.pending = false;
        line.active = true;
        let source = line.source as u32;
        self.interfaces[core].running.push((irq as u16, priority));
        if irq < 16 {
            irq as u32 | source << 10
        } else {
            irq as u32
        }
    }

    fn end(&mut self, core: usize, value: u32) {
        let irq = (value & 0x3FF) as usize;
        if irq >= LINES {
            return;
        }
        self.line_mut(core, irq).active = false;
        let running = &mut self.interfaces[core].running;
        if let Some(at) = running.iter().rposition(|r| r.0 as usize == irq) {
            running.remove(at);
        }
    }

    /// Read a CPU interface register of `core`; `offset` is from 0x17E00100.
    pub fn read_interface(&mut self, core: usize, offset: u32) -> u32 {
        let interface = &self.interfaces[core];
        match offset {
            0x00 => interface.enabled as u32,
            0x04 => interface.priority_mask as u32,
            0x08 => interface.binary_point as u32,
            0x0C => self.acknowledge(core),
            0x14 => interface.running.last().map_or(IDLE_PRIORITY, |r| r.1) as u32,
            0x18 => self.best(core).map_or(SPURIOUS, |(irq, _)| irq as u32),
            _ => 0,
        }
    }

    pub fn write_interface(&mut self, core: usize, offset: u32, value: u32) {
        let interface = &mut self.interfaces[core];
        match offset {
            0x00 => interface.enabled = value & 1 != 0,
            0x04 => interface.priority_mask = value as u8 & PRIORITY_MASK,
            0x08 => interface.binary_point = value as u8 & 7,
            0x10 => self.end(core, value),
            _ => {}
        }
    }

    /// Read a distributor register as `core`; `offset` is from 0x17E01000.
    pub fn read_distributor(&self, core: usize, offset: u32) -> u32 {
        let bits = |f: fn(&Line) -> bool| {
            let first = (offset as usize & 0x7C) * 8;
            (0..32)
                .filter(|bit| first + bit < LINES && f(self.line(core, first + bit)))
                .fold(0, |word, bit| word | 1 << bit)
        };
        let bytes = |f: fn(&Line) -> u8| {
            let first = offset as usize & 0x3FC;
            (0..4)
                .filter(|byte| first + byte < LINES)
                .fold(0, |word, byte| {
                    word | (f(self.line(core, first + byte)) as u32) << (byte * 8)
                })
        };
        match offset {
            0x000 => self.enabled as u32,
            // Lines in units of 32, minus one, and cores minus one.
            0x004 => (LINES as u32 / 32 - 1) | (CORES as u32 - 1) << 5,
            0x100..=0x1FF => bits(|l| l.enabled),
            0x200..=0x2FF => bits(|l| l.pending),
            0x300..=0x37F => bits(|l| l.active),
            0x400..=0x7FF => bytes(|l| l.priority),
            0x800..=0xBFF => {
                let first = offset as usize & 0x3FC;
                if first < PRIVATE {
                    // Private interrupts read as targeting the reader.
                    0x0101_0101 << core
                } else {
                    bytes(|l| l.targets)
                }
            }
            _ => 0,
        }
    }

    pub fn write_distributor(&mut self, core: usize, offset: u32, value: u32) {
        let each_bit = |gic: &mut Gic, f: fn(&mut Line)| {
            let first = (offset as usize & 0x7C) * 8;
            for bit in (0..32).filter(|bit| value & 1 << bit != 0 && first + bit < LINES) {
                f(gic.line_mut(core, first + bit));
            }
        };
        match offset {
            0x000 => self.enabled = value & 1 != 0,
            0x100..=0x17F => each_bit(self, |l| l.enabled = true),
            0x180..=0x1FF => {
                each_bit(self, |l| l.enabled = false);
                // Software interrupts cannot be disabled.
                for line in self.private[core][..16].iter_mut() {
                    line.enabled = true;
                }
            }
            0x200..=0x27F => each_bit(self, |l| l.pending = true),
            0x280..=0x2FF => each_bit(self, |l| l.pending = false),
            0x400..=0x7FF => {
                let first = offset as usize & 0x3FC;
                for byte in (0..4).filter(|byte| first + byte < LINES) {
                    self.line_mut(core, first + byte).priority =
                        (value >> (byte * 8)) as u8 & PRIORITY_MASK;
                }
            }
            0x800..=0xBFF => {
                let first = offset as usize & 0x3FC;
                for byte in (0..4).filter(|byte| (PRIVATE..LINES).contains(&(first + byte))) {
                    self.line_mut(core, first + byte).targets =
                        (value >> (byte * 8)) as u8 & ((1 << CORES) - 1);
                }
            }
            0xF00 => {
                let irq = (value & 0xF) as usize;
                let targets = match value >> 24 & 3 {
                    0 => (value >> 16) as u8,
                    1 => !(1u8 << core),
                    2 => 1 << core,
                    _ => 0,
                };
                for target in (0..CORES).filter(|t| targets & 1 << t != 0) {
                    let line = &mut self.private[target][irq];
                    line.pending = true;
                    line.source = core as u8;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PXI: usize = 0x50;

    fn ready() -> Gic {
        let mut gic = Gic::new();
        gic.write_distributor(0, 0x000, 1);
        for core in 0..CORES {
            gic.write_interface(core, 0x00, 1);
            gic.write_interface(core, 0x04, 0xF0);
        }
        gic
    }

    fn enable(gic: &mut Gic, irq: usize, targets: u8, priority: u8) {
        let word = (irq / 32 * 4) as u32;
        gic.write_distributor(0, 0x100 + word, 1 << (irq % 32));
        let byte = (irq & !3) as u32;
        let shift = (irq % 4) * 8;
        let old = gic.read_distributor(0, 0x800 + byte);
        gic.write_distributor(0, 0x800 + byte, old | (targets as u32) << shift);
        let old = gic.read_distributor(0, 0x400 + byte);
        gic.write_distributor(0, 0x400 + byte, old | (priority as u32) << shift);
    }

    #[test]
    fn a_raised_line_reaches_only_its_targets_once_everything_is_enabled() {
        let mut gic = Gic::new();
        gic.raise(PXI);
        assert!(!gic.irq_line(0));
        let mut gic = ready();
        gic.raise(PXI);
        assert!(!gic.irq_line(0), "the line is not enabled");
        enable(&mut gic, PXI, 0b01, 0x40);
        assert!(gic.irq_line(0));
        assert!(!gic.irq_line(1));
    }

    #[test]
    fn acknowledge_and_end_of_interrupt() {
        let mut gic = ready();
        enable(&mut gic, PXI, 0b01, 0x40);
        gic.raise(PXI);
        assert_eq!(gic.read_interface(0, 0x18), PXI as u32);
        assert_eq!(gic.read_interface(0, 0x0C), PXI as u32);
        assert!(!gic.irq_line(0));
        assert_eq!(gic.read_interface(0, 0x14), 0x40);
        assert_eq!(gic.read_interface(0, 0x0C), SPURIOUS);
        assert_ne!(gic.read_distributor(0, 0x300 + 8) & 1 << (PXI % 32), 0);
        gic.write_interface(0, 0x10, PXI as u32);
        assert_eq!(gic.read_interface(0, 0x14), 0xFF);
        assert_eq!(gic.read_distributor(0, 0x300 + 8), 0);
    }

    #[test]
    fn only_a_more_urgent_interrupt_preempts() {
        let mut gic = ready();
        enable(&mut gic, 0x50, 0b01, 0x40);
        enable(&mut gic, 0x51, 0b01, 0x40);
        enable(&mut gic, 0x52, 0b01, 0x20);
        gic.raise(0x50);
        gic.read_interface(0, 0x0C);
        gic.raise(0x51);
        assert!(!gic.irq_line(0), "same priority does not preempt");
        gic.raise(0x52);
        assert!(gic.irq_line(0));
        assert_eq!(gic.read_interface(0, 0x0C), 0x52);
        gic.write_interface(0, 0x10, 0x52);
        gic.write_interface(0, 0x10, 0x50);
        assert_eq!(gic.read_interface(0, 0x0C), 0x51);
    }

    #[test]
    fn the_priority_mask_holds_interrupts_back() {
        let mut gic = ready();
        enable(&mut gic, PXI, 0b01, 0x80);
        gic.raise(PXI);
        gic.write_interface(0, 0x04, 0x80);
        assert!(!gic.irq_line(0));
        gic.write_interface(0, 0x04, 0x90);
        assert!(gic.irq_line(0));
    }

    #[test]
    fn software_interrupts_carry_their_sender() {
        let mut gic = ready();
        // Core 1 sends interrupt 3 to everyone else.
        gic.write_distributor(1, 0xF00, 1 << 24 | 3);
        assert!(gic.irq_line(0));
        assert!(!gic.irq_line(1));
        assert_eq!(gic.read_interface(0, 0x0C), 3 | 1 << 10);
        // To a list, then to itself.
        gic.write_distributor(0, 0xF00, 0b10 << 16 | 5);
        assert!(gic.irq_line(1));
        gic.write_distributor(1, 0xF00, 2 << 24 | 6);
        assert_eq!(gic.read_interface(1, 0x0C), 5);
    }

    #[test]
    fn private_interrupts_are_per_core() {
        let mut gic = ready();
        gic.write_distributor(1, 0x100, 1 << IRQ_TIMER);
        gic.raise_private(0, IRQ_TIMER);
        gic.raise_private(1, IRQ_TIMER);
        assert!(
            !gic.irq_line(0),
            "core 0 has not enabled its timer interrupt"
        );
        assert!(gic.irq_line(1));
        assert_eq!(gic.read_interface(1, 0x0C), IRQ_TIMER as u32);
    }

    #[test]
    fn the_type_register_describes_the_controller() {
        let gic = Gic::new();
        assert_eq!(gic.read_distributor(0, 0x004), 3 | 1 << 5);
    }
}
