//! GBA PPU: scanline renderer for all five background modes, sprites (OBJ),
//! windows, mosaic and color effects.
//!
//! Rendering is scanline-based: the system root runs the CPU for one scanline
//! worth of cycles and then calls [`Ppu::render_scanline`]. The PPU reads the
//! display control registers, VRAM, OAM and palettes straight from the [`Bus`]
//! and writes native 15-bit BGR555 pixels into [`Ppu::framebuffer`].
//!
//! Timing constants match the GBA: 240 visible pixels + 68 H-blank pixels per
//! line (308 dots), 160 visible lines + 68 V-blank lines (228 lines), 4 cycles
//! per dot => 280896 cycles/frame.

use crate::bus::Bus;

/// Visible screen width in pixels.
pub const SCREEN_W: usize = 240;
/// Visible screen height in pixels.
pub const SCREEN_H: usize = 160;
/// Dots (pixels) per full scanline including H-blank.
pub const DOTS_PER_LINE: u32 = 308;
/// Visible pixels per scanline.
pub const VISIBLE_DOTS: u32 = 240;
/// Total scanlines per frame including V-blank.
pub const LINES_PER_FRAME: u32 = 228;
/// Visible scanlines.
pub const VISIBLE_LINES: u32 = 160;
/// CPU cycles per scanline (308 dots * 4).
pub const CYCLES_PER_LINE: u32 = DOTS_PER_LINE * 4;
/// CPU cycles per full frame.
pub const FRAME_CYCLES: u32 = CYCLES_PER_LINE * LINES_PER_FRAME;

/// Build a native 15-bit BGR555 colour.
#[inline]
pub fn rgb(r: u32, g: u32, b: u32) -> u16 {
    (((r & 0x1F) << 10) | ((g & 0x1F) << 5) | (b & 0x1F)) as u16
}

/// Layer ids used in the `top_layer` bookkeeping.
const BG0: u8 = 0;
const BG1: u8 = 1;
const BG2: u8 = 2;
const BG3: u8 = 3;
const LAYER_OBJ: u8 = 4;
const LAYER_BACKDROP: u8 = 5;

/// The GBA PPU.
pub struct Ppu {
    /// Native BGR555 framebuffer, `SCREEN_W * SCREEN_H` entries, row-major.
    pub framebuffer: Vec<u16>,
    /// Scanline buffers: colour + priority/transparency per BG and OBJ.
    bg_color: [[u16; SCREEN_W]; 4],
    bg_prio: [[u8; SCREEN_W]; 4],
    obj_color: [u16; SCREEN_W],
    obj_prio: [u8; SCREEN_W],
    /// Columns covered by OBJ-window sprites this scanline.
    objwin: [bool; SCREEN_W],
    /// Top layer per column after compositing (for color effects).
    top_layer: [u8; SCREEN_W],
    /// Top colour per column after compositing.
    top_color: [u16; SCREEN_W],
}

impl Default for Ppu {
    fn default() -> Self {
        Ppu {
            framebuffer: vec![0; SCREEN_W * SCREEN_H],
            bg_color: [[0; SCREEN_W]; 4],
            bg_prio: [[0; SCREEN_W]; 4],
            obj_color: [0; SCREEN_W],
            obj_prio: [0; SCREEN_W],
            objwin: [false; SCREEN_W],
            top_layer: [LAYER_BACKDROP; SCREEN_W],
            top_color: [0; SCREEN_W],
        }
    }
}

impl Ppu {
    pub fn new() -> Ppu {
        Ppu::default()
    }

