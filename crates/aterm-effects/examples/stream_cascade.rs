// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! STREAM-CASCADE — the audit fixture for the owner's 2026-09-12 report
//! ("while an LLM streams output I hear this looping sound 'doo doo doo doo'
//! up and down"): a deterministic stream of PTY line feeds at a fixed rate,
//! rendered through the shipping music box, with the cascade lane's own
//! census read off the engine rather than re-derived.
//!
//!   targo --unverified run --release -p aterm-effects --example stream_cascade \
//!       -- <out_dir> [--tag <name>] [--rates 5,12,25,60] [--seconds 6] [--text]
//!
//! WHAT THE HOST MINTS FOR A STREAMING LINE FEED (both `cursor_glow.rs`
//! seams, v0.76.0 and main, read side by side): a LICENSED, non-typing,
//! non-navigation cursor move — a row change, or a same-row hop of two or
//! more cells — reaches `cue_move_sound`'s `else` arm and cues
//! [`SoundKind::Jump`]; the synth routes that to `v2_cascade` (D18). An
//! UNLICENSED move (no key hint fresh within 0.25 s, fewer than two unpaid
//! presses in 2 s) mints nothing at all. This bench models the licensed
//! stream: one `Jump` per line feed, timestamped on the host input clock.
//!
//! `--text` adds the SLOW-STREAM echo shape between line feeds — a one-cell
//! advance per 60 Hz frame, which `classify_move` reads as `typing` and
//! `cue_move_sound` cues as an echo-born [`SoundKind::Typed`] (no key
//! credit, glyph class 0, rank 0, unshifted). A FAST stream (several cells
//! per frame) classifies every frame as a multi-cell hop, i.e. a `Jump` at
//! frame rate — which is the `--rates 60` row.
//!
//! Every WAV is stereo: LEFT is the synth's mono mix (no normalisation —
//! the two builds are only comparable if the gain staging is identical),
//! RIGHT is a 2 ms 2 kHz click at each line feed's own time at −20 dBFS.

use std::io::Write as _;
use std::path::PathBuf;

use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::tone::Tone;
use aterm_effects::trail_sound::{
    CHANNELS, EventMeta, SoundEvent, SoundGesture, SoundKind, SoundVoice, TrailSynth,
};

const SR: u32 = 48_000;
const SEED: u32 = 0x50_4F_4F_46; // "POOF" — the keyboard-song bench's seed
/// The host's audio device block (`aterm-gui`'s `trail_audio::BUFFER_FRAMES`).
const BLOCK: usize = 512;
/// The stream starts here, so the first cascade has silence in front of it.
const T0: f32 = 0.5;
/// Where a line feed lands: column 0 of the new row, i.e. hard left of the
/// host's pan map (`col / 68 → −0.9..0.9`).
const LF_PAN: f32 = -0.9;
/// A PTY move's `blaze()` sits high (a non-typing move slams the flare to
/// 1.0 and it decays); a constant keeps the two builds' brightness law out
/// of the comparison.
const HEAT: f32 = 0.6;
/// One 60 Hz frame — the cadence a slow stream's one-cell advances arrive at.
const FRAME_S: f32 = 1.0 / 60.0;

#[derive(Clone, Copy)]
struct Cue {
    t: f32,
    kind: SoundKind,
    pan: f32,
}

impl Cue {
    fn at_ms(self) -> u32 {
        ((self.t * 1000.0) as u32).max(1)
    }
}

/// `seconds` of line feeds at `rate` per second from [`T0`], plus (with
/// `text`) one echo-born Typed cue per frame between them.
fn scenario(rate: f32, seconds: f32, text: bool) -> Vec<Cue> {
    let mut cues = Vec::new();
    let period = 1.0 / rate;
    let n = (seconds * rate).round() as usize;
    for k in 0..n {
        let t_lf = T0 + k as f32 * period;
        if text {
            // The line's glyphs arrive one cell per frame after the line feed,
            // until the next line feed; the pan walks the columns.
            let mut col = 0usize;
            let mut t = t_lf + FRAME_S;
            while t < t_lf + period - 0.5 * FRAME_S {
                col += 1;
                cues.push(Cue {
                    t,
                    kind: SoundKind::Typed,
                    pan: (col as f32 / 68.0).clamp(0.0, 1.0) * 1.8 - 0.9,
                });
                t += FRAME_S;
            }
        }
        cues.push(Cue {
            t: t_lf,
            kind: SoundKind::Jump,
            pan: LF_PAN,
        });
    }
    cues.sort_by(|a, b| a.t.total_cmp(&b.t));
    cues
}

