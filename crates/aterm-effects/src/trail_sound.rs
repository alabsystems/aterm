// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trail sound: the aural half of the cursor effects. A pure, clockless,
//! allocation-free procedural synthesizer that gives every [`GlowStyle`] its
//! own sound palette, plus a namespaced gesture vocabulary other effects speak
//! through the same engine.
//!
//! - **Pure and host-agnostic.** No device I/O, clock reads or allocation after
//!   construction. The host pushes [`SoundEvent`]s and pulls interleaved stereo
//!   `f32` frames via [`TrailSynth::render`]. Time advances only by samples
//!   rendered, so output is deterministic for a seed and an event script.
//! - **Quiet by construction.** Soft attacks, a just-intonation pentatonic
//!   lattice (overlapping notes never beat harshly), short decays, a rate
//!   governor, and a per-style bed that fast typing melts into.
//! - **Silence is exact.** When every voice is dead and the bed has decayed,
//!   [`TrailSynth::is_quiet`] is `true` and [`render`](TrailSynth::render)
//!   writes zeros, so the host can pause its output queue.
//!
//! [`SoundGesture`] pairs a source effect with its vocabulary; a new effect adds
//! a variant and inherits the governor, beds, voice pool and host queue. Each
//! style's voice design and bed live on one [`Palette`] implementor registered
//! in [`palette_for`]. Style-agnostic gestures (the Kill swoosh, the Bonk,
//! cursor motions) are designed once before palette dispatch. Typing, deletion
//! and cursor motion are one voice moved along direction and scale
//! ([`gesture_shape`], [`gesture_bend`]); palettes state timbre, not intervals.
//!
//! The host owns policy: focus and reduced-motion gating, the `trail_sounds`
//! toggle and `trail_sound_volume`, per-source enables (`bonk`), and the
//! platform output queue.

mod rainbow_kitty_v2;

pub use rainbow_kitty_v2::{MelodyV2, RainbowKittyV2Palette, TimbreStops};

use rainbow_kitty_v2::{
    LANE_AGE_GUARD_S, LANE_FADE_STEAL_S, LANE_NONE, lane_cap, lane_drops_the_newcomer,
};

use crate::cursor_glow::GlowStyle;
use crate::tone::Tone;

/// Interleaved output channel count. Stereo: voices pan with the cursor's
/// column.
pub const CHANNELS: usize = 2;

/// Fixed polyphony, above what the governor admits in practice; when
/// exhausted the quietest voice is stolen, never the newest.
const MAX_VOICES: usize = 28;

/// What the cursor did: the cursor-trail gesture vocabulary, matching the
/// glow engine's `spawn` classification plus the adjacent-vs-jump split
/// (`dist <= 1.0`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SoundKind {
    /// A glyph was typed and the cursor stepped one cell: each style's
    /// signature key sound.
    Typed,
    /// Backspace / deletion: the erase poof, a soft unpitched cloud of air
    /// above the typing register. Style-agnostic and terminal before palette
    /// dispatch ([`TrailSynth::design_erase_poof`]): two band-passed noise
    /// bursts, no tonal partial (the `POOF_*` constants). Does not step the
    /// song, and rides its own gate ([`ERASE_MIN_GAP`]) so the first correction
    /// after a letter always speaks and a held Backspace cannot machine-gun.
    Backspace,
    /// Cursor navigation (arrows, clicks) — the Typed gesture at a whisper:
    /// same timbre family, much quieter and shorter.
    Navigation,
    /// A line-scale kill (^K/^U) that moved the cursor: a soft downward
    /// swoosh, a whole clause leaving at once.
    Kill,
    /// A word-scale kill (^W, Alt-D, Alt/Ctrl-Backspace): a slightly larger,
    /// softer cousin of the [`SoundKind::Backspace`] poof (the `WORD_POOF_*`
    /// constants). Terminal before palette dispatch; does not step the song.
    KillWord,
    /// The dispersal of the smoke puff a plain Backspace spawns
    /// (`cursor_glow`'s `PoofVoice::Puff`, cued on the same edge, so sound and
    /// smoke share `cursor_glow`'s `POOF_MIN_GAP` rate limit). The smallest
    /// sound in the vocabulary: a ~110 ms breath of air-band noise far under
    /// the keystroke floor ([`POOF_KIND_GAIN`], the `POOF_CLOUD_*` constants).
    /// It bypasses the min-gap governor like [`SoundKind::Land`], does not
    /// claim the beat, and does not step the melody.
    Poof,
    /// A multi-cell jump (Enter to a prompt, tab-complete, redraws): the
    /// style's flourish. Jumps bypass min-gap thinning, so rapid line feeds
    /// overlap into a "brrrring".
    Jump,
    /// A single-cell arrow move, the character-scale motion: one soft in-key
    /// melody tone one lattice step in the travel direction
    /// ([`gesture_shape`]). `dir` is +1 for right/up, -1 for left/down.
    /// Gap-thinned like a keystroke and softer than one.
    Glide { dir: i8 },
    /// A fast cursor run (a held arrow's coalesced echo or a multi-cell leap):
    /// the word-scale motion, a [`Glide`] repeated per character crossed and
    /// landing [`GESTURE_WORD_CHARS`] steps out ([`gesture_shape`]). One event
    /// carries the whole run as delayed voices, so it bypasses min-gap like
    /// [`Jump`]; the first note has `delay = 0`.
    Sweep { dir: i8 },
    /// The cursor landed: a bright in-key star chime over a soft arrival body,
    /// designed before palette dispatch and pitched through `melody_hz`.
    /// Bypasses the min-gap governor like [`SoundKind::Jump`] and does not step
    /// the phrase melody. No production path cues it; the benches and pins do.
    Land,
    /// The spacebar: the downbeat of the typing music. The host cues it for a
    /// bare Space; a space inside a committed IME run stays [`Typed`].
    ///
    /// Style-agnostic ([`TrailSynth::design_space`]): a round, monophonic bass
    /// root per word, walking the palette's anchor in one fixed bass register
    /// ([`SPACE_BASS_LO_HZ`]). Only the first space of a whitespace run is a
    /// downbeat; the rest answer with air plus an indent step, a soft bell
    /// climbing one lattice degree per extra space ([`SPACE_STEP_LEVEL`]).
    /// Neither steps nor cadences the song. Gap-thinned like a keystroke.
    Space,
    /// A bare Shift keydown: the ting before a capital, cued once per physical
    /// press (never on auto-repeat, release or mid-chord).
    ///
    /// A struck glass bell one lattice step above the melody (rotating,
    /// [`SHIFT_ROTATION`]), folded into its own bright register
    /// ([`SHIFT_TING_LO_HZ`]) at the Space's tier ([`SHIFT_KIND_GAIN`]). It
    /// does not step the melody, and its admission does not claim the min-gap
    /// beat ([`TrailSynth::push`]), so it never thins the keystroke it
    /// announces.
    Shift,

    // -- Rainbow kitty v2 (`docs/design/RAINBOW-KITTY-V2.md` §16) ----------
    // `push` routes these (and the v1 kinds while the v2 seam is engaged) to
    // `push_v2` before the v1 admission chain, so other palettes never see them.
    /// The keydown pre-cue for a navigation meteor (§15.2), minted before any
    /// move is observed; off by default, since a stray cue on a no-op move
    /// would be heard. Spawns only the tick and a provisional core, both with
    /// [`Voice::arm_ttl`]; an echo that never comes damps them over 12 ms.
    ///
    /// `dir` is the host's sign convention (+1 right/up), as
    /// [`crate::rainbow_kitty::Dir::sign`] mints it.
    MeteorArm {
        /// +1 rightward / upward, −1 leftward / downward.
        dir: i8,
    },
    /// A navigation move of at least
    /// [`crate::rainbow_kitty::timing::JUMP_MIN_CELLS`] cells (§12): tick,
    /// gliding core, whoosh, and the bell on the landing frame,
    /// `flight_ms(cells)` later.
    ///
    /// `cells` is the diagonal distance the pixels fly, so the synth derives
    /// the arrival edge from the glow's own function. A shorter move degrades
    /// to the nav tick (§12.3).
    Meteor {
        /// +1 rightward / upward, −1 leftward / downward. The core carries
        /// direction (rising vs falling); the whoosh is blind.
        dir: i8,
        /// Diagonal cell distance, the input to
        /// [`crate::rainbow_kitty::timing::flight_ms`].
        cells: u16,
        /// A [`SoundKind::MeteorArm`] preceded this and its voices are still
        /// live: claim them (re-target pan, `f1` and decays) rather than
        /// respawning, so one move never sounds two gestures.
        armed: bool,
    },
    /// A keyed Return with credit (§11): the cadence, never the brrrring. A
    /// PTY-only line feed stays [`SoundKind::Jump`].
    ///
    /// `cells` is the flight distance of the down-left vector; the resolution,
    /// the tonic dyad and the faraway bell all fire at `flight_ms(cells)`.
    Enter {
        /// Diagonal cell distance of the Return's own flight.
        cells: u16,
    },
    /// A paste landed: the up-strum (§28), four tines of the music box's live
    /// chord swept upward at 0 / 22 / 44 / 66 ms. Cued only while the rainbow
    /// kitty engine owns the frame (`CursorGlow::cue_paste`), with the echo
    /// credit a keystroke banks, so the paste's echo stays silent. It moves no
    /// melody state, and every voice but the music box drops it.
    Strum,
    /// A hero star was born and the shared token bucket paid for it (§13):
    /// one glint. Grain births and erase-born stars never mint this.
    Stardust {
        /// The star's seeded scintillation rate in Hz, integer-stepped in
        /// `[8, 12]`, so the glint twinkles with the star it rides. `0` means
        /// the default 10 Hz.
        twinkle_hz: u8,
    },
}

/// The sparkle-words gesture vocabulary (word-decoration events).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WordGesture {
    /// The curse-word bonk: the one discordant voice in the engine, a minor
    /// second plus tritone against the melody's current degree. Bypasses the
    /// governor gap like [`SoundKind::Jump`], ducks the melody and bed around
    /// it, and feeds the bed nothing.
    Bonk,
}

/// The sing-along celebration gesture vocabulary (`crate::kitty_sing`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CelebrationGesture {
    /// One bar of the sing-along riff, pushed once per visual bar boundary
    /// while the celebration is armed (wind-down pushes none); `bar` counts
    /// bars since the arm. The bar is scheduled as pre-delayed voices, so
    /// timing is sample-based ([`CELEBRATION_BAR_SECONDS`] equals
    /// `kitty_sing::SING_BAR_SECONDS`). `bar` is `u16` and
    /// `CELEBRATION_PHRASE_BARS` a power of two, so the form survives the wrap.
    ///
    /// `sig` is the held character's song signature
    /// (`kitty_sing::KittySing::signature`). Every per-key axis is a pure
    /// function of it, so the synth keeps no per-key state and a mid-hold key
    /// change takes effect on the next bar.
    RiffBar { bar: u16, sig: u32 },
    /// The outro (§27): pushed once on the armed run's last bar line. The
    /// pulse lead on *do*, its sub and the faraway ice bell,
    /// [`CELEBRATION_OUTRO_S`] long, so the song ends on the downbeat. `sig`
    /// is the run's signature. A human's release keeps the plain sing-duck
    /// crossfade.
    Outro { sig: u32 },
}

impl CelebrationGesture {
    /// The canonical riff-bar gesture: bar index plus the held character's
    /// song signature.
    #[must_use]
    pub fn riff_bar(bar: u16, sig: u32) -> Self {
        Self::RiffBar { bar, sig }
    }

    /// Build the outro gesture for a run in `sig`'s key — the stable
    /// constructor, like [`Self::riff_bar`].
    #[must_use]
    pub fn outro(sig: u32) -> Self {
        Self::Outro { sig }
    }
}

/// The prism wake gesture vocabulary: the answer to program output
/// (`crate::output_streak`). An episode produces at most one of each gesture;
/// output repeats far faster than typing, so this is the quietest and
/// sparsest vocabulary (see [`OUTPUT_PIP_KIND_GAIN`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputGesture {
    /// The episode's one soft pip, on the quiet→active crossing: two sine
    /// partials on the tune's own pentatonic lattice, lowpassed to a whisper.
    Shimmer,
    /// The episode's closing exhale — the same voice, one partial, quieter
    /// still. Sound as information (the output stopped), not repetition.
    Settle,
    /// The verdict: a command's exit code, sounded once on the OSC 133/633
    /// `D` that carries it. It replaces [`Self::Settle`] when both would speak;
    /// the host hushes the exhale for that episode.
    ///
    /// Voiced only by the music box ([`TrailSynth::v2_verdict`]); every other
    /// instrument is silent for it.
    Verdict {
        /// The exit status was non-zero.
        failed: bool,
        /// It ran long enough for the faraway ice bell (`CAD_BELL_*`).
        ice: bool,
    },
}

/// One namespaced gesture: which effect spoke (the variant) and what it said
/// (the payload). Shared shaping dispatches on the pair, so vocabularies
/// never collide.
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

/// Which palette family speaks for an event: the host's `trail_sound_style`
/// setting, carried per event like `gain` so a change applies on the next
/// keystroke. [`SoundVoice::Style`] follows [`SoundEvent::style`]; every other
/// voice overrides the style for the palette-designed gestures (Typed,
/// Navigation, Jump, the Kill swoosh's tint, the bed), while the
/// style-agnostic gestures keep their shared design and borrow the palette's
/// register. The roster is the "Typing sound" picker, in picker order;
/// [`SoundVoice::name`] spells each value once and [`SoundVoice::parse`]
/// accepts those spellings plus [`SoundVoice::ALIASES`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SoundVoice {
    /// Follow the visual trail style's palette.
    #[default]
    Style,
    /// One of the eight style palettes as a standalone instrument, chosen
    /// regardless of the look on screen. An alias of the `Style` table:
    /// `palette_for(Of(s), _) == palette_for(Style, s)`. `Of(RainbowKitty)` is
    /// never produced by [`SoundVoice::parse`] or listed in
    /// [`SoundVoice::ALL`]; built by hand, it takes the music box fork
    /// ([`TrailSynth::push_v2`]).
    Of(GlowStyle),
    /// The mechanical-keyboard palette: click + thock percussion.
    Mech,
    /// The typewriter: a dry slug clack over the platen thud; on Enter the
    /// margin bell dings and the carriage zips home. Machinery, not music.
    Typewriter,
    /// The marimba: a warm rosewood bar under a yarn mallet, its two-octave
    /// glint dying first. The roster's low-mid soft-transient voice.
    Marimba,
    /// The felt piano: a felt-muted hammer thud under a dark harmonic tone —
    /// the hush you can type on all day. The roster's lowest, darkest voice.
    Felt,
    /// The music box: the rainbow kitty instrument
    /// (`docs/design/RAINBOW-KITTY-V2.md` §9-§13), the only voice whose events
    /// bypass the v1 admission chain ([`TrailSynth::push_v2`]). A
    /// [`SoundVoice::Style`] event under `GlowStyle::RainbowKitty` is also the
    /// music box ([`TrailSynth::v2_engaged`]). The voice-agnostic pins cover it
    /// except the admission-chain ones (flood duck, `MIN_GAP` thinning) its IOI
    /// arc and lanes replace; its own laws live in `rainbow_kitty_v2`'s tests.
    RainbowKittyV2,
}

impl SoundVoice {
    /// The roster, in picker order: `auto` (the default), the music box, the
    /// eight style palettes, the keyboard, then the sound-only instruments.
    /// `Of(GlowStyle::RainbowKitty)` (listed as the music box) is absent.
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

    /// Alias spellings accepted by [`SoundVoice::parse`] (config files,
    /// `settings set`) but not offered by the picker, e.g. the trail-style
    /// names (`trail_sound_style = "water"`) and `mechanical` variants.
    pub const ALIASES: &[(&str, SoundVoice)] = &[
        ("style", SoundVoice::Style),
        ("follow", SoundVoice::Style),
        ("default", SoundVoice::Style),
        // The music box: the theme's names, plus `glass bell` so saved
        // configs land on the one rainbow instrument.
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
        // The laser trail's Settings name, so the trail-style spelling works
        // here too.
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

    /// The canonical spelling of each voice: the config value, the picker
    /// row and the introspection token. Lowercase with spaces, the
    /// `cursor_trail_style` convention. `Of(RainbowKitty)` answers as the
    /// music box.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            SoundVoice::Style => "auto",
            SoundVoice::Of(GlowStyle::RainbowKitty) | SoundVoice::RainbowKittyV2 => "music box",
            SoundVoice::Of(GlowStyle::Lumen) => "warm pluck",
            SoundVoice::Of(GlowStyle::Sparkle) => "glitter",
            SoundVoice::Of(GlowStyle::Comet) => "ice chime",
            SoundVoice::Of(GlowStyle::Water) => "droplet",
            // The classic wake rides the phaser voice (it is the phaser look);
            // it is not a separate `SoundVoice::ALL` entry.
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

    /// Resolve a spelling (canonical [`SoundVoice::name`] or a
    /// [`SoundVoice::ALIASES`] entry, trimmed, ASCII-case-insensitive) to its
    /// voice; `None` otherwise. Never yields `Of(RainbowKitty)`.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        let token = token.trim();
        Self::ALL
            .iter()
            .copied()
            .find(|v| token.eq_ignore_ascii_case(v.name()))
            .or_else(|| {
                Self::ALIASES
                    .iter()
                    .find(|(alias, _)| token.eq_ignore_ascii_case(alias))
                    .map(|&(_, voice)| voice)
            })
    }
}

/// One sound trigger, built by the host at the trail-spawn edge (or the
/// word-decoration drain) with the resolved user gain (motion-gated intensity
/// × `trail_sound_volume` × per-source enables).
#[derive(Clone, Copy, Debug)]
pub struct SoundEvent {
    pub style: GlowStyle,
    /// Which palette family speaks — see [`SoundVoice`]; `Style` is the
    /// pre-override identity.
    pub voice: SoundVoice,
    pub kind: SoundGesture,
    /// Stereo position, -1 (hard left) .. 1 (hard right), mapped from the
    /// cursor column; the synth narrows it so nothing sits fully in one ear.
    pub pan: f32,
    /// Typing heat 0..1 (the glow engine's blaze). Warms brightness and level
    /// a little; it must never be the difference between silent and loud.
    pub heat: f32,
    /// Style hue 0..1 where meaningful (phaser's live sweep hue) — mapped to
    /// melodic degree so the band's colour and pitch travel together.
    pub hue: f32,
    /// Final linear gain for this event, 0..1: user volume × any host-side
    /// gating. Zero is filtered out by [`TrailSynth::push`].
    pub gain: f32,
    /// The host's inferred [`Tone`] of the typed line (the [`crate::tone`]
    /// classifier). Trail gestures steer the melody with it (scale table,
    /// transpose, walk bias, note length; see [`tone_tables`]);
    /// [`Tone::Technical`] is the identity, and the Bonk ignores it.
    pub tone: Tone,
    /// Whether this gesture may feed the ambient bed (the host's
    /// `trail_sound_bed` setting, default on). With every event carrying
    /// `false` the bed is never energised and contributes exactly zero
    /// samples via its level-floor early-out; discrete notes are untouched.
    pub bed: bool,
    /// The glyph was shifted (a capital or shifted symbol): lifts this
    /// keystroke by [`SHIFT_GLYPH_LIFT`]. Distinct from [`SoundKind::Shift`],
    /// the bare modifier's own ting. Only the host sets it, from the key
    /// event's modifiers; echo-born cues and every non-`Typed` gesture carry
    /// `false`.
    pub shifted: bool,
}

/// Side-car scalars for the music box that [`SoundEvent`] does not carry
/// (`docs/design/RAINBOW-KITTY-V2.md` §16). A side-car with a [`Default`] keeps
/// the many struct-literal [`SoundEvent`] call sites unchanged;
/// [`TrailSynth::push`] is `push_meta(ev, EventMeta::default())`. Each default
/// is the field's "unknown".
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EventMeta {
    /// The host input-clock stamp in milliseconds, on both the keyed and the
    /// observed-echo paths: the melody's one time source (the inter-key gap
    /// shapes each note). The input clock rather than audio-thread arrival, so
    /// two cues delivered in one buffer cannot reorder a phrase boundary. `0` =
    /// unknown, falling back to [`TrailSynth::clock_s`].
    pub at_ms: u32,
    /// What kind of glyph landed (§16): a [`glyph_class`] id, filled at the
    /// keyed seam only by [`typed_glyph_class`]. The ids are a wire format,
    /// only ever appended; `v2_typed` reads it for the class voices (§10.4) and
    /// the punctuation phrasing. Eight classes name one character and the rest
    /// are buckets, every letter is `0`, and [`Self::rank`] folds case, so the
    /// stream cannot reconstruct prose or a password. ASCII-only; an echo-born
    /// cue carries `0`.
    pub glyph_class: u8,
    /// The typed glyph's alphabet rank (§3.1), filled at the keyed seam only by
    /// [`typed_glyph_rank`]: the derived melody's one input about what was
    /// written. Ordered and lossy: `0` = no key (every echo-born cue), `1..=26`
    /// a letter with case folded, `27..=36` a digit, `37..=44` punctuation in
    /// eight buckets, `45` anything else printable.
    pub rank: u8,
    /// The pan this gesture starts at, −1..1: the meteor's origin column,
    /// where [`SoundEvent::pan`] is its destination. `pan_from == pan` means
    /// the gesture does not travel.
    pub pan_from: f32,
    /// How far into the current audio block this cue arrived, in seconds.
    /// [`TrailSynth::spawn_seeded`] delays every voice of the cue by it, so
    /// each voice sounds a constant one block after arrival instead of
    /// jittering to the next block boundary. The host clamps it to one block;
    /// `0.0` is the identity.
    pub block_lead_s: f32,
    /// Flow heat, 0..1: how far into flow state the typist is, from the glow
    /// engine's flow counter (fast keys with no delete; a delete, a Kill or a
    /// slow key resets it). Unlike [`SoundEvent::heat`] it only climbs on a
    /// clean fast run; the music box reads only this. At `0.0` every
    /// flow-priced quantity is its base constant (`FLOW_BASS_DECAY_S`,
    /// `FLOW_P2_LVL`), so unstamped callers and other palettes are unaffected.
    pub flow: f32,
}

/// The engine's ceiling on [`EventMeta::block_lead_s`] (five 10.667 ms
/// blocks). The host clamps to one block; this only keeps a mis-stamped
/// side-car from scheduling a note into the far future.
const BLOCK_LEAD_MAX_S: f32 = 0.050;

/// [`EventMeta::rank`] for a typed character: the one producer of the
/// melody's alphabet order. `None` (an IME run, a bare modifier, an echo-born
/// cue) is `0`; punctuation buckets follow the keyboard's groups; any other
/// printable glyph is `45`, one rank above the last bucket.
#[must_use]
#[inline]
pub fn typed_glyph_rank(typed: Option<char>) -> u8 {
    let Some(c) = typed else { return 0 };
    if c.is_ascii_alphabetic() {
        // Case folded: the capital has its own voice in `SoundEvent::shifted`.
        return 1 + (c.to_ascii_lowercase() as u8 - b'a');
    }
    if c.is_ascii_digit() {
        return 27 + (c as u8 - b'0');
    }
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
}

/// Names for [`EventMeta::glyph_class`], produced by [`typed_glyph_class`]. The
/// values are a `u8` wire format (`0` = letter/unknown, ids only appended), so
/// they are constants rather than an enum and an unknown value lands on the
/// letter path.
pub mod glyph_class {
    /// Letters, the space, an IME run, anything non-ASCII, and "no key".
    pub const LETTER: u8 = 0;
    /// `?` — the question that rises.
    pub const QMARK: u8 = 1;
    /// `!`: struck forte as a tine, landing on a C or a G.
    pub const BANG: u8 = 2;
    /// `0`..`9` — the wood bar that counts.
    pub const DIGIT: u8 = 3;
    /// `.`: the full stop. A dot that ends something (the first of its
    /// space-delimited token, not behind a digit) is steered to a C and rings
    /// as a tine with a breath behind it; every other dot (`foo.bar`, `3.14`,
    /// `..`) is a bare pluck with no breath.
    pub const STOP: u8 = 4;
    /// `(` `[` `{` — the brackets that open (not a pluck: they open).
    pub const OPEN: u8 = 5;
    /// `)` `]` `}` — the brackets that close.
    pub const CLOSE: u8 = 6;
    /// `'` `"` `` ` `` — the quotes.
    pub const QUOTE: u8 = 7;
    /// `_` `~` `\` `|`: a line drawn.
    pub const LINE: u8 = 8;
    /// `/` — the slash that rises.
    pub const RISE: u8 = 9;
    /// `+` `=` `*` `%` `^` `<` `>` — the operators; `<` `>` are comparisons
    /// here, not an open/close pair.
    pub const MATH: u8 = 10;
    /// `@` `#` `$` `&` — every other ASCII mark.
    pub const SIGIL: u8 = 11;
    /// `,`: a half cadence. The token's first comma lands on a D or a G, a
    /// pluck 2 dB under a letter with a short breath; any later one is a bare
    /// pluck.
    pub const COMMA: u8 = 12;
    /// `;` — the semicolon. Appended with [`COMMA`], and shaped as it is;
    /// a deceptive turn, onto an E or an A.
    pub const SEMI: u8 = 13;
    /// `:` — the colon. Appended with [`COMMA`], and shaped as it is; an
    /// announcement, onto a G with its fifth opening under it.
    pub const COLON: u8 = 14;
    /// `-` — the dash. Appended with [`COMMA`]. A tie: it sounds the note
    /// before it, held a little longer than a letter.
    pub const DASH: u8 = 15;
    /// A key typed into a password prompt, not a glyph:
    /// [`super::typed_glyph_class`] never returns it. The host stamps it on a
    /// keyed cue the tty will not echo (`SinkWriter::tty_swallows_input`), with
    /// `rank = 0`, `shifted = false` and kind `Typed` even for the spacebar.
    /// The music box sounds every such cue as one fixed tine (`v2_secret`) and
    /// moves no melody state; the v1 palettes treat it as a rank-0 letter,
    /// which already says nothing per key.
    pub const SECRET: u8 = 16;
}

/// [`EventMeta::glyph_class`] for a typed character: the one producer of the
/// [`glyph_class`] table, shared by the host seam, the bench and the tests.
///
/// `None` (an IME run, a bare modifier, an echo-born cue), letters, the space
/// and non-ASCII glyphs are `LETTER`. The buckets are the keyboard's groups,
/// total over ASCII punctuation, and lossy by design.
#[must_use]
#[inline]
pub fn typed_glyph_class(typed: Option<char>) -> u8 {
    use glyph_class::*;
    match typed {
        None => LETTER,
        Some('?') => QMARK,
        Some('!') => BANG,
        Some(c) if c.is_ascii_digit() => DIGIT,
        Some('.') => STOP,
        Some(',') => COMMA,
        Some(';') => SEMI,
        Some(':') => COLON,
        Some('(' | '[' | '{') => OPEN,
        Some(')' | ']' | '}') => CLOSE,
        Some('\'' | '"' | '`') => QUOTE,
        Some('-') => DASH,
        Some('_' | '~' | '\\' | '|') => LINE,
        Some('/') => RISE,
        Some('+' | '=' | '*' | '%' | '^' | '<' | '>') => MATH,
        // `@ # $ &` — the four ASCII marks no bucket above names.
        Some(c) if c.is_ascii_punctuation() => SIGIL,
        // Letters, the space, non-ASCII: the letter path.
        Some(_) => LETTER,
    }
}

// ---------------------------------------------------------------------------
// Musical foundation
// ---------------------------------------------------------------------------

/// Just-intonation major pentatonic (1 9/8 5/4 3/2 5/3). Every interval is
/// consonant against every other, so overlapping voices stay harmonious;
/// equal temperament would beat audibly on the thirds.
const PENTA: [f32; 5] = [1.0, 1.125, 1.25, 1.5, 1.666_666_7];

/// `base` scaled to pentatonic `degree` (octaves wrap; negative degrees fall
/// below `base`).
#[inline]
fn penta(base: f32, degree: i32) -> f32 {
    let oct = degree.div_euclid(5);
    let step = degree.rem_euclid(5) as usize;
    base * PENTA[step] * (2.0_f32).powi(oct)
}

/// Frustrated minor pentatonic (1 6/5 4/3 3/2 9/5), just intonation. Every
/// pairwise interval stays outside the minor-second and tritone bands
/// (`tone_tables_are_mutually_consonant_and_exclude_the_bonk_clash`).
const PENTA_MINOR: [f32; 5] = [1.0, 1.2, 1.333_333_3, 1.5, 1.8];

/// Playful suspended pentatonic (1 9/8 4/3 3/2 5/3, the Japanese *yo* scale):
/// major penta with the third lifted to a fourth. A lydian ♯4 would be the
/// bonk's tritone (45/32), so suspension stands in for it.
const PENTA_YO: [f32; 5] = [1.0, 1.125, 1.333_333_3, 1.5, 1.666_666_7];

/// Excited transpose: one just whole tone (9/8) up, applied through
/// [`TrailSynth::melody_hz`]. Intervals are unchanged, so consonance holds.
const EXCITED_TRANSPOSE: f32 = 1.125;

/// Per-tone melodic material: (scale table, transpose ratio).
/// - Technical / Calm: major penta, untransposed. Technical is the full
///   identity; Calm also breathes longer and steps more gently.
/// - Excited: major penta a just whole tone up.
/// - Frustrated: minor penta (the walk bias adds a downward lean).
/// - Playful: the suspended *yo* penta (the walk takes wider steps).
///
/// No table contains the bonk's minor-second/tritone intervals.
#[inline]
fn tone_tables(tone: Tone) -> (&'static [f32; 5], f32) {
    match tone {
        Tone::Technical | Tone::Calm => (&PENTA, 1.0),
        Tone::Excited => (&PENTA, EXCITED_TRANSPOSE),
        Tone::Frustrated => (&PENTA_MINOR, 1.0),
        Tone::Playful => (&PENTA_YO, 1.0),
    }
}

/// Per-tone tempo feel: one multiplier on note length, decay and flourish
/// spacing, applied at spawn. Narrow (±12 %) so the melody leans rather than
/// lurches. Technical (the identity) and Frustrated are exactly 1.0. Bonk
/// voices are exempt (spawn skips `duck_exempt` prototypes).
#[inline]
fn tone_feel(tone: Tone) -> f32 {
    match tone {
        Tone::Technical | Tone::Frustrated => 1.0,
        Tone::Calm => 1.06,
        Tone::Excited => 0.88,
        Tone::Playful => 0.94,
    }
}

// ---------------------------------------------------------------------------
// The pulse: what makes the typed line a song rather than a stream of notes
// ---------------------------------------------------------------------------
//
// A keystroke is a position in a fixed bar, indexed by keystroke rather than
// clock (typing speed is the tempo):
//
//   * an accent slot sings: the theme steps and the keystroke plays its new
//     degree at full level;
//   * a ghost slot accompanies: the melody does not move, and the keystroke
//     plays a fixed consonant offset from the current degree, quieter
//     ([`SONG_GHOST_LEVEL`]) and shorter ([`SONG_GHOST_FEEL`]).
//
// The melody moves at a third of typing speed (~3.3 notes/s at 10 cps) while
// every keystroke still sounds (a silent key reads as a dropped keystroke).
/// The sentinel for an accent slot. `i8::MIN` cannot collide with a real
/// ghost offset (they live in ±4 degrees).
const SONG_ACCENT: i8 = i8::MIN;

/// The bar: [`SONG_ACCENT`] where the melody sings, elsewhere the ghost's
/// offset in scale degrees from the melody's current note. Four accents in
/// twelve; the ghosts spell a broken chord under each melody note (the octave
/// below, then a fourth below alternating with a third above). Whole scale
/// degrees, so a ghost never rubs against its note; the octave, not a unison,
/// since a second voice at the accent's frequency and a random phase would
/// comb-filter.
///
/// Registers: space downbeat 220-440 Hz, ghost arpeggio ~260-480 Hz, melody
/// ~520-950 Hz, erase poof 2.8-5 kHz.
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

/// A ghost's level against its accent (−5.2 dB): quiet enough to read as
/// accompaniment, loud enough never to read as a key that failed to fire.
const SONG_GHOST_LEVEL: f32 = 0.55;
/// A ghost's length against an accent's, as a `dur`/`decay`/`delay`
/// multiplier (the one the per-tone feel rides). At 10 cps a ghost is ~84 ms
/// and clears the next keystroke, so the accents' pitch is not masked.
const SONG_GHOST_FEEL: f32 = 0.62;

/// A typing gap (seconds) longer than this ends the current phrase. Enter (a
/// [`SoundKind::Jump`]) ends one too. Matches the ~0.6 s the governor's rate
/// estimate decays over.
const PHRASE_PAUSE_S: f32 = 0.6;

// ---------------------------------------------------------------------------
// The theme: one composed tune
// ---------------------------------------------------------------------------
//
// The accent lane plays a fixed theme, the held-key sing-along's verse
// ([`CELEBRATION_PHRASE`] bars 0..3), so typing and holding are two tempos of
// one piece: the hook A, its answer A' (landing on the tonic), the bridge B
// (bar 2 with its `REST`s dropped, since an accent lane has no sustain) and
// the half-cadence A'' (climbing to the 8th and falling back onto A, since
// typing has no final keystroke). 28 accents, ~8.4 s at 10 cps; the space
// walks the bass roots ([`SONG_BASS`]) on the word clock against it.
/// The tune, in scale degrees on the active [`crate::tone::Tone`]'s table:
/// [`CELEBRATION_PHRASE`] bars 0..3, with bars 2-3 de-syncopated.
const SONG_THEME: [i8; 28] = [
    // A — the hook (bar 0, verbatim).
    0, 2, 4, 5, 4, 2, 4, 7, //
    // A' — the answer (bar 1, verbatim).
    5, 7, 5, 4, 2, 4, 2, 0, //
    // B — the bridge (bar 2's pitches, de-syncopated).
    2, 4, 7, 5, 3, 2, //
    // A'' — the half-cadence (bar 3's pitches, de-syncopated); climbs highest
    // and leans back onto A.
    0, 2, 4, 5, 7, 8,
];

/// Where each phrase of [`SONG_THEME`] begins, plus the end sentinel.
///
/// A pause (a comma-length gap or an Enter) closes the current phrase and
/// the next keystroke opens the next one rather than restarting the theme,
/// so the piece always advances.
const SONG_FORM: [u8; 5] = [0, 8, 16, 22, 28];

/// The bass walk, one root per word, in degrees on the palette's anchor:
///
///   C E D G | C E A D | C E G A     (degrees 0 2 1 3 | 0 2 4 1 | 0 2 3 4)
///
/// - The first group is [`CELEBRATION_BASS`] bars 0..3, so the theme is
///   harmonised by its own progression.
/// - `C E` opens every group and the pair after it rotates.
/// - All five pentatonic roots appear, and no root recurs within four words
///   cyclically (`the_bass_walk_never_returns_inside_four_words`).
/// - Twelve words against the theme's 28 accents re-pair every 84 accents.
///
/// Folded by [`TrailSynth::space_root_hz`] into the audible [220, 440)
/// register (262–392 Hz at C5); a sub-220 Hz downbeat is not heard. Degrees
/// rather than Hz, so the walk rides every [`crate::tone::Tone`]'s table.
const SONG_BASS: [i8; 12] = [0, 2, 1, 3, 0, 2, 4, 1, 0, 2, 3, 4];

/// Per-tone melodic register `(lo, hi)` the theme is folded into.
///
/// Every register holds the whole theme (0..=8 shifted by [`melody_lean`]),
/// so the fold never rewrites a theme note
/// (`the_theme_never_folds_in_any_tone`); per-tone character lives in the
/// scale table, transpose and lean. The fold remains a total guard for the
/// bonk, the cursor family and the column nudge.
#[inline]
fn tone_register(tone: Tone) -> (i32, i32) {
    match tone {
        // The lean is +1: the theme occupies 1..=9.
        Tone::Excited => (0, 9),
        // The lean is −1: the theme occupies −1..=7.
        Tone::Frustrated => (-1, 8),
        _ => (0, 8),
    }
}

