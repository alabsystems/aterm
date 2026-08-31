// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! KEYBOARD-SONG A/B — the bench for the THREE THINGS the owner asked for
//! (2026-08-26/28): a delete that POOFS, a spacebar that is a low musical
//! downbeat, and typing that reads as "a little fun song" for ten minutes
//! rather than twenty seconds.
//!
//! It exists because `typing_voice_ab` CANNOT audit that request: its shared
//! `typing_script` contains ZERO `SoundKind::Space` cues (only Typed / Jump /
//! Backspace), so the one gesture the owner named is invisible to it. This
//! bench types REAL PROSE — every space is a [`SoundKind::Space`], every
//! newline a [`SoundKind::Jump`] — at realistic rates, with the edit and
//! whitespace corner cases spelled out.
//!
//!   cargo run --release -p aterm-effects --example keyboard_song_ab \
//!       -- <out_dir> [--tag <name>] [--voice <name>] [--style <name>]
//!
//! Renders each scenario at BOTH the host default volume (0.4) and 1.0, with
//! NO independent normalisation — the two sides of an A/B are only comparable
//! if the gain staging is identical, so the writer is a straight float dump of
//! whatever the synth produced. The `--tag` names the render (`current` vs
//! `proposed`); filenames are `<tag>-<scenario>-v<volume>.wav`.
//!
//! Reported per scenario: PITCHED onsets/second (a tonality test, so a noise
//! poof is not counted as a note), pre-clip peak (the soft clipper is
//! inverted, so the number is the real headroom the mix asked for), post-clip
//! peak, RMS, crest, max live voices, voice steals, and the render time as a
//! realtime factor. Plus a GESTURE probe table (Typed / Space / Backspace /
//! Shift / Land in isolation from one settled melody state) carrying each
//! gesture's spectral centroid, high-band fraction, tonality and peak — which
//! is where "the erase is airy and above the keystroke" is a MEASUREMENT.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::tone::Tone;
use aterm_effects::trail_sound::{
    CHANNELS, SoundEvent, SoundGesture, SoundKind, SoundVoice, TrailSynth,
};

const SR: u32 = 48_000;
const SEED: u32 = 0x50_4F_4F_46; // "POOF"
/// The host's audio device block (`aterm-gui`'s `trail_audio::BUFFER_FRAMES`),
/// so the render's cue quantisation matches the shipping one.
const BLOCK: usize = 512;

/// One scripted cue: (time s, gesture, pan, heat).
type Cue = (f32, SoundGesture, f32, f32);

// ---------------------------------------------------------------------------
// The scripts — real text, real spaces
// ---------------------------------------------------------------------------

/// The prose corpus. Ordinary English at ordinary word lengths, so the SPACE
/// cadence is the real one (~2 per second at 10 cps) rather than a synthetic
/// grid. Paragraph breaks are blank lines; every `\n` is an Enter.
const PROSE: &str = "\
the renderer keeps one atlas per face and never uploads a glyph twice.\n\
when the shaper hands back a run we look up each cluster, and only the\n\
misses cost anything at all.\n\
\n\
a cache that is wrong is worse than no cache, so the key carries the\n\
face id, the pixel size and the synthesis flags. two faces that differ\n\
only in weight can never collide.\n\
\n\
the slow path is deliberate. it runs once per new glyph and then never\n\
again for the life of the window, which is the whole point of paying\n\
for it up front.\n\
";

