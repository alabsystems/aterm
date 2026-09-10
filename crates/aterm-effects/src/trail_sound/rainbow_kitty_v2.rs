// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE MUSIC BOX** — Rainbow Kitty v2's instrument and its melody engine.
//!
//! Design of record: `docs/design/RAINBOW-KITTY-V2.md` §9 (the instrument),
//! §10 (`MelodyV2`), §11 (the gesture table), §12 (the meteor), §13 (stardust
//! and the token bucket), §14 (lanes), §15 (latency), §16 (the engine deltas)
//! and §20.2 (the laws this module is pinned on). Every `§` below is that
//! document.
//!
//! ## What this module is for
//!
//! The owner's report on v1 was that typing "sounds too chaotic and not
//! melodic and cute and whimsical". §9.0 anchors that report to **five
//! measured mechanisms**, and §9–§15 fix exactly those five. Everything here
//! is downstream of one of them, so each is named where it is cured:
//!
//! 1. **Two keystrokes in three were LEAPS.** `SONG_PULSE` voiced ghosts an
//!    octave down, a fourth down and a third up from the accent on the
//!    identical bright bell, so most 3–5 letter words rendered
//!    `accent, −8ve, −4th, accent, −8ve` — ≈ 6.7 octave-class leaps a second
//!    at 10 cps, which the ear files as texture rather than as a tune. v2 has
//!    NO ghost lane: every key is its own derived note, and the only
//!    same-pitch re-strike left is a doubled letter's ([`Touch::ReStrike`])
//!    — a music-box tremolo the text asked for. Cured in
//!    [`MelodyV2::on_typed`].
//! 2. **The caret's column transposed the melody.** v1 added
//!    `col_off = pan.round()` to the degree, so one verse note was three
//!    different pitches across one line. v2 reads the column for PAN and for
//!    nothing else; `deg` is never a function of `pan` anywhere in this file.
//! 3. **The bell's crown out-summed its fundamental, and an un-enveloped
//!    180 Hz noise band rumbled under every note for 300 ms.** v2's tine is
//!    sine-led with per-partial decays ([`P2_TAU_S`], [`P3_TAU_S`]) and a felt
//!    mallet that is gone by 25 ms ([`MALLET_TAU_S`]) — the engine deltas that
//!    make that possible are §16's rows 1–2.
//! 4. **The bar was indexed by KEYSTROKE with no clock**, so the meter
//!    followed finger jitter. v2's first answer was a 220 ms TIME gate on the
//!    playhead, and the owner ruled it the opposite of what he asked for
//!    (2026-09-08): a key that does not step adds no note, so above ~4.5 cps
//!    the melody stopped being something you were playing. **The gate is
//!    gone.** ONE KEYSTROKE IS ONE MELODY STEP AT EVERY TYPING SPEED, and the
//!    smoothing that the gate was buying now comes from note CHOICE —
//!    [`MelodyV2::derive`]'s conjunct interval ladder, its gravity, its
//!    reflection and its run guard — never from withholding a step. The
//!    finger jitter the old rule feared IS the performance: it is what turns
//!    an accelerating burst into a rising line ([`ACCEL_SHARE`]).
//! 5. **Navigation spoke twice on two clocks with no flight sound.** v2's
//!    meteor ([`TrailSynth::v2_meteor`]) is ONE gesture whose every time
//!    constant is a multiple of `flight_ms(cells)` — the same number the
//!    pixels fly — so the bell rings on the landing frame at any distance.
//!
//! ## Two laws this file inherits and may not break
//!
//! **ONE LATTICE** (§2.6). Every pitched thing here is a degree of the
//! just-intonation major pentatonic [`super::PENTA`] anchored at
//! [`TINE_BASE_HZ`], reached through [`super::penta`] and never through
//! `TrailSynth::melody_hz` — the latter multiplies by the tone table's
//! transpose, and §2.6 rules that mood may bend a decay but never a pitch.
//!
//! **DETERMINISM.** No clock is read here; time arrives as `at_ms`. Every
//! "random" quantity is a draw from the synth's seeded stream
//! (`TrailSynth::rnd_in`), so one script under one seed renders one waveform,
//! bit for bit (A27).

use super::{
    ERASE_MIN_GAP, EventMeta, HELD_ERASE_RUN_WINDOW, OutputGesture, PAN_LAW_SCALE, Palette,
    Partial, SoundEvent, SoundGesture, SoundKind, TrailSynth, Voice, pan_gains, penta,
};
use crate::rainbow_kitty::meteor::tri;
use crate::rainbow_kitty::timing;

// ===========================================================================
// §14 — LANES. Polyphony, caps and stealing.
// ===========================================================================

/// **THE IDENTITY LANE.** A voice tagged 0 went through the pre-v2
/// [`TrailSynth::claim`] with no census, no cap and no age guard — which is
/// every voice the nine pinned palettes build. `claim_lane` returns early on
/// it, so lane bookkeeping is not merely cheap on the v1 path, it is *absent*
/// from it.
pub(super) const LANE_NONE: u8 = 0;
/// The verse: steps, re-strikes, the nav tick, the meteor bell, the cadence's
/// pickup and resolution. Cap 4 (§14).
pub(super) const LANE_TUNE: u8 = 1;
/// THE BLOOM (§3.3): the slow-attacking 3f/4f/6f voice that fades in behind
/// a lit key's strike, the motif's answering voice, and the two air taps a
/// line end or a rest leaves in the room. Cap 2 — the two slots the deleted
/// capital echo vacated; formerly `LANE_ECHO`, whose only spawn site in this
/// module was that echo.
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
/// The bare-Shift lift. Cap 1.
pub(super) const LANE_SHIFT: u8 = 10;

/// FADE-STEAL / DAMP RAMP, seconds. §9.5 law 2: a damp is a ramp, never a cut.
/// It IS the shipped [`super::SPACE_DAMP_S`] — an alias, not a second 0.012,
/// because it is the same physical act (lift the finger off a ringing note)
/// at the same scale, and two spellings of one ramp would be two things to
/// retune.
pub(super) const LANE_FADE_STEAL_S: f32 = super::SPACE_DAMP_S;

/// THE TAIL LAW's ratio (§9.5 law 2, A13): a voice's `dur` is at least this
/// many of its decay τ, so the envelope has reached ≤ 7 % of peak (−23 dB;
/// `e^−2.7 = 0.067`) before the engine's 5 ms release ramp ends it. Every
/// `*_DUR_S` below that is not a tine (`3τ + 20 ms`, `e^−3`) is DERIVED as
/// this many of its own τ, because §11's own figures fell short of the law
/// it states — the table's 60 ms nav tick on a 30 ms τ ended at 13.5 %,
/// twice the law — and a number stated twice is a number that drifts.
const TAIL_DUR_PER_TAU: f32 = 2.7;

/// Two tonal partials closer than this are ONE PITCH (A11's exact-coincidence
/// bound): §9.5 law 5's same-pitch re-strike damps the old voice first.
const SAME_PITCH_HZ: f32 = 0.5;

/// THE AGE GUARD, seconds (§14). A voice younger than this is never stolen:
/// under 40 ms a note has not yet delivered its own attack, so cutting it
/// reads as a glitch rather than as a fade. When the guard bites, the lane
/// decides who loses — see [`lane_drops_the_newcomer`].
pub(super) const LANE_AGE_GUARD_S: f32 = 0.040;

/// §14's cap table. The caps sum to **23** here, plus POOF/SWOOSH's 2 (which
/// stays unlaned on its own shipped [`ERASE_MIN_GAP`] gate — v1 machinery v2
/// reuses byte-unchanged) = §14's 25 of 28 slots. The 3-slot headroom is the
/// reason a lane under its cap can ALWAYS be admitted and
/// `TrailSynth::steals()` reads 0 in every scenario (A23). PEDAL is the bed
/// knob's own lane, off by default (§9.7) and not built here.
///
/// **CASCADE reads 4, not §14's 1, and that is not a relaxation.** §14's "cap
/// 1, 180 ms exclusivity" is a cap on live *cascades*; the brrrring IS four
/// notes, so one cascade needs four voices. The single-cascade law is enforced
/// where it belongs — [`MelodyV2::on_jump`]'s [`CASCADE_EXCLUSIVE_MS`] window
/// — and giving the figure its own four slots is what stops it evicting the
/// typing's tines, or being evicted by them, mid-figure. A literal cap of 1
/// under the onset census would fade-steal each brrrring note 12 ms after
/// the next one opened — a staccato figure the pinned v1 brrrring never was.
///
/// **The arithmetic, so nobody re-derives it wrong:** TUNE 4 + BLOOM 2 + BASS
/// 1 + BREATH 1 + GLINT 3 + METEOR 3 + RAIN 3 + CADENCE 1 + CASCADE 4 + SHIFT
/// 1 = 23; + POOF 2 = **25**, which is §14's own total because CASCADE's
/// three extra slots are exactly the three §14 books to the PEDAL lane that
/// is not built. The day a pedal lane lands, those three slots are spoken for
/// twice and the 28-slot argument breaks by construction: CASCADE must then
/// give them back (or `MAX_VOICES` must grow). `the_lanes_hold_their_caps_
/// and_steals_are_zero` runs §14's worst case against this table.
pub(super) fn lane_cap(lane: u8) -> usize {
    match lane {
        LANE_TUNE | LANE_CASCADE => 4,
        LANE_BLOOM => 2,
        LANE_GLINT | LANE_METEOR | LANE_RAIN => 3,
        // BASS, BREATH, CADENCE, SHIFT — and anything unnamed, which cannot
        // occur but must not silently become unbounded.
        _ => 1,
    }
}

/// WHO LOSES when a full lane's oldest voice is younger than
/// [`LANE_AGE_GUARD_S`]: `true` = drop the newcomer, `false` = steal anyway.
///
/// §14 states it as a hierarchy of consequences. A missing GLINT or BLOOM is
/// a decoration that did not happen — inaudible as an absence. A missing TUNE
/// voice is **a key that made no sound**, which reads as a dropped keystroke;
/// that is worse than a clipped tail, so the tune always speaks.
pub(super) fn lane_drops_the_newcomer(lane: u8) -> bool {
    matches!(lane, LANE_GLINT | LANE_BLOOM)
}

// ===========================================================================
// §10.2 / §10.4 — the melody's clocks
// ===========================================================================

// ---------------------------------------------------------------------------
// THE DERIVED LINE (§3.1). There is NO time gate here, in any form — not a
// rate limiter, not a budget, not a token bucket. Every one of the constants
// below shapes WHICH note a key gets; not one of them can decide that a key
// gets no note. That is the owner's ruling of 2026-09-08 and it is the whole
// reason this section replaced a single `STEP_GATE_MS`.
// ---------------------------------------------------------------------------

/// THE INTERVAL LADDER. Real melodies are 70-80 % conjunct, so the bands are
/// cut for that and not for an even split of the fold: over the 13 folded
/// values this is 0 repeats, 7 seconds, 4 thirds and 2 fifths — 54 / 31 /
/// 15 % conjunct-to-leap. (The fold's zero is a second, not a repeat — see
/// below; the only repeat is the text's own.)
///
/// **A REPEAT IS EARNED BY A REPEATED LETTER, NEVER BY THE FOLD.** The fold
/// is modulo 13, so two letters exactly thirteen apart (`a`/`n`, `b`/`o`, …)
/// land on 0 as surely as `tt` does — and they are 2 of the 26 offsets a
/// random pair can take, which put the repeat rate of English prose near 15 %
/// where the doubled-letter rate is nearer 4. That is an accident of the
/// modulus and it is audible as a stutter, so [`MelodyV2::derive`] passes the
/// RAW difference's zero-ness in alongside the folded value: a genuine repeat
/// stays a repeat, and a folded-to-zero leap takes the smallest real move
/// there is.
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

/// THE DEAD BAND. Human inter-key jitter is ±25 %, so a band narrower than
/// this would let finger noise overwrite the alphabet's sign on most keys and
/// the word motifs would stop being recognisable. Outside it the hand is
/// genuinely accelerating or genuinely hesitating, and it takes the contour.
///
/// Measured against the SMOOTHED interval as it stood BEFORE this gap was
/// folded in ([`IOI_EMA_ALPHA`]) — the running tempo the key is early or late
/// against. Folding the gap in first would make every gap partly its own
/// reference and shrink the band by half.
const ACCEL_SHARE: f32 = 0.75;
const DECEL_SHARE: f32 = 1.35;

/// The register's middle, and how far the walk may wander before it is pulled
/// back. Reflection at the bounds only acts at the edges; gravity acts
/// everywhere, which is what turns a random walk's flat pitch distribution
/// into a bell around a centre — the thing that makes a wander sound like it
/// is IN a key rather than drifting through one.
///
/// **The pull is one-sided by construction, and that is the design.** With
/// centre 3 and reach 3 the down-pull arms at degree 7 and the up-pull would
/// arm below 0, which [`TUNE_DEG_LO`] makes unreachable: the lower half's
/// restoring force is the REFLECTION at 0, which bounces a descending walk
/// back up, and the upper half's is this gravity, which bends a climb over
/// before it can reach the ceiling and start bouncing there. The two together
/// settle the distribution around 3. The unreachable arm is kept as the law's
/// other half so the rule stays true if the register ever moves.
const MELODY_CENTRE_DEG: i32 = 3;
const MELODY_GRAVITY_DEG: i32 = 3;

/// Three identical strides running is a figure; four is a machine. The fourth
/// inverts.
///
/// THE RUN IS COUNTED ON THE STRIDE THE EAR GETS, not on the one `derive`
/// chose. Between the two sit the reflection at the register's bounds, the
/// word-head snap onto a chord tone and the subject's answer, and each of
/// them can turn a counted stride into a different sounded one: a `−1` off
/// degree 0 SOUNDS as `+1`, and a subject latched as `+1 +1 +1` answered
/// after a head that rose `+1` SOUNDS as four. Counted on `derive`'s stride,
/// this guard let both through — `0 1 2 3 4` on the bench's digit run, a
/// straight five-note scale at every answer of a rising subject — and the
/// render's siren verdict caught it. So [`MelodyV2::note_run`] is fed
/// `deg − from` after the snap, the guard is consulted by the answer as well
/// as by the derivation, and a head whose snap would complete the fourth
/// takes the nearest chord tone on the other side of the line instead.
const MELODY_RUN_MAX: u8 = 3;

/// HOW FAR A WORD HEAD MAY BE MOVED to land on a chord tone (§3.1 step 6).
/// Two degrees reaches five consecutive pitch classes and every chord of
/// [`CHORD_LOOP`] lights three of five, so the search always lands — the same
/// totality argument the rest cadence's own search rests on.
const WORD_HEAD_SNAP_DEG: i32 = 2;

/// THE LINE'S OWN MOTIF. The first three intervals after an Enter (or after a
/// phrase rest) are LATCHED as this line's subject — three, not four, because
/// at ~4.5 letters per English word four fills the whole word and the line
/// becomes the subject rather than answering it.
const MOTIF_LEN: usize = 3;
/// …and it is answered at every THIRD word head, transposed onto the live
/// chord tone the word head already snapped to. One word in three is about
/// one key in thirteen: a recall, not a loop.
///
/// **THE RATE MOVED 4 → 3 ON THE PANEL'S Q1 RULING (2026-09-09; §9).** Every
/// judge read the derived line as a melody and none of them could hear its
/// refrain. The rolls say why in seconds rather than in words: the bench's
/// prose take counts 46 word heads in 60 s — 1.30 s a word — so a rate of
/// four put 5.2 s between answers, outside the ~3-4 s in which an ear can
/// still bind a repetition to the antecedent it answers, and the answer
/// marks read as noise. Three puts them 3.9 s apart at prose tempo and 1.8 s
/// at 10 cps: inside the window at both.
///
/// **AND IT IS STILL COUNTED IN WORDS, NOT IN SECONDS.** A seconds-based
/// answer was offered and refused: it would fire at a different word head at
/// a different typing speed, and the same text typed at two speeds must play
/// the same degrees (the owner's ruling of 2026-09-08, which this file's
/// whole missing time gate exists to keep). Every term in the rate is the
/// text's.
const MOTIF_EVERY_WORDS: u8 = 3;

/// MACHINE REGULARITY, NOT SPEED. Three consecutive gaps agreeing this
/// closely — **on the same glyph** — is macOS key repeat; a genuinely fast
/// human hand is jittery and is never caught by it, where a rate threshold
/// would catch both.
///
/// **The same-glyph conjunct is this file's, not §3.1's, and it is load
/// bearing.** A held key repeats ONE character by definition, so the rank
/// costs nothing to require; without it a metronome — a script, a paste
/// replay, the census's own clean column — is machine-regular by construction
/// and every note of it would go unpitched. Regularity alone is not evidence
/// of a machine; regularity on one glyph is.
const AUTOREPEAT_JITTER_MS: u32 = 2;
const AUTOREPEAT_RUN: u8 = 3;
/// …and an absolute backstop at ~40 cps, past any hand. This one asks nothing
/// of the glyph: nothing human puts two DIFFERENT keys 25 ms apart either.
const AUTOREPEAT_FLOOR_MS: u32 = 25;

/// A typing gap this long is a REST, and a rest RESOLVES the line (§3.1).
///
/// It resolves the walk onto the live chord where it stands, clears the
/// contour bias and the run, and re-latches the motif so the phrase after the
/// pause writes a new subject. **It never substitutes for a step and never
/// withholds one:** the key that ends the pause resolves the line and then
/// sings its own derived note, exactly as every other key does.
///
/// **900, not v1's 600.** v1's 600 ms was tuned against a governor decay that
/// §16 row 11 sets to exactly 1.0 for v2; at 600 ms every ordinary think-pause
/// resolves, and a line that resolves every few words has no line left. 900 ms
/// is longer than a word-finding pause and shorter than a real stop.
pub(super) const PHRASE_PAUSE_MS: u32 = 900;

/// A gap longer than this is not typing at all: the IOI estimator RESTARTS at
/// [`IOI_DEFAULT_MS`] rather than dragging a stale 600 ms average into the
/// next burst.
const IOI_RESET_MS: u32 = 2_000;
/// IOI clamp floor / ceiling (ms), §9.6.
const IOI_MIN_MS: f32 = 30.0;
const IOI_MAX_MS: f32 = 600.0;
/// The IOI a fresh session assumes — 4 cps, which §9.6's table uses as the
/// 0 dB reference for the whole loudness arc.
const IOI_DEFAULT_MS: f32 = 250.0;
/// EMA weight on the newest gap (§9.6). 0.5 is two-gap memory: fast enough
/// that a burst is heard as a burst, slow enough that one stumbled key does
/// not brighten the note after it.
const IOI_EMA_ALPHA: f32 = 0.5;

/// How many keystrokes the undo stack remembers (§10.1). Eight is a word: a
/// correction runs backwards through the letters you actually mistyped, and a
/// deeper stack would let a Backspace un-sing a note whose phrase has already
/// closed.
const UNDO_N: usize = 8;

/// The PTY cascade's exclusivity window (D18). One 4-note brrrring per 180 ms,
/// however many line feeds arrive: 12 Jumps at 60 ms used to mean ~60 pitched
/// onsets a second.
pub(super) const CASCADE_EXCLUSIVE_MS: u32 = 180;
/// Inside a live cascade, the top-note re-strike's own floor (D18).
const CASCADE_RESTRIKE_MS: u32 = 60;
/// A `Jump` arriving this soon after a keyed `Enter` is that Return's OWN
/// line-feed echo, not a PTY cascade, and is swallowed (§10.4, A26: "a keyed
/// Enter → the cadence, no cascade"). §16 row 13 has the host suppress the
/// echo at the seam; this is the synth's own defence, on the same 150 ms
/// the design uses for an arm's ttl and the Enter-to-Enter cascade IOI.
const ENTER_ECHO_SWALLOW_MS: u32 = 150;

/// Keys since the previous Enter below which the cadence is a bare tonic dyad
/// rather than the full pickup/resolution/bell (§11's two Enter rows). A
/// Return typed at an empty shell prompt is not the end of a phrase.
const ENTER_PICKUP_MIN_KEYS: u8 = 4;

// ===========================================================================
// §9.1 — THE TINE. One voice per key.
// ===========================================================================

/// C5 — the lattice anchor (§2.6). Every pitched thing in v2 is
/// `penta(TINE_BASE_HZ, degree)`.
pub(super) const TINE_BASE_HZ: f32 = 523.25;

/// Voice attack, seconds. §9.5 law 1: ≥ 4 ms under 1 kHz, because the splatter
/// bandwidth of an onset is ≈ 1/(2π·attack) and 4 ms puts it at 40 Hz — below
/// the band where a click is heard as a click.
const TINE_ATTACK_S: f32 = 0.004;
/// The tail added to `3·τ_v` to get `dur` (§9.1). At `3τ` the envelope is at
/// −26 dB; the extra 20 ms is where the engine's own 5 ms release lives, so
/// the voice ends quietly rather than being cut (§9.5 law 2, A13).
const TINE_DUR_TAIL_S: f32 = 0.020;

/// P1 — the sine BODY. No FM, no glide, no grace bend: the pitch is settled at
/// sample 0, which is what makes a fast passage legible as pitches.
const P1_LVL: f32 = 0.50;
/// P2 — the OCTAVE, at −9.9 dB re P1.
///
/// **0.16 / τ 55 ms — the values §9.1 now states (originally 0.13 / 45).**
/// The capture-after bench (2026-09-05, `keyboard_song_ab` "music box", the
/// isolated Typed probe on C6) read the tine at centroid 1068 Hz, energy
/// above 2 kHz 0.017, against a §9.1 target of 1100–1400 Hz / 0.10–0.25
/// that has since been RETIRED (§22 A/B #17, ruled 2026-09-05 — "warm is
/// probably better": reaching e>2k 0.10 would have needed P2 ≈ 0.32 /
/// P3 ≈ 0.25, a glass bell, the very sound §9.0 cause 3 rejects). The
/// octave and the strike are the only energy the tine has above 2 kHz, so
/// both were raised a little — a little more level and a little more life
/// (centroid ≈ 1085 on that probe), the octave still dying well before the
/// body (τ_v 55–110 ms) — and that is where the ruling holds them.
const P2_RATIO: f32 = 2.0;
const P2_LVL: f32 = 0.16;
/// P2's OWN decay (§16 delta 1). It dies first: celesta physics — the octave
/// is the sparkle on the attack, not a second note. Per-partial decays
/// MULTIPLY the voice envelope, so the octave's effective τ at a 110 ms
/// body is `1/(1/55 + 1/110)` ≈ 37 ms.
const P2_TAU_S: f32 = 0.055;
/// P3 — the STRIKE, the music-box bar's inharmonic mode, at −12.4 dB re P1
/// (0.12 / τ 40 ms — the values §9.1 now states, originally 0.10 / 35 — the
/// same capture-after fit as [`P2_LVL`], held by the same warm ruling).
///
/// 2.760 is a *colour*, not a harmonic, and §9.5 law 4 would normally forbid
/// it: an inharmonic partial can land 15–60 Hz from a neighbour's harmonic and
/// never coincide exactly (D5 × 2.76 = 1625 Hz beats at 55 Hz against a live
/// G6 at 1570). It is admitted by the law's own exemption — **its τ is 40 ms**,
/// and a 15 Hz beat needs 67 ms for one cycle, so the partial is gone before
/// roughness can exist. That is why [`P3_TAU_S`] is not a taste knob: it may
/// sit anywhere at or under the exemption's 45 ms (A11's `decay ≤ 0.045`),
/// and 40 ms is as far as the capture-after brightening takes it.
const P3_RATIO: f32 = 2.760;
const P3_LVL: f32 = 0.12;
const P3_TAU_S: f32 = 0.040;

/// THE FELT MALLET — band-passed noise, 900 → 3200 Hz, the ear's onset
/// time-stamp and the whole of what makes the tine read as *struck*.
///
/// v1's mallet swept 5200 → 180 Hz with **no envelope of its own** (§9.0 cause
/// 3): a 180 Hz Q 0.7 band sat ≈ 9 dB under every bell for its full 300 ms,
/// rumbling, and paid a `tanf` per sample for the privilege. v2's is over in
/// 25 ms — a ≈ 50× cut in that cost per key, and the low rumble simply does
/// not exist.
///
/// **THE SWEEP TURNED AROUND ON THE PANEL'S Q2 RULING (2026-09-09; §9).** It
/// swept 3200 → 900 Hz, which is the warm-thump direction: a strike that
/// gets darker as it lands. The owner's standing ruling on this theme is
/// that the keystroke is a glass bell and that **a pitch sweep IS the
/// squeak** — the upward one — and his 2026-09-09 note is that the song
/// stopped being cute and sparkly. The capital zoom says the same thing
/// spectrally: B's note is horizontal bars from onset to decay, nothing
/// glints, which is exactly how a glass bell stops reading as one.
///
/// **AND IT IS THE SWEEP THAT MOVED, NOT THE PITCH.** A short upward glide
/// on the TINE was the other candidate and was refused: every pitched thing
/// in this module sits on the one lattice precisely so that no two live
/// voices can beat (§9.5 law 4), and a detuned fundamental sliding under a
/// live bloom's 3f is the one shape that law forbids. The mallet is
/// band-passed NOISE — it has no pitch to detune and nothing to beat with —
/// so the squeak is free there and costs the lattice nothing.
const MALLET_HZ0: f32 = 900.0;
const MALLET_HZ1: f32 = 3200.0;
const MALLET_GLIDE_S: f32 = 0.005;
const MALLET_Q: f32 = 0.7;
const MALLET_LVL: f32 = 0.45;
/// The mallet's own decay (§16 delta 2) — 6 ms, so it is ≈ −36 dB by 25 ms.
const MALLET_TAU_S: f32 = 0.006;

/// τ_v — the voice decay, adaptive to the inter-onset interval (§9.1):
/// `clamp(110·(0.068 + 3.727·IOI_s), 28, 110)` ms.
///
/// This is the masking law, not a taste dial. At 10 cps the keys are 100 ms
/// apart; a 110 ms note would overlap its successor and the pitches would pile
/// into a chord instead of a line. τ_v falls to 48 ms there, so **fast typing
/// thins the notes rather than the note count** — every key speaks, always.
///
/// **THE LINE IS FITTED THROUGH THE TWO POINTS §3.1 STATES**, and the floor
/// is 28 ms, not 55. With the step gate deleted, the roughness law that the
/// deleted re-strike coalescer was defending — keep amplitude modulation out
/// of the 15-60 Hz buzz band — is carried HERE, where this module's own
/// header says it belongs: by note LENGTH, never by note COUNT. §3.1 states
/// the law's two anchors: the 4 cps reference (250 ms) plays the full 110 ms
/// note, and **a 20 cps run plays 28 ms notes** instead of 55 ms notes
/// fighting each other. The design also wrote the curve as
/// `110·(0.35 + 2.6·IOI)`, and that line does not pass through its own second
/// anchor: at 50 ms it gives 52.8 ms, and with [`IOI_MIN_MS`] clamping the
/// intake at 30 ms its global minimum is 47.1 ms — so a 28 ms floor under it
/// could never bind, and the stated effect was never delivered (the step-3
/// review measured exactly that). The intercept is the term that governs
/// the fast end, so it is the intercept that moved: 0.068 / 3.727 is the
/// unique line through (250 ms → 110 ms) and (50 ms → 28 ms). Below 20 cps
/// the floor is the operating point — the intake clamp at 30 ms would read
/// 25 ms, and [`TAU_V_MIN_S`] holds it at 28 — and the multipliers that ride
/// outside the clamp ([`RESTRIKE_TAU_MUL`]) take their touches shorter still.
const TAU_V_BASE_S: f32 = 0.110;
const TAU_V_OFFSET: f32 = 0.068_181_8;
const TAU_V_SLOPE: f32 = 3.727_272_7;
const TAU_V_MIN_S: f32 = 0.028;
const TAU_V_MAX_S: f32 = 0.110;

/// THE ROOF — a one-pole lowpass, and §9.6's whole "brighter when fast, never
/// louder" mechanism. Plain roof lerps 4200 → 5200 Hz over 4 → 12 cps; a
/// chord-tone step opens it by [`ROOF_LIT_ADD_HZ`]; the glow's blaze adds up
/// to [`ROOF_HEAT_HZ`]; the ribbon's hue adds up to [`ROOF_HUE_ADD_HZ`] — and
/// **never a decibel**.
const ROOF_PLAIN_LO_HZ: f32 = 4200.0;
const ROOF_PLAIN_HI_HZ: f32 = 5200.0;
const ROOF_LIT_ADD_HZ: f32 = 2300.0;
const ROOF_MAX_HZ: f32 = 7500.0;
const ROOF_HEAT_HZ: f32 = 600.0;
/// THE NOTE'S OWN ROOF FOLLOWS THE ARC (§3.3 item 3). `ev.hue` is the live
/// rainbow hue the caret is painting, and the roof opens by up to this much
/// as the ribbon travels from the red end to the cyan end — added BEFORE the
/// [`ROOF_MAX_HZ`] clamp, exactly like the blaze's term, so it buys
/// brightness and never a decibel. Read through [`hue_arc`], so the wrap
/// reflects and there is no click at the seam.
const ROOF_HUE_ADD_HZ: f32 = 900.0;
/// The cps span the plain roof opens across (4 → 12 cps).
const ROOF_CPS_LO: f32 = 4.0;
const ROOF_CPS_SPAN: f32 = 8.0;
/// A re-strike's roof sits 400 Hz under a step's plain roof (§9.2) — the
/// tremolo is the same note *further away*, which is how a music box's second
/// strike of one tooth actually sounds.
const RESTRIKE_ROOF_DROP_HZ: f32 = 400.0;

/// §9.2's re-strike touch: the tine with the strike partial at zero, "as the
/// round mote is the star at arm 0".
const RESTRIKE_P2_LVL: f32 = 0.10;
const RESTRIKE_MALLET_LVL: f32 = 0.30;
const RESTRIKE_TAU_MUL: f32 = 0.7;
/// The re-strike ladder `L_n = max(0.60·0.85^(n−1), 0.35)` — −4.4, −5.8, −7.3,
/// −8.7, then the −9.1 dB floor. Falling, so a held key decays into the
/// texture instead of hammering; floored, so the fifth re-strike is still a
/// note and not a ghost of one.
///
/// **0.60, §22's A/B item 6, taken on the capture-after measurement.** With
/// §9.2's 0.70 the 10 cps prose mix sat at −35.8 dBFS RMS against v1's
/// −37.5 (+1.7 dB — §21.4's "cuteness must not buy loudness" guard), with the
/// re-strikes ≈ 37 % of the TUNE lane's energy. The lower base buys −1.3 dB
/// on the re-strikes (more DA-da-da contrast, the item's own words) while
/// the step — the note the ladder is written against — does not move.
const RESTRIKE_L0: f32 = 0.60;
const RESTRIKE_FALL: f32 = 0.85;
const RESTRIKE_FLOOR: f32 = 0.35;

// ---------------------------------------------------------------------------
// §3.3 — WHAT MAKES IT RAINBOW AND MAGICAL. Four additions, all inside the
// existing `Voice` vocabulary: no reverb, no delay line, no dependency. The
// tine itself is untouched — softening the strike would undo R1.
// ---------------------------------------------------------------------------

