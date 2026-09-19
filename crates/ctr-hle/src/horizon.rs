//! The high-level console: a process, the kernel that serves it, and the
//! clock they share.
//!
//! Time is counted in ARM11 cycles, as on the low-level machine. A thread
//! runs for a quantum, then the events that fell due are delivered; when no
//! thread can run, time jumps to the next event. Nothing depends on the host,
//! so a run repeats exactly.

use crate::memory::{
    AddressSpace, BusState, HleBus, Perm, CODE_BASE, LINEAR_BASE_OLD, PAGE, STACK_TOP, TLS_BASE,
};
use crate::svc::{self, Outcome};
use arm_core::{Arch, Cpu, HostTrap, HostTrapKind};
use ctr_core::bus::PhysMem;
use ctr_core::clock::{ARM11_HZ, FRAME_CYCLES};
use ctr_core::ctr::{BOTTOM_SCREEN, TOP_SCREEN};
use ctr_core::gpu::GpuExt;
use ctr_core::sched::Queue;
use ctr_fs::ThreeDsx;
use emu_core::video::Frame;
use emu_core::{Button, Screen, System};

/// ARM11 cycles a thread runs before due events are delivered. Coarser than
/// the low-level machine's, which has three processors to keep in step.
pub const QUANTUM: u64 = 1024;
/// The application's part of FCRAM on an Old 3DS in its default mode.
const APPLICATION_MEMORY: u32 = 64 << 20;
const MAIN_STACK_LEN: u32 = 0x4000;
/// Where the framebuffers lie in VRAM until a program sets its own: the
/// addresses a chainloader uses (see `ctr_core::boot`).
const DEFAULT_FRAMEBUFFERS: [u32; 2] = [0x1830_0000, 0x1834_6500];
/// Lines of log kept; a program that floods it loses the oldest.
const LOG_LINES: usize = 1000;

/// What the clock delivers.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum HleEvent {
    /// The display controllers reach the end of a frame.
    VBlank,
    /// The sleeping thread's time is up.
    Wake,
}

/// Whether the (so far only) thread can run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThreadState {
    Running,
    Sleeping,
    Exited,
}

/// The process's memory and the bookkeeping of what it has asked for.
pub struct Process {
    pub space: AddressSpace,
    /// Where the linear heap starts in the process, and how much of it the
    /// process has.
    pub linear_base: u32,
    pub linear_len: u32,
    /// How much ordinary heap is mapped from `HEAP_BASE`.
    pub heap_len: u32,
}

pub struct Horizon {
    pub mem: PhysMem,
    pub process: Process,
    pub cpu: Cpu,
    pub gpu: GpuExt,
    pub clock: Queue<HleEvent>,
    pub thread: ThreadState,
    bus_state: BusState,
    /// `HID_PAD` as the hardware has it: a clear bit is a pressed button.
    pad: u16,
    title: String,
    /// Debug output of the program and the kernel's own remarks.
    pub log: Vec<String>,
    /// Why the process was stopped, if the kernel had to stop it.
    pub fatal: Option<String>,
}

impl Horizon {
    /// A console running the 3DSX executable in `image`.
    pub fn from_3dsx(image: &[u8]) -> Result<Self, String> {
        let file = ThreeDsx::parse(image).map_err(|e| e.to_string())?;
        let placed = file.place(CODE_BASE).map_err(|e| e.to_string())?;

        let mut mem = PhysMem::new();
        let mut space = AddressSpace::new(APPLICATION_MEMORY);
        let out_of_memory = || "the program does not fit in memory".to_string();
        let perms = [Perm::RX, Perm::R, Perm::RW];
        for ((addr, bytes), perm) in placed.segments.iter().zip(perms) {
            if bytes.is_empty() {
                continue;
            }
            space
                .allocate(*addr, bytes.len() as u32, perm)
                .ok_or_else(out_of_memory)?;
            space
                .write_bytes(&mut mem, *addr, bytes)
                .ok_or_else(out_of_memory)?;
        }
        space
            .allocate(STACK_TOP - MAIN_STACK_LEN, MAIN_STACK_LEN, Perm::RW)
            .ok_or_else(out_of_memory)?;
        space
            .allocate(TLS_BASE, PAGE, Perm::RW)
            .ok_or_else(out_of_memory)?;

        let mut gpu = GpuExt::new();
        gpu.pdc[0].init_rgb8(DEFAULT_FRAMEBUFFERS[0], DEFAULT_FRAMEBUFFERS[0]);
        gpu.pdc[1].init_rgb8(DEFAULT_FRAMEBUFFERS[1], DEFAULT_FRAMEBUFFERS[1]);

        let mut clock = Queue::new();
        clock.schedule(FRAME_CYCLES as u64, HleEvent::VBlank);
        Ok(Horizon {
            mem,
            process: Process {
                space,
                linear_base: LINEAR_BASE_OLD,
                linear_len: 0,
                heap_len: 0,
            },
            cpu: Cpu::new_user(Arch::V6k, placed.entry, STACK_TOP),
            gpu,
            clock,
            thread: ThreadState::Running,
            bus_state: BusState::default(),
            pad: 0x0FFF,
            title: String::new(),
            log: Vec::new(),
            fatal: None,
        })
    }

