// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ERASE A/B — the audition bench for what a DELETE sounds like, the way
//! `typing_voice_ab` is the bench for the keystroke and `bed_audition` is the
//! bench for the bed. Neither of those can answer the question this one exists
//! for, because a delete is not one event: it is a keystroke bell and the erase
//! POOF'S OWN NOISE landing inside the same 45 ms, and the only thing that
//! settles whether that pair reads as *one gesture* or as *two keys* is an ear.
//!
//! It renders the SAME four delete shapes a shell user actually deletes with —
//! plain Backspace, a held Backspace run, Ctrl-W, Ctrl-U — through the real
//! synth in two arms:
//!
//! - `a-shipped-*` — the pre-2026-08-28 cue stream, where a plain Backspace
//!   fired its keystroke bell AND the full [`SoundKind::Kill`] swoosh (a
//!   per-command, tier-3 gesture) for one physical key;
//! - `b-puff-*` — the same shapes with the delete voices as they now ship: the
//!   bell plus [`SoundKind::Poof`], the cloud's small breath. Kill CHORDS are
//!   byte-identical between the arms — only the plain-Backspace shapes move,
//!   which is exactly the claim the pair is here to let you check by ear.
//!
//! Deterministic: fixed seed, fixed scripts, no dependency, no rng of its own.
//!
//!   cargo run -p aterm-effects --example erase_ab [-- <out_dir>] [--voice <name>]
//!   afplay target/erase-ab/b-puff-backspace.wav
//!
//! Each render prints peak dBFS and the time in ms between the bell's onset and
//! the delete voice's — the two numbers that say "quieter than the key" and
//! "close enough to be one gesture" — but the numbers are the sanity check. The
//! WAVs are the deliverable.

use std::path::PathBuf;

use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::tone::Tone;
use aterm_effects::trail_sound::{
    CHANNELS, SoundEvent, SoundGesture, SoundKind, SoundVoice, TrailSynth,
};

const SR: u32 = 48_000;
const SRF: f32 = 48_000.0;

/// One scripted delete: `(at seconds, kind)`, rendered in order.
type Script = Vec<(f32, SoundKind)>;

/// The keystroke bell a Backspace earns on its admitted cursor retreat, and the
/// delete voice that lands behind it. `poof_delay` is the frame the erase echo
/// takes to reach the probe (~16 ms — one frame at 60 Hz, measured live).
fn backspace(at: f32, delete_voice: Option<SoundKind>) -> Script {
    let mut s = vec![(at, SoundKind::Backspace)];
    s.extend(delete_voice.map(|k| (at + 0.016, k)));
    s
}

/// A HELD Backspace: keys at ~65 ms (macOS autorepeat), but the poof is rate-
/// limited to one per `POOF_MIN_GAP` (0.14 s) by `cursor_glow`, so only some of
/// the keys carry a delete voice. That asymmetry is audible and is the whole
/// reason this shape gets its own render.
fn held_backspace(delete_voice: Option<SoundKind>) -> Script {
    let mut s = Script::new();
    let mut last_poof = f32::MIN;
    for k in 0..10 {
        let t = 0.10 + k as f32 * 0.065;
        s.push((t, SoundKind::Backspace));
        if t - last_poof >= 0.14 {
            s.extend(delete_voice.map(|v| (t + 0.016, v)));
            last_poof = t;
        }
    }
    s
}

fn scripts(delete_voice: Option<SoundKind>) -> Vec<(&'static str, Script)> {
    vec![
        // One character, from a quiet screen.
        ("backspace", backspace(0.10, delete_voice)),
        // Ten of them, held.
        ("backspace-held", held_backspace(delete_voice)),
        // A word kill: no keystroke of its own, so the swoosh is its whole
        // voice — IDENTICAL in the two delete arms, and here as the reference
        // the puff should read as the little brother of. In the bell-only arm
        // it is genuinely empty (a kill chord has no bell), which makes its
        // `adds` column the swoosh's own level.
        ("ctrl-w", delete_voice.map_or_else(Script::new, |_| vec![(0.10, SoundKind::Kill)])),
        // A line kill, after the word it followed — the scale contrast.
        (
            "ctrl-u",
            delete_voice.map_or_else(Script::new, |_| {
                vec![(0.10, SoundKind::Kill), (0.55, SoundKind::Kill)]
            }),
        ),
        // …and the shape that matters most: a word TYPED, then corrected.
        // If the delete fights the bell, this is where it shows.
        ("typed-then-corrected", {
            let mut s: Script = (0..6).map(|k| (0.10 + k as f32 * 0.13, SoundKind::Typed)).collect();
            s.extend(backspace(0.92, delete_voice));
            s.extend(backspace(1.06, delete_voice));
            s.extend(backspace(1.20, delete_voice));
            s
        }),
    ]
}

