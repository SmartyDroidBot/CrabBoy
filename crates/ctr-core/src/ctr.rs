//! The 3DS as a frontend sees it.

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
/// The ARM9 runs; the ARM11, the GPU and the DSP do not exist yet. See
/// `docs/3ds/overview.md` for the milestones.
pub struct Ctr {
    firm: Vec<u8>,
    entry: Entry,
    cpu9: Cpu,
    arm9: Arm9,
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
        boot::hand_off(&mut arm9, &mut io, &mut cpu9, entry);
        Ok(Ctr {
            firm,
            entry,
            cpu9,
            arm9,
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

    /// Run the ARM9 for one instruction, or skip ahead while it waits for an
    /// interrupt, never past `limit`. Due events fire afterwards.
    fn advance(&mut self, limit: u64) {
        self.cpu9.irq_line = self.io.irq9.line();
        let cycles = if self.cpu9.halted() && !self.cpu9.irq_line {
            let wake = self.sched.next_due().map_or(limit, |due| due.min(limit));
            wake.saturating_sub(self.sched.now()).max(1)
        } else {
            let mut bus = Arm9Bus {
                arm9: &mut self.arm9,
                mem: &mut self.mem,
                io: &mut self.io,
                sched: &mut self.sched,
            };
            (self.cpu9.step(&mut bus) * ARM9_CYCLE) as u64
        };
        self.sched.advance(cycles);
        while let Some((at, event)) = self.sched.pop_due() {
            self.io.fire(at, event, &mut self.sched);
        }
    }
}

impl System for Ctr {
    fn name(&self) -> &'static str {
        "3ds"
    }

    fn info(&self) -> String {
        format!(
            "3DS FIRM, ARM9 entry {:#010x}, ARM11 entry {:#010x} (ARM9 only; no ARM11, GPU or DSP yet)",
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
        let limit = start + QUANTUM as u64;
        while self.sched.now() < limit {
            self.advance(limit);
        }
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
