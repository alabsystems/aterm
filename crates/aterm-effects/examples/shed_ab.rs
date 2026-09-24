// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SHED A/B — the ear bench for the 2026-09-22 sound-seam fix.
//!
//! Owner, 2026-09-22: *"find the sound effects for typing code. is it
//! working? check"*. It was not, most of the time: when the adaptive
//! load-shed latch (or macOS Reduce Motion) folded the cursor trail's light
//! to zero, `CursorGlow::tick` took its zero-amplitude return, closed the key
//! seam, and every keystroke was refused before a cue existed. The latch
//! FLAPS under load, so the symptom was "the typing sounds come and go".
//! The fix split `GlowConfig::intensity` (the light) from
//! `GlowConfig::audible` (whose window the key landed in), and hoisted the
//! click's thermal constants above the dark return so a shed click is the
//! same instrument as a lit one.
//!
//! The tests pin the LAW. Only an ear settles whether the result SOUNDS
//! right — whether a shed click is the same bell as a lit one, and whether
//! the flapping run now plays as one unbroken phrase. This bench renders the
//! same typed sentence through the REAL engine (`CursorGlow` minting the
//! key-time cue exactly as the host's key path does: the cue reads the seam
//! the LAST frame left, then the echo moves the caret) and the REAL synth
//! (`TrailSynth::push_meta` with the glyph's own class and rank, because in
//! the music box the key IS the note), under five lighting plans:
//!
//! - `a-lit`            — the trail lit throughout: the reference.
//! - `b-shed-fixed`     — the light shed throughout, AFTER the fix.
//! - `c-shed-before`    — the light shed throughout, BEFORE the fix. The
//!   old dark return closed the seam unconditionally, which is exactly what
//!   `audible = false` does today, so this arm is the old law, not a guess.
//! - `d-flap-before`    — lit and shed alternating every 0.5 s, the owner's
//!   measured symptom, BEFORE the fix.
//! - `e-flap-fixed`     — the same flapping, AFTER the fix.
//!
//! Beside each WAV it prints, per arm: keys pressed, cues the engine minted,
//! keys HEARD (a real onset in the rendered audio, not a cue count), the
//! level, and the mean click HEAT on lit and on shed keys — decision F's
//! quantity: if the hoist were missing, shed keys would read a stale or
//! `Default` heat and sound duller than lit ones. `keys.csv` carries the same
//! per key, for an ear to be checked against.
//!
//!   targo --unverified run -p aterm-effects --example shed_ab -- <out_dir>

use aterm_core::render::GlowQuad;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::tone::Tone;
use aterm_effects::trail_sound::{
    CHANNELS, EventMeta, SoundEvent, SoundGesture, SoundKind, SoundVoice, TrailSynth,
    typed_glyph_class, typed_glyph_rank,
};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;
const SRF: f32 = SR as f32;
/// The host's audio block (`aterm-gui`'s `trail_audio::BUFFER_FRAMES`).
const BLOCK: usize = 512;
/// The frame clock the engine is ticked on.
const FRAME_S: f32 = 1.0 / 60.0;
const ROWS: usize = 24;
const COLS: usize = 80;
/// The caret's row, and the prompt it starts after (`$ `).
const ROW: u16 = 2;
const PROMPT_COLS: u16 = 2;
/// The shipped `trail_sound_volume` default.
const VOLUME: f32 = 0.4;
/// Half-period of the flapping shed latch: lit for 0.5 s, shed for 0.5 s.
const FLAP_S: f32 = 0.5;
/// A heard key: the 30 ms after it rises this far over the 30 ms before it…
const ONSET_RISE_DB: f32 = 6.0;
/// …and peaks above this.
const ONSET_FLOOR_DBFS: f32 = -45.0;
const TEXT: &str = "the quick brown fox jumps over the lazy dog while the build runs";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Light {
    Lit,
    /// Shed, after the fix: the light is folded to zero, the window is
    /// focused, so the key still belongs here.
    ShedFixed,
    /// Shed, before the fix: the dark return closed the seam
    /// unconditionally — exactly what `audible = false` does now.
    ShedBefore,
}

impl Light {
    fn is_shed(self) -> bool {
        self != Self::Lit
    }
}

#[derive(Clone, Copy)]
enum Plan {
    Always(Light),
    /// Lit for `FLAP_S`, then the given shed light for `FLAP_S`, repeating.
    Flap(Light),
}

impl Plan {
    fn at(self, t: f32) -> Light {
        match self {
            Self::Always(l) => l,
            Self::Flap(shed) => {
                if ((t / FLAP_S) as u32) % 2 == 1 {
                    shed
                } else {
                    Light::Lit
                }
            }
        }
    }
}

fn geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: ROWS,
        cols: COLS,
        origin_x: 0,
        origin_y: 0,
        win_w: (COLS * 8) as u16,
        win_h: (ROWS * 16) as u16,
        head: 0,
    }
}