/// Fold a pitch register into `lo..=hi` by reflection rather than clamping.
///
/// Clamping would turn consecutive out-of-range notes into unisons; reflection
/// turns the excess into a step back down the lattice. Modular (a triangle
/// wave), so any input is handled in constant time.
#[inline]
fn fold_register(v: i32, lo: i32, hi: i32) -> i32 {
    let span = hi - lo;
    if span <= 0 {
        return lo;
    }
    let m = (v - lo).rem_euclid(2 * span);
    lo + if m <= span { m } else { 2 * span - m }
}

/// Per-tone lean added to the pitch register: Excited +1, Frustrated −1, the
/// rest 0.
#[inline]
fn melody_lean(tone: Tone) -> i32 {
    match tone {
        Tone::Excited => 1,
        Tone::Frustrated => -1,
        _ => 0,
    }
}

/// Default register for the style-agnostic cursor-movement notes
/// (Glide/Sweep), and the Kill/Bonk register. A palette with its own
/// [`Palette::anchor_hz`] moves the family into its register. The pitch is
/// drawn through `melody_hz` at the melody's current degree.
const CURSOR_ANCHOR_HZ: f32 = 330.0;

/// Spacing between the notes of a word-scale run, the first at `delay = 0`:
/// the run's own rate limit.
const CURSOR_SWEEP_STEP_S: f32 = 0.055;

// ---------------------------------------------------------------------------
// The gesture family: one rule for typing and the cursor motions
// ---------------------------------------------------------------------------
//
// Every family gesture renders a move on the text as the same move on the
// lattice: direction (`dir`, +1 forward, −1 backward) signs the pitch offset
// and the contour bend; scale (`notes`, `step`) is how much text it crosses.
//
//                  dir   notes   stride   lands at (from the melody degree)
//     Typed        +1      1       0      the degree itself
//     Shift        +1      1       0      +1 step, entered from below
//     Glide{dir}   dir     1       0      dir × 1 step
//     Sweep{dir}   dir     4     dir×1    dir × 3 steps
//
// Space and the deletions take the neutral shape (a fixed bass root,
// unpitched air). Encoded once in [`gesture_shape`] and [`gesture_bend`].

/// One character of travel, in lattice degrees.
const GESTURE_CHAR_STEP: i32 = 1;

/// A word gesture's size in characters. Three lattice steps is a pentatonic
/// sixth: the same move, plainly bigger.
const GESTURE_WORD_CHARS: i32 = 3;

/// A word-scale run is one note per character crossed, endpoints included.
const CURSOR_SWEEP_RUN: usize = GESTURE_WORD_CHARS as usize + 1;

/// The contour bend: one just whole tone (9/8). A voice enters one step from
/// its direction's side and settles onto its note over [`GESTURE_BEND_TAU`].
/// The ratio is [`PENTA`]'s own second degree, so the scoop passes through a
/// consonant interval, and is not the bonk's minor second (16/15).
const GESTURE_BEND: f32 = 1.125;
/// Bend time constant — short enough to read as articulation, not portamento
/// (a keystroke is ~135 ms, so the scoop is over inside the first tenth of it).
const GESTURE_BEND_TAU: f32 = 0.012;

/// The family geometry of one gesture (see the section comment). Kinds
/// outside the family take the neutral shape (forward, one note, no offset).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct GestureShape {
    /// +1 forward / creating, −1 undoing / backward.
    dir: i8,
    /// Notes in the gesture: 1 at character scale, [`CURSOR_SWEEP_RUN`] at
    /// word scale.
    notes: usize,
    /// Lattice degrees between consecutive notes (0 for a single note).
    step: i32,
    /// Lattice degrees from the melody's current degree to the first note.
    offset: i32,
}

/// The capital's lift for one event in scale degrees: [`SHIFT_GLYPH_LIFT`]
/// when a shifted glyph was typed, else zero. A function so palettes that
/// build their own degree (Phaser) apply the same rule.
#[inline]
fn shift_lift(ev: &SoundEvent, kind: SoundKind) -> i32 {
    if kind == SoundKind::Typed && ev.shifted {
        SHIFT_GLYPH_LIFT
    } else {
        0
    }
}

/// The capital's weight for one event: [`SHIFT_GLYPH_GAIN`] when a shifted
/// glyph was typed, exactly 1.0 otherwise.
#[inline]
fn shift_gain(ev: &SoundEvent, kind: SoundKind) -> f32 {
    if kind == SoundKind::Typed && ev.shifted {
        SHIFT_GLYPH_GAIN
    } else {
        1.0
    }
}

/// The capital's lift as a frequency ratio, for palettes whose pitch does not
/// come from the lattice (Fire: a noise snap over a random 85–125 Hz ember
/// knock). Derived from [`SHIFT_GLYPH_LIFT`] through the five-degree octave,
/// so ratio and degree agree.
#[inline]
fn shift_ratio(ev: &SoundEvent, kind: SoundKind) -> f32 {
    (2.0f32).powf(shift_lift(ev, kind) as f32 / PENTA.len() as f32)
}

/// The gesture family's rule (see the section comment): every family degree
/// in the engine comes from here.
#[inline]
fn gesture_shape(kind: SoundKind) -> GestureShape {
    match kind {
        // The lift before a capital: one step above the note it announces,
        // entered from below.
        SoundKind::Shift => GestureShape {
            dir: 1,
            notes: 1,
            step: 0,
            offset: GESTURE_CHAR_STEP,
        },
        // A character of travel, in the travel direction.
        SoundKind::Glide { dir } => GestureShape {
            dir,
            notes: 1,
            step: 0,
            offset: i32::from(dir) * GESTURE_CHAR_STEP,
        },
        // A word of travel: the character stride, walked out.
        SoundKind::Sweep { dir } => GestureShape {
            dir,
            notes: CURSOR_SWEEP_RUN,
            step: i32::from(dir) * GESTURE_CHAR_STEP,
            offset: 0,
        },
        // Forward, character-scale, on the degree itself. A deletion lands
        // here too, though it is unpitched and returns before palette dispatch.
        _ => GestureShape {
            dir: 1,
            notes: 1,
            step: 0,
            offset: 0,
        },
    }
}

/// Octave-fold `f` into `[lo, 2 × lo)`. Halving and doubling are exact in
/// binary floating point, so pitch class is preserved. The input is clamped,
/// so the loops are bounded.
#[inline]
fn fold_octave(f: f32, lo: f32) -> f32 {
    let mut f = f.clamp(20.0, 20_000.0);
    while f >= lo * 2.0 {
        f *= 0.5;
    }
    while f < lo {
        f *= 2.0;
    }
    f
}

/// A palette's anchor folded into the space's bass register
/// `[SPACE_BASS_LO_HZ, 2 × SPACE_BASS_LO_HZ)`.
#[inline]
fn bass_octave(anchor: f32) -> f32 {
    fold_octave(anchor, SPACE_BASS_LO_HZ)
}

/// The bend applied: `(f0, f1)` for a partial that settles onto `f` from
/// `dir`'s side. Forward enters from a step below, inverse from a step above.
#[inline]
fn gesture_bend(f: f32, dir: i8) -> (f32, f32) {
    if dir >= 0 {
        (f / GESTURE_BEND, f)
    } else {
        (f * GESTURE_BEND, f)
    }
}

/// The bonk's clash intervals in just intonation: the minor second (16/15)
/// rubs, the tritone (45/32) refuses to resolve. Neither exists in [`PENTA`]'s
/// lattice, so the bonk is never mistaken for a melody note.
const BONK_MINOR_SECOND: f32 = 16.0 / 15.0;
const BONK_TRITONE: f32 = 45.0 / 32.0;

// ---------------------------------------------------------------------------
// The loudness ladder
// ---------------------------------------------------------------------------
//
// Loudness is indexed by how often a gesture can repeat, not by how big it
// looks. Every per-character gesture sits on one floor; each rarer tier is
// ~+1.5 dB over the one below, ~7 dB in all.
//
// Figures are rendered peaks at the default `trail_sound_volume` (0.4), heat
// 0.5, one isolated event, median over the palettes. The governor scales the
// whole ladder by one scalar, so the intervals hold at any typing speed.
//
//   Tier 0   under      Glide, Sweep, Navigation    cursor motion
//   Tier 1  -21.0 dBFS  Typed, Space, Shift, Backspace   per character
//   Tier 3  -18.0 dBFS  Jump, Kill                  per line / command
//   Tier 4  -16.0 dBFS  Land                        rare spectacle
//   Tier 5  -14.0 dBFS  Bonk, the riff's peak bar   rarest punctuation
//
// Shift sits at the Space's gain; the
// v2 meteor arm keeps a whisper, [`METEOR_ARM_KIND_GAIN`]. The prism wake
// output pip sits under that ([`OUTPUT_PIP_KIND_GAIN`]): program output can
// repeat faster than any hand.
//
// Kind gains are the tier knob; `palette_trim` anchors each style's Typed on
// the floor.

/// Tier 1, the floor: the unit everything else is measured in.
const TYPED_KIND_GAIN: f32 = 1.0;
/// Tier 1, a shade under the floor: a correction should not out-shout the
/// typing it undoes, even when Backspace is held.
const BACKSPACE_KIND_GAIN: f32 = 0.85;
/// Tier 1, between the floor and the deletion: the downbeat repeats at typing
/// speed but grounds rather than states.
const SPACE_KIND_GAIN: f32 = 0.9;
/// Tier 1, at the Space's gain: the ting. A modifier steps nothing and claims
/// no beat, but it is an audible note.
const SHIFT_KIND_GAIN: f32 = 0.9;
/// The meteor arm's whisper, under the floor (a pre-cue, not a key). `push`
/// forks v2 kinds to `push_v2` before this table is read; the entry keeps the
/// ladder total.
const METEOR_ARM_KIND_GAIN: f32 = 0.5;
/// The capital's weight, +2.6 dB: the gain a shifted glyph
/// ([`SoundEvent::shifted`]) carries over its plain self, on top of
/// [`SHIFT_GLYPH_LIFT`]. It lifts a per-character gesture to about tier 3's
/// peak, the one deliberate break of the index-by-repeat-frequency rule.
///
/// Applied once, in [`shift_gain`], on `Typed` alone and after the kind gain,
/// so every palette inherits it through `g`. Two effects would eat it as the
/// octave lift walks the note up:
/// - each palette's `lp_cut` would roll the lifted note off, so the capital's
///   roof rises with it (`TrailSynth::shift_octave`, read in `spawn_seeded`);
/// - a detuned chorus twin (Comet ×1.006, Lumen ×1.004) beats at a random
///   phase and doubling the note would double the beat, so partials within
///   [`DETUNE_MAX_RATIO`] of another keep their beat in Hz.
const SHIFT_GLYPH_GAIN: f32 = 1.35;
/// A chorus twin's widest detune as a ratio, about half a semitone. Two
/// partials closer than this are one note beating, not two notes (the
/// lattice's smallest step is ×1.122). Read in `spawn_seeded` under the
/// capital's octave: the lift moves the note, not its shimmer.
const DETUNE_MAX_RATIO: f32 = 1.03;
/// Under the meteor arm, the quietest voice: program output can repeat faster
/// than a hand types. [`crate::output_streak`] already bounds it to one pip
/// per episode; this gain is an independent second guarantee.
const OUTPUT_PIP_KIND_GAIN: f32 = 0.34;
/// The episode's closing exhale, quieter again than the pip that opened it —
/// a release is never louder than its onset.
const OUTPUT_SETTLE_KIND_GAIN: f32 = 0.22;

/// The capital's lift in scale degrees: a shifted glyph
/// ([`SoundEvent::shifted`]) sings its note one octave up. Five degrees is a
/// pentatonic octave, so contour and consonance are untouched. Applied to the
/// degree in [`TrailSynth::design_trail`] for [`SoundKind::Typed`] alone; does
/// not move [`TrailSynth::walk`].
const SHIFT_GLYPH_LIFT: i32 = 5;

/// The bare Shift's rotation: scale degrees added to its base step
/// ([`GESTURE_CHAR_STEP`]), one entry per sounding [`SoundKind::Shift`],
/// cycling. A bare modifier steps neither the song nor the column, so without
/// it every Shift would play one pitch. The lifts step two degrees and fold at
/// the octave, giving offsets +1 +3 +5 +2 +4: every degree once per lap, each
/// on the active table. Pitch only; entry 0 is the unrotated step.
const SHIFT_ROTATION: [i32; 5] = [0, 2, 4, 1, 3];

// ---------------------------------------------------------------------------
// The ting: the bare Shift's voice
// ---------------------------------------------------------------------------
//
// A struck, bright, ringing bell in the house struck-glass shape: a sine body
// under a 3.01-ratio FM strike glint that dies in ~20 ms, a quiet octave
// partial, a 5 ms mallet out of 4.2 kHz, ringing ~0.43 s. It takes E6..E7
// ([1318.5, 2637) Hz), above every melody anchor's octave (space 220-440 Hz,
// melody ≳ 520 Hz) and below the glints and crowns (4.2-6.3 kHz). The lift
// degree ([`SHIFT_ROTATION`]) is octave-folded into the band, which keeps
// pitch class, so the ting stays consonant and visits five pitches per lap.
/// The engine's anti-click release ramp in seconds: the last 5 ms of every
/// voice, `rel = (dur − t) × 200` in [`TrailSynth::render`] (the literal
/// stays there; this is its name). A voice whose life is of this order lives
/// inside the ramp, so short lives add it on ([`RING_OUT_PER_TAU`]).
const RELEASE_RAMP_S: f32 = 0.005;
/// The life, in decay τ, of a voice meant to be heard ending rather than be
/// ended: `e^−5` is −43.5 dB, so the −40 dB point falls before the release
/// ramp starts. The general tail law (`rainbow_kitty_v2::TAIL_DUR_PER_TAU`,
/// 2.7 τ) is the floor for every voice; this stricter figure is for the ting,
/// whose tail is the whole gesture, and the meteor tick, whose crest must
/// clear the ramp.
const RING_OUT_PER_TAU: f32 = 5.0;
/// The bottom of the ting's register, E6. The top is one octave up.
const SHIFT_TING_LO_HZ: f32 = 1318.5;
/// The ting's envelope: a 3 ms strike (under 1 kHz a sub-4 ms attack clicks),
/// an 85 ms τ, and a life of [`RING_OUT_PER_TAU`] × τ plus the ramp (430 ms).
/// It is still at 0.49 of peak when a Shift+letter lands 60 ms on, so it is
/// heard through the key, and it reaches −40 dB before the ramp ends it. The
/// music box's ting takes the same life (`rainbow_kitty_v2::TING_DUR_S`).
const SHIFT_TING_ATTACK_S: f32 = 0.003;
const SHIFT_TING_DECAY_S: f32 = 0.085;
const SHIFT_TING_DUR_S: f32 = RING_OUT_PER_TAU * SHIFT_TING_DECAY_S + RELEASE_RAMP_S;
/// The strike glint's FM index — deeper than the space bell's 0.9 (a ting's
/// face flashes harder than a downbeat's) and shorter-lived.
const SHIFT_TING_GLINT: f32 = 1.3;
const SHIFT_TING_GLINT_TAU_S: f32 = 0.018;
/// The octave partial: roundness, well under the body.
const SHIFT_TING_OCTAVE_LEVEL: f32 = 0.14;
/// The ting's voice gain against `g` (kind gain included), fitted by
/// rendering against each palette's mean accent letter over the walk (the
/// twelve v1 voices agree within 1.8 dB): the walk mean sits at the Space's
/// tier, about 3 dB under the letter, and under the capital it announces on
/// every slot of the bar, in [0.56, 1.05] of the keystroke on the
/// isolated-seed ladder. The music box's ting is fitted to the same law
/// (`rainbow_kitty_v2::TING_LEVEL`).
const SHIFT_TING_VOICE_LEVEL: f32 = 0.33;

/// Octave-fold a pitch into the ting's register `[SHIFT_TING_LO_HZ, 2×)`.
#[inline]
fn ting_octave(f: f32) -> f32 {
    fold_octave(f, SHIFT_TING_LO_HZ)
}

// ---------------------------------------------------------------------------
// The erase poof: the deletion's own voice
// ---------------------------------------------------------------------------
//
// A deletion is a different event from a keystroke, not a quieter one: two
// band-passed noise bursts (a body and an air cap) and no tonal partial,
// designed at kind level and returning before palette dispatch so no palette
// adds a pitched voice. It sits above the melodic register, so the erase reads
// higher than the letter it removes. A puff is broad and soft: high Q rings (a
// whistle) and a sub-millisecond attack clicks. The body's cutoff is static,
// the cheap path (`spawn` caches the filter coefficient when `n_glide <= 0`);
// the cap settles a bounded step down ([`POOF_AIR_SETTLE_HZ`]) and pays for its
// swept band, affordable because [`ERASE_MIN_GAP`] caps deletions at ~13/s.
/// The poof's body, centred above every palette's melodic register so the
/// erase reads higher than the letter.
const POOF_BODY_HZ: f32 = 2800.0;
/// Broad, deliberately: `1/Q` is the SVF's damping, and a Q under 1 spreads
/// the burst into a puff instead of ringing it into a whistle.
const POOF_BODY_Q: f32 = 0.55;
const POOF_BODY_ATTACK_S: f32 = 0.002;
const POOF_BODY_DECAY_S: f32 = 0.022;
const POOF_BODY_DUR_S: f32 = 0.055;
/// The air cap: a brighter, later, quieter breath that gives the puff its
/// dispersal, the "-oof" after the "p".
const POOF_AIR_HZ: f32 = 5000.0;
const POOF_AIR_Q: f32 = 0.65;
/// The cap trails the body. The body is at `delay = 0`, so a deletion speaks
/// in the first post-cue synth buffer
/// (`every_gesture_speaks_in_its_first_post_cue_buffer`).
const POOF_AIR_DELAY_S: f32 = 0.006;
const POOF_AIR_ATTACK_S: f32 = 0.0035;
const POOF_AIR_DECAY_S: f32 = 0.030;
const POOF_AIR_DUR_S: f32 = 0.080;
/// Cap level against the body's 1.0 — enough to hear the dispersal, not
/// enough to turn the puff into a hiss.
const POOF_AIR_LEVEL: f32 = 0.7;
/// The poof's voice gain on the tier-carrying `g`. Band-passed noise peaks far
/// under a tonal voice at equal gain, so the compensation belongs here rather
/// than in [`BACKSPACE_KIND_GAIN`] (a tier statement). An isolated deletion
/// peaks ~4 dB under a keystroke at the default volume.
const POOF_VOICE_GAIN: f32 = 1.28;
/// The poof's own softening lowpass — open enough to pass the 5 kHz cap
/// (a keystroke's is 2.2-4.2 kHz), which is the other half of "higher".
const POOF_LP_CUT_HZ: f32 = 7000.0;
/// The cloud's puff ([`SoundKind::Poof`]): the visible smoke's own noise, in
/// the erase poof's synthesis family. The deletion already spoke the body, so
/// the cloud is only dispersal: one air-band burst, longer and softer than the
/// cap, settling downward. No partials and no rng draws (`spawn_seeded`), so
/// the melody's random stream is independent of how many clouds ride it.
const POOF_CLOUD_DUR_S: f32 = 0.11;
const POOF_CLOUD_ATTACK_S: f32 = 0.008;
const POOF_CLOUD_DECAY_S: f32 = 0.055;
/// Where the cloud's air settles to: the cap's band drifting down as the puff
/// thins — dispersal, not a second contact.
const POOF_CLOUD_SETTLE_HZ: f32 = 3600.0;
const POOF_CLOUD_GLIDE_S: f32 = 0.06;
/// The cloud voice's gain on the tier-carrying `g` (tier = [`POOF_KIND_GAIN`],
/// the smallest). Noise compensation like the Kill swoosh's 2.6, fitted so the
/// cloud sits ~5.7 dB under the erase poof's peak.
const POOF_CLOUD_GAIN: f32 = 0.85;
/// The air settles: a gentle downward bend (about a minor third in band
/// terms) as the puff disperses, the erase's mirror of the keystroke's upward
/// lean. Any further and the cap reads as a laser zap.
const POOF_AIR_SETTLE_HZ: f32 = 4100.0;
const POOF_AIR_SETTLE_GLIDE_S: f32 = 0.05;
/// The held-run shimmer: a little glitter on the last release of a held
/// delete run. Every admitted deletion in a held run schedules a shimmer
/// [`ERASE_SHIMMER_DELAY_S`] out and damps the pending one; held poofs arrive
/// every ~75-105 ms, inside the delay, so only the final poof's shimmer
/// sounds. Any other authored gesture also damps a pending shimmer. A held run
/// is at least [`HELD_ERASE_RUN_MIN`] admitted poofs, each gap inside
/// [`HELD_ERASE_RUN_WINDOW`] (thinned auto-repeat lands at 75 ms, single
/// corrections at typing speed ~200+ ms).
const HELD_ERASE_RUN_WINDOW: f32 = 0.14;
const HELD_ERASE_RUN_MIN: u32 = 4;
const ERASE_SHIMMER_DELAY_S: f32 = 0.16;
/// The shimmer's voice: a rising breath of air-band noise with a twinkle LFO.
/// No partials (nothing to clash with the tune) and a soft attack.
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
/// Fitted by rendering: a whisper under the poof it follows — present as a
/// small reward, never a second gesture.
const ERASE_SHIMMER_GAIN: f32 = 0.5;
/// The shimmer's retrigger damp ([`SPACE_DAMP_S`]'s length): a pending
/// shimmer expires unheard, a sounding one is released in 12 ms.
const ERASE_SHIMMER_DAMP_S: f32 = 0.012;
/// The word poof ([`SoundKind::KillWord`]): the erase poof one size up and one
/// shade softer. The body sits lower and both bursts run longer, all under the
/// line kill's swoosh.
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
/// The word poof's gain over [`POOF_VOICE_GAIN`], landing between the
/// single-character poof and the line swoosh.
const WORD_POOF_GAIN: f32 = 1.1;
/// The deletion's own minimum gap, independent of the keystroke governor's
/// [`MIN_GAP`]: a held Backspace (~30/s) is thinned to at most ~13/s. Gating
/// on `since_erase` rather than `since_voice` lets the first deletion after
/// typing speak even inside the keystroke's 45 ms window; a deletion does not
/// claim the shared beat, so the next letter is not thinned.
const ERASE_MIN_GAP: f32 = 0.075;

/// The ting's own gate: a ting is thinned against other tings only. The host
/// mints a Shift 30-100 ms before its capital, often inside [`MIN_GAP`] of the
/// previous key, so the keystroke governor would silence most tings at speed.
/// The host cues once per physical press, so this only guards re-press storms
/// (left+right, a bouncing switch).
const SHIFT_MIN_GAP: f32 = 0.075;
// ---------------------------------------------------------------------------
// The space downbeat: the word boundary as the phrase's floor
// ---------------------------------------------------------------------------
//
// The bass root walks the speaking palette's anchor through [`SONG_BASS`],
// folded into one register independent of the melody's degree (a root that
// followed the melody would jump an octave between words). The letters carry
// the tune; the spaces are the floor it stands on.
/// The base of the space's bass fold (Hz): [`bass_octave`] folds each
/// palette's anchor into `[SPACE_BASS_LO_HZ, 2 × SPACE_BASS_LO_HZ)`, keeping
/// its pitch class (so it stays consonant with the melody) while every
/// palette's downbeat lands in the same octave. The root sounds one octave up,
/// in [220, 440) Hz ([`TrailSynth::space_root_hz`]), above a laptop speaker's
/// rolloff.
const SPACE_BASS_LO_HZ: f32 = 110.0;
/// The downbeat's envelope, short on purpose: a long low note at ~2 spaces/s
/// is a kick drum that masks the melody and booms in headphones. 92 ms total
/// with a ~10 ms rounded attack.
const SPACE_ATTACK_S: f32 = 0.010;
const SPACE_DECAY_S: f32 = 0.052;
const SPACE_DUR_S: f32 = 0.092;
/// The octave above the root, quietly: it carries the note's identity on a
/// laptop speaker.
const SPACE_OCTAVE_LEVEL: f32 = 0.20;
/// The downbeat's voice gain, fitted by rendering to sit ~4 dB under an
/// isolated keystroke. Less than an equal-register cut would take because the
/// ear is less sensitive in the bass.
const SPACE_VOICE_LEVEL: f32 = 0.32;
/// The retrigger fade for the monophonic downbeat. Two overlapping spaces
/// would be two voices at the same frequency with random phase, a comb filter
/// that can cancel or sum to +6 dB; one voice at a time, retriggered through
/// the switch-damp's click-free linear ramp.
const SPACE_DAMP_S: f32 = 0.012;
/// The 4f tink's level, floored by [`SPACE_BELL_TINK`] (so at these values the
/// floor sounds); [`SPACE_BREATHE_FIFTH_LEVEL`] is its rested twin.
const SPACE_FIFTH_LEVEL: f32 = 0.07;

/// The space bell's FM strike-glint index: the melody bell's idiom
/// (`fm_ratio` 3.01) at a shallower index. The face flashes and is gone in
/// ~22 ms.
const SPACE_BELL_GLINT: f32 = 0.9;
/// The space bell's 4f tink floor, well under the melody bells' 0.30 so the
/// downbeat never out-sparkles a letter.
const SPACE_BELL_TINK: f32 = 0.12;
/// The downbeat breathes when the hand rests: a space after a phrase pause
/// ([`PHRASE_PAUSE_S`], the threshold that resets the bar) gets a softer
/// entry, a longer bloom and a lifted octave. At prose speed this never fires.
const SPACE_BREATHE_ATTACK_S: f32 = 0.014;
const SPACE_BREATHE_DECAY_S: f32 = 0.085;
const SPACE_BREATHE_DUR_S: f32 = 0.150;
const SPACE_BREATHE_OCTAVE_LEVEL: f32 = 0.24;
const SPACE_BREATHE_FIFTH_LEVEL: f32 = 0.10;
/// The level of a coalesced space (the second and later spaces of one
/// whitespace run), which keeps only the downbeat's breath: the run is one
/// gesture with one bass note, but a silent key would read as dropped.
const SPACE_RUN_BREATH_LEVEL: f32 = 0.35;
// ---------------------------------------------------------------------------
// The indent steps: a whitespace run's tail
// ---------------------------------------------------------------------------
//
// A run gets one bass root, one step of the walk, one bar line. Every space
// after the head also plays a step: a short soft bell one lattice degree higher
// per extra space off the run's root (a four-space indent is a small rising
// figure), on the active tone's lattice so it cannot rub against the tune, and
// clamped under `8 × SPACE_BASS_LO_HZ` (880 Hz), where the figure holds its top
// note. Steps advance nothing (not the walk, the bar or the tune) and ride
// over the tail's breath.
/// The step's voice gain against the head's ([`SPACE_VOICE_LEVEL`]): audibly
/// between the tail's breath (0.35) and the root (1.0), never over the root.
const SPACE_STEP_LEVEL: f32 = 0.6;
/// The step's own admission clock, against the previous admitted step or the
/// run's head: 3-4 deliberate taps (80-125 ms apart) all step, while a held
/// spacebar's ~30 Hz auto-repeat, already thinned by [`MIN_GAP`], steps on
/// every second admitted space
/// (`a_held_spacebar_cannot_machine_gun_the_indent_steps`).
const SPACE_STEP_MIN_GAP: f32 = 0.075;
/// The step's envelope: struck like the head (the same glass face at a
/// shallower glint), shorter — a tick that climbs, not a second downbeat.
const SPACE_STEP_ATTACK_S: f32 = 0.004;
const SPACE_STEP_DECAY_S: f32 = 0.040;
const SPACE_STEP_DUR_S: f32 = 0.100;
const SPACE_STEP_GLINT: f32 = 0.7;
const SPACE_STEP_OCTAVE_LEVEL: f32 = 0.18;
/// The top of the steps' register: the octave over the space band.
const SPACE_STEP_HI_HZ: f32 = SPACE_BASS_LO_HZ * 8.0;
/// The bass's air: the level of the high breath over the walking root,
/// against the root's own voice gain. Without it the bass has no energy above
/// 2 kHz and reads as a woody thud rather than an instrument.
///
/// Fitted against the note on the WAV bench, deleted since
/// (`RAINBOW-KITTY-V2.md` §21.4): its tonality figure (peak-over-median in
/// 150–4500 Hz) had to stay over 60 or the downbeat stopped reading as
/// pitched. 0.9 was the most air that held (tonality ~65).
const SPACE_AIR_LEVEL: f32 = 0.9;
/// The word boundary's twinkle, on the air voice: a chime four octaves up. 4f
/// would land in the melody's register and mask the tune; 16f lands in the
/// sparkle band of the keystroke's crown and glint, and a power-of-two
/// multiple keeps the root's pitch class. The pair is quiet against the root's
/// body and the shimmer LFO takes a further ~27 % of mean level, so the
/// downbeat gains glitter without getting louder; the level keeps every root's
/// head centroid above 450 Hz.
const SPACE_TWINKLE_RATIO: f32 = 16.0;
const SPACE_TWINKLE_DETUNE: f32 = 0.030;
const SPACE_TWINKLE: f32 = 0.14;
const SPACE_TWINKLE_TWIN: f32 = 0.09;
/// The air's shimmer LFO, over the whole voice (chime and breath), so the
/// space's top scintillates rather than puffs.
const SPACE_TWINKLE_RATE: f32 = 24.0;
const SPACE_TWINKLE_DEPTH: f32 = 0.55;
/// The air's length, long enough for ~2.5 shimmer cycles and still inside the
/// downbeat's own life.
const SPACE_TWINKLE_DUR_S: f32 = 0.105;
const SPACE_TWINKLE_DECAY_S: f32 = 0.038;
/// The sparkle band: the twinkle's one register for every root of the walk,
/// `[SPACE_TWINKLE_LO_HZ, 2 × SPACE_TWINKLE_LO_HZ)` = [4.4, 8.8) kHz. Roots
/// land anywhere in [220, 440), and a fixed 16f would leave the low roots'
/// twinkle dark (a 220 Hz root at 3.5 kHz), so [`twinkle_hz`] folds it here by
/// exact octaves: roots in [220, 275) twinkle five octaves up, the rest four.
/// The twin's detune stays [`SPACE_TWINKLE_DETUNE`] × the root in Hz
/// (6.6-13.2 Hz), under the roughness band in either octave.
const SPACE_TWINKLE_LO_HZ: f32 = SPACE_BASS_LO_HZ * 40.0;

/// The twinkle's pitch for a root: `root × SPACE_TWINKLE_RATIO`, folded by
/// exact octaves into the sparkle band [`SPACE_TWINKLE_LO_HZ`].
#[inline]
fn twinkle_hz(root: f32) -> f32 {
    fold_octave(root * SPACE_TWINKLE_RATIO, SPACE_TWINKLE_LO_HZ)
}
/// Tier 0, the sub-floor: cursor motion accompanies what you are doing rather
/// than being the thing you did, so the movement gestures sit ~3 dB under the
/// typing floor. Above the others in the family because a glide is one bare
/// sine pluck, intrinsically ~1 dB under a palette keystroke at equal gain.
const GLIDE_KIND_GAIN: f32 = 0.78;
/// Tier 0, one per gesture. `cursor_glow` splits its moves into Glide/Sweep;
/// `Navigation` reaches this table as the rainbow kitty engine's nav tick when
/// a named instrument speaks under that look, and from the audition benches.
const NAVIGATION_KIND_GAIN: f32 = 0.68;
/// Tier 0. The per-note taper in `design_cursor` drops the run's tail; this
/// sets its first note. Slightly under [`GLIDE_KIND_GAIN`]'s ratio because a
/// run's notes overlap.
const SWEEP_KIND_GAIN: f32 = 0.692;
/// Tier 3: a kill destroys a line. Most of its level lives in the swoosh's
/// voice gain (a voice deficit; see `design_trail`'s Kill arm).
const KILL_KIND_GAIN: f32 = 1.25;
/// Tier 2.5: a word kill sits above the single character's poof (with the
/// rest in [`WORD_POOF_GAIN`]) and under the line kill's swoosh.
const KILLWORD_KIND_GAIN: f32 = 1.0;
/// Under the floor: the cloud's puff ([`SoundKind::Poof`]) accompanies a
/// keystroke that already spoke, so it must read as texture, never a second
/// key. The smallest priority in the vocabulary; most of the level lives in
/// the voice (`design_poof`), since band-passed noise sits ~17 dB under a
/// tonal voice at equal gain. Fitted to land ~7 dB
/// under the Backspace voice; quieter and it falls inside that voice's
/// forward-masking shadow.
const POOF_KIND_GAIN: f32 = 0.68;
/// Tier 3: the trail ceiling, which [`BONK_KIND_GAIN`] must stay strictly
/// above (const-asserted).
const JUMP_KIND_GAIN: f32 = 1.25;

/// Bonk kind-gain, above every trail gesture: the wrong note must not hide in
/// the texture it interrupts.
const BONK_KIND_GAIN: f32 = 1.35;

/// The cursor-landing star chime's register ([`SoundKind::Land`]), an octave
/// over the trail's warm mid, so the stars glitter above the tune.
const LAND_ANCHOR_HZ: f32 = 784.0;
/// Tier 4, the landing kind-gain, below the trail ceiling: the landing's
/// presence comes from its voice design (three star tones over an arrival
/// body), which at the ceiling would outshout the bonk. Lands ~2 dB under
/// the bonk. Its margin over a keystroke varies by palette because Land is
/// style-agnostic.
const LAND_KIND_GAIN: f32 = 0.865;
/// Star tones per landing, [`LAND_STAR_DEGREE_STEP`] degrees apart and
/// pre-delayed [`LAND_STAR_STEP_S`] (delayed voices, no scheduler). The first
/// has `delay = 0`, so a landing speaks in the first post-cue synth buffer.
const LAND_STARS: usize = 3;
const LAND_STAR_STEP_S: f32 = 0.034;
const LAND_STAR_DEGREE_STEP: i32 = 2;

/// Master duck: how deep the melody and bed dip while a bonk speaks (0.55 ⇒
/// −7 dB at impact), and the exponential recovery time (recovered within
/// about a second).
const BONK_DUCK_DEPTH: f32 = 0.55;
const BONK_DUCK_TAU: f32 = 0.28;

/// Samples a geometric envelope/glide recursion runs before it is re-anchored
/// to a real `exp` (~1.3 ms at 48 kHz), bounding f32 drift to what compounds
/// over that many multiplies.
const ENV_REANCHOR: u32 = 64;

/// One celebration riff bar in seconds: 4 beats at the sing-along's 150 BPM.
/// Const-asserted equal to `kitty_sing::SING_BAR_SECONDS`, so the host can
/// push one [`CelebrationGesture::RiffBar`] per visual bar.
pub const CELEBRATION_BAR_SECONDS: f32 = 1.6;

/// One riff eighth-note in seconds (8 per bar).
const CELEBRATION_EIGHTH: f32 = CELEBRATION_BAR_SECONDS / 8.0;

/// The outro's length (§27): three beats at the sing-along tempo.
/// Const-asserted equal to `kitty_sing::OUTRO_SECONDS`.
pub const CELEBRATION_OUTRO_S: f32 = 1.2;

// The audio bar, its eighth-note, the outro and the visual dance clock share
// one tempo, asserted at compile time.
const _: () = assert!(CELEBRATION_BAR_SECONDS == crate::kitty_sing::SING_BAR_SECONDS);
const _: () = assert!(CELEBRATION_EIGHTH == crate::kitty_sing::SING_BEAT_SECONDS / 2.0);
const _: () = assert!(CELEBRATION_OUTRO_S == crate::kitty_sing::OUTRO_SECONDS);
const _: () = assert!(
    CELEBRATION_OUTRO_S < CELEBRATION_BAR_SECONDS,
    "the outro is shorter than a bar: it ends the song, it is not one more bar"
);

/// The outro lead's decay τ: a held *do* that is still sounding at the
/// three-beat line and gone within the sing duck's handback.
const CELEBRATION_OUTRO_DECAY_S: f32 = 0.34;

