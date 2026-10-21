// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The music box: Rainbow Kitty v2's instrument and its melody engine.
//!
//! Design: `docs/design/RAINBOW-KITTY-V2.md`, which the § references below
//! point into, except §3.x and §22: those name the prism and flow designs, which
//! are not in the tree.
//!
//! - Every key is its own derived note ([`MelodyV2::on_typed`]); the only
//!   same-pitch re-strike is a doubled letter's ([`Touch::ReStrike`]).
//! - The caret's column drives pan only; `deg` never depends on `pan`.
//! - One keystroke is one melody step at every typing speed. Smoothing comes
//!   from note choice in [`MelodyV2::derive`], never from withholding a step.
//! - The meteor ([`TrailSynth::v2_meteor`]) is one gesture whose time constants
//!   are multiples of `flight_ms(cells)`, so the bell rings on the landing frame.
//! - One lattice (§2.6): every pitch is a degree of the just major pentatonic
//!   [`super::PENTA`] at [`TINE_BASE_HZ`], reached through [`super::penta`],
//!   never `TrailSynth::melody_hz` (which applies the tone table's transpose).
//! - Determinism: no clock is read here; time arrives as `at_ms` and randomness
//!   from the synth's seeded stream (`TrailSynth::rnd_in`), so one script under
//!   one seed renders one waveform.

use super::glyph_class::{
    BANG, CLOSE, COLON, COMMA, DASH, DIGIT, LETTER, LINE, MATH, OPEN, QMARK, QUOTE, RISE, SECRET,
    SEMI, SIGIL, STOP,
};
use super::{
    ERASE_MIN_GAP, EventMeta, HELD_ERASE_RUN_WINDOW, OutputGesture, PAN_LAW_SCALE, Palette,
    Partial, SONG_FORM, SONG_THEME, SoundEvent, SoundGesture, SoundKind, TrailSynth, Voice,
    pan_gains, penta,
};
use crate::rainbow_kitty::meteor::tri;
use crate::rainbow_kitty::timing;

// §14 — lanes: polyphony, caps and stealing.

/// The identity lane: a voice tagged 0 bypasses lane bookkeeping entirely
/// (every voice the nine pinned palettes build); `claim_lane` returns early.
pub(super) const LANE_NONE: u8 = 0;
/// The verse: steps, re-strikes, the nav tick, the meteor bell, the cadence's
/// pickup and resolution. Cap 4 (§14).
pub(super) const LANE_TUNE: u8 = 1;
/// The bloom (§3.3): a lit key's slow-attacking 3f/4f/6f voice, the motif's
/// answering voice, and the air taps after a line end or rest. Cap 2.
pub(super) const LANE_BLOOM: u8 = 2;
/// The word downbeat's dyad, the meteor's thump, the cadence's tonic dyad.
/// Cap 1 — the bass is monophonic by law (§9.3).
pub(super) const LANE_BASS: u8 = 3;
/// The space's breath. Cap 1.
pub(super) const LANE_BREATH: u8 = 4;
/// Stardust glints. Cap 3, and the only lane besides BLOOM that drops an
/// incoming voice rather than cut a young one (§14's age guard).
pub(super) const LANE_GLINT: u8 = 5;
/// The meteor's tick, core and whoosh. Cap 3, exclusive: a new meteor damps
/// the old one over [`METEOR_INTERRUPT_S`].
pub(super) const LANE_METEOR: u8 = 6;
/// The meteor's five falling glints. Cap 3 (five are scheduled; at most three
/// are ever live, by the spacing).
pub(super) const LANE_RAIN: u8 = 7;
/// The Enter cadence's faraway ice bell. Cap 1.
pub(super) const LANE_CADENCE: u8 = 8;
/// The PTY line-feed brrrring (D18). Cap 4, one brrrring's notes, inside its own
/// [`CASCADE_EXCLUSIVE_MS`] window, so cascades never stack.
pub(super) const LANE_CASCADE: u8 = 9;
/// Grafts: voices that hang off a key without being a step (the shifted key's
/// ring, the `?` rise, the bracket tink, the `=` fifth; §10.4). Cap 2,
/// fade-steal: a graft is an identity, so the most-decayed graft yields over
/// [`LANE_FADE_STEAL_S`] rather than the newcomer being dropped. Two slots let
/// a shifted mark's ring and its graft sound together.
pub(super) const LANE_GRAFT: u8 = 10;
/// The ting: the bare Shift's bell ([`TING_LEVEL`]), alone. Cap 1: a second
/// Shift replaces the first. Its own lane keeps a graft under a shifted mark from
/// fade-stealing it mid-ring (`the_ting_rings_through_the_next_key`).
pub(super) const LANE_TING: u8 = 11;

/// Fade-steal / damp ramp, seconds. §9.5 law 2: a damp is a ramp, never a cut.
/// Aliases [`super::SPACE_DAMP_S`]: the same physical act at the same scale.
pub(super) const LANE_FADE_STEAL_S: f32 = super::SPACE_DAMP_S;

/// The tail law's ratio (§9.5 law 2): a voice's `dur` is at least this many of
/// its decay τ, so the envelope is at ≤ 7 % of peak (`e^−2.7`) before the
/// engine's 5 ms release ends it. Non-tine `*_DUR_S` values derive from it.
const TAIL_DUR_PER_TAU: f32 = 2.7;

/// Two tonal partials closer than this are one pitch (A11's exact-coincidence
/// bound): §9.5 law 5's same-pitch re-strike damps the old voice first.
const SAME_PITCH_HZ: f32 = 0.5;
/// The same law for a voice in the ting's register (1.2-2.5 kHz), as a ratio:
/// one cent. Same-note lattice pitches agree to float rounding; the nearest
/// different lattice pitch is a syntonic comma (21.5 cents) away.
const SAME_PITCH_CENTS: f32 = 1.0;

/// The age guard, seconds (§14): a voice younger than this has not delivered
/// its attack and is never stolen; [`lane_drops_the_newcomer`] decides who
/// loses instead.
pub(super) const LANE_AGE_GUARD_S: f32 = 0.040;

/// §14's cap table. Caps sum to 25, plus POOF/SWOOSH's 2 (unlaned, gated by
/// [`ERASE_MIN_GAP`]) = 27 of 28 slots. This budgets sounding voices, not
/// physical occupancy: pre-delays and fade tails still hold slots, so a batched
/// burst can exhaust the pool (decoration admission then protects the key's
/// strike). The bed pad renders outside the voice pool (§9.7).
///
/// CASCADE is 4 because one brrrring is four notes; one cascade at a time is
/// [`MelodyV2::on_jump`]'s [`CASCADE_EXCLUSIVE_MS`] window.
/// `the_lanes_hold_their_caps_and_steals_are_zero` runs §14's worst case.
pub(super) fn lane_cap(lane: u8) -> usize {
    match lane {
        LANE_TUNE | LANE_CASCADE => 4,
        LANE_BLOOM | LANE_GRAFT => 2,
        LANE_GLINT | LANE_METEOR | LANE_RAIN => 3,
        // The monophonic lanes; an unnamed lane is bounded too, never unlimited.
        LANE_BASS | LANE_BREATH | LANE_CADENCE | LANE_TING => 1,
        _ => 1,
    }
}

/// Who loses when a full lane's oldest voice is younger than
/// [`LANE_AGE_GUARD_S`]: `true` drops the newcomer, `false` steals anyway.
/// A missing GLINT or BLOOM is inaudible; a missing TUNE voice is a key that
/// made no sound, so the tune always speaks.
pub(super) fn lane_drops_the_newcomer(lane: u8) -> bool {
    matches!(lane, LANE_GLINT | LANE_BLOOM)
}

// The derived line (§3.1). No time gate: every constant below shapes which note
// a key gets; none can decide that a key gets no note.

/// The interval ladder. Real melodies are 70-80 % conjunct: over the 13
/// folded values this gives 7 seconds, 4 thirds and 2 fifths.
///
/// A repeat comes only from a repeated letter, never from the fold: letters
/// 13 apart (`a`/`n`) fold to 0 too, which would stutter. [`MelodyV2::derive`]
/// passes whether the raw difference is zero, so a folded-to-zero leap takes
/// the smallest real move instead.
const fn stride_mag(r: i32, repeated: bool) -> i32 {
    if repeated {
        return 0;
    }
    match r.abs() {
        0 => 1,
        1..=3 => 1,
        4..=5 => 2,
        _ => 3,
    }
}

/// The dead band. Inter-key jitter is ±25 %; within the band the alphabet's
/// sign keeps the contour, outside it the hand's acceleration or hesitation
/// takes it. Measured against the smoothed interval before this gap is folded
/// in ([`timing::IOI_EMA_ALPHA`]), so a gap is never its own reference.
const ACCEL_SHARE: f32 = 0.75;
const DECEL_SHARE: f32 = 1.35;

/// The register's middle, and how far the walk may wander before gravity pulls
/// it back; reflection acts only at the bounds. The pull is one-sided in
/// practice: the down-pull arms at degree 7, and the up-pull's arm (below 0) is
/// unreachable under [`TUNE_DEG_LO`] but keeps the rule correct if the register
/// moves.
const MELODY_CENTRE_DEG: i32 = 3;
const MELODY_GRAVITY_DEG: i32 = 3;

/// Three identical strides is a figure; the fourth inverts.
///
/// Counted on the sounded stride (`deg − from` after reflection, word-head snap
/// and motif answer, via [`MelodyV2::note_run`]), not on `derive`'s chosen one,
/// since those can turn a counted stride into a different sounded one. The
/// answer consults the guard too, and a head whose snap would complete a fourth
/// takes the nearest chord tone on the other side.
const MELODY_RUN_MAX: u8 = 3;

/// How far a word head may move to land on a chord tone (§3.1 step 6). Two
/// degrees span five pitch classes and every chord of [`CHORD_LOOP`] lights
/// three of five, so the search always lands.
const WORD_HEAD_SNAP_DEG: i32 = 2;

/// The line's motif: the first three intervals after a phrase rest or a
/// sentence's end are latched as the line's subject (four would fill a whole
/// ~4.5-letter English word).
const MOTIF_LEN: usize = 3;
/// The motif is answered at every third word head, transposed onto the chord
/// tone the head snapped to: about one key in thirteen (~3.9 s apart at prose
/// tempo), a recall rather than a loop. Counted in words, not seconds, so the
/// same text plays the same degrees at any typing speed.
const MOTIF_EVERY_WORDS: u8 = 3;

/// Machine regularity, not speed: three consecutive gaps agreeing this closely
/// on the same glyph is key autorepeat. A fast human hand is jittery and never
/// matches. The same-glyph condition keeps metronomic input (scripts, paste
/// replays) pitched.
const AUTOREPEAT_JITTER_MS: u32 = 2;
const AUTOREPEAT_RUN: u8 = 3;
/// …and an absolute backstop at ~40 cps, past any hand. This one asks nothing
/// of the glyph: nothing human puts two different keys 25 ms apart either.
const AUTOREPEAT_FLOOR_MS: u32 = 25;

/// A typing gap this long is a rest, and a rest resolves the line (§3.1): the
/// walk resolves onto the live chord, contour bias and run clear, and the motif
/// re-latches. The key ending the pause resolves the line, then sings its own
/// note. Shared with the ribbon via [`timing::PHRASE_PAUSE_MS`] and tempo-scaled
/// through [`timing::phrase_rest_ms`].
pub(super) const PHRASE_PAUSE_MS: u32 = timing::PHRASE_PAUSE_MS;

/// The IOI estimator's constants (§9.6), aliased from [`timing`] so the ribbon
/// runs the same law.
const IOI_DEFAULT_MS: f32 = timing::IOI_DEFAULT_MS;

/// Undo-stack depth in keystrokes (§10.1): about a word, so Backspace cannot
/// un-sing a note whose phrase has already closed.
const UNDO_N: usize = 8;

/// The PTY cascade's exclusivity window: one 4-note brrrring per 180 ms,
/// however many line feeds arrive.
pub(super) const CASCADE_EXCLUSIVE_MS: u32 = 180;
/// Inside a live cascade, the top-note re-strike's own floor (D18).
const CASCADE_RESTRIKE_MS: u32 = 60;
/// The line-feed run: a Jump this soon after the previous one is the next line
/// of the same output, so the pause that resolves a typed line ends a run
/// ([`CASCADE_EXCLUSIVE_MS`] is too short: at 5 lines/s every line feed would
/// start a new run).
const CASCADE_RUN_MS: u32 = PHRASE_PAUSE_MS;
/// A `Jump` this soon after a keyed `Enter` is that Return's own line-feed echo
/// and is swallowed (§10.4: a keyed Enter plays the cadence, no cascade). The
/// host also suppresses the echo (§16 row 13); this is the synth's own guard.
const ENTER_ECHO_SWALLOW_MS: u32 = 150;

/// Keys since the previous Enter below which the cadence is a bare tonic dyad
/// rather than the full pickup/resolution/bell (§11). A Return at an empty
/// prompt is not the end of a phrase.
const ENTER_PICKUP_MIN_KEYS: u8 = 4;

// §9.1 — the tine: one voice per key.

/// C5 — the lattice anchor (§2.6). Every pitched thing in v2 is
/// `penta(TINE_BASE_HZ, degree)`.
pub(super) const TINE_BASE_HZ: f32 = 523.25;

/// Voice attack, seconds. §9.5 law 1: ≥ 4 ms under 1 kHz, putting the onset's
/// splatter bandwidth (≈ 1/(2π·attack)) at 40 Hz, below an audible click.
const TINE_ATTACK_S: f32 = 0.004;
/// Added to `3·τ_v` to get `dur` (§9.1): at `3τ` the envelope is at −26 dB and
/// the extra 20 ms holds the engine's 5 ms release, so the voice ends quietly.
const TINE_DUR_TAIL_S: f32 = 0.020;

/// P1 — the sine body. No FM, no glide, no grace bend: the pitch is settled at
/// sample 0, which is what makes a fast passage legible as pitches.
const P1_LVL: f32 = 0.50;
/// P2, the octave, at −9.9 dB re P1. With the strike, it is the only tine
/// energy above 2 kHz; it dies well before the body (τ_v 55–110 ms), keeping
/// the tine warm rather than glassy.
const P2_RATIO: f32 = 2.0;
const P2_LVL: f32 = 0.16;
/// P2's own decay (§16 delta 1): the octave is the sparkle on the attack, not a
/// second note. Per-partial decays multiply the voice envelope, so at a 110 ms
/// body the effective τ is `1/(1/55 + 1/110)` ≈ 37 ms.
const P2_TAU_S: f32 = 0.055;
/// P3, the strike: the music-box bar's inharmonic mode, at −12.4 dB re P1.
/// 2.760 is a colour, not a harmonic; §9.5 law 4 admits it only because its τ is
/// at most 45 ms (A11): a 15 Hz beat needs 67 ms for one cycle, so the partial is
/// gone before roughness exists. [`P3_TAU_S`] is a constraint, not a taste knob.
const P3_RATIO: f32 = 2.760;
const P3_LVL: f32 = 0.12;
const P3_TAU_S: f32 = 0.040;

/// The felt mallet: band-passed noise sweeping up 1800 → 6400 Hz, the ear's
/// onset time-stamp and what makes the tine read as struck. The sweep lives on
/// the noise, which has no pitch to beat against the lattice (§9.5 law 4), and
/// sits above the tine's register so the chirp stays audible.
///
/// [`MALLET_LVL`] and [`MALLET_TAU_S`] are held by the limiter headroom pin
/// (`the_limiter_is_transparent_below_threshold_and_holds_the_ceiling_above`),
/// whose margin is thin: a louder or longer mallet fails it.
const MALLET_HZ0: f32 = 1800.0;
const MALLET_HZ1: f32 = 6400.0;
const MALLET_GLIDE_S: f32 = 0.005;
const MALLET_Q: f32 = 0.7;
const MALLET_LVL: f32 = 0.45;
/// The mallet's own decay (§16 delta 2) — 6 ms, so it is ≈ −36 dB by 25 ms.
const MALLET_TAU_S: f32 = 0.006;

/// τ_v, the voice decay, adaptive to the inter-onset interval (§9.1):
/// `clamp(110·(0.068 + 3.727·IOI_s), 28, 110)` ms, through 250 ms IOI → 110 ms
/// and 50 ms → 28 ms. Fast typing thins the notes, never the note count, and
/// note length keeps amplitude modulation out of the 15-60 Hz roughness band.
/// From 20 cps up the [`TAU_V_MIN_S`] floor is the operating point; multipliers
/// outside the clamp ([`RESTRIKE_TAU_MUL`]) go shorter still.
const TAU_V_BASE_S: f32 = 0.110;
const TAU_V_OFFSET: f32 = 0.068_181_8;
const TAU_V_SLOPE: f32 = 3.727_272_7;
const TAU_V_MIN_S: f32 = 0.028;
const TAU_V_MAX_S: f32 = 0.110;

/// The roof: a one-pole lowpass, §9.6's "brighter when fast, never louder".
/// Plain roof lerps 4200 → 5200 Hz over 4 → 12 cps; a chord-tone step adds
/// [`ROOF_LIT_ADD_HZ`], the glow's blaze up to [`ROOF_HEAT_HZ`], the ribbon's
/// hue up to [`ROOF_HUE_ADD_HZ`].
const ROOF_PLAIN_LO_HZ: f32 = 4200.0;
const ROOF_PLAIN_HI_HZ: f32 = 5200.0;
const ROOF_LIT_ADD_HZ: f32 = 2300.0;
const ROOF_MAX_HZ: f32 = 7500.0;
const ROOF_HEAT_HZ: f32 = 600.0;
/// The note's roof follows the rainbow arc (§3.3 item 3): it opens by up to
/// this much from the red end to the cyan end of `ev.hue`, added before the
/// [`ROOF_MAX_HZ`] clamp. Read through [`hue_arc`], so the wrap has no seam.
const ROOF_HUE_ADD_HZ: f32 = 900.0;
/// The cps span the plain roof opens across (4 → 12 cps).
const ROOF_CPS_LO: f32 = 4.0;
const ROOF_CPS_SPAN: f32 = 8.0;
/// A re-strike's roof sits 400 Hz under a step's plain roof (§9.2): the same
/// note further away, as a music box's second strike of one tooth sounds.
const RESTRIKE_ROOF_DROP_HZ: f32 = 400.0;

/// §9.2's re-strike touch: the tine with the strike partial at zero, a softer
/// octave and mallet, and a shorter τ.
const RESTRIKE_P2_LVL: f32 = 0.10;
const RESTRIKE_MALLET_LVL: f32 = 0.30;
const RESTRIKE_TAU_MUL: f32 = 0.7;
/// The re-strike ladder `L_n = max(0.60·0.85^(n−1), 0.35)`: −4.4, −5.8, −7.3,
/// −8.7, then the −9.1 dB floor. A held key decays into the texture instead of
/// hammering; the floor keeps the fifth re-strike a note.
const RESTRIKE_L0: f32 = 0.60;
const RESTRIKE_FALL: f32 = 0.85;
const RESTRIKE_FLOOR: f32 = 0.35;

// §3.3 — the rainbow: four additions in the existing `Voice` vocabulary (no
// reverb, no delay line). The tine itself is untouched.

/// The bloom: one extra voice per lit key in [`LANE_BLOOM`], a twelfth, two
/// octaves and a twelfth above that (§3.3's 3f / 4f / 6f), each decaying at
/// its own rate.
///
/// Stated as lattice degrees: on C, G and A they are exactly 3f / 4f / 6f; on
/// D and E the twelfth bends to the lattice's sixth, because the just
/// pentatonic's D–A is a comma off and exact harmonics would beat in the
/// 15-60 Hz roughness band (§9.5 law 4).
const BLOOM_DEGREES: [i32; 3] = [8, 10, 13];
const BLOOM_LVL: [f32; 3] = [0.10, 0.06, 0.035];
const BLOOM_TAU: [f32; 3] = [0.22, 0.30, 0.38];
/// The bloom fades in behind the strike. A bell arriving after its own mallet
/// is the enchantment cue that separates it from a brighter click.
const BLOOM_ATTACK_S: f32 = 0.018;
/// …12 ms behind the mallet, so the ear hears strike then bloom and not one
/// fatter transient.
const BLOOM_DELAY_S: f32 = 0.012;
/// The bloom must peak inside its own note: delay and attack scale with the
/// note's τ_v against the 4 cps reference (1.0 at prose tempo), floored here so
/// the strike-then-bloom order stays audible. At 10 cps and faster the head is
/// half: 6 ms behind the mallet, peaking 15 ms in.
const BLOOM_HEAD_MIN_MUL: f32 = 0.5;
/// The bloom's decay ceiling: the tine speaks, the bloom hangs, so notes overlap
/// and answer each other in the harmonics, never the fundamentals. The decay is
/// [`BLOOM_DECAY_TAU_MUL`] × τ_v under this ceiling, because a fixed decay stacks
/// at speed into an unbroken 2-7 kHz sheet.
const BLOOM_DECAY_S: f32 = 0.340;
/// The bloom decay multiplier: 3.1 × τ_v at the 4 cps reference (110 ms) meets
/// the 340 ms ceiling, so prose tempo is unchanged and the decay shortens only
/// as typing speeds up (149 ms at 10 cps, 87 ms at 20 cps), within 3-6× τ_v.
const BLOOM_DECAY_TAU_MUL: f32 = 3.1;
/// The tail law's `3τ + 20 ms` at the decay ceiling: the cap on a bloom's
/// per-note `dur`.
const BLOOM_DUR_S: f32 = 3.0 * BLOOM_DECAY_S + TINE_DUR_TAIL_S;
/// A slow twinkle on the hang — 5.5 Hz is well under the 15 Hz roughness
/// band, so it reads as shimmer and never as buzz.
const BLOOM_TW_RATE: f32 = 5.5;
const BLOOM_TW_DEPTH: f32 = 0.22;
/// The bloom's roof sits this far above the note's own: its partials live at
/// 3f-6f and a roof fitted to the fundamental would take the top off them.
const BLOOM_ROOF_ADD_HZ: f32 = 2400.0;
/// The bloom's level re the step it blooms behind, before [`hue_air`]. Fitted on
/// the bench: 2.0 keeps the prose centroid in 1250-1400 Hz with a 20 cps burst
/// still articulate, within §8's +1 dB loudness budget (3.0 crosses the 1600 Hz
/// glass line and swallows a third of the burst's onsets). Under budget pressure
/// this gives back first, never [`KEY_TINE_TRIM`].
const BLOOM_LEVEL: f32 = 2.0;
/// The bloom drifts with the colour: the strike stays on the caret's column
/// and the bloom's pan moves by up to this much with the hue.
const BLOOM_SPREAD: f32 = 0.28;

/// The answering voice (§3.3): on a motif-answering word head, one extra voice
/// in [`LANE_BLOOM`], this far behind the head, at the nearest lit chord tone
/// above the head's degree. A call and answer in a droppable lane.
const ANSWER_DELAY_S: f32 = 0.19;
/// −12 dB re the step.
const ANSWER_LEVEL: f32 = 0.25;

/// The air cloud (§3.3 item 4): two bloom taps behind a line's end (Enter) and
/// behind the key that ends a phrase rest, at unequal spacings and opposite
/// pans, which read as a room. Not per key (at 10 cps that would smear).
/// Levels are re the bloom they echo: −14 and −20 dB.
const AIR_TAP_DELAY_S: [f32; 2] = [0.09, 0.17];
const AIR_TAP_LEVEL: [f32; 2] = [0.199_526_2, 0.1];
const AIR_TAP_PAN: f32 = 0.4;

/// A capital's lift on the walk, in degrees (§3.1), bounded inside a word by
/// [`WORD_LEAP_MAX_DEG`]. A fifth is plainly a lift without driving the walk into
/// the ceiling from its middle; larger lifts reflect and fold back down, so a run
/// of capitals would lose its contour
/// (`a_run_of_capitals_keeps_its_contour_instead_of_stacking_on_the_ceiling`).
const CAPITAL_LIFT_DEG: i32 = 3;

// §22 — flow's voice: the music box opens as the hand finds its run. Flow
// (`EventMeta::flow`, 0..1) counts fast clean keys; here it is only a lerp
// parameter, so heat 0 is the cold instrument exactly. Its three levers add
// instrument, never notes or decibels (§9.6): an octave echo behind every lit
// step, a longer bass dyad, and a warmer tine octave with the strike's sum
// conserved.

/// The flow echo's delay: 25 ms, so the ear hears one note with a longer body,
/// well inside a flowing hand's IOI (83 ms at 12 cps).
///
/// It stays a fixed time: [`TrailSynth::spawn`] draws each oscillator's phase
/// independently, so tuning the delay to the note's period cannot fix the
/// relative phase; [`FLOW_ECHO_PHASE`] does that.
const FLOW_ECHO_DELAY_S: f32 = 0.025;
/// The flow echo's start phase: three quarters of a cycle on the strike's own
/// octave. The echo's fundamental is exactly the strike's [`P2_RATIO`]
/// partial, so the two interfere: in phase is loudest but raises the composite
/// peak at 12 cps (§9.6), quadrature sums in power on every key, and 0.75 beats
/// 0.25 on body and peak once the fundamental, the 2.760 partial and the roof's
/// phase shift are counted.
const FLOW_ECHO_PHASE: f32 = 0.75;
/// The flow echo's interval: one octave, five degrees on this lattice
/// ([`penta`] is `div_euclid(5)` over [`super::PENTA`]). A separate constant
/// from [`CAPITAL_LIFT_DEG`], so tuning the capital's register cannot turn the
/// echo into a beating sixth.
const FLOW_ECHO_OCTAVE_DEG: i32 = 5;
/// −12 dB re the step it follows, at heat 1 (the level [`ANSWER_LEVEL`] uses in
/// the same lane). At exactly zero flow the voice is not spawned at all.
const FLOW_ECHO_LEVEL: f32 = 0.25;

/// Key headroom at full flow (−0.4 dB): conserving the strike's partial sum does
/// not bound its sum with the bloom and echo at independent phases. Paid on a
/// square-root curve so the early crest is covered at quarter heat; cold is
/// identity.
const FLOW_KEY_HEADROOM: f32 = 0.954_992_6;

fn flow_key_headroom(flow: f32) -> f32 {
    lerp(1.0, FLOW_KEY_HEADROOM, flow.sqrt())
}

/// The capital rings: a shifted glyph is one strike and one ring, the ring in
/// [`LANE_GRAFT`] at −6 dB re the key's gain, [`CAP_RING_LIFT_DEG`] above the
/// octave. It replaces the flow echo on a shifted key
/// (`CAP_RING_LEVEL.max(FLOW_ECHO_LEVEL × flow)`), so there is one voice, not
/// two. Louder rings push a shouted line over the −14.5 dBFS ceiling; the
/// ring's presence comes from its register, clear of the strike's partials.
const CAP_RING_LEVEL: f32 = 0.501_187_2;
/// The capital's ring sits three pentatonic degrees past the octave
/// ([`FLOW_ECHO_OCTAVE_DEG`] + this): a twelfth. The strike stays the line's own
/// note, played [`forte`]; the flow echo stays at the octave. On this lattice
/// (`PENTA` = C D E G A, just), the ring over the strike:
///
/// ```text
///   strike   ring      interval over the octave   ring / strike
///   C        G         3/2  (a just fifth)        3.000
///   D        A         40/27 (a fifth, −21.5 ¢)   2.963
///   E        C         8/5  (a minor sixth)       3.200
///   G        D         3/2                        3.000
///   A        E         3/2                        3.000
/// ```
///
/// Every ring is a lattice tone and none lands on a partial of the strike's tine
/// (`1f`, [`P2_RATIO`], [`P3_RATIO`]), so there is no phase lock. Forte's lower
/// FM sideband ([`FORTE_FM_RATIO`] 4) sits at `3f`, but its index decays
/// ([`FORTE_FM_TAU_S`]) to under −20 dB re the ring by [`CAP_RING_DELAY_S`].
const CAP_RING_LIFT_DEG: i32 = 3;
/// When the ring opens: 60 ms (with a 30 ms swell), where the strike has decayed
/// to 0.37-0.45 and the bloom is past its crest, so the ring adds neither peak
/// (§9.6) nor an onset to the fine census.
const CAP_RING_DELAY_S: f32 = 0.060;
/// The ring's swell: 30 ms, so a Caps-Lock capital (ring, no pickup) stays one
/// onset by the fine census.
const CAP_RING_ATTACK_S: f32 = 0.030;
/// The bass dyad's decay at heat 1, from [`BASS_DECAY_S`]'s 0.111 s; `dur`
/// follows through the tail law ([`TAIL_DUR_PER_TAU`]). The octave partial
/// keeps its own [`BASS_OCT_TAU_S`]: its short bite defines the attack.
const FLOW_BASS_DECAY_S: f32 = 0.200;
/// The tine's octave partial at heat 1, from [`P2_LVL`]'s 0.16. Steps only: a
/// re-strike keeps [`RESTRIKE_P2_LVL`] so the tremolo stays a tremolo (§9.2).
const FLOW_P2_LVL: f32 = 0.22;

/// The step's fundamental and octave `(P1, P2)` at flow heat `flow` (§22 lever
/// 3); exactly `(P1_LVL, P2_LVL)` at `flow == 0.0`. P2 rises and P1 falls by the
/// same amount, so the strike's sum is 0.78 at every heat: warming P2 alone
/// would raise the onset peak, which §9.6 forbids.
fn flow_partials(flow: f32) -> (f32, f32) {
    let p2 = lerp(P2_LVL, FLOW_P2_LVL, flow);
    (P1_LVL - (p2 - P2_LVL), p2)
}

/// A passing (non-chord-tone) step is 2 dB under a lit one (§9.2). The chord
/// lights the verse; it never moves it (§10.3).
const PASSING_LEVEL: f32 = 0.794_328_2;

/// §9.6's loudness arc: `g_IOI = clamp(√(IOI_s / 0.25), 0.45, 1.0)`. The √ law
/// holds per-second energy flat as the rate climbs (A14: within +1 dB of 4 cps).
/// The reference is 4 cps ([`IOI_DEFAULT_MS`]), so the arc's knee sits there;
/// the floor `√(0.050/0.25)` is reached at 20 cps, and a higher floor would make
/// the melody louder the faster you type.
const G_IOI_REF_S: f32 = 0.25;
const G_IOI_MIN: f32 = 0.45;

/// Seeded velocity, in dB either side of nominal (§9.6). Drawn from the synth's
/// seeded stream, so a script replays bit-exactly (A27).
const VEL_DB_STEP: f32 = 1.0;
const VEL_DB_RESTRIKE: f32 = 1.5;
const VEL_DB_MOTION: f32 = 0.8;
/// Seeded pan jitter, ±0.03 (§9.1): un-stacks two voices on one column without
/// moving a sound off its glyph. In §9.1's units (after the stereo law's
/// `× 0.35`); [`TrailSynth::v2_pan`] divides it back out.
const PAN_JITTER: f32 = 0.03;

/// The loudness ladder's one scalar: the isolated nominal step peaks at
/// −21.0 dBFS at host volume 0.4, heat 0.5 (§9.1, A28), fitted on peak with
/// §9.6's velocity and pan jitter divided out.
const KEY_TINE_TRIM: f32 = 0.2741;

// §9.3 / §10.3 — the bass tine, the breath, and the pure-fifth chord loop.

/// C4 — the bass register's own anchor. §9.4 puts BASS at 261.6–436.0 Hz, and
/// every root and every dyad partner of [`CHORD_LOOP`] lands inside it.
const BASS_BASE_HZ: f32 = 261.63;

/// The four roots as exact ratios above [`BASS_BASE_HZ`] (§10.3): C 1/1,
/// A 5/3, F 4/3, G 3/2. These are the only roots whose fifth is pure on this
/// lattice (D–A is a wolf; E needs a B the pentatonic lacks). F is not a
/// PENTA degree, hence a separate table.
const CHORD_ROOT_RATIO: [f32; 4] = [1.0, 5.0 / 3.0, 4.0 / 3.0, 3.0 / 2.0];

/// One entry of §10.3's eight-bar loop. The partner is 3/2 above the root where
/// that stays under C5, else 4/3 below, so every dyad is an exact 3:2 or 4:3
/// inside [261.6, 436.0] Hz (A6).
///
/// The chord never transposes the verse: it only marks which notes are lit
/// (chord tone: lit roof, 0 dB, hero-eligible) or passing (plain roof, −2 dB).
#[derive(Clone, Copy, Debug)]
struct Chord {
    /// Index into [`CHORD_ROOT_RATIO`].
    root: usize,
    /// 1.5 (a pure fifth above) or 0.75 (a pure fourth below).
    partner: f32,
    /// Bit `d` set ⇒ verse degree `d` (mod 5) is a chord tone here.
    lit: u8,
}

/// I – vi – IV – V | I – V – vi – IV, advanced one step per word on the head of
/// every Space run (§10.3).
const CHORD_LOOP: [Chord; 8] = [
    // 0 · I  C  — C4 + G4, a pure fifth above.
    Chord {
        root: 0,
        partner: 1.5,
        lit: 0b0000_1101,
    },
    // 1 · vi Am — A4 + E4, a pure fourth below.
    Chord {
        root: 1,
        partner: 0.75,
        lit: 0b0001_0101,
    },
    // 2 · IV F  — F4 + C4. E is the maj7 here: the lush note, and lit.
    Chord {
        root: 2,
        partner: 0.75,
        lit: 0b0001_0101,
    },
    // 3 · V  G  — G4 + D4. A is the 9th.
    Chord {
        root: 3,
        partner: 0.75,
        lit: 0b0001_1010,
    },
    // 4 · I  C
    Chord {
        root: 0,
        partner: 1.5,
        lit: 0b0000_1101,
    },
    // 5 · V  G
    Chord {
        root: 3,
        partner: 0.75,
        lit: 0b0001_1010,
    },
    // 6 · vi Am
    Chord {
        root: 1,
        partner: 0.75,
        lit: 0b0001_0101,
    },
    // 7 · IV F
    Chord {
        root: 2,
        partner: 0.75,
        lit: 0b0001_0101,
    },
];

/// The chord a keyed Enter parks on (§10.3), so the next line's first word
/// boundary lands on I rather than Am.
const CHORD_AFTER_ENTER: u8 = 7;

// The rainbow sky: the drifting bed.

/// C3: two octaves under the tine and an octave under the bass dyad
/// ([`BASS_BASE_HZ`]). Every pad tone is `penta(BED_BASE_HZ, d)` for a lit
/// degree `d`, so the pad's top (degree 4, 218 Hz) stays below the bass
/// register.
pub(super) const BED_BASE_HZ: f32 = TINE_BASE_HZ * 0.25;
/// The number of chords the sky voices ([`CHORD_LOOP`]'s length), exported for
/// the parent's lattice law.
pub(super) const SKY_BED_CHORDS: usize = CHORD_LOOP.len();
/// Voicing balance, lowest tone first: the root-most tone carries, the upper
/// two colour. Static — the motion lives in pitch and in the hue.
const BED_WEIGHT: [f32; 3] = [1.0, 0.80, 0.65];
/// The pad never steps: portamento between chords, long enough that no
/// crossing is a step, short enough that a fast run through words audibly
/// moves.
const BED_GLIDE_TAU_S: f32 = 2.6;
/// The pad's level: air, not a drone, near −47 dBFS and at least 10 dB under the
/// typed line at 4 and 10 cps. The walk's pitch distribution is nearly flat, so
/// this pad is what says where home is.
const BED_LEVEL: f32 = 0.008_9;
/// A 12 s breath on the whole pad. Weather, not tremolo.
const BED_BREATH_HZ: f32 = 1.0 / 12.0;
const BED_BREATH_DEPTH: f32 = 0.35;
/// The hue's one-pole (τ, seconds) on the arc position the pad reads.
const BED_HUE_TAU_S: f32 = 1.2;
/// Tilt: one one-pole over the pad sum (reusing `bed.lp1`), warm and closed at
/// the red end of the arc, open at cyan. The whole travel stays under the
/// melody's register, so the pad's upper partials do not draw lines through the
/// notes, yet still moves the brightness by 1.3×.
const BED_TILT_LO_HZ: f32 = 350.0;
const BED_TILT_HI_HZ: f32 = 900.0;
/// Width: the top tone's twin is detuned by this many cents, so the pad's beat
/// rate walks from ≈ 0.45 Hz (4 cents on G3) to ≈ 1.4 Hz (11 cents on A3) as
/// the ribbon travels the arc. That slow beat is the shimmer.
const BED_DETUNE_CENTS_LO: f32 = 4.0;
const BED_DETUNE_CENTS_HI: f32 = 11.0;
/// The pad's partials, harmonic numbers at `1/n`: octaves and a fifth only, all
/// lattice intervals (no 7th harmonic), giving the hue's tilt something to act
/// on. The series stops at 4: partials 5, 6 and 8 of the 218 Hz tone land in the
/// melody's register (1090-1744 Hz), where a 6 dB/octave tilt cannot remove
/// them.
const BED_PARTIALS: [u32; 4] = [1, 2, 3, 4];
/// `1/√(Σ 1/n²)` over [`BED_PARTIALS`], so a pad tone has a single sine's RMS
/// and [`BED_LEVEL`] means what it says.
const BED_PARTIAL_NORM: f32 = 0.838_1;

const fn compute_sky_bed_degrees(chord: usize) -> [i32; 3] {
    let lit = CHORD_LOOP[chord % SKY_BED_CHORDS].lit;
    let mut out = [0i32; 3];
    let mut n = 0;
    let mut d = 0;
    while d < 5 {
        if lit & (1 << d) != 0 && n < 3 {
            out[n] = d;
            n += 1;
        }
        d += 1;
    }
    debug_assert!(n == 3, "every chord lights exactly three degrees");
    out
}

const SKY_BED_DEGREES_TABLE: [[i32; 3]; SKY_BED_CHORDS] = {
    let mut table = [[0i32; 3]; SKY_BED_CHORDS];
    let mut c = 0;
    while c < SKY_BED_CHORDS {
        table[c] = compute_sky_bed_degrees(c);
        c += 1;
    }
    table
};

/// Precomputed `penta(BED_BASE_HZ, d)` targets per chord in [`CHORD_LOOP`].
/// Every lit degree `d` lies in `0..5`, so `d.div_euclid(5) == 0` and
/// `BED_BASE_HZ * PENTA[d]` is bit-for-bit identical to `penta(BED_BASE_HZ, d)`.
const SKY_BED_TARGETS: [[f32; 3]; SKY_BED_CHORDS] = {
    let mut table = [[0.0f32; 3]; SKY_BED_CHORDS];
    let mut c = 0;
    while c < SKY_BED_CHORDS {
        let degs = SKY_BED_DEGREES_TABLE[c];
        table[c] = [
            BED_BASE_HZ * super::PENTA[degs[0] as usize],
            BED_BASE_HZ * super::PENTA[degs[1] as usize],
            BED_BASE_HZ * super::PENTA[degs[2] as usize],
        ];
        c += 1;
    }
    table
};

/// The sky's voicing for one chord of [`CHORD_LOOP`]: the chord's three lit
/// verse degrees (`Chord::lit`, bits 0..5 = C D E G A), ascending, on the
/// untransposed lattice. Voiced from the lit set rather than a root triad,
/// which on G or A would put an out-of-key B, C♯ or F♯ under the melody:
/// C E G on I, C E A on vi and IV, D G A on V.
#[inline]
pub(super) fn sky_bed_degrees(chord: usize) -> [i32; 3] {
    SKY_BED_DEGREES_TABLE[chord % SKY_BED_CHORDS]
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// §9.3's bass tine. Centroid under 500 Hz — the darkest sound in the theme.
const BASS_ROOT_LVL: f32 = 0.32;
const BASS_FIFTH_LVL: f32 = 0.22;
const BASS_OCT_LVL: f32 = 0.08;
/// The bass octave's own decay (§16 delta 1) — it thins the attack and leaves
/// the dyad, which is what keeps a downbeat *felt* rather than *heard*.
const BASS_OCT_TAU_S: f32 = 0.060;
const BASS_ATTACK_S: f32 = 0.008;
/// The bass dyad's decay: its 300 ms `dur` ("felt, not heard") over the tail law
/// ([`TAIL_DUR_PER_TAU`]).
const BASS_DECAY_S: f32 = 0.111;
const BASS_ROOF_HZ: f32 = 900.0;
/// The word downbeat's level, −4 dB re the step. With the head's twinkle
/// ([`SPACE_TWINKLE_LEVEL`]) the rendered head lands −4..−5 dB re the typed
/// letter: heard, never over it (§9.7). The verdict's dyads keep their own
/// [`VERDICT_DYAD_LEVEL`].
const BASS_LEVEL: f32 = 0.630_957_3;

/// §9.3's breath — the air a space moves, and the only thing a run's tail
/// says. Band-passed noise falling 900 → 380 Hz.
const BREATH_HZ0: f32 = 900.0;
const BREATH_HZ1: f32 = 380.0;
const BREATH_GLIDE_S: f32 = 0.080;
const BREATH_Q: f32 = 0.7;
const BREATH_ATTACK_S: f32 = 0.008;
const BREATH_DECAY_S: f32 = 0.055;
/// §11's 130 ms, raised to the tail law: [`TAIL_DUR_PER_TAU`] × 55.
const BREATH_DUR_S: f32 = TAIL_DUR_PER_TAU * BREATH_DECAY_S;
/// −16 dB re the step (§11).
const BREATH_LEVEL: f32 = 0.158_489_3;
/// The breath's roof, open: the engine's per-voice lowpass silences a voice
/// whose roof is 0. The band-pass shapes the exhale; the head's twinkle passes
/// through.
const BREATH_ROOF_HZ: f32 = 9000.0;
/// The word boundary's twinkle, on two of the head breath's silent partials,
/// so the downbeat gets a top with no new voice, lane or draw: the dyad's root
/// folded into the stardust band (the chord's own pitch class, so it never
/// beats against the verse) and the octave under it. A level on the breath
/// voice: −22 dB re the step, 8 dB under the dyad's root.
const SPACE_TWINKLE_LEVEL: f32 = 0.5;
/// The twinkle's octave-under partner, at −8 dB under it.
const SPACE_TWINKLE_UNDER: f32 = 0.2;
/// The indent steps: a run's head is the one downbeat; every later space plays
/// a short soft tine beside its breath, climbing the live chord's lit degrees
/// ([`indent_step_hz`]) and moving nothing (chord, playhead, beat). The level
/// sits between the breath (−16 dB) and the dyad (−4 dB), so each step is
/// heard and the run's peak stays the head's; in [`LANE_GLINT`] (drops the
/// newcomer) a step never steals the dyad, the breath or a tine.
const SPACE_STEP_LEVEL: f32 = 0.35;
/// The step's τ: a tick that climbs, gone in 155 ms (`3τ + 20 ms`) and about
/// −19 dB by a hand's next tap at 10/s, so the figure reads as a figure.
const SPACE_STEP_TAU_S: f32 = 0.045;
/// The step's rate gate on the cue clock, against the run's head or the last
/// admitted step (v1's [`super::SPACE_STEP_MIN_GAP`], in ms): a hand tapping
/// 80-125 ms apart hears every tap; a held spacebar (~30 Hz autorepeat) gets one
/// step per 75+ ms (`a_run_of_spaces_is_one_downbeat_and_a_rising_figure`).
const SPACE_STEP_MIN_GAP_MS: u32 = 75;
/// The steps' ceiling: two octaves over [`BASS_BASE_HZ`]. The figure climbs to
/// just under it and starts over ([`indent_step_hz`]).
const SPACE_STEP_HI_HZ: f32 = BASS_BASE_HZ * 4.0;

/// The indent step's pitch: the `n`-th step (1-based) of the run's rising
/// figure, the live chord's lit degrees on the keyed lattice from the first
/// above the dyad's `root` to the last under [`SPACE_STEP_HI_HZ`], then again.
/// On chord I over C4 that is E4 G4 C5 E5 G5; on vi over A4, C5 E5 A5.
fn indent_step_hz(chord: usize, root: f32, n: u32, key: i32) -> f32 {
    let lit = sky_bed_degrees(chord);
    let m = lit.len();
    let hz = |k: usize| penta(BASS_BASE_HZ, 5 * (k / m) as i32 + lit[k % m] + key);
    // The first lit degree over the root …
    let mut k0 = 0usize;
    for _ in 0..64 {
        if hz(k0) > root * 1.01 {
            break;
        }
        k0 += 1;
    }
    // … and how many lit degrees from it lie under the ceiling: the figure.
    let mut cycle = 0usize;
    while cycle < 64 && hz(k0 + cycle) < SPACE_STEP_HI_HZ {
        cycle += 1;
    }
    let cycle = cycle.max(1);
    let k = k0 + (n.saturating_sub(1) as usize) % cycle;
    let mut f = hz(k);
    // Only reachable when the figure's one note is the first lit degree and
    // it is already at the ceiling; `f` is finite and a few octaves wide.
    while f.is_finite() && f >= SPACE_STEP_HI_HZ {
        f *= 0.5;
    }
    f
}

// §10.4 — the class voices. The glyph class ([`EventMeta::glyph_class`])
// reshapes the key's tine and may add a decoration (`v2_typed`), and lets a
// phrase mark steer its own note ([`MelodyV2::on_typed`], [`cadence_target`]);
// the Space after a mark confirms the cadence in the bass. Every steered note
// is a function of the text: no key is gated, skipped or delayed.

/// [`Undo::last_mark`]'s values: what a typed key says about the phrase.
const MARK_NONE: u8 = 0;
const MARK_PERIOD: u8 = 1;
const MARK_BANG: u8 = 2;
const MARK_QMARK: u8 = 3;
const MARK_COMMA: u8 = 4;
const MARK_SEMI: u8 = 5;
const MARK_COLON: u8 = 6;
/// Set on [`Undo::last_mark`] by a doubled mark (`..`, `::`, `??`): the run
/// steers and confirms nothing. A bit beside the mark rather than zero, so the
/// third dot of `...` is not read as a fresh full stop. Any other key clears
/// it by overwriting the field.
const MARK_DOUBLED: u8 = 0x80;

/// The phrase mark a class is, or [`MARK_NONE`]. Keyed on the class (the
/// character never reaches the synth), so an echo-born cue (class 0) is a
/// letter.
fn phrase_kind(class: u8) -> u8 {
    match class {
        STOP => MARK_PERIOD,
        BANG => MARK_BANG,
        QMARK => MARK_QMARK,
        COMMA => MARK_COMMA,
        SEMI => MARK_SEMI,
        COLON => MARK_COLON,
        _ => MARK_NONE,
    }
}

/// The harmonic half: which [`CHORD_LOOP`] indices state the function a mark
/// asks the next Space for. `.` and `!` close on I (0, 4), `?` `,` `:` hang on
/// V (3, 5), `;` turns to vi (1, 6). `None` is the ordinary `chord + 1`.
fn mark_chords(mark: u8) -> Option<[u8; 2]> {
    match mark {
        MARK_PERIOD | MARK_BANG => Some([0, 4]),
        MARK_QMARK | MARK_COMMA | MARK_COLON => Some([3, 5]),
        MARK_SEMI => Some([1, 6]),
        _ => None,
    }
}

/// How far a steered mark may move the line, in degrees. Every candidate
/// table below is dense enough that the nearest candidate is inside it from
/// every degree of the register, and the anti-drone alternate is held to it.
const STEER_MAX_DEG: i32 = 3;

/// The candidate of `cands` nearest `from`; at equal distance the lower one
/// when `fall`, else the upper. `skip` is a degree to pass over (the
/// anti-drone's), or −1.
fn nearest_cand(cands: &[i32], from: i32, fall: bool, skip: i32) -> Option<i32> {
    let mut best: Option<i32> = None;
    for &c in cands {
        if c == skip {
            continue;
        }
        best = Some(match best {
            None => c,
            Some(b) => {
                let (dc, db) = ((c - from).abs(), (b - from).abs());
                if dc < db || (dc == db && ((c < b) == fall)) {
                    c
                } else {
                    b
                }
            }
        });
    }
    best
}

/// The melodic half: where a steering mark lands its own note. The candidate
/// nearest `from`, so the line is bent, never thrown; the move is at most
/// [`STEER_MAX_DEG`]:
///
/// | mark | role | candidates | tie |
/// |---|---|---|---|
/// | `.` | authentic cadence | C — 0, 5 | fall |
/// | `!` | sforzando arrival | C / G — 0, 3, 5, 8 | up |
/// | `?` | unresolved rise | the nearest G (3, 8) above, else the nearest D (1, 6) above; 8 holds | — |
/// | `,` | half cadence, a breath | D / G — 1, 3, 6, 8 | fall |
/// | `;` | deceptive turn | E / A — 2, 4, 7 | fall |
/// | `:` | announcement | G — 3, 8 | up |
///
/// Anti-drone, for `.` and `!` only: when the choice equals the degree the last
/// such cadence landed on (`last_cadence`) and another candidate is at most one
/// degree farther and within [`STEER_MAX_DEG`], that one is taken, so
/// consecutive sentences do not end on the same C. A `?` with nothing above
/// within the bound holds its note.
fn cadence_target(mark: u8, from: i32, last_cadence: i32) -> i32 {
    let (cands, fall): (&[i32], bool) = match mark {
        MARK_PERIOD => (&[0, 5], true),
        MARK_BANG => (&[0, 3, 5, 8], false),
        MARK_COMMA => (&[1, 3, 6, 8], true),
        MARK_SEMI => (&[2, 4, 7], true),
        MARK_COLON => (&[3, 8], false),
        MARK_QMARK => {
            let above = |cs: [i32; 2]| {
                cs.into_iter()
                    .find(|c| *c > from && *c - from <= STEER_MAX_DEG)
            };
            return above([3, 8]).or_else(|| above([1, 6])).unwrap_or(from);
        }
        _ => return from,
    };
    let first = nearest_cand(cands, from, fall, -1).unwrap_or(from);
    if !matches!(mark, MARK_PERIOD | MARK_BANG) || first != last_cadence {
        return first;
    }
    match nearest_cand(cands, from, fall, first) {
        Some(alt)
            if (alt - from).abs() <= (first - from).abs() + 1
                && (alt - from).abs() <= STEER_MAX_DEG =>
        {
            alt
        }
        _ => first,
    }
}

/// The aside is sotto voce: while a bracket `( [ {` is open, every key inside
/// it is struck −2 dB (this) under a darker roof ([`ASIDE_ROOF_MUL`]). Pitch is
/// unaffected. The opening bracket and the close that ends the aside are
/// outside it. Exactly ×1.0 outside an aside.
const ASIDE_GAIN: f32 = 0.794_328_2;
/// The aside's roof, ×0.8 of the key's own: further away, not muffled.
const ASIDE_ROOF_MUL: f32 = 0.8;
/// The aside expires after this many typed keys inside it, so an unclosed code
/// block `{` never stays quiet forever. Typed keys only; a Space is never
/// sotto voce.
const ASIDE_MAX_KEYS: u8 = 32;
/// Bracket-counter depth. Sotto voce is one level at any depth; the counter
/// keeps `f(g(x))` from ending the aside at its first `)`. Capped so a run of
/// `((((((` cannot bank closes for a later line.
const ASIDE_DEPTH_MAX: u8 = 4;

/// [`TypedPlan::quote`]'s values: what a quote did. `QUOTE_NONE` is every other
/// key and the apostrophe (`don't`: a quote mid-word with no quotation open).
const QUOTE_NONE: u8 = 0;
/// A quotation opens: a quote at a word head (after a Space, Enter, line feed
/// or session start) or straight after an open, with no quotation open. Its
/// note snaps up to the nearest live chord tone at or above the derived degree
/// and keeps the quote's ×2 sparkle and second wink ([`QUOTE_GLINT_MUL`]).
const QUOTE_OPENS: u8 = 1;
/// A quotation closes: any quote while one is open. Its note steers back toward
/// the quotation's opening degree by at most [`WORD_LEAP_MAX_DEG`], and it is
/// plain: one ×1 sparkle, no second wink.
const QUOTE_CLOSES: u8 = 2;

/// A number is a wood bar, the kalimba colour on the lattice: the tine's sine
/// with the odd harmonics 3f and 5f (degrees +8 and +12, exactly on the
/// lattice) instead of the octave and the 2.760 strike. 7f is off-lattice (it
/// beats against the glints), which also rules out a square-wave pulse. 3f at
/// 0.26 on A11's 45 ms bound carries the bar's identity. The pitch is the
/// ordinary walk: a digit's value as pitch would make a PIN audible and break
/// [`WORD_LEAP_MAX_DEG`]; the counting glint is the one value cue.
const WOOD_P2_RATIO: f32 = 3.0;
const WOOD_P2_LVL: f32 = 0.26;
const WOOD_P2_TAU_S: f32 = 0.045;
/// 5f at 0.08 with τ 30 ms: the bar's top, gone before it reads as a second
/// note. At C6 it is 2616 Hz, under the plain roof.
const WOOD_P3_RATIO: f32 = 5.0;
const WOOD_P3_LVL: f32 = 0.08;
const WOOD_P3_TAU_S: f32 = 0.030;
/// The partial sum is conserved (§9.6): the bar's three sum to the letter's
/// 0.78, so the onset peak matches. Written as arithmetic so a retune of either
/// bar partial moves P1 with it.
const WOOD_P1_LVL: f32 = P1_LVL + P2_LVL + P3_LVL - WOOD_P2_LVL - WOOD_P3_LVL;
/// The harder "tok": the felt mallet's band moved up (1400 → 3600 Hz) and
/// shortened (τ 4 ms, from 6) at the same [`MALLET_LVL`], as a wooden bar
/// struck with a harder beater. Q 1.0, under §9.5's 1.6.
const WOOD_MALLET_HZ0: f32 = 1400.0;
const WOOD_MALLET_HZ1: f32 = 3600.0;
const WOOD_MALLET_Q: f32 = 1.0;
const WOOD_MALLET_TAU_S: f32 = 0.004;
/// A number blips: its τ_v is 0.7 × the letter's, so a digit run reads faster
/// than a word at the same rate. The bloom behind a lit digit keeps the
/// unshortened step τ.
const DIGIT_TAU_MUL: f32 = 0.7;

/// The sparkle counts: a digit's glint is its own degree on the stardust
/// lattice, `COUNT_GLINT_DEG0 + digit % 5` plus `song_key` (G7 A7 C8 D8 E8 at
/// key 0, inside [`GLINT_LO_HZ`]..[`GLINT_HI_HZ`] with no fold), so `0 1 2 3 4`
/// climbs and `5 6 7 8 9` climbs again. A digit does not advance the rotation
/// cursor `glint_k`.
const COUNT_GLINT_DEG0: i32 = 13;
/// `typed_glyph_rank(Some('0'))`: digits have ranks 27..36, so `rank − 27` is
/// the digit. A constant because the rank producer is not `const`; pinned
/// against it by the digit test.
const DIGIT_RANK0: i32 = 27;
/// Past five it brightens: digits 0..4 sparkle at ×2 (−18 dB re the step),
/// 5..9 at [`KEY_GLINT_SHIFTED_MUL`] (−12). Two brightnesses × five pitches
/// make ten.
const COUNT_GLINT_LOW_MUL: f32 = 2.0;

// The pluck family (punctuation): a mark is a pitched staccato pluck on the
// same walk, lattice and `song_key` as a letter, with its own partials, a
// short τ and a dark fingertip. Every mark is plucked except the opening
// bracket and the three that phrase by ringing (the bang, the dash's tie, the
// steering `.`); see [`pluck_class`]. Each bucket adds one gesture (§10.4):
// stops breathe, `?` rises, `!` and quotes wink twice, brackets tink, lines
// and the slash zip, operators and the announcing `:` sound their fifth. A
// pluck does not hang: no bloom, no flow echo.

/// The pluck's τ: 0.5 × the step's τ_v, floored at 25 ms (13 cycles of C5, so
/// it is heard as a pitch). Family note lengths: 1.0 / [`DIGIT_TAU_MUL`] 0.7 /
/// 0.5. At the floor `3τ + 20` is 95 ms, under 10 cps's 100 ms key gap.
const PLUCK_TAU_MUL: f32 = 0.5;
const PLUCK_TAU_MIN_S: f32 = 0.025;
/// The pluck's partials: `1f` and `4f` only (the letter has `{1, 2, 2.76}`,
/// the digit `{1, 3, 5}`). 4f is the double octave, exactly on the lattice: a
/// string plucked near its end, a glassy top gone in 30 ms over a strong
/// fundamental. At D6 it is 4709 Hz, under every roof. P3 is muted. The partial
/// sum is the letter's 0.78 (§9.6).
const PLUCK_P1_LVL: f32 = 0.60;
const PLUCK_P2_RATIO: f32 = 4.0;
const PLUCK_P2_LVL: f32 = 0.18;
const PLUCK_P2_TAU_S: f32 = 0.030;
/// The fingertip: band-passed noise falling 1400 → 500 Hz over 15 ms at Q 0.9
/// (§9.5's 1.6), at 0.5, which sits 24-29 dB under the tone over the first
/// 30 ms: a note with a dark attack. In `Voice::weight()`'s quietest-steal
/// order a pluck sits beside a letter.
const PLUCK_NOISE_LVL: f32 = 0.5;
const PLUCK_HZ0: f32 = 1400.0;
const PLUCK_HZ1: f32 = 500.0;
const PLUCK_Q: f32 = 0.9;
const PLUCK_NOISE_TAU_S: f32 = 0.015;
/// `!` sparkles twice: the second ×4 glint lands 90 ms after the strike, far
/// enough from the first ([`KEY_GLINT_DELAY_S`]) to be a second wink and inside
/// the next key at 8 cps.
const BANG_GLINT2_DELAY_S: f32 = 0.090;
/// The sentence's full stop rings: the steering `.` is a tine on ×1.35 the
/// step's τ_v (forte's stretch, [`FORTE_TAU_GAIN`], without the hammer), under
/// [`MARK_TAU_MAX_S`]. An arrival is held.
const CADENCE_STOP_TAU_MUL: f32 = 1.35;
/// The dash is a tie: the note before it again, held ×1.5. Mid-word that is the
/// engine's re-strike (×1.5 on [`RESTRIKE_TAU_MUL`]'s 0.7); at a word head a
/// step on the common tone. No zip.
const DASH_TAU_MUL: f32 = 1.5;
/// The ceiling on both — [`FORTE_TAU_MAX_S`]'s 150 ms, for forte's reason:
/// past it the tail law's `3τ + 20` runs into the next word at prose rates.
const MARK_TAU_MAX_S: f32 = 0.150;
/// A steered `,` `;` `:` is −2 dB, a breath in the phrase; the full stop and
/// the bang are arrivals at the letters' level. Applied only to those marks
/// ([`TrailSynth::v2_typed`]'s `phrase_gain`).
const PAUSE_MARK_GAIN: f32 = 0.794;
/// …and its air is ×0.6 of the full stop's, which is the Space's own
/// ([`BREATH_LEVEL`]): a comma is a shorter breath than a sentence's end.
const PAUSE_BREATH_MUL: f32 = 0.6;
/// `?` rises: a second P1 sine three lattice degrees up (a spoken question
/// rises about a fifth), 60 ms after the pluck, at −3 dB, in [`LANE_GRAFT`],
/// possibly above [`TUNE_DEG_HI`]. From a sounding D the rise is two degrees, a
/// pure fourth to G, since D–A is this lattice's 40/27 wolf
/// (`the_question_never_rises_a_wolf_in_any_key`).
const QUEST_RISE_DEG: i32 = 3;
const QUEST_RISE_D_DEG: i32 = 2;
const QUEST_RISE_DELAY_S: f32 = 0.060;
const QUEST_RISE_LEVEL: f32 = 0.707_945_8;
/// The bracket's grace note, the tink: a P1-only sine one lattice degree up
/// for `( [ {` and down for `) ] }` (reflected into the register), 45 ms after
/// the key on a 35 ms τ (dur 2.7τ), at −8 dB. `(` and `)` share rank 42, so a
/// `)` right after `(` re-strikes its degree; the graft gate lets a close
/// straight after an open through (`MelodyV2::prev_class`), and the tink's
/// direction tells the pair apart.
const TINK_DEG: i32 = 1;
const TINK_DELAY_S: f32 = 0.045;
const TINK_TAU_S: f32 = 0.035;
const TINK_DUR_S: f32 = TAIL_DUR_PER_TAU * TINK_TAU_S;
const TINK_LEVEL: f32 = 0.398_107_2;
/// Quotes sparkle twice and small: ×2 (−18 dB) at [`KEY_GLINT_DELAY_S`] and
/// again 45 ms after the key — two little dots, closer than the bang's 90.
const QUOTE_GLINT_MUL: f32 = 2.0;
const QUOTE_GLINT2_DELAY_S: f32 = 0.045;
/// The zip: the pluck body with its noise swept instead of plucked,
/// 3200 → 900 Hz over 40 ms for a line drawn (`_ ~ \ |`), 900 → 3200 for the
/// rising slash. One voice; noise has no pitch to beat with.
const ZIP_HZ_LO: f32 = 900.0;
const ZIP_HZ_HI: f32 = 3200.0;
const ZIP_GLIDE_S: f32 = 0.040;
const ZIP_TAU_S: f32 = 0.030;
/// The fingertip's own level ([`PLUCK_NOISE_LVL`]): a zip is a pluck whose
/// noise moves, not a louder one.
const ZIP_MALLET_LVL: f32 = 0.5;
/// The operator's fifth (`+ = * % ^ < >`): the rise's voice swelling on
/// [`BLOOM_ATTACK_S`] from 30 ms, at −6 dB, a dyad opening under the pluck
/// (unlike `?`, which strikes its fifth later and louder). A swell cannot make
/// a new maximum (§9.6).
const FIFTH_DEG: i32 = 3;
const FIFTH_DELAY_S: f32 = 0.030;
const FIFTH_LEVEL: f32 = 0.501_187_2;
/// A steering stop breathes (`. , ; :` via [`stop_breathes`], when
/// [`TypedPlan::steered`]): the space's exhale ([`breath`], −16 dB) 20 ms after
/// the pluck, after its 15 ms fingertip. In [`LANE_BREATH`] (cap 1), so a
/// following space's breath fade-steals it.
const STOP_BREATH_DELAY_S: f32 = 0.020;

// §11 — the small gestures: the nav tick and the Shift ting.

/// The nav tick (§11, D17): the verse note, P1 only, no mallet — the sound of
/// a hop too short to be a meteor. −24 dB: present, never an event.
const NAV_ATTACK_S: f32 = 0.004;
const NAV_DECAY_S: f32 = 0.030;
/// §11's 60 ms, raised to the tail law: [`TAIL_DUR_PER_TAU`] × 30.
const NAV_DUR_S: f32 = TAIL_DUR_PER_TAU * NAV_DECAY_S;
const NAV_LEVEL: f32 = 0.063_095_73;

/// The bare Shift is a ting: a music box's top note on a tone of the live
/// chord, the high counterpart to the Space bar's low dyad. Its pitch is the
/// root, fifth or third of the chord ([`TING_TONES`], [`TING_PICK`]), folded
/// into [`TING_LO_HZ`]'s octave, so successive Shifts on one chord never ring
/// one pitch twice running and a word-head capital 60 ms later is a chord
/// interval away. Its timbre is a pure sine (FM sidebands clank), its octave, the
/// tine's 2.76f clink gone in 30 ms (A11), a quiet felt mallet and a real roof.
///
/// It rings through the key it announces: a second Shift replaces it, as does
/// a strike on its pitch (§9.5 law 5). It is never a step.
///
/// A 3 ms strike: §9.5 law 1's 4 ms floor applies under 1 kHz; at 1.2-2.5 kHz
/// 3 ms is four cycles.
const TING_ATTACK_S: f32 = 0.003;
/// The ring: a 200 ms τ, −10.8 dB re peak 250 ms after the key.
const TING_DECAY_S: f32 = 0.200;
/// v1's ting ring-out law ([`super::SHIFT_TING_DUR_S`]) on v2's τ:
/// [`super::RING_OUT_PER_TAU`] × τ + the release ramp, so the ting is at −43 dB
/// before the ramp starts (`tails_end_quietly_and_damps_are_ramps`).
const TING_DUR_S: f32 = super::RING_OUT_PER_TAU * TING_DECAY_S + super::RELEASE_RAMP_S;
/// The ting's roof, a real filter: `lp_k` = 0.851 at 48 kHz (the one-pole's
/// coefficient saturates at 7639 Hz, so anything above that is a bypass).
const TING_LP_HZ: f32 = 6500.0;
/// The octave partial and its extra decay (per-partial decay multiplies the
/// voice envelope, so the octave's τ is 1/(1/0.200 + 1/0.140) = 82 ms: it
/// rounds the attack and leaves the fundamental ringing).
const TING_P2_LVL: f32 = 0.12;
const TING_P2_TAU_S: f32 = 0.140;
/// The tine's clink at [`P3_RATIO`], making it the letters' instrument, on a
/// 30 ms extra decay: under A11's 45 ms exemption, and at most
/// 2.76 × 2480 = 6.8 kHz, under the roof's knee.
const TING_P3_LVL: f32 = 0.08;
const TING_P3_TAU_S: f32 = 0.030;
/// The felt mallet: the tine's band, Q and τ at under a quarter of the tine's
/// 0.45, since its 1.8 → 6.4 kHz chirp shares the ting's octave.
const TING_MALLET_LVL: f32 = 0.10;
/// The ting's register, `[1240, 2480)` Hz, v2's own: v1's
/// [`super::SHIFT_TING_LO_HZ`] (1318.5) would put E6 (1308 Hz) under the floor
/// and double it to a shrill 2616 Hz.
const TING_LO_HZ: f32 = 1240.0;
/// Which of the chord's three tones the k-th Shift rings: root-weighted (two
/// roots, two fifths, one third per lap), no two neighbours equal, the wrap
/// `[4] → [0]` included.
const TING_PICK: [usize; 5] = [0, 1, 0, 2, 1];
/// The chord's three ting tones, indexed by `Chord::root` (into
/// [`CHORD_ROOT_RATIO`]: C, A, F, G) and then by [`TING_PICK`]; each entry a
/// pentatonic class (C D E G A = 0..4), lit in every [`CHORD_LOOP`] chord on
/// that root (pinned against the `lit` masks).
const TING_TONES: [[i32; 3]; 4] = [
    [0, 3, 2], // root C (I):  C, G, E
    [4, 2, 0], // root A (vi): A, E, C
    [0, 4, 2], // root F (IV): C — F's fifth; F itself is off the lattice — A, E
    [3, 1, 4], // root G (V):  G, D, A
];
/// The ting's level against `KEY_TINE_TRIM`, fitted by rendering to
/// −4.5 ± 1.0 dB re the walk-mean typed key and bounded below by a music-box
/// floor of 0.50 of the first keystroke. The capital crest is answered by
/// [`TING_DUCK`], not by this level. No velocity draw and no `g_ioi`: a
/// modifier has no IOI.
const TING_LEVEL: f32 = 0.49;

/// The bell yields to the capital it announced: when a shifted letter that
/// opens a word or shifted run lands ([`Forte::ring`]), a live ting eases to
/// this fraction of its level over [`TING_DUCK_RAMP_S`] and keeps ringing there
/// on its own τ (`the_ting_rings_through_the_next_key`). Unducked, the bell and
/// the capital crest together over the −14.5 dBFS ceiling on the session's
/// first key (§0, F16). A Shift before a lowercase letter, a digit or a Space
/// rings untouched
/// (`the_bell_yields_to_the_shifted_key_it_announced_and_to_nothing_else`);
/// a shifted mark takes [`TING_DUCK_MARK`].
const TING_DUCK: f32 = 0.66;
/// The duck under a shifted mark that is not a capital letter (`( ) { } & +
/// : ? !` and kin): −6 dB, the same glide, never a damp. A one-second bell over
/// a 25 ms pluck is a drone, and code has many shifted marks: unducked, a line
/// of code is louder than prose past the design's C1 bound (+1.5 dB). Below a
/// half the curve is flat, since much of the bell's energy is spent before the
/// key lands.
const TING_DUCK_MARK: f32 = 0.50;
/// The duck's ramp: the house 12 ms (§9.5 law 2, a damp is a ramp). Shorter
/// ramps give the same crest, so the gentlest is used.
const TING_DUCK_RAMP_S: f32 = LANE_FADE_STEAL_S;

/// Octave-folds a pitch into the v2 ting's register `[TING_LO_HZ, 2×)`; v1's
/// [`super::ting_octave`] is separate.
fn ting_fold(mut f: f32) -> f32 {
    while f >= TING_LO_HZ * 2.0 {
        f *= 0.5;
    }
    while f < TING_LO_HZ {
        f *= 2.0;
    }
    f
}

/// The ting's voice at `f` (see [`TING_ATTACK_S`]): a pure sine, its octave,
/// the tine's clink and a quiet felt mallet under a real roof, in
/// [`LANE_TING`].
fn ting(f: f32) -> Voice {
    Voice {
        dur: TING_DUR_S,
        attack: TING_ATTACK_S,
        decay: TING_DECAY_S,
        p: [
            Partial {
                lvl: P1_LVL,
                f0: f,
                ..Partial::default()
            },
            Partial {
                lvl: TING_P2_LVL,
                f0: f * P2_RATIO,
                decay: TING_P2_TAU_S,
                ..Partial::default()
            },
            Partial {
                lvl: TING_P3_LVL,
                f0: f * P3_RATIO,
                decay: TING_P3_TAU_S,
                ..Partial::default()
            },
        ],
        n_lvl: TING_MALLET_LVL,
        n_f0: MALLET_HZ0,
        n_f1: MALLET_HZ1,
        n_glide: MALLET_GLIDE_S,
        n_q: MALLET_Q,
        n_decay: MALLET_TAU_S,
        lp_cut: TING_LP_HZ,
        lane: LANE_TING,
        ..Voice::default()
    }
}

/// The TUNE lane's degree span, `C5..G6` = 0..8 (§9.4): what [`reflect_deg`]
/// folds a walk accent or a bracket's tink back into.
const TUNE_DEG_LO: i32 = 0;
const TUNE_DEG_HI: i32 = 8;

// §13 — the stardust glint.

/// The glint's rotating interval above the verse note (§13): lattice degrees,
/// so a glint coincides with a live harmonic or sits a consonant interval from
/// it and never beats.
const GLINT_DEGREES: [i32; 3] = [7, 9, 11];
/// The stardust lane (§9.4), into which every glint is octave-folded: every
/// star is the same light whatever note threw it.
const GLINT_LO_HZ: f32 = 2800.0;
const GLINT_HI_HZ: f32 = 5600.0;
const GLINT_ATTACK_S: f32 = 0.002;
const GLINT_DECAY_S: f32 = 0.040;
/// §13's 95 ms, raised to the tail law: [`TAIL_DUR_PER_TAU`] × 40.
const GLINT_DUR_S: f32 = TAIL_DUR_PER_TAU * GLINT_DECAY_S;
/// −18 dB re the step (§11).
const GLINT_LEVEL: f32 = 0.125_892_5;
/// The sparkle on every key, as a multiplier on [`GLINT_LEVEL`]: −24 dB re the
/// step. A short glint buys brightness without costing note separation, as a
/// louder bloom would.
///
/// It ignores the hue ([`hue_air`] colours the bloom) but takes §9.6's loudness
/// arc: the caller multiplies by the strike's `g_ioi`, so the sparkle does not
/// overtake the melody at speed
/// (`all_glyph_sparkles_share_the_strikes_loudness_arc`). It takes `g`, not
/// `plan.level`, so a passing note gets the full sparkle too.
const KEY_GLINT_LEVEL_MUL: f32 = 0.5;
/// A shifted key's sparkle multiplier: ×4, −12 dB re the step, clearing the
/// masked threshold of the tine's partials in 3-5 kHz; off the crest with a
/// 40 ms τ, so it costs no decibel at speed. Read by the digit 5-9 counting
/// glint and the OPEN / BANG class rule; a shifted letter uses
/// [`FORTE_GLINT_MUL`], a shifted mark follows its class.
const KEY_GLINT_SHIFTED_MUL: f32 = 4.0;

/// A shifted key is forte, not higher: the same note struck harder, with no
/// pitch bend and the line's lit roof as a real filter ([`forte`],
/// [`Forte::of`]). The hardness is harmonic phase modulation,
/// `sin(ph + idx·sin(fm_ph))`: constant-amplitude, so the sidebands come out of
/// the carrier, never onto the peak. At ratio 4 they sit at 3f, 5f, 7f and 9f,
/// never on the fundamental, the tine's 2f or its 2.76f, and all harmonics of
/// the note.
const FORTE_FM_RATIO: f32 = 4.0;
/// The modulation index at force 1 at or under [`FORTE_FM_REF_HZ`]: 3f and 5f
/// ≈ −6 dB re the carrier at onset, gone within ~100 ms ([`FORTE_FM_TAU_S`]).
const FORTE_FM_INDEX: f32 = 0.9;
/// B♭5. Above it the index falls as `(ref / f)²` ([`forte_index`]), so the
/// sideband spectrum stops climbing with the note, as a piano's top octave
/// has fewer audible partials.
///
/// Second-order sidebands scale with the index squared; this reference keeps
/// C5..A5 at the full hammer and holds energy over 8 kHz under the −50 dB
/// "not harsh" bound at the top of the register (index 0.32 at G6), and the
/// 19-24 kHz band under −70 dBFS at the `song_key` ceiling.
const FORTE_FM_REF_HZ: f32 = 932.33;
/// The index's decay, seconds: A11's 45 ms exemption, so the hardness is an
/// onset and the body is the warm tine.
const FORTE_FM_TAU_S: f32 = 0.045;
/// The hammer's modulator lock ([`TrailSynth::v2_lock_hammer`]), in turns:
/// where the ratio-4 modulator stands when the fundamental crosses zero going
/// up. To first order the struck cycle is
/// `sin θ + (i/2)·(sin(3θ + c) + sin(5θ + c))`, flat-topped at `c = 0` and
/// spiked at half a turn. The key carrying [`WORD_CAPITAL_GAIN`], the only one
/// loud enough for its crest (with octave, mallet and ting) to bind, is struck
/// flat-topped to stay under the −14.5 dBFS ceiling.
const FORTE_FM_TURN_HEAD: f32 = 0.0;
/// Every other forte key is struck spiked, because its floor binds from the
/// other side: a capital's sample peak is ≥ +2.0 dB over its plain self (F3),
/// which the gain alone does not deliver. 0.54 is the top of a narrow plateau;
/// the pin's tightest take clears the floor by under 0.01 dB.
const FORTE_FM_TURN: f32 = 0.54;
/// How much level the octave takes from the fundamental at force 1: a
/// transfer, so `Σ p.lvl` and the peak are unchanged. Energy is not (the octave
/// decays faster, [`P2_TAU_S`]), so larger transfers push a mid-word capital
/// under the ≥ +2.0 dB floor.
const FORTE_P2_SHIFT: f32 = 0.06;
/// …and the octave's ceiling, so a hand already in flow
/// ([`flow_partials`]) is not pushed past a music box's balance.
const FORTE_P2_MAX: f32 = 0.30;
/// The octave's extra ring at force 1, seconds over [`P2_TAU_S`]. 2f is
/// harmonic, so A11's 45 ms bound (about inharmonic partials) does not bind.
const FORTE_P2_TAU_ADD_S: f32 = 0.045;
/// A harder blow sustains: the voice τ at force 1 is × (1 + this) …
const FORTE_TAU_GAIN: f32 = 0.35;
/// … under this ceiling, seconds ([`TAU_V_MAX_S`] × 1.35 = 0.1485).
const FORTE_TAU_MAX_S: f32 = 0.150;
/// The roof opens by this much at force 1, Hz, under [`ROOF_MAX_HZ`] — which
/// is under the 7639 Hz where the one-pole stops being a filter.
const FORTE_ROOF_ADD_HZ: f32 = 900.0;
/// A shifted letter's sparkle re the plain key's: ×2 (−18 dB re the step).
/// Forte's onset brightness carries the identity, so the glint stays below
/// [`KEY_GLINT_SHIFTED_MUL`].
const FORTE_GLINT_MUL: f32 = 2.0;
/// A shifted mark's weight (`( ) { } : " _ + * < > |` …): +1.0 dB, not the
/// letter's +2.6 ([`super::SHIFT_GLYPH_GAIN`]). Half of code punctuation is
/// shifted on a US layout; a capital's accent on each would be noise.
const MARK_SHIFT_GAIN: f32 = 1.122;

/// A capital letter at a word head is louder still: +3.0 dB on top of its
/// weight ([`super::SHIFT_GLYPH_GAIN`]), so it stands clear of the text around
/// it rather than at the loudness JND. Not a shifted mark and not an interior
/// capital: one accent per word. At the host default volume the head crests
/// near −15.4 dBFS, under the bus limiter's −14 dBFS knee.
const WORD_CAPITAL_GAIN: f32 = 1.412_537_5;

/// The key sparkle arrives with the bloom, the instant it opens. A glint at
/// delay zero peaks inside the strike's attack and adds phase-random jitter to
/// the crest, which (a peak being a max over keys) only ever raises it; a level
/// trim large enough to hide the jitter would erase the sparkle.
const KEY_GLINT_DELAY_S: f32 = BLOOM_DELAY_S + BLOOM_ATTACK_S;
/// Depth 0.5 gives 1–1.5 winks across the glint's life — the star's own
/// opening and closing, heard.
const GLINT_TWINKLE_DEPTH: f32 = 0.5;
/// The rate a glint takes when the glow did not name the star's own
/// scintillation frequency: the middle of D12's integer-stepped [8, 12] Hz.
const GLINT_TWINKLE_DEFAULT_HZ: f32 = 10.0;
const GLINT_LP_HZ: f32 = 8000.0;

// §12 — the meteor.

/// Layer 1, tick: band-passed noise on a 1.5 ms decay, at the origin
/// (§12.2); its life is [`MET_TICK_DUR_S`].
const MET_TICK_HZ0: f32 = 3200.0;
const MET_TICK_HZ1: f32 = 900.0;
const MET_TICK_GLIDE_S: f32 = 0.004;
const MET_TICK_Q: f32 = 0.7;
const MET_TICK_ATTACK_S: f32 = 0.000_3;
const MET_TICK_DECAY_S: f32 = 0.001_5;
/// The tick's life, 12.5 ms: [`super::RING_OUT_PER_TAU`] × its decay (−43 dB),
/// then the engine's release ramp, so the crest at ~0.5 ms renders at the
/// envelope's own height rather than inside the ramp.
const MET_TICK_DUR_S: f32 = super::RING_OUT_PER_TAU * MET_TICK_DECAY_S + super::RELEASE_RAMP_S;
/// The tick's level, fitted so the rendered peak lands at §12.2's −24 dB re the
/// tine step: the envelope crests at 0.58, and a 2 ms Q-0.7 noise burst crests
/// ~8 dB under its input. The crest varies ±3.5 dB by seed.
const MET_TICK_LEVEL: f32 = 0.28;
/// The tick's roof: a roof of 0 (`Voice::default()`) silences the voice, and
/// the whoosh's figure passes the 3200 Hz the tick starts at.
const MET_TICK_ROOF_HZ: f32 = MET_WHOOSH_LP_HZ;

/// Layer 2, core: the layer that carries direction. A sine gliding an octave
/// up (rightward / upward, `Dir::sign` `+1`) or down over the flight, with a
/// light inharmonic FM colour that dies with it. Its octave `[C5, C6)` derives
/// from [`TINE_BASE_HZ`], so a retune moves it with the tine.
const MET_CORE_LO_HZ: f32 = TINE_BASE_HZ;
const MET_CORE_HI_HZ: f32 = 2.0 * TINE_BASE_HZ;
/// Glide τ as a fraction of `T`: 0.45·T reaches 89 % of the octave by landing.
const MET_CORE_GLIDE_MUL: f32 = 0.45;
const MET_CORE_FM_RATIO: f32 = 3.01;
const MET_CORE_FM_INDEX: f32 = 0.35;
const MET_CORE_FM_TAU_MUL: f32 = 0.5;
const MET_CORE_ATTACK_S: f32 = 0.006;
const MET_CORE_DECAY_MUL: f32 = 0.7;
const MET_CORE_DUR_MUL: f32 = 1.9;
const MET_CORE_LP_HZ: f32 = 3800.0;
/// −8 dB re the step.
const MET_CORE_LEVEL: f32 = 0.398_107_2;
/// The armed pre-cue's core level, −20 dB (§15.2): an arm is a guess, so it
/// stays quieter. A claim glides the level to [`MET_CORE_LEVEL`] over the
/// flight rather than stepping it ([`TrailSynth::v2_claim_arm`]).
const MET_ARM_CORE_LEVEL: f32 = 0.1;
/// The (default-off) arm's time to live, seconds (D11).
const MET_ARM_TTL_S: f32 = 0.150;
/// The least life a claimed core keeps past the claim instant, seconds. An arm
/// that has sounded longer than the true flight's `1.9·T` would otherwise be
/// re-targeted to a `dur` already passed: a hard cut (§9.5 law 2). Twice the
/// 12 ms ramp.
const MET_CLAIM_MIN_TAIL_S: f32 = 0.030;

/// Layer 3, whoosh: direction-blind, since the rush of travel is the same
/// either way and direction lives in the core alone.
const MET_WHOOSH_HZ0: f32 = 1600.0;
const MET_WHOOSH_HZ1: f32 = 5200.0;
const MET_WHOOSH_GLIDE_MUL: f32 = 0.65;
/// §9.5 law 6 caps noise Q at 1.6; the whoosh at 1.5 is the ceiling in
/// practice.
const MET_WHOOSH_Q: f32 = 1.5;
const MET_WHOOSH_ATTACK_S: f32 = 0.018;
const MET_WHOOSH_DECAY_MUL: f32 = 0.6;
const MET_WHOOSH_DUR_MUL: f32 = 1.9;
const MET_WHOOSH_LP_HZ: f32 = 7000.0;
/// −6 dB re the step.
const MET_WHOOSH_LEVEL: f32 = 0.501_187_2;

/// Layer 4, bell: the tine step, thrown across the line and landing exactly at
/// `T`. τ 80 / dur 260 (D19), +2 dB: the loudest routine gesture.
const MET_BELL_TAU_S: f32 = 0.080;
const MET_BELL_DUR_S: f32 = 0.260;
const MET_BELL_LEVEL: f32 = 1.258_925_4;

/// Layer 5, thump — the bass tine on the current chord root, at the landing.
const MET_THUMP_ATTACK_S: f32 = 0.008;
const MET_THUMP_DECAY_S: f32 = 0.060;
/// §12.2's 120 ms, raised to the tail law: [`TAIL_DUR_PER_TAU`] × 60.
const MET_THUMP_DUR_S: f32 = TAIL_DUR_PER_TAU * MET_THUMP_DECAY_S;
/// −6 dB re the step.
const MET_THUMP_LEVEL: f32 = 0.501_187_2;

/// Layer 6, rain: five glints falling behind the landing, a descending
/// pentatonic in the stardust lane, one per fan star (1 m1 + 4 m2) in the fan's
/// throw order (D6). `FAN_HERO_N` is the shared count.
const MET_RAIN_HZ: [f32; timing::FAN_HERO_N] = [5232.5, 4709.25, 4186.0, 3488.33, 3139.5];
/// Where the rain starts, relative to the landing.
const MET_RAIN_LEAD_S: f32 = 0.010;
const MET_RAIN_ATTACK_S: f32 = 0.002;
const MET_RAIN_DECAY_S: f32 = 0.028;
/// §12.2's 66 ms, raised to the tail law: [`TAIL_DUR_PER_TAU`] × 28. Five at
/// 18 ms spacing overlap four deep, so the RAIN lane's cap of three keeps
/// "≤ 3 live": the fourth onset fade-steals the first, then at 14 % of peak.
const MET_RAIN_DUR_S: f32 = TAIL_DUR_PER_TAU * MET_RAIN_DECAY_S;
const MET_RAIN_TWINKLE_HZ: f32 = 12.0;
const MET_RAIN_TWINKLE_DEPTH: f32 = 0.5;
const MET_RAIN_LP_HZ: f32 = 8000.0;
/// −18 dB re the step, each.
const MET_RAIN_LEVEL: f32 = 0.125_892_5;
/// The rain's stereo scatter around the destination — small, alternating, and
/// closing in, so five glints read as one shower rather than five events.
const MET_RAIN_PAN: [f32; timing::FAN_HERO_N] = [0.25, -0.20, 0.15, -0.10, 0.05];

/// A new meteor damps the previous core, whoosh and rain over this (§12.3).
/// 15 ms rather than 12 so the swept noise and gliding tone do not click.
const METEOR_INTERRUPT_S: f32 = 0.015;

// §11 — the Enter cadence.

/// The pickup, a G5 at `t = 0` — the one cadence voice that does not wait for
/// the landing (D9).
const CAD_PICKUP_DEG: i32 = 3;
/// −2 dB re the step.
const CAD_PICKUP_LEVEL: f32 = 0.794_328_2;
/// The resolution: C5 when the verse is low, C6 when it is high, so the
/// cadence always resolves *downward onto* home rather than leaping away.
const CAD_RESOLUTION_LOW_DEG: i32 = 0;
const CAD_RESOLUTION_HIGH_DEG: i32 = 5;
/// The verse degree at or below which the resolution takes the low C.
const CAD_RESOLUTION_SPLIT: i8 = 2;
/// A question's return stays open: when the line's last mark is a `?`
/// ([`MelodyV2::pending_mark`], so `is it?⏎` and `is it? ⏎`; `??` is void),
/// the Enter's resolution lands this many degrees over its C: the G, a fifth
/// up, answered on the dominant. Only the resolution's degree (and its room)
/// moves; pickup, bell and tonic dyad are unchanged.
const CAD_QUESTION_LIFT_DEG: i32 = 3;
/// The tonic dyad, C4 + G4 — home, in the bass, −3 dB.
const CAD_DYAD_LEVEL: f32 = 0.707_945_8;
/// The faraway ice bell (§11): sine C6 with a light inharmonic FM colour whose
/// sidebands are deliberately off-lattice (§9.5 law 4 exempts it: that is what
/// "faraway" sounds like). Derived from [`TINE_BASE_HZ`].
const CAD_BELL_HZ: f32 = 2.0 * TINE_BASE_HZ;
const CAD_BELL_FM_RATIO: f32 = 3.01;
const CAD_BELL_FM_INDEX: f32 = 0.5;
const CAD_BELL_FM_TAU_S: f32 = 0.040;
const CAD_BELL_ATTACK_S: f32 = 0.008;
const CAD_BELL_DECAY_S: f32 = 0.160;
/// §11's 420 ms, raised to the tail law: [`TAIL_DUR_PER_TAU`] × 160.
const CAD_BELL_DUR_S: f32 = TAIL_DUR_PER_TAU * CAD_BELL_DECAY_S;
const CAD_BELL_TWINKLE_HZ: f32 = 9.0;
const CAD_BELL_TWINKLE_DEPTH: f32 = 0.3;
const CAD_BELL_LP_HZ: f32 = 3600.0;
/// −14 dB re the step.
const CAD_BELL_LEVEL: f32 = 0.199_526_2;

/// The ice bell at `delay` and its level trim: the bell `v2_enter` rings on a
/// full cadence, shared with the sing-along's outro
/// (`TrailSynth::design_celebration_outro`, RAINBOW-KITTY-V2.md §27). Spawn at
/// `gain × trim`, half a column out.
pub(super) fn cad_bell_voice(delay: f32) -> (Voice, f32) {
    let bell = Voice {
        delay,
        dur: CAD_BELL_DUR_S,
        attack: CAD_BELL_ATTACK_S,
        decay: CAD_BELL_DECAY_S,
        p: [
            Partial {
                lvl: P1_LVL,
                f0: CAD_BELL_HZ,
                fm_ratio: CAD_BELL_FM_RATIO,
                fm_i0: CAD_BELL_FM_INDEX,
                fm_tau: CAD_BELL_FM_TAU_S,
                ..Partial::default()
            },
            Partial::default(),
            Partial::default(),
        ],
        tw_rate: CAD_BELL_TWINKLE_HZ,
        tw_depth: CAD_BELL_TWINKLE_DEPTH,
        lp_cut: CAD_BELL_LP_HZ,
        lane: LANE_CADENCE,
        ..Voice::default()
    };
    (bell, KEY_TINE_TRIM * CAD_BELL_LEVEL)
}

/// The brrrring (D18): the cascade's four notes, their offsets in seconds and
/// their levels re the step. `walk, +2, +3, +5` at 0 / 45 / 90 / 135 ms.
const CASCADE_DEGREES: [i32; 4] = [0, 2, 3, 5];
const CASCADE_DELAYS_S: [f32; 4] = [0.0, 0.045, 0.090, 0.135];
/// 0 / −10.5 / −11.4 / −12.4 dB (§11).
const CASCADE_LEVELS: [f32; 4] = [1.0, 0.298_538_3, 0.269_153_5, 0.239_883_3];
/// A Jump landing inside a live cascade re-strikes the top note only, −12 dB.
const CASCADE_RESTRIKE_DEG: i32 = 5;
const CASCADE_RESTRIKE_LEVEL: f32 = 0.251_188_6;

/// The up-strum (§28): a paste's four tines at 0 / 22 / 44 / 66 ms, the live
/// chord's three lit degrees ascending and the root an octave up (`+5`). Twice
/// the cascade's pace: a strum is one gesture, a cascade a run.
const STRUM_DELAYS_S: [f32; 4] = [0.0, 0.022, 0.044, 0.066];
/// 0 / −2 / −4 / −6 dB re each other: the root leads, the strings above it
/// fall away evenly — a chord voiced, not a run of four notes.
const STRUM_SHAPE: [f32; 4] = [1.0, 0.794_328_2, 0.630_957_3, 0.501_187_2];
/// −3.7 dB on the whole strum: four tines 22 ms apart overlap inside one
/// tine's decay, so this keeps a paste under a lone step's peak
/// (`the_remainder_past_the_cap_is_one_strum_not_a_verse`).
const STRUM_TRIM: f32 = 0.653_130_6;

// The verdict: a command's exit code, sounded once on the `D` that carries it.
// Armed by the keyed Enter and spent by the first `D`; the host never forwards
// a `D` with no armed Enter, so program output never speaks.

/// The plagal fall, as indices into [`CHORD_LOOP`]: IV (F4 + C4) → I (C4 +
/// G4). Both chords are already in the loop, so the verdict adds no new pitch;
/// it adds an order the loop `I vi IV V | I V vi IV` never walks.
const VERDICT_PLAGAL: [usize; 2] = [2, 0];
/// The vi (A4 + E4): the red verdict. A minor already heard all session,
/// arriving alone and out of turn.
const VERDICT_MINOR: usize = 1;
/// The IV→I fall, seconds: longer than the bass decay ([`BASS_DECAY_S`]) so
/// the chords stay two chords, short enough to be one "amen".
const VERDICT_PLAGAL_S: f32 = 0.26;
/// Where a red verdict parks the chord loop, so the next word boundary advances
/// onto [`VERDICT_MINOR`] and the next word sings against the minor. A green
/// verdict parks at [`CHORD_AFTER_ENTER`], as a keyed Enter does.
const CHORD_AFTER_RED: u8 = 0;
/// The green dyads' level re the step: −6 dB, the loudest a once-per-command
/// gesture may be
/// (`the_verdict_is_the_music_boxs_alone_and_never_louder_than_a_key`).
/// Independent of the word downbeat's [`BASS_LEVEL`].
const VERDICT_DYAD_LEVEL: f32 = 0.501_187_2;
/// The gesture's voice trim: −1 dB on every verdict voice. A bass dyad with a
/// tine over it sums to a peak above any one level; untrimmed, a green cadence
/// lands over its ceiling.
const VERDICT_PEAK_TRIM: f32 = 0.891_250_9;
/// The red dyad, −9 dB: three under the green. A failure is told, not
/// announced.
const VERDICT_MINOR_LEVEL: f32 = 0.354_813_4;
/// The one tine over the resolution: E5, degree 2, −8 dB re the step. The
/// major seventh of the departing F and the third of the arriving C, so the
/// pair resolves rather than stops.
const VERDICT_TINE_DEG: i32 = 2;
const VERDICT_TINE_LEVEL: f32 = 0.398_107_2;
/// The verdict's hot key (sense 3, the tine's half): a typed key this soon
/// after a green long verdict is lit whatever the chord says; the audible twin
/// of [`super::super::rainbow_kitty::spine::VERDICT_WINDOW_S`], in ms.
const VERDICT_LIT_MS: u32 = 5_000;

// §10.1 — MelodyV2: the state.

/// Which touch a key gets (§9.2). Both voice the derived degree: nothing here
/// can voice a degree the line did not derive.
///
/// [`Touch::ReStrike`] is reached only through [`stride_mag`]'s single zero, a
/// doubled letter. [`MelodyV2::derive`] closes every other way back onto the
/// sounding degree (fold to zero, gravity cancelling a second, reflection), so
/// the same-pitch damp guards against a comb filter rather than punishing a
/// fast typist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Touch {
    /// The line moved. Full tine: body, octave, strike, mallet.
    Step,
    /// The line derived the pitch it is already on. The tine with the strike
    /// partial at zero, at `L_n` — a music-box tremolo, never a leap.
    ReStrike,
}

/// One frame of melody state: everything a keystroke of text decides, nothing
/// the hand decides (clock, tempo, autorepeat). [`MelodyV2::text`] is the live
/// frame, saved before a keystroke mutates it so a Backspace can restore it
/// (§10.4). A Backspace un-sings: "type five, delete five, type five again"
/// plays five notes, not ten.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Undo {
    /// The sounding degree, 0..8 (C5..G6). Every pitched thing that is not a
    /// bass root is relative to it; only [`MelodyV2::derive`] moves it on a
    /// keystroke.
    walk: i8,
    /// Keys since the line last moved, 1-based inside [`RESTRIKE_L0`]'s ladder
    /// (a doubled letter's tremolo).
    restrike: u8,
    /// Position in [`CHORD_LOOP`], advanced by Space run heads only.
    chord: u8,
    /// Letters since the last word boundary. Read by the `?`/`!` grafts and
    /// carried on the undo stack so a correction restores the word too.
    word_pos: u8,
    /// The previous key's alphabet rank ([`EventMeta::rank`]), the interval's
    /// left operand; `0` means nothing to measure against yet. A cue with no
    /// rank never clobbers it, so an echo-born cue cannot make the next real
    /// letter leap.
    prev_rank: u8,
    /// The stride the line is repeating, and how many times running —
    /// [`MELODY_RUN_MAX`]'s whole state.
    run_stride: i8,
    run_len: u8,
    /// The line's subject: the first [`MOTIF_LEN`] intervals since the last
    /// re-latch ([`MelodyV2::relatch_motif`]), and how many have been written.
    motif: [i8; MOTIF_LEN],
    motif_len: u8,
    /// How many of the subject's intervals are still to be answered, and
    /// where in it the answer has reached.
    motif_play: u8,
    motif_k: u8,
    /// Word heads since the subject was last answered ([`MOTIF_EVERY_WORDS`]).
    words_since_motif: u8,
    /// Whether the last typed key was shifted: [`CAPITAL_LIFT_DEG`] lifts a
    /// capital but not a run of them, and [`Forte::of`] tells the capital
    /// opening a run from those inside it. On the frame so "type `aB`, delete
    /// the `B`, type it again" is the same capital.
    prev_shifted: bool,
    /// The pending mark: what the last typed key said about the phrase,
    /// [`MARK_NONE`] or one of [`MARK_PERIOD`] … [`MARK_COLON`]. Every typed key
    /// writes it, so `foo.bar`, `3.14` and `v0.88.0` never reach the harmony:
    /// only a mark still last when the Space arrives becomes a chord
    /// ([`MelodyV2::on_space`]). Space does not clear it (the next word head
    /// reads it); Enter, a line feed and a kill do. A doubled mark sets
    /// [`MARK_DOUBLED`], and [`MelodyV2::pending_mark`] then reads 0.
    last_mark: u8,
    /// Whether this space-delimited token has spent its one steering mark, so
    /// `self.v2.walk.foo` cadences at most once (`word_pos` counts marks too,
    /// so it cannot say this).
    token_marked: bool,
    /// The degree an opening bracket sounded, −1 for none — where the closing
    /// one returns toward. Depth 1: a second `(` overwrites it.
    paren_deg: i8,
    /// The degree the last `.` or `!` cadenced on, −1 for none — the
    /// anti-drone's memory ([`cadence_target`]).
    last_cadence_deg: i8,
    /// The aside ([`ASIDE_GAIN`]): open brackets, capped at
    /// [`ASIDE_DEPTH_MAX`], and typed keys sounded inside it
    /// ([`ASIDE_MAX_KEYS`]). Sotto voce at depth ≥ 1; closed by a close that
    /// brings depth to 0, an Enter, a line feed, a kill, a phrase rest or the
    /// key cap.
    aside_depth: u8,
    aside_keys: u8,
    /// The quotation ([`QUOTE_OPENS`]): the degree the open quote sounded, the
    /// target the closing quote steers toward; −1 when none is open. Cleared
    /// with the phrase ([`MelodyV2::end_phrase`]).
    quote_deg: i8,
    /// Whether the last typed key was an open (`( [ {`): a quote there opens a
    /// quotation as at a word head (`("hi")`).
    after_open: bool,
}

impl Undo {
    /// A fresh frame: the lattice tonic (so the first key opens on home), the
    /// loop on its first chord, no history, subject, mark, bracket or quotation.
    const HOME: Self = Self {
        walk: 0,
        restrike: 0,
        chord: 0,
        word_pos: 0,
        prev_rank: 0,
        run_stride: 0,
        run_len: 0,
        motif: [0; MOTIF_LEN],
        motif_len: 0,
        motif_play: 0,
        motif_k: 0,
        words_since_motif: 0,
        prev_shifted: false,
        last_mark: MARK_NONE,
        token_marked: false,
        paren_deg: -1,
        last_cadence_deg: -1,
        aside_depth: 0,
        aside_keys: 0,
        quote_deg: -1,
        after_open: false,
    };
}

/// The melody engine (§10): `Copy`, alloc-free, deterministic, driven entirely
/// by `EventMeta::at_ms` from the host's input clock, never by audio-thread
/// arrival gaps, so the keyed and frame paths agree on phrase boundaries
/// (§10.1). The same events, seed and stamps produce the same notes (A27).
#[derive(Clone, Copy, Debug)]
pub struct MelodyV2 {
    /// The text's state, the live [`Undo`] frame (`push_undo` saves it,
    /// `pop_undo` restores it).
    text: Undo,
    /// Keystrokes that have moved the melody: one per typed key, without
    /// exception (test / introspection hook).
    steps: u32,
    /// `at_ms` of the last admitted key (any touch), the IOI's clock; `None`
    /// until the first, so the first keystroke does not see an enormous gap
    /// and cadence an unplayed phrase.
    last_key_ms: Option<u32>,
    /// The autorepeat detector's state: the previous raw gap and how many gaps
    /// of the machine-regular run have agreed. Not on the undo frame: it
    /// describes the hand, and a Backspace does not un-hold a key.
    prev_gap: u32,
    repeat_run: u8,
    /// Inter-onset interval, EMA'd, clamped 30..600 ms.
    ioi_ms: f32,
    /// True while inside a whitespace RUN: only its head is a downbeat.
    space_run: bool,
    /// Indent steps the run has sounded (0 at the head; saturating): the index
    /// into the rising figure ([`indent_step_hz`]), so the figure climbs one lit
    /// degree per sounded step.
    space_steps: u32,
    /// When the run's head or its last admitted indent step sounded (ms on
    /// the cue clock): the step's own gate, [`SPACE_STEP_MIN_GAP_MS`].
    last_step_ms: u32,
    /// The timbre ladder's stops: which of §3.3's additions this synth voices.
    /// All on in production; the bench toggles them one at a time.
    stops: TimbreStops,
    /// Flow heat for the cue being voiced (§22), 0..1, latched from
    /// [`EventMeta::flow`] at the top of [`TrailSynth::push_v2`]. A property of
    /// the instrument at this instant, not melody state: nothing derives from
    /// it, no undo frame carries it, and render never reads it.
    flow: f32,
    /// Position in [`GLINT_DEGREES`].
    glint_k: u8,
    /// The last line feed of the live cascade run (D18; [`MelodyV2::on_jump`]
    /// says why the last Jump, not the last cascade). `None` until the first,
    /// so a host clock starting at 0 does not swallow the first brrrring inside
    /// [`CASCADE_EXCLUSIVE_MS`] of zero.
    cascade_at: Option<u32>,
    /// The last top-note re-strike inside a live cascade.
    cascade_restrike_ms: u32,
    /// The run's playhead in [`SONG_THEME`]. The second and later line feeds of
    /// a run take their cascade's base from the authored theme, one phrase edge
    /// per line (a phrase's first note, then its last, then the next phrase's
    /// first), so a stream sings `0 7 5 0 2 2 0 8` and round again; the typed
    /// line stays derived. Re-headed by every lone line feed.
    run_theme_pos: u8,
    /// Which phrase of [`SONG_FORM`] `run_theme_pos` is inside.
    run_phrase: u8,
    /// Keys since the last keyed Enter — [`ENTER_PICKUP_MIN_KEYS`] decides
    /// whether a Return is a cadence or a bare tonic dyad.
    keys_since_enter: u8,
    /// `at_ms` of the last keyed Enter, `None` before the first — the clock
    /// [`ENTER_ECHO_SWALLOW_MS`] is measured on.
    last_enter_ms: Option<u32>,
    /// The undo stack (§10.1), newest last.
    undo: [Undo; UNDO_N],
    undo_len: u8,
    /// The newest tune voice `(slot, born)`, so a Backspace's 40 ms mute and a
    /// re-strike's 12 ms damp address that voice, not a later slot occupant.
    lead: Option<(u8, u32)>,
    /// The live bass voice, same addressing (the downbeat is monophonic).
    bass: Option<(u8, u32)>,
    /// The live ting ([`TING_LEVEL`]), same addressing: it rings through the
    /// next keyed cue, and a second Shift replaces it. Not on [`Undo`]: it
    /// addresses a voice, and un-singing a key does not un-press the modifier.
    lift: Option<(u8, u32)>,
    /// The current key's ring (the shifted key's twelfth, [`CAP_RING_LIFT_DEG`],
    /// [`CAP_RING_LEVEL`]), same addressing: a Backspace retires it (unheard if
    /// not yet opened, else a 40 ms ramp). Not on [`Undo`], as `lead`.
    ring: Option<(u8, u32)>,
    /// The current key's graft (`?`'s rise, the bracket's tink, the operator's
    /// fifth), same addressing and Backspace rule as `ring`. Both addresses are
    /// cleared by the next text key or line boundary, so deleting that key
    /// cannot retire an older one. Not on [`Undo`].
    graft: Option<(u8, u32)>,
    /// The previous typed key's glyph class, read by one gate: a close right
    /// after an open keeps its tink although it re-strikes the open's degree
    /// (`(` and `)` share a rank, [`TINK_DEG`]). Reset to `LETTER` wherever the
    /// decoration addresses are forgotten (erase, kill, space, paste, Enter).
    /// Not on [`Undo`].
    prev_class: u8,
    /// The verdict's open door (sense 3): `at_ms` of a green long verdict, spent
    /// by the next typed key ([`MelodyV2::take_verdict_lit`]). Not on [`Undo`]:
    /// un-singing that key does not un-finish the build.
    verdict_at_ms: Option<u32>,
    /// The meteor's scheduled bell and thump. A new meteor cancels them while
    /// unsounded and leaves them alone once they have spoken (§12.3).
    bell: Option<(u8, u32)>,
    thump: Option<(u8, u32)>,
}

impl Default for MelodyV2 {
    fn default() -> Self {
        Self::new()
    }
}

impl MelodyV2 {
    /// A fresh melody: home, the loop on I, no history and no subject.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            text: Undo {
                // Parked where a keyed Enter parks it, so the session's first
                // word boundary lands on I (§10.3).
                chord: CHORD_AFTER_ENTER,
                ..Undo::HOME
            },
            steps: 0,
            last_key_ms: None,
            prev_gap: 0,
            repeat_run: 0,
            ioi_ms: IOI_DEFAULT_MS,
            space_run: false,
            space_steps: 0,
            last_step_ms: 0,
            stops: TimbreStops::ALL,
            flow: 0.0,
            glint_k: 0,
            cascade_at: None,
            cascade_restrike_ms: 0,
            run_theme_pos: 0,
            run_phrase: 0,
            keys_since_enter: 0,
            last_enter_ms: None,
            undo: [Undo::HOME; UNDO_N],
            undo_len: 0,
            lead: None,
            bass: None,
            lift: None,
            ring: None,
            graft: None,
            prev_class: LETTER,
            verdict_at_ms: None,
            bell: None,
            thump: None,
        }
    }

    /// The sounding verse degree (test / introspection hook).
    #[must_use]
    #[cfg(test)]
    pub fn walk(&self) -> i8 {
        self.text.walk
    }

    /// Keystrokes that have moved the melody (test / introspection hook).
    /// Incremented once per typed key, unconditionally, where a key enters the
    /// melody: a census that pushes `n` typed keys and reads anything else has
    /// caught a gate.
    #[must_use]
    #[cfg(test)]
    pub fn steps(&self) -> u32 {
        self.steps
    }

    /// The live chord's index into [`CHORD_LOOP`] (test / introspection hook).
    #[must_use]
    #[cfg(test)]
    pub fn chord(&self) -> u8 {
        self.text.chord
    }

    /// Letters since the last word boundary (test / introspection hook).
    #[must_use]
    #[cfg(test)]
    pub fn word_pos(&self) -> u8 {
        self.text.word_pos
    }

    /// Whether the line is answering its own subject: the next key replays a
    /// latched interval rather than deriving its own (test / introspection
    /// hook). The bench's repeat ledger uses it to tell an answered unison
    /// (§3.1 asks for it) from a stalled repeat (§3.1 forbids it).
    #[must_use]
    #[cfg(test)]
    pub fn motif_answering(&self) -> bool {
        self.text.motif_play > 0
    }

    /// The smoothed inter-onset interval in ms (test / introspection hook).
    #[must_use]
    #[cfg(test)]
    pub fn ioi_ms(&self) -> f32 {
        self.ioi_ms
    }

    /// Whether time `at` is at a structural boundary of the line: a word head
    /// (nothing typed since the last Space, Enter or line feed) or a rest
    /// (nothing typed for [`PHRASE_PAUSE_MS`]). The handback needs both: a
    /// stream with no spaces (a base64 paste, a password field) never reaches a
    /// word head.
    fn at_boundary(&self, at: u32) -> bool {
        self.text.word_pos == 0
            || self
                .last_key_ms
                .is_some_and(|k| timing::phrase_rest_reached(at.saturating_sub(k), self.ioi_ms))
    }

    /// The state's undo frame ([`Undo`]): `push_undo` saves exactly this and
    /// `pop_undo` restores it, so "type a key, delete it" leaves `frame()`
    /// unchanged (`backspace_unsings_a_phrase`).
    fn frame(&self) -> Undo {
        self.text
    }

    fn push_undo(&mut self) {
        let frame = self.frame();
        if usize::from(self.undo_len) == UNDO_N {
            // Drop the oldest frame: a correction can walk back a word, and a
            // word is what the stack is sized for.
            self.undo.rotate_left(1);
            self.undo[UNDO_N - 1] = frame;
        } else {
            self.undo[usize::from(self.undo_len)] = frame;
            self.undo_len += 1;
        }
    }

    fn pop_undo(&mut self) {
        let Some(n) = self.undo_len.checked_sub(1) else {
            return;
        };
        self.text = self.undo[usize::from(n)];
        self.undo_len = n;
    }

    /// Is `deg` a chord tone of the live chord (§10.3)?
    fn deg_is_lit(&self, deg: i32) -> bool {
        let pc = deg.rem_euclid(5) as u8;
        CHORD_LOOP[usize::from(self.text.chord)].lit & (1 << pc) != 0
    }

    /// Is the sounding degree a chord tone of the live chord (§10.3)?
    fn lit(&self) -> bool {
        self.deg_is_lit(i32::from(self.text.walk))
    }

    /// The snap (§3.1 step 6, and the rest's resolution): the degree lit by the
    /// live chord nearest `deg`, searched outward to [`WORD_HEAD_SNAP_DEG`];
    /// ties go to the side continuing the contour from `from`. The ±2 window
    /// spans all five pitch classes, every chord lights three, and the register
    /// bounds cut at most two candidates off one side, so the `deg` fallback is
    /// unreachable; it stays so a chord-table change fails a test rather than
    /// playing an unlit note.
    fn nearest_lit_within(&self, deg: i32, from: i32) -> i32 {
        let deg = deg.clamp(TUNE_DEG_LO, TUNE_DEG_HI);
        if self.deg_is_lit(deg) {
            return deg;
        }
        let dir = if deg >= from { 1 } else { -1 };
        for k in 1..=WORD_HEAD_SNAP_DEG {
            for cand in [deg + dir * k, deg - dir * k] {
                if (TUNE_DEG_LO..=TUNE_DEG_HI).contains(&cand) && self.deg_is_lit(cand) {
                    return cand;
                }
            }
        }
        deg
    }

    /// Re-latches the subject (§3.1): the next [`MOTIF_LEN`] intervals become the
    /// line's new motif. Run by a rest and by the Space behind `.` `!` `?`, where
    /// a phrase ends; an Enter keeps the subject.
    fn relatch_motif(&mut self) {
        self.text.motif_len = 0;
        self.text.motif_play = 0;
        self.text.motif_k = 0;
        self.text.words_since_motif = 0;
    }

    /// The line boundary's half of the phrase state: an Enter, a line feed and
    /// a kill clear the pending mark, the token's steering budget and the open
    /// bracket. `last_cadence_deg` survives: the anti-drone spans consecutive
    /// sentences, including across one-sentence lines.
    fn end_phrase(&mut self) {
        self.text.last_mark = MARK_NONE;
        self.text.token_marked = false;
        self.text.paren_deg = -1;
        self.close_aside();
        self.text.quote_deg = -1;
        self.text.after_open = false;
    }

    /// Ends the aside ([`ASIDE_GAIN`]), every open bracket at once: on a line
    /// boundary, a phrase rest, or the [`ASIDE_MAX_KEYS`]-th key inside it.
    fn close_aside(&mut self) {
        self.text.aside_depth = 0;
        self.text.aside_keys = 0;
    }

    /// Whether an aside is open (test / introspection hook): between an opening
    /// bracket and whatever closes it ([`ASIDE_GAIN`]).
    #[must_use]
    #[cfg(test)]
    pub fn in_aside(&self) -> bool {
        self.text.aside_depth > 0
    }

    /// Whether a quotation is open (test / introspection hook; [`QUOTE_OPENS`]).
    #[must_use]
    #[cfg(test)]
    pub fn in_quote(&self) -> bool {
        self.text.quote_deg >= 0
    }

    /// The mark the next Space will confirm (test / introspection hook): 0
    /// none, 1 `.`, 2 `!`, 3 `?`, 4 `,`, 5 `;`, 6 `:`; 0 for a doubled mark
    /// ([`MARK_DOUBLED`]).
    #[must_use]
    pub fn pending_mark(&self) -> u8 {
        if self.text.last_mark & MARK_DOUBLED != 0 {
            MARK_NONE
        } else {
            self.text.last_mark
        }
    }

    /// Whether the live token has spent its steering mark (test / introspection
    /// hook). Read straight after a key, it says whether that key steered.
    #[must_use]
    #[cfg(test)]
    pub fn token_marked(&self) -> bool {
        self.text.token_marked
    }
}

/// The widest interval within a word, in lattice degrees: four, a major sixth
/// (5/3), since an octave-class leap inside a word is what A2 forbids.
/// [`MelodyV2::derive`] clamps its stride to this; the word-head snap may move
/// a further two degrees, a head's interval being between words.
const WORD_LEAP_MAX_DEG: i32 = 4;

/// What one admitted `Typed` asks the synth to spawn — the melody's whole
/// output for a keystroke, resolved before a single voice is built.
#[derive(Clone, Copy, Debug)]
struct TypedPlan {
    touch: Touch,
    /// The verse degree, before the borrowed `song_key`.
    deg: i32,
    lit: bool,
    /// Level re the step, before the loudness arc's `g_IOI`.
    level: f32,
    /// The felt touch: the tine's partials go to zero and the mallet alone
    /// speaks. Used for a live sing-along's re-strike (§10.2, so a held key
    /// cannot machine-gun under the riff) and for autorepeat
    /// ([`AUTOREPEAT_JITTER_MS`]). Never silence: a held key is heard as felt
    /// rather than pitched.
    mallet_only: bool,
    /// The same pitch again, whatever the touch: a doubled letter's re-strike
    /// or a word head's common tone. §9.5 law 5 damps the previous voice on
    /// this: two independently phased sines at one frequency comb.
    repeat: bool,
    /// This key is the first letter of a word.
    word_head: bool,
    /// This key ended a phrase rest ([`PHRASE_PAUSE_MS`]) and resolved the
    /// line before sounding — the room answers it (§3.3's air cloud).
    rest: bool,
    /// This word head opened the line's answer to its own subject — the
    /// answering voice sounds behind it (§3.3).
    answer_head: bool,
    /// A shifted key that opens a word or a run of shifted keys:
    /// `shifted && (word_head || !prev_shifted)`, where the walk's
    /// [`CAPITAL_LIFT_DEG`] accent fires, the hammer falls hardest, and the
    /// only place a capital rings ([`Forte::of`]).
    opens_shift: bool,
    /// This key is the token's one steering mark, landed on a cadence degree
    /// ([`cadence_target`]). Tells the sentence's full stop (a tine that rings
    /// and breathes) from the dot inside `foo.bar` (a pluck). False on every
    /// letter and digit.
    steered: bool,
    /// This key is inside an aside ([`ASIDE_GAIN`]): one was open before it and
    /// is still open after it, so neither the outermost opening bracket nor
    /// the close that ends it. Changes level and roof, never the degree.
    sotto: bool,
    /// What a quote did: [`QUOTE_OPENS`], [`QUOTE_CLOSES`], or [`QUOTE_NONE`]
    /// (every other key, and the apostrophe).
    quote: u8,
}

/// Which of §3.3's additions this synth voices: the timbre ladder's stops
/// (`plain` / `bloom` / `hue` / `room`). Production is [`TimbreStops::ALL`]; the
/// bench renders the rungs. A stop that is out spawns nothing and reads no hue,
/// so `plain` is the bare tine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimbreStops {
    /// The 3f/4f/6f bloom behind every lit key.
    pub bloom: bool,
    /// `hue_air` on the bloom's level and pan, `ROOF_HUE_ADD_HZ` on the note.
    pub hue: bool,
    /// The two-tap air cloud on Enter and phrase rests, and the answering
    /// voice at motif word heads.
    pub room: bool,
}

impl TimbreStops {
    /// Everything: the instrument that ships.
    pub const ALL: Self = Self {
        bloom: true,
        hue: true,
        room: true,
    };
    /// Nothing: the derived line through the shipped tine — the ladder's
    /// control.
    pub const PLAIN: Self = Self {
        bloom: false,
        hue: false,
        room: false,
    };
}

impl Default for TimbreStops {
    fn default() -> Self {
        Self::ALL
    }
}

impl MelodyV2 {
    /// Records a command's verdict. `green_long` opens the hot key's door; the
    /// chord is parked so the next word boundary lands on I after a green one
    /// ([`CHORD_AFTER_ENTER`]) or on vi after a red one ([`CHORD_AFTER_RED`]).
    /// The playhead is untouched: a finishing command is not a keystroke.
    fn note_verdict(&mut self, at: u32, failed: bool, green_long: bool) {
        self.text.chord = if failed {
            CHORD_AFTER_RED
        } else {
            CHORD_AFTER_ENTER
        };
        self.verdict_at_ms = green_long.then_some(at);
    }

    /// Take the hot key's door if this key is inside [`VERDICT_LIT_MS`] of
    /// it. Spent either way — one key, and the next one prices itself.
    fn take_verdict_lit(&mut self, at: u32) -> bool {
        let Some(mark) = self.verdict_at_ms.take() else {
            return false;
        };
        at.saturating_sub(mark) <= VERDICT_LIT_MS
    }

    /// On typed (§3.1): advances the state and returns what to play, spawning
    /// nothing itself. One keystroke is one melody step: no branch leaves
    /// the verse degree (`walk`) underived or `steps` unincremented, at any rate,
    /// for any glyph; smoothing comes from note choice in [`Self::derive`].
    fn on_typed(&mut self, at: u32, rank: u8, sing: bool, shifted: bool, class: u8) -> TypedPlan {
        let gap = at.saturating_sub(self.last_key_ms.unwrap_or(0));
        let first = self.last_key_ms.is_none();
        self.push_undo();
        // What the phrase was told before this key, read before this key
        // overwrites it: the word head behind a full stop and the doubled-mark
        // test need it.
        let said = self.text.last_mark;
        let rank_before = self.text.prev_rank;

        // Machine regularity, read on the raw gaps before the EMA smooths
        // them. It decides the touch, never the step.
        let held = self.detect_autorepeat(gap, rank, first);

        // A rest resolves the line (walk onto the live chord, contour bias
        // dropped, subject re-latched); then this key derives its own note as
        // any other key would.
        let rest = !first && timing::phrase_rest_reached(gap, self.ioi_ms);
        if rest {
            let here = i32::from(self.text.walk);
            self.text.walk = self.nearest_lit_within(here, here) as i8;
            self.text.run_stride = 0;
            self.text.run_len = 0;
            self.relatch_motif();
            // A rest ends the aside: the key breaking the silence is spoken
            // aloud again.
            self.close_aside();
        }

        // The word head, decided before `word_pos` moves: nothing typed since
        // the last Space, Enter or line feed.
        let word_head = self.text.word_pos == 0;
        let mut answer_head = false;
        if word_head {
            self.text.words_since_motif = self.text.words_since_motif.saturating_add(1);
            // The answer (§3.1): a latched subject is answered at every
            // MOTIF_EVERY_WORDS-th word head. The head snaps to a chord tone as
            // every head does, and the following keys replay the subject's
            // intervals from there: a transposition.
            if usize::from(self.text.motif_len) == MOTIF_LEN
                && self.text.words_since_motif >= MOTIF_EVERY_WORDS
            {
                self.text.motif_play = MOTIF_LEN as u8;
                self.text.motif_k = 0;
                self.text.words_since_motif = 0;
                answer_head = true;
            }
        }

        let from = i32::from(self.text.walk);
        let mut deg = self.derive(rank, gap, first, word_head);
        if word_head {
            // Word heads land lit (§3.1 step 6). Interiors may pass, sounding
            // at PASSING_LEVEL.
            deg = self.nearest_lit_within(deg, from);
            // The snap may not complete a run: `derive` already inverted a
            // fourth identical stride, and a two-degree snap could restore it.
            // Then the head takes the nearest chord tone on the other side of
            // the line (three of five classes are lit, so one is within three
            // degrees unless the register bound cuts it off, in which case the
            // snap stands).
            let sounded = deg - from;
            if sounded != 0
                && self.text.run_len >= MELODY_RUN_MAX
                && i32::from(self.text.run_stride) == sounded
            {
                let away = -sounded.signum();
                if let Some(alt) = (1..=WORD_HEAD_SNAP_DEG + 1)
                    .map(|k| from + away * k)
                    .find(|c| (TUNE_DEG_LO..=TUNE_DEG_HI).contains(c) && self.deg_is_lit(*c))
                {
                    deg = alt;
                }
            }
            // A sentence does not start on the note the last one ended on:
            // behind `. ` and `! ` the harmony is on I and the stop put the
            // line on a C or G, which I lights, so a head landing there takes
            // the nearest other lit degree within three, ties upward. A
            // backstop for a head on the stop's own rank under a doubled mark
            // (`the end. ...`).
            if matches!(said, MARK_PERIOD | MARK_BANG)
                && deg == from
                && let Some(alt) = (1..=STEER_MAX_DEG)
                    .flat_map(|k| [from + k, from - k])
                    .find(|c| (TUNE_DEG_LO..=TUNE_DEG_HI).contains(c) && self.deg_is_lit(*c))
            {
                deg = alt;
            }
        }
        // A capital is the same step lifted ([`CAPITAL_LIFT_DEG`]), before the
        // run is booked and the walk moves, so the line continues from the note
        // the ear got; inside a word it is held to A2's bound. It reflects at
        // the ceiling ([`reflect_deg`]) rather than clamping, so a run of
        // capitals keeps a contour instead of stacking on one pitch. Only a
        // capital that opens something lifts (the first shifted key after an
        // unshifted one, or a shifted word head, even one the snap landed home),
        // so shouting is one accent per word. A lift landing back on `from`
        // takes one further degree, else the capital would sound like its
        // lowercase twin; a doubled capital stays a doubled letter.
        let opens = word_head || !self.text.prev_shifted;
        // The same expression selects the hammer: a capital that opens
        // something is struck hardest and rings ([`Forte::of`]).
        let opens_shift = shifted && opens;
        if shifted && opens && !first && (deg != from || word_head) {
            let raw = deg + CAPITAL_LIFT_DEG;
            let mut lifted = reflect_deg(raw);
            if lifted == from {
                lifted = reflect_deg(raw + 1);
            }
            let lifted = if word_head {
                lifted
            } else {
                lifted.min(from + WORD_LEAP_MAX_DEG)
            };
            if lifted != from {
                deg = lifted;
            }
        }
        // The mark phrases the line after the derivation, snap and lift (so it
        // has the last word on the note) and before the run is booked (so the
        // guard counts the sounded stride):
        // - a doubled mark (`..`, `::`, `??`) steers nothing and voids the
        //   record ([`MARK_DOUBLED`]);
        // - `-` ties: it sounds the note it came from;
        // - `)` returns toward its `(`'s degree by at most
        //   [`WORD_LEAP_MAX_DEG`] and spends the record, not the token's
        //   steering budget;
        // - a steering mark (one per space-delimited token, never a `.` right
        //   behind a digit) lands on its cadence degree ([`cadence_target`]).
        let mark = phrase_kind(class);
        let doubled = mark != MARK_NONE && said & !MARK_DOUBLED == mark;
        let decimal =
            class == STOP && (DIGIT_RANK0..DIGIT_RANK0 + 10).contains(&i32::from(rank_before));
        let mut steered = false;
        // The quotation ([`QUOTE_OPENS`]), read before this key overwrites
        // `after_open`: a quote right after an open heads what the bracket
        // opened.
        let quote_head = word_head || self.text.after_open;
        let mut quote = QUOTE_NONE;
        if doubled {
            // No override.
        } else if class == DASH {
            if !first {
                deg = from;
            }
        } else if class == CLOSE {
            if self.text.paren_deg >= 0 && !first {
                let to = i32::from(self.text.paren_deg);
                deg = from + (to - from).clamp(-WORD_LEAP_MAX_DEG, WORD_LEAP_MAX_DEG);
            }
            self.text.paren_deg = -1;
        } else if class == QUOTE && self.text.quote_deg >= 0 {
            // A quotation closes, toward the degree it opened on — `)`'s
            // return, bounded the same way.
            let to = i32::from(self.text.quote_deg);
            deg = from + (to - from).clamp(-WORD_LEAP_MAX_DEG, WORD_LEAP_MAX_DEG);
            self.text.quote_deg = -1;
            quote = QUOTE_CLOSES;
        } else if class == QUOTE && quote_head {
            // A quotation opens: up to the nearest chord tone at or above the
            // derived degree, held to A2's in-word bound when not at a word
            // head (`("`), standing when no tone is in reach.
            let top = if word_head {
                TUNE_DEG_HI
            } else {
                (from + WORD_LEAP_MAX_DEG).min(TUNE_DEG_HI)
            };
            if let Some(up) = (deg..=top).find(|c| self.deg_is_lit(*c)) {
                deg = up;
            }
            self.text.quote_deg = deg as i8;
            quote = QUOTE_OPENS;
        } else if mark != MARK_NONE && !self.text.token_marked && !decimal {
            deg = cadence_target(mark, from, i32::from(self.text.last_cadence_deg));
            if matches!(mark, MARK_PERIOD | MARK_BANG) {
                self.text.last_cadence_deg = deg as i8;
            }
            steered = true;
            self.text.token_marked = true;
        }
        if class == OPEN {
            self.text.paren_deg = deg as i8;
        }
        // A closing quote carries the mark before it: `"Why?"` still asks, so
        // the next Space confirms it and the next Return reads it
        // ([`CAD_QUESTION_LIFT_DEG`]). Every other key writes its own.
        self.text.last_mark = if quote == QUOTE_CLOSES {
            self.text.last_mark
        } else if doubled {
            mark | MARK_DOUBLED
        } else {
            mark
        };
        // The aside ([`ASIDE_GAIN`]): a level, not a note (`deg` is final). An
        // open deepens it (to [`ASIDE_DEPTH_MAX`]), a close shallows it, and
        // the key is sotto voce when the aside is open on both sides of it.
        // The [`ASIDE_MAX_KEYS`]-th key inside ends it.
        let aside_before = self.text.aside_depth;
        if class == OPEN {
            self.text.aside_depth = (self.text.aside_depth + 1).min(ASIDE_DEPTH_MAX);
        } else if class == CLOSE {
            self.text.aside_depth = self.text.aside_depth.saturating_sub(1);
        }
        let sotto = aside_before > 0 && self.text.aside_depth > 0;
        self.text.after_open = class == OPEN;
        if self.text.aside_depth == 0 {
            self.text.aside_keys = 0;
        } else if sotto {
            self.text.aside_keys = self.text.aside_keys.saturating_add(1);
            if self.text.aside_keys >= ASIDE_MAX_KEYS {
                self.close_aside();
            }
        }
        // The run is booked on the sounded stride (after reflection, snap,
        // lift and mark): the only one [`MELODY_RUN_MAX`] can guarantee.
        self.note_run(deg - from);
        // The line writes its subject from its first intervals (§3.1), each
        // held to A2's in-word bound; not while answering it, nor on the
        // session's first key. A zero (a doubled letter) would replay as a
        // unison at every answer, and a third identical stride would make the
        // subject a ramp holding [`MELODY_RUN_MAX`]'s budget, so both are
        // refused and the line waits for the next real interval.
        if !first && usize::from(self.text.motif_len) < MOTIF_LEN && self.text.motif_play == 0 {
            let iv = (deg - from).clamp(-WORD_LEAP_MAX_DEG, WORD_LEAP_MAX_DEG) as i8;
            let ramp = usize::from(self.text.motif_len) == MOTIF_LEN - 1
                && self.text.motif[..usize::from(self.text.motif_len)]
                    .iter()
                    .all(|&m| m == iv);
            if iv != 0 && !ramp {
                self.text.motif[usize::from(self.text.motif_len)] = iv;
                self.text.motif_len += 1;
            }
        }
        self.text.walk = deg as i8;
        self.text.prev_shifted = shifted;
        self.steps = self.steps.saturating_add(1);

        // A re-strike is a repeated pitch inside a word: a doubled letter. Not
        // the session's first key, a word head's common tone, a steered mark
        // arriving where the line stands, or a quotation's open or close. The
        // repeated pitch still damps the previous voice (`repeat`).
        let repeat = deg == from && !first;
        let touch = if repeat && !word_head && !steered && quote == QUOTE_NONE {
            self.text.restrike = self.text.restrike.saturating_add(1);
            Touch::ReStrike
        } else {
            self.text.restrike = 0;
            Touch::Step
        };

        // The IOI, updated after `derive` has read it, so the contour asks
        // whether this key was early or late against the tempo as it stood
        // ([`timing::ioi_next`] holds a rest out of the estimate).
        self.ioi_ms = timing::ioi_next(self.ioi_ms, gap, first, rest);

        // The verdict's hot key (sense 3): the first key after a green long
        // command is lit whatever the chord says (full tine, 0 dB,
        // hero-eligible). The pitch is untouched. Spent by that one key.
        let lit = self.lit() || self.take_verdict_lit(at);
        // The step's level: 0 dB lit, −2 dB passing (§9.2); a re-strike is that
        // note again on the `L_n` ladder ([`RESTRIKE_L0`]), so the per-second
        // energy arc holds (§9.6, A14). A shifted key takes the lit level so the
        // capital's weight ([`super::SHIFT_GLYPH_GAIN`]) is not lost to
        // [`PASSING_LEVEL`]; `lit` itself is unchanged (bloom, hero request and
        // roof still read it). A steered mark is the arrival and takes it too.
        let step_level = if lit || shifted || steered {
            1.0
        } else {
            PASSING_LEVEL
        };
        let level = if touch == Touch::Step {
            step_level
        } else {
            step_level
                * (RESTRIKE_L0
                    * RESTRIKE_FALL.powi(i32::from(self.text.restrike).saturating_sub(1)))
                .max(RESTRIKE_FLOOR)
        };
        // A sing-along never mutes the typist's own line (§10.2, A31). Under a
        // live riff the melody is ducked beneath the cat; the riff takes only a
        // doubled letter's tremolo to the mallet (`sing && touch == ReStrike`).
        // A held key is caught by `held` at any rate.
        let mallet_only = held || (sing && touch == Touch::ReStrike);
        if rank != 0 {
            self.text.prev_rank = rank;
        }
        self.last_key_ms = Some(at);
        self.space_run = false;
        self.text.word_pos = self.text.word_pos.saturating_add(1);
        self.keys_since_enter = self.keys_since_enter.saturating_add(1);
        TypedPlan {
            touch,
            deg: i32::from(self.text.walk),
            lit,
            level,
            mallet_only,
            repeat,
            word_head,
            rest,
            answer_head,
            opens_shift,
            steered,
            sotto,
            quote,
        }
    }

    /// The derivation (§3.1): the degree this keystroke sounds, from what was
    /// typed and how. Pure apart from its run and motif bookkeeping (no clock,
    /// RNG or allocation), so the same text in the same rhythm is the same
    /// line (A27).
    fn derive(&mut self, rank: u8, gap: u32, first: bool, word_head: bool) -> i32 {
        let from = i32::from(self.text.walk);

        // The answer: the line replaying its subject. Not at the head itself,
        // which snaps to a chord tone and is the transposition point.
        if !word_head && self.text.motif_play > 0 {
            let iv = i32::from(self.text.motif[usize::from(self.text.motif_k)]);
            self.text.motif_k = self.text.motif_k.saturating_add(1);
            self.text.motif_play -= 1;
            let mut deg = reflect_deg(from + iv);
            // A reflected answer that lands where it left turns one degree
            // further, exactly as a derived stride does below.
            if iv != 0 && deg == from {
                deg = reflect_deg(from + iv + iv.signum());
            }
            // The answer is under the same run guard as the line: a `+1 +1 +1`
            // subject answered after a rising head would sound four identical
            // strides, so the fourth inverts here too.
            return self.break_run(from, deg);
        }

        // A session's very first key has nothing to be an interval from.
        if first {
            return from;
        }

        // Step 1: the text makes the interval, folded into −6..=6 so the
        // alphabet's distance is a musical one. With no pair of glyphs to
        // measure (an echo-born cue with no rank, or a new line's first
        // letter), the same fold reads the gap, so a stream that misses the
        // key seam (ssh, a program echoing) still moves.
        let (raw, repeated) = if rank != 0 && self.text.prev_rank != 0 {
            let d = i32::from(rank) - i32::from(self.text.prev_rank);
            (d, d == 0)
        } else {
            // The gap is a magnitude, never a repeat: two keys at the same
            // millisecond are the host's stamp failing, not a doubled letter.
            (gap as i32, false)
        };
        let r = fold_signed13(raw);
        let mag = stride_mag(r, repeated);
        // A folded-to-zero leap takes the raw difference's direction: `a` to
        // `n` climbed thirteen letters and must not become the repeat
        // [`stride_mag`] refused.
        let text_sign = if r != 0 { r.signum() } else { raw.signum() };

        // Step 2: the hand makes the contour. An accelerating burst climbs, a
        // hesitating hand descends, jitter inside the dead band keeps the
        // letter's sign. The same sentence in a different rhythm is a
        // different tune.
        let tempo = self.ioi_ms;
        let sign = if (gap as f32) < ACCEL_SHARE * tempo {
            1
        } else if (gap as f32) > DECEL_SHARE * tempo {
            -1
        } else {
            text_sign
        };
        let mut stride = mag * sign;

        // Step 3: gravity, so the line has a tessitura. See
        // [`MELODY_CENTRE_DEG`] for why the pull is one-sided.
        //
        // Gravity bends a moving line; it never stalls or starts one. A rising
        // second that gravity would cancel becomes a falling second, not an
        // unrequested repeat, and a doubled letter's zero stride is left alone.
        if mag != 0 {
            let pull = if from - MELODY_CENTRE_DEG > MELODY_GRAVITY_DEG {
                -1
            } else if from - MELODY_CENTRE_DEG < -MELODY_GRAVITY_DEG {
                1
            } else {
                0
            };
            stride += pull;
            if stride == 0 {
                stride = pull;
            }
        }
        // A2's in-word bound, as arithmetic: a sixth, never an octave class.
        stride = stride.clamp(-WORD_LEAP_MAX_DEG, WORD_LEAP_MAX_DEG);

        // Step 4: reflect, never clamp. A clamp pins against the ceiling like
        // a stuck siren; a reflection turns around like a phrase.
        let mut deg = reflect_deg(from + stride);
        // A reflection landing back on the note it left (`from + 2` off degree
        // 7) takes one further degree in the direction it turned: a turn is
        // not a stall.
        if stride != 0 && deg == from {
            deg = reflect_deg(from + stride + stride.signum());
        }

        // Step 5: the anti-siren guard, on the stride that will sound. After
        // the reflection, because a `+3` off degree 6 sounds as `+1`.
        self.break_run(from, deg)
    }

    /// The fourth identical stride inverts ([`MELODY_RUN_MAX`]), judged on the
    /// sounded stride `deg − from`. Returns `deg` unless it completes the
    /// fourth; then the first alternative with a different non-zero stride
    /// inside the in-word bound: the inversion; the inversion one degree wider
    /// (when the plain one reflects back onto `from` or onto the same stride);
    /// a wider step the same way; a narrower one. At most one of the four can
    /// equal the run stride, so one always exists.
    fn break_run(&self, from: i32, deg: i32) -> i32 {
        let sounded = deg - from;
        if sounded == 0
            || self.text.run_len < MELODY_RUN_MAX
            || i32::from(self.text.run_stride) != sounded
        {
            return deg;
        }
        let sgn = sounded.signum();
        [
            from - sounded,
            from - sounded - sgn,
            from + sounded + sgn,
            from + sounded - sgn,
        ]
        .into_iter()
        .map(reflect_deg)
        .find(|c| {
            let s = c - from;
            s != 0 && s != sounded && s.abs() <= WORD_LEAP_MAX_DEG
        })
        .unwrap_or(deg)
    }

    /// [`MELODY_RUN_MAX`]'s bookkeeping: how long the line has taken the same
    /// stride. A repeat (stride 0) is standing still, so it clears the count.
    fn note_run(&mut self, stride: i32) {
        if stride != 0 && i32::from(self.text.run_stride) == stride {
            self.text.run_len = self.text.run_len.saturating_add(1);
        } else {
            self.text.run_stride = stride.clamp(-WORD_LEAP_MAX_DEG, WORD_LEAP_MAX_DEG) as i8;
            self.text.run_len = u8::from(stride != 0);
        }
    }

    /// The autorepeat detectors ([`AUTOREPEAT_JITTER_MS`]). The melody still
    /// advances; either can only take the pitch off the note.
    fn detect_autorepeat(&mut self, gap: u32, rank: u8, first: bool) -> bool {
        if first {
            self.repeat_run = 1;
            self.prev_gap = 0;
            return false;
        }
        // Machine regularity, on one glyph — see [`AUTOREPEAT_JITTER_MS`] for
        // why the glyph is half the test.
        let regular = rank != 0
            && rank == self.text.prev_rank
            && self.prev_gap != 0
            && gap.abs_diff(self.prev_gap) <= AUTOREPEAT_JITTER_MS;
        self.repeat_run = if regular {
            self.repeat_run.saturating_add(1)
        } else {
            1
        };
        self.prev_gap = gap;
        self.repeat_run >= AUTOREPEAT_RUN || gap <= AUTOREPEAT_FLOOR_MS
    }

    /// §10.4's `on Space`. The playhead is untouched: a space is a rest.
    /// Returns `true` when this space opens its run: the downbeat, the only
    /// space of an indentation run that plays a bass note.
    fn on_space(&mut self, at: u32) -> bool {
        let head = !self.space_run;
        if head {
            // The undo frame is the state before the keystroke, so deleting
            // the space restores the chord and retyping advances it once.
            self.push_undo();
            // The space bass confirms the cadence the mark asked for: with a
            // mark pending (still the last typed key) the loop advances to the
            // next chord stating its function ([`mark_chords`]), else one
            // chord. A full stop, bang or question also ends the subject.
            let pending = self.pending_mark();
            let n = CHORD_LOOP.len() as u8;
            self.text.chord = match mark_chords(pending) {
                Some(want) => (1..=n)
                    .map(|k| (self.text.chord + k) % n)
                    .find(|c| want.contains(c))
                    .unwrap_or((self.text.chord + 1) % n),
                None => (self.text.chord + 1) % n,
            };
            if matches!(pending, MARK_PERIOD | MARK_BANG | MARK_QMARK) {
                self.relatch_motif();
            }
            self.text.token_marked = false;
            self.text.word_pos = 0;
            self.space_run = true;
            self.space_steps = 0;
        }
        self.last_key_ms = Some(at);
        self.keys_since_enter = self.keys_since_enter.saturating_add(1);
        head
    }

    /// §10.4's `on Enter`: ends the line home (the walk snapped near the
    /// register's centre), parks the loop on [`CHORD_AFTER_ENTER`] and clears
    /// the undo stack (no un-singing across a line break). The motif is not
    /// re-latched: the subject outlives the line until a real rest. Returns
    /// `(full, closed, asked)`: `full` when the cadence is earned
    /// ([`ENTER_PICKUP_MIN_KEYS`]), `closed` when the last mark was `.` or `!`
    /// (so [`TrailSynth::v2_enter`] drops the pickup), `asked` when it was `?`
    /// ([`CAD_QUESTION_LIFT_DEG`]).
    fn on_enter(&mut self, at: u32) -> (bool, bool, bool) {
        let full = self.keys_since_enter >= ENTER_PICKUP_MIN_KEYS;
        let closed = matches!(self.pending_mark(), MARK_PERIOD | MARK_BANG);
        let asked = self.pending_mark() == MARK_QMARK;
        self.end_phrase();
        self.text.chord = CHORD_AFTER_ENTER;
        self.text.walk = self.nearest_lit_within(MELODY_CENTRE_DEG, MELODY_CENTRE_DEG) as i8;
        self.text.run_stride = 0;
        self.text.run_len = 0;
        self.text.prev_rank = 0;
        self.repeat_run = 1;
        self.prev_gap = 0;
        self.text.word_pos = 0;
        self.text.restrike = 0;
        self.last_key_ms = Some(at);
        self.last_enter_ms = Some(at);
        self.undo_len = 0;
        self.space_run = false;
        self.keys_since_enter = 0;
        (full, closed, asked)
    }

    /// Is this line feed the echo of the keyed Return just admitted (§10.4,
    /// [`ENTER_ECHO_SWALLOW_MS`])? The cadence has already spoken for it.
    fn jump_echoes_enter(&self, at: u32) -> bool {
        self.last_enter_ms
            .is_some_and(|e| at.saturating_sub(e) < ENTER_ECHO_SWALLOW_MS)
    }

    /// §10.4's `on Jump`: a PTY line feed, no key credit. `Some(true)` heads a
    /// cascade (the brrrring), `Some(false)` re-strikes its top note, `None` is
    /// swallowed by [`CASCADE_RESTRIKE_MS`] (D18). A new cascade needs a gap in
    /// the line feeds, not since the last cascade (A26). A lone line feed
    /// resolves the line where it stands; later ones of a run walk
    /// [`SONG_THEME`]'s phrase edges, so a stream moves instead of looping.
    fn on_jump(&mut self, at: u32) -> Option<bool> {
        let in_run = self
            .cascade_at
            .is_some_and(|c| at.saturating_sub(c) < CASCADE_RUN_MS);
        if in_run {
            // The run walks the theme: later lines of one program's output take
            // its phrase edges in turn. Only the base moves; the rate law below
            // is unchanged.
            self.text.walk = self.run_theme_step();
        } else {
            // A lone line feed resolves the line where it stands (as a rest
            // does) and re-heads the run's theme. The head stands in for the
            // theme's first edge, so under the re-strike floor (which swallows
            // every other line of a fast stream) the heard edges are the
            // intended ones.
            let here = i32::from(self.text.walk);
            self.text.walk = self.nearest_lit_within(here, here) as i8;
            self.run_theme_pos = 0;
            self.run_phrase = 0;
            let _first_edge_stood_in_for = self.run_theme_step();
        }
        self.text.run_stride = 0;
        self.text.run_len = 0;
        self.text.restrike = 0;
        self.end_phrase();
        self.last_key_ms = Some(at);
        // A line feed ends the word, as a Space or keyed Enter does: the next
        // letter is a word head.
        self.text.word_pos = 0;
        self.space_run = false;
        let head = self
            .cascade_at
            .is_none_or(|c| at.saturating_sub(c) >= CASCADE_EXCLUSIVE_MS);
        self.cascade_at = Some(at);
        if head {
            self.cascade_restrike_ms = at;
            Some(true)
        } else if at.saturating_sub(self.cascade_restrike_ms) >= CASCADE_RESTRIKE_MS {
            self.cascade_restrike_ms = at;
            Some(false)
        } else {
            None
        }
    }

    /// One phrase edge of [`SONG_THEME`]: at a phrase's start take its first
    /// note and step in; elsewhere take the phrase's last note and open the
    /// next phrase. Only line feeds move this cursor, so a run sings
    /// `0 7 5 0 2 2 0 8` and round again. Reflected into the TUNE register.
    fn run_theme_step(&mut self) -> i8 {
        let form = |i: u8| usize::from(SONG_FORM[usize::from(i)]);
        let (start, end) = (form(self.run_phrase), form(self.run_phrase + 1));
        let pos = usize::from(self.run_theme_pos);
        let deg = if pos == start {
            self.run_theme_pos += 1;
            if usize::from(self.run_theme_pos) == end {
                self.open_next_run_phrase();
            }
            SONG_THEME[pos]
        } else {
            self.open_next_run_phrase();
            SONG_THEME[end - 1]
        };
        reflect_deg(i32::from(deg)) as i8
    }

    fn open_next_run_phrase(&mut self) {
        self.run_phrase = (self.run_phrase + 1) % (SONG_FORM.len() as u8 - 1);
        self.run_theme_pos = SONG_FORM[usize::from(self.run_phrase)];
    }

    /// §10.4's `on Backspace`: rewinds one keystroke of melody state. The sound
    /// is the unpitched poof: a deletion has no note (§19.2).
    fn on_backspace(&mut self, at: u32) {
        self.pop_undo();
        self.last_key_ms = Some(at);
        // A deletion ends a whitespace run: the space you type after
        // erasing one is a word boundary again, with its downbeat.
        self.space_run = false;
    }

    /// §10.4's `on Kill / KillWord`: the playhead is untouched (a clause is
    /// leaving, not the tune rewinding), but the undo history and the word go
    /// with the text.
    fn on_kill(&mut self, at: u32) {
        self.undo_len = 0;
        self.end_phrase();
        self.text.word_pos = 0;
        self.last_key_ms = Some(at);
    }

    /// The next glint's degree above the verse note (§13), rotating.
    fn next_glint_deg(&mut self) -> i32 {
        let deg = i32::from(self.text.walk) + GLINT_DEGREES[usize::from(self.glint_k)];
        self.glint_k = (self.glint_k + 1) % GLINT_DEGREES.len() as u8;
        deg
    }

    /// The answer's pitch (§3.3): the nearest degree lit by the live chord
    /// strictly above `deg`, always within three degrees. It may sit above the
    /// TUNE register: it lives in [`LANE_BLOOM`].
    fn answer_deg_above(&self, deg: i32) -> i32 {
        (1..=5)
            .map(|k| deg + k)
            .find(|d| self.deg_is_lit(*d))
            .unwrap_or(deg + CAPITAL_LIFT_DEG)
    }
}

// The instrument's arithmetic: §9.1's curves.

/// Folds a signed quantity into `−6..=6` by adding or subtracting 13: §3.1
/// step 1's interval fold, for the alphabet's distance and for the gap that
/// stands in for it when there is no pair of glyphs.
fn fold_signed13(d: i32) -> i32 {
    (d + 6).rem_euclid(13) - 6
}

/// Reflects a degree back inside the TUNE register (§3.1 step 4), never
/// clamps: a clamp holds a climbing line against the ceiling like a siren; a
/// reflection turns it around like a phrase.
///
/// A stride is bounded by [`WORD_LEAP_MAX_DEG`] and the register is eight
/// degrees wide, so one reflection suffices and the final clamp is
/// unreachable.
fn reflect_deg(deg: i32) -> i32 {
    let mut d = deg;
    for _ in 0..8 {
        if d > TUNE_DEG_HI {
            d = 2 * TUNE_DEG_HI - d;
        } else if d < TUNE_DEG_LO {
            d = 2 * TUNE_DEG_LO - d;
        } else {
            break;
        }
    }
    d.clamp(TUNE_DEG_LO, TUNE_DEG_HI)
}

/// τ_v from the smoothed inter-onset interval (§9.1). See [`TAU_V_BASE_S`] for
/// why this is the masking law and not a taste dial.
fn tau_v_s(ioi_s: f32) -> f32 {
    (TAU_V_BASE_S * (TAU_V_OFFSET + TAU_V_SLOPE * ioi_s)).clamp(TAU_V_MIN_S, TAU_V_MAX_S)
}

/// §9.6's loudness arc, `g_IOI = clamp(√(IOI_s / 0.25), 0.45, 1.0)`.
fn g_ioi(ioi_s: f32) -> f32 {
    (ioi_s / G_IOI_REF_S).sqrt().clamp(G_IOI_MIN, 1.0)
}

/// §9.6's brightness law: the roof rises with rate, opens on a chord tone,
/// takes up to [`ROOF_HEAT_HZ`] from the glow's blaze and up to
/// [`ROOF_HUE_ADD_HZ`] from the hue (§3.3 item 3), never a decibel. `hue` is
/// the arc position from [`hue_arc`] (0 red, 1 cyan); with the hue stop out
/// the caller passes 0.0.
fn roof_hz(cps: f32, lit: bool, heat: f32, hue: f32, touch: Touch) -> f32 {
    let u = ((cps - ROOF_CPS_LO) / ROOF_CPS_SPAN).clamp(0.0, 1.0);
    let plain = ROOF_PLAIN_LO_HZ + (ROOF_PLAIN_HI_HZ - ROOF_PLAIN_LO_HZ) * u;
    let base = if lit {
        (plain + ROOF_LIT_ADD_HZ).min(ROOF_MAX_HZ)
    } else {
        plain
    };
    let base = if touch == Touch::ReStrike {
        base - RESTRIKE_ROOF_DROP_HZ
    } else {
        base
    };
    (base + ROOF_HEAT_HZ * heat.clamp(0.0, 1.0) + ROOF_HUE_ADD_HZ * hue.clamp(0.0, 1.0))
        .min(ROOF_MAX_HZ)
}

/// The hue's position on the arc, 0 at the red end and 1 at the cyan end,
/// through [`tri`] (period two, so the hue is doubled) so the wrap at 1 → 0
/// reflects instead of jumping. Drives the note's roof and the bloom's pan.
fn hue_arc(hue: f32) -> f32 {
    tri(2.0 * hue.clamp(0.0, 1.0))
}

/// How much of the bloom survives at the red end of the arc: high enough that
/// the red end never goes dull (typed-key centroid near 1200 Hz) while the arc
/// still audibly moves.
const HUE_AIR_FLOOR: f32 = 0.80;

/// The hue's air (§3.3 item 2): the bloom is dimmest at the red end of the arc
/// and brightest at the cyan end. Smooth and periodic, so the wrap has no seam.
fn hue_air(hue: f32) -> f32 {
    HUE_AIR_FLOOR + (1.0 - HUE_AIR_FLOOR) * (0.5 - 0.5 * (core::f32::consts::TAU * hue).cos())
}

/// Octave-folds `f` into `[lo, hi)`. Halving and doubling are exact in binary
/// floating point, so the folded pitch keeps its lattice pitch class (§13).
/// Bounded: callers keep `lo` positive and the loops step by octaves.
fn fold_into(f: f32, lo: f32, hi: f32) -> f32 {
    let mut f = f.clamp(1.0, 40_000.0);
    for _ in 0..24 {
        if f < lo {
            f *= 2.0;
        } else if f >= hi {
            f *= 0.5;
        } else {
            break;
        }
    }
    f
}

/// The tine as a prototype voice (§9.1, §9.2). `f` is the fundamental, `tau`
/// the scaled voice decay, `roof` the resolved lowpass. `mallet_only` is
/// §10.2's felt touch. `flow` (§22) changes only the step's
/// fundamental-to-octave balance ([`flow_partials`]); at `0.0` it is inert.
fn tine(f: f32, touch: Touch, tau: f32, roof: f32, mallet_only: bool, flow: f32) -> Voice {
    let (p1, p2, p3, mallet) = match touch {
        Touch::Step => {
            let (p1, p2) = flow_partials(flow);
            (p1, p2, P3_LVL, MALLET_LVL)
        }
        Touch::ReStrike => (P1_LVL, RESTRIKE_P2_LVL, 0.0, RESTRIKE_MALLET_LVL),
    };
    let (p1, p2, p3) = if mallet_only {
        (0.0, 0.0, 0.0)
    } else {
        (p1, p2, p3)
    };
    Voice {
        dur: 3.0 * tau + TINE_DUR_TAIL_S,
        attack: TINE_ATTACK_S,
        decay: tau,
        p: [
            Partial {
                lvl: p1,
                f0: f,
                ..Partial::default()
            },
            Partial {
                lvl: p2,
                f0: f * P2_RATIO,
                decay: P2_TAU_S,
                ..Partial::default()
            },
            Partial {
                lvl: p3,
                f0: f * P3_RATIO,
                decay: P3_TAU_S,
                ..Partial::default()
            },
        ],
        n_lvl: mallet,
        n_f0: MALLET_HZ0,
        n_f1: MALLET_HZ1,
        n_glide: MALLET_GLIDE_S,
        n_q: MALLET_Q,
        n_decay: MALLET_TAU_S,
        lp_cut: roof,
        lane: LANE_TUNE,
        ..Voice::default()
    }
}

/// How hard the hammer falls: what a key's spelling asks of its strike.
/// Resolved once per key by [`Forte::of`], before a voice is built.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Forte {
    /// The hammer force, 0..=1, handed to [`forte`]. Exactly 0 on every plain
    /// key, re-strike and felt key, where `forte` is not called.
    force: f32,
    /// The strike's weight. Exactly 1.0 on a plain key.
    gain: f32,
    /// Whether the key's twelfth ([`CAP_RING_LIFT_DEG`]) rings behind it
    /// ([`CAP_RING_LEVEL`]): only for a capital letter that opens a word or a
    /// shifted run.
    ring: bool,
    /// The sparkle's multiplier re the plain key's ([`KEY_GLINT_LEVEL_MUL`]).
    glint_mul: f32,
    /// Where the hammer's modulator is locked, in turns ([`FORTE_FM_TURN_HEAD`]
    /// on the key carrying [`WORD_CAPITAL_GAIN`], [`FORTE_FM_TURN`] on every
    /// other). Read only when `force > 0`.
    fm_turn: f32,
}

impl Forte {
    /// The table. Only a step that is not `felt` is struck harder: a re-strike
    /// is §9.2's "the same note further away" and a felt key has no tine.
    ///
    /// | context | force | gain | ring | glint |
    /// |---|---|---|---|---|
    /// | plain key | 0 | ×1 | no | class rule |
    /// | capital LETTER at a word head | 1.0 | ×1.35 × [`WORD_CAPITAL_GAIN`] | yes | ×2 |
    /// | capital LETTER opening a shifted run mid-word | 0.8 | ×1.35 | yes | ×2 |
    /// | capital LETTER inside a shifted run (Caps Lock too) | 0.6 | ×1.35 | no | ×2 |
    /// | `!`, by class — shifted or not, so a layout cannot change it | 1.0 | ×1.35 | no | ×4 |
    /// | any other shifted mark | 0.35 | ×[`MARK_SHIFT_GAIN`] | no | class rule |
    /// | re-strike / felt | 0 | the weight only | no | — |
    ///
    /// The sparkle's call site fires only on a struck step.
    fn of(
        shifted: bool,
        class: u8,
        touch: Touch,
        felt: bool,
        word_head: bool,
        opens_shift: bool,
    ) -> Self {
        let step = touch == Touch::Step && !felt;
        let letter = class == LETTER;
        // `!` is a bang by class wherever it was typed from — but a felt
        // unshifted key is the felt mallet whatever its glyph (`classed`).
        let bang = class == BANG && (!felt || shifted);
        let weight = if (shifted && letter) || bang {
            super::SHIFT_GLYPH_GAIN
        } else if shifted {
            MARK_SHIFT_GAIN
        } else {
            1.0
        };
        let head = if shifted && letter && step && word_head {
            WORD_CAPITAL_GAIN
        } else {
            1.0
        };
        let force = if !step {
            0.0
        } else if bang {
            1.0
        } else if !shifted {
            0.0
        } else if !letter {
            0.35
        } else if word_head {
            1.0
        } else if opens_shift {
            0.8
        } else {
            0.6
        };
        let glint_mul = if shifted && letter {
            FORTE_GLINT_MUL
        } else if matches!(class, OPEN | BANG) {
            KEY_GLINT_SHIFTED_MUL
        } else if class == QUOTE {
            QUOTE_GLINT_MUL
        } else {
            1.0
        };
        Self {
            force,
            gain: weight * head,
            ring: shifted && letter && step && opens_shift,
            glint_mul,
            fm_turn: if head == 1.0 {
                FORTE_FM_TURN
            } else {
                FORTE_FM_TURN_HEAD
            },
        }
    }
}

/// A shifted key's decorations stay under [`ROOF_MAX_HZ`]. The bloom's roof
/// (note + [`BLOOM_ROOF_ADD_HZ`]) and the sparkle's ([`GLINT_LP_HZ`]) exceed
/// the 7639 Hz where the one-pole's coefficient saturates at 48 kHz, i.e. they
/// are a bypass; under a shifted key they are clamped to the highest roof that
/// still filters. Under a plain key this returns its operand unchanged.
fn shifted_roof(lp_cut: f32, shifted: bool) -> f32 {
    if shifted {
        lp_cut.min(ROOF_MAX_HZ)
    } else {
        lp_cut
    }
}

/// The hammer's modulation index on a fundamental of `f` Hz at `force`:
/// [`FORTE_FM_INDEX`] at and under [`FORTE_FM_REF_HZ`], falling as
/// `(ref / f)²` above it.
fn forte_index(f: f32, force: f32) -> f32 {
    let roll = (FORTE_FM_REF_HZ / f).min(1.0);
    FORTE_FM_INDEX * force * roll * roll
}

/// Strikes a built voice harder (a post-build reshaper beside [`wood`]); `f` is
/// the fundamental the voice was built on, `force` in `(0, 1]`. It adds the
/// hammer's phase modulation, moves level from the fundamental into a sounding
/// tine octave (`p[1]` at [`P2_RATIO`]; the wood bar's 3f and the pluck's 4f
/// are left alone) and rings it longer, stretches the voice τ under
/// [`FORTE_TAU_MAX_S`] and opens the roof under [`ROOF_MAX_HZ`].
///
/// Untouched: the mallet ([`MALLET_LVL`]'s headroom is spent), the 4 ms attack
/// (§9.5 law 1), every pitch and `Σ p.lvl`; a capital's decibels are
/// [`Forte::gain`]. The strike site locks the modulator's angle after the spawn
/// ([`TrailSynth::v2_lock_hammer`]).
fn forte(v: &mut Voice, f: f32, force: f32) {
    v.p[0].fm_ratio = FORTE_FM_RATIO;
    v.p[0].fm_i0 = forte_index(f, force);
    v.p[0].fm_tau = FORTE_FM_TAU_S;
    if v.p[1].f0 == f * P2_RATIO && v.p[1].lvl > 0.0 {
        let d = (FORTE_P2_SHIFT * force)
            .min(FORTE_P2_MAX - v.p[1].lvl)
            .max(0.0);
        v.p[0].lvl -= d;
        v.p[1].lvl += d;
        v.p[1].decay = P2_TAU_S + FORTE_P2_TAU_ADD_S * force;
    }
    v.decay = (v.decay * (1.0 + FORTE_TAU_GAIN * force))
        .min(FORTE_TAU_MAX_S)
        .max(v.decay);
    v.dur = 3.0 * v.decay + TINE_DUR_TAIL_S;
    v.lp_cut = (v.lp_cut + FORTE_ROOF_ADD_HZ * force).min(ROOF_MAX_HZ);
}

/// The digit's bar ([`WOOD_P2_RATIO`]): the key's tine reshaped, with the odd
/// lattice harmonics 3f / 5f in place of the octave and the 2.760 strike,
/// sum-conserved against the letter's ([`WOOD_P1_LVL`]), and the harder "tok"
/// in place of the felt. `f` is the fundamental `tine` was built on; a step
/// takes the bar's levels, a re-struck digit keeps the re-strike ladder's
/// (0.50 / 0.10 / 0). Neither the IOI nor the fundamental is touched.
fn wood(v: &mut Voice, f: f32, touch: Touch) {
    v.p[1].f0 = f * WOOD_P2_RATIO;
    v.p[1].decay = WOOD_P2_TAU_S;
    v.p[2].f0 = f * WOOD_P3_RATIO;
    v.p[2].decay = WOOD_P3_TAU_S;
    if touch == Touch::Step {
        v.p[0].lvl = WOOD_P1_LVL;
        v.p[1].lvl = WOOD_P2_LVL;
        v.p[2].lvl = WOOD_P3_LVL;
    }
    v.n_f0 = WOOD_MALLET_HZ0;
    v.n_f1 = WOOD_MALLET_HZ1;
    v.n_q = WOOD_MALLET_Q;
    v.n_decay = WOOD_MALLET_TAU_S;
}

/// Which classes are plucked ([`pluck`]): every mark except the opening bracket
/// (it opens: the lit tine, the bloom, the ×4 sparkle and a grace note up),
/// the bang (a sforzando arrival, struck forte at force 1 by class,
/// [`Forte::of`]) and the dash (a tie: the note before it, held). Letters and
/// digits are not marks. The full stop that steers ([`TypedPlan::steered`]) is
/// also a tine, decided in `v2_typed`, since only the melody knows which dot
/// it is.
fn pluck_class(class: u8) -> bool {
    matches!(
        class,
        QMARK | STOP | COMMA | SEMI | COLON | CLOSE | QUOTE | LINE | RISE | MATH | SIGIL
    )
}

/// Which classes may breathe behind their note ([`STOP_BREATH_DELAY_S`]): the
/// four stops `. , ; :`. Whether a key does is the melody's answer: only the
/// token's steering mark ([`TypedPlan::steered`]).
fn stop_breathes(class: u8) -> bool {
    matches!(class, STOP | COMMA | SEMI | COLON)
}

/// The staccato pluck, reshaping the mark's tine: the octave moves to 4f on its
/// own short decay, the 2.76 strike is muted, and the felt becomes the
/// fingertip, or for `_ ~ \ |` and `/` the zip. The caller has already put the
/// voice on the pluck's τ. As in [`wood`], frequencies and decays always move;
/// levels only on a step, so a re-struck mark keeps the re-strike ladder's.
fn pluck(v: &mut Voice, f: f32, class: u8, touch: Touch) {
    let step = touch == Touch::Step;
    v.p[1].f0 = f * PLUCK_P2_RATIO;
    v.p[1].decay = PLUCK_P2_TAU_S;
    // Muted on a step; the ladder's own 0 on a re-strike.
    v.p[2].lvl = 0.0;
    if step {
        v.p[0].lvl = PLUCK_P1_LVL;
        v.p[1].lvl = PLUCK_P2_LVL;
    }
    let (f0, f1, decay, lvl) = match class {
        // A line drawn: down.
        LINE => (ZIP_HZ_HI, ZIP_HZ_LO, ZIP_TAU_S, ZIP_MALLET_LVL),
        // A slash: up.
        RISE => (ZIP_HZ_LO, ZIP_HZ_HI, ZIP_TAU_S, ZIP_MALLET_LVL),
        _ => (PLUCK_HZ0, PLUCK_HZ1, PLUCK_NOISE_TAU_S, PLUCK_NOISE_LVL),
    };
    // Only the zip sweeps; the fingertip keeps the tine's own glide.
    if matches!(class, LINE | RISE) {
        v.n_glide = ZIP_GLIDE_S;
    }
    (v.n_f0, v.n_f1, v.n_q, v.n_decay) = (f0, f1, PLUCK_Q, decay);
    if step {
        v.n_lvl = lvl;
    }
}

/// §9.3's breath as a prototype voice: the air a space moves (900 → 380 Hz,
/// Q 0.7, 8 / 55 / 149 ms, [`LANE_BREATH`]), and the air a stop moves,
/// `delay` seconds after its pluck ([`STOP_BREATH_DELAY_S`]). A space's is at
/// delay 0.
fn breath(delay: f32) -> Voice {
    Voice {
        delay,
        dur: BREATH_DUR_S,
        attack: BREATH_ATTACK_S,
        decay: BREATH_DECAY_S,
        n_lvl: 1.0,
        n_f0: BREATH_HZ0,
        n_f1: BREATH_HZ1,
        n_glide: BREATH_GLIDE_S,
        n_q: BREATH_Q,
        lp_cut: BREATH_ROOF_HZ,
        lane: LANE_BREATH,
        ..Voice::default()
    }
}

/// §9.3's bass dyad as a prototype voice: `root`, its `partner` and the short
/// octave over the root, on `decay` with the tail law's `dur`, `delay` seconds
/// out. The word downbeat, the Return's tonic and the verdict's chords.
fn bass_dyad(root: f32, partner: f32, decay: f32, delay: f32) -> Voice {
    Voice {
        delay,
        dur: TAIL_DUR_PER_TAU * decay,
        attack: BASS_ATTACK_S,
        decay,
        p: [
            Partial {
                lvl: BASS_ROOT_LVL,
                f0: root,
                ..Partial::default()
            },
            Partial {
                lvl: BASS_FIFTH_LVL,
                f0: partner,
                ..Partial::default()
            },
            Partial {
                lvl: BASS_OCT_LVL,
                f0: root * 2.0,
                decay: BASS_OCT_TAU_S,
                ..Partial::default()
            },
        ],
        lp_cut: BASS_ROOF_HZ,
        lane: LANE_BASS,
        bass: true,
        ..Voice::default()
    }
}

/// A lone sine at `f0` on [`P1_LVL`], the upper partials silent.
fn p1_sine(f0: f32) -> [Partial; 3] {
    [
        Partial {
            lvl: P1_LVL,
            f0,
            ..Partial::default()
        },
        Partial::default(),
        Partial::default(),
    ]
}

/// §12.2 layer 1, the meteor's tick as a prototype voice: band-passed noise
/// falling 3200 → 900 Hz at the origin on a 1.5 ms decay, living
/// [`MET_TICK_DUR_S`] so its crest clears the release ramp, under
/// [`MET_TICK_ROOF_HZ`]. The arm adds its ttl.
fn met_tick() -> Voice {
    Voice {
        dur: MET_TICK_DUR_S,
        attack: MET_TICK_ATTACK_S,
        decay: MET_TICK_DECAY_S,
        n_lvl: 1.0,
        n_f0: MET_TICK_HZ0,
        n_f1: MET_TICK_HZ1,
        n_glide: MET_TICK_GLIDE_S,
        n_q: MET_TICK_Q,
        lp_cut: MET_TICK_ROOF_HZ,
        lane: LANE_METEOR,
        ..Voice::default()
    }
}

/// The head's breath: the breath with the word boundary's twinkle on its
/// otherwise silent partials ([`SPACE_TWINKLE_LEVEL`]): the dyad's `root`
/// folded into the stardust lane and its octave under, through
/// [`BREATH_ROOF_HZ`]. Only a run's head; a tail space and a stop take
/// [`breath`].
fn head_breath(root: f32) -> Voice {
    let twinkle = fold_into(root, GLINT_LO_HZ, GLINT_HI_HZ);
    Voice {
        p: [
            Partial {
                lvl: SPACE_TWINKLE_LEVEL,
                f0: twinkle,
                ..Partial::default()
            },
            Partial {
                lvl: SPACE_TWINKLE_UNDER,
                f0: twinkle * 0.5,
                ..Partial::default()
            },
            Partial::default(),
        ],
        ..breath(0.0)
    }
}

/// The bloom as a prototype voice (§3.3 item 1): [`BLOOM_DEGREES`] above the
/// note's lattice degree `deg`, no mallet, under a roof [`BLOOM_ROOF_ADD_HZ`]
/// above the note's. Decay and head scale with `tau_v` (the live IOI's
/// [`tau_v_s`], without the re-strike multiplier) under their ceilings, so at
/// the 4 cps reference each term equals its constant.
fn bloom(deg: i32, roof: f32, tau_v: f32) -> Voice {
    let decay = (BLOOM_DECAY_TAU_MUL * tau_v).min(BLOOM_DECAY_S);
    let head = (tau_v / TAU_V_MAX_S).clamp(BLOOM_HEAD_MIN_MUL, 1.0);
    Voice {
        delay: BLOOM_DELAY_S * head,
        // Redundant (`decay <= BLOOM_DECAY_S`); it names the ceiling.
        dur: (3.0 * decay + TINE_DUR_TAIL_S).min(BLOOM_DUR_S),
        attack: BLOOM_ATTACK_S * head,
        decay,
        p: core::array::from_fn(|i| Partial {
            lvl: BLOOM_LVL[i],
            f0: penta(TINE_BASE_HZ, deg + BLOOM_DEGREES[i]),
            decay: BLOOM_TAU[i],
            ..Partial::default()
        }),
        tw_rate: BLOOM_TW_RATE,
        tw_depth: BLOOM_TW_DEPTH,
        lp_cut: roof + BLOOM_ROOF_ADD_HZ,
        lane: LANE_BLOOM,
        ..Voice::default()
    }
}

// §16 row 9 — the v2 admission path on `TrailSynth`.

impl TrailSynth {
    /// The timbre ladder's seam (test hook): pull one of §3.3's stops and the
    /// synth renders that rung. Production never calls this; a fresh synth is
    /// [`TimbreStops::ALL`].
    #[cfg(test)]
    pub fn set_v2_timbre_stops(&mut self, stops: TimbreStops) {
        self.v2.stops = stops;
    }

    /// The melody's clock: the host's stamp where there is one, else the
    /// synth's block clock (§16 row 7), wrapping like a host's u32 millisecond
    /// stamp rather than saturating, which after 49.7 days would pin every gap
    /// at zero. A wrap costs one key a nonsense gap; it still steps and speaks.
    fn v2_at_ms(&self, meta: EventMeta) -> u32 {
        if meta.at_ms != 0 {
            meta.at_ms
        } else {
            (self.clock_s * 1000.0) as u64 as u32
        }
    }

    /// A seeded velocity multiplier, `10^(u·k/20)` with `u ∈ [−1, 1]` (§9.6),
    /// drawn from the synth's seeded stream so a script replays bit-exactly
    /// (A27).
    fn v2_velocity(&mut self, db: f32) -> f32 {
        let u = self.rnd_in(-1.0, 1.0);
        (10.0f32).powf(u * db / 20.0)
    }

    /// The seeded ±0.03 pan jitter (§9.1). §9.1 states it in the stereo law's
    /// output units (after `× 0.35`); this is the law's input, so the draw is
    /// divided back out and the heard pan moves by ±0.03.
    fn v2_pan(&mut self, pan: f32) -> f32 {
        pan + self.rnd_in(-PAN_JITTER, PAN_JITTER) / PAN_LAW_SCALE
    }

    /// §10.2's key handback: once the sing-along has ended and the line reaches
    /// a boundary (a word head or a rest, [`MelodyV2::at_boundary`]), the
    /// borrowed `song_key` returns to the neutral lattice before the next
    /// word's first note, never mid-word (A31). Called at the top of every
    /// event that can open a word.
    fn v2_hand_back_key(&mut self, at: u32) {
        if self.v2_key_pending && self.v2.at_boundary(at) {
            self.song_key = 0;
            self.v2_key_pending = false;
        }
    }

    /// §9.5 law 5 for lanes whose newcomer may land on a live voice's exact
    /// pitch (echo, glint): damps the old voice first over the 12 ms ramp, so
    /// two independently phased sines at one frequency never comb.
    fn v2_damp_same_pitch(&mut self, lane: u8, f: f32) {
        for v in &mut self.voices {
            if v.on && v.lane == lane && v.damp <= 0.0 && (v.p[0].f0 - f).abs() < SAME_PITCH_HZ {
                v.damp = LANE_FADE_STEAL_S;
                v.damp0 = LANE_FADE_STEAL_S;
            }
        }
    }

    /// Whether the bare Shift's ting is still live and on this pitch. Live: the
    /// address [`MelodyV2`] holds still names a sounding, non-fading
    /// [`LANE_TING`] voice. This pitch: within [`SAME_PITCH_CENTS`], a ratio,
    /// since the ting sits two octaves above the lanes [`SAME_PITCH_HZ`] was
    /// sized for. Read-only.
    fn v2_live_ting_at(&self, f: f32) -> bool {
        let Some((slot, born)) = self.v2.lift else {
            return false;
        };
        let v = &self.voices[usize::from(slot)];
        v.on && v.born == born
            && v.lane == LANE_TING
            && v.damp <= 0.0
            && (1200.0 * (v.p[0].f0 / f).log2()).abs() < SAME_PITCH_CENTS
    }

    /// Eases the live ting down to `to` of its level under the shifted key it
    /// announced, on the pan glide (§16 delta 3) with both far-end gains scaled,
    /// so pan holds and level is continuous. Draws nothing and arms at most once
    /// per ting; a ting already fading, or a stale address, is left alone.
    fn v2_duck_ting(&mut self, to: f32) {
        #[cfg(test)]
        if self.ting_unducked {
            return;
        }
        let Some((slot, born)) = self.v2.lift else {
            return;
        };
        let v = &mut self.voices[usize::from(slot)];
        if v.on
            && v.born == born
            && v.lane == LANE_TING
            && v.damp <= 0.0
            && v.t >= 0.0
            && v.pan_glide_s <= 0.0
        {
            (v.gl1, v.gr1) = (v.gl * to, v.gr * to);
            v.pan_glide_s = TING_DUCK_RAMP_S;
            v.pan_k = 1.0 / TING_DUCK_RAMP_S;
            v.pan_t0 = v.t;
        }
    }

    /// Spawn and return the voice's address — `(slot, born)` — so a later damp
    /// or cancel can prove it is still talking to the same voice.
    fn v2_spawn(&mut self, proto: Voice, gain: f32, pan: f32) -> Option<(u8, u32)> {
        let idx = self.spawn(proto, gain, pan)?;
        Some((idx as u8, self.voices[idx].born))
    }

    /// [`Self::v2_spawn`] with the fundamental's phase handed in, for a voice
    /// that must land in phase with a sounding partial. It still makes `spawn`'s
    /// four draws in order and discards the first phase, since one draw fewer
    /// would shift every later voice's seeded stream (A27); hence not
    /// [`TrailSynth::spawn_seeded`].
    fn v2_spawn_ph0(
        &mut self,
        proto: Voice,
        gain: f32,
        pan: f32,
        ph0: Option<f32>,
    ) -> Option<(u8, u32)> {
        let tw_ph = self.rnd();
        let drawn = [self.rnd(), self.rnd(), self.rnd()];
        // `None` (nothing to lock to) keeps the drawn phase: a constant would
        // start every unlockable voice at the same point and pile them up.
        let ph = [ph0.unwrap_or(drawn[0]), drawn[1], drawn[2]];
        let idx = self.spawn_seeded(proto, gain, pan, tw_ph, ph)?;
        Some((idx as u8, self.voices[idx].born))
    }

    /// The start phase that puts a voice in phase with this lead's sounding
    /// octave, `delay_s` behind the strike. Digit bars (3f) and plucks (4f)
    /// have no stationary 2f, so unmatched voices keep their seeded phase.
    /// The flow echo passes [`FLOW_ECHO_DELAY_S`].
    fn v2_flow_echo_phase(&self, fundamental: f32, delay_s: f32) -> Option<f32> {
        let (slot, born) = self.v2.lead?;
        let lead = &self.voices[usize::from(slot)];
        let p2 = lead.p[1];
        (lead.on
            && lead.born == born
            && lead.lane == LANE_TUNE
            && p2.lvl > 0.0
            && p2.glide <= 0.0
            && p2.f0 == fundamental)
            .then(|| (p2.ph + p2.f0 * delay_s + FLOW_ECHO_PHASE).fract())
    }

    /// Locks a forte strike's modulator to its own fundamental
    /// ([`FORTE_FM_TURN_HEAD`]). `spawn` has drawn the fundamental's phase and
    /// zeroed the modulator's; this sets the modulator `turn` turns past
    /// [`FORTE_FM_RATIO`] × that phase, and the render advances both at that
    /// ratio, so the angle holds. Draws nothing; reached only when
    /// `force > 0`. A stale address (the TUNE lane refused the key) is a no-op.
    fn v2_lock_hammer(&mut self, who: Option<(u8, u32)>, turn: f32) {
        let Some((slot, born)) = who else { return };
        let v = &mut self.voices[usize::from(slot)];
        if v.on && v.born == born && v.p[0].fm_ratio > 0.0 {
            v.p[0].fm_ph = (v.p[0].fm_ratio * v.p[0].ph + turn).fract();
        }
    }

    /// A decoration never steals from a full pool. A mark's rise, tink, fifth
    /// or breath sits in a lane that never drops a newcomer, and the pool-level
    /// steal takes the quietest voice, which can be that mark's own pluck, so
    /// under a full pool the decoration is dropped and the mark still sounds.
    /// Lane caps exclude fade tails and pre-delayed voices, and queued keys can
    /// arrive between renders, so the check is on the whole pool.
    fn v2_spawn_decoration(&mut self, proto: Voice, gain: f32, pan: f32) -> Option<(u8, u32)> {
        if self.voices.iter().all(|v| v.on) {
            return None;
        }
        self.v2_spawn(proto, gain, pan)
    }

    /// Ramps a specific voice down. Does nothing when the address is stale
    /// (slot recycled) or the voice is already damping: §9.5 law 2's ramp is
    /// armed once, never restarted.
    fn v2_damp(&mut self, who: Option<(u8, u32)>, ramp: f32) {
        let Some((slot, born)) = who else { return };
        let v = &mut self.voices[usize::from(slot)];
        if v.on && v.born == born && v.damp <= 0.0 {
            v.damp = ramp;
            v.damp0 = ramp;
        }
    }

    /// Cancels a pre-delayed voice that has not spoken yet (§12.3). A voice
    /// still at `t < 0` has produced no sample, so retiring it is silent. A
    /// voice that has started is left alone: a bell that has sounded is never
    /// damped.
    fn v2_cancel_unsounded(&mut self, who: Option<(u8, u32)>) {
        let Some((slot, born)) = who else { return };
        let v = &mut self.voices[usize::from(slot)];
        if v.on && v.born == born && v.t < 0.0 {
            v.on = false;
        }
    }

    /// Retires a deleted key's graft: cancels it unheard if it has not spoken
    /// (`t < 0`), else ramps it down over `ramp` like the note it decorated. A
    /// ring on a key the hand has just taken back goes with the note (unlike
    /// the meteor's bell, which is never damped once sounded).
    fn v2_retire(&mut self, who: Option<(u8, u32)>, ramp: f32) {
        let Some((slot, born)) = who else { return };
        let v = &mut self.voices[usize::from(slot)];
        if v.on && v.born == born {
            if v.t < 0.0 {
                v.on = false;
            } else if v.damp <= 0.0 {
                v.damp = ramp;
                v.damp0 = ramp;
            }
        }
    }

    /// Test only: zeroes the panned gains of every live voice outside `lane`, so
    /// a render returns that layer alone. `on` stays set: voices stay live with
    /// their lane slots and damp state, so the layer is measured against the
    /// full mix's spawn population.
    #[cfg(test)]
    fn mute_all_lanes_but(&mut self, lane: u8) {
        for v in &mut self.voices {
            if v.lane != lane {
                v.gl = 0.0;
                v.gr = 0.0;
            }
        }
    }

    /// The v2 admission path (§16 row 9), reached only from
    /// [`TrailSynth::push_meta`]'s top-of-function branch, never by a v1 event.
    /// It skips the flood governor, `MIN_GAP` thinning (v2's rate law is lane
    /// caps and IOI-shortened notes), `advance_song` and palette dispatch, and
    /// keeps the bed's style / tone latch, the rate estimate, the erase gate and
    /// the style-agnostic designers behind [`TrailSynth::design_trail`] (poof,
    /// word poof, swoosh, cloud).
    pub(super) fn push_v2(&mut self, ev: SoundEvent, meta: EventMeta) {
        // The verdict forks first, above the trail preamble, none of which
        // applies: the bus is already latched by the keyed Enter, the sky's
        // hue does not drift, the bed is not kicked, and it does not count
        // toward the rate estimate (it is not a keystroke).
        if let SoundGesture::Output(OutputGesture::Verdict { failed, ice }) = ev.kind {
            let at = self.v2_at_ms(meta);
            return self.v2_verdict(&ev, at, failed, ice);
        }
        let SoundGesture::Trail(kind) = ev.kind else {
            return;
        };
        // A key at a password prompt is one tone ([`SECRET`]): the host stamps
        // the class on a keyed cue its tty will not echo, and only
        // [`TrailSynth::v2_secret`] voices it. It reads as a Typed cue for the
        // bed's kick (space and letter kick alike) and latches no flow heat:
        // the preamble records that a key was pressed, never which.
        let secret =
            matches!(kind, SoundKind::Typed | SoundKind::Space) && meta.glyph_class == SECRET;
        let kind = if secret { SoundKind::Typed } else { kind };
        // The sky's colour: the hue's arc position through a one-pole with
        // τ = `BED_HUE_TAU_S`, stepped by the real time since the last event
        // (read before the rate bookkeeping resets it) and seeded on the first
        // v2 event so the pad enters in the ribbon's colour. Bookkeeping only:
        // with the bed off nothing reads it.
        {
            let arc = hue_arc(ev.hue);
            if self.v2_latched {
                let k = 1.0 - (-self.since_event / BED_HUE_TAU_S).exp();
                self.bed.hue_s += (arc - self.bed.hue_s) * k;
            } else {
                self.bed.hue_s = arc;
            }
        }
        // The first v2 trail event latches the bus (§9.7, §16 row 10): the
        // limiter is armed from here on, and the sing-along's key is handed
        // back at phrase boundaries rather than snapped (§10.2).
        self.v2_latched = true;
        // Style / tone follow the trail stream unconditionally, as in v1: a
        // bed re-enabled mid-stream must wake in the current constitution.
        self.bed_style = ev.style;
        self.bed_voice = ev.voice;
        self.tone = ev.tone;
        // Flow heat for this cue (§22), latched before any gesture is voiced
        // (and clamped by `push_meta`), so every voice this push mints uses one
        // number; `0.0` (unstamped hosts, archived renders) gives the cold
        // constants.
        if !secret {
            self.v2.flow = meta.flow;
        }
        // The bed's kick: v1's `bed_kick` behind the same `ev.bed` gate. With
        // `trail_sound_bed` off the pad stays at exact zero; on (the default)
        // it fades in over the first few keys through `tick_bed`'s 250 ms swell
        // and exhales to exact zero after the last, so idle still parks the
        // device.
        if ev.bed {
            self.kick_bed(kind, ev.gain);
        }
        // Other sources (the bonk, the riff) read the rate estimate, so v2
        // feeds it even though its own loudness law is the IOI arc.
        self.rate = self.rate * (-self.since_event / 0.6).exp() + 1.0;
        self.since_event = 0.0;
        let at = self.v2_at_ms(meta);

        // Nothing here damps the ting: it rings through whatever the hand does
        // next ([`TING_ATTACK_S`]).

        match kind {
            SoundKind::Typed if secret => {
                self.since_voice = 0.0;
                self.damp_pending_shimmer();
                self.v2_secret(&ev);
            }
            SoundKind::Typed => {
                self.since_voice = 0.0;
                self.damp_pending_shimmer();
                self.v2_typed(&ev, meta, at);
            }
            SoundKind::Space => {
                self.since_voice = 0.0;
                self.damp_pending_shimmer();
                self.v2_space(&ev, at);
            }
            SoundKind::Backspace => {
                // Every deletion takes the note away (§10.4): a 40 ms mute of
                // the newest tune voice, then the melody rewinds one keystroke.
                // Not behind the erase gate: a held Backspace must un-sing one
                // note per repeat. Backspace is unpitched: the poof only.
                self.v2_damp(self.v2.lead, ERASE_MUTE_S);
                self.v2.lead = None;
                // …and the capital's ring with it: unheard if it has not
                // opened yet, the same 40 ms ramp if it has.
                self.v2_retire(self.v2.ring, ERASE_MUTE_S);
                self.v2.ring = None;
                // …and the mark's graft (a `?` deleted before it asked never
                // asks; a fifth already swelling goes with its key).
                self.v2_retire(self.v2.graft, ERASE_MUTE_S);
                self.v2.graft = None;
                self.v2.prev_class = LETTER;
                self.v2.on_backspace(at);
                // The erase gate, on the poof alone: a deletion's sound is
                // thinned only against other deletions, and the held-run
                // bookkeeping is v1's, so the release shimmer still tells
                // autorepeat from a correction.
                if self.since_erase < ERASE_MIN_GAP {
                    return;
                }
                self.erase_run = if self.since_erase <= HELD_ERASE_RUN_WINDOW {
                    self.erase_run.saturating_add(1)
                } else {
                    1
                };
                self.since_erase = 0.0;
                self.damp_pending_shimmer();
                self.design_trail(ev, kind, 1.0, 0.0);
            }
            SoundKind::Kill | SoundKind::KillWord => {
                self.since_voice = 0.0;
                self.damp_pending_shimmer();
                // Every TUNE voice ramps down over the erase mute (§10.4), and
                // the grafts go with the line: a pre-delayed one is cancelled
                // unheard (a damp alone could let a near-onset voice start), a
                // sounding one takes the same ramp. The ting, in [`LANE_TING`],
                // rings through a kill as through every keyed cue.
                for v in &mut self.voices {
                    if !v.on || !matches!(v.lane, LANE_TUNE | LANE_GRAFT) {
                        continue;
                    }
                    if v.lane == LANE_GRAFT && v.t < 0.0 {
                        v.on = false;
                    } else if v.damp <= 0.0 {
                        v.damp = ERASE_MUTE_S;
                        v.damp0 = ERASE_MUTE_S;
                    }
                }
                self.v2.lead = None;
                self.v2.ring = None;
                self.v2.graft = None;
                self.v2.prev_class = LETTER;
                self.v2.on_kill(at);
                self.design_trail(ev, kind, 1.0, 0.0);
            }
            // The cloud's puff — an accompaniment, on the visual gate, and
            // v1's design verbatim.
            SoundKind::Poof => self.design_trail(ev, kind, 1.0, 0.0),
            SoundKind::Shift => self.v2_shift(&ev),
            // Everything under the meteor floor: the mini-fan's own voice.
            SoundKind::Navigation | SoundKind::Glide { .. } | SoundKind::Sweep { .. } => {
                self.v2_nav_tick(&ev);
            }
            // `Land` is dead under v2 (§12.3, A20): the meteor's bell is the
            // landing.
            SoundKind::Land => {}
            SoundKind::Jump => {
                // A keyed Return's own line-feed echo is not a cascade (A26):
                // a Jump within `ENTER_ECHO_SWALLOW_MS` of that Enter is
                // swallowed whole (no brrrring, no melody step, no beat).
                if self.v2.jump_echoes_enter(at) {
                    return;
                }
                self.since_voice = 0.0;
                self.v2_cascade(&ev, at);
            }
            SoundKind::Enter { cells } => {
                self.since_voice = 0.0;
                self.damp_pending_shimmer();
                self.v2_enter(&ev, at, cells);
            }
            SoundKind::Meteor { dir, cells, armed } => {
                self.since_voice = 0.0;
                self.v2_meteor(&ev, meta, dir, cells, armed);
            }
            SoundKind::MeteorArm { dir } => self.v2_meteor_arm(&ev, meta, dir),
            SoundKind::Stardust { twinkle_hz } => self.v2_glint(&ev, twinkle_hz),
            // A paste: one up-strum of the live chord. `v2_strum` touches no
            // melody state and does not damp the pending shimmer.
            SoundKind::Strum => {
                self.since_voice = 0.0;
                self.v2_strum(&ev);
            }
        }
    }
}

/// The password tone's degree ([`TrailSynth::v2_secret`]): the lattice anchor,
/// C5 at `song_key` 0, the tonic present in every chord.
const SECRET_DEG: i32 = 0;
/// The password tone's τ: 69 ms, [`tau_v_s`] at a 150 ms IOI, fixed so the
/// hand's rhythm does not shape each tone.
const SECRET_TAU_S: f32 = 0.069;

/// The Backspace / Kill mute (§10.4, §11): 40 ms, slower than the 12 ms damp,
/// since a note yanked in 12 ms under a puff of air reads as a glitch.
const ERASE_MUTE_S: f32 = 0.040;

// The gesture designers (§11's table, row by row).

impl TrailSynth {
    /// Typed: the step or re-strike, the bloom behind it, and on keys that earn
    /// them the answering voice and the air cloud (§9.2, §10.2, §11, §3.3).
    fn v2_typed(&mut self, ev: &SoundEvent, meta: EventMeta, at: u32) {
        self.v2_hand_back_key(at);
        let sing = self.sing > 0.0;
        // The key is the note: no early return on this path; every keystroke
        // that reaches here derives a degree and spawns a voice. The glyph
        // class goes in with the key so a phrase mark can steer its own note;
        // below it reshapes the key's tine and adds a decoration. `LETTER`
        // (every letter, every unstamped host and archived render) takes the
        // path the goldens pin.
        let class = meta.glyph_class;
        let plan = self.v2.on_typed(at, meta.rank, sing, ev.shifted, class);
        let stops = self.v2.stops;
        // A felt key (autorepeat, a sing-along re-strike) is the felt mallet
        // whatever its glyph: class shaping is skipped and the key still
        // sounds. `classed` is false on every letter.
        let classed = !plan.mallet_only && class != LETTER;
        // The token's steering `.` rings as a tine; every other dot
        // (`foo.bar`, `3.14`, the second of `..`) is a pluck.
        let cadence_stop = classed && class == STOP && plan.steered;
        // …so "is this key plucked" is the class's answer less that one key.
        let plucked = classed && pluck_class(class) && !cadence_stop;
        // A steered `,` `;` `:` is a breath in the phrase: −2 dB
        // ([`PAUSE_MARK_GAIN`]). ×1.0 on every other key.
        let pause_mark = classed && plan.steered && matches!(class, COMMA | SEMI | COLON);
        // §22: this cue's flow heat, read once. Zero is the cold box.
        let flow = self.v2.flow;
        let ioi_s = self.v2.ioi_ms * 0.001;
        let cps = 1.0 / ioi_s;
        let tau_mul = if plan.touch == Touch::Step {
            1.0
        } else {
            RESTRIKE_TAU_MUL
        };
        let tau = tau_v_s(ioi_s) * tau_mul;
        // A word head gets the lit roof (§10.2): the word boundary is heard in
        // the harmony, and the first letter of a word may open. A capital
        // opens the roof too (§10.4), as do an opening bracket and a bang, by
        // class, so an unshifted `!` on a non-US layout opens like Shift+1.
        let lit_roof = ev.shifted
            || (classed && matches!(class, OPEN | BANG))
            || cadence_stop
            || (plan.lit && (plan.touch == Touch::Step || plan.word_head));
        // The arc (§3.3 item 3): the hue opens the roof, or does nothing at all
        // with the stop out (`hue_arc(0) == 0`, so `plain` is exact).
        let arc = if stops.hue { hue_arc(ev.hue) } else { 0.0 };
        let roof = roof_hz(cps, lit_roof, ev.heat, arc, plan.touch);
        // The aside's darker roof ([`ASIDE_ROOF_MUL`]) on every voice this key
        // builds; `roof` unchanged outside an aside.
        let roof = if plan.sotto {
            roof * ASIDE_ROOF_MUL
        } else {
            roof
        };
        // …and its −2 dB ([`ASIDE_GAIN`]), on every voice the key spawns.
        // Exactly ×1.0 outside an aside.
        let aside_gain = if plan.sotto { ASIDE_GAIN } else { 1.0 };
        // Every key strikes the line's own degree (the walk's, in the song's
        // key), whatever its spelling. Strike, bloom, the ring's root and every
        // graft read this degree and this roof; spelling changes only how hard
        // the note is struck ([`Forte::of`], [`forte`]).
        let deg = plan.deg + i32::from(self.song_key);
        let f = penta(TINE_BASE_HZ, deg);
        let hammer = Forte::of(
            ev.shifted,
            class,
            plan.touch,
            plan.mallet_only,
            plan.word_head,
            plan.opens_shift,
        );
        if plan.repeat {
            // §9.5 law 5: a same-pitch note damps the old voice first; two
            // independently phased voices at one frequency comb. A word head's
            // common tone is as much the same pitch as a doubled letter.
            self.v2_damp(self.v2.lead, LANE_FADE_STEAL_S);
        }
        // The same law covers the ting: it rings through the key it announces
        // and is a chord tone, which a word head may snap to. When the strike
        // lands on the ting's pitch it takes over the note (deterministic, no
        // draw; a felt key has no sine and damps nothing). Runs before the
        // ring's check below, so a ting just put into its fade is no longer
        // live and does not also cost the capital its ring.
        if !plan.mallet_only && self.v2_live_ting_at(f) {
            self.v2_damp(self.v2.lift, LANE_FADE_STEAL_S);
        }
        // Under the capital it announced, the ting yields ([`TING_DUCK`]): the
        // opener (the key carrying forte's ring, and at a word head
        // [`WORD_CAPITAL_GAIN`]) eases a live ting down so the two do not crest
        // over the ceiling. After the same-pitch check, as above.
        if hammer.ring {
            self.v2_duck_ting(TING_DUCK);
        } else if ev.shifted && classed {
            // Under a shifted mark it yields further ([`TING_DUCK_MARK`]):
            // in code a bell every few keys would make it louder than prose
            // (the design's C1).
            self.v2_duck_ting(TING_DUCK_MARK);
        }
        let vel_db = if plan.touch == Touch::Step {
            VEL_DB_STEP
        } else {
            VEL_DB_RESTRIKE
        };
        let vel = self.v2_velocity(vel_db);
        let pan = self.v2_pan(ev.pan);
        let g = g_ioi(ioi_s) * flow_key_headroom(flow);
        // Spelling buys level, never speed (§9.6 is about rate): the strike's
        // gain carries the force table's weight ([`Forte::of`]), and the
        // capital's ring ([`CAP_RING_LEVEL`], a fraction of `gain`) rises with
        // it; the bloom has its own gain. ×1.0 on a plain key.
        let phrase_gain = if pause_mark { PAUSE_MARK_GAIN } else { 1.0 };
        let gain =
            ev.gain * KEY_TINE_TRIM * plan.level * g * vel * hammer.gain * phrase_gain * aside_gain;
        // The class reshapes the one tine (§10.4): one key, one TUNE voice. A
        // digit is the wood bar on a blip's τ, a plucked mark the staccato
        // pluck on the pluck's τ (floored), an opening bracket the lit tine, a
        // letter untouched. The full stop and the dash ring longer than a
        // letter, under [`MARK_TAU_MAX_S`] and never shortened by it; the
        // bang's length is forte's.
        let tau_k = if classed && class == DIGIT {
            tau * DIGIT_TAU_MUL
        } else if plucked {
            (tau * PLUCK_TAU_MUL).max(PLUCK_TAU_MIN_S)
        } else if cadence_stop {
            (tau * CADENCE_STOP_TAU_MUL).min(MARK_TAU_MAX_S).max(tau)
        } else if classed && class == DASH {
            (tau * DASH_TAU_MUL).min(MARK_TAU_MAX_S).max(tau)
        } else {
            tau
        };
        let mut voice = tine(f, plan.touch, tau_k, roof, plan.mallet_only, flow);
        if classed && class == DIGIT {
            wood(&mut voice, f, plan.touch);
        } else if plucked {
            pluck(&mut voice, f, class, plan.touch);
        }
        // A shifted key is struck harder on its own note ([`forte`]): steps
        // only, never a felt or plain key (the force table's zeros).
        if hammer.force > 0.0 {
            forte(&mut voice, f, hammer.force);
        }
        self.v2.lead = self.v2_spawn(voice, gain, pan);
        if hammer.force > 0.0 {
            self.v2_lock_hammer(self.v2.lead, hammer.fm_turn);
        }
        // Both decoration addresses belong to this key. Clear them before its
        // class/shift arms may set them, so deleting a later plain key cannot
        // take an older capital's ring or mark's graft with it.
        self.v2.ring = None;
        self.v2.graft = None;
        // The bloom (§3.3 item 1), behind every lit step with a pitch to bloom
        // from: not a passing note, a doubled letter's tremolo, a felt key or a
        // pluck (§10.4). Its level rides the hue's air (item 2) and its pan
        // drifts with the colour while the strike stays on the caret's column.
        let blooms =
            stops.bloom && plan.lit && plan.touch == Touch::Step && !plan.mallet_only && !plucked;
        // The bloom's own gain, kept for the air cloud, which is two taps of
        // it.
        let (air, spread) = if stops.hue {
            (hue_air(ev.hue), BLOOM_SPREAD * (hue_arc(ev.hue) - 0.5))
        } else {
            (hue_air(0.25), 0.0)
        };
        let bloom_gain =
            ev.gain * KEY_TINE_TRIM * plan.level * g * vel * BLOOM_LEVEL * air * aside_gain;
        if blooms {
            let mut halo = bloom(deg, roof, tau);
            // Every voice a shifted key spawns is under a real filter (F12:
            // `lp_cut` < 7639 Hz, [`shifted_roof`]). A plain key's bloom is
            // untouched.
            halo.lp_cut = shifted_roof(halo.lp_cut, ev.shifted);
            self.v2_spawn(halo, bloom_gain, pan + spread);
        }

        // The sparkle ([`KEY_GLINT_LEVEL_MUL`]): one glint on every struck key,
        // on the bloom's rung (so `plain` is still a tine), passing notes
        // included. Not on a re-strike or felt key: a new glint on a doubled
        // letter would be a brighter second strike, which §9.2's ladder
        // refuses.
        //
        // A digit's sparkle counts ([`COUNT_GLINT_DEG0`]) and leaves the
        // rotation cursor alone, but makes the rotation's draws (a pan and the
        // spawn), so later seeded streams are unchanged.
        if stops.bloom && plan.touch == Touch::Step && !plan.mallet_only {
            if classed && class == DIGIT {
                let d = (i32::from(meta.rank) - DIGIT_RANK0).clamp(0, 9);
                let deg = COUNT_GLINT_DEG0 + d % 5 + i32::from(self.song_key);
                let mul = if d < 5 {
                    COUNT_GLINT_LOW_MUL
                } else {
                    KEY_GLINT_SHIFTED_MUL
                };
                self.v2_glint_deg_at(
                    ev,
                    deg,
                    0,
                    GLINT_LEVEL * KEY_GLINT_LEVEL_MUL * mul * g * aside_gain,
                    vel,
                    KEY_GLINT_DELAY_S,
                );
            } else {
                // The class multiplier ([`Forte::of`]); a bang or an opening
                // quote winks a second time at its own distance
                // ([`BANG_GLINT2_DELAY_S`], [`QUOTE_GLINT2_DELAY_S`]), advancing
                // the rotation twice. A closing quote is plain: ×1, one wink.
                let closes_quote = classed && plan.quote == QUOTE_CLOSES;
                let mul = if closes_quote { 1.0 } else { hammer.glint_mul };
                self.v2_glint_at(
                    ev,
                    0,
                    GLINT_LEVEL * KEY_GLINT_LEVEL_MUL * mul * g * aside_gain,
                    vel,
                    KEY_GLINT_DELAY_S,
                );
                let second = match class {
                    BANG if classed => Some(BANG_GLINT2_DELAY_S),
                    QUOTE if classed && !closes_quote => Some(QUOTE_GLINT2_DELAY_S),
                    _ => None,
                };
                if let Some(delay) = second {
                    self.v2_glint_at(
                        ev,
                        0,
                        GLINT_LEVEL * KEY_GLINT_LEVEL_MUL * mul * g * aside_gain,
                        vel,
                        delay,
                    );
                }
            }
        }

        // The grafts (§10.4): one gesture per bucket in [`LANE_GRAFT`], on the
        // key's own pan and gain, behind the bloom stop, on a step only (a
        // re-struck `??` or `((` does not ask or open twice), except a close
        // right after its open (`()` shares a rank). All go through
        // [`TrailSynth::v2_spawn_decoration`], so under a full pool the
        // decoration is dropped, never the mark's pluck.
        let closes_its_open = class == CLOSE && self.v2.prev_class == OPEN;
        if stops.bloom && (plan.touch == Touch::Step || closes_its_open) && classed {
            match class {
                // `?` rises a fifth, 60 ms on, −3 dB, struck; from a D it rises
                // a fourth to G ([`QUEST_RISE_D_DEG`]), judged on the sounding
                // degree since D–A is this lattice's wolf. The operators sound
                // their fifth, swelling on the bloom's attack from 30 ms at
                // −6 dB, and so does the steered (announcing) colon: G's fifth
                // D opens under it. An inner colon (`a::b`, `12:30`) does not.
                QMARK | MATH | COLON if class != COLON || plan.steered => {
                    let (up, attack, delay, level) = if class == QMARK {
                        let up = if deg.rem_euclid(5) == 1 {
                            QUEST_RISE_D_DEG
                        } else {
                            QUEST_RISE_DEG
                        };
                        (up, TINE_ATTACK_S, QUEST_RISE_DELAY_S, QUEST_RISE_LEVEL)
                    } else {
                        (FIFTH_DEG, BLOOM_ATTACK_S, FIFTH_DELAY_S, FIFTH_LEVEL)
                    };
                    let mut graft = tine(
                        penta(TINE_BASE_HZ, deg + up),
                        Touch::Step,
                        tau,
                        roof,
                        false,
                        flow,
                    );
                    graft.n_lvl = 0.0;
                    graft.p[1].lvl = 0.0;
                    graft.p[2].lvl = 0.0;
                    graft.attack = attack;
                    graft.delay = delay;
                    graft.lane = LANE_GRAFT;
                    self.v2.graft = self.v2_spawn_decoration(graft, gain * level, pan);
                }
                // `( [ {` tink up, `) ] }` tink down: the grace note.
                OPEN | CLOSE => {
                    let step = if class == OPEN { TINK_DEG } else { -TINK_DEG };
                    let t_deg = reflect_deg(plan.deg + step) + i32::from(self.song_key);
                    self.v2.graft = self.v2_tink(t_deg, roof, gain, pan);
                }
                _ => {}
            }
        }
        self.v2.prev_class = if classed { class } else { LETTER };
        // A stop breathes: the space's exhale, 20 ms behind the mark, on the
        // space's own draw (a pan), at the space's level behind the full stop
        // and [`PAUSE_BREATH_MUL`] of it behind `,` `;` `:`. Only the steering
        // mark breathes (one per token, none behind a decimal point), so a line
        // of Rust paths is not mostly air.
        if classed && plan.steered && stop_breathes(class) {
            let pan = self.v2_pan(ev.pan);
            let level = if class == STOP { 1.0 } else { PAUSE_BREATH_MUL };
            self.v2_spawn_decoration(
                breath(STOP_BREATH_DELAY_S),
                ev.gain * KEY_TINE_TRIM * BREATH_LEVEL * g * level * aside_gain,
                pan,
            );
        }

        // Flow's echo and the capital's ring (§22 lever 1), one voice per key,
        // never both: the echo is the key's octave behind a lit unshifted step
        // in flow, the ring a twelfth behind a capital letter that opens a word
        // or shifted run ([`Forte::ring`]). Each rides this key's pan,
        // velocity, loudness arc and lattice, with no mallet. Neither is behind
        // a timbre stop: flow heat is the echo's stop and `ev.shifted` the
        // ring's. At `flow == 0.0` on an unshifted key nothing is claimed or
        // drawn, so the cold box is byte-identical (the goldens prove it). The
        // echo is in [`LANE_BLOOM`] (it drops a newcomer, so at speed the
        // decoration thins, never the tune); the ring in [`LANE_GRAFT`], since
        // under a fast shout BLOOM's age guard would drop the ring, an identity.
        //
        // Every other shifted step makes the ring's four draws and discards
        // them, so the seeded stream behind a shifted key is unchanged (under a
        // full pool nothing is drawn, as the decoration guard does). A shifted
        // key takes no flow echo, and the ring's 2.76f strike partial is muted:
        // a resonance is not struck.
        let ring_gain = if ev.shifted {
            gain * CAP_RING_LEVEL.max(FLOW_ECHO_LEVEL * flow)
        } else {
            gain * FLOW_ECHO_LEVEL * flow
        };
        let struck = plan.touch == Touch::Step && !plan.mallet_only;
        // The ring yields to a live ting on its own pitch (§9.5 law 5): both
        // are chord tones in one register, so a ting on D7 and a capital on G5
        // would put two D7 sines on unrelated phases. The ting already is the
        // resonance; the refused ring's four draws are still made and
        // discarded.
        let ring_deg = deg + FLOW_ECHO_OCTAVE_DEG + CAP_RING_LIFT_DEG;
        let rings = hammer.ring && !self.v2_live_ting_at(penta(TINE_BASE_HZ, ring_deg));
        if ev.shifted && struck && !rings && !self.voices.iter().all(|v| v.on) {
            for _ in 0..4 {
                let _ = self.rnd();
            }
        }
        // A pluck has no octave to extend (its second partial is 4f, on a
        // 30 ms decay). Flow must not reintroduce the hang staccato deletes.
        let echoes = flow > 0.0 && plan.lit && !plucked && !ev.shifted;
        if (echoes || rings) && struck {
            // `echoes` and `rings` are exclusive (`echoes` needs an unshifted
            // key, a ring a shifted letter): the octave is the echo's, the
            // twelfth the ring's.
            let echo_deg = if rings {
                ring_deg
            } else {
                deg + FLOW_ECHO_OCTAVE_DEG
            };
            let mut echo = tine(
                penta(TINE_BASE_HZ, echo_deg),
                Touch::Step,
                tau,
                roof,
                false,
                flow,
            );
            echo.n_lvl = 0.0;
            // The echo swells rather than strikes: with the tine's 4 ms attack
            // it would crest with the bloom (at and under 4 cps) and raise the
            // prose peak in flow. Suppressing a spawn also skips
            // [`TrailSynth::spawn`]'s four draws and re-rolls every voice behind
            // it, which confounds A/B comparisons of this voice.
            if rings {
                echo.p[2].lvl = 0.0;
                echo.attack = CAP_RING_ATTACK_S;
                echo.delay = CAP_RING_DELAY_S;
                echo.lane = LANE_GRAFT;
                // Drawn phase: a twelfth has no partial of the strike to lock
                // to ([`CAP_RING_LIFT_DEG`]).
                self.v2.ring = self.v2_spawn_decoration(echo, ring_gain, pan);
            } else {
                echo.attack = BLOOM_ATTACK_S;
                echo.delay = FLOW_ECHO_DELAY_S;
                echo.lane = LANE_BLOOM;
                let echo_gain = ring_gain;
                // The echo's fundamental is exactly the strike's P2 partial, so
                // it starts at that partial's phase, advanced by the delay and
                // offset by [`FLOW_ECHO_PHASE`], and the pair sums in power on
                // every key. `p2.f0` rather than a re-derived `2 × f`, so the
                // two cannot drift apart by rounding.
                let ph0 = self.v2_flow_echo_phase(echo.p[0].f0, FLOW_ECHO_DELAY_S);
                self.v2_spawn_ph0(echo, echo_gain, pan, ph0);
            }
        }

        if stops.room {
            // The answering voice (§3.3): the head opening the line's reply to
            // its subject is answered from above, a beat later, on the nearest
            // lit tone, in the droppable lane.
            if plan.answer_head && !plan.mallet_only {
                let a_deg = self.v2.answer_deg_above(plan.deg) + i32::from(self.song_key);
                let mut answer = tine(
                    penta(TINE_BASE_HZ, a_deg),
                    Touch::Step,
                    tau,
                    roof,
                    false,
                    flow,
                );
                answer.n_lvl = 0.0;
                answer.delay = ANSWER_DELAY_S;
                answer.lane = LANE_BLOOM;
                let vel = self.v2_velocity(VEL_DB_MOTION);
                let pan = self.v2_pan(ev.pan);
                self.v2_spawn(
                    answer,
                    ev.gain * KEY_TINE_TRIM * ANSWER_LEVEL * g * vel * aside_gain,
                    pan,
                );
            }
            // The air cloud (§3.3 item 4): the room answers the key that ends a
            // phrase rest (after it resolves the line and sings), never a key
            // inside the phrase.
            if plan.rest && !plan.mallet_only {
                self.v2_air_cloud(deg, roof, bloom_gain, pan, 0.0, ev.shifted);
            }
        }
    }

    /// The two-tap air cloud (§3.3 item 4): two bloom taps on `deg`, `after`
    /// seconds past the key, at unequal spacings on opposite sides of `pan`:
    /// the theme's only reverb, spent only where a line ends. Built at
    /// [`TAU_V_MAX_S`], not the live τ_v, since a shortened room is a click.
    fn v2_air_cloud(
        &mut self,
        deg: i32,
        roof: f32,
        bloom_gain: f32,
        pan: f32,
        after: f32,
        shifted: bool,
    ) {
        for k in 0..AIR_TAP_DELAY_S.len() {
            let mut tap = bloom(deg, roof, TAU_V_MAX_S);
            tap.lp_cut = shifted_roof(tap.lp_cut, shifted);
            tap.delay = after + AIR_TAP_DELAY_S[k];
            let side = if k % 2 == 0 {
                -AIR_TAP_PAN
            } else {
                AIR_TAP_PAN
            };
            self.v2_spawn(tap, bloom_gain * AIR_TAP_LEVEL[k], pan + side);
        }
    }

    /// Space: the word boundary (§9.3, §10.3, §10.4). The run's head advances
    /// the chord loop and plays the bass dyad; its tail answers with air (and
    /// indent steps), since indentation is one gesture. The playhead is
    /// untouched either way: a space is a rest.
    fn v2_space(&mut self, ev: &SoundEvent, at: u32) {
        self.v2.ring = None;
        self.v2.graft = None;
        self.v2.prev_class = LETTER;
        let head = self.v2.on_space(at);
        let g = g_ioi(self.v2.ioi_ms * 0.001);
        // The live chord's root — the dyad's fundamental, the twinkle's pitch
        // class and the indent steps' floor. Pure: no draw, no state.
        let ci = usize::from(self.v2.text.chord);
        let chord = CHORD_LOOP[ci];
        let root = penta(
            BASS_BASE_HZ * CHORD_ROOT_RATIO[chord.root],
            self.song_key.into(),
        );
        if head {
            // Monophonic (§9.3): a new downbeat damps the old one over the
            // shipped 12 ms ramp rather than stacking on it.
            self.v2_damp(self.v2.bass, LANE_FADE_STEAL_S);
            // The downbeat rings longer in flow (§22 lever 2): the decay lerps
            // to [`FLOW_BASS_DECAY_S`] and `dur` follows the tail law, adding
            // body without an onset. At zero flow it is the Return's dyad
            // exactly.
            let decay = lerp(BASS_DECAY_S, FLOW_BASS_DECAY_S, self.v2.flow);
            let voice = bass_dyad(root, root * chord.partner, decay, 0.0);
            let vel = self.v2_velocity(VEL_DB_MOTION);
            let gain = ev.gain * KEY_TINE_TRIM * BASS_LEVEL * g * vel;
            // The bass is centred: §11's stereo rule keeps everything under
            // 436 Hz inside ±0.15, and the downbeat is the floor of the mix.
            self.v2.bass = self.v2_spawn(voice, gain, 0.0);
            // The head opens the indent step's gate.
            self.v2.last_step_ms = at;
        }
        let pan = self.v2_pan(ev.pan);
        // The head breathes with a twinkle ([`SPACE_TWINKLE_LEVEL`]) on the
        // breath's silent partials, so the head is still one breath and one
        // dyad (same voices, lanes and draws).
        let exhale = if head { head_breath(root) } else { breath(0.0) };
        self.v2_spawn(exhale, ev.gain * KEY_TINE_TRIM * BREATH_LEVEL * g, pan);
        // The indent step ([`SPACE_STEP_LEVEL`]): the run's tail climbs the
        // live chord's lit degrees over the dyad's root, one per extra space,
        // on its own gate. Never on the head, never louder than it, in the
        // stardust's drop-the-newcomer lane. Its draws land only on tail
        // spaces, so every stream after a word boundary is unchanged.
        if !head && at.saturating_sub(self.v2.last_step_ms) >= SPACE_STEP_MIN_GAP_MS {
            self.v2.last_step_ms = at;
            self.v2.space_steps = self.v2.space_steps.saturating_add(1);
            let f = indent_step_hz(ci, root, self.v2.space_steps, self.song_key.into());
            let mut step = tine(
                f,
                Touch::Step,
                SPACE_STEP_TAU_S,
                ROOF_PLAIN_LO_HZ,
                false,
                0.0,
            );
            step.lane = LANE_GLINT;
            let pan = self.v2_pan(ev.pan);
            self.v2_spawn(step, ev.gain * KEY_TINE_TRIM * SPACE_STEP_LEVEL * g, pan);
        }
    }

    /// Shift: the ting ([`TING_ATTACK_S`] has the design). No melody step, beat
    /// or playhead move: a modifier is intent, not authorship. The cursor is
    /// v1's `shift_step`, advanced as `design_shift` advances it, indexing
    /// [`TING_PICK`] (same length, asserted below). rng: `v2_pan` 1 + spawn 4.
    fn v2_shift(&mut self, ev: &SoundEvent) {
        const _: () = assert!(TING_PICK.len() == super::SHIFT_ROTATION.len());
        self.v2_damp(self.v2.lift, LANE_FADE_STEAL_S);
        let k = usize::from(self.shift_step);
        self.shift_step = (self.shift_step + 1) % super::SHIFT_ROTATION.len() as u8;
        let class = TING_TONES[CHORD_LOOP[usize::from(self.v2.text.chord)].root][TING_PICK[k]];
        let f = ting_fold(penta(TINE_BASE_HZ, class + i32::from(self.song_key)));
        let pan = self.v2_pan(ev.pan);
        self.v2.lift = self.v2_spawn(ting(f), ev.gain * KEY_TINE_TRIM * TING_LEVEL, pan);
    }

    /// Secret: a key typed at a password prompt ([`SECRET`]): one fixed plain
    /// tine on [`SECRET_DEG`] at centre pan, with nothing a letter, capital,
    /// digit, mark or space could vary (no forte, bloom, echo, ring, glint,
    /// breath, velocity or pan draw). It moves no melody, and the same-pitch
    /// damp (§9.5 law 5) makes a fast password replace its last tone rather
    /// than comb. rng: spawn's 4 draws.
    fn v2_secret(&mut self, ev: &SoundEvent) {
        let f = penta(TINE_BASE_HZ, SECRET_DEG + i32::from(self.song_key));
        self.v2_damp_same_pitch(LANE_TUNE, f);
        let voice = tine(f, Touch::Step, SECRET_TAU_S, ROOF_PLAIN_LO_HZ, false, 0.0);
        self.v2_spawn(voice, ev.gain * KEY_TINE_TRIM, 0.0);
    }

    /// Nav tick: the mini-fan's voice (D17, §12.3's floor): the verse note, P1
    /// only, no mallet, −24 dB. A hop too small for a meteor, and what a fizzled
    /// arm resolves into.
    fn v2_nav_tick(&mut self, ev: &SoundEvent) {
        let deg = i32::from(self.v2.text.walk) + i32::from(self.song_key);
        let f = penta(TINE_BASE_HZ, deg);
        // §9.5 law 5: navigation never advances the verse (§12.3), so the tick
        // is always a same-pitch re-strike; damping first makes it replace its
        // predecessor rather than comb.
        self.v2_damp_same_pitch(LANE_TUNE, f);
        let voice = Voice {
            dur: NAV_DUR_S,
            attack: NAV_ATTACK_S,
            decay: NAV_DECAY_S,
            p: p1_sine(f),
            lp_cut: ROOF_PLAIN_LO_HZ,
            lane: LANE_TUNE,
            ..Voice::default()
        };
        let pan = self.v2_pan(ev.pan);
        self.v2_spawn(voice, ev.gain * KEY_TINE_TRIM * NAV_LEVEL, pan);
    }

    /// The lit step's roof at the live IOI under this cue's hue — the one
    /// roof a whole-gesture's tines share (strum, cadence, verdict, bell).
    fn v2_lit_roof(&self, ev: &SoundEvent) -> f32 {
        let arc = if self.v2.stops.hue {
            hue_arc(ev.hue)
        } else {
            0.0
        };
        let cps = 1.0 / (self.v2.ioi_ms * 0.001);
        roof_hz(cps, true, ev.heat, arc, Touch::Step)
    }

    /// PTY cascade: the brrrring, capped (D18). The first line feed of a run
    /// plays the four-note figure; one inside the live cascade re-strikes the
    /// top note alone at −12 dB, and one within 60 ms of that is silent. Twelve
    /// jumps at 60 ms are one cascade and at most eleven quiet re-strikes.
    fn v2_cascade(&mut self, ev: &SoundEvent, at: u32) {
        self.v2_hand_back_key(at);
        let Some(head) = self.v2.on_jump(at) else {
            return;
        };
        let ioi_s = self.v2.ioi_ms * 0.001;
        let tau = tau_v_s(ioi_s);
        let cps = 1.0 / ioi_s;
        let arc = if self.v2.stops.hue {
            hue_arc(ev.hue)
        } else {
            0.0
        };
        let base = i32::from(self.v2.text.walk) + i32::from(self.song_key);
        if head {
            let roof = roof_hz(cps, true, ev.heat, arc, Touch::Step);
            for k in 0..CASCADE_DEGREES.len() {
                let f = penta(TINE_BASE_HZ, base + CASCADE_DEGREES[k]);
                let mut voice = tine(f, Touch::Step, tau, roof, false, self.v2.flow);
                voice.delay = CASCADE_DELAYS_S[k];
                voice.lane = LANE_CASCADE;
                // ± alternating (§11): the figure walks across the stereo
                // field so four fast notes read as a run, not a chord.
                let pan = if k % 2 == 0 { ev.pan } else { -ev.pan };
                let gain = ev.gain * KEY_TINE_TRIM * CASCADE_LEVELS[k];
                self.v2_spawn(voice, gain, pan);
            }
        } else {
            let f = penta(TINE_BASE_HZ, base + CASCADE_RESTRIKE_DEG);
            let mut voice = tine(
                f,
                Touch::ReStrike,
                tau * RESTRIKE_TAU_MUL,
                roof_hz(cps, false, ev.heat, arc, Touch::ReStrike),
                false,
                self.v2.flow,
            );
            voice.lane = LANE_CASCADE;
            let gain = ev.gain * KEY_TINE_TRIM * CASCADE_RESTRIKE_LEVEL;
            self.v2_spawn(voice, gain, ev.pan);
        }
    }

    /// Strum: a paste's one gesture (§28): the live chord's three lit degrees
    /// ascending and the root an octave up, four tines at [`STRUM_DELAYS_S`] on
    /// the cascade's lane, all at the paste's pan. No melody state moves (`walk`,
    /// `steps`, `word_pos`, the run, the undo stack), so the verse resumes where
    /// it stood (`the_remainder_past_the_cap_is_one_strum_not_a_verse`).
    fn v2_strum(&mut self, ev: &SoundEvent) {
        // The paste owns the new text: deleting it must not retire an older
        // key's decoration. Forgetting the addresses leaves that note sounding
        // and keeps the melody/undo state the strum does not advance.
        self.v2.ring = None;
        self.v2.graft = None;
        self.v2.prev_class = LETTER;
        let lit = sky_bed_degrees(usize::from(self.v2.text.chord));
        let degrees = [lit[0], lit[1], lit[2], lit[0] + 5];
        let tau = tau_v_s(self.v2.ioi_ms * 0.001);
        let roof = self.v2_lit_roof(ev);
        let base = i32::from(self.song_key);
        for k in 0..degrees.len() {
            let f = penta(TINE_BASE_HZ, base + degrees[k]);
            let mut voice = tine(f, Touch::Step, tau, roof, false, self.v2.flow);
            voice.delay = STRUM_DELAYS_S[k];
            voice.lane = LANE_CASCADE;
            let gain = ev.gain * KEY_TINE_TRIM * STRUM_TRIM * STRUM_SHAPE[k];
            self.v2_spawn(voice, gain, ev.pan);
        }
    }

    /// Enter: the cadence (§11, D9). The resolution C, the tonic dyad and the
    /// faraway bell fire at `flight_ms(cells)`, the same instant the pin and the
    /// landing squash use; only the pickup is at `t = 0`.
    fn v2_enter(&mut self, ev: &SoundEvent, at: u32, cells: u16) {
        self.v2.ring = None;
        self.v2.graft = None;
        self.v2.prev_class = LETTER;
        self.v2_hand_back_key(at);
        let (full, closed, asked) = self.v2.on_enter(at);
        // §10.4 reads `walk` after the phrase has cadenced, so the resolution
        // answers the note the theme was heading for.
        let walk = self.v2.text.walk;
        let t = timing::flight_ms(f32::from(cells)) * 0.001;
        let ioi_s = self.v2.ioi_ms * 0.001;
        let tau = tau_v_s(ioi_s);
        let stops = self.v2.stops;
        let roof = self.v2_lit_roof(ev);
        let key = i32::from(self.song_key);
        // The resolution's degree: low or high C, so the cadence resolves home
        // rather than leaping; behind a `?` the G above it
        // ([`CAD_QUESTION_LIFT_DEG`]), leaving the question open.
        let resolution = if walk <= CAD_RESOLUTION_SPLIT {
            CAD_RESOLUTION_LOW_DEG
        } else {
            CAD_RESOLUTION_HIGH_DEG
        };
        let resolution = if asked {
            resolution + CAD_QUESTION_LIFT_DEG
        } else {
            resolution
        };
        // The room answers the line's end (§3.3 item 4): two bloom taps behind
        // the note the cadence resolves onto (the resolution when the cadence
        // is earned, else the walk's note), at the bloom's level for this hue.
        let home = if full { resolution } else { i32::from(walk) };
        if stops.room {
            let air = if stops.hue {
                hue_air(ev.hue)
            } else {
                hue_air(0.25)
            };
            let g = g_ioi(ioi_s);
            let bloom_gain = ev.gain * KEY_TINE_TRIM * g * BLOOM_LEVEL * air;
            self.v2_air_cloud(home + key, roof, bloom_gain, ev.pan, t, ev.shifted);
        }
        if full {
            // The pickup, at t = 0 — the one cadence voice that leads.
            let mut pickup = tine(
                penta(TINE_BASE_HZ, CAD_PICKUP_DEG + key),
                Touch::Step,
                tau,
                roof,
                false,
                self.v2.flow,
            );
            pickup.lane = LANE_TUNE;
            // A line the text already closed gets a codetta, not a second
            // cadence: behind a `.` or `!` the line is already home, so the
            // pickup (the upbeat) is not struck; resolution, bell and dyad
            // remain. Its four draws are made and discarded, so later seeded
            // streams are unchanged.
            if closed {
                for _ in 0..4 {
                    let _ = self.rnd();
                }
            } else {
                self.v2_spawn(pickup, ev.gain * KEY_TINE_TRIM * CAD_PICKUP_LEVEL, ev.pan);
            }

            // The resolution, at t = T, on the degree decided above.
            let mut res = tine(
                penta(TINE_BASE_HZ, resolution + key),
                Touch::Step,
                tau,
                roof,
                false,
                self.v2.flow,
            );
            res.delay = t;
            res.lane = LANE_TUNE;
            self.v2_spawn(res, ev.gain * KEY_TINE_TRIM, ev.pan);

            // The faraway ice bell at t = T. Its FM sidebands are deliberately
            // off-lattice (§9.5 law 4's second exemption).
            let (bell, _) = cad_bell_voice(t);
            // Half a column out (§11): far is quiet and slightly off-axis,
            // never loud and centred.
            self.v2_spawn(
                bell,
                ev.gain * KEY_TINE_TRIM * CAD_BELL_LEVEL,
                -0.5 * ev.pan,
            );
        }

        // The tonic dyad at t = T: home in the bass on every Return. The bass
        // is monophonic (§9.3); the dyad takes the lane at its onset, so the
        // previous downbeat is fade-stolen `T` later (`lane_onset_steal`).
        let root = penta(BASS_BASE_HZ, key);
        let dyad = bass_dyad(root, root * 1.5, BASS_DECAY_S, t);
        self.v2.bass = self.v2_spawn(dyad, ev.gain * KEY_TINE_TRIM * CAD_DYAD_LEVEL, 0.0);
    }

    /// The verdict: a command's exit code, told once, on this instrument's
    /// lattice. Green is a plagal cadence, the IV dyad falling to the I dyad
    /// [`VERDICT_PLAGAL_S`] later with one E5 tine over the resolution; a long
    /// one adds the Enter cadence's ice bell. Red is the vi alone, and the loop
    /// is parked so the next word sings against it. Every pitch is already in
    /// [`CHORD_LOOP`] or on the lattice, the playhead does not move, and there
    /// is no `g_ioi`: a verdict is not a keystroke.
    fn v2_verdict(&mut self, ev: &SoundEvent, at: u32, failed: bool, ice: bool) {
        // The line's colour at this moment: the τ, roof and hue arc the cadence
        // would take. `ev.heat` is pinned at 0 by the host for non-authored
        // gestures.
        let tau = tau_v_s(self.v2.ioi_ms * 0.001);
        let roof = self.v2_lit_roof(ev);
        let key = i32::from(self.song_key);

        // The bass is monophonic (§9.3): each dyad takes the lane at its own
        // onset, so the plagal's two chords steal in turn like two word
        // downbeats, and the melody's `bass` field points at the last.
        let dyad = |me: &mut Self, chord_ix: usize, delay: f32, level: f32| {
            let chord = CHORD_LOOP[chord_ix];
            let root = penta(BASS_BASE_HZ * CHORD_ROOT_RATIO[chord.root], key);
            let voice = bass_dyad(root, root * chord.partner, BASS_DECAY_S, delay);
            // Centred, like every downbeat (§11's stereo rule).
            me.v2.bass = me.v2_spawn(
                voice,
                ev.gain * KEY_TINE_TRIM * VERDICT_PEAK_TRIM * level,
                0.0,
            );
        };

        if failed {
            dyad(self, VERDICT_MINOR, 0.0, VERDICT_MINOR_LEVEL);
        } else {
            dyad(self, VERDICT_PLAGAL[0], 0.0, VERDICT_DYAD_LEVEL);
            dyad(
                self,
                VERDICT_PLAGAL[1],
                VERDICT_PLAGAL_S,
                VERDICT_DYAD_LEVEL,
            );
            // The one tine over the resolution: E5, shared by IV and I. Not the
            // melody's `walk`: the verdict must not read as the line's next
            // note (§14's rule for the output pip).
            let mut voice = tine(
                penta(TINE_BASE_HZ, VERDICT_TINE_DEG + key),
                Touch::Step,
                tau,
                roof,
                false,
                self.v2.flow,
            );
            voice.delay = VERDICT_PLAGAL_S;
            voice.lane = LANE_TUNE;
            self.v2_spawn(
                voice,
                ev.gain * KEY_TINE_TRIM * VERDICT_PEAK_TRIM * VERDICT_TINE_LEVEL,
                ev.pan,
            );
            if ice {
                // The faraway ice bell, the Enter cadence's voice (§11), half a
                // column out: far is quiet and slightly off-axis.
                let (bell, _) = cad_bell_voice(VERDICT_PLAGAL_S);
                self.v2_spawn(
                    bell,
                    ev.gain * KEY_TINE_TRIM * VERDICT_PEAK_TRIM * CAD_BELL_LEVEL,
                    -0.5 * ev.pan,
                );
            }
        }
        // The harmony's park, and the hot key's door. Last, so that nothing
        // above can read a chord the verdict has already moved on from.
        self.v2.note_verdict(at, failed, !failed && ice);
    }

    /// Stardust: one hero star, one token, one glint (§13). A single sine on a
    /// lattice degree, octave-folded into the stardust lane, twinkling at the
    /// star's own scintillation rate, so what winks on screen and in the ear is
    /// one number (D12).
    fn v2_glint(&mut self, ev: &SoundEvent, twinkle_hz: u8) {
        let vel = self.v2_velocity(VEL_DB_RESTRIKE);
        // The hero star is its own event and has no strike to stand off
        // from, so it keeps delay zero.
        self.v2_glint_at(ev, twinkle_hz, GLINT_LEVEL, vel, 0.0);
    }

    /// The glint at a level and velocity the caller owns: the hero star draws
    /// its own velocity; a typed key's sparkle shares the strike's, since it is
    /// part of that strike and a second draw would shift the seeded stream
    /// (A27).
    fn v2_glint_at(&mut self, ev: &SoundEvent, twinkle_hz: u8, level: f32, vel: f32, delay: f32) {
        let deg = self.v2.next_glint_deg() + i32::from(self.song_key);
        self.v2_glint_deg_at(ev, deg, twinkle_hz, level, vel, delay);
    }

    /// The tink, the bracket's grace note ([`TINK_DEG`]): a P1-only sine on
    /// lattice degree `deg` (reflected and keyed by the caller), on the key's
    /// roof and pan with no draw of its own. Returns the address for
    /// [`MelodyV2::graft`].
    fn v2_tink(&mut self, deg: i32, roof: f32, gain: f32, pan: f32) -> Option<(u8, u32)> {
        let voice = Voice {
            delay: TINK_DELAY_S,
            dur: TINK_DUR_S,
            attack: TINE_ATTACK_S,
            decay: TINK_TAU_S,
            p: p1_sine(penta(TINE_BASE_HZ, deg)),
            lp_cut: roof,
            lane: LANE_GRAFT,
            ..Voice::default()
        };
        self.v2_spawn_decoration(voice, gain * TINK_LEVEL, pan)
    }

    /// The glint on an explicit lattice degree (already keyed): the digit's
    /// counting sparkle ([`COUNT_GLINT_DEG0`]) names its own degree and leaves
    /// the rotation alone; other callers come through [`Self::v2_glint_at`],
    /// which takes the degree from the rotation. The octave fold, same-pitch
    /// damp and single pan draw live here for both.
    fn v2_glint_deg_at(
        &mut self,
        ev: &SoundEvent,
        deg: i32,
        twinkle_hz: u8,
        level: f32,
        vel: f32,
        delay: f32,
    ) {
        let f = fold_into(penta(TINE_BASE_HZ, deg), GLINT_LO_HZ, GLINT_HI_HZ);
        let rate = if twinkle_hz == 0 {
            GLINT_TWINKLE_DEFAULT_HZ
        } else {
            f32::from(twinkle_hz)
        };
        let voice = Voice {
            delay,
            dur: GLINT_DUR_S,
            attack: GLINT_ATTACK_S,
            decay: GLINT_DECAY_S,
            p: p1_sine(f),
            tw_rate: rate,
            tw_depth: GLINT_TWINKLE_DEPTH,
            lp_cut: shifted_roof(GLINT_LP_HZ, ev.shifted),
            lane: LANE_GLINT,
            ..Voice::default()
        };
        let pan = self.v2_pan(ev.pan);
        // §9.5 law 5: the rotation returns to a pitch every third glint; a
        // live one at that pitch is damped first.
        self.v2_damp_same_pitch(LANE_GLINT, f);
        self.v2_spawn(voice, ev.gain * KEY_TINE_TRIM * level * vel, pan);
    }
}

// §12 — the meteor: one gesture on the observed move.

impl TrailSynth {
    /// The keydown pre-cue (§15.2, D11; off by default). Spawns only the tick
    /// and a provisional core at −20 dB with a static pan at the origin, both
    /// carrying [`MET_ARM_TTL_S`]. Enabled only where the measured echo p50
    /// exceeds one audio buffer, since an arm with no move behind it is a stray
    /// "pff".
    fn v2_meteor_arm(&mut self, ev: &SoundEvent, meta: EventMeta, dir: i8) {
        let pan = meta.pan_from;
        let mut tick = met_tick();
        // Both armed voices carry the ttl (`SoundKind::MeteorArm`); the tick's
        // 12.5 ms life ends before it could expire, but a claim clears it like
        // the core's.
        tick.arm_ttl = MET_ARM_TTL_S;
        self.v2_spawn(tick, ev.gain * KEY_TINE_TRIM * MET_TICK_LEVEL, pan);
        // A provisional flight: `T` is unknown until the echo lands, so the
        // arm takes the maximum, and the claim re-targets it downward.
        let t = timing::FLIGHT_MAX_MS * 0.001;
        let mut core = self.v2_core_voice(dir, t);
        core.arm_ttl = MET_ARM_TTL_S;
        core.pan_glide_s = 0.0;
        self.v2_spawn(core, ev.gain * KEY_TINE_TRIM * MET_ARM_CORE_LEVEL, pan);
    }

    /// The core's prototype at flight `t`: a sine, an octave of exponential
    /// glide in the travel direction, and a light inharmonic FM colour that dies
    /// with it (§12.2 layer 2).
    fn v2_core_voice(&self, dir: i8, t: f32) -> Voice {
        let lo = fold_into(
            penta(
                TINE_BASE_HZ,
                i32::from(self.v2.text.walk) + i32::from(self.song_key),
            ),
            MET_CORE_LO_HZ,
            MET_CORE_HI_HZ,
        );
        // Rightward / upward rises (`Dir::sign` = +1), leftward / downward
        // falls (A17: Ctrl-E core f1/f0 == 2, Ctrl-A == 0.5).
        let (f0, f1) = if dir > 0 {
            (lo, lo * 2.0)
        } else {
            (lo * 2.0, lo)
        };
        Voice {
            dur: MET_CORE_DUR_MUL * t,
            attack: MET_CORE_ATTACK_S,
            decay: MET_CORE_DECAY_MUL * t,
            p: [
                Partial {
                    lvl: P1_LVL,
                    f0,
                    f1,
                    glide: MET_CORE_GLIDE_MUL * t,
                    fm_ratio: MET_CORE_FM_RATIO,
                    fm_i0: MET_CORE_FM_INDEX,
                    fm_tau: MET_CORE_FM_TAU_MUL * t,
                    ..Partial::default()
                },
                Partial::default(),
                Partial::default(),
            ],
            lp_cut: MET_CORE_LP_HZ,
            lane: LANE_METEOR,
            pan_glide_s: t,
            ..Voice::default()
        }
    }

    /// Fizzles every unclaimed arm over the 12 ms ramp (§12.3), when the
    /// promised move is a sub-floor hop; the ttl in [`TrailSynth::render`]
    /// covers no move at all.
    fn v2_release_arm(&mut self) {
        for v in &mut self.voices {
            if v.on && v.lane == LANE_METEOR && v.arm_ttl > 0.0 && v.damp <= 0.0 {
                v.arm_ttl = 0.0;
                v.damp = LANE_FADE_STEAL_S;
                v.damp0 = LANE_FADE_STEAL_S;
            }
        }
    }

    /// Claims the armed voices rather than respawning them (§15.2): a late claim
    /// re-targets, never sounds twice and never clicks. Returns the re-aimed
    /// core's `born`, so the caller identifies the new gesture by identity.
    ///
    /// Everything audible is continuous across the claim (§9.5 law 2):
    /// - pitch: the glide's `f0` is re-based so the new exponential (true
    ///   `0.45·T`, from the voice's own `t`) passes through the frequency
    ///   sounding now;
    /// - level: the envelope is re-seeded with the true `0.7·T` decay and the
    ///   near-end gain compensated so `env × gain` does not move; the claimed
    ///   level (−8 dB) is the pan glide's far end, reached over the flight;
    /// - pan: the travel starts now ([`Voice::pan_t0`]) from the arm's origin;
    /// - FM: its τ keeps the arm's value; re-seeding it would step the phase.
    fn v2_claim_arm(&mut self, ev: &SoundEvent, dir: i8, t: f32) -> Option<u32> {
        let target = self.v2_core_voice(dir, t);
        let vel = self.v2_velocity(VEL_DB_MOTION);
        let claimed_gain = ev.gain * KEY_TINE_TRIM * MET_CORE_LEVEL * vel;
        let dt = self.inv_sr;
        let mut claimed = None;
        for v in &mut self.voices {
            if !(v.on && v.lane == LANE_METEOR && v.arm_ttl > 0.0) {
                continue;
            }
            v.arm_ttl = 0.0;
            if v.p[0].lvl <= 0.0 {
                // The tick — a 12.5 ms noise burst that has already sounded.
                // Nothing to re-aim; it simply keeps its ttl-free life.
                continue;
            }
            let now = v.t.max(0.0);
            let p = &mut v.p[0];
            // Pitch: what the arm is sounding at this instant …
            let cur = if v.env_run {
                p.f1 + (p.f0 - p.f1) * p.g_e
            } else {
                p.f0
            };
            // … becomes the point the true glide passes through at `now`.
            p.f1 = target.p[0].f1;
            p.glide = target.p[0].glide.max(1e-4);
            p.k_g = (-dt / p.glide).exp();
            p.f0 = p.f1 + (cur - p.f1) * (now / p.glide).exp();
            // Level: the true decay, with the envelope's value at `now`
            // carried across by compensating the near-end gain.
            let env_now = if v.env_run { v.env_d } else { 1.0 };
            v.decay = target.decay.max(1e-3);
            v.k_d = (-dt / v.decay).exp();
            let env_new = (-now / v.decay).exp();
            let comp = env_now / env_new.max(1e-6);
            v.gl *= comp;
            v.gr *= comp;
            v.dur = target.dur.max(now + MET_CLAIM_MIN_TAIL_S);
            // Pan: far end at the destination at the claimed level, near end
            // where the arm is at its level; the lerp starts now and lasts the
            // flight.
            v.pan1 = ev.pan;
            (v.gl1, v.gr1) = pan_gains(claimed_gain, ev.pan);
            v.pan_glide_s = t;
            v.pan_k = 1.0 / t;
            v.pan_t0 = now;
            // Re-seed every recursion on the next sample: with the re-based
            // `f0`, the compensated gain and the untouched FM τ, each restarts
            // on its current value.
            v.env_run = false;
            claimed = Some(v.born);
        }
        claimed
    }

    /// The meteor (§12): tick, gliding core, whoosh, bell exactly on the landing
    /// frame, thump, five falling glints. Every time constant is a multiple of
    /// `T = flight_ms(cells)`, the number `crate::rainbow_kitty::timing` hands
    /// both the glow and the synth, so sound and pixels agree at any distance
    /// (A16).
    fn v2_meteor(&mut self, ev: &SoundEvent, meta: EventMeta, dir: i8, cells: u16, armed: bool) {
        // The floor (§12.3): a hop under the meteor distance is the nav tick
        // only, and an arm whose move turns out to be a short hop fizzles here.
        if cells < timing::JUMP_MIN_CELLS {
            self.v2_release_arm();
            self.v2_nav_tick(ev);
            return;
        }
        let t = timing::flight_ms(f32::from(cells)) * 0.001;
        let claimed = if armed {
            self.v2_claim_arm(ev, dir, t)
        } else {
            None
        };

        // Retire the previous gesture (§12.3): its sounding core, whoosh and
        // rain ramp down over 15 ms; anything still unsounded (bell, thump,
        // unfallen rain) is cancelled outright and frees its slot now. The
        // voice this call just claimed is excluded by identity (`born`), since
        // an earlier core over the same distance shares every other field.
        for v in &mut self.voices {
            if !v.on || v.damp > 0.0 || !matches!(v.lane, LANE_METEOR | LANE_RAIN) {
                continue;
            }
            if claimed == Some(v.born) {
                continue;
            }
            if v.t < 0.0 {
                v.on = false;
            } else {
                v.damp = METEOR_INTERRUPT_S;
                v.damp0 = METEOR_INTERRUPT_S;
            }
        }
        self.v2_cancel_unsounded(self.v2.bell);
        self.v2_cancel_unsounded(self.v2.thump);
        self.v2.bell = None;
        self.v2.thump = None;

        let g = ev.gain * KEY_TINE_TRIM;
        if claimed.is_none() {
            // Layer 1 — the tick, at the origin, 12.5 ms long. The ear
            // time-stamps a transient, so the gesture opens with one.
            self.v2_spawn(met_tick(), g * MET_TICK_LEVEL, meta.pan_from);

            // Layer 2 — the core, carrying direction and travelling.
            let mut core = self.v2_core_voice(dir, t);
            core.pan1 = ev.pan;
            let vel = self.v2_velocity(VEL_DB_MOTION);
            self.v2_spawn(core, g * MET_CORE_LEVEL * vel, meta.pan_from);
        }

        // Layer 3 — the whoosh, direction-blind (A17). The rush of travel is
        // the same rush whichever way you went; direction lives in one place.
        let whoosh = Voice {
            dur: MET_WHOOSH_DUR_MUL * t,
            attack: MET_WHOOSH_ATTACK_S,
            decay: MET_WHOOSH_DECAY_MUL * t,
            n_lvl: 1.0,
            n_f0: MET_WHOOSH_HZ0,
            n_f1: MET_WHOOSH_HZ1,
            n_glide: MET_WHOOSH_GLIDE_MUL * t,
            n_q: MET_WHOOSH_Q,
            lp_cut: MET_WHOOSH_LP_HZ,
            lane: LANE_METEOR,
            pan1: ev.pan,
            pan_glide_s: t,
            ..Voice::default()
        };
        self.v2_spawn(whoosh, g * MET_WHOOSH_LEVEL, meta.pan_from);

        // Layer 4 — the bell at exactly `t = T`: the tune's current note thrown
        // across the line. Navigation carries the verse, never advances it.
        let deg = i32::from(self.v2.text.walk) + i32::from(self.song_key);
        let mut bell = tine(
            penta(TINE_BASE_HZ, deg),
            Touch::Step,
            MET_BELL_TAU_S,
            self.v2_lit_roof(ev),
            false,
            self.v2.flow,
        );
        bell.delay = t;
        bell.dur = MET_BELL_DUR_S;
        self.v2.bell = self.v2_spawn(bell, g * MET_BELL_LEVEL, ev.pan);

        // Layer 5 — the thump, the chord root under the landing.
        let chord = CHORD_LOOP[usize::from(self.v2.text.chord)];
        let root = penta(
            BASS_BASE_HZ * CHORD_ROOT_RATIO[chord.root],
            i32::from(self.song_key),
        );
        let thump = Voice {
            delay: t,
            dur: MET_THUMP_DUR_S,
            attack: MET_THUMP_ATTACK_S,
            decay: MET_THUMP_DECAY_S,
            p: [
                Partial {
                    lvl: BASS_ROOT_LVL,
                    f0: root,
                    ..Partial::default()
                },
                Partial {
                    lvl: BASS_FIFTH_LVL,
                    f0: root * chord.partner,
                    ..Partial::default()
                },
                Partial::default(),
            ],
            lp_cut: BASS_ROOF_HZ,
            lane: LANE_BASS,
            ..Voice::default()
        };
        self.v2.thump = self.v2_spawn(thump, g * MET_THUMP_LEVEL, 0.0);

        // Layer 6 — the rain: five glints, one per fan star, in the fan's throw
        // order, spaced by the shared `rain_spacing_ms(T)` so the shower lasts
        // as long as the flight and is gone by T + 250.
        let step_s = timing::rain_spacing_ms(t * 1000.0) * 0.001;
        for k in 0..timing::FAN_HERO_N {
            let glint = Voice {
                delay: t + MET_RAIN_LEAD_S + k as f32 * step_s,
                dur: MET_RAIN_DUR_S,
                attack: MET_RAIN_ATTACK_S,
                decay: MET_RAIN_DECAY_S,
                p: p1_sine(MET_RAIN_HZ[k]),
                tw_rate: MET_RAIN_TWINKLE_HZ,
                tw_depth: MET_RAIN_TWINKLE_DEPTH,
                lp_cut: MET_RAIN_LP_HZ,
                lane: LANE_RAIN,
                ..Voice::default()
            };
            self.v2_spawn(glint, g * MET_RAIN_LEVEL, ev.pan + MET_RAIN_PAN[k]);
        }
    }
}

// The palette entry.

/// The music box as a [`Palette`] roster entry: its bed and register, which
/// the bed mixer and kind-level gestures read through `palette_for`.
///
/// `design` is empty: [`TrailSynth::push_meta`] routes a v2 event to
/// [`TrailSynth::push_v2`] before `design_trail`, and v2 calls `design_trail`
/// only for the kind-level terminal designers (poof, word poof, swoosh,
/// cloud), which return before palette dispatch (§16 row 9).
pub struct RainbowKittyV2Palette;

impl Palette for RainbowKittyV2Palette {
    fn design(
        &self,
        _s: &mut TrailSynth,
        _ev: &SoundEvent,
        _kind: SoundKind,
        _g: f32,
        _deg: i32,
        _col_off: i32,
    ) {
    }

    /// The rainbow sky. The `trail_sound_bed` setting ships on; with it off,
    /// `push_v2` feeds this nothing and it is never reached (`bed_sample`'s
    /// level floor).
    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, _u1: f32, _u2: f32) -> (f32, f32) {
        s.bed_rainbow_sky(dt, lvl)
    }

    fn anchor_hz(&self) -> f32 {
        TINE_BASE_HZ
    }
}

// The rainbow sky: the body.

impl TrailSynth {
    /// One stereo sample `(mid, side)` of the sky pad, behind the music box's
    /// palette bed; the caller has floored `bed.level` and folded level × gain
    /// into `lvl`. The pad glides to the live chord's lit degrees, and the hue's
    /// arc (`bed.hue_s`) drives the tilt and the top tone's twin detune, never a
    /// pitch, so it stays in key. No rng: a render replays from (events, seed).
    pub(super) fn bed_rainbow_sky(&mut self, dt: f32, lvl: f32) -> (f32, f32) {
        let tgt = SKY_BED_TARGETS[usize::from(self.v2.text.chord) % SKY_BED_CHORDS];
        let b = &mut self.bed;
        let glide = 1.0 - (-dt / BED_GLIDE_TAU_S).exp();
        let arc = b.hue_s.clamp(0.0, 1.0);
        let detune = (2.0f32).powf(lerp(BED_DETUNE_CENTS_LO, BED_DETUNE_CENTS_HI, arc) / 1200.0);
        let tone = |ph: f32| -> f32 {
            let mut x = 0.0;
            for n in BED_PARTIALS {
                x += super::sin01((ph * n as f32).fract()) / n as f32;
            }
            x * BED_PARTIAL_NORM
        };
        let mut m = 0.0;
        for i in 0..3 {
            if b.var_f[i] <= 0.0 {
                // First sample: seed at target so the pad enters on the
                // chord instead of sweeping up from 0 Hz.
                b.var_f[i] = tgt[i];
            }
            b.var_f[i] += (tgt[i] - b.var_f[i]) * glide;
            b.var_ph[i] = (b.var_ph[i] + b.var_f[i] * dt).fract();
            let mut x = tone(b.var_ph[i]);
            if i == 2 {
                // The width: the top tone's detuned twin, half and half, so the
                // pair sums to the tone's weight in phase and beats at
                // `f · (detune − 1)`.
                b.var_ph[3] = (b.var_ph[3] + b.var_f[2] * detune * dt).fract();
                x = 0.5 * (x + tone(b.var_ph[3]));
            }
            m += x * BED_WEIGHT[i];
        }
        // The tilt: one one-pole over the pad sum, its cutoff on the arc.
        let cut = lerp(BED_TILT_LO_HZ, BED_TILT_HI_HZ, arc);
        let k = (cut * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (m - b.lp1);
        // The breath: a raised cosine on the whole pad, on its own phase
        // (`ph3`, which nothing else in the music box's bed uses).
        b.ph3 = (b.ph3 + BED_BREATH_HZ * dt).fract();
        let breath = 1.0 - BED_BREATH_DEPTH * (0.5 - 0.5 * super::sin01((b.ph3 + 0.25).fract()));
        (b.lp1 * breath * lvl * BED_LEVEL, 0.0)
    }
}

// §20.2 — the laws, pinned.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor_glow::GlowStyle;
    use crate::tone::Tone;
    use crate::trail_sound::{
        CelebrationGesture, LIMIT_ATTACK_S, LIMIT_RELEASE_S, LIMIT_THRESHOLD, SoundVoice,
        TrailSynth, WordGesture, limit,
    };

    const SR: f32 = 48_000.0;
    const SEED: u32 = 0xA5A5_1234;
    /// The host default volume, and the gain every level in §11 is stated at.
    const VOL: f32 = 0.4;

    /// A fresh synth. Every event below carries the rainbow kitty look, which
    /// selects the music box (`TrailSynth::v2_engaged` reads the event alone).
    fn synth() -> TrailSynth {
        TrailSynth::new(SR, SEED)
    }

    fn event(kind: SoundKind, pan: f32, shifted: bool) -> SoundEvent {
        SoundEvent {
            style: GlowStyle::RainbowKitty,
            voice: SoundVoice::Style,
            kind: SoundGesture::Trail(kind),
            pan,
            heat: 0.5,
            hue: 0.0,
            gain: VOL,
            tone: Tone::Technical,
            bed: false,
            shifted,
        }
    }

    /// A keystroke with a character behind it, stamped through the engine's own
    /// class and rank producers: the melody's input is `(rank, at_ms)` and the
    /// class voices' is `glyph_class`, and leaving either at 0 would test the
    /// no-glyph fallback.
    fn push_ch(s: &mut TrailSynth, kind: SoundKind, at: u32, ch: char) {
        s.push_meta(
            event(kind, 0.0, ch.is_uppercase()),
            EventMeta {
                at_ms: at,
                glyph_class: crate::trail_sound::typed_glyph_class(Some(ch)),
                rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                ..EventMeta::default()
            },
        );
    }

    /// Type `text` from `t0`, one key per `period` ms through [`push_ch`]: a
    /// space is a Space, anything else a typed glyph. Returns the next `at_ms`.
    fn type_line(s: &mut TrailSynth, text: &str, t0: u32, period: u32) -> u32 {
        let mut at = t0;
        for ch in text.chars() {
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            push_ch(s, kind, at, ch);
            at += period;
        }
        at
    }

    // -- The rainbow sky ----------------------------------------------------

    /// The sky is fed by the music box's own keys behind the knob: with
    /// `bed: false` the bed's energy stays at exact zero; with `bed: true` a
    /// keystroke kicks it, it sounds under the notes, and after
    /// typing stops it exhales to exact zero so the host's idle pause engages.
    #[test]
    fn the_sky_is_fed_by_the_music_boxs_keys_only_behind_the_knob() {
        let mut off = synth();
        let mut on = synth();
        let mut buf = [0.0f32; 512];
        for _ in 0..12 {
            off.push(event(SoundKind::Typed, 0.0, false));
            on.push(SoundEvent {
                bed: true,
                ..event(SoundKind::Typed, 0.0, false)
            });
            for _ in 0..8 {
                off.render(&mut buf);
                on.render(&mut buf);
            }
        }
        assert_eq!(off.bed.energy, 0.0, "the knob off feeds nothing");
        assert_eq!(off.bed.level, 0.0);
        assert!(
            on.bed.energy > 0.0 && on.bed.level > 0.1,
            "the knob on: the pad is up"
        );
        // The pad is actually in the output while the notes are up…
        let mut sounding = synth();
        sounding.push(SoundEvent {
            bed: true,
            ..event(SoundKind::Typed, 0.0, false)
        });
        for _ in 0..30 {
            sounding.render(&mut buf);
        }
        let (l, r) = sounding.bed_sample(1.0 / SR);
        assert!(
            l != 0.0 && (l - r).abs() < 1e-9,
            "the pad is a mono floor under the notes"
        );
        // …and the bed exhales to exact zero within six seconds of the last key
        // (`buf` is 256 stereo frames: 5.33 ms a render).
        let renders_per_s = 48_000 / (buf.len() / crate::trail_sound::CHANNELS);
        for _ in 0..(6 * renders_per_s) {
            on.render(&mut buf);
        }
        assert_eq!(
            on.bed.level, 0.0,
            "the bed snaps to exact zero after the typing"
        );
        assert_eq!(on.bed.energy, 0.0);
        assert!(on.is_quiet(), "idle parks the device");
    }

    /// Type `text` at a fixed period from `t0`, one cue per character, and
    /// return the degree the line sounded on each typed key.
    fn type_degrees(text: &str, t0: u32, period: u32) -> Vec<i8> {
        type_degrees_rhythm(text, t0, &[period])
    }

    /// …and the same with a repeating rhythm, so two takes can differ in
    /// nothing but the hand.
    fn type_degrees_rhythm(text: &str, t0: u32, periods: &[u32]) -> Vec<i8> {
        let mut s = synth();
        let mut at = t0;
        let mut out = Vec::new();
        for (i, ch) in text.chars().enumerate() {
            let kind = match ch {
                ' ' => SoundKind::Space,
                '\n' => SoundKind::Enter { cells: 30 },
                _ => SoundKind::Typed,
            };
            push_ch(&mut s, kind, at, ch);
            if matches!(kind, SoundKind::Typed) {
                out.push(s.v2.walk());
            }
            at += periods[i % periods.len()];
        }
        out
    }

    fn push(s: &mut TrailSynth, kind: SoundKind, at: u32, pan: f32, shifted: bool) {
        s.push_meta(
            event(kind, pan, shifted),
            EventMeta {
                at_ms: at,
                ..EventMeta::default()
            },
        );
    }

    /// Every voice spawned since the `born` watermark, newest last.
    fn since(s: &TrailSynth, mark: u32) -> Vec<Voice> {
        let mut v: Vec<Voice> = s
            .voices
            .iter()
            .filter(|v| v.on && v.born > mark)
            .copied()
            .collect();
        v.sort_by_key(|v| v.born);
        v
    }

    /// The TUNE-lane voices among them — the tine's own lane.
    fn tune_voices(v: &[Voice]) -> Vec<Voice> {
        v.iter().filter(|v| v.lane == LANE_TUNE).copied().collect()
    }

    /// The verdict is the music box's alone (the other voices render it as
    /// silence), green and red both speak, and both deliver ≤ −6 dB re a
    /// keystroke.
    #[test]
    fn the_verdict_is_the_music_boxs_alone_and_never_louder_than_a_key() {
        fn verdict(voice: SoundVoice, style: GlowStyle, failed: bool, ice: bool) -> SoundEvent {
            SoundEvent {
                style,
                voice,
                kind: SoundGesture::Output(OutputGesture::Verdict { failed, ice }),
                pan: 0.0,
                heat: 0.0,
                hue: 0.0,
                gain: VOL,
                tone: Tone::Technical,
                bed: false,
                shifted: false,
            }
        }

        // 1 — every other instrument is silent.
        for voice in [
            SoundVoice::Mech,
            SoundVoice::Typewriter,
            SoundVoice::Marimba,
            SoundVoice::Felt,
            SoundVoice::Of(GlowStyle::Water),
            SoundVoice::Of(GlowStyle::Lumen),
        ] {
            let mut s = synth();
            s.push(verdict(voice, GlowStyle::RainbowKitty, false, true));
            assert_eq!(
                render_peak(&mut s, 64),
                0.0,
                "{voice:?} has no verdict — a cat and a music box, or nothing"
            );
        }

        // 2 — the music box speaks for both verdicts.
        let mut green = synth();
        green.push(verdict(
            SoundVoice::RainbowKittyV2,
            GlowStyle::RainbowKitty,
            false,
            true,
        ));
        let g = render_mono(&mut green, 64);
        assert!(g.iter().any(|x| x.abs() > 1e-4), "a success is told");

        let mut red = synth();
        red.push(verdict(
            SoundVoice::RainbowKittyV2,
            GlowStyle::RainbowKitty,
            true,
            false,
        ));
        let r = render_peak(&mut red, 64);
        assert!(r > 0.0, "a failure is told");

        // 3 — and neither is as loud as one keystroke.
        let mut key = synth();
        push_ch(&mut key, SoundKind::Typed, 0, 'a');
        let k = render_peak(&mut key, 64);
        let g_peak = g.iter().fold(0.0f32, |a, x| a.max(x.abs()));
        // ≤ −6 dB re a step, as a delivered peak rather than a level constant.
        let ceiling = k * 0.501_187_2;
        assert!(
            g_peak <= ceiling,
            "the green verdict delivered {:.2} dB re a step — the ceiling is −6",
            20.0 * (g_peak / k).log10()
        );
        assert!(
            r <= ceiling,
            "and the red one delivered {:.2} dB re a step",
            20.0 * (r / k).log10()
        );
    }

    /// The remainder is one strum, not a verse (§28). After a typed word, a
    /// paste gives exactly four tines on the cascade lane on the live chord's
    /// lit degrees plus the root's octave; `walk`, `steps` and `word_pos` are
    /// unchanged and the next key steps from where the word left off. The
    /// other voices render a strum as silence.
    #[test]
    fn the_remainder_past_the_cap_is_one_strum_not_a_verse() {
        let mut s = synth();
        let mut at = 1_000;
        for ch in "hello".chars() {
            push_ch(&mut s, SoundKind::Typed, at, ch);
            at += 90;
        }
        let (walk, steps, word_pos) = (s.v2.walk(), s.v2.steps(), s.v2.word_pos());
        let mark = s.born_seq;
        push(&mut s, SoundKind::Strum, at, 0.25, false);
        let strum = since(&s, mark);
        assert_eq!(strum.len(), 4, "four tines, one gesture: {strum:?}");
        assert!(strum.iter().all(|v| v.lane == LANE_CASCADE));
        let lit = sky_bed_degrees(usize::from(s.v2.chord()));
        let want = [lit[0], lit[1], lit[2], lit[0] + 5];
        for (v, deg) in strum.iter().zip(want) {
            let f = penta(TINE_BASE_HZ, i32::from(s.song_key) + deg);
            assert!(
                (v.p[0].f0 - f).abs() < 1e-3,
                "tine at {} Hz is not lit degree {deg} ({f} Hz)",
                v.p[0].f0
            );
        }
        assert!(
            strum.windows(2).all(|w| w[1].p[0].f0 > w[0].p[0].f0),
            "an UP-strum rises string by string"
        );
        assert_eq!(
            (s.v2.walk(), s.v2.steps(), s.v2.word_pos()),
            (walk, steps, word_pos),
            "the verse does not advance on a paste"
        );
        // The next key steps once, from where the word stood.
        push_ch(&mut s, SoundKind::Typed, at + 90, 'x');
        assert_eq!(s.v2.steps(), steps + 1, "…and the next key is one step");

        // The music box's alone: every other instrument renders a strum as
        // exact silence.
        for voice in [
            SoundVoice::Mech,
            SoundVoice::Typewriter,
            SoundVoice::Marimba,
            SoundVoice::Felt,
            SoundVoice::Of(GlowStyle::Water),
            SoundVoice::Of(GlowStyle::Lumen),
        ] {
            let mut other = synth();
            other.push(SoundEvent {
                voice,
                ..event(SoundKind::Strum, 0.0, false)
            });
            assert_eq!(render_peak(&mut other, 64), 0.0, "{voice:?} has no strum");
        }
        // And under the music box it is never louder than a keystroke.
        let mut key = synth();
        push_ch(&mut key, SoundKind::Typed, 0, 'a');
        let k = render_peak(&mut key, 64);
        let mut strummed = synth();
        push(&mut strummed, SoundKind::Strum, 0, 0.0, false);
        let p = render_peak(&mut strummed, 64);
        assert!(p > 0.0, "the music box strums");
        assert!(
            p <= k,
            "a strum delivered {:.2} dB re a step; a paste never out-shouts a key",
            20.0 * (p / k).log10()
        );
    }

    fn render_peak(s: &mut TrailSynth, blocks: usize) -> f32 {
        let mut buf = [0.0f32; 960];
        let mut peak = 0.0f32;
        for _ in 0..blocks {
            s.render(&mut buf);
            for x in buf {
                peak = peak.max(x.abs());
            }
        }
        peak
    }

    /// Render `blocks` × 480 frames into one mono vector (L+R)/2.
    fn render_mono(s: &mut TrailSynth, blocks: usize) -> Vec<f32> {
        let mut buf = [0.0f32; 960];
        let mut out = Vec::with_capacity(blocks * 480);
        for _ in 0..blocks {
            s.render(&mut buf);
            for f in buf.as_chunks::<2>().0 {
                out.push(0.5 * (f[0] + f[1]));
            }
        }
        out
    }

    // -- A28: the ladder floor ------------------------------------------

    // -- R1: one keystroke, one melody step ------------------------------

    /// One keystroke is one melody step at every typing speed, and no keystroke
    /// is silent. The sweep runs from 3 cps to 50 cps (past any hand, into
    /// autorepeat): at every rate the melody advances once per key and spawns a
    /// TUNE voice for every key.
    #[test]
    fn every_key_is_a_step_at_every_rate_and_no_key_is_silent() {
        // A pangram-ish key stream, so the ranks are spread and consecutive
        // letters are rarely equal.
        const KEYS: &str = "thequickbrownfoxjumpsoverthelazydogandthenwritesitdownagain";
        for period in [333u32, 250, 222, 200, 167, 125, 100, 71, 50, 33, 20] {
            let mut s = synth();
            let mut spoke = 0;
            let mut at = 1_000u32;
            for ch in KEYS.chars() {
                let mark = s.born_seq;
                push_ch(&mut s, SoundKind::Typed, at, ch);
                if !tune_voices(&since(&s, mark)).is_empty() {
                    spoke += 1;
                }
                at += period;
            }
            let n = KEYS.chars().count() as u32;
            assert_eq!(
                s.v2.steps(),
                n,
                "at {:.1} cps the melody advanced {} times for {n} keys — a gate has grown back",
                1_000.0 / f64::from(period),
                s.v2.steps()
            );
            assert_eq!(
                spoke,
                n as usize,
                "at {:.1} cps only {spoke} of {n} keys spawned a tune voice",
                1_000.0 / f64::from(period)
            );
        }
    }

    /// A held key still steps and still speaks: autorepeat (one glyph on a
    /// machine-regular clock, [`AUTOREPEAT_JITTER_MS`]) takes the tine's
    /// partials to zero and leaves the mallet, felt rather than pitched, while
    /// the melody keeps moving.
    #[test]
    fn a_held_key_is_felt_not_silenced_and_the_line_keeps_moving() {
        let mut s = synth();
        let mut onsets = 0;
        let mut pitched = 0;
        for k in 0..60u32 {
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Typed, 1_000 + k * 33, 'a');
            for v in tune_voices(&since(&s, mark)) {
                onsets += 1;
                if v.p[0].lvl > 0.0 {
                    pitched += 1;
                }
            }
        }
        assert_eq!(s.v2.steps(), 60, "the melody stalled under a held key");
        assert_eq!(onsets, 60, "a held key went SILENT — never legal again");
        assert!(
            pitched <= usize::from(AUTOREPEAT_RUN),
            "{pitched} of 60 auto-repeat onsets were pitched; the detector \
             should have taken all but its own run-in to the mallet alone"
        );

        // A fast human hand is not a machine: the same rate with human jitter
        // and different letters stays pitched.
        let mut s = synth();
        let mut pitched = 0;
        let jitter = [30u32, 37, 33, 41, 28, 35, 39, 31];
        let mut at = 1_000u32;
        for (k, ch) in "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefgh"
            .chars()
            .enumerate()
        {
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Typed, at, ch);
            if tune_voices(&since(&s, mark))
                .iter()
                .any(|v| v.p[0].lvl > 0.0)
            {
                pitched += 1;
            }
            at += jitter[k % jitter.len()];
        }
        assert_eq!(
            pitched,
            60,
            "a fast HUMAN hand was mistaken for a machine on {} of 60 keys",
            60 - pitched
        );
    }

    /// The same text typed the same way is the same line; typed differently it
    /// is a different one (R2, §3.1 determinism): neither the text alone nor
    /// the rhythm alone fixes the tune, and the two together fix it exactly.
    #[test]
    fn the_line_is_a_function_of_the_text_and_the_rhythm_and_of_nothing_else() {
        const TEXT: &str = "the quick brown fox jumps over the lazy dog";
        const OTHER: &str = "the quick brown fox jumps over the busy dog";
        let steady = type_degrees(TEXT, 1_000, 200);

        assert_eq!(
            steady,
            type_degrees(TEXT, 1_000, 200),
            "the same text at the same rhythm played two different lines"
        );
        assert_eq!(
            steady,
            type_degrees(TEXT, 40_000, 200),
            "the line moved when the take started later — an absolute clock leaked in"
        );
        assert!(
            !steady.is_empty() && steady.len() == TEXT.chars().filter(|c| *c != ' ').count(),
            "every typed key must have sounded a degree"
        );

        // A different rhythm, same text: the contour reads the hand, so the
        // line must differ.
        let hurried = type_degrees_rhythm(TEXT, 1_000, &[120, 340, 150, 300, 130]);
        assert_ne!(
            steady, hurried,
            "the same text typed with a different rhythm played the SAME line — \
             the hand is not reaching the melody"
        );

        // A different text, same rhythm: the line must differ, from the
        // changed word onward and not before it.
        let other = type_degrees(OTHER, 1_000, 200);
        assert_ne!(
            steady, other,
            "two different sentences at one rhythm played the same line — \
             the text is not reaching the melody"
        );
        let common = steady
            .iter()
            .zip(&other)
            .take_while(|(a, b)| a == b)
            .count();
        let before_change = TEXT
            .chars()
            .zip(OTHER.chars())
            .take_while(|(a, b)| a == b)
            .filter(|(a, _)| *a != ' ')
            .count();
        assert!(
            common >= before_change,
            "the line diverged at key {common}, BEFORE the text did at key \
             {before_change} — something other than the text moved it"
        );
    }

    /// The run guard holds on the sounded line, not on `derive`'s stride: the
    /// pathological streams of the WAV bench (§21.4; the bench is deleted)
    /// stream for stream, plus prose where a rising subject's answer could
    /// sound a five-note scale. Counted on the sounded degrees, as the bench
    /// counted them; every take stays inside the TUNE register.
    #[test]
    fn the_run_guard_holds_on_the_sounded_line_not_the_counted_one() {
        // (text, period ms) scenes, each followed by the bench's 700 ms
        // think — `Hand::steady(12.0)` is 83 ms, `Hand::steady(3.5)` 286.
        const SCENES: [(&str, u32); 6] = [
            ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 83),
            ("0123456789 0123456789", 83),
            ("!!!!", 83),
            ("aGVsbG8gd29ybGQgdGhpcyBpcyBub3QgcHJvc2U+Pz8/", 83),
            ("the the the the the the the the ", 83),
            ("!!!! 999 aaaa", 286),
        ];
        let mut takes: Vec<(&str, Vec<i8>)> = Vec::new();
        let mut s = synth();
        let mut at = 500u32;
        let mut degs = Vec::new();
        for (text, period) in SCENES {
            for ch in text.chars() {
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(&mut s, kind, at, ch);
                if kind == SoundKind::Typed {
                    degs.push(s.v2.walk());
                }
                at += period;
            }
            at += 700;
        }
        takes.push(("pathological, as the bench types it", degs));
        // The prose as the bench's hand types it (`type_text`'s cadence): 100 ms
        // a key at 10 cps, a 550 ms rest after a full stop, a Jump and a 350 ms
        // pause at a line ending. The rests matter: they resolve the walk and
        // re-latch the subject, and one run-guard case arises only on the
        // subject this cadence latches.
        let mut s = synth();
        let mut at = 500u32;
        let mut degs = Vec::new();
        for ch in BENCH_PROSE.chars() {
            let kind = match ch {
                ' ' => SoundKind::Space,
                '\n' => SoundKind::Jump,
                _ => SoundKind::Typed,
            };
            push_ch(&mut s, kind, at, ch);
            if kind == SoundKind::Typed {
                degs.push(s.v2.walk());
            }
            at += 100;
            if ch == '\n' {
                at += 350;
            }
            if ch == '.' {
                at += 550;
            }
        }
        takes.push(("prose, as the bench types it", degs));
        // …and at three more hands, because which subject gets latched, and
        // so where the answer can leak, depends on the rhythm.
        for periods in [
            &[100u32][..],
            &[83, 91, 83, 77, 83],
            &[140, 500, 130, 620, 150],
        ] {
            takes.push(("prose", type_degrees_rhythm(BENCH_PROSE, 1_000, periods)));
        }
        for (name, degs) in takes {
            assert!(
                degs.iter()
                    .all(|d| (TUNE_DEG_LO..=TUNE_DEG_HI).contains(&i32::from(*d))),
                "the line left the TUNE register on {name}"
            );
            let mut run = 1usize;
            let mut worst = (1usize, 0usize);
            for (i, w) in degs.windows(3).enumerate() {
                if w[1] - w[0] == w[2] - w[1] && w[1] != w[0] {
                    run += 1;
                } else {
                    run = 1;
                }
                if run > worst.0 {
                    worst = (run, i + 2);
                }
            }
            assert!(
                worst.0 <= usize::from(MELODY_RUN_MAX),
                "{} identical strides SOUNDED without turning on {name}, ending at key {} \
                 (…{:?}) — the guard counted a stride the ear did not get",
                worst.0,
                worst.1,
                &degs[worst.1.saturating_sub(5)..=worst.1]
            );
        }
    }

    /// Every repeated pitch is charged to a cause the design names: the text
    /// (equal alphabet ranks), a word head's chord snap holding a common tone,
    /// the line answering its subject on a latched unison, or the mark phrasing
    /// it (`phr`: a dash ties, a steered mark may arrive where the line stands,
    /// a closing bracket may already be home). A repeat with no cause is the
    /// engine standing still while the hand moved, so the residual is zero.
    #[test]
    fn every_repeated_pitch_is_charged_to_a_cause_the_design_names() {
        const PATHOLOGICAL: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1234567890 !!!! \
             aGVsbG8gd29ybGQgdGhpcyBpcyBub3QgcHJvc2U ffff the the the the the the the the";
        // A metronome, a hurried hand, a hesitating one and a machine-fast run:
        // the contour reads the gap, so each gets its own account.
        const RHYTHMS: [&[u32]; 4] = [
            &[200],
            &[83, 91, 83, 77, 83],
            &[140, 500, 130, 620, 150],
            &[71],
        ];
        // `ordinary` says whether the corpus is text a person would write. The
        // pathological one (30 held `a`s, `!!!!`, `ffff`, one word eight times)
        // is a third doubled letters, so a bound on its total repeat share
        // would bound the fixture, not the engine.
        for (name, text, ordinary) in [
            ("prose", BENCH_PROSE, true),
            ("pathological", PATHOLOGICAL, false),
        ] {
            for periods in RHYTHMS {
                let mut s = synth();
                let mut at = 1_000u32;
                let (mut keys, mut dbl, mut head, mut subj, mut stall) = (0, 0, 0, 0, 0);
                let mut phr = 0;
                let mut prev_rank = 0u8;
                let mut prev_deg: Option<i8> = None;
                for (i, ch) in text.chars().enumerate() {
                    let kind = match ch {
                        ' ' => SoundKind::Space,
                        '\n' => SoundKind::Enter { cells: 30 },
                        _ => SoundKind::Typed,
                    };
                    // Read before the push, with the engine's own predicates:
                    // `word_pos == 0` is the word-head branch,
                    // `!word_head && motif_play > 0` the replay branch.
                    let was_head = s.v2.word_pos() == 0;
                    let was_answer = !was_head && s.v2.motif_answering();
                    let was_marked = s.v2.token_marked();
                    let rank = crate::trail_sound::typed_glyph_rank(Some(ch));
                    push_ch(&mut s, kind, at, ch);
                    if matches!(kind, SoundKind::Typed) {
                        keys += 1;
                        let deg = s.v2.walk();
                        let class = crate::trail_sound::typed_glyph_class(Some(ch));
                        // The mark's override runs last in `on_typed`, so it
                        // produced the degree whenever it fired: a tie, a
                        // bracket's return, or the token's steering mark (the
                        // budget went false → true).
                        let phrased =
                            matches!(class, DASH | CLOSE) || (!was_marked && s.v2.token_marked());
                        if prev_deg == Some(deg) {
                            // Charged to the branch that produced the degree:
                            // an answered interval never consulted the alphabet.
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
                        prev_deg = Some(deg);
                    }
                    at += periods[i % periods.len()];
                }
                assert!(
                    keys > 100,
                    "fixture too short to say anything ({keys} keys on {name})"
                );
                assert_eq!(
                    stall, 0,
                    "{stall} of {keys} keys on {name} at rhythm {periods:?} repeated \
                     the sounding pitch with no cause §3.1 names — the line stood \
                     still while the hand moved (dbl {dbl}, head {head}, subj {subj}, \
                     phr {phr})"
                );
                // What the engine adds to the text's own repetition is bounded
                // on every corpus: `dbl` is the text's, while `head` and `subj`
                // are the instrument's own.
                assert!(
                    (head + subj) * 10 < keys,
                    "the ENGINE repeated the sounding pitch on {} of {keys} keys on \
                     {name} at rhythm {periods:?} (head {head}, subj {subj}) — \
                     accounted for, but that is the instrument's repetition, not \
                     the text's",
                    head + subj
                );
                // On text a person would write, the total is bounded too: past a
                // tenth the line repeats more than it derives.
                assert!(
                    !ordinary || (dbl + head + subj + phr) * 10 < keys,
                    "{} of {keys} keys on {name} at rhythm {periods:?} repeated the \
                     last pitch (dbl {dbl}, head {head}, subj {subj}, phr {phr}) — \
                     accounted for, but that is no longer a derived line",
                    dbl + head + subj + phr
                );
            }
        }
    }

    // -- A2: the anti-leap law, on the bench's own prose -------------------

    /// The prose corpus: §21.4's `prose-10cps` is this text at 10 cps, and A2
    /// is pinned on it so the pin sees every wrap, line feed and sentence rest.
    const BENCH_PROSE: &str = "\
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

    /// One scripted cue, the bench's own shape: (time s, gesture, pan, heat,
    /// shifted, glyph class).
    type Cue = (f32, SoundKind, f32, f32, bool, u8);

    /// The bench's `needs_shift`: the glyphs a US layout cannot produce
    /// without Shift.
    fn bench_needs_shift(ch: char) -> bool {
        ch.is_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(ch)
    }

    /// The bench's `type_text`: a space is a `Space`, a newline a `Jump`,
    /// everything else `Typed`; a line ending rests 350 ms and a full stop
    /// 550.
    fn bench_type_text(cues: &mut Vec<Cue>, t0: f32, cps: f32, text: &str, heat: f32) -> f32 {
        let mut t = t0;
        let dt = 1.0 / cps;
        let mut col = 0.0f32;
        for ch in text.chars() {
            let pan = (col / 68.0).clamp(0.0, 1.0) * 1.8 - 0.9;
            let kind = match ch {
                ' ' => SoundKind::Space,
                '\n' => SoundKind::Jump,
                _ => SoundKind::Typed,
            };
            let class = if kind == SoundKind::Typed {
                crate::trail_sound::typed_glyph_class(Some(ch))
            } else {
                0
            };
            cues.push((t, kind, pan, heat, bench_needs_shift(ch), class));
            t += dt;
            if ch == '\n' {
                col = 0.0;
                t += 0.35;
            } else {
                col += 1.0;
            }
            if ch == '.' {
                t += 0.55;
            }
        }
        t
    }

    /// The bench's prose take (`prose-10cps`, §21.4): 60 s of the corpus at
    /// 10 cps, looped, a 1.4 s think between paragraphs.
    fn bench_prose_cues() -> Vec<Cue> {
        let mut cues = Vec::new();
        let mut t = 0.5f32;
        while t < 60.0 {
            t = bench_type_text(&mut cues, t, 10.0, BENCH_PROSE, 0.55);
            t += 1.4;
        }
        cues.retain(|c| c.0 < 60.0);
        cues
    }

    /// [`push_ch`] with a flow heat on the side-car (§22).
    fn push_meta_ch(s: &mut TrailSynth, kind: SoundKind, at: u32, ch: char, flow: f32) {
        s.push_meta(
            event(kind, 0.0, ch.is_uppercase()),
            EventMeta {
                at_ms: at,
                glyph_class: crate::trail_sound::typed_glyph_class(Some(ch)),
                rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                flow,
                ..EventMeta::default()
            },
        );
    }

    /// One spawn of the prose take: the cue that minted it, its lane, its
    /// fundamental and the verse degree sounding after that cue.
    #[derive(Clone, Copy, Debug)]
    struct Spawn {
        cue: usize,
        lane: u8,
        f0: f32,
        deg: i8,
    }

    /// Drives the bench's prose corpus and cue script (the bench's seed, the
    /// music box by voice, 512-frame blocks), logging every spawn and the
    /// melody's `word_pos` after every cue.
    ///
    /// Unstamped (`at_ms` 0, `rank` 0): the block-clock fallback's pin, since
    /// A2's anti-leap law must hold on both clocks and the bench covers the
    /// stamped one. The glyph class is stamped: it is a fact about the key.
    fn drive_bench_prose(cues: &[Cue]) -> (TrailSynth, Vec<Spawn>, Vec<u8>) {
        const BLOCK: usize = 512;
        let mut s = TrailSynth::new(SR, 0x504F_4F46);
        let frames = (61.5 * SR) as usize;
        let mut stereo = vec![0.0f32; BLOCK * 2];
        let mut log = Vec::new();
        let mut word_pos = Vec::with_capacity(cues.len());
        let (mut f, mut ci) = (0usize, 0usize);
        while f < frames {
            let n = BLOCK.min(frames - f);
            let t = f as f32 / SR;
            while ci < cues.len() && cues[ci].0 <= t {
                let (ct, kind, pan, heat, shifted, glyph_class) = cues[ci];
                let mut ev = event(kind, pan, shifted);
                ev.heat = heat;
                ev.hue = (ct * 0.18).fract();
                ev.voice = SoundVoice::RainbowKittyV2;
                let mark = s.born_seq;
                s.push_meta(
                    ev,
                    EventMeta {
                        glyph_class,
                        ..EventMeta::default()
                    },
                );
                for v in since(&s, mark) {
                    log.push(Spawn {
                        cue: ci,
                        lane: v.lane,
                        f0: v.p[0].f0,
                        deg: s.v2.walk(),
                    });
                }
                word_pos.push(s.v2.word_pos());
                ci += 1;
            }
            s.render(&mut stereo[..n * 2]);
            f += n;
        }
        (s, log, word_pos)
    }

    /// Consecutive typed onsets never leap inside a word (A2), on the bench's
    /// 60 s of prose at 10 cps: an in-word onset is a unison or a stride of at
    /// most [`WORD_LEAP_MAX_DEG`]; every lead pair wider than a sixth straddles
    /// a `Jump`, and a line feed ends the word.
    #[test]
    fn consecutive_typed_onsets_are_never_a_leap_inside_a_word() {
        let cues = bench_prose_cues();
        let (s, log, word_pos) = drive_bench_prose(&cues);
        assert_eq!(s.steals(), 0, "the prose take ran the voice pool dry");

        // Inside a word: consecutive TUNE spawns minted by Typed cues with no
        // other cue between them.
        let typed: Vec<Spawn> = log
            .iter()
            .filter(|e| e.lane == LANE_TUNE && matches!(cues[e.cue].1, SoundKind::Typed))
            .copied()
            .collect();
        let mut pairs = 0usize;
        for w in typed.windows(2) {
            let (a, b) = (w[0], w[1]);
            if cues[a.cue + 1..b.cue]
                .iter()
                .any(|c| !matches!(c.1, SoundKind::Typed))
            {
                continue;
            }
            pairs += 1;
            let leap = i32::from(b.deg) - i32::from(a.deg);
            assert!(
                leap.abs() <= WORD_LEAP_MAX_DEG,
                "an in-word leap of {leap} degrees ({:.0} Hz -> {:.0} Hz) at cue {} — \
                 this is exactly v1's ghost defect",
                a.f0,
                b.f0,
                b.cue
            );
        }
        assert!(
            pairs >= 250,
            "only {pairs} in-word pairs — the take is not the bench's"
        );

        // Every lead pair wider than a sixth is the wrap or straddles a line
        // feed.
        let lead: Vec<Spawn> = log
            .iter()
            .filter(|e| matches!(e.lane, LANE_TUNE | LANE_CASCADE) && e.f0 > 0.0)
            .copied()
            .collect();
        let mut wide = 0usize;
        for w in lead.windows(2) {
            let (a, b) = (w[0], w[1]);
            let up = (b.f0 / a.f0).max(a.f0 / b.f0);
            if up <= 5.0 / 3.0 + 1e-3 {
                continue;
            }
            wide += 1;
            let wrap = a.deg == TUNE_DEG_HI as i8 && b.deg == TUNE_DEG_LO as i8;
            let line_feed = cues[a.cue..=b.cue]
                .iter()
                .any(|c| matches!(c.1, SoundKind::Jump));
            assert!(
                wrap || line_feed,
                "a lead pair {:.0} Hz -> {:.0} Hz ({up:.2}x) at cue {} is neither the form \
                 wrap nor a line feed",
                a.f0,
                b.f0,
                b.cue
            );
        }
        assert!(
            wide > 0,
            "the take has no wide pair at all — the finding is vacuous"
        );

        // A line feed ends the word: the first letter after a `Jump` is a word
        // head.
        for (i, c) in cues.iter().enumerate() {
            if !matches!(c.1, SoundKind::Jump) {
                continue;
            }
            if let Some(next) = cues[i + 1..]
                .iter()
                .position(|c| matches!(c.1, SoundKind::Typed))
            {
                assert_eq!(
                    word_pos[i + 1 + next],
                    1,
                    "the first letter after the line feed at cue {i} was not a word head"
                );
            }
        }
    }

    /// A mid-word rest never leaps by more than a sixth: the key ending it
    /// sounds within a sixth of C5, and the walk moves no further than the
    /// rest's resolution plus one derived stride.
    #[test]
    fn a_mid_word_rest_never_cadences_by_more_than_a_sixth() {
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        push(&mut s, SoundKind::Typed, 1_100, 0.0, false);
        assert_eq!(s.v2.word_pos(), 2, "fixture: two letters into a word");
        let before = s.v2.walk();
        let mark = s.born_seq;
        push(&mut s, SoundKind::Typed, 2_100, 0.0, false);
        let v = tune_voices(&since(&s, mark))[0];
        let ratio = v.p[0].f0 / penta(TINE_BASE_HZ, 0);
        assert!(
            ratio <= 5.0 / 3.0 + 1e-4,
            "the key after a mid-word rest leapt {ratio:.3}x from C5 — E6 is a tenth, \
             the ghost defect on the one key still inside the word"
        );
        assert!(
            (i32::from(s.v2.walk()) - i32::from(before)).abs()
                <= WORD_HEAD_SNAP_DEG + WORD_LEAP_MAX_DEG,
            "the rest resolved by more than the resolution plus one derived stride"
        );
    }

    // -- A4: nothing transposes the verse --------------------------------

    /// The column and the mood never move the tune (§2.6, A4): the same script
    /// at hard left, centre and hard right, under all five tones, plays the
    /// same notes.
    #[test]
    fn the_column_and_the_mood_never_move_the_tune() {
        let script = |pan: f32, tone: Tone| -> Vec<f32> {
            let mut s = synth();
            let mut out = Vec::new();
            for k in 0..24u32 {
                let mark = s.born_seq;
                let mut ev = event(SoundKind::Typed, pan, false);
                ev.tone = tone;
                s.push_meta(
                    ev,
                    EventMeta {
                        at_ms: 1_000 + k * 250,
                        ..EventMeta::default()
                    },
                );
                for v in tune_voices(&since(&s, mark)) {
                    out.push(v.p[0].f0);
                }
            }
            out
        };
        let reference = script(0.0, Tone::Technical);
        assert!(!reference.is_empty());
        for pan in [-0.9f32, 0.0, 0.9] {
            for tone in Tone::ALL {
                assert_eq!(
                    script(pan, tone),
                    reference,
                    "pan {pan} / {tone:?} moved the tune"
                );
            }
        }
    }

    // -- §3.1 / §3.3 / §3.4: the onset repairs and the timbre ---------------

    /// Shouting is a colour, not a ceiling: a run of capitals keeps a contour
    /// instead of piling onto the top degree, because the lift is a fifth and
    /// it reflects at the register bound rather than clamping. The bound is
    /// stated against the lowercase line's occupancy, so shouting must still
    /// sit above ordinary text.
    #[test]
    fn a_run_of_capitals_keeps_its_contour_instead_of_stacking_on_the_ceiling() {
        let line = |text: &str| -> Vec<i32> {
            let mut s = synth();
            let mut at = 1_000u32;
            let mut degs = Vec::new();
            for ch in text.chars() {
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(&mut s, kind, at, ch);
                if kind == SoundKind::Typed {
                    degs.push(i32::from(s.v2.walk()));
                }
                at += 110;
            }
            degs
        };
        let caps = line("THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG");
        let lower = line("the quick brown fox jumps over the lazy dog");
        assert!(caps.len() > 30, "fixture too short ({} keys)", caps.len());

        let mean = |v: &[i32]| v.iter().sum::<i32>() as f32 / v.len() as f32;
        let ceiling = caps.iter().filter(|&&d| d == TUNE_DEG_HI).count();
        assert!(
            ceiling * 4 < caps.len(),
            "{ceiling} of {} shouted keys sounded the single degree {TUNE_DEG_HI}: \
             a plateau, not a line ({caps:?})",
            caps.len()
        );
        let top = caps.iter().filter(|&&d| d >= TUNE_DEG_HI - 1).count();
        assert!(
            top * 2 < caps.len(),
            "{top} of {} shouted keys sat on the top two degrees ({caps:?})",
            caps.len()
        );
        let distinct = {
            let mut v = caps.clone();
            v.sort_unstable();
            v.dedup();
            v.len()
        };
        assert!(
            distinct >= 5,
            "a shouted line visited only {distinct} degrees ({caps:?})"
        );
        // …and it is still shouting: capitals sit above the same text typed
        // in lower case.
        assert!(
            mean(&caps) > mean(&lower),
            "shouted {:.2} vs spoken {:.2}: the lift stopped being audible",
            mean(&caps),
            mean(&lower)
        );
    }

    /// A bare Shift is a ting that never steps. For every chord × `song_key`
    /// −3..=+4, ten Shifts on a static chord: each is one [`LANE_TING`] voice
    /// with no FM and no bend, inside `[1240, 2480)` on a lattice pitch whose
    /// class is lit in the live chord; no two neighbours share a pitch, and the
    /// ten visit exactly three. Walk, steps, chord and `since_voice` are
    /// unchanged. A second Shift damps the first.
    #[test]
    fn a_bare_shift_is_a_ting_that_never_steps() {
        let fold = |mut f: f32| -> f32 {
            while f >= 2_480.0 {
                f *= 0.5;
            }
            while f < 1_240.0 {
                f *= 2.0;
            }
            f
        };
        for (chord, live) in CHORD_LOOP.iter().enumerate() {
            for key in -3i8..=4 {
                let mut heard: Vec<f32> = Vec::new();
                let mut s = synth();
                s.v2.text.chord = chord as u8;
                s.song_key = key;
                for n in 0..10u32 {
                    let mark = s.born_seq;
                    push(&mut s, SoundKind::Shift, 1_000 + n * 300, 0.0, false);
                    let lift = since(&s, mark);
                    assert_eq!(lift.len(), 1, "chord {chord} Shift {n}: one voice");
                    let v = lift[0];
                    let f = v.p[0].f0;
                    let class = (0..5)
                        .find(|&c| (fold(penta(TINE_BASE_HZ, c + i32::from(key))) - f).abs() < 0.5)
                        .unwrap_or_else(|| {
                            panic!("chord {chord} key {key} Shift {n}: {f} Hz is off the lattice")
                        });
                    assert!(
                        (1_240.0..2_480.0).contains(&f) && live.lit & (1 << class) != 0,
                        "chord {chord} key {key} Shift {n}: the ting sounded {f} Hz, class \
                         {class}, not a lit tone of {:#07b} in the ting's band",
                        live.lit
                    );
                    assert!(
                        v.p.iter()
                            .all(|p| p.fm_ratio == 0.0 && p.fm_i0 == 0.0 && p.glide == 0.0),
                        "Shift {n}: the ting wears no FM and does not bend"
                    );
                    assert_eq!(v.lane, LANE_TING, "the ting lives in its own lane");
                    heard.push(f);
                }
                assert!(
                    heard
                        .windows(2)
                        .all(|w| (w[0] - w[1]).abs() > SAME_PITCH_HZ),
                    "chord {chord} key {key}: a pitch repeated back to back in {heard:?}"
                );
                let mut distinct = heard.clone();
                distinct.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
                distinct.dedup_by(|a, b| (*a - *b).abs() < SAME_PITCH_HZ);
                assert_eq!(distinct.len(), 3, "root, fifth and third: {heard:?}");
                assert_eq!(
                    usize::from(s.v2.text.chord),
                    chord,
                    "a Shift moved the harmony"
                );
            }
        }

        // After real typing: five Shifts step nothing.
        let mut s = synth();
        type_line(&mut s, "hello ", 1_000, 150);
        let before = (
            s.v2.walk(),
            s.v2.steps(),
            s.v2.chord(),
            s.since_voice.to_bits(),
        );
        for k in 0..5u32 {
            push(&mut s, SoundKind::Shift, 1_900 + k * 300, 0.0, false);
        }
        assert_eq!(
            (
                s.v2.walk(),
                s.v2.steps(),
                s.v2.chord(),
                s.since_voice.to_bits()
            ),
            before,
            "five bare Shifts moved the walk, the step count, the chord or v1's beat — a \
             modifier never steps"
        );

        // Two shifts 40 ms apart: the second replaces the first.
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        push(&mut s, SoundKind::Shift, 1_300, 0.0, false);
        let (slot, born) = s.v2.lift.expect("the Shift left its ting's address");
        push(&mut s, SoundKind::Shift, 1_340, 0.0, false);
        let first = &s.voices[usize::from(slot)];
        assert!(
            first.on && first.born == born,
            "the first ting's slot was recycled under it"
        );
        assert_eq!(
            first.damp, LANE_FADE_STEAL_S,
            "a second Shift must damp the first ting over 12 ms — never two tings"
        );
        assert_ne!(
            s.v2.lift,
            Some((slot, born)),
            "the second Shift must take over the ting's address"
        );
    }

    /// A strike or a ring on the ting's own pitch never combs (§9.5 law 5). A
    /// searching fixture (a context line, then a Shift on each chord × cursor
    /// step, then every letter of both cases 60 ms on): a strike within a cent
    /// of the live ting damps it over [`LANE_FADE_STEAL_S`] and any other leaves
    /// it ringing; every capital that rang is typed again with the ting tuned
    /// onto its ring (natural word heads rarely collide), and then rings
    /// nothing.
    #[test]
    fn a_strike_or_a_ring_on_the_tings_pitch_never_combs() {
        let cents = |a: f32, b: f32| (1200.0 * (a / b).log2()).abs();
        let (mut strikes, mut forced) = (0usize, 0usize);
        // The ring (a twelfth, [`CAP_RING_LIFT_DEG`]) reaches the ting's
        // 1240-2480 Hz only from the verse's lowest degrees, so these lines
        // leave word heads down there.
        for ctx in ["hello ", "a ", "zoo ", "mmm tt "] {
            for chord in 0..CHORD_LOOP.len() as u8 {
                for k in 0..5u8 {
                    let mut base = synth();
                    type_line(&mut base, ctx, 1_000, 150);
                    let _ = render_mono(&mut base, 40);
                    base.v2.text.chord = chord;
                    base.shift_step = k;
                    push(&mut base, SoundKind::Shift, 2_400, 0.0, false);
                    let (slot, born) = base.v2.lift.expect("the ting's address");
                    let slot = usize::from(slot);
                    let ting_f = base.voices[slot].p[0].f0;
                    for ch in ('a'..='z').chain('A'..='Z') {
                        let mut s = base.clone();
                        let mark = s.born_seq;
                        push_ch(&mut s, SoundKind::Typed, 2_460, ch);
                        let strike = tune_voices(&since(&s, mark))[0];
                        let ting_v = s.voices[slot];
                        assert!(ting_v.on && ting_v.born == born);
                        let on_it = cents(strike.p[0].f0, ting_f) < 1.0;
                        strikes += usize::from(on_it);
                        assert_eq!(
                            ting_v.damp,
                            if on_it { LANE_FADE_STEAL_S } else { 0.0 },
                            "chord {chord} step {k} `{ch}`: a strike at {} Hz against a ting at \
                             {ting_f} Hz",
                            strike.p[0].f0
                        );
                        let Some((ring_slot, _)) = s.v2.ring else {
                            continue;
                        };
                        let ring_f = s.voices[usize::from(ring_slot)].p[0].f0;
                        let mut tuned = base.clone();
                        tuned.voices[slot].p[0].f0 = ring_f;
                        let mark = tuned.born_seq;
                        push_ch(&mut tuned, SoundKind::Typed, 2_460, ch);
                        forced += 1;
                        assert!(
                            tuned.v2.ring.is_none()
                                && !since(&tuned, mark).iter().any(|v| v.lane == LANE_GRAFT),
                            "chord {chord} step {k} `{ch}`: a ring swelled in on a live ting \
                             tuned to its own {ring_f} Hz"
                        );
                    }
                }
            }
        }
        assert!(
            strikes > 0 && forced > 0,
            "fixture: the search must reach a strike collision ({strikes}) and a ringing \
             capital ({forced})"
        );
    }

    /// The bell yields to the shifted key it announced, and to nothing else
    /// ([`TING_DUCK`]): Shift, then 60 ms later a key. An opening capital ducks
    /// the ting once without damping it; a shifted mark and `!` duck it to
    /// [`TING_DUCK_MARK`]; a lowercase letter, a digit, a Space and a run's
    /// second capital duck nothing. Rendered on its own lane against an
    /// unducked twin (`ting_unducked`), the ducked bell is exactly the duck ×
    /// the unducked one past the ramp, with no step across it.
    #[test]
    fn the_bell_yields_to_the_shifted_key_it_announced_and_to_nothing_else() {
        // → (the ting after the key, the ting's lane rendered 40 ms from the
        // key).
        let take = |ctx: &str, keys: &str, unducked: bool| -> (Voice, Vec<f32>) {
            let mut s = synth();
            s.ting_unducked = unducked;
            let mut at = 1_000u32;
            for c in ctx.chars() {
                at = type_line(&mut s, &c.to_string(), at, 150);
                let _ = render_mono(&mut s, 15);
            }
            push(&mut s, SoundKind::Shift, at, 0.0, false);
            let (slot, born) = s.v2.lift.expect("the ting's address");
            let _ = render_mono(&mut s, 6);
            for (i, c) in keys.chars().enumerate() {
                let kind = if c == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                let shifted = c.is_uppercase() || "(!".contains(c);
                s.push_meta(
                    event(kind, 0.0, shifted),
                    EventMeta {
                        at_ms: at + 60 + i as u32 * 10,
                        glyph_class: crate::trail_sound::typed_glyph_class(Some(c)),
                        rank: crate::trail_sound::typed_glyph_rank(Some(c)),
                        ..EventMeta::default()
                    },
                );
                if i + 1 < keys.len() {
                    let _ = render_mono(&mut s, 1);
                }
            }
            let v = s.voices[usize::from(slot)];
            assert!(
                v.on && v.born == born && v.damp == 0.0 && s.v2.lift == Some((slot, born)),
                "fixture ({ctx:?} + {keys:?}): the ting is live, undamped, at its address"
            );
            s.mute_all_lanes_but(LANE_TING);
            (v, render_mono(&mut s, 4))
        };
        for (ctx, keys, ducks) in [
            ("hello ", "W", Some(TING_DUCK)),
            ("", "W", Some(TING_DUCK)),
            ("he", "L", Some(TING_DUCK)),
            ("hello ", "w", None),
            ("hello ", "7", None),
            ("hello ", "(", Some(TING_DUCK_MARK)),
            ("hello ", "!", Some(TING_DUCK_MARK)),
            ("hello", " ", None),
        ] {
            let (_, x) = take(ctx, keys, false);
            let (_, x0) = take(ctx, keys, true);
            let Some(duck) = ducks else {
                assert_eq!(
                    x, x0,
                    "{ctx:?} + {keys:?}: only the capital it announced ducks"
                );
                continue;
            };
            // 20 ms on (960 samples; the ramp is 12): sample for sample.
            for (a, b) in x[960..].iter().zip(&x0[960..]) {
                assert!(
                    (a - b * duck).abs() <= 1e-6 + 1e-3 * b.abs(),
                    "{ctx:?} + {keys:?}: past the ramp the ducked bell is {a}, the unducked \
                     {b} × {duck}"
                );
            }
            let step = |x: &[f32]| x.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
            assert!(
                step(&x) <= step(&x0),
                "{ctx:?} + {keys:?}: the duck is a step ({} against the bell's own {})",
                step(&x),
                step(&x0)
            );
        }
        // Once per ting: the second capital of a run does not re-arm it.
        let (one, _) = take("hello ", "W", false);
        let (two, _) = take("hello ", "WO", false);
        assert_eq!(
            (two.pan_t0, two.gl1, two.gr1),
            (one.pan_t0, one.gl1, one.gr1),
            "a run's second capital re-armed the duck"
        );
    }

    /// The ting rings through the key it announces: rendered 160 ms past a
    /// capital, a shifted mark (every deferred graft opened), a Backspace, an
    /// arrow and a Stardust hero, it is still on, undamped, at its own address,
    /// since a push-time check cannot see an onset steal.
    #[test]
    fn the_ting_rings_through_the_next_key() {
        for (kind, ch) in [
            (SoundKind::Typed, Some('W')),
            (SoundKind::Typed, Some('?')),
            (SoundKind::Typed, Some('(')),
            (SoundKind::Typed, Some('+')),
            (SoundKind::Backspace, None),
            (SoundKind::Navigation, None),
            (SoundKind::Stardust { twinkle_hz: 9 }, None),
        ] {
            let mut s = synth();
            type_line(&mut s, "hello ", 1_000, 150);
            push(&mut s, SoundKind::Shift, 1_900, 0.0, false);
            let (slot, born) = s.v2.lift.expect("the Shift left its ting's address");
            match ch {
                Some(c) => s.push_meta(
                    event(kind, 0.0, true),
                    EventMeta {
                        at_ms: 1_960,
                        glyph_class: crate::trail_sound::typed_glyph_class(Some(c)),
                        rank: crate::trail_sound::typed_glyph_rank(Some(c)),
                        ..EventMeta::default()
                    },
                ),
                None => push(&mut s, kind, 1_960, 0.0, false),
            }
            let mut buf = [0.0f32; 960];
            for _ in 0..16 {
                s.render(&mut buf);
            }
            let v = &s.voices[usize::from(slot)];
            assert!(
                v.on && v.born == born && v.damp == 0.0 && s.v2.lift == Some((slot, born)),
                "{kind:?} {ch:?}: 160 ms on, the ting must still be on, undamped and at its \
                 own address — the 2026-09-10 resolve is the sound the owner could not hear"
            );
        }
    }

    /// A shifted key does not bend or jump an octave: the strike is the line's
    /// degree, nothing glides, every voice the key spawns is under a real
    /// filter, and only a capital letter rings (the twelfth,
    /// [`CAP_RING_LIFT_DEG`]; no shifted digit, bang, bracket or lowercase
    /// letter, and none takes flow's echo). After a phrase rest a shifted key's
    /// room taps are under a real filter too, while a plain key's keep their
    /// open roof.
    #[test]
    fn a_shifted_key_does_not_bend() {
        for ch in ['A', '7', '!', '('] {
            let mut s = synth();
            let mark = s.born_seq;
            s.push_meta(
                event(SoundKind::Typed, 0.0, true),
                EventMeta {
                    at_ms: 1_000,
                    rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                    glyph_class: crate::trail_sound::typed_glyph_class(Some(ch)),
                    flow: 1.0,
                    ..EventMeta::default()
                },
            );
            let lead = s.voices[usize::from(s.v2.lead.expect("one strike").0)];
            let line = i32::from(s.v2.walk()) + i32::from(s.song_key);
            assert_eq!(
                lead.p[0].f0,
                penta(TINE_BASE_HZ, line),
                "{ch}: the strike is the LINE's degree"
            );
            let spawned = since(&s, mark);
            for v in &spawned {
                assert_eq!(
                    v.p.map(|p| p.glide),
                    [0.0; 3],
                    "{ch}: nothing it spawns bends"
                );
                assert!(
                    v.lp_cut < 7_639.0,
                    "{ch}: a lane-{} voice is under a {} Hz roof — a bypass, not a filter",
                    v.lane,
                    v.lp_cut
                );
            }
            if ch == 'A' {
                assert!(
                    spawned.iter().any(|v| v.lane == LANE_GLINT),
                    "not vacuous: the capital's sparkle is under the clamp"
                );
                let (slot, born) =
                    s.v2.ring
                        .expect("a capital that opens a word owns its ring");
                let ring = s.voices[usize::from(slot)];
                assert_eq!((ring.born, ring.lane), (born, LANE_GRAFT));
                assert_eq!(
                    ring.p[0].f0,
                    penta(
                        TINE_BASE_HZ,
                        line + FLOW_ECHO_OCTAVE_DEG + CAP_RING_LIFT_DEG
                    ),
                    "the ring is the strike's twelfth"
                );
            } else {
                assert_eq!(s.v2.ring, None, "{ch}: a shifted MARK does not ring");
            }
            assert!(
                !s.voices
                    .iter()
                    .any(|v| v.on && v.lane == LANE_BLOOM && v.delay == FLOW_ECHO_DELAY_S),
                "{ch}: a shifted key takes no flow echo — ring or no ring"
            );
        }
        let mut plain = synth();
        push_ch(&mut plain, SoundKind::Typed, 1_000, 'w');
        assert_eq!(plain.v2.ring, None, "a lowercase letter rang");
        // The key that ends a phrase rest (`plan.rest`, so the air cloud
        // spawns): a capital after a pause keeps the room's taps under a real
        // filter; the taps are asserted present so the check cannot go vacuous.
        let after_rest = |ch: char, shifted: bool| -> Vec<Voice> {
            let mut s = synth();
            type_line(&mut s, "hello ", 1_000, 150);
            let _ = render_mono(&mut s, 200);
            let mark = s.born_seq;
            s.push_meta(
                event(SoundKind::Typed, 0.0, shifted),
                EventMeta {
                    at_ms: 5_000,
                    rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                    glyph_class: crate::trail_sound::typed_glyph_class(Some(ch)),
                    flow: if shifted { 1.0 } else { 0.0 },
                    ..EventMeta::default()
                },
            );
            since(&s, mark)
        };
        let taps = |v: &[Voice]| -> Vec<f32> {
            v.iter()
                .filter(|v| v.lane == LANE_BLOOM && AIR_TAP_DELAY_S.contains(&v.delay))
                .map(|v| v.lp_cut)
                .collect()
        };
        for ch in ['W', '7', '!', '('] {
            let spawned = after_rest(ch, true);
            if ch == 'W' {
                assert_eq!(
                    taps(&spawned).len(),
                    AIR_TAP_DELAY_S.len(),
                    "the rest's room did not answer"
                );
            }
            for v in &spawned {
                assert!(
                    v.lp_cut < 7_639.0,
                    "{ch} after a rest: a lane-{} voice (delay {}) is under a {} Hz roof — \
                     a bypass, not a filter",
                    v.lane,
                    v.delay,
                    v.lp_cut
                );
            }
        }
        // The same take unshifted keeps the room's own roof: the clamp is the
        // shifted key's alone.
        let open = taps(&after_rest('w', false));
        assert_eq!(open.len(), AIR_TAP_DELAY_S.len());
        assert!(
            open.iter().all(|c| *c > ROOF_MAX_HZ),
            "the plain key's room lost its open roof: {open:?}"
        );
    }

    #[test]
    fn all_glyph_sparkles_share_the_strikes_loudness_arc() {
        let mag = |v: &Voice| (v.gl * v.gl + v.gr * v.gr).sqrt();
        let mut attenuated = 0;
        for gap in [250u32, 125, 50] {
            // The third column is the strike's own weight, divided back out
            // below. Every key is pushed unshifted, but `!` carries the
            // letter's +2.6 dB by class, so an unshifted `!` plays the bang a
            // US Shift+1 does. The sparkle rides `g` and the velocity, never
            // the weight.
            for (ch, mul, count, weight) in [
                ('a', 1.0, 1, 1.0),
                ('7', KEY_GLINT_SHIFTED_MUL, 1, 1.0),
                (
                    '!',
                    KEY_GLINT_SHIFTED_MUL,
                    2,
                    crate::trail_sound::SHIFT_GLYPH_GAIN,
                ),
                ('"', QUOTE_GLINT_MUL, 2, 1.0),
                ('(', KEY_GLINT_SHIFTED_MUL, 1, 1.0),
            ] {
                let mut s = synth();
                push_ch(&mut s, SoundKind::Typed, 1_000, 'z');
                let _ = render_mono(&mut s, 40);
                let at = 1_000 + gap;
                let rank = crate::trail_sound::typed_glyph_rank(Some(ch));
                let class = crate::trail_sound::typed_glyph_class(Some(ch));
                let plan = s.v2.clone().on_typed(at, rank, false, false, class);
                assert_eq!(plan.touch, Touch::Step);
                let mark = s.born_seq;
                push_ch(&mut s, SoundKind::Typed, at, ch);
                let g = g_ioi(s.v2.ioi_ms * 0.001);
                attenuated += usize::from(g < 1.0);
                let voices = since(&s, mark);
                let lead = voices
                    .iter()
                    .find(|v| v.lane == LANE_TUNE)
                    .expect("one strike");
                let glints: Vec<_> = voices.iter().filter(|v| v.lane == LANE_GLINT).collect();
                assert_eq!(
                    glints.len(),
                    count,
                    "{ch}: both punctuation winks are tested"
                );
                for glint in glints {
                    // The same g and seeded velocity cancel through this key's
                    // strike. Omitting g on any glint breaks the ratio above
                    // conversational speed.
                    let ratio = mag(glint) * plan.level * weight / mag(lead);
                    let want = GLINT_LEVEL * KEY_GLINT_LEVEL_MUL * mul;
                    assert!(
                        (ratio / want - 1.0).abs() < 1e-3,
                        "{ch} at gap {gap}: {ratio} vs {want}"
                    );
                    if g < 1.0 {
                        let without_arc = ratio / g;
                        assert!(
                            (without_arc / want - 1.0).abs() > 1e-3,
                            "negative control: omitting the arc must be observable"
                        );
                    }
                }
            }
        }
        assert!(
            attenuated >= 10,
            "both fast rates must actually exercise attenuation"
        );
    }

    /// The class sentence: one key of every punctuation bucket, each a step. The
    /// rank table folds families onto one rank, so a mark right after another
    /// of its family is a re-strike; the sentence keeps a letter between marks
    /// that share a rank.
    const MARK_SENTENCE: &str =
        "Hello, World! 123 (a test)? Yes! a-b c/d e=f \"q\" [x] y{z} a<b> @# m_n";

    // ---- Phrasing from punctuation -----------------------------------------

    /// One typed key of a phrasing census: the glyph, the degree the line
    /// came from, the degree it sounded, whether it was the token's steering
    /// mark, and the chord index after it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct PhraseKey {
        ch: char,
        from: i8,
        deg: i8,
        steered: bool,
        chord: u8,
    }

    /// Types `text` through the shipping seam at one key per `period` ms and
    /// returns its typed keys. A space is a Space, `\n` an Enter, a capital is
    /// shifted. `classless` forces every class to `LETTER`: the negative
    /// control, and what an echo-born cue carries.
    fn type_phrase(
        s: &mut TrailSynth,
        text: &str,
        at: &mut u32,
        period: u32,
        classless: bool,
    ) -> Vec<PhraseKey> {
        let mut keys = Vec::new();
        for ch in text.chars() {
            let kind = match ch {
                ' ' => SoundKind::Space,
                '\n' => SoundKind::Enter { cells: 30 },
                _ => SoundKind::Typed,
            };
            let from = s.v2.walk();
            let was_marked = s.v2.token_marked();
            if classless && kind == SoundKind::Typed {
                s.push_meta(
                    event(kind, 0.0, ch.is_uppercase()),
                    EventMeta {
                        at_ms: *at,
                        glyph_class: LETTER,
                        rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                        ..EventMeta::default()
                    },
                );
            } else {
                push_ch(s, kind, *at, ch);
            }
            if kind == SoundKind::Typed {
                keys.push(PhraseKey {
                    ch,
                    from,
                    deg: s.v2.walk(),
                    steered: !was_marked && s.v2.token_marked(),
                    chord: s.v2.chord(),
                });
            }
            *at += period;
            let _ = render_mono(s, 4);
        }
        keys
    }

    /// Six sentences, every phrase mark, a dash at a word head and one inside
    /// a word, a bracket pair, and no line feed — one phrase, no rests.
    const PHRASE_CORPUS: &str = "Well, that works. Does it ring; or does it knock? \
         Yes: it rings! Wait - a well-made tie, then (an aside) and home. \
         Two, three; four: go. The end.";

    /// Punctuation phrases the line (the design's P1-P8, P14, P21, P24; P15 and
    /// P16 are `a_sentence_neither_ends_nor_starts_where_the_last_one_did`):
    /// over [`PHRASE_CORPUS`], three seeds and two rates, every steering mark
    /// lands on its cadence class within three degrees, `-` ties and `)`
    /// returns toward its `(`. Negative control (P21): with every class forced
    /// to `LETTER` the line differs and some full stop misses C; an echo-born
    /// mark is that classless key (P24).
    #[test]
    fn punctuation_phrases_the_line() {
        const SEEDS: [u32; 3] = [SEED, 0x5EED_1234, 0xCAFE_F00D];
        let mut census = [0usize; 8];
        // Which keys steer is the text's alone: one answer at every rate and
        // seed. The degrees between the marks depend on the hand too
        // (`derive`'s step 2), so one degree hash across rates is not claimed.
        let mut who_steers: Option<Vec<bool>> = None;
        for seed in SEEDS {
            for period in [250u32, 100] {
                let mut s = TrailSynth::new(SR, seed);
                let mut at = 1_000;
                let keys = type_phrase(&mut s, PHRASE_CORPUS, &mut at, period, false);
                assert_eq!(s.v2.steps() as usize, keys.len(), "one key, one step");
                let flags: Vec<bool> = keys.iter().map(|k| k.steered).collect();
                assert_eq!(
                    who_steers.get_or_insert_with(|| flags.clone()),
                    &flags,
                    "the steering marks moved with the rate or the seed ({period} ms)"
                );
                let mut paren: Option<i8> = None;
                for (i, k) in keys.iter().enumerate() {
                    let (deg, from) = (i32::from(k.deg), i32::from(k.from));
                    let pc = deg.rem_euclid(5);
                    let what = format!("`{}` (key {i}, {period} ms, seed {seed:#x})", k.ch);
                    if k.steered {
                        assert!(
                            (deg - from).abs() <= STEER_MAX_DEG,
                            "{what}: steered {from} -> {deg}, more than a fourth"
                        );
                    }
                    match k.ch {
                        '.' | ',' | ';' | ':' | '?' | '!' => {
                            assert!(k.steered, "{what}: the corpus's marks all steer");
                            let (slot, ok) = match k.ch {
                                '.' => (0, pc == 0),
                                ',' => (1, matches!(pc, 1 | 3)),
                                ';' => (2, matches!(pc, 2 | 4)),
                                ':' => (3, pc == 3),
                                '?' => (4, matches!(pc, 1 | 3) && (deg > from || from == 8)),
                                _ => (5, matches!(pc, 0 | 3)),
                            };
                            assert!(ok, "{what}: landed on degree {deg} from {from}");
                            census[slot] += 1;
                        }
                        '-' => {
                            assert_eq!(k.deg, k.from, "{what}: a dash is a tie");
                            census[6] += 1;
                        }
                        '(' => paren = Some(k.deg),
                        ')' => {
                            let to = i32::from(paren.take().expect("fixture: a `(` first"));
                            let want = from + (to - from).clamp(-4, 4);
                            assert_eq!(deg, want, "{what}: `)` returns toward {to} from {from}");
                            census[7] += 1;
                        }
                        _ => {}
                    }
                }
                // -- P21: the classless twin ---------------------------------
                let mut twin = TrailSynth::new(SR, seed);
                let mut at = 1_000;
                let plain = type_phrase(&mut twin, PHRASE_CORPUS, &mut at, period, true);
                assert!(plain.iter().all(|k| !k.steered), "a classless key steered");
                let line = |ks: &[PhraseKey]| ks.iter().map(|k| k.deg).collect::<Vec<_>>();
                assert_ne!(
                    line(&keys),
                    line(&plain),
                    "negative control: the class changed nothing ({period} ms)"
                );
                assert!(
                    plain
                        .iter()
                        .any(|k| k.ch == '.' && i32::from(k.deg).rem_euclid(5) != 0),
                    "negative control: the classless line cadences on C by itself"
                );
            }
        }
        assert!(
            census.iter().all(|n| *n >= 6),
            "fixture: a mark of the table was never measured ({census:?})"
        );

        // -- P24: an echo-born mark is a letter ------------------------------
        let mut s = synth();
        let mut at = 1_000;
        let _ = type_phrase(&mut s, "the end", &mut at, 150, false);
        let mark = s.born_seq;
        let keys = type_phrase(&mut s, ".", &mut at, 150, true);
        let born = since(&s, mark);
        assert!(!keys[0].steered && s.v2.pending_mark() == MARK_NONE);
        assert!(born.iter().all(|v| v.lane != LANE_BREATH));
    }

    /// A sentence neither ends nor starts where the last one did (the design's
    /// P15, P16). P16: behind `. ` and `! ` an uncapitalised head never repeats
    /// the closing degree; the rule binds on a head on the stop's own rank
    /// (`the end. ...`), shown by a faithful twin told no mark was said. P15:
    /// two full stops from degree 2 or 3 close on different Cs in every order,
    /// and a Backspace over the second restores the anti-drone's memory.
    #[test]
    fn a_sentence_neither_ends_nor_starts_where_the_last_one_did() {
        const LOWER: &str = "it rings. and it knocks. so it goes! on and on. then a rest. \
             and home. two. three! four. go on. the end. yes.";
        // Type `text` → (sentence heads seen, heads only the rule moved). A
        // head is the key behind a steered `.` / `!` and a Space.
        let heads_of = |s: &mut TrailSynth, text: &str, period: u32| {
            let (mut heads, mut moved) = (0usize, 0usize);
            let mut at = 1_000u32;
            let mut stop: Option<i8> = None;
            let mut spaced = false;
            for ch in text.chars() {
                if ch == ' ' || stop.is_none() || !spaced {
                    let keys = type_phrase(s, &ch.to_string(), &mut at, period, false);
                    spaced = ch == ' ';
                    if let Some(k) = keys.first() {
                        stop = (matches!(ch, '.' | '!') && k.steered).then_some(k.deg);
                    }
                    continue;
                }
                let closed = stop.take().expect("a stop");
                assert_eq!(s.v2.walk(), closed, "fixture: a Space moved the line");
                let class = crate::trail_sound::typed_glyph_class(Some(ch));
                let rank = crate::trail_sound::typed_glyph_rank(Some(ch));
                let (mut faithful, mut unruled) = (s.v2, s.v2);
                unruled.text.last_mark = MARK_NONE;
                let want = faithful.on_typed(at, rank, false, false, class).deg;
                let bare = unruled.on_typed(at, rank, false, false, class).deg;
                let _ = type_phrase(s, &ch.to_string(), &mut at, period, false);
                assert_eq!(i32::from(s.v2.walk()), want, "fixture: the twin drifted");
                assert_ne!(
                    s.v2.walk(),
                    closed,
                    "`{ch}` in {text:?} ({period} ms): the sentence starts on the note the \
                     last one ended on"
                );
                heads += 1;
                moved += usize::from(bare == i32::from(closed));
            }
            (heads, moved)
        };
        let mut heads = 0usize;
        for seed in [SEED, 0x5EED_1234, 0xCAFE_F00D] {
            for period in [250u32, 150, 100] {
                heads += heads_of(&mut TrailSynth::new(SR, seed), LOWER, period).0;
            }
        }
        assert_eq!(heads, 99, "fixture: eleven heads × nine takes");
        // And the head on the stop's own rank, from every bar of the chord loop
        // (`words` Spaces ahead of it).
        let (mut heads, mut moved) = (0usize, 0usize);
        for words in 0..8 {
            for tail in ["the end. ... and on", "wow! !! yes"] {
                let text = format!("{}{tail}", "so ".repeat(words));
                let (h, m) = heads_of(&mut synth(), &text, 150);
                (heads, moved) = (heads + h, moved + m);
            }
        }
        assert_eq!(
            (heads, moved),
            (16, 16),
            "fixture: the rule is not what moved these heads — P16 is vacuous here"
        );

        // -- P15: the anti-drone's store, through `on_typed` ------------------
        for (first, second) in [('.', '.'), ('.', '!'), ('!', '.')] {
            for a in [2i8, 3] {
                for b in [2i8, 3] {
                    let mut s = synth();
                    let mut at = 1_000u32;
                    let _ = type_phrase(&mut s, "so it goes", &mut at, 150, false);
                    s.v2.text.walk = a;
                    let one = type_phrase(&mut s, &first.to_string(), &mut at, 150, false)[0];
                    let _ = type_phrase(&mut s, " and on", &mut at, 150, false);
                    s.v2.text.walk = b;
                    let two = type_phrase(&mut s, &second.to_string(), &mut at, 150, false)[0];
                    assert!(one.steered && two.steered, "fixture: both stops steer");
                    let pcs = |k: PhraseKey, ch| match ch {
                        '.' => k.deg.rem_euclid(5) == 0,
                        _ => matches!(k.deg.rem_euclid(5), 0 | 3),
                    };
                    assert!(pcs(one, first) && pcs(two, second), "{one:?} {two:?}");
                    assert_ne!(
                        two.deg, one.deg,
                        "`{first}` from {a} then `{second}` from {b}: both close on {}",
                        one.deg
                    );
                    // …and Backspace hands the anti-drone its memory back.
                    push(&mut s, SoundKind::Backspace, at, 0.0, false);
                    at += 150;
                    let _ = render_mono(&mut s, 4);
                    assert_eq!(
                        s.v2.text.walk, b,
                        "fixture: the Backspace restored the walk"
                    );
                    let again = type_phrase(&mut s, &second.to_string(), &mut at, 150, false)[0];
                    assert_eq!(
                        again.deg, two.deg,
                        "the retyped stop forgot the last cadence"
                    );
                }
            }
        }
    }

    /// The strike one typed key spawns: its TUNE voice, through the shipping
    /// seam ([`push_ch`]).
    fn strike_of(s: &mut TrailSynth, ch: char, at: u32) -> Voice {
        let mark = s.born_seq;
        push_ch(s, SoundKind::Typed, at, ch);
        tune_voices(&since(s, mark))[0]
    }

    /// `prefix` typed, then `ch` struck on the engine and on its twin with
    /// the aside closed by hand → (level ratio, roof ratio, same pitch). The
    /// twin shares every draw, so the ratio is the aside's and nothing else.
    fn aside_vs_twin(prefix: &str, ch: char) -> (f32, f32, bool) {
        let mut s = synth();
        let mut at = 1_000;
        let _ = type_phrase(&mut s, prefix, &mut at, 150, false);
        let mut t = s.clone();
        t.v2.close_aside();
        let a = strike_of(&mut s, ch, at);
        let b = strike_of(&mut t, ch, at);
        (
            (a.gl + a.gr) / (b.gl + b.gr),
            a.lp_cut / b.lp_cut,
            a.p[0].f0 == b.p[0].f0,
        )
    }

    /// An aside is sotto voce, and it ends: inside `( [ {` the strike is −2 dB
    /// with the roof ×0.8 on the same pitch, against a twin whose aside was
    /// closed by hand; the opening bracket and the close that ends it are
    /// outside; it ends at the outer close, an Enter, a line feed, a kill, a
    /// rest and the 32nd key; a Backspace over any of those restores it.
    #[test]
    fn an_aside_is_sotto_voce_and_ends_where_it_should() {
        const DB: f32 = -2.0;
        for (prefix, ch) in [
            ("so (ab", 'c'),
            ("so (ab", '7'),
            ("so (ab", ','),
            ("x [y", 'z'),
            ("x {y", 'z'),
            ("f(g", '('),
            ("f(g(x", ')'),
        ] {
            let (lvl, roof, pitch) = aside_vs_twin(prefix, ch);
            let db = 20.0 * lvl.log10();
            assert!(
                (db - DB).abs() < 0.01,
                "{prefix:?} + `{ch}`: inside the aside the key is {db:+.3} dB re its twin"
            );
            assert!(
                (roof - ASIDE_ROOF_MUL).abs() < 1e-5,
                "{prefix:?} + `{ch}`: the aside's roof is ×{roof:.4}"
            );
            assert!(pitch, "{prefix:?} + `{ch}`: the aside moved the pitch");
        }
        for (prefix, ch) in [
            ("so", '('),
            ("so (ab", ')'),
            ("f(g(x)", ')'),
            ("so (ab)", 'c'),
            ("so ab", 'c'),
        ] {
            let (lvl, roof, pitch) = aside_vs_twin(prefix, ch);
            assert!(
                lvl == 1.0 && roof == 1.0 && pitch,
                "{prefix:?} + `{ch}` is outside the aside and was touched: ×{lvl} level, \
                 ×{roof} roof"
            );
        }
        // Every roof inside an aside is a real filter.
        let mut s = synth();
        let mut at = 1_000;
        let _ = type_phrase(&mut s, "(", &mut at, 150, false);
        for ch in "abcdefghijklmnopqrstuvwxyzAB!?".chars() {
            let v = strike_of(&mut s, ch, at);
            at += 100;
            assert!(v.lp_cut < 7639.0, "`{ch}` in an aside: roof {}", v.lp_cut);
            let _ = render_mono(&mut s, 2);
        }

        // -- Where it ends ---------------------------------------------------
        let open = |text: &str| {
            let mut s = synth();
            let mut at = 1_000;
            let _ = type_phrase(&mut s, text, &mut at, 150, false);
            assert!(s.v2.in_aside(), "fixture: {text:?} opens an aside");
            (s, at)
        };
        let (s, _) = open("f(g(x)");
        assert!(s.v2.in_aside(), "an inner close ended the outer aside");
        for (what, kind) in [
            ("an Enter", SoundKind::Enter { cells: 5 }),
            ("a line feed", SoundKind::Jump),
            ("a kill", SoundKind::Kill),
        ] {
            let (mut s, at) = open("so (ab");
            push(&mut s, kind, at, 0.0, false);
            assert!(!s.v2.in_aside(), "{what} left the aside open");
        }
        // A phrase rest: the key that breaks the silence is spoken aloud.
        let (mut s, at) = open("so (ab");
        let mut t = s.clone();
        t.v2.close_aside();
        let a = strike_of(&mut s, 'c', at + 1_500);
        let b = strike_of(&mut t, 'c', at + 1_500);
        assert!(!s.v2.in_aside(), "a rest left the aside open");
        assert_eq!(
            (a.gl, a.lp_cut),
            (b.gl, b.lp_cut),
            "the key after a rest is still sotto voce"
        );
        // The 32nd key inside ends it, and is itself the last quiet one.
        let (mut s, mut at) = open("(");
        for k in 1..=u32::from(ASIDE_MAX_KEYS) {
            assert!(s.v2.in_aside(), "the aside ended early, before key {k}");
            let _ = type_phrase(&mut s, "a", &mut at, 150, false);
        }
        assert!(!s.v2.in_aside(), "32 keys in, the aside is still open");
        // …and a Backspace over that 32nd key re-opens it, count and all;
        // retyped, it expires again.
        push(&mut s, SoundKind::Backspace, at, 0.0, false);
        at += 150;
        assert!(
            s.v2.in_aside(),
            "a Backspace over the expiry did not re-open it"
        );
        assert_eq!(s.v2.text.aside_keys, ASIDE_MAX_KEYS - 1);
        let _ = type_phrase(&mut s, "a", &mut at, 150, false);
        assert!(
            !s.v2.in_aside(),
            "retyped, the 32nd key did not end the aside"
        );

        // -- Backspace across the brackets -----------------------------------
        let (mut s, at) = open("so (ab");
        let _ = type_phrase(&mut s, ")", &mut at.clone(), 150, false);
        assert!(!s.v2.in_aside(), "fixture: `)` closes");
        push(&mut s, SoundKind::Backspace, at + 150, 0.0, false);
        assert!(
            s.v2.in_aside(),
            "un-typing the `)` did not re-open the aside"
        );
        assert_eq!(s.v2.text.aside_keys, 2, "…with its two keys");
        for _ in 0..3 {
            push(&mut s, SoundKind::Backspace, at + 300, 0.0, false);
        }
        assert!(!s.v2.in_aside(), "un-typing the `(` left the aside open");
    }

    /// A quotation lifts and comes home; an apostrophe is neither. Against a
    /// twin given the same rank as an unsteered MATH key: a quote at a word head
    /// or behind an open opens a quotation (a chord tone at or above the derived
    /// degree, two winks), the next quote closes it (toward the open's degree,
    /// one wink), and a mid-word quote with none open is an apostrophe (the
    /// derived degree). An Enter ends the quotation; a Backspace restores it.
    #[test]
    fn a_quotation_lifts_and_comes_home_and_an_apostrophe_is_neither() {
        const TEXTS: [&str; 4] = [
            "she said \"wait for me\" and left",
            "it's 'fine' he said, don't go",
            "call (\"hi\") then \"so\" ok",
            "say `ls` or 'no' now",
        ];
        let glints = |v: &[Voice]| v.iter().filter(|v| v.lane == LANE_GLINT).count();
        let (mut opens, mut lifted, mut closes, mut apostrophes) = (0, 0, 0, 0);
        for seed in [SEED, 0x5EED_1234, 0xCAFE_F00D] {
            for period in [250u32, 100] {
                for text in TEXTS {
                    let mut s = TrailSynth::new(SR, seed);
                    let mut at = 1_000u32;
                    let mut open_deg: Option<i32> = None;
                    for ch in text.chars() {
                        let class = crate::trail_sound::typed_glyph_class(Some(ch));
                        if class != QUOTE {
                            let _ = type_phrase(&mut s, &ch.to_string(), &mut at, period, false);
                            continue;
                        }
                        let rank = crate::trail_sound::typed_glyph_rank(Some(ch));
                        let from = i32::from(s.v2.walk());
                        let head = s.v2.word_pos() == 0 || s.v2.text.after_open;
                        let derived = {
                            let mut twin = s.v2;
                            twin.on_typed(at, rank, false, false, MATH).deg
                        };
                        let mark = s.born_seq;
                        push_ch(&mut s, SoundKind::Typed, at, ch);
                        let born = since(&s, mark);
                        at += period;
                        let _ = render_mono(&mut s, 4);
                        let deg = i32::from(s.v2.walk());
                        match open_deg {
                            Some(q) => {
                                let want = from + (q - from).clamp(-4, 4);
                                assert_eq!(deg, want, "{text:?}: the close did not come home");
                                assert_eq!(glints(&born), 1, "{text:?}: the close sparkles");
                                assert!(!s.v2.in_quote(), "{text:?}: the close left it open");
                                open_deg = None;
                                closes += 1;
                            }
                            None if head => {
                                assert!(
                                    deg >= derived && s.v2.deg_is_lit(deg),
                                    "{text:?}: the open quote is on {deg}, derived {derived}"
                                );
                                assert_eq!(glints(&born), 2, "{text:?}: the open winks twice");
                                assert!(s.v2.in_quote(), "{text:?}: the open did not open");
                                open_deg = Some(deg);
                                opens += 1;
                                lifted += usize::from(deg > derived);
                            }
                            None => {
                                assert_eq!(deg, derived, "{text:?}: the apostrophe was steered");
                                assert_eq!(glints(&born), 2, "{text:?}: the apostrophe's winks");
                                assert!(!s.v2.in_quote(), "{text:?}: an apostrophe opened");
                                apostrophes += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(
            closes > 0 && apostrophes > 0 && lifted > 0 && opens == closes,
            "fixture: {opens} opened ({lifted} lifted), {closes} closed, {apostrophes} apostrophes"
        );

        // -- An Enter ends it; a Backspace puts it back ------------------------
        let mut s = synth();
        let mut at = 1_000;
        let _ = type_phrase(&mut s, "say \"hi", &mut at, 150, false);
        assert!(s.v2.in_quote(), "fixture: the quotation is open");
        let open = s.v2.frame();
        let _ = type_phrase(&mut s, "\"", &mut at, 150, false);
        assert!(!s.v2.in_quote());
        push(&mut s, SoundKind::Backspace, at, 0.0, false);
        at += 150;
        assert_eq!(s.v2.frame(), open, "un-typing the close did not re-open it");
        for _ in 0..3 {
            push(&mut s, SoundKind::Backspace, at, 0.0, false);
            at += 150;
        }
        assert!(!s.v2.in_quote(), "un-typing the open left it open");
        let _ = type_phrase(&mut s, "\"hi\n", &mut at, 150, false);
        assert!(!s.v2.in_quote(), "an Enter left the quotation open");
    }

    /// A question's Return lands on the G ([`CAD_QUESTION_LIFT_DEG`]): a line
    /// whose last mark is a `?` (with or without a Space behind it, or inside a
    /// closing quote, which carries the mark) resolves its Enter on the G a
    /// fifth over the usual C, from every bar of the chord loop. `.`, `!`, a
    /// letter, `,` and the void `??` keep their C exactly.
    #[test]
    fn a_question_s_return_lands_on_the_g() {
        // → the resolution's f0.
        let enter = |text: &str, words: usize| -> f32 {
            let mut s = synth();
            let mut at = 1_000;
            for _ in 0..words {
                let _ = type_phrase(&mut s, "ab ", &mut at, 150, false);
            }
            let _ = type_phrase(&mut s, text, &mut at, 150, false);
            let mark = s.born_seq;
            push(&mut s, SoundKind::Enter { cells: 30 }, at, 0.0, false);
            let born = since(&s, mark);
            let res: Vec<_> = tune_voices(&born)
                .into_iter()
                .filter(|v| v.delay > 0.0)
                .collect();
            assert_eq!(res.len(), 1, "{text:?}: one resolution");
            res[0].p[0].f0
        };
        for words in 0..CHORD_LOOP.len() {
            let home = enter("so it goes.", words);
            for text in ["is it so?", "is it so? ", "he said \"is it so?\""] {
                let f = enter(text, words);
                let g = [CAD_RESOLUTION_LOW_DEG, CAD_RESOLUTION_HIGH_DEG]
                    .map(|d| penta(TINE_BASE_HZ, d + CAD_QUESTION_LIFT_DEG));
                assert!(
                    g.contains(&f),
                    "{text:?} after {words} words: the Return landed on {f:.1} Hz, not a G"
                );
                assert!(
                    (f / home - 1.5).abs() < 1e-4,
                    "{text:?}: the G is not the fifth over the C the full stop took"
                );
            }
            for text in [
                "so it goes.",
                "wow it goes!",
                "so it goes",
                "so it goes,",
                "is it so??",
            ] {
                let f = enter(text, words);
                assert_eq!(
                    f, home,
                    "{text:?} after {words} words: a Return with no question moved"
                );
                assert!(
                    [CAD_RESOLUTION_LOW_DEG, CAD_RESOLUTION_HIGH_DEG]
                        .map(|d| penta(TINE_BASE_HZ, d))
                        .contains(&f),
                    "{text:?}: the Return is not on a C"
                );
            }
        }
    }

    /// The question never rises a wolf, in any key ([`QUEST_RISE_D_DEG`]). For
    /// every `song_key` and a `?` steered from every degree, the rise over its
    /// pluck is a pure fourth (from the sounding D), a pure fifth, or E's pure
    /// minor sixth to C (8/5), never the 40/27.
    #[test]
    fn the_question_never_rises_a_wolf_in_any_key() {
        let mut fourths = 0;
        for key in -3i8..=4 {
            for from in TUNE_DEG_LO..=TUNE_DEG_HI {
                let mut s = synth();
                let mut at = 1_000u32;
                let _ = type_phrase(&mut s, "is it so", &mut at, 150, false);
                s.song_key = key;
                s.v2.text.walk = from as i8;
                let mark = s.born_seq;
                let k = type_phrase(&mut s, "?", &mut at, 150, false)[0];
                assert!(k.steered, "fixture: the `?` steers");
                assert_eq!(s.song_key, key, "fixture: the key was handed back");
                let born = since(&s, mark);
                let t = tune_voices(&born)[0];
                let g: Vec<_> = born.iter().filter(|v| v.lane == LANE_GRAFT).collect();
                assert_eq!(g.len(), 1, "one rise");
                let ratio = g[0].p[0].f0 / t.p[0].f0;
                let pure = [4.0 / 3.0, 1.5f32, 1.6]
                    .iter()
                    .any(|p| (ratio / p - 1.0).abs() < 1e-4);
                assert!(
                    pure,
                    "key {key}, `?` on degree {}: the rise is {ratio:.4} — not 4/3, 3/2 or 8/5",
                    k.deg
                );
                fourths += usize::from(ratio < 1.4);
            }
        }
        assert!(fourths > 0, "fixture: no `?` ever sounded a D");
    }

    /// A mark confirms its cadence in the Space bass (the design's P9, P10, P13,
    /// P17, P18): from every bar of the loop the Space behind `.` `!`, `,` `:`
    /// `?` or `;` advances to the next chord of that mark's function (bass C4,
    /// G4 or A4), a letter and the void `...` take `chord + 1`, the subject
    /// re-latches behind `.` `!` `?` but not `,`, and a Return behind a full stop
    /// is a codetta (no pickup).
    #[test]
    fn a_mark_confirms_its_cadence_in_the_space_bass() {
        const C4: f32 = 261.63;
        const G4: f32 = 392.445;
        const A4: f32 = 436.05;
        let n = CHORD_LOOP.len() as u8;
        let root_of = |c: u8| BASS_BASE_HZ * CHORD_ROOT_RATIO[CHORD_LOOP[usize::from(c)].root];
        for words in 0..n {
            for (tail, want_hz, want_ix) in [
                ("cat.", Some(C4), Some([0u8, 4])),
                ("cat!", Some(C4), Some([0, 4])),
                ("cat,", Some(G4), Some([3, 5])),
                ("cat:", Some(G4), Some([3, 5])),
                ("cat?", Some(G4), Some([3, 5])),
                ("cat;", Some(A4), Some([1, 6])),
                ("cat", None, None),
                ("cat...", None, None),
                ("cat??", None, None),
            ] {
                let mut s = synth();
                let mut at = 1_000;
                // Walk the loop to a different chord in every outer pass.
                for _ in 0..words {
                    let _ = type_phrase(&mut s, "ab ", &mut at, 150, false);
                }
                let _ = type_phrase(&mut s, tail, &mut at, 150, false);
                let before = s.v2.chord();
                let mark = s.born_seq;
                push_ch(&mut s, SoundKind::Space, at, ' ');
                let after = s.v2.chord();
                let bass: Vec<Voice> = since(&s, mark).into_iter().filter(|v| v.bass).collect();
                assert_eq!(bass.len(), 1, "`{tail} `: one downbeat");
                let got = bass[0].p[0].f0;
                assert!(
                    (got - root_of(after)).abs() < 0.5,
                    "`{tail} `: the bass is {got} Hz, not chord {after}'s root"
                );
                let advanced = (after + n - before - 1) % n + 1;
                assert!((1..=n).contains(&advanced), "P10");
                match (want_hz, want_ix) {
                    (Some(hz), Some(ix)) => {
                        assert!(
                            (got - hz).abs() < 0.5,
                            "`{tail} ` from chord {before}: the bass is {got} Hz, not {hz}"
                        );
                        assert!(ix.contains(&after), "`{tail} `: chord {after}");
                        // The next one forward: nothing of that function
                        // was stepped over.
                        assert!(
                            (1..advanced).all(|k| !ix.contains(&((before + k) % n))),
                            "`{tail} ` from chord {before} skipped a chord of its own function"
                        );
                    }
                    _ => {
                        assert_eq!(advanced, 1, "`{tail} `: an ordinary advance");
                        assert_eq!(s.v2.pending_mark(), MARK_NONE, "`{tail}`");
                    }
                }
            }
        }

        // -- P17: the subject -------------------------------------------------
        for (mark, relatched) in [('.', true), ('!', true), ('?', true), (',', false)] {
            let mut s = synth();
            let mut at = 1_000;
            let _ = type_phrase(&mut s, "the quick brown fox", &mut at, 150, false);
            assert_eq!(
                usize::from(s.v2.text.motif_len),
                MOTIF_LEN,
                "fixture: a subject"
            );
            let _ = type_phrase(&mut s, &format!("{mark} "), &mut at, 150, false);
            assert_eq!(
                s.v2.text.motif_len == 0,
                relatched,
                "`{mark} `: the subject's latch"
            );
        }

        // -- P18: the codetta -------------------------------------------------
        for tail in ["home.", "home. ", "home!"] {
            let mut closed = synth();
            let mut at = 1_000;
            let _ = type_phrase(&mut closed, "we are ", &mut at, 150, false);
            let _ = type_phrase(&mut closed, tail, &mut at, 150, false);
            // The twin: the same synth, to the bit, with the mark forgotten.
            let mut open = closed.clone();
            open.v2.text.last_mark = MARK_NONE;
            let enter = |s: &mut TrailSynth| -> usize {
                let mark = s.born_seq;
                push(s, SoundKind::Enter { cells: 30 }, at, 0.0, false);
                tune_voices(&since(s, mark)).len()
            };
            assert_eq!(
                (enter(&mut open), enter(&mut closed)),
                (2, 1),
                "`{tail}`+Enter: the pickup and the resolution, against the resolution alone"
            );
        }
    }

    /// Code punctuation never moves the harmony (the design's P11, P12): a
    /// token steers at most once, a decimal point never does, and a mark that is
    /// not the last thing typed before the Space books nothing.
    #[test]
    fn code_punctuation_never_moves_the_harmony() {
        for (token, steered_want) in [
            ("self.v2.walk.foo.bar.baz", 1usize),
            ("3.14", 0),
            ("v0.88.0", 0),
            ("std::io::Result", 1),
            ("a.b,c;d:e", 1),
        ] {
            let mut s = synth();
            let mut at = 1_000;
            let _ = type_phrase(&mut s, "let x = ", &mut at, 110, false);
            let chord = s.v2.chord();
            let keys = type_phrase(&mut s, token, &mut at, 110, false);
            assert_eq!(
                keys.iter().filter(|k| k.steered).count(),
                steered_want,
                "`{token}`: steering marks"
            );
            assert!(
                keys.iter().all(|k| k.chord == chord),
                "`{token}`: a mark inside a token moved the chord"
            );
            // No three mark notes in a row on one pitch.
            let marks: Vec<i8> = keys
                .iter()
                .filter(|k| !k.ch.is_ascii_alphanumeric())
                .map(|k| k.deg)
                .collect();
            assert!(
                marks.windows(3).all(|w| !(w[0] == w[1] && w[1] == w[2])),
                "`{token}`: three mark notes share a pitch ({marks:?})"
            );
            push_ch(&mut s, SoundKind::Space, at, ' ');
            assert_eq!(
                s.v2.chord(),
                (chord + 1) % CHORD_LOOP.len() as u8,
                "`{token} `: the Space behind a letter or a digit is an ordinary advance"
            );
        }
    }

    /// Backspace un-sings a phrase (the design's P19): for every class id and an
    /// unnamed one, plain and shifted, and for a Space, "type it, delete it"
    /// leaves [`MelodyV2::frame`] unchanged, and `ab. c` deleted back to `a` and
    /// retyped plays the same degrees onto the same chord. [`SECRET`] moves no
    /// melody and is excluded. Negative control: a typed mark with no Backspace
    /// leaves a different frame.
    #[test]
    fn backspace_unsings_a_phrase() {
        for class in (0..=15u8).chain([17]) {
            for shifted in [false, true] {
                let mut s = synth();
                let mut at = 1_000;
                let _ = type_phrase(&mut s, "so (ab", &mut at, 150, false);
                let before = s.v2.frame();
                s.push_meta(
                    event(SoundKind::Typed, 0.0, shifted),
                    EventMeta {
                        at_ms: at,
                        glyph_class: class,
                        rank: 37,
                        ..EventMeta::default()
                    },
                );
                assert_ne!(s.v2.frame(), before, "class {class}: the control is blind");
                push(&mut s, SoundKind::Backspace, at + 150, 0.0, false);
                assert_eq!(
                    s.v2.frame(),
                    before,
                    "class {class} (shifted {shifted}): typed and deleted, the phrase moved"
                );
            }
        }
        // A Space behind a mark: the chord it booked comes back too.
        let mut s = synth();
        let mut at = 1_000;
        let _ = type_phrase(&mut s, "ab;", &mut at, 150, false);
        let before = s.v2.frame();
        push_ch(&mut s, SoundKind::Space, at, ' ');
        assert_ne!(s.v2.frame().chord, before.chord);
        push(&mut s, SoundKind::Backspace, at + 150, 0.0, false);
        assert_eq!(
            s.v2.frame(),
            before,
            "a deleted Space left its cadence behind"
        );

        // `ab. c`, four Backspaces, and again.
        let mut s = synth();
        let mut at = 1_000;
        let _ = type_phrase(&mut s, "a", &mut at, 150, false);
        let home = s.v2.frame();
        let first = type_phrase(&mut s, "b. c", &mut at, 150, false);
        let chord = s.v2.chord();
        for _ in 0..4 {
            push(&mut s, SoundKind::Backspace, at, 0.0, false);
            at += 150;
        }
        assert_eq!(s.v2.frame(), home, "four Backspaces did not reach `a`");
        let again = type_phrase(&mut s, "b. c", &mut at, 150, false);
        let line = |ks: &[PhraseKey]| ks.iter().map(|k| (k.deg, k.steered)).collect::<Vec<_>>();
        assert_eq!(
            line(&again),
            line(&first),
            "the retyped phrase is another tune"
        );
        assert_eq!(
            s.v2.chord(),
            chord,
            "the retyped phrase is on another chord"
        );
        assert!(first[1].steered, "fixture: the full stop steered");
    }

    /// An unnamed class id is the letter. A host may stamp an id this engine
    /// does not name (`17`, `18`, `200`, `255`; `16` is [`SECRET`]): on
    /// identical engine state and side-car but for the id, every unnamed id
    /// renders one output bit for bit and never breathes, and unshifted it is
    /// the letter's output exactly. Negative control: a full stop renders
    /// apart from the letter, so "equal" is not the comparison's only answer.
    #[test]
    fn an_unnamed_class_id_is_still_the_letter() {
        // Three letters, the mark, the mark again (its re-strike), a letter.
        // Everything but `class` is identical across takes, the rank included.
        let rank = crate::trail_sound::typed_glyph_rank(Some('.'));
        // -> (the lanes the two marks spawned, the output's bits).
        let take = |class: u8, shifted: bool| -> (Vec<u8>, Vec<u32>) {
            let mut s = synth();
            let mut out = Vec::new();
            let mut at = 1_000;
            for ch in "hel".chars() {
                push_ch(&mut s, SoundKind::Typed, at, ch);
                out.extend(render_mono(&mut s, 15));
                at += 150;
            }
            let mut lanes = Vec::new();
            for _ in 0..2 {
                let mark = s.born_seq;
                s.push_meta(
                    event(SoundKind::Typed, 0.0, shifted),
                    EventMeta {
                        at_ms: at,
                        glyph_class: class,
                        rank,
                        ..EventMeta::default()
                    },
                );
                lanes.extend(since(&s, mark).iter().map(|v| v.lane));
                out.extend(render_mono(&mut s, 15));
                at += 150;
            }
            push_ch(&mut s, SoundKind::Typed, at, 'o');
            out.extend(render_mono(&mut s, 60));
            (lanes, out.iter().map(|x| x.to_bits()).collect())
        };
        for shifted in [false, true] {
            let letter = take(LETTER, shifted);
            assert!(
                take(STOP, shifted).1 != letter.1,
                "negative control: a full stop rendered as the letter"
            );
            // Shifted, an unnamed id is not quite the letter: [`Forte::of`]
            // reads "a capital" as `shifted && class == LETTER`, so it gets a
            // shifted mark's force on the letter's tine.
            let first = take(17, shifted);
            for unknown in [17u8, 18, 200, 255] {
                let got = take(unknown, shifted);
                assert!(
                    got.1 == first.1,
                    "class {unknown} (shifted {shifted}): an output of its own"
                );
                assert!(
                    !got.0.contains(&LANE_BREATH),
                    "class {unknown} (shifted {shifted}) breathed"
                );
                if !shifted {
                    assert!(
                        got.1 == letter.1,
                        "class {unknown}: not the letter's output"
                    );
                }
            }
        }
    }

    /// The keys a password might be made of, one of every class the keyboard
    /// stamps: letters of both cases, digits, every mark bucket, and the space.
    /// `(kind, shifted, rank)` is what the keyed seam sends for each; only the
    /// class is replaced by [`SECRET`].
    const SECRET_KEYS: &str = "aZ7.,;:-([)]'\"_~/+=@#?! ";

    /// One secret-stamped cue for `ch`, carrying everything its ordinary cue
    /// would (kind, shift, rank, a moving pan) except the class.
    fn push_secret(s: &mut TrailSynth, at: u32, ch: char, pan: f32) {
        let kind = if ch == ' ' {
            SoundKind::Space
        } else {
            SoundKind::Typed
        };
        s.push_meta(
            event(
                kind,
                pan,
                ch.is_uppercase() || "!?\"(){}_~+@#:".contains(ch),
            ),
            EventMeta {
                at_ms: at,
                glyph_class: SECRET,
                rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                ..EventMeta::default()
            },
        );
    }

    /// A line with a walk, a chord, a mark and an IOI in it, rendered out —
    /// what a secret must not disturb. Returns the next free `at_ms`.
    fn prose_prelude(s: &mut TrailSynth) -> u32 {
        let mut at = 1_000;
        for ch in "Hello, wor".chars() {
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            push_ch(s, kind, at, ch);
            let _ = render_mono(s, 15);
            at += 150;
        }
        at
    }

    /// A password key is one tone, whatever it was: on one engine state, one
    /// [`SECRET`] cue per key of [`SECRET_KEYS`] spawns the same single voice,
    /// field for field, and renders the same output bit for bit, whatever kind,
    /// shift, rank or pan it carried.
    ///
    /// Negative control: the same keys with their ordinary classes are told
    /// apart by the same comparison.
    #[test]
    fn a_password_key_is_one_tone_whatever_it_was() {
        let take = |ch: char, secret: bool| -> (Vec<String>, Vec<u32>) {
            let mut s = synth();
            let at = prose_prelude(&mut s);
            let mark = s.born_seq;
            if secret {
                // The pan moves with the key, as a caret would — the tone
                // must not.
                push_secret(&mut s, at, ch, (ch as u32 % 7) as f32 / 7.0 - 0.4);
            } else {
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(&mut s, kind, at, ch);
            }
            let voices = since(&s, mark)
                .iter()
                .map(|v| format!("{v:?}"))
                .collect::<Vec<_>>();
            let out = render_mono(&mut s, 40);
            (voices, out.iter().map(|x| x.to_bits()).collect())
        };
        let first = take('a', true);
        assert_eq!(first.0.len(), 1, "one key, one tone: {:?}", first.0);
        for ch in SECRET_KEYS.chars() {
            let got = take(ch, true);
            assert_eq!(got.0, first.0, "`{ch}`: a voice of its own at the prompt");
            assert!(
                got.1 == first.1,
                "`{ch}`: an output of its own at the prompt"
            );
        }
        // Negative control: unstamped, the keys are their ordinary selves.
        let plain: Vec<_> = ['a', 'Z', '7', '.', ' ']
            .iter()
            .map(|&ch| take(ch, false))
            .collect();
        for (i, a) in plain.iter().enumerate() {
            assert!(
                a.1 != first.1,
                "key {i}: its ordinary click RENDERED as the password tone"
            );
            for b in &plain[i + 1..] {
                assert!(
                    a.1 != b.1,
                    "two ordinary keys rendered alike: the control is blind"
                );
            }
        }
    }

    /// A password leaves the melody where it was: after a whole password
    /// [`MelodyV2`] is field for field the state before it, and the next key
    /// derives as if nothing was typed (the secret's four draws aside, so the
    /// comparison hands over the untouched take's stream). Negative control:
    /// the same keys unstamped move the line.
    #[test]
    fn a_password_leaves_the_melody_where_it_was() {
        let mut with = synth();
        let at0 = prose_prelude(&mut with);
        let mut without = with.clone();
        let before = format!("{:?}", with.v2);
        let mut at = at0;
        for ch in SECRET_KEYS.chars() {
            push_secret(&mut with, at, ch, 0.0);
            let _ = render_mono(&mut with, 3);
            at += 120;
        }
        assert_eq!(
            format!("{:?}", with.v2),
            before,
            "a password moved the melody's state"
        );
        let _ = render_mono(&mut with, 60);
        let _ = render_mono(&mut without, 60 + 3 * SECRET_KEYS.chars().count());
        with.rng = without.rng;
        // The next ordinary key, on both.
        let next = |s: &mut TrailSynth| -> (String, Vec<String>) {
            let mark = s.born_seq;
            push_ch(s, SoundKind::Typed, at, 'l');
            let voices = since(s, mark)
                .into_iter()
                .map(|mut v| {
                    v.born = 0;
                    format!("{v:?}")
                })
                .collect();
            (format!("{:?}", s.v2), voices)
        };
        let (a, b) = (next(&mut with), next(&mut without));
        assert_eq!(
            a.1, b.1,
            "the key after the password is not the key it would have been"
        );
        assert!(!a.1.is_empty());
        // Negative control: the same keys, unstamped, move the line.
        let mut typed = synth();
        let mut at = prose_prelude(&mut typed);
        let before = format!("{:?}", typed.v2);
        for ch in SECRET_KEYS.chars() {
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            push_ch(&mut typed, kind, at, ch);
            at += 120;
        }
        assert_ne!(format!("{:?}", typed.v2), before, "the control is blind");
    }

    /// A re-struck mark does not spark or graft again (§9.2): in `!!!!`, `....`,
    /// `((((` and `????` the first key asks, opens or winks and each repeat is
    /// one step with no sparkle and no graft; only the steering first dot
    /// breathes and books a cadence. The run deliberately crosses into
    /// autorepeat (the felt mallet alone, still not silent).
    #[test]
    fn a_re_struck_mark_does_not_spark_or_graft_again() {
        const PERIOD_MS: u32 = 125;
        let lane = |vs: &[Voice], l: u8| -> Vec<Voice> {
            vs.iter().filter(|v| v.lane == l).copied().collect()
        };
        let mut felt_keys = 0usize;
        for run in ["!!!!", "....", "((((", "????"] {
            let mut s = synth();
            type_line(&mut s, "hello ", 1_000, 150);
            let _ = render_mono(&mut s, 12);
            for (i, ch) in run.chars().enumerate() {
                let mark = s.born_seq;
                push_ch(&mut s, SoundKind::Typed, 1_900 + i as u32 * PERIOD_MS, ch);
                let vs = since(&s, mark);
                let class = crate::trail_sound::typed_glyph_class(Some(ch));
                assert_eq!(
                    lane(&vs, LANE_TUNE).len(),
                    1,
                    "`{run}` key {i}: one key is one step, re-strike or not"
                );
                let grafts = lane(&vs, LANE_GRAFT).len();
                let glints = lane(&vs, LANE_GLINT).len();
                // A felt key (the ladder's auto-repeat rung) has no pitch.
                felt_keys += usize::from(lane(&vs, LANE_TUNE)[0].p[0].lvl == 0.0);
                if i == 0 {
                    assert!(glints >= 1, "the first `{ch}` did not sparkle");
                    let wanted = usize::from(matches!(class, QMARK | OPEN));
                    assert_eq!(
                        grafts, wanted,
                        "the first `{ch}` grafted {grafts} voices, not {wanted}"
                    );
                } else {
                    assert_eq!(glints, 0, "a re-struck `{ch}` sparkled again");
                    assert_eq!(grafts, 0, "a re-struck `{ch}` grafted again");
                }
                if stop_breathes(class) {
                    let want = usize::from(i == 0);
                    assert_eq!(
                        lane(&vs, LANE_BREATH).len(),
                        want,
                        "`{run}` key {i}: only the steering stop breathes"
                    );
                    assert_eq!(
                        s.v2.pending_mark(),
                        if i == 0 { MARK_PERIOD } else { MARK_NONE },
                        "`{run}` key {i}: only the steering stop books a cadence"
                    );
                }
                let _ = render_mono(&mut s, 12);
            }
        }
        assert!(
            felt_keys > 0,
            "fixture: a machine-regular run never reached the ladder's felt rung"
        );
    }

    /// An adjacent pair closes with its tink (§10.4). `(` and `)` share a rank,
    /// so a `)` right after its `(` lands on the same degree and is a
    /// re-strike, but the re-strike gate is for a mark struck again (`((`,
    /// `??`); the tink's direction already tells open from close.
    ///
    /// Control: a close after a close is still a re-struck mark and grafts
    /// nothing, so `))` does not tink twice.
    #[test]
    fn an_adjacent_pair_closes_with_its_tink() {
        let lane = |vs: &[Voice], l: u8| -> Vec<Voice> {
            vs.iter().filter(|v| v.lane == l).copied().collect()
        };
        for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
            let mut s = synth();
            type_line(&mut s, "hello ", 1_000, 150);
            let _ = render_mono(&mut s, 12);
            push_ch(&mut s, SoundKind::Typed, 1_900, open);
            let _ = render_mono(&mut s, 12);
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Typed, 2_040, close);
            let vs = since(&s, mark);
            assert!(
                s.v2.text.restrike > 0,
                "fixture: `{open}{close}` — the close did not land on its open's degree"
            );
            let g = lane(&vs, LANE_GRAFT);
            assert_eq!(
                g.len(),
                1,
                "`{open}{close}`: the close grafted {} tinks",
                g.len()
            );
            let key = i32::from(s.song_key);
            let want = penta(
                TINE_BASE_HZ,
                reflect_deg(i32::from(s.v2.walk()) - TINK_DEG) + key,
            );
            assert!(
                (g[0].p[0].f0 - want).abs() < SAME_PITCH_HZ,
                "`{open}{close}`: the tink is at {} Hz, not one degree DOWN at {want}",
                g[0].p[0].f0
            );
            let _ = render_mono(&mut s, 12);
            // Control: a second close is a re-struck mark.
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Typed, 2_180, close);
            let vs = since(&s, mark);
            assert!(
                s.v2.text.restrike > 0,
                "fixture: `{close}{close}` — the second close did not re-strike"
            );
            assert!(
                lane(&vs, LANE_GRAFT).is_empty(),
                "`{close}{close}`: a re-struck close tinked again"
            );
        }
    }

    /// Real shifted US punctuation metadata, including a queued burst that
    /// outruns the renderer: every key keeps its TUNE voice after all of its
    /// decorations are admitted.
    #[test]
    fn shifted_marks_never_steal_their_own_strike_in_a_batched_burst() {
        let mut s = synth();
        let mut full_pool_keys = 0;
        for (index, ch) in "Aa!b?c(D)e{F}g<H>I+J=K\"L:M~N_O|P"
            .chars()
            .cycle()
            .take(512)
            .enumerate()
        {
            let shifted = ch.is_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(ch);
            s.push_meta(
                event(SoundKind::Typed, 0.0, shifted),
                EventMeta {
                    at_ms: 1_000 + index as u32 * 93,
                    glyph_class: crate::trail_sound::typed_glyph_class(Some(ch)),
                    rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                    ..EventMeta::default()
                },
            );
            let (slot, born) = s.v2.lead.expect("every key admits its strike");
            let lead = s.voices[usize::from(slot)];
            assert!(
                lead.on && lead.born == born && lead.lane == LANE_TUNE,
                "key {index} ({ch:?}, shifted={shifted}) lost its strike to a decoration"
            );
            full_pool_keys += usize::from(s.voices.iter().all(|voice| voice.on));
        }
        assert!(
            full_pool_keys > 0,
            "the burst must exercise pool exhaustion"
        );
    }

    fn graft_retirement_model() -> aterm_spec::derive::Model {
        // `same` means that the deletion still owns this generation: a
        // recycled slot or a later text key both revoke that ownership.
        aterm_spec::ty_model! {
            KeyGraftRetirement {
                const Buggy = 0;
                var on = 1;
                var sounded = 0;
                var damped = 0;
                var same = 1;
                var done = 0;
                action Sound when (done == 0 && sounded == 0) { sounded = 1; }
                action Disown when (done == 0 && same == 1) { same = 0; }
                action Retire when (done == 0) {
                    done = 1;
                    on = if same == 1 && sounded == 0 && Buggy == 0 { 0 } else { on };
                    damped = if same == 1 && sounded == 1 && Buggy == 0 { 1 } else { damped };
                }
                invariant OwnedVoiceRetires: done == 0 || same == 0 ||
                    (sounded == 0 && on == 0) || (sounded == 1 && on == 1 && damped == 1);
                invariant UnownedVoiceSurvives: same == 1 || (on == 1 && damped == 0);
                invariant Bounds: on <= 1 && sounded <= 1 && damped <= 1 && same <= 1 && done <= 1;
            }
        }
    }

    #[test]
    fn graft_retirement_model_proves_and_catches_an_omitted_delete() {
        let model = graft_retirement_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    /// Binds the model to voices created by the real typed-key path and advanced
    /// by the sample renderer. Current addresses retire through Backspace, Kill
    /// and KillWord; stale addresses exercise the guarded retirement seam.
    /// Covers every pitched graft and the capital ring.
    #[test]
    fn typed_grafts_conform_to_owned_retirement_before_and_after_onset() {
        let model = graft_retirement_model();
        let mut negative_controls = 0;
        for deletion in [SoundKind::Backspace, SoundKind::Kill, SoundKind::KillWord] {
            for ch in ['?', '(', ')', '=', 'W'] {
                for sounded in [false, true] {
                    for stale in [false, true] {
                        let mut s = synth();
                        type_line(&mut s, "hello ", 1_000, 150);
                        let _ = render_mono(&mut s, 12);
                        push_ch(&mut s, SoundKind::Typed, 1_900, ch);
                        let (slot, born) = if ch == 'W' { s.v2.ring } else { s.v2.graft }
                            .expect("the typed key produced its pitched decoration");
                        let slot = usize::from(slot);
                        assert!(s.voices[slot].on && s.voices[slot].t < 0.0);
                        let mut expected = model.init_state();
                        // Exercise retirement close to onset too: a damp alone
                        // would let the 30 ms fifth start during its 40 ms ramp.
                        let _ = render_mono(&mut s, if sounded { 10 } else { 2 });
                        if sounded {
                            assert!(s.voices[slot].on && s.voices[slot].t >= 0.0);
                            assert!(model.fire("Sound", &mut expected));
                        } else {
                            assert!(s.voices[slot].on && s.voices[slot].t < 0.0);
                        }
                        assert_eq!(s.voices[slot].born, born);
                        assert_eq!(s.voices[slot].damp, 0.0);
                        let before = expected.clone();
                        if stale {
                            assert!(model.fire("Disown", &mut expected));
                            s.v2_retire(Some((slot as u8, born.wrapping_sub(1))), ERASE_MUTE_S);
                        } else {
                            push(&mut s, deletion, 2_000, 0.0, false);
                            assert!(s.v2.ring.is_none() && s.v2.graft.is_none());
                        }
                        assert!(model.fire("Retire", &mut expected));
                        let v = s.voices[slot];
                        // An unheard cancelled slot may already hold the
                        // deletion's poof, but none of the retired generation.
                        let owned_alive = v.on && v.born == born;
                        assert_eq!(i64::from(owned_alive), expected["on"]);
                        assert_eq!(i64::from(owned_alive && v.damp > 0.0), expected["damped"]);
                        assert!(model.check_invariant("OwnedVoiceRetires", &expected));
                        assert!(model.check_invariant("UnownedVoiceSurvives", &expected));
                        if !stale {
                            let mut missing_retire = before;
                            missing_retire.insert("done", 1);
                            assert!(!model.check_invariant("OwnedVoiceRetires", &missing_retire));
                            negative_controls += 1;
                            if !sounded {
                                let _ = render_mono(&mut s, 10);
                                let later = s.voices[slot];
                                assert!(
                                    !later.on || later.born != born,
                                    "{deletion:?} let an unheard {ch:?} graft start later"
                                );
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(negative_controls, 30);
    }

    /// A later plain key, space, Enter or paste releases deletion ownership of
    /// an older decoration without muting it; driven through real input and
    /// Backspace on both sides of the sample-clock onset.
    #[test]
    fn later_text_keys_disown_the_previous_keys_ring_and_graft() {
        let model = graft_retirement_model();
        let mut negative_controls = 0;
        for ch in ['?', 'W'] {
            for sounded in [false, true] {
                for next in [
                    SoundKind::Typed,
                    SoundKind::Space,
                    SoundKind::Enter { cells: 1 },
                    SoundKind::Strum,
                ] {
                    let mut s = synth();
                    type_line(&mut s, "hello ", 1_000, 150);
                    let _ = render_mono(&mut s, 12);
                    push_ch(&mut s, SoundKind::Typed, 1_900, ch);
                    let address = if ch == 'W' { s.v2.ring } else { s.v2.graft }
                        .expect("the earlier key owns a decoration");
                    let (slot, born) = (usize::from(address.0), address.1);
                    let mut expected = model.init_state();
                    if sounded {
                        let _ = render_mono(&mut s, 10);
                        assert!(model.fire("Sound", &mut expected));
                    }
                    assert_eq!(s.voices[slot].t >= 0.0, sounded);
                    assert!(s.voices[slot].on && s.voices[slot].born == born);
                    assert_eq!(s.voices[slot].damp, 0.0);

                    push_ch(&mut s, next, 2_100, 'b');
                    assert!(model.fire("Disown", &mut expected));
                    let owns = s.v2.ring == Some(address) || s.v2.graft == Some(address);
                    assert_eq!(
                        i64::from(owns),
                        expected["same"],
                        "{next:?} kept {ch:?}'s address"
                    );
                    push(&mut s, SoundKind::Backspace, 2_120, 0.0, false);
                    assert!(model.fire("Retire", &mut expected));
                    let v = s.voices[slot];
                    assert_eq!(i64::from(v.on && v.born == born), expected["on"]);
                    assert_eq!(i64::from(v.damp > 0.0), expected["damped"]);

                    // Negative control: deleting the later key must not
                    // cancel/ramp this earlier generation as though owned.
                    let mut wrong_owner = expected;
                    wrong_owner.insert(if sounded { "damped" } else { "on" }, i64::from(sounded));
                    assert!(!model.check_invariant("UnownedVoiceSurvives", &wrong_owner));
                    negative_controls += 1;
                }
            }
        }
        assert_eq!(negative_controls, 16);
    }

    /// Every class is deterministic: [`MARK_SENTENCE`] with a Shift cue 60 ms
    /// ahead of every capital, rendered twice under one seed, is
    /// bit-identical. Every draw on the class paths comes from the seeded
    /// stream, and no branch reads a clock.
    #[test]
    fn every_class_is_deterministic() {
        let script = |text: &str| -> Vec<f32> {
            let mut s = synth();
            let mut out: Vec<f32> = Vec::new();
            let mut at = 1_000u32;
            for ch in text.chars() {
                if ch.is_uppercase() {
                    push(&mut s, SoundKind::Shift, at - 60, 0.0, false);
                }
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(&mut s, kind, at, ch);
                out.extend(render_mono(&mut s, 12));
                at += 125;
            }
            out.extend(render_mono(&mut s, 40));
            out
        };
        let a = script(MARK_SENTENCE);
        let b = script(MARK_SENTENCE);
        assert_eq!(a.len(), b.len());
        assert!(
            a.len() >= 3 * SR as usize,
            "the determinism script is only {} s long",
            a.len() as f32 / SR
        );
        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
            assert!(
                x.to_bits() == y.to_bits(),
                "sample {i} differed between two runs of the class sentence ({x} vs {y})"
            );
        }
        assert!(a.iter().any(|x| x.abs() > 1e-4), "the sentence was silent");
    }

    /// A doubled letter is always a re-strike (§3.1 step 1), in the outer
    /// register too: gravity acts on a stride, never on a repeat. Every prefix
    /// of the pangram, every letter doubled after it.
    #[test]
    fn a_doubled_letter_is_a_re_strike_wherever_the_line_stands() {
        const KEYS: &str = "thequickbrownfoxjumpsoverthelazydog";
        let mut checked = 0usize;
        let mut outer = 0usize;
        for n in 1..=KEYS.chars().count() {
            for ch in 'a'..='z' {
                let mut s = synth();
                let mut at = 1_000u32;
                for c in KEYS.chars().take(n) {
                    push_ch(&mut s, SoundKind::Typed, at, c);
                    at += 90;
                }
                push_ch(&mut s, SoundKind::Typed, at, ch);
                let before = s.v2.walk();
                let mark = s.born_seq;
                push_ch(&mut s, SoundKind::Typed, at + 90, ch);
                checked += 1;
                if (i32::from(before) - MELODY_CENTRE_DEG).abs() > MELODY_GRAVITY_DEG {
                    outer += 1;
                }
                assert_eq!(
                    s.v2.walk(),
                    before,
                    "`{ch}{ch}` after {:?} moved the line from {before} to {} — a doubled \
                     letter is a repeat, and gravity may not turn it into a step",
                    &KEYS[..n],
                    s.v2.walk()
                );
                let lead = tune_voices(&since(&s, mark));
                assert_eq!(lead.len(), 1);
                assert_eq!(lead[0].p[2].lvl, 0.0, "a re-strike has no strike partial");
                assert!(s.v2.text.restrike >= 1, "the re-strike ladder did not arm");
            }
        }
        assert!(
            outer > 0,
            "fixture: {checked} doubles checked and none stood where gravity is armed"
        );
    }

    /// A word head's common tone is an accent, not a tremolo (§3.1 step 6):
    /// when the chord snap lands the head on the previous word's last degree,
    /// the head keeps the step's touch (full level, strike partial, roof), and
    /// the previous voice is still damped.
    #[test]
    fn a_word_head_common_tone_keeps_the_steps_touch_and_still_damps_the_old_voice() {
        let mut s = synth();
        let mut at = 1_000u32;
        let mut heads_repeated = 0usize;
        let mut steered_repeated = 0usize;
        let mut doubles = 0usize;
        for line in BENCH_PROSE.lines() {
            for word in line.split(' ') {
                if word.is_empty() {
                    continue;
                }
                let mut prev = '\0';
                for (i, ch) in word.chars().enumerate() {
                    let before = s.v2.walk();
                    let old_lead = s.v2.lead;
                    let was_marked = s.v2.token_marked();
                    let mark = s.born_seq;
                    push_ch(&mut s, SoundKind::Typed, at, ch);
                    at += 100;
                    let lead = tune_voices(&since(&s, mark))[0];
                    if s.v2.walk() != before {
                        prev = ch;
                        continue;
                    }
                    let steered = !was_marked && s.v2.token_marked();
                    // The old voice is damping — on a head and on a double.
                    if let Some((slot, born)) = old_lead {
                        let v = &s.voices[usize::from(slot)];
                        assert!(
                            v.born != born || !v.on || v.damp > 0.0,
                            "a repeated pitch did not damp the voice before it"
                        );
                    }
                    if i == 0 {
                        heads_repeated += 1;
                        assert_eq!(
                            s.v2.text.restrike, 0,
                            "a word head armed the re-strike ladder"
                        );
                        assert!(
                            lead.p[2].lvl > 0.0 && lead.n_lvl == MALLET_LVL,
                            "a word head's common tone lost the step's strike"
                        );
                    } else if steered {
                        // A steered mark arriving on the degree the line stands
                        // on is the cadence, not a doubled letter: a step, like
                        // the head, and the damp above held for it.
                        steered_repeated += 1;
                        assert_eq!(
                            s.v2.text.restrike, 0,
                            "a cadence armed the re-strike ladder"
                        );
                        assert!(
                            lead.p[0].lvl >= P1_LVL,
                            "a cadence's common tone took the re-strike's levels"
                        );
                    } else {
                        assert_eq!(prev, ch, "an in-word repeat that is not a doubled letter");
                        doubles += 1;
                        assert!(s.v2.text.restrike >= 1);
                        assert_eq!(lead.p[2].lvl, 0.0);
                    }
                    prev = ch;
                }
                push(&mut s, SoundKind::Space, at, 0.0, false);
                at += 100;
            }
            push(&mut s, SoundKind::Jump, at, 0.0, false);
            at += 200;
        }
        assert!(doubles > 0, "fixture: the prose types no doubled letter");
        assert!(
            heads_repeated > 0,
            "fixture: no word head landed on the previous word's last degree"
        );
        assert!(
            steered_repeated > 0,
            "fixture: no mark of the prose cadenced on the degree it stood on"
        );
    }

    /// The room answers a line's end and a rest, and nothing inside a phrase;
    /// the subject is answered from above (§3.3 item 4, the answering voice).
    #[test]
    fn the_room_answers_line_ends_and_rests_and_the_subject_is_answered_from_above() {
        let taps = |v: &[Voice]| -> Vec<Voice> {
            v.iter()
                .filter(|v| v.lane == LANE_BLOOM && v.delay > BLOOM_DELAY_S + 1e-6)
                .copied()
                .collect()
        };
        let mut s = synth();
        let mut at = 1_000u32;
        // Inside a phrase: blooms, but no taps and no answer.
        for ch in "abcd".chars() {
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Typed, at, ch);
            assert!(
                taps(&since(&s, mark)).is_empty(),
                "the room spoke inside a phrase"
            );
            at += 200;
        }
        // A rest: the key after ≥ PHRASE_PAUSE_MS carries the room's taps.
        at += PHRASE_PAUSE_MS;
        let mark = s.born_seq;
        push_ch(&mut s, SoundKind::Typed, at, 'e');
        assert_eq!(
            taps(&since(&s, mark)).len(),
            AIR_TAP_DELAY_S.len(),
            "a rest leaves the room's taps"
        );

        // The answer: a subject latched from a line's first three intervals,
        // answered at the fourth word head: one bloom-lane voice ANSWER_DELAY_S
        // behind the head, no mallet, above the head on a lit tone.
        let mut s = synth();
        let mut at = 1_000u32;
        let mut answers = 0usize;
        for (i, ch) in "abcd efg hij klm nop qrs".chars().enumerate() {
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            let mark = s.born_seq;
            push_ch(&mut s, kind, at, ch);
            at += 120;
            if ch == ' ' {
                continue;
            }
            let spawned = since(&s, mark);
            let lead = tune_voices(&spawned)[0];
            let answer: Vec<&Voice> = spawned
                .iter()
                .filter(|v| v.lane == LANE_BLOOM && (v.delay - ANSWER_DELAY_S).abs() < 1e-6)
                .collect();
            if answer.is_empty() {
                continue;
            }
            answers += 1;
            assert_eq!(answer.len(), 1);
            let a = answer[0];
            assert!(
                s.v2.motif_answering(),
                "an answer with no subject at key {i}"
            );
            assert_eq!(a.n_lvl, 0.0, "the answer has no mallet");
            assert!(
                a.p[0].f0 > lead.p[0].f0 * 1.05,
                "the answer ({} Hz) is not above the head ({} Hz)",
                a.p[0].f0,
                lead.p[0].f0
            );
        }
        assert!(answers >= 1, "the subject was never answered");

        // With the room stop out: no taps, no answer.
        let mut s = synth();
        s.set_v2_timbre_stops(TimbreStops {
            bloom: true,
            hue: true,
            room: false,
        });
        let mut at = 1_000u32;
        let mut extra = 0usize;
        for ch in "abcd efg hij klm nop".chars() {
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            let mark = s.born_seq;
            push_ch(&mut s, kind, at, ch);
            extra += taps(&since(&s, mark)).len();
            at += 1_000;
        }
        assert_eq!(extra, 0, "the room spoke with its stop out");
    }

    // -- A8: Backspace ----------------------------------------------------

    // -- A16: one clock for the bell and the landing ----------------------

    /// The bell and the landing share one clock (§8.1, §12.1, D9, A16): the
    /// bell's pre-delay is `flight_ms(cells)` to the f32 bit (the number
    /// `rainbow_kitty::timing` hands the pixels) at every distance, and the
    /// Enter cadence's three landing voices are on the same edge.
    #[test]
    fn the_bell_and_the_landing_share_one_clock() {
        for cells in [8u16, 20, 50, 70, 200] {
            let want = timing::flight_ms(f32::from(cells)) * 0.001;
            let mut s = synth();
            push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
            let mark = s.born_seq;
            s.push_meta(
                event(
                    SoundKind::Meteor {
                        dir: 1,
                        cells,
                        armed: false,
                    },
                    0.7,
                    false,
                ),
                EventMeta {
                    at_ms: 1_100,
                    pan_from: -0.7,
                    ..EventMeta::default()
                },
            );
            let bell = since(&s, mark)
                .into_iter()
                .find(|v| v.lane == LANE_TUNE)
                .expect("the meteor must ring a bell");
            assert_eq!(
                bell.delay.to_bits(),
                want.to_bits(),
                "cells {cells}: the bell is at {} s, the pixels land at {want} s",
                bell.delay
            );

            // Enter: resolution C, tonic dyad and faraway bell, all on the
            // same edge (D9).
            let mut s = synth();
            for k in 0..6u32 {
                push(&mut s, SoundKind::Typed, 1_000 + k * 200, 0.0, false);
            }
            let mark = s.born_seq;
            push(&mut s, SoundKind::Enter { cells }, 2_400, 0.0, false);
            let landed: Vec<Voice> = since(&s, mark)
                .into_iter()
                .filter(|v| v.delay > 0.0 && v.lane != LANE_BLOOM)
                .collect();
            assert_eq!(
                landed.len(),
                3,
                "cells {cells}: the cadence must land three voices on the edge"
            );
            for v in landed {
                assert_eq!(
                    v.delay.to_bits(),
                    want.to_bits(),
                    "cells {cells}: a cadence voice landed at {} s, not {want} s",
                    v.delay
                );
            }
        }
    }

    // -- A17 / A20 / A26: the rest of the gesture table --------------------

    /// Direction lives in the core (§12.3, A17): a rightward meteor's core
    /// glides up, a leftward one's down; the whoosh is blind to it.
    #[test]
    fn the_core_carries_direction_and_the_whoosh_does_not() {
        let ratios = |dir: i8| -> (f32, f32, f32) {
            let mut s = synth();
            push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
            let mark = s.born_seq;
            s.push_meta(
                event(
                    SoundKind::Meteor {
                        dir,
                        cells: 50,
                        armed: false,
                    },
                    0.8,
                    false,
                ),
                EventMeta {
                    at_ms: 1_100,
                    pan_from: -0.8,
                    ..EventMeta::default()
                },
            );
            let v = since(&s, mark);
            let core = v
                .iter()
                .find(|v| v.lane == LANE_METEOR && v.p[0].lvl > 0.0)
                .expect("core");
            let whoosh = v
                .iter()
                .find(|v| v.lane == LANE_METEOR && v.n_lvl > 0.0 && v.n_f1 > v.n_f0)
                .expect("whoosh");
            (core.p[0].f1 / core.p[0].f0, whoosh.n_f0, whoosh.n_f1)
        };
        let (right, wf0, wf1) = ratios(1);
        let (left, wf0b, wf1b) = ratios(-1);
        assert!(
            right > 1.0 && left < 1.0,
            "core ratios: right {right}, left {left}"
        );
        assert_eq!((wf0, wf1), (wf0b, wf1b), "the whoosh knows the direction");
    }

    /// `Land` is silent under v2 (§12.3, A20): the meteor's bell is the
    /// landing.
    #[test]
    fn land_is_silent_under_v2() {
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let mark = s.born_seq;
        push(&mut s, SoundKind::Land, 1_100, 0.0, false);
        assert!(
            since(&s, mark).is_empty(),
            "a v2 Land spawned {} voice(s)",
            since(&s, mark).len()
        );
    }

    /// The cascade never stacks (D18, A26): one `Jump` is the four-note
    /// brrrring; twelve `Jump`s at 60 ms are one cascade plus quiet top-note
    /// re-strikes that do not repeat one pitch. A keyed `Enter` is the cadence
    /// and mints no cascade. A streamed run's cascades walk the authored theme
    /// instead of looping one figure.
    #[test]
    fn the_cascade_never_stacks() {
        let mut s = synth();
        let mark = s.born_seq;
        push(&mut s, SoundKind::Jump, 1_000, -0.5, false);
        assert_eq!(
            since(&s, mark).len(),
            4,
            "one Jump is the four-note brrrring"
        );

        let mut s = synth();
        let mut cascades = 0;
        let mut onsets = 0;
        let mut restrikes: Vec<f32> = Vec::new();
        for k in 0..12u32 {
            let mark = s.born_seq;
            push(&mut s, SoundKind::Jump, 1_000 + k * 60, -0.5, false);
            let born = since(&s, mark);
            onsets += born.len();
            match born.len() {
                4 => cascades += 1,
                1 => restrikes.push(born[0].p[0].f0),
                _ => {}
            }
            let mut buf = [0.0f32; 960];
            for _ in 0..6 {
                s.render(&mut buf);
            }
        }
        // A26, exactly: 12 line feeds arriving 60 ms apart are one cascade
        // plus at most eleven quiet top-note re-strikes.
        assert_eq!(
            cascades, 1,
            "12 line feeds 60 ms apart minted {cascades} four-note cascades; \
             a run gets one"
        );
        assert!(
            onsets <= 16,
            "12 line feeds produced {onsets} onsets; the cascade stacked"
        );
        // The re-strikes walk with the run too: v0.82.0 re-struck one 1046 Hz
        // note every 60 ms, the loop the owner heard.
        assert!(
            restrikes.len() >= 2 && restrikes.windows(2).any(|w| w[0] != w[1]),
            "the re-strikes of a run are one pitch: {restrikes:?}"
        );

        // A keyed Enter is the cadence and no cascade (A26): the PTY's line
        // feed for it, within 150 ms, is swallowed whole. A line feed later
        // than that is a cascade again.
        let mut s = synth();
        for k in 0..5u32 {
            push(&mut s, SoundKind::Typed, 1_000 + k * 200, 0.0, false);
        }
        let mark = s.born_seq;
        push(&mut s, SoundKind::Enter { cells: 30 }, 2_000, 0.0, false);
        let after_enter = s.v2.walk();
        push(&mut s, SoundKind::Jump, 2_020, 0.0, false);
        let born = since(&s, mark);
        assert!(
            born.iter().all(|v| v.lane != LANE_CASCADE),
            "the keyed Return's own line-feed echo minted a cascade"
        );
        assert_eq!(
            s.v2.walk(),
            after_enter,
            "the swallowed echo moved the line"
        );
        let mark = s.born_seq;
        push(&mut s, SoundKind::Jump, 2_400, 0.0, false);
        assert_eq!(
            since(&s, mark).len(),
            4,
            "a line feed past the echo window is a PTY cascade again"
        );

        // A streamed run at 5 lines/s: every line feed is a cascade head, and
        // the heads walk the theme's phrase edges rather than loop one figure.
        let mut s = synth();
        let mut buf = [0.0f32; 960];
        let mut heads = Vec::new();
        for k in 0..10u32 {
            let mark = s.born_seq;
            push(&mut s, SoundKind::Jump, 1_000 + k * 200, -0.9, false);
            let born = since(&s, mark);
            assert_eq!(born.len(), 4, "line feed {k} of the run: one cascade");
            heads.push(born[0].p[0].f0);
            for _ in 0..20 {
                s.render(&mut buf);
            }
        }
        heads.sort_by(f32::total_cmp);
        heads.dedup();
        assert!(
            heads.len() >= 3,
            "a line-feed run looped one figure: {heads:?}"
        );
    }

    // -- A6: the pure-fifth loop -----------------------------------------

    /// Space walks pure fifths, and Enter comes home (§10.3, A6): every dyad is
    /// an exact 3:2 or 4:3 inside the BASS lane's 261.6–436.0 Hz, and after an
    /// Enter the next Space plays I.
    #[test]
    fn the_space_walks_pure_fifths_and_enter_comes_home() {
        let mut s = synth();
        for w in 0..8u32 {
            push(&mut s, SoundKind::Typed, 1_000 + w * 400, 0.0, false);
            let mark = s.born_seq;
            push(&mut s, SoundKind::Space, 1_200 + w * 400, 0.0, false);
            let bass = since(&s, mark)
                .into_iter()
                .find(|v| v.lane == LANE_BASS)
                .expect("a word head is a downbeat");
            let (root, fifth) = (bass.p[0].f0, bass.p[1].f0);
            let ratio = fifth / root;
            assert!(
                (ratio - 1.5).abs() < 1e-4 || (ratio - 0.75).abs() < 1e-4,
                "dyad ratio {ratio} is neither a pure fifth above nor a pure \
                 fourth below"
            );
            assert!(
                (261.0..=437.0).contains(&root) && (261.0..=437.0).contains(&fifth),
                "the dyad ({root} + {fifth}) left the BASS lane"
            );
        }
        let mut s2 = synth();
        push(&mut s2, SoundKind::Typed, 1_000, 0.0, false);
        push(&mut s2, SoundKind::Enter { cells: 40 }, 1_200, 0.0, false);
        let mark = s2.born_seq;
        push(&mut s2, SoundKind::Typed, 1_600, 0.0, false);
        push(&mut s2, SoundKind::Space, 1_800, 0.0, false);
        let bass = since(&s2, mark)
            .into_iter()
            .find(|v| v.lane == LANE_BASS && v.delay == 0.0)
            .expect("the first word of a new line is a downbeat");
        assert!(
            (bass.p[0].f0 - BASS_BASE_HZ).abs() < 1e-3
                && (bass.p[1].f0 / bass.p[0].f0 - 1.5).abs() < 1e-4,
            "after an Enter the first Space must play I (C4 + G4), got {} + {}",
            bass.p[0].f0,
            bass.p[1].f0
        );
    }

    /// A run of spaces is one downbeat and a rising figure: after the head's
    /// dyad, each space 100 ms on adds one indent step in [`LANE_GLINT`],
    /// strictly rising, quieter than the downbeat, moving no chord; a space
    /// inside [`SPACE_STEP_MIN_GAP_MS`] is breath alone. A held spacebar (96
    /// repeats at 33 ms) gets at most 13 steps a second, one at a time, and
    /// every glint it mints is a step (the figure's pitches are
    /// `the_indent_figure_is_a_cycle_on_every_chord_and_key`).
    #[test]
    fn a_run_of_spaces_is_one_downbeat_and_a_rising_figure() {
        let mag = |v: &Voice| (v.gl * v.gl + v.gr * v.gr).sqrt();
        let mut s = synth();
        type_line(&mut s, "hello", 1_000, 150);
        let mark = s.born_seq;
        push(&mut s, SoundKind::Space, 2_000, 0.0, false);
        let head = since(&s, mark);
        let bass = head
            .iter()
            .find(|v| v.lane == LANE_BASS)
            .expect("the run's head is the downbeat");
        assert!(
            head.iter().all(|v| v.lane != LANE_GLINT),
            "the head is the downbeat, not a step"
        );
        let (root, head_gain) = (bass.p[0].f0, mag(bass));
        let chord_idx = s.v2.text.chord;
        let walk = s.v2.walk();
        let mut last = root;
        for n in 1..=3u32 {
            let mark = s.born_seq;
            push(&mut s, SoundKind::Space, 2_000 + n * 100, 0.0, false);
            let v = since(&s, mark);
            assert!(
                v.iter().all(|v| v.lane != LANE_BASS),
                "space {n} of the run: a second downbeat"
            );
            assert!(
                v.iter().any(|v| v.lane == LANE_BREATH),
                "space {n} of the run: the breath is still under the step"
            );
            let steps: Vec<&Voice> = v.iter().filter(|v| v.lane == LANE_GLINT).collect();
            assert_eq!(
                steps.len(),
                1,
                "space {n} of the run: exactly one indent step"
            );
            let step = steps[0];
            let f = step.p[0].f0;
            assert!(
                f > last * 1.01,
                "space {n}: the figure must RISE ({last} -> {f} Hz)"
            );
            assert!(
                mag(step) < head_gain,
                "space {n}: a step ({}) may not be louder than the downbeat ({head_gain})",
                mag(step)
            );
            last = f;
        }
        let mark = s.born_seq;
        push(&mut s, SoundKind::Space, 2_340, 0.0, false);
        assert!(
            since(&s, mark).iter().all(|v| v.lane != LANE_GLINT),
            "a space inside SPACE_STEP_MIN_GAP_MS of the last step is breath alone"
        );
        assert_eq!(s.v2.text.chord, chord_idx, "the tail moved the chord");
        assert_eq!(s.v2.walk(), walk, "the tail moved the walk");

        // The held spacebar: 96 repeats at 33 ms, 3.2 s.
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let mut buf = [0.0f32; 3_168]; // 33 ms
        let (mut heard, mut first_second) = (0usize, 0usize);
        // The letter's own sparkle is a LANE_GLINT birth too; count from here.
        let glints_before = s.lane_births[usize::from(LANE_GLINT)];
        for k in 0..96u32 {
            let mark = s.born_seq;
            push(&mut s, SoundKind::Space, 1_200 + k * 33, 0.0, false);
            s.render(&mut buf);
            let steps: Vec<Voice> = since(&s, mark)
                .into_iter()
                .filter(|v| v.lane == LANE_GLINT)
                .collect();
            assert!(
                steps.len() <= 1,
                "repeat {k}: {} steps at once",
                steps.len()
            );
            if !steps.is_empty() {
                heard += 1;
                first_second += usize::from(k < 30);
            }
        }
        assert_eq!(
            (s.lane_births[usize::from(LANE_GLINT)] - glints_before) as usize,
            heard,
            "every LANE_GLINT birth of a held spacebar is an indent step"
        );
        assert!(
            (4..=13).contains(&first_second),
            "a held spacebar minted {first_second} steps/s — the gate admits one per 75+ ms"
        );
        assert!(
            heard >= 30,
            "3.2 s of auto-repeat minted only {heard} steps"
        );
    }

    /// The indent figure is a cycle on every chord and key ([`indent_step_hz`]):
    /// over every chord, song key −3..=4 and 300 steps, every step is a lit
    /// degree above the dyad's root and under [`SPACE_STEP_HI_HZ`], and the
    /// figure is periodic and strictly rising inside a pass (the first cut's
    /// fold ran out at the 54th step).
    #[test]
    fn the_indent_figure_is_a_cycle_on_every_chord_and_key() {
        for (ci, chord) in CHORD_LOOP.iter().enumerate() {
            for key in -3..=4i32 {
                let root = penta(BASS_BASE_HZ * CHORD_ROOT_RATIO[chord.root], key);
                let f: Vec<f32> = (1..=300u32)
                    .map(|n| indent_step_hz(ci, root, n, key))
                    .collect();
                let cycle = f
                    .iter()
                    .skip(1)
                    .position(|x| (x - f[0]).abs() < SAME_PITCH_HZ)
                    .map(|i| i + 1)
                    .unwrap_or_else(|| panic!("chord {ci} key {key}: no cycle in {f:?}"));
                for (i, x) in f.iter().enumerate() {
                    let n = i + 1;
                    assert!(
                        *x < SPACE_STEP_HI_HZ && *x > root,
                        "chord {ci} key {key} step {n}: {x} Hz is not in ({root}, {SPACE_STEP_HI_HZ})"
                    );
                    if key == 0 {
                        assert!(
                            (BASS_BASE_HZ..SPACE_STEP_HI_HZ).contains(x),
                            "chord {ci} step {n}: {x} Hz left the §9.4 band"
                        );
                    }
                    assert!(
                        (-20i32..40).any(|deg| {
                            chord.lit & (1 << deg.rem_euclid(5)) != 0
                                && (penta(BASS_BASE_HZ, deg + key) - x).abs() < SAME_PITCH_HZ
                        }),
                        "chord {ci} key {key} step {n}: {x} Hz is not a lit degree"
                    );
                    assert!(
                        (x - f[i % cycle]).abs() < SAME_PITCH_HZ,
                        "chord {ci} key {key} step {n}: {x} Hz breaks the {cycle}-note cycle"
                    );
                    if i % cycle > 0 {
                        assert!(
                            *x > f[i - 1] * 1.01,
                            "chord {ci} key {key} step {n}: {} -> {x} does not rise",
                            f[i - 1]
                        );
                    }
                }
            }
        }
    }

    // -- §16: the deltas are identity-defaulted ---------------------------

    /// Every engine delta defaults to the identity (§16, A25): a v1 event builds
    /// voices on which every field §16 added is zero. The rainbow kitty look is
    /// the music box, not a v1 event, so it is not in the sweep.
    #[test]
    fn a_v1_event_leaves_every_v2_delta_at_its_identity() {
        for style in [
            GlowStyle::Lumen,
            GlowStyle::Sparkle,
            GlowStyle::Comet,
            GlowStyle::Fire,
        ] {
            for kind in [
                SoundKind::Typed,
                SoundKind::Backspace,
                SoundKind::Jump,
                SoundKind::Kill,
                SoundKind::Space,
                SoundKind::Land,
            ] {
                let mut s = TrailSynth::new(SR, SEED);
                let mut ev = event(kind, 0.3, false);
                ev.style = style;
                s.push(ev);
                for v in s.voices.iter().filter(|v| v.on) {
                    assert_eq!(v.lane, LANE_NONE, "{style:?}/{kind:?}: laned");
                    assert_eq!(v.n_decay, 0.0, "{style:?}/{kind:?}: n_decay");
                    assert_eq!(v.pan_glide_s, 0.0, "{style:?}/{kind:?}: pan glide");
                    assert_eq!(v.arm_ttl, 0.0, "{style:?}/{kind:?}: arm ttl");
                    assert_eq!(v.gl1, v.gl, "{style:?}/{kind:?}: gl1");
                    assert_eq!(v.gr1, v.gr, "{style:?}/{kind:?}: gr1");
                    for p in &v.p {
                        assert_eq!(p.decay, 0.0, "{style:?}/{kind:?}: partial decay");
                    }
                }
            }
        }
    }

    #[test]
    fn the_lanes_hold_their_caps_and_steals_are_zero() {
        let mut s = synth();
        for k in 0..200u32 {
            let at = 1_000 + k * 40;
            match k % 10 {
                9 => push(&mut s, SoundKind::Space, at, 0.0, false),
                7 => push(
                    &mut s,
                    SoundKind::Stardust { twinkle_hz: 9 },
                    at,
                    0.2,
                    false,
                ),
                5 => s.push_meta(
                    event(
                        SoundKind::Meteor {
                            dir: -1,
                            cells: 60,
                            armed: false,
                        },
                        0.5,
                        false,
                    ),
                    EventMeta {
                        at_ms: at,
                        pan_from: -0.5,
                        ..EventMeta::default()
                    },
                ),
                _ => push(&mut s, SoundKind::Typed, at, 0.1, k % 4 == 0),
            }
            let mut buf = [0.0f32; 960];
            for _ in 0..4 {
                s.render(&mut buf);
            }
            assert_caps(&s);
        }
        assert_eq!(s.steals(), 0, "the 28-voice pool ran dry under v2");

        // §14's worst case, as a burst inside one flight: 10 cps typing with
        // capitals, a downbeat, glints, two meteors 40 ms apart and a keyed
        // Enter with its full cadence, all pre-delayed onto the same 100 ms.
        // The lanes must hold at every block and the pool never run dry
        // (`steals()` must read 0). The audio clock advances with the stamps:
        // one 10 ms block per 10 ms of script, caps checked after every block.
        let mut s = synth();
        let mut buf = [0.0f32; 960];
        let mut now = 1_000u32;
        let mut run_to = |s: &mut TrailSynth, at: u32| {
            while now < at {
                s.render(&mut buf);
                assert_caps(s);
                now += 10;
            }
        };
        let mut at = 1_000;
        for k in 0..6u32 {
            run_to(&mut s, at);
            push(&mut s, SoundKind::Typed, at, 0.1, k % 2 == 0);
            at += 100;
        }
        run_to(&mut s, at);
        push(&mut s, SoundKind::Space, at, 0.1, false);
        run_to(&mut s, at + 10);
        push(
            &mut s,
            SoundKind::Stardust { twinkle_hz: 9 },
            at + 10,
            0.2,
            false,
        );
        run_to(&mut s, at + 20);
        meteor(&mut s, at + 20, 60, false);
        run_to(&mut s, at + 60);
        meteor(&mut s, at + 60, 60, false);
        run_to(&mut s, at + 70);
        push(&mut s, SoundKind::Typed, at + 70, 0.3, true);
        run_to(&mut s, at + 80);
        push(
            &mut s,
            SoundKind::Stardust { twinkle_hz: 12 },
            at + 80,
            0.3,
            false,
        );
        run_to(&mut s, at + 90);
        push(&mut s, SoundKind::Enter { cells: 40 }, at + 90, 0.3, false);
        run_to(&mut s, at + 170);
        push(&mut s, SoundKind::Typed, at + 170, 0.0, true);
        run_to(&mut s, at + 270);
        push(&mut s, SoundKind::Typed, at + 270, 0.0, false);
        run_to(&mut s, at + 370);
        push(&mut s, SoundKind::Space, at + 370, 0.0, false);
        run_to(&mut s, at + 800);
        assert_eq!(s.steals(), 0, "§14's worst case ran the 28-voice pool dry");
    }

    /// §14 at one instant: every lane at or under its cap, counting sounding
    /// voices only (a pre-delayed voice is a schedule entry: "5 scheduled,
    /// ≤ 3 live"), and no drop-newcomer lane (glint, bloom) has fade-stolen a
    /// sounding voice under the 40 ms age guard. A voice damped before its
    /// pre-delay ran out never sounded and is not a steal.
    fn assert_caps(s: &TrailSynth) {
        for lane in [
            LANE_TUNE,
            LANE_BLOOM,
            LANE_BASS,
            LANE_BREATH,
            LANE_GLINT,
            LANE_METEOR,
            LANE_RAIN,
            LANE_CADENCE,
            LANE_CASCADE,
            LANE_GRAFT,
            LANE_TING,
        ] {
            let live = s
                .voices
                .iter()
                .filter(|v| v.on && v.lane == lane && v.damp <= 0.0 && v.t >= 0.0)
                .count();
            assert!(
                live <= lane_cap(lane),
                "lane {lane} held {live} voices, over its cap of {}",
                lane_cap(lane)
            );
        }
        for v in s.voices.iter().filter(|v| {
            v.on && lane_drops_the_newcomer(v.lane) && v.damp > 0.0 && v.damp0 == LANE_FADE_STEAL_S
        }) {
            // Its age when the ramp was armed: now, less the ramp burnt.
            let age = v.t - (v.damp0 - v.damp);
            assert!(
                !(0.0..LANE_AGE_GUARD_S - 1e-4).contains(&age),
                "lane {} fade-stole a voice {age} s old — under the 40 ms age guard",
                v.lane
            );
        }
    }

    /// A glint is a lattice pitch in the stardust lane, twinkling at its own
    /// star's rate (§13, D12, A21).
    #[test]
    fn a_hero_star_is_a_pitched_glint_at_its_own_stars_rate() {
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        for (i, rate) in [8u8, 11, 12].iter().enumerate() {
            let mark = s.born_seq;
            push(
                &mut s,
                SoundKind::Stardust { twinkle_hz: *rate },
                1_100 + i as u32 * 300,
                0.4,
                false,
            );
            let g = since(&s, mark);
            assert_eq!(g.len(), 1, "a hero is exactly one glint");
            let g = g[0];
            assert!(g.p[1].lvl == 0.0 && g.p[2].lvl == 0.0, "one partial only");
            assert!(
                (GLINT_LO_HZ..GLINT_HI_HZ).contains(&g.p[0].f0),
                "the glint at {} Hz left the STARDUST lane",
                g.p[0].f0
            );
            assert_eq!(
                g.tw_rate,
                f32::from(*rate),
                "the glint took its own star's rate"
            );
            let mut buf = [0.0f32; 960];
            for _ in 0..14 {
                s.render(&mut buf);
            }
        }
    }

    // -- A8, held: the erase gate thins the poof, never the rewind ----------

    /// A held Backspace un-sings one note per repeat (§10.4, A8). The 40 ms mute
    /// of the erased note and the rewind stand outside the 75 ms erase gate,
    /// which thins only the poof. Three letters, three deletions at autorepeat
    /// speed: the retyped letter derives from the first letter's playhead, while
    /// the poof speaks once for the run.
    #[test]
    fn a_held_backspace_unsings_one_note_per_repeat() {
        let mut s = synth();
        let mark = s.born_seq;
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let first_f0: Vec<f32> = tune_voices(&since(&s, mark))
            .iter()
            .map(|v| v.p[0].f0)
            .collect();
        let first = s.v2.walk();
        push(&mut s, SoundKind::Typed, 1_300, 0.0, false);
        push(&mut s, SoundKind::Typed, 1_600, 0.0, false);
        assert_ne!(s.v2.walk(), first, "three steps must have moved the line");
        let mut buf = [0.0f32; 960];
        let mut poofs = 0;
        for k in 0..3u32 {
            let mark = s.born_seq;
            push(&mut s, SoundKind::Backspace, 1_900 + k * 33, 0.0, false);
            if k == 0 {
                assert!(
                    s.voices.iter().any(|v| v.on
                        && v.lane == LANE_TUNE
                        && (v.damp - ERASE_MUTE_S).abs() < 1e-6),
                    "the erased letter's note was not muted by the erase"
                );
            }
            if since(&s, mark).iter().any(|v| v.lane == LANE_NONE) {
                poofs += 1;
            }
            // ≈ 30 ms of audio between repeats — inside the erase gate.
            for _ in 0..3 {
                s.render(&mut buf);
            }
        }
        assert_eq!(
            poofs, 1,
            "the erase gate thins the poof to one per 75 ms; it spoke {poofs} times"
        );
        // The state rewound one note per repeat, asserted on the state rather
        // than through a pitch.
        assert_eq!(
            s.v2.walk(),
            first,
            "the line did not rewind one note per repeat"
        );
        let mark = s.born_seq;
        push(&mut s, SoundKind::Typed, 2_300, 0.0, false);
        let retyped: Vec<f32> = tune_voices(&since(&s, mark))
            .iter()
            .map(|v| v.p[0].f0)
            .collect();
        // The retyped letter still sings. Its pitch depends on the rewound
        // state and on the hand (this retype follows a different pause), so a
        // different note is allowed (R2); the undo stack owes the state
        // (asserted above), R1 owes a note (asserted here).
        assert_eq!(
            retyped.len(),
            1,
            "the retyped letter spawned {} tune voices, not one",
            retyped.len()
        );
        assert!(
            retyped[0] > 0.0 && !first_f0.is_empty(),
            "the retyped letter made no pitched sound"
        );
    }

    // -- A24: the bus limiter, and idle -----------------------------------

    /// Render `blocks` × 480 frames and return the interleaved output.
    fn render_all(s: &mut TrailSynth, blocks: usize) -> Vec<f32> {
        let mut buf = [0.0f32; 960];
        let mut out = Vec::with_capacity(blocks * 960);
        for _ in 0..blocks {
            s.render(&mut buf);
            out.extend_from_slice(&buf);
        }
        out
    }

    fn peak_of(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
    }

    /// The limiter is transparent below its threshold, holds the ceiling above
    /// it, and idle is exact silence (§9.7, A24): bit-identical latched vs not
    /// below −14 dBFS, `limit` pinned sample-exact at ±15 dBFS and at the
    /// threshold and moving one part in a thousand over it (a hard knee), a
    /// meteor at full host volume held at the ceiling within the attack's
    /// overshoot, and the limiter and bus back at exactly 1.0 and 0 once idle.
    #[test]
    fn the_limiter_is_transparent_below_threshold_and_holds_the_ceiling_above() {
        // Below. Same seed, same event; one bus latched, one not.
        let mut a = synth();
        push(&mut a, SoundKind::Typed, 1_000, 0.0, false);
        assert!(a.v2_latched, "the first v2 trail event latches the bus");
        let mut b = synth();
        push(&mut b, SoundKind::Typed, 1_000, 0.0, false);
        b.v2_latched = false;
        let ya = render_all(&mut a, 40);
        let yb = render_all(&mut b, 40);
        let peak = peak_of(&yb);
        assert!(
            peak > 0.05 && peak < LIMIT_THRESHOLD,
            "the probe must sound under the threshold (peak {peak})"
        );
        assert!(
            ya.iter().zip(&yb).all(|(p, q)| p.to_bits() == q.to_bits()),
            "below the threshold the latched bus moved a bit"
        );
        assert_eq!(
            (a.lim_g_l, a.lim_g_r),
            (1.0, 1.0),
            "the gain left 1.0 under the threshold"
        );

        // The knee, sample-exact: `limit` on a settled follower returns the
        // input's bits at ±15 dBFS and at the threshold itself (the follower's
        // test is strict); one part in a thousand over it the gain moves on
        // that sample. No tolerance: a hard knee.
        let k_att = 1.0 - (-1.0 / (SR * LIMIT_ATTACK_S)).exp();
        let k_rel = (-1.0 / (SR * LIMIT_RELEASE_S)).exp();
        let minus_15_dbfs = 10f32.powf(-15.0 / 20.0);
        for y in [minus_15_dbfs, -minus_15_dbfs, LIMIT_THRESHOLD] {
            let (mut env, mut g) = (0.0f32, 1.0f32);
            let out = limit(y, &mut env, &mut g, k_att, k_rel);
            assert_eq!(out.to_bits(), y.to_bits(), "a soft knee touched {y}");
            assert_eq!(g, 1.0, "the gain left 1.0 at {y}");
        }
        let over = LIMIT_THRESHOLD * 1.001;
        let (mut env, mut g) = (0.0f32, 1.0f32);
        let out = limit(over, &mut env, &mut g, k_att, k_rel);
        assert!(
            g < 1.0 && out < over,
            "one part in a thousand over the threshold the gain stayed at {g} — the knee is not hard"
        );

        // Above. A meteor at host volume 1.0, limited and not.
        let loud = |latched: bool| -> (TrailSynth, Vec<f32>) {
            let mut s = synth();
            let mut ev = event(
                SoundKind::Meteor {
                    dir: 1,
                    cells: 50,
                    armed: false,
                },
                0.5,
                false,
            );
            ev.gain = 1.0;
            s.push_meta(
                ev,
                EventMeta {
                    at_ms: 1_000,
                    pan_from: -0.5,
                    ..EventMeta::default()
                },
            );
            s.v2_latched = latched;
            let y = render_all(&mut s, 60);
            (s, y)
        };
        let (_, raw) = loud(false);
        let (mut s, held) = loud(true);
        let raw_peak = peak_of(&raw);
        let held_peak = peak_of(&held);
        assert!(
            raw_peak > LIMIT_THRESHOLD * 1.25,
            "the unlimited meteor must clear the threshold (peak {raw_peak})"
        );
        // A feed-forward limiter with a 0.5 ms attack lets the first half
        // millisecond of a transient through; it promises the sustained level
        // never exceeds the threshold. So samples over the ceiling are the
        // onset's few ms, where the unlimited meteor rides over it for tens.
        let over = |y: &[f32]| {
            y.iter()
                .filter(|x| x.abs() > LIMIT_THRESHOLD * 1.02)
                .count()
        };
        let (raw_over, held_over) = (over(&raw), over(&held));
        assert!(
            held_over <= 2 * 384 && raw_over > 4 * held_over,
            "the limiter left {held_over} samples over the ceiling (unlimited: {raw_over})"
        );
        assert!(
            held_peak <= LIMIT_THRESHOLD * 1.3 && held_peak < raw_peak,
            "the limited meteor peaked at {held_peak} (unlimited {raw_peak}) — the attack is not 0.5 ms"
        );
        assert!(
            held_peak > LIMIT_THRESHOLD * 0.9,
            "the limiter pumped the meteor down to {held_peak}, far under the ceiling"
        );

        // Idle. Two more seconds: the tails are gone, the bus is exact zero
        // and the limiter is reset.
        let tail = render_all(&mut s, 200);
        assert!(s.is_quiet(), "the synth did not settle to quiet");
        assert!(
            tail[tail.len() - 960..].iter().all(|x| *x == 0.0),
            "idle is not exact silence"
        );
        assert_eq!(
            (s.lim_g_l, s.lim_g_r),
            (1.0, 1.0),
            "the limiter did not reset on silence"
        );
    }

    // -- §10.1: the fallback clock and the paused queue -------------------

    /// The fallback clock wraps like a host stamp and never saturates (§16 row
    /// 7). Past the 49.7-day `u32` wrap two unstamped keys 300 ms apart are
    /// still two steps; the wrapping key reads a nonsense gap and may lose one
    /// interval, but it steps and sounds, the IOI stays finite, and the line
    /// walks on.
    #[test]
    fn the_fallback_clock_wraps_like_a_host_stamp_instead_of_saturating() {
        fn key(s: &mut TrailSynth) {
            s.push(event(SoundKind::Typed, 0.0, false));
        }
        fn render_ms(s: &mut TrailSynth, ms: usize) {
            let mut buf = [0.0f32; 960];
            for _ in 0..ms / 10 {
                s.render(&mut buf);
            }
        }
        const WRAP_S: f64 = 4_294_967.296;

        // Past the wrap: 204 ms after the 2³² boundary.
        let mut s = synth();
        s.clock_s = WRAP_S + 0.204;
        key(&mut s);
        render_ms(&mut s, 300);
        key(&mut s);
        assert_eq!(
            s.v2.steps(),
            2,
            "past 49.7 days the second key must still step — the clock saturated ({})",
            s.v2.steps()
        );

        // Across the wrap: the cost is a wrapped host stamp's, no more.
        let mut s = synth();
        s.clock_s = WRAP_S - 0.296;
        key(&mut s);
        assert_eq!(s.v2.steps(), 1, "the first key steps");
        render_ms(&mut s, 300);
        key(&mut s);
        assert_eq!(
            s.v2.steps(),
            2,
            "the wrapping key must STILL step — a wrapped clock may cost the \
             contour its gap, and may never cost the key its note"
        );
        render_ms(&mut s, 1_000);
        key(&mut s);
        render_ms(&mut s, 300);
        key(&mut s);
        assert_eq!(s.v2.steps(), 4, "healed, the line must walk on");
        assert!(
            s.v2.ioi_ms() > 0.0 && s.v2.ioi_ms().is_finite(),
            "the wrap left the IOI estimator in a state the arithmetic cannot use"
        );
    }

    // -- A18: fizzle, claim, interrupt ------------------------------------

    fn arm(s: &mut TrailSynth, at: u32) {
        s.push_meta(
            event(SoundKind::MeteorArm { dir: 1 }, 0.0, false),
            EventMeta {
                at_ms: at,
                pan_from: -0.6,
                ..EventMeta::default()
            },
        );
    }

    fn meteor(s: &mut TrailSynth, at: u32, cells: u16, armed: bool) {
        s.push_meta(
            event(
                SoundKind::Meteor {
                    dir: 1,
                    cells,
                    armed,
                },
                0.6,
                false,
            ),
            EventMeta {
                at_ms: at,
                pan_from: -0.6,
                ..EventMeta::default()
            },
        );
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// A meteor fizzles, claims without a click, interrupts, and never eats a
    /// sounded bell (§12.3, §15.2, A18): an unclaimed arm damps at its ttl, an
    /// arm whose move is a hop fizzles under the nav tick, a claim re-targets
    /// the armed core with the bus continuous, and a second meteor damps the
    /// first's sounding layers and cancels only its unsounded bell.
    #[test]
    fn a_meteor_fizzles_claims_without_a_click_and_never_eats_a_sounded_bell() {
        let mut buf = [0.0f32; 960];

        // 1. An arm with no move behind it damps at its ttl, never a cut.
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let mark = s.born_seq;
        arm(&mut s, 1_100);
        let armed = since(&s, mark);
        assert_eq!(
            armed.len(),
            2,
            "an arm is the tick and the core, nothing else"
        );
        assert!(
            armed.iter().all(|v| v.arm_ttl > 0.0),
            "both armed voices carry the ttl"
        );
        for _ in 0..16 {
            s.render(&mut buf);
        }
        let core = s
            .voices
            .iter()
            .find(|v| v.on && v.lane == LANE_METEOR && v.p[0].lvl > 0.0)
            .expect("at 160 ms the armed core is still on its 12 ms fizzle ramp");
        assert!(
            core.arm_ttl == 0.0 && core.damp > 0.0,
            "an unclaimed arm must fizzle on a ramp at its ttl"
        );

        // 2. An arm whose move is a sub-floor hop: the arm fizzles, the nav
        // tick speaks, and nothing else does.
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        arm(&mut s, 1_100);
        for _ in 0..2 {
            s.render(&mut buf);
        }
        let mark = s.born_seq;
        meteor(&mut s, 1_120, 5, true);
        let born = since(&s, mark);
        assert_eq!(born.len(), 1, "a sub-floor hop is the nav tick alone");
        assert!(
            born[0].lane == LANE_TUNE && born[0].n_lvl == 0.0 && born[0].p[0].lvl > 0.0,
            "the hop's voice is the verse note, P1 only"
        );
        assert!(
            s.voices
                .iter()
                .filter(|v| v.on && v.lane == LANE_METEOR)
                .all(|v| v.damp > 0.0 && v.arm_ttl == 0.0),
            "the fizzled arm was not damped"
        );

        // 3. The claim is continuous.
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        arm(&mut s, 1_100);
        let before = render_all(&mut s, 3);
        let mark = s.born_seq;
        meteor(&mut s, 1_130, 50, true);
        let after = render_all(&mut s, 1);
        let t = timing::flight_ms(50.0) * 0.001;
        let cores: Vec<&Voice> = s
            .voices
            .iter()
            .filter(|v| v.on && v.lane == LANE_METEOR && v.p[0].lvl > 0.0 && v.damp <= 0.0)
            .collect();
        assert_eq!(cores.len(), 1, "a claim re-targets — never two cores");
        assert!(
            cores[0].arm_ttl == 0.0 && cores[0].pan_glide_s == t && cores[0].born <= mark,
            "the claimed core is the ARMED voice, re-aimed over the true flight"
        );
        assert!(
            since(&s, mark)
                .iter()
                .all(|v| v.p[0].lvl <= 0.0 || v.delay > 0.0),
            "the claim spawned a second sounding tonal voice"
        );
        for ch in 0..2 {
            let pre: Vec<f32> = before.iter().skip(ch).step_by(2).copied().collect();
            let post: Vec<f32> = after.iter().skip(ch).step_by(2).copied().collect();
            // The sine's own slope over the 20 ms before the claim — past
            // the tick, which is over by 12.5 ms.
            let slope = pre[pre.len() - 960..]
                .windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max);
            let jump = (post[0] - pre[pre.len() - 1]).abs();
            assert!(
                jump <= slope * 1.5 + 1e-6,
                "channel {ch}: the claim stepped the bus by {jump} against a pre-claim slope of {slope}"
            );
            let (r0, r1) = (rms(&pre[pre.len() - 48..]), rms(&post[..48]));
            assert!(
                (0.8..=1.25).contains(&(r1 / r0)),
                "channel {ch}: the level jumped across the claim — 1 ms RMS {r0} → {r1}"
            );
        }

        // 4. Meteor then meteor at 40 ms.
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let mark = s.born_seq;
        meteor(&mut s, 1_100, 50, false);
        let first = since(&s, mark);
        let (first_bell_slot, first_bell_born) = s.v2.bell.expect("the first meteor rang a bell");
        for _ in 0..4 {
            s.render(&mut buf);
        }
        let mark = s.born_seq;
        meteor(&mut s, 1_140, 50, false);
        for was in &first {
            let Some(v) = s.voices.iter().find(|v| v.born == was.born) else {
                continue;
            };
            if was.lane == LANE_TUNE || was.lane == LANE_BASS || was.lane == LANE_RAIN {
                assert!(
                    !v.on && v.damp == 0.0,
                    "an UNSOUNDED voice of the first meteor (lane {}) was damped, not cancelled",
                    was.lane
                );
            } else if v.on {
                assert!(
                    v.damp > 0.0,
                    "a sounding layer of the first meteor was not damped"
                );
            }
        }
        let first_bell = &s.voices[usize::from(first_bell_slot)];
        assert!(
            !(first_bell.on && first_bell.born == first_bell_born),
            "the first meteor's unsounded bell survived the second meteor"
        );
        let (slot, born) = s.v2.bell.expect("the second meteor rang a bell");
        assert!(
            since(&s, mark)
                .iter()
                .any(|v| v.born == born && v.lane == LANE_TUNE),
            "the second bell is the second meteor's own"
        );
        for _ in 0..15 {
            s.render(&mut buf);
        }
        let bell = &s.voices[usize::from(slot)];
        assert!(
            bell.on && bell.born == born && bell.t >= 0.0 && bell.damp == 0.0,
            "at T + 50 the second bell must be sounding and undamped"
        );
    }

    // -- A31: the sing-along ----------------------------------------------

    /// A sing-along never mutes the typist's line, and hands the key back at a
    /// phrase boundary (§10.2, A31, R1). Under a live riff every key still
    /// sounds its own pitched note (ducked); the riff takes only a doubled
    /// letter's tremolo (`sing && touch == ReStrike`). When the riff dies the
    /// borrowed `song_key` is held until the line reaches a word boundary and
    /// handed back there, once, before that word's first note.
    #[test]
    fn a_sing_along_never_machine_guns_and_hands_the_key_back_at_a_phrase_boundary() {
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let mut riff = event(SoundKind::Typed, 0.0, false);
        riff.kind = SoundGesture::Celebration(CelebrationGesture::RiffBar { bar: 0, sig: 4 });
        s.push(riff);
        let key = s.song_key;
        assert!(
            key != 0 && s.sing > 0.0,
            "the riff must borrow a key and arm the sing duck"
        );

        let mut buf = [0.0f32; 960];
        let (mut pitched, mut mallets, mut heads, mut keys) = (0usize, 0usize, 0usize, 0usize);
        // 60 keys of real words at 30 cps — a machine-gun burst with word
        // boundaries in it, which is what the law is about.
        const BURST: &str = "the cat sings so the hand keeps time under it and never over it x";
        // False, not true: a key was already typed above to arm the fixture,
        // so the burst opens inside a word.
        let mut head_next = false;
        for (k, ch) in BURST.chars().take(60).enumerate() {
            let mark = s.born_seq;
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            push_ch(&mut s, kind, 1_100 + k as u32 * 33, ch);
            if ch != ' ' {
                keys += 1;
                if head_next {
                    heads += 1;
                }
                head_next = false;
                for v in tune_voices(&since(&s, mark)) {
                    if v.p[0].lvl > 0.0 {
                        pitched += 1;
                    } else {
                        mallets += 1;
                    }
                }
            } else {
                head_next = true;
            }
            for _ in 0..3 {
                s.render(&mut buf);
            }
        }
        assert_eq!(
            pitched + mallets,
            keys,
            "{keys} keys under the riff produced {} tune onsets — a key went \
             silent, which the sing duck may never do",
            pitched + mallets
        );
        assert!(
            s.sing > 0.0,
            "the riff must outlive the burst for the law to be tested"
        );
        assert!(
            heads >= 8,
            "fixture: the burst must contain words ({heads})"
        );
        // The doubled letters the burst types inside a word — `keeps` — are
        // the only keys the riff may take to the mallet.
        let doubled = BURST
            .chars()
            .take(60)
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|w| w[0] == w[1] && w[0] != ' ')
            .count();
        assert!(doubled > 0, "fixture: the burst must type a doubled letter");
        assert_eq!(
            mallets, doubled,
            "{mallets} keys went to the mallet under the riff for {doubled} doubled \
             letters — a riff may take the tremolo off a repeat and nothing else"
        );
        assert_eq!(
            pitched,
            keys - doubled,
            "{pitched} pitched TUNE onsets under the riff for {keys} keys — the riff \
             muted the typist's own line"
        );
        assert_eq!(
            s.song_key, key,
            "the key was released while the song was live"
        );

        // The riff dies: the key is held, pending the boundary.
        let mut blocks = 0;
        while s.sing > 0.0 && blocks < 800 {
            s.render(&mut buf);
            blocks += 1;
        }
        assert_eq!(s.sing, 0.0, "the riff must have died");
        assert_eq!(
            s.song_key, key,
            "the key was SNAPPED when the riff died — §10.2 hands it back at the boundary"
        );
        assert!(s.v2_key_pending, "the handback must be pending");

        // Typing on in words: the key changes once, to neutral, on a key that
        // opens a word. The keys continue the burst's clock, so the render time
        // that killed the riff opens no gap (a ≥ 900 ms gap would be a rest and
        // hand the key back before the word head under test).
        let mut changes = Vec::new();
        let mut at = 3_200u32;
        for (k, ch) in "in the middle of a word the key is held and never snapped away"
            .chars()
            .enumerate()
        {
            let pos = s.v2.word_pos();
            let before = s.song_key;
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            push_ch(&mut s, kind, at, ch);
            at += 250 + (k as u32 % 3) * 7;
            if s.song_key != before {
                changes.push((pos, s.song_key));
            }
        }
        assert_eq!(
            changes.len(),
            1,
            "song_key changed {} times after the riff; once, at the boundary",
            changes.len()
        );
        let (pos, to) = changes[0];
        assert_eq!(to, 0, "the handback goes to the neutral lattice");
        assert_eq!(
            pos, 0,
            "the key was handed back MID-WORD, at word_pos {pos}"
        );
        assert!(!s.v2_key_pending, "the handback must clear itself");
    }

    /// A v1 voice after the music box snaps a pending key rather than holding
    /// it forever (§10.2): the roster is read per event, so after auditioning
    /// the music box and switching to marimba no event would reach a v2
    /// boundary. The first v1 trail event snaps the key; a bonk does not (A31).
    #[test]
    fn a_v1_voice_after_the_music_box_snaps_a_pending_key_instead_of_holding_it_for_ever() {
        // The music box is reached by voice under a v1 look, as the settings
        // row auditions it (`event()` carries the rainbow kitty look, which
        // selects the music box by itself).
        let mut s = TrailSynth::new(SR, SEED);
        let mut music_box = event(SoundKind::Typed, 0.0, false);
        music_box.voice = SoundVoice::RainbowKittyV2;
        s.push_meta(
            music_box,
            EventMeta {
                at_ms: 1_000,
                ..EventMeta::default()
            },
        );
        assert!(s.v2_latched, "the music box latches the seam by voice");
        let mut riff = event(SoundKind::Typed, 0.0, false);
        riff.kind = SoundGesture::Celebration(CelebrationGesture::RiffBar { bar: 0, sig: 4 });
        s.push(riff);
        let key = s.song_key;
        assert!(key != 0 && s.sing > 0.0, "the riff must borrow a key");

        // The riff dies: latched, the key is held pending the boundary.
        let mut buf = [0.0f32; 960];
        let mut blocks = 0;
        while s.sing > 0.0 && blocks < 800 {
            s.render(&mut buf);
            blocks += 1;
        }
        assert_eq!(s.sing, 0.0, "the riff must have died");
        assert!(
            s.v2_key_pending && s.song_key == key,
            "the key is held pending the boundary (fixture precondition)"
        );

        // A bonk walks the v1 chain but is not a trail event: the hold stands.
        let mut bonk = event(SoundKind::Typed, 0.0, false);
        bonk.kind = SoundGesture::Words(WordGesture::Bonk);
        s.push(bonk);
        assert!(
            s.v2_key_pending && s.song_key == key,
            "a bonk snapped a key the music box was holding for its boundary"
        );

        // A v1 trail event — marimba on a nine-palette style — snaps it,
        // before its note is designed.
        let mut marimba = event(SoundKind::Typed, 0.0, false);
        marimba.style = GlowStyle::Sparkle;
        marimba.voice = SoundVoice::Marimba;
        s.push(marimba);
        assert_eq!(
            s.song_key, 0,
            "the v1 voice's typed register stayed transposed: the handback never came"
        );
        assert!(
            !s.v2_key_pending,
            "the snap must clear the pending handback"
        );
    }

    // -- A13: tails and damps ---------------------------------------------

    /// Assert §9.5 law 2 on every laned voice of `voices`; returns the lanes
    /// seen so the caller can prove the sweep was not vacuous.
    fn assert_tails(voices: &[Voice], what: &str) -> Vec<u8> {
        let mut lanes = Vec::new();
        for v in voices {
            if v.lane == LANE_NONE {
                continue;
            }
            let tail = (-v.dur / v.decay).exp();
            assert!(
                tail <= 0.07,
                "{what}: a lane-{} voice (dur {} s, decay {} s) ends at {:.1} % of peak — over A13's 7 %",
                v.lane,
                v.dur,
                v.decay,
                tail * 100.0
            );
            lanes.push(v.lane);
        }
        lanes
    }

    /// Tails end quietly; damps are ramps (§9.5 law 2, A13). Every laned voice
    /// §11 builds has `exp(−dur/decay) ≤ 0.07` (≤ −23 dB when the 5 ms release
    /// ends it), and every damp v2 arms is at least the 12 ms ramp. Checked
    /// gesture by gesture at the spawn.
    #[test]
    fn tails_end_quietly_and_damps_are_ramps() {
        let mut s = synth();
        let mut buf = [0.0f32; 960];
        let mut seen: Vec<u8> = Vec::new();
        let script: [(SoundKind, bool); 15] = [
            (SoundKind::Typed, false),
            (SoundKind::Typed, true),
            (SoundKind::Space, false),
            (SoundKind::Space, false),
            (SoundKind::Shift, false),
            (SoundKind::Navigation, false),
            (SoundKind::Jump, false),
            (SoundKind::Typed, false),
            (SoundKind::Typed, false),
            (SoundKind::Typed, false),
            (SoundKind::Typed, false),
            (SoundKind::Enter { cells: 40 }, false),
            (
                SoundKind::Meteor {
                    dir: 1,
                    cells: 50,
                    armed: false,
                },
                false,
            ),
            (SoundKind::MeteorArm { dir: -1 }, false),
            (SoundKind::Stardust { twinkle_hz: 10 }, false),
        ];
        let mut at = 1_000;
        for (kind, shifted) in script {
            let mark = s.born_seq;
            s.push_meta(
                event(kind, 0.3, shifted),
                EventMeta {
                    at_ms: at,
                    pan_from: -0.3,
                    ..EventMeta::default()
                },
            );
            seen.extend(assert_tails(&since(&s, mark), &format!("{kind:?}")));
            for _ in 0..60 {
                s.render(&mut buf);
            }
            at += 600;
        }
        // The damps: a Backspace mute, a Kill mute, a same-pitch damp and a
        // meteor interrupt, each ≥ 12 ms.
        push(&mut s, SoundKind::Typed, at, 0.0, false);
        push(&mut s, SoundKind::Backspace, at + 100, 0.0, false);
        push(&mut s, SoundKind::Typed, at + 200, 0.0, false);
        push(&mut s, SoundKind::Typed, at + 260, 0.0, false);
        push(&mut s, SoundKind::Kill, at + 300, 0.0, false);
        meteor(&mut s, at + 400, 50, false);
        meteor(&mut s, at + 440, 50, false);
        let damped = s
            .voices
            .iter()
            .filter(|v| v.on && v.lane != LANE_NONE && v.damp > 0.0)
            .count();
        assert!(
            damped >= 3,
            "the damp sweep saw only {damped} damping voices"
        );
        for v in s.voices.iter().filter(|v| v.on && v.lane != LANE_NONE) {
            if v.damp > 0.0 {
                assert!(
                    v.damp0 >= LANE_FADE_STEAL_S - 1e-6,
                    "a lane-{} voice is damping over {} s — under the 12 ms ramp",
                    v.lane,
                    v.damp0
                );
            }
        }
        seen.sort_unstable();
        seen.dedup();
        for lane in [
            LANE_TUNE,
            LANE_BLOOM,
            LANE_BASS,
            LANE_BREATH,
            LANE_GLINT,
            LANE_METEOR,
            LANE_RAIN,
            LANE_CADENCE,
            LANE_CASCADE,
            LANE_GRAFT,
        ] {
            assert!(seen.contains(&lane), "the tail sweep never saw lane {lane}");
        }
    }

    // -- §22: flow's voice ---------------------------------------------------

    /// Flow's voice is the identity at heat zero (§22): a cold lit step spawns
    /// the bloom and nothing else, so a side-car that never stamps flow never
    /// hears flow's echo; the same step in flow spawns the bloom and the echo.
    #[test]
    fn flow_s_voice_is_identity_at_heat_zero() {
        let blooms_of = |flow: f32| -> usize {
            let mut s = synth();
            let mark = s.born_seq;
            push_meta_ch(&mut s, SoundKind::Typed, 1_000, 'a', flow);
            since(&s, mark)
                .iter()
                .filter(|v| v.lane == LANE_BLOOM)
                .count()
        };
        assert_eq!(
            blooms_of(0.0),
            1,
            "the cold step spawns the bloom and nothing else"
        );
        assert_eq!(
            blooms_of(1.0),
            2,
            "negative control: the flowing step spawns the bloom and the echo"
        );
    }

    /// The echo never cancels the octave it reinforces ([`FLOW_ECHO_PHASE`]):
    /// its phase is locked to the live strike's own [`P2_RATIO`] octave, and
    /// only while that octave is this key's and stationary. A recycled slot, a
    /// gliding partial, and a timbre with no sounding octave (a wood 3f, a
    /// muted pluck) get no locked phase, so the echo cannot comb against a
    /// partial that is not there.
    #[test]
    fn flow_echo_phase_requires_the_current_keys_sounding_octave() {
        // Three timbres: the letter, the digit, and `?` (a pluck, whose second
        // partial is 4f).
        for ch in ['e', '7', '?'] {
            let mut s = synth();
            push_meta_ch(&mut s, SoundKind::Typed, 1_000, ch, 1.0);
            let (slot, _) = s.v2.lead.expect("the real typed key owns a strike");
            let lead = s.voices[usize::from(slot)];
            let f = 2.0 * lead.p[0].f0;
            let phase = s.v2_flow_echo_phase(f, FLOW_ECHO_DELAY_S);
            if ch == 'e' {
                let p2 = lead.p[1];
                let want = (p2.ph + p2.f0 * FLOW_ECHO_DELAY_S + FLOW_ECHO_PHASE).fract();
                assert_eq!(phase, Some(want));
                let echo = s
                    .voices
                    .iter()
                    .find(|v| v.on && v.lane == LANE_BLOOM && v.delay == FLOW_ECHO_DELAY_S)
                    .expect("a lit flow key actually spawned its echo");
                assert_eq!(echo.p[0].ph, want, "the shipping spawn uses that phase");
                let mut stale = s.clone();
                stale.voices[usize::from(slot)].born = lead.born.wrapping_add(1);
                assert_eq!(
                    stale.v2_flow_echo_phase(f, FLOW_ECHO_DELAY_S),
                    None,
                    "a recycled slot is not this key"
                );
                let mut bent = s.clone();
                // A 10 ms glide on the octave partial: no shipping voice glides
                // a tine partial, and the guard must still reject one.
                bent.voices[usize::from(slot)].p[1].glide = 0.010;
                assert_eq!(
                    bent.v2_flow_echo_phase(f, FLOW_ECHO_DELAY_S),
                    None,
                    "a moving partial is not stationary"
                );
            } else {
                assert_eq!(
                    phase, None,
                    "{ch}: a wood 3f or muted octave cannot phase-lock 2f"
                );
                // An unconditional P2 calculation would still return a number
                // here; the guard must distinguish these timbres from a real
                // octave, not accept every lead address.
                assert!(lead.on);
                assert!(lead.p[1].lvl == 0.0 || lead.p[1].f0 != f);
            }
        }
    }

    /// The no-hang rule holds in flow too: a plucked mark has no stationary
    /// octave for flow to extend (its question / fifth / ring stays its class or
    /// Shift gesture). Drives the real metadata and spawn path, with lit notes
    /// as a non-vacuity control for the echo admission.
    #[test]
    fn flowing_knocks_do_not_regrow_an_octave_echo() {
        let mut caught_old_gate = 0;
        for flow in [0.0, 0.5, 1.0] {
            let mut s = synth();
            for (index, ch) in "a.b,c;d:e?f!g)h]i}j'k\"l`m-n_o~p\\q|r/s+t=u*v%w^x<y>z@a#b$c&"
                .chars()
                .enumerate()
            {
                let at = 1_000 + index as u32 * 125;
                let class = crate::trail_sound::typed_glyph_class(Some(ch));
                let rank = crate::trail_sound::typed_glyph_rank(Some(ch));
                let plan = s.v2.clone().on_typed(at, rank, false, false, class);
                let mark = s.born_seq;
                push_meta_ch(&mut s, SoundKind::Typed, at, ch, flow);
                // The token's steering full stop is a tine with an octave, and
                // flow may extend it. Every other dot and plucked class is held
                // to the rule.
                if pluck_class(class) && !(class == STOP && plan.steered) {
                    let born = since(&s, mark);
                    assert!(born.iter().any(|v| v.lane == LANE_TUNE));
                    assert!(
                        born.iter()
                            .all(|v| v.lane != LANE_BLOOM || v.delay != FLOW_ECHO_DELAY_S),
                        "{ch:?} at flow {flow} acquired a long hang"
                    );
                    if flow > 0.0 && plan.lit && plan.touch == Touch::Step && !plan.mallet_only {
                        // A gate without the pluck check would admit this real
                        // key and create the forbidden octave; the control is
                        // tied to a reachable lit step, not a synthetic flag.
                        caught_old_gate += 1;
                    }
                }
                let _ = render_mono(&mut s, 12);
            }
        }
        assert!(
            caught_old_gate >= 10,
            "hot lit knocks must exercise the old gate"
        );
    }

    // ---- The word move is the same loudness every time -------------------

    /// §9.5 law 5 in the tune lane: a same-pitch re-strike damps the old voice
    /// first (12 ms). The nav tick is always a same-pitch re-strike (navigation
    /// never advances the verse, §12.3), so a word-hop pair and a held-key
    /// repeat hit this exact frequency; undamped, two independently phased
    /// ticks comb and a quiet hop can vanish.
    #[test]
    fn a_re_struck_nav_tick_replaces_its_predecessor_instead_of_combing() {
        // After the second tick nothing at that pitch is left sounding
        // undamped in the tune lane.
        let mut s = synth();
        s.push(event(SoundKind::Navigation, 0.0, false));
        let mut buf = [0.0f32; 96];
        for _ in 0..20 {
            s.render(&mut buf);
        }
        let f = penta(
            TINE_BASE_HZ,
            i32::from(s.v2.text.walk) + i32::from(s.song_key),
        );
        let live_before = s
            .voices
            .iter()
            .filter(|v| v.on && v.lane == LANE_TUNE && (v.p[0].f0 - f).abs() < SAME_PITCH_HZ)
            .count();
        assert_eq!(live_before, 1, "the first tick is still sounding");
        s.push(event(SoundKind::Navigation, 0.0, false));
        let undamped = s
            .voices
            .iter()
            .filter(|v| {
                v.on && v.lane == LANE_TUNE
                    && (v.p[0].f0 - f).abs() < SAME_PITCH_HZ
                    && v.damp <= 0.0
            })
            .count();
        assert_eq!(
            undamped, 1,
            "§9.5 law 5: exactly the newcomer is undamped at that pitch"
        );
    }

    /// The v1 governor is not a drop path for a word hop. The rainbow-kitty
    /// engine is engaged by the look and the synth by the voice, so a named
    /// instrument under the rainbow kitty style sends `Navigation` down the v1
    /// admission chain, whose `MIN_GAP` thinning must not silence a hop within
    /// 45 ms of the previous key. The glow already limits it to one cue per
    /// observed move.
    #[test]
    fn a_word_hop_is_never_thinned_by_the_typing_gap_under_a_named_instrument() {
        for voice in [SoundVoice::Style, SoundVoice::Marimba] {
            let mut s = synth();
            let typed = SoundEvent {
                voice,
                ..event(SoundKind::Typed, 0.0, false)
            };
            let nav = SoundEvent {
                voice,
                ..event(SoundKind::Navigation, 0.0, false)
            };
            s.push(typed);
            let after_key = s.voices.iter().filter(|v| v.on).count();
            // 30 ms later — inside MIN_GAP (45 ms).
            let mut buf = [0.0f32; 96];
            for _ in 0..30 {
                s.render(&mut buf);
            }
            s.push(nav);
            let after_nav = s.voices.iter().filter(|v| v.on).count();
            assert!(
                after_nav > after_key,
                "{voice:?}: a word hop 30 ms after a key must still sound"
            );
        }
    }
}
