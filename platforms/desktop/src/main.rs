use eframe::egui;
use emu_core::{Button, System};
use rodio::{buffer::SamplesBuffer, OutputStream, Sink};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

// Default window size at 3x the GBA display (240x160); GB games (160x144)
// scale to fit inside it.
const WIN_W: f32 = 240.0 * 3.0;
const WIN_H: f32 = 160.0 * 3.0;
const MIN_W: f32 = 240.0 * 2.0;
const MIN_H: f32 = 144.0 * 2.0;

/// The window icon (the crab-and-console crop of the project logo).
const APP_ICON: &[u8] = include_bytes!("../../../assets/icon-256.png");

fn sav_path_for(rom: &str) -> Option<String> {
    let p = std::path::Path::new(rom);
    p.extension()?;
    let stem = p.with_extension("");
    Some(format!("{}.sav", stem.to_string_lossy()))
}

/// Derive the `.rtc` path from a `.sav` path (e.g. `game.sav` -> `game.rtc`).
fn rtc_path_for(sav: &str) -> Option<String> {
    let p = std::path::Path::new(sav);
    if p.extension()? != "sav" {
        return None;
    }
    let stem = p.with_extension("");
    Some(format!("{}.rtc", stem.to_string_lossy()))
}

/// Derive a save-state slot path from a `.sav` path. `suffix` is e.g.
/// `".state0"` or `".stateq"` (quicksave), yielding `game.state0` / `game.stateq`.
fn state_path_for(sav: &str, suffix: &str) -> Option<String> {
    let p = std::path::Path::new(sav);
    if p.extension()? != "sav" {
        return None;
    }
    let stem = p.with_extension("");
    Some(format!("{}{suffix}", stem.to_string_lossy()))
}

/// File suffixes for the four state slots plus the quicksave slot.
const STATE_SLOTS: [&str; 5] = [".state0", ".state1", ".state2", ".state3", ".stateq"];

/// Locate a GBA BIOS image: an explicit `--bios` path, else `gba_bios.bin`
/// next to the ROM, else next to the executable. Returns `None` when no
/// image is found; the GBA then boots with high-level BIOS emulation.
fn find_gba_bios(explicit: Option<&str>, rom_path: &str) -> Option<Vec<u8>> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(p) = explicit {
        candidates.push(p.into());
    }
    if let Some(dir) = std::path::Path::new(rom_path).parent() {
        candidates.push(dir.join("gba_bios.bin"));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.to_owned()))
    {
        candidates.push(dir.join("gba_bios.bin"));
    }
    candidates.into_iter().find_map(|p| std::fs::read(p).ok())
}

#[derive(Clone, Copy)]
struct Palette {
    colors: [[u8; 4]; 4],
}

const PALETTES: [(&str, Palette); 4] = [
    (
        "Original (grey-green)",
        Palette {
            colors: [
                [0xE0, 0xF8, 0xD0, 0xFF],
                [0x88, 0xC0, 0x70, 0xFF],
                [0x34, 0x68, 0x56, 0xFF],
                [0x08, 0x18, 0x20, 0xFF],
            ],
        },
    ),
    (
        "Pocket (blue)",
        Palette {
            colors: [
                [0xE8, 0xF0, 0xE0, 0xFF],
                [0x70, 0xA8, 0xC8, 0xFF],
                [0x28, 0x48, 0x88, 0xFF],
                [0x10, 0x20, 0x40, 0xFF],
            ],
        },
    ),
    (
        "Light (yellow-green)",
        Palette {
            colors: [
                [0xF0, 0xE8, 0xA0, 0xFF],
                [0x98, 0xC0, 0x50, 0xFF],
                [0x30, 0x68, 0x30, 0xFF],
                [0x10, 0x28, 0x10, 0xFF],
            ],
        },
    ),
    (
        "Grayscale",
        Palette {
            colors: [
                [0xFF, 0xFF, 0xFF, 0xFF],
                [0xAA, 0xAA, 0xAA, 0xFF],
                [0x55, 0x55, 0x55, 0xFF],
                [0, 0, 0, 0xFF],
            ],
        },
    ),
];