/// Type `text` from `t0` at `cps`, cueing a real gesture per character: a
/// space is a [`SoundKind::Space`], a newline an Enter ([`SoundKind::Jump`]),
/// everything else a [`SoundKind::Typed`]. Sentence ends and blank lines rest
/// like a person does. Returns the time the run ended.
fn type_text(cues: &mut Vec<Cue>, t0: f32, cps: f32, text: &str, heat: f32) -> f32 {
    let mut t = t0;
    let dt = 1.0 / cps;
    let mut col = 0.0f32;
    for ch in text.chars() {
        // The pan the host would send: the caret's column across the pane.
        let pan = (col / 68.0).clamp(0.0, 1.0) * 1.8 - 0.9;
        let kind = match ch {
            ' ' => SoundKind::Space,
            '\n' => SoundKind::Jump,
            _ => SoundKind::Typed,
        };
        cues.push((t, SoundGesture::Trail(kind), pan, heat));
        t += dt;
        if ch == '\n' {
            col = 0.0;
            // A line ending is a beat of thought.
            t += 0.35;
        } else {
            col += 1.0;
        }
        if ch == '.' {
            t += 0.55; // sentence rest
        }
    }
    t
}

/// 60 s of realistic prose at 10 cps with the pauses a typist actually takes,
/// looping the corpus until the window is full.
fn scenario_prose() -> Scenario {
    let mut cues = Vec::new();
    let mut t = 0.5f32;
    while t < 60.0 {
        t = type_text(&mut cues, t, 10.0, PROSE, 0.55);
        t += 1.4; // between paragraphs, a longer think
    }
    cues.retain(|c| c.0 < 60.0);
    Scenario {
        name: "prose".into(),
        cues,
        seconds: 61.5,
        window: (0.5, 60.0),
    }
}

/// THE EDIT SCENARIO — the deletion cases the owner named, in one pass:
/// `type 12 → delete 12` three times, then twenty alternating type/delete
/// pairs, then a HELD backspace (30 Hz auto-repeat, the rate the min-gap
/// governor actually has to survive).
fn scenario_edit() -> Scenario {
    let mut cues = Vec::new();
    let mut t = 0.5f32;
    let push = |cues: &mut Vec<Cue>, t: f32, k: SoundKind, pan: f32| {
        cues.push((t, SoundGesture::Trail(k), pan, 0.5));
    };
    for _ in 0..3 {
        for i in 0..12 {
            push(&mut cues, t, SoundKind::Typed, -0.5 + i as f32 * 0.08);
            t += 0.1;
        }
        t += 0.3;
        for i in 0..12 {
            push(&mut cues, t, SoundKind::Backspace, 0.46 - i as f32 * 0.08);
            t += 0.1;
        }
        t += 0.7;
    }
    t += 0.6;
    // Alternating: the case where a poof must not be swallowed by the
    // keystroke inside the global min-gap.
    for i in 0..20 {
        push(&mut cues, t, SoundKind::Typed, 0.0);
        t += 0.09;
        push(&mut cues, t, SoundKind::Backspace, 0.0);
        t += 0.09;
        let _ = i;
    }
    t += 0.9;
    // HELD backspace: 30 per second for a second.
    for _ in 0..30 {
        push(&mut cues, t, SoundKind::Backspace, 0.2);
        t += 1.0 / 30.0;
    }
    Scenario {
        name: "edit".into(),
        cues,
        seconds: t + 2.0,
        window: (0.4, t + 1.0),
    }
}

/// THE WHITESPACE SCENARIO — ordinary word spaces, then the pathological
/// cases: single-letter words (`a a a a`), four-space indentation, and an
/// eight-space run. The coalescing law is audible here or nowhere.
fn scenario_space() -> Scenario {
    let mut cues = Vec::new();
    let mut t = 0.5f32;
    t = type_text(
        &mut cues,
        t,
        9.0,
        "the quick brown fox jumps over it\n",
        0.5,
    );
    t += 1.0;
    t = type_text(&mut cues, t, 9.0, "a a a a a a a a\n", 0.5);
    t += 1.0;
    t = type_text(&mut cues, t, 9.0, "    indented    twice\n", 0.5);
    t += 1.0;
    t = type_text(&mut cues, t, 9.0, "gap        here\n", 0.5);
    t += 0.8;
    Scenario {
        name: "space".into(),
        cues,
        seconds: t + 1.5,
        window: (0.4, t + 0.8),
    }
}

