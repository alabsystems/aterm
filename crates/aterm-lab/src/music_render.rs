// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `music-render`: a typing take through the trail synth to a WAV file, with the cost of each audio block.
//!
//! The bench types a text with a seeded timing model (no keystroke of a person is read or recorded),
//! pushes the events the host would build for those keys into a [`TrailSynth`] in 512-frame blocks at
//! 48 kHz, as the app's output queue does, and writes what the synth rendered. The same text, seed
//! and voice give the same bytes on every run.

use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::tone::{self, Tone, ToneScratch};
use aterm_effects::trail_sound::{
    CHANNELS, EventMeta, OutputGesture, SoundEvent, SoundGesture, SoundKind, SoundVoice,
    TrailSynth, typed_glyph_class, typed_glyph_rank,
};

use crate::instance::{take_flag, take_value};
use crate::paths::from_caller;
use crate::{Fail, Res};

/// The app's output rate (`aterm-gui`'s `trail_audio`).
pub const SAMPLE_RATE: u32 = 48_000;
/// The app's queue block, in frames.
pub const BLOCK_FRAMES: usize = 512;
/// One block's duration in nanoseconds: the budget a render must fit in.
const BLOCK_BUDGET_NS: u64 = BLOCK_FRAMES as u64 * 1_000_000_000 / SAMPLE_RATE as u64;
/// Where the first key lands in the file, so the take never starts on sample zero.
const LEAD_IN_MS: u32 = 500;
/// How long after the last key the render runs at least: past the pause that ends a phrase, so a
/// voice that answers it is heard.
const ANSWER_GRACE_MS: u32 = 1_600;
/// How long after Enter a command's result comes back under `--results`.
pub const RESULT_AFTER_MS: u32 = 350;
/// The columns the bench's line wraps its pan across.
const COLS: u32 = 100;
/// The typing-heat rise per key and the time constant it cools with.
const HEAT_PER_KEY: f32 = 0.12;
const HEAT_COOL_MS: f32 = 1_500.0;
/// A gap under this keeps the flow count; one over [`FLOW_BREAK_MS`] resets it.
const FLOW_FAST_MS: u32 = 250;
const FLOW_BREAK_MS: u32 = 700;
/// Fast keys in a row for full flow heat.
const FLOW_FULL_KEYS: f32 = 40.0;
/// The mood model's window, in characters (the host's is the same 160).
const TONE_WINDOW: usize = 160;

/// The text the bench types when none is given: prose, a question, a list, brackets and two commands.
pub const DEFAULT_TEXT: &str = "\
The kitty listens while you type. Every key is one note, and a sentence should climb, turn and come home.
Is that too much to ask of a music box?
Maybe! But look: (one, two, three) and then a rest.
git status
targo --unverified test -p aterm-effects
";

/// The verb's own help (`music-render --help`).
pub const HELP: &str = "\
usage: targo --unverified lab music-render [--text TEXT | --text-file FILE] [--voice NAME[,NAME]...]
         [--results PF-...] [--seed N] [--wpm N] [--gain X] [--tail-secs N] [--stem NAME] [--no-match]
         [<out_dir>]

Types a text with a seeded timing model and renders it through the trail synth to one 48 kHz stereo float WAV
per voice, <out_dir>/<stem>-<voice>.wav (default out_dir: $TMPDIR/.ubd/music-render.noindex). No keystroke of
a person is read. A newline is Enter, followed by the pause of reading a result. The same text, seed and voice
give the same bytes on every run.

--voice names are the sound picker's (`music box`, `marimba`, `felt`, `typewriter`, ...; default `music box`).
Several voices on one take are matched on energy, never on peak: each is scaled down to the quietest one's RMS
(--no-match writes them as rendered). --wpm sets the timing model's speed (default 70), --gain the event gain
(default 0.4), --tail-secs how long past the last key the render may run to reach silence (default 8).

--results makes every line a command typed at a shell prompt: Enter on a line that is not empty runs it, and its
result comes back 350 ms later. The letters say how the commands end, one each and around again: `p` passes,
`f` fails, `-` gives no result. A voice hears what the app would send it: `music box ii` every result, any
other voice only a failure (the app sends those a pass only from a command that ran two seconds).