struct CrabBoyApp {
    system: Option<Box<dyn System>>,
    sav_path: Option<String>,
    bios_path: Option<String>,
    keymap: HashMap<Button, egui::Key>,
    prev_keys: HashSet<egui::Key>,
    paused: bool,
    fast_forward: bool,
    palette: Palette,
    palette_name: String,
    screen_texture: Option<egui::TextureHandle>,
    fps: f64,
    frame_count: u64,
    accum: f64,
    last_frame: Instant,
    last_fps: Instant,
    fps_frames: u64,
    status: String,
    rom_title: String,
    last_title: String,
    audio: Option<(OutputStream, Sink)>,
    audio_rate: u32,
}

impl CrabBoyApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        rom_path: Option<String>,
        bios_path: Option<String>,
        audio: Option<(OutputStream, Sink)>,
    ) -> Self {
        let mut keymap = HashMap::new();
        for b in Button::ALL {
            if let Some(k) = default_key(b) {
                keymap.insert(b, k);
            }
        }
        let mut app = CrabBoyApp {
            system: None,
            sav_path: None,
            bios_path,
            keymap,
            prev_keys: HashSet::new(),
            paused: false,
            fast_forward: false,
            palette: PALETTES[0].1,
            palette_name: PALETTES[0].0.to_string(),
            screen_texture: None,
            fps: 0.0,
            frame_count: 0,
            accum: 0.0,
            last_frame: Instant::now(),
            last_fps: Instant::now(),
            fps_frames: 0,
            status: "No ROM loaded".to_string(),
            rom_title: String::new(),
            last_title: String::new(),
            audio,
            audio_rate: 8192,
        };
        if let Some(path) = rom_path {
            app.load_rom(&path, &cc.egui_ctx);
        }
        app
    }

    fn load_rom(&mut self, path: &str, ctx: &egui::Context) {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                self.status = format!("Failed to read ROM '{}': {e}", path);
                return;
            }
        };
        let kind = match crab_systems::detect(&data) {
            Some(k) => k,
            None => {
                self.status = format!("'{}' is not a Game Boy or GBA ROM", path);
                return;
            }
        };
        let opts = crab_systems::LoadOptions {
            gba_bios: if kind == crab_systems::Kind::Gba {
                find_gba_bios(self.bios_path.as_deref(), path)
            } else {
                None
            },
            cold: false,
        };
        let bios_note = if opts.gba_bios.is_some() {
            ", BIOS boot"
        } else {
            ""
        };
        let mut system = match crab_systems::load_with(data, &opts) {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("Failed to load cartridge: {e}");
                return;
            }
        };
        let sav_path = sav_path_for(path);
        self.rom_title = system.title();
        if self.rom_title.is_empty() {
            self.rom_title = kind.name().to_ascii_uppercase();
        }
        self.status = format!("Loaded '{}'{bios_note}", system.info());
        if system.battery_backed() {
            if let Some(sav) = &sav_path {
                if let Ok(d) = std::fs::read(sav) {
                    system.load_data(&d);
                    self.status =
                        format!("Loaded '{}'{bios_note} (battery save found)", system.info());
                }
                if let Some(rtc) = rtc_path_for(sav) {
                    if let Ok(d) = std::fs::read(&rtc) {
                        system.load_rtc(&d);
                    }
                }
            }
        }

        self.system = Some(system);
        self.sav_path = sav_path;
        self.frame_count = 0;
        self.accum = 0.0;
        self.paused = false;
        ctx.request_repaint();
    }

    fn reset(&mut self) {
        if let Some(system) = &mut self.system {
            system.reset();
            self.frame_count = 0;
            self.accum = 0.0;
            self.paused = false;
        }
    }

    /// Write battery SRAM and (if present) the MBC3 RTC to disk. Called only
    /// when `sram_changed()` reports the game wrote its save data, plus on exit
    /// and reset, so `.sav`/`.rtc` reflect the game's own saves rather than a
    /// wall-clock timer.
    fn flush_save(&mut self) {
        let Some(system) = &mut self.system else {
            return;
        };
        if !system.battery_backed() {
            return;
        }
        let data = system.save_data();
        if let Some(sav) = &self.sav_path {
            if !data.is_empty() {
                if let Err(e) = std::fs::write(sav, &data) {
                    eprintln!("failed to write save {sav}: {e}");
                }
            }
        }
        let rtc = system.rtc_data();
        if !rtc.is_empty() {
            if let Some(p) = self.sav_path.as_ref().and_then(|s| rtc_path_for(s)) {
                if let Err(e) = std::fs::write(&p, &rtc) {
                    eprintln!("failed to write RTC {p}: {e}");
                }
            }
        }
    }

    fn save_slot(&mut self, suffix: &str) {
        let (Some(system), Some(sav)) = (&self.system, &self.sav_path) else {
            self.status = "No ROM loaded".to_string();
            return;
        };
        let data = system.save_state();
        if data.is_empty() {
            self.status = "Save states not supported for this system".to_string();
            return;
        }
        if let Some(path) = state_path_for(sav, suffix) {
            match std::fs::write(&path, &data) {
                Ok(_) => self.status = format!("Saved state {suffix}"),
                Err(e) => self.status = format!("Save failed: {e}"),
            }
        }
    }

    fn load_slot(&mut self, suffix: &str) {
        let (Some(system), Some(sav)) = (&mut self.system, &self.sav_path) else {
            self.status = "No ROM loaded".to_string();
            return;
        };
        let Some(path) = state_path_for(sav, suffix) else {
            return;
        };
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(_) => {
                self.status = format!("No state in slot {suffix}");
                return;
            }
        };
        match system.load_state(&data) {
            Ok(_) => {
                self.status = format!("Loaded state {suffix}");
                // Loading restores SRAM/RTC; refresh the .sav/.rtc files to match.
                self.flush_save();
            }
            Err(e) => self.status = format!("Load failed: {e}"),
        }
    }

    fn run_frame(&mut self) {
        let dirty = if let Some(system) = &mut self.system {
            system.run_frame();
            self.frame_count += 1;
            let audio = system.take_audio();
            if !audio.samples.is_empty() {
                let rate = system.audio_rate();
                if rate != self.audio_rate {
                    // Sample rate changed (e.g. CGB double-speed toggle); drop
                    // the old sink and open a fresh one at the new rate.
                    self.audio.take();
                    self.audio =
                        rodio::OutputStream::try_default()
                            .ok()
                            .and_then(|(stream, handle)| {
                                Sink::try_new(&handle).ok().map(|sink| (stream, sink))
                            });
                    self.audio_rate = rate;
                }
                if let Some((_, sink)) = &self.audio {
                    let src = SamplesBuffer::new(2, self.audio_rate, audio.samples);
                    sink.append(src);
                }
            }
            system.sram_changed()
        } else {
            false
        };
        if dirty {
            self.flush_save();
        }
    }

    fn advance(&mut self, ctx: &egui::Context, dt: f64) {
        if self.paused || self.system.is_none() {
            return;
        }
        if self.fast_forward {
            let n = (dt * 240.0).floor() as u32;
            let n = n.clamp(1, 16);
            for _ in 0..n {
                self.run_frame();
            }
            ctx.request_repaint();
        } else {
            let period = 1.0 / self.system.as_ref().map_or(60.0, |s| s.frame_rate());
            self.accum += dt;
            while self.accum >= period {
                self.accum -= period;
                self.run_frame();
            }
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                (period - self.accum).max(0.001),
            ));
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = if self.rom_title.is_empty() {
            "CrabBoy".to_string()
        } else {
            format!(
                "CrabBoy \u{2014} {} \u{2014} {:.0} FPS \u{2014} Frame {}",
                self.rom_title, self.fps, self.frame_count
            )
        };
        if title != self.last_title {
            self.last_title = title.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let raw: Vec<egui::Event> = ctx.input(|i| i.raw.events.clone());
        let mut keys: HashSet<egui::Key> = self.prev_keys.clone();
        let mut newly: Vec<egui::Key> = Vec::new();
        for e in raw {
            match e {
                egui::Event::Key {
                    physical_key: Some(k),
                    pressed,
                    ..
                } => {
                    if pressed {
                        if !keys.contains(&k) {
                            newly.push(k);
                        }
                        keys.insert(k);
                    } else {
                        keys.remove(&k);
                    }
                }
                egui::Event::WindowFocused(false) => {
                    keys.clear();
                }
                _ => {}
            }
        }

        if let Some(system) = &mut self.system {
            for (&b, k) in &self.keymap {
                if keys.contains(k) {
                    system.press(b);
                } else {
                    system.release(b);
                }
            }
        }

        let shift = ctx.input(|i| i.modifiers.shift);
        for &k in &newly {
            match k {
                egui::Key::P => self.paused = !self.paused,
                egui::Key::R => self.reset(),
                egui::Key::F => self.fast_forward = !self.fast_forward,
                egui::Key::F1 => self.state_key(0, shift),
                egui::Key::F2 => self.state_key(1, shift),
                egui::Key::F3 => self.state_key(2, shift),
                egui::Key::F4 => self.state_key(3, shift),
                egui::Key::F5 => self.save_slot(".stateq"),
                egui::Key::F9 => self.load_slot(".stateq"),
                _ => {}
            }
        }

        self.prev_keys = keys;
    }

    fn shade_image(&self, shades: &[u8], w: usize, h: usize) -> egui::ColorImage {
        let mut img = egui::ColorImage::new([w, h], egui::Color32::BLACK);
        for (i, &shade) in shades.iter().enumerate() {
            let c = self.palette.colors[(shade & 3) as usize];
            img.pixels[i] = egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
        }
        img
    }

    fn rgb_image(&self, rgb: &[u8], w: usize, h: usize) -> egui::ColorImage {
        let mut img = egui::ColorImage::new([w, h], egui::Color32::BLACK);
        for (i, px) in rgb.as_chunks::<3>().0.iter().enumerate() {
            img.pixels[i] = egui::Color32::from_rgb(px[0], px[1], px[2]);
        }
        img
    }

    fn draw_screen(&mut self, ui: &mut egui::Ui) {
        let Some(system) = &self.system else {
            ui.label("No ROM loaded. Use the file picker or drag & drop a .gb/.gbc/.gba file.");
            return;
        };
        let frame = system.frame();
        let w = frame.width as usize;
        let h = frame.height as usize;
        let img = match &frame.rgb {
            Some(rgb) => self.rgb_image(rgb, w, h),
            None => self.shade_image(&frame.shades, w, h),
        };
        let tex = self.screen_texture.get_or_insert_with(|| {
            ui.ctx()
                .load_texture("gb-screen", img.clone(), egui::TextureOptions::NEAREST)
        });
        tex.set(img, egui::TextureOptions::NEAREST);
        let avail = ui.available_size();
        let scale = (avail.x / w as f32).min(avail.y / h as f32);
        let size = egui::vec2(w as f32 * scale, h as f32 * scale);
        ui.image((tex.id(), size));
    }

    fn palette_ui(&mut self, ui: &mut egui::Ui) {
        for (name, pal) in PALETTES {
            if ui
                .selectable_label(self.palette_name == name, name)
                .clicked()
            {
                self.palette = pal;
                self.palette_name = name.to_string();
            }
        }
    }

    /// Handle a state-slot key (F1-F4): save normally, load with Shift held.
    fn state_key(&mut self, slot: usize, shift: bool) {
        if slot < 4 {
            let suffix = STATE_SLOTS[slot];
            if shift {
                self.load_slot(suffix);
            } else {
                self.save_slot(suffix);
            }
        }
    }
}