    #[inline]
    fn dispcnt(bus: &Bus) -> u16 {
        u16::from_le_bytes([bus.io.regs[0], bus.io.regs[1]])
    }
    #[inline]
    fn bgcnt(bus: &Bus, bg: usize) -> u16 {
        let o = 0x10 + bg * 8;
        u16::from_le_bytes([bus.io.regs[o], bus.io.regs[o + 1]])
    }
    #[inline]
    fn reg16(bus: &Bus, off: usize) -> u16 {
        u16::from_le_bytes([bus.io.regs[off & 0x3FF], bus.io.regs[(off + 1) & 0x3FF]])
    }
    #[inline]
    fn reg32(bus: &Bus, off: usize) -> u32 {
        let o = off & 0x3FC;
        u32::from_le_bytes([
            bus.io.regs[o],
            bus.io.regs[o + 1],
            bus.io.regs[o + 2],
            bus.io.regs[o + 3],
        ])
    }
    #[inline]
    fn vram_index(addr: usize) -> usize {
        addr & (crate::bus::VRAM_SIZE - 1)
    }
    #[inline]
    fn vram16(bus: &Bus, addr: usize) -> u16 {
        u16::from_le_bytes([
            bus.vram[Self::vram_index(addr)],
            bus.vram[Self::vram_index(addr + 1)],
        ])
    }
    #[inline]
    fn bg_palette(bus: &Bus, index: usize) -> u16 {
        u16::from_le_bytes([bus.palram[index * 2], bus.palram[index * 2 + 1]]) & 0x7FFF
    }
    #[inline]
    fn obj_palette(bus: &Bus, index: usize) -> u16 {
        u16::from_le_bytes([bus.palram[0x200 + index * 2], bus.palram[0x200 + index * 2 + 1]]) & 0x7FFF
    }

    /// Render one scanline (`y` in 0..160) into the framebuffer.
    pub fn render_scanline(&mut self, bus: &Bus, y: u32) {
        let disp = Self::dispcnt(bus);
        // Forced blank: fill white.
        if disp & (1 << 6) != 0 {
            for x in 0..SCREEN_W {
                self.framebuffer[y as usize * SCREEN_W + x] = 0x7FFF;
            }
            return;
        }
        let mode = (disp & 7) as usize;

        let bg_enabled = [
            disp & (1 << 8) != 0,
            disp & (1 << 9) != 0,
            disp & (1 << 10) != 0,
            disp & (1 << 11) != 0,
        ];
        let obj_enabled = disp & (1 << 12) != 0;
        let win0 = disp & (1 << 13) != 0;
        let win1 = disp & (1 << 14) != 0;
        let obj_win = disp & (1 << 15) != 0;

        self.bg_prio = [[0; SCREEN_W]; 4];
        self.objwin = [false; SCREEN_W];
        self.obj_prio = [0; SCREEN_W];

        for (bg, &enabled) in bg_enabled.iter().enumerate() {
            if !enabled {
                continue;
            }
            match mode {
                0 => self.render_text(bus, bg, y),
                1 => {
                    if bg <= 1 {
                        self.render_text(bus, bg, y);
                    } else {
                        self.render_affine(bus, bg, y);
                    }
                }
                2 => self.render_affine(bus, bg, y),
                3..=5
                    if bg == 2 => {
                        self.render_bitmap(bus, mode, y);
                    }
                _ => {}
            }
        }
        if obj_enabled {
            self.render_obj(bus, y, disp);
        }
        self.composite(bus, y, win0, win1, obj_win);
    }

