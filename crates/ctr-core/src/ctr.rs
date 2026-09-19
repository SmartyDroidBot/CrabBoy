//! The 3DS as a frontend sees it.

use crate::arm11::gic::CORES;
use crate::arm11::{Arm11, Arm11Bus};
use crate::arm9::{Arm9, Arm9Bus};
use crate::boot::{self, Entry};
use crate::bus::PhysMem;
use crate::clock::{ARM11_HZ, ARM9_CYCLE, FRAME_CYCLES, QUANTUM};
use crate::io::Io;
use crate::sched::Scheduler;
use arm_core::{Arch, Cpu};
use emu_core::{Button, Frame, Screen, System};

/// The upper display.
pub const TOP_SCREEN: Screen = Screen::new(400, 240);
/// The lower, touch-sensitive display.
pub const BOTTOM_SCREEN: Screen = Screen::new(320, 240);

/// A Nintendo 3DS.
///
/// Both ARM11 cores and the ARM9 run; the GPU's 3D engine and the DSP do not
/// exist yet. See `docs/3ds/overview.md` for the milestones.
pub struct Ctr {
    firm: Vec<u8>,
    entry: Entry,
    cpu9: Cpu,
    cpu11: [Cpu; CORES],
    /// ARM11 cycles each processor has run ahead of the quantum it was given.
    debt9: u64,
    debt11: [u64; CORES],
    arm9: Arm9,
    arm11: Arm11,
    mem: PhysMem,
    io: Io,
    sched: Scheduler,
}

impl Ctr {
    /// Build a console that launches `firm` directly, without boot ROMs.
    pub fn from_firm(firm: Vec<u8>) -> Result<Self, String> {
        let mut mem = PhysMem::new();
        let entry = boot::load_firm(&mut mem, &firm)?;
        let mut arm9 = Arm9::new();
        let mut arm11 = Arm11::new();
        let mut io = Io::new();
        let mut sched = Scheduler::new();
        let mut cpu9 = Cpu::new(
            Arch::V5te,
            &Arm9Bus {
                arm9: &mut arm9,
                mem: &mut mem,
                io: &mut io,
                sched: &mut sched,
            },
        );
        let mut core = |core| {
            Cpu::new(
                Arch::V6k,
                &Arm11Bus {
                    core,
                    arm11: &mut arm11,
                    mem: &mut mem,
                    io: &mut io,
                    sched: &mut sched,
                },
            )
        };
        let mut cpu11 = [core(0), core(1)];
        boot::hand_off(&mut arm9, &mut arm11, &mut io, &mut cpu9, &mut cpu11, entry);
        io.power_on(&mut sched);
        Ok(Ctr {
            firm,
            entry,
            cpu9,
            cpu11,
            debt9: 0,
            debt11: [0; CORES],
            arm9,
            arm11,
            mem,
            io,
            sched,
        })
    }

    /// [`Ctr::from_firm`] as a boxed [`System`].
    pub fn system(firm: Vec<u8>) -> Result<Box<dyn System>, String> {
        Ok(Box::new(Ctr::from_firm(firm)?))
    }

    pub fn entry(&self) -> Entry {
        self.entry
    }

    pub fn mem(&self) -> &PhysMem {
        &self.mem
    }

    pub fn cpu9(&self) -> &Cpu {
        &self.cpu9
    }

    pub fn cpu11(&self, core: usize) -> &Cpu {
        &self.cpu11[core]
    }

    pub fn io(&self) -> &Io {
        &self.io
    }

    pub fn io_mut(&mut self) -> &mut Io {
        &mut self.io
    }

    /// One line on where the processors are, for diagnostics.
    pub fn describe(&self) -> String {
        let one = |name: &str, cpu: &Cpu| {
            let [_, und, svc, pabt, dabt, irq, _] = cpu.exceptions_taken();
            format!(
                "{name} pc {:#010x} cpsr {:#010x}{} und {und} svc {svc} pabt {pabt} dabt {dabt} irq {irq}",
                cpu.reg(15),
                cpu.cpsr(),
                if cpu.halted() { " halted" } else { "" },
            )
        };
        format!(
            "{}\n    {}\n    {}",
            one("arm9   ", &self.cpu9),
            one("arm11/0", &self.cpu11[0]),
            one("arm11/1", &self.cpu11[1]),
        )
    }

    /// ARM11 cycles since power-on.
    pub fn cycles(&self) -> u64 {
        self.sched.now()
    }

