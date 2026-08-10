// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TYPING-VOICE A/B — the bench for the KEYSTROKE voice, the way
//! `bed_audition` is the bench for the bed and `mix_meter` is the bench for
//! the ladder. Neither of those can answer "does the typing still read as a
//! phrase": `mix_meter` reports one isolated event's peak and `bed_audition`
//! scores the BED by subtraction, treating the melody as a fixed reference.
//!
//! Renders ONE typing script through the real synth in several states of the
//! borrowed song key ([`TrailSynth`]'s `song_key`, latched by a riff bar) and
//! measures each: note density, per-note pitch, the interval histogram,
//! interval-sequence autocorrelation (phrase vs scatter), 15-30 Hz roughness,
//! spectral centroid, energy above 2 kHz, and level. Deterministic — fixed
//! seed, fixed script, hand-rolled FFT, no dependency.
//!
//!   cargo run -p aterm-effects --example typing_voice_ab [-- <out_dir>]
//!   afplay target/typing-voice-ab/a-neutral.wav
//!
//! Scenarios:
//! - `a-neutral`     — no celebration: the untransposed typed melody.
//! - `b-after-song-` — a song, then SILENCE, then typing: the key is handed
//!   back by `is_quiet()`, so this must equal `a-neutral` note for note.
//! - `d-leak-`       — a song, then typing at 6-8 cps: the gaps between
//!   keystrokes still reach `is_quiet()`, so the key is handed back.
//! - `e-leak-fast-`  — a song, then typing at 15 cps: the notes overlap, the
//!   synth is never quiet, and the borrowed key OUTLIVES the song. The pitch
//!   spread across held characters is the measurement.
//! - `c-during-song-` — typing under the live riff (mix, not isolated).

use std::io::Write as _;
use std::path::PathBuf;

use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::kitty_sing::song_signature;
use aterm_effects::tone::Tone;
use aterm_effects::trail_sound::{
    CHANNELS, CelebrationGesture, SoundEvent, SoundGesture, SoundKind, SoundVoice, TrailSynth,
};

const SR: u32 = 48_000;
const SEED: u32 = 0xBEDA_0D10;

/// One scripted cue: (time s, gesture, pan, heat).
type Cue = (f32, SoundGesture, f32, f32);

/// The TYPING script every scenario shares, offset by `t0`: three paragraphs
/// of 5-8 cps with pauses, an Enter, and a backspace correction — the same
/// shape `bed_audition` uses, so the two harnesses read against each other.
fn typing_script(t0: f32) -> Vec<Cue> {
    let mut cues: Vec<Cue> = Vec::new();
    let typing = |from: f32, to: f32, cps: f32, heat: f32, cues: &mut Vec<Cue>| {
        let n = ((to - from) * cps) as usize;
        for i in 0..n {
            let t = from + i as f32 / cps;
            let pan = -0.8 + 1.6 * ((t - from) / (to - from).max(1e-3));
            cues.push((t0 + t, SoundGesture::Trail(SoundKind::Typed), pan, heat));
        }
    };
    typing(0.0, 4.0, 6.0, 0.35, &mut cues);
    cues.push((t0 + 4.3, SoundGesture::Trail(SoundKind::Jump), -0.9, 0.4));
    typing(4.5, 8.0, 8.0, 0.6, &mut cues);
    for i in 0..4 {
        cues.push((
            t0 + 8.2 + i as f32 / 8.0,
            SoundGesture::Trail(SoundKind::Backspace),
            0.4 - i as f32 * 0.06,
            0.45,
        ));
    }
    typing(9.0, 12.5, 7.0, 0.55, &mut cues);
    cues
}

/// Seconds per celebration bar — mirrors `kitty_sing::SING_BAR_SECONDS`
/// (pinned equal to the synth's `CELEBRATION_BAR_SECONDS`).
const BAR_S: f32 = aterm_effects::kitty_sing::SING_BAR_SECONDS;

/// A scenario: a name, the cues, and the window (start, end) the measurement
/// runs over so only the TYPING is scored.
struct Scenario {
    name: String,
    cues: Vec<Cue>,
    window: (f32, f32),
    seconds: f32,
    /// Subtract a riff-ONLY render of the same script, isolating the typed
    /// layer (the two renders share seed and cue order, so the riff's rng
    /// draws and voices cancel to the soft-clip residual).
    isolate_typing: bool,
}