    /// Render a text-mode background (BG0-3, tile map + tiles + palettes).
    fn render_text(&mut self, bus: &Bus, bg: usize, y: u32) {
        let cnt = Self::bgcnt(bus, bg);
        let priority = (cnt & 3) as u8;
        let char_base = ((cnt >> 2) & 3) as usize;
        let palette256 = cnt & (1 << 6) != 0;
        let screen_base = ((cnt >> 7) & 31) as usize;
        let size = ((cnt >> 14) & 3) as usize;
        let hofs = Self::reg16(bus, 0x12 + bg * 8) as u32;
        let vofs = Self::reg16(bus, 0x14 + bg * 8) as u32;

        let (tile_w, tile_h) = match size {
            0 => (32, 32),
            1 => (64, 32),
            2 => (32, 64),
            _ => (64, 64),
        };
        let map = screen_base * 0x800;
        let char_block_byte = if palette256 { 0x8000 } else { 0x4000 };
        let char_base_byte = char_base * char_block_byte;

        let mosaic = (cnt >> 4) & 3 != 0;
        let mh = if mosaic { (Self::reg16(bus, 0x4C) & 0x0F) as usize } else { 0 };

        for x in 0..SCREEN_W {
            let mx = if mosaic { x & !mh } else { x };
            let sx = (mx as u32).wrapping_add(hofs) & (tile_w as u32 * 8 - 1);
            let sy = y.wrapping_add(vofs) & (tile_h as u32 * 8 - 1);
            let tx = (sx / 8) as usize;
            let ty = (sy / 8) as usize;
            let entry_addr = map + (ty * tile_w + tx) * 2;
            let entry = Self::vram16(bus, entry_addr);

            let tile = (entry & 0x3FF) as usize;
            let pal_bank = ((entry >> 10) & 0xF) as usize;
            let hflip = entry & (1 << 12) != 0;
            let vflip = entry & (1 << 13) != 0;

            let mut px = (sx % 8) as usize;
            let mut py = (sy % 8) as usize;
            if hflip {
                px = 7 - px;
            }
            if vflip {
                py = 7 - py;
            }

            let tile_off = tile * if palette256 { 64 } else { 32 } + py * 8 + px;
            let byte = bus.vram[Self::vram_index(char_base_byte + tile_off)];
            let (color_idx, opaque) = if palette256 {
                (byte as u32, byte != 0)
            } else {
                let lo = byte & 0x0F;
                let hi = (byte >> 4) & 0x0F;
                if lo != 0 {
                    (pal_bank as u32 * 16 + lo as u32, true)
                } else if hi != 0 {
                    (pal_bank as u32 * 16 + hi as u32, true)
                } else {
                    (0, false)
                }
            };
            if !opaque {
                continue;
            }
            self.bg_color[bg][x] = Self::bg_palette(bus, color_idx as usize);
            self.bg_prio[bg][x] = priority + 1;
        }
    }

    /// Render an affine background (BG2/BG3 in modes 1/2).
    fn render_affine(&mut self, bus: &Bus, bg: usize, y: u32) {
        let cnt = Self::bgcnt(bus, bg);
        let priority = (cnt & 3) as u8;
        let screen_base = ((cnt >> 7) & 31) as usize;
        let size = ((cnt >> 14) & 3) as usize;
        let wrap = ((cnt >> 12) & 3) != 0;
        // BG2 affine parameters at 0x30, BG3 at 0x40.
        let base = if bg == 2 { 0x30 } else { 0x40 };
        let pa = Self::reg16(bus, base) as i16 as i32;
        let pb = Self::reg16(bus, base + 2) as i16 as i32;
        let pc = Self::reg16(bus, base + 4) as i16 as i32;
        let pd = Self::reg16(bus, base + 6) as i16 as i32;
        let ref_x = Self::reg32(bus, base + 8) as i32;
        let ref_y = Self::reg32(bus, base + 12) as i32;

        let (tile_w, tile_h) = match size {
            0 => (32, 32),
            1 => (64, 64),
            2 => (128, 128),
            _ => (256, 256),
        };
        let map = screen_base * 0x800;

        let ix = (ref_x as i64 + (pb as i64) * (y as i64) * 256) >> 8;
        let iy = (ref_y as i64 + (pd as i64) * (y as i64) * 256) >> 8;

        for x in 0..SCREEN_W {
            let sx = ix + (pa as i64) * (x as i64);
            let sy = iy + (pc as i64) * (x as i64);
            if !wrap {
                let w = tile_w as i64 * 8;
                let h = tile_h as i64 * 8;
                if sx < 0 || sx >= w || sy < 0 || sy >= h {
                    continue;
                }
            }
            let sx = sx & (tile_w as i64 * 8 - 1);
            let sy = sy & (tile_h as i64 * 8 - 1);
            let tx = (sx / 8) as usize;
            let ty = (sy / 8) as usize;
            let entry_addr = map + (ty * tile_w + tx) * 2;
            let entry = Self::vram16(bus, entry_addr);
            let tile = (entry & 0x3FF) as usize;
            let px = (sx % 8) as usize;
            let py = (sy % 8) as usize;
            let byte = bus.vram[Self::vram_index(tile * 64 + py * 8 + px)];
            if byte == 0 {
                continue;
            }
            self.bg_color[bg][x] = Self::bg_palette(bus, byte as usize);
            self.bg_prio[bg][x] = priority + 1;
        }
    }

