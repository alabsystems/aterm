// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `mix_meter`: measures the trail-sound loudness ladder.
//!
//! The ladder in `trail_sound.rs` puts tier 1 (a keystroke) at −21.0 dBFS and
//! every other gesture against it; `palette_trim`, which lands each palette on
//! that floor, is fitted, not derived. This renders real events through the real
//! `TrailSynth` and reports each one's delivered peak, so a retune is fitted
//! against a measurement.
//!
//! ```text
//! targo --unverified run -q -p aterm-effects --example mix_meter
//! targo --unverified run -q -p aterm-effects --example mix_meter -- rainbow kitty
//! targo --unverified run -q -p aterm-effects --example mix_meter -- --voice typewriter
//! targo --unverified run -q -p aterm-effects --example mix_meter -- --voice droplet lumen
//! ```
//!
//! `--voice <name>` selects the typing-sound voice (`SoundVoice::parse`; default
//! `auto`); the bare style argument selects the look the events ride, which
//! matters only under `auto`. Peaks are reported at the host's default
//! `trail_sound_volume` (0.4) and at `gain = 1.0`, so a palette's trim is judged
//! apart from the volume knob.

use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::tone::Tone;
use aterm_effects::trail_sound::{
    CHANNELS, CelebrationGesture, SoundEvent, SoundGesture, SoundKind, SoundVoice, TrailSynth,
    WordGesture,
};

const SR: f32 = 48_000.0;
/// The host's shipped `trail_sound_volume` default.
const DEFAULT_VOLUME: f32 = 0.4;
/// Long enough for the longest scheduled gesture (the riff bar) to finish.
const TAIL_S: f32 = 2.4;

/// Render one gesture in isolation and return its peak absolute sample.
fn peak_of(voice: SoundVoice, style: GlowStyle, kind: SoundGesture, gain: f32, heat: f32) -> f32 {
    let mut s = TrailSynth::new(SR, 0x5EED_1234);
    s.push(SoundEvent {
        style,
        voice,
        kind,
        pan: 0.0,
        heat,
        hue: 0.0,
        gain,
        tone: Tone::Technical,
        bed: false,
        shifted: false,
    });
    let frames = (SR * TAIL_S) as usize;
    let mut buf = vec![0.0f32; frames * CHANNELS];
    s.render(&mut buf);
    buf.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

fn db(x: f32) -> f32 {
    if x <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * x.log10()
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `--voice <name>` / `--voice=<name>` — quote a two-word name
    // (`--voice "glass bell"`).
    let mut voice = SoundVoice::Style;
    if let Some(i) = args
        .iter()
        .position(|a| a == "--voice" || a.starts_with("--voice="))
    {
        let flag = args.remove(i);
        let name = match flag.strip_prefix("--voice=") {
            Some(inline) => inline.to_string(),
            None if i < args.len() => args.remove(i),
            None => String::new(),
        };
        voice = match SoundVoice::parse(&name) {
            Some(v) => v,
            None => {
                eprintln!(
                    "unknown voice: {name:?} (one of: {})",
                    SoundVoice::ALL
                        .iter()
                        .map(|v| v.name())
                        .collect::<Vec<_>>()
                        .join(" | ")
                );
                std::process::exit(2);
            }
        };
    }
    let arg = args.join(" ");
    let style = match arg.trim() {
        "" | "rainbow kitty" | "rainbow" | "kitty" => GlowStyle::RainbowKitty,
        "lumen" => GlowStyle::Lumen,
        "phaser" => GlowStyle::Phaser,
        "sparkle" => GlowStyle::Sparkle,
        "fire" => GlowStyle::Fire,
        "laser" => GlowStyle::Laser,
        "beam" => GlowStyle::Beam,
        "water" => GlowStyle::Water,
        "comet" => GlowStyle::Comet,
        other => {
            eprintln!("unknown style: {other}");
            std::process::exit(2);
        }
    };

    // Every discrete gesture; the riff bar is measured after.
    let gestures: [(&str, SoundGesture); 9] = [
        ("Typed", SoundGesture::Trail(SoundKind::Typed)),
        ("Backspace", SoundGesture::Trail(SoundKind::Backspace)),
        ("Glide", SoundGesture::Trail(SoundKind::Glide { dir: 1 })),
        ("Navigation", SoundGesture::Trail(SoundKind::Navigation)),
        ("Sweep", SoundGesture::Trail(SoundKind::Sweep { dir: 1 })),
        ("Kill", SoundGesture::Trail(SoundKind::Kill)),
        ("Jump", SoundGesture::Trail(SoundKind::Jump)),
        ("Land", SoundGesture::Trail(SoundKind::Land)),
        ("Bonk", SoundGesture::Words(WordGesture::Bonk)),
    ];

    println!(
        "MIX METER — voice {:?} ({}) on look {style:?} @ {SR} Hz, Tone::Technical, heat 0.5, pan 0\n",
        voice,
        voice.name()
    );
    println!(
        "{:<12} {:>12} {:>12}   vs Typed",
        "gesture", "@vol 0.40", "@gain 1.0"
    );
    println!("{}", "-".repeat(56));

    let typed_ref = peak_of(
        voice,
        style,
        SoundGesture::Trail(SoundKind::Typed),
        DEFAULT_VOLUME,
        0.5,
    );

    for (name, g) in gestures {
        let at_vol = peak_of(voice, style, g, DEFAULT_VOLUME, 0.5);
        let at_one = peak_of(voice, style, g, 1.0, 0.5);
        let rel = db(at_vol) - db(typed_ref);
        println!(
            "{name:<12} {:>11.2} {:>11.2}   {rel:>+6.2} dB",
            db(at_vol),
            db(at_one)
        );
    }

    // The riff on its loudest bar (the chorus push, bar 4 of the 8-bar form):
    // the peak a listener meets.
    let riff = peak_of(
        voice,
        style,
        SoundGesture::Celebration(CelebrationGesture::riff_bar(4, 0)),
        DEFAULT_VOLUME,
        1.0,
    );
    println!(
        "{:<12} {:>11.2} {:>11.2}   {:>+6.2} dB",
        "RiffBar(4)",
        db(riff),
        db(peak_of(
            voice,
            style,
            SoundGesture::Celebration(CelebrationGesture::riff_bar(4, 0)),
            1.0,
            1.0
        )),
        db(riff) - db(typed_ref)
    );

    println!(
        "\nTIER 1 target for Typed is -21.0 dBFS at gain 1.0 (see the ladder \
         doc above `TYPED_KIND_GAIN`)."
    );
    let typed_one = peak_of(
        voice,
        style,
        SoundGesture::Trail(SoundKind::Typed),
        1.0,
        0.5,
    );
    println!(
        "Typed @ gain 1.0 measures {:.2} dBFS  ->  trim correction x{:.4} ({:+.2} dB)",
        db(typed_one),
        10f32.powf((-21.0 - db(typed_one)) / 20.0),
        -21.0 - db(typed_one)
    );
}
