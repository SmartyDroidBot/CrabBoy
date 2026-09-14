//! Rasterise `assets/logo.svg` into the icon files the frontends embed.
//!
//! ```text
//! cargo run -p crab-cli --features logo-tools --bin render_logo
//! ```
//!
//! Writes `assets/logo.png` (the full artwork), the square icon crop of the
//! crab and console (no wordmark) as `assets/icon-256.png`,
//! `assets/icon-64.png`, `assets/icon.ico`, and the web icons under
//! `platforms/wasm/web/`. Pure Rust (resvg + ico); run from the repo root.

use std::path::Path;

const SVG: &str = "assets/logo.svg";
const LOGO_PNG: &str = "assets/logo.png";
const LOGO_SIZE: u32 = 512;
/// Master render size of the icon crop, downscaled to every icon size.
const ICON_MASTER: u32 = 1024;
/// Padding around the artwork inside the square icon, as a fraction.
const ICON_PAD: f32 = 0.06;
const ICON_PNGS: &[(&str, u32)] = &[
    ("assets/icon-256.png", 256),
    ("assets/icon-64.png", 64),
    ("platforms/wasm/web/favicon-32.png", 32),
    ("platforms/wasm/web/icon-192.png", 192),
    ("platforms/wasm/web/apple-touch-icon.png", 180),
];
const ICO_PATH: &str = "assets/icon.ico";
const ICO_SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("render_logo: {msg}");
    std::process::exit(1)
}

/// An RGBA8 image with straight (non-premultiplied) alpha.
struct Rgba {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Rgba {
    fn from_pixmap(p: &resvg::tiny_skia::Pixmap) -> Rgba {
        let mut px = Vec::with_capacity(p.data().len());
        for c in p.pixels() {
            let c = c.demultiply();
            px.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
        }
        Rgba {
            w: p.width(),
            h: p.height(),
            px,
        }
    }

    fn alpha(&self, x: u32, y: u32) -> u8 {
        self.px[((y * self.w + x) * 4 + 3) as usize]
    }

    /// Area-average downscale (exact when `self.w` is a multiple of `w`).
    fn downscale(&self, w: u32, h: u32) -> Rgba {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            let y0 = y * self.h / h;
            let y1 = ((y + 1) * self.h / h).max(y0 + 1);
            for x in 0..w {
                let x0 = x * self.w / w;
                let x1 = ((x + 1) * self.w / w).max(x0 + 1);
                // Average premultiplied colour so transparent pixels do not
                // bleed their (undefined) colour into the edge.
                let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let i = ((sy * self.w + sx) * 4) as usize;
                        let pa = self.px[i + 3] as u64;
                        r += self.px[i] as u64 * pa;
                        g += self.px[i + 1] as u64 * pa;
                        b += self.px[i + 2] as u64 * pa;
                        a += pa;
                        n += 1;
                    }
                }
                // A fully transparent block has r = g = b = 0 already.
                let d = a.max(1);
                out.extend_from_slice(&[
                    ((r + d / 2) / d) as u8,
                    ((g + d / 2) / d) as u8,
                    ((b + d / 2) / d) as u8,
                    ((a + n / 2) / n) as u8,
                ]);
            }
        }
        Rgba { w, h, px: out }
    }

    fn encode_png(&self) -> Vec<u8> {
        let mut p = resvg::tiny_skia::Pixmap::new(self.w, self.h).unwrap();
        for (dst, src) in p.pixels_mut().iter_mut().zip(self.px.as_chunks::<4>().0) {
            *dst =
                resvg::tiny_skia::ColorU8::from_rgba(src[0], src[1], src[2], src[3]).premultiply();
        }
        p.encode_png()
            .unwrap_or_else(|e| fail(format!("png encode: {e}")))
    }
}

