//! Dump the first N frames of audio from a ROM to a stereo 16-bit WAV file.
//!
//! Usage: `wav <rom> <out.wav> [frames]` (frames defaults to 60).
//! Writes at the native 8192 Hz APU sample rate.

use emu_core::System;
use gb_core::cartridge::Cartridge;
use gb_core::gb::Gb;

fn main() {
    let mut args = std::env::args().skip(1);
    let rom_path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("usage: wav <rom> <out.wav> [frames]");
            std::process::exit(1);
        }
    };
    let out_path = args.next().unwrap_or_else(|| "out.wav".to_string());
    let frames: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);

    let data = std::fs::read(&rom_path).expect("read ROM");
    let cart = Cartridge::load(&data).expect("load cartridge");
    let mut emu: Box<dyn System> = Gb::system(cart);

    let mut audio: Vec<f32> = Vec::new();
    for _ in 0..frames {
        emu.run_frame();
        audio.extend(emu.take_audio().samples);
    }

    write_wav(&out_path, &audio, 8192).expect("write WAV");
    eprintln!(
        "wrote {} samples ({:.1} s) to {} at 8192 Hz",
        audio.len() / 2,
        audio.len() as f64 / 2.0 / 8192.0,
        out_path
    );
}

fn write_wav(path: &str, samples: &[f32], sample_rate: u32) -> std::io::Result<()> {
    use std::io::Write;
    let num_samples = samples.len() as u32;
    let data_bytes = num_samples * 2; // 16-bit mono-equivalent data size
    let file = std::fs::File::create(path)?;
    let mut w = std::io::BufWriter::new(file);

    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_bytes).to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?; // fmt chunk size
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&2u16.to_le_bytes())?; // channels
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&(sample_rate * 2 * 2).to_le_bytes())?; // byte rate
    w.write_all(&4u16.to_le_bytes())?; // block align
    w.write_all(&16u16.to_le_bytes())?; // bits per sample
    w.write_all(b"data")?;
    w.write_all(&data_bytes.to_le_bytes())?;

    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        w.write_all(&v.to_le_bytes())?;
    }
    w.flush()
}