/// **THE BLOOM** — the single biggest lever, and the one that separates
/// *struck* from *enchanted*. One extra voice per lit key, in the lane the
/// deleted capital echo vacated ([`LANE_BLOOM`]): a twelfth, two octaves,
/// and a twelfth above that — §3.3's 3f / 4f / 6f — each dying at its own
/// rate.
///
/// **STATED AS LATTICE DEGREES, NOT AS RATIOS**, because this module has
/// ONE LATTICE and the bloom is a pitched thing. On C, G and A the degrees
/// ARE 3f / 4f / 6f exactly; on D and E the twelfth bends to the lattice's
/// own sixth (10/3, 6.4) because the just pentatonic's D–A is a wolf: D×3 is
/// 27/16 and the lattice's A is 5/3, a syntonic comma apart, and the first
/// render with exact harmonics beat D5×6 against A5×4 at 43.6 Hz — inside
/// the 15-60 Hz roughness band §9.5 law 4 forbids, with a 220-380 ms τ that
/// no exemption covers. On the lattice every bloom partial either coincides
/// exactly with a live harmonic or sits a lattice step from it, which is
/// the same argument that admits the glints (§13).
const BLOOM_DEGREES: [i32; 3] = [8, 10, 13];
const BLOOM_LVL: [f32; 3] = [0.10, 0.06, 0.035];
const BLOOM_TAU: [f32; 3] = [0.22, 0.30, 0.38];
/// IT FADES IN BEHIND THE STRIKE. Everything else in this theme attacks in
/// 4 ms and decays; this one blooms. A bell that arrives after its own mallet
/// is the canonical enchantment cue, and it is why this reads as magic rather
/// than as a brighter click.
const BLOOM_ATTACK_S: f32 = 0.018;
/// …12 ms behind the mallet, so the ear hears STRIKE then BLOOM and not one
/// fatter transient.
const BLOOM_DELAY_S: f32 = 0.012;
/// **THE BLOOM MUST PEAK INSIDE ITS OWN NOTE.** Delay + attack is 30 ms, and
/// 30 ms is 60 % of a 50 ms inter-key interval: at 20 cps the bloom of key
/// *n* was arriving on top of key *n+1*'s strike, so the enchantment accent
/// was phase-locked to the wrong note (the panel's Q5 ruling, 2026-09-09).
/// Both are therefore scaled by the note's own τ_v against the 4 cps
/// reference — 1.0 at prose tempo, so the anchor is again untouched — under
/// this floor, which keeps the STRIKE-then-BLOOM order audible (a head that
/// scaled to nothing would fuse the two into one fatter transient, which is
/// the thing [`BLOOM_DELAY_S`] exists to prevent). At 10 cps and faster the
/// head is half: 6 ms behind the mallet, peaking 15 ms in.
const BLOOM_HEAD_MIN_MUL: f32 = 0.5;
/// THE HANG'S CEILING. The tine speaks; the bloom hangs. Notes therefore
/// overlap and answer each other in the HARMONICS, where consonance is
/// structural, and never in the fundamentals.
///
/// **THE LAW WAS WRITTEN AS "3-6× THE TINE'S OWN TAU" AND SHIPPED AS A
/// CONSTANT** — which is in law at the 4 cps anchor (3.1 × 110 ms) and out
/// of it by a factor of two at 20 cps (12.1 × 28 ms). The panel's Q5/Q2
/// rulings (2026-09-09; §9) measured what that costs: with a 1.04 s `dur`
/// on a 50 ms inter-key interval the hangs stack until the 2-7 kHz band is
/// an unbroken sheet — the trough between notes above 2 kHz falls from
/// 13.2 dB at 4 cps to 5.1 at 10 cps and 3.0 at 20 — and it takes 3.5 dB of
/// full-band note separation at 10 cps and 3.3 dB at 20 with it. The melody
/// band never smeared (18.4 / 15.5 / 11.5 dB troughs on every rung), so this
/// hang was the whole of the mud at speed, and [`TAU_V_MIN_S`] does not
/// govern it: the NOTE was never the problem and does not move.
///
/// So the decay is now the law as it was written — [`BLOOM_DECAY_TAU_MUL`] ×
/// the note's own τ_v, under this ceiling — and this constant is what it
/// always was at the anchor the design fitted it on.
const BLOOM_DECAY_S: f32 = 0.340;
/// The written law's multiplier, and it is 3.1 for an exact reason: τ_v at
/// the 4 cps reference is [`TAU_V_MAX_S`] 110 ms, and 3.1 × 110 ms = 341 ms
/// takes the ceiling — so the bloom the owner has already heard at prose
/// tempo is bit-for-bit the bloom he heard before, and the change binds only
/// as the hand speeds up (149 ms at 10 cps, 87 ms at 20 cps, back inside the
/// stated 3-6× band at every rate).
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
/// The bloom's level re the step it blooms behind, BEFORE [`hue_air`].
///
/// **FITTED ON THE BENCH (2026-09-08), NOT TAKEN FROM §3.3.** §3.3's own
/// figure of 0.30 put the bloom ≈ −25 dB under the fundamental (the partial
/// levels above are already a fifth of the tine's P1) and moved the bench
/// probe's centroid by three hertz — inaudible, and nothing like the lever
/// the design describes. The prediction is the law, so the level was swept
/// on the bench's own gesture probe (`keyboard_song_ab --probes`) and prose
/// scene, seed `0x504f4f46`, vol 0.4, red end of the arc (`hue_air` 0.55):
///
/// ```text
/// BLOOM_LEVEL   probe centroid  hi>2k   prose rms    prose centroid  burst ons/s
/// plain (0)         905 Hz      0.009   (−36.9 est)      —              18.3
/// 1.0               936         0.023   −36.69 dBFS   1132 Hz          18.3
/// 1.5               975         0.039   −36.48        1248              17.1
/// 2.0              1027         0.062   −36.20        1394              15.3
/// 3.0              1163         0.122   −35.49        1725              11.4
/// ```
///
/// The shipped take reads −36.25 dBFS on the same scene, so §8's +1.0 dB
/// budget is met at every rung (the τ_v refit gave the room back). What
/// decides it is the other two columns: at 3.0 the prose scene's centroid
/// clears the **1600 Hz glass line** and the bloom's hang swallows a third
/// of a 20 cps burst's onsets — the mud R1's shorter notes were bought to
/// avoid; at 1.5 `hi>2k` lands exactly on §3.3's predicted 0.04. 2.0 sits
/// between: the prose centroid inside §3.3's 1250-1400 window, `hi>2k` at
/// 1.5× the prediction, the burst still articulate. The probe's own centroid
/// reads 1027 Hz here against §3.3's 1250-1400 because that window was
/// written against a 1085 Hz tine on the authored verse's probe note; the
/// derived line lands the probe on a lower degree (905 Hz plain), and the
/// RISE is what the prediction is about. Under budget pressure this is the
/// constant that gives back first, never [`KEY_TINE_TRIM`] up; the ladder
/// between 1.5 and 2.0 is the owner's by ear.
const BLOOM_LEVEL: f32 = 2.0;
/// THE BLOOM DRIFTS WITH THE COLOUR. The strike stays on the caret's column
/// and the bloom's pan moves by up to this much with the hue, so the note
/// OPENS in the field.
const BLOOM_SPREAD: f32 = 0.28;

/// **THE ANSWERING VOICE** (§3.3). On a motif-answering word head — the head
/// that opens the line's reply to its own subject, one key in eighteen or so
/// — one extra voice in [`LANE_BLOOM`], this far behind the head, pitched at
/// the nearest lit chord tone ABOVE the head's degree. A call and its answer,
/// in a droppable lane: counterpoint, not decoration.
const ANSWER_DELAY_S: f32 = 0.19;
/// −12 dB re the step.
const ANSWER_LEVEL: f32 = 0.25;

/// **THE AIR CLOUD** (§3.3 item 4) — the only literal room in the theme, and
/// it is NOT per key. Two bloom taps behind a line's end (Enter) and behind
/// the key that ends a phrase rest, at unequal spacings and opposite pans:
/// two taps at unequal spacings read as a room; per key at 10 cps they would
/// read as a smear. Levels are re the bloom they echo — −14 and −20 dB.
const AIR_TAP_DELAY_S: [f32; 2] = [0.09, 0.17];
const AIR_TAP_LEVEL: [f32; 2] = [0.199_526_2, 0.1];
const AIR_TAP_PAN: f32 = 0.4;

/// **A CAPITAL IS ONE ONSET** (§3.1 "Boundaries"): its own single step,
/// lifted, plus the open roof `ev.shifted` already buys. Where there used to
/// be three sounds (the lift, the letter, an octave echo 25 ms later) there
/// is now the letter, higher. Inside a word the lift is bounded by A2's
/// in-word law ([`WORD_LEAP_MAX_DEG`]): a camelCase capital is an accent,
/// not the octave-class leap this instrument exists to cure.
///
/// **THE ONE ONSET IS RIGHT AND THE OCTAVE WAS NOT** (the panel's Q4 ruling,
/// 2026-09-09; §9). Every judge measured the single onset as correct and the
/// capital as no quieter than the three-sound version it replaced (−18.8
/// dBFS against −18.4). What the rolls found instead was a register pinned
/// against its own ceiling: a lift of five degrees on a walk centred on
/// three took the ceiling from almost anywhere, and because it was applied
/// to EVERY shifted key a run of capitals became a permanent transposition
/// sitting on the clamp. ALL CAPS measured mean degree 7.3 with 19 of 21
/// keys on degrees 7-8 — the string `878787858785878787878`, a two-note
/// plateau — and Title Case put 6 of 21 on the ceiling. Shouting stopped
/// being a melody and became an alarm, and every capital in ordinary prose
/// tended toward the same pitch, which is the exact opposite of an accent.
///
/// Three degrees is a fifth: still plainly a lift, and it no longer arms
/// into the clamp from the register's middle.
const CAPITAL_LIFT_DEG: i32 = 3;

// ---------------------------------------------------------------------------
// §22 — FLOW'S VOICE. The music box OPENS as the hand finds its run.
//
// FLOW is one earned counter, not a rate: keys typed at `disp >= 0.8` with no
// delete, and it drops to zero on a delete, a Kill or a slow key. The engine
// publishes it as `EventMeta::flow`, 0..1, and this file reads it as a LERP
// PARAMETER AND NOTHING ELSE — every quantity below is `lerp(shipped, open,
// flow)`, so heat 0 is the shipped instrument bit for bit and there is no
// branch anywhere that flow can take which the cold box does not.
//
// THREE LEVERS, and the choice of which three is the whole design:
//
// 1. THE OCTAVE ECHO RETURNS. §3.1's "a capital is one onset" deleted the
//    25 ms octave echo that used to follow a capital, and `LANE_BLOOM` still
//    carries the two slots it vacated (it was `LANE_ECHO`). Flow gives it
//    back — not to capitals, to EVERY LIT STEP — at the level it always had.
//    An octave a beat behind the strike is the sound of a box with a bigger
//    resonator, and it is the one addition that reads as "more instrument"
//    rather than as "more notes": it has no degree of its own, no rhythm of
//    its own and no key of its own. It rides the step's own keystroke, which
//    is the law (every light has a keystroke behind it).
// 2. THE BASS DYAD RINGS LONGER. The downbeat is the floor of the mix and
//    the only monophonic voice in the theme, so lengthening its decay adds
//    body underneath everything without adding a single onset.
// 3. THE OCTAVE PARTIAL WARMS. `P2_LVL` is the tine's own 2f — the partial
//    that says "metal bar in a wooden box" — and lifting it is a timbre
//    change, not a level change.
//
// WHY THESE AND NOT A LOUDER BOX. §9.6's law is that speed may buy brightness
// and never a decibel, and §21.4's is that cuteness may not buy loudness.
// Every lever above is measured against the 4 cps cold reference by
// `the_box_opens_with_the_hand_and_never_gets_louder`, which is the guard.
//
// LEVER 1 WAS PROPOSED FOR RETIREMENT ON 2026-09-09, and it survived on a
// measurement rather than on a preference. The derived line's brighter strike
// and this echo landed the same day and the guard went red at +0.23 dB on one
// word; the echo, being the day's new voice, was the obvious suspect, and
// every way of paying for it failed a law rather than a taste — half the
// level moved the rise to quarter heat and made the arc non-monotone, and a
// step trim that paid the whole 0.23 dB at heat 1 still left +0.10 dB at
// quarter heat, because the rise saturates early and a linear give-back
// cannot follow a saturating one. All of that is reproduced and true. It was
// the wrong suspect: the day's OTHER new voice, the per-key sparkle, was
// arriving BEFORE the note it decorates and peaking inside the strike's own
// attack, and a peak is a max over keys, so its phase-random contribution
// only ever lifted whichever key already carried the maximum. Delay the
// sparkle onto the bloom ([`KEY_GLINT_DELAY_S`]), give the echo back the
// octave a separate ruling had quietly made a sixth
// ([`FLOW_ECHO_OCTAVE_DEG`]) and let it swell instead of strike, and the
// guard is green at every rung — quarter heat included, at -0.11 dB.
// ---------------------------------------------------------------------------

/// THE ECHO'S OWN DELAY — the deleted capital echo's 25 ms, unchanged, so
/// the ear hears one note with a longer body and not two notes. Well inside
/// a flowing hand's own IOI (83 ms at 12 cps), which is what keeps the echo
/// attached to the key that earned it.
const FLOW_ECHO_DELAY_S: f32 = 0.025;
/// **AND ITS INTERVAL — ONE OCTAVE**, which in this lattice is five degrees
/// ([`penta`] is `div_euclid(5)` over [`super::PENTA`], so five degrees is
/// `× 2`).
///
/// It has its own name because it was borrowing one. §22 wrote the echo as
/// `deg + CAPITAL_LIFT_DEG` back when [`CAPITAL_LIFT_DEG`] was 5 and the two
/// numbers agreed by accident; the panel's Q4 ruling then moved
/// [`CAPITAL_LIFT_DEG`] to 3 for the capital's REGISTER — a change about
/// shouting, decided on rolls of shifted keys — and silently retuned §22's
/// octave echo to a sixth. A sixth over the fundamental beats with it and
/// with the strike's own [`P2_RATIO`] octave; an octave locks to both. That
/// is worth +0.19 dB on the flowing word's peak and +0.73 at 12 cps, and it
/// is why this is a constant and not an expression: the next ruling on the
/// capital's register must not be able to reach the flow echo's pitch.
const FLOW_ECHO_OCTAVE_DEG: i32 = 5;
/// −12 dB re the step it follows, at heat 1 — the level the capital echo had
/// and the level [`ANSWER_LEVEL`] still uses in the same lane. It LERPS from
/// exact zero, and at exact zero the voice is not spawned at all: flow's echo
/// is structurally absent from the cold box, never a gain-0 render of it.
const FLOW_ECHO_LEVEL: f32 = 0.25;
/// The bass dyad's decay at heat 1, from [`BASS_DECAY_S`]'s 0.111 s. Its
/// `dur` follows through the tail law ([`TAIL_DUR_PER_TAU`]) rather than
/// through [`BASS_DUR_S`], which is the cold constant.
///
/// The octave partial keeps its own 60 ms ([`BASS_OCT_TAU_S`]): the dyad is
/// the root and the fifth, and the octave's short bite is the attack's
/// definition, not the body.
const FLOW_BASS_DECAY_S: f32 = 0.200;
/// The tine's octave partial at heat 1, from [`P2_LVL`]'s 0.16.
///
/// THE STEP'S ONLY. A re-strike's P2 stays [`RESTRIKE_P2_LVL`]: §9.2 takes
/// the partials off a doubled letter deliberately, and warming the tremolo
/// would undo the thing that makes it read as a tremolo rather than as a
/// second note.
const FLOW_P2_LVL: f32 = 0.22;

/// **THE STEP'S FUNDAMENTAL AND OCTAVE at flow heat `flow`** (§22 lever 3) —
/// `(P1, P2)`, exactly `(P1_LVL, P2_LVL)` at `flow == 0.0` because [`lerp`]
/// returns `a` there identically.
///
/// **THE STRIKE'S SUM IS CONSERVED**, and that is not a detail: the octave
/// LERPS UP and the fundamental lerps down by the same amount, so
/// `p1 + p2 + p3` is 0.78 at every heat. Measured, and the measurement is
/// why: warming P2 alone took the prose take's PEAK up 0.55 dB at 4 cps and
/// 0.49 at 8 (30 s of the bench corpus, seed `0x504F4F46`, vol 0.4) — three
/// coincident partials all starting at their own peak, so the sum IS the
/// onset. §9.6 rules that the box may buy brightness and never a decibel,
/// and a strike 0.55 dB taller is a decibel. Conserving the sum keeps the
/// onset where the ladder put it (`the_isolated_step_lands_on_the_ladder_
/// floor`'s −21.0 dBFS is fitted on exactly this peak) and spends flow on
/// the thing that actually reads as a fuller box: the RATIO. At heat 1 the
/// fundamental-to-octave ratio goes 3.13 : 1 → 2 : 1, which is a music box
/// with a bigger, rounder bar, not a louder one.
fn flow_partials(flow: f32) -> (f32, f32) {
    let p2 = lerp(P2_LVL, FLOW_P2_LVL, flow);
    (P1_LVL - (p2 - P2_LVL), p2)
}

/// A PASSING (non-chord-tone) step is 2 dB under a lit one (§9.2). The chord
/// lights the verse; it never moves it (§10.3).
const PASSING_LEVEL: f32 = 0.794_328_2;

/// §9.6's loudness arc: `g_IOI = clamp(√(IOI_s / 0.25), 0.45, 1.0)`.
///
/// The arc REPLACES v1's flood governor (§16 row 11). v1 ducked every typed
/// voice by `1/√(1 + 0.55·rate)` — −6.5 dB at 10 cps — while Jump/Sweep/Land
/// bypassed it entirely, so the tune was the quietest layer in its own mix
/// (§9.0 cause 5). The arc holds per-second energy flat to within +1 dB of the
/// 4 cps reference (A14) *without* making the melody the thing that gives way.
///
/// **The floor is 0.45, not 0.6, and the reference is 0.25 s, not 0.15
/// (§3.1).** The arc is a √ law precisely so that per-second energy stays
/// flat as the rate climbs: below the reference `rate · g² = 1/REF`, a
/// constant, so every note the rate adds is paid for exactly. Two things
/// broke that under R1 and both are fixed here.
///
/// **The reference.** `g` is capped at 1.0 (§9.6: the arc may buy brightness,
/// never a decibel), so the flat law only holds for `IOI ≤ REF` and typing
/// slower than `REF` simply gets quieter. At 0.15 s the cap bit at 6.7 cps,
/// which held every rate between 4 and 6.7 cps BELOW the flat line while
/// every rate above it sat ON it: A14's own 4 cps reference read 2.2 dB low,
/// and with the gate deleted — which doubles the note count at 8 cps — the
/// measured arc ran to **+3.2 dB re 4 cps**, straight through A14's +1 dB
/// ceiling and §21.4's "cuteness must not buy loudness". 0.25 s is
/// [`IOI_DEFAULT_MS`], which is 4 cps, which is the 0 dB reference §9.6
/// writes the whole table against; putting the cap's knee ON the reference is
/// what makes the arc flat from 4 cps up instead of from 6.7.
///
/// **The floor.** Under the gate the notes above ~17 cps were thinned anyway,
/// so a floor cost nothing; with every key now speaking, a floor at 0.6 is
/// the melody getting louder the faster you type. 0.45 is `√(0.050/0.25)` —
/// reached at 20 cps, past any prose hand and inside the rate where
/// [`AUTOREPEAT_FLOOR_MS`] has already taken the partials off the note.
const G_IOI_REF_S: f32 = 0.25;
const G_IOI_MIN: f32 = 0.45;

/// Seeded velocity, in dB either side of nominal (§9.6). Per-session
/// deterministic, never per-frame: the draw comes from the synth's own seeded
/// stream, so a script replays bit-exactly (A27).
const VEL_DB_STEP: f32 = 1.0;
const VEL_DB_RESTRIKE: f32 = 1.5;
const VEL_DB_MOTION: f32 = 0.8;
/// Seeded pan jitter, ±0.03 (§9.1) — enough to un-stack two voices on the same
/// column, far too little to move a sound off its glyph. Stated in §9.1's
/// units, i.e. AFTER the stereo law's `× 0.35`; [`TrailSynth::v2_pan`]
/// divides it back out so the jitter the ear gets is the one the table says.
const PAN_JITTER: f32 = 0.03;

/// **`KEY_TINE_TRIM`** — the one scalar the v2 loudness ladder rests on,
/// fitted so the ISOLATED STEP peaks at −21.0 dBFS at host volume 0.4 with
/// heat 0.5 (§9.1's trim row, A28).
///
/// Fitted on PEAK, per §9.1 and §9.6: the arc is allowed to buy brightness,
/// never loudness, so the quantity that must land on the ladder floor is the
/// one the ladder is written in. RMS and crest are then *watched*, not fitted
/// — `v2_the_tine_is_small_and_warm` pins the centroid and
/// `the_isolated_step_lands_on_the_ladder_floor` pins the peak.
///
/// **Fitted on the NOMINAL step** — §9.6's seeded ±1 dB velocity and ±0.03
/// pan jitter divided out (the pin does the division). 0.2741 re-fits the
/// 2026-09-05 capture: the previous 0.2591 had been fitted to the unit
/// fixture's own +0.96 dB velocity draw, so the nominal step sat 1.0 dB
/// under the floor and the bench's seed read it at −22.7 dBFS; the brighter
/// octave and strike ([`P2_LVL`], [`P3_LVL`]) then raised the onset peak by
/// 0.5 dB, and this is the residual.
const KEY_TINE_TRIM: f32 = 0.2741;

// ===========================================================================
// §9.3 / §10.3 — the bass tine, the breath, and the pure-fifth chord loop
// ===========================================================================

/// C4 — the bass register's own anchor. §9.4 puts BASS at 261.6–436.0 Hz, and
/// every root and every dyad partner of [`CHORD_LOOP`] lands inside it.
const BASS_BASE_HZ: f32 = 261.63;

/// The four roots, as exact ratios above [`BASS_BASE_HZ`] (§10.3):
/// C 1/1, A 5/3, F 4/3, G 3/2.
///
/// **Only C, F, G and A are used, and that is a tuning fact rather than a
/// taste.** They are the only roots whose fifth is PURE on this lattice: D–A
/// is 27/40 (a wolf) and E would need a B the pentatonic does not have. F is
/// 4/3 and is not itself a PENTA degree, which is exactly why the roots need
/// their own four-entry table instead of being read off the verse's.
const CHORD_ROOT_RATIO: [f32; 4] = [1.0, 5.0 / 3.0, 4.0 / 3.0, 3.0 / 2.0];

/// One entry of §10.3's eight-bar loop: `(root, dyad partner ratio, LIT_SET)`.
///
/// The partner is **3/2 above** the root where that still lands under C5, and
/// **4/3 below** otherwise, so every dyad is an exact 3:2 or 4:3 and every
/// voice of it sits inside [261.6, 436.0] (A6 asserts `P2/P1 ∈ {1.5, 0.75}`
/// exactly). `LIT_SET` is a bit per verse degree mod 5 (C D E G A = 0 1 2 3 4).
///
/// **The chord never transposes the verse.** It decides only which verse notes
/// are lit — chord tone: lit roof, 0 dB, hero-eligible — and which are passing
/// (plain roof, −2 dB). About 80 % of steps land lit.
#[derive(Clone, Copy, Debug)]
struct Chord {
    /// Index into [`CHORD_ROOT_RATIO`].
    root: usize,
    /// 1.5 (a pure fifth above) or 0.75 (a pure fourth below).
    partner: f32,
    /// Bit `d` set ⇒ verse degree `d` (mod 5) is a chord tone here.
    lit: u8,
}

/// I – vi – IV – V | I – V – vi – IV, the loop `chord` walks one step per
/// word (§10.3). It advances on the HEAD of every Space run, so the harmony
/// moves at the rate of the text's own words.
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

/// The chord a keyed Enter parks on (§10.3), so the **first** word boundary of
/// the next line lands on I. Without it every line's first bass is Am, which
/// is a minor colour on a fresh thought.
const CHORD_AFTER_ENTER: u8 = 7;

// ===========================================================================
// THE RAINBOW SKY — the drifting bed (THE PRISM §3.2)
// ===========================================================================

/// C3 — two octaves under the tine and a full octave under the bass dyad's
/// own band ([`BASS_BASE_HZ`] 261.63), so the word downbeat still reads as
/// the downbeat and the pad reads as the floor under it. Every pad tone is
/// `penta(BED_BASE_HZ, d)` for a lit degree `d`, so the top of the pad
/// (degree 4, 218 Hz) stays clear of the bass register by construction
/// (`the_sky_pad_is_the_live_chords_lit_degrees_under_the_bass`).
pub(super) const BED_BASE_HZ: f32 = TINE_BASE_HZ * 0.25;
/// The number of tournament chords the sky voices — [`CHORD_LOOP`]'s length,
/// exported so the parent's lattice law can walk them.
pub(super) const SKY_BED_CHORDS: usize = CHORD_LOOP.len();
/// Voicing balance, lowest tone first: the root-most tone carries, the upper
/// two colour. Static — the MOTION lives in pitch and in the hue.
const BED_WEIGHT: [f32; 3] = [1.0, 0.80, 0.65];
/// THE PAD NEVER STEPS. Portamento between chords, long enough that no
/// crossing is a step and short enough that a fast run through the words is
/// audibly moving. (`bed_chord_drift` uses 1.2 s against a 7.5 s bar; the sky
/// moves once per WORD, so it needs longer.)
const BED_GLIDE_TAU_S: f32 = 2.6;
/// The pad's level. Air, not a drone: noticed when it stops, never when it
/// starts. THE PRISM's number; on the audition harness (`bed_audition.rs`,
/// vol 0.4, the 20 s script) it read a bed RMS of about −44 dBFS, ≈ 8 dB
/// under the melody, and cleared the harness's −50 dBFS audibility line in
/// most 50 ms windows while typing — the measured figures are in the step's
/// report and in `target/bed-audition/c5-rainbow-sky.metrics.json`. The
/// owner sets it from here by ear.
///
/// **−3 dB ON THE PANEL'S Q6 RULING (2026-09-09; §9).** With the bed now on
/// by default it was isolated under the derived line by subtraction (bed-on
/// minus bed-off, same seed and script; residual-to-line correlation
/// −0.0003, so it really is the bed): −43.8 dBFS at 4 cps and −43.1 at 10,
/// which is only 7.8 and 6.6 dB under the line's own 50 ms medians, and over
/// −50 dBFS for 98-99 % of the take. Six decibels of headroom is not a floor
/// under a melody, it is a second voice. At 0.008_9 the sky sits near
/// −47 dBFS, ≥ 10 dB under the line at both speeds, and every judge kept it
/// ON: the walk's own pitch distribution is nearly flat (3.02 bits of a
/// possible 3.17, the tonic class only 18 % of keys), so this pad — voiced
/// from the same [`CHORD_LOOP`] the word heads snap to — is the only thing
/// in the mix saying where home is. It is not trimmed further for exactly
/// that reason.
const BED_LEVEL: f32 = 0.008_9;
/// A 12 s breath on the whole pad. Weather, not tremolo.
const BED_BREATH_HZ: f32 = 1.0 / 12.0;
const BED_BREATH_DEPTH: f32 = 0.35;
/// The hue's one-pole (τ, seconds) on the arc position the pad reads.
const BED_HUE_TAU_S: f32 = 1.2;
/// TILT — one one-pole over the pad sum (reusing `bed.lp1`). Red end of the
/// arc warm and closed, cyan end open and glassy: the colour you can see is
/// the colour you can hear.
///
/// **THE ARC MOVED DOWN UNDER THE MELODY (the panel's Q6 ruling,
/// 2026-09-09).** The level was only half the finding: 12 % of the bed's
/// power sat inside 450-1900 Hz, where the notes live, and the isolated
/// spectrogram drew continuous harmonic LINES through 500-1000 Hz rather
/// than a low carpet — so the lower body of every note stopped standing on
/// black. The cause was this ceiling: at the cyan end the pad's own lowpass
/// opened to 2600 Hz and let the upper partials of a 218 Hz pad tone through
/// into the melody's own register. 350 → 900 Hz puts the arc's WHOLE travel
/// under the band the line is played in — the red end is now further closed
/// than it was, not merely the cyan end less open, which is what keeps the
/// colour audible while the ceiling comes down: the pad's own brightness
/// still moves by more than the 1.3× `the_sky_follows_the_hue_in_tilt_and_
/// width_and_never_in_pitch` demands, and that pin is green unmoved (at
/// 450 → 900 it read 1.24× and would have had to be weakened, which is the
/// wrong way round — the law is the law and the constants answer to it).
/// The pad's centroid was already 262 Hz with
/// its strongest bins at 131-135 Hz, so this costs almost none of what it
/// is: it takes the sky out of the melody's way and leaves it a sky.
const BED_TILT_LO_HZ: f32 = 350.0;
const BED_TILT_HI_HZ: f32 = 900.0;
/// WIDTH — the top tone's twin is detuned by this many cents, so the pad's
/// own beat rate walks from ≈ 0.45 Hz (4 cents on G3, 196 Hz) to ≈ 1.4 Hz
/// (11 cents on A3, 218 Hz) as the ribbon travels the arc. That slow,
/// drifting beat IS the shimmer, and it costs one phase increment.
const BED_DETUNE_CENTS_LO: f32 = 4.0;
const BED_DETUNE_CENTS_HI: f32 = 11.0;
/// THE PAD'S PARTIALS, harmonic numbers at `1/n`: octaves, fifths and the
/// major third of each pad tone and nothing else — every partial is itself
/// a lattice interval from its tone, so the pad's whole spectrum sits on the
/// same consonances the melody does (the 7th harmonic, a flat seventh, is
/// left out on purpose). A pure sine has nothing for a tilt to tilt; these
/// are what the hue's filter acts on.
///
/// **THE SERIES STOPS AT 4 (the panel's Q6 ruling, 2026-09-09).** It ran to
/// the 8th, and a one-pole tilt is 6 dB an octave — far too gentle to take
/// a partial out of a band it is sitting in the middle of. On the pad's
/// highest tone (degree 4, 218 Hz) partials 5, 6 and 8 land at 1090, 1308
/// and 1744 Hz: not near the melody's register but IN it, on lattice degrees
/// the line itself plays. Truncating removes them outright, which the tilt
/// alone could not, and the four that remain reach 872 Hz at the very top —
/// under the arc's own new ceiling, so the sky is under the melody at every
/// point on the arc and by construction rather than by filtering.
const BED_PARTIALS: [u32; 4] = [1, 2, 3, 4];
/// `1 / sqrt(Σ 1/n²)` over [`BED_PARTIALS`], so a pad tone has the RMS of
/// the sine it replaces and [`BED_LEVEL`] means what it says. Re-derived for
/// the shortened series: `1/√(1 + 1/4 + 1/9 + 1/16)` = 0.838_1, against
/// 0.815_1 for the seven — so the trim above is the whole of the level
/// change and the truncation does not quietly add one.
const BED_PARTIAL_NORM: f32 = 0.838_1;

/// THE SKY'S VOICING for one chord of [`CHORD_LOOP`]: the chord's three LIT
/// verse degrees (`Chord::lit`, bits 0..5 = C D E G A), ascending, on the
/// UNTRANSPOSED lattice. The pad is voiced from the lit set rather than
/// stacked on `CHORD_ROOT_RATIO[root]` because a root triad built on G or A
/// puts a B, a C♯ or an F♯ under a C-pentatonic melody — out of key — while
/// the lit set is by definition the chord's tones that ARE on the lattice:
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
/// **111 ms, §22's A/B item 5 ("felt, not heard": root −6 / fifth −9 /
/// dur 300).** The item names the duration; the tail law (A13) then fixes
/// the decay, `300 / 2.7 = 111 ms`, so the dyad still ends at ≤ 7 % of peak.
/// Taken on the capture-after measurement — see [`BASS_LEVEL`].
const BASS_DECAY_S: f32 = 0.111;
/// The A/B item's 300 ms, as the tail law spells it: [`TAIL_DUR_PER_TAU`] ×
/// 111.
const BASS_DUR_S: f32 = TAIL_DUR_PER_TAU * BASS_DECAY_S;
const BASS_ROOF_HZ: f32 = 900.0;
/// Root −6 dB re the step, the fifth (0.22 / 0.32 under it) at −9 — §22's
/// A/B item 5, "felt, not heard", in place of §11's −3 / −6.
///
/// Taken on the capture-after measurement: the downbeat was ≈ 32 % of the
/// prose mix's energy (Σ gain²·τ over the bench's 60 s at 10 cps: BASS 0.673
/// × 160 ms against TUNE 3.66 × ≈ 55 ms), and the mix sat +1.7 dB over v1's
/// RMS. Half the level and two thirds the ring take the dyad to ≈ 11 % of the
/// mix — a floor you feel under the tune, not a second voice beside it.
const BASS_LEVEL: f32 = 0.501_187_2;

/// §9.3's breath — the air a space moves, and the only thing a run's TAIL
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

// ===========================================================================
// §11 — the small gestures: the nav tick and the Shift lift
// ===========================================================================

/// The NAV TICK (§11, D17): the verse note, P1 only, no mallet — the sound of
/// a hop too short to be a meteor. −24 dB: present, never an event.
const NAV_ATTACK_S: f32 = 0.004;
const NAV_DECAY_S: f32 = 0.030;
/// §11's 60 ms, raised to the tail law: [`TAIL_DUR_PER_TAU`] × 30.
const NAV_DUR_S: f32 = TAIL_DUR_PER_TAU * NAV_DECAY_S;
const NAV_LEVEL: f32 = 0.063_095_73;

