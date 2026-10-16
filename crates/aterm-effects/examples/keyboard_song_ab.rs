// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keyboard-song A/B bench: a delete that poofs, a spacebar that is a low
//! musical downbeat, and typing that reads as a pleasant song over long
//! sessions. It types real prose (every space a [`SoundKind::Space`], every
//! newline a [`SoundKind::Jump`]) at realistic rates, plus the edit and
//! whitespace corner cases.
//!
//!   targo --unverified run --release -p aterm-effects \
//!       --example keyboard_song_ab -- <out_dir> \
//!       [--tag <name>] [--voice <name>] [--style <name>]
//!       [--cps <rate>] [--jitter <pct>] [--seed <n>] [--bed on|off]
//!       [--timbre plain|bloom|hue|room] [--jitterfix ship|j0|j1] [--metronome]
//!       [--census] [--probes] [--scripts] [--text "<s>"]
//!
//! - `--scripts` renders the listening reel's fixed scripts ([`SCRIPTS`]) and
//!   `--text "<s>"` one line of the caller's own, through the product's key path
//!   (a bare Shift ahead of every shifted run, Return as [`SoundKind::Enter`]),
//!   one TSV row per take.
//! - `--metronome` types the prose scene with no rests, as the census does, so a
//!   WAV pair at two rates makes tempo invariance audible.
//! - `--census` sweeps 3-14 cps, clean and at ±25 % jitter, over several
//!   corpora, reporting keys, steps, distinct-pitch %, melody notes/s, silent
//!   keys and a hash of the degree sequence, read off the engine's own
//!   [`aterm_effects::trail_sound::MelodyV2`] hooks. No WAVs.
//!
//! Every cue goes through [`TrailSynth::push_meta`] with its scripted press time,
//! so the melody runs on the shipping clock (bare `push` falls back to the
//! 512-frame block clock). Every WAV is stereo: left the synth, right a 2 ms
//! 2 kHz tick at each key's press time (−20 dBFS). The music channel leads with
//! [`PRE_ROLL_FRAMES`] of silence (the host's queue depth), so the file shows the
//! real finger-to-note offset.
//!
//! Each scenario renders at the host default volume (0.4) and 1.0 with no
//! normalisation; files are `<tag>-<scenario>-v<volume>.wav`. Reported per
//! scenario: pitched onsets/s, pre-clip peak (the soft clipper inverted),
//! post-clip peak, RMS, crest, max live voices, voice steals and realtime factor,
//! plus a gesture probe table ([`PROBES`], each from one settled melody state)
//! with spectral centroid, high-band fractions, tonality, peak and onset counts.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::tone::Tone;
use aterm_effects::trail_sound::{
    CHANNELS, EventMeta, MIN_GAP, SPACE_HEAD_CENTROID_FLOOR_HZ, SPACE_HEAD_HI_FLOOR,
    SPACE_HEAD_RE_TYPED_DB, SoundEvent, SoundGesture, SoundKind, SoundVoice, TimbreStops,
    TrailSynth,
};

const SR: u32 = 48_000;
const SEED: u32 = 0x50_4F_4F_46; // "POOF"
/// The host's audio device block (`aterm-gui`'s `trail_audio::BUFFER_FRAMES`),
/// so the render's cue quantisation matches the shipping one.
const BLOCK: usize = 512;

/// The two audio buffers the host's output queue holds rendered when a fresh
/// cue arrives: `(BUFFER_COUNT − 1) × BUFFER_FRAMES` = 1024 frames = 21.3 ms at
/// 48 kHz, the steady-state finger-to-note distance. Prepended to the music
/// channel only, so the click track keeps the key's own time.
const PRE_ROLL_FRAMES: usize = 2 * BLOCK;

/// One scripted cue — a key press, with everything the shipping seam would
/// know about it.
#[derive(Clone, Copy, Debug)]
struct Cue {
    /// The press time, seconds into the take. This is the number that becomes
    /// [`EventMeta::at_ms`] and the number the click track ticks at: one time
    /// source for the melody's clock and for the ear's reference.
    t: f32,
    gesture: SoundGesture,
    pan: f32,
    heat: f32,
    /// [`SoundEvent::shifted`]: set on capitals and shifted symbols.
    shifted: bool,
    /// The character the key put on screen (`'\0'` where no glyph backs the
    /// cue: a bare Shift, a held Backspace). The melody is derived from what
    /// was typed, so the bench passes it through [`EventMeta::glyph_class`]
    /// and [`EventMeta::rank`].
    ch: char,
}

impl Cue {
    /// The key's time on the host input clock, ms: what the melody's gate reads
    /// and the click track ticks at. `0` means "no host stamp" to `v2_at_ms`,
    /// so the first millisecond is pushed as 1.
    fn at_ms(self) -> u32 {
        ((self.t * 1000.0) as u32).max(1)
    }

    /// The host side-car for this press.
    fn meta(self) -> EventMeta {
        // A key at a password prompt: the host stamps class `SECRET`, rank 0.
        if self.ch == SECRET_CH {
            return EventMeta {
                at_ms: self.at_ms(),
                glyph_class: aterm_effects::trail_sound::glyph_class::SECRET,
                ..EventMeta::default()
            };
        }
        EventMeta {
            at_ms: self.at_ms(),
            // Glyph class and alphabet rank through the engine's own
            // producers, so the bench cannot disagree with the shipping tables.
            glyph_class: aterm_effects::trail_sound::typed_glyph_class(
                (self.ch != '\0').then_some(self.ch),
            ),
            rank: aterm_effects::trail_sound::typed_glyph_rank(
                (self.ch != '\0').then_some(self.ch),
            ),
            // The keyed seam has no travel: a keystroke's origin is its column.
            // Only the meteor reads this.
            pan_from: 0.0,
            // The bench spawns each cue at its own frame (`BlockFix::spawn_frame`),
            // so there is no pre-roll to hand over.
            block_lead_s: 0.0,
            // Flow heat: a scripted take has no hand, so it plays the cold box
            // (0.0 is the identity).
            flow: 0.0,
        }
    }
}

/// [`Cue::ch`] for a key typed at a no-echo prompt: the bench's spelling of the
/// host's `SECRET` stamp, in the private-use area so no script can type it.
const SECRET_CH: char = '\u{E000}';

/// The glyphs a US layout cannot produce without Shift.
fn needs_shift(ch: char) -> bool {
    ch.is_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(ch)
}

/// A deterministic xorshift32: `--seed N` twice renders byte-identically and
/// two seeds are two different hands on one text.
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        // 0 is xorshift's fixed point; anything else is a full-period state.
        Self(if seed == 0 { 0x9E37_79B9 } else { seed })
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Uniform in [−1, 1].
    fn bipolar(&mut self) -> f32 {
        f64::from(self.next_u32()) as f32 / (u32::MAX as f32) * 2.0 - 1.0
    }
}

/// The hand: a typing rate and how much it wobbles. Human inter-key gaps at
/// prose speed carry ±25 %, and the derived line reads that wobble as an
/// accelerating or hesitating hand ([`ACCEL_SHARE`] in the engine), so the
/// jitter column shows the melody is generated from the typing pattern.
struct Hand {
    cps: f32,
    /// Fractional, so 0.25 is ±25 %.
    jitter: f32,
    rng: Rng,
}

impl Hand {
    fn new(cps: f32, jitter: f32, seed: u32) -> Self {
        Self {
            cps,
            jitter,
            rng: Rng::new(seed),
        }
    }

    /// A steady metronome (no jitter, the default seed).
    fn steady(cps: f32) -> Self {
        Self::new(cps, 0.0, SEED)
    }

    /// The next inter-key interval, seconds.
    fn dt(&mut self) -> f32 {
        let base = 1.0 / self.cps;
        if self.jitter <= 0.0 {
            base
        } else {
            base * (1.0 + self.jitter * self.rng.bipolar())
        }
    }
}

// ---------------------------------------------------------------------------
// The scripts — real text, real spaces
// ---------------------------------------------------------------------------