/// A 20 cps BURST — twice the sustained rate the governor is tuned for, six
/// seconds of it, with spaces at real word cadence.
fn scenario_burst() -> Scenario {
    let mut cues = Vec::new();
    let mut t = 0.5f32;
    let mut i = 0usize;
    while t < 6.5 {
        // ~5.5 characters per word, so a space lands where one would.
        let kind = if i % 6 == 5 {
            SoundKind::Space
        } else {
            SoundKind::Typed
        };
        cues.push((
            t,
            SoundGesture::Trail(kind),
            ((i % 60) as f32 / 30.0) - 1.0,
            0.9,
        ));
        t += 0.05;
        i += 1;
    }
    Scenario {
        name: "burst".into(),
        cues,
        seconds: 8.0,
        window: (0.4, 7.0),
    }
}

struct Scenario {
    name: String,
    cues: Vec<Cue>,
    seconds: f32,
    /// The measurement window (s).
    window: (f32, f32),
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

struct Rendered {
    mono: Vec<f32>,
    max_voices: usize,
    steals: u32,
    render_s: f64,
}

fn render(sc: &Scenario, voice: SoundVoice, style: GlowStyle, volume: f32) -> Rendered {
    let frames = (sc.seconds * SR as f32) as usize;
    let mut synth = TrailSynth::new(SR as f32, SEED);
    let mut mono = vec![0.0f32; frames];
    let mut stereo = vec![0.0f32; BLOCK * CHANNELS];
    let mut cue_i = 0usize;
    let mut f = 0usize;
    let mut max_voices = 0usize;
    let t0 = Instant::now();
    while f < frames {
        let n = BLOCK.min(frames - f);
        let t = f as f32 / SR as f32;
        while cue_i < sc.cues.len() && sc.cues[cue_i].0 <= t {
            let (ct, kind, pan, heat) = sc.cues[cue_i];
            synth.push(SoundEvent {
                style,
                voice,
                kind,
                pan,
                heat,
                hue: (ct * 0.18).fract(),
                gain: volume,
                tone: Tone::Technical,
                bed: false, // the KEYSTROKE is what is on trial
            });
            cue_i += 1;
        }
        synth.render(&mut stereo[..n * CHANNELS]);
        max_voices = max_voices.max(synth.live_voices());
        for i in 0..n {
            mono[f + i] = 0.5 * (stereo[i * 2] + stereo[i * 2 + 1]);
        }
        f += n;
    }
    Rendered {
        mono,
        max_voices,
        steals: synth.steals(),
        render_s: t0.elapsed().as_secs_f64(),
    }
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

/// Invert the synth's output saturator, so the reported PEAK is the level the
/// mix actually asked for rather than what the clipper let through.
/// `soft_clip(x) = x·(27+x²)/(27+9x²)`, clamped to ±0.98 — strictly increasing
/// where it is not clamped, so three Newton steps from `y` converge.
fn pre_clip(y: f32) -> f32 {
    let s = y.signum();
    let y = y.abs();
    if y >= 0.979_9 {
        return f32::INFINITY; // clamped: the pre-clip level is unknowable
    }
    let mut x = y;
    for _ in 0..40 {
        let d = 27.0 + 9.0 * x * x;
        let fx = x * (27.0 + x * x) / d - y;
        // d/dx of the rational saturator.
        let num = (27.0 + 3.0 * x * x) * d - x * (27.0 + x * x) * 18.0 * x;
        let dfx = num / (d * d);
        if dfx.abs() < 1e-9 {
            break;
        }
        let step = fx / dfx;
        x -= step;
        if step.abs() < 1e-9 {
            break;
        }
    }
    s * x
}

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-9).log10()
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

// -- FFT (radix-2 DIT, hand-rolled — no dependency) --------------------------

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

fn mag_at(x: &[f32], start: usize) -> Vec<f32> {
    let mut re = vec![0.0f32; FFT_N];
    let mut im = vec![0.0f32; FFT_N];
    for (i, r) in re.iter_mut().enumerate() {
        let w = 0.5 * (1.0 - (core::f32::consts::TAU * i as f32 / FFT_N as f32).cos());
        *r = x.get(start + i).copied().unwrap_or(0.0) * w;
    }
    fft(&mut re, &mut im);
    (0..FFT_N / 2)
        .map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt())
        .collect()
}

