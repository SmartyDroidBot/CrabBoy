//! Arranging a console's displays into one image.
//!
//! Frontends draw a single picture. A console with several displays (the 3DS)
//! has them stacked top to bottom, each centred horizontally, which is how
//! the hardware is built; a console with one display is laid out as itself.

use crate::{Frame, Screen, System};

/// Where each display of a system sits in the composed image.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Layout {
    pub width: u16,
    pub height: u16,
    screens: Vec<Screen>,
    origins: Vec<(u16, u16)>,
}

impl Layout {
    /// Stack `screens` vertically in order, centring each one.
    pub fn stacked(screens: &[Screen]) -> Self {
        let width = screens.iter().map(|s| s.width).max().unwrap_or(0);
        let mut y = 0;
        let mut origins = Vec::with_capacity(screens.len());
        for s in screens {
            origins.push(((width - s.width) / 2, y));
            y += s.height;
        }
        Layout {
            width,
            height: y,
            screens: screens.to_vec(),
            origins,
        }
    }

    /// The layout of every display of `system`.
    pub fn of(system: &dyn System) -> Self {
        Layout::stacked(&system.screens())
    }

    /// The top-left corner of display `index` in the composed image.
    pub fn origin(&self, index: usize) -> Option<(u16, u16)> {
        self.origins.get(index).copied()
    }

    /// The display under the composed-image point `(x, y)`, with the point in
    /// that display's own coordinates.
    pub fn locate(&self, x: u16, y: u16) -> Option<(usize, u16, u16)> {
        self.screens
            .iter()
            .zip(&self.origins)
            .position(|(s, &(ox, oy))| x >= ox && x - ox < s.width && y >= oy && y - oy < s.height)
            .map(|i| (i, x - self.origins[i].0, y - self.origins[i].1))
    }

    /// Every display of `system` in one frame. A single display is returned
    /// untouched (shades included); several are composed in colour over
    /// black, with monochrome frames mapped through `palette`.
    pub fn compose(&self, system: &dyn System, palette: &[[u8; 4]]) -> Frame {
        if self.screens.len() <= 1 {
            return system.frame();
        }
        let mut out = Frame::new(self.width, self.height);
        let mut rgb = vec![0u8; self.width as usize * self.height as usize * 3];
        for (i, (s, &(ox, oy))) in self.screens.iter().zip(&self.origins).enumerate() {
            let src = system.frame_at(i).to_rgb(palette);
            let row = s.width as usize * 3;
            for y in 0..s.height as usize {
                let at = ((oy as usize + y) * self.width as usize + ox as usize) * 3;
                rgb[at..at + row].copy_from_slice(&src[y * row..(y + 1) * row]);
            }
        }
        out.rgb = Some(rgb);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_screens_stack_with_the_narrow_one_centred() {
        let l = Layout::stacked(&[Screen::new(400, 240), Screen::new(320, 240)]);
        assert_eq!((l.width, l.height), (400, 480));
        assert_eq!(l.origin(0), Some((0, 0)));
        assert_eq!(l.origin(1), Some((40, 240)));
    }

    #[test]
    fn locate_maps_points_into_screen_coordinates() {
        let l = Layout::stacked(&[Screen::new(400, 240), Screen::new(320, 240)]);
        assert_eq!(l.locate(399, 239), Some((0, 399, 239)));
        assert_eq!(l.locate(40, 240), Some((1, 0, 0)));
        assert_eq!(l.locate(359, 479), Some((1, 319, 239)));
        assert_eq!(l.locate(39, 240), None);
        assert_eq!(l.locate(360, 300), None);
        assert_eq!(l.locate(0, 480), None);
    }

    #[test]
    fn a_single_screen_is_its_own_layout() {
        let l = Layout::stacked(&[Screen::new(160, 144)]);
        assert_eq!((l.width, l.height), (160, 144));
        assert_eq!(l.locate(159, 143), Some((0, 159, 143)));
    }
}