/// Render the square `view` (x, y, side in SVG units) at `size` pixels.
/// Rows at or below `clip_y` (SVG units) are cleared, which keeps the
/// wordmark out of the icon crop.
fn render(tree: &resvg::usvg::Tree, size: u32, view: (f32, f32, f32), clip_y: Option<f32>) -> Rgba {
    let (x0, y0, side) = view;
    let scale = size as f32 / side;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
    let t = resvg::tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, -x0 * scale, -y0 * scale);
    resvg::render(tree, t, &mut pixmap.as_mut());
    if let Some(clip_y) = clip_y {
        let first = ((clip_y - y0) * scale).max(0.0) as usize;
        let row = size as usize * 4;
        if first * row < pixmap.data().len() {
            pixmap.data_mut()[first * row..].fill(0);
        }
    }
    Rgba::from_pixmap(&pixmap)
}

/// Square region (x, y, side) in SVG units around the artwork above the
/// wordmark: the wordmark is the band of rows below the first fully
/// transparent row found scanning upward from 70 % of the height.
fn icon_view(full: &Rgba, svg_side: f32) -> ((f32, f32, f32), f32) {
    let row_empty = |y: u32| (0..full.w).all(|x| full.alpha(x, y) == 0);
    let mut art_bottom = full.h * 7 / 10;
    while art_bottom > 0 && !row_empty(art_bottom) {
        art_bottom -= 1;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (full.w, full.h, 0, 0);
    for y in 0..art_bottom {
        for x in 0..full.w {
            if full.alpha(x, y) != 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x0 >= x1 || y0 >= y1 {
        fail("could not find the artwork bounding box");
    }
    let unit = svg_side / full.w as f32;
    let (bx, by) = (x0 as f32 * unit, y0 as f32 * unit);
    let (bw, bh) = ((x1 - x0 + 1) as f32 * unit, (y1 - y0 + 1) as f32 * unit);
    let side = bw.max(bh) * (1.0 + 2.0 * ICON_PAD);
    let cx = bx + bw / 2.0;
    let cy = by + bh / 2.0;
    println!(
        "artwork bbox: x={bx:.0} y={by:.0} w={bw:.0} h={bh:.0} (wordmark starts at row {art_bottom}); icon view: x={:.0} y={:.0} side={side:.0}",
        cx - side / 2.0,
        cy - side / 2.0
    );
    (
        (cx - side / 2.0, cy - side / 2.0, side),
        art_bottom as f32 * unit,
    )
}

fn write(path: &str, bytes: &[u8]) {
    if let Some(dir) = Path::new(path).parent() {
        std::fs::create_dir_all(dir)
            .unwrap_or_else(|e| fail(format!("mkdir {}: {e}", dir.display())));
    }
    std::fs::write(path, bytes).unwrap_or_else(|e| fail(format!("write {path}: {e}")));
    println!("wrote {path} ({} bytes)", bytes.len());
}

fn main() {
    let svg = std::fs::read(SVG)
        .unwrap_or_else(|e| fail(format!("read {SVG}: {e} (run from the repo root)")));
    let tree = resvg::usvg::Tree::from_data(&svg, &resvg::usvg::Options::default())
        .unwrap_or_else(|e| fail(format!("parse {SVG}: {e}")));
    let size = tree.size();
    if (size.width() - size.height()).abs() > 0.5 {
        fail(format!(
            "expected a square SVG, got {}x{}",
            size.width(),
            size.height()
        ));
    }
    let svg_side = size.width();

    let full = render(&tree, LOGO_SIZE, (0.0, 0.0, svg_side), None);
    write(LOGO_PNG, &full.encode_png());

    let (view, clip_y) = icon_view(
        &render(&tree, svg_side.round() as u32, (0.0, 0.0, svg_side), None),
        svg_side,
    );
    let master = render(&tree, ICON_MASTER, view, Some(clip_y));
    for (path, px) in ICON_PNGS {
        write(path, &master.downscale(*px, *px).encode_png());
    }

    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &px in ICO_SIZES {
        let img = master.downscale(px, px);
        let image = ico::IconImage::from_rgba_data(px, px, img.px);
        let entry = if px >= 256 {
            ico::IconDirEntry::encode(&image)
        } else {
            ico::IconDirEntry::encode_as_bmp(&image)
        };
        dir.add_entry(entry.unwrap_or_else(|e| fail(format!("ico {px}px: {e}"))));
    }
    let mut bytes = Vec::new();
    dir.write(&mut bytes)
        .unwrap_or_else(|e| fail(format!("ico: {e}")));
    write(ICO_PATH, &bytes);
}