    fn pad_bit(button: Button) -> Option<u16> {
        Some(match button {
            Button::A => 1 << 0,
            Button::B => 1 << 1,
            Button::Select => 1 << 2,
            Button::Start => 1 << 3,
            Button::Right => 1 << 4,
            Button::Left => 1 << 5,
            Button::Up => 1 << 6,
            Button::Down => 1 << 7,
            Button::R => 1 << 8,
            Button::L => 1 << 9,
            Button::X => 1 << 10,
            Button::Y => 1 << 11,
            // ZL and ZR are not on the pad register.
            Button::ZL | Button::ZR => return None,
        })
    }

    /// Run every processor through the quantum that starts now, in a fixed
    /// order, each on its own clock; then move time to the end of the quantum
    /// and fire what fell due. Effects between processors therefore become
    /// visible at quantum boundaries, identically on every host.
    fn quantum(&mut self) {
        let start = self.sched.now();
        let length = QUANTUM as u64;

        for core in 0..CORES {
            let mut used = self.debt11[core];
            while used < length {
                self.sched.set_now(start + used);
                let cpu = &mut self.cpu11[core];
                cpu.irq_line = self.io.mpcore.gic.irq_line(core);
                if cpu.halted() && !cpu.irq_line {
                    used = length;
                    break;
                }
                let mut bus = Arm11Bus {
                    core,
                    arm11: &mut self.arm11,
                    mem: &mut self.mem,
                    io: &mut self.io,
                    sched: &mut self.sched,
                };
                used += cpu.step(&mut bus) as u64;
            }
            self.debt11[core] = used - length;
        }

        let mut used = self.debt9;
        while used < length {
            self.sched.set_now(start + used);
            self.cpu9.irq_line = self.io.irq9.line();
            if self.cpu9.halted() && !self.cpu9.irq_line {
                used = length;
                break;
            }
            let mut bus = Arm9Bus {
                arm9: &mut self.arm9,
                mem: &mut self.mem,
                io: &mut self.io,
                sched: &mut self.sched,
            };
            used += (self.cpu9.step(&mut bus) * ARM9_CYCLE) as u64;
        }
        self.debt9 = used - length;

        self.sched.set_now(start + length);
        while let Some((at, event)) = self.sched.pop_due() {
            self.io.fire(at, event, &mut self.sched);
        }
    }

    /// Whether every processor is asleep with nothing to wake it yet.
    fn idle(&self) -> bool {
        self.cpu9.halted()
            && !self.io.irq9.line()
            && (0..CORES).all(|n| self.cpu11[n].halted() && !self.io.mpcore.gic.irq_line(n))
    }
}