/// TONALITY of one window: the loudest bin in 150..4500 Hz over the MEDIAN bin
/// in the same band. A struck tone is a spike over a quiet floor (ratio in the
/// tens); a band-passed noise burst is a broad hump (ratio near unity). The
/// threshold below is what separates "a note" from "a poof".
fn tonality(mag: &[f32]) -> f64 {
    let hz_per_bin = f64::from(SR) / FFT_N as f64;
    let k0 = (150.0 / hz_per_bin) as usize;
    let k1 = ((4500.0 / hz_per_bin) as usize).min(mag.len() - 1);
    if k1 <= k0 + 8 {
        return 0.0;
    }
    let mut band: Vec<f64> = mag[k0..=k1].iter().map(|&m| f64::from(m)).collect();
    let peak = band.iter().cloned().fold(0.0f64, f64::max);
    band.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = band[band.len() / 2].max(1e-12);
    peak / med
}

/// A window is a PITCHED note when its spectrum spikes this far over its own
/// band median. Fitted between the two populations, not guessed: a rendered
/// glass-bell keystroke reads in the hundreds, the erase poof in the low tens.
const PITCHED_TONALITY: f64 = 60.0;

/// Onset frames. RISE detection, not threshold crossing: at 10 cps the notes
/// are 100 ms apart under 100-300 ms tails, so the envelope NEVER returns to a
/// floor between them and a re-arming threshold counts one note per burst
/// (measured: 1.2 onsets/s on a 10 cps script). Only the derivative separates
/// them. A 2.7 ms RMS window smooths the pulse/sub-octave beat; the 35 ms
/// refractory is under the 50 ms spacing of the fastest script here.
fn onsets(x: &[f32], from: usize, to: usize) -> Vec<usize> {
    const W: usize = 128;
    const REFRACTORY: usize = SR as usize * 35 / 1000;
    let env: Vec<f32> = (from..to)
        .step_by(W)
        .map(|s| {
            let c = &x[s..(s + W).min(to)];
            (c.iter().map(|v| v * v).sum::<f32>() / c.len().max(1) as f32).sqrt()
        })
        .collect();
    let peak = env.iter().fold(0.0f32, |m, &v| m.max(v));
    let floor = peak * 0.06;
    let mut out: Vec<usize> = Vec::new();
    for i in 0..env.len() {
        if env[i] <= floor {
            continue;
        }
        // The first audible window is an onset by definition (its attack is
        // shorter than one window); after that a note announces itself by a
        // rise over the decay it lands on.
        if !(i == 0 || env[i] > env[i - 1] * 1.35) {
            continue;
        }
        let at = from + i * W;
        if out.last().is_none_or(|&p| at >= p + REFRACTORY) {
            out.push(at);
        }
    }
    out
}

/// The loudest partial (Hz) in `lo..hi`, parabolically interpolated.
fn peak_hz(mag: &[f32], lo: f32, hi: f32) -> f64 {
    let hz_per_bin = f64::from(SR) / FFT_N as f64;
    let k0 = ((f64::from(lo) / hz_per_bin) as usize).max(1);
    let k1 = ((f64::from(hi) / hz_per_bin) as usize).min(mag.len() - 2);
    if k1 <= k0 {
        return 0.0;
    }
    let mut best = k0;
    for k in k0..=k1 {
        if mag[k] > mag[best] {
            best = k;
        }
    }
    let (a, b, c) = (
        f64::from(mag[best - 1]),
        f64::from(mag[best]),
        f64::from(mag[best + 1]),
    );
    let den = a - 2.0 * b + c;
    let d = if den.abs() < 1e-12 {
        0.0
    } else {
        0.5 * (a - c) / den
    };
    (best as f64 + d) * hz_per_bin
}