/// The default look (`cursor_trail_style = "rainbow kitty pet"`), lit.
fn base() -> GlowConfig {
    GlowConfig {
        classic_mono: false,
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: true,
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
        style: GlowStyle::RainbowKitty,
        color: 0x0050_FA7B,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        audible: true,
        radius: 0.6,
        ring: true,
        beam: false,
        head_dx: 0.5,
        pack: None,
    }
}

fn cfg_for(light: Light, base: &GlowConfig) -> GlowConfig {
    match light {
        Light::Lit => GlowConfig { ..*base },
        Light::ShedFixed => GlowConfig {
            intensity: 0.0,
            audible: true,
            ..*base
        },
        Light::ShedBefore => GlowConfig {
            intensity: 0.0,
            audible: false,
            ..*base
        },
    }
}

struct Key {
    t: f32,
    ch: char,
}

/// The sentence, typed by one deterministic hand: ~115 ms a key, ±25 ms of
/// jitter, a little longer after a space. Byte-identical on every run.
fn keys() -> Vec<Key> {
    let mut rng = 0x2545_F491u32;
    let mut t = 0.30f32;
    TEXT.chars()
        .map(|ch| {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            let jitter = (f64::from(rng) / f64::from(u32::MAX) - 0.5) as f32 * 0.05;
            let k = Key { t, ch };
            t += 0.115 + jitter + if ch == ' ' { 0.04 } else { 0.0 };
            k
        })
        .collect()
}

/// One key the engine turned into sound.
struct Minted {
    t: f32,
    ch: char,
    kind: SoundKind,
    col: u16,
    heat: f32,
    hue: f32,
}

/// What happened to every key, minted or refused.
struct Pressed {
    t: f32,
    ch: char,
    light: Light,
    minted: Option<f32>,
}

/// Drive the REAL engine through the take: ticks on the frame clock, and each
/// key cued between frames against the seam the last tick left — the host's
/// order — then the echo moves the caret for the next tick.
fn perform(plan: Plan, keys: &[Key]) -> (Vec<Minted>, Vec<Pressed>) {
    let base = base();
    let g = geom();
    let mut glow = CursorGlow::default();
    let mut out: Vec<GlowQuad> = Vec::new();
    let t0 = Instant::now();
    let at = |s: f32| t0 + Duration::from_secs_f32(s);
    let end = keys.last().map_or(1.0, |k| k.t) + 1.5;
    let mut col = PROMPT_COLS;
    let mut minted = Vec::new();
    let mut pressed = Vec::new();
    let mut next = 0;
    let mut f = 0.0f32;
    while f <= end {
        while next < keys.len() && keys[next].t <= f {
            let k = &keys[next];
            let kind = if k.ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            let got = if glow.cue_keystroke_shifted(at(k.t), kind, false) {
                glow.take_key_cue()
            } else {
                None
            };
            if let Some(cue) = got {
                minted.push(Minted {
                    t: k.t,
                    ch: k.ch,
                    kind: cue.kind,
                    col: cue.col,
                    heat: cue.heat,
                    hue: cue.hue,
                });
            }
            pressed.push(Pressed {
                t: k.t,
                ch: k.ch,
                light: plan.at(k.t),
                minted: got.map(|c| c.heat),
            });
            col = (col + 1).min(COLS as u16 - 1);
            next += 1;
        }
        let c = cfg_for(plan.at(f), &base);
        out.clear();
        glow.tick(Some((ROW, col)), at(f), &c, g, &mut out);
        f += FRAME_S;
    }
    (minted, pressed)
}

/// The host's event for a key-time cue, with the glyph's side-car.
fn event(m: &Minted) -> (SoundEvent, EventMeta) {
    let pan = (f32::from(m.col) / (COLS as f32 - 1.0) * 2.0 - 1.0).clamp(-1.0, 1.0);
    (
        SoundEvent {
            style: GlowStyle::RainbowKitty,
            voice: SoundVoice::Style,
            kind: SoundGesture::Trail(m.kind),
            pan,
            heat: m.heat,
            hue: m.hue,
            gain: VOLUME,
            tone: Tone::Technical,
            bed: false,
            shifted: false,
        },
        EventMeta {
            at_ms: ((m.t * 1000.0) as u32).max(1),
            glyph_class: typed_glyph_class(Some(m.ch)),
            rank: typed_glyph_rank(Some(m.ch)),
            pan_from: 0.0,
            block_lead_s: 0.0,
            flow: 0.0,
        },
    )
}