struct Rendered {
    mono: Vec<f32>,
    /// `(at_ms, f0_hz, head, admitted)` of every cascade voice asked for.
    log: Vec<(u32, f32, bool, bool)>,
    melody: [u32; 3],
    lane: [u32; 3],
    steals: u32,
    max_voices: usize,
}

fn render(cues: &[Cue], seconds: f32, volume: f32) -> Rendered {
    let frames = ((T0 + seconds + 1.0) * SR as f32) as usize;
    let mut synth = TrailSynth::new(SR as f32, SEED);
    let mut mono = vec![0.0f32; frames];
    let mut stereo = vec![0.0f32; BLOCK * CHANNELS];
    // J1: every onset exactly one block after its press — the shipping
    // behaviour (`block_lead_s` on the host's `push_meta`).
    let spawn: Vec<usize> = cues
        .iter()
        .map(|c| (f64::from(c.t) * f64::from(SR)) as usize + BLOCK)
        .collect();
    let mut log = Vec::new();
    let mut cue_i = 0usize;
    let mut f = 0usize;
    let mut max_voices = 0usize;
    while f < frames {
        while cue_i < cues.len() && spawn[cue_i] <= f {
            let cue = cues[cue_i];
            synth.push_meta(
                SoundEvent {
                    style: GlowStyle::RainbowKitty,
                    voice: SoundVoice::Style,
                    kind: SoundGesture::Trail(cue.kind),
                    pan: cue.pan,
                    heat: HEAT,
                    hue: (cue.t * 0.18).fract(),
                    gain: volume,
                    tone: Tone::Technical,
                    bed: false,
                    shifted: false,
                },
                EventMeta {
                    at_ms: cue.at_ms(),
                    ..Default::default()
                },
            );
            cue_i += 1;
        }
        let next_block = (f / BLOCK + 1) * BLOCK;
        let next_cue = spawn.get(cue_i).copied().unwrap_or(usize::MAX);
        let to = frames.min(next_block).min(next_cue);
        let n = to.saturating_sub(f).max(1).min(frames - f);
        synth.render(&mut stereo[..n * CHANNELS]);
        synth.drain_cascade_log(&mut log);
        max_voices = max_voices.max(synth.live_voices());
        for i in 0..n {
            mono[f + i] = 0.5 * (stereo[i * 2] + stereo[i * 2 + 1]);
        }
        f += n;
    }
    Rendered {
        mono,
        log,
        melody: synth.melody_v2().cascade_census(),
        lane: synth.lane_census(TrailSynth::cascade_lane()),
        steals: synth.steals(),
        max_voices,
    }
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-9).log10()
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

/// Onsets heard in the mix: a 6 ms peak follower that rises by more than
/// 4 dB over its value 6 ms earlier, above −48 dBFS, at most one per 20 ms.
fn onsets(mono: &[f32]) -> Vec<usize> {
    let rel = (-1.0 / (SR as f32 * 0.006)).exp();
    let lag = (SR as f32 * 0.006) as usize;
    let refractory = (SR as f32 * 0.020) as usize;
    let floor = 10f32.powf(-48.0 / 20.0);
    let mut env = vec![0.0f32; mono.len()];
    let mut e = 0.0f32;
    for (i, &x) in mono.iter().enumerate() {
        e = x.abs().max(e * rel);
        env[i] = e;
    }
    let mut out = Vec::new();
    let mut last = 0usize;
    let mut armed = true;
    for i in lag..mono.len() {
        let rise = env[i] > env[i - lag] * 1.585 && env[i] > floor;
        if rise && armed && i - last >= refractory {
            out.push(i);
            last = i;
            armed = false;
        } else if !rise {
            armed = true;
        }
    }
    out
}