    /// Render a bitmap background (modes 3/4/5).
    fn render_bitmap(&mut self, bus: &Bus, mode: usize, y: u32) {
        let disp = Self::dispcnt(bus);
        let frame_page = (disp >> 3) & 1 != 0;
        for x in 0..SCREEN_W {
            if mode == 3 {
                let addr = (y as usize * SCREEN_W + x) * 2;
                self.bg_color[2][x] = Self::vram16(bus, addr);
                self.bg_prio[2][x] = 1;
            } else if mode == 4 {
                let page = if frame_page { 0xA000 } else { 0 };
                let byte = bus.vram[Self::vram_index(page + y as usize * SCREEN_W + x)];
                if byte == 0 {
                    continue;
                }
                self.bg_color[2][x] = Self::bg_palette(bus, byte as usize);
                self.bg_prio[2][x] = 1;
            } else {
                if y >= 128 {
                    continue;
                }
                let page = if frame_page { 0xA000 } else { 0 };
                let addr = page + (y as usize * 160 + x) * 2;
                self.bg_color[2][x] = Self::vram16(bus, addr);
                self.bg_prio[2][x] = 1;
            }
        }
    }

    /// Render sprites (OBJ) into the OBJ layer buffer.
    fn render_obj(&mut self, bus: &Bus, y: u32, disp: u16) {
        const SIZES: [[usize; 4]; 3] = [
            [8, 16, 32, 64],
            [16, 32, 32, 64],
            [8, 8, 16, 32],
        ];
        let obj_1d = disp & (1 << 5) != 0;

        for i in 0..128 {
            let o = i * 8;
            let a0 = u16::from_le_bytes([bus.oam[o], bus.oam[o + 1]]);
            let a1 = u16::from_le_bytes([bus.oam[o + 2], bus.oam[o + 3]]);
            let a2 = u16::from_le_bytes([bus.oam[o + 4], bus.oam[o + 5]]);

            let y_pos = (a0 & 0xFF) as i32;
            let affine_mode = (a0 >> 8) & 3;
            let mode = (a0 >> 10) & 3;
            let palette256 = a0 & (1 << 13) != 0;
            let shape = ((a0 >> 14) & 3) as usize;
            let x_pos = (a1 & 0x1FF) as i32;
            let size = ((a1 >> 10) & 3) as usize;
            let hflip = a1 & (1 << 12) != 0;
            let vflip = a1 & (1 << 13) != 0;
            let affine_index = ((a1 >> 9) & 31) as usize;
            let tile_base = (a2 & 0x3FF) as usize;
            let priority = ((a2 >> 10) & 3) as u8;
            let pal_bank = ((a2 >> 12) & 0xF) as usize;

            let (w, h) = {
                let w = SIZES[shape][size];
                let h = if shape == 0 {
                    w
                } else if shape == 1 {
                    w / 2
                } else {
                    w * 2
                };
                (w, h)
            };
            let affine = affine_mode != 0;

            // Affine parameters (signed 8.8) for the selected matrix.
            let (pa, pb, pc, pd) = if affine {
                let ao = (affine_index / 2) * 8;
                let ao = ao & 0x3FF;
                (
                    u16::from_le_bytes([bus.oam[ao], bus.oam[ao + 1]]) as i16 as i32,
                    u16::from_le_bytes([bus.oam[ao + 2], bus.oam[ao + 3]]) as i16 as i32,
                    u16::from_le_bytes([bus.oam[ao + 4], bus.oam[ao + 5]]) as i16 as i32,
                    u16::from_le_bytes([bus.oam[ao + 6], bus.oam[ao + 7]]) as i16 as i32,
                )
            } else {
                (0, 0, 0, 0)
            };

            // OBJ-window sprites (mode 2) don't draw, but mark their columns.
            if mode == 2 {
                for dx in 0..w {
                    let x = x_pos + dx as i32;
                    if x >= 0 && (x as usize) < SCREEN_W && y >= (y_pos as u32) && y < (y_pos as u32 + h as u32) {
                        self.objwin[x as usize] = true;
                    }
                }
                continue;
            }
            if mode == 1 {
                // Semi-transparent: treat as normal for now.
            }

            for dy in 0..h {
                let sy = y_pos + dy as i32;
                if sy < 0 || sy >= SCREEN_H as i32 {
                    continue;
                }
                if y != sy as u32 {
                    continue;
                }
                for dx in 0..w {
                    let sx = x_pos + dx as i32;
                    if sx < 0 || sx >= SCREEN_W as i32 {
                        continue;
                    }
                    // Sample the tile pixel.
                    let (tpx, tpy) = if affine {
                        // Map back through the inverse matrix (approx via forward).
                        // Use the matrix to sample: (dx,dy) relative to centre.
                        let cx = (dx as i32 - (w as i32 / 2)) as i64;
                        let cy = (dy as i32 - (h as i32 / 2)) as i64;
                        let mx = (pa as i64 * cx + pb as i64 * cy) >> 8;
                        let my = (pc as i64 * cx + pd as i64 * cy) >> 8;
                        let tpx = (mx as i32).rem_euclid(w as i32);
                        let tpy = (my as i32).rem_euclid(h as i32);
                        (tpx as usize, tpy as usize)
                    } else {
                        let mut tpx = dx;
                        let mut tpy = dy;
                        if hflip {
                            tpx = w - 1 - tpx;
                        }
                        if vflip {
                            tpy = h - 1 - tpy;
                        }
                        (tpx, tpy)
                    };

                    // Tile index: for 2D mapping tiles are laid out in rows; for
                    // 1D mapping tiles advance per width.
                    let tile_in_row = tpx / 8;
                    let tile_y = tpy / 8;
                    let tiles_per_row = w / 8;
                    let tile_index = if obj_1d {
                        tile_base + tile_y * tiles_per_row + tile_in_row
                    } else {
                        // 2D: tile_base plus row offset using the standard layout.
                        tile_base + (tile_y * (32 / 8)) + tile_in_row
                    };
                    let px = tpx % 8;
                    let py = tpy % 8;
                    let tile_off = tile_index * (if palette256 { 64 } else { 32 }) + py * 8 + px;
                    let byte = bus.vram[Self::vram_index(tile_off)];
                    let (color_idx, opaque) = if palette256 {
                        (byte as usize, byte != 0)
                    } else {
                        let lo = byte & 0x0F;
                        let hi = (byte >> 4) & 0x0F;
                        if lo != 0 {
                            (pal_bank * 16 + lo as usize, true)
                        } else if hi != 0 {
                            (pal_bank * 16 + hi as usize, true)
                        } else {
                            (0, false)
                        }
                    };
                    if !opaque {
                        continue;
                    }
                    let color = Self::obj_palette(bus, color_idx);
                    // OBJ with equal priority over a BG: OBJ wins here via
                    // later compositing; store colour + priority.
                    self.obj_color[sx as usize] = color;
                    self.obj_prio[sx as usize] = priority + 1;
                }
            }
        }
    }