fn render(script: &Script, voice: SoundVoice, secs: f32) -> Vec<f32> {
    let mut s = TrailSynth::new(SRF, 0x5EED_1234);
    let total = (SRF * secs) as usize;
    let mut buf = vec![0.0f32; total * CHANNELS];
    // Advance in 1 ms blocks, pushing each event as its instant arrives.
    let block = (SRF / 1000.0) as usize;
    let mut i = 0usize;
    let mut next = 0usize;
    while i < total {
        let t = i as f32 / SRF;
        while next < script.len() && script[next].0 <= t {
            s.push(SoundEvent {
                style: GlowStyle::RainbowKitty,
                voice,
                kind: SoundGesture::Trail(script[next].1),
                pan: 0.0,
                heat: 0.45,
                hue: 0.0,
                gain: 0.4,
                tone: Tone::Technical,
                bed: false,
            });
            next += 1;
        }
        let n = block.min(total - i);
        s.render(&mut buf[i * CHANNELS..(i + n) * CHANNELS]);
        i += n;
    }
    // Fold to mono for the audition file.
    buf.as_chunks::<CHANNELS>()
        .0
        .iter()
        .map(|c| (c[0] + c[1]) * 0.5)
        .collect()
}

fn peak_db(mono: &[f32]) -> f32 {
    let p = mono.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if p <= 0.0 { -120.0 } else { 20.0 * p.log10() }
}

fn rms_db(mono: &[f32]) -> f32 {
    if mono.is_empty() {
        return -120.0;
    }
    let e: f32 = mono.iter().map(|x| x * x).sum::<f32>() / mono.len() as f32;
    if e <= 0.0 { -120.0 } else { 10.0 * e.log10() }
}

fn wav_bytes(mono: &[f32]) -> Vec<u8> {
    let data_len = (mono.len() * 4) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&3u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&SR.to_le_bytes());
    w.extend_from_slice(&(SR * 4).to_le_bytes());
    w.extend_from_slice(&4u16.to_le_bytes());
    w.extend_from_slice(&32u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for &x in mono {
        w.extend_from_slice(&x.to_le_bytes());
    }
    w
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out: Option<PathBuf> = None;
    let mut voice = SoundVoice::Style;
    while let Some(a) = args.next() {
        if let Some(name) = a
            .strip_prefix("--voice=")
            .map(str::to_string)
            .or_else(|| (a == "--voice").then(|| args.next().unwrap_or_default()))
        {
            voice = SoundVoice::parse(&name).unwrap_or(SoundVoice::Style);
        } else if !a.starts_with('-') {
            out = Some(PathBuf::from(a));
        }
    }
    let out = out.unwrap_or_else(|| PathBuf::from("target/erase-ab"));
    std::fs::create_dir_all(&out).expect("out dir");

    // A = the shipped-until-2026-08-28 stream (Backspace fired the full kill
    // swoosh); B = the puff. Kill chords are identical in both.
    let arms: [(&str, Option<SoundKind>); 3] = [
        ("a-shipped", Some(SoundKind::Kill)),
        ("b-puff", Some(SoundKind::Poof)),
        // THE REFERENCE, and the reason it earns a place beside the two arms:
        // it is what a delete sounds like with NO delete voice at all — the
        // keystroke bell alone. Measured against it, `a-shipped` turns out to
        // be BIT-IDENTICAL on every plain-Backspace shape, because the swoosh
        // it cues lands ~16 ms behind the bell and the synth's `MIN_GAP` (45 ms)
        // thins it. The delete "noise" the code believed it was making was
        // never reaching the speaker; that is what `b-puff`'s min-gap bypass
        // fixes, and this file is the control that proves it.
        ("c-bell-only", None),
    ];
    // The bell-only reference first, so every other arm can be reported as what
    // it ADDS over a delete with no delete voice at all.
    // Every arm of one shape is rendered to the SAME length (the shape's
    // longest script + 1.2 s of tail) so the difference is sample-aligned.
    let lens: Vec<f32> = scripts(Some(SoundKind::Kill))
        .into_iter()
        .map(|(_, s)| s.last().map_or(0.5, |e| e.0) + 1.2)
        .collect();
    let bells: Vec<Vec<f32>> = scripts(None)
        .into_iter()
        .enumerate()
        .map(|(i, (_, s))| render(&s, voice, lens[i]))
        .collect();
    println!("{:<34}  {:>9}  {:>12}", "file", "peak dBFS", "adds (rms)");
    for (arm, delete_voice) in arms {
        for (i, (name, script)) in scripts(delete_voice).into_iter().enumerate() {
            let secs = bells[i].len() as f32 / SRF;
            let mono = render(&script, voice, secs);
            let file = format!("{arm}-{name}.wav");
            std::fs::write(out.join(&file), wav_bytes(&mono)).expect("wav");
            let add: Vec<f32> = mono
                .iter()
                .zip(&bells[i])
                .map(|(a, b)| a - b)
                .collect();
            println!(
                "{file:<34}  {:>9.2}  {:>12}",
                peak_db(&mono),
                if add.iter().all(|x| *x == 0.0) {
                    "SILENT".to_string()
                } else {
                    format!("{:.2} dB", rms_db(&add))
                }
            );
        }
    }
    // The two delete voices in ISOLATION, for a direct A/B of the sound itself.
    for (name, kind) in [("iso-swoosh", SoundKind::Kill), ("iso-puff", SoundKind::Poof)] {
        let mono = render(&vec![(0.05, kind)], voice, 0.9);
        let file = format!("{name}.wav");
        std::fs::write(out.join(&file), wav_bytes(&mono)).expect("wav");
        println!("{file:<34}  {:>9.2}", peak_db(&mono));
    }
    println!("\nwrote to {}", out.display());
}
