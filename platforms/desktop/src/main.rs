use eframe::egui;
use emu_core::{Button, System};
use gb_core::cartridge::Cartridge;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

const GB_W: usize = 160;
const GB_H: usize = 144;

fn sav_path_for(rom: &str) -> Option<String> {
    let p = std::path::Path::new(rom);
    p.extension()?;
    let stem = p.with_extension("");
    Some(format!("{}.sav", stem.to_string_lossy()))
}

/// The 8 buttons a Game Boy exposes (subset of emu_core::Button).
const GB_BUTTONS: [Button; 8] = [
    Button::Up,
    Button::Down,
    Button::Left,
    Button::Right,
    Button::A,
    Button::B,
    Button::Start,
    Button::Select,
];

fn key_name(key: egui::Key) -> String {
    format!("{:?}", key)
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
            colors: [[0xFF, 0xFF, 0xFF, 0xFF], [0xAA, 0xAA, 0xAA, 0xFF], [0x55, 0x55, 0x55, 0xFF], [0, 0, 0, 0xFF]],
        },
    ),
];

struct CrabBoyApp {
    rom_data: Option<Vec<u8>>,
    system: Option<Box<dyn System>>,
    sav_path: Option<String>,
    keymap: HashMap<Button, egui::Key>,
    capture: Option<Button>,
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
    held_keys_str: String,
}

