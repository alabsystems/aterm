// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trail sound: the aural half of the cursor effects.

mod rainbow_kitty_v2;

pub use rainbow_kitty_v2::{MelodyV2, RainbowKittyV2Palette, TimbreStops};

use rainbow_kitty_v2::{
    LANE_AGE_GUARD_S, LANE_FADE_STEAL_S, LANE_NONE, lane_cap, lane_drops_the_newcomer,
};

use crate::cursor_glow::GlowStyle;
use crate::tone::Tone;

/// Interleaved output channel count.
pub const CHANNELS: usize = 2;

const MAX_VOICES: usize = 28;

/// What the cursor did: the cursor-trail gesture vocabulary, matching the glow engine's `spawn` classification plus the adjacent-vs-jump split (`dist <= 1.0`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SoundKind {
    /// A glyph was typed and the cursor stepped one cell: each style's signature key sound.
    Typed,
    /// Backspace / deletion: the erase poof, a soft unpitched cloud of air above the typing register.
    Backspace,
    /// Cursor navigation (arrows, clicks) — the Typed gesture at a whisper: same timbre family, much quieter and shorter.
    Navigation,
    /// A line-scale kill (^K/^U) that moved the cursor: a soft downward swoosh, a whole clause leaving at once.
    Kill,
    /// A word-scale kill (^W, Alt-D, Alt/Ctrl-Backspace): a slightly larger, softer cousin of the [`SoundKind::Backspace`] poof (the `WORD_POOF_*` constants).
    KillWord,
    /// The dispersal of the smoke puff a plain Backspace spawns (`cursor_glow`'s `PoofVoice::Puff`, cued on the same edge, so sound and smoke share `cursor_glow`'s `POOF_MIN_GAP` rate limit).
    Poof,
    /// A multi-cell jump (Enter to a prompt, tab-complete, redraws): the style's flourish.
    Jump,
    /// A single-cell arrow move, the character-scale motion: one soft in-key melody tone one lattice step in the travel direction ([`gesture_shape`]).
    Glide { dir: i8 },
    /// A fast cursor run (a held arrow's coalesced echo or a multi-cell leap): the word-scale motion, a [`SoundKind::Glide`] repeated per character crossed and landing [`GESTURE_WORD_CHARS`] steps out ([`gesture_shape`]).
    Sweep { dir: i8 },
    /// The cursor landed: a bright in-key star chime over a soft arrival body, designed before palette dispatch and pitched through `melody_hz`.
    Land,
    /// The spacebar: the downbeat of the typing music.
    Space,
    /// A bare Shift keydown: the ting before a capital, cued once per physical press (never on auto-repeat, release or mid-chord).
    Shift,

    /// The keydown pre-cue for a navigation meteor (§15.2), minted before any move is observed; off by default, since a stray cue on a no-op move would be heard.
    MeteorArm {
        /// +1 rightward / upward, −1 leftward / downward.
        dir: i8,
    },
    /// A navigation move of at least [`crate::rainbow_kitty::timing::JUMP_MIN_CELLS`] cells (§12): tick, gliding core, whoosh, and the bell on the landing frame, `flight_ms(cells)` later.
    Meteor {
        /// +1 rightward / upward, −1 leftward / downward.
        dir: i8,
        /// Diagonal cell distance, the input to [`crate::rainbow_kitty::timing::flight_ms`].
        cells: u16,
        /// A [`SoundKind::MeteorArm`] preceded this and its voices are still live: claim them (re-target pan, `f1` and decays) rather than respawning, so one move never sounds two gestures.
        armed: bool,
    },
    /// A keyed Return with credit (§11): the cadence, never the brrrring.
    Enter {
        /// Diagonal cell distance of the Return's own flight.
        cells: u16,
    },
    /// A paste landed: the up-strum (§28), four tines of the music box's live chord swept upward at 0 / 22 / 44 / 66 ms.
    Strum,
    /// A hero star was born and the shared token bucket paid for it (§13): one glint.
    Stardust {
        /// The star's seeded scintillation rate in Hz, integer-stepped in `[8, 12]`, so the glint twinkles with the star it rides.
        twinkle_hz: u8,
    },
}

/// The sparkle-words gesture vocabulary (word-decoration events).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WordGesture {
    /// The curse-word bonk: the one discordant voice in the engine, a minor second plus tritone against the melody's current degree.
    Bonk,
}

/// The sing-along celebration gesture vocabulary (`crate::kitty_sing`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CelebrationGesture {
    /// One bar of the sing-along riff, pushed once per visual bar boundary while the celebration is armed (wind-down pushes none); `bar` counts bars since the arm.
    RiffBar { bar: u16, sig: u32 },
    /// The outro (§27): pushed once on the armed run's last bar line.
    Outro { sig: u32 },
}

impl CelebrationGesture {
    /// The canonical riff-bar gesture: bar index plus the held character's song signature.
    #[inline]
    #[must_use]
    pub const fn riff_bar(bar: u16, sig: u32) -> Self {
        Self::RiffBar { bar, sig }
    }

    /// Build the outro gesture for a run in `sig`'s key — the stable constructor, like [`Self::riff_bar`].
    #[inline]
    #[must_use]
    pub const fn outro(sig: u32) -> Self {
        Self::Outro { sig }
    }
}

/// The prism wake gesture vocabulary: the answer to program output (`crate::output_streak`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputGesture {
    /// The episode's one soft pip, on the quiet→active crossing: two sine partials on the tune's own pentatonic lattice, lowpassed to a whisper.
    Shimmer,
    /// The episode's closing exhale — the same voice, one partial, quieter still.
    Settle,
    /// The verdict: a command's exit code, sounded once on the OSC 133/633 `D` that carries it.
    Verdict {
        /// The exit status was non-zero.
        failed: bool,
        /// It ran long enough for the faraway ice bell (`CAD_BELL_*`).
        ice: bool,
    },
}

/// One namespaced gesture: which effect spoke (the variant) and what it said (the payload).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SoundGesture {
    /// Cursor-trail gestures, the founding vocabulary.
    Trail(SoundKind),
    /// Sparkle-words gestures.
    Words(WordGesture),
    /// Sing-along gestures (`crate::kitty_sing`'s celebration).
    Celebration(CelebrationGesture),
    /// Prism wake gestures: the program-output pip, its exhale and the verdict.
    Output(OutputGesture),
}

/// Which palette family speaks for an event: the host's `trail_sound_style` setting, carried per event like `gain` so a change applies on the next keystroke.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SoundVoice {
    /// Follow the visual trail style's palette.
    #[default]
    Style,
    /// One of the eight style palettes as a standalone instrument, chosen regardless of the look on screen.
    Of(GlowStyle),
    /// The mechanical-keyboard palette: click + thock percussion.
    Mech,
    /// The typewriter: a dry slug clack over the platen thud; on Enter the margin bell dings and the carriage zips home.
    Typewriter,
    /// The marimba: a warm rosewood bar under a yarn mallet, its two-octave glint dying first.
    Marimba,
    /// The felt piano: a felt-muted hammer thud under a dark harmonic tone — the hush you can type on all day.
    Felt,
    /// The music box: the rainbow kitty instrument (`docs/design/RAINBOW-KITTY-V2.md` §9-§13), the only voice whose events bypass the v1 admission chain ([`TrailSynth::push_v2`]).
    RainbowKittyV2,
}

impl SoundVoice {
    /// The roster, in picker order: `auto` (the default), the music box, the eight style palettes, the keyboard, then the sound-only instruments.
    pub const ALL: &[SoundVoice] = &[
        SoundVoice::Style,
        SoundVoice::RainbowKittyV2,
        SoundVoice::Of(GlowStyle::Lumen),
        SoundVoice::Of(GlowStyle::Sparkle),
        SoundVoice::Of(GlowStyle::Comet),
        SoundVoice::Of(GlowStyle::Water),
        SoundVoice::Of(GlowStyle::Phaser),
        SoundVoice::Of(GlowStyle::Laser),
        SoundVoice::Of(GlowStyle::Beam),
        SoundVoice::Of(GlowStyle::Fire),
        SoundVoice::Mech,
        SoundVoice::Typewriter,
        SoundVoice::Marimba,
        SoundVoice::Felt,
    ];

    /// Alias spellings accepted by [`SoundVoice::parse`] (config files, `settings set`) but not offered by the picker, e.g. the trail-style names (`trail_sound_style = "water"`) and `mechanical` variants.
    pub const ALIASES: &[(&str, SoundVoice)] = &[
        ("style", SoundVoice::Style),
        ("follow", SoundVoice::Style),
        ("default", SoundVoice::Style),
        ("glass bell", SoundVoice::RainbowKittyV2),
        ("bell", SoundVoice::RainbowKittyV2),
        ("bells", SoundVoice::RainbowKittyV2),
        ("kitty", SoundVoice::RainbowKittyV2),
        ("rainbow kitty", SoundVoice::RainbowKittyV2),
        ("rainbow kitty pet", SoundVoice::RainbowKittyV2),
        ("rainbow dog pet", SoundVoice::RainbowKittyV2),
        ("lumen", SoundVoice::Of(GlowStyle::Lumen)),
        ("lamplight", SoundVoice::Of(GlowStyle::Lumen)),
        ("pluck", SoundVoice::Of(GlowStyle::Lumen)),
        ("sparkle", SoundVoice::Of(GlowStyle::Sparkle)),
        ("chimes", SoundVoice::Of(GlowStyle::Sparkle)),
        ("comet", SoundVoice::Of(GlowStyle::Comet)),
        ("ice", SoundVoice::Of(GlowStyle::Comet)),
        ("chime", SoundVoice::Of(GlowStyle::Comet)),
        ("water", SoundVoice::Of(GlowStyle::Water)),
        ("drop", SoundVoice::Of(GlowStyle::Water)),
        ("raindrop", SoundVoice::Of(GlowStyle::Water)),
        ("plip", SoundVoice::Of(GlowStyle::Water)),
        ("phaser", SoundVoice::Of(GlowStyle::Phaser)),
        ("laser", SoundVoice::Of(GlowStyle::Laser)),
        ("lightning", SoundVoice::Of(GlowStyle::Laser)),
        ("beam", SoundVoice::Of(GlowStyle::Beam)),
        ("tube", SoundVoice::Of(GlowStyle::Beam)),
        ("fire", SoundVoice::Of(GlowStyle::Fire)),
        ("hearth", SoundVoice::Of(GlowStyle::Fire)),
        ("ember", SoundVoice::Of(GlowStyle::Fire)),
        ("mech", SoundVoice::Mech),
        ("thock", SoundVoice::Mech),
        ("mechanical keyboard", SoundVoice::Mech),
        ("keyboard", SoundVoice::Mech),
        ("clack", SoundVoice::Typewriter),
        ("wood", SoundVoice::Marimba),
        ("mallet", SoundVoice::Marimba),
        ("xylophone", SoundVoice::Marimba),
        ("felt piano", SoundVoice::Felt),
        ("piano", SoundVoice::Felt),
        ("hush", SoundVoice::Felt),
    ];

    /// The canonical spelling of each voice: the config value, the picker row and the introspection token.
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            SoundVoice::Style => "auto",
            SoundVoice::Of(GlowStyle::RainbowKitty) | SoundVoice::RainbowKittyV2 => "music box",
            SoundVoice::Of(GlowStyle::Lumen) => "warm pluck",
            SoundVoice::Of(GlowStyle::Sparkle) => "glitter",
            SoundVoice::Of(GlowStyle::Comet) => "ice chime",
            SoundVoice::Of(GlowStyle::Water) => "droplet",
            SoundVoice::Of(GlowStyle::Phaser | GlowStyle::Classic) => "pew",
            SoundVoice::Of(GlowStyle::Laser) => "zap",
            SoundVoice::Of(GlowStyle::Beam) => "tick",
            SoundVoice::Of(GlowStyle::Fire) => "crackle",
            SoundVoice::Mech => "mechanical",
            SoundVoice::Typewriter => "typewriter",
            SoundVoice::Marimba => "marimba",
            SoundVoice::Felt => "felt",
        }
    }

    /// Resolve a spelling (canonical [`SoundVoice::name`] or a [`SoundVoice::ALIASES`] entry, trimmed, ASCII-case-insensitive) to its voice; `None` otherwise.
    #[inline]
    #[must_use]
    pub const fn parse(token: &str) -> Option<Self> {
        let token = crate::cursor_glow::spellings::trim_whitespace_bytes(token);
        let mut i = 0;
        while i < Self::ALL.len() {
            let v = Self::ALL[i];
            if crate::cursor_glow::spellings::eq_ascii_ci(token, v.name()) {
                return Some(v);
            }
            i += 1;
        }
        let mut j = 0;
        while j < Self::ALIASES.len() {
            let (alias, voice) = Self::ALIASES[j];
            if crate::cursor_glow::spellings::eq_ascii_ci(token, alias) {
                return Some(voice);
            }
            j += 1;
        }
        None
    }
}

/// One sound trigger, built by the host at the trail-spawn edge (or the word-decoration drain) with the resolved user gain (motion-gated intensity × `trail_sound_volume` × per-source enables).
#[derive(Clone, Copy, Debug)]
pub struct SoundEvent {
    pub style: GlowStyle,
    /// Which palette family speaks — see [`SoundVoice`]; `Style` is the pre-override identity.
    pub voice: SoundVoice,
    pub kind: SoundGesture,
    /// Stereo position, -1 (hard left) .. 1 (hard right), mapped from the cursor column; the synth narrows it so nothing sits fully in one ear.
    pub pan: f32,
    /// Typing heat 0..1 (the glow engine's blaze).
    pub heat: f32,
    /// Style hue 0..1 where meaningful (phaser's live sweep hue) — mapped to melodic degree so the band's colour and pitch travel together.
    pub hue: f32,
    /// Final linear gain for this event, 0..1: user volume × any host-side gating.
    pub gain: f32,
    /// The host's inferred [`Tone`] of the typed line (the [`crate::tone`] classifier).
    pub tone: Tone,
    /// Whether this gesture may feed the ambient bed (the host's `trail_sound_bed` setting, default off).
    pub bed: bool,
    /// The glyph was shifted (a capital or shifted symbol): lifts this keystroke by [`SHIFT_GLYPH_LIFT`].
    pub shifted: bool,
}

impl SoundEvent {
    /// An event with every field set, for a test that needs one whole: it changes the fields it
    /// is about by struct update, so a field added later gets its value here and no test literal
    /// has to name every field (the trail-diff closure's tests among them, §15.0.6).
    #[cfg(test)]
    #[doc(hidden)]
    #[must_use]
    pub const fn sample_for_test() -> Self {
        Self {
            style: GlowStyle::RainbowKitty,
            voice: SoundVoice::Style,
            kind: SoundGesture::Trail(SoundKind::Typed),
            pan: 0.25,
            heat: 0.5,
            hue: 0.125,
            gain: 0.75,
            tone: Tone::Technical,
            bed: false,
            shifted: false,
        }
    }
}

/// Side-car scalars for the music box that [`SoundEvent`] does not carry (`docs/design/RAINBOW-KITTY-V2.md` §16).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EventMeta {
    /// The host input-clock stamp in milliseconds, on both the keyed and the observed-echo paths: the melody's one time source (the inter-key gap shapes each note).
    pub at_ms: u32,
    /// What kind of glyph landed (§16): a [`glyph_class`] id, filled at the keyed seam only by [`typed_glyph_class`].
    pub glyph_class: u8,
    /// The typed glyph's alphabet rank (§3.1), filled at the keyed seam only by [`typed_glyph_rank`]: the derived melody's one input about what was written.
    pub rank: u8,
    /// The pan this gesture starts at, −1..1: the meteor's origin column, where [`SoundEvent::pan`] is its destination.
    pub pan_from: f32,
    /// How far into the current audio block this cue arrived, in seconds.
    pub block_lead_s: f32,
    /// Flow heat, 0..1: how far into flow state the typist is, from the glow engine's flow counter (fast keys with no delete; a delete, a Kill or a slow key resets it).
    pub flow: f32,
}

impl Default for EventMeta {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl EventMeta {
    /// Zeroed event metadata.
    pub const EMPTY: Self = Self {
        at_ms: 0,
        glyph_class: 0,
        rank: 0,
        pan_from: 0.0,
        block_lead_s: 0.0,
        flow: 0.0,
    };
}

const BLOCK_LEAD_MAX_S: f32 = 0.050;

const ASCII_GLYPH_RANK: [u8; 128] = {
    let mut table = [45u8; 128];
    let mut b = 0usize;
    while b < 128 {
        let c = b as u8 as char;
        table[b] = if c.is_ascii_alphabetic() {
            1 + (c.to_ascii_lowercase() as u8 - b'a')
        } else if c.is_ascii_digit() {
            27 + (c as u8 - b'0')
        } else {
            match c {
                '.' | ',' => 37,
                ';' | ':' => 38,
                '\'' | '"' => 39,
                '!' | '?' => 40,
                '-' | '_' | '+' | '=' => 41,
                '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' => 42,
                '/' | '\\' | '|' => 43,
                c if c.is_ascii_punctuation() => 44,
                _ => 45,
            }
        };
        b += 1;
    }
    table
};

/// [`EventMeta::rank`] for a typed character: the one producer of the melody's alphabet order.
#[must_use]
#[inline]
pub const fn typed_glyph_rank(typed: Option<char>) -> u8 {
    let Some(c) = typed else { return 0 };
    if c.is_ascii() {
        ASCII_GLYPH_RANK[c as usize]
    } else {
        45
    }
}

/// Names for [`EventMeta::glyph_class`], produced by [`typed_glyph_class`].
pub mod glyph_class {
    /// Letters, the space, an IME run, anything non-ASCII, and "no key".
    pub const LETTER: u8 = 0;
    /// `?` — the question that rises.
    pub const QMARK: u8 = 1;
    /// `!`: struck forte as a tine, landing on a C or a G.
    pub const BANG: u8 = 2;
    /// `0`..`9` — the wood bar that counts.
    pub const DIGIT: u8 = 3;
    /// `.`: the full stop.
    pub const STOP: u8 = 4;
    /// Opening brackets (`()`, `[]`, `{}`) — not a pluck: they open.
    pub const OPEN: u8 = 5;
    /// Closing brackets (`()`, `[]`, `{}`).
    pub const CLOSE: u8 = 6;
    /// Single, double, and backtick quote marks.
    pub const QUOTE: u8 = 7;
    /// `_` `~` `\` `|`: a line drawn.
    pub const LINE: u8 = 8;
    /// `/` — the slash that rises.
    pub const RISE: u8 = 9;
    /// `+` `=` `*` `%` `^` `<` `>` — the operators; `<` `>` are comparisons here, not an open/close pair.
    pub const MATH: u8 = 10;
    /// `@` `#` `$` `&` — every other ASCII mark.
    pub const SIGIL: u8 = 11;
    /// `,`: a half cadence.
    pub const COMMA: u8 = 12;
    /// `;` — the semicolon.
    pub const SEMI: u8 = 13;
    /// `:` — the colon.
    pub const COLON: u8 = 14;
    /// `-` — the dash.
    pub const DASH: u8 = 15;
    /// A key typed into a password prompt, not a glyph: [`super::typed_glyph_class`] never returns it.
    pub const SECRET: u8 = 16;
}

const ASCII_GLYPH_CLASS: [u8; 128] = {
    use glyph_class::*;
    let mut table = [LETTER; 128];
    let mut b = 0usize;
    while b < 128 {
        let c = b as u8 as char;
        table[b] = match c {
            '?' => QMARK,
            '!' => BANG,
            c if c.is_ascii_digit() => DIGIT,
            '.' => STOP,
            ',' => COMMA,
            ';' => SEMI,
            ':' => COLON,
            '(' | '[' | '{' => OPEN,
            ')' | ']' | '}' => CLOSE,
            '\'' | '"' | '`' => QUOTE,
            '-' => DASH,
            '_' | '~' | '\\' | '|' => LINE,
            '/' => RISE,
            '+' | '=' | '*' | '%' | '^' | '<' | '>' => MATH,
            c if c.is_ascii_punctuation() => SIGIL,
            _ => LETTER,
        };
        b += 1;
    }
    table
};

/// [`EventMeta::glyph_class`] for a typed character: the one producer of the [`glyph_class`] table, shared by the host seam, the bench and the tests.
#[must_use]
#[inline]
pub const fn typed_glyph_class(typed: Option<char>) -> u8 {
    use glyph_class::*;
    let Some(c) = typed else { return LETTER };
    if c.is_ascii() {
        ASCII_GLYPH_CLASS[c as usize]
    } else {
        LETTER
    }
}

const PENTA: [f32; 5] = [1.0, 1.125, 1.25, 1.5, 1.666_666_7];

#[inline]
const fn pow2i(oct: i32) -> f32 {
    if oct < -126 {
        0.0
    } else if oct > 127 {
        f32::INFINITY
    } else {
        f32::from_bits(((127 + oct) as u32) << 23)
    }
}

#[inline]
const fn penta(base: f32, degree: i32) -> f32 {
    let oct = degree.div_euclid(5);
    let step = (degree - oct * 5) as usize;
    base * PENTA[step] * pow2i(oct)
}

const PENTA_MINOR: [f32; 5] = [1.0, 1.2, 1.333_333_3, 1.5, 1.8];

const PENTA_YO: [f32; 5] = [1.0, 1.125, 1.333_333_3, 1.5, 1.666_666_7];

const EXCITED_TRANSPOSE: f32 = 1.125;

#[inline]
const fn tone_tables(tone: Tone) -> (&'static [f32; 5], f32) {
    match tone {
        Tone::Technical | Tone::Calm => (&PENTA, 1.0),
        Tone::Excited => (&PENTA, EXCITED_TRANSPOSE),
        Tone::Frustrated => (&PENTA_MINOR, 1.0),
        Tone::Playful => (&PENTA_YO, 1.0),
    }
}

#[inline]
const fn tone_feel(tone: Tone) -> f32 {
    match tone {
        Tone::Technical | Tone::Frustrated => 1.0,
        Tone::Calm => 1.06,
        Tone::Excited => 0.88,
        Tone::Playful => 0.94,
    }
}

const SONG_ACCENT: i8 = i8::MIN;

const SONG_PULSE: [i8; 12] = [
    SONG_ACCENT,
    -5,
    -3,
    SONG_ACCENT,
    -5,
    2,
    SONG_ACCENT,
    -5,
    -3,
    SONG_ACCENT,
    -5,
    2,
];

const SONG_GHOST_LEVEL: f32 = 0.55;
const SONG_GHOST_FEEL: f32 = 0.62;

const PHRASE_PAUSE_S: f32 = 0.6;

const SONG_THEME: [i8; 28] = [
    0, 2, 4, 5, 4, 2, 4, 7, 5, 7, 5, 4, 2, 4, 2, 0, 2, 4, 7, 5, 3, 2, 0, 2, 4, 5, 7, 8,
];

const SONG_FORM: [u8; 5] = [0, 8, 16, 22, 28];

const SONG_BASS: [i8; 12] = [0, 2, 1, 3, 0, 2, 4, 1, 0, 2, 3, 4];

#[inline]
const fn tone_register(tone: Tone) -> (i32, i32) {
    match tone {
        Tone::Excited => (0, 9),
        Tone::Frustrated => (-1, 8),
        _ => (0, 8),
    }
}

#[inline]
const fn fold_register(v: i32, lo: i32, hi: i32) -> i32 {
    let span = hi - lo;
    if span <= 0 {
        return lo;
    }
    let m = (v - lo).rem_euclid(2 * span);
    lo + if m <= span { m } else { 2 * span - m }
}

#[inline]
const fn melody_lean(tone: Tone) -> i32 {
    match tone {
        Tone::Excited => 1,
        Tone::Frustrated => -1,
        _ => 0,
    }
}

const CURSOR_ANCHOR_HZ: f32 = 330.0;

const CURSOR_SWEEP_STEP_S: f32 = 0.055;

const GESTURE_CHAR_STEP: i32 = 1;

const GESTURE_WORD_CHARS: i32 = 3;

const CURSOR_SWEEP_RUN: usize = GESTURE_WORD_CHARS as usize + 1;

const GESTURE_BEND: f32 = 1.125;
const GESTURE_BEND_TAU: f32 = 0.012;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct GestureShape {
    dir: i8,
    notes: usize,
    step: i32,
    offset: i32,
}

#[inline]
const fn shift_lift(ev: &SoundEvent, kind: SoundKind) -> i32 {
    if matches!(kind, SoundKind::Typed) && ev.shifted {
        SHIFT_GLYPH_LIFT
    } else {
        0
    }
}

#[inline]
const fn shift_gain(ev: &SoundEvent, kind: SoundKind) -> f32 {
    if matches!(kind, SoundKind::Typed) && ev.shifted {
        SHIFT_GLYPH_GAIN
    } else {
        1.0
    }
}

const SHIFT_GLYPH_RATIO: f32 =
    crate::color_math::powf_pos_f32(2.0, SHIFT_GLYPH_LIFT as f32 / PENTA.len() as f32);

#[inline]
const fn shift_ratio(ev: &SoundEvent, kind: SoundKind) -> f32 {
    if shift_lift(ev, kind) != 0 {
        SHIFT_GLYPH_RATIO
    } else {
        1.0
    }
}

#[inline]
const fn gesture_shape(kind: SoundKind) -> GestureShape {
    match kind {
        SoundKind::Shift => GestureShape {
            dir: 1,
            notes: 1,
            step: 0,
            offset: GESTURE_CHAR_STEP,
        },
        SoundKind::Glide { dir } => GestureShape {
            dir,
            notes: 1,
            step: 0,
            offset: (dir as i32) * GESTURE_CHAR_STEP,
        },
        SoundKind::Sweep { dir } => GestureShape {
            dir,
            notes: CURSOR_SWEEP_RUN,
            step: (dir as i32) * GESTURE_CHAR_STEP,
            offset: 0,
        },
        _ => GestureShape {
            dir: 1,
            notes: 1,
            step: 0,
            offset: 0,
        },
    }
}

#[inline]
const fn fold_octave(f: f32, lo: f32) -> f32 {
    let mut f = f.clamp(20.0, 20_000.0);
    while f >= lo * 2.0 {
        f *= 0.5;
    }
    while f < lo {
        f *= 2.0;
    }
    f
}

#[inline]
const fn bass_octave(anchor: f32) -> f32 {
    fold_octave(anchor, SPACE_BASS_LO_HZ)
}

#[inline]
const fn gesture_bend(f: f32, dir: i8) -> (f32, f32) {
    const INV_GESTURE_BEND: f32 = 1.0 / GESTURE_BEND;
    if dir >= 0 {
        (f * INV_GESTURE_BEND, f)
    } else {
        (f * GESTURE_BEND, f)
    }
}

const BONK_MINOR_SECOND: f32 = 16.0 / 15.0;
const BONK_TRITONE: f32 = 45.0 / 32.0;

const TYPED_KIND_GAIN: f32 = 1.0;
const BACKSPACE_KIND_GAIN: f32 = 0.85;
const SPACE_KIND_GAIN: f32 = 0.9;
const SHIFT_KIND_GAIN: f32 = 0.9;
const METEOR_ARM_KIND_GAIN: f32 = 0.5;
const SHIFT_GLYPH_GAIN: f32 = 1.35;
const DETUNE_MAX_RATIO: f32 = 1.03;
const OUTPUT_PIP_KIND_GAIN: f32 = 0.34;
const OUTPUT_SETTLE_KIND_GAIN: f32 = 0.22;

const SHIFT_GLYPH_LIFT: i32 = 5;

const SHIFT_ROTATION: [i32; 5] = [0, 2, 4, 1, 3];

const RELEASE_RAMP_S: f32 = 0.005;
const RING_OUT_PER_TAU: f32 = 5.0;
const SHIFT_TING_LO_HZ: f32 = 1318.5;
const SHIFT_TING_ATTACK_S: f32 = 0.003;
const SHIFT_TING_DECAY_S: f32 = 0.085;
const SHIFT_TING_DUR_S: f32 = RING_OUT_PER_TAU * SHIFT_TING_DECAY_S + RELEASE_RAMP_S;
const SHIFT_TING_GLINT: f32 = 1.3;
const SHIFT_TING_GLINT_TAU_S: f32 = 0.018;
const SHIFT_TING_OCTAVE_LEVEL: f32 = 0.14;
const SHIFT_TING_VOICE_LEVEL: f32 = 0.33;

#[inline]
const fn ting_octave(f: f32) -> f32 {
    fold_octave(f, SHIFT_TING_LO_HZ)
}

const POOF_BODY_HZ: f32 = 2800.0;
const POOF_BODY_Q: f32 = 0.55;
const POOF_BODY_ATTACK_S: f32 = 0.002;
const POOF_BODY_DECAY_S: f32 = 0.022;
const POOF_BODY_DUR_S: f32 = 0.055;
const POOF_AIR_HZ: f32 = 5000.0;
const POOF_AIR_Q: f32 = 0.65;
const POOF_AIR_DELAY_S: f32 = 0.006;
const POOF_AIR_ATTACK_S: f32 = 0.0035;
const POOF_AIR_DECAY_S: f32 = 0.030;
const POOF_AIR_DUR_S: f32 = 0.080;
const POOF_AIR_LEVEL: f32 = 0.7;
const POOF_VOICE_GAIN: f32 = 1.28;
const POOF_LP_CUT_HZ: f32 = 7000.0;
const POOF_CLOUD_DUR_S: f32 = 0.11;
const POOF_CLOUD_ATTACK_S: f32 = 0.008;
const POOF_CLOUD_DECAY_S: f32 = 0.055;
const POOF_CLOUD_SETTLE_HZ: f32 = 3600.0;
const POOF_CLOUD_GLIDE_S: f32 = 0.06;
const POOF_CLOUD_GAIN: f32 = 0.85;
const POOF_AIR_SETTLE_HZ: f32 = 4100.0;
const POOF_AIR_SETTLE_GLIDE_S: f32 = 0.05;
const HELD_ERASE_RUN_WINDOW: f32 = 0.14;
const HELD_ERASE_RUN_MIN: u32 = 4;
const ERASE_SHIMMER_DELAY_S: f32 = 0.16;
const ERASE_SHIMMER_DUR_S: f32 = 0.26;
const ERASE_SHIMMER_ATTACK_S: f32 = 0.030;
const ERASE_SHIMMER_DECAY_S: f32 = 0.110;
const ERASE_SHIMMER_HZ0: f32 = 5200.0;
const ERASE_SHIMMER_HZ1: f32 = 7200.0;
const ERASE_SHIMMER_GLIDE_S: f32 = 0.09;
const ERASE_SHIMMER_Q: f32 = 1.1;
const ERASE_SHIMMER_TW_RATE: f32 = 19.0;
const ERASE_SHIMMER_TW_DEPTH: f32 = 0.5;
const ERASE_SHIMMER_LP_HZ: f32 = 8500.0;
const ERASE_SHIMMER_GAIN: f32 = 0.5;
const ERASE_SHIMMER_DAMP_S: f32 = 0.012;
const WORD_POOF_BODY_HZ: f32 = 2300.0;
const WORD_POOF_BODY_Q: f32 = 0.5;
const WORD_POOF_BODY_ATTACK_S: f32 = 0.0035;
const WORD_POOF_BODY_DECAY_S: f32 = 0.040;
const WORD_POOF_BODY_DUR_S: f32 = 0.085;
const WORD_POOF_AIR_HZ: f32 = 4400.0;
const WORD_POOF_AIR_SETTLE_HZ: f32 = 3500.0;
const WORD_POOF_AIR_GLIDE_S: f32 = 0.07;
const WORD_POOF_AIR_Q: f32 = 0.6;
const WORD_POOF_AIR_DELAY_S: f32 = 0.010;
const WORD_POOF_AIR_ATTACK_S: f32 = 0.006;
const WORD_POOF_AIR_DECAY_S: f32 = 0.070;
const WORD_POOF_AIR_DUR_S: f32 = 0.150;
const WORD_POOF_AIR_LEVEL: f32 = 0.8;
const WORD_POOF_GAIN: f32 = 1.1;
const ERASE_MIN_GAP: f32 = 0.075;

const SHIFT_MIN_GAP: f32 = 0.075;
const SPACE_BASS_LO_HZ: f32 = 110.0;
const SPACE_ATTACK_S: f32 = 0.010;
const SPACE_DECAY_S: f32 = 0.052;
const SPACE_DUR_S: f32 = 0.092;
const SPACE_OCTAVE_LEVEL: f32 = 0.20;
const SPACE_VOICE_LEVEL: f32 = 0.32;
const SPACE_DAMP_S: f32 = 0.012;
const SPACE_FIFTH_LEVEL: f32 = 0.07;

const SPACE_BELL_GLINT: f32 = 0.9;
const SPACE_BELL_TINK: f32 = 0.12;
const SPACE_BREATHE_ATTACK_S: f32 = 0.014;
const SPACE_BREATHE_DECAY_S: f32 = 0.085;
const SPACE_BREATHE_DUR_S: f32 = 0.150;
const SPACE_BREATHE_OCTAVE_LEVEL: f32 = 0.24;
const SPACE_BREATHE_FIFTH_LEVEL: f32 = 0.10;
const SPACE_RUN_BREATH_LEVEL: f32 = 0.35;
const SPACE_STEP_LEVEL: f32 = 0.6;
const SPACE_STEP_MIN_GAP: f32 = 0.075;
const SPACE_STEP_ATTACK_S: f32 = 0.004;
const SPACE_STEP_DECAY_S: f32 = 0.040;
const SPACE_STEP_DUR_S: f32 = 0.100;
const SPACE_STEP_GLINT: f32 = 0.7;
const SPACE_STEP_OCTAVE_LEVEL: f32 = 0.18;
const SPACE_STEP_HI_HZ: f32 = SPACE_BASS_LO_HZ * 8.0;
const SPACE_AIR_LEVEL: f32 = 0.9;
const SPACE_TWINKLE_RATIO: f32 = 16.0;
const SPACE_TWINKLE_DETUNE: f32 = 0.030;
const SPACE_TWINKLE: f32 = 0.14;
const SPACE_TWINKLE_TWIN: f32 = 0.09;
const SPACE_TWINKLE_RATE: f32 = 24.0;
const SPACE_TWINKLE_DEPTH: f32 = 0.55;
const SPACE_TWINKLE_DUR_S: f32 = 0.105;
const SPACE_TWINKLE_DECAY_S: f32 = 0.038;
const SPACE_TWINKLE_LO_HZ: f32 = SPACE_BASS_LO_HZ * 40.0;

#[inline]
const fn twinkle_hz(root: f32) -> f32 {
    fold_octave(root * SPACE_TWINKLE_RATIO, SPACE_TWINKLE_LO_HZ)
}
const GLIDE_KIND_GAIN: f32 = 0.78;
const NAVIGATION_KIND_GAIN: f32 = 0.68;
const SWEEP_KIND_GAIN: f32 = 0.692;
const KILL_KIND_GAIN: f32 = 1.25;
const KILLWORD_KIND_GAIN: f32 = 1.0;
const POOF_KIND_GAIN: f32 = 0.68;
const JUMP_KIND_GAIN: f32 = 1.25;

const BONK_KIND_GAIN: f32 = 1.35;

const LAND_ANCHOR_HZ: f32 = 784.0;
const LAND_KIND_GAIN: f32 = 0.865;
const LAND_STARS: usize = 3;
const LAND_STAR_STEP_S: f32 = 0.034;
const LAND_STAR_DEGREE_STEP: i32 = 2;

const BONK_DUCK_DEPTH: f32 = 0.55;
const BONK_DUCK_TAU: f32 = 0.28;

const ENV_REANCHOR: u32 = 64;

/// One celebration riff bar in seconds: 4 beats at the sing-along's 150 BPM.
pub const CELEBRATION_BAR_SECONDS: f32 = 1.6;

const CELEBRATION_EIGHTH: f32 = CELEBRATION_BAR_SECONDS / 8.0;

/// The outro's length (§27): three beats at the sing-along tempo.
pub const CELEBRATION_OUTRO_S: f32 = 1.2;

const _: () = assert!(CELEBRATION_BAR_SECONDS == crate::kitty_sing::SING_BAR_SECONDS);
const _: () = assert!(CELEBRATION_EIGHTH == crate::kitty_sing::SING_BEAT_SECONDS / 2.0);
const _: () = assert!(CELEBRATION_OUTRO_S == crate::kitty_sing::OUTRO_SECONDS);
const _: () = assert!(
    CELEBRATION_OUTRO_S < CELEBRATION_BAR_SECONDS,
    "the outro is shorter than a bar: it ends the song, it is not one more bar"
);
const _: () = assert!(
    (7.0 + CELEBRATION_SWING) * CELEBRATION_EIGHTH < CELEBRATION_BAR_SECONDS,
    "the last swung eighth starts inside its bar"
);
const _: () = assert!(
    (u16::MAX as usize + 1).is_multiple_of(CELEBRATION_PHRASE_BARS),
    "the u16 bar counter wraps on a phrase boundary"
);

const CELEBRATION_OUTRO_DECAY_S: f32 = 0.34;

const CELEBRATION_OUTRO_SUB_DEG: i32 = -10;

const REST: i32 = i32::MIN;

const CELEBRATION_PHRASE_BARS: usize = 8;

const CELEBRATION_PHRASE: [[i32; 8]; CELEBRATION_PHRASE_BARS] = [
    [0, 2, 4, 5, 4, 2, 4, 7],
    [5, 7, 5, 4, 2, 4, 2, 0],
    [2, 4, REST, 7, 5, REST, 3, 2],
    [0, 2, 4, 5, 7, REST, 8, REST],
    [5, REST, 7, 8, 9, 7, 5, 4],
    [9, 8, 7, 5, 7, 5, 4, 2],
    [0, REST, 2, REST, 4, REST, 2, REST],
    [4, 5, 7, 8, 9, 7, REST, REST],
];

const CELEBRATION_FILL: [i32; 4] = [7, 5, 4, 2];

const CELEBRATION_BASS: [[i32; 4]; CELEBRATION_PHRASE_BARS] = [
    [-10, REST, -7, REST],
    [-8, REST, -10, REST],
    [-9, REST, -6, REST],
    [-7, REST, -7, REST],
    [-10, -10, -7, -7],
    [-8, -8, -9, -9],
    [-10, REST, -6, REST],
    [-7, -7, -9, REST],
];

const CELEBRATION_GROOVE: [f32; 8] = [0.55, 0.33, 0.44, 0.36, 0.50, 0.33, 0.46, 0.38];

const CELEBRATION_BAR_LIFT: [f32; CELEBRATION_PHRASE_BARS] =
    [1.00, 0.96, 1.02, 1.00, 1.12, 1.08, 0.86, 1.06];

const CELEBRATION_SWING: f32 = 0.14;

const CELEBRATION_BUILD_BARS: f32 = 6.0;

const CELEBRATION_CLAP_BAR: u16 = 8;

const CELEBRATION_BASE_HZ: f32 = 523.25;

const CELEBRATION_KIND_GAIN: f32 = 0.6;

const CELEBRATION_DAMP_S: f32 = 0.05;

const SONG_INTERVALS: [i32; 4] = [-1, 1, -2, 2];

const SONG_CODED_STEPS: u32 = 16;
const SONG_STEPS: u32 = SONG_CODED_STEPS + 1;

#[inline]
const fn song_degrees(sig: u32) -> [i32; SONG_STEPS as usize] {
    let mut degrees = [0i32; SONG_STEPS as usize];
    let mut code = sig as u64;
    let first = (code % 5) as i32;
    let mut degree = first;
    degrees[0] = degree;
    code /= 5;
    let mut i = 1usize;
    while i < SONG_CODED_STEPS as usize {
        let choice = (code & 3) as usize;
        code >>= 2;
        degree = (degree + SONG_INTERVALS[choice]).rem_euclid(5);
        degrees[i] = degree;
        i += 1;
    }
    let last = degree;
    let mut j = 0;
    while j < SONG_INTERVALS.len() {
        let bridge = (last + SONG_INTERVALS[j]).rem_euclid(5);
        if bridge != first {
            degrees[SONG_CODED_STEPS as usize] = bridge;
            break;
        }
        j += 1;
    }
    degrees
}

#[inline]
const fn celebration_root(sig: u32) -> i32 {
    (sig % 5) as i32 - 2
}

const MODE_ROTATIONS: [i32; 3] = [0, 2, -1];
#[inline]
const fn celebration_mode(sig: u32) -> i32 {
    MODE_ROTATIONS[(sig % 3) as usize]
}

const CELEBRATION_LEAD_DUTY: f32 = 0.25;

const CELEBRATION_VERSE: [bool; CELEBRATION_PHRASE_BARS] =
    [true, true, false, true, true, true, false, false];

#[inline]
const fn verse_register(idx: usize) -> i32 {
    if idx == 4 || idx == 5 { 4 } else { 1 }
}

const VERSE_ORDINAL_BASE: [Option<u32>; CELEBRATION_PHRASE_BARS] = {
    let mut table = [None; CELEBRATION_PHRASE_BARS];
    let mut base = 0u32;
    let mut i = 0;
    while i < CELEBRATION_PHRASE_BARS {
        if CELEBRATION_VERSE[i] {
            table[i] = Some(base);
            let row = &CELEBRATION_PHRASE[i];
            let mut j = 0;
            while j < row.len() {
                if row[j] != REST {
                    base += 1;
                }
                j += 1;
            }
        }
        i += 1;
    }
    table
};

#[inline]
const fn verse_ordinal_base(idx: usize) -> Option<u32> {
    VERSE_ORDINAL_BASE[idx]
}

#[inline]
const fn celebration_bar_degrees(sig: u32, idx: usize) -> [i32; 8] {
    let mut row = CELEBRATION_PHRASE[idx];
    let Some(base) = verse_ordinal_base(idx) else {
        return row;
    };
    let degrees = song_degrees(sig);
    let shelf = verse_register(idx);
    let mut ordinal = base;
    let mut i = 0;
    while i < row.len() {
        if row[i] != REST {
            row[i] = degrees[(ordinal % SONG_STEPS) as usize] + shelf;
            ordinal += 1;
        }
        i += 1;
    }
    row
}

#[inline]
const fn celebration_slot_delay(slot: usize) -> f32 {
    let swing = if slot.is_multiple_of(2) {
        0.0
    } else {
        CELEBRATION_SWING
    };
    (slot as f32 + swing) * CELEBRATION_EIGHTH
}

#[inline]
const fn celebration_sway(pan: f32, deg: i32) -> f32 {
    pan + 0.06 * (deg - 4) as f32
}

#[inline]
const fn celebration_lead(hz: f32, shimmer: f32, third: Partial) -> [Partial; 3] {
    [
        Partial {
            lvl: 0.55,
            f0: hz,
            f1: hz,
            wave: Wave::Pulse {
                duty: CELEBRATION_LEAD_DUTY,
            },
            ..Partial::DEFAULT
        },
        Partial {
            lvl: shimmer,
            f0: hz * 2.0,
            f1: hz * 2.0,
            wave: Wave::Pulse { duty: 0.5 },
            ..Partial::DEFAULT
        },
        third,
    ]
}

#[inline]
const fn celebration_span(phrase: &[i32; 8], i: usize, limit: usize) -> usize {
    let mut span = 1usize;
    while i + span < limit && phrase[i + span] == REST {
        span += 1;
    }
    span
}

const SING_DUCK_DEPTH: f32 = 0.65;
const SING_DUCK_TAU: f32 = 0.40;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
enum Wave {
    #[default]
    Sine,
    Pulse {
        duty: f32,
    },
}

#[derive(Clone, Copy, Default, Debug)]
struct Partial {
    lvl: f32,
    f0: f32,
    f1: f32,
    glide: f32,
    fm_ratio: f32,
    fm_i0: f32,
    fm_tau: f32,
    wave: Wave,
    decay: f32,
    ph: f32,
    fm_ph: f32,
    g_e: f32,
    k_g: f32,
    fm_e: f32,
    k_f: f32,
    p_e: f32,
    k_p: f32,
}

impl Partial {
    pub(super) const DEFAULT: Self = Self {
        lvl: 0.0,
        f0: 0.0,
        f1: 0.0,
        glide: 0.0,
        fm_ratio: 0.0,
        fm_i0: 0.0,
        fm_tau: 0.0,
        wave: Wave::Sine,
        decay: 0.0,
        ph: 0.0,
        fm_ph: 0.0,
        g_e: 0.0,
        k_g: 0.0,
        fm_e: 0.0,
        k_f: 0.0,
        p_e: 0.0,
        k_p: 0.0,
    };
}

#[derive(Clone, Copy, Default, Debug)]
struct Voice {
    on: bool,
    t: f32,
    delay: f32,
    dur: f32,
    gl: f32,
    gr: f32,
    attack: f32,
    decay: f32,
    p: [Partial; 3],
    n_lvl: f32,
    n_f0: f32,
    n_f1: f32,
    n_glide: f32,
    n_q: f32,
    n_lp: f32,
    n_bp: f32,
    env_a: f32,
    env_d: f32,
    k_a: f32,
    k_d: f32,
    n_e: f32,
    k_n: f32,
    env_run: bool,
    env_n: u32,
    tw_rate: f32,
    tw_depth: f32,
    tw_ph: f32,
    lp_cut: f32,
    lp: f32,
    lp_k: f32,
    n_damp: f32,
    svf_g: f32,
    svf_den: f32,
    duck_exempt: bool,
    celebration: bool,
    bass: bool,
    shimmer: bool,
    damp: f32,
    damp0: f32,

