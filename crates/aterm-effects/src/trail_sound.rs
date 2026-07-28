// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trail SOUND — the aural half of the cursor effects: a pure, clockless,
//! allocation-free procedural synthesizer that gives every [`GlowStyle`] its
//! own signature sound palette, and (since the cursor-audio framework) a
//! namespaced gesture vocabulary other effects can speak through the same
//! engine.
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
//!   nine shipped palettes are pinned byte-identical to their pre-framework
//!   (v0.56) rendering by the `v056_reference` proofs below.
//! - **Kind-level gestures.** Gestures whose sound is style-agnostic (the
//!   Kill swoosh, the curse-word Bonk) are designed ONCE before palette
//!   dispatch, tinted at most by a per-palette anchor
//!   ([`Palette::bonk_anchor_hz`]).
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
    /// Backspace / deletion — the Typed gesture mirrored (pitch falls or the
    /// contour reverses), so editing "sounds like undoing".
    Backspace,
    /// Cursor navigation (arrows, clicks) — the Typed gesture at a whisper:
    /// same timbre family, much quieter and shorter.
    Navigation,
    /// A kill (^K/^U/^W) that moved the cursor — a soft downward swoosh.
    Kill,
    /// A multi-cell jump (Enter to a prompt, tab-complete, screen redraws) —
    /// the style's flourish: an arpeggio, a splash, a whoosh, a power chord.
    /// Rapid line feeds are rapid Jumps, and because Jumps bypass the
    /// min-gap thinning their flourishes overlap into the owner's beloved
    /// "brrrring!" — pinned byte-exact by
    /// `brrrring_of_rapid_line_feeds_is_pinned`.
    Jump,
    /// A SMALL cursor move — a single-cell arrow. One soft, IN-KEY melody
    /// tone a scale-step in the travel direction, so scrubbing through text
    /// CONTINUES the tune rather than interrupting it. `dir` is +1 for a
    /// rightward / upward move, -1 for left / down (the direction rides IN the
    /// kind, so [`SoundEvent`] gains no new scalar and its non-finite filter is
    /// untouched). Gap-thinned exactly like a keystroke (out of the always-
    /// admit set), and softer than one (kind-gain in the Navigation whisper
    /// band).
    Glide { dir: i8 },
    /// A FAST cursor run — a held arrow's coalesced echo or a multi-cell leap
    /// (Ctrl-A/E, word motion). A short PRE-DELAYED run of in-key scale-tones
    /// sweeping from the current degree outward `dir` per step — the aural
    /// twin of the cursor sweeping across text, aligned to the same tone/key
    /// as the melody. One event carries the whole run (delayed voices, no
    /// scheduler — the arpeggio idiom), so it BYPASSES min-gap like [`Jump`]
    /// (thinning it would silence the run mid-flight); its own inter-note
    /// delays rate-limit it. The first note has `delay = 0`, so it speaks in
    /// the first post-cue synth buffer.
    Sweep { dir: i8 },
    /// The cursor LANDED — the aural twin of the Nyan fast-jump STARBURST
    /// (`cursor_glow`'s `Starburst`, cued at the same edge under the same
    /// `NYAN_BURST_MIN_DIST` gate, so stars and chime can never diverge). A
    /// bright IN-KEY star chime over a soft arrival body: the biggest thing
    /// the trail vocabulary says, because it accompanies the biggest thing the
    /// trail DRAWS.
    ///
    /// Before this existed a landing was announced by whatever the MOVE
    /// classified as — a Jump flourish at best, and for a hinted Ctrl-A/E leap
    /// (which reaches the very same starburst code) nothing louder than the
    /// [`SoundKind::Sweep`] whisper, 22 dB under the bonk.
    ///
    /// Style-agnostic and designed once before palette dispatch (like
    /// Kill/Bonk), pitched through `melody_hz` so it sits in the melody's key
    /// under every [`crate::tone::Tone`]. BYPASSES the min-gap governor like
    /// [`SoundKind::Jump`] (a starburst you can SEE must never be thinned into
    /// silence) and does NOT step the phrase melody — it punctuates the tune,
    /// it does not compose it.
    Land,
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

/// The FULL-NYAN celebration gesture vocabulary (the held-key sing-along —
/// `crate::nyan_sing`).
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
    /// `nyan_sing::SING_BAR_SECONDS`), with the documented ± ~60 ms host
    /// buffer/scheduling tolerance (`nyan_sing` module doc).
    /// The payload is `u16`, not `u8`, for the ESCALATION: a `u8` wraps every
    /// 256 bars (409.6 s of held key) and would restart the build ramp. `u16`
    /// wraps at 65 536 bars (~29 h) and, because `CELEBRATION_PHRASE_BARS` is a
    /// power of two, the FORM survives even that wrap in phase (pinned by
    /// `celebration_form_is_wrap_safe`).
    RiffBar { bar: u16 },
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
    /// FULL-NYAN sing-along gestures (`crate::nyan_sing`'s celebration).
    Celebration(CelebrationGesture),
}

/// WHICH palette family speaks for an event — the host's `trail_sound_style`
/// setting riding the per-event seam exactly like `gain`/`tone`/`bed` (the
/// established policy-carriage precedent), so a settings change takes effect
/// on the next keystroke with zero channel changes. [`SoundVoice::Style`] is
/// the exact identity: the palette follows [`SoundEvent::style`], bit for bit
/// (byte-pinned by the `v056_reference` proofs). [`SoundVoice::Mech`] routes
/// every palette-designed gesture to the mechanical-keyboard palette
/// ([`MechPalette`]) regardless of the visual trail style; the style-agnostic
/// kind-level gestures (Glide/Sweep melody tones, the bonk's clash shape)
/// keep their shared design. An enum, so the non-finite filter below has
/// nothing new to check.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SoundVoice {
    /// Follow the visual trail style's palette — today's sound, bit for bit.
    #[default]
    Style,
    /// The mechanical-keyboard palette: click + thock percussion.
    Mech,
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
    /// cached host-side). Rides the per-event seam exactly like `gain` — the
    /// established policy-carriage precedent — so a live settings/tone change
    /// takes effect on the next keystroke with zero channel changes. TRAIL
    /// gestures steer the melody's constitution with it (scale table,
    /// transpose, walk bias, note-length feel — see [`tone_tables`]);
    /// [`Tone::Technical`] is the exact identity (today's sound, bit for
    /// bit), and the Bonk ignores the field entirely (the wrong note is
    /// wrong in every mood). An enum, so the non-finite filter below has
    /// nothing new to check.
    pub tone: Tone,
    /// Whether this gesture may feed the ambient BED layer (the continuous
    /// per-style texture). Rides the per-event seam exactly like `gain` and
    /// `tone` — the established policy-carriage precedent — so the host's
    /// `trail_sound_bed` setting (default OFF: the owner dislikes the drone)
    /// takes effect on the next keystroke with zero channel changes. With
    /// every event carrying `false` the bed is never energised, so the bed
    /// mixer contributes EXACTLY ZERO samples structurally (its level-floor
    /// early-out, grains included — not a gain-0 render of the same DSP);
    /// the discrete notes, the brrrring, the bonk and the melody are
    /// untouched. A bool: the non-finite filter below has nothing new to
    /// check. The bed DSP itself stays intact behind this gate — a redesign
    /// tournament evaluates it next phase.
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
/// This is the consonant stand-in for the brief's "lydian-ish" — a true
/// lydian ♯4 IS the bonk's tritone (45/32), so shipping it in a melody table
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

/// Peak height of the raised-cosine CONTOUR ARC, in scale degrees: every
/// phrase rises to a mid-phrase peak and falls back, so it has a SHAPE instead
/// of a flat random drift. Added to the pitch register, never to the motif
/// accumulator, so it colours the contour without compounding.
const ARC_AMP: f32 = 2.0;

/// The bright LEAP at the arc peak, in scale degrees — the classic
/// "leap-and-recover" that makes a line sing; the motif steps back on the
/// following note. Because degrees stay on the active table, the leap is a
/// consonant sixth/octave, never a rub.
const MELODY_LEAP: i32 = 2;

/// Per-tone melodic REGISTER `(lo, hi)` the phrase walk is clamped into — the
/// tonal twin of the old per-tone walk clamps (Excited taller, Frustrated
/// shorter). `lo` is 0 for every table so the cadence tonics (degrees 0 and 5)
/// are always reachable.
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

/// Per-tone LEAN added to the pitch register: Excited climbs (+1), Frustrated
/// sinks (−1), the rest sit level (0). A quiet directional bias on top of the
/// arc, the modern form of the old walk's per-tone reversion pull.
fn melody_lean(tone: Tone) -> i32 {
    match tone {
        Tone::Excited => 1,
        Tone::Frustrated => -1,
        _ => 0,
    }
}

/// The register the STYLE-AGNOSTIC cursor-movement notes (Glide/Sweep) sing
/// in — a warm mid anchor, the same one the kind-level Kill/Bonk voices use.
/// The pitch is drawn through `melody_hz` at the melody's current degree, so
/// the cursor note sits on the active tone's table exactly like the tune.
const CURSOR_ANCHOR_HZ: f32 = 330.0;

/// A cursor SWEEP is this many pre-delayed scale-tones (a tasteful run, not a
/// machine-gun), spaced [`CURSOR_SWEEP_STEP_S`] apart with the first at
/// `delay = 0` (first-buffer audible).
const CURSOR_SWEEP_RUN: usize = 4;
const CURSOR_SWEEP_STEP_S: f32 = 0.055;

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
// are audibly SLIGHTLY louder, never a jump scare (owner, 2026-07-24: "bigger
// rare actions should be slightly louder. common typing should be the most soft
// (but not too soft)").
//
// The dBFS figures are RENDERED peaks at the default `trail_sound_volume` (0.4),
// heat 0.5, one isolated event, median over the ten palettes. The governor
// translates the WHOLE ladder rigidly (one scalar on `g` for every Trail kind
// and the bonk), so the intervals hold at any typing speed.
//
//   TIER 1  -21.0 dBFS  Typed, Backspace, Glide     per CHARACTER (10+/s)
//   TIER 2  -19.5 dBFS  Sweep, Navigation           per GESTURE
//   TIER 3  -18.0 dBFS  Jump, Kill                  per LINE / COMMAND
//   TIER 4  -16.0 dBFS  Land                        rare spectacle
//   TIER 5  -14.0 dBFS  Bonk, the riff's peak bar   rarest punctuation
//
// Kind gains are the TIER knob; `palette_trim` anchors each style's Typed on the
// floor. Where a gain looks surprising it is compensating a VOICE design, and
// the comment says so.

