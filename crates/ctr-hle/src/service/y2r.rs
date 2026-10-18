//! The YUV-to-RGB converter, `y2r:u` (3dbrew, "Camera Services", the y2r:u
//! section, and its command pages).
//!
//! Y2R is a block of hardware that turns YUV pictures (a camera's, or a
//! video decoder's) into RGB ones. A client describes the picture (input
//! format, line width and line count, the coefficients of the conversion,
//! alpha, the output format, rotation and whether the output is lines or
//! 8x8 tiles), names the buffers the DMA reads the planes from and writes
//! the result to, starts the conversion and waits for the transfer-end
//! event. Pictures go through in strips of eight lines.
//!
//! Each buffer is a DMA transfer: `unit` bytes, then a gap of `gap` bytes
//! skipped, again and again until `size` bytes have moved. The planes are
//! gathered that way, converted, and the result scattered the same way.
//!
//! The arithmetic: each coefficient has eight fraction bits, the offsets
//! three more, so for a sample (Y, U, V), with Y_A..B_Offset the
//! coefficients,
//!
//! ```text
//! R = ((Y*Y_A + V*R_V) >> 3) + R_Offset
//! G = ((Y*Y_A - V*G_V - U*G_U) >> 3) + G_Offset
//! B = ((Y*Y_A + U*B_U) >> 3) + B_Offset
//! ```
//!
//! and each channel is `(channel + 0x18) >> 5` clamped to 0..255. The
//! rounding term 0x18 is what other emulators found to match captures of
//! the hardware. Dithering, which only changes the low bits of the 16-bit
//! formats, is not done.

use crate::horizon::Horizon;
use crate::ipc::{Reply, Request};
use crate::kernel::ObjId;
use crate::result;
use crate::service::table::{event_handle, new_event, words_of};
use crate::service::Service;

/// Input formats.
const YUV422_INDIV_8: u8 = 0;
const YUV420_INDIV_8: u8 = 1;
const YUV422_INDIV_16: u8 = 2;
const YUV420_INDIV_16: u8 = 3;
const YUV422_BATCH: u8 = 4;
/// Output formats.
const RGB_32: u8 = 0;
const RGB_24: u8 = 1;
const RGB_16_555: u8 = 2;
/// Block alignment: 8x8 tiles in Morton order, as GPU textures are.
const BLOCK_8_BY_8: u8 = 1;

/// The standard coefficients, as GetStandardCoefficientParams gives them:
/// Y_A, R_V, G_V, G_U, B_U, R_Offset, G_Offset, B_Offset. They are the
/// ITU-R BT.601 and BT.709 matrices, for full-range ("PC") samples and for
/// studio-range ("scaling") ones, in the fixed point above; for example
/// BT.601's R_V is 1.402 * 256 and its R_Offset is -1.402 * 128 * 32.
const STANDARD: [[i16; 8]; 4] = [
    [0x100, 0x166, 0xB6, 0x58, 0x1C5, -0x166F, 0x10EE, -0x1C5B],
    [0x100, 0x193, 0x77, 0x2F, 0x1DB, -0x1933, 0xA7C, -0x1D51],
    [0x12A, 0x198, 0xD0, 0x64, 0x204, -0x1BDE, 0x10F2, -0x229B],
    [0x12A, 0x1CA, 0x88, 0x36, 0x21C, -0x1F04, 0x99C, -0x2421],
];

/// A DMA transfer of a plane or of the result.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Transfer {
    pub addr: u32,
    pub size: u32,
    pub unit: u32,
    pub gap: i32,
}

impl Transfer {
    /// From a SetSending or SetReceiving request: address, size, unit and
    /// gap (both signed 16-bit).
    fn of(request: &Request) -> Self {
        Transfer {
            addr: request.word(1),
            size: request.word(2),
            unit: request.word(3) as i16 as i32 as u32,
            gap: request.word(4) as i16 as i32,
        }
    }

    /// The runs of the transfer: address and length of each unit.
    fn runs(&self) -> Vec<(u32, u32)> {
        let unit = if self.unit == 0 || self.unit > self.size {
            self.size
        } else {
            self.unit
        };
        let mut runs = Vec::new();
        let (mut addr, mut moved) = (self.addr, 0);
        while moved < self.size && unit != 0 {
            let len = unit.min(self.size - moved);
            runs.push((addr, len));
            moved += len;
            addr = addr.wrapping_add(len).wrapping_add(self.gap as u32);
        }
        runs
    }

