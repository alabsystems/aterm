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
//!   targo --unverified run --release -p aterm-effects \
//!       --example keyboard_song_ab -- <out_dir> \
//!       [--tag <name>] [--voice <name>] [--style <name>]
//!       [--cps <rate>] [--jitter <pct>] [--seed <n>] [--bed on|off]
//!       [--timbre plain|bloom|hue|room] [--jitterfix ship|j0|j1] [--metronome]
//!       [--census] [--probes]
//!
//! `--metronome` types the prose scene as a STRICT metronome — no sentence
//! rest, no line-ending beat, no paragraph think — the way the census types
//! it, so a WAV pair at two rates is the census's own tempo-invariance claim
//! made audible (design §8's first open question). The default keeps the
//! rests, because a person takes them.
//!
//! # THE STAMP (2026-09-08) — why every WAV before this date was evidence of
//! # nothing
//!
//! This bench used to call bare [`TrailSynth::push`]. `EventMeta::default()`
//! leaves `at_ms = 0`, and rainbow kitty v2's `v2_at_ms` reads `0` as "no host
//! stamp" and falls back to the synth's own 512-frame block clock. So the
//! melody's own clock — under the shipped engine a 220 ms step gate, and under
//! the derived line the inter-key interval the whole contour is read from —
//! HAD NEVER BEEN EXERCISED BY THIS BENCH at the resolution a hand types at:
//! every cue drained into one block carried one identical timestamp. It now
//! calls
//! [`TrailSynth::push_meta`] with the cue's own scripted press time, so the
//! clock under test is the shipping clock.
//!
//! # THE CLICK TRACK — an ear cannot judge alignment from a mono file
//!
//! Every WAV is now STEREO: **left is the synth, right is a 2 ms 2 kHz tick at
//! each key's own scripted press time** at −20 dBFS. The music channel — and
//! only the music channel — carries [`PRE_ROLL_FRAMES`] of leading silence,
//! the two buffers the host's audio queue already has rendered ahead of a
//! fresh cue (`trail_audio::BUFFER_COUNT − 1` × `BUFFER_FRAMES` = 1024 frames
//! = 21.3 ms at 48 kHz). The file therefore carries the REAL perceived offset
//! between finger and note, and the two channels line up under each other in
//! any editor. "The notes don't align with the keystrokes" is a measurement
//! here, not an opinion.
//!
//! # THE CENSUS — `--census`, and it needs no ear
//!
//! Sweeps 3 / 4 / 4.5 / 5 / 6 / 8 / 10 / 14 cps, clean and at ±25 % jitter,
//! over three corpora, and reports what the melody law did to each: keys,
//! steps, distinct-pitch %, melody notes/s, SILENT keys and a hash of the
//! degree sequence itself — read off the engine's
//! own [`aterm_effects::trail_sound::MelodyV2`] hooks rather than re-derived,
//! so the bench cannot drift from the law it measures.
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
    CHANNELS, EventMeta, SoundEvent, SoundGesture, SoundKind, SoundVoice, TimbreStops, TrailSynth,
};

const SR: u32 = 48_000;
const SEED: u32 = 0x50_4F_4F_46; // "POOF"
/// The host's audio device block (`aterm-gui`'s `trail_audio::BUFFER_FRAMES`),
/// so the render's cue quantisation matches the shipping one.
const BLOCK: usize = 512;

/// The two audio buffers the host's output queue already holds rendered when
/// a fresh cue arrives — `trail_audio::BUFFER_COUNT − 1` (3 − 1) times
/// `BUFFER_FRAMES` (512). 1024 frames = 21.3 ms at 48 kHz, and it is the
/// steady-state distance between a finger and the note it asked for. Prepended
/// to the MUSIC channel alone, so the click track keeps the key's own time and
/// the file carries the offset rather than hiding it.
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
    /// [`SoundEvent::shifted`] — the host-priced "this glyph was typed with
    /// Shift held". The scripts below set it exactly where a person's hand
    /// would: capitals and shifted symbols.
    shifted: bool,
    /// THE CHARACTER THE KEY PUT ON SCREEN, which the old tuple threw away.
    /// `'\0'` where no glyph backs the cue — a bare Shift, a held Backspace.
    /// The melody is DERIVED from what was typed (R2), so the bench hands the
    /// engine the character through the two slots the host seam fills:
    /// [`EventMeta::glyph_class`] and [`EventMeta::rank`].
    ch: char,
}

impl Cue {
    /// THE KEY'S OWN TIME on the host input clock, ms — the number the
    /// melody's gate reads and the number the click track ticks at. One
    /// definition, so the census's `algn` column compares the engine's stamp
    /// against the same clock the cue was pushed with rather than a second
    /// rounding of it.
    ///
    /// `0` means "no host stamp" to `v2_at_ms`, so the first millisecond of a
    /// take is pushed to 1 rather than silently falling back to the block
    /// clock.
    fn at_ms(self) -> u32 {
        ((self.t * 1000.0) as u32).max(1)
    }

    /// The host side-car for this press.
    fn meta(self) -> EventMeta {
        EventMeta {
            at_ms: self.at_ms(),
            // THE GLYPH CLASS and THE ALPHABET RANK — what the derived melody
            // and the class voices know about WHAT was typed. Both through
            // the engine's own producers, never a second copy of either
            // table: a bench that carried its own alphabet (or its own idea
            // of which marks knock) could agree with a melody the shipping
            // code does not play.
            glyph_class: aterm_effects::trail_sound::typed_glyph_class(
                (self.ch != '\0').then_some(self.ch),
            ),
            rank: aterm_effects::trail_sound::typed_glyph_rank(
                (self.ch != '\0').then_some(self.ch),
            ),
            // The keyed seam has no travel: a keystroke's origin IS its
            // column. Only the meteor reads this.
            pan_from: 0.0,
            // The bench renders on its own grid: cues land ON a block
            // boundary by construction, so there is no pre-roll to hand over.
            block_lead_s: 0.0,
            // §22's FLOW HEAT. The bench plays the COLD box: flow is a
            // state the hand earns at the keyboard, and a scripted take
            // has no hand. 0.0 is the identity.
            flow: 0.0,
        }
    }
}

/// The glyphs a US layout cannot produce without Shift.
fn needs_shift(ch: char) -> bool {
    ch.is_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(ch)
}

/// A deterministic xorshift32. The jitter has to be a PERFORMANCE, not noise:
/// `--seed N` twice must render byte-identically (§7 step 2d's `cmp` test) and
/// two seeds must be two different hands on one text (§7 step 2c).
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

