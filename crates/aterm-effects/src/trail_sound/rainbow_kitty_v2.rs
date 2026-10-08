// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The music box: Rainbow Kitty v2's instrument and its melody engine.
//!
//! Design: `docs/design/RAINBOW-KITTY-V2.md` (§9 instrument, §10 `MelodyV2`,
//! §11 gesture table, §12 meteor, §13 stardust, §14 lanes, §15 latency, §16
//! engine deltas, §20.2 laws). Every `§` below refers to that document.
//!
//! - Every key is its own derived note ([`MelodyV2::on_typed`]); the only
//!   same-pitch re-strike is a doubled letter's ([`Touch::ReStrike`]).
//! - The caret's column drives pan only; `deg` never depends on `pan`.
//! - The tine is sine-led with per-partial decays ([`P2_TAU_S`], [`P3_TAU_S`])
//!   and a felt mallet gone by 25 ms ([`MALLET_TAU_S`]).
//! - One keystroke is one melody step at every typing speed. Smoothing comes
//!   from note choice in [`MelodyV2::derive`] (conjunct ladder, gravity,
//!   reflection, run guard), never from withholding a step.
//! - The meteor ([`TrailSynth::v2_meteor`]) is one gesture whose time
//!   constants are multiples of `flight_ms(cells)`, so the bell rings on the
//!   landing frame at any distance.
//!
//! Two laws:
//!
//! - One lattice (§2.6): every pitch is a degree of the just-intonation major
//!   pentatonic [`super::PENTA`] anchored at [`TINE_BASE_HZ`], reached through
//!   [`super::penta`], never `TrailSynth::melody_hz` (which applies the tone
//!   table's transpose; mood may bend a decay, never a pitch).
//! - Determinism: no clock is read here; time arrives as `at_ms`, and every
//!   random quantity comes from the synth's seeded stream
//!   (`TrailSynth::rnd_in`), so one script under one seed renders one waveform.

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
/// The PTY line-feed brrrring (D18). Cap 1, plus its own
/// [`CASCADE_EXCLUSIVE_MS`] window — the cascade never stacks.
pub(super) const LANE_CASCADE: u8 = 9;
/// Grafts: voices that hang off a key without being a step (the shifted key's
/// ring, the `?` rise, the bracket tink, the `=` fifth; §10.4). Cap 2,
/// fade-steal: a graft is an identity, so the most-decayed graft yields over
/// [`LANE_FADE_STEAL_S`] rather than the newcomer being dropped. Two slots let
/// a shifted mark's ring and its graft sound together.
pub(super) const LANE_GRAFT: u8 = 10;
/// The ting: the bare Shift's bell ([`TING_LEVEL`]), alone. Cap 1: a second
/// Shift replaces the first. Its own lane keeps a graft under a shifted mark
/// from fade-stealing it mid-ring
/// (`the_ting_rings_through_the_next_key`).
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
/// physical occupancy: pre-delays and fade tails still hold slots, so a
/// batched burst can exhaust the pool (decoration admission protects the
/// key's strike then). The bed pad renders outside the voice pool (§9.7).
///
/// CASCADE is 4 because one brrrring is four notes; the one-cascade-at-a-time
/// rule is [`MelodyV2::on_jump`]'s [`CASCADE_EXCLUSIVE_MS`] window, and four
/// slots keep the figure and the typing's tines from evicting each other.
///
/// TUNE 4 + BLOOM 2 + BASS 1 + BREATH 1 + GLINT 3 + METEOR 3 + RAIN 3 +
/// CADENCE 1 + CASCADE 4 + GRAFT 2 + TING 1 = 25.
/// `the_lanes_hold_their_caps_and_steals_are_zero` runs §14's worst case.
pub(super) fn lane_cap(lane: u8) -> usize {
    match lane {
        LANE_TUNE | LANE_CASCADE => 4,
        LANE_BLOOM | LANE_GRAFT => 2,
        LANE_GLINT | LANE_METEOR | LANE_RAIN => 3,
        // BASS, BREATH, CADENCE, TING, and any unnamed lane (bounded, never
        // unlimited).
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

// §10.2 / §10.4 — the melody's clocks.

// ---------------------------------------------------------------------------
// The derived line (§3.1). No time gate of any kind: every constant below
// shapes which note a key gets; none can decide that a key gets no note
// (owner, 2026-09-08).
// ---------------------------------------------------------------------------

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
/// in ([`IOI_EMA_ALPHA`]), so a gap is never its own reference.
const ACCEL_SHARE: f32 = 0.75;
const DECEL_SHARE: f32 = 1.35;

/// The register's middle, and how far the walk may wander before it is pulled
/// back. Gravity acts everywhere and turns a random walk into a bell around the
/// centre; reflection acts only at the bounds.
///
/// The pull is one-sided in practice: the down-pull arms at degree 7, the
/// up-pull would arm below 0, which [`TUNE_DEG_LO`] makes unreachable, so the
/// reflection at 0 is the lower half's restoring force. The unreachable arm
/// keeps the rule correct if the register moves.
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

/// The line's motif: the first three intervals after an Enter or phrase rest
/// are latched as the line's subject (four would fill a whole ~4.5-letter
/// English word).
const MOTIF_LEN: usize = 3;
/// The motif is answered at every third word head, transposed onto the chord
/// tone the head snapped to: about one key in thirteen, a recall rather than
/// a loop. At prose tempo that is ~3.9 s apart, within the window in which an
/// ear binds a repetition to its antecedent.
///
/// Counted in words, not seconds, so the same text plays the same degrees at
/// any typing speed (owner, 2026-09-08).
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
/// re-latches. It never replaces or withholds a step; the key ending the pause
/// resolves the line and then sings its own note.
///
/// Longer than a word-finding pause, shorter than a real stop. Shared with the
/// ribbon via [`timing::PHRASE_PAUSE_MS`]; the rest is tempo-scaled through
/// [`timing::phrase_rest_ms`] (`max(900, 2.5 · ioi)`).
pub(super) const PHRASE_PAUSE_MS: u32 = timing::PHRASE_PAUSE_MS;

/// The IOI estimator's constants (§9.6), aliased from [`timing`] so the ribbon
/// runs the same law.
const IOI_DEFAULT_MS: f32 = timing::IOI_DEFAULT_MS;
#[cfg(test)]
const IOI_MIN_MS: f32 = timing::IOI_MIN_MS;

/// Undo-stack depth in keystrokes (§10.1): about a word, so Backspace cannot
/// un-sing a note whose phrase has already closed.
const UNDO_N: usize = 8;

/// The PTY cascade's exclusivity window: one 4-note brrrring per 180 ms,
/// however many line feeds arrive.
pub(super) const CASCADE_EXCLUSIVE_MS: u32 = 180;
/// Inside a live cascade, the top-note re-strike's own floor (D18).
const CASCADE_RESTRIKE_MS: u32 = 60;
/// The line-feed run: a Jump this soon after the previous one is the next line
/// of the same output. Equal to [`PHRASE_PAUSE_MS`]: the pause that resolves a
/// typed line ends a line-feed run. ([`CASCADE_EXCLUSIVE_MS`] is too short: at
/// 5 lines/s every line feed would start a new run.)
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
///
/// 2.760 is a colour, not a harmonic. §9.5 law 4 admits it only because its τ
/// is at most 45 ms (A11): a 15 Hz beat against a neighbour's harmonic needs
/// 67 ms for one cycle, so the partial is gone before roughness exists. So
/// [`P3_TAU_S`] is a constraint, not a taste knob.
const P3_RATIO: f32 = 2.760;
const P3_LVL: f32 = 0.12;
const P3_TAU_S: f32 = 0.040;

/// The felt mallet: band-passed noise sweeping up 1800 → 6400 Hz, the ear's
/// onset time-stamp and what makes the tine read as struck. Over in 25 ms.
///
/// The sweep rises (the glass-bell squeak) and lives on the noise, not the
/// tine: noise has no pitch to beat against the lattice (§9.5 law 4). It sits
/// entirely above the tine's register so the chirp stays audible.
///
/// A fixed-Q band-pass's bandwidth scales with `f0`, so the high sweep carries
/// more noise power at the same [`MALLET_LVL`]; it spends no RMS on prose
/// (`the_felt_mallet_chirps_up_into_the_sparkle_band_and_climbs`).
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
/// `clamp(110·(0.068 + 3.727·IOI_s), 28, 110)` ms.
///
/// The masking law: at 10 cps keys are 100 ms apart and a 110 ms note would
/// pile into a chord, so fast typing thins the notes, never the note count.
/// The line passes through §3.1's two anchors: 250 ms IOI → 110 ms and 50 ms →
/// 28 ms, keeping amplitude modulation out of the 15-60 Hz roughness band by
/// note length. Below 20 cps the [`TAU_V_MIN_S`] floor is the operating point;
/// multipliers outside the clamp ([`RESTRIKE_TAU_MUL`]) go shorter still.
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

/// §9.2's re-strike touch: the tine with the strike partial at zero, "as the
/// round mote is the star at arm 0".
const RESTRIKE_P2_LVL: f32 = 0.10;
const RESTRIKE_MALLET_LVL: f32 = 0.30;
const RESTRIKE_TAU_MUL: f32 = 0.7;
/// The re-strike ladder `L_n = max(0.60·0.85^(n−1), 0.35)`: −4.4, −5.8, −7.3,
/// −8.7, then the −9.1 dB floor. A held key decays into the texture instead of
/// hammering; the floor keeps the fifth re-strike a note. The 0.60 base keeps
/// the prose mix from getting louder than v1 while the step itself is
/// unchanged.
const RESTRIKE_L0: f32 = 0.60;
const RESTRIKE_FALL: f32 = 0.85;
const RESTRIKE_FLOOR: f32 = 0.35;

// ---------------------------------------------------------------------------
// §3.3 — what makes it rainbow and magical: four additions inside the existing
// `Voice` vocabulary (no reverb, no delay line). The tine itself is untouched.
// ---------------------------------------------------------------------------

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
/// …12 ms behind the mallet, so the ear hears strike then BLOOM and not one
/// fatter transient.
const BLOOM_DELAY_S: f32 = 0.012;
/// The bloom must peak inside its own note: delay and attack scale with the
/// note's τ_v against the 4 cps reference (1.0 at prose tempo), floored here so
/// the strike-then-bloom order stays audible. At 10 cps and faster the head is
/// half: 6 ms behind the mallet, peaking 15 ms in.
const BLOOM_HEAD_MIN_MUL: f32 = 0.5;
/// The bloom's decay ceiling. The tine speaks; the bloom hangs, so notes
/// overlap and answer each other in the harmonics, never the fundamentals.
///
/// The decay is [`BLOOM_DECAY_TAU_MUL`] × the note's τ_v under this ceiling.
/// A fixed decay stacks at speed until the 2-7 kHz band is an unbroken sheet;
/// scaling with τ_v keeps notes separate at 10-20 cps.
const BLOOM_DECAY_S: f32 = 0.340;
/// The bloom decay multiplier: 3.1 × τ_v at the 4 cps reference (110 ms) hits
/// the 341 ms ceiling, so prose tempo is unchanged and the decay shortens only
/// as typing speeds up (149 ms at 10 cps, 87 ms at 20 cps), within 3-6× τ_v.
const BLOOM_DECAY_TAU_MUL: f32 = 3.1;
/// The tail law's `3τ + 20 ms`, as the tine's own `dur` is built — computed
/// per note now, off the decay above; this is its value at the anchor.
const BLOOM_DUR_S: f32 = 3.0 * BLOOM_DECAY_S + TINE_DUR_TAIL_S;
/// A slow twinkle on the hang — 5.5 Hz is well under the 15 Hz roughness
/// band, so it reads as shimmer and never as buzz.
const BLOOM_TW_RATE: f32 = 5.5;
const BLOOM_TW_DEPTH: f32 = 0.22;
/// The bloom's roof sits this far above the note's own: its partials live at
/// 3f-6f and a roof fitted to the fundamental would take the top off them.
const BLOOM_ROOF_ADD_HZ: f32 = 2400.0;
/// The bloom's level re the step it blooms behind, before [`hue_air`].
///
/// Fitted on the bench, not taken from §3.3 (whose 0.30 was inaudible): 3.0
/// pushes the prose centroid past the 1600 Hz glass line and swallows a third
/// of a 20 cps burst's onsets; 1.5 lands on §3.3's predicted `hi>2k` of 0.04;
/// 2.0 keeps the prose centroid in §3.3's 1250-1400 Hz window with the burst
/// still articulate, within §8's +1 dB loudness budget. Under budget pressure
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

/// A capital's lift on the walk, in degrees (§3.1): one onset, its own step,
/// lifted. Inside a word it is bounded by [`WORD_LEAP_MAX_DEG`].
///
/// Three degrees (a fifth) is plainly a lift without arming into the register
/// ceiling from its middle; larger lifts reflect at the bound and fold back
/// down, so a run of capitals would plateau or lose its contour
/// (`a_run_of_capitals_keeps_its_contour_instead_of_stacking_on_the_ceiling`).
/// The capital's accent is otherwise [`forte`] and its ring.
const CAPITAL_LIFT_DEG: i32 = 3;

// ---------------------------------------------------------------------------
// §22 — flow's voice: the music box opens as the hand finds its run.
//
// Flow is an earned counter (keys typed at `disp >= 0.8` with no delete; a
// delete, a Kill or a slow key zeroes it), published as `EventMeta::flow`,
// 0..1. It is only a lerp parameter here: every quantity is
// `lerp(shipped, open, flow)`, so heat 0 is the cold instrument exactly.
//
// Three levers, each adding instrument rather than notes or decibels:
//
// 1. The octave echo: every lit step gets an octave swell a beat behind the
//    strike, riding the step's own keystroke (no degree, rhythm or key of its
//    own).
// 2. The bass dyad rings longer: body underneath without a new onset.
// 3. The tine's octave partial `P2_LVL` warms: a timbre change, with the
//    strike's sum conserved.
//
// §9.6: speed may buy brightness, never a decibel.
// `the_box_opens_with_the_hand_and_never_gets_louder` guards every lever
// against the 4 cps cold reference.
// ---------------------------------------------------------------------------

/// The flow echo's delay: 25 ms, so the ear hears one note with a longer body,
/// well inside a flowing hand's IOI (83 ms at 12 cps).
///
/// It stays a fixed time: [`TrailSynth::spawn`] draws each oscillator's phase
/// independently, so tuning the delay to the note's period cannot fix the
/// relative phase; [`FLOW_ECHO_PHASE`] does that.
const FLOW_ECHO_DELAY_S: f32 = 0.025;
/// The flow echo's start phase: three quarters of a cycle on the strike's own
/// octave (pinned by `the_flow_echo_always_adds_to_the_octave`).
///
/// The echo's fundamental equals the strike's [`P2_RATIO`] partial exactly, so
/// the two interfere. With drawn phases the echo's contribution ranged from
/// adding body to cancelling. In phase is loudest but raises the composite
/// peak at 12 cps (§9.6: never a decibel); at quadrature the pair sums in
/// power on every key. 0.75 beats 0.25 on both body and peak, because the
/// fundamental, the 2.760 partial and the roof's phase shift are also in the
/// window.
const FLOW_ECHO_PHASE: f32 = 0.75;
/// The flow echo's interval: one octave, five degrees on this lattice
/// ([`penta`] is `div_euclid(5)` over [`super::PENTA`]). A separate constant
/// from [`CAPITAL_LIFT_DEG`], so tuning the capital's register cannot turn the
/// echo into a beating sixth.
const FLOW_ECHO_OCTAVE_DEG: i32 = 5;
/// −12 dB re the step it follows, at heat 1 (the level [`ANSWER_LEVEL`] uses in
/// the same lane). At exactly zero flow the voice is not spawned at all.
const FLOW_ECHO_LEVEL: f32 = 0.25;

/// Key headroom at full flow (−0.4 dB): conserving the strike's partial sum
/// does not bound its sum with the bloom and echo at independent phases. Paid
/// on a square-root curve so the early crest is covered at quarter heat; cold
/// is identity. The rendered tests cover their stated corpus, rates and heats,
/// not every sequence or seed.
const FLOW_KEY_HEADROOM: f32 = 0.954_992_6;

fn flow_key_headroom(flow: f32) -> f32 {
    lerp(1.0, FLOW_KEY_HEADROOM, flow.sqrt())
}

/// The capital rings: a shifted glyph is one strike and one ring, the ring in
/// [`LANE_GRAFT`] at −6 dB re the key's gain, [`CAP_RING_LIFT_DEG`] above the
/// octave. It replaces the flow echo on a shifted key
/// (`CAP_RING_LEVEL.max(FLOW_ECHO_LEVEL × flow)`), so there is one voice, not
/// two. The bloom does not read `shifted`.
///
/// Louder rings push a shouted line over the ceiling pinned by
/// `the_ting_and_a_word_opening_capital_crest_no_higher_than_before_forte`;
/// the ring's presence comes from its register, clear of the strike's
/// partials.
const CAP_RING_LEVEL: f32 = 0.501_187_2;
/// The capital's ring sits three pentatonic degrees past the octave
/// ([`FLOW_ECHO_OCTAVE_DEG`] + this): a twelfth. The strike stays the line's
/// own note, played [`forte`]; the flow echo stays at the octave
/// (owner, 2026-09-27).
///
/// On this lattice (`PENTA` = C D E G A, just), the ring over the strike:
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
/// Every ring is a lattice tone and none lands on a partial of the strike's
/// tine (`1f`, [`P2_RATIO`], [`P3_RATIO`]), so there is no phase lock. Forte's
/// lower FM sideband ([`FORTE_FM_RATIO`] 4) sits at `3f`, but its index decays
/// ([`FORTE_FM_TAU_S`]) to under −20 dB re the ring by [`CAP_RING_DELAY_S`]
/// (`the_capital_ring_is_a_twelfth_on_its_own_phase_and_its_draws_are_spent_either_way`).
/// The highest ring (over G6) is D8, 4709 Hz at `song_key` 0, under the key's
/// lit roof.
const CAP_RING_LIFT_DEG: i32 = 3;
/// When the ring opens: 60 ms delay (with a 30 ms swell), where the strike has
/// decayed to 0.37-0.45 and the bloom is past its crest, so the ring adds
/// neither peak (§9.6) nor an onset to the fine census. Earlier, it crested
/// with the bloom and glint.
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
/// 3); exactly `(P1_LVL, P2_LVL)` at `flow == 0.0`.
///
/// The strike's sum is conserved: P2 lerps up and P1 down by the same amount,
/// so `p1 + p2 + p3` is 0.78 at every heat. Warming P2 alone raises the onset
/// peak (the coincident partials all start at their peak), which §9.6 forbids;
/// flow instead moves the P1:P2 ratio from 3.13:1 to 2:1.
fn flow_partials(flow: f32) -> (f32, f32) {
    let p2 = lerp(P2_LVL, FLOW_P2_LVL, flow);
    (P1_LVL - (p2 - P2_LVL), p2)
}

/// A passing (non-chord-tone) step is 2 dB under a lit one (§9.2). The chord
/// lights the verse; it never moves it (§10.3).
const PASSING_LEVEL: f32 = 0.794_328_2;

/// §9.6's loudness arc: `g_IOI = clamp(√(IOI_s / 0.25), 0.45, 1.0)`.
///
/// Replaces a flood governor that ducked only the typed voices (§16 row 11).
/// The √ law holds per-second energy flat as the rate climbs: below the
/// reference `rate · g² = 1/REF`, so every added note is paid for (A14: within
/// +1 dB of 4 cps).
///
/// The reference is 0.25 s ([`IOI_DEFAULT_MS`], 4 cps), the 0 dB point of
/// §9.6's table, so the cap's knee sits on the reference and the arc is flat
/// from 4 cps up. The floor 0.45 = `√(0.050/0.25)` is reached at 20 cps; a
/// higher floor would make the melody louder the faster you type.
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
/// −21.0 dBFS at host volume 0.4, heat 0.5 (§9.1, A28).
///
/// Fitted on peak (the arc may buy brightness, never loudness), with §9.6's
/// velocity and pan jitter divided out. `v2_the_tine_is_small_and_warm` pins
/// the centroid and `the_isolated_step_lands_on_the_ladder_floor` the peak.
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

/// One entry of §10.3's eight-bar loop: `(root, dyad partner ratio, LIT_SET)`.
///
/// The partner is 3/2 above the root where that stays under C5, else 4/3 below,
/// so every dyad is an exact 3:2 or 4:3 inside [261.6, 436.0] Hz (A6).
/// `LIT_SET` is a bit per verse degree mod 5 (C D E G A = 0 1 2 3 4).
///
/// The chord never transposes the verse: it only marks which notes are lit
/// (chord tone: lit roof, 0 dB, hero-eligible) or passing (plain roof,
/// −2 dB). About 80 % of steps land lit.
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

// The rainbow sky: the drifting bed (THE PRISM §3.2).

/// C3: two octaves under the tine and an octave under the bass dyad
/// ([`BASS_BASE_HZ`]). Every pad tone is `penta(BED_BASE_HZ, d)` for a lit
/// degree `d`, so the pad's top (degree 4, 218 Hz) stays below the bass
/// register (`the_sky_pad_is_the_live_chords_lit_degrees_under_the_bass`).
pub(super) const BED_BASE_HZ: f32 = TINE_BASE_HZ * 0.25;
/// The number of tournament chords the sky voices — [`CHORD_LOOP`]'s length,
/// exported so the parent's lattice law can walk them.
pub(super) const SKY_BED_CHORDS: usize = CHORD_LOOP.len();
/// Voicing balance, lowest tone first: the root-most tone carries, the upper
/// two colour. Static — the motion lives in pitch and in the hue.
const BED_WEIGHT: [f32; 3] = [1.0, 0.80, 0.65];
/// The pad never steps: portamento between chords, long enough that no
/// crossing is a step, short enough that a fast run through words audibly
/// moves.
const BED_GLIDE_TAU_S: f32 = 2.6;
/// The pad's level: air, not a drone; noticed when it stops, never when it
/// starts. It sits near −47 dBFS, at least 10 dB under the typed line at 4 and
/// 10 cps. It stays at this level because the walk's pitch distribution is
/// nearly flat, and this pad, voiced from [`CHORD_LOOP`], is what says where
/// home is.
const BED_LEVEL: f32 = 0.008_9;
/// A 12 s breath on the whole pad. Weather, not tremolo.
const BED_BREATH_HZ: f32 = 1.0 / 12.0;
const BED_BREATH_DEPTH: f32 = 0.35;
/// The hue's one-pole (τ, seconds) on the arc position the pad reads.
const BED_HUE_TAU_S: f32 = 1.2;
/// Tilt: one one-pole over the pad sum (reusing `bed.lp1`). Red end of the arc
/// warm and closed, cyan end open: the visible colour is the audible one.
///
/// The arc's whole travel (350 → 900 Hz) stays under the melody's register so
/// the pad's upper partials do not draw lines through the notes, while its
/// brightness still moves by the 1.3× that
/// `the_sky_follows_the_hue_in_tilt_and_width_and_never_in_pitch` requires.
const BED_TILT_LO_HZ: f32 = 350.0;
const BED_TILT_HI_HZ: f32 = 900.0;
/// Width: the top tone's twin is detuned by this many cents, so the pad's beat
/// rate walks from ≈ 0.45 Hz (4 cents on G3) to ≈ 1.4 Hz (11 cents on A3) as
/// the ribbon travels the arc. That slow beat is the shimmer.
const BED_DETUNE_CENTS_LO: f32 = 4.0;
const BED_DETUNE_CENTS_HI: f32 = 11.0;
/// The pad's partials, harmonic numbers at `1/n`: octaves, a fifth and nothing
/// else, all lattice intervals from their tone (no 7th harmonic). They give
/// the hue's tilt something to act on.
///
/// The series stops at 4: a 6 dB/octave tilt cannot remove partials that sit
/// in the melody's register (5, 6 and 8 of the 218 Hz tone land at
/// 1090-1744 Hz). The top remaining partial is 872 Hz, under the arc's
/// ceiling.
const BED_PARTIALS: [u32; 4] = [1, 2, 3, 4];
/// `1 / sqrt(Σ 1/n²)` over [`BED_PARTIALS`], so a pad tone has the RMS of the
/// sine it replaces and [`BED_LEVEL`] means what it says:
/// `1/√(1 + 1/4 + 1/9 + 1/16)` = 0.838_1.
const BED_PARTIAL_NORM: f32 = 0.838_1;

/// The sky's voicing for one chord of [`CHORD_LOOP`]: the chord's three lit
/// verse degrees (`Chord::lit`, bits 0..5 = C D E G A), ascending, on the
/// untransposed lattice. Voiced from the lit set rather than a root triad,
/// which on G or A would put an out-of-key B, C♯ or F♯ under the melody:
/// C E G on I, C E A on vi and IV, D G A on V.
pub(super) fn sky_bed_degrees(chord: usize) -> [i32; 3] {
    let lit = CHORD_LOOP[chord % SKY_BED_CHORDS].lit;
    let mut out = [0i32; 3];
    let mut n = 0;
    for d in 0..5 {
        if lit & (1 << d) != 0 && n < 3 {
            out[n] = d;
            n += 1;
        }
    }
    debug_assert_eq!(n, 3, "every chord lights exactly three degrees");
    out
}

/// `a + (b - a) t`.
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
/// The bass dyad's decay (§22, "felt, not heard", dur 300 ms): the
/// tail law (A13) gives `300 / 2.7 = 111 ms`, ending at ≤ 7 % of peak.
const BASS_DECAY_S: f32 = 0.111;
const BASS_ROOF_HZ: f32 = 900.0;
/// The word downbeat's level, −4 dB re the step. With the head's twinkle
/// ([`SPACE_TWINKLE_LEVEL`]), which gives the dark dyad energy above 2 kHz that
/// a laptop speaker can reproduce, the rendered head lands −4..−5 dB re the
/// typed letter: heard, never over it. §9.7's "never louder than a keystroke"
/// is pinned by `the_ladder_holds_for_the_key_family` and
/// `a_space_head_is_heard_and_never_over_the_letter`.
///
/// The dyad's 111 ms τ is unchanged, so its ring under the tune grows only by
/// the level. The verdict's dyads keep their own level
/// ([`VERDICT_DYAD_LEVEL`]): an exit code is told once per command.
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
/// The breath's roof. The engine's per-voice lowpass has
/// `lp_k = clamp(lp_cut · dt · 2π, 0, 1)`, so a roof of 0 silences the voice
/// entirely; the breath needs an open roof. The 900 → 380 Hz band-pass shapes
/// the exhale, and the head's twinkle passes through it. Independent of the
/// ting's roof.
const BREATH_ROOF_HZ: f32 = 9000.0;
/// The word boundary's twinkle: the head's breath carries three partials, so
/// the downbeat gets a top with no new voice, lane or draw. Partial 1 is the
/// dyad's root octave-folded into the stardust band (`[GLINT_LO_HZ,
/// GLINT_HI_HZ)`, 2800-5600 Hz, §13: C4 → 4186, F4 → 5581, G4 → 3139.5,
/// A4 → 3488 Hz), the chord's own pitch class, so it never beats against the
/// verse; partial 2 is the octave under it ([`SPACE_TWINKLE_UNDER`]). Both
/// share the breath's envelope (8 / 55 / 149 ms; [`BREATH_DUR_S`] is 2.7 τ)
/// (owner, 2026-09-16).
///
/// A partial level on the breath voice (× [`BREATH_LEVEL`]): 0.5 × −16 dB =
/// −22 dB re the step, 8 dB under the dyad's root. Fitted by rendering;
/// `a_space_head_is_heard_and_never_over_the_letter` pins the result.
const SPACE_TWINKLE_LEVEL: f32 = 0.5;
/// The twinkle's octave-under partner, at −8 dB under it.
const SPACE_TWINKLE_UNDER: f32 = 0.2;
/// The indent steps: a run's head is the one downbeat (the dyad, one chord
/// step); every later space of the run plays a short soft tine beside its
/// breath, climbing the live chord's lit degrees (§10.3's `LIT_SET`, so every
/// step is a chord tone, consonant with the dyad and the sky pad) from the
/// first lit degree over the dyad's root up to the last under
/// `4 × BASS_BASE_HZ` (1046 Hz), then starting over ([`indent_step_hz`])
/// (owner, 2026-09-16).
/// Steps share the tune's lower octave (C5..C6) as chord tones only and never
/// reach its upper octave. They move nothing: not the chord, the playhead or
/// the beat.
///
/// Level: between the breath ([`BREATH_LEVEL`], −16 dB) and the dyad
/// ([`BASS_LEVEL`], −4 dB). Each step is heard, and the run's peak stays the
/// head's own. In [`LANE_GLINT`] (cap 3, drops the newcomer), so a step never
/// steals the dyad, the breath or a tine.
const SPACE_STEP_LEVEL: f32 = 0.35;
/// The step's τ: a tick that climbs, gone in 155 ms (`3τ + 20 ms`) — under
/// the 100 ms of a hand tapping at 10/s, so the figure reads as a figure.
const SPACE_STEP_TAU_S: f32 = 0.045;
/// The step's rate gate on the cue clock, against the run's head or the last
/// admitted step ([`super::SPACE_STEP_MIN_GAP`] in ms): a hand tapping 80-125 ms
/// apart hears every tap; a held spacebar (~30 Hz autorepeat) gets one step
/// per 75+ ms (`a_run_of_spaces_is_one_downbeat_and_a_rising_figure`).
const SPACE_STEP_MIN_GAP_MS: u32 = 75;
/// The steps' ceiling: two octaves over [`BASS_BASE_HZ`]. The figure climbs to
/// just under it and starts over ([`indent_step_hz`]).
const SPACE_STEP_HI_HZ: f32 = BASS_BASE_HZ * 4.0;

/// The indent step's pitch: the `n`-th step of the run's rising figure (`n` is
/// 1 for the first admitted step, [`MelodyV2::space_steps`]).
///
/// The figure is the live chord's lit degrees on the keyed lattice, from the
/// first strictly above the dyad's `root` to the last under
/// [`SPACE_STEP_HI_HZ`], then the same notes again: a cycle, strictly rising
/// within each pass. On chord I over C4 that is E4 G4 C5 E5 G5; on vi over A4
/// it is C5 E5 A5.
///
/// The degree is reduced modulo the figure's length before the lattice is
/// read, so every `n` yields a note of the figure, below the ceiling
/// (`the_indent_figure_is_a_cycle_on_every_chord_and_key`). Total: the search
/// is bounded, the cycle has at least one note, and the halving loop runs only
/// when that one note is itself at the ceiling.
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

// ===========================================================================
// §10.4 — the class voices: the wood bar (digits)
// ===========================================================================
//
// The glyph class ([`EventMeta::glyph_class`], from
// `trail_sound::glyph_class`) reaches the synth in two places: `v2_typed`
// reshapes the key's tine and may add a decoration in a decoration's lane,
// and [`MelodyV2::on_typed`] lets a phrase mark steer its own note
// (owner, 2026-09-08).
// `. ! ? , ; :` land on a cadence degree ([`cadence_target`]), `-` ties, `)`
// returns toward its `(`, and the Space after a mark confirms the cadence in
// the bass ([`MelodyV2::on_space`]). A letter's and a digit's degree come only
// from the rank and the hand (digit ranks 27..36). Every steered note is a
// function of the text: no key is gated, skipped or delayed, so the same text
// at any speed is the same line.

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
/// within the bound holds its note (owner, 2026-09-12).
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
/// The aside's roof, ×0.8 of the key's own: further away, not muffled. Roofs
/// are at most [`ROOF_MAX_HZ`] (7500), so the aside's is at most 6000 Hz.
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
/// with the odd harmonics 3f and 5f instead of the octave and the 2.760
/// strike. 3f is degree +8 and 5f degree +12, exactly on the lattice, so §9.5
/// law 4 holds by construction. 7f is omitted: it is off-lattice (D5 × 7 beats
/// against the C8 glint at 65 Hz), which also rules out a square-wave chip
/// pulse.
///
/// 3f at 0.26 with τ 45 ms (A11's exemption bound): the twelfth carries the
/// bar's identity; [`WOOD_P1_LVL`] follows so the onset peak does not move.
/// The digit's pitch is the ordinary walk: mapping a digit's value to pitch
/// would make a PIN audible and break [`WORD_LEAP_MAX_DEG`]; the counting glint
/// is the one value cue.
const WOOD_P2_RATIO: f32 = 3.0;
const WOOD_P2_LVL: f32 = 0.26;
const WOOD_P2_TAU_S: f32 = 0.045;
/// 5f at 0.08 with τ 30 ms: the bar's top, gone before it reads as a second
/// note. At C6 it is 2616 Hz, under the plain roof.
const WOOD_P3_RATIO: f32 = 5.0;
const WOOD_P3_LVL: f32 = 0.08;
const WOOD_P3_TAU_S: f32 = 0.030;
/// The partial sum is conserved (§9.6): the bar's three sum to the letter's
/// 0.78 (`0.44 + 0.26 + 0.08`), so the onset peak matches the letter's. Stated
/// as arithmetic so a retune of either bar partial moves P1 with it.
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
/// lattice, `COUNT_GLINT_DEG0 + digit % 5` (degrees 13..17 = G7 3139.5 /
/// A7 3488.33 / C8 4186.0 / D8 4709.25 / E8 5232.5 Hz, all inside
/// [`GLINT_LO_HZ`]..[`GLINT_HI_HZ`] with no fold), so `0 1 2 3 4` climbs and
/// `5 6 7 8 9` climbs again. A digit does not advance the rotation cursor
/// `glint_k`. On the lattice (+`song_key`).
const COUNT_GLINT_DEG0: i32 = 13;
/// `typed_glyph_rank(Some('0'))`: digits have ranks 27..36, so `rank − 27` is
/// the digit. A constant because the rank producer is not `const`; pinned
/// against it by the digit test.
const DIGIT_RANK0: i32 = 27;
/// Past five it brightens: digits 0..4 sparkle at ×2 (−18 dB re the step),
/// 5..9 at [`KEY_GLINT_SHIFTED_MUL`] (−12). Two brightnesses × five pitches
/// make ten.
const COUNT_GLINT_LOW_MUL: f32 = 2.0;

// ---------------------------------------------------------------------------
// The pluck family (punctuation)
// ---------------------------------------------------------------------------
//
// A punctuation mark is a pitched staccato pluck: the same walk, lattice and
// `song_key` as a letter or digit, with its own partial table
// ([`PLUCK_P2_RATIO`]), short τ ([`PLUCK_TAU_MUL`]) and a dark fingertip
// under the tone ([`PLUCK_NOISE_LVL`]).
//
// Every mark is plucked except four: the opening bracket and the three that
// phrase by ringing (the bang, a sforzando arrival on the forte tine; the
// dash, a tie; the sentence-ending steering `.`). Each bucket adds one
// gesture (§10.4): the steering stop breathes, `?` rises, `!` sparkles twice,
// `(` opens (the lit tine) with a grace note up, `)` is plucked with a grace
// note down, quotes sparkle twice and small, `_ ~ \ |` zip down, `/` zips up,
// `=` and the announcing `:` sound their fifth, and `@ # $ &` are just
// plucked. A pluck does not hang: no bloom, no flow echo.

/// The pluck's τ: 0.5 × the step's τ_v, floored at 25 ms (13 cycles of C5, so
/// it is heard as a pitch). Family note lengths: 1.0 / [`DIGIT_TAU_MUL`] 0.7 /
/// 0.5. At the floor `3τ + 20` is 95 ms, under 10 cps's 100 ms key gap.
const PLUCK_TAU_MUL: f32 = 0.5;
const PLUCK_TAU_MIN_S: f32 = 0.025;
/// The pluck's partials: `1f` and `4f` only (the letter has `{1, 2, 2.76}`,
/// the digit `{1, 3, 5}`). 4f is the double octave, on the lattice exactly, and
/// is what a string plucked near its end sounds like: a glassy top gone in
/// 30 ms over a strong fundamental. At D6 it is 4709 Hz, under every roof. P3 is
/// muted.
///
/// The partial sum is conserved (§9.6): `0.60 + 0.18 = 0.78`, the letter's.
/// Pinned as arithmetic by the family test.
const PLUCK_P1_LVL: f32 = 0.60;
const PLUCK_P2_RATIO: f32 = 4.0;
const PLUCK_P2_LVL: f32 = 0.18;
const PLUCK_P2_TAU_S: f32 = 0.030;
/// The fingertip: band-passed noise falling 1400 → 500 Hz over 15 ms at Q 0.9
/// (§9.5's 1.6), at 0.5, which sits 24-29 dB under the tone over the first
/// 30 ms: a note with a dark attack
/// (`the_three_families_are_distinct_timbres_on_one_line`). In
/// `Voice::weight()`'s quietest-steal order a pluck sits beside a letter.
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
/// rises about a fifth), 60 ms after the pluck (heard as a second event, before
/// the next key at 12 cps), at −3 dB re the pluck, in [`LANE_GRAFT`]. The rise
/// may sit above [`TUNE_DEG_HI`].
///
/// From a D the rise is two degrees: D + 3 is A, and D–A on this lattice is
/// the 40/27 wolf; D + 2 is G, a pure fourth. "D" is the sounding class
/// (degree in the song's key), so no `song_key` restores the wolf
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
/// 3200 → 900 Hz over 40 ms for a line drawn (`- _ ~ \ |`), 900 → 3200 for the
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
/// A stop breathes (`. , ; :` via [`stop_breathes`], only when it steers,
/// [`TypedPlan::steered`]): the space's exhale ([`breath`], −16 dB at
/// [`BREATH_LEVEL`]) 20 ms after the pluck, after its 15 ms fingertip. In
/// [`LANE_BREATH`], cap 1: a following space's breath fade-steals it, and only
/// the space has the bass dyad.
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
/// chord, the high counterpart to the Space bar's low dyad (owner, 2026-09-16).
///
/// Pitch ([`TING_TONES`], [`TING_PICK`], [`ting_fold`]): the root, fifth or
/// third of `CHORD_LOOP[chord]`, two to three octaves above the Space dyad,
/// folded into [`TING_LO_HZ`]'s octave. v1's `shift_step` cursor (shared by
/// both engines) indexes a root-weighted pick, so successive Shifts on one
/// chord ring root, fifth, root, third, fifth, never the same pitch twice
/// running. A word-head capital 60 ms later snaps to a lit tone of the same
/// chord, so ting and capital are a chord interval apart.
///
/// Timbre: P1 a pure sine (no FM, whose inharmonic sidebands clank; see
/// `a_bare_shift_is_a_ting_that_never_steps`); the octave at [`TING_P2_LVL`]
/// on an extra decay; the tine's 2.76f clink at [`TING_P3_LVL`], gone in 30 ms
/// (A11); a quiet felt mallet ([`TING_MALLET_LVL`]); a 200 ms body τ; a roof
/// that is a real filter ([`TING_LP_HZ`]).
///
/// It rings through the key it announces: `push_v2` does not damp it on the
/// next keyed cue. A second Shift replaces it, as does a strike landing on its
/// pitch (§9.5 law 5; see `v2_typed`). It is never a step:
/// `MelodyV2::on_typed` is not called and walk, steps, playhead and
/// `since_voice` are untouched.
///
/// A 3 ms strike: §9.5 law 1's 4 ms floor applies under 1 kHz; at 1.2-2.5 kHz
/// 3 ms is four cycles.
const TING_ATTACK_S: f32 = 0.003;
/// The ring: a 200 ms τ, −10.8 dB re peak 250 ms after the key. v1 keeps its
/// own [`super::SHIFT_TING_DECAY_S`].
const TING_DECAY_S: f32 = 0.200;
/// [`super::RING_OUT_PER_TAU`] × τ + the release ramp (1.005 s), v1's ting
/// ring-out law ([`super::SHIFT_TING_DUR_S`]) on v2's τ: the ting is at −43 dB
/// before the 5 ms ramp starts
/// (`the_ting_decays_out_instead_of_being_cut`).
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
/// The ting's register, `[1240, 2480)` Hz, v2's own (not v1's
/// [`super::SHIFT_TING_LO_HZ`], 1318.5). On this lattice E6 is 1308.125 Hz,
/// under v1's floor, which would double it to a shrill 2616 Hz. At
/// `song_key` 0 the ting is C7 2093.0, G6 1569.75, E6 1308.125, A6 1744.17 or
/// D7 2354.6 Hz.
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
/// −4.5 ± 1.0 dB re the walk-mean typed key. On the isolated-seed ladder
/// (`the_ladder_holds_for_the_key_family`) it sits just over that test's
/// music-box floor of 0.50 of the first keystroke, which bounds it from below.
/// The capital crest is answered by [`TING_DUCK`], not this level. No velocity
/// draw and no `g_ioi`: a modifier has no IOI.
const TING_LEVEL: f32 = 0.49;

/// The bell yields to the capital it announced: when a shifted letter that
/// opens a word or shifted run lands ([`Forte::ring`]), a live ting eases to
/// this fraction of its level over [`TING_DUCK_RAMP_S`] and keeps ringing
/// there, decaying on its own τ (`the_ting_rings_through_the_next_key`).
///
/// Unducked, the 200 ms bell and the capital crest together over the −14.5
/// dBFS ceiling on the session's first key (§0, F16); at 0.66 the bell is
/// under the capital at the fraction of peak a shorter tick would have had.
/// A Shift before a lowercase letter, a digit or a Space rings untouched
/// (`the_ting_is_musical_not_harsh`,
/// `the_ting_decays_out_instead_of_being_cut`); a shifted mark takes
/// [`TING_DUCK_MARK`].
const TING_DUCK: f32 = 0.66;
/// The duck under a shifted mark that is not a capital letter (`( ) { } & +
/// : ? !` and kin): −6 dB, the same 12 ms glide, never a damp.
///
/// A one-second bell over a 25 ms pluck is a drone, and code has many shifted
/// marks: unducked, the bells made a line of code louder than prose past the
/// design's C1 bound (+1.5 dB). Below a half the curve is flat (much of the
/// bell's energy is spent before the key lands), so a half is taken
/// (`a_line_of_code_is_no_louder_than_prose`). A capital keeps [`TING_DUCK`].
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
/// The STARDUST lane (§9.4), into which every glint is octave-folded: every
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
/// step.
///
/// The glint buys brightness without costing note separation, unlike a louder
/// bloom: 2 ms attack, 40 ms decay, octave-folded into
/// [`GLINT_LO_HZ`]-[`GLINT_HI_HZ`] on [`GLINT_DEGREES`], and gone
/// ([`GLINT_DUR_S`] 108 ms) before the next key at any typing speed. It
/// arrives at [`KEY_GLINT_DELAY_S`], with the bloom.
///
/// It ignores the hue ([`hue_air`] colours the bloom, not the sparkle) but
/// takes §9.6's loudness arc: the caller multiplies by the same `g_ioi` as the
/// strike and bloom, so energy per second stays flat and the sparkle does not
/// overtake the melody at speed (`the_sparkle_is_under_the_loudness_arc`).
/// It takes `g`, not `plan.level` (§9.2's passing-note attenuation): a passing
/// note gets the full sparkle too.
const KEY_GLINT_LEVEL_MUL: f32 = 0.5;
/// A shifted key's sparkle multiplier: ×4, −12 dB re the step, clearing the
/// masked threshold of the tine's partials spreading into 3-5 kHz. A glint off
/// the crest at [`KEY_GLINT_DELAY_S`] with a 40 ms τ, so it costs no decibel
/// at speed.
///
/// Read by the digit 5-9 counting glint and the OPEN / BANG class rule. A
/// shifted letter uses [`FORTE_GLINT_MUL`] instead; a shifted mark follows
/// its class.
const KEY_GLINT_SHIFTED_MUL: f32 = 4.0;

/// A shifted key is forte, not higher: the same note struck harder, with no
/// pitch bend (`a_shifted_key_does_not_bend`) and the line's lit roof as a
/// real filter. Kept alongside: [`WORD_CAPITAL_GAIN`], [`CAPITAL_LIFT_DEG`]
/// (the walk's accent where a capital opens something),
/// [`super::SHIFT_GLYPH_GAIN`] on letters and the bang, and the host's rule
/// that a committed uppercase glyph counts as shifted. [`forte`] and
/// [`Forte::of`] give the hammer-force table, the phase-modulated hardness and
/// the richer, longer body: brightness at 0 dB of structural peak, mellowing
/// like a struck string.
///
/// The hammer's hardness is harmonic phase modulation on the fundamental: the
/// render computes `sin(ph + idx·sin(fm_ph))`, constant-amplitude, so the
/// sidebands come out of the carrier, never onto the peak. At ratio 4 they sit
/// at 3f, 5f, 7f and 9f: never on the fundamental, the tine's 2f or its 2.76f,
/// and integer multiples of a lattice pitch.
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
/// The hammer's modulator is locked to its carrier
/// ([`TrailSynth::v2_lock_hammer`]): in turns, where the ratio-4 modulator
/// stands when the fundamental crosses zero going up.
///
/// The angle between them shapes the struck cycle: to first order
/// `sin θ + (i/2)·(sin(3θ + c) + sin(5θ + c))`, flat-topped at `c = 0` and
/// spiked at half a turn. The partial alone has 0 dB structural peak at any
/// angle; summed with its octave, mallet and the preceding ting it does not.
/// The key carrying [`WORD_CAPITAL_GAIN`], the only one loud enough for the
/// crest to bind, is struck flat-topped to stay under the −14.5 dBFS ceiling.
const FORTE_FM_TURN_HEAD: f32 = 0.0;
/// Every other forte key is struck spiked, because its floor binds from the
/// other side: a capital's sample peak is ≥ +2.0 dB over its plain self (F3),
/// which the gain alone does not deliver (the plain twin sits on another
/// degree under another noise draw). 0.54 is the top of a narrow plateau; the
/// pin's tightest take clears the floor by under 0.01 dB.
const FORTE_FM_TURN: f32 = 0.54;
/// How much level the octave takes from the fundamental at force 1: a
/// transfer, so `Σ p.lvl` is unchanged. Sum-conserved is peak-conserved but
/// not energy-conserved (the octave decays faster, [`P2_TAU_S`]), so larger
/// transfers push a mid-word capital under the ≥ +2.0 dB floor
/// (`a_capital_rings_and_out_peaks_its_plain_self_by_its_weight`)
/// (owner, 2026-09-16).
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
/// weight ([`super::SHIFT_GLYPH_GAIN`], +2.6 dB), +5.6 dB over its plain self,
/// so a word-opening capital stands clear of the text around it rather than
/// at the loudness JND. Not a shifted mark and not an interior capital, which
/// keeps shouting to one accent per word. At the host default volume an
/// isolated step is −21 dBFS, so the head crests near −15.4, under the bus
/// limiter's −14 dBFS knee.
const WORD_CAPITAL_GAIN: f32 = 1.412_537_5;

/// The key sparkle arrives with the bloom, not before the note:
/// [`BLOOM_DELAY_S`] + [`BLOOM_ATTACK_S`], the instant the bloom opens.
///
/// A glint at delay zero ([`GLINT_ATTACK_S`] 2 ms against [`TINE_ATTACK_S`]
/// 4 ms) peaks inside the strike's attack and adds phase-random jitter to the
/// crest; a peak is a max over keys, so the jitter only ever raises it
/// (`the_box_opens_with_the_hand_and_never_gets_louder`). Moving the arrival
/// keeps the sparkle's level and spectrum while taking it off the crest; a
/// level trim large enough to hide the jitter would erase the sparkle.
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
/// The tick's life, 12.5 ms: [`super::RING_OUT_PER_TAU`] × 1.5 ms of decay
/// (−43 dB), then the engine's 5 ms release ramp ([`super::RELEASE_RAMP_S`]),
/// so the crest at ~0.5 ms renders at the envelope's own height rather than
/// inside the ramp. Well inside `LANE_METEOR`'s cap 3 (tick, core, whoosh)
/// and the arm's 150 ms ttl.
const MET_TICK_DUR_S: f32 = super::RING_OUT_PER_TAU * MET_TICK_DECAY_S + super::RELEASE_RAMP_S;
/// The tick's level, fitted so the rendered peak lands at §12.2's −24 dB re the
/// tine step. As a gain the figure would render ~16 dB low: the envelope
/// crests at 0.58, a 2 ms Q-0.7 noise burst crests ~8 dB under its input. The
/// noise crest varies ±3.5 dB by seed; `the_meteor_tick_is_heard` reads ten
/// seeds inside −24 ± 5 dB and their median inside ±1.5.
const MET_TICK_LEVEL: f32 = 0.28;
/// The tick's roof. A roof of 0 (`Voice::default()`) silences the voice, see
/// [`BREATH_ROOF_HZ`]. The whoosh's figure ([`MET_WHOOSH_LP_HZ`]) passes the
/// 3200 Hz the tick starts at, so the band-passed noise is what §12.2
/// describes (`the_meteor_tick_is_heard`).
const MET_TICK_ROOF_HZ: f32 = MET_WHOOSH_LP_HZ;

/// Layer 2, core: the layer that carries direction. A sine gliding an octave
/// up (rightward / upward) or down over the flight, with a light inharmonic FM
/// colour that dies with it. Direction follows `crate::rainbow_kitty::Dir::sign`
/// (`+1` for rightward or upward), the sign `SoundCue::dir` carries.
///
/// The core's octave is `[C5, C6)`, derived from [`TINE_BASE_HZ`] so a retune
/// moves it with the tine.
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

// ===========================================================================
// The verdict: a command's exit code, sounded once on the `D` that carries
// it. Armed by the keyed Enter and spent by the first `D`; the host never
// forwards a `D` with no armed Enter, so program output never speaks.
// ===========================================================================

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
/// gesture may be (under
/// `the_verdict_is_the_music_boxs_alone_and_never_louder_than_a_key`'s
/// ceiling). Independent of the word downbeat's [`BASS_LEVEL`].
const VERDICT_DYAD_LEVEL: f32 = 0.501_187_2;
/// The gesture's voice trim: −1 dB on every verdict voice. A bass dyad with a
/// tine over it sums to a peak above any one level; untrimmed, a green cadence
/// lands over its ceiling. Trimmed, the delivered peak is −6.14 dB re a
/// keystroke (`the_verdict_is_the_music_boxs_alone_and_never_louder_than_a_key`).
const VERDICT_PEAK_TRIM: f32 = 0.891_250_9;
/// The RED dyad, −9 dB: three under the green. A failure is told, not
/// announced — you already know; the cat is only agreeing with you.
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

/// Which of the tine's three touches a key gets (§9.2). There are only three,
/// two at the same pitch: nothing here can voice a degree the line did not
/// derive.
///
/// [`Touch::ReStrike`] is reached only through [`stride_mag`]'s single zero, a
/// doubled letter: a real repeat from a real repeat in the text.
/// [`MelodyV2::derive`] closes every other way back onto the sounding degree
/// (fold to zero, gravity cancelling a second, reflection), so the same-pitch
/// damp guards against a comb filter rather than punishing a fast typist.
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
    /// The derived line's state (§3.1) rides this frame: the melody is a
    /// function of the text, so un-writing a character must un-write
    /// everything it decided.
    ///
    /// The previous key's alphabet rank ([`EventMeta::rank`]), the interval's
    /// left operand. `0` means nothing to measure against yet; a cue with no
    /// rank never clobbers it, so an echo-born cue cannot make the next real
    /// letter leap.
    prev_rank: u8,
    /// The stride the line is repeating, and how many times running —
    /// [`MELODY_RUN_MAX`]'s whole state.
    run_stride: i8,
    run_len: u8,
    /// The line's subject: the first [`MOTIF_LEN`] intervals since the last
    /// Enter or rest, and how many have been written.
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
    /// The phrase's state: a mark moves its own note and books the next
    /// Space's chord, so un-typing a mark un-books both; all four fields ride
    /// the frame.
    ///
    /// The pending mark: what the last typed key said about the phrase,
    /// [`MARK_NONE`] or one of [`MARK_PERIOD`] … [`MARK_COLON`]. Every typed key
    /// writes it (a letter writes `MARK_NONE`), so `foo.bar`, `3.14` and
    /// `v0.88.0` never reach the harmony: only a mark that is still the last
    /// thing typed when the Space arrives becomes a chord
    /// ([`MelodyV2::on_space`]). Space does not clear it (the next word head
    /// reads it); Enter, a line feed and a kill do. A doubled mark sets
    /// [`MARK_DOUBLED`] and [`MelodyV2::pending_mark`] then reads 0.
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
    /// [`ASIDE_DEPTH_MAX`], and typed keys sounded inside the aside
    /// ([`ASIDE_MAX_KEYS`]). Sotto voce at depth ≥ 1. Closed by a close that
    /// brings depth to 0, an Enter, a line feed, a kill, a phrase rest or the
    /// 32nd key. On the frame so un-typing a bracket restores the aside.
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
/// by `EventMeta::at_ms`.
///
/// Time comes from the host's input clock, never from audio-thread arrival
/// gaps, so the keyed and frame paths agree on phrase boundaries (§10.1). No
/// clock reads, allocation or randomness: the same events, seed and stamps
/// produce the same notes (A27).
#[derive(Clone, Copy, Debug)]
pub struct MelodyV2 {
    /// The text's state, the live [`Undo`] frame (`push_undo` saves it,
    /// `pop_undo` restores it).
    text: Undo,
    /// Keystrokes that have moved the melody: one per typed key, without
    /// exception (test / introspection hook; the census's `steps` column).
    steps: u32,
    /// `at_ms` of the last admitted key (any touch), the IOI's clock; `None`
    /// until the first, so the first keystroke does not see an enormous gap
    /// and cadence an unplayed phrase.
    last_key_ms: Option<u32>,
    /// `at_ms` of the last typed key that actually spawned a tune voice.
    last_onset_ms: u32,
    /// The autorepeat detector's state: the previous raw gap and how many gaps
    /// of the machine-regular run have agreed. Not on the undo frame: it
    /// describes the hand, and a Backspace does not un-hold a key.
    prev_gap: u32,
    repeat_run: u8,
    /// Inter-onset interval, EMA'd, clamped 30..600 ms.
    ioi_ms: f32,
    /// True while inside a whitespace RUN: only its head is a downbeat.
    space_run: bool,
    /// Indent steps the run has admitted (0 at the head): the step's index
    /// into the rising figure ([`indent_step_hz`]). Counts steps that played,
    /// not spaces, so the figure climbs one lit degree per sounded step. Reset
    /// at the run's head; saturating.
    space_steps: u32,
    /// When the run's head or its last admitted indent step sounded (ms on
    /// the cue clock): the step's own gate, [`SPACE_STEP_MIN_GAP_MS`].
    last_step_ms: u32,
    /// The timbre ladder's stops (§7 step 3): which of §3.3's additions this
    /// synth voices. All on in production; the bench toggles them one at a
    /// time.
    stops: TimbreStops,
    /// Flow heat for the cue being voiced (§22), 0..1: latched from
    /// [`EventMeta::flow`] at the top of [`TrailSynth::push_v2`] and read by
    /// every gesture that push mints. A property of the instrument at this
    /// instant, not melody state: nothing derives from it, no undo frame
    /// carries it, and it is never read at render time.
    flow: f32,
    /// Position in [`GLINT_DEGREES`].
    glint_k: u8,
    /// The last line feed of the live cascade run (D18); see
    /// [`MelodyV2::on_jump`] for why this tracks the last Jump rather than the
    /// last cascade. `None` until the first line feed, so a host whose clock
    /// starts at 0 does not have its first brrrring swallowed inside
    /// [`CASCADE_EXCLUSIVE_MS`] of zero.
    cascade_at: Option<u32>,
    /// The last top-note re-strike inside a live cascade.
    cascade_restrike_ms: u32,
    /// The run's playhead in [`SONG_THEME`] (§10.4 `on Jump`: "then an accent
    /// step"). The second and later line feeds of a run take their cascade's
    /// base from the authored theme, one phrase edge per line (a phrase's
    /// first note, then its last, then the next phrase's first), so a stream
    /// sings `0 7 5 0 2 2 0 8` and round again. The typed line stays derived.
    /// Re-headed by every lone line feed.
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
            last_onset_ms: 0,
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
    pub fn walk(&self) -> i8 {
        self.text.walk
    }

    /// Keystrokes that have moved the melody (test / introspection hook).
    /// Incremented once per typed key, unconditionally, where a key enters the
    /// melody: a census that pushes `n` typed keys and reads anything else has
    /// caught a gate.
    #[must_use]
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
    pub fn word_pos(&self) -> u8 {
        self.text.word_pos
    }

    /// Whether the line is answering its own subject: the next key replays a
    /// latched interval rather than deriving its own (test / introspection
    /// hook). The bench's repeat ledger uses it to tell an answered unison
    /// (§3.1 asks for it) from a stalled repeat (§3.1 forbids it).
    #[must_use]
    pub fn motif_answering(&self) -> bool {
        self.text.motif_play > 0
    }

    /// The smoothed inter-onset interval in ms (test / introspection hook).
    #[must_use]
    #[cfg(test)]
    pub fn ioi_ms(&self) -> f32 {
        self.ioi_ms
    }

    /// When a typed key last spawned a tune voice, on the host input clock (test
    /// / introspection hook). Written by [`TrailSynth::v2_typed`] after the
    /// spawn returned a slot, so it records what the mixer did. A key that
    /// leaves it unchanged made no sound (the TUNE lane refused the voice);
    /// the census's `silent` column must read 0.
    #[must_use]
    pub fn last_onset_ms(&self) -> u32 {
        self.last_onset_ms
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
    /// live chord nearest `deg`, searched outward to [`WORD_HEAD_SNAP_DEG`].
    /// `from` breaks ties: the snap takes the side continuing the derived
    /// contour, so a rising word head still rises.
    ///
    /// Total: five consecutive degrees carry all five pitch classes, every chord
    /// lights three, and the register bounds cut at most two candidates off one
    /// side, so the `deg` fallback is unreachable. It stays so a chord table
    /// change fails a test loudly rather than returning an unlit note.
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
    /// line's new motif. Run by an Enter and by a rest, where a line ends.
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
    pub fn in_aside(&self) -> bool {
        self.text.aside_depth > 0
    }

    /// Whether a quotation is open (test / introspection hook; [`QUOTE_OPENS`]).
    #[must_use]
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
    pub fn token_marked(&self) -> bool {
        self.text.token_marked
    }
}

/// The widest interval within a word, in lattice degrees: four, a major sixth
/// (5/3). Five is the octave, and an octave-class leap inside a word is what
/// A2 forbids.
///
/// [`MelodyV2::derive`] clamps its stride to this: [`stride_mag`] tops out at
/// 3 and gravity may add one. The word-head snap may move a further two
/// degrees, since a word head's interval is between words, which A2 does not
/// govern.
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

/// Which of §3.3's additions this synth voices: the timbre ladder's stops (§7
/// step 3: `plain` / `bloom` / `hue` / `room`). Production is
/// [`TimbreStops::ALL`]; the bench renders the rungs. A stop that is out spawns
/// nothing and reads no hue, so `plain` is the bare tine.
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
    /// [`Self::walk`] underived or [`Self::steps`] unincremented, at any rate,
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

        // A rest resolves the line: the walk resolves onto the live chord, the
        // contour bias drops and the subject re-latches; then this key derives
        // its own note as any other key would. Nothing here consumes the
        // keystroke.
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
            // A sentence does not start on the note the last one ended on.
            // Behind `. ` and `! ` the Space has put the harmony on I and the
            // stop put the line on a C or G, which I lights; a head landing
            // there would be the cadence twice, so it takes the nearest other
            // lit degree within three, ties upward. A backstop: ordinary heads
            // are already kept off that note; it binds on a head on the stop's
            // own rank under a doubled mark (`the end. ...`), pinned by
            // `a_sentence_neither_ends_nor_starts_where_the_last_one_did`.
            if matches!(said, MARK_PERIOD | MARK_BANG)
                && deg == from
                && let Some(alt) = (1..=STEER_MAX_DEG)
                    .flat_map(|k| [from + k, from - k])
                    .find(|c| (TUNE_DEG_LO..=TUNE_DEG_HI).contains(c) && self.deg_is_lit(*c))
            {
                deg = alt;
            }
        }
        // A capital is the same step, lifted ([`CAPITAL_LIFT_DEG`]), applied
        // before the run is booked and before the walk moves, so the line
        // continues from the note the ear got and the run guard counts the
        // sounded stride.
        //
        // It reflects at the ceiling ([`reflect_deg`]) rather than clamping,
        // so a run of capitals keeps a contour instead of stacking on one
        // pitch. Inside a word the lift is held to A2's in-word bound from the
        // note before. A doubled capital stays a doubled letter (the repeat
        // stands). A word head's common tone is not a typed repeat, so a
        // shifted word head is lifted even when the snap landed it home. A
        // lift landing back on `from` takes one further degree, as `derive`'s
        // reflection does; otherwise the capital would sound like its
        // lowercase twin.
        //
        // Only a capital that opens something lifts: the first shifted key
        // after an unshifted one, or a shifted word head (so a shouted line
        // gets one accent per word). Lifting every shifted key is a permanent
        // transposition that jams shouting into the top of the register;
        // interior capitals derive their own line and are struck forte
        // ([`Forte::of`]).
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
        // The mark phrases the line, after the derivation, snap and lift (so
        // it has the last word on the note) and before the run is booked (so
        // the guard counts the sounded stride). Everything here is a function
        // of the text: nothing is gated, skipped or delayed, no clock read.
        //
        // - A doubled mark (`..`, `::`, `??`) steers nothing and voids the
        //   record ([`MARK_DOUBLED`]): an ellipsis trails off on the doubled
        //   glyph's fading re-strike and confirms no cadence.
        // - `-` ties: it sounds the note it came from (mid-word a soft
        //   re-strike, at a word head a step on the common tone).
        // - `)` returns toward its `(`'s degree by at most
        //   [`WORD_LEAP_MAX_DEG`] and spends the record (depth 1), not the
        //   token's steering budget.
        // - A steering mark (at most one per space-delimited token, never a `.`
        //   right behind a digit: `3.14`, `v0.88.0`) lands on its cadence
        //   degree ([`cadence_target`]).
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
        // The line writes its subject from its first intervals (§3.1). A key
        // answering the subject does not rewrite it, and neither does a key
        // with no interval to give: the session's first key (no note before
        // it; `deg - from` would be an untyped unison replayed at every
        // answer).
        //
        // Each latched interval is held to A2's in-word bound, since the
        // subject is answered mid-word and a head's snap or a capital's lift
        // can widen the first interval past a sixth.
        //
        // Two intervals are refused the latch, and the line waits for the
        // next real one:
        // - a zero (a doubled letter), which would replay as a unison at every
        //   answer;
        // - a third identical stride, which would make the subject a ramp
        //   holding [`MELODY_RUN_MAX`]'s whole budget, so an answer after a
        //   rising head would sound the fourth. The subject always has a turn.
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

        // A re-strike is a repeated pitch inside a word: a doubled letter
        // (§3.1 step 1's zero stride). Not a re-strike:
        // - the session's first key (nothing to repeat);
        // - a word head on the previous word's last degree: a common tone
        //   across a chord change, and §3.1 step 6 makes the head prominent;
        // - a steered mark arriving where the line stands: the cadence;
        // - a quotation's open or close. The apostrophe keeps the rule.
        // The repeated pitch still damps the previous voice (`repeat`); only
        // the touch (level and timbre) is the step's.
        let repeat = deg == from && !first;
        let touch = if repeat && !word_head && !steered && quote == QUOTE_NONE {
            self.text.restrike = self.text.restrike.saturating_add(1);
            Touch::ReStrike
        } else {
            self.text.restrike = 0;
            Touch::Step
        };

        // The IOI, updated after `derive` has read it: the contour asks
        // whether this key was early or late against the tempo as it stood,
        // not one that has already absorbed half this gap ([`IOI_EMA_ALPHA`]).
        //
        // A rest is not tempo information: it is held out of the estimate.
        // Folding a rest in (clamped to [`IOI_MAX_MS`]) would drag the tempo
        // slow, make the next keys read as accelerating and push the line up
        // via [`ACCEL_SHARE`], by an amount that depends on typing speed, so
        // the same text at two speeds would stop playing the same degrees.
        //
        // A ≥ [`IOI_RESET_MS`] gap restarts the tempo: after two seconds the
        // hand's tempo is unknown and the default beats a stale average.
        self.ioi_ms = timing::ioi_next(self.ioi_ms, gap, first, rest);

        // The verdict's hot key (sense 3): the first key after a green long
        // command is lit whatever the chord says (full tine, 0 dB,
        // hero-eligible). The pitch is untouched. Spent by that one key.
        let lit = self.lit() || self.take_verdict_lit(at);
        // The step's level: 0 dB lit, −2 dB passing (§9.2). A re-strike is that
        // note again, softer: `L_n = max(0.60 · 0.85^(n−1), 0.35)` re the step
        // it repeats, so the per-second energy arc (§9.6, A14) holds.
        //
        // A shifted key always takes the lit level: the capital's weight
        // ([`super::SHIFT_GLYPH_GAIN`]) must land on every shifted glyph, and
        // an unlit interior capital would otherwise lose most of it to
        // [`PASSING_LEVEL`]. `lit` itself is unchanged (bloom, hero request and
        // roof still read it). A steered mark takes the lit level too: it is
        // the arrival, and its harmony is the one the next Space states
        // (owner, 2026-09-16).
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
        // A cue with no key behind it carries no rank and must not clobber the
        // one the next real letter is measured against.
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
            // The space bass confirms the cadence the mark asked for. With no
            // mark pending the loop advances one chord. With one, it advances
            // to the next chord (strictly forward, cyclically) stating the
            // mark's function ([`mark_chords`]): `. ` and `! ` land on the
            // C4+G4 dyad, `, ` `: ` `? ` on G4, `; ` on A4. The mark must still
            // be the last typed key (`foo.bar baz` arrives with [`MARK_NONE`]).
            // A full stop, bang or question also ends the subject.
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

    /// §10.4's `on Enter`. Ends the line home, parks the loop on
    /// [`CHORD_AFTER_ENTER`] and clears the undo stack: you cannot un-sing
    /// across a line break.
    ///
    /// The walk resolves onto the register's centre, snapped to a tone of the
    /// chord the next line opens against; the contour bias drops and
    /// `prev_rank` clears (a new line's first letter takes the rhythm's
    /// interval). The motif is not re-latched here: the subject outlives the
    /// line and is re-latched only at a real rest (≥ [`PHRASE_PAUSE_MS`], in
    /// [`MelodyV2::on_typed`]), so one subject recurs for as long as the typist
    /// keeps going.
    ///
    /// Returns `(full cadence earned (§11: ≥ 4 keys since the last Enter),
    /// closed, asked)`: `closed` when the last mark was `.` or `!` (the mark was
    /// the cadence, so [`TrailSynth::v2_enter`] drops the pickup), `asked` when
    /// it was `?` ([`CAD_QUESTION_LIFT_DEG`] resolves on the G).
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

    /// §10.4's `on Jump`: a PTY line feed, no key credit.
    ///
    /// Returns `Some(true)` for the head of a cascade (the four-note brrrring),
    /// `Some(false)` for a top-note re-strike inside a live one, and `None` when
    /// the cascade's 60 ms floor swallows it (D18).
    ///
    /// A new cascade needs a gap in the line feeds, not a gap since the last
    /// cascade: twelve Jumps at 60 ms are exactly one 4-note cascade plus
    /// ≤ 11 top-note re-strikes (A26; D18's "the first Jump of a run").
    ///
    /// A lone line feed resolves the derived line where it stands; the second
    /// and later line feeds of a run (within [`CASCADE_RUN_MS`] of the last)
    /// walk [`SONG_THEME`]'s phrase edges (§10.4's "accent step"), so a stream
    /// moves instead of looping one figure at one pitch.
    fn on_jump(&mut self, at: u32) -> Option<bool> {
        let in_run = self
            .cascade_at
            .is_some_and(|c| at.saturating_sub(c) < CASCADE_RUN_MS);
        if in_run {
            // The run walks the theme (§10.4: "then an accent step"): later
            // lines of one program's output take the theme's phrase edges in
            // turn. Only the base moves; the rate law below is unchanged.
            self.text.walk = self.run_theme_step();
        } else {
            // A lone line feed is not a keystroke: nothing to derive from. It
            // resolves the line where it stands (as a rest does) and the
            // cascade is built on the resolved note; the contour bias goes
            // too. It re-heads the run's theme, and the head stands in for
            // the theme's first edge: the cursor moves past edge one without
            // sounding it, so under the 60 ms re-strike floor (which swallows
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
        // letter is a word head (`cadence_degree`).
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

/// How much of the bloom survives at the red end of the arc. High enough that
/// the red end never goes dull (typed-key centroid near 1200 Hz) while the arc
/// still audibly moves. The bloom's level is unchanged (§21.4).
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
    /// The sparkle's call site fires only on a struck step. `fm_turn` is
    /// [`FORTE_FM_TURN_HEAD`] on the row carrying [`WORD_CAPITAL_GAIN`] and
    /// [`FORTE_FM_TURN`] on every other.
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

/// Strikes a built voice harder: forte, a post-build reshaper beside [`wood`].
/// `f` is the fundamental the voice was built on, `force` the hammer force in
/// `(0, 1]`. Never called at force 0.
///
/// 1. Hardness: harmonic phase modulation on the fundamental
///    ([`FORTE_FM_RATIO`]), bright at onset, gone in ~100 ms, 0 dB of peak.
/// 2. A richer body, sum-conserved: only a sounding tine octave (`p[1]` at
///    [`P2_RATIO`], not muted) gains level from the fundamental and rings
///    longer; the wood bar's 3f and the pluck's 4f are left alone. The ratios
///    stay `[1, 2, 2.76]`.
/// 3. Sustain: the voice τ stretches, under [`FORTE_TAU_MAX_S`].
/// 4. The roof opens a little, under [`ROOF_MAX_HZ`].
///
/// Untouched: the mallet (`n_lvl`; [`MALLET_LVL`]'s headroom is spent), the
/// 4 ms attack (§9.5 law 1), every pitch, and `Σ p.lvl`. The decibels a
/// capital carries are [`Forte::gain`]. The rendered peak depends on the
/// modulator's angle to the carrier, which the strike site locks once the
/// voice is spawned ([`TrailSynth::v2_lock_hammer`]).
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

/// The staccato pluck: the mark's voice, reshaping the tine. The octave moves
/// to the double octave ([`PLUCK_P2_RATIO`]) on its own short decay, the 2.76
/// strike is muted, and the felt becomes the fingertip ([`PLUCK_HZ0`] →
/// [`PLUCK_HZ1`]), or for `_ ~ \ |` and `/` the zip: the same noise swept
/// down or up ([`ZIP_HZ_HI`] ↔ [`ZIP_HZ_LO`]) over [`ZIP_GLIDE_S`]. The
/// caller has already put the voice on the pluck's τ ([`PLUCK_TAU_MUL`]).
///
/// As in [`wood`], frequencies and decays always move; levels only on a step.
/// A re-struck mark keeps the re-strike ladder's `0.50 / 0.10 / 0` and
/// [`RESTRIKE_MALLET_LVL`]. Neither the IOI nor the fundamental is touched.
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

/// The bloom as a prototype voice (§3.3 item 1): three partials at
/// [`BLOOM_DEGREES`] above the note's lattice degree `deg`, each with its own
/// decay, fading in over [`BLOOM_ATTACK_S`] from [`BLOOM_DELAY_S`] behind the
/// strike, under a roof [`BLOOM_ROOF_ADD_HZ`] above the note's. No mallet.
///
/// It hangs as long as its note earns: [`BLOOM_DECAY_TAU_MUL`] × `tau_v`
/// under [`BLOOM_DECAY_S`], `dur` by the tail law, and the head scaled the
/// same way under [`BLOOM_HEAD_MIN_MUL`]. `tau_v` is [`tau_v_s`] of the live
/// IOI without the re-strike multiplier (a bloom rides only a
/// [`Touch::Step`]). At the 4 cps reference each term equals its constant.
fn bloom(deg: i32, roof: f32, tau_v: f32) -> Voice {
    let decay = (BLOOM_DECAY_TAU_MUL * tau_v).min(BLOOM_DECAY_S);
    let head = (tau_v / TAU_V_MAX_S).clamp(BLOOM_HEAD_MIN_MUL, 1.0);
    Voice {
        delay: BLOOM_DELAY_S * head,
        // Same ceiling as the decay; the `min` is redundant
        // (`decay <= BLOOM_DECAY_S`) and names the ceiling explicitly.
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
    /// The timbre ladder's seam (§7 step 3; bench and test hook): pull one of
    /// §3.3's stops and the synth renders that rung. Production never calls
    /// this; a fresh synth is [`TimbreStops::ALL`].
    pub fn set_v2_timbre_stops(&mut self, stops: TimbreStops) {
        self.v2.stops = stops;
    }

    /// The melody's clock: the host's stamp where there is one, else the
    /// synth's block clock (§16 row 7).
    ///
    /// The fallback wraps like a host's u32 millisecond stamp (`f64 → u64 →
    /// u32` truncates modulo 2³²) rather than saturating, which after 49.7 days
    /// would pin every gap at zero forever. A wrap costs one key a nonsense
    /// gap (the autorepeat floor takes it to the mallet; it still steps and
    /// speaks) and the next gap is real.
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
    /// announced ([`TING_DUCK`] under a capital, [`TING_DUCK_MARK`] under a
    /// mark). Uses the pan glide (§16 delta 3) with both far-end gains at `to`
    /// × the near-end ones, started now ([`Voice::pan_t0`]), so pan holds and
    /// level is continuous. Draws nothing, reads only the voice's clock, and
    /// arms at most once per ting. A ting already fading (a strike on its pitch
    /// took it over) is left to its damp; a stale address is a no-op.
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
    /// that must land in phase with a sounding partial.
    ///
    /// It draws the same four values `spawn` draws, in the same order, and
    /// discards the first oscillator phase: every later voice reads the same
    /// seeded stream, so consuming one draw fewer would shift everything
    /// behind it (A27). Hence not [`TrailSynth::spawn_seeded`].
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
        #[cfg(test)]
        if self.hammer_unlocked {
            return;
        }
        let Some((slot, born)) = who else { return };
        let v = &mut self.voices[usize::from(slot)];
        if v.on && v.born == born && v.p[0].fm_ratio > 0.0 {
            v.p[0].fm_ph = (v.p[0].fm_ratio * v.p[0].ph + turn).fract();
        }
    }

    /// A decoration never steals from a full pool: `claim_lane`'s rule R1
    /// (§14: a missing decoration is inaudible, a missing TUNE voice is a
    /// silent key), applied by hand to lanes that do not get it from
    /// [`lane_drops_the_newcomer`].
    ///
    /// GRAFT and BREATH lanes never drop a newcomer (a graft is an identity; a
    /// space's exhale is most of the space). But a mark's rise, tink, fifth or
    /// breath decorates the mark's one voice, and `claim`'s pool-level steal
    /// takes the quietest voice, which can be that mark's own short pluck
    /// spawned just before, silencing it. So under a full pool the decoration
    /// is dropped and the mark still sounds.
    ///
    /// Several queued keys may arrive between render calls, and lane caps
    /// exclude fade tails and pre-delayed voices, so admission must also check
    /// the full pool.
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
    ///
    /// It does not run the flood governor (§9.6's IOI arc replaces it), `MIN_GAP`
    /// thinning (v2's rate law is lane caps and IOI-shortened notes),
    /// `advance_song` or `design_trail`'s palette dispatch. It keeps the v1
    /// machinery v2 reuses unchanged: the bed's style / tone latch, the rate
    /// estimate, the erase gate, and the four style-agnostic designers behind
    /// [`TrailSynth::design_trail`] (poof, word poof, swoosh, cloud), which
    /// return before palette dispatch.
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
        // A key at a password prompt is one tone ([`SECRET`]). The host stamps
        // the class on a keyed cue its tty will not echo; a Typed or Space cue
        // carrying it is voiced only by [`TrailSynth::v2_secret`]. It is read
        // as a Typed cue for the bed's kick (so space and letter kick alike)
        // and latches no flow heat: the preamble records that a key was
        // pressed, never which (owner, 2026-09-27).
        let secret =
            matches!(kind, SoundKind::Typed | SoundKind::Space) && meta.glyph_class == SECRET;
        let kind = if secret { SoundKind::Typed } else { kind };
        // The sky's colour (THE PRISM §3.2): the hue's arc position through a
        // one-pole with τ = `BED_HUE_TAU_S`, stepped by the real time since the
        // last event (read before the rate bookkeeping resets it). Seeded on
        // the first v2 event so the pad enters in the ribbon's colour.
        // Bookkeeping only: with the bed off nothing reads it.
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
        // The bed's kick (§3.2 "How it starts"): v1's `bed_kick` behind the
        // same `ev.bed` gate. With `trail_sound_bed` off the bed's energy stays
        // at exact zero and the pad is silent; on (the default) it fades in
        // over the first few keys through `tick_bed`'s 250 ms swell and exhales
        // to exact zero after the last, so idle still parks the device
        // (owner, 2026-09-09).
        if ev.bed {
            self.kick_bed(kind, ev.gain);
        }
        // Other sources (the bonk, the riff) read the rate estimate, so v2
        // feeds it even though its own loudness law is the IOI arc.
        self.rate = self.rate * (-self.since_event / 0.6).exp() + 1.0;
        self.since_event = 0.0;
        let at = self.v2_at_ms(meta);

        // The ting rings through whatever the hand does next ([`TING_LEVEL`]):
        // it is a note of its own, consonant over the capital by construction,
        // and is replaced only by a second Shift (`v2_shift`) or a strike on
        // its own pitch (`v2_typed`, §9.5 law 5). Its address stays live for
        // both.

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
        // that reaches here derives a degree and spawns a voice.
        //
        // The glyph class (§10.4; `trail_sound::glyph_class`) goes in with the
        // key so a phrase mark can steer its own note; below it reshapes the
        // key's tine and adds a decoration. `LETTER` (every letter, every
        // unstamped host and archived render) takes the path the goldens pin.
        let class = meta.glyph_class;
        let plan = self.v2.on_typed(at, meta.rank, sing, ev.shifted, class);
        #[cfg(test)]
        let plan = TypedPlan {
            sotto: plan.sotto && !self.aside_off,
            ..plan
        };
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
        // THE ARC (§3.3 item 3): the hue opens the roof, or does nothing at
        // all with the stop out — `hue_arc(0) == 0`, so `plain` is exact.
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
        // Spelling buys level, never speed (§9.6 is about rate). The strike's
        // gain carries the force table's weight ([`Forte::of`]): v1's
        // [`super::SHIFT_GLYPH_GAIN`] (+2.6 dB) on letters and the bang (by
        // class), [`MARK_SHIFT_GAIN`] on every other shifted mark, and
        // [`WORD_CAPITAL_GAIN`] on top for a shifted letter at a word head on
        // a step. The capital's ring ([`CAP_RING_LEVEL`], a fraction of `gain`)
        // rises with it; the bloom has its own gain and does not. ×1.0 on a
        // plain key (owner, 2026-09-16).
        let phrase_gain = if pause_mark { PAUSE_MARK_GAIN } else { 1.0 };
        let gain =
            ev.gain * KEY_TINE_TRIM * plan.level * g * vel * hammer.gain * phrase_gain * aside_gain;
        // The class reshapes the one tine (§10.4): one key, one TUNE voice. A
        // digit is the wood bar ([`wood`]) on a blip's τ ([`DIGIT_TAU_MUL`]); a
        // plucked mark ([`pluck_class`]) is the staccato pluck ([`pluck`]) on
        // the pluck's τ ([`PLUCK_TAU_MUL`], floored); an opening bracket is the
        // lit tine. A letter is untouched (`tau_k == tau`).
        //
        // Two marks ring longer than a letter: the sentence's full stop
        // ([`CADENCE_STOP_TAU_MUL`]) and the dash, a tie ([`DASH_TAU_MUL`]),
        // both under [`MARK_TAU_MAX_S`] and never shortened by it. The bang's
        // length is forte's.
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
        // A shifted key is struck harder on its own note ([`forte`]); no
        // partial glides. Steps only, never a felt key (the force table's
        // zeros), and never on a plain key, whose voice stays `tine`'s.
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
        // The onset clock records what the mixer did: a key that leaves it
        // unchanged was refused by the TUNE lane (the census's `silent`
        // column).
        if self.v2.lead.is_some() {
            self.v2.last_onset_ms = at;
        }

        // The bloom (§3.3 item 1), behind every lit step with a pitch to bloom
        // from. A passing note stays a plain strike, a doubled letter's
        // tremolo stays a tremolo, a felt key has nothing to bloom. Its level
        // rides the hue's air (item 2) and its pan drifts with the colour
        // while the strike stays on the caret's column. With the hue stop out
        // both reduce to the bloom rung's constants. A pluck does not hang
        // (§10.4); digits and opening brackets keep their bloom.
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

        // The sparkle ([`KEY_GLINT_LEVEL_MUL`]): one glint on every struck
        // key, on the same rung as the bloom (so `plain` is still a tine). It
        // fires on passing notes too, but not on a re-strike or felt key (a
        // new glint on a doubled letter would be a brighter second strike,
        // which §9.2's ladder refuses). It arrives at [`KEY_GLINT_DELAY_S`],
        // off the strike's crest (§9.6).
        //
        // A digit's sparkle counts ([`COUNT_GLINT_DEG0`]): its own stardust
        // degree at ×2 for `0..4` and ×4 for `5..9` ([`COUNT_GLINT_LOW_MUL`]),
        // leaving the rotation cursor alone. It makes the rotation's draws (a
        // pan and the spawn), so later seeded streams are unchanged.
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
                // ×4 on an opening bracket and a bang, ×2 on a quote
                // ([`QUOTE_GLINT_MUL`]), by class; ×2 on a capital letter
                // ([`FORTE_GLINT_MUL`]); ×1 on everything else. A bang or an
                // opening quote winks a second time at its own distance
                // ([`BANG_GLINT2_DELAY_S`], [`QUOTE_GLINT2_DELAY_S`]),
                // advancing the rotation twice. A closing quote
                // ([`QUOTE_CLOSES`]) is plain: ×1, one wink.
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

        // The grafts (§10.4): one gesture per bucket in [`LANE_GRAFT`]
        // (fade-steal, cap 2), on the key's own pan and gain, behind the bloom
        // stop, on a step only: a re-struck `??` or `((` does not ask or open
        // twice. The exception is a close right after its open (`()` shares a
        // rank). The stop's breath is not a pitched graft and not behind the
        // stop: it is the space's exhale. All go through
        // [`TrailSynth::v2_spawn_decoration`], so under a full pool the
        // decoration is dropped, never the mark's pluck.
        let closes_its_open = class == CLOSE && self.v2.prev_class == OPEN;
        if stops.bloom && (plan.touch == Touch::Step || closes_its_open) && classed {
            match class {
                // `?` rises: a fifth above, 60 ms on, −3 dB, struck. From a D it
                // rises a fourth, to G ([`QUEST_RISE_D_DEG`]): D–A is this
                // lattice's wolf. The sounding degree (`deg`, in the song's
                // key) is what is asked, since the wolf is between lattice
                // classes.
                //
                // `+ = * % ^ < >` sound their fifth: the rise's voice swelling
                // on the bloom's attack from 30 ms, −6 dB. So does the steered
                // (announcing) colon: it stands on G, and G's fifth D opens
                // under it. An inner colon (`a::b`, `12:30`) does not.
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

        // Flow's echo and the capital's ring (§22 lever 1; [`CAP_RING_LEVEL`]).
        // The echo is the key's octave, given to every lit step in flow at
        // [`FLOW_ECHO_DELAY_S`] behind its strike and [`FLOW_ECHO_LEVEL`]
        // under it. The ring is a twelfth ([`CAP_RING_LIFT_DEG`]) behind a
        // capital letter that opens a word or shifted run ([`Forte::ring`]),
        // in or out of flow: [`CAP_RING_DELAY_S`] behind the strike,
        // [`CAP_RING_ATTACK_S`] of swell, −6 dB. One voice per key, never both.
        //
        // It rides this key: same pan, seeded velocity, loudness arc and
        // lattice. No mallet (`n_lvl = 0`): a second felt hit would be a second
        // onset.
        //
        // Not under a timbre stop: flow heat is the echo's stop and
        // `ev.shifted` the ring's. At `flow == 0.0` on an unshifted key the
        // branch is not taken, no slot is claimed and no draw is made, so the
        // cold box is byte-identical (the music-box goldens prove it).
        //
        // The echo goes in [`LANE_BLOOM`], which drops a newcomer rather than
        // stealing, so at a flowing 12 cps the lane thins the decoration, never
        // the tune. The ring goes in [`LANE_GRAFT`]: under a fast shout the
        // previous key's bloom and echo are under the 40 ms age guard, and
        // BLOOM would drop the ring, an identity. GRAFT fade-steals its oldest
        // instead. If a batched burst fills the whole pool, the ring yields
        // rather than stealing the strike it decorates.
        //
        // - Every other shifted step makes the ring's four draws and discards
        //   them, so each seeded stream behind a shifted key is unchanged
        //   (under a full pool nothing is drawn, as the decoration guard does).
        // - A shifted key takes no flow echo: it would be a second voice on a
        //   key whose ring was refused.
        // - The ring shares no partial with the strike's tine, so it keeps its
        //   drawn phase, through `spawn`'s own four draws.
        // - Its 2.76f strike partial is muted: a resonance is not struck.
        let ring_gain = if ev.shifted {
            gain * CAP_RING_LEVEL.max(FLOW_ECHO_LEVEL * flow)
        } else {
            gain * FLOW_ECHO_LEVEL * flow
        };
        let struck = plan.touch == Touch::Step && !plan.mallet_only;
        // The ring yields to a live ting on its own pitch (§9.5 law 5). The
        // ring sits in the ting's register and both are chord tones, so a
        // ting on D7 and a capital on G5 would put two D7 sines on unrelated
        // phases. The ting already is the resonance, so the ring is not
        // spawned; its four draws are made and discarded like every refused
        // ring's.
        let ring_deg = deg + FLOW_ECHO_OCTAVE_DEG + CAP_RING_LIFT_DEG;
        #[cfg(test)]
        let ring_deg = if self.ring_at_octave {
            deg + FLOW_ECHO_OCTAVE_DEG
        } else {
            ring_deg
        };
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
            // The echo swells rather than strikes. With the tine's 4 ms attack
            // at [`FLOW_ECHO_DELAY_S`] it would crest at the millisecond the
            // bloom does (at and under 4 cps), two decorations of one key
            // cresting together and raising the prose peak in flow. On the
            // bloom's attack it opens into the note's decay instead.
            //
            // The echo's measured worth is small (see [`FLOW_ECHO_PHASE`]);
            // most of flow's ring-out comes from the bass decay. Note that
            // suppressing a spawn also skips [`TrailSynth::spawn`]'s four draws
            // and re-rolls every voice behind it, which confounds A/B
            // comparisons of this voice.
            //
            // The capital ring opens 60 ms behind the key with a 30 ms swell
            // ([`CAP_RING_DELAY_S`]).
            //
            // Order matters: this attack moves the peak only once the sparkle
            // has been moved off the strike's crest ([`KEY_GLINT_DELAY_S`]);
            // before that, the glint owned the word's maximum.
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
                #[cfg(test)]
                let echo_gain = echo_gain * self.echo_trim;
                // The echo lands at a known phase on the octave it reinforces
                // ([`FLOW_ECHO_PHASE`]). Its fundamental is exactly the
                // strike's [`P2_RATIO`] partial, and two sines at one frequency
                // interfere at whatever relative phase they get. Handing it the
                // strike's P2 phase, advanced by the echo's delay and offset by
                // [`FLOW_ECHO_PHASE`], makes the pair sum in power on every key.
                // `p2.f0` rather than a re-derived `2 × f`, so the two cannot
                // drift apart by rounding.
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

    /// The two-tap air cloud (§3.3 item 4): two bloom taps on degree `deg`,
    /// `after` seconds past the key, at [`AIR_TAP_DELAY_S`] and [`AIR_TAP_LEVEL`]
    /// re the bloom, panned to opposite sides of `pan`. Unequal spacings read
    /// as a room; it is the theme's only reverb, spent only where a line ends.
    ///
    /// The taps are built at [`TAU_V_MAX_S`], not the live τ_v: the hang
    /// scaling exists for per-key blooms that stack at speed, and a shortened
    /// room is a click. Under a shifted key their roof takes the same clamp as
    /// the halo ([`shifted_roof`]); a plain key's room is unchanged.
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

    /// Shift: the ting ([`TING_ATTACK_S`] has the design). One struck bell in
    /// [`LANE_TING`] at `ting_fold(penta(TING_TONES[chord.root][TING_PICK[k]] +
    /// key))`, a tone of the chord the Space bar is playing, two to three
    /// octaves over the dyad: a pure sine under its octave, the tine's 30 ms
    /// clink and a quiet felt mallet, ringing [`TING_DUR_S`]. No FM
    /// (owner, 2026-09-16).
    ///
    /// No melody step, beat or playhead move: `on_typed` is not called and
    /// `push_v2` leaves `since_voice` alone, so a modifier is intent, not
    /// authorship. A second Shift replaces the first. The cursor is v1's
    /// `TrailSynth::shift_step`, advanced as `design_shift` advances it (one
    /// cursor for both engines), indexing [`TING_PICK`] (same length, asserted
    /// below). rng: `v2_pan` 1 + spawn 4.
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

    /// Secret: a key typed at a password prompt (the host stamps [`SECRET`] on a
    /// keyed cue the tty will not echo). One fixed plain tine: [`SECRET_DEG`] in
    /// the song's key, a [`Touch::Step`] on [`SECRET_TAU_S`] under
    /// [`ROOF_PLAIN_LO_HZ`] at cold flow, gain `ev.gain ×` [`KEY_TINE_TRIM`],
    /// centre pan. No forte, bloom, echo, ring, glint, breath, velocity draw or
    /// pan jitter: nothing a letter, capital, digit, mark or space could vary
    /// (owner, 2026-09-27).
    ///
    /// It moves no melody: `on_typed` is not called and no [`MelodyV2`] field is
    /// written, so the line after the prompt is the line that would have played
    /// without the secret. Every secret key is one pitch, so a same-pitch damp
    /// (§9.5 law 5) makes a fast password replace its last tone rather than
    /// comb. rng: spawn's 4 draws.
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
        // §9.5 law 5: the nav tick is always a same-pitch re-strike, since
        // navigation never advances the verse (§12.3). Two independently phased
        // ticks at one frequency comb, randomly swinging a quiet tick by
        // several dB and making hops inaudible; damping first makes a re-struck
        // tick replace its predecessor, as `v2_typed`'s `plan.repeat` does.
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
            // Half a column out (§11): the bell is FAR, and far is quiet and
            // slightly off-axis, never loud and centred.
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
    /// lattice.
    ///
    /// Green is a plagal cadence: the IV dyad (F4 + C4) falling to the I dyad
    /// (C4 + G4) [`VERDICT_PLAGAL_S`] later, with one E5 tine over the
    /// resolution (the note both chords own). A long one adds the Enter
    /// cadence's faraway ice bell ([`CAD_BELL_HZ`], C6).
    ///
    /// Red is the vi alone (A4 + E4), −9 dB, no tine and no bell, and the chord
    /// loop is parked so the next word sings against it.
    ///
    /// No new pitch: every frequency is a [`CHORD_LOOP`] entry through
    /// [`CHORD_ROOT_RATIO`] or a [`penta`] degree of [`TINE_BASE_HZ`], and the
    /// playhead does not move ([`MelodyV2::note_verdict`]).
    ///
    /// Loudness: the loudest voice is [`VERDICT_DYAD_LEVEL`] (−6 dB re a step);
    /// every other voice is under it. Once per command (the host's
    /// arm-and-spend gate); no `g_ioi`, since a verdict is not a keystroke.
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

    /// The tink ([`TINK_DEG`]), the bracket's grace note: a P1-only sine on
    /// lattice degree `deg` (reflected and keyed by the caller), [`TINK_DELAY_S`]
    /// after the key on the tine's 4 ms attack and a [`TINK_TAU_S`] decay, under
    /// the key's roof, at [`TINK_LEVEL`] of the key's gain on the key's pan (no
    /// draw of its own), in [`LANE_GRAFT`]. Returns the address for
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
            // time-stamps a transient, so the gesture must OPEN with one.
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

    /// The rainbow sky (THE PRISM §3.2). The `trail_sound_bed` setting ships on;
    /// with it off, `push_v2` feeds this nothing and it is never reached
    /// (`bed_sample`'s level floor) (owner, 2026-09-09).
    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, _u1: f32, _u2: f32) -> (f32, f32) {
        s.bed_rainbow_sky(dt, lvl)
    }

    fn anchor_hz(&self) -> f32 {
        TINE_BASE_HZ
    }
}

// The rainbow sky: the body.

impl TrailSynth {
    /// One stereo sample `(mid, side)` of the sky pad (THE PRISM §3.2), the body
    /// behind the music box's palette bed. The caller has floored `bed.level`
    /// and folded level × gain into `lvl`; the result lands in the ducked mix.
    ///
    /// Seed-at-target, one-pole portamento on the oscillator frequencies and a
    /// weighted sum, where the bar is the live chord (`v2.chord`, advanced once
    /// per word by `on_space`), each tone carries [`BED_PARTIALS`] for the hue to
    /// tilt, and the hue's arc (`bed.hue_s`) drives the tilt and the top tone's
    /// twin detune, never a pitch, so the pad stays in key. Sample-driven, no
    /// rng: a render is bit-replayable from (events, seed).
    pub(super) fn bed_rainbow_sky(&mut self, dt: f32, lvl: f32) -> (f32, f32) {
        let degs = sky_bed_degrees(usize::from(self.v2.text.chord));
        let mut tgt = [0.0f32; 3];
        for (t, d) in tgt.iter_mut().zip(degs) {
            *t = penta(BED_BASE_HZ, d);
        }
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
        // THE BREATH: a raised cosine on the whole pad, on its own phase
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
    use crate::trail_sound::tests::par_map;
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

    // -- The rainbow sky (THE PRISM §3.2) ------------------------------------

    /// The pad is the harmony the melody snaps to: for every chord of the loop
    /// the sky voices that chord's three lit verse degrees, ascending, on the
    /// untransposed lattice, every tone under the bass register
    /// ([`BASS_BASE_HZ`]). Partials are normalised to a sine's RMS.
    #[test]
    fn the_sky_pad_is_the_live_chords_lit_degrees_under_the_bass() {
        assert!((BED_BASE_HZ - TINE_BASE_HZ / 4.0).abs() < 1e-6);
        for (chord, c) in CHORD_LOOP.iter().enumerate() {
            let degs = sky_bed_degrees(chord);
            let lit = c.lit;
            assert!(
                degs.windows(2).all(|w| w[1] > w[0]),
                "chord {chord}: ascending, distinct ({degs:?})"
            );
            for d in degs {
                assert!((0..5).contains(&d), "chord {chord}: a verse degree ({d})");
                assert!(lit & (1 << d) != 0, "chord {chord}: degree {d} is lit");
                let hz = penta(BED_BASE_HZ, d);
                assert!(
                    hz < BASS_BASE_HZ,
                    "chord {chord}: pad tone {hz} Hz under the bass register"
                );
            }
            assert_eq!(
                lit.count_ones(),
                3,
                "chord {chord}: the loop lights exactly three degrees"
            );
        }
        for n in BED_PARTIALS {
            assert!(
                matches!(n, 1 | 2 | 4 | 8 | 3 | 6 | 5),
                "partial {n} is an octave, a fifth or a major third of its tone"
            );
        }
        let rms: f32 = BED_PARTIALS
            .iter()
            .map(|&n| 1.0 / (n * n) as f32)
            .sum::<f32>()
            .sqrt();
        assert!(
            (BED_PARTIAL_NORM - 1.0 / rms).abs() < 1e-3,
            "the partial norm is 1/√Σ1/n² = {}",
            1.0 / rms
        );
    }

    /// The hue drives tilt and width, never pitch: the pad at the red and cyan
    /// ends keeps its oscillator frequencies bit for bit and is brighter (more
    /// energy in its first difference) at the cyan end. The arc is slewed per
    /// v2 event through the one-pole: seeded on the first key, glided after.
    #[test]
    fn the_sky_follows_the_hue_in_tilt_and_width_and_never_in_pitch() {
        let render = |arc: f32| -> (Vec<f32>, [f32; 4]) {
            let mut s = synth();
            s.bed.hue_s = arc;
            let dt = 1.0 / SR;
            let out: Vec<f32> = (0..48_000).map(|_| s.bed_rainbow_sky(dt, 0.4).0).collect();
            (out, s.bed.var_f)
        };
        let (red, f_red) = render(0.0);
        let (cyan, f_cyan) = render(1.0);
        // Pitch: the three tones' frequencies are the chord's, whatever the hue.
        assert!(
            f_red
                .iter()
                .zip(&f_cyan)
                .take(3)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "the hue moved a pitch: {f_red:?} vs {f_cyan:?}"
        );
        // …and they are the chord the synth actually stands on (a fresh
        // session parks on `CHORD_AFTER_ENTER`, not on I).
        let chord = usize::from(synth().v2.chord());
        for (i, d) in sky_bed_degrees(chord).into_iter().enumerate() {
            assert!(
                (f_red[i] - penta(BED_BASE_HZ, d)).abs() < 1e-3,
                "tone {i}: {} vs degree {d} of chord {chord}",
                f_red[i]
            );
        }
        let brightness = |x: &[f32]| {
            let e: f64 = x.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
            let d: f64 = x
                .windows(2)
                .map(|w| f64::from(w[1] - w[0]) * f64::from(w[1] - w[0]))
                .sum();
            d / e.max(1e-18)
        };
        let (b_red, b_cyan) = (brightness(&red), brightness(&cyan));
        assert!(
            b_cyan > b_red * 1.3,
            "the cyan end is the open end: brightness red {b_red:.3e} vs cyan {b_cyan:.3e}"
        );
        assert!(
            red.iter().any(|&v| v != 0.0) && cyan.iter().any(|&v| v != 0.0),
            "the pad sounds at both ends"
        );

        // The slew: seeded on the first v2 key, then a one-pole in real time.
        let mut s = synth();
        let key = |hue: f32| SoundEvent {
            hue,
            ..event(SoundKind::Typed, 0.0, false)
        };
        s.push(key(0.25)); // arc = tri(0.5) = 0.5
        assert!(
            (s.bed.hue_s - 0.5).abs() < 1e-6,
            "seeded at the first key's arc"
        );
        let mut buf = [0.0f32; 512];
        while s.since_event < 1.0 {
            s.render(&mut buf);
        }
        let gap = s.since_event;
        s.push(key(0.0)); // arc 0
        let want = 0.5 * (-gap / BED_HUE_TAU_S).exp();
        assert!(
            (s.bed.hue_s - want).abs() < 1e-4,
            "one-pole toward the new arc over {gap:.3} s: {} vs {want}",
            s.bed.hue_s
        );
    }

    /// The sky is fed by the music box's own keys behind the knob: with
    /// `bed: false` the bed's energy stays at exact zero; with `bed: true` a
    /// keystroke kicks it by v1's number, it sounds under the notes, and after
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
        // v1's table, shared: a key is 0.3, a jump 0.5, a nav tick 0.12.
        let mut k = synth();
        k.push(SoundEvent {
            bed: true,
            ..event(SoundKind::Typed, 0.0, false)
        });
        assert!(
            (k.bed.energy - 0.3).abs() < 1e-6,
            "a key kicks the bed by v1's 0.3"
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

    /// The verdict's sound:
    ///
    /// 1. The music box's alone: the other nine voices are silent for a
    ///    verdict (a plagal cadence needs this instrument's chord loop), and
    ///    their path has no new arithmetic.
    /// 2. Green and red both speak, and green resolves: the plagal's two dyads
    ///    land as separate onsets, still ringing past [`VERDICT_PLAGAL_S`].
    /// 3. ≤ −6 dB re a step, as a delivered peak ([`VERDICT_DYAD_LEVEL`] as an
    ///    outcome).
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

        // 1 — every other instrument, and every other look, is silent.
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
        let mut other_look = synth();
        other_look.push(verdict(SoundVoice::Style, GlowStyle::Lumen, false, true));
        assert_eq!(
            render_peak(&mut other_look, 64),
            0.0,
            "…and neither does another look through `Style`"
        );

        // 2 — the music box speaks for both verdicts, and the green one is
        // still resolving after the plagal's fall.
        let mut green = synth();
        green.push(verdict(
            SoundVoice::RainbowKittyV2,
            GlowStyle::RainbowKitty,
            false,
            true,
        ));
        let g = render_mono(&mut green, 64);
        let fall = (VERDICT_PLAGAL_S * SR) as usize;
        assert!(
            g[..fall].iter().any(|x| x.abs() > 1e-4),
            "the subdominant sounds first"
        );
        assert!(
            g[fall..].iter().any(|x| x.abs() > 1e-4),
            "…and home lands {VERDICT_PLAGAL_S} s later — it is a CADENCE, \
             not a chord"
        );

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
    /// paste gives exactly four tines on the cascade lane at 0 / 22 / 44 / 66 ms
    /// on the live chord's lit degrees plus the root's octave; `walk`, `steps`
    /// and `word_pos` are unchanged and the next key steps from where the word
    /// left off. The other nine voices render a strum as silence.
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
        let delays: Vec<f32> = strum.iter().map(|v| v.delay).collect();
        assert_eq!(delays, STRUM_DELAYS_S.to_vec(), "0 / 22 / 44 / 66 ms");
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

        // The music box's alone: every other instrument and every other look
        // renders a strum as exact silence.
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
        for style in [
            GlowStyle::Lumen,
            GlowStyle::Phaser,
            GlowStyle::Sparkle,
            GlowStyle::Fire,
            GlowStyle::Laser,
            GlowStyle::Beam,
            GlowStyle::Water,
            GlowStyle::Comet,
            GlowStyle::Classic,
        ] {
            let mut other = synth();
            other.push(SoundEvent {
                style,
                ..event(SoundKind::Strum, 0.0, false)
            });
            assert_eq!(render_peak(&mut other, 64), 0.0, "{style:?} has no strum");
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

    /// `(cos, sin)` of `TAU · m / n` for every `m` in `0..n`: the twiddles of
    /// an `n`-point DFT. Bin `k`'s `i`-th twiddle is entry `(k · i) mod n`,
    /// the same angle modulo a turn as `TAU · k / n · i`, so a naive DFT reads
    /// `n` trig pairs instead of computing `n² / 2` of them.
    fn dft_twiddles(n: usize) -> Vec<(f64, f64)> {
        (0..n)
            .map(|m| {
                let a = core::f64::consts::TAU * m as f64 / n as f64;
                (a.cos(), a.sin())
            })
            .collect()
    }

    /// Bin `k`'s `(re, im)` of the naive DFT of `x` (already windowed),
    /// through [`dft_twiddles`]'s table for `x.len()`. Counted loops, so an
    /// unoptimized build pays no iterator adaptors per term.
    fn dft_bin(x: &[f64], tw: &[(f64, f64)], k: usize) -> (f64, f64) {
        let n = x.len();
        debug_assert_eq!(tw.len(), n);
        let (mut re, mut im) = (0.0f64, 0.0f64);
        let (mut i, mut m) = (0usize, 0usize);
        while i < n {
            let (c, s) = tw[m];
            re += x[i] * c;
            im -= x[i] * s;
            i += 1;
            m += k;
            if m >= n {
                m -= n;
            }
        }
        (re, im)
    }

    /// Spectral centroid of a Hann-windowed slice by naive DFT: small and slow,
    /// with no library version to drift under the pinned design property.
    fn centroid_hz(x: &[f32]) -> f32 {
        let n = x.len();
        let win: Vec<f32> = (0..n)
            .map(|i| 0.5 * (1.0 - (core::f32::consts::TAU * i as f32 / n as f32).cos()))
            .collect();
        let xs: Vec<f64> = x
            .iter()
            .zip(&win)
            .map(|(s, h)| f64::from(*s * *h))
            .collect();
        let tw = dft_twiddles(n);
        let mut num = 0.0f64;
        let mut den = 0.0f64;
        for k in 1..n / 2 {
            let (re, im) = dft_bin(&xs, &tw, k);
            let mag = (re * re + im * im).sqrt();
            let f = k as f64 * f64::from(SR) / n as f64;
            num += f * mag;
            den += mag;
        }
        if den <= 0.0 { 0.0 } else { (num / den) as f32 }
    }

    // -- A28: the ladder floor ------------------------------------------

    /// The isolated step lands on the ladder floor: −21.0 dBFS at the host
    /// default volume, heat 0.5 (§9.1, A28). The pin [`KEY_TINE_TRIM`] is fitted
    /// against, on peak (§9.6). Measured on the nominal step: the peak is linear
    /// in gain below the limiter, so scaling by `nominal / seeded` divides out
    /// §9.6's seeded velocity and pan jitter exactly.
    #[test]
    fn the_isolated_step_lands_on_the_ladder_floor() {
        let mut s = synth();
        let mark = s.born_seq;
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let v = tune_voices(&since(&s, mark))[0];
        // The ladder's own per-channel gain at pan 0, before the draw: the
        // step is lit (C5 under the parked IV), level 1.0, g_IOI 1.0.
        let nominal = VOL * KEY_TINE_TRIM * core::f32::consts::FRAC_1_SQRT_2;
        let seeded = v.gl.max(v.gr);
        let draw_db = 20.0 * (seeded / nominal).log10();
        assert!(
            draw_db.abs() <= VEL_DB_STEP + 0.25,
            "this seed's velocity + pan-jitter draw is {draw_db:+.2} dB, outside §9.6's \
             ±{VEL_DB_STEP} dB (plus the ±0.03 pan's ≤ 0.2 dB channel tilt)"
        );
        let peak = render_peak(&mut s, 40) * nominal / seeded;
        let db = 20.0 * peak.log10();
        assert!(
            (db + 21.0).abs() <= 0.3,
            "the isolated v2 step's NOMINAL peak is {db:.2} dBFS (this seed's draw \
             {draw_db:+.2} dB), off the -21.0 floor; re-fit KEY_TINE_TRIM to {:.4}",
            KEY_TINE_TRIM * 10f32.powf((-21.0 - db) / 20.0)
        );
    }

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

    /// The pathological stream never becomes a siren (§3.1 step 5): a held key,
    /// a digit run, `!!!!`, a base64 blob and one word eight times stay inside
    /// the TUNE register, repeat no stride more than [`MELODY_RUN_MAX`] times,
    /// and do not sit on one degree.
    #[test]
    fn a_pathological_stream_never_becomes_a_siren() {
        const PATHOLOGICAL: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1234567890 !!!! \
             aGVsbG8gd29ybGQgdGhpcyBpcyBub3QgcHJvc2U ffff the the the the the the the the";
        let degs = type_degrees_rhythm(PATHOLOGICAL, 1_000, &[83, 91, 83, 77, 83]);
        assert!(
            degs.len() > 100,
            "fixture too short to say anything ({} keys)",
            degs.len()
        );
        for d in &degs {
            assert!(
                (TUNE_DEG_LO..=TUNE_DEG_HI).contains(&i32::from(*d)),
                "the line left the register at degree {d}"
            );
        }
        let mut run = 1usize;
        let mut worst = 1usize;
        for w in degs.windows(3) {
            if w[1] - w[0] == w[2] - w[1] && w[1] != w[0] {
                run += 1;
            } else {
                run = 1;
            }
            worst = worst.max(run);
        }
        assert!(
            worst <= usize::from(MELODY_RUN_MAX),
            "{worst} identical strides ran without turning — that is a siren, \
             and MELODY_RUN_MAX is meant to invert the fourth"
        );
        let mut hist = [0usize; (TUNE_DEG_HI + 1) as usize];
        for d in &degs {
            hist[*d as usize] += 1;
        }
        let top = hist.iter().max().copied().unwrap_or(0);
        assert!(
            top * 2 < degs.len(),
            "{top} of {} keys landed on ONE degree — the line is stuck",
            degs.len()
        );
        assert!(
            hist.iter().filter(|n| **n > 0).count() >= 5,
            "the line only ever visited {} of the register's nine degrees",
            hist.iter().filter(|n| **n > 0).count()
        );
    }

    /// The run guard holds on the sounded line, not on `derive`'s stride: the
    /// bench's `scenario_pathological` stream for stream, plus prose where a
    /// rising subject's answer could sound a five-note scale. Counted on the
    /// sounded degrees, as the bench counts them.
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

    /// Every repeated pitch is charged to a cause the design names. A corpus
    /// that types `ll` asks for the same note twice, so the claim is an account,
    /// not "100 % distinct". Each key repeating the sounding degree is charged
    /// to:
    ///
    /// * the text (this key's alphabet rank equals the last one's);
    /// * a word head whose chord snap held a common tone;
    /// * the line answering its subject on a latched unison;
    /// * the mark phrasing it (`phr`): a dash ties, a steered mark may arrive on
    ///   the degree the line stands on, a closing bracket may already be home.
    ///
    /// The residual must be zero: a repeat with no cause is the engine standing
    /// still while the hand moved.
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

    /// `keyboard_song_ab`'s prose corpus: §21.4's `prose-10cps` is this text at
    /// 10 cps, and A2 is pinned on it so the pin sees every wrap, line feed and
    /// sentence rest the bench sees.
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

    /// The bench's `scenario_prose`: 60 s of the corpus at 10 cps, looped,
    /// a 1.4 s think between paragraphs.
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

    /// The loudness harness (§9.6, and flow's guard): drives the bench's prose
    /// corpus at `cps` with flow heat pinned at `flow`, renders the take and
    /// returns `(rms dBFS, peak dBFS)`.
    ///
    /// The window is a fixed wall clock at every rate, since §9.6 bounds energy
    /// per second. Stamped, unlike [`drive_bench_prose`] (`at_ms` from press
    /// time, `rank` from the glyph), because loudness depends on which notes
    /// the derivation chose.
    fn prose_loudness(cps: f32, flow: f32) -> (f32, f32) {
        prose_loudness_with_key_headroom(cps, flow, true)
    }

    /// Counterfactual mode removes only the key headroom after each real spawn.
    /// Voices, timing, phases, RNG draws, lane occupancy and the bed's input
    /// stay on the shipping path; the over-peak must return.
    fn prose_loudness_with_key_headroom(cps: f32, flow: f32, headroom: bool) -> (f32, f32) {
        prose_loudness_seeded(cps, flow, headroom, 0x504F_4F46)
    }

    /// [`prose_loudness_with_key_headroom`] on a named seed.
    fn prose_loudness_seeded(cps: f32, flow: f32, headroom: bool, seed: u32) -> (f32, f32) {
        const BLOCK: usize = 512;
        const TAKE_S: f32 = 30.0;
        // (press time s, gesture, pan, shifted, glyph class, rank)
        let mut cues: Vec<(f32, SoundKind, f32, bool, u8, u8)> = Vec::new();
        let mut t = 0.5f32;
        let dt = 1.0 / cps;
        let mut col = 0.0f32;
        while t < TAKE_S {
            for ch in BENCH_PROSE.chars() {
                let pan = (col / 68.0).clamp(0.0, 1.0) * 1.8 - 0.9;
                let kind = match ch {
                    ' ' => SoundKind::Space,
                    '\n' => SoundKind::Jump,
                    _ => SoundKind::Typed,
                };
                let (class, rank) = if kind == SoundKind::Typed {
                    (
                        crate::trail_sound::typed_glyph_class(Some(ch)),
                        crate::trail_sound::typed_glyph_rank(Some(ch)),
                    )
                } else {
                    (0, 0)
                };
                cues.push((t, kind, pan, bench_needs_shift(ch), class, rank));
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
            // The bench's think between paragraphs.
            t += 1.4;
        }
        cues.retain(|c| c.0 < TAKE_S);
        let mut s = TrailSynth::new(SR, seed);
        let frames = (TAKE_S * SR) as usize;
        let mut stereo = vec![0.0f32; BLOCK * 2];
        let (mut f, mut ci) = (0usize, 0usize);
        let (mut sq, mut n, mut peak) = (0.0f64, 0usize, 0.0f32);
        while f < frames {
            let take = BLOCK.min(frames - f);
            let now = f as f32 / SR;
            while ci < cues.len() && cues[ci].0 <= now {
                let (ct, kind, pan, shifted, glyph_class, rank) = cues[ci];
                let mut ev = event(kind, pan, shifted);
                ev.heat = 0.55;
                ev.hue = (ct * 0.18).fract();
                ev.voice = SoundVoice::RainbowKittyV2;
                let born_before = s.born_seq;
                s.push_meta(
                    ev,
                    EventMeta {
                        at_ms: (ct * 1000.0) as u32,
                        glyph_class,
                        rank,
                        flow,
                        ..EventMeta::default()
                    },
                );
                if !headroom && kind == SoundKind::Typed {
                    let allowance = flow_key_headroom(flow);
                    for v in s.voices.iter_mut().filter(|v| v.on && v.born > born_before) {
                        v.gl /= allowance;
                        v.gr /= allowance;
                        v.gl1 /= allowance;
                        v.gr1 /= allowance;
                    }
                }
                ci += 1;
            }
            s.render(&mut stereo[..take * 2]);
            for x in &stereo[..take * 2] {
                sq += f64::from(*x) * f64::from(*x);
                peak = peak.max(x.abs());
            }
            n += take * 2;
            f += take;
        }
        let rms = (sq / n as f64).sqrt() as f32;
        (20.0 * rms.log10(), 20.0 * peak.log10())
    }

    /// The lane-isolated loudness harness: [`prose_loudness`]'s script and
    /// clock, with every voice outside `lane` silenced at its panned gains
    /// before each block, returning that layer's energy per second.
    ///
    /// Silencing at `gl`/`gr` rather than `on` keeps voices live, holding lane
    /// slots and damp state, so the layer sees the full mix's spawn population
    /// (§14's caps, `v2_damp_same_pitch`).
    fn lane_loudness(cps: f32, flow: f32, lane: u8) -> f32 {
        const BLOCK: usize = 512;
        const TAKE_S: f32 = 30.0;
        let mut cues: Vec<(f32, SoundKind, f32, bool, u8)> = Vec::new();
        let mut t = 0.5f32;
        let dt = 1.0 / cps;
        let mut col = 0.0f32;
        while t < TAKE_S {
            for ch in BENCH_PROSE.chars() {
                let pan = (col / 68.0).clamp(0.0, 1.0) * 1.8 - 0.9;
                let kind = match ch {
                    ' ' => SoundKind::Space,
                    '\n' => SoundKind::Jump,
                    _ => SoundKind::Typed,
                };
                let rank = if kind == SoundKind::Typed {
                    crate::trail_sound::typed_glyph_rank(Some(ch))
                } else {
                    0
                };
                cues.push((t, kind, pan, bench_needs_shift(ch), rank));
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
            t += 1.4;
        }
        cues.retain(|c| c.0 < TAKE_S);
        let mut s = TrailSynth::new(SR, 0x504F_4F46);
        let frames = (TAKE_S * SR) as usize;
        let mut stereo = vec![0.0f32; BLOCK * 2];
        let (mut f, mut ci) = (0usize, 0usize);
        let (mut sq, mut n) = (0.0f64, 0usize);
        while f < frames {
            let take = BLOCK.min(frames - f);
            let now = f as f32 / SR;
            while ci < cues.len() && cues[ci].0 <= now {
                let (ct, kind, pan, shifted, rank) = cues[ci];
                let mut ev = event(kind, pan, shifted);
                ev.heat = 0.55;
                ev.hue = (ct * 0.18).fract();
                ev.voice = SoundVoice::RainbowKittyV2;
                s.push_meta(
                    ev,
                    EventMeta {
                        at_ms: (ct * 1000.0) as u32,
                        rank,
                        flow,
                        ..EventMeta::default()
                    },
                );
                ci += 1;
            }
            s.mute_all_lanes_but(lane);
            s.render(&mut stereo[..take * 2]);
            for x in &stereo[..take * 2] {
                sq += f64::from(*x) * f64::from(*x);
            }
            n += take * 2;
            f += take;
        }
        let rms = (sq / n as f64).sqrt() as f32;
        20.0 * rms.log10()
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

    /// The ring-out probe (§22's "opens"): `t`, `h`, `e` at 10 cps and a
    /// Space, at flow heat `flow`, rendered for 1.5 s. Returns `(peak dBFS,
    /// body dBFS)`, the body being the RMS from 400 to 1200 ms, after every
    /// onset: the box ringing. Louder moves the peak; fuller moves the body.
    fn word_ring_out(flow: f32) -> (f32, f32) {
        const HEAD_S: f32 = 0.400;
        const TAIL_S: f32 = 1.200;
        let mut s = synth();
        let mut buf = [0.0f32; 960];
        let mut out: Vec<f32> = Vec::new();
        let script = [(1_000u32, 't'), (1_100, 'h'), (1_200, 'e'), (1_300, ' ')];
        let mut k = 0usize;
        // 10 ms per block at 48 kHz, 150 blocks = 1.5 s.
        for b in 0..150u32 {
            let now = 1_000 + b * 10;
            while k < script.len() && script[k].0 <= now {
                let (at, ch) = script[k];
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                s.push_meta(
                    event(kind, 0.0, false),
                    EventMeta {
                        at_ms: at,
                        rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                        flow,
                        ..EventMeta::default()
                    },
                );
                k += 1;
            }
            s.render(&mut buf);
            out.extend_from_slice(&buf);
        }
        let peak = out.iter().fold(0.0f32, |a, x| a.max(x.abs()));
        let lo = (HEAD_S * SR) as usize * 2;
        let hi = (TAIL_S * SR) as usize * 2;
        let body = rms_of(&out[lo..hi]);
        (20.0 * peak.log10(), 20.0 * body.log10())
    }

    /// Linear RMS of a slice, in f64 so a 30 s take does not lose the tail.
    fn rms_of(x: &[f32]) -> f32 {
        let sq: f64 = x.iter().map(|v| f64::from(*v) * f64::from(*v)).sum();
        (sq / x.len() as f64).sqrt() as f32
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

    /// Consecutive typed onsets never leap inside a word (A2), pinned on the
    /// bench's 60 s of prose at 10 cps. Between two `Typed` cues with no Space,
    /// line feed or Enter between them, an onset is a unison (a doubled letter)
    /// or a stride of at most [`WORD_LEAP_MAX_DEG`] degrees, which
    /// [`MelodyV2::derive`] guarantees by clamping. Also pinned: every
    /// consecutive lead-lane pair wider than a sixth straddles a `Jump`, and a
    /// line feed ends the word.
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
        let (mut pairs, mut near) = (0usize, 0usize);
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
            if leap.abs() <= 1 {
                near += 1;
            }
        }
        assert!(
            pairs >= 250,
            "only {pairs} in-word pairs — the take is not the bench's"
        );
        // §21.4's conjunct share: ≥ 60 % unison-or-one-degree. The derived line
        // lands well above it; gravity's ±1 and [`MELODY_RUN_MAX`]'s inversion
        // lift it over what [`stride_mag`]'s bands alone give, into the
        // 70-80 % conjunct range of real melodies.
        let pct = 100.0 * near as f32 / pairs as f32;
        println!("in-word conjunct share: {pct:.1} % of {pairs} pairs");
        assert!(
            pct >= 60.0,
            "only {pct:.0} % of {pairs} in-word pairs were a unison or one degree"
        );

        // The finding: every lead pair wider than a sixth is the wrap or
        // straddles a line feed.
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

    /// A mid-word rest never cadences by more than a sixth
    /// ([`MelodyV2::cadence_degree`]): a think-pause inside the word lands on
    /// the chord tone nearest the phrase's final degree within a sixth, while
    /// the same pause after a Space closes onto the final degree itself.
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
        assert!(
            !since(&s, mark).is_empty(),
            "the key that ends a rest must still sing its own note"
        );
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

        // The resolution itself is a chord tone, reached before the key derives
        // its own note, so a rest lands the line on the harmony.
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        push(&mut s, SoundKind::Typed, 1_100, 0.0, false);
        push(&mut s, SoundKind::Typed, 1_200, 0.0, false);
        let probe = s.v2;
        let resolved = {
            let here = i32::from(probe.walk());
            probe.nearest_lit_within(here, here)
        };
        assert!(
            probe.deg_is_lit(resolved),
            "the rest's resolution is not a chord tone"
        );
        assert!(
            (resolved - i32::from(probe.walk())).abs() <= WORD_HEAD_SNAP_DEG,
            "the rest's resolution moved more than the snap's own bound"
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
        // Mood still bends the decay (§10.4): the same step rings 1.06× longer
        // under Calm and 0.88× under Excited; feel, never pitch.
        let decay_under = |tone: Tone| -> f32 {
            let mut s = synth();
            let mut ev = event(SoundKind::Typed, 0.0, false);
            ev.tone = tone;
            let mark = s.born_seq;
            s.push_meta(
                ev,
                EventMeta {
                    at_ms: 1_000,
                    ..EventMeta::default()
                },
            );
            tune_voices(&since(&s, mark))[0].decay
        };
        let neutral = decay_under(Tone::Technical);
        for (tone, feel) in [(Tone::Calm, 1.06f32), (Tone::Excited, 0.88)] {
            let ratio = decay_under(tone) / neutral;
            assert!(
                (ratio - feel).abs() < 1e-4,
                "{tone:?} scaled the tine's decay by {ratio}, not tone_feel's {feel}"
            );
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

        let hist = |v: &[i32]| {
            let mut h = [0usize; (TUNE_DEG_HI + 1) as usize];
            for &d in v {
                h[d as usize] += 1;
            }
            h
        };
        let mean = |v: &[i32]| v.iter().sum::<i32>() as f32 / v.len() as f32;
        println!(
            "CAPITAL_LIFT_DEG {CAPITAL_LIFT_DEG}: shouted mean {:.2} {:?}\n\
             {:>25} spoken  mean {:.2} {:?}",
            mean(&caps),
            hist(&caps),
            "",
            mean(&lower),
            hist(&lower)
        );
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

    /// A capital is one strike on a lifted degree, and one ring (§3.1, §2.3
    /// i-ii; [`CAP_RING_LEVEL`]). The letter is its lowercase twin's step lifted
    /// [`CAPITAL_LIFT_DEG`] degrees through [`reflect_deg`], with one ring in
    /// [`LANE_GRAFT`] a twelfth up ([`CAP_RING_LIFT_DEG`]),
    /// [`CAP_RING_DELAY_S`] behind it on a [`CAP_RING_ATTACK_S`] swell, no
    /// mallet, at [`CAP_RING_LEVEL`] of the key's gain (same pan, same draw),
    /// and one sparkle at [`FORTE_GLINT_MUL`] × the lowercase key's. The
    /// lowercase twin spawns no GRAFT voice. The bare modifier is pinned in
    /// `a_bare_shift_is_a_ting_that_never_steps`.
    #[test]
    fn a_capital_is_one_strike_with_a_lifted_degree_and_one_ring() {
        let walk_after = |cap: bool| -> (i8, Vec<Voice>) {
            let mut s = synth();
            for (i, ch) in "hello ".chars().enumerate() {
                push_ch(
                    &mut s,
                    if ch == ' ' {
                        SoundKind::Space
                    } else {
                        SoundKind::Typed
                    },
                    1_000 + i as u32 * 150,
                    ch,
                );
            }
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Typed, 1_900, if cap { 'W' } else { 'w' });
            (s.v2.walk(), since(&s, mark))
        };
        let (low, plain) = walk_after(false);
        let (high, spawned) = walk_after(true);
        // The note the line came from — the walk after "hello ", which both
        // takes share.
        let from = {
            let mut s = synth();
            for (i, ch) in "hello ".chars().enumerate() {
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(&mut s, kind, 1_000 + i as u32 * 150, ch);
            }
            i32::from(s.v2.walk())
        };
        let want = {
            let raw = i32::from(low) + CAPITAL_LIFT_DEG;
            let r = reflect_deg(raw);
            if r == from { reflect_deg(raw + 1) } else { r }
        };
        assert_eq!(
            i32::from(high),
            want,
            "the capital `W` sounded degree {high} where `w` sounded {low} from \
             {from}: not {CAPITAL_LIFT_DEG} degrees up, reflected at the \
             register's bound and nudged off the note it came from"
        );
        assert_ne!(
            high, low,
            "the capital sounded its own lowercase twin's degree {low}: not an accent"
        );
        assert!(
            (TUNE_DEG_LO..=TUNE_DEG_HI).contains(&i32::from(high)),
            "the lift left the register at degree {high}"
        );
        let tune: Vec<&Voice> = spawned.iter().filter(|v| v.lane == LANE_TUNE).collect();
        assert_eq!(
            tune.len(),
            1,
            "a capital spawned {} TUNE voices, not one",
            tune.len()
        );
        assert!(
            tune[0].p[0].lvl > 0.0 && tune[0].delay == 0.0,
            "the capital's own note must be pitched and on the key"
        );
        let mag = |v: &Voice| (v.gl * v.gl + v.gr * v.gr).sqrt();
        // A hammer does not bend: the strike sits on the lattice note (the
        // line's degree) from its first sample.
        assert_eq!(tune[0].p[0].glide, 0.0);
        let f = tune[0].p[0].f0;
        assert_eq!(
            f,
            penta(TINE_BASE_HZ, i32::from(high)),
            "the capital strikes the line's own degree"
        );
        for v in &spawned {
            assert!(
                v.lane != LANE_TUNE || core::ptr::eq(v, tune[0]),
                "a second TUNE voice rode the capital"
            );
            assert!(
                (v.delay - 0.025).abs() > 1e-6,
                "the retired 25 ms echo is back (delay {} s, f0 {} Hz)",
                v.delay,
                v.p[0].f0
            );
        }
        // The ring: one GRAFT voice at the twelfth ([`CAP_RING_LIFT_DEG`]),
        // 60 ms on, swelling, no mallet, −6 dB re the key's gain on its pan.
        let rings: Vec<&Voice> = spawned.iter().filter(|v| v.lane == LANE_GRAFT).collect();
        assert_eq!(
            rings.len(),
            1,
            "a capital spawned {} GRAFT voices — one ring",
            rings.len()
        );
        let ring = rings[0];
        let twelfth = penta(
            TINE_BASE_HZ,
            i32::from(high) + FLOW_ECHO_OCTAVE_DEG + CAP_RING_LIFT_DEG,
        );
        assert!(
            (ring.p[0].f0 - twelfth).abs() < SAME_PITCH_HZ,
            "the ring is at {} Hz, not the key's twelfth {twelfth}",
            ring.p[0].f0,
        );
        assert_eq!(
            ring.delay, CAP_RING_DELAY_S,
            "the ring opens 60 ms behind the strike"
        );
        assert_eq!(ring.attack, CAP_RING_ATTACK_S, "the ring swells");
        assert_eq!(ring.n_lvl, 0.0, "the ring has no mallet — no second onset");
        assert_eq!(ring.p[2].lvl, 0.0, "…and no strike partial: it resonates");
        assert!(
            (mag(ring) / (mag(tune[0]) * CAP_RING_LEVEL) - 1.0).abs() < 1e-3,
            "the ring is {} against the key's {} × {CAP_RING_LEVEL}",
            mag(ring),
            mag(tune[0])
        );
        // The sparkle: one glint with the bloom at [`FORTE_GLINT_MUL`] × the
        // lowercase key's; same seed and draws up to it, so the ratio is exact.
        let glint = |v: &[Voice]| -> Vec<Voice> {
            v.iter()
                .filter(|v| v.lane == LANE_GLINT && (v.delay - KEY_GLINT_DELAY_S).abs() < 1e-6)
                .copied()
                .collect()
        };
        let (lower, upper) = (glint(&plain), glint(&spawned));
        assert_eq!(
            upper.len(),
            1,
            "a capital carries one sparkle at KEY_GLINT_DELAY_S"
        );
        assert_eq!(lower.len(), 1, "its lowercase twin carries one sparkle too");
        assert!(
            (mag(&upper[0]) / (mag(&lower[0]) * FORTE_GLINT_MUL) - 1.0).abs() < 1e-3,
            "the capital's sparkle is {} against the lowercase {} × {FORTE_GLINT_MUL}",
            mag(&upper[0]),
            mag(&lower[0])
        );
        assert!(
            plain.iter().all(|v| v.lane != LANE_GRAFT),
            "the lowercase `w` spawned a GRAFT voice — the ring leaked onto the plain path"
        );
    }

    /// A capital rings and out-peaks its plain self by its weight (§9.6;
    /// [`CAP_RING_DELAY_S`]). Over four seeds, every letter, two contexts (a
    /// word head after `"hello "` and mid-word after `"hel"`), on fair pairs
    /// (equal lit-ness, or a passing plain key against its lit capital, where
    /// the exact weight is `SHIFT_GLYPH_GAIN / PASSING_LEVEL`):
    ///
    /// - the capital's rendered sample peak sits within a window over its plain
    ///   self around the exact weight ([`super::SHIFT_GLYPH_GAIN`], plus
    ///   [`WORD_CAPITAL_GAIN`] for a word-opening capital);
    /// - the strike's first 80 ms of energy is ≥ +2.0 dB over the plain key in
    ///   every context and seed (phase-free, unlike the sample peak);
    /// - the body (80-300 ms, after every onset) is louder on the capital,
    ///   broadband and at the ring's own pitch: a single DFT bin at each key's
    ///   twelfth ([`CAP_RING_LIFT_DEG`]), with the plain key's twelfth as the
    ///   control.
    ///
    /// The music box's capital is the line's note struck forte ([`forte`]), not
    /// an octave lift; v1's octave lift ([`super::SHIFT_GLYPH_LIFT`]) is pinned
    /// separately (`a_shifted_glyph_lands_an_octave_over_its_plain_self`).
    /// [`FORTE_P2_SHIFT`] is fitted against the energy floor. The cross-context
    /// pin is
    /// `a_shifted_key_is_forte_not_higher_and_a_word_opening_capital_is_loudest`.
    #[test]
    fn a_capital_rings_and_out_peaks_its_plain_self_by_its_weight() {
        const SEEDS: [u32; 4] = [SEED, 0x5EED_1234, 0x504F_4F46, 0xCAFE_F00D];
        /// 400 ms: long enough for every "hello " tine to be under its tail
        /// law; whatever remains is identical in both takes.
        const HEAD_BLOCKS: usize = 40;
        /// 600 ms after the key: the strike, the ring and their tails.
        const TAKE_BLOCKS: usize = 60;
        /// The body window, in mono samples at 48 kHz: 80-300 ms.
        const BODY: core::ops::Range<usize> = 3_840..14_400;
        const PAIRS_PER_SEED: usize = 6;
        /// The capital's weight on the crest: at least +1.5 dB over the
        /// plain key (louder, unmistakably) …
        const CAPITAL_PEAK_FLOOR_DB: f32 = 1.5;
        /// The crest may sit at most this far above the exact weight: the ring
        /// buys no decibel of its own, but a sample peak wanders with the
        /// partials' seeded phases, so the ≥ 2 dB floor is pinned on energy
        /// below instead.
        const CAPITAL_PEAK_SLACK_DB: f32 = 1.9;
        /// The louder floor, on the strike's first 80 ms of
        /// RMS, where the phase cannot vote: ≥ +2.0 dB in every context …
        const CAPITAL_ONSET_FLOOR_DB: f32 = 2.0;
        /// … and within half a decibel of the exact weight (the bloom, its
        /// own unshifted gain, dilutes the window by 0.1-0.25 dB).
        const ONSET_DILUTION_DB: f32 = 0.5;
        /// The ting ahead of it is bounded by the coherent sum above, over
        /// the same exact weight.
        const ANNOUNCED_PEAK_SLACK_DB: f32 = 3.4;
        /// The body must carry the ring: broadband, over the bloom's hang…
        const BODY_FLOOR_DB: f32 = 1.5;
        /// …and at the twelfth, where the ring lives ([`CAP_RING_LIFT_DEG`]).
        const BODY_RING_FLOOR_DB: f32 = 3.0;
        /// The two contexts: a word head (after `"hello "`) and mid-word (after
        /// `"hel"`, no Space); the key under test lands at 1 900 ms in both.
        const CONTEXTS: [&str; 2] = ["hello ", "hel"];
        let head = |s: &mut TrailSynth, ctx: &str| {
            for (i, ch) in ctx.chars().enumerate() {
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(s, kind, 1_000 + i as u32 * 150, ch);
            }
        };
        // The fair pairs: the TUNE voice's gain is `level × vel` on a shared
        // first draw, so the gain ratio is the level ratio. A pair is fair when
        // that ratio is exactly the capital's weight (equal lit-ness) or the
        // weight over PASSING_LEVEL; anything else is a re-strike or another
        // touch.
        let tune_gain = |ctx: &str, ch: char| -> f32 {
            let mut s = synth();
            head(&mut s, ctx);
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Typed, 1_900, ch);
            let v = since(&s, mark)
                .into_iter()
                .find(|v| v.lane == LANE_TUNE)
                .expect("every key is a step");
            (v.gl * v.gl + v.gr * v.gr).sqrt()
        };
        // A word-opening capital carries [`WORD_CAPITAL_GAIN`] on top, so the
        // word-head context's exact weights are the mid-word ones times it:
        // +5.6 dB lit, +7.6 over a passing plain key.
        let weight_of = |ctx: &str, c: char| -> Option<f32> {
            let (lo, hi) = (tune_gain(ctx, c), tune_gain(ctx, c.to_ascii_uppercase()));
            let ratio = hi / lo;
            let head = if ctx.ends_with(' ') {
                WORD_CAPITAL_GAIN
            } else {
                1.0
            };
            [
                crate::trail_sound::SHIFT_GLYPH_GAIN * head,
                crate::trail_sound::SHIFT_GLYPH_GAIN * head / PASSING_LEVEL,
            ]
            .into_iter()
            .find(|w| (ratio / w - 1.0).abs() < 1e-4)
        };
        // A lattice pitch's twelfth — the ring's pitch over that strike
        // ([`CAP_RING_LIFT_DEG`]).
        let twelfth_of = |f: f32| -> f32 {
            let d = (-10..30)
                .find(|&d| penta(TINE_BASE_HZ, d) == f)
                .expect("a strike is a lattice pitch");
            penta(TINE_BASE_HZ, d + FLOW_ECHO_OCTAVE_DEG + CAP_RING_LIFT_DEG)
        };
        // One take: the render from the key (or from the Shift ahead of it)
        // and the key's own fundamental, for the ring's bin.
        let take = |seed: u32, ctx: &str, ch: char, lead_shift: bool| -> (Vec<f32>, f32) {
            let mut s = TrailSynth::new(SR, seed);
            head(&mut s, ctx);
            let _ = render_mono(&mut s, HEAD_BLOCKS);
            let mut out = Vec::new();
            if lead_shift {
                push(&mut s, SoundKind::Shift, 1_840, 0.0, false);
                out.extend(render_mono(&mut s, 6));
            }
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Typed, 1_900, ch);
            let partial = since(&s, mark)
                .iter()
                .find(|v| v.lane == LANE_TUNE)
                .expect("every key is a step")
                .p[0];
            let f = if partial.glide > 0.0 {
                partial.f1
            } else {
                partial.f0
            };
            out.extend(render_mono(&mut s, TAKE_BLOCKS));
            (out, f)
        };
        let db = |x: f32| 20.0 * x.log10();
        // The body is the ring: every onset is over by 80 ms.
        fn body(x: &[f32]) -> &[f32] {
            &x[x.len() - TAKE_BLOCKS * 480..][BODY]
        }
        // The onset is the strike: the key's first 80 ms.
        fn onset(x: &[f32]) -> &[f32] {
            &x[x.len() - TAKE_BLOCKS * 480..][..BODY.start]
        }
        // One bin of the DFT at `f`, as a magnitude: the instrument a
        // broadband RMS is not, under the bloom.
        let bin_at = |x: &[f32], f: f32| -> f32 {
            let w = core::f32::consts::TAU * f / SR;
            let (re, im) = x
                .iter()
                .enumerate()
                .fold((0.0f32, 0.0f32), |(re, im), (n, &v)| {
                    let ph = w * n as f32;
                    (re + v * ph.cos(), im - v * ph.sin())
                });
            (re * re + im * im).sqrt() / x.len() as f32
        };
        for ctx in CONTEXTS {
            let pairs: Vec<(char, f32)> = ('a'..='z')
                .filter_map(|c| weight_of(ctx, c).map(|w| (c, w)))
                .take(PAIRS_PER_SEED)
                .collect();
            assert_eq!(
                pairs.len(),
                PAIRS_PER_SEED,
                "only {pairs:?} land their capital on a fair level after {ctx:?}"
            );
            let mut lo_seen = f32::MAX;
            let mut hi_seen = f32::MIN;
            let mut onset_short = f32::MIN;
            let mut onset_lo = f32::MAX;
            for seed in SEEDS {
                for &(ch, weight) in &pairs {
                    let cap = ch.to_ascii_uppercase();
                    let weight_db = db(weight);
                    let (plain, f_plain) = take(seed, ctx, ch, false);
                    let (capital, f_cap) = take(seed, ctx, cap, false);
                    let (announced, _) = take(seed, ctx, cap, true);
                    let (p0, p1, p2) = (
                        db(peak_of(&plain)),
                        db(peak_of(&capital)),
                        db(peak_of(&announced)),
                    );
                    lo_seen = lo_seen.min(p1 - p0);
                    hi_seen = hi_seen.max(p1 - p0);
                    // The ≥ +2.0 dB floor on the strike's energy: the first
                    // 80 ms of the capital over the plain key's, phase-free.
                    // Lands at the exact weight less the bloom's small
                    // unshifted dilution.
                    let (s0, s1) = (db(rms_of(onset(&plain))), db(rms_of(onset(&capital))));
                    onset_short = onset_short.max(weight_db - (s1 - s0));
                    onset_lo = onset_lo.min(s1 - s0);
                    assert!(
                        s1 >= s0 + CAPITAL_ONSET_FLOOR_DB
                            && s1 >= s0 + weight_db - ONSET_DILUTION_DB,
                        "seed {seed:#x} `{cap}` after {ctx:?}: the strike's first 80 ms is \
                         {s1:.2} dB against the plain {s0:.2} — under the capital's weight \
                         (exact {weight_db:.2}, floor +{CAPITAL_ONSET_FLOOR_DB})"
                    );
                    assert!(
                        p1 >= p0 + CAPITAL_PEAK_FLOOR_DB
                            && p1 <= p0 + weight_db + CAPITAL_PEAK_SLACK_DB,
                        "seed {seed:#x} `{cap}` after {ctx:?}: the capital peaks {p1:.2} dBFS \
                         against its plain self's {p0:.2} — outside the capital's \
                         +{CAPITAL_PEAK_FLOOR_DB}..+{:.1} dB weight (exact weight {weight_db:.2})",
                        weight_db + CAPITAL_PEAK_SLACK_DB
                    );
                    assert!(
                        p2 <= p0 + weight_db + ANNOUNCED_PEAK_SLACK_DB,
                        "seed {seed:#x} Shift+`{cap}` after {ctx:?}: the announced capital \
                         peaks {p2:.2} dBFS against the plain {p0:.2} — the ting and the \
                         strike crest together beyond the coherent sum"
                    );
                    let (b0, b1) = (db(rms_of(body(&plain))), db(rms_of(body(&capital))));
                    assert!(
                        b1 >= b0 + BODY_FLOOR_DB,
                        "seed {seed:#x} `{cap}` after {ctx:?}: the body after the key is \
                         {b1:.2} dB against the plain {b0:.2} — the ring is not heard"
                    );
                    // At the twelfth: the plain take holds nothing there once
                    // its 2.76f strike partial is gone; the capital holds the
                    // ring. Each bin at its own key's twelfth, with the plain
                    // key's twelfth as the control.
                    let (o0, o1) = (
                        db(bin_at(body(&plain), twelfth_of(f_plain))),
                        db(bin_at(body(&capital), twelfth_of(f_cap))),
                    );
                    assert!(
                        o1 >= o0 + BODY_RING_FLOOR_DB,
                        "seed {seed:#x} `{cap}` after {ctx:?}: the twelfth in the body is \
                         {o1:.2} dB against the plain key's own twelfth {o0:.2} — the ring \
                         is not heard AS THE TWELFTH"
                    );
                }
            }
            println!(
                "capital after {ctx:?}: crest +{lo_seen:.2}..+{hi_seen:.2} dB over the plain \
                 key, onset energy ≥ +{onset_lo:.2} dB and at most {onset_short:.2} dB under \
                 the exact weight ({} pairs × {} seeds)",
                pairs.len(),
                SEEDS.len()
            );
        }
    }

    /// A bare Shift is a ting that never steps (§10.4, §11, §22 row 9; design on
    /// [`TING_ATTACK_S`]).
    ///
    /// The pitch is a tone of the chord the Space bar is playing. For every
    /// chord of the loop × every cursor step × `song_key` −3..=+4:
    ///
    /// - T1: `f0` is `fold(penta(class + key))` within 0.5 Hz and inside
    ///   `[1240, 2480)`, the class read from a table spelled out here by note
    ///   name, not from [`TING_TONES`];
    /// - T2: that class is lit in the live chord's mask;
    /// - T3: ten Shifts on a static chord: no two neighbours equal, exactly
    ///   three pitches (root, fifth, third);
    /// - at `song_key` 0 only five pitches exist: C7, G6, E6 (just, 1308.125 Hz),
    ///   A6, D7.
    ///
    /// The voice (T4-T9): no FM; the octave at exactly `2 × f0`; the 2.76f
    /// clink on a decay A11 exempts (≤ 45 ms); the mallet at 0.10 on the tine's
    /// rising band; a roof that is a real filter; [`LANE_TING`]; the
    /// 3 / 200 / 1005 ms envelope at `KEY_TINE_TRIM × TING_LEVEL` with no
    /// velocity draw.
    ///
    /// T10: exactly five rng draws (`v2_pan` 1 + spawn 4). T11: the melody has
    /// not moved: walk, step count, chord and v1's beat (`since_voice`) are
    /// unchanged. A second Shift 40 ms later damps the first (12 ms): never
    /// two tings.
    #[test]
    fn a_bare_shift_is_a_ting_that_never_steps() {
        // C D E G A = 0 1 2 3 4, per chord root name, in pick order
        // root / fifth / third, spelled here rather than read from the engine.
        let tones = |chord: usize| -> [i32; 3] {
            match chord {
                0 | 4 => [0, 3, 2], // I   C: C G E
                1 | 6 => [4, 2, 0], // vi  A: A E C
                2 | 7 => [0, 4, 2], // IV  F: C (F is off the lattice) A E
                3 | 5 => [3, 1, 4], // V   G: G D A
                _ => unreachable!("the loop is eight chords"),
            }
        };
        const PICK: [usize; 5] = [0, 1, 0, 2, 1];
        let fold = |mut f: f32| -> f32 {
            while f >= 2_480.0 {
                f *= 0.5;
            }
            while f < 1_240.0 {
                f *= 2.0;
            }
            f
        };
        let nominal = VOL * KEY_TINE_TRIM * TING_LEVEL;
        let mut at_key0: Vec<f32> = Vec::new();
        for (chord, live) in CHORD_LOOP.iter().enumerate() {
            for key in -3i8..=4 {
                let mut heard: Vec<f32> = Vec::new();
                let mut s = synth();
                s.v2.text.chord = chord as u8;
                s.song_key = key;
                for n in 0..10usize {
                    let k = n % 5;
                    assert_eq!(usize::from(s.shift_step), k, "the cursor is v1's, mod 5");
                    let (mark, rng) = (s.born_seq, s.rng);
                    push(&mut s, SoundKind::Shift, 1_000 + n as u32 * 300, 0.0, false);
                    let lift = since(&s, mark);
                    assert_eq!(lift.len(), 1, "chord {chord} Shift {n}: one voice");
                    let v = lift[0];
                    // T1 / T2.
                    let class = tones(chord)[PICK[k]];
                    assert!(
                        live.lit & (1 << class) != 0,
                        "chord {chord} pick {k}: class {class} is not lit in {:#07b}",
                        live.lit
                    );
                    let want = fold(penta(TINE_BASE_HZ, class + i32::from(key)));
                    assert!(
                        (v.p[0].f0 - want).abs() < 0.5 && (1_240.0..2_480.0).contains(&v.p[0].f0),
                        "chord {chord} key {key} Shift {n}: the ting sounded {} Hz, not \
                         class {class} folded ({want} Hz)",
                        v.p[0].f0
                    );
                    // T4-T9.
                    assert!(
                        v.p.iter()
                            .all(|p| p.fm_ratio == 0.0 && p.fm_i0 == 0.0 && p.glide == 0.0),
                        "Shift {n}: the ting wears no FM and does not bend"
                    );
                    assert_eq!(v.p[0].lvl, P1_LVL);
                    assert_eq!(v.p[1].f0, 2.0 * v.p[0].f0, "the octave, exactly");
                    assert_eq!((v.p[1].lvl, v.p[1].decay), (0.12, 0.140));
                    assert!((v.p[2].f0 / v.p[0].f0 - 2.760).abs() < 1e-4 && v.p[2].lvl == 0.08);
                    assert!(
                        v.p[2].decay > 0.0 && v.p[2].decay <= 0.045,
                        "the clink must die inside A11's 45 ms exemption ({} s)",
                        v.p[2].decay
                    );
                    assert_eq!(
                        v.n_lvl, 0.10,
                        "the felt mallet, at under a quarter of the tine's"
                    );
                    assert_eq!(
                        (v.n_f0, v.n_f1, v.n_q, v.n_decay),
                        (MALLET_HZ0, MALLET_HZ1, MALLET_Q, MALLET_TAU_S),
                        "…and on the tine's own rising band"
                    );
                    assert_eq!(v.lp_cut, 6_500.0);
                    assert!(
                        v.lp_cut * core::f32::consts::TAU / SR < 1.0,
                        "the roof must be a filter: lp_k saturates at 7639 Hz"
                    );
                    assert_eq!(v.lane, LANE_TING, "the ting lives in its own lane");
                    assert_eq!((v.attack, v.decay, v.dur), (0.003, 0.200, 1.005));
                    let got = (v.gl * v.gl + v.gr * v.gr).sqrt();
                    assert!(
                        (got / nominal - 1.0).abs() < 0.05,
                        "the ting is {got} against KEY_TINE_TRIM × TING_LEVEL = {nominal}"
                    );
                    // T10.
                    let mut x = rng;
                    for _ in 0..5 {
                        x ^= x << 13;
                        x ^= x >> 17;
                        x ^= x << 5;
                    }
                    assert_eq!(
                        s.rng, x,
                        "a Shift makes five draws: one pan, four for the spawn"
                    );
                    heard.push(v.p[0].f0);
                }
                // T3.
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
                if key == 0 {
                    at_key0.extend(distinct);
                }
                // T11, on the static chord.
                assert_eq!(
                    usize::from(s.v2.text.chord),
                    chord,
                    "a Shift moved the harmony"
                );
            }
        }
        at_key0.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
        at_key0.dedup_by(|a, b| (*a - *b).abs() < SAME_PITCH_HZ);
        let names = [1_308.125f32, 1_569.75, 1_744.17, 2_093.0, 2_354.6];
        assert!(
            at_key0.len() == names.len()
                && at_key0.iter().zip(names).all(|(a, b)| (a - b).abs() < 0.5),
            "at song_key 0 the ting sang {at_key0:?}, not E6 G6 A6 C7 D7"
        );

        // T11, after real typing: five Shifts step nothing.
        let mut s = synth();
        for (i, ch) in "hello ".chars().enumerate() {
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            push_ch(&mut s, kind, 1_000 + i as u32 * 150, ch);
        }
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

    /// A radix-2 transform in f64: [`bin_powers`]' numbers (Hann, one-sided,
    /// normalised by the window's power) at `n log n`, for pins reading a whole
    /// second of ting. `x.len()` must be a power of two.
    fn fft_powers(x: &[f32]) -> Vec<f64> {
        let n = x.len();
        assert!(n.is_power_of_two());
        let tau = core::f64::consts::TAU;
        let win: Vec<f64> = (0..n)
            .map(|i| 0.5 * (1.0 - (tau * i as f64 / n as f64).cos()))
            .collect();
        let wsum: f64 = win.iter().map(|w| w * w).sum();
        let mut re: Vec<f64> = x.iter().zip(&win).map(|(s, w)| f64::from(*s) * w).collect();
        let mut im = vec![0.0f64; n];
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
            let ang = -tau / len as f64;
            for i in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let (cr, ci) = ((ang * k as f64).cos(), (ang * k as f64).sin());
                    let (a, b) = (i + k, i + k + len / 2);
                    let (vr, vi) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                    (re[b], im[b]) = (re[a] - vr, im[a] - vi);
                    (re[a], im[a]) = (re[a] + vr, im[a] + vi);
                }
            }
            len <<= 1;
        }
        (0..n / 2)
            .map(|k| 2.0 * (re[k] * re[k] + im[k] * im[k]) / (n as f64 * wsum))
            .collect()
    }

    /// Bin powers summed over 4096-point Hann frames hopping 2048 through
    /// `x[from..to]` (a short tail is dropped).
    fn framed_powers(x: &[f32], from: usize, to: usize) -> Vec<f64> {
        let mut acc = vec![0.0f64; 2_048];
        let mut at = from;
        while at + 4_096 <= to.min(x.len()) {
            for (a, p) in acc.iter_mut().zip(fft_powers(&x[at..at + 4_096])) {
                *a += p;
            }
            at += 2_048;
        }
        acc
    }

    /// The ting is musical, not harsh. Rendered alone in a silent box, vol 0.4,
    /// on each of its five pitches at `song_key` 0 over three seeds, beside an
    /// FM-struck ting (3.01 FM at index 1.3 / 18 ms, 0.45 mallet, 85 ms, a 9 kHz
    /// bypass roof) rebuilt here as every clause's negative control:
    ///
    /// - T12, a note: energy outside ±3 % of f0 and 2f0 is ≤ −30 dB re the
    ///   total, over 40-400 ms and over 0-400 ms from the key. The negative
    ///   control is on the second reading: the FM clank lasts 18 ms and its
    ///   lower sideband (2.01f) sits inside the octave's band, so it passes the
    ///   first.
    /// - T13 / T14, not harsh: energy over 6 kHz ≤ 0.02 of the total; no bin
    ///   over 8 kHz within 50 dB of the loudest.
    /// - T15, the strike is felt, not a click: the first 60 ms centroid is
    ///   ≤ 1.6 × f0.
    /// - T16, it rings: 250 ms after the key the envelope is ≥ −12 dB re peak.
    #[test]
    fn the_ting_is_musical_not_harsh() {
        const SEEDS: [u32; 3] = [SEED, 0x5EED_1234, 0xCAFE_F00D];
        /// T12. Worst case is E6, where ±3 % is narrowest against the clink's
        /// skirt; the clink and mallet are the strike.
        const INHARMONIC_CEIL_DB: f64 = -30.0;
        /// T13 (D7's clink sits at 6.5 kHz).
        const OVER_6K_CEIL: f64 = 0.02;
        /// T14.
        const OVER_8K_CEIL_DB: f64 = -50.0;
        /// T15.
        const ONSET_CENTROID_CEIL: f32 = 1.6;
        /// T16.
        const RING_250_FLOOR_DB: f32 = -12.0;
        let gain = VOL * KEY_TINE_TRIM * TING_LEVEL;
        // The FM-struck control ting, from v1's constants.
        let old = |f: f32| -> Voice {
            use crate::trail_sound::{
                SHIFT_TING_DECAY_S, SHIFT_TING_DUR_S, SHIFT_TING_GLINT, SHIFT_TING_GLINT_TAU_S,
            };
            Voice {
                dur: SHIFT_TING_DUR_S,
                attack: 0.003,
                decay: SHIFT_TING_DECAY_S,
                p: [
                    Partial {
                        lvl: P1_LVL,
                        f0: f,
                        fm_ratio: 3.01,
                        fm_i0: SHIFT_TING_GLINT,
                        fm_tau: SHIFT_TING_GLINT_TAU_S,
                        ..Partial::default()
                    },
                    Partial {
                        lvl: P2_LVL,
                        f0: f * P2_RATIO,
                        decay: P2_TAU_S,
                        ..Partial::default()
                    },
                    Partial::default(),
                ],
                n_lvl: MALLET_LVL,
                n_f0: MALLET_HZ0,
                n_f1: MALLET_HZ1,
                n_glide: MALLET_GLIDE_S,
                n_q: MALLET_Q,
                n_decay: MALLET_TAU_S,
                lp_cut: 9_000.0,
                lane: LANE_TING,
                ..Voice::default()
            }
        };
        let alone = |seed: u32, v: Voice| -> Vec<f32> {
            let mut s = TrailSynth::new(SR, seed);
            assert!(s.v2_spawn(v, gain, 0.0).is_some());
            render_mono(&mut s, 104)
        };
        // Energy outside ±3 % of f0 and of 2f0, re the total, over
        // `x[from..19_200]` (to 400 ms).
        let inharmonic_db = |x: &[f32], f: f32, from: usize| -> f64 {
            let p = framed_powers(x, from, 19_200);
            let total: f64 = p.iter().sum();
            let off: f64 = p
                .iter()
                .enumerate()
                .filter(|(k, _)| {
                    let h = *k as f32 * SR / 4_096.0;
                    (h / f - 1.0).abs() > 0.03 && (h / (2.0 * f) - 1.0).abs() > 0.03
                })
                .map(|(_, e)| e)
                .sum();
            10.0 * (off / total).log10()
        };
        let over_8k_db = |p: &[f64]| -> f64 {
            let loudest = p.iter().fold(0.0f64, |m, e| m.max(*e));
            let over = p[bin_of(8_000.0, 4_096)..]
                .iter()
                .fold(0.0f64, |m, e| m.max(*e));
            10.0 * (over / loudest).log10()
        };
        let ring_250_db = |x: &[f32]| -> f32 {
            let env: Vec<f32> = x.chunks(96).map(rms_of).collect();
            let peak = env.iter().fold(0.0f32, |m, e| m.max(*e));
            20.0 * (env[125] / peak).log10()
        };
        // Worst of the new ting; best of the control (so the control fails on
        // every pitch and seed tried, except T14, which only its two top
        // pitches reach: worst there).
        let (mut t12_body, mut t12, mut t12_old) = (f64::MIN, f64::MIN, f64::MAX);
        let (mut t13, mut t14, mut t14_old) = (f64::MIN, f64::MIN, f64::MIN);
        let (mut t15, mut t15_old) = (f32::MIN, f32::MAX);
        let (mut t16, mut t16_old) = (f32::MAX, f32::MIN);
        // E6 G6 A6 C7 D7: classes 2 3 4 0 1.
        for class in [2, 3, 4, 0, 1] {
            let f = ting_fold(penta(TINE_BASE_HZ, class));
            for seed in SEEDS {
                let x = alone(seed, ting(f));
                let was = alone(seed, old(f));
                t12_body = t12_body.max(inharmonic_db(&x, f, 1_920));
                t12 = t12.max(inharmonic_db(&x, f, 0));
                t12_old = t12_old.min(inharmonic_db(&was, f, 0));
                let p = framed_powers(&x, 0, x.len());
                let total: f64 = p.iter().sum();
                let over6: f64 = p[bin_of(6_000.0, 4_096)..].iter().sum();
                t13 = t13.max(over6 / total);
                t14 = t14.max(over_8k_db(&p));
                t14_old = t14_old.max(over_8k_db(&framed_powers(&was, 0, was.len())));
                t15 = t15.max(centroid_hz(&x[..2_880]) / f);
                t15_old = t15_old.min(centroid_hz(&was[..2_880]) / f);
                t16 = t16.min(ring_250_db(&x));
                t16_old = t16_old.max(ring_250_db(&was));
            }
        }
        println!(
            "ting: inharmonic {t12_body:.1} dB over 40-400 ms and {t12:.1} from the key (old \
             recipe, best {t12_old:.1}); over 6 kHz {t13:.4}; loudest bin over 8 kHz {t14:.1} dB \
             (old {t14_old:.1}); onset centroid x{t15:.3} f0 (old, best x{t15_old:.3}); 250 ms \
             {t16:.1} dB (old {t16_old:.1})"
        );
        assert!(
            t12_body <= INHARMONIC_CEIL_DB && t12 <= INHARMONIC_CEIL_DB,
            "T12: {t12_body:.1} dB of the ting's 40-400 ms, {t12:.1} dB of its first 400, is \
             neither its note nor its octave"
        );
        assert!(
            t12_old > INHARMONIC_CEIL_DB + 6.0,
            "T12 negative control: the 3.01-FM recipe reads {t12_old:.1} dB from the key — the \
             instrument cannot hear the clank this pin exists to refuse"
        );
        assert!(
            t13 <= OVER_6K_CEIL,
            "T13: {t13:.4} of the ting is over 6 kHz"
        );
        assert!(
            t14 <= OVER_8K_CEIL_DB && t14_old > OVER_8K_CEIL_DB,
            "T14: a bin over 8 kHz at {t14:.1} dB (control: the old recipe, {t14_old:.1})"
        );
        assert!(
            t15 <= ONSET_CENTROID_CEIL && t15_old > ONSET_CENTROID_CEIL,
            "T15: onset centroid x{t15:.3} f0 (control: the old recipe, x{t15_old:.3})"
        );
        assert!(
            t16 >= RING_250_FLOOR_DB && t16_old < RING_250_FLOOR_DB,
            "T16: the ting is {t16:.1} dB at 250 ms (the old one {t16_old:.1}) — it must RING, \
             and the instrument must be able to tell"
        );
    }

    /// A strike or a ring on the ting's own pitch never combs (§9.5 law 5).
    ///
    /// The fixture searches rather than forcing a degree: a context line, then a
    /// Shift on each chord × cursor step, then a word head (every letter,
    /// lowercase for T21 and capital for T22) 60 ms on. Each take has a twin in
    /// which the ting was detuned off the lattice before the key, so the two
    /// differ only in the collision.
    ///
    /// - T21: a strike within a cent of the live ting damps it over
    ///   [`LANE_FADE_STEAL_S`]; any other strike leaves it ringing.
    /// - T22: a capital whose ring would land within a cent of the live ting
    ///   leaves no ring, and its four draws are still made: the rng and the
    ///   next key's seeded phases and gains equal the twin's. Also driven
    ///   forced (the ting tuned onto each ringing capital's twelfth), since
    ///   natural word heads rarely put the ring on the ting.
    /// - Order: when the strike took the ting's pitch, the ting is already
    ///   fading and costs the capital nothing.
    #[test]
    fn a_strike_or_a_ring_on_the_tings_pitch_never_combs() {
        let cents = |a: f32, b: f32| (1200.0 * (a / b).log2()).abs();
        let (mut strikes, mut yields, mut misses, mut forced_yields) = (0, 0, 0, 0);
        // The contexts typed before the Shift. The ring (a twelfth,
        // [`CAP_RING_LIFT_DEG`]) reaches the ting's 1240-2480 Hz only from the
        // verse's lowest degrees, so these lines leave word heads down there.
        const CONTEXTS: [&str; 4] = ["hello ", "a ", "zoo ", "mmm tt "];
        for ctx in CONTEXTS {
            for chord in 0..CHORD_LOOP.len() as u8 {
                for k in 0..5u8 {
                    let mut base = synth();
                    for (i, ch) in ctx.chars().enumerate() {
                        let kind = if ch == ' ' {
                            SoundKind::Space
                        } else {
                            SoundKind::Typed
                        };
                        push_ch(&mut base, kind, 1_000 + i as u32 * 150, ch);
                    }
                    let _ = render_mono(&mut base, 40);
                    base.v2.text.chord = chord;
                    base.shift_step = k;
                    push(&mut base, SoundKind::Shift, 2_400, 0.0, false);
                    let (slot, born) = base.v2.lift.expect("the ting's address");
                    let slot = usize::from(slot);
                    let ting_f = base.voices[slot].p[0].f0;
                    for ch in ('a'..='z').chain('A'..='Z') {
                        let run = |detuned: bool| -> (TrailSynth, Vec<Voice>) {
                            let mut s = base.clone();
                            if detuned {
                                // A quarter-tone off every lattice pitch.
                                s.voices[slot].p[0].f0 *= 1.03;
                            }
                            let mark = s.born_seq;
                            push_ch(&mut s, SoundKind::Typed, 2_460, ch);
                            let spawned = since(&s, mark);
                            (s, spawned)
                        };
                        let (s, spawned) = run(false);
                        let (twin, twin_spawned) = run(true);
                        let strike = tune_voices(&spawned)[0];
                        let ting_v = s.voices[slot];
                        assert!(ting_v.on && ting_v.born == born);
                        let struck_on_it = cents(strike.p[0].f0, ting_f) < 1.0;
                        // T21.
                        if struck_on_it {
                            strikes += 1;
                            assert_eq!(
                                ting_v.damp, LANE_FADE_STEAL_S,
                                "chord {chord} step {k} `{ch}`: a strike on the ting's own \
                                 {ting_f} Hz left it ringing — two phases at one pitch"
                            );
                        } else {
                            assert_eq!(
                                ting_v.damp, 0.0,
                                "chord {chord} step {k} `{ch}`: a strike at {} Hz damped a ting \
                                 at {ting_f} Hz",
                                strike.p[0].f0
                            );
                        }
                        assert_eq!(twin.voices[slot].damp, 0.0, "the twin's ting is off-pitch");
                        // T22.
                        let twin_rang = twin.v2.ring.is_some();
                        // The ring's pitch is read off the twin's ring (the
                        // twin's detune moved only the ting).
                        let ring_on_it = twin.v2.ring.is_some_and(|(slot, _)| {
                            cents(twin.voices[usize::from(slot)].p[0].f0, ting_f) < 1.0
                        });
                        if ring_on_it && !struck_on_it {
                            yields += 1;
                            assert!(
                                s.v2.ring.is_none()
                                    && !spawned.iter().any(|v| v.lane == LANE_GRAFT),
                                "chord {chord} step {k} `{ch}`: a ring swelled in on the live \
                                 ting's own {ting_f} Hz"
                            );
                            assert_eq!(spawned.len() + 1, twin_spawned.len());
                        } else {
                            if twin_rang {
                                misses += 1;
                            }
                            assert_eq!(
                                s.v2.ring.is_some(),
                                twin_rang,
                                "chord {chord} step {k} `{ch}`: the ring yielded to a ting it \
                                 does not collide with"
                            );
                        }
                        // …and the seeded stream is where the twin's is, whatever
                        // happened: the next key is the same voice.
                        assert_eq!(s.rng, twin.rng, "chord {chord} step {k} `{ch}`: the draws");
                        let next = |mut s: TrailSynth| -> Vec<(f32, f32, f32)> {
                            let mark = s.born_seq;
                            push_ch(&mut s, SoundKind::Typed, 2_610, 'e');
                            since(&s, mark)
                                .iter()
                                .map(|v| (v.p[0].ph, v.gl, v.gr))
                                .collect()
                        };
                        // T22, forced: natural word heads almost never put a
                        // ring on the ting's pitch, so every capital that rang
                        // is typed again with the ting tuned onto its ring (a
                        // real collision to the engine, which reads the voice)
                        // and must yield as a natural one would.
                        if let Some((ring_slot, _)) = twin.v2.ring {
                            let ring_f = twin.voices[usize::from(ring_slot)].p[0].f0;
                            let mut tuned = base.clone();
                            tuned.voices[slot].p[0].f0 = ring_f;
                            let mark = tuned.born_seq;
                            push_ch(&mut tuned, SoundKind::Typed, 2_460, ch);
                            let forced = since(&tuned, mark);
                            forced_yields += 1;
                            assert!(
                                tuned.v2.ring.is_none()
                                    && !forced.iter().any(|v| v.lane == LANE_GRAFT),
                                "chord {chord} step {k} `{ch}`: a ring swelled in on a live \
                                 ting tuned to its own {ring_f} Hz"
                            );
                            assert_eq!(forced.len() + 1, twin_spawned.len());
                            assert_eq!(tuned.rng, twin.rng, "`{ch}`: the refused ring's draws");
                            assert_eq!(
                                next(tuned),
                                next(twin.clone()),
                                "`{ch}`: the key behind the capital"
                            );
                        }
                        if ring_on_it {
                            assert_eq!(next(s), next(twin), "`{ch}`: the key behind the capital");
                        }
                    }
                }
            }
        }
        println!(
            "strikes on the ting {strikes}, rings yielded {yields} (forced {forced_yields}), \
             rings unaffected {misses}"
        );
        // Natural plus forced ring collisions.
        assert!(
            strikes > 0 && yields + forced_yields > 0 && misses > 0,
            "fixture: the search must reach a strike collision ({strikes}), a ring collision \
             ({yields} natural, {forced_yields} forced) and a ring that does not collide \
             ({misses})"
        );
    }

    /// The bell yields to the shifted key it announced, and to nothing else
    /// ([`TING_DUCK`]). Shift, then 60 ms later a key:
    ///
    /// - a capital letter opening a word or a shifted run arms the glide once
    ///   (far-end gains [`TING_DUCK`] × the near-end ones, so pan holds, over
    ///   [`TING_DUCK_RAMP_S`], on the voice's own clock) without damping it:
    ///   the address stays live;
    /// - a shifted mark and `!` arm the same glide to [`TING_DUCK_MARK`];
    /// - a lowercase letter, a digit, a Space and the second capital of a run
    ///   behind one Shift arm nothing;
    /// - it draws nothing: the seeded stream behind the capital matches the
    ///   unducked twin's (`ting_unducked`);
    /// - rendered, the ting's lane alone: 20 ms past the key the ducked take is
    ///   [`TING_DUCK`] × the unducked one sample for sample (to a part in a
    ///   thousand), and its largest sample-to-sample move across the ramp is no
    ///   larger than the unducked bell's.
    #[test]
    fn the_bell_yields_to_the_shifted_key_it_announced_and_to_nothing_else() {
        // → (the ting after the key, the rng after the key, the ting's lane
        // rendered 40 ms from the key).
        let take = |ctx: &str, keys: &str, unducked: bool| -> (Voice, u32, Vec<f32>) {
            let mut s = synth();
            s.ting_unducked = unducked;
            let mut at = 1_000u32;
            for c in ctx.chars() {
                let kind = if c == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(&mut s, kind, at, c);
                let _ = render_mono(&mut s, 15);
                at += 150;
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
            let rng = s.rng;
            s.mute_all_lanes_but(LANE_TING);
            (v, rng, render_mono(&mut s, 4))
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
            let (v, rng, x) = take(ctx, keys, false);
            let (plain, rng0, x0) = take(ctx, keys, true);
            assert_eq!(
                rng, rng0,
                "{ctx:?} + {keys:?}: the duck drew from the stream"
            );
            assert_eq!(plain.pan_glide_s, 0.0, "control: the unducked twin glides");
            let Some(duck) = ducks else {
                assert_eq!(
                    (v.pan_glide_s, v.gl1, v.gr1),
                    (0.0, v.gl, v.gr),
                    "{ctx:?} + {keys:?}: only the capital it announced ducks the ting"
                );
                assert_eq!(x, x0, "{ctx:?} + {keys:?}: the ting's lane moved");
                continue;
            };
            assert_eq!(
                (v.pan_glide_s, v.gl1, v.gr1),
                (TING_DUCK_RAMP_S, v.gl * duck, v.gr * duck),
                "{ctx:?} + {keys:?}: the far end is {duck} × the near end, pan unmoved"
            );
            assert_eq!((v.gl, v.gr), (plain.gl, plain.gr), "the near end moved");
            let armed = 0.060 - 1.0 / SR..=0.060 + 1.0 / SR;
            assert!(
                armed.contains(&v.pan_t0),
                "{ctx:?} + {keys:?}: the glide starts at the key ({} s)",
                v.pan_t0
            );
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
        let (one, ..) = take("hello ", "W", false);
        let (two, ..) = take("hello ", "WO", false);
        assert_eq!(
            (two.pan_t0, two.gl1, two.gr1),
            (one.pan_t0, one.gl1, one.gr1),
            "a run's second capital re-armed the duck"
        );
    }

    /// The ting rings through the key it announces, whatever the key: over a
    /// capital, a lowercase letter, a Space, a Backspace, an arrow and a
    /// Stardust hero there is no damp and its address stays live (so a second
    /// Shift can still replace it). Every key here is off the ting's pitch;
    /// `a_strike_or_a_ring_on_the_tings_pitch_never_combs` pins the other case.
    ///
    /// Also through a shifted mark's grafts, rendered: Shift then `?` / `(` /
    /// `+` renders 160 ms past the key (every deferred graft has opened: ring
    /// and rise at 60 ms, tink at 45 ms, fifth at 30 ms) and the ting is still
    /// on, undamped, at its own address. A push-time check cannot see an
    /// onset steal; [`LANE_TING`] keeps grafts from reaching it.
    #[test]
    fn the_ting_rings_through_the_next_key() {
        let after = |kind: SoundKind, ch: Option<char>, shifted: bool| -> (f32, bool, bool) {
            let mut s = synth();
            for (i, ch) in "hello ".chars().enumerate() {
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(&mut s, kind, 1_000 + i as u32 * 150, ch);
            }
            push(&mut s, SoundKind::Shift, 1_900, 0.0, false);
            let (slot, born) = s.v2.lift.expect("the Shift left its pickup's address");
            match ch {
                Some(c) if shifted => s.push_meta(
                    event(kind, 0.0, true),
                    EventMeta {
                        at_ms: 1_960,
                        glyph_class: crate::trail_sound::typed_glyph_class(Some(c)),
                        rank: crate::trail_sound::typed_glyph_rank(Some(c)),
                        ..EventMeta::default()
                    },
                ),
                Some(c) => push_ch(&mut s, kind, 1_960, c),
                None => push(&mut s, kind, 1_960, 0.0, false),
            }
            let v = &s.voices[usize::from(slot)];
            assert!(
                v.on && v.born == born,
                "{kind:?}: the ting's slot was recycled under it"
            );
            let at_push = v.damp;
            // 160 ms of audio past the key: every deferred graft has opened
            // and run its lane census.
            let mut buf = [0.0f32; 960];
            for _ in 0..16 {
                s.render(&mut buf);
            }
            let v = &s.voices[usize::from(slot)];
            let rendered_ok = v.on && v.born == born && v.damp == 0.0;
            (at_push, rendered_ok, s.v2.lift.is_none())
        };
        for (kind, ch, shifted, what) in [
            (SoundKind::Typed, Some('W'), true, "a capital"),
            (SoundKind::Typed, Some('w'), false, "a lowercase letter"),
            (
                SoundKind::Typed,
                Some('?'),
                true,
                "a shifted `?` (rise + ring)",
            ),
            (
                SoundKind::Typed,
                Some('('),
                true,
                "a shifted `(` (tink + ring)",
            ),
            (
                SoundKind::Typed,
                Some('+'),
                true,
                "a shifted `+` (fifth + ring)",
            ),
            (
                SoundKind::Typed,
                Some('{'),
                true,
                "a shifted `{` (tink + ring)",
            ),
            (SoundKind::Space, None, false, "a space"),
            (SoundKind::Backspace, None, false, "a deletion"),
            (SoundKind::Navigation, None, false, "an arrow"),
            (
                SoundKind::Stardust { twinkle_hz: 9 },
                None,
                false,
                "a Stardust hero",
            ),
        ] {
            let (damp, rendered_ok, cleared) = after(kind, ch, shifted);
            assert_eq!(
                damp, 0.0,
                "{what} must let the ting ring through it (damp {damp}) — the 2026-09-10 \
                 resolve is the sound the owner could not hear"
            );
            assert!(
                rendered_ok,
                "{what}: 160 ms on, the ting must still be on, undamped and at its own \
                 address — a graft's onset must not fade-steal it"
            );
            assert!(!cleared, "{what} must leave the ting's address live");
        }
    }

    /// A digit is a wood bar that counts in the stardust ([`WOOD_P2_RATIO`],
    /// [`COUNT_GLINT_DEG0`]). `0`..`9` at 8 cps after "hello ", against a twin
    /// fed the same ranks with the class forced to `LETTER` (same seed and
    /// stamps). Each digit is exactly one TUNE voice with partials at 3f and 5f
    /// on the bar's levels `[0.44, 0.26, 0.08]` (the letter's 0.78, conserved),
    /// decaying `45 / 30 ms`, on a τ of `0.7 × τ_v(IOI)`, struck with the
    /// 1400 → 3600 Hz "tok". It blooms exactly when the twin blooms, on the
    /// twin's bloom τ. Its one sparkle is on the counting degree (G7 A7 C8 D8
    /// E8, then again) at `GLINT_LEVEL × KEY_GLINT_LEVEL_MUL × 2` for `0..4` and
    /// `× 4` for `5..9` once the seeded velocity is divided out, so `7` against
    /// `2` is exactly 2.0. The rotation cursor has not moved, and the walk is
    /// the twin's, key for key: a digit's class never moves a degree.
    #[test]
    fn a_digit_is_a_wood_bar_that_counts_in_the_stardust() {
        const COUNT_HZ: [f32; 5] = [3139.5, 3488.33, 4186.0, 4709.25, 5232.5];
        const PERIOD_MS: u32 = 125;
        assert_eq!(
            i32::from(crate::trail_sound::typed_glyph_rank(Some('0'))),
            DIGIT_RANK0,
            "the digit row of the rank table moved under DIGIT_RANK0"
        );
        assert!(
            (WOOD_P1_LVL - 0.44).abs() < 1e-6
                && (WOOD_P2_LVL - 0.26).abs() < 1e-6
                && (WOOD_P1_LVL + WOOD_P2_LVL + WOOD_P3_LVL - (P1_LVL + P2_LVL + P3_LVL)).abs()
                    < 1e-6,
            "the bar's partial sum is not the letter's"
        );
        let head = |s: &mut TrailSynth| {
            for (i, ch) in "hello ".chars().enumerate() {
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(s, kind, 1_000 + i as u32 * 150, ch);
            }
            let _ = render_mono(s, 12);
        };
        let mag = |v: &Voice| (v.gl * v.gl + v.gr * v.gr).sqrt();
        let mut s = synth();
        let mut twin = synth();
        head(&mut s);
        head(&mut twin);
        let k0 = s.v2.glint_k;
        let mut walks = Vec::new();
        let mut twin_walks = Vec::new();
        let mut count_levels = Vec::new();
        let mut bloomed = 0usize;
        for (d, ch) in ('0'..='9').enumerate() {
            let at = 1_900 + d as u32 * PERIOD_MS;
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Typed, at, ch);
            let spawned = since(&s, mark);
            let tmark = twin.born_seq;
            twin.push_meta(
                event(SoundKind::Typed, 0.0, false),
                EventMeta {
                    at_ms: at,
                    glyph_class: LETTER,
                    rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                    ..EventMeta::default()
                },
            );
            let plain = since(&twin, tmark);
            walks.push(s.v2.walk());
            twin_walks.push(twin.v2.walk());
            // THE BAR.
            let tune: Vec<&Voice> = spawned.iter().filter(|v| v.lane == LANE_TUNE).collect();
            assert_eq!(tune.len(), 1, "`{ch}` spawned {} TUNE voices", tune.len());
            let t = tune[0];
            let f = t.p[0].f0;
            assert!(
                (t.p[1].f0 - WOOD_P2_RATIO * f).abs() < 0.5
                    && (t.p[2].f0 - WOOD_P3_RATIO * f).abs() < 0.5,
                "`{ch}`: partials at {} / {} Hz over {f} — not 3f / 5f",
                t.p[1].f0,
                t.p[2].f0
            );
            assert_eq!(
                [t.p[0].lvl, t.p[1].lvl, t.p[2].lvl],
                [WOOD_P1_LVL, WOOD_P2_LVL, WOOD_P3_LVL],
                "`{ch}`: the bar's levels"
            );
            assert_eq!(
                (t.p[1].decay, t.p[2].decay),
                (WOOD_P2_TAU_S, WOOD_P3_TAU_S),
                "`{ch}`: the bar's partial decays"
            );
            let tau = tau_v_s(s.v2.ioi_ms * 0.001) * DIGIT_TAU_MUL;
            assert!(
                (t.decay - tau).abs() < 1e-6,
                "`{ch}`: τ {} against the blip's {tau}",
                t.decay
            );
            assert_eq!(
                (t.n_f0, t.n_f1, t.n_q, t.n_decay, t.n_lvl),
                (
                    WOOD_MALLET_HZ0,
                    WOOD_MALLET_HZ1,
                    WOOD_MALLET_Q,
                    WOOD_MALLET_TAU_S,
                    MALLET_LVL
                ),
                "`{ch}`: the tok"
            );
            assert!(
                plain.iter().any(|v| v.lane == LANE_TUNE),
                "the twin's key is a step too"
            );
            // THE BLOOM rides the walk's lit-ness, as the twin's does, on the
            // un-shortened step τ.
            let blooms: Vec<&Voice> = spawned.iter().filter(|v| v.lane == LANE_BLOOM).collect();
            let plain_blooms: Vec<&Voice> = plain.iter().filter(|v| v.lane == LANE_BLOOM).collect();
            assert_eq!(
                blooms.len(),
                plain_blooms.len(),
                "`{ch}`: {} blooms against the twin's {}",
                blooms.len(),
                plain_blooms.len()
            );
            assert!(blooms.len() <= 1, "`{ch}` bloomed twice");
            if let (Some(b), Some(pb)) = (blooms.first(), plain_blooms.first()) {
                bloomed += 1;
                assert_eq!(b.decay, pb.decay, "`{ch}`: the bloom took the blip's τ");
            }
            // The count.
            let glints: Vec<&Voice> = spawned.iter().filter(|v| v.lane == LANE_GLINT).collect();
            assert_eq!(glints.len(), 1, "`{ch}` carries {} sparkles", glints.len());
            let gl = glints[0];
            assert!(
                (gl.p[0].f0 - COUNT_HZ[d % 5]).abs() < 0.5,
                "`{ch}` sparkled at {} Hz, not the counting degree's {}",
                gl.p[0].f0,
                COUNT_HZ[d % 5]
            );
            assert_eq!(
                gl.delay, KEY_GLINT_DELAY_S,
                "`{ch}`: the count arrives with the bloom"
            );
            // The level, with the seeded velocity divided out through the key's
            // own TUNE gain (`VOL × KEY_TINE_TRIM × level × g_IOI × vel`, the
            // bloom saying which level the walk gave the key): the sparkle
            // shares the strike's draw, so the quotient is the table's
            // constant. The twin cannot lend its draws: the mallet noise draws
            // per sample while it sounds, and the bar's 4 ms tok stops drawing
            // before the felt's 6 ms does.
            let level = if blooms.is_empty() {
                PASSING_LEVEL
            } else {
                1.0
            };
            let vel = mag(t) / (VOL * KEY_TINE_TRIM * level * g_ioi(s.v2.ioi_ms * 0.001));
            let mul = if d < 5 {
                COUNT_GLINT_LOW_MUL
            } else {
                KEY_GLINT_SHIFTED_MUL
            };
            let g = g_ioi(s.v2.ioi_ms * 0.001);
            let want = GLINT_LEVEL * KEY_GLINT_LEVEL_MUL * mul * g;
            let got = mag(gl) / (VOL * KEY_TINE_TRIM * vel);
            assert!(
                (got / want - 1.0).abs() < 1e-3,
                "`{ch}`: the count sparkle is {got} re the key against the table's {want} (×{mul})"
            );
            count_levels.push(got / g);
            let _ = render_mono(&mut s, 12);
            let _ = render_mono(&mut twin, 12);
        }
        assert!(
            bloomed > 0,
            "no digit was lit — the walk never landed on a chord tone"
        );
        assert_eq!(s.v2.glint_k, k0, "a digit advanced the sparkle rotation");
        assert_eq!(walks, twin_walks, "the class moved a degree");
        assert!(
            (count_levels[7] / count_levels[2] - 2.0).abs() < 1e-3,
            "`7` sparkles at {} against `2`'s {} — not twice (velocity divided out)",
            count_levels[7],
            count_levels[2]
        );
    }

    /// A shifted key does not bend, does not jump an octave, keeps its family's
    /// timbre, and only a capital that opens something rings. Glyph for glyph:
    /// the struck `f0` is the line's degree exactly, `glide == 0` on every
    /// partial, every voice the key spawned is under a real filter
    /// (`lp_cut` < 7639 Hz, where the one-pole saturates), and the ring belongs
    /// to the capital letter alone: a shifted digit, bang or bracket owns none,
    /// and none takes flow's echo in its place.
    #[test]
    fn a_shifted_key_does_not_bend() {
        for ch in ['A', '7', '!', '('] {
            let mut s = synth();
            let class = crate::trail_sound::typed_glyph_class(Some(ch));
            let mark = s.born_seq;
            s.push_meta(
                event(SoundKind::Typed, 0.0, true),
                EventMeta {
                    at_ms: 1_000,
                    rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                    glyph_class: class,
                    flow: 1.0,
                    ..EventMeta::default()
                },
            );
            let lead = s.voices[usize::from(s.v2.lead.expect("one strike").0)];
            let f = penta(TINE_BASE_HZ, i32::from(s.v2.walk()) + i32::from(s.song_key));
            assert_eq!(lead.p[0].f0, f, "{ch}: the strike is the LINE's degree");
            // Each family's own table: the bar's {1, 3, 5}, the pluck's {1, 4}
            // with its third partial muted, the tine's {1, 2, 2.76}.
            let ratios = if class == DIGIT {
                [1.0, WOOD_P2_RATIO, WOOD_P3_RATIO]
            } else if pluck_class(class) {
                [1.0, PLUCK_P2_RATIO, P3_RATIO]
            } else {
                [1.0, P2_RATIO, P3_RATIO]
            };
            for (partial, ratio) in lead.p.iter().zip(ratios) {
                assert_eq!(partial.glide, 0.0, "{ch}: a hammer does not bend");
                if partial.lvl > 0.0 {
                    assert_eq!(partial.f0, f * ratio, "{ch}: preserve the timbre");
                }
            }
            if class == DIGIT {
                assert_eq!((lead.n_f0, lead.n_f1), (WOOD_MALLET_HZ0, WOOD_MALLET_HZ1));
            } else if pluck_class(class) {
                assert_eq!((lead.p[1].lvl, lead.p[2].lvl), (PLUCK_P2_LVL, 0.0));
                assert_eq!((lead.n_f0, lead.n_f1), (PLUCK_HZ0, PLUCK_HZ1));
            } else {
                assert_eq!((lead.n_f0, lead.n_f1), (MALLET_HZ0, MALLET_HZ1));
            }
            for v in since(&s, mark) {
                assert_eq!(
                    v.p.map(|p| p.glide),
                    [0.0; 3],
                    "{ch}: nothing it spawns bends"
                );
                // Every voice, the bloom and the sparkle too
                // ([`shifted_roof`]).
                assert!(
                    v.lp_cut < 7_639.0,
                    "{ch}: a lane-{} voice is under a {} Hz roof — a bypass, not a filter",
                    v.lane,
                    v.lp_cut
                );
            }
            if ch == 'A' {
                // Not vacuous: the capital spawned a sparkle under the clamp,
                // and the same key unshifted keeps the sparkle's own roof
                // exactly.
                let glint_roof = |s: &TrailSynth, mark: u32| {
                    since(s, mark)
                        .iter()
                        .find(|v| v.lane == LANE_GLINT)
                        .map(|v| v.lp_cut)
                };
                assert_eq!(glint_roof(&s, mark), Some(ROOF_MAX_HZ));
                let mut plain = synth();
                let plain_mark = plain.born_seq;
                push_ch(&mut plain, SoundKind::Typed, 1_000, 'a');
                assert_eq!(glint_roof(&plain, plain_mark), Some(GLINT_LP_HZ));
                let (slot, born) =
                    s.v2.ring
                        .expect("a capital that opens a word owns its ring");
                let ring = s.voices[usize::from(slot)];
                assert_eq!(ring.born, born);
                assert_eq!(ring.lane, LANE_GRAFT);
                // The ring is the twelfth ([`CAP_RING_LIFT_DEG`]); the strike
                // above is `f`.
                assert_eq!(
                    ring.p[0].f0,
                    penta(
                        TINE_BASE_HZ,
                        i32::from(s.v2.walk())
                            + i32::from(s.song_key)
                            + FLOW_ECHO_OCTAVE_DEG
                            + CAP_RING_LIFT_DEG
                    )
                );
                assert_eq!(ring.p[2].lvl, 0.0, "a resonance is not struck");
                assert_eq!(ring.n_lvl, 0.0);
                assert_eq!(ring.delay, CAP_RING_DELAY_S);
                assert_eq!(ring.attack, CAP_RING_ATTACK_S);
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
        // The key that ends a phrase rest: the loop above types into fresh
        // synths, so `plan.rest` is never set and the air cloud never spawns.
        // A capital after a pause (the start of a sentence) must keep the room's
        // taps under a real filter too; the taps are asserted present so the
        // check cannot go vacuous.
        for ch in ['W', '7', '!', '('] {
            let mut s = synth();
            let mut at = 1_000;
            for c in "hello".chars() {
                push_ch(&mut s, SoundKind::Typed, at, c);
                at += 150;
            }
            push_ch(&mut s, SoundKind::Space, at, ' ');
            let _ = render_mono(&mut s, 200);
            let mark = s.born_seq;
            s.push_meta(
                event(SoundKind::Typed, 0.0, true),
                EventMeta {
                    at_ms: 5_000,
                    rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                    glyph_class: crate::trail_sound::typed_glyph_class(Some(ch)),
                    flow: 1.0,
                    ..EventMeta::default()
                },
            );
            let spawned = since(&s, mark);
            let taps = spawned
                .iter()
                .filter(|v| v.lane == LANE_BLOOM && AIR_TAP_DELAY_S.contains(&v.delay))
                .count();
            if ch == 'W' {
                assert_eq!(
                    taps,
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
        // The same take unshifted keeps the room's own roof, to the bit: the
        // clamp is the shifted key's alone, and the goldens' room is untouched.
        let mut plain = synth();
        let mut at = 1_000;
        for c in "hello".chars() {
            push_ch(&mut plain, SoundKind::Typed, at, c);
            at += 150;
        }
        push_ch(&mut plain, SoundKind::Space, at, ' ');
        let _ = render_mono(&mut plain, 200);
        let mark = plain.born_seq;
        push_ch(&mut plain, SoundKind::Typed, 5_000, 'w');
        let open: Vec<f32> = since(&plain, mark)
            .iter()
            .filter(|v| v.lane == LANE_BLOOM && AIR_TAP_DELAY_S.contains(&v.delay))
            .map(|v| v.lp_cut)
            .collect();
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

    /// The class sentence: one key of every punctuation bucket, each landing on
    /// a step. The rank table ([`crate::trail_sound::typed_glyph_rank`]) folds
    /// families onto one rank (`!` `?` are 40, `- _ + =` 41, `( ) [ ] { } < >`
    /// 42), so a mark right after another of its family is a re-strike (no
    /// sparkle, no graft, §9.2). The sentence keeps a letter between marks that
    /// share a rank; `a_re_struck_mark_does_not_spark_or_graft_again` pins the
    /// other half.
    const MARK_SENTENCE: &str =
        "Hello, World! 123 (a test)? Yes! a-b c/d e=f \"q\" [x] y{z} a<b> @# m_n";

    /// Every punctuation bucket has its voice, and each one speaks (§10.4).
    /// Where the marks land is `punctuation_phrases_the_line`'s business; how
    /// the families sound against each other is
    /// `the_three_families_are_distinct_timbres_on_one_line`'s.
    ///
    /// [`MARK_SENTENCE`] at 8 cps: every key is one melody step and one TUNE
    /// voice, and every plucked class is the pluck (`[0.60, 0.18, 0]` on
    /// `{1f, 4f}`, the 4f on its 30 ms decay, no bloom) on 0.5 τ floored at
    /// 25 ms. Each bucket's gesture is measured where it lands:
    ///
    /// - `,` is plucked at [`PLUCK_NOISE_LVL`] on a falling 1400 → 500 Hz
    ///   fingertip and, steering, breathes the space's exhale 20 ms later
    ///   (900 → 380 Hz) at [`PAUSE_BREATH_MUL`];
    /// - `?` rises a fifth 60 ms on, −3 dB, a lone sine with no mallet;
    /// - `!` is the forte tine (a sforzando arrival), opens the lit roof by
    ///   class alone, blooms when lit, and winks twice (30 and 90 ms, equally
    ///   bright);
    /// - `(` is the full lit tine with its bloom plus a grace note one degree up
    ///   at 45 ms, −8 dB;
    /// - `)` is plucked, with the grace note one degree down;
    /// - `"` is plucked and winks twice, small (30 and 45 ms);
    /// - `-` ties ([`DASH_TAU_MUL`]; mid-word the engine's re-strike, no zip),
    ///   `_` zips down (3200 → 900 over 40 ms), `/` zips up;
    /// - `=` and `<` sound a swelling fifth 30 ms on, −6 dB;
    /// - `@` is just plucked: one sparkle, no graft.
    ///
    /// Every level is read as `√(gl² + gr²)`, exactly the gain handed to `spawn`
    /// ([`pan_gains`] is equal-power), so a graft's quotient against its key is
    /// the table's constant with velocity and loudness arc divided out.
    ///
    /// The sparkle multipliers are measured against the same key stamped as a
    /// `LETTER`: `vel` is drawn before the tine and the magnitude is
    /// pan-invariant, so the quotient is the multiplier even though a pluck's
    /// missing bloom shifts the draw stream.
    #[test]
    fn punctuation_is_voiced_and_each_bucket_speaks() {
        /// 8 cps: fast enough to be typing, slow enough that the 90 ms second
        /// wink and the 60 ms rise both land before the next key.
        const PERIOD_MS: u32 = 125;
        let mag = |v: &Voice| (v.gl * v.gl + v.gr * v.gr).sqrt();
        let lane = |vs: &[Voice], l: u8| -> Vec<Voice> {
            vs.iter().filter(|v| v.lane == l).copied().collect()
        };
        let mut s = synth();
        let mut at = 1_000u32;
        let mut keys = 0u32;
        // The first time each glyph is typed: what it spawned, the IOI it was
        // played at, the degree it sounded and whether it was a step.
        let mut first: Vec<(char, Vec<Voice>, f32, i8, bool)> = Vec::new();
        for ch in MARK_SENTENCE.chars() {
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            let mark = s.born_seq;
            push_ch(&mut s, kind, at, ch);
            at += PERIOD_MS;
            if ch != ' ' {
                keys += 1;
                let spawned = since(&s, mark);
                let tune = lane(&spawned, LANE_TUNE);
                assert_eq!(
                    tune.len(),
                    1,
                    "`{ch}` spawned {} TUNE voices — one key is one step",
                    tune.len()
                );
                let class = crate::trail_sound::typed_glyph_class(Some(ch));
                // (A felt key — the ladder's auto-repeat rung — has no pitch
                // and no class shaping at all; the sentence types none.)
                if pluck_class(class) {
                    let t = tune[0];
                    assert!(t.p[0].lvl > 0.0, "fixture: `{ch}` was a felt key");
                    // The pluck's table on every plucked key, step or
                    // re-strike: the second partial at 4f on its 30 ms decay,
                    // the third muted.
                    assert_eq!(
                        (t.p[1].f0, t.p[1].decay, t.p[2].lvl),
                        (t.p[0].f0 * PLUCK_P2_RATIO, PLUCK_P2_TAU_S, 0.0),
                        "`{ch}` was plucked with the octave or the strike still in it"
                    );
                    // Levels move on a step only: a re-struck mark keeps the
                    // re-strike ladder's 0.50 / 0.10 / 0 and its felt level, as
                    // a re-struck digit does.
                    let want = if s.v2.text.restrike == 0 {
                        ([PLUCK_P1_LVL, PLUCK_P2_LVL, 0.0], None)
                    } else {
                        ([P1_LVL, RESTRIKE_P2_LVL, 0.0], Some(RESTRIKE_MALLET_LVL))
                    };
                    assert_eq!(
                        [t.p[0].lvl, t.p[1].lvl, t.p[2].lvl],
                        want.0,
                        "`{ch}`: the pluck's levels (restrike {})",
                        s.v2.text.restrike
                    );
                    if let Some(felt) = want.1 {
                        assert_eq!(t.n_lvl, felt, "`{ch}`: a re-struck mark's felt");
                    }
                    assert!(
                        lane(&spawned, LANE_BLOOM).is_empty(),
                        "`{ch}` hung a bloom — a pluck does not hang"
                    );
                }
                if !first.iter().any(|r| r.0 == ch) {
                    first.push((
                        ch,
                        spawned,
                        s.v2.ioi_ms,
                        s.v2.walk(),
                        s.v2.text.restrike == 0,
                    ));
                }
            }
            // The audio clock keeps up with the script, so the lanes retire
            // between keys as they do on a host.
            let _ = render_mono(&mut s, 12);
        }
        assert_eq!(
            s.v2.steps(),
            keys,
            "the sentence stepped {} times for {keys} glyph keys",
            s.v2.steps()
        );
        let key = i32::from(s.song_key);
        let get = |ch: char| -> &(char, Vec<Voice>, f32, i8, bool) {
            let r = first
                .iter()
                .find(|r| r.0 == ch)
                .expect("the sentence types it");
            assert!(r.4, "fixture: `{ch}` landed on a re-strike, not a step");
            let t = lane(&r.1, LANE_TUNE)[0];
            let want = penta(TINE_BASE_HZ, i32::from(r.3) + key);
            assert!(
                (t.p[0].f0 - want).abs() < SAME_PITCH_HZ,
                "fixture: `{ch}` sounded {} Hz, not its walk's degree {}",
                t.p[0].f0,
                r.3
            );
            r
        };
        // -- STOP: the pluck, and the breath behind it ----------------------
        {
            let (ch, vs, ioi, _, _) = get(',');
            let t = lane(vs, LANE_TUNE)[0];
            assert_eq!(
                (t.n_lvl, t.n_f0, t.n_f1, t.n_q, t.n_decay),
                (
                    PLUCK_NOISE_LVL,
                    PLUCK_HZ0,
                    PLUCK_HZ1,
                    PLUCK_Q,
                    PLUCK_NOISE_TAU_S
                ),
                "`{ch}`: the fingertip"
            );
            assert_eq!(
                (
                    PLUCK_NOISE_LVL,
                    PLUCK_HZ0,
                    PLUCK_HZ1,
                    PLUCK_Q,
                    PLUCK_NOISE_TAU_S
                ),
                (0.5, 1400.0, 500.0, 0.9, 0.015),
                "the fingertip's numbers are the 2026-09-20 ruling's"
            );
            assert!(t.n_f0 > t.n_f1, "`{ch}`: the fingertip falls");
            let want = (tau_v_s(ioi * 0.001) * PLUCK_TAU_MUL).max(PLUCK_TAU_MIN_S);
            assert!(
                (t.decay - want).abs() < 1e-6,
                "`{ch}`: τ {} against the pluck's {want}",
                t.decay
            );
            let br = lane(vs, LANE_BREATH);
            assert_eq!(br.len(), 1, "`{ch}`: a stop breathes once");
            assert_eq!(
                br[0].delay, STOP_BREATH_DELAY_S,
                "`{ch}`: the breath is 20 ms behind the pluck"
            );
            assert_eq!(
                (br[0].n_f0, br[0].n_f1),
                (BREATH_HZ0, BREATH_HZ1),
                "`{ch}`: 900 -> 380 Hz, the space's own exhale"
            );
            assert!(
                lane(vs, LANE_GRAFT).is_empty(),
                "`{ch}`: a stop grafts no pitch"
            );
        }
        // -- QMARK: the rise -----------------------------------------------
        {
            let (ch, vs, _, walk, _) = get('?');
            let t = lane(vs, LANE_TUNE)[0];
            let g = lane(vs, LANE_GRAFT);
            assert_eq!(g.len(), 1, "`{ch}`: one rise");
            // A steered `?` stands on a D or a G, and from a D the rise is the
            // pure fourth to G, not the wolf fifth to A ([`QUEST_RISE_D_DEG`]).
            assert!(
                matches!(i32::from(*walk).rem_euclid(5), 1 | 3),
                "`{ch}` was steered to degree {walk}, which is neither D nor G"
            );
            let up = if (i32::from(*walk) + key).rem_euclid(5) == 1 {
                QUEST_RISE_D_DEG
            } else {
                QUEST_RISE_DEG
            };
            let want = penta(TINE_BASE_HZ, i32::from(*walk) + key + up);
            assert!(
                (g[0].p[0].f0 - want).abs() < SAME_PITCH_HZ,
                "`{ch}`: the rise is at {} Hz, not the pure interval above at {want}",
                g[0].p[0].f0
            );
            assert_eq!(g[0].delay, QUEST_RISE_DELAY_S, "`{ch}`: 60 ms behind");
            assert_eq!(g[0].n_lvl, 0.0, "`{ch}`: the rise has no mallet");
            assert_eq!(
                [g[0].p[1].lvl, g[0].p[2].lvl],
                [0.0, 0.0],
                "`{ch}`: the rise is a lone sine"
            );
            assert!(
                (mag(&g[0]) / (mag(&t) * QUEST_RISE_LEVEL) - 1.0).abs() < 1e-3,
                "`{ch}`: the rise is {} against the pluck's {} × {QUEST_RISE_LEVEL}",
                mag(&g[0]),
                mag(&t)
            );
        }
        // -- BANG: harder, lit by class, two winks -------------------------
        {
            let (ch, vs, ioi, _, _) = get('!');
            let t = lane(vs, LANE_TUNE)[0];
            // The bang is not a pluck: it is the letter's tine (octave at 2f,
            // the 2.76 strike, the felt mallet) struck forte.
            assert!(!pluck_class(BANG), "the bang is still in the pluck family");
            assert_eq!(
                (t.p[1].f0, t.p[2].f0, t.n_lvl, t.n_f0, t.n_f1),
                (
                    t.p[0].f0 * P2_RATIO,
                    t.p[0].f0 * P3_RATIO,
                    MALLET_LVL,
                    MALLET_HZ0,
                    MALLET_HZ1
                ),
                "`{ch}`: the bang is the letter's tine, not a pluck"
            );
            assert!(
                t.p[1].lvl > P2_LVL && t.p[2].lvl == P3_LVL,
                "`{ch}`: forte moved level into the octave ({}) and left the strike ({})",
                t.p[1].lvl,
                t.p[2].lvl
            );
            let cps = 1_000.0 / ioi;
            let lit = roof_hz(cps, true, 0.5, hue_arc(0.0), Touch::Step);
            let unlit = roof_hz(cps, false, 0.5, hue_arc(0.0), Touch::Step);
            assert!(lit > unlit, "fixture: the lit roof is the plain one here");
            // The bang is struck forte by class (force 1), and forte opens the
            // lit roof by [`FORTE_ROOF_ADD_HZ`] under [`ROOF_MAX_HZ`].
            let lit = (lit + FORTE_ROOF_ADD_HZ).min(ROOF_MAX_HZ);
            assert!(
                (t.lp_cut - lit).abs() < 1e-3,
                "`{ch}`: the roof is {} Hz, not the LIT {lit} its class opens, forte",
                t.lp_cut
            );
            assert_eq!(
                (t.p[0].fm_ratio, t.p[0].fm_tau),
                (FORTE_FM_RATIO, FORTE_FM_TAU_S),
                "`{ch}`: the hammer's hardness, by class"
            );
            let gl = lane(vs, LANE_GLINT);
            assert_eq!(gl.len(), 2, "`{ch}`: two winks");
            let mut delays = [gl[0].delay, gl[1].delay];
            delays.sort_by(f32::total_cmp);
            assert_eq!(
                delays,
                [KEY_GLINT_DELAY_S, BANG_GLINT2_DELAY_S],
                "`{ch}`: the winks land at 30 and 90 ms"
            );
            assert!(
                (mag(&gl[0]) / mag(&gl[1]) - 1.0).abs() < 1e-3,
                "`{ch}`: the second wink is not as bright as the first"
            );
        }
        // -- OPEN: the lit tine, its bloom and a grace note up -------------
        {
            let (ch, vs, _, walk, _) = get('(');
            let t = lane(vs, LANE_TUNE)[0];
            assert!(
                t.p[1].lvl > 0.0 && t.p[2].lvl > 0.0 && t.n_lvl == MALLET_LVL,
                "`{ch}`: an opening bracket is the lit tine, not a pluck"
            );
            assert_eq!(
                lane(vs, LANE_BLOOM).len(),
                1,
                "`{ch}`: it opens, so it keeps its bloom"
            );
            let g = lane(vs, LANE_GRAFT);
            assert_eq!(g.len(), 1, "`{ch}`: one grace note");
            let want = penta(TINE_BASE_HZ, reflect_deg(i32::from(*walk) + TINK_DEG) + key);
            assert!(
                (g[0].p[0].f0 - want).abs() < SAME_PITCH_HZ,
                "`{ch}`: the tink is at {} Hz, not one degree up at {want}",
                g[0].p[0].f0
            );
            assert_eq!(
                (g[0].delay, g[0].decay, g[0].dur),
                (TINK_DELAY_S, TINK_TAU_S, TINK_DUR_S),
                "`{ch}`: the tink's 45 ms delay, 35 ms τ and tail"
            );
            assert!(
                (mag(&g[0]) / (mag(&t) * TINK_LEVEL) - 1.0).abs() < 1e-3,
                "`{ch}`: the tink is {} against the key's {} × {TINK_LEVEL}",
                mag(&g[0]),
                mag(&t)
            );
        }
        // -- CLOSE: the pluck, and the grace note down ---------------------
        {
            let (ch, vs, _, walk, _) = get(')');
            let t = lane(vs, LANE_TUNE)[0];
            assert_eq!(
                [t.p[0].lvl, t.p[1].lvl, t.p[2].lvl],
                [PLUCK_P1_LVL, PLUCK_P2_LVL, 0.0],
                "`{ch}`: a closing bracket is plucked"
            );
            let g = lane(vs, LANE_GRAFT);
            assert_eq!(g.len(), 1, "`{ch}`: one grace note");
            let want = penta(TINE_BASE_HZ, reflect_deg(i32::from(*walk) - TINK_DEG) + key);
            assert!(
                (g[0].p[0].f0 - want).abs() < SAME_PITCH_HZ,
                "`{ch}`: the tink is at {} Hz, not one degree DOWN at {want}",
                g[0].p[0].f0
            );
        }
        // -- QUOTE: two little dots ----------------------------------------
        {
            let (ch, vs, _, _, _) = get('"');
            let t = lane(vs, LANE_TUNE)[0];
            assert_eq!(t.n_lvl, PLUCK_NOISE_LVL, "`{ch}`: a quote is plucked");
            let gl = lane(vs, LANE_GLINT);
            assert_eq!(gl.len(), 2, "`{ch}`: two dots");
            let mut delays = [gl[0].delay, gl[1].delay];
            delays.sort_by(f32::total_cmp);
            assert_eq!(
                delays,
                [KEY_GLINT_DELAY_S, QUOTE_GLINT2_DELAY_S],
                "`{ch}`: the dots land at 30 and 45 ms"
            );
            assert!(
                lane(vs, LANE_GRAFT).is_empty(),
                "`{ch}`: a quote grafts no pitch"
            );
        }
        // -- DASH: the tie --------------------------------------------------
        // Not `get`: a tie inside a word IS a re-strike, by design.
        {
            let r = first
                .iter()
                .find(|r| r.0 == '-')
                .expect("the sentence types it");
            let t = lane(&r.1, LANE_TUNE)[0];
            assert!(!r.4, "fixture: `a-b`'s dash is mid-word, so a re-strike");
            assert_eq!(
                (t.p[1].f0, t.n_f0, t.n_f1, t.n_glide),
                (t.p[0].f0 * P2_RATIO, MALLET_HZ0, MALLET_HZ1, MALLET_GLIDE_S),
                "`-`: a tie is the tine — no 4f, no zip"
            );
            let want = (tau_v_s(r.2 * 0.001) * RESTRIKE_TAU_MUL * DASH_TAU_MUL)
                .min(MARK_TAU_MAX_S)
                .max(tau_v_s(r.2 * 0.001) * RESTRIKE_TAU_MUL);
            assert!(
                (t.decay - want).abs() < 1e-6,
                "`-`: τ {} against the held re-strike's {want}",
                t.decay
            );
            assert!(lane(&r.1, LANE_GRAFT).is_empty(), "`-`: no graft");
        }
        // -- LINE and rise: the zips ---------------------------------------
        for (ch, down) in [('_', true), ('/', false)] {
            let (_, vs, _, _, _) = get(ch);
            let t = lane(vs, LANE_TUNE)[0];
            let (f0, f1) = if down {
                (ZIP_HZ_HI, ZIP_HZ_LO)
            } else {
                (ZIP_HZ_LO, ZIP_HZ_HI)
            };
            assert_eq!(
                (t.n_lvl, t.n_f0, t.n_f1, t.n_glide, t.n_decay),
                (ZIP_MALLET_LVL, f0, f1, ZIP_GLIDE_S, ZIP_TAU_S),
                "`{ch}`: the zip"
            );
            assert!(
                lane(vs, LANE_GRAFT).is_empty(),
                "`{ch}`: a zip is one voice, no graft"
            );
        }
        // -- MATH: the swelling fifth --------------------------------------
        for ch in ['=', '<'] {
            let (_, vs, _, walk, _) = get(ch);
            let t = lane(vs, LANE_TUNE)[0];
            assert_eq!(
                [t.p[0].lvl, t.p[1].lvl, t.p[2].lvl],
                [PLUCK_P1_LVL, PLUCK_P2_LVL, 0.0],
                "`{ch}`: an operator is plucked"
            );
            let g = lane(vs, LANE_GRAFT);
            assert_eq!(g.len(), 1, "`{ch}`: one fifth");
            let want = penta(TINE_BASE_HZ, i32::from(*walk) + key + FIFTH_DEG);
            assert!(
                (g[0].p[0].f0 - want).abs() < SAME_PITCH_HZ,
                "`{ch}`: the fifth is at {} Hz, not {want}",
                g[0].p[0].f0
            );
            assert_eq!(
                (g[0].delay, g[0].attack),
                (FIFTH_DELAY_S, BLOOM_ATTACK_S),
                "`{ch}`: the fifth opens 30 ms on, swelling"
            );
            assert!(
                (mag(&g[0]) / (mag(&t) * FIFTH_LEVEL) - 1.0).abs() < 1e-3,
                "`{ch}`: the fifth is {} against the pluck's {} × {FIFTH_LEVEL}",
                mag(&g[0]),
                mag(&t)
            );
        }
        // -- Sigil: a pluck and nothing else -------------------------------
        {
            let (ch, vs, _, _, _) = get('@');
            let t = lane(vs, LANE_TUNE)[0];
            assert_eq!(t.n_lvl, PLUCK_NOISE_LVL, "`{ch}`: a sigil is plucked");
            assert_eq!(lane(vs, LANE_GLINT).len(), 1, "`{ch}`: one sparkle");
            assert!(lane(vs, LANE_GRAFT).is_empty(), "`{ch}`: and no graft");
        }
        // -- The sparkle multipliers, against the same key as a LETTER -----
        let glint_mul = |ch: char| -> f32 {
            let once = |class: u8| -> f32 {
                let mut s = synth();
                for (i, c) in "hello ".chars().enumerate() {
                    let kind = if c == ' ' {
                        SoundKind::Space
                    } else {
                        SoundKind::Typed
                    };
                    push_ch(&mut s, kind, 1_000 + i as u32 * 150, c);
                }
                let mark = s.born_seq;
                s.push_meta(
                    event(SoundKind::Typed, 0.0, false),
                    EventMeta {
                        at_ms: 1_900,
                        glyph_class: class,
                        rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                        ..EventMeta::default()
                    },
                );
                let vs = since(&s, mark);
                let gl = lane(&vs, LANE_GLINT);
                assert!(!gl.is_empty(), "`{ch}` did not sparkle at all");
                mag(&gl[0])
            };
            once(crate::trail_sound::typed_glyph_class(Some(ch))) / once(LETTER)
        };
        for (ch, want) in [
            ('!', KEY_GLINT_SHIFTED_MUL),
            ('(', KEY_GLINT_SHIFTED_MUL),
            ('"', QUOTE_GLINT_MUL),
            ('@', 1.0),
            (',', 1.0),
        ] {
            let got = glint_mul(ch);
            assert!(
                (got / want - 1.0).abs() < 1e-3,
                "`{ch}` sparkles at ×{got} where the table says ×{want}"
            );
        }
        // -- The `plain` rung is still a bare tine --------------------------
        // With every stop out (§7 step 3) an unshifted mark spawns its TUNE
        // voice and, if it is a steering stop, the space's exhale, and nothing
        // else. Grafts and sparkles are behind `stops.bloom`; the breath is
        // the air the key moves, not a decoration.
        let mut p = synth();
        p.set_v2_timbre_stops(TimbreStops::PLAIN);
        for (i, ch) in ",?!()\"-/=<@".chars().enumerate() {
            let at = 1_000 + i as u32 * 1_000;
            // A letter between the marks, so none of them is a re-strike of
            // its rank-mate.
            push_ch(&mut p, SoundKind::Typed, at - 400, 'm');
            let _ = render_mono(&mut p, 40);
            let mark = p.born_seq;
            push_ch(&mut p, SoundKind::Typed, at, ch);
            let vs = since(&p, mark);
            let want = if stop_breathes(crate::trail_sound::typed_glyph_class(Some(ch))) {
                vec![LANE_TUNE, LANE_BREATH]
            } else {
                vec![LANE_TUNE]
            };
            assert_eq!(
                vs.iter().map(|v| v.lane).collect::<Vec<_>>(),
                want,
                "`{ch}` on the plain rung spawned more than the ladder's control"
            );
            let _ = render_mono(&mut p, 40);
        }
    }

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

    /// Punctuation phrases the line (the design's P1-P8, P14-P16, P21, P24).
    ///
    /// [`PHRASE_CORPUS`] × 3 seeds × {4, 10} cps, read off the melody's hooks.
    /// Every steering mark lands on its cadence class (`.` on C, `,` on D/G, `;`
    /// on E/A, `:` on G, `?` on a D/G above where it came from (8 holds), `!` on
    /// C/G) by a move of at most three degrees; `-` sounds the degree before
    /// it; `)` returns toward its `(`; the word behind `. ` does not start on
    /// the sentence's last note; and two full stops from the middle of the
    /// register do not close on the same C.
    ///
    /// Negative control (P21): the same keys with every class forced to
    /// `LETTER` play a different line with some full stop off C, so the landing
    /// is the class's doing. An echo-born mark (class 0) is that classless key:
    /// a letter's tine, no breath, nothing booked (P24).
    #[test]
    fn punctuation_phrases_the_line() {
        const SEEDS: [u32; 3] = [SEED, 0x5EED_1234, 0xCAFE_F00D];
        let mut census = [0usize; 8];
        // Which keys steer is the text's alone: one answer at every rate and on
        // every seed. The degrees between the marks depend on the hand as well
        // (`derive`'s step 2), so the design's P20 (one degree hash across
        // every cps and ±25 % jitter) does not hold even for letters-only text
        // and is not claimed here.
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
                let mut last_stop: Option<PhraseKey> = None;
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
                    // P16 — the key behind `. ` / `! ` is a word head.
                    if let Some(stop) = last_stop.take() {
                        assert_ne!(
                            k.deg, stop.deg,
                            "{what}: the sentence starts on the note the last one ended on"
                        );
                    }
                    if matches!(k.ch, '.' | '!') {
                        last_stop = Some(*k);
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

        // -- P15: the anti-drone, on the law itself -------------------------
        for a in [2, 3] {
            let first = cadence_target(MARK_PERIOD, a, -1);
            assert_eq!(first.rem_euclid(5), 0);
            for b in [2, 3] {
                let second = cadence_target(MARK_PERIOD, b, first);
                assert_eq!(second.rem_euclid(5), 0);
                assert_ne!(
                    second, first,
                    "two sentences from degrees {a} and {b} close on the same C"
                );
                assert!((second - b).abs() <= STEER_MAX_DEG);
            }
        }
        // …and the whole table is inside the register and the move bound,
        // from every degree, whatever the last cadence was.
        for mark in MARK_PERIOD..=MARK_COLON {
            for from in TUNE_DEG_LO..=TUNE_DEG_HI {
                for last in -1..=TUNE_DEG_HI {
                    let to = cadence_target(mark, from, last);
                    assert!(
                        (TUNE_DEG_LO..=TUNE_DEG_HI).contains(&to)
                            && (to - from).abs() <= STEER_MAX_DEG,
                        "mark {mark}: {from} -> {to} (last cadence {last})"
                    );
                }
            }
        }

        // -- P24: an echo-born mark is a letter ------------------------------
        let mut s = synth();
        let mut at = 1_000;
        let _ = type_phrase(&mut s, "the end", &mut at, 150, false);
        let mark = s.born_seq;
        let keys = type_phrase(&mut s, ".", &mut at, 150, true);
        let born = since(&s, mark);
        let t = tune_voices(&born)[0];
        assert!(!keys[0].steered && s.v2.pending_mark() == MARK_NONE);
        assert_eq!(
            (t.p[1].f0, t.n_f0, t.n_f1),
            (t.p[0].f0 * P2_RATIO, MALLET_HZ0, MALLET_HZ1),
            "an echo-born `.` is not the letter's tine"
        );
        assert!(born.iter().all(|v| v.lane != LANE_BREATH));
    }

    /// A sentence neither ends nor starts where the last one did, through the
    /// engine (the design's P15 and P16).
    ///
    /// - P16, lowercase heads: behind `. ` and `! ` the head of an uncapitalised
    ///   sentence is never the degree the previous sentence closed on. On
    ///   ordinary prose older laws already ensure this; the rule binds on a
    ///   head on the stop's own rank (`the end. ...`, `wow! !!`), a zero
    ///   stride under a doubled mark. Non-vacuous by a twin: at every head the
    ///   melody is copied, told no mark was said, and given the same key; that
    ///   copy lands on the cadence's note at every head. An unedited copy is
    ///   asserted to match the engine, so the twin is faithful.
    /// - P15, the store: two full stops both arriving from degree 2 or 3 (the
    ///   walk set by hand, the keys through the shipping seam) close on two
    ///   different Cs in all four orders, `!` included; a Backspace over the
    ///   second stop restores the first one's degree to the anti-drone, so the
    ///   retyped stop lands where it did.
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
        let (mut heads, mut moved) = (0usize, 0usize);
        for seed in [SEED, 0x5EED_1234, 0xCAFE_F00D] {
            for period in [250u32, 150, 100] {
                let (h, m) = heads_of(&mut TrailSynth::new(SR, seed), LOWER, period);
                (heads, moved) = (heads + h, moved + m);
            }
        }
        println!("P16: the head rule moved {moved} of {heads} single-spaced lowercase heads");
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
        println!("P16: the head rule moved {moved} of {heads} same-rank heads");
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

    /// An aside is sotto voce, and it ends ([`ASIDE_GAIN`], [`ASIDE_ROOF_MUL`],
    /// [`ASIDE_MAX_KEYS`]). Inside `( [ {` (a letter, a digit, a mark, a nested
    /// bracket, a close that leaves an outer aside open) the strike is −2 dB
    /// and the roof ×0.8 against a twin whose aside was closed by hand, on the
    /// same pitch. The opening bracket and the close that ends the aside are
    /// outside it (ratio exactly 1). The aside ends at the outer close, an
    /// Enter, a line feed, a kill, a phrase rest and the 32nd key inside it; a
    /// Backspace over any of those restores it.
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

    /// A quotation lifts and comes home; an apostrophe is neither
    /// ([`QUOTE_OPENS`], [`QUOTE_CLOSES`]). Every quote of a small corpus × 3
    /// seeds × two rates, against a twin: the melody copied before the key and
    /// given the same rank as a MATH key (a class no rule steers), whose degree
    /// is the derived one. A quote at a word head or behind an open, with none
    /// open, opens one (a chord tone at or above the derived degree) and
    /// sparkles twice; a quote while one is open closes it (the open's degree,
    /// by at most [`WORD_LEAP_MAX_DEG`]) with one sparkle; a mid-word quote
    /// with none open is an apostrophe: the derived degree and two sparkles. An
    /// Enter ends the quotation, and a Backspace over either quote restores it.
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
        println!(
            "quotes: {opens} opened ({lifted} lifted off the derived degree), {closes} closed, \
             {apostrophes} apostrophes"
        );
        assert_eq!(
            (opens, closes, apostrophes),
            (36, 36, 12),
            "fixture: the corpus"
        );
        assert!(lifted > 0, "fixture: no open quote ever had to move up");

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
    /// letter, `,` and the void `??` keep their C exactly, and pickup, bell and
    /// tonic dyad are the same voices either way.
    #[test]
    fn a_question_s_return_lands_on_the_g() {
        // → (the resolution's f0, every other voice the Return spawned).
        let enter = |text: &str, words: usize| -> (f32, Vec<(u8, f32)>) {
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
            let rest = born
                .iter()
                .filter(|v| !(v.lane == LANE_TUNE && v.delay > 0.0))
                .map(|v| (v.lane, v.p[0].f0))
                .collect();
            (res[0].p[0].f0, rest)
        };
        let n = CHORD_LOOP.len();
        let mut landed = Vec::new();
        for words in 0..n {
            let (home, voices) = enter("so it goes.", words);
            for text in ["is it so?", "is it so? ", "he said \"is it so?\""] {
                let (f, v) = enter(text, words);
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
                // Everything else the Return spawned matches the full stop's,
                // except the pickup (withheld after a `.`, the codetta) and
                // the room's taps, which answer the G.
                let mut rest = v.clone();
                rest.retain(|x| !voices.contains(x));
                assert_eq!(
                    rest.len(),
                    1 + AIR_TAP_DELAY_S.len(),
                    "{text:?}: more than the pickup and the room differ: {rest:?}"
                );
                assert!(
                    rest.iter()
                        .any(|x| x.0 == LANE_TUNE && x.1 == penta(TINE_BASE_HZ, CAD_PICKUP_DEG)),
                    "{text:?}: the pickup is not among them"
                );
                landed.push(f);
            }
            for text in [
                "so it goes.",
                "wow it goes!",
                "so it goes",
                "so it goes,",
                "is it so??",
            ] {
                let (f, _) = enter(text, words);
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
        println!("?⏎ lands on {:?} Hz", {
            let mut l = landed.clone();
            l.dedup();
            l
        });
    }

    /// A line of code is no louder than prose (the design's C1: code-script RMS
    /// minus lowercase-prose RMS ≤ +1.5 dB). The reel's two scripts
    /// (`keyboard_song_ab`'s `code` and `prose-lower`), typed as the product
    /// hears them (a bare Shift [`CAPITAL_SHIFT_LEAD_MS`] ahead of every shifted
    /// run, a keyed Return and its pause) at the reel's two rates, RMS over the
    /// bench's window (0.4 s in, to 1 s past the last key).
    ///
    /// Negative control: with the bell unducked ([`TING_DUCK_MARK`] off) and the
    /// aside off ([`ASIDE_GAIN`]), both of which quiet code, the take reads
    /// over the limit, so the pin still sees a bell arriving on a line with no
    /// brackets.
    #[test]
    fn a_line_of_code_is_no_louder_than_prose() {
        const CODE: &str = "let v = self.v2.walk(x[0], &y) + foo::bar(42); // ok\n\
                            if (a != b) { return f(a, b)?; }\n";
        const PROSE_LOWER: &str = "hello world is this it yes it is 314 roughly well known done\n";
        const C1_MAX_DB: f32 = 1.5;
        const SHIFT_LEAD_MS: u32 = 60;
        /// The bench's own measurement window: 0.4 s in, to 1 s past the
        /// last key's slot.
        const WINDOW_FROM_MS: u32 = 400;
        const WINDOW_PAST_MS: u32 = 1_000;
        let needs_shift = |c: char| c.is_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(c);
        // → the take's mean square.
        let take = |text: &str, cps: f32, unducked: bool| -> f64 {
            let mut s = synth();
            s.ting_unducked = unducked;
            s.aside_off = unducked;
            // (press time ms, the glyph; `\0` is the bare Shift).
            let mut cues: Vec<(u32, char)> = Vec::new();
            let (mut t, mut in_run) = (500.0f32, false);
            for ch in text.chars() {
                let shifted = needs_shift(ch);
                if shifted && !in_run {
                    cues.push((t as u32 - SHIFT_LEAD_MS, '\0'));
                }
                in_run = shifted;
                cues.push((t as u32, ch));
                t += 1000.0 / cps + if ch == '\n' { 350.0 } else { 0.0 };
            }
            cues.sort_by_key(|c| c.0);
            let (mut now, mut col, mut sum, mut n) = (0u32, 0u16, 0.0f64, 0usize);
            let mut run = |s: &mut TrailSynth, until: u32, now: &mut u32| {
                while *now + 10 <= until {
                    let x = render_mono(s, 1);
                    if *now >= WINDOW_FROM_MS {
                        sum += x.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>();
                        n += x.len();
                    }
                    *now += 10;
                }
            };
            for (at, ch) in cues {
                run(&mut s, at, &mut now);
                match ch {
                    '\0' => push(&mut s, SoundKind::Shift, at, 0.0, false),
                    ' ' => push_ch(&mut s, SoundKind::Space, at, ch),
                    '\n' => {
                        push(&mut s, SoundKind::Enter { cells: col }, at, 0.0, false);
                        col = 0;
                    }
                    _ => s.push_meta(
                        event(SoundKind::Typed, 0.0, needs_shift(ch)),
                        EventMeta {
                            at_ms: at,
                            glyph_class: crate::trail_sound::typed_glyph_class(Some(ch)),
                            rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                            ..EventMeta::default()
                        },
                    ),
                }
                col += 1;
            }
            run(&mut s, t as u32 + WINDOW_PAST_MS, &mut now);
            assert_eq!(s.steals(), 0, "{text:?} at {cps} cps stole a voice (C4)");
            sum / n as f64
        };
        let over = |cps: f32, unducked: bool| -> f32 {
            (10.0 * (take(CODE, cps, unducked) / take(PROSE_LOWER, cps, unducked)).log10()) as f32
        };
        for cps in [6.0f32, 10.0] {
            let db = over(cps, false);
            println!("C1 at {cps} cps: code is {db:+.2} dB re lowercase prose");
            assert!(
                db <= C1_MAX_DB,
                "at {cps} cps a line of code is {db:+.2} dB over lowercase prose (C1: \
                 {C1_MAX_DB})"
            );
        }
        let loud = over(10.0, true);
        println!("C1 at 10 cps, the bell unducked and the aside off: {loud:+.2} dB");
        assert!(
            loud > C1_MAX_DB,
            "negative control: with the bell at full level over every shifted mark the code \
             script is {loud:+.2} dB over prose — inside C1 too, so this pin cannot see the duck"
        );
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

    /// A mark confirms its cadence in the Space bass (the design's P9, P10,
    /// P13, P17, P18). From every position of the chord loop: `. ` and `! ` put
    /// the bass on C4 (the C4+G4 dyad), `, ` `: ` `? ` on G4, `; ` on A4, within
    /// 0.5 Hz; the index only advances (1..=8 steps, cyclically) to the next
    /// chord of that function; a letter and the void `...` take the ordinary
    /// `chord + 1`. The subject re-latches behind `. ` `! ` `? ` but not `, `. A
    /// Return behind a full stop is a codetta: one TUNE voice fewer (the
    /// pickup) with the seeded stream behind it unchanged.
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
            let enter = |s: &mut TrailSynth| -> (usize, Voice) {
                let mark = s.born_seq;
                push(s, SoundKind::Enter { cells: 30 }, at, 0.0, false);
                let tunes = tune_voices(&since(s, mark)).len();
                let mark = s.born_seq;
                push_ch(s, SoundKind::Typed, at + 400, 'n');
                (tunes, tune_voices(&since(s, mark))[0])
            };
            let (c_tunes, c_next) = enter(&mut closed);
            let (o_tunes, o_next) = enter(&mut open);
            assert_eq!(
                (o_tunes, c_tunes),
                (2, 1),
                "`{tail}`+Enter: the pickup and the resolution, against the resolution alone"
            );
            assert_eq!(
                (c_next.p[0].ph, c_next.p[1].ph, c_next.gl, c_next.gr),
                (o_next.p[0].ph, o_next.p[1].ph, o_next.gl, o_next.gr),
                "`{tail}`+Enter: the next key's seeded phase and velocity moved"
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
            // Only the steering dot breathes and rings; the rest are plucks.
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Space, at, ' ');
            let _ = since(&s, mark);
            assert_eq!(
                s.v2.chord(),
                (chord + 1) % CHORD_LOOP.len() as u8,
                "`{token} `: the Space behind a letter or a digit is an ordinary advance"
            );
        }
        // An unsteered dot is the pluck, breathless; the steering one is the
        // tine, and breathes once.
        let mut s = synth();
        let mut at = 1_000;
        let mut dots = Vec::new();
        for ch in "foo.bar.baz".chars() {
            let mark = s.born_seq;
            let k = type_phrase(&mut s, &ch.to_string(), &mut at, 110, false)[0];
            if ch == '.' {
                dots.push((k.steered, since(&s, mark)));
            }
        }
        let ratio = |vs: &[Voice]| {
            let t = tune_voices(vs)[0];
            t.p[1].f0 / t.p[0].f0
        };
        let breaths = |vs: &[Voice]| vs.iter().filter(|v| v.lane == LANE_BREATH).count();
        assert_eq!(
            (dots[0].0, ratio(&dots[0].1), breaths(&dots[0].1)),
            (true, P2_RATIO, 1)
        );
        assert_eq!(
            (dots[1].0, ratio(&dots[1].1), breaths(&dots[1].1)),
            (false, PLUCK_P2_RATIO, 0)
        );
    }

    /// Backspace un-sings a phrase (the design's P19; §10.4's undo law). For
    /// every class id (and an unnamed one, `17`), plain and shifted, "type it,
    /// delete it" leaves [`MelodyV2::frame`] unchanged; so does a Space. And
    /// `ab. c`, deleted back to `a` and retyped, plays the same degrees onto the
    /// same chord.
    ///
    /// `frame()` is everything the text decides and exactly what `push_undo`
    /// saves; the clock, tempo and autorepeat state describe the hand and are
    /// not undone (see `prev_gap`). [`SECRET`] is excluded: it moves no melody
    /// by design (`a_password_leaves_the_melody_where_it_was`).
    ///
    /// Negative control: a typed mark with no Backspace leaves a different
    /// frame.
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

    /// A full stop rings and a comma is a breath (the design's P22, P23): one
    /// key into a silent box over eight seeds.
    ///
    /// - P22: the steering `.` (a tine on ×[`CADENCE_STOP_TAU_MUL`] τ) takes at
    ///   least 1.6× as long to fall 20 dB as the steering `,` (a pluck on
    ///   ×[`PLUCK_TAU_MUL`]), on every seed.
    /// - P23: the comma's onset peak, mean over the seeds, is −2 ± 0.5 dB re a
    ///   letter's at the same IOI. A mean because the partials' drawn phases
    ///   spread single takes; the pluck's crest sits slightly under
    ///   [`PAUSE_MARK_GAIN`]'s −2.0 (two partials summing, not three).
    #[test]
    fn a_full_stop_rings_and_a_comma_is_a_breath() {
        /// P22, a floor.
        const STOP_OVER_COMMA_T20: f32 = 1.6;
        /// P23 (fit).
        const COMMA_RE_LETTER_DB: f32 = -2.0;
        const COMMA_RE_LETTER_TOL_DB: f32 = 0.5;
        let take = |seed: u32, ch: char| -> Vec<f32> {
            let mut s = TrailSynth::new(SR, seed);
            push_ch(&mut s, SoundKind::Typed, 1_000, ch);
            render_mono(&mut s, 60)
        };
        let peak_db = |x: &[f32]| 20.0 * x.iter().fold(0.0f32, |m, v| m.max(v.abs())).log10();
        let t20_ms = |x: &[f32]| -> f32 {
            let env: Vec<f32> = x.chunks(96).map(rms_of).collect();
            let (at, top) = env
                .iter()
                .enumerate()
                .fold((0, 0.0f32), |m, (i, e)| if *e > m.1 { (i, *e) } else { m });
            let n = env[at..]
                .iter()
                .position(|e| *e < 0.1 * top)
                .unwrap_or(env.len() - at);
            (at + n) as f32 * 2.0
        };
        let mut worst = f32::MAX;
        let mut comma_re_letter = Vec::new();
        for k in 0..8u32 {
            let seed = SEED ^ (k.wrapping_mul(0x9E37_79B9));
            let (stop, comma, letter) = (take(seed, '.'), take(seed, ','), take(seed, 'm'));
            worst = worst.min(t20_ms(&stop) / t20_ms(&comma));
            // The onset: the first 30 ms, before any decoration has opened.
            comma_re_letter.push(peak_db(&comma[..1_440]) - peak_db(&letter[..1_440]));
        }
        let mean = comma_re_letter.iter().sum::<f32>() / comma_re_letter.len() as f32;
        let (lo, hi) = comma_re_letter
            .iter()
            .fold((f32::MAX, f32::MIN), |m, d| (m.0.min(*d), m.1.max(*d)));
        println!(
            "phrasing audio: stop/comma T20 worst x{worst:.2}; comma re letter mean \
             {mean:+.2} dB ({lo:+.2} .. {hi:+.2})"
        );
        assert!(
            worst >= STOP_OVER_COMMA_T20,
            "P22: the full stop rings only x{worst:.2} the comma"
        );
        assert!(
            (mean - COMMA_RE_LETTER_DB).abs() <= COMMA_RE_LETTER_TOL_DB,
            "P23: the comma is {mean:+.2} dB re a letter, not {COMMA_RE_LETTER_DB} ± \
             {COMMA_RE_LETTER_TOL_DB}"
        );
    }

    /// The class ids route correctly. `,` `;` `:` have their own ids (`COMMA`,
    /// `SEMI`, `COLON`) apart from `STOP`, and `-` has `DASH` apart from `LINE`.
    /// On identical engine state and side-car but for the class id: a steering
    /// `.` is a tine landing on C, a steering `,` `;` `:` a pluck 2 dB down on
    /// its cadence degree, and `-` a tie on the tine. An unnamed id (`17`,
    /// `255`) is the letter, as the side-car's identity column says; `16` is
    /// [`SECRET`].
    ///
    /// Negative controls: the comparison tells a stop from a letter, a stop from
    /// a line, and a breathless stop (a wrong [`stop_breathes`]) from a
    /// breathing one, so "equal" is not its only answer.
    #[test]
    fn the_split_classes_are_told_apart_and_an_unnamed_id_is_still_the_letter() {
        // One take: three letters, the mark, the mark again (its re-strike), a
        // letter. Everything but `class` is identical across takes, the rank
        // included (`.`'s), so the walk is one walk.
        let rank = crate::trail_sound::typed_glyph_rank(Some('.'));
        // -> (every voice as text, the output's bits, the lanes, the first
        //     mark's own TUNE voice).
        let take = |class: u8, shifted: bool| -> (Vec<String>, Vec<u32>, Vec<u8>, Voice) {
            let mut s = synth();
            let mut out = Vec::new();
            let mut at = 1_000;
            for ch in "hel".chars() {
                push_ch(&mut s, SoundKind::Typed, at, ch);
                out.extend(render_mono(&mut s, 15));
                at += 150;
            }
            let mut born = Vec::new();
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
                // Read at the spawn, before a block can end a short voice.
                born.extend(since(&s, mark));
                out.extend(render_mono(&mut s, 15));
                at += 150;
            }
            // `Voice` derives `Debug` over every field and an `f32` prints
            // round-trip exact, so equal text is field-for-field equality (and
            // tells `-0.0` from `0.0`, which `==` would not).
            let voices = born.iter().map(|v| format!("{v:?}")).collect::<Vec<_>>();
            let lanes = born.iter().map(|v| v.lane).collect::<Vec<_>>();
            push_ch(&mut s, SoundKind::Typed, at, 'o');
            out.extend(render_mono(&mut s, 60));
            let tune = tune_voices(&born)[0];
            (
                voices,
                out.iter().map(|x| x.to_bits()).collect(),
                lanes,
                tune,
            )
        };
        for shifted in [false, true] {
            let stop = take(STOP, shifted);
            let line = take(LINE, shifted);
            let letter = take(LETTER, shifted);
            assert!(
                stop.0.len() >= 3,
                "fixture: a stop and its re-strike are at least two plucks and a breath, \
                 not {} voices",
                stop.0.len()
            );
            let ratio = |v: &Voice| v.p[1].f0 / v.p[0].f0;
            assert_eq!(
                (ratio(&stop.3), stop.3.p[0].f0),
                (P2_RATIO, penta(TINE_BASE_HZ, 0)),
                "STOP (shifted {shifted}): the steering full stop is a tine on C"
            );
            for (class, name) in [(COMMA, "COMMA"), (SEMI, "SEMI"), (COLON, "COLON")] {
                let got = take(class, shifted);
                assert_eq!(
                    ratio(&got.3),
                    PLUCK_P2_RATIO,
                    "{name} (shifted {shifted}) is not plucked"
                );
                assert!(
                    got.2.contains(&LANE_BREATH),
                    "{name} (shifted {shifted}): the steering mark did not breathe"
                );
                assert_ne!(
                    got.0, stop.0,
                    "{name} (shifted {shifted}) is still voiced as the full stop"
                );
            }
            let dash = take(DASH, shifted);
            assert_eq!(
                (ratio(&dash.3), dash.3.n_f0, dash.3.n_f1),
                (P2_RATIO, MALLET_HZ0, MALLET_HZ1),
                "DASH (shifted {shifted}) is a tie on the tine: no 4f, no zip"
            );
            assert_eq!(
                (ratio(&line.3), line.3.n_f0, line.3.n_f1),
                (PLUCK_P2_RATIO, ZIP_HZ_HI, ZIP_HZ_LO),
                "LINE (shifted {shifted}) still draws its line"
            );
            // An unnamed id lands on the letter path (the letter's tine with the
            // octave at 2f, no breath, no zip), and every unnamed id is one
            // path. Unshifted it is the letter exactly. Shifted it is not quite:
            // [`Forte::of`] reads "a capital" as `shifted && class == LETTER`,
            // so a shifted unnamed id gets a shifted mark's force (0.35,
            // [`MARK_SHIFT_GAIN`]) on the letter's tine. No host stamps such an
            // id; this pins what one gets. [`SECRET`] (16) is pinned by
            // `a_password_key_is_one_tone_whatever_it_was`.
            let first_unknown = take(17, shifted);
            for unknown in [17u8, 18, 200, 255] {
                let got = take(unknown, shifted);
                assert_eq!(got.0, first_unknown.0, "class {unknown}: a path of its own");
                assert!(
                    got.1 == first_unknown.1,
                    "class {unknown}: an output of its own"
                );
                assert!(
                    !got.2.contains(&LANE_BREATH),
                    "class {unknown} (shifted {shifted}) breathed"
                );
                let (tune, want) = (got.3, letter.3);
                assert_eq!(
                    (tune.p[1].f0 / tune.p[0].f0, tune.n_f0, tune.n_f1),
                    (want.p[1].f0 / want.p[0].f0, want.n_f0, want.n_f1),
                    "class {unknown} (shifted {shifted}) is not on the letter's tine"
                );
                if !shifted {
                    assert_eq!(got.0, letter.0, "class {unknown} is not the letter");
                    assert!(
                        got.1 == letter.1,
                        "class {unknown}: not the letter's output"
                    );
                }
            }
            // Negative controls: the comparison can say "different".
            assert_ne!(stop.0, letter.0, "a stop compared equal to a letter");
            assert_ne!(stop.0, line.0, "a stop compared equal to a line");
            assert_ne!(dash.0, stop.0, "a dash compared equal to a stop");
            assert!(stop.1 != line.1, "a stop RENDERED equal to a line");
            // A comma that plucks but lost its breath, or a dash that gained
            // one, is a difference it sees: the breath is among the voices
            // compared.
            assert!(
                stop.2.contains(&LANE_BREATH) && !line.2.contains(&LANE_BREATH),
                "fixture: the breath is not among the voices compared"
            );
        }
        // And the routing is the split's, glyph by glyph: what the keyboard
        // stamps for the five marks lands on the predicates the engine reads.
        for ch in ['.', ',', ';', ':'] {
            let class = crate::trail_sound::typed_glyph_class(Some(ch));
            assert!(pluck_class(class) && stop_breathes(class), "`{ch}`");
        }
        for ch in ['_', '~', '\\', '|'] {
            let class = crate::trail_sound::typed_glyph_class(Some(ch));
            assert!(pluck_class(class) && !stop_breathes(class), "`{ch}`");
            assert_eq!(class, LINE, "`{ch}` does not draw a line");
        }
        let dash = crate::trail_sound::typed_glyph_class(Some('-'));
        assert!(dash == DASH && !pluck_class(dash) && !stop_breathes(dash));
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

    /// A password key is one tone, whatever it was. At a canonical no-echo
    /// prompt the host stamps [`SECRET`] on the keyed cue. On one engine state,
    /// one secret cue per key of [`SECRET_KEYS`]: every one spawns the same
    /// single voice (field for field: one plain tine, no forte, on the password
    /// degree, centre pan) and renders the same output bit for bit, whatever
    /// kind, shift, rank or pan it carried (owner, 2026-09-27).
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
        let mut s = synth();
        let at = prose_prelude(&mut s);
        let mark = s.born_seq;
        push_secret(&mut s, at, 'Q', 0.3);
        let v = since(&s, mark)[0];
        let song_key = i32::from(s.song_key);
        assert_eq!(v.lane, LANE_TUNE);
        assert_eq!(v.p[0].f0, penta(TINE_BASE_HZ, SECRET_DEG + song_key));
        assert_eq!(
            (v.p[1].f0 / v.p[0].f0, v.p[2].f0 / v.p[0].f0),
            (P2_RATIO, P3_RATIO),
            "the tine's own partials: no wood bar, no pluck"
        );
        assert_eq!(v.p[0].fm_ratio, 0.0, "no forte");
        assert_eq!(v.lp_cut, ROOF_PLAIN_LO_HZ, "the plain roof");
        assert_eq!(v.decay, SECRET_TAU_S);
        assert!(s.v2.ring.is_none() && s.v2.graft.is_none());
        assert_eq!(v.gl, v.gr, "centre pan");
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

    /// A password leaves the melody where it was. Every secret key sounds, and
    /// none moves [`MelodyV2`]: after a whole password the line's state (walk,
    /// steps, chord, undo frames, marks, IOI, flow latch, voice addresses) is
    /// field for field the state before it, and the next ordinary key derives
    /// as if the password was never typed. The seeded stream is the one thing a
    /// secret advances (its tine's four draws), so the comparison hands the
    /// password take the untouched take's stream.
    ///
    /// Negative control: the same keys typed unstamped do move the line.
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

    /// Letters, numbers and symbols are three timbres on one line (the design's
    /// D1-D13). Matching is the walk, the lattice and `song_key`, which no
    /// family touches; distinct is the partial table, note length and noise
    /// colour:
    ///
    /// | family | partials (Σ 0.78) | note τ | noise |
    /// |---|---|---|---|
    /// | letter — the tine | 1f 0.50, 2f 0.16, 2.76f 0.12 | ×1.0 | felt 0.45, 1.8 → 6.4 kHz |
    /// | digit — the wood bar | 1f 0.44, 3f 0.26, 5f 0.08 | ×0.7 | tok 0.45, 1.4 → 3.6 kHz |
    /// | symbol — the pluck | 1f 0.60, 4f 0.18 | ×0.5, floor 25 ms | fingertip 0.5, 1.4 → 0.5 kHz |
    ///
    /// The rendered half uses the reshapers directly: one [`tine`] per family on
    /// one pitch, roof, τ and gain, handed to [`wood`] or [`pluck`] and spawned
    /// alone into a silent synth, so two takes differ only in the family. Three
    /// degrees (floor, middle, ceiling), lit and unlit roof, 4 / 8 / 10 cps,
    /// three seeds. The structural half (D7, D10) drives the typed path.
    ///
    /// Negative control: a dead-wood knock (`[0.50, 0, 0]`, noise 1.4, τ ×0.45
    /// floored at 20 ms) rebuilt here. It passes the design's D8 (tonality ≥ 60)
    /// and D9 (noise-to-tone ≤ −6 dB) as drawn, so those are pinned where they
    /// discriminate: tonality ≥ 600 and at least ×2.5 the knock on the same
    /// pitch and seed; noise-to-tone ≤ −22 dB. D11 and D12 are pinned at the
    /// measured values, as the constants state.
    #[test]
    fn the_three_families_are_distinct_timbres_on_one_line() {
        const SEEDS: [u32; 3] = [SEED, 0x5EED_1234, 0xCAFE_F00D];
        const IOIS_S: [f32; 3] = [0.25, 0.125, 0.1];
        /// D3 / D4 / D5, dB.
        const DIGIT_3F_OVER_2F_DB: f64 = 10.0;
        const SYMBOL_4F_OVER_DB: f64 = 10.0;
        const LETTER_2F_OVER_3F_DB: f64 = 6.0;
        /// D6: the three families' measured pitch, cents apart.
        const PITCH_SPREAD_CENTS: f32 = 5.0;
        /// D8 (see the doc): the design's 60 is kept as the floor it is, and
        /// the pin that tells the pluck from the knock is 600.
        const TONALITY_FLOOR: f64 = 600.0;
        const TONALITY_OVER_KNOCK: f64 = 2.5;
        /// D9 (see the doc), dB.
        const NOISE_TO_TONE_CEIL_DB: f32 = -22.0;
        /// D11: time to −20 dB, digit over symbol. The structural ratio is
        /// 0.7 / 0.5 = 1.4, but the bar's loud 3f is gone in 45 ms and takes the
        /// early envelope with it, so the pin sits under the measured ×1.24;
        /// the order letter > digit > symbol holds on every take.
        const DIGIT_OVER_SYMBOL_T20: f32 = 1.2;
        /// D12: onset peak re the letter's, dB, as the mean over three seeds
        /// (a single take is a lottery of seeded phases). Measured:
        ///
        /// ```text
        ///            4 cps            8 cps            10 cps
        /// digit    −1.23 … −0.86    −1.28 … −0.97    −1.29 … −1.05
        /// symbol   −0.46 … −0.30    −1.17 … −0.71    −1.30 … −0.84
        /// ```
        ///
        /// The digit sits slightly outside ±1.0: its sum is conserved, but its
        /// upper partials sit nearer the roof and its τ is ×0.7, so the 4 ms
        /// attack spends more before the crest. Every reading is under the
        /// letter (§9.6's lawful direction), so the pin is the measurement plus
        /// a tenth, and no family is ever louder than the letter.
        const PEAK_REF_IOI_DB: f32 = 1.35;
        const PEAK_UNDER_FLOOR_DB: f32 = -1.45;
        const PEAK_OVER_CEIL_DB: f32 = 0.0;

        // -- D2: the sums, as arithmetic --------------------------------------
        let letter_sum = P1_LVL + P2_LVL + P3_LVL;
        assert!((letter_sum - 0.78).abs() < 1e-6);
        assert!((WOOD_P1_LVL + WOOD_P2_LVL + WOOD_P3_LVL - 0.78).abs() < 1e-6);
        assert!((PLUCK_P1_LVL + PLUCK_P2_LVL - 0.78).abs() < 1e-6);
        assert_eq!(
            (
                WOOD_P2_LVL,
                PLUCK_P1_LVL,
                PLUCK_P2_LVL,
                PLUCK_P2_RATIO,
                PLUCK_P2_TAU_S
            ),
            (0.26, 0.60, 0.18, 4.0, 0.030),
            "the 2026-09-20 ruling's numbers"
        );
        assert_eq!(
            (
                PLUCK_TAU_MUL,
                PLUCK_TAU_MIN_S,
                PLUCK_NOISE_LVL,
                ZIP_MALLET_LVL
            ),
            (0.5, 0.025, 0.5, 0.5)
        );

        let gain = VOL * KEY_TINE_TRIM;
        // The dead-wood knock negative control.
        let old_knock = |f: f32, tau: f32, roof: f32| -> Voice {
            let mut v = tine(f, Touch::Step, (tau * 0.45).max(0.020), roof, false, 0.0);
            v.p[1].lvl = 0.0;
            v.p[2].lvl = 0.0;
            v.n_lvl = 1.4;
            v.n_f0 = 1_400.0;
            v.n_f1 = 500.0;
            v.n_q = 0.9;
            v.n_decay = 0.015;
            v
        };
        let render = |seed: u32, v: Voice| -> Vec<f32> {
            let mut s = TrailSynth::new(SR, seed);
            assert!(
                s.v2_spawn(v, gain, 0.0).is_some(),
                "an empty pool admits one voice"
            );
            render_mono(&mut s, 40)
        };
        let peak_db = |x: &[f32]| 20.0 * x.iter().fold(0.0f32, |m, v| m.max(v.abs())).log10();
        // Energy within ±3 % of `hz`, dB, over the first 2048 samples.
        let band = |pw: &[f64], hz: f32| -> f64 {
            let (lo, hi) = (bin_of(hz * 0.97, 2_048), bin_of(hz * 1.03, 2_048) + 1);
            10.0 * pw[lo..=hi].iter().sum::<f64>().max(1e-30).log10()
        };
        // The bench's `tonality`, number for number (`keyboard_song_ab`).
        let tonality = |x: &[f32]| -> f64 {
            let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            let start = x.iter().position(|v| v.abs() > peak * 0.05).unwrap_or(0);
            let pw = fft_powers(&x[start..start + 2_048]);
            let mut b: Vec<f64> = pw[bin_of(150.0, 2_048)..=bin_of(4_500.0, 2_048)]
                .iter()
                .map(|p| p.sqrt())
                .collect();
            let top = b.iter().copied().fold(0.0, f64::max);
            b.sort_by(f64::total_cmp);
            top / b[b.len() / 2].max(1e-12)
        };
        // The noise alone over the tone alone, RMS over 0-30 ms, dB.
        let noise_to_tone = |seed: u32, v: Voice| -> f32 {
            let mut tone = v;
            tone.n_lvl = 0.0;
            let mut noise = v;
            for p in &mut noise.p {
                p.lvl = 0.0;
            }
            20.0 * (rms_of(&render(seed, noise)[..1_440]) / rms_of(&render(seed, tone)[..1_440]))
                .log10()
        };
        // The forte pin's clock: from the KEY to the first 2 ms that is 20 dB
        // under the loudest 2 ms.
        let t20_ms = |x: &[f32]| -> f32 {
            let env: Vec<f32> = x.chunks(96).map(rms_of).collect();
            let (at, top) = env
                .iter()
                .enumerate()
                .fold((0, 0.0f32), |m, (i, e)| if *e > m.1 { (i, *e) } else { m });
            let n = env[at..]
                .iter()
                .position(|e| *e < 0.1 * top)
                .unwrap_or(env.len() - at);
            (at + n) as f32 * 2.0
        };
        // [`pitch_hz`] on the fundamental alone: four one-pole passes at 1.3f
        // remove the 2.76f strike from the letter's autocorrelation, which
        // otherwise reads 8-14 cents flat.
        let cents_off = |x: &[f32], f: f32| -> f32 {
            let mut y = x.to_vec();
            let k = 1.0 - (-core::f32::consts::TAU * 1.3 * f / SR).exp();
            for _ in 0..4 {
                let mut prev = 0.0f32;
                for v in &mut y {
                    prev += k * (*v - prev);
                    *v = prev;
                }
            }
            1_200.0 * (pitch_hz(&y[240..4_080], f * 0.75, f * 1.333) / f).log2()
        };

        let (mut d3, mut d4, mut d5) = (f64::MAX, f64::MAX, f64::MAX);
        let (mut d6, mut d8, mut d8_ratio, mut d8_knock) = (0.0f32, f64::MAX, f64::MAX, 0.0f64);
        let (mut d9, mut d9_knock) = (f32::MIN, f32::MAX);
        let (mut d11, mut d12_lo, mut d12_hi, mut d12_ref) = (f32::MAX, f32::MAX, f32::MIN, 0.0f32);
        for ioi in IOIS_S {
            let tau = tau_v_s(ioi);
            for lit in [false, true] {
                let roof = roof_hz(1.0 / ioi, lit, 0.5, hue_arc(0.0), Touch::Step);
                for deg in [TUNE_DEG_LO, 4, TUNE_DEG_HI] {
                    let f = penta(TINE_BASE_HZ, deg);
                    let letter = tine(f, Touch::Step, tau, roof, false, 0.0);
                    let mut digit = tine(f, Touch::Step, tau * DIGIT_TAU_MUL, roof, false, 0.0);
                    wood(&mut digit, f, Touch::Step);
                    let tau_p = (tau * PLUCK_TAU_MUL).max(PLUCK_TAU_MIN_S);
                    let mut symbol = tine(f, Touch::Step, tau_p, roof, false, 0.0);
                    pluck(&mut symbol, f, SIGIL, Touch::Step);
                    let knock = old_knock(f, tau, roof);
                    // -- D1 / D2, structural ---------------------------------
                    let ratios = |v: &Voice| -> Vec<f32> {
                        v.p.iter()
                            .filter(|p| p.lvl > 0.0)
                            .map(|p| p.f0 / f)
                            .collect()
                    };
                    // (To a part in a million: `f * 3.0 / f` is not 3.0 in
                    // f32 at every degree.)
                    let same = |got: Vec<f32>, want: &[f32]| {
                        got.len() == want.len()
                            && got
                                .iter()
                                .zip(want)
                                .all(|(g, w)| (g / w - 1.0).abs() < 1e-6)
                    };
                    assert!(same(ratios(&letter), &[1.0, 2.0, 2.76]), "D1: the tine");
                    assert!(same(ratios(&digit), &[1.0, 3.0, 5.0]), "D1: the bar");
                    assert!(same(ratios(&symbol), &[1.0, 4.0]), "D1: the pluck");
                    for v in [&letter, &digit, &symbol] {
                        let sum: f32 = v.p.iter().map(|p| p.lvl).sum();
                        assert!((sum - 0.78).abs() < 1e-6, "Σ p.lvl = {sum}");
                        assert_eq!(v.p[0].f0, f, "no family moves the fundamental");
                        assert_eq!(v.attack, TINE_ATTACK_S, "the 4 ms attack is law 1");
                        assert_eq!(v.lp_cut, roof);
                    }
                    assert_eq!(
                        (symbol.p[1].decay, symbol.n_lvl, symbol.n_f0, symbol.n_f1),
                        (PLUCK_P2_TAU_S, PLUCK_NOISE_LVL, PLUCK_HZ0, PLUCK_HZ1)
                    );
                    // A re-struck mark: frequencies and decays move, levels
                    // stay the ladder's.
                    let mut again = tine(f, Touch::ReStrike, tau_p, roof, false, 0.0);
                    pluck(&mut again, f, SIGIL, Touch::ReStrike);
                    assert_eq!(
                        (
                            again.p.map(|p| p.lvl),
                            again.n_lvl,
                            again.p[1].f0,
                            again.n_f0
                        ),
                        (
                            [P1_LVL, RESTRIKE_P2_LVL, 0.0],
                            RESTRIKE_MALLET_LVL,
                            f * PLUCK_P2_RATIO,
                            PLUCK_HZ0
                        )
                    );
                    // -- rendered --------------------------------------------
                    let mut peaks = [0.0f32; 3];
                    for seed in SEEDS {
                        let takes = [letter, digit, symbol].map(|v| render(seed, v));
                        let pw = [0, 1, 2].map(|i| fft_powers(&takes[i][..2_048]));
                        d5 = d5.min(band(&pw[0], 2.0 * f) - band(&pw[0], 3.0 * f));
                        d3 = d3.min(band(&pw[1], 3.0 * f) - band(&pw[1], 2.0 * f));
                        d4 = d4
                            .min(band(&pw[2], 4.0 * f) - band(&pw[2], 2.0 * f))
                            .min(band(&pw[2], 4.0 * f) - band(&pw[2], 3.0 * f));
                        let cents = [0, 1, 2].map(|i| cents_off(&takes[i], f));
                        let spread = cents.iter().copied().fold(f32::MIN, f32::max)
                            - cents.iter().copied().fold(f32::MAX, f32::min);
                        d6 = d6.max(spread);
                        let k = render(seed, knock);
                        let (ton, ton_k) = (tonality(&takes[2]), tonality(&k));
                        d8 = d8.min(ton);
                        d8_ratio = d8_ratio.min(ton / ton_k);
                        d8_knock = d8_knock.max(ton_k);
                        d9 = d9.max(noise_to_tone(seed, symbol));
                        d9_knock = d9_knock.min(noise_to_tone(seed, knock));
                        let t20 = [0, 1, 2].map(|i| t20_ms(&takes[i]));
                        assert!(
                            t20[0] > t20[1] && t20[1] > t20[2],
                            "ioi {ioi} deg {deg} seed {seed:#x}: letter > digit > symbol, \
                             got {t20:?} ms"
                        );
                        d11 = d11.min(t20[1] / t20[2]);
                        for (p, x) in peaks.iter_mut().zip(&takes) {
                            *p += peak_db(x) / SEEDS.len() as f32;
                        }
                    }
                    for fam in [1, 2] {
                        let re = peaks[fam] - peaks[0];
                        d12_lo = d12_lo.min(re);
                        d12_hi = d12_hi.max(re);
                        if ioi == 0.25 {
                            d12_ref = d12_ref.max(re.abs());
                        }
                    }
                }
            }
        }
        println!(
            "families: D3 {d3:+.1} D4 {d4:+.1} D5 {d5:+.1} dB | D6 {d6:.1} cents | D8 pluck \
             {d8:.0} (x{d8_ratio:.2} the knock's, whose best is {d8_knock:.0}) | D9 pluck \
             {d9:.1} knock {d9_knock:.1} dB | D11 x{d11:.2} | D12 {d12_lo:+.2} .. {d12_hi:+.2} dB \
             (ref IOI |{d12_ref:.2}|)"
        );
        assert!(
            d3 >= DIGIT_3F_OVER_2F_DB,
            "D3: the bar's 3f is {d3:+.1} dB over 2f"
        );
        assert!(
            d4 >= SYMBOL_4F_OVER_DB,
            "D4: the pluck's 4f is {d4:+.1} dB over 2f / 3f"
        );
        assert!(
            d5 >= LETTER_2F_OVER_3F_DB,
            "D5: the tine's 2f is {d5:+.1} dB over 3f"
        );
        assert!(
            d6 <= PITCH_SPREAD_CENTS,
            "D6: the families read {d6:.1} cents apart on one degree"
        );
        assert!(d8 >= TONALITY_FLOOR, "D8: the pluck's tonality is {d8:.0}");
        assert!(
            d8_ratio >= TONALITY_OVER_KNOCK,
            "D8: the pluck is only x{d8_ratio:.2} the knock's tonality"
        );
        assert!(
            d8_knock < TONALITY_FLOOR,
            "D8 negative control: the old knock reads {d8_knock:.0} and must FAIL the floor"
        );
        assert!(
            d9 <= NOISE_TO_TONE_CEIL_DB,
            "D9: the pluck's noise is {d9:.1} dB re its tone"
        );
        assert!(
            d9_knock > NOISE_TO_TONE_CEIL_DB,
            "D9 negative control: the old knock reads {d9_knock:.1} dB and must FAIL the ceiling"
        );
        assert!(
            d11 >= DIGIT_OVER_SYMBOL_T20,
            "D11: digit / symbol T20 is x{d11:.2}"
        );
        assert!(
            d12_ref <= PEAK_REF_IOI_DB,
            "D12: at the reference IOI a family's onset peak is {d12_ref:.2} dB off the letter's"
        );
        assert!(
            d12_lo >= PEAK_UNDER_FLOOR_DB && d12_hi <= PEAK_OVER_CEIL_DB,
            "D12: the families' onset peaks are {d12_lo:+.2} .. {d12_hi:+.2} dB re the letter's"
        );

        // -- D7 and D10, on the typed path ---------------------------------------
        // One synth per family: the same ranks at the same stamps, the class
        // forced (the digit pin's twin idiom), so the walk is one walk.
        for period_ms in [250u32, 125, 100] {
            let mut taus: Vec<[f32; 3]> = Vec::new();
            let mut synths = [synth(), synth(), synth()];
            for (n, ch) in "etaoinshr".chars().enumerate() {
                let at = 1_000 + n as u32 * period_ms;
                let mut row = [0.0f32; 3];
                let mut hz = [0.0f32; 3];
                for (i, class) in [LETTER, DIGIT, SIGIL].into_iter().enumerate() {
                    let s = &mut synths[i];
                    let mark = s.born_seq;
                    s.push_meta(
                        event(SoundKind::Typed, 0.0, false),
                        EventMeta {
                            at_ms: at,
                            glyph_class: class,
                            rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                            ..EventMeta::default()
                        },
                    );
                    let tune: Vec<Voice> = since(s, mark)
                        .into_iter()
                        .filter(|v| v.lane == LANE_TUNE)
                        .collect();
                    assert_eq!(tune.len(), 1);
                    let want = penta(TINE_BASE_HZ, i32::from(s.v2.walk()) + i32::from(s.song_key));
                    assert!(
                        (1_200.0 * (tune[0].p[0].f0 / want).log2()).abs() < 1.0,
                        "D7: class {class} sounded {} Hz, not its walk's {want}",
                        tune[0].p[0].f0
                    );
                    row[i] = tune[0].decay;
                    hz[i] = tune[0].p[0].f0;
                    let _ = render_mono(s, 6);
                }
                assert!(
                    hz[0] == hz[1] && hz[1] == hz[2],
                    "D6: one rank, one stamp, three classes — {hz:?} Hz"
                );
                // Steps only: a re-strike's τ carries the ladder's own 0.7.
                if synths.iter().all(|s| s.v2.text.restrike == 0) && n > 0 {
                    taus.push(row);
                }
            }
            assert!(!taus.is_empty(), "fixture: no step was measured");
            for [l, d, p] in taus {
                assert!(
                    (d / l - DIGIT_TAU_MUL).abs() < 1e-5,
                    "D10: digit τ {d} / {l}"
                );
                let want = (l * PLUCK_TAU_MUL).max(PLUCK_TAU_MIN_S);
                assert!((p - want).abs() < 1e-6, "D10: pluck τ {p}, want {want}");
            }
        }
        // The floor is reached: at the fastest τ_v the pluck is 25 ms, not 14.
        const { assert!(TAU_V_MIN_S * PLUCK_TAU_MUL < PLUCK_TAU_MIN_S) };

        // D7, the corpora the design names: every TUNE voice of the mark
        // sentence and of the ten digits is on its walk's lattice degree.
        for text in [MARK_SENTENCE, "0123456789"] {
            let mut s = synth();
            for (n, ch) in text.chars().enumerate() {
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                let mark = s.born_seq;
                push_ch(&mut s, kind, 1_000 + n as u32 * 125, ch);
                if ch != ' ' {
                    let want = penta(TINE_BASE_HZ, i32::from(s.v2.walk()) + i32::from(s.song_key));
                    for v in since(&s, mark).iter().filter(|v| v.lane == LANE_TUNE) {
                        assert!(
                            (1_200.0 * (v.p[0].f0 / want).log2()).abs() < 1.0,
                            "D7: `{ch}` sounded {} Hz against its walk's {want}",
                            v.p[0].f0
                        );
                    }
                }
                let _ = render_mono(&mut s, 12);
            }
        }

        // -- D13: a line of code at 10 cps -------------------------------------
        // The bench's `code` script: no steal, and the pool is never near full,
        // though code is mostly marks.
        let code = "let v = self.v2.walk(x[0], &y) + foo::bar(42); // ok\n\
                    if (a != b) { return f(a, b)?; }\n";
        let mut s = synth();
        let mut live = 0usize;
        for (n, ch) in code.chars().enumerate() {
            let kind = match ch {
                ' ' => SoundKind::Space,
                '\n' => SoundKind::Jump,
                _ => SoundKind::Typed,
            };
            s.push_meta(
                event(kind, 0.0, kind == SoundKind::Typed && bench_needs_shift(ch)),
                EventMeta {
                    at_ms: 1_000 + n as u32 * 100,
                    glyph_class: crate::trail_sound::typed_glyph_class(Some(ch)),
                    rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                    ..EventMeta::default()
                },
            );
            live = live.max(s.live_voices());
            let _ = render_mono(&mut s, 10);
        }
        println!(
            "families: code at 10 cps — {live} live voices at most, {} steals",
            s.steals()
        );
        assert_eq!(s.steals(), 0, "D13: a line of code stole a voice");
        assert!(live <= 25, "D13: {live} live voices on a line of code");
    }

    /// A re-struck mark does not spark or graft again (§9.2's re-strike
    /// ladder). `!!!!`, `....`, `((((` and `????` at 8 cps: the first key asks,
    /// opens or winks; each repeat is the same note further away, with no
    /// sparkle and no graft. The pluck shaping is not gated: a re-struck `.` is
    /// still plucked.
    ///
    /// The first dot of `....` is the token's steering mark (a tine that rings,
    /// with the one breath); the three behind it are a doubled mark: void
    /// ([`MARK_DOUBLED`]), plucked on the fading re-strike, breathless. `!` is
    /// the forte tine, so `!!!!`'s re-strikes are the letter's.
    ///
    /// Four presses on an exactly regular 125 ms become autorepeat
    /// ([`MelodyV2::detect_autorepeat`]): the felt mallet alone, class shaping
    /// skipped, still not silent (§3.1). The run crosses that rung on purpose
    /// and asserts which side each key is on.
    #[test]
    fn a_re_struck_mark_does_not_spark_or_graft_again() {
        const PERIOD_MS: u32 = 125;
        let lane = |vs: &[Voice], l: u8| -> Vec<Voice> {
            vs.iter().filter(|v| v.lane == l).copied().collect()
        };
        let mut felt_keys = 0usize;
        let mut restruck_plucks = 0usize;
        for run in ["!!!!", "....", "((((", "????"] {
            let mut s = synth();
            for (i, c) in "hello ".chars().enumerate() {
                let kind = if c == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(&mut s, kind, 1_000 + i as u32 * 150, c);
            }
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
                // A felt key (the ladder's auto-repeat rung) has no pitch at
                // all: the class shaping never ran.
                let felt = lane(&vs, LANE_TUNE)[0].p[0].lvl == 0.0;
                if felt {
                    felt_keys += 1;
                } else if class == STOP && i == 0 {
                    let t = lane(&vs, LANE_TUNE)[0];
                    assert_eq!(
                        (t.p[1].f0, t.p[2].lvl),
                        (t.p[0].f0 * P2_RATIO, P3_LVL),
                        "`{run}` key 0: the steering full stop is the tine"
                    );
                    assert_eq!(s.v2.pending_mark(), MARK_PERIOD);
                } else if pluck_class(class) {
                    // The pluck's table (4f on its 30 ms decay) on every touch,
                    // with the pluck's levels on the step and the ladder's own
                    // (0.50 / 0.10 / 0, felt 0.30) on every re-strike.
                    let t = lane(&vs, LANE_TUNE)[0];
                    assert_eq!(
                        (t.p[1].f0, t.p[1].decay),
                        (t.p[0].f0 * PLUCK_P2_RATIO, PLUCK_P2_TAU_S),
                        "`{run}` key {i}: a struck `{ch}` stopped being a pluck"
                    );
                    let want = if i == 0 {
                        [PLUCK_P1_LVL, PLUCK_P2_LVL, 0.0]
                    } else {
                        restruck_plucks += 1;
                        assert_eq!(
                            t.n_lvl, RESTRIKE_MALLET_LVL,
                            "`{run}` key {i}: a re-struck `{ch}` took the step's fingertip"
                        );
                        [P1_LVL, RESTRIKE_P2_LVL, 0.0]
                    };
                    assert_eq!(
                        [t.p[0].lvl, t.p[1].lvl, t.p[2].lvl],
                        want,
                        "`{run}` key {i}: `{ch}`'s levels"
                    );
                }
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
                    if i > 0 {
                        assert_eq!(
                            s.v2.pending_mark(),
                            MARK_NONE,
                            "`{run}` key {i}: a doubled stop still books a cadence"
                        );
                    }
                }
                let _ = render_mono(&mut s, 12);
            }
        }
        assert!(
            felt_keys > 0,
            "fixture: a machine-regular run never reached the ladder's felt rung"
        );
        assert!(
            restruck_plucks > 0,
            "fixture: no re-struck pluck was ever measured"
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
            for (i, c) in "hello ".chars().enumerate() {
                let kind = if c == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(&mut s, kind, 1_000 + i as u32 * 150, c);
            }
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

    /// A deleted mark does not still ask (§12.3). A graft is addressed, so a
    /// Backspace takes it back: unheard if it has not opened (`t < 0`), on the
    /// [`ERASE_MUTE_S`] ramp if it has. A Kill takes the whole [`LANE_GRAFT`]
    /// down and forgets all three addresses. The unheard half is the Tier-1 bind
    /// `typed_grafts_conform_to_owned_retirement_before_and_after_onset`.
    #[test]
    fn a_deleted_mark_does_not_still_ask() {
        let head = |s: &mut TrailSynth| {
            for (i, c) in "hello ".chars().enumerate() {
                let kind = if c == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(s, kind, 1_000 + i as u32 * 150, c);
            }
            let _ = render_mono(s, 12);
        };
        // A fifth already swelling is ramped, not cut.
        {
            let mut s = synth();
            head(&mut s);
            push_ch(&mut s, SoundKind::Typed, 1_900, '=');
            let (slot, born) = s.v2.graft.expect("`=` left its fifth's address");
            let _ = render_mono(&mut s, 10);
            assert!(
                s.voices[usize::from(slot)].t > 0.0,
                "fixture: the fifth has not opened yet"
            );
            push(&mut s, SoundKind::Backspace, 2_000, 0.0, false);
            let v = s.voices[usize::from(slot)];
            assert!(
                v.on && v.born == born,
                "the sounding fifth was cut instead of ramped"
            );
            assert_eq!(
                v.damp, ERASE_MUTE_S,
                "the fifth's ramp is not the erase mute"
            );
        }
        // A Kill takes the whole lane, and every address with it.
        {
            let mut s = synth();
            head(&mut s);
            push_ch(&mut s, SoundKind::Typed, 1_900, 'W');
            push_ch(&mut s, SoundKind::Typed, 2_000, '(');
            let _ = render_mono(&mut s, 6);
            let live: Vec<usize> = s
                .voices
                .iter()
                .enumerate()
                .filter(|(_, v)| v.on && v.lane == LANE_GRAFT)
                .map(|(i, _)| i)
                .collect();
            assert!(
                live.len() >= 2,
                "fixture: the ring and the tink are not both live ({})",
                live.len()
            );
            push(&mut s, SoundKind::Kill, 2_060, 0.0, false);
            for i in live {
                let v = s.voices[i];
                assert!(
                    !v.on || v.damp > 0.0,
                    "a killed line left a graft ringing in slot {i}"
                );
            }
            assert!(
                s.v2.ring.is_none() && s.v2.graft.is_none() && s.v2.lift.is_none(),
                "the Kill kept a graft address it had already damped"
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
                        for (index, c) in "hello ".chars().enumerate() {
                            let kind = if c == ' ' {
                                SoundKind::Space
                            } else {
                                SoundKind::Typed
                            };
                            push_ch(&mut s, kind, 1_000 + index as u32 * 150, c);
                        }
                        let _ = render_mono(&mut s, 12);
                        push_ch(&mut s, SoundKind::Typed, 1_900, ch);
                        let (slot, born) = if ch == 'W' { s.v2.ring } else { s.v2.graft }
                            .expect("the typed key produced its pitched decoration");
                        let slot = usize::from(slot);
                        assert!(s.voices[slot].on && s.voices[slot].t < 0.0);
                        let mut expected = model.init_state();
                        // Exercise retirement close to onset too: the old Kill
                        // damp let the 30 ms fifth start during its 40 ms ramp.
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
                    for (index, c) in "hello ".chars().enumerate() {
                        let kind = if c == ' ' {
                            SoundKind::Space
                        } else {
                            SoundKind::Typed
                        };
                        push_ch(&mut s, kind, 1_000 + index as u32 * 150, c);
                    }
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

    /// Every class is deterministic, and the letter path is unchanged (A27).
    ///
    /// 1. [`MARK_SENTENCE`] with a Shift cue 60 ms ahead of every capital,
    ///    rendered twice under one seed: bit-identical. Every draw on the class
    ///    paths comes from the seeded stream, and no branch reads a clock.
    /// 2. A letter-only script renders to a fixed fingerprint. Every branch
    ///    that touches the voice for a class is behind `class != LETTER`, and
    ///    `typed_glyph_class` answers `LETTER` for every letter and space in
    ///    the corpus (asserted alongside).
    #[test]
    fn every_class_is_deterministic_and_the_letter_path_is_untouched() {
        /// FNV-1a over the raw bits of `"hello world"` at 8 cps: 12 blocks of
        /// 480 frames per key and 40 after the last.
        ///
        /// Identical in debug and release: the equal-power pan law
        /// (`trail_sound::pan_gains`) keeps the cosine and sine as separate
        /// calls (an optimised build would otherwise fuse them into a
        /// `sincos` that differs in the last bit). A release-only miss with
        /// debug green points at `pan_gains`, not at this pin.
        const LETTER_PATH_FOLD: u64 = 0x11f9_a5a4_2d77_1774;
        let fold = |x: &[f32]| -> u64 {
            x.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, s| {
                (h ^ u64::from(s.to_bits())).wrapping_mul(0x0000_0100_0000_01b3)
            })
        };
        let script = |text: &str, shift_cues: bool| -> Vec<f32> {
            let mut s = synth();
            let mut out: Vec<f32> = Vec::new();
            let mut at = 1_000u32;
            for ch in text.chars() {
                if shift_cues && ch.is_uppercase() {
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
        let a = script(MARK_SENTENCE, true);
        let b = script(MARK_SENTENCE, true);
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
        for ch in "hello world".chars() {
            assert_eq!(
                crate::trail_sound::typed_glyph_class(Some(ch)),
                LETTER,
                "`{ch}` is not a LETTER to the class table"
            );
        }
        let letters = script("hello world", false);
        crate::arm64_pin::deterministic_on_x86_64(
            "the letter-only script",
            &fold(&letters),
            || fold(&script("hello world", false)),
        );
        crate::arm64_pin::assert_pinned(
            "trail_sound::rainbow_kitty_v2::tests::every_class_is_deterministic_and_the_letter_path_is_untouched",
            "LETTER_PATH_FOLD",
            fold(&letters),
            LETTER_PATH_FOLD,
            format_args!(
                "the LETTER path moved: {} samples folded to {:#018x}, and the base tree \
                 3aa2e825b rendered {LETTER_PATH_FOLD:#018x} — find the leak before changing the pin",
                letters.len(),
                fold(&letters)
            ),
        );
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

    /// τ_v passes through §3.1's two anchors (110 ms at 4 cps, 28 ms at 20 cps),
    /// and from there down the floor, not the intake clamp, decides the note.
    #[test]
    fn tau_v_meets_both_anchors_and_the_floor_is_reachable() {
        assert!((tau_v_s(0.25) - 0.110).abs() < 1e-4, "{}", tau_v_s(0.25));
        assert!((tau_v_s(0.05) - 0.028).abs() < 1e-4, "{}", tau_v_s(0.05));
        assert_eq!(
            tau_v_s(IOI_MIN_MS * 0.001),
            TAU_V_MIN_S,
            "the floor does not bind"
        );
        // The curve must reach the floor, or the floor is not a law.
        const { assert!(TAU_V_BASE_S * (TAU_V_OFFSET + TAU_V_SLOPE * IOI_MIN_MS * 0.001) < TAU_V_MIN_S) }
        // …and through the engine: a 20 cps run's notes are 28 ms.
        let mut s = synth();
        let mut last = 0.0f32;
        for (k, ch) in "abcdefghijklmnopqrstuvwxyz".chars().enumerate() {
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Typed, 1_000 + k as u32 * 50, ch);
            last = tune_voices(&since(&s, mark))[0].decay;
        }
        assert!(
            (last - 0.028).abs() < 1e-3,
            "a settled 20 cps run plays {last} s notes, not 28 ms"
        );
    }

    /// The bloom fades in behind every lit step, on the lattice, and follows
    /// the hue (§3.3 items 1-3).
    ///
    /// (a) A session's first key carries [`IOI_DEFAULT_MS`] (4 cps, τ_v =
    /// [`TAU_V_MAX_S`]), so it reads exactly `BLOOM_DELAY_S`, `BLOOM_ATTACK_S`,
    /// `BLOOM_DECAY_S` and `BLOOM_DUR_S`: the τ_v scaling is a no-op at the
    /// anchor. (b) At 9 cps, where the scaling binds, the bloom is found by its
    /// lane and partials.
    ///
    /// A lit step carries one voice in [`LANE_BLOOM`] at [`BLOOM_DELAY_S`] with
    /// [`BLOOM_ATTACK_S`], partials on [`BLOOM_DEGREES`] above the note (3f /
    /// 4f / 6f exactly on C); a passing note, re-strike or felt key carries
    /// none. Its level scales by [`hue_air`] and its pan drifts by
    /// [`BLOOM_SPREAD`]; the note's roof opens by [`ROOF_HUE_ADD_HZ`] and its
    /// gain does not move. With the stops out, none of it exists.
    #[test]
    fn the_bloom_rides_every_lit_step_on_the_lattice_and_follows_the_hue() {
        // (a) The session's first key is C on I: a lit step. Its bloom.
        let mut s = synth();
        let mark = s.born_seq;
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let spawned = since(&s, mark);
        let lead = tune_voices(&spawned)[0];
        let blooms: Vec<&Voice> = spawned.iter().filter(|v| v.lane == LANE_BLOOM).collect();
        assert_eq!(blooms.len(), 1, "a lit step carries exactly one bloom");
        let b = blooms[0];
        assert_eq!(b.delay, BLOOM_DELAY_S);
        assert_eq!(b.attack, BLOOM_ATTACK_S);
        assert_eq!(b.decay, BLOOM_DECAY_S);
        assert_eq!(
            b.dur, BLOOM_DUR_S,
            "at the 4 cps anchor the per-note hang must be the constant it \
             replaced, to the bit"
        );
        assert_eq!(
            b.n_lvl, 0.0,
            "the bloom has no mallet: the strike already happened"
        );
        for (k, ratio) in [3.0f32, 4.0, 6.0].iter().enumerate() {
            assert!(
                (b.p[k].f0 - lead.p[0].f0 * ratio).abs() < 1e-2,
                "on C the bloom's partial {k} is {} Hz, not {ratio}f",
                b.p[k].f0
            );
            assert_eq!(b.p[k].decay, BLOOM_TAU[k]);
        }
        assert!(
            (-b.dur / b.decay).exp() <= 0.07,
            "the bloom's tail breaks A13"
        );

        // (b) Over a corpus: a bloom iff the key was a lit step with a pitch.
        let mut s = synth();
        let mut buf = [0.0f32; 960];
        let (mut with, mut without) = (0usize, 0usize);
        for (k, ch) in "the quick brown fox jumps over the lazy dogg"
            .chars()
            .enumerate()
        {
            let mark = s.born_seq;
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            push_ch(&mut s, kind, 1_000 + k as u32 * 110, ch);
            // The audio clock keeps up with the hand, as on a host: a bench
            // that never renders fills the pool and measures the pool.
            for _ in 0..11 {
                s.render(&mut buf);
            }
            if ch == ' ' {
                continue;
            }
            let spawned = since(&s, mark);
            let lead = tune_voices(&spawned)[0];
            let step = lead.p[2].lvl > 0.0; // a Step's strike partial; 0 on a re-strike / felt key
            // A bloom is the LANE_BLOOM voice with the bloom's partials: the
            // answering voice shares the lane but is a tine, and the air taps
            // are blooms held off by their own delay. Its attack is scaled at
            // this rhythm, so it is not matched on.
            let bloomed = spawned
                .iter()
                .filter(|v| {
                    v.lane == LANE_BLOOM
                        && v.delay < AIR_TAP_DELAY_S[0]
                        && v.n_lvl == 0.0
                        && v.p[2].lvl == BLOOM_LVL[2]
                })
                .count();
            let want = usize::from(s.v2.lit() && step);
            assert_eq!(
                bloomed,
                want,
                "key {k} `{ch}`: lit {} step {step}",
                s.v2.lit()
            );
            if want == 1 { with += 1 } else { without += 1 }
        }
        assert!(
            with > 10 && without > 0,
            "fixture: with {with}, without {without}"
        );

        // (c) The hue: air on the bloom's level, spread on its pan, the arc
        // on the note's roof — and not a decibel on the note.
        let under_hue = |hue: f32, stops: TimbreStops| -> (Voice, Option<Voice>) {
            let mut s = synth();
            s.set_v2_timbre_stops(stops);
            let mut ev = event(SoundKind::Typed, 0.0, false);
            ev.hue = hue;
            ev.heat = 0.0;
            let mark = s.born_seq;
            s.push_meta(
                ev,
                EventMeta {
                    at_ms: 1_000,
                    ..EventMeta::default()
                },
            );
            let spawned = since(&s, mark);
            (
                tune_voices(&spawned)[0],
                spawned.iter().find(|v| v.lane == LANE_BLOOM).copied(),
            )
        };
        let (red, red_b) = under_hue(0.0, TimbreStops::ALL);
        let (cyan, cyan_b) = under_hue(0.5, TimbreStops::ALL);
        let (red_b, cyan_b) = (red_b.expect("bloom"), cyan_b.expect("bloom"));
        assert_eq!(
            red.gl + red.gr,
            cyan.gl + cyan.gr,
            "the hue bought a decibel on the note"
        );
        assert!(
            (cyan.lp_cut - red.lp_cut - ROOF_HUE_ADD_HZ).abs() < 1e-2,
            "the arc opened the roof by {} Hz, not {ROOF_HUE_ADD_HZ}",
            cyan.lp_cut - red.lp_cut
        );
        let ratio = (cyan_b.gl.powi(2) + cyan_b.gr.powi(2)).sqrt()
            / (red_b.gl.powi(2) + red_b.gr.powi(2)).sqrt();
        let want = hue_air(0.5) / hue_air(0.0);
        assert!(
            (ratio - want).abs() < 1e-3,
            "the bloom's level moved ×{ratio} from red to cyan, hue_air says ×{want}"
        );
        // Red sits BLOOM_SPREAD/2 to one side of the strike, cyan to the other:
        // the pans differ by the spread (equal-power law: read the angle back
        // off the gains).
        let angle = |v: &Voice| v.gr.atan2(v.gl);
        assert!(
            angle(&cyan_b) > angle(&red_b) + 1e-3 && angle(&red_b) < angle(&red) - 1e-3,
            "the bloom's pan does not drift with the hue"
        );
        // Stops out: no bloom, no arc, the pre-§3.3 roof exactly.
        let (plain_red, none_r) = under_hue(0.0, TimbreStops::PLAIN);
        let (plain_cyan, none_c) = under_hue(0.5, TimbreStops::PLAIN);
        assert!(none_r.is_none() && none_c.is_none(), "PLAIN still blooms");
        assert_eq!(
            plain_red.lp_cut, plain_cyan.lp_cut,
            "PLAIN still reads the hue"
        );
        assert_eq!(
            plain_red.lp_cut, red.lp_cut,
            "the red end of the arc is not the shipped roof"
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
        // A rest: the key after ≥ PHRASE_PAUSE_MS carries two taps, at the
        // air cloud's spacings, on opposite sides.
        at += PHRASE_PAUSE_MS;
        let mark = s.born_seq;
        push_ch(&mut s, SoundKind::Typed, at, 'e');
        let after_rest = since(&s, mark);
        let lead = tune_voices(&after_rest)[0];
        let t = taps(&after_rest);
        assert_eq!(
            t.len(),
            2,
            "a rest leaves two taps in the room, got {}",
            t.len()
        );
        for (tap, k) in t.iter().zip(AIR_TAP_DELAY_S) {
            assert!((tap.delay - k).abs() < 1e-6, "tap at {} s", tap.delay);
            assert!(
                (tap.p[1].f0 - 4.0 * lead.p[0].f0).abs() < 1e-2,
                "the tap is not the key's own bloom"
            );
        }
        let side = |v: &Voice| v.gr.atan2(v.gl);
        assert!(
            (side(&t[0]) - side(&lead)) * (side(&t[1]) - side(&lead)) < 0.0,
            "the two taps sit on the same side of the note"
        );
        assert!(
            t[0].gl.hypot(t[0].gr) > t[1].gl.hypot(t[1].gr),
            "the later tap must be the quieter one"
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

    /// The bloomed step is brighter than the tine and under the glass line (§3.3,
    /// §8 step 6): on §9.1's probe the bloom lifts the isolated step's centroid,
    /// and it stays under 1600 Hz, past which [`BLOOM_LEVEL`] must give back.
    #[test]
    fn the_bloomed_step_is_brighter_than_the_tine_and_under_the_glass_line() {
        // `a` then `e`, 300 ms apart: a rising third inside the dead band, so
        // the probed key lands on E, lit under the parked IV, with a bloom to
        // measure (the fixture asserts the chord tone).
        let probe = |stops: TimbreStops, hue: f32| -> f32 {
            let mut s = synth();
            s.set_v2_timbre_stops(stops);
            let key = |ch: char| EventMeta {
                rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                ..EventMeta::default()
            };
            let mut ev = event(SoundKind::Typed, 0.0, false);
            ev.hue = hue;
            s.push_meta(
                ev,
                EventMeta {
                    at_ms: 1_000,
                    ..key('a')
                },
            );
            let _ = render_mono(&mut s, 30);
            s.push_meta(
                ev,
                EventMeta {
                    at_ms: 1_300,
                    ..key('e')
                },
            );
            assert!(s.v2.lit(), "fixture: the probed key must be a chord tone");
            probe_centroid_hz(&render_mono(&mut s, 50))
        };
        let plain = probe(TimbreStops::PLAIN, 0.0);
        let red = probe(TimbreStops::ALL, 0.0);
        let cyan = probe(TimbreStops::ALL, 0.5);
        println!(
            "§9.1 probe centroid: plain {plain:.0} Hz, bloomed red {red:.0} Hz, cyan {cyan:.0} Hz"
        );
        // Red end ~770 Hz, cyan ~950 Hz, over a ~680 Hz bare tine.
        assert!(
            red > plain + 60.0,
            "the bloom did not lift the centroid ({plain} -> {red})"
        );
        assert!(
            cyan > red,
            "the cyan end of the arc is not brighter than the red end"
        );
        assert!(
            cyan < 1600.0,
            "the bloomed step reads {cyan:.0} Hz at the cyan end — a glass bell"
        );
    }

    // -- A11: no roughness ------------------------------------------------

    /// No partial pair beats in the 15–60 Hz roughness band above 1 kHz (§9.5
    /// law 4, A11).
    ///
    /// On a just lattice harmonic partials coincide or sit a consonant interval
    /// apart, so only an inharmonic partial can break the law. Exempt: the
    /// 2.760 strike (τ 40 ms, under the 67 ms one 15 Hz beat needs) and
    /// inharmonic FM voices (ice bell, meteor core), whose sidebands are
    /// off-lattice by design. A fractional FM ratio is exempt; an integer ratio
    /// (forte's [`FORTE_FM_RATIO`] 4) is a lattice voice and is censused.
    ///
    /// A gliding partial is censused at the pitch it rests on, and the glide's
    /// own τ is held to the same 45 ms bound (no pitched partial glides today).
    ///
    /// The script presses a bare Shift 60 ms ahead of every ninth cue: the
    /// ting is a pure lattice sine ringing a second in the tune's octave
    /// register, and the census must see its fundamental or the clause is
    /// vacuous.
    #[test]
    fn no_partial_pair_beats_in_the_roughness_band_above_one_kilohertz() {
        let mut s = synth();
        let mut worst: Option<(f32, f32)> = None;
        let mut tings_censused = 0u32;
        for k in 0..120u32 {
            let at = 1_000 + k * 100;
            if k % 9 == 8 {
                push(&mut s, SoundKind::Shift, at - 60, 0.0, false);
            }
            if k % 13 == 12 {
                // The classed keys: the digit's 3f / 5f bar partials are
                // censused live against everything else.
                push_ch(&mut s, SoundKind::Typed, at, '7');
            } else if k % 6 == 5 {
                push(&mut s, SoundKind::Space, at, 0.0, false);
            } else {
                push(
                    &mut s,
                    SoundKind::Typed,
                    at,
                    (k % 7) as f32 * 0.2 - 0.6,
                    k % 11 == 0,
                );
            }
            tings_censused += s
                .voices
                .iter()
                .filter(|v| v.on && v.lane == LANE_TING && v.damp <= 0.0)
                .count() as u32;
            // Everything alive at this instant, i.e. everything that can beat
            // against everything else.
            let live: Vec<(f32, f32, bool, f32)> = s
                .voices
                .iter()
                .filter(|v| v.on)
                .flat_map(|v| {
                    v.p.iter().filter(|p| p.lvl > 0.0).map(|p| {
                        // Where the partial lives, not where it started: a
                        // gliding partial's `f0` is a value it leaves. The
                        // glide's own bound is asserted below, which makes this
                        // legitimate.
                        let rest = if p.glide > 0.0 { p.f1 } else { p.f0 };
                        // Inharmonic FM only: forte's ratio 4 is a lattice
                        // voice and stays in the census.
                        let ice = p.fm_ratio > 0.0 && p.fm_ratio.fract() != 0.0;
                        (rest, p.decay, ice, p.glide)
                    })
                })
                .collect();
            for (i, a) in live.iter().enumerate() {
                // A glide is admitted by A11's 45 ms exemption for the same
                // arithmetic: while travelling, its pairs with the live lattice
                // sweep through the roughness band (≈ 5 ms inside 0.5-60 Hz for
                // a typical case) rather than sitting in it, and even a 60 Hz
                // beat needs 17 ms for one cycle.
                assert!(
                    a.3 <= 0.045,
                    "a pitched partial glides with τ {} s, outside A11's 45 ms \
                     exemption: it sits detuned long enough to beat",
                    a.3
                );
                for b in &live[i + 1..] {
                    // Exempt: sub-kilohertz pairs (the law is stated above
                    // 1 kHz), short-lived strike partials, and inharmonic FM
                    // voices.
                    if a.0 < 1000.0 || b.0 < 1000.0 {
                        continue;
                    }
                    if a.2 || b.2 {
                        continue;
                    }
                    let exempt = |d: f32| d > 0.0 && d <= 0.045;
                    if exempt(a.1) || exempt(b.1) {
                        continue;
                    }
                    let d = (a.0 - b.0).abs();
                    if (0.5..=60.0).contains(&d) {
                        worst = Some((a.0, b.0));
                    }
                }
            }
            let mut buf = [0.0f32; 960];
            s.render(&mut buf);
        }
        assert!(
            worst.is_none(),
            "a partial pair beats in the roughness band: {worst:?}"
        );
        // 13 Shifts, each ringing across ~10 cues (some cut short by the
        // same-pitch damp).
        assert!(
            tings_censused >= 60,
            "the ting was live at only {tings_censused} censuses — the script is not \
             testing the bell against the line"
        );
        println!("A11: the ting was live at {tings_censused} censuses");
    }

    // -- §9.1's isolated measurements ------------------------------------

    /// The tine is small and warm, where v1's bell was hard and bright (§9.0's
    /// third cause). v2's tine is sine-led with per-partial decays (octave gone
    /// in 55 ms, strike in 40, felt mallet by 25); on one probe and window its
    /// spectral centre must sit well below v1's glass bell, whose figure on this
    /// exact probe is the recorded [`V1_BELL_PROBE_CENTROID_HZ`].
    ///
    /// This probe is magnitude-weighted over 85 ms, the most generous reading a
    /// wideband mallet shoulder can get, so the felt mallet's high sweep lifts
    /// it; `the_isolated_step_centroid_is_the_one_the_partial_table_builds`
    /// reads the same instrument energy-weighted over the whole body and stays
    /// in its 550-800 Hz band.
    #[test]
    fn the_tine_is_small_and_warm_not_hard_and_bright() {
        /// v1's glass bell on this probe (seed `SEED`, one `Typed` at 0 ms,
        /// `render_mono` over 12 blocks, the 4096-sample magnitude centroid): a
        /// recorded baseline.
        const V1_BELL_PROBE_CENTROID_HZ: f32 = 3167.122;
        // 4096 samples = 85 ms: the strike, the octave and the first third of
        // the body, the window in which a tine is small or hard. The tine alone:
        // §3.3's bloom is a second voice, pinned by its own test.
        let mut s = synth();
        s.set_v2_timbre_stops(TimbreStops::PLAIN);
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let v2 = render_mono(&mut s, 12);
        let v2_c = centroid_hz(&v2[..4096]);
        println!("85 ms magnitude centroid: v2 {v2_c:.0} Hz");

        assert!(
            v2_c < V1_BELL_PROBE_CENTROID_HZ * 0.75,
            "the v2 tine's centroid is {v2_c:.0} Hz against v1's \
             {V1_BELL_PROBE_CENTROID_HZ:.0} Hz — the bell did not get smaller"
        );
        assert!(
            (500.0..=1300.0).contains(&v2_c),
            "the v2 tine's centroid is {v2_c:.0} Hz, outside this probe's \
             measured 500-1300 Hz band for a C5 step"
        );
    }

    /// The felt mallet chirps up into the sparkle band, and it climbs
    /// ([`MALLET_HZ0`] / [`MALLET_HZ1`]). At 1800 → 6400 Hz the whole sweep sits
    /// above every pitched thing on the key.
    ///
    /// Where the transient sits: on a plain isolated step (no bloom, no glint),
    /// the energy-weighted centroid over the mallet's 25 ms life, restricted to
    /// 1.5-12 kHz so neither the tine's octave (1046 Hz on C5) nor its strike
    /// (1443 Hz) nor the SVF's shoulder can vote, lies above a bound only the
    /// raised sweep clears.
    ///
    /// How loud it is: a fixed-Q band-pass's bandwidth is `f0/Q`, so the higher
    /// sweep delivers more noise power at the same [`MALLET_LVL`]; the chirp
    /// stands clear of the note's body in the same band.
    ///
    /// That it climbs is pinned on the voice; the assertion says why the
    /// render cannot settle it.
    #[test]
    fn the_felt_mallet_chirps_up_into_the_sparkle_band_and_climbs() {
        /// Energy-weighted centroid of `x` over `lo_hz..=hi_hz`: a magnitude
        /// weight would let a 12 dB/octave noise shoulder outvote the band it
        /// fell off, which is the quantity under test.
        fn centroid_band(x: &[f32], lo_hz: f32, hi_hz: f32) -> f32 {
            let n = x.len();
            let win: Vec<f32> = (0..n)
                .map(|i| 0.5 * (1.0 - (core::f32::consts::TAU * i as f32 / n as f32).cos()))
                .collect();
            let mut num = 0.0f64;
            let mut den = 0.0f64;
            for k in 1..n / 2 {
                let f = k as f64 * f64::from(SR) / n as f64;
                if f < f64::from(lo_hz) || f > f64::from(hi_hz) {
                    continue;
                }
                let mut re = 0.0f64;
                let mut im = 0.0f64;
                let w = core::f64::consts::TAU * k as f64 / n as f64;
                for (i, (v, h)) in x.iter().zip(&win).enumerate() {
                    let a = w * i as f64;
                    let v = f64::from(*v * *h);
                    re += v * a.cos();
                    im -= v * a.sin();
                }
                let e = re * re + im * im;
                num += e * f;
                den += e;
            }
            if den <= 0.0 { 0.0 } else { (num / den) as f32 }
        }

        /// Energy in `lo_hz..=hi_hz`, dB — the same transform, summed
        /// instead of weighted.
        fn band_energy_db(x: &[f32], lo_hz: f32, hi_hz: f32) -> f32 {
            let n = x.len();
            let win: Vec<f32> = (0..n)
                .map(|i| 0.5 * (1.0 - (core::f32::consts::TAU * i as f32 / n as f32).cos()))
                .collect();
            let mut e = 0.0f64;
            for k in 1..n / 2 {
                let f = k as f64 * f64::from(SR) / n as f64;
                if f < f64::from(lo_hz) || f > f64::from(hi_hz) {
                    continue;
                }
                let mut re = 0.0f64;
                let mut im = 0.0f64;
                let w = core::f64::consts::TAU * k as f64 / n as f64;
                for (i, (v, h)) in x.iter().zip(&win).enumerate() {
                    let a = w * i as f64;
                    let v = f64::from(*v * *h);
                    re += v * a.cos();
                    im -= v * a.sin();
                }
                e += re * re + im * im;
            }
            10.0 * (e.max(1e-30)).log10() as f32
        }

        let mut s = synth();
        s.set_v2_timbre_stops(TimbreStops::PLAIN);
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let x = render_mono(&mut s, 20);
        // The onset, found the way §9.1's probe finds one: the first
        // 256-frame window over 15 % of the take's peak.
        let env: Vec<f32> = x.chunks(256).map(rms).collect();
        let peak = env.iter().fold(0.0f32, |m, v| m.max(*v));
        let on = env.iter().position(|e| *e > peak * 0.15).unwrap_or(0) * 256;
        // The mallet's whole life: 25 ms, ≈ 4τ, where it is −36 dB.
        let life = (0.025 * SR) as usize;
        let transient = &x[on..(on + life).min(x.len())];
        let centre = centroid_band(transient, 1_500.0, 12_000.0);
        // How loud: the transient's energy over 1.5 kHz (only the mallet lives
        // there on a plain step) against the note's body, the 25 ms starting
        // 100 ms in, where the mallet is 16 τ gone. A ratio, so upstream trims
        // cancel.
        let body = &x[(on + (0.100 * SR) as usize).min(x.len())
            ..(on + (0.100 * SR) as usize + life).min(x.len())];
        let audibility =
            band_energy_db(transient, 1_500.0, 12_000.0) - band_energy_db(body, 1_500.0, 12_000.0);
        println!(
            "felt mallet: transient centre {centre:.0} Hz over 1.5-12 kHz, \
             {audibility:.2} dB over the note's own body there"
        );
        assert!(
            centre > 3_700.0,
            "the felt mallet's transient sits at {centre:.0} Hz — back inside \
             the tine's own register, where a chirp cannot be heard as one"
        );
        assert!(
            audibility > 44.0,
            "the chirp stands {audibility:.2} dB over the note's own body — \
             the owner asked for higher AND audible, and the octave pays for \
             both out of the band-pass's own width"
        );
        // It climbs, pinned on the voice the strike is built from. The render
        // cannot settle it: the sweep's τ is 5 ms and the mallet decays in 6,
        // so a late window over 1.5 kHz measures the tine's skirt, falling
        // whichever way the cutoff moves, and energy- and magnitude-weighted
        // reads disagree on the sign.
        let v = tine(523.0, Touch::Step, 0.110, ROOF_PLAIN_LO_HZ, false, 0.0);
        assert!(
            v.n_f1 > v.n_f0,
            "the felt mallet sweeps {} -> {} Hz: downward is the warm-thump \
             direction the Q2 ruling turned around",
            v.n_f0,
            v.n_f1
        );
        assert!(
            v.n_f0 > 1_500.0,
            "the felt mallet starts at {} Hz, inside the tine's own register",
            v.n_f0
        );
    }

    /// Fundamental of `x`, Hz, by autocorrelation with parabolic interpolation
    /// over the lag axis: fine enough to resolve a two-semitone move inside a
    /// 15 ms window, where a DFT bin is 67 Hz wide.
    ///
    /// The search band is `[lo_hz, hi_hz]`: an autocorrelation peaks at every
    /// whole number of periods, so a band an octave under the note reads its
    /// subharmonic. Callers centre the band on the take's own lattice pitch,
    /// ±a fourth.
    fn pitch_hz(x: &[f32], lo_hz: f32, hi_hz: f32) -> f32 {
        let lo = (SR / hi_hz) as usize;
        let hi = (SR / lo_hz) as usize;
        if x.len() <= hi + 2 {
            return 0.0;
        }
        let ac = |lag: usize| -> f64 {
            x[..x.len() - lag]
                .iter()
                .zip(&x[lag..])
                .map(|(a, b)| f64::from(*a) * f64::from(*b))
                .sum()
        };
        let mut best = lo;
        let mut best_v = f64::NEG_INFINITY;
        for lag in lo..=hi {
            let v = ac(lag);
            if v > best_v {
                best_v = v;
                best = lag;
            }
        }
        let (a, b, c) = (ac(best - 1), best_v, ac(best + 1));
        let den = a - 2.0 * b + c;
        let frac = if den.abs() > 1e-30 {
            0.5 * (a - c) / den
        } else {
            0.0
        };
        SR / (best as f32 + frac as f32)
    }

    /// A shifted key holds its pitch from its first millisecond, exactly as its
    /// lowercase twin does, settled on the render (the structural half is
    /// `a_shifted_key_does_not_bend`).
    ///
    /// The same line state and key position, one letter shifted and one not;
    /// the fundamental is tracked by autocorrelation over the note's head and
    /// its body, after a lowpass that rides the note and removes the felt
    /// mallet. Three claims: the lowercase note does not move; the shifted one
    /// moves no more than it (under 1 %); and it sits on the lattice pitch of
    /// the line's degree (the walk's accent, [`CAPITAL_LIFT_DEG`], not an
    /// octave), within 1 %.
    #[test]
    fn a_shifted_key_holds_its_pitch_from_its_first_millisecond() {
        /// One-pole lowpass at `hz`, run twice — enough to put the mallet's
        /// 1800 Hz floor 20 dB down and leave the fundamental untouched.
        fn lp(x: &[f32], hz: f32) -> Vec<f32> {
            let mut y = x.to_vec();
            let k = 1.0 - (-core::f32::consts::TAU * hz / SR).exp();
            for _ in 0..2 {
                let mut prev = 0.0f32;
                for v in &mut y {
                    prev += k * (*v - prev);
                    *v = prev;
                }
            }
            y
        }

        // One settling key, then 600 ms of silence (its tail and glint gone),
        // then the key under test, shifted and not. A previous note still
        // ringing, or a space's bass dyad in the search band, would be what the
        // autocorrelation measured. Plain, so bloom and sparkle are out: the
        // quantity is the strike's own pitch.
        let take = |shifted: bool| -> (f32, f32, f32) {
            let mut s = synth();
            s.set_v2_timbre_stops(TimbreStops::PLAIN);
            push_ch(&mut s, SoundKind::Typed, 1_000, 'a');
            let _ = render_mono(&mut s, 60);
            let mark = s.born_seq;
            push_ch(
                &mut s,
                SoundKind::Typed,
                1_600,
                if shifted { 'W' } else { 'w' },
            );
            let rest = since(&s, mark)
                .into_iter()
                .find(|v| v.lane == LANE_TUNE)
                .map_or(0.0, |v| {
                    if v.p[0].glide > 0.0 {
                        v.p[0].f1
                    } else {
                        v.p[0].f0
                    }
                });
            // The lowpass rides the note (1.85 × the lattice pitch, ~1200 Hz
            // for the lowercase 654 Hz), and so does the search band.
            let x = lp(&render_mono(&mut s, 20), 1.85 * rest);
            let env: Vec<f32> = x.chunks(256).map(rms).collect();
            let peak = env.iter().fold(0.0f32, |m, v| m.max(*v));
            let on = env.iter().position(|e| *e > peak * 0.15).unwrap_or(0) * 256;
            let win = |from_ms: f32, len_ms: f32| -> f32 {
                let a = (on + (from_ms * 0.001 * SR) as usize).min(x.len());
                let b = (a + (len_ms * 0.001 * SR) as usize).min(x.len());
                pitch_hz(&x[a..b], rest * 0.75, rest * 1.34)
            };
            // The head is the note's first milliseconds (2-18 ms); the body
            // starts 45 ms in.
            (win(2.0, 16.0), win(45.0, 30.0), rest)
        };
        let (lo_head, lo_body, lo_rest) = take(false);
        let (hi_head, hi_body, hi_rest) = take(true);
        println!(
            "lower  {lo_head:.0} -> {lo_body:.0} Hz ({:+.1} %), rest {lo_rest:.1}\n\
             shifted {hi_head:.0} -> {hi_body:.0} Hz ({:+.1} %), rest {hi_rest:.1}",
            100.0 * (lo_body / lo_head - 1.0),
            100.0 * (hi_body / hi_head - 1.0),
        );
        // 1 — the unshifted note is a NOTE: it does not move.
        assert!(
            (lo_body / lo_head - 1.0).abs() < 0.01,
            "the lowercase key's pitch moved {lo_head:.0} -> {lo_body:.0} Hz"
        );
        // 2: and so is the shifted one: a hammer does not bend.
        assert!(
            (hi_body / hi_head - 1.0).abs() < 0.01,
            "the shifted key ran {hi_head:.0} -> {hi_body:.0} Hz, a move of \
             {:+.1} % — forte is the same note struck harder, not a bend into it",
            100.0 * (hi_body / hi_head - 1.0)
        );
        // 3 — And it sits on the lattice, head and body.
        assert!(
            (hi_body / hi_rest - 1.0).abs() < 0.01 && (hi_head / hi_rest - 1.0).abs() < 0.01,
            "the shifted key read {hi_head:.0} -> {hi_body:.0} Hz against a lattice \
             pitch of {hi_rest:.1}"
        );
        assert!(
            hi_rest > 0.0 && lo_rest > 0.0,
            "fixture: both takes must spawn a pitched TUNE voice"
        );
    }

    // -- A8: Backspace ----------------------------------------------------

    /// Backspace is unpitched, mutes and rewinds (§10.4, §19.2, A8). The
    /// deletion's sound is the style-agnostic poof, with no tonal partial.
    /// Type "ab", erase the "b", type it again: the second "b" plays exactly
    /// the note the first one did.
    #[test]
    fn backspace_is_unpitched_and_takes_the_note_away() {
        let mut s = synth();
        // Far enough apart that every key is a step, so the rewind is visible
        // in the playhead rather than hidden inside one gate window.
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let mark = s.born_seq;
        push(&mut s, SoundKind::Typed, 1_400, 0.0, false);
        let first_b: Vec<f32> = tune_voices(&since(&s, mark))
            .iter()
            .map(|v| v.p[0].f0)
            .collect();
        let after = s.v2.walk();

        let mark = s.born_seq;
        push(&mut s, SoundKind::Backspace, 1_800, 0.0, false);
        let poof = since(&s, mark);
        assert!(!poof.is_empty(), "the erase poof must speak");
        for v in &poof {
            for p in &v.p {
                assert!(
                    p.lvl <= 0.0,
                    "a Backspace spawned a TONAL partial at {} Hz — a deletion \
                     has no note (ruled twice)",
                    p.f0
                );
            }
        }
        // The newest tune voice is muted over 40 ms, not cut.
        assert!(
            s.voices
                .iter()
                .any(|v| v.on && v.lane == LANE_TUNE && (v.damp - ERASE_MUTE_S).abs() < 1e-6),
            "the newest tune voice was not muted by the erase"
        );
        // …and the state is back where the erased letter found it.
        let mark = s.born_seq;
        push(&mut s, SoundKind::Typed, 2_200, 0.0, false);
        let second_b: Vec<f32> = tune_voices(&since(&s, mark))
            .iter()
            .map(|v| v.p[0].f0)
            .collect();
        assert_eq!(
            second_b, first_b,
            "the retyped letter sang a different note — the melody did not rewind"
        );
        assert_eq!(
            s.v2.walk(),
            after,
            "the line did not land where it had been"
        );
    }

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
            let spawned = since(&s, mark);
            let landed: Vec<&Voice> = spawned
                .iter()
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
            // The room answers the line's end (§3.3 item 4): two bloom taps
            // behind the resolution, on the same edge plus their spacings,
            // after the note, never on it.
            let taps: Vec<f32> = spawned
                .iter()
                .filter(|v| v.lane == LANE_BLOOM)
                .map(|v| v.delay)
                .collect();
            assert_eq!(
                taps.len(),
                AIR_TAP_DELAY_S.len(),
                "cells {cells}: a line end leaves exactly two taps in the room"
            );
            for (tap, k) in taps.iter().zip(AIR_TAP_DELAY_S) {
                assert!(
                    (tap - (want + k)).abs() < 1e-5,
                    "cells {cells}: an air tap at {tap} s, not {} s",
                    want + k
                );
            }
        }
    }

    // -- A17 / A20 / A26: the rest of the gesture table --------------------

    /// Direction lives in the core; the whoosh is blind (§12.3, A17). The core
    /// and whoosh leave the origin column and arrive at the destination on the
    /// flight's own clock.
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
        assert!((right - 2.0).abs() < 1e-4, "rightward core ratio {right}");
        assert!((left - 0.5).abs() < 1e-4, "leftward core ratio {left}");
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
    /// re-strikes. A keyed `Enter` is the cadence and mints no cascade.
    #[test]
    fn the_cascade_never_stacks() {
        let mut s = synth();
        let mark = s.born_seq;
        push(&mut s, SoundKind::Jump, 1_000, -0.5, false);
        let first = since(&s, mark);
        assert_eq!(first.len(), 4, "one Jump is the four-note brrrring");
        let mut delays: Vec<f32> = first.iter().map(|v| v.delay).collect();
        delays.sort_by(f32::total_cmp);
        for (got, want) in delays.iter().zip(CASCADE_DELAYS_S) {
            assert!((got - want).abs() < 1e-6, "cascade delay {got} != {want}");
        }

        let mut s = synth();
        let mut cascades = 0;
        let mut onsets = 0;
        for k in 0..12u32 {
            let mark = s.born_seq;
            push(&mut s, SoundKind::Jump, 1_000 + k * 60, -0.5, false);
            let n = since(&s, mark).len();
            onsets += n;
            if n == 4 {
                cascades += 1;
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
    }

    /// The run walks the theme; the lone line feed stands. While a program
    /// streams, each line feed of a run takes the authored theme's next phrase
    /// edge (`523 1308 1046 523 654 654 523 1570` Hz) instead of one looping
    /// figure; D18's rate law is unchanged. A lone line feed resolves the line
    /// where it stands.
    #[test]
    fn a_line_feed_run_walks_the_theme_and_a_lone_line_feed_stands() {
        let base_hz = |v: &Voice| v.p[0].f0;
        let expect = |deg: i32| penta(TINE_BASE_HZ, deg);
        let mut s = synth();
        for k in 0..5u32 {
            push(&mut s, SoundKind::Typed, 1_000 + k * 150, 0.0, false);
        }
        // A lone line feed 1.3 s after the last key: the derived line
        // resolves where it stands and the cascade is built on it.
        let before = i32::from(s.v2.walk());
        let resolved = s.v2.nearest_lit_within(before, before);
        let mark = s.born_seq;
        push(&mut s, SoundKind::Jump, 3_000, -0.9, false);
        let head = since(&s, mark);
        assert_eq!(head.len(), 4, "a lone line feed is one four-note cascade");
        assert_eq!(
            i32::from(s.v2.walk()),
            resolved,
            "a lone line feed resolves the line where it stands"
        );
        assert!((base_hz(&head[0]) - expect(resolved)).abs() < 0.01);

        // Ten more at 200 ms (5 lines/s): each is a cascade head (200 ≥ 180),
        // each is in the run (200 < 900), and the bases walk the theme's phrase
        // edges.
        let mut buf = [0.0f32; 960];
        let render_ms = |s: &mut TrailSynth, buf: &mut [f32; 960], ms: usize| {
            for _ in 0..ms / 20 {
                s.render(buf);
            }
        };
        // The head stands in for the theme's first edge (0), so the run
        // resumes at the second.
        let theme_edges = [7, 5, 0, 2, 2, 0, 8, 0, 7, 5];
        for (k, &deg) in theme_edges.iter().enumerate() {
            render_ms(&mut s, &mut buf, 200);
            let mark = s.born_seq;
            push(&mut s, SoundKind::Jump, 3_200 + k as u32 * 200, -0.9, false);
            let born = since(&s, mark);
            assert_eq!(born.len(), 4, "line feed {} of the run: one cascade", k + 2);
            assert_eq!(
                i32::from(s.v2.walk()),
                deg,
                "line feed {} of the run walks the theme",
                k + 2
            );
            let got = base_hz(&born[0]);
            assert!(
                (got - expect(deg)).abs() < 0.01,
                "line feed {} of the run: base {got:.1} Hz, want {:.1}",
                k + 2,
                expect(deg)
            );
        }

        // A line feed a second after the last stands alone: the line resolves
        // where the run left it and the theme is re-headed, so the next run
        // starts on the theme's first edge.
        let here = i32::from(s.v2.walk());
        let resolved = s.v2.nearest_lit_within(here, here);
        render_ms(&mut s, &mut buf, 1_000);
        push(
            &mut s,
            SoundKind::Jump,
            3_200 + 9 * 200 + 1_000,
            -0.9,
            false,
        );
        assert_eq!(
            i32::from(s.v2.walk()),
            resolved,
            "a lone line feed after a run resolves"
        );
        render_ms(&mut s, &mut buf, 200);
        push(
            &mut s,
            SoundKind::Jump,
            3_200 + 9 * 200 + 1_200,
            -0.9,
            false,
        );
        assert_eq!(
            i32::from(s.v2.walk()),
            7,
            "a new run re-heads the theme: its head stood in for edge one, so its second line is edge two"
        );

        // Inside a live cascade the rate law holds (twelve line feeds at 60 ms
        // are one cascade and at most eleven re-strikes), and each re-strike is
        // the top note over the run's moving walk.
        let mut s = synth();
        let mut cascades = 0;
        let mut restrikes: Vec<f32> = Vec::new();
        for k in 0..12u32 {
            let mark = s.born_seq;
            push(&mut s, SoundKind::Jump, 1_000 + k * 60, -0.9, false);
            let born = since(&s, mark);
            match born.len() {
                4 => cascades += 1,
                1 => restrikes.push(base_hz(&born[0])),
                0 => {}
                n => panic!("line feed {k}: {n} voices"),
            }
            render_ms(&mut s, &mut buf, 60);
        }
        assert_eq!(cascades, 1, "a 60 ms run is one cascade");
        assert!(restrikes.len() <= 11, "{} re-strikes", restrikes.len());
        let want: Vec<f32> = [7, 5, 0, 2, 2, 0, 8, 0, 7, 5, 0]
            .iter()
            .map(|d| expect(d + CASCADE_RESTRIKE_DEG))
            .collect();
        for (i, (g, w)) in restrikes.iter().zip(&want).enumerate() {
            assert!(
                (g - w).abs() < 0.01,
                "re-strike {i}: {g:.1} Hz, want {w:.1}"
            );
        }
        assert!(
            restrikes.windows(2).any(|w| w[0] != w[1]),
            "the re-strikes of a run must not be one pitch"
        );
    }

    // -- A6: the pure-fifth loop -----------------------------------------

    /// Space walks pure fifths, and Enter comes home (§10.3, A6): every dyad is
    /// an exact 3:2 or 4:3 inside the BASS lane's 261.6–436.0 Hz, and after an
    /// Enter the next Space plays I.
    #[test]
    fn the_space_walks_pure_fifths_and_enter_comes_home() {
        let mut s = synth();
        let mut roots = Vec::new();
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
            roots.push(root);
        }
        // The roots follow `CHORD_LOOP` in order (I vi IV V | I V vi IV) from
        // the parked chord, so the session's first word lands on I.
        let want: Vec<f32> = CHORD_LOOP
            .iter()
            .map(|c| BASS_BASE_HZ * CHORD_ROOT_RATIO[c.root])
            .collect();
        assert_eq!(roots.len(), want.len());
        for (k, (got, want)) in roots.iter().zip(&want).enumerate() {
            assert!(
                (got - want).abs() < 1e-3,
                "word {k}: the downbeat's root is {got} Hz, CHORD_LOOP says {want} Hz"
            );
        }
        // …and the loop is the eight-bar one, not a wander.
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

    /// A space head is heard, and never over the letter ([`BASS_LEVEL`])
    /// (owner, 2026-09-16).
    /// After `"hello"`, one Space against one more letter in the same state,
    /// rendered from the key, on four seeds:
    ///
    /// 1. The tier: the head's peak sits in `[−6.5, −2.0]` dB re the letter's
    ///    (the ladder test's `space ≤ typed × 1.05` is the outer ceiling,
    ///    §9.7).
    /// 2. Top: the head's 200 ms centroid is over 450 Hz and at least a
    ///    fiftieth of its energy is above 2 kHz, so a laptop speaker has
    ///    something to carry.
    /// 3. The anatomy: one dyad in [`LANE_BASS`] and one breath in
    ///    [`LANE_BREATH`], no glint (the twinkle is the breath's own partials,
    ///    folded into `[GLINT_LO_HZ, GLINT_HI_HZ)` at the dyad's root class,
    ///    through an open roof), and the dyad is the root and its partner.
    /// 4. The breath is heard: a tail space 40 ms behind the head (inside the
    ///    step gate: breath alone) adds sound ([`BREATH_ROOF_HZ`]).
    #[test]
    fn a_space_head_is_heard_and_never_over_the_letter() {
        const SEEDS: [u32; 4] = [SEED, 0x5EED_1234, 0x504F_4F46, 0xCAFE_F00D];
        const HEAD_BLOCKS: usize = 40;
        const TAKE_BLOCKS: usize = 60;
        /// The spectral window: the head's first 200 ms.
        const SPECTRUM: core::ops::Range<usize> = 0..9_600;
        const TIER_LO_DB: f32 = -6.5;
        const TIER_HI_DB: f32 = -2.0;
        const CENTROID_FLOOR_HZ: f32 = 450.0;
        const HI_FRACTION_FLOOR: f32 = 0.02;
        let db = |x: f32| 20.0 * x.log10();
        let head = |s: &mut TrailSynth| {
            for (i, ch) in "hello".chars().enumerate() {
                push_ch(s, SoundKind::Typed, 1_000 + i as u32 * 150, ch);
            }
        };
        // Energy fraction above 2 kHz of a Hann-windowed slice, by the same
        // naive DFT `centroid_hz` uses.
        let hi_fraction = |x: &[f32]| -> f32 {
            let n = x.len();
            let xs: Vec<f64> = x
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let h = 0.5 * (1.0 - (core::f64::consts::TAU * i as f64 / n as f64).cos());
                    f64::from(*s) * h
                })
                .collect();
            let tw = dft_twiddles(n);
            let mut above = 0.0f64;
            let mut total = 0.0f64;
            for k in 1..n / 2 {
                let (re, im) = dft_bin(&xs, &tw, k);
                let e = re * re + im * im;
                total += e;
                if k as f64 * f64::from(SR) / n as f64 >= 2_000.0 {
                    above += e;
                }
            }
            if total <= 0.0 {
                0.0
            } else {
                (above / total) as f32
            }
        };
        for seed in SEEDS {
            let take = |kind: SoundKind, ch: char| -> (Vec<f32>, Vec<Voice>) {
                let mut s = TrailSynth::new(SR, seed);
                head(&mut s);
                let _ = render_mono(&mut s, HEAD_BLOCKS);
                let mark = s.born_seq;
                push_ch(&mut s, kind, 1_900, ch);
                let born = since(&s, mark);
                (render_mono(&mut s, TAKE_BLOCKS), born)
            };
            let (letter, _) = take(SoundKind::Typed, 'w');
            let (space, born) = take(SoundKind::Space, ' ');
            let (p_letter, p_space) = (db(peak_of(&letter)), db(peak_of(&space)));
            let re_letter = p_space - p_letter;
            let centroid = centroid_hz(&space[SPECTRUM]);
            let hi = hi_fraction(&space[SPECTRUM]);
            println!(
                "seed {seed:#x}: space head {p_space:.2} dBFS, {re_letter:+.2} dB re the \
                 letter's {p_letter:.2}; centroid {centroid:.0} Hz; over 2 kHz {hi:.3}"
            );
            assert!(
                (TIER_LO_DB..=TIER_HI_DB).contains(&re_letter),
                "seed {seed:#x}: the space head is {re_letter:+.2} dB re the letter — \
                 outside [{TIER_LO_DB}, {TIER_HI_DB}]: heard beside the key, never over it"
            );
            assert!(
                centroid > CENTROID_FLOOR_HZ,
                "seed {seed:#x}: the head's centroid is {centroid:.0} Hz — the dark dyad the \
                 owner could not hear sat at 303"
            );
            assert!(
                hi >= HI_FRACTION_FLOOR,
                "seed {seed:#x}: {hi:.3} of the head's energy is over 2 kHz — a laptop \
                 speaker has nothing to carry"
            );
            // The anatomy.
            let lanes: Vec<u8> = born.iter().map(|v| v.lane).collect();
            assert_eq!(
                lanes,
                vec![LANE_BASS, LANE_BREATH],
                "seed {seed:#x}: the head is one dyad and one breath, nothing else"
            );
            let (dyad, breath) = (&born[0], &born[1]);
            assert!(
                dyad.bass && dyad.p[0].lvl == BASS_ROOT_LVL && dyad.p[1].lvl == BASS_FIFTH_LVL,
                "seed {seed:#x}: the dyad is the dyad"
            );
            let root = dyad.p[0].f0;
            let twinkle = breath.p[0];
            assert!(
                (GLINT_LO_HZ..GLINT_HI_HZ).contains(&twinkle.f0),
                "seed {seed:#x}: the twinkle at {} Hz is outside the stardust lane",
                twinkle.f0
            );
            assert!(
                (twinkle.f0 / root).log2().fract().abs() < 1e-4,
                "seed {seed:#x}: the twinkle ({} Hz) is not the root's ({root} Hz) own class",
                twinkle.f0
            );
            assert_eq!(twinkle.lvl, SPACE_TWINKLE_LEVEL);
            assert_eq!(breath.p[1].lvl, SPACE_TWINKLE_UNDER);
            assert_eq!(
                breath.p[1].f0,
                twinkle.f0 * 0.5,
                "the partner is the octave under"
            );
            assert!(
                breath.lp_cut >= GLINT_HI_HZ,
                "seed {seed:#x}: the breath's roof ({}) would take the twinkle off",
                breath.lp_cut
            );
            assert!(breath.n_lvl > 0.0, "the head's breath still breathes");
            assert!(
                twinkle.decay <= 0.0 && breath.p[1].decay <= 0.0,
                "the twinkle rides the breath's own envelope"
            );
            // The breath is heard: a tail space inside the step gate, against
            // the same take without it.
            let with_tail = {
                let mut s = TrailSynth::new(SR, seed);
                head(&mut s);
                let _ = render_mono(&mut s, HEAD_BLOCKS);
                push_ch(&mut s, SoundKind::Space, 1_900, ' ');
                let mut out = render_mono(&mut s, 4);
                let mark = s.born_seq;
                push_ch(&mut s, SoundKind::Space, 1_940, ' ');
                let tail = since(&s, mark);
                assert_eq!(
                    tail.iter().map(|v| v.lane).collect::<Vec<_>>(),
                    vec![LANE_BREATH],
                    "seed {seed:#x}: a space 40 ms behind the head is breath alone"
                );
                assert!(
                    tail[0].p.iter().all(|p| p.lvl <= 0.0),
                    "a tail breath has no twinkle"
                );
                out.extend(render_mono(&mut s, TAKE_BLOCKS - 4));
                out
            };
            let added = with_tail
                .iter()
                .zip(&space)
                .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
            assert!(
                added > 0.0,
                "seed {seed:#x}: the tail space's breath added nothing to the render — \
                 the breath is silent again"
            );
            assert!(
                db(added) < p_space,
                "seed {seed:#x}: the breath ({:.2} dBFS) is not under the head ({p_space:.2})",
                db(added)
            );
        }
    }

    /// A run of spaces is one downbeat and a rising figure ([`SPACE_STEP_LEVEL`])
    /// (owner, 2026-09-16).
    /// After "hello", four spaces 100 ms apart: the first is the downbeat (the
    /// dyad in [`LANE_BASS`], one chord step, no step voice); each of the next
    /// three is its breath plus one indent step in [`LANE_GLINT`], a lit degree
    /// of the live chord, strictly rising, inside
    /// `[BASS_BASE_HZ, SPACE_STEP_HI_HZ)`, quieter than the downbeat, never a
    /// second downbeat, never moving the chord. A fifth space 40 ms behind is
    /// inside [`SPACE_STEP_MIN_GAP_MS`] and is breath alone. A held spacebar
    /// (96 repeats at 33 ms) gets at most 13 steps a second and hears every
    /// one: each a lit degree in the band, climbing one lit degree per
    /// admitted step and starting over under the ceiling ([`indent_step_hz`]).
    #[test]
    fn a_run_of_spaces_is_one_downbeat_and_a_rising_figure() {
        let mag = |v: &Voice| (v.gl * v.gl + v.gr * v.gr).sqrt();
        let mut s = synth();
        for (i, ch) in "hello".chars().enumerate() {
            push_ch(&mut s, SoundKind::Typed, 1_000 + i as u32 * 150, ch);
        }
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
        let chord = CHORD_LOOP[usize::from(s.v2.text.chord)];
        let chord_idx = s.v2.text.chord;
        let walk = s.v2.walk();
        let key = i32::from(s.song_key);
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
                (BASS_BASE_HZ..SPACE_STEP_HI_HZ).contains(&f),
                "space {n}: the step left its two octaves ({f} Hz)"
            );
            assert!(
                (0..20).any(|deg| {
                    chord.lit & (1 << (deg % 5)) != 0
                        && (penta(BASS_BASE_HZ, deg + key) - f).abs() < SAME_PITCH_HZ
                }),
                "space {n}: {f} Hz is not a lit degree of the live chord"
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

        // The held spacebar: 96 repeats at 33 ms, 3.2 s, every step heard.
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let mut buf = [0.0f32; 3_168]; // 33 ms
        let mut heard: Vec<f32> = Vec::new();
        let mut first_second = 0usize;
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
            if let Some(step) = steps.first() {
                heard.push(step.p[0].f0);
                if k < 30 {
                    first_second += 1;
                }
            }
        }
        assert_eq!(
            (s.lane_births[usize::from(LANE_GLINT)] - glints_before) as usize,
            heard.len(),
            "every LANE_GLINT birth of a held spacebar is an indent step"
        );
        println!(
            "held space: {first_second} indent steps in the first second of 33 ms auto-repeat, \
             {} in 3.2 s: {heard:?}",
            heard.len()
        );
        assert!(
            (4..=13).contains(&first_second),
            "a held spacebar minted {first_second} steps/s — the gate admits one per 75+ ms"
        );
        assert!(
            heard.len() >= 30,
            "3.2 s of auto-repeat minted only {} steps",
            heard.len()
        );
        let chord = CHORD_LOOP[usize::from(s.v2.text.chord)];
        let key = i32::from(s.song_key);
        for (i, f) in heard.iter().enumerate() {
            assert!(
                (BASS_BASE_HZ..SPACE_STEP_HI_HZ).contains(f),
                "held space, step {}: {f} Hz left [{BASS_BASE_HZ}, {SPACE_STEP_HI_HZ}) — the \
                 first cut's fold ran out at the 54th space",
                i + 1
            );
            assert!(
                (0..20).any(|deg| {
                    chord.lit & (1 << (deg % 5)) != 0
                        && (penta(BASS_BASE_HZ, deg + key) - f).abs() < SAME_PITCH_HZ
                }),
                "held space, step {}: {f} Hz is not a lit degree of the live chord",
                i + 1
            );
        }
        // The figure: strictly rising until it starts over at its first note.
        let cycle = heard
            .iter()
            .skip(1)
            .position(|f| (f - heard[0]).abs() < SAME_PITCH_HZ)
            .map(|i| i + 1)
            .expect("3.2 s of steps never came back to the figure's first note");
        assert!((2..=6).contains(&cycle), "a figure of {cycle} notes");
        for (i, f) in heard.iter().enumerate() {
            let expect = heard[i % cycle];
            assert!(
                (f - expect).abs() < SAME_PITCH_HZ,
                "held space, step {}: {f} Hz, but the figure's note {} is {expect}",
                i + 1,
                i % cycle + 1
            );
            if i % cycle > 0 {
                assert!(
                    *f > heard[i - 1] * 1.01,
                    "held space, step {}: the figure must RISE inside a pass ({} -> {f})",
                    i + 1,
                    heard[i - 1]
                );
            }
        }
    }

    /// The indent figure is a cycle on every chord and key ([`indent_step_hz`]).
    /// For every chord of [`CHORD_LOOP`], every song key the latch can hand it
    /// (−3..=4, `latch_song_key`) and 300 steps:
    ///
    /// 1. every step is under [`SPACE_STEP_HI_HZ`] and strictly above the
    ///    dyad's root, and at key 0 inside the §9.4 band
    ///    `[BASS_BASE_HZ, SPACE_STEP_HI_HZ)`;
    /// 2. every step is a lit degree of the chord on the keyed lattice;
    /// 3. the figure is periodic from its first note (step `n` is step
    ///    `n + cycle`) and strictly rising inside a pass;
    /// 4. on chord I over C4 the figure is E4 G4 C5 E5 G5.
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
                if ci == 0 && key == 0 {
                    let want = [327.04, 392.45, 523.26, 654.08, 784.89];
                    assert_eq!(cycle, want.len(), "chord I over C4: E4 G4 C5 E5 G5");
                    for (x, w) in f.iter().zip(want) {
                        assert!((x - w).abs() < SAME_PITCH_HZ, "chord I: {x} vs {w}");
                    }
                }
                println!(
                    "chord {ci} key {key}: root {root:.1} Hz, {cycle}-note figure {:?}",
                    &f[..cycle]
                );
            }
        }
    }

    // -- §16: the deltas are identity-defaulted ---------------------------

    /// Every engine delta defaults to the identity (§16's identity column,
    /// A25). A v1 event builds voices on which every field §16 added is zero,
    /// so every branch reading one is untaken and every v1 f32 expression gets
    /// its v1 operands; this is the structural reason
    /// `palettes_render_within_one_16bit_step_of_v056_reference` and
    /// `brrrring_of_rapid_line_feeds_is_pinned` hold. The rainbow kitty look is
    /// the music box, not a v1 event, and is not in the sweep.
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

    /// The meteor's tick is heard (§12.2 layer 1; [`MET_TICK_ROOF_HZ`],
    /// [`MET_TICK_DUR_S`], [`MET_TICK_LEVEL`]).
    ///
    /// 1. The prototype carries the roof, and every tick both meteor sites
    ///    spawn (the unarmed meteor's, the armed pre-cue's) is that prototype,
    ///    found by its falling 3200 → 900 glide;
    /// 2. the tick alone renders sound in its first 10 ms, and the same voice
    ///    with the roof at 0 renders nothing ([`BREATH_ROOF_HZ`]);
    /// 3. over ten seeds its rendered peak sits within −24 ± 5 dB re the tine
    ///    step's rendered peak, and the median within ±1.5;
    /// 4. it is a tick, not a bang: under the step on every seed, −40 dB re
    ///    its crest by 10 ms, and silent from 20 ms on (its 12.5 ms life has
    ///    ended).
    #[test]
    fn the_meteor_tick_is_heard() {
        let tick = met_tick();
        assert_eq!(tick.lp_cut, MET_TICK_ROOF_HZ);
        assert!(
            tick.lp_cut >= MET_TICK_HZ0,
            "the roof would take the tick's 3200 Hz off"
        );
        let is_tick = |v: &Voice| v.n_lvl > 0.0 && v.n_f0 == MET_TICK_HZ0 && v.n_f1 < v.n_f0;
        // 1. Both sites.
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let mark = s.born_seq;
        meteor(&mut s, 1_100, 50, false);
        let born = since(&s, mark);
        let ticks: Vec<&Voice> = born.iter().filter(|v| is_tick(v)).collect();
        assert_eq!(ticks.len(), 1, "an unarmed meteor opens with one tick");
        assert_eq!(
            ticks[0].lp_cut, MET_TICK_ROOF_HZ,
            "the meteor's tick has no roof"
        );
        assert_eq!(ticks[0].lane, LANE_METEOR);
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let mark = s.born_seq;
        arm(&mut s, 1_100);
        let born = since(&s, mark);
        let ticks: Vec<&Voice> = born.iter().filter(|v| is_tick(v)).collect();
        assert_eq!(ticks.len(), 1, "an armed pre-cue opens with one tick");
        assert_eq!(
            ticks[0].lp_cut, MET_TICK_ROOF_HZ,
            "the arm's tick has no roof"
        );
        assert_eq!(
            ticks[0].arm_ttl, MET_ARM_TTL_S,
            "the arm's tick carries the ttl"
        );
        // 2. The tick alone: heard now, silent before. Three windows: the
        // crest (0-10 ms), the ramp's end (10-20 ms), and after the life.
        let alone = |seed: u32, roof: f32| -> (f32, f32, f32) {
            let mut s = TrailSynth::new(SR, seed);
            let mut v = met_tick();
            v.lp_cut = roof;
            s.v2_spawn(v, VOL * KEY_TINE_TRIM * MET_TICK_LEVEL, 0.0);
            let crest = render_peak(&mut s, 1);
            let tail = render_peak(&mut s, 1);
            let rest = render_peak(&mut s, 4);
            (crest, tail, rest)
        };
        let (before, _, _) = alone(SEED, 0.0);
        assert_eq!(
            before, 0.0,
            "with the roof at 0 the tick rendered — the before is gone"
        );
        // 3. At its ruled level, against the tine step rendered in the same
        // frame (one key, 300 ms, the module's seed).
        const TICK_SEEDS: [u32; 10] = [
            SEED,
            0x5EED_1234,
            0x504F_4F46,
            0xCAFE_F00D,
            0xBEEF,
            0xDEAD_BEEF,
            0x0000_0001,
            0x1234_5678,
            0xFACE_FEED,
            0x7777_7777,
        ];
        const TICK_RE_STEP_DB: f32 = -24.0;
        const TICK_SEED_TOL_DB: f32 = 5.0;
        const TICK_MEDIAN_TOL_DB: f32 = 1.5;
        let step = {
            let mut s = synth();
            push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
            render_peak(&mut s, 30)
        };
        let mut re: Vec<f32> = Vec::new();
        for seed in TICK_SEEDS {
            let (crest, tail, rest) = alone(seed, MET_TICK_ROOF_HZ);
            assert!(crest > 0.0, "seed {seed:#x}: the tick is silent again");
            let db = 20.0 * (crest / step).log10();
            println!(
                "meteor tick alone, vol 0.4, seed {seed:#x}: crest {crest:.5} ({:.2} dBFS) = \
                 {db:+.2} dB re the step ({:.2} dBFS); 10-20 ms {tail:.6}; 20-60 ms {rest}",
                20.0 * crest.log10(),
                20.0 * step.log10()
            );
            // 4. A tick, not a bang.
            assert!(
                crest < step,
                "seed {seed:#x}: the tick ({crest}) peaks over the tine step ({step})"
            );
            assert!(
                tail <= crest * 0.01,
                "seed {seed:#x}: 10 ms in, the tick must be −40 dB under its crest ({tail} vs \
                 {crest})"
            );
            assert_eq!(
                rest, 0.0,
                "seed {seed:#x}: the tick sounded past its 12.5 ms life"
            );
            assert!(
                (db - TICK_RE_STEP_DB).abs() <= TICK_SEED_TOL_DB,
                "seed {seed:#x}: the tick's rendered peak must sit at §12.2's {TICK_RE_STEP_DB} \
                 dB re the step ± {TICK_SEED_TOL_DB} (got {db:+.2})"
            );
            re.push(db);
        }
        re.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
        let median = re[re.len() / 2];
        assert!(
            (median - TICK_RE_STEP_DB).abs() <= TICK_MEDIAN_TOL_DB,
            "over {} seeds the tick's median peak must sit at {TICK_RE_STEP_DB} dB re the step \
             ± {TICK_MEDIAN_TOL_DB} (got {median:+.2}; readings {re:?})",
            re.len()
        );
    }

    /// The ting decays out instead of being cut ([`TING_DUR_S`]): the envelope
    /// alone is under −40 dB before the end, the rendered −40 dB point comes at
    /// least 10 ms before the release ramp starts, and the −60 dB point falls
    /// inside the ramp.
    #[test]
    fn the_ting_decays_out_instead_of_being_cut() {
        assert!(
            (-TING_DUR_S / TING_DECAY_S).exp() <= 0.01,
            "the ting's envelope must be under −40 dB by its own decay before its life ends"
        );
        let life_ms = TING_DUR_S * 1000.0;
        let ramp_ms = life_ms - crate::trail_sound::RELEASE_RAMP_S * 1000.0;
        let mut s = synth();
        push(&mut s, SoundKind::Shift, 1_000, 0.0, false);
        let m = render_mono(&mut s, 110);
        let peak = m.iter().fold(0.0f32, |a, x| a.max(x.abs()));
        let ring = |db: f32| {
            m.iter()
                .rposition(|x| x.abs() > peak * 10f32.powf(db / 20.0))
                .map_or(0.0, |i| i as f32 / 48.0)
        };
        let (to40, to60) = (ring(-40.0), ring(-60.0));
        println!(
            "music-box ting: peak {:.2} dBFS, −40 dB at {to40:.1} ms, −60 dB at {to60:.1} ms, \
             ramp at {ramp_ms:.1}, life {life_ms:.1}",
            20.0 * peak.log10()
        );
        assert!(
            to40 + 10.0 <= ramp_ms,
            "the ting must reach −40 dB re its peak by decay, at least 10 ms before the ramp \
             starts ({to40:.1} ms vs the ramp at {ramp_ms:.1})"
        );
        assert!(
            to60 >= ramp_ms && to60 <= life_ms,
            "the −60 dB point should be the ramp's ({to60:.1} ms, ramp {ramp_ms:.1}..{life_ms:.1}) \
             — otherwise the −40 dB reading was a cut"
        );
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
    /// and the rewind stand outside the 75 ms erase gate, which thins only the
    /// poof. Three letters, three deletions at autorepeat speed: the retyped
    /// letter derives from the first letter's playhead, while the poof speaks
    /// once for the run.
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
    /// it, and idle is exact silence (§9.7, §16 row 10, A24). Below −14 dBFS the
    /// latched bus renders bit-identical to the unlatched one at the ladder's
    /// −21 dBFS step and at A24's −15 dBFS (proving a hard knee); `limit` is
    /// pinned sample-exact at the threshold and one part in a thousand over it.
    /// Above, a meteor at host volume 1.0 (≈ −8 dBFS unlimited) is held at the
    /// 0.2 ceiling within the 0.5 ms attack's overshoot, and once its tails are
    /// gone the limiter is back at exactly 1.0 and the bus at exact zero.
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

        // Below, at A24's −15 dBFS: one decibel under the threshold renders
        // bit-identical latched vs not (a soft knee would fail here). Host
        // volume 0.8 puts the nominal tine near −15 dBFS; this seed's velocity
        // draw is read off a throwaway spawn and divided out of the host volume
        // so the probe is −15 dBFS whatever the seed.
        let minus_15_dbfs = 10f32.powf(-15.0 / 20.0);
        let draw = {
            let mut s = synth();
            let mark = s.born_seq;
            push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
            let v = tune_voices(&since(&s, mark))[0];
            v.gl.max(v.gr) / (VOL * KEY_TINE_TRIM * core::f32::consts::FRAC_1_SQRT_2)
        };
        let probe = |latched: bool| -> (TrailSynth, Vec<f32>) {
            let mut s = synth();
            let mut ev = event(SoundKind::Typed, 0.0, false);
            ev.gain = 0.8 / draw;
            s.push_meta(
                ev,
                EventMeta {
                    at_ms: 1_000,
                    ..EventMeta::default()
                },
            );
            s.v2_latched = latched;
            let y = render_all(&mut s, 40);
            (s, y)
        };
        let (a, ya) = probe(true);
        let (_, yb) = probe(false);
        let peak = peak_of(&yb);
        // Within ±1 dB of −15 dBFS and under the threshold, post soft-clip:
        // −14.3 dBFS here bounds the pre-clip peak under 0.2.
        assert!(
            peak > 10f32.powf(-16.0 / 20.0) && peak < 10f32.powf(-14.3 / 20.0),
            "the −15 dBFS probe must sit one decibel under the threshold (peak {peak}, law {minus_15_dbfs})"
        );
        assert!(
            ya.iter().zip(&yb).all(|(p, q)| p.to_bits() == q.to_bits()),
            "one decibel under the threshold the latched bus moved a bit — the knee is soft"
        );
        assert_eq!(
            (a.lim_g_l, a.lim_g_r),
            (1.0, 1.0),
            "the gain left 1.0 one decibel under the threshold"
        );

        // The knee, sample-exact: `limit` on a settled follower returns the
        // input's bits at ±15 dBFS and at the threshold itself (the follower's
        // test is strict); one part in a thousand over it the gain moves on
        // that sample. No tolerance: a hard knee.
        let k_att = 1.0 - (-1.0 / (SR * LIMIT_ATTACK_S)).exp();
        let k_rel = (-1.0 / (SR * LIMIT_RELEASE_S)).exp();
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

    /// A pause the render clock never saw still rests the phrase (§10.1, §16
    /// row 7). Unstamped, the melody reads the synth's clock, which advances
    /// only while the host renders; a host that pauses its queue must account
    /// the pause through `resume_after`. Three unstamped keys at 300 ms, a 5 s
    /// unrendered pause, one more key: accounted, it resolves the line and
    /// resets the IOI; unaccounted, it is the fourth key of the burst.
    #[test]
    fn a_pause_the_render_clock_never_saw_still_rests_the_phrase() {
        let drive = |account: bool| -> (u32, i8, f32, bool) {
            let mut s = synth();
            let mut buf = [0.0f32; 960];
            for _ in 0..3 {
                s.push(event(SoundKind::Typed, 0.0, false));
                for _ in 0..30 {
                    s.render(&mut buf);
                }
            }
            if account {
                s.resume_after(5.0);
            }
            s.push(event(SoundKind::Typed, 0.0, false));
            (s.v2.steps(), s.v2.walk(), s.v2.ioi_ms(), s.v2.lit())
        };
        let (steps, rested_walk, ioi, _) = drive(true);
        assert_eq!(steps, 4, "every key steps, pause or no pause");
        assert_eq!(ioi, IOI_DEFAULT_MS, "a ≥ 2 s gap must restart the IOI");
        let (steps, walk, ioi, _) = drive(false);
        assert_eq!(steps, 4, "every key steps, pause or no pause");
        assert_ne!(
            ioi, IOI_DEFAULT_MS,
            "the unaccounted pause must read as no pause at all — the control is broken"
        );
        assert_ne!(
            walk, rested_walk,
            "the accounted and unaccounted takes played the same note — the \
             rest never reached the melody, so this test proves nothing"
        );
        // A clock never runs back, and never takes a NaN.
        let mut s = synth();
        let before = s.clock_s;
        s.resume_after(f32::NAN);
        s.resume_after(-3.0);
        assert_eq!(
            s.clock_s, before,
            "a non-finite or negative pause moved the clock"
        );
    }

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
    /// sounded bell (§12.3, §15.2, A18). An arm with no move damps at its ttl
    /// over 12 ms; an arm whose move is a 5-cell hop fizzles under the nav tick;
    /// a claim re-targets the armed core (one core, no ttl, the true flight)
    /// with the bus continuous across the claim (checked on the boundary sample
    /// and the 1 ms RMS); a second meteor 40 ms later damps the first's
    /// sounding layers over 15 ms, cancels its unsounded bell, and leaves the
    /// second's bell to ring.
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
            armed.iter().all(|v| v.arm_ttl == MET_ARM_TTL_S),
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
            core.arm_ttl == 0.0 && core.damp > 0.0 && core.damp0 == LANE_FADE_STEAL_S,
            "an unclaimed arm must fizzle over the 12 ms ramp at its ttl"
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
                    v.damp > 0.0 && v.damp0 == METEOR_INTERRUPT_S,
                    "a sounding layer of the first meteor was not damped over 15 ms"
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

        // Typing on in words: the handback waits for a boundary, and a word
        // head is the derived line's boundary. The key changes once, to
        // neutral, on a key that opens a word. The keys continue the burst's
        // clock (`at_ms` is the melody's only time source), so the render time
        // that killed the riff opens no gap; a ≥ 900 ms gap would be a rest,
        // which would hand the key back before the word head under test.
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
    /// it forever (§10.2, §16 row 9). The word-boundary handback runs only on
    /// the v2 path, but the roster is read per event: audition "music box",
    /// switch to "marimba", let a riff die, and no v1 event would ever reach a
    /// v2 boundary. The first v1 trail event snaps the key (v1's law); a bonk
    /// on the same chain does not, so under the music box a held key stays
    /// held (A31).
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

    // -- §9.1: the isolated probe -------------------------------------------

    /// §9.1's isolated probe, as `typing_voice_ab`'s family rows measure a
    /// gesture: the energy-weighted centroid over 2048-sample Hann windows at a
    /// 1024 hop, from the gesture's first onset to 200 ms past it.
    fn probe_centroid_hz(x: &[f32]) -> f32 {
        const N: usize = 2048;
        let env: Vec<f32> = x.chunks(256).map(rms).collect();
        let peak = env.iter().fold(0.0f32, |m, v| m.max(*v));
        let from = env.iter().position(|e| *e > peak * 0.15).unwrap_or(0) * 256;
        let to = (from + SR as usize / 5).min(x.len());
        let win: Vec<f32> = (0..N)
            .map(|i| 0.5 * (1.0 - (core::f32::consts::TAU * i as f32 / N as f32).cos()))
            .collect();
        let mut num = 0.0f64;
        let mut den = 0.0f64;
        let mut s = from;
        while s + N <= to {
            let frame = &x[s..s + N];
            for k in 1..N / 2 {
                let mut re = 0.0f64;
                let mut im = 0.0f64;
                let w = core::f64::consts::TAU * k as f64 / N as f64;
                for (i, (v, h)) in frame.iter().zip(&win).enumerate() {
                    let a = w * i as f64;
                    let v = f64::from(*v * *h);
                    re += v * a.cos();
                    im -= v * a.sin();
                }
                let e = re * re + im * im;
                num += e * k as f64 * f64::from(SR) / N as f64;
                den += e;
            }
            s += N / 2;
        }
        if den <= 0.0 { 0.0 } else { (num / den) as f32 }
    }

    /// The isolated step's centroid is the one §9.1's partial table builds, on
    /// §9.1's probe (energy-weighted over the gesture's body).
    ///
    /// With energy ∝ lvl²·τ (a partial's decay multiplies the voice envelope),
    /// P1 carries almost all the energy, the octave and strike about 5 %, and
    /// the felt mallet a little spread over its sweep, which puts the centroid
    /// near 600-700 Hz. The pin is 550–800 Hz, so drift toward mud or toward
    /// glass fails. (`the_tine_is_small_and_warm_not_hard_and_bright` reads the
    /// strike and octave against v1 over 85 ms; this reads the whole body.)
    #[test]
    fn the_isolated_step_centroid_is_the_one_the_partial_table_builds() {
        // The tine's table, so the tine alone (see the bloom's own pin).
        let mut s = synth();
        s.set_v2_timbre_stops(TimbreStops::PLAIN);
        // One settling key, ~340 ms, then the probe — the family probe's
        // own stance.
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let _ = render_mono(&mut s, 34);
        push(&mut s, SoundKind::Typed, 1_340, 0.0, false);
        let x = render_mono(&mut s, 50);
        let c = probe_centroid_hz(&x);
        println!("isolated E5 step, §9.1 probe centroid: {c:.0} Hz");
        assert!(
            (550.0..=800.0).contains(&c),
            "the isolated v2 step's centroid is {c:.0} Hz on the §9.1 probe, outside the \
             550–800 Hz the partial table builds"
        );
    }

    // -- A14: the loudness arc --------------------------------------------

    /// Faster is brighter, never louder (§9.6, A14). Per-second TUNE energy
    /// (Σ(gl+gr)² over the pitched spawns of steady typing) sits within A14's
    /// +1 / −3 dB of the 4 cps reference at 8, 10, 15 and 20 cps. With every key
    /// a full step, `rate · g²` is flat by construction above the
    /// [`G_IOI_REF_S`] reference; the residual is the lit-versus-passing mix
    /// and the seeded velocity.
    ///
    /// The roof is non-decreasing in rate for every touch and lighting, and heat
    /// moves the roof and never the gain.
    #[test]
    fn faster_is_brighter_never_louder() {
        let energy_per_s = |cps: u32| -> f32 {
            let mut s = synth();
            let mut buf = [0.0f32; 960];
            let period = 1_000 / cps;
            let mut energy = 0.0f32;
            // Two seconds settle the IOI estimator; twenty are measured.
            for k in 0..(22 * cps) {
                let mark = s.born_seq;
                push(&mut s, SoundKind::Typed, 1_000 + k * period, 0.0, false);
                if k >= 2 * cps {
                    for v in tune_voices(&since(&s, mark)) {
                        if v.p[0].lvl > 0.0 {
                            energy += (v.gl + v.gr).powi(2);
                        }
                    }
                }
                for _ in 0..(period / 10).max(1) {
                    s.render(&mut buf);
                }
            }
            energy / 20.0
        };
        let reference = energy_per_s(4);
        assert!(reference > 0.0);
        let arc: Vec<(u32, f32)> = [8u32, 10, 15, 20]
            .into_iter()
            .map(|cps| (cps, 10.0 * (energy_per_s(cps) / reference).log10()))
            .collect();
        println!("A14 arc re 4 cps: {arc:?}");
        for (cps, db) in &arc {
            assert!(
                (-3.0..=1.0).contains(db),
                "{cps} cps: per-second TUNE energy is {db:+.2} dB re 4 cps, outside +1/−3; arc {arc:?}"
            );
        }
        for lit in [false, true] {
            for touch in [Touch::Step, Touch::ReStrike] {
                let mut last = 0.0f32;
                for cps in [2.0f32, 4.0, 6.0, 8.0, 10.0, 12.0, 15.0, 20.0] {
                    let roof = roof_hz(cps, lit, 0.5, 0.0, touch);
                    assert!(
                        roof >= last,
                        "lit {lit} / {touch:?}: the roof fell from {last} to {roof} Hz at {cps} cps"
                    );
                    last = roof;
                }
            }
        }
        let under_heat = |heat: f32| -> (f32, f32) {
            let mut s = synth();
            let mut ev = event(SoundKind::Typed, 0.0, false);
            ev.heat = heat;
            let mark = s.born_seq;
            s.push_meta(
                ev,
                EventMeta {
                    at_ms: 1_000,
                    ..EventMeta::default()
                },
            );
            let v = tune_voices(&since(&s, mark))[0];
            (v.gl + v.gr, v.lp_cut)
        };
        let (cold_gain, cold_roof) = under_heat(0.0);
        let (hot_gain, hot_roof) = under_heat(1.0);
        assert_eq!(cold_gain, hot_gain, "the glow's blaze bought a decibel");
        assert!(
            (hot_roof - cold_roof - ROOF_HEAT_HZ).abs() < 1e-3,
            "the blaze opened the roof by {} Hz, not {ROOF_HEAT_HZ}",
            hot_roof - cold_roof
        );
    }

    // -- §22: flow's voice ---------------------------------------------------

    /// The box opens with the hand and never gets louder (§22, §9.6, §21.4).
    ///
    /// It opens: on one word in isolation the ring-out (the 400-1200 ms window,
    /// with no onset in it) rises by at least 2 dB between heat 0 and heat 1:
    /// the bass dyad's longer decay and the octave echo are body.
    ///
    /// It never gets louder, on 30 s of the bench's prose at 4, 8 and 12 cps,
    /// cold and flowing:
    ///
    /// * the flowing take's RMS is at most 1 dB over the cold 4 cps reference
    ///   at any rate (§9.6);
    /// * the peak does not rise at any rate: the §9.1 ladder is written in
    ///   peak (`the_isolated_step_lands_on_the_ladder_floor` fits
    ///   [`KEY_TINE_TRIM`] on it).
    ///
    /// The flowing box measures slightly quieter than the cold one at every
    /// rate, and brighter (higher centroid, more energy over 2 kHz), because
    /// [`flow_partials`] conserves the strike's sum: flow adds body, not level.
    #[test]
    fn the_box_opens_with_the_hand_and_never_gets_louder() {
        // Every take this law reads is its own render from its own synth —
        // the one word's ring-out at five heats, 43 takes of the prose
        // corpus — so they render on the binary's threads first and the law
        // reads them in its own order below (serially it is 9.4 s, the lib
        // binary's longest test). The ring-out at heat 1 is read twice (the
        // box opens; the last rung) and rendered once: `word_ring_out` is a function of its heat alone,
        // a fresh `synth()` on `SEED` each call.
        #[derive(Clone, Copy, PartialEq)]
        enum Render {
            RingOut(f32),
            Prose(f32, f32),
            NoHeadroom(f32, f32),
            Seeded(f32, f32, bool, u32),
        }
        let rates = [4.0f32, 8.0, 12.0];
        // The 30 s prose takes first, the 1.5 s ring-outs last, so the short
        // renders fill the pool's tail.
        let mut renders: Vec<Render> = Vec::new();
        for flow in [0.0f32, 1.0, 0.25, 0.5, 0.75] {
            renders.extend(rates.map(|cps| Render::Prose(cps, flow)));
        }
        renders.push(Render::NoHeadroom(8.0, 1.0));
        for seed in [7u32, 0xA, 0xC] {
            for cps in rates {
                for (flow, headroom) in [(0.0, true), (1.0, true), (1.0, false)] {
                    renders.push(Render::Seeded(cps, flow, headroom, seed));
                }
            }
        }
        renders.extend([0.0f32, 1.0, 0.25, 0.5, 0.75].map(Render::RingOut));
        let read = par_map(&renders, |r| match *r {
            Render::RingOut(flow) => word_ring_out(flow),
            Render::Prose(cps, flow) => prose_loudness(cps, flow),
            Render::NoHeadroom(cps, flow) => prose_loudness_with_key_headroom(cps, flow, false),
            Render::Seeded(cps, flow, headroom, seed) => {
                prose_loudness_seeded(cps, flow, headroom, seed)
            }
        });
        let rendered = |r: Render| read[renders.iter().position(|&x| x == r).expect("rendered")];

        // -- it opens ----------------------------------------------------
        let (cold_peak, cold_body) = rendered(Render::RingOut(0.0));
        let (hot_peak, hot_body) = rendered(Render::RingOut(1.0));
        println!(
            "one word: cold peak {cold_peak:.2} body {cold_body:.2} | \
             flow peak {hot_peak:.2} body {hot_body:.2} dBFS"
        );
        assert!(
            hot_body - cold_body >= 2.0,
            "the box did not open: one word's 400-1200 ms ring-out moved {:+.2} dB \
             (cold {cold_body:.2} dBFS, flowing {hot_body:.2}), under the 2 dB the \
             bass decay and the octave echo are supposed to be worth",
            hot_body - cold_body
        );
        assert!(
            hot_peak <= cold_peak + PEAK_EPS_DB,
            "one word's PEAK rose {:+.2} dB in flow ({cold_peak:.2} -> {hot_peak:.2} dBFS)",
            hot_peak - cold_peak
        );
        // It opens gradually: flow is a lerp, so the ring-out must climb and
        // the peak must fall at every rung. Quarter heat decides this clause,
        // since the rise saturates early.
        let mut last = (cold_peak, cold_body);
        for heat in [0.25f32, 0.5, 0.75, 1.0] {
            let rung = rendered(Render::RingOut(heat));
            println!(
                "  heat {heat:.2}: peak {:.2} ring-out {:.2} dBFS",
                rung.0, rung.1
            );
            assert!(
                rung.1 > last.1 && rung.0 <= last.0 + PEAK_EPS_DB,
                "heat {heat}: the box did not open monotonically \
                 (peak {:.2} -> {:.2}, ring-out {:.2} -> {:.2} dBFS)",
                last.0,
                rung.0,
                last.1,
                rung.1
            );
            last = rung;
        }

        // -- it never gets louder ----------------------------------------
        let cold: Vec<(f32, f32)> = rates
            .iter()
            .map(|c| rendered(Render::Prose(*c, 0.0)))
            .collect();
        let hot: Vec<(f32, f32)> = rates
            .iter()
            .map(|c| rendered(Render::Prose(*c, 1.0)))
            .collect();
        for (i, cps) in rates.iter().enumerate() {
            println!(
                "{cps:>4} cps: rms {:.2} -> {:.2} ({:+.2} dB) | peak {:.2} -> {:.2} ({:+.2} dB)",
                cold[i].0,
                hot[i].0,
                hot[i].0 - cold[i].0,
                cold[i].1,
                hot[i].1,
                hot[i].1 - cold[i].1
            );
        }
        // §9.6's reference: the cold take at the arc's own 4 cps.
        let reference = cold[0].0;
        for (i, cps) in rates.iter().enumerate() {
            assert!(
                hot[i].0 - reference <= 1.0,
                "{cps} cps flowing: prose RMS is {:+.2} dB over the cold 4 cps \
                 reference ({reference:.2} dBFS), past §9.6's +1 dB",
                hot[i].0 - reference
            );
            assert!(
                hot[i].1 <= cold[i].1 + PEAK_EPS_DB,
                "{cps} cps: the prose PEAK rose {:+.2} dB in flow ({:.2} -> {:.2} dBFS) \
                 — speed may buy brightness and never a decibel (§9.6)",
                hot[i].1 - cold[i].1,
                cold[i].1,
                hot[i].1
            );
        }

        // A full-flow-only pass can hide an earlier overshoot. Check the
        // three intermediate heats against each rate's own cold peak too.
        for flow in [0.25, 0.5, 0.75] {
            for (i, cps) in rates.iter().enumerate() {
                let warm = rendered(Render::Prose(*cps, flow));
                println!(
                    "{cps} cps heat {flow}: RMS {:.3}, peak {:.3}, delta {:+.3} dB",
                    warm.0,
                    warm.1,
                    warm.1 - cold[i].1
                );
                assert!(
                    warm.0 - reference <= 1.0,
                    "{cps} cps heat {flow}: RMS exceeded the reference"
                );
                assert!(
                    warm.1 <= cold[i].1 + PEAK_EPS_DB,
                    "{cps} cps heat {flow}: peak rose {:+.3} dB",
                    warm.1 - cold[i].1
                );
            }
        }
        // THE NEGATIVE CONTROL. One take's over-peak without the allowance is
        // a lottery of that take's seeded phases, and any change to the synth
        // (the marks' pitched pluck did) re-rolls it; so the control is a
        // census over seeds, not one take. On the law's own seed no rate and
        // no heat over-peaks without the allowance (measured: -0.12 .. -0.74
        // dB at 4 / 8 / 12 cps x heat 0.25 / 0.5 / 0.75 / 1); it is other
        // seeds the allowance holds. Measured over 12 seeds x 3 rates at full
        // heat, WITHOUT -> WITH the allowance, re each take's own cold peak:
        // seed 7 at 4 cps +0.391 -> +0.018, seed 0xA at 4 cps +0.095 ->
        // -0.279, seed 0xC at 12 cps +0.102 -> -0.294. The census asks for a
        // take the allowance holds under the law and its removal puts over
        // it. It does not hold EVERY seed (seed 8 at 8 cps reads +0.48 with
        // it), as the constant's doc states.
        let no_headroom = rendered(Render::NoHeadroom(8.0, 1.0));
        println!(
            "law's seed without the allowance: 8 cps peak {:.3}, delta {:+.3} dB",
            no_headroom.1,
            no_headroom.1 - cold[1].1
        );
        let mut held = 0usize;
        for seed in [7u32, 0xA, 0xC] {
            for cps in rates {
                let cold = rendered(Render::Seeded(cps, 0.0, true, seed)).1;
                let with = rendered(Render::Seeded(cps, 1.0, true, seed)).1;
                let without = rendered(Render::Seeded(cps, 1.0, false, seed)).1;
                assert!(
                    without > with,
                    "seed {seed:#x} {cps} cps: removing the allowance did not raise the peak"
                );
                let hit = without > cold + PEAK_EPS_DB && with <= cold + PEAK_EPS_DB;
                println!(
                    "control seed {seed:#x} {cps:>4} cps: without {:+.3} with {:+.3} dB re cold{}",
                    without - cold,
                    with - cold,
                    if hit {
                        "  <- held by the allowance"
                    } else {
                        ""
                    }
                );
                held += usize::from(hit);
            }
        }
        assert!(
            held >= 1,
            "negative control: removing the allowance must reproduce an over-peak \
             that the allowance holds, on at least one take of the census"
        );
    }

    /// §9.6's arc binds the sparkle too: the per-key glint's energy per second
    /// may not rise with the typing rate.
    ///
    /// `g_IOI = clamp(√(IOI/0.25), 0.45, 1)` makes energy per event fall in
    /// proportion to the gap, keeping `rate × energy` flat; every per-key voice
    /// takes it, including [`v2_glint_at`](TrailSynth::v2_glint_at)'s caller.
    /// Measured per lane with [`lane_loudness`] at 4 / 10 / 20 cps: the tune
    /// and bloom fall with rate (the arc and §9.1's shortening τ_v), and the
    /// glint stays flat. `g_ioi(0.25) == 1.0`, so 4 cps is unaffected.
    ///
    /// It takes `g`, not `plan.level` (§9.2's passing-note attenuation): a
    /// passing note gets the full sparkle.
    #[test]
    fn the_sparkle_is_under_the_loudness_arc() {
        let rates = [4.0f32, 10.0, 20.0];
        let glint: Vec<f32> = rates
            .iter()
            .map(|c| lane_loudness(*c, 0.0, LANE_GLINT))
            .collect();
        let tune: Vec<f32> = rates
            .iter()
            .map(|c| lane_loudness(*c, 0.0, LANE_TUNE))
            .collect();
        for (i, cps) in rates.iter().enumerate() {
            println!(
                "{cps:>5} cps: glint {:.2}  tune {:.2}  (glint is {:.2} dB under the line)",
                glint[i],
                tune[i],
                tune[i] - glint[i]
            );
        }
        // The law: a rise is the defect; the epsilon is there so the script's
        // own fixed pauses (which do not scale with `cps`) cannot fail it.
        for i in 1..rates.len() {
            assert!(
                glint[i] <= glint[i - 1] + 0.25,
                "the sparkle's per-second energy ROSE {:+.2} dB from {} to {} cps \
                 ({:.2} -> {:.2} dBFS): it is outside §9.6's arc",
                glint[i] - glint[i - 1],
                rates[i - 1],
                rates[i],
                glint[i - 1],
                glint[i]
            );
        }
        // The balance: the gap between the line and its sparkle still narrows
        // with rate, because §9.1's masking law shortens τ_v as the hand speeds
        // up and a 40 ms glint has no ring to shorten. The arc owed only the
        // rise; this clause keeps the gap from collapsing again.
        for (i, cps) in rates.iter().enumerate() {
            assert!(
                tune[i] - glint[i] >= 25.0,
                "at {cps} cps the sparkle is only {:.2} dB under the line \
                 (25.25 dB with the arc applied, 18.84 dB without it)",
                tune[i] - glint[i]
            );
        }
    }

    /// The flow echo's instrument: one lit key in flow, and the RMS dBFS of the
    /// window 25–175 ms behind it, where the echo lives (it opens at
    /// [`FLOW_ECHO_DELAY_S`], `dur` `3τ_v + 20 ms`).
    ///
    /// `trim` scales the echo's gain: 0.0 renders the same take with the echo
    /// silent (same four rng draws, slot and lane census), so the difference
    /// between two takes is the echo alone. Not spawning it would re-roll the
    /// seeded stream behind it.
    fn one_key_body(seed: u32, trim: f32) -> f32 {
        let mut s = TrailSynth::new(SR, seed);
        s.echo_trim = trim;
        let mut buf = [0.0f32; 96];
        let mut out: Vec<f32> = Vec::new();
        push_meta_ch(&mut s, SoundKind::Typed, 1_000, 'e', 1.0);
        for _ in 0..400u32 {
            s.render(&mut buf);
            out.extend_from_slice(&buf);
        }
        let lo = (0.025 * SR) as usize * 2;
        let hi = (0.175 * SR) as usize * 2;
        20.0 * rms_of(&out[lo..hi]).log10()
    }

    /// The echo never cancels the octave it reinforces ([`FLOW_ECHO_PHASE`]).
    /// The echo's fundamental is exactly the strike's [`P2_RATIO`] partial, so
    /// the two interfere. Across 48 seeds against a bit-identical silent-echo
    /// control:
    ///
    /// ```text
    ///   phase      mean      sd    worst key
    ///   drawn    +0.231   0.152      -0.056
    ///   0.50     -0.000   0.058      -0.077   (antiphase: nothing at all)
    ///   0.75     +0.244   0.055      +0.167   (this)
    /// ```
    ///
    /// Every key must get body from the echo, and the spread must be small
    /// enough to be a property of the instrument rather than the seed. The
    /// mean is not asserted: a louder echo (in phase) is refused by §9.6, whose
    /// guard is `the_box_opens_with_the_hand_and_never_gets_louder`.
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

    #[test]
    fn the_flow_echo_always_adds_to_the_octave() {
        let mut d: Vec<f32> = Vec::new();
        for k in 0..48u32 {
            let seed = 0x1000_0001u32.wrapping_mul(k.wrapping_add(1)) ^ (k << 7);
            d.push(one_key_body(seed, 1.0) - one_key_body(seed, 0.0));
        }
        let n = d.len() as f32;
        let mean = d.iter().sum::<f32>() / n;
        let sd = (d.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n).sqrt();
        let (mn, mx) = d
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), x| (a.min(*x), b.max(*x)));
        println!(
            "the echo is worth {mean:+.3} dB of note body (sd {sd:.3}, \
             worst key {mn:+.3}, best {mx:+.3}) over {n} seeds"
        );
        assert!(
            mn > 0.10,
            "the echo took body OFF its own note on some key: worst {mn:+.3} dB \
             (mean {mean:+.3}) — it is interfering with the strike's P2 octave \
             at an unfixed phase"
        );
        assert!(
            sd < 0.09,
            "the echo's body is a lottery: sd {sd:.3} dB over {n} seeds \
             ({mn:+.3} to {mx:+.3})"
        );
    }

    const PEAK_EPS_DB: f32 = 0.05;

    /// Flow's voice is the identity at heat zero (§22). At heat 0 every
    /// flow-priced quantity is its constant and flow's own voice does not
    /// exist: no slot is claimed, no seeded draw made, no sample moved.
    ///
    /// 1. [`flow_partials`] at 0 is exactly `(P1_LVL, P2_LVL)` (bit equality:
    ///    `lerp(a, b, 0.0)` must return `a`);
    /// 2. the downbeat's decay and duration at heat 0 are exactly
    ///    [`BASS_DECAY_S`] and its tail-law `dur`;
    /// 3. a lit step at heat 0 spawns exactly one [`LANE_BLOOM`] voice (the
    ///    bloom), where the same step in flow spawns two;
    /// 4. a 1.5 s render of a word is bit-identical between an explicitly zero
    ///    side-car and one that never stamped the field, while the same script
    ///    in flow is not.
    ///
    /// `music_box_golden::ORACLE_SCRIPT_FOLD` and `BRRRRING_FOLD` cover the
    /// same claim at whole-render scale.
    #[test]
    fn flow_s_voice_is_identity_at_heat_zero() {
        // 1 — the partials.
        assert_eq!(flow_partials(0.0), (P1_LVL, P2_LVL));
        assert_eq!(flow_partials(1.0).1, FLOW_P2_LVL);
        assert!(
            (flow_partials(1.0).0 + flow_partials(1.0).1 - P1_LVL - P2_LVL).abs() < 1e-7,
            "the strike's sum is not conserved at heat 1: {:?}",
            flow_partials(1.0)
        );

        // 2 — the downbeat.
        let bass_of = |flow: f32| -> (f32, f32) {
            let mut s = synth();
            let mark = s.born_seq;
            s.push_meta(
                event(SoundKind::Space, 0.0, false),
                EventMeta {
                    at_ms: 1_000,
                    flow,
                    ..EventMeta::default()
                },
            );
            let v = since(&s, mark)
                .into_iter()
                .find(|v| v.lane == LANE_BASS)
                .expect("the word head plays a bass dyad");
            (v.decay, v.dur)
        };
        assert_eq!(
            bass_of(0.0),
            (BASS_DECAY_S, TAIL_DUR_PER_TAU * BASS_DECAY_S)
        );
        let (hot_decay, hot_dur) = bass_of(1.0);
        assert!((hot_decay - FLOW_BASS_DECAY_S).abs() < 1e-7);
        assert!((hot_dur - TAIL_DUR_PER_TAU * FLOW_BASS_DECAY_S).abs() < 1e-6);

        // 3 — the echo is structurally absent from the cold box.
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
            "the flowing step spawns the bloom and the echo"
        );

        // 4 — the whole render.
        let take = |meta: fn(u32, char) -> EventMeta| -> Vec<f32> {
            let mut s = synth();
            let mut buf = [0.0f32; 960];
            let mut out = Vec::new();
            let script = [(1_000u32, 't'), (1_100, 'h'), (1_200, 'e'), (1_300, ' ')];
            let mut k = 0usize;
            for b in 0..150u32 {
                let now = 1_000 + b * 10;
                while k < script.len() && script[k].0 <= now {
                    let (at, ch) = script[k];
                    let kind = if ch == ' ' {
                        SoundKind::Space
                    } else {
                        SoundKind::Typed
                    };
                    s.push_meta(event(kind, 0.0, false), meta(at, ch));
                    k += 1;
                }
                s.render(&mut buf);
                out.extend_from_slice(&buf);
            }
            out
        };
        let unstamped = take(|at, ch| EventMeta {
            at_ms: at,
            rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
            ..EventMeta::default()
        });
        let cold = take(|at, ch| EventMeta {
            at_ms: at,
            rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
            flow: 0.0,
            ..EventMeta::default()
        });
        let hot = take(|at, ch| EventMeta {
            at_ms: at,
            rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
            flow: 1.0,
            ..EventMeta::default()
        });
        assert_eq!(
            cold, unstamped,
            "a side-car stamped flow 0 rendered a different waveform from one that \
             stamped nothing — the identity default is not the identity"
        );
        assert_ne!(
            hot, cold,
            "flow 1 rendered the cold waveform: the levers are not wired"
        );
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
        // THE LAW, structurally: after the second tick nothing at that pitch
        // is left sounding undamped in the tune lane.
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

        // The law, audibly: the second tick's own window peaks within a narrow
        // band of a lone tick's at every cadence a hand or key repeat can
        // produce.
        fn second_window_peak(seed: u32, pair: bool, delta_ms: usize) -> f32 {
            let mut s = TrailSynth::new(SR, seed);
            let mut buf = [0.0f32; 96]; // 1 ms of stereo @ 48 kHz
            if pair {
                s.push(event(SoundKind::Navigation, 0.0, false));
                for _ in 0..delta_ms {
                    s.render(&mut buf);
                }
            }
            s.push(event(SoundKind::Navigation, 0.0, false));
            let mut peak = 0.0f32;
            // The tick's whole life: NAV_DUR_S = 2.7 × 30 ms = 81 ms.
            for _ in 0..90 {
                s.render(&mut buf);
                for sample in buf {
                    peak = peak.max(sample.abs());
                }
            }
            peak
        }
        for delta in [16usize, 33, 66] {
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for k in 0..32u32 {
                let seed = 0x1000_0001u32.wrapping_mul(k + 1) ^ 0x5EED_BEEF;
                let solo = second_window_peak(seed, false, delta);
                let pair = second_window_peak(seed, true, delta);
                let db = 20.0 * (pair / solo).log10();
                lo = lo.min(db);
                hi = hi.max(db);
            }
            // The damped tail leaves at most a small sum, and a re-struck tick
            // is never quieter than a lone one.
            assert!(
                lo >= -1.0 && hi <= 3.0,
                "{delta} ms pair: the re-struck tick swings {lo:.2} … {hi:.2} dB \
                 against a lone tick — that is a comb, not a re-strike"
            );
        }
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

    /// Mean-square level per DFT bin of a Hann-windowed slice, one-sided and
    /// normalised by the window's power: a sum over a band is that band's
    /// mean-square level re full scale, and a ratio of sums is an energy
    /// fraction. Naive, like [`centroid_hz`]; callers pass 4096 samples a few
    /// times.
    fn bin_powers(x: &[f32]) -> Vec<f64> {
        let n = x.len();
        let win: Vec<f64> = (0..n)
            .map(|i| 0.5 * (1.0 - (core::f64::consts::TAU * i as f64 / n as f64).cos()))
            .collect();
        let wsum: f64 = win.iter().map(|w| w * w).sum();
        let xs: Vec<f64> = x.iter().zip(&win).map(|(s, h)| f64::from(*s) * h).collect();
        let tw = dft_twiddles(n);
        (0..n / 2)
            .map(|k| {
                let (re, im) = dft_bin(&xs, &tw, k);
                2.0 * (re * re + im * im) / (n as f64 * wsum)
            })
            .collect()
    }

    /// The bin index of `hz` in an `n`-sample transform.
    fn bin_of(hz: f32, n: usize) -> usize {
        (hz * n as f32 / SR) as usize
    }

    /// Forte is the same note struck harder: brighter at the onset, mellow in
    /// the body, longer, and no structural peak ([`forte`]).
    ///
    /// One tine built by [`tine`], and the same tine handed to [`forte`] at each
    /// force of the table ([`Forte::of`]: 0.35 / 0.6 / 0.8 / 1.0), spawned
    /// alone into a silent synth at one gain, so the twin is on the same pitch
    /// and two takes differ only by `forte`. Three degrees (floor, middle,
    /// ceiling), three seeds.
    ///
    /// Structural, exact: every pitch, the attack, the mallet and `Σ p.lvl` are
    /// what `tine` built; nothing glides; the roof is still a filter
    /// (< 7639 Hz).
    ///
    /// Rendered, against the force-0 twin:
    ///
    /// - F4, brighter where a hammer is: centroid over the first 40 ms;
    /// - F5, and it mellows: centroid over 120-200 ms;
    /// - F6: the forte note's own onset-to-late centroid ratio;
    /// - F7, it sustains: time to −20 dB;
    /// - F8, monotone in force: onset centroid, time to −20 dB and the sample
    ///   peak (on one lock angle, [`FORTE_FM_TURN`]) never fall as force rises;
    /// - F9 / F10, not harsh: energy over 6 kHz, and the loudest bin over
    ///   8 kHz re the loudest bin;
    /// - F11, nothing reaches Nyquist: 19-24 kHz at the `song_key` ceiling
    ///   (degree 8 + 4, 2616 Hz), where the index has fallen per
    ///   [`FORTE_FM_REF_HZ`].
    #[test]
    fn forte_is_brighter_at_the_onset_mellow_in_the_body_and_costs_no_structural_peak() {
        const SEEDS: [u32; 3] = [SEED, 0x5EED_1234, 0xCAFE_F00D];
        const FORCES: [f32; 5] = [0.0, 0.35, 0.6, 0.8, 1.0];
        const TAU_S: f32 = TAU_V_MAX_S;
        /// The lit roof of a conversational hand: 4200 + 2300.
        const ROOF_HZ: f32 = ROOF_PLAIN_LO_HZ + ROOF_LIT_ADD_HZ;
        /// F4 at force 1. The worst case is degree 0 (C5), where the felt
        /// mallet's chirp, common to both takes, sits above the hammer's 3f / 5f
        /// and dilutes the centroid; a larger index would break the −50 dB
        /// over-8 kHz law and the crest pin ([`FORTE_FM_REF_HZ`]). At 0.6 the
        /// pin is the ×1.10 hard floor.
        const ONSET_BRIGHTER_FULL: f32 = 1.15;
        const ONSET_BRIGHTER_RUN: f32 = 1.10;
        /// F5: the body stays within this of the twin's (the octave forte fed
        /// is still ringing).
        const BODY_MELLOW_CEIL: f32 = 1.20;
        /// F6: the forte note's onset centroid over its own body's.
        const ONSET_OVER_BODY_FLOOR: f32 = 1.2;
        /// F7: time to −20 dB at force 1 over the twin's, from the key.
        const SUSTAIN_FLOOR: f32 = 1.25;
        /// F8, the peak: never more than 0.25 dB under the force below.
        const PEAK_STEP_FLOOR: f32 = 0.9716;
        const OVER_6K_CEIL: f64 = 0.04;
        const OVER_8K_CEIL_DB: f64 = -50.0;
        const NYQUIST_BAND_CEIL_DBFS: f64 = -70.0;
        let gain = VOL * KEY_TINE_TRIM * crate::trail_sound::SHIFT_GLYPH_GAIN * WORD_CAPITAL_GAIN;
        let build = |f: f32, force: f32| -> Voice {
            let mut v = tine(f, Touch::Step, TAU_S, ROOF_HZ, false, 0.0);
            if force > 0.0 {
                forte(&mut v, f, force);
            }
            v
        };
        // Spawned and locked as the product does it ([`Forte::of`]'s
        // `fm_turn`: force 1 here is the word-opening capital's).
        let render_at = |seed: u32, v: Voice, turn: f32| -> Vec<f32> {
            let mut s = TrailSynth::new(SR, seed);
            let who = s.v2_spawn(v, gain, 0.0);
            assert!(who.is_some(), "an empty pool admits one voice");
            s.v2_lock_hammer(who, turn);
            render_mono(&mut s, 40)
        };
        let render = |seed: u32, v: Voice, force: f32| -> Vec<f32> {
            let turn = if force == 1.0 {
                FORTE_FM_TURN_HEAD
            } else {
                FORTE_FM_TURN
            };
            render_at(seed, v, turn)
        };
        // Time from the key to the first 2 ms that is 20 dB under the loudest
        // 2 ms. From the key, not from the loudest chunk: a flat-topped onset
        // peaks a few ms later, which would make an equally long note read
        // shorter.
        let t20_ms = |x: &[f32]| -> f32 {
            let env: Vec<f32> = x.chunks(96).map(rms_of).collect();
            let (at, peak) = env
                .iter()
                .enumerate()
                .fold((0, 0.0f32), |m, (i, e)| if *e > m.1 { (i, *e) } else { m });
            let n = env[at..]
                .iter()
                .position(|e| *e < 0.1 * peak)
                .unwrap_or(env.len() - at);
            (at + n) as f32 * 2.0
        };
        let (mut f4_full, mut f4_run, mut f5, mut f6, mut f7) =
            (f32::MAX, f32::MAX, f32::MIN, f32::MAX, f32::MAX);
        let (mut f9, mut f10) = (f64::MIN, f64::MIN);
        for deg in [TUNE_DEG_LO, 4, TUNE_DEG_HI] {
            let f = penta(TINE_BASE_HZ, deg);
            let twin = build(f, 0.0);
            let sum = |v: &Voice| v.p[0].lvl + v.p[1].lvl + v.p[2].lvl;
            for force in FORCES {
                let v = build(f, force);
                // -- structural ------------------------------------------------
                assert!(
                    (sum(&v) - sum(&twin)).abs() < 1e-6,
                    "force {force}: Σ p.lvl moved {} -> {}",
                    sum(&twin),
                    sum(&v)
                );
                assert_eq!(
                    v.n_lvl, twin.n_lvl,
                    "force {force}: the mallet is untouched"
                );
                assert_eq!(
                    v.attack, twin.attack,
                    "force {force}: the 4 ms attack is law 1"
                );
                assert_eq!(
                    v.p.map(|p| (p.f0, p.glide)),
                    twin.p.map(|p| (p.f0, p.glide)),
                    "force {force}: forte moved a pitch"
                );
                assert_eq!(v.p[2].lvl, twin.p[2].lvl, "force {force}: the 2.76f strike");
                assert!(v.lp_cut < 7_639.0 && v.lp_cut >= twin.lp_cut);
                assert!(v.decay >= twin.decay && v.decay <= FORTE_TAU_MAX_S);
                if force > 0.0 {
                    assert_eq!(v.p[0].fm_ratio, FORTE_FM_RATIO);
                    assert!(v.p[0].fm_i0 <= FORTE_FM_INDEX * force);
                    assert!(v.p[1].lvl > twin.p[1].lvl && v.p[1].lvl <= FORTE_P2_MAX);
                } else {
                    assert_eq!(v.p[0].fm_ratio, 0.0, "force 0 is the tine, untouched");
                }
            }
            // -- rendered --------------------------------------------------------
            for seed in SEEDS {
                let takes: Vec<Vec<f32>> = FORCES
                    .iter()
                    .map(|k| render(seed, build(f, *k), *k))
                    .collect();
                let onset: Vec<f32> = takes.iter().map(|x| centroid_hz(&x[..1_920])).collect();
                let body: Vec<f32> = takes
                    .iter()
                    .map(|x| centroid_hz(&x[5_760..9_600]))
                    .collect();
                let t20: Vec<f32> = takes.iter().map(|x| t20_ms(x)).collect();
                // The peak is read on one lock angle across the forces (the
                // spiked one every force under 1 uses). The product's force-1
                // key is struck flat-topped so its peak does not follow
                // ([`FORTE_FM_TURN_HEAD`]); its loudness is [`Forte::gain`].
                let peak: Vec<f32> = FORCES
                    .iter()
                    .map(|k| peak_of(&render_at(seed, build(f, *k), FORTE_FM_TURN)))
                    .collect();
                println!(
                    "deg {deg} seed {seed:#x}: onset {onset:.0?} Hz, body {body:.0?} Hz, t20 {t20:.0?} ms, peak {peak:.4?}"
                );
                f4_full = f4_full.min(onset[4] / onset[0]);
                f4_run = f4_run.min(onset[2] / onset[0]);
                f5 = f5.max(body[4] / body[0]);
                f6 = f6.min(onset[4] / body[4]);
                f7 = f7.min(t20[4] / t20[0]);
                for k in 1..FORCES.len() {
                    assert!(
                        onset[k] >= onset[k - 1] * 0.995 && t20[k] >= t20[k - 1],
                        "deg {deg} seed {seed:#x}: force {} is not at least force {}: onset \
                         {onset:.0?} Hz, t20 {t20:.0?} ms",
                        FORCES[k],
                        FORCES[k - 1]
                    );
                    // F8's peak leg: a harder blow never peaks under the
                    // unstruck tine, and from one force to the next the peak
                    // falls by no more than the phase lottery of three partials
                    // and a mallet.
                    assert!(
                        peak[k] >= peak[0] && peak[k] >= peak[k - 1] * PEAK_STEP_FLOOR,
                        "deg {deg} seed {seed:#x}: the peak fell with the force: {peak:.4?}"
                    );
                }
                if seed == SEED {
                    let p = bin_powers(&takes[4][..4_096]);
                    let total: f64 = p.iter().sum();
                    let over6: f64 = p[bin_of(6_000.0, 4_096)..].iter().sum();
                    let loud = p.iter().copied().fold(0.0f64, f64::max);
                    let over8 = p[bin_of(8_000.0, 4_096)..]
                        .iter()
                        .copied()
                        .fold(0.0f64, f64::max);
                    f9 = f9.max(over6 / total);
                    f10 = f10.max(10.0 * (over8 / loud).log10());
                    println!(
                        "deg {deg}: over 6 kHz {:.4}, loudest bin over 8 kHz {:.1} dB",
                        over6 / total,
                        10.0 * (over8 / loud).log10()
                    );
                }
            }
        }
        println!(
            "forte vs its force-0 twin, worst case: onset centroid ×{f4_full:.3} at force 1, \
             ×{f4_run:.3} at 0.6; body ×{f5:.3}; onset/body ×{f6:.2}; t20 ×{f7:.2}; over 6 kHz \
             {f9:.4}; loudest bin over 8 kHz {f10:.1} dB re the loudest"
        );
        assert!(f4_full >= ONSET_BRIGHTER_FULL && f4_run >= ONSET_BRIGHTER_RUN);
        assert!(f5 <= BODY_MELLOW_CEIL, "the body stayed bright: ×{f5:.3}");
        assert!(
            f6 >= ONSET_OVER_BODY_FLOOR,
            "the onset is not the bright part: ×{f6:.2}"
        );
        assert!(f7 >= SUSTAIN_FLOOR, "forte did not sustain: ×{f7:.2}");
        assert!(f9 <= OVER_6K_CEIL, "{f9:.4} of a forte note is over 6 kHz");
        assert!(
            f10 <= OVER_8K_CEIL_DB,
            "a bin over 8 kHz is {f10:.1} dB re the loudest"
        );
        // F11 — the ceiling of everything a key can strike: degree 8 under
        // `song_key` +4, force 1.
        let top = penta(TINE_BASE_HZ, TUNE_DEG_HI + 4);
        let p = bin_powers(&render(SEED, build(top, 1.0), 1.0)[..4_096]);
        let band: f64 = p[bin_of(19_000.0, 4_096)..].iter().sum();
        let band_dbfs = 10.0 * band.max(1e-30).log10();
        println!("19-24 kHz under a forte {top:.0} Hz: {band_dbfs:.1} dBFS");
        assert!(
            band_dbfs <= NYQUIST_BAND_CEIL_DBFS,
            "{band_dbfs:.1} dBFS sits in 19-24 kHz under a forte {top:.0} Hz strike"
        );
    }

    /// A shifted key is forte, not higher, everywhere, and the capital that
    /// opens a word is the loudest key in it.
    ///
    /// 1. Not higher: the struck fundamental is the line's degree exactly
    ///    (`penta(walk + key)`, not twice it), and nothing the key spawns
    ///    glides.
    /// 2. Struck by the table ([`Forte::of`]): the hammer's FM at the force its
    ///    context earns: 1.0 at a word head (including a session's first key
    ///    and a Caps Lock word head), 0.8 where a capital opens a shifted run
    ///    mid-word, 0.6 inside the run, 0.35 on a shifted mark, none on a
    ///    re-strike. How force sounds is pinned by
    ///    `forte_is_brighter_at_the_onset_mellow_in_the_body_and_costs_no_structural_peak`.
    /// 3. Louder: the TUNE gain's ratio to the plain twin is exactly the weight
    ///    ([`super::SHIFT_GLYPH_GAIN`] on a letter, × [`WORD_CAPITAL_GAIN`] at a
    ///    word head only; [`MARK_SHIFT_GAIN`] on a shifted mark), and the render
    ///    follows: a capital is ≥ +2.0 dB on the strike's first 80 ms of RMS,
    ///    ≥ +4.5 dB where it opens a word, with the sample peak holding the same
    ///    floors (because the hammer is phase-locked; the drawn angle is the
    ///    failing control); a shifted mark is its +1.0 dB.
    /// 4. Only an opener rings: a [`LANE_GRAFT`] voice [`CAP_RING_DELAY_S`]
    ///    behind the key exists iff the key is a capital letter opening a word
    ///    or a shifted run.
    ///
    /// Five contexts (a session's first key, a word head, mid-word, inside a
    /// shouted run, a word head under Caps Lock with no Shift press), each key
    /// typed shifted and not from the same line state, tails flushed, three
    /// seeds. Over the shouted pangram the sounded pitches are the walk's
    /// degrees, so the contour
    /// `a_run_of_capitals_keeps_its_contour_instead_of_stacking_on_the_ceiling`
    /// pins is what the ear gets.
    #[test]
    fn a_shifted_key_is_forte_not_higher_and_a_word_opening_capital_is_loudest() {
        const SEEDS: [u32; 3] = [SEED, 0x5EED_1234, 0xCAFE_F00D];
        const HEAD_BLOCKS: usize = 40;
        const TAKE_BLOCKS: usize = 12;
        const ONSET: usize = 3_840;
        const SHIFTED_FLOOR_DB: f32 = 2.0;
        const WORD_CAPITAL_FLOOR_DB: f32 = 4.5;
        /// The sample peak's floors are the energy floors (the design's F3, a
        /// hard floor), held because the hammer's modulator is locked
        /// ([`FORTE_FM_TURN_HEAD`] / [`FORTE_FM_TURN`]). Outside word heads the
        /// margin is very thin and not forte's to give: with `forte` off the
        /// same take falls short, because the plain twin sits on another degree
        /// under another noise draw. The drawn-angle control fails both.
        const SHIFTED_PEAK_FLOOR_DB: f32 = SHIFTED_FLOOR_DB;
        const WORD_CAPITAL_PEAK_FLOOR_DB: f32 = WORD_CAPITAL_FLOOR_DB;
        /// A shifted mark's rendered weight: +1.0 dB ([`MARK_SHIFT_GAIN`]) on the
        /// first 80 ms of RMS, within 0.8 dB: the mark's forte (force 0.35)
        /// holds its short voice a little longer, and the shifted twin sits on
        /// the walk's accented degree.
        const MARK_DB: f32 = 1.0;
        const MARK_TOL_DB: f32 = 0.8;
        // (context, the keys under test, is the key a word head, the force a
        // capital LETTER earns there, does it open something)
        const CONTEXTS: [(&str, &str, bool, f32, bool); 5] = [
            ("", "atw", true, 1.0, true),
            ("hello ", "awfbq?(:", true, 1.0, true),
            ("hel", "awfbq?(:", false, 0.8, true),
            ("say HEL", "leo", false, 0.6, false),
            ("SAY ", "hwt", true, 1.0, true),
        ];
        let db = |x: f32| 20.0 * x.log10();
        let head = |s: &mut TrailSynth, ctx: &str| {
            for (i, ch) in ctx.chars().enumerate() {
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(s, kind, 1_000 + i as u32 * 150, ch);
            }
        };
        // One take: the glyph `ch`, spelled shifted or not (a letter is its
        // capital; a mark is the same mark from a layout needing no Shift) →
        // (the TUNE voice, line Hz, has a ring, onset RMS, peak).
        let take_on = |seed: u32, ctx: &str, ch: char, shifted: bool, unlocked: bool| {
            let mut s = TrailSynth::new(SR, seed);
            s.hammer_unlocked = unlocked;
            head(&mut s, ctx);
            let _ = render_mono(&mut s, HEAD_BLOCKS);
            let glyph = if shifted { ch.to_ascii_uppercase() } else { ch };
            let mark = s.born_seq;
            s.push_meta(
                event(SoundKind::Typed, 0.0, shifted),
                EventMeta {
                    at_ms: 1_000 + ctx.chars().count() as u32 * 150,
                    glyph_class: crate::trail_sound::typed_glyph_class(Some(glyph)),
                    rank: crate::trail_sound::typed_glyph_rank(Some(glyph)),
                    ..EventMeta::default()
                },
            );
            let spawned = since(&s, mark);
            let v = *spawned
                .iter()
                .find(|v| v.lane == LANE_TUNE)
                .expect("every key is a step");
            for w in &spawned {
                assert_eq!(w.p.map(|p| p.glide), [0.0; 3], "`{ch}`: a voice bends");
            }
            let rings = s.v2.ring.is_some();
            let line = penta(TINE_BASE_HZ, i32::from(s.v2.walk()) + i32::from(s.song_key));
            let x = render_mono(&mut s, TAKE_BLOCKS);
            (v, line, rings, rms_of(&x[..ONSET]), peak_of(&x))
        };
        let take =
            |seed: u32, ctx: &str, ch: char, shifted: bool| take_on(seed, ctx, ch, shifted, false);
        let mut worst = (f32::MAX, f32::MAX, f32::MAX, f32::MAX, f32::MAX, f32::MIN);
        for (ctx, keys, word_head, letter_force, opens) in CONTEXTS {
            for ch in keys.chars() {
                let letter = ch.is_alphabetic();
                for seed in SEEDS {
                    let (v0, _, ring0, r0, p0) = take(seed, ctx, ch, false);
                    let (v1, line, ring1, r1, p1) = take(seed, ctx, ch, true);
                    let tag = format!("seed {seed:#x} `{ch}` after {ctx:?}");
                    let (g0, g1) = (
                        (v0.gl * v0.gl + v0.gr * v0.gr).sqrt(),
                        (v1.gl * v1.gl + v1.gr * v1.gr).sqrt(),
                    );
                    // 1 — Not higher: the line's own degree, to the bit.
                    assert_eq!(
                        v1.p[0].f0, line,
                        "{tag}: struck {} Hz over a line at {line} — not the line's degree",
                        v1.p[0].f0
                    );
                    assert!(
                        (v1.p[0].f0 / (2.0 * line) - 1.0).abs() > 0.4,
                        "{tag}: the 2026-09-19 octave is back"
                    );
                    // 2: struck by the table. A re-strike (`LL`) has a level-0
                    // strike partial and is never struck harder.
                    let restruck = v1.p[2].lvl == 0.0 && letter;
                    let force = if restruck {
                        0.0
                    } else if letter {
                        letter_force
                    } else {
                        0.35
                    };
                    let want_i0 = forte_index(v1.p[0].f0, force);
                    assert_eq!(
                        (v1.p[0].fm_i0, v0.p[0].fm_i0),
                        (want_i0, 0.0),
                        "{tag}: not the table's force {force}"
                    );
                    assert!(v1.lp_cut < 7_639.0, "{tag}: the roof is a bypass");
                    // 3 — Louder, and loudest where a capital opens a word.
                    let capital_head = word_head && letter;
                    let base = if letter {
                        crate::trail_sound::SHIFT_GLYPH_GAIN
                    } else {
                        MARK_SHIFT_GAIN
                    };
                    let weights = [base, base / PASSING_LEVEL].map(|w| {
                        if capital_head {
                            w * WORD_CAPITAL_GAIN
                        } else {
                            w
                        }
                    });
                    let gain_db = db(g1 / g0);
                    assert!(
                        weights.iter().any(|w| (g1 / g0 / w - 1.0).abs() < 1e-3),
                        "{tag}: the strike's gain is {gain_db:+.2} dB over the plain key — \
                         not the table's weight (word-opening capital: {capital_head})"
                    );
                    let (d, dp) = (db(r1 / r0), db(p1 / p0));
                    if letter {
                        let (floor, peak_floor) = if capital_head {
                            (WORD_CAPITAL_FLOOR_DB, WORD_CAPITAL_PEAK_FLOOR_DB)
                        } else {
                            (SHIFTED_FLOOR_DB, SHIFTED_PEAK_FLOOR_DB)
                        };
                        assert!(
                            d >= floor && dp >= peak_floor,
                            "{tag}: the capital is {d:+.2} dB (first 80 ms RMS) and {dp:+.2} dB \
                             (peak) over the plain key — under +{floor} / +{peak_floor}"
                        );
                        if capital_head {
                            worst.0 = worst.0.min(d);
                            worst.1 = worst.1.min(dp);
                        } else {
                            worst.2 = worst.2.min(d);
                            worst.3 = worst.3.min(dp);
                        }
                    } else {
                        // Against the gain's own ratio, so a passing plain
                        // twin (−2 dB) does not read as a louder mark.
                        let over = d - gain_db + MARK_DB;
                        assert!(
                            (over - MARK_DB).abs() <= MARK_TOL_DB,
                            "{tag}: the shifted mark renders {over:+.2} dB — not its \
                             +{MARK_DB} ± {MARK_TOL_DB}"
                        );
                        worst.4 = worst.4.min(over);
                        worst.5 = worst.5.max(over);
                    }
                    // 4: only an opener rings.
                    assert_eq!(
                        (ring1, ring0),
                        (letter && opens && !restruck, false),
                        "{tag}: the ring (shifted, plain)"
                    );
                }
            }
        }
        println!(
            "shifted over plain, worst case: a word-opening capital {:+.2} dB RMS / {:+.2} dB \
             peak, any other capital {:+.2} / {:+.2}, a shifted mark {:+.2}..{:+.2} dB",
            worst.0, worst.1, worst.2, worst.3, worst.4, worst.5
        );

        // Negative control: the hammer on its drawn angle (`hammer_unlocked`;
        // the lock draws nothing, so the takes differ by that angle only): the
        // peak floors do not hold.
        let mut drawn = (f32::MAX, f32::MAX);
        for (ctx, keys, word_head, _, _) in CONTEXTS {
            for ch in keys.chars().filter(|c| c.is_alphabetic()) {
                for seed in SEEDS {
                    let p0 = take_on(seed, ctx, ch, false, true).4;
                    let p1 = take_on(seed, ctx, ch, true, true).4;
                    if word_head {
                        drawn.0 = drawn.0.min(db(p1 / p0));
                    } else {
                        drawn.1 = drawn.1.min(db(p1 / p0));
                    }
                }
            }
        }
        println!(
            "…and on the DRAWN angle: a word-opening capital {:+.2} dB peak, any other {:+.2}",
            drawn.0, drawn.1
        );
        assert!(
            drawn.0 < WORD_CAPITAL_PEAK_FLOOR_DB && drawn.1 < SHIFTED_PEAK_FLOOR_DB,
            "negative control: the unlocked hammer holds the peak floors too ({:+.2} / {:+.2}) — \
             this pin cannot see the lock",
            drawn.0,
            drawn.1
        );

        // The shouted line keeps its contour in the ear: it is the walk.
        let mut s = synth();
        let mut sounded = Vec::new();
        let mut at = 1_000u32;
        for ch in "THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG".chars() {
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            let mark = s.born_seq;
            push_ch(&mut s, kind, at, ch);
            at += 110;
            if kind != SoundKind::Typed {
                continue;
            }
            let Some(v) = since(&s, mark).into_iter().find(|v| v.lane == LANE_TUNE) else {
                continue;
            };
            let p = v.p[0];
            if p.lvl <= 0.0 {
                continue;
            }
            let line = penta(TINE_BASE_HZ, i32::from(s.v2.walk()) + i32::from(s.song_key));
            assert_eq!(
                p.f0, line,
                "`{ch}`: a shouted key struck {} Hz over a line at {line}",
                p.f0
            );
            sounded.push((p.f0 * 10.0) as u32);
        }
        let mut pitches = sounded.clone();
        pitches.sort_unstable();
        pitches.dedup();
        let most = pitches
            .iter()
            .map(|p| sounded.iter().filter(|q| *q == p).count())
            .max()
            .unwrap_or(0);
        assert!(
            pitches.len() >= 5 && most * 3 < sounded.len(),
            "the shouted line sounded {} pitches, one of them {most} times in {}: a plateau",
            pitches.len(),
            sounded.len()
        );
    }

    /// A word-opening capital rings a twelfth over its own strike, measured from
    /// the sound. For capitals after "hello " whose word-head strikes land on
    /// C6, A5 and D6 (rings at exactly `3f`, and `40/27 × 2 = 2.963f` for D):
    /// the loudest pitch within ±20 % of the fundamental over the first 40 ms is
    /// the line's own note, and the loudest pitch between `2.4f` and `3.6f`
    /// over 150-400 ms (after the 2.76f strike partial and forte's 3f sideband
    /// have gone) is the twelfth, within a quarter semitone.
    ///
    /// Negative control: `ring_at_octave` leaves nothing at the twelfth.
    #[test]
    fn a_word_opening_capital_rings_a_twelfth_over_its_own_strike() {
        // Peak pitch of `x` in [lo, hi] Hz, 1 Hz steps, Hann-windowed.
        let peak = |x: &[f32], lo: f32, hi: f32| -> (f32, f32) {
            let n = x.len();
            let win: Vec<f32> = (0..n)
                .map(|i| 0.5 * (1.0 - (core::f32::consts::TAU * i as f32 / n as f32).cos()))
                .collect();
            // The windowed samples once, not once per probed frequency; the
            // scan below is a counted loop (no iterator adaptors per term).
            let xs: Vec<f64> = x
                .iter()
                .zip(&win)
                .map(|(&v, &h)| f64::from(v * h))
                .collect();
            let mut best = (0.0f32, 0.0f32);
            let mut f = lo;
            while f <= hi {
                let w = core::f32::consts::TAU * f / SR;
                let (mut re, mut im) = (0.0f64, 0.0f64);
                let mut i = 0;
                while i < n {
                    let ph = f64::from(w) * i as f64;
                    re += xs[i] * ph.cos();
                    im -= xs[i] * ph.sin();
                    i += 1;
                }
                let mag = ((re * re + im * im).sqrt() / n as f64) as f32;
                if mag > best.1 {
                    best = (f, mag);
                }
                f += 1.0;
            }
            best
        };
        let mut rang = 0;
        let mut levels = Vec::new();
        for ch in ['B', 'F', 'O', 'W', 'Z'] {
            let take = |at_octave: bool| -> Option<(f32, f32, f32, f32, f32)> {
                let mut s = synth();
                s.ring_at_octave = at_octave;
                s.set_v2_timbre_stops(TimbreStops::PLAIN);
                for (i, c) in "hello ".chars().enumerate() {
                    let kind = if c == ' ' {
                        SoundKind::Space
                    } else {
                        SoundKind::Typed
                    };
                    push_ch(&mut s, kind, 1_000 + i as u32 * 150, c);
                }
                let _ = render_mono(&mut s, 60);
                push_ch(&mut s, SoundKind::Typed, 2_500, ch);
                s.v2.ring?;
                let f = penta(TINE_BASE_HZ, i32::from(s.v2.walk()) + i32::from(s.song_key));
                let x = render_mono(&mut s, 40);
                let (strike_hz, strike_mag) = peak(&x[..1_920], 0.8 * f, 1.2 * f);
                let (ring_hz, ring_mag) = peak(&x[7_200..19_200], 2.4 * f, 3.6 * f);
                Some((
                    f,
                    strike_hz,
                    ring_hz,
                    20.0 * (ring_mag / strike_mag).log10(),
                    ring_mag,
                ))
            };
            let Some((f, strike_hz, ring_hz, ring_db, ring_mag)) = take(false) else {
                continue;
            };
            rang += 1;
            let twelfth = penta(
                TINE_BASE_HZ,
                i32::from({
                    let mut s = synth();
                    for (i, c) in "hello ".chars().enumerate() {
                        let kind = if c == ' ' {
                            SoundKind::Space
                        } else {
                            SoundKind::Typed
                        };
                        push_ch(&mut s, kind, 1_000 + i as u32 * 150, c);
                    }
                    push_ch(&mut s, SoundKind::Typed, 2_500, ch);
                    s.v2.walk()
                }) + FLOW_ECHO_OCTAVE_DEG
                    + CAP_RING_LIFT_DEG,
            );
            let cents = |a: f32, b: f32| (1200.0 * (a / b).log2()).abs();
            println!(
                "`{ch}`: strike {strike_hz:.1} Hz (line {f:.1}), ring {ring_hz:.1} Hz \
                 (twelfth {twelfth:.1}, x{:.3}), ring bin {ring_db:+.1} dB re the strike's onset bin",
                ring_hz / f
            );
            assert!(
                cents(strike_hz, f) < 25.0,
                "`{ch}`: the strike sounded {strike_hz} Hz over a line at {f} Hz"
            );
            assert!(
                cents(ring_hz, twelfth) < 25.0,
                "`{ch}`: the ring sounded {ring_hz} Hz, not the twelfth {twelfth} Hz"
            );
            levels.push(ring_db);
            let (_, _, octave_hz, _, octave_mag) = take(true).expect("the control rings too");
            let below = 20.0 * (ring_mag / octave_mag).log10();
            assert!(
                below >= 12.0,
                "negative control: with the ring on the octave the band's loudest pitch \
                 ({octave_hz} Hz) is only {below:.1} dB under the twelfth's — the \
                 measurement is not reading the ring"
            );
        }
        assert_eq!(rang, 5, "fixture: only {rang} of the five capitals rang");
        let (lo, hi) = levels
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), l| (lo.min(*l), hi.max(*l)));
        println!("ring bins {lo:+.1} .. {hi:+.1} dB re the strike's onset over {rang} capitals");
    }

    /// The capital's ring is a twelfth on its own phase, and a shifted key that
    /// does not ring still spends the ring's draws ([`Forte::ring`]).
    ///
    /// The ring is [`CAP_RING_LIFT_DEG`] past the octave (a twelfth over this
    /// `W`'s C), on no partial of the strike's tine, so it keeps its drawn phase
    /// with no lock: one DFT bin at the ring's frequency over 80-200 ms, divided
    /// by the TUNE voice's gain (so the seeded velocity cancels), spreads
    /// ≤ 1.0 dB over five seeds.
    ///
    /// Negative control: `ring_at_octave` (the same draws, the ring on the
    /// octave on its drawn phase) spreads ≥ 3 dB.
    ///
    /// The draws: from one cloned line state under `TimbreStops::PLAIN`, a
    /// capital opening a run (rings) and the same capital inside a run (does
    /// not) leave the seeded stream at the same point; an unshifted key, which
    /// never draws for a ring, leaves it elsewhere.
    #[test]
    fn the_capital_ring_is_a_twelfth_on_its_own_phase_and_its_draws_are_spent_either_way() {
        const SEEDS: [u32; 5] = [SEED, 0x5EED_1234, 0x504F_4F46, 0xCAFE_F00D, 0x0BAD_CAFE];
        const OWN_PHASE_SPREAD_CEIL_DB: f32 = 1.0;
        const OCTAVE_SPREAD_FLOOR_DB: f32 = 3.0;
        let ring_bin = |seed: u32, at_octave: bool| -> f32 {
            let mut s = TrailSynth::new(SR, seed);
            s.ring_at_octave = at_octave;
            s.set_v2_timbre_stops(TimbreStops::PLAIN);
            for (i, ch) in "hello ".chars().enumerate() {
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(&mut s, kind, 1_000 + i as u32 * 150, ch);
            }
            let _ = render_mono(&mut s, 60);
            push_ch(&mut s, SoundKind::Typed, 2_500, 'W');
            let lead = s.voices[usize::from(s.v2.lead.expect("one strike").0)];
            let (slot, _) = s.v2.ring.expect("a word-opening capital rings");
            let ring = s.voices[usize::from(slot)];
            let line = i32::from(s.v2.walk()) + i32::from(s.song_key);
            assert_eq!(
                lead.p[0].f0,
                penta(TINE_BASE_HZ, line),
                "the strike is the line's own note"
            );
            assert_eq!(ring.p[2].lvl, 0.0);
            if at_octave {
                assert_eq!(ring.p[0].f0, lead.p[1].f0, "control: the octave");
            } else {
                assert_eq!(
                    ring.p[0].f0,
                    penta(
                        TINE_BASE_HZ,
                        line + FLOW_ECHO_OCTAVE_DEG + CAP_RING_LIFT_DEG
                    ),
                    "the ring is the twelfth"
                );
                for p in lead.p.iter().filter(|p| p.lvl > 0.0) {
                    let cents = 1200.0 * (ring.p[0].f0 / p.f0).log2();
                    assert!(
                        cents.abs() > 100.0,
                        "the ring sits {cents:.0} cents from a strike partial at {} Hz",
                        p.f0
                    );
                }
            }
            let x = render_mono(&mut s, 20);
            let w = core::f32::consts::TAU * ring.p[0].f0 / SR;
            let body = &x[3_840..9_600];
            let (re, im) = body
                .iter()
                .enumerate()
                .fold((0.0f32, 0.0f32), |(re, im), (n, &v)| {
                    let ph = w * n as f32;
                    (re + v * ph.cos(), im - v * ph.sin())
                });
            let mag = (re * re + im * im).sqrt() / body.len() as f32;
            20.0 * (mag / (lead.gl * lead.gl + lead.gr * lead.gr).sqrt()).log10()
        };
        let spread = |at_octave: bool| -> f32 {
            let bins: Vec<f32> = SEEDS.iter().map(|s| ring_bin(*s, at_octave)).collect();
            let (lo, hi) = bins
                .iter()
                .fold((f32::MAX, f32::MIN), |(lo, hi), b| (lo.min(*b), hi.max(*b)));
            println!(
                "ring bin, at the octave {at_octave}: {bins:.2?} dB re the key — spread {:.2}",
                hi - lo
            );
            hi - lo
        };
        let (own, octave) = (spread(false), spread(true));
        assert!(
            own <= OWN_PHASE_SPREAD_CEIL_DB,
            "the twelfth's bin wanders {own:.2} dB over the seeds on its drawn phase"
        );
        assert!(
            octave >= OCTAVE_SPREAD_FLOOR_DB,
            "negative control: on the strike's octave the drawn ring wanders only \
             {octave:.2} dB — the instrument cannot see the lottery a shared partial is"
        );

        // The draws.
        let mut base = synth();
        base.set_v2_timbre_stops(TimbreStops::PLAIN);
        for (i, ch) in "hel".chars().enumerate() {
            push_ch(&mut base, SoundKind::Typed, 1_000 + i as u32 * 150, ch);
        }
        let _ = render_mono(&mut base, 60);
        let after = |prev_shifted: bool, ch: char| -> (u32, bool) {
            let mut s = base.clone();
            s.v2.text.prev_shifted = prev_shifted;
            push_ch(&mut s, SoundKind::Typed, 2_500, ch);
            (s.rng, s.v2.ring.is_some())
        };
        let (opener, rang) = after(false, 'P');
        let (interior, rang_inside) = after(true, 'P');
        let (plain, _) = after(false, 'p');
        assert!(rang && !rang_inside, "fixture: one rings and one does not");
        assert_eq!(
            opener, interior,
            "a capital that does not ring left the seeded stream somewhere else"
        );
        assert_ne!(
            opener, plain,
            "negative control: an unshifted key drew as much as a ringing capital"
        );
    }

    /// A plain key is the voice [`tine`] built; `forte` never touches it. Field
    /// by field and bit for bit, so routing the plain path through the
    /// reshaper fails here by name.
    #[test]
    fn a_plain_key_is_the_tine_forte_never_touched() {
        let mut s = synth();
        for (i, ch) in "hello wor".chars().enumerate() {
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            let mut prev = s.v2;
            let at = 1_000 + i as u32 * 150;
            push_ch(&mut s, kind, at, ch);
            if kind != SoundKind::Typed {
                continue;
            }
            let plan = prev.on_typed(
                at,
                crate::trail_sound::typed_glyph_rank(Some(ch)),
                false,
                false,
                LETTER,
            );
            assert!(!plan.opens_shift && !plan.steered);
            let ioi_s = s.v2.ioi_ms * 0.001;
            let tau = tau_v_s(ioi_s)
                * if plan.touch == Touch::Step {
                    1.0
                } else {
                    RESTRIKE_TAU_MUL
                };
            let lit_roof = plan.lit && (plan.touch == Touch::Step || plan.word_head);
            let roof = roof_hz(1.0 / ioi_s, lit_roof, 0.5, hue_arc(0.0), plan.touch);
            let f = penta(TINE_BASE_HZ, plan.deg + i32::from(s.song_key));
            let want = tine(f, plan.touch, tau, roof, false, 0.0);
            let got = s.voices[usize::from(s.v2.lead.expect("one strike").0)];
            let bits = |v: &Voice| {
                let mut b = vec![
                    v.dur.to_bits(),
                    v.attack.to_bits(),
                    v.decay.to_bits(),
                    v.n_lvl.to_bits(),
                    v.lp_cut.to_bits(),
                ];
                for p in v.p {
                    b.extend([
                        p.lvl.to_bits(),
                        p.f0.to_bits(),
                        p.f1.to_bits(),
                        p.glide.to_bits(),
                        p.fm_ratio.to_bits(),
                        p.fm_i0.to_bits(),
                        p.fm_tau.to_bits(),
                        p.decay.to_bits(),
                    ]);
                }
                b
            };
            assert_eq!(bits(&got), bits(&want), "`{ch}`: not the tine `tine` built");
        }
    }

    /// Backspace un-types the shift with the capital (`prev_shifted` is on the
    /// [`Undo`] frame, since it decides [`Forte::of`]'s force and the ring).
    /// Type `aB`, delete the `B`, type `B` again: the second `B` is the run's
    /// opener again (force 0.8, ringing), not an interior capital.
    #[test]
    fn backspace_restores_the_hammer() {
        let strike = |s: &TrailSynth| {
            let v = s.voices[usize::from(s.v2.lead.expect("one strike").0)];
            // Not the τ: the IOI's EMA is the hand's, off the frame by design.
            (v.p[0].f0, v.p[0].fm_i0, v.p[1].lvl, s.v2.ring.is_some())
        };
        let mut s = synth();
        push_ch(&mut s, SoundKind::Typed, 1_000, 'a');
        push_ch(&mut s, SoundKind::Typed, 1_200, 'B');
        let first = strike(&s);
        assert!(
            first.3 && first.1 > 0.0,
            "fixture: `B` opens a run — forte, ringing"
        );
        let _ = render_mono(&mut s, 30);
        push(&mut s, SoundKind::Backspace, 1_500, 0.0, false);
        assert!(
            !s.v2.text.prev_shifted,
            "the deleted capital's shift came back with it"
        );
        let _ = render_mono(&mut s, 30);
        push_ch(&mut s, SoundKind::Typed, 1_800, 'B');
        assert_eq!(
            strike(&s),
            first,
            "the retyped `B` is not the `B` that was deleted"
        );
        // Negative control: with the shift left set, the same retype is an
        // interior capital.
        let mut stale = synth();
        push_ch(&mut stale, SoundKind::Typed, 1_000, 'a');
        push_ch(&mut stale, SoundKind::Typed, 1_200, 'B');
        let _ = render_mono(&mut stale, 30);
        push(&mut stale, SoundKind::Backspace, 1_500, 0.0, false);
        stale.v2.text.prev_shifted = true;
        let _ = render_mono(&mut stale, 30);
        push_ch(&mut stale, SoundKind::Typed, 1_800, 'B');
        assert_ne!(
            strike(&stale),
            first,
            "the control cannot tell the two capitals apart"
        );
    }

    /// The ting and the capital it announces crest no higher than the recorded
    /// baseline, in every context (§9.6). A bare Shift, then a word-opening
    /// capital 60 ms later, in three contexts (a session's first key, a word
    /// head, a sentence head), eight letters, five seeds, host volume 0.4.
    ///
    /// Per context, against the recorded baseline (`BEFORE`):
    ///
    /// - the worst take is ≤ that context's baseline worst + 0.3 dB (F16's
    ///   second clause);
    /// - the median is ≤ its baseline median + 0.5 dB;
    /// - the context that met the −14.5 dBFS ceiling (the session's first key)
    ///   still meets it, measured on the ting and the capital together (F16's
    ///   first clause).
    ///
    /// Negative controls: `ting_unducked` puts the first key over −14.5
    /// ([`TING_DUCK`]); `hammer_unlocked` (the drawn angle) moves the first
    /// key's median up by more than `LOCK_MEDIAN_GAP_DB`
    /// ([`FORTE_FM_TURN_HEAD`]).
    ///
    /// The word head and sentence head do not meet −14.5 dBFS and never did:
    /// what crests there is [`WORD_CAPITAL_GAIN`] over a sounding Space,
    /// through the limiter's −14 dBFS knee. That clause stays open for those
    /// contexts. A shouted line at 10 cps peaks no higher than its baseline.
    #[test]
    fn the_ting_and_a_word_opening_capital_crest_no_higher_than_before_forte() {
        const SEEDS: [u32; 5] = [SEED, 0x5EED_1234, 0x504F_4F46, 0xCAFE_F00D, 0x0BAD_CAFE];
        /// (context, baseline worst take, baseline median), dBFS.
        const BEFORE: [(&str, f32, f32); 3] = [
            ("", -14.60, -15.55),
            ("hello ", -13.59, -14.43),
            ("it was. ", -13.71, -15.08),
        ];
        const CREST_SLACK_DB: f32 = 0.3;
        const MEDIAN_SLACK_DB: f32 = 0.5;
        /// How far the lock must move the first key's median.
        const LOCK_MEDIAN_GAP_DB: f32 = 0.3;
        /// The design's ceiling (§0, F16), dBFS at VOL 0.4.
        const CEILING_DBFS: f32 = -14.5;
        /// …and its all-caps reading: −13.35 dBFS. Forte measures −13.52.
        const SHOUT_BEFORE_DBFS: f32 = -13.35;
        let db = |x: f32| 20.0 * x.log10();
        // → (worst, median) over letters × seeds. `unducked` leaves the ting
        // at full level under its capital (the duck draws nothing: draws and
        // lane slots are unchanged).
        let crest = |ctx: &str, unlocked: bool, unducked: bool| -> (f32, f32) {
            let mut all = Vec::new();
            for seed in SEEDS {
                for ch in "TWABHISM".chars() {
                    let mut s = TrailSynth::new(SR, seed);
                    s.hammer_unlocked = unlocked;
                    s.ting_unducked = unducked;
                    let mut at = 1_000u32;
                    for c in ctx.chars() {
                        let kind = if c == ' ' {
                            SoundKind::Space
                        } else {
                            SoundKind::Typed
                        };
                        push_ch(&mut s, kind, at, c);
                        let _ = render_mono(&mut s, 15);
                        at += 150;
                    }
                    let _ = render_mono(&mut s, 25);
                    at += 250;
                    push(&mut s, SoundKind::Shift, at, 0.0, false);
                    let mut x = render_mono(&mut s, 6);
                    push_ch(&mut s, SoundKind::Typed, at + 60, ch);
                    x.extend(render_mono(&mut s, 40));
                    all.push(db(peak_of(&x)));
                }
            }
            all.sort_by(f32::total_cmp);
            println!(
                "ting + word-opening capital after {ctx:?} (unlocked {unlocked}, unducked \
                 {unducked}): \
                 worst {:.2}, median {:.2}, least {:.2} dBFS",
                all[all.len() - 1],
                all[all.len() / 2],
                all[0]
            );
            (all[all.len() - 1], all[all.len() / 2])
        };
        for (ctx, before_worst, before_median) in BEFORE {
            let (worst, median) = crest(ctx, false, false);
            assert!(
                worst <= before_worst + CREST_SLACK_DB,
                "after {ctx:?} the ting and its capital crest at {worst:.2} dBFS — over the \
                 pre-forte tree's {before_worst} by more than {CREST_SLACK_DB} dB"
            );
            assert!(
                median <= before_median + MEDIAN_SLACK_DB,
                "after {ctx:?} the median crest is {median:.2} dBFS — over the pre-forte \
                 tree's {before_median} by more than {MEDIAN_SLACK_DB} dB"
            );
            if before_worst <= CEILING_DBFS {
                assert!(
                    worst <= CEILING_DBFS,
                    "after {ctx:?} the crest met {CEILING_DBFS} dBFS before forte \
                     ({before_worst}) and is {worst:.2} now"
                );
                let (loud, _) = crest(ctx, false, true);
                assert!(
                    loud > CEILING_DBFS,
                    "negative control: with the ting UNDUCKED the crest is {loud:.2} dBFS — \
                     under the ceiling too, so this pin cannot see the duck"
                );
                let (_, drawn) = crest(ctx, true, false);
                assert!(
                    drawn > before_median + MEDIAN_SLACK_DB && drawn >= median + LOCK_MEDIAN_GAP_DB,
                    "negative control: on a DRAWN angle the median crest is {drawn:.2} dBFS \
                     against the locked {median:.2} — inside the pin too, so this pin cannot \
                     see the lock"
                );
            }
        }
        let mut shout = f32::MIN;
        for seed in SEEDS {
            let mut s = TrailSynth::new(SR, seed);
            let mut x = Vec::new();
            for (i, ch) in "THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG"
                .chars()
                .enumerate()
            {
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(&mut s, kind, 1_000 + i as u32 * 100, ch);
                x.extend(render_mono(&mut s, 10));
            }
            x.extend(render_mono(&mut s, 40));
            shout = shout.max(db(peak_of(&x)));
        }
        println!("ALL CAPS at 10 cps: {shout:.2} dBFS");
        assert!(
            shout <= SHOUT_BEFORE_DBFS,
            "a shouted line at 10 cps peaks {shout:.2} dBFS — over the pre-forte tree's \
             {SHOUT_BEFORE_DBFS}"
        );
    }
}