/// THE LIFT IS FELT, NOT PITCHED (§3.1 "Bare Shift", 2026-09-08). A bare
/// Shift used to play a rotating pitched tine 30-100 ms before the letter it
/// preceded — a note with no character behind it, and the first of the three
/// onsets one capital used to cost. It is now the felt mallet alone, on the
/// lift's own envelope: a modifier stays felt and can never be mistaken for
/// a step. The rotation table (`SHIFT_LIFT`) and its cursor retired with the
/// pitch; nothing about how you reach for a capital can touch the tune.
const LIFT_ATTACK_S: f32 = 0.004;
const LIFT_DECAY_S: f32 = 0.040;
/// §11's 80 ms, raised to the tail law: [`TAIL_DUR_PER_TAU`] × 40.
const LIFT_DUR_S: f32 = TAIL_DUR_PER_TAU * LIFT_DECAY_S;
/// −9 dB re the step (§11) — a modifier is intent, not authorship…
const LIFT_LEVEL: f32 = 0.354_813_4;
/// …and the felt alone at 0.4 of that (−17 dB re the step): §3.1's figure.
const LIFT_FELT_MUL: f32 = 0.4;
/// The TUNE lane's degree span, `C5..G6` = 0..8 (§9.4). The lift folds into
/// it so a rotation off a high verse note cannot climb out of the register.
const TUNE_DEG_LO: i32 = 0;
const TUNE_DEG_HI: i32 = 8;

// ===========================================================================
// §13 — the stardust glint
// ===========================================================================

/// The glint's rotating interval above the verse note (§13). All three are
/// lattice degrees, so a glint either coincides EXACTLY with a live harmonic
/// or sits a consonant interval from it — it can never beat.
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
/// **THE SPARKLE ON EVERY KEY** (the panel's Q2 ruling, 2026-09-09; §9), as
/// a multiplier on [`GLINT_LEVEL`]: −24 dB re the step.
///
/// The owner asked for bright, cute and sparkly, and there were two places
/// to buy it. The BLOOM was the obvious one and it is the wrong one: the
/// floor measurements price bloom brightness at 3.5 dB of note separation at
/// 10 cps and 3.3 dB at 20, and legibility of the melody line is the one
/// brake the ruling keeps. The GLINT is free. It was already written, for
/// the hero star (§13): 2 ms of attack, a 40 ms decay, octave-folded into
/// [`GLINT_LO_HZ`]-[`GLINT_HI_HZ`], and on [`GLINT_DEGREES`] — lattice
/// degrees, so it can never beat with anything live. Its whole life is
/// [`GLINT_DUR_S`] 108 ms and its decay is gone inside 40, so at every
/// typing speed a key's glint has cleared before the next key strikes and
/// the trough between two notes does not move by a decibel. Short, high,
/// glassy, on every key, at every hue is the literal description of what was
/// asked for. It arrives at [`KEY_GLINT_DELAY_S`], with the bloom.
///
/// **IT DOES NOT READ THE HUE.** [`hue_air`] dims the bloom at the red end
/// by design (the colour you can hear); the sparkle is what the owner asked
/// to have everywhere, so the arc colours the hang and never the glint.
const KEY_GLINT_LEVEL_MUL: f32 = 0.5;
/// **AND A CAPITAL GETS TWICE AS MUCH OF IT** (the panel's Q4 ruling): a
/// shifted key's identity is a SPARKLE on its own note, which is what the
/// register was being made to say and could not — see [`CAPITAL_LIFT_DEG`].
/// It costs no register, no second onset, and, being a glint, nothing at
/// speed either.
const KEY_GLINT_SHIFTED_MUL: f32 = 2.0;

/// **AND IT ARRIVES WITH THE BLOOM, NOT BEFORE THE NOTE** (§9.6's loudness
/// law; fixed 2026-09-09) — [`BLOOM_DELAY_S`] + [`BLOOM_ATTACK_S`], which is
/// the instant the bloom opens on.
///
/// Between 81225c15e and this constant the sparkle was spawned at delay
/// zero, and its [`GLINT_ATTACK_S`] is 2 ms against the tine's
/// [`TINE_ATTACK_S`] 4 — so the highlight arrived BEFORE the note it was a
/// highlight of, and peaked inside the strike's own attack. A voice on the
/// crest adds to the crest, and this one is a sine at a folded lattice pitch
/// with no harmonic relation to the tine, so what it added was
/// PHASE-RANDOM: measured on the flowing word, +0.11 / +0.23 / −0.22 dB on
/// keys 1 / 2 / 3 from one identical voice. The quantity §22 and §9.1 are
/// both written in is a PEAK, and a peak is a max over keys, so jitter that
/// averages to nothing still only ever lifts whichever key already carried
/// the maximum. It took `the_box_opens_with_the_hand_and_never_gets_louder`
/// from −0.22 dB to +0.23.
///
/// Moving the arrival is the SPECTRAL fix, and that is why it is this one
/// and not a trim: the level, the pitch, the twinkle and the 40 ms decay are
/// all untouched, so the sparkle's energy and its whole spectrum survive
/// exactly — only its instant moves, out of the strike's attack and onto the
/// bloom's opening, where the composite is already 1.6 dB down and the same
/// voice can no longer make a new maximum. Trimming the level was measured
/// instead and rejected: the jitter is proportional to the level, so holding
/// it under `PEAK_EPS_DB` needs about −15 dB on the glint, which is not a
/// sparkle any more. Conserving it against the strike (scaling the tine's
/// partials down by the glint's share, as [`flow_partials`] conserves the
/// octave) was also measured and rejected: it moved the flowing word's peak
/// delta only +0.23 → +0.20 while taking 0.3 dB off the whole box, because
/// what it scales is the strike and what breaks the law is the jitter.
const KEY_GLINT_DELAY_S: f32 = BLOOM_DELAY_S + BLOOM_ATTACK_S;
/// Depth 0.5 gives 1–1.5 winks across the glint's life — the star's own
/// opening and closing, heard.
const GLINT_TWINKLE_DEPTH: f32 = 0.5;
/// The rate a glint takes when the glow did not name the star's own
/// scintillation frequency: the middle of D12's integer-stepped [8, 12] Hz.
const GLINT_TWINKLE_DEFAULT_HZ: f32 = 10.0;
const GLINT_LP_HZ: f32 = 8000.0;

// ===========================================================================
// §12 — the meteor
// ===========================================================================

/// Layer 1, TICK: band-passed noise, 4 ms of it, at the origin (§12.2).
const MET_TICK_HZ0: f32 = 3200.0;
const MET_TICK_HZ1: f32 = 900.0;
const MET_TICK_GLIDE_S: f32 = 0.004;
const MET_TICK_Q: f32 = 0.7;
const MET_TICK_ATTACK_S: f32 = 0.000_3;
const MET_TICK_DECAY_S: f32 = 0.001_5;
const MET_TICK_DUR_S: f32 = 0.004;
/// −24 dB re the step.
const MET_TICK_LEVEL: f32 = 0.063_095_73;

/// Layer 2, CORE — **the layer that carries direction**. A sine gliding an
/// octave up (rightward / upward) or down (leftward / downward) over the
/// flight, with a light inharmonic FM colour that dies with it.
///
/// The direction convention is `crate::rainbow_kitty::Dir::sign` — `+1` for a
/// rightward *or upward* move — which is the host's own sign and the one
/// `SoundCue::dir` already carries. §12.2's table and §12.3's prose disagree
/// about the vertical case; the frozen contract's `sign()` settles it, and
/// A17's assertion (Ctrl-E rising, Ctrl-A falling) is satisfied either way.
///
/// The core's octave is `[C5, C6)` — the lattice anchor and its double,
/// DERIVED so that retuning [`TINE_BASE_HZ`] moves the core with the tine.
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
/// The armed pre-cue's core level, −20 dB (§15.2) — quieter than the claimed
/// one, because an arm is a guess and a guess that turns out wrong has to be
/// forgivable. A CLAIM never steps it up: the level rides the pan glide from
/// this to [`MET_CORE_LEVEL`] over the flight ([`TrailSynth::v2_claim_arm`]).
const MET_ARM_CORE_LEVEL: f32 = 0.1;
/// The (default-off) arm's time to live, seconds (D11).
const MET_ARM_TTL_S: f32 = 0.150;
/// The least life a CLAIMED core keeps past the claim instant, seconds. An
/// arm that has sounded longer than the true flight's `1.9·T` would
/// otherwise be re-targeted to a `dur` it has already passed — a hard cut,
/// which no v2 voice may suffer (§9.5 law 2). Twice the 12 ms ramp: room for
/// the 5 ms release to be a release.
const MET_CLAIM_MIN_TAIL_S: f32 = 0.030;

/// Layer 3, WHOOSH — **direction-blind on purpose**: the rush of travel is the
/// same rush whichever way you went, and putting direction in two places is
/// how a gesture starts to disagree with itself.
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

/// Layer 4, BELL — the tine step, thrown across the line and landing **exactly
/// at `T`**. τ 80 / dur 260 (D19), +2 dB: the loudest routine gesture in the
/// theme, once per navigation.
const MET_BELL_TAU_S: f32 = 0.080;
const MET_BELL_DUR_S: f32 = 0.260;
const MET_BELL_LEVEL: f32 = 1.258_925_4;

/// Layer 5, THUMP — the bass tine on the current chord root, at the landing.
const MET_THUMP_ATTACK_S: f32 = 0.008;
const MET_THUMP_DECAY_S: f32 = 0.060;
/// §12.2's 120 ms, raised to the tail law: [`TAIL_DUR_PER_TAU`] × 60.
const MET_THUMP_DUR_S: f32 = TAIL_DUR_PER_TAU * MET_THUMP_DECAY_S;
/// −6 dB re the step.
const MET_THUMP_LEVEL: f32 = 0.501_187_2;

/// Layer 6, RAIN — five glints falling behind the landing, a descending
/// pentatonic in the STARDUST lane, ONE PER FAN STAR (1 m1 + 4 m2) in the
/// fan's own throw order (D6). `FAN_HERO_N` is the shared count.
const MET_RAIN_HZ: [f32; timing::FAN_HERO_N] = [5232.5, 4709.25, 4186.0, 3488.33, 3139.5];
/// Where the rain starts, relative to the landing.
const MET_RAIN_LEAD_S: f32 = 0.010;
const MET_RAIN_ATTACK_S: f32 = 0.002;
const MET_RAIN_DECAY_S: f32 = 0.028;
/// §12.2's 66 ms, raised to the tail law: [`TAIL_DUR_PER_TAU`] × 28. Five at
/// an 18 ms spacing overlap four deep, so the RAIN lane's cap of three is
/// what keeps "≤ 3 live" true — the fourth glint's onset fade-steals the
/// first, which by then is at 14 % of its peak.
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
/// 15 ms, not 12: the interrupted layers are *swept* noise and a *gliding*
/// tone, and a slightly longer ramp keeps the sweep from clicking as it dies.
const METEOR_INTERRUPT_S: f32 = 0.015;

// ===========================================================================
// §11 — the Enter cadence
// ===========================================================================

/// The pickup, a G5 at `t = 0` — the ONE cadence voice that does not wait for
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
/// The tonic dyad, C4 + G4 — home, in the bass, −3 dB.
const CAD_DYAD_LEVEL: f32 = 0.707_945_8;
/// The faraway ICE BELL (§11). Sine C6 with a light inharmonic FM colour: its
/// sidebands are deliberately NOT lattice pitches, which §9.5 law 4 exempts
/// precisely because "faraway" is what an unrelated partial sounds like.
/// C6 — the lattice anchor's octave, DERIVED from [`TINE_BASE_HZ`].
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

/// THE BRRRRING (D18): the cascade's four notes, their offsets in seconds and
/// their levels re the step. `walk, +2, +3, +5` at 0 / 45 / 90 / 135 ms.
const CASCADE_DEGREES: [i32; 4] = [0, 2, 3, 5];
const CASCADE_DELAYS_S: [f32; 4] = [0.0, 0.045, 0.090, 0.135];
/// 0 / −10.5 / −11.4 / −12.4 dB (§11).
const CASCADE_LEVELS: [f32; 4] = [1.0, 0.298_538_3, 0.269_153_5, 0.239_883_3];
/// A Jump landing INSIDE a live cascade re-strikes the top note only, −12 dB.
const CASCADE_RESTRIKE_DEG: i32 = 5;
const CASCADE_RESTRIKE_LEVEL: f32 = 0.251_188_6;

// ===========================================================================
// THE VERDICT — a command's exit code, sounded once, on the `D` that carries
// it. Armed by the keyed Enter and spent by the first `D`; a `D` with no
// armed Enter never reaches this file at all (the host's gate), because
// program output must never speak.
// ===========================================================================

/// THE PLAGAL FALL, as indices into [`CHORD_LOOP`]: **IV (F4 + C4) → I (C4 +
/// G4)**. Both chords are already in the loop — bar 2 and bar 0 — so the
/// verdict adds not one pitch the typing has not been playing all along. What
/// it adds is an ORDER the eight-bar loop never walks: IV straight to I is
/// the one cadence the tournament `I vi IV V | I V vi IV` cannot produce,
/// which is why it can mean something the typing does not already mean.
const VERDICT_PLAGAL: [usize; 2] = [2, 0];
/// The **vi (A4 + E4)** — the RED verdict. Bars 1 and 6 of the loop: the
/// minor is not a new colour, it is one you have been hearing all session,
/// arriving alone and out of turn.
const VERDICT_MINOR: usize = 1;
/// The IV→I fall, seconds. Longer than the bass's own decay
/// ([`BASS_DECAY_S`] 0.111 s) so the two chords are two chords and not a
/// cluster, and short enough that the pair is one gesture: "amen", at the
/// speed people actually say it.
const VERDICT_PLAGAL_S: f32 = 0.26;
/// Where a RED verdict parks the chord loop, so the FIRST word boundary of
/// whatever you type next advances onto [`VERDICT_MINOR`]: **the next word
/// sings against the minor.** A green verdict parks at [`CHORD_AFTER_ENTER`]
/// instead, exactly as a keyed Enter does, and the resolution holds.
const CHORD_AFTER_RED: u8 = 0;
/// The green dyads' level re the step. [`BASS_LEVEL`]'s own −6 dB, which is
/// the word downbeat's level, which is the loudest a once-per-command gesture
/// is permitted to be — stated THROUGH the downbeat's constant rather than as
/// a fresh number, so the ceiling and the thing it is measured against can
/// never drift apart.
const VERDICT_DYAD_LEVEL: f32 = BASS_LEVEL;
/// **THE GESTURE'S VOICE TRIM** — one decibel off EVERY verdict voice (the
/// bonk's idiom: "the kind gain is not the knob for this"). The levels above
/// are each stated re the step in the file's own units, but what the ear gets
/// is a bass DYAD with a tine over it, and three partials plus a strike sum
/// to a peak above any one of them: measured on the shipped instrument, an
/// untrimmed green cadence delivers **−5.14 dB** re a keystroke — over the
/// ceiling this gesture is allowed. A flat −1 dB puts the DELIVERED peak at
/// **−6.14 dB**, which is the law as an OUTCOME, pinned by
/// `the_verdict_is_the_music_boxs_alone_and_never_louder_than_a_key`.
const VERDICT_PEAK_TRIM: f32 = 0.891_250_9;
/// The RED dyad, −9 dB: three under the green. A failure is TOLD, not
/// announced — you already know; the cat is only agreeing with you.
const VERDICT_MINOR_LEVEL: f32 = 0.354_813_4;
/// The one tine over the resolution: **E5**, degree 2 of the lattice. It is
/// the major seventh of the F that is leaving and the third of the C it lands
/// on — one note that belongs to both chords, which is exactly why the pair
/// resolves rather than merely stopping. −8 dB re the step.
const VERDICT_TINE_DEG: i32 = 2;
const VERDICT_TINE_LEVEL: f32 = 0.398_107_2;
/// **THE VERDICT'S HOT KEY** (sense 3, the tine's half): a typed key this
/// soon after a green long verdict is LIT whatever the chord thinks — the
/// audible twin of [`super::super::rainbow_kitty::spine::VERDICT_WINDOW_S`],
/// and the same five seconds in the melody's own millisecond clock.
const VERDICT_LIT_MS: u32 = 5_000;

// ===========================================================================
// §10.1 — MelodyV2: the state
// ===========================================================================

/// WHICH OF THE TINE'S THREE TOUCHES a key gets (§9.2).
///
/// The whole of §9.0's first cure is that these are the ONLY three, and that
/// two of them are the same pitch. v1's fourth "touch" — the ghost, an octave
/// or a fourth or a third away on the identical bell — is not carried, and
/// cannot be: nothing in this module can voice a degree the line did not
/// derive.
///
/// **[`Touch::ReStrike`] now means what its own name says.** Under the
/// deleted step gate it meant "you typed too fast", which is how a fast hand
/// came to truncate its own notes; it is now reached only through
/// [`stride_mag`]'s single zero, which is a DOUBLED LETTER — a real musical
/// repeat, from a real repeat in the text. Every other way the derivation
/// could arrive back on the sounding degree (a fold to zero, gravity
/// cancelling a second, a reflection landing where it started) is closed in
/// [`MelodyV2::derive`] on purpose. So the same-pitch damp is defending
/// against a real comb filter instead of punishing a typist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Touch {
    /// The line moved. Full tine: body, octave, strike, mallet.
    Step,
    /// The line derived the pitch it is already on. The tine with the strike
    /// partial at zero, at `L_n` — a music-box tremolo, never a leap.
    ReStrike,
}

/// ONE FRAME OF MELODY STATE, saved before a keystroke mutates it so that a
/// Backspace can put it back (§10.4).
///
/// A Backspace UN-SINGS: "type five, delete five, type five again" must play
/// five notes, not ten. v1 declined to *advance* the song on a deletion, which
/// stops the tune running ahead of the text but cannot rewind it; the undo
/// stack is what makes the correction actually walk backwards through the
/// notes you mistyped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Undo {
    walk: i8,
    restrike: u8,
    chord: u8,
    word_pos: u8,
    /// THE DERIVED LINE'S WHOLE STATE (§3.1). The melody is now a function of
    /// the text, so un-writing a character has to un-write everything that
    /// character decided: the rank the next interval is measured against, the
    /// run the contour was building, and the subject the line had latched.
    /// Anything left off this frame is a way for "type five, delete five,
    /// type five again" to come back a different tune.
    prev_rank: u8,
    run_stride: i8,
    run_len: u8,
    motif: [i8; MOTIF_LEN],
    motif_len: u8,
    motif_play: u8,
    motif_k: u8,
    words_since_motif: u8,
}

/// **THE MELODY ENGINE** (§10) — `Copy`, alloc-free, deterministic, and driven
/// entirely by `EventMeta::at_ms`.
///
/// Time comes from the host's INPUT clock and never from audio-thread arrival
/// gaps, so the keyed path and the frame path cannot jitter a phrase boundary
/// between them (§10.1). Nothing in here reads a clock, allocates, or draws
/// randomness: given the same events, the same seed and the same stamps, it
/// produces the same notes (A27).
#[derive(Clone, Copy, Debug)]
pub struct MelodyV2 {
    /// THE SOUNDING DEGREE, 0..8 (C5..G6). Every pitched thing in v2 that is
    /// not a bass root is stated relative to this, and [`MelodyV2::derive`]
    /// is the only thing that moves it on a keystroke.
    walk: i8,
    /// HOW MANY KEYSTROKES HAVE MOVED THE MELODY — one per typed key, with
    /// no exception anywhere in this file (test / introspection hook, and
    /// the census's `steps` column).
    steps: u32,
    /// `at_ms` of the last admitted key (any touch) — the IOI's clock.
    last_key_ms: u32,
    /// `at_ms` of the last typed key that actually SPAWNED a tune voice.
    last_onset_ms: u32,
    /// Keys since the line last moved, 1-based inside [`RESTRIKE_L0`]'s
    /// ladder — a doubled letter's tremolo, not a fast hand's.
    restrike: u8,
    /// THE PREVIOUS KEY'S ALPHABET RANK ([`EventMeta::rank`]), and the left
    /// operand of the interval. `0` is "nothing to measure against yet", and
    /// a cue that carries no rank never clobbers it: an echo-born cue must
    /// not make the next real letter leap.
    prev_rank: u8,
    /// The stride the line is repeating, and how many times running —
    /// [`MELODY_RUN_MAX`]'s whole state.
    run_stride: i8,
    run_len: u8,
    /// THE LINE'S OWN SUBJECT: the first [`MOTIF_LEN`] intervals since the
    /// last Enter or rest, and how many of them have been written.
    motif: [i8; MOTIF_LEN],
    motif_len: u8,
    /// How many of the subject's intervals are still to be answered, and
    /// where in it the answer has reached.
    motif_play: u8,
    motif_k: u8,
    /// Word heads since the subject was last answered ([`MOTIF_EVERY_WORDS`]).
    words_since_motif: u8,
    /// WAS THE LAST TYPED KEY SHIFTED — the only thing [`CAPITAL_LIFT_DEG`]
    /// needs in order to lift a capital and not a run of them. Like the
    /// auto-repeat state below it describes the hand rather than the text.
    prev_shifted: bool,
    /// THE AUTO-REPEAT DETECTOR'S STATE: the previous raw gap and how many
    /// gaps of the live machine-regular run have agreed. Deliberately NOT on
    /// the undo frame — it describes the hand on the key, not the text, and a
    /// Backspace does not un-hold a key.
    prev_gap: u32,
    repeat_run: u8,
    /// Inter-onset interval, EMA'd, clamped 30..600 ms.
    ioi_ms: f32,
    /// Position in [`CHORD_LOOP`], advanced by Space run heads only.
    chord: u8,
    /// Letters since the last word boundary. Read by the `?`/`!` grafts and
    /// carried on the undo stack so a correction restores the word too.
    word_pos: u8,
    /// True while inside a whitespace RUN: only its head is a downbeat.
    space_run: bool,
    /// THE TIMBRE LADDER'S STOPS (§7 step 3) — which of §3.3's additions
    /// this synth voices. All on in production; the bench pulls them one at a
    /// time so the owner hears one variable per file.
    stops: TimbreStops,
    /// **FLOW HEAT FOR THE CUE BEING VOICED** (§22), 0..1 — latched from
    /// [`EventMeta::flow`] at the top of [`TrailSynth::push_v2`] and read by
    /// every gesture that push mints.
    ///
    /// It lives here rather than being threaded through nine signatures
    /// because it is a property of the INSTRUMENT at this instant and not of
    /// any one note: the same number prices the step's octave partial, the
    /// downbeat's decay and the echo, and a per-call parameter would be the
    /// same number spelled three ways. It is NOT melody state — nothing
    /// derives from it, no undo frame carries it, and a Backspace does not
    /// rewind it — so it is written on every push and never read at render
    /// time.
    flow: f32,
    /// Position in [`GLINT_DEGREES`].
    glint_k: u8,
    /// The last line feed of the live cascade RUN (D18) — see
    /// [`MelodyV2::on_jump`] for why this tracks the last JUMP and not the
    /// last cascade.
    cascade_at: u32,
    /// FALSE until the first line feed. Without it the first Jump of a
    /// session would read `at − cascade_at = at` against a `cascade_at` of
    /// zero that no cascade ever set, and a line feed inside
    /// [`CASCADE_EXCLUSIVE_MS`] of the clock's zero — every host that stamps
    /// nothing, whose fallback clock starts at 0 — would have its first
    /// brrrring swallowed as a re-strike of a cascade that never played
    /// (found by the roster sweeps when the music box joined them, §17.3
    /// phase 7). The same guard [`MelodyV2::seen_key`] gives the first key.
    seen_jump: bool,
    /// The last top-note re-strike inside a live cascade.
    cascade_restrike_ms: u32,
    /// Keys since the last keyed Enter — [`ENTER_PICKUP_MIN_KEYS`] decides
    /// whether a Return is a cadence or a bare tonic dyad.
    keys_since_enter: u8,
    /// `at_ms` of the last keyed Enter, and whether there has been one — the
    /// clock [`ENTER_ECHO_SWALLOW_MS`] is measured on.
    last_enter_ms: u32,
    seen_enter: bool,
    /// FALSE until the first admitted key. Without it a session's first
    /// keystroke would see `at − last_key_ms = at`, i.e. an enormous gap, and
    /// cadence a phrase that has not been played yet.
    seen_key: bool,
    /// The undo stack (§10.1), newest last.
    undo: [Undo; UNDO_N],
    undo_len: u8,
    /// THE NEWEST TUNE VOICE — `(slot, born)`, so a Backspace's 40 ms mute and
    /// a re-strike's 12 ms damp address the voice they mean and not whatever
    /// later took its slot.
    lead: Option<(u8, u32)>,
    /// The live bass voice, same addressing (the downbeat is monophonic).
    bass: Option<(u8, u32)>,
    /// **THE VERDICT'S OPEN DOOR** (sense 3, the tine's half): `at_ms` of a
    /// green long verdict, spent by the next typed key
    /// ([`MelodyV2::take_verdict_lit`]). Deliberately NOT on [`Undo`]: the
    /// door was opened by the shell and walked through by a key, and
    /// un-singing that key does not un-finish the build.
    verdict_at_ms: Option<u32>,
    /// The meteor's scheduled bell and thump. A new meteor CANCELS these while
    /// they are still unsounded and leaves them alone once they have spoken:
    /// "a bell that has already sounded is never damped" (§12.3).
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
            // The lattice tonic, so a session's first key opens on home even
            // before anything has been derived.
            walk: 0,
            steps: 0,
            last_key_ms: 0,
            last_onset_ms: 0,
            restrike: 0,
            prev_rank: 0,
            run_stride: 0,
            run_len: 0,
            prev_shifted: false,
            motif: [0; MOTIF_LEN],
            motif_len: 0,
            motif_play: 0,
            motif_k: 0,
            words_since_motif: 0,
            prev_gap: 0,
            repeat_run: 0,
            ioi_ms: IOI_DEFAULT_MS,
            // Parked where a keyed Enter parks it, so the session's FIRST
            // word boundary lands on I exactly as every later line's does
            // (§10.3: "without that, every line's first bass is Am" — and a
            // session's first line is a line).
            chord: CHORD_AFTER_ENTER,
            word_pos: 0,
            space_run: false,
            stops: TimbreStops::ALL,
            flow: 0.0,
            glint_k: 0,
            cascade_at: 0,
            seen_jump: false,
            cascade_restrike_ms: 0,
            keys_since_enter: 0,
            last_enter_ms: 0,
            seen_enter: false,
            seen_key: false,
            undo: [Undo {
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
            }; UNDO_N],
            undo_len: 0,
            lead: None,
            bass: None,
            verdict_at_ms: None,
            bell: None,
            thump: None,
        }
    }

    /// THE SOUNDING VERSE DEGREE (test / introspection hook).
    #[must_use]
    pub fn walk(&self) -> i8 {
        self.walk
    }

    /// HOW MANY KEYSTROKES HAVE MOVED THE MELODY (test / introspection hook).
    ///
    /// **This is R1's whole falsifiable content.** It is incremented once per
    /// typed key, unconditionally, at the one place a key enters the melody —
    /// so a census that pushes `n` typed keys and reads anything but `n` here
    /// has caught a gate growing back. There is no branch it can miss,
    /// because there is no branch.
    #[must_use]
    pub fn steps(&self) -> u32 {
        self.steps
    }

    /// The live chord's index into [`CHORD_LOOP`] (test / introspection hook).
    #[must_use]
    pub fn chord(&self) -> u8 {
        self.chord
    }

    /// Letters since the last word boundary (test / introspection hook).
    #[must_use]
    pub fn word_pos(&self) -> u8 {
        self.word_pos
    }

    /// TRUE WHILE THE LINE IS ANSWERING ITS OWN SUBJECT — the next key will
    /// replay one of [`MOTIF_LEN`] latched intervals rather than derive its
    /// own (test / introspection hook).
    ///
    /// A census cannot otherwise tell an answered interval apart from a
    /// derived one, and the difference decides how a repeated pitch is
    /// CHARGED: a subject that latched a unison and is replaying it is the
    /// line answering itself, which §3.1 asks for, where the same repeat
    /// arriving from nowhere is the line stalling, which §3.1 forbids. The
    /// bench's repeat ledger reads this hook to keep those two apart, and
    /// §3.3's bloom will want the same notes named.
    #[must_use]
    pub fn motif_answering(&self) -> bool {
        self.motif_play > 0
    }

    /// The smoothed inter-onset interval in ms (test / introspection hook).
    #[must_use]
    pub fn ioi_ms(&self) -> f32 {
        self.ioi_ms
    }

    /// WHEN A TYPED KEY LAST SPAWNED A TUNE VOICE, on the host input clock
    /// (test / introspection hook).
    ///
    /// Written by [`TrailSynth::v2_typed`] AFTER the spawn returned a slot,
    /// so it records what the mixer actually did rather than what the melody
    /// intended. A key that leaves this where it was made no sound — which,
    /// with the re-strike coalescer deleted, can now only mean the TUNE lane
    /// refused the voice. The census's `silent` column is exactly this test,
    /// and it is meant to read 0 for ever.
    #[must_use]
    pub fn last_onset_ms(&self) -> u32 {
        self.last_onset_ms
    }

    /// TRUE AT A STRUCTURAL BOUNDARY of the line, at time `at`: a word head
    /// (nothing typed since the last Space, Enter or line feed) or a rest
    /// (nothing typed for [`PHRASE_PAUSE_MS`]).
    ///
    /// These are the two boundaries the derived line still has, and the
    /// handback needs BOTH. A word head alone is the finer and the usual one,
    /// but a stream with no spaces in it — a pasted base64 blob, a password
    /// field — would never reach one, and the borrowed key would be held for
    /// as long as that stream ran.
    fn at_boundary(&self, at: u32) -> bool {
        self.word_pos == 0
            || (self.seen_key && at.saturating_sub(self.last_key_ms) >= PHRASE_PAUSE_MS)
    }

    fn push_undo(&mut self) {
        let frame = Undo {
            walk: self.walk,
            restrike: self.restrike,
            chord: self.chord,
            word_pos: self.word_pos,
            prev_rank: self.prev_rank,
            run_stride: self.run_stride,
            run_len: self.run_len,
            motif: self.motif,
            motif_len: self.motif_len,
            motif_play: self.motif_play,
            motif_k: self.motif_k,
            words_since_motif: self.words_since_motif,
        };
        if usize::from(self.undo_len) == UNDO_N {
            // Drop the OLDEST frame: a correction can walk back a word, and a
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
        let f = self.undo[usize::from(n)];
        self.undo_len = n;
        self.walk = f.walk;
        self.restrike = f.restrike;
        self.chord = f.chord;
        self.word_pos = f.word_pos;
        self.prev_rank = f.prev_rank;
        self.run_stride = f.run_stride;
        self.run_len = f.run_len;
        self.motif = f.motif;
        self.motif_len = f.motif_len;
        self.motif_play = f.motif_play;
        self.motif_k = f.motif_k;
        self.words_since_motif = f.words_since_motif;
    }

    /// Is `deg` a chord tone of the live chord (§10.3)?
    fn deg_is_lit(&self, deg: i32) -> bool {
        let pc = deg.rem_euclid(5) as u8;
        CHORD_LOOP[usize::from(self.chord)].lit & (1 << pc) != 0
    }

    /// Is the sounding degree a chord tone of the live chord (§10.3)?
    fn lit(&self) -> bool {
        self.deg_is_lit(i32::from(self.walk))
    }

    /// **THE SNAP** (§3.1 step 6, and the rest's own resolution): the degree
    /// lit by the live chord NEAREST to `deg`, searched outward to
    /// [`WORD_HEAD_SNAP_DEG`].
    ///
    /// This is the bounded search the rest cadence used to carry privately,
    /// lifted out so a word head and a resolution use ONE law. `from` is the
    /// degree the line is coming from and it breaks ties: at equal distance
    /// the snap takes the side that CONTINUES the derived contour, so a
    /// rising word head that has to move still rises.
    ///
    /// Total: five consecutive degrees carry all five pitch classes, every
    /// chord of [`CHORD_LOOP`] lights three of them, and the register's own
    /// bounds cut at most two candidates off one side — so a lit degree is
    /// always inside ±2 and the `deg` fallback below is unreachable. It is
    /// kept because a function that lights the chord differently must fail
    /// loudly in a test rather than silently return an unlit note.
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

    /// RE-LATCH THE SUBJECT (§3.1): the next [`MOTIF_LEN`] intervals will be
    /// this line's new motif, and nothing of the old one is answered. Run by
    /// an Enter and by a rest, which are the two places a line ends.
    fn relatch_motif(&mut self) {
        self.motif_len = 0;
        self.motif_play = 0;
        self.motif_k = 0;
        self.words_since_motif = 0;
    }
}