/// The autocorrelation pitch of the 20 ms after an onset, 300–3000 Hz.
fn pitch_at(mono: &[f32], i: usize) -> f32 {
    let start = i + (SR as usize * 3 / 1000);
    let n = SR as usize * 20 / 1000;
    if start + n + SR as usize / 300 >= mono.len() {
        return 0.0;
    }
    let seg = &mono[start..start + n];
    let e0: f32 = seg.iter().map(|v| v * v).sum::<f32>().max(1e-12);
    let (lo, hi) = (SR as usize / 3000, SR as usize / 300);
    let mut best = (0usize, -1.0f32);
    for l in lo..=hi {
        let mut acc = 0.0f32;
        for k in 0..n {
            acc += seg[k] * mono[start + k + l];
        }
        let r = acc / e0;
        if r > best.1 {
            best = (l, r);
        }
    }
    if best.0 == 0 {
        0.0
    } else {
        SR as f32 / best.0 as f32
    }
}

/// The smallest repeat period of a pitch sequence (in notes), or 0.
fn period_of(seq: &[f32]) -> usize {
    if seq.len() < 4 {
        return 0;
    }
    'p: for p in 1..=seq.len() / 2 {
        for i in 0..seq.len() - p {
            if (seq[i] - seq[i + p]).abs() > 0.5 {
                continue 'p;
            }
        }
        return p;
    }
    0
}

fn direction_changes(seq: &[f32]) -> usize {
    let mut n = 0usize;
    let mut prev: Option<f32> = None;
    let mut last_dir = 0i8;
    for &f in seq {
        if let Some(p) = prev {
            let d = (f - p).signum() as i8;
            if d != 0 {
                if last_dir != 0 && d != last_dir {
                    n += 1;
                }
                last_dir = d;
            }
        }
        prev = Some(f);
    }
    n
}

fn wav_stereo(l: &[f32], r: &[f32]) -> Vec<u8> {
    let n = l.len().max(r.len());
    let data_len = (n * 2 * 4) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&SR.to_le_bytes());
    w.extend_from_slice(&(SR * 8).to_le_bytes());
    w.extend_from_slice(&8u16.to_le_bytes());
    w.extend_from_slice(&32u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        w.extend_from_slice(&l.get(i).copied().unwrap_or(0.0).to_le_bytes());
        w.extend_from_slice(&r.get(i).copied().unwrap_or(0.0).to_le_bytes());
    }
    w
}

