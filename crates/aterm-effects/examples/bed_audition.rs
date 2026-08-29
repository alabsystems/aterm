// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AMBIENT-BED TOURNAMENT render harness — the judging bench for the bed
//! redesign (`trail_sound::BedVariant`). The owner dislikes the shipping low
//! drone ("don't keep it if it doesn't sound good"; beds ship OFF behind
//! `trail_sound_bed`), so every candidate is rendered DETERMINISTICALLY
//! (fixed seed, fixed 20 s script) against the real rainbow kitty melody — beds are
//! judged in context, not in isolation — and measured, not just auditioned:
//!
//! per candidate, into `<out_dir>/` (default `target/bed-audition/`):
//! - `<name>.wav` — the mono f32 mix (hand-rolled 44-byte WAV header:
//!   format 3 / IEEE float, so no dependency and no quantization);
//! - `<name>.spectrogram.png` — log-frequency spectrogram (2048-point
//!   hand-rolled radix-2 FFT, Hann window, hop 1024; single-hue
//!   dark→light sequential colormap — magnitude is a MAGNITUDE, so it gets
//!   a sequential ramp, never a rainbow);
//! - `<name>.metrics.json` — the judge's numbers (see [`Metrics`]).
//!
//! The BED-ONLY signal is isolated by subtraction: the C4/SILENCE render is
//! the melody reference (identical seed + script ⇒ identical voices), so
//! `bed ≈ candidate − reference`. The chain after the bed mix (soft-clip,
//! DC blocker) is a hair nonlinear, but at bed levels (≤ −40 dBFS) the
//! residual is negligible against the −50 dBFS audibility threshold — and
//! the estimate is deterministic, which is what a tournament needs.
//!
//!   cargo run -p aterm-effects --example bed_audition [-- <out_dir>]
//!   afplay target/bed-audition/c1-chord-drift.wav
//!
//! Tests (run with `cargo test -p aterm-effects --example bed_audition`):
//! harness determinism (two renders ⇒ byte-identical WAV bytes) and metric
//! sanity (an all-zero signal scores zero flux/centroid/roughness/audibility
//! and floor loudness).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::tone::Tone;
use aterm_effects::trail_sound::{
    BedVariant, CHANNELS, SoundEvent, SoundGesture, SoundKind, SoundVoice, TrailSynth,
};

/// Output stream rate — the host's canonical rate, shared with
/// `trail_sound_demo`.
const SR: u32 = 48_000;

/// Fixed audition length: long enough for two C2 breaths and most of a C1
/// chord cycle to land inside the melody, plus a 4 s tail for the exhale.
const SECONDS: f32 = 20.0;

/// Fixed harness seed — the whole tournament replays bit-exactly from it.
const SEED: u32 = 0xBEDA_0D10;

/// The entrants, in bracket order. Filename stems double as candidate ids
/// in the report, so keep them stable.
const CANDIDATES: [(&str, BedVariant); 5] = [
    ("c0-current", BedVariant::Current),
    ("c1-chord-drift", BedVariant::ChordDrift),
    ("c2-breathing", BedVariant::Breathing),
    ("c3-shimmer", BedVariant::Shimmer),
    ("c4-silence", BedVariant::Silence),
];

// ---------------------------------------------------------------------------
// The scripted session
// ---------------------------------------------------------------------------

/// One scripted gesture: (time s, kind, pan, heat).
type Cue = (f32, SoundKind, f32, f32);