    fn gather(&self, h: &Horizon) -> Vec<u8> {
        let mut data = Vec::with_capacity(self.size as usize);
        for (addr, len) in self.runs() {
            match h.process.space.read_bytes(&h.mem, addr, len as usize) {
                Some(bytes) => data.extend(bytes),
                None => data.resize(data.len() + len as usize, 0),
            }
        }
        data
    }

    fn scatter(&self, h: &mut Horizon, data: &[u8]) {
        let mut at = 0;
        for (addr, len) in self.runs() {
            let end = (at + len as usize).min(data.len());
            if at >= end {
                break;
            }
            h.process
                .space
                .write_bytes(&mut h.mem, addr, &data[at..end]);
            at = end;
        }
    }
}

/// What a conversion needs to know.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Params {
    pub input_format: u8,
    pub output_format: u8,
    pub rotation: u8,
    pub block_alignment: u8,
    pub line_width: u16,
    pub lines: u16,
    pub coefficients: [i16; 8],
    pub alpha: u16,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            input_format: YUV422_INDIV_8,
            output_format: RGB_32,
            rotation: 0,
            block_alignment: 0,
            line_width: 0,
            lines: 0,
            coefficients: STANDARD[0],
            alpha: 0,
        }
    }
}

/// The planes as gathered: Y, U and V apart, or the batch of all three.
#[derive(Default)]
pub struct Planes {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub batch: Vec<u8>,
}

/// One channel from its sum, rounded and clamped.
fn channel(sum: i32) -> u8 {
    ((sum + 0x18) >> 5).clamp(0, 255) as u8
}

/// The offset of pixel (x, y) in an 8x8 tile in Morton order.
fn morton(x: usize, y: usize) -> usize {
    (x & 1) | (y & 1) << 1 | (x & 2) << 1 | (y & 2) << 2 | (x & 4) << 2 | (y & 4) << 3
}

impl Params {
    fn bytes_per_pixel(&self) -> usize {
        match self.output_format {
            RGB_32 => 4,
            RGB_24 => 3,
            _ => 2,
        }
    }

    /// The Y, U and V samples of pixel (x, y); zero past the planes' end.
    fn sample(&self, p: &Planes, x: usize, y: usize) -> (i32, i32, i32) {
        let w = self.line_width as usize;
        let at = |plane: &[u8], i: usize| plane.get(i).copied().unwrap_or(0) as i32;
        let chroma = (x / 2, if self.input_format & 1 != 0 { y / 2 } else { y });
        match self.input_format {
            YUV422_INDIV_8 | YUV420_INDIV_8 => {
                let c = chroma.1 * (w / 2) + chroma.0;
                (at(&p.y, y * w + x), at(&p.u, c), at(&p.v, c))
            }
            YUV422_INDIV_16 | YUV420_INDIV_16 => {
                // Each sample in the low byte of a halfword.
                let c = chroma.1 * (w / 2) + chroma.0;
                (at(&p.y, 2 * (y * w + x)), at(&p.u, 2 * c), at(&p.v, 2 * c))
            }
            _ => {
                // Y0 U Y1 V for each pair of pixels.
                let pair = 2 * (y * w + (x & !1));
                (
                    at(&p.batch, 2 * (y * w + x)),
                    at(&p.batch, pair + 1),
                    at(&p.batch, pair + 3),
                )
            }
        }
    }

    /// Pixel (x, y) in the output format.
    fn pixel(&self, p: &Planes, x: usize, y: usize, out: &mut Vec<u8>) {
        let (sy, su, sv) = self.sample(p, x, y);
        let c = self.coefficients.map(i32::from);
        let luma = sy * c[0];
        let r = channel(((luma + sv * c[1]) >> 3) + c[5]);
        let g = channel(((luma - sv * c[2] - su * c[3]) >> 3) + c[6]);
        let b = channel(((luma + su * c[4]) >> 3) + c[7]);
        let (r5, g5, b5) = ((r >> 3) as u16, (g >> 3) as u16, (b >> 3) as u16);
        match self.output_format {
            RGB_32 => out.extend([self.alpha as u8, b, g, r]),
            RGB_24 => out.extend([b, g, r]),
            RGB_16_555 => {
                let a = self.alpha >> 7 & 1;
                out.extend((r5 << 11 | g5 << 6 | b5 << 1 | a).to_le_bytes());
            }
            _ => out.extend((r5 << 11 | ((g >> 2) as u16) << 5 | b5).to_le_bytes()),
        }
    }