fn click_track(cues: &[Cue], frames: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; frames];
    let len = SR as usize * 2 / 1000;
    let amp = 10f32.powf(-20.0 / 20.0);
    for c in cues.iter().filter(|c| c.kind == SoundKind::Jump) {
        let s = (c.t * SR as f32) as usize + BLOCK;
        for i in 0..len {
            if s + i < frames {
                out[s + i] = amp * (i as f32 / SR as f32 * 2000.0 * std::f32::consts::TAU).sin();
            }
        }
    }
    out
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out = PathBuf::from("target/stream-cascade");
    let mut tag = "current".to_string();
    let mut rates: Vec<f32> = vec![5.0, 12.0, 25.0, 60.0];
    let mut seconds = 6.0f32;
    let mut text = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--tag" => tag = args.next().expect("--tag wants a value"),
            "--rates" => {
                rates = args
                    .next()
                    .expect("--rates wants a list")
                    .split(',')
                    .map(|s| s.parse::<f32>().expect("rate"))
                    .collect();
            }
            "--seconds" => seconds = args.next().expect("--seconds").parse().expect("seconds"),
            "--text" => text = true,
            other => out = PathBuf::from(other),
        }
    }
    std::fs::create_dir_all(&out).expect("out dir");
    let mut index = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out.join("INDEX.txt"))
        .expect("index");
    let suffix = if text { "-text" } else { "" };
    println!(
        "build={tag} seconds={seconds} text={text} window=[{T0:.2}, {:.2}] s",
        T0 + seconds
    );
    println!(
        "{:<8}{:>6}{:>5}{:>6}{:>7}{:>9}{:>9}{:>8}{:>8}{:>8}{:>9}{:>9}{:>9}{:>9}{:>9}{:>7}{:>7}",
        "build",
        "lps",
        "vol",
        "LFs",
        "heads",
        "restrike",
        "swallow",
        "births",
        "refused",
        "ondrop",
        "casc/s",
        "notes/s",
        "onset/s",
        "rms dB",
        "peak dB",
        "per",
        "turns"
    );
    for &rate in &rates {
        let cues = scenario(rate, seconds, text);
        let lfs = cues.iter().filter(|c| c.kind == SoundKind::Jump).count();
        for volume in [0.4f32, 1.0] {
            let r = render(&cues, seconds, volume);
            let w0 = (T0 * SR as f32) as usize;
            let w1 = ((T0 + seconds) * SR as f32) as usize;
            let win = &r.mono[w0..w1];
            let rms_db = db(rms(win));
            let peak_db = db(f64::from(win.iter().fold(0.0f32, |m, &v| m.max(v.abs()))));
            let ons = onsets(win);
            let [heads, restrikes, swallowed] = r.melody;
            let [births, refused, ondrop] = r.lane;
            let notes = births.saturating_sub(ondrop);
            // The contour the engine asked for: every admitted cascade voice's
            // f0 in time order — heads contribute their four notes, re-strikes
            // one — and the base line alone (each head's first note + every
            // re-strike), which is what "up and down" would be a property of.
            let all: Vec<f32> = r.log.iter().filter(|e| e.3).map(|e| e.1).collect();
            let mut base: Vec<f32> = Vec::new();
            let mut last_head_at = u32::MAX;
            for e in r.log.iter().filter(|e| e.3) {
                if e.2 {
                    if e.0 != last_head_at {
                        base.push(e.1);
                        last_head_at = e.0;
                    }
                } else {
                    base.push(e.1);
                }
            }
            let per = period_of(&base);
            let turns = direction_changes(&base);
            println!(
                "{:<8}{:>6.0}{:>5.1}{:>6}{:>7}{:>9}{:>9}{:>8}{:>8}{:>8}{:>9.2}{:>9.2}{:>9.2}{:>9.1}{:>9.1}{:>7}{:>7}",
                tag,
                rate,
                volume,
                lfs,
                heads,
                restrikes,
                swallowed,
                births,
                refused,
                ondrop,
                f64::from(heads) / f64::from(seconds),
                f64::from(notes) / f64::from(seconds),
                ons.len() as f64 / f64::from(seconds),
                rms_db,
                peak_db,
                per,
                turns
            );
            if volume == 0.4 {
                let fmin = base.iter().copied().fold(f32::MAX, f32::min);
                let fmax = base.iter().copied().fold(0.0f32, f32::max);
                let head_ts: Vec<u32> = {
                    let mut v: Vec<u32> =
                        r.log.iter().filter(|e| e.2 && e.3).map(|e| e.0).collect();
                    v.dedup();
                    v
                };
                let ioi: Vec<u32> = head_ts.windows(2).map(|w| w[1] - w[0]).collect();
                println!(
                    "  base line ({} notes, {:.0}..{:.0} Hz, repeat every {} notes{}): {}",
                    base.len(),
                    fmin,
                    fmax,
                    per,
                    if per > 0 && head_ts.len() > 1 {
                        format!(
                            " = {:.2} s",
                            per as f64 * ioi.iter().map(|&x| f64::from(x)).sum::<f64>()
                                / ioi.len().max(1) as f64
                                / 1000.0
                        )
                    } else {
                        String::new()
                    },
                    base.iter()
                        .take(24)
                        .map(|f| format!("{f:.0}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                println!(
                    "  all admitted cascade voices: {} (first 16 Hz: {})",
                    all.len(),
                    all.iter()
                        .take(16)
                        .map(|f| format!("{f:.0}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                println!(
                    "  head IOIs ms (first 8): {:?}; audio onsets {} — pitch at first 12 onsets: {}",
                    ioi.iter().take(8).collect::<Vec<_>>(),
                    ons.len(),
                    ons.iter()
                        .take(12)
                        .map(|&i| format!("{:.0}", pitch_at(win, i)))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                println!("  steals {} max_voices {}", r.steals, r.max_voices);
            }
            let path = out.join(format!("{tag}-lf{rate:.0}{suffix}-v{volume:.1}.wav"));
            let click = click_track(&cues, r.mono.len());
            std::fs::File::create(&path)
                .expect("wav")
                .write_all(&wav_stereo(&r.mono, &click))
                .expect("write");
            writeln!(
                index,
                "{} — {tag} {rate:.0} line feeds/s{} vol {volume:.1}: heads {heads} restrikes {restrikes} swallowed {swallowed} notes/s {:.2} rms {rms_db:.1} dBFS period {per} notes",
                path.file_name().unwrap().to_string_lossy(),
                if text { " + slow-stream echo" } else { "" },
                f64::from(notes) / f64::from(seconds),
            )
            .expect("index line");
            println!("wrote {}", path.display());
        }
    }
}