/// TIER 1 — the FLOOR. A keystroke is the unit everything else is measured in.
const TYPED_KIND_GAIN: f32 = 1.0;
/// TIER 1. Deletions arrive at typing speed, so they sit ON the floor, not under
/// it (was 0.8 — 2 dB of "softer", which only made an already-common gesture
/// harder to hear). The mirrored PITCH still carries "undo".
const BACKSPACE_KIND_GAIN: f32 = 1.0;
/// TIER 1. Above 1.0 because a glide's whole voice is one bare sine pluck,
/// intrinsically ~1 dB under a palette keystroke at equal gain. Was 0.32, i.e.
/// 11 dB below a keystroke: a cursor move you could SEE moved a live typing mix
/// by -0.2 dB — inaudible.
const GLIDE_KIND_GAIN: f32 = 1.11;
/// TIER 2 — one per gesture, not one per character. NOTE no production path
/// cues `Navigation` any more (`cursor_glow` splits every hinted move into
/// Glide/Sweep); this arm is kept for the audition harness and for hosts that
/// still speak it. Was 0.32.
const NAVIGATION_KIND_GAIN: f32 = 1.19;
/// TIER 2. The per-note taper in `design_cursor` still drops the run's tail;
/// this sets where its FIRST note lands. Was 0.30.
const SWEEP_KIND_GAIN: f32 = 1.14;
/// TIER 3 — a kill is per-command and destroys a line; it may not be the
/// quietest thing in the engine (it WAS, by 14 dB under a keystroke). Most of
/// that correction lives in the swoosh's own voice gain rather than here,
/// because the deficit was a VOICE deficit — see `design_trail`'s Kill arm.
const KILL_KIND_GAIN: f32 = 1.25;
/// TIER 3. UNCHANGED at 1.25: with the palette trim in place this already lands
/// Jump at -17.6 dBFS, and holding it here keeps [`BONK_KIND_GAIN`] strictly
/// above every trail kind-gain (const-asserted).
const JUMP_KIND_GAIN: f32 = 1.25;

/// Bonk kind-gain. Deliberately ABOVE every trail gesture (Jump's 1.25 was
/// the previous ceiling): punctuation with higher priority, per the owner's
/// brief — the wrong note must not hide in the texture it interrupts.
const BONK_KIND_GAIN: f32 = 1.35;

/// The cursor-LANDING star chime ([`SoundKind::Land`]). Register an octave
/// over the trail's warm mid — the stars glitter ABOVE the tune, they do not
/// sit in it.
const LAND_ANCHOR_HZ: f32 = 784.0;
/// TIER 4 — the landing kind-gain. DELIBERATELY BELOW the trail tiers' ceiling
/// (Jump/Kill 1.25): the landing's presence comes from its VOICE DESIGN — three
/// stacked star tones over an arrival body, four voices where a keystroke has
/// one — not from its gain. At 1.25 the delivered chime measured -12.8 dBFS,
/// i.e. LOUDER than the bonk it is meant to sit under; 0.865 lands it on tier 4
/// at -16.0 dBFS: +5 dB over a keystroke, 2 dB under the bonk.
///
/// (An earlier revision quoted "+10.9 dB over a keystroke" as if universal. That
/// was a Nyan-only figure — against Laser the same chime is only +5.9 dB,
/// because Land is style-agnostic while the keystroke is not.)
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

/// One celebration riff bar in seconds — 4 beats at the sing-along's
/// 150 BPM. Pinned equal to the VISUAL clock's `nyan_sing::SING_BAR_SECONDS`
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
/// turnaround. Bars 0 and 1 are the ORIGINAL two-bar cell VERBATIM, so the
/// celebration still OPENS on the phrase the owner already knows and then keeps
/// GOING instead of looping every 3.2 s. (2026-07-24, owner: "the sing song
/// needs a bit more, it sounds too repetitive to me". It was never more varied
/// than two bars — the loop has been 3.2 s since it was written.)
///
/// `REST` slots do NOT silence: the preceding note SUSTAINS through them, which
/// is where the phrase's long notes and its air come from.
const CELEBRATION_PHRASE: [[i32; 8]; CELEBRATION_PHRASE_BARS] = [
    // 0 — A, THE HOOK (the original `CELEBRATION_BAR_A`, verbatim).
    [0, 2, 4, 5, 4, 2, 4, 7],
    // 1 — A', THE ANSWER (the original `CELEBRATION_BAR_B`, verbatim).
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
/// its own motion, which is what the pre-change riff lacked: its "bass" was the
/// lead's own note an octave down, so there was no harmony to follow.
///
/// A bass note rides as the THIRD PARTIAL of the lead voice that OPENS its beat
/// — zero extra voices, exactly like the old downbeat sub — so every non-`REST`
/// bass beat REQUIRES a non-`REST` lead slot at `2 * beat`, pinned by
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
/// downbeat strongest, a BACKBEAT lift on slots 2 and 6, offbeats ghosted.
/// ~4.5 dB of internal dynamics where the pre-change riff had 1.9 dB across
/// exactly two levels.
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

/// Riff register root — C5, the Nyan palette's own chip register.
const CELEBRATION_BASE_HZ: f32 = 523.25;

/// TIER 5 — the sing-along riff's place on the loudness ladder. The riff is the
/// ONLY governor-exempt gesture (`design_celebration` omits the flood duck by
/// design), so without a kind gain of its own it escaped the ladder entirely:
/// bar 12 rendered at -9.5 dBFS, 2 dB OVER the bonk and 11 dB over a keystroke
/// — while additionally ducking that keystroke -9 dB ([`SING_DUCK_DEPTH`]).
/// That is a jump scare, not "slightly louder".
///
/// 0.6 lands the whole ESCALATION inside tier 5: the riff opens at -18.9 dBFS
/// (bar 0, lean), reaches -14.9 by the chorus and tops out at -13.7 with the
/// full build and clap. It still audibly grows — it grows INTO the ceiling
/// instead of through it.
const CELEBRATION_KIND_GAIN: f32 = 0.6;

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
    /// Amplitude twinkle (sparkle/comet glitter): depth 0..1 at `tw_rate` Hz,
    /// phase-jittered per voice so clusters shimmer instead of pulsing.
    tw_rate: f32,
    tw_depth: f32,
    tw_ph: f32,
    /// One-pole lowpass cutoff (Hz) applied to the voice's mixed output —
    /// the master "keep it soft" control every palette leans on.
    lp_cut: f32,
    lp: f32,
    /// Exempt from the master duck envelope. Only the bonk sets this: the
    /// duck exists to make room FOR it, so ducking the bonk itself would
    /// cancel the gesture. `false` (every trail voice, every bed grain)
    /// renders on the exact pre-framework signal path — with the duck
    /// envelope at rest its multiplier is exactly 1.0, proven bit-identical
    /// by the `v056_reference` tests.
    duck_exempt: bool,
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

/// One AMBIENT-BED TOURNAMENT candidate. The owner dislikes the shipping low
/// drone ("don't keep it if it doesn't sound good"; beds are off-by-default
/// behind `trail_sound_bed`), so the redesign is run as a judged tournament:
/// each candidate is a complete alternative continuous-bed design, rendered
/// and measured by `examples/bed_audition.rs` against the real melody.
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
    /// `bed_grain` units, i.e. the disliked drone under audit. The default,
    /// and the only variant hosts can ever reach.
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

/// C2 breath period in seconds, and the brief's swell law endpoints: the
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
/// degree 10 is 4× the anchor (≈2.1 kHz on the nyan chip register), 15 is
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
    /// Melodic random walk over pentatonic degrees, gently mean-reverting so
    /// phrases wander but never march off the top of the register. Advanced
    /// by TRAIL gestures only — a bonk clashes AGAINST the current degree, it
    /// does not move the melody. Step distribution, range and reversion
    /// target are shaped by [`Self::tone`] (the melody's "personality
    /// knobs").
    ///
    /// SINCE THE MELODIC RE-BASELINE `walk` is no longer stepped by a
    /// memoryless ±1 draw; it is the DERIVED current degree of the phrase
    /// generator (`phrase_home + phrase_step + arc`, clamped) — see
    /// [`Self::advance_melody`]. It stays the one scalar every consumer reads
    /// (the bonk clashes against it; `design_trail` offsets it by the column;
    /// a cursor Glide/Sweep plays relative to it), so those paths are
    /// untouched — only HOW it moves changed.
    walk: i32,
    /// PHRASE-AWARE MELODY STATE. The 4-step motif CELL — scale-step deltas
    /// re-chosen each phrase and replayed to fill it, so a recognisable shape
    /// RECURS and varies instead of drifting. This memory (a cell, a cursor, a
    /// contour) is the whole difference between a tune and the old aimless
    /// walk — the owner asked for "more of a melody".
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
    /// pinned path (v056 references, brrrring, bonk) renders bit-identical.
    sing: f32,
    /// Seconds of full sing-duck hold remaining (block-rate countdown).
    sing_hold: f32,
    /// DC blockers (one-pole highpass ~20 Hz) per channel.
    dc_x_l: f32,
    dc_y_l: f32,
    dc_x_r: f32,
    dc_y_r: f32,
}