/// PRE — no celebration ever fires: `song_key` is structurally 0. This is the
/// keystroke voice every build shipped before the latch reached the canonical
/// `RiffBarSig` payload (2026-08-09, `e2efacaf`).
fn scenario_neutral() -> Scenario {
    Scenario {
        name: "a-neutral".into(),
        cues: typing_script(0.0),
        window: (3.0, 12.5),
        seconds: 14.0,
        isolate_typing: false,
    }
}

/// POST — the cat sang eight bars on `ch`, the song ENDED, and the typing
/// continues. `song_key` is released only by `is_quiet()`, which continuous
/// typing never reaches, so the whole rest of the session is transposed.
fn scenario_after_song(ch: char) -> Scenario {
    let sig = song_signature(ch);
    let mut cues: Vec<Cue> = Vec::new();
    for bar in 0..8u16 {
        cues.push((
            bar as f32 * BAR_S,
            SoundGesture::Celebration(CelebrationGesture::riff_bar(bar, sig)),
            0.0,
            1.0,
        ));
    }
    // The song ends; 3 s of gap lets every riff voice die and the sing duck
    // hand back, so what follows is TYPING ALONE — in the borrowed key.
    let t0 = 8.0 * BAR_S + 3.0;
    cues.extend(typing_script(t0));
    Scenario {
        name: format!("b-after-song-{ch}"),
        cues,
        window: (t0 + 3.0, t0 + 12.5),
        seconds: t0 + 14.0,
        isolate_typing: false,
    }
}

/// THE LEAK. Identical to [`scenario_after_song`] except the typing starts
/// while the last bar is still ringing, so the synth NEVER reaches
/// `is_quiet()` — the only place `song_key` is released. The measurement
/// window opens 3 s after the last bar, by which time every riff voice is
/// dead and the sing duck has handed back: what is scored is TYPING ALONE,
/// still transposed by a song that stopped.
fn scenario_leak(ch: char) -> Scenario {
    let sig = song_signature(ch);
    let mut cues: Vec<Cue> = Vec::new();
    for bar in 0..8u16 {
        cues.push((
            bar as f32 * BAR_S,
            SoundGesture::Celebration(CelebrationGesture::riff_bar(bar, sig)),
            0.0,
            1.0,
        ));
    }
    // Typing overlaps the LAST bar (pushed at 7*BAR_S), so a voice is always
    // on from here to the end — `is_quiet()` never fires.
    let t0 = 7.0 * BAR_S + 0.05;
    cues.extend(typing_script(t0));
    cues.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    // Score from +3 s (duck released, riff silent) for 9 s of pure typing.
    Scenario {
        name: format!("d-leak-{ch}"),
        cues,
        window: (t0 + 3.0, t0 + 12.5),
        seconds: t0 + 14.0,
        isolate_typing: false,
    }
}

/// THE LEAK, at a cadence that never lets the synth fall silent: 15 cps
/// (~180 wpm) with a 105 ms note is a continuous voice, so `is_quiet()` —
/// the ONLY place `song_key` is released — never fires and the borrowed key
/// outlives the song. Scored over the last 2 s, long after the riff is dead.
fn scenario_leak_fast(ch: char) -> Scenario {
    let sig = song_signature(ch);
    let mut cues: Vec<Cue> = Vec::new();
    for bar in 0..8u16 {
        cues.push((
            bar as f32 * BAR_S,
            SoundGesture::Celebration(CelebrationGesture::riff_bar(bar, sig)),
            0.0,
            1.0,
        ));
    }
    let t0 = 7.0 * BAR_S + 0.05;
    for i in 0..(15 * 6) {
        let t = t0 + i as f32 / 15.0;
        cues.push((t, SoundGesture::Trail(SoundKind::Typed), 0.0, 0.6));
    }
    cues.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    Scenario {
        name: format!("e-leak-fast-{ch}"),
        cues,
        window: (t0 + 3.5, t0 + 6.0),
        seconds: t0 + 8.0,
        isolate_typing: false,
    }
}