/// THE WIDEST INTERVAL A WORD MAY CARRY, in lattice degrees: four — a major
/// sixth (5/3). Five is the octave, and an octave-class leap between two
/// keys of one word is the defect this instrument exists to cure (§9.0
/// cause 1, A2).
///
/// [`MelodyV2::derive`] CLAMPS its stride to this, which is what makes A2 a
/// property of the arithmetic rather than of the table it used to read:
/// [`stride_mag`] tops out at 3 (a fifth) and gravity may add one, so the
/// clamp is the guard on that sum and the one place the law is written. The
/// word-head snap can move a further two degrees, and may: a word head is
/// the letter AFTER a space, so the interval it widens is a between-word
/// interval, which A2 has never governed.
const WORD_LEAP_MAX_DEG: i32 = 4;

/// What one admitted `Typed` asks the synth to spawn — the melody's whole
/// output for a keystroke, resolved before a single voice is built.
#[derive(Clone, Copy, Debug)]
struct TypedPlan {
    touch: Touch,
    /// The verse degree, before the borrowed `song_key`.
    deg: i32,
    lit: bool,
    /// Level re the step, BEFORE the loudness arc's `g_IOI`.
    level: f32,
    /// THE FELT TOUCH: the tine's partials go to zero and the mallet alone
    /// speaks. Two callers — a live sing-along's re-strike (§10.2, so a held
    /// key cannot machine-gun under the 150 BPM riff) and macOS auto-repeat
    /// ([`AUTOREPEAT_JITTER_MS`]).
    ///
    /// **It is not silence and never becomes silence.** A held key is still
    /// the human playing; it is heard as *felt* rather than *pitched*, which
    /// is what R1 costs to honour, honoured.
    mallet_only: bool,
    /// THE SAME PITCH AGAIN, whatever the touch: a doubled letter's re-strike
    /// or a word head's common tone. §9.5 law 5 damps the previous voice on
    /// this, not on the touch — two independently phased sines at one
    /// frequency comb whether the second is a tremolo or an accent.
    repeat: bool,
    /// This key is the first letter of a word.
    word_head: bool,
    /// This key ended a phrase rest ([`PHRASE_PAUSE_MS`]) and resolved the
    /// line before sounding — the room answers it (§3.3's air cloud).
    rest: bool,
    /// This word head opened the line's answer to its own subject — the
    /// answering voice sounds behind it (§3.3).
    answer_head: bool,
}