/// Spectral centroid + fraction of energy over 2 kHz, over `from..to`.
fn spectrum(x: &[f32], from: usize, to: usize) -> (f64, f64) {
    let hz_per_bin = f64::from(SR) / FFT_N as f64;
    let (mut num, mut den, mut hi) = (0.0f64, 0.0f64, 0.0f64);
    let mut s = from;
    while s + FFT_N <= to {
        for (k, &m) in mag_at(x, s).iter().enumerate() {
            let e = f64::from(m) * f64::from(m);
            num += e * k as f64 * hz_per_bin;
            den += e;
            if k as f64 * hz_per_bin > 2000.0 {
                hi += e;
            }
        }
        s += FFT_N / 2;
    }
    if den < 1e-15 {
        (0.0, 0.0)
    } else {
        (num / den, hi / den)
    }
}

struct Row {
    name: String,
    volume: f32,
    onsets_hz: f64,
    pitched_hz: f64,
    /// PITCHED onsets per second whose note DIFFERS from the previous
    /// pitched onset's by more than a semitone — i.e. how often the melody
    /// actually MOVES. `pitched_hz` counts accompaniment too; this is the
    /// number a listener hears as "the tune".
    melody_hz: f64,
    /// …and the complement: the share of pitched onsets that repeat the
    /// previous note (accompaniment, by design — not the drone the melody
    /// generator was rewritten to remove).
    repeat_pct: f64,
    pre_peak_db: f64,
    peak_db: f64,
    rms_db: f64,
    crest_db: f64,
    centroid_hz: f64,
    max_voices: usize,
    steals: u32,
    rt_factor: f64,
}

fn analyze(name: &str, volume: f32, r: &Rendered, window: (f32, f32)) -> Row {
    let from = (window.0 * SR as f32) as usize;
    let to = ((window.1 * SR as f32) as usize).min(r.mono.len());
    let seg = &r.mono[from..to];
    let ons = onsets(&r.mono, from, to);
    // Pitch each onset once: the tonality test says whether it is a note at
    // all, the peak partial says WHICH note. `SETTLE` past the onset, so the
    // family's ~12 ms contour bend has landed and the pitch read is the note
    // the gesture lands ON rather than the one it scoops through.
    const SETTLE: usize = SR as usize / 40;
    let notes: Vec<f64> = ons
        .iter()
        .filter(|&&s| s + SETTLE + FFT_N < to)
        .filter_map(|&s| {
            let m = mag_at(&r.mono, s + SETTLE);
            (tonality(&m) >= PITCHED_TONALITY).then(|| peak_hz(&m, 120.0, 6000.0))
        })
        .collect();
    let pitched = notes.len();
    let moves = notes
        .windows(2)
        .filter(|w| w[0] > 0.0 && (12.0 * (w[1] / w[0]).log2()).abs() > 1.0)
        .count();
    let span = f64::from(window.1 - window.0);
    let peak = seg.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let pre = seg.iter().fold(0.0f32, |m, &v| m.max(pre_clip(v).abs()));
    let rms_v = rms(seg);
    let (centroid, _) = spectrum(&r.mono, from, to);
    let audio_s = f64::from(r.mono.len() as f32 / SR as f32);
    Row {
        name: name.into(),
        volume,
        onsets_hz: ons.len() as f64 / span,
        pitched_hz: pitched as f64 / span,
        melody_hz: moves as f64 / span,
        repeat_pct: if pitched < 2 {
            0.0
        } else {
            (1.0 - moves as f64 / (pitched - 1) as f64) * 100.0
        },
        pre_peak_db: db(f64::from(pre)),
        peak_db: db(f64::from(peak)),
        rms_db: db(rms_v),
        crest_db: db(f64::from(peak)) - db(rms_v),
        centroid_hz: centroid,
        max_voices: r.max_voices,
        steals: r.steals,
        rt_factor: audio_s / r.render_s.max(1e-9),
    }
}