    n_decay: f32,
    nd_e: f32,
    k_nd: f32,
    pan1: f32,
    gl1: f32,
    gr1: f32,
    pan_glide_s: f32,
    pan_k: f32,
    pan_t0: f32,
    lane: u8,
    born: u32,
    arm_ttl: f32,
}

impl Voice {
    pub(super) const DEFAULT: Self = Self {
        on: false,
        t: 0.0,
        delay: 0.0,
        dur: 0.0,
        gl: 0.0,
        gr: 0.0,
        attack: 0.0,
        decay: 0.0,
        p: [Partial::DEFAULT; 3],
        n_lvl: 0.0,
        n_f0: 0.0,
        n_f1: 0.0,
        n_glide: 0.0,
        n_q: 0.0,
        n_lp: 0.0,
        n_bp: 0.0,
        env_a: 0.0,
        env_d: 0.0,
        k_a: 0.0,
        k_d: 0.0,
        n_e: 0.0,
        k_n: 0.0,
        env_run: false,
        env_n: 0,
        tw_rate: 0.0,
        tw_depth: 0.0,
        tw_ph: 0.0,
        lp_cut: 0.0,
        lp: 0.0,
        lp_k: 0.0,
        n_damp: 0.0,
        svf_g: 0.0,
        svf_den: 0.0,
        duck_exempt: false,
        celebration: false,
        bass: false,
        shimmer: false,
        damp: 0.0,
        damp0: 0.0,
        n_decay: 0.0,
        nd_e: 0.0,
        k_nd: 0.0,
        pan1: 0.0,
        gl1: 0.0,
        gr1: 0.0,
        pan_glide_s: 0.0,
        pan_k: 0.0,
        pan_t0: 0.0,
        lane: 0,
        born: 0,
        arm_ttl: 0.0,
    };

    #[inline]
    const fn weight(&self) -> f32 {
        if !self.on {
            return -1.0;
        }
        let env = crate::color_math::exp_f32(-self.t / self.decay.max(1e-3));
        (self.gl + self.gr) * env * (self.p[0].lvl + self.p[1].lvl + self.p[2].lvl + self.n_lvl)
    }
}

#[derive(Clone, Copy, Default, Debug)]
struct Bed {
    energy: f32,
    level: f32,
    gain: f32,
    out_lvl: f32,
    hp_m: f32,
    hp_s: f32,
    ph1: f32,
    ph2: f32,
    ph3: f32,
    lfo1: f32,
    lfo2: f32,
    lp1: f32,
    lp2: f32,
    timer: f32,
    droop: f32,
    var_ph: [f32; 4],
    var_f: [f32; 4],
    hue_s: f32,
    sky_glide_dt: f32,
    sky_glide: f32,
    sky_detune_arc: f32,
    sky_detune: f32,
    hp_dt: f32,
    hp_k: f32,
}

impl Bed {
    const DEFAULT: Self = Self {
        energy: 0.0,
        level: 0.0,
        gain: 0.0,
        out_lvl: 0.0,
        hp_m: 0.0,
        hp_s: 0.0,
        ph1: 0.0,
        ph2: 0.0,
        ph3: 0.0,
        lfo1: 0.0,
        lfo2: 0.0,
        lp1: 0.0,
        lp2: 0.0,
        timer: 0.0,
        droop: 0.0,
        var_ph: [0.0; 4],
        var_f: [0.0; 4],
        hue_s: 0.0,
        sky_glide_dt: 0.0,
        sky_glide: 0.0,
        sky_detune_arc: 0.0,
        sky_detune: 0.0,
        hp_dt: 0.0,
        hp_k: 0.0,
    };
    #[inline]
    const fn lfo_u1(&self) -> f32 {
        0.5 * (1.0 + sin01(self.lfo1))
    }

    #[inline]
    const fn lfo_u2(&self) -> f32 {
        0.5 * (1.0 + sin01(self.lfo2))
    }
}

#[inline]
const fn bed_kick(kind: SoundKind) -> f32 {
    match kind {
        SoundKind::Jump | SoundKind::Kill | SoundKind::Land => 0.5,
        SoundKind::KillWord => 0.4,
        SoundKind::Typed | SoundKind::Backspace | SoundKind::Space => 0.3,
        SoundKind::Navigation
        | SoundKind::Glide { .. }
        | SoundKind::Sweep { .. }
        | SoundKind::Shift => 0.12,
        SoundKind::Poof
        | SoundKind::MeteorArm { .. }
        | SoundKind::Meteor { .. }
        | SoundKind::Enter { .. }
        | SoundKind::Stardust { .. } => 0.0,
        SoundKind::Strum => 0.3,
    }
}

/// Procedural synthesizer: [`push`](Self::push) [`SoundEvent`]s in, [`render`](Self::render) interleaved stereo `f32` frames out.
#[derive(Clone, Debug)]
pub struct TrailSynth {
    #[cfg(test)]
    pub(crate) ting_unducked: bool,
    inv_sr: f32,
    rng: u32,
    voices: [Voice; MAX_VOICES],
    bed: Bed,
    bed_style: GlowStyle,
    bed_voice: SoundVoice,
    rate: f32,
    since_voice: f32,
    since_event: f32,
    block_lead_s: f32,
    since_erase: f32,
    since_shift: f32,
    erase_run: u32,
    space_run: bool,
    space_head: bool,
    walk: i32,
    theme_pos: u8,
    phrase_idx: u8,
    bass_step: u8,
    shift_step: u8,
    space_run_len: u8,
    space_run_deg: i32,
    space_run_root_hz: f32,
    since_space_step: f32,
    song_pulse: u8,
    song_accent: bool,
    song_ghost: i8,
    song_notes: u32,
    song_feel: f32,
    shift_octave: f32,
    tone: Tone,
    duck: f32,
    sing: f32,
    song_key: i8,
    sing_hold: f32,
    last_riff_sig: Option<u32>,
    steals: u32,
    #[cfg(test)]
    lane_births: [u32; 16],
    v2: MelodyV2,
    born_seq: u32,
    clock_s: f64,
    v2_latched: bool,
    v2_key_pending: bool,
    lim_env_l: f32,
    lim_g_l: f32,
    lim_env_r: f32,
    lim_g_r: f32,
    dc_x_l: f32,
    dc_y_l: f32,
    dc_x_r: f32,
    dc_y_r: f32,
}

const MASTER: f32 = 2.0;

const PAN_LAW_SCALE: f32 = 0.35;
const PAN_LAW_CLAMP: f32 = 0.6;

#[inline]
const fn pan_gains(gain: f32, pan: f32) -> (f32, f32) {
    let p = (pan * PAN_LAW_SCALE).clamp(-PAN_LAW_CLAMP, PAN_LAW_CLAMP);
    let a = (p + 1.0) * core::f32::consts::FRAC_PI_4;
    let (s, c) = crate::supernova::sin_cos_f32(a);
    (gain * c, gain * s)
}

const LIMIT_THRESHOLD: f32 = 0.2;
const LIMIT_ATTACK_S: f32 = 0.000_5;
const LIMIT_RELEASE_S: f32 = 0.080;

/// Governor: sustained admission gap for discrete voices, per event kind pressure. ~45 ms ⇒ at most ~22 voices/s even under key repeat.
pub(crate) const MIN_GAP: f32 = 0.045;

#[inline]
const fn palette_trim(voice: SoundVoice, style: GlowStyle) -> f32 {
    match voice {
        SoundVoice::Mech => 0.68,
        SoundVoice::Of(s) => palette_trim(SoundVoice::Style, s),
        SoundVoice::Typewriter => 0.5116,
        SoundVoice::Marimba => 0.9326,
        SoundVoice::Felt => 0.7385,
        SoundVoice::RainbowKittyV2 => 1.0,
        SoundVoice::Style => match style {
            GlowStyle::Lumen => 0.95,
            GlowStyle::Phaser | GlowStyle::Classic => 0.94,
            GlowStyle::RainbowKitty => 1.0,
            GlowStyle::Sparkle => 1.27,
            GlowStyle::Fire => 0.88,
            GlowStyle::Laser => 0.77,
            GlowStyle::Beam => 1.70,
            GlowStyle::Water => 1.01,
            GlowStyle::Comet => 1.64,
        },
    }
}

#[inline]
const fn kill_swoosh_band(voice: SoundVoice, style: GlowStyle) -> (f32, f32) {
    match voice {
        SoundVoice::RainbowKittyV2 => (1600.0, 350.0),
        SoundVoice::Mech => (1000.0, 220.0),
        SoundVoice::Typewriter => (2200.0, 500.0),
        SoundVoice::Marimba => (1300.0, 300.0),
        SoundVoice::Felt => (700.0, 160.0),
        SoundVoice::Of(s) => kill_swoosh_band(SoundVoice::Style, s),
        SoundVoice::Style => match style {
            GlowStyle::Water => (900.0, 250.0),
            GlowStyle::Fire => (1400.0, 300.0),
            GlowStyle::Sparkle => (2600.0, 700.0),
            GlowStyle::Comet => (1200.0, 280.0),
            _ => (1600.0, 350.0),
        },
    }
}

impl TrailSynth {
    #[inline]
    #[must_use]
    pub const fn new(sample_rate: f32, seed: u32) -> Self {
        Self {
            #[cfg(test)]
            ting_unducked: false,
            inv_sr: 1.0 / sample_rate.max(8_000.0),
            rng: seed | 1,
            voices: [Voice::DEFAULT; MAX_VOICES],
            bed: Bed::DEFAULT,
            bed_style: GlowStyle::Lumen,
            bed_voice: SoundVoice::Style,
            rate: 0.0,
            since_voice: 1.0,
            since_event: 1.0,
            block_lead_s: 0.0,
            since_erase: 1.0,
            erase_run: 0,
            space_run: false,
            space_head: false,
            walk: 2,
            theme_pos: 0,
            phrase_idx: 0,
            bass_step: 0,
            shift_step: 0,
            space_run_len: 0,
            space_run_deg: 0,
            space_run_root_hz: 0.0,
            since_space_step: 1.0,
            since_shift: 1.0,
            song_pulse: 0,
            song_accent: true,
            song_ghost: 0,
            song_notes: 0,
            song_feel: 1.0,
            shift_octave: 1.0,
            tone: Tone::Technical,
            duck: 0.0,
            sing: 0.0,
            song_key: 0,
            sing_hold: 0.0,
            last_riff_sig: None,
            steals: 0,
            #[cfg(test)]
            lane_births: [0; 16],
            v2: MelodyV2::new(),
            born_seq: 0,
            clock_s: 0.0,
            v2_latched: false,
            v2_key_pending: false,
            lim_env_l: 0.0,
            lim_g_l: 1.0,
            lim_env_r: 0.0,
            lim_g_r: 1.0,
            dc_x_l: 0.0,
            dc_y_l: 0.0,
            dc_x_r: 0.0,
            dc_y_r: 0.0,
        }
    }

    #[inline]
    const fn rnd(&mut self) -> f32 {
        xorshift01(&mut self.rng)
    }

    #[inline]
    const fn rnd_in(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.rnd()
    }

    /// True once every voice is dead, the bed has decayed and the DC blocker has settled: [`render`](Self::render) then writes exact zeros, so the host can pause its output queue.
    #[inline]
    #[must_use]
    pub const fn is_quiet(&self) -> bool {
        if !(self.bed.level < 1e-3
            && self.bed.energy < 1e-3
            && self.bed.out_lvl < 1e-5
            && self.dc_x_l == 0.0
            && self.dc_x_r == 0.0
            && self.dc_y_l == 0.0
            && self.dc_y_r == 0.0)
        {
            return false;
        }
        let mut i = 0;
        while i < self.voices.len() {
            if self.voices[i].on {
                return false;
            }
            i += 1;
        }
        true
    }

    /// Number of live voices (test/diagnostic hook).
    #[inline]
    #[must_use]
    pub const fn live_voices(&self) -> usize {
        let mut n = 0usize;
        let mut i = 0;
        while i < self.voices.len() {
            if self.voices[i].on {
                n += 1;
            }
            i += 1;
        }
        n
    }

    /// Voice steals since construction (diagnostic).
    #[inline]
    #[must_use]
    pub const fn steals(&self) -> u32 {
        self.steals
    }

    /// Diagnostic: (bed energy, bed level) — demo/tuning hook.
    #[inline]
    #[must_use]
    pub const fn debug_bed(&self) -> (f32, f32) {
        (self.bed.energy, self.bed.level)
    }

    /// Push one gesture.
    #[inline]
    pub const fn push(&mut self, ev: SoundEvent) {
        self.push_meta(ev, EventMeta::EMPTY);
    }

    /// Test-only: account a pause the render clock did not see (§10.1, §16 row 7).
    #[cfg(test)]
    pub fn resume_after(&mut self, paused_s: f32) {
        if !paused_s.is_finite() || paused_s <= 0.0 {
            return;
        }
        self.clock_s += f64::from(paused_s);
        self.since_event += paused_s;
        self.since_voice += paused_s;
        self.since_erase += paused_s;
        self.since_space_step += paused_s;
        self.since_shift += paused_s;
        self.rate *= (-paused_s / 0.6).exp();
    }

    #[inline]
    const fn release_song_key(&mut self) {
        if self.v2_latched {
            self.v2_key_pending = self.song_key != 0;
        } else {
            self.song_key = 0;
        }
    }

    #[inline]
    const fn v2_engaged(&self, ev: &SoundEvent) -> bool {
        matches!(
            ev.kind,
            SoundGesture::Trail(_) | SoundGesture::Output(OutputGesture::Verdict { .. })
        ) && match ev.voice {
            SoundVoice::RainbowKittyV2 | SoundVoice::Of(GlowStyle::RainbowKitty) => true,
            SoundVoice::Style => matches!(ev.style, GlowStyle::RainbowKitty),
            _ => false,
        }
    }

    /// [`Self::push`] with the v2 side-car (§16 rows 6-8).
    #[inline]
    pub const fn push_meta(&mut self, ev: SoundEvent, meta: EventMeta) {
        // TOML accepts NaN/Inf, and one non-finite scalar would poison the
        // persistent voice and bed state. Reject before any mutation and clamp
        // finite inputs into range. Every scalar `SoundEvent` carries must
        // appear in this filter.
        if !ev.pan.is_finite()
            || !ev.heat.is_finite()
            || !ev.hue.is_finite()
            || !ev.gain.is_finite()
        {
            return;
        }
        if !meta.pan_from.is_finite() || !meta.block_lead_s.is_finite() || !meta.flow.is_finite() {
            return;
        }
        let meta = EventMeta {
            pan_from: meta.pan_from.clamp(-1.0, 1.0),
            flow: meta.flow.clamp(0.0, 1.0),
            block_lead_s: meta.block_lead_s.clamp(0.0, BLOCK_LEAD_MAX_S),
            ..meta
        };
        self.block_lead_s = meta.block_lead_s;
        self.push_meta_latched(ev, meta);
        self.block_lead_s = 0.0;
    }

    #[inline]
    const fn push_meta_latched(&mut self, ev: SoundEvent, meta: EventMeta) {
        let ev = SoundEvent {
            pan: ev.pan.clamp(-1.0, 1.0),
            heat: ev.heat.clamp(0.0, 1.0),
            hue: ev.hue.clamp(0.0, 1.0),
            gain: ev.gain.clamp(0.0, 1.0),
            ..ev
        };
        if ev.gain <= 0.0 {
            return;
        }
        if self.v2_engaged(&ev) {
            // Rain glints are muted while the riff owns the room (`sing > 0`,
            // §27): the stardust chime shares the riff shimmer's register and
            // reads as a wrong note. Only the music box ever sets `sing`.
            if self.sing > 0.0 && matches!(ev.kind, SoundGesture::Trail(SoundKind::Stardust { .. }))
            {
                return;
            }
            return self.push_v2(ev, meta);
        }
        if matches!(ev.kind, SoundGesture::Trail(SoundKind::Strum)) {
            return;
        }
        if self.v2_key_pending && matches!(ev.kind, SoundGesture::Trail(_)) {
            self.song_key = 0;
            self.v2_key_pending = false;
        }
        let pause = self.since_event;
        const INV_RATE_TAU: f32 = -1.0 / 0.6;
        self.rate = if self.rate > 0.0 && self.since_event > 0.0 {
            self.rate * crate::color_math::exp_f32(self.since_event * INV_RATE_TAU) + 1.0
        } else {
            self.rate + 1.0
        };
        self.since_event = 0.0;
        if let SoundGesture::Trail(kind) = ev.kind {
            self.bed_style = ev.style;
            self.bed_voice = ev.voice;
            self.tone = ev.tone;
            match kind {
                SoundKind::Space => {
                    self.space_head = !self.space_run;
                    self.space_run = true;
                    self.space_run_len = if self.space_head {
                        0
                    } else {
                        self.space_run_len.saturating_add(1)
                    };
                    if self.space_head {
                        self.song_pulse = 0;
                    }
                }
                SoundKind::Shift => {}
                _ => self.space_run = false,
            }
            if ev.bed {
                self.kick_bed(kind, ev.gain);
            }
        }

        let duck = 1.0 / crate::color_math::sqrt_f32(1.0 + 0.55 * self.rate);
        let bypass = matches!(
            ev.kind,
            SoundGesture::Trail(SoundKind::Jump)
                | SoundGesture::Trail(SoundKind::Sweep { .. })
                | SoundGesture::Trail(SoundKind::Navigation)
                | SoundGesture::Trail(SoundKind::Meteor { .. })
                | SoundGesture::Trail(SoundKind::Land)
                | SoundGesture::Trail(SoundKind::Poof)
                | SoundGesture::Words(WordGesture::Bonk)
                | SoundGesture::Celebration(_)
                | SoundGesture::Output(_)
        );
        let erase = matches!(ev.kind, SoundGesture::Trail(SoundKind::Backspace));
        let shift = matches!(ev.kind, SoundGesture::Trail(SoundKind::Shift));
        let admit = if erase {
            self.since_erase >= ERASE_MIN_GAP
        } else if shift {
            self.since_shift >= SHIFT_MIN_GAP
        } else {
            bypass || self.since_voice >= MIN_GAP
        };
        if !admit {
            // A thinned space head is not a head: otherwise the run would be
            // rooted on a note that never sounded. Un-consume it so the next
            // admitted space opens the run with its own bass root.
            if matches!(ev.kind, SoundGesture::Trail(SoundKind::Space)) && self.space_head {
                self.space_run = false;
            }
            return;
        }
        if shift {
            self.since_shift = 0.0;
        }
        if erase {
            self.erase_run = if self.since_erase <= HELD_ERASE_RUN_WINDOW {
                self.erase_run.saturating_add(1)
            } else {
                1
            };
            self.since_erase = 0.0;
        } else if !matches!(
            ev.kind,
            SoundGesture::Trail(SoundKind::Shift)
                | SoundGesture::Trail(SoundKind::Poof)
                | SoundGesture::Trail(SoundKind::Navigation)
                | SoundGesture::Output(_)
        ) {
            self.since_voice = 0.0;
        }
        if !matches!(
            ev.kind,
            SoundGesture::Trail(SoundKind::Shift)
                | SoundGesture::Trail(SoundKind::Poof)
                | SoundGesture::Output(_)
        ) {
            self.damp_pending_shimmer();
        }
        match ev.kind {
            SoundGesture::Trail(kind) => {
                // Keystrokes, edits and jumps advance the melody. Cursor
                // gestures (Glide/Sweep), Space and Shift play in the melody
                // without stepping it. Determinism is per (events, seed, tone)
                // script.
                if !matches!(
                    kind,
                    SoundKind::Glide { .. }
                        | SoundKind::Sweep { .. }
                        | SoundKind::Land
                        | SoundKind::Space
                        | SoundKind::Shift
                        | SoundKind::Backspace
                        | SoundKind::KillWord
                        | SoundKind::Poof
                ) {
                    self.advance_song(kind, pause);
                }
                self.design_trail(ev, kind, duck, pause);
            }
            SoundGesture::Words(WordGesture::Bonk) => self.design_bonk(ev, duck),
            SoundGesture::Output(gesture) => self.design_output_pip(&ev, gesture, duck),
            SoundGesture::Celebration(CelebrationGesture::RiffBar { bar, sig }) => {
                self.latch_song_key(sig);
                self.design_celebration(ev, bar, sig);
            }
            SoundGesture::Celebration(CelebrationGesture::Outro { sig }) => {
                self.latch_song_key(sig);
                self.design_celebration_outro(ev, sig);
            }
        }
    }

    #[inline]
    const fn advance_song(&mut self, kind: SoundKind, pause: f32) {
        let boundary = matches!(kind, SoundKind::Jump) || pause > PHRASE_PAUSE_S;
        if boundary {
            self.song_pulse = 0;
        }
        let slot = if matches!(kind, SoundKind::Typed) {
            let s = SONG_PULSE[self.song_pulse as usize];
            self.song_pulse = (self.song_pulse + 1) % SONG_PULSE.len() as u8;
            s
        } else {
            SONG_ACCENT
        };
        if slot == SONG_ACCENT {
            self.song_accent = true;
            self.song_ghost = 0;
            self.song_notes = self.song_notes.wrapping_add(1);
            self.advance_melody(kind, pause);
        } else {
            self.song_accent = false;
            self.song_ghost = slot;
        }
    }

    #[inline]
    const fn advance_melody(&mut self, kind: SoundKind, pause: f32) {
        let (lo, hi) = tone_register(self.tone);
        let start = SONG_FORM[self.phrase_idx as usize] as usize;
        let end = SONG_FORM[self.phrase_idx as usize + 1] as usize;
        let pos = self.theme_pos as usize;
        let ran_out = pos >= end;
        let rest = (matches!(kind, SoundKind::Jump) || pause > PHRASE_PAUSE_S) && pos > start;
        if rest && !ran_out {
            self.walk = fold_register(SONG_THEME[end - 1] as i32 + melody_lean(self.tone), lo, hi);
            self.open_next_phrase();
            return;
        }
        if ran_out {
            self.open_next_phrase();
        }
        self.walk = fold_register(
            SONG_THEME[self.theme_pos as usize] as i32 + melody_lean(self.tone),
            lo,
            hi,
        );
        self.theme_pos += 1;
    }

    #[inline]
    const fn space_root_hz(&self, anchor: f32, step: u8) -> f32 {
        let deg = (SONG_BASS[step as usize] as i32) + (self.song_key as i32);
        bass_octave(self.melody_hz(anchor, deg)) * 2.0
    }

    #[inline]
    const fn open_next_phrase(&mut self) {
        const _: () = assert!((SONG_FORM.len() - 1).is_power_of_two());
        self.phrase_idx = (self.phrase_idx + 1) & (SONG_FORM.len() as u8 - 2);
        self.theme_pos = SONG_FORM[self.phrase_idx as usize];
    }

    #[inline]
    const fn melody_hz(&self, base: f32, degree: i32) -> f32 {
        let (table, transpose) = tone_tables(self.tone);
        let oct = degree.div_euclid(5);
        let step = (degree - oct * 5) as usize;
        base * transpose * table[step] * pow2i(oct)
    }

    #[inline]
    const fn all_voices_on(&self) -> bool {
        let mut i = 0;
        while i < self.voices.len() {
            if !self.voices[i].on {
                return false;
            }
            i += 1;
        }
        true
    }

    #[inline]
    const fn claim_lane(&mut self, lane: u8, deferred: bool) -> Option<usize> {
        if lane == LANE_NONE {
            return Some(self.claim());
        }
        if !deferred && !self.lane_make_room(lane, MAX_VOICES) {
            return None;
        }
        if lane_drops_the_newcomer(lane) && self.all_voices_on() {
            return None;
        }
        Some(self.claim())
    }