/// The outro's sub — the bass *do* (C3, `CELEBRATION_BASS`'s `-10`).
const CELEBRATION_OUTRO_SUB_DEG: i32 = -10;

/// A grid slot that holds: no new note, the previous one sustains through it.
/// A sentinel rather than `Option<i32>` keeps the tables flat `[i32; 8]`
/// literals.
const REST: i32 = i32::MIN;

/// Bars in the authored phrase. A power of two so the form index survives
/// the host's `u64` → `u16` bar-counter truncation in phase (`2^16 % 8 == 0`;
/// `celebration_form_is_wrap_safe`).
const CELEBRATION_PHRASE_BARS: usize = 8;

/// The sing-along riff: an original eight-bar just-intonation major
/// pentatonic song (degrees on [`PENTA`]), in the mood of chip-tune cat songs
/// but not any existing tune. It shares the engine's consonant lattice, so the
/// typed melody under it never rubs.
///
/// Form A A' B A" | C C' B' D: verse, lifted chorus, breathing bridge,
/// turnaround; bars 0 and 1 are the opening hook.
///
/// `REST` slots do not silence: the preceding note sustains through them.
const CELEBRATION_PHRASE: [[i32; 8]; CELEBRATION_PHRASE_BARS] = [
    // 0 — A, the hook.
    [0, 2, 4, 5, 4, 2, 4, 7],
    // 1 — A', the answer.
    [5, 7, 5, 4, 2, 4, 2, 0],
    // 2 — B, the hook syncopated: two held notes and two holes.
    [2, 4, REST, 7, 5, REST, 3, 2],
    // 3 — A", the hook again, half-cadencing high and holding over the
    //     dominant pedal below — the song's first real breath.
    [0, 2, 4, 5, 7, REST, 8, REST],
    // 4 — C, the lift: the chorus, up an octave, peaking on the 9th degree.
    [5, REST, 7, 8, 9, 7, 5, 4],
    // 5 — C', the descent out of the top.
    [9, 8, 7, 5, 7, 5, 4, 2],
    // 6 — B', the bridge: four held notes, low and wide. This bar's air is
    //     what stops the song reading as a machine.
    [0, REST, 2, REST, 4, REST, 2, REST],
    // 7 — D, the turnaround: climb, then [`CELEBRATION_FILL`] tumbles the
    //     phrase back onto bar 0's tonic in the last two slots.
    [4, 5, 7, 8, 9, 7, REST, REST],
];

/// The turnaround fill: four sixteenths across the last two slots of the last
/// bar, landing back on the hook.
const CELEBRATION_FILL: [i32; 4] = [7, 5, 4, 2];

/// The bassline, one note per beat (four per bar), an independent voice.
///
/// A bass note rides as the third partial of the lead voice that opens its
/// beat (no extra voice), so every non-`REST` bass beat requires a non-`REST`
/// lead slot at `2 * beat` (`celebration_bass_always_has_a_carrier`).
const CELEBRATION_BASS: [[i32; 4]; CELEBRATION_PHRASE_BARS] = [
    [-10, REST, -7, REST], // C3 … G3 …
    [-8, REST, -10, REST], // E3 … C3 …
    [-9, REST, -6, REST],  // D3 … A3 …
    [-7, REST, -7, REST],  // G3 … G3 …  the pedal under the held cadence
    [-10, -10, -7, -7],    // the chorus pulse
    [-8, -8, -9, -9],
    [-10, REST, -6, REST],
    [-7, -7, -9, REST], // the walk home; beat 3 belongs to the fill
];

/// The groove: per-slot velocity across the eighth grid, shared by every bar.
/// Downbeat strongest, a backbeat lift on slots 2 and 6, offbeats ghosted
/// (~4.5 dB of internal dynamics).
const CELEBRATION_GROOVE: [f32; 8] = [0.55, 0.33, 0.44, 0.36, 0.50, 0.33, 0.46, 0.38];

/// Per-bar dynamic shape multiplying [`CELEBRATION_GROOVE`]: the chorus pushes,
/// the bridge drops back so the turnaround has somewhere to go.
const CELEBRATION_BAR_LIFT: [f32; CELEBRATION_PHRASE_BARS] =
    [1.00, 0.96, 1.02, 1.00, 1.12, 1.08, 0.86, 1.06];

/// Swing: odd eighths land this fraction of an eighth late. Applied to the
/// pre-delay only, so the bar stays exactly [`CELEBRATION_BAR_SECONDS`] long.
const CELEBRATION_SWING: f32 = 0.14;

/// Escalation: over its first bars the song's low end fades up, the lowpass
/// opens and the octave shimmer grows. A pure function of the bar index with
/// no rng draws, so the riff replays independently of the typed layer.
const CELEBRATION_BUILD_BARS: f32 = 6.0;

/// From this bar on (one full phrase in), the backbeat slots add a chip clap,
/// a short noise burst in the note's own noise channel (no extra voice).
const CELEBRATION_CLAP_BAR: u16 = 8;

/// Riff register root — C5, the rainbow kitty palette's own chip register.
const CELEBRATION_BASE_HZ: f32 = 523.25;

/// Tier 5: the riff's place on the loudness ladder. The riff is the only
/// governor-exempt gesture (`design_celebration` omits the flood duck), so it
/// needs its own kind gain. 0.6 keeps the whole escalation inside tier 5: it
/// grows into the ceiling rather than through it.
const CELEBRATION_KIND_GAIN: f32 = 0.6;

/// How long an old key's still-ringing celebration voices take to yield when
/// a bar arrives under a different signature. Riff notes ring past the bar
/// line (1.5× legato plus tails); across a key switch those tails would carry
/// the old root and mode under the new verse. 50 ms reads as a lifted pedal
/// rather than a cut. Same-signature bars never damp; a release without a
/// switch is left to the sing-duck wind-down.
const CELEBRATION_DAMP_S: f32 = 0.05;

// ---------------------------------------------------------------------------
// Per-key song axes
// ---------------------------------------------------------------------------
//
// Every axis is a pure function of the gesture payload's `sig`, the bijective
// per-character song signature (`kitty_sing::song_signature`). The synth keeps
// no per-key state, so a mid-hold key change is just the next bar arriving
// with a different payload.
//
// The walk encoding: first symbol radix 5, then base-4 interval transitions;
// `5 * 4^15 > 2^32`, so sixteen coded notes carry the whole signature
// injectively.

/// The verse walk's four moves: one base-4 digit of `sig / 5` per transition.
/// No move is 0 (mod 5), so adjacent coded notes always change degree (no
/// drone), and nearby characters get different contours rather than
/// transposed copies.
const SONG_INTERVALS: [i32; 4] = [-1, 1, -2, 2];

/// Coded notes per walk cycle: sixteen carry every bit of the `u32`
/// signature without collision, plus one bridge note ([`SONG_STEPS`]) chosen
/// away from both endpoints so the walk also moves across its own wrap.
const SONG_CODED_STEPS: u32 = 16;
const SONG_STEPS: u32 = SONG_CODED_STEPS + 1;

/// Decode coded walk symbol `index` of `sig`: reversible mixed radix —
/// symbol zero is `sig % 5`, each transition one base-4 digit of `sig / 5`
/// spent on [`SONG_INTERVALS`], the degree wrapped into the pentatonic
/// window `0..=4` (a wrap is itself a move — a compound leap, never a
/// repeat).
fn song_coded_degree(sig: u32, index: u32) -> i32 {
    debug_assert!(index < SONG_CODED_STEPS);
    let mut code = u64::from(sig);
    let mut degree = (code % 5) as i32;
    code /= 5;
    for _ in 0..index {
        let choice = (code & 3) as usize;
        code >>= 2;
        degree = (degree + SONG_INTERVALS[choice]).rem_euclid(5);
    }
    degree
}

/// The walk degree (`0..=4`) at verse-note `ordinal`, cycling every
/// [`SONG_STEPS`] notes with the bridge at index 16.
fn song_degree(sig: u32, ordinal: u32) -> i32 {
    let index = ordinal % SONG_STEPS;
    if index < SONG_CODED_STEPS {
        return song_coded_degree(sig, index);
    }
    let first = song_coded_degree(sig, 0);
    let last = song_coded_degree(sig, SONG_CODED_STEPS - 1);
    for interval in SONG_INTERVALS {
        let bridge = (last + interval).rem_euclid(5);
        if bridge != first {
            return bridge;
        }
    }
    unreachable!("four distinct nonzero pentatonic moves cannot all land on the first note")
}

/// Axis 2, the root the celebration transposes to: `sig % 5 - 2`, inside one
/// pentatonic octave (-2..=2), so no character moves the riff into a register
/// that reads as a different instrument.
fn celebration_root(sig: u32) -> i32 {
    (sig % 5) as i32 - 2
}

/// Axis 3, mode feel: an integer degree rotation folded (with the root) into
/// every sounding degree before [`TrailSynth::melody_hz`]. Pentatonic modes are
/// rotations and the just lattice's steps are unequal, so rotation changes the
/// interval colour under the same contour while staying on the consonant
/// lattice (tone tables are never swapped per key). The class is `sig % 3` ∈
/// {0, 2, 4}; rotation by 4 is realised octave-folded as −1, so the combined
/// root+mode offset stays in −3..=4.
const MODE_ROTATIONS: [i32; 3] = [0, 2, -1];
fn celebration_mode(sig: u32) -> i32 {
    MODE_ROTATIONS[(sig % 3) as usize]
}

/// The riff lead's pulse duty, one timbre for every key: duty is the chip
/// voice's loudest timbral lever, and per-key duty would sound like different
/// instruments.
const CELEBRATION_LEAD_DUTY: f32 = 0.25;

/// Which form slots are verse (per-key walk) vs the shared chorus. Verses are
/// the A-family (bars 0/1/3) and the lift pair C/C' (bars 4/5); B (2), the
/// bridge B' (6) and the turnaround D (7, fill included) keep the authored
/// phrase for every key, so every key's song is recognisably the celebration.
const CELEBRATION_VERSE: [bool; CELEBRATION_PHRASE_BARS] =
    [true, true, false, true, true, true, false, false];

/// Register shelf lifting the walk's `0..=4` window into each verse family's
/// tessitura: the A-family sits just over the hook's shelf, the C-family keeps
/// the lift into bars 4/5.
fn verse_register(idx: usize) -> i32 {
    if idx == 4 || idx == 5 { 4 } else { 1 }
}

/// The walk ordinal of the first sounding slot of verse bar `idx` (`None` for
/// chorus bars). Sounding verse slots are numbered consecutively across the
/// form, counted from the authored `REST` pattern.
fn verse_ordinal_base(idx: usize) -> Option<u32> {
    if !CELEBRATION_VERSE[idx] {
        return None;
    }
    let mut base = 0u32;
    for (i, row) in CELEBRATION_PHRASE.iter().enumerate().take(idx) {
        if CELEBRATION_VERSE[i] {
            base += row.iter().filter(|&&d| d != REST).count() as u32;
        }
    }
    Some(base)
}

/// Axis 1, the verse: this bar's contour row for `sig`. Chorus bars return the
/// authored row verbatim (`verse_walk_always_moves_and_keeps_rhythm_and_register`); verse
/// bars keep the authored rhythm (the `REST` pattern, and with it the groove,
/// swing, span and bass-carrier laws) and replace each sounding degree with
/// the signature's walk. Root and mode are folded in by `design_celebration`
/// at pitch time.
fn celebration_bar_degrees(sig: u32, idx: usize) -> [i32; 8] {
    let mut row = CELEBRATION_PHRASE[idx];
    let Some(base) = verse_ordinal_base(idx) else {
        return row;
    };
    let shelf = verse_register(idx);
    let mut ordinal = base;
    for slot in &mut row {
        if *slot != REST {
            *slot = song_degree(sig, ordinal) + shelf;
            ordinal += 1;
        }
    }
    row
}

/// The swung pre-delay of eighth-grid slot `slot`. Slot 0 is exactly 0.0, so
/// every bar speaks in its first post-cue synth buffer.
fn celebration_slot_delay(slot: usize) -> f32 {
    let swing = if slot.is_multiple_of(2) {
        0.0
    } else {
        CELEBRATION_SWING
    };
    (slot as f32 + swing) * CELEBRATION_EIGHTH
}

/// Pitch-panned stereo: notes move across the field with their register
/// rather than ping-ponging per eighth. Degree 4 sits at the event's pan;
/// `spawn` narrows and clamps afterwards.
fn celebration_sway(pan: f32, deg: i32) -> f32 {
    pan + 0.06 * (deg - 4) as f32
}

/// The riff's pulse lead on `hz`: the [`CELEBRATION_LEAD_DUTY`] pulse, its
/// square octave `shimmer`, and `third` (the bass, or silence).
fn celebration_lead(hz: f32, shimmer: f32, third: Partial) -> [Partial; 3] {
    [
        Partial {
            lvl: 0.55,
            f0: hz,
            f1: hz,
            wave: Wave::Pulse {
                duty: CELEBRATION_LEAD_DUTY,
            },
            ..Partial::default()
        },
        Partial {
            lvl: shimmer,
            f0: hz * 2.0,
            f1: hz * 2.0,
            wave: Wave::Pulse { duty: 0.5 },
            ..Partial::default()
        },
        third,
    ]
}

/// How many eighth-grid slots the note at `i` owns: itself plus the `REST` slots
/// that follow, bounded by `limit` (the turnaround bar caps this at 6 so the
/// sustained note cannot ring under the sixteenth fill).
fn celebration_span(phrase: &[i32; 8], i: usize, limit: usize) -> usize {
    let mut span = 1usize;
    while i + span < limit && phrase[i + span] == REST {
        span += 1;
    }
    span
}

/// Sing-along duck: how deep the ordinary melody and bed sit while the riff
/// speaks (0.65 ⇒ about −9 dB; the riff replaces the melody), and the
/// exponential handback time once bars stop arriving.
const SING_DUCK_DEPTH: f32 = 0.65;
const SING_DUCK_TAU: f32 = 0.40;

// ---------------------------------------------------------------------------
// Voices
// ---------------------------------------------------------------------------

/// Oscillator flavour for one partial.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
enum Wave {
    #[default]
    Sine,
    /// Naive pulse at `duty`, softened by the per-voice lowpass — the chiptune
    /// voice. Aliasing is inaudible here: fundamentals sit low, the lowpass is
    /// gentle, and levels are tiny.
    Pulse { duty: f32 },
}

/// One partial of a voice: a (possibly FM'd) oscillator with an exponential
/// frequency glide `f0 → f1` (τ = `glide` seconds). Three of these cover
/// every palette: plucks (1 sine), dyads/chirps (2), glassy FM bells (FM on
/// the fundamental + a shimmer partial).
#[derive(Clone, Copy, Default, Debug)]
struct Partial {
    lvl: f32,
    f0: f32,
    f1: f32,
    /// Exponential glide time constant in seconds; ≤ 0 means "hold f0".
    glide: f32,
    /// FM: modulator frequency = carrier × ratio; index decays with `fm_tau`.
    /// Ratio 0 disables FM. Slightly inharmonic ratios (3.01, 2.76…) are what
    /// make the comet's chimes read as ice rather than organ.
    fm_ratio: f32,
    fm_i0: f32,
    fm_tau: f32,
    wave: Wave,
    /// Per-partial decay in seconds (`RAINBOW-KITTY-V2.md` §16 delta 1).
    ///
    /// `0.0` shares the voice envelope: the branch is untaken and the loop
    /// evaluates `s += p.lvl * x`, so the pinned palettes render unchanged.
    /// A layered decay lets a tine's octave and inharmonic strike die in
    /// tens of ms while the fundamental rings; a partial with τ ≤ 45 ms is
    /// gone before a 15 Hz beat completes a cycle, so it cannot sound rough.
    decay: f32,
    ph: f32,
    fm_ph: f32,
    /// Geometric glide / FM-index state: `g_e` is the running `e^(-t/glide)`,
    /// `fm_e` the running `e^(-t/fm_tau)`, `k_g`/`k_f` their per-sample steps
    /// `e^(-dt/τ)` from `TrailSynth::spawn`. Seeded with a real `exp` at the
    /// first sounding sample (see `Voice::env_run`).
    g_e: f32,
    k_g: f32,
    fm_e: f32,
    k_f: f32,
    /// The running `e^(-t/decay)` and its constant per-sample step, seeded and
    /// re-anchored on exactly the [`ENV_REANCHOR`] clock the voice envelope
    /// uses, so all recursions in a voice stay mutually consistent.
    p_e: f32,
    k_p: f32,
}

/// A live voice: up to three partials + one band-passed noise burst through a
/// shared envelope, twinkle LFO, per-voice lowpass and equal-power pan.
#[derive(Clone, Copy, Default, Debug)]
struct Voice {
    on: bool,
    /// Seconds since onset (post-delay).
    t: f32,
    /// Pre-delay before the voice sounds — splash droplets and arpeggio notes
    /// are just delayed voices, no separate scheduler.
    delay: f32,
    /// Hard lifetime; a 5 ms release ramp at the end guarantees no clicks.
    dur: f32,
    /// Panned linear gains (gain × equal-power pan), left/right.
    gl: f32,
    gr: f32,
    /// Envelope: `(1 - e^(-t/attack)) · e^(-t/decay)`.
    attack: f32,
    decay: f32,
    p: [Partial; 3],
    /// Band-passed noise: level + state-variable filter sweeping `n_f0 → n_f1`.
    n_lvl: f32,
    n_f0: f32,
    n_f1: f32,
    n_glide: f32,
    n_q: f32,
    n_lp: f32,
    n_bp: f32,
    /// Geometric envelope / noise-glide state: the running `e^(-t/attack)`,
    /// `e^(-t/decay)` and `e^(-t/n_glide)` with their per-sample steps from
    /// `TrailSynth::spawn`. The design fields (`attack`, `decay`, `n_f0`,
    /// `n_f1`) stay untouched; tests read them back after `push`.
    env_a: f32,
    env_d: f32,
    k_a: f32,
    k_d: f32,
    n_e: f32,
    k_n: f32,
    /// False until the first sounding sample, where every recursion above is
    /// seeded from a real `exp`. A pre-delayed voice starts at `t = -delay`,
    /// so the zero crossing is not a multiple of `dt` and seeding cannot
    /// happen in `spawn`.
    env_run: bool,
    /// Samples since the recursions were last anchored to a real `exp`.
    ///
    /// A geometric recursion drifts in f32 as rounding compounds over a long
    /// ring-out; re-anchoring every [`ENV_REANCHOR`] samples bounds the drift
    /// while replacing all but one exponential per window.
    env_n: u32,
    /// Amplitude twinkle (sparkle/comet glitter): depth 0..1 at `tw_rate` Hz,
    /// phase-jittered per voice so clusters shimmer instead of pulsing.
    tw_rate: f32,
    tw_depth: f32,
    tw_ph: f32,
    /// One-pole lowpass cutoff (Hz) applied to the voice's mixed output —
    /// the master "keep it soft" control every palette leans on.
    lp_cut: f32,
    lp: f32,
    /// Render coefficients precomputed by [`TrailSynth::spawn`] from fields
    /// that never change afterwards. Same expressions and operands, so every
    /// sample stays bit-identical.
    ///
    /// `lp_k` is the per-voice lowpass coefficient; `n_damp` the noise SVF's
    /// damping `1/Q`. `svf_g`/`svf_den` are the SVF coefficient and
    /// denominator, cached only when the noise cutoff is constant
    /// (`n_lvl > 0` and `n_glide <= 0`). The divisor is cached, not its
    /// reciprocal: `x / d` and `x * (1.0 / d)` differ in f32.
    lp_k: f32,
    n_damp: f32,
    svf_g: f32,
    svf_den: f32,
    /// Exempt from the master duck envelope; only the bonk sets it, since the
    /// duck makes room for the bonk. `false` renders the plain signal path
    /// (at rest the duck multiplier is exactly 1.0).
    duck_exempt: bool,
    /// Marks a sing-along celebration voice (bar notes and fills), the address
    /// tag for the key-switch damp. `duck_exempt` cannot serve: a key change
    /// must never damp the bonk.
    celebration: bool,
    /// Marks the Space downbeat's bass voice, so the next word boundary can
    /// damp it ([`SPACE_DAMP_S`]). Separate from `celebration` so the riff and
    /// the typing's floor never damp each other.
    bass: bool,
    /// Marks the held-run release shimmer scheduled by
    /// [`design_erase_poof`](TrailSynth::design_erase_poof), so the run's next
    /// gesture can damp it before it sounds ([`ERASE_SHIMMER_DELAY_S`]).
    /// Separate from `bass` so neither damps the other.
    shimmer: bool,
    /// Remaining damp release in seconds; `0.0` = not damped. `render` burns
    /// it on the sample clock (pre-delay included, so an unstarted voice
    /// expires unheard) and scales the envelope by the linear ramp
    /// `damp / damp0`. The `0.0` default leaves the branch untaken.
    damp: f32,
    /// The length the damp was armed to, the ramp's denominator, so a release
    /// starts at exactly ×1.0: [`CELEBRATION_DAMP_S`] for a key switch,
    /// [`SPACE_DAMP_S`] for a space retrigger.
    damp0: f32,

    // -- Rainbow kitty v2 engine deltas (§16 rows 2-5) ---------------------
    //
    // Each defaults to the identity: a zero field leaves its branch untaken
    // and the v1 render unchanged.
    /// Per-noise-channel decay in seconds (§16 delta 2); `0.0` shares the
    /// voice envelope. Gives the felt mallet a short life of its own, so its
    /// noise band does not rumble under the whole note.
    n_decay: f32,
    /// The running `e^(-t/n_decay)` and its per-sample step (see `p_e`).
    nd_e: f32,
    k_nd: f32,
    /// Pan glide (§16 delta 3): the panned gains this voice arrives at.
    /// `pan_glide_s == 0.0` means static and the render reads `gl`/`gr`.
    /// Only the meteor uses it, travelling from the origin column to the
    /// destination on the pixels' clock (§12.2).
    pan1: f32,
    /// The panned gains derived from `pan1` by
    /// [`TrailSynth::spawn_seeded`]'s equal-power law, the same law as the
    /// static pan.
    gl1: f32,
    gr1: f32,
    /// Glide length in seconds; `0.0` = static pan.
    pan_glide_s: f32,
    /// `1.0 / pan_glide_s`, resolved by [`TrailSynth::spawn_seeded`] so the
    /// per-sample lerp costs two mul-adds and no divide.
    pan_k: f32,
    /// When the glide started, in the voice's own `t`: `0.0` for a voice that
    /// travels from its onset, the claim instant for an armed core re-aimed
    /// mid-flight (§15.2). Read only when `pan_glide_s > 0.0`.
    pan_t0: f32,
    /// The lane this voice was claimed into (§16 delta 4, §14).
    /// [`LANE_NONE`] (0) means unlaned: claimed by [`TrailSynth::claim`]
    /// with no per-lane cap.
    lane: u8,
    /// Spawn ordinal, monotone: identifies the oldest voice in a lane and a
    /// cancelled meteor bell exactly, even after slot reuse.
    born: u32,
    /// Armed-voice time to live in seconds (§16 delta 5); `0.0` = never
    /// expires. Set only on meteor pre-cues: a kept arm clears it on claim,
    /// an unkept one runs out and the voice damps over 12 ms.
    arm_ttl: f32,
}

impl Voice {
    /// Peak-ish loudness proxy used for voice stealing (quietest loses).
    #[inline]
    fn weight(&self) -> f32 {
        if !self.on {
            return -1.0;
        }
        let env = (-self.t / self.decay.max(1e-3)).exp();
        (self.gl + self.gr) * env * (self.p[0].lvl + self.p[1].lvl + self.p[2].lvl + self.n_lvl)
    }
}

// ---------------------------------------------------------------------------
// Beds — the continuous per-style textures
// ---------------------------------------------------------------------------

/// Continuous background layer state. One bed exists; it follows the style of
/// the most recent trail event (the energy smoothing softens a style change).
/// Word gestures feed it nothing, so a bonk does not swell the ambience.
#[derive(Clone, Copy, Default, Debug)]
struct Bed {
    /// Fast accumulator each event tops up (clamped ≤ 1); decays ~0.9 s.
    energy: f32,
    /// Slew-limited audible level derived from `energy` — rises in ~250 ms,
    /// falls in ~700 ms, so the bed swells in behind a burst of typing and
    /// exhales after it, never gating on and off per key.
    level: f32,
    /// Smoothed event gain so the bed honours the user volume.
    gain: f32,
    /// Oscillator/LFO phases (usage varies per style).
    ph1: f32,
    ph2: f32,
    ph3: f32,
    lfo1: f32,
    lfo2: f32,
    /// Filter states (stream noise, crackle body).
    lp1: f32,
    lp2: f32,
    /// Countdown (seconds) to the next stochastic bed grain — sparkle's
    /// micro-chimes, water's ambient plips, fire's pops.
    timer: f32,
    /// Beam power-down: smoothed droop factor that bends the hum flat as the
    /// energy dies, the aural twin of the tube thinning to a hairline.
    droop: f32,
    /// The rainbow sky pad state (`rainbow_kitty_v2::bed_rainbow_sky`; unread
    /// by other beds): oscillator phases (turns) and portamento'd frequencies
    /// (Hz; `<= 0` means not yet seeded).
    var_ph: [f32; 4],
    var_f: [f32; 4],
    /// The sky's colour: the rainbow hue's arc position
    /// (`rainbow_kitty_v2::hue_arc`, 0 red to 1 cyan) through a one-pole with
    /// τ = `BED_HUE_TAU_S`, slewed per v2 event. The arc is continuous where
    /// the raw hue wraps. Drives the sky pad's tilt and detune, never a pitch;
    /// read by the music box's bed only.
    hue_s: f32,
}

/// The bed's energy kick per admitted trail gesture, shared by v1's trail arm
/// and the music box's `push_v2` so both feed the bed the same amounts.
#[inline]
fn bed_kick(kind: SoundKind) -> f32 {
    match kind {
        SoundKind::Jump | SoundKind::Kill | SoundKind::Land => 0.5,
        // A word kill is per-command like the line kill, at word scale.
        SoundKind::KillWord => 0.4,
        // A space is typing cadence exactly like a letter.
        SoundKind::Typed | SoundKind::Backspace | SoundKind::Space => 0.3,
        // Cursor scrubbing and the shift lift feed the bed at a whisper.
        // The smoke puff feeds nothing: its Backspace already kicked the bed.
        SoundKind::Navigation
        | SoundKind::Glide { .. }
        | SoundKind::Sweep { .. }
        | SoundKind::Shift => 0.12,
        // The music box's own gestures ride a keystroke or jump that already
        // kicked the bed (v1 never sees these kinds).
        SoundKind::Poof
        | SoundKind::MeteorArm { .. }
        | SoundKind::Meteor { .. }
        | SoundKind::Enter { .. }
        | SoundKind::Stardust { .. } => 0.0,
        // A paste is typing cadence arriving at once: the Typed row.
        SoundKind::Strum => 0.3,
    }
}

// ---------------------------------------------------------------------------
// The synth
// ---------------------------------------------------------------------------

/// See the module docs. Construct once per output stream with the stream's
/// sample rate, [`push`](Self::push) events from the trail-spawn edge (and
/// the word-decoration drain), call [`render`](Self::render) from the audio
/// callback.
#[derive(Clone, Debug)]
pub struct TrailSynth {
    /// Test-only: leave the bare Shift's ting at full level under its capital
    /// (`rainbow_kitty_v2`'s `v2_duck_ting`), the unducked twin of
    /// `the_bell_yields_to_the_shifted_key_it_announced_and_to_nothing_else`.
    /// The duck draws nothing.
    #[cfg(test)]
    pub(crate) ting_unducked: bool,
    inv_sr: f32,
    rng: u32,
    voices: [Voice; MAX_VOICES],
    bed: Bed,
    bed_style: GlowStyle,
    /// The voice family latched beside [`Self::bed_style`] by the same trail
    /// events, so the bed texture follows the `trail_sound_style` override
    /// exactly like it follows the visual style.
    bed_voice: SoundVoice,
    /// Smoothed event rate (events/second) for the governor.
    rate: f32,
    /// Seconds since the last admitted discrete voice (min-gap thinning).
    since_voice: f32,
    /// Seconds since the last event of any kind (rate decay bookkeeping).
    since_event: f32,
    /// The block pre-roll for the push in flight ([`EventMeta::block_lead_s`]):
    /// set by [`Self::push_meta`] for one push, so its voices are pre-delayed
    /// by the cue's arrival offset and render-time spawns never are. `0.0`
    /// between pushes.
    block_lead_s: f32,
    /// Seconds since the last admitted deletion, the erase gate's clock
    /// ([`ERASE_MIN_GAP`]). Separate from [`Self::since_voice`] so a poof
    /// survives a correction inside the keystroke gap and a held Backspace
    /// is thinned even when nothing else speaks.
    since_erase: f32,
    /// Seconds since the last admitted bare Shift, the ting's clock
    /// ([`SHIFT_MIN_GAP`]). Separate from [`Self::since_voice`] so a ting
    /// survives the letter before its capital and a re-press storm is thinned.
    since_shift: f32,
    /// Held-run length: consecutive admitted deletions with gaps inside
    /// [`HELD_ERASE_RUN_WINDOW`]. Reaches [`HELD_ERASE_RUN_MIN`] only under
    /// auto-repeat; decides whether the run earns its release shimmer.
    erase_run: u32,
    /// `true` when the previous text gesture was a space. Only a run's head is
    /// a bass downbeat; its tail answers with air
    /// ([`SPACE_RUN_BREATH_LEVEL`]). Set by every Space, admitted or not,
    /// cleared by every other text gesture; a bare [`SoundKind::Shift`]
    /// leaves it alone.
    space_run: bool,
    /// Scratch: whether the space being designed opens its run, written by
    /// [`Self::push`] just before the design call that reads it, so the run law
    /// stays with the admission policy.
    space_head: bool,
    /// The melody's current degree on the pentatonic lattice: the theme's
    /// note at the playhead plus the tone's [`melody_lean`], folded into
    /// [`tone_register`] ([`Self::advance_melody`]). Advanced by trail
    /// gestures only; the bonk clashes against it, `design_trail` offsets it
    /// by column, and Glide/Sweep play relative to it.
    walk: i32,
    /// The theme's playhead: the index into [`SONG_THEME`] the next accent
    /// sings. Advances one per accent and wraps at 28.
    theme_pos: u8,
    /// The phrase of [`SONG_FORM`] the playhead is in, as the index of its
    /// first note (`SONG_FORM[phrase_idx] .. SONG_FORM[phrase_idx + 1]`).
    ///
    /// A phrase boundary (Enter, a comma-length pause, or the phrase running
    /// out) jumps the playhead to the next phrase rather than to zero, so a
    /// pause answers the phrase it interrupted.
    phrase_idx: u8,
    /// The bass walk's position: the index into [`SONG_BASS`] the next word
    /// boundary plays. Advanced by whitespace-run heads only, so the harmony
    /// moves per word while the theme moves per accent; the two clocks never
    /// re-align, which varies the long form.
    bass_step: u8,
    /// The lift's rotation position: the index into [`SHIFT_ROTATION`] the
    /// next sounding bare [`SoundKind::Shift`] plays. Advanced only by
    /// admitted lifts and never rewound, a third clock beside the theme
    /// (accents) and the bass (words). The music box reads the same cursor
    /// as an index into `rainbow_kitty_v2::TING_PICK`.
    shift_step: u8,
    /// The whitespace run's length after its head (0 at the head). Follows
    /// the text like [`Self::space_run`], so a thinned space still counts.
    space_run_len: u8,
    /// The run's root captured at the head: the walk degree (song key
    /// included) and the folded frequency the bass sounded, so the tail
    /// climbs from the note that was heard.
    space_run_deg: i32,
    space_run_root_hz: f32,
    /// The indent step's admission clock ([`SPACE_STEP_MIN_GAP`]): seconds
    /// since the run's head or the last admitted step.
    since_space_step: f32,
    /// The bar position: the index into [`SONG_PULSE`] the next keystroke
    /// plays. Advanced by keystrokes only and reset to the downbeat at every
    /// phrase boundary, so a phrase opens on an accent.
    song_pulse: u8,
    /// Whether the keystroke being designed is an accent (the melody sings)
    /// or a ghost. Written by [`Self::advance_song`] just before the design.
    song_accent: bool,
    /// A ghost's offset in scale degrees from [`Self::walk`] — meaningless
    /// while `song_accent` is true.
    song_ghost: i8,
    /// Melody notes sung since construction. Not read by the DSP; tests use
    /// it to tell an accent from a ghost.
    song_notes: u32,
    /// Length multiplier for the current palette dispatch: 1.0 except for a
    /// ghost keystroke ([`SONG_GHOST_FEEL`]). Set and cleared around that one
    /// dispatch, so every other spawn keeps `spawn`'s multiply-free path.
    song_feel: f32,
    /// The capital's octave ratio: [`shift_ratio`] inside a shifted glyph's
    /// dispatch, exactly 1.0 elsewhere, set and cleared like `song_feel`.
    /// `spawn_seeded` raises every tonal voice's roof by it (so the lifted
    /// note is not eaten by the palette's fixed `lp_cut`) and divides each
    /// detuned twin's offset by it (so a shimmer stated in cents keeps its
    /// rate in Hz). See [`SHIFT_GLYPH_GAIN`].
    shift_octave: f32,
    /// The melody's current tone, the last trail event's inferred mood. Steers
    /// the scale table and transpose ([`tone_tables`]), the walk, and the
    /// spawn-time feel ([`tone_feel`]). Set even for thinned events, like
    /// `bed_style`; the bonk neither reads nor writes it.
    tone: Tone,
    /// Master duck envelope 0..1: snapped to 1 when a bonk is admitted,
    /// exponential decay (τ [`BONK_DUCK_TAU`]) per sample rendered. Applied
    /// as `1 − BONK_DUCK_DEPTH · duck` to every non-exempt voice and the bed
    /// — one multiply per frame, exactly 1.0 while at rest.
    duck: f32,
    /// Sing-along duck envelope 0..1: snapped to 1 when a
    /// [`CelebrationGesture::RiffBar`] is admitted, held for the bar
    /// (`sing_hold`, so the riff does not pump between bars), then released
    /// exponentially (τ [`SING_DUCK_TAU`]). Applied beside `duck` as
    /// `1 − DEPTH · sing`, exactly ×1.0 at rest.
    sing: f32,
    /// The song's key, borrowed by the typing: a pentatonic root offset
    /// latched by [`TrailSynth::latch_song_key`] from every riff bar through
    /// the same `celebration_root(sig)` the riff transposes by, and released
    /// with the sing duck. While the cat sings, typed notes share its key.
    song_key: i8,
    /// Seconds of full sing-duck hold remaining (block-rate countdown).
    sing_hold: f32,
    /// The signature of the last admitted riff bar, the key-switch damp's
    /// memory. It never shapes a bar; it only names
    /// whose tails are stale when the next bar's `sig` differs. Never cleared:
    /// a stale value can only damp celebration voices, and after the song
    /// there are none.
    last_riff_sig: Option<u32>,
    /// Voice steals since construction, a diagnostic for the tests, never read
    /// by the DSP. A steal means
    /// [`Self::claim`] cut a live voice short, an audible clipped tail.
    steals: u32,
    /// Test-only: voice births per lane, read by the lane pins.
    #[cfg(test)]
    lane_births: [u32; 16],
    /// The rainbow kitty melody state (§10.1): verse, chord loop, re-strike
    /// ladder, undo stack. Untouched until the first music-box event
    /// ([`Self::v2_engaged`]).
    v2: MelodyV2,
    /// Spawn ordinal source (see [`Voice::born`]), monotone from 0. Read only
    /// by the lane census and the meteor's bell-identity check.
    born_seq: u32,
    /// The synth's own clock, seconds since construction, advanced at block
    /// rate in [`Self::render`]: v2's melody timing falls back to it when
    /// `EventMeta::at_ms == 0`. `f64` because the melody reads whole
    /// milliseconds and a terminal runs for days. It advances only while the
    /// host renders, so a host that pauses its output stamps `at_ms`.
    clock_s: f64,
    /// The v2 latch (§9.7, §16 row 10): `true` from the first v2 trail event
    /// on, never cleared. It arms the bus limiter in [`Self::render`] and
    /// turns the sing-along's `song_key` snap into §10.2's word-boundary
    /// handback.
    v2_latched: bool,
    /// The key handback is pending (§10.2): the sing-along ended mid-word, so
    /// `song_key` is held until the next word boundary. Set only while
    /// [`Self::v2_latched`]; cleared by the handback, the next riff bar's
    /// latch, or the first v1 trail event, which takes v1's snap
    /// ([`Self::push_meta`]).
    v2_key_pending: bool,
    /// §9.7's bus peak limiter, two floats per channel: the peak follower
    /// (`lim_env_*`, instant attack, [`LIMIT_RELEASE_S`] release) and the
    /// smoothed gain (`lim_g_*`, [`LIMIT_ATTACK_S`] attack). Read only while
    /// [`Self::v2_latched`].
    lim_env_l: f32,
    lim_g_l: f32,
    lim_env_r: f32,
    lim_g_r: f32,
    /// DC blockers (one-pole highpass ~20 Hz) per channel.
    dc_x_l: f32,
    dc_y_l: f32,
    dc_x_r: f32,
    dc_y_r: f32,
}

