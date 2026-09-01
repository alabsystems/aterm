// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `mix_meter` — INTROSPECTION for the trail-sound loudness ladder.
//!
//! The ladder in `trail_sound.rs` is a design document written in dBFS: TIER 1
//! (a keystroke) is the floor at −21.0 dBFS and every other gesture is placed
//! against it. But `palette_trim` — the per-style constant that lands each
//! palette ON that floor — is a FITTED number, and nothing in the test suite
//! measures it. Change a partial level, a lowpass corner or a spawn gain and
//! the ladder silently drifts; the byte-pin oracle keeps passing, because it
//! only proves the two copies of the DSP agree with each other.
//!
//! This tool closes that loop. It renders real events through the real
//! `TrailSynth` and reports the delivered peak of each one, so the ladder can
//! be READ rather than assumed — and so a retune can be fitted against a
//! measurement instead of a guess.
//!
//! ```text
//! cargo run -q -p aterm-effects --example mix_meter
//! cargo run -q -p aterm-effects --example mix_meter -- rainbow kitty
//! cargo run -q -p aterm-effects --example mix_meter -- --voice typewriter
//! cargo run -q -p aterm-effects --example mix_meter -- --voice droplet lumen
//! ```
//!
//! `--voice <name>` selects the TYPING-SOUND VOICE (`SoundVoice::parse`: the
//! picker's canonical spellings or a documented alias; default `auto`); the
//! bare style argument still selects the LOOK the events ride, which only
//! matters under `auto`. This is the bench every `palette_trim` fit is read
//! on, so a sound-only voice must be meterable without an edit.
//!
//! Levels are reported at the host's DEFAULT `trail_sound_volume` (0.4) and,
//! separately, normalized to `gain = 1.0` so the palette's own trim can be
//! judged independently of the user's volume knob.

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

    // Every discrete gesture, plus the two composed ones (a riff bar, and a
    // keystroke measured at a typing-flood rate rather than in isolation).
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

    // The sing-along riff: measured on its loudest authored bar (the chorus
    // push at bar 4 of the 8-bar form) rather than bar 0, so the ladder is
    // read at the peak the listener actually meets.
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