impl CrabBoyApp {
    fn new(cc: &eframe::CreationContext<'_>, rom_path: Option<String>) -> Self {
        let mut keymap = HashMap::new();
        for b in GB_BUTTONS {
            keymap.insert(b, default_key(b));
        }
        let mut app = CrabBoyApp {
            rom_data: None,
            system: None,
            sav_path: None,
            keymap,
            capture: None,
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
            held_keys_str: String::new(),
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
        let cart = match Cartridge::load(&data) {
            Ok(c) => c,
            Err(e) => {
                self.status = format!("Failed to load cartridge: {e}");
                return;
            }
        };
        let info = format!("{} ({})", cart.title, cart.mbc);
        let battery = cart.has_battery();
        let sav_path = sav_path_for(path);

        let mut system: Box<dyn System> = gb_core::gb::Gb::system(cart);
        if battery {
            if let Some(sav) = &sav_path {
                if let Ok(d) = std::fs::read(sav) {
                    system.load_data(&d);
                    self.status = format!("Loaded '{}' (battery save found)", info);
                }
            }
        }

        self.rom_data = Some(data);
        self.sav_path = sav_path;
        self.system = Some(system);
        self.frame_count = 0;
        self.accum = 0.0;
        self.paused = false;
        if !self.status.starts_with("Loaded") {
            self.status = format!("Loaded '{info}'");
        }
        ctx.request_repaint();
    }

    fn reset(&mut self) {
        if let Some(data) = &self.rom_data {
            if let Ok(cart) = Cartridge::load(data) {
                let battery = cart.has_battery();
                let mut system: Box<dyn System> = gb_core::gb::Gb::system(cart);
                if battery {
                    if let Some(sav) = &self.sav_path {
                        if let Ok(d) = std::fs::read(sav) {
                            system.load_data(&d);
                        }
                    }
                }
                self.system = Some(system);
                self.frame_count = 0;
                self.accum = 0.0;
                self.paused = false;
            }
        }
    }

    fn save_state(&self) {
        if let (Some(system), Some(sav)) = (&self.system, &self.sav_path) {
            if system.battery_backed() {
                let data = system.save_data();
                if !data.is_empty() {
                    if let Err(e) = std::fs::write(sav, &data) {
                        eprintln!("failed to write save {sav}: {e}");
                    }
                }
            }
        }
    }

    fn run_frame(&mut self) {
        if let Some(system) = &mut self.system {
            system.run_frame();
            self.frame_count += 1;
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
            let period = 1.0 / 60.0;
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

    fn handle_input(&mut self, ctx: &egui::Context) {
        let raw: Vec<egui::Event> = ctx.input(|i| i.raw.events.clone());
        let mut keys: HashSet<egui::Key> = self.prev_keys.clone();
        let mut newly: Vec<egui::Key> = Vec::new();
        for e in raw {
            match e {
                egui::Event::Key { physical_key, pressed, .. } => {
                    if let Some(k) = physical_key {
                        if pressed {
                            if !keys.contains(&k) {
                                newly.push(k);
                            }
                            keys.insert(k);
                        } else {
                            keys.remove(&k);
                        }
                    }
                }
                egui::Event::WindowFocused(false) => {
                    keys.clear();
                }
                _ => {}
            }
        }

        if let Some(act) = self.capture {
            if let Some(&k) = newly.first() {
                self.keymap.insert(act, k);
                self.capture = None;
            }
        }

        if let Some(system) = &mut self.system {
            for b in GB_BUTTONS {
                let held = self.keymap.get(&b).map(|k| keys.contains(k)).unwrap_or(false);
                if held {
                    system.press(b);
                } else {
                    system.release(b);
                }
            }
        }

        for &k in &newly {
            match k {
                egui::Key::P => self.paused = !self.paused,
                egui::Key::R => self.reset(),
                egui::Key::F => self.fast_forward = !self.fast_forward,
                _ => {}
            }
        }

        self.prev_keys = keys;
        let mut parts: Vec<String> = Vec::new();
        for k in &self.prev_keys {
            let acts: Vec<String> = GB_BUTTONS
                .iter()
                .filter(|b| self.keymap.get(b) == Some(k))
                .map(|b| b.label().to_string())
                .collect();
            let acts = if acts.is_empty() {
                "-".to_string()
            } else {
                acts.join(",")
            };
            parts.push(format!("{}:{}", key_name(*k), acts));
        }
        parts.sort();
        self.held_keys_str = parts.join("  ");
    }

    fn shade_image(&self, shades: &[u8]) -> egui::ColorImage {
        let mut img = egui::ColorImage::new([GB_W, GB_H], egui::Color32::BLACK);
        for (i, &shade) in shades.iter().enumerate() {
            let c = self.palette.colors[(shade & 3) as usize];
            img.pixels[i] = egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
        }
        img
    }

    fn draw_screen(&mut self, ui: &mut egui::Ui) {
        let Some(system) = &self.system else {
            ui.label("No ROM loaded. Use the file picker or drag & drop a .gb file.");
            return;
        };
        let frame = system.frame();
        let img = self.shade_image(&frame.shades);
        let tex = self.screen_texture.get_or_insert_with(|| {
            ui.ctx().load_texture(
                "gb-screen",
                img.clone(),
                egui::TextureOptions::NEAREST,
            )
        });
        tex.set(img, egui::TextureOptions::NEAREST);
        let avail = ui.available_size();
        let scale = (avail.x / GB_W as f32).min(avail.y / GB_H as f32);
        let size = egui::vec2(GB_W as f32 * scale, GB_H as f32 * scale);
        ui.image((tex.id(), size));
    }

    fn controls_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Controls");
        for b in GB_BUTTONS {
            let name = self.keymap.get(&b).map(|k| key_name(*k)).unwrap_or_else(|| "-".into());
            let label = if self.capture == Some(b) {
                "Press a key...".to_string()
            } else {
                name
            };
            if ui
                .button(format!("{}: {}", b.label(), label))
                .on_hover_text("Click to rebind")
                .clicked()
            {
                self.capture = Some(b);
            }
        }
        if self.capture.is_none() {
            ui.separator();
            ui.label("Shortcuts: P = pause, R = reset, F = fast-forward");
        }
    }

    fn palette_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Palette");
        for (name, pal) in PALETTES {
            if ui.selectable_label(self.palette_name == name, name).clicked() {
                self.palette = pal;
                self.palette_name = name.to_string();
            }
        }
    }
}

fn default_key(b: Button) -> egui::Key {
    match b {
        Button::Up => egui::Key::ArrowUp,
        Button::Down => egui::Key::ArrowDown,
        Button::Left => egui::Key::ArrowLeft,
        Button::Right => egui::Key::ArrowRight,
        Button::A => egui::Key::Z,
        Button::B => egui::Key::X,
        Button::Start => egui::Key::Enter,
        Button::Select => egui::Key::Backspace,
        _ => egui::Key::Space,
    }
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
                if ui.button("Open ROM...").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Game Boy ROM", &["gb", "gbc"])
                        .pick_file()
                    {
                        let p = path.to_string_lossy().to_string();
                        self.load_rom(&p, ctx);
                    }
                }
                if ui.button("Reset").clicked() {
                    self.reset();
                }
                ui.separator();
                ui.toggle_value(&mut self.paused, "Pause");
                ui.toggle_value(&mut self.fast_forward, "Fast-forward");
                ui.separator();
                ui.label(format!(
                    "FPS: {:.0}   Frame: {}",
                    self.fps, self.frame_count
                ));
            });
        });

        egui::TopBottomPanel::top("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status);
                if !self.held_keys_str.is_empty() {
                    ui.separator();
                    ui.monospace(format!("held: {}", self.held_keys_str));
                }
            });
        });

        egui::SidePanel::right("controls")
            .default_width(190.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                self.controls_ui(ui);
                ui.add_space(8.0);
                self.palette_ui(ui);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.draw_screen(ui);
        });

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

        if self.frame_count.is_multiple_of(3600) {
            self.save_state();
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

fn main() -> eframe::Result<()> {
    let rom_path: Option<String> = {
        let mut args = std::env::args();
        let mut path = None;
        while let Some(a) = args.next() {
            if a == "--rom" {
                path = args.next();
            }
        }
        path
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([GB_W as f32 * 4.0, GB_H as f32 * 4.0 + 60.0])
            .with_title("CrabBoy Emulator"),
        ..Default::default()
    };
    eframe::run_native(
        "CrabBoy Emulator",
        options,
        Box::new(move |cc| Ok(Box::new(CrabBoyApp::new(cc, rom_path)))),
    )
}