/// Master output scale: a default-volume (0.4) Typed event peaks near
/// −21 dBFS, audible but far below alert level. [`palette_trim`] flattens the
/// palettes onto that floor. Tune overall loudness here.
const MASTER: f32 = 2.0;

/// Stereo column scale: a voice's pan is `col × 0.35` before the equal-power
/// split (§11). Shared by the meteor's re-aimed glide and every spawn.
const PAN_LAW_SCALE: f32 = 0.35;
/// The stereo law's clamp — never fully one-eared.
const PAN_LAW_CLAMP: f32 = 0.6;

/// The equal-power pan: `(gl, gr)` for a voice of `gain` at column pan `pan`
/// (−1..1, pre-law). One law for every spawn and the meteor's re-aimed glide
/// ([`rainbow_kitty_v2`]).
///
/// The sine's operand goes through `black_box` so an optimised build cannot
/// fuse `cos`/`sin` into one `sincosf` libcall, whose last bit differs from
/// the separate calls on Darwin; this keeps renders identical across
/// profiles. `black_box` is a hint, so this holds for the current toolchain
/// only; if it stops holding, keep the calls apart by stronger means rather
/// than pinning per profile.
#[inline]
fn pan_gains(gain: f32, pan: f32) -> (f32, f32) {
    let p = (pan * PAN_LAW_SCALE).clamp(-PAN_LAW_CLAMP, PAN_LAW_CLAMP);
    let a = (p + 1.0) * core::f32::consts::FRAC_PI_4;
    (gain * a.cos(), gain * core::hint::black_box(a).sin())
}

/// §9.7's bus peak limiter: threshold 0.2 linear (−14 dBFS) after `MASTER`,
/// before `soft_clip`; hard knee. Active only once [`TrailSynth::v2_latched`].
/// Below the threshold its gain is exactly 1.0, so it is bit-transparent.
const LIMIT_THRESHOLD: f32 = 0.2;
/// Gain-reduction attack, seconds (§9.7: 0.5 ms).
const LIMIT_ATTACK_S: f32 = 0.000_5;
/// Peak-follower release, seconds (§9.7: 80 ms).
const LIMIT_RELEASE_S: f32 = 0.080;

/// Governor: sustained admission gap for discrete voices, per event kind
/// pressure. ~45 ms ⇒ at most ~22 voices/s even under key repeat.
pub(crate) const MIN_GAP: f32 = 0.045;