    #[inline]
    const fn lane_make_room(&mut self, lane: u8, skip: usize) -> bool {
        let cap = lane_cap(lane);
        let mut live = 0usize;
        let mut oldest = None;
        let mut oldest_born = u32::MAX;
        let mut i = 0;
        while i < self.voices.len() {
            let v = &self.voices[i];
            if i != skip && v.on && v.lane == lane && v.damp <= 0.0 && v.t >= 0.0 {
                live += 1;
                if v.born < oldest_born {
                    oldest_born = v.born;
                    oldest = Some(i);
                }
            }
            i += 1;
        }
        if live < cap {
            return true;
        }
        let Some(idx) = oldest else {
            return true;
        };
        if self.voices[idx].t < LANE_AGE_GUARD_S && lane_drops_the_newcomer(lane) {
            return false;
        }
        self.voices[idx].damp = LANE_FADE_STEAL_S;
        self.voices[idx].damp0 = LANE_FADE_STEAL_S;
        true
    }

    #[inline]
    const fn lane_onset_steal(&mut self, vi: usize) -> bool {
        let lane = self.voices[vi].lane;
        !self.lane_make_room(lane, vi)
    }

    #[inline]
    const fn claim(&mut self) -> usize {
        let mut best = 0;
        let mut best_w = f32::MAX;
        let mut i = 0;
        while i < self.voices.len() {
            let v = &self.voices[i];
            if !v.on {
                return i;
            }
            let w = v.weight();
            if w < best_w {
                best_w = w;
                best = i;
            }
            i += 1;
        }
        self.steals = self.steals.saturating_add(1);
        best
    }

    #[inline]
    const fn spawn(&mut self, proto: Voice, gain: f32, pan: f32) -> Option<usize> {
        let tw_ph = self.rnd();
        let ph = [self.rnd(), self.rnd(), self.rnd()];
        self.spawn_seeded(proto, gain, pan, tw_ph, ph)
    }