// ---------------------------------------------------------------------------
// The gesture probe — one gesture alone, from one settled melody state
// ---------------------------------------------------------------------------

struct ProbeRow {
    name: String,
    peak_db: f64,
    rms_db: f64,
    centroid_hz: f64,
    hi_frac: f64,
    tonality: f64,
    voices: usize,
}

fn probe(
    kind: SoundKind,
    voice: SoundVoice,
    style: GlowStyle,
    volume: f32,
) -> (ProbeRow, Vec<f32>) {
    let mut synth = TrailSynth::new(SR as f32, SEED);
    let ev = |kind| SoundEvent {
        style,
        voice,
        kind: SoundGesture::Trail(kind),
        pan: 0.0,
        heat: 0.5,
        hue: 0.0,
        gain: volume,
        tone: Tone::Technical,
        bed: false,
    };
    // THREE settling keystrokes, ~340 ms apart: enough that the previous
    // note's tail is dead, and — since the bar's accents fall every third
    // keystroke — enough that the PROBE itself lands on an accent. A gesture
    // measured on a ghost slot would be reported against the accompaniment
    // rather than against the tune.
    let mut warm = vec![0.0f32; 16_384 * CHANNELS];
    for _ in 0..3 {
        synth.push(ev(SoundKind::Typed));
        synth.render(&mut warm);
    }
    synth.push(ev(kind));
    let voices = synth.live_voices();
    let frames = SR as usize / 2;
    let mut stereo = vec![0.0f32; frames * CHANNELS];
    synth.render(&mut stereo);
    let mono: Vec<f32> = (0..frames)
        .map(|i| 0.5 * (stereo[i * 2] + stereo[i * 2 + 1]))
        .collect();
    let peak = mono.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    // Score the gesture's own body: onset to 250 ms past it.
    let start = mono.iter().position(|v| v.abs() > peak * 0.05).unwrap_or(0);
    let end = (start + SR as usize / 4).min(mono.len());
    let (centroid, hi) = spectrum(&mono, start, end);
    let row = ProbeRow {
        name: format!("{kind:?}"),
        peak_db: db(f64::from(peak)),
        rms_db: db(rms(&mono[start..end])),
        centroid_hz: centroid,
        hi_frac: hi,
        tonality: tonality(&mag_at(&mono, start)),
        voices,
    };
    (row, mono)
}

/// The gesture probes, back to back with 400 ms of air between them — the
/// file to open FIRST, because it is where a keystroke, a downbeat and an
/// erase can be heard against each other rather than one at a time.
fn probe_reel(voice: SoundVoice, style: GlowStyle, volume: f32) -> (Vec<ProbeRow>, Vec<f32>) {
    let gap = vec![0.0f32; SR as usize * 2 / 5];
    let mut reel = Vec::new();
    let mut rows = Vec::new();
    for kind in PROBE_KINDS {
        let (row, mono) = probe(kind, voice, style, volume);
        reel.extend_from_slice(&mono);
        reel.extend_from_slice(&gap);
        rows.push(row);
    }
    (rows, reel)
}

/// The gestures the probe table and the reel cover, in reel order.
const PROBE_KINDS: [SoundKind; 8] = [
    SoundKind::Typed,
    SoundKind::Space,
    SoundKind::Backspace,
    SoundKind::KillWord,
    SoundKind::Shift,
    SoundKind::Kill,
    SoundKind::Land,
    SoundKind::Jump,
];

// ---------------------------------------------------------------------------
// WAV
// ---------------------------------------------------------------------------