/// Per-palette level trim, so the loudness ladder depends on the gesture, not
/// the look. Each value makes that style's `Typed` peak at the ladder floor
/// (−21 dBFS at gain 0.4 / heat 0.5, fitted by rendering).
///
/// Applied at palette dispatch only; gestures designed before it (Kill,
/// Glide, Sweep, Land, Bonk, the riff) are untouched. Kill's swoosh band is
/// style-tinted ([`kill_swoosh_band`]), so it keeps ~3 dB of spread.
fn palette_trim(voice: SoundVoice, style: GlowStyle) -> f32 {
    match voice {
        SoundVoice::Mech => 0.68,
        // A standalone style voice delegates to its style's trim.
        SoundVoice::Of(s) => palette_trim(SoundVoice::Style, s),
        // Sound-only voices, fitted on isolated peak at the host default
        // volume. A trim equalises peak only; check RMS and crest when
        // re-fitting, since a single scalar is not a brightness knob.
        SoundVoice::Typewriter => 0.5116,
        SoundVoice::Marimba => 0.9326,
        SoundVoice::Felt => 0.7385,
        // The music box's trim lives in its module
        // ([`rainbow_kitty_v2::KEY_TINE_TRIM`]); this arm and the rainbow
        // kitty look's are the unity identity for the unreachable v1 dispatch.
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

/// The kill swoosh's band `(hi, lo)`: the register the line-kill's noise falls
/// through. Designed kind-level so it is a swoosh in every palette, tinted by
/// the speaking voice. The match is exhaustive, so a new voice must choose a
/// tint. `Style` resolves the look's table; a standalone style voice takes
/// its own style's tint (`Of(Water)` under Lumen falls through water).
fn kill_swoosh_band(voice: SoundVoice, style: GlowStyle) -> (f32, f32) {
    match voice {
        // The music box uses the default swoosh (§10.4).
        SoundVoice::RainbowKittyV2 => (1600.0, 350.0),
        // The mech kill: a dull sweep down through the case register — a
        // hand brushing keys, not a musical fall.
        SoundVoice::Mech => (1000.0, 220.0),
        // The carriage lever thrown: dry paper-and-metal, brighter than the
        // keyboard's case but well under any glass.
        SoundVoice::Typewriter => (2200.0, 500.0),
        // A yarn mallet swept along the bars — the wooden mid.
        SoundVoice::Marimba => (1300.0, 300.0),
        // The felt's dampers brushed down the strings: the darkest fall.
        SoundVoice::Felt => (700.0, 160.0),
        SoundVoice::Of(s) => kill_swoosh_band(SoundVoice::Style, s),
        SoundVoice::Style => match style {
            GlowStyle::Water => (900.0, 250.0),
            GlowStyle::Fire => (1400.0, 300.0),
            GlowStyle::Sparkle => (2600.0, 700.0),
            // Comet lives in deep space (A3 transmissions) — its kill
            // swoosh falls dark with it, not through an icy top.
            GlowStyle::Comet => (1200.0, 280.0),
            _ => (1600.0, 350.0),
        },
    }
}

impl TrailSynth {
    pub fn new(sample_rate: f32, seed: u32) -> Self {
        Self {
            #[cfg(test)]
            ting_unducked: false,
            inv_sr: 1.0 / sample_rate.max(8_000.0),
            rng: seed | 1,
            voices: [Voice::default(); MAX_VOICES],
            bed: Bed::default(),
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
            // A session's first keystroke plays `SONG_THEME[0]`, the tonic;
            // `walk` starts at 2.
            theme_pos: 0,
            phrase_idx: 0,
            // The bass opens on the walk's root (C), so the first word boundary
            // of a session grounds the hook on its own tonic.
            bass_step: 0,
            shift_step: 0,
            space_run_len: 0,
            space_run_deg: 0,
            space_run_root_hz: 0.0,
            since_space_step: 1.0,
            since_shift: 1.0,
            // The bar opens on its downbeat, so a session's first keystroke is
            // an accent (the loudness ladder pins measure that level).
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

    /// Uniform in [0,1): [`xorshift01`] on the synth's stream.
    #[inline]
    fn rnd(&mut self) -> f32 {
        xorshift01(&mut self.rng)
    }

    /// Uniform in [lo, hi).
    #[inline]
    fn rnd_in(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.rnd()
    }

    /// True once every voice is dead, the bed has decayed and the DC blocker
    /// has settled: [`render`](Self::render) then writes exact zeros, so the
    /// host can pause its output queue.
    #[inline]
    pub fn is_quiet(&self) -> bool {
        // 1e-3 pre-gain bed level is far below audibility (the style
        // coefficients scale it down another 20+ dB); snapping to exact zero
        // there is inaudible and lets the host pause quickly.
        self.voices.iter().all(|v| !v.on)
            && self.bed.level < 1e-3
            && self.bed.energy < 1e-3
            // ...and the DC blocker has settled, so a stream that goes quiet
            // renders the same tail as one kept awake. `render` snaps its state
            // to exact zero below -120 dBFS, so this equality is reached.
            && self.dc_y_l == 0.0
            && self.dc_y_r == 0.0
    }

    /// Number of live voices (test/diagnostic hook).
    pub fn live_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.on).count()
    }

    /// Voice steals since construction (diagnostic). Nonzero means the voice
    /// pool ran dry and a live tail was cut.
    #[must_use]
    pub fn steals(&self) -> u32 {
        self.steals
    }

    /// Diagnostic: (bed energy, bed level) — demo/tuning hook.
    pub fn debug_bed(&self) -> (f32, f32) {
        (self.bed.energy, self.bed.level)
    }

    // -- event intake -------------------------------------------------------

    /// Push one gesture. Applies the rate governor, feeds the bed (trail
    /// gestures only), and (if admitted) designs the gesture's voice(s).
    /// O(MAX_VOICES), no alloc.
    #[inline]
    pub fn push(&mut self, ev: SoundEvent) {
        self.push_meta(ev, EventMeta::default());
    }

    /// Test-only: account a pause the render clock did not see (§10.1, §16
    /// row 7). [`Self::clock_s`] advances only inside [`Self::render`], so a
    /// paused output queue freezes it and every think-pause would read as the
    /// same short gap; hosts stamp `at_ms` instead. The v1 governor clocks
    /// advance too. Non-finite or negative input is ignored.
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

    /// Release the song's key: v1 snaps; once the music box has spoken,
    /// §10.2's handback holds `song_key` until the next word boundary
    /// (`rainbow_kitty_v2`'s `v2_hand_back_key`). A v1 trail event arriving
    /// first snaps instead ([`Self::push_meta`]).
    fn release_song_key(&mut self) {
        if self.v2_latched {
            self.v2_key_pending = self.song_key != 0;
        } else {
            self.song_key = 0;
        }
    }

    /// Whether this event belongs to the music box (§16 row 9).
    ///
    /// Yes when the event names it ([`SoundVoice::RainbowKittyV2`] or
    /// `Of(GlowStyle::RainbowKitty)`), or follows the look
    /// ([`SoundVoice::Style`]) and the look is the rainbow kitty. A named
    /// instrument always plays itself. Decided per event, with no latch.
    ///
    /// Trail gestures only: the bonk, the sing-along riff and the output pips
    /// stay on the v1 chain even under the music box (§11).
    #[inline]
    fn v2_engaged(&self, ev: &SoundEvent) -> bool {
        // The verdict is the one non-trail gesture the music box voices; its
        // cadence uses v2's lattice (`CHORD_LOOP`, `TINE_BASE_HZ`), so it forks
        // before palette dispatch too.
        matches!(
            ev.kind,
            SoundGesture::Trail(_) | SoundGesture::Output(OutputGesture::Verdict { .. })
        ) && match ev.voice {
            SoundVoice::RainbowKittyV2 | SoundVoice::Of(GlowStyle::RainbowKitty) => true,
            SoundVoice::Style => ev.style == GlowStyle::RainbowKitty,
            _ => false,
        }
    }

    /// [`Self::push`] with the v2 side-car (§16 rows 6-8). Hosts that have
    /// nothing to stamp call `push`; the keyed seam calls this.
    pub fn push_meta(&mut self, ev: SoundEvent, meta: EventMeta) {
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
            // Flow heat is only a lerp parameter; outside 0..1 it would
            // extrapolate.
            flow: meta.flow.clamp(0.0, 1.0),
            // The host clamps to one block; this bound only stops a
            // mis-stamped cue from scheduling far into the future.
            block_lead_s: meta.block_lead_s.clamp(0.0, BLOCK_LEAD_MAX_S),
            ..meta
        };
        // The pre-roll applies to this push only (§3.4 c): `spawn_seeded`
        // reads it, and it is cleared on every exit so render-time spawns
        // never inherit it.
        self.block_lead_s = meta.block_lead_s;
        self.push_meta_latched(ev, meta);
        self.block_lead_s = 0.0;
    }

    /// The body of [`Self::push_meta`] after the boundary filter, with the
    /// block pre-roll already latched on `self`.
    fn push_meta_latched(&mut self, ev: SoundEvent, meta: EventMeta) {
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
        // Rainbow kitty v2 forks here (§16 row 9): after the non-finite filter,
        // before the governor, `advance_song` and `design_trail`. v2 has its
        // own loudness law (the §9.6 IOI arc instead of the flood duck), its
        // own melody (`MelodyV2`) and its own admission (per-lane caps), so it
        // never touches the arithmetic the v1 palettes are pinned on.
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
        // The strum belongs to the music box alone (RAINBOW-KITTY-V2.md §28):
        // under any other voice a paste keeps its echo's Jump, so the gesture
        // is dropped before the governor, bed or melody see it
        // (`the_remainder_past_the_cap_is_one_strum_not_a_verse`).
        if matches!(ev.kind, SoundGesture::Trail(SoundKind::Strum)) {
            return;
        }
        // A v1 trail event resolves a pending handback by snapping. The
        // handback is detected only on the v2 path while the voice is chosen
        // per event, so after switching from the music box to another voice
        // `song_key` would otherwise stay transposed forever. Trail only: a
        // bonk or output pip must not snap a key v2 is holding (§10.2).
        if self.v2_key_pending && matches!(ev.kind, SoundGesture::Trail(_)) {
            self.song_key = 0;
            self.v2_key_pending = false;
        }
        // The typing pause since the previous event, captured before the
        // governor resets `since_event`; `advance_melody` reads it as a phrase
        // boundary.
        let pause = self.since_event;
        // Governor bookkeeping: exponential rate estimate over ~0.6 s. Every
        // source pays in, so notes right after a bonk are ducked too.
        self.rate = self.rate * (-self.since_event / 0.6).exp() + 1.0;
        self.since_event = 0.0;
        // Bed: every trail event feeds energy, admitted or not, so fast typing
        // melts into the texture. Word gestures skip the bed feed entirely.
        if let SoundGesture::Trail(kind) = ev.kind {
            // Style and tone follow the trail stream unconditionally (thinned
            // and bed-off events included), so a re-enabled bed wakes in the
            // current style.
            self.bed_style = ev.style;
            self.bed_voice = ev.voice;
            self.tone = ev.tone;
            // The whitespace run follows the text, not the audio: a thinned
            // space still ends its run. A bare Shift leaves the run untouched.
            match kind {
                SoundKind::Space => {
                    self.space_head = !self.space_run;
                    self.space_run = true;
                    // The run's length follows the text too, so the fourth
                    // space of an indent is the fourth degree either way.
                    self.space_run_len = if self.space_head {
                        0
                    } else {
                        self.space_run_len.saturating_add(1)
                    };
                    if self.space_head {
                        // The word boundary is the bar line: resetting the bar
                        // here makes each word's first letter an accent, with
                        // the space as the bass note under it. Typing speed
                        // still sets the tempo. Only the run head does it, so
                        // an indent re-bars once.
                        self.song_pulse = 0;
                    }
                }
                SoundKind::Shift => {}
                _ => self.space_run = false,
            }
            // `ev.bed` (the `trail_sound_bed` setting) gates the energy feed:
            // unfed, the bed's level stays at its exact-zero floor and emits
            // nothing. Turning it off mid-breath lets the live bed decay
            // normally.
            if ev.bed {
                self.kick_bed(kind, ev.gain);
            }
        }

        // Ducking: louder alone, softer in a flood. 1/sqrt keeps perceived
        // per-second loudness roughly flat as rate climbs.
        let duck = 1.0 / (1.0 + 0.55 * self.rate).sqrt();
        // Min-gap thinning for the discrete layer. Jumps always speak, and the
        // bonk is never thinned.
        let bypass = matches!(
            ev.kind,
            SoundGesture::Trail(SoundKind::Jump)
                // A Sweep carries a whole pre-delayed run, so it bypasses the
                // gap like a Jump. A Glide stays gap-thinned so a held arrow
                // cannot machine-gun.
                | SoundGesture::Trail(SoundKind::Sweep { .. })
                // The v2 engine's twins of Sweep and Jump. The rainbow kitty
                // engine follows the look while the synth follows the voice, so
                // a named instrument under that look receives `Navigation` /
                // `Meteor` here; they are rate-limited upstream by the glow's
                // one-cue-per-move rule.
                | SoundGesture::Trail(SoundKind::Navigation)
                | SoundGesture::Trail(SoundKind::Meteor { .. })
              // A landing always speaks, like a Jump.
                | SoundGesture::Trail(SoundKind::Land)
              // The smoke puff's rate limit is the visual one (`cursor_glow`'s
              // POOF_MIN_GAP), so the keystroke gap does not thin it.
                | SoundGesture::Trail(SoundKind::Poof)
              // The Shift ting is admitted on its own clock in the `shift`
              // arm below, which `admit` checks before `bypass`.
                | SoundGesture::Words(WordGesture::Bonk)
                // A riff bar is one event per ~1.6 s carrying a whole phrase,
                // so thinning it would silence entire bars.
                | SoundGesture::Celebration(_)
                // An output cue is governed once per episode and not retried,
                // so it is never thinned here. It still does not claim
                // `since_voice` below, so it cannot steal the next key's slot.
                | SoundGesture::Output(_)
        );
        // The erase gate: a deletion is thinned only against other deletions
        // ([`ERASE_MIN_GAP`]).
        let erase = ev.kind == SoundGesture::Trail(SoundKind::Backspace);
        // The ting's gate: a bare Shift is thinned only against other bare
        // Shifts ([`SHIFT_MIN_GAP`]), never by the keystroke gap, since it
        // lands within one MIN_GAP of the letter before its capital.
        let shift = ev.kind == SoundGesture::Trail(SoundKind::Shift);
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
        // Admissions that do not claim the shared beat: the Shift ting (it
        // would thin the capital it announces), the erase poof (gated on its
        // own clock; claiming would thin the letter after a correction) and
        // the smoke puff (an accompaniment, which would thin the next key).
        // Every other admission resets the clock.
        if erase {
            // Held-run bookkeeping, before the clock resets: the gap back to
            // the previous admitted deletion tells auto-repeat from single
            // corrections ([`HELD_ERASE_RUN_WINDOW`]).
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
                // The nav tick does not claim the beat either: a quiet aside
                // must not thin the letter typed after it.
                | SoundGesture::Trail(SoundKind::Navigation)
                // An output pip is unauthored; letting it claim the beat would
                // let the machine thin the human's next keystroke.
                | SoundGesture::Output(_)
        ) {
            self.since_voice = 0.0;
        }
        // Any authored admission expires a held delete run's pending release
        // shimmer (an erase reschedules its own, so only the run's last one
        // survives). The puff and the Shift lift are accompaniments and leave
        // it alone; the puff arrives a frame after its own deletion.
        if !matches!(
            ev.kind,
            SoundGesture::Trail(SoundKind::Shift)
                | SoundGesture::Trail(SoundKind::Poof)
                // Output landing mid-run must not kill the user's shimmer.
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
                        // A deletion undoes a character, so the song does not
                        // step past the vanished letter's note (and the poof is
                        // unpitched anyway).
                        | SoundKind::Backspace
                        // A word kill is a deletion at word scale.
                        | SoundKind::KillWord
                        // The puff is an accompaniment riding a deletion.
                        | SoundKind::Poof
                ) {
                    self.advance_song(kind, pause);
                }
                self.design_trail(ev, kind, duck, pause);
            }
            SoundGesture::Words(WordGesture::Bonk) => self.design_bonk(ev, duck),
            // Prism wake: designed at kind level (no `palette_trim`) and spawned
            // seeded, so machine output never consumes the typing melody's rng
            // draws.
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

    /// Place one composing gesture in the bar ([`SONG_PULSE`]) and, on an
    /// accent, step the theme under it.
    ///
    /// Only a keystroke can be a ghost; Enter, a kill or a jump always lands at
    /// full level on a fresh note. Enter and a long typing pause reset the bar
    /// to its downbeat, so a new phrase opens on an accent.
    fn advance_song(&mut self, kind: SoundKind, pause: f32) {
        let boundary = matches!(kind, SoundKind::Jump) || pause > PHRASE_PAUSE_S;
        if boundary {
            self.song_pulse = 0;
        }
        // Punctuation never ghosts, and after a boundary the bar is at its
        // downbeat (an accent), so both paths agree.
        let slot = if matches!(kind, SoundKind::Typed) {
            let s = SONG_PULSE[usize::from(self.song_pulse)];
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

    /// Play the next note of the theme, setting [`Self::walk`] to its degree.
    ///
    /// [`SONG_THEME`] plays in order, one degree per accent, wrapping at 28.
    /// Nothing is drawn, so the melody is a pure function of the accents
    /// struck and where the phrase marks fell. `pause` is the typing gap
    /// before this event.
    ///
    /// A phrase boundary (Enter, a comma-length gap, or the phrase running out)
    /// opens the next phrase, so a rest advances the form rather than
    /// rewinding it; the boundary note is the closing phrase's last degree.
    /// The per-tone lean transposes the whole line; pitch flows through
    /// [`Self::melody_hz`].
    fn advance_melody(&mut self, kind: SoundKind, pause: f32) {
        let (lo, hi) = tone_register(self.tone);
        let start = usize::from(SONG_FORM[usize::from(self.phrase_idx)]);
        let end = usize::from(SONG_FORM[usize::from(self.phrase_idx) + 1]);
        let pos = usize::from(self.theme_pos);
        // The phrase ran out: its last note already sounded, so roll into the
        // next phrase's first note rather than repeat it.
        let ran_out = pos >= end;
        // A rest (Enter or a comma-length gap) cuts the phrase short and
        // cadences on its final degree. Not at a phrase's first note: there
        // is nothing to close, and closing would skip a phrase per pause.
        let rest = (matches!(kind, SoundKind::Jump) || pause > PHRASE_PAUSE_S) && pos > start;
        if rest && !ran_out {
            self.walk = fold_register(
                i32::from(SONG_THEME[end - 1]) + melody_lean(self.tone),
                lo,
                hi,
            );
            self.open_next_phrase();
            return;
        }
        if ran_out {
            self.open_next_phrase();
        }
        self.walk = fold_register(
            i32::from(SONG_THEME[usize::from(self.theme_pos)]) + melody_lean(self.tone),
            lo,
            hi,
        );
        self.theme_pos += 1;
    }

    /// The bass root for walk position `step` under `anchor`: the walk's
    /// degree on the tone's lattice (song key included), octave-folded into
    /// the bass register `[2·SPACE_BASS_LO_HZ, 4·SPACE_BASS_LO_HZ)`.
    ///
    /// Folding the walked pitch (not just the root) keeps every note of the
    /// walk in the band for every palette and song key, and keeps consecutive
    /// roots within an octave.
    /// At position 0 with no song key it equals `bass_octave(anchor) * 2`.
    fn space_root_hz(&self, anchor: f32, step: u8) -> f32 {
        let deg = i32::from(SONG_BASS[usize::from(step)]) + i32::from(self.song_key);
        bass_octave(self.melody_hz(anchor, deg)) * 2.0
    }

    /// Hand the playhead to the next phrase of the form (answer after hook,
    /// bridge after answer, hook after the half-cadence); never a rewind.
    fn open_next_phrase(&mut self) {
        self.phrase_idx = (self.phrase_idx + 1) % (SONG_FORM.len() as u8 - 1);
        self.theme_pos = SONG_FORM[usize::from(self.phrase_idx)];
    }

    /// The melody's pitch lattice under the current tone: `base` scaled to
    /// `degree` of the tone's table, times the tone's transpose. Under
    /// [`Tone::Technical`]/[`Tone::Calm`] this equals [`penta`] bit for bit.
    /// The bonk calls [`penta`] directly: its clash is anchored to the
    /// untransposed lattice.
    fn melody_hz(&self, base: f32, degree: i32) -> f32 {
        let (table, transpose) = tone_tables(self.tone);
        let oct = degree.div_euclid(5);
        let step = degree.rem_euclid(5) as usize;
        base * transpose * table[step] * (2.0_f32).powi(oct)
    }

    /// Claim a voice slot in a lane (`RAINBOW-KITTY-V2.md` §16 delta 4, §14),
    /// v2's per-lane polyphony law. [`LANE_NONE`] forwards straight to
    /// [`Self::claim`] with no census. For a real lane:
    /// - the caps sum to 25 of 28 slots, so a lane under its cap is always
    ///   admitted without a steal;
    /// - a full lane fade-steals its oldest member over [`LANE_FADE_STEAL_S`]
    ///   and the newcomer takes another slot;
    /// - unless that member is younger than [`LANE_AGE_GUARD_S`] and the lane
    ///   can afford a loss (glint, bloom): then the newcomer is dropped. A tune
    ///   voice always speaks.
    ///
    /// Voices already damping are neither counted nor re-stolen.
    ///
    /// A `deferred` (pre-delayed) newcomer takes a slot now and its lane at
    /// its onset ([`Self::lane_onset_steal`]): a voice at `t < 0` has not
    /// sounded yet, so it must not evict one that has.
    fn claim_lane(&mut self, lane: u8, deferred: bool) -> Option<usize> {
        if lane == LANE_NONE {
            return Some(self.claim());
        }
        if !deferred && !self.lane_make_room(lane, MAX_VOICES) {
            return None;
        }
        // A decoration never steals from a full pool: `claim`'s pool steal
        // takes the quietest voice, which under a burst can be the tune voice
        // it decorates. Lanes that drop newcomers under the age guard drop
        // them under pool exhaustion too.
        if lane_drops_the_newcomer(lane) && self.voices.iter().all(|v| v.on) {
            return None;
        }
        Some(self.claim())
    }

    /// The lane census (§14): counts the sounding voices of `lane` (on, not
    /// damping, past their pre-delay) other than slot `skip`; at the cap it
    /// fade-steals the oldest over [`LANE_FADE_STEAL_S`], unless the age guard
    /// applies to a lane that drops newcomers, in which case it returns
    /// `false` and the newcomer does not get the lane.
    fn lane_make_room(&mut self, lane: u8, skip: usize) -> bool {
        let cap = lane_cap(lane);
        let mut live = 0usize;
        let mut oldest = None;
        let mut oldest_born = u32::MAX;
        for (i, v) in self.voices.iter().enumerate() {
            if i == skip || !v.on || v.lane != lane || v.damp > 0.0 || v.t < 0.0 {
                continue;
            }
            live += 1;
            if v.born < oldest_born {
                oldest_born = v.born;
                oldest = Some(i);
            }
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

    /// A pre-delayed laned voice takes its lane at its onset (§14, §12.2):
    /// called by [`Self::render`] on the first sounding sample of slot `vi`.
    /// Returns `true` when the newcomer was dropped by the age guard and must
    /// be switched off unheard.
    fn lane_onset_steal(&mut self, vi: usize) -> bool {
        let lane = self.voices[vi].lane;
        !self.lane_make_room(lane, vi)
    }

    /// Claim a voice slot: first free, else steal the quietest.
    fn claim(&mut self) -> usize {
        let mut best = 0;
        let mut best_w = f32::MAX;
        for (i, v) in self.voices.iter().enumerate() {
            if !v.on {
                return i;
            }
            let w = v.weight();
            if w < best_w {
                best_w = w;
                best = i;
            }
        }
        // Nothing was free: `best` is a live voice about to be cut short.
        self.steals = self.steals.saturating_add(1);
        best
    }

    /// Spawn one voice from a prototype: applies pan narrowing + equal-power
    /// law, resets runtime state, randomizes oscillator phases.
    fn spawn(&mut self, proto: Voice, gain: f32, pan: f32) -> Option<usize> {
        // Draw order (tremolo phase, then the three partials) is pinned by the
        // golden streams.
        let tw_ph = self.rnd();
        let ph = [self.rnd(), self.rnd(), self.rnd()];
        self.spawn_seeded(proto, gain, pan, tw_ph, ph)
    }

    /// [`Self::spawn`] with the oscillator phases handed in rather than drawn,
    /// for noise-only kind-level layers (fixed zero phases). It consumes no
    /// rng, so the palette voices after it draw the same stream as without it.
    fn spawn_seeded(
        &mut self,
        proto: Voice,
        gain: f32,
        pan: f32,
        tw_ph: f32,
        ph: [f32; 3],
    ) -> Option<usize> {
        // Lane 0 forwards to `claim` and reads no random state. A v2 spawn its
        // lane cannot admit (§14's age guard) returns `None` and spawns
        // nothing. A pre-delayed v2 voice is censused at its onset in `render`
        // (`lane_onset_steal`), so it cannot cut a sounding voice early.
        let idx = self.claim_lane(proto.lane, proto.delay > 0.0)?;
        #[cfg(test)]
        {
            let l = usize::from(proto.lane).min(15);
            self.lane_births[l] = self.lane_births[l].saturating_add(1);
        }
        let mut v = proto;
        // Tone tempo-feel times the bar's ghost-length multiplier
        // (`SONG_GHOST_FEEL`): one multiplier on length, decay and flourish
        // spacing. Bonk voices are exempt via `duck_exempt`, and a product of
        // exactly 1.0 skips the multiplies.
        let feel = tone_feel(self.tone) * self.song_feel;
        if !v.duck_exempt && feel != 1.0 {
            v.dur *= feel;
            v.decay *= feel;
            // A laned (v2) voice's pre-delay is never scaled by mood: v2
            // pre-delays are shared with the pixels (the flight clock, rain
            // spacing), and §10.4 lets `tone_feel` scale decay only.
            if v.lane == LANE_NONE {
                v.delay *= feel;
            }
        }
        // A capital's lowpass rises with its note: inside a shifted glyph's
        // dispatch every tonal voice's roof is lifted by [`shift_ratio`], so
        // the weight ([`SHIFT_GLYPH_GAIN`]) survives at every degree. Noise-only
        // voices keep their band. Exactly 1.0 outside that dispatch.
        if self.shift_octave != 1.0 && v.p.iter().any(|p| p.lvl > 0.0) {
            v.lp_cut *= self.shift_octave;
            // ...and the lift moves the note, not its shimmer. A partial within
            // [`DETUNE_MAX_RATIO`] of another is a chorus twin whose beat rate
            // scales with the note; its offset is divided by the octave so the
            // capital beats like the plain letter. Harmonics are outside the
            // window and untouched.
            for i in 0..v.p.len() {
                for j in 0..v.p.len() {
                    if i == j || v.p[i].lvl <= 0.0 || v.p[j].lvl <= 0.0 {
                        continue;
                    }
                    let r0 = v.p[j].f0 / v.p[i].f0;
                    if r0 > 1.0 && r0 < DETUNE_MAX_RATIO {
                        v.p[j].f0 = v.p[i].f0 * (1.0 + (r0 - 1.0) / self.shift_octave);
                    }
                    let r1 = if v.p[i].f1 > 0.0 {
                        v.p[j].f1 / v.p[i].f1
                    } else {
                        0.0
                    };
                    if r1 > 1.0 && r1 < DETUNE_MAX_RATIO {
                        v.p[j].f1 = v.p[i].f1 * (1.0 + (r1 - 1.0) / self.shift_octave);
                    }
                }
            }
        }
        v.on = true;
        // Delay is negative onset time plus the cue's block pre-roll
        // ([`EventMeta::block_lead_s`]), so each voice sounds a constant one
        // block after arrival. Applied to `t`, not `delay`: `delay > 0` marks
        // a voice for the onset lane census, and a lead must not re-census it.
        v.t = -(v.delay + self.block_lead_s);
        (v.gl, v.gr) = pan_gains(gain, pan);
        // Spawn ordinal: monotone, never recycled. Saturating, so it cannot
        // panic; a saturated counter only stops ordering.
        self.born_seq = self.born_seq.saturating_add(1);
        v.born = self.born_seq;
        // The glide's far end through the same equal-power law; untaken when
        // `pan_glide_s == 0.0` (every voice outside the meteor).
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
        // Geometric steps: each exponential in the sample loop follows
        // `E_{n+1} = E_n · e^(-dt/τ)`, one multiply per sample. Computed after
        // the tone-feel multiplies, which move `decay`. Seeding happens at the
        // first sounding sample (`env_run`).
        let dt = self.inv_sr;
        v.k_a = (-dt / v.attack.max(2e-4)).exp();
        v.k_d = (-dt / v.decay.max(1e-3)).exp();
        v.k_n = if v.n_glide > 0.0 {
            (-dt / v.n_glide).exp()
        } else {
            1.0
        };
        v.env_run = false;
        v.env_n = 0;
        v.k_nd = if v.n_decay > 0.0 {
            (-dt / v.n_decay).exp()
        } else {
            1.0
        };
        for (part, ph) in v.p.iter_mut().zip(ph) {
            part.ph = ph;
            part.k_g = if part.glide > 0.0 {
                (-dt / part.glide).exp()
            } else {
                1.0
            };
            part.k_f = if part.fm_ratio > 0.0 {
                (-dt / part.fm_tau.max(1e-3)).exp()
            } else {
                1.0
            };
            part.k_p = if part.decay > 0.0 {
                (-dt / part.decay).exp()
            } else {
                1.0
            };
        }
        v.lp = 0.0;
        v.n_lp = 0.0;
        v.n_bp = 0.0;
        // Render-loop invariants, resolved once. The render loop never writes
        // `lp_cut`/`n_q`/`n_f0`/`n_glide`/`n_lvl`.
        v.lp_k = (v.lp_cut * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        if v.n_lvl > 0.0 {
            v.n_damp = 1.0 / v.n_q.max(0.3);
            if v.n_glide <= 0.0 {
                // Constant cutoff ⇒ constant SVF coefficient and divisor.
                v.svf_g = (core::f32::consts::PI * (v.n_f0 * dt).min(0.45)).tan();
                v.svf_den = 1.0 + v.svf_g * (v.svf_g + v.n_damp);
            }
        }
        self.voices[idx] = v;
        Some(idx)
    }

    // -- kind-level + per-palette sound design ------------------------------

    /// Design and spawn the voice(s) for one admitted trail gesture: the
    /// kind-level shaping shared by all styles, the style-agnostic Kill
    /// swoosh, then the per-palette dispatch (each [`Palette`] implementor is
    /// its own sound designer).
    fn design_trail(&mut self, ev: SoundEvent, kind: SoundKind, duck: f32, pause: f32) {
        // Heat warms level slightly (+45 % at full blaze) — presence, not
        // a volume ride.
        let g = ev.gain * duck * (0.55 + 0.45 * ev.heat);
        // Kind-level shaping shared by all styles.
        let kg = match kind {
            // Tier 1 — per character: the floor.
            SoundKind::Typed => TYPED_KIND_GAIN,
            SoundKind::Backspace => BACKSPACE_KIND_GAIN,
            SoundKind::Space => SPACE_KIND_GAIN,
            SoundKind::Glide { .. } => GLIDE_KIND_GAIN, // Tier 0, under the floor.
            // Tier 1, at the Space's gain: intent, not authorship.
            SoundKind::Shift => SHIFT_KIND_GAIN,
            // Under the floor: accompaniment, not authorship.
            SoundKind::Poof => POOF_KIND_GAIN,
            // Tier 0 — cursor motion, under the floor.
            SoundKind::Navigation => NAVIGATION_KIND_GAIN,
            SoundKind::Sweep { .. } => SWEEP_KIND_GAIN,
            // Tier 2.5 — per word: the poof one size up, under the line kill.
            SoundKind::KillWord => KILLWORD_KIND_GAIN,
            // Tier 3 — per line / command.
            SoundKind::Kill => KILL_KIND_GAIN,
            SoundKind::Jump => JUMP_KIND_GAIN,
            // Tier 4 — the rare spectacle.
            SoundKind::Land => LAND_KIND_GAIN,
            // Unreachable: `push` forks v2 kinds to `push_v2`. The arms keep
            // the ladder total and name each v2 gesture's tier.
            SoundKind::MeteorArm { .. } => METEOR_ARM_KIND_GAIN,
            SoundKind::Meteor { .. } | SoundKind::Enter { .. } | SoundKind::Strum => JUMP_KIND_GAIN,
            SoundKind::Stardust { .. } => POOF_KIND_GAIN,
        };
        // The capital's weight is applied once here, after the kind gain and
        // before any palette reads `g`. Exactly ×1.0 on an unshifted key
        // ([`shift_gain`]).
        let g = g * kg * shift_gain(&ev, kind);
        // The column only nudges the melody by ±1 degree.
        let col_off = (ev.pan).round() as i32;
        // In the song's key while the song plays (`song_key`, zero otherwise),
        // plus the family offset (`gesture_shape`): a character move one step
        // in its direction, a word move striding out.
        let shape = gesture_shape(kind);
        // The ghost offset applies to keystrokes only: cursor motion, landings
        // and jumps play the melody's own degree whatever the bar is doing.
        let ghosting = kind == SoundKind::Typed && !self.song_accent;
        // A shifted glyph sings its note an octave up ([`shift_lift`]) without
        // touching `self.walk`, so spelling cannot move the theme.
        let deg = self.walk
            + col_off
            + i32::from(self.song_key)
            + shape.offset
            + if ghosting {
                i32::from(self.song_ghost)
            } else {
                0
            }
            + shift_lift(&ev, kind);

        // Cursor movement (Glide/Sweep) is designed once here, before palette
        // dispatch, relative to the melody's current degree. `g` already
        // carries its kind gain, heat warmth and flood duck.
        if matches!(kind, SoundKind::Glide { .. } | SoundKind::Sweep { .. }) {
            self.design_cursor(&ev, shape, deg, g);
            return;
        }

        // The landing is style-agnostic, designed before palette dispatch.
        // `g` already carries LAND_KIND_GAIN, heat warmth and the flood duck.
        if kind == SoundKind::Land {
            self.design_land(&ev, g);
            return;
        }

        // The erase poof is terminal: it returns before palette dispatch, so
        // no style lays a pitched note under the puff.
        if kind == SoundKind::Backspace {
            self.design_erase_poof(&ev, g);
            return;
        }

        // The word poof: the deletion family at word scale, also terminal.
        if kind == SoundKind::KillWord {
            self.design_word_poof(&ev, g);
            return;
        }

        // The downbeat and the lift are style-agnostic, each borrowing the
        // palette's register through `anchor_hz`.
        if kind == SoundKind::Space {
            self.design_space(&ev, g, pause);
            return;
        }
        if kind == SoundKind::Shift {
            self.design_shift(&ev, deg, g);
            return;
        }

        // A kill is a style-tinted downward swoosh for every palette: soft
        // noise falling through the style's register. Designed once here.
        if kind == SoundKind::Kill {
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
                ..Voice::default()
            };
            // Pure band-passed noise peaks ~17 dB under a tonal voice at the
            // same gain, so it is compensated here rather than in
            // `KILL_KIND_GAIN`, which must stay under `BONK_KIND_GAIN`.
            self.spawn(v, g * 2.6, ev.pan);
            return;
        }

        // The smoke puff, style-agnostic like the kill swoosh.
        if kind == SoundKind::Poof {
            self.design_poof(&ev, g);
            return;
        }

        // The palette trim lands this style's keystroke on the ladder floor,
        // so the kind-gain tiers mean the same dB in every style.
        let g = g * palette_trim(ev.voice, ev.style);
        // A ghost is quieter and shorter than its accent. The length rides
        // `spawn`'s feel multiply, set and cleared around this one dispatch.
        let g = if ghosting { g * SONG_GHOST_LEVEL } else { g };
        self.song_feel = if ghosting { SONG_GHOST_FEEL } else { 1.0 };
        // A capital's roof rises with its note and its twins keep their beat
        // (`shift_octave`, read in `spawn_seeded`); 1.0 on an unshifted key.
        self.shift_octave = shift_ratio(&ev, kind);
        palette_for(ev.voice, ev.style).design(self, &ev, kind, g, deg, col_off);
        self.song_feel = 1.0;
        self.shift_octave = 1.0;
    }

    /// The cursor-movement gestures, built from the same [`gesture_shape`]
    /// rule as typing and deletion, so a word motion is a character motion
    /// at word scale:
    ///
    /// - Register: [`Self::melody_hz`] at the palette's [`Palette::anchor_hz`].
    /// - Degree: `deg` carries the melody degree, column nudge, song key and
    ///   shape offset.
    /// - Contour: [`gesture_bend`] on `dir`, forward leaning up, backward
    ///   leaning down.
    /// - Scale: `shape.notes` tones striding `shape.step` degrees,
    ///   [`CURSOR_SWEEP_STEP_S`] apart and tapering; the first has
    ///   `delay = 0`.
    ///
    /// A soft sine pluck, under the typing floor (tier 0).
    fn design_cursor(&mut self, ev: &SoundEvent, shape: GestureShape, deg: i32, g: f32) {
        let anchor = palette_for(ev.voice, ev.style).anchor_hz();
        for i in 0..shape.notes {
            let f = self.melody_hz(anchor, deg + shape.step * i as i32);
            let (f0, f1) = gesture_bend(f, shape.dir);
            let v = Voice {
                // First note immediate; the rest follow at a steady spacing,
                // which rate-limits the run.
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
                        ..Partial::default()
                    },
                    // A whisper of sub-octave body so the tone reads as warm,
                    // not as a beep.
                    Partial {
                        lvl: 0.12,
                        f0: f0 * 0.5,
                        f1: f1 * 0.5,
                        glide: GESTURE_BEND_TAU,
                        ..Partial::default()
                    },
                    Partial::default(),
                ],
                lp_cut: 2200.0,
                ..Voice::default()
            };
            // A single-note move keeps the character level; a run tapers so it
            // lands.
            let level = if shape.notes == 1 {
                0.5
            } else {
                0.45 * (1.0 - 0.15 * i as f32)
            };
            self.spawn(v, g * level, ev.pan);
        }
    }

    /// The cursor-landing star chime: three in-key star tones
    /// [`LAND_STAR_DEGREE_STEP`] degrees apart in [`LAND_ANCHOR_HZ`]'s register,
    /// alternating either side of the landing column [`LAND_STAR_STEP_S`]
    /// apart, over a soft arrival body with a whisper of air. The first star
    /// has `delay = 0`.
    ///
    /// Pitched through [`Self::melody_hz`] at the current degree, so it is
    /// consonant under every [`crate::tone::Tone`]. Not duck-exempt.
    fn design_land(&mut self, ev: &SoundEvent, g: f32) {
        for i in 0..LAND_STARS {
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
                        ..Partial::default()
                    },
                    Partial {
                        lvl: 0.2,
                        f0: f * 0.5,
                        f1: f * 0.5,
                        ..Partial::default()
                    },
                    Partial {
                        lvl: 0.1,
                        f0: f * 2.0,
                        f1: f * 2.0,
                        ..Partial::default()
                    },
                ],
                tw_rate: 11.0,
                tw_depth: 0.35,
                lp_cut: 4200.0,
                ..Voice::default()
            };
            // The stars scatter off the landing, alternating sides.
            let pan = ev.pan + 0.22 * if i % 2 == 0 { -1.0 } else { 1.0 };
            self.spawn(v, g * (0.5 - 0.12 * i as f32), pan);
        }
        // The arrival: a round low tonic with a breath of air — the thing that
        // makes the stars read as landing rather than merely twinkling.
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
                    ..Partial::default()
                },
                Partial::default(),
                Partial::default(),
            ],
            n_lvl: 0.2,
            n_f0: 2600.0,
            n_f1: 700.0,
            n_glide: 0.05,
            n_q: 0.8,
            lp_cut: 2000.0,
            ..Voice::default()
        };
        self.spawn(body, g * 0.3, ev.pan);
    }

    /// The spacebar's bass: one short, round, centred root per word, walking
    /// [`SONG_BASS`] in the bass register ([`SPACE_BASS_LO_HZ`]), with a breath
    /// of top ([`SPACE_AIR_LEVEL`]) so it does not read as a thud. Monophonic
    /// ([`SPACE_DAMP_S`]); only a whitespace run's head gets a root, the tail
    /// answers with air and an indent step. No [`gesture_bend`] scoop, and
    /// centred because hard-panned lows read as a defect.
    fn design_space(&mut self, ev: &SoundEvent, g: f32, pause: f32) {
        let anchor = palette_for(ev.voice, ev.style).anchor_hz();
        // The breath — the exhale between words. Both the head and the run's
        // tail wear it; it is the whole voice of a coalesced space.
        let breath = |lvl: f32| Voice {
            dur: 0.11,
            attack: 0.008,
            decay: 0.055,
            n_lvl: lvl,
            n_f0: 900.0,
            n_f1: 380.0,
            n_glide: 0.08,
            n_q: 0.7,
            lp_cut: 1400.0,
            ..Voice::default()
        };
        if !self.space_head {
            self.spawn_seeded(
                breath(0.05),
                g * SPACE_VOICE_LEVEL * SPACE_RUN_BREATH_LEVEL,
                ev.pan,
                0.0,
                [0.0; 3],
            );
            // The indent step (the `SPACE_STEP_*` constants): one lattice
            // degree over the run's root per extra space, as a ratio off the
            // root that sounded, clamped under [`SPACE_STEP_HI_HZ`]. On its own
            // admission clock so a held spacebar cannot machine-gun it; seeded,
            // so indent depth never shifts the melody's rng stream.
            if self.since_space_step >= SPACE_STEP_MIN_GAP && self.space_run_root_hz > 0.0 {
                self.since_space_step = 0.0;
                let deg = self.space_run_deg;
                let root = self.melody_hz(anchor, deg);
                // The figure climbs then holds: the n-th extra space takes the
                // highest degree at or under `n` whose pitch is under
                // [`SPACE_STEP_HI_HZ`]. The ratio is monotonic in degree, so
                // walking `n` down is a clamp; the u8 run length bounds it.
                let mut f = self.space_run_root_hz;
                for n in (1..=i32::from(self.space_run_len)).rev() {
                    let step =
                        self.space_run_root_hz * (self.melody_hz(anchor, deg + n) / root).max(1.0);
                    if step < SPACE_STEP_HI_HZ {
                        f = step;
                        break;
                    }
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
                            ..Partial::default()
                        },
                        Partial {
                            lvl: SPACE_STEP_OCTAVE_LEVEL,
                            f0: f * 2.0,
                            f1: f * 2.0,
                            ..Partial::default()
                        },
                        Partial::default(),
                    ],
                    // A thumb, like the head's: 5 ms of felt out of 4.2 kHz.
                    n_lvl: 0.4,
                    n_f0: 4200.0,
                    n_f1: 180.0,
                    n_glide: 0.005,
                    n_q: 0.7,
                    lp_cut: 5200.0,
                    ..Voice::default()
                };
                // Centred like the root it climbs from: the floor of the room.
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
        // A fresh run: the head opens the step clock, so a second space inside
        // one step gap is breath alone.
        self.since_space_step = 0.0;
        // One bass voice: retrigger the live downbeat rather than stack on it.
        for v in &mut self.voices {
            if v.on && v.bass && v.damp <= 0.0 {
                v.damp = SPACE_DAMP_S;
                v.damp0 = SPACE_DAMP_S;
            }
        }
        // The downbeat breathes after a phrase pause (the `SPACE_BREATHE_*`
        // constants): its bass blooms a little. At prose cadence the envelope
        // below is untouched.
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
        // The low bell: the keystroke family's struck-glass shape (sine body
        // with the house 3.01 FM strike face, a quiet double-octave tink, a
        // soft mallet) one octave above `bass_octave`, in [220, 440) Hz: under
        // every palette's melody, above a laptop speaker's rolloff. The pitch
        // is this word's bar root ([`SONG_BASS`]); the walk steps after the
        // read, once per run head.
        let f = self.space_root_hz(anchor, self.bass_step);
        // The run's root, for the indent steps that may follow: the degree the
        // walk read (song key included) and the folded note that sounds.
        self.space_run_deg =
            i32::from(SONG_BASS[usize::from(self.bass_step)]) + i32::from(self.song_key);
        self.space_run_root_hz = f;
        self.bass_step = (self.bass_step + 1) % SONG_BASS.len() as u8;
        let v = Voice {
            bass: true,
            dur,
            attack,
            decay,
            p: [
                // The body: the law partial, strictly loudest, on the note.
                Partial {
                    lvl: 0.55,
                    f0: f,
                    f1: f,
                    fm_ratio: 3.01,
                    fm_i0: SPACE_BELL_GLINT,
                    fm_tau: 0.022,
                    ..Partial::default()
                },
                // The octave above: the bell's roundness (rested, it blooms —
                // the breathing downbeat keeps its deeper exhale).
                Partial {
                    lvl: oct_lvl,
                    f0: f * 2.0,
                    f1: f * 2.0,
                    ..Partial::default()
                },
                // The tink: the double octave (4f) at a whisper, kin to the
                // melody bells' ping but well under them.
                Partial {
                    lvl: fifth_lvl.max(SPACE_BELL_TINK),
                    f0: f * 4.0,
                    f1: f * 4.0,
                    ..Partial::default()
                },
            ],
            // The mallet: a 5 ms felt tap out of 4.2 kHz, softer than the
            // melody bell's — a thumb, not a mallet.
            n_lvl: 0.5,
            n_f0: 4200.0,
            n_f1: 180.0,
            n_glide: 0.005,
            n_q: 0.7,
            // The roof is open enough for the tink's air and the thumb's
            // strike, below the letters' 4f band.
            lp_cut: 5200.0,
            ..breath(0.05)
        };
        self.spawn(v, g * SPACE_VOICE_LEVEL, 0.0);
        // The air: a breath of quiet high noise riding the root, seeded so it
        // consumes no `rnd()` draws. Its partials carry the twinkle, a detuned
        // pair folded into the sparkle band ([`twinkle_hz`]); the detune is in
        // Hz of the root, so the beat rate is the same in any octave.
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
                        ..Partial::default()
                    },
                    Partial {
                        lvl: SPACE_TWINKLE_TWIN,
                        f0: tw + f * SPACE_TWINKLE_DETUNE,
                        f1: tw + f * SPACE_TWINKLE_DETUNE,
                        ..Partial::default()
                    },
                    Partial::default(),
                ],
                n_lvl: 0.5,
                n_f0: 6400.0,
                n_f1: 3800.0,
                n_glide: 0.020,
                n_q: 1.0,
                tw_rate: SPACE_TWINKLE_RATE,
                tw_depth: SPACE_TWINKLE_DEPTH,
                // Not a roof: the one-pole coefficient clamps to 1 (a bypass)
                // for any cut above SR/2π, so the sparkle band's top is not
                // darkened at 48 kHz.
                lp_cut: 8000.0,
                ..Voice::default()
            },
            g * SPACE_VOICE_LEVEL * SPACE_AIR_LEVEL,
            0.0,
            0.0,
            [0.0; 3],
        );
    }

    /// The ting, the bare Shift's voice (the `SHIFT_TING_*` constants): one
    /// struck-glass bell (sine body with the 3.01 FM strike glint, a quiet
    /// octave, a felt mallet) ringing [`SHIFT_TING_DUR_S`] in its own register
    /// ([`SHIFT_TING_LO_HZ`]). The degree is `deg` plus the next
    /// [`SHIFT_ROTATION`] entry; the rotation steps only on this admitted path,
    /// so a thinned ting consumes no note. rng: one `spawn`, four draws.
    fn design_shift(&mut self, ev: &SoundEvent, deg: i32, g: f32) {
        let anchor = palette_for(ev.voice, ev.style).anchor_hz();
        let deg = deg + SHIFT_ROTATION[usize::from(self.shift_step)];
        self.shift_step = (self.shift_step + 1) % SHIFT_ROTATION.len() as u8;
        let f = ting_octave(self.melody_hz(anchor, deg));
        // The grace note follows the beat it announces: on a ghost slot (the
        // next keystroke's `song_pulse`, which a bare Shift never advances)
        // the ting takes the ghost's level so it does not out-peak its letter.
        let ghost_next = SONG_PULSE[usize::from(self.song_pulse)] != SONG_ACCENT;
        let g = if ghost_next { g * SONG_GHOST_LEVEL } else { g };
        let v = Voice {
            dur: SHIFT_TING_DUR_S,
            attack: SHIFT_TING_ATTACK_S,
            decay: SHIFT_TING_DECAY_S,
            p: [
                // The body: the law partial, strictly loudest, on the note,
                // wearing the strike glint.
                Partial {
                    lvl: 0.55,
                    f0: f,
                    f1: f,
                    fm_ratio: 3.01,
                    fm_i0: SHIFT_TING_GLINT,
                    fm_tau: SHIFT_TING_GLINT_TAU_S,
                    ..Partial::default()
                },
                // The octave: roundness.
                Partial {
                    lvl: SHIFT_TING_OCTAVE_LEVEL,
                    f0: f * 2.0,
                    f1: f * 2.0,
                    ..Partial::default()
                },
                Partial::default(),
            ],
            // The mallet: 5 ms of felt out of 4.2 kHz — the same thumb the
            // space bell is struck with.
            n_lvl: 0.35,
            n_f0: 4200.0,
            n_f1: 180.0,
            n_glide: 0.005,
            n_q: 0.7,
            // The roof, open: the glint's sidebands (3.01 × f ± f) are the
            // "ting".
            lp_cut: 9000.0,
            ..Voice::default()
        };
        self.spawn(v, g * SHIFT_TING_VOICE_LEVEL, ev.pan);
    }

    /// The erase poof, the deletion's whole voice (the `POOF_*` constants): a
    /// broad noise body at the press and a brighter air cap dispersing behind
    /// it ([`POOF_AIR_SETTLE_HZ`]); no tonal partial and no palette voice.
    /// Seeded, so it consumes no `rnd()` draws. A held run keeps exactly one
    /// pending release shimmer booked behind it: each admitted poof expires the
    /// previous one ([`Self::push`]) and books the next, so the glitter sounds
    /// once, on the run's last release.
    fn design_erase_poof(&mut self, ev: &SoundEvent, g: f32) {
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
            ..Voice::default()
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
            ..Voice::default()
        };
        self.spawn_seeded(
            air,
            g * POOF_VOICE_GAIN * POOF_AIR_LEVEL,
            ev.pan,
            0.0,
            [0.0; 3],
        );
        // Book the held run's release shimmer. The previous one was expired in
        // `push`; this one survives only if the run ends here.
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
                ..Voice::default()
            };
            self.spawn_seeded(glitter, g * ERASE_SHIMMER_GAIN, ev.pan, 0.0, [0.0; 3]);
        }
    }

    /// Expire the held delete run's pending release shimmer, booked by
    /// [`Self::design_erase_poof`] and still inside its pre-delay. The damp
    /// burns through the pre-delay, so it dies unheard; a shimmer already
    /// sounding is left to ring rather than clicked off.
    fn damp_pending_shimmer(&mut self) {
        for v in &mut self.voices {
            if v.on && v.shimmer && v.t < 0.0 && v.damp <= 0.0 {
                v.damp = ERASE_SHIMMER_DAMP_S;
                v.damp0 = ERASE_SHIMMER_DAMP_S;
            }
        }
    }

    /// The word poof ([`SoundKind::KillWord`]), the erase poof's larger, softer
    /// cousin (the `WORD_POOF_*` constants): the same anatomy at word scale,
    /// lower and longer, terminal before palette dispatch, and under the line
    /// kill's swoosh.
    fn design_word_poof(&mut self, ev: &SoundEvent, g: f32) {
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
            ..Voice::default()
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
            ..Voice::default()
        };
        self.spawn_seeded(
            air,
            g * POOF_VOICE_GAIN * WORD_POOF_GAIN * WORD_POOF_AIR_LEVEL,
            ev.pan,
            0.0,
            [0.0; 3],
        );
    }

    /// The smoke puff's noise ([`SoundKind::Poof`]), designed at kind level
    /// beside the erase poof it disperses from. A puff, not a thud, and never
    /// in the typing's way:
    /// - pure noise, so it cannot land on a wrong note;
    /// - the same `POOF_*` air band as the deletion, since cloud and poof are
    ///   one event;
    /// - longer and softer than the poof's cap, settling downward
    ///   ([`POOF_CLOUD_SETTLE_HZ`]).
    ///
    /// Tier: [`POOF_KIND_GAIN`]; noise compensation: [`POOF_CLOUD_GAIN`].
    fn design_poof(&mut self, ev: &SoundEvent, g: f32) {
        let v = Voice {
            dur: POOF_CLOUD_DUR_S,
            attack: POOF_CLOUD_ATTACK_S,
            decay: POOF_CLOUD_DECAY_S,
            n_lvl: 0.5,
            n_f0: POOF_AIR_HZ,
            n_f1: POOF_CLOUD_SETTLE_HZ,
            n_glide: POOF_CLOUD_GLIDE_S,
            // The cap's own width: broad enough to stay air, never a whistle.
            n_q: POOF_AIR_Q,
            lp_cut: POOF_LP_CUT_HZ,
            ..Voice::default()
        };
        self.spawn_seeded(v, g * POOF_CLOUD_GAIN, ev.pan, 0.0, [0.0; 3]);
    }

    /// Prism wake's voice, the answer to program output: two sine partials
    /// (fundamental plus a quarter-amplitude octave), a 12 ms attack and a
    /// low per-voice cutoff, so it is noticed only when idle. Pitched on the
    /// tune's pentatonic lattice via [`Self::melody_hz`], at a degree from the
    /// theme-seeded hue ([`SoundEvent::hue`]).
    ///
    /// [`OutputGesture::Settle`] is the same voice without the octave and
    /// quieter: a release is never louder than its onset.
    fn design_output_pip(&mut self, ev: &SoundEvent, gesture: OutputGesture, duck: f32) {
        // The verdict belongs to the music box alone (`v2_engaged` forks it to
        // `push_v2`); it reaches here only under another instrument, which
        // has no chord loop to cadence on, so it stays silent.
        let (kind_gain, octave) = match gesture {
            OutputGesture::Shimmer => (OUTPUT_PIP_KIND_GAIN, 0.25),
            OutputGesture::Settle => (OUTPUT_SETTLE_KIND_GAIN, 0.0),
            OutputGesture::Verdict { .. } => return,
        };
        let g = ev.gain * duck * (0.55 + 0.45 * ev.heat) * kind_gain;
        // Five lattice degrees chosen by the theme-seeded hue (clamped at the
        // door), not `self.walk`, so the pip never reads as the next note.
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
                    ..Partial::default()
                },
                Partial {
                    lvl: octave,
                    f0: f * 2.0,
                    f1: f * 2.0,
                    ..Partial::default()
                },
                Partial::default(),
            ],
            // No noise burst at all — a breath would read as an erase poof.
            n_lvl: 0.0,
            // The voice lowpass keeps it soft.
            lp_cut: 2200.0,
            ..Voice::default()
        };
        self.spawn_seeded(v, g, ev.pan, 0.0, [0.0; 3]);
    }

    /// The curse-word bonk, designed once at kind level. It alone uses
    /// non-pentatonic intervals: a clash (minor second + tritone against the
    /// current walk degree, in the palette's register via
    /// [`Palette::anchor_hz`]) over a round low thump. Both voices are
    /// duck-exempt and arm the master duck. [`BONK_KIND_GAIN`] > 1, no bed
    /// feed, no walk step.
    fn design_bonk(&mut self, ev: SoundEvent, duck: f32) {
        let g = ev.gain * duck * (0.55 + 0.45 * ev.heat) * BONK_KIND_GAIN;
        let root = penta(palette_for(ev.voice, ev.style).anchor_hz(), self.walk);
        let m2 = root * BONK_MINOR_SECOND;
        let tt = root * BONK_TRITONE;
        // The clash: both wrong notes sag onto their targets from ~a third
        // above — the pitch drops like something landing on your head.
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
                    ..Partial::default()
                },
                Partial {
                    lvl: 0.5,
                    f0: tt * 1.35,
                    f1: tt,
                    glide: 0.02,
                    ..Partial::default()
                },
                Partial::default(),
            ],
            lp_cut: 2600.0,
            duck_exempt: true,
            ..Voice::default()
        };
        // The voice trims hold the bonk at its ladder tier, below alert level;
        // `BONK_KIND_GAIN` sets priority, not level.
        self.spawn(clash, g * 0.306, ev.pan);
        // The body: a low woody thump with a whisper of knock noise.
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
                    ..Partial::default()
                },
                Partial::default(),
                Partial::default(),
            ],
            n_lvl: 0.18,
            n_f0: 700.0,
            n_f1: 180.0,
            n_glide: 0.05,
            n_q: 0.9,
            lp_cut: 900.0,
            duck_exempt: true,
            ..Voice::default()
        };
        self.spawn(thump, g * 0.258, ev.pan * 0.6);
        self.duck = 1.0;
    }

    /// Latch [`Self::song_key`], the pentatonic shift the typed melody borrows
    /// while the cat sings, from the bar's signature.
    ///
    /// Separate from [`Self::design_celebration`] because the key outlives the
    /// bar (released with the sing duck in `render`). Derived from the same
    /// `sig` the riff transposes by, so typing and riff always agree.
    fn latch_song_key(&mut self, sig: u32) {
        // The whole shift: root plus mode rotation, the same two axes the riff
        // applies. −3..=4 by construction (`celebration_root` is
        // `sig % 5 − 2`, `celebration_mode` one of {0, 2, −1}), so the i8
        // narrowing is total and `MODE_ROTATIONS`'s register guard covers it.
        self.song_key = (celebration_root(sig) + celebration_mode(sig)) as i8;
        // A song that starts again is not one that is handing its key back.
        self.v2_key_pending = false;
    }

    /// One bar of the sing-along riff from the eight-bar [`CELEBRATION_PHRASE`]
    /// form: up to eight swung eighth-note pulse voices as pre-delayed spawns,
    /// [`CELEBRATION_BASS`] folded into each beat's lead voice, a sixteenth
    /// fill on the turnaround, and the sing duck armed for the bar.
    ///
    /// Each key sings its own verse through three axes of `sig` alone, so a key
    /// change is just the next bar's payload ([`Self::last_riff_sig`] only
    /// damps old tails): the verse walk ([`celebration_bar_degrees`]; chorus
    /// bars keep the authored phrase), the root ([`celebration_root`]) and the
    /// mode ([`celebration_mode`]).
    ///
    /// Unlike the trail path: no governor duck (the armed state is a key-repeat
    /// flood), no walk step, no bed feed, and `duck_exempt` voices, so the riff
    /// rides above its own sing duck and skips [`tone_feel`]. Pitch goes
    /// through [`Self::melody_hz`] like the typed melody. Callers latch the key
    /// with [`Self::latch_song_key`] first.
    fn design_celebration(&mut self, ev: SoundEvent, bar: u16, sig: u32) {
        // The switch damp: under one held key the riff's overhang is its
        // sustain pedal, but a bar with a different signature would ring the
        // old key under the new verse. Put every active celebration voice into
        // the short [`CELEBRATION_DAMP_S`] release before the new bar spawns.
        // Same-sig bars skip this, and it draws no rng.
        if self.last_riff_sig.is_some_and(|old| old != sig) {
            for v in &mut self.voices {
                if v.on && v.celebration && v.damp <= 0.0 {
                    v.damp = CELEBRATION_DAMP_S;
                    v.damp0 = CELEBRATION_DAMP_S;
                }
            }
        }
        self.last_riff_sig = Some(sig);
        let g = ev.gain * (0.55 + 0.45 * ev.heat) * CELEBRATION_KIND_GAIN;
        let idx = usize::from(bar) % CELEBRATION_PHRASE_BARS;
        // Axis 1 — this key's verse (chorus bars come back authored).
        let phrase = &celebration_bar_degrees(sig, idx);
        let bass_bar = &CELEBRATION_BASS[idx];
        // Root plus mode rotation: one degree shift applied to every sounding
        // degree (lead, bass, fill) before `melody_hz`.
        let shift = celebration_root(sig) + celebration_mode(sig);
        // Escalation ramp 0..1 across the opening bars, a pure function of
        // the bar index (no rng draw).
        let build = (f32::from(bar) / CELEBRATION_BUILD_BARS).min(1.0);
        let lift = CELEBRATION_BAR_LIFT[idx];
        let clap = bar >= CELEBRATION_CLAP_BAR;
        let turnaround = idx == CELEBRATION_PHRASE_BARS - 1;
        // On the turnaround the last two slots belong to the fill, so a
        // sustain may not reach into them.
        let sustain_limit = if turnaround { 6 } else { phrase.len() };
        let lp_cut = 2800.0 + 1400.0 * build;
        let shimmer = 0.10 + 0.12 * build;
        for (i, &deg) in phrase.iter().enumerate() {
            if deg == REST {
                continue; // held by the note before it
            }
            let span = celebration_span(phrase, i, sustain_limit);
            // `deg` is this key's contour degree (verse walk or shared chorus),
            // `shift` its root + mode. The bar grid is shared, so a mid-hold
            // key change modulates at the next bar.
            let hz = self.melody_hz(CELEBRATION_BASE_HZ, deg + shift);
            // The bassline rides the beat's lead voice as its third partial;
            // `build` fades the low end in over the opening bars.
            let sub = if i.is_multiple_of(2) && bass_bar[i / 2] != REST {
                let b = self.melody_hz(CELEBRATION_BASE_HZ, bass_bar[i / 2] + shift);
                Partial {
                    lvl: 0.42 * build,
                    f0: b,
                    f1: b,
                    ..Partial::default()
                }
            } else {
                Partial::default()
            };
            // Backbeat clap once a full phrase has run, fused into the note's
            // noise channel so it costs no voice.
            let (n_lvl, n_f0, n_f1, n_glide, n_q) = if clap && (i == 2 || i == 6) {
                (0.10, 2600.0, 1200.0, 0.02, 1.4)
            } else {
                (0.0, 0.0, 0.0, 0.0, 0.0)
            };
            let v = Voice {
                delay: celebration_slot_delay(i),
                // Span 1 is exactly 0.30 s: in f32,
                // `CELEBRATION_EIGHTH * 1.5 == 0.30` bit for bit.
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
                ..Voice::default()
            };
            self.spawn(
                v,
                g * CELEBRATION_GROOVE[i] * lift,
                celebration_sway(ev.pan, deg),
            );
        }
        // The fill: the turnaround's final two slots subdivide into sixteenths
        // and crescendo back onto the hook.
        if turnaround {
            for (k, &deg) in CELEBRATION_FILL.iter().enumerate() {
                // The fill carries the shift too, staying in the song's key.
                let hz = self.melody_hz(CELEBRATION_BASE_HZ, deg + shift);
                let v = Voice {
                    delay: (6.0 + 0.5 * k as f32) * CELEBRATION_EIGHTH,
                    dur: CELEBRATION_EIGHTH,
                    attack: 0.003,
                    decay: 0.055,
                    p: celebration_lead(hz, shimmer, Partial::default()),
                    lp_cut,
                    duck_exempt: true,
                    celebration: true,
                    ..Voice::default()
                };
                self.spawn(
                    v,
                    g * (0.34 + 0.06 * k as f32) * lift,
                    celebration_sway(ev.pan, deg),
                );
            }
        }
        // Arm and hold the sing duck for the bar plus a hair, so back-to-back
        // bars never gap; exponential handback once they stop.
        self.sing = 1.0;
        self.sing_hold = CELEBRATION_BAR_SECONDS + 0.1;
    }

    /// The outro (§27): one lead voice on *do* (the riff's pulse timbre, its
    /// octave shimmer and the bass *do* as third partial) held for
    /// [`CELEBRATION_OUTRO_S`], under the faraway ice bell
    /// ([`rainbow_kitty_v2::cad_bell_voice`]). `sig` supplies the run's root +
    /// mode, so *do* is this key's. Sounding celebration voices ride out
    /// undamped; the sing duck holds for the outro, then hands back.
    fn design_celebration_outro(&mut self, ev: SoundEvent, sig: u32) {
        self.last_riff_sig = Some(sig);
        let g = ev.gain * (0.55 + 0.45 * ev.heat) * CELEBRATION_KIND_GAIN;
        let shift = celebration_root(sig) + celebration_mode(sig);
        let hz = self.melody_hz(CELEBRATION_BASE_HZ, shift);
        let sub = self.melody_hz(CELEBRATION_BASE_HZ, CELEBRATION_OUTRO_SUB_DEG + shift);
        let bass = Partial {
            lvl: 0.42,
            f0: sub,
            f1: sub,
            ..Partial::default()
        };
        let lead = Voice {
            dur: CELEBRATION_OUTRO_S,
            attack: 0.004,
            decay: CELEBRATION_OUTRO_DECAY_S,
            p: celebration_lead(hz, 0.22, bass),
            lp_cut: 4200.0,
            duck_exempt: true,
            celebration: true,
            ..Voice::default()
        };
        self.spawn(lead, g * CELEBRATION_GROOVE[0], celebration_sway(ev.pan, 0));
        let (mut bell, trim) = rainbow_kitty_v2::cad_bell_voice(0.0);
        bell.duck_exempt = true;
        bell.celebration = true;
        self.spawn(bell, ev.gain * trim, -0.5 * ev.pan);
        self.sing = 1.0;
        self.sing_hold = CELEBRATION_OUTRO_S;
    }

    // -- rendering ----------------------------------------------------------

    /// Render interleaved stereo into `out` (length must be even). Overwrites
    /// (does not mix). Advances the synth's clock by `out.len() / 2` samples.
    pub fn render(&mut self, out: &mut [f32]) {
        debug_assert!(out.len().is_multiple_of(CHANNELS));
        let frames = out.len() / CHANNELS;
        if frames == 0 {
            return;
        }
        let dt_block = frames as f32 * self.inv_sr;
        // The fallback clock (see `clock_s`), advanced before the quiet
        // early-out: silence still takes time.
        self.clock_s += f64::from(dt_block);
        // Rate estimate decays with real (sample-clock) time.
        self.since_event += dt_block;
        self.since_voice += dt_block;
        self.since_erase += dt_block;
        self.since_space_step += dt_block;
        self.since_shift += dt_block;
        self.rate *= (-dt_block / 0.6).exp();

        if self.is_quiet() {
            out.fill(0.0);
            self.bed.level = 0.0;
            self.bed.energy = 0.0;
            // A parked duck must not survive silence into the next phrase —
            // an unrelated later keystroke would be mysteriously quiet.
            self.duck = 0.0;
            // The sing duck obeys the same law (its hold can only outlive
            // its voices on a starved host clock).
            self.sing = 0.0;
            self.sing_hold = 0.0;
            self.release_song_key();
            // A settled limiter is a reset limiter, like the DC blocker.
            self.lim_env_l = 0.0;
            self.lim_g_l = 1.0;
            self.lim_env_r = 0.0;
            self.lim_g_r = 1.0;
            // The DC blocker needs no reset: `is_quiet` requires it settled.
            return;
        }

        // Bed housekeeping at block rate: energy decay, level slew, grain
        // spawning (grains become ordinary voices).
        self.tick_bed(dt_block);

        let dt = self.inv_sr;
        // Per-sample duck recovery factor (τ = BONK_DUCK_TAU); `self.duck` is
        // written only by `design_bonk` inside `push`, so a block that starts
        // with `duck == 0.0` never needs the exponential.
        let duck_step = if self.duck > 0.0 {
            (-dt / BONK_DUCK_TAU).exp()
        } else {
            0.0
        };
        // The bed's floor test is block-invariant: `bed.level` is written only
        // by `tick_bed`, the quiet early-out and `push`, never inside the
        // sample loop.
        let bed_on = self.bed.level >= 1e-4;
        // In circuit while latched; the latch does not change inside a block.
        let lim_on = self.v2_latched;
        // §9.7's limiter coefficients: per-sample steps of its two time
        // constants, resolved once per block when `v2_latched`.
        let (lim_att, lim_rel) = if lim_on {
            (
                1.0 - (-dt / LIMIT_ATTACK_S).exp(),
                (-dt / LIMIT_RELEASE_S).exp(),
            )
        } else {
            (0.0, 0.0)
        };
        // Nothing inside the sample loop ever turns a voice on, so trailing
        // inactive slots can be skipped for the entire block.
        let active_voices_end = self.voices.iter().rposition(|v| v.on).map_or(0, |i| i + 1);
        for f in 0..frames {
            let (mut l, mut r) = (0.0f32, 0.0f32);
            // Duck-exempt sum — the bonk itself, riding above the dip.
            let (mut xl, mut xr) = (0.0f32, 0.0f32);

            // Discrete voices.
            //
            // A counted `while`, not `for vi in 0..MAX_VOICES`: an unoptimized
            // build calls the range iterator four functions deep per slot, and
            // this loop runs up to `MAX_VOICES` times per frame. Same slots,
            // same order.
            let mut next_vi = 0;
            while next_vi < active_voices_end {
                let vi = next_vi;
                next_vi += 1;
                // Indexed, not `iter_mut`, so the body can reach other `self`
                // fields (`lane_onset_steal`).
                let v = &mut self.voices[vi];
                if !v.on {
                    continue;
                }
                v.t += dt;
                // The switch damp burns on the sample clock, pre-delay
                // included, so an unstarted old-key voice expires unheard.
                if v.damp > 0.0 {
                    v.damp -= dt;
                    if v.damp <= 0.0 {
                        v.on = false;
                        continue;
                    }
                }
                // The arm's fizzle (§12.3): an unclaimed pre-cue burns its ttl,
                // then damps over the fade-steal's 12 ms ramp.
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
                    continue; // pre-delay
                }
                if v.t >= v.dur {
                    v.on = false;
                    continue;
                }
                // A pre-delayed laned voice takes its lane on its first
                // sounding sample (§14), so it evicts at its onset.
                if !v.env_run && v.lane != LANE_NONE && v.delay > 0.0 && self.lane_onset_steal(vi) {
                    self.voices[vi].on = false;
                    continue;
                }
                let v = &mut self.voices[vi];
                // Geometric recursions, re-anchored on the first sounding
                // sample and every `ENV_REANCHOR` samples after. One flag drives
                // all five so they stay mutually consistent.
                let seed = !v.env_run || v.env_n >= ENV_REANCHOR;
                if seed {
                    v.env_n = 0;
                } else {
                    v.env_n += 1;
                }
                let mut s = 0.0f32;
                // Indexed for the same reason as the voice loop above.
                let mut next_pi = 0;
                while next_pi < v.p.len() {
                    let p = &mut v.p[next_pi];
                    next_pi += 1;
                    if p.lvl <= 0.0 {
                        continue;
                    }
                    let freq = if p.glide > 0.0 {
                        p.g_e = if seed {
                            (-v.t / p.glide).exp()
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
                                    (-v.t / p.fm_tau.max(1e-3)).exp()
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
                    // Per-partial decay (§16 delta 1); `decay == 0.0` shares
                    // the voice envelope.
                    if p.decay > 0.0 {
                        p.p_e = if seed {
                            (-v.t / p.decay).exp()
                        } else {
                            p.p_e * p.k_p
                        };
                        s += p.lvl * x * p.p_e;
                    } else {
                        s += p.lvl * x;
                    }
                }
                // Noise burst through the state-variable bandpass.
                if v.n_lvl > 0.0 {
                    let white = xorshift01(&mut self.rng) * 2.0 - 1.0;
                    // The damping is `spawn`'s cached `1.0 / n_q.max(0.3)`.
                    let damp = v.n_damp;
                    let (g_svf, den) = if v.n_glide > 0.0 {
                        // A swept cutoff: `n_e` supplies `fc` and `tan`
                        // follows it per sample.
                        v.n_e = if seed {
                            (-v.t / v.n_glide).exp()
                        } else {
                            v.n_e * v.k_n
                        };
                        let fc = v.n_f1 + (v.n_f0 - v.n_f1) * v.n_e;
                        let g = (core::f32::consts::PI * (fc * dt).min(0.45)).tan();
                        (g, 1.0 + g * (g + damp))
                    } else {
                        // A constant cutoff: coefficient and divisor were
                        // computed in `spawn` (see `Voice::svf_g`).
                        (v.svf_g, v.svf_den)
                    };
                    let hp = (white - v.n_lp - damp * v.n_bp) / den;
                    v.n_bp += g_svf * hp;
                    v.n_lp += g_svf * v.n_bp;
                    // Per-noise decay (§16 delta 2); identity at
                    // `n_decay == 0.0`.
                    if v.n_decay > 0.0 {
                        v.nd_e = if seed {
                            (-v.t / v.n_decay).exp()
                        } else {
                            v.nd_e * v.k_nd
                        };
                        s += v.n_lvl * v.n_bp * v.nd_e;
                    } else {
                        s += v.n_lvl * v.n_bp;
                    }
                }
                // Envelope, twinkle and release guard; both exponentials step
                // geometrically.
                if seed {
                    v.env_a = (-v.t / v.attack.max(2e-4)).exp();
                    v.env_d = (-v.t / v.decay.max(1e-3)).exp();
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
                let rel = ((v.dur - v.t) * 200.0).clamp(0.0, 1.0); // 5 ms anti-click
                env *= rel;
                // The switch damp release: the anti-click's linear ramp at
                // CELEBRATION_DAMP_S scale, starting at ~1.0; the countdown
                // above kills the voice at zero.
                if v.damp > 0.0 {
                    env *= v.damp / v.damp0.max(1e-4);
                }
                // Per-voice softening lowpass (coefficient cached by
                // `spawn` from the identical expression — see `Voice::lp_k`).
                v.lp += v.lp_k * (s - v.lp);
                let y = v.lp * env;
                // Pan glide (§16 delta 3); `pan_glide_s == 0.0` uses the
                // static gains.
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

            // Bed sample, floored once per block (`bed_on`, above): below the
            // floor the bed adds nothing and its LFOs rest.
            if bed_on {
                let (bl, br) = self.bed_sample(dt);
                l += bl;
                r += br;
            }

            // Master duck: melody and bed dip around a live bonk and under a
            // live riff. Each factor is exactly ×1.0 at rest.
            let dmul = (1.0 - BONK_DUCK_DEPTH * self.duck) * (1.0 - SING_DUCK_DEPTH * self.sing);
            let l = l * dmul + xl;
            let r = r * dmul + xr;
            self.duck *= duck_step;

            // Master: DC block, soft saturate, hard safety clamp.
            let xl = l * MASTER;
            let yl = xl - self.dc_x_l + 0.995 * self.dc_y_l;
            self.dc_x_l = xl;
            self.dc_y_l = yl;
            let xr = r * MASTER;
            let yr = xr - self.dc_x_r + 0.995 * self.dc_y_r;
            self.dc_x_r = xr;
            self.dc_y_r = yr;
            // The bus limiter (§9.7, §16 row 10), after MASTER and before the
            // soft clip; active once the first v2 trail event latches it
            // (unless a bench holds it out). Below threshold its gain is
            // exactly 1.0.
            let (yl, yr) = if lim_on {
                (
                    limit(yl, &mut self.lim_env_l, &mut self.lim_g_l, lim_att, lim_rel),
                    limit(yr, &mut self.lim_env_r, &mut self.lim_g_r, lim_att, lim_rel),
                )
            } else {
                (yl, yr)
            };
            out[f * 2] = soft_clip(yl);
            out[f * 2 + 1] = soft_clip(yr);
        }
        // Below ~0.01 % depth the duck is arithmetic noise — snap to the
        // exact 0.0 the bit-identity contract (dmul == 1.0) requires.
        if self.duck < 1e-4 {
            self.duck = 0.0;
        }
        // The DC blocker snaps to zero the same way: below -120 dBFS its tail
        // is arithmetic, and an exact zero lets `is_quiet` be an equality.
        if self.dc_y_l.abs() < 1e-6 && self.dc_x_l.abs() < 1e-6 {
            self.dc_x_l = 0.0;
            self.dc_y_l = 0.0;
        }
        if self.dc_y_r.abs() < 1e-6 && self.dc_x_r.abs() < 1e-6 {
            self.dc_x_r = 0.0;
            self.dc_y_r = 0.0;
        }
        // Sing-duck housekeeping at block rate (the envelope is a hold): burn
        // the hold, then the exponential handback, then the exact-zero snap.
        if self.sing > 0.0 {
            if self.sing_hold > 0.0 {
                self.sing_hold -= dt_block;
            } else {
                self.sing *= (-dt_block / SING_DUCK_TAU).exp();
            }
            if self.sing < 1e-4 {
                self.sing = 0.0;
                // The key is released with the duck, so the borrowed transpose
                // never outlives the song (the quiet early-out is never reached
                // while someone keeps typing). Under the music box this is
                // §10.2's boundary handback (`release_song_key`).
                self.release_song_key();
            }
        }
    }

    /// Feed the bed for one admitted trail gesture: [`bed_kick`]'s energy for
    /// the kind and the smoothed gain for the user volume. Both engines (v1's
    /// trail arm and `push_v2`) call this; the `ev.bed` gate stays at each
    /// call site.
    pub(super) fn kick_bed(&mut self, kind: SoundKind, gain: f32) {
        self.bed.energy = (self.bed.energy + bed_kick(kind)).min(1.0);
        self.bed.gain += (gain - self.bed.gain) * 0.3;
    }

    /// Block-rate bed upkeep: energy decay, level slew, stochastic grains
    /// (grain design is per-palette — [`Palette::bed_grain`]).
    fn tick_bed(&mut self, dt: f32) {
        let b = &mut self.bed;
        // Below ~-34 dB the bed is imperceptible: hurry the tail so the host's
        // queue pause engages about a second after audibility ends.
        let tau_e = if b.energy < 0.02 { 0.2 } else { 0.7 };
        b.energy *= (-dt / tau_e).exp();
        // Asymmetric slew: swell in ~250 ms, breathe out ~700 ms.
        let target = b.energy.min(1.0);
        // Falling slew also hurries once inaudible (same rationale as tau_e).
        let tau = if target > b.level {
            0.25
        } else if b.level < 0.02 {
            0.2
        } else {
            0.7
        };
        b.level += (target - b.level) * (1.0 - (-dt / tau).exp());
        // Floor snap: below −62 dB pre-gain, snap to exact zero so `is_quiet`
        // engages promptly.
        if b.energy < 3e-3 && b.level < 3e-3 {
            b.energy = 0.0;
            b.level = 0.0;
        }
        // Beam power-down droop: rises toward 1 as the level dies.
        let dying = (0.35 - b.level).max(0.0) / 0.35;
        b.droop += (dying - b.droop) * (1.0 - (-dt / 0.4).exp());

        // Stochastic grains for the textures that live on scarcity.
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

    /// One stereo sample of the continuous bed texture: the shared LFO
    /// prologue here, the per-style body on the palette
    /// ([`Palette::bed_sample`]).
    fn bed_sample(&mut self, dt: f32) -> (f32, f32) {
        let lvl = self.bed.level * self.bed.gain;
        // Shared slow LFOs (used as undulation / vibrato per style).
        self.bed.lfo1 = (self.bed.lfo1 + 0.23 * dt).fract();
        self.bed.lfo2 = (self.bed.lfo2 + 0.71 * dt).fract();
        let u1 = 0.5 * (1.0 + sin01(self.bed.lfo1));
        let u2 = 0.5 * (1.0 + sin01(self.bed.lfo2));

        let (m, side) =
            palette_for(self.bed_voice, self.bed_style).bed_sample(self, dt, lvl, u1, u2);
        // `side` widens beds slightly; kept tiny to stay mono-compatible.
        (m + side, m - side)
    }
}

// ---------------------------------------------------------------------------
// Palettes — one unit per GlowStyle, registered in `palette_for`
// ---------------------------------------------------------------------------

/// One style's complete sound identity: discrete voice design, stochastic bed
/// grains and the continuous bed texture. Implementors are stateless; all
/// state lives on [`TrailSynth`], passed back in.
///
/// Adding a palette = one implementor + one [`palette_for`] arm.
trait Palette {
    /// Design and spawn the voice(s) for one admitted trail gesture. The
    /// style-agnostic kinds (Kill, Land, movement, space, shift, the deletions
    /// and the puff) are designed before dispatch and never arrive here.
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        deg: i32,
        col_off: i32,
    );

    /// Spawn one stochastic bed grain and re-arm `s.bed.timer`. The default
    /// suits purely tonal beds: no grain, fixed re-arm.
    fn bed_grain(&self, s: &mut TrailSynth, _level: f32, _gain: f32) {
        s.bed.timer = 0.25; // styles with purely tonal beds
    }

    /// One stereo sample `(mid, side)` of the continuous bed texture. The
    /// caller has advanced the shared LFOs (`u1`/`u2`) and folded level × gain
    /// into `lvl`. The default is exact zeros: a keyboard or typewriter has
    /// no weather.
    fn bed_sample(
        &self,
        _s: &mut TrailSynth,
        _dt: f32,
        _lvl: f32,
        _u1: f32,
        _u2: f32,
    ) -> (f32, f32) {
        (0.0, 0.0)
    }

    /// The palette's melodic base: the register its keystroke lives in,
    /// borrowed by the style-agnostic gestures so they speak in the typing's
    /// octave (the bonk clashes here; the movement family,
    /// [`TrailSynth::design_cursor`], sings here).
    ///
    /// Default: the Lumen mid register, also right for unpitched palettes.
    fn anchor_hz(&self) -> f32 {
        CURSOR_ANCHOR_HZ
    }
}

/// The palette registry, the one place a [`GlowStyle`] binds to its sound. A
/// non-[`SoundVoice::Style`] voice overrides the style binding;
/// [`SoundVoice::Of`] aliases into the style table; the sound-only voices
/// bind their own palettes. The rainbow kitty look, `Of(RainbowKitty)` and
/// [`SoundVoice::RainbowKittyV2`] all resolve to the music box.
fn palette_for(voice: SoundVoice, style: GlowStyle) -> &'static dyn Palette {
    match voice {
        SoundVoice::Mech => &MechPalette,
        SoundVoice::Typewriter => &TypewriterPalette,
        SoundVoice::Marimba => &MarimbaPalette,
        SoundVoice::Felt => &FeltPalette,
        // The music box's bed and register; its `design` is never dispatched
        // (`push` routes v2 to `push_v2` first).
        SoundVoice::RainbowKittyV2 => &RainbowKittyV2Palette,
        SoundVoice::Of(s) => palette_for(SoundVoice::Style, s),
        SoundVoice::Style => match style {
            GlowStyle::Lumen => &LumenPalette,
            GlowStyle::Phaser | GlowStyle::Classic => &PhaserPalette,
            GlowStyle::RainbowKitty => &RainbowKittyV2Palette,
            GlowStyle::Sparkle => &SparklePalette,
            GlowStyle::Fire => &FirePalette,
            GlowStyle::Laser => &LaserPalette,
            GlowStyle::Beam => &BeamPalette,
            GlowStyle::Water => &WaterPalette,
            GlowStyle::Comet => &CometPalette,
        },
    }
}

/// Lumen, lamplight: each key blooms onto its note with a beating twin and a
/// sub-octave glow, over a breathing lamp pad. The default sound.
struct LumenPalette;

impl Palette for LumenPalette {
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        deg: i32,
        _col_off: i32,
    ) {
        // Each key blooms: a warm mid tone easing up onto its note, a softly
        // beating twin for width, a sub-octave glow and a breath of air.
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
                    ..Partial::default()
                },
                // The beating twin — a shade off, so the tone glows instead
                // of pinging.
                Partial {
                    lvl: 0.22,
                    f0: f0 * 1.004,
                    f1: f1v * 1.004,
                    glide: 0.05,
                    ..Partial::default()
                },
                // Sub-octave warmth: the lamp's body.
                Partial {
                    lvl: 0.16,
                    f0: f0 * 0.5,
                    f1: f1v * 0.5,
                    glide: 0.05,
                    ..Partial::default()
                },
            ],
            // A breath of air around the filament — soft, wide, tiny.
            n_lvl: 0.02,
            n_f0: 3400.0,
            n_f1: 3400.0,
            n_glide: 0.0,
            n_q: 0.6,
            lp_cut: 2000.0,
            ..Voice::default()
        };
        s.spawn(v, g * 0.42, ev.pan);
        if kind == SoundKind::Jump {
            // Grace note a fifth up, blooming the same way — the little
            // "arrived" flourish.
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
                        ..Partial::default()
                    },
                    Partial {
                        lvl: 0.2,
                        f0: f2 * 1.004,
                        f1: f2 * 1.004,
                        ..Partial::default()
                    },
                    Partial::default(),
                ],
                lp_cut: 2200.0,
                ..Voice::default()
            };
            s.spawn(v2, g * 0.3, ev.pan);
        }
    }

    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, u1: f32, _u2: f32) -> (f32, f32) {
        // The lamp left on: the airy pair plus a soft third a tenth above
        // that breathes with the slow LFO, so the afterglow swells and dims.
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 165.0 * dt).fract();
        b.ph2 = (b.ph2 + 165.6 * dt).fract();
        b.ph3 = (b.ph3 + 412.5 * dt).fract();
        let sm = sin01(b.ph1) + sin01(b.ph2) + (0.1 + 0.2 * u1) * sin01(b.ph3);
        (sm * lvl * 0.024, sm * lvl * 0.005)
    }
}