fn default_key(b: Button) -> Option<egui::Key> {
    Some(match b {
        Button::Up => egui::Key::ArrowUp,
        Button::Down => egui::Key::ArrowDown,
        Button::Left => egui::Key::ArrowLeft,
        Button::Right => egui::Key::ArrowRight,
        Button::A => egui::Key::Z,
        Button::B => egui::Key::X,
        Button::Start => egui::Key::Enter,
        Button::Select => egui::Key::Backspace,
        Button::L => egui::Key::A,
        Button::R => egui::Key::S,
        _ => return None,
    })
}

impl eframe::App for CrabBoyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f64();
        self.last_frame = now;

        self.handle_input(ctx);
        self.advance(ctx, dt);

        self.fps_frames += 1;
        if now.duration_since(self.last_fps).as_secs_f64() >= 1.0 {
            self.fps = self.fps_frames as f64 / now.duration_since(self.last_fps).as_secs_f64();
            self.fps_frames = 0;
            self.last_fps = now;
        }

        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open ROM...").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("Game Boy / GBA ROM", &["gb", "gbc", "gba"])
                            .pick_file()
                        {
                            let p = path.to_string_lossy().to_string();
                            self.load_rom(&p, ctx);
                        }
                        ui.close_menu();
                    }
                    if ui.button("Reset").clicked() {
                        self.reset();
                        ui.close_menu();
                    }
                    ui.separator();
                    ui.menu_button("Save State", |ui| {
                        for (i, suffix) in STATE_SLOTS.iter().enumerate() {
                            let label = if i == 4 {
                                "Quick (F5)"
                            } else {
                                &format!("Slot {} (F{})", i + 1, i + 1)
                            };
                            if ui.button(label).clicked() {
                                self.save_slot(suffix);
                                ui.close_menu();
                            }
                        }
                    });
                    ui.menu_button("Load State", |ui| {
                        for (i, suffix) in STATE_SLOTS.iter().enumerate() {
                            let label = if i == 4 {
                                "Quick (F9)"
                            } else {
                                &format!("Slot {} (Shift+F{})", i + 1, i + 1)
                            };
                            if ui.button(label).clicked() {
                                self.load_slot(suffix);
                                ui.close_menu();
                            }
                        }
                    });
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        ui.close_menu();
                    }
                });
                ui.menu_button("View", |ui| {
                    ui.checkbox(&mut self.paused, "Pause");
                    ui.checkbox(&mut self.fast_forward, "Fast-forward");
                    ui.separator();
                    self.palette_ui(ui);
                });
                ui.menu_button("Help", |ui| {
                    ui.label("Controls:");
                    ui.label("Arrows: D-pad");
                    ui.label("Z: A    X: B");
                    ui.label("Enter: Start    Backspace: Select");
                    ui.label("A: L (GBA)    S: R (GBA)");
                    ui.label("P: Pause    R: Reset    F: Fast-forward");
                    ui.label("F1-F4: Save state    Shift+F1-F4: Load state");
                    ui.label("F5: Quick save    F9: Quick load");
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(&self.status);
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.draw_screen(ui);
        });

        self.update_title(ctx);

        let dropped: Vec<String> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.as_ref().map(|p| p.to_string_lossy().to_string()))
                .collect()
        });
        if let Some(path) = dropped.into_iter().next() {
            self.load_rom(&path, ctx);
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.flush_save();
    }
}