/// THE HAND: a typing rate and how much it wobbles.
///
/// Human inter-key gaps at prose speed carry ±25 %, and the DELETED step gate
/// was a HARD threshold on that jittery signal — at 4.4 cps a ±10 ms wobble
/// flipped a key across the 220 ms boundary between "a new note" and "a muted
/// repeat of the last one". That is why the jitter is a first-class dial here
/// and not a garnish: it was the axis the aliasing lived on, and it is now the
/// axis the CONTOUR lives on — the derived line reads the same wobble as an
/// accelerating or a hesitating hand ([`ACCEL_SHARE`] in the engine), so the
/// jitter column is what proves the melody is generated from the typing
/// pattern rather than merely triggered by it.
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

    /// A steady metronome — what every scenario used before the flags landed,
    /// so the shipped scripts render exactly as they did.
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

/// Type `text` as a STRICT METRONOME: one cue per character at the hand's own
/// interval and NOTHING else — no sentence rest, no line-ending beat.
///
/// The census exists to read the melody law at ONE rate, and a rest is a
/// different law on a different clock (`PHRASE_PAUSE_MS`'s resolution). Mixing
/// the two smears the numbers the census is for. The ear's takes keep
/// [`type_text`]'s rests, because a person takes them.
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

/// 60 s of realistic prose at 10 cps with the pauses a typist actually takes,
/// looping the corpus until the window is full.
///
/// `metronome` drops every rest — the same script [`census_row`] types, so
/// the take's first 403 keys ARE the census's `prose` row for that rate and
/// the row's `seq` says whether two takes play the same degrees.
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

/// THE EDIT SCENARIO — the deletion cases the owner named, in one pass:
/// `type 12 → delete 12` three times, then twenty alternating type/delete
/// pairs, then a HELD backspace (30 Hz auto-repeat, the rate the min-gap
/// governor actually has to survive).
fn scenario_edit() -> Scenario {
    /// THE WORD THIS SCENARIO WRITES, ERASES AND WRITES AGAIN — twelve
    /// letters, one per key of a lap, so a lap is exactly the twelve presses
    /// the script already had.
    ///
    /// It used to be twelve of the letter `x`. That was harmless while the
    /// melody walked a pre-composed verse and the character reached nothing,
    /// and it became a lie the day the line was DERIVED from the glyph: a
    /// doubled letter is a stride of zero by §3.1, so twelve `x`s are twelve
    /// re-strikes of ONE note, and the reel pair this scenario exists to make
    /// — "type / delete / retype: the melody un-writes, one note per key both
    /// ways" — had no melody in it to un-write. A fixture that types one
    /// letter cannot demonstrate a melody derived from the letters.
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
    // Alternating: the case where a poof must not be swallowed by the
    // keystroke inside the global min-gap. The letters keep advancing through
    // the word, so the twenty keys are twenty notes and a swallowed one is
    // audible as a hole rather than as more of the same pitch.
    for i in 0..20 {
        push(&mut cues, t, SoundKind::Typed, 0.0, word[i % word.len()]);
        t += 0.09;
        push(&mut cues, t, SoundKind::Backspace, 0.0, '\0');
        t += 0.09;
    }
    t += 0.9;
    // HELD backspace: 30 per second for a second.
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

/// THE WHITESPACE SCENARIO — ordinary word spaces, then the pathological
/// cases: single-letter words (`a a a a`), four-space indentation, and an
/// eight-space run. The coalescing law is audible here or nowhere.
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

/// THE SHIFT SCENARIO — the owner's "when doing a shift-key press (not the
/// shift, but the shifted key) make it higher pitched", as an A/B you can hear
/// inside one file.
///
/// Three passes over the SAME words: all lower case, then Capitalised, then
/// SHOUTED. The pitch difference is the whole point, so the words, the cadence
/// and the pans are identical across the three — only `shifted` moves.
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

/// THE ROTATION SCENARIO — the owner's 2026-08-31 ask, as a take you can hear:
/// "when pressing shift, the tone should rotate, same for space."
///
/// Two halves, deliberately sparse so every gesture is separable and can be
/// PITCHED from the render rather than argued about:
///
/// - A. THE DRONE CASE. Twenty BARE [`SoundKind::Shift`] presses, 0.45 s
///   apart, with nothing else sounding and the pan held at centre — so the
///   column nudge is constant and the melody never steps. Whatever pitch
///   variety is here is the lift's OWN, which is exactly the thing the owner
///   says is missing. Every other scenario in this bench sets `shifted` on
///   capitals and cues no bare modifier at all, so this case has never been
///   measured before.
/// - B. THE WORD CLOCK. Twenty-four words of prose at 9 cps — two laps of an
///   eight-step bass walk, two of a twelve — so a walk that returns to its
///   root too soon shows up as a repeat inside the take.
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
            // A BARE MODIFIER PUTS NO GLYPH ON SCREEN. This is defect (i) of
            // §2.3 made visible in the script: a cue with no character behind
            // it that nonetheless sings a pitched note.
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

/// A 20 cps BURST — twice the sustained rate the governor is tuned for, six
/// seconds of it, with spaces at real word cadence.
fn scenario_burst() -> Scenario {
    /// THE LETTERS THE BURST IS MADE OF — real text, cycled, spaces removed
    /// because the script cues its own on the sixth key.
    ///
    /// It used to be the letter `x`, once per key. Under a derived line that
    /// makes every key of the burst a doubled letter and therefore a
    /// re-strike of one pitch, which would have handed the owner a reel pair
    /// captioned "every key is now its own note" playing three notes a second
    /// under twelve onsets. The rate, the pan sweep, the heat and the word
    /// cadence are all exactly as they were; only the glyph is real.
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

/// THE PATHOLOGICAL SCENARIO — the text that could make a siren.
///
/// The melody is to be DERIVED from what you typed (R2), and a derivation is
/// only as good as its worst input. Held letters, a digit run, four bare
/// exclamation marks, a base64 blob with no words in it, and a repeated word:
/// text with no prosody, no word rhythm and long runs of one character, typed
/// fast. If the derivation turns any of these into a klaxon, an alarm clock or
/// a machine gun, it is here that it shows — not in a paragraph of English.
fn scenario_pathological() -> Scenario {
    let mut cues = Vec::new();
    let mut t = 0.5f32;
    let hand = &mut Hand::steady(12.0);
    // A HELD LETTER at auto-repeat speed — one character, thirty times.
    t = type_metronome(&mut cues, t, hand, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 0.6);
    t += 0.7;
    // DIGITS, which the glyph class separates from letters.
    t = type_metronome(&mut cues, t, hand, "0123456789 0123456789", 0.6);
    t += 0.7;
    // THE HERO GLYPH, four times, alone.
    t = type_metronome(&mut cues, t, hand, "!!!!", 0.6);
    t += 0.7;
    // A BASE64 BLOB: capitals, digits and punctuation with no word shape at
    // all, so the Shift path fires at random and no boundary ever arrives.
    t = type_metronome(
        &mut cues,
        t,
        hand,
        "aGVsbG8gd29ybGQgdGhpcyBpcyBub3QgcHJvc2U+Pz8/",
        0.6,
    );
    t += 0.7;
    // ONE WORD, over and over: the repetition case a pattern-derived melody
    // must not turn into a drone.
    t = type_metronome(&mut cues, t, hand, "the the the the the the the the ", 0.6);
    t += 0.7;
    // AND THE SAME TEXT SLOW, so the take carries the pathological material at
    // both ends of the speed range.
    t = type_metronome(&mut cues, t, &mut Hand::steady(3.5), "!!!! 999 aaaa", 0.6);
    Scenario {
        name: "pathological".into(),
        cues,
        seconds: t + 1.5,
        window: (0.4, t + 0.8),
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
    /// THE DEGREE THE LINE SOUNDED ON EVERY TYPED KEY of this take, read off
    /// `MelodyV2::walk` as the render ran.
    ///
    /// **This is what settles "renders without a siren", and the audio
    /// columns are not.** No spectral column can refute a siren: `range st`
    /// is wide whether the line is a phrase or a ramp, `centroid` is an
    /// average over both, and an envelope detector cannot even resolve
    /// overlapping notes at prose density. A siren is a property of the
    /// SEQUENCE — a ramp that never turns, a register it walks out of, one
    /// degree it sits on — so it is settled on the sequence.
    degrees: Vec<i8>,
}

/// WHERE A CUE'S VOICE IS ALLOWED TO START, relative to the key.
///
/// The shipping host renders whole 512-frame blocks and applies every queued
/// cue at a block boundary, so a key that lands mid-block starts 0–10.7 ms
/// late — a uniform jitter completely uncorrelated with the melody's own
/// clock. §7 step 2b puts the two candidate repairs on one click track and
/// lets the owner pick by ear.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BlockFix {
    /// TODAY. The cue is applied at the first block boundary at or after its
    /// press time: 0–10.7 ms late, and a different amount every key.
    Ship,
    /// J0 — ZERO ADDED LATENCY. The onset lands on the key's own sample.
    ///
    /// The bench reaches this by splitting the render at the cue's frame,
    /// which is the BEST CASE of the repair: a real j0 inside the host would
    /// place the voice at a block boundary with its clock already advanced,
    /// which also costs up to 10.7 ms off the head of the mallet. What the
    /// click track settles here is the ALIGNMENT; the mallet's truncation is
    /// the part that has to be judged on the engine once `block_lead_s`
    /// exists.
    J0,
    /// J1 — A CONSTANT +10.7 ms, mallet intact. Every onset is exactly one
    /// block late, so the jitter is zero and the offset is a constant an ear
    /// adapts to within a phrase.
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
            // Round UP to the block boundary: the block containing the cue is
            // already being filled when the cue arrives.
            Self::Ship => exact.div_ceil(BLOCK) * BLOCK,
            Self::J0 => exact,
            Self::J1 => exact + BLOCK,
        }
    }
}