    /// The whole picture, strip by strip, as the output DMA receives it.
    pub fn convert(&self, p: &Planes) -> Vec<u8> {
        let (w, lines) = (self.line_width as usize, self.lines as usize);
        let bpp = self.bytes_per_pixel();
        let mut out = Vec::with_capacity(w * lines * bpp);
        for top in (0..lines).step_by(8) {
            let rows = (lines - top).min(8);
            // The strip, eight rows of the picture, turned as asked.
            let (rw, rh) = match self.rotation {
                1 | 3 => (8, w),
                _ => (w, 8),
            };
            let source = |i: usize, j: usize| -> (usize, usize) {
                // Row i, column j of the turned strip: which pixel.
                match self.rotation {
                    1 => (i, 7 - j),
                    2 => (w - 1 - j, 7 - i),
                    3 => (w - 1 - i, j),
                    _ => (j, i),
                }
            };
            let put = |i: usize, j: usize, out: &mut Vec<u8>| {
                let (x, y) = source(i, j);
                self.pixel(p, x, top + y, out);
            };
            if self.block_alignment == BLOCK_8_BY_8 {
                let mut tile = vec![0u8; 64 * bpp];
                for ti in (0..rh).step_by(8) {
                    for tj in (0..rw).step_by(8) {
                        for y in 0..8 {
                            for x in 0..8 {
                                let mut px = Vec::with_capacity(bpp);
                                if ti + y < rh && tj + x < rw {
                                    put(ti + y, tj + x, &mut px);
                                } else {
                                    px.resize(bpp, 0);
                                }
                                let at = morton(x, y) * bpp;
                                tile[at..at + bpp].copy_from_slice(&px);
                            }
                        }
                        out.extend_from_slice(&tile);
                    }
                }
            } else {
                // Lines: only the rows the picture has, unturned.
                let rh = if self.rotation & 1 == 0 { rows } else { rh };
                for i in 0..rh {
                    for j in 0..rw {
                        put(i, j, &mut out);
                    }
                }
            }
        }
        out
    }
}

#[derive(Default)]
pub struct Y2r {
    params: Params,
    standard: u8,
    spacial_dithering: u32,
    temporal_dithering: u32,
    transfer_end_interrupt: bool,
    dithering_weights: [u32; 8],
    y: Transfer,
    u: Transfer,
    v: Transfer,
    batch: Transfer,
    output: Transfer,
    busy: bool,
    finished: bool,
    transfer_end: Option<ObjId>,
    /// The number of the conversion under way, the tag of its end.
    conversion: u32,
}

/// How long the hardware takes. Not measured: an estimate of the block and
/// its DMA moving a pixel every four ARM11 cycles, so a 400x240 picture
/// takes about 1.4 ms. Changing it is a timing change.
pub mod timing {
    pub const CYCLES_PER_PIXEL: u64 = 4;
}

impl Y2r {
    /// Start a conversion: it ends after its cost.
    fn start(&mut self, h: &mut Horizon) {
        self.busy = true;
        self.finished = false;
        self.conversion = self.conversion.wrapping_add(1);
        let pixels = self.params.line_width as u64 * self.params.lines as u64;
        let cost = (pixels * timing::CYCLES_PER_PIXEL).max(1);
        h.schedule_service("y2r:u", cost, self.conversion);
    }

    /// Convert the picture and announce the end of the transfer.
    fn convert(&mut self, h: &mut Horizon) {
        let planes = if self.params.input_format == YUV422_BATCH {
            Planes {
                batch: self.batch.gather(h),
                ..Planes::default()
            }
        } else {
            Planes {
                y: self.y.gather(h),
                u: self.u.gather(h),
                v: self.v.gather(h),
                batch: Vec::new(),
            }
        };
        let out = self.params.convert(&planes);
        self.output.scatter(h, &out);
        if h.trace & crate::trace::SERVICE != 0 {
            h.note(format!(
                "y2r: {}x{} lines, format {} to {}, rotation {}, block {} -> {:#010x}",
                self.params.line_width,
                self.params.lines,
                self.params.input_format,
                self.params.output_format,
                self.params.rotation,
                self.params.block_alignment,
                self.output.addr
            ));
        }
        self.finish(h);
    }

