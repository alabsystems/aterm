// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trail SOUND — the aural half of the cursor effects: a pure, clockless,
//! allocation-free procedural synthesizer that gives every [`GlowStyle`] its
//! own signature sound palette, plus a namespaced gesture vocabulary other
//! effects can speak through the same engine.
//!
//! Design contract (mirrors the rest of this crate):
//! - **Pure + host-agnostic.** No device I/O, no clock reads, no allocation
//!   after construction. The host pushes [`SoundEvent`]s (one per real cursor
//!   move, the same edge that spawns trail sparks — or one per sparkle-words
//!   gesture) and pulls interleaved stereo `f32` frames via
//!   [`TrailSynth::render`]. Time advances only by samples rendered, so the
//!   whole engine is deterministic given a seed and an event script — and
//!   therefore unit-testable without an audio device.
//! - **Quiet by construction.** Terminal sounds live UNDER the user's
//!   attention, not on top of it. Every palette is tuned toward soft attacks,
//!   musical intervals (a just-intonation major pentatonic, so overlapping
//!   notes can never beat harshly), and short decays. Two dedicated
//!   anti-annoyance systems run on top:
//!   1. a RATE GOVERNOR — per-event gain ducks as typing speed rises and a
//!      per-style minimum gap thins discrete voices, so a 20 cps burst reads
//!      as a gentle texture instead of a hail of pings;
//!   2. a per-style BED — a continuous, very quiet texture (water's stream,
//!      fire's crackle, beam's photon hum…) fed by an energy accumulator that
//!      each event tops up. Fast typing melts INTO the bed; silence lets it
//!      breathe out over ~a second. The bed is the sound analog of the
//!      trail's afterglow.
//! - **Silence is exact.** When every voice is dead and the bed has decayed,
//!   [`TrailSynth::is_quiet`] returns `true` and [`render`](TrailSynth::render)
//!   writes literal zeros — the host can pause its output queue and run zero
//!   audio work between bursts, the same discipline as the glow engine's
//!   `is_active`/deadline wakeups.
//!
//! # The cursor-audio FRAMEWORK
//!
//! Three seams make this an engine rather than one effect's speaker:
//! - **Namespaced gestures.** [`SoundGesture`] pairs a source effect with its
//!   own gesture vocabulary: `Trail(SoundKind)` is the founding cursor-trail
//!   set, `Words(WordGesture)` is sparkle-words'. A new effect adds a variant
//!   and its gesture enum — no existing arm changes meaning, and the shared
//!   plumbing (governor, beds, voice pool, host queue) is inherited whole.
//! - **Per-palette units.** Each [`GlowStyle`]'s voice design, bed grains and
//!   bed texture live on one [`Palette`] implementor, registered in a single
//!   [`palette_for`] match. A trait (not a data table) is deliberate: the
//!   palettes are PROCEDURAL — hue-driven degrees, rng-scattered chimes,
//!   arpeggio scheduling — and flattening them into prototype tables would
//!   mean inventing an interpreter for exactly that logic and re-encoding
//!   nine proven sounds through it (byte-drift risk for zero new power). A
//!   future data-driven Trail-Pack palette is still one `Palette` impl away —
//!   `Voice`/`Partial` prototypes are plain-old-data `Copy` structs, so a
//!   table-driven implementor can spawn them without touching this seam. The
//!   nine shipped palettes are pinned to their pre-framework (v0.56) rendering
//!   by the `v056_reference` proofs below — as of the transcendental rewrite,
//!   to within `V056_TOLERANCE` rather than bit-for-bit, and with ONE stated
//!   design change mirrored into the oracle rather than pinned away: the
//!   deletion's erase POOF (2026-08-26/28 owner ask — see the `POOF_*`
//!   constants). See the tolerance constant:
//!   the per-sample envelopes and oscillators no longer call `exp`/`sin` per
//!   sample, so the last bits move, and the proofs assert an audibly-inaudible
//!   bound (measured peak 0.38 of a 16-bit quantization step) instead of
//!   equality. Every claim of "bit-identical" elsewhere in this file that is
//!   about a PRIOR BUILD of the audio is to be read against that bound; the
//!   claims about run-to-run DETERMINISM (same seed and script ⇒ same output)
//!   remain exact, because nothing here became nondeterministic.
//! - **Kind-level gestures.** Gestures whose sound is style-agnostic (the
//!   Kill swoosh, the curse-word Bonk, the cursor MOTIONS) are designed ONCE
//!   before palette dispatch, tinted at most by a per-palette register
//!   ([`Palette::anchor_hz`]).
//! - **One gesture family.** Typing, deletion and the cursor motions are not
//!   four sound designs: they are one voice moved along two axes — DIRECTION
//!   (forward vs undoing, which signs both the pitch offset and the contour
//!   bend) and SCALE (a character vs a word). The rule is stated and encoded
//!   once, in [`gesture_shape`] + [`gesture_bend`]; palettes state timbre, not
//!   intervals.
//!
//! The host owns policy: focus gating, reduced-motion gating (an intensity of
//! zero simply means "push no events"), the user's `trail_sounds` toggle and
//! `trail_sound_volume`, per-source enables (the profanity `bonk` knob), and
//! the platform output queue.

use crate::cursor_glow::GlowStyle;
use crate::tone::Tone;

/// Interleaved output channel count. The synth is stereo: droplets, chimes
/// and blips pan gently with the cursor's column, which is a large part of
/// why the result reads as "alive" rather than "a keyboard beeper".
pub const CHANNELS: usize = 2;

/// Fixed polyphony. 28 voices is comfortably more than the governor will
/// ever admit (min-gap thinning caps sustained admission around ~20/s and
/// typical decays are < 300 ms); when it IS exhausted the quietest voice is
/// stolen, never the newest.
const MAX_VOICES: usize = 28;

/// What the cursor actually did — the cursor-trail gesture vocabulary, the
/// same classification the glow engine's `spawn` derives from its hints
/// (typed glyph / backspace / navigation / kill) plus the adjacent-vs-jump
/// split (`dist <= 1.0`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SoundKind {
    /// A glyph was typed and the cursor stepped one cell — the bread-and-
    /// butter event; each style's signature "key" sound.
    Typed,
    /// Backspace / deletion — THE POOF: a soft cloud of air, unpitched,
    /// sitting ABOVE the typing register (owner, 2026-08-26/28: "there used to
    /// be a cloud poof on delete", "backspaces are poofy higher notes").
    ///
    /// Style-agnostic and designed once before palette dispatch
    /// ([`TrailSynth::design_erase_poof`]) — and it RETURNS there, so no
    /// palette adds a pitched voice under it. Two band-passed noise bursts,
    /// no tonal partial: see the `POOF_*` constants for the design brief and
    /// for why the previous "the keystroke, a lattice step down" reading is
    /// what the owner heard as "the normal key press".
    ///
    /// Does NOT step the song (undoing a character must not advance the tune
    /// past it) and rides its OWN admission gate ([`ERASE_MIN_GAP`]) rather
    /// than the keystroke governor's, so the first correction after a letter
    /// always speaks and a held Backspace still cannot machine-gun.
    Backspace,
    /// Cursor navigation (arrows, clicks) — the Typed gesture at a whisper:
    /// same timbre family, much quieter and shorter.
    Navigation,
    /// A LINE-scale kill (^K/^U) that moved the cursor — a soft downward
    /// swoosh: a whole clause leaving at once.
    Kill,
    /// A WORD-scale kill (^W, Alt-D, Alt/Ctrl-Backspace) — the erase poof's
    /// SLIGHTLY LARGER, SOFTER COUSIN rather than the line kill's tier-3
    /// swoosh (owner, 2026-08-29: a word-kill is one word going, not a
    /// clause). Same family as [`SoundKind::Backspace`]: two band-passed
    /// noise bursts, no tonal partial, terminal before palette dispatch —
    /// the body a shade LOWER and LONGER than a single character's (a word
    /// is bigger), the air settling further (see the `WORD_POOF_*`
    /// constants). Like every deletion it does NOT step the song.
    KillWord,
    /// THE CLOUD'S LITTLE NOISE — the sound of the erase POOF a plain Backspace
    /// puffs (`cursor_glow`'s `PoofVoice::Puff`, cued on the very edge that
    /// spawns the smoke, so what you hear and what you see are one event and
    /// share one rate limit).
    ///
    /// Deliberately the SMALLEST thing in the vocabulary: a ~110 ms breath of
    /// air-band noise, no partials, no pitch — a puff, not a thud, and far
    /// under the keystroke floor ([`POOF_KIND_GAIN`]) because a Backspace has
    /// ALREADY spoken its erase POOF and this is that poof's own dispersal
    /// riding UNDER it (same `POOF_*` band — see the `POOF_CLOUD_*` brief).
    /// The kill chord's [`SoundKind::Kill`] swoosh is the same idea at clause
    /// scale; this is the one-character version, and the reason a Backspace no
    /// longer fires the full swoosh on top of its own erase voice.
    ///
    /// Three admission properties, all of them consequences of "it accompanies a
    /// visible puff rather than composing the tune":
    /// - it BYPASSES the min-gap governor, exactly as [`SoundKind::Land`] does
    ///   and for the same reason — the cloud is on glass whether or not the gap
    ///   would have thinned the note, and the erase poof that precedes it in
    ///   the very same drain would otherwise eat it every single time;
    /// - it does NOT claim the beat (the [`SoundKind::Shift`] rule): an
    ///   accompaniment that owned the gap would thin the NEXT keystroke;
    /// - it does NOT step the phrase melody — a puff of air is not a note.
    ///
    /// Rate-limiting is upstream and visual: `cursor_glow`'s `POOF_MIN_GAP`
    /// governs the light, and the cue rides the light, so a held Backspace can
    /// never machine-gun this however it bypasses the audio gap.
    Poof,
    /// A multi-cell jump (Enter to a prompt, tab-complete, screen redraws) —
    /// the style's flourish: an arpeggio, a splash, a whoosh, a power chord.
    /// Rapid line feeds are rapid Jumps, and because Jumps bypass the
    /// min-gap thinning their flourishes overlap into the owner's beloved
    /// "brrrring!" — pinned byte-exact by
    /// `brrrring_of_rapid_line_feeds_is_pinned`.
    Jump,
    /// A SMALL cursor move — a single-cell arrow: the family's CHARACTER-scale
    /// motion. One soft, IN-KEY melody tone one lattice step in the travel
    /// direction ([`gesture_shape`]), so scrubbing through text CONTINUES the
    /// tune rather than interrupting it. `dir` is +1 for a rightward / upward
    /// move, -1 for left / down (the direction rides IN the kind, so
    /// [`SoundEvent`] gains no new scalar and its non-finite filter is
    /// untouched). Gap-thinned exactly like a keystroke (out of the always-
    /// admit set), and softer than one (kind-gain in the Navigation whisper
    /// band).
    Glide { dir: i8 },
    /// A FAST cursor run — a held arrow's coalesced echo or a multi-cell leap
    /// (Ctrl-A/E, WORD MOTION): the family's WORD-scale motion, and exactly
    /// that — a [`Glide`] repeated once per character crossed, landing
    /// [`GESTURE_WORD_CHARS`] steps out ([`gesture_shape`]). Same voice, same
    /// interval per step, more of them. One event carries the whole run
    /// (delayed voices, no scheduler — the arpeggio idiom), so it BYPASSES
    /// min-gap like [`Jump`] (thinning it would silence the run mid-flight);
    /// its own inter-note delays rate-limit it. The first note has
    /// `delay = 0`, so it speaks in the first post-cue synth buffer.
    Sweep { dir: i8 },
    /// The cursor LANDED — the aural twin of the rainbow kitty fast-jump STARBURST
    /// (`cursor_glow`'s `Starburst`, cued at the same edge under the same
    /// `RAINBOW_BURST_MIN_DIST` gate, so stars and chime can never diverge). A
    /// bright IN-KEY star chime over a soft arrival body: the biggest thing
    /// the trail vocabulary says, because it accompanies the biggest thing the
    /// trail DRAWS.
    ///
    /// Style-agnostic and designed once before palette dispatch (like
    /// Kill/Bonk), pitched through `melody_hz` so it sits in the melody's key
    /// under every [`crate::tone::Tone`]. BYPASSES the min-gap governor like
    /// [`SoundKind::Jump`] (a starburst you can SEE must never be thinned into
    /// silence) and does NOT step the phrase melody — it punctuates the tune,
    /// it does not compose it.
    Land,
    /// The SPACEBAR — the DOWNBEAT of the typing music (owner ask,
    /// 2026-08-26: "I'd like a more musical space bar (maybe spacebar can be
    /// lower notes)"). The host cues it where it cues the typing click, when
    /// the pressed key was the bare Space; a space inside a committed IME run
    /// stays [`Typed`] (the run is one gesture, not a word boundary).
    ///
    /// Style-agnostic and designed once before palette dispatch
    /// ([`TrailSynth::design_space`]): a round bass root on the speaking
    /// palette's own tonic, octave-folded into ONE bass register and FIXED
    /// there — independent of the melody's current degree (see
    /// [`SPACE_BASS_LO_HZ`] for why the old nearest-tonic rule made the
    /// downbeat jump an octave between words). MONOPHONIC: a new word boundary
    /// retriggers the bass rather than stacking on it. Only the FIRST space of
    /// a whitespace RUN is a downbeat — indentation is one gesture, not four
    /// bass notes — and the rest answer with air alone.
    ///
    /// Does NOT step the song (a bar line is not a note of the tune) and does
    /// not cadence it either — that stays owned by Enter and real pauses.
    /// Gap-thinned exactly like a keystroke.
    Space,
    /// A bare SHIFT keydown — the LIFT before a capital: the anticipation of
    /// the letter that has not landed yet, cued by the host once per physical
    /// press (never on auto-repeat, never on release, never mid-chord).
    ///
    /// The family's fourth member ([`gesture_shape`]): where a deletion is
    /// one step BELOW entered from above, the lift is one step ABOVE entered
    /// from below — a pickup note leaning toward where the capital will land.
    /// Quieter than every per-character gesture (a modifier is INTENT, not
    /// authorship), it does not step the melody, and — alone in the trail
    /// vocabulary — its admission does not claim the min-gap beat
    /// ([`TrailSynth::push`]): a grace note must never thin the keystroke it
    /// announces.
    Shift,
}

/// The sparkle-words gesture vocabulary (word-decoration events).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WordGesture {
    /// The curse-word BONK — deliberately the ONE discordant voice in the
    /// whole engine. Every other gesture is constrained to a mutually
    /// consonant pentatonic lattice, so a minor-second + tritone clash
    /// against the melody's current degree is instantly legible as "wrong"
    /// (that is the joke). Admission bypasses the governor gap like
    /// [`SoundKind::Jump`], the melody+bed dip around it (the master duck
    /// envelope), and it feeds the bed nothing — punctuation, not weather.
    Bonk,
}

/// The SING-ALONG celebration gesture vocabulary (the held-key sing-along —
/// `crate::kitty_sing`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CelebrationGesture {
    /// One BAR of the sing-along riff. The host pushes exactly one per
    /// visual bar boundary while the celebration is ARMED (wind-down pushes
    /// none — the sing-duck's own release is the audio crossfade, and the
    /// last bar's tails decay naturally). `bar` is the bar index since the
    /// arm; the riff alternates two authored phrases on its parity, so the
    /// loop is fully determined by the event data — no scheduler state.
    ///
    /// The riff is scheduled a WHOLE BAR at a time as pre-delayed voices
    /// (the engine's "arpeggio notes are just delayed voices" idiom), so its
    /// internal timing is SAMPLES-based and deterministic; the visual dance
    /// clock shares the same tempo anchored at the same arm instant
    /// ([`CELEBRATION_BAR_SECONDS`] is pinned equal to
    /// `kitty_sing::SING_BAR_SECONDS`), with the documented ± ~60 ms host
    /// buffer/scheduling tolerance (`kitty_sing` module doc).
    /// `bar` is `u16`, not `u8`, for the ESCALATION: a `u8` wraps every
    /// 256 bars (409.6 s of held key) and would restart the build ramp. `u16`
    /// wraps at 65 536 bars (~29 h) and, because `CELEBRATION_PHRASE_BARS` is a
    /// power of two, the FORM survives even that wrap in phase (pinned by
    /// `celebration_form_is_wrap_safe`).
    ///
    /// `sig` is the held character's BIJECTIVE song signature
    /// (`kitty_sing::KittySing::signature`). Every per-key axis of the
    /// celebration is a pure function of it — the verse melody walk, the
    /// root transpose, the mode rotation (`design_celebration`) — because
    /// the seamless hand-over law forbids synth-side per-key state: a
    /// mid-hold key change simply changes the payload the NEXT bar carries,
    /// over the same uninterrupted bar grid.
    RiffBar { bar: u16, sig: u32 },
}

impl CelebrationGesture {
    /// Build the canonical riff-bar gesture: bar index + the held
    /// character's song signature. Hosts (and the audition benches)
    /// construct through THIS — the stable constructor — not the variant
    /// literal.
    #[must_use]
    pub fn riff_bar(bar: u16, sig: u32) -> Self {
        Self::RiffBar { bar, sig }
    }
}

/// One namespaced gesture: WHICH effect spoke, and WHAT it said. The
/// source-effect axis is the enum variant; the gesture axis is the payload.
/// Shared shaping (governor, admission, bed feed) dispatches on the pair, so
/// vocabularies can never collide across effects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SoundGesture {
    /// Cursor-trail gestures — the founding vocabulary; byte-identical in
    /// every way to the pre-framework `SoundKind` events.
    Trail(SoundKind),
    /// Sparkle-words gestures.
    Words(WordGesture),
    /// SING-ALONG sing-along gestures (`crate::kitty_sing`'s celebration).
    Celebration(CelebrationGesture),
}

/// WHICH palette family speaks for an event — the host's `trail_sound_style`
/// setting riding the per-event seam like `gain`: policy is carried per
/// event, so a settings change takes effect on the next keystroke with zero
/// channel changes. [`SoundVoice::Style`] is the exact identity: the palette
/// follows [`SoundEvent::style`], bit for bit (byte-pinned by the
/// `v056_reference` proofs). Every other voice overrides the style binding
/// WHOLESALE for the palette-designed gestures (Typed / Backspace /
/// Navigation / Jump, the Kill swoosh's tint, and the ambient bed) regardless
/// of the visual trail style; the style-agnostic kind-level gestures
/// (Glide/Sweep melody tones, the bonk's clash shape) keep their shared design
/// and merely borrow the speaking palette's register.
///
/// THE TYPING-SOUND PICKER (owner ask, on hearing the glass bell: "is there a
/// way to add different typing sounds in the settings? let's do that too").
/// The roster below IS the Settings ▸ Sound ▸ "Typing sound" picker, in
/// picker order; [`SoundVoice::name`] spells each value ONCE (the host's
/// option list is derived from it, never re-typed) and [`SoundVoice::parse`]
/// accepts those spellings plus the documented [`SoundVoice::ALIASES`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SoundVoice {
    /// Follow the visual trail style's palette — today's sound, bit for bit.
    #[default]
    Style,
    /// One of the nine style palettes as a STANDALONE instrument — the SAME
    /// palette struct the trail would pick for that look, chosen regardless of
    /// the look actually on screen. An ALIAS of the `Style` table, never a
    /// fork: `palette_for(Of(s), _) == palette_for(Style, s)`, so the nine
    /// shipped palettes stay byte-pinned through it. `Of(Custom)` is never
    /// produced ([`SoundVoice::parse`] cannot yield it, it is not in
    /// [`SoundVoice::ALL`]); were it built by hand it would ride Lumen exactly
    /// as `Style` under a `pack:` look does.
    Of(GlowStyle),
    /// The mechanical-keyboard palette: click + thock percussion.
    Mech,
    /// The TYPEWRITER: a dry slug clack over the platen thud; on Enter the
    /// margin bell dings and the carriage zips home. Machinery, not music.
    Typewriter,
    /// The MARIMBA: a warm rosewood bar under a yarn mallet, its two-octave
    /// glint dying first. The roster's low-mid soft-transient voice.
    Marimba,
    /// The FELT piano: a felt-muted hammer thud under a dark harmonic tone —
    /// the hush you can type on all day. The roster's lowest, darkest voice.
    Felt,
}

impl SoundVoice {
    /// THE ROSTER, in picker order: `auto` first (the default), then the nine
    /// style palettes by what they SOUND like, then the keyboard, then the
    /// three sound-only instruments. `Of(GlowStyle::Custom)` is deliberately
    /// absent — a Trail Pack is a look, not a sound.
    pub const ALL: &[SoundVoice] = &[
        SoundVoice::Style,
        SoundVoice::Of(GlowStyle::RainbowKitty),
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

    /// The documented ALIAS spellings — accepted by [`SoundVoice::parse`]
    /// (config files, `settings set`), never offered by the picker. The
    /// trail-style names are here so `trail_sound_style = "water"` reads
    /// naturally, and every pre-picker `mechanical` spelling still lands.
    pub const ALIASES: &[(&str, SoundVoice)] = &[
        ("style", SoundVoice::Style),
        ("follow", SoundVoice::Style),
        ("default", SoundVoice::Style),
        ("bell", SoundVoice::Of(GlowStyle::RainbowKitty)),
        ("bells", SoundVoice::Of(GlowStyle::RainbowKitty)),
        ("kitty", SoundVoice::Of(GlowStyle::RainbowKitty)),
        ("rainbow kitty", SoundVoice::Of(GlowStyle::RainbowKitty)),
        ("rainbow kitty pet", SoundVoice::Of(GlowStyle::RainbowKitty)),
        ("rainbow dog pet", SoundVoice::Of(GlowStyle::RainbowKitty)),
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

    /// THE CANONICAL SPELLING of each voice — the config value, the picker
    /// row, the introspection token. Spelled here ONCE (`const fn`, so the
    /// host's option list is literally built from these). Lowercase with
    /// spaces, the `cursor_trail_style` convention (`"rainbow kitty"`).
    /// `Of(Custom)` answers as Lumen because that is the palette it rides.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            SoundVoice::Style => "auto",
            SoundVoice::Of(GlowStyle::RainbowKitty) => "glass bell",
            SoundVoice::Of(GlowStyle::Lumen | GlowStyle::Custom) => "warm pluck",
            SoundVoice::Of(GlowStyle::Sparkle) => "glitter",
            SoundVoice::Of(GlowStyle::Comet) => "ice chime",
            SoundVoice::Of(GlowStyle::Water) => "droplet",
            SoundVoice::Of(GlowStyle::Phaser) => "pew",
            SoundVoice::Of(GlowStyle::Laser) => "zap",
            SoundVoice::Of(GlowStyle::Beam) => "tick",
            SoundVoice::Of(GlowStyle::Fire) => "crackle",
            SoundVoice::Mech => "mechanical",
            SoundVoice::Typewriter => "typewriter",
            SoundVoice::Marimba => "marimba",
            SoundVoice::Felt => "felt",
        }
    }

    /// Resolve a spelling — canonical [`SoundVoice::name`] or documented
    /// [`SoundVoice::ALIASES`] entry, trimmed, ASCII-case-insensitive — to its
    /// voice; `None` for anything else (the host falls back to `Style`, the
    /// validator warns). Never yields `Of(Custom)`.
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

/// One sound trigger. The host builds this at the trail-spawn edge (or the
/// word-decoration drain) with the data it already has in hand there (style,
/// column, typing heat, live hue) and the resolved user gain (motion-gated
/// intensity × `trail_sound_volume` × per-source enables).
#[derive(Clone, Copy, Debug)]
pub struct SoundEvent {
    pub style: GlowStyle,
    /// Which palette family speaks — see [`SoundVoice`]; `Style` is the
    /// pre-override identity.
    pub voice: SoundVoice,
    pub kind: SoundGesture,
    /// Stereo position, -1 (hard left) .. 1 (hard right). The host maps the
    /// cursor column across the pane; the synth additionally narrows it so
    /// nothing ever sits fully in one ear.
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
    /// The host's inferred TONE of what is being typed (the tiny
    /// [`crate::tone`] classifier over the current typed line, throttled and
    /// cached host-side). Rides the per-event seam like `gain`. TRAIL
    /// gestures steer the melody's constitution with it (scale table,
    /// transpose, walk bias, note-length feel — see [`tone_tables`]);
    /// [`Tone::Technical`] is the exact identity (today's sound, bit for
    /// bit), and the Bonk ignores the field entirely (the wrong note is
    /// wrong in every mood).
    pub tone: Tone,
    /// Whether this gesture may feed the ambient BED layer (the continuous
    /// per-style texture). Rides the per-event seam like `gain`, so the
    /// host's `trail_sound_bed` setting (default OFF) takes effect on the
    /// next keystroke. With every event carrying `false` the bed is never
    /// energised, so the bed mixer contributes EXACTLY ZERO samples
    /// structurally (its level-floor early-out, grains included — not a
    /// gain-0 render of the same DSP); the discrete notes, the brrrring, the
    /// bonk and the melody are untouched.
    pub bed: bool,
}

// ---------------------------------------------------------------------------
// Musical foundation
// ---------------------------------------------------------------------------

/// Just-intonation major pentatonic (1 9/8 5/4 3/2 5/3). Every interval in
/// the set is consonant against every other, so ANY pile-up of overlapping
/// voices — the normal state while typing — stays harmonious. Equal
/// temperament would beat audibly on the thirds; just ratios lock.
const PENTA: [f32; 5] = [1.0, 1.125, 1.25, 1.5, 1.666_666_7];

/// `base` scaled to pentatonic `degree` (octaves wrap; negative degrees fall
/// below `base`).
fn penta(base: f32, degree: i32) -> f32 {
    let oct = degree.div_euclid(5);
    let step = degree.rem_euclid(5) as usize;
    base * PENTA[step] * (2.0_f32).powi(oct)
}

/// FRUSTRATED minor pentatonic (1 6/5 4/3 3/2 9/5), just intonation. Same
/// mutual-consonance law as [`PENTA`]: every pairwise interval (octave wraps
/// and inversions included) stays outside the minor-second rub zone and the
/// tritone band, so a frustrated flood is dark but never harsh — proven by
/// `tone_tables_are_mutually_consonant_and_exclude_the_bonk_clash`.
const PENTA_MINOR: [f32; 5] = [1.0, 1.2, 1.333_333_3, 1.5, 1.8];

/// PLAYFUL suspended pentatonic (1 9/8 4/3 3/2 5/3 — the Japanese *yo*
/// scale): major penta with the third lifted to a fourth, open and floaty.
/// The consonant stand-in for "lydian-ish": a true lydian ♯4 IS the bonk's
/// tritone (45/32), so shipping that in a melody table
/// would both break the no-beating invariant and dilute the bonk's
/// exclusive claim to "wrong". Suspension reads as whimsy without either.
const PENTA_YO: [f32; 5] = [1.0, 1.125, 1.333_333_3, 1.5, 1.666_666_7];

/// EXCITED transpose: one just whole tone (9/8) up, applied to every
/// palette's base through [`TrailSynth::melody_hz`]. Internal intervals are
/// untouched (a transposed lattice is the same lattice), so consonance is
/// inherited from [`PENTA`] wholesale.
const EXCITED_TRANSPOSE: f32 = 1.125;

/// Per-tone melodic constitution: (scale table, transpose ratio). The ONE
/// binding from the classifier's coarse mood to musical material:
/// - Technical / Calm — today's major penta, untransposed. Technical is the
///   full identity (walk + feel too); Calm keeps the table but breathes
///   longer and steps more gently.
/// - Excited — major penta a whole tone brighter (+2 semitones just).
/// - Frustrated — minor penta (walk bias adds the downward lean).
/// - Playful — the suspended *yo* penta (walk gets wider steps).
///
/// Every table is mutually consonant per the module's no-beating invariant,
/// and none contains the bonk's minor-second/tritone identities.
fn tone_tables(tone: Tone) -> (&'static [f32; 5], f32) {
    match tone {
        Tone::Technical | Tone::Calm => (&PENTA, 1.0),
        Tone::Excited => (&PENTA, EXCITED_TRANSPOSE),
        Tone::Frustrated => (&PENTA_MINOR, 1.0),
        Tone::Playful => (&PENTA_YO, 1.0),
    }
}

/// Per-tone TEMPO-FEEL: one multiplier on note length, decay, and flourish
/// spacing (`Voice::dur`/`decay`/`delay`, applied at spawn). Deliberately
/// narrow (±12 %): the melody should lean, not lurch. Technical and
/// Frustrated are exactly 1.0 — Technical because it is the pinned identity,
/// Frustrated because its character lives in the minor table + downward
/// bias, not in dragging the room. Bonk voices are exempt (spawn skips
/// `duck_exempt` prototypes), keeping that path byte-untouched.
fn tone_feel(tone: Tone) -> f32 {
    match tone {
        Tone::Technical | Tone::Frustrated => 1.0,
        Tone::Calm => 1.06,
        Tone::Excited => 0.88,
        Tone::Playful => 0.94,
    }
}

// ---------------------------------------------------------------------------
// Phrase-aware melody generator — the mechanism that turns the memoryless
// random walk into a TUNE (repeat-and-vary motif, contour arc, call-and-
// response, cadence). All parameters are deterministic (`rnd()` + phrase
// state); every pitch still flows through `melody_hz`, so the no-beating
// consonance invariant and the tone-adaptive scale are inherited unchanged.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// THE PULSE — what makes the typed line a SONG rather than a stream of notes
// ---------------------------------------------------------------------------
//
// Owner, 2026-08-28: "I want typing to sound like a little fun song."
//
// WHAT WAS WRONG, measured rather than asserted: on a 60 s script of real
// prose at 10 cps the shipped build produced 10.5 PITCHED ONSETS PER SECOND,
// every one of them a fresh melodic event on a freshly-drawn motif. No music
// is ten melody notes a second. At that density the ear cannot follow a line
// at all — attacks mask pitch, 100-300 ms tails overlap continuously, and
// however good the note choices are the result reads as texture. The phrase
// generator below was doing real work (motif, contour arc, call-and-response,
// cadence) at a rate that made all of it inaudible: a 6-8 note phrase was over
// in 0.7 seconds.
//
// THE FIX IS RHYTHM, not different notes. A keystroke is not a note any more —
// it is a POSITION IN A BAR. The bar is authored and fixed:
//
//   * an ACCENT slot SINGS: the phrase generator steps and the keystroke plays
//     its new degree at full level;
//   * a GHOST slot ACCOMPANIES: the melody does NOT move, and the keystroke
//     plays a fixed consonant offset from the melody's current degree, quieter
//     ([`SONG_GHOST_LEVEL`]) and shorter ([`SONG_GHOST_FEEL`]).
//
// So the melody moves at a THIRD of typing speed — ~3.3 notes a second at
// 10 cps, which is a tune you can hear — while every keystroke still speaks,
// which is not negotiable: a key that makes no sound reads as a dropped
// keystroke, and the min-gap governor already spends the ear's tolerance for
// that during bursts. This is dynamics, not rests.
//
// It is ALSO why the phrase generator finally lands. A 7-note phrase now
// unfolds over ~21 keystrokes (~2 s at 10 cps) instead of 0.7 s, and because
// the bar is 12 slots while a phrase is 6-8 notes, the two cycle against each
// other: the same authored accompaniment figure falls on different notes of
// the tune every pass. That phasing is the long-form variation a
// default-ON sound needs to survive ten minutes rather than twenty seconds —
// no table long enough to be "the song" would ever fit here, and one short
// enough to fit would loop audibly.
//
// TYPING SPEED IS THE TEMPO. The bar is indexed by KEYSTROKE, not by clock, so
// the figure stretches and compresses with the hands playing it. That is the
// one thing a terminal's music can do that a player cannot.
/// The sentinel for an ACCENT slot. `i8::MIN` cannot collide with a real
/// ghost offset (they live in ±4 degrees).
const SONG_ACCENT: i8 = i8::MIN;

/// THE BAR. [`SONG_ACCENT`] where the melody sings; elsewhere the GHOST's
/// offset in scale degrees from the melody's current note.
///
/// Four accents in twelve — a steady beat every third keystroke, which is what
/// makes the pattern legible as a pulse rather than as level drift.
///
/// The ghosts spell a BROKEN CHORD under each melody note: the octave below
/// (−5) and then a colour tone, alternating a pentatonic fourth below (−3)
/// with a third above (+2) so the figure has a six-slot period against the
/// bar's twelve. Every offset is an integer scale DEGREE, so the module's
/// no-beating law is inherited whole — a ghost cannot rub against the note it
/// accompanies whatever the melody is doing or which [`crate::tone::Tone`] is
/// speaking.
///
/// The octave is doing real work and is not interchangeable with a unison. A
/// ghost on the melody's own degree would be a SECOND VOICE AT THE SAME
/// FREQUENCY as the accent still ringing above it, with independently
/// randomised oscillator phase — a comb filter, exactly the artifact
/// [`SPACE_DAMP_S`] exists to prevent for the downbeat. An octave is the most
/// consonant interval there is and cannot cancel.
///
/// It also completes the arrangement's REGISTERS. Under the glass bell: the
/// space downbeat at ~130 Hz, the ghost arpeggio at ~260-480, the melody at
/// ~520-950, the erase poof at 2.8-5 kHz. Four bands, one per role, nothing
/// fighting anything.
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

/// A GHOST's level against the accent it accompanies (−5.2 dB). Wide enough
/// that the accents read as THE TUNE and the ghosts as accompaniment under
/// it; narrow enough that a ghost never reads as a key that failed to fire.
const SONG_GHOST_LEVEL: f32 = 0.55;
/// A GHOST's length against an accent's — the same `dur`/`decay`/`delay`
/// multiplier the per-tone feel rides, so it needs no new machinery in the
/// voice.
///
/// This is the MASKING fix, and it is worth more than the level cut. At 10 cps
/// the keystrokes are 100 ms apart and the glass bell's note is ~135 ms, so
/// every note used to overlap its neighbour; a ghost at 0.62 is ~84 ms and
/// clears the next keystroke entirely. Two thirds of the notes stop piling up,
/// which is what lets the accents' pitch actually be heard.
const SONG_GHOST_FEEL: f32 = 0.62;

/// Phrase length in notes — a phrase runs 6..=8 notes, chosen per phrase, then
/// cadences. Short enough that the motif cell (4 notes) recurs and varies
/// audibly within one breath; long enough not to feel like stuttering.
const PHRASE_MIN: u8 = 6;
const PHRASE_MAX: u8 = 8;

/// A typing gap (seconds) longer than this ENDS the current phrase — the
/// natural comma the ear already hears. Enter (a [`SoundKind::Jump`]) ends one
/// too; both trigger the cadence onto the tonic. Sized to the same ~0.6 s the
/// governor's rate estimate decays over.
const PHRASE_PAUSE_S: f32 = 0.6;

/// Height of the phrase's CONTOUR ARC, in scale degrees: every phrase LIFTS
/// through its middle and settles back for the cadence, so the line has a
/// SHAPE instead of a flat random drift. Added to the pitch register, never to
/// the motif accumulator, so it colours the contour without compounding.
///
/// TWO degrees, and the arc moves in ONE jump of that size rather than
/// climbing a degree at a time. That is not a taste call — it is what stops
/// the two pitch axes from cancelling. The motif walks in ±1 steps; when the
/// arc also stepped by ±1 (`round(2·sin πf)`, the old law) the two annihilated
/// exactly, and a note that should have moved played the same pitch again.
/// Measured over a 12.5 s typing script that was 54 % of all consecutive
/// notes: the "melody" was mostly a drone. A lift of a pentatonic THIRD is
/// orthogonal to a step of a second — no motif delta can cancel it — and
/// unisons fall to ~8 % with the contour left intact (mid-phrase notes still
/// average +2.9 degrees over the phrase's ends).
const ARC_LIFT: i32 = 2;

/// The REPEAT-AND-VARY lift: the motif cell's SECOND pass sits a scale-degree
/// higher. It rides the pitch register beside the arc — never the delta — so
/// the cell's step pattern stays exactly periodic (the motif genuinely
/// recurs, which is what a lag-4 interval autocorrelation reads) and the lift
/// can never zero out a step the way `delta += 1` could.
const MOTIF_VARY: i32 = 1;

/// The bright LEAP at the arc peak, in scale degrees — the classic
/// "leap-and-recover" that makes a line sing; the motif steps back on the
/// following note. Because degrees stay on the active table, the leap is a
/// consonant sixth/octave, never a rub.
const MELODY_LEAP: i32 = 2;

/// Per-tone melodic REGISTER `(lo, hi)` the phrase walk is clamped into —
/// Excited taller, Frustrated shorter. `lo` is 0 for every table so the
/// cadence tonics (degrees 0 and 5) are always reachable.
fn tone_register(tone: Tone) -> (i32, i32) {
    match tone {
        Tone::Technical | Tone::Calm => (0, 7),
        Tone::Excited => (0, 8),
        Tone::Frustrated => (0, 6),
        Tone::Playful => (0, 8),
    }
}

/// Per-tone motif STEP SPAN: the maximum absolute scale-step a motif delta may
/// take. Playful skips and leaps (±2, the whimsy knob); every other mood walks
/// in gentle steps (±1). Technical is exactly ±1 — the re-baselined identity.
fn melody_span(tone: Tone) -> i32 {
    match tone {
        Tone::Playful => 2,
        _ => 1,
    }
}

/// Fold a pitch register into `lo..=hi` by REFLECTION rather than clamping.
///
/// Clamping is a silent unison generator: consecutive notes whose registers
/// both land past the ceiling play the SAME pitch, so a phrase that leans into
/// the top of its register goes flat — the melody stops moving exactly where
/// its contour is strongest. A reflection turns the excess into a step back
/// DOWN the lattice, so every note is still a real move, still on the tone's
/// table, and still inside its register. Modular (a triangle wave), so an
/// arbitrarily far-out input is total and cheap rather than looping.
fn fold_register(v: i32, lo: i32, hi: i32) -> i32 {
    let span = hi - lo;
    if span <= 0 {
        return lo;
    }
    let m = (v - lo).rem_euclid(2 * span);
    lo + if m <= span { m } else { 2 * span - m }
}

/// Per-tone LEAN added to the pitch register: Excited climbs (+1), Frustrated
/// sinks (−1), the rest sit level (0) — a quiet directional bias on top of the
/// arc.
fn melody_lean(tone: Tone) -> i32 {
    match tone {
        Tone::Excited => 1,
        Tone::Frustrated => -1,
        _ => 0,
    }
}

/// The DEFAULT register the STYLE-AGNOSTIC cursor-movement notes (Glide/Sweep)
/// sing in — a warm mid anchor. A palette that names its own melodic base
/// ([`Palette::anchor_hz`]) moves the movement family into ITS register, so
/// scrubbing and typing share an octave; this is the fallback for palettes
/// that don't (and the kind-level Kill/Bonk register). The pitch is drawn
/// through `melody_hz` at the melody's current degree, so the cursor note sits
/// on the active tone's table exactly like the tune.
const CURSOR_ANCHOR_HZ: f32 = 330.0;

/// Spacing between the notes of a WORD-scale run, with the first at
/// `delay = 0` (first-buffer audible) — the run's own rate limit.
const CURSOR_SWEEP_STEP_S: f32 = 0.055;

// ---------------------------------------------------------------------------
// THE GESTURE FAMILY — ONE rule for typing, deletion and the word motions
// ---------------------------------------------------------------------------
//
// WHAT WAS WRONG: the four per-character/per-word gestures were four unrelated
// sound designs. A keystroke sat on the melody degree; every palette pushed
// BACKSPACE down by whatever interval its author happened to like (−2 in
// Lumen/Phaser/Comet, −3 in rainbow kitty, no offset at all in the other six);
// a Glide played one bare sine step at a fixed 330 Hz anchor no matter which
// register the typing was in; and a Sweep was four of that same sine. Nothing
// tied a deletion to the keystroke it undoes, or a word motion to the
// character motion it magnifies — so the edit vocabulary read as a bag of
// noises rather than one voice doing different things.
//
// THE RULE. Every gesture in the family is THE SAME MOVE ON THE TEXT rendered
// as the same move on the lattice, along two axes and nothing else:
//
//   DIRECTION (`dir`)  +1 = forward / creating, −1 = undoing / backward. It
//     signs both the pitch OFFSET and the contour BEND: a forward gesture
//     leans UP onto its note, its inverse leans DOWN onto it. A deletion is a
//     keystroke played backwards, not a different instrument.
//   SCALE (`notes`, `step`)  how much TEXT the gesture crosses. A CHARACTER
//     gesture is one note, [`GESTURE_CHAR_STEP`] lattice degrees of travel. A
//     WORD gesture is that same stride walked out over [`GESTURE_WORD_CHARS`]
//     characters — same voice, same interval per character, more of them — so
//     it LANDS exactly where a character move of word size would land.
//
//                  dir   notes   stride   lands at (from the melody degree)
//     Typed        +1      1       0      the degree itself
//     Shift        +1      1       0      +1 step, entered from BELOW
//     Glide{dir}   dir     1       0      dir × 1 step
//     Sweep{dir}   dir     4     dir×1    dir × 3 steps
//
// (Neither SPACE nor BACKSPACE is a family degree any more, and for the same
// reason: they are not moves on the melody's lattice. The space grounds the
// REGISTER on a fixed bass root (`design_space`); the deletion is UNPITCHED
// air (`design_erase_poof`). Both take the neutral shape below.
//
// Backspace WAS a family member — the keystroke a step down, entered from
// above — and the owner reported the result, twice, as indistinguishable from
// typing. The rule was right for the MOTIONS and wrong for the edit: a glide
// really is a keystroke that moved, but a deletion is a different event.)
//
// Encoded ONCE, in [`gesture_shape`] + [`gesture_bend`], and READ from the
// three places a family degree is built: the shared `deg` in
// [`TrailSynth::design_trail`] (so every palette's deletion mirrors by the
// same interval — the per-palette offsets are gone), [`TrailSynth::design_cursor`]
// (Glide/Sweep), and Phaser's hue-driven degree, which builds its own and so
// must ask for the offset rather than spell one.

/// One CHARACTER of travel, in lattice degrees.
const GESTURE_CHAR_STEP: i32 = 1;

/// What a WORD gesture is worth in characters — the ONE scale factor between
/// a character motion and a word motion. Three lattice steps is a pentatonic
/// sixth: plainly the same move, plainly bigger.
const GESTURE_WORD_CHARS: i32 = 3;

/// A WORD-scale run is one note per character crossed, endpoints included.
const CURSOR_SWEEP_RUN: usize = GESTURE_WORD_CHARS as usize + 1;

/// THE CONTOUR BEND — the family's articulation, one just WHOLE TONE (9/8).
/// A voice enters one lattice step from its direction's side and settles onto
/// its note over [`GESTURE_BEND_TAU`]. Two properties earn the ratio: it is
/// [`PENTA`]'s own second degree, so even mid-scoop the voice is passing
/// through a consonant lattice interval rather than smearing off it, and it is
/// NOT the bonk's minor second (16/15), whose exclusive claim to "wrong" the
/// module protects everywhere else.
///
/// The two directions read as a matched pair: a keystroke arrives from a step
/// BELOW and settles up onto its note (the letter landing); a deletion starts
/// on the note it is removing and slides a step DOWN off it (the letter
/// leaving).
const GESTURE_BEND: f32 = 1.125;
/// Bend time constant — short enough to read as articulation, not portamento
/// (a keystroke is ~135 ms, so the scoop is over inside the first tenth of it).
const GESTURE_BEND_TAU: f32 = 0.012;

/// The family geometry of one gesture: see the section comment above. Kinds
/// outside the family (Jump, Kill, Land, Navigation) take the neutral shape —
/// forward, one note, no offset — so callers never branch on membership.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct GestureShape {
    /// +1 forward / creating, −1 undoing / backward.
    dir: i8,
    /// Notes in the gesture: 1 at character scale, [`CURSOR_SWEEP_RUN`] at
    /// word scale.
    notes: usize,
    /// Lattice degrees between consecutive notes (0 for a single note).
    step: i32,
    /// Lattice degrees from the melody's current degree to the FIRST note.
    offset: i32,
}

/// THE RULE, encoded. Every family degree in the engine comes from here.
fn gesture_shape(kind: SoundKind) -> GestureShape {
    match kind {
        // (A DELETION IS NOT HERE ANY MORE. It used to be `dir: -1, notes: 1,
        // step: 0, offset: -GESTURE_CHAR_STEP` — the keystroke's voice one
        // lattice step down — and that arithmetic is exactly why the owner
        // twice reported deletions as "the normal key press". A deletion is
        // now unpitched (see the ERASE POOF constants) and returns before
        // palette dispatch, so it has no lattice degree to shape and takes the
        // neutral shape below like Jump / Kill / Land.)
        //
        // The lift before a capital: one step above the note it announces,
        // entered from below — the deletion's exact mirror.
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
        // A WORD of travel: the character stride, walked out.
        SoundKind::Sweep { dir } => GestureShape {
            dir,
            notes: CURSOR_SWEEP_RUN,
            step: i32::from(dir) * GESTURE_CHAR_STEP,
            offset: 0,
        },
        // Forward, character-scale, on the degree itself.
        _ => GestureShape {
            dir: 1,
            notes: 1,
            step: 0,
            offset: 0,
        },
    }
}

/// OCTAVE-FOLD a palette's melodic anchor into the SPACE's bass register
/// `[SPACE_BASS_LO_HZ, 2 × SPACE_BASS_LO_HZ)`. Halving and doubling are exact
/// in binary floating point and preserve PITCH CLASS exactly, so the folded
/// root is still the palette's own tonic — consonant with its melody by
/// construction — while every palette's downbeat lands in one register.
/// Total: the clamp bounds the input, so the loops run at most a few times
/// and cannot spin on a zero, an infinity or a NaN-free extreme.
fn bass_octave(anchor: f32) -> f32 {
    let mut f = anchor.clamp(20.0, 20_000.0);
    while f >= SPACE_BASS_LO_HZ * 2.0 {
        f *= 0.5;
    }
    while f < SPACE_BASS_LO_HZ {
        f *= 2.0;
    }
    f
}

/// The BEND applied: `(f0, f1)` for a partial that settles onto `f` from
/// `dir`'s side. Forward enters from a step below, inverse from a step above.
fn gesture_bend(f: f32, dir: i8) -> (f32, f32) {
    if dir >= 0 {
        (f / GESTURE_BEND, f)
    } else {
        (f * GESTURE_BEND, f)
    }
}

/// The bonk's clash intervals, in just intonation like everything else: the
/// minor second (16/15) rubs, the tritone (45/32) refuses to resolve. Neither
/// ratio exists anywhere in [`PENTA`]'s lattice (including octave wraps), so
/// the bonk can never be mistaken for a melody note.
const BONK_MINOR_SECOND: f32 = 16.0 / 15.0;
const BONK_TRITONE: f32 = 45.0 / 32.0;

// ---------------------------------------------------------------------------
// THE LOUDNESS LADDER — one designed hierarchy, not a set of independent knobs
// ---------------------------------------------------------------------------
//
// RULE: loudness is indexed by how OFTEN a gesture can repeat, not by how big it
// LOOKS. Every per-character gesture sits on one FLOOR; each rarer tier is
// ~+1.5 dB over the one below, and the whole span is ~7 dB — the rarest events
// are audibly SLIGHTLY louder, never a jump scare.
//
// The dBFS figures are RENDERED peaks at the default `trail_sound_volume` (0.4),
// heat 0.5, one isolated event, median over the ten palettes. The governor
// translates the WHOLE ladder rigidly (one scalar on `g` for every Trail kind
// and the bonk), so the intervals hold at any typing speed.
//
//   TIER 1  -21.0 dBFS  Typed, Space, Backspace, Glide   per CHARACTER (10+/s)
//   TIER 2  -19.5 dBFS  Sweep, Navigation           per GESTURE
//   TIER 3  -18.0 dBFS  Jump, Kill                  per LINE / COMMAND
//   TIER 4  -16.0 dBFS  Land                        rare spectacle
//   TIER 5  -14.0 dBFS  Bonk, the riff's peak bar   rarest punctuation
//
// (Shift sits UNDER tier 1 with the movement family — a modifier is intent,
// not authorship — and under Glide too: the lift is the quietest voice in the
// engine, pinned relatively by `the_ladder_holds_for_the_key_family`.)
//
// Kind gains are the TIER knob; `palette_trim` anchors each style's Typed on the
// floor. Where a gain looks surprising it is compensating a VOICE design, and
// the comment says so.

/// TIER 1 — the FLOOR. A keystroke is the unit everything else is measured in.
const TYPED_KIND_GAIN: f32 = 1.0;
/// TIER 1, a shade UNDER the floor. A deletion is a CORRECTION: it should not
/// announce itself as loudly as the character it removes, and a burst of them
/// (hold backspace) must not out-shout the typing it is undoing.
///
/// `e8d1a1d9` moved this to 1.0 as part of flattening the ladder; the owner's
/// 2026-08-04 mix pass puts deletions back under typing.
const BACKSPACE_KIND_GAIN: f32 = 0.85;
/// TIER 1, between the floor and the deletion. The DOWNBEAT: a word boundary
/// is authorship (it repeats at typing speed, so it lives on the per-character
/// tier), but it grounds rather than states.
const SPACE_KIND_GAIN: f32 = 0.9;
/// UNDER the movement family's sub-floor. A bare modifier is INTENT — the
/// hand shaping the next letter, not a letter — so the lift sits below even
/// a Glide's whisper (0.78): present in the music, never a note you count.
const SHIFT_KIND_GAIN: f32 = 0.5;

// ---------------------------------------------------------------------------
// THE ERASE POOF — the deletion's OWN voice (owner, 2026-08-26/28)
// ---------------------------------------------------------------------------
//
// WHAT WAS WRONG, in the owner's words: "I hear backspace sounds but they
// aren't poofs anymore they sound like the normal key press", and the fix they
// asked for: "there used to be a cloud poof on delete", "backspaces are poofy
// higher notes".
//
// The cause was structural, not a mix miss. A deletion was the TYPED voice
// transposed — `gesture_shape` gave it `notes: 1, step: 0` a lattice step
// down, so it WAS the keystroke, played lower. The 2026-08-26 "felt lift-off"
// added two quiet noise UNDER-layers (a 260→190 Hz damp and a 1500→430 Hz
// breath), but both FELL and both sat UNDERNEATH the still-dominant pitched
// key voice — the exact opposite of a higher poof. Measured on the shipped
// build: the deletion's spectral centroid was 1089 Hz against the keystroke's
// 1474 Hz. The erase was DARKER than the letter it removed.
//
// THE RULE NOW: a deletion is not a quieter keystroke, it is a DIFFERENT
// EVENT, and it gets a different voice. Two band-passed noise bursts and no
// tonal partial at all — a body that is the puff and an air cap over it —
// designed once at kind level and returning BEFORE palette dispatch, so no
// palette can put a pitched residue back under it. It does not step the song
// either: undoing a character must not advance the tune past it.
//
// A puff is BROAD and SOFT. High Q rings (a whistle); a sub-millisecond attack
// clicks (a tick). The BODY's cutoff is STATIC, which is also the cheap path —
// `spawn` caches the state-variable filter's coefficient and divisor once when
// `n_glide <= 0` — while the AIR CAP settles a bounded step down
// ([`POOF_AIR_SETTLE_HZ`]) and pays the swept band for it (deletions are
// ERASE_MIN_GAP-thinned, so at most ~13 swept voices/s).
/// The poof's BODY: the puff itself, centred where "air" lives — above every
/// palette's melodic register, which is what makes the erase read as HIGHER
/// than the letter rather than under it.
const POOF_BODY_HZ: f32 = 2800.0;
/// Broad, deliberately: `1/Q` is the SVF's damping, and a Q under 1 spreads
/// the burst into a puff instead of ringing it into a whistle.
const POOF_BODY_Q: f32 = 0.55;
const POOF_BODY_ATTACK_S: f32 = 0.002;
const POOF_BODY_DECAY_S: f32 = 0.022;
const POOF_BODY_DUR_S: f32 = 0.055;
/// The AIR CAP: a brighter, later, quieter breath that gives the puff its
/// dispersal — the "-oof" after the "p".
const POOF_AIR_HZ: f32 = 5000.0;
const POOF_AIR_Q: f32 = 0.65;
/// The cap trails the body: contact first, dispersal after. The BODY is at
/// `delay = 0`, so a deletion still speaks in the first post-cue synth buffer
/// (`every_gesture_speaks_in_its_first_post_cue_buffer`).
const POOF_AIR_DELAY_S: f32 = 0.006;
const POOF_AIR_ATTACK_S: f32 = 0.0035;
const POOF_AIR_DECAY_S: f32 = 0.030;
const POOF_AIR_DUR_S: f32 = 0.080;
/// Cap level against the body's 1.0 — enough to hear the dispersal, not
/// enough to turn the puff into a hiss.
const POOF_AIR_LEVEL: f32 = 0.7;
/// The poof's VOICE gain, on the tier-carrying `g`. Pure band-passed noise
/// peaks far under a tonal voice at equal gain (the Kill swoosh pays 2.6 for
/// the same reason), so the compensation belongs in the VOICE, not in
/// [`BACKSPACE_KIND_GAIN`] — which is a TIER statement and must stay under
/// the keystroke's. Fitted by rendering: an isolated deletion peaks
/// -25.0 dBFS at the host default volume, ~4 dB under a keystroke, pinned by
/// `the_erase_is_an_airy_poof_above_the_keystroke`.
const POOF_VOICE_GAIN: f32 = 1.28;
/// The poof's own softening lowpass — open enough to pass the 5 kHz cap
/// (a keystroke's is 2.2-4.2 kHz), which is the other half of "higher".
const POOF_LP_CUT_HZ: f32 = 7000.0;
/// THE CLOUD'S PUFF ([`SoundKind::Poof`]) — the visible smoke's own little
/// noise, wired to the SAME synthesis family as the erase poof it accompanies
/// (the 2026-08-28/29 reconciliation: the cloud work says WHEN the cue fires,
/// the noise-poof work says WHAT it sounds like). The deletion's retreat
/// already spoke the poof's BODY, so the cloud carries only DISPERSAL: one
/// air-band burst, longer and softer than the cap, settling downward as the
/// smoke thins. No partials, no pitch — and no rng draws (`spawn_seeded`), so
/// the typing melody's stream is independent of how many clouds ride it.
const POOF_CLOUD_DUR_S: f32 = 0.11;
const POOF_CLOUD_ATTACK_S: f32 = 0.008;
const POOF_CLOUD_DECAY_S: f32 = 0.055;
/// Where the cloud's air settles to: the cap's band drifting down as the puff
/// thins — dispersal, not a second contact.
const POOF_CLOUD_SETTLE_HZ: f32 = 3600.0;
const POOF_CLOUD_GLIDE_S: f32 = 0.06;
/// The cloud voice's own gain on the tier-carrying `g` (tier =
/// [`POOF_KIND_GAIN`], the smallest in the vocabulary). Band-passed noise
/// needs the same class of compensation the Kill swoosh's 2.6 is (noise peaks
/// ~17 dB under a tonal voice at equal gain); fitted by rendering so the
/// cloud's air sits ~5.7 dB under the erase poof's own peak (0.032 vs 0.062
/// at the ladder fixture's gain) — plainly present, plainly an accompaniment,
/// never a second key. Pinned relatively by
/// `the_puff_is_the_quietest_delete_in_the_ladder`.
const POOF_CLOUD_GAIN: f32 = 0.85;
/// THE AIR SETTLES. The cap used to hold 5.0 kHz flat; a real puff of air
/// DROPS as it disperses — the "-oof" relaxes. A gentle downward bend
/// (~a minor third in band terms) reads as the poof SETTLING rather than
/// ringing, and it is the erase's articulation half of the family law: the
/// keystroke leans UP onto its note, the deletion breathes DOWN off it.
/// Small on purpose: further and the cap turns into a laser zap.
const POOF_AIR_SETTLE_HZ: f32 = 4100.0;
const POOF_AIR_SETTLE_GLIDE_S: f32 = 0.05;
/// THE HELD-RUN SHIMMER — the little glitter on the LAST release of a held
/// delete run (owner ask, 2026-08-29: "a touch of shimmer on a HELD delete
/// run's last release").
///
/// The engine cannot hear the future, so the "last" poof is found the way the
/// SPACE finds its previous downbeat: every admitted deletion in a held run
/// SCHEDULES a shimmer [`ERASE_SHIMMER_DELAY_S`] out and DAMPS the previous
/// pending one (the pre-delay damp expires it unheard — the switch-damp law).
/// While the key is held, admitted poofs arrive every ~75-105 ms — always
/// inside the delay — so only the run's final poof's shimmer survives to
/// sound, ~160 ms after the finger lifts. Any other authored gesture damps a
/// pending shimmer too: hands back on the keys means the run's story is over.
///
/// A run must be HELD to earn one: at least [`HELD_ERASE_RUN_MIN`] admitted
/// poofs whose gaps each sit inside [`HELD_ERASE_RUN_WINDOW`] (auto-repeat
/// thinned by [`ERASE_MIN_GAP`] lands at 75 ms; deliberate single corrections
/// at typing speed land ~200+ ms and never chain).
const HELD_ERASE_RUN_WINDOW: f32 = 0.14;
const HELD_ERASE_RUN_MIN: u32 = 4;
const ERASE_SHIMMER_DELAY_S: f32 = 0.16;
/// The shimmer's voice: a rising breath of air-band noise with a twinkle —
/// dust settling where the words were. No partials (nothing to clash with
/// the tune), soft attack (an afterglow, not a key), and the twinkle LFO is
/// what makes it SHIMMER rather than hiss.
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
/// The shimmer's retrigger damp (the [`SPACE_DAMP_S`] click-free ramp at the
/// same length): a pending shimmer is expired unheard, a sounding one is
/// released in 12 ms.
const ERASE_SHIMMER_DAMP_S: f32 = 0.012;
/// THE WORD POOF ([`SoundKind::KillWord`]) — the erase poof one size up and
/// one shade softer. A word leaving is a BIGGER puff, not a LOUDER key: the
/// body sits lower (a larger cloud is a darker one) and both bursts run
/// longer, while the whole voice stays under the line kill's swoosh.
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
/// The word poof's voice gain over [`POOF_VOICE_GAIN`]'s fitted base —
/// "slightly larger": measured to land between the single-character poof and
/// the line swoosh on the delete ladder.
const WORD_POOF_GAIN: f32 = 1.1;
/// THE ERASE GATE — the deletion's OWN minimum gap, independent of the
/// keystroke governor's [`MIN_GAP`].
///
/// Two properties, and neither is available from the shared gap. A held
/// Backspace arrives at ~30/s; at 55-80 ms per poof that is a continuous hiss
/// and a voice-pool drain, so deletions are thinned to at most ~13/s among
/// THEMSELVES. And a correction typed at speed ("x", backspace, "y") puts the
/// deletion inside the keystroke's 45 ms window, where the shared gap would
/// swallow the very poof the owner is asking to hear — so the FIRST deletion
/// after typing always speaks, because it is gated on `since_erase` and not on
/// `since_voice`. Symmetrically a deletion does not CLAIM the shared beat
/// (the [`SoundKind::Shift`] law): the letter typed right after a correction
/// must not be thinned by the correction.
const ERASE_MIN_GAP: f32 = 0.075;
// ---------------------------------------------------------------------------
// THE SPACE DOWNBEAT — the word boundary as the phrase's floor
// ---------------------------------------------------------------------------
//
// Owner, 2026-08-26: "I'd like a more musical space bar (maybe spacebar can be
// lower notes)". The space was already the lowest thing in the vocabulary, but
// it was not a MUSICAL one: its pitch came from `if walk * 2 <= 5 { 0 } else
// { 5 }`, the cadence's nearest-tonic rule, which is a function of wherever the
// melody happened to be standing. Degree 0 resolves to `anchor × 0.5` and
// degree 5 to `anchor × 1.0` — so two spaces one word apart could sound a
// FULL OCTAVE apart for no reason the ear can attach to the text. A downbeat
// that moves at random is not a downbeat.
//
// It is a FIXED bass root now: the speaking palette's own tonic, octave-folded
// into one bass register, independent of the melody's degree. The letters
// carry the tune; the spaces are the floor it stands on.
/// The bottom of the SPACE's bass register (Hz). Each palette's melodic anchor
/// is halved into `[SPACE_BASS_LO_HZ, 2 × SPACE_BASS_LO_HZ)` — an OCTAVE fold,
/// so the note keeps the palette's exact pitch class (and therefore stays
/// consonant with its melody) while every palette's downbeat lands in the same
/// bass octave. Without the fold `anchor / 2` spans 98 Hz (Felt) to 440 Hz
/// (Laser): a sub-bass in one voice and a mid-register tone sitting inside the
/// tune in another. A3-ish bottom, so the fundamental survives a laptop
/// speaker's rolloff at all.
const SPACE_BASS_LO_HZ: f32 = 110.0;
/// The downbeat's envelope — short on purpose. A 200 ms low note at prose's
/// ~2 spaces per second is a kick drum: it masks the melodic fundamentals
/// above it, eats the headroom the letters need, and booms in headphones.
/// 92 ms total, ~10 ms attack (a round entry, not a thump), decays away long
/// before the next word boundary.
const SPACE_ATTACK_S: f32 = 0.010;
const SPACE_DECAY_S: f32 = 0.052;
const SPACE_DUR_S: f32 = 0.092;
/// The octave above the root, quietly. A laptop speaker reproduces almost
/// nothing at 130 Hz; the octave is what carries the note's IDENTITY there
/// while the fundamental carries its weight on anything with a woofer.
const SPACE_OCTAVE_LEVEL: f32 = 0.20;
/// The downbeat's VOICE gain. Fitted by rendering to sit ~4 dB under an
/// isolated keystroke (-29.4 dBFS against -25.3 at the host default volume) —
/// the shipped space measured 1.5 dB OVER it. Not the 6 dB an equal-register
/// voice would take: at 130-215 Hz the ear is ~8 phon less sensitive than at
/// the keystroke's 700 Hz-2 kHz, so a 6 dB electrical cut would put the
/// downbeat under the noise floor of the room. Pinned relatively by
/// `the_ladder_holds_for_the_key_family`.
const SPACE_VOICE_LEVEL: f32 = 0.268;
/// The retrigger fade for the MONOPHONIC downbeat. Two spaces close enough to
/// overlap would be two voices at the SAME fixed frequency with independently
/// randomised oscillator phase — which is a comb filter, not a bass note: the
/// pair can cancel to near silence or sum to +6 dB purely on phase luck. One
/// bass voice at a time, retriggered, using the switch-damp's own click-free
/// linear ramp.
const SPACE_DAMP_S: f32 = 0.012;
/// THE WARMTH — a barely-audible perfect FIFTH over the root (owner ask,
/// 2026-08-29: "give it warmth — a soft bloom, a barely-audible fifth"). At
/// `f × 1.5` it keeps the palette's pitch class family (the dominant of the
/// tonic the melody already stands on) and lands in 165-330 Hz — above the
/// bass weight, below the tune. The level is deliberately under the octave's:
/// warmth you FEEL in the chord, not a note you could hum back.
const SPACE_FIFTH_LEVEL: f32 = 0.07;
/// THE DOWNBEAT BREATHES when the hand rests. A space that arrives after a
/// PHRASE pause ([`PHRASE_PAUSE_S`] — the same threshold that resets the bar)
/// is the first beat of a fresh thought, and it gets room: a slightly softer
/// entry, a longer bloom, the octave and fifth lifted a shade. At prose speed
/// (~2 spaces/s, pauses far under the threshold) none of this fires, so the
/// anti-kick-drum envelope above stays the working sound.
const SPACE_BREATHE_ATTACK_S: f32 = 0.014;
const SPACE_BREATHE_DECAY_S: f32 = 0.085;
const SPACE_BREATHE_DUR_S: f32 = 0.150;
const SPACE_BREATHE_OCTAVE_LEVEL: f32 = 0.24;
const SPACE_BREATHE_FIFTH_LEVEL: f32 = 0.10;
/// A COALESCED space — the second and later spaces of one whitespace run —
/// keeps only the downbeat's BREATH, at this level. Indentation and a run of
/// blanks are ONE gesture in the text and get ONE bass note; but a key that
/// makes literally no sound reads as a dropped keystroke, so the run's tail
/// answers with air alone.
const SPACE_RUN_BREATH_LEVEL: f32 = 0.35;
/// TIER 0 — the SUB-FLOOR. Cursor motion is not authorship: it accompanies what
/// you are doing rather than being the thing you did, so the three movement
/// gestures sit AUDIBLY under the typing floor.
///
/// These three were 0.32 / 0.32 / 0.30 until `e8d1a1d9` raised them ~11 dB in a
/// single pass, which put a cursor move at the same level as a keystroke and
/// thickened the texture around every key (the held-key repeat work in
/// `3803f0d7` then cues far more of them). The original values were too quiet
/// to read at all; these land the movement family ~3 dB under the floor, which
/// is present but plainly subordinate — the balance the owner asked for on
/// 2026-08-04 ("tune the noises in relative volume").
///
/// Above the others in the family because a glide's whole voice is one bare
/// sine pluck, intrinsically ~1 dB under a palette keystroke at equal gain.
///
/// UNCHANGED by the 2026-08-10 register move (the movement family now sings at
/// each palette's own [`Palette::anchor_hz`] rather than a fixed 330 Hz):
/// re-measured at -24.16 dBFS against -24.10 before, i.e. 0.06 dB — the tier
/// is stated in absolute dBFS and it did not move.
const GLIDE_KIND_GAIN: f32 = 0.78;
/// TIER 0 — one per gesture, not one per character. No production path cues
/// `Navigation` (`cursor_glow` splits every hinted move into Glide/Sweep);
/// the arm serves the audition harness and hosts that still speak it.
const NAVIGATION_KIND_GAIN: f32 = 0.68;
/// TIER 0. The per-note taper in `design_cursor` drops the run's tail; this
/// sets where its FIRST note lands.
///
/// RE-FITTED 2026-08-10 (0.73 → 0.692) for the register move [`GLIDE_KIND_GAIN`]
/// rode out unchanged: a word run's notes OVERLAP, so where a single glide's
/// level was untouched a whole run gained 0.46 dB. Given back here, so rainbow
/// kitty's Sweep measures -23.7 dBFS — its exact pre-move level.
const SWEEP_KIND_GAIN: f32 = 0.692;
/// TIER 3 — a kill is per-command and destroys a line, so it may not be the
/// quietest thing in the engine. Most of the level correction lives in the
/// swoosh's own voice gain rather than here, because the deficit is a VOICE
/// deficit — see `design_trail`'s Kill arm.
const KILL_KIND_GAIN: f32 = 1.25;
/// TIER 2.5 — a WORD KILL destroys a word, not a line: audibly above the
/// single character's poof ([`BACKSPACE_KIND_GAIN`], with the rest of the
/// "slightly larger" in [`WORD_POOF_GAIN`]'s voice compensation), audibly
/// under the line kill's swoosh. Pinned relatively by
/// `the_word_kill_sits_between_the_poof_and_the_swoosh`.
const KILLWORD_KIND_GAIN: f32 = 1.0;
/// UNDER THE FLOOR, with the SHIFT lift — and for the same reason. The cloud's
/// puff ([`SoundKind::Poof`]) accompanies a keystroke that has already spoken,
/// so it is an ACCOMPANIMENT, not authorship: it must be plainly audible as a
/// texture and never as a second key. Sits at the bottom of the ladder, below
/// [`SHIFT_KIND_GAIN`], because unlike the lift it layers ON TOP of the very
/// bell it follows rather than replacing anything.
///
/// Like [`KILL_KIND_GAIN`], most of the level lives in the voice: this is pure
/// band-passed noise with no partials, ~17 dB under a tonal voice at equal
/// gain, and `design_poof` carries that compensation. This number is the
/// PRIORITY statement, and it is the smallest one in the vocabulary.
///
/// FITTED BY MEASUREMENT, not by taste alone (`examples/erase_ab`): at 0.42 the
/// puff metered -34.0 dBFS against the Backspace bell's -22.9 — 11 dB down and
/// 16 ms behind it, which is inside the bell's forward-masking shadow, i.e. the
/// same "cued but never heard" outcome the swoosh it replaced already had. 0.68
/// lands it ~7 dB under the bell: plainly present, plainly subordinate — the
/// same relationship the movement family holds to the typing floor.
const POOF_KIND_GAIN: f32 = 0.68;
/// TIER 3. With the palette trim in place this lands Jump at -17.6 dBFS, and
/// it is the trail ceiling [`BONK_KIND_GAIN`] must stay strictly above
/// (const-asserted).
const JUMP_KIND_GAIN: f32 = 1.25;

/// Bonk kind-gain. Deliberately ABOVE every trail gesture (Jump's 1.25 is the
/// trail ceiling): punctuation with higher priority — the wrong note must not
/// hide in the texture it interrupts.
const BONK_KIND_GAIN: f32 = 1.35;

/// The cursor-LANDING star chime ([`SoundKind::Land`]). Register an octave
/// over the trail's warm mid — the stars glitter ABOVE the tune, they do not
/// sit in it.
const LAND_ANCHOR_HZ: f32 = 784.0;
/// TIER 4 — the landing kind-gain. DELIBERATELY BELOW the trail tiers' ceiling
/// (Jump/Kill 1.25): the landing's presence comes from its VOICE DESIGN — three
/// stacked star tones over an arrival body, four voices where a keystroke has
/// one — not from its gain. Raised to the ceiling the chime measures
/// -12.8 dBFS, LOUDER than the bonk it is meant to sit under; 0.865 lands it on
/// tier 4 at -16.0 dBFS, 2 dB under the bonk. Its margin over a keystroke
/// varies by palette (+5.9 dB against Laser, +10.9 against rainbow kitty) because Land
/// is style-agnostic while the keystroke is not.
const LAND_KIND_GAIN: f32 = 0.865;
/// Star tones per landing, [`LAND_STAR_DEGREE_STEP`] scale-degrees apart and
/// pre-delayed [`LAND_STAR_STEP_S`] — the arpeggio idiom (delayed voices, no
/// scheduler). The FIRST has `delay = 0`, so a landing always speaks in the
/// first post-cue synth buffer.
const LAND_STARS: usize = 3;
const LAND_STAR_STEP_S: f32 = 0.034;
const LAND_STAR_DEGREE_STEP: i32 = 2;

/// Master duck: how deep the melody + bed dip while a bonk speaks (0.55 ⇒
/// −7 dB at the instant of impact), and the exponential recovery time. Sized
/// so the dip is felt as the room making way rather than a volume glitch,
/// and fully recovered within ~a second.
const BONK_DUCK_DEPTH: f32 = 0.55;
const BONK_DUCK_TAU: f32 = 0.28;

/// How many samples a geometric envelope/glide recursion may run before it is
/// re-anchored to a real `exp`. 256 samples is ~5.3 ms at 48 kHz, so a voice
/// pays one exponential per recursion per 5 ms instead of one per sample —
/// keeping essentially all of the win — while bounding f32 drift to what
/// compounds over 256 multiplies rather than over a whole note.
const ENV_REANCHOR: u32 = 64;

/// One celebration riff bar in seconds — 4 beats at the sing-along's
/// 150 BPM. Pinned equal to the VISUAL clock's `kitty_sing::SING_BAR_SECONDS`
/// by `celebration_bar_matches_the_visual_clock`, so the host can schedule
/// one [`CelebrationGesture::RiffBar`] per visual bar and the two clocks
/// share one tempo (sync tolerance documented on the gesture).
pub const CELEBRATION_BAR_SECONDS: f32 = 1.6;

/// One riff eighth-note in seconds (8 per bar).
const CELEBRATION_EIGHTH: f32 = CELEBRATION_BAR_SECONDS / 8.0;

/// A grid slot that HOLDS — no new note; the previous one sustains through it.
/// A sentinel rather than `Option<i32>` so the tables stay the flat `[i32; 8]`
/// literals this module reads at a glance, and the const-table, zero-allocation
/// discipline is untouched.
const REST: i32 = i32::MIN;

/// Bars in the authored phrase. A POWER OF TWO on purpose: the host truncates
/// its `u64` bar counter into the gesture payload, so the form index must
/// survive that wrap IN PHASE (`2^16 % 8 == 0`) or the song would jump-cut once
/// per wrap. Pinned by `celebration_form_is_wrap_safe`.
const CELEBRATION_PHRASE_BARS: usize = 8;

/// THE SING-ALONG RIFF — an ORIGINAL composition, an eight-bar
/// just-intonation MAJOR-pentatonic song (degrees on [`PENTA`]). Deliberately
/// NOT the copyrighted "Nyan Cat" melody (Momoiro Clover Z / daniwell's
/// "Nyanyanyanyanyanyanya!"): the homage is the mood — chip pulse waves,
/// pentatonic bounce, relentless cheer — never the tune. Authored here,
/// mutually consonant with everything else the engine plays by the
/// pentatonic-lattice law, so the typed-note melody under it can never rub.
///
/// The form is A A' B A" | C C' B' D — verse, lifted chorus, breathing bridge,
/// turnaround; bars 0 and 1 are the hook the celebration opens on.
///
/// `REST` slots do NOT silence: the preceding note SUSTAINS through them, which
/// is where the phrase's long notes and its air come from.
const CELEBRATION_PHRASE: [[i32; 8]; CELEBRATION_PHRASE_BARS] = [
    // 0 — A, THE HOOK.
    [0, 2, 4, 5, 4, 2, 4, 7],
    // 1 — A', THE ANSWER.
    [5, 7, 5, 4, 2, 4, 2, 0],
    // 2 — B, the hook syncopated: two held notes and two holes.
    [2, 4, REST, 7, 5, REST, 3, 2],
    // 3 — A", the hook again, half-cadencing high and HOLDING over the
    //     dominant pedal below — the song's first real breath.
    [0, 2, 4, 5, 7, REST, 8, REST],
    // 4 — C, THE LIFT: the chorus, up an octave, peaking on the 9th degree.
    [5, REST, 7, 8, 9, 7, 5, 4],
    // 5 — C', the descent out of the top.
    [9, 8, 7, 5, 7, 5, 4, 2],
    // 6 — B', the bridge: four HELD notes, low and wide. This bar's air is
    //     what stops the song reading as a machine.
    [0, REST, 2, REST, 4, REST, 2, REST],
    // 7 — D, THE TURNAROUND: climb, then [`CELEBRATION_FILL`] tumbles the
    //     phrase back onto bar 0's tonic. The last two slots belong to the
    //     fill, which is why they rest here.
    [4, 5, 7, 8, 9, 7, REST, REST],
];

/// The TURNAROUND FILL: four SIXTEENTHS across the last two slots of the last
/// bar, landing the song back on the hook.
const CELEBRATION_FILL: [i32; 4] = [7, 5, 4, 2];

/// THE BASSLINE, one note per BEAT (four per bar) — an independent voice with
/// its own motion, so the lead has a harmony to follow.
///
/// A bass note rides as the THIRD PARTIAL of the lead voice that OPENS its
/// beat — zero extra voices — so every non-`REST` bass beat REQUIRES a
/// non-`REST` lead slot at `2 * beat`, pinned by
/// `celebration_bass_always_has_a_carrier`.
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

/// THE GROOVE — per-slot velocity across the eighth grid, shared by every bar:
/// downbeat strongest, a BACKBEAT lift on slots 2 and 6, offbeats ghosted —
/// ~4.5 dB of internal dynamics.
const CELEBRATION_GROOVE: [f32; 8] = [0.55, 0.33, 0.44, 0.36, 0.50, 0.33, 0.46, 0.38];

/// Per-BAR dynamic shape multiplying [`CELEBRATION_GROOVE`]: the chorus pushes,
/// the bridge drops back so the turnaround has somewhere to go.
const CELEBRATION_BAR_LIFT: [f32; CELEBRATION_PHRASE_BARS] =
    [1.00, 0.96, 1.02, 1.00, 1.12, 1.08, 0.86, 1.06];

/// SWING: odd eighths land this fraction of an eighth LATE. Applied to a note's
/// PRE-DELAY only — the bar is still exactly [`CELEBRATION_BAR_SECONDS`] long,
/// so the visual clock this module pins itself to cannot drift
/// (`celebration_bar_matches_the_visual_clock`).
const CELEBRATION_SWING: f32 = 0.14;

/// ESCALATION: the song opens lean and fills in over its first bars — the low
/// end fades up, the lowpass opens, the octave shimmer grows. A pure function of
/// the bar index: no new rng draws, so the riff stays replayable independently
/// of what the typed layer consumed from the shared stream.
const CELEBRATION_BUILD_BARS: f32 = 6.0;

/// From this bar on (one full phrase in), the backbeat slots pick up a chip
/// CLAP — a short noise burst fused into the note's OWN noise channel, so it
/// costs no extra voice.
const CELEBRATION_CLAP_BAR: u16 = 8;

/// Riff register root — C5, the rainbow kitty palette's own chip register.
const CELEBRATION_BASE_HZ: f32 = 523.25;

/// TIER 5 — the sing-along riff's place on the loudness ladder. The riff is the
/// ONLY governor-exempt gesture (`design_celebration` omits the flood duck by
/// design), so without a kind gain of its own it escapes the ladder entirely:
/// at 1.0 bar 12 renders at -9.5 dBFS, 2 dB OVER the bonk and 11 dB over a
/// keystroke it is simultaneously ducking by -9 dB ([`SING_DUCK_DEPTH`]).
///
/// 0.6 lands the whole ESCALATION inside tier 5: the riff opens at -18.9 dBFS
/// (bar 0, lean), reaches -14.9 by the chorus and tops out at -13.7 with the
/// full build and clap. It still audibly grows — it grows INTO the ceiling
/// instead of through it.
const CELEBRATION_KIND_GAIN: f32 = 0.6;

/// THE SWITCH DAMP — how long an old key's still-ringing celebration voices
/// take to yield once a bar arrives under a DIFFERENT signature (owner: "the
/// transition between switching between repeated keys also has overlapping
/// music. that needs to be fixed"). Riff notes deliberately ring past the bar
/// line — 1.5× legato plus decay tails — and held on ONE key that overhang is
/// the song's sustain pedal; across a key switch the same tails carry the OLD
/// root + mode under the NEW verse: two keys at once. 50 ms is long enough to
/// read as a lifted pedal rather than a cut, short enough that the clash never
/// registers as harmony. Same-sig bars never damp, so a single-key celebration
/// renders byte-identically to the pre-damp synth; release-without-switch is
/// untouched too — the sing-duck wind-down already owns that exit.
const CELEBRATION_DAMP_S: f32 = 0.05;

// ---------------------------------------------------------------------------
// The per-key song axes (owner: "I also want a more obvious difference in the
// tune generation when pressing different keys")
// ---------------------------------------------------------------------------
//
// Every axis below is a PURE FUNCTION of the gesture payload's `sig` — the
// bijective per-character song signature (`kitty_sing::song_signature`). The
// seamless hand-over law forbids synth-side per-key state, so a mid-hold key
// change is nothing but the next bar arriving with a different payload.
//
// The walk encoding is re-implemented from the salvaged held-key-songs design
// (the crashed 2026-07 branch): its mixer + mixed-radix decode predate the
// loudness ladder so the code could not be cherry-picked, but the scheme is
// its — first symbol radix 5, then base-4 interval transitions;
// `5 * 4^15 > 2^32`, so sixteen coded notes carry the ENTIRE signature
// injectively.

/// THE VERSE WALK's four moves — the mixed-radix decode spends one base-4
/// digit of `sig / 5` per transition. No move is 0 (mod 5), so adjacent
/// coded notes ALWAYS land on a different scale degree: no key's verse can
/// degenerate into a drone, and nearby characters get different CONTOURS,
/// not transposed copies of one authored line.
const SONG_INTERVALS: [i32; 4] = [-1, 1, -2, 2];

/// Coded notes per walk cycle: sixteen carry every bit of the `u32`
/// signature without collision, plus one BRIDGE note ([`SONG_STEPS`]) chosen
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

/// AXIS 2 — THE ROOT the whole celebration transposes to: `sig % 5 - 2`,
/// structurally inside one pentatonic octave (-2..=2). The register guard is
/// unchanged from the old `key` payload: no character can transpose the riff
/// into a register that reads as a different instrument.
fn celebration_root(sig: u32) -> i32 {
    (sig % 5) as i32 - 2
}

/// AXIS 3 — MODE-FEEL: an integer degree rotation folded (with the root)
/// into every sounding degree BEFORE [`TrailSynth::melody_hz`]. Pentatonic
/// modes ARE rotations, and the lattice is JUST intonation — its five steps
/// are unequal — so rotating the song's degrees changes the interval COLOR
/// under the same contour while staying on the shared consonant lattice (the
/// de8589d9 mood seam multiplies on top, and tone tables are NEVER swapped
/// per key — that would hand the bonk's no-beating law to a keyboard). The
/// rotation class is `sig % 3` ∈ {0, 2, 4}; the 4-rotation is realized
/// octave-folded as −1 (a five-note rotation by 4 IS the rotation by −1, one
/// octave apart), so the register guard extends to this axis: the combined
/// root+mode offset stays in −3..=4.
const MODE_ROTATIONS: [i32; 3] = [0, 2, -1];
fn celebration_mode(sig: u32) -> i32 {
    MODE_ROTATIONS[(sig % 3) as usize]
}

/// AXIS 4 — per-key pulse DUTY for the riff's lead (chip timbre families
/// {0.25, 0.375, 0.5}), wired but GATED OFF: an owner decision pending the
/// owner's ear. Duty is the loudest timbral lever the chip voice has — three
/// keys apart would read as three different instruments — so it ships dark
/// until it has been heard. Flipping the flag is the whole change.
const CELEBRATION_KEY_DUTY: bool = false;
fn celebration_duty(sig: u32) -> f32 {
    if CELEBRATION_KEY_DUTY {
        [0.25, 0.375, 0.5][((sig >> 8) % 3) as usize]
    } else {
        0.25
    }
}

/// Which form slots are VERSE (per-key walk) vs the SHARED CHORUS. The
/// verses are the A-family (bars 0/1/3) and the lift pair C/C' (bars 4/5);
/// the syncopated B (2), the breathing bridge B' (6) and the turnaround D
/// (7, fill included) keep the AUTHORED phrase for every key — the shared
/// chorus that keeps every key's song recognizably THE celebration.
const CELEBRATION_VERSE: [bool; CELEBRATION_PHRASE_BARS] =
    [true, true, false, true, true, true, false, false];

/// Register shelf lifting the walk's `0..=4` window into each verse
/// family's authored tessitura: the A-family verses sit just over the hook's
/// shelf, the C-family verses keep THE LIFT — the form still rises into
/// bars 4/5 for every key.
fn verse_register(idx: usize) -> i32 {
    if idx == 4 || idx == 5 { 4 } else { 1 }
}

/// The walk ordinal of the FIRST sounding slot of verse bar `idx` (`None`
/// for chorus bars): sounding verse slots are numbered consecutively across
/// the form, so the walk keeps striding instead of restarting per bar.
/// Counted from the authored REST pattern, never hand-pinned.
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

/// AXIS 1 — THE VERSE. This bar's CONTOUR row for `sig`: chorus bars return
/// the AUTHORED row verbatim (byte-identical for every signature — pinned by
/// `chorus_bars_are_byte_identical_across_sigs`); verse bars keep the
/// authored RHYTHM (the REST pattern, and with it the groove, swing, span
/// and bass-carrier laws) and replace each sounding degree with the
/// signature's walk. Root and mode are NOT applied here — this is the
/// contour; `design_celebration` folds the shift in at pitch time so the
/// chorus modulates with the song while its contour stays shared.
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

/// The pre-delay of eighth-grid slot `slot`, SWUNG: odd slots land
/// [`CELEBRATION_SWING`] of an eighth late. Slot 0 is always exactly 0.0, so
/// every bar still speaks in its first post-cue synth buffer.
fn celebration_slot_delay(slot: usize) -> f32 {
    let swing = if slot.is_multiple_of(2) {
        0.0
    } else {
        CELEBRATION_SWING
    };
    (slot as f32 + swing) * CELEBRATION_EIGHTH
}

/// PITCH-PANNED stereo: the song's notes move across the field with their
/// REGISTER instead of ping-ponging L/R on every eighth (a 5 Hz alternation is
/// itself a repetition cue). Degree 4 — the middle of the singable range — sits
/// at the event's own pan. `spawn` narrows and clamps afterwards, so nothing can
/// land fully in one ear.
fn celebration_sway(pan: f32, deg: i32) -> f32 {
    pan + 0.06 * (deg - 4) as f32
}

/// How many eighth-grid slots the note at `i` OWNS: itself plus the `REST` slots
/// that follow, bounded by `limit` (the turnaround bar caps this at 6 so the
/// sustained note cannot ring under the sixteenth fill).
fn celebration_span(phrase: &[i32; 8], i: usize, limit: usize) -> usize {
    let mut span = 1usize;
    while i + span < limit && phrase[i + span] == REST {
        span += 1;
    }
    span
}

/// Sing-along duck: how deep the ordinary melody + bed sit while the riff
/// speaks (0.65 ⇒ about −9 dB: the riff REPLACES the melody rather than
/// merely dimming it), and the exponential HANDBACK time once bars stop
/// arriving — the audio half of the ~1 s wind-down crossfade.
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
    /// make the comet's chimes read as ICE rather than organ.
    fm_ratio: f32,
    fm_i0: f32,
    fm_tau: f32,
    wave: Wave,
    ph: f32,
    fm_ph: f32,
    /// GEOMETRIC GLIDE / FM-INDEX STATE. `g_e` is the running `e^(-t/glide)`
    /// and `fm_e` the running `e^(-t/fm_tau)`; `k_g`/`k_f` are their constant
    /// per-sample steps `e^(-dt/τ)`, computed once in `TrailSynth::spawn`.
    /// Both running values are SEEDED with a real `exp` at the voice's first
    /// sounding sample and stepped by one multiply thereafter — see the note
    /// on `Voice::env_run`.
    g_e: f32,
    k_g: f32,
    fm_e: f32,
    k_f: f32,
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
    /// GEOMETRIC ENVELOPE / NOISE-GLIDE STATE: the running `e^(-t/attack)`,
    /// `e^(-t/decay)` and `e^(-t/n_glide)`, with their constant per-sample
    /// steps `e^(-dt/τ)` from `TrailSynth::spawn`. `attack`/`decay`/`n_f0`/
    /// `n_f1` themselves are left untouched — they are the voice's DESIGN and
    /// several tests read them back after `push`.
    env_a: f32,
    env_d: f32,
    k_a: f32,
    k_d: f32,
    n_e: f32,
    k_n: f32,
    /// False until the voice's FIRST SOUNDING sample, where every recursion
    /// above is seeded from a real `exp` at that exact `t`. It cannot be
    /// seeded in `spawn`: a pre-delayed voice starts at `t = -delay`, and
    /// when `t` finally crosses zero it is NOT a multiple of `dt`, so the
    /// sequence has to start from wherever the crossing landed.
    env_run: bool,
    /// Samples since the recursions above were last anchored to a real `exp`.
    ///
    /// A geometric recursion is exact in the reals but DRIFTS in f32: each step
    /// carries a rounding error and they compound. Measured against the v0.56
    /// oracle, seeding once per note and multiplying for the rest of an 8 s
    /// ring-out reached a peak deviation of 7.0e-4 — 23 times a 16-bit
    /// quantization step, at -63 dBFS, which is NOT inaudible. Re-anchoring
    /// every [`ENV_REANCHOR`] samples bounds the compounding to that window
    /// while still replacing all but one exponential in it.
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
    /// PRECOMPUTED render coefficients — the sample loop's REMAINING
    /// per-sample loop invariants, resolved once by [`TrailSynth::spawn`]
    /// from fields that never move again (`dt` is `inv_sr`, written once in
    /// `new` and never again; the geometric `k_*` steps above already work
    /// this way). Same expressions, same operands, evaluated once instead
    /// of 48 000 times a second — IEEE-754 ops are deterministic, so every
    /// rendered sample stays bit-identical.
    ///
    /// `lp_k` is the per-voice softening-lowpass coefficient; `n_damp` the
    /// noise SVF's damping `1/Q`. `svf_g`/`svf_den` are the SVF coefficient
    /// and DENOMINATOR — cached only when the noise cutoff is a lifetime
    /// constant (`n_lvl > 0` and `n_glide <= 0`, which is Lumen's and
    /// Comet's air band), where the loop was paying a scalar `tanf` per
    /// sample for a number that never changed. The DIVISOR is cached, not
    /// its reciprocal: `x / d` and `x * (1.0 / d)` are different f32 values
    /// and this render is judged on bits.
    lp_k: f32,
    n_damp: f32,
    svf_g: f32,
    svf_den: f32,
    /// Exempt from the master duck envelope. Only the bonk sets this: the
    /// duck exists to make room FOR it, so ducking the bonk itself would
    /// cancel the gesture. `false` (every trail voice, every bed grain)
    /// renders on the exact pre-framework signal path — with the duck
    /// envelope at rest its multiplier is exactly 1.0, proven bit-identical
    /// by the `v056_reference` tests.
    duck_exempt: bool,
    /// A SING-ALONG CELEBRATION voice — set by every `design_celebration`
    /// spawn, bar notes and turnaround fill alike. The SWITCH DAMP's address
    /// tag: `duck_exempt` cannot serve (the bonk shares it, and a key change
    /// must never damp the bonk). Default `false` keeps every other path
    /// byte-identical.
    celebration: bool,
    /// THE SPACE DOWNBEAT'S address tag — set by `design_space`'s bass voice
    /// and nothing else, so a new word boundary can find the previous one and
    /// damp it (see [`SPACE_DAMP_S`]). A separate flag from `celebration` on
    /// purpose: a riff bar must never damp the typing's floor, and the floor
    /// must never damp the riff. Default `false` keeps every other path
    /// byte-identical.
    bass: bool,
    /// THE HELD-RUN SHIMMER'S address tag — set by the pending release
    /// glitter [`design_erase_poof`](TrailSynth::design_erase_poof) schedules
    /// and nothing else, so the run's next poof (or any authored gesture) can
    /// find the pending one and damp it before it sounds (see
    /// [`ERASE_SHIMMER_DELAY_S`]). Its own flag for the same reason `bass`
    /// is: a shimmer must never damp the downbeat and a space must never damp
    /// the shimmer's *sounding* tail by address. Default `false` keeps every
    /// other path byte-identical.
    shimmer: bool,
    /// SWITCH-DAMP release remaining (seconds); `0.0` = not damped. Armed by
    /// `design_celebration` when a bar arrives under a NEW signature; `render`
    /// burns it on the sample clock — pre-delay included, so an old-key voice
    /// that never started expires unheard — and scales the envelope by the
    /// linear ramp `damp / CELEBRATION_DAMP_S`: the 5 ms anti-click ramp's own
    /// law at pedal-lift scale, click-free with no new envelope machinery.
    /// The `0.0` default keeps the branch untaken, so every pinned path
    /// renders through the exact pre-damp arithmetic.
    damp: f32,
    /// The FULL length the damp above was armed to — the ramp's denominator,
    /// so a release starts at exactly ×1.0 whatever its length. Written beside
    /// every `damp`; a key switch arms [`CELEBRATION_DAMP_S`] and a space
    /// retrigger the much shorter [`SPACE_DAMP_S`]. (Before this field the
    /// denominator was the celebration constant literally, which is the same
    /// f32 on that path — the pinned renders are unmoved.)
    damp0: f32,
}

impl Voice {
    /// Peak-ish loudness proxy used for voice stealing (quietest loses).
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
/// the most recent TRAIL event (styles change via settings, not mid-phrase,
/// so a crossfade would be over-engineering — the energy smoothing already
/// softens the hand-off). Word gestures feed it nothing: a bonk must not
/// swell the ambience it interrupts.
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
    /// TOURNAMENT-candidate state (used only while [`BedVariant`] ≠
    /// `Current`; dead weight otherwise — `Default`-zeroed, never read by the
    /// shipping palette beds, so the pinned paths cannot observe it):
    /// oscillator phases (turns), portamento'd oscillator frequencies (Hz;
    /// `<= 0` means "not yet seeded"), and the variant's own slow clock in
    /// seconds (chord bars, breath cycles, shimmer LFOs all derive from it —
    /// samples-driven like every other clock here, so candidates replay
    /// bit-exactly).
    var_ph: [f32; 4],
    var_f: [f32; 4],
    var_t: f32,
}

// ---------------------------------------------------------------------------
// Ambient-bed TOURNAMENT — candidate variants behind the audition seam
// ---------------------------------------------------------------------------

/// One AMBIENT-BED TOURNAMENT candidate. Beds ship off-by-default behind
/// `trail_sound_bed`, and the redesign of the low drone runs as a judged
/// tournament: each candidate is a complete alternative continuous-bed
/// design, rendered and measured by `examples/bed_audition.rs` against the
/// real melody.
///
/// Selection is an ENGINE-LEVEL seam ([`TrailSynth::set_bed_variant`]), not a
/// host setting: no config path reaches it, the default is [`Current`]
/// (`BedVariant::Current`), and with the default selected the palette beds
/// render through the exact pre-tournament code path — the shipping sound
/// stays byte-pinned while the challengers live beside it.
///
/// Candidate DSP obeys the same two laws as everything else in this module:
/// - CONSONANCE — every candidate pitch is drawn through
///   [`TrailSynth::melody_hz`] at integer lattice degrees, so whatever tone
///   table the melody is in, bed tones sit ON that table (mutual consonance
///   inherited structurally, proven by
///   `bed_variant_pitches_stay_on_the_active_lattice_for_every_tone`);
/// - LOUDNESS — candidates ride the same energy/level/gain machinery as the
///   shipping beds (fed per event, governor-smoothed, master-ducked), so the
///   flood law holds per candidate
///   (`every_bed_variant_keeps_the_flood_duck_law`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BedVariant {
    /// C0 — the incumbent-in-code: the per-palette `Palette::bed_sample`/
    /// `bed_grain` units. The default, and the only variant hosts can ever
    /// reach.
    #[default]
    Current,
    /// C1 — SLOW CHORD DRIFT: three bed tones stacked on the pentatonic
    /// lattice walk a four-bar triad progression over a ~30 s cycle,
    /// portamento gliding between bars — never static, never stepping.
    ChordDrift,
    /// C2 — BREATHING PAD: a lattice pad whose amplitude swells 0.05→0.2 on
    /// a ~12 s raised cosine while a lowpass tilt opens and closes with the
    /// same breath — the bed inhales and exhales instead of holding a note.
    Breathing,
    /// C3 — SHIMMER WASH: four very quiet HIGH lattice partials (nothing
    /// below ~4× the palette anchor — no low fundamental at all) fading in
    /// and out on slow incommensurate LFOs; air, not floor.
    Shimmer,
    /// C4 — SILENCE: no bed. The shipping DEFAULT experience (the
    /// `trail_sound_bed` gate is off) as an explicit tournament entrant, so
    /// "keep no bed" is judged with the same artifacts as every challenger.
    /// Contributes literally zero samples
    /// (`silence_candidate_contributes_exact_zero_bed_samples`).
    Silence,
}

impl BedVariant {
    /// Every tournament entrant, C0..C4 — the audition harness and the
    /// variant proofs iterate this so a new candidate is automatically
    /// rendered and law-checked.
    pub const ALL: [BedVariant; 5] = [
        BedVariant::Current,
        BedVariant::ChordDrift,
        BedVariant::Breathing,
        BedVariant::Shimmer,
        BedVariant::Silence,
    ];
}

/// C1 chord cycle: total walk length in seconds (four bars of ~7.5 s — slow
/// enough to read as weather, fast enough that a 30 s listen hears motion).
const CHORD_DRIFT_CYCLE_S: f32 = 30.0;

/// C1 bar roots, in pentatonic-lattice degrees. A I → IV-ish → ii-ish →
/// V-ish walk that ends a step above home so the cycle leans back into bar
/// one — "never static" as a structural property of the progression, not a
/// tuning accident.
const CHORD_DRIFT_ROOTS: [i32; 4] = [0, 3, 1, 4];

/// C1 chord stack on each root: the pentatonic root triad (degrees 0/2/4 of
/// the active table — under the major table that is 1 : 5/4 : 5/3). Same-
/// table stacking is what makes consonance structural: every chord tone is a
/// lattice degree, and lattice-degree pairs are proven outside the bonk's
/// rub zones for every tone table.
const CHORD_DRIFT_STACK: [i32; 3] = [0, 2, 4];

/// C1 portamento time constant (seconds) between bar targets. ~1.2 s: long
/// enough that bar changes read as the pad LEANING to the next chord, short
/// enough that the glide (the only moment bed pitch is off-lattice) is a
/// transition, not a state.
const CHORD_DRIFT_GLIDE_TAU: f32 = 1.2;

/// C2 breath period in seconds, and the swell-law endpoints: the
/// pad's amplitude factor rides `0.05..=0.2` on a raised cosine — even at
/// the top of the breath the bed stays a texture, and at the bottom it all
/// but disappears without ever gating.
const BREATH_CYCLE_S: f32 = 12.0;
const BREATH_SWELL_MIN: f32 = 0.05;
const BREATH_SWELL_MAX: f32 = 0.2;

/// C2 pad voicing: root triad + octave root (lattice degrees on the active
/// table, one octave under the palette anchor — the same register the
/// shipping beds hum in).
const BREATH_DEGREES: [i32; 4] = [0, 2, 4, 5];

/// C3 wash voicing: HIGH lattice degrees on the palette anchor itself —
/// degree 10 is 4× the anchor (≈2.1 kHz on the rainbow kitty chip register), 15 is
/// 8×. Nothing lower exists in this candidate; "no low fundamental" is the
/// voicing, not a filter.
const SHIMMER_DEGREES: [i32; 4] = [10, 12, 14, 15];

/// C3 per-partial shimmer LFO rates in Hz. Mutually incommensurate-ish so
/// the four fades never phase-lock into a loop the ear can count — the wash
/// glitters instead of pulsing.
const SHIMMER_RATES: [f32; 4] = [0.13, 0.19, 0.29, 0.23];

// ---------------------------------------------------------------------------
// The synth
// ---------------------------------------------------------------------------

/// See the module docs. Construct once per output stream with the stream's
/// sample rate, [`push`](Self::push) events from the trail-spawn edge (and
/// the word-decoration drain), call [`render`](Self::render) from the audio
/// callback.
#[derive(Clone, Debug)]
pub struct TrailSynth {
    inv_sr: f32,
    rng: u32,
    voices: [Voice; MAX_VOICES],
    bed: Bed,
    bed_style: GlowStyle,
    /// The voice family latched beside [`Self::bed_style`] by the same trail
    /// events, so the bed texture follows the `trail_sound_style` override
    /// exactly like it follows the visual style.
    bed_voice: SoundVoice,
    /// Which AMBIENT-BED design the bed mixer renders — the tournament seam.
    /// Always [`BedVariant::Current`] outside the audition harness (no host
    /// path sets it), so the shipping palette beds stay byte-pinned.
    bed_variant: BedVariant,
    /// Smoothed event rate (events/second) for the governor.
    rate: f32,
    /// Seconds since the last ADMITTED discrete voice (min-gap thinning).
    since_voice: f32,
    /// Seconds since the last event of any kind (rate decay bookkeeping).
    since_event: f32,
    /// Seconds since the last ADMITTED deletion — the erase gate's own clock
    /// ([`ERASE_MIN_GAP`]). Separate from [`Self::since_voice`] on purpose:
    /// a poof must survive a correction typed inside the keystroke gap, and a
    /// held Backspace must be thinned even when nothing else is speaking.
    since_erase: f32,
    /// HELD-RUN length: consecutive ADMITTED deletions whose gaps each sat
    /// inside [`HELD_ERASE_RUN_WINDOW`]. Reaches [`HELD_ERASE_RUN_MIN`] only
    /// under auto-repeat; read by the erase designer to decide whether the
    /// run has earned its release shimmer.
    erase_run: u32,
    /// WHITESPACE-RUN state: `true` when the previous TEXT gesture was a
    /// space. Only the run's HEAD is a bass downbeat; its tail answers with
    /// air ([`SPACE_RUN_BREATH_LEVEL`]). Set by every Space event whether or
    /// not the governor admitted it — the run is a property of the TEXT, so a
    /// thinned space still closes the run — and cleared by every other text
    /// gesture. A bare [`SoundKind::Shift`] leaves it alone: a modifier is not
    /// a character.
    space_run: bool,
    /// Scratch: does the space now being designed OPEN its run? Written by
    /// [`Self::push`] one line before the design call that reads it, so the
    /// run law lives with the rest of the admission policy instead of being
    /// re-derived inside the voice designer.
    space_head: bool,
    /// The melody's CURRENT degree on the pentatonic lattice: the derived
    /// output of the phrase generator (`phrase_home + phrase_step + arc`,
    /// clamped into [`tone_register`]) — see [`Self::advance_melody`].
    /// Advanced by TRAIL gestures only: a bonk clashes AGAINST the current
    /// degree, it does not move the melody. It is the one scalar every
    /// consumer reads — the bonk clashes against it, `design_trail` offsets
    /// it by the column, a cursor Glide/Sweep plays relative to it.
    walk: i32,
    /// PHRASE-AWARE MELODY STATE. The 4-step motif CELL — scale-step deltas
    /// re-chosen each phrase and replayed to fill it, so a recognisable shape
    /// RECURS and varies instead of drifting.
    motif: [i8; 4],
    /// Index of the current note within the phrase (`0..phrase_len`).
    phrase_pos: u8,
    /// This phrase's length in notes ([`PHRASE_MIN`]..=[`PHRASE_MAX`]).
    phrase_len: u8,
    /// CALL-AND-RESPONSE toggle (the celebration bar-parity idiom applied to
    /// the typed line): flipped at every phrase boundary. Even phrases ASK
    /// (the motif as chosen), odd phrases ANSWER (the motif inverted) — two
    /// shapes reading as dialogue.
    phrase_parity: bool,
    /// Unclamped in-phrase degree ACCUMULATOR (reset to 0 each phrase); the
    /// pitch register is `phrase_home + phrase_step + arc`, clamped. Kept
    /// unclamped so the motif's delta pattern stays exactly periodic (the
    /// motif genuinely recurs) even when the clamped pitch saturates.
    phrase_step: i32,
    /// The register ANCHOR this phrase opened on — the tonic the previous
    /// cadence resolved to.
    phrase_home: i32,
    /// THE BAR POSITION — the index into [`SONG_PULSE`] the NEXT keystroke
    /// will play. Advanced by keystrokes only (the gestures that compose), and
    /// reset to the downbeat by every phrase boundary, so a new phrase always
    /// opens on an accent.
    song_pulse: u8,
    /// Whether the keystroke now being designed is an ACCENT (the melody sang)
    /// or a GHOST. Written by [`Self::advance_song`] one step before the design
    /// that reads it, like [`Self::space_head`].
    song_accent: bool,
    /// A GHOST's offset in scale degrees from [`Self::walk`] — meaningless
    /// while `song_accent` is true.
    song_ghost: i8,
    /// MELODY NOTES SUNG since construction: incremented exactly where the
    /// phrase generator steps. Not read by the DSP — it is how a test tells an
    /// accent from a ghost without reaching into the bar itself.
    song_notes: u32,
    /// The length multiplier the CURRENT palette dispatch spawns through —
    /// 1.0 for everything except a ghost keystroke ([`SONG_GHOST_FEEL`]).
    /// Set and cleared around the one dispatch that uses it, so every other
    /// spawn (bed grains, the bonk, the riff, the kind-level voices) sees the
    /// exact 1.0 that keeps `spawn`'s multiply-free path.
    song_feel: f32,
    /// The melody's current TONE — the last trail event's inferred mood.
    /// Steers the scale table/transpose ([`tone_tables`]), the walk shaping,
    /// and the spawn-time feel ([`tone_feel`]). Follows the event stream the
    /// same way `bed_style` does (set even for governor-thinned events, so
    /// the bed's grains stay in the melody's current constitution); the bonk
    /// neither reads nor writes it.
    tone: Tone,
    /// Master duck envelope 0..1: snapped to 1 when a bonk is admitted,
    /// exponential decay (τ [`BONK_DUCK_TAU`]) per sample rendered. Applied
    /// as `1 − BONK_DUCK_DEPTH · duck` to every non-exempt voice and the bed
    /// — one multiply per frame, exactly 1.0 while at rest.
    duck: f32,
    /// SING-ALONG duck envelope 0..1: snapped to 1 when a
    /// [`CelebrationGesture::RiffBar`] is admitted, HELD at 1 for the bar's
    /// length (`sing_hold`, so the riff replaces the melody without pumping
    /// between bars), then exponential handback (τ [`SING_DUCK_TAU`]) once
    /// bars stop — the audio wind-down. Applied beside `duck` as one more
    /// `1 − DEPTH · sing` factor: exactly ×1.0 while at rest, so every
    /// pinned path (v056 references, brrrring, bonk) is left exactly ×1.0 —
    /// this factor introduces no drift of its own (see `V056_TOLERANCE` for
    /// the drift the transcendental rewrite does introduce).
    sing: f32,
    /// THE SONG'S KEY, borrowed by the TYPING (owner, 2026-08-04: "more musical
    /// in kitty rainbow").
    ///
    /// The held-key sing-along transposes its authored riff by a pentatonic root
    /// offset picked from the held character (`kitty_sing::KittySing::key`), but
    /// the typed melody had no notion of a key at all: it kept walking its own
    /// lattice at its own anchor while the riff played in a different one. The
    /// two were only ACCIDENTALLY compatible — the anchors sit a near-just fourth
    /// apart — and nothing bound a typed note to the chord under it, so the
    /// typing was simply ducked out of the way rather than made to agree.
    ///
    /// Latched by [`TrailSynth::latch_song_key`] from every riff bar,
    /// through the same `celebration_root(sig)` the riff itself transposes by,
    /// and released with the sing duck, so it is exactly as live as the song is:
    /// while the cat sings, what you type is IN THE SAME KEY and reads as
    /// counterpoint; the moment the song ends the typing is back on the neutral
    /// lattice, note for note as before.
    song_key: i8,
    /// Seconds of full sing-duck hold remaining (block-rate countdown).
    sing_hold: f32,
    /// The signature of the last admitted riff bar — the SWITCH DAMP's memory.
    /// NOT per-key musical state: it never shapes a bar (the bar design stays
    /// a pure function of the payload — `same_key_stability` proves it), it
    /// only names which key's tails are stale when the next bar's `sig`
    /// differs, so `design_celebration` can damp them before the new verse.
    /// Deliberately never cleared: a stale memory can only damp CELEBRATION
    /// voices, and once a song has wound down there are none left to damp.
    last_riff_sig: Option<u32>,
    /// VOICE STEALS since construction — a pure diagnostic counter for the
    /// audition benches (`keyboard_song_ab`), never read by the DSP. A steal
    /// is the pool running out: [`Self::claim`] cut a live voice short, which
    /// is audible as a clipped tail, so a bench that reports a nonzero count
    /// is reporting a real mix defect rather than a statistic.
    steals: u32,
    /// DC blockers (one-pole highpass ~20 Hz) per channel.
    dc_x_l: f32,
    dc_y_l: f32,
    dc_x_r: f32,
    dc_y_r: f32,
}

/// Master output scale. Sized so a single default-volume (0.4) Typed event
/// peaks on the ladder floor, ~−21 dBFS — clearly audible in a quiet room at
/// normal system volume, far below alert/bell level. (The per-palette voice
/// designs differ by ~8 dB for the SAME gesture; [`palette_trim`], not this
/// constant, is what flattens them onto that floor.)
///
/// Tune delivered loudness HERE rather than in the kind gains or the rate
/// governor: those are duplicated VERBATIM in the `v056_reference` oracle,
/// while this const is the one the oracle SHARES — so both byte-identity pins
/// multiply by the same value and still hold.
const MASTER: f32 = 2.0;

/// Governor: sustained admission gap for discrete voices, per event kind
/// pressure. ~45 ms ⇒ at most ~22 voices/s even under key repeat.
const MIN_GAP: f32 = 0.045;

/// PER-PALETTE LEVEL TRIM — the one knob that makes the loudness ladder a
/// property of the GESTURE instead of the LOOK. Each palette's voice design has
/// its own intrinsic level (a Mech thock is ~8 dB hotter than a Beam blip at
/// the same gain); untrimmed, every style has its OWN ladder.
///
/// Each value is fitted so that style's `Typed` peaks at the ladder FLOOR
/// (-21.0 dBFS at gain 0.4 / heat 0.5, 24-seed mean) — measured by rendering,
/// not asserted. Residual spread across the ten palettes: 0.2 dB.
///
/// Applied at the palette dispatch ONLY, so the gestures designed BEFORE that
/// dispatch (Kill, Glide, Sweep, Land, Bonk, the riff) are untouched — a
/// structural boundary, not a claim that they are uniform: Kill's swoosh band
/// is style-tinted (Water 900/250, Fire 1400/300, Sparkle 2600/700, Comet
/// 1200/280, else 1600/350, plus its own Mech branch), leaving it ~3 dB of
/// residual spread. Glide/Sweep/Land/Bonk/riff genuinely are flat.
fn palette_trim(voice: SoundVoice, style: GlowStyle) -> f32 {
    match voice {
        SoundVoice::Mech => 0.68,
        // A STANDALONE style voice is the style's palette under a different
        // look — the same fitted number, by DELEGATION, so the nine trims
        // below stay the single source (no second copy to drift).
        SoundVoice::Of(s) => palette_trim(SoundVoice::Style, s),
        // The three sound-only voices — each fitted 2026-08-17 on `mix_meter
        // --voice <name>` exactly like the glass bell (isolated PEAK at the
        // host default volume, delta-fitted from a seed trim, never the
        // meter's ×0.40 "gain 1.0" suggestion line), and RMS/crest watched
        // on `typing_voice_ab --all` (scenario `a-neutral`) per the lesson
        // on `RainbowKittyPalette::design`:
        //   typewriter  -20.99 @vol 0.40 through 0.5116 — RMS -40.6 dB,
        //               crest 23.1 dB, centroid 1606 Hz, energy over 2 kHz
        //               0.43. The crest is the identity: a 13 ms paper
        //               clack IS transient (the shipped crackle sits at
        //               20.7), and the window's Enter ding lifts the peak;
        //               it is measured, not a fitting artifact.
        //   marimba     -21.01 @vol 0.40 through 0.9326 — RMS -30.3, crest
        //               11.3, centroid 485 Hz: the yarn mallet's soft strike
        //               on a ringing bar, RMS inside the pitched pack's
        //               -27..-32 band (ice chime -27.4, warm pluck -31.6).
        //   felt        -21.01 @vol 0.40 through 0.7385 — RMS -30.2, crest
        //               12.2, centroid 439 Hz, energy over 2 kHz 0.000: the
        //               darkest voice in the roster, as designed.
        // Jump lands +3.0 / +3.3 / +3.0 dB over Typed (TIER 3), Backspace
        // -2.6 / -1.3 / -2.6 (TIER 1, "a shade under").
        SoundVoice::Typewriter => 0.5116,
        SoundVoice::Marimba => 0.9326,
        SoundVoice::Felt => 0.7385,
        SoundVoice::Style => match style {
            GlowStyle::Lumen | GlowStyle::Custom => 0.95,
            GlowStyle::Phaser => 0.94,
            // RE-FITTED 2026-08-16 with THE GLASS KEY, on the SAME quantity
            // the ladder is written in: `mix_meter`'s isolated PEAK at the
            // host default volume. Fitted by DELTA against the previous
            // verified fit rather than the meter's own suggestion line (that
            // line targets gain 1.0 and prints a ×0.39 "correction" even for
            // a correctly fitted palette — do not follow it): the tingly
            // bell had verified -20.99 @vol 0.40 through 0.8498, the tuned
            // glass bell reads -20.69 through the same trim — 0.31 dB hot
            // from the mallet transient outweighing the softer sine body —
            // so 0.8498 × 10^(-0.31/20) lands the bell back on the -21.0
            // floor (verified: -21.00 @vol 0.40; RMS and crest watched at
            // the fit per the lesson above: a-neutral RMS -37.2 dB, crest
            // 17.3 dB — the struck-glass sharpness is the designed identity,
            // measured, not a fitting artifact).
            //
            // The trim is deliberately NOT the knob for brightness or crest.
            // A trim is one scalar: it holds the LADDER; the sparkle lives in
            // the VOICE (register, partials, envelope, tinks) and is measured
            // there — see `RainbowKittyPalette::design`.
            GlowStyle::RainbowKitty => 0.8200,
            GlowStyle::Sparkle => 1.27,
            GlowStyle::Fire => 0.88,
            GlowStyle::Laser => 0.77,
            GlowStyle::Beam => 1.70,
            GlowStyle::Water => 1.01,
            GlowStyle::Comet => 1.64,
        },
    }
}

/// THE KILL SWOOSH'S BAND `(hi, lo)` — the register the line-kill's soft
/// noise falls through. A kill is designed kind-level (before palette
/// dispatch) so it sounds like a swoosh in every palette, but it is TINTED by
/// the speaking palette: the one voice-first decision the kind-level path
/// makes, and — before the picker — the ONE non-exhaustive voice test in the
/// synth (`if voice == Mech {…} else { match style {…} }`), which would have
/// let a new voice silently fall into the look's tint. Exhaustive now: the
/// compiler is the registry check.
///
/// `Style` resolves exactly the pre-picker table (byte-pinned by the
/// `v056_reference` oracle for all nine looks); a standalone style voice
/// takes its OWN style's tint by delegation, so `Of(Water)` under a Lumen
/// look still falls through water.
fn kill_swoosh_band(voice: SoundVoice, style: GlowStyle) -> (f32, f32) {
    match voice {
        // The mech kill: a dull sweep down through the case register — a
        // hand brushing keys, not a musical fall.
        SoundVoice::Mech => (1000.0, 220.0),
        // The carriage lever thrown: dry paper-and-metal, brighter than the
        // keyboard's case but well under any glass.
        SoundVoice::Typewriter => (2200.0, 500.0),
        // A yarn mallet swept along the bars — the wooden mid.
        SoundVoice::Marimba => (1300.0, 300.0),
        // The felt's dampers brushed down the strings — the darkest fall.
        // Meters -21.6 dBFS, 0.8 dB under Water's own kill (-20.8): inside
        // the ~3 dB residual spread the ladder doc grants the swoosh's tint.
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
            inv_sr: 1.0 / sample_rate.max(8_000.0),
            rng: seed | 1,
            voices: [Voice::default(); MAX_VOICES],
            bed: Bed::default(),
            bed_style: GlowStyle::Lumen,
            bed_voice: SoundVoice::Style,
            bed_variant: BedVariant::default(),
            rate: 0.0,
            since_voice: 1.0,
            since_event: 1.0,
            since_erase: 1.0,
            erase_run: 0,
            space_run: false,
            space_head: false,
            walk: 2,
            // Phrase state opens EMPTY: `phrase_len == 0` (with `phrase_pos ==
            // 0`) makes the very first trail note a phrase boundary, so the
            // melody starts by cadencing onto the tonic and drawing its first
            // motif. The bonk-moves-nothing proof pushes no trail note, so
            // `walk` stays at its documented init of 2.
            motif: [0; 4],
            phrase_pos: 0,
            phrase_len: 0,
            phrase_parity: false,
            phrase_step: 0,
            phrase_home: 2,
            // The bar opens on its DOWNBEAT, so the very first keystroke of a
            // session is an accent — which is also what keeps the loudness
            // ladder's isolated-keystroke pins measuring the accent level.
            song_pulse: 0,
            song_accent: true,
            song_ghost: 0,
            song_notes: 0,
            song_feel: 1.0,
            tone: Tone::Technical,
            duck: 0.0,
            sing: 0.0,
            song_key: 0,
            sing_hold: 0.0,
            last_riff_sig: None,
            steals: 0,
            dc_x_l: 0.0,
            dc_y_l: 0.0,
            dc_x_r: 0.0,
            dc_y_r: 0.0,
        }
    }

    /// xorshift32 — deterministic, allocation-free. Uniform in [0,1).
    fn rnd(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x >> 8) as f32 * (1.0 / 16_777_216.0)
    }

    /// Uniform in [lo, hi).
    fn rnd_in(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.rnd()
    }

    /// True while any voice is live or the bed is audibly breathing out.
    /// When false, [`render`](Self::render) writes exact zeros — pause away.
    pub fn is_quiet(&self) -> bool {
        // 1e-3 pre-gain bed level is far below audibility (the style
        // coefficients scale it down another 20+ dB); snapping to exact zero
        // there is inaudible and lets the host pause quickly.
        self.voices.iter().all(|v| !v.on)
            && self.bed.level < 1e-3
            && self.bed.energy < 1e-3
            // …AND THE DC BLOCKER HAS SETTLED. Without this the early-out
            // TRUNCATED the blocker's own tail: a synth that reached silence
            // stopped mid-decay and jumped to exact zeros, while an otherwise
            // identical stream that stayed awake for any reason (a live bed,
            // say) rendered that tail out. Two runs of one script could then
            // differ by ~10 sixteen-bit steps purely on whether they crossed
            // the quiet threshold — which is what `mech_bed_is_structurally_
            // silent` measures. `render` snaps the state to EXACT zero once
            // the tail is below -120 dBFS, so this test is a real equality
            // and cannot wait forever.
            && self.dc_y_l == 0.0
            && self.dc_y_r == 0.0
    }

    /// Number of live voices (test/diagnostic hook).
    pub fn live_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.on).count()
    }

    /// VOICE STEALS since construction (test/diagnostic hook — see the field).
    /// A nonzero count on a realistic script means the 28-voice pool ran dry
    /// and a live tail was cut, so the benches report it beside the peak.
    #[must_use]
    pub fn steals(&self) -> u32 {
        self.steals
    }

    /// Diagnostic: (bed energy, bed level) — demo/tuning hook.
    pub fn debug_bed(&self) -> (f32, f32) {
        (self.bed.energy, self.bed.level)
    }

    /// Select which AMBIENT-BED TOURNAMENT candidate the bed mixer renders —
    /// the audition seam (`examples/bed_audition.rs`). Intended to be set
    /// once, right after construction: candidates share the [`Bed`] state
    /// fields, so a mid-stream switch is well-defined (worst case a brief
    /// portamento from the previous variant's frequencies) but is nothing
    /// the tournament measures. Hosts never call this — the default,
    /// [`BedVariant::Current`], renders the shipping palette beds through
    /// their exact pre-tournament path.
    pub fn set_bed_variant(&mut self, variant: BedVariant) {
        self.bed_variant = variant;
    }

    // -- event intake -------------------------------------------------------

    /// Push one gesture. Applies the rate governor, feeds the bed (trail
    /// gestures only), and (if admitted) designs the gesture's voice(s).
    /// O(MAX_VOICES), no alloc.
    pub fn push(&mut self, ev: SoundEvent) {
        // Defense in depth at the pure synth boundary: TOML accepts NaN/Inf and
        // hosts are fallible. One non-finite scalar would poison the persistent
        // bed/voice state and every later sample. Reject it before any mutation;
        // clamp finite out-of-range inputs into the documented domain. Every
        // scalar `SoundEvent` carries must appear in this filter.
        if !ev.pan.is_finite()
            || !ev.heat.is_finite()
            || !ev.hue.is_finite()
            || !ev.gain.is_finite()
        {
            return;
        }
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
        // The typing PAUSE since the previous event, captured BEFORE the
        // governor resets `since_event` below — the phrase generator reads it
        // as a natural phrase boundary (a comma is a gap; see `advance_melody`).
        let pause = self.since_event;
        // Governor bookkeeping: exponential rate estimate over ~0.6 s. EVERY
        // source pays into it — a bonk raises the rate, so the melody notes
        // right after it are governor-ducked too (the same direction as the
        // explicit duck envelope; both make room).
        self.rate = self.rate * (-self.since_event / 0.6).exp() + 1.0;
        self.since_event = 0.0;
        // Bed: every TRAIL event feeds energy, admitted or not — this is what
        // makes fast typing MELT into the texture instead of stacking voices.
        // Word gestures deliberately skip the whole bed feed (style hand-off
        // included): a bonk is punctuation and must not swell the ambience.
        if let SoundGesture::Trail(kind) = ev.kind {
            // Style + tone follow the trail stream UNCONDITIONALLY (thinned
            // and bed-off events included): the melody reads `tone` for every
            // note, and a bed re-enabled mid-stream must wake in the current
            // style/constitution, not whichever one last carried `bed`.
            self.bed_style = ev.style;
            self.bed_voice = ev.voice;
            self.tone = ev.tone;
            // THE WHITESPACE RUN follows the TEXT, not the audio: a space the
            // governor thins still ends the run it belongs to, so the next
            // space is not mistaken for a second head. A bare Shift is not a
            // character and leaves the run untouched.
            match kind {
                SoundKind::Space => {
                    self.space_head = !self.space_run;
                    self.space_run = true;
                }
                SoundKind::Shift => {}
                _ => self.space_run = false,
            }
            // The ENERGY feed is what `ev.bed` gates (the `trail_sound_bed`
            // setting, default OFF): un-fed, the bed's level never leaves its
            // exact-zero floor, so the bed mixer emits zero samples and spawns
            // zero grains — structurally, not via a zero gain. Flipping the
            // setting OFF mid-breath simply starves the feed: the live bed
            // exhales through its normal ~1 s decay and snaps to exact zero.
            if ev.bed {
                let kick = match kind {
                    SoundKind::Jump | SoundKind::Kill | SoundKind::Land => 0.5,
                    // A word kill is per-command like the line kill, at word
                    // scale.
                    SoundKind::KillWord => 0.4,
                    // A space is typing cadence exactly like a letter.
                    SoundKind::Typed | SoundKind::Backspace | SoundKind::Space => 0.3,
                    // Cursor scrubbing feeds the bed at a whisper — presence,
                    // not a swell. The shift lift joins it: a modifier is
                    // presence too, never a swell of its own.
                    // THE CLOUD'S PUFF feeds it NOTHING: it is the accompaniment
                    // of a Backspace that already kicked the bed on this very
                    // gesture, and counting it again would let a delete swell the
                    // weather twice as hard as a keystroke.
                    SoundKind::Navigation
                    | SoundKind::Glide { .. }
                    | SoundKind::Sweep { .. }
                    | SoundKind::Shift => 0.12,
                    SoundKind::Poof => 0.0,
                };
                self.bed.energy = (self.bed.energy + kick).min(1.0);
                self.bed.gain += (ev.gain - self.bed.gain) * 0.3;
            }
        }

        // Ducking: louder alone, softer in a flood. 1/sqrt keeps perceived
        // per-second loudness roughly flat as rate climbs.
        let duck = 1.0 / (1.0 + 0.55 * self.rate).sqrt();
        // Min-gap thinning for the discrete layer. Jumps always speak (they
        // are rare and are the punctuation of the phrase); the bonk outranks
        // even them — the wrong note may never be thinned into silence.
        let bypass = matches!(
            ev.kind,
            SoundGesture::Trail(SoundKind::Jump)
                // A cursor SWEEP is one event carrying a whole pre-delayed run;
                // thinning it would silence the run mid-flight, so it bypasses
                // the gap like a Jump (its own inter-note delays rate-limit it).
                // A Glide is deliberately NOT here — it stays gap-thinned like a
                // keystroke, so a held arrow can't machine-gun.
                | SoundGesture::Trail(SoundKind::Sweep { .. })
              // A LANDING is punctuation you can SEE: the starburst is on
              // glass whether or not the gap would have thinned the note,
              // so thinning it would silence a VISIBLE celebration.
                | SoundGesture::Trail(SoundKind::Land)
              // THE CLOUD'S PUFF, on the LANDING's exact reasoning: it is the
              // voice of a puff that is ON GLASS, and the keystroke bell it
              // accompanies is drained from the SAME frame's cue list one slot
              // ahead of it — so under the gap it would be thinned into silence
              // every single time, not occasionally. Its rate limit is the
              // VISUAL one (`cursor_glow`'s POOF_MIN_GAP): no cloud, no puff.
                | SoundGesture::Trail(SoundKind::Poof)
                | SoundGesture::Words(WordGesture::Bonk)
                // A riff bar is ONE event per ~1.6 s carrying the whole
                // phrase — thinning it would silence entire bars, so it
                // outranks the gap exactly like the other punctuation.
                | SoundGesture::Celebration(_)
        );
        // THE ERASE GATE. A deletion is thinned against OTHER DELETIONS
        // ([`ERASE_MIN_GAP`]) and against nothing else — see the constant for
        // why the shared keystroke gap cannot serve both.
        let erase = ev.kind == SoundGesture::Trail(SoundKind::Backspace);
        let admit = if erase {
            self.since_erase >= ERASE_MIN_GAP
        } else {
            bypass || self.since_voice >= MIN_GAP
        };
        if !admit {
            return;
        }
        // The SHIFT lift is admitted through the gap like everything else,
        // but it does not CLAIM the beat: shift-then-capital lands inside
        // one MIN_GAP at speed, and a grace note that owned the gap would
        // thin the very keystroke it announces. The ERASE POOF is out for the
        // mirror-image reason — it is gated on its own clock, so claiming the
        // shared beat would let a correction thin the letter typed after it.
        // THE CLOUD'S PUFF is out too, on the accompaniment reading of the
        // same rule: where a grace note must not thin the keystroke it
        // announces, an accompaniment must not thin the keystroke it FOLLOWS —
        // the next key of a held Backspace run lands well inside one MIN_GAP
        // of the puff, and a puff that claimed the beat would eat the poof.
        // Every other admission resets the clock exactly as before.
        if erase {
            // HELD-RUN bookkeeping, BEFORE the clock resets: the gap back to
            // the previous ADMITTED deletion is what distinguishes a held
            // key's auto-repeat from deliberate single corrections (see
            // [`HELD_ERASE_RUN_WINDOW`]).
            self.erase_run = if self.since_erase <= HELD_ERASE_RUN_WINDOW {
                self.erase_run.saturating_add(1)
            } else {
                1
            };
            self.since_erase = 0.0;
        } else if !matches!(
            ev.kind,
            SoundGesture::Trail(SoundKind::Shift) | SoundGesture::Trail(SoundKind::Poof)
        ) {
            self.since_voice = 0.0;
        }
        // Any authored admission ends a held delete run's story: expire its
        // PENDING release shimmer (the erase itself re-schedules a fresh one
        // in its designer, which is exactly the retrigger that keeps only the
        // run's LAST shimmer alive). The cloud's Puff and the Shift lift are
        // accompaniments and touch nothing — the puff in particular arrives a
        // frame BEHIND the very deletion whose shimmer it must not kill.
        if !matches!(
            ev.kind,
            SoundGesture::Trail(SoundKind::Shift) | SoundGesture::Trail(SoundKind::Poof)
        ) {
            self.damp_pending_shimmer();
        }
        match ev.kind {
            SoundGesture::Trail(kind) => {
                // A KEYSTROKE / edit / jump advances the phrase-aware melody
                // (the tune). A cursor GESTURE (Glide/Sweep) plays IN the
                // melody without stepping it — the cursor sings the current
                // degree, it does not compose; the SPACE grounds it and the
                // SHIFT anticipates it, and neither composes either. Draw
                // counts differ per phrase (rng only at phrase boundaries),
                // which is fine: determinism is per (events, seed, tone)
                // script.
                if !matches!(
                    kind,
                    SoundKind::Glide { .. }
                        | SoundKind::Sweep { .. }
                        | SoundKind::Land
                        | SoundKind::Space
                        | SoundKind::Shift
                        // A DELETION UNDOES A CHARACTER. Advancing the tune
                        // past the note whose letter just vanished would leave
                        // the song ahead of the text — type five, delete five,
                        // type five again and the melody has walked ten notes
                        // for five characters. The poof is unpitched anyway:
                        // stepping the song would move a note nothing sounds.
                        | SoundKind::Backspace
                        // A WORD KILL is a deletion too — the same law at
                        // word scale, and its poof is just as unpitched.
                        | SoundKind::KillWord
                        // THE CLOUD'S PUFF is an accompaniment, not a note of
                        // the sentence — and it rides a deletion, which just
                        // declined to step for exactly the reason above.
                        | SoundKind::Poof
                ) {
                    self.advance_song(kind, pause);
                }
                self.design_trail(ev, kind, duck, pause);
            }
            SoundGesture::Words(WordGesture::Bonk) => self.design_bonk(ev, duck),
            SoundGesture::Celebration(CelebrationGesture::RiffBar { bar, sig }) => {
                self.latch_song_key(sig);
                self.design_celebration(ev, bar, sig);
            }
        }
    }

    /// Place one composing gesture in THE BAR ([`SONG_PULSE`]) and, on an
    /// accent, step the phrase generator under it. This is the seam that turns
    /// a note-per-keystroke stream into a song: see the PULSE section for the
    /// measurement that motivated it.
    ///
    /// Only a KEYSTROKE can be a ghost. An Enter, a kill or a jump is
    /// punctuation — it lands, always, at full level on a fresh melody note —
    /// and an Enter additionally resets the bar to its downbeat so a new
    /// phrase opens on an accent. A long typing PAUSE does the same, so the
    /// note after a think is always the tune and never the accompaniment.
    fn advance_song(&mut self, kind: SoundKind, pause: f32) {
        let boundary = matches!(kind, SoundKind::Jump) || pause > PHRASE_PAUSE_S;
        if boundary {
            self.song_pulse = 0;
        }
        // Punctuation never ghosts; and after a boundary the bar is at its
        // downbeat, which IS an accent — so both paths agree by construction
        // rather than by a second rule.
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

    /// Advance the PHRASE-AWARE MELODY by one note, setting [`Self::walk`] to
    /// the new degree. Deterministic — `rnd()` is drawn only at phrase
    /// boundaries (a fixed 1 + 4 draws), never per note, so the per-note step
    /// is a pure function of phrase state and the whole generator replays
    /// bit-for-bit per `(events, seed, tone)`. `pause` is the typing gap since
    /// the previous event (captured before the governor reset it).
    ///
    /// The tune-making devices are layered onto the register — a
    /// repeat-and-vary MOTIF cell, a raised-cosine contour ARC,
    /// CALL-AND-RESPONSE by phrase parity, a leap-and-recover at the peak, and
    /// a CADENCE onto the tonic at phrase ends (Enter or a pause) — with pitch
    /// still flowing through [`Self::melody_hz`], so consonance and tone
    /// adaptation are inherited untouched.
    fn advance_melody(&mut self, kind: SoundKind, pause: f32) {
        let (lo, hi) = tone_register(self.tone);
        // PHRASE BOUNDARY: Enter (a Jump), a comma-length typing gap, or the
        // phrase simply running its length. On any of these the melody
        // CADENCES onto the nearest tonic (degree 0, or its octave 5 — both
        // the pentatonic root pitch class), then OPENS a fresh phrase from
        // there: a new length, a new motif, the call/response parity flipped.
        // This is what makes phrases LAND instead of drifting forever.
        let boundary = matches!(kind, SoundKind::Jump)
            || pause > PHRASE_PAUSE_S
            || self.phrase_pos >= self.phrase_len;
        if boundary {
            let tonic = if self.walk * 2 <= 5 { 0 } else { 5 };
            self.walk = tonic.clamp(lo, hi);
            self.phrase_home = self.walk;
            self.phrase_step = 0;
            self.phrase_pos = 0;
            self.phrase_parity ^= true;
            // A fresh phrase length (6..=8) and a fresh 4-step motif cell. The
            // draw COUNT is fixed (1 + 4) whatever the tone, so the rng stream
            // is phrase-periodic and the byte pins cross-check cleanly.
            self.phrase_len =
                PHRASE_MIN + (self.rnd() * ((PHRASE_MAX - PHRASE_MIN + 1) as f32)) as u8;
            let span = melody_span(self.tone);
            // THE MOTIF CELL — three drawn steps and a CLOSING one. Two
            // properties are what make the line a tune instead of a scatter,
            // and both were missing:
            // - EVERY STEP MOVES. The old draw included 0, and a zero delta
            //   plays the same pitch twice; worse, a ±1 delta cancels exactly
            //   against the arc's own ±1, so the two most common cases both
            //   produced a repeated note. Measured over a 12.5 s typing
            //   script: 54 % of consecutive notes were UNISONS — the melody's
            //   single loudest defect.
            // - THE CELL CLOSES. The fourth step is whatever returns the cell
            //   to where it began, so the second pass starts where the first
            //   did instead of the accumulator walking into the register bound
            //   (the other unison source: two saturated notes are one pitch,
            //   and a saturated motif is no longer periodic).
            // The draw COUNT is fixed (1 + 3) whatever the tone, so the rng
            // stream stays phrase-periodic and the byte pins cross-check.
            let mut sum = 0;
            for i in 0..3 {
                // 2·span NONZERO choices: −span..=−1 then 1..=span.
                let k = (self.rnd() * (2 * span) as f32) as i32;
                let d = if k < span { k - span } else { k - span + 1 };
                self.motif[i] = d as i8;
                sum += d;
            }
            // The closing leap, held inside one step more than the cell's own
            // span so the return home stays a consonant lattice interval (a
            // pentatonic sixth at span 1) rather than a lurch.
            self.motif[3] = (-sum).clamp(-(span + 1), span + 1) as i8;
            return;
        }
        // MOTIF STEP: the cell delta at this position drives the note. The cell
        // is inverted on ANSWER phrases (call/response), transposed up a degree
        // on its SECOND pass (repeat-and-vary), and given one bright LEAP at the
        // arc peak (leap-and-recover — the motif steps back on the next note).
        let idx = (self.phrase_pos % 4) as usize;
        let mut delta = i32::from(self.motif[idx]);
        if self.phrase_parity {
            delta = -delta;
        }
        if self.phrase_pos == self.phrase_len / 2 {
            delta += MELODY_LEAP;
        }
        self.phrase_step += delta;
        // The CONTOUR ARC (a lift of [`ARC_LIFT`] degrees through the middle of
        // the phrase, settling back for the cadence), the repeat-and-vary lift
        // on the cell's second pass, and the per-tone LEAN — all three fold
        // into the PITCH register, never the motif accumulator, so the cell's
        // delta pattern stays exactly periodic (the motif genuinely recurs)
        // while the line still has shape.
        let frac = f32::from(self.phrase_pos) / f32::from(self.phrase_len);
        let arc = ARC_LIFT * (core::f32::consts::PI * frac).sin().round() as i32;
        let vary = if self.phrase_pos >= 4 { MOTIF_VARY } else { 0 };
        self.phrase_pos += 1;
        self.walk = fold_register(
            self.phrase_home + self.phrase_step + arc + vary + melody_lean(self.tone),
            lo,
            hi,
        );
    }

    /// The melody's pitch lattice under the CURRENT tone: `base` scaled to
    /// `degree` of the tone's table, times the tone's transpose. Every
    /// palette draws its pitches through this rather than the free [`penta`];
    /// under [`Tone::Technical`]/[`Tone::Calm`] the table is [`PENTA`] and
    /// the transpose an exact ×1.0, so the result is bit-identical to
    /// [`penta`] (the `v056_reference` proofs run through this very path).
    /// The bonk deliberately keeps calling [`penta`]: its clash is anchored
    /// to the untransposed lattice and must stay byte-untouched.
    fn melody_hz(&self, base: f32, degree: i32) -> f32 {
        let (table, transpose) = tone_tables(self.tone);
        let oct = degree.div_euclid(5);
        let step = degree.rem_euclid(5) as usize;
        base * transpose * table[step] * (2.0_f32).powi(oct)
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
        // Nothing was free: `best` is a LIVE voice about to be cut short.
        self.steals = self.steals.saturating_add(1);
        best
    }

    /// Spawn one voice from a prototype: applies pan narrowing + equal-power
    /// law, resets runtime state, randomizes oscillator phases.
    #[allow(clippy::too_many_arguments)]
    fn spawn(&mut self, proto: Voice, gain: f32, pan: f32) {
        // The historical draw order — tremolo phase, then the three partials —
        // hoisted here so the pinned streams are byte-unmoved.
        let tw_ph = self.rnd();
        let ph = [self.rnd(), self.rnd(), self.rnd()];
        self.spawn_seeded(proto, gain, pan, tw_ph, ph);
    }

    /// [`Self::spawn`] with the oscillator phases HANDED IN rather than
    /// drawn — the seam a NOISE-ONLY kind-level layer rides (fixed zero
    /// phases: a silent partial has no phase to hear) so it consumes NO rng
    /// and the palette design behind it draws exactly the stream it always
    /// did. That is what keeps the felt lift-off from moving Sparkle's
    /// scatter, the tinks, or any other seed-replayed voice by a single
    /// draw.
    fn spawn_seeded(&mut self, proto: Voice, gain: f32, pan: f32, tw_ph: f32, ph: [f32; 3]) {
        let idx = self.claim();
        let p = (pan * 0.35).clamp(-0.6, 0.6); // never fully one-eared
        let a = (p + 1.0) * core::f32::consts::FRAC_PI_4;
        let mut v = proto;
        // Tone TEMPO-FEEL: one narrow multiplier on length, decay and
        // flourish spacing (arpeggio/droplet delays ARE the phrase's tempo).
        // Bonk voices are exempt via their `duck_exempt` mark — that path is
        // byte-pinned — and a feel of exactly 1.0 (Technical, Frustrated)
        // skips the multiplies entirely, keeping the neutral path
        // bit-identical to the pre-tone build.
        // …times the BAR's own length multiplier, which is 1.0 everywhere
        // except inside a ghost keystroke's palette dispatch (see
        // `SONG_GHOST_FEEL`). Both are exactly 1.0 on the pinned neutral path,
        // so the product is too and the multiplies below stay skipped.
        let feel = tone_feel(self.tone) * self.song_feel;
        if !v.duck_exempt && feel != 1.0 {
            v.dur *= feel;
            v.decay *= feel;
            v.delay *= feel;
        }
        v.on = true;
        v.t = -v.delay; // delay is modelled as negative onset time
        v.gl = gain * a.cos();
        v.gr = gain * a.sin();
        v.tw_ph = tw_ph;
        // GEOMETRIC STEPS. Every exponential in the sample loop is evaluated
        // from `v.t`, which advances by a constant `dt`, so each one is
        // `E_{n+1} = E_n · e^(-dt/τ)`: one multiply per sample instead of a
        // scalar libm call. The steps depend only on the voice's time
        // constants and on `inv_sr` (written once in `new`), so they are
        // computable here — AFTER the tone-feel multiplies above, which move
        // `decay`. `env_run` false means "not yet seeded"; the seed happens
        // at the first sounding sample, not here.
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
        }
        v.lp = 0.0;
        v.n_lp = 0.0;
        v.n_bp = 0.0;
        // Render-loop invariants, resolved here instead of 48 000 times a
        // second. Kept AFTER the tone-feel multiplies above on principle
        // (they move `dur`/`decay`/`delay` — none of the inputs below, but
        // "derived fields come last" is the invariant worth keeping whole).
        // None of these inputs can move during the voice's life: the render
        // loop never writes `lp_cut`/`n_q`/`n_f0`/`n_glide`/`n_lvl`.
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
    }

    // -- kind-level + per-palette sound design ------------------------------

    /// Design and spawn the voice(s) for one admitted TRAIL gesture: the
    /// kind-level shaping shared by all styles, the style-agnostic Kill
    /// swoosh, then the per-palette dispatch (each [`Palette`] implementor IS
    /// its own sound designer).
    fn design_trail(&mut self, ev: SoundEvent, kind: SoundKind, duck: f32, pause: f32) {
        // Heat warms level slightly (+45 % at full blaze) — presence, not
        // a volume ride.
        let g = ev.gain * duck * (0.55 + 0.45 * ev.heat);
        // Kind-level shaping shared by all styles.
        let kg = match kind {
            // TIER 1 — per CHARACTER: the floor.
            SoundKind::Typed => TYPED_KIND_GAIN,
            SoundKind::Backspace => BACKSPACE_KIND_GAIN,
            SoundKind::Space => SPACE_KIND_GAIN,
            SoundKind::Glide { .. } => GLIDE_KIND_GAIN,
            // UNDER the floor — intent, not authorship.
            SoundKind::Shift => SHIFT_KIND_GAIN,
            // …and under THAT — accompaniment, not authorship.
            SoundKind::Poof => POOF_KIND_GAIN,
            // TIER 2 — per GESTURE.
            SoundKind::Navigation => NAVIGATION_KIND_GAIN,
            SoundKind::Sweep { .. } => SWEEP_KIND_GAIN,
            // TIER 2.5 — per WORD: the poof one size up, under the line kill.
            SoundKind::KillWord => KILLWORD_KIND_GAIN,
            // TIER 3 — per LINE / COMMAND.
            SoundKind::Kill => KILL_KIND_GAIN,
            SoundKind::Jump => JUMP_KIND_GAIN,
            // TIER 4 — the rare spectacle you can SEE.
            SoundKind::Land => LAND_KIND_GAIN,
        };
        let g = g * kg;
        // The column only NUDGES the melody ±1: the phrase motif owns the
        // pitch, so a long line drifts by at most a single scale-step instead
        // of the column swamping the tune.
        let col_off = (ev.pan).round() as i32;
        // IN THE SONG'S KEY WHILE THE SONG IS PLAYING (see `song_key`). It is
        // zero whenever the cat is not singing, so the ordinary typed melody is
        // untouched and every neutral-path proof — including the v0.56 byte-pin
        // oracle, which has no such field — stays exact.
        //
        // THE FAMILY OFFSET rides here, once, for every palette: a deletion is
        // a keystroke one lattice step down, a character move is one step in
        // the travel direction, a word move starts on the degree and strides
        // out (see `gesture_shape`). Palettes no longer spell their own
        // deletion interval — that is what made the edit vocabulary incoherent.
        let shape = gesture_shape(kind);
        // THE GHOST OFFSET rides here too, and ONLY for a keystroke: the bar
        // is the TYPING's rhythm. A cursor motion, a landing or a jump plays
        // the melody's own degree whatever the bar is doing — they accompany
        // the tune, they are not played by it.
        let ghosting = kind == SoundKind::Typed && !self.song_accent;
        let deg = self.walk
            + col_off
            + i32::from(self.song_key)
            + shape.offset
            + if ghosting { i32::from(self.song_ghost) } else { 0 };

        // CURSOR MOVEMENT (Glide/Sweep) is a style-agnostic, IN-KEY gesture
        // designed once here (like Kill/Bonk), before palette dispatch: it
        // plays relative to the melody's current degree on the active tone, so
        // scrubbing sits inside the tune. `g` already carries its soft
        // kind-gain, the heat warmth, and the flood duck.
        if matches!(kind, SoundKind::Glide { .. } | SoundKind::Sweep { .. }) {
            self.design_cursor(&ev, shape, deg, g);
            return;
        }

        // THE LANDING — style-agnostic like Kill/Bonk and designed HERE, before
        // palette dispatch, so a starburst sounds like a starburst in every
        // trail style. `g` already carries LAND_KIND_GAIN, the heat warmth and
        // the flood duck.
        if kind == SoundKind::Land {
            self.design_land(&ev, g);
            return;
        }

        // THE ERASE POOF — style-agnostic like Kill/Land and, unlike the old
        // deletion, TERMINAL: it returns before the palette dispatch below, so
        // no style can lay a pitched note under the puff. That return is the
        // whole fix; the felt under-layers this replaces did not have it, and
        // the palette's own inverted keystroke stayed the dominant voice.
        if kind == SoundKind::Backspace {
            self.design_erase_poof(&ev, g);
            return;
        }

        // THE WORD POOF — the deletion family at word scale, terminal before
        // palette dispatch exactly like the character's poof and for the same
        // reason: no style may lay a pitched note under a puff of air.
        if kind == SoundKind::KillWord {
            self.design_word_poof(&ev, g);
            return;
        }

        // THE DOWNBEAT and THE LIFT — style-agnostic like Kill/Land, designed
        // here so a word boundary and a shift lift sound like themselves in
        // every palette, each borrowing the speaking palette's register
        // through `anchor_hz` exactly as the movement family does.
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
            // 2.6, not the ~0.5 a tonal voice would take: this voice is PURE
            // band-passed noise (no partials), whose peak sits ~17 dB under a
            // tonal voice at the same gain, so the compensation belongs in the
            // VOICE. Not in `KILL_KIND_GAIN`, which is a PRIORITY statement
            // and must stay under `BONK_KIND_GAIN`.
            self.spawn(v, g * 2.6, ev.pan);
            return;
        }

        // THE CLOUD'S PUFF — the erase poof's air, designed here beside the
        // kill swoosh and style-agnostic for the same reason: a puff of air
        // sounds like a puff of air in every palette.
        if kind == SoundKind::Poof {
            self.design_poof(&ev, g);
            return;
        }

        // The palette's own level trim lands THIS style's keystroke on the
        // ladder floor, so the kind-gain tiers above mean the same number of
        // dB in every style.
        let g = g * palette_trim(ev.voice, ev.style);
        // A GHOST is quieter and SHORTER than the accent it accompanies. The
        // length rides `spawn`'s existing feel multiply; it is set and cleared
        // around this one dispatch, so every other spawn in the engine — bed
        // grains, the bonk, the riff, the kind-level voices — still sees the
        // exact 1.0 that keeps the multiply-free path (and therefore the byte
        // pins) untouched.
        let g = if ghosting { g * SONG_GHOST_LEVEL } else { g };
        self.song_feel = if ghosting { SONG_GHOST_FEEL } else { 1.0 };
        palette_for(ev.voice, ev.style).design(self, &ev, kind, g, deg, col_off);
        self.song_feel = 1.0;
    }

    /// The CURSOR-MOVEMENT gestures — the family's MOTION half, designed once
    /// here from the SAME [`gesture_shape`] rule the typing and the deletion
    /// obey, so a word motion is audibly a character motion at word scale and
    /// both are audibly relatives of the keystroke:
    ///
    /// - REGISTER — pitched through [`Self::melody_hz`] at the palette's own
    ///   melodic base ([`Palette::anchor_hz`]), the register its keystroke
    ///   lives in, so scrubbing sits in the typing's octave instead of a fixed
    ///   330 Hz of its own.
    /// - DEGREE — `deg` already carries the melody's current degree, the
    ///   column nudge, the borrowed song key and the shape's offset, so a
    ///   cursor note is in the tune's key exactly like a typed one.
    /// - CONTOUR — [`gesture_bend`] on `dir`: forward moves lean up onto their
    ///   note, backward moves lean down onto it. Same articulation as a
    ///   keystroke and its deletion.
    /// - SCALE — `shape.notes` tones striding `shape.step` degrees, pre-delayed
    ///   [`CURSOR_SWEEP_STEP_S`] apart (the arpeggio idiom — delayed voices, no
    ///   scheduler) and tapering, so a WORD move is one CHARACTER move walked
    ///   out. The FIRST note has `delay = 0`, so any move speaks in the first
    ///   post-cue synth buffer.
    ///
    /// The voice stays a soft sine pluck rather than borrowing the palette's
    /// full timbre: motion is not authorship, and the loudness ladder puts the
    /// whole movement family under the typing floor (TIER 0).
    ///
    /// `dir` rides in the kind (an enum payload, nothing for the non-finite
    /// filter to check), so [`SoundEvent`] carries no new scalar.
    fn design_cursor(&mut self, ev: &SoundEvent, shape: GestureShape, deg: i32, g: f32) {
        let anchor = palette_for(ev.voice, ev.style).anchor_hz();
        for i in 0..shape.notes {
            let f = self.melody_hz(anchor, deg + shape.step * i as i32);
            let (f0, f1) = gesture_bend(f, shape.dir);
            let v = Voice {
                // First note immediate; the rest trail behind at a steady
                // spacing — the run's own rate limit, so bypassing min-gap
                // can't machine-gun.
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
            // A single-note move keeps the old character level; a run tapers
            // across itself so it reads as a gesture that LANDS rather than a
            // flat block of tones.
            let level = if shape.notes == 1 {
                0.5
            } else {
                0.45 * (1.0 - 0.15 * i as f32)
            };
            self.spawn(v, g * level, ev.pan);
        }
    }

    /// The cursor-LANDING star chime — the aural twin of the rainbow kitty fast-jump
    /// STARBURST. Three IN-KEY star tones stepping [`LAND_STAR_DEGREE_STEP`]
    /// scale-degrees apart in [`LAND_ANCHOR_HZ`]'s bright register, scattered
    /// alternately either side of the landing column and pre-delayed
    /// [`LAND_STAR_STEP_S`] apart (the arpeggio idiom — delayed voices, no
    /// scheduler), over one soft low ARRIVAL BODY with a whisper of air. The
    /// first star has `delay = 0`, so a landing speaks in the first post-cue
    /// buffer like every other audible gesture.
    ///
    /// Pitched through [`Self::melody_hz`] at the melody's CURRENT degree — not
    /// the raw scale — so the chime is consonant with the tune under every
    /// [`crate::tone::Tone`] and inherits the module's no-beating law wholesale.
    /// NOT duck-exempt: a bonk still punches through it, so wrong-note priority
    /// is preserved.
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
        // makes the stars read as LANDING rather than merely twinkling.
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

    /// THE SPACEBAR'S DOWNBEAT — one short bass root on the speaking palette's
    /// own tonic, octave-folded into a single bass register
    /// ([`SPACE_BASS_LO_HZ`]) and FIXED there. See that constant for why the
    /// pitch no longer comes from the melody's degree: the old nearest-tonic
    /// rule made consecutive spaces jump a full octave on nothing the ear can
    /// attach to the text.
    ///
    /// MONOPHONIC — a live downbeat is damped before the next one spawns
    /// ([`SPACE_DAMP_S`]), because two voices at one FIXED frequency with
    /// randomised phase comb-filter against each other. And only the HEAD of a
    /// whitespace run gets the bass at all: the tail answers with the breath
    /// alone, so indentation is one gesture rather than four bass notes.
    ///
    /// The voice is deliberately UN-articulated: no [`gesture_bend`] scoop
    /// (a downbeat arrives, it does not lean), a round attack, a dark
    /// low-pass, and a breath of air over it. It is CENTRED rather than
    /// panned with the caret column — a bass root is the room's floor, not a
    /// position in it, and hard-panned low frequencies read as a defect.
    fn design_space(&mut self, ev: &SoundEvent, g: f32, pause: f32) {
        let anchor = palette_for(ev.voice, ev.style).anchor_hz();
        // THE BREATH — the exhale between words. Both the head and the run's
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
            return;
        }
        // ONE bass voice: retrigger the live downbeat rather than stack on it.
        for v in &mut self.voices {
            if v.on && v.bass && v.damp <= 0.0 {
                v.damp = SPACE_DAMP_S;
                v.damp0 = SPACE_DAMP_S;
            }
        }
        // THE DOWNBEAT BREATHES when the hand rests (see the SPACE_BREATHE_*
        // constants): a space arriving off a PHRASE pause — the same boundary
        // that resets the bar — opens a fresh thought, and its bass blooms a
        // little instead of merely landing. At prose cadence the pause never
        // clears the threshold and the working envelope below is untouched.
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
        let f = self.melody_hz(bass_octave(anchor), i32::from(self.song_key));
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
                    ..Partial::default()
                },
                // The octave above: roundness on a woofer, IDENTITY on a
                // laptop speaker that reproduces nothing at the fundamental.
                Partial {
                    lvl: oct_lvl,
                    f0: f * 2.0,
                    f1: f * 2.0,
                    ..Partial::default()
                },
                // The WARMTH — a barely-audible fifth over the root (see
                // [`SPACE_FIFTH_LEVEL`]): felt in the chord, never hummable.
                Partial {
                    lvl: fifth_lvl,
                    f0: f * 1.5,
                    f1: f * 1.5,
                    ..Partial::default()
                },
            ],
            ..breath(0.05)
        };
        self.spawn(v, g * SPACE_VOICE_LEVEL, 0.0);
    }

    /// The SHIFT LIFT — the family's anticipation gesture: one whisper-level
    /// tone a lattice step ABOVE the melody's degree (`deg` already carries
    /// the shape's +1), entered from below through the family's own
    /// [`gesture_bend`], so the lift leans exactly the way the capital's
    /// keystroke will land. A breath of high air keys "lift" without adding
    /// a pitch; everything else about the voice is the movement family's
    /// soft sine, shorter and quieter still.
    fn design_shift(&mut self, ev: &SoundEvent, deg: i32, g: f32) {
        let anchor = palette_for(ev.voice, ev.style).anchor_hz();
        let f = self.melody_hz(anchor, deg);
        let (f0, f1) = gesture_bend(f, 1);
        let v = Voice {
            dur: 0.10,
            attack: 0.004,
            decay: 0.05,
            p: [
                Partial {
                    lvl: 0.45,
                    f0,
                    f1,
                    glide: GESTURE_BEND_TAU,
                    ..Partial::default()
                },
                Partial::default(),
                Partial::default(),
            ],
            // The air of a hand lifting — high, tiny, static.
            n_lvl: 0.02,
            n_f0: 4800.0,
            n_f1: 4800.0,
            n_glide: 0.0,
            n_q: 0.5,
            lp_cut: 2600.0,
            ..Voice::default()
        };
        self.spawn(v, g * 0.5, ev.pan);
    }

    /// THE ERASE POOF — the deletion's whole voice (see the `POOF_*` constants
    /// for the design brief). A broad noise BODY at the instant of the press
    /// and a brighter AIR CAP dispersing behind it — SETTLING downward as it
    /// goes ([`POOF_AIR_SETTLE_HZ`]): the "-oof" relaxes off the "p" instead
    /// of holding its band. NO tonal partial anywhere, which is what makes it
    /// a puff rather than a note, and no palette voice behind it, because
    /// `design_trail` returns here.
    ///
    /// The BODY's cutoff is STATIC — `spawn` caches the state-variable
    /// filter's coefficient and divisor once instead of paying a `tan` per
    /// sample. The cap pays the swept band for its settle; deletions are
    /// [`ERASE_MIN_GAP`]-thinned, so at most ~13 swept voices/s.
    ///
    /// Spawned through [`Self::spawn_seeded`] with fixed phases — a voice with
    /// no tonal partial has no phase to hear, and consuming no `rnd()` draws
    /// keeps the typing melody's seeded stream independent of how many
    /// corrections are interleaved with it.
    ///
    /// A HELD run additionally keeps exactly ONE pending release SHIMMER
    /// scheduled behind it (see the `ERASE_SHIMMER_*` constants): every
    /// admitted poof of the run expires the previous pending one (the damp in
    /// [`Self::push`]) and books the next, so the glitter sounds once, on the
    /// run's last release.
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
        // THE HELD RUN'S RELEASE SHIMMER — booked, not sounded: the previous
        // pending one was expired in `push` and this one survives only if the
        // run ends here (the future the engine cannot hear, answered the way
        // the SPACE answers its monophony — by retrigger).
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

    /// Expire the held delete run's PENDING release shimmer — the voice
    /// booked by [`Self::design_erase_poof`] that has not yet begun to sound
    /// (`t < 0`, still inside its pre-delay). The damp burns on the sample
    /// clock pre-delay included, so the voice dies unheard; a shimmer already
    /// SOUNDING is left to ring — it is a whisper, and cutting it audibly
    /// would be the click this engine never makes.
    fn damp_pending_shimmer(&mut self) {
        for v in &mut self.voices {
            if v.on && v.shimmer && v.t < 0.0 && v.damp <= 0.0 {
                v.damp = ERASE_SHIMMER_DAMP_S;
                v.damp0 = ERASE_SHIMMER_DAMP_S;
            }
        }
    }

    /// THE WORD POOF ([`SoundKind::KillWord`]) — the erase poof's slightly
    /// larger, softer cousin (see the `WORD_POOF_*` constants): the same
    /// body-then-air anatomy at word scale, terminal before palette dispatch
    /// exactly like the character's. A word leaving is a BIGGER puff, not a
    /// louder key — the body sits lower and both bursts run longer, and the
    /// whole voice stays under the line kill's swoosh
    /// (`the_word_kill_sits_between_the_poof_and_the_swoosh`).
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
        self.spawn_seeded(body, g * POOF_VOICE_GAIN * WORD_POOF_GAIN, ev.pan, 0.0, [0.0; 3]);
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

    /// THE CLOUD'S LITTLE NOISE ([`SoundKind::Poof`]) — the visible smoke's
    /// own air, designed at kind level beside the erase poof whose dispersal
    /// it is.
    ///
    /// The owner's brief for it, verbatim: *"a puff, not a thud, and it must
    /// not fight the typing"*. So:
    /// - PURE NOISE, no partials — a puff of air has no pitch, and a pitchless
    ///   voice cannot land on a wrong note of the tune it plays under;
    /// - THE ERASE POOF'S OWN FAMILY: the same `POOF_*` air band the deletion
    ///   speaks, because the cloud and the poof are one physical event seen
    ///   and heard — a different band here read as a second, unrelated sound;
    /// - LONGER AND SOFTER than the poof's cap, settling downward
    ///   ([`POOF_CLOUD_SETTLE_HZ`]): the smoke thinning, not a second contact.
    ///
    /// The tier lives in [`POOF_KIND_GAIN`] (the quietest in the vocabulary)
    /// and the noise compensation in [`POOF_CLOUD_GAIN`]; see the
    /// `POOF_CLOUD_*` constants for the design brief.
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

    /// The curse-word BONK — designed once at kind level exactly like Kill,
    /// free to use NON-pentatonic intervals precisely because everything else
    /// in the engine is constrained consonant. Two voices: the clash (minor
    /// second + tritone against the melody's CURRENT walk degree, in the
    /// active palette's own register via [`Palette::anchor_hz`]) over a
    /// round low thump — cartoon "bonk", not alarm. Both are duck-exempt and
    /// arm the master duck so the melody makes way. Feel gates: kind-gain
    /// [`BONK_KIND_GAIN`] > 1, no bed feed, no walk step.
    fn design_bonk(&mut self, ev: SoundEvent, duck: f32) {
        let g = ev.gain * duck * (0.55 + 0.45 * ev.heat) * BONK_KIND_GAIN;
        let root = penta(palette_for(ev.voice, ev.style).anchor_hz(), self.walk);
        let m2 = root * BONK_MINOR_SECOND;
        let tt = root * BONK_TRITONE;
        // The clash: both wrong notes sag onto their targets from ~a third
        // above — the pitch DROPS like something landing on your head.
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
        // VOICE TRIM: holds the bonk's delivered level near -16 dBFS. Without
        // it the clash rides `MASTER` up to ~-9.4 dBFS — genuine alert
        // territory, against this module's "far below alert/bell level" ethos.
        // `BONK_KIND_GAIN` is deliberately NOT the knob for this: its job is
        // PRIORITY (the bonk outranks every trail kind-gain).
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

    /// Latch [`Self::song_key`] — the pentatonic root the typed melody borrows
    /// while the cat sings — from the bar's ONE payload, the signature.
    ///
    /// SEPARATE from [`Self::design_celebration`] because the key outlives the
    /// bar: `design_celebration` schedules THIS bar's voices and returns, while
    /// the latch stands until the sing duck hands back (`song_key = 0` in
    /// `render`). Reading it out of `celebration_root` rather than storing an
    /// independent "key" field is what makes the two layers provably agree —
    /// the typed note and the riff are transposed by the same integer, derived
    /// from the same `sig`, so they cannot drift apart.
    ///
    /// THE DEFECT THIS CLOSED (kept for the archaeology): the latch used to
    /// live in the long-deleted `RiffBar { key }` shim arm only, so on the
    /// canonical signature payload the typed melody silently kept walking
    /// the neutral lattice under a transposed song — exactly the "two
    /// lattices that only happened to be a near-just fourth apart" the
    /// feature was written to end.
    fn latch_song_key(&mut self, sig: u32) {
        // THE WHOLE SHIFT, not half of it. The riff transposes every sounding
        // degree by `root + mode` (`design_celebration`), but this latch used
        // to carry the ROOT alone — so "typing joins the song's key" was true
        // only for the characters whose mode rotation happens to be 0, i.e.
        // 33.7 % of printable ASCII; for the rest the typed note sat a
        // rotation away from the song it was supposedly in. Reading the same
        // two axes the riff reads is what makes the two layers provably agree.
        //
        // −3..=4 by construction (`celebration_root` is `sig % 5 − 2`,
        // `celebration_mode` one of {0, 2, −1}), so the i8 narrowing is total
        // and the register guard documented on `MODE_ROTATIONS` covers it.
        self.song_key = (celebration_root(sig) + celebration_mode(sig)) as i8;
    }

    /// One BAR of the SING-ALONG sing-along riff — one bar of the eight-bar
    /// [`CELEBRATION_PHRASE`] form (see that constant's "not the Nyan Cat
    /// melody" note). Up to eight SWUNG eighth-note pulse-wave voices scheduled
    /// as pre-delayed spawns (the engine's arpeggio idiom — samples-based, no
    /// scheduler), [`CELEBRATION_BASS`] folded into the lead voice that opens
    /// each beat, a sixteenth-note fill on the turnaround, and the sing-duck
    /// armed + held for the bar so the ordinary melody makes way and hands back
    /// on wind-down.
    ///
    /// EVERY KEY SINGS ITS OWN VERSE (owner: "I also want a more obvious
    /// difference in the tune generation when pressing different keys").
    /// Three audible axes derive from `sig` — the held character's bijective
    /// song signature — and nothing else, so the design is a pure function
    /// of the payload (the seamless hand-over law: no per-key state SHAPES a
    /// bar, a key change is just the next bar's payload; the synth's one
    /// signature memory, [`Self::last_riff_sig`], exists to damp the previous
    /// key's tails — see THE SWITCH DAMP below — never to voice the new bar):
    ///
    /// * AXIS 1 — the VERSE MELODY: the verse bars' sounding degrees come
    ///   from `sig`'s mixed-radix walk ([`celebration_bar_degrees`]) while
    ///   the chorus bars keep the authored phrase for every key — a
    ///   different verse of the same recognizable celebration. Rhythm,
    ///   groove, swing, build and clap tables stay shared.
    /// * AXIS 2 — the ROOT: [`celebration_root`], clamped -2..=2 (the
    ///   register guard stands), applied to every sounding degree — the
    ///   turnaround FILL included, which the old code dropped (it tumbled
    ///   back to the reference key mid-modulation).
    /// * AXIS 3 — the MODE: [`celebration_mode`]'s pentatonic rotation,
    ///   folded in with the root — felt color, same lattice.
    /// * AXIS 4 — per-key pulse DUTY ([`celebration_duty`]) — wired but
    ///   gated OFF pending the owner's ear.
    ///
    /// Deliberate deviations from the trail path, each an anti-annoyance
    /// inversion: NO governor duck (the armed state IS a key-repeat flood —
    /// rate-ducking the headline riff would crush it to a whisper exactly
    /// while it is the point; the typed notes underneath keep both ducks),
    /// NO walk step and its authored RHYTHM is untouched (`duck_exempt` voices
    /// skip [`tone_feel`], so the phrase keeps its own tempo), NO bed feed
    /// (structural: only `Trail` gestures reach the feed), and `duck_exempt`
    /// voices (the riff must ride ABOVE the sing duck it arms; a concurrent
    /// bonk still punches through on the same exemption, wrong-note priority
    /// preserved).
    ///
    /// PITCH, however, sings on the ACTIVE tone's lattice — the riff calls
    /// [`Self::melody_hz`], exactly like the typed melody it plays over, not
    /// the free [`penta`]. The riff's authored degrees are PENTA degrees, but
    /// under a non-neutral mood the ducked melody underneath is transposed
    /// (Excited) or on a different table (Frustrated/Playful); a riff left on
    /// the untransposed lattice would then rub the bonk's own banned intervals
    /// (minor second / tritone) against those notes. Sharing `melody_hz` keeps
    /// the riff mutually consonant with the melody under EVERY tone, and —
    /// because `melody_hz` is the exact identity under Technical/Calm (table
    /// PENTA, transpose ×1.0) — every pinned neutral-path celebration proof is
    /// byte-untouched. The bonk alone keeps the raw `penta` (its clash is
    /// anchored to the untransposed lattice by design).
    ///
    /// Callers must latch the key with [`Self::latch_song_key`] first — see
    /// that method for why the two are separate.
    fn design_celebration(&mut self, ev: SoundEvent, bar: u16, sig: u32) {
        // THE SWITCH DAMP (owner: "the transition between switching between
        // repeated keys also has overlapping music. that needs to be fixed").
        // The riff's notes deliberately ring past the bar line, and under ONE
        // held key that overhang is the song's sustain pedal — but this bar
        // carries a DIFFERENT signature, so every tail still sounding is the
        // OLD key's root + mode bleeding under the new verse. Lift the pedal:
        // force each still-active celebration voice into the short
        // [`CELEBRATION_DAMP_S`] release BEFORE the new bar spawns. Same-sig
        // bars never reach this loop (single-key celebrations stay
        // byte-identical to the pre-damp render), the wind-down without a
        // switch is untouched (the sing-duck crossfade owns that exit), and
        // the path draws no rng — the damp is as deterministic as the bar.
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
        // AXIS 1 — this key's verse (chorus bars come back authored).
        let phrase = &celebration_bar_degrees(sig, idx);
        let bass_bar = &CELEBRATION_BASS[idx];
        // AXES 2 + 3 — root and mode rotation: ONE combined integer degree
        // shift applied to every sounding degree (lead, bass, fill) before
        // `melody_hz`, so the whole song modulates coherently and stays on
        // the shared consonant lattice.
        let shift = celebration_root(sig) + celebration_mode(sig);
        // AXIS 4 — per-key duty, owner-gated (see [`CELEBRATION_KEY_DUTY`]).
        let duty = celebration_duty(sig);
        // Escalation ramp 0..1 across the opening bars of the hold. A PURE
        // function of the bar index — no rng draw — so the riff replays
        // independently of the typed layer's consumption of the shared stream.
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
            // THE HELD KEY PICKS THE SONG: `deg` is this key's contour degree
            // (verse walk or shared chorus), `shift` its root + mode. The
            // rhythm and the bar grid are untouched, so a mid-hold key change
            // is a modulation on the next bar boundary, never a restart.
            let hz = self.melody_hz(CELEBRATION_BASE_HZ, deg + shift);
            // The BASSLINE rides the lead voice that opens its beat — third
            // partial, no extra voice. `build` fades the low end in over the
            // opening bars.
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
            // BACKBEAT CLAP once the hold has run a full phrase: fused into the
            // note's own band-passed noise channel, so it costs no voice.
            let (n_lvl, n_f0, n_f1, n_glide, n_q) = if clap && (i == 2 || i == 6) {
                (0.10, 2600.0, 1200.0, 0.02, 1.4)
            } else {
                (0.0, 0.0, 0.0, 0.0, 0.0)
            };
            let v = Voice {
                delay: celebration_slot_delay(i),
                // Span 1 is EXACTLY 0.30 s: in f32,
                // `CELEBRATION_EIGHTH * 1.5 == 0.30` bit for bit.
                dur: span as f32 * CELEBRATION_EIGHTH * 1.5,
                attack: 0.004,
                decay: 0.10 * span as f32,
                p: [
                    Partial {
                        lvl: 0.55,
                        f0: hz,
                        f1: hz,
                        wave: Wave::Pulse { duty },
                        ..Partial::default()
                    },
                    Partial {
                        lvl: shimmer,
                        f0: hz * 2.0,
                        f1: hz * 2.0,
                        wave: Wave::Pulse { duty: 0.5 },
                        ..Partial::default()
                    },
                    sub,
                ],
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
        // THE FILL: the turnaround's final two slots subdivide into sixteenths
        // and crescendo back onto the hook.
        if turnaround {
            for (k, &deg) in CELEBRATION_FILL.iter().enumerate() {
                // THE FILL CARRIES THE SHIFT (the fix): it used to call
                // `melody_hz(BASE, deg)` bare, so the turnaround's four
                // sixteenths tumbled back to the REFERENCE key — a wrong-key
                // hiccup once every eight bars for every transposed hold.
                let hz = self.melody_hz(CELEBRATION_BASE_HZ, deg + shift);
                let v = Voice {
                    delay: (6.0 + 0.5 * k as f32) * CELEBRATION_EIGHTH,
                    dur: CELEBRATION_EIGHTH,
                    attack: 0.003,
                    decay: 0.055,
                    p: [
                        Partial {
                            lvl: 0.55,
                            f0: hz,
                            f1: hz,
                            wave: Wave::Pulse { duty },
                            ..Partial::default()
                        },
                        Partial {
                            lvl: shimmer,
                            f0: hz * 2.0,
                            f1: hz * 2.0,
                            wave: Wave::Pulse { duty: 0.5 },
                            ..Partial::default()
                        },
                        Partial::default(),
                    ],
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
        // Arm + hold the sing duck for the whole bar (plus a hair so two
        // bars scheduled back-to-back never gap): constant depth while bars
        // keep arriving, exponential handback once they stop.
        self.sing = 1.0;
        self.sing_hold = CELEBRATION_BAR_SECONDS + 0.1;
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
        // Rate estimate decays with real (sample-clock) time.
        self.since_event += dt_block;
        self.since_voice += dt_block;
        self.since_erase += dt_block;
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
            self.song_key = 0;
            // (The DC blocker needs no reset here: `is_quiet` now requires it
            // to have SETTLED to exact zero, so this early-out can no longer
            // truncate a tail mid-decay.)
            return;
        }

        // Bed housekeeping at block rate: energy decay, level slew, grain
        // spawning (grains become ordinary voices).
        self.tick_bed(dt_block);

        let dt = self.inv_sr;
        // Per-sample duck recovery factor (τ = BONK_DUCK_TAU).
        let duck_step = (-dt / BONK_DUCK_TAU).exp();
        // The bed's floor test is BLOCK-invariant. `bed.level` is written in
        // exactly three places — `tick_bed` (which just ran, above), the
        // quiet early-out, and `push` — and none of them runs inside the
        // sample loop; no palette bed body touches it either. So the decision
        // is made once here instead of 512 times, which is what the shipping
        // `trail_sound_bed = false` default was paying for a layer whose
        // level is EXACTLY 0.0.
        let bed_on = self.bed.level >= 1e-4;
        for f in 0..frames {
            let (mut l, mut r) = (0.0f32, 0.0f32);
            // Duck-exempt sum — the bonk itself, riding above the dip.
            let (mut xl, mut xr) = (0.0f32, 0.0f32);

            // Discrete voices.
            for vi in 0..MAX_VOICES {
                // (Indexing, not iter_mut: the borrow checker frees us to
                // read self.rng etc. — and the loop body is the hot path.)
                let v = &mut self.voices[vi];
                if !v.on {
                    continue;
                }
                v.t += dt;
                // THE SWITCH DAMP clock burns on the sample clock from the
                // moment the order lands, pre-delay included: an old-key
                // voice that never started simply expires unheard instead of
                // opening its note under the new verse. Untaken (damp 0.0)
                // for every voice outside a key switch — the pinned paths
                // render through the exact pre-damp arithmetic.
                if v.damp > 0.0 {
                    v.damp -= dt;
                    if v.damp <= 0.0 {
                        v.on = false;
                        continue;
                    }
                }
                if v.t < 0.0 {
                    continue; // pre-delay
                }
                if v.t >= v.dur {
                    v.on = false;
                    continue;
                }
                // GEOMETRIC RECURSIONS, seeded on the first sounding sample.
                // `v.t`, `v.dur` and every phase accumulator are untouched, so
                // onset, lifetime, tuning and phase are exactly as before;
                // only the exponentials' last bits move.
                // Re-anchor on the first sounding sample AND every
                // `ENV_REANCHOR` samples after it. One flag drives all five
                // recursions below, so they re-anchor together and stay
                // mutually consistent.
                let seed = !v.env_run || v.env_n >= ENV_REANCHOR;
                if seed {
                    v.env_n = 0;
                } else {
                    v.env_n += 1;
                }
                let mut s = 0.0f32;
                for p in &mut v.p {
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
                    s += p.lvl * x;
                }
                // Noise burst through the state-variable bandpass.
                if v.n_lvl > 0.0 {
                    let white = {
                        let mut x = self.rng;
                        x ^= x << 13;
                        x ^= x >> 17;
                        x ^= x << 5;
                        self.rng = x;
                        (x >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
                    };
                    // The damping is `spawn`'s cached `1.0 / n_q.max(0.3)`
                    // — the identical f32 the per-sample divide produced.
                    let damp = v.n_damp;
                    let (g_svf, den) = if v.n_glide > 0.0 {
                        // A SWEPT cutoff really does move per sample: the
                        // geometric `n_e` recursion supplies `fc`, and the
                        // `tan` has to follow it.
                        v.n_e = if seed {
                            (-v.t / v.n_glide).exp()
                        } else {
                            v.n_e * v.k_n
                        };
                        let fc = v.n_f1 + (v.n_f0 - v.n_f1) * v.n_e;
                        let g = (core::f32::consts::PI * (fc * dt).min(0.45)).tan();
                        (g, 1.0 + g * (g + damp))
                    } else {
                        // A CONSTANT cutoff: coefficient and divisor were
                        // both computed in `spawn` from the identical
                        // operands (see `Voice::svf_g`) — this branch was
                        // paying a scalar `tanf` per sample for them.
                        (v.svf_g, v.svf_den)
                    };
                    let hp = (white - v.n_lp - damp * v.n_bp) / den;
                    v.n_bp += g_svf * hp;
                    v.n_lp += g_svf * v.n_bp;
                    s += v.n_lvl * v.n_bp;
                }
                // Envelope + twinkle + release guard. Both exponentials step
                // geometrically; the envelope's SHAPE, the 5 ms release ramp
                // and the voice's hard lifetime are unchanged, since `v.t` and
                // `v.dur` never moved.
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
                // THE SWITCH DAMP release: the same linear-ramp law as the
                // anti-click above, at pedal-lift scale (CELEBRATION_DAMP_S).
                // It starts at ~1.0 the sample the damp is armed — continuous,
                // click-free — and the countdown at the top of the loop kills
                // the voice the moment it reaches zero.
                if v.damp > 0.0 {
                    env *= v.damp / v.damp0.max(1e-4);
                }
                // Per-voice softening lowpass (coefficient cached by
                // `spawn` from the identical expression — see `Voice::lp_k`).
                v.lp += v.lp_k * (s - v.lp);
                let y = v.lp * env;
                if v.duck_exempt {
                    xl += y * v.gl;
                    xr += y * v.gr;
                } else {
                    l += y * v.gl;
                    r += y * v.gr;
                }
            }

            // Bed sample. Skipping it below the floor is EXACT, not an
            // approximation: `bed_sample` returns `(0.0, 0.0)` before it
            // writes any state, and `l`/`r` start at `+0.0` and can never
            // become `-0.0` (IEEE-754 addition yields `-0.0` only when both
            // operands are `-0.0`, and the initial value is `+0.0`), so
            // `l += 0.0` is a true no-op on every reachable value — the one
            // sign-of-zero laundering this hoist could have introduced cannot
            // arise. The bench's per-workload bit checksum is the witness.
            if bed_on {
                let (bl, br) = self.bed_sample(dt);
                l += bl;
                r += br;
            }

            // Master duck: the melody + bed dip around a live bonk AND
            // under a live sing-along riff; each factor is exactly ×1.0 (and
            // the exempt sum exactly +0.0) while its envelope rests, so the
            // default path adds nothing to the pre-framework render
            // (`a * 1.0 == a` for every finite f32 — the pinned proofs run
            // through this very multiply).
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
            out[f * 2] = soft_clip(yl);
            out[f * 2 + 1] = soft_clip(yr);
        }
        // Below ~0.01 % depth the duck is arithmetic noise — snap to the
        // exact 0.0 the bit-identity contract (dmul == 1.0) requires.
        if self.duck < 1e-4 {
            self.duck = 0.0;
        }
        // THE DC BLOCKER, same idiom, same reason: below -120 dBFS its tail is
        // arithmetic rather than signal, and an exact zero is what lets
        // `is_quiet` be an equality instead of a truncation (see there).
        // A settled synth is a RESET synth.
        if self.dc_y_l.abs() < 1e-6 && self.dc_x_l.abs() < 1e-6 {
            self.dc_x_l = 0.0;
            self.dc_y_l = 0.0;
        }
        if self.dc_y_r.abs() < 1e-6 && self.dc_x_r.abs() < 1e-6 {
            self.dc_x_r = 0.0;
            self.dc_y_r = 0.0;
        }
        // Sing-duck housekeeping at BLOCK rate (the envelope is a hold, not
        // a per-sample glide — constant within any host-sized block): burn
        // the hold first, then the exponential handback, then the same
        // exact-zero snap as the bonk duck.
        if self.sing > 0.0 {
            if self.sing_hold > 0.0 {
                self.sing_hold -= dt_block;
            } else {
                self.sing *= (-dt_block / SING_DUCK_TAU).exp();
            }
            if self.sing < 1e-4 {
                self.sing = 0.0;
                // THE KEY IS RELEASED WITH THE DUCK — the law `song_key`'s doc
                // states ("exactly as live as the song is"). It used to be
                // released ONLY in the `is_quiet()` early return above, which a
                // typist never reaches: at ~9.5 cps or faster a voice is always
                // live, so the borrowed transpose outlived the song for the
                // rest of the session. Measured 3.5-6.0 s after the riff died,
                // the typed register was still pinned by whichever key had been
                // held — a 9.2-semitone spread across 'z'/'o'/'a'/'e'.
                self.song_key = 0;
            }
        }
    }

    /// Block-rate bed upkeep: energy decay, level slew, stochastic grains
    /// (grain design is per-palette — [`Palette::bed_grain`]).
    fn tick_bed(&mut self, dt: f32) {
        // Grains belong to the SHIPPING bed designs (sparkle's chimes,
        // water's plips, fire's pops): a tournament candidate is judged on
        // its own texture alone, so grain arming is gated to `Current`.
        // Hoisted before the borrow; trivially true on the default path.
        let grains_live = self.bed_variant == BedVariant::Current;
        let b = &mut self.bed;
        // Below ~-34 dB the bed is already imperceptible: hurry the tail so
        // the host's queue pause engages within ~a second of audibility
        // ending, instead of chasing the exponential for five more.
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
        // Floor snap: once the whole bed sits below −62 dB pre-gain it is
        // imperceptible — snap to exact zero so `is_quiet` (and the host's
        // queue pause) engages promptly instead of chasing an infinite tail.
        if b.energy < 3e-3 && b.level < 3e-3 {
            b.energy = 0.0;
            b.level = 0.0;
        }
        // Beam power-down droop: rises toward 1 as the level dies.
        let dying = (0.35 - b.level).max(0.0) / 0.35;
        b.droop += (dying - b.droop) * (1.0 - (-dt / 0.4).exp());

        // Stochastic grains for the textures that live on scarcity.
        if grains_live && b.level > 0.02 {
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
        if self.bed.level < 1e-4 {
            return (0.0, 0.0);
        }
        // Tournament dispatch BEFORE the palette prologue: a candidate owns
        // its whole texture (LFOs included, run off the variant clock), and
        // the shipping path below must not share state with it. On the
        // default (`Current`) this branch is never taken, so the palette
        // beds render through the exact pre-tournament code.
        if self.bed_variant != BedVariant::Current {
            return self.bed_variant_sample(dt);
        }
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

    // -- ambient-bed tournament candidates ----------------------------------

    /// One stereo sample of the selected non-`Current` tournament candidate.
    /// Same contract as the palette [`Palette::bed_sample`] path: the caller
    /// has already floored `bed.level`, and the result lands in the DUCKED
    /// mix sum, so the bonk/sing duck and the master chain apply to
    /// candidates exactly as they do to the shipping beds (the loudness law
    /// is inherited, not re-implemented). All randomness-free: candidates
    /// derive every modulation from the sample-driven variant clock, so a
    /// candidate render is bit-replayable from (events, seed, variant).
    fn bed_variant_sample(&mut self, dt: f32) -> (f32, f32) {
        let lvl = self.bed.level * self.bed.gain;
        // The palette's own melodic register anchors every candidate — the
        // same "wrong against the melody actually playing" logic as the
        // bonk anchor, reused for the opposite purpose (being RIGHT under
        // that melody).
        let anchor = palette_for(self.bed_voice, self.bed_style).anchor_hz();
        self.bed.var_t += dt;
        let (m, side) = match self.bed_variant {
            // Structurally unreachable (the caller dispatches `Current` to
            // the palette path) — but this is the audio hot path, so the
            // defensive arm is silence, never a panic.
            BedVariant::Current => (0.0, 0.0),
            BedVariant::ChordDrift => self.bed_chord_drift(dt, lvl, anchor),
            BedVariant::Breathing => self.bed_breathing(dt, lvl, anchor),
            BedVariant::Shimmer => self.bed_shimmer(dt, lvl, anchor),
            // C4: the bed layer contributes literal zeros — the "no bed"
            // incumbent rendered through the identical harness so its
            // artifacts are comparable.
            BedVariant::Silence => (0.0, 0.0),
        };
        (m + side, m - side)
    }

    /// C1 — SLOW CHORD DRIFT. Three sines stacked on the active lattice
    /// ([`CHORD_DRIFT_STACK`] over the walking [`CHORD_DRIFT_ROOTS`]), one
    /// octave under the palette anchor, portamento-gliding between bars so
    /// the pad is never static and never steps. Pitch TARGETS are always
    /// `melody_hz` lattice degrees — consonance with whatever the melody is
    /// doing is inherited from the table invariant.
    fn bed_chord_drift(&mut self, dt: f32, lvl: f32, anchor: f32) -> (f32, f32) {
        let bar_s = CHORD_DRIFT_CYCLE_S / CHORD_DRIFT_ROOTS.len() as f32;
        let bar = ((self.bed.var_t / bar_s) as usize) % CHORD_DRIFT_ROOTS.len();
        let root = CHORD_DRIFT_ROOTS[bar];
        let mut tgt = [0.0f32; 3];
        for (t, off) in tgt.iter_mut().zip(CHORD_DRIFT_STACK) {
            *t = self.melody_hz(anchor * 0.5, root + off);
        }
        let b = &mut self.bed;
        let glide = 1.0 - (-dt / CHORD_DRIFT_GLIDE_TAU).exp();
        // Voicing balance: root carries, upper tones color. Static weights —
        // the MOTION lives in pitch, which is the candidate's thesis.
        const WEIGHT: [f32; 3] = [1.0, 0.8, 0.65];
        let mut m = 0.0;
        for i in 0..3 {
            if b.var_f[i] <= 0.0 {
                // First sample: seed at target so the pad enters ON the
                // chord instead of sweeping up from 0 Hz.
                b.var_f[i] = tgt[i];
            }
            b.var_f[i] += (tgt[i] - b.var_f[i]) * glide;
            b.var_ph[i] = (b.var_ph[i] + b.var_f[i] * dt).fract();
            m += sin01(b.var_ph[i]) * WEIGHT[i];
        }
        // A whisper of amplitude undulation (~0.09 Hz off the variant
        // clock) so held bars still breathe a little.
        let und = 0.85 + 0.15 * sin01((b.var_t * 0.09).fract());
        (m * und * lvl * 0.014, 0.0)
    }

    /// C2 — BREATHING PAD. The [`BREATH_DEGREES`] lattice pad under the
    /// swell law (amplitude factor 0.05→0.2 on a ~12 s raised
    /// cosine) with the spectral tilt animating on the same breath: a
    /// one-pole lowpass opens toward ~2.6 kHz at the top of the inhale and
    /// closes to ~250 Hz at the bottom, so the pad brightens as it swells —
    /// breath, not tremolo.
    fn bed_breathing(&mut self, dt: f32, lvl: f32, anchor: f32) -> (f32, f32) {
        let mut freq = [0.0f32; 4];
        for (f, d) in freq.iter_mut().zip(BREATH_DEGREES) {
            *f = self.melody_hz(anchor * 0.5, d);
        }
        let b = &mut self.bed;
        // Raised cosine in turns: sin01(x + 0.25) == cos(2πx).
        let breath = 0.5 - 0.5 * sin01(((b.var_t / BREATH_CYCLE_S).fract() + 0.25).fract());
        let swell = BREATH_SWELL_MIN + (BREATH_SWELL_MAX - BREATH_SWELL_MIN) * breath;
        const WEIGHT: [f32; 4] = [1.0, 0.7, 0.55, 0.4];
        let mut m = 0.0;
        for i in 0..4 {
            b.var_ph[i] = (b.var_ph[i] + freq[i] * dt).fract();
            m += sin01(b.var_ph[i]) * WEIGHT[i];
        }
        // Tilt rides breath² so the top octave only speaks near full
        // inhale — the animation is spectral, not just louder.
        let cut = 250.0 + 2350.0 * breath * breath;
        let k = (cut * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (m - b.lp1);
        (b.lp1 * swell * lvl * 0.16, 0.0)
    }

    /// C3 — SHIMMER WASH. Four very quiet HIGH lattice partials
    /// ([`SHIMMER_DEGREES`] on the anchor itself — no low fundamental
    /// exists), each fading on its own slow LFO ([`SHIMMER_RATES`],
    /// incommensurate) with alternating-side placement for a gentle width.
    /// The candidate that tests "does a bed even need a floor?".
    fn bed_shimmer(&mut self, dt: f32, lvl: f32, anchor: f32) -> (f32, f32) {
        let mut freq = [0.0f32; 4];
        for (f, d) in freq.iter_mut().zip(SHIMMER_DEGREES) {
            *f = self.melody_hz(anchor, d);
        }
        let b = &mut self.bed;
        const WEIGHT: [f32; 4] = [0.9, 0.8, 0.7, 0.5];
        let (mut m, mut side) = (0.0f32, 0.0f32);
        for i in 0..4 {
            b.var_ph[i] = (b.var_ph[i] + freq[i] * dt).fract();
            // Fade 0.3..1.0 — partials recede, never gate (the wash must
            // shimmer, not blink). Phase-offset per partial so the four
            // fades never align.
            let fade_ph = (b.var_t * SHIMMER_RATES[i] + i as f32 * 0.37).fract();
            let fade = 0.65 + 0.35 * sin01(fade_ph);
            let x = sin01(b.var_ph[i]) * WEIGHT[i] * fade;
            m += x;
            // Alternate partials lean left/right a touch.
            side += if i % 2 == 0 { x * 0.18 } else { -(x * 0.18) };
        }
        // 0.015: tuned by the audition metrics — "very quiet" (≈ −50 dBFS
        // bed RMS, ~10 dB under the melody) yet still crossing the −50 dBFS
        // audibility line during typing, so the wash is an entrant rather
        // than an inaudible no-op.
        (m * lvl * 0.015, side * lvl * 0.015)
    }
}

// ---------------------------------------------------------------------------
// Palettes — one unit per GlowStyle, registered in `palette_for`
// ---------------------------------------------------------------------------

/// One style's complete sound identity: discrete voice design, stochastic
/// bed grains, and the continuous bed texture. Implementors are stateless
/// units — ALL state lives on [`TrailSynth`], which is passed back in, so a
/// palette is exactly a namespace for its numbers (each one IS its own sound
/// designer; the intent comments ride with the numbers).
///
/// Adding a palette = one implementor + one [`palette_for`] arm. See the
/// module docs for why this is a trait rather than a prototype data table
/// (procedural palettes; byte-identity of the shipped nine), and how a
/// data-driven Trail-Pack palette would still fit behind this exact seam.
trait Palette {
    /// Design and spawn the voice(s) for one admitted trail gesture (the
    /// style-agnostic kinds — Kill, Land, the movement family, the space
    /// comma and the shift lift — are designed kind-level before dispatch
    /// and never arrive here).
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
    /// caller has already advanced the shared LFOs (`u1`/`u2`) and folded
    /// level × gain into `lvl`.
    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, u1: f32, u2: f32) -> (f32, f32);

    /// THE PALETTE'S OWN MELODIC BASE — the register its keystroke lives in,
    /// and the one register every style-agnostic gesture borrows so it speaks
    /// in the same octave as the typing rather than in one of its own:
    /// - the curse BONK clashes here, so the wrong note is wrong AGAINST the
    ///   melody actually playing;
    /// - the MOVEMENT family (Glide/Sweep, [`TrailSynth::design_cursor`]) sings
    ///   here, so scrubbing is the typing's relative;
    /// - the bed's tournament candidates stack their lattice here.
    ///
    /// Default: the Lumen mid register (also right for unpitched palettes like
    /// Fire). Renamed from `bonk_anchor_hz` when the movement family joined the
    /// bonk in reading it — one register per palette, named once.
    fn anchor_hz(&self) -> f32 {
        CURSOR_ANCHOR_HZ
    }
}

/// The palette registry — the ONE place a [`GlowStyle`] binds to its sound.
/// Trail Packs (`Custom`) are DATA-driven looks with no sound palette of
/// their own, so they ride Lumen's pluck (a pack-declared palette would
/// register here too). A non-[`SoundVoice::Style`] voice overrides the style
/// binding wholesale — the host's `trail_sound_style` decoupling — while
/// `Style` resolves exactly the pre-override table, so the byte pins hold.
/// [`SoundVoice::Of`] is an ALIAS into that table (the standalone instrument
/// IS the style's palette, chosen regardless of the look), never a fork of
/// it; the three sound-only voices bind their own palettes here.
fn palette_for(voice: SoundVoice, style: GlowStyle) -> &'static dyn Palette {
    match voice {
        SoundVoice::Mech => &MechPalette,
        SoundVoice::Typewriter => &TypewriterPalette,
        SoundVoice::Marimba => &MarimbaPalette,
        SoundVoice::Felt => &FeltPalette,
        SoundVoice::Of(s) => palette_for(SoundVoice::Style, s),
        SoundVoice::Style => match style {
            GlowStyle::Lumen | GlowStyle::Custom => &LumenPalette,
            GlowStyle::Phaser => &PhaserPalette,
            GlowStyle::RainbowKitty => &RainbowKittyPalette,
            GlowStyle::Sparkle => &SparklePalette,
            GlowStyle::Fire => &FirePalette,
            GlowStyle::Laser => &LaserPalette,
            GlowStyle::Beam => &BeamPalette,
            GlowStyle::Water => &WaterPalette,
            GlowStyle::Comet => &CometPalette,
        },
    }
}

/// LUMEN — lamplight: each key BLOOMS onto its note with a beating twin and
/// sub-octave glow, over its own breathing lamp pad. The "default" sound: a
/// good keyboard should sound like this feels.
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
        // LAMPLIGHT: each key BLOOMS — a warm mid tone easing gently UP onto
        // its note, a softly beating twin for width, a sub-octave glow, and a
        // breath of air. Backspace dims (the bloom drifts back down).
        //
        // `deg` already carries the family's deletion step (`gesture_shape`);
        // this palette states the DIM, not the interval.
        let f = s.melody_hz(330.0, deg);
        let (f0, f1v) = if kind == SoundKind::Backspace {
            (f, f * 0.94)
        } else {
            (f * 0.97, f)
        };
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
        // The lamp left on — the airy pair plus a soft third a tenth above
        // that breathes with the slow LFO, so the afterglow gently swells
        // and dims instead of holding flat.
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 165.0 * dt).fract();
        b.ph2 = (b.ph2 + 165.6 * dt).fract();
        b.ph3 = (b.ph3 + 412.5 * dt).fract();
        let sm = sin01(b.ph1) + sin01(b.ph2) + (0.1 + 0.2 * u1) * sin01(b.ph3);
        (sm * lvl * 0.024, sm * lvl * 0.005)
    }
}

/// PHASER — the spectrum sweep: a soft "pew" gliding down onto a pentatonic
/// target whose degree follows the LIVE HUE, so the band's colour and pitch
/// travel together. Backspace sweeps UP (the phaser un-fires).
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
        // THE PLAYFUL EMITTER: a rounded "boop" that SETTLES onto a note
        // whose degree follows the LIVE HUE, a tiny low THOCK for the tactile
        // landing, an up-turned "hm?" on backspace, a two-note "ba-deep!"
        // on Jump.
        let hue_deg = (ev.hue * 5.0) as i32;
        // Phaser builds its OWN degree (the live hue drives it, not the melody
        // walk), so it must ASK for the family's deletion step rather than
        // spell an interval of its own — the rule has exactly one home.
        let d = hue_deg + col_off + gesture_shape(kind).offset;
        let f = s.melody_hz(392.0, d);
        let (f0, f1v) = if kind == SoundKind::Backspace {
            (f, f * 1.12)
        } else {
            (f * 1.09, f)
        };
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
        // Charged-emitter PURR — detuned triangle-ish pair under a gentle
        // sweep kept low and slow (400–900 Hz): a contented hum, not a
        // filter show.
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

/// THE GRACE BEND — the bell takes the gesture family's contour bend at GRACE
/// depth: the same direction from [`gesture_shape`]/[`gesture_bend`] (a
/// deletion is still the keystroke mirrored), at a third of the whole tone
/// (~0.65 st), settled (3τ) in ~12 ms under the mallet click. The full-depth
/// 12 ms scoop measured ~1.4 st still audibly gliding at 24 ms — that rise WAS
/// the squeak the owner named. Applied to the FUNDAMENTAL only: the tinks
/// strike at fixed pitch, because the glass doesn't slide. Shared verbatim
/// with the `v056_reference` oracle copy, exactly like [`GESTURE_BEND_TAU`].
const KEY_BELL_BEND_SHARE: f32 = 0.33;
/// The grace bend's glide time constant (seconds) — 3τ ≈ 12 ms, inside the
/// mallet transient, so the bend reads as articulation, never as pitch.
const KEY_BELL_BEND_TAU: f32 = 0.004;

/// RAINBOW KITTY — the glass-bell ribbon: tiny struck bells walking the
/// pentatonic — mallet click, sine body with an icy FM glint, twin detuned
/// double-octave tinks shimmering over a real ring-out.
/// Jump = a fast major-arpeggio run up, the classic power-up (and, under
/// rapid line feeds, the beloved "brrrring!" — see [`SoundKind::Jump`]),
/// now a cascade of the same bells.
struct RainbowKittyPalette;

impl Palette for RainbowKittyPalette {
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        deg: i32,
        _col_off: i32,
    ) {
        // THE GLASS KEY — "sounds too much like a squeak versus the magical
        // tinkly bells from before." (owner, 2026-08-16). This ruling
        // condemns the founding 25 %-duty chirp CORE itself: the 2026-08-15
        // tingly-bell restore put the right glass tink over the wrong body,
        // and the owner's ears named what remained. The sanctioned raw
        // material is the celebration bells' own strike/glass/ring physics
        // (`fb714f49`), scaled to tier-1 keystroke level and driven through
        // the untouched melody/gesture seam.
        //
        // THE SQUEAK-MAKERS, measured, and how each dies:
        // - THE AUDIBLE UPWARD SCOOP. The full whole-tone bend (τ 12 ms) on
        //   all three partials measured ~1.4 st still gliding at 24 ms — a
        //   rising glide on a dense midrange tone IS the squeak archetype
        //   (the beloved v0.20.0 voice had no glide at all). Now a GRACE
        //   bend: same family direction (a deletion still mirrors), a third
        //   of the depth, τ 4 ms — settled by ~12 ms, under the mallet click
        //   — and on the FUNDAMENTAL only; the tinks strike at fixed pitch,
        //   because the glass doesn't slide.
        // - THE 25 % PULSE COMB. Harmonics at 2f −3.3 dB / 3f −10.3 dB filled
        //   0.5–2.3 kHz — the reedy buzz. Now a PURE SINE body wearing an
        //   inharmonic FM strike face (ratio 3.01, the house ICE idiom;
        //   `fm_tau` ≪ decay, so the bright partials die first — which IS
        //   bell physics).
        // - THE FUSED TINK. Exactly 4f, same onset, same bend, same envelope,
        //   −8 dB UNDER the fundamental: one brighter squeak, never
        //   "chirp + glass". Now the crown — a DETUNED PAIR at 4f (0.34) and
        //   4.010f (0.18) whose 5–13 Hz beat is the built-in twinkle
        //   (detune 0.010 keeps even degree 7's 13.1 Hz under the 15–30 Hz
        //   roughness band), fixed-pitch while the body scoops: differential
        //   fate at onset makes two auditory objects. The fundamental stays
        //   the strictly LOUDEST single partial (0.44 vs 0.30/0.18) — the
        //   family tests select the loudest partial and it must land exactly
        //   on the melody note.
        // - MIDRANGE UNDER A HORN'S ROOF. 84 % of the energy sat in
        //   0.5–2 kHz below lp 3000. The sine body empties the comb, the
        //   roof opens to 4200 (the celebration floor), and the energy moves
        //   to the 2.1–3.1 kHz tink band and the 5 kHz mallet transient.
        // - NOTHING RANG. The 110 ms hard stop killed the tink with the body
        //   — a squeak-toy click. Now dur 0.30 / decay 0.085: a struck
        //   envelope (under 1/e in 85 ms — the pluck law holds) with a
        //   ~200 ms audible glass tail whose crossings belong to the tinks.
        //
        // THE TRIM LESSON, inherited. `57ad9c7c` ("the doop is cute again") was briefed as
        // "a BIT cuter" but landed a full revert to the founding voice, and its
        // trim re-fit was done on the WRONG QUANTITY: `mix_meter` reports
        // PEAK, so 1.39 → 1.097 equalised peak to within 0.1 dB while RMS
        // fell 3.5 dB and CREST rose 3.6 dB (14.0 → 17.6). "Cuteness must not
        // buy loudness" was enforced; what it bought instead went unmeasured.
        // The lesson stands for THIS re-voice too: fit the trim on peak to
        // hold the −21.0 dBFS ladder floor, and WATCH RMS and crest while
        // doing it — a struck bell's crest is high because that sharpness is
        // the identity, but it must be a measured choice, not a fitting
        // artifact. (Fit and figures live on [`palette_trim`].)
        //
        // TUNED on the design's own fallback ladder, re-measured per rung
        // (the spec's opening levels read 2487 Hz on the a-neutral centroid,
        // over the 2200 ceiling — glassy-thin): body 0.40 → 0.44, tink
        // 0.34 → 0.30, and the FM glint settled at 1.4 — its 1.2 rung put
        // the centroid at 2146 but cost the strike-vs-ring tilt (+245 Hz
        // against the ≥ +300 acceptance; the glint IS the strike face). The
        // mallet stayed 1.1: a 1.3 trial bought +5 Hz of tilt for +0.45 dB
        // of peak — the wrong trade.
        //
        // MEASURED over the 12.5 s typing script (`typing_voice_ab`,
        // scenario `a-neutral`; squeak-era figures in brackets):
        //   spectral centroid   2199 Hz   [1633]  (accept 1700–2200)
        //   energy over 2 kHz   0.387     [0.252] (accept 0.30–0.60)
        //   sweep_st            +0.03 st  [~+0.9] (accept ≤ 0.35 — the
        //                                          chirp is measurably dead)
        //   roughness 15–30 Hz  0.114     [0.170] (accept ≤ 0.15)
        //   crest               17.3 dB   [16.7]  (accept 15.5–19.5)
        // Isolated Typed: pitch settled by 6 ms at +0.21 st total rise
        // [1.4 st, still gliding at 24 ms], ring −15.7 dB of peak at 150 ms
        // [dead at 110 ms], strike-vs-ring centroid tilt +308 Hz — the
        // bright partials genuinely die first.
        let base = 523.25; // C5 — the founding register, unchanged
        // `deg` already carries the family's deletion step; the palette states
        // the TIMBRE, not the interval.
        let f = s.melody_hz(base, deg);
        let (b0, b1) = gesture_bend(f, gesture_shape(kind).dir);
        // The GRACE bend: enter a third of the family bend out, settle onto
        // the note in ~12 ms — under the mallet, over before the ring.
        let f0 = b1 + (b0 - b1) * KEY_BELL_BEND_SHARE;
        let mk = |f0: f32, f1: f32, delay: f32| Voice {
            delay,
            dur: 0.30,
            attack: 0.0015,
            decay: 0.085,
            p: [
                // THE GLASS BODY — pure sine on the melody note (the law
                // partial: strictly loudest, lands exactly), grace-bent, its
                // FM strike glint ~1/e gone by 30 ms: the bright face of the
                // strike dies ~3× faster than the body it excites.
                Partial {
                    lvl: 0.44,
                    f0,
                    f1,
                    glide: KEY_BELL_BEND_TAU,
                    fm_ratio: 3.01,
                    fm_i0: 1.4,
                    fm_tau: 0.030,
                    ..Partial::default()
                },
                // THE TWIN TINK — the crown's detuned half: beats against the
                // 4f tink at 0.010·f (5.2 Hz at C5, 13.1 Hz at degree 7), the
                // twinkle itself, organically pitch-dependent.
                Partial {
                    lvl: 0.18,
                    f0: f1 * 4.010,
                    f1: f1 * 4.010,
                    ..Partial::default()
                },
                // THE TINK — pure double-octave glass, fixed pitch.
                Partial {
                    lvl: 0.30,
                    f0: f1 * 4.0,
                    f1: f1 * 4.0,
                    ..Partial::default()
                },
            ],
            // THE MALLET — a 6 ms click falling out of 5.2 kHz, parked
            // sub-audibly under the strike so the ring stays pure.
            n_lvl: 1.1,
            n_f0: 5200.0,
            n_f1: 180.0,
            n_glide: 0.006,
            n_q: 0.7,
            lp_cut: 4200.0,
            ..Voice::default()
        };
        s.spawn(mk(f0, f, 0.0), g * 0.30, ev.pan);
        if kind == SoundKind::Jump {
            // 1-3-5-8 run, 45 ms apart — the rainbow leaps, now a cascade of
            // ringing bells. Each note of the run sings onto its own degree
            // with its own grace bend, so the flourish is the keystroke's
            // articulation repeated, not a different voice.
            let mut leap = |step: i32, delay: f32, lvl: f32, pan: f32| {
                let fl = s.melody_hz(base, deg + step);
                let (a0, a1) = gesture_bend(fl, 1);
                s.spawn(
                    mk(a1 + (a0 - a1) * KEY_BELL_BEND_SHARE, fl, delay),
                    g * lvl,
                    pan,
                );
            };
            leap(2, 0.045, 0.3, ev.pan * 0.5);
            leap(3, 0.09, 0.27, ev.pan * 0.2);
            leap(5, 0.135, 0.24, -ev.pan * 0.3);
        }
    }

    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, _u1: f32, _u2: f32) -> (f32, f32) {
        // Faint detuned pulse pad — the chip's idle hum.
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 261.6 * dt).fract();
        b.ph2 = (b.ph2 + 262.6 * dt).fract();
        let sm =
            (if b.ph1 < 0.5 { 1.0 } else { -1.0f32 }) + (if b.ph2 < 0.5 { 1.0 } else { -1.0f32 });
        let k = (900.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (sm - b.lp1);
        (b.lp1 * lvl * 0.022, 0.0)
    }

    /// Tracks the palette's own `base` (C5). The bonk's clash is defined as an
    /// interval AGAINST the voice it interrupts (`BONK_MINOR_SECOND` /
    /// `BONK_TRITONE` against this anchor), and the movement family sings in
    /// this register too, so an anchor left behind at an old `base` would stop
    /// clashing AND put scrubbing in a different octave from typing — the
    /// invariant `tone_tables_are_mutually_consonant_and_exclude_the_bonk_clash`
    /// pins the two moving together.
    fn anchor_hz(&self) -> f32 {
        523.25
    }
}

/// SPARKLE — glitter: two or three micro-chimes high in the register with
/// sharp attacks, shimmering twinkle-LFO decays and scattered pans — a
/// handful of glitter tossed per key.
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
        // A scatter of twinkling grains per key, each grain a round DONG —
        // C5, a soft 3 ms attack, a warm sub-octave body and a gentle glassy
        // halo.
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
        // A barely-there warm pad breathes under the grains (C4 pair,
        // slow-swelling soft octave), so the background is a glow, not a
        // void, and never a whine.
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

/// FIRE — the hearth: a soft crackle burst (band-passed noise with a random
/// centre) over a warm low ember thump that grows with heat. Jump = a
/// whoosh, the flame leaping. (Unpitched — the bonk keeps the default mid
/// register anchor.)
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
        // GENUINE CRACKLE: every key is an impulsive few-millisecond high-Q
        // SNAP — a wood fibre parting — over a woody ember knock.
        // Impulsiveness, not softness, is what separates fire from water.
        // Jump = the flame LEAPING: a dark low whoomph (no high splash —
        // that would be a wave) plus a scatter of spark snaps.
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
            // The snap. Backspace cracks darker (a duller, lower fibre),
            // typing cracks bright.
            let centre = if kind == SoundKind::Backspace {
                s.rnd_in(900.0, 1800.0)
            } else {
                s.rnd_in(2000.0, 4800.0)
            };
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
            // The ember: a short woody knock under the snap that only
            // really speaks when typing is hot — the log settling.
            let fe = s.rnd_in(85.0, 125.0);
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
        // Crackles in the embers: mostly bright impulsive snaps,
        // occasionally a low log-knock — never the soft mid-band plops
        // that read as dripping water.
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
        // The roar under the crackle — dark filtered noise whose loudness
        // FLICKERS fast and irregularly (~9 Hz random flutter), like flame
        // light. Flicker is the one cue that separates fire-body from wind
        // or surf: waves swell smoothly and slowly, flames gutter — a slow
        // LFO undulation here reads as waves.
        let white = {
            let mut x = s.rng;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            s.rng = x;
            (x >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
        };
        let b = &mut s.bed;
        let k = (380.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (white - b.lp1);
        // lp2 doubles as the flicker state: the same white draw through a
        // very low one-pole (~9 Hz) wanders irregularly; scaled around
        // unity it gutters the roar like flamelight.
        let kf = (9.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp2 += kf * (white - b.lp2);
        let flick = (1.0 + 26.0 * b.lp2).clamp(0.35, 1.8);
        (b.lp1 * lvl * 0.4 * flick, 0.0)
    }
}

/// LASER — the bolt: a fast exponential zap falling two octaves, with a
/// whisper of noise at the muzzle. Backspace = the zap reversed (rising).
/// Kept SOFT — the archetype at -26 dB.
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
        // THE LIGHTNING STRIKE: a two-octave dive with a brief electric
        // SIZZLE on the tone and a bright high-Q CRACK of air snapping over
        // it. Backspace = the zap reversed, crackless and softer. Jump = the
        // FULL STRIKE: crack, zap, sub-thump landing, then THUNDER — a long
        // low roll that sweeps down and echoes once. The archetype is kept
        // soft; thunder rumbles, it never booms.
        let f_hi = s.melody_hz(880.0, deg.min(5));
        let f_lo = f_hi * 0.25;
        let (a, b) = if kind == SoundKind::Backspace {
            (f_lo, f_hi)
        } else {
            (f_hi, f_lo)
        };
        let dur = if kind == SoundKind::Jump { 0.2 } else { 0.11 };
        let v = Voice {
            dur,
            attack: 0.001,
            decay: if kind == SoundKind::Jump { 0.08 } else { 0.045 },
            p: [
                Partial {
                    lvl: 0.7,
                    f0: a,
                    f1: b,
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
                    f0: a * 2.0,
                    f1: b * 2.0,
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
        if kind != SoundKind::Backspace {
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
        }
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
        // The storm keeps grumbling: mostly distant thunder rolls, now and
        // then the faint tick of a far-off strike too far away to hear
        // crack properly.
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
        // The storm on the horizon — deep filtered rumble whose level
        // swells and sags slowly and UNEVENLY (a ~1.3 Hz random wander, not
        // a metronome LFO): weather, not machinery.
        let white = {
            let mut x = s.rng;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            s.rng = x;
            (x >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
        };
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

/// BEAM — the photon tube: each key is only a glassy tick of the rod
/// (root+fifth dyad, very short) because the SIGNATURE lives in the bed's
/// sustained hum that swells behind typing and powers down after it —
/// scored to the tube's visual power-down.
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
        // THE ROCKET CONSOLE: every key is a button on a spacecraft panel —
        // a soft rubberized THUD (the button seating) and a muted low
        // confirmation blip settling downward. No glassy tick, no dyad: a
        // rod chime here feeds the bed hum. Backspace = the blip inverted
        // (a gentle un-press). Jump = ENGAGE: press, a slow rising two-tone
        // confirm, and a distant engine surge from below decks. Everything
        // at a whisper — the standing law: never annoying.
        let f = s.melody_hz(330.0, (deg / 2) * 2); // even degrees: stabler line
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
        // The console acknowledging: a muted blip settling onto its pitch
        // (backspace rises instead — "un-pressed").
        let (b0, b1) = if kind == SoundKind::Backspace {
            (f, f * 1.08)
        } else {
            (f * 1.06, f)
        };
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
            // ENGAGE: the two-tone confirm rising a fifth…
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
        // The ship, not the hum: the hull ambience of a rocket coasting
        // through deep space — very low filtered air (the engines, decks
        // away), NO tonal content at all, breathing slowly. A sustained
        // chord here is a whine no matter how soft. The power-down droop
        // darkens the engine note — the cutoff sinks as the energy dies, so
        // the ship audibly winds down with the tube's visual fade.
        let white = {
            let mut x = s.rng;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            s.rng = x;
            (x >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
        };
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

/// WATER — the droplet: a sine "plip" whose pitch FALLS like a real drop
/// striking a pool, sometimes answered by a tiny rising bubble. Backspace =
/// the drop in reverse (rising, the drop un-falling). Jump = a small splash:
/// noise + three scattered late droplets.
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
        // THE DROPLET, done the way water actually sounds: a soft surface
        // TAP and then the BLOOP — the collapsing air bubble's RISING chirp.
        // The rise is the cue: a falling pitch reads as a dry blip, not a
        // drop. A round attack and a low "gulp" partial keep it liquid.
        // Backspace = the bloop reversed (falling — the drop climbing back
        // out).
        let f = s.melody_hz(430.0, deg);
        let rev = kind == SoundKind::Backspace;
        let (a, b) = if rev {
            (f * 1.15, f * 0.8)
        } else {
            (f * 0.85, f * 1.25)
        };
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
        } else if !rev && s.rnd() < 0.18 {
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
        let white = {
            let mut x = s.rng;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            s.rng = x;
            (x >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
        };
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

/// COMET — ice: a glassy FM bell (slightly inharmonic modulator ⇒
/// crystalline, not churchy) with a long twinkling decay and a whisper of
/// "dust" hiss. Jump = a falling chime cluster, ice scattering off the
/// nucleus.
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
        // DEEP SPACE: every key is a soft TRANSMISSION — a low hollow tone
        // drifting downward a few cents (the doppler of something immense
        // passing far away), a faintly beating twin so the pair shimmers
        // darkly, a cold distant twelfth, and the whisper of ice dust.
        // Nothing clicks, nothing chimes. The excitement: rare SHOOTING
        // STARS off typed keys, and Jump = the full FLYBY — a doppler swoosh
        // with a scatter of SHORT crystal debris glints (icy, not churchy).
        // Backspace drifts UP: it recedes. (The deletion's lattice STEP comes
        // from `gesture_shape` via `deg`; this palette states the drift.)
        let f = s.melody_hz(220.0, deg); // A3 region — the void register
        let (f0, f1v) = if kind == SoundKind::Backspace {
            (f * 0.97, f * 1.03)
        } else {
            (f * 1.04, f * 0.97)
        };
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
            // The shooting star: a thin bright streak falling fast,
            // twinkling as it burns — quiet, gone in a blink, crossing to
            // the OTHER side of the stereo sky.
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
            // The flyby: band noise sweeping down through the void with the
            // tone falling under it — the nucleus passing close enough to
            // feel.
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
            // Debris glints: three SHORT crystal ticks scattered in the
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
        // The void — a deep pair beating once every ~2 s (the slow dark
        // pulse of empty space) under a cold fifth that breathes with the
        // slow LFO. Kept low deliberately: a ~1 kHz shimmer here reads as
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

/// MECH — a mechanical keyboard: every key is PERCUSSION, not a note. Two
/// layers per stroke: a tiny broadband switch CLICK (band-passed noise, a few
/// milliseconds) over a low damped case THOCK (a fast-falling fundamental
/// with knock noise — the bonk thump's cousin, softer and shorter). Pitch is
/// humanized a few Hz per key so a flood reads as fingers, not a machine gun.
/// Reached ONLY through the host's `trail_sound_style` override
/// ([`SoundVoice::Mech`]) — no [`GlowStyle`] binds here, so the nine shipped
/// style palettes keep their byte pins untouched. Unpitched by design: no
/// lattice membership to prove, and the default 330 Hz bonk anchor is right
/// (the doc on [`Palette::anchor_hz`] calls this out for unpitched
/// palettes). The bed is structurally silent — a keyboard has no weather.
struct MechPalette;

impl MechPalette {
    /// One keystroke: the switch click + the case thock. `body` is the
    /// thock's starting fundamental (falls to 55 % over the glide) and
    /// `bright` scales the click's band centre; both are pre-humanized by
    /// the caller so Typed / Backspace / Navigation share one voicing.
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
            SoundKind::Backspace => {
                // The mirrored stroke: a lower, duller thock (the big key
                // under the strong finger), click softened.
                let body = s.rnd_in(118.0, 136.0);
                Self::key(s, ev.pan, g * 0.9, body, 0.8, false);
            }
            SoundKind::Navigation => {
                // A whisper tick: the switch alone, no case body.
                let bright = s.rnd_in(0.85, 1.0);
                Self::key(s, ev.pan, g, 0.0, bright, true);
            }
            SoundKind::Jump => {
                // The flourish: a SPACEBAR/RETURN clunk — deeper, longer body
                // — plus two pre-delayed lighter ticks, so rapid line feeds
                // clatter (this palette's answer to the brrrring).
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
            // Kill / Poof / Glide / Sweep / Space / Shift are designed kind-level
            // before palette dispatch and never arrive here (trait doc);
            // Bonk and the riff route through their own designers.
            SoundKind::Kill
            | SoundKind::KillWord
            | SoundKind::Poof
            | SoundKind::Glide { .. }
            | SoundKind::Sweep { .. }
            | SoundKind::Land
            | SoundKind::Space
            | SoundKind::Shift => {}
        }
    }

    fn bed_sample(
        &self,
        _s: &mut TrailSynth,
        _dt: f32,
        _lvl: f32,
        _u1: f32,
        _u2: f32,
    ) -> (f32, f32) {
        // A keyboard has no weather: the mech bed is STRUCTURALLY silent
        // (exact zeros, not a quiet render), whatever `trail_sound_bed` says.
        (0.0, 0.0)
    }
}

/// TYPEWRITER — machinery, not music: every key is a dry SLUG CLACK (a
/// short broad-band paper-and-metal burst) over the PLATEN THUD (a fast-
/// falling low body with knock noise), with the typebar's own inharmonic
/// metal RING parked −10 dB under so the clack has a face without a squeal.
/// Enter is a CARRIAGE RETURN: the margin bell DINGS at the carriage's right,
/// the carriage ZIPS home and CLUNKS against the stop — the typewriter's
/// answer to the brrrring (rapid line feeds pile dings under the governor).
///
/// Reached ONLY through the picker ([`SoundVoice::Typewriter`]) — no
/// [`GlowStyle`] binds here, so the nine shipped palettes keep their byte
/// pins. Unpitched by design (the Mech/Fire ruling: no lattice membership to
/// prove; the default 330 Hz anchor is right for the bonk and the movement
/// family). The bed is structurally silent — a typewriter has no weather
/// either. Humanised per key (`b` clack brightness, `r` ring tuning, `body`
/// thud fundamental) so a flood reads as fingers, not a machine gun.
///
/// COMFORT (the owner's squeak/harshness axis, measured on `typing_voice_ab
/// --voice typewriter`, scenario `a-neutral`): ≤ 90 ms of energy per key, the
/// ring at −10 dB, `n_q 0.6` broad ("paper", not "glass"), lp 5200; the
/// figures ride the `palette_trim` comment.
struct TypewriterPalette;

impl TypewriterPalette {
    /// One slug strike: the CLACK (`bright` scales its band), the RING when
    /// `ring` (the typebar's metal, tuned by `r`), and the PLATEN THUD when
    /// `body > 0` (its starting fundamental, falling to 55 %). Typed /
    /// Backspace / Navigation share this one voicing, pre-humanised by the
    /// caller.
    fn strike(s: &mut TrailSynth, pan: f32, g: f32, bright: f32, ring: f32, body: f32) {
        // THE CLACK — 40 ms of band-passed paper-and-metal falling out of
        // 3.2 kHz (13 ms decay); the broad Q is what keeps it paper. It
        // LEADS the strike (spawn ×1.6): band-passed noise peaks ~17 dB under
        // a tonal voice at equal gain (the Kill arm's law), so a clack that
        // is to be heard as the identity — centroid 1.6 kHz, not Mech's 126 —
        // has to be spawned hot, and the platen kept low.
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
            // THE SLUG RING — an inharmonic pair (3150/4720, ratio 1.498…
            // no lattice interval) at fixed pitch: the typebar's metal, −10 dB
            // under the clack and dying with it (decay 22 ms), so it is a
            // face on the strike, never a note.
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
            // THE PLATEN THUD — the rubber roller taking the slug: a body
            // tone falling to 55 % over 25 ms with a knock of noise, kept
            // under 1.4 kHz so it is weight, not tone.
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
            SoundKind::Backspace => {
                // THE BACKSPACER LEVER — the deletion mirror law in TIMBRE
                // (the Fire/Mech form: no lattice to move): a duller clack,
                // a lower platen, and NO ring — the lever throws the carriage,
                // it strikes no slug.
                let body = s.rnd_in(138.0, 156.0);
                // (×1.5: the duller clack and the lower platen both peak
                // well under a slug strike; this holds the deletion at TIER
                // 1's "a shade under" rather than a hole in the texture.)
                Self::strike(s, ev.pan, g * 1.5, 0.6, 0.0, body);
            }
            SoundKind::Navigation => {
                // THE ESCAPEMENT "tik" — the clack alone, a shade duller: the
                // carriage stepping one space, no slug and no platen. Lifted
                // ×2 so the lone duller burst still reads as the tier-0
                // whisper rather than vanishing (it peaks well under a full
                // strike, having neither ring nor platen).
                Self::strike(s, ev.pan, g * 2.0, 0.75, 0.0, 0.0);
            }
            SoundKind::Jump => {
                // THE CARRIAGE RETURN. The margin bell first (the leading
                // voice, immediate — the ear's cue), hanging at the carriage's
                // right; then the carriage zips home and clunks on the stop.
                let ding = Voice {
                    dur: 0.45,
                    attack: 0.001,
                    decay: 0.14,
                    p: [
                        // The bell: C7 with an inharmonic FM strike face
                        // (2.71 — the house ICE idiom) that dies first.
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
                // The ZIP — the carriage flying home: 160 ms of noise falling
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
                // The CLUNK — the carriage against the left stop.
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
            // Kill / Glide / Sweep / Land / Space / Shift are designed
            // kind-level before palette dispatch and never arrive here
            // (trait doc).
            SoundKind::Kill
            | SoundKind::KillWord
            | SoundKind::Poof
            | SoundKind::Glide { .. }
            | SoundKind::Sweep { .. }
            | SoundKind::Land
            | SoundKind::Space
            | SoundKind::Shift => {}
        }
    }

    fn bed_sample(
        &self,
        _s: &mut TrailSynth,
        _dt: f32,
        _lvl: f32,
        _u1: f32,
        _u2: f32,
    ) -> (f32, f32) {
        // A typewriter has no weather: the bed is STRUCTURALLY silent (exact
        // zeros, not a quiet render), whatever `trail_sound_bed` says — the
        // same law as [`MechPalette`].
        (0.0, 0.0)
    }
}

/// MARIMBA — a warm rosewood bar under a yarn mallet: the fundamental sings
/// the melody note (grace-bent, the struck-idiophone ruling the glass bell
/// established — a bar doesn't slide, so the family bend is a third of the
/// depth and settled under the mallet), with the marimba's tuned second mode
/// two octaves up as its OWN voice that dies ~4× faster than the bar —
/// differential fate at onset makes the glint an auditory object of its
/// own, and the fundamental stays the strictly loudest partial (the family
/// tests select the loudest partial and it must land exactly on the melody
/// note). Enter is a mallet "ta-DA": the note with its fifth, then the
/// octave. Anchored at C4 (261.63) — the warm side, an octave under the bell.
///
/// Reached ONLY through the picker ([`SoundVoice::Marimba`]). The bed is
/// the resonator tubes' breath: two soft sines an octave under the anchor,
/// swelling on the slow LFO.
struct MarimbaPalette;

impl MarimbaPalette {
    /// The melodic base — the marimba's C4, one octave under the bell.
    const BASE_HZ: f32 = 261.63;

    /// One BAR strike on lattice degree `deg`: the bar voice (grace-bent onto
    /// its note from `dir`'s side, the yarn mallet's soft noise pad on it),
    /// plus the 4f GLINT when `glint`. `decay` is the bar's — Navigation
    /// dead-strokes it short.
    #[allow(clippy::too_many_arguments)]
    fn bar(
        s: &mut TrailSynth,
        deg: i32,
        dir: i8,
        delay: f32,
        decay: f32,
        glint: bool,
        g: f32,
        pan: f32,
    ) {
        let f = s.melody_hz(Self::BASE_HZ, deg);
        let (b0, b1) = gesture_bend(f, dir);
        // The GRACE bend on the fundamental only (the bar doesn't slide).
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
            // THE YARN MALLET — a soft, broad pad of noise falling out of
            // 700 Hz in 8 ms: the wrap on the mallet, not a click.
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
            // THE GLINT — the bar's tuned second mode, two octaves up, fixed
            // pitch, gone in ~30 ms while the bar rings for 130: the
            // marimba's characteristic "tok" of brightness at the strike.
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
        // `deg` already carries the family's deletion step (`gesture_shape`);
        // the palette states the TIMBRE, not the interval — a deletion is the
        // same bar one lattice step down, entered from above.
        let dir = gesture_shape(kind).dir;
        match kind {
            SoundKind::Typed | SoundKind::Backspace => {
                Self::bar(s, deg, dir, 0.0, 0.13, true, g, ev.pan);
            }
            SoundKind::Navigation => {
                // The DEAD STROKE — mallet held on the bar: the note, choked
                // in 50 ms, no glint (lifted ×1.25: the choke takes the peak
                // with it, and this is the tier-0 whisper, not a mute).
                Self::bar(s, deg, dir, 0.0, 0.05, false, g * 1.25, ev.pan);
            }
            SoundKind::Jump => {
                // "ta-DA": the note, then 70 ms behind its fifth (mirrored
                // pan) and octave struck together — the marimba's arrived
                // flourish, two mallets.
                Self::bar(s, deg, 1, 0.0, 0.13, true, g, ev.pan);
                Self::bar(s, deg + 3, 1, 0.07, 0.13, false, g * 0.4, -ev.pan);
                Self::bar(s, deg + 5, 1, 0.07, 0.13, true, g * 0.45, ev.pan * 0.5);
            }
            // Kill / Glide / Sweep / Land / Space / Shift are designed
            // kind-level before palette dispatch and never arrive here
            // (trait doc).
            SoundKind::Kill
            | SoundKind::KillWord
            | SoundKind::Poof
            | SoundKind::Glide { .. }
            | SoundKind::Sweep { .. }
            | SoundKind::Land
            | SoundKind::Space
            | SoundKind::Shift => {}
        }
    }

    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, u1: f32, _u2: f32) -> (f32, f32) {
        // THE RESONATORS' BREATH — the tubes under the bars, humming an
        // octave under the anchor (C3) with its fifth, swelling on the slow
        // LFO and lowpassed to a warmth. Mono (side 0): the tubes are one
        // instrument.
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

/// FELT — a felt-muted piano: the padded hammer's thud enriches the onset
/// (harmonic FM on the fundamental, index settling to a pure sine in 20 ms —
/// the knock, then the string), a dark 2f/3f pair for body, everything under
/// 1.3 kHz. The roster's hush voice: the darkest palette (energy over 2 kHz
/// under a tenth) at the lowest anchor (G3, 196.0). Enter is a soft broken
/// triad — the note with its bass octave, the fifth a beat behind. Bed: the
/// dampers lifted — two barely-detuned low strings sympathetically humming.
///
/// Reached ONLY through the picker ([`SoundVoice::Felt`]). Same grace bend
/// on the fundamental as the bell and the marimba: a struck string, entered
/// from the family's side, settled under the hammer.
struct FeltPalette;

impl FeltPalette {
    /// The melodic base — G3, the lowest voice in the roster.
    const BASE_HZ: f32 = 196.0;

    /// One felted note on lattice degree `deg`, entered from `dir`'s side.
    /// `thud` scales the hammer pad (0 for the una-corda whisper), `decay` is
    /// the string's.
    #[allow(clippy::too_many_arguments)]
    fn note(
        s: &mut TrailSynth,
        deg: i32,
        dir: i8,
        delay: f32,
        decay: f32,
        thud: f32,
        g: f32,
        pan: f32,
    ) {
        let f = s.melody_hz(Self::BASE_HZ, deg);
        let (b0, b1) = gesture_bend(f, dir);
        let f0 = b1 + (b0 - b1) * KEY_BELL_BEND_SHARE;
        let v = Voice {
            delay,
            dur: 0.6,
            attack: 0.004,
            decay,
            p: [
                // THE STRING — the law partial (strictly loudest, lands
                // exactly), the hammer's knock as a harmonic FM face that
                // settles to sine in ~20 ms.
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
            // THE FELT PAD — the hammer's soft thud, a low broad noise
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
        // `deg` already carries the family's deletion step; the palette
        // states the TIMBRE, not the interval.
        let dir = gesture_shape(kind).dir;
        match kind {
            SoundKind::Typed | SoundKind::Backspace => {
                Self::note(s, deg, dir, 0.0, 0.19, 1.0, g, ev.pan);
            }
            SoundKind::Navigation => {
                // UNA CORDA — the soft pedal's whisper: shorter, no hammer
                // thud, a shade under the keystroke (the tier-0 whisper).
                Self::note(s, deg, dir, 0.0, 0.09, 0.0, g * 0.8, ev.pan);
            }
            SoundKind::Jump => {
                // A soft BROKEN TRIAD: the note over its bass octave at once,
                // the fifth a beat behind — the felt's arrived flourish.
                Self::note(s, deg, 1, 0.0, 0.19, 1.0, g, ev.pan);
                Self::note(s, deg - 5, 1, 0.0, 0.22, 1.0, g * 0.7, ev.pan * 0.5);
                Self::note(s, deg + 3, 1, 0.06, 0.19, 0.5, g * 0.5, -ev.pan * 0.5);
            }
            // Kill / Glide / Sweep / Land / Space / Shift are designed
            // kind-level before palette dispatch and never arrive here
            // (trait doc).
            SoundKind::Kill
            | SoundKind::KillWord
            | SoundKind::Poof
            | SoundKind::Glide { .. }
            | SoundKind::Sweep { .. }
            | SoundKind::Land
            | SoundKind::Space
            | SoundKind::Shift => {}
        }
    }

    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, _u1: f32, _u2: f32) -> (f32, f32) {
        // THE DAMPERS LIFTED — two low strings (G2, barely detuned so they
        // breathe against each other every ~3 s) humming in sympathy, dark
        // under 400 Hz. Mono: one soundboard.
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
/// the interpolation error by four; this is the dial if the table is ever
/// judged audible (see [`sin01`]).
const SIN_TAB_BITS: u32 = 10;
const SIN_TAB_LEN: usize = 1 << SIN_TAB_BITS;

/// `sin(2π·i/1024)`, plus one wrap-around guard entry so the interpolation
/// never needs a second index wrap. 4 KB of `.rodata` — a fifth of the L1
/// footprint the voice array alone already has.
///
/// (`approx_constant` fires on the quarter-turn entries, which really are
/// `1/√2` — they are sine values, not a hand-typed constant; and
/// `excessive_precision` on the shortest f32 round-trip forms.)
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

/// `sin(2π·ph)` for phase in turns — a 1024-point table with linear
/// interpolation, in place of a full-precision scalar `sinf`.
///
/// ACCURACY, because this is the one place in the module that trades
/// exactness for speed. Linear interpolation of `sin(2π·x)` on a step
/// `h = 1/1024` has a worst-case error of `(2π)²·h²/8 = 4.71e-6`; measured
/// against the true sine across a dense sweep of the whole turn it is
/// **4.76e-6**, i.e. -106.5 dB relative to an oscillator at full scale. This
/// synth's own peak sits around -21 dBFS, so the residual lands near
/// -128 dBFS: below a 20-bit converter's floor and ~32 dB under the 16-bit
/// LSB. The error is periodic at 1024× the partial's own frequency, so it is
/// not broadband hiss but one fixed distortion product that folds back above
/// 15 kHz at that level, and it grows with neither time, polyphony nor level.
///
/// TIMING AND TUNING ARE UNTOUCHED. Every phase accumulator (`p.ph`,
/// `p.fm_ph`, `v.tw_ph`, the bed's) is unchanged, so pitch, phase, envelopes
/// and voice lifetimes are exactly where they were; only the WAVEFORM's last
/// bits move. The exposed places to listen before believing that are the long
/// quiet drones where one sine plays alone for seconds — Comet's 110 Hz pair,
/// Beam's hum.
#[inline]
fn sin01(ph: f32) -> f32 {
    // Call sites hand over `.fract()`ed phases in [0,1) — except the FM
    // carrier, whose modulation term pushes the argument outside — so wrap
    // here rather than assume. `x` can round to exactly 1.0 for a tiny
    // negative `ph`, which is why the index is masked and the fraction is
    // taken from the UNMASKED truncation: that case lands on `SIN_TAB[0]`
    // with a zero fraction, which is exactly `sin(2π)`.
    let x = ph - ph.floor();
    let z = x * SIN_TAB_LEN as f32;
    let j = z as usize;
    let f = z - j as f32;
    let i = j & (SIN_TAB_LEN - 1);
    let a = SIN_TAB[i];
    let b = SIN_TAB[i + 1];
    a + f * (b - a)
}

/// Triangle in turns, ±1.
#[inline]
fn tri(ph: f32) -> f32 {
    4.0 * (ph - 0.5).abs() - 1.0
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
            // Bed ON — the v0.56 oracle has no bed gate, so the byte pins
            // need it fed; the bed-off proofs build their own `bed: false`
            // events.
            bed: true,
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

    /// The bonk's two exclusive rub zones (minor-second crush, tritone band):
    /// shared by every consonance proof so the zone bounds cannot drift apart
    /// between tests.
    fn assert_outside_bonk_zones(raw: f32, ctx: &str) {
        let r = reduce(raw);
        let m2_zone = r > 1.0 + 1e-4 && r < 1.09;
        let tritone_zone = r > 1.395 && r < 1.43;
        assert!(
            !m2_zone && !tritone_zone,
            "{ctx}: interval {r} lands in a bonk rub zone"
        );
    }

    const STYLES: [GlowStyle; 9] = [
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

    /// EVERY VOICE the picker can select — [`SoundVoice::ALL`] verbatim, so a
    /// voice cannot join the roster without joining every voice-agnostic
    /// sweep below (onset, rise, ladder floor, decay, governor).
    const VOICES: &[SoundVoice] = SoundVoice::ALL;

    /// Every gesture the host can cue, in the order the loudness ladder
    /// names them. Shared by the touch-to-ear pins below so a new
    /// [`SoundKind`] cannot land without an onset proof.
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

    /// TOUCH-TO-EAR, THE SYNTH'S SHARE: exactly zero.
    ///
    /// The keystroke→sound budget is spent almost entirely OUTSIDE this file
    /// — the host's AudioQueue holds `BUFFER_COUNT × BUFFER_FRAMES` of
    /// already-rendered audio ahead of any cue — so the ONE thing the synth
    /// owes the ear is that a cue is audible in the very first buffer rendered
    /// after it, with nothing in front of it. The arpeggio idiom (flourishes
    /// are pre-delayed voices, no scheduler) makes that easy to lose by
    /// accident: give a palette's LEADING voice a `delay`, or design a
    /// gesture whose first note is a grace note, and every keystroke in that
    /// style silently gets slower with no test to say so. Four doc comments
    /// asserted this contract; nothing enforced it.
    ///
    /// Both halves are needed. The delay check states the intent structurally
    /// (`spawn` models pre-delay as a NEGATIVE onset time, so the earliest
    /// live voice reads its own delay straight back); the render check proves
    /// the intent survived into samples — a leading voice at `delay = 0` whose
    /// envelope or gain leaves the buffer empty is just as late to the ear.
    #[test]
    fn every_gesture_speaks_in_its_first_post_cue_buffer() {
        // The host's real block geometry (`trail_audio::BUFFER_FRAMES`).
        const BLOCK: usize = 512;
        // 0.33 ms. Three partials at independent random phases cannot all
        // hold a zero crossing this long, so the window forgives phase luck
        // while still failing any pre-delay worth a millisecond.
        const HEAD: usize = 16;
        for &voice in VOICES {
            for style in STYLES {
                for kind in KINDS {
                    let mut s = TrailSynth::new(48_000.0, 0x0A7E);
                    // Bed OFF: this pins the GESTURE's onset. The bed is a
                    // slew-limited swell with no onset to pin, and mixing it
                    // in would let ambience mask a late note.
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

    /// …and a PER-CHARACTER gesture's leading voice must RISE promptly, not
    /// merely start at sample zero. A voice can be first-buffer audible and
    /// still read as late if its envelope blooms: what the ear times is the
    /// rise, and past some point the attack IS the latency.
    ///
    /// Pinned on the envelope constant rather than on rendered level, and
    /// deliberately so. An exponential attack rises linearly at first, so a
    /// "time to −20 dB of peak" threshold barely moves when `attack` grows —
    /// it passes an eightfold slowdown unchanged. The knob is the honest
    /// thing to bound.
    ///
    /// The budget is 16 ms against a measured worst case of 12 ms today
    /// (Comet's bell and Sparkle's shimmer, both deliberate blooms): one step
    /// of headroom to retune a palette, nowhere near enough to turn a click
    /// into a fade-in.
    ///
    /// Per-character only. The TIER 3/4 gestures are punctuation — Kill's
    /// swoosh is a 20 ms-attack noise fall BY DESIGN, and Jump's and Land's
    /// peaks legitimately arrive in a later pre-delayed note. Their
    /// promptness claim is the first-buffer pin above, which they do keep.
    #[test]
    fn a_per_character_gesture_rises_within_sixteen_milliseconds() {
        const BUDGET_S: f32 = 0.016;
        for &voice in VOICES {
            for style in STYLES {
                for kind in [
                    SoundKind::Typed,
                    SoundKind::Backspace,
                    SoundKind::Glide { dir: 1 },
                ] {
                    let mut s = TrailSynth::new(48_000.0, 0x0A7E);
                    let mut e = ev(style, kind);
                    e.voice = voice;
                    e.bed = false;
                    s.push(e);
                    // The LEADING voice only (`t == 0.0` — pre-delayed
                    // flourishes sit at a negative onset time and are free to
                    // swell however the palette likes).
                    let attack = s
                        .voices
                        .iter()
                        .filter(|v| v.on && v.t == 0.0)
                        .map(|v| v.attack)
                        .fold(f32::MAX, f32::min);
                    assert!(
                        attack <= BUDGET_S,
                        "{voice:?}/{style:?}/{kind:?}: leading attack {attack} s exceeds \
                         the {BUDGET_S} s budget — past here the rise, not the delivery, \
                         is what the ear waits on"
                    );
                }
            }
        }
    }

    /// A fresh synth is quiet and renders exact zeros.
    #[test]
    fn silent_when_idle() {
        let mut s = TrailSynth::new(48_000.0, 7);
        assert!(s.is_quiet());
        let mut buf = [1.0f32; 512];
        s.render(&mut buf);
        assert!(buf.iter().all(|&x| x == 0.0));
    }

    /// Every style × gesture (all five trail kinds AND the word bonk)
    /// produces sound, stays in range, never NaNs, and decays back to exact
    /// silence.
    #[test]
    fn all_styles_sound_and_decay() {
        for style in STYLES {
            for gesture in [
                SoundGesture::Trail(SoundKind::Typed),
                SoundGesture::Trail(SoundKind::Backspace),
                SoundGesture::Trail(SoundKind::Navigation),
                SoundGesture::Trail(SoundKind::Kill),
                SoundGesture::Trail(SoundKind::Jump),
                SoundGesture::Words(WordGesture::Bonk),
            ] {
                let mut s = TrailSynth::new(48_000.0, 42);
                let mut e = ev(style, SoundKind::Typed);
                e.kind = gesture;
                s.push(e);
                let mut peak = 0.0f32;
                let mut buf = [0.0f32; 1024];
                // 6 s is enough for every decay + bed exhale.
                for _ in 0..(6 * 48_000 * 2 / 1024) {
                    s.render(&mut buf);
                    for &x in &buf {
                        assert!(x.is_finite(), "{style:?}/{gesture:?} produced non-finite");
                        peak = peak.max(x.abs());
                    }
                }
                assert!(
                    peak > 1e-4,
                    "{style:?}/{gesture:?} was inaudible (peak {peak})"
                );
                assert!(peak <= 0.98, "{style:?}/{gesture:?} clipped (peak {peak})");
                assert!(
                    s.is_quiet(),
                    "{style:?}/{gesture:?} never decayed to silence"
                );
                s.render(&mut buf);
                assert!(
                    buf.iter().all(|&x| x == 0.0),
                    "{style:?}/{gesture:?} quiet but nonzero"
                );
            }
        }
    }

    /// A [`SoundVoice::Mech`] event (the `trail_sound_style = "mechanical"`
    /// override) riding any visual style.
    fn mech(style: GlowStyle, kind: SoundKind) -> SoundEvent {
        SoundEvent {
            voice: SoundVoice::Mech,
            ..ev(style, kind)
        }
    }

    /// The default voice is the exact pre-override identity: `Style`.
    #[test]
    fn mech_voice_default_is_style() {
        assert_eq!(SoundVoice::default(), SoundVoice::Style);
    }

    /// The mech palette honours the same audibility/decay contract as every
    /// style palette (`all_styles_sound_and_decay`), across every gesture and
    /// regardless of which visual style the events ride.
    #[test]
    fn mech_voice_sounds_and_decays() {
        for style in [GlowStyle::Lumen, GlowStyle::RainbowKitty] {
            for gesture in [
                SoundGesture::Trail(SoundKind::Typed),
                SoundGesture::Trail(SoundKind::Backspace),
                SoundGesture::Trail(SoundKind::Navigation),
                SoundGesture::Trail(SoundKind::Kill),
                SoundGesture::Trail(SoundKind::Jump),
                SoundGesture::Trail(SoundKind::Glide { dir: 1 }),
                SoundGesture::Words(WordGesture::Bonk),
            ] {
                let mut s = TrailSynth::new(48_000.0, 42);
                let mut e = mech(style, SoundKind::Typed);
                e.kind = gesture;
                s.push(e);
                let mut peak = 0.0f32;
                let mut buf = [0.0f32; 1024];
                for _ in 0..(6 * 48_000 * 2 / 1024) {
                    s.render(&mut buf);
                    for &x in &buf {
                        assert!(x.is_finite(), "mech/{gesture:?} produced non-finite");
                        peak = peak.max(x.abs());
                    }
                }
                assert!(peak > 1e-4, "mech/{gesture:?} was inaudible (peak {peak})");
                assert!(peak <= 0.98, "mech/{gesture:?} clipped (peak {peak})");
                assert!(s.is_quiet(), "mech/{gesture:?} never decayed to silence");
                s.render(&mut buf);
                assert!(
                    buf.iter().all(|&x| x == 0.0),
                    "mech/{gesture:?} quiet but nonzero"
                );
            }
        }
    }

    /// The mech flood obeys the governor exactly like the style palettes
    /// (mirrors `flood_is_ducked`'s absolute ceiling).
    #[test]
    fn mech_flood_is_ducked() {
        let mut s = TrailSynth::new(48_000.0, 7);
        let mut buf = [0.0f32; 960];
        let mut peak = 0.0f32;
        // 3 s at 25 keys/s: push one Typed every 40 ms, render 10 ms blocks.
        for i in 0..75 {
            s.push(mech(GlowStyle::Lumen, SoundKind::Typed));
            let _ = i;
            for _ in 0..4 {
                s.render(&mut buf);
                for &x in &buf {
                    assert!(x.is_finite());
                    peak = peak.max(x.abs());
                }
            }
        }
        assert!(peak < 0.5, "mech flood must stay governed (peak {peak})");
    }

    /// Same (seed, events) mech script ⇒ bit-identical output, humanization
    /// included (the per-key rnd draws ride the deterministic xorshift).
    #[test]
    fn mech_is_deterministic() {
        let run = || {
            let mut s = TrailSynth::new(48_000.0, 0xBEEF);
            let mut out = Vec::new();
            let mut buf = [0.0f32; 960];
            for kind in [
                SoundKind::Typed,
                SoundKind::Backspace,
                SoundKind::Jump,
                SoundKind::Navigation,
            ] {
                s.push(mech(GlowStyle::Water, kind));
                for _ in 0..12 {
                    s.render(&mut buf);
                    out.extend_from_slice(&buf);
                }
            }
            out
        };
        let (a, b) = (run(), run());
        assert!(
            a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()),
            "mech render must be deterministic"
        );
    }

    /// The mech bed is STRUCTURALLY silent: with `bed: true` events feeding
    /// energy, the mix is bit-identical to the same script with `bed: false`
    /// — the bed mixer contributes exact zeros, not a quiet texture.
    #[test]
    fn mech_bed_is_structurally_silent() {
        let run = |bed: bool| {
            let mut s = TrailSynth::new(48_000.0, 99);
            let mut out = Vec::new();
            let mut buf = [0.0f32; 960];
            for _ in 0..20 {
                let mut e = mech(GlowStyle::RainbowKitty, SoundKind::Typed);
                e.bed = bed;
                s.push(e);
                for _ in 0..4 {
                    s.render(&mut buf);
                    out.extend_from_slice(&buf);
                }
            }
            out
        };
        let (on, off) = (run(true), run(false));
        assert!(
            on.iter().zip(&off).all(|(x, y)| x.to_bits() == y.to_bits()),
            "the mech bed must contribute exactly zero samples"
        );
    }

    // -- the typing-sound picker: every voice, by name --------------------

    /// The same trail event under an explicit typing-sound VOICE.
    fn voiced(voice: SoundVoice, style: GlowStyle, kind: SoundKind) -> SoundEvent {
        SoundEvent {
            voice,
            ..ev(style, kind)
        }
    }

    /// The three sound-only instruments — the voices with palettes of their
    /// OWN (the nine `Of(_)` voices alias the style table, proven verbatim
    /// below; Mech has its own quintet above).
    const NEW_VOICES: [SoundVoice; 3] = [
        SoundVoice::Typewriter,
        SoundVoice::Marimba,
        SoundVoice::Felt,
    ];

    /// THE ROSTER ROUND-TRIPS: every voice's canonical name parses back to
    /// itself, every documented alias lands on its listed voice, `auto` is
    /// the default identity, and no Trail-Pack `Of(Custom)` is offered — a
    /// pack is a look, not a sound (its name answers as the palette it rides,
    /// Lumen's).
    #[test]
    fn voice_roster_round_trips() {
        assert_eq!(SoundVoice::parse("auto"), Some(SoundVoice::Style));
        assert_eq!(
            SoundVoice::ALL[0],
            SoundVoice::Style,
            "auto leads the picker"
        );
        for &v in SoundVoice::ALL {
            assert_eq!(SoundVoice::parse(v.name()), Some(v), "{v:?}");
            // Case- and whitespace-insensitive, like every other enum key.
            assert_eq!(
                SoundVoice::parse(&format!("  {}  ", v.name().to_ascii_uppercase())),
                Some(v)
            );
            assert!(
                v.name().chars().all(|c| c.is_ascii_lowercase() || c == ' '),
                "{v:?}: canonical spellings are lowercase words ({:?})",
                v.name()
            );
        }
        // Names are unique (the picker cannot show one spelling twice).
        for (i, a) in SoundVoice::ALL.iter().enumerate() {
            for b in &SoundVoice::ALL[i + 1..] {
                assert_ne!(a.name(), b.name(), "{a:?} vs {b:?}");
            }
        }
        for &(alias, voice) in SoundVoice::ALIASES {
            assert_eq!(SoundVoice::parse(alias), Some(voice), "alias {alias:?}");
            assert!(
                SoundVoice::ALL.iter().all(|v| v.name() != alias),
                "alias {alias:?} shadows a canonical name"
            );
        }
        assert!(
            !SoundVoice::ALL.contains(&SoundVoice::Of(GlowStyle::Custom)),
            "a Trail Pack is a look, not a sound"
        );
        assert_eq!(SoundVoice::Of(GlowStyle::Custom).name(), "warm pluck");
        assert_eq!(
            SoundVoice::parse("warm pluck"),
            Some(SoundVoice::Of(GlowStyle::Lumen))
        );
        for junk in ["", "  ", "plasma", "glass", "auto bell", "pack:foo"] {
            assert_eq!(SoundVoice::parse(junk), None, "{junk:?}");
        }
        assert_eq!(SoundVoice::default(), SoundVoice::Style);
    }

    /// A STANDALONE STYLE VOICE IS THE STYLE'S PALETTE, VERBATIM. `Of(s)`
    /// under any look renders bit-identical to `Style` under look `s` — every
    /// gesture kind (Kill's tint and the bed included), so the nine shipped
    /// palettes reach the picker as aliases of the pinned table, never as a
    /// second copy that could drift.
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
            // The bonk carries the voice too (its clash REGISTER follows the
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
        for style in STYLES {
            let reference = run(SoundVoice::Style, style);
            // Under a DIFFERENT look (Lumen, or Comet when the style is
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
            assert_eq!(
                kill_swoosh_band(SoundVoice::Of(style), other),
                kill_swoosh_band(SoundVoice::Style, style)
            );
            assert_eq!(
                palette_trim(SoundVoice::Of(style), other),
                palette_trim(SoundVoice::Style, style)
            );
        }
    }

    /// EVERY VOICE ON THE LADDER FLOOR — the isolated keystroke's peak on
    /// `mix_meter`'s exact stance (seed 0x5EED_1234, gain 0.4, heat 0.5, pan
    /// 0, bed off, 2.4 s tail). The picker's own voices (Mech and the three
    /// instruments) are fitted to −21.0 dBFS and held within ±0.5 dB.
    ///
    /// The nine style palettes are PINNED AT THEIR MEASURED VALUES, which is
    /// a REPORTED FINDING, not a retune: `palette_trim`'s doc claims each of
    /// them lands on −21.0 (fitted as a 24-seed mean, an earlier ladder
    /// pass), but on this single-seed stance only rainbow kitty (re-fitted
    /// 2026-08-16), water, phaser and beam sit within half a dB of it —
    /// lumen (−19.3), comet (−18.4), fire (−18.7), laser (−19.2) read hot
    /// and sparkle (−23.1) cold. Those palettes are byte-pinned by the
    /// `v056_reference` oracle; their trims are shared with it and were not
    /// moved here (the picker exposes them, it does not re-voice them). This
    /// pin makes any future drift a stated event.
    #[test]
    fn every_voice_lands_typed_on_the_ladder_floor() {
        fn peak_db(voice: SoundVoice) -> f32 {
            let mut s = TrailSynth::new(48_000.0, 0x5EED_1234);
            let mut e = voiced(voice, GlowStyle::RainbowKitty, SoundKind::Typed);
            e.hue = 0.0;
            e.bed = false;
            s.push(e);
            let mut buf = vec![0.0f32; (48_000.0f32 * 2.4) as usize * CHANNELS];
            s.render(&mut buf);
            20.0 * buf.iter().fold(0.0f32, |m, v| m.max(v.abs())).log10()
        }
        // (voice, expected peak dBFS)
        let table: [(SoundVoice, f32); 14] = [
            (SoundVoice::Style, -21.0), // rides the RainbowKitty look here
            (SoundVoice::Of(GlowStyle::RainbowKitty), -21.0),
            (SoundVoice::Of(GlowStyle::Lumen), -19.34),
            (SoundVoice::Of(GlowStyle::Sparkle), -23.10),
            (SoundVoice::Of(GlowStyle::Comet), -18.41),
            (SoundVoice::Of(GlowStyle::Water), -21.10),
            (SoundVoice::Of(GlowStyle::Phaser), -20.96),
            (SoundVoice::Of(GlowStyle::Laser), -19.20),
            (SoundVoice::Of(GlowStyle::Beam), -20.46),
            (SoundVoice::Of(GlowStyle::Fire), -18.73),
            (SoundVoice::Mech, -21.34),
            (SoundVoice::Typewriter, -21.0),
            (SoundVoice::Marimba, -21.0),
            (SoundVoice::Felt, -21.0),
        ];
        assert_eq!(
            table.len(),
            SoundVoice::ALL.len(),
            "every voice is measured"
        );
        for (voice, expect) in table {
            assert!(SoundVoice::ALL.contains(&voice));
            let got = peak_db(voice);
            assert!(
                (got - expect).abs() <= 0.5,
                "{voice:?} ({}): Typed peaks at {got:.2} dBFS, expected {expect:.2} ± 0.5",
                voice.name()
            );
        }
    }

    /// EVERY PITCHED INSTRUMENT LANDS ITS KEYSTROKE ON THE MELODY NOTE in its
    /// own register — the surviving half of the family's first claim on the
    /// two picker-only pitched voices. (Its other half — "and mirrors it a
    /// lattice step down for a deletion" — is retired: a deletion is a poof
    /// now, proven by `the_deletion_is_a_poof_and_only_a_poof`, and the
    /// arithmetic this used to assert is exactly what the owner reported as
    /// indistinguishable from typing.)
    #[test]
    fn every_pitched_voice_lands_on_the_melody_note() {
        for voice in [SoundVoice::Marimba, SoundVoice::Felt] {
            let (ts, typed) = family_voices_in(voice, GlowStyle::Lumen, SoundKind::Typed);
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

    /// The typewriter's SLUG STRIKE is clack + ring + platen — and its
    /// deletion is none of them: the poof replaces the whole voice, so the
    /// lever throws no slug at all. (This test used to assert the deletion's
    /// own darker platen and dull clack; those voices no longer exist, and
    /// what remains worth pinning is that the strike still has all three and
    /// the erase borrows none of them.)
    #[test]
    fn the_typewriter_strike_keeps_its_three_voices_and_lends_none_to_the_erase() {
        let spawned = |kind: SoundKind| {
            let mut s = TrailSynth::new(48_000.0, 0x7E57);
            let mut e = voiced(SoundVoice::Typewriter, GlowStyle::Lumen, kind);
            e.bed = false;
            s.push(e);
            s.voices
                .iter()
                .filter(|v| v.on)
                .copied()
                .collect::<Vec<_>>()
        };
        let typed = spawned(SoundKind::Typed);
        let back = spawned(SoundKind::Backspace);
        assert_eq!(typed.len(), 3, "a slug strike is clack + ring + platen");
        assert_eq!(back.len(), 2, "the erase is the poof's body + air cap");
        let ring = |vs: &[Voice]| {
            vs.iter()
                .filter(|v| v.p[0].lvl > 0.0 && v.p[1].lvl > 0.0)
                .count()
        };
        assert_eq!(
            ring(&typed),
            1,
            "the strike carries the typebar's metal pair"
        );
        assert_eq!(ring(&back), 0, "the erase carries no tone at all");
        assert!(
            back.iter().all(|v| v.p.iter().all(|p| p.lvl <= 0.0)),
            "…not one tonal partial anywhere in it"
        );
    }

    /// Every new instrument honours the audibility/decay contract of every
    /// style palette (`all_styles_sound_and_decay`), across every gesture —
    /// the Bonk and a Glide included — whatever look the events ride.
    #[test]
    fn new_voices_sound_and_decay() {
        for voice in NEW_VOICES {
            for style in [GlowStyle::Lumen, GlowStyle::RainbowKitty] {
                for gesture in [
                    SoundGesture::Trail(SoundKind::Typed),
                    SoundGesture::Trail(SoundKind::Backspace),
                    SoundGesture::Trail(SoundKind::Navigation),
                    SoundGesture::Trail(SoundKind::Kill),
                    SoundGesture::Trail(SoundKind::Jump),
                    SoundGesture::Trail(SoundKind::Glide { dir: 1 }),
                    SoundGesture::Words(WordGesture::Bonk),
                ] {
                    let mut s = TrailSynth::new(48_000.0, 42);
                    let mut e = voiced(voice, style, SoundKind::Typed);
                    e.kind = gesture;
                    s.push(e);
                    let mut peak = 0.0f32;
                    let mut buf = [0.0f32; 1024];
                    for _ in 0..(6 * 48_000 * 2 / 1024) {
                        s.render(&mut buf);
                        for &x in &buf {
                            assert!(x.is_finite(), "{voice:?}/{gesture:?} produced non-finite");
                            peak = peak.max(x.abs());
                        }
                    }
                    assert!(
                        peak > 1e-4,
                        "{voice:?}/{gesture:?} was inaudible (peak {peak})"
                    );
                    assert!(peak <= 0.98, "{voice:?}/{gesture:?} clipped (peak {peak})");
                    assert!(
                        s.is_quiet(),
                        "{voice:?}/{gesture:?} never decayed to silence"
                    );
                    s.render(&mut buf);
                    assert!(
                        buf.iter().all(|&x| x == 0.0),
                        "{voice:?}/{gesture:?} quiet but nonzero"
                    );
                }
            }
        }
    }

    /// Every new instrument's flood obeys the governor exactly like the
    /// style palettes (mirrors `flood_is_ducked`'s absolute ceiling).
    #[test]
    fn new_voices_flood_is_ducked() {
        for voice in NEW_VOICES {
            let mut s = TrailSynth::new(48_000.0, 7);
            let mut buf = [0.0f32; 960];
            let mut peak = 0.0f32;
            for _ in 0..75 {
                s.push(voiced(voice, GlowStyle::Lumen, SoundKind::Typed));
                for _ in 0..4 {
                    s.render(&mut buf);
                    for &x in &buf {
                        assert!(x.is_finite());
                        peak = peak.max(x.abs());
                    }
                }
            }
            assert!(
                peak < 0.5,
                "{voice:?} flood must stay governed (peak {peak})"
            );
        }
    }

    /// Same (seed, events) script ⇒ bit-identical output for every new
    /// instrument, humanisation included.
    #[test]
    fn new_voices_are_deterministic() {
        for voice in NEW_VOICES {
            let run = || {
                let mut s = TrailSynth::new(48_000.0, 0xBEEF);
                let mut out = Vec::new();
                let mut buf = [0.0f32; 960];
                for kind in [
                    SoundKind::Typed,
                    SoundKind::Backspace,
                    SoundKind::Jump,
                    SoundKind::Navigation,
                ] {
                    s.push(voiced(voice, GlowStyle::Water, kind));
                    for _ in 0..12 {
                        s.render(&mut buf);
                        out.extend_from_slice(&buf);
                    }
                }
                out
            };
            let (a, b) = (run(), run());
            assert!(
                a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()),
                "{voice:?} render must be deterministic"
            );
        }
    }

    /// THE BEDS: a keyboard has no weather (Typewriter's bed is structurally
    /// silent, exact zeros like Mech's), while the two instruments carry a
    /// DESIGNED bed — feeding it changes the render.
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
        let same = |a: &[f32], b: &[f32]| a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits());
        assert!(
            same(
                &run(SoundVoice::Typewriter, true),
                &run(SoundVoice::Typewriter, false)
            ),
            "the typewriter bed must contribute exactly zero samples"
        );
        for voice in [SoundVoice::Marimba, SoundVoice::Felt] {
            assert!(
                !same(&run(voice, true), &run(voice, false)),
                "{voice:?} has a designed bed: feeding it must change the render"
            );
        }
    }

    /// The Kill swoosh's tint is voice-first and exhaustive: the four
    /// non-style voices each fall through their own band, `Style` keeps the
    /// pre-picker table (byte-pinned by the oracle), and the tints are
    /// distinct so the picker's keyboards and instruments are told apart on
    /// a line kill too.
    #[test]
    fn kill_swoosh_band_is_voice_first() {
        let bands = [
            kill_swoosh_band(SoundVoice::Mech, GlowStyle::Sparkle),
            kill_swoosh_band(SoundVoice::Typewriter, GlowStyle::Sparkle),
            kill_swoosh_band(SoundVoice::Marimba, GlowStyle::Sparkle),
            kill_swoosh_band(SoundVoice::Felt, GlowStyle::Sparkle),
        ];
        for (i, a) in bands.iter().enumerate() {
            assert!(a.0 > a.1, "a swoosh falls: {a:?}");
            for b in &bands[i + 1..] {
                assert_ne!(a, b);
            }
            // The look does not leak in.
            assert_ne!(*a, kill_swoosh_band(SoundVoice::Style, GlowStyle::Sparkle));
        }
        assert_eq!(
            kill_swoosh_band(SoundVoice::Mech, GlowStyle::Lumen),
            (1000.0, 220.0)
        );
        assert_eq!(
            kill_swoosh_band(SoundVoice::Style, GlowStyle::Water),
            (900.0, 250.0)
        );
        assert_eq!(
            kill_swoosh_band(SoundVoice::Style, GlowStyle::Fire),
            (1400.0, 300.0)
        );
        assert_eq!(
            kill_swoosh_band(SoundVoice::Style, GlowStyle::Sparkle),
            (2600.0, 700.0)
        );
        assert_eq!(
            kill_swoosh_band(SoundVoice::Style, GlowStyle::Comet),
            (1200.0, 280.0)
        );
        assert_eq!(
            kill_swoosh_band(SoundVoice::Style, GlowStyle::Lumen),
            (1600.0, 350.0)
        );
    }

    /// A sustained 25 cps flood must NOT get louder than a single event —
    /// the governor's whole job. (Peak here is texture + bed, capped well
    /// under the soft-clip knee.)
    #[test]
    fn flood_is_ducked() {
        for style in STYLES {
            // Single event peak…
            let mut s1 = TrailSynth::new(48_000.0, 3);
            s1.push(ev(style, SoundKind::Typed));
            let mut single = 0.0f32;
            let mut buf = [0.0f32; 960];
            for _ in 0..100 {
                s1.render(&mut buf);
                for &x in &buf {
                    single = single.max(x.abs());
                }
            }
            // …vs a 3-second 25 cps flood.
            let mut s2 = TrailSynth::new(48_000.0, 3);
            let mut flood = 0.0f32;
            for i in 0..75 {
                let mut e = ev(style, SoundKind::Typed);
                e.pan = ((i % 20) as f32) / 10.0 - 1.0;
                s2.push(e);
                for _ in 0..2 {
                    s2.render(&mut buf); // 2×960 frames = 40 ms per key
                    for &x in &buf {
                        flood = flood.max(x.abs());
                    }
                }
            }
            assert!(
                // The absolute slack is an OUTPUT-unit allowance, so it must ride
                // MASTER: changing the master scale scales both `flood` and
                // `single`, but would otherwise leave this term fixed and
                // silently tighten the bound (0.05 at the historical 0.9).
                flood <= single * 2.5 + MASTER * (0.05 / 0.9),
                "{style:?} flood peak {flood} vs single {single} — governor failed"
            );
            assert!(
                flood < 0.5,
                "{style:?} flood absolute peak too hot: {flood}"
            );
        }
    }

    /// BACKLOG DEPTH CANNOT BUY A CLICK. It is the governor, not the glow
    /// engine's cue backlog, that decides how many clicks a stalled frame
    /// delivers: `since_voice` advances only with RENDERED samples, and a
    /// drain pushes the whole backlog inside one host tick with no render
    /// between — so every cue after the first shares one audio instant, sits
    /// inside [`MIN_GAP`], and is thinned to silence INSIDE the synth. A
    /// 24-deep backlog is audibly identical to a 1-deep one, so neither
    /// growing the cap nor value-ranking its drop policy buys a click: a
    /// newcomer still lands at the tail of the same batch and is thinned by
    /// whatever preceded it.
    ///
    /// (The bypass kinds — Jump/Sweep/Land/Bonk/riff — are the deliberate
    /// exception and are asserted separately; a keystroke is not one of them.)
    #[test]
    fn a_drained_cue_batch_speaks_once_however_deep_the_backlog() {
        for style in STYLES {
            let one = {
                let mut s = TrailSynth::new(48_000.0, 11);
                s.push(ev(style, SoundKind::Typed));
                s.live_voices()
            };
            assert!(one > 0, "{style:?}: a lone keystroke must speak");
            // 8 = today's MAX_SOUND_CUES; 24 = any cap a "size it for the worst
            // burst" fix would reach for. Neither adds a voice.
            for depth in [8usize, 24] {
                let mut s = TrailSynth::new(48_000.0, 11);
                for _ in 0..depth {
                    s.push(ev(style, SoundKind::Typed));
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

    /// THE MEASUREMENT behind the key-time click's timbre prediction: is a
    /// one-keystroke heat lag audible? `heat` rides the note level as
    /// `0.55 + 0.45·heat` and the ember/thump layer as
    /// `0.1 + 0.28·heat`, so one missing `HEAT_GAIN` step
    /// (0.16, the glow engine's per-keystroke charge at full cadence) is a
    /// systematic level error through the whole cold→hot ramp of every typing
    /// burst — not a one-off.
    ///
    /// Measured, not asserted. Rendered spread across the palettes: 1.06-1.49
    /// dB from a cold start (Fire highest — its ember layer carries more of the
    /// heat term) and 0.81-1.19 dB mid-ramp. The bound below is the floor of
    /// that. ~0.8 dB sits right at the loudness JND, and it lands on EVERY
    /// click of the ramp in the same direction, which is what makes it read as
    /// "the sound is behind" rather than as one quiet note.
    #[test]
    fn one_keystroke_of_heat_lag_is_audible() {
        // Bed OFF isolates the discrete note (the click itself) — the bed's
        // slow texture would smear the comparison.
        let peak_at = |style, heat: f32| {
            let mut s = TrailSynth::new(48_000.0, 5);
            s.push(SoundEvent {
                heat,
                bed: false,
                ..ev(style, SoundKind::Typed)
            });
            let mut buf = [0.0f32; 960];
            let mut peak = 0.0f32;
            for _ in 0..100 {
                s.render(&mut buf);
                for &x in &buf {
                    peak = peak.max(x.abs());
                }
            }
            peak
        };
        // The glow engine's HEAT_GAIN at full typing cadence.
        const STEP: f32 = 0.16;
        for style in STYLES {
            // Cold start (the loudest part of the ramp) and mid-ramp.
            for base in [0.0f32, 0.4] {
                let (lag, live) = (peak_at(style, base), peak_at(style, base + STEP));
                let db = 20.0 * (live / lag).log10();
                assert!(
                    db >= 0.75,
                    "{style:?} @ heat {base}: one keystroke of heat lag is only \
                     {db:.2} dB ({lag} -> {live}) — if this ever goes inaudible, \
                     `cue_keystroke`'s timbre prediction can be retired"
                );
            }
        }
    }

    /// BEDS ARE OFF BY DEFAULT: events carrying `bed: false` — what every
    /// host event carries under the `trail_sound_bed` default — never
    /// energise the bed layer, so
    /// the bed mixer contributes exactly ZERO samples (structurally: the
    /// level floor never lifts, grains never arm — not a gain-0 render),
    /// while the discrete notes still speak at full identity; and the very
    /// next `bed: true` event (the setting flipped on) re-energises it.
    #[test]
    fn bed_off_events_never_energise_the_bed_and_the_setting_reenables() {
        for style in STYLES {
            let mut s = TrailSynth::new(48_000.0, 7);
            let bedless = SoundEvent {
                bed: false,
                ..ev(style, SoundKind::Typed)
            };
            s.push(bedless);
            assert_eq!(
                s.debug_bed(),
                (0.0, 0.0),
                "{style:?}: a bed-off event feeds nothing"
            );
            let mut buf = [0.0f32; 960];
            let mut peak = 0.0f32;
            // 6 s: every discrete voice decays fully (the audibility test's
            // proven bound) — with no bed there is nothing left after.
            for _ in 0..(6 * 48_000 * 2 / 960) {
                s.render(&mut buf);
                for &x in &buf {
                    peak = peak.max(x.abs());
                }
            }
            assert!(
                peak > 1e-4,
                "{style:?}: the note itself must still speak with the bed off"
            );
            assert_eq!(
                s.debug_bed(),
                (0.0, 0.0),
                "{style:?}: the bed floor never lifts across the whole render"
            );
            assert!(
                s.is_quiet(),
                "{style:?}: no bed tail may hold the synth awake"
            );
            s.render(&mut buf);
            assert!(
                buf.iter().all(|&x| x == 0.0),
                "{style:?}: bed-off silence is exact digital zero"
            );

            // The setting flips ON: the very next keystroke feeds the bed.
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

    /// TOURNAMENT CONSONANCE LAW: every bed-candidate pitch is an integer
    /// degree on the ACTIVE tone lattice, so for every tone table, every
    /// (bed tone × bed tone) and (bed tone × melody-walk note) interval —
    /// octave-reduced, inversions included — stays outside the bonk's
    /// minor-second and tritone rub zones. The zone predicate is the shared
    /// `assert_outside_bonk_zones`, as in
    /// `tone_tables_are_mutually_consonant_and_exclude_the_bonk_clash` (that
    /// test also proves the zones non-vacuous via the bonk's own ratios).
    /// Plus the C3 voicing pin: SHIMMER truly has no low fundamental.
    #[test]
    fn bed_variant_pitches_stay_on_the_active_lattice_for_every_tone() {
        // Union of every candidate's lattice degrees (C1 chords, C2 pad,
        // C3 wash — C0/C4 have no candidate pitches).
        let mut bed_degrees: Vec<i32> = Vec::new();
        for root in CHORD_DRIFT_ROOTS {
            for off in CHORD_DRIFT_STACK {
                bed_degrees.push(root + off);
            }
        }
        bed_degrees.extend(BREATH_DEGREES);
        bed_degrees.extend(SHIMMER_DEGREES);
        // Melody notes the beds must sit under: the walk's clamped range
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
            // C3's thesis is structural: its lowest partial sits ≥ 4× the
            // palette anchor — there is no low fundamental at all.
            for d in SHIMMER_DEGREES {
                assert!(
                    s.melody_hz(330.0, d) >= 330.0 * 3.9,
                    "{tone:?}: shimmer degree {d} dips into fundamental territory"
                );
            }
        }
    }

    /// TOURNAMENT LOUDNESS LAW: every candidate rides the shared
    /// energy/level/gain bed machinery, so the flood-duck bound of
    /// `flood_is_ducked` holds per candidate — a 25 cps flood under ANY bed
    /// design never gets meaningfully louder than a single event, and never
    /// approaches the clip ceiling.
    #[test]
    fn every_bed_variant_keeps_the_flood_duck_law() {
        for variant in BedVariant::ALL {
            // Single event peak…
            let mut s1 = TrailSynth::new(48_000.0, 3);
            s1.set_bed_variant(variant);
            s1.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
            let mut single = 0.0f32;
            let mut buf = [0.0f32; 960];
            for _ in 0..100 {
                s1.render(&mut buf);
                for &x in &buf {
                    single = single.max(x.abs());
                }
            }
            // …vs a 3-second 25 cps flood.
            let mut s2 = TrailSynth::new(48_000.0, 3);
            s2.set_bed_variant(variant);
            let mut flood = 0.0f32;
            for i in 0..75 {
                let mut e = ev(GlowStyle::RainbowKitty, SoundKind::Typed);
                e.pan = ((i % 20) as f32) / 10.0 - 1.0;
                s2.push(e);
                for _ in 0..2 {
                    s2.render(&mut buf); // 2×960 frames = 40 ms per key
                    for &x in &buf {
                        flood = flood.max(x.abs());
                    }
                }
            }
            assert!(
                // The same MASTER-riding output-unit slack as `flood_is_ducked`.
                flood <= single * 2.5 + MASTER * (0.05 / 0.9),
                "{variant:?} flood peak {flood} vs single {single} — governor failed"
            );
            assert!(
                flood < 0.5,
                "{variant:?} flood absolute peak too hot: {flood}"
            );
        }
    }

    /// HARNESS DETERMINISM at the engine level, per candidate: the same
    /// (seed, events, variant) script renders bit-identically — every
    /// candidate modulation runs off the sample-driven variant clock, no
    /// extra rng draws — and every candidate decays to EXACT silence like
    /// the shipping beds (same energy/level floor snap), so `is_quiet`
    /// still lets the host pause the queue under any bed design.
    #[test]
    fn bed_variants_render_deterministically_and_decay_to_exact_silence() {
        for variant in BedVariant::ALL {
            let run = || {
                let mut s = TrailSynth::new(48_000.0, 0xBED_5EED);
                s.set_bed_variant(variant);
                let mut acc = 0u64;
                let mut buf = [0.0f32; 512];
                for i in 0..40 {
                    if i % 2 == 0 {
                        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
                    }
                    s.render(&mut buf);
                    for &x in &buf {
                        acc = acc.rotate_left(7).wrapping_add(u64::from(x.to_bits()));
                    }
                }
                // 6 s tail: the audibility test's proven decay bound.
                for _ in 0..(6 * 48_000 * 2 / 512) {
                    s.render(&mut buf);
                }
                assert!(s.is_quiet(), "{variant:?} never decayed to silence");
                s.render(&mut buf);
                assert!(
                    buf.iter().all(|&x| x == 0.0),
                    "{variant:?} quiet but nonzero"
                );
                acc
            };
            assert_eq!(run(), run(), "{variant:?} must replay bit-exactly");
        }
    }

    /// The tournament is non-vacuous: the five candidates are pairwise
    /// distinct textures under an identical melody script (bit-hash over
    /// the mixed render — the only degree of freedom is the bed design).
    #[test]
    fn bed_candidates_are_pairwise_distinct_textures() {
        let render_hash = |variant: BedVariant| {
            let mut s = TrailSynth::new(48_000.0, 0xD15C);
            s.set_bed_variant(variant);
            let mut acc = 0u64;
            let mut buf = [0.0f32; 512];
            for i in 0..40 {
                if i % 2 == 0 {
                    s.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
                }
                s.render(&mut buf);
                for &x in &buf {
                    acc = acc.rotate_left(7).wrapping_add(u64::from(x.to_bits()));
                }
            }
            acc
        };
        let hashes: Vec<(BedVariant, u64)> = BedVariant::ALL
            .into_iter()
            .map(|v| (v, render_hash(v)))
            .collect();
        for (i, &(va, ha)) in hashes.iter().enumerate() {
            for &(vb, hb) in &hashes[i + 1..] {
                assert_ne!(
                    ha, hb,
                    "{va:?} and {vb:?} rendered identically — a candidate is a no-op"
                );
            }
        }
    }

    /// C4's contract is exact: with the bed layer fully energised, the
    /// SILENCE candidate's bed mixer returns literal (0.0, 0.0) — zero
    /// samples contributed, so "no bed" is judged from the identical
    /// harness rather than a differently-plumbed control render.
    #[test]
    fn silence_candidate_contributes_exact_zero_bed_samples() {
        let mut s = TrailSynth::new(48_000.0, 7);
        s.set_bed_variant(BedVariant::Silence);
        let mut buf = [0.0f32; 960];
        for _ in 0..10 {
            s.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
            s.render(&mut buf);
        }
        let (energy, level) = s.debug_bed();
        assert!(
            energy > 0.0 && level > 1e-3,
            "precondition: the bed layer must be energised (energy {energy}, level {level})"
        );
        let dt = 1.0 / 48_000.0;
        for _ in 0..64 {
            assert_eq!(
                s.bed_sample(dt),
                (0.0, 0.0),
                "the SILENCE candidate leaked bed samples"
            );
        }
    }

    /// Deterministic: same seed + same script ⇒ identical output.
    #[test]
    fn deterministic() {
        let run = || {
            let mut s = TrailSynth::new(48_000.0, 99);
            let mut acc = 0.0f64;
            let mut buf = [0.0f32; 512];
            for i in 0..40 {
                if i % 3 == 0 {
                    s.push(ev(STYLES[i % 9], SoundKind::Typed));
                }
                s.render(&mut buf);
                for &x in &buf {
                    acc += f64::from(x) * 1e3;
                }
            }
            acc
        };
        assert_eq!(run().to_bits(), run().to_bits());
    }

    /// Bonk determinism: a script that interleaves typing with bonks renders
    /// bit-identically across runs — the bonk (and its duck envelope) draw
    /// all randomness from the shared xorshift and all time from samples.
    #[test]
    fn deterministic_with_bonks() {
        let run = || {
            let mut s = TrailSynth::new(48_000.0, 0x5EED_50FD);
            let mut acc = 0u64;
            let mut buf = [0.0f32; 512];
            for i in 0..60 {
                if i % 3 == 0 {
                    s.push(ev(STYLES[i % 9], SoundKind::Typed));
                }
                if i % 10 == 4 {
                    s.push(bonk(STYLES[i % 9]));
                }
                s.render(&mut buf);
                for &x in &buf {
                    acc = acc.rotate_left(7).wrapping_add(u64::from(x.to_bits()));
                }
            }
            acc
        };
        assert_eq!(run(), run());
    }

    /// Zero-gain events are dropped entirely (the host's reduced-motion /
    /// muted path pushes nothing, but belt-and-braces).
    #[test]
    fn zero_gain_is_inert() {
        let mut s = TrailSynth::new(48_000.0, 5);
        let mut e = ev(GlowStyle::Water, SoundKind::Typed);
        e.gain = 0.0;
        s.push(e);
        assert!(s.is_quiet());
        assert_eq!(s.live_voices(), 0);
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

    /// Voice stealing: hammering jumps (which bypass the min-gap) never
    /// exceeds the pool or panics.
    #[test]
    fn polyphony_is_bounded() {
        let mut s = TrailSynth::new(48_000.0, 11);
        for _ in 0..200 {
            s.push(ev(GlowStyle::Comet, SoundKind::Jump));
        }
        assert!(s.live_voices() <= MAX_VOICES);
        let mut buf = [0.0f32; 512];
        s.render(&mut buf);
        assert!(buf.iter().all(|x| x.is_finite()));
    }

    // -- bonk behavioral proofs ---------------------------------------------

    /// Admission priority: within the min-gap window a Typed event is thinned
    /// but a Bonk always speaks (Jump-class governor bypass — punctuation may
    /// never be swallowed by the thinning meant for texture).
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

    /// TYPING JOINS THE SONG'S KEY (owner, 2026-08-04: "more musical in kitty
    /// rainbow"). While the cat sings, a typed note transposes with the riff, so
    /// the two layers are one piece of music instead of two lattices that only
    /// happened to be a near-just fourth apart. When the song ends, the typed
    /// melody is back on the neutral lattice exactly as before.
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
        // A neutral typed note.
        let mut plain = TrailSynth::new(48_000.0, 7);
        plain.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
        let neutral = hz_of(&plain);
        assert!(neutral > 0.0, "the typed note speaks");

        // The same note, with the song live in a transposed key. Built through
        // the CANONICAL constructor — the payload every host speaks once the
        // pending GUI patch lands — because the latch used to exist only on the
        // deprecated `RiffBar { key }` arm, and a fixture that reaches for the
        // shim would have gone on passing while the shipping path was mute.
        // `sig = 4` is a root of +2 (`4 % 5 - 2`) and a mode rotation of +2
        // (`MODE_ROTATIONS[4 % 3]`). The latched key must be the SUM — the same
        // integer `design_celebration` shifts every sounding riff degree by —
        // or the typing sits a rotation away from the song it is supposedly in
        // (it did, for the 66 % of characters whose rotation is nonzero).
        const SIG_IN_KEY_TWO: u32 = 4;
        let shift = celebration_root(SIG_IN_KEY_TWO) + celebration_mode(SIG_IN_KEY_TWO);
        assert_eq!(
            (celebration_root(SIG_IN_KEY_TWO), shift),
            (2, 4),
            "fixture precondition: root +2, root+mode +4"
        );
        let mut singing = TrailSynth::new(48_000.0, 7);
        singing.push(SoundEvent {
            kind: SoundGesture::Celebration(CelebrationGesture::riff_bar(0, SIG_IN_KEY_TWO)),
            ..ev(GlowStyle::RainbowKitty, SoundKind::Typed)
        });
        assert_eq!(
            i32::from(singing.song_key),
            shift,
            "the riff latches the key it actually transposes by (root + mode)"
        );
        let mut after = TrailSynth::new(48_000.0, 7);
        after.song_key = 2;
        after.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
        let transposed = hz_of(&after);
        assert!(
            transposed > neutral * 1.05,
            "a typed note rides the song's key: {neutral} -> {transposed}"
        );

        // …and the key is released with the song, not left parked.
        let mut ended = TrailSynth::new(48_000.0, 7);
        ended.song_key = 2;
        ended.sing = 1.0;
        let mut buf = vec![0.0f32; 48_000 * 2 * 4];
        ended.render(&mut buf); // long enough for every voice + hold to die
        assert!(ended.is_quiet(), "the fixture reaches silence");
        assert_eq!(
            ended.song_key, 0,
            "the borrowed key is handed back when the song ends"
        );
        let mut back = TrailSynth::new(48_000.0, 7);
        back.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
        assert_eq!(
            hz_of(&back),
            neutral,
            "and the neutral typed melody is unchanged, note for note"
        );

        // THE HAND-BACK MUST NOT DEPEND ON SILENCE. The release used to live
        // only in the `is_quiet()` early return, which a typist never reaches:
        // at ~9.5 cps or faster a voice is always live, so the borrowed key
        // outlived the song for the rest of the session (measured 3.5-6.0 s
        // after the last bar, the typed register was still pinned by whichever
        // character had been held). Here the synth is kept AUDIBLE throughout
        // by continuous typing, and the key must still come back with the
        // sing duck.
        let mut busy = TrailSynth::new(48_000.0, 7);
        busy.push(SoundEvent {
            kind: SoundGesture::Celebration(CelebrationGesture::riff_bar(0, SIG_IN_KEY_TWO)),
            ..ev(GlowStyle::RainbowKitty, SoundKind::Typed)
        });
        assert_ne!(busy.song_key, 0, "the song latched a key");
        let mut blk = vec![0.0f32; 480 * CHANNELS]; // 10 ms
        for i in 0..1_200 {
            // 12 s of typing at ~20 cps: `is_quiet()` is never true.
            if i % 5 == 0 {
                busy.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
            }
            busy.render(&mut blk);
            assert!(!busy.is_quiet(), "the fixture never falls silent");
        }
        assert_eq!(
            busy.song_key, 0,
            "the borrowed key is released with the sing duck, not with silence"
        );
    }

    /// The bonk is the one deliberately discordant voice: its clash partials
    /// sit a just-intonation minor second and tritone above the melody's
    /// CURRENT walk degree — ratios that exist nowhere in the pentatonic
    /// lattice — and pushing it moves neither the walk nor the bed.
    #[test]
    fn bonk_is_discordant_against_the_walk_and_moves_nothing() {
        let mut s = TrailSynth::new(48_000.0, 33);
        let walk_before = s.walk;
        // The rainbow kitty's anchor (C5) at the current degree — one register
        // per palette (`Palette::anchor_hz`), tracking its `base`, so it moved
        // with the typing voice when the doop went back to G4 on 2026-08-10
        // and again when the tingly bell went back up to C5 on 2026-08-15.
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
        assert!(
            clash.iter().any(|r| (r - BONK_MINOR_SECOND).abs() < 1e-3),
            "missing the minor-second clash: {clash:?}"
        );
        assert!(
            clash.iter().any(|r| (r - BONK_TRITONE).abs() < 1e-3),
            "missing the tritone clash: {clash:?}"
        );
        // Neither ratio is a pentatonic degree (any octave wrap).
        for ratio in [BONK_MINOR_SECOND, BONK_TRITONE] {
            for step in PENTA {
                for oct in [0.5f32, 1.0, 2.0] {
                    assert!(
                        (ratio - step * oct).abs() > 1e-3,
                        "bonk ratio {ratio} collides with the consonant lattice"
                    );
                }
            }
        }
        const {
            assert!(
                BONK_KIND_GAIN > 1.25,
                "the bonk must outrank every trail kind-gain (Jump = 1.25)"
            );
        }
    }

    /// The master duck envelope: a bonk snaps it to 1, rendering decays it,
    /// and while it is up the (non-exempt) melody/bed mix is measurably
    /// quieter than the identical un-bonked synth — the melody dips AROUND
    /// the bonk. Measured in a window where the bonk's own voices are dead,
    /// so the only differences are the duck and the exactly-equal bed.
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

    // -- SING-ALONG sing-along proofs ----------------------------------------

    /// One sing-along riff bar for a held key's signature (the celebration
    /// gesture helper). `heat` 1.0: the host pins momentum to full while
    /// armed — that IS maximal flow. `bed` false: the host states the policy
    /// the gesture actually gets (Celebration, like Words, structurally
    /// never reaches the bed feed).
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
        }
    }

    /// The neutral-signature riff bar — the reference voicing (root 0, home
    /// mode) the axis-agnostic celebration proofs speak in.
    fn riff(style: GlowStyle, bar: u16) -> SoundEvent {
        riff_sig(style, bar, crate::kitty_sing::NEUTRAL_SIGNATURE)
    }

    /// RIFF DETERMINISM: the samples are a pure function of (seed, events) —
    /// two synths fed the identical two-phrase celebration script for the
    /// SAME held key ('w''s signature, so the per-key verse path is on the
    /// line too) render byte-identical audio, and the riff genuinely sounds
    /// (non-zero).
    #[test]
    fn celebration_riff_is_deterministic_given_seed_and_events() {
        let w = crate::kitty_sing::song_signature('w');
        let run = || {
            let mut s = TrailSynth::new(48_000.0, 42);
            let mut out = Vec::new();
            let mut buf = [0.0f32; 960]; // 10 ms blocks
            for bar in 0..(CELEBRATION_PHRASE_BARS as u16 * 2) {
                s.push(riff_sig(GlowStyle::RainbowKitty, bar, w));
                for _ in 0..160 {
                    // one bar = 1.6 s
                    s.render(&mut buf);
                    out.extend_from_slice(&buf);
                }
            }
            out
        };
        let (a, b) = (run(), run());
        assert_eq!(a, b, "riff must replay bit-exact per (seed, events)");
        assert!(
            a.iter().any(|&x| x != 0.0),
            "the celebration must actually sound"
        );
    }

    /// A riff bar is punctuation: it bypasses the min-gap thinning exactly
    /// like Jump and the Bonk — a bar landing right after an admitted typed
    /// voice still schedules its full 8-note phrase.
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
    }

    /// THE SING DUCK: while the riff speaks, ordinary (non-exempt) melody
    /// voices render attenuated by the documented depth — the riff REPLACES
    /// the melody — and once bars stop, the envelope hands back to exact
    /// rest (the same snap-to-zero bit-identity contract as the bonk duck).
    #[test]
    fn celebration_ducks_the_melody_and_hands_back() {
        // Identical lone rainbow kitty typed notes; one renders under a pinned sing
        // duck. Bed OFF so the energy compared is exactly the melody voice.
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
        // …and with no further bars it hands back: after the bar + tails +
        // a few handback τ the envelope has snapped to exact zero while the
        // synth is still live (keep it breathing with quiet nav events).
        // 7 s: the 0.30 s glass-bell nav voice overlaps the 200 ms cadence,
        // so `is_quiet()` never zeroes `sing` for us — the exponential
        // handback itself must reach the 1e-4 snap, which takes
        // hold (1.7 s) + τ·ln(1e4) ≈ 5.4 s. The assertion stays exact; this
        // is the time the envelope math has always needed on a synth that
        // genuinely never falls silent.
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

    /// Celebration gestures structurally never feed the ambient bed (the
    /// same law as Words): a riff bar deposits zero bed energy even when the
    /// event's `bed` flag rides ON.
    #[test]
    fn celebration_never_feeds_the_bed() {
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

    /// The audio bar and the visual dance clock are ONE tempo: the samples-
    /// based bar length is pinned equal to `kitty_sing`'s wall-clock bar (the
    /// documented ± ~60 ms host-buffer skew is tolerance, not tempo drift).
    #[test]
    fn celebration_bar_matches_the_visual_clock() {
        assert_eq!(CELEBRATION_BAR_SECONDS, crate::kitty_sing::SING_BAR_SECONDS);
        assert_eq!(
            CELEBRATION_EIGHTH,
            crate::kitty_sing::SING_BEAT_SECONDS / 2.0
        );
    }

    /// A bass note rides the lead voice that OPENS its beat, so it needs a
    /// carrier: no `REST` lead slot may sit under a sounding bass beat, or the
    /// low end silently vanishes for that beat.
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

    /// The host truncates its `u64` bar counter into the gesture payload, so
    /// the form must tile the payload's range EXACTLY — otherwise the song
    /// jump-cuts once per wrap.
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

    /// POLYPHONY BUDGET: a whole bar is spawned at once as pre-delayed voices,
    /// so its note count is HELD for the bar. The fullest bar must leave room
    /// for the typed layer underneath a held key.
    #[test]
    fn celebration_bar_fits_the_polyphony_budget() {
        for (b, bar) in CELEBRATION_PHRASE.iter().enumerate() {
            let mut n = bar.iter().filter(|d| **d != REST).count();
            if b == CELEBRATION_PHRASE_BARS - 1 {
                n += CELEBRATION_FILL.len();
            }
            assert!(n <= 10, "bar {b} spawns {n} voices — over the riff budget");
        }
        // Live: sixteen bars with a typed flood under them never produce a
        // non-finite sample.
        let mut s = TrailSynth::new(48_000.0, 11);
        let mut buf = [0.0f32; 960];
        for bar in 0..16u16 {
            s.push(riff(GlowStyle::RainbowKitty, bar));
            for block in 0..160 {
                if block % 5 == 0 {
                    s.push(SoundEvent {
                        bed: false,
                        ..ev(GlowStyle::RainbowKitty, SoundKind::Typed)
                    });
                }
                s.render(&mut buf);
                assert!(buf.iter().all(|x| x.is_finite()));
            }
        }
    }

    /// The riff's phrase tables stay on the consonant major-pentatonic
    /// lattice (degrees, not raw ratios), so the sing-along can never rub
    /// against the typed melody under it and the bonk keeps its exclusive
    /// claim to "wrong".
    #[test]
    fn celebration_phrases_live_on_the_pentatonic_lattice() {
        for deg in CELEBRATION_PHRASE.iter().flatten().filter(|d| **d != REST) {
            assert!(
                (0..=9).contains(deg),
                "riff degree {deg} outside the two-octave singable range"
            );
        }
        for deg in &CELEBRATION_FILL {
            assert!((0..=9).contains(deg), "fill degree {deg} out of range");
        }
        for deg in CELEBRATION_BASS.iter().flatten().filter(|d| **d != REST) {
            assert!(
                (-12..=-1).contains(deg),
                "bass degree {deg} outside the bass register"
            );
        }
        // EIGHT DIFFERENT bars, pairwise — a song, not a stuck record.
        for (a, bar_a) in CELEBRATION_PHRASE.iter().enumerate() {
            // `skip(a + 1)` keeps the upper triangle: each unordered pair is
            // compared once, and `b` still names the bar's own index.
            for (b, bar_b) in CELEBRATION_PHRASE.iter().enumerate().skip(a + 1) {
                assert_ne!(bar_a, bar_b, "bars {a} and {b} are the same bar");
            }
        }
    }

    /// THE RIFF INHERITS THE ACTIVE TONE'S LATTICE. A live typed stream leaves
    /// `self.tone` at the current mood; the sing-along must transpose onto the
    /// SAME table the melody uses, or its authored PENTA degrees rub the bonk's
    /// banned intervals against a melody that has moved to a minor/yo table or
    /// been transposed. The neutral (Technical/Calm) path is the exact identity
    /// — the riff on untransposed PENTA — so every pinned celebration proof
    /// (determinism, duck, bed) is byte-untouched.
    #[test]
    fn celebration_riff_follows_the_active_tone_lattice() {
        // A render hash of one riff bar with `self.tone` pre-set, as the typed
        // stream underneath a held key would leave it. Nothing but PITCH is
        // tone-dependent in the celebration path (walk unused, `tone_feel`
        // skipped for `duck_exempt` voices), so a divergence is a pitch shift.
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
        assert_eq!(
            technical,
            render_toned(Tone::Calm),
            "neutral tones leave the riff on untransposed PENTA (byte-identical)"
        );
        for tone in [Tone::Excited, Tone::Frustrated, Tone::Playful] {
            assert_ne!(
                render_toned(tone),
                technical,
                "{tone:?} must transpose the riff onto its own consonant lattice"
            );
        }
        // The pitch law itself: under Technical the riff seam IS the free
        // penta (the identity `melody_hz` collapses to), so the neutral riff
        // stays exactly the authored PENTA phrase.
        let mut s = TrailSynth::new(48_000.0, 1);
        s.tone = Tone::Technical;
        for &deg in CELEBRATION_PHRASE
            .iter()
            .flatten()
            .chain(&CELEBRATION_FILL)
            .chain(CELEBRATION_BASS.iter().flatten())
            .filter(|d| **d != REST)
        {
            assert_eq!(
                s.melody_hz(CELEBRATION_BASE_HZ, deg).to_bits(),
                penta(CELEBRATION_BASE_HZ, deg).to_bits(),
                "neutral riff degree {deg} must be the exact untransposed identity"
            );
        }
    }

    // -- per-key song-axis proofs ---------------------------------------------

    /// SAME KEY, SAME SONG — and NO synth-side per-key state: the voices a
    /// `sig('w')` bar schedules are identical whether the synth is fresh or
    /// just sang a DIFFERENT key's bar first (the seamless hand-over law —
    /// the design must be a pure function of the payload, or a mid-hold key
    /// change would drag stale identity across the bar boundary).
    #[test]
    fn same_key_stability() {
        let fingerprint = |v: &Voice| {
            (
                v.delay.to_bits(),
                v.dur.to_bits(),
                v.p[0].f0.to_bits(),
                v.p[1].f0.to_bits(),
                v.p[2].f0.to_bits(),
            )
        };
        let w = crate::kitty_sing::song_signature('w');
        // Fresh synth: the reference 'w' bar.
        let mut a = TrailSynth::new(48_000.0, 7);
        a.push(riff_sig(GlowStyle::RainbowKitty, 0, w));
        let mut reference: Vec<_> = a.voices.iter().filter(|v| v.on).map(fingerprint).collect();
        reference.sort_unstable();
        // Same seed, but ANOTHER key's bar spoke first (the bridge bar — its
        // four voices leave room in the pool for the full 'w' bar).
        let mut b = TrailSynth::new(48_000.0, 7);
        b.push(riff_sig(
            GlowStyle::RainbowKitty,
            6,
            crate::kitty_sing::song_signature('a'),
        ));
        let before: Vec<_> = b.voices.iter().filter(|v| v.on).map(fingerprint).collect();
        b.push(riff_sig(GlowStyle::RainbowKitty, 0, w));
        let mut after: Vec<_> = b.voices.iter().filter(|v| v.on).map(fingerprint).collect();
        // Multiset-subtract the first bar's voices; what remains is exactly
        // what the 'w' bar scheduled (subtraction is collision-safe: equal
        // fingerprints are interchangeable in a multiset).
        for f in before {
            let i = after
                .iter()
                .position(|x| *x == f)
                .expect("the 'a' bar's voices must still be present");
            after.swap_remove(i);
        }
        after.sort_unstable();
        assert_eq!(
            after, reference,
            "a 'w' bar must schedule identical voices with or without another \
             key's bar before it"
        );
    }

    /// THE SWITCH DAMP (owner: "the transition between switching between
    /// repeated keys also has overlapping music. that needs to be fixed"):
    /// a bar under a NEW signature orders every still-ringing celebration
    /// voice into the [`CELEBRATION_DAMP_S`] release, and within 100 ms of
    /// the boundary the old key's ring is gone while the new verse sings on.
    #[test]
    fn key_switch_damps_the_old_keys_ring() {
        let t = crate::kitty_sing::song_signature('t');
        let e = crate::kitty_sing::song_signature('e');
        assert_ne!(t, e, "fixture: two different songs");
        let mut s = TrailSynth::new(48_000.0, 42);
        s.push(riff_sig(GlowStyle::RainbowKitty, 4, t));
        // Render exactly one bar: the legato notes (1.5× eighths) ring past
        // the bar line by design — that overhang IS the defect on a switch.
        let mut buf = [0.0f32; 960]; // 10 ms blocks
        for _ in 0..160 {
            s.render(&mut buf);
        }
        let ringing = s.voices.iter().filter(|v| v.on && v.celebration).count();
        assert!(
            ringing > 0,
            "fixture: the old key's tails must still ring at the boundary"
        );
        assert!(
            s.voices.iter().all(|v| v.damp == 0.0),
            "no damp before the switch"
        );
        // THE SWITCH: the next bar carries the other key's signature.
        s.push(riff_sig(GlowStyle::RainbowKitty, 5, e));
        let damped = s.voices.iter().filter(|v| v.on && v.damp > 0.0).count();
        assert_eq!(
            damped, ringing,
            "every old tail must be ordered into the release"
        );
        assert!(
            s.voices
                .iter()
                .any(|v| v.on && v.celebration && v.damp == 0.0),
            "…and the new bar's voices must not be damped"
        );
        // Within 100 ms past the boundary the old ring has collapsed to
        // nothing (the release is 50 ms; the countdown kills the voice) while
        // the new verse is audibly under way.
        for _ in 0..10 {
            s.render(&mut buf);
        }
        assert_eq!(
            s.voices.iter().filter(|v| v.on && v.damp > 0.0).count(),
            0,
            "the old key's ring must be silent within 100 ms of the boundary"
        );
        assert!(
            s.voices.iter().any(|v| v.on && v.celebration),
            "while the new verse sings on"
        );
        assert!(
            buf.iter().any(|&x| x != 0.0),
            "and the new bar actually sounds"
        );

        // A switch with the old bar still in PRE-DELAY (the host latches per
        // visual bar, so this is defensive): the damp clock burns through the
        // pre-delay too, so an unstarted old-key voice expires unheard — it
        // must never open its note under the new verse.
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

    /// SAME KEY, NO DAMP — the byte-fidelity guard: consecutive bars under
    /// ONE signature never arm the release, so a single-key celebration
    /// renders through the exact pre-damp arithmetic (the render-side damp
    /// branch is untaken at `damp == 0.0` and adds no float op — together
    /// with `celebration_riff_is_deterministic_given_seed_and_events` this
    /// keeps the restored v0.20.0 verse bit-exact).
    #[test]
    fn same_key_bars_never_damp() {
        let w = crate::kitty_sing::song_signature('w');
        let mut s = TrailSynth::new(48_000.0, 42);
        let mut buf = [0.0f32; 960];
        for bar in 0..4u16 {
            s.push(riff_sig(GlowStyle::RainbowKitty, bar, w));
            assert!(
                s.voices.iter().all(|v| v.damp == 0.0),
                "bar {bar}: a same-sig bar must never damp the ringing tails"
            );
            for _ in 0..160 {
                s.render(&mut buf);
            }
        }
    }

    /// CROSS-KEY DIFFERENCE — the owner's ask, pinned at the melody level:
    /// 'w', 'a' and 'z' get verse INTERVAL SEQUENCES that differ pairwise.
    /// Intervals, not degrees: a transpose preserves every interval, so this
    /// can only pass if the verses differ in CONTOUR — different tunes, not
    /// one tune nudged.
    #[test]
    fn cross_key_difference() {
        let intervals = |sig: u32| -> Vec<i32> {
            let mut degs = Vec::new();
            // `CELEBRATION_VERSE` is `[bool; CELEBRATION_PHRASE_BARS]`, indexed
            // by the same bar ordinal the walk uses — so iterating IT *is* the
            // per-bar loop, and the bar count can never drift out of step with
            // the flag table the way a separate `0..N` range could.
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

    /// THE FILL CARRIES THE ROOT: the turnaround's four sixteenths used to
    /// take the pitch lattice WITHOUT the transpose, so every transposed
    /// hold got a wrong-key hiccup once per eight bars — the tumble home
    /// landed in the REFERENCE key, not the held one.
    #[test]
    fn fill_carries_root() {
        // sig 24: 24 % 5 == 4 ⇒ root +2, 24 % 3 == 0 ⇒ home mode — the
        // shift is pure root.
        let sig = 24u32;
        assert_eq!(celebration_root(sig), 2);
        assert_eq!(celebration_mode(sig), 0);
        let mut s = TrailSynth::new(48_000.0, 3);
        s.push(riff_sig(GlowStyle::RainbowKitty, 7, sig)); // the turnaround bar
        // Fill voices are the only sixteenths: dur == exactly one eighth
        // (phrase voices hold ≥ 1.5 eighths; `duck_exempt` skips tone_feel,
        // so the durations are bit-exact).
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

    /// THE SHARED CHORUS: bars B (2), B' (6) and the turnaround D (7) return
    /// the AUTHORED contour for every signature, byte-identical — what keeps
    /// every key's song recognizably THE celebration while the verses roam.
    #[test]
    fn chorus_bars_are_byte_identical_across_sigs() {
        let sigs = [
            0u32,
            1,
            11,
            12,
            24,
            0xDEAD_BEEF,
            u32::MAX,
            crate::kitty_sing::song_signature('w'),
            crate::kitty_sing::song_signature('a'),
            crate::kitty_sing::song_signature('z'),
        ];
        for &sig in &sigs {
            for idx in [2usize, 6, 7] {
                assert!(!CELEBRATION_VERSE[idx]);
                assert_eq!(
                    celebration_bar_degrees(sig, idx),
                    CELEBRATION_PHRASE[idx],
                    "chorus bar {idx} drifted for sig {sig:#x}"
                );
            }
        }
    }

    /// THE VERSE WALK LAWS over a spread of signatures: the authored RHYTHM
    /// survives verbatim (the REST pattern — and with it the groove, swing,
    /// span and bass-carrier laws), every sounding degree stays inside the
    /// singable lattice window, adjacent sounding verse notes always MOVE
    /// (no key can drone), and the root register guard holds for every sig.
    #[test]
    fn verse_walk_always_moves_and_keeps_rhythm_and_register() {
        let sigs: Vec<u32> = "wazqx09~ \t"
            .chars()
            .map(crate::kitty_sing::song_signature)
            .chain([0, 1, 5, 12, 24, 0x7FFF_FFFF, u32::MAX])
            .collect();
        for &sig in &sigs {
            assert!((-2..=2).contains(&celebration_root(sig)), "root guard");
            assert!(MODE_ROTATIONS.contains(&celebration_mode(sig)));
            for idx in 0..CELEBRATION_PHRASE_BARS {
                let row = celebration_bar_degrees(sig, idx);
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

    /// AXIS 3 IS AUDIBLE ON ITS OWN: two signatures with the same root and
    /// the same contour (a chorus bar is authored for every key) but
    /// different mode classes render differently — and a same-root/same-mode
    /// pair renders identically, so the difference IS the rotation and a
    /// chorus bar depends on the payload through (root, mode) alone.
    #[test]
    fn mode_rotation_alone_changes_the_chorus_color() {
        // 12 ≡ 2, 22 ≡ 2, 42 ≡ 2 (mod 5) — one shared root (0);
        // 12 ≡ 0, 22 ≡ 1, 42 ≡ 0 (mod 3) — modes 0, 2, 0.
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
        assert_eq!(celebration_root(12), celebration_root(42));
        assert_eq!(celebration_mode(12), celebration_mode(42));
        assert_eq!(
            render(12),
            render(42),
            "chorus bars carry no per-key axis beyond root + mode"
        );
    }

    /// The neutral signature (nothing held) IS the reference voicing — root
    /// 0, home mode.
    /// Cross-pins `kitty_sing::NEUTRAL_SIGNATURE` to this module's decode.
    #[test]
    fn neutral_signature_is_the_reference_voicing() {
        use crate::kitty_sing::NEUTRAL_SIGNATURE;
        assert_eq!(celebration_root(NEUTRAL_SIGNATURE), 0);
        assert_eq!(celebration_mode(NEUTRAL_SIGNATURE), 0);
    }

    /// AXIS 4 stays DARK until the owner has heard it: with the gate off,
    /// every signature's lead duty is the authored 0.25 — flipping
    /// [`CELEBRATION_KEY_DUTY`] is the entire enable.
    #[test]
    fn per_key_duty_is_gated_off_pending_the_owners_ear() {
        for sig in [
            0u32,
            12,
            24,
            crate::kitty_sing::song_signature('w'),
            u32::MAX,
        ] {
            if CELEBRATION_KEY_DUTY {
                assert!([0.25f32, 0.375, 0.5].contains(&celebration_duty(sig)));
            } else {
                assert_eq!(celebration_duty(sig), 0.25);
            }
        }
    }

    /// THE CLAP BAR: the backbeat clap joins at bar 8 — one full phrase in
    /// ([`CELEBRATION_CLAP_BAR`]) — and not a bar earlier. The lead voices
    /// carry no other noise burst, so any live noise channel IS the clap.
    #[test]
    fn the_clap_arrives_at_bar_eight() {
        assert_eq!(CELEBRATION_CLAP_BAR, 8);
        let clapping = |bar: u16| {
            let mut s = TrailSynth::new(48_000.0, 7);
            s.push(riff(GlowStyle::RainbowKitty, bar));
            s.voices.iter().any(|v| v.on && v.n_lvl > 0.0)
        };
        assert!(!clapping(7), "bar 7 is still clean");
        assert!(clapping(8), "bar 8 picks up the backbeat clap");
    }

    // -- tone-melody proofs --------------------------------------------------

    /// THE NO-BEATING INVARIANT, extended to every tone table: all pairwise
    /// intervals of every table (octave wraps and inversions included) stay
    /// outside the two rub zones the bonk owns — the minor-second crush and
    /// the tritone band — and the transpose cannot smuggle one in (a
    /// transposed lattice has the same internal intervals). The zones are
    /// proven non-vacuous by the bonk's own identities landing inside them:
    /// the ONE discordant voice keeps its exclusive claim to "wrong" under
    /// every mood.
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
        // Non-vacuous: the bonk's identities DO live in the zones.
        let m2 = reduce(BONK_MINOR_SECOND);
        assert!(m2 > 1.0 + 1e-4 && m2 < 1.09, "minor-second zone drifted");
        let tt = reduce(BONK_TRITONE);
        assert!(tt > 1.395 && tt < 1.43, "tritone zone drifted");
    }

    /// The tone → scale-table/transpose mapping, degree by degree: Technical
    /// and Calm are bit-identical to the free [`penta`] (the identity),
    /// Excited is the same lattice one just whole tone up, Frustrated walks
    /// the minor table, Playful the suspended *yo* table.
    #[test]
    fn tone_selects_scale_table_and_transpose() {
        let mut s = TrailSynth::new(48_000.0, 1);
        for deg in -7..12 {
            s.tone = Tone::Technical;
            let neutral = s.melody_hz(330.0, deg);
            assert_eq!(
                neutral.to_bits(),
                penta(330.0, deg).to_bits(),
                "Technical must be the exact identity at degree {deg}"
            );
            s.tone = Tone::Calm;
            assert_eq!(
                s.melody_hz(330.0, deg).to_bits(),
                neutral.to_bits(),
                "Calm keeps today's table untransposed at degree {deg}"
            );
            s.tone = Tone::Excited;
            assert_eq!(
                s.melody_hz(330.0, deg).to_bits(),
                (penta(330.0, deg) * EXCITED_TRANSPOSE).to_bits(),
                "Excited is the same lattice a whole tone up at degree {deg}"
            );
        }
        s.tone = Tone::Frustrated;
        assert_eq!(s.melody_hz(330.0, 1), 330.0 * 1.2, "minor third");
        assert_eq!(s.melody_hz(330.0, 4), 330.0 * 1.8, "minor seventh");
        s.tone = Tone::Playful;
        assert_eq!(s.melody_hz(330.0, 2), 330.0 * 1.333_333_3, "yo fourth");
    }

    /// Determinism, tone included: the same (events, seed, tone) script
    /// renders bit-identically — and the tone axis is not a no-op (a mixed-
    /// tone script diverges from the all-neutral one).
    #[test]
    fn melody_is_deterministic_given_events_seed_and_tone() {
        let tones = [
            Tone::Calm,
            Tone::Excited,
            Tone::Frustrated,
            Tone::Playful,
            Tone::Technical,
        ];
        let run = |script: &[Tone]| {
            let mut s = TrailSynth::new(48_000.0, 0x70_4E5D);
            let mut acc = 0u64;
            let mut buf = [0.0f32; 512];
            for (i, &tone) in script.iter().enumerate() {
                s.push(toned(STYLES[i % 9], SoundKind::Typed, tone));
                for _ in 0..3 {
                    s.render(&mut buf);
                    for &x in &buf {
                        acc = acc.rotate_left(7).wrapping_add(u64::from(x.to_bits()));
                    }
                }
            }
            acc
        };
        let mixed: Vec<Tone> = (0..40).map(|i| tones[i % tones.len()]).collect();
        assert_eq!(run(&mixed), run(&mixed), "tone scripts must replay exactly");
        let neutral = vec![Tone::Technical; 40];
        assert_eq!(run(&neutral), run(&neutral));
        assert_ne!(
            run(&mixed),
            run(&neutral),
            "the tone axis must actually recolor the melody"
        );
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

    /// The bonk is tone-blind end to end: identical bytes whatever tone the
    /// event carries (its clash stays anchored to the untransposed lattice,
    /// its voices skip the feel scaling via `duck_exempt`, and it neither
    /// reads nor writes the synth's tone).
    #[test]
    fn bonk_path_ignores_tone() {
        let run = |tone: Tone| {
            let mut s = TrailSynth::new(48_000.0, 0x0B0B);
            let mut e = bonk(GlowStyle::RainbowKitty);
            e.tone = tone;
            s.push(e);
            let mut acc = 0u64;
            let mut buf = [0.0f32; 960];
            for _ in 0..40 {
                s.render(&mut buf);
                for &x in &buf {
                    acc = acc.rotate_left(11).wrapping_add(u64::from(x.to_bits()));
                }
            }
            acc
        };
        let neutral = run(Tone::Technical);
        for tone in [Tone::Calm, Tone::Excited, Tone::Frustrated, Tone::Playful] {
            assert_eq!(run(tone), neutral, "{tone:?} leaked into the bonk");
        }
    }

    /// Tempo-feel is real and bounded: an Excited pluck is measurably
    /// shorter than the neutral one (dur × 0.88 exactly), and a bonk spawned
    /// while the melody is Calm keeps its exact kind-level envelope — the
    /// exemption in `spawn`.
    #[test]
    fn tone_feel_scales_notes_but_never_the_bonk() {
        let first_voice_dur = |s: &TrailSynth| {
            s.voices
                .iter()
                .find(|v| v.on)
                .map(|v| v.dur)
                .expect("a voice spawned")
        };
        let mut neutral = TrailSynth::new(48_000.0, 4);
        neutral.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        let base_dur = first_voice_dur(&neutral);
        let mut excited = TrailSynth::new(48_000.0, 4);
        excited.push(toned(GlowStyle::Lumen, SoundKind::Typed, Tone::Excited));
        let excited_dur = first_voice_dur(&excited);
        assert!(
            (excited_dur - base_dur * 0.88).abs() < 1e-6,
            "Excited feel must shorten the pluck: {excited_dur} vs {base_dur}"
        );
        // Calm melody, then a bonk: the bonk's two voices keep their exact
        // kind-level durations (0.30 clash / 0.16 thump), untouched by the
        // 1.06 Calm feel.
        let mut s = TrailSynth::new(48_000.0, 4);
        s.push(toned(GlowStyle::Lumen, SoundKind::Typed, Tone::Calm));
        s.push(bonk(GlowStyle::Lumen));
        let mut bonk_durs: Vec<f32> = s
            .voices
            .iter()
            .filter(|v| v.on && v.duck_exempt)
            .map(|v| v.dur)
            .collect();
        bonk_durs.sort_by(f32::total_cmp);
        assert_eq!(
            bonk_durs,
            vec![0.16, 0.30],
            "bonk envelope must stay pinned"
        );
    }

    // -- the bar: what makes the typed line a song --------------------------

    /// THE BAR IS AUTHORED AND CONSONANT. Every ghost offset must sit ON the
    /// scale lattice (an integer scale-degree), so an accompaniment note can
    /// never rub against the melody note it accompanies under any
    /// [`crate::tone::Tone`] — the module's no-beating law is inherited rather
    /// than re-argued. And the beat must be REGULAR: an irregular accent
    /// pattern reads as level drift, which is exactly what a listener hears as
    /// "the volume is glitching" rather than as rhythm.
    #[test]
    fn the_bar_is_a_regular_beat_of_consonant_offsets() {
        let accents: Vec<usize> = SONG_PULSE
            .iter()
            .enumerate()
            .filter(|(_, s)| **s == SONG_ACCENT)
            .map(|(i, _)| i)
            .collect();
        assert!(
            accents.len() >= 3,
            "the melody must sing often enough to BE a melody"
        );
        assert!(
            accents.len() * 2 < SONG_PULSE.len(),
            "…and less than half the time, or the density this exists to cut \
             is still there"
        );
        assert_eq!(accents[0], 0, "the bar opens on its downbeat");
        let step = accents[1] - accents[0];
        for w in accents.windows(2) {
            assert_eq!(
                w[1] - w[0],
                step,
                "the beat must be EVEN — an irregular accent reads as level \
                 drift, not as rhythm: {accents:?}"
            );
        }
        assert!(
            SONG_PULSE.len().is_multiple_of(step),
            "…and the bar must close on the beat so it loops in phase"
        );
        for &slot in &SONG_PULSE {
            if slot == SONG_ACCENT {
                continue;
            }
            assert!(
                (-5..=4).contains(&slot),
                "a ghost {slot} degrees out leaves the melody's register \
                 (an octave down is the floor: further and the accompaniment \
                 lands in the space downbeat's band)"
            );
            assert_ne!(
                slot, 0,
                "a ghost on the melody's own degree is a second voice at the \
                 SAME frequency as the accent still ringing above it, with \
                 randomised phase — a comb filter, not an accompaniment"
            );
        }
        // The GHOST FIGURE must actually move: a single repeated offset is a
        // pedal drone, which is the defect the melody generator itself was
        // rewritten to remove.
        let distinct: std::collections::BTreeSet<i8> = SONG_PULSE
            .iter()
            .copied()
            .filter(|&s| s != SONG_ACCENT)
            .collect();
        assert!(
            distinct.len() >= 3,
            "the accompaniment must be a FIGURE, not a pedal: {distinct:?}"
        );
    }

    /// THE MELODY MOVES AT A MUSICAL RATE. This is the whole point of the bar
    /// and it is a measurement, not an intention: on a plain typing stream the
    /// phrase generator must step roughly once per [`SONG_PULSE`] beat, not
    /// once per keystroke.
    ///
    /// The shipped build this replaces sang ten melody notes a second at
    /// 10 cps. No music is ten melody notes a second; at that density attacks
    /// mask pitch and the line reads as texture however good the notes are.
    #[test]
    fn the_melody_sings_at_a_third_of_typing_speed() {
        let mut s = TrailSynth::new(48_000.0, 0x50_4E_47);
        let mut buf = [0.0f32; 9600]; // 100 ms — 10 cps, every note admitted
        const KEYS: u32 = 300;
        for _ in 0..KEYS {
            s.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
            s.render(&mut buf);
        }
        let accents = SONG_PULSE.iter().filter(|&&x| x == SONG_ACCENT).count() as f32;
        let expect = KEYS as f32 * accents / SONG_PULSE.len() as f32;
        let got = s.song_notes as f32;
        assert!(
            (got - expect).abs() <= expect * 0.12,
            "the melody sang {got} notes for {KEYS} keystrokes; the bar asks \
             for about {expect}"
        );
        assert!(
            got * 2.5 < KEYS as f32,
            "…and it must be well under a note per keystroke, which is the \
             density this exists to cut"
        );
    }

    /// A GHOST ACCOMPANIES: it does not move the melody, it is quieter than
    /// the accent it follows, and it is SHORTER — the masking half of the fix,
    /// which matters more than the level half. At 10 cps the keystrokes are
    /// 100 ms apart and a glass-bell note is ~135 ms, so every note used to
    /// overlap its neighbour.
    #[test]
    fn a_ghost_is_quieter_and_shorter_than_the_accent_it_follows() {
        let mut s = TrailSynth::new(48_000.0, 0x6805_7000);
        let mut buf = [0.0f32; 9600];
        let key = |s: &mut TrailSynth, buf: &mut [f32]| -> (f32, f32, i32, u32) {
            let before: [bool; MAX_VOICES] = core::array::from_fn(|i| s.voices[i].on);
            s.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
            let v = s
                .voices
                .iter()
                .enumerate()
                .find(|(i, v)| v.on && !before[*i])
                .map(|(_, v)| (v.gl.hypot(v.gr), v.dur))
                .expect("a keystroke speaks");
            let out = (v.0, v.1, s.walk, s.song_notes);
            s.render(buf);
            out
        };
        // Slot 0 is the downbeat: an ACCENT.
        let (a_gain, a_dur, a_walk, a_notes) = key(&mut s, &mut buf);
        // Slot 1 is a GHOST.
        let (g_gain, g_dur, g_walk, g_notes) = key(&mut s, &mut buf);
        assert_eq!(
            g_notes,
            a_notes,
            "a ghost must not step the melody (it sang {} times)",
            g_notes - a_notes
        );
        assert_eq!(g_walk, a_walk, "…so the melody degree is unchanged");
        assert!(
            g_gain < a_gain * 0.75,
            "a ghost is audibly under its accent ({g_gain} vs {a_gain})"
        );
        assert!(
            g_gain > a_gain * 0.35,
            "…but not so far under that it reads as a key that failed to fire"
        );
        assert!(
            g_dur < a_dur * 0.8,
            "a ghost is SHORTER, so two thirds of the notes stop overlapping \
             their neighbours ({g_dur} vs {a_dur})"
        );
    }

    /// PUNCTUATION LANDS. An Enter, a kill or a jump is never an accompaniment
    /// note: it always sings, at full level, and an Enter resets the bar so
    /// the new phrase opens on its downbeat. Same for the note after a real
    /// typing PAUSE — the note after a think is the tune, never the
    /// accompaniment.
    #[test]
    fn punctuation_and_the_note_after_a_pause_always_sing() {
        for kind in [SoundKind::Jump, SoundKind::Kill, SoundKind::Navigation] {
            let mut s = TrailSynth::new(48_000.0, 0x9011_1CE5);
            let mut buf = [0.0f32; 9600];
            // Land on a GHOST slot, then punctuate.
            s.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
            s.render(&mut buf);
            let sung = s.song_notes;
            s.push(ev(GlowStyle::RainbowKitty, kind));
            assert!(
                s.song_notes > sung && s.song_accent,
                "{kind:?} must land as a melody note, not a ghost"
            );
        }
        // A PAUSE re-opens the bar on its downbeat.
        let mut s = TrailSynth::new(48_000.0, 0x9011_1CE6);
        let mut buf = [0.0f32; 9600];
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
        s.render(&mut buf);
        assert_ne!(s.song_pulse, 0, "fixture: mid-bar");
        // Longer than PHRASE_PAUSE_S of silence.
        for _ in 0..8 {
            s.render(&mut buf);
        }
        let sung = s.song_notes;
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
        assert!(
            s.song_notes > sung && s.song_accent,
            "the first note after a think must be the tune"
        );
        assert_eq!(s.song_pulse, 1, "…from the top of the bar");
    }

    // -- phrase-aware melody proofs (structural, not byte) ------------------

    /// A MOTIF RECURS: the phrase generator replays a 4-note CELL to fill the
    /// phrase, so the same delta shape returns — the thing a memoryless walk
    /// (one state integer, note n+1 depends only on note n) can NEVER do. With
    /// a length-8 phrase and a forced motif, the cell at note positions 1-3
    /// reappears verbatim at positions 5-7 (the second pass), proving the
    /// melody is built from a reusable motif rather than per-note randomness.
    #[test]
    fn a_motif_recurs_across_the_phrase() {
        let mut s = TrailSynth::new(48_000.0, 0x0DDB_A11E);
        // Force a known mid-phrase state (a length-8 phrase, a fixed cell), so
        // the note positions are predictable. Reads/writes of private fields
        // are fair game — the tests live in the module.
        s.tone = Tone::Technical;
        s.motif = [1, -1, 2, 0];
        s.phrase_len = 8;
        s.phrase_pos = 0;
        s.phrase_parity = false;
        s.phrase_step = 0;
        s.phrase_home = 0;
        s.walk = 0;
        let mut deltas = [0i32; 8];
        for d in &mut deltas {
            // Keep every note ADMITTED (force the gap open) and IN-PHRASE (no
            // pause, no Jump), so advance_melody takes the motif-step branch.
            // Keystrokes that land on a GHOST slot of the bar do not step the
            // generator at all, so keep pushing until the melody SINGS: what
            // this proves is a property of the phrase generator, and the bar
            // only decides how often it is asked (see `SONG_PULSE`).
            let sung = s.song_notes;
            let before = s.phrase_step;
            while s.song_notes == sung {
                s.since_voice = 1.0;
                s.since_event = 0.0;
                s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
            }
            *d = s.phrase_step - before;
        }
        assert_eq!(
            &deltas[1..4],
            &deltas[5..8],
            "the motif cell must recur across the phrase (positions 1-3 == 5-7): {deltas:?}"
        );
        // And the cell is the FORCED motif (not fresh randomness per note).
        assert_eq!(
            &deltas[1..4],
            &[-1, 2, 0],
            "the recurring cell is the motif"
        );
    }

    /// A PHRASE RESOLVES: Enter (a Jump), like a comma-length typing pause,
    /// CADENCES the melody onto the tonic — degree 0 or its octave 5, the
    /// pentatonic root pitch class. This is what makes phrases LAND instead of
    /// drifting off the top of the register forever: the walk wanders during
    /// the phrase, then the Enter snaps it home.
    #[test]
    fn a_phrase_resolves_toward_the_tonic_on_enter() {
        let mut s = TrailSynth::new(48_000.0, 0xCADE_5EED);
        let mut buf = [0.0f32; 256];
        // Type a handful of notes so the walk climbs off the tonic.
        for _ in 0..5 {
            s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
            for _ in 0..2 {
                s.render(&mut buf); // ~5 ms: well under the phrase-pause gap
            }
        }
        // Now an Enter — the cadence must land the melody on a tonic degree.
        s.push(ev(GlowStyle::Lumen, SoundKind::Jump));
        assert!(
            s.walk == 0 || s.walk == 5,
            "Enter must cadence the melody onto a tonic (degree 0 or 5), got {}",
            s.walk
        );
    }

    /// Walk `n` typed notes of one tone and return the degree sequence — the
    /// melody as integers, with no audio in the way.
    fn walk_sequence(tone: Tone, seed: u32, n: usize) -> Vec<i32> {
        let mut s = TrailSynth::new(48_000.0, seed);
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            // Keep every note admitted and in-phrase: no gap, no thinning.
            s.since_voice = 1.0;
            s.since_event = 0.0;
            let sung = s.song_notes;
            s.push(SoundEvent {
                tone,
                ..ev(GlowStyle::Lumen, SoundKind::Typed)
            });
            // THE MELODY'S sequence, not the keystrokes': a keystroke on a
            // GHOST slot of the bar accompanies the current note rather than
            // stepping to a new one (see `SONG_PULSE`), so sampling `walk` per
            // keystroke would read the bar's rhythm as the melody droning.
            if s.song_notes != sung {
                out.push(s.walk);
            }
        }
        out
    }

    /// THE MELODY MOVES. A phrase generator whose steps cancel against its own
    /// contour is a drone with extra state: before the arc was made orthogonal
    /// to the motif (see [`ARC_LIFT`]) and zero deltas were removed from the
    /// draw, 54 % of consecutive typed notes were the SAME PITCH — measured
    /// over a 12.5 s script, and audible as the "scatter of pitches that never
    /// goes anywhere" the owner heard. This pins the defect shut for every
    /// tone: fewer than a quarter of steps may repeat, and the line must use a
    /// real spread of its register.
    #[test]
    fn the_typed_line_moves_more_than_it_repeats() {
        for tone in [
            Tone::Technical,
            Tone::Calm,
            Tone::Excited,
            Tone::Frustrated,
            Tone::Playful,
        ] {
            let (lo, hi) = tone_register(tone);
            for seed in [1u32, 0x5EED, 0xA11CE, 0xBEEF] {
                let seq = walk_sequence(tone, seed, 400);
                let steps = seq.windows(2).count();
                let unison = seq.windows(2).filter(|w| w[0] == w[1]).count();
                let pct = unison as f32 / steps as f32 * 100.0;
                assert!(
                    pct < 25.0,
                    "{tone:?}/{seed:#x}: {pct:.1}% of the melody's steps repeat a pitch — \
                     the line is droning, not singing"
                );
                let (mn, mx) = seq
                    .iter()
                    .fold((i32::MAX, i32::MIN), |(a, b), &d| (a.min(d), b.max(d)));
                assert!(
                    mn >= lo && mx <= hi,
                    "{tone:?}: the walk left its register: {mn}..{mx} vs {lo}..{hi}"
                );
                assert!(
                    mx - mn >= 4,
                    "{tone:?}: the walk used only {} degrees of its register",
                    mx - mn
                );
            }
        }
    }

    /// THE CONTOUR ARC AND THE MOTIF ARE ORTHOGONAL — the property the whole
    /// fix rests on. The motif walks in steps of [`melody_span`]; the arc moves
    /// in ONE jump of [`ARC_LIFT`]. If a single arc move can equal a single
    /// motif step the two cancel and the note repeats, which is exactly what
    /// `round(2·sin)` (an arc that climbed ±1 at a time) did to every mood.
    ///
    /// PLAYFUL is the deliberate exception: skipping as wide as the arc IS its
    /// character (`melody_span` 2, the whimsy knob), so for that one mood a
    /// cancellation is possible on the ~3 of 7 transitions where the arc moves.
    /// It is held instead by the empirical bound in
    /// `the_typed_line_moves_more_than_it_repeats`, which covers every tone.
    #[test]
    fn the_contour_arc_cannot_cancel_a_motif_step() {
        for tone in [
            Tone::Technical,
            Tone::Calm,
            Tone::Excited,
            Tone::Frustrated,
            Tone::Playful,
        ] {
            if tone == Tone::Playful {
                assert_eq!(melody_span(tone), ARC_LIFT, "the documented exception");
                continue;
            }
            assert!(
                ARC_LIFT > melody_span(tone),
                "{tone:?}: an arc lift of {ARC_LIFT} is reachable by one motif step of \
                 {} — the two can cancel into a unison",
                melody_span(tone)
            );
        }
        // And the arc really is a single move, not a climb: over any phrase
        // length it takes exactly two values, 0 and ARC_LIFT.
        for len in PHRASE_MIN..=PHRASE_MAX {
            for pos in 0..len {
                let frac = f32::from(pos) / f32::from(len);
                let arc = ARC_LIFT * (core::f32::consts::PI * frac).sin().round() as i32;
                assert!(
                    arc == 0 || arc == ARC_LIFT,
                    "arc took an intermediate value {arc} at {pos}/{len}"
                );
            }
        }
    }

    /// The REGISTER FOLD is total, in range, and — unlike the clamp it
    /// replaced — never turns two different registers into one pitch.
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

    // -- THE GESTURE FAMILY -------------------------------------------------

    /// THE RULE, as a table. Direction signs the offset; scale sets the note
    /// count and the stride; a WORD gesture lands exactly
    /// [`GESTURE_WORD_CHARS`] character-steps out. Everything else in the
    /// engine reads its family geometry from this one function, so this test
    /// IS the specification.
    #[test]
    fn the_gesture_family_is_one_rule() {
        let typed = gesture_shape(SoundKind::Typed);
        assert_eq!(
            (typed.dir, typed.notes, typed.step, typed.offset),
            (1, 1, 0, 0),
            "a keystroke is the reference: forward, one note, on the degree"
        );
        for dir in [1i8, -1] {
            let glide = gesture_shape(SoundKind::Glide { dir });
            let sweep = gesture_shape(SoundKind::Sweep { dir });
            assert_eq!(glide.dir, dir);
            assert_eq!(sweep.dir, dir);
            assert_eq!(glide.notes, 1, "a character motion is one note");
            assert_eq!(
                glide.offset,
                i32::from(dir) * GESTURE_CHAR_STEP,
                "a character motion travels one character"
            );
            // THE SCALE LAW: the word motion's LAST note lands where a
            // character motion of word size would.
            let word_travel = sweep.offset + sweep.step * (sweep.notes as i32 - 1);
            assert_eq!(
                word_travel,
                i32::from(dir) * GESTURE_CHAR_STEP * GESTURE_WORD_CHARS,
                "a word motion must land {GESTURE_WORD_CHARS} character-steps out"
            );
            assert_eq!(
                sweep.step, glide.offset,
                "and it must get there by REPEATING the character step, not by leaping"
            );
        }
        // Gestures outside the family take the neutral shape, so no caller has
        // to branch on membership. BACKSPACE is one of them now: the deletion
        // left the pitched family when it became a poof, and this arm is what
        // keeps a future edit from quietly putting a lattice degree back under
        // it (which is exactly the sound the owner rejected).
        for kind in [
            SoundKind::Jump,
            SoundKind::Kill,
            SoundKind::Land,
            SoundKind::Navigation,
            SoundKind::Backspace,
        ] {
            let s = gesture_shape(kind);
            assert_eq!((s.dir, s.notes, s.step, s.offset), (1, 1, 0, 0), "{kind:?}");
        }
        // The BEND is directional and exactly mirrored: forward arrives from
        // below, inverse from above, both landing on the note.
        let (up0, up1) = gesture_bend(440.0, 1);
        let (dn0, dn1) = gesture_bend(440.0, -1);
        assert_eq!((up1, dn1), (440.0, 440.0), "both land on the note");
        assert!(up0 < 440.0 && dn0 > 440.0, "and enter from opposite sides");
        assert!((up0 * GESTURE_BEND - 440.0).abs() < 1e-3);
        assert!((dn0 / GESTURE_BEND - 440.0).abs() < 1e-3);
    }

    /// Spawn the family gesture `kind` from a SETTLED melody state and report
    /// the synth (for its degree) plus its live voices' `(landing pitch, entry
    /// pitch)` pairs — the loudest partial of each — ordered by pitch.
    fn family_voices(style: GlowStyle, kind: SoundKind) -> (TrailSynth, Vec<(f32, f32)>) {
        family_voices_in(SoundVoice::Style, style, kind)
    }

    /// [`family_voices`] under an explicit typing-sound VOICE (the picker's
    /// standalone instruments speak from the same settled melody state).
    fn family_voices_in(
        voice: SoundVoice,
        style: GlowStyle,
        kind: SoundKind,
    ) -> (TrailSynth, Vec<(f32, f32)>) {
        let mut s = TrailSynth::new(48_000.0, 0xFA_1117);
        let mut buf = [0.0f32; 1024]; // 512 frames ≈ 10.7 ms
        // THREE settling keystrokes, each followed by ~270 ms — under
        // PHRASE_PAUSE_S, so the probe stays in the same phrase, and enough of
        // them that the probe itself lands on the bar's next ACCENT (the beat
        // falls every third keystroke — see `SONG_PULSE`). A gesture measured
        // on a ghost slot would be read against the accompaniment rather than
        // against the tune. Palettes whose voices run longer than the settle
        // (Sparkle's 0.55 s chimes, Comet's drift) are handled by DIFFING the
        // voice pool rather than by waiting: what is measured is what THIS
        // gesture spawned.
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

    /// A DELETION UNDOES: it leaves the song exactly where the letters put it.
    /// Type five, delete five, type five again and the tune must have walked
    /// FIVE notes, not ten — otherwise the melody runs ahead of the text every
    /// time the typist corrects themselves, which is most of the time.
    #[test]
    fn a_deletion_does_not_advance_the_song() {
        let mut a = TrailSynth::new(48_000.0, 0xD0_1E_7E);
        let mut b = TrailSynth::new(48_000.0, 0xD0_1E_7E);
        let mut buf = [0.0f32; 1024]; // ~10.7 ms, clear of every gap
        for i in 0..5 {
            a.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
            b.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
            for _ in 0..9 {
                a.render(&mut buf);
                b.render(&mut buf);
            }
            let _ = i;
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
            a.walk, b.walk,
            "twelve corrections moved the song by {} degrees",
            b.walk - a.walk
        );
        assert_eq!(
            (a.phrase_pos, a.phrase_step),
            (b.phrase_pos, b.phrase_step),
            "…and they must not have moved the phrase state either"
        );
    }

    /// THE DOWNBEAT IS FIXED: a Space plays the speaking palette's own tonic,
    /// octave-folded into ONE bass register, wherever the melody happens to be
    /// standing — and it composes nothing.
    ///
    /// The rule it replaces derived the pitch from `walk` through the
    /// cadence's nearest-tonic test, so a space landed an OCTAVE apart
    /// depending on a melody degree the ear cannot connect to the text. That
    /// is the second half of this proof: the pitch is asserted to be the same
    /// from two DIFFERENT melody states.
    #[test]
    fn the_space_is_a_fixed_bass_downbeat() {
        let (s, spaces) = family_voices(GlowStyle::RainbowKitty, SoundKind::Space);
        assert_eq!(spaces.len(), 1, "the downbeat is one voice");
        let (land, enter) = spaces[0];
        assert!(
            (enter - land).abs() < 0.5,
            "a downbeat arrives, it does not lean: {enter} -> {land}"
        );
        let anchor = palette_for(SoundVoice::Style, GlowStyle::RainbowKitty).anchor_hz();
        let expect = s.melody_hz(bass_octave(anchor), 0);
        assert!(
            (land - expect).abs() < 0.5,
            "the downbeat lands on the palette's octave-folded tonic: got \
             {land}, expected {expect}"
        );
        assert!(
            (SPACE_BASS_LO_HZ..SPACE_BASS_LO_HZ * 2.0).contains(&land),
            "…inside the ONE bass register ({land} Hz)"
        );
        // INDEPENDENT OF THE MELODY. Walk the tune to a different degree and
        // the downbeat must not move a cent.
        let mut t = TrailSynth::new(48_000.0, 0xFA_1117);
        let mut buf = [0.0f32; 1024];
        let mut moved = false;
        for _ in 0..40 {
            t.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
            for _ in 0..6 {
                t.render(&mut buf);
            }
            if t.walk != s.walk {
                moved = true;
                break;
            }
        }
        assert!(moved, "fixture: the melody must reach a different degree");
        let before: [bool; MAX_VOICES] = core::array::from_fn(|i| t.voices[i].on);
        t.since_voice = 1.0;
        t.push(ev(GlowStyle::RainbowKitty, SoundKind::Space));
        let far = t
            .voices
            .iter()
            .enumerate()
            .filter(|(i, v)| v.on && !before[*i])
            .filter_map(|(_, v)| (v.p[0].lvl > 0.0).then_some(v.p[0].f1))
            .next()
            .expect("the downbeat speaks");
        assert!(
            (far - land).abs() < 0.5,
            "the downbeat must not follow the melody: {far} Hz at degree {} \
             against {land} Hz at degree {}",
            t.walk,
            s.walk
        );
        // The downbeat composes nothing: phrase position and degree are
        // exactly what one Typed probe leaves behind (family_voices pushes
        // Typed first, then the probed kind).
        let (tp, _) = family_voices(GlowStyle::RainbowKitty, SoundKind::Typed);
        assert_eq!(
            s.phrase_pos,
            tp.phrase_pos.saturating_sub(1),
            "a space must not step the song (the Typed probe stepped once more)"
        );
    }

    /// A WHITESPACE RUN IS ONE GESTURE: indentation and a run of blanks get
    /// ONE bass downbeat, and the rest of the run answers with air alone —
    /// never silence (a key that makes no sound reads as a dropped keystroke)
    /// and never a second bass note (four stacked roots is a kick drum).
    #[test]
    fn a_whitespace_run_lands_one_downbeat() {
        let mut s = TrailSynth::new(48_000.0, 0x5A_CE_00);
        // ~52 ms: past MIN_GAP (so every space is ADMITTED and the coalescing
        // under test is the run law, not the governor) and inside SPACE_DUR_S
        // (so a stacked root would still be sounding to catch).
        let mut buf = [0.0f32; 5000];
        let bass = |s: &TrailSynth| s.voices.iter().filter(|v| v.on && v.bass).count();
        let space = |s: &mut TrailSynth| {
            let before = s.live_voices();
            s.push(ev(GlowStyle::RainbowKitty, SoundKind::Space));
            s.live_voices() - before
        };
        // The HEAD of the run: one bass downbeat.
        assert_eq!(space(&mut s), 1, "the head speaks");
        assert_eq!(bass(&s), 1, "…as a bass downbeat");
        // The TAIL: still audible, never a second root.
        for i in 0..3 {
            s.render(&mut buf);
            assert_eq!(
                space(&mut s),
                1,
                "space {i} of the run must still SPEAK — a key that makes no \
                 sound reads as a dropped keystroke"
            );
            assert!(
                bass(&s) <= 1,
                "a run of spaces must never stack bass roots (found {})",
                bass(&s)
            );
            assert!(
                s.voices
                    .iter()
                    .any(|v| v.on && !v.bass && v.n_lvl > 0.0 && v.p.iter().all(|p| p.lvl <= 0.0)),
                "…the run's tail is AIR: a breath with no tone under it"
            );
        }
        // A letter CLOSES the run: the next space opens a new one.
        s.render(&mut buf);
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
        s.render(&mut buf);
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Space));
        assert!(
            s.voices.iter().any(|v| v.on && v.bass && v.damp <= 0.0),
            "a word boundary after a letter is a fresh downbeat"
        );
    }

    /// THE DOWNBEAT IS MONOPHONIC. Two voices at one FIXED frequency with
    /// independently randomised phase are a comb filter, not a bass note: the
    /// pair can cancel to near silence or sum to +6 dB on phase luck alone.
    /// A new word boundary damps the live root instead of stacking on it.
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
            undamped, 1,
            "exactly one bass root may be sounding at a time (of {} live)",
            live.len()
        );
        assert!(
            live.iter().any(|v| v.damp > 0.0 && v.damp0 == SPACE_DAMP_S),
            "the previous root is released, not cut: a hard stop clicks"
        );
    }

    /// THE LIFT LEANS INTO THE NEXT KEYSTROKE — one whisper a lattice step
    /// ABOVE the melody, entered from below (the deletion's exact mirror) —
    /// and its admission never claims the min-gap beat: a capital typed at
    /// speed (shift, then the letter inside one MIN_GAP) still clicks.
    #[test]
    fn shift_is_the_lift_and_never_claims_the_beat() {
        let (s, lifts) = family_voices(GlowStyle::RainbowKitty, SoundKind::Shift);
        assert_eq!(lifts.len(), 1, "the lift is one voice");
        let (land, enter) = lifts[0];
        assert!(
            enter < land,
            "the lift enters from below like the keystroke it announces: \
             {enter} -> {land}"
        );
        let anchor = palette_for(SoundVoice::Style, GlowStyle::RainbowKitty).anchor_hz();
        let expect = s.melody_hz(anchor, s.walk + GESTURE_CHAR_STEP);
        assert!(
            (land - expect).abs() < 0.5,
            "the lift sits one step above the melody: got {land}, expected {expect}"
        );
        // The beat stays unclaimed: shift, then a letter INSIDE the min-gap —
        // the letter must still speak.
        let mut s = TrailSynth::new(48_000.0, 0xC0FF_EE01);
        s.push(ev(GlowStyle::Lumen, SoundKind::Shift));
        let before = s.voices.iter().filter(|v| v.on).count();
        // No render between the two pushes: since_voice is 0 s if shift had
        // claimed it, MIN_GAP-satisfying only because it did not.
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        let after = s.voices.iter().filter(|v| v.on).count();
        assert!(
            after > before,
            "the capital right behind a shift must not be thinned by its own \
             grace note ({before} -> {after} voices)"
        );
    }

    /// THE LADDER HOLDS THROUGH THE NEW FAMILY, relatively and for EVERY
    /// voice in the roster: a correction (felt layer included) stays under
    /// the keystroke it undoes, the comma stays at or under the letters it
    /// separates, and the lift whispers under all of them. Relative pins on
    /// purpose — the law is the ORDER, not a dBFS figure per palette.
    #[test]
    fn the_ladder_holds_for_the_key_family() {
        fn peak(voice: SoundVoice, kind: SoundKind) -> f32 {
            let mut s = TrailSynth::new(48_000.0, 0x5EED_1234);
            let mut e = voiced(voice, GlowStyle::RainbowKitty, kind);
            e.hue = 0.0;
            e.bed = false;
            s.push(e);
            let mut buf = vec![0.0f32; (48_000.0f32 * 2.4) as usize * CHANNELS];
            s.render(&mut buf);
            buf.iter().fold(0.0f32, |m, v| m.max(v.abs()))
        }
        for &voice in SoundVoice::ALL {
            let typed = peak(voice, SoundKind::Typed);
            let back = peak(voice, SoundKind::Backspace);
            let space = peak(voice, SoundKind::Space);
            let shift = peak(voice, SoundKind::Shift);
            assert!(
                back < typed,
                "{voice:?}: the correction may not out-shout the keystroke \
                 (backspace {back} vs typed {typed})"
            );
            // One isolated-seed grace: the ORDER is the law, with a small
            // per-voice tolerance for the comma (its low register can meter
            // hot against a bright palette's mid).
            assert!(
                space <= typed * 1.05,
                "{voice:?}: the comma must not rise over the letters \
                 (space {space} vs typed {typed})"
            );
            assert!(
                shift < back && shift < space,
                "{voice:?}: the lift is the quietest of the family \
                 (shift {shift} vs backspace {back} / space {space})"
            );
        }
    }

    /// THE ERASE POOF IS THE WHOLE DELETION, in every voice of the roster: two
    /// NOISE-ONLY voices (a delay-0 body — the first-buffer latency law — and
    /// a delayed air cap) and NOTHING ELSE. The "and nothing else" is the
    /// point: the owner reported deletions twice as "the normal key press",
    /// and the cause was a pitched palette voice speaking under the noise
    /// layers. If a palette voice ever comes back, this fails.
    #[test]
    fn the_deletion_is_a_poof_and_only_a_poof() {
        for &voice in SoundVoice::ALL {
            for style in STYLES {
                let mut s = TrailSynth::new(48_000.0, 0xFE17_0FF5);
                let mut e = voiced(voice, style, SoundKind::Backspace);
                e.bed = false;
                s.push(e);
                let live: Vec<&Voice> = s.voices.iter().filter(|v| v.on).collect();
                assert_eq!(
                    live.len(),
                    2,
                    "{voice:?}/{style:?}: a deletion is the body and the cap — \
                     no palette voice may speak under them"
                );
                for v in &live {
                    assert!(
                        v.p.iter().all(|p| p.lvl <= 0.0),
                        "{voice:?}/{style:?}: the poof carries NO tonal partial \
                         (a puff is air, not a note a step down)"
                    );
                    assert!(v.n_lvl > 0.0, "{voice:?}/{style:?}: …and is noise");
                    // THE CONTACT is static; THE DISPERSAL may SETTLE — and
                    // only settle. The body's band never moves (a swept
                    // contact reads as a falling whistle, which is the sound
                    // this replaced); the air cap breathes DOWN a bounded
                    // step (2026-08-29 beautification: the "-oof" relaxes),
                    // never up and never far — past a fourth it is a zap.
                    if v.delay == 0.0 {
                        assert!(
                            v.n_glide <= 0.0 && v.n_f0 == v.n_f1,
                            "{voice:?}/{style:?}: the BODY's band is static"
                        );
                    } else {
                        assert!(
                            v.n_f1 <= v.n_f0 && v.n_f1 >= v.n_f0 * 0.75,
                            "{voice:?}/{style:?}: the cap SETTLES a bounded \
                             step down ({} -> {})",
                            v.n_f0,
                            v.n_f1
                        );
                    }
                    assert!(
                        v.n_q < 1.0,
                        "{voice:?}/{style:?}: broad Q — a resonant burst whistles"
                    );
                    assert!(
                        v.attack >= 0.0015,
                        "{voice:?}/{style:?}: a sub-millisecond attack clicks"
                    );
                }
                let mut bands: Vec<f32> = live.iter().map(|v| v.n_f0).collect();
                bands.sort_by(f32::total_cmp);
                assert_eq!(
                    bands,
                    vec![POOF_BODY_HZ, POOF_AIR_HZ],
                    "{voice:?}/{style:?}: the body and the air cap"
                );
                let delays: Vec<f32> = live.iter().map(|v| v.delay).collect();
                assert!(
                    delays.contains(&0.0),
                    "{voice:?}/{style:?}: the body speaks in the first buffer"
                );
                assert!(
                    delays.contains(&POOF_AIR_DELAY_S),
                    "{voice:?}/{style:?}: the cap disperses behind it"
                );
            }
        }
    }

    /// THE ERASE IS AIRY AND ABOVE THE KEYSTROKE — the owner's ask
    /// ("backspaces are poofy higher notes") as a MEASUREMENT on the rendered
    /// signal rather than on the voice prototypes: the deletion's spectral
    /// centroid sits well over the keystroke's, its energy is overwhelmingly
    /// above 2 kHz, and it is UNPITCHED (no spectral spike over its own band
    /// floor).
    ///
    /// The shipped build this replaces measured the other way round: a
    /// deletion centroid of 1089 Hz against a keystroke's 1474 Hz. The erase
    /// was darker than the letter it removed, which is what "sounds like the
    /// normal key press" was describing.
    #[test]
    fn the_erase_is_an_airy_poof_above_the_keystroke() {
        /// Render one gesture alone after a settling keystroke and report
        /// `(peak, rms, spectral centroid Hz, energy fraction over 2 kHz,
        /// tonality)`.
        fn probe(kind: SoundKind) -> (f32, f32, f32, f32, f32) {
            let mut s = TrailSynth::new(48_000.0, 0x9007_F00F);
            // Three settling keystrokes, so the PROBE lands on the bar's next
            // ACCENT (slots 0..2 consumed, so the measured event is slot 3 —
            // see `SONG_PULSE`). Measuring a ghost against a poof would be
            // comparing the accompaniment to the erase, not the tune.
            let mut settle = vec![0.0f32; 16_384 * CHANNELS];
            for _ in 0..3 {
                let mut warm = ev(GlowStyle::RainbowKitty, SoundKind::Typed);
                warm.bed = false;
                s.push(warm);
                s.render(&mut settle);
            }
            assert_eq!(
                SONG_PULSE[usize::from(s.song_pulse)],
                SONG_ACCENT,
                "fixture: the probe must be measured on an accent"
            );
            let mut e = ev(GlowStyle::RainbowKitty, kind);
            e.bed = false;
            s.push(e);
            let n = 24_000usize; // 0.5 s
            let mut buf = vec![0.0f32; n * CHANNELS];
            s.render(&mut buf);
            let mono: Vec<f32> = (0..n).map(|i| 0.5 * (buf[i * 2] + buf[i * 2 + 1])).collect();
            let peak = mono.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            let rms = (mono.iter().map(|v| v * v).sum::<f32>() / n as f32).sqrt();
            // Goertzel-free centroid: a coarse DFT over 64 log-ish bands is
            // enough to separate 1 kHz from 5 kHz and needs no FFT here.
            let (mut num, mut den) = (0.0f64, 0.0f64);
            let mut over2k = 0.0f64;
            let mut peak_bin = 0.0f64;
            let mut bins: Vec<f64> = Vec::new();
            for k in 1..=160 {
                let hz = k as f32 * 50.0; // 50 Hz .. 8 kHz
                let (mut re, mut im) = (0.0f64, 0.0f64);
                let w = core::f32::consts::TAU * hz / 48_000.0;
                for (i, &x) in mono.iter().enumerate().take(4_800) {
                    let ph = w * i as f32;
                    re += f64::from(x) * f64::from(ph.cos());
                    im += f64::from(x) * f64::from(ph.sin());
                }
                let e = re * re + im * im;
                bins.push(e.sqrt());
                peak_bin = peak_bin.max(e.sqrt());
                num += e * f64::from(hz);
                den += e;
                if hz > 2000.0 {
                    over2k += e;
                }
            }
            bins.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let med = bins[bins.len() / 2].max(1e-12);
            (
                peak,
                rms,
                if den < 1e-15 { 0.0 } else { (num / den) as f32 },
                if den < 1e-15 { 0.0 } else { (over2k / den) as f32 },
                (peak_bin / med) as f32,
            )
        }
        let (t_peak, t_rms, t_cent, _, t_tone) = probe(SoundKind::Typed);
        let (b_peak, b_rms, b_cent, b_hi, b_tone) = probe(SoundKind::Backspace);
        assert!(
            b_cent > t_cent * 1.35,
            "the erase must sit ABOVE the keystroke: centroid {b_cent:.0} Hz \
             vs the keystroke's {t_cent:.0} Hz"
        );
        // RECALIBRATED 0.60 → 0.55 with the deliberate 2026-08-29 settle
        // ([`POOF_AIR_SETTLE_HZ`]): the cap now breathes DOWN as it disperses,
        // which was measured to move this fixture 0.612 → 0.597 — the fixture
        // meters the poof OVER the residual ring of its three warm-up
        // keystrokes, so its absolute value sits well under the isolated
        // poof's (75 %+) and the old floor had ~1 % of margin. The claim
        // pinned is unchanged in kind: the erase's energy lives overwhelmingly
        // in the air band, and the centroid law above is the stronger half.
        assert!(
            b_hi > 0.55,
            "the poof is AIR: only {:.0}% of its energy is over 2 kHz",
            b_hi * 100.0
        );
        assert!(
            b_tone * 4.0 < t_tone,
            "the poof must be UNPITCHED — its spectrum peaks {b_tone:.1}x over \
             its own floor where the keystroke peaks {t_tone:.1}x"
        );
        assert!(
            b_peak < t_peak,
            "a correction may not out-peak the character it removes \
             ({b_peak} vs {t_peak})"
        );
        // Codex's mix bound: the deletion's short-term energy at least 6 dB
        // under the keystroke's, so a burst of corrections cannot out-shout
        // the typing it is undoing.
        let quieter_db = 20.0 * (f64::from(t_rms) / f64::from(b_rms).max(1e-9)).log10();
        assert!(
            quieter_db >= 6.0,
            "the erase must sit at least 6 dB under the keystroke in energy; \
             measured {quieter_db:.1} dB"
        );
    }

    /// THE POOF IS ONE SOUND IN EVERY LOOK. The deletion interval used to be
    /// spelled per palette — −2 in three of them, −3 in one, absent in six —
    /// then unified onto one shared lattice offset, which is the arithmetic
    /// the owner heard as "the normal key press". It is now a kind-level voice
    /// designed BEFORE palette dispatch, like the Kill swoosh and the Land
    /// chime: a deletion sounds like a deletion whatever is on screen, and no
    /// style contributes anything to it.
    #[test]
    fn every_palette_erases_with_the_same_poof() {
        let render = |style: GlowStyle| {
            let mut s = TrailSynth::new(48_000.0, 0x9E_11_5E);
            let mut e = ev(style, SoundKind::Backspace);
            e.bed = false;
            s.push(e);
            let mut buf = vec![0.0f32; 12_000 * CHANNELS];
            s.render(&mut buf);
            buf
        };
        let reference = render(GlowStyle::Lumen);
        for style in STYLES {
            let got = render(style);
            assert!(
                got.iter()
                    .zip(&reference)
                    .all(|(a, b)| (a - b).abs() < 1e-9),
                "{style:?}: the erase poof must be style-agnostic"
            );
        }
    }

    /// A WORD MOTION IS A CHARACTER MOTION AT SCALE — the family's second
    /// claim, measured on the rendered voices: the run's notes step by the
    /// SAME interval a Glide travels, and its last note lands
    /// [`GESTURE_WORD_CHARS`] of those steps out.
    #[test]
    fn a_word_motion_is_a_character_motion_at_scale() {
        let anchor = palette_for(SoundVoice::Style, GlowStyle::RainbowKitty).anchor_hz();
        for dir in [1i8, -1] {
            let (gs, glide) = family_voices(GlowStyle::RainbowKitty, SoundKind::Glide { dir });
            let (ss, sweep) = family_voices(GlowStyle::RainbowKitty, SoundKind::Sweep { dir });
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
            // The character motion lands ONE step out…
            assert!(
                (glide[0].0 - gs.melody_hz(anchor, gs.walk + step)).abs() < 0.5,
                "dir {dir}: a character motion must land one step out"
            );
            // …and the word motion walks that SAME step, once per character.
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
            // And it is audibly BIGGER than the character motion it scales.
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

    /// The movement family sings in the PALETTE'S register, not one of its
    /// own: `Palette::anchor_hz` is the single register per style that the
    /// keystroke, the bonk and the cursor voices all read.
    #[test]
    fn cursor_motion_sings_in_the_palettes_register() {
        for style in [
            GlowStyle::RainbowKitty,
            GlowStyle::Sparkle,
            GlowStyle::Lumen,
        ] {
            let anchor = palette_for(SoundVoice::Style, style).anchor_hz();
            let (_, glide) = family_voices(style, SoundKind::Glide { dir: 1 });
            let f = glide[0].0;
            assert!(
                f > anchor * 0.4 && f < anchor * 4.0,
                "{style:?}: a cursor note at {f} Hz is not in its palette's \
                 register (anchor {anchor})"
            );
        }
    }

    // -- cursor-movement gesture proofs -------------------------------------

    /// Total spawned-voice energy proxy (Σ gl²+gr² over live voices) — a
    /// loudness stand-in for "softer than a keystroke" comparisons.
    fn voice_energy(s: &TrailSynth) -> f32 {
        s.voices
            .iter()
            .filter(|v| v.on)
            .map(|v| v.gl * v.gl + v.gr * v.gr)
            .sum()
    }

    /// THE CLOUD'S LITTLE NOISE ([`SoundKind::Poof`]) — every admission property
    /// it needs in order to be HEARD at all, and every one it needs in order not
    /// to be a nuisance.
    ///
    /// THE ONE THAT MAKES OR BREAKS IT is the min-gap bypass. The puff is cued
    /// on the erase poof, and the Backspace bell that licensed that poof is
    /// drained from the SAME frame's cue list one slot ahead of it — well inside
    /// `MIN_GAP` (45 ms). A gap-thinned puff would therefore be thinned EVERY
    /// TIME, not occasionally: it would be dead code that unit tests pass and
    /// nobody ever hears. That is exactly the class of "green but inaudible" the
    /// visual half of this feature was also lost to, so it is pinned here, in the
    /// synth, at the seam that decides.
    #[test]
    fn the_clouds_puff_survives_the_bell_it_follows() {
        // 1. IT SPEAKS inside the min-gap, right behind the keystroke.
        let mut s = TrailSynth::new(48_000.0, 1);
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Backspace)); // since_voice ← 0
        let after_bell = s.live_voices();
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Poof));
        assert!(
            s.live_voices() > after_bell,
            "the puff must speak behind the bell it accompanies — under the gap \
             it would be silenced on every single delete"
        );

        // 2. IT DOES NOT CLAIM THE BEAT: the next keystroke still speaks. (A
        //    held Backspace lands its next key well inside one MIN_GAP of the
        //    puff, so a puff that reset the clock would eat the bell.)
        let mut s = TrailSynth::new(48_000.0, 1);
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Poof));
        let after_puff = s.live_voices();
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Backspace));
        assert!(
            s.live_voices() > after_puff,
            "the puff is an accompaniment: it must not thin the key that follows"
        );

        // 3. IT IS NOT A NOTE OF THE TUNE.
        let mut s = TrailSynth::new(48_000.0, 0x9A11);
        let walk = s.walk;
        s.push(ev(GlowStyle::RainbowKitty, SoundKind::Poof));
        assert_eq!(s.walk, walk, "a puff of air does not compose the melody");

        // 4. IT IS PITCHLESS — pure noise, no partials. A puff that carried a
        //    tone could land on a wrong note of the tune it plays under.
        let live: Vec<Voice> = s.voices.iter().filter(|v| v.on).copied().collect();
        assert_eq!(live.len(), 1, "the puff is ONE voice");
        assert!(
            live[0].p.iter().all(|p| p.lvl == 0.0),
            "the puff has no tonal partials"
        );
        assert!(live[0].n_lvl > 0.0, "…it is band-passed noise");
    }

    /// …AND IT IS THE SMALLEST THING IN THE VOCABULARY. The owner's brief for
    /// it: *"a puff, not a thud, and it must not fight the typing bell."*
    ///
    /// Two relative pins, per palette voice, because the law is the ORDER and
    /// not a dBFS figure:
    /// - UNDER THE BELL it layers on top of — anything else and the delete
    ///   sounds like two keys;
    /// - FAR under the [`SoundKind::Kill`] swoosh, which is the same gesture at
    ///   CLAUSE scale. This is the pin that refutes the shipped defect it
    ///   replaced, where a plain Backspace fired that full swoosh outright.
    #[test]
    fn the_puff_is_the_quietest_delete_in_the_ladder() {
        fn peak(voice: SoundVoice, kind: SoundKind) -> f32 {
            let mut s = TrailSynth::new(48_000.0, 0x5EED_1234);
            let mut e = voiced(voice, GlowStyle::RainbowKitty, kind);
            e.hue = 0.0;
            e.bed = false;
            s.push(e);
            let mut buf = vec![0.0f32; (48_000.0f32 * 2.4) as usize * CHANNELS];
            s.render(&mut buf);
            buf.iter().fold(0.0f32, |m, v| m.max(v.abs()))
        }
        for &voice in SoundVoice::ALL {
            let puff = peak(voice, SoundKind::Poof);
            let back = peak(voice, SoundKind::Backspace);
            let kill = peak(voice, SoundKind::Kill);
            assert!(puff > 0.0, "{voice:?}: the puff must be audible at all");
            assert!(
                puff < back,
                "{voice:?}: the puff rides UNDER the bell it follows \
                 (puff {puff} vs backspace {back})"
            );
            assert!(
                puff < kill * 0.6,
                "{voice:?}: a one-character puff is not a line kill \
                 (puff {puff} vs kill swoosh {kill})"
            );
        }
    }

    /// THE WORD KILL SITS BETWEEN THE POOF AND THE SWOOSH — the delete ladder's
    /// new middle rung, in every voice of the roster: a ^W is one WORD going,
    /// so it must be audibly bigger than a single character's poof and audibly
    /// smaller than the clause-scale line kill. And it is the poof's COUSIN in
    /// anatomy, not the swoosh's: two band-passed noise bursts (a delay-0
    /// body, a settling air cap), no tonal partial, terminal before palette
    /// dispatch, and — like every deletion — it does not step the song.
    #[test]
    fn the_word_kill_sits_between_the_poof_and_the_swoosh() {
        fn peak(voice: SoundVoice, kind: SoundKind) -> f32 {
            let mut s = TrailSynth::new(48_000.0, 0x5EED_1234);
            let mut e = voiced(voice, GlowStyle::RainbowKitty, kind);
            e.hue = 0.0;
            e.bed = false;
            s.push(e);
            let mut buf = vec![0.0f32; (48_000.0f32 * 2.4) as usize * CHANNELS];
            s.render(&mut buf);
            buf.iter().fold(0.0f32, |m, v| m.max(v.abs()))
        }
        for &voice in SoundVoice::ALL {
            let back = peak(voice, SoundKind::Backspace);
            let word = peak(voice, SoundKind::KillWord);
            let kill = peak(voice, SoundKind::Kill);
            assert!(
                word > back,
                "{voice:?}: a word leaving is BIGGER than a character leaving \
                 (word {word} vs backspace {back})"
            );
            assert!(
                word < kill,
                "{voice:?}: …and SOFTER than a clause leaving \
                 (word {word} vs kill swoosh {kill})"
            );
        }
        // ANATOMY, on every style: the poof family, and only the poof family.
        for style in STYLES {
            let mut s = TrailSynth::new(48_000.0, 0x0BAD_C0DE);
            let walk = s.walk;
            let mut e = ev(style, SoundKind::KillWord);
            e.bed = false;
            s.push(e);
            assert_eq!(s.walk, walk, "{style:?}: a deletion does not compose");
            let live: Vec<&Voice> = s.voices.iter().filter(|v| v.on).collect();
            assert_eq!(
                live.len(),
                2,
                "{style:?}: the word poof is the body and the cap — no palette \
                 voice may speak under them"
            );
            for v in &live {
                assert!(
                    v.p.iter().all(|p| p.lvl <= 0.0),
                    "{style:?}: the word poof carries NO tonal partial"
                );
                assert!(v.n_lvl > 0.0, "{style:?}: …and is noise");
            }
            let mut bands: Vec<f32> = live.iter().map(|v| v.n_f0).collect();
            bands.sort_by(f32::total_cmp);
            assert_eq!(
                bands,
                vec![WORD_POOF_BODY_HZ, WORD_POOF_AIR_HZ],
                "{style:?}: the body sits LOWER than the character poof's \
                 (a bigger cloud is a darker one)"
            );
            const {
                assert!(
                    WORD_POOF_BODY_HZ < POOF_BODY_HZ && WORD_POOF_AIR_HZ < POOF_AIR_HZ,
                    "the word poof is the same shape one size DOWN in band"
                );
            }
        }
    }

    /// A HELD DELETE RUN EARNS ONE SHIMMER, ON ITS LAST RELEASE — the pending
    /// glitter is re-booked by every admitted poof of the run and the previous
    /// booking is expired unheard (the switch-damp law at pre-delay), so
    /// however long the hold, exactly one shimmer survives and it is the LAST
    /// one. Short runs earn none, and typing again before the glitter sounds
    /// cancels it: the reward is for finishing the erase, not for erasing.
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
        // …and 100 ms after that it is SOUNDING, undamped: the run's one
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
        assert!(
            sounding[0].p.iter().all(|p| p.lvl <= 0.0) && sounding[0].n_lvl > 0.0,
            "the shimmer is air, not a note"
        );
        assert!(
            sounding[0].tw_depth > 0.0,
            "…and it twinkles — that is the shimmer"
        );

        // SHORT RUNS EARN NOTHING: three quick corrections book no glitter.
        let mut s = TrailSynth::new(48_000.0, 0x51D3_0002);
        for _ in 0..(HELD_ERASE_RUN_MIN - 1) {
            let mut e = ev(GlowStyle::RainbowKitty, SoundKind::Backspace);
            e.bed = false;
            s.push(e);
            s.render(&mut buf);
        }
        assert_eq!(pending(&s), 0, "a short run is not a held run");

        // TYPING CANCELS THE PENDING REWARD: hold, then type before it sounds.
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

    /// THE DOWNBEAT'S WARMTH AND ITS BREATH. At typing speed the space is the
    /// working bass — short, quiet, kick-drum-proof — now with a
    /// barely-audible FIFTH folded into the chord under the octave. After a
    /// PHRASE pause (the same [`PHRASE_PAUSE_S`] that resets the bar) the
    /// downbeat BREATHES: a longer bloom, the upper partials lifted a shade —
    /// the first beat of a fresh thought gets room the mid-sentence beats
    /// never take.
    #[test]
    fn the_downbeat_carries_a_fifth_and_breathes_after_a_rest() {
        let mut s = TrailSynth::new(48_000.0, 0xBA55_0001);
        let mut buf = vec![0.0f32; 4_800 * CHANNELS]; // 100 ms per render
        let mut warm = ev(GlowStyle::RainbowKitty, SoundKind::Typed);
        warm.bed = false;
        s.push(warm);
        s.render(&mut buf); // 100 ms — well inside the phrase
        let mut e = ev(GlowStyle::RainbowKitty, SoundKind::Space);
        e.bed = false;
        s.push(e);
        let at_speed: Vec<Voice> = s
            .voices
            .iter()
            .filter(|v| v.on && v.bass)
            .copied()
            .collect();
        assert_eq!(at_speed.len(), 1, "the downbeat is one voice");
        let v = at_speed[0];
        assert_eq!(v.dur, SPACE_DUR_S, "at speed, the working envelope");
        assert_eq!(v.p[1].lvl, SPACE_OCTAVE_LEVEL);
        assert_eq!(v.p[2].lvl, SPACE_FIFTH_LEVEL, "the fifth is present…");
        assert!(
            v.p[2].lvl < v.p[1].lvl,
            "…UNDER the octave: warmth, not a hummable note"
        );
        assert!(
            (v.p[2].f0 - v.p[0].f0 * 1.5).abs() < 0.01,
            "…and it is the perfect fifth of the root ({} vs {})",
            v.p[2].f0,
            v.p[0].f0 * 1.5
        );
        // THE REST. A keystroke closes the whitespace run, then the hand
        // lifts for a beat over the phrase threshold.
        let mut k = ev(GlowStyle::RainbowKitty, SoundKind::Typed);
        k.bed = false;
        s.push(k);
        for _ in 0..7 {
            s.render(&mut buf); // 700 ms > PHRASE_PAUSE_S
        }
        let before: Vec<bool> = s.voices.iter().map(|v| v.on && v.bass).collect();
        let mut e = ev(GlowStyle::RainbowKitty, SoundKind::Space);
        e.bed = false;
        s.push(e);
        let breathed: Vec<Voice> = s
            .voices
            .iter()
            .enumerate()
            .filter(|(i, v)| v.on && v.bass && !before[*i])
            .map(|(_, v)| *v)
            .collect();
        assert_eq!(breathed.len(), 1, "the rested downbeat is one voice");
        let b = breathed[0];
        assert_eq!(b.dur, SPACE_BREATHE_DUR_S, "after a rest, the bloom");
        assert_eq!(b.attack, SPACE_BREATHE_ATTACK_S);
        assert_eq!(b.decay, SPACE_BREATHE_DECAY_S);
        assert_eq!(b.p[1].lvl, SPACE_BREATHE_OCTAVE_LEVEL);
        assert_eq!(b.p[2].lvl, SPACE_BREATHE_FIFTH_LEVEL);
        assert_eq!(
            b.p[0].f0, v.p[0].f0,
            "breathing changes the ROOM, never the note: the root is fixed"
        );
        // The bloom is still no kick drum: shorter than a Kill swoosh, and
        // its whole life fits inside half a second of thought.
        assert!(b.dur < 0.28, "the bloom stays a downbeat, not a pad");
    }

    /// A GLIDE plays exactly ONE in-key tone, a scale-step in the travel
    /// direction of the melody's current degree, and sits ON the typing floor
    /// beside the keystroke it accompanies. It does NOT step the phrase (the
    /// cursor sings the tune, it doesn't compose it).
    #[test]
    fn glide_plays_one_soft_in_key_tone() {
        let mut s = TrailSynth::new(48_000.0, 0x6117);
        let walk = s.walk; // the current melodic degree (init 2)
        s.push(ev(GlowStyle::Lumen, SoundKind::Glide { dir: 1 }));
        assert_eq!(
            s.walk, walk,
            "a cursor glide must not step the phrase melody"
        );
        assert_eq!(s.live_voices(), 1, "a glide is a single tone");
        // The note the glide LANDS on (`f1`): the family's contour bend enters
        // it from a lattice step below (`gesture_bend`), so `f0` is the scoop,
        // not the pitch.
        let f = s
            .voices
            .iter()
            .find(|v| v.on)
            .map(|v| v.p[0].f1)
            .expect("a glide voice");
        // In-key: one scale-step up from the current degree, on the active
        // (Technical ⇒ PENTA) table, in the palette's own register.
        let expect = s.melody_hz(CURSOR_ANCHOR_HZ, walk + GESTURE_CHAR_STEP);
        assert!(
            (f - expect).abs() < 1.0,
            "glide pitch {f} must be the in-key step {expect}"
        );

        // LADDER TIER 1: typing is the FLOOR, and a glide — also a
        // per-character gesture — sits ON that floor with it, not beneath it.
        // What is pinned is that it never becomes a jump scare either way:
        // within one tier of a keystroke, in BOTH directions. The band is wide
        // on the upper side because a glide is style-agnostic (designed before
        // palette dispatch) while a keystroke carries `palette_trim`, so their
        // exact ratio legitimately varies by style — here Lumen trims to 0.95.
        let mut typed = TrailSynth::new(48_000.0, 0x6117);
        typed.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        let (g_e, t_e) = (voice_energy(&s), voice_energy(&typed));
        assert!(
            g_e >= t_e * 0.5 && g_e <= t_e * 2.5,
            "a glide must sit ON the typing floor, within one tier either way: \
             glide {g_e} vs keystroke {t_e}"
        );
    }

    /// A SWEEP plays a short RATE-LIMITED run of in-key scale-tones: exactly
    /// [`CURSOR_SWEEP_RUN`] notes, the first at `delay = 0` (so it speaks in
    /// the first synth buffer), the rest pre-delayed into a spread run (its own
    /// internal rate limit). A Sweep BYPASSES min-gap (the run isn't thinned
    /// mid-flight) while a Glide stays gap-thinned like a keystroke.
    #[test]
    fn sweep_plays_a_rate_limited_run() {
        let mut s = TrailSynth::new(48_000.0, 0x5133);
        let walk = s.walk;
        s.push(ev(GlowStyle::Lumen, SoundKind::Sweep { dir: 1 }));
        assert_eq!(
            s.walk, walk,
            "a cursor sweep must not step the phrase melody"
        );
        assert_eq!(
            s.live_voices(),
            CURSOR_SWEEP_RUN,
            "a sweep is a run of {CURSOR_SWEEP_RUN} tones"
        );
        // Onsets: delay is modelled as a negative onset time (v.t = -delay).
        let mut delays: Vec<f32> = s.voices.iter().filter(|v| v.on).map(|v| -v.t).collect();
        delays.sort_by(f32::total_cmp);
        assert_eq!(
            delays[0], 0.0,
            "the sweep's first note must be immediate (first-buffer audible)"
        );
        assert!(
            *delays.last().unwrap() > 0.1,
            "the sweep must spread into a run, not stack a chord: {delays:?}"
        );

        // Admission: a Sweep bypasses min-gap; a Glide right after a keystroke
        // is thinned.
        let mut a = TrailSynth::new(48_000.0, 1);
        a.push(ev(GlowStyle::Lumen, SoundKind::Typed)); // since_voice ← 0
        let before = a.live_voices();
        a.push(ev(GlowStyle::Lumen, SoundKind::Glide { dir: 1 })); // within min-gap
        assert_eq!(
            a.live_voices(),
            before,
            "a glide inside the min-gap must be thinned"
        );
        let mut b = TrailSynth::new(48_000.0, 1);
        b.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        let before = b.live_voices();
        b.push(ev(GlowStyle::Lumen, SoundKind::Sweep { dir: -1 })); // within min-gap
        assert!(
            b.live_voices() > before,
            "a sweep inside the min-gap must still speak (bypasses thinning)"
        );
    }

    // -- byte-identity proofs vs the v0.56 monolithic synth -----------------

    /// The v0.56 synth, frozen verbatim as the byte-identity oracle for the
    /// palette refactor: the pre-framework monolithic `design()`, bed
    /// matches, and render loop (no gesture namespacing, no duck envelope).
    /// It reuses the production `Voice`/`Partial`/`Bed` types and helpers —
    /// those are unchanged plain data (the new `duck_exempt` flag defaults
    /// `false` and this oracle never sets it) — so any drift the refactor
    /// introduced in push/design/bed/render arithmetic, rng ordering, or
    /// accumulation order shows up as a bit mismatch.
    ///
    /// "Frozen" admits DELIBERATE redesigns, mirrored in lock-step exactly
    /// like the phrase-generator state below: the deletion's felt lift-off
    /// layer (2026-08-26 owner ask) is duplicated verbatim in this oracle's
    /// `design`, so the pin accepts the intended sound and still catches
    /// accidental drift in everything around it.
    mod v056_reference {
        use super::super::*;

        pub struct RefSynth {
            inv_sr: f32,
            rng: u32,
            voices: [Voice; MAX_VOICES],
            bed: Bed,
            bed_style: GlowStyle,
            rate: f32,
            since_voice: f32,
            since_event: f32,
            /// The ERASE GATE's clock, mirrored from production.
            since_erase: f32,
            walk: i32,
            // The phrase-generator state, kept in lock-step with
            // `TrailSynth`'s: duplicated here on purpose, so a divergence in
            // the generator itself also trips the pin (the oracle's other
            // value is catching drift in voice/bed/render arithmetic and rng
            // ordering).
            motif: [i8; 4],
            phrase_pos: u8,
            phrase_len: u8,
            phrase_parity: bool,
            phrase_step: i32,
            phrase_home: i32,
            /// THE BAR, mirrored (see `SONG_PULSE`).
            song_pulse: u8,
            song_accent: bool,
            song_ghost: i8,
            /// The ghost's length multiplier, mirrored: production applies it
            /// inside `spawn`'s tone-feel multiply, which this v0.56 twin
            /// predates, so it rides `spawn_seeded` here instead.
            song_feel: f32,
            dc_x_l: f32,
            dc_y_l: f32,
            dc_x_r: f32,
            dc_y_r: f32,
        }

        impl RefSynth {
            pub fn new(sample_rate: f32, seed: u32) -> Self {
                Self {
                    inv_sr: 1.0 / sample_rate.max(8_000.0),
                    rng: seed | 1,
                    voices: [Voice::default(); MAX_VOICES],
                    bed: Bed::default(),
                    bed_style: GlowStyle::Lumen,
                    rate: 0.0,
                    since_voice: 1.0,
                    since_event: 1.0,
                    since_erase: 1.0,
                    walk: 2,
                    motif: [0; 4],
                    phrase_pos: 0,
                    phrase_len: 0,
                    phrase_parity: false,
                    phrase_step: 0,
                    phrase_home: 2,
                    song_pulse: 0,
                    song_accent: true,
                    song_ghost: 0,
                    song_feel: 1.0,
                    dc_x_l: 0.0,
                    dc_y_l: 0.0,
                    dc_x_r: 0.0,
                    dc_y_r: 0.0,
                }
            }

            fn rnd(&mut self) -> f32 {
                let mut x = self.rng;
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                self.rng = x;
                (x >> 8) as f32 * (1.0 / 16_777_216.0)
            }

            fn rnd_in(&mut self, lo: f32, hi: f32) -> f32 {
                lo + (hi - lo) * self.rnd()
            }

            fn is_quiet(&self) -> bool {
                self.voices.iter().all(|v| !v.on) && self.bed.level < 1e-3 && self.bed.energy < 1e-3
            }

            pub fn push(
                &mut self,
                style: GlowStyle,
                kind: SoundKind,
                pan: f32,
                heat: f32,
                hue: f32,
                gain: f32,
            ) {
                let (pan, heat, hue, gain) = (
                    pan.clamp(-1.0, 1.0),
                    heat.clamp(0.0, 1.0),
                    hue.clamp(0.0, 1.0),
                    gain.clamp(0.0, 1.0),
                );
                if gain <= 0.0 {
                    return;
                }
                let pause = self.since_event;
                self.rate = self.rate * (-self.since_event / 0.6).exp() + 1.0;
                self.since_event = 0.0;
                self.bed_style = style;
                let kick = match kind {
                    SoundKind::Jump | SoundKind::Kill | SoundKind::Land => 0.5,
                    // The word kill is never pushed by the pins; mirrored at
                    // production's value so the two cannot drift.
                    SoundKind::KillWord => 0.4,
                    // Space/Shift are never pushed by the pins; the arms keep
                    // the match total, mirroring production's grouping.
                    SoundKind::Typed | SoundKind::Backspace | SoundKind::Space => 0.3,
                    SoundKind::Navigation
                    | SoundKind::Glide { .. }
                    | SoundKind::Sweep { .. }
                    | SoundKind::Shift => 0.12,
                    // The cloud's puff is likewise never pushed by the pins;
                    // mirrored at production's value so the two cannot drift.
                    SoundKind::Poof => 0.0,
                };
                self.bed.energy = (self.bed.energy + kick).min(1.0);
                self.bed.gain += (gain - self.bed.gain) * 0.3;
                let duck = 1.0 / (1.0 + 0.55 * self.rate).sqrt();
                // THE ERASE GATE, mirrored: a deletion is thinned against
                // other deletions and claims no shared beat.
                let erase = kind == SoundKind::Backspace;
                let admit = if erase {
                    self.since_erase >= ERASE_MIN_GAP
                } else {
                    kind == SoundKind::Jump || self.since_voice >= MIN_GAP
                };
                if !admit {
                    return;
                }
                if erase {
                    self.since_erase = 0.0;
                } else {
                    self.since_voice = 0.0;
                }
                // The phrase-aware melody, Technical path — kept verbatim in
                // step with `TrailSynth::advance_melody` (this
                // oracle only ever renders the neutral tone, so the register is
                // (0,7), the motif span 1, and the lean 0). The pins push only
                // Typed/Backspace/Navigation/Kill/Jump, so the cursor gestures
                // never reach here.
                //
                // A DELETION NO LONGER STEPS THE SONG (mirrored): the erase is
                // unpitched and undoes a character, so it leaves the tune
                // exactly where the letters put it.
                if erase {
                    self.design(style, kind, pan, heat, hue, gain, duck);
                    self.song_feel = 1.0;
                    return;
                }
                // THE BAR, mirrored verbatim from `TrailSynth::advance_song`:
                // only a keystroke can ghost, an Enter or a long pause resets
                // to the downbeat, and on a ghost the phrase generator below
                // does not run at all.
                let bar_boundary = kind == SoundKind::Jump || pause > PHRASE_PAUSE_S;
                if bar_boundary {
                    self.song_pulse = 0;
                }
                let slot = if kind == SoundKind::Typed {
                    let s = SONG_PULSE[usize::from(self.song_pulse)];
                    self.song_pulse = (self.song_pulse + 1) % SONG_PULSE.len() as u8;
                    s
                } else {
                    SONG_ACCENT
                };
                if slot != SONG_ACCENT {
                    self.song_accent = false;
                    self.song_ghost = slot;
                    self.design(style, kind, pan, heat, hue, gain, duck);
                    // Production resets the feel the instant the dispatch
                    // returns, so the invariant "exactly 1.0 outside a palette
                    // dispatch" holds for the bed grains too.
                    self.song_feel = 1.0;
                    return;
                }
                self.song_accent = true;
                self.song_ghost = 0;
                let boundary = kind == SoundKind::Jump
                    || pause > PHRASE_PAUSE_S
                    || self.phrase_pos >= self.phrase_len;
                if boundary {
                    self.walk = if self.walk * 2 <= 5 { 0 } else { 5 };
                    self.phrase_home = self.walk;
                    self.phrase_step = 0;
                    self.phrase_pos = 0;
                    self.phrase_parity ^= true;
                    self.phrase_len =
                        PHRASE_MIN + (self.rnd() * ((PHRASE_MAX - PHRASE_MIN + 1) as f32)) as u8;
                    // Three NONZERO drawn steps (span 1 ⇒ ±1) and a closing
                    // one, verbatim with the production draw.
                    let mut sum = 0;
                    for i in 0..3 {
                        let k = (self.rnd() * 2.0) as i32;
                        let d = if k < 1 { k - 1 } else { k };
                        self.motif[i] = d as i8;
                        sum += d;
                    }
                    self.motif[3] = (-sum).clamp(-2, 2) as i8;
                } else {
                    let idx = (self.phrase_pos % 4) as usize;
                    let mut delta = i32::from(self.motif[idx]);
                    if self.phrase_parity {
                        delta = -delta;
                    }
                    if self.phrase_pos == self.phrase_len / 2 {
                        delta += MELODY_LEAP;
                    }
                    self.phrase_step += delta;
                    let frac = f32::from(self.phrase_pos) / f32::from(self.phrase_len);
                    let arc = ARC_LIFT * (core::f32::consts::PI * frac).sin().round() as i32;
                    let vary = if self.phrase_pos >= 4 { MOTIF_VARY } else { 0 };
                    self.phrase_pos += 1;
                    self.walk =
                        fold_register(self.phrase_home + self.phrase_step + arc + vary, 0, 7);
                }
                self.design(style, kind, pan, heat, hue, gain, duck);
                self.song_feel = 1.0;
            }

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
                best
            }

            fn spawn(&mut self, proto: Voice, gain: f32, pan: f32) {
                let tw_ph = self.rnd();
                let ph = [self.rnd(), self.rnd(), self.rnd()];
                self.spawn_seeded(proto, gain, pan, tw_ph, ph);
            }

            // The production `spawn_seeded` twin: the felt lift-off mirror
            // below must consume NO rng, exactly as production's does.
            fn spawn_seeded(
                &mut self,
                proto: Voice,
                gain: f32,
                pan: f32,
                tw_ph: f32,
                ph: [f32; 3],
            ) {
                let idx = self.claim();
                let p = (pan * 0.35).clamp(-0.6, 0.6);
                let a = (p + 1.0) * core::f32::consts::FRAC_PI_4;
                let mut v = proto;
                // The GHOST's length, mirrored from production's feel multiply
                // (`TrailSynth::spawn`): exactly 1.0 outside a ghost, so every
                // other spawn keeps this twin's original arithmetic.
                if self.song_feel != 1.0 {
                    v.dur *= self.song_feel;
                    v.decay *= self.song_feel;
                    v.delay *= self.song_feel;
                }
                v.on = true;
                v.t = -v.delay;
                v.gl = gain * a.cos();
                v.gr = gain * a.sin();
                v.tw_ph = tw_ph;
                for (part, ph) in v.p.iter_mut().zip(ph) {
                    part.ph = ph;
                }
                v.lp = 0.0;
                v.n_lp = 0.0;
                v.n_bp = 0.0;
                self.voices[idx] = v;
            }

            #[allow(clippy::too_many_arguments)]
            fn design(
                &mut self,
                style: GlowStyle,
                kind: SoundKind,
                pan: f32,
                heat: f32,
                hue: f32,
                gain: f32,
                duck: f32,
            ) {
                let g = gain * duck * (0.55 + 0.45 * heat);
                let kg = match kind {
                    // TIER 1 — per CHARACTER: the floor.
                    SoundKind::Typed => TYPED_KIND_GAIN,
                    SoundKind::Backspace => BACKSPACE_KIND_GAIN,
                    SoundKind::Glide { .. } => GLIDE_KIND_GAIN,
                    // Never reached (the pins emit neither the comma nor the
                    // lift); present so the match stays total.
                    SoundKind::Space => SPACE_KIND_GAIN,
                    SoundKind::Shift => SHIFT_KIND_GAIN,
                    // The cloud's puff, likewise never reached.
                    SoundKind::Poof => POOF_KIND_GAIN,
                    // TIER 2 — per GESTURE. Never reached (the pins emit no
                    // cursor gestures); present so the match stays total.
                    SoundKind::Navigation => NAVIGATION_KIND_GAIN,
                    SoundKind::Sweep { .. } => SWEEP_KIND_GAIN,
                    // TIER 2.5 — per WORD, likewise never reached.
                    SoundKind::KillWord => KILLWORD_KIND_GAIN,
                    // TIER 3 — per LINE / COMMAND.
                    SoundKind::Kill => KILL_KIND_GAIN,
                    SoundKind::Jump => JUMP_KIND_GAIN,
                    // TIER 4 — the rare spectacle.
                    SoundKind::Land => LAND_KIND_GAIN,
                };
                let g = g * kg;
                // ±1 column nudge and the gesture-family offset, as in
                // production (`song_key` has no analogue here — the oracle
                // never sings).
                let col_off = (pan).round() as i32;
                // THE GHOST OFFSET, mirrored: keystrokes only.
                let ghosting = kind == SoundKind::Typed && !self.song_accent;
                let deg = self.walk
                    + col_off
                    + gesture_shape(kind).offset
                    + if ghosting {
                        i32::from(self.song_ghost)
                    } else {
                        0
                    };

                // A kill is a style-tinted downward swoosh for every palette: soft
                // noise falling through the style's register. Designed once here.
                if kind == SoundKind::Kill {
                    let (hi, lo) = match style {
                        GlowStyle::Water => (900.0, 250.0),
                        GlowStyle::Fire => (1400.0, 300.0),
                        GlowStyle::Sparkle => (2600.0, 700.0),
                        GlowStyle::Comet => (1200.0, 280.0),
                        _ => (1600.0, 350.0),
                    };
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
                    self.spawn(v, g * 2.6, pan);
                    return;
                }

                // THE ERASE POOF, mirrored VERBATIM from
                // `TrailSynth::design_erase_poof` (the phrase-generator
                // discipline: a deliberate redesign — the 2026-08-26/28 owner
                // ask — is duplicated in lock-step so the pin keeps catching
                // accidental drift in everything else). It RETURNS, exactly as
                // production does: a deletion reaches no palette arm below.
                // Same constants, same spawn order, no rng draws.
                if kind == SoundKind::Backspace {
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
                    self.spawn_seeded(body, g * POOF_VOICE_GAIN, pan, 0.0, [0.0; 3]);
                    let air = Voice {
                        delay: POOF_AIR_DELAY_S,
                        dur: POOF_AIR_DUR_S,
                        attack: POOF_AIR_ATTACK_S,
                        decay: POOF_AIR_DECAY_S,
                        n_lvl: 0.5,
                        n_f0: POOF_AIR_HZ,
                        // THE SETTLE (2026-08-29 beautification), mirrored in
                        // lock-step exactly like the poof itself: the cap
                        // breathes down as it disperses.
                        n_f1: POOF_AIR_SETTLE_HZ,
                        n_glide: POOF_AIR_SETTLE_GLIDE_S,
                        n_q: POOF_AIR_Q,
                        lp_cut: POOF_LP_CUT_HZ,
                        ..Voice::default()
                    };
                    self.spawn_seeded(air, g * POOF_VOICE_GAIN * POOF_AIR_LEVEL, pan, 0.0, [0.0; 3]);
                    // (No held-run shimmer here: the pins replay isolated
                    // deletions and short scripts, never a held run — the
                    // run counter below its threshold spawns nothing, so the
                    // mirror stays voice-for-voice equal without one.)
                    return;
                }

                // Mirrors the production trim at the palette dispatch: the
                // same point in the chain — after the Kill early return, before
                // any palette voice is designed.
                let g = g * palette_trim(SoundVoice::Style, style);
                // …then the GHOST's level and length, in production's order.
                let g = if ghosting { g * SONG_GHOST_LEVEL } else { g };
                self.song_feel = if ghosting { SONG_GHOST_FEEL } else { 1.0 };

                match style {
                    // LUMEN/CUSTOM — the design intent lives on
                    // `LumenPalette::design`; this arm is its frozen twin.
                    GlowStyle::Lumen | GlowStyle::Custom => {
                        let f = penta(330.0, deg);
                        let (f0, f1v) = if kind == SoundKind::Backspace {
                            (f, f * 0.94)
                        } else {
                            (f * 0.97, f)
                        };
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
                                // The beating twin — a shade off, so the tone glows
                                // instead of pinging.
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
                        self.spawn(v, g * 0.42, pan);
                        if kind == SoundKind::Jump {
                            // Grace note a fifth up, blooming the same way — the
                            // little "arrived" flourish, warmer than before.
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
                            self.spawn(v2, g * 0.3, pan);
                        }
                    }

                    // PHASER — see `PhaserPalette::design`.
                    GlowStyle::Phaser => {
                        let hue_deg = (hue * 5.0) as i32;
                        let d = hue_deg + col_off + gesture_shape(kind).offset;
                        let f = penta(392.0, d);
                        // The rounded settle: start a whisker sharp and relax onto
                        // the note. Backspace inverts it.
                        let (f0, f1v) = if kind == SoundKind::Backspace {
                            (f, f * 1.12)
                        } else {
                            (f * 1.09, f)
                        };
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
                                // A quiet octave doubles the warmth without reaching
                                // into the register that grated.
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
                        self.spawn(v, g * 0.4, pan);
                        // The thock: a whisper of low filtered noise, gone in ~15 ms
                        // — felt more than heard, the key landing somewhere soft.
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
                        self.spawn(t, g * 0.25, pan);
                        if kind == SoundKind::Jump {
                            // "…deep!" — a fifth up, with a soft glassy glint riding
                            // it: the emitter's little ta-da.
                            let f2 = penta(392.0, d + 3);
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
                            self.spawn(v2, g * 0.34, pan * 0.5);
                        }
                    }

                    // RAINBOW KITTY — see `RainbowKittyPalette::design` (THE
                    // GLASS KEY, in verbatim lockstep: `penta` for the
                    // production `melody_hz` — the exact identity on this
                    // oracle's neutral-tone-only scripts).
                    GlowStyle::RainbowKitty => {
                        let base = 523.25; // C5 — the founding register
                        let f = penta(base, deg);
                        let (b0, b1) = gesture_bend(f, gesture_shape(kind).dir);
                        let f0 = b1 + (b0 - b1) * KEY_BELL_BEND_SHARE;
                        let mk = |f0: f32, f1: f32, delay: f32| Voice {
                            delay,
                            dur: 0.30,
                            attack: 0.0015,
                            decay: 0.085,
                            p: [
                                // THE GLASS BODY — grace-bent sine + FM glint.
                                Partial {
                                    lvl: 0.44,
                                    f0,
                                    f1,
                                    glide: KEY_BELL_BEND_TAU,
                                    fm_ratio: 3.01,
                                    fm_i0: 1.4,
                                    fm_tau: 0.030,
                                    ..Partial::default()
                                },
                                // THE TWIN TINK — the detuned half of the pair.
                                Partial {
                                    lvl: 0.18,
                                    f0: f1 * 4.010,
                                    f1: f1 * 4.010,
                                    ..Partial::default()
                                },
                                // THE TINK — pure double-octave glass.
                                Partial {
                                    lvl: 0.30,
                                    f0: f1 * 4.0,
                                    f1: f1 * 4.0,
                                    ..Partial::default()
                                },
                            ],
                            // THE MALLET — the 6 ms strike click.
                            n_lvl: 1.1,
                            n_f0: 5200.0,
                            n_f1: 180.0,
                            n_glide: 0.006,
                            n_q: 0.7,
                            lp_cut: 4200.0,
                            ..Voice::default()
                        };
                        self.spawn(mk(f0, f, 0.0), g * 0.30, pan);
                        if kind == SoundKind::Jump {
                            // 1-3-5-8 run, 45 ms apart — the bell cascade.
                            let mut leap = |step: i32, delay: f32, lvl: f32, pan: f32| {
                                let fl = penta(base, deg + step);
                                let (a0, a1) = gesture_bend(fl, 1);
                                self.spawn(
                                    mk(a1 + (a0 - a1) * KEY_BELL_BEND_SHARE, fl, delay),
                                    g * lvl,
                                    pan,
                                );
                            };
                            leap(2, 0.045, 0.3, pan * 0.5);
                            leap(3, 0.09, 0.27, pan * 0.2);
                            leap(5, 0.135, 0.24, -pan * 0.3);
                        }
                    }

                    // SPARKLE — see `SparklePalette::design`.
                    GlowStyle::Sparkle => {
                        let n = if kind == SoundKind::Jump { 3 } else { 2 };
                        for i in 0..n {
                            let d = deg + i * 2 + (self.rnd() * 2.0) as i32;
                            let f = penta(523.25, d); // C5 — the dong register
                            let delay = i as f32 * self.rnd_in(0.022, 0.05);
                            let v = Voice {
                                delay,
                                dur: 0.55,
                                attack: 0.003,
                                decay: self.rnd_in(0.14, 0.22),
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
                                tw_rate: self.rnd_in(7.0, 11.0),
                                tw_depth: 0.4,
                                lp_cut: 2800.0,
                                ..Voice::default()
                            };
                            let pan = pan + self.rnd_in(-0.4, 0.4);
                            self.spawn(v, g * 0.28 / (1.0 + i as f32 * 0.4), pan);
                        }
                    }

                    // FIRE — see `FirePalette::design`.
                    GlowStyle::Fire => {
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
                            self.spawn(v, g * 0.5, pan);
                            // Sparks thrown by the leap: a loose cluster of snaps.
                            for i in 0..4 {
                                let centre = self.rnd_in(2000.0, 5000.0);
                                let v = Voice {
                                    delay: 0.02 + i as f32 * 0.05 + self.rnd_in(0.0, 0.03),
                                    dur: 0.05,
                                    attack: 0.0003,
                                    decay: self.rnd_in(0.006, 0.012),
                                    n_lvl: 0.9,
                                    n_f0: centre,
                                    n_f1: centre * 0.85,
                                    n_glide: 0.008,
                                    n_q: self.rnd_in(3.5, 6.0),
                                    lp_cut: 5200.0,
                                    ..Voice::default()
                                };
                                let pan = pan + self.rnd_in(-0.5, 0.5);
                                let sg = g * self.rnd_in(0.16, 0.3);
                                self.spawn(v, sg, pan);
                            }
                        } else {
                            // The snap. Backspace cracks darker (a duller, lower
                            // fibre), typing cracks bright.
                            let centre = if kind == SoundKind::Backspace {
                                self.rnd_in(900.0, 1800.0)
                            } else {
                                self.rnd_in(2000.0, 4800.0)
                            };
                            let v = Voice {
                                dur: 0.05,
                                attack: 0.0003,
                                decay: self.rnd_in(0.006, 0.014),
                                n_lvl: 0.9,
                                n_f0: centre,
                                n_f1: centre * 0.85,
                                n_glide: 0.008,
                                n_q: self.rnd_in(3.5, 6.0),
                                lp_cut: 5200.0,
                                ..Voice::default()
                            };
                            self.spawn(v, g * 0.42, pan);
                            // Crackles cluster: sometimes a second, quieter micro-snap
                            // trails the first by a few tens of ms.
                            if self.rnd() < 0.35 {
                                let c2 = self.rnd_in(1600.0, 4200.0);
                                let v2 = Voice {
                                    delay: self.rnd_in(0.012, 0.04),
                                    dur: 0.04,
                                    attack: 0.0003,
                                    decay: self.rnd_in(0.005, 0.01),
                                    n_lvl: 0.9,
                                    n_f0: c2,
                                    n_f1: c2 * 0.85,
                                    n_glide: 0.008,
                                    n_q: self.rnd_in(3.5, 6.0),
                                    lp_cut: 5200.0,
                                    ..Voice::default()
                                };
                                let pan = pan + self.rnd_in(-0.3, 0.3);
                                self.spawn(v2, g * 0.22, pan);
                            }
                            // The ember: a short woody knock under the snap that only
                            // really speaks when typing is hot — the log settling.
                            let fe = self.rnd_in(85.0, 125.0);
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
                            self.spawn(v3, g * (0.1 + 0.28 * heat), pan * 0.5);
                        }
                    }

                    // LASER — see `LaserPalette::design`.
                    GlowStyle::Laser => {
                        let f_hi = penta(880.0, deg.min(5));
                        let f_lo = f_hi * 0.25;
                        let (a, b) = if kind == SoundKind::Backspace {
                            (f_lo, f_hi)
                        } else {
                            (f_hi, f_lo)
                        };
                        let dur = if kind == SoundKind::Jump { 0.2 } else { 0.11 };
                        let v = Voice {
                            dur,
                            attack: 0.001,
                            decay: if kind == SoundKind::Jump { 0.08 } else { 0.045 },
                            p: [
                                Partial {
                                    lvl: 0.7,
                                    f0: a,
                                    f1: b,
                                    glide: 0.024,
                                    // The sizzle: a burst of inharmonic FM that dies
                                    // in ~12 ms — electricity, not tone.
                                    fm_ratio: 7.03,
                                    fm_i0: 1.2,
                                    fm_tau: 0.012,
                                    ..Partial::default()
                                },
                                Partial {
                                    lvl: 0.15,
                                    f0: a * 2.0,
                                    f1: b * 2.0,
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
                        self.spawn(v, g * 0.4, pan);
                        if kind != SoundKind::Backspace {
                            // The crack: air snapping shut behind the bolt — one
                            // bright impulsive tick, gone in 8 ms.
                            let cf = self.rnd_in(3200.0, 5200.0);
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
                            self.spawn(crack, g * 0.2, pan);
                        }
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
                            self.spawn(v2, g * 0.3, pan * 0.4);
                            // The thunder: a long low roll sweeping down through the
                            // sky, arriving just behind the flash…
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
                            self.spawn(roll, g * 0.45, pan * 0.3);
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
                            self.spawn(echo, g * 0.22, -pan * 0.5);
                        }
                    }

                    // BEAM — see `BeamPalette::design`.
                    GlowStyle::Beam => {
                        let f = penta(330.0, (deg / 2) * 2); // even degrees: stabler line
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
                        self.spawn(thud, g * 0.22, pan);
                        // The console acknowledging: a muted blip settling onto its
                        // pitch (backspace rises instead — "un-pressed").
                        let (b0, b1) = if kind == SoundKind::Backspace {
                            (f, f * 1.08)
                        } else {
                            (f * 1.06, f)
                        };
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
                        self.spawn(blip, g * 0.3, pan);
                        if kind == SoundKind::Jump {
                            // ENGAGE: the two-tone confirm rising a fifth…
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
                            self.spawn(v2, g * 0.24, pan);
                            // …and the engines answering from below decks: a soft
                            // deep surge, all air and thrust, no tone.
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
                            self.spawn(surge, g * 0.26, pan * 0.3);
                        }
                    }

                    // WATER — see `WaterPalette::design`.
                    GlowStyle::Water => {
                        let f = penta(430.0, deg);
                        let rev = kind == SoundKind::Backspace;
                        let (a, b) = if rev {
                            (f * 1.15, f * 0.8)
                        } else {
                            (f * 0.85, f * 1.25)
                        };
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
                        self.spawn(tap, g * 0.16, pan);
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
                        self.spawn(v, g * 0.42, pan);
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
                            self.spawn(vs, g * 0.32, pan);
                            // …and the droplets it throws up, landing late and wide —
                            // each one a little rising bloop, same language as the
                            // key drops.
                            for i in 0..3 {
                                let fd = penta(430.0, deg + 2 + i);
                                let vd = Voice {
                                    delay: self.rnd_in(0.05, 0.16),
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
                                let pan = pan + self.rnd_in(-0.5, 0.5);
                                let gd = g * self.rnd_in(0.12, 0.22);
                                self.spawn(vd, gd, pan);
                            }
                        } else if !rev && self.rnd() < 0.18 {
                            // The occasional answering bubble — a tiny rising chirp
                            // a beat later. Scarcity is what keeps it delightful.
                            let vb = Voice {
                                delay: self.rnd_in(0.05, 0.09),
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
                            self.spawn(vb, g * 0.18, pan);
                        }
                    }

                    // COMET — see `CometPalette::design`.
                    GlowStyle::Comet => {
                        let f = penta(220.0, deg); // A3 region — the void register
                        let (f0, f1v) = if kind == SoundKind::Backspace {
                            (f * 0.97, f * 1.03)
                        } else {
                            (f * 1.04, f * 0.97)
                        };
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
                                // The beating twin: ~10 cents off, a slow dark
                                // shimmer instead of a ring.
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
                        self.spawn(v, g * 0.34, pan);
                        if kind == SoundKind::Typed && self.rnd() < 0.14 {
                            // The shooting star: a thin bright streak falling fast,
                            // twinkling as it burns — quiet, gone in a blink, and it
                            // crosses to the OTHER side of the stereo sky.
                            let hi = self.rnd_in(1400.0, 2200.0);
                            let delay = self.rnd_in(0.03, 0.1);
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
                            self.spawn(star, g * 0.16, -pan * 0.6);
                        }
                        if kind == SoundKind::Jump {
                            // The flyby: band noise sweeping down through the void
                            // with the tone falling under it — the nucleus passing
                            // close enough to feel.
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
                            self.spawn(swoosh, g * 0.4, pan);
                            // Debris glints: three SHORT crystal ticks scattered in
                            // the wake — icy, not churchy.
                            for i in 0..3 {
                                let fg = penta(1046.5, (self.rnd() * 5.0) as i32);
                                let delay = 0.08 + i as f32 * 0.07 + self.rnd_in(0.0, 0.03);
                                let decay = self.rnd_in(0.03, 0.05);
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
                                let pan = pan + self.rnd_in(-0.5, 0.5);
                                self.spawn(glint, g * 0.12, pan);
                            }
                        }
                    }
                }
            }

            pub fn render(&mut self, out: &mut [f32]) {
                let frames = out.len() / CHANNELS;
                if frames == 0 {
                    return;
                }
                let dt_block = frames as f32 * self.inv_sr;
                self.since_event += dt_block;
                self.since_voice += dt_block;
                self.since_erase += dt_block;
                self.rate *= (-dt_block / 0.6).exp();

                if self.is_quiet() {
                    out.fill(0.0);
                    self.bed.level = 0.0;
                    self.bed.energy = 0.0;
                    return;
                }

                self.tick_bed(dt_block);

                let dt = self.inv_sr;
                for f in 0..frames {
                    let (mut l, mut r) = (0.0f32, 0.0f32);
                    for vi in 0..MAX_VOICES {
                        let v = &mut self.voices[vi];
                        if !v.on {
                            continue;
                        }
                        v.t += dt;
                        if v.t < 0.0 {
                            continue;
                        }
                        if v.t >= v.dur {
                            v.on = false;
                            continue;
                        }
                        let mut s = 0.0f32;
                        for p in &mut v.p {
                            if p.lvl <= 0.0 {
                                continue;
                            }
                            let freq = if p.glide > 0.0 {
                                p.f1 + (p.f0 - p.f1) * (-v.t / p.glide).exp()
                            } else {
                                p.f0
                            };
                            let ph_inc = freq * dt;
                            p.ph = (p.ph + ph_inc).fract();
                            let x = match p.wave {
                                Wave::Sine => {
                                    if p.fm_ratio > 0.0 {
                                        p.fm_ph = (p.fm_ph + freq * p.fm_ratio * dt).fract();
                                        let idx = p.fm_i0 * (-v.t / p.fm_tau.max(1e-3)).exp();
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
                            s += p.lvl * x;
                        }
                        if v.n_lvl > 0.0 {
                            let white = {
                                let mut x = self.rng;
                                x ^= x << 13;
                                x ^= x >> 17;
                                x ^= x << 5;
                                self.rng = x;
                                (x >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
                            };
                            let fc = if v.n_glide > 0.0 {
                                v.n_f1 + (v.n_f0 - v.n_f1) * (-v.t / v.n_glide).exp()
                            } else {
                                v.n_f0
                            };
                            let g_svf = (core::f32::consts::PI * (fc * dt).min(0.45)).tan();
                            let damp = 1.0 / v.n_q.max(0.3);
                            let hp =
                                (white - v.n_lp - damp * v.n_bp) / (1.0 + g_svf * (g_svf + damp));
                            v.n_bp += g_svf * hp;
                            v.n_lp += g_svf * v.n_bp;
                            s += v.n_lvl * v.n_bp;
                        }
                        let mut env = (1.0 - (-v.t / v.attack.max(2e-4)).exp())
                            * (-v.t / v.decay.max(1e-3)).exp();
                        if v.tw_depth > 0.0 {
                            v.tw_ph = (v.tw_ph + v.tw_rate * dt).fract();
                            env *= 1.0 - v.tw_depth * 0.5 * (1.0 + sin01(v.tw_ph));
                        }
                        let rel = ((v.dur - v.t) * 200.0).clamp(0.0, 1.0);
                        env *= rel;
                        let k = (v.lp_cut * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
                        v.lp += k * (s - v.lp);
                        let y = v.lp * env;
                        l += y * v.gl;
                        r += y * v.gr;
                    }

                    let (bl, br) = self.bed_sample(dt);
                    l += bl;
                    r += br;

                    let xl = l * MASTER;
                    let yl = xl - self.dc_x_l + 0.995 * self.dc_y_l;
                    self.dc_x_l = xl;
                    self.dc_y_l = yl;
                    let xr = r * MASTER;
                    let yr = xr - self.dc_x_r + 0.995 * self.dc_y_r;
                    self.dc_x_r = xr;
                    self.dc_y_r = yr;
                    out[f * 2] = soft_clip(yl);
                    out[f * 2 + 1] = soft_clip(yr);
                }
            }

            fn tick_bed(&mut self, dt: f32) {
                let b = &mut self.bed;
                // Below ~-34 dB the bed is already imperceptible: hurry the tail so
                // the host's queue pause engages within ~a second of audibility
                // ending, instead of chasing the exponential for five more.
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
                // Floor snap: once the whole bed sits below −62 dB pre-gain it is
                // imperceptible — snap to exact zero so `is_quiet` (and the host's
                // queue pause) engages promptly instead of chasing an infinite tail.
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
                        let level = self.bed.level;
                        let gain = self.bed.gain;
                        match style {
                            GlowStyle::Sparkle => {
                                // Residual glitter: soft dongs drifting after the
                                // hands stop, in the same register as the key
                                // grains.
                                let f = penta(523.25, (self.rnd() * 8.0) as i32);
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
                                let pan = self.rnd_in(-0.8, 0.8);
                                self.spawn(v, gain * level * 0.08, pan);
                                self.bed.timer = self.rnd_in(0.15, 0.5) / level.max(0.05);
                            }
                            GlowStyle::Water => {
                                // Distant drips in the stream.
                                let f = penta(430.0, (self.rnd() * 6.0) as i32);
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
                                let pan = self.rnd_in(-0.7, 0.7);
                                self.spawn(v, gain * level * 0.09, pan);
                                self.bed.timer = self.rnd_in(0.25, 0.9) / level.max(0.05);
                            }
                            GlowStyle::Fire => {
                                // Crackles in the embers: mostly bright impulsive
                                // snaps, occasionally a low log-knock — never the soft
                                // mid-band plops that read as dripping water.
                                let low = self.rnd() < 0.12;
                                let (centre, q, decay, lvl) = if low {
                                    (self.rnd_in(240.0, 480.0), 2.0, 0.02, 0.2)
                                } else {
                                    (
                                        self.rnd_in(1400.0, 4600.0),
                                        self.rnd_in(3.0, 6.0),
                                        self.rnd_in(0.005, 0.012),
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
                                let pan = self.rnd_in(-0.6, 0.6);
                                self.spawn(v, gain * level * lvl, pan);
                                self.bed.timer = self.rnd_in(0.03, 0.18) / level.max(0.05);
                            }
                            GlowStyle::Laser => {
                                // The storm keeps grumbling: mostly distant thunder
                                // rolls, now and then the faint tick of a far-off
                                // strike too far away to hear crack properly.
                                if self.rnd() < 0.25 {
                                    let cf = self.rnd_in(3000.0, 5200.0);
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
                                    let pan = self.rnd_in(-0.8, 0.8);
                                    self.spawn(tick, gain * level * 0.05, pan);
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
                                    let pan = self.rnd_in(-0.7, 0.7);
                                    self.spawn(roll, gain * level * 0.18, pan);
                                }
                                self.bed.timer = self.rnd_in(0.5, 1.4) / level.max(0.05);
                            }
                            GlowStyle::Comet => {
                                // Distant signals: far off in the dark, a pure tone
                                // swells in, bends, and dies — something out there,
                                // rarely, answering.
                                let f = penta(660.0, (self.rnd() * 5.0) as i32);
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
                                let pan = self.rnd_in(-0.8, 0.8);
                                self.spawn(v, gain * level * 0.06, pan);
                                self.bed.timer = self.rnd_in(0.6, 1.6) / level.max(0.05);
                            }
                            GlowStyle::Phaser => {
                                // While charged, the emitter dreams: rare, very quiet
                                // round pips wandering the pentatonic — the next
                                // colour being considered.
                                let f = penta(392.0, (self.rnd() * 5.0) as i32);
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
                                let pan = self.rnd_in(-0.5, 0.5);
                                self.spawn(v, gain * level * 0.05, pan);
                                self.bed.timer = self.rnd_in(0.45, 1.2) / level.max(0.05);
                            }
                            _ => {
                                self.bed.timer = 0.25; // styles with purely tonal beds
                            }
                        }
                    }
                }
            }

            fn bed_sample(&mut self, dt: f32) -> (f32, f32) {
                let b = &mut self.bed;
                if b.level < 1e-4 {
                    return (0.0, 0.0);
                }
                let lvl = b.level * b.gain;
                // Shared slow LFOs (used as undulation / vibrato per style).
                b.lfo1 = (b.lfo1 + 0.23 * dt).fract();
                b.lfo2 = (b.lfo2 + 0.71 * dt).fract();
                let u1 = 0.5 * (1.0 + sin01(b.lfo1));
                let u2 = 0.5 * (1.0 + sin01(b.lfo2));

                let (m, side) = match self.bed_style {
                    // BEAM — see `BeamPalette::bed_sample`.
                    GlowStyle::Beam => {
                        let white = {
                            let mut x = self.rng;
                            x ^= x << 13;
                            x ^= x >> 17;
                            x ^= x << 5;
                            self.rng = x;
                            (x >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
                        };
                        let cut = 130.0 - 55.0 * b.droop;
                        let k = (cut.max(60.0) * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
                        b.lp1 += k * (white - b.lp1);
                        let k2 = (90.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
                        b.lp2 += k2 * (b.lp1 - b.lp2);
                        let breathe = 0.75 + 0.25 * u1;
                        (b.lp2 * lvl * 0.5 * breathe, b.lp1 * lvl * 0.03)
                    }
                    // WATER: the stream — lowpassed noise undulating on two
                    // incommensurate LFOs.
                    GlowStyle::Water => {
                        let white = {
                            let mut x = self.rng;
                            x ^= x << 13;
                            x ^= x >> 17;
                            x ^= x << 5;
                            self.rng = x;
                            (x >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
                        };
                        let k = (600.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
                        b.lp1 += k * (white - b.lp1);
                        let k2 = (180.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
                        b.lp2 += k2 * (b.lp1 - b.lp2);
                        let und = 0.55 + 0.3 * u1 + 0.15 * u2;
                        (b.lp2 * lvl * 0.5 * und, b.lp1 * lvl * 0.06)
                    }
                    // FIRE — see `FirePalette::bed_sample`.
                    GlowStyle::Fire => {
                        let white = {
                            let mut x = self.rng;
                            x ^= x << 13;
                            x ^= x >> 17;
                            x ^= x << 5;
                            self.rng = x;
                            (x >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
                        };
                        let k = (380.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
                        b.lp1 += k * (white - b.lp1);
                        // lp2 doubles as the flicker state: the same white draw
                        // through a very low one-pole (~9 Hz) wanders irregularly;
                        // scaled around unity it gutters the roar like flamelight.
                        let kf = (9.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
                        b.lp2 += kf * (white - b.lp2);
                        let flick = (1.0 + 26.0 * b.lp2).clamp(0.35, 1.8);
                        (b.lp1 * lvl * 0.4 * flick, 0.0)
                    }
                    // COMET — see `CometPalette::bed_sample`.
                    GlowStyle::Comet => {
                        b.ph1 = (b.ph1 + 110.0 * dt).fract();
                        b.ph2 = (b.ph2 + 110.5 * dt).fract();
                        b.ph3 = (b.ph3 + 165.0 * dt).fract();
                        let s = 0.5 * sin01(b.ph1)
                            + 0.5 * sin01(b.ph2)
                            + (0.08 + 0.12 * u1) * sin01(b.ph3);
                        (s * lvl * 0.055, s * lvl * 0.014)
                    }
                    // PHASER: charged-emitter purr — detuned triangle-ish pair under
                    // a gentle sweep. The sweep stays low and slow (400–900 Hz): a
                    // contented hum, not a filter show.
                    GlowStyle::Phaser => {
                        b.ph1 = (b.ph1 + 196.0 * dt).fract();
                        b.ph2 = (b.ph2 + 196.8 * dt).fract();
                        let s = tri(b.ph1) + tri(b.ph2);
                        let k =
                            ((400.0 + 500.0 * u1) * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
                        b.lp1 += k * (s - b.lp1);
                        (b.lp1 * lvl * 0.05, 0.0)
                    }
                    // RAINBOW KITTY: faint detuned pulse pad — the chip's idle hum.
                    GlowStyle::RainbowKitty => {
                        b.ph1 = (b.ph1 + 261.6 * dt).fract();
                        b.ph2 = (b.ph2 + 262.6 * dt).fract();
                        let s = (if b.ph1 < 0.5 { 1.0 } else { -1.0f32 })
                            + (if b.ph2 < 0.5 { 1.0 } else { -1.0f32 });
                        let k = (900.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
                        b.lp1 += k * (s - b.lp1);
                        (b.lp1 * lvl * 0.022, 0.0)
                    }
                    // SPARKLE — see `SparklePalette::bed_sample`.
                    GlowStyle::Sparkle => {
                        b.ph1 = (b.ph1 + 261.6 * dt).fract();
                        b.ph2 = (b.ph2 + 262.4 * dt).fract();
                        b.ph3 = (b.ph3 + 523.9 * dt).fract();
                        let s = sin01(b.ph1) + sin01(b.ph2) + (0.08 + 0.15 * u2) * sin01(b.ph3);
                        (s * lvl * 0.022, s * lvl * 0.006)
                    }
                    // LASER — see `LaserPalette::bed_sample`.
                    GlowStyle::Laser => {
                        let white = {
                            let mut x = self.rng;
                            x ^= x << 13;
                            x ^= x >> 17;
                            x ^= x << 5;
                            self.rng = x;
                            (x >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
                        };
                        let k = (140.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
                        b.lp1 += k * (white - b.lp1);
                        let ks = (1.3 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
                        b.lp2 += ks * (white - b.lp2);
                        let swell = (1.0 + 40.0 * b.lp2).clamp(0.25, 2.2);
                        (b.lp1 * lvl * 0.5 * swell, 0.0)
                    }
                    // LUMEN/CUSTOM — see `LumenPalette::bed_sample`.
                    GlowStyle::Lumen | GlowStyle::Custom => {
                        b.ph1 = (b.ph1 + 165.0 * dt).fract();
                        b.ph2 = (b.ph2 + 165.6 * dt).fract();
                        b.ph3 = (b.ph3 + 412.5 * dt).fract();
                        let s = sin01(b.ph1) + sin01(b.ph2) + (0.1 + 0.2 * u1) * sin01(b.ph3);
                        (s * lvl * 0.024, s * lvl * 0.005)
                    }
                };
                // `side` widens beds slightly; kept tiny to stay mono-compatible.
                (m + side, m - side)
            }
        }
    }

    /// PALETTE-REFACTOR FIDELITY PROOF: for every style, a script that exercises
    /// every trail kind at varied pan/heat/hue plus a 25 cps flood renders on the
    /// live per-palette synth within `V056_TOLERANCE` of the frozen v0.56
    /// monolith. This is the "nine existing palettes keep their sound by
    /// default" contract, held as an executable oracle against an independent
    /// reference implementation rather than a platform-pinned golden hash.
    ///
    /// It asserted BIT-EQUALITY until the transcendental rewrite (table-driven
    /// `sin01`, geometric envelope recursion) made that impossible. The oracle
    /// deliberately kept its per-sample, per-style granularity — a whole-buffer
    /// average would let one loud glitch hide inside a quiet buffer — and the
    /// bound is an audibility threshold, not a fitted number. What the oracle
    /// CANNOT see is `sin01` itself: `v056_reference` calls the same function,
    /// so the table would agree with itself by construction. That gap is closed
    /// by `sin_table_stays_below_one_16bit_step`.
    /// How far the live synth may deviate from the frozen v0.56 reference, per
    /// sample, per style.
    ///
    /// THREE-BILLIONTHS SHORT OF ONE 16-BIT STEP, and that is the point: a 16-bit
    /// quantization step is 1/32768 = 3.0518e-5, so a deviation under this bound
    /// cannot move an output sample by a full step. The constant is not fitted to
    /// whatever the code happens to produce — it is the audibility threshold, and
    /// the code is measured against it with room to spare.
    ///
    /// MEASURED (3.67 M samples, all nine styles, every gesture kind plus a 25 cps
    /// flood plus an 8 s ring-out, against `v056_reference::RefSynth`):
    ///   peak deviation 1.17e-5  = 0.38 of a 16-bit step  (-98.6 dBFS)
    ///   RMS  deviation 3.2e-7   = 0.011 of a step        (-129.8 dBFS)
    ///   deviation vs signal RMS                          (-91.6 dB)
    /// i.e. 2.6x headroom at the peak. The sources are the two transcendental
    /// rewrites: `sin01`'s interpolated table (own error 4.8e-6 = 0.16 of a step,
    /// pinned separately by `sin_table_stays_below_one_16bit_step`, which the
    /// oracle CANNOT see because the reference calls the same `sin01`) and the
    /// geometric envelope recursion (bounded by [`ENV_REANCHOR`]; seeding once per
    /// note instead reached 7.0e-4 = 23 steps, which is NOT inaudible — that is
    /// why the re-anchor exists).
    const V056_TOLERANCE: f32 = 3.0e-5;

    /// THE ORACLE'S BLIND SPOT, closed. `v056_reference::RefSynth` calls the
    /// crate's own [`sin01`], so swapping that function's body for an
    /// interpolated table changes BOTH sides of
    /// `palettes_render_within_one_16bit_step_of_v056_reference` identically and
    /// the deviation it reports for the table is exactly zero. The table
    /// therefore needs its own reference — real `sinf` — and this is it.
    ///
    /// MEASURED over 4 M phases: peak error 4.8e-6 = 0.16 of a 16-bit
    /// quantization step (-106.4 dBFS), RMS 2.4e-6. The bound below is one
    /// full step, so a failure here means the table has become able to move an
    /// output sample — the only way this approximation could become audible.
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

    #[test]
    fn palettes_render_within_one_16bit_step_of_v056_reference() {
        let kinds = [
            SoundKind::Typed,
            SoundKind::Backspace,
            SoundKind::Navigation,
            SoundKind::Kill,
            SoundKind::Jump,
        ];
        for style in STYLES {
            let mut new = TrailSynth::new(48_000.0, 0xA5A5_1234);
            let mut old = v056_reference::RefSynth::new(48_000.0, 0xA5A5_1234);
            let mut nb = [0.0f32; 960];
            let mut ob = [0.0f32; 960];
            let step = |new: &mut TrailSynth,
                        old: &mut v056_reference::RefSynth,
                        nb: &mut [f32; 960],
                        ob: &mut [f32; 960],
                        blocks: usize| {
                for _ in 0..blocks {
                    new.render(nb);
                    old.render(ob);
                    for (i, (a, b)) in nb.iter().zip(ob.iter()).enumerate() {
                        let d = (a - b).abs();
                        assert!(
                            d <= V056_TOLERANCE,
                            "{style:?}: sample {i} deviated {d:e} from v0.56, over the \
                             {V056_TOLERANCE:e} bound ({:.2} of a 16-bit step)",
                            d * 32768.0
                        );
                    }
                }
            };
            // Every kind, spaced out (all admitted), varied fields.
            for (i, kind) in kinds.iter().copied().enumerate() {
                let pan = -0.9 + i as f32 * 0.45;
                let heat = i as f32 * 0.22;
                let hue = i as f32 * 0.19;
                new.push(SoundEvent {
                    style,
                    voice: SoundVoice::Style,
                    kind: SoundGesture::Trail(kind),
                    pan,
                    heat,
                    hue,
                    gain: 0.4,
                    tone: Tone::Technical,
                    bed: true, // the v0.56 reference has no bed gate
                });
                old.push(style, kind, pan, heat, hue, 0.4);
                step(&mut new, &mut old, &mut nb, &mut ob, 5); // 100 ms
            }
            // A flood (min-gap thinning + bed swell + governor duck).
            for i in 0..50 {
                let pan = ((i % 20) as f32) / 10.0 - 1.0;
                new.push(SoundEvent {
                    style,
                    voice: SoundVoice::Style,
                    kind: SoundGesture::Trail(SoundKind::Typed),
                    pan,
                    heat: 0.9,
                    hue: 0.5,
                    gain: 0.4,
                    tone: Tone::Technical,
                    bed: true, // the v0.56 reference has no bed gate
                });
                old.push(style, SoundKind::Typed, pan, 0.9, 0.5, 0.4);
                step(&mut new, &mut old, &mut nb, &mut ob, 2); // 40 ms per key
            }
            // The full exhale back to silence.
            step(&mut new, &mut old, &mut nb, &mut ob, 150); // 3 s
        }
    }

    /// THE OWNER'S BELOVED "BRRRRING!": rapid line feeds arrive at the synth
    /// as rapid Jump gestures (the glow engine classifies an Enter's
    /// multi-cell move as [`SoundKind::Jump`] — pinned in cursor_glow's
    /// `rapid_line_feeds_cue_jump_and_typed_gestures`), Jumps bypass min-gap
    /// thinning,
    /// and their overlapping flourishes ARE the brrrring. This pins the whole
    /// cue path bit-exactly against the frozen v0.56 synth for the default
    /// (Lumen) and rainbow kitty palettes, and proves every jump in the burst actually
    /// speaks.
    #[test]
    fn brrrring_of_rapid_line_feeds_is_pinned() {
        for style in [GlowStyle::Lumen, GlowStyle::RainbowKitty] {
            let mut new = TrailSynth::new(48_000.0, 0x5EED_50FD);
            let mut old = v056_reference::RefSynth::new(48_000.0, 0x5EED_50FD);
            let mut nb = [0.0f32; 960];
            let mut ob = [0.0f32; 960];
            // Six Enters at ~60 ms — held-Enter cadence at a shell prompt.
            for _ in 0..6 {
                new.push(SoundEvent {
                    style,
                    voice: SoundVoice::Style,
                    kind: SoundGesture::Trail(SoundKind::Jump),
                    pan: -0.8,
                    heat: 0.3,
                    hue: 0.0,
                    gain: 0.4,
                    tone: Tone::Technical,
                    bed: true, // the v0.56 reference has no bed gate
                });
                old.push(style, SoundKind::Jump, -0.8, 0.3, 0.0, 0.4);
                // Every jump must actually speak (min-gap bypass): Lumen's
                // pluck + grace note, the rainbow kitty's blip + 3-note arpeggio run.
                let per_jump = if style == GlowStyle::RainbowKitty {
                    4
                } else {
                    2
                };
                assert!(
                    new.live_voices() >= per_jump,
                    "{style:?}: a rapid jump was thinned out of the brrrring"
                );
                for _ in 0..3 {
                    new.render(&mut nb);
                    old.render(&mut ob);
                    for (a, b) in nb.iter().zip(ob.iter()) {
                        let d = (a - b).abs();
                        assert!(
                            d <= V056_TOLERANCE,
                            "{style:?}: the brrrring drifted {d:e} from v0.56, over the \
                             {V056_TOLERANCE:e} bound"
                        );
                    }
                }
            }
            // And the ring-out.
            for _ in 0..100 {
                new.render(&mut nb);
                old.render(&mut ob);
                for (a, b) in nb.iter().zip(ob.iter()) {
                    assert!((a - b).abs() <= V056_TOLERANCE);
                }
            }
        }
    }
}
