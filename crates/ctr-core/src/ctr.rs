//! The 3DS as a frontend sees it.

use crate::boot::{self, Entry};
use crate::bus::PhysMem;
use crate::clock::{ARM11_HZ, FRAME_CYCLES, QUANTUM};
use emu_core::{Frame, Screen, System};

/// The upper display.
pub const TOP_SCREEN: Screen = Screen::new(400, 240);
/// The lower, touch-sensitive display.
pub const BOTTOM_SCREEN: Screen = Screen::new(320, 240);

/// A Nintendo 3DS.
///
/// The processors are not emulated yet: the machine loads a FIRM, keeps time
/// and shows blank screens. See `docs/3ds/overview.md` for the milestones.
pub struct Ctr {
    firm: Vec<u8>,
    mem: PhysMem,
    entry: Entry,
    /// ARM11 cycles since power-on.
    cycles: u64,
}

impl Ctr {
    /// Build a console that launches `firm` directly, without boot ROMs.
    pub fn from_firm(firm: Vec<u8>) -> Result<Self, String> {
        let mut mem = PhysMem::new();
        let entry = boot::load_firm(&mut mem, &firm)?;
        Ok(Ctr {
            firm,
            mem,
            entry,
            cycles: 0,
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

    /// ARM11 cycles since power-on.
    pub fn cycles(&self) -> u64 {
        self.cycles
    }

    fn blank(screen: Screen) -> Frame {
        let mut frame = Frame::new(screen.width, screen.height);
        frame.rgb = Some(vec![0; screen.pixels() * 3]);
        frame
    }
}

impl System for Ctr {
    fn name(&self) -> &'static str {
        "3ds"
    }

    fn info(&self) -> String {
        format!(
            "3DS FIRM, ARM9 entry {:#010x}, ARM11 entry {:#010x} (processors not emulated yet)",
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
        self.mem = PhysMem::new();
        self.entry = boot::load_firm(&mut self.mem, &self.firm)
            .expect("the FIRM loaded when the console was built");
        self.cycles = 0;
    }

    fn press(&mut self, _button: emu_core::Button) {}

    fn release(&mut self, _button: emu_core::Button) {}

    fn step(&mut self) -> u32 {
        self.cycles += QUANTUM as u64;
        QUANTUM
    }

    fn frame_cycles(&self) -> u32 {
        FRAME_CYCLES
    }

    fn frame(&self) -> Frame {
        Ctr::blank(TOP_SCREEN)
    }

    fn frame_at(&self, index: usize) -> Frame {
        match index {
            1 => Ctr::blank(BOTTOM_SCREEN),
            _ => self.frame(),
        }
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

    fn console() -> Ctr {
        Ctr::from_firm(build(0, 0x0800_6000, &[(0x0800_6000, &[0xAA; 8])])).unwrap()
    }

    #[test]
    fn reports_both_screens_top_first() {
        let ctr = console();
        assert_eq!(ctr.screen(), TOP_SCREEN);
        assert_eq!(ctr.screens(), [TOP_SCREEN, BOTTOM_SCREEN]);
        assert_eq!(ctr.frame().width, 400);
        assert_eq!(ctr.frame_at(1).width, 320);
        assert_eq!(ctr.frame_at(1).rgb.unwrap().len(), 320 * 240 * 3);
        assert_eq!(ctr.frame_at(7).width, 400);
    }

    #[test]
    fn a_frame_advances_whole_quanta_past_the_frame_length() {
        let mut ctr = console();
        ctr.run_frame();
        let cycles = ctr.cycles();
        assert!(cycles >= FRAME_CYCLES as u64);
        assert!(cycles < (FRAME_CYCLES + QUANTUM) as u64);
        assert_eq!(cycles % QUANTUM as u64, 0);
    }

    #[test]
    fn reset_reloads_the_firm_and_restarts_time() {
        let mut ctr = console();
        ctr.run_frame();
        ctr.reset();
        assert_eq!(ctr.cycles(), 0);
        assert_eq!(ctr.mem().slice(0x0800_6000, 8), Some(&[0xAA; 8][..]));
    }

    #[test]
    fn rejects_an_image_that_is_not_a_firm() {
        assert!(Ctr::from_firm(vec![0; 0x200]).is_err());
    }
}