/// Render the minted cues through the real synth in host-sized blocks, each
/// pushed at its own sample, folded to mono.
fn render(minted: &[Minted], secs: f32) -> Vec<f32> {
    let mut synth = TrailSynth::new(SRF, 0x5EED_5EED);
    let total = (SRF * secs) as usize;
    let mut buf = vec![0.0f32; total * CHANNELS];
    let mut i = 0usize;
    let mut next = 0usize;
    while i < total {
        let t = i as f32 / SRF;
        while next < minted.len() && minted[next].t <= t {
            let (ev, meta) = event(&minted[next]);
            synth.push_meta(ev, meta);
            next += 1;
        }
        let until = minted
            .get(next)
            .map_or(total, |m| ((m.t * SRF) as usize).max(i + 1));
        let n = BLOCK.min(total - i).min(until - i).max(1);
        synth.render(&mut buf[i * CHANNELS..(i + n) * CHANNELS]);
        i += n;
    }
    buf.as_chunks::<CHANNELS>()
        .0
        .iter()
        .map(|c| c.iter().sum::<f32>() / CHANNELS as f32)
        .collect()
}

fn db(x: f32) -> f32 {
    if x <= 1e-9 { -120.0 } else { 20.0 * x.log10() }
}

fn window_peak(mono: &[f32], a: f32, b: f32) -> f32 {
    let i = (a.max(0.0) * SRF) as usize;
    let j = ((b.max(0.0) * SRF) as usize).min(mono.len());
    if i >= j {
        return 0.0;
    }
    mono[i..j].iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

/// The rise, in dB, of the 30 ms after a key over the 30 ms before it — an
/// onset in the AUDIO, so a key whose predecessor is still ringing is not
/// counted as heard just because the room is not silent.
fn onset(mono: &[f32], t: f32) -> (f32, f32) {
    let after = window_peak(mono, t, t + 0.030);
    let before = window_peak(mono, t - 0.030, t);
    (db(after) - db(before), db(after))
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

fn mean(xs: impl Iterator<Item = f32>) -> Option<f32> {
    let (s, n) = xs.fold((0.0f32, 0u32), |(s, n), x| (s + x, n + 1));
    (n > 0).then(|| s / n as f32)
}

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "target/shed_ab".into()),
    );
    std::fs::create_dir_all(&dir).expect("create the output directory");
    let keys = keys();
    let secs = keys.last().map_or(1.0, |k| k.t) + 1.5;
    let arms = [
        ("a-lit", Plan::Always(Light::Lit)),
        ("b-shed-fixed", Plan::Always(Light::ShedFixed)),
        ("c-shed-before", Plan::Always(Light::ShedBefore)),
        ("d-flap-before", Plan::Flap(Light::ShedBefore)),
        ("e-flap-fixed", Plan::Flap(Light::ShedFixed)),
    ];
    let mut csv =
        String::from("arm,i,t_s,char,light,minted,heat,onset_rise_db,onset_peak_dbfs,heard\n");
    println!(
        "{:<14} {:>5} {:>6} {:>6} {:>10} {:>9} {:>9} {:>10}",
        "arm", "keys", "minted", "heard", "peak_dBFS", "rms_dBFS", "heat_lit", "heat_shed"
    );
    for (name, plan) in arms {
        let (minted, pressed) = perform(plan, &keys);
        let mono = render(&minted, secs);
        std::fs::write(dir.join(format!("{name}.wav")), wav_bytes(&mono)).expect("write wav");
        let mut heard = 0;
        for (i, p) in pressed.iter().enumerate() {
            let (rise, peak) = onset(&mono, p.t);
            let h = rise >= ONSET_RISE_DB && peak >= ONSET_FLOOR_DBFS;
            heard += usize::from(h);
            let _ = writeln!(
                csv,
                "{name},{i},{:.3},{},{:?},{},{},{rise:.1},{peak:.1},{}",
                p.t,
                if p.ch == ' ' {
                    "SPACE".to_string()
                } else {
                    p.ch.to_string()
                },
                p.light,
                u8::from(p.minted.is_some()),
                p.minted.map_or(String::new(), |h| format!("{h:.3}")),
                u8::from(h),
            );
        }
        let peak = mono.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let rms = (mono.iter().map(|x| x * x).sum::<f32>() / mono.len().max(1) as f32).sqrt();
        let heat_of = |shed: bool| {
            mean(
                pressed
                    .iter()
                    .filter(|p| p.light.is_shed() == shed)
                    .filter_map(|p| p.minted),
            )
            .map_or("-".to_string(), |h| format!("{h:.3}"))
        };
        println!(
            "{name:<14} {:>5} {:>6} {:>6} {:>10.1} {:>9.1} {:>9} {:>10}",
            pressed.len(),
            minted.len(),
            heard,
            db(peak),
            db(rms),
            heat_of(false),
            heat_of(true),
        );
    }
    std::fs::write(dir.join("keys.csv"), csv).expect("write keys.csv");
    println!("wrote {}", dir.display());
}