/// The prose corpus: ordinary English at ordinary word lengths, so the space
/// cadence is real (~2 per second at 10 cps). Every `\n` is an Enter.
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
fn type_text(cues: &mut Vec<Cue>, t0: f32, hand: &mut Hand, text: &str, heat: f32) -> f32 {
    let mut t = t0;
    let mut col = 0.0f32;
    for ch in text.chars() {
        // The pan the host would send: the caret's column across the pane.
        let pan = (col / 68.0).clamp(0.0, 1.0) * 1.8 - 0.9;
        let kind = match ch {
            ' ' => SoundKind::Space,
            '\n' => SoundKind::Jump,
            _ => SoundKind::Typed,
        };
        cues.push(Cue {
            t,
            gesture: SoundGesture::Trail(kind),
            pan,
            heat,
            shifted: needs_shift(ch),
            ch,
        });
        t += hand.dt();
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

/// Type `text` as a strict metronome: one cue per character at the hand's
/// interval and no rests. The census reads the melody at one rate, and a rest
/// is a different rule on a different clock (`PHRASE_PAUSE_MS`). The ear's
/// takes keep [`type_text`]'s rests.
fn type_metronome(cues: &mut Vec<Cue>, t0: f32, hand: &mut Hand, text: &str, heat: f32) -> f32 {
    let mut t = t0;
    let mut col = 0.0f32;
    for ch in text.chars() {
        let pan = (col / 68.0).clamp(0.0, 1.0) * 1.8 - 0.9;
        let kind = match ch {
            ' ' => SoundKind::Space,
            '\n' => SoundKind::Jump,
            _ => SoundKind::Typed,
        };
        cues.push(Cue {
            t,
            gesture: SoundGesture::Trail(kind),
            pan,
            heat,
            shifted: needs_shift(ch),
            ch,
        });
        t += hand.dt();
        if ch == '\n' {
            col = 0.0;
        } else {
            col += 1.0;
        }
    }
    t
}

/// 60 s of prose at `cps` with a typist's pauses, looping the corpus.
/// `metronome` drops every rest, so the take's first pass (403 typed keys) is
/// the census's `prose` row at that rate.
fn scenario_prose(cps: f32, jitter: f32, seed: u32, metronome: bool) -> Scenario {
    let mut cues = Vec::new();
    let mut hand = Hand::new(cps, jitter, seed);
    let mut t = 0.5f32;
    while t < 60.0 {
        if metronome {
            t = type_metronome(&mut cues, t, &mut hand, PROSE, 0.55);
        } else {
            t = type_text(&mut cues, t, &mut hand, PROSE, 0.55);
            t += 1.4; // between paragraphs, a longer think
        }
    }
    cues.retain(|c| c.t < 60.0);
    Scenario {
        name: "prose".into(),
        cues,
        seconds: 61.5,
        window: (0.5, 60.0),
    }
}

/// The edit scenario: `type 12 → delete 12` three times, then twenty
/// alternating type/delete pairs, then a held backspace (30 Hz auto-repeat,
/// the rate the min-gap governor has to survive).
fn scenario_edit() -> Scenario {
    /// The word this scenario writes, erases and writes again: twelve distinct
    /// letters, one per key of a lap, so the retyped melody has notes to
    /// un-write (a repeated letter would be a stride of zero).
    const EDIT_WORD: &str = "unmistakable";
    let mut cues = Vec::new();
    let mut t = 0.5f32;
    let push = |cues: &mut Vec<Cue>, t: f32, k: SoundKind, pan: f32, ch: char| {
        cues.push(Cue {
            t,
            gesture: SoundGesture::Trail(k),
            pan,
            heat: 0.5,
            shifted: false,
            // An erase and a held Backspace put no glyph on screen.
            ch: if k == SoundKind::Typed { ch } else { '\0' },
        });
    };
    let word: Vec<char> = EDIT_WORD.chars().collect();
    for _ in 0..3 {
        for (i, ch) in word.iter().enumerate() {
            push(&mut cues, t, SoundKind::Typed, -0.5 + i as f32 * 0.08, *ch);
            t += 0.1;
        }
        t += 0.3;
        for i in 0..word.len() {
            push(
                &mut cues,
                t,
                SoundKind::Backspace,
                0.46 - i as f32 * 0.08,
                '\0',
            );
            t += 0.1;
        }
        t += 0.7;
    }
    t += 0.6;
    // Alternating: a poof must not be swallowed by the keystroke inside the
    // global min-gap. The letters advance through the word, so a swallowed
    // note is audible as a hole.
    for i in 0..20 {
        push(&mut cues, t, SoundKind::Typed, 0.0, word[i % word.len()]);
        t += 0.09;
        push(&mut cues, t, SoundKind::Backspace, 0.0, '\0');
        t += 0.09;
    }
    t += 0.9;
    // A held Backspace: 30 per second for a second.
    for _ in 0..30 {
        push(&mut cues, t, SoundKind::Backspace, 0.2, '\0');
        t += 1.0 / 30.0;
    }
    Scenario {
        name: "edit".into(),
        cues,
        seconds: t + 2.0,
        window: (0.4, t + 1.0),
    }
}

/// The whitespace scenario: ordinary word spaces, then single-letter words
/// (`a a a a`), four-space indentation and an eight-space run.
fn scenario_space() -> Scenario {
    let mut cues = Vec::new();
    let mut t = 0.5f32;
    let hand = &mut Hand::steady(9.0);
    t = type_text(
        &mut cues,
        t,
        hand,
        "the quick brown fox jumps over it\n",
        0.5,
    );
    t += 1.0;
    t = type_text(&mut cues, t, hand, "a a a a a a a a\n", 0.5);
    t += 1.0;
    t = type_text(&mut cues, t, hand, "    indented    twice\n", 0.5);
    t += 1.0;
    t = type_text(&mut cues, t, hand, "gap        here\n", 0.5);
    t += 0.8;
    Scenario {
        name: "space".into(),
        cues,
        seconds: t + 1.5,
        window: (0.4, t + 0.8),
    }
}

/// The shift scenario: three passes over the same words (lower case,
/// Capitalised, SHOUTED) with identical cadence and pans, so only `shifted`
/// moves.
fn scenario_shift() -> Scenario {
    let mut cues = Vec::new();
    let mut t = 0.5f32;
    for text in [
        "the quick brown fox jumps\n",
        "The Quick Brown Fox Jumps\n",
        "THE QUICK BROWN FOX JUMPS\n",
    ] {
        t = type_text(&mut cues, t, &mut Hand::steady(9.0), text, 0.5);
        t += 0.9;
    }
    // …and the mixed case a person actually types: a sentence with one capital
    // and a shifted symbol.
    t = type_text(
        &mut cues,
        t,
        &mut Hand::steady(9.0),
        "Hello, World! (a test)\n",
        0.5,
    );
    Scenario {
        name: "shift".into(),
        cues,
        seconds: t + 1.5,
        window: (0.4, t + 0.8),
    }
}

/// The rotation scenario: does a repeated Shift or Space rotate its tone?
///
/// - A. Twenty bare [`SoundKind::Shift`] presses 0.45 s apart, alone and
///   centred, so the melody never steps and any pitch variety is the ting's
///   own.
/// - B. Twenty-four words of prose at 9 cps (two laps of an eight-step bass
///   walk, two of a twelve), so a walk returning to its root too soon shows
///   as a repeat.
fn scenario_rotate() -> Scenario {
    let mut cues = Vec::new();
    let mut t = 0.5f32;
    for _ in 0..20 {
        cues.push(Cue {
            t,
            gesture: SoundGesture::Trail(SoundKind::Shift),
            pan: 0.0,
            heat: 0.5,
            shifted: false,
            // A bare modifier puts no glyph on screen.
            ch: '\0',
        });
        t += 0.45;
    }
    t += 1.2;
    t = type_text(
        &mut cues,
        t,
        &mut Hand::steady(9.0),
        "the renderer keeps one atlas per face and never uploads a glyph twice \
         because a cache that is wrong is worse than no cache at all here\n",
        0.5,
    );
    Scenario {
        name: "rotate".into(),
        cues,
        seconds: t + 1.5,
        window: (0.4, t + 0.8),
    }
}

/// A 20 cps burst, twice the sustained rate the governor is tuned for, for six
/// seconds, with spaces at real word cadence.
fn scenario_burst() -> Scenario {
    /// The letters the burst is made of: real text, cycled, spaces removed
    /// (the script cues its own on the sixth key), so each key is its own note.
    const BURST_LETTERS: &str = "therendererkeepsoneatlasperfaceandneveruploadsaglyphtwice";
    let letters: Vec<char> = BURST_LETTERS.chars().collect();
    let mut cues = Vec::new();
    let mut t = 0.5f32;
    let mut i = 0usize;
    let mut li = 0usize;
    while t < 6.5 {
        // ~5.5 characters per word, so a space lands where one would.
        let kind = if i % 6 == 5 {
            SoundKind::Space
        } else {
            SoundKind::Typed
        };
        let ch = if kind == SoundKind::Space {
            ' '
        } else {
            let c = letters[li % letters.len()];
            li += 1;
            c
        };
        cues.push(Cue {
            t,
            gesture: SoundGesture::Trail(kind),
            pan: ((i % 60) as f32 / 30.0) - 1.0,
            heat: 0.9,
            shifted: false,
            ch,
        });
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

/// The pathological scenario: held letters, a digit run, bare exclamation
/// marks, a base64 blob and a repeated word, typed fast. Text with no prosody
/// and long runs of one character is where a derived melody would turn into a
/// siren or machine gun.
fn scenario_pathological() -> Scenario {
    let mut cues = Vec::new();
    let mut t = 0.5f32;
    let hand = &mut Hand::steady(12.0);
    // A held letter at auto-repeat speed, thirty times.
    t = type_metronome(&mut cues, t, hand, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 0.6);
    t += 0.7;
    // Digits, which the glyph class separates from letters.
    t = type_metronome(&mut cues, t, hand, "0123456789 0123456789", 0.6);
    t += 0.7;
    // The hero glyph, four times, alone.
    t = type_metronome(&mut cues, t, hand, "!!!!", 0.6);
    t += 0.7;
    // A base64 blob: capitals, digits and punctuation with no word shape, so the
    // Shift path fires at random and no boundary ever arrives.
    t = type_metronome(
        &mut cues,
        t,
        hand,
        "aGVsbG8gd29ybGQgdGhpcyBpcyBub3QgcHJvc2U+Pz8/",
        0.6,
    );
    t += 0.7;
    // One word, over and over: the repetition a pattern-derived melody must not
    // turn into a drone.
    t = type_metronome(&mut cues, t, hand, "the the the the the the the the ", 0.6);
    t += 0.7;
    // The same text slow, so both ends of the speed range are covered.
    t = type_metronome(&mut cues, t, &mut Hand::steady(3.5), "!!!! 999 aaaa", 0.6);
    Scenario {
        name: "pathological".into(),
        cues,
        seconds: t + 1.5,
        window: (0.4, t + 0.8),
    }
}

// ---------------------------------------------------------------------------
// The listening reel: `--scripts` / `--text`, the product's own key path
// ---------------------------------------------------------------------------
//
// Fixed texts typed the way the product hears them, for before/after
// listening: a bare Shift ahead of each shifted run and Return as the keyed
// [`SoundKind::Enter`]. The scenarios above keep their own typist.

/// One fixed script of the reel.
struct Script {
    name: &'static str,
    text: &'static str,
}

/// The reel's sentence: every phrase mark, a word-head capital after each
/// kind of cadence, a decimal, a parenthesis and a hyphen.
const SCRIPT_PROSE: &str =
    "Hello, World! Is this it? Yes; it is: 3.14 (roughly) - well-known, done.\n";

/// The fixed scripts. `shift-space`, `prose-lower` and `password` are built in
/// [`script_scenario`].
const SCRIPTS: [Script; 5] = [
    Script {
        name: "prose",
        text: SCRIPT_PROSE,
    },
    Script {
        name: "code",
        text: "let v = self.v2.walk(x[0], &y) + foo::bar(42); // ok\n\
               if (a != b) { return f(a, b)?; }\n",
    },
    Script {
        name: "caps",
        text: "the quick BROWN FOX Jumps Over THE LAZY DOG",
    },
    Script {
        name: "digits",
        text: "0123456789 2026 42 3.14159 10,000 v0.88.0",
    },
    // Punctuation phrasing: a quotation that lifts and comes home, an aside
    // inside it, a question whose Return lands on the G, then a full stop's
    // Return on the C.
    Script {
        name: "phrasing",
        text: "She said \"wait (not yet) please?\"\nok.\n",
    },
];

/// The two rates every script is typed at: an unhurried hand and a fast one.
const SCRIPT_CPS: [f32; 2] = [6.0, 10.0];

/// How long a take runs past its last key, so the longest tail in it (the
/// Enter's bell, a ting) has rung out before the file ends.
const SCRIPT_TAIL_S: f32 = 2.5;

/// `prose-lower`: the reel's sentence lowercased with marks stripped and
/// doubled spaces collapsed; the plain-typing loudness reference.
fn prose_lower() -> String {
    let mut out = String::new();
    for ch in SCRIPT_PROSE.chars() {
        let ch = ch.to_ascii_lowercase();
        let keep = ch.is_ascii_alphanumeric() || ch == '\n' || ch == ' ';
        if keep && !(ch == ' ' && out.ends_with(' ')) {
            out.push(ch);
        }
    }
    out
}

/// A script as cues, plus the windows its ting + word-head capital pairs sound
/// in (the Shift's press to [`CREST_AFTER_CAPITAL_S`] past the letter).
struct ScriptTake {
    scenario: Scenario,
    crest_windows: Vec<(f32, f32)>,
}

/// How far past the capital the crest window runs: the strike's own peak and
/// the ring that swells 60 ms behind it.
const CREST_AFTER_CAPITAL_S: f32 = 0.150;

/// Type `text` the way the product hears it:
///
/// - one bare [`SoundKind::Shift`] cue [`CAPITAL_SHIFT_LEAD_MS`] before each
///   maximal run of [`needs_shift`] characters (the hand holds Shift across a
///   run and lets go for anything unshifted, Space included);
/// - `\n` is a keyed Return: [`SoundKind::Enter`] with the column as its
///   flight distance;
/// - every cue stamped through [`Cue::meta`].
///
/// A sentence rests only where it ends (`.` `!` `?` followed by whitespace or
/// the end), not inside `3.14` or `self.v2.walk`.
fn type_script(name: String, text: &str, cps: f32, seed: u32) -> ScriptTake {
    let mut cues = Vec::new();
    let mut crest_windows = Vec::new();
    let mut hand = Hand::new(cps, 0.0, seed);
    let chars: Vec<char> = text.chars().collect();
    let lead = CAPITAL_SHIFT_LEAD_MS as f32 / 1000.0;
    let mut t = 0.5f32;
    let mut col = 0u16;
    let mut in_run = false;
    for (i, &ch) in chars.iter().enumerate() {
        let pan = (f32::from(col) / 68.0).clamp(0.0, 1.0) * 1.8 - 0.9;
        let shifted = needs_shift(ch);
        if shifted && !in_run {
            cues.push(Cue {
                t: t - lead,
                gesture: SoundGesture::Trail(SoundKind::Shift),
                pan,
                heat: 0.5,
                shifted: false,
                ch: '\0',
            });
            let word_head = i == 0 || chars[i - 1].is_whitespace();
            if ch.is_uppercase() && word_head {
                crest_windows.push((t - lead, t + CREST_AFTER_CAPITAL_S));
            }
        }
        in_run = shifted;
        let (kind, glyph) = match ch {
            ' ' => (SoundKind::Space, ch),
            // No glyph backs a Return.
            '\n' => (SoundKind::Enter { cells: col }, '\0'),
            _ => (SoundKind::Typed, ch),
        };
        cues.push(Cue {
            t,
            gesture: SoundGesture::Trail(kind),
            pan,
            heat: 0.5,
            shifted,
            ch: glyph,
        });
        t += hand.dt();
        if ch == '\n' {
            col = 0;
            t += 0.35; // a line ending is a beat of thought, as in `type_text`
        } else {
            col += 1;
        }
        let ends = chars.get(i + 1).is_none_or(|c| c.is_whitespace());
        if matches!(ch, '.' | '!' | '?') && ends {
            t += 0.55; // sentence rest
        }
    }
    // Sort (stable) so the cue list is in press order at any rate.
    cues.sort_by(|a, b| a.t.partial_cmp(&b.t).expect("finite cue times"));
    ScriptTake {
        scenario: Scenario {
            name,
            cues,
            seconds: t + SCRIPT_TAIL_S,
            window: (0.4, t + 1.0),
        },
        crest_windows,
    }
}

/// `shift-space`: sixteen alternating bare Shift / Space cues 0.45 s apart.
/// Eight Spaces walk the whole chord loop, so every ting is heard against
/// the low tone it is supposed to answer.
fn script_shift_space(name: String) -> ScriptTake {
    let mut cues = Vec::new();
    let mut t = 0.5f32;
    for i in 0..16 {
        let (kind, ch) = if i % 2 == 0 {
            (SoundKind::Shift, '\0')
        } else {
            (SoundKind::Space, ' ')
        };
        cues.push(Cue {
            t,
            gesture: SoundGesture::Trail(kind),
            pan: 0.0,
            heat: 0.5,
            shifted: false,
            ch,
        });
        t += 0.45;
    }
    ScriptTake {
        scenario: Scenario {
            name,
            cues,
            seconds: t + SCRIPT_TAIL_S,
            window: (0.4, t + 1.0),
        },
        crest_windows: Vec::new(),
    }
}

/// `password`: a `sudo` line, a password typed at its no-echo prompt, the
/// Return, and a line after it. The password is what the host sends: every key
/// a `Typed` cue stamped `SECRET` (the space too), unshifted, at one pan, with
/// no bare Shift. The following line should sound as if the password had never
/// been typed.
const PASSWORD_BEFORE: &str = "sudo make install\n";
const PASSWORD: &str = "Correct Horse 42!";
const PASSWORD_AFTER: &str = "done, it built.\n";

fn script_password(name: String, cps: f32, seed: u32) -> ScriptTake {
    let before = type_script(String::new(), PASSWORD_BEFORE, cps, seed);
    let mut cues = before.scenario.cues;
    let mut crest_windows = before.crest_windows;
    let mut t = before.scenario.seconds - SCRIPT_TAIL_S;
    let mut hand = Hand::new(cps, 0.0, seed);
    for _ in PASSWORD.chars() {
        cues.push(Cue {
            t,
            gesture: SoundGesture::Trail(SoundKind::Typed),
            pan: -0.9,
            heat: 0.5,
            shifted: false,
            ch: SECRET_CH,
        });
        t += hand.dt();
    }
    cues.push(Cue {
        t,
        gesture: SoundGesture::Trail(SoundKind::Enter { cells: 0 }),
        pan: -0.9,
        heat: 0.5,
        shifted: false,
        ch: '\0',
    });
    t += hand.dt() + 0.35;
    // `type_script` starts its first key at 0.5 s.
    let after = type_script(String::new(), PASSWORD_AFTER, cps, seed);
    let shift = t - 0.5;
    cues.extend(after.scenario.cues.into_iter().map(|c| Cue {
        t: c.t + shift,
        ..c
    }));
    crest_windows.extend(
        after
            .crest_windows
            .into_iter()
            .map(|(a, b)| (a + shift, b + shift)),
    );
    let seconds = after.scenario.seconds + shift;
    ScriptTake {
        scenario: Scenario {
            name,
            cues,
            seconds,
            window: (0.4, seconds - SCRIPT_TAIL_S + 1.0),
        },
        crest_windows,
    }
}

/// The reel: every script at every rate and both volumes, one WAV and one TSV
/// row per take. `text` (`--text`) replaces the fixed scripts; `rates` is
/// [`SCRIPT_CPS`] unless `--cps` named one.
#[allow(clippy::too_many_arguments)]
fn script_reel(
    out: &std::path::Path,
    tag: &str,
    text: Option<&str>,
    rates: &[f32],
    voice: SoundVoice,
    style: GlowStyle,
    fix: BlockFix,
    bed: bool,
    seed: u32,
    timbre: Timbre,
) {
    let mut tsv = String::from(
        "script\tcps\tvol\tkeys\tpeak_dbfs\trms_db\tcentroid_hz\tfrac_gt_2k\tfrac_gt_6k\t\
         band_2k8_5k6_db\tmax_voices\tsteals\tting_capital_crest_dbfs\tdegrees\n",
    );
    let lower = prose_lower();
    for &cps in rates {
        let label = |name: &str| format!("script-{name}-{cps}cps");
        let takes: Vec<ScriptTake> = match text {
            Some(text) => vec![type_script(label("text"), text, cps, seed)],
            None => SCRIPTS
                .iter()
                .map(|s| type_script(label(s.name), s.text, cps, seed))
                // `shift-space` is rate-independent; it is rendered under both
                // so the reel is one grid.
                .chain([script_shift_space(label("shift-space"))])
                .chain([type_script(label("prose-lower"), &lower, cps, seed)])
                .chain([script_password(label("password"), cps, seed)])
                .collect(),
        };
        for take in &takes {
            let sc = &take.scenario;
            let click = click_track(
                &sc.cues,
                (sc.seconds * SR as f32) as usize + PRE_ROLL_FRAMES,
            );
            for volume in [0.4f32, 1.0] {
                let r = render(sc, voice, style, volume, fix, bed, seed, timbre);
                let path = out.join(format!("{tag}-{}-v{volume:.1}.wav", sc.name));
                std::fs::File::create(&path)
                    .and_then(|mut f| f.write_all(&wav_bytes(&r.mono, &click)))
                    .expect("script wav");
                let from = (sc.window.0 * SR as f32) as usize;
                let to = ((sc.window.1 * SR as f32) as usize).min(r.mono.len());
                let seg = &r.mono[from..to];
                let peak = seg.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                let rms_v = rms(seg);
                let bands = spectrum_bands(&r.mono, from, to);
                // The crest: the loudest sample inside any ting + word-head
                // capital window, on the shipping bus.
                let crest = take
                    .crest_windows
                    .iter()
                    .map(|&(a, b)| {
                        let a = ((a * SR as f32) as usize).min(r.mono.len());
                        // One block on: J1 lands every onset a block late.
                        let b = ((b * SR as f32) as usize + BLOCK).min(r.mono.len());
                        r.mono[a..b].iter().fold(0.0f32, |m, v| m.max(v.abs()))
                    })
                    .fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.max(v))));
                let degrees: Vec<String> = r.degrees.iter().map(i8::to_string).collect();
                tsv.push_str(&format!(
                    "{}\t{cps}\t{volume:.1}\t{}\t{:.2}\t{:.2}\t{:.0}\t{:.4}\t{:.4}\t{:.2}\t{}\t{}\t{}\t{}\n",
                    sc.name,
                    sc.cues.len(),
                    db(f64::from(peak)),
                    db(rms_v),
                    bands.centroid_hz,
                    bands.hi2,
                    bands.hi6,
                    band_db(rms_v, bands.presence),
                    r.max_voices,
                    r.steals,
                    crest.map_or_else(|| "-".to_string(), |c| format!("{:.2}", db(f64::from(c)))),
                    degrees.join(" "),
                ));
                println!("wrote {}", path.display());
            }
        }
    }
    let path = out.join(format!("{tag}-scripts.tsv"));
    std::fs::write(&path, &tsv).expect("scripts tsv");
    println!("wrote {}\n", path.display());
    print!("{tsv}");
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
    /// The degree the line sounded on every typed key (`MelodyV2::walk`). A
    /// siren is a property of the sequence that no spectral column can refute;
    /// [`siren_verdict`] judges it.
    degrees: Vec<i8>,
}

/// Where a cue's voice is allowed to start, relative to the key. The host
/// renders whole 512-frame blocks, so a cue applied at the next block boundary
/// starts 0–10.7 ms late, a jitter uncorrelated with the melody's clock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BlockFix {
    /// The block clock with no lead (`--jitterfix ship`): the cue is applied at
    /// the first block boundary at or after its press, 0–10.7 ms late, a
    /// different amount every key.
    Ship,
    /// Zero added latency: the onset lands on the key's own sample. The bench
    /// splits the render at the cue's frame, the best case; a host j0 would
    /// also truncate up to 10.7 ms off the mallet's head.
    J0,
    /// What ships (the host's `EventMeta::block_lead_s`): a constant +10.7 ms
    /// with the mallet intact, every onset exactly one block late, zero jitter.
    J1,
}