/// The same key held, typing UNDER the live song — the case the feature was
/// written for. Scored over the riff window; the riff itself is in the mix,
/// which is the point (roughness is a two-layer property).
fn scenario_during_song(ch: char) -> Scenario {
    let sig = song_signature(ch);
    let mut cues: Vec<Cue> = Vec::new();
    for bar in 0..16u16 {
        cues.push((
            bar as f32 * BAR_S,
            SoundGesture::Celebration(CelebrationGesture::riff_bar(bar, sig)),
            0.0,
            1.0,
        ));
    }
    cues.extend(typing_script(0.0));
    cues.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    Scenario {
        name: format!("c-during-song-{ch}"),
        cues,
        window: (3.0, 12.5),
        seconds: 14.0,
        isolate_typing: false,
    }
}

fn render_cues(cues: &[Cue], seconds: f32) -> Vec<f32> {
    let sc = Scenario {
        name: String::new(),
        cues: cues.to_vec(),
        window: (0.0, 0.0),
        seconds,
        isolate_typing: false,
    };
    render_one(&sc)
}

/// The full render, then (when asked) minus the riff-only render.
fn render(sc: &Scenario) -> Vec<f32> {
    let full = render_one(sc);
    if !sc.isolate_typing {
        return full;
    }
    let riff: Vec<Cue> = sc
        .cues
        .iter()
        .filter(|c| matches!(c.1, SoundGesture::Celebration(_)))
        .cloned()
        .collect();
    let only = render_cues(&riff, sc.seconds);
    full.iter().zip(&only).map(|(a, b)| a - b).collect()
}