One row per voice: frames and seconds, peak and RMS in dBFS, the gain the match applied, seconds from the last
key to exact silence (`silence_s=none` when the tail ran out first), and the render time of one 512-frame block
(p50, p99, max, in microseconds, over the blocks that had sound) with the p99's share of the block's 10.67 ms.

exit: 0; 2 a usage or file error
";

/// One event of a take: what the host would hand the synth for one key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    /// Milliseconds from the start of the take.
    pub at_ms: u32,
    /// The gesture.
    pub kind: SoundKind,
    /// The character behind it; `None` for a bare Shift and for Enter.
    pub ch: Option<char>,
    /// The key was shifted.
    pub shifted: bool,
    /// Where the cursor is, -0.9..0.9.
    pub pan: f32,
    /// Where the gesture started (Enter's flight starts at the end of its line).
    pub pan_from: f32,
    /// Typing heat, 0..1.
    pub heat: f32,
    /// Flow heat, 0..1.
    pub flow: f32,
    /// The ribbon's hue under the cursor, 0..1.
    pub hue: f32,
    /// The mood of the line so far.
    pub tone: Tone,
    /// Enter was pressed at a shell prompt, on a line that was not empty.
    pub prompt: bool,
    /// Not a key: the result of the command the Enter before it ran, `Some(failed)`.
    pub result: Option<bool>,
}

/// A deterministic integer generator (splitmix64), so a take is the same on every machine.
struct Rng(u64);

impl Rng {
    const fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `lo..=hi`.
    const fn within(&mut self, lo: u32, hi: u32) -> u32 {
        lo + (self.next() % (hi - lo + 1) as u64) as u32
    }
}

fn needs_shift(ch: char) -> bool {
    ch.is_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(ch)
}

fn pan_at(col: u32) -> f32 {
    (col.min(COLS) as f32 / COLS as f32) * 1.8 - 0.9
}

/// The mood of `line`'s last [`TONE_WINDOW`] characters, by the shipped model.
fn tone_of(line: &str, scratch: &mut ToneScratch) -> Tone {
    let Some(model) = tone::builtin() else {
        return Tone::default();
    };
    let skip = line.chars().count().saturating_sub(TONE_WINDOW);
    let start = line.char_indices().nth(skip).map_or(line.len(), |(i, _)| i);
    model.classify(&line[start..], scratch)
}