impl BlockFix {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "j0" => Some(Self::J0),
            "j1" => Some(Self::J1),
            "ship" | "shipped" => Some(Self::Ship),
            _ => None,
        }
    }

    /// The frame this cue's voice is spawned at.
    fn spawn_frame(self, t: f32) -> usize {
        let exact = (f64::from(t) * f64::from(SR)).max(0.0) as usize;
        match self {
            // Round up to the block boundary: the block containing the cue is
            // already being filled when the cue arrives.
            Self::Ship => exact.div_ceil(BLOCK) * BLOCK,
            Self::J0 => exact,
            Self::J1 => exact + BLOCK,
        }
    }
}

/// Whether this take is a siren, computed on the sequence it played.
///
/// A siren climbs or falls without turning, walks out of its register, or
/// sits on one note. Each has a guarantee in `rainbow_kitty_v2.rs`
/// (reflection at `TUNE_DEG_LO/HI`, `MELODY_RUN_MAX` inverting the fourth
/// identical stride, gravity toward `MELODY_CENTRE_DEG`); this reads them back.
///
/// Returns the longest run of identical strides, how many of the nine degrees
/// the line visited, the largest share any one degree took (%), and how many
/// degrees fell outside the register.
fn siren_verdict(degrees: &[i8]) -> (usize, usize, f64, usize) {
    // The tune register (`rainbow_kitty_v2::TUNE_DEG_LO..=TUNE_DEG_HI`), not
    // importable from an example; the `out` count catches a change to it.
    const DEG_LO: i8 = 0;
    const DEG_HI: i8 = 8;
    let out = degrees
        .iter()
        .filter(|d| !(DEG_LO..=DEG_HI).contains(d))
        .count();
    let (mut run, mut worst) = (1usize, 1usize);
    for w in degrees.windows(3) {
        if w[1] - w[0] == w[2] - w[1] && w[1] != w[0] {
            run += 1;
        } else {
            run = 1;
        }
        worst = worst.max(run);
    }
    let mut hist = [0usize; (DEG_HI + 1) as usize];
    for d in degrees {
        if (DEG_LO..=DEG_HI).contains(d) {
            hist[*d as usize] += 1;
        }
    }
    let visited = hist.iter().filter(|n| **n > 0).count();
    let top = hist.iter().max().copied().unwrap_or(0);
    let share = if degrees.is_empty() {
        0.0
    } else {
        100.0 * top as f64 / degrees.len() as f64
    };
    (worst, visited, share, out)
}