/// Master output scale. Sized so a single default-volume Typed event peaks
/// around −20 dBFS — clearly audible in a quiet room at normal system volume,
/// far below alert/bell level.
///
/// MEASURED, not asserted — but PER STYLE, because the per-palette voice gains
/// swamp the kind-gain ladder: a single default-volume (0.4) Typed event peaks
/// at −23.7 dBFS on Nyan, −19.1 on Lumen, −17.6 on Mech and −25.7 on Beam —
/// an 8.1 dB spread across palettes for the SAME gesture. (An earlier revision
/// of this comment quoted "−24.0 Nyan / −23.1 Lumen" from a single-style
/// measurement; the Lumen figure was 4 dB stale. Quote the spread, not one
/// palette.)
/// It was 0.9 — which delivered −30.9 dBFS and −43.1 dBFS RMS, ~11 dB under
/// this doc's OWN spec: the owner's "the volume of effects are a bit too
/// quiet" (2026-07-24). Raised uniformly rather than by re-tuning the kind
/// gains or the rate governor because those are duplicated VERBATIM in the
/// `v056_reference` oracle, while this const is the one the oracle SHARES — so
/// both byte-identity pins multiply by the same changed value and still hold.
const MASTER: f32 = 2.0;

/// Governor: sustained admission gap for discrete voices, per event kind
/// pressure. ~45 ms ⇒ at most ~22 voices/s even under key repeat.
const MIN_GAP: f32 = 0.045;