    fn composite(&mut self, bus: &Bus, y: u32, win0: bool, win1: bool, obj_win: bool) {
        let win0h = Self::reg16(bus, 0x40);
        let win1h = Self::reg16(bus, 0x42);
        let win0v = Self::reg16(bus, 0x44);
        let win1v = Self::reg16(bus, 0x46);
        let winin = Self::reg16(bus, 0x48);
        let winout = Self::reg16(bus, 0x4A);
        let bldcnt = Self::reg16(bus, 0x50);
        let bldalpha = Self::reg16(bus, 0x52);
        let bldy = Self::reg16(bus, 0x54);

        let effect = (bldcnt >> 10) & 3;
        let first_tgt = bldcnt & 0x3F;
        let second_tgt = (bldcnt >> 8) & 0x3F;
        let eva = (bldalpha & 0x1F) as u32;
        let evb = ((bldalpha >> 8) & 0x1F) as u32;
        let evy = (bldy & 0x1F) as u32;

        let row = y as usize * SCREEN_W;
        for x in 0..SCREEN_W {
            // Determine the window covering this pixel.
            let any_window = win0 || win1 || obj_win;
            let mask = if !any_window {
                0x3F
            } else if win0 && Self::in_window(x as i32, y as i32, win0h, win0v) {
                winin & 0x3F
            } else if win1 && Self::in_window(x as i32, y as i32, win1h, win1v) {
                (winin >> 8) & 0x3F
            } else if obj_win && self.objwin[x] {
                (winout >> 8) & 0x3F
            } else {
                winout & 0x3F
            };

            // Composite by priority: scan topmost-first (p=0 on top), and
            // within a priority OBJ sits above BG and higher BG above lower.
            let order = [LAYER_OBJ, BG3, BG2, BG1, BG0];
            let mut color = 0u16;
            let mut top_layer = LAYER_BACKDROP;
            let mut found_top = false;
            let mut second_color = 0u16;
            let mut have_second = false;
            for p in 0..4u8 {
                for &layer in &order {
                    let bit = 1u16 << layer;
                    if mask & bit == 0 {
                        continue;
                    }
                    let (prio, layer_color) = if layer == LAYER_OBJ {
                        (self.obj_prio[x], self.obj_color[x])
                    } else {
                        (self.bg_prio[layer as usize][x], self.bg_color[layer as usize][x])
                    };
                    if prio == 0 || prio - 1 != p {
                        continue;
                    }
                    if !found_top {
                        found_top = true;
                        top_layer = layer;
                        color = layer_color;
                    } else if second_tgt & (1u16 << layer) != 0 && !have_second {
                        second_color = layer_color;
                        have_second = true;
                    }
                }
            }
            self.top_layer[x] = top_layer;
            self.top_color[x] = color;

            // Color effects.
            let in_first = first_tgt & (1 << top_layer) != 0;
            let out = match effect {
                1 if in_first => {
                    let src = color;
                    let dst = if have_second { second_color } else { 0 };
                    Self::alpha_blend(src, dst, eva, evb)
                }
                2 if in_first => Self::brighten(color, evy),
                3 if in_first => Self::darken(color, evy),
                _ => color,
            };
            self.framebuffer[row + x] = out;
        }
    }

