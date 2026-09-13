pub const SCREEN_W: usize = 160;
pub const SCREEN_H: usize = 144;

#[derive(Clone, Copy)]
pub struct Sprite {
    pub x: i16,
    pub y: u8,
    pub tile: u8,
    pub attr: u8,
    pub height: u8,
}

pub struct Ppu {
    pub ly: u8,
    pub mode: u8,
    pub dot: u32,
    pub(crate) mode3_cycles: u32,
    pub(crate) prev_mode: u8,
    pub(crate) prev_coinc: bool,
    pub line_sprites: [Sprite; 10],
    pub line_sprite_count: usize,
    pub vblank_interrupts: u64,
    pub frame_buffer: [u8; SCREEN_W * SCREEN_H],
    /// True when rendering with CGB colour semantics (the loaded cartridge
    /// requests CGB mode). Monochrome games use DMG-mode-on-CGB instead.
    pub cgb: bool,
    /// Background palette RAM: 8 palettes × 4 colours × 2 bytes (RGB555).
    pub(crate) bg_pal: [u8; 64],
    /// Object palette RAM: 8 palettes × 4 colours × 2 bytes (RGB555).
    pub(crate) obj_pal: [u8; 64],
    /// Full-colour RGB888 framebuffer (SCREEN_W * SCREEN_H * 3 bytes).
    pub(crate) rgb_buffer: Vec<u8>,
}

impl Ppu {
    pub fn new() -> Ppu {
        Ppu {
            ly: 0,
            mode: 0,
            dot: 0,
            mode3_cycles: 172,
            prev_mode: 0,
            prev_coinc: false,
            line_sprites: [Sprite {
                x: 0,
                y: 0,
                tile: 0,
                attr: 0,
                height: 8,
            }; 10],
            line_sprite_count: 0,
            vblank_interrupts: 0,
            frame_buffer: [0; SCREEN_W * SCREEN_H],
            cgb: false,
            bg_pal: [0; 64],
            obj_pal: [0; 64],
            rgb_buffer: vec![0; SCREEN_W * SCREEN_H * 3],
        }
    }

    pub fn reset(&mut self, io: &mut [u8; 0x80]) {
        self.ly = 0;
        self.mode = 0;
        self.dot = 0;
        self.prev_mode = 0;
        self.prev_coinc = false;
        io[0x44] = 0;
    }

    /// Resolve the address of a palette register (0xFF68–0xFF6B) read.
    pub(crate) fn read_pal_reg(&self, addr: u16, io: &[u8; 0x80]) -> u8 {
        let index = match addr {
            0xFF68 => io[0x68] & 0x3F,
            0xFF69 => {
                let i = io[0x68] & 0x3F;
                return self.bg_pal[i as usize];
            }
            0xFF6A => io[0x6A] & 0x3F,
            0xFF6B => {
                let i = io[0x6A] & 0x3F;
                return self.obj_pal[i as usize];
            }
            _ => 0,
        };
        // 0xFF68 / 0xFF6A return the current index.
        index | 0x40 | 0x80
    }

    pub(crate) fn write_bg_pal(&mut self, value: u8, io: &mut [u8; 0x80]) {
        let i = io[0x68] & 0x3F;
        self.bg_pal[i as usize] = value;
        if io[0x68] & 0x80 != 0 {
            io[0x68] = 0x80 | ((i + 1) & 0x3F);
        }
    }

    pub(crate) fn write_obj_pal(&mut self, value: u8, io: &mut [u8; 0x80]) {
        let i = io[0x6A] & 0x3F;
        self.obj_pal[i as usize] = value;
        if io[0x6A] & 0x80 != 0 {
            io[0x6A] = 0x80 | ((i + 1) & 0x3F);
        }
    }

    /// Decode a palette RAM entry into RGB888. `pal_idx` is 0–7 for the BG
    /// palettes or 8–15 for the OBJ palettes; `col` is the 2-bit colour index.
    fn palette_rgb(&self, pal_idx: u8, col: u8) -> [u8; 3] {
        let (ram, p) = if pal_idx >= 8 {
            (self.obj_pal, pal_idx - 8)
        } else {
            (self.bg_pal, pal_idx)
        };
        let i = (p as usize & 7) * 8 + (col as usize & 3) * 2;
        let lo = ram[i];
        let hi = ram[i + 1];
        let c15 = (hi as u16 & 0x7F) << 8 | lo as u16;
        let r = ((c15 & 0x1F) * 255 / 31) as u8;
        let g = (((c15 >> 5) & 0x1F) * 255 / 31) as u8;
        let b = (((c15 >> 10) & 0x1F) * 255 / 31) as u8;
        [r, g, b]
    }