/// PER-PALETTE LEVEL TRIM — the one knob that makes the loudness ladder a
/// property of the GESTURE instead of the LOOK. Each palette's voice design has
/// its own intrinsic level (a Mech thock is ~8 dB hotter than a Beam blip at the
/// same gain), so before this every style had its OWN ladder: a Laser Jump was
/// LOUDER than the bonk while a Beam Jump was quieter than a Lumen keystroke.
///
/// Each value is fitted so that style's `Typed` peaks at the ladder FLOOR
/// (-21.0 dBFS at gain 0.4 / heat 0.5, 24-seed mean) — measured by rendering,
/// not asserted. Residual spread across the ten palettes: 0.2 dB, from 8.1.
///
/// Applied at the palette dispatch ONLY, so the gestures designed BEFORE that
/// dispatch (Kill, Glide, Sweep, Land, Bonk, the riff) are untouched.
///
/// NOT because they are all perfectly flat: Kill's swoosh band is style-tinted
/// (Water 900/250, Fire 1400/300, Sparkle 2600/700, Comet 1200/280, else
/// 1600/350, plus its own Mech branch), which leaves it ~3 dB of residual
/// spread. Glide/Sweep/Land/Bonk/riff genuinely are flat. The reason the trim
/// stops here is structural — it is the knob that anchors a PALETTE, and these
/// gestures never reach a palette — not a claim that every one of them is
/// already uniform.
fn palette_trim(voice: SoundVoice, style: GlowStyle) -> f32 {
    match voice {
        SoundVoice::Mech => 0.68,
        SoundVoice::Style => match style {
            GlowStyle::Lumen | GlowStyle::Custom => 0.95,
            GlowStyle::Phaser => 0.94,
            GlowStyle::Nyan => 1.39,
            GlowStyle::Sparkle => 1.27,
            GlowStyle::Fire => 0.88,
            GlowStyle::Laser => 0.77,
            GlowStyle::Beam => 1.70,
            GlowStyle::Water => 1.01,
            GlowStyle::Comet => 1.64,
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
            tone: Tone::Technical,
            duck: 0.0,
            sing: 0.0,
            sing_hold: 0.0,
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
        self.voices.iter().all(|v| !v.on) && self.bed.level < 1e-3 && self.bed.energy < 1e-3
    }

    /// Number of live voices (test/diagnostic hook).
    pub fn live_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.on).count()
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
        // clamp finite out-of-range inputs into the documented domain. (No new
        // float field rode in with the gesture namespacing — the filter below
        // still covers every scalar `SoundEvent` carries.)
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
            // The ENERGY feed is what `ev.bed` gates (the `trail_sound_bed`
            // setting, default OFF): un-fed, the bed's level never leaves its
            // exact-zero floor, so the bed mixer emits zero samples and spawns
            // zero grains — structurally, not via a zero gain. Flipping the
            // setting OFF mid-breath simply starves the feed: the live bed
            // exhales through its normal ~1 s decay and snaps to exact zero.
            if ev.bed {
                let kick = match kind {
                    SoundKind::Jump | SoundKind::Kill | SoundKind::Land => 0.5,
                    SoundKind::Typed | SoundKind::Backspace => 0.3,
                    // Cursor scrubbing feeds the bed as gently as the old
                    // Navigation whisper — presence, not a swell.
                    SoundKind::Navigation | SoundKind::Glide { .. } | SoundKind::Sweep { .. } => {
                        0.12
                    }
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
        let admit = matches!(
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
                | SoundGesture::Words(WordGesture::Bonk)
                // A riff bar is ONE event per ~1.6 s carrying the whole
                // phrase — thinning it would silence entire bars, so it
                // outranks the gap exactly like the other punctuation.
                | SoundGesture::Celebration(_)
        ) || self.since_voice >= MIN_GAP;
        if !admit {
            return;
        }
        self.since_voice = 0.0;
        match ev.kind {
            SoundGesture::Trail(kind) => {
                // A KEYSTROKE / edit / jump advances the phrase-aware melody
                // (the tune). A cursor GESTURE (Glide/Sweep) plays IN the
                // melody without stepping it — the cursor sings the current
                // degree, it does not compose. Draw counts differ per phrase
                // (rng only at phrase boundaries), which is fine: determinism
                // is per (events, seed, tone) script.
                if !matches!(
                    kind,
                    SoundKind::Glide { .. } | SoundKind::Sweep { .. } | SoundKind::Land
                ) {
                    self.advance_melody(kind, pause);
                }
                self.design_trail(ev, kind, duck);
            }
            SoundGesture::Words(WordGesture::Bonk) => self.design_bonk(ev, duck),
            SoundGesture::Celebration(CelebrationGesture::RiffBar { bar }) => {
                self.design_celebration(ev, bar);
            }
        }
    }

    /// Advance the PHRASE-AWARE MELODY by one note, setting [`Self::walk`] to
    /// the new degree. Deterministic — `rnd()` is drawn only at phrase
    /// boundaries (a fixed 1 + 4 draws), never per note, so the per-note step
    /// is a pure function of phrase state and the whole generator replays
    /// bit-for-bit per `(events, seed, tone)`. `pause` is the typing gap since
    /// the previous event (captured before the governor reset it).
    ///
    /// This is the mechanism that answers "I thought we'd have more of a
    /// melody": every named tune-making device — a repeat-and-vary MOTIF cell,
    /// a raised-cosine contour ARC, CALL-AND-RESPONSE by phrase parity, a
    /// leap-and-recover at the peak, and a CADENCE onto the tonic at phrase
    /// ends (Enter or a pause) — layered onto the register, with pitch still
    /// flowing through [`Self::melody_hz`] so consonance and tone adaptation
    /// are inherited untouched.
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
            for i in 0..4 {
                self.motif[i] = (self.rnd() * (2 * span + 1) as f32) as i8 - span as i8;
            }
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
        if self.phrase_pos == 4 {
            delta += 1; // the cell returns a scale-degree higher: the "vary"
        }
        if self.phrase_pos == self.phrase_len / 2 {
            delta += MELODY_LEAP;
        }
        self.phrase_step += delta;
        // Raised-cosine CONTOUR ARC over the phrase (rise to a mid-phrase peak,
        // fall back) plus the per-tone LEAN — both fold into the PITCH register,
        // never the motif accumulator, so the cell's delta pattern stays exactly
        // periodic (the motif genuinely recurs) while the line still has shape.
        let frac = f32::from(self.phrase_pos) / f32::from(self.phrase_len);
        let arc = (ARC_AMP * (core::f32::consts::PI * frac).sin()).round() as i32;
        self.phrase_pos += 1;
        self.walk =
            (self.phrase_home + self.phrase_step + arc + melody_lean(self.tone)).clamp(lo, hi);
    }

    /// The melody's pitch lattice under the CURRENT tone: `base` scaled to
    /// `degree` of the tone's table, times the tone's transpose. This is
    /// what every palette calls where it used to call the free [`penta`] —
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
        best
    }

    /// Spawn one voice from a prototype: applies pan narrowing + equal-power
    /// law, resets runtime state, randomizes oscillator phases.
    #[allow(clippy::too_many_arguments)]
    fn spawn(&mut self, proto: Voice, gain: f32, pan: f32) {
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
        let feel = tone_feel(self.tone);
        if !v.duck_exempt && feel != 1.0 {
            v.dur *= feel;
            v.decay *= feel;
            v.delay *= feel;
        }
        v.on = true;
        v.t = -v.delay; // delay is modelled as negative onset time
        v.gl = gain * a.cos();
        v.gr = gain * a.sin();
        v.tw_ph = self.rnd();
        for part in &mut v.p {
            part.ph = self.rnd();
        }
        v.lp = 0.0;
        v.n_lp = 0.0;
        v.n_bp = 0.0;
        self.voices[idx] = v;
    }

    // -- kind-level + per-palette sound design ------------------------------

    /// Design and spawn the voice(s) for one admitted TRAIL gesture: the
    /// kind-level shaping shared by all styles, the style-agnostic Kill
    /// swoosh, then the per-palette dispatch. The numbers that were the
    /// monolithic `design()` live on the [`Palette`] implementors now — each
    /// palette IS its own sound designer.
    fn design_trail(&mut self, ev: SoundEvent, kind: SoundKind, duck: f32) {
        // Heat warms level slightly (+45 % at full blaze) — presence, not
        // a volume ride.
        let g = ev.gain * duck * (0.55 + 0.45 * ev.heat);
        // Kind-level shaping shared by all styles.
        let kg = match kind {
            // TIER 1 — per CHARACTER: the floor.
            SoundKind::Typed => TYPED_KIND_GAIN,
            SoundKind::Backspace => BACKSPACE_KIND_GAIN,
            SoundKind::Glide { .. } => GLIDE_KIND_GAIN,
            // TIER 2 — per GESTURE.
            SoundKind::Navigation => NAVIGATION_KIND_GAIN,
            SoundKind::Sweep { .. } => SWEEP_KIND_GAIN,
            // TIER 3 — per LINE / COMMAND.
            SoundKind::Kill => KILL_KIND_GAIN,
            SoundKind::Jump => JUMP_KIND_GAIN,
            // TIER 4 — the rare spectacle you can SEE.
            SoundKind::Land => LAND_KIND_GAIN,
        };
        let g = g * kg;
        // The column now only NUDGES the melody ±1 (was ±2): the phrase motif
        // owns the pitch, so a long line drifts by at most a single scale-step
        // instead of the column swamping the tune.
        let col_off = (ev.pan).round() as i32;
        let deg = self.walk + col_off;

        // CURSOR MOVEMENT (Glide/Sweep) is a style-agnostic, IN-KEY gesture
        // designed once here (like Kill/Bonk), before palette dispatch: it
        // plays relative to the melody's current degree on the active tone, so
        // scrubbing sits inside the tune. `g` already carries its soft
        // kind-gain, the heat warmth, and the flood duck.
        if let SoundKind::Glide { dir } | SoundKind::Sweep { dir } = kind {
            self.design_cursor(&ev, kind, dir, g);
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

        // A kill is a style-tinted downward swoosh for every palette: soft
        // noise falling through the style's register. Designed once here.
        if kind == SoundKind::Kill {
            let (hi, lo) = if ev.voice == SoundVoice::Mech {
                // The mech kill: a dull sweep down through the case register —
                // a hand brushing keys, not a musical fall.
                (1000.0, 220.0)
            } else {
                match ev.style {
                    GlowStyle::Water => (900.0, 250.0),
                    GlowStyle::Fire => (1400.0, 300.0),
                    GlowStyle::Sparkle => (2600.0, 700.0),
                    // Comet fell to deep space (A3 transmissions) — its kill
                    // swoosh falls dark with it, not through the old icy top.
                    GlowStyle::Comet => (1200.0, 280.0),
                    _ => (1600.0, 350.0),
                }
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
            // 2.6, not 0.5: this voice is PURE band-passed noise (no partials),
            // whose peak sits ~17 dB under a tonal voice at the same gain — so
            // at 0.5 the kill was the QUIETEST gesture in the engine, 14 dB
            // under a keystroke and completely masked by live typing (inserting
            // one moved a 6 cps mix by -0.2 dB). The correction belongs in the
            // VOICE, not in `KILL_KIND_GAIN`, which is a PRIORITY statement and
            // must stay under `BONK_KIND_GAIN`.
            self.spawn(v, g * 2.6, ev.pan);
            return;
        }

        // The palette's own level trim lands THIS style's keystroke on the
        // ladder floor, so the kind-gain tiers above mean the same number of dB
        // in every style. Before this, style — not gesture — dominated loudness:
        // a Laser Jump was louder than the bonk, a Beam Jump quieter than a
        // Lumen keystroke, and "the ladder" did not exist as heard.
        let g = g * palette_trim(ev.voice, ev.style);
        palette_for(ev.voice, ev.style).design(self, &ev, kind, g, deg, col_off);
    }

    /// The CURSOR-MOVEMENT gesture, aligned with the melody — the owner's
    /// "a sound effect for the movement of the cursor aligned with the
    /// melody". Style-agnostic and soft (a gentle sine pluck), pitched through
    /// [`Self::melody_hz`] on the ACTIVE tone at the melody's CURRENT degree,
    /// so it sits in the same key/scale as the tune the typing is playing:
    ///
    /// - [`SoundKind::Glide`] — one tone a scale-step in the travel direction
    ///   (`walk + dir`): moving through text CONTINUES the melody.
    /// - [`SoundKind::Sweep`] — a short PRE-DELAYED run of
    ///   [`CURSOR_SWEEP_RUN`] scale-tones stepping out from the current degree
    ///   `dir` per note (the arpeggio idiom — delayed voices, no scheduler),
    ///   gain tapering across the run. The FIRST note has `delay = 0`, so a
    ///   Sweep always speaks in the first post-cue synth buffer.
    ///
    /// `dir` rides in the kind (an enum payload, nothing for the non-finite
    /// filter to check), so [`SoundEvent`] carries no new scalar.
    fn design_cursor(&mut self, ev: &SoundEvent, kind: SoundKind, dir: i8, g: f32) {
        let mk = |f: f32| Voice {
            dur: 0.18,
            attack: 0.006,
            decay: 0.08,
            p: [
                Partial {
                    lvl: 0.5,
                    f0: f,
                    f1: f,
                    ..Partial::default()
                },
                // A whisper of sub-octave body so the tone reads as warm, not
                // as a beep.
                Partial {
                    lvl: 0.12,
                    f0: f * 0.5,
                    f1: f * 0.5,
                    ..Partial::default()
                },
                Partial::default(),
            ],
            lp_cut: 2200.0,
            ..Voice::default()
        };
        match kind {
            SoundKind::Sweep { .. } => {
                for i in 0..CURSOR_SWEEP_RUN {
                    let deg = self.walk + i32::from(dir) * i as i32;
                    let f = self.melody_hz(CURSOR_ANCHOR_HZ, deg);
                    let mut v = mk(f);
                    // First note immediate (first-buffer audible); the rest
                    // trail behind at a steady spacing — the run's own rate
                    // limit, so bypassing min-gap can't machine-gun.
                    v.delay = i as f32 * CURSOR_SWEEP_STEP_S;
                    // Taper across the run so it reads as a gesture that lands,
                    // not a flat block of tones.
                    let taper = 1.0 - 0.15 * i as f32;
                    self.spawn(v, g * 0.45 * taper, ev.pan);
                }
            }
            // Glide: a single in-key step in the travel direction.
            _ => {
                let f = self.melody_hz(CURSOR_ANCHOR_HZ, self.walk + i32::from(dir));
                self.spawn(mk(f), g * 0.5, ev.pan);
            }
        }
    }

    /// The cursor-LANDING star chime — the aural twin of the Nyan fast-jump
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

    /// The curse-word BONK — designed once at kind level exactly like Kill,
    /// free to use NON-pentatonic intervals precisely because everything else
    /// in the engine is constrained consonant. Two voices: the clash (minor
    /// second + tritone against the melody's CURRENT walk degree, in the
    /// active palette's own register via [`Palette::bonk_anchor_hz`]) over a
    /// round low thump — cartoon "bonk", not alarm. Both are duck-exempt and
    /// arm the master duck so the melody makes way. Feel gates: kind-gain
    /// [`BONK_KIND_GAIN`] > 1, no bed feed, no walk step.
    fn design_bonk(&mut self, ev: SoundEvent, duck: f32) {
        let g = ev.gain * duck * (0.55 + 0.45 * ev.heat) * BONK_KIND_GAIN;
        let root = penta(palette_for(ev.voice, ev.style).bonk_anchor_hz(), self.walk);
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
        // VOICE TRIM (2026-07-24, with the MASTER 0.9 -> 2.0 lift): the bonk's
        // authored ABSOLUTE level is ~-16 dBFS, and riding the master lift
        // untouched would have carried it to -9.4 dBFS — genuine alert
        // territory, against this module's "far below alert/bell level" ethos.
        // `BONK_KIND_GAIN` is deliberately NOT reduced: its job is PRIORITY
        // (the bonk outranks every trail kind-gain), which is unchanged.
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

    /// One BAR of the FULL-NYAN sing-along riff — one bar of the eight-bar
    /// [`CELEBRATION_PHRASE`] form (see that constant's "not the Nyan Cat
    /// melody" note). Up to eight SWUNG eighth-note pulse-wave voices scheduled
    /// as pre-delayed spawns (the engine's arpeggio idiom — samples-based, no
    /// scheduler), [`CELEBRATION_BASS`] folded into the lead voice that opens
    /// each beat, a sixteenth-note fill on the turnaround, and the sing-duck
    /// armed + held for the bar so the ordinary melody makes way and hands back
    /// on wind-down.
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
    fn design_celebration(&mut self, ev: SoundEvent, bar: u16) {
        let g = ev.gain * (0.55 + 0.45 * ev.heat) * CELEBRATION_KIND_GAIN;
        let idx = usize::from(bar) % CELEBRATION_PHRASE_BARS;
        let phrase = &CELEBRATION_PHRASE[idx];
        let bass_bar = &CELEBRATION_BASS[idx];
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
            let hz = self.melody_hz(CELEBRATION_BASE_HZ, deg);
            // The BASSLINE rides the lead voice that opens its beat — third
            // partial, no extra voice (the old downbeat sub, given a part of
            // its own). `build` fades the low end in over the opening bars.
            let sub = if i.is_multiple_of(2) && bass_bar[i / 2] != REST {
                let b = self.melody_hz(CELEBRATION_BASE_HZ, bass_bar[i / 2]);
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
                // Span 1 is EXACTLY the pre-change 0.30 s: in f32,
                // `CELEBRATION_EIGHTH * 1.5 == 0.30` bit for bit.
                dur: span as f32 * CELEBRATION_EIGHTH * 1.5,
                attack: 0.004,
                decay: 0.10 * span as f32,
                p: [
                    Partial {
                        lvl: 0.55,
                        f0: hz,
                        f1: hz,
                        wave: Wave::Pulse { duty: 0.25 },
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
                let hz = self.melody_hz(CELEBRATION_BASE_HZ, deg);
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
                            wave: Wave::Pulse { duty: 0.25 },
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
            return;
        }

        // Bed housekeeping at block rate: energy decay, level slew, grain
        // spawning (grains become ordinary voices).
        self.tick_bed(dt_block);

        let dt = self.inv_sr;
        // Per-sample duck recovery factor (τ = BONK_DUCK_TAU).
        let duck_step = (-dt / BONK_DUCK_TAU).exp();
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
                if v.t < 0.0 {
                    continue; // pre-delay
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
                    let fc = if v.n_glide > 0.0 {
                        v.n_f1 + (v.n_f0 - v.n_f1) * (-v.t / v.n_glide).exp()
                    } else {
                        v.n_f0
                    };
                    let g_svf = (core::f32::consts::PI * (fc * dt).min(0.45)).tan();
                    let damp = 1.0 / v.n_q.max(0.3);
                    let hp = (white - v.n_lp - damp * v.n_bp) / (1.0 + g_svf * (g_svf + damp));
                    v.n_bp += g_svf * hp;
                    v.n_lp += g_svf * v.n_bp;
                    s += v.n_lvl * v.n_bp;
                }
                // Envelope + twinkle + release guard.
                let mut env =
                    (1.0 - (-v.t / v.attack.max(2e-4)).exp()) * (-v.t / v.decay.max(1e-3)).exp();
                if v.tw_depth > 0.0 {
                    v.tw_ph = (v.tw_ph + v.tw_rate * dt).fract();
                    env *= 1.0 - v.tw_depth * 0.5 * (1.0 + sin01(v.tw_ph));
                }
                let rel = ((v.dur - v.t) * 200.0).clamp(0.0, 1.0); // 5 ms anti-click
                env *= rel;
                // Per-voice softening lowpass.
                let k = (v.lp_cut * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
                v.lp += k * (s - v.lp);
                let y = v.lp * env;
                if v.duck_exempt {
                    xl += y * v.gl;
                    xr += y * v.gr;
                } else {
                    l += y * v.gl;
                    r += y * v.gr;
                }
            }

            // Bed sample.
            let (bl, br) = self.bed_sample(dt);
            l += bl;
            r += br;

            // Master duck: the melody + bed dip around a live bonk AND
            // under a live sing-along riff; each factor is exactly ×1.0 (and
            // the exempt sum exactly +0.0) while its envelope rests, so the
            // default path is bit-identical to the pre-framework render
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
        let anchor = palette_for(self.bed_voice, self.bed_style).bonk_anchor_hz();
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
    /// brief's swell law (amplitude factor 0.05→0.2 on a ~12 s raised
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
    /// Design and spawn the voice(s) for one admitted trail gesture (Kill is
    /// designed kind-level before dispatch and never arrives here).
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

    /// The register the curse BONK clashes in — each palette's own melodic
    /// base, so the wrong note is wrong AGAINST the melody actually playing
    /// rather than in some unrelated octave. Default: the Lumen mid register
    /// (also right for unpitched palettes like Fire).
    fn bonk_anchor_hz(&self) -> f32 {
        330.0
    }
}

/// The palette registry — the ONE place a [`GlowStyle`] binds to its sound.
/// Trail Packs (`Custom`) are DATA-driven looks with no sound palette of
/// their own, so they ride Lumen's pluck (a pack-declared palette would
/// register here too). A non-[`SoundVoice::Style`] voice overrides the style
/// binding wholesale — the host's `trail_sound_style` decoupling — while
/// `Style` resolves exactly the pre-override table, so the byte pins hold.
fn palette_for(voice: SoundVoice, style: GlowStyle) -> &'static dyn Palette {
    match voice {
        SoundVoice::Mech => &MechPalette,
        SoundVoice::Style => match style {
            GlowStyle::Lumen | GlowStyle::Custom => &LumenPalette,
            GlowStyle::Phaser => &PhaserPalette,
            GlowStyle::Nyan => &NyanPalette,
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
/// sub-octave glow. The "default" sound: a good keyboard should sound like
/// this feels. (The airy Lumen/Laser shared pad retired with the live-review
/// redesign: Lumen breathes its own lamp pad, Laser rumbles a distant storm.)
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
        // LAMPLIGHT (live review: "blank and small"): each key BLOOMS — a
        // warm mid tone easing gently UP onto its note, a softly beating
        // twin for width, a sub-octave glow, and a breath of air. Backspace
        // dims (the bloom drifts back down).
        let f = s.melody_hz(
            330.0,
            deg + if kind == SoundKind::Backspace { -2 } else { 0 },
        );
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
            // "arrived" flourish, warmer than before.
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
        // THE PLAYFUL EMITTER (live review: "so blank… satisfaction and
        // some cuteness"; the 2.2× pew dive and 3× shimmer were the
        // shrillness): a rounded "boop" that SETTLES onto a note whose
        // degree follows the LIVE HUE, a tiny low THOCK for the tactile
        // landing, an up-turned "hm?" on backspace, a two-note "ba-deep!"
        // on Jump.
        let hue_deg = (ev.hue * 5.0) as i32;
        let d = hue_deg + col_off + if kind == SoundKind::Backspace { -2 } else { 0 };
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
                // the register that grated.
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

    fn bonk_anchor_hz(&self) -> f32 {
        392.0
    }
}

/// NYAN — the chiptune ribbon: quantized pulse-wave blips walking the
/// pentatonic, straight off a 8-bit sound chip but tiny and lowpassed.
/// Jump = a fast major-arpeggio run up, the classic power-up (and, under
/// rapid line feeds, the beloved "brrrring!" — see [`SoundKind::Jump`]).
struct NyanPalette;

impl Palette for NyanPalette {
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        kind: SoundKind,
        g: f32,
        deg: i32,
        _col_off: i32,
    ) {
        // MELLOWED (live review: "more mellow… make the doop doop sound a
        // bit longer"): rounded "doop"s — a 50 % square (hollow, odd-
        // harmonic) instead of the buzzy 25 % pulse, a sub-octave sine for
        // warmth, an eased attack, each note lingering a touch longer, the
        // register down a fourth from the old C5 ping.
        let base = 392.0; // G4 — the mellow chip register
        let d = deg + if kind == SoundKind::Backspace { -3 } else { 0 };
        let f = s.melody_hz(base, d);
        let mk = |f: f32, delay: f32| Voice {
            delay,
            dur: 0.13,
            attack: 0.0025,
            decay: 0.068,
            p: [
                Partial {
                    lvl: 0.45,
                    f0: f,
                    f1: f,
                    wave: Wave::Pulse { duty: 0.5 },
                    ..Partial::default()
                },
                // Sub-octave sine: the warmth under the doop.
                Partial {
                    lvl: 0.18,
                    f0: f * 0.5,
                    f1: f * 0.5,
                    ..Partial::default()
                },
                Partial::default(),
            ],
            lp_cut: 1900.0,
            ..Voice::default()
        };
        s.spawn(mk(f, 0.0), g * 0.34, ev.pan);
        if kind == SoundKind::Jump {
            // 1-3-5-8 run, 45 ms apart — the rainbow leaps.
            s.spawn(mk(s.melody_hz(base, d + 2), 0.045), g * 0.3, ev.pan * 0.5);
            s.spawn(mk(s.melody_hz(base, d + 3), 0.09), g * 0.27, ev.pan * 0.2);
            s.spawn(mk(s.melody_hz(base, d + 5), 0.135), g * 0.24, -ev.pan * 0.3);
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

    fn bonk_anchor_hz(&self) -> f32 {
        392.0
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
        // GROWN UP (live review: "not ding ding ding but dong dong dong"):
        // still a scatter of twinkling grains per key, but each grain is a
        // round DONG — C5, two octaves under the old G6 dings, a soft 3 ms
        // attack, a warm sub-octave body and a gentle glassy halo.
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
                    // A gentle halo where the old ding lived — quiet.
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
        // Residual glitter: soft dongs drifting after the hands stop — same
        // register as the key grains, so the afterglow never turns back
        // into dings.
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
        // No longer grains alone — a barely-there warm pad breathes under
        // them (C4 pair, slow-swelling soft octave), so the background is a
        // glow, not a void, and never a whine.
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 261.6 * dt).fract();
        b.ph2 = (b.ph2 + 262.4 * dt).fract();
        b.ph3 = (b.ph3 + 523.9 * dt).fract();
        let sm = sin01(b.ph1) + sin01(b.ph2) + (0.08 + 0.15 * u2) * sin01(b.ph3);
        (sm * lvl * 0.022, sm * lvl * 0.006)
    }

    fn bonk_anchor_hz(&self) -> f32 {
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
        // GENUINE CRACKLE (live review: "super bad… doesn't sound like
        // water and waves, make it like fire crackling"): every key is an
        // impulsive few-millisecond high-Q SNAP — a wood fibre parting —
        // over a woody ember knock. Impulsiveness, not softness, is what
        // separates fire from water. Jump = the flame LEAPING: a dark low
        // whoomph (no high splash — that would be a wave) plus a scatter
        // of spark snaps.
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
        // or surf: waves swell smoothly and slowly, flames gutter. (The old
        // slow-LFO undulation here is exactly why this bed read as waves.)
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
        // THE LIGHTNING STRIKE (live review: "I want the ZAP sound and like
        // a lightning/thunderstorm effect"): the two-octave dive kept, with
        // a brief electric SIZZLE on the tone and a bright high-Q CRACK of
        // air snapping over it. Backspace = the zap reversed, crackless and
        // softer. Jump = the FULL STRIKE: crack, zap, sub-thump landing,
        // then THUNDER — a long low roll that sweeps down and echoes once.
        // Still the archetype kept soft; thunder rumbles, it never booms.
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
        // a metronome LFO): weather, not machinery. Replaces the anonymous
        // airy pad.
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

    fn bonk_anchor_hz(&self) -> f32 {
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
        // THE ROCKET CONSOLE (live review: "the background noise I hate so
        // much that I can't take it… buttons being pressed on a rocket ship
        // traveling in deep space"): every key is a button on a spacecraft
        // panel — a soft rubberized THUD (the button seating) and a muted
        // low confirmation blip settling downward. No glassy tick, no dyad
        // — the old rod chime fed the hum. Backspace = the blip inverted
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
        // The ship, not the hum. The old sustained root+fifth+octave chord
        // was the single most complained-about sound in the whole engine
        // ("agonizing", then "I hate it so much I can't take it") — a held
        // tone is a whine no matter how soft. Replaced with the hull
        // ambience of a rocket coasting through deep space: very low
        // filtered air (the engines, decks away), no tonal content at all,
        // breathing slowly. The power-down droop survives as the engine
        // note darkening — the cutoff sinks as the energy dies, so the ship
        // audibly winds down with the tube's visual fade.
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

    fn bonk_anchor_hz(&self) -> f32 {
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
        // THE DROPLET, done the way water actually sounds (live review:
        // "the typing sound doesn't very match… the water trail"): a soft
        // surface TAP and then the BLOOP — the collapsing air bubble's
        // RISING chirp (the old voice fell in pitch, which is why it read
        // as a dry blip; real drops rise). A round attack and a low "gulp"
        // partial keep it liquid. Backspace = the bloop reversed (falling —
        // the drop climbing back out). The stream/ocean bed is untouched.
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

    fn bonk_anchor_hz(&self) -> f32 {
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
        // DEEP SPACE (live review: "I don't like the bell… space themed and
        // eerily mysterious with a couple of excitement"): every key is a
        // soft TRANSMISSION — a low hollow tone drifting downward a few
        // cents (the doppler of something immense passing far away), a
        // faintly beating twin so the pair shimmers darkly, a cold distant
        // twelfth, and the whisper of ice dust. Nothing clicks, nothing
        // chimes. The excitement: rare SHOOTING STARS off typed keys, and
        // Jump = the full FLYBY — a doppler swoosh with a scatter of short
        // crystal debris glints (a tenth of the old bell's decay — icy, not
        // churchy). Backspace drifts UP: it recedes.
        let d = deg + if kind == SoundKind::Backspace { -2 } else { 0 };
        let f = s.melody_hz(220.0, d); // A3 region — the void register
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
            n_lvl: 0.025, // ice dust, kept
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
        // slow LFO. Replaces the old ~1 kHz shimmer, which read as
        // tinnitus, with something felt in the chest.
        let b = &mut s.bed;
        b.ph1 = (b.ph1 + 110.0 * dt).fract();
        b.ph2 = (b.ph2 + 110.5 * dt).fract();
        b.ph3 = (b.ph3 + 165.0 * dt).fract();
        let sm = 0.5 * sin01(b.ph1) + 0.5 * sin01(b.ph2) + (0.08 + 0.12 * u1) * sin01(b.ph3);
        (sm * lvl * 0.055, sm * lvl * 0.014)
    }

    fn bonk_anchor_hz(&self) -> f32 {
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
/// (the doc on [`Palette::bonk_anchor_hz`] calls this out for unpitched
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
            // Kill / Glide / Sweep are designed kind-level before palette
            // dispatch and never arrive here (trait doc); Bonk and the riff
            // route through their own designers.
            SoundKind::Kill
            | SoundKind::Glide { .. }
            | SoundKind::Sweep { .. }
            | SoundKind::Land => {}
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

/// `sin(2π·ph)` for phase in turns.
#[inline]
fn sin01(ph: f32) -> f32 {
    (ph * core::f32::consts::TAU).sin()
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
            // The neutral identity tone — every pre-tone proof keeps its
            // exact pre-tone meaning under it.
            tone: Tone::Technical,
            // Bed ON: every pre-`trail_sound_bed` proof (byte pins included)
            // keeps its exact pre-gate meaning; the bed-off proofs construct
            // their own `bed: false` events.
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
            // Structurally irrelevant on a Words gesture (a bonk never feeds
            // the bed) — carried as ON so the pinned bonk scripts stay
            // byte-stable if that structure ever changes under review.
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

    const STYLES: [GlowStyle; 9] = [
        GlowStyle::Lumen,
        GlowStyle::Phaser,
        GlowStyle::Nyan,
        GlowStyle::Sparkle,
        GlowStyle::Fire,
        GlowStyle::Laser,
        GlowStyle::Beam,
        GlowStyle::Water,
        GlowStyle::Comet,
    ];

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
        for style in [GlowStyle::Lumen, GlowStyle::Nyan] {
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
                let mut e = mech(GlowStyle::Nyan, SoundKind::Typed);
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

    /// THE BACKLOG-CROWDING REFUTATION (adversarial review). The claim under
    /// test: the glow engine's 8-slot cue backlog can overflow under a stalled
    /// present, and a dropped keystroke cue is a lost click — so the backlog
    /// should be grown, or its drop policy made value-ranked.
    ///
    /// It is the governor, not the backlog, that decides how many clicks a
    /// stalled frame delivers. `since_voice` advances only with RENDERED
    /// samples, and a drain pushes the whole backlog inside one host tick with
    /// no render between — so every cue after the first shares one audio
    /// instant, sits inside [`MIN_GAP`], and is thinned to silence INSIDE the
    /// synth. A 24-deep backlog is audibly identical to a 1-deep one. Growing
    /// the cap therefore buys exactly zero clicks, and evicting a "less
    /// valuable" older cue to make room for a newer keystroke buys zero too:
    /// the newcomer still lands at the tail of the same batch and is thinned by
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

    /// THE MEASUREMENT behind the key-time click's timbre fix (adversarial
    /// review asked for evidence that a one-keystroke heat lag is not
    /// inaudible). `heat` rides the note level as `0.55 + 0.45·heat` and the
    /// ember/thump layer as `0.1 + 0.28·heat`, so one missing `HEAT_GAIN` step
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

    /// OWNER PROOF ("that ambient bed — I don't like it": beds are OFF by
    /// default): events carrying `bed: false` — what every host event carries
    /// under the `trail_sound_bed` default — never energise the bed layer, so
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
    /// minor-second and tritone rub zones. The zone predicate mirrors
    /// `tone_tables_are_mutually_consonant_and_exclude_the_bonk_clash` (that
    /// test also proves the zones non-vacuous via the bonk's own ratios).
    /// Plus the C3 voicing pin: SHIMMER truly has no low fundamental.
    #[test]
    fn bed_variant_pitches_stay_on_the_active_lattice_for_every_tone() {
        fn reduce(mut r: f32) -> f32 {
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
            // palette anchor — there is no low fundamental to dislike.
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
            s1.push(ev(GlowStyle::Nyan, SoundKind::Typed));
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
                let mut e = ev(GlowStyle::Nyan, SoundKind::Typed);
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
                        s.push(ev(GlowStyle::Nyan, SoundKind::Typed));
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
                    s.push(ev(GlowStyle::Nyan, SoundKind::Typed));
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
            s.push(ev(GlowStyle::Nyan, SoundKind::Typed));
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

    /// The bonk is the one deliberately discordant voice: its clash partials
    /// sit a just-intonation minor second and tritone above the melody's
    /// CURRENT walk degree — ratios that exist nowhere in the pentatonic
    /// lattice — and pushing it moves neither the walk nor the bed.
    #[test]
    fn bonk_is_discordant_against_the_walk_and_moves_nothing() {
        let mut s = TrailSynth::new(48_000.0, 33);
        let walk_before = s.walk;
        // Nyan anchor, current degree (G4 since the mellowing pass dropped
        // the chip register a fourth — the anchor tracks the melody).
        let root = penta(392.0, walk_before);
        s.push(bonk(GlowStyle::Nyan));
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

    // -- FULL-NYAN sing-along proofs ----------------------------------------

    /// One sing-along riff bar (the celebration gesture helper). `heat` 1.0:
    /// the host pins momentum to full while armed — that IS maximal flow.
    /// `bed` false: the host states the policy the gesture actually gets
    /// (Celebration, like Words, structurally never reaches the bed feed).
    fn riff(style: GlowStyle, bar: u16) -> SoundEvent {
        SoundEvent {
            style,
            voice: SoundVoice::Style,
            kind: SoundGesture::Celebration(CelebrationGesture::RiffBar { bar }),
            pan: 0.0,
            heat: 1.0,
            hue: 0.0,
            gain: 0.4,
            tone: Tone::Technical,
            bed: false,
        }
    }

    /// RIFF DETERMINISM: the samples are a pure function of (seed, events) —
    /// two synths fed the identical two-bar celebration script render
    /// byte-identical audio, and the riff genuinely sounds (non-zero).
    #[test]
    fn celebration_riff_is_deterministic_given_seed_and_events() {
        let run = || {
            let mut s = TrailSynth::new(48_000.0, 42);
            let mut out = Vec::new();
            let mut buf = [0.0f32; 960]; // 10 ms blocks
            for bar in 0..(CELEBRATION_PHRASE_BARS as u16 * 2) {
                s.push(riff(GlowStyle::Nyan, bar));
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
        s.push(ev(GlowStyle::Nyan, SoundKind::Typed));
        let before = s.live_voices();
        s.push(riff(GlowStyle::Nyan, 0)); // zero gap — would be thinned
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
        // Identical lone Nyan typed notes; one renders under a pinned sing
        // duck. Bed OFF so the energy compared is exactly the melody voice.
        let quiet_note = |sing: bool| {
            let mut s = TrailSynth::new(48_000.0, 9);
            s.push(SoundEvent {
                bed: false,
                ..ev(GlowStyle::Nyan, SoundKind::Typed)
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
        s.push(riff(GlowStyle::Nyan, 0));
        assert_eq!(s.sing, 1.0, "a riff bar must arm the sing duck");
        assert!(s.sing_hold > 0.0, "…and hold it for the bar");
        // …and with no further bars it hands back: after the bar + tails +
        // a few handback τ the envelope has snapped to exact zero while the
        // synth is still live (keep it breathing with quiet nav events).
        let mut buf = [0.0f32; 960];
        for block in 0..400 {
            // ~4 s
            if block % 20 == 0 {
                s.push(SoundEvent {
                    bed: false,
                    ..ev(GlowStyle::Nyan, SoundKind::Navigation)
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
            ..riff(GlowStyle::Nyan, 0)
        });
        assert_eq!(
            s.debug_bed(),
            (0.0, 0.0),
            "a riff bar must not swell the ambience it plays over"
        );
    }

    /// The audio bar and the visual dance clock are ONE tempo: the samples-
    /// based bar length is pinned equal to `nyan_sing`'s wall-clock bar (the
    /// documented ± ~60 ms host-buffer skew is tolerance, not tempo drift).
    #[test]
    fn celebration_bar_matches_the_visual_clock() {
        assert_eq!(CELEBRATION_BAR_SECONDS, crate::nyan_sing::SING_BAR_SECONDS);
        assert_eq!(
            CELEBRATION_EIGHTH,
            crate::nyan_sing::SING_BEAT_SECONDS / 2.0
        );
    }

    /// The riff's phrase tables stay on the consonant major-pentatonic
    /// lattice (degrees, not raw ratios) — so the sing-along can never rub
    /// against the typed melody under it, and the bonk keeps its exclusive
    /// claim to "wrong".
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
        // And the swung last eighth still starts inside its own bar.
        assert!((7.0 + CELEBRATION_SWING) * CELEBRATION_EIGHTH < CELEBRATION_BAR_SECONDS);
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
            s.push(riff(GlowStyle::Nyan, bar));
            for block in 0..160 {
                if block % 5 == 0 {
                    s.push(SoundEvent {
                        bed: false,
                        ..ev(GlowStyle::Nyan, SoundKind::Typed)
                    });
                }
                s.render(&mut buf);
                assert!(buf.iter().all(|x| x.is_finite()));
            }
        }
    }

    #[test]
    fn celebration_phrases_live_on_the_pentatonic_lattice() {
        for deg in CELEBRATION_PHRASE.iter().flatten().filter(|d| **d != REST) {
            assert!(
                (0..=9).contains(deg),
                "riff degree {deg} outside the two-octave singable range"
            );
        }
        // Two DIFFERENT authored bars — a loop, not a stuck record.
        for deg in &CELEBRATION_FILL {
            assert!((0..=9).contains(deg), "fill degree {deg} out of range");
        }
        for deg in CELEBRATION_BASS.iter().flatten().filter(|d| **d != REST) {
            assert!(
                (-12..=-1).contains(deg),
                "bass degree {deg} outside the bass register"
            );
        }
        // EIGHT DIFFERENT bars, pairwise — a song, not a stuck record. This
        // supersedes the old `assert_ne!(CELEBRATION_BAR_A, CELEBRATION_BAR_B)`.
        for a in 0..CELEBRATION_PHRASE_BARS {
            for b in (a + 1)..CELEBRATION_PHRASE_BARS {
                assert_ne!(
                    CELEBRATION_PHRASE[a], CELEBRATION_PHRASE[b],
                    "bars {a} and {b} are the same bar"
                );
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
            s.push(riff(GlowStyle::Nyan, 0));
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
        fn reduce(mut r: f32) -> f32 {
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
            let mut e = bonk(GlowStyle::Nyan);
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
            s.since_voice = 1.0;
            s.since_event = 0.0;
            let before = s.phrase_step;
            s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
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
    /// drifting off the top of the register forever (the owner's "aimless"
    /// complaint). The walk wanders during the phrase, then the Enter snaps it
    /// home.
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

    /// A GLIDE plays exactly ONE in-key tone, a scale-step in the travel
    /// direction of the melody's current degree, and is SOFTER than the
    /// keystroke it accompanies. It does NOT step the phrase (the cursor sings
    /// the tune, it doesn't compose it).
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
        let f = s
            .voices
            .iter()
            .find(|v| v.on)
            .map(|v| v.p[0].f0)
            .expect("a glide voice");
        // In-key: one scale-step up from the current degree, on the active
        // (Technical ⇒ PENTA) table.
        let expect = s.melody_hz(CURSOR_ANCHOR_HZ, walk + 1);
        assert!(
            (f - expect).abs() < 1.0,
            "glide pitch {f} must be the in-key step {expect}"
        );

        // LADDER TIER 1 (2026-07-24). This assertion used to read "a glide must
        // be SOFTER than a keystroke". That contract was retired deliberately,
        // on the owner's direction: "common typing should be the most soft (but
        // not too soft) and whatever other effects need to be heard." Measured,
        // the old rule had put a glide 11 dB under a keystroke — a cursor move
        // you could SEE moved a live 6 cps typing mix by 0.2 dB, i.e. it was
        // inaudible. Typing is the FLOOR now, and a glide sits on that floor
        // WITH it (both are per-character gestures), not beneath it.
        //
        // What is still pinned is that it never becomes a jump scare: within
        // one tier of a keystroke, in BOTH directions. The band is wide on the
        // upper side because a glide is style-agnostic (designed before palette
        // dispatch) while a keystroke carries `palette_trim`, so their exact
        // ratio legitimately varies by style — here Lumen trims to 0.95.
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
            walk: i32,
            // Re-frozen with the melodic re-baseline: the phrase-generator
            // state, kept in lock-step with `TrailSynth`'s so the byte pins
            // cross-check the whole render path around a SHARED generator (the
            // oracle's value is catching drift in voice/bed/render arithmetic
            // and rng ordering; the generator itself is duplicated here on
            // purpose so any divergence in it also trips the pin).
            motif: [i8; 4],
            phrase_pos: u8,
            phrase_len: u8,
            phrase_parity: bool,
            phrase_step: i32,
            phrase_home: i32,
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
                    walk: 2,
                    motif: [0; 4],
                    phrase_pos: 0,
                    phrase_len: 0,
                    phrase_parity: false,
                    phrase_step: 0,
                    phrase_home: 2,
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
                    SoundKind::Typed | SoundKind::Backspace => 0.3,
                    SoundKind::Navigation | SoundKind::Glide { .. } | SoundKind::Sweep { .. } => {
                        0.12
                    }
                };
                self.bed.energy = (self.bed.energy + kick).min(1.0);
                self.bed.gain += (gain - self.bed.gain) * 0.3;
                let duck = 1.0 / (1.0 + 0.55 * self.rate).sqrt();
                let admit = kind == SoundKind::Jump || self.since_voice >= MIN_GAP;
                if !admit {
                    return;
                }
                self.since_voice = 0.0;
                // The RE-BASELINED phrase-aware melody, Technical path — kept
                // verbatim in step with `TrailSynth::advance_melody` (this
                // oracle only ever renders the neutral tone, so the register is
                // (0,7), the motif span 1, and the lean 0). The pins push only
                // Typed/Backspace/Navigation/Kill/Jump, so the cursor gestures
                // never reach here.
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
                    for i in 0..4 {
                        self.motif[i] = (self.rnd() * 3.0) as i8 - 1;
                    }
                } else {
                    let idx = (self.phrase_pos % 4) as usize;
                    let mut delta = i32::from(self.motif[idx]);
                    if self.phrase_parity {
                        delta = -delta;
                    }
                    if self.phrase_pos == 4 {
                        delta += 1;
                    }
                    if self.phrase_pos == self.phrase_len / 2 {
                        delta += MELODY_LEAP;
                    }
                    self.phrase_step += delta;
                    let frac = f32::from(self.phrase_pos) / f32::from(self.phrase_len);
                    let arc = (ARC_AMP * (core::f32::consts::PI * frac).sin()).round() as i32;
                    self.phrase_pos += 1;
                    self.walk = (self.phrase_home + self.phrase_step + arc).clamp(0, 7);
                }
                self.design(style, kind, pan, heat, hue, gain, duck);
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
                let idx = self.claim();
                let p = (pan * 0.35).clamp(-0.6, 0.6);
                let a = (p + 1.0) * core::f32::consts::FRAC_PI_4;
                let mut v = proto;
                v.on = true;
                v.t = -v.delay;
                v.gl = gain * a.cos();
                v.gr = gain * a.sin();
                v.tw_ph = self.rnd();
                for part in &mut v.p {
                    part.ph = self.rnd();
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
                    // TIER 2 — per GESTURE. Never reached (the pins emit no
                    // cursor gestures); present so the match stays total.
                    SoundKind::Navigation => NAVIGATION_KIND_GAIN,
                    SoundKind::Sweep { .. } => SWEEP_KIND_GAIN,
                    // TIER 3 — per LINE / COMMAND.
                    SoundKind::Kill => KILL_KIND_GAIN,
                    SoundKind::Jump => JUMP_KIND_GAIN,
                    // TIER 4 — the rare spectacle.
                    SoundKind::Land => LAND_KIND_GAIN,
                };
                let g = g * kg;
                // Re-baselined with the melody: ±1 column nudge (was ±2).
                let col_off = (pan).round() as i32;
                let deg = self.walk + col_off;

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

                // Mirrors the production trim at the palette dispatch: the
                // same point in the chain — after the Kill early return, before
                // any palette voice is designed.
                let g = g * palette_trim(SoundVoice::Style, style);

                match style {
                    // LUMEN — lamplight: the default voice, but no longer a bare
                    // pluck. Each key BLOOMS — a warm mid tone easing gently UP onto
                    // its note (light coming on, the inverse of comet's fall), a
                    // softly beating twin for width, a sub-octave glow underneath,
                    // and a breath of air where comet keeps ice dust. Fuller and
                    // rounder than the old two-partial tick, still unobtrusive: a
                    // good keyboard should sound like this feels. Backspace dims
                    // (the bloom drifts back down). Trail Packs (Custom) are
                    // DATA-driven looks with no sound palette of their own, so they
                    // ride the default bloom.
                    GlowStyle::Lumen | GlowStyle::Custom => {
                        let f = penta(
                            330.0,
                            deg + if kind == SoundKind::Backspace { -2 } else { 0 },
                        );
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

                    // PHASER — the playful emitter: a rounded "boop" that SETTLES
                    // onto a pentatonic note whose degree follows the LIVE HUE, so
                    // the band's colour and pitch still travel together — but the
                    // note now leads with its warm fundamental (no 2.2× dive-bomb,
                    // no 3× shimmer image; those were the shrillness). Under it, a
                    // tiny low THOCK gives each key a tactile, satisfying landing.
                    // Backspace turns the boop upward — a small questioning "hm?"
                    // as the phaser un-fires. Jump = a miniature two-note "ba-deep!"
                    GlowStyle::Phaser => {
                        let hue_deg = (hue * 5.0) as i32;
                        let d =
                            hue_deg + col_off + if kind == SoundKind::Backspace { -2 } else { 0 };
                        let f = penta(392.0, d);
                        // The rounded settle: start a whisker sharp and relax onto
                        // the note — a "byoop", not a laser. Backspace inverts it.
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

                    // NYAN — the chiptune ribbon, mellowed: rounded "doop"s walking
                    // the pentatonic — still a sound chip, but one with a felt mute.
                    // A 50 % square (hollow, odd-harmonic) replaces the buzzy 25 %
                    // pulse, a sub-octave sine warms the body, the attack is eased so
                    // the blip doesn't click, and each note lingers a touch longer
                    // than the old machine-gun "doo". Register sits at G4, a fourth
                    // below the old C5 ping. Jump = the same major-arpeggio power-up.
                    GlowStyle::Nyan => {
                        let base = 392.0; // G4 — the mellow chip register
                        let d = deg + if kind == SoundKind::Backspace { -3 } else { 0 };
                        let f = penta(base, d);
                        let mk = |f: f32, delay: f32| Voice {
                            delay,
                            dur: 0.13,
                            attack: 0.0025,
                            decay: 0.068,
                            p: [
                                Partial {
                                    lvl: 0.45,
                                    f0: f,
                                    f1: f,
                                    wave: Wave::Pulse { duty: 0.5 },
                                    ..Partial::default()
                                },
                                // Sub-octave sine: the warmth under the doop.
                                Partial {
                                    lvl: 0.18,
                                    f0: f * 0.5,
                                    f1: f * 0.5,
                                    ..Partial::default()
                                },
                                Partial::default(),
                            ],
                            lp_cut: 1900.0,
                            ..Voice::default()
                        };
                        self.spawn(mk(f, 0.0), g * 0.34, pan);
                        if kind == SoundKind::Jump {
                            // 1-3-5-8 run, 45 ms apart — the rainbow leaps.
                            self.spawn(mk(penta(base, d + 2), 0.045), g * 0.3, pan * 0.5);
                            self.spawn(mk(penta(base, d + 3), 0.09), g * 0.27, pan * 0.2);
                            self.spawn(mk(penta(base, d + 5), 0.135), g * 0.24, -pan * 0.3);
                        }
                    }

                    // SPARKLE — glitter, grown up: still a scatter of two or three
                    // shimmering grains per key, but each grain is now a round DONG
                    // — C5 register (two octaves under the old G6 dings), a soft
                    // 3 ms attack instead of a click, a warm sub-octave body, a
                    // gentle glassy halo, and the twinkle kept. The scatter and the
                    // stereo toss stay; only the voice traded ding for dong.
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
                                    // A gentle halo where the old ding lived — quiet.
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

                    // FIRE — the hearth: every key is a genuine crackle SNAP — an
                    // impulsive few-millisecond high-Q noise ring, the sound of a wood
                    // fibre parting — over a woody ember knock that grows with heat.
                    // Impulsiveness (not softness) is what separates fire from water:
                    // a long soft noise burst reads as a plop, a 10 ms snap reads as
                    // burning. Jump = the flame LEAPING: a dark low whoomph (no high
                    // splash — that would be a wave) plus a scatter of spark snaps.
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

                    // LASER — the LIGHTNING STRIKE: every key is a real ZAP — the
                    // fast two-octave dive kept, but with a brief electric SIZZLE
                    // (fast-decaying FM buzz) on the tone and a bright high-Q CRACK
                    // of air snapping over it. Backspace = the zap reversed (rising),
                    // crackless and softer. Jump = the FULL STRIKE: crack, zap, a
                    // round sub-thump as the bolt lands, then the THUNDER — a long
                    // low roll that sweeps down and echoes once. Still the archetype
                    // kept soft; thunder rumbles, it never booms.
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

                    // BEAM — the ROCKET CONSOLE: every key is a button pressed on a
                    // spacecraft panel somewhere in deep space — a soft rubberized
                    // THUD (tiny dark noise tap: the button seating) and a muted low
                    // confirmation blip that settles downward (acknowledged, says the
                    // ship). No glassy tick, no dyad — the old rod chime fed the hum
                    // the user couldn't take. Backspace = the blip inverted (a
                    // gentle deny/un-press). Jump = ENGAGE: press, a slow rising
                    // two-tone confirm, and a distant engine surge answering from
                    // below decks. Everything soft — the whole console lives at a
                    // whisper (the standing law: never annoying).
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

                    // WATER — the droplet, done the way water actually sounds: a
                    // soft surface TAP and then the BLOOP — the collapsing air
                    // bubble's RISING chirp (the old voice fell in pitch, which is
                    // why it read as a dry blip, not a drop; real drops rise). A
                    // round attack and a low "gulp" partial keep it liquid.
                    // Backspace = the bloop reversed (falling — the drop climbing
                    // back out). Jump = a small splash: noise + three scattered
                    // late bloops. The stream/ocean bed is untouched.
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

                    // COMET — deep space. Not a bell any more: every key is a soft
                    // TRANSMISSION — a low hollow tone drifting downward a few cents
                    // (the doppler of something immense passing far away), a faintly
                    // beating twin under it so the pair shimmers darkly, a cold
                    // distant fifth, and the old whisper of ice dust. Eerie lives in
                    // the soft attack + detune-beat + drift; nothing clicks, nothing
                    // chimes. The excitement: once in a while a key throws a tiny
                    // SHOOTING STAR (a quick quiet streak falling out of the dark),
                    // and Jump is the full FLYBY — a doppler swoosh with a scatter of
                    // crystal debris glints. Backspace drifts UP: it recedes.
                    GlowStyle::Comet => {
                        let d = deg + if kind == SoundKind::Backspace { -2 } else { 0 };
                        let f = penta(220.0, d); // A3 region — the void register
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
                            n_lvl: 0.025, // ice dust, kept
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
                            // the wake — icy, not churchy (tenth of the old decay).
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
                                // hands stop — same register as the key grains, so
                                // the afterglow never turns back into dings.
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
                    // BEAM: the ship, not the hum. The old sustained root+fifth+octave
                    // chord was the single most complained-about sound in the whole
                    // engine ("agonizing", then "I hate it so much I can't take it")
                    // — a held tone is a whine no matter how soft. Replaced with the
                    // hull ambience of a rocket coasting through deep space: very low
                    // filtered air (the engines, decks away), no tonal content at
                    // all, breathing slowly. The power-down droop survives as the
                    // engine note darkening — the cutoff sinks as the energy dies, so
                    // the ship audibly winds down with the tube's visual fade.
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
                    // FIRE: the roar under the crackle — dark filtered noise whose
                    // loudness FLICKERS fast and irregularly, like flame light. The
                    // flicker (a ~9 Hz random flutter) is the one cue that separates
                    // fire-body from wind or surf: waves swell smoothly and slowly,
                    // flames gutter. (The old slow-LFO undulation here is exactly why
                    // this bed used to read as waves.)
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
                    // COMET: the void — a deep pair beating once every ~2 s (the
                    // slow dark pulse of empty space) under a cold fifth that
                    // breathes with the slow LFO. Replaces the old ~1 kHz shimmer,
                    // which read as tinnitus, with something felt in the chest.
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
                    // NYAN: faint detuned pulse pad — the chip's idle hum.
                    GlowStyle::Nyan => {
                        b.ph1 = (b.ph1 + 261.6 * dt).fract();
                        b.ph2 = (b.ph2 + 262.6 * dt).fract();
                        let s = (if b.ph1 < 0.5 { 1.0 } else { -1.0f32 })
                            + (if b.ph2 < 0.5 { 1.0 } else { -1.0f32 });
                        let k = (900.0 * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
                        b.lp1 += k * (s - b.lp1);
                        (b.lp1 * lvl * 0.022, 0.0)
                    }
                    // SPARKLE: no longer grains alone — a barely-there warm pad
                    // breathes underneath them (C4 pair, slow-swelling soft octave),
                    // so the background is a glow, not a void, and never a whine.
                    GlowStyle::Sparkle => {
                        b.ph1 = (b.ph1 + 261.6 * dt).fract();
                        b.ph2 = (b.ph2 + 262.4 * dt).fract();
                        b.ph3 = (b.ph3 + 523.9 * dt).fract();
                        let s = sin01(b.ph1) + sin01(b.ph2) + (0.08 + 0.15 * u2) * sin01(b.ph3);
                        (s * lvl * 0.022, s * lvl * 0.006)
                    }
                    // LASER gets a barely-there airy pad an octave under its voice.
                    // LASER: the storm on the horizon — deep filtered rumble whose
                    // level swells and sags slowly and UNEVENLY (a ~1.3 Hz random
                    // wander, not a metronome LFO): weather, not machinery. Replaces
                    // the old anonymous airy pad.
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
                    // LUMEN/CUSTOM: the lamp left on — the same detuned pair, plus a
                    // soft third a tenth above that breathes with the slow LFO, so
                    // the afterglow gently swells and dims instead of holding flat.
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

    /// PALETTE-REFACTOR BYTE-IDENTITY PROOF: for every style, a script that
    /// exercises every trail kind at varied pan/heat/hue plus a 25 cps flood
    /// renders BIT-IDENTICALLY on the refactored per-palette synth and the
    /// frozen v0.56 monolith. This is the "nine existing palettes remain
    /// byte-identical by default" contract, held as an executable oracle
    /// rather than a platform-pinned golden hash.
    #[test]
    fn palettes_render_byte_identical_to_v056_reference() {
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
                        assert_eq!(
                            a.to_bits(),
                            b.to_bits(),
                            "{style:?}: sample {i} diverged from v0.56"
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
    /// (Lumen) and Nyan palettes, and proves every jump in the burst actually
    /// speaks.
    #[test]
    fn brrrring_of_rapid_line_feeds_is_pinned() {
        for style in [GlowStyle::Lumen, GlowStyle::Nyan] {
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
                // pluck + grace note, Nyan's blip + 3-note arpeggio run.
                let per_jump = if style == GlowStyle::Nyan { 4 } else { 2 };
                assert!(
                    new.live_voices() >= per_jump,
                    "{style:?}: a rapid jump was thinned out of the brrrring"
                );
                for _ in 0..3 {
                    new.render(&mut nb);
                    old.render(&mut ob);
                    for (a, b) in nb.iter().zip(ob.iter()) {
                        assert_eq!(
                            a.to_bits(),
                            b.to_bits(),
                            "{style:?}: the brrrring drifted from v0.56"
                        );
                    }
                }
            }
            // And the ring-out.
            for _ in 0..100 {
                new.render(&mut nb);
                old.render(&mut ob);
                for (a, b) in nb.iter().zip(ob.iter()) {
                    assert_eq!(a.to_bits(), b.to_bits());
                }
            }
        }
    }
}