/// Type `text` at about `wpm` words a minute: the events of one take, in time order.
///
/// The model, all in integer milliseconds: each gap is the mean scaled by a triangular factor of
/// 0.6 to 1.4, a word's first letter is slower and the space before it quicker, one key in sixteen
/// hesitates, a comma and a full stop are followed by a breath, a shifted key is announced by its
/// Shift keydown when the key before it was not shifted, and Enter is followed by the pause of
/// reading what came back.
#[must_use]
pub fn type_text(text: &str, seed: u64, wpm: u32) -> Vec<Key> {
    let mut rng = Rng(seed);
    let mean = 60_000 / (wpm.clamp(10, 300) * 5);
    let mut keys = Vec::with_capacity(text.len() + text.len() / 8);
    let mut scratch = ToneScratch::new();
    let mut line = String::new();
    let (mut at, mut col, mut heat, mut fast_run) = (LEAD_IN_MS, 0u32, 0.0f32, 0u32);
    let (mut prev, mut prev_shifted, mut mood) = (None::<char>, false, Tone::default());
    for ch in text.chars().filter(|c| *c != '\r') {
        let mut gap = mean * (600 + (rng.within(0, 400) + rng.within(0, 400))) / 1_000;
        if prev.is_some_and(|p| p == ' ' || p == '\n') {
            gap = gap * 5 / 4;
        }
        if ch == ' ' {
            gap = gap * 9 / 10;
        }
        if ch == '\n' {
            gap = gap * 13 / 10;
        }
        if rng.within(0, 15) == 0 {
            gap = gap * rng.within(200, 400) / 100;
        }
        gap += match prev {
            Some(',' | ';' | ':') => rng.within(180, 320),
            Some('.' | '?' | '!') => rng.within(450, 900),
            Some('\n') => rng.within(900, 2_200),
            _ => 0,
        };
        let gap = if prev.is_none() { 0 } else { gap.max(25) };
        at += gap;
        heat = (heat / (1.0 + gap as f32 / HEAT_COOL_MS) + HEAT_PER_KEY).min(1.0);
        fast_run = match gap {
            g if g > FLOW_BREAK_MS => 0,
            g if g < FLOW_FAST_MS => fast_run + 1,
            _ => fast_run,
        };
        let shifted = needs_shift(ch);
        let pan = pan_at(col);
        let base = Key {
            at_ms: at,
            kind: SoundKind::Typed,
            ch: Some(ch),
            shifted,
            pan,
            pan_from: pan,
            heat,
            flow: (fast_run as f32 / FLOW_FULL_KEYS).min(1.0),
            hue: (col as f32 * 0.021).fract(),
            tone: mood,
            prompt: false,
            result: None,
        };
        // The take's first key has the whole lead-in before it; any other needs room in its gap.
        let room = if prev.is_none() { LEAD_IN_MS } else { gap };
        if shifted && !prev_shifted && room > 40 {
            keys.push(Key {
                at_ms: at - rng.within(20, 90).min(room - 20),
                kind: SoundKind::Shift,
                ch: None,
                shifted: false,
                ..base
            });
        }
        match ch {
            '\n' => {
                keys.push(Key {
                    kind: SoundKind::Enter {
                        cells: col.clamp(1, COLS) as u16,
                    },
                    ch: None,
                    shifted: false,
                    pan: pan_at(0),
                    ..base
                });
                col = 0;
                line.clear();
                mood = Tone::default();
            }
            ' ' => {
                keys.push(Key {
                    kind: SoundKind::Space,
                    shifted: false,
                    ..base
                });
                line.push(' ');
                mood = tone_of(&line, &mut scratch);
                col += 1;
            }
            _ => {
                keys.push(base);
                line.push(ch);
                col += 1;
            }
        }
        prev = Some(ch);
        prev_shifted = shifted;
    }
    keys
}

/// `take` as typed at a shell prompt: every Enter that ends a line with something on it runs a
/// command, whose result follows it by [`RESULT_AFTER_MS`]. `results` says how the commands end, one
/// letter each and around again: `p` a pass, `f` a failure, anything else no result.
#[must_use]
pub fn at_a_prompt(take: &[Key], results: &str) -> Vec<Key> {
    let letters: Vec<char> = results.chars().collect();
    let mut out = Vec::with_capacity(take.len() + take.len() / 16);
    let (mut typed_on_line, mut command) = (false, 0usize);
    for key in take {
        let enter = matches!(key.kind, SoundKind::Enter { .. });
        if !enter {
            typed_on_line |= key.kind != SoundKind::Shift;
            out.push(*key);
            continue;
        }
        out.push(Key {
            prompt: typed_on_line,
            ..*key
        });
        if typed_on_line && !letters.is_empty() {
            let failed = match letters[command % letters.len()] {
                'p' => Some(false),
                'f' => Some(true),
                _ => None,
            };
            if failed.is_some() {
                out.push(Key {
                    at_ms: key.at_ms + RESULT_AFTER_MS,
                    ch: None,
                    shifted: false,
                    prompt: false,
                    result: failed,
                    ..*key
                });
            }
            command += 1;
        }
        typed_on_line = false;
    }
    out
}

/// What one voice made of a take.
#[derive(Clone, Debug)]
pub struct Rendered {
    /// Interleaved stereo samples.
    pub samples: Vec<f32>,
    /// The frame of the block the last key was pushed in.
    pub last_key_frame: usize,
    /// The frame from which every later sample is exactly zero, when the tail reached silence.
    pub silent_frame: Option<usize>,
    /// Nanoseconds each block with sound took to render.
    pub block_ns: Vec<u64>,
}