fn main() -> eframe::Result<()> {
    let (rom_path, bios_path) = {
        let mut args = std::env::args().skip(1);
        let mut rom = None;
        let mut bios = None;
        while let Some(a) = args.next() {
            match a.as_str() {
                "--rom" => rom = args.next(),
                "--bios" => bios = args.next(),
                "-h" | "--help" => {
                    println!("usage: CrabBoy [--rom <file>] [--bios <gba_bios.bin>] [<file>]");
                    return Ok(());
                }
                _ if rom.is_none() && !a.starts_with('-') => rom = Some(a),
                _ => {}
            }
        }
        (rom, bios)
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([WIN_W, WIN_H + 60.0])
        .with_min_inner_size([MIN_W, MIN_H + 40.0])
        .with_title("CrabBoy Emulator");
    match eframe::icon_data::from_png_bytes(APP_ICON) {
        Ok(icon) => viewport = viewport.with_icon(icon),
        Err(e) => eprintln!("warning: could not decode the embedded app icon: {e}"),
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let audio = OutputStream::try_default()
        .ok()
        .and_then(|(stream, handle)| Sink::try_new(&handle).ok().map(|sink| (stream, sink)));
    if audio.is_none() {
        eprintln!("warning: no audio output device found; running silently");
    }

    eframe::run_native(
        "CrabBoy Emulator",
        options,
        Box::new(move |cc| Ok(Box::new(CrabBoyApp::new(cc, rom_path, bios_path, audio)))),
    )
}