/// WHICH OF §3.3's ADDITIONS THIS SYNTH VOICES — the timbre ladder's stops
/// (§7 step 3: `plain` / `bloom` / `hue` / `room`, one variable per file).
/// Production is [`TimbreStops::ALL`]; the bench renders the rungs. A stop
/// that is out spawns nothing and reads no hue, so `plain` is the shipped
/// tine byte for byte.
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
    /// **ON TYPED**, entire (§3.1). Advances the state and returns what to
    /// play; it spawns nothing itself, so the whole melody law is testable
    /// without a synth.
    ///
    /// **ONE KEYSTROKE IS ONE MELODY STEP.** There is no branch on this path
    /// that leaves [`Self::walk`] underived or [`Self::steps`] unincremented,
    /// at any typing rate, for any glyph, held key or not. Everything the
    /// deleted 220 ms gate was buying — a line that stays conjunct, stays in
    /// register, and does not run away under a fast hand — is bought here by
    /// note CHOICE instead, in [`Self::derive`].
    /// **THE VERDICT ARRIVED** (§ THE VERDICT). `green_long` opens the hot
    /// key's door; the chord is parked so the next word boundary lands where
    /// the verdict wants it — on I after a green one ([`CHORD_AFTER_ENTER`],
    /// the keyed Enter's own park), on the vi after a red one
    /// ([`CHORD_AFTER_RED`]), which is the whole of "the next word sings
    /// against the minor".
    ///
    /// The PLAYHEAD IS UNTOUCHED, exactly as a Space leaves it: a command
    /// finishing is not a keystroke, so it may not move the tune. It may only
    /// decide what the tune's next word is heard against.
    fn note_verdict(&mut self, at: u32, failed: bool, green_long: bool) {
        self.chord = if failed {
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

    fn on_typed(&mut self, at: u32, rank: u8, sing: bool, shifted: bool) -> TypedPlan {
        let gap = at.saturating_sub(self.last_key_ms);
        let first = !self.seen_key;
        self.push_undo();

        // MACHINE REGULARITY, read on the RAW gaps before the EMA smooths
        // them away. It decides the TOUCH and never the step.
        let held = self.detect_autorepeat(gap, rank, first);

        // A REST RESOLVES THE LINE, and that is all it does. It resolves the
        // walk onto the live chord where it stands, drops the contour bias
        // and re-latches the subject — then this key derives its own note
        // from the resolved position exactly as any other key would. Nothing
        // here can consume the keystroke.
        let rest = !first && gap >= PHRASE_PAUSE_MS;
        if rest {
            let here = i32::from(self.walk);
            self.walk = self.nearest_lit_within(here, here) as i8;
            self.run_stride = 0;
            self.run_len = 0;
            self.relatch_motif();
        }

        // THE WORD HEAD, decided BEFORE `word_pos` moves: this key is the
        // first letter of a word iff nothing has been typed since the last
        // Space, Enter or line feed.
        let word_head = self.word_pos == 0;
        let mut answer_head = false;
        if word_head {
            self.words_since_motif = self.words_since_motif.saturating_add(1);
            // THE ANSWER (§3.1). A latched subject is answered at every
            // MOTIF_EVERY_WORDS-th word head: this head snaps to a chord tone
            // as every head does, and the keys after it replay the subject's
            // intervals from wherever that landed — the transposition the
            // design asks for, for free.
            if usize::from(self.motif_len) == MOTIF_LEN
                && self.words_since_motif >= MOTIF_EVERY_WORDS
            {
                self.motif_play = MOTIF_LEN as u8;
                self.motif_k = 0;
                self.words_since_motif = 0;
                answer_head = true;
            }
        }

        let from = i32::from(self.walk);
        let mut deg = self.derive(rank, gap, first, word_head);
        if word_head {
            // WORD HEADS LAND LIT (§3.1 step 6). Interiors may pass, and a
            // passing note sounds at PASSING_LEVEL — the brightness law
            // already in place below.
            deg = self.nearest_lit_within(deg, from);
            // …BUT THE SNAP MAY NOT COMPLETE A MACHINE'S RUN. `derive` has
            // already inverted a fourth identical stride; the snap can move
            // the head two degrees and hand that stride straight back. When
            // it would, the head takes the nearest chord tone on the OTHER
            // side of the line: three of five classes are lit, so one is
            // always inside three degrees unless the register's bound cuts
            // it off — and then the snap stands, which is the rare case the
            // render's siren verdict exists to notice.
            let sounded = deg - from;
            if sounded != 0
                && self.run_len >= MELODY_RUN_MAX
                && i32::from(self.run_stride) == sounded
            {
                let away = -sounded.signum();
                if let Some(alt) = (1..=WORD_HEAD_SNAP_DEG + 1)
                    .map(|k| from + away * k)
                    .find(|c| (TUNE_DEG_LO..=TUNE_DEG_HI).contains(c) && self.deg_is_lit(*c))
                {
                    deg = alt;
                }
            }
        }
        // A CAPITAL IS THE SAME STEP, LIFTED ([`CAPITAL_LIFT_DEG`]) — applied
        // BEFORE the run is booked and BEFORE the walk moves, so the line
        // continues from the note the ear got and the siren verdict counts
        // the stride it heard.
        //
        // **AND IT REFLECTS AT THE CEILING, IT DOES NOT CLAMP** (the panel's
        // Q4 ruling, 2026-09-09). This was the one place in this file that
        // broke its own step-4 law — "a clamp pins against the ceiling and
        // reads as a stuck siren; a reflection turns around and reads as a
        // phrase" — and the shift scene is what a stuck siren looks like on
        // a roll: 11 of 21 capitals landing on the single degree 8. Under
        // [`reflect_deg`] a lift that would leave the register turns back
        // into it, so a run of capitals keeps a contour instead of stacking
        // on one pitch, and the accent survives at every degree the walk can
        // be standing on. The capital's IDENTITY moved out of the register
        // and into the light, where it costs nothing:
        // [`KEY_GLINT_SHIFTED_MUL`].
        // Inside a word the lift is held to A2's in-word bound, measured from
        // the note before: an accent, never an octave-class leap between two
        // letters of one word.
        // A doubled capital is still a doubled letter (the repeat stands),
        // and a capital already at the ceiling is not lifted ONTO the note
        // it came from: the lift may raise a step, never manufacture a
        // repeat the text did not type.
        //
        // **BUT A WORD HEAD'S COMMON TONE IS NOT A REPEAT THE TEXT TYPED**,
        // and refusing to lift it left a capital with no accent at all. The
        // `deg == from` test was standing in for "this key is a doubled
        // letter"; at a word head it catches something else entirely — the
        // chord snap landing the head on the degree the previous word ended
        // on, which §3.1 step 6 and the `repeat` ledger below BOTH already
        // rule is voice leading and not a re-strike. So the head is exempt,
        // by the same word the rest of this function uses: a shifted word
        // head is lifted whether or not the snap happened to land it home,
        // and a doubled letter is still never lifted.
        //
        // **A LIFT THAT LANDS BACK ON THE NOTE THE LINE CAME FROM TAKES ONE
        // FURTHER DEGREE**, which is the repair `derive`'s own reflection
        // makes two branches up, for the same reason and in the same words:
        // the line moved, and a capital is an accent. Without it the lift is
        // simply cancelled — and a cancelled lift is a capital that sounds
        // exactly like its lowercase twin, which is the one thing this whole
        // branch exists to prevent.
        //
        // **AND IT IS THE TRANSITION INTO SHIFT THAT LIFTS, NOT EVERY
        // SHIFTED KEY** (the panel's Q4 ruling, 2026-09-09, on measurement
        // the panel took twice). A lift applied to every shifted key is not
        // an accent at all: it is a permanent transposition, and a
        // transposition that starts from a walk centred on
        // [`MELODY_CENTRE_DEG`] 3 spends a run of capitals jammed into the
        // top of an eight-degree register with nowhere to go. Cutting the
        // lift did not cure it and neither did reflecting instead of
        // clamping — both were tried on the engine, and ALL CAPS still spent
        // 20 of 35 keys on the top two degrees with the register's whole
        // lower half unused. An accent is a thing that happens ONCE, where
        // the hand changes, so that is where it happens: the first shifted
        // key after an unshifted one is lifted, and the ones after it derive
        // their own line exactly as lower case does. Title Case is untouched
        // by this — every capital in it is a transition — and SHOUTING keeps
        // its own contour, sitting above the spoken line because its opening
        // lift put it there rather than because every key was pinned. Its
        // identity is carried where it costs no register at all:
        // [`KEY_GLINT_SHIFTED_MUL`] doubles the sparkle on every shifted key,
        // including all the ones this branch now leaves alone.
        //
        // **A CAPITAL LIFTS WHERE IT OPENS SOMETHING** — a shifted run, or a
        // WORD. The second half is not a softening of the first, it is the
        // same rule read at the boundary §3.1 puts every other accent on: in
        // a fully shouted line the shift is never released, so a transition
        // test alone lifted nothing after the first key and SHOUTING came
        // out pitched exactly like speaking (measured: identical means over
        // the pangram). One lifted note per shouted word is the accent — the
        // word head is where the chord snap, the answer and the line's own
        // arcs already land — and every interior capital derives its own
        // line, which is what keeps the register open.
        let opens = word_head || !self.prev_shifted;
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
        // THE RUN IS BOOKED ON WHAT WILL SOUND — after the reflection, after
        // the snap, after the lift — because that is the stride the ear
        // counts and the only one [`MELODY_RUN_MAX`] can be a guarantee about.
        self.note_run(deg - from);
        // THE LINE WRITES ITS OWN SUBJECT out of its first INTERVALS, and
        // that word is load-bearing (§3.1: "the first three intervals after
        // an Enter"). A key that is answering the subject does not also
        // rewrite it — and neither does a key that made no interval to give.
        //
        // A SESSION'S VERY FIRST KEY IS EXACTLY THAT KEY. It has no note
        // before it to be a distance FROM, so `derive` returns the degree it
        // stood on and `deg - from` is a unison the text never typed. Latched
        // as the subject's opening interval it became a repeated note at
        // EVERY answering word head, for the whole session: 24 of them on the
        // bench's 403-key prose, one per answer, all at `word_pos == 1`,
        // every one of them the engine standing still while the hand moved.
        // The subject now opens on the first real distance between two keys.
        //
        // THE SUBJECT IS A WITHIN-WORD FIGURE, and it is answered inside a
        // word, so each latched interval is held to A2's in-word bound: a
        // head's snap or a capital's lift may widen the line's first interval
        // past a sixth, and an answer replaying that mid-word would be the
        // octave-class leap A2 forbids.
        //
        // **A SUBJECT IS A FIGURE, AND A FIGURE IS NEITHER A STALL NOR A
        // RAMP** (the panel's Q1 ruling, 2026-09-09; §9). Two intervals are
        // refused the latch, and the line simply waits for the next real one
        // — the same shape as the first-key rule above, for the same reason.
        //
        // **A ZERO IS NOT AN INTERVAL.** A doubled letter's stride is zero by
        // §3.1 step 1, and latched into the subject it is replayed as a
        // UNISON at every answer for as long as the subject lives — the line
        // standing still while the hand moves, which is precisely what the
        // first-key rule was written to stop. It cost 12 of 111 keys on the
        // pathological corpus once the subject outlived its line.
        //
        // **AND THREE IDENTICAL STRIDES ARE A MACHINE, NOT A SUBJECT.**
        // [`MELODY_RUN_MAX`] says three of them is a figure and the fourth
        // inverts; a SUBJECT of three of them is a ramp that arrives already
        // holding the guard's whole budget, so an answer that follows a word
        // head which itself rose sounds the fourth — and the head's own snap
        // has only the chord tones within reach to escape onto, which is the
        // documented rare case where the snap stands. Answering more often
        // made a rare case a regular one. So the third interval is not
        // latched when it would make the subject a ramp: the subject the
        // line writes has a turn in it, always, and it is then answerable at
        // any rate without arithmetic anywhere else having to move.
        if !first && usize::from(self.motif_len) < MOTIF_LEN && self.motif_play == 0 {
            let iv = (deg - from).clamp(-WORD_LEAP_MAX_DEG, WORD_LEAP_MAX_DEG) as i8;
            let ramp = usize::from(self.motif_len) == MOTIF_LEN - 1
                && self.motif[..usize::from(self.motif_len)]
                    .iter()
                    .all(|&m| m == iv);
            if iv != 0 && !ramp {
                self.motif[usize::from(self.motif_len)] = iv;
                self.motif_len += 1;
            }
        }
        self.walk = deg as i8;
        self.prev_shifted = shifted;
        self.steps = self.steps.saturating_add(1);

        // A RE-STRIKE IS A REPEATED PITCH INSIDE A WORD, and now only that: a
        // doubled letter (§3.1 step 1's stride of zero). A session's FIRST
        // key is a step whatever degree it lands on: there is no note before
        // it for it to be a repeat of. And A WORD HEAD IS NEVER A RE-STRIKE:
        // when the chord snap lands the head on the degree the previous word
        // ended on, that is a common tone across a chord change — voice
        // leading — and §3.1 step 6 exists to make the head PROMINENT. Under
        // the ladder it sounded −4.4 dB below the interiors it is meant to
        // lead (the step-3 review counted ~22 of 99 prose heads inverted that
        // way). The repeated pitch still damps the previous voice (`repeat`,
        // below); only the touch, and with it the level and the timbre, is
        // the step's.
        let repeat = deg == from && !first;
        let touch = if repeat && !word_head {
            self.restrike = self.restrike.saturating_add(1);
            Touch::ReStrike
        } else {
            self.restrike = 0;
            Touch::Step
        };

        // THE IOI, updated AFTER `derive` has read it: the contour asks
        // whether this key was early or late against the tempo as it stood,
        // and a tempo that had already absorbed half of this very gap
        // ([`IOI_EMA_ALPHA`]) would answer a different question.
        //
        // **A REST IS NOT TEMPO INFORMATION** (the panel's Q1 ruling,
        // 2026-09-09 — §9). A `rest` gap used to be folded in like any
        // other, clamped to [`IOI_MAX_MS`], and it dragged the tempo up to
        // 600 ms; the next two or three keys then came in FASTER than that
        // stale-slow reference and [`ACCEL_SHARE`] forced every one of them
        // upward. Measured on the bench, the accel branch fired on 4.0 % of
        // keys at 4 cps, 6.7 % at 10 and 8.9 % at 20 and NEVER ONCE between
        // two steady keys: it was entirely this artefact. It is what held
        // the line at mean degree 4.1-4.7 against [`MELODY_CENTRE_DEG`] 3,
        // which then pinned word figures against the register ceiling where
        // the reflection rewrote them.
        //
        // **AND IT BROKE THE OWNER'S OWN RULING.** [`IOI_MAX_MS`] is an
        // absolute clamp, so one rest distorted the tempo by 2.4× at 4 cps
        // and by 12× at 20 cps — the same text at two speeds stopped playing
        // the same degrees (89.8 % agreement on the reels) after every
        // pause. Holding the tempo across a rest is what makes the accel /
        // decel test scale-free again, which is the only form in which
        // "same notes faster" can be true.
        //
        // A ≥ [`IOI_RESET_MS`] gap still RESTARTS the tempo rather than
        // holding it: after two seconds the hand's tempo is genuinely
        // unknown, and a stale average is worse than the default.
        self.ioi_ms = if !self.seen_key || gap >= IOI_RESET_MS {
            IOI_DEFAULT_MS
        } else if rest {
            self.ioi_ms
        } else {
            (1.0 - IOI_EMA_ALPHA) * self.ioi_ms
                + IOI_EMA_ALPHA * (gap as f32).clamp(IOI_MIN_MS, IOI_MAX_MS)
        };

        // **THE VERDICT'S HOT KEY** (sense 3): the first key after a green
        // long command is LIT whatever the chord thinks — full tine, 0 dB,
        // hero-eligible. The PITCH is untouched: the line is still the line
        // the text wrote, and only its brightness is the answer to the build.
        // Spent here, by that one key, whatever it turns out to be.
        let lit = self.lit() || self.take_verdict_lit(at);
        // The step's own level: 0 dB lit, −2 dB passing (§9.2). A re-strike
        // is THAT note again, softer — `L_n = max(0.60 · 0.85^(n−1), 0.35)`
        // re the step it repeats, so a passing note's tremolo stays a passing
        // note's tremolo and the per-second energy arc (§9.6, A14) does not
        // buy +0.1 dB on the passing notes' re-strikes.
        let step_level = if lit { 1.0 } else { PASSING_LEVEL };
        let level = if touch == Touch::Step {
            step_level
        } else {
            step_level
                * (RESTRIKE_L0 * RESTRIKE_FALL.powi(i32::from(self.restrike).saturating_sub(1)))
                    .max(RESTRIKE_FLOOR)
        };
        // A SING-ALONG NEVER MUTES THE TYPIST'S OWN LINE (§10.2, A31, and
        // R1's spirit). Under a live riff the melody is already ducked under
        // the cat by the sing duck; the one thing the riff still takes off a
        // key is a doubled letter's tremolo, which is §3.1's own spelling —
        // `sing && touch == ReStrike`. An earlier rewrite sent every
        // non-word-head key to the mallet while the riff was armed (a whole
        // bar, τ 0.40 s handback), which left roughly one pitched note per
        // word: R1 in letter and not in spirit. The machine gun A31 is named
        // for was the deleted gate's re-strike ladder firing under the riff;
        // with the gate gone a fast hand's notes are its own line, ducked,
        // and a held key is caught by `held` at any rate.
        let mallet_only = held || (sing && touch == Touch::ReStrike);
        // A CUE WITH NO KEY BEHIND IT CARRIES NO RANK, and must not clobber
        // the one the next real letter will be measured against.
        if rank != 0 {
            self.prev_rank = rank;
        }
        self.last_key_ms = at;
        self.seen_key = true;
        self.space_run = false;
        self.word_pos = self.word_pos.saturating_add(1);
        self.keys_since_enter = self.keys_since_enter.saturating_add(1);
        TypedPlan {
            touch,
            deg: i32::from(self.walk),
            lit,
            level,
            mallet_only,
            repeat,
            word_head,
            rest,
            answer_head,
        }
    }

    /// **THE DERIVATION** (§3.1): the degree this keystroke sounds, from what
    /// was typed and how it was typed. Pure in everything but its own run and
    /// motif bookkeeping — no clock, no RNG, no allocation — so the same text
    /// typed with the same rhythm is the same line, every time (A27).
    fn derive(&mut self, rank: u8, gap: u32, first: bool, word_head: bool) -> i32 {
        let from = i32::from(self.walk);

        // THE ANSWER: the line replaying its own subject. Not at the head
        // itself — the head snaps to a chord tone and IS the transposition
        // the subject is answered onto.
        if !word_head && self.motif_play > 0 {
            let iv = i32::from(self.motif[usize::from(self.motif_k)]);
            self.motif_k = self.motif_k.saturating_add(1);
            self.motif_play -= 1;
            let mut deg = reflect_deg(from + iv);
            // A reflected answer that lands where it left turns one degree
            // further, exactly as a derived stride does below.
            if iv != 0 && deg == from {
                deg = reflect_deg(from + iv + iv.signum());
            }
            // THE ANSWER IS UNDER THE SAME GUARD AS THE LINE. A subject is
            // latched from strides the guard already allowed, so it can be
            // `+1 +1 +1` — and answered after a head that itself rose, that
            // is four identical strides sounding, which is the machine
            // [`MELODY_RUN_MAX`] exists to refuse. The fourth inverts here
            // too: the subject comes back in inversion, which is what an
            // answer has been allowed to do for three hundred years.
            return self.break_run(from, deg);
        }

        // A session's very first key has nothing to be an interval FROM.
        if first {
            return from;
        }

        // *Step 1 — the text makes the interval.* Folded into −6..=6 so the
        // alphabet's distance is a musical distance and not a register jump.
        // With no pair of glyphs to measure — an echo-born cue with no rank,
        // or the first letter of a new line — the RHYTHM is what there is,
        // and the same fold reads the gap: still generative, still
        // deterministic, and a stream that misses the key seam (ssh, a
        // program echoing) still gets a moving line instead of one note.
        let (raw, repeated) = if rank != 0 && self.prev_rank != 0 {
            let d = i32::from(rank) - i32::from(self.prev_rank);
            (d, d == 0)
        } else {
            // The gap is a magnitude, never a repeat: two keys at the same
            // millisecond are the host's stamp failing, not a doubled letter.
            (gap as i32, false)
        };
        let r = fold_signed13(raw);
        let mag = stride_mag(r, repeated);
        // THE DIRECTION OF A FOLDED-TO-ZERO LEAP is the raw difference's own,
        // because the folded value has none left to give: `a` to `n` climbed
        // thirteen letters and must not be handed a signless stride, which
        // would silently become the repeat [`stride_mag`] just refused it.
        let text_sign = if r != 0 { r.signum() } else { raw.signum() };

        // *Step 2 — the hand makes the contour.* An accelerating burst
        // climbs; a hesitating hand descends; ordinary jitter inside the dead
        // band leaves the letter's own sign alone. This is the whole of "the
        // melody is generated from the typing patterns": type the same
        // sentence in a different rhythm and it is a different tune.
        let tempo = self.ioi_ms;
        let sign = if (gap as f32) < ACCEL_SHARE * tempo {
            1
        } else if (gap as f32) > DECEL_SHARE * tempo {
            -1
        } else {
            text_sign
        };
        let mut stride = mag * sign;

        // *Step 3 — gravity*, so the line has a tessitura and cannot walk to
        // a bound. See [`MELODY_CENTRE_DEG`] for why the pull is one-sided.
        //
        // GRAVITY BENDS A MOVING LINE; IT NEVER STALLS ONE AND NEVER STARTS
        // ONE. A rising second at the top of the register would come out of
        // the addition as a stride of zero — a repeated note the text never
        // asked for, and one the ear reads as a stutter rather than as a
        // pull — so it becomes a falling second instead, the move the
        // register wanted. And a DOUBLED LETTER is a stride of zero by §3.1
        // step 1, "and therefore `Touch::ReStrike`": gravity applied to it
        // would turn the text's own repeat into a step, which the step-3
        // review caught in the outer register (one of the corpus's ten
        // doubled pairs moved). The pull acts on a stride, not on a repeat.
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

        // *Step 4 — reflect, never clamp.* A clamp pins against the ceiling
        // and reads as a stuck siren; a reflection turns around and reads as
        // a phrase.
        let mut deg = reflect_deg(from + stride);
        // A REFLECTION THAT LANDS BACK ON THE NOTE IT LEFT (`from + 2` off
        // degree 7, say) is the same unearned repeat gravity could make: the
        // line turned, and a turn is not a stall. It takes one further degree
        // in the direction it turned, which is inside the register by the
        // same bound that put it outside.
        if stride != 0 && deg == from {
            deg = reflect_deg(from + stride + stride.signum());
        }

        // *Step 5 — the anti-siren guard*, on the stride that will SOUND.
        // §3.1 orders it after the reflection, and that order is the whole
        // point: a `+3` off degree 6 sounds as `+1`, and a guard that had
        // compared the `+3` against a run of `+1`s would have waved the
        // fourth `+1` through — which is exactly what the render caught.
        self.break_run(from, deg)
    }

    /// **THE FOURTH IDENTICAL STRIDE INVERTS** ([`MELODY_RUN_MAX`]), judged
    /// on the stride the ear will get: `deg − from`, after the reflection.
    /// Returns `deg` untouched unless it would complete the fourth; then the
    /// first alternative that sounds a DIFFERENT non-zero stride inside the
    /// in-word bound, tried in this order: the inversion; the inversion one
    /// degree wider (when the plain inversion reflects back onto `from`, or
    /// onto the same stride — at degree 0 a rising run cannot be inverted
    /// at all); a wider step the same way; a narrower one. Something in the
    /// list always exists: the register is nine degrees and the run stride
    /// is one number, so at most one of the four candidates can equal it.
    fn break_run(&self, from: i32, deg: i32) -> i32 {
        let sounded = deg - from;
        if sounded == 0 || self.run_len < MELODY_RUN_MAX || i32::from(self.run_stride) != sounded {
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

    /// [`MELODY_RUN_MAX`]'s bookkeeping: how long the line has been taking
    /// the same stride. A repeat (stride 0) is not a run — it is the line
    /// standing still — so it clears the count rather than extending it.
    fn note_run(&mut self, stride: i32) {
        if stride != 0 && i32::from(self.run_stride) == stride {
            self.run_len = self.run_len.saturating_add(1);
        } else {
            self.run_stride = stride.clamp(-WORD_LEAP_MAX_DEG, WORD_LEAP_MAX_DEG) as i8;
            self.run_len = u8::from(stride != 0);
        }
    }

    /// **THE AUTO-REPEAT DETECTORS** ([`AUTOREPEAT_JITTER_MS`]). Both leave
    /// the melody advancing; all either can do is take the pitch off the
    /// note.
    fn detect_autorepeat(&mut self, gap: u32, rank: u8, first: bool) -> bool {
        if first {
            self.repeat_run = 1;
            self.prev_gap = 0;
            return false;
        }
        // Machine regularity, ON ONE GLYPH — see [`AUTOREPEAT_JITTER_MS`] for
        // why the glyph is half the test.
        let regular = rank != 0
            && rank == self.prev_rank
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

    /// §10.4's `on Space`. **The playhead is untouched: a space is a rest.**
    /// Returns `true` when this space OPENS its run — the downbeat, the only
    /// one of a run of indentation that plays a bass note.
    fn on_space(&mut self, at: u32) -> bool {
        let head = !self.space_run;
        if head {
            // The undo frame is the state BEFORE the keystroke — as it is for
            // a letter — so deleting the space puts the chord back where the
            // word left it, and the retyped space advances it exactly once.
            self.push_undo();
            self.chord = (self.chord + 1) % CHORD_LOOP.len() as u8;
            self.word_pos = 0;
            self.space_run = true;
        }
        self.last_key_ms = at;
        self.seen_key = true;
        self.keys_since_enter = self.keys_since_enter.saturating_add(1);
        head
    }

    /// §10.4's `on Enter`. Ends the line home, parks the loop on
    /// [`CHORD_AFTER_ENTER`] and clears the undo stack — you cannot un-sing
    /// across a line break.
    ///
    /// **A LINE ENDS HOME; THE SUBJECT OUTLIVES IT** (§3.1, amended by the
    /// panel's Q1 ruling of 2026-09-09 — §9): the walk resolves onto the
    /// register's centre, snapped to a tone of the chord the next line will
    /// open against, and the contour bias is dropped. `prev_rank` is cleared
    /// too: the
    /// first letter of a new line has no letter before it, so it takes the
    /// rhythm's interval rather than one measured against the last line's
    /// final character.
    ///
    /// **THE MOTIF IS NO LONGER RE-LATCHED HERE, AND THAT IS THE Q1 FIX.**
    /// It used to be, on the reading that "recurrence exists inside a line
    /// and nothing repeats across a session" — but the bench's own corpus
    /// showed what that costs: 403 keys wrote NINE subjects and answered
    /// each about twice before throwing it away, so nothing recurring
    /// survived eleven seconds and no judge could hear a refrain in any
    /// roll. A subject the length of one line is not a subject. It is
    /// re-latched on the one boundary that is a real musical rest — the
    /// ≥ [`PHRASE_PAUSE_MS`] gap [`MelodyV2::on_typed`] already resolves the
    /// line on — so one subject is answered for as long as the typist keeps
    /// typing, and the hand that stops for a second writes the next one.
    ///
    /// Returns whether the full cadence is earned (§11: ≥ 4 keys since the
    /// last Enter) or whether this Return is a bare tonic dyad.
    fn on_enter(&mut self, at: u32) -> bool {
        let full = self.keys_since_enter >= ENTER_PICKUP_MIN_KEYS;
        self.chord = CHORD_AFTER_ENTER;
        self.walk = self.nearest_lit_within(MELODY_CENTRE_DEG, MELODY_CENTRE_DEG) as i8;
        self.run_stride = 0;
        self.run_len = 0;
        self.prev_rank = 0;
        self.repeat_run = 1;
        self.prev_gap = 0;
        self.word_pos = 0;
        self.restrike = 0;
        self.last_key_ms = at;
        self.last_enter_ms = at;
        self.seen_enter = true;
        self.seen_key = true;
        self.undo_len = 0;
        self.space_run = false;
        self.keys_since_enter = 0;
        full
    }

    /// Is this line feed the ECHO of the keyed Return just admitted (§10.4,
    /// [`ENTER_ECHO_SWALLOW_MS`])? The cadence has already spoken for it.
    fn jump_echoes_enter(&self, at: u32) -> bool {
        self.seen_enter && at.saturating_sub(self.last_enter_ms) < ENTER_ECHO_SWALLOW_MS
    }

    /// §10.4's `on Jump` — a PTY line feed, no key credit. The boundary is
    /// v1's: cadence the phrase if we are inside one, else step it.
    ///
    /// Returns `Some(true)` for the head of a cascade (the four-note
    /// brrrring), `Some(false)` for a top-note re-strike inside a live one,
    /// and `None` when the cascade's own 60 ms floor swallows it (D18).
    ///
    /// **A NEW CASCADE NEEDS A GAP IN THE LINE FEEDS, not a gap since the
    /// last cascade.** §10.4's pseudo-code advances `cascade_at` only on a
    /// head, which would let a 60 ms line-feed storm mint a fresh four-note
    /// figure every 180 ms — and A26 fixes the law the other way: twelve
    /// Jumps at 60 ms are "**exactly one** 4-note cascade + ≤ 11 top-note
    /// re-strikes". D18's own words are "the first Jump of a **run**", so the
    /// run is what the window measures, and the assertion is what settles the
    /// contradiction.
    fn on_jump(&mut self, at: u32) -> Option<bool> {
        // A LINE FEED IS NOT A KEYSTROKE: there is no glyph and no hand
        // behind it, so there is nothing to derive from. It RESOLVES the line
        // where it stands — the same act a rest performs — and the cascade is
        // built on the resolved note. The contour bias goes with it: whatever
        // the program printed is not a continuation of your typing.
        let here = i32::from(self.walk);
        self.walk = self.nearest_lit_within(here, here) as i8;
        self.run_stride = 0;
        self.run_len = 0;
        self.restrike = 0;
        self.last_key_ms = at;
        self.seen_key = true;
        // A LINE FEED ENDS THE WORD, as a Space or a keyed Enter does: the
        // next letter is a word head, and a pause after the line is a pause
        // between words, not inside one (`cadence_degree`).
        self.word_pos = 0;
        self.space_run = false;
        let head = !self.seen_jump || at.saturating_sub(self.cascade_at) >= CASCADE_EXCLUSIVE_MS;
        self.seen_jump = true;
        self.cascade_at = at;
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

    /// §10.4's `on Backspace`: rewind one keystroke of melody state. The sound
    /// is the shipped, unpitched poof — a deletion has no note (§19.2, ruled
    /// twice).
    fn on_backspace(&mut self, at: u32) {
        self.pop_undo();
        self.last_key_ms = at;
        self.seen_key = true;
        // A deletion ENDS a whitespace run: the space you type after
        // erasing one is a word boundary again, with its downbeat.
        self.space_run = false;
    }

    /// §10.4's `on Kill / KillWord`: the playhead is UNTOUCHED (a kill is not
    /// a rewind of the tune, it is a clause leaving), but the undo history and
    /// the word are gone with the text.
    fn on_kill(&mut self, at: u32) {
        self.undo_len = 0;
        self.word_pos = 0;
        self.last_key_ms = at;
        self.seen_key = true;
    }

    /// The next glint's degree above the verse note (§13), rotating.
    fn next_glint_deg(&mut self) -> i32 {
        let deg = i32::from(self.walk) + GLINT_DEGREES[usize::from(self.glint_k)];
        self.glint_k = (self.glint_k + 1) % GLINT_DEGREES.len() as u8;
        deg
    }

    /// THE ANSWER'S PITCH (§3.3): the nearest degree lit by the live chord
    /// strictly ABOVE `deg`. Three of five classes are lit, so one is always
    /// inside three degrees; the answer may sit above the TUNE register,
    /// because it lives in [`LANE_BLOOM`] and the register is a lane's law.
    fn answer_deg_above(&self, deg: i32) -> i32 {
        (1..=5)
            .map(|k| deg + k)
            .find(|d| self.deg_is_lit(*d))
            .unwrap_or(deg + CAPITAL_LIFT_DEG)
    }
}

// ===========================================================================
// The instrument's arithmetic — §9.1's curves, stated once
// ===========================================================================

/// FOLD a signed quantity into `−6..=6` by adding or subtracting 13 until it
/// is in range — the interval fold of §3.1 step 1, stated once and used both
/// for the alphabet's distance and for the millisecond gap that stands in for
/// it when there is no pair of glyphs to measure.
fn fold_signed13(d: i32) -> i32 {
    (d + 6).rem_euclid(13) - 6
}

/// REFLECT a degree back inside the TUNE register (§3.1 step 4) — *never*
/// clamp it. A clamp pins a climbing line against the ceiling and holds it
/// there, which is what a siren is; a reflection turns the line around, which
/// is what a phrase does.
///
/// The loop is the totality guard: a stride is bounded by
/// [`WORD_LEAP_MAX_DEG`] and the register is eight degrees wide, so one
/// reflection always suffices and the final clamp is unreachable.
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
/// [`ROOF_HUE_ADD_HZ`] from the ribbon's hue (§3.3 item 3) — and never, at
/// any point, a decibel. `hue` is the arc position already read through
/// [`hue_arc`] (0 at the red end, 1 at the cyan end), so a caller with the
/// hue stop out passes 0.0 and gets the pre-§3.3 roof exactly.
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

/// THE HUE'S POSITION ON THE ARC, 0 at the red end and 1 at the cyan end,
/// through [`tri`] so the wrap at hue 1 → 0 REFLECTS instead of jumping:
/// `tri` has period two, so the hue is doubled and the arc is a mirror about
/// its middle — the same shape [`hue_air`]'s cosine has, with no seam. One
/// number drives the note's roof and the bloom's pan.
fn hue_arc(hue: f32) -> f32 {
    tri(2.0 * hue.clamp(0.0, 1.0))
}

/// THE FLOOR UNDER THE ARC — how much of the bloom survives at the RED end.
///
/// **0.55 → 0.80 ON THE PANEL'S Q2 RULING (2026-09-09; §9).** The owner asked
/// for bright, cute and sparkly, and the bench probe found the wire pulling
/// the other way: at the red end the typed key measured a 1034 Hz centroid
/// with 0.066 of its energy over 2 kHz, DIMMER than the flat bloom rung's
/// own 1148 / 0.117, against 1282 / 0.177 at the cyan end. So for something
/// like half of every 5.56 s arc the sparkle was being switched off, and the
/// ladder render shows exactly that — the 2-7 kHz partial lines fading in
/// and out on the cycle where the plain and bloom panels hold them steady.
/// At 0.80 the arc still moves (it is the colour you can hear, and that is
/// item 2's whole point) but it never goes dull: the red end lands near
/// 1200 Hz. The arc buys brightness and still never buys a decibel — the
/// bloom's LEVEL is untouched, and the Q6 bed trim more than pays the
/// ≈ +0.4 dB this adds at the red end (§21.4).
const HUE_AIR_FLOOR: f32 = 0.80;

/// THE HUE'S OWN AIR (§3.3 item 2). `ev.hue` reaches the synth on every event
/// and was read nowhere in this module; this is that wire, connected: the
/// bloom is dimmest at the red end of the arc and brightest at the cyan end,
/// so the light you can see is the light you can hear. Smooth and periodic,
/// so the wrap has no seam.
fn hue_air(hue: f32) -> f32 {
    HUE_AIR_FLOOR + (1.0 - HUE_AIR_FLOOR) * (0.5 - 0.5 * (core::f32::consts::TAU * hue).cos())
}

/// OCTAVE-FOLD `f` into `[lo, hi)`. Halving and doubling are exact in binary
/// floating point and preserve pitch class exactly, so the folded pitch is
/// still a lattice degree — which is the whole reason every glint can be
/// promised "a lattice pitch class, never a beat" (§13).
///
/// Bounded: `lo` is guarded positive by its callers and the loops step by
/// octaves, so at most a couple of dozen iterations are possible for any
/// finite input.
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

/// THE TINE, as a prototype voice (§9.1, §9.2). `f` is the fundamental, `tau`
/// the already-scaled voice decay, `roof` the already-resolved lowpass.
///
/// `mallet_only` is §10.2's sing-along rule: under a live riff a re-strike
/// drops to the felt alone — you still feel the key, the cat still owns the
/// tune.
///
/// `flow` is §22's flow heat, and it reaches exactly one place here: the
/// STEP's fundamental-to-octave balance ([`flow_partials`]). At `0.0` this
/// function is its own pre-flow self, expression for expression.
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

/// THE BLOOM, as a prototype voice (§3.3 item 1): three partials at
/// [`BLOOM_DEGREES`] above the note's lattice degree `deg`, each with its
/// own decay, fading in over [`BLOOM_ATTACK_S`] from [`BLOOM_DELAY_S`] behind
/// the strike, twinkling slowly, under a roof [`BLOOM_ROOF_ADD_HZ`] above the
/// note's own. No mallet: the strike already happened.
///
/// **IT HANGS FOR AS LONG AS ITS OWN NOTE EARNS** (the panel's Q2/Q5 ruling,
/// 2026-09-09): [`BLOOM_DECAY_TAU_MUL`] × `tau_v` under [`BLOOM_DECAY_S`],
/// with the tail law's `3τ + 20 ms` computed from that, and the head scaled
/// the same way under [`BLOOM_HEAD_MIN_MUL`]. `tau_v` is the note's τ_v —
/// [`tau_v_s`] of the live IOI, WITHOUT the re-strike multiplier, because a
/// bloom only ever rides a [`Touch::Step`]. At the 4 cps reference every one
/// of these terms is its own constant to the bit; they bind as you speed up,
/// which is where the wash was.
fn bloom(deg: i32, roof: f32, tau_v: f32) -> Voice {
    let decay = (BLOOM_DECAY_TAU_MUL * tau_v).min(BLOOM_DECAY_S);
    let head = (tau_v / TAU_V_MAX_S).clamp(BLOOM_HEAD_MIN_MUL, 1.0);
    Voice {
        delay: BLOOM_DELAY_S * head,
        // …under the same ceiling as the decay it is built from. The
        // `min` is arithmetically redundant (`decay <= BLOOM_DECAY_S` by the
        // line above) and it is kept so the tail law's ceiling is one
        // constant a reader can find rather than an inference.
        dur: (3.0 * decay + TINE_DUR_TAIL_S).min(BLOOM_DUR_S),
        attack: BLOOM_ATTACK_S * head,
        decay,
        p: [
            Partial {
                lvl: BLOOM_LVL[0],
                f0: penta(TINE_BASE_HZ, deg + BLOOM_DEGREES[0]),
                decay: BLOOM_TAU[0],
                ..Partial::default()
            },
            Partial {
                lvl: BLOOM_LVL[1],
                f0: penta(TINE_BASE_HZ, deg + BLOOM_DEGREES[1]),
                decay: BLOOM_TAU[1],
                ..Partial::default()
            },
            Partial {
                lvl: BLOOM_LVL[2],
                f0: penta(TINE_BASE_HZ, deg + BLOOM_DEGREES[2]),
                decay: BLOOM_TAU[2],
                ..Partial::default()
            },
        ],
        tw_rate: BLOOM_TW_RATE,
        tw_depth: BLOOM_TW_DEPTH,
        lp_cut: roof + BLOOM_ROOF_ADD_HZ,
        lane: LANE_BLOOM,
        ..Voice::default()
    }
}

// ===========================================================================
// §16 row 9 — the v2 admission path on `TrailSynth`
// ===========================================================================

impl TrailSynth {
    /// THE TIMBRE LADDER'S SEAM (§7 step 3; bench and test hook). Pull one
    /// of §3.3's stops and the synth renders that rung — `plain` is the
    /// derived line through the shipped tine, byte for byte. Production
    /// never calls this: a fresh synth is [`TimbreStops::ALL`].
    pub fn set_v2_timbre_stops(&mut self, stops: TimbreStops) {
        self.v2.stops = stops;
    }

    /// The stops in force (test / introspection hook).
    #[must_use]
    pub fn v2_timbre_stops(&self) -> TimbreStops {
        self.v2.stops
    }

    /// THE MELODY'S CLOCK. The host's stamp where there is one; the synth's
    /// own block clock where there is not (§16 row 7's identity default).
    ///
    /// The fallback WRAPS like a host stamp; it never saturates. `f64 → u64`
    /// (that cast's saturation is 584 million years out) then `u64 → u32`,
    /// which truncates — reduces modulo 2³² — exactly as the host's own u32
    /// millisecond counter does. A saturating `f64 → u32` cast would pin
    /// every event after 49.7 days at `u32::MAX`: every gap zero, forever —
    /// no rests, no IOI reset — with nothing to heal it. A wrap costs what a
    /// wrapped host stamp costs and heals the same way: the wrapping key
    /// reads a zero gap, so it derives its interval from a nonsense
    /// millisecond count and the auto-repeat floor takes it to the mallet —
    /// **and it still steps and still speaks**, because nothing on this path
    /// may cost a key its note. The next gap is a real one and the line
    /// carries on from wherever the wrapping key left it.
    fn v2_at_ms(&self, meta: EventMeta) -> u32 {
        if meta.at_ms != 0 {
            meta.at_ms
        } else {
            (self.clock_s * 1000.0) as u64 as u32
        }
    }

    /// A seeded velocity multiplier, `10^(u·k/20)` with `u ∈ [−1, 1]` (§9.6).
    /// Per-SESSION deterministic, never per-frame: the draw comes from the
    /// synth's own seeded stream, so one script replays bit-exactly (A27).
    fn v2_velocity(&mut self, db: f32) -> f32 {
        let u = self.rnd_in(-1.0, 1.0);
        (10.0f32).powf(u * db / 20.0)
    }

    /// The seeded ±0.03 pan jitter (§9.1) — enough to un-stack two voices on
    /// one column, far too little to move a sound off its glyph. §9.1 states
    /// it in the stereo law's OUTPUT units (after `× 0.35`), and this is the
    /// law's input, so the draw is divided back out: the pan the ear gets
    /// moves by ±0.03, not ±0.0105.
    fn v2_pan(&mut self, pan: f32) -> f32 {
        pan + self.rnd_in(-PAN_JITTER, PAN_JITTER) / PAN_LAW_SCALE
    }

    /// §10.2's KEY HANDBACK: when the sing-along has ended and the line has
    /// reached a boundary, the borrowed `song_key` goes back to the neutral
    /// lattice — here, BEFORE the next word's first note is voiced, and never
    /// mid-word (A31). Called at the top of every event that can open a word.
    ///
    /// **The boundary is now the WORD or the REST, not the phrase.** With
    /// the authored form deleted there is no phrase index to wait for; a word
    /// head and a think-pause are the structure the line still has, and both
    /// are finer than a phrase, so a held key is handed back sooner rather
    /// than later. The law it serves — "never transpose an utterance
    /// mid-way" — is unchanged. See [`MelodyV2::at_boundary`].
    fn v2_hand_back_key(&mut self, at: u32) {
        if self.v2_key_pending && self.v2.at_boundary(at) {
            self.song_key = 0;
            self.v2_key_pending = false;
        }
    }

    /// §9.5 law 5 for the lanes whose newcomer may land on a live voice's
    /// exact pitch (echo, glint): damp the old voice first, over the 12 ms
    /// ramp, so two independently phased sines at one frequency never comb.
    fn v2_damp_same_pitch(&mut self, lane: u8, f: f32) {
        for v in &mut self.voices {
            if v.on && v.lane == lane && v.damp <= 0.0 && (v.p[0].f0 - f).abs() < SAME_PITCH_HZ {
                v.damp = LANE_FADE_STEAL_S;
                v.damp0 = LANE_FADE_STEAL_S;
            }
        }
    }

    /// Spawn and return the voice's ADDRESS — `(slot, born)` — so a later damp
    /// or cancel can prove it is still talking to the same voice.
    fn v2_spawn(&mut self, proto: Voice, gain: f32, pan: f32) -> Option<(u8, u32)> {
        let idx = self.spawn(proto, gain, pan)?;
        Some((idx as u8, self.voices[idx].born))
    }

    /// Ramp a specific voice down. Silently does nothing when the address is
    /// stale (the slot was recycled) or the voice is already damping — §9.5
    /// law 2's ramp is never restarted, only armed once.
    fn v2_damp(&mut self, who: Option<(u8, u32)>, ramp: f32) {
        let Some((slot, born)) = who else { return };
        let v = &mut self.voices[usize::from(slot)];
        if v.on && v.born == born && v.damp <= 0.0 {
            v.damp = ramp;
            v.damp0 = ramp;
        }
    }

    /// CANCEL A PRE-DELAYED VOICE THAT HAS NOT SPOKEN YET (§12.3). A voice
    /// still at `t < 0` has produced no sample, so retiring it is silent —
    /// "a pre-delayed voice that never started expires unheard". A voice that
    /// HAS started is left strictly alone: **a bell that has already sounded
    /// is never damped.**
    fn v2_cancel_unsounded(&mut self, who: Option<(u8, u32)>) {
        let Some((slot, born)) = who else { return };
        let v = &mut self.voices[usize::from(slot)];
        if v.on && v.born == born && v.t < 0.0 {
            v.on = false;
        }
    }

    /// Ramp every live voice of one lane down.
    fn v2_damp_lane(&mut self, lane: u8, ramp: f32) {
        for v in &mut self.voices {
            if v.on && v.lane == lane && v.damp <= 0.0 {
                v.damp = ramp;
                v.damp0 = ramp;
            }
        }
    }

    /// Ramp every live TUNE voice down over `ramp` — the Kill family's
    /// "mute all tune voices" (§10.4).
    fn v2_mute_tune(&mut self, ramp: f32) {
        self.v2_damp_lane(LANE_TUNE, ramp);
        self.v2.lead = None;
    }

    /// THE v2 ADMISSION PATH (§16 row 9) — reached only from
    /// [`TrailSynth::push_meta`]'s single top-of-function branch, and never
    /// entered by a v1 event.
    ///
    /// It deliberately does NOT run: the flood governor (§16 row 11 sets that
    /// duck to exactly 1.0 — the IOI arc of §9.6 replaces it), `MIN_GAP`
    /// thinning (the per-lane caps and the IOI-shortened note are v2's rate
    /// law — one key, one step, at every speed),
    /// `advance_song` (v2 has no bar of ghosts) or `design_trail`'s palette
    /// dispatch (v2 designs its own voices). What it DOES keep is every piece
    /// of v1 machinery v2 explicitly reuses byte-unchanged: the bed's style /
    /// tone latch, the rate estimate other sources read, the erase gate, and
    /// the four terminal style-agnostic designers behind
    /// [`TrailSynth::design_trail`] (poof, word poof, swoosh, cloud) — all of
    /// which return BEFORE palette dispatch, which is exactly why they can be
    /// reused rather than copied.
    pub(super) fn push_v2(&mut self, ev: SoundEvent, meta: EventMeta) {
        // **THE VERDICT FORKS FIRST**, above every line of the trail
        // preamble, because none of that preamble is true of it: it does not
        // latch the bus (a verdict always follows a keyed Enter, which
        // latched it), it does not drift the sky's hue (the machine has no
        // colour), it does not kick the bed (weather is authorship) and it
        // does not pay into the rate estimate (it is not a keystroke — that
        // is the whole point of the arm-and-spend gate that let it in).
        if let SoundGesture::Output(OutputGesture::Verdict { failed, ice }) = ev.kind {
            let at = self.v2_at_ms(meta);
            return self.v2_verdict(&ev, at, failed, ice);
        }
        let SoundGesture::Trail(kind) = ev.kind else {
            return;
        };
        // THE SKY'S COLOUR (THE PRISM §3.2): the hue's arc position through
        // a one-pole with τ = `BED_HUE_TAU_S`, stepped by the real time since
        // the last event (read BEFORE the rate bookkeeping resets it). Seeded
        // on the first v2 event so the pad enters in the ribbon's colour
        // rather than gliding up from red. Unconditional — it is bookkeeping,
        // not sound: with the bed knob off nothing reads it.
        {
            let arc = hue_arc(ev.hue);
            if self.v2_latched {
                let k = 1.0 - (-self.since_event / BED_HUE_TAU_S).exp();
                self.bed.hue_s += (arc - self.bed.hue_s) * k;
            } else {
                self.bed.hue_s = arc;
            }
        }
        // THE FIRST v2 TRAIL EVENT LATCHES THE BUS (§9.7, §16 row 10): the
        // limiter is armed from here on, and the sing-along's key is handed
        // back at phrase boundaries rather than snapped (§10.2).
        self.v2_latched = true;
        // Style / tone follow the trail stream unconditionally, as in v1: a
        // bed re-enabled mid-stream must wake in the current constitution.
        self.bed_style = ev.style;
        self.bed_voice = ev.voice;
        self.tone = ev.tone;
        // FLOW HEAT FOR THIS CUE (§22). Latched before any gesture is voiced
        // and clamped by `push_meta`'s own boundary filter, so every voice
        // this push mints is priced on one number — and on `0.0`, which is
        // every unstamped host and every archived render, on the shipped
        // constants themselves.
        self.v2.flow = meta.flow;
        // THE BED'S KICK (§3.2 "How it starts") — v1's own feed, the one
        // table both engines call (`bed_kick`), behind the same `ev.bed`
        // gate: with the `trail_sound_bed` setting off (the default) the
        // bed's energy never leaves its exact-zero floor and the pad is
        // silent by construction; on, it fades in behind the first two or
        // three keys through `tick_bed`'s own 250 ms swell and exhales to
        // exact zero after the last, so idle still parks the device.
        if ev.bed {
            self.kick_bed(kind, ev.gain);
        }
        // The rate estimate is bookkeeping other sources (the bonk, the riff)
        // still read, so v2 pays into it even though its own loudness law is
        // the IOI arc.
        self.rate = self.rate * (-self.since_event / 0.6).exp() + 1.0;
        self.since_event = 0.0;
        let at = self.v2_at_ms(meta);

        match kind {
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
                // THE NOTE IS TAKEN AWAY on EVERY deletion (§10.4): a 40 ms
                // mute of the newest tune voice, then the melody rewinds one
                // keystroke. Not behind the erase gate — a held Backspace
                // deletes a character per repeat and must un-sing one per
                // repeat, or "type five, delete five, retype" resumes from
                // the wrong playhead. Backspace is UNPITCHED — the sound is
                // the shipped poof and nothing else.
                self.v2_damp(self.v2.lead, ERASE_MUTE_S);
                self.v2.lead = None;
                self.v2.on_backspace(at);
                // THE ERASE GATE, byte-unchanged, on the POOF alone: a
                // deletion's sound is thinned against OTHER DELETIONS and
                // against nothing else, and the held-run bookkeeping is
                // v1's, so the release shimmer still knows an auto-repeat
                // from a correction.
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
                self.v2_mute_tune(ERASE_MUTE_S);
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
            // **`Land` IS DEAD UNDER v2** (§12.3, A20): the meteor's bell IS
            // the landing, and a second landing voice on a second clock is
            // precisely §9.0's fifth cause.
            SoundKind::Land => {}
            SoundKind::Jump => {
                // A KEYED RETURN'S OWN LINE-FEED ECHO IS NOT A CASCADE (A26):
                // the cadence already spoke for it, and a Jump inside
                // `ENTER_ECHO_SWALLOW_MS` of that Enter is swallowed whole —
                // no brrrring, no melody step, no beat claimed.
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
        }
    }
}

/// The Backspace / Kill mute (§10.4, §11). 40 ms rather than the 12 ms damp
/// everything else uses: an erase is a slower, softer gesture than a
/// retrigger, and a note yanked in 12 ms under a puff of air reads as a
/// glitch in the puff.
const ERASE_MUTE_S: f32 = 0.040;

// ===========================================================================
// The gesture designers (§11's table, row by row)
// ===========================================================================

impl TrailSynth {
    /// **TYPED** — the step or the re-strike, the bloom behind it, and on
    /// the keys that earn them the answering voice and the air cloud (§9.2,
    /// §10.2, §11's first rows, §3.3).
    fn v2_typed(&mut self, ev: &SoundEvent, meta: EventMeta, at: u32) {
        self.v2_hand_back_key(at);
        let sing = self.sing > 0.0;
        // THE KEY IS THE NOTE. There is no early return on this path and
        // there must never be one again: every keystroke that reaches here
        // derives a degree and spawns a voice for it.
        let plan = self.v2.on_typed(at, meta.rank, sing, ev.shifted);
        let stops = self.v2.stops;
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
        // A WORD HEAD STILL GETS THE LIT ROOF (§10.2). The word boundary is
        // heard in the harmony, not forced into the playhead — v1's word-head
        // re-bar is not carried — but the first letter of a word may open.
        // A CAPITAL OPENS THE ROOF too (§10.4): the one other place spelling
        // touches the sound, beside the lift `on_typed` gave the degree.
        let lit_roof = ev.shifted || (plan.lit && (plan.touch == Touch::Step || plan.word_head));
        // THE ARC (§3.3 item 3): the hue opens the roof, or does nothing at
        // all with the stop out — `hue_arc(0) == 0`, so `plain` is exact.
        let arc = if stops.hue { hue_arc(ev.hue) } else { 0.0 };
        let roof = roof_hz(cps, lit_roof, ev.heat, arc, plan.touch);
        let deg = plan.deg + i32::from(self.song_key);
        let f = penta(TINE_BASE_HZ, deg);
        if plan.repeat {
            // §9.5 law 5: a same-pitch note damps the old voice first. Two
            // independently phased voices at ONE frequency are a comb
            // filter, which is the artefact the space damp exists to
            // prevent — and a word head's common tone is as much the same
            // pitch as a doubled letter's tremolo.
            self.v2_damp(self.v2.lead, LANE_FADE_STEAL_S);
        }
        let vel_db = if plan.touch == Touch::Step {
            VEL_DB_STEP
        } else {
            VEL_DB_RESTRIKE
        };
        let vel = self.v2_velocity(vel_db);
        let pan = self.v2_pan(ev.pan);
        let g = g_ioi(ioi_s);
        let gain = ev.gain * KEY_TINE_TRIM * plan.level * g * vel;
        let voice = tine(f, plan.touch, tau, roof, plan.mallet_only, flow);
        self.v2.lead = self.v2_spawn(voice, gain, pan);
        // THE ONSET CLOCK records what the MIXER did, not what the melody
        // intended: a key that leaves this where it was is a key the TUNE
        // lane refused, and the census's `silent` column is that test.
        if self.v2.lead.is_some() {
            self.v2.last_onset_ms = at;
        }

        // THE BLOOM (§3.3 item 1) — behind every LIT step that has a pitch
        // to bloom from. A passing note stays a plain strike, a doubled
        // letter's tremolo stays a tremolo, and a felt key has nothing above
        // it to bloom. Its level rides the hue's own air (item 2) and its pan
        // drifts with the colour while the strike stays on the caret's
        // column, so the note OPENS in the field. Both are two multiplies,
        // and both are exactly the bloom rung's constants with the hue stop
        // out.
        let blooms = stops.bloom && plan.lit && plan.touch == Touch::Step && !plan.mallet_only;
        // The bloom's own gain, kept for the air cloud, which is two taps of
        // it.
        let (air, spread) = if stops.hue {
            (hue_air(ev.hue), BLOOM_SPREAD * (hue_arc(ev.hue) - 0.5))
        } else {
            (hue_air(0.25), 0.0)
        };
        let bloom_gain = ev.gain * KEY_TINE_TRIM * plan.level * g * vel * BLOOM_LEVEL * air;
        if blooms {
            self.v2_spawn(bloom(deg, roof, tau), bloom_gain, pan + spread);
        }

        // **THE SPARKLE** ([`KEY_GLINT_LEVEL_MUL`]) — one glint on every
        // struck key, twice as bright on a capital. It rides the same rung
        // as the bloom, so `plain` is still exactly a tine; it fires on
        // PASSING notes as well as lit ones, because the owner asked for
        // every key to sparkle and a passing note is a note; and it does NOT
        // fire on a re-strike or a felt key — a doubled letter is "the same
        // note further away" by §9.2 and a new glint on top of it would be a
        // brighter second strike, which is the one thing that ladder exists
        // to refuse.
        //
        // It arrives at [`KEY_GLINT_DELAY_S`] — with the bloom, off the
        // strike's own crest. The light comes off the bar after the bar is
        // struck, and a highlight that landed inside the strike's attack was
        // buying decibels rather than brightness (§9.6).
        if stops.bloom && plan.touch == Touch::Step && !plan.mallet_only {
            let mul = if ev.shifted {
                KEY_GLINT_SHIFTED_MUL
            } else {
                1.0
            };
            self.v2_glint_at(
                ev,
                0,
                GLINT_LEVEL * KEY_GLINT_LEVEL_MUL * mul,
                vel,
                KEY_GLINT_DELAY_S,
            );
        }

        // **FLOW'S ECHO** (§22 lever 1) — the capital's octave echo, given
        // back to every LIT STEP once the hand is in flow, at
        // [`FLOW_ECHO_DELAY_S`] behind its own strike and
        // [`FLOW_ECHO_LEVEL`] under it.
        //
        // It rides THIS key: same pan, same seeded velocity, same loudness
        // arc, same lattice degree one octave up — one keystroke, one light.
        // No mallet (`n_lvl = 0`): the strike already happened, and a second
        // felt hit 25 ms later would be the second onset a capital was
        // stripped of.
        //
        // NOT under a timbre stop, and that is deliberate: flow heat IS its
        // stop. At `flow == 0.0` the branch is not taken, no slot is claimed
        // and no draw is made, so the cold box is byte-identical rather than
        // being a gain-0 render of a wider one — which is the same argument
        // §9.7 makes for the bed's exact-zero floor.
        //
        // It goes in [`LANE_BLOOM`], whose cap of 2 is literally the two
        // slots this echo vacated when §3.1 deleted it. A full bloom lane
        // drops the newcomer rather than stealing (§14's hierarchy), so at a
        // flowing 12 cps the lane thins the DECORATION and never the tune.
        if flow > 0.0 && plan.lit && plan.touch == Touch::Step && !plan.mallet_only {
            let mut echo = tine(
                penta(TINE_BASE_HZ, deg + FLOW_ECHO_OCTAVE_DEG),
                Touch::Step,
                tau,
                roof,
                false,
                flow,
            );
            echo.n_lvl = 0.0;
            // IT SWELLS, IT DOES NOT STRIKE. Dropping the mallet took the
            // FELT off the echo but left it the tine's own 4 ms
            // [`TINE_ATTACK_S`], which is still an onset — and at
            // [`FLOW_ECHO_DELAY_S`] behind the key that onset peaked at
            // 29 ms, which is the millisecond the bloom peaks on at every
            // rate slow enough to keep the full head (`head == 1.0` at and
            // under 4 cps). Two decorations of one key, in one lane, cresting
            // together: measured +0.51 dB on the 4 cps prose peak in flow,
            // and it is the whole of what was left of §9.6's law once the
            // sparkle and the interval were fixed. On the bloom's own attack
            // the echo opens into the note's decay instead of punching a
            // second time into it, which is what "one keystroke, one light"
            // said in the first place — the flowing 4 cps peak is −0.29 dB,
            // and the ring-out the echo exists for is +3.85 dB.
            //
            // ORDER MATTERS, and this is the one place it is written down.
            // m15 measured this same attack on 2026-09-09 and reported that
            // it "moved the peak not at all" — reproduced here exactly,
            // +0.23 dB before and +0.23 dB after. It is true, and it is true
            // because with the sparkle still on the strike's crest the
            // word's maximum belonged to the GLINT: softening the onset of a
            // voice that does not own the peak cannot move the peak. Delay
            // the sparkle first and the same attack is worth the law.
            echo.attack = BLOOM_ATTACK_S;
            echo.delay = FLOW_ECHO_DELAY_S;
            echo.lane = LANE_BLOOM;
            self.v2_spawn(echo, gain * FLOW_ECHO_LEVEL * flow, pan);
        }

        if stops.room {
            // THE ANSWERING VOICE (§3.3): the head that opens the line's
            // reply to its own subject is answered from above, a beat later,
            // on the nearest lit tone — one key in eighteen or so, in the
            // droppable lane.
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
                    ev.gain * KEY_TINE_TRIM * ANSWER_LEVEL * g * vel,
                    pan,
                );
            }
            // THE AIR CLOUD (§3.3 item 4): the key that ends a phrase rest
            // resolves the line and then sings, and the room answers THAT
            // note — never a key inside the phrase.
            if plan.rest && !plan.mallet_only {
                self.v2_air_cloud(deg, roof, bloom_gain, pan, 0.0);
            }
        }
    }

    /// THE TWO-TAP AIR CLOUD (§3.3 item 4): two bloom taps on degree `deg`,
    /// `after` seconds past the key, at [`AIR_TAP_DELAY_S`] and
    /// [`AIR_TAP_LEVEL`] re the bloom, panned to opposite sides of `pan`. Two
    /// taps at unequal spacings read as a room; that is the whole of the
    /// reverb this theme has, and it is spent only where a line ends.
    ///
    /// **THE ROOM KEEPS THE FULL HANG.** Its taps are built at
    /// [`TAU_V_MAX_S`] rather than at the live note's τ_v, so the Q2 hang
    /// scaling does not reach them: that scaling exists because a PER-KEY
    /// hang stacks three and four deep at speed, and a room is spent twice
    /// at a line's end and nowhere else. A room shortened to 87 ms is a
    /// click, not air.
    fn v2_air_cloud(&mut self, deg: i32, roof: f32, bloom_gain: f32, pan: f32, after: f32) {
        for k in 0..AIR_TAP_DELAY_S.len() {
            let mut tap = bloom(deg, roof, TAU_V_MAX_S);
            tap.delay = after + AIR_TAP_DELAY_S[k];
            let side = if k % 2 == 0 {
                -AIR_TAP_PAN
            } else {
                AIR_TAP_PAN
            };
            self.v2_spawn(tap, bloom_gain * AIR_TAP_LEVEL[k], pan + side);
        }
    }

    /// **SPACE** — the word boundary (§9.3, §10.3, §10.4).
    ///
    /// The run's HEAD advances the chord loop and plays the bass dyad; its
    /// TAIL answers with air alone, because indentation is one gesture and not
    /// four bass notes. Either way the PLAYHEAD IS UNTOUCHED: a space is a
    /// rest, and the tune resumes where it stopped.
    fn v2_space(&mut self, ev: &SoundEvent, at: u32) {
        let head = self.v2.on_space(at);
        let g = g_ioi(self.v2.ioi_ms * 0.001);
        if head {
            // Monophonic (§9.3): a new downbeat damps the old one over the
            // shipped 12 ms ramp rather than stacking on it.
            self.v2_damp(self.v2.bass, LANE_FADE_STEAL_S);
            let chord = CHORD_LOOP[usize::from(self.v2.chord)];
            let root = penta(
                BASS_BASE_HZ * CHORD_ROOT_RATIO[chord.root],
                self.song_key.into(),
            );
            let partner = root * chord.partner;
            // **THE DOWNBEAT RINGS LONGER IN FLOW** (§22 lever 2): the
            // dyad's decay lerps to [`FLOW_BASS_DECAY_S`] and its `dur`
            // follows through the tail law, so the floor of the mix gains
            // body without gaining an onset. `lerp(a, b, 0.0)` is `a`, and
            // `TAIL_DUR_PER_TAU * BASS_DECAY_S` IS [`BASS_DUR_S`], so the
            // cold downbeat is the shipped constant twice over.
            let decay = lerp(BASS_DECAY_S, FLOW_BASS_DECAY_S, self.v2.flow);
            let voice = Voice {
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
            };
            let vel = self.v2_velocity(VEL_DB_MOTION);
            let gain = ev.gain * KEY_TINE_TRIM * BASS_LEVEL * g * vel;
            // The bass is CENTRED: §11's stereo rule keeps everything under
            // 436 Hz inside ±0.15, and the downbeat is the floor of the mix.
            self.v2.bass = self.v2_spawn(voice, gain, 0.0);
        }
        let voice = Voice {
            dur: BREATH_DUR_S,
            attack: BREATH_ATTACK_S,
            decay: BREATH_DECAY_S,
            n_lvl: 1.0,
            n_f0: BREATH_HZ0,
            n_f1: BREATH_HZ1,
            n_glide: BREATH_GLIDE_S,
            n_q: BREATH_Q,
            lane: LANE_BREATH,
            ..Voice::default()
        };
        let pan = self.v2_pan(ev.pan);
        self.v2_spawn(voice, ev.gain * KEY_TINE_TRIM * BREATH_LEVEL * g, pan);
    }

    /// **SHIFT** — the bare modifier's lift (§10.4, §11, §3.1): the felt
    /// mallet alone, no pitch, no beat claimed, no playhead moved. A modifier
    /// is intent, not authorship, and since 2026-09-08 it is not a note
    /// either — it cannot be mistaken for a step, and one capital is no
    /// longer three onsets.
    fn v2_shift(&mut self, ev: &SoundEvent) {
        let voice = Voice {
            dur: LIFT_DUR_S,
            attack: LIFT_ATTACK_S,
            decay: LIFT_DECAY_S,
            p: [Partial::default(), Partial::default(), Partial::default()],
            n_lvl: MALLET_LVL,
            n_f0: MALLET_HZ0,
            n_f1: MALLET_HZ1,
            n_glide: MALLET_GLIDE_S,
            n_q: MALLET_Q,
            n_decay: MALLET_TAU_S,
            lp_cut: ROOF_PLAIN_LO_HZ,
            lane: LANE_SHIFT,
            ..Voice::default()
        };
        let pan = self.v2_pan(ev.pan);
        self.v2_spawn(
            voice,
            ev.gain * KEY_TINE_TRIM * LIFT_LEVEL * LIFT_FELT_MUL,
            pan,
        );
    }

    /// **NAV TICK** — the mini-fan's voice (D17, §12.3's floor): the verse
    /// note, P1 only, no mallet, −24 dB. The sound of a hop too small to be a
    /// meteor, and the sound a fizzled arm resolves into.
    fn v2_nav_tick(&mut self, ev: &SoundEvent) {
        let deg = i32::from(self.v2.walk) + i32::from(self.song_key);
        let voice = Voice {
            dur: NAV_DUR_S,
            attack: NAV_ATTACK_S,
            decay: NAV_DECAY_S,
            p: [
                Partial {
                    lvl: P1_LVL,
                    f0: penta(TINE_BASE_HZ, deg),
                    ..Partial::default()
                },
                Partial::default(),
                Partial::default(),
            ],
            lp_cut: ROOF_PLAIN_LO_HZ,
            lane: LANE_TUNE,
            ..Voice::default()
        };
        let pan = self.v2_pan(ev.pan);
        self.v2_spawn(voice, ev.gain * KEY_TINE_TRIM * NAV_LEVEL, pan);
    }

    /// **PTY CASCADE** — the owner's beloved brrrring, capped (D18).
    ///
    /// The FIRST line feed of a run plays the four-note figure; a line feed
    /// arriving inside the live cascade re-strikes the top note alone at
    /// −12 dB, and one arriving inside 60 ms of that is silent. Twelve jumps
    /// at 60 ms are therefore one cascade and at most eleven quiet
    /// re-strikes, instead of v1's ~60 pitched onsets a second.
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
        let base = i32::from(self.v2.walk) + i32::from(self.song_key);
        if head {
            for k in 0..CASCADE_DEGREES.len() {
                let f = penta(TINE_BASE_HZ, base + CASCADE_DEGREES[k]);
                let mut voice = tine(
                    f,
                    Touch::Step,
                    tau,
                    roof_hz(cps, true, ev.heat, arc, Touch::Step),
                    false,
                    self.v2.flow,
                );
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

    /// **ENTER** — the cadence (§11, D9).
    ///
    /// D9 is the whole of this function's shape: the resolution C, the tonic
    /// dyad and the faraway bell all fire at `flight_ms(cells)` — the SAME
    /// `Instant` the pin and the landing squash read — and only the pickup is
    /// at `t = 0`. A cadence on a constant while the pixels flew a
    /// distance-dependent arc is v1's coupling defect at Enter scale.
    fn v2_enter(&mut self, ev: &SoundEvent, at: u32, cells: u16) {
        self.v2_hand_back_key(at);
        let full = self.v2.on_enter(at);
        // §10.4 reads `walk` AFTER the phrase has been cadenced, so the
        // resolution answers the note the theme was heading for and not the
        // note the last keystroke happened to leave behind.
        let walk = self.v2.walk;
        let t = timing::flight_ms(f32::from(cells)) * 0.001;
        let ioi_s = self.v2.ioi_ms * 0.001;
        let tau = tau_v_s(ioi_s);
        let stops = self.v2.stops;
        let arc = if stops.hue { hue_arc(ev.hue) } else { 0.0 };
        let roof = roof_hz(1.0 / ioi_s, true, ev.heat, arc, Touch::Step);
        let key = i32::from(self.song_key);
        // THE ROOM ANSWERS THE LINE'S END (§3.3 item 4): two bloom taps
        // behind the note the cadence resolves onto — the resolution at
        // `t` when the cadence is earned, home where it is a bare dyad —
        // at the bloom's own level for this hue. A line ends, and the air
        // it ends in is heard once.
        let home = if full {
            if walk <= CAD_RESOLUTION_SPLIT {
                CAD_RESOLUTION_LOW_DEG
            } else {
                CAD_RESOLUTION_HIGH_DEG
            }
        } else {
            i32::from(walk)
        };
        if stops.room {
            let air = if stops.hue {
                hue_air(ev.hue)
            } else {
                hue_air(0.25)
            };
            let g = g_ioi(ioi_s);
            let bloom_gain = ev.gain * KEY_TINE_TRIM * g * BLOOM_LEVEL * air;
            self.v2_air_cloud(home + key, roof, bloom_gain, ev.pan, t);
        }
        if full {
            // THE PICKUP, at t = 0 — the one cadence voice that leads.
            let mut pickup = tine(
                penta(TINE_BASE_HZ, CAD_PICKUP_DEG + key),
                Touch::Step,
                tau,
                roof,
                false,
                self.v2.flow,
            );
            pickup.lane = LANE_TUNE;
            self.v2_spawn(pickup, ev.gain * KEY_TINE_TRIM * CAD_PICKUP_LEVEL, ev.pan);

            // THE RESOLUTION, at t = T. Low or high C, so the cadence always
            // resolves onto home rather than leaping away from it.
            let deg = if walk <= CAD_RESOLUTION_SPLIT {
                CAD_RESOLUTION_LOW_DEG
            } else {
                CAD_RESOLUTION_HIGH_DEG
            };
            let mut res = tine(
                penta(TINE_BASE_HZ, deg + key),
                Touch::Step,
                tau,
                roof,
                false,
                self.v2.flow,
            );
            res.delay = t;
            res.lane = LANE_TUNE;
            self.v2_spawn(res, ev.gain * KEY_TINE_TRIM, ev.pan);

            // THE FARAWAY ICE BELL, at t = T. Its FM sidebands are
            // deliberately not lattice pitches (§9.5 law 4's second
            // exemption) — that inharmonicity is what "faraway" sounds like.
            let bell = Voice {
                delay: t,
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
            // Half a column out (§11): the bell is FAR, and far is quiet and
            // slightly off-axis, never loud and centred.
            self.v2_spawn(
                bell,
                ev.gain * KEY_TINE_TRIM * CAD_BELL_LEVEL,
                -0.5 * ev.pan,
            );
        }

        // THE TONIC DYAD, at t = T — home in the bass, on every Return. The
        // bass is monophonic (§9.3), and the dyad takes the lane AT ITS
        // ONSET: the previous downbeat is fade-stolen when this one sounds,
        // `T` later, not at the key edge (`lane_onset_steal`).
        let root = penta(BASS_BASE_HZ, key);
        let dyad = Voice {
            delay: t,
            dur: BASS_DUR_S,
            attack: BASS_ATTACK_S,
            decay: BASS_DECAY_S,
            p: [
                Partial {
                    lvl: BASS_ROOT_LVL,
                    f0: root,
                    ..Partial::default()
                },
                Partial {
                    lvl: BASS_FIFTH_LVL,
                    f0: root * 1.5,
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
        };
        self.v2.bass = self.v2_spawn(dyad, ev.gain * KEY_TINE_TRIM * CAD_DYAD_LEVEL, 0.0);
    }

    /// **THE VERDICT** (§ THE VERDICT) — a command's exit code, told once, in
    /// this instrument's own lattice.
    ///
    /// GREEN is a **plagal cadence**: the IV dyad (F4 + C4) falling to the I
    /// dyad (C4 + G4) [`VERDICT_PLAGAL_S`] later, with ONE E5 tine over the
    /// resolution — the note both chords own. A long one adds the faraway ice
    /// bell the Enter cadence already knows ([`CAD_BELL_HZ`], C6), reused
    /// voice for voice: the theme has exactly one bell, and this is it.
    ///
    /// RED is the **vi** alone (A4 + E4), −9 dB, no tine and no bell — the
    /// chord the loop lands on twice a bar, arriving out of turn — and the
    /// chord loop is parked so the next word you type sings against it.
    ///
    /// **NOTHING HERE IS NEW PITCH.** Every frequency is a [`CHORD_LOOP`]
    /// entry through [`CHORD_ROOT_RATIO`] or a [`penta`] degree of
    /// [`TINE_BASE_HZ`]; the cadence sits INSIDE the derived line's lattice
    /// and does not transpose it, and the melody's playhead does not move
    /// ([`MelodyV2::note_verdict`]).
    ///
    /// **LOUDNESS.** The loudest voice is [`VERDICT_DYAD_LEVEL`], which IS
    /// [`BASS_LEVEL`] — the word downbeat's own −6 dB re a step — and every
    /// other voice is under it. Once per command, by the host's arm-and-spend
    /// gate; §9.6's arc is untouched, because a verdict is not a keystroke and
    /// takes no `g_ioi` (the Enter cadence's own rule, held to).
    fn v2_verdict(&mut self, ev: &SoundEvent, at: u32, failed: bool, ice: bool) {
        // The line's own colour for this moment: the same τ, roof and hue arc
        // the cadence would take, read off the melody as it stands. The heat
        // term is `ev.heat`, which the host pins at 0 for every non-authored
        // gesture — output is not authorship, and neither is an exit code.
        let ioi_s = self.v2.ioi_ms * 0.001;
        let tau = tau_v_s(ioi_s);
        let stops = self.v2.stops;
        let arc = if stops.hue { hue_arc(ev.hue) } else { 0.0 };
        let roof = roof_hz(1.0 / ioi_s, true, ev.heat, arc, Touch::Step);
        let key = i32::from(self.song_key);

        // The bass is monophonic (§9.3): each dyad takes the lane at its own
        // ONSET, so the plagal's two chords steal from each other in turn
        // exactly as two word downbeats do, and the last one is what the
        // melody's `bass` field points at.
        let dyad = |me: &mut Self, chord_ix: usize, delay: f32, level: f32| {
            let chord = CHORD_LOOP[chord_ix];
            let root = penta(BASS_BASE_HZ * CHORD_ROOT_RATIO[chord.root], key);
            let partner = root * chord.partner;
            let voice = Voice {
                delay,
                dur: BASS_DUR_S,
                attack: BASS_ATTACK_S,
                decay: BASS_DECAY_S,
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
            };
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
            // THE ONE TINE, over the resolution: E5, the note the IV and the I
            // share. Not the melody's `walk` — the verdict must not read as
            // the line's next note, which is §14's rule for the output pip and
            // is the same rule here for the same reason.
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
                // THE FARAWAY ICE BELL, the Enter cadence's voice verbatim
                // (§11) — the theme has one bell and this is it. Half a
                // column out, because far is quiet and slightly off-axis.
                let bell = Voice {
                    delay: VERDICT_PLAGAL_S,
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

    /// **STARDUST** — one hero star, one token, one glint (§13).
    ///
    /// A single sine on a lattice degree, octave-folded into the STARDUST lane
    /// so every star is the same light whatever note threw it, twinkling at
    /// **the star's own scintillation rate** — what you see winking and what
    /// you hear winking are one number (D12).
    fn v2_glint(&mut self, ev: &SoundEvent, twinkle_hz: u8) {
        let vel = self.v2_velocity(VEL_DB_RESTRIKE);
        // The hero star is its own event and has no strike to stand off
        // from, so it keeps delay zero.
        self.v2_glint_at(ev, twinkle_hz, GLINT_LEVEL, vel, 0.0);
    }

    /// The glint itself, at a level and a velocity the caller owns: the hero
    /// star draws its own velocity, and a typed key's sparkle SHARES the
    /// strike's — it is part of that strike, and a second draw would also
    /// walk the seeded stream every other voice reads from (A27: a script
    /// replays bit-exactly).
    fn v2_glint_at(&mut self, ev: &SoundEvent, twinkle_hz: u8, level: f32, vel: f32, delay: f32) {
        let deg = self.v2.next_glint_deg() + i32::from(self.song_key);
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
            p: [
                Partial {
                    lvl: P1_LVL,
                    f0: f,
                    ..Partial::default()
                },
                Partial::default(),
                Partial::default(),
            ],
            tw_rate: rate,
            tw_depth: GLINT_TWINKLE_DEPTH,
            lp_cut: GLINT_LP_HZ,
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

// ===========================================================================
// §12 — the meteor: one gesture on the observed move
// ===========================================================================

impl TrailSynth {
    /// **THE (default-off) KEYDOWN PRE-CUE** (§15.2, D11).
    ///
    /// It spawns ONLY the tick and a provisional core at −20 dB with a static
    /// pan at the origin, both carrying [`MET_ARM_TTL_S`]. It is off by
    /// default and enabled only where the measured echo p50 exceeds one audio
    /// buffer, because an arm that fires with no move behind it is a stray
    /// "pff" — the audio twin of the stray trail the owner vetoed.
    fn v2_meteor_arm(&mut self, ev: &SoundEvent, meta: EventMeta, dir: i8) {
        let pan = meta.pan_from;
        let tick = Voice {
            dur: MET_TICK_DUR_S,
            attack: MET_TICK_ATTACK_S,
            decay: MET_TICK_DECAY_S,
            n_lvl: 1.0,
            n_f0: MET_TICK_HZ0,
            n_f1: MET_TICK_HZ1,
            n_glide: MET_TICK_GLIDE_S,
            n_q: MET_TICK_Q,
            lane: LANE_METEOR,
            // Both armed voices carry the ttl (`SoundKind::MeteorArm`); the
            // tick's 4 ms life ends long before it could expire, but a claim
            // finds and clears it like the core's.
            arm_ttl: MET_ARM_TTL_S,
            ..Voice::default()
        };
        self.v2_spawn(tick, ev.gain * KEY_TINE_TRIM * MET_TICK_LEVEL, pan);
        // A PROVISIONAL flight: `T` is unknown until the echo lands, so the
        // arm takes the maximum, and the claim re-targets it downward.
        let t = timing::FLIGHT_MAX_MS * 0.001;
        let mut core = self.v2_core_voice(dir, t);
        core.arm_ttl = MET_ARM_TTL_S;
        core.pan_glide_s = 0.0;
        self.v2_spawn(core, ev.gain * KEY_TINE_TRIM * MET_ARM_CORE_LEVEL, pan);
    }

    /// The core's prototype at flight `t` — sine, an octave of exponential
    /// glide in the travel direction, a light inharmonic FM colour that dies
    /// with it (§12.2 layer 2).
    fn v2_core_voice(&self, dir: i8, t: f32) -> Voice {
        let lo = fold_into(
            penta(
                TINE_BASE_HZ,
                i32::from(self.v2.walk) + i32::from(self.song_key),
            ),
            MET_CORE_LO_HZ,
            MET_CORE_HI_HZ,
        );
        // RIGHTWARD / UPWARD RISES (`Dir::sign` = +1); leftward / downward
        // falls. A17 reads this as "Ctrl-E core f1/f0 == 2, Ctrl-A == 0.5".
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

    /// FIZZLE every unclaimed arm over the 12 ms ramp (§12.3). Called when the
    /// promised move turns out to be a sub-floor hop; the ttl in
    /// [`TrailSynth::render`] handles the case where no move comes at all.
    fn v2_release_arm(&mut self) {
        for v in &mut self.voices {
            if v.on && v.lane == LANE_METEOR && v.arm_ttl > 0.0 && v.damp <= 0.0 {
                v.arm_ttl = 0.0;
                v.damp = LANE_FADE_STEAL_S;
                v.damp0 = LANE_FADE_STEAL_S;
            }
        }
    }

    /// CLAIM the armed voices rather than respawning them (§15.2) — **a late
    /// claim re-targets, never sounds twice — and never clicks.** Returns the
    /// re-aimed core's `born`, so the caller can tell the NEW gesture from
    /// the one it is retiring by identity rather than by any field a
    /// previous flight over the same distance would share.
    ///
    /// Every quantity the ear can follow is CONTINUOUS across the claim
    /// instant (§9.5 law 2: "a damp is a 12 ms ramp, never a cut" — a step
    /// up is a cut in reverse):
    /// - **pitch**: the glide's `f0` is re-based so the new exponential,
    ///   seeded from the voice's own `t` with the true `0.45·T`, passes
    ///   through the frequency the arm is sounding RIGHT NOW;
    /// - **level**: the envelope is re-seeded with the true `0.7·T` decay
    ///   and the near-end gain compensated by the ratio of old to new
    ///   envelope at this `t`, so `env × gain` does not move; the CLAIMED
    ///   level (−8 dB, not the arm's −20) is the pan glide's far end and is
    ///   reached over the flight, not in one sample;
    /// - **pan**: the travel starts NOW ([`Voice::pan_t0`]), from the origin
    ///   the arm holds, not from a point already `t` seconds along it;
    /// - **FM**: its τ is left at the arm's provisional value — re-seeding
    ///   the index against a new τ would step the phase.
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
                // The tick — a 4 ms noise burst that has already sounded.
                // Nothing to re-aim; it simply keeps its ttl-free life.
                continue;
            }
            let now = v.t.max(0.0);
            let p = &mut v.p[0];
            // PITCH: what the arm is sounding at this instant …
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
            // LEVEL: the true decay, with the envelope's value at `now`
            // carried across by compensating the near-end gain.
            let env_now = if v.env_run { v.env_d } else { 1.0 };
            v.decay = target.decay.max(1e-3);
            v.k_d = (-dt / v.decay).exp();
            let env_new = (-now / v.decay).exp();
            let comp = env_now / env_new.max(1e-6);
            v.gl *= comp;
            v.gr *= comp;
            v.dur = target.dur.max(now + MET_CLAIM_MIN_TAIL_S);
            // PAN: the far end at the destination, at the claimed level; the
            // near end is wherever the arm is, at the arm's level; the lerp
            // between them starts now and lasts the flight.
            v.pan1 = ev.pan;
            (v.gl1, v.gr1) = pan_gains(claimed_gain, ev.pan);
            v.pan_glide_s = t;
            v.pan_k = 1.0 / t;
            v.pan_t0 = now;
            // Re-seed every recursion on the next sounding sample: with the
            // re-based `f0`, the compensated gain and the untouched FM τ,
            // each restarts on the value it holds now.
            v.env_run = false;
            claimed = Some(v.born);
        }
        claimed
    }

    /// **THE METEOR** (§12) — tick, gliding core, whoosh, bell exactly on the
    /// landing frame, thump, five falling glints.
    ///
    /// Every time constant here is a multiple of `T = flight_ms(cells)`, the
    /// number `crate::rainbow_kitty::timing` hands BOTH the glow and the
    /// synth, so the sound cannot drift from the pixels at any distance
    /// (A16). v1's flight ran on `RAINBOW_METEOR_FLIGHT_S` while its audio ran
    /// on `CURSOR_SWEEP_STEP_S`; that is the coupling defect this function
    /// exists to make impossible.
    fn v2_meteor(&mut self, ev: &SoundEvent, meta: EventMeta, dir: i8, cells: u16, armed: bool) {
        // THE FLOOR (§12.3). A hop under the meteor distance is the nav tick
        // and nothing else — and an arm whose echo turns out to be a
        // three-column Ctrl-A fizzles here, which is the common case the
        // pre-cue exists to be judged on.
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

        // RETIRE THE PREVIOUS GESTURE (§12.3): its SOUNDING core, whoosh and
        // rain ramp down over 15 ms; anything of it still unsounded — its
        // bell, its thump, the rain glints that have not fallen yet — is
        // cancelled outright, "a pre-delayed voice that never started
        // expires unheard", and gives its slot back now rather than in
        // 15 ms. Ctrl-A right after Ctrl-E is a falling core over the same
        // rush, exactly as the pixels retire the previous train. The voice
        // this very call just claimed is the NEW gesture and is excluded by
        // IDENTITY — an earlier unclaimed core flown over the same distance
        // shares every field but `born`.
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
            // Layer 1 — THE TICK, at the origin, 4 ms long. The ear
            // time-stamps a transient, so the gesture must OPEN with one.
            let tick = Voice {
                dur: MET_TICK_DUR_S,
                attack: MET_TICK_ATTACK_S,
                decay: MET_TICK_DECAY_S,
                n_lvl: 1.0,
                n_f0: MET_TICK_HZ0,
                n_f1: MET_TICK_HZ1,
                n_glide: MET_TICK_GLIDE_S,
                n_q: MET_TICK_Q,
                lane: LANE_METEOR,
                ..Voice::default()
            };
            self.v2_spawn(tick, g * MET_TICK_LEVEL, meta.pan_from);

            // Layer 2 — THE CORE, carrying direction and travelling.
            let mut core = self.v2_core_voice(dir, t);
            core.pan1 = ev.pan;
            let vel = self.v2_velocity(VEL_DB_MOTION);
            self.v2_spawn(core, g * MET_CORE_LEVEL * vel, meta.pan_from);
        }

        // Layer 3 — THE WHOOSH, direction-BLIND (A17). The rush of travel is
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

        // Layer 4 — THE BELL, at exactly `t = T`. The tune's current note,
        // thrown across the line: navigation never advances the verse, it
        // only carries it.
        let ioi_s = self.v2.ioi_ms * 0.001;
        let deg = i32::from(self.v2.walk) + i32::from(self.song_key);
        let arc = if self.v2.stops.hue {
            hue_arc(ev.hue)
        } else {
            0.0
        };
        let mut bell = tine(
            penta(TINE_BASE_HZ, deg),
            Touch::Step,
            MET_BELL_TAU_S,
            roof_hz(1.0 / ioi_s, true, ev.heat, arc, Touch::Step),
            false,
            self.v2.flow,
        );
        bell.delay = t;
        bell.dur = MET_BELL_DUR_S;
        self.v2.bell = self.v2_spawn(bell, g * MET_BELL_LEVEL, ev.pan);

        // Layer 5 — THE THUMP, the chord root under the landing.
        let chord = CHORD_LOOP[usize::from(self.v2.chord)];
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

        // Layer 6 — THE RAIN: five glints, one per fan star, in the fan's own
        // throw order, spaced by the SHARED `rain_spacing_ms(T)` so the shower
        // is as long as the flight was and the last glint is gone by T + 250.
        let step_s = timing::rain_spacing_ms(t * 1000.0) * 0.001;
        for k in 0..timing::FAN_HERO_N {
            let glint = Voice {
                delay: t + MET_RAIN_LEAD_S + k as f32 * step_s,
                dur: MET_RAIN_DUR_S,
                attack: MET_RAIN_ATTACK_S,
                decay: MET_RAIN_DECAY_S,
                p: [
                    Partial {
                        lvl: P1_LVL,
                        f0: MET_RAIN_HZ[k],
                        ..Partial::default()
                    },
                    Partial::default(),
                    Partial::default(),
                ],
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

// ===========================================================================
// The palette entry
// ===========================================================================

/// **THE MUSIC BOX**, as a [`Palette`] roster entry.
///
/// v2 does not use the palette dispatch: [`TrailSynth::push_meta`] routes a v2
/// event to [`TrailSynth::push_v2`] before `design_trail` is ever reached, and
/// that is the structural claim §16 row 9 makes ("no shared function's
/// arithmetic is edited"). This impl exists so the registry stays total and so
/// a hand-built event that somehow arrives at the dispatch still sounds like
/// the tine rather than falling through to another style's timbre.
pub struct RainbowKittyV2Palette;

impl Palette for RainbowKittyV2Palette {
    fn design(
        &self,
        s: &mut TrailSynth,
        ev: &SoundEvent,
        _kind: SoundKind,
        g: f32,
        deg: i32,
        _col_off: i32,
    ) {
        let ioi_s = s.v2.ioi_ms * 0.001;
        let arc = if s.v2.stops.hue { hue_arc(ev.hue) } else { 0.0 };
        let voice = tine(
            penta(TINE_BASE_HZ, deg),
            Touch::Step,
            tau_v_s(ioi_s),
            roof_hz(1.0 / ioi_s, false, ev.heat, arc, Touch::Step),
            false,
            s.v2.flow,
        );
        s.spawn(voice, g * KEY_TINE_TRIM, ev.pan);
    }

    /// THE RAINBOW SKY (THE PRISM §3.2) — the same body the tournament's
    /// `BedVariant::RainbowSky` renders, so what the owner auditions as
    /// `c5-rainbow-sky` is byte for byte what the knob turns on. §9.7's "no
    /// bed by default" still holds where it is decided: the
    /// `trail_sound_bed` setting ships OFF, so `push_v2` feeds this nothing
    /// and it is never reached (`bed_sample`'s level floor); the ear decides
    /// the default from the audition WAVs.
    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32, _u1: f32, _u2: f32) -> (f32, f32) {
        s.bed_rainbow_sky(dt, lvl)
    }

    fn anchor_hz(&self) -> f32 {
        TINE_BASE_HZ
    }
}

// ===========================================================================
// THE RAINBOW SKY — the body
// ===========================================================================

impl TrailSynth {
    /// One stereo sample `(mid, side)` of the sky pad (THE PRISM §3.2), the
    /// body behind both `BedVariant::RainbowSky` and the music box's palette
    /// bed. The caller has floored `bed.level` and folded level × gain into
    /// `lvl`; the result lands in the ducked mix sum like every other bed.
    ///
    /// Modelled on `bed_chord_drift`'s proven arithmetic — seed-at-target,
    /// one-pole portamento on the oscillator frequencies, weighted sum — with
    /// three differences that are the design: the bar is the LIVE chord
    /// (`v2.chord`, which `on_space` advances once per word) instead of a
    /// 30 s timer; each tone carries [`BED_PARTIALS`] so there is a spectrum
    /// for the hue to tilt; and the hue's arc (`bed.hue_s`) drives the tilt
    /// and the top tone's twin detune — and never a pitch, so no amount of
    /// hue motion can take the pad out of key. Sample-driven throughout, no
    /// rng: a candidate render is bit-replayable from (events, seed).
    pub(super) fn bed_rainbow_sky(&mut self, dt: f32, lvl: f32) -> (f32, f32) {
        let degs = sky_bed_degrees(usize::from(self.v2.chord));
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
                // First sample: seed at target so the pad enters ON the
                // chord instead of sweeping up from 0 Hz.
                b.var_f[i] = tgt[i];
            }
            b.var_f[i] += (tgt[i] - b.var_f[i]) * glide;
            b.var_ph[i] = (b.var_ph[i] + b.var_f[i] * dt).fract();
            let mut x = tone(b.var_ph[i]);
            if i == 2 {
                // THE WIDTH: the top tone's detuned twin, half and half, so
                // the pair sums to the tone's weight when in phase and beats
                // at `f · (detune − 1)` — the shimmer.
                b.var_ph[3] = (b.var_ph[3] + b.var_f[2] * detune * dt).fract();
                x = 0.5 * (x + tone(b.var_ph[3]));
            }
            m += x * BED_WEIGHT[i];
        }
        // THE TILT: one one-pole over the pad sum, its cutoff on the arc.
        let cut = lerp(BED_TILT_LO_HZ, BED_TILT_HI_HZ, arc);
        let k = (cut * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (m - b.lp1);
        // THE BREATH: a raised cosine on the whole pad, on its own phase
        // (`ph3`, which nothing else in the music box's bed uses), so the
        // body is the same on the tournament clock and the palette path.
        b.ph3 = (b.ph3 + BED_BREATH_HZ * dt).fract();
        let breath = 1.0 - BED_BREATH_DEPTH * (0.5 - 0.5 * super::sin01((b.ph3 + 0.25).fract()));
        (b.lp1 * breath * lvl * BED_LEVEL, 0.0)
    }
}

// ===========================================================================
// §20.2 — the laws, pinned
// ===========================================================================

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

    /// A fresh synth. Nothing to engage: every event below carries the
    /// rainbow kitty look, and that look IS the music box (§17.3 phase 7 —
    /// the latch is gone, `TrailSynth::v2_engaged` reads the event alone).
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

    /// A KEYSTROKE WITH A CHARACTER BEHIND IT — the shipping seam's own
    /// stamp, through the engine's own rank producer, because the derived
    /// melody's whole input is `(rank, at_ms)` and a fixture that leaves the
    /// rank at 0 is testing the no-glyph fallback rather than the melody.
    fn push_ch(s: &mut TrailSynth, kind: SoundKind, at: u32, ch: char) {
        s.push_meta(
            event(kind, 0.0, ch.is_uppercase()),
            EventMeta {
                at_ms: at,
                rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                ..EventMeta::default()
            },
        );
    }

    // -- THE RAINBOW SKY (THE PRISM §3.2) -----------------------------------

    /// THE PAD IS THE HARMONY THE MELODY IS SNAPPED TO: for every chord of the
    /// loop the sky voices exactly that chord's three LIT verse degrees,
    /// ascending, on the untransposed lattice — so nothing it plays can be out
    /// of key against a derived step — and every pad tone sits under the bass
    /// register ([`BASS_BASE_HZ`]), two octaves under the tine. The partial
    /// set is octaves, fifths and the major third only, normalised to a
    /// sine's RMS.
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

    /// THE HUE DRIVES THE TILT AND THE WIDTH, AND NEVER A PITCH: the same pad
    /// rendered at the red end and the cyan end of the arc keeps its
    /// oscillator frequencies bit for bit, and is brighter (more of its energy
    /// in its first difference) at the cyan end. And the arc is slewed per v2
    /// event through the one-pole — seeded on the first key, glided after.
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

    /// THE SKY IS FED BY THE MUSIC BOX'S OWN KEYS, BEHIND THE KNOB: with
    /// `bed: false` (the shipping default) the bed's energy never leaves its
    /// exact-zero floor and the pad contributes nothing; with `bed: true` a
    /// keystroke kicks it by v1's own number, it sounds under the notes, and
    /// after the typing stops it exhales to EXACT zero so the host's idle
    /// pause still engages.
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
        // The pad is actually in the output while the notes are up (the
        // palette path, not only the tournament's)…
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
    /// return the degree the line sounded on each TYPED key.
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

    /// Render `blocks` × 480 frames and return the interleaved output's peak.
    /// **THE VERDICT, sense 2 — the sound.** Three laws in one run, all of
    /// them about who is allowed to speak and how loudly.
    ///
    /// 1. **THE MUSIC BOX'S ALONE.** The nine other voices are SILENT for a
    ///    verdict — a plagal cadence needs the chord loop, and the chord loop
    ///    is this instrument's. This is also the whole of "the nine other
    ///    voices stay byte-exact": there is no new arithmetic on their path.
    /// 2. **GREEN AND RED BOTH SPEAK**, and green resolves — the plagal's two
    ///    dyads land as two separate onsets, so the tail is still ringing well
    ///    past [`VERDICT_PLAGAL_S`].
    /// 3. **≤ −6 dB RE A STEP.** The verdict's peak stays under a plain
    ///    keystroke's, once per command, which is [`VERDICT_DYAD_LEVEL`]
    ///    stated as an outcome rather than as a constant.
    ///
    /// Before the change the gesture did not exist and every one of these
    /// renders was silence.
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
        // ≤ −6 dB RE A STEP, as a DELIVERED peak and not merely as a level
        // constant: the ceiling the gesture is allowed, measured.
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

    /// SPECTRAL CENTROID of a Hann-windowed slice, by naive DFT. Small and
    /// slow on purpose: the quantity under test is a design property, and a
    /// hand-rolled transform has no library version to drift under it.
    fn centroid_hz(x: &[f32]) -> f32 {
        let n = x.len();
        let win: Vec<f32> = (0..n)
            .map(|i| 0.5 * (1.0 - (core::f32::consts::TAU * i as f32 / n as f32).cos()))
            .collect();
        let mut num = 0.0f64;
        let mut den = 0.0f64;
        for k in 1..n / 2 {
            let mut re = 0.0f64;
            let mut im = 0.0f64;
            let w = core::f64::consts::TAU * k as f64 / n as f64;
            for (i, (s, h)) in x.iter().zip(&win).enumerate() {
                let a = w * i as f64;
                let v = f64::from(*s * *h);
                re += v * a.cos();
                im -= v * a.sin();
            }
            let mag = (re * re + im * im).sqrt();
            let f = k as f64 * f64::from(SR) / n as f64;
            num += f * mag;
            den += mag;
        }
        if den <= 0.0 { 0.0 } else { (num / den) as f32 }
    }

    // -- A28: the ladder floor ------------------------------------------

    /// **THE ISOLATED STEP LANDS ON THE LADDER FLOOR** — −21.0 dBFS at the
    /// host default volume, heat 0.5 (§9.1's trim row, A28).
    ///
    /// This is the pin [`KEY_TINE_TRIM`] is FITTED against, and it is fitted
    /// on PEAK because §9.6 rules that the loudness arc may buy brightness and
    /// never loudness: the quantity that must sit on the floor is the one the
    /// whole ladder is written in.
    ///
    /// **Pinned on the NOMINAL step — §9.6's seeded velocity (±1 dB) and pan
    /// jitter (±0.03) divided back out.** A28's ±0.3 dB cannot be a law on
    /// one seed's draw: the trim had been fitted to THIS fixture's +0.96 dB
    /// draw, which put the nominal step 1.0 dB under the floor, and the
    /// bench's seed (`keyboard_song_ab`, "POOF", a −0.55 dB draw) read the
    /// same instrument at −22.7 dBFS on 2026-09-05. The peak is linear in
    /// the voice's gain below the limiter, so the measured peak scaled by
    /// `nominal / seeded` IS the nominal peak, exactly.
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

    /// **ONE KEYSTROKE IS ONE MELODY STEP, AT EVERY TYPING SPEED** (R1,
    /// ruled 2026-09-08) — and no keystroke is ever silent.
    ///
    /// This test replaces `the_verse_advances_on_the_clock_and_every_key_
    /// still_speaks`, which asserted the DELETED 220 ms gate: that two steps
    /// were never closer than 220 ms and that at 10 cps the line stepped
    /// every third key. The owner ruled that law "the OPPOSITE of what I
    /// want", so the test that pinned it was asserting the defect.
    ///
    /// The sweep runs from 3 cps to 50 cps — past any hand, into auto-repeat
    /// — and at every rate the melody must advance once per key AND spawn a
    /// TUNE voice for every key. There is no rate at which either may fall
    /// off, which is why the sweep goes well past the rate a person can
    /// reach: a gate that grew back somewhere would show as a hole here
    /// before it ever reached an ear.
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

    /// **A HELD KEY STILL STEPS AND STILL SPEAKS** — it is *felt* rather than
    /// *pitched*, and never silent (§3.1's auto-repeat clause).
    ///
    /// macOS key repeat is one glyph on a machine-regular clock, which is
    /// what [`AUTOREPEAT_JITTER_MS`] tests for. The old law coalesced those
    /// onsets away; the new one takes the tine's partials to zero and leaves
    /// the mallet, so the roll is felt under the fingers without becoming a
    /// pitched machine gun — and the melody keeps moving underneath it.
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

        // A GENUINELY FAST HUMAN HAND IS NOT A MACHINE. The same rate with
        // human jitter on it, and different letters, stays pitched.
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

    /// **THE SAME TEXT TYPED THE SAME WAY IS THE SAME LINE; TYPED
    /// DIFFERENTLY IT IS A DIFFERENT ONE** (R2, and §3.1's determinism
    /// clause).
    ///
    /// This is the whole content of "generated from the typing patterns", as
    /// a falsifiable statement about the degree sequence: the text alone does
    /// not fix the tune, the rhythm alone does not fix the tune, and the two
    /// together fix it exactly.
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

        // A DIFFERENT RHYTHM, same text: the contour reads the hand, so the
        // line must differ.
        let hurried = type_degrees_rhythm(TEXT, 1_000, &[120, 340, 150, 300, 130]);
        assert_ne!(
            steady, hurried,
            "the same text typed with a different rhythm played the SAME line — \
             the hand is not reaching the melody"
        );

        // A DIFFERENT TEXT, same rhythm: the alphabet reads the glyphs, so
        // the line must differ — and must differ from the word that changed
        // onward, not before it.
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

    /// **THE PATHOLOGICAL STREAM NEVER BECOMES A SIREN** (§3.1 step 5).
    ///
    /// The bench's own worst input — a held key, a digit run, `!!!!`, a
    /// base64 blob and one word eight times — put through the melody, with
    /// three closed guarantees checked on the degree sequence it produces:
    /// every degree is inside the TUNE register (the reflection never lets it
    /// out), no stride repeats more than [`MELODY_RUN_MAX`] times (the line
    /// never climbs or falls without turning), and the line does not sit on
    /// one degree.
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

    /// **THE RUN GUARD HOLDS ON THE LINE THE EAR GETS**, not on the stride
    /// `derive` counted — the bench's `scenario_pathological`, stream for
    /// stream and pause for pause, plus the prose the answer leaks on.
    ///
    /// The render's siren verdict found `0 1 2 3 4` in this exact take: the
    /// held `a`s park the line on degree 0, the 783 ms think before `0` reads
    /// as a hesitation and turns the stride negative, the floor reflects it
    /// to `+1` — and the run was booked as `−1`, so three more `+1`s passed
    /// the guard. It also found every answer of a rising subject sounding as
    /// a straight five-note scale in `prose` and `rotate`. Both are counted
    /// here the way the bench counts them, on the sounded degrees.
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
        // THE PROSE AS THE BENCH'S HAND TYPES IT — `type_text`'s cadence,
        // rest for rest: 100 ms a key at 10 cps, a 550 ms sentence rest
        // after a full stop, a Jump and a 350 ms beat of thought at a line
        // ending. The rests matter: a rest resolves the walk and re-latches
        // the subject, and the third leak the render caught (a `+3` off
        // degree 6 sounding as the fourth `+1` of a run, at key 295) only
        // arises on the subject THIS cadence latches.
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

    /// **EVERY REPEATED PITCH IS CHARGED TO A CAUSE THE DESIGN WROTE DOWN**
    /// — §8 step 3's "the census reads 100 % distinct", in the only form that
    /// is simultaneously true and worth having.
    ///
    /// §8 asks for 100 % distinct. §3.1 of the same document keeps
    /// [`Touch::ReStrike`] for "a stride of zero — a genuinely repeated
    /// pitch, from a doubled letter". Both cannot hold of one column: a
    /// corpus that types `ll` has ASKED for the same note twice, and an
    /// engine that refused would be overwriting the text instead of deriving
    /// from it. So the claim is not a percentage — it is an ACCOUNT. Every
    /// key that repeats the sounding degree is charged to one of the three
    /// causes §3.1 names:
    ///
    /// * the text asked (this key's alphabet rank equals the last one's);
    /// * a word head whose chord snap held a common tone across the change;
    /// * the line answering its own subject on a latched unison.
    ///
    /// **The residual must be zero**, because a repeat with no cause is the
    /// engine standing still while the hand moved — R1's defect in miniature,
    /// and exactly what the deleted gate used to do wholesale.
    ///
    /// THIS TEST HAS ALREADY CAUGHT ONE. The subject used to latch its first
    /// interval off the session's opening key, which has no note before it to
    /// be a distance from, so the interval was a unison the text never typed
    /// — and it came back as a repeated note at EVERY answering word head for
    /// the rest of the session: 24 of them in the bench's 403-key prose, all
    /// at `word_pos == 1`. Held against the whole sweep of rhythms below,
    /// nothing like it can land again unnoticed.
    #[test]
    fn every_repeated_pitch_is_charged_to_a_cause_the_design_names() {
        const PATHOLOGICAL: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1234567890 !!!! \
             aGVsbG8gd29ybGQgdGhpcyBpcyBub3QgcHJvc2U ffff the the the the the the the the";
        // A metronome, a hurried hand, a hesitating one, and a machine-fast
        // run — the contour reads the gap, so each is a different line and
        // each gets its own account.
        const RHYTHMS: [&[u32]; 4] = [
            &[200],
            &[83, 91, 83, 77, 83],
            &[140, 500, 130, 620, 150],
            &[71],
        ];
        // `ordinary` says whether the corpus is text a person would write.
        // The pathological one is 30 held `a`s, `!!!!`, `ffff` and one word
        // eight times: a third of its keys ARE a doubled letter, so a bound
        // on the TOTAL repeat share there would be a bound on the fixture
        // rather than on the engine.
        for (name, text, ordinary) in [
            ("prose", BENCH_PROSE, true),
            ("pathological", PATHOLOGICAL, false),
        ] {
            for periods in RHYTHMS {
                let mut s = synth();
                let mut at = 1_000u32;
                let (mut keys, mut dbl, mut head, mut subj, mut stall) = (0, 0, 0, 0, 0);
                let mut prev_rank = 0u8;
                let mut prev_deg: Option<i8> = None;
                for (i, ch) in text.chars().enumerate() {
                    let kind = match ch {
                        ' ' => SoundKind::Space,
                        '\n' => SoundKind::Enter { cells: 30 },
                        _ => SoundKind::Typed,
                    };
                    // Read BEFORE the push: both predicates move on it, and
                    // both are the engine's own — `word_pos == 0` is the word
                    // head branch, `!word_head && motif_play > 0` is the
                    // replay branch, verbatim.
                    let was_head = s.v2.word_pos() == 0;
                    let was_answer = !was_head && s.v2.motif_answering();
                    let rank = crate::trail_sound::typed_glyph_rank(Some(ch));
                    push_ch(&mut s, kind, at, ch);
                    if matches!(kind, SoundKind::Typed) {
                        keys += 1;
                        let deg = s.v2.walk();
                        if prev_deg == Some(deg) {
                            // Charged to the branch that PRODUCED the degree,
                            // never to the first plausible story: an answered
                            // interval never consulted the alphabet.
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
                     still while the hand moved (dbl {dbl}, head {head}, subj {subj})"
                );
                // WHAT THE ENGINE ADDS TO THE TEXT'S OWN REPETITION is
                // bounded on EVERY corpus. `dbl` is the text's doing and a
                // corpus is entitled to as many doubled letters as it likes;
                // `head` and `subj` are the instrument's, and an instrument
                // that returns to the sounding note on a tenth of the keys of
                // its own accord is decorating rather than deriving.
                assert!(
                    (head + subj) * 10 < keys,
                    "the ENGINE repeated the sounding pitch on {} of {keys} keys on \
                     {name} at rhythm {periods:?} (head {head}, subj {subj}) — \
                     accounted for, but that is the instrument's repetition, not \
                     the text's",
                    head + subj
                );
                // …and on text a person would actually write, the total is
                // bounded too: past a tenth the line is repeating more than
                // it is deriving, whatever the account says.
                assert!(
                    !ordinary || (dbl + head + subj) * 10 < keys,
                    "{} of {keys} keys on {name} at rhythm {periods:?} repeated the \
                     last pitch (dbl {dbl}, head {head}, subj {subj}) — accounted \
                     for, but that is no longer a derived line",
                    dbl + head + subj
                );
            }
        }
    }

    // -- A2: the anti-leap law, on the bench's own prose -------------------

    /// `keyboard_song_ab`'s prose corpus, verbatim — §21.4's `prose-10cps`
    /// is THIS text at 10 cps, and A2 is pinned on it rather than on a
    /// synthetic word so the pin sees every form wrap, every line feed and
    /// every sentence rest the bench sees.
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
    /// shifted).
    type Cue = (f32, SoundKind, f32, f32, bool);

    /// The bench's `needs_shift`: the glyphs a US layout cannot produce
    /// without Shift.
    fn bench_needs_shift(ch: char) -> bool {
        ch.is_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(ch)
    }

    /// The bench's `type_text`, verbatim: a space is a `Space`, a newline a
    /// `Jump` (the bench cues line feeds as PTY jumps), everything else
    /// `Typed`; a line ending rests 350 ms and a full stop 550.
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
            cues.push((t, kind, pan, heat, bench_needs_shift(ch)));
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

    /// **THE LOUDNESS HARNESS** (§9.6's law, and flow's own guard) — drive
    /// the bench's prose corpus at `cps` with the flow heat pinned at `flow`
    /// for every cue, render the whole take, and return its
    /// `(rms dBFS, peak dBFS)`.
    ///
    /// The window is a FIXED WALL CLOCK at every rate, because the quantity
    /// §9.6 bounds is loudness — energy per second — and a window that grew
    /// with the note count would measure nothing at all: a take twice as
    /// dense over twice as long has the same RMS as the sparse one and the
    /// law would be vacuous.
    ///
    /// STAMPED, unlike [`drive_bench_prose`]: `at_ms` from the script's own
    /// press time and `rank` from the glyph, because the loudness of a take
    /// depends on which notes the derivation chose (a passing note is 2 dB
    /// under a lit one) and the unranked fallback plays a different line.
    fn prose_loudness(cps: f32, flow: f32) -> (f32, f32) {
        const BLOCK: usize = 512;
        const TAKE_S: f32 = 30.0;
        // (press time s, gesture, pan, shifted, rank)
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
            // The bench's think between paragraphs.
            t += 1.4;
        }
        cues.retain(|c| c.0 < TAKE_S);
        let mut s = TrailSynth::new(SR, 0x504F_4F46);
        let frames = (TAKE_S * SR) as usize;
        let mut stereo = vec![0.0f32; BLOCK * 2];
        let (mut f, mut ci) = (0usize, 0usize);
        let (mut sq, mut n, mut peak) = (0.0f64, 0usize, 0.0f32);
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

    /// [`push_ch`] with a flow heat on the side-car (§22).
    fn push_meta_ch(s: &mut TrailSynth, kind: SoundKind, at: u32, ch: char, flow: f32) {
        s.push_meta(
            event(kind, 0.0, ch.is_uppercase()),
            EventMeta {
                at_ms: at,
                rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                flow,
                ..EventMeta::default()
            },
        );
    }

    /// **THE RING-OUT PROBE** (§22's "opens") — one word in isolation:
    /// `t`, `h`, `e` at 10 cps and the Space that ends them, at flow heat
    /// `flow`, rendered for 1.5 s from the first key.
    ///
    /// Returns `(peak dBFS, body dBFS)`, where the BODY is the RMS of the
    /// window from 400 ms to 1200 ms. Every onset in the script is over by
    /// 300 ms, so nothing in that window is a strike: it is the BOX RINGING,
    /// which is the quantity "fuller" actually names. A theme that got fuller
    /// by getting louder would move the peak; one that got fuller by ringing
    /// longer moves only the body, and the two numbers together are what
    /// separate them.
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
    /// fundamental and the verse degree the melody was sounding after that
    /// cue.
    #[derive(Clone, Copy, Debug)]
    struct Spawn {
        cue: usize,
        lane: u8,
        f0: f32,
        deg: i8,
    }

    /// Drive the bench's prose corpus and cue script — the bench's seed, the
    /// music box reached by voice, 512-frame blocks — logging every spawn and
    /// the melody's `word_pos` after every cue.
    ///
    /// Deliberately still on UNSTAMPED `push`, i.e. the synth's own block
    /// clock: `keyboard_song_ab` now stamps `EventMeta::at_ms` with the
    /// scripted press time (2026-09-08), and A2's anti-leap law must hold on
    /// BOTH clocks — the host stamp and the block-clock fallback a host with
    /// nothing to stamp still lands on. This is the fallback's pin; the bench
    /// is the stamped one.
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
                let (ct, kind, pan, heat, shifted) = cues[ci];
                let mut ev = event(kind, pan, shifted);
                ev.heat = heat;
                ev.hue = (ct * 0.18).fract();
                ev.voice = SoundVoice::RainbowKittyV2;
                let mark = s.born_seq;
                s.push(ev);
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

    /// **CONSECUTIVE TYPED ONSETS NEVER LEAP INSIDE A WORD** (§9.0 cause 1,
    /// A2) — the measured defect this whole instrument exists to cure, pinned
    /// on the bench's own 60 s of prose at 10 cps.
    ///
    /// v1 voiced two keystrokes in three as a ghost an octave down, a fourth
    /// down or a third up from the accent, on the identical bright bell: about
    /// 6.7 octave-class leaps a second at 10 cps. v2 has no ghost lane at all,
    /// so the law is stated positively and EXACTLY: inside a word — between
    /// two `Typed` cues with no Space, line feed or Enter between them — a
    /// typed onset is a unison (a doubled letter), or a derived stride — at
    /// most [`WORD_LEAP_MAX_DEG`] degrees, which [`MelodyV2::derive`] clamps
    /// by construction. **There is no longer any exemption.** The authored
    /// form's wrap (A″'s peak G6 leaning back onto A's C5, degree 8 → 0) was
    /// the one leap wider than a sixth this test used to allow; the derived
    /// line has no form to wrap, and reflects at the register's bounds
    /// instead of leaping across them, so the exemption is deleted rather
    /// than widened.
    ///
    /// The capture-after analysis of 2026-09-05 reported four "in-word"
    /// leaps beyond the wrap; every one straddled a LINE FEED (the brrrring's
    /// cadence note and its +5 top, then the next line's first note), which
    /// that analysis could not see because it drew word boundaries at bass
    /// onsets alone. So this pin also states the finding: across the whole
    /// take, every consecutive lead-lane pair wider than a sixth is either the
    /// wrap or straddles a `Jump`; and a line feed ENDS the word (the next
    /// letter is a word head), so a rest after it is a rest between words.
    #[test]
    fn consecutive_typed_onsets_are_never_a_leap_inside_a_word() {
        let cues = bench_prose_cues();
        let (s, log, word_pos) = drive_bench_prose(&cues);
        assert_eq!(s.steals(), 0, "the prose take ran the voice pool dry");

        // INSIDE A WORD: consecutive TUNE spawns minted by Typed cues with no
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
        // §21.4's CONJUNCT SHARE — ≥ 60 % unison-or-one-degree, KEPT, and now
        // earned by the melody rather than by the gate.
        //
        // The old number was met because above 4.5 cps most keys were a muted
        // repeat of the last note: the "conjunct" share was really the
        // tremolo's. With the gate deleted it is the derived line's own, and
        // it is higher — measured 76.5 % of 307 in-word pairs, against the
        // 46 % that [`stride_mag`]'s bands alone would give over a uniform
        // fold. Gravity's ±1 and [`MELODY_RUN_MAX`]'s inversion are what make
        // up the difference, which puts the line inside the 70-80 % conjunct
        // that the ladder's own doc says real melodies run at.
        let pct = 100.0 * near as f32 / pairs as f32;
        println!("in-word conjunct share: {pct:.1} % of {pairs} pairs");
        assert!(
            pct >= 60.0,
            "only {pct:.0} % of {pairs} in-word pairs were a unison or one degree"
        );

        // THE FINDING: every lead pair wider than a sixth is the wrap or
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

        // A LINE FEED ENDS THE WORD: the first letter after a `Jump` is a word
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

    /// **A MID-WORD REST NEVER CADENCES BY MORE THAN A SIXTH** (§10.2's
    /// cadence, made exact for A2 by [`MelodyV2::cadence_degree`]).
    ///
    /// A think-pause after the hook's FIRST note, inside the word, would
    /// close the phrase onto its last — E6 over C5, a tenth, the one
    /// octave-class leap the instrument could still make between two keys of
    /// one word. Folded, the cadence lands on the chord tone nearest that
    /// final degree inside a sixth (A5 under the parked IV) and the next
    /// phrase still opens where §10.2 says. Between words the cadence is
    /// §10.2's own: the same pause after a Space closes onto E6 itself.
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

        // THE RESOLUTION ITSELF is a chord tone, and it happens BEFORE the
        // key derives its own note — so a rest lands the line on the harmony
        // rather than wherever the last letter left it.
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

    /// **THE COLUMN AND THE MOOD NEVER MOVE THE TUNE** (§9.0 cause 2, §2.6,
    /// A4).
    ///
    /// v1 added `col_off = pan.round()` to the degree, so one verse note was
    /// three different pitches across one line, and the tone tables
    /// transposed the whole lattice by 9/8 when the classifier read
    /// "excited". Both are gone: the same script at hard left, centre and
    /// hard right, under all five tones, plays the same notes.
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
        // …while MOOD STILL BENDS THE DECAY (§10.4: "`tone_feel` still
        // multiplies decay"): the same step under Calm rings 1.06× longer,
        // under Excited 0.88× — feel, never pitch.
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

    /// **SHOUTING IS A COLOUR, NOT A CEILING** — the panel's Q4 ruling of
    /// 2026-09-09, as a law.
    ///
    /// A run of capitals used to be a permanent transposition sitting on the
    /// clamp: the rendered shift scene measured ALL CAPS at mean degree 7.3
    /// with 19 of its 21 keys on degrees 7-8 and 11 of them on the single
    /// degree 8 — the string `878787858785878787878`, which is not a melody
    /// but an alarm. Both halves of the cure are pinned here, because either
    /// one alone leaves the defect reachable: the lift is a fifth rather than
    /// an octave, and it REFLECTS at the register's bound rather than
    /// clamping to it (this file's own step-4 law, which the lift was the one
    /// place to break).
    ///
    /// The bound is stated against the LOWER-CASE line's own occupancy, so
    /// this cannot be satisfied by making capitals quieter or lower: shouting
    /// must still sit above ordinary text, it just may not pile onto one
    /// note.
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
        // …and it is still SHOUTING: capitals sit above the same text typed
        // in lower case.
        let mean = |v: &[i32]| v.iter().sum::<i32>() as f32 / v.len() as f32;
        assert!(
            mean(&caps) > mean(&lower),
            "shouted {:.2} vs spoken {:.2}: the lift stopped being audible",
            mean(&caps),
            mean(&lower)
        );
    }

    /// **A CAPITAL IS ONE ONSET, AN OCTAVE UP; A BARE SHIFT IS FELT, NOT
    /// PITCHED** (§3.1 "Boundaries", §2.3 i-ii, §8 step 4).
    ///
    /// One capital used to be three sounds: the bare Shift's pitched lift,
    /// the letter, and an octave echo 25 ms behind it at −8 dB. Now the
    /// letter is its own single step lifted [`CAPITAL_LIFT_DEG`] degrees —
    /// the same derivation as its lowercase twin, higher — and the modifier
    /// is the felt mallet alone.
    ///
    /// **RE-PINNED ON THE PANEL'S Q4 RULING (2026-09-09).** The lift was
    /// five degrees `.min(TUNE_DEG_HI)` and is three degrees through
    /// [`reflect_deg`]: this assertion read the clamp back, so it is the
    /// clamp it had to stop reading. It now says what the ruling says — the
    /// capital is the lowercase note lifted, and at the top of the register
    /// it TURNS rather than piling onto degree 8 (a run of capitals measured
    /// 19 of 21 keys on two pitches before this).
    #[test]
    fn a_capital_is_one_lifted_onset_and_a_bare_shift_is_felt_not_pitched() {
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
        let (low, _) = walk_after(false);
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
        // No second pitched sound of the capital's own: nothing at the
        // retired echo's 25 ms, nothing at the note's octave.
        let f = tune[0].p[0].f0;
        for v in &spawned {
            assert!(
                v.lane != LANE_TUNE || core::ptr::eq(v, tune[0]),
                "a second TUNE voice rode the capital"
            );
            assert!(
                (v.delay - 0.025).abs() > 1e-6 && (v.p[0].f0 - 2.0 * f).abs() > SAME_PITCH_HZ,
                "the capital's octave echo is back (delay {} s, f0 {} Hz)",
                v.delay,
                v.p[0].f0
            );
        }

        // THE BARE MODIFIER: one voice in the SHIFT lane, every partial at
        // zero, the mallet alone, at 0.4 of the lift's old level.
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let mark = s.born_seq;
        push(&mut s, SoundKind::Shift, 1_300, 0.0, false);
        let lift = since(&s, mark);
        assert_eq!(lift.len(), 1, "a bare Shift spawned {} voices", lift.len());
        let v = lift[0];
        assert_eq!(v.lane, LANE_SHIFT);
        assert!(
            v.p.iter().all(|p| p.lvl == 0.0) && v.n_lvl > 0.0,
            "a bare Shift must be felt (mallet only), never pitched"
        );
        let nominal = VOL * KEY_TINE_TRIM * LIFT_LEVEL * LIFT_FELT_MUL;
        let got = (v.gl * v.gl + v.gr * v.gr).sqrt();
        assert!(
            (got / nominal - 1.0).abs() < 0.05,
            "the felt lift is {got} against LIFT_LEVEL × 0.4 = {nominal}"
        );
    }

    /// **A DOUBLED LETTER IS ALWAYS A RE-STRIKE** (§3.1 step 1), in the
    /// outer register too: gravity acts on a stride, never on a repeat. Every
    /// prefix of the pangram, every letter doubled after it.
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
                assert!(s.v2.restrike >= 1, "the re-strike ladder did not arm");
            }
        }
        assert!(
            outer > 0,
            "fixture: {checked} doubles checked and none stood where gravity is armed"
        );
    }

    /// **A WORD HEAD'S COMMON TONE IS AN ACCENT, NOT A TREMOLO** (§3.1 step
    /// 6; step-3 review, finding 1). When the chord snap lands the head on
    /// the degree the previous word ended on, the head keeps the STEP's
    /// touch — full level, strike partial, roof — and the previous voice is
    /// still damped, because two sines at one pitch comb whatever the touch.
    #[test]
    fn a_word_head_common_tone_keeps_the_steps_touch_and_still_damps_the_old_voice() {
        let mut s = synth();
        let mut at = 1_000u32;
        let mut heads_repeated = 0usize;
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
                    let mark = s.born_seq;
                    push_ch(&mut s, SoundKind::Typed, at, ch);
                    at += 100;
                    let lead = tune_voices(&since(&s, mark))[0];
                    if s.v2.walk() != before {
                        prev = ch;
                        continue;
                    }
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
                        assert_eq!(s.v2.restrike, 0, "a word head armed the re-strike ladder");
                        assert!(
                            lead.p[2].lvl > 0.0 && lead.n_lvl == MALLET_LVL,
                            "a word head's common tone lost the step's strike"
                        );
                    } else {
                        assert_eq!(prev, ch, "an in-word repeat that is not a doubled letter");
                        doubles += 1;
                        assert!(s.v2.restrike >= 1);
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
    }

    /// **τ_v PASSES THROUGH §3.1's TWO ANCHORS, AND THE FLOOR BINDS AT
    /// 20 cps** (step-3 review, finding 8): 110 ms at the 4 cps reference,
    /// 28 ms at 20 cps, and the floor — not the intake clamp — decides the
    /// note from there down.
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

    /// **THE BLOOM FADES IN BEHIND EVERY LIT STEP, ON THE LATTICE, AND
    /// FOLLOWS THE HUE** (§3.3 items 1-3).
    ///
    /// **RE-PINNED ON THE PANEL'S Q2/Q5 RULING (2026-09-09).** The hang and
    /// the head are no longer three constants: they are the note's own τ_v
    /// through [`BLOOM_DECAY_TAU_MUL`] and [`BLOOM_HEAD_MIN_MUL`], under
    /// those constants as ceilings. Part (a) is unchanged and is now also
    /// the proof that the change is a NO-OP AT THE DESIGN'S OWN ANCHOR: a
    /// session's first key carries [`IOI_DEFAULT_MS`] 250 ms, which is 4 cps,
    /// which is [`TAU_V_MAX_S`] — so it still reads exactly `BLOOM_DELAY_S`,
    /// `BLOOM_ATTACK_S`, `BLOOM_DECAY_S` and `BLOOM_DUR_S`, bit for bit, and
    /// what the owner has already heard at prose tempo has not moved. Part
    /// (b) types at 9 cps, where it does bind, so it can no longer find the
    /// bloom by its attack; it finds it by its lane and its partials, which
    /// is what a bloom actually is.
    ///
    /// A lit step carries one voice in [`LANE_BLOOM`] at [`BLOOM_DELAY_S`]
    /// with [`BLOOM_ATTACK_S`], its partials on [`BLOOM_DEGREES`] above the
    /// note — 3f / 4f / 6f exactly on C — and nothing else does: not a
    /// passing note, not a re-strike, not a felt key. Its level scales by
    /// [`hue_air`] and its pan drifts by [`BLOOM_SPREAD`]; the note's roof
    /// opens by [`ROOF_HUE_ADD_HZ`] and its gain does not move by a decibel.
    /// With the stops out, none of it exists.
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

        // (b) Over a corpus: a bloom iff the key was a lit STEP with a pitch.
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
            // The answering voice shares the lane; a bloom is the voice
            // with the bloom's own attack.
            // A bloom is the LANE_BLOOM voice whose partials are the bloom's
            // — the answering voice shares the lane but is a tine, and the
            // air taps are blooms held off by their own delay. Its attack is
            // no longer a constant to match on (the Q2 head scaling), and at
            // this rhythm it is half of one.
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
        // Red sits BLOOM_SPREAD/2 to one side of the strike, cyan to the
        // other: the pans differ, and by the spread. (Equal-power law: read
        // the angle back off the gains.)
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

    /// **THE ROOM ANSWERS A LINE'S END AND A REST, AND NOTHING INSIDE A
    /// PHRASE; THE SUBJECT IS ANSWERED FROM ABOVE** (§3.3 item 4, the
    /// answering voice).
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
        // A REST: the key after ≥ PHRASE_PAUSE_MS carries two taps, at the
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

        // THE ANSWER: a subject latched from the first three intervals of a
        // line, answered at the fourth word head — one voice in the bloom
        // lane, ANSWER_DELAY_S behind the head, no mallet, above the head on
        // a lit tone.
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

    /// **THE BLOOMED STEP IS BRIGHTER THAN THE TINE AND UNDER THE GLASS
    /// LINE** (§3.3's falsifiable prediction, §8 step 6). On §9.1's own
    /// probe the bloom lifts the isolated step's centroid, and it stays under
    /// 1600 Hz — past that the bloom has become the glass bell the v2 train
    /// retired, and [`BLOOM_LEVEL`] must give back.
    #[test]
    fn the_bloomed_step_is_brighter_than_the_tine_and_under_the_glass_line() {
        // `a` then `e`, 300 ms apart: a rising third inside the dead band,
        // so the probed key lands on E — lit under the parked IV — and has
        // a bloom to measure. (A rhythm-derived probe lands on D, a passing
        // note, which blooms nothing: the fixture asserts the chord tone.)
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
        // Measured 679 -> 771 Hz at the red end, 952 at cyan (BLOOM_LEVEL 2.0).
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

    // -- A27: determinism -------------------------------------------------

    /// **THE SAME WORD TYPED AT THE SAME SPEED SOUNDS THE SAME** (A27).
    ///
    /// The engine reads no clock (time arrives as `at_ms`), allocates nothing
    /// on the steady path, and draws every "random" quantity from the synth's
    /// own seeded stream — so one script under one seed is one waveform, bit
    /// for bit. This is the property the whole bench plan rests on.
    #[test]
    fn the_same_word_typed_at_the_same_speed_sounds_the_same() {
        let run = || {
            let mut s = synth();
            let mut out = Vec::new();
            for (i, shifted) in [true, false, false, false, false].iter().enumerate() {
                push(
                    &mut s,
                    SoundKind::Typed,
                    1_000 + i as u32 * 120,
                    -0.3 + i as f32 * 0.1,
                    *shifted,
                );
                out.extend(render_mono(&mut s, 6));
            }
            push(&mut s, SoundKind::Space, 1_600, 0.2, false);
            out.extend(render_mono(&mut s, 40));
            out
        };
        let a = run();
        let b = run();
        assert_eq!(a.len(), b.len());
        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
            assert!(
                x.to_bits() == y.to_bits(),
                "sample {i} differed between two runs of one script ({x} vs {y})"
            );
        }
        assert!(a.iter().any(|x| x.abs() > 1e-4), "the script was silent");
    }

    // -- A11: no roughness ------------------------------------------------

    /// **NO PARTIAL PAIR BEATS IN THE 15–60 Hz ROUGHNESS BAND ABOVE 1 kHz**
    /// (§9.5 law 4, A11).
    ///
    /// Two tones whose difference falls in that band are heard as *buzz*
    /// rather than as two notes. On a just lattice harmonic partials either
    /// coincide exactly or sit a consonant interval apart, so the only way to
    /// break the law is an inharmonic partial — and v2 admits exactly one, the
    /// 2.760 strike, under the law's own exemption: **its τ is 40 ms**, and a
    /// 15 Hz beat needs 67 ms for one cycle. The FM voices (the ice bell, the
    /// meteor core) are exempt for the second stated reason: their sidebands
    /// are not lattice pitches by design.
    #[test]
    fn no_partial_pair_beats_in_the_roughness_band_above_one_kilohertz() {
        let mut s = synth();
        let mut worst: Option<(f32, f32)> = None;
        for k in 0..120u32 {
            let at = 1_000 + k * 100;
            if k % 6 == 5 {
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
            // Everything alive at this instant, i.e. everything that can beat
            // against everything else.
            let live: Vec<(f32, f32, bool)> = s
                .voices
                .iter()
                .filter(|v| v.on)
                .flat_map(|v| {
                    v.p.iter()
                        .filter(|p| p.lvl > 0.0)
                        .map(|p| (p.f0, p.decay, p.fm_ratio > 0.0))
                })
                .collect();
            for (i, a) in live.iter().enumerate() {
                for b in &live[i + 1..] {
                    // Exempt: sub-kilohertz pairs (the law is stated above
                    // 1 kHz), short-lived strike partials, and FM voices.
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
    }

    // -- §9.1's isolated measurements (the warm ruling, §22 A/B #17) ------

    /// **THE TINE IS SMALL AND WARM, WHERE v1's BELL WAS HARD AND BRIGHT** —
    /// §9.0's third cause, measured.
    ///
    /// v1's key was a struck-glass bell whose 4f crown out-summed its own
    /// fundamental (0.34 at 4f plus 0.21 at 4.010f — a 5–13 Hz beat), with an
    /// 8f/8.064f accent glint, a 12f top, and a mallet noise band sweeping
    /// 5200 → 180 Hz **with no envelope of its own**, so a 180 Hz Q 0.7 band
    /// rumbled under every note for its full 300 ms. Measured centroid
    /// 2199–3072 Hz: bright reads as *hard*, not as *small*.
    ///
    /// v2's tine is sine-led with per-partial decays — the octave gone in
    /// 55 ms, the strike in 40, the felt mallet by 25 — and this test measures
    /// the difference on ONE probe, same window: the v2 tine's spectral
    /// centre must sit **well below** the v1 bell's. The bell itself is
    /// deleted (§17.3 phase 7), so its half of the probe is the figure it
    /// measured on this exact probe and window on the last tree that still
    /// carried it — [`V1_BELL_PROBE_CENTROID_HZ`] — rather than a live render.
    ///
    /// (§9.1 once stated an isolated target of 1100–1400 Hz on
    /// `typing_voice_ab`'s own probe; that target was RETIRED on 2026-09-05 —
    /// §22 A/B #17, "warm" — and §9.1 now states the measured warm values.
    /// `typing_voice_ab` power-weights over a different window and a whole
    /// prose scenario rather than one note. The number below is this probe's,
    /// on this window, and the comparative assertion is the law: a band copied
    /// across two instruments would be measuring the instrument, not the tine.)
    #[test]
    fn the_tine_is_small_and_warm_not_hard_and_bright() {
        /// v1's glass bell on THIS probe (seed `SEED`, one `Typed` at 0 ms,
        /// `render_mono` over 12 blocks, the 4096-sample magnitude centroid):
        /// measured 3167.122 Hz on 2026-09-06 from the pre-deletion tree
        /// (commit `9f5431d42`, the v1 chain's `event(Typed)` render), the
        /// same run that read the tine at 788.453 Hz.
        const V1_BELL_PROBE_CENTROID_HZ: f32 = 3167.122;
        // 4096 samples = 85 ms: the whole of the strike and the octave, and
        // the first third of the body — the window in which a tine is either
        // small or hard.
        // THE TINE ALONE: this probe compares the strike's own spectrum with
        // v1's bell. §3.3's bloom is a second voice behind it and is pinned
        // by its own test; with it in, this would be measuring the bloom.
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
            (500.0..=1000.0).contains(&v2_c),
            "the v2 tine's centroid is {v2_c:.0} Hz, outside this probe's \
             measured 500-1000 Hz band for a C5 step"
        );
    }

    // -- A8: Backspace ----------------------------------------------------

    /// **BACKSPACE IS UNPITCHED, MUTES, AND REWINDS** (§10.4, §19.2, A8).
    ///
    /// The deletion's sound is the shipped, style-agnostic poof, byte-
    /// unchanged: no tonal partial anywhere in it. Its effect on the MELODY is
    /// the part v1 could not express — declining to advance the tune stops the
    /// song running ahead of the text but cannot put back the note whose
    /// letter just vanished. Type "ab", erase the "b", type it again: the
    /// second "b" plays exactly the note the first one did.
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

    /// **THE BELL AND THE LANDING SHARE ONE CLOCK** (§8.1, §12.1, D9, A16) —
    /// the coupling contract, measured.
    ///
    /// v1's flight ran on `RAINBOW_METEOR_FLIGHT_S` while its audio ran on
    /// `CURSOR_SWEEP_STEP_S`, so the bell and the landing drifted apart with
    /// distance and neither could be retuned without silently breaking the
    /// other. Here the bell's pre-delay is `flight_ms(cells)` **to the f32
    /// bit** — the same number `rainbow_kitty::timing` hands the pixels — at
    /// every distance, and the Enter cadence's three landing voices are all on
    /// that same edge.
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

            // ENTER: resolution C, tonic dyad and faraway bell, all on the
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
            // THE ROOM ANSWERS THE LINE'S END (§3.3 item 4): two bloom taps
            // behind the resolution, on the same edge plus their own
            // spacings — the room is heard after the note, never on it.
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

    /// **DIRECTION LIVES IN THE CORE; THE WHOOSH IS BLIND** (§12.3, A17), and
    /// the gesture travels: the core and the whoosh leave the origin column
    /// and arrive at the destination on the flight's own clock.
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

    /// **`Land` IS SILENT UNDER v2** (§12.3, A20). The meteor's bell *is* the
    /// landing; a second landing voice on a second clock is §9.0's fifth
    /// cause, and v2 simply does not have one.
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

    /// **THE CASCADE NEVER STACKS** (D18, A26). One `Jump` is the four-note
    /// brrrring; twelve `Jump`s at 60 ms are ONE cascade plus quiet top-note
    /// re-strikes, not sixty pitched onsets a second. A keyed `Enter` is the
    /// cadence and mints no cascade at all.
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
        // A26, exactly: 12 line feeds arriving 60 ms apart are ONE cascade
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

        // A KEYED ENTER IS THE CADENCE AND NO CASCADE (A26's last clause):
        // the line feed the PTY echoes for it — inside 150 ms — is swallowed
        // whole, so a Return never plays the cadence AND the brrrring. A
        // line feed later than that is a cascade again.
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

    // -- A6: the pure-fifth loop -----------------------------------------

    /// **SPACE WALKS PURE FIFTHS, AND ENTER COMES HOME** (§10.3, A6).
    ///
    /// Every dyad is an exact 3:2 or 4:3 — the only fifths this lattice can
    /// spell purely — and every voice of it lands inside the BASS lane's
    /// 261.6–436.0 Hz. After an Enter the next Space plays I, so a fresh line
    /// does not open on a minor colour.
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
        // THE ROOTS FOLLOW `CHORD_LOOP` IN ORDER — I vi IV V | I V vi IV —
        // from the parked chord, so the session's first word lands on I.
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

    // -- §16: the deltas are identity-defaulted ---------------------------

    /// **EVERY ENGINE DELTA DEFAULTS TO THE IDENTITY** (§16's identity
    /// column, A25).
    ///
    /// The companion pins are `palettes_render_within_one_16bit_step_of_
    /// v056_reference` and `brrrring_of_rapid_line_feeds_is_pinned`, which
    /// measure the eight other palettes against a frozen v0.56 synth. This
    /// test states the STRUCTURAL reason they still pass: a v1 event builds
    /// voices on which every field §16 added is at zero, so every branch that
    /// reads one is untaken and every pre-v2 f32 expression is evaluated on
    /// its pre-v2 operands. The rainbow kitty look is not in the sweep: it is
    /// the music box, not a v1 event (§17.3 phase 7).
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

    /// **THE LANES HOLD AND NOTHING IS STOLEN** (§14, A23). The caps sum to
    /// 25 of 28 slots, so a lane under its cap can always be admitted and the
    /// global pool never runs dry — `steals()` reports a real mix defect, and
    /// on v2's own worst case it must report none.
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

        // §14's WORST CASE, the one the table was sized for, run as a burst
        // inside one flight: 10 cps typing with capitals, a downbeat, glints,
        // two meteors 40 ms apart, and a keyed Enter with its full cadence —
        // meteor (3 + 3 rain + bell + thump) + Enter (3) + tune + bass +
        // glint, all pre-delayed onto the same 100 ms. The lanes must hold
        // at every block and the pool must never run dry.
        // The audio clock advances with the stamps, as on a host: one 10 ms
        // block per 10 ms of script, the caps checked after every block.
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

    /// §14 at one instant: every lane at or under its cap, counting SOUNDING
    /// voices only — the cap is a polyphony cap, and a pre-delayed voice is a
    /// schedule entry (§14's "5 scheduled, ≤ 3 live") — and no drop-newcomer
    /// lane (glint, bloom) has fade-stolen a SOUNDING voice under the 40 ms
    /// age guard. (A voice damped before its pre-delay ran out never
    /// sounded — "expires unheard" — and is not a steal.)
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
            LANE_SHIFT,
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
                age < 0.0 || age >= LANE_AGE_GUARD_S - 1e-4,
                "lane {} fade-stole a voice {age} s old — under the 40 ms age guard",
                v.lane
            );
        }
    }

    /// **A GLINT IS A LATTICE PITCH IN THE STARDUST LANE, TWINKLING AT ITS
    /// OWN STAR'S RATE** (§13, D12, A21).
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

    /// **A HELD BACKSPACE UN-SINGS ONE NOTE PER REPEAT** (§10.4, A8).
    ///
    /// The 40 ms mute and the melody rewind stand OUTSIDE the 75 ms erase
    /// gate: that gate thins the poof against other poofs and nothing else.
    /// Three letters, three deletions at auto-repeat speed, and the retyped
    /// letter sings the first letter's note from the first letter's playhead
    /// — while the poof itself still speaks once for the run. With the
    /// rewind behind the gate only one deletion would un-sing, and the
    /// retyped letter would resume two notes on.
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
        // THE STATE REWOUND ONE NOTE PER REPEAT — which is the claim, and it
        // is asserted where it lives rather than through a pitch.
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
        // …and the retyped letter still SINGS. Its pitch is derived from the
        // rewound state AND from the hand, so a letter retyped after a
        // different pause is entitled to a different note (R2): this retype
        // is 334 ms after the last deletion where the original was 300 ms
        // after its predecessor. What the undo stack owes is the STATE,
        // asserted above; what R1 owes is a note, asserted here.
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

    /// **THE LIMITER IS TRANSPARENT BELOW ITS THRESHOLD, HOLDS THE CEILING
    /// ABOVE IT, AND IDLE IS EXACT SILENCE** (§9.7, §16 row 10, A24).
    ///
    /// Below −14 dBFS the latched bus renders BIT-IDENTICAL to the unlatched
    /// one — the gain is exactly 1.0, not nearly — at the ladder's −21 dBFS
    /// step AND at A24's own −15 dBFS, one decibel under the threshold, which
    /// is what proves the knee hard rather than merely quiet; `limit` itself
    /// is then pinned sample-exact at the threshold and one part in a
    /// thousand over it. Above it, a meteor at host volume 1.0 (composite
    /// ≈ −8 dBFS unlimited) is held at the 0.2 ceiling within the 0.5 ms
    /// attack's own overshoot, and once its tails are gone the limiter is
    /// back at exactly 1.0 and the bus at exact zero.
    #[test]
    fn the_limiter_is_transparent_below_threshold_and_holds_the_ceiling_above() {
        // BELOW. Same seed, same event; one bus latched, one not.
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

        // BELOW, AT THE LAW'S OWN LEVEL. A24 says a −15 dBFS signal — ONE
        // decibel under the −14 dBFS threshold, not seven — renders
        // bit-identical latched vs not. That is the hard-knee half of §9.7:
        // a soft knee of a few dB passes the ladder-floor probe above and
        // fails this one. Host volume 0.8 puts the NOMINAL tine at twice the
        // ladder floor, ≈ −15 dBFS (A28: −21.0 ± 0.3 dBFS at 0.4) — and this
        // seed's §9.6 velocity draw (+1.1 dB) would put it over the
        // threshold, so the draw is read off a throwaway spawn and divided
        // out of the host volume: the probe is the law's −15 dBFS, whatever
        // the seed.
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
        // Within ±1 dB of −15 dBFS and under the threshold, post soft-clip —
        // −14.3 dBFS here bounds the pre-clip peak under 0.2, so the probe is
        // the law's, not a quieter stand-in.
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

        // THE KNEE, SAMPLE-EXACT. `limit` on a settled follower returns the
        // input's own bits at −15 dBFS, at −(−15 dBFS), and at the threshold
        // itself (the follower's test is strict); one part in a thousand
        // over it the gain moves on THAT sample. No tolerance anywhere: hard
        // knee means no gain change of any kind below, and a change at once
        // above.
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

        // ABOVE. A meteor at host volume 1.0, limited and not.
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
        // A feed-forward limiter with a 0.5 ms attack lets the first
        // half-millisecond of a rising transient through; what it promises
        // is that the SUSTAINED level never exceeds the threshold. So: the
        // samples over the ceiling are the onset's, a few ms of them, where
        // the unlimited meteor rides over it for tens of milliseconds.
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

        // IDLE. Two more seconds: the tails are gone, the bus is exact zero
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

    /// **A PAUSE THE RENDER CLOCK NEVER SAW STILL RESTS THE PHRASE** (§10.1,
    /// §16 row 7).
    ///
    /// With no host stamp the melody reads the synth's own clock, which
    /// advances only while the host renders; a host that pauses its queue
    /// after silence must account the pause through `resume_after`, or every
    /// think-pause reads as ≈ 0.9 s and the 900 ms rest cadence never fires.
    /// Three unstamped keys at 300 ms, a 5 s pause with no render, one more
    /// key: accounted, it RESOLVES the line onto the chord and resets the
    /// IOI; unaccounted, it is merely the fourth key of the same burst.
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

    /// **THE FALLBACK CLOCK WRAPS LIKE A HOST STAMP; IT NEVER SATURATES**
    /// (§16 row 7).
    ///
    /// `u32` milliseconds wrap at 49.7 days. A host stamp wraps and the
    /// melody carries on; a saturating cast would pin every later event at
    /// `u32::MAX` — every gap zero, no step ever again, nothing to heal it.
    /// Past the wrap two unstamped keys 300 ms apart must still be two
    /// steps; and the wrap itself costs exactly what a host wrap costs: the
    /// wrapping key reads a nonsense gap, so the contour may lose that one
    /// interval — but the key still steps and still sounds, the IOI
    /// estimator stays finite, and the line walks on.
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

        // PAST THE WRAP: 204 ms after the 2³² boundary.
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

        // ACROSS THE WRAP: the cost is a wrapped host stamp's, no more.
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

    /// **A METEOR FIZZLES, CLAIMS WITHOUT A CLICK, INTERRUPTS, AND NEVER EATS
    /// A SOUNDED BELL** (§12.3, §15.2, A18).
    ///
    /// An arm with no move behind it damps at its ttl over the 12 ms ramp; an
    /// arm whose echo is a 5-cell hop fizzles under the nav tick; a claim
    /// re-targets the ARMED core — one core, no ttl, the true flight — and
    /// the bus is continuous across the claim instant (a ×4 level step or a
    /// re-seeded envelope would show as a jump in both the boundary sample
    /// and the 1 ms RMS); a second meteor 40 ms after the first damps the
    /// first's sounding layers over 15 ms, cancels its unsounded bell, and
    /// leaves the second's bell to ring.
    #[test]
    fn a_meteor_fizzles_claims_without_a_click_and_never_eats_a_sounded_bell() {
        let mut buf = [0.0f32; 960];

        // 1. AN ARM WITH NO MOVE BEHIND IT damps at its ttl — never a cut.
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

        // 2. AN ARM WHOSE ECHO IS A SUB-FLOOR HOP: the arm fizzles, the nav
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

        // 3. THE CLAIM IS CONTINUOUS.
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
            // the tick, which is over by 4 ms.
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

        // 4. METEOR THEN METEOR AT 40 ms.
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

    /// **A SING-ALONG NEVER MUTES THE TYPIST'S LINE, AND HANDS THE KEY BACK
    /// AT A PHRASE BOUNDARY** (§10.2, A31, R1).
    ///
    /// Under a live riff every key still sounds ITS OWN PITCHED NOTE — the
    /// sing duck is what makes room for the cat, not silence — and the one
    /// thing the riff takes off a key is a doubled letter's tremolo (§3.1's
    /// own spelling, `sing && touch == ReStrike`). When the riff dies the
    /// borrowed `song_key` is NOT snapped — it is held until the line reaches
    /// a word boundary and handed back there, once, before that word's first
    /// note.
    ///
    /// **This replaces the word-head rule the step-3 tree carried**, under
    /// which every non-word-head key went to the mallet for the whole bar a
    /// riff is armed (τ 0.40 s handback on top) — roughly one pitched note
    /// per word, which the owner's ruling calls the melody in letter and not
    /// in spirit. The machine gun A31 is named for was the deleted gate's
    /// re-strike ladder hammering under the riff; a fast hand's derived line
    /// is the same line it plays without the riff, ducked, and a held key is
    /// caught by the auto-repeat detector at any rate.
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
        // FALSE, not true: a key was already typed above to arm the fixture,
        // so the burst opens INSIDE a word.
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

        // The riff dies: the key is HELD, pending the boundary.
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

        // Typing on — WORDS, because the handback waits for a boundary and a
        // word head is the boundary the derived line has. The key changes
        // exactly once, to neutral, and only on a key that opens a word.
        // The keys continue the burst's own clock. `at_ms` is the melody's
        // only time source, so the seconds of rendering that killed the riff
        // do not open a gap in it — and a ≥ 900 ms gap would be a REST, which
        // is a boundary in its own right and would hand the key back before
        // the word head this half of the test is about.
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

    /// **A v1 VOICE AFTER THE MUSIC BOX SNAPS A PENDING KEY; IT DOES NOT HOLD
    /// IT FOR EVER** (§10.2, §16 row 9 — the v1 chain keeps v1's law).
    ///
    /// The v2 latch is sticky and the word-boundary handback is detected on
    /// the v2 path alone, but the roster is read per event: audition "music
    /// box", go back to "marimba", let a riff die, and the borrowed
    /// `song_key` would otherwise stay pinned for the session — no v1 event
    /// ever reaches a v2 phrase boundary. The first v1 TRAIL event snaps it
    /// (v1's own law); a bonk on the same chain does not, so under the music
    /// box a key held for its boundary is still held (A31).
    #[test]
    fn a_v1_voice_after_the_music_box_snaps_a_pending_key_instead_of_holding_it_for_ever() {
        // The music box is reached BY VOICE under a v1 look, the way the
        // settings row auditions it (`event()` carries the rainbow kitty
        // look, which would be the music box by itself).
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

    /// **TAILS END QUIETLY; DAMPS ARE RAMPS** (§9.5 law 2, A13).
    ///
    /// Every laned voice §11 builds has `exp(−dur/decay) ≤ 0.07` — at −23 dB
    /// or below when the engine's 5 ms release ends it — and every damp v2
    /// arms is at least the 12 ms ramp. Gesture by gesture, at the spawn, so
    /// the tick's 4 ms life and the rain's 76 ms are examined alongside the
    /// tine's `3τ + 20`.
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
            LANE_SHIFT,
        ] {
            assert!(seen.contains(&lane), "the tail sweep never saw lane {lane}");
        }
    }

    // -- §9.1: the isolated probe -------------------------------------------

    /// §9.1's ISOLATED PROBE, as `typing_voice_ab`'s family rows measure a
    /// gesture: the ENERGY-weighted centroid over 2048-sample Hann windows at
    /// a 1024 hop, from the gesture's first onset to 200 ms past it.
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

    /// **THE ISOLATED STEP'S CENTROID IS THE ONE §9.1's PARTIAL TABLE BUILDS,
    /// ON §9.1's OWN PROBE** — energy-weighted over the gesture's body the
    /// way `typing_voice_ab` reads a gesture.
    ///
    /// §9.1 once stated an isolated target of 1100–1400 Hz, and the tine's
    /// own table cannot produce it: with energy ∝ lvl²·τ (a partial's own decay
    /// MULTIPLIES the voice envelope, so the octave's effective τ is 37 ms
    /// and the strike's 29), P1 (0.50, τ 110) carries 0.0275, P2 (0.16, τ 55)
    /// 0.0009 and P3 (0.12, τ 40) 0.0004 — the octave and the strike together
    /// are 5 % of the energy — and the felt mallet (0.45, τ 6 ms) another
    /// 0.0012 spread over 900–3200 Hz, which puts the analytic centroid near
    /// 600 Hz and the measured one at ≈ 680 (≈ 670 on §9.1's original
    /// 0.13 / 45 and 0.10 / 35). Reaching 1100 Hz would need P2 at ≈ 0.5 or a
    /// τ ten times longer, i.e. a different instrument from the one the
    /// table describes. This pin is the table's number, 550–800 Hz, so drift
    /// toward mud OR toward glass fails. The contradiction was resolved in
    /// the design of record on 2026-09-05 (§22 A/B #17, "warm is probably
    /// better"): the 1100–1400 line is retired and §9.1 states the measured
    /// warm values, so this band is the design's own, not one the code chases.
    /// (The 85 ms magnitude-weighted window in
    /// `the_tine_is_small_and_warm_not_hard_and_bright` reads the strike and
    /// octave against v1; this one reads the whole body.)
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

    /// **FASTER IS BRIGHTER, NEVER LOUDER** (§9.6, A14).
    ///
    /// Per-second TUNE energy — Σ(gl+gr)² over the pitched spawns of steady
    /// typing — sits inside A14's +1 / −3 dB of the 4 cps reference at 8, 10,
    /// 15 and 20 cps.
    ///
    /// **Every rate is now on one band, and that is the point.** The old
    /// version pinned 8 cps separately, to +0.54 dB, because §9.6's table was
    /// arithmetic over the re-strike ladder: under the 220 ms gate an 8 cps
    /// take was one step and one muted re-strike per 250 ms, so the energy
    /// depended on which keys the gate let through. With every key a full
    /// step (R1), `rate · g²` is flat by construction below the arc's
    /// reference and the whole sweep collapses onto ~0 dB — measured
    /// +0.98 / +0.90 / −0.01 / +0.09 at 8 / 10 / 15 / 20 cps, where the
    /// residual is the take's own lit-versus-passing mix (a passing note is
    /// 2 dB down) and the ±1 dB seeded velocity, not the arc. It is the
    /// reference at [`G_IOI_REF_S`], moved from 0.15 s to 0.25 s, that buys
    /// this; on the old reference the same sweep read +3.2 dB at 8 cps and
    /// broke the law outright.
    ///
    /// The roof, meanwhile, is a non-decreasing function of the rate for
    /// every touch and lighting, and heat moves the roof and never the gain:
    /// the arc buys brightness with speed, never a decibel.
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

    // -- §22: FLOW'S VOICE -------------------------------------------------

    /// **THE BOX OPENS WITH THE HAND AND NEVER GETS LOUDER** (§22, §9.6's
    /// loudness law, §21.4).
    ///
    /// Two halves, and neither is worth anything without the other.
    ///
    /// **IT OPENS.** On one word in isolation the RING-OUT — the 400-1200 ms
    /// window, which contains no onset at all — must rise by at least 2 dB
    /// between heat 0 and heat 1. That is the whole of what "fuller" means
    /// here: the bass dyad's longer decay and the octave echo are both BODY,
    /// and body is what shows up in a window with no strike in it.
    ///
    /// **IT NEVER GETS LOUDER.** Two bounds, on 30 s of the bench's prose
    /// corpus at 4, 8 and 12 cps, cold (heat 0) and flowing (heat 1):
    ///
    /// * §9.6's own law — the flowing take's RMS may not sit more than 1 dB
    ///   over the COLD 4 cps reference, at any rate;
    /// * and the PEAK may not rise at all, at any rate. §9.6 rules that speed
    ///   may buy brightness and never a decibel, and the peak is the quantity
    ///   the whole §9.1 ladder is written in
    ///   (`the_isolated_step_lands_on_the_ladder_floor` fits
    ///   [`KEY_TINE_TRIM`] on exactly it).
    ///
    /// **RE-BAKED 2026-09-09**, on the merge of the sparkle's own delay
    /// ([`KEY_GLINT_DELAY_S`]) with the derived line's brighter strike — the
    /// glint moving off the strike's crest moves every one of these numbers,
    /// because what it stops adding is a phase-random maximum. 30 s of
    /// `BENCH_PROSE`, seed `0x504F4F46`, vol 0.4, hue on its own arc:
    ///
    /// ```text
    ///          cold RMS   flow RMS   Δ     cold peak  flow peak   Δ
    ///  4 cps   -33.09     -33.61   -0.52    -17.21     -17.50   -0.29
    ///  8 cps   -35.30     -35.77   -0.47    -16.72     -17.94   -1.22
    /// 12 cps   -36.45     -36.96   -0.51    -16.63     -17.12   -0.49
    /// ```
    ///
    /// And the BRIGHTNESS the box buys instead, over the same takes (Welch,
    /// 2048-sample Hann frames at a 1024 hop, cold → flowing):
    ///
    /// ```text
    ///          centroid Hz      energy > 2 kHz
    ///  4 cps   1355.8 -> 1422.9   12.32 % -> 16.96 %
    /// 10 cps   1363.8 -> 1390.7   16.51 % -> 19.39 %
    /// ```
    ///
    /// The flowing box is a fraction of a decibel QUIETER than the cold one
    /// at every rate, and that is the design: [`flow_partials`] conserves the
    /// strike's sum, so everything flow adds is body rather than level. The
    /// literal reading of "P2 0.16 → 0.22" — warm the octave and leave the
    /// fundamental alone — was measured first and rejected: it took the prose
    /// PEAK up 0.55 dB at 4 cps and 0.49 at 8, which is a decibel bought with
    /// speed.
    #[test]
    fn the_box_opens_with_the_hand_and_never_gets_louder() {
        // -- it opens ----------------------------------------------------
        let (cold_peak, cold_body) = word_ring_out(0.0);
        let (hot_peak, hot_body) = word_ring_out(1.0);
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
        // AND IT OPENS GRADUALLY. Flow is a LERP, so the ring-out must climb
        // at every rung and the peak must fall at every rung — a box that
        // arrived all at once would be a threshold wearing a lerp's clothes.
        // Measured ring-out: -54.05 / -53.87 / -52.40 / -51.21 / -50.20 dBFS,
        // and the peak falls at EVERY rung with it: -19.72 / -19.83 / -19.87
        // / -19.91 / -19.95. Quarter heat is the rung that decides this
        // clause — the rise saturates early, so a fix that only pays at
        // heat 1 leaves +0.10 dB standing here (measured, with the sparkle
        // back on the strike's crest).
        let mut last = (cold_peak, cold_body);
        for heat in [0.25f32, 0.5, 0.75, 1.0] {
            let rung = word_ring_out(heat);
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
        let rates = [4.0f32, 8.0, 12.0];
        let cold: Vec<(f32, f32)> = rates.iter().map(|c| prose_loudness(*c, 0.0)).collect();
        let hot: Vec<(f32, f32)> = rates.iter().map(|c| prose_loudness(*c, 1.0)).collect();
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
        // §9.6's reference: the COLD take at the arc's own 4 cps.
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
    }

    /// The slack allowed on "the peak did not rise": one twentieth of a
    /// decibel, which is a hundredth of the smallest level difference §9.1's
    /// ladder is written in and two orders under audibility. It is here so
    /// the law reads as "did not rise" rather than as an exact float
    /// comparison over a 1.4 M-sample take; every measured figure is
    /// comfortably NEGATIVE.
    const PEAK_EPS_DB: f32 = 0.05;

    /// **FLOW'S VOICE IS THE IDENTITY AT HEAT ZERO** (§22).
    ///
    /// Flow is a LERP parameter, and the whole safety of shipping it into an
    /// instrument with a baked whole-render golden rests on one claim: at
    /// heat 0 every flow-priced quantity IS its shipped constant, and flow's
    /// own voice does not exist. Not "is inaudible", not "renders at gain 0"
    /// — does not exist, so no slot is claimed, no seeded draw is made and no
    /// sample moves.
    ///
    /// Four clauses, from the constants outwards:
    ///
    /// 1. [`flow_partials`] at 0 is exactly `(P1_LVL, P2_LVL)` — bit equality
    ///    on f32, because `lerp(a, b, 0.0)` must return `a` and not `a` plus
    ///    a rounding error;
    /// 2. the downbeat's decay and duration at heat 0 are exactly
    ///    [`BASS_DECAY_S`] and [`BASS_DUR_S`];
    /// 3. a lit step at heat 0 spawns exactly ONE [`LANE_BLOOM`] voice — the
    ///    bloom — where the same step in flow spawns two;
    /// 4. and a whole 1.5 s render of a word is bit-identical between an
    ///    explicitly-zero side-car and one that never stamped the field,
    ///    while the SAME script in flow is not — so the test cannot pass by
    ///    flow being inert everywhere.
    ///
    /// The archived proof is beside it and not in it: `music_box_golden::
    /// ORACLE_SCRIPT_FOLD` and `BRRRRING_FOLD` were baked before flow
    /// existed, and they are in this change's gate unchanged.
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
        assert_eq!(bass_of(0.0), (BASS_DECAY_S, BASS_DUR_S));
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
}