/// 32-bit float mono, written VERBATIM — no normalisation, no dither. An A/B
/// whose two sides are independently normalised is not an A/B.
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
    let mut out = PathBuf::from("target/keyboard-song-ab");
    let mut tag = "current".to_string();
    let mut voice = SoundVoice::Style;
    let mut style = GlowStyle::RainbowKitty;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--tag" => tag = args.next().unwrap_or_else(|| "current".into()),
            "--voice" => {
                let v = args.next().unwrap_or_default();
                voice = SoundVoice::parse(&v).unwrap_or_else(|| {
                    eprintln!("unknown voice {v:?}");
                    std::process::exit(2);
                });
            }
            "--style" => {
                let s = args.next().unwrap_or_default();
                style = match s.as_str() {
                    "lumen" => GlowStyle::Lumen,
                    "sparkle" => GlowStyle::Sparkle,
                    "fire" => GlowStyle::Fire,
                    "water" => GlowStyle::Water,
                    "comet" => GlowStyle::Comet,
                    "laser" => GlowStyle::Laser,
                    "beam" => GlowStyle::Beam,
                    "phaser" => GlowStyle::Phaser,
                    _ => GlowStyle::RainbowKitty,
                };
            }
            other => out = PathBuf::from(other),
        }
    }
    std::fs::create_dir_all(&out).expect("out dir");

    let scenarios = [
        scenario_prose(),
        scenario_edit(),
        scenario_space(),
        scenario_burst(),
    ];
    let mut rows: Vec<Row> = Vec::new();
    for sc in &scenarios {
        for volume in [0.4f32, 1.0] {
            let r = render(sc, voice, style, volume);
            let path = out.join(format!("{tag}-{}-v{volume:.1}.wav", sc.name));
            let mut f = std::fs::File::create(&path).expect("wav");
            f.write_all(&wav_bytes(&r.mono)).expect("write");
            rows.push(analyze(&sc.name, volume, &r, sc.window));
            println!("wrote {}", path.display());
        }
    }

    println!("\n== {tag}: {} / {style:?} ==", voice.name());
    println!(
        "{:<8} {:>4} {:>7} {:>7} {:>7} {:>6} {:>9} {:>8} {:>8} {:>6} {:>8} {:>5} {:>5} {:>6}",
        "scene",
        "vol",
        "ons/s",
        "pitch/s",
        "melo/s",
        "rept%",
        "pre-pk dB",
        "peak dB",
        "rms dB",
        "crest",
        "centroid",
        "vmax",
        "steal",
        "xRT"
    );
    for r in &rows {
        println!(
            "{:<8} {:>4.1} {:>7.2} {:>7.2} {:>7.2} {:>6.0} {:>9.2} {:>8.2} {:>8.2} {:>6.1} {:>8.0} {:>5} {:>5} {:>6.0}",
            r.name,
            r.volume,
            r.onsets_hz,
            r.pitched_hz,
            r.melody_hz,
            r.repeat_pct,
            r.pre_peak_db,
            r.peak_db,
            r.rms_db,
            r.crest_db,
            r.centroid_hz,
            r.max_voices,
            r.steals,
            r.rt_factor
        );
    }

    for volume in [0.4f32, 1.0] {
        println!("\n== gesture probes (isolated, vol {volume}) ==");
        println!(
            "{:<12} {:>8} {:>8} {:>9} {:>8} {:>9} {:>6}",
            "gesture", "peak dB", "rms dB", "centroid", "hi>2k", "tonality", "voices"
        );
        let (probes, reel) = probe_reel(voice, style, volume);
        for p in &probes {
            println!(
                "{:<12} {:>8.2} {:>8.2} {:>9.0} {:>8.3} {:>9.1} {:>6}",
                p.name, p.peak_db, p.rms_db, p.centroid_hz, p.hi_frac, p.tonality, p.voices
            );
        }
        let path = out.join(format!("{tag}-gestures-v{volume:.1}.wav"));
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(&wav_bytes(&reel)))
            .expect("gesture reel");
        println!(
            "\nwrote {} — the reel, in order: {}",
            path.display(),
            PROBE_KINDS
                .iter()
                .map(|k| format!("{k:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}
