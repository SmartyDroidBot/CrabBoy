//! Shared helpers for the headless CrabBoy binaries: frame hashing, image
//! and WAV encoders, and the scripted-input grammar.

use emu_core::{Axis, Button, System};

/// FNV-1a 32-bit hash. The wasm smoke test (`platforms/wasm/tests/run.js`)
/// implements the same function so frame hashes compare across targets.
pub fn fnv1a32(data: &[u8]) -> u32 {
    let mut h = 0x811C_9DC5u32;
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Parse a button name as used in input scripts (case-insensitive).
pub fn parse_button(name: &str) -> Option<Button> {
    Some(match name.trim().to_ascii_uppercase().as_str() {
        "A" => Button::A,
        "B" => Button::B,
        "START" => Button::Start,
        "SELECT" => Button::Select,
        "UP" => Button::Up,
        "DOWN" => Button::Down,
        "LEFT" => Button::Left,
        "RIGHT" => Button::Right,
        "L" => Button::L,
        "R" => Button::R,
        "X" => Button::X,
        "Y" => Button::Y,
        "ZL" => Button::ZL,
        "ZR" => Button::ZR,
        _ => return None,
    })
}

/// What a scripted input event does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Input {
    /// Press (`true`) or release a button.
    Button(Button, bool),
    /// Hold the stylus at a point of the touch screen (the 3DS's bottom
    /// screen, 320x240), or lift it.
    Touch(Option<(u16, u16)>),
    /// Move the 3DS circle pad; both axes span -32767..=32767, `y` up.
    CirclePad(i16, i16),
}

/// One scripted input event, applied at `frame` (frames completed so far).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InputEvent {
    pub frame: u64,
    pub input: Input,
}

impl InputEvent {
    pub fn apply(&self, system: &mut dyn System) {
        match self.input {
            Input::Button(button, true) => system.press(button),
            Input::Button(button, false) => system.release(button),
            Input::Touch(point) => system.set_touch(point),
            Input::CirclePad(x, y) => system.set_axis(Axis::CirclePad, x, y),
        }
    }
}