#[allow(clippy::too_many_arguments)]
fn render(
    sc: &Scenario,
    voice: SoundVoice,
    style: GlowStyle,
    volume: f32,
    fix: BlockFix,
    bed: bool,
    seed: u32,
    timbre: Timbre,
) -> Rendered {
    let frames = (sc.seconds * SR as f32) as usize;
    let mut synth = TrailSynth::new(SR as f32, seed);
    synth.set_v2_timbre_stops(timbre.stops());
    let mut mono = vec![0.0f32; frames];
    let mut stereo = vec![0.0f32; BLOCK * CHANNELS];
    let spawn: Vec<usize> = sc.cues.iter().map(|c| fix.spawn_frame(c.t)).collect();
    let mut cue_i = 0usize;
    let mut f = 0usize;
    let mut max_voices = 0usize;
    let mut degrees: Vec<i8> = Vec::with_capacity(sc.cues.len());
    let t0 = Instant::now();
    while f < frames {
        while cue_i < sc.cues.len() && spawn[cue_i] <= f {
            let cue = sc.cues[cue_i];
            // `push_meta`, not `push`: the contour is derived from this stamp.
            synth.push_meta(
                SoundEvent {
                    style,
                    voice,
                    kind: cue.gesture,
                    pan: cue.pan,
                    heat: cue.heat,
                    hue: (cue.t * 0.18).fract(),
                    gain: volume,
                    tone: Tone::Technical,
                    bed,
                    shifted: cue.shifted,
                },
                cue.meta(),
            );
            if cue.gesture == SoundGesture::Trail(SoundKind::Typed) {
                degrees.push(synth.melody_v2().walk());
            }
            cue_i += 1;
        }
        // Render to the next event or 512-frame boundary. Under `BlockFix::Ship`
        // every spawn frame is a boundary; only J0 and J1 split a block.
        let next_block = (f / BLOCK + 1) * BLOCK;
        let next_cue = spawn.get(cue_i).copied().unwrap_or(usize::MAX);
        let to = frames.min(next_block).min(next_cue);
        let n = to.saturating_sub(f).max(1).min(frames - f);
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
        degrees,
    }
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

/// Invert the synth's output saturator so the reported peak is what the mix
/// asked for. `soft_clip(x) = x·(27+x²)/(27+9x²)`, clamped to ±0.98, is
/// strictly increasing where unclamped, so Newton's method converges.
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

// -- FFT (radix-2 DIT, hand-rolled) -------------------------------------------

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

/// Tonality of one window: the loudest bin in 150..4500 Hz over the median bin
/// in that band. A struck tone spikes over a quiet floor (tens); band-passed
/// noise is a broad hump (near unity).
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

/// A window is a pitched note when its spectrum spikes this far over its band
/// median: between a keystroke (hundreds) and the erase poof (low tens).
const PITCHED_TONALITY: f64 = 60.0;

/// The scene table's onset detector: a rise of 1.35× over the decay it lands
/// on, with a 35 ms refractory (under the fastest script's 50 ms spacing).
const SCENE_ONSET: (usize, f32) = (35, 1.35);

/// The fine onset census, for the gesture-probe rows only: a 10 ms refractory
/// and a 1.12× rise, fine enough to see a second strike or an echo tens of ms
/// behind a key.
///
/// A capital (Shift's ting, then the letter 60 ms on) is judged on the scene
/// census instead: the ting rings through the strike, and two lattice partials a
/// consonant interval apart ripple the summed envelope at their difference
/// frequency, which this detector's 2.67 ms window counts as extra rises (the
/// unit pins are `a_capital_is_one_strike_with_a_lifted_degree_and_one_ring` and
/// `a_bare_shift_is_a_ting_that_never_steps`). It is also blind under the bass:
/// the window rides the waveform of the bass dyad under a Space or a Kill
/// (65-130 Hz), so it is taken only on rows [`Probe::census`] marks.
const FINE_ONSET: (usize, f32) = (10, 1.12);

/// Onset frames by rise detection, not threshold crossing: at 10 cps the
/// envelope never returns to a floor between notes, so only the derivative
/// separates them. A 2.7 ms RMS window smooths the partial beat; the caller
/// names the (refractory ms, rise ratio) pair: [`SCENE_ONSET`] or
/// [`FINE_ONSET`].
fn onsets(x: &[f32], from: usize, to: usize, (refractory_ms, rise): (usize, f32)) -> Vec<usize> {
    const W: usize = 128;
    let refractory: usize = SR as usize * refractory_ms / 1000;
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
        // The first audible window is an onset; after that a note announces
        // itself by a rise over the decay it lands on.
        if !(i == 0 || env[i] > env[i - 1] * rise) {
            continue;
        }
        let at = from + i * W;
        if out.last().is_none_or(|&p| at >= p + refractory) {
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

/// What [`spectrum_bands`] reads off a span: the centroid and three energy
/// shares, each a fraction of the span's total spectral energy.
struct Bands {
    centroid_hz: f64,
    /// Over 2 kHz.
    hi2: f64,
    /// Energy share over 6 kHz, the "harsh" band.
    hi6: f64,
    /// 2.8–5.6 kHz, the ear's most sensitive octave.
    presence: f64,
}

/// The 2.8–5.6 kHz presence octave.
const PRESENCE_BAND_HZ: (f64, f64) = (2800.0, 5600.0);

/// Spectral centroid and the band shares, over `from..to`.
fn spectrum_bands(x: &[f32], from: usize, to: usize) -> Bands {
    let hz_per_bin = f64::from(SR) / FFT_N as f64;
    let (mut num, mut den, mut hi) = (0.0f64, 0.0f64, 0.0f64);
    let (mut hi6, mut presence) = (0.0f64, 0.0f64);
    let mut s = from;
    while s + FFT_N <= to {
        for (k, &m) in mag_at(x, s).iter().enumerate() {
            let e = f64::from(m) * f64::from(m);
            let hz = k as f64 * hz_per_bin;
            // Not `e * hz`: this order keeps the centroid column comparable, bit
            // for bit, with earlier takes.
            num += e * k as f64 * hz_per_bin;
            den += e;
            if hz > 2000.0 {
                hi += e;
            }
            if hz > 6000.0 {
                hi6 += e;
            }
            if (PRESENCE_BAND_HZ.0..=PRESENCE_BAND_HZ.1).contains(&hz) {
                presence += e;
            }
        }
        s += FFT_N / 2;
    }
    if den < 1e-15 {
        Bands {
            centroid_hz: 0.0,
            hi2: 0.0,
            hi6: 0.0,
            presence: 0.0,
        }
    } else {
        Bands {
            centroid_hz: num / den,
            hi2: hi / den,
            hi6: hi6 / den,
            presence: presence / den,
        }
    }
}

/// Spectral centroid + fraction of energy over 2 kHz, over `from..to`.
fn spectrum(x: &[f32], from: usize, to: usize) -> (f64, f64) {
    let b = spectrum_bands(x, from, to);
    (b.centroid_hz, b.hi2)
}

/// A band's own level in dB: the span's RMS scaled by the band's energy share.
/// Absolute, so a before and an after reading compare.
fn band_db(rms_v: f64, share: f64) -> f64 {
    db(rms_v * share.max(0.0).sqrt())
}

struct Row {
    name: String,
    volume: f32,
    onsets_hz: f64,
    pitched_hz: f64,
    /// Pitched onsets per second whose note differs from the previous one by
    /// more than a semitone: how often the melody moves (`pitched_hz` counts
    /// accompaniment too).
    melody_hz: f64,
    /// The share of pitched onsets repeating the previous note
    /// (accompaniment).
    repeat_pct: f64,
    pre_peak_db: f64,
    peak_db: f64,
    rms_db: f64,
    crest_db: f64,
    centroid_hz: f64,
    /// The note range the melody used, in semitones between its lowest and
    /// highest pitched onset.
    range_st: f64,
    note_lo_hz: f64,
    note_hi_hz: f64,
    max_voices: usize,
    steals: u32,
    rt_factor: f64,
}

fn analyze(name: &str, volume: f32, r: &Rendered, window: (f32, f32)) -> Row {
    let from = (window.0 * SR as f32) as usize;
    let to = ((window.1 * SR as f32) as usize).min(r.mono.len());
    let seg = &r.mono[from..to];
    let ons = onsets(&r.mono, from, to, SCENE_ONSET);
    // Pitch each onset once, `SETTLE` past it so the ~12 ms contour bend has
    // landed: tonality says whether it is a note, the peak partial which note.
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
    // The melodic span: the extremes of the pitched onsets (`notes` is already
    // tonality-gated, so percussive outliers cannot enter).
    let (lo, hi) = notes
        .iter()
        .filter(|&&f| f > 0.0)
        .fold((f64::MAX, 0.0f64), |(a, b), &f| (a.min(f), b.max(f)));
    let (lo, hi) = if lo > hi { (0.0, 0.0) } else { (lo, hi) };
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
        range_st: if lo > 0.0 && hi > lo {
            12.0 * (hi / lo).log2()
        } else {
            0.0
        },
        note_lo_hz: lo,
        note_hi_hz: hi,
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
    /// Onsets in the gesture's own body by the scene detector ([`SCENE_ONSET`]),
    /// over the difference signal taken before the bus limiter (see [`probe`]).
    onsets: usize,
    /// The same body counted by [`FINE_ONSET`], on the rows it is valid for. A
    /// capital must read one strike.
    fine: Option<usize>,
    /// The [`EventMeta::glyph_class`] the row's key was stamped with.
    class: u8,
    /// Energy share over 6 kHz, and the 2.8–5.6 kHz octave's level: the
    /// "harsh" readings.
    hi6_frac: f64,
    presence_db: f64,
}

/// One gesture of the probe table.
struct Probe {
    label: &'static str,
    kind: SoundKind,
    /// [`SoundEvent::shifted`] on the cue — the `Capital` row is a `Typed`
    /// with `shifted`, the glyph `A` where `Typed` is `a`.
    shifted: bool,
    /// The live hue on the event (`Typed@cyan` is the bright end of the arc,
    /// where §3.3's bloom is loudest and the 1600 Hz ceiling has to hold).
    hue: f32,
    /// The modifier's own cue first: a bare Shift at the segment start, the
    /// gesture [`CAPITAL_SHIFT_LEAD_MS`] later, as the host mints a capital (the
    /// lift fires on the modifier's keydown).
    lift: bool,
    /// Take the [`FINE_ONSET`] census on this row — valid only where the
    /// gesture has no bass under it (see the constant).
    census: bool,
    /// Presses of the gesture in this row, [`RUN_TAP_MS`] apart (`Space×4` is
    /// a four-space indent tapped at 10/s, so the run's tail is measured beside
    /// its head); 1 elsewhere.
    run: u8,
    /// The character the key put on screen, so the row stamps the real
    /// [`EventMeta::glyph_class`] and [`EventMeta::rank`]. `None` means class 0,
    /// rank 0 ("a letter, no key").
    ch: Option<char>,
    /// Keys typed into the settled state before the gesture, in both takes, so
    /// they cancel in the difference and only set context: `"A"` makes the row
    /// the interior of a shifted run, `" "` a word head. A letter here takes
    /// one settling keystroke's place so the row still lands on the accent.
    prefix: &'static str,
}

/// Tap interval of a multi-press probe row: 100 ms, a hand tapping an indent
/// (the indent step's gate is fitted to 8-12 taps/s).
const RUN_TAP_MS: usize = 100;

/// How long a hand holds Shift before the letter lands (the lift arrives
/// 30-100 ms ahead of the capital).
const CAPITAL_SHIFT_LEAD_MS: usize = 60;

/// The settling keystrokes before every probe row (see [`probe`]): three,
/// so the row lands on the bar's accent.
const PROBE_SETTLE: usize = 3;

/// The walk reference for the ting's verdict. The `Typed` row is one degree of
/// the melody, and a letter's level varies by several dB across palettes and
/// degrees while the ting's does not. So the reference letter is the mean
/// `Typed` peak over these settle counts: the row's own accent and the next
/// three accents of the walk (as the engine's
/// `a_capital_is_louder_at_every_degree_of_the_walk` fixture uses).
const PROBE_WALK_SETTLES: [usize; 4] = [PROBE_SETTLE, 6, 9, 12];

/// One gesture's takes: its row, and the four monos the row was scored on.
struct Take {
    row: ProbeRow,
    /// The shipping bus, `with` the gesture and `without` it: the reel, and the
    /// level and spectrum columns.
    bus_with: Vec<f32>,
    bus_alone: Vec<f32>,
    /// The same two takes with §9.7's bus limiter out of circuit: the onset
    /// census's domain (see [`probe`]).
    pre_with: Vec<f32>,
    pre_alone: Vec<f32>,
}

/// One gesture alone from one settled melody state, scored on `with − without`:
/// the settled synth rendered with and without the gesture, sharing seed and
/// settling keys, so everything the gesture did not cause (a settling key's
/// bloom tail) cancels and a sound it damped shows as the negative of what it
/// cut. The reel keeps the raw `with` render.
///
/// The onset census is taken with the bus limiter out
/// ([`TrailSynth::set_bus_limiter`]): the difference isolates the gesture only
/// through a linear bus, and the limiter's gain moves a strike's envelope by
/// about a decibel, which the fine census would count as a strike. The reel and
/// the level and spectrum columns use the shipping bus. `soft_clip` stays in on
/// both (memoryless and compressive, it can only lower a rise ratio).
fn probe(
    p: &Probe,
    voice: SoundVoice,
    style: GlowStyle,
    volume: f32,
    seed: u32,
    timbre: Timbre,
    settle: usize,
) -> Take {
    let ev = |kind, shifted| SoundEvent {
        style,
        voice,
        kind: SoundGesture::Trail(kind),
        pan: 0.0,
        heat: 0.5,
        hue: p.hue,
        gain: volume,
        tone: Tone::Technical,
        bed: false,
        shifted,
    };
    // The class and the rank through the engine's own producers, as
    // [`Cue::meta`] stamps them; `None` stamps class 0, rank 0.
    let stamp = |frame: usize, ch: Option<char>| EventMeta {
        at_ms: ((frame as f64 * 1000.0 / f64::from(SR)) as u32).max(1),
        glyph_class: aterm_effects::trail_sound::typed_glyph_class(ch),
        rank: aterm_effects::trail_sound::typed_glyph_rank(ch),
        pan_from: 0.0,
        block_lead_s: 0.0,
        flow: 0.0,
    };
    // Three settling keystrokes ([`PROBE_SETTLE`]), ~340 ms apart: the previous
    // tail is dead, and the probe lands on an accent (every third keystroke), so
    // it is measured against the tune, not the accompaniment. Callers vary
    // `settle` in multiples of three ([`PROBE_WALK_SETTLES`]). Each is stamped,
    // or the settling keys collapse onto the block clock. A letter in the prefix
    // takes one settling keystroke's place; a Space is not a keystroke of the bar.
    let settle = settle.saturating_sub(p.prefix.chars().filter(|c| *c != ' ').count());
    let settled = |limiter: bool| {
        let mut synth = TrailSynth::new(SR as f32, seed);
        synth.set_v2_timbre_stops(timbre.stops());
        synth.set_bus_limiter(limiter);
        let mut warm = vec![0.0f32; 16_384 * CHANNELS];
        for i in 0..settle {
            synth.push_meta(ev(SoundKind::Typed, false), stamp(i * 16_384, None));
            synth.render(&mut warm);
        }
        // The context keys ([`Probe::prefix`]), one settle interval apart.
        for (i, ch) in p.prefix.chars().enumerate() {
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            synth.push_meta(
                ev(kind, needs_shift(ch)),
                stamp((settle + i) * 16_384, Some(ch)),
            );
            synth.render(&mut warm);
        }
        synth
    };
    let frames = SR as usize / 2;
    let to_mono = |stereo: &[f32]| -> Vec<f32> {
        (0..frames)
            .map(|i| 0.5 * (stereo[i * 2] + stereo[i * 2 + 1]))
            .collect()
    };

    // The pair of takes, on the bus as given.
    let takes = |limiter: bool| -> (Vec<f32>, Vec<f32>, usize) {
        // With the gesture. A lift is its own cue at the segment's first frame;
        // the gesture lands CAPITAL_SHIFT_LEAD_MS later, stamped accordingly.
        let mut synth = settled(limiter);
        let mut stereo = vec![0.0f32; frames * CHANNELS];
        let t0 = (settle + p.prefix.chars().count()) * 16_384;
        let mut at = 0usize;
        if p.lift {
            synth.push_meta(ev(SoundKind::Shift, false), stamp(t0, None));
            at = SR as usize * CAPITAL_SHIFT_LEAD_MS / 1000;
            synth.render(&mut stereo[..at * CHANNELS]);
        }
        synth.push_meta(ev(p.kind, p.shifted), stamp(t0 + at, p.ch));
        let voices = synth.live_voices();
        // A multi-press row: later presses land RUN_TAP_MS apart, each stamped
        // at its own time with audio rendered between them.
        let mut cursor = at;
        for _ in 1..p.run.max(1) {
            let next = cursor + SR as usize * RUN_TAP_MS / 1000;
            synth.render(&mut stereo[cursor * CHANNELS..next * CHANNELS]);
            synth.push_meta(ev(p.kind, p.shifted), stamp(t0 + next, p.ch));
            cursor = next;
        }
        synth.render(&mut stereo[cursor * CHANNELS..]);
        let raw = to_mono(&stereo);

        // Without it: the same settled synth left to ring.
        let mut quiet = settled(limiter);
        let mut alone = vec![0.0f32; frames * CHANNELS];
        quiet.render(&mut alone);
        (raw, to_mono(&alone), voices)
    };
    let difference =
        |a: &[f32], b: &[f32]| -> Vec<f32> { a.iter().zip(b).map(|(a, b)| a - b).collect() };
    let (bus_with, bus_alone, voices) = takes(true);
    let (pre_with, pre_alone, _) = takes(false);
    let mono = difference(&bus_with, &bus_alone);
    let body = difference(&pre_with, &pre_alone);

    let peak = mono.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    // Score the gesture's own body: onset to 250 ms past it.
    let start = mono.iter().position(|v| v.abs() > peak * 0.05).unwrap_or(0);
    let end = (start + SR as usize / 4).min(mono.len());
    let bands = spectrum_bands(&mono, start, end);
    let (centroid, hi) = (bands.centroid_hz, bands.hi2);
    let body_rms = rms(&mono[start..end]);
    // Onset census over the whole half second (before the bus limiter): any
    // second sound behind the key counts against the gesture.
    let count = |law| onsets(&body, 0, body.len(), law).len();
    let row = ProbeRow {
        name: p.label.to_string(),
        peak_db: db(f64::from(peak)),
        rms_db: db(body_rms),
        centroid_hz: centroid,
        hi_frac: hi,
        tonality: tonality(&mag_at(&mono, start)),
        voices,
        onsets: count(SCENE_ONSET),
        fine: p.census.then(|| count(FINE_ONSET)),
        class: aterm_effects::trail_sound::typed_glyph_class(p.ch),
        hi6_frac: bands.hi6,
        presence_db: band_db(body_rms, bands.presence),
    };
    Take {
        row,
        bus_with,
        bus_alone,
        pre_with,
        pre_alone,
    }
}

/// The gesture probes back to back with 400 ms of air between them: a
/// keystroke, a downbeat and an erase heard against each other.
fn probe_reel(
    voice: SoundVoice,
    style: GlowStyle,
    volume: f32,
    seed: u32,
    timbre: Timbre,
) -> (Vec<Take>, Vec<f32>, Vec<Cue>) {
    let gap = vec![0.0f32; SR as usize * 2 / 5];
    let mut reel = Vec::new();
    let mut takes = Vec::new();
    // Each gesture fires on its segment's first frame (a lift row: the Shift
    // there, the gesture CAPITAL_SHIFT_LEAD_MS on), so the click track is exact.
    let mut marks = Vec::new();
    for p in &PROBES {
        let t = reel.len() as f32 / SR as f32;
        let mark = |t: f32, gesture: SoundGesture, shifted: bool| Cue {
            t,
            gesture,
            pan: 0.0,
            heat: 0.5,
            shifted,
            ch: '\0',
        };
        let mut at = t;
        if p.lift {
            marks.push(mark(t, SoundGesture::Trail(SoundKind::Shift), false));
            at += CAPITAL_SHIFT_LEAD_MS as f32 / 1000.0;
        }
        marks.push(mark(at, SoundGesture::Trail(p.kind), p.shifted));
        let take = probe(p, voice, style, volume, seed, timbre, PROBE_SETTLE);
        reel.extend_from_slice(&take.bus_with);
        reel.extend_from_slice(&gap);
        takes.push(take);
    }
    (takes, reel, marks)
}

/// The gestures the probe table and reel cover, in reel order. `Capital` is
/// the host's whole gesture: the bare Shift's cue, then the shifted `Typed`
/// 60 ms on.
const PROBES: [Probe; 28] = [
    Probe {
        label: "Typed",
        kind: SoundKind::Typed,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: true,
        run: 1,
        ch: None,
        prefix: "",
    },
    Probe {
        label: "Typed@cyan",
        kind: SoundKind::Typed,
        shifted: false,
        hue: 0.5,
        lift: false,
        census: true,
        run: 1,
        ch: None,
        prefix: "",
    },
    Probe {
        label: "Capital",
        kind: SoundKind::Typed,
        shifted: true,
        hue: 0.0,
        lift: true,
        census: true,
        run: 1,
        ch: None,
        prefix: "",
    },
    Probe {
        label: "Space",
        kind: SoundKind::Space,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: None,
        prefix: "",
    },
    Probe {
        label: "Space×4",
        kind: SoundKind::Space,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
        run: 4,
        ch: None,
        prefix: "",
    },
    Probe {
        label: "Backspace",
        kind: SoundKind::Backspace,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: None,
        prefix: "",
    },
    Probe {
        label: "KillWord",
        kind: SoundKind::KillWord,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: None,
        prefix: "",
    },
    Probe {
        label: "Shift",
        kind: SoundKind::Shift,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: true,
        run: 1,
        ch: None,
        prefix: "",
    },
    Probe {
        label: "Kill",
        kind: SoundKind::Kill,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: None,
        prefix: "",
    },
    Probe {
        label: "Land",
        kind: SoundKind::Land,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: None,
        prefix: "",
    },
    Probe {
        label: "Jump",
        kind: SoundKind::Jump,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: None,
        prefix: "",
    },
    // Character-carrying rows: digits, marks and capitals in known contexts.
    // A mark's row is its own voice (`shifted` as a US layout needs, no lift).
    // The three capitals are the three contexts one can land in: a word head
    // (ting, then `B` after a Space), mid-word (after a letter), and the
    // interior of a shifted run (`B` after a shifted `A`, no new Shift).
    Probe {
        label: "Digit1",
        kind: SoundKind::Typed,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: Some('1'),
        prefix: "",
    },
    Probe {
        label: "Digit5",
        kind: SoundKind::Typed,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: Some('5'),
        prefix: "",
    },
    Probe {
        label: "Digit9",
        kind: SoundKind::Typed,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: Some('9'),
        prefix: "",
    },
    Probe {
        label: "Comma",
        kind: SoundKind::Typed,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: Some(','),
        prefix: "",
    },
    Probe {
        label: "Period",
        kind: SoundKind::Typed,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: Some('.'),
        prefix: "",
    },
    Probe {
        label: "Qmark",
        kind: SoundKind::Typed,
        shifted: true,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: Some('?'),
        prefix: "",
    },
    Probe {
        label: "Bang",
        kind: SoundKind::Typed,
        shifted: true,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: Some('!'),
        prefix: "",
    },
    Probe {
        label: "Semi",
        kind: SoundKind::Typed,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: Some(';'),
        prefix: "",
    },
    Probe {
        label: "Colon",
        kind: SoundKind::Typed,
        shifted: true,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: Some(':'),
        prefix: "",
    },
    Probe {
        label: "Dash",
        kind: SoundKind::Typed,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: Some('-'),
        prefix: "",
    },
    Probe {
        label: "Open",
        kind: SoundKind::Typed,
        shifted: true,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: Some('('),
        prefix: "",
    },
    Probe {
        label: "Close",
        kind: SoundKind::Typed,
        shifted: true,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: Some(')'),
        prefix: "",
    },
    Probe {
        label: "Sigil",
        kind: SoundKind::Typed,
        shifted: true,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: Some('$'),
        prefix: "",
    },
    Probe {
        label: "Math",
        kind: SoundKind::Typed,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: Some('='),
        prefix: "",
    },
    Probe {
        label: "Capital-head",
        kind: SoundKind::Typed,
        shifted: true,
        hue: 0.0,
        lift: true,
        census: false,
        run: 1,
        ch: Some('B'),
        prefix: " ",
    },
    Probe {
        label: "Capital-midword",
        kind: SoundKind::Typed,
        shifted: true,
        hue: 0.0,
        lift: true,
        census: false,
        run: 1,
        ch: Some('B'),
        prefix: "",
    },
    Probe {
        label: "Capital-run-interior",
        kind: SoundKind::Typed,
        shifted: true,
        hue: 0.0,
        lift: false,
        census: false,
        run: 1,
        ch: Some('B'),
        prefix: "A",
    },
];

// ---------------------------------------------------------------------------
// The rotation table: does a repeated gesture repeat its note?
// ---------------------------------------------------------------------------
//
// The scene table mixes every pitched onset, so a drone under a moving melody
// reads as moving, and the gesture probe fires each gesture once. So this
// follows one gesture across a take, pitched at the script's own cue times: a
// gesture swallowed by the min-gap reads as its neighbour's tail and is
// dropped by the prominence test.

/// Two pitches are the same note within this many cents: a quarter tone,
/// wider than the FFT's resolution here and far narrower than the lattice's
/// smallest step (~2 semitones).
const SAME_NOTE_CENTS: f64 = 50.0;

struct Rotation {
    name: String,
    hz: Vec<f64>,
    /// Distinct notes sounded, at [`SAME_NOTE_CENTS`] resolution.
    distinct: usize,
    /// The closest recurrence: the fewest events between two soundings of one
    /// note, `None` when no note recurred. "Rotates" means this is large.
    min_repeat: Option<usize>,
}

/// Pitch one gesture at each of `cues` (seconds), reading the loudest partial
/// in `lo..hi`, the gesture's register (so a bass root is not read as the
/// letter over it).
fn rotation_of(name: &str, mono: &[f32], cues: &[f32], lo: f32, hi: f32) -> Rotation {
    // Past the ~12 ms contour bend, as the scene table's pitch reads are.
    const SETTLE: usize = SR as usize / 40;
    let mut hz = Vec::new();
    for &t in cues {
        let s = (t * SR as f32) as usize + SETTLE;
        if s + FFT_N >= mono.len() {
            continue;
        }
        let m = mag_at(mono, s);
        // Prominence, not tonality: the band is narrow, so the question is
        // whether this gesture spoke here or the window holds only a
        // neighbour's decay.
        let band_peak = peak_hz(&m, lo, hi);
        if band_peak <= 0.0 || tonality(&m) < 8.0 {
            continue;
        }
        hz.push(band_peak);
    }
    let cents = |a: f64, b: f64| (1200.0 * (a / b).log2()).abs();
    let mut distinct: Vec<f64> = Vec::new();
    for &f in &hz {
        if !distinct.iter().any(|&d| cents(f, d) < SAME_NOTE_CENTS) {
            distinct.push(f);
        }
    }
    let mut min_repeat = None;
    for i in 0..hz.len() {
        for j in i + 1..hz.len() {
            if cents(hz[i], hz[j]) < SAME_NOTE_CENTS {
                min_repeat = Some(min_repeat.map_or(j - i, |m: usize| m.min(j - i)));
                break;
            }
        }
    }
    Rotation {
        name: name.into(),
        hz,
        distinct: distinct.len(),
        min_repeat,
    }
}

/// Every cue time in `sc` whose gesture is `kind`.
fn cue_times(sc: &Scenario, kind: SoundKind) -> Vec<f32> {
    sc.cues
        .iter()
        .filter(|c| c.gesture == SoundGesture::Trail(kind))
        .map(|c| c.t)
        .collect()
}

// ---------------------------------------------------------------------------
// WAV
// ---------------------------------------------------------------------------

/// The click: 2 ms of 2 kHz under a raised-cosine window at −20 dBFS. Short
/// enough to be unambiguous at 48 kHz, windowed so it has no click of its own,
/// and quiet enough not to compete with the music channel.
const CLICK_HZ: f32 = 2_000.0;
const CLICK_MS: f32 = 2.0;
const CLICK_LEVEL: f32 = 0.1;

/// The right channel: one tick at every scripted key press, on the key's own
/// time, with no pre-roll (the pre-roll is what the file shows).
fn click_track(cues: &[Cue], frames: usize) -> Vec<f32> {
    let mut click = vec![0.0f32; frames];
    let n = (CLICK_MS * 0.001 * SR as f32) as usize;
    for cue in cues {
        let s0 = (f64::from(cue.t) * f64::from(SR)).max(0.0) as usize;
        for i in 0..n {
            let Some(dst) = click.get_mut(s0 + i) else {
                break;
            };
            let ph = i as f32 / n as f32;
            let w = 0.5 * (1.0 - (core::f32::consts::TAU * ph).cos());
            let x = (core::f32::consts::TAU * CLICK_HZ * i as f32 / SR as f32).sin();
            // Ticks can land on one frame twice (a key and a modifier at one
            // instant); summing is right and cannot clip at this level.
            *dst += CLICK_LEVEL * w * x;
        }
    }
    click
}

/// 32-bit float stereo, written verbatim (no normalisation, no dither), so
/// A/B sides share gain staging. Left is the synth, delayed by
/// [`PRE_ROLL_FRAMES`] (the host's steady-state queue depth); right is
/// `click` at the keys' own times.
fn wav_bytes(mono: &[f32], click: &[f32]) -> Vec<u8> {
    let frames = (mono.len() + PRE_ROLL_FRAMES).max(click.len());
    let data_len = (frames * CHANNELS * 4) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&3u16.to_le_bytes());
    w.extend_from_slice(&(CHANNELS as u16).to_le_bytes());
    w.extend_from_slice(&SR.to_le_bytes());
    w.extend_from_slice(&(SR * (CHANNELS as u32) * 4).to_le_bytes());
    w.extend_from_slice(&((CHANNELS * 4) as u16).to_le_bytes());
    w.extend_from_slice(&32u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        let l = i
            .checked_sub(PRE_ROLL_FRAMES)
            .and_then(|j| mono.get(j))
            .copied()
            .unwrap_or(0.0);
        let r = click.get(i).copied().unwrap_or(0.0);
        w.extend_from_slice(&l.to_le_bytes());
        w.extend_from_slice(&r.to_le_bytes());
    }
    w
}

/// An aligned float WAV: every channel on one clock from frame 0 (no pre-roll,
/// no click), so channels can be subtracted sample for sample.
/// `--dump-probes` writes the gesture takes this way.
fn wav_aligned(chans: &[&[f32]]) -> Vec<u8> {
    let n = chans.len();
    let frames = chans.iter().map(|c| c.len()).max().unwrap_or(0);
    let data_len = (frames * n * 4) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&3u16.to_le_bytes());
    w.extend_from_slice(&(n as u16).to_le_bytes());
    w.extend_from_slice(&SR.to_le_bytes());
    w.extend_from_slice(&(SR * (n as u32) * 4).to_le_bytes());
    w.extend_from_slice(&((n * 4) as u16).to_le_bytes());
    w.extend_from_slice(&32u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        for c in chans {
            w.extend_from_slice(&c.get(i).copied().unwrap_or(0.0).to_le_bytes());
        }
    }
    w
}

// ---------------------------------------------------------------------------
// The census: what the melody rule does to a hand, with no ear in the loop
// ---------------------------------------------------------------------------
//
// One keystroke is one melody step at every typing speed, checked offline on
// the engine's own state after every push (`MelodyV2::steps`, `last_onset_ms`,
// `walk`), never re-derived from the constants.

/// The rates swept, characters per second.
const CENSUS_CPS: [f32; 8] = [3.0, 4.0, 4.5, 5.0, 6.0, 8.0, 10.0, 14.0];

/// The jitter columns: a metronome, and a hand.
const CENSUS_JITTER: [f32; 2] = [0.0, 0.25];

struct CensusRow {
    corpus: &'static str,
    cps: f32,
    jitter: f32,
    /// Typed keys only: a space is a rest and an Enter a cadence, neither a note
    /// the melody's playhead steps on.
    keys: usize,
    /// Keys that moved `MelodyV2::steps` (the melody advanced). R1: equal to
    /// `keys` on every row, at every rate.
    steps: usize,
    /// Keys whose sounding degree differs from the previous key's: how often
    /// the melody moved.
    distinct: usize,
    /// The repeat ledger: every key that repeated is charged to a cause the
    /// design names, and the residual (`stall`) must be zero. A doubled letter
    /// asks for a repeated note, so "100 % distinct" is stated as "every key
    /// moved or repeated for a named reason".
    ///
    /// `dbl`: the previous typed key had the same alphabet rank
    /// (`Touch::ReStrike`), the only repeat the text asked for.
    dbl: usize,
    /// `head`: a word head whose chord snap landed on the degree the last word
    /// ended on (a common tone across a chord change).
    head: usize,
    /// `subj`: the line answering its subject on an interval the subject
    /// latched as a unison.
    subj: usize,
    /// `phr`: a punctuation mark phrased it. A dash is a tie and repeats; a
    /// steering mark may land on the cadence degree the line already stands
    /// on; a closing bracket may already be where its opener was.
    phr: usize,
    /// `stall`: a repeat with no named cause (gravity cancelling a stride, a
    /// fold sending a distance to zero, a reflection landing on the note it
    /// left). Must be zero on every row.
    stall: usize,
    /// Keys that made no sound: the push left `MelodyV2::last_onset_ms`
    /// unchanged, advancing the state in silence.
    silent: usize,
    /// The largest gap, in ms, between a key's scripted press time and the stamp
    /// on the onset the mixer made for it (`MelodyV2::last_onset_ms`). A stamped
    /// row must read 0 (asserted); `blockclk` shows the 512-frame block
    /// quantisation (up to 10.7 ms).
    algn_ms: u32,
    /// The wall time the typing occupied, seconds.
    span_s: f64,
    /// FNV-1a hash of the degree sequence (the walk after every typed key). The
    /// derived contour reads the inter-key gap, so the sequence tells two clocks
    /// apart where `dist%` and `silent` saturate; two runs agreeing here played
    /// the same tune.
    seq: u64,
}

impl CensusRow {
    fn distinct_pct(&self) -> f64 {
        if self.keys == 0 {
            0.0
        } else {
            100.0 * self.distinct as f64 / self.keys as f64
        }
    }

    fn notes_hz(&self) -> f64 {
        if self.span_s <= 0.0 {
            0.0
        } else {
            self.distinct as f64 / self.span_s
        }
    }
}

/// Type `text` as a metronome at `cps` with `jitter`, push it through the
/// shipping engine as a take is rendered, and count what the melody did.
#[allow(clippy::too_many_arguments)]
fn census_row(
    corpus: &'static str,
    text: &str,
    cps: f32,
    jitter: f32,
    seed: u32,
    voice: SoundVoice,
    style: GlowStyle,
    fix: BlockFix,
    bed: bool,
    stamped: bool,
    timbre: Timbre,
) -> CensusRow {
    let mut cues = Vec::new();
    let mut hand = Hand::new(cps, jitter, seed);
    let end = type_metronome(&mut cues, 0.5, &mut hand, text, 0.55);
    let frames = ((end + 1.0) * SR as f32) as usize;
    let spawn: Vec<usize> = cues.iter().map(|c| fix.spawn_frame(c.t)).collect();
    let mut synth = TrailSynth::new(SR as f32, seed);
    synth.set_v2_timbre_stops(timbre.stops());
    let mut stereo = vec![0.0f32; BLOCK * CHANNELS];
    let (mut keys, mut steps, mut distinct, mut silent) = (0usize, 0usize, 0usize, 0usize);
    // The repeat ledger, and the previous typed key's rank it is kept against.
    // `0` is "no key behind this cue", which is never a repeat.
    let (mut dbl, mut head, mut subj, mut stall) = (0usize, 0usize, 0usize, 0usize);
    let mut phr = 0usize;
    let mut prev_rank: u8 = 0;
    let mut algn_ms: u32 = 0;
    // FNV-1a, 64-bit: order-sensitive, which is all this needs.
    let mut seq: u64 = 0xcbf2_9ce4_8422_2325;
    let (mut ci, mut f) = (0usize, 0usize);
    while f < frames {
        while ci < cues.len() && spawn[ci] <= f {
            let cue = cues[ci];
            let typed = cue.gesture == SoundGesture::Trail(SoundKind::Typed);
            let m = synth.melody_v2();
            let (was_step, was_onset, was_walk) = (m.steps(), m.last_onset_ms(), m.walk());
            // Read before the push (`word_pos` moves on it): a word head iff
            // nothing was typed since the last Space or Enter.
            let was_head = m.word_pos() == 0;
            // The engine's replay predicate, read before the push: a word head
            // is never a replay.
            let was_answer = !was_head && m.motif_answering();
            // The token's steering budget, read before the push: a key that
            // takes it false → true is the steering mark.
            let was_marked = m.token_marked();
            let rank =
                aterm_effects::trail_sound::typed_glyph_rank((cue.ch != '\0').then_some(cue.ch));
            let ev = SoundEvent {
                style,
                voice,
                kind: cue.gesture,
                pan: cue.pan,
                heat: cue.heat,
                hue: (cue.t * 0.18).fract(),
                gain: 0.4,
                tone: Tone::Technical,
                bed,
                shifted: cue.shifted,
            };
            if stamped {
                synth.push_meta(ev, cue.meta());
            } else {
                // Control: bare `push` leaves `at_ms = 0`, so the melody runs
                // on the 512-frame block clock.
                synth.push(ev);
            }
            if typed {
                let m = synth.melody_v2();
                keys += 1;
                if m.steps() != was_step {
                    steps += 1;
                }
                seq ^= (m.walk() as u8) as u64;
                seq = seq.wrapping_mul(0x0000_0100_0000_01b3);
                if m.last_onset_ms() == was_onset {
                    silent += 1;
                }
                // The very first key of a take is a new note by definition:
                // there is no previous pitch for it to repeat.
                if keys == 1 || m.walk() != was_walk {
                    distinct += 1;
                } else {
                    // It repeated: charge the branch that produced the degree.
                    // An answered interval never consulted the alphabet, so it
                    // is never charged to a doubled letter. The mark's override
                    // runs last, so where it fired it produced the degree.
                    let class = aterm_effects::trail_sound::typed_glyph_class(
                        (cue.ch != '\0').then_some(cue.ch),
                    );
                    let phrased = class == aterm_effects::trail_sound::glyph_class::DASH
                        || class == aterm_effects::trail_sound::glyph_class::CLOSE
                        || (!was_marked && m.token_marked());
                    if phrased {
                        phr += 1;
                    } else if was_answer {
                        subj += 1;
                    } else if rank != 0 && rank == prev_rank {
                        dbl += 1;
                    } else if was_head {
                        head += 1;
                    } else {
                        stall += 1;
                    }
                }
                if rank != 0 {
                    prev_rank = rank;
                }
                // The onset's stamp against the finger's own clock.
                algn_ms = algn_ms.max(m.last_onset_ms().abs_diff(cue.at_ms()));
            }
            ci += 1;
        }
        let next_block = (f / BLOCK + 1) * BLOCK;
        let next_cue = spawn.get(ci).copied().unwrap_or(usize::MAX);
        let to = frames.min(next_block).min(next_cue);
        let n = to.saturating_sub(f).max(1).min(frames - f);
        synth.render(&mut stereo[..n * CHANNELS]);
        f += n;
    }
    // First press to last press plus the last key's nominal interval; for a
    // metronome that is `cues.len() / cps`, so notes/s compares to cps.
    let span_s = match (cues.first(), cues.last()) {
        (Some(a), Some(b)) => f64::from(b.t - a.t) + f64::from(1.0 / cps),
        _ => 0.0,
    };
    CensusRow {
        corpus,
        cps,
        jitter,
        keys,
        steps,
        distinct,
        dbl,
        head,
        subj,
        phr,
        stall,
        silent,
        algn_ms,
        span_s,
        seq,
    }
}

#[allow(clippy::too_many_arguments)]
/// Space-head census: types the prose scenario at 6, 8 and 10 cps, clean and
/// at ±25 % jitter, through the engine as [`render`] does, and asks for every
/// Space whether the push spawned anything. A silent head was thinned by v1's
/// keystroke governor (`MIN_GAP`) or refused by a v2 lane. Prints heads,
/// silent heads, tails, silent tails and the shortest letter→space gap per
/// row, on the engine the flags name.
fn space_census(
    seed: u32,
    voice: SoundVoice,
    style: GlowStyle,
    fix: BlockFix,
    bed: bool,
    timbre: Timbre,
) {
    println!(
        "== space-head census ({}, {:?}/{:?}) ==",
        timbre.name(),
        voice,
        style
    );
    println!(
        "{:>4} {:>6} {:>6} {:>7} {:>6} {:>7} {:>9}",
        "cps", "jitter", "heads", "silent", "tails", "silent", "min gap"
    );
    for cps in [6.0f32, 8.0, 10.0] {
        for jitter in [0.0f32, 0.25] {
            let sc = scenario_prose(cps, jitter, seed, false);
            let frames = (sc.seconds * SR as f32) as usize;
            let mut synth = TrailSynth::new(SR as f32, seed);
            synth.set_v2_timbre_stops(timbre.stops());
            let mut stereo = vec![0.0f32; BLOCK * CHANNELS];
            let spawn: Vec<usize> = sc.cues.iter().map(|c| fix.spawn_frame(c.t)).collect();
            let (mut heads, mut silent_heads, mut tails, mut silent_tails) = (0usize, 0, 0, 0);
            let mut min_gap = f32::MAX;
            let mut prev_space = false;
            let mut prev_t = f32::MIN;
            let (mut ci, mut f) = (0usize, 0usize);
            while f < frames {
                while ci < sc.cues.len() && spawn[ci] <= f {
                    let cue = sc.cues[ci];
                    let is_space = cue.gesture == SoundGesture::Trail(SoundKind::Space);
                    let before = synth.live_voices();
                    synth.push_meta(
                        SoundEvent {
                            style,
                            voice,
                            kind: cue.gesture,
                            pan: cue.pan,
                            heat: cue.heat,
                            hue: (cue.t * 0.18).fract(),
                            gain: 0.4,
                            tone: Tone::Technical,
                            bed,
                            shifted: cue.shifted,
                        },
                        cue.meta(),
                    );
                    if is_space {
                        let spoke = synth.live_voices() > before;
                        if prev_space {
                            tails += 1;
                            silent_tails += usize::from(!spoke);
                        } else {
                            heads += 1;
                            silent_heads += usize::from(!spoke);
                            min_gap = min_gap.min(cue.t - prev_t);
                        }
                    }
                    prev_space = is_space;
                    prev_t = cue.t;
                    ci += 1;
                }
                let next_block = (f / BLOCK + 1) * BLOCK;
                let next_cue = spawn.get(ci).copied().unwrap_or(usize::MAX);
                let to = frames.min(next_block).min(next_cue);
                let n = to.saturating_sub(f).max(1).min(frames - f);
                synth.render(&mut stereo[..n * CHANNELS]);
                f += n;
            }
            println!(
                "{cps:>4.0} {:>5.0}% {heads:>6} {silent_heads:>7} {tails:>6} {silent_tails:>7} {:>7.1} ms",
                jitter * 100.0,
                min_gap * 1000.0
            );
        }
    }
}

/// Shift census (see the engine's `SHIFT_MIN_GAP`): the prose scenarios with
/// every third word capitalised and a bare `Shift` `lead` ms ahead of each
/// capital, counting Shifts the engine left silent. At 10 cps with a 60 ms
/// lead the Shift lands 40 ms after the previous key, inside the keystroke
/// governor's 45 ms. The music box has no governor, so its rows read 0.
fn shift_census(
    seed: u32,
    voice: SoundVoice,
    style: GlowStyle,
    fix: BlockFix,
    bed: bool,
    timbre: Timbre,
) {
    println!(
        "== shift census ({}, {:?}/{:?}) ==",
        timbre.name(),
        voice,
        style
    );
    // `in gap`: Shifts inside the keystroke governor's [`MIN_GAP`] of the cue
    // before them (what a governor-gated Shift would lose); `silent`: what the
    // engine actually left silent.
    println!(
        "{:>4} {:>6} {:>5} {:>6} {:>6} {:>6} {:>9}",
        "cps", "jitter", "lead", "shifts", "in gap", "silent", "min gap"
    );
    for cps in [6.0f32, 8.0, 10.0] {
        for jitter in [0.0f32, 0.25] {
            for lead_ms in [30u32, 60] {
                let sc = scenario_prose(cps, jitter, seed, false);
                let mut cues = sc.cues.clone();
                let mut shifts_in = Vec::new();
                let (mut word, mut head) = (0usize, true);
                for c in cues.iter_mut() {
                    let typed = c.gesture == SoundGesture::Trail(SoundKind::Typed);
                    if typed && head {
                        if word % 3 == 0 && c.ch.is_ascii_lowercase() {
                            c.shifted = true;
                            c.ch = c.ch.to_ascii_uppercase();
                            shifts_in.push(Cue {
                                t: c.t - lead_ms as f32 / 1000.0,
                                gesture: SoundGesture::Trail(SoundKind::Shift),
                                pan: c.pan,
                                heat: c.heat,
                                shifted: false,
                                ch: '\0',
                            });
                        }
                        word += 1;
                    }
                    head = !typed;
                }
                cues.extend(shifts_in);
                cues.sort_by(|a, b| a.t.partial_cmp(&b.t).expect("finite cue times"));
                let frames = (sc.seconds * SR as f32) as usize;
                let mut synth = TrailSynth::new(SR as f32, seed);
                synth.set_v2_timbre_stops(timbre.stops());
                let mut stereo = vec![0.0f32; BLOCK * CHANNELS];
                let spawn: Vec<usize> = cues.iter().map(|c| fix.spawn_frame(c.t)).collect();
                let (mut shifts, mut in_gap, mut silent) = (0usize, 0usize, 0usize);
                let mut min_gap = f32::MAX;
                let mut prev_t = f32::MIN;
                let (mut ci, mut f) = (0usize, 0usize);
                while f < frames {
                    while ci < cues.len() && spawn[ci] <= f {
                        let cue = cues[ci];
                        let is_shift = cue.gesture == SoundGesture::Trail(SoundKind::Shift);
                        let before = synth.live_voices();
                        synth.push_meta(
                            SoundEvent {
                                style,
                                voice,
                                kind: cue.gesture,
                                pan: cue.pan,
                                heat: cue.heat,
                                hue: (cue.t * 0.18).fract(),
                                gain: 0.4,
                                tone: Tone::Technical,
                                bed,
                                shifted: cue.shifted,
                            },
                            cue.meta(),
                        );
                        if is_shift {
                            shifts += 1;
                            in_gap += usize::from(cue.t - prev_t < MIN_GAP);
                            silent += usize::from(synth.live_voices() <= before);
                            min_gap = min_gap.min(cue.t - prev_t);
                        }
                        prev_t = cue.t;
                        ci += 1;
                    }
                    let next_block = (f / BLOCK + 1) * BLOCK;
                    let next_cue = spawn.get(ci).copied().unwrap_or(usize::MAX);
                    let to = frames.min(next_block).min(next_cue);
                    let n = to.saturating_sub(f).max(1).min(frames - f);
                    synth.render(&mut stereo[..n * CHANNELS]);
                    f += n;
                }
                println!(
                    "{cps:>4.0} {:>5.0}% {lead_ms:>3} ms {shifts:>6} {in_gap:>6} {silent:>6} {:>7.1} ms",
                    jitter * 100.0,
                    min_gap * 1000.0
                );
            }
        }
    }
}

/// The census sweep, printed.
fn census(
    seed: u32,
    voice: SoundVoice,
    style: GlowStyle,
    fix: BlockFix,
    bed: bool,
    timbre: Timbre,
) {
    // Corpora:
    // - `letters`: the prose with spaces and line feeds removed, a pure key
    //   stream that isolates the melody rule.
    // - `prose`: the same text with spaces as `Space` and newlines as `Jump`;
    //   neither is a derived note, but both take time and move the harmony.
    // - `blockclk`: `letters` pushed with bare `push`, no stamp, so the melody
    //   clock is quantised to the spawn grid. A control: the derived contour reads
    //   the inter-key gap, so under `--jitterfix ship` its `seq` differs from
    //   `letters`' on every row; off Ship the spawn tracks the cue's own time and
    //   the two clocks coincide (the census says so).
    let letters: String = PROSE.chars().filter(|c| !c.is_whitespace()).collect();
    // `phrase`: the reel's sentence and line of code three times over (every
    // phrase mark, capitals, a decimal, brackets, a tie, `::` and a dotted path),
    // so the ledger is read on the keys phrasing acts on (`prose` has only `.`
    // and `,`).
    let phrase: String = [SCRIPTS[0].text, SCRIPTS[1].text].concat().repeat(3);
    let corpora: [(&'static str, &str, bool); 4] = [
        ("letters", &letters, true),
        ("prose", PROSE, true),
        ("phrase", &phrase, true),
        ("blockclk", &letters, false),
    ];

    println!(
        "== census: the melody law against a hand ==\n\
         seed {seed:#010x}, {} / {style:?}, spawn {fix:?}, bed {}\n",
        voice.name(),
        if bed { "on" } else { "off" }
    );
    println!(
        "{:<8} {:>5} {:>5} {:>6} {:>6} {:>7} {:>8} {:>7} {:>4} {:>5} {:>5} {:>4} {:>6} {:>5}  {:<16}",
        "corpus",
        "cps",
        "jit%",
        "keys",
        "steps",
        "dist%",
        "notes/s",
        "silent",
        "dbl",
        "head",
        "subj",
        "phr",
        "stall",
        "algn",
        "seq"
    );
    // The totals the verdict below is computed from.
    let (mut worst_stall, mut rows_off_r1) = (0usize, 0usize);
    let (mut worst_algn, mut worst_algn_ctl) = (0u32, 0u32);
    for (corpus, text, stamped) in corpora {
        for jitter in CENSUS_JITTER {
            for cps in CENSUS_CPS {
                let r = census_row(
                    corpus, text, cps, jitter, seed, voice, style, fix, bed, stamped, timbre,
                );
                // The ledger must sum, or a repeat has an unnamed cause.
                assert_eq!(
                    r.distinct + r.dbl + r.head + r.subj + r.phr + r.stall,
                    r.keys,
                    "the repeat ledger does not account for every key on \
                     {corpus} {cps} cps {:.0}% jitter",
                    jitter * 100.0
                );
                // A stamped row's note carries the key's own time, exactly.
                if stamped {
                    assert_eq!(
                        r.algn_ms,
                        0,
                        "a stamped row's onset does not carry the key's own \
                         time on {corpus} {cps} cps {:.0}% jitter",
                        jitter * 100.0
                    );
                }
                worst_stall = worst_stall.max(r.stall);
                if stamped {
                    worst_algn = worst_algn.max(r.algn_ms);
                } else {
                    worst_algn_ctl = worst_algn_ctl.max(r.algn_ms);
                }
                if r.steps != r.keys || r.silent != 0 {
                    rows_off_r1 += 1;
                }
                println!(
                    "{:<8} {:>5.1} {:>5.0} {:>6} {:>6} {:>7.1} {:>8.2} {:>7} {:>4} {:>5} {:>5} {:>4} {:>6} {:>5}  {:016x}",
                    r.corpus,
                    r.cps,
                    r.jitter * 100.0,
                    r.keys,
                    r.steps,
                    r.distinct_pct(),
                    r.notes_hz(),
                    r.silent,
                    r.dbl,
                    r.head,
                    r.subj,
                    r.phr,
                    r.stall,
                    r.algn_ms,
                    r.seq
                );
            }
            println!();
        }
    }
    if fix != BlockFix::Ship {
        println!(
            "NOTE: spawn is {fix:?}, so the `blockclk` control is not discriminating\n\
             here — an unstamped push reads the spawn grid, and off Ship that grid\n\
             is (near) the cue's own time. `blockclk` converging on `letters` in\n\
             THIS table is arithmetic, not an unwired stamp. Re-run with\n\
             --jitterfix ship to falsify the stamp.\n"
        );
    }
    println!(
        "R1 IS MET when `steps` EQUALS `keys` and `silent` is 0 on EVERY row, with\n\
         notes/s tracking cps. Those two columns are the ruling: one keystroke is\n\
         one melody step, at every typing speed, and no key is ever silent.\n\
         Rows off R1: {rows_off_r1}.\n\
         \n\
         §8 SAYS \"100 % DISTINCT\". §3.1 OF THE SAME DOCUMENT KEEPS `Touch::ReStrike`\n\
         for \"a stride of zero — a genuinely repeated pitch, from a doubled letter\".\n\
         Both cannot be true of one column, and the design's own §3.1 is the half\n\
         that is right: a corpus that types `ll` has ASKED for the same note twice,\n\
         and an engine that refused would be overwriting the text rather than\n\
         deriving from it. So the proof is not dist%, and it is not an excuse for\n\
         dist%: it is the LEDGER. Every key that repeated is charged to a cause the\n\
         design names — `dbl` (the text asked), `head` (a common tone held across a\n\
         chord change at a word boundary), `subj` (the line answering its own\n\
         latched subject), `phr` (a MARK phrased it: a dash's tie, a bracket's\n\
         return, a steering mark arriving on the cadence degree it stood on) —\n\
         and `stall`, the residual, is the engine standing still while the hand\n\
         moved. `distinct + dbl + head + subj + phr + stall` is asserted\n\
         equal to `keys` on every row, so nothing can hide outside it.\n\
         \n\
         THE §8 PROOF IS DISCHARGED WHEN `stall` IS 0 ON EVERY ROW. That is\n\
         \"100 % distinct\" in the only form that is at once true, checkable and\n\
         worth having. Worst `stall` across this sweep: {worst_stall}.\n\
         \n\
         `algn` IS R1's OTHER HALF, and the half an ear cannot settle: the largest\n\
         gap between a key's own press time and the stamp on the onset the mixer\n\
         made for it. Every STAMPED row is asserted to read 0 — the note carries\n\
         the finger's clock, not the audio callback's. Worst across the stamped\n\
         rows: {worst_algn} ms. `blockclk` reads the block quantisation instead —\n\
         worst {worst_algn_ctl} ms — which is what an unstamped push costs, and is\n\
         the same defect the host still carries at `app_render.rs`'s frame drain.\n\
         \n\
         `blockclk` carries no rank at all, so it can never see a doubled letter and\n\
         its dist% is trivially 100 — which is why it is a CONTROL and not a target.\n\
         A stamped row reading 100.0 would mean the text had stopped reaching the\n\
         melody.\n\
         \n\
         THE STAMP is live while `blockclk`'s seq differs from `letters`' seq on\n\
         every row under spawn Ship: the derived contour reads the inter-key gap,\n\
         so a melody clocked on the 512-frame block grid cannot play the same\n\
         tune as one clocked on the hand."
    );
}

// ---------------------------------------------------------------------------
// The timbre ladder's seam
// ---------------------------------------------------------------------------

/// Which instrument the take is rendered on, one addition per rung via
/// [`TrailSynth::set_v2_timbre_stops`] on the same engine: `plain` is the
/// derived line through the tine (the control), `bloom` adds the 3f/4f/6f
/// bloom, `hue` couples the bloom's air, pan and the note's roof to the live
/// hue, and `room` (the default, what ships) adds the Enter/rest air cloud and
/// the answering voice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Timbre {
    Plain,
    Bloom,
    Hue,
    Room,
}

impl Timbre {
    /// The engine's stops for this rung.
    fn stops(self) -> TimbreStops {
        match self {
            Self::Plain => TimbreStops::PLAIN,
            Self::Bloom => TimbreStops {
                bloom: true,
                hue: false,
                room: false,
            },
            Self::Hue => TimbreStops {
                bloom: true,
                hue: true,
                room: false,
            },
            Self::Room => TimbreStops::ALL,
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "plain" => Some(Self::Plain),
            "bloom" => Some(Self::Bloom),
            "hue" => Some(Self::Hue),
            "room" => Some(Self::Room),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Bloom => "bloom",
            Self::Hue => "hue",
            Self::Room => "room",
        }
    }
}

fn usage() -> ! {
    eprintln!(
        "keyboard_song_ab <out_dir> [--tag <name>] [--voice <name>] [--style <name>]\n\
        \x20   [--cps <rate>] [--jitter <pct>] [--seed <n>] [--bed on|off]\n\
        \x20   [--timbre plain|bloom|hue|room] [--jitterfix ship|j0|j1] [--metronome]\n\
        \x20   [--census] [--space-census] [--shift-census] [--probes] [--dump-probes]\n\
        \x20   [--scripts] [--text \"<s>\"]"
    );
    std::process::exit(2)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out = PathBuf::from("target/keyboard-song-ab");
    let mut tag = "current".to_string();
    let mut voice = SoundVoice::Style;
    let mut style = GlowStyle::RainbowKitty;
    let mut cps = 10.0f32;
    let mut jitter = 0.0f32;
    let mut seed = SEED;
    let mut bed = false;
    let mut timbre = Timbre::Room;
    let mut timbre_named = false;
    let mut want_probes = false;
    let mut dump_probes = false;
    // J1 is what ships, so it is the default: the host sets
    // `EventMeta::block_lead_s` on every `push_meta`, which puts every onset a
    // constant one block after its press. `--jitterfix ship` models the block
    // clock without that lead.
    let mut fix = BlockFix::J1;
    let mut want_census = false;
    let mut want_space_census = false;
    let mut want_shift_census = false;
    let mut metronome = false;
    let mut want_scripts = false;
    let mut script_text: Option<String> = None;
    let mut cps_named = false;
    while let Some(a) = args.next() {
        let mut val = |what: &str| {
            args.next().unwrap_or_else(|| {
                eprintln!("{what} wants a value");
                usage()
            })
        };
        match a.as_str() {
            "--tag" => tag = val("--tag"),
            "--voice" => {
                let v = val("--voice");
                voice = SoundVoice::parse(&v).unwrap_or_else(|| {
                    eprintln!("unknown voice {v:?}");
                    std::process::exit(2);
                });
            }
            "--style" => {
                let s = val("--style");
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
            // The prose take's typing rate; every other scenario keeps its own
            // scripted cadence.
            "--cps" => {
                let v = val("--cps");
                cps_named = true;
                cps = v
                    .parse::<f32>()
                    .ok()
                    .filter(|r| *r > 0.1)
                    .unwrap_or_else(|| {
                        eprintln!("--cps wants a rate over 0.1, got {v:?}");
                        std::process::exit(2);
                    });
            }
            // Per-interval wobble in percent: 25 is the ±25 % a hand carries.
            "--jitter" => {
                let v = val("--jitter");
                jitter = v
                    .parse::<f32>()
                    .ok()
                    .filter(|j| (0.0..=90.0).contains(j))
                    .map(|j| j / 100.0)
                    .unwrap_or_else(|| {
                        eprintln!("--jitter wants 0..90 percent, got {v:?}");
                        std::process::exit(2);
                    });
            }
            // Seeds the hand (the jitter) and the synth (the velocity draw), so
            // one seed is one performance end to end.
            "--seed" => {
                let v = val("--seed");
                seed = v
                    .strip_prefix("0x")
                    .map_or_else(
                        || v.parse::<u32>().ok(),
                        |h| u32::from_str_radix(h, 16).ok(),
                    )
                    .unwrap_or_else(|| {
                        eprintln!("--seed wants a u32, got {v:?}");
                        std::process::exit(2);
                    });
            }
            "--bed" => {
                let v = val("--bed");
                bed = match v.as_str() {
                    "on" | "yes" | "true" => true,
                    "off" | "no" | "false" => false,
                    _ => {
                        eprintln!("--bed wants on|off, got {v:?}");
                        std::process::exit(2);
                    }
                };
            }
            "--timbre" => {
                let v = val("--timbre");
                timbre = Timbre::parse(&v).unwrap_or_else(|| {
                    eprintln!("unknown timbre {v:?} (plain|bloom|hue|room)");
                    std::process::exit(2);
                });
                timbre_named = true;
            }
            "--jitterfix" => {
                let v = val("--jitterfix");
                fix = BlockFix::parse(&v).unwrap_or_else(|| {
                    eprintln!("unknown jitterfix {v:?} (j0|j1)");
                    std::process::exit(2);
                });
            }
            "--census" => want_census = true,
            "--space-census" => want_space_census = true,
            "--shift-census" => want_shift_census = true,
            "--probes" => want_probes = true,
            "--dump-probes" => dump_probes = true,
            "--metronome" => metronome = true,
            "--scripts" => want_scripts = true,
            // One line of the caller's own, typed by the reel's typist. A
            // literal `\n` in the argument is a Return.
            "--text" => script_text = Some(val("--text").replace("\\n", "\n")),
            "-h" | "--help" => usage(),
            other if other.starts_with("--") => {
                eprintln!("unknown flag {other:?}");
                usage()
            }
            other => out = PathBuf::from(other),
        }
    }

    if want_census {
        // No WAVs: the census reads engine state, not audio.
        census(seed, voice, style, fix, bed, timbre);
        return;
    }
    // Each census is its own flag; both may run together.
    if want_space_census || want_shift_census {
        if want_space_census {
            space_census(seed, voice, style, fix, bed, timbre);
        }
        if want_shift_census {
            shift_census(seed, voice, style, fix, bed, timbre);
        }
        return;
    }

    // The ladder's four files must not overwrite each other under one tag: a
    // rung named on the command line carries its name in the file.
    let tag = if timbre_named {
        format!("{tag}-{}", timbre.name())
    } else {
        tag
    };
    println!(
        "timbre: {} ({})",
        timbre.name(),
        match timbre {
            Timbre::Plain => "the derived line through the shipped tine — the control",
            Timbre::Bloom => "+ the 3f/4f/6f bloom",
            Timbre::Hue => "+ hue_air and the hue spread on the bloom, ROOF_HUE_ADD_HZ on the note",
            Timbre::Room =>
                "+ the Enter/rest air cloud and the answering voice — the instrument that ships",
        }
    );

    std::fs::create_dir_all(&out).expect("out dir");
    if want_probes {
        probe_tables(&out, &tag, voice, style, seed, timbre, dump_probes);
        return;
    }
    if want_scripts || script_text.is_some() {
        // The reel's two rates, unless `--cps` named one.
        let rates: &[f32] = if cps_named { &[cps] } else { &SCRIPT_CPS };
        script_reel(
            &out,
            &tag,
            script_text.as_deref(),
            rates,
            voice,
            style,
            fix,
            bed,
            seed,
            timbre,
        );
        return;
    }

    let scenarios = [
        scenario_prose(cps, jitter, seed, metronome),
        scenario_edit(),
        scenario_space(),
        scenario_shift(),
        scenario_burst(),
        scenario_rotate(),
        scenario_pathological(),
    ];
    let mut rows: Vec<Row> = Vec::new();
    let mut rots: Vec<Rotation> = Vec::new();
    // (scene, keys, longest identical-stride run, degrees visited, top
    // degree's share %, degrees outside the register)
    let mut sirens: Vec<(String, usize, usize, usize, f64, usize)> = Vec::new();
    for sc in &scenarios {
        let click = click_track(
            &sc.cues,
            (sc.seconds * SR as f32) as usize + PRE_ROLL_FRAMES,
        );
        for volume in [0.4f32, 1.0] {
            let r = render(sc, voice, style, volume, fix, bed, seed, timbre);
            let path = out.join(format!("{tag}-{}-v{volume:.1}.wav", sc.name));
            let mut f = std::fs::File::create(&path).expect("wav");
            f.write_all(&wav_bytes(&r.mono, &click)).expect("write");
            if volume == 0.4 {
                let (worst, visited, share, outside) = siren_verdict(&r.degrees);
                sirens.push((
                    sc.name.to_string(),
                    r.degrees.len(),
                    worst,
                    visited,
                    share,
                    outside,
                ));
                // The lift is read in the melody's register (450-4000 Hz), the bass
                // in its own (200-470 Hz).
                let shifts = cue_times(sc, SoundKind::Shift);
                if !shifts.is_empty() {
                    rots.push(rotation_of(
                        &format!("Shift/{}", sc.name),
                        &r.mono,
                        &shifts,
                        450.0,
                        4000.0,
                    ));
                }
                let spaces = cue_times(sc, SoundKind::Space);
                if !spaces.is_empty() {
                    rots.push(rotation_of(
                        &format!("Space/{}", sc.name),
                        &r.mono,
                        &spaces,
                        200.0,
                        470.0,
                    ));
                }
            }
            rows.push(analyze(&sc.name, volume, &r, sc.window));
            println!("wrote {}", path.display());
        }
    }

    println!(
        "\n== {tag}: {} / {style:?} — prose {cps:.2} cps ±{:.0}%{}, seed {seed:#010x}, \
         spawn {fix:?}, bed {} ==",
        voice.name(),
        jitter * 100.0,
        if metronome {
            " (metronome, no rests)"
        } else {
            ""
        },
        if bed { "on" } else { "off" }
    );
    println!(
        "stereo: LEFT the synth, delayed {PRE_ROLL_FRAMES} frames ({:.1} ms — the host's \
         queue depth); RIGHT a 2 kHz tick at each key's own time.",
        PRE_ROLL_FRAMES as f32 * 1000.0 / SR as f32
    );
    println!(
        "{:<13} {:>4} {:>7} {:>7} {:>7} {:>6} {:>9} {:>8} {:>8} {:>6} {:>8} {:>7} {:>6} {:>6} {:>5} {:>5} {:>6}",
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
        "range st",
        "lo Hz",
        "hi Hz",
        "vmax",
        "steal",
        "xRT"
    );
    for r in &rows {
        println!(
            "{:<13} {:>4.1} {:>7.2} {:>7.2} {:>7.2} {:>6.0} {:>9.2} {:>8.2} {:>8.2} {:>6.1} {:>8.0} {:>7.1} {:>6.0} {:>6.0} {:>5} {:>5} {:>6.0}",
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
            r.range_st,
            r.note_lo_hz,
            r.note_hi_hz,
            r.max_voices,
            r.steals,
            r.rt_factor
        );
    }

    println!(
        "\n== siren verdict (vol 0.4) — §8 step 3's third proof, on the SEQUENCE ==\n\
         No column in the table above can refute a siren: `range st` is wide for a\n\
         phrase and for a ramp alike, `centroid` averages over both, and an envelope\n\
         detector cannot resolve overlapping notes at prose density. A siren is a\n\
         property of the degrees, so it is settled on the degrees."
    );
    println!(
        "{:<13} {:>5} {:>8} {:>8} {:>8} {:>8}  verdict",
        "scene", "keys", "max run", "visited", "top deg%", "outside"
    );
    let mut siren_free = true;
    for (name, keys, worst, visited, share, outside) in &sirens {
        // [`siren_verdict`]'s three guarantees; under 20 keys the share and
        // visited counts say nothing.
        let thin = *keys < 20;
        let ok = *outside == 0 && *worst <= 3 && (thin || (*share < 50.0 && *visited >= 5));
        siren_free &= ok;
        println!(
            "{name:<13} {keys:>5} {worst:>8} {visited:>8} {share:>8.1} {outside:>8}  {}",
            if ok {
                "not a siren"
            } else {
                "SIREN — the line ramped, escaped or stuck"
            }
        );
    }
    println!(
        "{}",
        if siren_free {
            "every scene is a phrase, including `pathological`."
        } else {
            "AT LEAST ONE SCENE IS A SIREN — §8 step 3's third proof FAILS."
        }
    );

    println!("\n== rotation (vol 0.4) — does a repeated gesture repeat its NOTE? ==");
    println!(
        "{:<22} {:>6} {:>9} {:>11}  notes sounded (Hz)",
        "gesture/scene", "events", "distinct", "min-repeat"
    );
    for r in &rots {
        let notes: Vec<String> = r.hz.iter().take(14).map(|f| format!("{f:.0}")).collect();
        let heard = notes.join(" ");
        let more = if r.hz.len() > 14 { " …" } else { "" };
        println!(
            "{:<22} {:>6} {:>9} {:>11}  {heard}{more}",
            r.name,
            r.hz.len(),
            r.distinct,
            r.min_repeat
                .map_or_else(|| "never".into(), |m| format!("{m} events")),
        );
    }

    probe_tables(&out, &tag, voice, style, seed, timbre, dump_probes);
}

/// The gesture probe table and reel at both volumes, split out so `--probes`
/// prints the onset census and centroid without rendering the scenes.
fn probe_tables(
    out: &std::path::Path,
    tag: &str,
    voice: SoundVoice,
    style: GlowStyle,
    seed: u32,
    timbre: Timbre,
    dump: bool,
) {
    for volume in [0.4f32, 1.0] {
        println!("\n== gesture probes (isolated, vol {volume}) ==");
        // `class` is the stamped [`EventMeta::glyph_class`] (0 where no char);
        // `hi>6k` and `2.8-5.6k` are the "harsh" readings.
        println!(
            "{:<20} {:>5} {:>8} {:>8} {:>9} {:>8} {:>8} {:>9} {:>9} {:>6} {:>6} {:>5}",
            "gesture",
            "class",
            "peak dB",
            "rms dB",
            "centroid",
            "hi>2k",
            "hi>6k",
            "2.8-5.6k",
            "tonality",
            "voices",
            "onsets",
            "fine"
        );
        let (takes, reel, marks) = probe_reel(voice, style, volume, seed, timbre);
        let probes: Vec<&ProbeRow> = takes.iter().map(|t| &t.row).collect();
        for p in &probes {
            println!(
                "{:<20} {:>5} {:>8.2} {:>8.2} {:>9.0} {:>8.3} {:>8.4} {:>9.2} {:>9.1} {:>6} {:>6} {:>5}",
                p.name,
                p.class,
                p.peak_db,
                p.rms_db,
                p.centroid_hz,
                p.hi_frac,
                p.hi6_frac,
                p.presence_db,
                p.tonality,
                p.voices,
                p.onsets,
                p.fine.map_or("-".to_string(), |n| n.to_string())
            );
        }
        // A keystroke is one onset by the fine census (`Typed 1`); `Capital` is
        // two keys (the bare Shift's ting and the letter 60 ms on) and reads 2.
        // The bare Shift sings by design.
        let find = |name: &str| probes.iter().find(|p| p.name == name);
        let fine_of = |name: &str| find(name).and_then(|p| p.fine).unwrap_or(0);
        let typed = fine_of("Typed");
        // The capital is counted on the scene census (see [`FINE_ONSET`]).
        let capital = find("Capital").map_or(0, |p| p.onsets);
        let caps_lock = find("CapsLockA").and_then(|p| p.fine);
        let shift_tonality = find("Shift").map_or(0.0, |p| p.tonality);
        let shift_pitched = shift_tonality >= PITCHED_TONALITY;
        // The ting is read against the walk-mean letter
        // ([`PROBE_WALK_SETTLES`]), averaged in dB with the row's own.
        let typed_walk_db = {
            let rows: Vec<f64> = PROBE_WALK_SETTLES
                .iter()
                .map(|&settle| {
                    if settle == PROBE_SETTLE {
                        find("Typed").map_or(f64::NAN, |t| t.peak_db)
                    } else {
                        probe(&PROBES[0], voice, style, volume, seed, timbre, settle)
                            .row
                            .peak_db
                    }
                })
                .collect();
            rows.iter().sum::<f64>() / rows.len() as f64
        };
        let shift_re_typed = find("Shift").map_or(f64::NAN, |s| s.peak_db - typed_walk_db);
        // The bare Shift is a ting. On the v1 palettes it sits at the Space's
        // tier, −3 dB re the walk-mean keystroke; the music box's 200 ms bell on a
        // tone of the live chord rings 2.4× as long, so it sits 1.5 dB lower:
        // −4.5 ± 1.0 dB.
        let music_box = match voice {
            SoundVoice::RainbowKittyV2 | SoundVoice::Of(GlowStyle::RainbowKitty) => true,
            SoundVoice::Style => style == GlowStyle::RainbowKitty,
            _ => false,
        };
        #[allow(non_snake_case)]
        let (SHIFT_RE_TYPED_TARGET_DB, SHIFT_RE_TYPED_TOL_DB): (f64, f64) =
            if music_box { (-4.5, 1.0) } else { (-3.0, 1.5) };
        let ruled = if music_box {
            "2026-09-20"
        } else {
            "2026-09-16"
        };
        let shift_level_ok =
            (shift_re_typed - SHIFT_RE_TYPED_TARGET_DB).abs() <= SHIFT_RE_TYPED_TOL_DB;
        let caps_lock_ok = caps_lock.is_none_or(|n| n == 1);
        // The ting's verdict is its own line: the Typed clause ("a keystroke is
        // one onset") reads red on the v1 palettes.
        println!(
            "ting ({ruled}): Shift {shift_re_typed:+.2} dB re the walk-mean Typed \
             ({typed_walk_db:.2} dBFS over settle {PROBE_WALK_SETTLES:?}; the row's own \
             degree reads {:.2}), tonality {shift_tonality:.1} ({}) — {} the \
             {SHIFT_RE_TYPED_TARGET_DB:+.1} ± {SHIFT_RE_TYPED_TOL_DB} dB window",
            find("Typed").map_or(f64::NAN, |t| t.peak_db),
            if shift_pitched { "pitched" } else { "FELT" },
            if shift_pitched && shift_level_ok {
                "a note beside the key, inside"
            } else {
                "OFF"
            }
        );
        println!(
            "onset census (fine: {} ms, rise {}; Capital = Shift then the letter \
             {CAPITAL_SHIFT_LEAD_MS} ms on, on the scene census): Typed {typed}, Capital \
             {capital}{}, Shift tonality \
             {shift_tonality:.1} ({}) at {shift_re_typed:+.2} dB re the walk-mean Typed (target \
             {SHIFT_RE_TYPED_TARGET_DB:+.1} ± {SHIFT_RE_TYPED_TOL_DB}, the {ruled} ting) — {}",
            FINE_ONSET.0,
            FINE_ONSET.1,
            caps_lock.map_or(String::new(), |n| format!(", CapsLockA {n}")),
            if shift_pitched { "pitched" } else { "FELT" },
            if typed == 1 && capital == 2 && caps_lock_ok && shift_pitched && shift_level_ok {
                "a keystroke is one onset, a Shift+capital is two keys, and the ting \
                 rings beside the key (§8 step 4 as re-ruled 2026-09-10 and 2026-09-16 holds)"
            } else if !shift_pitched {
                "§8 STEP 4 (2026-09-10) FAILS: the bare Shift does not sing"
            } else if !shift_level_ok {
                "§8 STEP 4 (2026-09-16 / 2026-09-20) FAILS: the ting is off its window"
            } else if capital != 2 {
                "§8 STEP 4 (2026-09-10) FAILS: Shift then a capital is not two onsets"
            } else {
                "§8 STEP 4 (2026-09-10) FAILS: a keystroke is not one onset"
            }
        );
        // Forte, not higher: the `Capital-run-interior` row's `B` (inside a
        // shifted run, so the walk's accent does not fire) against its lowercase
        // twin `b` from the same settled state must read one note, 25 ms past
        // the onset.
        if let Some(interior) = PROBES.iter().find(|p| p.label == "Capital-run-interior") {
            let twin = Probe {
                label: "run-interior twin",
                kind: interior.kind,
                shifted: false,
                hue: interior.hue,
                lift: interior.lift,
                census: false,
                run: interior.run,
                ch: Some('b'),
                prefix: interior.prefix,
            };
            let struck_hz = |p: &Probe| -> f64 {
                let t = probe(p, voice, style, volume, seed, timbre, PROBE_SETTLE);
                let mono: Vec<f32> = t
                    .bus_with
                    .iter()
                    .zip(&t.bus_alone)
                    .map(|(a, b)| a - b)
                    .collect();
                let peak = mono.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                let start = mono.iter().position(|v| v.abs() > peak * 0.05).unwrap_or(0);
                let at = (start + SR as usize / 40).min(mono.len().saturating_sub(FFT_N + 1));
                peak_hz(&mag_at(&mono, at), 120.0, 6000.0)
            };
            let (cap_hz, twin_hz) = (struck_hz(interior), struck_hz(&twin));
            let cents = 1200.0 * (cap_hz / twin_hz).log2();
            println!(
                "forte (2026-09-20): a run-interior capital strikes {cap_hz:.1} Hz against its \
                 lowercase twin's {twin_hz:.1} ({cents:+.0} cents) — {}",
                if cents.abs() <= SAME_NOTE_CENTS {
                    "the line's own degree, struck harder: not higher"
                } else {
                    "NOT THE LINE'S DEGREE: a shifted key is sounding a different note"
                }
            );
        }
        // The space head must be heard: −6.5..0 dB re the walk-mean Typed (never
        // over the letter), centroid ≥ 450 Hz, and at least a fiftieth of the
        // energy over 2 kHz (a laptop speaker rolls off a bare bass). The letter is
        // the walk mean because the v1 downbeat is a fixed bass root while the
        // letter walks its palette's roof. The clauses are the engine's
        // `SPACE_HEAD_*` constants, shared with its pin
        // `the_space_head_is_heard_with_top_on_every_v1_voice`.
        let (space_re_typed_lo_db, space_re_typed_hi_db) = SPACE_HEAD_RE_TYPED_DB;
        if let (Some(sp), Some(t)) = (find("Space"), find("Typed")) {
            let re = sp.peak_db - typed_walk_db;
            let re_row = sp.peak_db - t.peak_db;
            let tier_ok = (space_re_typed_lo_db..=space_re_typed_hi_db).contains(&re);
            let top_ok =
                sp.centroid_hz >= SPACE_HEAD_CENTROID_FLOOR_HZ && sp.hi_frac >= SPACE_HEAD_HI_FLOOR;
            println!(
                "space head (2026-09-16): Space {re:+.2} dB re the walk-mean Typed \
                 ({typed_walk_db:.2} dBFS over settle {PROBE_WALK_SETTLES:?}; the row's own \
                 degree reads {re_row:+.2}), centroid {:.0} Hz, over 2 kHz {:.3} — {}",
                sp.centroid_hz,
                sp.hi_frac,
                if tier_ok && top_ok {
                    "heard beside the letter, never over it, with top for a laptop speaker"
                        .to_string()
                } else if !tier_ok {
                    format!(
                        "OFF ITS {space_re_typed_lo_db}..{space_re_typed_hi_db} dB WINDOW re \
                         the letter"
                    )
                } else {
                    format!(
                        "NO TOP: centroid under {SPACE_HEAD_CENTROID_FLOOR_HZ:.0} Hz or under \
                         {SPACE_HEAD_HI_FLOOR} of the energy over 2 kHz"
                    )
                }
            );
        }
        // The bloomed Typed probe's centroid sits in the 1250-1400 Hz window
        // and under 1600 Hz, past which the bloom becomes a glass bell.
        for name in ["Typed", "Typed@cyan"] {
            let Some(p) = probes.iter().find(|p| p.name == name) else {
                continue;
            };
            let c = p.centroid_hz;
            println!(
                "timbre probe ({}): {name} centroid {c:.0} Hz, hi>2k {:.3} — {}",
                timbre.name(),
                p.hi_frac,
                if timbre == Timbre::Plain {
                    "the tine alone (the control)"
                } else if c >= 1600.0 {
                    "OVER 1600 Hz: the bloom is a glass bell — BLOOM_LEVEL must give back"
                } else if (1250.0..1400.0).contains(&c) {
                    "inside §3.3's 1250-1400 Hz window"
                } else {
                    "outside §3.3's 1250-1400 Hz window, under the 1600 Hz ceiling"
                }
            );
        }
        if dump {
            // The takes themselves, for other tools: per gesture, `with` on
            // the left and `without` on the right, sample-aligned, once on the
            // shipping bus and once with the limiter out, so any census can be
            // taken on either domain.
            for t in &takes {
                for (domain, with, alone) in [
                    ("bus", &t.bus_with, &t.bus_alone),
                    ("pre", &t.pre_with, &t.pre_alone),
                ] {
                    let path = out.join(format!(
                        "{tag}-probe-{}-v{volume:.1}-{domain}.wav",
                        t.row.name
                    ));
                    std::fs::File::create(&path)
                        .and_then(|mut f| f.write_all(&wav_aligned(&[with, alone])))
                        .expect("probe dump");
                }
            }
            println!(
                "wrote {}/{tag}-probe-<gesture>-v{volume:.1}-{{bus,pre}}.wav — each take, \
                 L = with the gesture, R = without, aligned; `bus` is the shipping render, \
                 `pre` the same with the bus limiter out (the onset census's domain)",
                out.display()
            );
        }
        let path = out.join(format!("{tag}-gestures-v{volume:.1}.wav"));
        let click = click_track(&marks, reel.len() + PRE_ROLL_FRAMES);
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(&wav_bytes(&reel, &click)))
            .expect("gesture reel");
        println!(
            "\nwrote {} — the reel, in order: {}",
            path.display(),
            PROBES
                .iter()
                .map(|p| p.label.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}