/// A MODERATE 16 s typing session + 4 s tail — deliberately not a flood
/// (the flood law has its own lib proof): paragraphs of 5–8 cps typing with
/// pauses, two Enter jumps, and one backspace correction, so the beds are
/// heard doing their real job — sitting under a living melody, breathing
/// through the gaps.
fn scenario() -> Vec<Cue> {
    let mut cues: Vec<Cue> = Vec::new();
    let typing = |from: f32, to: f32, cps: f32, heat: f32, cues: &mut Vec<Cue>| {
        let n = ((to - from) * cps) as usize;
        for i in 0..n {
            let t = from + i as f32 / cps;
            // Pan walks the line left→right like a real cursor.
            let pan = -0.8 + 1.6 * ((t - from) / (to - from).max(1e-3));
            cues.push((t, SoundKind::Typed, pan, heat));
        }
    };
    typing(0.0, 4.0, 6.0, 0.35, &mut cues); // warm-up paragraph
    cues.push((4.3, SoundKind::Jump, -0.9, 0.4)); // Enter
    typing(4.5, 8.0, 8.0, 0.6, &mut cues); // brisker paragraph
    for i in 0..4 {
        // A correction: four backspaces walking left.
        cues.push((
            8.2 + i as f32 / 8.0,
            SoundKind::Backspace,
            0.4 - i as f32 * 0.06,
            0.45,
        ));
    }
    typing(9.0, 12.5, 7.0, 0.55, &mut cues);
    cues.push((12.8, SoundKind::Jump, -0.9, 0.5)); // Enter
    typing(13.0, 16.0, 5.0, 0.4, &mut cues); // trailing thought
    // 16..20 s: silence — the tail where a bed's exhale (or its refusal to
    // die) is plainly audible and measurable.
    cues
}