/// Phaser, the spectrum sweep: a soft "pew" gliding down onto a pentatonic
/// target whose degree follows the live hue, so colour and pitch travel
/// together.
struct PhaserPalette;

impl Palette for PhaserPalette {
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        _deg: i32,
        col_off: i32,
    ) {
        // A rounded "boop" settling onto a note whose degree follows the live
        // hue, a tiny low thock for the landing, a two-note "ba-deep!" on Jump.
        let hue_deg = (ev.hue * 5.0) as i32;
        // Phaser builds its own degree from the hue, so it adds the family
        // offset and the capital's lift rather than spelling its own intervals.
        let d = hue_deg + col_off + gesture_shape(kind).offset + shift_lift(ev, kind);
        let f = s.melody_hz(392.0, d);
        let (f0, f1v) = (f * 1.09, f);
        let v = Voice {
            dur: 0.18,
            attack: 0.004,
            decay: if kind == SoundKind::Jump { 0.09 } else { 0.075 },
            p: [
                Partial {
                    lvl: 0.75,
                    f0,
                    f1: f1v,
                    glide: 0.022,
                    ..Partial::default()
                },
                // A quiet octave doubles the warmth without reaching into
                // the shrill upper register.
                Partial {
                    lvl: 0.12,
                    f0: f * 2.0,
                    f1: f1v * 2.0,
                    glide: 0.022,
                    ..Partial::default()
                },
                Partial::default(),
            ],
            lp_cut: 1900.0,
            ..Voice::default()
        };
        s.spawn(v, g * 0.4, ev.pan);
        // The thock: a whisper of low filtered noise, gone in ~15 ms — felt
        // more than heard, the key landing somewhere soft.
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
            ..Voice::default()
        };
        s.spawn(t, g * 0.25, ev.pan);
        if kind == SoundKind::Jump {
            // "…deep!" — a fifth up, with a soft glassy glint riding it:
            // the emitter's little ta-da.
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
                        ..Partial::default()
                    },
                    Partial {
                        lvl: 0.1,
                        f0: f2 * 2.0,
                        f1: f2 * 2.0,
                        fm_ratio: 3.01,
                        fm_i0: 0.4,
                        fm_tau: 0.05,
                        ..Partial::default()
                    },
                    Partial::default(),
                ],
                lp_cut: 2100.0,
                ..Voice::default()
            };
            s.spawn(v2, g * 0.34, ev.pan * 0.5);
        }
    }

    fn bed_grain(&self, s: &mut TrailSynth, level: f32, gain: f32) {
        // While charged, the emitter dreams: rare, very quiet round pips
        // wandering the pentatonic — the next colour being considered.
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
                    ..Partial::default()
                },
                Partial::default(),
                Partial::default(),
            ],
            lp_cut: 1700.0,
            ..Voice::default()
        };
        let pan = s.rnd_in(-0.5, 0.5);
        s.spawn(v, gain * level * 0.05, pan);
        s.bed.timer = s.rnd_in(0.45, 1.2) / level.max(0.05);
    }

    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, u1: f32, _u2: f32) -> (f32, f32) {
        // Charged-emitter purr: a detuned triangle-ish pair under a low, slow
        // sweep (400–900 Hz), a hum rather than a filter show.
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 196.0 * dt).fract();
        b.ph2 = (b.ph2 + 196.8 * dt).fract();
        let sm = tri(b.ph1) + tri(b.ph2);
        let k = ((400.0 + 500.0 * u1) * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (sm - b.lp1);
        (b.lp1 * lvl * 0.05, 0.0)
    }

    fn anchor_hz(&self) -> f32 {
        392.0
    }
}

/// The grace bend: a struck note takes the family's contour bend
/// ([`gesture_shape`]/[`gesture_bend`]) at a third of the depth (~0.65 st),
/// settled (3τ) in ~12 ms under the strike, so a bar or string does not
/// audibly glide. Applied to the law partial only; the body strikes at fixed
/// pitch. Used by the marimba and the felt.
const KEY_BELL_BEND_SHARE: f32 = 0.33;
/// The grace bend's glide time constant (seconds) — 3τ ≈ 12 ms, inside the
/// strike transient, so the bend reads as articulation, never as pitch.
const KEY_BELL_BEND_TAU: f32 = 0.004;

/// Sparkle, glitter: two or three micro-chimes high in the register with
/// sharp attacks, twinkling decays and scattered pans per key.
struct SparklePalette;

impl Palette for SparklePalette {
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        deg: i32,
        _col_off: i32,
    ) {
        // A scatter of twinkling grains per key, each a round C5 dong with a
        // 3 ms attack, a warm sub-octave body and a glassy halo.
        let n = if kind == SoundKind::Jump { 3 } else { 2 };
        for i in 0..n {
            let d = deg + i * 2 + (s.rnd() * 2.0) as i32;
            let f = s.melody_hz(523.25, d); // C5 — the dong register
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
                        ..Partial::default()
                    },
                    // The round body under the dong.
                    Partial {
                        lvl: 0.15,
                        f0: f * 0.5,
                        f1: f * 0.5,
                        ..Partial::default()
                    },
                    // A gentle glassy halo — quiet.
                    Partial {
                        lvl: 0.08,
                        f0: f * 2.01,
                        f1: f * 2.01,
                        ..Partial::default()
                    },
                ],
                tw_rate: s.rnd_in(7.0, 11.0),
                tw_depth: 0.4,
                lp_cut: 2800.0,
                ..Voice::default()
            };
            let pan = ev.pan + s.rnd_in(-0.4, 0.4);
            s.spawn(v, g * 0.28 / (1.0 + i as f32 * 0.4), pan);
        }
    }

    fn bed_grain(&self, s: &mut TrailSynth, level: f32, gain: f32) {
        // Residual glitter: soft dongs drifting after the hands stop, in the
        // same register as the key grains.
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
                    ..Partial::default()
                },
                Partial::default(),
                Partial::default(),
            ],
            tw_rate: 9.0,
            tw_depth: 0.4,
            lp_cut: 2800.0,
            ..Voice::default()
        };
        let pan = s.rnd_in(-0.8, 0.8);
        s.spawn(v, gain * level * 0.08, pan);
        s.bed.timer = s.rnd_in(0.15, 0.5) / level.max(0.05);
    }

    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, _u1: f32, u2: f32) -> (f32, f32) {
        // A barely-there warm pad under the grains (C4 pair, slow-swelling
        // soft octave): a glow, never a whine.
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 261.6 * dt).fract();
        b.ph2 = (b.ph2 + 262.4 * dt).fract();
        b.ph3 = (b.ph3 + 523.9 * dt).fract();
        let sm = sin01(b.ph1) + sin01(b.ph2) + (0.08 + 0.15 * u2) * sin01(b.ph3);
        (sm * lvl * 0.022, sm * lvl * 0.006)
    }

    fn anchor_hz(&self) -> f32 {
        523.25
    }
}

/// Fire, the hearth: a crackle burst (band-passed noise with a random centre)
/// over a warm low ember thump that grows with heat; Jump is a whoosh.
/// Unpitched, so the bonk keeps the default anchor.
struct FirePalette;

impl Palette for FirePalette {
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        _deg: i32,
        _col_off: i32,
    ) {
        // Crackle: every key is an impulsive few-ms high-Q snap over a woody
        // ember knock; impulsiveness separates fire from water. Jump is the
        // flame leaping: a dark low whoomph plus a scatter of spark snaps.
        if kind == SoundKind::Jump {
            // The whoomph: air rushing into the flare, all low-mid.
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
                ..Voice::default()
            };
            s.spawn(v, g * 0.5, ev.pan);
            // Sparks thrown by the leap: a loose cluster of snaps.
            for i in 0..4 {
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
                    ..Voice::default()
                };
                let pan = ev.pan + s.rnd_in(-0.5, 0.5);
                let sg = g * s.rnd_in(0.16, 0.3);
                s.spawn(v, sg, pan);
            }
        } else {
            // The snap: typing cracks bright.
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
                ..Voice::default()
            };
            s.spawn(v, g * 0.42, ev.pan);
            // Crackles cluster: sometimes a second, quieter micro-snap
            // trails the first by a few tens of ms.
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
                    ..Voice::default()
                };
                let pan = ev.pan + s.rnd_in(-0.3, 0.3);
                s.spawn(v2, g * 0.22, pan);
            }
            // The ember: a short woody knock that speaks when typing is hot.
            // Fire's one pitched element, so the capital's lift
            // ([`shift_ratio`]) lands here.
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
                        ..Partial::default()
                    },
                    Partial::default(),
                    Partial::default(),
                ],
                lp_cut: 450.0,
                ..Voice::default()
            };
            s.spawn(v3, g * (0.1 + 0.28 * ev.heat), ev.pan * 0.5);
        }
    }

    fn bed_grain(&self, s: &mut TrailSynth, level: f32, gain: f32) {
        // Crackles in the embers: mostly bright snaps, occasionally a low
        // log-knock, never mid-band plops that read as water.
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
            ..Voice::default()
        };
        let pan = s.rnd_in(-0.6, 0.6);
        s.spawn(v, gain * level * lvl, pan);
        s.bed.timer = s.rnd_in(0.03, 0.18) / level.max(0.05);
    }

    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, _u1: f32, _u2: f32) -> (f32, f32) {
        // The roar under the crackle: dark filtered noise whose loudness
        // flickers irregularly (~9 Hz random flutter). A smooth slow LFO here
        // would read as waves.
        let white = s.rnd() * 2.0 - 1.0;
        let b = &mut s.bed;
        let k = (380.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (white - b.lp1);
        // lp2 doubles as the flicker state: the same white draw through a
        // ~9 Hz one-pole wanders irregularly around unity.
        let kf = (9.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp2 += kf * (white - b.lp2);
        let flick = (1.0 + 26.0 * b.lp2).clamp(0.35, 1.8);
        (b.lp1 * lvl * 0.4 * flick, 0.0)
    }
}

/// Laser, the bolt: a fast exponential zap falling two octaves, with a
/// whisper of noise at the muzzle.
struct LaserPalette;

impl Palette for LaserPalette {
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        deg: i32,
        _col_off: i32,
    ) {
        // Lightning: a two-octave dive with a brief sizzle on the tone and a
        // bright high-Q crack of air. Jump is the full strike: crack, zap,
        // sub-thump, then a soft thunder roll that sweeps down and echoes once.
        // `deg.min(5)` caps the melody before the dive; the capital's octave
        // ([`shift_lift`]) is peeled off before the clamp and added back after,
        // so it is never eaten near the ceiling.
        let lift = shift_lift(ev, kind);
        let f_hi = s.melody_hz(880.0, (deg - lift).min(5) + lift);
        let f_lo = f_hi * 0.25;
        let dur = if kind == SoundKind::Jump { 0.2 } else { 0.11 };
        let v = Voice {
            dur,
            attack: 0.001,
            decay: if kind == SoundKind::Jump { 0.08 } else { 0.045 },
            p: [
                Partial {
                    lvl: 0.7,
                    f0: f_hi,
                    f1: f_lo,
                    glide: 0.024,
                    // The sizzle: a burst of inharmonic FM that dies in
                    // ~12 ms — electricity, not tone.
                    fm_ratio: 7.03,
                    fm_i0: 1.2,
                    fm_tau: 0.012,
                    ..Partial::default()
                },
                Partial {
                    lvl: 0.15,
                    f0: f_hi * 2.0,
                    f1: f_lo * 2.0,
                    glide: 0.024,
                    ..Partial::default()
                },
                Partial::default(),
            ],
            n_lvl: 0.08,
            n_f0: 3200.0,
            n_f1: 1400.0,
            n_glide: 0.02,
            n_q: 1.0,
            lp_cut: 3800.0,
            ..Voice::default()
        };
        s.spawn(v, g * 0.4, ev.pan);
        // The crack: air snapping shut behind the bolt — one bright
        // impulsive tick, gone in 8 ms.
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
            ..Voice::default()
        };
        s.spawn(crack, g * 0.2, ev.pan);
        if kind == SoundKind::Jump {
            // A round sub-thump under the strike — the bolt landing.
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
                        ..Partial::default()
                    },
                    Partial::default(),
                    Partial::default(),
                ],
                lp_cut: 400.0,
                ..Voice::default()
            };
            s.spawn(v2, g * 0.3, ev.pan * 0.4);
            // The thunder: a long low roll sweeping down through the sky,
            // arriving just behind the flash…
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
                ..Voice::default()
            };
            s.spawn(roll, g * 0.45, ev.pan * 0.3);
            // …and its echo off the far side of the sky.
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
                ..Voice::default()
            };
            s.spawn(echo, g * 0.22, -ev.pan * 0.5);
        }
    }

    fn bed_grain(&self, s: &mut TrailSynth, level: f32, gain: f32) {
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
                ..Voice::default()
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
                ..Voice::default()
            };
            let pan = s.rnd_in(-0.7, 0.7);
            s.spawn(roll, gain * level * 0.18, pan);
        }
        s.bed.timer = s.rnd_in(0.5, 1.4) / level.max(0.05);
    }

    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, _u1: f32, _u2: f32) -> (f32, f32) {
        // The storm on the horizon: deep filtered rumble whose level swells
        // and sags unevenly (a ~1.3 Hz random wander, not an LFO).
        let white = s.rnd() * 2.0 - 1.0;
        let b = &mut s.bed;
        let k = (140.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (white - b.lp1);
        let ks = (1.3 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp2 += ks * (white - b.lp2);
        let swell = (1.0 + 40.0 * b.lp2).clamp(0.25, 2.2);
        (b.lp1 * lvl * 0.5 * swell, 0.0)
    }

    fn anchor_hz(&self) -> f32 {
        880.0
    }
}

/// Beam, the photon tube: each key is only a soft console tick, because the
/// signature is the bed's hum that swells behind typing and powers down
/// with the tube's visual fade.
struct BeamPalette;

impl Palette for BeamPalette {
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        deg: i32,
        _col_off: i32,
    ) {
        // A rocket console: every key is a soft rubberized thud and a muted
        // low confirmation blip settling downward. Jump is "engage": press, a
        // slow rising two-tone confirm and a distant engine surge.
        // Even degrees give a stabler line. [`SHIFT_GLYPH_LIFT`] is odd, so it
        // is peeled off before quantising and added back after, keeping the
        // capital an octave.
        let lift = shift_lift(ev, kind);
        let f = s.melody_hz(330.0, ((deg - lift) / 2) * 2 + lift);
        // The button seating: felt more than heard.
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
            ..Voice::default()
        };
        s.spawn(thud, g * 0.22, ev.pan);
        // The console acknowledging: a muted blip settling onto its pitch.
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
                    ..Partial::default()
                },
                Partial {
                    lvl: 0.12,
                    f0: b0 * 2.0,
                    f1: b1 * 2.0,
                    glide: 0.02,
                    ..Partial::default()
                },
                Partial::default(),
            ],
            lp_cut: 1600.0,
            ..Voice::default()
        };
        s.spawn(blip, g * 0.3, ev.pan);
        if kind == SoundKind::Jump {
            // Engage: the two-tone confirm rising a fifth…
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
                        ..Partial::default()
                    },
                    Partial::default(),
                    Partial::default(),
                ],
                lp_cut: 1800.0,
                ..Voice::default()
            };
            s.spawn(v2, g * 0.24, ev.pan);
            // …and the engines answering from below decks: a soft deep
            // surge, all air and thrust, no tone.
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
                ..Voice::default()
            };
            s.spawn(surge, g * 0.26, ev.pan * 0.3);
        }
    }

    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, u1: f32, _u2: f32) -> (f32, f32) {
        // The hull ambience of a coasting rocket: very low filtered air, no
        // tonal content (a sustained chord would whine), breathing slowly.
        // The cutoff sinks as the energy dies, winding down with the tube's
        // visual fade.
        let white = s.rnd() * 2.0 - 1.0;
        let b = &mut s.bed;
        let cut = 130.0 - 55.0 * b.droop;
        let k = (cut.max(60.0) * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (white - b.lp1);
        let k2 = (90.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp2 += k2 * (b.lp1 - b.lp2);
        let breathe = 0.75 + 0.25 * u1;
        (b.lp2 * lvl * 0.5 * breathe, b.lp1 * lvl * 0.03)
    }

    fn anchor_hz(&self) -> f32 {
        330.0
    }
}

/// Water, the droplet: a surface tap then the bubble's rising "bloop", with
/// a small splash (noise plus scattered late droplets) on Jump.
struct WaterPalette;

impl Palette for WaterPalette {
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        deg: i32,
        _col_off: i32,
    ) {
        // The droplet: a soft surface tap, then the collapsing bubble's rising
        // chirp (a falling pitch reads as a dry blip). A round attack and a
        // low "gulp" partial keep it liquid.
        let f = s.melody_hz(430.0, deg);
        let (a, b) = (f * 0.85, f * 1.25);
        // The tap: the surface giving way — tiny, dull, instant.
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
            ..Voice::default()
        };
        s.spawn(tap, g * 0.16, ev.pan);
        // The bloop: the bubble singing as it collapses.
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
                    ..Partial::default()
                },
                // The gulp: a low round body under the chirp.
                Partial {
                    lvl: 0.12,
                    f0: a * 0.5,
                    f1: b * 0.5,
                    glide: 0.045,
                    ..Partial::default()
                },
                Partial::default(),
            ],
            lp_cut: 1800.0,
            ..Voice::default()
        };
        s.spawn(v, g * 0.42, ev.pan);
        if kind == SoundKind::Jump {
            // Splash body…
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
                ..Voice::default()
            };
            s.spawn(vs, g * 0.32, ev.pan);
            // …and the droplets it throws up, landing late and wide — each
            // one a little rising bloop, same language as the key drops.
            for i in 0..3 {
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
                            ..Partial::default()
                        },
                        Partial::default(),
                        Partial::default(),
                    ],
                    lp_cut: 2200.0,
                    ..Voice::default()
                };
                let pan = ev.pan + s.rnd_in(-0.5, 0.5);
                let gd = g * s.rnd_in(0.12, 0.22);
                s.spawn(vd, gd, pan);
            }
        } else if s.rnd() < 0.18 {
            // The occasional answering bubble — a tiny rising chirp
            // a beat later. Scarcity is what keeps it delightful.
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
                        ..Partial::default()
                    },
                    Partial::default(),
                    Partial::default(),
                ],
                lp_cut: 2400.0,
                ..Voice::default()
            };
            s.spawn(vb, g * 0.18, ev.pan);
        }
    }

    fn bed_grain(&self, s: &mut TrailSynth, level: f32, gain: f32) {
        // Distant drips in the stream.
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
                    ..Partial::default()
                },
                Partial::default(),
                Partial::default(),
            ],
            lp_cut: 2200.0,
            ..Voice::default()
        };
        let pan = s.rnd_in(-0.7, 0.7);
        s.spawn(v, gain * level * 0.09, pan);
        s.bed.timer = s.rnd_in(0.25, 0.9) / level.max(0.05);
    }

    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, u1: f32, u2: f32) -> (f32, f32) {
        // The stream — lowpassed noise undulating on two incommensurate LFOs.
        let white = s.rnd() * 2.0 - 1.0;
        let b = &mut s.bed;
        let k = (600.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (white - b.lp1);
        let k2 = (180.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp2 += k2 * (b.lp1 - b.lp2);
        let und = 0.55 + 0.3 * u1 + 0.15 * u2;
        (b.lp2 * lvl * 0.5 * und, b.lp1 * lvl * 0.06)
    }

    fn anchor_hz(&self) -> f32 {
        430.0
    }
}

/// Comet, ice and deep space: soft low transmissions with a cold twelfth and
/// ice-dust hiss, rare shooting stars, and a doppler flyby with crystal
/// debris on Jump.
struct CometPalette;

impl Palette for CometPalette {
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        deg: i32,
        _col_off: i32,
    ) {
        // Deep space: every key is a soft transmission, a low hollow tone
        // drifting down a few cents, a faintly beating twin, a cold distant
        // twelfth and a whisper of ice dust. Rare shooting stars come off
        // typed keys; Jump is the flyby, a doppler swoosh with short crystal
        // glints.
        let f = s.melody_hz(220.0, deg); // A3 region — the void register
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
                    ..Partial::default()
                },
                // The beating twin: ~10 cents off, a slow dark shimmer
                // instead of a ring.
                Partial {
                    lvl: 0.32,
                    f0: f0 * 1.006,
                    f1: f1v * 1.006,
                    glide: 0.16,
                    ..Partial::default()
                },
                // A cold twelfth, barely there — starlight overhead.
                Partial {
                    lvl: 0.07,
                    f0: f0 * 3.0,
                    f1: f1v * 3.0,
                    glide: 0.16,
                    ..Partial::default()
                },
            ],
            n_lvl: 0.025, // ice dust
            n_f0: 6000.0,
            n_f1: 6000.0,
            n_glide: 0.0,
            n_q: 0.7,
            lp_cut: 1600.0,
            ..Voice::default()
        };
        s.spawn(v, g * 0.34, ev.pan);
        if kind == SoundKind::Typed && s.rnd() < 0.14 {
            // The shooting star: a thin bright streak falling fast and
            // twinkling, crossing to the other side of the stereo field.
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
                        ..Partial::default()
                    },
                    Partial::default(),
                    Partial::default(),
                ],
                tw_rate: 9.0,
                tw_depth: 0.4,
                lp_cut: 3200.0,
                ..Voice::default()
            };
            s.spawn(star, g * 0.16, -ev.pan * 0.6);
        }
        if kind == SoundKind::Jump {
            // The flyby: band noise sweeping down with the tone falling under
            // it.
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
                        ..Partial::default()
                    },
                    Partial::default(),
                    Partial::default(),
                ],
                n_lvl: 0.5,
                n_f0: 1200.0,
                n_f1: 260.0,
                n_glide: 0.12,
                n_q: 1.4,
                lp_cut: 2400.0,
                ..Voice::default()
            };
            s.spawn(swoosh, g * 0.4, ev.pan);
            // Debris glints: three short crystal ticks scattered in the
            // wake.
            for i in 0..3 {
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
                            ..Partial::default()
                        },
                        Partial::default(),
                        Partial::default(),
                    ],
                    tw_rate: 8.0,
                    tw_depth: 0.35,
                    lp_cut: 5200.0,
                    ..Voice::default()
                };
                let pan = ev.pan + s.rnd_in(-0.5, 0.5);
                s.spawn(glint, g * 0.12, pan);
            }
        }
    }

    fn bed_grain(&self, s: &mut TrailSynth, level: f32, gain: f32) {
        // Distant signals: far off in the dark, a pure tone swells in,
        // bends, and dies — something out there, rarely, answering.
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
                    ..Partial::default()
                },
                Partial::default(),
                Partial::default(),
            ],
            lp_cut: 2600.0,
            ..Voice::default()
        };
        let pan = s.rnd_in(-0.8, 0.8);
        s.spawn(v, gain * level * 0.06, pan);
        s.bed.timer = s.rnd_in(0.6, 1.6) / level.max(0.05);
    }

    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, u1: f32, _u2: f32) -> (f32, f32) {
        // The void: a deep pair beating every ~2 s under a cold fifth that
        // breathes with the slow LFO. Kept low: a ~1 kHz shimmer reads as
        // tinnitus.
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 110.0 * dt).fract();
        b.ph2 = (b.ph2 + 110.5 * dt).fract();
        b.ph3 = (b.ph3 + 165.0 * dt).fract();
        let sm = 0.5 * sin01(b.ph1) + 0.5 * sin01(b.ph2) + (0.08 + 0.12 * u1) * sin01(b.ph3);
        (sm * lvl * 0.055, sm * lvl * 0.014)
    }

    fn anchor_hz(&self) -> f32 {
        220.0
    }
}