    #[inline]
    const fn spawn_seeded(
        &mut self,
        proto: Voice,
        gain: f32,
        pan: f32,
        tw_ph: f32,
        ph: [f32; 3],
    ) -> Option<usize> {
        let Some(idx) = self.claim_lane(proto.lane, proto.delay > 0.0) else {
            return None;
        };
        #[cfg(test)]
        {
            let l = if proto.lane < 15 {
                proto.lane as usize
            } else {
                15
            };
            self.lane_births[l] = self.lane_births[l].saturating_add(1);
        }
        let mut v = proto;
        let feel = tone_feel(self.tone) * self.song_feel;
        if !v.duck_exempt && feel != 1.0 {
            v.dur *= feel;
            v.decay *= feel;
            if v.lane == LANE_NONE {
                v.delay *= feel;
            }
        }
        if self.shift_octave != 1.0 && (v.p[0].lvl > 0.0 || v.p[1].lvl > 0.0 || v.p[2].lvl > 0.0) {
            v.lp_cut *= self.shift_octave;
            let inv_shift_octave = 1.0 / self.shift_octave;
            let mut i = 0;
            while i < v.p.len() {
                if v.p[i].lvl > 0.0 {
                    let (fi0, fi1) = (v.p[i].f0, v.p[i].f1);
                    let mut j = 0;
                    while j < v.p.len() {
                        if i != j && v.p[j].lvl > 0.0 {
                            let r0 = v.p[j].f0 / fi0;
                            if r0 > 1.0 && r0 < DETUNE_MAX_RATIO {
                                v.p[j].f0 = fi0 * (1.0 + (r0 - 1.0) * inv_shift_octave);
                            }
                            let r1 = if fi1 > 0.0 { v.p[j].f1 / fi1 } else { 0.0 };
                            if r1 > 1.0 && r1 < DETUNE_MAX_RATIO {
                                v.p[j].f1 = fi1 * (1.0 + (r1 - 1.0) * inv_shift_octave);
                            }
                        }
                        j += 1;
                    }
                }
                i += 1;
            }
        }
        v.on = true;
        v.t = -(v.delay + self.block_lead_s);
        (v.gl, v.gr) = pan_gains(gain, pan);
        self.born_seq = self.born_seq.saturating_add(1);
        v.born = self.born_seq;
        if v.pan_glide_s > 0.0 {
            (v.gl1, v.gr1) = pan_gains(gain, v.pan1);
            v.pan_k = 1.0 / v.pan_glide_s;
            v.pan_t0 = 0.0;
        } else {
            v.gl1 = v.gl;
            v.gr1 = v.gr;
            v.pan_k = 0.0;
        }
        v.tw_ph = tw_ph;
        let dt = self.inv_sr;
        v.k_a = crate::color_math::exp_f32(-dt / v.attack.max(2e-4));
        v.k_d = crate::color_math::exp_f32(-dt / v.decay.max(1e-3));
        v.k_n = if v.n_glide > 0.0 {
            crate::color_math::exp_f32(-dt / v.n_glide)
        } else {
            1.0
        };
        v.env_run = false;
        v.env_n = 0;
        v.k_nd = if v.n_decay > 0.0 {
            crate::color_math::exp_f32(-dt / v.n_decay)
        } else {
            1.0
        };
        let mut p_idx = 0;
        while p_idx < 3 {
            let part = &mut v.p[p_idx];
            part.ph = ph[p_idx];
            part.k_g = if part.glide > 0.0 {
                crate::color_math::exp_f32(-dt / part.glide)
            } else {
                1.0
            };
            part.k_f = if part.fm_ratio > 0.0 {
                crate::color_math::exp_f32(-dt / part.fm_tau.max(1e-3))
            } else {
                1.0
            };
            part.k_p = if part.decay > 0.0 {
                crate::color_math::exp_f32(-dt / part.decay)
            } else {
                1.0
            };
            p_idx += 1;
        }
        v.lp = 0.0;
        v.n_lp = 0.0;
        v.n_bp = 0.0;
        v.lp_k = (v.lp_cut * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        if v.n_lvl > 0.0 {
            v.n_damp = 1.0 / v.n_q.max(0.3);
            if v.n_glide <= 0.0 {
                let (s, c) =
                    crate::supernova::sin_cos_f32(core::f32::consts::PI * (v.n_f0 * dt).min(0.45));
                v.svf_g = s / c;
                v.svf_den = 1.0 + v.svf_g * (v.svf_g + v.n_damp);
            }
        }
        self.voices[idx] = v;
        Some(idx)
    }

    #[inline]
    const fn design_trail(&mut self, ev: SoundEvent, kind: SoundKind, duck: f32, pause: f32) {
        let g = ev.gain * duck * (0.55 + 0.45 * ev.heat);
        let kg = match kind {
            SoundKind::Typed => TYPED_KIND_GAIN,
            SoundKind::Backspace => BACKSPACE_KIND_GAIN,
            SoundKind::Space => SPACE_KIND_GAIN,
            SoundKind::Glide { .. } => GLIDE_KIND_GAIN,
            SoundKind::Shift => SHIFT_KIND_GAIN,
            SoundKind::Poof => POOF_KIND_GAIN,
            SoundKind::Navigation => NAVIGATION_KIND_GAIN,
            SoundKind::Sweep { .. } => SWEEP_KIND_GAIN,
            SoundKind::KillWord => KILLWORD_KIND_GAIN,
            SoundKind::Kill => KILL_KIND_GAIN,
            SoundKind::Jump => JUMP_KIND_GAIN,
            SoundKind::Land => LAND_KIND_GAIN,
            SoundKind::MeteorArm { .. } => METEOR_ARM_KIND_GAIN,
            SoundKind::Meteor { .. } | SoundKind::Enter { .. } | SoundKind::Strum => JUMP_KIND_GAIN,
            SoundKind::Stardust { .. } => POOF_KIND_GAIN,
        };
        let g = g * kg * shift_gain(&ev, kind);
        match kind {
            SoundKind::Land => {
                self.design_land(&ev, g);
                return;
            }
            SoundKind::Backspace => {
                self.design_erase_poof(&ev, g);
                return;
            }
            SoundKind::KillWord => {
                self.design_word_poof(&ev, g);
                return;
            }
            SoundKind::Space => {
                self.design_space(&ev, g, pause);
                return;
            }
            SoundKind::Kill => {
                let (hi, lo) = kill_swoosh_band(ev.voice, ev.style);
                let v = Voice {
                    dur: 0.28,
                    attack: 0.02,
                    decay: 0.12,
                    n_lvl: 0.5,
                    n_f0: hi,
                    n_f1: lo,
                    n_glide: 0.10,
                    n_q: 1.2,
                    lp_cut: 2400.0,
                    ..Voice::DEFAULT
                };
                self.spawn(v, g * 2.6, ev.pan);
                return;
            }
            SoundKind::Poof => {
                self.design_poof(&ev, g);
                return;
            }
            _ => {}
        }
        let col_off = (ev.pan).round() as i32;
        let shape = gesture_shape(kind);
        let ghosting = matches!(kind, SoundKind::Typed) && !self.song_accent;
        let deg = self.walk
            + col_off
            + (self.song_key as i32)
            + shape.offset
            + if ghosting { self.song_ghost as i32 } else { 0 }
            + shift_lift(&ev, kind);

        if matches!(kind, SoundKind::Glide { .. } | SoundKind::Sweep { .. }) {
            self.design_cursor(&ev, shape, deg, g);
            return;
        }
        if matches!(kind, SoundKind::Shift) {
            self.design_shift(&ev, deg, g);
            return;
        }

        let g = g * palette_trim(ev.voice, ev.style);
        let g = if ghosting { g * SONG_GHOST_LEVEL } else { g };
        self.song_feel = if ghosting { SONG_GHOST_FEEL } else { 1.0 };
        self.shift_octave = shift_ratio(&ev, kind);
        palette_for(ev.voice, ev.style).design(self, &ev, kind, g, deg, col_off);
        self.song_feel = 1.0;
        self.shift_octave = 1.0;
    }

    #[inline]
    const fn design_cursor(&mut self, ev: &SoundEvent, shape: GestureShape, deg: i32, g: f32) {
        let anchor = palette_for(ev.voice, ev.style).anchor_hz();
        let mut i = 0;
        while i < shape.notes {
            let f = self.melody_hz(anchor, deg + shape.step * i as i32);
            let (f0, f1) = gesture_bend(f, shape.dir);
            let v = Voice {
                delay: i as f32 * CURSOR_SWEEP_STEP_S,
                dur: 0.18,
                attack: 0.006,
                decay: 0.08,
                p: [
                    Partial {
                        lvl: 0.5,
                        f0,
                        f1,
                        glide: GESTURE_BEND_TAU,
                        ..Partial::DEFAULT
                    },
                    Partial {
                        lvl: 0.12,
                        f0: f0 * 0.5,
                        f1: f1 * 0.5,
                        glide: GESTURE_BEND_TAU,
                        ..Partial::DEFAULT
                    },
                    Partial::DEFAULT,
                ],
                lp_cut: 2200.0,
                ..Voice::DEFAULT
            };
            let level = if shape.notes == 1 {
                0.5
            } else {
                0.45 * (1.0 - 0.15 * i as f32)
            };
            self.spawn(v, g * level, ev.pan);
            i += 1;
        }
    }

    #[inline]
    const fn design_land(&mut self, ev: &SoundEvent, g: f32) {
        let mut i = 0;
        while i < LAND_STARS {
            let deg = self.walk + LAND_STAR_DEGREE_STEP * i as i32;
            let f = self.melody_hz(LAND_ANCHOR_HZ, deg);
            let v = Voice {
                delay: i as f32 * LAND_STAR_STEP_S,
                dur: 0.46,
                attack: 0.002,
                decay: 0.13,
                p: [
                    Partial {
                        lvl: 0.55,
                        f0: f,
                        f1: f,
                        fm_ratio: 2.01,
                        fm_i0: 0.42,
                        fm_tau: 0.03,
                        ..Partial::DEFAULT
                    },
                    Partial {
                        lvl: 0.2,
                        f0: f * 0.5,
                        f1: f * 0.5,
                        ..Partial::DEFAULT
                    },
                    Partial {
                        lvl: 0.1,
                        f0: f * 2.0,
                        f1: f * 2.0,
                        ..Partial::DEFAULT
                    },
                ],
                tw_rate: 11.0,
                tw_depth: 0.35,
                lp_cut: 4200.0,
                ..Voice::DEFAULT
            };
            let pan = ev.pan + 0.22 * if i & 1 == 0 { -1.0 } else { 1.0 };
            self.spawn(v, g * (0.5 - 0.12 * i as f32), pan);
            i += 1;
        }
        let low = self.melody_hz(LAND_ANCHOR_HZ * 0.25, self.walk);
        let body = Voice {
            dur: 0.22,
            attack: 0.005,
            decay: 0.075,
            p: [
                Partial {
                    lvl: 0.7,
                    f0: low,
                    f1: low,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
                Partial::DEFAULT,
            ],
            n_lvl: 0.2,
            n_f0: 2600.0,
            n_f1: 700.0,
            n_glide: 0.05,
            n_q: 0.8,
            lp_cut: 2000.0,
            ..Voice::DEFAULT
        };
        self.spawn(body, g * 0.3, ev.pan);
    }

    #[inline]
    const fn space_breath(lvl: f32) -> Voice {
        Voice {
            dur: 0.11,
            attack: 0.008,
            decay: 0.055,
            n_lvl: lvl,
            n_f0: 900.0,
            n_f1: 380.0,
            n_glide: 0.08,
            n_q: 0.7,
            lp_cut: 1400.0,
            ..Voice::DEFAULT
        }
    }

    #[inline]
    const fn design_space(&mut self, ev: &SoundEvent, g: f32, pause: f32) {
        let anchor = palette_for(ev.voice, ev.style).anchor_hz();
        if !self.space_head {
            self.spawn_seeded(
                Self::space_breath(0.05),
                g * SPACE_VOICE_LEVEL * SPACE_RUN_BREATH_LEVEL,
                ev.pan,
                0.0,
                [0.0; 3],
            );
            if self.since_space_step >= SPACE_STEP_MIN_GAP && self.space_run_root_hz > 0.0 {
                self.since_space_step = 0.0;
                let deg = self.space_run_deg;
                let root = self.melody_hz(anchor, deg);
                let inv_root = 1.0 / root;
                let mut f = self.space_run_root_hz;
                let mut n = self.space_run_len as i32;
                while n >= 1 {
                    let step = self.space_run_root_hz
                        * (self.melody_hz(anchor, deg + n) * inv_root).max(1.0);
                    if step < SPACE_STEP_HI_HZ {
                        f = step;
                        break;
                    }
                    n -= 1;
                }
                let step = Voice {
                    dur: SPACE_STEP_DUR_S,
                    attack: SPACE_STEP_ATTACK_S,
                    decay: SPACE_STEP_DECAY_S,
                    p: [
                        Partial {
                            lvl: 0.55,
                            f0: f,
                            f1: f,
                            fm_ratio: 3.01,
                            fm_i0: SPACE_STEP_GLINT,
                            fm_tau: 0.018,
                            ..Partial::DEFAULT
                        },
                        Partial {
                            lvl: SPACE_STEP_OCTAVE_LEVEL,
                            f0: f * 2.0,
                            f1: f * 2.0,
                            ..Partial::DEFAULT
                        },
                        Partial::DEFAULT,
                    ],
                    n_lvl: 0.4,
                    n_f0: 4200.0,
                    n_f1: 180.0,
                    n_glide: 0.005,
                    n_q: 0.7,
                    lp_cut: 5200.0,
                    ..Voice::DEFAULT
                };
                self.spawn_seeded(
                    step,
                    g * SPACE_VOICE_LEVEL * SPACE_STEP_LEVEL,
                    0.0,
                    0.0,
                    [0.0; 3],
                );
            }
            return;
        }
        self.since_space_step = 0.0;
        let mut i = 0;
        while i < self.voices.len() {
            let v = &mut self.voices[i];
            if v.on && v.bass && v.damp <= 0.0 {
                v.damp = SPACE_DAMP_S;
                v.damp0 = SPACE_DAMP_S;
            }
            i += 1;
        }
        let rested = pause > PHRASE_PAUSE_S;
        let (attack, decay, dur, oct_lvl, fifth_lvl) = if rested {
            (
                SPACE_BREATHE_ATTACK_S,
                SPACE_BREATHE_DECAY_S,
                SPACE_BREATHE_DUR_S,
                SPACE_BREATHE_OCTAVE_LEVEL,
                SPACE_BREATHE_FIFTH_LEVEL,
            )
        } else {
            (
                SPACE_ATTACK_S,
                SPACE_DECAY_S,
                SPACE_DUR_S,
                SPACE_OCTAVE_LEVEL,
                SPACE_FIFTH_LEVEL,
            )
        };
        let f = self.space_root_hz(anchor, self.bass_step);
        self.space_run_deg = (SONG_BASS[self.bass_step as usize] as i32) + (self.song_key as i32);
        self.space_run_root_hz = f;
        self.bass_step = (self.bass_step + 1) % SONG_BASS.len() as u8;
        let v = Voice {
            bass: true,
            dur,
            attack,
            decay,
            p: [
                Partial {
                    lvl: 0.55,
                    f0: f,
                    f1: f,
                    fm_ratio: 3.01,
                    fm_i0: SPACE_BELL_GLINT,
                    fm_tau: 0.022,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: oct_lvl,
                    f0: f * 2.0,
                    f1: f * 2.0,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: fifth_lvl.max(SPACE_BELL_TINK),
                    f0: f * 4.0,
                    f1: f * 4.0,
                    ..Partial::DEFAULT
                },
            ],
            n_lvl: 0.5,
            n_f0: 4200.0,
            n_f1: 180.0,
            n_glide: 0.005,
            n_q: 0.7,
            lp_cut: 5200.0,
            ..Self::space_breath(0.05)
        };
        self.spawn(v, g * SPACE_VOICE_LEVEL, 0.0);
        let tw = twinkle_hz(f);
        self.spawn_seeded(
            Voice {
                dur: SPACE_TWINKLE_DUR_S,
                attack: 0.004,
                decay: SPACE_TWINKLE_DECAY_S,
                p: [
                    Partial {
                        lvl: SPACE_TWINKLE,
                        f0: tw,
                        f1: tw,
                        ..Partial::DEFAULT
                    },
                    Partial {
                        lvl: SPACE_TWINKLE_TWIN,
                        f0: tw + f * SPACE_TWINKLE_DETUNE,
                        f1: tw + f * SPACE_TWINKLE_DETUNE,
                        ..Partial::DEFAULT
                    },
                    Partial::DEFAULT,
                ],
                n_lvl: 0.5,
                n_f0: 6400.0,
                n_f1: 3800.0,
                n_glide: 0.020,
                n_q: 1.0,
                tw_rate: SPACE_TWINKLE_RATE,
                tw_depth: SPACE_TWINKLE_DEPTH,
                lp_cut: 8000.0,
                ..Voice::DEFAULT
            },
            g * SPACE_VOICE_LEVEL * SPACE_AIR_LEVEL,
            0.0,
            0.0,
            [0.0; 3],
        );
    }

    #[inline]
    const fn design_shift(&mut self, ev: &SoundEvent, deg: i32, g: f32) {
        let anchor = palette_for(ev.voice, ev.style).anchor_hz();
        let deg = deg + SHIFT_ROTATION[self.shift_step as usize];
        self.shift_step = (self.shift_step + 1) % SHIFT_ROTATION.len() as u8;
        let f = ting_octave(self.melody_hz(anchor, deg));
        let ghost_next = SONG_PULSE[self.song_pulse as usize] != SONG_ACCENT;
        let g = if ghost_next { g * SONG_GHOST_LEVEL } else { g };
        let v = Voice {
            dur: SHIFT_TING_DUR_S,
            attack: SHIFT_TING_ATTACK_S,
            decay: SHIFT_TING_DECAY_S,
            p: [
                Partial {
                    lvl: 0.55,
                    f0: f,
                    f1: f,
                    fm_ratio: 3.01,
                    fm_i0: SHIFT_TING_GLINT,
                    fm_tau: SHIFT_TING_GLINT_TAU_S,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: SHIFT_TING_OCTAVE_LEVEL,
                    f0: f * 2.0,
                    f1: f * 2.0,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
            ],
            n_lvl: 0.35,
            n_f0: 4200.0,
            n_f1: 180.0,
            n_glide: 0.005,
            n_q: 0.7,
            lp_cut: 9000.0,
            ..Voice::DEFAULT
        };
        self.spawn(v, g * SHIFT_TING_VOICE_LEVEL, ev.pan);
    }

    #[inline]
    const fn design_erase_poof(&mut self, ev: &SoundEvent, g: f32) {
        let body = Voice {
            dur: POOF_BODY_DUR_S,
            attack: POOF_BODY_ATTACK_S,
            decay: POOF_BODY_DECAY_S,
            n_lvl: 0.5,
            n_f0: POOF_BODY_HZ,
            n_f1: POOF_BODY_HZ,
            n_glide: 0.0,
            n_q: POOF_BODY_Q,
            lp_cut: POOF_LP_CUT_HZ,
            ..Voice::DEFAULT
        };
        self.spawn_seeded(body, g * POOF_VOICE_GAIN, ev.pan, 0.0, [0.0; 3]);
        let air = Voice {
            delay: POOF_AIR_DELAY_S,
            dur: POOF_AIR_DUR_S,
            attack: POOF_AIR_ATTACK_S,
            decay: POOF_AIR_DECAY_S,
            n_lvl: 0.5,
            n_f0: POOF_AIR_HZ,
            n_f1: POOF_AIR_SETTLE_HZ,
            n_glide: POOF_AIR_SETTLE_GLIDE_S,
            n_q: POOF_AIR_Q,
            lp_cut: POOF_LP_CUT_HZ,
            ..Voice::DEFAULT
        };
        self.spawn_seeded(
            air,
            g * POOF_VOICE_GAIN * POOF_AIR_LEVEL,
            ev.pan,
            0.0,
            [0.0; 3],
        );
        if self.erase_run >= HELD_ERASE_RUN_MIN {
            let glitter = Voice {
                shimmer: true,
                delay: ERASE_SHIMMER_DELAY_S,
                dur: ERASE_SHIMMER_DUR_S,
                attack: ERASE_SHIMMER_ATTACK_S,
                decay: ERASE_SHIMMER_DECAY_S,
                n_lvl: 0.5,
                n_f0: ERASE_SHIMMER_HZ0,
                n_f1: ERASE_SHIMMER_HZ1,
                n_glide: ERASE_SHIMMER_GLIDE_S,
                n_q: ERASE_SHIMMER_Q,
                tw_rate: ERASE_SHIMMER_TW_RATE,
                tw_depth: ERASE_SHIMMER_TW_DEPTH,
                lp_cut: ERASE_SHIMMER_LP_HZ,
                ..Voice::DEFAULT
            };
            self.spawn_seeded(glitter, g * ERASE_SHIMMER_GAIN, ev.pan, 0.0, [0.0; 3]);
        }
    }

    #[inline]
    const fn damp_pending_shimmer(&mut self) {
        let mut i = 0;
        while i < self.voices.len() {
            let v = &mut self.voices[i];
            if v.on && v.shimmer && v.t < 0.0 && v.damp <= 0.0 {
                v.damp = ERASE_SHIMMER_DAMP_S;
                v.damp0 = ERASE_SHIMMER_DAMP_S;
            }
            i += 1;
        }
    }

    #[inline]
    const fn design_word_poof(&mut self, ev: &SoundEvent, g: f32) {
        let body = Voice {
            dur: WORD_POOF_BODY_DUR_S,
            attack: WORD_POOF_BODY_ATTACK_S,
            decay: WORD_POOF_BODY_DECAY_S,
            n_lvl: 0.5,
            n_f0: WORD_POOF_BODY_HZ,
            n_f1: WORD_POOF_BODY_HZ,
            n_glide: 0.0,
            n_q: WORD_POOF_BODY_Q,
            lp_cut: POOF_LP_CUT_HZ,
            ..Voice::DEFAULT
        };
        self.spawn_seeded(
            body,
            g * POOF_VOICE_GAIN * WORD_POOF_GAIN,
            ev.pan,
            0.0,
            [0.0; 3],
        );
        let air = Voice {
            delay: WORD_POOF_AIR_DELAY_S,
            dur: WORD_POOF_AIR_DUR_S,
            attack: WORD_POOF_AIR_ATTACK_S,
            decay: WORD_POOF_AIR_DECAY_S,
            n_lvl: 0.5,
            n_f0: WORD_POOF_AIR_HZ,
            n_f1: WORD_POOF_AIR_SETTLE_HZ,
            n_glide: WORD_POOF_AIR_GLIDE_S,
            n_q: WORD_POOF_AIR_Q,
            lp_cut: POOF_LP_CUT_HZ,
            ..Voice::DEFAULT
        };
        self.spawn_seeded(
            air,
            g * POOF_VOICE_GAIN * WORD_POOF_GAIN * WORD_POOF_AIR_LEVEL,
            ev.pan,
            0.0,
            [0.0; 3],
        );
    }

    #[inline]
    const fn design_poof(&mut self, ev: &SoundEvent, g: f32) {
        let v = Voice {
            dur: POOF_CLOUD_DUR_S,
            attack: POOF_CLOUD_ATTACK_S,
            decay: POOF_CLOUD_DECAY_S,
            n_lvl: 0.5,
            n_f0: POOF_AIR_HZ,
            n_f1: POOF_CLOUD_SETTLE_HZ,
            n_glide: POOF_CLOUD_GLIDE_S,
            n_q: POOF_AIR_Q,
            lp_cut: POOF_LP_CUT_HZ,
            ..Voice::DEFAULT
        };
        self.spawn_seeded(v, g * POOF_CLOUD_GAIN, ev.pan, 0.0, [0.0; 3]);
    }

    #[inline]
    const fn design_output_pip(&mut self, ev: &SoundEvent, gesture: OutputGesture, duck: f32) {
        let (kind_gain, octave) = match gesture {
            OutputGesture::Shimmer => (OUTPUT_PIP_KIND_GAIN, 0.25),
            OutputGesture::Settle => (OUTPUT_SETTLE_KIND_GAIN, 0.0),
            OutputGesture::Verdict { .. } => return,
        };
        let g = ev.gain * duck * (0.55 + 0.45 * ev.heat) * kind_gain;
        let deg = (ev.hue * 5.0) as i32;
        let f = self.melody_hz(palette_for(ev.voice, ev.style).anchor_hz(), deg);
        let v = Voice {
            dur: 0.34,
            attack: 0.012,
            decay: 0.28,
            p: [
                Partial {
                    lvl: 0.5,
                    f0: f,
                    f1: f,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: octave,
                    f0: f * 2.0,
                    f1: f * 2.0,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
            ],
            n_lvl: 0.0,
            lp_cut: 2200.0,
            ..Voice::DEFAULT
        };
        self.spawn_seeded(v, g, ev.pan, 0.0, [0.0; 3]);
    }

    #[inline]
    const fn design_bonk(&mut self, ev: SoundEvent, duck: f32) {
        let g = ev.gain * duck * (0.55 + 0.45 * ev.heat) * BONK_KIND_GAIN;
        let root = penta(palette_for(ev.voice, ev.style).anchor_hz(), self.walk);
        let m2 = root * BONK_MINOR_SECOND;
        let tt = root * BONK_TRITONE;
        let clash = Voice {
            dur: 0.30,
            attack: 0.003,
            decay: 0.11,
            p: [
                Partial {
                    lvl: 0.62,
                    f0: m2 * 1.35,
                    f1: m2,
                    glide: 0.02,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: 0.5,
                    f0: tt * 1.35,
                    f1: tt,
                    glide: 0.02,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
            ],
            lp_cut: 2600.0,
            duck_exempt: true,
            ..Voice::DEFAULT
        };
        self.spawn(clash, g * 0.306, ev.pan);
        let thump = Voice {
            dur: 0.16,
            attack: 0.004,
            decay: 0.06,
            p: [
                Partial {
                    lvl: 0.85,
                    f0: 150.0,
                    f1: 92.0,
                    glide: 0.045,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
                Partial::DEFAULT,
            ],
            n_lvl: 0.18,
            n_f0: 700.0,
            n_f1: 180.0,
            n_glide: 0.05,
            n_q: 0.9,
            lp_cut: 900.0,
            duck_exempt: true,
            ..Voice::DEFAULT
        };
        self.spawn(thump, g * 0.258, ev.pan * 0.6);
        self.duck = 1.0;
    }

    #[inline]
    const fn latch_song_key(&mut self, sig: u32) {
        self.song_key = (celebration_root(sig) + celebration_mode(sig)) as i8;
        self.v2_key_pending = false;
    }

    #[inline]
    const fn design_celebration(&mut self, ev: SoundEvent, bar: u16, sig: u32) {
        // Same-sig bars skip this, and it draws no rng.
        if matches!(self.last_riff_sig, Some(old) if old != sig) {
            let mut i = 0;
            while i < self.voices.len() {
                let v = &mut self.voices[i];
                if v.on && v.celebration && v.damp <= 0.0 {
                    v.damp = CELEBRATION_DAMP_S;
                    v.damp0 = CELEBRATION_DAMP_S;
                }
                i += 1;
            }
        }
        self.last_riff_sig = Some(sig);
        const _: () = assert!(CELEBRATION_PHRASE_BARS.is_power_of_two());
        let g = ev.gain * (0.55 + 0.45 * ev.heat) * CELEBRATION_KIND_GAIN;
        let idx = (bar as usize) & (CELEBRATION_PHRASE_BARS - 1);
        let phrase = &celebration_bar_degrees(sig, idx);
        let bass_bar = &CELEBRATION_BASS[idx];
        let shift = celebration_root(sig) + celebration_mode(sig);
        const INV_CELEBRATION_BUILD_BARS: f32 = 1.0 / CELEBRATION_BUILD_BARS;
        let build = ((bar as f32) * INV_CELEBRATION_BUILD_BARS).min(1.0);
        let lift = CELEBRATION_BAR_LIFT[idx];
        let clap = bar >= CELEBRATION_CLAP_BAR;
        let turnaround = idx == CELEBRATION_PHRASE_BARS - 1;
        let sustain_limit = if turnaround { 6 } else { phrase.len() };
        let lp_cut = 2800.0 + 1400.0 * build;
        let shimmer = 0.10 + 0.12 * build;
        let mut i = 0;
        while i < phrase.len() {
            let deg = phrase[i];
            // Distinct branch.
            if deg == REST {
                i += 1;
                continue;
            }
            let span = celebration_span(phrase, i, sustain_limit);
            let hz = self.melody_hz(CELEBRATION_BASE_HZ, deg + shift);
            let sub = if i.is_multiple_of(2) && bass_bar[i / 2] != REST {
                let b = self.melody_hz(CELEBRATION_BASE_HZ, bass_bar[i / 2] + shift);
                Partial {
                    lvl: 0.42 * build,
                    f0: b,
                    f1: b,
                    ..Partial::DEFAULT
                }
            } else {
                Partial::DEFAULT
            };
            let (n_lvl, n_f0, n_f1, n_glide, n_q) = if clap && (i == 2 || i == 6) {
                (0.10, 2600.0, 1200.0, 0.02, 1.4)
            } else {
                (0.0, 0.0, 0.0, 0.0, 0.0)
            };
            let v = Voice {
                delay: celebration_slot_delay(i),
                dur: span as f32 * CELEBRATION_EIGHTH * 1.5,
                attack: 0.004,
                decay: 0.10 * span as f32,
                p: celebration_lead(hz, shimmer, sub),
                n_lvl,
                n_f0,
                n_f1,
                n_glide,
                n_q,
                lp_cut,
                duck_exempt: true,
                celebration: true,
                ..Voice::DEFAULT
            };
            self.spawn(
                v,
                g * CELEBRATION_GROOVE[i] * lift,
                celebration_sway(ev.pan, deg),
            );
            i += 1;
        }
        if turnaround {
            let mut k = 0;
            while k < CELEBRATION_FILL.len() {
                let deg = CELEBRATION_FILL[k];
                let hz = self.melody_hz(CELEBRATION_BASE_HZ, deg + shift);
                let v = Voice {
                    delay: (6.0 + 0.5 * k as f32) * CELEBRATION_EIGHTH,
                    dur: CELEBRATION_EIGHTH,
                    attack: 0.003,
                    decay: 0.055,
                    p: celebration_lead(hz, shimmer, Partial::DEFAULT),
                    lp_cut,
                    duck_exempt: true,
                    celebration: true,
                    ..Voice::DEFAULT
                };
                self.spawn(
                    v,
                    g * (0.34 + 0.06 * k as f32) * lift,
                    celebration_sway(ev.pan, deg),
                );
                k += 1;
            }
        }
        self.sing = 1.0;
        self.sing_hold = CELEBRATION_BAR_SECONDS + 0.1;
    }

    #[inline]
    const fn design_celebration_outro(&mut self, ev: SoundEvent, sig: u32) {
        self.last_riff_sig = Some(sig);
        let g = ev.gain * (0.55 + 0.45 * ev.heat) * CELEBRATION_KIND_GAIN;
        let shift = celebration_root(sig) + celebration_mode(sig);
        let hz = self.melody_hz(CELEBRATION_BASE_HZ, shift);
        let sub = self.melody_hz(CELEBRATION_BASE_HZ, CELEBRATION_OUTRO_SUB_DEG + shift);
        let bass = Partial {
            lvl: 0.42,
            f0: sub,
            f1: sub,
            ..Partial::DEFAULT
        };
        let lead = Voice {
            dur: CELEBRATION_OUTRO_S,
            attack: 0.004,
            decay: CELEBRATION_OUTRO_DECAY_S,
            p: celebration_lead(hz, 0.22, bass),
            lp_cut: 4200.0,
            duck_exempt: true,
            celebration: true,
            ..Voice::DEFAULT
        };
        self.spawn(lead, g * CELEBRATION_GROOVE[0], celebration_sway(ev.pan, 0));
        let (mut bell, trim) = rainbow_kitty_v2::cad_bell_voice(0.0);
        bell.duck_exempt = true;
        bell.celebration = true;
        self.spawn(bell, ev.gain * trim, -0.5 * ev.pan);
        self.sing = 1.0;
        self.sing_hold = CELEBRATION_OUTRO_S;
    }

    /// Render interleaved stereo into `out` (length must be even).
    #[inline]
    pub const fn render(&mut self, out: &mut [f32]) {
        debug_assert!(out.len().is_multiple_of(CHANNELS));
        let frames = out.len() / CHANNELS;
        if frames == 0 {
            return;
        }
        let dt_block = frames as f32 * self.inv_sr;
        self.clock_s += dt_block as f64;
        self.since_event += dt_block;
        self.since_voice += dt_block;
        self.since_erase += dt_block;
        self.since_space_step += dt_block;
        self.since_shift += dt_block;
        if self.rate > 0.0 {
            const INV_RATE_TAU: f32 = -1.0 / 0.6;
            self.rate *= crate::color_math::exp_f32(dt_block * INV_RATE_TAU);
            if self.rate < 1e-7 {
                self.rate = 0.0;
            }
        }

        if self.is_quiet() {
            let mut i = 0;
            while i < out.len() {
                out[i] = 0.0;
                i += 1;
            }
            self.bed.level = 0.0;
            self.bed.energy = 0.0;
            self.bed.out_lvl = 0.0;
            self.bed.hp_m = 0.0;
            self.bed.hp_s = 0.0;
            self.dc_x_l = 0.0;
            self.dc_y_l = 0.0;
            self.dc_x_r = 0.0;
            self.dc_y_r = 0.0;
            self.duck = 0.0;
            self.sing = 0.0;
            self.sing_hold = 0.0;
            self.release_song_key();
            self.lim_env_l = 0.0;
            self.lim_g_l = 1.0;
            self.lim_env_r = 0.0;
            self.lim_g_r = 1.0;
            return;
        }

        self.tick_bed(dt_block);

        let dt = self.inv_sr;
        let duck_step = if self.duck > 0.0 {
            const INV_BONK_DUCK_TAU: f32 = -1.0 / BONK_DUCK_TAU;
            crate::color_math::exp_f32(dt * INV_BONK_DUCK_TAU)
        } else {
            0.0
        };
        let bed_target = if self.bed.level >= 1e-4 {
            self.bed.level * self.bed.gain
        } else {
            0.0
        };
        let bed_on = bed_target > 0.0 || self.bed.out_lvl >= 1e-5;
        let bed_slew = if bed_on {
            const INV_BED_OUT_TAU_S: f32 = -1.0 / 0.012;
            1.0 - crate::color_math::exp_f32(dt * INV_BED_OUT_TAU_S)
        } else {
            0.0
        };
        let bed_palette = if bed_on {
            Some(palette_for(self.bed_voice, self.bed_style))
        } else {
            None
        };
        let sing_mul = 1.0 - SING_DUCK_DEPTH * self.sing;
        let lim_on = self.v2_latched;
        let (lim_att, lim_rel) = if lim_on {
            const INV_LIMIT_ATTACK_S: f32 = -1.0 / LIMIT_ATTACK_S;
            const INV_LIMIT_RELEASE_S: f32 = -1.0 / LIMIT_RELEASE_S;
            (
                1.0 - crate::color_math::exp_f32(dt * INV_LIMIT_ATTACK_S),
                crate::color_math::exp_f32(dt * INV_LIMIT_RELEASE_S),
            )
        } else {
            (0.0, 0.0)
        };
        const DC_BLOCK_HZ: f32 = 40.0;
        let dc_r = crate::color_math::exp_f32(-core::f32::consts::TAU * DC_BLOCK_HZ * dt);
        let dc_g = 0.5 * (1.0 + dc_r);
        let mut active_voices = [0u8; MAX_VOICES];
        let mut active_len = 0usize;
        let mut i = 0;
        while i < self.voices.len() {
            if self.voices[i].on {
                active_voices[active_len] = i as u8;
                active_len += 1;
            }
            i += 1;
        }
        let mut f = 0;
        while f < frames {
            let (mut l, mut r) = (0.0f32, 0.0f32);
            let (mut xl, mut xr) = (0.0f32, 0.0f32);

            let mut read_idx = 0;
            let mut write_idx = 0;
            while read_idx < active_len {
                let vi = active_voices[read_idx] as usize;
                read_idx += 1;
                let v = &mut self.voices[vi];
                v.t += dt;
                if v.damp > 0.0 {
                    v.damp -= dt;
                    if v.damp <= 0.0 {
                        v.on = false;
                        continue;
                    }
                }
                if v.arm_ttl > 0.0 {
                    v.arm_ttl -= dt;
                    if v.arm_ttl <= 0.0 {
                        v.arm_ttl = 0.0;
                        if v.damp <= 0.0 {
                            v.damp = LANE_FADE_STEAL_S;
                            v.damp0 = LANE_FADE_STEAL_S;
                        }
                    }
                }
                if v.t < 0.0 {
                    active_voices[write_idx] = vi as u8;
                    write_idx += 1;
                    continue;
                }
                if v.t >= v.dur {
                    v.on = false;
                    continue;
                }
                if !v.env_run && v.lane != LANE_NONE && v.delay > 0.0 && self.lane_onset_steal(vi) {
                    self.voices[vi].on = false;
                    continue;
                }
                active_voices[write_idx] = vi as u8;
                write_idx += 1;
                let v = &mut self.voices[vi];
                let seed = !v.env_run || v.env_n >= ENV_REANCHOR;
                if seed {
                    v.env_n = 0;
                } else {
                    v.env_n += 1;
                }
                let mut s = 0.0f32;
                let mut next_pi = 0;
                while next_pi < v.p.len() {
                    let p = &mut v.p[next_pi];
                    next_pi += 1;
                    if p.lvl <= 0.0 {
                        continue;
                    }
                    let freq = if p.glide > 0.0 {
                        p.g_e = if seed {
                            crate::color_math::exp_f32(-v.t / p.glide)
                        } else {
                            p.g_e * p.k_g
                        };
                        p.f1 + (p.f0 - p.f1) * p.g_e
                    } else {
                        p.f0
                    };
                    let ph_inc = freq * dt;
                    p.ph = (p.ph + ph_inc).fract();
                    let x = match p.wave {
                        Wave::Sine => {
                            if p.fm_ratio > 0.0 {
                                p.fm_ph = (p.fm_ph + freq * p.fm_ratio * dt).fract();
                                p.fm_e = if seed {
                                    crate::color_math::exp_f32(-v.t / p.fm_tau.max(1e-3))
                                } else {
                                    p.fm_e * p.k_f
                                };
                                let idx = p.fm_i0 * p.fm_e;
                                sin01(p.ph + idx * sin01(p.fm_ph) * 0.159_154_94)
                            } else {
                                sin01(p.ph)
                            }
                        }
                        Wave::Pulse { duty } => {
                            if p.ph < duty {
                                1.0
                            } else {
                                -1.0
                            }
                        }
                    };
                    if p.decay > 0.0 {
                        p.p_e = if seed {
                            crate::color_math::exp_f32(-v.t / p.decay)
                        } else {
                            p.p_e * p.k_p
                        };
                        s += p.lvl * x * p.p_e;
                    } else {
                        s += p.lvl * x;
                    }
                }
                if v.n_lvl > 0.0 {
                    let white = xorshift01(&mut self.rng) * 2.0 - 1.0;
                    let damp = v.n_damp;
                    let (g_svf, den) = if v.n_glide > 0.0 {
                        v.n_e = if seed {
                            crate::color_math::exp_f32(-v.t / v.n_glide)
                        } else {
                            v.n_e * v.k_n
                        };
                        let fc = v.n_f1 + (v.n_f0 - v.n_f1) * v.n_e;
                        let (sn, cs) = crate::supernova::sin_cos_f32(
                            core::f32::consts::PI * (fc * dt).min(0.45),
                        );
                        let g = sn / cs;
                        (g, 1.0 + g * (g + damp))
                    } else {
                        (v.svf_g, v.svf_den)
                    };
                    let hp = (white - v.n_lp - damp * v.n_bp) / den;
                    v.n_bp += g_svf * hp;
                    v.n_lp += g_svf * v.n_bp;
                    if v.n_decay > 0.0 {
                        v.nd_e = if seed {
                            crate::color_math::exp_f32(-v.t / v.n_decay)
                        } else {
                            v.nd_e * v.k_nd
                        };
                        s += v.n_lvl * v.n_bp * v.nd_e;
                    } else {
                        s += v.n_lvl * v.n_bp;
                    }
                }
                if seed {
                    v.env_a = crate::color_math::exp_f32(-v.t / v.attack.max(2e-4));
                    v.env_d = crate::color_math::exp_f32(-v.t / v.decay.max(1e-3));
                    v.env_run = true;
                } else {
                    v.env_a *= v.k_a;
                    v.env_d *= v.k_d;
                }
                let mut env = (1.0 - v.env_a) * v.env_d;
                if v.tw_depth > 0.0 {
                    v.tw_ph = (v.tw_ph + v.tw_rate * dt).fract();
                    env *= 1.0 - v.tw_depth * 0.5 * (1.0 + sin01(v.tw_ph));
                }
                let rel = ((v.dur - v.t) * 200.0).clamp(0.0, 1.0);
                env *= rel;
                if v.damp > 0.0 {
                    env *= v.damp / v.damp0.max(1e-4);
                }
                v.lp += v.lp_k * (s - v.lp);
                let y = v.lp * env;
                let (gl, gr) = if v.pan_glide_s > 0.0 {
                    let u = ((v.t - v.pan_t0) * v.pan_k).clamp(0.0, 1.0);
                    (v.gl + (v.gl1 - v.gl) * u, v.gr + (v.gr1 - v.gr) * u)
                } else {
                    (v.gl, v.gr)
                };
                if v.duck_exempt {
                    xl += y * gl;
                    xr += y * gr;
                } else {
                    l += y * gl;
                    r += y * gr;
                }
            }
            active_len = write_idx;

            if let Some(palette) = bed_palette {
                self.bed.out_lvl += (bed_target - self.bed.out_lvl) * bed_slew;
                let (bl, br) = self.bed_sample_with(palette, dt, self.bed.out_lvl);
                l += bl;
                r += br;
            }

            let dmul = (1.0 - BONK_DUCK_DEPTH * self.duck) * sing_mul;
            let l = l * dmul + xl;
            let r = r * dmul + xr;
            self.duck *= duck_step;

            let xl = l * MASTER;
            let yl = dc_g * (xl - self.dc_x_l) + dc_r * self.dc_y_l;
            self.dc_x_l = xl;
            self.dc_y_l = yl;
            let xr = r * MASTER;
            let yr = dc_g * (xr - self.dc_x_r) + dc_r * self.dc_y_r;
            self.dc_x_r = xr;
            self.dc_y_r = yr;
            let (yl, yr) = if lim_on {
                (
                    limit(yl, &mut self.lim_env_l, &mut self.lim_g_l, lim_att, lim_rel),
                    limit(yr, &mut self.lim_env_r, &mut self.lim_g_r, lim_att, lim_rel),
                )
            } else {
                (yl, yr)
            };
            let base = f * 2;
            out[base] = soft_clip(yl);
            out[base + 1] = soft_clip(yr);
            f += 1;
        }
        if bed_target == 0.0 && self.bed.out_lvl < 1e-5 {
            self.bed.out_lvl = 0.0;
            self.bed.hp_m = 0.0;
            self.bed.hp_s = 0.0;
        }
        if self.duck < 1e-4 {
            self.duck = 0.0;
        }
        if self.dc_y_l.abs() < 1e-6 && self.dc_x_l.abs() < 1e-6 {
            self.dc_x_l = 0.0;
            self.dc_y_l = 0.0;
        }
        if self.dc_y_r.abs() < 1e-6 && self.dc_x_r.abs() < 1e-6 {
            self.dc_x_r = 0.0;
            self.dc_y_r = 0.0;
        }
        if self.sing > 0.0 {
            if self.sing_hold > 0.0 {
                self.sing_hold -= dt_block;
            } else {
                const INV_SING_DUCK_TAU: f32 = -1.0 / SING_DUCK_TAU;
                self.sing *= crate::color_math::exp_f32(dt_block * INV_SING_DUCK_TAU);
            }
            if self.sing < 1e-4 {
                self.sing = 0.0;
                self.release_song_key();
            }
        }
    }

    /// Feed the bed for one admitted trail gesture: [`bed_kick`]'s energy for the kind and the smoothed gain for the user volume.
    #[inline]
    pub(super) const fn kick_bed(&mut self, kind: SoundKind, gain: f32) {
        self.bed.energy = (self.bed.energy + bed_kick(kind)).min(1.0);
        self.bed.gain += (gain - self.bed.gain) * 0.3;
    }

    #[inline]
    const fn tick_bed(&mut self, dt: f32) {
        let b = &mut self.bed;
        if b.energy > 0.0 {
            let inv_tau_e = if b.energy < 0.02 { -5.0 } else { -1.0 / 0.32 };
            b.energy *= crate::color_math::exp_f32(dt * inv_tau_e);
        }
        let target = b.energy.min(1.0);
        if b.level != target {
            let inv_tau = if target > b.level {
                -4.0
            } else if b.level < 0.02 {
                -5.0
            } else {
                -1.0 / 0.32
            };
            b.level += (target - b.level) * (1.0 - crate::color_math::exp_f32(dt * inv_tau));
        }
        if b.energy < 3e-3 && b.level < 3e-3 {
            b.energy = 0.0;
            b.level = 0.0;
        }
        const INV_035: f32 = 1.0 / 0.35;
        let dying = (0.35 - b.level).max(0.0) * INV_035;
        if b.droop != dying {
            const INV_DROOP_TAU: f32 = -1.0 / 0.4;
            b.droop += (dying - b.droop) * (1.0 - crate::color_math::exp_f32(dt * INV_DROOP_TAU));
            if b.level == 0.0 && (dying - b.droop).abs() < 1e-6 {
                b.droop = dying;
            }
        }

        if b.level > 0.02 {
            b.timer -= dt;
            if b.timer <= 0.0 {
                let style = self.bed_style;
                let voice = self.bed_voice;
                let level = self.bed.level;
                let gain = self.bed.gain;
                palette_for(voice, style).bed_grain(self, level, gain);
            }
        }
    }

    #[cfg(test)]
    #[inline]
    pub(super) const fn bed_sample(&mut self, dt: f32) -> (f32, f32) {
        let palette = palette_for(self.bed_voice, self.bed_style);
        let lvl = self.bed.level * self.bed.gain;
        self.bed_sample_with(palette, dt, lvl)
    }

    #[inline]
    const fn bed_sample_with(&mut self, palette: PaletteKind, dt: f32, lvl: f32) -> (f32, f32) {
        self.bed.lfo1 = (self.bed.lfo1 + 0.23 * dt).fract();
        self.bed.lfo2 = (self.bed.lfo2 + 0.71 * dt).fract();

        let (m, side) = palette.bed_sample(self, dt, lvl);
        if matches!(palette, PaletteKind::Mech | PaletteKind::Typewriter) {
            return (0.0, 0.0);
        }
        const BED_HP_HZ: f32 = 220.0;
        let b = &mut self.bed;
        if b.hp_dt != dt {
            b.hp_dt = dt;
            b.hp_k = 1.0 - crate::color_math::exp_f32(-core::f32::consts::TAU * BED_HP_HZ * dt);
        }
        let k_hp = b.hp_k;
        b.hp_m += k_hp * (m - b.hp_m);
        b.hp_s += k_hp * (side - b.hp_s);
        let m = m - b.hp_m;
        let side = side - b.hp_s;
        (m + side, m - side)
    }
}

#[derive(Clone, Copy)]
enum PaletteKind {
    Lumen,
    Phaser,
    RainbowKittyV2,
    Sparkle,
    Fire,
    Laser,
    Beam,
    Water,
    Comet,
    Mech,
    Typewriter,
    Marimba,
    Felt,
}

impl PaletteKind {
    #[inline]
    const fn design(
        self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        deg: i32,
        col_off: i32,
    ) {
        match self {
            Self::Lumen => LumenPalette::design(s, ev, kind, g, deg),
            Self::Phaser => PhaserPalette::design(s, ev, kind, g, col_off),
            Self::RainbowKittyV2 => {}
            Self::Sparkle => SparklePalette::design(s, ev, kind, g, deg),
            Self::Fire => FirePalette::design(s, ev, kind, g),
            Self::Laser => LaserPalette::design(s, ev, kind, g, deg),
            Self::Beam => BeamPalette::design(s, ev, kind, g, deg),
            Self::Water => WaterPalette::design(s, ev, kind, g, deg),
            Self::Comet => CometPalette::design(s, ev, kind, g, deg),
            Self::Mech => MechPalette::design(s, ev, kind, g),
            Self::Typewriter => TypewriterPalette::design(s, ev, kind, g),
            Self::Marimba => MarimbaPalette::design(s, ev, kind, g, deg),
            Self::Felt => FeltPalette::design(s, ev, kind, g, deg),
        }
    }

    #[inline]
    const fn bed_grain(self, s: &mut TrailSynth, level: f32, gain: f32) {
        match self {
            Self::Phaser => PhaserPalette::bed_grain(s, level, gain),
            Self::Sparkle => SparklePalette::bed_grain(s, level, gain),
            Self::Fire => FirePalette::bed_grain(s, level, gain),
            Self::Laser => LaserPalette::bed_grain(s, level, gain),
            Self::Water => WaterPalette::bed_grain(s, level, gain),
            Self::Comet => CometPalette::bed_grain(s, level, gain),
            _ => s.bed.timer = 0.25,
        }
    }

    #[inline]
    const fn bed_sample(self, s: &mut TrailSynth, dt: f32, lvl: f32) -> (f32, f32) {
        match self {
            Self::Lumen => LumenPalette::bed_sample(s, dt, lvl),
            Self::Phaser => PhaserPalette::bed_sample(s, dt, lvl),
            Self::RainbowKittyV2 => RainbowKittyV2Palette::bed_sample(s, dt, lvl),
            Self::Sparkle => SparklePalette::bed_sample(s, dt, lvl),
            Self::Fire => FirePalette::bed_sample(s, dt, lvl),
            Self::Laser => LaserPalette::bed_sample(s, dt, lvl),
            Self::Beam => BeamPalette::bed_sample(s, dt, lvl),
            Self::Water => WaterPalette::bed_sample(s, dt, lvl),
            Self::Comet => CometPalette::bed_sample(s, dt, lvl),
            Self::Marimba => MarimbaPalette::bed_sample(s, dt, lvl),
            Self::Felt => FeltPalette::bed_sample(s, dt, lvl),
            Self::Mech | Self::Typewriter => (0.0, 0.0),
        }
    }

    #[inline]
    const fn anchor_hz(self) -> f32 {
        match self {
            Self::Phaser => 392.0,
            Self::RainbowKittyV2 => RainbowKittyV2Palette::anchor_hz(),
            Self::Sparkle => 523.25,
            Self::Laser => 880.0,
            Self::Beam => 330.0,
            Self::Water => 430.0,
            Self::Comet => 220.0,
            Self::Marimba => MarimbaPalette::BASE_HZ,
            Self::Felt => FeltPalette::BASE_HZ,
            Self::Lumen | Self::Fire | Self::Mech | Self::Typewriter => CURSOR_ANCHOR_HZ,
        }
    }
}

#[inline]
const fn palette_for(voice: SoundVoice, style: GlowStyle) -> PaletteKind {
    match voice {
        SoundVoice::Mech => PaletteKind::Mech,
        SoundVoice::Typewriter => PaletteKind::Typewriter,
        SoundVoice::Marimba => PaletteKind::Marimba,
        SoundVoice::Felt => PaletteKind::Felt,
        SoundVoice::RainbowKittyV2 => PaletteKind::RainbowKittyV2,
        SoundVoice::Of(s) => palette_for(SoundVoice::Style, s),
        SoundVoice::Style => match style {
            GlowStyle::Lumen => PaletteKind::Lumen,
            GlowStyle::Phaser | GlowStyle::Classic => PaletteKind::Phaser,
            GlowStyle::RainbowKitty => PaletteKind::RainbowKittyV2,
            GlowStyle::Sparkle => PaletteKind::Sparkle,
            GlowStyle::Fire => PaletteKind::Fire,
            GlowStyle::Laser => PaletteKind::Laser,
            GlowStyle::Beam => PaletteKind::Beam,
            GlowStyle::Water => PaletteKind::Water,
            GlowStyle::Comet => PaletteKind::Comet,
        },
    }
}

struct LumenPalette;

impl LumenPalette {
    #[inline]
    const fn design(s: &mut TrailSynth, ev: &SoundEvent, kind: SoundKind, g: f32, deg: i32) {
        let f = s.melody_hz(330.0, deg);
        let (f0, f1v) = (f * 0.97, f);
        let v = Voice {
            dur: 0.24,
            attack: 0.005,
            decay: 0.09,
            p: [
                Partial {
                    lvl: 0.62,
                    f0,
                    f1: f1v,
                    glide: 0.05,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: 0.22,
                    f0: f0 * 1.004,
                    f1: f1v * 1.004,
                    glide: 0.05,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: 0.16,
                    f0: f0 * 0.5,
                    f1: f1v * 0.5,
                    glide: 0.05,
                    ..Partial::DEFAULT
                },
            ],
            n_lvl: 0.02,
            n_f0: 3400.0,
            n_f1: 3400.0,
            n_glide: 0.0,
            n_q: 0.6,
            lp_cut: 2000.0,
            ..Voice::DEFAULT
        };
        s.spawn(v, g * 0.42, ev.pan);
        if matches!(kind, SoundKind::Jump) {
            let f2 = f * 1.5;
            let v2 = Voice {
                delay: 0.055,
                dur: 0.26,
                attack: 0.005,
                decay: 0.09,
                p: [
                    Partial {
                        lvl: 0.6,
                        f0: f2 * 0.97,
                        f1: f2,
                        glide: 0.05,
                        ..Partial::DEFAULT
                    },
                    Partial {
                        lvl: 0.2,
                        f0: f2 * 1.004,
                        f1: f2 * 1.004,
                        ..Partial::DEFAULT
                    },
                    Partial::DEFAULT,
                ],
                lp_cut: 2200.0,
                ..Voice::DEFAULT
            };
            s.spawn(v2, g * 0.3, ev.pan);
        }
    }

    #[inline]
    const fn bed_sample(s: &mut TrailSynth, dt: f32, lvl: f32) -> (f32, f32) {
        let u1 = s.bed.lfo_u1();
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 330.0 * dt).fract();
        b.ph2 = (b.ph2 + 330.8 * dt).fract();
        b.ph3 = (b.ph3 + 495.0 * dt).fract();
        let sm = sin01(b.ph1) + sin01(b.ph2) + (0.1 + 0.2 * u1) * sin01(b.ph3);
        (sm * lvl * 0.015, sm * lvl * 0.004)
    }
}

struct PhaserPalette;

impl PhaserPalette {
    #[inline]
    const fn design(s: &mut TrailSynth, ev: &SoundEvent, kind: SoundKind, g: f32, col_off: i32) {
        let hue_deg = (ev.hue * 5.0) as i32;
        let d = hue_deg + col_off + gesture_shape(kind).offset + shift_lift(ev, kind);
        let f = s.melody_hz(392.0, d);
        let (f0, f1v) = (f * 1.09, f);
        let v = Voice {
            dur: 0.18,
            attack: 0.004,
            decay: if matches!(kind, SoundKind::Jump) {
                0.09
            } else {
                0.075
            },
            p: [
                Partial {
                    lvl: 0.75,
                    f0,
                    f1: f1v,
                    glide: 0.022,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: 0.12,
                    f0: f * 2.0,
                    f1: f1v * 2.0,
                    glide: 0.022,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
            ],
            lp_cut: 1900.0,
            ..Voice::DEFAULT
        };
        s.spawn(v, g * 0.4, ev.pan);
        let t = Voice {
            dur: 0.04,
            attack: 0.0008,
            decay: 0.014,
            n_lvl: 0.5,
            n_f0: 420.0,
            n_f1: 300.0,
            n_glide: 0.01,
            n_q: 1.1,
            lp_cut: 1200.0,
            ..Voice::DEFAULT
        };
        s.spawn(t, g * 0.25, ev.pan);
        if matches!(kind, SoundKind::Jump) {
            let f2 = s.melody_hz(392.0, d + 3);
            let v2 = Voice {
                delay: 0.07,
                dur: 0.22,
                attack: 0.004,
                decay: 0.09,
                p: [
                    Partial {
                        lvl: 0.7,
                        f0: f2 * 1.09,
                        f1: f2,
                        glide: 0.022,
                        ..Partial::DEFAULT
                    },
                    Partial {
                        lvl: 0.1,
                        f0: f2 * 2.0,
                        f1: f2 * 2.0,
                        fm_ratio: 3.01,
                        fm_i0: 0.4,
                        fm_tau: 0.05,
                        ..Partial::DEFAULT
                    },
                    Partial::DEFAULT,
                ],
                lp_cut: 2100.0,
                ..Voice::DEFAULT
            };
            s.spawn(v2, g * 0.34, ev.pan * 0.5);
        }
    }

    #[inline]
    const fn bed_grain(s: &mut TrailSynth, level: f32, gain: f32) {
        let grain_deg = (s.rnd() * 5.0) as i32;
        let f = s.melody_hz(392.0, grain_deg);
        let v = Voice {
            dur: 0.14,
            attack: 0.005,
            decay: 0.055,
            p: [
                Partial {
                    lvl: 0.6,
                    f0: f * 1.06,
                    f1: f,
                    glide: 0.03,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
                Partial::DEFAULT,
            ],
            lp_cut: 1700.0,
            ..Voice::DEFAULT
        };
        let pan = s.rnd_in(-0.5, 0.5);
        s.spawn(v, gain * level * 0.05, pan);
        s.bed.timer = s.rnd_in(0.45, 1.2) / level.max(0.05);
    }

    #[inline]
    const fn bed_sample(s: &mut TrailSynth, dt: f32, lvl: f32) -> (f32, f32) {
        let u1 = s.bed.lfo_u1();
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 392.0 * dt).fract();
        b.ph2 = (b.ph2 + 393.0 * dt).fract();
        let sm = tri(b.ph1) + tri(b.ph2);
        let k = ((520.0 + 480.0 * u1) * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (sm - b.lp1);
        (b.lp1 * lvl * 0.028, 0.0)
    }
}

const KEY_BELL_BEND_SHARE: f32 = 0.33;
const KEY_BELL_BEND_TAU: f32 = 0.004;

struct SparklePalette;

impl SparklePalette {
    #[inline]
    const fn design(s: &mut TrailSynth, ev: &SoundEvent, kind: SoundKind, g: f32, deg: i32) {
        let n = if matches!(kind, SoundKind::Jump) {
            3
        } else {
            2
        };
        let mut i = 0;
        while i < n {
            let d = deg + i * 2 + (s.rnd() * 2.0) as i32;
            let f = s.melody_hz(523.25, d);
            let delay = i as f32 * s.rnd_in(0.022, 0.05);
            let v = Voice {
                delay,
                dur: 0.55,
                attack: 0.003,
                decay: s.rnd_in(0.14, 0.22),
                p: [
                    Partial {
                        lvl: 0.5,
                        f0: f,
                        f1: f,
                        fm_ratio: 3.01,
                        fm_i0: 0.6,
                        fm_tau: 0.04,
                        ..Partial::DEFAULT
                    },
                    Partial {
                        lvl: 0.15,
                        f0: f * 0.5,
                        f1: f * 0.5,
                        ..Partial::DEFAULT
                    },
                    Partial {
                        lvl: 0.08,
                        f0: f * 2.01,
                        f1: f * 2.01,
                        ..Partial::DEFAULT
                    },
                ],
                tw_rate: s.rnd_in(7.0, 11.0),
                tw_depth: 0.4,
                lp_cut: 2800.0,
                ..Voice::DEFAULT
            };
            let pan = ev.pan + s.rnd_in(-0.4, 0.4);
            s.spawn(v, g * 0.28 / (1.0 + i as f32 * 0.4), pan);
            i += 1;
        }
    }

    #[inline]
    const fn bed_grain(s: &mut TrailSynth, level: f32, gain: f32) {
        let grain_deg = (s.rnd() * 8.0) as i32;
        let f = s.melody_hz(523.25, grain_deg);
        let v = Voice {
            dur: 0.45,
            attack: 0.003,
            decay: 0.14,
            p: [
                Partial {
                    lvl: 0.5,
                    f0: f,
                    f1: f,
                    fm_ratio: 3.01,
                    fm_i0: 0.5,
                    fm_tau: 0.04,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
                Partial::DEFAULT,
            ],
            tw_rate: 9.0,
            tw_depth: 0.4,
            lp_cut: 2800.0,
            ..Voice::DEFAULT
        };
        let pan = s.rnd_in(-0.8, 0.8);
        s.spawn(v, gain * level * 0.08, pan);
        s.bed.timer = s.rnd_in(0.15, 0.5) / level.max(0.05);
    }

    #[inline]
    const fn bed_sample(s: &mut TrailSynth, dt: f32, lvl: f32) -> (f32, f32) {
        let u2 = s.bed.lfo_u2();
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 261.6 * dt).fract();
        b.ph2 = (b.ph2 + 262.4 * dt).fract();
        b.ph3 = (b.ph3 + 523.9 * dt).fract();
        let sm = sin01(b.ph1) + sin01(b.ph2) + (0.08 + 0.15 * u2) * sin01(b.ph3);
        (sm * lvl * 0.022, sm * lvl * 0.006)
    }
}

struct FirePalette;

impl FirePalette {
    #[inline]
    const fn design(s: &mut TrailSynth, ev: &SoundEvent, kind: SoundKind, g: f32) {
        // Crackle: every key is an impulsive few-ms high-Q snap over a woody
        // ember knock; impulsiveness separates fire from water. Jump is the
        // flame leaping: a dark low whoomph plus a scatter of spark snaps.
        if matches!(kind, SoundKind::Jump) {
            let v = Voice {
                dur: 0.25,
                attack: 0.012,
                decay: 0.09,
                n_lvl: 0.7,
                n_f0: 160.0,
                n_f1: 420.0,
                n_glide: 0.06,
                n_q: 1.1,
                lp_cut: 900.0,
                ..Voice::DEFAULT
            };
            s.spawn(v, g * 0.5, ev.pan);
            let mut i = 0;
            while i < 4 {
                let centre = s.rnd_in(2000.0, 5000.0);
                let v = Voice {
                    delay: 0.02 + i as f32 * 0.05 + s.rnd_in(0.0, 0.03),
                    dur: 0.05,
                    attack: 0.0003,
                    decay: s.rnd_in(0.006, 0.012),
                    n_lvl: 0.9,
                    n_f0: centre,
                    n_f1: centre * 0.85,
                    n_glide: 0.008,
                    n_q: s.rnd_in(3.5, 6.0),
                    lp_cut: 5200.0,
                    ..Voice::DEFAULT
                };
                let pan = ev.pan + s.rnd_in(-0.5, 0.5);
                let sg = g * s.rnd_in(0.16, 0.3);
                s.spawn(v, sg, pan);
                i += 1;
            }
        } else {
            let centre = s.rnd_in(2000.0, 4800.0);
            let v = Voice {
                dur: 0.05,
                attack: 0.0003,
                decay: s.rnd_in(0.006, 0.014),
                n_lvl: 0.9,
                n_f0: centre,
                n_f1: centre * 0.85,
                n_glide: 0.008,
                n_q: s.rnd_in(3.5, 6.0),
                lp_cut: 5200.0,
                ..Voice::DEFAULT
            };
            s.spawn(v, g * 0.42, ev.pan);
            if s.rnd() < 0.35 {
                let c2 = s.rnd_in(1600.0, 4200.0);
                let v2 = Voice {
                    delay: s.rnd_in(0.012, 0.04),
                    dur: 0.04,
                    attack: 0.0003,
                    decay: s.rnd_in(0.005, 0.01),
                    n_lvl: 0.9,
                    n_f0: c2,
                    n_f1: c2 * 0.85,
                    n_glide: 0.008,
                    n_q: s.rnd_in(3.5, 6.0),
                    lp_cut: 5200.0,
                    ..Voice::DEFAULT
                };
                let pan = ev.pan + s.rnd_in(-0.3, 0.3);
                s.spawn(v2, g * 0.22, pan);
            }
            let fe = s.rnd_in(85.0, 125.0) * shift_ratio(ev, kind);
            let v3 = Voice {
                dur: 0.09,
                attack: 0.002,
                decay: 0.035,
                p: [
                    Partial {
                        lvl: 0.7,
                        f0: fe * 1.3,
                        f1: fe,
                        glide: 0.03,
                        ..Partial::DEFAULT
                    },
                    Partial::DEFAULT,
                    Partial::DEFAULT,
                ],
                lp_cut: 450.0,
                ..Voice::DEFAULT
            };
            s.spawn(v3, g * (0.1 + 0.28 * ev.heat), ev.pan * 0.5);
        }
    }

    #[inline]
    const fn bed_grain(s: &mut TrailSynth, level: f32, gain: f32) {
        let low = s.rnd() < 0.12;
        let (centre, q, decay, lvl) = if low {
            (s.rnd_in(240.0, 480.0), 2.0, 0.02, 0.2)
        } else {
            (
                s.rnd_in(1400.0, 4600.0),
                s.rnd_in(3.0, 6.0),
                s.rnd_in(0.005, 0.012),
                0.14,
            )
        };
        let v = Voice {
            dur: 0.035,
            attack: 0.0005,
            decay,
            n_lvl: 0.9,
            n_f0: centre,
            n_f1: centre * 0.85,
            n_glide: 0.008,
            n_q: q,
            lp_cut: 5200.0,
            ..Voice::DEFAULT
        };
        let pan = s.rnd_in(-0.6, 0.6);
        s.spawn(v, gain * level * lvl, pan);
        s.bed.timer = s.rnd_in(0.03, 0.18) / level.max(0.05);
    }

    #[inline]
    const fn bed_sample(s: &mut TrailSynth, dt: f32, lvl: f32) -> (f32, f32) {
        let white = s.rnd() * 2.0 - 1.0;
        let b = &mut s.bed;
        let k = (380.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (white - b.lp1);
        let kf = (9.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp2 += kf * (white - b.lp2);
        let flick = (1.0 + 26.0 * b.lp2).clamp(0.35, 1.8);
        (b.lp1 * lvl * 0.4 * flick, 0.0)
    }
}

struct LaserPalette;

impl LaserPalette {
    #[inline]
    const fn design(s: &mut TrailSynth, ev: &SoundEvent, kind: SoundKind, g: f32, deg: i32) {
        let lift = shift_lift(ev, kind);
        let base_deg = deg - lift;
        let clamped_deg = if base_deg < 5 { base_deg } else { 5 };
        let f_hi = s.melody_hz(880.0, clamped_deg + lift);
        let f_lo = f_hi * 0.25;
        let is_jump = matches!(kind, SoundKind::Jump);
        let dur = if is_jump { 0.2 } else { 0.11 };
        let v = Voice {
            dur,
            attack: 0.001,
            decay: if is_jump { 0.08 } else { 0.045 },
            p: [
                Partial {
                    lvl: 0.7,
                    f0: f_hi,
                    f1: f_lo,
                    glide: 0.024,
                    fm_ratio: 7.03,
                    fm_i0: 1.2,
                    fm_tau: 0.012,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: 0.15,
                    f0: f_hi * 2.0,
                    f1: f_lo * 2.0,
                    glide: 0.024,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
            ],
            n_lvl: 0.08,
            n_f0: 3200.0,
            n_f1: 1400.0,
            n_glide: 0.02,
            n_q: 1.0,
            lp_cut: 3800.0,
            ..Voice::DEFAULT
        };
        s.spawn(v, g * 0.4, ev.pan);
        let cf = s.rnd_in(3200.0, 5200.0);
        let crack = Voice {
            dur: 0.03,
            attack: 0.0002,
            decay: 0.008,
            n_lvl: 0.9,
            n_f0: cf,
            n_f1: cf * 0.8,
            n_glide: 0.006,
            n_q: 4.5,
            lp_cut: 6000.0,
            ..Voice::DEFAULT
        };
        s.spawn(crack, g * 0.2, ev.pan);
        if is_jump {
            let v2 = Voice {
                dur: 0.14,
                attack: 0.003,
                decay: 0.06,
                p: [
                    Partial {
                        lvl: 0.8,
                        f0: 140.0,
                        f1: 90.0,
                        glide: 0.05,
                        ..Partial::DEFAULT
                    },
                    Partial::DEFAULT,
                    Partial::DEFAULT,
                ],
                lp_cut: 400.0,
                ..Voice::DEFAULT
            };
            s.spawn(v2, g * 0.3, ev.pan * 0.4);
            let roll = Voice {
                delay: 0.06,
                dur: 1.1,
                attack: 0.05,
                decay: 0.38,
                n_lvl: 0.8,
                n_f0: 320.0,
                n_f1: 85.0,
                n_glide: 0.25,
                n_q: 0.75,
                lp_cut: 500.0,
                ..Voice::DEFAULT
            };
            s.spawn(roll, g * 0.45, ev.pan * 0.3);
            let echo = Voice {
                delay: 0.42,
                dur: 0.9,
                attack: 0.09,
                decay: 0.3,
                n_lvl: 0.7,
                n_f0: 220.0,
                n_f1: 70.0,
                n_glide: 0.22,
                n_q: 0.75,
                lp_cut: 380.0,
                ..Voice::DEFAULT
            };
            s.spawn(echo, g * 0.22, -ev.pan * 0.5);
        }
    }

    #[inline]
    const fn bed_grain(s: &mut TrailSynth, level: f32, gain: f32) {
        // The storm keeps grumbling: mostly distant rolls, now and then the
        // faint tick of a far-off strike.
        if s.rnd() < 0.25 {
            let cf = s.rnd_in(3000.0, 5200.0);
            let tick = Voice {
                dur: 0.03,
                attack: 0.0005,
                decay: 0.008,
                n_lvl: 0.9,
                n_f0: cf,
                n_f1: cf * 0.8,
                n_glide: 0.006,
                n_q: 5.0,
                lp_cut: 6000.0,
                ..Voice::DEFAULT
            };
            let pan = s.rnd_in(-0.8, 0.8);
            s.spawn(tick, gain * level * 0.05, pan);
        } else {
            let roll = Voice {
                dur: 0.7,
                attack: 0.08,
                decay: 0.28,
                n_lvl: 0.8,
                n_f0: 260.0,
                n_f1: 90.0,
                n_glide: 0.3,
                n_q: 0.8,
                lp_cut: 420.0,
                ..Voice::DEFAULT
            };
            let pan = s.rnd_in(-0.7, 0.7);
            s.spawn(roll, gain * level * 0.18, pan);
        }
        s.bed.timer = s.rnd_in(0.5, 1.4) / level.max(0.05);
    }

    #[inline]
    const fn bed_sample(s: &mut TrailSynth, dt: f32, lvl: f32) -> (f32, f32) {
        let white = s.rnd() * 2.0 - 1.0;
        let b = &mut s.bed;
        let k = (420.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (white - b.lp1);
        let ks = (1.3 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp2 += ks * (white - b.lp2);
        let swell = (1.0 + 40.0 * b.lp2).clamp(0.25, 2.2);
        (b.lp1 * lvl * 0.28 * swell, 0.0)
    }
}

struct BeamPalette;

impl BeamPalette {
    #[inline]
    const fn design(s: &mut TrailSynth, ev: &SoundEvent, kind: SoundKind, g: f32, deg: i32) {
        let lift = shift_lift(ev, kind);
        let f = s.melody_hz(330.0, ((deg - lift) / 2) * 2 + lift);
        let thud = Voice {
            dur: 0.035,
            attack: 0.0008,
            decay: 0.013,
            n_lvl: 0.6,
            n_f0: 520.0,
            n_f1: 340.0,
            n_glide: 0.01,
            n_q: 1.0,
            lp_cut: 1100.0,
            ..Voice::DEFAULT
        };
        s.spawn(thud, g * 0.22, ev.pan);
        let (b0, b1) = (f * 1.06, f);
        let blip = Voice {
            dur: 0.11,
            attack: 0.004,
            decay: 0.045,
            p: [
                Partial {
                    lvl: 0.6,
                    f0: b0,
                    f1: b1,
                    glide: 0.02,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: 0.12,
                    f0: b0 * 2.0,
                    f1: b1 * 2.0,
                    glide: 0.02,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
            ],
            lp_cut: 1600.0,
            ..Voice::DEFAULT
        };
        s.spawn(blip, g * 0.3, ev.pan);
        if matches!(kind, SoundKind::Jump) {
            let v2 = Voice {
                delay: 0.05,
                dur: 0.3,
                attack: 0.02,
                decay: 0.11,
                p: [
                    Partial {
                        lvl: 0.45,
                        f0: f,
                        f1: f * 1.5,
                        glide: 0.09,
                        ..Partial::DEFAULT
                    },
                    Partial::DEFAULT,
                    Partial::DEFAULT,
                ],
                lp_cut: 1800.0,
                ..Voice::DEFAULT
            };
            s.spawn(v2, g * 0.24, ev.pan);
            let surge = Voice {
                delay: 0.1,
                dur: 0.55,
                attack: 0.09,
                decay: 0.2,
                n_lvl: 0.7,
                n_f0: 200.0,
                n_f1: 95.0,
                n_glide: 0.2,
                n_q: 0.8,
                lp_cut: 380.0,
                ..Voice::DEFAULT
            };
            s.spawn(surge, g * 0.26, ev.pan * 0.3);
        }
    }

    #[inline]
    const fn bed_sample(s: &mut TrailSynth, dt: f32, lvl: f32) -> (f32, f32) {
        let white = s.rnd() * 2.0 - 1.0;
        let u1 = s.bed.lfo_u1();
        let b = &mut s.bed;
        let cut = 680.0 - 140.0 * b.droop;
        let k = (cut.max(420.0) * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (white - b.lp1);
        let k2 = (620.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp2 += k2 * (b.lp1 - b.lp2);
        let breathe = 0.75 + 0.25 * u1;
        (b.lp2 * lvl * 0.22 * breathe, b.lp1 * lvl * 0.025)
    }
}

struct WaterPalette;

impl WaterPalette {
    #[inline]
    const fn design(s: &mut TrailSynth, ev: &SoundEvent, kind: SoundKind, g: f32, deg: i32) {
        let f = s.melody_hz(430.0, deg);
        let (a, b) = (f * 0.85, f * 1.25);
        let tap = Voice {
            dur: 0.03,
            attack: 0.0006,
            decay: 0.011,
            n_lvl: 0.6,
            n_f0: 900.0,
            n_f1: 500.0,
            n_glide: 0.008,
            n_q: 1.0,
            lp_cut: 1600.0,
            ..Voice::DEFAULT
        };
        s.spawn(tap, g * 0.16, ev.pan);
        let v = Voice {
            dur: 0.16,
            attack: 0.006,
            decay: 0.06,
            p: [
                Partial {
                    lvl: 0.75,
                    f0: a,
                    f1: b,
                    glide: 0.045,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: 0.12,
                    f0: a * 0.5,
                    f1: b * 0.5,
                    glide: 0.045,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
            ],
            lp_cut: 1800.0,
            ..Voice::DEFAULT
        };
        s.spawn(v, g * 0.42, ev.pan);
        if matches!(kind, SoundKind::Jump) {
            let vs = Voice {
                dur: 0.22,
                attack: 0.008,
                decay: 0.08,
                n_lvl: 0.5,
                n_f0: 1400.0,
                n_f1: 500.0,
                n_glide: 0.07,
                n_q: 0.8,
                lp_cut: 2200.0,
                ..Voice::DEFAULT
            };
            s.spawn(vs, g * 0.32, ev.pan);
            let mut i = 0;
            while i < 3 {
                let fd = s.melody_hz(430.0, deg + 2 + i);
                let vd = Voice {
                    delay: s.rnd_in(0.05, 0.16),
                    dur: 0.12,
                    attack: 0.004,
                    decay: 0.04,
                    p: [
                        Partial {
                            lvl: 0.7,
                            f0: fd * 0.9,
                            f1: fd * 1.3,
                            glide: 0.03,
                            ..Partial::DEFAULT
                        },
                        Partial::DEFAULT,
                        Partial::DEFAULT,
                    ],
                    lp_cut: 2200.0,
                    ..Voice::DEFAULT
                };
                let pan = ev.pan + s.rnd_in(-0.5, 0.5);
                let gd = g * s.rnd_in(0.12, 0.22);
                s.spawn(vd, gd, pan);
                i += 1;
            }
        } else if s.rnd() < 0.18 {
            let vb = Voice {
                delay: s.rnd_in(0.05, 0.09),
                dur: 0.07,
                attack: 0.004,
                decay: 0.03,
                p: [
                    Partial {
                        lvl: 0.6,
                        f0: f * 0.7,
                        f1: f * 1.6,
                        glide: 0.03,
                        ..Partial::DEFAULT
                    },
                    Partial::DEFAULT,
                    Partial::DEFAULT,
                ],
                lp_cut: 2400.0,
                ..Voice::DEFAULT
            };
            s.spawn(vb, g * 0.18, ev.pan);
        }
    }

    #[inline]
    const fn bed_grain(s: &mut TrailSynth, level: f32, gain: f32) {
        let grain_deg = (s.rnd() * 6.0) as i32;
        let f = s.melody_hz(430.0, grain_deg);
        let v = Voice {
            dur: 0.1,
            attack: 0.003,
            decay: 0.04,
            p: [
                Partial {
                    lvl: 0.6,
                    f0: f * 2.0,
                    f1: f,
                    glide: 0.025,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
                Partial::DEFAULT,
            ],
            lp_cut: 2200.0,
            ..Voice::DEFAULT
        };
        let pan = s.rnd_in(-0.7, 0.7);
        s.spawn(v, gain * level * 0.09, pan);
        s.bed.timer = s.rnd_in(0.25, 0.9) / level.max(0.05);
    }

    #[inline]
    const fn bed_sample(s: &mut TrailSynth, dt: f32, lvl: f32) -> (f32, f32) {
        let white = s.rnd() * 2.0 - 1.0;
        let u1 = s.bed.lfo_u1();
        let u2 = s.bed.lfo_u2();
        let b = &mut s.bed;
        let k = (600.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (white - b.lp1);
        let k2 = (180.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp2 += k2 * (b.lp1 - b.lp2);
        let und = 0.55 + 0.3 * u1 + 0.15 * u2;
        (b.lp2 * lvl * 0.5 * und, b.lp1 * lvl * 0.06)
    }
}

struct CometPalette;

impl CometPalette {
    #[inline]
    const fn design(s: &mut TrailSynth, ev: &SoundEvent, kind: SoundKind, g: f32, deg: i32) {
        let f = s.melody_hz(220.0, deg);
        let (f0, f1v) = (f * 1.04, f * 0.97);
        let v = Voice {
            dur: 0.35,
            attack: 0.012,
            decay: 0.14,
            p: [
                Partial {
                    lvl: 0.55,
                    f0,
                    f1: f1v,
                    glide: 0.16,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: 0.32,
                    f0: f0 * 1.006,
                    f1: f1v * 1.006,
                    glide: 0.16,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: 0.07,
                    f0: f0 * 3.0,
                    f1: f1v * 3.0,
                    glide: 0.16,
                    ..Partial::DEFAULT
                },
            ],
            n_lvl: 0.025,
            n_f0: 6000.0,
            n_f1: 6000.0,
            n_glide: 0.0,
            n_q: 0.7,
            lp_cut: 1600.0,
            ..Voice::DEFAULT
        };
        s.spawn(v, g * 0.34, ev.pan);
        if matches!(kind, SoundKind::Typed) && s.rnd() < 0.14 {
            let hi = s.rnd_in(1400.0, 2200.0);
            let delay = s.rnd_in(0.03, 0.1);
            let star = Voice {
                delay,
                dur: 0.22,
                attack: 0.004,
                decay: 0.09,
                p: [
                    Partial {
                        lvl: 0.35,
                        f0: hi,
                        f1: hi * 0.35,
                        glide: 0.07,
                        ..Partial::DEFAULT
                    },
                    Partial::DEFAULT,
                    Partial::DEFAULT,
                ],
                tw_rate: 9.0,
                tw_depth: 0.4,
                lp_cut: 3200.0,
                ..Voice::DEFAULT
            };
            s.spawn(star, g * 0.16, -ev.pan * 0.6);
        }
        if matches!(kind, SoundKind::Jump) {
            let swoosh = Voice {
                dur: 0.45,
                attack: 0.03,
                decay: 0.16,
                p: [
                    Partial {
                        lvl: 0.3,
                        f0: f * 2.0,
                        f1: f * 0.9,
                        glide: 0.12,
                        ..Partial::DEFAULT
                    },
                    Partial::DEFAULT,
                    Partial::DEFAULT,
                ],
                n_lvl: 0.5,
                n_f0: 1200.0,
                n_f1: 260.0,
                n_glide: 0.12,
                n_q: 1.4,
                lp_cut: 2400.0,
                ..Voice::DEFAULT
            };
            s.spawn(swoosh, g * 0.4, ev.pan);
            let mut i = 0;
            while i < 3 {
                let glint_deg = (s.rnd() * 5.0) as i32;
                let fg = s.melody_hz(1046.5, glint_deg);
                let delay = 0.08 + i as f32 * 0.07 + s.rnd_in(0.0, 0.03);
                let decay = s.rnd_in(0.03, 0.05);
                let glint = Voice {
                    delay,
                    dur: 0.12,
                    attack: 0.001,
                    decay,
                    p: [
                        Partial {
                            lvl: 0.3,
                            f0: fg,
                            f1: fg,
                            fm_ratio: 3.01,
                            fm_i0: 1.2,
                            fm_tau: 0.03,
                            ..Partial::DEFAULT
                        },
                        Partial::DEFAULT,
                        Partial::DEFAULT,
                    ],
                    tw_rate: 8.0,
                    tw_depth: 0.35,
                    lp_cut: 5200.0,
                    ..Voice::DEFAULT
                };
                let pan = ev.pan + s.rnd_in(-0.5, 0.5);
                s.spawn(glint, g * 0.12, pan);
                i += 1;
            }
        }
    }

    #[inline]
    const fn bed_grain(s: &mut TrailSynth, level: f32, gain: f32) {
        let grain_deg = (s.rnd() * 5.0) as i32;
        let f = s.melody_hz(660.0, grain_deg);
        let v = Voice {
            dur: 0.5,
            attack: 0.09,
            decay: 0.16,
            p: [
                Partial {
                    lvl: 0.5,
                    f0: f * 1.01,
                    f1: f * 0.99,
                    glide: 0.3,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
                Partial::DEFAULT,
            ],
            lp_cut: 2600.0,
            ..Voice::DEFAULT
        };
        let pan = s.rnd_in(-0.8, 0.8);
        s.spawn(v, gain * level * 0.06, pan);
        s.bed.timer = s.rnd_in(0.6, 1.6) / level.max(0.05);
    }

    #[inline]
    const fn bed_sample(s: &mut TrailSynth, dt: f32, lvl: f32) -> (f32, f32) {
        let u1 = s.bed.lfo_u1();
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 330.0 * dt).fract();
        b.ph2 = (b.ph2 + 330.7 * dt).fract();
        b.ph3 = (b.ph3 + 440.0 * dt).fract();
        let sm = 0.5 * sin01(b.ph1) + 0.5 * sin01(b.ph2) + (0.08 + 0.12 * u1) * sin01(b.ph3);
        (sm * lvl * 0.026, sm * lvl * 0.008)
    }
}

struct MechPalette;

impl MechPalette {
    #[inline]
    const fn key(s: &mut TrailSynth, pan: f32, g: f32, body: f32, bright: f32, click_only: bool) {
        let click = Voice {
            dur: 0.035,
            attack: 0.001,
            decay: 0.012,
            n_lvl: 0.55,
            n_f0: 1900.0 * bright,
            n_f1: 800.0 * bright,
            n_glide: 0.012,
            n_q: 0.8,
            lp_cut: 3600.0,
            ..Voice::DEFAULT
        };
        s.spawn(click, g * if click_only { 0.4 } else { 0.34 }, pan);
        if click_only {
            return;
        }
        let thock = Voice {
            dur: 0.11,
            attack: 0.002,
            decay: 0.045,
            p: [
                Partial {
                    lvl: 0.9,
                    f0: body,
                    f1: body * 0.55,
                    glide: 0.03,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
                Partial::DEFAULT,
            ],
            n_lvl: 0.12,
            n_f0: 620.0,
            n_f1: 170.0,
            n_glide: 0.045,
            n_q: 0.9,
            lp_cut: 1000.0,
            ..Voice::DEFAULT
        };
        s.spawn(thock, g * 0.5, pan * 0.7);
    }

    #[inline]
    const fn design(s: &mut TrailSynth, ev: &SoundEvent, kind: SoundKind, g: f32) {
        match kind {
            SoundKind::Typed => {
                let body = s.rnd_in(150.0, 174.0);
                let bright = s.rnd_in(0.9, 1.1);
                Self::key(s, ev.pan, g, body, bright, false);
            }
            SoundKind::Navigation => {
                let bright = s.rnd_in(0.85, 1.0);
                Self::key(s, ev.pan, g, 0.0, bright, true);
            }
            SoundKind::Jump => {
                let clunk = Voice {
                    dur: 0.16,
                    attack: 0.002,
                    decay: 0.07,
                    p: [
                        Partial {
                            lvl: 0.95,
                            f0: s.rnd_in(104.0, 118.0),
                            f1: 62.0,
                            glide: 0.045,
                            ..Partial::DEFAULT
                        },
                        Partial::DEFAULT,
                        Partial::DEFAULT,
                    ],
                    n_lvl: 0.22,
                    n_f0: 520.0,
                    n_f1: 140.0,
                    n_glide: 0.05,
                    n_q: 0.9,
                    lp_cut: 850.0,
                    ..Voice::DEFAULT
                };
                s.spawn(clunk, g * 0.6, ev.pan * 0.6);
                let mut i = 0;
                while i < 2 {
                    let tick = Voice {
                        delay: 0.05 + i as f32 * 0.055 + s.rnd_in(0.0, 0.015),
                        dur: 0.035,
                        attack: 0.001,
                        decay: 0.012,
                        n_lvl: 0.5,
                        n_f0: s.rnd_in(1500.0, 2000.0),
                        n_f1: 700.0,
                        n_glide: 0.012,
                        n_q: 0.8,
                        lp_cut: 3200.0,
                        ..Voice::DEFAULT
                    };
                    s.spawn(tick, g * 0.22, -ev.pan * 0.4);
                    i += 1;
                }
            }
            _ => {}
        }
    }
}

struct TypewriterPalette;

impl TypewriterPalette {
    #[inline]
    const fn strike(s: &mut TrailSynth, pan: f32, g: f32, bright: f32, ring: f32, body: f32) {
        let clack = Voice {
            dur: 0.04,
            attack: 0.0008,
            decay: 0.013,
            n_lvl: 0.6,
            n_f0: 3200.0 * bright,
            n_f1: 1400.0 * bright,
            n_glide: 0.008,
            n_q: 0.6,
            lp_cut: 5200.0,
            ..Voice::DEFAULT
        };
        s.spawn(clack, g * 1.6, pan);
        if ring > 0.0 {
            let ring_v = Voice {
                dur: 0.09,
                attack: 0.001,
                decay: 0.022,
                p: [
                    Partial {
                        lvl: 0.5,
                        f0: 3150.0 * ring,
                        f1: 3150.0 * ring,
                        ..Partial::DEFAULT
                    },
                    Partial {
                        lvl: 0.3,
                        f0: 4720.0 * ring,
                        f1: 4720.0 * ring,
                        ..Partial::DEFAULT
                    },
                    Partial::DEFAULT,
                ],
                lp_cut: 6000.0,
                ..Voice::DEFAULT
            };
            s.spawn(ring_v, g * 0.35, pan);
        }
        if body > 0.0 {
            let thud = Voice {
                dur: 0.08,
                attack: 0.0015,
                decay: 0.03,
                p: [
                    Partial {
                        lvl: 0.9,
                        f0: body,
                        f1: body * 0.55,
                        glide: 0.025,
                        ..Partial::DEFAULT
                    },
                    Partial::DEFAULT,
                    Partial::DEFAULT,
                ],
                n_lvl: 0.15,
                n_f0: 900.0,
                n_f1: 260.0,
                n_glide: 0.03,
                n_q: 0.9,
                lp_cut: 1400.0,
                ..Voice::DEFAULT
            };
            s.spawn(thud, g * 0.22, pan * 0.7);
        }
    }

    #[inline]
    const fn design(s: &mut TrailSynth, ev: &SoundEvent, kind: SoundKind, g: f32) {
        match kind {
            SoundKind::Typed => {
                let bright = s.rnd_in(0.9, 1.1);
                let ring = s.rnd_in(0.97, 1.03);
                let body = s.rnd_in(200.0, 232.0);
                Self::strike(s, ev.pan, g, bright, ring, body);
            }
            SoundKind::Navigation => {
                Self::strike(s, ev.pan, g * 2.0, 0.75, 0.0, 0.0);
            }
            SoundKind::Jump => {
                let ding = Voice {
                    dur: 0.45,
                    attack: 0.001,
                    decay: 0.14,
                    p: [
                        Partial {
                            lvl: 0.5,
                            f0: 2093.0,
                            f1: 2093.0,
                            fm_ratio: 2.71,
                            fm_i0: 0.9,
                            fm_tau: 0.05,
                            ..Partial::DEFAULT
                        },
                        Partial {
                            lvl: 0.12,
                            f0: 4.0 * 2093.0,
                            f1: 4.0 * 2093.0,
                            ..Partial::DEFAULT
                        },
                        Partial::DEFAULT,
                    ],
                    n_lvl: 0.5,
                    n_f0: 6000.0,
                    n_f1: 900.0,
                    n_glide: 0.004,
                    n_q: 0.7,
                    lp_cut: 7000.0,
                    ..Voice::DEFAULT
                };
                s.spawn(ding, g * 0.56, (ev.pan * 0.3 + 0.35).clamp(-1.0, 1.0));
                let zip = Voice {
                    delay: 0.07,
                    dur: 0.16,
                    attack: 0.01,
                    decay: 0.06,
                    n_lvl: 0.55,
                    n_f0: 2400.0,
                    n_f1: 700.0,
                    n_glide: 0.09,
                    n_q: 1.1,
                    lp_cut: 3600.0,
                    ..Voice::DEFAULT
                };
                s.spawn(zip, g * 0.48, -0.35);
                let clunk = Voice {
                    delay: 0.20,
                    dur: 0.09,
                    attack: 0.0015,
                    decay: 0.035,
                    p: [
                        Partial {
                            lvl: 0.9,
                            f0: 130.0,
                            f1: 70.0,
                            glide: 0.03,
                            ..Partial::DEFAULT
                        },
                        Partial::DEFAULT,
                        Partial::DEFAULT,
                    ],
                    n_lvl: 0.2,
                    n_f0: 700.0,
                    n_f1: 200.0,
                    n_glide: 0.03,
                    n_q: 0.9,
                    lp_cut: 900.0,
                    ..Voice::DEFAULT
                };
                s.spawn(clunk, g * 0.61, -0.5);
            }
            _ => {}
        }
    }
}

struct MarimbaPalette;

impl MarimbaPalette {
    const BASE_HZ: f32 = 261.63;

    #[inline]
    const fn bar(
        s: &mut TrailSynth,
        deg: i32,
        delay: f32,
        decay: f32,
        glint: bool,
        g: f32,
        pan: f32,
    ) {
        let f = s.melody_hz(Self::BASE_HZ, deg);
        let (b0, b1) = gesture_bend(f, 1);
        let f0 = b1 + (b0 - b1) * KEY_BELL_BEND_SHARE;
        let bar = Voice {
            delay,
            dur: 0.42,
            attack: 0.002,
            decay,
            p: [
                Partial {
                    lvl: 0.6,
                    f0,
                    f1: f,
                    glide: KEY_BELL_BEND_TAU,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
                Partial::DEFAULT,
            ],
            n_lvl: 0.35,
            n_f0: 700.0,
            n_f1: 250.0,
            n_glide: 0.008,
            n_q: 0.5,
            lp_cut: 3000.0,
            ..Voice::DEFAULT
        };
        s.spawn(bar, g * 0.42, pan);
        if glint {
            let glint_v = Voice {
                delay,
                dur: 0.12,
                attack: 0.001,
                decay: 0.03,
                p: [
                    Partial {
                        lvl: 0.5,
                        f0: f * 4.0,
                        f1: f * 4.0,
                        ..Partial::DEFAULT
                    },
                    Partial::DEFAULT,
                    Partial::DEFAULT,
                ],
                lp_cut: 6000.0,
                ..Voice::DEFAULT
            };
            s.spawn(glint_v, g * 0.12, pan);
        }
    }

    #[inline]
    const fn design(s: &mut TrailSynth, ev: &SoundEvent, kind: SoundKind, g: f32, deg: i32) {
        match kind {
            SoundKind::Typed => {
                Self::bar(s, deg, 0.0, 0.13, true, g, ev.pan);
            }
            SoundKind::Navigation => {
                Self::bar(s, deg, 0.0, 0.05, false, g * 1.25, ev.pan);
            }
            SoundKind::Jump => {
                Self::bar(s, deg, 0.0, 0.13, true, g, ev.pan);
                Self::bar(s, deg + 3, 0.07, 0.13, false, g * 0.4, -ev.pan);
                Self::bar(s, deg + 5, 0.07, 0.13, true, g * 0.45, ev.pan * 0.5);
            }
            _ => {}
        }
    }

    #[inline]
    const fn bed_sample(s: &mut TrailSynth, dt: f32, lvl: f32) -> (f32, f32) {
        let u1 = s.bed.lfo_u1();
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 261.6 * dt).fract();
        b.ph2 = (b.ph2 + 392.4 * dt).fract();
        let sm = (0.6 + 0.4 * u1) * (sin01(b.ph1) + 0.7 * sin01(b.ph2));
        let k = (750.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (sm - b.lp1);
        (b.lp1 * lvl * 0.011, 0.0)
    }
}

struct FeltPalette;

impl FeltPalette {
    const BASE_HZ: f32 = 196.0;

    #[inline]
    const fn note(
        s: &mut TrailSynth,
        deg: i32,
        delay: f32,
        decay: f32,
        thud: f32,
        g: f32,
        pan: f32,
    ) {
        let f = s.melody_hz(Self::BASE_HZ, deg);
        let (b0, b1) = gesture_bend(f, 1);
        let f0 = b1 + (b0 - b1) * KEY_BELL_BEND_SHARE;
        let v = Voice {
            delay,
            dur: 0.6,
            attack: 0.004,
            decay,
            p: [
                Partial {
                    lvl: 0.5,
                    f0,
                    f1: f,
                    glide: KEY_BELL_BEND_TAU,
                    fm_ratio: 1.0,
                    fm_i0: 0.35,
                    fm_tau: 0.02,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: 0.22,
                    f0: f * 2.0,
                    f1: f * 2.0,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: 0.10,
                    f0: f * 3.0,
                    f1: f * 3.0,
                    ..Partial::DEFAULT
                },
            ],
            n_lvl: 0.3 * thud,
            n_f0: 350.0,
            n_f1: 120.0,
            n_glide: 0.012,
            n_q: 0.5,
            lp_cut: 1300.0,
            ..Voice::DEFAULT
        };
        s.spawn(v, g * 0.5, pan);
    }

    #[inline]
    const fn design(s: &mut TrailSynth, ev: &SoundEvent, kind: SoundKind, g: f32, deg: i32) {
        match kind {
            SoundKind::Typed => {
                Self::note(s, deg, 0.0, 0.19, 1.0, g, ev.pan);
            }
            SoundKind::Navigation => {
                Self::note(s, deg, 0.0, 0.09, 0.0, g * 0.8, ev.pan);
            }
            SoundKind::Jump => {
                Self::note(s, deg, 0.0, 0.19, 1.0, g, ev.pan);
                Self::note(s, deg - 5, 0.0, 0.22, 1.0, g * 0.7, ev.pan * 0.5);
                Self::note(s, deg + 3, 0.06, 0.19, 0.5, g * 0.5, -ev.pan * 0.5);
            }
            _ => {}
        }
    }

    #[inline]
    const fn bed_sample(s: &mut TrailSynth, dt: f32, lvl: f32) -> (f32, f32) {
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 392.0 * dt).fract();
        b.ph2 = (b.ph2 + 392.6 * dt).fract();
        let sm = sin01(b.ph1) + sin01(b.ph2);
        let k = (650.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (sm - b.lp1);
        (b.lp1 * lvl * 0.010, 0.0)
    }
}

const SIN_TAB_BITS: u32 = 10;
const SIN_TAB_LEN: usize = 1 << SIN_TAB_BITS;

#[rustfmt::skip]
#[allow(clippy::approx_constant, clippy::excessive_precision)]
static SIN_TAB: [f32; SIN_TAB_LEN + 1] = [
    0.0, 0.0061358847, 0.012271538, 0.01840673, 0.024541229, 0.030674804,
    0.036807224, 0.04293826, 0.049067676, 0.055195246, 0.061320737, 0.06744392,
    0.07356457, 0.07968244, 0.08579731, 0.091908954, 0.09801714, 0.10412163,
    0.110222206, 0.11631863, 0.12241068, 0.1284981, 0.1345807, 0.14065824,
    0.14673047, 0.15279719, 0.15885815, 0.16491312, 0.17096189, 0.17700422,
    0.18303989, 0.18906866, 0.19509032, 0.20110464, 0.20711137, 0.21311031,
    0.21910124, 0.22508392, 0.2310581, 0.2370236, 0.24298018, 0.24892761,
    0.25486565, 0.2607941, 0.26671275, 0.27262136, 0.2785197, 0.28440753,
    0.29028466, 0.2961509, 0.30200595, 0.30784965, 0.31368175, 0.31950203,
    0.3253103, 0.3311063, 0.33688986, 0.34266073, 0.34841868, 0.35416353,
    0.35989505, 0.36561298, 0.3713172, 0.37700742, 0.38268343, 0.38834503,
    0.39399204, 0.3996242, 0.4052413, 0.41084316, 0.41642955, 0.42200026,
    0.42755508, 0.43309382, 0.43861625, 0.44412214, 0.44961134, 0.45508358,
    0.46053872, 0.4659765, 0.47139674, 0.47679922, 0.48218378, 0.48755017,
    0.4928982, 0.49822766, 0.50353837, 0.50883013, 0.51410276, 0.519356,
    0.52458966, 0.52980363, 0.53499764, 0.54017144, 0.545325, 0.55045795,
    0.55557024, 0.56066155, 0.5657318, 0.57078075, 0.57580817, 0.58081394,
    0.58579785, 0.5907597, 0.5956993, 0.60061646, 0.60551107, 0.6103828,
    0.6152316, 0.6200572, 0.6248595, 0.62963825, 0.6343933, 0.63912445,
    0.64383155, 0.6485144, 0.65317285, 0.6578067, 0.6624158, 0.66699994,
    0.671559, 0.6760927, 0.680601, 0.6850837, 0.68954057, 0.69397146,
    0.69837624, 0.70275474, 0.70710677, 0.7114322, 0.71573085, 0.72000253,
    0.7242471, 0.72846437, 0.7326543, 0.7368166, 0.7409511, 0.74505776,
    0.7491364, 0.7531868, 0.7572088, 0.7612024, 0.76516724, 0.76910335,
    0.77301043, 0.7768885, 0.7807372, 0.78455657, 0.7883464, 0.79210657,
    0.7958369, 0.79953724, 0.8032075, 0.8068476, 0.81045717, 0.8140363,
    0.8175848, 0.8211025, 0.8245893, 0.82804507, 0.8314696, 0.8348629,
    0.8382247, 0.841555, 0.8448536, 0.84812033, 0.8513552, 0.854558,
    0.8577286, 0.86086696, 0.86397284, 0.86704624, 0.87008697, 0.873095,
    0.8760701, 0.8790122, 0.8819213, 0.8847971, 0.88763964, 0.89044875,
    0.8932243, 0.89596623, 0.8986745, 0.9013488, 0.9039893, 0.9065957,
    0.909168, 0.91170603, 0.9142098, 0.9166791, 0.9191139, 0.92151403,
    0.9238795, 0.9262102, 0.9285061, 0.93076694, 0.9329928, 0.9351835,
    0.937339, 0.9394592, 0.94154406, 0.94359344, 0.9456073, 0.9475856,
    0.94952816, 0.951435, 0.953306, 0.9551412, 0.95694035, 0.95870346,
    0.9604305, 0.9621214, 0.96377605, 0.96539444, 0.96697646, 0.9685221,
    0.97003126, 0.9715039, 0.97293997, 0.97433937, 0.9757021, 0.97702813,
    0.9783174, 0.9795698, 0.98078525, 0.9819639, 0.9831055, 0.9842101,
    0.98527765, 0.9863081, 0.9873014, 0.9882576, 0.9891765, 0.9900582,
    0.99090266, 0.99170977, 0.99247956, 0.9932119, 0.993907, 0.9945646,
    0.9951847, 0.9957674, 0.9963126, 0.9968203, 0.99729043, 0.99772304,
    0.9981181, 0.99847555, 0.99879545, 0.99907774, 0.99932235, 0.9995294,
    0.9996988, 0.9998306, 0.9999247, 0.99998116, 1.0, 0.99998116,
    0.9999247, 0.9998306, 0.9996988, 0.9995294, 0.99932235, 0.99907774,
    0.99879545, 0.99847555, 0.9981181, 0.99772304, 0.99729043, 0.9968203,
    0.9963126, 0.9957674, 0.9951847, 0.9945646, 0.993907, 0.9932119,
    0.99247956, 0.99170977, 0.99090266, 0.9900582, 0.9891765, 0.9882576,
    0.9873014, 0.9863081, 0.98527765, 0.9842101, 0.9831055, 0.9819639,
    0.98078525, 0.9795698, 0.9783174, 0.97702813, 0.9757021, 0.97433937,
    0.97293997, 0.9715039, 0.97003126, 0.9685221, 0.96697646, 0.96539444,
    0.96377605, 0.9621214, 0.9604305, 0.95870346, 0.95694035, 0.9551412,
    0.953306, 0.951435, 0.94952816, 0.9475856, 0.9456073, 0.94359344,
    0.94154406, 0.9394592, 0.937339, 0.9351835, 0.9329928, 0.93076694,
    0.9285061, 0.9262102, 0.9238795, 0.92151403, 0.9191139, 0.9166791,
    0.9142098, 0.91170603, 0.909168, 0.9065957, 0.9039893, 0.9013488,
    0.8986745, 0.89596623, 0.8932243, 0.89044875, 0.88763964, 0.8847971,
    0.8819213, 0.8790122, 0.8760701, 0.873095, 0.87008697, 0.86704624,
    0.86397284, 0.86086696, 0.8577286, 0.854558, 0.8513552, 0.84812033,
    0.8448536, 0.841555, 0.8382247, 0.8348629, 0.8314696, 0.82804507,
    0.8245893, 0.8211025, 0.8175848, 0.8140363, 0.81045717, 0.8068476,
    0.8032075, 0.79953724, 0.7958369, 0.79210657, 0.7883464, 0.78455657,
    0.7807372, 0.7768885, 0.77301043, 0.76910335, 0.76516724, 0.7612024,
    0.7572088, 0.7531868, 0.7491364, 0.74505776, 0.7409511, 0.7368166,
    0.7326543, 0.72846437, 0.7242471, 0.72000253, 0.71573085, 0.7114322,
    0.70710677, 0.70275474, 0.69837624, 0.69397146, 0.68954057, 0.6850837,
    0.680601, 0.6760927, 0.671559, 0.66699994, 0.6624158, 0.6578067,
    0.65317285, 0.6485144, 0.64383155, 0.63912445, 0.6343933, 0.62963825,
    0.6248595, 0.6200572, 0.6152316, 0.6103828, 0.60551107, 0.60061646,
    0.5956993, 0.5907597, 0.58579785, 0.58081394, 0.57580817, 0.57078075,
    0.5657318, 0.56066155, 0.55557024, 0.55045795, 0.545325, 0.54017144,
    0.53499764, 0.52980363, 0.52458966, 0.519356, 0.51410276, 0.50883013,
    0.50353837, 0.49822766, 0.4928982, 0.48755017, 0.48218378, 0.47679922,
    0.47139674, 0.4659765, 0.46053872, 0.45508358, 0.44961134, 0.44412214,
    0.43861625, 0.43309382, 0.42755508, 0.42200026, 0.41642955, 0.41084316,
    0.4052413, 0.3996242, 0.39399204, 0.38834503, 0.38268343, 0.37700742,
    0.3713172, 0.36561298, 0.35989505, 0.35416353, 0.34841868, 0.34266073,
    0.33688986, 0.3311063, 0.3253103, 0.31950203, 0.31368175, 0.30784965,
    0.30200595, 0.2961509, 0.29028466, 0.28440753, 0.2785197, 0.27262136,
    0.26671275, 0.2607941, 0.25486565, 0.24892761, 0.24298018, 0.2370236,
    0.2310581, 0.22508392, 0.21910124, 0.21311031, 0.20711137, 0.20110464,
    0.19509032, 0.18906866, 0.18303989, 0.17700422, 0.17096189, 0.16491312,
    0.15885815, 0.15279719, 0.14673047, 0.14065824, 0.1345807, 0.1284981,
    0.12241068, 0.11631863, 0.110222206, 0.10412163, 0.09801714, 0.091908954,
    0.08579731, 0.07968244, 0.07356457, 0.06744392, 0.061320737, 0.055195246,
    0.049067676, 0.04293826, 0.036807224, 0.030674804, 0.024541229, 0.01840673,
    0.012271538, 0.0061358847, 0.0, -0.0061358847, -0.012271538, -0.01840673,
    -0.024541229, -0.030674804, -0.036807224, -0.04293826, -0.049067676, -0.055195246,
    -0.061320737, -0.06744392, -0.07356457, -0.07968244, -0.08579731, -0.091908954,
    -0.09801714, -0.10412163, -0.110222206, -0.11631863, -0.12241068, -0.1284981,
    -0.1345807, -0.14065824, -0.14673047, -0.15279719, -0.15885815, -0.16491312,
    -0.17096189, -0.17700422, -0.18303989, -0.18906866, -0.19509032, -0.20110464,
    -0.20711137, -0.21311031, -0.21910124, -0.22508392, -0.2310581, -0.2370236,
    -0.24298018, -0.24892761, -0.25486565, -0.2607941, -0.26671275, -0.27262136,
    -0.2785197, -0.28440753, -0.29028466, -0.2961509, -0.30200595, -0.30784965,
    -0.31368175, -0.31950203, -0.3253103, -0.3311063, -0.33688986, -0.34266073,
    -0.34841868, -0.35416353, -0.35989505, -0.36561298, -0.3713172, -0.37700742,
    -0.38268343, -0.38834503, -0.39399204, -0.3996242, -0.4052413, -0.41084316,
    -0.41642955, -0.42200026, -0.42755508, -0.43309382, -0.43861625, -0.44412214,
    -0.44961134, -0.45508358, -0.46053872, -0.4659765, -0.47139674, -0.47679922,
    -0.48218378, -0.48755017, -0.4928982, -0.49822766, -0.50353837, -0.50883013,
    -0.51410276, -0.519356, -0.52458966, -0.52980363, -0.53499764, -0.54017144,
    -0.545325, -0.55045795, -0.55557024, -0.56066155, -0.5657318, -0.57078075,
    -0.57580817, -0.58081394, -0.58579785, -0.5907597, -0.5956993, -0.60061646,
    -0.60551107, -0.6103828, -0.6152316, -0.6200572, -0.6248595, -0.62963825,
    -0.6343933, -0.63912445, -0.64383155, -0.6485144, -0.65317285, -0.6578067,
    -0.6624158, -0.66699994, -0.671559, -0.6760927, -0.680601, -0.6850837,
    -0.68954057, -0.69397146, -0.69837624, -0.70275474, -0.70710677, -0.7114322,
    -0.71573085, -0.72000253, -0.7242471, -0.72846437, -0.7326543, -0.7368166,
    -0.7409511, -0.74505776, -0.7491364, -0.7531868, -0.7572088, -0.7612024,
    -0.76516724, -0.76910335, -0.77301043, -0.7768885, -0.7807372, -0.78455657,
    -0.7883464, -0.79210657, -0.7958369, -0.79953724, -0.8032075, -0.8068476,
    -0.81045717, -0.8140363, -0.8175848, -0.8211025, -0.8245893, -0.82804507,
    -0.8314696, -0.8348629, -0.8382247, -0.841555, -0.8448536, -0.84812033,
    -0.8513552, -0.854558, -0.8577286, -0.86086696, -0.86397284, -0.86704624,
    -0.87008697, -0.873095, -0.8760701, -0.8790122, -0.8819213, -0.8847971,
    -0.88763964, -0.89044875, -0.8932243, -0.89596623, -0.8986745, -0.9013488,
    -0.9039893, -0.9065957, -0.909168, -0.91170603, -0.9142098, -0.9166791,
    -0.9191139, -0.92151403, -0.9238795, -0.9262102, -0.9285061, -0.93076694,
    -0.9329928, -0.9351835, -0.937339, -0.9394592, -0.94154406, -0.94359344,
    -0.9456073, -0.9475856, -0.94952816, -0.951435, -0.953306, -0.9551412,
    -0.95694035, -0.95870346, -0.9604305, -0.9621214, -0.96377605, -0.96539444,
    -0.96697646, -0.9685221, -0.97003126, -0.9715039, -0.97293997, -0.97433937,
    -0.9757021, -0.97702813, -0.9783174, -0.9795698, -0.98078525, -0.9819639,
    -0.9831055, -0.9842101, -0.98527765, -0.9863081, -0.9873014, -0.9882576,
    -0.9891765, -0.9900582, -0.99090266, -0.99170977, -0.99247956, -0.9932119,
    -0.993907, -0.9945646, -0.9951847, -0.9957674, -0.9963126, -0.9968203,
    -0.99729043, -0.99772304, -0.9981181, -0.99847555, -0.99879545, -0.99907774,
    -0.99932235, -0.9995294, -0.9996988, -0.9998306, -0.9999247, -0.99998116,
    -1.0, -0.99998116, -0.9999247, -0.9998306, -0.9996988, -0.9995294,
    -0.99932235, -0.99907774, -0.99879545, -0.99847555, -0.9981181, -0.99772304,
    -0.99729043, -0.9968203, -0.9963126, -0.9957674, -0.9951847, -0.9945646,
    -0.993907, -0.9932119, -0.99247956, -0.99170977, -0.99090266, -0.9900582,
    -0.9891765, -0.9882576, -0.9873014, -0.9863081, -0.98527765, -0.9842101,
    -0.9831055, -0.9819639, -0.98078525, -0.9795698, -0.9783174, -0.97702813,
    -0.9757021, -0.97433937, -0.97293997, -0.9715039, -0.97003126, -0.9685221,
    -0.96697646, -0.96539444, -0.96377605, -0.9621214, -0.9604305, -0.95870346,
    -0.95694035, -0.9551412, -0.953306, -0.951435, -0.94952816, -0.9475856,
    -0.9456073, -0.94359344, -0.94154406, -0.9394592, -0.937339, -0.9351835,
    -0.9329928, -0.93076694, -0.9285061, -0.9262102, -0.9238795, -0.92151403,
    -0.9191139, -0.9166791, -0.9142098, -0.91170603, -0.909168, -0.9065957,
    -0.9039893, -0.9013488, -0.8986745, -0.89596623, -0.8932243, -0.89044875,
    -0.88763964, -0.8847971, -0.8819213, -0.8790122, -0.8760701, -0.873095,
    -0.87008697, -0.86704624, -0.86397284, -0.86086696, -0.8577286, -0.854558,
    -0.8513552, -0.84812033, -0.8448536, -0.841555, -0.8382247, -0.8348629,
    -0.8314696, -0.82804507, -0.8245893, -0.8211025, -0.8175848, -0.8140363,
    -0.81045717, -0.8068476, -0.8032075, -0.79953724, -0.7958369, -0.79210657,
    -0.7883464, -0.78455657, -0.7807372, -0.7768885, -0.77301043, -0.76910335,
    -0.76516724, -0.7612024, -0.7572088, -0.7531868, -0.7491364, -0.74505776,
    -0.7409511, -0.7368166, -0.7326543, -0.72846437, -0.7242471, -0.72000253,
    -0.71573085, -0.7114322, -0.70710677, -0.70275474, -0.69837624, -0.69397146,
    -0.68954057, -0.6850837, -0.680601, -0.6760927, -0.671559, -0.66699994,
    -0.6624158, -0.6578067, -0.65317285, -0.6485144, -0.64383155, -0.63912445,
    -0.6343933, -0.62963825, -0.6248595, -0.6200572, -0.6152316, -0.6103828,
    -0.60551107, -0.60061646, -0.5956993, -0.5907597, -0.58579785, -0.58081394,
    -0.57580817, -0.57078075, -0.5657318, -0.56066155, -0.55557024, -0.55045795,
    -0.545325, -0.54017144, -0.53499764, -0.52980363, -0.52458966, -0.519356,
    -0.51410276, -0.50883013, -0.50353837, -0.49822766, -0.4928982, -0.48755017,
    -0.48218378, -0.47679922, -0.47139674, -0.4659765, -0.46053872, -0.45508358,
    -0.44961134, -0.44412214, -0.43861625, -0.43309382, -0.42755508, -0.42200026,
    -0.41642955, -0.41084316, -0.4052413, -0.3996242, -0.39399204, -0.38834503,
    -0.38268343, -0.37700742, -0.3713172, -0.36561298, -0.35989505, -0.35416353,
    -0.34841868, -0.34266073, -0.33688986, -0.3311063, -0.3253103, -0.31950203,
    -0.31368175, -0.30784965, -0.30200595, -0.2961509, -0.29028466, -0.28440753,
    -0.2785197, -0.27262136, -0.26671275, -0.2607941, -0.25486565, -0.24892761,
    -0.24298018, -0.2370236, -0.2310581, -0.22508392, -0.21910124, -0.21311031,
    -0.20711137, -0.20110464, -0.19509032, -0.18906866, -0.18303989, -0.17700422,
    -0.17096189, -0.16491312, -0.15885815, -0.15279719, -0.14673047, -0.14065824,
    -0.1345807, -0.1284981, -0.12241068, -0.11631863, -0.110222206, -0.10412163,
    -0.09801714, -0.091908954, -0.08579731, -0.07968244, -0.07356457, -0.06744392,
    -0.061320737, -0.055195246, -0.049067676, -0.04293826, -0.036807224, -0.030674804,
    -0.024541229, -0.01840673, -0.012271538, -0.0061358847, 0.0,
];

#[inline]
const fn sin01(ph: f32) -> f32 {
    let x = ph - ph.floor();
    let z = x * SIN_TAB_LEN as f32;
    let j = z as usize;
    let f = z - j as f32;
    let i = j & (SIN_TAB_LEN - 1);
    let a = SIN_TAB[i];
    let b = SIN_TAB[i + 1];
    a + f * (b - a)
}

#[inline]
const fn xorshift01(rng: &mut u32) -> f32 {
    let mut x = *rng;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *rng = x;
    (x >> 8) as f32 * (1.0 / 16_777_216.0)
}

#[inline]
const fn tri(ph: f32) -> f32 {
    4.0 * (ph - 0.5).abs() - 1.0
}

#[inline]
const fn limit(y: f32, env: &mut f32, g: &mut f32, k_att: f32, k_rel: f32) -> f32 {
    *env = y.abs().max(*env * k_rel);
    let target = if *env > LIMIT_THRESHOLD {
        LIMIT_THRESHOLD / *env
    } else {
        1.0
    };
    *g = if target < *g {
        *g + (target - *g) * k_att
    } else {
        target
    };
    y * *g
}

#[inline]
const fn soft_clip(x: f32) -> f32 {
    let y = x * (27.0 + x * x) / (27.0 + 9.0 * x * x);
    y.clamp(-0.98, 0.98)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_glyph_rank_is_ordered_total_and_lossy() {
        assert_eq!(typed_glyph_rank(None), 0, "no key behind the cue");
        for (k, c) in ('a'..='z').enumerate() {
            assert_eq!(typed_glyph_rank(Some(c)), 1 + k as u8, "{c:?}");
            assert_eq!(
                typed_glyph_rank(Some(c.to_ascii_uppercase())),
                typed_glyph_rank(Some(c)),
                "{c:?}: case must fold"
            );
        }
        for (k, c) in ('0'..='9').enumerate() {
            assert_eq!(typed_glyph_rank(Some(c)), 27 + k as u8, "{c:?}");
        }
        let buckets: std::collections::BTreeSet<u8> = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~"
            .chars()
            .map(|c| typed_glyph_rank(Some(c)))
            .collect();
        assert_eq!(
            buckets,
            (37..=44).collect::<std::collections::BTreeSet<u8>>(),
            "ASCII punctuation must populate all eight ranks 37..=44"
        );
        for c in ['漢', '£', 'é', '٣', ' ', '\t'] {
            assert_eq!(typed_glyph_rank(Some(c)), 45, "{c:?}");
        }
        for u in 0u32..0x3000 {
            if let Some(c) = char::from_u32(u) {
                assert!(typed_glyph_rank(Some(c)) <= 45, "{c:?} ranked out of range");
            }
        }
    }

    #[test]
    fn the_glyph_class_table_is_one_for_host_bench_and_engine() {
        use glyph_class::*;
        assert_eq!(typed_glyph_class(None), LETTER, "no key behind the cue");
        for c in '0'..='9' {
            assert_eq!(typed_glyph_class(Some(c)), DIGIT, "{c:?}");
        }
        for c in (0x21u8..0x7f)
            .map(char::from)
            .filter(char::is_ascii_punctuation)
        {
            let class = typed_glyph_class(Some(c));
            assert!(
                (QMARK..=DASH).contains(&class) && class != DIGIT,
                "{c:?} fell out of the mark buckets ({class})"
            );
        }
        for c in ('a'..='z')
            .chain('A'..='Z')
            .chain([' ', 'é', '漢', '٣', '£', '\t'])
        {
            assert_eq!(typed_glyph_class(Some(c)), LETTER, "{c:?}");
        }
        for u in 0u32..0x3000 {
            if let Some(c) = char::from_u32(u) {
                assert!(
                    typed_glyph_class(Some(c)) <= DASH,
                    "{c:?} classed out of range"
                );
            }
        }
    }

    #[test]
    fn an_explicit_instrument_auditions_itself_under_the_music_box() {
        use GlowStyle::{Lumen, RainbowKitty, Water};
        let synth = TrailSynth::new(48_000.0, 7);
        for (look, voice, v2) in [
            (RainbowKitty, SoundVoice::Marimba, false),
            (RainbowKitty, SoundVoice::Felt, false),
            (RainbowKitty, SoundVoice::Of(Water), false),
            (RainbowKitty, SoundVoice::Style, true),
            (RainbowKitty, SoundVoice::RainbowKittyV2, true),
            (RainbowKitty, SoundVoice::Of(RainbowKitty), true),
            (Lumen, SoundVoice::Style, false),
            (Lumen, SoundVoice::RainbowKittyV2, true),
            (Lumen, SoundVoice::Of(RainbowKitty), true),
        ] {
            let e = SoundEvent {
                voice,
                ..ev(look, SoundKind::Land)
            };
            assert_eq!(synth.v2_engaged(&e), v2, "{voice:?} under {look:?}");
        }
        assert!(
            !synth.v2_engaged(&SoundEvent {
                voice: SoundVoice::RainbowKittyV2,
                ..bonk(RainbowKitty)
            }),
            "the bonk is style-agnostic and never the music box's"
        );
    }

    #[test]
    fn the_block_pre_roll_delays_every_voice_of_the_push_and_nothing_after() {
        const LEAD_S: f32 = 0.005;
        let typed = |style: GlowStyle| SoundEvent {
            bed: false,
            ..ev(style, SoundKind::Typed)
        };
        let onset = |style: GlowStyle, lead: f32| -> usize {
            let mut s = TrailSynth::new(48_000.0, 0xB10C_1EAD);
            s.push_meta(
                typed(style),
                EventMeta {
                    at_ms: 1_000,
                    block_lead_s: lead,
                    ..EventMeta::default()
                },
            );
            assert_eq!(s.block_lead_s, 0.0, "the latch is cleared on the way out");
            let mut out = vec![0.0f32; 4 * 512 * CHANNELS];
            for block in out.chunks_mut(512 * CHANNELS) {
                s.render(block);
            }
            out.iter()
                .position(|&x| x != 0.0)
                .expect("a typed key sounds")
                / CHANNELS
        };
        for style in [GlowStyle::Lumen, GlowStyle::RainbowKitty] {
            let (t0, t1) = (onset(style, 0.0), onset(style, LEAD_S));
            let want = (LEAD_S * 48_000.0).round() as usize;
            assert!(
                t1.abs_diff(t0 + want) <= 1,
                "{style:?}: onset moved {} frames for a {want}-frame lead",
                t1 as i64 - t0 as i64
            );
        }
        let mut s = TrailSynth::new(48_000.0, 1);
        s.push_meta(
            typed(GlowStyle::Lumen),
            EventMeta {
                block_lead_s: f32::NAN,
                ..EventMeta::default()
            },
        );
        assert!(s.voices.iter().all(|v| !v.on), "a NaN pre-roll is refused");
        s.push_meta(
            typed(GlowStyle::Lumen),
            EventMeta {
                block_lead_s: 9.0,
                ..EventMeta::default()
            },
        );
        let pending = s
            .voices
            .iter()
            .filter(|v| v.on)
            .map(|v| -v.t)
            .fold(0.0f32, f32::max);
        assert!(
            pending <= BLOCK_LEAD_MAX_S + 1e-6,
            "a wild pre-roll is clamped to the ceiling, got {pending}"
        );
    }

    const fn ev(style: GlowStyle, kind: SoundKind) -> SoundEvent {
        SoundEvent {
            style,
            voice: SoundVoice::Style,
            kind: SoundGesture::Trail(kind),
            pan: 0.0,
            heat: 0.5,
            hue: 0.3,
            gain: 0.4,
            tone: Tone::Technical,
            bed: true,
            shifted: false,
        }
    }

    const fn bonk(style: GlowStyle) -> SoundEvent {
        SoundEvent {
            style,
            voice: SoundVoice::Style,
            kind: SoundGesture::Words(WordGesture::Bonk),
            pan: 0.0,
            heat: 0.5,
            hue: 0.3,
            gain: 0.4,
            tone: Tone::Technical,
            bed: true,
            shifted: false,
        }
    }

    const fn output(style: GlowStyle, gesture: OutputGesture) -> SoundEvent {
        SoundEvent {
            style,
            voice: SoundVoice::Style,
            kind: SoundGesture::Output(gesture),
            pan: 0.0,
            heat: 0.0,
            hue: 0.3,
            gain: 0.4,
            tone: Tone::Technical,
            bed: false,
            shifted: false,
        }
    }

    const fn toned(style: GlowStyle, kind: SoundKind, tone: Tone) -> SoundEvent {
        SoundEvent {
            tone,
            ..ev(style, kind)
        }
    }

    const fn reduce(mut r: f32) -> f32 {
        while r < 1.0 {
            r *= 2.0;
        }
        while r >= 2.0 {
            r /= 2.0;
        }
        r
    }

    fn assert_outside_bonk_zones(raw: f32, ctx: &str) {
        let r = reduce(raw);
        let m2_zone = r > 1.0 + 1e-4 && r < 1.09;
        let tritone_zone = r > 1.395 && r < 1.43;
        assert!(
            !m2_zone && !tritone_zone,
            "{ctx}: interval {r} lands in a bonk rub zone"
        );
    }

    const STYLES: [GlowStyle; 8] = [
        GlowStyle::Lumen,
        GlowStyle::Phaser,
        GlowStyle::Sparkle,
        GlowStyle::Fire,
        GlowStyle::Laser,
        GlowStyle::Beam,
        GlowStyle::Water,
        GlowStyle::Comet,
    ];

    const LOOKS: [GlowStyle; 9] = [
        GlowStyle::Lumen,
        GlowStyle::Phaser,
        GlowStyle::RainbowKitty,
        GlowStyle::Sparkle,
        GlowStyle::Fire,
        GlowStyle::Laser,
        GlowStyle::Beam,
        GlowStyle::Water,
        GlowStyle::Comet,
    ];

    const VOICES: &[SoundVoice] = SoundVoice::ALL;

    const fn is_music_box(voice: SoundVoice, style: GlowStyle) -> bool {
        matches!(
            voice,
            SoundVoice::RainbowKittyV2 | SoundVoice::Of(GlowStyle::RainbowKitty)
        ) || (matches!(voice, SoundVoice::Style) && matches!(style, GlowStyle::RainbowKitty))
    }

    const KINDS: [SoundKind; 8] = [
        SoundKind::Typed,
        SoundKind::Backspace,
        SoundKind::Glide { dir: 1 },
        SoundKind::Navigation,
        SoundKind::Sweep { dir: 1 },
        SoundKind::Kill,
        SoundKind::Jump,
        SoundKind::Land,
    ];

    #[test]
    fn every_gesture_speaks_in_its_first_post_cue_buffer() {
        const BLOCK: usize = 512;
        const HEAD: usize = 16;
        for &voice in VOICES {
            for style in LOOKS {
                for kind in KINDS {
                    // The music box has no landing voice (§12.3: the meteor's
                    // bell is the landing), so there is no onset to time.
                    if is_music_box(voice, style) && kind == SoundKind::Land {
                        continue;
                    }
                    let mut s = TrailSynth::new(48_000.0, 0x0A7E);
                    let mut e = ev(style, kind);
                    e.voice = voice;
                    e.bed = false;
                    s.push(e);
                    let ctx = format!("{voice:?}/{style:?}/{kind:?}");
                    assert!(s.live_voices() > 0, "{ctx}: cued but spawned nothing");
                    let lead = s
                        .voices
                        .iter()
                        .filter(|v| v.on)
                        .map(|v| -v.t)
                        .fold(f32::MAX, f32::min);
                    assert_eq!(
                        lead, 0.0,
                        "{ctx}: the LEADING voice must be immediate — a pre-delay \
                         here is keystroke latency no host can claw back"
                    );
                    let mut blk = vec![0.0f32; BLOCK * CHANNELS];
                    s.render(&mut blk);
                    assert!(
                        blk[..HEAD * CHANNELS].iter().any(|x| *x != 0.0),
                        "{ctx}: the first post-cue buffer must OPEN with signal"
                    );
                }
            }
        }
    }

    #[test]
    fn all_styles_sound_and_decay() {
        use GlowStyle::{Lumen, RainbowKitty};
        const TRAILS: [SoundGesture; 6] = [
            SoundGesture::Trail(SoundKind::Typed),
            SoundGesture::Trail(SoundKind::Backspace),
            SoundGesture::Trail(SoundKind::Navigation),
            SoundGesture::Trail(SoundKind::Kill),
            SoundGesture::Trail(SoundKind::Jump),
            SoundGesture::Words(WordGesture::Bonk),
        ];
        const WITH_GLIDE: [SoundGesture; 7] = [
            SoundGesture::Trail(SoundKind::Typed),
            SoundGesture::Trail(SoundKind::Backspace),
            SoundGesture::Trail(SoundKind::Navigation),
            SoundGesture::Trail(SoundKind::Kill),
            SoundGesture::Trail(SoundKind::Jump),
            SoundGesture::Trail(SoundKind::Glide { dir: 1 }),
            SoundGesture::Words(WordGesture::Bonk),
        ];
        let mut rows: Vec<(SoundVoice, &[GlowStyle], &[SoundGesture])> = vec![
            (SoundVoice::Style, &LOOKS, &TRAILS),
            (SoundVoice::Mech, &[Lumen, RainbowKitty], &WITH_GLIDE),
        ];
        for voice in NEW_VOICES {
            rows.push((voice, &[Lumen, RainbowKitty], &WITH_GLIDE));
        }
        for (voice, styles, gestures) in rows {
            for &style in styles {
                for &gesture in gestures {
                    let what = format!("{voice:?}/{style:?}/{gesture:?}");
                    let mut s = TrailSynth::new(48_000.0, 42);
                    let mut e = voiced(voice, style, SoundKind::Typed);
                    e.kind = gesture;
                    s.push(e);
                    let mut peak = 0.0f32;
                    let mut buf = [0.0f32; 1024];
                    for _ in 0..(6 * 48_000 * 2 / 1024) {
                        s.render(&mut buf);
                        for &x in &buf {
                            assert!(x.is_finite(), "{what} produced non-finite");
                            peak = peak.max(x.abs());
                        }
                    }
                    assert!(peak > 1e-4, "{what} was inaudible (peak {peak})");
                    assert!(peak <= 0.98, "{what} clipped (peak {peak})");
                    assert!(s.is_quiet(), "{what} never decayed to silence");
                    s.render(&mut buf);
                    assert!(buf.iter().all(|&x| x == 0.0), "{what} quiet but nonzero");
                }
            }
        }
    }

    const fn voiced(voice: SoundVoice, style: GlowStyle, kind: SoundKind) -> SoundEvent {
        SoundEvent {
            voice,
            ..ev(style, kind)
        }
    }

    fn isolated_peak(voice: SoundVoice, kind: SoundKind, shifted: bool) -> f32 {
        let mut s = TrailSynth::new(48_000.0, 0x5EED_1234);
        let mut e = voiced(voice, GlowStyle::RainbowKitty, kind);
        e.hue = 0.0;
        e.bed = false;
        e.shifted = shifted;
        s.push(e);
        let mut buf = vec![0.0f32; (48_000.0f32 * 2.4) as usize * CHANNELS];
        s.render(&mut buf);
        buf.iter().fold(0.0f32, |m, v| m.max(v.abs()))
    }

    const NEW_VOICES: [SoundVoice; 3] = [
        SoundVoice::Typewriter,
        SoundVoice::Marimba,
        SoundVoice::Felt,
    ];

    #[test]
    fn voice_roster_round_trips() {
        const AUTO: Option<SoundVoice> = SoundVoice::parse("\u{00a0}AUTO\u{2003}");
        assert_eq!(AUTO, Some(SoundVoice::Style));
        assert_eq!(SoundVoice::parse("auto"), Some(SoundVoice::Style));
        for &v in SoundVoice::ALL {
            assert_eq!(SoundVoice::parse(v.name()), Some(v), "{v:?}");
            for padding in ["  ", "\u{00a0}", "\u{2003}\u{3000}"] {
                assert_eq!(
                    SoundVoice::parse(&format!(
                        "{padding}{}{padding}",
                        v.name().to_ascii_uppercase()
                    )),
                    Some(v)
                );
            }
        }
        for &(alias, voice) in SoundVoice::ALIASES {
            assert_eq!(SoundVoice::parse(alias), Some(voice), "alias {alias:?}");
            assert_eq!(
                SoundVoice::parse(&format!("\u{00a0}{}\u{2003}", alias.to_ascii_uppercase())),
                Some(voice),
                "padded alias {alias:?}"
            );
            assert!(
                SoundVoice::ALL.iter().all(|v| v.name() != alias),
                "alias {alias:?} shadows a canonical name"
            );
        }
        for junk in [
            "",
            "  ",
            "plasma",
            "glass",
            "auto bell",
            "\u{200b}auto",
            "auto\u{feff}",
            "warm  pluck",
            "music\u{00a0}box",
        ] {
            assert_eq!(SoundVoice::parse(junk), None, "{junk:?}");
        }
    }

    #[test]
    fn standalone_voice_is_the_style_palette_verbatim() {
        let script = [
            SoundKind::Typed,
            SoundKind::Typed,
            SoundKind::Backspace,
            SoundKind::Navigation,
            SoundKind::Glide { dir: 1 },
            SoundKind::Sweep { dir: -1 },
            SoundKind::Kill,
            SoundKind::Jump,
            SoundKind::Land,
            SoundKind::Typed,
        ];
        let run = |voice: SoundVoice, look: GlowStyle| {
            let mut s = TrailSynth::new(48_000.0, 0x0574_111D);
            let mut out = Vec::new();
            let mut buf = [0.0f32; 960];
            for kind in script {
                s.push(voiced(voice, look, kind));
                for _ in 0..8 {
                    s.render(&mut buf);
                    out.extend_from_slice(&buf);
                }
            }
            s.push(SoundEvent {
                voice,
                ..bonk(look)
            });
            for _ in 0..40 {
                s.render(&mut buf);
                out.extend_from_slice(&buf);
            }
            out
        };
        for style in LOOKS {
            let reference = run(SoundVoice::Style, style);
            let other = if style == GlowStyle::Lumen {
                GlowStyle::Comet
            } else {
                GlowStyle::Lumen
            };
            let standalone = run(SoundVoice::Of(style), other);
            assert!(
                reference.len() == standalone.len()
                    && reference
                        .iter()
                        .zip(&standalone)
                        .all(|(x, y)| x.to_bits() == y.to_bits()),
                "{style:?}: Of({style:?}) under {other:?} must be bit-identical to Style"
            );
        }
    }

    #[test]
    fn every_pitched_voice_lands_on_the_melody_note() {
        for voice in [SoundVoice::Marimba, SoundVoice::Felt] {
            let (ts, typed) = family_voices_in(voice, GlowStyle::Lumen, SoundKind::Typed, false);
            assert!(!typed.is_empty(), "{voice:?} speaks");
            let (t_land, t_enter) = typed[0];
            assert!(
                t_enter < t_land,
                "{voice:?}: a keystroke arrives from below its note: \
                 {t_enter}->{t_land}"
            );
            let anchor = palette_for(voice, GlowStyle::Lumen).anchor_hz();
            let expect_t = ts.melody_hz(anchor, ts.walk);
            assert!(
                (t_land - expect_t).abs() < 0.5,
                "{voice:?}: the keystroke lands on the melody note in its own register \
                 ({t_land} vs {expect_t})"
            );
        }
    }

    #[test]
    fn new_voice_beds_are_silent_for_keyboards_and_designed_for_instruments() {
        let run = |voice: SoundVoice, bed: bool| {
            let mut s = TrailSynth::new(48_000.0, 99);
            let mut out = Vec::new();
            let mut buf = [0.0f32; 960];
            for _ in 0..20 {
                let mut e = voiced(voice, GlowStyle::RainbowKitty, SoundKind::Typed);
                e.bed = bed;
                s.push(e);
                for _ in 0..4 {
                    s.render(&mut buf);
                    out.extend_from_slice(&buf);
                }
            }
            out
        };
        for (voice, silent) in [
            (SoundVoice::Mech, true),
            (SoundVoice::Typewriter, true),
            (SoundVoice::Marimba, false),
            (SoundVoice::Felt, false),
        ] {
            let (on, off) = (run(voice, true), run(voice, false));
            let same = on.iter().zip(&off).all(|(x, y)| x.to_bits() == y.to_bits());
            assert_eq!(
                same, silent,
                "{voice:?}: a keyboard's bed contributes exactly zero samples; an \
                 instrument's designed bed must change the render"
            );
        }
    }

    #[test]
    fn flood_is_ducked() {
        struct Flood {
            label: String,
            event: SoundEvent,
            seed: u32,
            blocks_per_key: usize,
            pan_sweep: bool,
            relative: bool,
        }
        let palette = |label: String, event: SoundEvent| Flood {
            label,
            event,
            seed: 3,
            blocks_per_key: 2,
            pan_sweep: true,
            relative: true,
        };
        let voice = |v: SoundVoice| Flood {
            label: format!("{v:?}"),
            event: voiced(v, GlowStyle::Lumen, SoundKind::Typed),
            seed: 7,
            blocks_per_key: 4,
            pan_sweep: false,
            relative: false,
        };
        let mut rows: Vec<Flood> = LOOKS
            .iter()
            .map(|&st| palette(format!("{st:?}"), ev(st, SoundKind::Typed)))
            .collect();
        rows.push(voice(SoundVoice::Mech));
        rows.extend(NEW_VOICES.map(voice));
        for row in rows {
            let label = &row.label;
            let synth = || TrailSynth::new(48_000.0, row.seed);
            let mut buf = [0.0f32; 960];
            let mut s = synth();
            let mut flood = 0.0f32;
            for i in 0..75 {
                let mut e = row.event;
                if row.pan_sweep {
                    e.pan = ((i % 20) as f32) / 10.0 - 1.0;
                }
                s.push(e);
                for _ in 0..row.blocks_per_key {
                    s.render(&mut buf);
                    for &x in &buf {
                        assert!(x.is_finite(), "{label}: flood produced non-finite");
                        flood = flood.max(x.abs());
                    }
                }
            }
            if row.relative {
                let mut s1 = synth();
                s1.push(row.event);
                let mut single = 0.0f32;
                for _ in 0..100 {
                    s1.render(&mut buf);
                    for &x in &buf {
                        single = single.max(x.abs());
                    }
                }
                assert!(
                    flood <= single * 2.5 + MASTER * (0.05 / 0.9),
                    "{label} flood peak {flood} vs single {single} — governor failed"
                );
            }
            assert!(flood < 0.5, "{label} flood absolute peak too hot: {flood}");
        }
    }

    #[test]
    fn a_drained_cue_batch_speaks_once_however_deep_the_backlog() {
        for style in LOOKS {
            let one = {
                let mut s = TrailSynth::new(48_000.0, 11);
                s.push(ev(style, SoundKind::Typed));
                s.live_voices()
            };
            assert!(one > 0, "{style:?}: a lone keystroke must speak");
            for depth in [8usize, 24] {
                let mut s = TrailSynth::new(48_000.0, 11);
                for _ in 0..depth {
                    s.push(ev(style, SoundKind::Typed));
                }
                if style == GlowStyle::RainbowKitty {
                    let mut buf = [0.0f32; 960];
                    let mut peak = 0.0f32;
                    for _ in 0..4 {
                        s.render(&mut buf);
                        for &x in &buf {
                            peak = peak.max(x.abs());
                        }
                    }
                    let mut lone = TrailSynth::new(48_000.0, 11);
                    lone.push(ev(style, SoundKind::Typed));
                    let mut single = 0.0f32;
                    for _ in 0..4 {
                        lone.render(&mut buf);
                        for &x in &buf {
                            single = single.max(x.abs());
                        }
                    }
                    assert!(
                        peak <= single * 2.5 + MASTER * (0.05 / 0.9),
                        "{style:?}: {depth} keystroke cues drained together peaked \
                         at {peak} against one key's {single} — the lane cap and \
                         the limiter are what must hold this, since the melody \
                         may no longer thin a key"
                    );
                    assert!(
                        peak < 0.5,
                        "{style:?}: {depth}-deep batch absolute peak too hot: {peak}"
                    );
                    continue;
                }
                assert_eq!(
                    s.live_voices(),
                    one,
                    "{style:?}: {depth} keystroke cues drained together must \
                     speak exactly as loudly as one — the min-gap governor \
                     thins the rest, so backlog depth cannot buy a click"
                );
            }
        }
    }

    #[test]
    fn bed_off_events_never_energise_the_bed_and_the_setting_reenables() {
        for style in STYLES {
            let mut s = TrailSynth::new(48_000.0, 7);
            let bedless = SoundEvent {
                bed: false,
                ..ev(style, SoundKind::Typed)
            };
            s.push(bedless);
            let mut buf = [0.0f32; 960];
            let mut peak = 0.0f32;
            for _ in 0..50 {
                s.render(&mut buf);
                peak = buf.iter().fold(peak, |m, x| m.max(x.abs()));
            }
            assert!(
                peak > 1e-4,
                "{style:?}: the note itself must still speak with the bed off"
            );
            assert_eq!(
                s.debug_bed(),
                (0.0, 0.0),
                "{style:?}: the bed floor never lifts under a bed-off event"
            );
            s.push(SoundEvent {
                bed: true,
                ..bedless
            });
            assert!(
                s.debug_bed().0 > 0.0,
                "{style:?}: a bed-on event re-energises the layer immediately"
            );
        }
    }

    #[test]
    fn sky_bed_pitches_stay_on_the_active_lattice_for_every_tone() {
        let mut bed_degrees: Vec<i32> = Vec::new();
        for chord in 0..rainbow_kitty_v2::SKY_BED_CHORDS {
            bed_degrees.extend(rainbow_kitty_v2::sky_bed_degrees(chord));
        }
        let melody_degrees: Vec<i32> = (-2..=10).collect();
        for tone in Tone::ALL {
            let mut s = TrailSynth::new(48_000.0, 1);
            s.tone = tone;
            for &bd in &bed_degrees {
                for &md in bed_degrees.iter().chain(&melody_degrees) {
                    let r = s.melody_hz(330.0, bd) / s.melody_hz(330.0, md);
                    let ctx = format!("{tone:?} bed deg {bd} vs deg {md}");
                    assert_outside_bonk_zones(r, &ctx);
                    assert_outside_bonk_zones(2.0 / reduce(r), &format!("{ctx} (inversion)"));
                }
            }
        }
    }

    #[test]
    fn nonfinite_event_fields_are_rejected_before_state_mutation() {
        for kind in [
            SoundGesture::Trail(SoundKind::Jump),
            SoundGesture::Words(WordGesture::Bonk),
        ] {
            for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                for field in 0..4 {
                    let mut s = TrailSynth::new(48_000.0, 17);
                    let mut e = ev(GlowStyle::Water, SoundKind::Jump);
                    e.kind = kind;
                    match field {
                        0 => e.pan = bad,
                        1 => e.heat = bad,
                        2 => e.hue = bad,
                        3 => e.gain = bad,
                        _ => unreachable!(),
                    }
                    s.push(e);
                    assert!(s.is_quiet(), "field {field}={bad:?} mutated synth state");
                    assert_eq!(s.live_voices(), 0);
                    let mut buf = [1.0f32; 512];
                    s.render(&mut buf);
                    assert!(
                        buf.iter()
                            .all(|sample| sample.is_finite() && *sample == 0.0)
                    );
                }
            }
        }
    }

    #[test]
    fn bonk_bypasses_min_gap_thinning_like_jump() {
        let mut s = TrailSynth::new(48_000.0, 21);
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        let after_first = s.live_voices();
        assert!(after_first >= 1);
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        assert_eq!(s.live_voices(), after_first, "typed within gap must thin");
        s.push(bonk(GlowStyle::Lumen));
        assert_eq!(
            s.live_voices(),
            after_first + 2,
            "bonk within gap must be admitted"
        );
    }

    #[test]
    fn output_cues_bypass_but_never_claim_the_human_voice_gap() {
        let mut s = TrailSynth::new(48_000.0, 0x0A17_0A17);
        s.push(ev(GlowStyle::Lumen, SoundKind::Jump));
        let after_enter = s.live_voices();
        let musical = |s: &TrailSynth| {
            (
                s.rng,
                s.walk,
                s.theme_pos,
                s.phrase_idx,
                s.bass_step,
                s.song_pulse,
                s.song_notes,
            )
        };
        let musical_state = musical(&s);
        let human_gap = s.since_voice;
        assert!(
            human_gap < MIN_GAP,
            "Enter must actually occupy the shared human admission gap"
        );

        s.push(output(GlowStyle::Lumen, OutputGesture::Shimmer));
        assert_eq!(s.live_voices(), after_enter + 1, "Shimmer must be admitted");
        assert_eq!(
            musical(&s),
            musical_state,
            "machine output must not rewrite the human melody"
        );
        assert_eq!(
            s.since_voice, human_gap,
            "machine output must not claim the next human admission slot"
        );

        s.since_voice = MIN_GAP;
        s.push(output(GlowStyle::Lumen, OutputGesture::Shimmer));
        let after_output = s.live_voices();
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        assert!(
            s.live_voices() > after_output,
            "an unauthored cue cannot steal the following key's beat"
        );
    }

    #[test]
    fn typing_transposes_with_the_song_and_returns_when_it_ends() {
        let hz_of = |s: &TrailSynth| {
            s.voices
                .iter()
                .filter(|v| v.on)
                .flat_map(|v| v.p.iter())
                .filter(|p| p.lvl > 0.0)
                .map(|p| p.f1)
                .fold(0.0f32, f32::max)
        };
        let mut plain = TrailSynth::new(48_000.0, 7);
        plain.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        let neutral = hz_of(&plain);
        assert!(neutral > 0.0, "the typed note speaks");

        const SIG_IN_KEY_TWO: u32 = 4;
        let bar = SoundEvent {
            kind: SoundGesture::Celebration(CelebrationGesture::riff_bar(0, SIG_IN_KEY_TWO)),
            ..ev(GlowStyle::Lumen, SoundKind::Typed)
        };
        let mut singing = TrailSynth::new(48_000.0, 7);
        singing.push(bar);
        assert_eq!(
            i32::from(singing.song_key),
            celebration_root(SIG_IN_KEY_TWO) + celebration_mode(SIG_IN_KEY_TWO),
            "the riff latches the key it actually transposes by (root + mode)"
        );
        let mut after = TrailSynth::new(48_000.0, 7);
        after.song_key = 2;
        after.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        let transposed = hz_of(&after);
        assert!(
            transposed > neutral * 1.05,
            "a typed note rides the song's key: {neutral} -> {transposed}"
        );

        let mut busy = TrailSynth::new(48_000.0, 7);
        busy.push(bar);
        assert_ne!(busy.song_key, 0, "the song latched a key");
        let mut blk = vec![0.0f32; 480 * CHANNELS];
        for i in 0..1_200 {
            if i % 5 == 0 {
                busy.push(ev(GlowStyle::Lumen, SoundKind::Typed));
            }
            busy.render(&mut blk);
            assert!(!busy.is_quiet(), "the fixture never falls silent");
        }
        assert_eq!(
            busy.song_key, 0,
            "the borrowed key is released with the sing duck, not with silence"
        );
    }

    #[test]
    fn bonk_is_discordant_against_the_walk_and_moves_nothing() {
        let mut s = TrailSynth::new(48_000.0, 33);
        let walk_before = s.walk;
        let root = penta(523.25, walk_before);
        s.push(bonk(GlowStyle::RainbowKitty));
        assert_eq!(s.walk, walk_before, "a bonk must not step the melody walk");
        assert_eq!(
            s.debug_bed(),
            (0.0, 0.0),
            "a bonk must not swell the ambient bed"
        );
        let clash: Vec<f32> = s
            .voices
            .iter()
            .filter(|v| v.on && v.duck_exempt)
            .flat_map(|v| v.p.iter())
            .filter(|p| p.lvl > 0.0 && p.f1 > root * 0.9)
            .map(|p| p.f1 / root)
            .collect();
        for ratio in [BONK_MINOR_SECOND, BONK_TRITONE] {
            assert!(
                clash.iter().any(|r| (r - ratio).abs() < 1e-3),
                "missing the {ratio} clash: {clash:?}"
            );
        }
    }

    #[test]
    fn bonk_ducks_the_melody_around_it() {
        let build = || {
            let mut s = TrailSynth::new(48_000.0, 77);
            let mut buf = [0.0f32; 960];
            for _ in 0..6 {
                s.push(ev(GlowStyle::Beam, SoundKind::Typed));
                for _ in 0..4 {
                    s.render(&mut buf);
                }
            }
            s
        };
        let mut plain = build();
        let mut bonked = build();
        bonked.push(bonk(GlowStyle::Beam));
        assert_eq!(bonked.duck, 1.0, "bonk admission must arm the duck");
        assert_eq!(
            plain.debug_bed(),
            bonked.debug_bed(),
            "the bonk must leave the bed untouched"
        );
        let mut skip = [0.0f32; 960];
        for _ in 0..32 {
            plain.render(&mut skip);
            bonked.render(&mut skip);
        }
        assert_eq!(bonked.live_voices(), 0, "bonk voices must be dead by now");
        let mut e_plain = 0.0f64;
        let mut e_bonked = 0.0f64;
        let mut buf = [0.0f32; 960];
        for _ in 0..20 {
            plain.render(&mut buf);
            for &x in &buf {
                e_plain += f64::from(x) * f64::from(x);
            }
            bonked.render(&mut buf);
            for &x in &buf {
                e_bonked += f64::from(x) * f64::from(x);
            }
        }
        assert!(
            e_bonked < e_plain * 0.9,
            "duck failed to dip the melody: bonked {e_bonked} vs plain {e_plain}"
        );
        for _ in 0..300 {
            bonked.render(&mut buf);
        }
        assert_eq!(bonked.duck, 0.0, "the duck must snap back to exact rest");
    }

    const fn riff_sig(style: GlowStyle, bar: u16, sig: u32) -> SoundEvent {
        SoundEvent {
            style,
            voice: SoundVoice::Style,
            kind: SoundGesture::Celebration(CelebrationGesture::riff_bar(bar, sig)),
            pan: 0.0,
            heat: 1.0,
            hue: 0.0,
            gain: 0.4,
            tone: Tone::Technical,
            bed: false,
            shifted: false,
        }
    }

    const fn riff(style: GlowStyle, bar: u16) -> SoundEvent {
        riff_sig(style, bar, crate::kitty_sing::NEUTRAL_SIGNATURE)
    }

    #[test]
    fn celebration_bypasses_min_gap_thinning() {
        let mut s = TrailSynth::new(48_000.0, 5);
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
        let before = s.live_voices();
        s.push(riff(GlowStyle::RainbowKitty, 0));
        assert_eq!(
            s.live_voices(),
            before + 8,
            "the whole bar must be admitted despite the gap"
        );
        let mut s = TrailSynth::new(48_000.0, 3);
        s.push(SoundEvent {
            bed: true,
            ..riff(GlowStyle::RainbowKitty, 0)
        });
        assert_eq!(
            s.debug_bed(),
            (0.0, 0.0),
            "a riff bar must not swell the ambience it plays over"
        );
    }

    #[test]
    fn celebration_ducks_the_melody_and_hands_back() {
        let quiet_note = |sing: bool| {
            let mut s = TrailSynth::new(48_000.0, 9);
            s.push(SoundEvent {
                bed: false,
                ..ev(GlowStyle::RainbowKitty, SoundKind::Typed)
            });
            if sing {
                s.sing = 1.0;
                s.sing_hold = 10.0;
            }
            let mut e = 0.0f64;
            let mut buf = [0.0f32; 960];
            for _ in 0..10 {
                s.render(&mut buf);
                for &x in &buf {
                    e += f64::from(x) * f64::from(x);
                }
            }
            e
        };
        let open = quiet_note(false);
        let ducked = quiet_note(true);
        assert!(
            ducked < open * 0.4,
            "sing duck must attenuate the melody: {ducked} vs {open}"
        );

        let mut s = TrailSynth::new(48_000.0, 9);
        s.push(riff(GlowStyle::RainbowKitty, 0));
        assert_eq!(s.sing, 1.0, "a riff bar must arm the sing duck");
        assert!(s.sing_hold > 0.0, "…and hold it for the bar");
        let mut buf = [0.0f32; 960];
        for block in 0..700 {
            if block % 20 == 0 {
                s.push(SoundEvent {
                    bed: false,
                    ..ev(GlowStyle::RainbowKitty, SoundKind::Navigation)
                });
            }
            s.render(&mut buf);
        }
        assert_eq!(s.sing, 0.0, "the sing duck must hand back to exact rest");
    }

    const fn outro_sig(sig: u32) -> SoundEvent {
        SoundEvent {
            kind: SoundGesture::Celebration(CelebrationGesture::outro(sig)),
            ..riff_sig(GlowStyle::RainbowKitty, 0, sig)
        }
    }

    #[test]
    fn the_outro_s_lead_is_do_and_holds_the_sing_duck() {
        let sig = crate::kitty_sing::song_signature('G');
        let mut s = TrailSynth::new(48_000.0, 9);
        s.push(riff_sig(GlowStyle::RainbowKitty, 0, sig));
        s.push(outro_sig(sig));
        let shift = celebration_root(sig) + celebration_mode(sig);
        let want = s.melody_hz(CELEBRATION_BASE_HZ, shift);
        let sub = s.melody_hz(CELEBRATION_BASE_HZ, CELEBRATION_OUTRO_SUB_DEG + shift);
        let lead = s
            .voices
            .iter()
            .filter(|v| v.on && v.celebration && v.duck_exempt)
            .find(|v| (v.p[0].f0 - want).abs() < 1e-3)
            .expect("the outro's lead sounds on do in the run's key");
        assert!((lead.p[2].f0 - sub).abs() < 1e-3, "…over the bass do");
        assert_eq!(s.sing, 1.0, "the outro holds the sing duck");
        assert!(
            (s.sing_hold - CELEBRATION_OUTRO_S).abs() < 1e-6,
            "…for exactly its own length, then hands back"
        );
    }

    #[test]
    fn rain_glints_are_muted_under_the_sing() {
        let glint = |style| SoundEvent {
            bed: false,
            ..ev(style, SoundKind::Stardust { twinkle_hz: 0 })
        };
        let mut s = TrailSynth::new(48_000.0, 9);
        let quiet = s.live_voices();
        s.push(glint(GlowStyle::RainbowKitty));
        assert!(
            s.live_voices() > quiet,
            "precondition: a glint sounds when nothing sings"
        );

        let mut s = TrailSynth::new(48_000.0, 9);
        s.push(riff(GlowStyle::RainbowKitty, 0));
        assert!(s.sing > 0.0);
        let under = s.live_voices();
        s.push(glint(GlowStyle::RainbowKitty));
        assert_eq!(
            s.live_voices(),
            under,
            "under the sing the glint is dropped"
        );

        let mut s = TrailSynth::new(48_000.0, 9);
        s.sing = 1.0;
        s.sing_hold = 10.0;
        let other = s.live_voices();
        s.push(glint(GlowStyle::Sparkle));
        assert!(
            s.live_voices() > other,
            "another palette's glint is not the music box's"
        );
    }

    #[test]
    fn celebration_bass_always_has_a_carrier() {
        for (b, (lead, bass)) in CELEBRATION_PHRASE.iter().zip(&CELEBRATION_BASS).enumerate() {
            for (beat, &deg) in bass.iter().enumerate() {
                if deg != REST {
                    assert_ne!(
                        lead[beat * 2],
                        REST,
                        "bar {b} beat {beat}: bass note has no carrier voice"
                    );
                }
            }
        }
    }

    #[test]
    fn celebration_bar_fits_the_polyphony_budget() {
        for (b, bar) in CELEBRATION_PHRASE.iter().enumerate() {
            let mut n = bar.iter().filter(|d| **d != REST).count();
            if b == CELEBRATION_PHRASE_BARS - 1 {
                n += CELEBRATION_FILL.len();
            }
            assert!(n <= 10, "bar {b} spawns {n} voices — over the riff budget");
        }
    }

    #[test]
    fn celebration_riff_follows_the_active_tone_lattice() {
        let render_toned = |tone: Tone| {
            let mut s = TrailSynth::new(48_000.0, 42);
            s.tone = tone;
            s.push(riff(GlowStyle::RainbowKitty, 0));
            let mut acc = 0u64;
            let mut buf = [0.0f32; 960];
            for _ in 0..160 {
                s.render(&mut buf);
                for &x in &buf {
                    assert!(x.is_finite(), "{tone:?} riff produced non-finite");
                    acc = acc.rotate_left(7).wrapping_add(u64::from(x.to_bits()));
                }
            }
            acc
        };
        let technical = render_toned(Tone::Technical);
        for tone in [Tone::Excited, Tone::Frustrated, Tone::Playful] {
            assert_ne!(
                render_toned(tone),
                technical,
                "{tone:?} must transpose the riff onto its own consonant lattice"
            );
        }
    }

    #[test]
    fn key_switch_damps_the_old_keys_ring() {
        let t = crate::kitty_sing::song_signature('t');
        let e = crate::kitty_sing::song_signature('e');
        assert_ne!(t, e, "fixture: two different songs");
        let mut s = TrailSynth::new(48_000.0, 42);
        let mut buf = [0.0f32; 960];
        for bar in [3u16, 4] {
            s.push(riff_sig(GlowStyle::RainbowKitty, bar, t));
            assert!(
                s.voices.iter().all(|v| v.damp == 0.0),
                "bar {bar}: a same-sig bar must never damp the ringing tails"
            );
            for _ in 0..160 {
                s.render(&mut buf);
            }
        }
        let ringing = s.voices.iter().filter(|v| v.on && v.celebration).count();
        assert!(
            ringing > 0,
            "fixture: the old key's tails must still ring at the boundary"
        );
        s.push(riff_sig(GlowStyle::RainbowKitty, 5, e));
        assert_eq!(
            s.voices.iter().filter(|v| v.on && v.damp > 0.0).count(),
            ringing,
            "every old tail must be ordered into the release"
        );
        for _ in 0..10 {
            s.render(&mut buf);
        }
        assert_eq!(
            s.voices.iter().filter(|v| v.on && v.damp > 0.0).count(),
            0,
            "the old key's ring must be silent within 100 ms of the boundary"
        );
        assert!(
            s.voices.iter().any(|v| v.on && v.celebration) && buf.iter().any(|&x| x != 0.0),
            "while the new verse sings on"
        );

        let mut s2 = TrailSynth::new(48_000.0, 42);
        s2.push(riff_sig(GlowStyle::RainbowKitty, 4, t));
        s2.push(riff_sig(GlowStyle::RainbowKitty, 5, e));
        for _ in 0..10 {
            s2.render(&mut buf);
        }
        assert_eq!(
            s2.voices.iter().filter(|v| v.on && v.damp > 0.0).count(),
            0,
            "an unstarted old-key voice must expire unheard, not sound late"
        );
    }

    #[test]
    fn cross_key_difference() {
        let intervals = |sig: u32| -> Vec<i32> {
            let mut degs = Vec::new();
            for (idx, &is_verse) in CELEBRATION_VERSE.iter().enumerate() {
                if is_verse {
                    degs.extend(
                        celebration_bar_degrees(sig, idx)
                            .into_iter()
                            .filter(|&d| d != REST),
                    );
                }
            }
            degs.windows(2).map(|w| w[1] - w[0]).collect()
        };
        let keys = ['w', 'a', 'z'];
        for (i, &a) in keys.iter().enumerate() {
            for &b in &keys[i + 1..] {
                assert_ne!(
                    intervals(crate::kitty_sing::song_signature(a)),
                    intervals(crate::kitty_sing::song_signature(b)),
                    "{a:?} and {b:?} must sing different verse contours"
                );
            }
        }
    }

    #[test]
    fn fill_carries_root() {
        let sig = 24u32;
        assert_eq!(celebration_root(sig), 2);
        assert_eq!(celebration_mode(sig), 0);
        let mut s = TrailSynth::new(48_000.0, 3);
        s.push(riff_sig(GlowStyle::RainbowKitty, 7, sig));
        let mut fills: Vec<&Voice> = s
            .voices
            .iter()
            .filter(|v| v.on && v.dur.to_bits() == CELEBRATION_EIGHTH.to_bits())
            .collect();
        fills.sort_by(|a, b| a.delay.total_cmp(&b.delay));
        assert_eq!(fills.len(), CELEBRATION_FILL.len(), "four fill sixteenths");
        for (k, v) in fills.iter().enumerate() {
            let deg = CELEBRATION_FILL[k];
            assert_eq!(
                v.p[0].f0.to_bits(),
                penta(CELEBRATION_BASE_HZ, deg + 2).to_bits(),
                "fill note {k} must sing in the held key's root"
            );
            assert_ne!(
                v.p[0].f0.to_bits(),
                penta(CELEBRATION_BASE_HZ, deg).to_bits(),
                "regression guard: fill note {k} fell back to the reference key"
            );
        }
    }

    #[test]
    fn verse_walk_always_moves_and_keeps_rhythm_and_register() {
        for deg in &CELEBRATION_FILL {
            assert!((0..=9).contains(deg), "fill degree {deg} out of range");
        }
        for deg in CELEBRATION_BASS.iter().flatten().filter(|d| **d != REST) {
            assert!(
                (-12..=-1).contains(deg),
                "bass degree {deg} outside the bass register"
            );
        }
        let sigs: Vec<u32> = "wazqx09~ \t"
            .chars()
            .map(crate::kitty_sing::song_signature)
            .chain([0, 1, 5, 11, 12, 24, 0xDEAD_BEEF, 0x7FFF_FFFF, u32::MAX])
            .collect();
        for &sig in &sigs {
            assert!((-2..=2).contains(&celebration_root(sig)), "root guard");
            assert!(MODE_ROTATIONS.contains(&celebration_mode(sig)));
            for idx in 0..CELEBRATION_PHRASE_BARS {
                let row = celebration_bar_degrees(sig, idx);
                if !CELEBRATION_VERSE[idx] {
                    assert_eq!(
                        row, CELEBRATION_PHRASE[idx],
                        "chorus bar {idx} drifted for sig {sig:#x}"
                    );
                }
                for slot in 0..row.len() {
                    assert_eq!(
                        row[slot] == REST,
                        CELEBRATION_PHRASE[idx][slot] == REST,
                        "bar {idx} slot {slot}: the rhythm is authored, never per-key"
                    );
                }
                let sounding: Vec<i32> = row.iter().copied().filter(|&d| d != REST).collect();
                for &d in &sounding {
                    assert!(
                        (0..=9).contains(&d),
                        "bar {idx}: degree {d} off the singable range (sig {sig:#x})"
                    );
                }
                if CELEBRATION_VERSE[idx] {
                    for w in sounding.windows(2) {
                        assert_ne!(w[0], w[1], "bar {idx}: a verse drone (sig {sig:#x})");
                    }
                }
            }
        }
    }

    #[test]
    fn mode_rotation_alone_changes_the_chorus_color() {
        let render = |sig: u32| {
            let mut s = TrailSynth::new(48_000.0, 42);
            s.push(riff_sig(GlowStyle::RainbowKitty, 6, sig));
            let mut acc = 0u64;
            let mut buf = [0.0f32; 960];
            for _ in 0..160 {
                s.render(&mut buf);
                for &x in &buf {
                    acc = acc.rotate_left(7).wrapping_add(u64::from(x.to_bits()));
                }
            }
            acc
        };
        assert_eq!(celebration_root(12), celebration_root(22));
        assert_ne!(celebration_mode(12), celebration_mode(22));
        assert_ne!(render(12), render(22), "the mode axis must be audible");
    }

    #[test]
    fn neutral_signature_is_the_reference_voicing() {
        use crate::kitty_sing::NEUTRAL_SIGNATURE;
        assert_eq!(celebration_root(NEUTRAL_SIGNATURE), 0);
        assert_eq!(celebration_mode(NEUTRAL_SIGNATURE), 0);
    }

    #[test]
    fn tone_tables_are_mutually_consonant_and_exclude_the_bonk_clash() {
        for tone in Tone::ALL {
            let (table, transpose) = tone_tables(tone);
            assert!(
                (1.0..2.0).contains(&transpose),
                "{tone:?}: transpose {transpose} out of the octave"
            );
            for (i, &a) in table.iter().enumerate() {
                assert!(
                    (1.0..2.0).contains(&a),
                    "{tone:?}: degree {i} out of octave"
                );
                for &b in &table[i..] {
                    let r = b / a;
                    assert_outside_bonk_zones(r, &format!("{tone:?} {a}:{b}"));
                    assert_outside_bonk_zones(2.0 / reduce(r), &format!("{tone:?} inv {a}:{b}"));
                }
            }
        }
        let m2 = reduce(BONK_MINOR_SECOND);
        assert!(m2 > 1.0 + 1e-4 && m2 < 1.09, "minor-second zone drifted");
        let tt = reduce(BONK_TRITONE);
        assert!(tt > 1.395 && tt < 1.43, "tritone zone drifted");
    }

    #[test]
    fn every_tone_recolors_and_still_decays_to_silence() {
        let render_hash = |tone: Tone| {
            let mut s = TrailSynth::new(48_000.0, 0xBEEF);
            let mut acc = 0u64;
            let mut buf = [0.0f32; 1024];
            for _ in 0..8 {
                s.push(toned(GlowStyle::Lumen, SoundKind::Typed, tone));
                for _ in 0..2 {
                    s.render(&mut buf);
                    for &x in &buf {
                        assert!(x.is_finite(), "{tone:?} produced non-finite");
                        assert!(x.abs() <= 0.98, "{tone:?} clipped");
                        acc = acc.rotate_left(9).wrapping_add(u64::from(x.to_bits()));
                    }
                }
            }
            for _ in 0..(6 * 48_000 * 2 / 1024) {
                s.render(&mut buf);
            }
            assert!(s.is_quiet(), "{tone:?} never decayed to silence");
            acc
        };
        let neutral = render_hash(Tone::Technical);
        for tone in [Tone::Calm, Tone::Excited, Tone::Frustrated, Tone::Playful] {
            assert_ne!(
                render_hash(tone),
                neutral,
                "{tone:?} rendered byte-identically to Technical"
            );
        }
    }

    #[test]
    fn the_bar_is_a_regular_beat_of_consonant_offsets() {
        let accents: Vec<usize> = SONG_PULSE
            .iter()
            .enumerate()
            .filter(|(_, s)| **s == SONG_ACCENT)
            .map(|(i, _)| i)
            .collect();
        assert!(
            accents.len() >= 2 && accents[0] == 0,
            "the bar opens on its downbeat"
        );
        let step = accents[1] - accents[0];
        assert!(
            accents.windows(2).all(|w| w[1] - w[0] == step)
                && SONG_PULSE.len().is_multiple_of(step),
            "the beat must be EVEN and close the bar so it loops in phase: {accents:?}"
        );
        for &slot in SONG_PULSE.iter().filter(|&&s| s != SONG_ACCENT) {
            assert!(
                (-5..=4).contains(&slot),
                "a ghost {slot} degrees out leaves the melody's register (an octave \
                 down is the floor: further and it lands in the space downbeat's band)"
            );
            assert_ne!(
                slot, 0,
                "a ghost on the melody's own degree is a second voice at the SAME \
                 frequency as the accent still ringing above it — a comb filter"
            );
        }
    }

    #[test]
    fn punctuation_and_the_note_after_a_pause_always_sing() {
        for kind in [SoundKind::Jump, SoundKind::Kill, SoundKind::Navigation] {
            let mut s = TrailSynth::new(48_000.0, 0x9011_1CE5);
            let mut buf = [0.0f32; 9600];
            s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
            s.render(&mut buf);
            let sung = s.song_notes;
            s.push(ev(GlowStyle::Lumen, kind));
            assert!(
                s.song_notes > sung && s.song_accent,
                "{kind:?} must land as a melody note, not a ghost"
            );
        }
        let mut s = TrailSynth::new(48_000.0, 0x9011_1CE6);
        let mut buf = [0.0f32; 9600];
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        s.render(&mut buf);
        assert_ne!(s.song_pulse, 0, "fixture: mid-bar");
        for _ in 0..8 {
            s.render(&mut buf);
        }
        let sung = s.song_notes;
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        assert!(
            s.song_notes > sung && s.song_accent,
            "the first note after a think must be the tune"
        );
        assert_eq!(s.song_pulse, 1, "…from the top of the bar");
    }

    #[test]
    fn the_theme_is_played_in_order_and_loops() {
        let mut s = TrailSynth::new(48_000.0, 0x0DDB_A11E);
        s.tone = Tone::Technical;
        let mut sung = Vec::new();
        while sung.len() < SONG_THEME.len() * 2 + 1 {
            let before = s.song_notes;
            s.since_voice = 1.0;
            s.since_event = 0.0;
            s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
            if s.song_notes != before {
                sung.push(s.walk as i8);
            }
        }
        assert_eq!(
            &sung[..SONG_THEME.len()],
            &SONG_THEME[..],
            "the first pass must be the theme, note for note"
        );
        assert_eq!(
            &sung[SONG_THEME.len()..SONG_THEME.len() * 2],
            &SONG_THEME[..],
            "…and the second pass is the same tune again, in phase"
        );
        assert_eq!(
            sung[SONG_THEME.len() * 2],
            SONG_THEME[0],
            "the loop falls back onto the hook"
        );
    }

    #[test]
    fn a_rest_cadences_the_phrase_and_opens_the_next() {
        let mut s = TrailSynth::new(48_000.0, 0xCADE_5EED);
        let mut buf = [0.0f32; 256];
        let accent = |s: &mut TrailSynth, kind: SoundKind| {
            let before = s.song_notes;
            while s.song_notes == before {
                s.since_voice = 1.0;
                s.since_event = 0.0;
                s.push(ev(GlowStyle::Lumen, kind));
            }
        };
        for _ in 0..3 {
            accent(&mut s, SoundKind::Typed);
        }
        assert_eq!(s.walk, 4, "fixture: three notes into the hook");
        assert_eq!(s.phrase_idx, 0, "…still inside phrase A");
        s.push(ev(GlowStyle::Lumen, SoundKind::Jump));
        assert_eq!(
            i32::from(SONG_THEME[usize::from(SONG_FORM[1]) - 1]),
            s.walk,
            "a rest cadences onto the phrase's own last note, got {}",
            s.walk
        );
        assert_eq!(s.phrase_idx, 1, "…and opens the ANSWER, not the hook again");
        s.render(&mut buf);
        accent(&mut s, SoundKind::Typed);
        assert_eq!(
            s.walk,
            i32::from(SONG_THEME[usize::from(SONG_FORM[1])]),
            "the phrase after a rest continues the form"
        );
        let mut p = TrailSynth::new(48_000.0, 0x9A11_5EED);
        for _ in 0..3 {
            accent(&mut p, SoundKind::Typed);
        }
        p.since_event = PHRASE_PAUSE_S + 0.1;
        p.since_voice = 1.0;
        p.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        assert_eq!(
            p.walk,
            i32::from(SONG_THEME[usize::from(SONG_FORM[1]) - 1]),
            "a comma-length gap cadences exactly as an Enter does"
        );
        assert_eq!(p.phrase_idx, 1, "…and advances the form too");
    }

    #[test]
    fn a_rest_before_a_phrase_starts_cannot_skip_it() {
        let mut s = TrailSynth::new(48_000.0, 0x5C1D_0001);
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        assert_eq!(
            s.walk,
            i32::from(SONG_THEME[0]),
            "a session opens on the theme's first note"
        );
        assert_eq!(s.phrase_idx, 0, "…inside the first phrase, not past it");
        s.since_event = PHRASE_PAUSE_S + 0.1;
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        assert_eq!(s.phrase_idx, 1, "one rest, one phrase");
        s.since_event = PHRASE_PAUSE_S + 0.1;
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        assert_eq!(
            s.phrase_idx, 1,
            "a rest with nothing played cannot burn a phrase"
        );
        assert_eq!(
            s.walk,
            i32::from(SONG_THEME[usize::from(SONG_FORM[1])]),
            "…it plays the phrase's first note instead"
        );
    }

    #[test]
    fn the_theme_never_folds_in_any_tone() {
        for tone in [
            Tone::Technical,
            Tone::Calm,
            Tone::Excited,
            Tone::Frustrated,
            Tone::Playful,
        ] {
            let (lo, hi) = tone_register(tone);
            let lean = melody_lean(tone);
            for (i, &deg) in SONG_THEME.iter().enumerate() {
                let want = i32::from(deg) + lean;
                assert_eq!(
                    fold_register(want, lo, hi),
                    want,
                    "{tone:?}: theme note {i} (degree {deg} + lean {lean}) folds out of \
                     {lo}..{hi} — the register is rewriting the melody"
                );
            }
        }
    }

    #[test]
    fn the_song_form_partitions_the_theme() {
        assert_eq!(usize::from(SONG_FORM[0]), 0, "the form opens at note 0");
        assert_eq!(
            usize::from(SONG_FORM[SONG_FORM.len() - 1]),
            SONG_THEME.len(),
            "the form's sentinel is the theme's length"
        );
        for w in SONG_FORM.windows(2) {
            assert!(
                w[1] > w[0],
                "a phrase must contain at least one note: {w:?}"
            );
        }
    }

    #[test]
    fn the_bass_walk_never_returns_inside_four_words() {
        const MIN_RETURN: usize = 4;
        assert_eq!(
            SONG_BASS
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            5,
            "the walk must use every root of the lattice"
        );
        let n = SONG_BASS.len();
        let mut s = TrailSynth::new(48_000.0, 0x5A_CE_11);
        for tone in Tone::ALL {
            s.tone = tone;
            for style in STYLES {
                let anchor = palette_for(SoundVoice::Style, style).anchor_hz();
                for key in -5i8..=5 {
                    s.song_key = key;
                    let hz: Vec<f32> = (0..n as u8).map(|i| s.space_root_hz(anchor, i)).collect();
                    for i in 0..n {
                        for j in i + 1..n {
                            let d = (j - i).min(n - (j - i));
                            let cents = 1200.0 * (hz[j] / hz[i]).log2().abs();
                            assert!(
                                cents > 25.0 || d >= MIN_RETURN,
                                "{style:?}/{tone:?}/key {key}: the walk returns to \
                                 {:.1} Hz after only {d} word(s) (steps {i} and {j})",
                                hz[i]
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_register_fold_is_total_and_never_flattens() {
        for (lo, hi) in [(0, 7), (0, 6), (0, 8), (3, 3)] {
            for v in -40..40 {
                let f = fold_register(v, lo, hi);
                assert!(f >= lo && f <= hi, "fold({v}) = {f} escaped {lo}..{hi}");
            }
        }
        assert_ne!(fold_register(8, 0, 7), fold_register(9, 0, 7));
        assert_ne!(fold_register(-1, 0, 7), fold_register(-2, 0, 7));
    }

    fn family_voices(style: GlowStyle, kind: SoundKind) -> (TrailSynth, Vec<(f32, f32)>) {
        family_voices_in(SoundVoice::Style, style, kind, false)
    }

    fn family_voices_in(
        voice: SoundVoice,
        style: GlowStyle,
        kind: SoundKind,
        shifted: bool,
    ) -> (TrailSynth, Vec<(f32, f32)>) {
        let mut s = TrailSynth::new(48_000.0, 0xFA_1117);
        let mut buf = [0.0f32; 1024];
        for _ in 0..3 {
            s.push(SoundEvent {
                voice,
                ..ev(style, SoundKind::Typed)
            });
            for _ in 0..25 {
                s.render(&mut buf);
            }
        }
        assert_eq!(
            SONG_PULSE[usize::from(s.song_pulse)],
            SONG_ACCENT,
            "fixture: the probe must speak on an accent"
        );
        let before: [bool; MAX_VOICES] = core::array::from_fn(|i| s.voices[i].on);
        s.since_voice = 1.0;
        s.push(SoundEvent {
            voice,
            shifted,
            ..ev(style, kind)
        });
        let mut out: Vec<(f32, f32)> = s
            .voices
            .iter()
            .enumerate()
            .filter(|(i, v)| v.on && !before[*i])
            .filter_map(|(_, v)| {
                let p = v.p.iter().max_by(|a, b| a.lvl.total_cmp(&b.lvl))?;
                (p.lvl > 0.0).then_some((p.f1, p.f0))
            })
            .collect();
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        (s, out)
    }

    #[test]
    fn a_deletion_does_not_advance_the_song() {
        let mut a = TrailSynth::new(48_000.0, 0xD0_1E_7E);
        let mut b = TrailSynth::new(48_000.0, 0xD0_1E_7E);
        let mut buf = [0.0f32; 1024];
        for _ in 0..5 {
            a.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
            b.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
            for _ in 0..9 {
                a.render(&mut buf);
                b.render(&mut buf);
            }
        }
        for _ in 0..12 {
            b.push(ev(GlowStyle::RainbowKitty, SoundKind::Backspace));
            for _ in 0..9 {
                a.render(&mut buf);
                b.render(&mut buf);
            }
        }
        for _ in 0..9 {
            a.render(&mut buf);
        }
        assert_eq!(
            a.walk,
            b.walk,
            "twelve corrections moved the song by {} degrees",
            b.walk - a.walk
        );
        assert_eq!(
            (a.theme_pos, a.phrase_idx),
            (b.theme_pos, b.phrase_idx),
            "…and they must not have moved the theme's playhead either"
        );
    }

    #[test]
    fn the_space_is_a_lawful_walking_bass() {
        let (s, spaces) = family_voices(GlowStyle::Lumen, SoundKind::Space);
        let (land, enter) = spaces[0];
        assert!(
            (enter - land).abs() < 0.5,
            "a bass root arrives, it does not lean: {enter} -> {land}"
        );
        let anchor = palette_for(SoundVoice::Style, GlowStyle::Lumen).anchor_hz();
        let expect = s.space_root_hz(anchor, 0);
        assert!(
            (land - expect).abs() < 0.5,
            "the first word's root is the palette's octave-folded tonic: \
             got {land}, expected {expect}"
        );
        assert!(
            (SPACE_BASS_LO_HZ * 2.0..SPACE_BASS_LO_HZ * 4.0).contains(&land),
            "…inside the ONE audible bass register ({land} Hz)"
        );
        let new_root = |w: &mut TrailSynth| {
            let before: [bool; MAX_VOICES] = core::array::from_fn(|i| w.voices[i].on);
            w.since_voice = 1.0;
            w.push(ev(GlowStyle::Lumen, SoundKind::Space));
            w.voices
                .iter()
                .enumerate()
                .filter(|(i, v)| v.on && !before[*i] && v.bass)
                .filter_map(|(_, v)| (v.p[0].lvl > 0.0).then_some(v.p[0].f1))
                .next()
                .expect("the bass speaks")
        };
        let mut w = TrailSynth::new(48_000.0, 0xBA_55_11);
        let mut buf = [0.0f32; 2048];
        for i in 0..SONG_BASS.len() {
            let f = new_root(&mut w);
            let want = s.space_root_hz(anchor, i as u8);
            assert!(
                (f - want).abs() < 0.5,
                "word {i} played {f} Hz, not the walk's root {want} Hz"
            );
            w.render(&mut buf);
            w.since_voice = 1.0;
            w.push(ev(GlowStyle::Lumen, SoundKind::Typed));
            w.render(&mut buf);
        }
        let mut t = TrailSynth::new(48_000.0, 0xFA_1117);
        let mut moved = false;
        for _ in 0..40 {
            t.push(ev(GlowStyle::Lumen, SoundKind::Typed));
            for _ in 0..6 {
                t.render(&mut buf);
            }
            if t.walk != s.walk {
                moved = true;
                break;
            }
        }
        assert!(moved, "fixture: the melody must reach a different degree");
        assert_eq!(t.bass_step, 0, "fixture: no word boundary has passed");
        let far = new_root(&mut t);
        assert!(
            (far - land).abs() < 0.5,
            "the bass must not follow the melody: {far} Hz at degree {} \
             against {land} Hz at degree {}",
            t.walk,
            s.walk
        );
        let (tp, _) = family_voices(GlowStyle::Lumen, SoundKind::Typed);
        assert_eq!(
            s.theme_pos,
            tp.theme_pos.saturating_sub(1),
            "a space must not step the song (the Typed probe stepped once more)"
        );
    }

    #[test]
    fn a_word_boundary_re_bars_the_pulse() {
        let mut s = TrailSynth::new(48_000.0, 0xBA_12_11);
        let mut buf = [0.0f32; 2048];
        for _ in 0..5 {
            s.since_voice = 1.0;
            s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
            s.render(&mut buf);
        }
        assert_ne!(s.song_pulse, 0, "fixture: the bar is mid-figure");
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Space));
        assert_eq!(s.song_pulse, 0, "a word boundary opens a fresh bar");
        let sung = s.song_notes;
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        assert_eq!(
            s.song_notes,
            sung + 1,
            "the first letter of a word must be an accent"
        );
        assert!(s.song_accent, "…and must be designed as one");
        s.render(&mut buf);
        for _ in 0..3 {
            s.since_voice = 1.0;
            s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
            s.render(&mut buf);
        }
        let mid = s.song_pulse;
        assert_ne!(mid, 0, "fixture: the bar is mid-figure again");
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Space));
        assert_eq!(s.song_pulse, 0, "the run's HEAD re-bars");
        s.render(&mut buf);
        for _ in 0..3 {
            s.since_voice = 1.0;
            s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
            s.render(&mut buf);
        }
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        s.render(&mut buf);
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Space));
        let at_head = s.song_pulse;
        s.render(&mut buf);
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Space));
        assert_eq!(
            s.song_pulse, at_head,
            "the tail of a whitespace run must not re-bar again"
        );
    }

    #[test]
    fn a_whitespace_run_lands_one_downbeat() {
        let mut s = TrailSynth::new(48_000.0, 0x5A_CE_00);
        let mut buf = [0.0f32; 8000];
        let bass = |s: &TrailSynth| s.voices.iter().filter(|v| v.on && v.bass).count();
        let space = |s: &mut TrailSynth| {
            let before = s.live_voices();
            s.push(ev(GlowStyle::Lumen, SoundKind::Space));
            s.live_voices() - before
        };
        let step_before = s.bass_step;
        assert_eq!(space(&mut s), 2, "the head speaks: a root and its air");
        assert_eq!(bass(&s), 1, "…exactly one of them a bass root");
        let head_root = s
            .voices
            .iter()
            .find(|v| v.on && v.bass)
            .map(|v| v.p[0].f1)
            .expect("the head's root");
        let mut last_step = head_root;
        let mut held = false;
        for i in 0..12 {
            s.render(&mut buf);
            let before: [bool; MAX_VOICES] = core::array::from_fn(|k| s.voices[k].on);
            assert_eq!(
                space(&mut s),
                2,
                "space {i} of the run must SPEAK twice — its breath and its indent \
                 step (a key that makes no sound reads as a dropped keystroke)"
            );
            assert!(
                bass(&s) <= 1,
                "a run of spaces must never stack bass roots (found {})",
                bass(&s)
            );
            let step = s
                .voices
                .iter()
                .enumerate()
                .filter(|(k, v)| v.on && !before[*k] && !v.bass && v.p[0].lvl > 0.0)
                .map(|(_, v)| v.p[0].f1)
                .next()
                .expect("…and its INDENT STEP: a pitched bell");
            if held {
                assert!(
                    (step - last_step).abs() < 0.01,
                    "once the figure has reached the roof it HOLDS its top note \
                     ({last_step} -> {step} Hz on step {i})"
                );
            } else if (step - last_step).abs() < 0.01 && i > 0 {
                held = true;
                assert!(
                    step * 1.2001 >= SPACE_STEP_HI_HZ,
                    "the figure may only hold when the next lattice degree would cross \
                     the roof ({step} Hz held under {SPACE_STEP_HI_HZ})"
                );
            } else {
                assert!(
                    step > last_step * 1.05,
                    "step {i} must climb over the note before it ({last_step} -> {step} Hz)"
                );
            }
            assert!(
                (SPACE_BASS_LO_HZ * 2.0..SPACE_STEP_HI_HZ).contains(&step),
                "step {i} must sit in the space register or the octave over it ({step} Hz)"
            );
            last_step = step;
        }
        assert!(
            held,
            "twelve tail spaces from a root under 440 Hz must reach the 880 Hz roof and hold"
        );
        assert_eq!(
            s.bass_step,
            (step_before + 1) % SONG_BASS.len() as u8,
            "a whitespace run must spend exactly one step of the walk"
        );
        s.render(&mut buf);
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        s.render(&mut buf);
        s.push(ev(GlowStyle::Lumen, SoundKind::Space));
        assert!(
            s.voices.iter().any(|v| v.on && v.bass && v.damp <= 0.0),
            "a word boundary after a letter is a fresh root"
        );
        assert_eq!(
            s.bass_step,
            (step_before + 2) % SONG_BASS.len() as u8,
            "…and the harmony moves on"
        );
    }

    #[test]
    fn a_held_spacebar_cannot_machine_gun_the_indent_steps() {
        let mut s = TrailSynth::new(48_000.0, 0x5A_CE_33);
        let mut buf = [0.0f32; 3_168];
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        s.render(&mut buf);
        let (mut steps, mut breaths) = (0usize, 0usize);
        for _ in 0..30 {
            let before: [bool; MAX_VOICES] = core::array::from_fn(|i| s.voices[i].on);
            s.push(ev(GlowStyle::Lumen, SoundKind::Space));
            for (i, v) in s.voices.iter().enumerate() {
                if v.on && !before[i] && !v.bass {
                    if v.p[0].lvl > 0.0 {
                        steps += 1;
                    } else {
                        breaths += 1;
                    }
                }
            }
            s.render(&mut buf);
        }
        assert!(
            (4..=13).contains(&steps),
            "a held spacebar minted {steps} steps/s — the gate admits one per 75+ ms"
        );
        assert!(breaths >= steps, "every admitted tail space breathes");
    }

    #[test]
    fn two_word_boundaries_never_stack_one_bass_note() {
        let mut s = TrailSynth::new(48_000.0, 0xBA_55_01);
        let mut buf = [0.0f32; 4096];
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Space));
        s.render(&mut buf);
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
        s.render(&mut buf);
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Space));
        let live: Vec<&Voice> = s.voices.iter().filter(|v| v.on && v.bass).collect();
        let undamped = live.iter().filter(|v| v.damp <= 0.0).count();
        assert_eq!(
            undamped,
            1,
            "exactly one bass root may be sounding at a time (of {} live)",
            live.len()
        );
        assert!(
            live.iter().any(|v| v.damp > 0.0 && v.damp0 == SPACE_DAMP_S),
            "the previous root is released, not cut: a hard stop clicks"
        );
    }

    #[test]
    fn shift_is_the_ting_and_never_claims_the_beat() {
        let live = |s: &TrailSynth| s.voices.iter().filter(|v| v.on).count();
        let mut s = TrailSynth::new(48_000.0, 0xC0FF_EE01);
        s.push(ev(GlowStyle::Lumen, SoundKind::Shift));
        let before = live(&s);
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        assert!(
            live(&s) > before,
            "the capital right behind a shift must not be thinned by its own grace note"
        );
        let mut s = TrailSynth::new(48_000.0, 0xC0FF_EE02);
        let mut buf = [0.0f32; 2_880];
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        s.render(&mut buf);
        let before = live(&s);
        s.push(ev(GlowStyle::Lumen, SoundKind::Shift));
        assert_eq!(
            live(&s),
            before + 1,
            "a Shift 30 ms after the letter before its capital must ting — the \
             keystroke gap is not the ting's gate"
        );
        s.render(&mut buf);
        let before = live(&s);
        s.push(ev(GlowStyle::Lumen, SoundKind::Shift));
        assert_eq!(
            live(&s),
            before,
            "a second Shift 30 ms after an admitted one is thinned (SHIFT_MIN_GAP)"
        );
        s.render(&mut buf);
        s.render(&mut buf);
        let before = live(&s);
        s.push(ev(GlowStyle::Lumen, SoundKind::Shift));
        assert_eq!(
            live(&s),
            before + 1,
            "a Shift 90 ms after the last admitted one tings"
        );
    }

    #[test]
    fn a_thinned_space_head_re_opens_the_run() {
        let mut buf = [0.0f32; 2_880];
        let fresh_after_space = |s: &mut TrailSynth| -> Vec<Voice> {
            let before: [bool; MAX_VOICES] = core::array::from_fn(|i| s.voices[i].on);
            s.push(ev(GlowStyle::Lumen, SoundKind::Space));
            s.voices
                .iter()
                .enumerate()
                .filter(|(i, v)| v.on && !before[*i])
                .map(|(_, v)| *v)
                .collect()
        };
        let mut c = TrailSynth::new(48_000.0, 0x5A_CE_7E);
        c.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        for _ in 0..4 {
            c.render(&mut buf);
        }
        let head = fresh_after_space(&mut c);
        assert_eq!(
            head.iter().filter(|v| v.bass).count(),
            1,
            "fixture: a head is one root"
        );

        let mut s = TrailSynth::new(48_000.0, 0x5A_CE_7E);
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        s.render(&mut buf);
        let thinned = fresh_after_space(&mut s);
        assert!(
            thinned.is_empty(),
            "fixture: the first space lands inside MIN_GAP and is thinned"
        );
        assert_eq!(s.space_run_root_hz, 0.0, "fixture: no root has sounded yet");
        for _ in 0..3 {
            s.render(&mut buf);
        }
        let fresh = fresh_after_space(&mut s);
        assert_eq!(
            fresh.iter().filter(|v| v.bass).count(),
            1,
            "the space after a thinned head is the head: one bass root"
        );
        assert_eq!(
            fresh.len(),
            head.len(),
            "…and exactly a head's voices — no indent step off a root nobody heard \
             ({} voices against the control's {})",
            fresh.len(),
            head.len()
        );
        assert!(
            fresh.iter().all(|v| v.dur != SPACE_STEP_DUR_S || v.bass),
            "no voice on the indent step's envelope"
        );
        assert!(
            s.space_run_root_hz > 0.0,
            "the run is rooted on the note that sounded"
        );
        assert_eq!(
            s.space_run_len, 0,
            "the run's length restarts at its real head"
        );
    }

    #[test]
    fn the_ting_rotates_and_never_drones() {
        let mut s = TrailSynth::new(48_000.0, 0x5111_F700);
        let mut buf = [0.0f32; 2048];
        let anchor = palette_for(SoundVoice::Style, GlowStyle::Lumen).anchor_hz();
        let walk0 = s.walk;
        let mut sung: Vec<f32> = Vec::new();
        for _ in 0..3 * SHIFT_ROTATION.len() {
            let before: [bool; MAX_VOICES] = core::array::from_fn(|i| s.voices[i].on);
            s.since_voice = 1.0;
            s.since_shift = 1.0;
            s.push(ev(GlowStyle::Lumen, SoundKind::Shift));
            let (f, enter) = s
                .voices
                .iter()
                .enumerate()
                .filter(|(i, v)| v.on && !before[*i] && v.p[0].lvl > 0.0)
                .map(|(_, v)| (v.p[0].f1, v.p[0].f0))
                .next()
                .expect("the lift speaks");
            assert!(
                (enter - f).abs() < 0.5,
                "a ting is struck, it does not lean: {enter} -> {f}"
            );
            sung.push(f);
            s.render(&mut buf);
        }
        assert_eq!(
            s.walk, walk0,
            "fixture: a bare shift composes nothing — the melody must not have moved"
        );
        for i in 0..sung.len() {
            for j in i + 1..sung.len() {
                let d = j - i;
                assert!(
                    1200.0 * (sung[j] / sung[i]).log2().abs() > 25.0 || d >= SHIFT_ROTATION.len(),
                    "lift {j} repeated lift {i}'s note after {d} press(es): {sung:?}"
                );
            }
        }
        for (i, &f) in sung.iter().enumerate() {
            let want = ting_octave(s.melody_hz(
                anchor,
                walk0 + GESTURE_CHAR_STEP + SHIFT_ROTATION[i % SHIFT_ROTATION.len()],
            ));
            assert!(
                (f - want).abs() < 0.5,
                "ting {i} played {f} Hz, not the rotation's degree at {want} Hz"
            );
            assert!(
                (SHIFT_TING_LO_HZ..SHIFT_TING_LO_HZ * 2.0).contains(&f),
                "ting {i} left its register ({f} Hz)"
            );
        }
        let mut t = TrailSynth::new(48_000.0, 0x5111_F701);
        t.since_voice = 1.0;
        t.push(ev(GlowStyle::Lumen, SoundKind::Shift));
        let after_one = t.shift_step;
        assert_eq!(after_one, 1, "fixture: the first lift sounded");
        t.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        let before: [bool; MAX_VOICES] = core::array::from_fn(|i| t.voices[i].on);
        t.push(ev(GlowStyle::Lumen, SoundKind::Shift));
        assert!(
            !t.voices.iter().enumerate().any(|(i, v)| v.on && !before[i]),
            "fixture: the lift behind a keystroke must be thinned"
        );
        assert_eq!(
            t.shift_step, after_one,
            "a thinned lift must not step the rotation"
        );
    }

    #[test]
    fn a_shifted_glyph_lands_an_octave_over_its_plain_self() {
        for style in STYLES {
            let (plain_s, plain) =
                family_voices_in(SoundVoice::Style, style, SoundKind::Typed, false);
            let (lifted_s, lifted) =
                family_voices_in(SoundVoice::Style, style, SoundKind::Typed, true);
            let plain_hz: Vec<f32> = plain.iter().map(|&(f1, _)| f1).collect();
            assert!(
                !plain_hz.is_empty() && plain_hz.len() == lifted.len(),
                "{style:?}: the keystroke speaks, and the lift adds or drops no voice"
            );
            let near = |t: f32| {
                plain_hz
                    .iter()
                    .any(|&p| (t - p).abs() < 0.5f32.max(p * 0.002))
            };
            let mut octaves = 0;
            for &(f, _) in &lifted {
                if near(f / 2.0) {
                    octaves += 1;
                } else {
                    assert!(
                        near(f),
                        "{style:?}: a shifted voice at {f} Hz is neither its plain \
                         self nor an octave over it ({plain_hz:?})"
                    );
                }
            }
            assert!(
                octaves >= 1,
                "{style:?}: no voice took the capital's octave ({plain_hz:?} -> {lifted:?})"
            );
            assert_eq!(
                (lifted_s.walk, lifted_s.theme_pos, lifted_s.phrase_idx),
                (plain_s.walk, plain_s.theme_pos, plain_s.phrase_idx),
                "{style:?}: shiftedness must not move the tune"
            );
        }
        let root = |shifted: bool| {
            let mut s = TrailSynth::new(48_000.0, 0x5A_FE_01);
            s.push(SoundEvent {
                shifted,
                ..ev(GlowStyle::Lumen, SoundKind::Space)
            });
            s.voices
                .iter()
                .filter(|v| v.on && v.bass)
                .map(|v| v.p[0].f1)
                .next()
        };
        assert_eq!(
            root(false),
            root(true),
            "a shifted Space is still a word boundary"
        );
        for &voice in SoundVoice::ALL {
            let plain = isolated_peak(voice, SoundKind::Typed, false);
            let cap = isolated_peak(voice, SoundKind::Typed, true);
            let db = 20.0 * (cap / plain).log10();
            assert!(
                db >= 1.5,
                "{voice:?}: a capital must be LOUDER than its plain self \
                 (plain {plain}, shifted {cap}: {db:+.2} dB)"
            );
        }
    }

    #[test]
    fn the_ting_decays_out_instead_of_being_cut() {
        let life_ms = SHIFT_TING_DUR_S * 1000.0;
        let ramp_ms = life_ms - RELEASE_RAMP_S * 1000.0;
        for (voice, style) in [
            (SoundVoice::Style, GlowStyle::Lumen),
            (SoundVoice::Style, GlowStyle::Comet),
            (SoundVoice::Felt, GlowStyle::Lumen),
        ] {
            let mut s = TrailSynth::new(48_000.0, 0x504F_4F46);
            s.push_meta(
                SoundEvent {
                    hue: 0.0,
                    bed: false,
                    ..voiced(voice, style, SoundKind::Shift)
                },
                EventMeta {
                    at_ms: 1,
                    ..EventMeta::default()
                },
            );
            let mut o = vec![0.0f32; 24_000 * CHANNELS];
            s.render(&mut o);
            let m: Vec<f32> = o.chunks(CHANNELS).map(|f| 0.5 * (f[0] + f[1])).collect();
            let peak = m.iter().fold(0.0f32, |a, x| a.max(x.abs()));
            let ring = |db: f32| {
                m.iter()
                    .rposition(|x| x.abs() > peak * 10f32.powf(db / 20.0))
                    .map_or(0.0, |i| i as f32 / 48.0)
            };
            let (to40, to60) = (ring(-40.0), ring(-60.0));
            assert!(
                to40 + 10.0 <= ramp_ms,
                "{voice:?}/{style:?}: the ting must reach −40 dB re its peak by decay, at least \
                 10 ms before the ramp starts ({to40:.1} ms vs the ramp at {ramp_ms:.1})"
            );
            assert!(
                to60 >= ramp_ms && to60 <= life_ms,
                "{voice:?}/{style:?}: the −60 dB point should be the ramp's ({to60:.1} ms, ramp \
                 {ramp_ms:.1}..{life_ms:.1}) — otherwise the −40 dB reading was a cut"
            );
        }
    }

    #[test]
    fn the_deletion_is_a_poof_and_only_a_poof() {
        for kind in [SoundKind::Backspace, SoundKind::KillWord] {
            for &voice in SoundVoice::ALL {
                for style in LOOKS {
                    let ctx = format!("{kind:?}/{voice:?}/{style:?}");
                    let mut s = TrailSynth::new(48_000.0, 0xFE17_0FF5);
                    let walk = s.walk;
                    let mut e = voiced(voice, style, kind);
                    e.bed = false;
                    s.push(e);
                    assert_eq!(s.walk, walk, "{ctx}: a deletion does not compose");
                    let live: Vec<&Voice> = s.voices.iter().filter(|v| v.on).collect();
                    assert_eq!(
                        live.len(),
                        2,
                        "{ctx}: a deletion is the body and the cap — no palette voice \
                         may speak under them"
                    );
                    assert!(
                        live.iter().any(|v| v.delay == 0.0),
                        "{ctx}: the body speaks in the first buffer"
                    );
                    for v in &live {
                        assert!(
                            v.p.iter().all(|p| p.lvl <= 0.0) && v.n_lvl > 0.0,
                            "{ctx}: the poof is noise and carries NO tonal partial"
                        );
                        if v.delay == 0.0 {
                            assert!(
                                v.n_glide <= 0.0 && v.n_f0 == v.n_f1,
                                "{ctx}: the BODY's band is static"
                            );
                        }
                        assert!(v.n_q < 1.0, "{ctx}: broad Q — a resonant burst whistles");
                        assert!(v.attack >= 0.0015, "{ctx}: a sub-millisecond attack clicks");
                    }
                }
            }
        }
    }

    #[test]
    fn a_word_motion_is_a_character_motion_at_scale() {
        let anchor = palette_for(SoundVoice::Style, GlowStyle::Lumen).anchor_hz();
        for dir in [1i8, -1] {
            let (gs, glide) = family_voices(GlowStyle::Lumen, SoundKind::Glide { dir });
            let (ss, sweep) = family_voices(GlowStyle::Lumen, SoundKind::Sweep { dir });
            assert_eq!(glide.len(), 1, "a character motion is one note");
            assert_eq!(
                sweep.len(),
                CURSOR_SWEEP_RUN,
                "a word motion is one note per character crossed"
            );
            assert_eq!(gs.walk, ss.walk, "fixture: one degree for both probes");
            let step = i32::from(dir) * GESTURE_CHAR_STEP;
            assert!(
                (glide[0].0 - gs.melody_hz(anchor, gs.walk + step)).abs() < 0.5,
                "dir {dir}: a character motion must land one step out"
            );
            let mut expect: Vec<f32> = (0..CURSOR_SWEEP_RUN)
                .map(|i| ss.melody_hz(anchor, ss.walk + step * i as i32))
                .collect();
            expect.sort_by(f32::total_cmp);
            for (got, want) in sweep.iter().map(|p| p.0).zip(expect) {
                assert!(
                    (got - want).abs() < 0.5,
                    "dir {dir}: the run must be the character step repeated: {got} vs {want}"
                );
            }
            let span = sweep[sweep.len() - 1].0 / sweep[0].0;
            let one =
                gs.melody_hz(anchor, gs.walk + GESTURE_CHAR_STEP) / gs.melody_hz(anchor, gs.walk);
            assert!(
                span > one * 1.05,
                "dir {dir}: a word motion must travel further than a character one \
                 ({span} vs {one})"
            );
        }
    }

    #[test]
    fn the_clouds_puff_survives_the_bell_it_follows() {
        let mut s = TrailSynth::new(48_000.0, 1);
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Backspace));
        let after_bell = s.live_voices();
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Poof));
        assert!(
            s.live_voices() > after_bell,
            "the puff must speak behind the bell it accompanies"
        );

        let mut s = TrailSynth::new(48_000.0, 1);
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Poof));
        let after_puff = s.live_voices();
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Backspace));
        assert!(
            s.live_voices() > after_puff,
            "the puff is an accompaniment: it must not thin the key that follows"
        );

        let mut s = TrailSynth::new(48_000.0, 0x9A11);
        let walk = s.walk;
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Poof));
        assert_eq!(s.walk, walk, "a puff of air does not compose the melody");
    }

    #[test]
    fn a_held_delete_run_shimmers_once_on_release() {
        let pending = |s: &TrailSynth| {
            s.voices
                .iter()
                .filter(|v| v.on && v.shimmer && v.t < 0.0 && v.damp <= 0.0)
                .count()
        };
        let mut s = TrailSynth::new(48_000.0, 0x51D3_0001);
        let mut buf = vec![0.0f32; 4_800 * CHANNELS];
        for i in 0..8u32 {
            let mut e = ev(GlowStyle::RainbowKitty, SoundKind::Backspace);
            e.bed = false;
            s.push(e);
            let want = usize::from(i + 1 >= HELD_ERASE_RUN_MIN);
            assert_eq!(
                pending(&s),
                want,
                "after {} admitted deletions the run holds exactly {want} \
                 pending shimmer(s)",
                i + 1
            );
            s.render(&mut buf);
        }
        assert_eq!(pending(&s), 1, "the last booking survives the release");
        s.render(&mut buf);
        let sounding: Vec<&Voice> = s
            .voices
            .iter()
            .filter(|v| v.on && v.shimmer && v.t >= 0.0)
            .collect();
        assert_eq!(sounding.len(), 1, "exactly one shimmer reaches the ear");
        assert!(
            sounding[0].damp <= 0.0,
            "…and nothing damped the one that speaks"
        );

        let mut s = TrailSynth::new(48_000.0, 0x51D3_0002);
        for _ in 0..(HELD_ERASE_RUN_MIN - 1) {
            let mut e = ev(GlowStyle::RainbowKitty, SoundKind::Backspace);
            e.bed = false;
            s.push(e);
            s.render(&mut buf);
        }
        assert_eq!(pending(&s), 0, "a short run is not a held run");

        let mut s = TrailSynth::new(48_000.0, 0x51D3_0003);
        for _ in 0..HELD_ERASE_RUN_MIN {
            let mut e = ev(GlowStyle::RainbowKitty, SoundKind::Backspace);
            e.bed = false;
            s.push(e);
            s.render(&mut buf);
        }
        assert_eq!(pending(&s), 1, "fixture: the hold booked its shimmer");
        let mut t = ev(GlowStyle::RainbowKitty, SoundKind::Typed);
        t.bed = false;
        s.push(t);
        assert_eq!(
            pending(&s),
            0,
            "a keystroke inside the pre-delay expires the booking"
        );
        s.render(&mut buf);
        s.render(&mut buf);
        let mut e = ev(GlowStyle::RainbowKitty, SoundKind::Backspace);
        e.bed = false;
        s.push(e);
        assert_eq!(pending(&s), 0, "a lone correction is not a held run");
    }

    #[test]
    fn cursor_motions_never_step_the_song_and_only_the_sweep_bypasses_the_gap() {
        for (kind, bypasses) in [
            (SoundKind::Glide { dir: 1 }, false),
            (SoundKind::Sweep { dir: -1 }, true),
        ] {
            let mut s = TrailSynth::new(48_000.0, 0x6117);
            let walk = s.walk;
            s.push(ev(GlowStyle::Lumen, kind));
            assert!(s.live_voices() > 0, "{kind:?} speaks");
            assert_eq!(s.walk, walk, "{kind:?} must not step the phrase melody");

            let mut a = TrailSynth::new(48_000.0, 1);
            a.push(ev(GlowStyle::Lumen, SoundKind::Typed));
            let before = a.live_voices();
            a.push(ev(GlowStyle::Lumen, kind));
            assert_eq!(
                a.live_voices() > before,
                bypasses,
                "{kind:?} inside the min-gap: only the sweep bypasses thinning"
            );
        }
    }

    #[test]
    fn sin_table_stays_below_one_16bit_step() {
        const ONE_16BIT_STEP: f32 = 1.0 / 32768.0;
        let mut worst = 0.0f32;
        let mut worst_ph = 0.0f32;
        for i in 0..400_000u32 {
            let base = i as f32 / 400_000.0;
            for ph in [base, base - 1.0, base + 1.0, base + 7.5] {
                let exact = ((ph - ph.floor()) * core::f32::consts::TAU).sin();
                let d = (sin01(ph) - exact).abs();
                if d > worst {
                    worst = d;
                    worst_ph = ph;
                }
            }
        }
        assert!(
            worst < ONE_16BIT_STEP,
            "sin01's table drifted {worst:e} from libm at ph={worst_ph} — \
             {:.3} of a 16-bit step, so it can now move an output sample",
            worst / ONE_16BIT_STEP
        );
    }
}