/// Render one candidate: fresh synth, fixed seed, the shared script, mono
/// downmix. Bit-deterministic per (SEED, script, variant) — the lib proof
/// `bed_variants_render_deterministically_and_decay_to_exact_silence`
/// covers the engine; [`tests::two_runs_produce_byte_identical_wavs`] pins
/// the whole harness path, WAV bytes included.
fn render_candidate(variant: BedVariant, seconds: f32) -> Vec<f32> {
    let cues = scenario();
    let frames = (seconds * SR as f32) as usize;
    let mut synth = TrailSynth::new(SR as f32, SEED);
    synth.set_bed_variant(variant);
    let mut mono = vec![0.0f32; frames];
    let mut stereo = [0.0f32; 256 * CHANNELS];
    let mut cue_i = 0;
    let mut f = 0;
    while f < frames {
        let n = 256.min(frames - f);
        let t = f as f32 / SR as f32;
        while cue_i < cues.len() && cues[cue_i].0 <= t {
            let (ct, kind, pan, heat) = cues[cue_i];
            synth.push(SoundEvent {
                style: GlowStyle::RainbowKitty,
                voice: SoundVoice::Style,
                kind: SoundGesture::Trail(kind),
                pan,
                heat,
                hue: (ct * 0.18).fract(),
                gain: 0.4, // default trail_sound_volume
                tone: Tone::Technical,
                // Bed ON: the audition audits beds — that is the point.
                bed: true,
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
// WAV — 44-byte header, IEEE-float mono
// ---------------------------------------------------------------------------

/// The complete WAV file as bytes (44-byte RIFF/fmt/data header + raw f32
/// LE samples, format 3 = IEEE float, mono). In-memory so the determinism
/// test can compare byte-for-byte without touching disk.
fn wav_bytes(mono: &[f32]) -> Vec<u8> {
    let data_len = (mono.len() * 4) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    w.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&SR.to_le_bytes());
    w.extend_from_slice(&(SR * 4).to_le_bytes()); // byte rate
    w.extend_from_slice(&4u16.to_le_bytes()); // block align
    w.extend_from_slice(&32u16.to_le_bytes()); // bits per sample
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for &x in mono {
        w.extend_from_slice(&x.to_le_bytes());
    }
    w
}

// ---------------------------------------------------------------------------
// FFT + spectral frames
// ---------------------------------------------------------------------------

/// In-place iterative radix-2 DIT FFT. Hand-rolled (~30 lines) so the
/// harness adds no DSP dependency; n must be a power of two.
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

/// FFT window / hop: 2048-sample Hann frames, half overlap ⇒ 1024 magnitude
/// bins per frame (≈23.4 Hz resolution, ≈21 ms hop) — the "~1024-bin FFT"
/// of the brief.
const FFT_N: usize = 2048;
const HOP: usize = 1024;

/// Hann-windowed magnitude frames, normalized so a full-scale sine reads
/// ≈1.0 in its bin (mag / (Σwindow/2)).
fn spectral_frames(x: &[f32]) -> Vec<Vec<f32>> {
    let hann: Vec<f32> = (0..FFT_N)
        .map(|i| 0.5 * (1.0 - (core::f32::consts::TAU * i as f32 / FFT_N as f32).cos()))
        .collect();
    let norm = 2.0 / hann.iter().sum::<f32>();
    let mut frames = Vec::new();
    let mut start = 0;
    while start + FFT_N <= x.len() {
        let mut re: Vec<f32> = (0..FFT_N).map(|i| x[start + i] * hann[i]).collect();
        let mut im = vec![0.0f32; FFT_N];
        fft(&mut re, &mut im);
        frames.push(
            (0..FFT_N / 2)
                .map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt() * norm)
                .collect(),
        );
        start += HOP;
    }
    frames
}

// ---------------------------------------------------------------------------
// Metrics — the judge's numbers
// ---------------------------------------------------------------------------

/// One candidate's scorecard. All bed_* rows are computed on the BED-ONLY
/// difference signal; roughness is computed on the full mix (beating between
/// bed and melody partials is exactly the interaction being judged).
struct Metrics {
    /// Mean relative spectral flux of the bed: frame-to-frame positive
    /// magnitude change / previous frame energy. LOW = static drone (the
    /// complaint); moderate = the texture moves.
    bed_spectral_flux: f64,
    /// Energy-weighted mean frequency (Hz) of the bed across all frames —
    /// where the bed lives (drone ≈ low, shimmer ≈ kHz).
    bed_spectral_centroid_hz: f64,
    /// Peak of the normalized autocorrelation of the bed's RMS envelope,
    /// lags 0.5–10 s. HIGH = the bed repeats itself audibly (monotony —
    /// the disliked property); low = it evolves.
    bed_envelope_autocorr_peak: f64,
    /// Fraction of the full mix's envelope-modulation energy in the
    /// 15–30 Hz beat band — the classic sensory-roughness region for
    /// partial-pair beating. Higher = harsher.
    mix_roughness_15_30hz: f64,
    /// RMS loudness (dBFS, floored at −120) of bed and melody, and the
    /// headline "how loud is the bed relative to the notes".
    bed_rms_db: f64,
    melody_rms_db: f64,
    bed_minus_melody_db: f64,
    /// % of 50 ms windows where the bed alone exceeds −50 dBFS — how much
    /// of the session the bed is actually audible.
    bed_audible_pct: f64,
}

/// RMS in dBFS with a −120 dB floor (an exact-zero signal reads −120.0, not
/// −inf, so the JSON stays plain numbers).
fn rms_db(x: &[f32]) -> f64 {
    let ms = x.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>() / x.len().max(1) as f64;
    20.0 * ms.sqrt().max(1e-6).log10()
}

/// Mean relative spectral flux over frames with energy; a silent signal has
/// no energetic frames and scores exactly 0.
fn spectral_flux(frames: &[Vec<f32>]) -> f64 {
    let mut acc = 0.0f64;
    let mut n = 0u32;
    for w in frames.windows(2) {
        let prev_e: f64 = w[0].iter().map(|&m| f64::from(m)).sum();
        if prev_e < 1e-9 {
            continue;
        }
        let pos: f64 = w[0]
            .iter()
            .zip(&w[1])
            .map(|(&a, &b)| f64::from(b - a).max(0.0))
            .sum();
        acc += pos / prev_e;
        n += 1;
    }
    if n == 0 { 0.0 } else { acc / f64::from(n) }
}

/// Global energy-weighted spectral centroid (Hz); exactly 0 for silence.
fn spectral_centroid_hz(frames: &[Vec<f32>]) -> f64 {
    let hz_per_bin = f64::from(SR) / FFT_N as f64;
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for frame in frames {
        for (k, &m) in frame.iter().enumerate() {
            let e = f64::from(m) * f64::from(m);
            num += e * k as f64 * hz_per_bin;
            den += e;
        }
    }
    if den < 1e-12 { 0.0 } else { num / den }
}

/// Monotony: peak normalized autocorrelation of the hop-rate RMS envelope
/// over lags 0.5–10 s. A flat or silent envelope (variance ~0) scores 0 —
/// constancy is reported by the loudness/audibility rows, not disguised as
/// periodicity here.
fn envelope_autocorr_peak(x: &[f32]) -> f64 {
    let env: Vec<f64> = x
        .chunks(HOP)
        .map(|c| {
            (c.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>() / c.len() as f64).sqrt()
        })
        .collect();
    let n = env.len();
    let mean = env.iter().sum::<f64>() / n.max(1) as f64;
    let e0: f64 = env.iter().map(|&v| (v - mean) * (v - mean)).sum();
    if e0 < 1e-12 {
        return 0.0;
    }
    let hop_rate = f64::from(SR) / HOP as f64;
    let (lag_lo, lag_hi) = (
        (0.5 * hop_rate) as usize,
        ((10.0 * hop_rate) as usize).min(n.saturating_sub(1)),
    );
    let mut peak = 0.0f64;
    for lag in lag_lo..=lag_hi {
        let r: f64 = (0..n - lag)
            .map(|i| (env[i] - mean) * (env[i + lag] - mean))
            .sum();
        peak = peak.max(r / e0);
    }
    peak
}

/// Roughness estimate: rectify → 80 Hz one-pole envelope follower →
/// decimate to 480 Hz → 8192-point FFT of the middle stretch → energy in
/// the 15–30 Hz beat band as a fraction of the 2–240 Hz modulation total.
/// Silence (or any unmodulated signal) scores 0.
fn roughness_15_30(x: &[f32]) -> f64 {
    let k = 1.0 - (-core::f32::consts::TAU * 80.0 / SR as f32).exp();
    let mut lp = 0.0f32;
    let mut env: Vec<f32> = Vec::with_capacity(x.len() / 100 + 1);
    for (i, &v) in x.iter().enumerate() {
        lp += k * (v.abs() - lp);
        if i % 100 == 0 {
            env.push(lp); // 480 Hz envelope rate
        }
    }
    const N: usize = 8192;
    if env.len() < N {
        env.resize(N, 0.0);
    }
    let start = (env.len() - N) / 2;
    let mean = env[start..start + N].iter().sum::<f32>() / N as f32;
    let mut re: Vec<f32> = env[start..start + N].iter().map(|&v| v - mean).collect();
    let mut im = vec![0.0f32; N];
    fft(&mut re, &mut im);
    let env_rate = SR as f64 / 100.0;
    let bin = |hz: f64| ((hz * N as f64 / env_rate) as usize).min(N / 2);
    let band = |lo: usize, hi: usize| -> f64 {
        (lo..=hi)
            .map(|i| f64::from(re[i]) * f64::from(re[i]) + f64::from(im[i]) * f64::from(im[i]))
            .sum()
    };
    let beat = band(bin(15.0), bin(30.0));
    let total = band(bin(2.0), N / 2 - 1);
    if total < 1e-12 { 0.0 } else { beat / total }
}

/// % of 50 ms windows above −50 dBFS RMS.
fn audible_pct_above_minus50(x: &[f32]) -> f64 {
    const WIN: usize = (SR as usize) / 20; // 50 ms
    let thresh = 10.0f64.powf(-50.0 / 20.0);
    let mut over = 0usize;
    let mut total = 0usize;
    for c in x.chunks(WIN) {
        let rms =
            (c.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>() / c.len() as f64).sqrt();
        total += 1;
        if rms > thresh {
            over += 1;
        }
    }
    if total == 0 {
        0.0
    } else {
        100.0 * over as f64 / total as f64
    }
}

/// The full scorecard for one candidate mix against the melody reference.
fn metrics(mix: &[f32], melody: &[f32]) -> Metrics {
    let bed: Vec<f32> = mix.iter().zip(melody).map(|(&a, &b)| a - b).collect();
    let bed_frames = spectral_frames(&bed);
    let bed_db = rms_db(&bed);
    let mel_db = rms_db(melody);
    Metrics {
        bed_spectral_flux: spectral_flux(&bed_frames),
        bed_spectral_centroid_hz: spectral_centroid_hz(&bed_frames),
        bed_envelope_autocorr_peak: envelope_autocorr_peak(&bed),
        mix_roughness_15_30hz: roughness_15_30(mix),
        bed_rms_db: bed_db,
        melody_rms_db: mel_db,
        bed_minus_melody_db: bed_db - mel_db,
        bed_audible_pct: audible_pct_above_minus50(&bed),
    }
}

/// Hand-formatted JSON (the harness stays dependency-free beyond the
/// existing dev `png`).
fn metrics_json(name: &str, m: &Metrics) -> String {
    format!(
        "{{\n  \"candidate\": \"{name}\",\n  \"seconds\": {SECONDS},\n  \"sample_rate\": {SR},\n  \
         \"bed_spectral_flux\": {:.6},\n  \"bed_spectral_centroid_hz\": {:.2},\n  \
         \"bed_envelope_autocorr_peak\": {:.4},\n  \"mix_roughness_15_30hz\": {:.6},\n  \
         \"bed_rms_db\": {:.2},\n  \"melody_rms_db\": {:.2},\n  \"bed_minus_melody_db\": {:.2},\n  \
         \"bed_audible_pct_above_minus50dbfs\": {:.2}\n}}\n",
        m.bed_spectral_flux,
        m.bed_spectral_centroid_hz,
        m.bed_envelope_autocorr_peak,
        m.mix_roughness_15_30hz,
        m.bed_rms_db,
        m.melody_rms_db,
        m.bed_minus_melody_db,
        m.bed_audible_pct,
    )
}

// ---------------------------------------------------------------------------
// Spectrogram PNG
// ---------------------------------------------------------------------------

/// Spectrogram rows (top = 24 kHz, bottom = 30 Hz, log-frequency).
const SPEC_H: usize = 512;
const SPEC_F_LO: f64 = 30.0;

/// Single-hue sequential colormap (deep navy → light blue), monotonic in
/// lightness: magnitude is a magnitude, so it gets one hue dark→light —
/// never a rainbow (rainbows lie about order and break under CVD).
fn magnitude_color(t: f32) -> [u8; 3] {
    const STOPS: [[f32; 3]; 5] = [
        [0.04, 0.06, 0.13],
        [0.10, 0.22, 0.43],
        [0.20, 0.45, 0.70],
        [0.55, 0.78, 0.94],
        [0.94, 0.98, 1.00],
    ];
    let x = t.clamp(0.0, 1.0) * (STOPS.len() - 1) as f32;
    let i = (x as usize).min(STOPS.len() - 2);
    let f = x - i as f32;
    let mut rgb = [0u8; 3];
    for c in 0..3 {
        rgb[c] = ((STOPS[i][c] + (STOPS[i + 1][c] - STOPS[i][c]) * f) * 255.0) as u8;
    }
    rgb
}

/// Render the log-frequency spectrogram to `path`: −100..−20 dBFS mapped
/// onto the sequential ramp; each pixel row takes the PEAK magnitude of the
/// bins it spans so narrow high partials (the shimmer) stay visible after
/// the log warp.
fn write_spectrogram(path: &Path, frames: &[Vec<f32>]) {
    let w = frames.len().max(1);
    let f_hi = f64::from(SR) / 2.0;
    let hz_per_bin = f64::from(SR) / FFT_N as f64;
    let mut img = vec![0u8; w * SPEC_H * 3];
    for (x, frame) in frames.iter().enumerate() {
        for y in 0..SPEC_H {
            // Row band edges on the log axis (y=0 is the top / f_hi).
            let fa = f_hi * (SPEC_F_LO / f_hi).powf(y as f64 / SPEC_H as f64);
            let fb = f_hi * (SPEC_F_LO / f_hi).powf((y + 1) as f64 / SPEC_H as f64);
            let (lo, hi) = (
                ((fb / hz_per_bin) as usize).min(FFT_N / 2 - 1),
                ((fa / hz_per_bin) as usize).min(FFT_N / 2 - 1),
            );
            let mut mag = 0.0f32;
            for &m in &frame[lo..=hi.max(lo)] {
                mag = mag.max(m);
            }
            let db = 20.0 * mag.max(1e-9).log10();
            let t = ((db + 100.0) / 80.0).clamp(0.0, 1.0);
            let rgb = magnitude_color(t);
            let px = (y * w + x) * 3;
            img[px..px + 3].copy_from_slice(&rgb);
        }
    }
    let file = std::fs::File::create(path).expect("create spectrogram png");
    let mut enc = aterm_png::Encoder::new(std::io::BufWriter::new(file), w as u32, SPEC_H as u32);
    enc.set_color(aterm_png::ColorType::Rgb);
    enc.set_depth(aterm_png::BitDepth::Eight);
    enc.write_header()
        .expect("png header")
        .write_image_data(&img)
        .expect("png data");
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    // Workspace target/ by default, wherever the harness is invoked from.
    let out_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bed-audition"));
    std::fs::create_dir_all(&out_dir).expect("create out dir");
    let out_dir = out_dir.canonicalize().expect("canonicalize out dir");

    // The melody reference IS the C4 render — one render, two roles.
    let melody = render_candidate(BedVariant::Silence, SECONDS);

    println!("candidate        flux    centroid  autocorr  rough    bed dB  rel dB  audible%");
    for (name, variant) in CANDIDATES {
        let mix = if variant == BedVariant::Silence {
            melody.clone()
        } else {
            render_candidate(variant, SECONDS)
        };
        let m = metrics(&mix, &melody);

        let wav_path = out_dir.join(format!("{name}.wav"));
        std::fs::File::create(&wav_path)
            .and_then(|mut f| f.write_all(&wav_bytes(&mix)))
            .expect("write wav");
        let png_path = out_dir.join(format!("{name}.spectrogram.png"));
        write_spectrogram(&png_path, &spectral_frames(&mix));
        let json_path = out_dir.join(format!("{name}.metrics.json"));
        std::fs::write(&json_path, metrics_json(name, &m)).expect("write metrics json");

        println!(
            "{name:<16} {:>6.4}  {:>8.1}  {:>8.3}  {:>6.4}  {:>6.1}  {:>6.1}  {:>7.1}",
            m.bed_spectral_flux,
            m.bed_spectral_centroid_hz,
            m.bed_envelope_autocorr_peak,
            m.mix_roughness_15_30hz,
            m.bed_rms_db,
            m.bed_minus_melody_db,
            m.bed_audible_pct,
        );
    }
    println!("artifacts in {}", out_dir.display());
}

// ---------------------------------------------------------------------------
// Proofs
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// HARNESS DETERMINISM: for every candidate, two full renders produce
    /// byte-identical WAV files (header + payload) — the tournament's
    /// artifacts are reproducible evidence, not one-off recordings.
    #[test]
    fn two_runs_produce_byte_identical_wavs() {
        for (name, variant) in CANDIDATES {
            let a = wav_bytes(&render_candidate(variant, SECONDS));
            let b = wav_bytes(&render_candidate(variant, SECONDS));
            assert_eq!(a, b, "{name}: WAV bytes differed between runs");
        }
    }

    /// METRIC SANITY: an all-zero signal scores exactly zero flux, centroid,
    /// monotony, roughness and audibility, and floor loudness — so a silent
    /// bed can never win or lose a metric by numerical accident.
    #[test]
    fn metrics_of_silence_are_zero() {
        let zeros = vec![0.0f32; (SECONDS * SR as f32) as usize];
        let m = metrics(&zeros, &zeros);
        assert_eq!(m.bed_spectral_flux, 0.0);
        assert_eq!(m.bed_spectral_centroid_hz, 0.0);
        assert_eq!(m.bed_envelope_autocorr_peak, 0.0);
        assert_eq!(m.mix_roughness_15_30hz, 0.0);
        assert_eq!(m.bed_audible_pct, 0.0);
        assert!(m.bed_rms_db <= -119.0, "loudness floor: {}", m.bed_rms_db);
    }

    /// The scripted session is itself deterministic and MODERATE (the brief:
    /// beds must be judged under a living melody, not a flood): a sanity pin
    /// on the cue count and time range so a future edit can't silently turn
    /// the audition into a stress test.
    #[test]
    fn scenario_is_moderate_and_inside_the_window() {
        let cues = scenario();
        assert_eq!(cues, scenario(), "scenario must be a pure function");
        assert!(
            (80..=200).contains(&cues.len()),
            "moderate typing means ~5-8 cps over ~16 s, got {} cues",
            cues.len()
        );
        assert!(cues.iter().all(|c| c.0 >= 0.0 && c.0 < SECONDS - 3.5));
    }
}