/// The synth event for `key`, as the host's keyed seam builds it.
fn event_of(
    key: &Key,
    voice: SoundVoice,
    gain: f32,
    block_start_ms: f32,
) -> (SoundEvent, EventMeta) {
    let typed = matches!(key.kind, SoundKind::Typed | SoundKind::Space);
    (
        SoundEvent {
            style: GlowStyle::RainbowKitty,
            voice,
            kind: match key.result {
                Some(failed) => SoundGesture::Output(OutputGesture::Verdict { failed, ice: false }),
                None => SoundGesture::Trail(key.kind),
            },
            pan: key.pan,
            heat: key.heat,
            hue: key.hue,
            gain,
            tone: key.tone,
            bed: false,
            shifted: key.shifted,
        },
        EventMeta {
            at_ms: key.at_ms,
            glyph_class: if typed { typed_glyph_class(key.ch) } else { 0 },
            rank: if typed { typed_glyph_rank(key.ch) } else { 0 },
            pan_from: key.pan_from,
            block_lead_s: ((key.at_ms as f32 - block_start_ms) / 1_000.0).max(0.0),
            flow: key.flow,
            session: 1,
            prompt: key.prompt,
        },
    )
}

/// Render `take` through one synth with `voice`, block by block, then run the tail until the synth is
/// silent or `tail_secs` have passed.
#[must_use]
pub fn render(
    take: &[Key],
    voice: SoundVoice,
    gain: f32,
    synth_seed: u32,
    tail_secs: u32,
) -> Rendered {
    let mut synth = TrailSynth::new(SAMPLE_RATE as f32, synth_seed);
    let mut block = vec![0.0f32; BLOCK_FRAMES * CHANNELS];
    let last_ms = take.last().map_or(0, |k| k.at_ms);
    let tail_frames = tail_secs as usize * SAMPLE_RATE as usize;
    let mut out = Rendered {
        samples: Vec::new(),
        last_key_frame: 0,
        silent_frame: None,
        block_ns: Vec::new(),
    };
    let (mut frame, mut next) = (0usize, 0usize);
    loop {
        let start_ms = frame as f32 * 1_000.0 / SAMPLE_RATE as f32;
        let end_ms = (frame + BLOCK_FRAMES) as f32 * 1_000.0 / SAMPLE_RATE as f32;
        while next < take.len() && (take[next].at_ms as f32) < end_ms {
            // The app sends a quick pass to the new music box alone.
            if take[next].result != Some(false) || voice == SoundVoice::MusicBoxII {
                let (ev, meta) = event_of(&take[next], voice, gain, start_ms);
                synth.push_meta(ev, meta);
            }
            out.last_key_frame = frame;
            next += 1;
        }
        // Silence counts once a rest's answer has had its chance, and from where it began.
        let done = next == take.len() && start_ms >= (last_ms + ANSWER_GRACE_MS) as f32;
        let sounding = !synth.is_quiet();
        if sounding {
            out.silent_frame = None;
        } else if next == take.len() && out.silent_frame.is_none() {
            out.silent_frame = Some(frame);
        }
        let t = Instant::now();
        synth.render(&mut block);
        if sounding {
            out.block_ns.push(t.elapsed().as_nanos() as u64);
        }
        out.samples.extend_from_slice(&block);
        frame += BLOCK_FRAMES;
        if done && (!sounding || frame > out.last_key_frame + tail_frames) {
            return out;
        }
    }
}

/// The largest absolute sample.
#[must_use]
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// The root mean square of the take up to its last sample that is not zero.
#[must_use]
pub fn rms(samples: &[f32]) -> f64 {
    let end = samples.iter().rposition(|s| *s != 0.0).map_or(0, |i| i + 1);
    if end == 0 {
        return 0.0;
    }
    let sum: f64 = samples[..end]
        .iter()
        .map(|s| f64::from(*s) * f64::from(*s))
        .sum();
    (sum / end as f64).sqrt()
}

/// Scale every take down to the quietest one's RMS, and say the gain each got. A silent take is left alone.
pub fn match_energy(takes: &mut [Vec<f32>]) -> Vec<f64> {
    let levels: Vec<f64> = takes.iter().map(|t| rms(t)).collect();
    let target = levels
        .iter()
        .copied()
        .filter(|l| *l > 0.0)
        .fold(f64::INFINITY, f64::min);
    levels
        .iter()
        .zip(takes.iter_mut())
        .map(|(level, take)| {
            if *level <= 0.0 || !target.is_finite() {
                return 1.0;
            }
            let g = target / level;
            for s in take.iter_mut() {
                *s = (f64::from(*s) * g) as f32;
            }
            g
        })
        .collect()
}