    fn finish(&mut self, h: &mut Horizon) {
        self.busy = false;
        self.finished = true;
        if self.transfer_end_interrupt {
            let id = *self.transfer_end.get_or_insert_with(|| new_event(h));
            h.kernel.signal(id);
        }
    }
}

impl Service for Y2r {
    fn name(&self) -> &str {
        "y2r:u"
    }

    fn call(&mut self, h: &mut Horizon, _object: u32, request: &Request) -> Reply {
        let command = request.command();
        let ok = || Reply::new(command, result::SUCCESS);
        let value = request.word(1);
        let p = &mut self.params;
        match command {
            0x01 => {
                p.input_format = value as u8;
                ok()
            }
            0x02 => ok().word(p.input_format as u32),
            0x03 => {
                p.output_format = value as u8;
                ok()
            }
            0x04 => ok().word(p.output_format as u32),
            0x05 => {
                p.rotation = value as u8 & 3;
                ok()
            }
            0x06 => ok().word(p.rotation as u32),
            0x07 => {
                p.block_alignment = value as u8;
                ok()
            }
            0x08 => ok().word(p.block_alignment as u32),
            0x09 => {
                self.spacial_dithering = value & 0xFF;
                ok()
            }
            0x0A => ok().word(self.spacial_dithering),
            0x0B => {
                self.temporal_dithering = value & 0xFF;
                ok()
            }
            0x0C => ok().word(self.temporal_dithering),
            0x0D => {
                self.transfer_end_interrupt = value & 0xFF != 0;
                ok()
            }
            0x0E => ok().word(self.transfer_end_interrupt as u32),
            0x0F => {
                let handle = event_handle(h, &mut self.transfer_end);
                ok().handles(&[handle], false)
            }
            // SetSendingY, SetSendingU, SetSendingV, SetSendingYUYV.
            0x10 => {
                self.y = Transfer::of(request);
                ok()
            }
            0x11 => {
                self.u = Transfer::of(request);
                ok()
            }
            0x12 => {
                self.v = Transfer::of(request);
                ok()
            }
            0x13 => {
                self.batch = Transfer::of(request);
                ok()
            }
            // IsFinishedSendingYuv, Y, U, V.
            0x14..=0x17 => ok().word(self.finished as u32),
            0x18 => {
                self.output = Transfer::of(request);
                ok()
            }
            0x19 => ok().word(self.finished as u32),
            0x1A => {
                p.line_width = value as u16;
                ok()
            }
            0x1B => ok().word(p.line_width as u32),
            0x1C => {
                p.lines = value as u16;
                ok()
            }
            0x1D => ok().word(p.lines as u32),
            // SetCoefficientParams: eight 16-bit values in four words.
            0x1E => {
                for n in 0..4 {
                    let w = request.word(1 + n);
                    p.coefficients[2 * n] = w as i16;
                    p.coefficients[2 * n + 1] = (w >> 16) as i16;
                }
                ok()
            }
            0x1F => {
                let bytes: Vec<u8> = p
                    .coefficients
                    .iter()
                    .flat_map(|c| c.to_le_bytes())
                    .collect();
                ok().words(&words_of(&bytes))
            }
            0x20 => {
                self.standard = value as u8 & 3;
                p.coefficients = STANDARD[self.standard as usize];
                ok()
            }
            0x21 => {
                let standard = STANDARD[value as usize & 3];
                let bytes: Vec<u8> = standard.iter().flat_map(|c| c.to_le_bytes()).collect();
                ok().words(&words_of(&bytes))
            }
            0x22 => {
                p.alpha = value as u16;
                ok()
            }
            0x23 => ok().word(p.alpha as u32),
            0x24 => {
                self.dithering_weights = std::array::from_fn(|n| request.word(1 + n));
                ok()
            }
            0x25 => ok().words(&self.dithering_weights),
            0x26 => {
                self.start(h);
                ok()
            }
            0x27 => {
                self.busy = false;
                ok()
            }
            0x28 => ok().word(self.busy as u32),
            // SetPackageParameter: formats, rotation and alignment; width and
            // lines; standard coefficient and alpha.
            0x29 => {
                let [a, b, c] = [request.word(1), request.word(2), request.word(3)];
                p.input_format = a as u8;
                p.output_format = (a >> 8) as u8;
                p.rotation = (a >> 16) as u8 & 3;
                p.block_alignment = (a >> 24) as u8;
                p.line_width = b as u16;
                p.lines = (b >> 16) as u16;
                self.standard = c as u8 & 3;
                p.coefficients = STANDARD[self.standard as usize];
                p.alpha = (c >> 16) as u16;
                ok()
            }
            // PingProcess: no other process is connected.
            0x2A => ok().word(0),
            // DriverInitialize, DriverFinalize.
            0x2B | 0x2C => {
                if command == 0x2B {
                    *p = Params::default();
                }
                ok()
            }
            0x2D => {
                let a = p.input_format as u32
                    | (p.output_format as u32) << 8
                    | (p.rotation as u32) << 16
                    | (p.block_alignment as u32) << 24;
                let b = p.line_width as u32 | (p.lines as u32) << 16;
                let c = self.standard as u32 | (p.alpha as u32) << 16;
                ok().words(&[a, b, c])
            }
            _ => h.unhandled("y2r:u", request),
        }
    }