/// Mech, a mechanical keyboard: every key is percussion, a tiny broadband
/// switch click over a low damped case thock, humanized a few Hz per key.
/// Reached only through [`SoundVoice::Mech`]; no [`GlowStyle`] binds here.
/// Unpitched, so the default 330 Hz anchor applies; the bed is silent.
struct MechPalette;

impl MechPalette {
    /// One keystroke: the switch click + the case thock. `body` is the
    /// thock's starting fundamental (falls to 55 % over the glide), `bright`
    /// scales the click's band centre; the caller humanizes both.
    fn key(s: &mut TrailSynth, pan: f32, g: f32, body: f32, bright: f32, click_only: bool) {
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
            ..Voice::default()
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
                    ..Partial::default()
                },
                Partial::default(),
                Partial::default(),
            ],
            n_lvl: 0.12,
            n_f0: 620.0,
            n_f1: 170.0,
            n_glide: 0.045,
            n_q: 0.9,
            lp_cut: 1000.0,
            ..Voice::default()
        };
        s.spawn(thock, g * 0.5, pan * 0.7);
    }
}

impl Palette for MechPalette {
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        _deg: i32,
        _col_off: i32,
    ) {
        match kind {
            SoundKind::Typed => {
                // Per-key humanization: a few Hz of body drift + a touch of
                // click brightness spread, so a flood sounds like fingers.
                let body = s.rnd_in(150.0, 174.0);
                let bright = s.rnd_in(0.9, 1.1);
                Self::key(s, ev.pan, g, body, bright, false);
            }
            SoundKind::Navigation => {
                // A whisper tick: the switch alone, no case body.
                let bright = s.rnd_in(0.85, 1.0);
                Self::key(s, ev.pan, g, 0.0, bright, true);
            }
            SoundKind::Jump => {
                // The flourish: a deeper, longer spacebar/Return clunk plus two
                // pre-delayed lighter ticks, so rapid line feeds clatter.
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
                            ..Partial::default()
                        },
                        Partial::default(),
                        Partial::default(),
                    ],
                    n_lvl: 0.22,
                    n_f0: 520.0,
                    n_f1: 140.0,
                    n_glide: 0.05,
                    n_q: 0.9,
                    lp_cut: 850.0,
                    ..Voice::default()
                };
                s.spawn(clunk, g * 0.6, ev.pan * 0.6);
                for i in 0..2 {
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
                        ..Voice::default()
                    };
                    s.spawn(tick, g * 0.22, -ev.pan * 0.4);
                }
            }
            // Every other kind is designed before dispatch or routed to
            // `push_v2`.
            _ => {}
        }
    }
}

/// Typewriter: every key is a dry slug clack (a short broadband burst) over
/// the platen thud, with the typebar's inharmonic metal ring −10 dB under.
/// Enter is a carriage return: the margin bell dings, the carriage zips home
/// and clunks against the stop.
///
/// Reached only through [`SoundVoice::Typewriter`]; unpitched (the default
/// 330 Hz anchor applies) and with a silent bed. Humanised per key (`b` clack
/// brightness, `r` ring tuning, `body` thud fundamental). At most ~90 ms of
/// energy per key, broad `n_q 0.6`, lp 5200.
struct TypewriterPalette;

impl TypewriterPalette {
    /// One slug strike: the clack (`bright` scales its band), the ring when
    /// `ring` (tuned by `r`), and the platen thud when `body > 0` (starting
    /// fundamental, falling to 55 %). The caller humanises.
    fn strike(s: &mut TrailSynth, pan: f32, g: f32, bright: f32, ring: f32, body: f32) {
        // The clack: 40 ms of broad band-passed noise falling out of 3.2 kHz
        // (13 ms decay). Band-passed noise peaks ~17 dB under a tonal voice at
        // equal gain, so it is spawned hot (×1.6) and the platen kept low.
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
            ..Voice::default()
        };
        s.spawn(clack, g * 1.6, pan);
        if ring > 0.0 {
            // The slug ring: an inharmonic pair (3150/4720, no lattice
            // interval) at fixed pitch, −10 dB under the clack and dying with
            // it, a face on the strike rather than a note.
            let ring_v = Voice {
                dur: 0.09,
                attack: 0.001,
                decay: 0.022,
                p: [
                    Partial {
                        lvl: 0.5,
                        f0: 3150.0 * ring,
                        f1: 3150.0 * ring,
                        ..Partial::default()
                    },
                    Partial {
                        lvl: 0.3,
                        f0: 4720.0 * ring,
                        f1: 4720.0 * ring,
                        ..Partial::default()
                    },
                    Partial::default(),
                ],
                lp_cut: 6000.0,
                ..Voice::default()
            };
            s.spawn(ring_v, g * 0.35, pan);
        }
        if body > 0.0 {
            // The platen thud: a body tone falling to 55 % over 25 ms with a
            // knock of noise, under 1.4 kHz so it is weight, not tone.
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
                        ..Partial::default()
                    },
                    Partial::default(),
                    Partial::default(),
                ],
                n_lvl: 0.15,
                n_f0: 900.0,
                n_f1: 260.0,
                n_glide: 0.03,
                n_q: 0.9,
                lp_cut: 1400.0,
                ..Voice::default()
            };
            s.spawn(thud, g * 0.22, pan * 0.7);
        }
    }
}

impl Palette for TypewriterPalette {
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        _deg: i32,
        _col_off: i32,
    ) {
        match kind {
            SoundKind::Typed => {
                let bright = s.rnd_in(0.9, 1.1);
                let ring = s.rnd_in(0.97, 1.03);
                let body = s.rnd_in(200.0, 232.0);
                Self::strike(s, ev.pan, g, bright, ring, body);
            }
            SoundKind::Navigation => {
                // The escapement "tik": the clack alone, a shade duller, lifted
                // ×2 so it still reads at tier 0 without ring or platen.
                Self::strike(s, ev.pan, g * 2.0, 0.75, 0.0, 0.0);
            }
            SoundKind::Jump => {
                // The carriage return: the margin bell first and immediately,
                // then the carriage zips home and clunks on the stop.
                let ding = Voice {
                    dur: 0.45,
                    attack: 0.001,
                    decay: 0.14,
                    p: [
                        // The bell: C7 with an inharmonic FM strike face
                        // (2.71 — the house ice idiom) that dies first.
                        Partial {
                            lvl: 0.5,
                            f0: 2093.0,
                            f1: 2093.0,
                            fm_ratio: 2.71,
                            fm_i0: 0.9,
                            fm_tau: 0.05,
                            ..Partial::default()
                        },
                        // Its double-octave shimmer.
                        Partial {
                            lvl: 0.12,
                            f0: 4.0 * 2093.0,
                            f1: 4.0 * 2093.0,
                            ..Partial::default()
                        },
                        Partial::default(),
                    ],
                    // The clapper: a 4 ms tick out of 6 kHz.
                    n_lvl: 0.5,
                    n_f0: 6000.0,
                    n_f1: 900.0,
                    n_glide: 0.004,
                    n_q: 0.7,
                    lp_cut: 7000.0,
                    ..Voice::default()
                };
                s.spawn(ding, g * 0.56, (ev.pan * 0.3 + 0.35).clamp(-1.0, 1.0));
                // The zip — the carriage flying home: 160 ms of noise falling
                // through 2.4 kHz → 700 Hz, sliding left.
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
                    ..Voice::default()
                };
                s.spawn(zip, g * 0.48, -0.35);
                // The clunk — the carriage against the left stop.
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
                            ..Partial::default()
                        },
                        Partial::default(),
                        Partial::default(),
                    ],
                    n_lvl: 0.2,
                    n_f0: 700.0,
                    n_f1: 200.0,
                    n_glide: 0.03,
                    n_q: 0.9,
                    lp_cut: 900.0,
                    ..Voice::default()
                };
                s.spawn(clunk, g * 0.61, -0.5);
            }
            // Every other kind is designed before dispatch or routed to
            // `push_v2`.
            _ => {}
        }
    }
}

/// Marimba: a warm rosewood bar under a yarn mallet. The fundamental sings the
/// melody note with the grace bend ([`KEY_BELL_BEND_SHARE`]), and the tuned
/// second mode two octaves up is its own voice dying ~4× faster; the
/// fundamental stays the strictly loudest partial. Enter is a "ta-DA": the
/// note, then its fifth and octave. Anchored at C4 (261.63).
///
/// Reached only through [`SoundVoice::Marimba`]. The bed is the resonator
/// tubes' breath: two soft sines an octave under the anchor.
struct MarimbaPalette;

impl MarimbaPalette {
    /// The melodic base — the marimba's C4, one octave under the bell.
    const BASE_HZ: f32 = 261.63;

    /// One bar strike on lattice degree `deg`: the bar voice (grace-bent from
    /// below, with the yarn mallet's soft noise pad) plus the 4f glint when
    /// `glint`. `decay` is the bar's; Navigation dead-strokes it short.
    fn bar(s: &mut TrailSynth, deg: i32, delay: f32, decay: f32, glint: bool, g: f32, pan: f32) {
        let f = s.melody_hz(Self::BASE_HZ, deg);
        let (b0, b1) = gesture_bend(f, 1);
        // The grace bend on the fundamental only (the bar doesn't slide).
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
                    ..Partial::default()
                },
                Partial::default(),
                Partial::default(),
            ],
            // The yarn mallet: a soft broad noise pad falling out of 700 Hz in
            // 8 ms.
            n_lvl: 0.35,
            n_f0: 700.0,
            n_f1: 250.0,
            n_glide: 0.008,
            n_q: 0.5,
            lp_cut: 3000.0,
            ..Voice::default()
        };
        s.spawn(bar, g * 0.42, pan);
        if glint {
            // The glint: the bar's tuned second mode two octaves up, fixed
            // pitch, gone in ~30 ms while the bar rings for 130.
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
                        ..Partial::default()
                    },
                    Partial::default(),
                    Partial::default(),
                ],
                lp_cut: 6000.0,
                ..Voice::default()
            };
            s.spawn(glint_v, g * 0.12, pan);
        }
    }
}

impl Palette for MarimbaPalette {
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        deg: i32,
        _col_off: i32,
    ) {
        match kind {
            SoundKind::Typed => {
                Self::bar(s, deg, 0.0, 0.13, true, g, ev.pan);
            }
            SoundKind::Navigation => {
                // The dead stroke: the note choked in 50 ms, no glint, lifted
                // ×1.25 so it stays a tier-0 whisper.
                Self::bar(s, deg, 0.0, 0.05, false, g * 1.25, ev.pan);
            }
            SoundKind::Jump => {
                // "ta-DA": the note, then 70 ms later its fifth (mirrored pan)
                // and octave together.
                Self::bar(s, deg, 0.0, 0.13, true, g, ev.pan);
                Self::bar(s, deg + 3, 0.07, 0.13, false, g * 0.4, -ev.pan);
                Self::bar(s, deg + 5, 0.07, 0.13, true, g * 0.45, ev.pan * 0.5);
            }
            // Every other kind is designed before dispatch or routed to
            // `push_v2`.
            _ => {}
        }
    }

    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, u1: f32, _u2: f32) -> (f32, f32) {
        // The resonators: C3 and its fifth, swelling on the slow LFO and
        // lowpassed. Mono (side 0).
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 130.8 * dt).fract();
        b.ph2 = (b.ph2 + 196.2 * dt).fract();
        let sm = (0.6 + 0.4 * u1) * (sin01(b.ph1) + 0.7 * sin01(b.ph2));
        let k = (600.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (sm - b.lp1);
        (b.lp1 * lvl * 0.018, 0.0)
    }

    /// Tracks the palette's own base (C4): the bonk clashes and the movement
    /// family sings in the marimba's register.
    fn anchor_hz(&self) -> f32 {
        Self::BASE_HZ
    }
}

/// Felt, a felt-muted piano: harmonic FM on the fundamental settling to a
/// pure sine in 20 ms (hammer knock, then string), a dark 2f/3f pair, all
/// under 1.3 kHz. The darkest palette, at the lowest anchor (G3, 196.0).
/// Enter is a soft broken triad; the bed is two barely-detuned low strings
/// humming in sympathy.
///
/// Reached only through [`SoundVoice::Felt`]. Uses the grace bend on the
/// fundamental like the marimba.
struct FeltPalette;

impl FeltPalette {
    /// The melodic base — G3, the lowest voice in the roster.
    const BASE_HZ: f32 = 196.0;

    /// One felted note on lattice degree `deg`, entered from below.
    /// `thud` scales the hammer pad (0 for the una-corda whisper), `decay` is
    /// the string's.
    fn note(s: &mut TrailSynth, deg: i32, delay: f32, decay: f32, thud: f32, g: f32, pan: f32) {
        let f = s.melody_hz(Self::BASE_HZ, deg);
        let (b0, b1) = gesture_bend(f, 1);
        let f0 = b1 + (b0 - b1) * KEY_BELL_BEND_SHARE;
        let v = Voice {
            delay,
            dur: 0.6,
            attack: 0.004,
            decay,
            p: [
                // The string: the law partial (strictly loudest, lands
                // exactly), with the hammer's knock as harmonic FM settling
                // to sine in ~20 ms.
                Partial {
                    lvl: 0.5,
                    f0,
                    f1: f,
                    glide: KEY_BELL_BEND_TAU,
                    fm_ratio: 1.0,
                    fm_i0: 0.35,
                    fm_tau: 0.02,
                    ..Partial::default()
                },
                // The dark body: octave and twelfth, felt-quiet.
                Partial {
                    lvl: 0.22,
                    f0: f * 2.0,
                    f1: f * 2.0,
                    ..Partial::default()
                },
                Partial {
                    lvl: 0.10,
                    f0: f * 3.0,
                    f1: f * 3.0,
                    ..Partial::default()
                },
            ],
            // The felt pad — the hammer's soft thud, a low broad noise
            // falling out of 350 Hz.
            n_lvl: 0.3 * thud,
            n_f0: 350.0,
            n_f1: 120.0,
            n_glide: 0.012,
            n_q: 0.5,
            lp_cut: 1300.0,
            ..Voice::default()
        };
        s.spawn(v, g * 0.5, pan);
    }
}

impl Palette for FeltPalette {
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        deg: i32,
        _col_off: i32,
    ) {
        match kind {
            SoundKind::Typed => {
                Self::note(s, deg, 0.0, 0.19, 1.0, g, ev.pan);
            }
            SoundKind::Navigation => {
                // Una corda — the soft pedal's whisper: shorter, no hammer
                // thud, a shade under the keystroke (the tier-0 whisper).
                Self::note(s, deg, 0.0, 0.09, 0.0, g * 0.8, ev.pan);
            }
            SoundKind::Jump => {
                // A soft broken triad: the note over its bass octave, the fifth
                // a beat behind.
                Self::note(s, deg, 0.0, 0.19, 1.0, g, ev.pan);
                Self::note(s, deg - 5, 0.0, 0.22, 1.0, g * 0.7, ev.pan * 0.5);
                Self::note(s, deg + 3, 0.06, 0.19, 0.5, g * 0.5, -ev.pan * 0.5);
            }
            // Every other kind is designed before dispatch or routed to
            // `push_v2`.
            _ => {}
        }
    }

    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, _u1: f32, _u2: f32) -> (f32, f32) {
        // The dampers lifted: two low G2 strings, detuned to breathe every
        // ~3 s, dark under 400 Hz. Mono.
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 98.0 * dt).fract();
        b.ph2 = (b.ph2 + 98.3 * dt).fract();
        let sm = sin01(b.ph1) + sin01(b.ph2);
        let k = (400.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (sm - b.lp1);
        (b.lp1 * lvl * 0.02, 0.0)
    }

    /// Tracks the palette's own base (G3): the bonk clashes and the movement
    /// family sings in the felt's register.
    fn anchor_hz(&self) -> f32 {
        Self::BASE_HZ
    }
}

/// Sine-table resolution: 2^10 points over one turn. Each extra bit divides
/// the interpolation error by four (see [`sin01`]).
const SIN_TAB_BITS: u32 = 10;
const SIN_TAB_LEN: usize = 1 << SIN_TAB_BITS;

/// `sin(2π·i/1024)`, plus one wrap-around guard entry so the interpolation
/// never needs a second index wrap. 4 KB of `.rodata`.
///
/// (`approx_constant` fires on the quarter-turn entries, which are sine
/// values; `excessive_precision` on the shortest f32 round-trip forms.)
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

/// `sin(2π·ph)` for phase in turns: a 1024-point table with linear
/// interpolation instead of `sinf`.
///
/// Worst-case error is `(2π)²·h²/8 ≈ 4.7e-6` (-106.5 dB re full scale), so at
/// this synth's levels the residual sits near -128 dBFS, below the 16-bit
/// LSB. It is one fixed distortion product at 1024× the partial frequency,
/// independent of time, polyphony and level. Phase accumulators are
/// unchanged, so pitch, envelopes and lifetimes are exact; only the
/// waveform's last bits differ.
#[inline]
fn sin01(ph: f32) -> f32 {
    // Most call sites pass `.fract()`ed phases, but the FM carrier's argument
    // can leave [0,1), so wrap here. `x` can round to exactly 1.0 for a tiny
    // negative `ph`: the index is masked and the fraction comes from the
    // unmasked truncation, landing on `SIN_TAB[0]` with a zero fraction.
    let x = ph - ph.floor();
    let z = x * SIN_TAB_LEN as f32;
    let j = z as usize;
    let f = z - j as f32;
    let i = j & (SIN_TAB_LEN - 1);
    let a = SIN_TAB[i];
    let b = SIN_TAB[i + 1];
    a + f * (b - a)
}

/// xorshift32 — deterministic, allocation-free. Uniform in [0,1). A free fn
/// so `render` can draw while it holds a voice borrow.
#[inline]
fn xorshift01(rng: &mut u32) -> f32 {
    let mut x = *rng;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *rng = x;
    (x >> 8) as f32 * (1.0 / 16_777_216.0)
}

/// Triangle in turns, ±1.
#[inline]
fn tri(ph: f32) -> f32 {
    4.0 * (ph - 0.5).abs() - 1.0
}