/// **IS THIS TAKE A SIREN?** — §8 step 3's third proof, computed on the
/// sequence the take actually played.
///
/// A siren is a ramp: the line climbs (or falls) without turning, or walks
/// out of its register, or sits on one note. Each of those is a property of
/// the DEGREE SEQUENCE and of nothing else, and each has a closed guarantee
/// behind it in `rainbow_kitty_v2.rs` — reflection at `TUNE_DEG_LO/HI`,
/// `MELODY_RUN_MAX` inverting the fourth identical stride, gravity pulling
/// toward `MELODY_CENTRE_DEG`. This reads the three back off a rendered take,
/// so the render is a check and not just a smoke test.
///
/// Returned: the longest run of identical strides, how many degrees of the
/// nine the line visited, the largest share any one degree took (%), and how
/// many degrees fell outside the register.
fn siren_verdict(degrees: &[i8]) -> (usize, usize, f64, usize) {
    // The TUNE register, `rainbow_kitty_v2::TUNE_DEG_LO..=TUNE_DEG_HI`. Not
    // importable from an example, so it is written once here and the `out`
    // count is what catches a change to it.
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
            // THE STAMP. `push_meta`, not `push`: the melody's contour is
            // derived from THIS number, so a bench that leaves it at 0 tests
            // the block clock and reports it as the melody.
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
        // Render to the next event or the next 512-frame boundary, whichever
        // comes first. Under `BlockFix::Ship` every spawn frame IS a boundary,
        // so this is the shipping cadence exactly; the two repairs are the
        // only thing that ever splits a block.
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

/// The scene table's onset detector: a note announces itself by a rise of
/// 1.35× over the decay it lands on, and nothing counts inside 35 ms of the
/// last — under the 50 ms spacing of the fastest script here, over the
/// sub-octave beat of one note.
const SCENE_ONSET: (usize, f32) = (35, 1.35);

/// THE FINE CENSUS, for §8 step 4's rows only. The proof has to be able to
/// SEE the sounds it says are gone: the bare Shift's lift landed 30-100 ms
/// before the capital, and the octave echo 25 ms behind it (the retired
/// `ECHO_DELAY_S`) at −8 dB — so the refractory is 10 ms, and the rise is
/// 1.12, which is what a −8 dB voice adds to a decaying note's envelope
/// (incoherent: √(1 + 0.4²) = 1.08 at the same level, more where the letter
/// has already decayed). Measured on both engines over the rise ratio, same
/// rows, same seed (`Capital` = Shift then the letter 60 ms on):
///
/// ```text
/// rise        1.35  1.20  1.12  1.08  1.05
/// Capital old    2     2     3     7    13    (v0.76.0: lift + letter + echo)
/// Capital new    1     1     1     1     1    (this tree)
/// Typed   new    1     1     1     1     8
/// Typed@cyan     1     1     1     2    10    (1.08: the bloom's own 12 ms swell)
/// ```
///
/// So the instrument is coarse and its window is narrow: at 1.12 it sees the
/// old engine's echo and not the new bloom's swell; at 1.08 it counts the
/// swell; at 1.05 it counts the tine's own partial beat. It refutes the old
/// engine (3 ≠ 1, and the lift is 2 ≠ 1 at every ratio) and passes this one,
/// which is what §8 asked of it — the EXACT statements are the unit pins
/// `a_capital_is_one_strike_with_a_lifted_degree_and_one_ring` (one TUNE
/// voice, nothing at 25 ms, ONE ring at 2f swelling in 60 ms on — no rise
/// this census can count) and `a_bare_shift_is_a_pitched_pickup_that_never_steps`
/// (one GRAFT voice, a lone P1: since 2026-09-10 the Shift IS an onset of
/// its own, which is why the `Capital` row now reads 2 — two KEYS).
/// At the scene detector's 35 ms the census read `Capital 1` on the old
/// engine too — the finding that made this constant.
///
/// VALID ONLY ABOVE THE BASS: the 2.7 ms RMS window holds a full period of
/// the tune's lowest note (261 Hz) but rides the waveform of the bass dyad
/// under a Space or a Kill (65-130 Hz) and counts every cycle, so the fine
/// census is taken only on the rows [`Probe::census`] marks.
const FINE_ONSET: (usize, f32) = (10, 1.12);

/// Onset frames. RISE detection, not threshold crossing: at 10 cps the notes
/// are 100 ms apart under 100-300 ms tails, so the envelope NEVER returns to a
/// floor between them and a re-arming threshold counts one note per burst
/// (measured: 1.2 onsets/s on a 10 cps script). Only the derivative separates
/// them. A 2.7 ms RMS window smooths the pulse/sub-octave beat; the caller
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
        // The first audible window is an onset by definition (its attack is
        // shorter than one window); after that a note announces itself by a
        // rise over the decay it lands on.
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
    /// The NOTE RANGE the melody actually used, in semitones between its
    /// lowest and highest pitched onset — "how wide is the tune", which is the
    /// number the owner's "a greater range of notes" ask lives in.
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
    // The melodic SPAN: the extremes of the pitched onsets. Percussive
    // outliers cannot enter — `notes` is already tonality-gated.
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
    /// How many onsets the gesture's own body carries by the scene table's
    /// detector ([`SCENE_ONSET`]), over the gesture's DIFFERENCE signal (see
    /// [`probe`]).
    onsets: usize,
    /// THE FINE CENSUS (§8 step 4's proof): the same body counted by
    /// [`FINE_ONSET`], on the rows it is valid for. A capital used to read
    /// three sounds — the lift, the letter, an octave echo 25 ms behind it;
    /// it must read ONE.
    fine: Option<usize>,
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
    /// THE MODIFIER'S OWN CUE FIRST: a bare-Shift `SoundKind::Shift` at the
    /// segment's start, the gesture [`CAPITAL_SHIFT_LEAD_MS`] later — which
    /// is how the host mints a capital (`app_input.rs` fires the lift on the
    /// modifier's own keydown, the shifted letter follows on its own key).
    /// Without it the row was one cue and §2.3's first onset was never in it.
    lift: bool,
    /// Take the [`FINE_ONSET`] census on this row — valid only where the
    /// gesture has no bass under it (see the constant).
    census: bool,
}