    /// The console as a frontend drives it.
    pub fn system(image: Vec<u8>) -> Result<Box<dyn System>, String> {
        Ok(Box::new(Horizon::from_3dsx(&image)?))
    }

    pub fn note(&mut self, line: String) {
        if self.log.len() == LOG_LINES {
            self.log.remove(0);
        }
        self.log.push(line);
    }

    /// Stop the process for good, saying why.
    pub fn stop(&mut self, why: String) {
        self.note(format!("fatal: {why}"));
        self.fatal.get_or_insert(why);
        self.thread = ThreadState::Exited;
    }

    /// Put the thread to sleep for `nanoseconds` of emulated time.
    pub fn sleep(&mut self, nanoseconds: u64) {
        let cycles = (nanoseconds as u128 * ARM11_HZ as u128 / 1_000_000_000) as u64;
        self.thread = ThreadState::Sleeping;
        self.clock
            .schedule(self.clock.now() + cycles.max(1), HleEvent::Wake);
    }

    fn handle_trap(&mut self, trap: HostTrap) {
        match trap.kind {
            HostTrapKind::Supervisor(number) => match svc::call(self, number) {
                Outcome::Done => {}
                Outcome::Unknown => {
                    let regs: Vec<String> =
                        (0..4).map(|r| format!("{:#x}", self.cpu.reg(r))).collect();
                    self.stop(format!(
                        "supervisor call {number:#04x} at {:#010x} is not implemented (r0-r3: {})",
                        trap.pc,
                        regs.join(", ")
                    ));
                }
            },
            kind => {
                let fault = self.bus_state.fault_address;
                self.stop(format!(
                    "{kind:?} at {:#010x} (last faulting address {fault:#010x}, lr {:#010x})",
                    trap.pc,
                    self.cpu.reg(14)
                ));
            }
        }
    }

    /// Run the thread for a quantum, or let the time pass if it cannot run.
    fn quantum(&mut self) {
        let end = self.clock.now() + QUANTUM;
        while self.thread == ThreadState::Running && self.clock.now() < end {
            let mut bus = HleBus {
                space: &self.process.space,
                mem: &mut self.mem,
                state: &mut self.bus_state,
                core: 0,
                tls: TLS_BASE,
            };
            let cycles = self.cpu.step(&mut bus);
            self.clock.advance(cycles as u64);
            if let Some(trap) = self.cpu.take_trap() {
                self.handle_trap(trap);
            }
        }
        if self.clock.now() < end {
            self.clock.set_now(end);
        }
        while let Some((_, event)) = self.clock.pop_due() {
            match event {
                HleEvent::VBlank => {
                    self.gpu.vblank();
                    let next = self.clock.now() + FRAME_CYCLES as u64;
                    self.clock.schedule(next, HleEvent::VBlank);
                }
                HleEvent::Wake => {
                    if self.thread == ThreadState::Sleeping {
                        self.thread = ThreadState::Running;
                    }
                }
            }
        }
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
            Button::ZL | Button::ZR => return None,
        })
    }

    /// The buttons held, a set bit for each (the sense of `hid:USER`).
    pub fn pad_held(&self) -> u32 {
        (!self.pad & 0x0FFF) as u32
    }
}

