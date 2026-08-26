// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! INSTRUMENT AUDITION — evidence for EARS (the audio twin of the house
//! verify-UX-with-pixels law). Renders the sing-along celebration's core
//! moments end-to-end — real [`KittySing`] detector fed a scripted press
//! stream, real host-style bar latch, real [`TrailSynth`] — into
//! double-clickable WAVs, so the owner auditions the voicing and the merge
//! BEFORE a cut, exactly like the art previews.
//!
//!   cargo run -p aterm-effects --example instrument_audition [-- <out_dir>]
//!   afplay <out_dir>/hold-t.wav
//!
//! Scenarios:
//! - `hold-t`      — one key held ~11 s: its verse, bar 0 through the build
//!   to the clap. THE TIMBRE BENCH: render this before and after a voicing
//!   change for the A/B.
//! - `hold-e`      — a different key held: a different verse of the same
//!   celebration.
//! - `switch-t-e`  — five bars on 't', then the hold slides to 'e': THE
//!   MERGE BENCH — the seamless modulation at the next bar boundary,
//!   ringing tails across it.
//! - `performance` — a ~30 s multi-key medley, ending in the plain
//!   graceful wind-down.
//!
//! Deterministic: fixed seed, fixed press script, the pinned 150 BPM grid.

use std::path::PathBuf;
use std::time::Duration;

use aterm_time::Instant;

use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::kitty_sing::KittySing;
use aterm_effects::tone::Tone;
use aterm_effects::trail_sound::{
    CHANNELS, CelebrationGesture, SoundEvent, SoundGesture, SoundVoice, TrailSynth,
};

const SR: u32 = 48_000;
const SEED: u32 = 0x51A6_B01D;
const SESSION: u64 = 1;

/// One scripted press: (seconds from scenario start, character). `None`
/// releases (a gap past the repeat cadence — the lazy release does the rest).
type Press = (f32, char);

/// A gesture cue resolved by the host sim: (seconds, gesture).
type Cue = (f32, CelebrationGesture);

/// Hold `ch` at 30 ms cadence over `[from, to)` seconds.
fn hold(from: f32, to: f32, ch: char, out: &mut Vec<Press>) {
    let mut t = from;
    while t < to {
        out.push((t, ch));
        t += 0.03;
    }
}

/// THE HOST SIM: feed the press script through a real [`KittySing`] and run
/// `app_render`'s exact once-per-new-bar latch at ~60 Hz, collecting the
/// riff payloads with their frame times. This is the same data path the GUI
/// speaks — the WAV is the shipping instrument, not a mock of it.
fn host_cues(presses: &[Press], seconds: f32) -> Vec<Cue> {
    let t0 = Instant::now(); // never awaited — pure arithmetic below
    let mut sing = KittySing::default();
    let mut latch: Option<u64> = None;
    let mut cues: Vec<Cue> = Vec::new();
    let mut press_i = 0usize;
    let mut clock = 0.0f32;
    while clock < seconds {
        let now = t0 + Duration::from_secs_f32(clock);
        while press_i < presses.len() && presses[press_i].0 <= clock {
            sing.note_char(now, SESSION, presses[press_i].1);
            press_i += 1;
        }
        if let Some(bar) = sing.bar(now) {
            if latch != Some(bar) {
                latch = Some(bar);
                cues.push((
                    clock,
                    CelebrationGesture::riff_bar((bar & 0xffff) as u16, sing.signature()),
                ));
            }
        } else if sing.drive(now) <= 0.0 {
            sing.settle(now);
            latch = None;
        }
        clock += 1.0 / 60.0;
    }
    cues
}

/// Render the cue list through a fresh synth; mono mixdown.
fn render(cues: &[Cue], seconds: f32) -> Vec<f32> {
    let frames = (seconds * SR as f32) as usize;
    let mut synth = TrailSynth::new(SR as f32, SEED);
    let mut mono = vec![0.0f32; frames];
    let mut stereo = [0.0f32; 256 * CHANNELS];
    let mut cue_i = 0usize;
    let mut f = 0usize;
    while f < frames {
        let n = 256.min(frames - f);
        let t = f as f32 / SR as f32;
        while cue_i < cues.len() && cues[cue_i].0 <= t {
            synth.push(SoundEvent {
                style: GlowStyle::RainbowKitty,
                voice: SoundVoice::Style,
                kind: SoundGesture::Celebration(cues[cue_i].1),
                pan: 0.0,
                heat: 1.0,
                hue: 0.0,
                gain: 0.4,
                tone: Tone::Technical,
                bed: false,
            });
            cue_i += 1;
        }
        synth.render(&mut stereo[..n * CHANNELS]);
        for i in 0..n {
            mono[f + i] = 0.5 * (stereo[i * 2] + stereo[i * 2 + 1]);
        }
        f += n;
    }
    mono
}

/// Minimal 32-bit-float WAV (the `typing_voice_ab` writer, mono @ 48 kHz).
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
    let out: PathBuf = std::env::args().nth(1).map_or_else(
        || PathBuf::from("target/instrument-audition"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&out).expect("out dir");

    // hold-t / hold-e: ~0.5 s to arm + 10 bars.
    for ch in ['t', 'e'] {
        let mut presses = Vec::new();
        hold(0.0, 11.0, ch, &mut presses);
        let cues = host_cues(&presses, 13.0);
        let mono = render(&cues, 14.0);
        let name = format!("hold-{ch}.wav");
        std::fs::write(out.join(&name), wav_bytes(&mono)).expect("wav");
        println!(
            "wrote {} ({} riff cues)",
            out.join(&name).display(),
            cues.len()
        );
    }

    // switch-t-e: five 't' bars, slide to 'e', four more bars.
    {
        let mut presses = Vec::new();
        hold(0.0, 8.6, 't', &mut presses); // arm ~0.5 s + 5 bars
        hold(8.63, 15.5, 'e', &mut presses);
        let cues = host_cues(&presses, 17.5);
        let mono = render(&cues, 19.0);
        std::fs::write(out.join("switch-t-e.wav"), wav_bytes(&mono)).expect("wav");
        println!(
            "wrote {} ({} riff cues)",
            out.join("switch-t-e.wav").display(),
            cues.len()
        );
    }

    // performance: a multi-key medley — each key a verse of the one
    // celebration — then release into the plain graceful wind-down.
    {
        let mut presses = Vec::new();
        hold(0.0, 7.0, 't', &mut presses);
        hold(7.03, 12.0, 'e', &mut presses);
        hold(12.03, 16.8, '.', &mut presses);
        hold(16.83, 20.0, ' ', &mut presses);
        hold(20.03, 26.5, 'e', &mut presses);
        let cues = host_cues(&presses, 29.0);
        let mono = render(&cues, 31.0);
        std::fs::write(out.join("performance.wav"), wav_bytes(&mono)).expect("wav");
        println!(
            "wrote {} ({} riff cues)",
            out.join("performance.wav").display(),
            cues.len()
        );
    }
}