/// Parse an input script such as `START@400,!START@410` (also accepts `;`
/// separators and the older `RELEASE START@410` spelling). `TOUCH:X:Y@F`
/// holds the stylus at a point of the touch screen and `!TOUCH@F` lifts it;
/// `CPAD:X:Y@F` moves the circle pad and `!CPAD@F` centres it. Events are
/// returned sorted by frame, in script order within a frame.
pub fn parse_input_script(spec: &str) -> Result<Vec<InputEvent>, String> {
    let mut out = Vec::new();
    for item in spec
        .split([',', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let (name, frame) = item
            .rsplit_once('@')
            .ok_or_else(|| format!("bad input event {item:?}: expected NAME@FRAME"))?;
        let frame: u64 = frame
            .trim()
            .parse()
            .map_err(|_| format!("bad frame number in {item:?}"))?;
        let name = name.trim();
        let (name, pressed) = if let Some(n) = name.strip_prefix('!') {
            (n, false)
        } else if let Some(n) = name.strip_prefix("RELEASE") {
            (n, false)
        } else {
            (name, true)
        };
        let mut parts = name.trim().split(':');
        let kind = parts.next().unwrap_or("").to_ascii_uppercase();
        let point: Vec<&str> = parts.collect();
        let input = match (kind.as_str(), pressed) {
            ("TOUCH" | "CPAD", false) if point.is_empty() => {
                if kind == "TOUCH" {
                    Input::Touch(None)
                } else {
                    Input::CirclePad(0, 0)
                }
            }
            ("TOUCH", true) => match point[..] {
                [x, y] => match (x.trim().parse::<u16>(), y.trim().parse::<u16>()) {
                    (Ok(x), Ok(y)) if x < 320 && y < 240 => Input::Touch(Some((x, y))),
                    _ => return Err(format!("{item:?}: a touch is TOUCH:X:Y inside 320x240")),
                },
                _ => return Err(format!("{item:?}: a touch is TOUCH:X:Y")),
            },
            ("CPAD", true) => match point[..] {
                [x, y] => match (x.trim().parse::<i16>(), y.trim().parse::<i16>()) {
                    (Ok(x), Ok(y)) if x > i16::MIN && y > i16::MIN => Input::CirclePad(x, y),
                    _ => return Err(format!("{item:?}: CPAD axes span -32767..=32767")),
                },
                _ => return Err(format!("{item:?}: the circle pad is CPAD:X:Y")),
            },
            _ if !point.is_empty() => return Err(format!("unknown input {name:?}")),
            _ => Input::Button(
                parse_button(name).ok_or_else(|| format!("unknown button {name:?}"))?,
                pressed,
            ),
        };
        out.push(InputEvent { frame, input });
    }
    out.sort_by_key(|e| e.frame);
    Ok(out)
}

/// Encode an RGBA8888 image as a PNG (8-bit RGB, no filtering, stored
/// deflate blocks). Pure Rust, no dependencies.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }

    let stride = width as usize * 4;
    let mut raw = Vec::with_capacity(height as usize * (width as usize * 3 + 1));
    for row in rgba.chunks(stride) {
        raw.push(0);
        for px in row.as_chunks::<4>().0 {
            raw.extend_from_slice(&px[..3]);
        }
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, block) in blocks.iter().enumerate() {
        zlib.push((i + 1 == blocks.len()) as u8);
        let len = block.len() as u16;
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Encode an RGBA8888 image as a binary PPM (P6).
pub fn encode_ppm(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
    for px in rgba.as_chunks::<4>().0 {
        out.extend_from_slice(&px[..3]);
    }
    out
}

/// Encode interleaved stereo `f32` samples as a 16-bit PCM WAV file.
pub fn encode_wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_bytes = samples.len() as u32 * 2;
    let mut w = Vec::with_capacity(44 + data_bytes as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    w.extend_from_slice(b"WAVE");
    w.extend_from_slice(b"fmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&sample_rate.to_le_bytes());
    w.extend_from_slice(&(sample_rate * 4).to_le_bytes());
    w.extend_from_slice(&4u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_bytes.to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        w.extend_from_slice(&v.to_le_bytes());
    }
    w
}

/// A file read where it lies (shared with the desktop frontend).
pub use crab_systems::FileStorage;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a32_matches_reference_vectors() {
        assert_eq!(fnv1a32(b""), 0x811C_9DC5);
        assert_eq!(fnv1a32(b"a"), 0xE40C_292C);
        assert_eq!(fnv1a32(b"foobar"), 0xBF9C_F968);
    }

    #[test]
    fn input_script_grammar() {
        let ev = parse_input_script("START@400, !start@410; RELEASE A@5").unwrap();
        let at = |frame, input| InputEvent { frame, input };
        assert_eq!(
            ev,
            [
                at(5, Input::Button(Button::A, false)),
                at(400, Input::Button(Button::Start, true)),
                at(410, Input::Button(Button::Start, false)),
            ]
        );
        assert!(parse_input_script("START").is_err());
        assert!(parse_input_script("FOO@1").is_err());
        assert!(parse_input_script("").unwrap().is_empty());
    }

    #[test]
    fn input_scripts_touch_and_move_the_circle_pad() {
        let ev = parse_input_script("TOUCH:160:120@30,!TOUCH@32,CPAD:0:-32767@40,!cpad@50,ZL@1")
            .unwrap();
        let inputs: Vec<Input> = ev.iter().map(|e| e.input).collect();
        assert_eq!(
            inputs,
            [
                Input::Button(Button::ZL, true),
                Input::Touch(Some((160, 120))),
                Input::Touch(None),
                Input::CirclePad(0, -32767),
                Input::CirclePad(0, 0),
            ]
        );
        assert!(
            parse_input_script("TOUCH:320:0@1").is_err(),
            "off the screen"
        );
        assert!(parse_input_script("TOUCH:1@1").is_err());
        assert!(parse_input_script("CPAD:-32768:0@1").is_err());
        assert!(parse_input_script("A:1:2@1").is_err());
    }

    #[test]
    fn png_has_a_valid_signature_and_size() {
        let png = encode_png(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(&png[16..24], &[0, 0, 0, 2, 0, 0, 0, 1]);
    }

    #[test]
    fn wav_header_describes_stereo_16_bit() {
        let wav = encode_wav(&[0.0, 1.0], 8192);
        assert_eq!(wav.len(), 48);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 2);
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            8192
        );
        assert_eq!(i16::from_le_bytes([wav[46], wav[47]]), 32767);
    }
}