    /// The end of conversion `tag`, unless it was stopped or another one
    /// started since.
    fn event(&mut self, h: &mut Horizon, tag: u32) {
        if self.busy && tag == self.conversion {
            self.convert(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::testing::{
        connect, console, event, read_bytes, run_for, send, write_bytes, HEAP,
    };

    fn gray(params: &Params, y: u8) -> Vec<u8> {
        let n = params.line_width as usize * params.lines as usize;
        let planes = Planes {
            y: vec![y; n],
            u: vec![128; n],
            v: vec![128; n],
            batch: Vec::new(),
        };
        params.convert(&planes)
    }

    #[test]
    fn grey_stays_grey_and_the_standards_differ_in_range() {
        let mut p = Params {
            line_width: 8,
            lines: 8,
            alpha: 0xFF,
            ..Params::default()
        };
        // The coefficients are rounded, so grey comes out a step off in
        // green.
        assert_eq!(gray(&p, 128)[..4], [0xFF, 128, 129, 128]);
        assert_eq!(gray(&p, 0)[1..4], [0, 1, 0]);
        // Studio range: 16 is black and 235 white.
        p.coefficients = STANDARD[2];
        assert_eq!(gray(&p, 16)[1..4], [0, 0, 0]);
        assert_eq!(gray(&p, 235)[1..4], [255, 255, 255]);
        // Pure red in BT.601: V high, U low.
        let planes = Planes {
            y: vec![76; 64],
            u: vec![85; 64],
            v: vec![255; 64],
            batch: Vec::new(),
        };
        p.coefficients = STANDARD[0];
        let px = &p.convert(&planes)[..4];
        assert!(px[3] > 250 && px[2] < 5 && px[1] < 5, "{px:?}");
    }

    #[test]
    fn the_input_formats_find_their_chroma() {
        // A pixel pair whose chroma makes it red, then one whose makes it
        // blue, in each layout of an 4x2 picture.
        let p = |input_format| Params {
            input_format,
            line_width: 4,
            lines: 2,
            output_format: RGB_24,
            ..Params::default()
        };
        let red_then_blue = |out: Vec<u8>| {
            let px: Vec<&[u8]> = out.chunks(3).collect();
            px[1][2] > 200 && px[1][0] < 60 && px[2][0] > 200 && px[2][2] < 60
        };
        let batch = Planes {
            batch: [76, 85, 76, 255, 29, 255, 29, 107].repeat(2),
            ..Planes::default()
        };
        assert!(red_then_blue(p(YUV422_BATCH).convert(&batch)));
        let planes = |rows: usize, step: usize| Planes {
            y: [76, 76, 29, 29]
                .repeat(2)
                .iter()
                .flat_map(|&s| [s, 0][..step].to_vec())
                .collect(),
            u: [85, 255]
                .repeat(rows)
                .iter()
                .flat_map(|&s| [s, 0][..step].to_vec())
                .collect(),
            v: [255, 107]
                .repeat(rows)
                .iter()
                .flat_map(|&s| [s, 0][..step].to_vec())
                .collect(),
            batch: Vec::new(),
        };
        assert!(red_then_blue(p(YUV422_INDIV_8).convert(&planes(2, 1))));
        assert!(red_then_blue(p(YUV420_INDIV_8).convert(&planes(1, 1))));
        assert!(red_then_blue(p(YUV422_INDIV_16).convert(&planes(2, 2))));
        assert!(red_then_blue(p(YUV420_INDIV_16).convert(&planes(1, 2))));
    }

    #[test]
    fn tiles_are_in_morton_order_and_rotation_turns_each_strip() {
        // A 16x8 picture whose Y is its column number, in RGB24.
        let p = Params {
            line_width: 16,
            lines: 8,
            output_format: RGB_24,
            block_alignment: BLOCK_8_BY_8,
            ..Params::default()
        };
        let planes = Planes {
            y: (0..128).map(|i| (i % 16) as u8 * 16).collect(),
            u: vec![128; 64],
            v: vec![128; 64],
            batch: Vec::new(),
        };
        let out = p.convert(&planes);
        assert_eq!(out.len(), 128 * 3);
        // Morton index 1 is (1, 0), index 2 is (0, 1): columns 1 and 0.
        assert_eq!((out[3], out[6]), (16, 0));
        // The second tile starts at column 8.
        assert_eq!(out[64 * 3], 128);
        // Turned a quarter clockwise in lines: an 8-wide, 16-tall strip
        // whose first row is the picture's first column, bottom up.
        let turned = Params {
            rotation: 1,
            block_alignment: 0,
            ..p
        };
        let out = turned.convert(&planes);
        assert_eq!(out.len(), 128 * 3);
        assert!(out.chunks(3).take(8).all(|px| px[0] == 0));
        assert!(out.chunks(3).skip(8).take(8).all(|px| px[0] == 16));
    }

    #[test]
    fn a_conversion_gathers_scatters_and_signals_the_end() {
        let mut h = console();
        let y2r = connect(&mut h, "y2r:u");
        // 8x8 YUV422 in separate planes, to RGB565 lines.
        write_bytes(&mut h, HEAP, &[235; 64]);
        write_bytes(&mut h, HEAP + 0x100, &[128; 32]);
        write_bytes(&mut h, HEAP + 0x200, &[128; 32]);
        for words in [
            [0x0029_01C0, 0x0000_0300, 0x0008_0008, 0xFF_0002],
            [0x000D_0040, 1, 0, 0],
        ] {
            assert_eq!(send(&mut h, y2r, &words)[1], 0);
        }
        let event_reply = send(&mut h, y2r, &[0x000F_0000]);
        let end = event_reply[3];
        for (command, addr, size) in [
            (0x10, HEAP, 64),
            (0x11, HEAP + 0x100, 32),
            (0x12, HEAP + 0x200, 32),
        ] {
            let header = (command as u32) << 16 | 0x0102;
            send(&mut h, y2r, &[header, addr, size, 8, 0, 0, 0xFFFF_8001]);
        }
        // The output, two lines at a time with a 16-byte gap after each.
        send(
            &mut h,
            y2r,
            &[0x0018_0102, HEAP + 0x400, 128, 32, 16, 0, 0xFFFF_8001],
        );
        assert_eq!(send(&mut h, y2r, &[0x0026_0000]), [0x0026_0040, 0]);
        // Busy for the picture's cost, then done.
        assert_eq!(send(&mut h, y2r, &[0x0028_0000])[2], 1);
        assert_eq!(event(&h, end), Some(false));
        run_for(
            &mut h,
            64 * timing::CYCLES_PER_PIXEL + 2 * crate::horizon::QUANTUM,
        );
        assert_eq!(event(&h, end), Some(true));
        assert_eq!(send(&mut h, y2r, &[0x0028_0000])[2], 0);
        assert_eq!(send(&mut h, y2r, &[0x0019_0000])[2], 1);
        // White, and the gap untouched.
        assert_eq!(read_bytes(&h, HEAP + 0x400, 2), [0xFF, 0xFF]);
        assert_eq!(read_bytes(&h, HEAP + 0x400 + 32, 2), [0, 0]);
        assert_eq!(read_bytes(&h, HEAP + 0x400 + 48, 2), [0xFF, 0xFF]);
        assert!(h.log.is_empty(), "{:?}", h.log);
    }
}