    fn in_window(x: i32, y: i32, wh: u16, wv: u16) -> bool {
        let x1 = (wh & 0xFF) as i32;
        let x2 = ((wh >> 8) & 0xFF) as i32;
        let y1 = (wv & 0xFF) as i32;
        let y2 = ((wv >> 8) & 0xFF) as i32;
        x >= x1 && x <= x2 && y >= y1 && y <= y2
    }

    fn alpha_blend(src: u16, dst: u16, eva: u32, evb: u32) -> u16 {
        let sr = ((src >> 10) & 0x1F) as u32;
        let sg = ((src >> 5) & 0x1F) as u32;
        let sb = (src & 0x1F) as u32;
        let dr = ((dst >> 10) & 0x1F) as u32;
        let dg = ((dst >> 5) & 0x1F) as u32;
        let db = (dst & 0x1F) as u32;
        let r = (sr * eva + dr * evb).min(0x10 * 0x1F) / 16;
        let g = (sg * eva + dg * evb).min(0x10 * 0x1F) / 16;
        let b = (sb * eva + db * evb).min(0x10 * 0x1F) / 16;
        rgb(r, g, b)
    }

    fn brighten(color: u16, evy: u32) -> u16 {
        let r = ((color >> 10) & 0x1F) as u32;
        let g = ((color >> 5) & 0x1F) as u32;
        let b = (color & 0x1F) as u32;
        let f = |c: u32| (c + ((31 - c) * evy / 16)).min(31);
        rgb(f(r), f(g), f(b))
    }

