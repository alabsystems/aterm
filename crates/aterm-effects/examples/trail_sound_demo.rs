// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Audition demo for the trail sound palettes — drives
//! [`aterm_effects::trail_sound::TrailSynth`] through the same scenario the
//! live host produces (steady typing, a hot burst, a thinking pause, a
//! backspace run, an Enter jump, then silence) and writes one WAV per style,
//! plus a numeric peak/RMS report so level tuning is reviewable in text.
//!
//!   cargo run -p aterm-effects --example trail_sound_demo -- <out_dir>
//!
//! Listen with e.g. `afplay <out_dir>/water.wav`.

use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::Path;

use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::trail_sound::{
    CHANNELS, SoundEvent, SoundGesture, SoundKind, SoundVoice, TrailSynth,
};

const SR: u32 = 48_000;

/// (name, style) for every audible trail style.
const STYLES: [(&str, GlowStyle); 9] = [
    ("lumen", GlowStyle::Lumen),
    ("phaser", GlowStyle::Phaser),
    ("nyan", GlowStyle::RainbowKitty),
    ("sparkle", GlowStyle::Sparkle),
    ("fire", GlowStyle::Fire),
    ("laser", GlowStyle::Laser),
    ("beam", GlowStyle::Beam),
    ("water", GlowStyle::Water),
    ("comet", GlowStyle::Comet),
];

/// One scripted event: (time in seconds, kind, pan, heat).
type Cue = (f32, SoundKind, f32, f32);

/// The review scenario, shared by every style: 2 s of steady typing panning
/// across the line, a 1.2 s hot burst, a pause, a backspace run, and the
/// Enter jump — then 3.5 s of tail so the bed's exhale is captured.
fn scenario() -> Vec<Cue> {
    let mut cues = Vec::new();
    // Steady typing, 6 cps, heat warming up.
    for i in 0..12 {
        let t = i as f32 / 6.0;
        cues.push((t, SoundKind::Typed, -0.8 + t * 0.55, (t / 2.0) * 0.7));
    }
    // Hot burst, 20 cps.
    for i in 0..24 {
        let t = 2.0 + i as f32 / 20.0;
        cues.push((t, SoundKind::Typed, -0.6 + (i as f32 / 24.0) * 1.2, 0.9));
    }
    // Thinking pause… then four backspaces.
    for i in 0..4 {
        cues.push((
            3.9 + i as f32 / 8.0,
            SoundKind::Backspace,
            0.5 - i as f32 * 0.05,
            0.4,
        ));
    }
    // Enter: the jump flourish.
    cues.push((4.6, SoundKind::Jump, -0.9, 0.5));
    cues
}

fn main() {
    let out_dir = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: trail_sound_demo <out_dir>");
        std::process::exit(2);
    });
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    println!("style    peak dBFS   rms dBFS   quiet-at");
    for (name, style) in STYLES {
        let cues = scenario();
        let total_s = 10.5f32;
        let frames = (total_s * SR as f32) as usize;
        let mut synth = TrailSynth::new(SR as f32, 0xA7E12);
        let mut pcm = vec![0.0f32; frames * CHANNELS];

        // Hue sweeps like the live phaser band does.
        let mut cue_i = 0;
        let block = 256;
        let mut quiet_at = f32::NAN;
        let mut f = 0;
        while f < frames {
            let t = f as f32 / SR as f32;
            while cue_i < cues.len() && cues[cue_i].0 <= t {
                let (ct, kind, pan, heat) = cues[cue_i];
                synth.push(SoundEvent {
                    style,
                    voice: SoundVoice::Style,
                    kind: SoundGesture::Trail(kind),
                    pan,
                    heat,
                    hue: (ct * 0.18).fract(),
                    gain: 0.4, // default trail_sound_volume
                    tone: aterm_effects::tone::Tone::Technical,
                    // Bed ON: the demo/tuning harness audits the FULL palette
                    // (the redesign tournament listens to beds here even
                    // though the product default is off).
                    bed: true,
                });
                cue_i += 1;
            }
            let n = block.min(frames - f);
            synth.render(&mut pcm[f * CHANNELS..(f + n) * CHANNELS]);
            if quiet_at.is_nan() && t > 4.7 && synth.is_quiet() {
                quiet_at = t;
            }
            f += n;
        }

        let peak = pcm.iter().fold(0.0f32, |a, &x| a.max(x.abs()));
        let rms = (pcm
            .iter()
            .map(|&x| f64::from(x) * f64::from(x))
            .sum::<f64>()
            / pcm.len() as f64)
            .sqrt() as f32;
        println!(
            "{name:<8} {:>8.1}   {:>8.1}   {quiet_at:.2}s",
            20.0 * peak.max(1e-9).log10(),
            20.0 * rms.max(1e-9).log10(),
        );

        write_wav(Path::new(&out_dir).join(format!("{name}.wav")), &pcm);
    }
    println!("WAVs written to {out_dir}");
}

/// Minimal RIFF/WAVE writer: 16-bit PCM, stereo, 48 kHz. Hand-rolled so the
/// demo adds no dependency.
fn write_wav(path: std::path::PathBuf, pcm: &[f32]) {
    let mut w = BufWriter::new(File::create(&path).expect("create wav"));
    let data_len = (pcm.len() * 2) as u32;
    let byte_rate = SR * CHANNELS as u32 * 2;
    w.write_all(b"RIFF").unwrap();
    w.write_all(&(36 + data_len).to_le_bytes()).unwrap();
    w.write_all(b"WAVEfmt ").unwrap();
    w.write_all(&16u32.to_le_bytes()).unwrap();
    w.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
    w.write_all(&(CHANNELS as u16).to_le_bytes()).unwrap();
    w.write_all(&SR.to_le_bytes()).unwrap();
    w.write_all(&byte_rate.to_le_bytes()).unwrap();
    w.write_all(&((CHANNELS * 2) as u16).to_le_bytes()).unwrap(); // block align
    w.write_all(&16u16.to_le_bytes()).unwrap();
    w.write_all(b"data").unwrap();
    w.write_all(&data_len.to_le_bytes()).unwrap();
    for &x in pcm {
        let s = (x.clamp(-1.0, 1.0) * 32_767.0) as i16;
        w.write_all(&s.to_le_bytes()).unwrap();
    }
}