/// One sample of §9.7's bus peak limiter on one channel. `env` is the peak
/// follower (instant attack, `k_rel` per-sample release); `g` the applied
/// gain, attacking toward `threshold / env` by `k_att` while the follower is
/// over the threshold and exactly `1.0` once it is back under. Hard knee.
#[inline]
fn limit(y: f32, env: &mut f32, g: &mut f32, k_att: f32, k_rel: f32) -> f32 {
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

/// Gentle tanh-shaped saturation + hard clamp: transparent at design levels,
/// a graceful ceiling if many jumps pile up.
#[inline]
fn soft_clip(x: f32) -> f32 {
    let y = x * (27.0 + x * x) / (27.0 + 9.0 * x * x); // tanh-ish, cheap
    y.clamp(-0.98, 0.98)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The glyph rank ([`EventMeta::rank`], §3.1's R2) is ordered (letters
    /// 1..=26 and digits 27..=36 in order, so a signed difference between keys
    /// is a musical distance), total (no input panics) and lossy (case folded,
    /// punctuation in eight buckets, so a rank stream cannot be read as text).
    #[test]
    fn the_glyph_rank_is_ordered_total_and_lossy() {
        assert_eq!(typed_glyph_rank(None), 0, "no key behind the cue");
        // Ordered — the letters.
        for (k, c) in ('a'..='z').enumerate() {
            assert_eq!(typed_glyph_rank(Some(c)), 1 + k as u8, "{c:?}");
            assert_eq!(
                typed_glyph_rank(Some(c.to_ascii_uppercase())),
                typed_glyph_rank(Some(c)),
                "{c:?}: case must fold"
            );
        }
        // Ordered — the digits, above every letter.
        for (k, c) in ('0'..='9').enumerate() {
            assert_eq!(typed_glyph_rank(Some(c)), 27 + k as u8, "{c:?}");
        }
        // Lossy — eight punctuation buckets, and every printable that is
        // none of the above shares one rank above them.
        let buckets: std::collections::BTreeSet<u8> = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~"
            .chars()
            .map(|c| typed_glyph_rank(Some(c)))
            .collect();
        assert!(
            (2..=8).contains(&buckets.len()),
            "ASCII punctuation collapsed into {} ranks; the table says eight",
            buckets.len()
        );
        assert!(
            buckets.iter().all(|r| (37..=44).contains(r)),
            "a punctuation rank landed outside 37..=44: {buckets:?}"
        );
        for c in ['漢', '£', 'é', '٣', ' ', '\t'] {
            assert_eq!(typed_glyph_rank(Some(c)), 45, "{c:?}");
        }
        // Total — no input panics, and nothing exceeds the documented top.
        for u in 0u32..0x3000 {
            if let Some(c) = char::from_u32(u) {
                assert!(typed_glyph_rank(Some(c)) <= 45, "{c:?} ranked out of range");
            }
        }
    }

    /// The glyph class ([`glyph_class`]) is total over ASCII punctuation: every
    /// mark lands in a mark bucket, never the letter's tine or the digit's;
    /// letters of either case, the space, `None` and non-ASCII are `LETTER`;
    /// and no `char` classes above `DASH` (`SECRET` is stamped by the host
    /// alone).
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

    /// Which engine speaks is decided per event, by the look and the voice
    /// alone ([`TrailSynth::v2_engaged`]): follow-the-look under the rainbow
    /// kitty and the music box by either name take the v2 fork; an explicit
    /// instrument auditions itself under the music box's look; the bonk stays
    /// on the v1 chain (§11).
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

    /// The block pre-roll (§3.4 c): a cue stamped `block_lead_s` sounds exactly
    /// that much later than unstamped, on a v1 palette and on the music box,
    /// and the latch is cleared on the way out so nothing after the push
    /// inherits it. A NaN lead is refused; a wild one is clamped to the
    /// engine's ceiling, never scheduled into next week.
    #[test]
    fn the_block_pre_roll_delays_every_voice_of_the_push_and_nothing_after() {
        const LEAD_S: f32 = 0.005;
        // Bed off: the first non-zero sample must be the voice's onset, not
        // the bed swelling in under it.
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

    fn ev(style: GlowStyle, kind: SoundKind) -> SoundEvent {
        SoundEvent {
            style,
            voice: SoundVoice::Style,
            kind: SoundGesture::Trail(kind),
            pan: 0.0,
            heat: 0.5,
            hue: 0.3,
            gain: 0.4,
            // The neutral identity tone: the byte-identity pins below hold
            // only on this path.
            tone: Tone::Technical,
            // Bed on: the v0.56 oracle has no bed gate. Bed-off tests build
            // their own `bed: false` events.
            bed: true,
            // Unshifted: the identity. The capital's lift has its own proof.
            shifted: false,
        }
    }

    fn bonk(style: GlowStyle) -> SoundEvent {
        SoundEvent {
            style,
            voice: SoundVoice::Style,
            kind: SoundGesture::Words(WordGesture::Bonk),
            pan: 0.0,
            heat: 0.5,
            hue: 0.3,
            gain: 0.4,
            tone: Tone::Technical,
            // Structurally irrelevant on a Words gesture: a bonk never
            // reaches the bed feed.
            bed: true,
            shifted: false,
        }
    }

    fn output(style: GlowStyle, gesture: OutputGesture) -> SoundEvent {
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

    /// The same trail event under an explicit tone (the tone-proof helper).
    fn toned(style: GlowStyle, kind: SoundKind, tone: Tone) -> SoundEvent {
        SoundEvent {
            tone,
            ..ev(style, kind)
        }
    }

    /// Octave-reduce a frequency ratio into `[1.0, 2.0)` — the consonance
    /// proofs' shared interval normal form.
    fn reduce(mut r: f32) -> f32 {
        while r < 1.0 {
            r *= 2.0;
        }
        while r >= 2.0 {
            r /= 2.0;
        }
        r
    }

    /// The bonk's two rub zones (minor-second crush, tritone band), shared by
    /// every consonance test.
    fn assert_outside_bonk_zones(raw: f32, ctx: &str) {
        let r = reduce(raw);
        let m2_zone = r > 1.0 + 1e-4 && r < 1.09;
        let tritone_zone = r > 1.395 && r < 1.43;
        assert!(
            !m2_zone && !tritone_zone,
            "{ctx}: interval {r} lands in a bonk rub zone"
        );
    }

    /// The eight v1 palettes: every look whose `Style` event walks the v1
    /// admission chain. The rainbow kitty look is the music box (tested in
    /// `rainbow_kitty_v2`); see [`LOOKS`] for engine-agnostic sweeps.
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

    /// Every look with a sound: [`STYLES`] plus the rainbow kitty (the music
    /// box), for the engine-agnostic laws (sound and exact decay, first-buffer
    /// onset, `Of(look)` equals `Style`, deletion is a poof, the fidelity
    /// oracle).
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

    /// Every voice the picker offers ([`SoundVoice::ALL`]), so a new voice
    /// joins every voice-agnostic sweep. Where a v1 admission-chain law cannot
    /// apply to the music box, the sweep excludes it by name
    /// ([`is_music_box`]) with the reason beside it.
    const VOICES: &[SoundVoice] = SoundVoice::ALL;

    /// Whether (voice, look) is the music box, restating
    /// [`TrailSynth::v2_engaged`]: the voice by name, or `Style` under the
    /// rainbow kitty.
    fn is_music_box(voice: SoundVoice, style: GlowStyle) -> bool {
        matches!(
            voice,
            SoundVoice::RainbowKittyV2 | SoundVoice::Of(GlowStyle::RainbowKitty)
        ) || (voice == SoundVoice::Style && style == GlowStyle::RainbowKitty)
    }

    /// Every gesture the host can cue, in loudness-ladder order, so a new
    /// [`SoundKind`] cannot land without an onset test.
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

    /// A cue is audible in the first buffer rendered after it, with nothing
    /// ahead of it: a pre-delayed leading voice or a grace-note-first gesture
    /// would slow every keystroke in that style. Checked on the earliest live
    /// voice's delay and on the rendered samples, since a `delay = 0` voice
    /// whose envelope leaves the buffer empty is just as late.
    #[test]
    fn every_gesture_speaks_in_its_first_post_cue_buffer() {
        // The host's real block geometry (`trail_audio::BUFFER_FRAMES`).
        const BLOCK: usize = 512;
        // 0.33 ms: three partials at random phases cannot all sit at a zero
        // crossing this long, while any millisecond of pre-delay fails.
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
                    // Bed off: this pins the gesture's onset; the bed's swell
                    // could mask a late note.
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

    /// Every palette × gesture produces sound, stays in range, never NaNs, and
    /// decays to exact silence, one row per palette:
    ///
    /// * the style palettes, every look, all five trail kinds and the bonk;
    /// * the mech override and each instrument on either look, a Glide
    ///   included.
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
                    // 6 s is enough for every decay + bed exhale.
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

    // -- the typing-sound picker: every voice, by name --------------------

    /// The same trail event under an explicit typing-sound voice.
    fn voiced(voice: SoundVoice, style: GlowStyle, kind: SoundKind) -> SoundEvent {
        SoundEvent {
            voice,
            ..ev(style, kind)
        }
    }

    /// The isolated stance: one gesture of `voice` on a fresh
    /// synth (seed 0x5EED_1234, rainbow kitty look, gain 0.4, heat 0.5, pan 0,
    /// hue 0, bed off), peak over a 2.4 s tail.
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

    /// The three sound-only instruments with palettes of their own (the `Of(_)`
    /// voices alias the style table; Mech and the music box are tested
    /// elsewhere).
    const NEW_VOICES: [SoundVoice; 3] = [
        SoundVoice::Typewriter,
        SoundVoice::Marimba,
        SoundVoice::Felt,
    ];

    /// The roster round-trips: every canonical name parses back to its voice
    /// (case- and whitespace-insensitive), every alias lands on its listed
    /// voice without shadowing a canonical name, `auto` is the look's voice,
    /// and junk is refused.
    #[test]
    fn voice_roster_round_trips() {
        assert_eq!(SoundVoice::parse("auto"), Some(SoundVoice::Style));
        for &v in SoundVoice::ALL {
            assert_eq!(SoundVoice::parse(v.name()), Some(v), "{v:?}");
            assert_eq!(
                SoundVoice::parse(&format!("  {}  ", v.name().to_ascii_uppercase())),
                Some(v)
            );
        }
        for &(alias, voice) in SoundVoice::ALIASES {
            assert_eq!(SoundVoice::parse(alias), Some(voice), "alias {alias:?}");
            assert!(
                SoundVoice::ALL.iter().all(|v| v.name() != alias),
                "alias {alias:?} shadows a canonical name"
            );
        }
        for junk in ["", "  ", "plasma", "glass", "auto bell"] {
            assert_eq!(SoundVoice::parse(junk), None, "{junk:?}");
        }
    }

    /// A standalone style voice is the style's palette verbatim: `Of(s)` under
    /// any look renders bit-identical to `Style` under look `s` for every
    /// gesture (Kill's tint and the bed included). Proven over `LOOKS`, so it
    /// holds for the rainbow kitty's music box too.
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
                s.push(voiced(voice, look, kind)); // `ev` carries bed: true
                for _ in 0..8 {
                    s.render(&mut buf);
                    out.extend_from_slice(&buf);
                }
            }
            // The bonk carries the voice too (its clash register follows the
            // speaking palette's anchor).
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
            // Under a different look (Lumen, or Comet when the style is
            // Lumen), so the look demonstrably does not leak in.
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

    /// Every pitched instrument lands its keystroke on the melody note in its
    /// own register. (Deletions are poofs:
    /// `the_deletion_is_a_poof_and_only_a_poof`.)
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

    /// Beds: the keyboards' (Mech, Typewriter) are structurally silent, so
    /// `bed: true` renders bit-identical to `bed: false`, while the two
    /// instruments' designed beds change the render when fed.
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

    /// A sustained 25 cps flood never gets louder than a single event (the
    /// governor's job) and never nears the clip ceiling. One row per palette:
    ///
    /// * every look's palette, the music box's included, holds the bound
    ///   relative to a single event of the same palette and the absolute one;
    /// * the mech override and each instrument hold the absolute ceiling.
    #[test]
    fn flood_is_ducked() {
        struct Flood {
            label: String,
            event: SoundEvent,
            seed: u32,
            /// 960-frame blocks rendered after each key.
            blocks_per_key: usize,
            /// Sweep the pan across the flood (the look rows).
            pan_sweep: bool,
            /// Also hold the flood to a single event's peak.
            relative: bool,
        }
        let palette = |label: String, event: SoundEvent| Flood {
            label,
            event,
            seed: 3,
            blocks_per_key: 2, // 2×960 frames = 40 ms per key
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
            // A 3-second 25 cps flood.
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
                // …vs a single event's peak.
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
                    // The absolute slack is in output units, so it scales with
                    // MASTER like `flood` and `single` do.
                    flood <= single * 2.5 + MASTER * (0.05 / 0.9),
                    "{label} flood peak {flood} vs single {single} — governor failed"
                );
            }
            assert!(flood < 0.5, "{label} flood absolute peak too hot: {flood}");
        }
    }

    /// Backlog depth cannot buy a click. `since_voice` advances only with
    /// rendered samples and a drain pushes the whole backlog in one host tick,
    /// so every cue after the first sits inside [`MIN_GAP`] and is thinned. The
    /// music box never thins a keystroke, so for it §14's lane cap and the bus
    /// limiter bound the output instead.
    #[test]
    fn a_drained_cue_batch_speaks_once_however_deep_the_backlog() {
        for style in LOOKS {
            let one = {
                let mut s = TrailSynth::new(48_000.0, 11);
                s.push(ev(style, SoundKind::Typed));
                s.live_voices()
            };
            assert!(one > 0, "{style:?}: a lone keystroke must speak");
            // 8 is the glow engine's `MAX_SOUND_CUES`; 24 any larger cap.
            // Neither adds a voice.
            for depth in [8usize, 24] {
                let mut s = TrailSynth::new(48_000.0, 11);
                for _ in 0..depth {
                    s.push(ev(style, SoundKind::Typed));
                }
                if style == GlowStyle::RainbowKitty {
                    // Every cue speaks; the lane cap and limiter bound the
                    // output below.
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

    /// Bed-off events are silent: `bed: false` events never energise the bed
    /// (the level floor never lifts) while the notes still speak; the next
    /// `bed: true` event re-energises it.
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

    /// The sky's consonance law: every pitch of the music box's bed is an
    /// integer degree on the active tone lattice, so for every tone table,
    /// every bed × bed and bed × melody interval (octave-reduced, inversions
    /// included) stays outside the bonk's rub zones
    /// (`assert_outside_bonk_zones`).
    #[test]
    fn sky_bed_pitches_stay_on_the_active_lattice_for_every_tone() {
        // The sky voices the live chord's lit degrees, chord by chord.
        let mut bed_degrees: Vec<i32> = Vec::new();
        for chord in 0..rainbow_kitty_v2::SKY_BED_CHORDS {
            bed_degrees.extend(rainbow_kitty_v2::sky_bed_degrees(chord));
        }
        // Melody notes the bed must sit under: the walk's clamped range
        // (0..=8 across all tones) plus the ±2 column offset.
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

    // -- bonk behavioral proofs ---------------------------------------------

    /// Admission priority: within the min-gap window a Typed event is thinned
    /// but a Bonk always speaks.
    #[test]
    fn bonk_bypasses_min_gap_thinning_like_jump() {
        let mut s = TrailSynth::new(48_000.0, 21);
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        let after_first = s.live_voices();
        assert!(after_first >= 1);
        // Immediately again — inside MIN_GAP, thinned.
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        assert_eq!(s.live_voices(), after_first, "typed within gap must thin");
        // The bonk lands regardless (clash + thump = two voices).
        s.push(bonk(GlowStyle::Lumen));
        assert_eq!(
            s.live_voices(),
            after_first + 2,
            "bonk within gap must be admitted"
        );
    }

    /// Prism wake's sole cue survives the shared keystroke gap (it is governed
    /// once per episode upstream) and consumes no rng, melody state or
    /// admission slot.
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

        // No rendered time has passed since Enter: the cue lands inside its gap.
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

        // Once the Enter's own gap has elapsed, inserting an output cue just
        // before the next key cannot thin that key.
        s.since_voice = MIN_GAP;
        s.push(output(GlowStyle::Lumen, OutputGesture::Shimmer));
        let after_output = s.live_voices();
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        assert!(
            s.live_voices() > after_output,
            "an unauthored cue cannot steal the following key's beat"
        );
    }

    /// Typing joins the song's key: while the cat sings, a typed note
    /// transposes with the riff (the latched key is root + mode, the shift
    /// `design_celebration` applies); the borrowed key is handed back with the
    /// sing duck even while continuous typing keeps the synth audible.
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

        // `sig = 4` is root +2 and mode rotation +2.
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

        // The hand-back must not depend on silence: 12 s of typing at ~20 cps
        // keeps the synth audible, and the key still returns with the duck.
        let mut busy = TrailSynth::new(48_000.0, 7);
        busy.push(bar);
        assert_ne!(busy.song_key, 0, "the song latched a key");
        let mut blk = vec![0.0f32; 480 * CHANNELS]; // 10 ms
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

    /// The bonk is the one discordant voice: its clash partials sit a
    /// just-intonation minor second and tritone above the current walk degree,
    /// and pushing it moves neither the walk nor the bed.
    #[test]
    fn bonk_is_discordant_against_the_walk_and_moves_nothing() {
        let mut s = TrailSynth::new(48_000.0, 33);
        let walk_before = s.walk;
        // The rainbow kitty's anchor (C5) at the current degree
        // (`Palette::anchor_hz`).
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

    /// The master duck: a bonk snaps it to 1, rendering decays it, and while
    /// it is up the non-exempt melody/bed mix is quieter than an un-bonked
    /// twin. Measured after the bonk's own voices have died.
    #[test]
    fn bonk_ducks_the_melody_around_it() {
        // Two identical Beam phrases (the bed hum sustains); one gets a bonk.
        let build = || {
            let mut s = TrailSynth::new(48_000.0, 77);
            let mut buf = [0.0f32; 960]; // 480 frames = 10 ms per block
            for _ in 0..6 {
                s.push(ev(GlowStyle::Beam, SoundKind::Typed));
                for _ in 0..4 {
                    s.render(&mut buf); // 40 ms per key: bed energy builds
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
        // Skip past the bonk voices (clash dur 0.30 s ⇒ 32 × 10 ms), then
        // compare the remaining bed exhale energy over the next 0.2 s.
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
        // And the duck recovers: fully at rest again within a few seconds
        // (the sub-audibility snap, or the exact-silence path zeroing it).
        for _ in 0..300 {
            bonked.render(&mut buf);
        }
        assert_eq!(bonked.duck, 0.0, "the duck must snap back to exact rest");
    }

    // -- Sing-along proofs ---------------------------------------------------

    /// One sing-along riff bar for a held key's signature. `heat` 1.0 (the
    /// host pins momentum to full while armed); `bed` false (celebrations
    /// never reach the bed feed).
    fn riff_sig(style: GlowStyle, bar: u16, sig: u32) -> SoundEvent {
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

    /// The neutral-signature riff bar — the reference voicing (root 0, home
    /// mode) the axis-agnostic celebration proofs speak in.
    fn riff(style: GlowStyle, bar: u16) -> SoundEvent {
        riff_sig(style, bar, crate::kitty_sing::NEUTRAL_SIGNATURE)
    }

    /// A riff bar bypasses min-gap thinning like Jump and the Bonk: a bar right
    /// after an admitted typed voice still schedules its full phrase. It never
    /// feeds the bed, even with the event's `bed` flag on.
    #[test]
    fn celebration_bypasses_min_gap_thinning() {
        let mut s = TrailSynth::new(48_000.0, 5);
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
        let before = s.live_voices();
        s.push(riff(GlowStyle::RainbowKitty, 0)); // zero gap — would be thinned
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

    /// The sing duck: while the riff speaks, non-exempt melody voices are
    /// attenuated by the documented depth; once bars stop, the envelope
    /// returns to exact rest.
    #[test]
    fn celebration_ducks_the_melody_and_hands_back() {
        // Identical lone rainbow kitty typed notes; one renders under a pinned sing
        // duck. Bed off so the energy compared is exactly the melody voice.
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

        // Admission arms + holds the envelope…
        let mut s = TrailSynth::new(48_000.0, 9);
        s.push(riff(GlowStyle::RainbowKitty, 0));
        assert_eq!(s.sing, 1.0, "a riff bar must arm the sing duck");
        assert!(s.sing_hold > 0.0, "…and hold it for the bar");
        // ...and with no further bars it hands back to exact zero while the
        // synth stays live (quiet nav events keep it breathing, so `is_quiet`
        // never zeroes `sing`). 7 s covers hold (1.7 s) + τ·ln(1e4) ≈ 5.4 s.
        let mut buf = [0.0f32; 960];
        for block in 0..700 {
            // ~7 s
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

    fn outro_sig(sig: u32) -> SoundEvent {
        SoundEvent {
            kind: SoundGesture::Celebration(CelebrationGesture::outro(sig)),
            ..riff_sig(GlowStyle::RainbowKitty, 0, sig)
        }
    }

    /// §27, the outro ends on *do*: a duck-exempt lead whose fundamental is
    /// degree 0 in the run's key (`root + mode`) over the bass *do*, holding
    /// the sing duck for the outro's own length.
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

    /// §27, rain glints are muted under the sing: a music-box stardust chime
    /// spawns no voice while the sing duck holds and its glint once at rest;
    /// other palettes' glints are unaffected.
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

    /// A bass note rides the lead voice opening its beat, so no `REST` lead
    /// slot may sit under a sounding bass beat.
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

    /// The host truncates its `u64` bar counter into the payload, so the form
    /// must tile the payload's range exactly or the song jump-cuts per wrap.
    #[test]
    fn celebration_form_is_wrap_safe() {
        assert!(CELEBRATION_PHRASE_BARS.is_power_of_two());
        assert_eq!((usize::from(u16::MAX) + 1) % CELEBRATION_PHRASE_BARS, 0);
        // And the swung last eighth still starts inside its own bar — every
        // operand is a constant, so this is a build failure, not a test failure.
        const {
            assert!((7.0 + CELEBRATION_SWING) * CELEBRATION_EIGHTH < CELEBRATION_BAR_SECONDS);
        }
    }

    /// Polyphony budget: a bar's notes are all spawned at once, so the fullest
    /// bar must leave room for the typed layer under a held key.
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

    /// The riff inherits the active tone's lattice: its PENTA degrees are
    /// transposed onto the same table as the melody, so it never rubs the
    /// bonk's banned intervals against a moved melody.
    #[test]
    fn celebration_riff_follows_the_active_tone_lattice() {
        // A render hash of one riff bar with `self.tone` pre-set. Only pitch is
        // tone-dependent in the celebration path, so a divergence is a pitch
        // shift.
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

    // -- per-key song-axis proofs ---------------------------------------------

    /// The switch damp: consecutive bars under one signature never arm the
    /// release, but a bar under a new signature sends every ringing
    /// celebration voice into the [`CELEBRATION_DAMP_S`] release, so within
    /// 100 ms the old key's ring is gone while the new verse sings. An old-key
    /// voice still in pre-delay at the switch expires unheard.
    #[test]
    fn key_switch_damps_the_old_keys_ring() {
        let t = crate::kitty_sing::song_signature('t');
        let e = crate::kitty_sing::song_signature('e');
        assert_ne!(t, e, "fixture: two different songs");
        let mut s = TrailSynth::new(48_000.0, 42);
        let mut buf = [0.0f32; 960]; // 10 ms blocks
        // Two bars of one key, each rendered whole: the legato notes (1.5×
        // eighths) ring past the bar line by design.
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
        // The switch: the next bar carries the other key's signature.
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

        // A switch with the old bar still in pre-delay: the damp burns through
        // it, so an unstarted old-key voice never opens under the new verse.
        let mut s2 = TrailSynth::new(48_000.0, 42);
        s2.push(riff_sig(GlowStyle::RainbowKitty, 4, t));
        s2.push(riff_sig(GlowStyle::RainbowKitty, 5, e));
        for _ in 0..10 {
            s2.render(&mut buf); // 100 ms ≫ the 50 ms release
        }
        assert_eq!(
            s2.voices.iter().filter(|v| v.on && v.damp > 0.0).count(),
            0,
            "an unstarted old-key voice must expire unheard, not sound late"
        );
    }

    /// Cross-key difference: 'w', 'a' and 'z' get pairwise-different verse
    /// interval sequences. Intervals, not degrees, so a mere transpose cannot
    /// pass.
    #[test]
    fn cross_key_difference() {
        let intervals = |sig: u32| -> Vec<i32> {
            let mut degs = Vec::new();
            // Iterating `CELEBRATION_VERSE` is the per-bar loop, so the bar
            // count cannot drift from the flag table.
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

    /// The fill carries the root: the turnaround's sixteenths are transposed
    /// with the held key.
    #[test]
    fn fill_carries_root() {
        // sig 24: 24 % 5 == 4 ⇒ root +2, 24 % 3 == 0 ⇒ home mode — the
        // shift is pure root.
        let sig = 24u32;
        assert_eq!(celebration_root(sig), 2);
        assert_eq!(celebration_mode(sig), 0);
        let mut s = TrailSynth::new(48_000.0, 3);
        s.push(riff_sig(GlowStyle::RainbowKitty, 7, sig)); // the turnaround bar
        // Fill voices are the only sixteenths: dur is exactly one eighth
        // (phrase voices hold ≥ 1.5 eighths; `duck_exempt` skips tone_feel).
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

    /// The riff's tables over many signatures: the authored rhythm survives
    /// verbatim, every degree stays in the singable window, adjacent verse
    /// notes always move, the root register guard holds, and the shared
    /// chorus bars return the authored contour for every signature. The fill
    /// and the bass stay in their registers.
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

    /// The mode axis is audible alone: same root and contour with different
    /// mode classes renders differently.
    #[test]
    fn mode_rotation_alone_changes_the_chorus_color() {
        // 12 ≡ 22 ≡ 2 (mod 5) — one shared root; 12 ≡ 0, 22 ≡ 1 (mod 3).
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

    /// The neutral signature is the reference voicing (root 0, home mode);
    /// cross-pins `kitty_sing::NEUTRAL_SIGNATURE` to this module's decode.
    #[test]
    fn neutral_signature_is_the_reference_voicing() {
        use crate::kitty_sing::NEUTRAL_SIGNATURE;
        assert_eq!(celebration_root(NEUTRAL_SIGNATURE), 0);
        assert_eq!(celebration_mode(NEUTRAL_SIGNATURE), 0);
    }

    // -- tone-melody proofs --------------------------------------------------

    /// The no-beating invariant for every tone table: all pairwise intervals
    /// (octave wraps and inversions included) stay outside the bonk's two rub
    /// zones, and a transpose cannot add one. The bonk's own ratios landing
    /// inside the zones prove them non-vacuous.
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
                    // Octave inversion of the same pair.
                    assert_outside_bonk_zones(2.0 / reduce(r), &format!("{tone:?} inv {a}:{b}"));
                }
            }
        }
        // Non-vacuous: the bonk's identities do live in the zones.
        let m2 = reduce(BONK_MINOR_SECOND);
        assert!(m2 > 1.0 + 1e-4 && m2 < 1.09, "minor-second zone drifted");
        let tt = reduce(BONK_TRITONE);
        assert!(tt > 1.395 && tt < 1.43, "tritone zone drifted");
    }

    /// Each non-neutral tone audibly diverges from the pinned neutral render
    /// (table swap, transpose, walk bias, or feel — any of the four), while
    /// staying finite, bounded, and decaying to exact silence like every
    /// other gesture.
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
            // Full decay: every tone honours the exact-silence contract.
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

    // -- the bar: what makes the typed line a song --------------------------

    /// The bar is authored and consonant: the accents fall on an even beat
    /// that closes the bar (an irregular one is heard as level drift, not
    /// rhythm), and every ghost is an integer degree inside the melody's
    /// register and never the melody's own degree.
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

    /// Punctuation lands: Enter, a kill or a jump always sings at full level,
    /// and Enter resets the bar to its downbeat. The note after a real typing
    /// pause is the tune too.
    #[test]
    fn punctuation_and_the_note_after_a_pause_always_sing() {
        for kind in [SoundKind::Jump, SoundKind::Kill, SoundKind::Navigation] {
            let mut s = TrailSynth::new(48_000.0, 0x9011_1CE5);
            let mut buf = [0.0f32; 9600];
            // Land on a ghost slot, then punctuate.
            s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
            s.render(&mut buf);
            let sung = s.song_notes;
            s.push(ev(GlowStyle::Lumen, kind));
            assert!(
                s.song_notes > sung && s.song_accent,
                "{kind:?} must land as a melody note, not a ghost"
            );
        }
        // A pause re-opens the bar on its downbeat.
        let mut s = TrailSynth::new(48_000.0, 0x9011_1CE6);
        let mut buf = [0.0f32; 9600];
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        s.render(&mut buf);
        assert_ne!(s.song_pulse, 0, "fixture: mid-bar");
        // Longer than PHRASE_PAUSE_S of silence.
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

    // -- theme proofs (structural, not byte) --------------------------------

    /// The tune is played in order: sustained typing walks [`SONG_THEME`] from
    /// first note to last and back onto the hook.
    ///
    /// Sampled at the accents, since a ghost-slot keystroke accompanies the
    /// current note rather than stepping (see [`SONG_PULSE`]).
    #[test]
    fn the_theme_is_played_in_order_and_loops() {
        let mut s = TrailSynth::new(48_000.0, 0x0DDB_A11E);
        s.tone = Tone::Technical;
        let mut sung = Vec::new();
        // Two full passes plus a note, so the loop is proved and not just the
        // first statement.
        while sung.len() < SONG_THEME.len() * 2 + 1 {
            let before = s.song_notes;
            // Keep every note admitted (force the gap open) and in-phrase (no
            // pause, no Jump), so the playhead advances one note at a time.
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

    /// A pause closes a phrase and the next one answers: Enter and a
    /// comma-length gap cadence onto the phrase's final degree, then hand the
    /// playhead to the next phrase, never back to zero.
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
        // Three notes into the hook (phrase A, degrees 0 2 4 …).
        for _ in 0..3 {
            accent(&mut s, SoundKind::Typed);
        }
        assert_eq!(s.walk, 4, "fixture: three notes into the hook");
        assert_eq!(s.phrase_idx, 0, "…still inside phrase A");
        // Now an Enter. It must land on A's last degree (7, the hook's leap),
        // not on a generic tonic.
        s.push(ev(GlowStyle::Lumen, SoundKind::Jump));
        assert_eq!(
            i32::from(SONG_THEME[usize::from(SONG_FORM[1]) - 1]),
            s.walk,
            "a rest cadences onto the phrase's own last note, got {}",
            s.walk
        );
        assert_eq!(s.phrase_idx, 1, "…and opens the ANSWER, not the hook again");
        // And the next note is the answer's first degree — the form moved on.
        s.render(&mut buf);
        accent(&mut s, SoundKind::Typed);
        assert_eq!(
            s.walk,
            i32::from(SONG_THEME[usize::from(SONG_FORM[1])]),
            "the phrase after a rest continues the form"
        );
        // A pause does the same thing as an Enter (the ~0.6 s comma).
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

    /// A rest at a phrase mark cannot skip a phrase: the cadence fires only
    /// once the phrase has begun (`pos > start`), so a typist who pauses
    /// between words does not race through the form.
    #[test]
    fn a_rest_before_a_phrase_starts_cannot_skip_it() {
        let mut s = TrailSynth::new(48_000.0, 0x5C1D_0001);
        // A new synth starts at the hook's first note with `since_event = 1.0`
        // (a full rest); the first keystroke must still be the hook's first
        // degree.
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        assert_eq!(
            s.walk,
            i32::from(SONG_THEME[0]),
            "a session opens on the theme's first note"
        );
        assert_eq!(s.phrase_idx, 0, "…inside the first phrase, not past it");
        // One rest closes the hook and opens the answer.
        s.since_event = PHRASE_PAUSE_S + 0.1;
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        assert_eq!(s.phrase_idx, 1, "one rest, one phrase");
        // A second rest, immediately, with the answer not yet begun: it must
        // play the answer's first note rather than cadence past the phrase.
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

    /// The register never edits the tune: every tone's [`tone_register`] holds
    /// all of [`SONG_THEME`] shifted by its [`melody_lean`], so the fold in
    /// `advance_melody` is the identity. A ceiling one degree too low would
    /// fold the half-cadence's peak into a dip.
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

    /// The form is well-formed: the phrase index covers the theme exactly with
    /// no gap or overlap, and every phrase has a note, which makes
    /// `open_next_phrase`'s modular step total.
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

    /// The bass walk is a rotation, not a restrike: a root does not come back
    /// soon after it sounded, for every tone, palette and song key. Stated in
    /// Hz, since two degrees folding to one pitch would be the same defect.
    /// That the mix plays this table word for word is
    /// `the_space_is_a_lawful_walking_bass`.
    #[test]
    fn the_bass_walk_never_returns_inside_four_words() {
        /// A word may not hear its own root back inside this many words.
        const MIN_RETURN: usize = 4;
        // All five lattice roots: with only four, twelve steps force some
        // root to recur too soon for this floor.
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
                            // Cyclic: the walk wraps, so the distance between
                            // step 11 and step 0 is one word, not eleven.
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

    /// The register fold is total, in range, and never maps two different
    /// registers to one pitch.
    #[test]
    fn the_register_fold_is_total_and_never_flattens() {
        for (lo, hi) in [(0, 7), (0, 6), (0, 8), (3, 3)] {
            for v in -40..40 {
                let f = fold_register(v, lo, hi);
                assert!(f >= lo && f <= hi, "fold({v}) = {f} escaped {lo}..{hi}");
            }
        }
        // A clamp maps every out-of-range value to one endpoint; a reflection
        // keeps them distinct as long as they are within a span of each other.
        assert_ne!(fold_register(8, 0, 7), fold_register(9, 0, 7));
        assert_ne!(fold_register(-1, 0, 7), fold_register(-2, 0, 7));
    }

    // -- The gesture family -------------------------------------------------

    /// Spawn the family gesture `kind` from a settled melody state and return
    /// the synth plus its new voices' `(landing pitch, entry pitch)` pairs
    /// (loudest partial of each), ordered by pitch.
    fn family_voices(style: GlowStyle, kind: SoundKind) -> (TrailSynth, Vec<(f32, f32)>) {
        family_voices_in(SoundVoice::Style, style, kind, false)
    }

    /// [`family_voices`] under an explicit typing-sound voice (the picker's
    /// standalone instruments speak from the same settled melody state), the
    /// probe `shifted` or not.
    fn family_voices_in(
        voice: SoundVoice,
        style: GlowStyle,
        kind: SoundKind,
        shifted: bool,
    ) -> (TrailSynth, Vec<(f32, f32)>) {
        let mut s = TrailSynth::new(48_000.0, 0xFA_1117);
        let mut buf = [0.0f32; 1024]; // 512 frames ≈ 10.7 ms
        // Three settling keystrokes ~270 ms apart (under PHRASE_PAUSE_S), so
        // the probe stays in the phrase and lands on the bar's next accent.
        // Voices are diffed against the pool rather than waited out, so only
        // what this gesture spawned is measured.
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

    /// A deletion undoes: type five, delete five, type five again and the tune
    /// has walked five notes, not ten.
    #[test]
    fn a_deletion_does_not_advance_the_song() {
        let mut a = TrailSynth::new(48_000.0, 0xD0_1E_7E);
        let mut b = TrailSynth::new(48_000.0, 0xD0_1E_7E);
        let mut buf = [0.0f32; 1024]; // ~10.7 ms, clear of every gap
        for _ in 0..5 {
            a.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
            b.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
            for _ in 0..9 {
                a.render(&mut buf);
                b.render(&mut buf);
            }
        }
        // `b` additionally corrects itself twelve times.
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

    /// The space is a lawful bass: it plays the walk's current root, folded
    /// into one register on a lattice degree, independent of the melody's
    /// degree, and it composes nothing.
    #[test]
    fn the_space_is_a_lawful_walking_bass() {
        let (s, spaces) = family_voices(GlowStyle::Lumen, SoundKind::Space);
        // Sorted by pitch: the root first.
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
        // The bass root a space just spawned.
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
        // It walks: word i plays the walk's root i.
        let mut w = TrailSynth::new(48_000.0, 0xBA_55_11);
        let mut buf = [0.0f32; 2048];
        for i in 0..SONG_BASS.len() {
            let f = new_root(&mut w);
            let want = s.space_root_hz(anchor, i as u8);
            assert!(
                (f - want).abs() < 0.5,
                "word {i} played {f} Hz, not the walk's root {want} Hz"
            );
            // A letter closes the whitespace run, so the next space is a fresh
            // word head and takes the next root.
            w.render(&mut buf);
            w.since_voice = 1.0;
            w.push(ev(GlowStyle::Lumen, SoundKind::Typed));
            w.render(&mut buf);
        }
        // Independent of the melody: moving the tune with the word count held
        // fixed must not move the bass.
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
        // The space composes nothing: the theme's playhead is exactly what one
        // Typed probe leaves behind (family_voices pushes Typed first, then the
        // probed kind).
        let (tp, _) = family_voices(GlowStyle::Lumen, SoundKind::Typed);
        assert_eq!(
            s.theme_pos,
            tp.theme_pos.saturating_sub(1),
            "a space must not step the song (the Typed probe stepped once more)"
        );
    }

    /// The word is the downbeat: a whitespace run's head re-bars the pulse, so
    /// every word's first letter lands on an accent. The space is the bass
    /// opening the bar under it.
    #[test]
    fn a_word_boundary_re_bars_the_pulse() {
        let mut s = TrailSynth::new(48_000.0, 0xBA_12_11);
        let mut buf = [0.0f32; 2048];
        // Type a word of awkward length, so the bar is left mid-figure.
        for _ in 0..5 {
            s.since_voice = 1.0;
            s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
            s.render(&mut buf);
        }
        assert_ne!(s.song_pulse, 0, "fixture: the bar is mid-figure");
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Space));
        assert_eq!(s.song_pulse, 0, "a word boundary opens a fresh bar");
        // …so the next letter sings.
        let sung = s.song_notes;
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        assert_eq!(
            s.song_notes,
            sung + 1,
            "the first letter of a word must be an accent"
        );
        assert!(s.song_accent, "…and must be designed as one");
        // A run re-bars once, so a four-space indent is not four downbeats.
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
        // Head, then tail: the tail must leave the bar alone.
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

    /// A whitespace run is one gesture: one bass root and one walk step, with
    /// every tail space still speaking (never silence, never a second root).
    /// Every tail space clearing [`SPACE_STEP_MIN_GAP`] plays an indent step
    /// one lattice degree higher, holding its top note once the next degree
    /// would cross [`SPACE_STEP_HI_HZ`]. A letter closes the run and the next
    /// space takes the next root.
    #[test]
    fn a_whitespace_run_lands_one_downbeat() {
        let mut s = TrailSynth::new(48_000.0, 0x5A_CE_00);
        // ~83 ms: past MIN_GAP and SPACE_STEP_MIN_GAP (every space admitted
        // and every tail space steps), inside SPACE_DUR_S (a stacked root
        // would still be sounding).
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
                // The first repeat: the next degree would have crossed the
                // roof. The widest pentatonic step is the minor third (6/5).
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
        // A letter closes the run: the next space opens a new one and takes
        // the next root.
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

    /// A held spacebar cannot machine-gun the indent steps
    /// ([`SPACE_STEP_MIN_GAP`]): thirty spaces 33 ms apart (auto-repeat) mint
    /// between 4 and 13 steps in the second, and every admitted space still
    /// breathes.
    #[test]
    fn a_held_spacebar_cannot_machine_gun_the_indent_steps() {
        let mut s = TrailSynth::new(48_000.0, 0x5A_CE_33);
        let mut buf = [0.0f32; 3_168]; // 33 ms
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

    /// The downbeat is monophonic: two equal-frequency voices at random phases
    /// comb-filter, so a new word boundary damps the live root instead of
    /// stacking on it.
    #[test]
    fn two_word_boundaries_never_stack_one_bass_note() {
        let mut s = TrailSynth::new(48_000.0, 0xBA_55_01);
        let mut buf = [0.0f32; 4096]; // ~43 ms, well inside SPACE_DUR_S
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

    /// The ting's admission never claims the min-gap beat: a capital typed at
    /// speed still clicks. The beat never thins the ting either
    /// ([`SHIFT_MIN_GAP`]): a Shift inside one [`MIN_GAP`] of the letter
    /// before the capital still tings; only a second Shift inside
    /// [`SHIFT_MIN_GAP`] of an admitted one is thinned, and a third past the
    /// gate tings again. Its pitch is `the_ting_rotates_and_never_drones`.
    #[test]
    fn shift_is_the_ting_and_never_claims_the_beat() {
        let live = |s: &TrailSynth| s.voices.iter().filter(|v| v.on).count();
        // Shift, then a letter with no render between: the letter speaks.
        let mut s = TrailSynth::new(48_000.0, 0xC0FF_EE01);
        s.push(ev(GlowStyle::Lumen, SoundKind::Shift));
        let before = live(&s);
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        assert!(
            live(&s) > before,
            "the capital right behind a shift must not be thinned by its own grace note"
        );
        // A letter, then the Shift 30 ms later (inside MIN_GAP): it tings.
        let mut s = TrailSynth::new(48_000.0, 0xC0FF_EE02);
        let mut buf = [0.0f32; 2_880]; // 30 ms
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
        // …but a re-press inside the ting's own gate is thinned…
        s.render(&mut buf);
        let before = live(&s);
        s.push(ev(GlowStyle::Lumen, SoundKind::Shift));
        assert_eq!(
            live(&s),
            before,
            "a second Shift 30 ms after an admitted one is thinned (SHIFT_MIN_GAP)"
        );
        // …and past it, the ting is back.
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

    /// A thinned space head re-opens the run: a letter, a Space 30 ms later
    /// (inside [`MIN_GAP`], thinned), then a Space 80 ms after that. The second
    /// space is the run's head, a bass root, not an indent step off a root
    /// nobody heard.
    #[test]
    fn a_thinned_space_head_re_opens_the_run() {
        let mut buf = [0.0f32; 2_880]; // 30 ms
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
        // The control: a letter, 110 ms, a space — a plain head, and the set
        // of voices a head spawns (the root and whatever air it wears).
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

    /// The ting rotates and never drones: repeated bare Shifts (no melody
    /// step, fixed column, no song key) walk [`SHIFT_ROTATION`] instead of
    /// repeating one pitch. Each ting is struck one lattice step above the
    /// melody plus its rotation degree, octave-folded into
    /// [`SHIFT_TING_LO_HZ`]'s band; a thinned ting spends no note.
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
            // Bypass the ting's own gate ([`SHIFT_MIN_GAP`]): the rotation is
            // under test, not the re-press gate.
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
        // Press shift five times and hear five notes: nothing comes back
        // inside a lap, over every lap.
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
        // The governor owns the rotation's clock: a thinned ting spends no
        // note.
        let mut t = TrailSynth::new(48_000.0, 0x5111_F701);
        t.since_voice = 1.0;
        t.push(ev(GlowStyle::Lumen, SoundKind::Shift));
        let after_one = t.shift_step;
        assert_eq!(after_one, 1, "fixture: the first lift sounded");
        // A keystroke claims the beat, so a ting right behind it (no render
        // between, inside MIN_GAP) is thinned and must not advance the
        // rotation.
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

    /// A shifted glyph sings higher and louder: the same keystroke from the
    /// same melody state, shifted and not, lands a clean octave apart
    /// ([`SHIFT_GLYPH_LIFT`]) in every pitched palette without moving the
    /// theme; a shifted Space is still a word boundary at the same root; and a
    /// capital's rendered peak is over its plain self in every voice
    /// (owner, 2026-09-16).
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
            // Every voice either follows the note (×2) or is a fixed-pitch
            // element (×1, e.g. Fire's ember bed); at least one must follow,
            // and none may go down.
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
        // A shifted space is still a word boundary — the lift is Typed-only.
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

    /// The ting decays out instead of being cut: its life is
    /// [`RING_OUT_PER_TAU`] × τ plus the ramp ([`SHIFT_TING_DUR_S`]). On three
    /// v1 voices the rendered −40 dB point comes at least 10 ms before the
    /// release ramp, and the −60 dB point sits inside the ramp.
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

    /// A deletion is a poof in every voice and look, at either scale (a
    /// Backspace or a word kill): two noise-only voices (a delay-0 body and a
    /// delayed air cap) and nothing else, so no pitched palette voice speaks
    /// under it, and it does not step the song. The body's band is static (a
    /// swept contact reads as a falling whistle), the Q broad and the attack
    /// soft enough not to click.
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

    /// A word motion is a character motion at scale: the run's notes step by
    /// the interval a Glide travels, and its last note lands
    /// [`GESTURE_WORD_CHARS`] steps out.
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
            // A cursor gesture does not step the phrase, so both probes speak
            // from the melody's current degree (pan 0 ⇒ no column nudge).
            assert_eq!(gs.walk, ss.walk, "fixture: one degree for both probes");
            let step = i32::from(dir) * GESTURE_CHAR_STEP;
            // The character motion lands one step out…
            assert!(
                (glide[0].0 - gs.melody_hz(anchor, gs.walk + step)).abs() < 0.5,
                "dir {dir}: a character motion must land one step out"
            );
            // …and the word motion walks that same step, once per character.
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
            // And it is audibly bigger than the character motion it scales.
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

    // -- cursor-movement gesture proofs -------------------------------------

    /// The smoke puff ([`SoundKind::Poof`]) is heard and never a nuisance: it
    /// bypasses the min-gap (the Backspace that licensed it is drained one
    /// slot ahead in the same frame, so a gap-thinned puff would be thinned
    /// every time), it does not claim the beat, and it is not a note of the
    /// tune.
    #[test]
    fn the_clouds_puff_survives_the_bell_it_follows() {
        let mut s = TrailSynth::new(48_000.0, 1);
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Backspace)); // since_voice ← 0
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

    /// A held delete run earns one shimmer, on its last release: every
    /// admitted poof re-books the pending glitter and the previous booking
    /// expires unheard. Short runs earn none, and typing before it sounds
    /// cancels it.
    #[test]
    fn a_held_delete_run_shimmers_once_on_release() {
        let pending = |s: &TrailSynth| {
            s.voices
                .iter()
                .filter(|v| v.on && v.shimmer && v.t < 0.0 && v.damp <= 0.0)
                .count()
        };
        let mut s = TrailSynth::new(48_000.0, 0x51D3_0001);
        let mut buf = vec![0.0f32; 4_800 * CHANNELS]; // 100 ms per render
        // A held run: 8 admitted deletions 100 ms apart (inside the
        // HELD_ERASE_RUN_WINDOW, past the ERASE_MIN_GAP).
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
        // The finger lifts: 100 ms later the booking is still pending…
        assert_eq!(pending(&s), 1, "the last booking survives the release");
        // …and 100 ms after that it is sounding, undamped: the run's one
        // audible glitter.
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

        // Short runs earn nothing: three quick corrections book no glitter.
        let mut s = TrailSynth::new(48_000.0, 0x51D3_0002);
        for _ in 0..(HELD_ERASE_RUN_MIN - 1) {
            let mut e = ev(GlowStyle::RainbowKitty, SoundKind::Backspace);
            e.bed = false;
            s.push(e);
            s.render(&mut buf);
        }
        assert_eq!(pending(&s), 0, "a short run is not a held run");

        // Typing cancels the pending reward: hold, then type before it sounds.
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
        // A deliberate single correction much later books nothing new.
        s.render(&mut buf);
        s.render(&mut buf);
        let mut e = ev(GlowStyle::RainbowKitty, SoundKind::Backspace);
        e.bed = false;
        s.push(e);
        assert_eq!(pending(&s), 0, "a lone correction is not a held run");
    }

    /// Cursor motions compose nothing, and only the word-scale one bypasses
    /// the gap: a Glide or a Sweep leaves the phrase melody where it was; a
    /// Glide right after a keystroke is thinned like one, while a Sweep inside
    /// the min-gap still speaks. Their pitches are
    /// `a_word_motion_is_a_character_motion_at_scale`.
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
            a.push(ev(GlowStyle::Lumen, SoundKind::Typed)); // since_voice ← 0
            let before = a.live_voices();
            a.push(ev(GlowStyle::Lumen, kind));
            assert_eq!(
                a.live_voices() > before,
                bypasses,
                "{kind:?} inside the min-gap: only the sweep bypasses thinning"
            );
        }
    }

    /// The [`sin01`] table against real `sinf`. The bound is one 16-bit step:
    /// beyond it the table could move an output sample.
    #[test]
    fn sin_table_stays_below_one_16bit_step() {
        const ONE_16BIT_STEP: f32 = 1.0 / 32768.0;
        let mut worst = 0.0f32;
        let mut worst_ph = 0.0f32;
        // Dense sweep, plus the wrap cases the FM carrier reaches (`sin01` is
        // handed arguments outside [0,1) there, so the wrap must hold too).
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