/// How long a hand holds Shift before the letter lands: §2.3 measured the
/// lift arriving 30-100 ms ahead of the capital it precedes.
const CAPITAL_SHIFT_LEAD_MS: usize = 60;

/// One gesture alone, from one settled melody state, measured as a
/// DIFFERENCE: the settled synth is rendered twice, once with the gesture
/// pushed and once left alone, and the row is scored on `with − without`.
/// Both runs share the seed and the settling keys, so everything the
/// gesture did not cause cancels sample for sample — in particular the third
/// settling key's 340 ms bloom tail, which under the shipping stops was
/// louder than a felt Shift and had the `Shift` row reporting the BLOOM's
/// tonality (221) and spectrum (hi>2k 0.997) instead of the thump's. What
/// remains is the gesture's own body, and a sound the gesture DAMPED shows
/// up in it too (as the negative of the ring it cut), which is correct: that
/// is part of what the key did. The reel keeps the raw `with` render, which
/// is what the ear hears.
///
/// Returns the row and the raw render of the gesture's segment.
fn probe(
    p: &Probe,
    voice: SoundVoice,
    style: GlowStyle,
    volume: f32,
    seed: u32,
    timbre: Timbre,
) -> (ProbeRow, Vec<f32>) {
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
    let stamp = |frame: usize| EventMeta {
        at_ms: ((frame as f64 * 1000.0 / f64::from(SR)) as u32).max(1),
        glyph_class: 0,
        rank: 0,
        pan_from: 0.0,
        block_lead_s: 0.0,
        // §22's FLOW HEAT. The bench plays the COLD box: flow is a
        // state the hand earns at the keyboard, and a scripted take
        // has no hand. 0.0 is the identity.
        flow: 0.0,
    };
    // THREE settling keystrokes, ~340 ms apart: enough that the previous
    // note's tail is dead, and — since the bar's accents fall every third
    // keystroke — enough that the PROBE itself lands on an accent. A gesture
    // measured on a ghost slot would be reported against the accompaniment
    // rather than against the tune.
    //
    // Stamped, like every other push in this bench: at 341 ms apart the
    // settling keys are three unhurried notes — but only if the engine is
    // reading a real clock. Unstamped they collapse onto the block clock and
    // the probe is measured from a state the shipping engine never reaches.
    let settled = || {
        let mut synth = TrailSynth::new(SR as f32, seed);
        synth.set_v2_timbre_stops(timbre.stops());
        let mut warm = vec![0.0f32; 16_384 * CHANNELS];
        for i in 0..3 {
            synth.push_meta(ev(SoundKind::Typed, false), stamp(i * 16_384));
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

    // WITH the gesture. The lift, when the row carries one, is its own cue
    // at the segment's first frame; the gesture lands CAPITAL_SHIFT_LEAD_MS
    // of rendered audio later, stamped that much later, exactly as two host
    // pushes would arrive.
    let mut synth = settled();
    let mut stereo = vec![0.0f32; frames * CHANNELS];
    let t0 = 3 * 16_384;
    let mut at = 0usize;
    if p.lift {
        synth.push_meta(ev(SoundKind::Shift, false), stamp(t0));
        at = SR as usize * CAPITAL_SHIFT_LEAD_MS / 1000;
        synth.render(&mut stereo[..at * CHANNELS]);
    }
    synth.push_meta(ev(p.kind, p.shifted), stamp(t0 + at));
    let voices = synth.live_voices();
    synth.render(&mut stereo[at * CHANNELS..]);
    let raw = to_mono(&stereo);

    // WITHOUT it: the same settled synth left to ring.
    let mut quiet = settled();
    let mut alone = vec![0.0f32; frames * CHANNELS];
    quiet.render(&mut alone);
    let alone = to_mono(&alone);
    let mono: Vec<f32> = raw.iter().zip(&alone).map(|(a, b)| a - b).collect();

    let peak = mono.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    // Score the gesture's own body: onset to 250 ms past it.
    let start = mono.iter().position(|v| v.abs() > peak * 0.05).unwrap_or(0);
    let end = (start + SR as usize / 4).min(mono.len());
    let (centroid, hi) = spectrum(&mono, start, end);
    // The onset census over the whole half second: a second sound anywhere
    // behind the key — an echo, a lift, a tap — counts against the gesture.
    let count = |law| onsets(&mono, 0, mono.len(), law).len();
    let row = ProbeRow {
        name: p.label.to_string(),
        peak_db: db(f64::from(peak)),
        rms_db: db(rms(&mono[start..end])),
        centroid_hz: centroid,
        hi_frac: hi,
        tonality: tonality(&mag_at(&mono, start)),
        voices,
        onsets: count(SCENE_ONSET),
        fine: p.census.then(|| count(FINE_ONSET)),
    };
    (row, raw)
}

/// The gesture probes, back to back with 400 ms of air between them — the
/// file to open FIRST, because it is where a keystroke, a downbeat and an
/// erase can be heard against each other rather than one at a time.
fn probe_reel(
    voice: SoundVoice,
    style: GlowStyle,
    volume: f32,
    seed: u32,
    timbre: Timbre,
) -> (Vec<ProbeRow>, Vec<f32>, Vec<Cue>) {
    let gap = vec![0.0f32; SR as usize * 2 / 5];
    let mut reel = Vec::new();
    let mut rows = Vec::new();
    // Each probe's gesture fires on the FIRST frame of its own segment (a
    // row with a lift: the Shift there, the gesture CAPITAL_SHIFT_LEAD_MS
    // on), so the reel's click track is exact rather than detected.
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
        let (row, mono) = probe(p, voice, style, volume, seed, timbre);
        reel.extend_from_slice(&mono);
        reel.extend_from_slice(&gap);
        rows.push(row);
    }
    (rows, reel, marks)
}

/// The gestures the probe table and the reel cover, in reel order. `Capital`
/// is the host's whole gesture — the bare Shift's cue, then the shifted
/// `Typed` 60 ms on — and it is in the table because §8 step 4's proof is
/// stated on it: under the shipped engine that pair was THREE onsets.
const PROBES: [Probe; 10] = [
    Probe {
        label: "Typed",
        kind: SoundKind::Typed,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: true,
    },
    Probe {
        label: "Typed@cyan",
        kind: SoundKind::Typed,
        shifted: false,
        hue: 0.5,
        lift: false,
        census: true,
    },
    Probe {
        label: "Capital",
        kind: SoundKind::Typed,
        shifted: true,
        hue: 0.0,
        lift: true,
        census: true,
    },
    Probe {
        label: "Space",
        kind: SoundKind::Space,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
    },
    Probe {
        label: "Backspace",
        kind: SoundKind::Backspace,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
    },
    Probe {
        label: "KillWord",
        kind: SoundKind::KillWord,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
    },
    Probe {
        label: "Shift",
        kind: SoundKind::Shift,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: true,
    },
    Probe {
        label: "Kill",
        kind: SoundKind::Kill,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
    },
    Probe {
        label: "Land",
        kind: SoundKind::Land,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
    },
    Probe {
        label: "Jump",
        kind: SoundKind::Jump,
        shifted: false,
        hue: 0.0,
        lift: false,
        census: false,
    },
];

// ---------------------------------------------------------------------------
// The ROTATION table — does a repeated gesture repeat its NOTE?
// ---------------------------------------------------------------------------
//
// The scene table above cannot answer this. `pitch/s` and `rept%` are computed
// over EVERY pitched onset in the mix, so a drone underneath a moving melody
// reads as a moving melody; and the gesture probe fires each gesture exactly
// ONCE, which is the one number of presses at which nothing can be shown to
// repeat. The owner's ask is about the SECOND press and the eighth, so the
// measurement has to follow one gesture across a take.
//
// Pitched at the SCRIPT'S OWN CUE TIMES rather than by onset detection: the
// script knows when every Shift and every Space was pressed, so there is no
// question of which onset belongs to which gesture, and a gesture that fell
// under the governor's min-gap simply reads as its neighbour's tail (and is
// dropped by the prominence test below) instead of shifting every later
// reading by one.

/// Two pitches are THE SAME NOTE within this many cents. A quarter tone: wider
/// than the FFT's own resolution at these frequencies, far narrower than the
/// smallest step the pentatonic lattice can make (~2 semitones).
const SAME_NOTE_CENTS: f64 = 50.0;

struct Rotation {
    name: String,
    hz: Vec<f64>,
    /// Distinct notes sounded, at [`SAME_NOTE_CENTS`] resolution.
    distinct: usize,
    /// The CLOSEST recurrence: the fewest events between two soundings of one
    /// note. `None` when no note recurred in the take at all. THIS is the
    /// number the ask lives in — "rotate" means this is large, not merely that
    /// `distinct > 1`.
    min_repeat: Option<usize>,
}

/// Pitch one gesture at each of `cues` (seconds), reading the loudest partial
/// in `lo..hi` — the register the gesture is known to live in, which is what
/// keeps a bass root from being read as the letter over it and vice versa.
fn rotation_of(name: &str, mono: &[f32], cues: &[f32], lo: f32, hi: f32) -> Rotation {
    // Past the family's ~12 ms contour bend, so the reading is the note the
    // gesture LANDS on rather than the one it scoops through — the same
    // settle the scene table's pitch reads use.
    const SETTLE: usize = SR as usize / 40;
    let mut hz = Vec::new();
    for &t in cues {
        let s = (t * SR as f32) as usize + SETTLE;
        if s + FFT_N >= mono.len() {
            continue;
        }
        let m = mag_at(mono, s);
        // PROMINENCE, not tonality: the band is already narrow, so the test
        // that matters is whether this gesture actually spoke here or whether
        // the window holds nothing but a neighbour's decay.
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

/// THE CLICK: 2 ms of 2 kHz under a raised-cosine window, at −20 dBFS.
///
/// Short enough that its own onset is unambiguous at 48 kHz, windowed so it
/// has no click of its own to confuse the eye in an editor, and 20 dB down so
/// it never competes with the music channel it is there to measure.
const CLICK_HZ: f32 = 2_000.0;
const CLICK_MS: f32 = 2.0;
const CLICK_LEVEL: f32 = 0.1;

/// The RIGHT channel: one tick at every scripted key press, on the key's own
/// time — no pre-roll, because the pre-roll is exactly the thing being shown.
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

/// 32-bit float STEREO, written VERBATIM — no normalisation, no dither. An
/// A/B whose two sides are independently normalised is not an A/B, and that
/// rule is unchanged: the click rides its own channel and never touches the
/// music's gain staging.
///
/// LEFT is the synth, delayed by [`PRE_ROLL_FRAMES`]; RIGHT is `click`, at the
/// keys' own times. The delay is the host's real steady-state queue depth, so
/// the horizontal distance between a tick and the note under it in an editor
/// IS the offset the owner hears.
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

// ---------------------------------------------------------------------------
// THE CENSUS — what the melody law does to a hand, with no ear in the loop
// ---------------------------------------------------------------------------
//
// R1 says one keystroke is one melody step at every typing speed. That is a
// falsifiable claim about a pure function of the key times, so it is settled
// here — offline, in a second, before a single WAV is rendered — rather than
// argued about.
//
// The numbers are read off the ENGINE'S OWN state after every push
// (`MelodyV2::steps`, `last_onset_ms`, `walk`), never re-derived from the
// constants: a census that carried its own copy of the law could agree with a
// design that the shipping code does not implement, which is exactly the
// failure this whole exercise is recovering from.

/// The rates swept, characters per second. 4.5 straddled the DELETED 220 ms
/// gate (222 ms per key), which is where a ±25 % wobble used to decide whether
/// a key was a note at all; it is kept as the row where a regression would
/// show first.
const CENSUS_CPS: [f32; 8] = [3.0, 4.0, 4.5, 5.0, 6.0, 8.0, 10.0, 14.0];

/// The jitter columns: a metronome, and a hand.
const CENSUS_JITTER: [f32; 2] = [0.0, 0.25];

struct CensusRow {
    corpus: &'static str,
    cps: f32,
    jitter: f32,
    /// TYPED keys only — a space is a rest and an Enter is a cadence; neither
    /// is a note the melody's playhead can step on.
    keys: usize,
    /// Keys that moved `MelodyV2::steps` — the melody advanced. Under R1
    /// this must equal `keys` on every row, at every rate, for ever.
    steps: usize,
    /// Keys whose sounding degree DIFFERS from the previous key's. This is
    /// the number "the melody moved" lives in: a re-strike repeats the last
    /// pitch, and so, occasionally, does a rest cadence.
    distinct: usize,
    /// **THE REPEAT LEDGER**, and it is what discharges the §8 proof rather
    /// than what excuses it.
    ///
    /// §8 step 3 asks for "100 % distinct". §3.1 of the same document keeps
    /// `Touch::ReStrike` and defines it as "a stride of zero — a genuinely
    /// repeated pitch, from a doubled letter". Both cannot be true of the
    /// same column: a corpus that types `ll` has ASKED for two of the same
    /// note, and an engine that refused would be overwriting the text
    /// instead of deriving from it. So the census does not report one number
    /// and argue about the remainder — it ACCOUNTS for every key that
    /// repeated, against the causes the design itself names, and the residual
    /// is the number that must be zero.
    ///
    /// `dbl` — the previous typed key had the same alphabet rank. §3.1's
    /// `Touch::ReStrike`, exactly, and the only repeat the text asked for.
    dbl: usize,
    /// `head` — a word head whose chord snap landed on the degree the last
    /// word ended on: a COMMON TONE across a chord change, which is voice
    /// leading and not a stall.
    head: usize,
    /// `subj` — the line answering its own subject on an interval the
    /// subject genuinely latched as a unison. §3.1 asks for exactly this
    /// replay, and a subject is entitled to a repeated note (so is every
    /// subject ever written); what it is NOT entitled to is a unison it never
    /// heard, which is what the session's first key used to hand it.
    subj: usize,
    /// `stall` — a repeat with no cause the design names: gravity cancelling
    /// a real stride, a fold sending a real distance to zero, a reflection
    /// landing back on the note it left. Every one of these is the engine
    /// standing still while the hand moved, which is the DEFECT this whole
    /// change exists to remove, in miniature.
    ///
    /// **THIS COLUMN IS THE §8 PROOF.** `stall == 0` on every row is "100 %
    /// distinct" in the only form that is simultaneously true, checkable and
    /// worth having: every key either moved the melody's pitch, or repeated
    /// it for a reason written down in §3.1.
    stall: usize,
    /// Keys that made NO SOUND AT ALL — a re-strike inside
    /// `RESTRIKE_COALESCE_MS`, which advanced the state in silence.
    silent: usize,
    /// **DOES THE NOTE CARRY THE KEY'S OWN TIME?** The largest gap, in ms,
    /// between a key's scripted press time and the time stamped on the onset
    /// the mixer actually made for it (`MelodyV2::last_onset_ms`, written
    /// AFTER the TUNE spawn returned a slot).
    ///
    /// This is "do the notes align with the keystrokes" as a NUMBER, settled
    /// in the one domain where it is decidable. An envelope detector run over
    /// the rendered music channel cannot resolve overlapping notes at prose
    /// density and returns the same ratio for any take you give it; this
    /// reads what the engine did, per key, and cannot be fooled by density.
    ///
    /// A STAMPED ROW MUST READ 0, and the assert below says so. `blockclk`
    /// reads the 512-frame block quantisation (up to 10.7 ms) because an
    /// unstamped push has no key time to carry — which is the whole point of
    /// keeping it in the table, and is the same defect the host still has at
    /// `app_render.rs`'s frame drain, at frame granularity.
    algn_ms: u32,
    /// The wall time the typing occupied, seconds.
    span_s: f64,
    /// A HASH OF THE DEGREE SEQUENCE ITSELF — FNV-1a over the walk after
    /// every typed key.
    ///
    /// This is the column that survived the gate's deletion. With every key
    /// stepping, `dist%` and `silent` are saturated on every row and can no
    /// longer tell two clocks apart; the SEQUENCE still can, because the
    /// derived contour reads the inter-key gap and a quantised clock reports
    /// a different gap. It is also what makes a determinism claim readable
    /// off one line: two runs that agree here played the same tune.
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
/// SHIPPING engine exactly as a take is rendered, and count what the melody
/// did.
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
    // The repeat ledger, and the previous TYPED key's rank it is kept
    // against. `0` is "no key behind this cue", which is never a repeat.
    let (mut dbl, mut head, mut subj, mut stall) = (0usize, 0usize, 0usize, 0usize);
    let mut prev_rank: u8 = 0;
    let mut algn_ms: u32 = 0;
    // FNV-1a, 64-bit — small, dependency-free, and order-sensitive, which is
    // the whole requirement.
    let mut seq: u64 = 0xcbf2_9ce4_8422_2325;
    let (mut ci, mut f) = (0usize, 0usize);
    while f < frames {
        while ci < cues.len() && spawn[ci] <= f {
            let cue = cues[ci];
            let typed = cue.gesture == SoundGesture::Trail(SoundKind::Typed);
            let m = synth.melody_v2();
            let (was_step, was_onset, was_walk) = (m.steps(), m.last_onset_ms(), m.walk());
            // Read BEFORE the push, because `word_pos` moves on it: this key
            // is a word head iff nothing has been typed since the last Space
            // or Enter, which is the same predicate the engine branches on.
            let was_head = m.word_pos() == 0;
            // The engine's own replay predicate, read before the push:
            // `!word_head && motif_play > 0`. A word head is never a replay —
            // it is the chord tone the subject is answered ONTO.
            let was_answer = !was_head && m.motif_answering();
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
                // THE OLD BENCH, kept alive as a control: bare `push` leaves
                // `at_ms = 0`, `v2_at_ms` falls back to the synth's 512-frame
                // block clock, and the gate is measured on a 10.667 ms grid
                // that has nothing to do with the hand.
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
                    // IT REPEATED. Which of the design's own causes was it?
                    // Charged to the BRANCH THAT PRODUCED THE DEGREE, not to
                    // the first plausible story: an answered interval never
                    // consulted the alphabet, so it cannot be charged to a
                    // doubled letter even when the text happens to have one.
                    if was_answer {
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
    // The typing's own duration: first press to last press, plus the one
    // nominal interval the last key occupies. For a metronome that is exactly
    // `cues.len() / cps`, so notes/s is directly comparable to cps.
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
        stall,
        silent,
        algn_ms,
        span_s,
        seq,
    }
}

/// The census sweep, printed.
#[allow(clippy::too_many_arguments)]
fn census(
    seed: u32,
    voice: SoundVoice,
    style: GlowStyle,
    fix: BlockFix,
    bed: bool,
    timbre: Timbre,
) {
    // TWO CORPORA, and the difference between them is the point.
    //
    // `letters` is the prose with its spaces and line feeds removed: a pure
    // key stream, which isolates the melody law. It is the column the §2.6
    // predictions were stated in, because the deleted gate's arithmetic there
    // was exactly `floor(220 / (1000/cps))` and nothing else.
    //
    // `prose` is the same text with every space a `Space` and every newline a
    // `Jump`. Neither is a derived note, but both eat wall time and both move
    // the harmony, so the gap between two TYPED keys across a word boundary is
    // wider and the contour reads differently. It is the realistic column.
    // …and `blockclk` is `letters` again, pushed the way this bench pushed
    // for its whole life before 2026-09-08: bare `push`, no stamp, the melody
    // clock quantised to whatever grid the voice is spawned on. It is a
    // CONTROL, not a candidate.
    //
    // WHAT THE CONTROL REFUTES CHANGED WHEN THE GATE DIED, and the change is
    // the whole point of the exercise. Under the gate, `blockclk` disagreed
    // with `letters` in `dist%`: a quantised clock crossed a 220 ms threshold
    // on different keys, and at 4.5 cps clean it read 83.4 % against the
    // stamped 100.0 %. With every key now stepping, `dist%` is 100 and
    // `silent` is 0 in BOTH columns and neither can tell the two clocks apart.
    // The `seq` column can, and more sharply than the old one ever did: the
    // derived contour reads the inter-key GAP on every single key, so
    // quantising the clock to a 10.667 ms grid changes the tune everywhere
    // rather than at a threshold. `blockclk` must differ from `letters` in
    // `seq` on EVERY row.
    //
    // IT ONLY REFUTES UNDER `spawn Ship`, and the reason is worth stating
    // because it is easy to read the table the wrong way. The unstamped clock
    // is the spawn grid, so `--jitterfix` moves the control as well as the
    // take. Under J0 the spawn IS the cue's own sample, so the block clock and
    // the hand's clock coincide BY CONSTRUCTION and the control converges on
    // `letters` while the stamp is perfectly sound. The census says so on
    // stdout when the spawn is not Ship.
    let letters: String = PROSE.chars().filter(|c| !c.is_whitespace()).collect();
    let corpora: [(&'static str, &str, bool); 3] = [
        ("letters", &letters, true),
        ("prose", PROSE, true),
        ("blockclk", &letters, false),
    ];

    println!(
        "== census: the melody law against a hand ==\n\
         seed {seed:#010x}, {} / {style:?}, spawn {fix:?}, bed {}\n",
        voice.name(),
        if bed { "on" } else { "off" }
    );
    println!(
        "{:<8} {:>5} {:>5} {:>6} {:>6} {:>7} {:>8} {:>7} {:>4} {:>5} {:>5} {:>6} {:>5}  {:<16}",
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
        "stall",
        "algn",
        "seq"
    );
    // The two totals the acceptance line is stated over, so the verdict is
    // computed from the sweep rather than transcribed from a comment.
    let (mut worst_stall, mut rows_off_r1) = (0usize, 0usize);
    let (mut worst_algn, mut worst_algn_ctl) = (0u32, 0u32);
    for (corpus, text, stamped) in corpora {
        for jitter in CENSUS_JITTER {
            for cps in CENSUS_CPS {
                let r = census_row(
                    corpus, text, cps, jitter, seed, voice, style, fix, bed, stamped, timbre,
                );
                // THE LEDGER MUST SUM. If it ever does not, a repeat has a
                // cause the census cannot name, and every percentage below it
                // is describing something other than what happened.
                assert_eq!(
                    r.distinct + r.dbl + r.head + r.subj + r.stall,
                    r.keys,
                    "the repeat ledger does not account for every key on \
                     {corpus} {cps} cps {:.0}% jitter",
                    jitter * 100.0
                );
                // A STAMPED ROW'S NOTE CARRIES THE KEY'S OWN TIME, exactly.
                // This is the alignment claim, asserted rather than heard.
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
                    "{:<8} {:>5.1} {:>5.0} {:>6} {:>6} {:>7.1} {:>8.2} {:>7} {:>4} {:>5} {:>5} {:>6} {:>5}  {:016x}",
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
         latched subject) — and `stall`, the residual, is the engine standing still\n\
         while the hand moved. `distinct + dbl + head + subj + stall` is asserted\n\
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
// The timbre ladder's seam (§7 step 3)
// ---------------------------------------------------------------------------

/// Which instrument the take is rendered on — one variable per file, so the
/// owner can say where to stop. Each rung is the previous one plus exactly
/// one of §3.3's additions, pulled through [`TrailSynth::set_v2_timbre_stops`]
/// on the very same engine: `plain` is the derived line through the shipped
/// tine (the control), `bloom` adds the 3f/4f/6f bloom, `hue` couples the
/// bloom's air and pan and the note's roof to the live hue, and `room` — the
/// full proposal and what ships — adds the Enter/rest air cloud and the
/// answering voice. `room` is the default; a render without `--timbre` is the
/// instrument the user gets.
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
        \x20   [--census] [--probes]"
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
    // **J1 IS WHAT SHIPS, SO IT IS WHAT THE BENCH RENDERS** (the panel's Q3
    // ruling, 2026-09-09; §9). The host already sets
    // `EventMeta::block_lead_s` on every `push_meta`
    // (`aterm_gui::trail_audio::…::push_meta`) and the engine already spends
    // it at `v.t = -(v.delay + self.block_lead_s)`, which puts every onset a
    // constant one block after its press: J1, live, today. `Ship` here was
    // modelling the block clock as it behaved BEFORE that wire existed, so a
    // default of `Ship` meant every reel measured a jitter the product does
    // not have. The ruling took J1 over J0 on the transient — J0's 0-10.7 ms
    // bite out of the mallet's head varies with block phase, i.e. randomly
    // per key, which trades a constant nobody can hear for an attack
    // brightness that changes on every note — so the bench's default is now
    // the shipping behaviour and `--jitterfix ship` is the historical one.
    let mut fix = BlockFix::J1;
    let mut want_census = false;
    let mut metronome = false;
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
            // The prose take's typing rate. Every other scenario keeps its own
            // scripted cadence — the burst is a burst at any setting.
            "--cps" => {
                let v = val("--cps");
                cps = v
                    .parse::<f32>()
                    .ok()
                    .filter(|r| *r > 0.1)
                    .unwrap_or_else(|| {
                        eprintln!("--cps wants a rate over 0.1, got {v:?}");
                        std::process::exit(2);
                    });
            }
            // Per-interval wobble, PERCENT: 25 is the ±25 % a hand carries.
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
            // Seeds the HAND (the jitter) and the SYNTH (the velocity draw),
            // so one seed is one performance end to end.
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
            "--probes" => want_probes = true,
            "--metronome" => metronome = true,
            "-h" | "--help" => usage(),
            other if other.starts_with("--") => {
                eprintln!("unknown flag {other:?}");
                usage()
            }
            other => out = PathBuf::from(other),
        }
    }

    if want_census {
        // No WAVs: the census needs no ear, and rendering 96 takes to print a
        // table would make the one run that must happen FIRST the slowest.
        census(seed, voice, style, fix, bed, timbre);
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
        probe_tables(&out, &tag, voice, style, seed, timbre);
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
                // THE LIFT is read in the melody's register, THE BASS in its
                // own [220, 440) band — the register map's two bands are
                // disjoint, which is the whole reason a space can be pitched
                // out of a take that is otherwise made of letters.
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
        // The three closed guarantees, read back: MELODY_RUN_MAX inverts the
        // fourth identical stride, reflection keeps the line in the register,
        // and gravity plus the register keep it off one note. A take with
        // fewer than a handful of keys says nothing either way.
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

    probe_tables(&out, &tag, voice, style, seed, timbre);
}

/// THE GESTURE PROBE TABLE AND REEL, at both volumes — one gesture alone from
/// one settled melody state. Split out so `--probes` can print §8 step 4's
/// onset census and step 6's centroid without rendering seven scenes first.
fn probe_tables(
    out: &std::path::Path,
    tag: &str,
    voice: SoundVoice,
    style: GlowStyle,
    seed: u32,
    timbre: Timbre,
) {
    for volume in [0.4f32, 1.0] {
        println!("\n== gesture probes (isolated, vol {volume}) ==");
        println!(
            "{:<12} {:>8} {:>8} {:>9} {:>8} {:>9} {:>6} {:>6} {:>5}",
            "gesture",
            "peak dB",
            "rms dB",
            "centroid",
            "hi>2k",
            "tonality",
            "voices",
            "onsets",
            "fine"
        );
        let (probes, reel, marks) = probe_reel(voice, style, volume, seed, timbre);
        for p in &probes {
            println!(
                "{:<12} {:>8.2} {:>8.2} {:>9.0} {:>8.3} {:>9.1} {:>6} {:>6} {:>5}",
                p.name,
                p.peak_db,
                p.rms_db,
                p.centroid_hz,
                p.hi_frac,
                p.tonality,
                p.voices,
                p.onsets,
                p.fine.map_or("-".to_string(), |n| n.to_string())
            );
        }
        // §8 STEP 4, RE-RULED 2026-09-10 (the owner: "a sound effect for the
        // shift key and shifted keys"), read off the table. A KEYSTROKE is
        // still one onset by the FINE census (`Typed 1`; a Caps-Lock capital
        // — the `CapsLockA` row, when the table carries it — rings but does
        // not strike twice, its ring swelling on a 30 ms attack). The
        // `Capital` row is TWO KEYS — the bare Shift's pickup and the shifted
        // letter 60 ms on — and reads 2. And the bare Shift SINGS BY DESIGN:
        // a pitched pickup on the lattice at −6 dB re the keystroke, where
        // the 2026-09-08 felt mallet measured −61 dBFS (≈ −39 dB re Typed)
        // and was what the owner could not hear. See [`FINE_ONSET`] for the
        // census on both engines.
        let find = |name: &str| probes.iter().find(|p| p.name == name);
        let fine_of = |name: &str| find(name).and_then(|p| p.fine).unwrap_or(0);
        let (typed, capital) = (fine_of("Typed"), fine_of("Capital"));
        let caps_lock = find("CapsLockA").and_then(|p| p.fine);
        let shift_tonality = find("Shift").map_or(0.0, |p| p.tonality);
        let shift_pitched = shift_tonality >= PITCHED_TONALITY;
        let shift_re_typed = match (find("Shift"), find("Typed")) {
            (Some(s), Some(t)) => s.peak_db - t.peak_db,
            _ => f64::NAN,
        };
        const SHIFT_RE_TYPED_TARGET_DB: f64 = -6.0;
        const SHIFT_RE_TYPED_TOL_DB: f64 = 1.5;
        let shift_level_ok =
            (shift_re_typed - SHIFT_RE_TYPED_TARGET_DB).abs() <= SHIFT_RE_TYPED_TOL_DB;
        let caps_lock_ok = caps_lock.is_none_or(|n| n == 1);
        println!(
            "onset census (fine: {} ms, rise {}; Capital = Shift then the letter \
             {CAPITAL_SHIFT_LEAD_MS} ms on): Typed {typed}, Capital {capital}{}, Shift tonality \
             {shift_tonality:.1} ({}) at {shift_re_typed:+.2} dB re Typed (target \
             {SHIFT_RE_TYPED_TARGET_DB:+.1} ± {SHIFT_RE_TYPED_TOL_DB}) — {}",
            FINE_ONSET.0,
            FINE_ONSET.1,
            caps_lock.map_or(String::new(), |n| format!(", CapsLockA {n}")),
            if shift_pitched { "pitched" } else { "FELT" },
            if typed == 1 && capital == 2 && caps_lock_ok && shift_pitched && shift_level_ok {
                "a keystroke is one onset, a Shift+capital is two keys, and the pickup \
                 sings under the key (§8 step 4 as re-ruled 2026-09-10 holds)"
            } else if !shift_pitched {
                "§8 STEP 4 (2026-09-10) FAILS: the bare Shift does not sing"
            } else if !shift_level_ok {
                "§8 STEP 4 (2026-09-10) FAILS: the pickup is off its −6 dB window"
            } else if capital != 2 {
                "§8 STEP 4 (2026-09-10) FAILS: Shift then a capital is not two onsets"
            } else {
                "§8 STEP 4 (2026-09-10) FAILS: a keystroke is not one onset"
            }
        );
        // §8 STEP 6's PROOF, on the same row: the bloomed Typed probe's
        // centroid sits in §3.3's 1250-1400 Hz window and UNDER 1600 Hz, past
        // which the bloom has become the glass bell the v2 train retired.
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