/// `samples` (interleaved stereo) as a 32-bit float WAV file.
#[must_use]
pub fn wav_bytes(samples: &[f32]) -> Vec<u8> {
    const FLOAT: u16 = 3;
    let data = (samples.len() * 4) as u32;
    let frames = (samples.len() / CHANNELS) as u32;
    let block_align = (CHANNELS * 4) as u16;
    let mut b = Vec::with_capacity(58 + samples.len() * 4);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(50 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&18u32.to_le_bytes());
    b.extend_from_slice(&FLOAT.to_le_bytes());
    b.extend_from_slice(&(CHANNELS as u16).to_le_bytes());
    b.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    b.extend_from_slice(&(SAMPLE_RATE * u32::from(block_align)).to_le_bytes());
    b.extend_from_slice(&block_align.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(b"fact");
    b.extend_from_slice(&4u32.to_le_bytes());
    b.extend_from_slice(&frames.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

fn dbfs(x: f64) -> String {
    if x <= 0.0 {
        "-inf".to_string()
    } else {
        format!("{:.1}", 20.0 * x.log10())
    }
}

/// The value at `share` (0..=1) of the sorted `ns`, in microseconds.
fn percentile_us(sorted: &[u64], share: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let at = ((sorted.len() - 1) as f64 * share).round() as usize;
    sorted[at] as f64 / 1_000.0
}

/// The report row of one voice.
fn row(voice: SoundVoice, file: &Path, r: &Rendered, samples: &[f32], gain: f64) -> String {
    let frames = samples.len() / CHANNELS;
    let mut ns = r.block_ns.clone();
    ns.sort_unstable();
    let silence = r.silent_frame.map_or_else(
        || "none".to_string(),
        |f| {
            format!(
                "{:.2}",
                f.saturating_sub(r.last_key_frame) as f64 / f64::from(SAMPLE_RATE)
            )
        },
    );
    let p99 = percentile_us(&ns, 0.99);
    format!(
        "voice={:?} frames={frames} secs={:.2} peak_dbfs={} rms_dbfs={} match_gain_db={} silence_s={silence} \
         block_us_p50={:.1} block_us_p99={p99:.1} block_us_max={:.1} p99_share_of_block={:.2}% file={}",
        voice.name(),
        frames as f64 / f64::from(SAMPLE_RATE),
        dbfs(f64::from(peak(samples))),
        dbfs(rms(samples)),
        dbfs(gain),
        percentile_us(&ns, 0.5),
        percentile_us(&ns, 1.0),
        p99 * 1_000.0 / BLOCK_BUDGET_NS as f64 * 100.0,
        file.display(),
    )
}

/// What the command line asked for.
struct Args {
    text: String,
    voices: Vec<SoundVoice>,
    seed: u64,
    wpm: u32,
    gain: f32,
    tail_secs: u32,
    stem: String,
    matched: bool,
    results: Option<String>,
    out_dir: PathBuf,
}

fn number<T: std::str::FromStr>(flag: &str, v: Option<String>, default: T) -> Result<T, Fail> {
    match v {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|_| Fail::usage(format!("music-render: {flag} {v:?} is not a number"))),
    }
}

fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut args = args.to_vec();
    let text = match (
        take_value(&mut args, "--text")?,
        take_value(&mut args, "--text-file")?,
    ) {
        (Some(_), Some(_)) => {
            return Err(Fail::usage(
                "music-render: give --text or --text-file, not both",
            ));
        }
        (Some(t), None) => t.replace("\\n", "\n"),
        (None, Some(f)) => std::fs::read_to_string(from_caller(&f))
            .map_err(|e| Fail::usage(format!("music-render: {f}: {e}")))?,
        (None, None) => DEFAULT_TEXT.to_string(),
    };
    let mut voices = Vec::new();
    while let Some(list) = take_value(&mut args, "--voice")? {
        for name in list.split(',') {
            let voice = SoundVoice::parse(name)
                .ok_or_else(|| Fail::usage(format!("music-render: no voice named {name:?}")))?;
            if !voices.contains(&voice) {
                voices.push(voice);
            }
        }
    }
    if voices.is_empty() {
        voices.push(SoundVoice::RainbowKittyV2);
    }
    let seed = number("--seed", take_value(&mut args, "--seed")?, 1u64)?;
    let wpm = number("--wpm", take_value(&mut args, "--wpm")?, 70u32)?;
    let gain = number("--gain", take_value(&mut args, "--gain")?, 0.4f32)?;
    let tail_secs = number("--tail-secs", take_value(&mut args, "--tail-secs")?, 8u32)?;
    if !(0.0..=1.0).contains(&gain) {
        return Err(Fail::usage("music-render: --gain is 0 to 1"));
    }
    let stem = take_value(&mut args, "--stem")?.unwrap_or_else(|| "take".to_string());
    if stem.is_empty() || stem.contains('/') {
        return Err(Fail::usage(
            "music-render: --stem is a file name's start, with no slash",
        ));
    }
    let matched = !take_flag(&mut args, "--no-match");
    let results = take_value(&mut args, "--results")?;
    if results
        .as_ref()
        .is_some_and(|r| r.is_empty() || !r.chars().all(|c| "pf-".contains(c)))
    {
        return Err(Fail::usage(
            "music-render: --results is letters of `p`, `f` and `-`",
        ));
    }
    if let Some(flag) = args.iter().find(|a| a.starts_with("--")) {
        return Err(Fail::usage(format!("music-render: no flag {flag}")));
    }
    let out_dir = match args.as_slice() {
        [] => std::env::temp_dir()
            .join(".ubd")
            .join("music-render.noindex"),
        [dir] => PathBuf::from(from_caller(dir)),
        _ => return Err(Fail::usage("music-render: one output folder")),
    };
    if text.trim().is_empty() {
        return Err(Fail::usage("music-render: the text is empty"));
    }
    Ok(Args {
        text,
        voices,
        seed,
        wpm,
        gain,
        tail_secs,
        stem,
        matched,
        results,
        out_dir,
    })
}

/// `music-render`: render a typed text through each named voice and write the files.
/// # Errors
/// A usage error, or a file that could not be read or written.
pub fn cmd(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    let a = parse_args(args)?;
    let take = type_text(&a.text, a.seed, a.wpm);
    let take = match &a.results {
        Some(results) => at_a_prompt(&take, results),
        None => take,
    };
    let renders: Vec<Rendered> = a
        .voices
        .iter()
        .map(|v| render(&take, *v, a.gain, a.seed as u32, a.tail_secs))
        .collect();
    let mut samples: Vec<Vec<f32>> = renders.iter().map(|r| r.samples.clone()).collect();
    let gains = if a.matched && samples.len() > 1 {
        match_energy(&mut samples)
    } else {
        vec![1.0; samples.len()]
    };
    std::fs::create_dir_all(&a.out_dir)
        .map_err(|e| Fail::usage(format!("music-render: {}: {e}", a.out_dir.display())))?;
    let results = take.iter().filter(|k| k.result.is_some()).count();
    println!(
        "take keys={} results={results} last_key_s={:.2} seed={} wpm={}",
        take.len() - results,
        f64::from(take.last().map_or(0, |k| k.at_ms)) / 1_000.0,
        a.seed,
        a.wpm
    );
    for (((voice, r), s), gain) in a.voices.iter().zip(&renders).zip(&samples).zip(&gains) {
        let file = a
            .out_dir
            .join(format!("{}-{}.wav", a.stem, voice.name().replace(' ', "-")));
        std::fs::write(&file, wav_bytes(s))
            .map_err(|e| Fail::usage(format!("music-render: {}: {e}", file.display())))?;
        println!("{}", row(*voice, &file, r, s, *gain));
    }
    Ok(0)
}

#[cfg(test)]
#[path = "music_render_tests.rs"]
mod tests;