impl System for Horizon {
    fn name(&self) -> &'static str {
        "3ds"
    }

    fn info(&self) -> String {
        "3DS homebrew (3DSX), high-level mode".to_string()
    }

    fn title(&self) -> String {
        self.title.clone()
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

    fn reset(&mut self) {}

    fn press(&mut self, button: Button) {
        if let Some(bit) = Horizon::pad_bit(button) {
            self.pad &= !bit;
        }
    }

    fn release(&mut self, button: Button) {
        if let Some(bit) = Horizon::pad_bit(button) {
            self.pad |= bit;
        }
    }

    fn step(&mut self) -> u32 {
        let start = self.clock.now();
        if self.thread != ThreadState::Running {
            // Nothing can run: go to the quantum that holds the next event,
            // but never further than a frame, so the frontend keeps its pace.
            let horizon = start + FRAME_CYCLES as u64;
            let wake = self
                .clock
                .next_due()
                .map_or(horizon, |due| due.min(horizon));
            self.clock
                .set_now(start + wake.saturating_sub(start) / QUANTUM * QUANTUM);
        }
        self.quantum();
        (self.clock.now() - start) as u32
    }

    fn frame_cycles(&self) -> u32 {
        FRAME_CYCLES
    }

    fn frame(&self) -> Frame {
        self.gpu.pdc[0].frame(&self.mem)
    }

    fn frame_at(&self, index: usize) -> Frame {
        self.gpu.pdc[index.min(1)].frame(&self.mem)
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
    use crate::memory::HEAP_BASE;
    use ctr_fs::threedsx::build;

    fn program(code: &[u32], tail: &[u8]) -> Vec<u8> {
        let mut bytes: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
        bytes.extend_from_slice(tail);
        build([&bytes, &[], &[]], 0, &Default::default())
    }

    /// Paints the first pixel of the top framebuffer, grows its heap, sleeps
    /// a millisecond, notes the time, says hello and exits.
    fn hello() -> Vec<u8> {
        program(
            &[
                0xE59F_0048, // ldr  r0, =0x1F300000   the top framebuffer
                0xE3A0_10FF, // mov  r1, #0xFF
                0xE5C0_1002, // strb r1, [r0, #2]      red is the third byte
                0xE3A0_0003, // mov  r0, #3            commit
                0xE3A0_1302, // mov  r1, #0x08000000
                0xE3A0_2000, // mov  r2, #0
                0xE3A0_3A01, // mov  r3, #0x1000
                0xE3A0_4003, // mov  r4, #3            read and write
                0xEF00_0001, // svc  0x01              ControlMemory
                0xE581_1000, // str  r1, [r1]
                0xE59F_0024, // ldr  r0, =1000000
                0xE3A0_1000, // mov  r1, #0
                0xEF00_000A, // svc  0x0A              SleepThread
                0xEF00_0028, // svc  0x28              GetSystemTick
                0xE3A0_2302, // mov  r2, #0x08000000
                0xE582_0004, // str  r0, [r2, #4]
                0xE28F_0010, // adr  r0, message
                0xE3A0_1005, // mov  r1, #5
                0xEF00_003D, // svc  0x3D              OutputDebugString
                0xEF00_0003, // svc  0x03              ExitProcess
                0x1F30_0000,
                1_000_000,
            ],
            b"hello",
        )
    }

    #[test]
    fn a_program_paints_allocates_sleeps_and_exits() {
        let mut console = Horizon::from_3dsx(&hello()).unwrap();
        console.run_frame();
        assert_eq!(console.fatal, None, "{:?}", console.log);
        assert_eq!(console.thread, ThreadState::Exited);
        assert_eq!(console.log, ["debug: hello", "the process exited"]);

        // The framebuffer runs up the first column, so its first pixel is
        // the bottom left one.
        let frame = console.frame();
        let rgb = frame.rgb.as_ref().unwrap();
        let at = 239 * 400 * 3;
        assert_eq!(rgb[at..at + 3], [0xFF, 0, 0]);
        assert_eq!(rgb.iter().filter(|&&b| b != 0).count(), 1);

        let space = &console.process.space;
        assert_eq!(space.read32(&console.mem, HEAP_BASE), Some(HEAP_BASE));
        // A millisecond is 268,111 cycles, and the thread wakes at the end
        // of the quantum that holds that moment.
        let tick = space.read32(&console.mem, HEAP_BASE + 4).unwrap() as u64;
        assert!((268_111..268_111 + 2 * QUANTUM).contains(&tick), "{tick}");
    }

    #[test]
    fn time_passes_at_frame_pace_once_the_process_is_gone() {
        let mut console = Horizon::from_3dsx(&hello()).unwrap();
        for _ in 0..3 {
            console.run_frame();
        }
        assert_eq!(console.clock.now() / FRAME_CYCLES as u64, 3);
        assert_eq!(console.screens().len(), 2);
    }

    #[test]
    fn what_the_kernel_cannot_serve_stops_the_process_with_a_report() {
        let mut console = Horizon::from_3dsx(&program(&[0xEF00_00FE], &[])).unwrap();
        console.run_frame();
        let why = console.fatal.clone().unwrap();
        assert!(why.contains("supervisor call 0xfe at 0x00100000"), "{why}");

        // A store to nowhere.
        let code = [0xE3A0_0000, 0xE580_0000]; // mov r0, #0 ; str r0, [r0]
        let mut console = Horizon::from_3dsx(&program(&code, &[])).unwrap();
        console.run_frame();
        let why = console.fatal.clone().unwrap();
        assert!(why.contains("DataAbort at 0x00100004"), "{why}");
    }

    #[test]
    fn buttons_reach_the_pad() {
        let mut console = Horizon::from_3dsx(&hello()).unwrap();
        console.press(Button::A);
        console.press(Button::Y);
        assert_eq!(console.pad_held(), 1 | 1 << 11);
        console.release(Button::A);
        assert_eq!(console.pad_held(), 1 << 11);
    }
}