    fn darken(color: u16, evy: u32) -> u16 {
        let r = ((color >> 10) & 0x1F) as u32;
        let g = ((color >> 5) & 0x1F) as u32;
        let b = (color & 0x1F) as u32;
        let f = |c: u32| c - (c * evy / 16);
        rgb(f(r), f(g), f(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;

    fn set16(bus: &mut Bus, off: usize, v: u16) {
        bus.io.regs[off] = v as u8;
        bus.io.regs[off + 1] = (v >> 8) as u8;
    }

    fn test_bus() -> Bus {
        Bus::new(vec![0; 0x4000])
    }

    #[test]
    fn forced_blank_is_white() {
        let mut bus = test_bus();
        set16(&mut bus, 0, 1 << 6); // forced blank
        let mut ppu = Ppu::new();
        ppu.render_scanline(&bus, 0);
        assert_eq!(ppu.framebuffer[0], 0x7FFF);
    }

    #[test]
    fn mode3_bitmap() {
        let mut bus = test_bus();
        set16(&mut bus, 0, 0x0403); // mode 3, BG2 enabled
        // Pixel at (5,3): address (3*240+5)*2 = 1450.
        let color = rgb(31, 0, 0); // pure red
        bus.vram[1450] = color as u8;
        bus.vram[1451] = (color >> 8) as u8;
        let mut ppu = Ppu::new();
        ppu.render_scanline(&bus, 3);
        assert_eq!(ppu.framebuffer[3 * SCREEN_W + 5], color);
        // Unset pixels stay black.
        assert_eq!(ppu.framebuffer[0], 0);
    }

    #[test]
    fn mode0_text_tile() {
        let mut bus = test_bus();
        set16(&mut bus, 0, 0x0100); // mode 0, BG0 enabled
        // BG0CNT at 0x10: priority 0, char base 0, screen base 0, size 0.
        set16(&mut bus, 0x10, 0);
        // Map entry (0,0) -> tile 1 (stored in VRAM).
        bus.vram[0] = 1;
        bus.vram[1] = 0;
        // Tile 1 data at char base 0: byte offset tile*32. Pixel (0,0) -> 5.
        bus.vram[32] = 5;
        // BG palette color 5 = green.
        let green = rgb(0, 31, 0);
        bus.palram[5 * 2] = green as u8;
        bus.palram[5 * 2 + 1] = (green >> 8) as u8;
        let mut ppu = Ppu::new();
        ppu.render_scanline(&bus, 0);
        assert_eq!(ppu.framebuffer[0], green);
    }

    #[test]
    fn mode2_affine_identity() {
        let mut bus = test_bus();
        set16(&mut bus, 0, 0x0402); // mode 2, BG2 enabled
        // BG2CNT at 0x20: priority 0, screen base 0, size 0, no wrap.
        set16(&mut bus, 0x20, 0);
        // Identity matrix PA=256 (1.0), PD=256 (BG2 affine params at 0x30).
        set16(&mut bus, 0x30, 256);
        set16(&mut bus, 0x36, 256);
        // Map entry (0,0) -> tile 1; tile 1 pixel (0,0) -> 7.
        bus.vram[0] = 1;
        bus.vram[1] = 0;
        bus.vram[64] = 7;
        let cyan = rgb(0, 31, 31);
        bus.palram[7 * 2] = cyan as u8;
        bus.palram[7 * 2 + 1] = (cyan >> 8) as u8;
        let mut ppu = Ppu::new();
        ppu.render_scanline(&bus, 0);
        assert_eq!(ppu.framebuffer[0], cyan);
    }

    #[test]
    fn obj_sprite() {
        let mut bus = test_bus();
        set16(&mut bus, 0, 0x1400); // mode 0, OBJ enabled
        // OAM sprite 0: attr0 y=0, square, 16-color; attr1 x=8, size 0 (8x8);
        // attr2 tile 2, priority 0, palette bank 0.
        bus.oam[0] = 0; // y=0
        bus.oam[1] = 0;
        bus.oam[2] = 8; // x=8
        bus.oam[3] = 0;
        bus.oam[4] = 2; // tile 2
        bus.oam[5] = 0;
        // Tile 2, 16-color: byte offset 2*32. Pixel (0,0) -> 3.
        bus.vram[2 * 32] = 3;
        let yellow = rgb(31, 31, 0);
        bus.palram[0x200 + 3 * 2] = yellow as u8;
        bus.palram[0x200 + 3 * 2 + 1] = (yellow >> 8) as u8;
        let mut ppu = Ppu::new();
        ppu.render_scanline(&bus, 0);
        assert_eq!(ppu.framebuffer[8], yellow);
    }
}