impl System for Ctr {
    fn name(&self) -> &'static str {
        "3ds"
    }

    fn info(&self) -> String {
        format!(
            "3DS FIRM, ARM9 entry {:#010x}, ARM11 entry {:#010x} (no 3D engine or DSP yet)",
            self.entry.arm9, self.entry.arm11
        )
    }

    fn title(&self) -> String {
        String::new()
    }

    fn screen(&self) -> Screen {
        TOP_SCREEN
    }

    fn screens(&self) -> Vec<Screen> {
        vec![TOP_SCREEN, BOTTOM_SCREEN]
    }

    fn frame_rate(&self) -> f64 {
        ARM11_HZ as f64 / FRAME_CYCLES as f64
    }

    fn reset(&mut self) {
        *self = Ctr::from_firm(std::mem::take(&mut self.firm))
            .expect("the FIRM loaded when the console was built");
    }

    fn press(&mut self, button: Button) {
        if let Some(bit) = Ctr::pad_bit(button) {
            self.io.pad &= !bit;
        }
    }

    fn release(&mut self, button: Button) {
        if let Some(bit) = Ctr::pad_bit(button) {
            self.io.pad |= bit;
        }
    }

    fn step(&mut self) -> u32 {
        let start = self.sched.now();
        if self.idle() {
            // Sleep to the quantum that holds the next event, but never for
            // more than a frame, so the frontend keeps its cadence.
            let horizon = start + FRAME_CYCLES as u64;
            let wake = self
                .sched
                .next_due()
                .map_or(horizon, |due| due.min(horizon));
            let quanta = wake.saturating_sub(start) / QUANTUM as u64;
            self.sched.set_now(start + quanta * QUANTUM as u64);
        }
        self.quantum();
        (self.sched.now() - start) as u32
    }

    fn frame_cycles(&self) -> u32 {
        FRAME_CYCLES
    }

    fn frame(&self) -> Frame {
        self.io.pdc[0].frame(&self.mem)
    }

    fn frame_at(&self, index: usize) -> Frame {
        self.io.pdc[index.min(1)].frame(&self.mem)
    }

    fn audio_rate(&self) -> u32 {
        32_728
    }

    fn battery_backed(&self) -> bool {
        false
    }

    fn save_data(&self) -> Vec<u8> {
        Vec::new()
    }

    fn load_data(&mut self, _data: &[u8]) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctr_fs::firm::build;

    fn words(code: &[u32]) -> Vec<u8> {
        code.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// Paints the first pixel of the top framebuffer it was handed: blue
    /// always, red while A is held.
    fn pixel_payload() -> Vec<u8> {
        let code = words(&[
            0xE591_4004, // ldr   r4, [r1, #4]     argv[1]: the framebuffers
            0xE594_5000, // ldr   r5, [r4]         top left
            0xE59F_601C, // ldr   r6, =0x10146000  HID_PAD
            0xE1D6_70B0, // ldrh  r7, [r6]
            0xE317_0001, // tst   r7, #1           A is bit 0, low when held
            0x03A0_80FF, // moveq r8, #0xFF
            0x13A0_8000, // movne r8, #0
            0xE5C5_8002, // strb  r8, [r5, #2]     red is the third byte
            0xE3A0_9080, // mov   r9, #0x80
            0xE5C5_9000, // strb  r9, [r5]         blue is the first
            0xEAFF_FFF7, // b     back to the ldrh
            0x1014_6000,
        ]);
        build(0, 0x0800_6000, &[(0x0800_6000, &code)])
    }

    /// Counts timer 0 interrupts at 0x08001000 and sleeps in between.
    fn timer_payload() -> Vec<u8> {
        let main = words(&[
            0xE59F_0020, // ldr r0, =0x10001000   IRQ_IE
            0xE3A0_1C01, // mov r1, #0x100        timer 0
            0xE580_1000, // str r1, [r0]
            0xE59F_2018, // ldr r2, =0x10003000
            0xE59F_3018, // ldr r3, =0x00C0FF00   reload 0xFF00, start, interrupt
            0xE582_3000, // str r3, [r2]
            0xE321_F013, // msr cpsr_c, #0x13     unmask IRQ
            0xEE07_0F90, // mcr p15, 0, r0, c7, c0, 4   wait for interrupt
            0xEAFF_FFFD, // b   back to the wait
            0xE1A0_0000,
            0x1000_1000,
            0x1000_3000,
            0x00C0_FF00,
        ]);
        let handler = words(&[
            0xE59F_0018, // ldr  r0, =0x10001004  IRQ_IF
            0xE3A0_1C01, // mov  r1, #0x100
            0xE580_1000, // str  r1, [r0]         acknowledge
            0xE59F_0010, // ldr  r0, =0x08001000
            0xE590_1000, // ldr  r1, [r0]
            0xE281_1001, // add  r1, r1, #1
            0xE580_1000, // str  r1, [r0]
            0xE25E_F004, // subs pc, lr, #4
            0x1000_1004,
            0x0800_1000,
        ]);
        build(
            0,
            0x0800_6000,
            &[(0x0800_6000, &main), (0x0800_0000, &handler)],
        )
    }

    /// The ARM11 sends a word over PXI and sleeps; the ARM9 waits for it and
    /// stores it at 0x08001000.
    fn pxi_payload() -> Vec<u8> {
        let arm11 = words(&[
            0xE59F_0014, // ldr r0, =0x10163004   PXI_CNT
            0xE3A0_1902, // mov r1, #0x8000       enable
            0xE580_1000, // str r1, [r0]
            0xE59F_200C, // ldr r2, =0x1234
            0xE580_2004, // str r2, [r0, #4]      PXI_SEND
            0xE320_F003, // wfi
            0xEAFF_FFFD, // b   back to the wfi
            0x1016_3004,
            0x0000_1234,
        ]);
        let arm9 = words(&[
            0xE59F_0020, // ldr r0, =0x10008004   PXI_CNT
            0xE3A0_1902, // mov r1, #0x8000
            0xE580_1000, // str r1, [r0]
            0xE590_1000, // ldr r1, [r0]
            0xE311_0C01, // tst r1, #0x100        receive FIFO empty
            0x1AFF_FFFC, // bne back to the ldr
            0xE590_2008, // ldr r2, [r0, #8]      PXI_RECV
            0xE59F_300C, // ldr r3, =0x08001000
            0xE583_2000, // str r2, [r3]
            0xEAFF_FFFE, // b   .
            0x1000_8004,
            0,
            0x0800_1000,
        ]);
        build(
            0x1FF8_0000,
            0x0800_6000,
            &[(0x0800_6000, &arm9), (0x1FF8_0000, &arm11)],
        )
    }

    #[test]
    fn the_processors_talk_over_pxi() {
        let mut ctr = Ctr::from_firm(pxi_payload()).unwrap();
        ctr.run_frame();
        let word = ctr.mem().slice(0x0800_1000, 4).unwrap();
        assert_eq!(u32::from_le_bytes(word.try_into().unwrap()), 0x1234);
        assert!(ctr.cpu11(0).halted());
        assert!(ctr.cpu11(1).halted(), "the second core waits to be started");
        assert_eq!(ctr.cpu11(1).exceptions_taken(), [0; 7]);
    }

    const BOTTOM_LEFT: usize = 239 * 400 * 3;

    #[test]
    fn reports_both_screens_top_first() {
        let ctr = Ctr::from_firm(pixel_payload()).unwrap();
        assert_eq!(ctr.screen(), TOP_SCREEN);
        assert_eq!(ctr.screens(), [TOP_SCREEN, BOTTOM_SCREEN]);
        assert_eq!(ctr.frame().width, 400);
        assert_eq!(ctr.frame_at(1).width, 320);
        assert_eq!(ctr.frame_at(1).rgb.unwrap().len(), 320 * 240 * 3);
    }

    #[test]
    fn a_payload_draws_to_the_screen_and_reads_the_buttons() {
        let mut ctr = Ctr::from_firm(pixel_payload()).unwrap();
        ctr.run_frame();
        let rgb = ctr.frame().rgb.unwrap();
        assert_eq!(rgb[BOTTOM_LEFT..BOTTOM_LEFT + 3], [0x00, 0x00, 0x80]);
        assert!(rgb[..BOTTOM_LEFT].iter().all(|b| *b == 0));

        ctr.press(Button::A);
        ctr.run_frame();
        let rgb = ctr.frame().rgb.unwrap();
        assert_eq!(rgb[BOTTOM_LEFT..BOTTOM_LEFT + 3], [0xFF, 0x00, 0x80]);

        ctr.release(Button::A);
        ctr.run_frame();
        assert_eq!(ctr.frame().rgb.unwrap()[BOTTOM_LEFT], 0x00);
    }

    #[test]
    fn timer_interrupts_wake_the_processor_at_the_programmed_rate() {
        let count = |ctr: &Ctr| {
            let bytes = ctr.mem().slice(0x0800_1000, 4).unwrap();
            u32::from_le_bytes(bytes.try_into().unwrap())
        };
        let mut ctr = Ctr::from_firm(timer_payload()).unwrap();
        ctr.run_frame();
        // 0x100 counts of 4 cycles: an interrupt every 1024 cycles.
        let expected = FRAME_CYCLES / 1024;
        let got = count(&ctr);
        assert!(got.abs_diff(expected) <= 1, "{got} interrupts");
        assert!(ctr.cpu9().halted() || got > 0);

        let mut again = Ctr::from_firm(timer_payload()).unwrap();
        again.run_frame();
        assert_eq!(count(&again), got, "runs are reproducible");
        assert_eq!(again.cycles(), ctr.cycles());
    }

    #[test]
    fn a_frame_advances_whole_quanta_past_the_frame_length() {
        let mut ctr = Ctr::from_firm(pixel_payload()).unwrap();
        ctr.run_frame();
        assert!(ctr.cycles() >= FRAME_CYCLES as u64);
        assert!(ctr.cycles() < (FRAME_CYCLES + 2 * QUANTUM) as u64);
    }

    #[test]
    fn reset_reloads_the_firm_and_restarts_time() {
        let mut ctr = Ctr::from_firm(pixel_payload()).unwrap();
        ctr.run_frame();
        ctr.reset();
        assert_eq!(ctr.cycles(), 0);
        assert_eq!(ctr.cpu9().reg(15), 0x0800_6000);
        assert!(ctr.frame().rgb.unwrap().iter().all(|b| *b == 0));
    }

    #[test]
    fn rejects_an_image_that_is_not_a_firm() {
        assert!(Ctr::from_firm(vec![0; 0x200]).is_err());
    }
}