fn render_one(sc: &Scenario) -> Vec<f32> {
    let frames = (sc.seconds * SR as f32) as usize;
    let mut synth = TrailSynth::new(SR as f32, SEED);
    let mut mono = vec![0.0f32; frames];
    let mut stereo = [0.0f32; 256 * CHANNELS];
    let mut cue_i = 0;
    let mut f = 0;
    while f < frames {
        let n = 256.min(frames - f);
        let t = f as f32 / SR as f32;
        while cue_i < sc.cues.len() && sc.cues[cue_i].0 <= t {
            let (ct, kind, pan, heat) = sc.cues[cue_i].clone();
            synth.push(SoundEvent {
                style: GlowStyle::RainbowKitty,
                voice: SoundVoice::Style,
                kind,
                pan,
                heat,
                hue: (ct * 0.18).fract(),
                gain: 0.4,
                tone: Tone::Technical,
                bed: false, // the KEYSTROKE is what is on trial
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

// ---------------------------------------------------------------------------
// FFT (radix-2 DIT, hand-rolled — no dependency), lifted from bed_audition
// ---------------------------------------------------------------------------

fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -core::f32::consts::TAU / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        let mut i = 0;
        while i < n {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (ur, ui) = (re[i + k], im[i + k]);
                let (ar, ai) = (re[i + k + len / 2], im[i + k + len / 2]);
                let (vr, vi) = (ar * cr - ai * ci, ar * ci + ai * cr);
                re[i + k] = ur + vr;
                im[i + k] = ui + vi;
                re[i + k + len / 2] = ur - vr;
                im[i + k + len / 2] = ui - vi;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
            i += len;
        }
        len <<= 1;
    }
}

const FFT_N: usize = 2048;

fn hann() -> Vec<f32> {
    (0..FFT_N)
        .map(|i| 0.5 * (1.0 - (core::f32::consts::TAU * i as f32 / FFT_N as f32).cos()))
        .collect()
}

/// Magnitude spectrum of one 2048-sample window.
fn mag_at(x: &[f32], start: usize, w: &[f32]) -> Vec<f32> {
    let mut re = vec![0.0f32; FFT_N];
    let mut im = vec![0.0f32; FFT_N];
    for i in 0..FFT_N {
        re[i] = x.get(start + i).copied().unwrap_or(0.0) * w[i];
    }
    fft(&mut re, &mut im);
    (0..FFT_N / 2)
        .map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt())
        .collect()
}

/// Dominant partial (Hz) in `lo..hi`, parabolically interpolated. The
/// RainbowKitty doop is a 25 % pulse whose FUNDAMENTAL is the loudest
/// partial by design, so the peak IS the note.
fn peak_hz(mag: &[f32], lo: f32, hi: f32) -> f32 {
    let hz_per_bin = SR as f32 / FFT_N as f32;
    let k0 = (lo / hz_per_bin) as usize;
    let k1 = ((hi / hz_per_bin) as usize).min(mag.len() - 2);
    let mut best = k0.max(1);
    for k in k0.max(1)..=k1 {
        if mag[k] > mag[best] {
            best = k;
        }
    }
    let (a, b, c) = (mag[best - 1], mag[best], mag[best + 1]);
    let denom = a - 2.0 * b + c;
    let d = if denom.abs() < 1e-12 {
        0.0
    } else {
        0.5 * (a - c) / denom
    };
    (best as f32 + d) * hz_per_bin
}

/// Onset frames: an amplitude-envelope rise crossing a fraction of the
/// window's peak after a dip. Deterministic and script-independent.
fn onsets(x: &[f32], from: usize, to: usize) -> Vec<usize> {
    const W: usize = 64; // ~1.3 ms envelope window
    let env: Vec<f32> = (from..to)
        .step_by(W)
        .map(|s| {
            x[s..(s + W).min(to)]
                .iter()
                .fold(0.0f32, |m, v| m.max(v.abs()))
        })
        .collect();
    let peak = env.iter().fold(0.0f32, |m, &v| m.max(v));
    let thr = peak * 0.25;
    let mut out = Vec::new();
    let mut armed = true;
    for (i, &e) in env.iter().enumerate() {
        if armed && e > thr {
            out.push(from + i * W);
            armed = false;
        } else if !armed && e < thr * 0.4 {
            armed = true;
        }
    }
    out
}

/// dBFS RMS with a -120 floor.
fn rms_db(x: &[f32]) -> f64 {
    let ms = x.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>() / x.len().max(1) as f64;
    20.0 * ms.sqrt().max(1e-6).log10()
}

/// Fraction of envelope-modulation energy in the 15-30 Hz sensory-roughness
/// band (bed_audition's `mix_roughness_15_30hz`, same construction).
fn roughness(x: &[f32]) -> f64 {
    const EW: usize = 64;
    let env: Vec<f32> = x
        .chunks(EW)
        .map(|c| (c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let n = FFT_N.min(env.len().next_power_of_two());
    if n < 256 {
        return 0.0;
    }
    let env_sr = SR as f32 / EW as f32;
    let w: Vec<f32> = (0..n)
        .map(|i| 0.5 * (1.0 - (core::f32::consts::TAU * i as f32 / n as f32).cos()))
        .collect();
    let mut total = 0.0f64;
    let mut band = 0.0f64;
    let mut start = 0;
    let mut frames = 0;
    while start + n <= env.len() {
        let mean = env[start..start + n].iter().sum::<f32>() / n as f32;
        let mut re: Vec<f32> = (0..n).map(|i| (env[start + i] - mean) * w[i]).collect();
        let mut im = vec![0.0f32; n];
        fft(&mut re, &mut im);
        let hz_per_bin = env_sr / n as f32;
        for k in 1..n / 2 {
            let e = f64::from(re[k] * re[k] + im[k] * im[k]);
            total += e;
            let hz = k as f32 * hz_per_bin;
            if (15.0..=30.0).contains(&hz) {
                band += e;
            }
        }
        start += n / 2;
        frames += 1;
    }
    if frames == 0 || total < 1e-12 {
        0.0
    } else {
        band / total
    }
}

/// Peak normalized autocorrelation of the INTERVAL sequence over lags
/// 2..=len/2 — "does the line repeat a motif" (phrase) or not (scatter).
fn seq_autocorr_peak(v: &[f64]) -> f64 {
    let n = v.len();
    if n < 8 {
        return 0.0;
    }
    let mean = v.iter().sum::<f64>() / n as f64;
    let var = v.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>();
    if var < 1e-9 {
        return 1.0; // a constant line is maximally "repetitive"
    }
    let mut best = 0.0f64;
    for lag in 2..=n / 2 {
        let mut acc = 0.0;
        for i in 0..n - lag {
            acc += (v[i] - mean) * (v[i + lag] - mean);
        }
        best = best.max(acc / var);
    }
    best
}

struct Report {
    name: String,
    notes: usize,
    density_hz: f64,
    median_hz: f64,
    lo_hz: f64,
    hi_hz: f64,
    span_semitones: f64,
    mean_abs_interval_st: f64,
    interval_hist: Vec<(i32, usize)>,
    unison_pct: f64,
    interval_autocorr: f64,
    roughness: f64,
    centroid_hz: f64,
    /// Fraction of spectral energy above 2 kHz — chirp vs body.
    hi_frac: f64,
    /// Lowest bin holding >1 % of the peak magnitude (Hz) — how deep the
    /// voice's skirt reaches.
    low_edge_hz: f64,
    rms_db: f64,
    peak_db: f64,
}

fn analyze(name: &str, x: &[f32], window: (f32, f32)) -> Report {
    let from = (window.0 * SR as f32) as usize;
    let to = ((window.1 * SR as f32) as usize).min(x.len());
    let seg = &x[from..to];
    let w = hann();
    let ons = onsets(x, from, to);
    // Per-note pitch: a 2048-window from the onset. The doop is ~105 ms, so
    // the 43 ms window sits inside the note's body.
    let hzs: Vec<f64> = ons
        .iter()
        .filter(|&&s| s + FFT_N < to)
        .map(|&s| f64::from(peak_hz(&mag_at(x, s, &w), 150.0, 6000.0)))
        .collect();
    let mut sorted = hzs.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    let lo = sorted.first().copied().unwrap_or(0.0);
    let hi = sorted.last().copied().unwrap_or(0.0);
    let intervals: Vec<f64> = hzs
        .windows(2)
        .map(|p| 12.0 * (p[1] / p[0]).log2())
        .collect();
    let mut hist: std::collections::BTreeMap<i32, usize> = Default::default();
    for &iv in &intervals {
        *hist.entry(iv.round() as i32).or_default() += 1;
    }
    let unison = hist.get(&0).copied().unwrap_or(0) as f64 / intervals.len().max(1) as f64 * 100.0;
    // Global centroid over the window.
    let mut num = 0.0f64;
    let mut den = 0.0f64;
    let mut hi_e = 0.0f64;
    let mut acc = vec![0.0f64; FFT_N / 2];
    let hz_per_bin = f64::from(SR) / FFT_N as f64;
    let mut s = from;
    while s + FFT_N <= to {
        for (k, &m) in mag_at(x, s, &w).iter().enumerate() {
            let e = f64::from(m) * f64::from(m);
            acc[k] += e;
            num += e * k as f64 * hz_per_bin;
            den += e;
            if k as f64 * hz_per_bin > 2000.0 {
                hi_e += e;
            }
        }
        s += FFT_N;
    }
    let peak_bin = acc.iter().cloned().fold(0.0f64, f64::max);
    let low_edge = acc
        .iter()
        .position(|&e| e > peak_bin * 1e-4)
        .map_or(0.0, |k| k as f64 * hz_per_bin);
    Report {
        name: name.into(),
        notes: hzs.len(),
        density_hz: hzs.len() as f64 / f64::from(window.1 - window.0),
        median_hz: median,
        lo_hz: lo,
        hi_hz: hi,
        span_semitones: if lo > 0.0 {
            12.0 * (hi / lo).log2()
        } else {
            0.0
        },
        mean_abs_interval_st: intervals.iter().map(|v| v.abs()).sum::<f64>()
            / intervals.len().max(1) as f64,
        interval_hist: hist.into_iter().collect(),
        unison_pct: unison,
        interval_autocorr: seq_autocorr_peak(&intervals),
        roughness: roughness(seg),
        centroid_hz: if den < 1e-12 { 0.0 } else { num / den },
        hi_frac: if den < 1e-12 { 0.0 } else { hi_e / den },
        low_edge_hz: low_edge,
        rms_db: rms_db(seg),
        peak_db: 20.0 * f64::from(seg.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6)).log10(),
    }
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
    let out: PathBuf = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("target/typing-voice-ab"), PathBuf::from);
    std::fs::create_dir_all(&out).expect("out dir");

    let mut scenarios = vec![scenario_neutral()];
    // Real held keys, spanning the root/mode classes the mixer produces.
    for ch in ['a', 'e', 'k', 'o', 'z', ' '] {
        scenarios.push(scenario_after_song(ch));
    }
    for ch in ['a', 'e', 'k', 'o', 'z', ' '] {
        scenarios.push(scenario_leak(ch));
    }
    for ch in ['a', 'e', 'k', 'o', 'z', ' '] {
        scenarios.push(scenario_leak_fast(ch));
    }
    for ch in ['a', 'e', 'z'] {
        scenarios.push(scenario_during_song(ch));
    }
    println!("per-key song axes: root = sig%5-2, mode = [0,2,-1][sig%3]");
    for ch in ['a', 'e', 'k', 'o', 'z', ' '] {
        let sig = song_signature(ch);
        println!(
            "  {:?}  sig={:<12} root={:+}  mode={:+}  root+mode={:+}",
            ch,
            sig,
            (sig % 5) as i32 - 2,
            [0, 2, -1][(sig % 3) as usize],
            (sig % 5) as i32 - 2 + [0, 2, -1][(sig % 3) as usize]
        );
    }

    println!(
        "{:<22} {:>5} {:>7} {:>8} {:>8} {:>7} {:>7} {:>7} {:>6} {:>7} {:>8} {:>8} {:>7} {:>8} {:>8}",
        "scenario",
        "notes",
        "n/s",
        "med Hz",
        "lo Hz",
        "hi Hz",
        "span st",
        "|ivl|st",
        "uni%",
        "ivl-ac",
        "rough",
        "cent Hz",
        "hi>2k",
        "low Hz",
        "rms dB"
    );
    let mut reports = Vec::new();
    for sc in &scenarios {
        let mono = render(sc);
        std::fs::write(out.join(format!("{}.wav", sc.name)), wav_bytes(&mono)).expect("wav");
        let r = analyze(&sc.name, &mono, sc.window);
        println!(
            "{:<22} {:>5} {:>7.2} {:>8.1} {:>8.1} {:>7.1} {:>7.2} {:>7.2} {:>6.1} {:>7.3} {:>8.4} {:>8.0} {:>7.3} {:>8.0} {:>8.1}",
            r.name,
            r.notes,
            r.density_hz,
            r.median_hz,
            r.lo_hz,
            r.hi_hz,
            r.span_semitones,
            r.mean_abs_interval_st,
            r.unison_pct,
            r.interval_autocorr,
            r.roughness,
            r.centroid_hz,
            r.hi_frac,
            r.low_edge_hz,
            r.rms_db
        );
        reports.push(r);
    }
    println!("\nINTERVAL HISTOGRAMS (semitones between consecutive typed notes)");
    for r in &reports {
        let cells: Vec<String> = r
            .interval_hist
            .iter()
            .map(|(k, v)| format!("{k:+}:{v}"))
            .collect();
        println!("  {:<22} {}", r.name, cells.join("  "));
    }
    println!("\npeak dBFS");
    for r in &reports {
        println!("  {:<22} {:>7.2}", r.name, r.peak_db);
    }

    // Machine-readable, for the next agent.
    let mut f = std::fs::File::create(out.join("metrics.json")).expect("metrics");
    writeln!(f, "[").unwrap();
    for (i, r) in reports.iter().enumerate() {
        writeln!(
            f,
            "  {{\"name\":\"{}\",\"notes\":{},\"density_hz\":{:.4},\"median_hz\":{:.2},\"lo_hz\":{:.2},\"hi_hz\":{:.2},\"span_semitones\":{:.3},\"mean_abs_interval_st\":{:.3},\"unison_pct\":{:.2},\"interval_autocorr\":{:.4},\"roughness\":{:.5},\"centroid_hz\":{:.1},\"hi_frac\":{:.4},\"low_edge_hz\":{:.1},\"rms_db\":{:.2},\"peak_db\":{:.2}}}{}",
            r.name,
            r.notes,
            r.density_hz,
            r.median_hz,
            r.lo_hz,
            r.hi_hz,
            r.span_semitones,
            r.mean_abs_interval_st,
            r.unison_pct,
            r.interval_autocorr,
            r.roughness,
            r.centroid_hz,
            r.hi_frac,
            r.low_edge_hz,
            r.rms_db,
            r.peak_db,
            if i + 1 == reports.len() { "" } else { "," }
        )
        .unwrap();
    }
    writeln!(f, "]").unwrap();
    println!(
        "\nwrote {} wavs + metrics.json to {}",
        reports.len(),
        out.display()
    );
}