    pub fn step(
        &mut self,
        cycles: u32,
        io: &mut [u8; 0x80],
        vram: &mut [u8; 0x4000],
        oam: &mut [u8; 0xA0],
    ) {
        let lcdc = io[0x40];
        if lcdc & 0x80 == 0 {
            if self.ly != 0 || self.mode != 0 {
                self.reset(io);
            }
            return;
        }

        self.dot += cycles;
        // `dot` is the position within the current 456-dot line:
        //   mode 2 (OAM):      dot in [0, 80)
        //   mode 3 (render):   dot in [80, 80 + mode3_cycles)
        //   mode 0 (HBlank):   dot in [80 + mode3_cycles, 456)
        //   mode 1 (VBlank):   456 dots per line
        loop {
            match self.mode {
                0 => {
                    if self.dot >= 456 {
                        self.dot -= 456;
                        self.end_scanline(io, vram, oam);
                    } else {
                        break;
                    }
                }
                1 => {
                    if self.dot >= 456 {
                        self.dot -= 456;
                        self.ly = self.ly.wrapping_add(1);
                        self.set_ly(io);
                        if self.ly == 154 {
                            self.ly = 0;
                            self.set_ly(io);
                            self.begin_scanline(io, vram, oam);
                        }
                    } else {
                        break;
                    }
                }
                2 => {
                    if self.dot >= 80 {
                        self.mode = 3;
                        self.update_stat(io);
                    } else {
                        break;
                    }
                }
                3 => {
                    if self.dot >= 80 + self.mode3_cycles {
                        self.render_line(io, vram, oam);
                        self.mode = 0;
                        self.update_stat(io);
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
    }

    fn end_scanline(&mut self, io: &mut [u8; 0x80], vram: &mut [u8; 0x4000], oam: &mut [u8; 0xA0]) {
        self.ly = self.ly.wrapping_add(1);
        self.set_ly(io);
        if self.ly == 144 {
            self.mode = 1;
            self.update_stat(io);
            io[0x0F] |= 0x01;
            self.vblank_interrupts += 1;
        } else {
            self.begin_scanline(io, vram, oam);
        }
    }

    fn begin_scanline(
        &mut self,
        io: &mut [u8; 0x80],
        _vram: &mut [u8; 0x4000],
        oam: &mut [u8; 0xA0],
    ) {
        self.scan_oam(io, oam);
        self.mode = 2;
        self.update_stat(io);
    }

    fn scan_oam(&mut self, io: &mut [u8; 0x80], oam: &mut [u8; 0xA0]) {
        let lcdc = io[0x40];
        let height = if lcdc & 0x04 != 0 { 16 } else { 8 };
        let mut count = 0;
        for i in 0..40 {
            if count >= 10 {
                break;
            }
            let o = i * 4;
            let sy = oam[o];
            let sx = oam[o + 1];
            let tile = oam[o + 2];
            let attr = oam[o + 3];
            if sx == 0 || sx > 168 {
                continue;
            }
            let screen_y = sy as i16 - 16;
            if (self.ly as i16) >= screen_y && (self.ly as i16) < screen_y + height as i16 {
                self.line_sprites[count] = Sprite {
                    x: sx as i16 - 8,
                    y: sy,
                    tile,
                    attr,
                    height,
                };
                count += 1;
            }
        }
        self.line_sprite_count = count;
        self.mode3_cycles = 172 + 6 * count.min(10) as u32;
    }

    fn set_ly(&mut self, io: &mut [u8; 0x80]) {
        io[0x44] = self.ly;
        self.update_stat(io);
    }

    fn update_stat(&mut self, io: &mut [u8; 0x80]) {
        let stat = io[0x41];
        let coinc = self.ly == io[0x45];

        // STAT interrupt on mode transitions: mode 2 -> OAM (0x20),
        // mode 1 -> VBlank (0x10), mode 0 -> HBlank (0x08). Mode 3 never fires.
        if self.mode != self.prev_mode {
            let bit = match self.mode {
                2 => 0x20,
                1 => 0x10,
                0 => 0x08,
                _ => 0,
            };
            if bit != 0 && stat & bit != 0 {
                io[0x0F] |= 0x02;
            }
        }

        // STAT interrupt on the rising edge of LY == LYC (0x40).
        if coinc && !self.prev_coinc && stat & 0x40 != 0 {
            io[0x0F] |= 0x02;
        }

        self.prev_mode = self.mode;
        self.prev_coinc = coinc;

        let mut new_stat = (stat & 0xF8) | (self.mode & 0x03);
        if coinc {
            new_stat |= 0x04;
        } else {
            new_stat &= !0x04;
        }
        io[0x41] = new_stat;
    }

    fn tile_base(&self, io: &[u8; 0x80], tile_index: u8) -> usize {
        let lcdc = io[0x40];
        if lcdc & 0x10 != 0 {
            (0x8000 + tile_index as usize * 16) & 0x1FFF
        } else {
            let signed = tile_index as i8 as i16;
            ((0x9000i32 + signed as i32 * 16) & 0x1FFF) as usize
        }
    }

    fn tile_pixels(
        &self,
        vram: &[u8; 0x4000],
        io: &[u8; 0x80],
        tile_index: u8,
        row: u8,
        bank: usize,
    ) -> [u8; 8] {
        let base = self.tile_base(io, tile_index) + bank * 0x2000;
        let lo = vram[base + row as usize * 2];
        let hi = vram[base + row as usize * 2 + 1];
        let mut out = [0u8; 8];
        for (px, c) in out.iter_mut().enumerate() {
            let bit = 7 - px;
            *c = ((hi >> bit) & 1) << 1 | ((lo >> bit) & 1);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn render_bg(
        &self,
        io: &[u8; 0x80],
        vram: &[u8; 0x4000],
        bg_color: &mut [u8; SCREEN_W],
        bg_shade: &mut [u8; SCREEN_W],
        bg_pal: &mut [u8; SCREEN_W],
        bg_prio: &mut [u8; SCREEN_W],
        cache: &mut [Option<[u8; 8]>; 512],
    ) {
        let lcdc = io[0x40];
        let scy = io[0x42] as usize;
        let scx = io[0x43] as usize;
        let map_base = if lcdc & 0x08 != 0 { 0x1C00 } else { 0x1800 };
        let bgp = io[0x47];

        let y = (self.ly as usize).wrapping_add(scy);
        let tile_row = (y >> 3) & 31;
        let row_in_tile = y & 7;

        for px in 0..SCREEN_W {
            let x = px.wrapping_add(scx);
            let tile_col = (x >> 3) & 31;
            let addr = map_base + tile_row * 32 + tile_col;
            let tile_index = vram[addr];
            let attr = if self.cgb { vram[0x2000 + addr] } else { 0 };
            let bank = if self.cgb {
                (attr as usize >> 3) & 1
            } else {
                0
            };
            let pal = if self.cgb { attr & 7 } else { 0 };
            let row = if self.cgb && attr & 0x40 != 0 {
                7 - row_in_tile as u8
            } else {
                row_in_tile as u8
            };
            let pixels = *cache[bank * 256 + tile_index as usize]
                .get_or_insert_with(|| self.tile_pixels(vram, io, tile_index, row, bank));
            let col = if self.cgb && attr & 0x20 != 0 {
                7 - (x & 7) as u8
            } else {
                (x & 7) as u8
            };
            let cv = pixels[col as usize];
            bg_color[px] = cv;
            bg_pal[px] = pal;
            bg_prio[px] = if self.cgb { attr >> 7 } else { 0 };
            if self.cgb {
                bg_shade[px] = cv;
            } else {
                bg_shade[px] = (bgp >> (cv * 2)) & 3;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_window(
        &self,
        io: &[u8; 0x80],
        vram: &[u8; 0x4000],
        bg_color: &mut [u8; SCREEN_W],
        bg_shade: &mut [u8; SCREEN_W],
        bg_pal: &mut [u8; SCREEN_W],
        bg_prio: &mut [u8; SCREEN_W],
        cache: &mut [Option<[u8; 8]>; 512],
    ) {
        let lcdc = io[0x40];
        let wy = io[0x4A] as usize;
        if (self.ly as usize) < wy {
            return;
        }
        let wx = io[0x4B] as i32 - 7;
        if wx > 159 {
            return;
        }
        let map_base = if lcdc & 0x20 != 0 { 0x1C00 } else { 0x1800 };
        let bgp = io[0x47];

        let win_y = self.ly as usize - wy;
        let tile_row = (win_y >> 3) & 31;
        let row_in_tile = win_y & 7;

        for (win_col, px) in (wx.max(0) as usize..SCREEN_W).enumerate() {
            let tile_col = (win_col >> 3) & 31;
            let addr = map_base + tile_row * 32 + tile_col;
            let tile_index = vram[addr];
            let attr = if self.cgb { vram[0x2000 + addr] } else { 0 };
            let bank = if self.cgb {
                (attr as usize >> 3) & 1
            } else {
                0
            };
            let pal = if self.cgb { attr & 7 } else { 0 };
            let row = if self.cgb && attr & 0x40 != 0 {
                7 - row_in_tile as u8
            } else {
                row_in_tile as u8
            };
            let pixels = *cache[bank * 256 + tile_index as usize]
                .get_or_insert_with(|| self.tile_pixels(vram, io, tile_index, row, bank));
            let col = if self.cgb && attr & 0x20 != 0 {
                7 - (win_col & 7) as u8
            } else {
                (win_col & 7) as u8
            };
            let cv = pixels[col as usize];
            bg_color[px] = cv;
            bg_pal[px] = pal;
            bg_prio[px] = if self.cgb { attr >> 7 } else { 0 };
            if self.cgb {
                bg_shade[px] = cv;
            } else {
                bg_shade[px] = (bgp >> (cv * 2)) & 3;
            }
        }
    }

    fn render_sprites(
        &self,
        io: &[u8; 0x80],
        vram: &[u8; 0x4000],
        bg_color: &[u8; SCREEN_W],
        bg_prio: &[u8; SCREEN_W],
        shade: &mut [u8; SCREEN_W],
        pal: &mut [u8; SCREEN_W],
    ) {
        let mut sprites = [self.line_sprites[0]; 10];
        sprites[..self.line_sprite_count]
            .copy_from_slice(&self.line_sprites[..self.line_sprite_count]);
        sprites[..self.line_sprite_count].sort_by_key(|s| s.x);

        let lcdc = io[0x40];
        let bg_enabled = lcdc & 0x01 != 0;

        // Higher-priority sprites (smaller x; on an x tie, earlier in OAM) must
        // be drawn on top, so iterate from the back of the sorted list to the
        // front, letting the front-most sprite overwrite the others.
        for i in (0..self.line_sprite_count).rev() {
            let s = sprites[i];
            let flip_x = s.attr & 0x20 != 0;
            let flip_y = s.attr & 0x40 != 0;
            let priority = s.attr & 0x80 != 0;
            // OBJ palette: CGB uses attr bits 0-2; DMG selects OBJ palette 0/1
            // via attr bit 4 (mapped onto CGB OBJ palettes 0/1).
            let pal_num = if self.cgb {
                8 + (s.attr & 7)
            } else if s.attr & 0x10 != 0 {
                9
            } else {
                8
            };

            let top = s.y as i16 - 16;
            let row = self.ly as i16 - top;
            let mut tile = s.tile;
            if s.height == 16 {
                if flip_y {
                    if row < 8 {
                        tile = (s.tile & !1) + 1;
                    }
                } else if row >= 8 {
                    tile = (s.tile & !1) + 1;
                }
            }
            let tile_row = if flip_y { 7 - (row & 7) } else { row & 7 };
            // Sprite tiles are always indexed from 0x8000 (unsigned), independent of LCDC bit 4.
            let bank = if self.cgb {
                (s.attr as usize >> 3) & 1
            } else {
                0
            };
            let base = ((0x8000 + tile as usize * 16) & 0x1FFF) | (bank * 0x2000);
            let lo = vram[base + tile_row as usize * 2];
            let hi = vram[base + tile_row as usize * 2 + 1];
            let mut pixels = [0u8; 8];
            for (px, c) in pixels.iter_mut().enumerate() {
                let bit = 7 - px;
                *c = ((hi >> bit) & 1) << 1 | ((lo >> bit) & 1);
            }

            let obp = if s.attr & 0x10 != 0 {
                io[0x49]
            } else {
                io[0x48]
            };
            for p in 0..8 {
                let px = s.x + p as i16;
                if px < 0 || px >= SCREEN_W as i16 {
                    continue;
                }
                let col = if flip_x { 7 - p } else { p };
                let cv = pixels[col as usize];
                if cv == 0 {
                    continue;
                }
                // BG priority: in CGB, an attr BG-priority tile hides OBJ; in
                // DMG-mode, the OBJ priority bit does so when BG is non-zero.
                if self.cgb {
                    if bg_prio[px as usize] != 0 && bg_color[px as usize] != 0 {
                        continue;
                    }
                } else if bg_enabled && priority && bg_color[px as usize] != 0 {
                    continue;
                }
                if self.cgb {
                    shade[px as usize] = cv;
                } else {
                    shade[px as usize] = (obp >> (cv * 2)) & 3;
                }
                pal[px as usize] = pal_num;
            }
        }
    }

    fn render_line(&mut self, io: &mut [u8; 0x80], vram: &mut [u8; 0x4000], _oam: &mut [u8; 0xA0]) {
        let lcdc = io[0x40];
        let mut bg_color = [0u8; SCREEN_W];
        let mut bg_shade = [0u8; SCREEN_W];
        let mut bg_pal = [0u8; SCREEN_W];
        let mut bg_prio = [0u8; SCREEN_W];
        let mut shade = [0u8; SCREEN_W];
        let mut pal = [0u8; SCREEN_W];
        let mut tile_cache = [None; 512];

        if lcdc & 0x01 != 0 {
            self.render_bg(
                io,
                vram,
                &mut bg_color,
                &mut bg_shade,
                &mut bg_pal,
                &mut bg_prio,
                &mut tile_cache,
            );
        }
        if lcdc & 0x40 != 0 {
            self.render_window(
                io,
                vram,
                &mut bg_color,
                &mut bg_shade,
                &mut bg_pal,
                &mut bg_prio,
                &mut tile_cache,
            );
        }
        shade.copy_from_slice(&bg_shade);
        pal.copy_from_slice(&bg_pal);
        if lcdc & 0x02 != 0 {
            self.render_sprites(io, vram, &bg_color, &bg_prio, &mut shade, &mut pal);
        }

        let row = self.ly as usize * SCREEN_W;
        for px in 0..SCREEN_W {
            self.frame_buffer[row + px] = shade[px];
            let c = self.palette_rgb(pal[px], shade[px]);
            let o = (row + px) * 3;
            self.rgb_buffer[o] = c[0];
            self.rgb_buffer[o + 1] = c[1];
            self.rgb_buffer[o + 2] = c[2];
        }
    }
}

impl Default for Ppu {
    fn default() -> Self {
        Self::new()
    }
}

impl emu_core::device::Device for Ppu {
    fn kind(&self) -> &'static str {
        "PPU"
    }

    fn reset(&mut self) {
        *self = Ppu::new();
    }

    fn tick(&mut self, cycles: u32, bus: &mut dyn emu_core::bus::Bus) {
        if let Some(gb) = bus.as_any_mut().downcast_mut::<crate::bus::Bus>() {
            self.step(cycles, &mut gb.io, &mut gb.vram, &mut gb.oam);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stepping_ppu() -> ([u8; 0x80], [u8; 0x4000], [u8; 0xA0], Ppu) {
        let mut io = [0u8; 0x80];
        io[0x40] = 0x80; // LCD on
        let vram = [0u8; 0x4000];
        let oam = [0u8; 0xA0];
        (io, vram, oam, Ppu::new())
    }

    #[test]
    fn stat_oam_interrupt_fires_on_mode_two_entry() {
        let (mut io, mut vram, mut oam, mut ppu) = stepping_ppu();
        io[0x41] = 0x20; // enable OAM (mode 2) STAT interrupt
        io[0x0F] = 0;
        ppu.step(456, &mut io, &mut vram, &mut oam); // complete the first line
        assert_ne!(
            io[0x0F] & 0x02,
            0,
            "OAM STAT interrupt fires entering mode 2"
        );
    }

    #[test]
    fn stat_oam_interrupt_does_not_fire_when_disabled() {
        let (mut io, mut vram, mut oam, mut ppu) = stepping_ppu();
        io[0x41] = 0x00; // no STAT interrupts enabled
        io[0x0F] = 0;
        ppu.step(456, &mut io, &mut vram, &mut oam);
        assert_eq!(io[0x0F] & 0x02, 0, "no STAT interrupt when disabled");
    }

    #[test]
    fn stat_lyc_interrupt_fires_on_coincidence_edge() {
        let (mut io, mut vram, mut oam, mut ppu) = stepping_ppu();
        io[0x45] = 1; // LYC = 1
        io[0x41] = 0x40; // enable LYC STAT interrupt
        io[0x0F] = 0;
        ppu.step(456, &mut io, &mut vram, &mut oam); // LY goes 0 -> 1
        assert_ne!(
            io[0x0F] & 0x02,
            0,
            "LYC STAT interrupt fires on coincidence edge"
        );
    }
}
