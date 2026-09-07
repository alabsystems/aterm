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
//!    NO ghost lane: a key inside the step gate is a SAME-PITCH re-strike at
//!    falling level ([`Touch::ReStrike`]) — a music-box tremolo. Cured in
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
//!    followed finger jitter. v2's verse advances on a [`STEP_GATE_MS`] TIME
//!    gate read off the host's input clock (`EventMeta::at_ms`): at ≤ 4.5 cps
//!    every key sings, at 10 cps the verse steps every third key, and "a third
//!    of typing speed" stops being a rule and becomes a property of the clock.
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
    ERASE_MIN_GAP, EventMeta, HELD_ERASE_RUN_WINDOW, PAN_LAW_SCALE, Palette, Partial, SONG_FORM,
    SONG_THEME, SoundEvent, SoundGesture, SoundKind, TrailSynth, Voice, pan_gains, penta,
};
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
/// The capital echo and the `?`/`!` grafts. Cap 2.
pub(super) const LANE_ECHO: u8 = 2;
/// The word downbeat's dyad, the meteor's thump, the cadence's tonic dyad.
/// Cap 1 — the bass is monophonic by law (§9.3).
pub(super) const LANE_BASS: u8 = 3;
/// The space's breath. Cap 1.
pub(super) const LANE_BREATH: u8 = 4;
/// Stardust glints. Cap 3, and the only lane besides ECHO that drops an
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
/// **The arithmetic, so nobody re-derives it wrong:** TUNE 4 + ECHO 2 + BASS
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
        LANE_ECHO => 2,
        LANE_GLINT | LANE_METEOR | LANE_RAIN => 3,
        // BASS, BREATH, CADENCE, SHIFT — and anything unnamed, which cannot
        // occur but must not silently become unbounded.
        _ => 1,
    }
}

/// WHO LOSES when a full lane's oldest voice is younger than
/// [`LANE_AGE_GUARD_S`]: `true` = drop the newcomer, `false` = steal anyway.
///
/// §14 states it as a hierarchy of consequences. A missing GLINT or ECHO is a
/// decoration that did not happen — inaudible as an absence. A missing TUNE
/// voice is **a key that made no sound**, which reads as a dropped keystroke;
/// that is worse than a clipped tail, so the tune always speaks.
pub(super) fn lane_drops_the_newcomer(lane: u8) -> bool {
    matches!(lane, LANE_GLINT | LANE_ECHO)
}

// ===========================================================================
// §10.2 / §10.4 — the melody's clocks
// ===========================================================================

/// **THE STEP GATE**, milliseconds — the single number that turns v1's
/// keystroke-indexed bar into a meter (§9.0 cause 4).
///
/// The playhead advances at most once per 220 ms, measured from the last
/// STEP and not from the last key. Everything the owner asked for falls out of
/// that one clock instead of needing three rules:
/// - at ≤ 4.5 cps every key is a step (the "slow typing promotes every key"
///   law, with no separate rule);
/// - at 5 cps the verse steps every second key;
/// - at 10 cps every third — v1's "a third of typing speed", now a property of
///   the clock rather than of the finger.
///
/// 220 ms is ~2.3 notes/s at the low end and holds the verse near 4 notes/s
/// under a fast hand: a music box's tempo, not a typist's.
pub(super) const STEP_GATE_MS: u32 = 220;

/// A typing gap this long ends the phrase and CADENCES it (§10.2).
///
/// **900, not v1's 600.** v1's 600 ms was tuned against a governor decay that
/// §16 row 11 sets to exactly 1.0 for v2; at 600 ms every ordinary think-pause
/// cadences, which skips a phrase of the form each time and means a session
/// never plays the piece through. 900 ms is longer than a word-finding pause
/// and shorter than a real stop.
pub(super) const PHRASE_PAUSE_MS: u32 = 900;

/// Re-strikes closer together than this are COALESCED — the state still
/// advances, but no second voice is spawned (§10.2).
///
/// 60 ms is chosen against the roughness band, not against taste: a 30 Hz
/// auto-repeat becomes a ≤ 16.7 Hz roll, which is under the 15–60 Hz band
/// where amplitude modulation is heard as buzz (§9.5 law 4). **Steps are never
/// thinned** — only re-strikes are — so a held key can never silence the tune.
pub(super) const RESTRIKE_COALESCE_MS: u32 = 60;

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

/// THE FELT MALLET — band-passed noise, 3200 → 900 Hz, the ear's onset
/// time-stamp and the whole of what makes the tine read as *struck*.
///
/// v1's mallet swept 5200 → 180 Hz with **no envelope of its own** (§9.0 cause
/// 3): a 180 Hz Q 0.7 band sat ≈ 9 dB under every bell for its full 300 ms,
/// rumbling, and paid a `tanf` per sample for the privilege. v2's is over in
/// 25 ms — a ≈ 50× cut in that cost per key, and the low rumble simply does
/// not exist.
const MALLET_HZ0: f32 = 3200.0;
const MALLET_HZ1: f32 = 900.0;
const MALLET_GLIDE_S: f32 = 0.005;
const MALLET_Q: f32 = 0.7;
const MALLET_LVL: f32 = 0.45;
/// The mallet's own decay (§16 delta 2) — 6 ms, so it is ≈ −36 dB by 25 ms.
const MALLET_TAU_S: f32 = 0.006;

/// τ_v — the voice decay, adaptive to the inter-onset interval (§9.1):
/// `clamp(110·(0.35 + 2.6·IOI_s), 55, 110)` ms.
///
/// This is the masking law, not a taste dial. At 10 cps the keys are 100 ms
/// apart; a 110 ms note would overlap its successor and the pitches would pile
/// into a chord instead of a line. τ_v falls to 67 ms there and to the 55 ms
/// floor above ~13 cps, so **fast typing thins the notes rather than the
/// note count** — every key still speaks (A15).
const TAU_V_BASE_S: f32 = 0.110;
const TAU_V_OFFSET: f32 = 0.35;
const TAU_V_SLOPE: f32 = 2.6;
const TAU_V_MIN_S: f32 = 0.055;
const TAU_V_MAX_S: f32 = 0.110;

/// THE ROOF — a one-pole lowpass, and §9.6's whole "brighter when fast, never
/// louder" mechanism. Plain roof lerps 4200 → 5200 Hz over 4 → 12 cps; a
/// chord-tone step opens it by [`ROOF_LIT_ADD`]; the glow's blaze adds up to
/// [`ROOF_HEAT_HZ`] and **never a decibel**.
const ROOF_PLAIN_LO_HZ: f32 = 4200.0;
const ROOF_PLAIN_HI_HZ: f32 = 5200.0;
const ROOF_LIT_ADD_HZ: f32 = 2300.0;
const ROOF_MAX_HZ: f32 = 7500.0;
const ROOF_HEAT_HZ: f32 = 600.0;
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

/// §9.2's echo touch — the capital's octave, 25 ms behind its own note.
const ECHO_P2_LVL: f32 = 0.08;
const ECHO_TAU_MUL: f32 = 0.55;
const ECHO_DELAY_S: f32 = 0.025;
/// −8 dB re the step (§11).
const ECHO_LEVEL: f32 = 0.398_107_2;
/// The echo's interval, as a ratio. `[2]` — the octave the owner's ruling asks
/// for; §10.4's rotating `[2, 3, 4]` is an A/B, not the default.
const ECHO_RATIO: [f32; 1] = [2.0];
/// The echo is octave-folded down to sit inside the LIT lane (§9.4).
const ECHO_FOLD_MAX_HZ: f32 = 3200.0;

/// A PASSING (non-chord-tone) step is 2 dB under a lit one (§9.2). The chord
/// lights the verse; it never moves it (§10.3).
const PASSING_LEVEL: f32 = 0.794_328_2;

/// §9.6's loudness arc: `g_IOI = clamp(√(IOI_s / 0.15), 0.6, 1.0)`.
///
/// The arc REPLACES v1's flood governor (§16 row 11). v1 ducked every typed
/// voice by `1/√(1 + 0.55·rate)` — −6.5 dB at 10 cps — while Jump/Sweep/Land
/// bypassed it entirely, so the tune was the quietest layer in its own mix
/// (§9.0 cause 5). The arc holds per-second energy flat to within +1 dB of the
/// 4 cps reference (A14) *without* making the melody the thing that gives way.
const G_IOI_REF_S: f32 = 0.15;
const G_IOI_MIN: f32 = 0.6;

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

/// THE LIFT'S ROTATION (§10.4; owner 2026-08-31, "the tone should rotate").
/// A bare Shift plays `walk + {1, 3, 5, 2, 4}` in turn — it never claims the
/// beat and never moves the playhead, so how you reach for a capital cannot
/// change the tune.
const SHIFT_LIFT: [i32; 5] = [1, 3, 5, 2, 4];
const LIFT_ATTACK_S: f32 = 0.004;
const LIFT_DECAY_S: f32 = 0.040;
/// §11's 80 ms, raised to the tail law: [`TAIL_DUR_PER_TAU`] × 40.
const LIFT_DUR_S: f32 = TAIL_DUR_PER_TAU * LIFT_DECAY_S;
/// −9 dB re the step (§11) — a modifier is intent, not authorship.
const LIFT_LEVEL: f32 = 0.354_813_4;
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
// §10.1 — MelodyV2: the state
// ===========================================================================

/// WHICH OF THE TINE'S THREE TOUCHES a key gets (§9.2).
///
/// The whole of §9.0's first cure is that these are the ONLY three, and that
/// two of them are the same pitch: a key inside the step gate does not get a
/// different note, it gets the same note again, softer. v1's fourth "touch" —
/// the ghost, an octave or a fourth or a third away on the identical bell — is
/// not carried, and cannot be: nothing in this module can voice a degree the
/// verse did not step to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Touch {
    /// The verse advanced. Full tine: body, octave, strike, mallet.
    Step,
    /// A key arrived inside the gate. The tine with the strike partial at
    /// zero, at `L_n` — a music-box tremolo, never a leap.
    ReStrike,
    /// A capital's octave, 25 ms behind its own note.
    Echo,
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
    theme_pos: u8,
    phrase_idx: u8,
    walk: i8,
    restrike: u8,
    chord: u8,
    word_pos: u8,
    /// The step clock rides the stack too: rewinding the playhead without
    /// rewinding its gate would let the letter you retype be a re-strike of a
    /// note that is no longer sounding.
    last_step_ms: u32,
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
    /// Playhead into [`SONG_THEME`], 0..28.
    theme_pos: u8,
    /// Which phrase of [`SONG_FORM`] the playhead sits in, 0..4.
    phrase_idx: u8,
    /// THE SOUNDING VERSE DEGREE, 0..8 (C5..G6). Every pitched thing in v2
    /// that is not a bass root is stated relative to this.
    walk: i8,
    /// `at_ms` of the last verse STEP — the step gate's own clock.
    last_step_ms: u32,
    /// `at_ms` of the last admitted key (any touch) — the IOI's clock.
    last_key_ms: u32,
    /// `at_ms` of the last TUNE onset that actually spawned a voice — the
    /// coalescing clock ([`RESTRIKE_COALESCE_MS`]).
    last_onset_ms: u32,
    /// Keys since the last step, 1-based inside [`RESTRIKE_L0`]'s ladder.
    restrike: u8,
    /// Inter-onset interval, EMA'd, clamped 30..600 ms.
    ioi_ms: f32,
    /// Position in [`CHORD_LOOP`], advanced by Space run heads only.
    chord: u8,
    /// Letters since the last word boundary. Read by the `?`/`!` grafts and
    /// carried on the undo stack so a correction restores the word too.
    word_pos: u8,
    /// True while inside a whitespace RUN: only its head is a downbeat.
    space_run: bool,
    /// Position in [`SHIFT_LIFT`].
    shift_step: u8,
    /// Position in [`ECHO_RATIO`].
    echo_k: u8,
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
    /// A fresh melody: the theme at its first note, the loop on I, no history.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            theme_pos: 0,
            phrase_idx: 0,
            // The hook's own tonic, so a session's first key opens on home
            // even before the first step has run.
            walk: 0,
            last_step_ms: 0,
            last_key_ms: 0,
            last_onset_ms: 0,
            restrike: 0,
            ioi_ms: IOI_DEFAULT_MS,
            // Parked where a keyed Enter parks it, so the session's FIRST
            // word boundary lands on I exactly as every later line's does
            // (§10.3: "without that, every line's first bass is Am" — and a
            // session's first line is a line).
            chord: CHORD_AFTER_ENTER,
            word_pos: 0,
            space_run: false,
            shift_step: 0,
            echo_k: 0,
            glint_k: 0,
            cascade_at: 0,
            seen_jump: false,
            cascade_restrike_ms: 0,
            keys_since_enter: 0,
            last_enter_ms: 0,
            seen_enter: false,
            seen_key: false,
            undo: [Undo {
                theme_pos: 0,
                phrase_idx: 0,
                walk: 0,
                restrike: 0,
                chord: 0,
                word_pos: 0,
                last_step_ms: 0,
            }; UNDO_N],
            undo_len: 0,
            lead: None,
            bass: None,
            bell: None,
            thump: None,
        }
    }

    /// THE SOUNDING VERSE DEGREE (test / introspection hook).
    #[must_use]
    pub fn walk(&self) -> i8 {
        self.walk
    }

    /// THE PLAYHEAD (test / introspection hook).
    #[must_use]
    pub fn theme_pos(&self) -> u8 {
        self.theme_pos
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

    /// The smoothed inter-onset interval in ms (test / introspection hook).
    #[must_use]
    pub fn ioi_ms(&self) -> f32 {
        self.ioi_ms
    }

    fn phrase_start(&self) -> usize {
        usize::from(SONG_FORM[usize::from(self.phrase_idx)])
    }

    fn phrase_end(&self) -> usize {
        usize::from(SONG_FORM[usize::from(self.phrase_idx) + 1])
    }

    fn at_phrase_start(&self) -> bool {
        usize::from(self.theme_pos) == self.phrase_start()
    }

    /// Hand the playhead to the NEXT phrase of the form — never a rewind to
    /// zero. That is the whole difference between a theme and a ringtone, and
    /// it is v1's rule kept verbatim.
    fn open_next_phrase(&mut self) {
        self.phrase_idx = (self.phrase_idx + 1) % (SONG_FORM.len() as u8 - 1);
        self.theme_pos = SONG_FORM[usize::from(self.phrase_idx)];
    }

    fn push_undo(&mut self) {
        let frame = Undo {
            theme_pos: self.theme_pos,
            phrase_idx: self.phrase_idx,
            walk: self.walk,
            restrike: self.restrike,
            chord: self.chord,
            word_pos: self.word_pos,
            last_step_ms: self.last_step_ms,
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
        self.theme_pos = f.theme_pos;
        self.phrase_idx = f.phrase_idx;
        self.walk = f.walk;
        self.restrike = f.restrike;
        self.chord = f.chord;
        self.word_pos = f.word_pos;
        self.last_step_ms = f.last_step_ms;
    }

    /// The verse STEPS: the playhead's note becomes the sounding degree and
    /// the gate's clock restarts.
    fn step(&mut self, at: u32) {
        self.last_step_ms = at;
        self.restrike = 0;
    }

    /// Is the sounding degree a chord tone of the live chord (§10.3)?
    fn lit(&self) -> bool {
        let pc = i32::from(self.walk).rem_euclid(5) as u8;
        CHORD_LOOP[usize::from(self.chord)].lit & (1 << pc) != 0
    }

    /// THE REST CADENCE'S DEGREE (§10.2), made exact for A2's in-word law.
    ///
    /// §10.2 closes a paused phrase onto its final degree — "the note the
    /// theme was heading for". Between words that is the whole cadence and
    /// it is kept verbatim. INSIDE a word it can be the one leap the
    /// instrument otherwise cannot make: a pause after the hook's first note
    /// (C5, `word_pos` 1) would close onto the hook's last (E6) — a tenth,
    /// and exactly the octave-class jump §9.0's first cause is about, on the
    /// one key where a listener is still inside the word. So a mid-word
    /// cadence may reach at most [`WORD_LEAP_MAX_DEG`] degrees — a sixth —
    /// from the sounding note: the final degree when it lies within that,
    /// else the CHORD TONE nearest to it inside the sixth (the live chord's
    /// own note, so the cadence is still a cadence). The playhead is
    /// untouched either way: the next phrase opens where §10.2 says.
    ///
    /// Total: a sixth is four consecutive pitch classes and every chord of
    /// [`CHORD_LOOP`] lights three of five, so the search always lands; the
    /// clamp below is the unreachable guard that keeps the function total on
    /// the TUNE register.
    fn cadence_degree(&self) -> i8 {
        let target = i32::from(SONG_THEME[self.phrase_end() - 1]);
        let from = i32::from(self.walk);
        let leap = target - from;
        if self.word_pos == 0 || leap.abs() <= WORD_LEAP_MAX_DEG {
            return target as i8;
        }
        let dir = leap.signum();
        let lit = CHORD_LOOP[usize::from(self.chord)].lit;
        for k in (1..=WORD_LEAP_MAX_DEG).rev() {
            let d = from + dir * k;
            if (TUNE_DEG_LO..=TUNE_DEG_HI).contains(&d) && lit & (1 << d.rem_euclid(5)) != 0 {
                return d as i8;
            }
        }
        (from + dir * WORD_LEAP_MAX_DEG).clamp(TUNE_DEG_LO, TUNE_DEG_HI) as i8
    }
}

/// THE WIDEST INTERVAL A WORD MAY CARRY, in lattice degrees: four — a major
/// sixth (5/3). Five is the octave, and an octave-class leap between two
/// keys of one word is the defect this instrument exists to cure (§9.0
/// cause 1, A2). The theme's own steps never exceed three (a fifth); the only
/// other in-word move the engine can make is the rest cadence, which
/// [`MelodyV2::cadence_degree`] folds to this.
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
    /// Whether a voice is spawned at all — a re-strike inside
    /// [`RESTRIKE_COALESCE_MS`] advances the state and stays silent.
    speaks: bool,
    /// Under a live sing-along a re-strike drops to the mallet alone (§10.2),
    /// so a held key cannot machine-gun under the 150 BPM riff.
    mallet_only: bool,
}

impl MelodyV2 {
    /// §10.2's `on Typed`, entire. Advances the state and returns what to
    /// play; it spawns nothing itself, so the whole melody law is testable
    /// without a synth.
    fn on_typed(&mut self, at: u32, sing: bool) -> TypedPlan {
        let gap = at.saturating_sub(self.last_key_ms);
        self.ioi_ms = if self.seen_key && gap < IOI_RESET_MS {
            (1.0 - IOI_EMA_ALPHA) * self.ioi_ms
                + IOI_EMA_ALPHA * (gap as f32).clamp(IOI_MIN_MS, IOI_MAX_MS)
        } else {
            IOI_DEFAULT_MS
        };
        self.push_undo();

        // A REST CADENCES THE PHRASE. A think-pause closes the phrase you are
        // in onto its own final degree — the note the theme was heading for —
        // and the next key opens the phrase after it. Not applicable at a
        // phrase's very first note: there is no phrase to close, and closing
        // one would skip a phrase per pause.
        let rest = self.seen_key && gap >= PHRASE_PAUSE_MS && !self.at_phrase_start();
        let touch = if rest {
            self.walk = self.cadence_degree();
            self.open_next_phrase();
            self.step(at);
            Touch::Step
        } else if !self.seen_key || at.saturating_sub(self.last_step_ms) >= STEP_GATE_MS {
            self.walk = SONG_THEME[usize::from(self.theme_pos)];
            self.theme_pos += 1;
            if self.theme_pos == SONG_FORM[usize::from(self.phrase_idx) + 1] {
                self.open_next_phrase();
            }
            self.step(at);
            Touch::Step
        } else {
            self.restrike = self.restrike.saturating_add(1);
            Touch::ReStrike
        };

        let lit = self.lit();
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
        // STEPS ARE NEVER THINNED (A15); only re-strikes coalesce.
        let speaks =
            touch == Touch::Step || at.saturating_sub(self.last_onset_ms) >= RESTRIKE_COALESCE_MS;
        // A SING-ALONG ADMITS STEPS ONLY (§10.2, A31).
        let mallet_only = sing && touch == Touch::ReStrike;
        if speaks {
            self.last_onset_ms = at;
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
            speaks,
            mallet_only,
        }
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

    /// §10.4's `on Enter`. Closes the phrase, parks the loop on
    /// [`CHORD_AFTER_ENTER`] and clears the undo stack — you cannot un-sing
    /// across a line break.
    ///
    /// Returns whether the full cadence is earned (§11: ≥ 4 keys since the
    /// last Enter) or whether this Return is a bare tonic dyad.
    fn on_enter(&mut self, at: u32) -> bool {
        if !self.at_phrase_start() {
            self.walk = SONG_THEME[self.phrase_end() - 1];
            self.open_next_phrase();
        }
        let full = self.keys_since_enter >= ENTER_PICKUP_MIN_KEYS;
        self.chord = CHORD_AFTER_ENTER;
        self.word_pos = 0;
        self.restrike = 0;
        self.last_step_ms = at;
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
        if self.at_phrase_start() {
            self.walk = SONG_THEME[usize::from(self.theme_pos)];
            self.theme_pos += 1;
            if self.theme_pos == SONG_FORM[usize::from(self.phrase_idx) + 1] {
                self.open_next_phrase();
            }
        } else {
            self.walk = SONG_THEME[self.phrase_end() - 1];
            self.open_next_phrase();
        }
        self.step(at);
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

    /// §10.4's `on Shift`: the next lift degree, folded into the TUNE lane.
    /// **Never claims the beat, never moves the playhead.**
    fn on_shift(&mut self) -> i32 {
        let deg = super::fold_register(
            i32::from(self.walk) + SHIFT_LIFT[usize::from(self.shift_step)],
            TUNE_DEG_LO,
            TUNE_DEG_HI,
        );
        self.shift_step = (self.shift_step + 1) % SHIFT_LIFT.len() as u8;
        deg
    }

    /// The next glint's degree above the verse note (§13), rotating.
    fn next_glint_deg(&mut self) -> i32 {
        let deg = i32::from(self.walk) + GLINT_DEGREES[usize::from(self.glint_k)];
        self.glint_k = (self.glint_k + 1) % GLINT_DEGREES.len() as u8;
        deg
    }

    /// The next capital echo's ratio (§10.4), rotating.
    fn next_echo_ratio(&mut self) -> f32 {
        let r = ECHO_RATIO[usize::from(self.echo_k)];
        self.echo_k = (self.echo_k + 1) % ECHO_RATIO.len() as u8;
        r
    }
}

// ===========================================================================
// The instrument's arithmetic — §9.1's curves, stated once
// ===========================================================================

/// τ_v from the smoothed inter-onset interval (§9.1). See [`TAU_V_BASE_S`] for
/// why this is the masking law and not a taste dial.
fn tau_v_s(ioi_s: f32) -> f32 {
    (TAU_V_BASE_S * (TAU_V_OFFSET + TAU_V_SLOPE * ioi_s)).clamp(TAU_V_MIN_S, TAU_V_MAX_S)
}

/// §9.6's loudness arc, `g_IOI = clamp(√(IOI_s / 0.15), 0.6, 1.0)`.
fn g_ioi(ioi_s: f32) -> f32 {
    (ioi_s / G_IOI_REF_S).sqrt().clamp(G_IOI_MIN, 1.0)
}

/// §9.6's brightness law: the roof rises with rate, opens on a chord tone,
/// takes up to [`ROOF_HEAT_HZ`] from the glow's blaze — and never, at any
/// point, a decibel.
fn roof_hz(cps: f32, lit: bool, heat: f32, touch: Touch) -> f32 {
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
    (base + ROOF_HEAT_HZ * heat.clamp(0.0, 1.0)).min(ROOF_MAX_HZ)
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

/// OCTAVE-FOLD `f` down until it is at or below `max` — the capital echo's
/// register guard (§10.4: "folded ≤ 3200 Hz").
fn fold_below(f: f32, max: f32) -> f32 {
    let mut f = f.clamp(1.0, 40_000.0);
    for _ in 0..24 {
        if f > max {
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
fn tine(f: f32, touch: Touch, tau: f32, roof: f32, mallet_only: bool) -> Voice {
    let (p1, p2, p3, mallet) = match touch {
        Touch::Step => (P1_LVL, P2_LVL, P3_LVL, MALLET_LVL),
        Touch::ReStrike => (P1_LVL, RESTRIKE_P2_LVL, 0.0, RESTRIKE_MALLET_LVL),
        Touch::Echo => (P1_LVL, ECHO_P2_LVL, 0.0, 0.0),
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

// ===========================================================================
// §16 row 9 — the v2 admission path on `TrailSynth`
// ===========================================================================

impl TrailSynth {
    /// THE MELODY'S CLOCK. The host's stamp where there is one; the synth's
    /// own block clock where there is not (§16 row 7's identity default).
    ///
    /// The fallback WRAPS like a host stamp; it never saturates. `f64 → u64`
    /// (that cast's saturation is 584 million years out) then `u64 → u32`,
    /// which truncates — reduces modulo 2³² — exactly as the host's own u32
    /// millisecond counter does. A saturating `f64 → u32` cast would pin
    /// every event after 49.7 days at `u32::MAX`: every gap zero, forever —
    /// no steps, no rests, no IOI reset — with nothing to heal it. A wrap
    /// costs what a wrapped host stamp costs and heals the same way: the
    /// wrapping key reads a zero gap (a re-strike), the step gate stays shut
    /// while `last_step_ms` sits on the far side of the wrap, and the next
    /// rest cadence or Enter re-anchors it.
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

    /// §10.2's KEY HANDBACK: when the sing-along has ended and the verse has
    /// reached a phrase boundary, the borrowed `song_key` goes back to the
    /// neutral lattice — here, BEFORE the phrase's first note is voiced, and
    /// never mid-phrase (A31). Called at the top of every event that can
    /// open a phrase.
    fn v2_hand_back_key(&mut self) {
        if self.v2_key_pending && self.v2.at_phrase_start() {
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
    /// thinning (the step gate and the per-lane caps are v2's rate law),
    /// `advance_song` (v2 has no bar of ghosts) or `design_trail`'s palette
    /// dispatch (v2 designs its own voices). What it DOES keep is every piece
    /// of v1 machinery v2 explicitly reuses byte-unchanged: the bed's style /
    /// tone latch, the rate estimate other sources read, the erase gate, and
    /// the four terminal style-agnostic designers behind
    /// [`TrailSynth::design_trail`] (poof, word poof, swoosh, cloud) — all of
    /// which return BEFORE palette dispatch, which is exactly why they can be
    /// reused rather than copied.
    pub(super) fn push_v2(&mut self, ev: SoundEvent, meta: EventMeta) {
        let SoundGesture::Trail(kind) = ev.kind else {
            return;
        };
        // THE FIRST v2 TRAIL EVENT LATCHES THE BUS (§9.7, §16 row 10): the
        // limiter is armed from here on, and the sing-along's key is handed
        // back at phrase boundaries rather than snapped (§10.2).
        self.v2_latched = true;
        // Style / tone follow the trail stream unconditionally, as in v1: a
        // bed re-enabled mid-stream must wake in the current constitution.
        self.bed_style = ev.style;
        self.bed_voice = ev.voice;
        self.tone = ev.tone;
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
                self.v2_typed(&ev, at);
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
    /// **TYPED** — the step or the re-strike, plus a capital's echo (§9.2,
    /// §10.2, §11's first three rows).
    fn v2_typed(&mut self, ev: &SoundEvent, at: u32) {
        self.v2_hand_back_key();
        let sing = self.sing > 0.0;
        let plan = self.v2.on_typed(at, sing);
        if !plan.speaks {
            // Coalesced: the state advanced, the ear is spared. A 30 Hz
            // auto-repeat becomes a ≤ 16.7 Hz roll, under the roughness band.
            return;
        }
        let ioi_s = self.v2.ioi_ms * 0.001;
        let cps = 1.0 / ioi_s;
        // A plan is a step or a re-strike; the echo is built below, off the
        // same key, and is not a touch the melody can hand out.
        let tau_mul = if plan.touch == Touch::Step {
            1.0
        } else {
            RESTRIKE_TAU_MUL
        };
        let tau = tau_v_s(ioi_s) * tau_mul;
        // A WORD-HEAD RE-STRIKE STILL GETS THE LIT ROOF (§10.2). The word
        // boundary is heard in the harmony, not forced into the playhead —
        // v1's word-head re-bar is not carried — but the first letter of a
        // word may still open.
        let word_head = self.v2.word_pos == 1;
        // A CAPITAL OPENS THE ROOF (§10.4: "the verse note plays normally
        // (lit roof)"). It is the one place spelling touches the sound at
        // all, and it touches only brightness: the walk is never disturbed.
        let lit_roof = ev.shifted || (plan.lit && (plan.touch == Touch::Step || word_head));
        let roof = roof_hz(cps, lit_roof, ev.heat, plan.touch);
        let deg = plan.deg + i32::from(self.song_key);
        let f = penta(TINE_BASE_HZ, deg);
        if plan.touch == Touch::ReStrike {
            // §9.5 law 5: a same-pitch re-strike damps the old voice first.
            // Two independently phased voices at ONE frequency are a comb
            // filter, which is the artefact the space damp exists to prevent.
            self.v2_damp(self.v2.lead, LANE_FADE_STEAL_S);
        }
        let vel_db = if plan.touch == Touch::Step {
            VEL_DB_STEP
        } else {
            VEL_DB_RESTRIKE
        };
        let vel = self.v2_velocity(vel_db);
        let pan = self.v2_pan(ev.pan);
        let gain = ev.gain * KEY_TINE_TRIM * plan.level * g_ioi(ioi_s) * vel;
        let voice = tine(f, plan.touch, tau, roof, plan.mallet_only);
        self.v2.lead = self.v2_spawn(voice, gain, pan);

        // THE CAPITAL'S ECHO (§11): the note, then its octave 25 ms later at
        // −8 dB in the LIT lane. It never touches `walk`, so how you spell a
        // word cannot move the theme.
        if ev.shifted {
            let ratio = self.v2.next_echo_ratio();
            let echo_f = fold_below(f * ratio, ECHO_FOLD_MAX_HZ);
            let echo_roof = roof_hz(cps, true, ev.heat, Touch::Echo);
            let mut voice = tine(
                echo_f,
                Touch::Echo,
                tau_v_s(ioi_s) * ECHO_TAU_MUL,
                echo_roof,
                false,
            );
            voice.delay = ECHO_DELAY_S;
            voice.lane = LANE_ECHO;
            let vel = self.v2_velocity(VEL_DB_RESTRIKE);
            let gain = ev.gain * KEY_TINE_TRIM * ECHO_LEVEL * g_ioi(ioi_s) * vel;
            // §9.5 law 5: a second capital inside the gate echoes the SAME
            // octave; the previous echo is damped first.
            self.v2_damp_same_pitch(LANE_ECHO, echo_f);
            self.v2_spawn(voice, gain, pan);
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
            let voice = Voice {
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

    /// **SHIFT** — the bare modifier's lift (§10.4, §11). One sine, one
    /// rotation step, no mallet, no beat claimed, no playhead moved.
    fn v2_shift(&mut self, ev: &SoundEvent) {
        let deg = self.v2.on_shift() + i32::from(self.song_key);
        let voice = Voice {
            dur: LIFT_DUR_S,
            attack: LIFT_ATTACK_S,
            decay: LIFT_DECAY_S,
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
            lane: LANE_SHIFT,
            ..Voice::default()
        };
        let pan = self.v2_pan(ev.pan);
        self.v2_spawn(voice, ev.gain * KEY_TINE_TRIM * LIFT_LEVEL, pan);
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
        self.v2_hand_back_key();
        let Some(head) = self.v2.on_jump(at) else {
            return;
        };
        let ioi_s = self.v2.ioi_ms * 0.001;
        let tau = tau_v_s(ioi_s);
        let cps = 1.0 / ioi_s;
        let base = i32::from(self.v2.walk) + i32::from(self.song_key);
        if head {
            for k in 0..CASCADE_DEGREES.len() {
                let f = penta(TINE_BASE_HZ, base + CASCADE_DEGREES[k]);
                let mut voice = tine(
                    f,
                    Touch::Step,
                    tau,
                    roof_hz(cps, true, ev.heat, Touch::Step),
                    false,
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
                roof_hz(cps, false, ev.heat, Touch::ReStrike),
                false,
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
        self.v2_hand_back_key();
        let full = self.v2.on_enter(at);
        // §10.4 reads `walk` AFTER the phrase has been cadenced, so the
        // resolution answers the note the theme was heading for and not the
        // note the last keystroke happened to leave behind.
        let walk = self.v2.walk;
        let t = timing::flight_ms(f32::from(cells)) * 0.001;
        let ioi_s = self.v2.ioi_ms * 0.001;
        let tau = tau_v_s(ioi_s);
        let roof = roof_hz(1.0 / ioi_s, true, ev.heat, Touch::Step);
        let key = i32::from(self.song_key);
        if full {
            // THE PICKUP, at t = 0 — the one cadence voice that leads.
            let mut pickup = tine(
                penta(TINE_BASE_HZ, CAD_PICKUP_DEG + key),
                Touch::Step,
                tau,
                roof,
                false,
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

    /// **STARDUST** — one hero star, one token, one glint (§13).
    ///
    /// A single sine on a lattice degree, octave-folded into the STARDUST lane
    /// so every star is the same light whatever note threw it, twinkling at
    /// **the star's own scintillation rate** — what you see winking and what
    /// you hear winking are one number (D12).
    fn v2_glint(&mut self, ev: &SoundEvent, twinkle_hz: u8) {
        let deg = self.v2.next_glint_deg() + i32::from(self.song_key);
        let f = fold_into(penta(TINE_BASE_HZ, deg), GLINT_LO_HZ, GLINT_HI_HZ);
        let rate = if twinkle_hz == 0 {
            GLINT_TWINKLE_DEFAULT_HZ
        } else {
            f32::from(twinkle_hz)
        };
        let voice = Voice {
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
        let vel = self.v2_velocity(VEL_DB_RESTRIKE);
        let pan = self.v2_pan(ev.pan);
        // §9.5 law 5: the rotation returns to a pitch every third glint; a
        // live one at that pitch is damped first.
        self.v2_damp_same_pitch(LANE_GLINT, f);
        self.v2_spawn(voice, ev.gain * KEY_TINE_TRIM * GLINT_LEVEL * vel, pan);
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
        let mut bell = tine(
            penta(TINE_BASE_HZ, deg),
            Touch::Step,
            MET_BELL_TAU_S,
            roof_hz(1.0 / ioi_s, true, ev.heat, Touch::Step),
            false,
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
        let voice = tine(
            penta(TINE_BASE_HZ, deg),
            Touch::Step,
            tau_v_s(ioi_s),
            roof_hz(1.0 / ioi_s, false, ev.heat, Touch::Step),
            false,
        );
        s.spawn(voice, g * KEY_TINE_TRIM, ev.pan);
    }

    /// §9.7: **no bed by default** — the silence between notes is the
    /// instrument. The optional pedal haze is a knob, not a layer, and it is
    /// not carried here.
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

    fn anchor_hz(&self) -> f32 {
        TINE_BASE_HZ
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

    // -- A1 / A15: the clock, not the count ------------------------------

    /// **THE VERSE ADVANCES ON THE CLOCK, NOT ON THE COUNT** (§9.0 cause 4,
    /// A1) — **and every key still speaks** (A15).
    ///
    /// Two steps are never closer than [`STEP_GATE_MS`]; at 4 cps every key is
    /// a step (the "slow typing promotes every key" law, which needs no rule
    /// of its own); at 10 cps the verse steps about every third key; and a
    /// 30 Hz auto-repeat advances the state 60 times while spawning at most
    /// one TUNE voice per [`RESTRIKE_COALESCE_MS`], which keeps the roll under
    /// the 15–60 Hz roughness band.
    #[test]
    fn the_verse_advances_on_the_clock_and_every_key_still_speaks() {
        // 4 cps — every key is a step.
        let mut s = synth();
        let mut steps = 0;
        for k in 0..40u32 {
            let before = s.v2.theme_pos();
            push(&mut s, SoundKind::Typed, 1_000 + k * 250, 0.0, false);
            if s.v2.theme_pos() != before {
                steps += 1;
            }
        }
        assert_eq!(steps, 40, "at 4 cps every key must sing the verse");

        // 10 cps — the verse steps every third key, on the clock.
        let mut s = synth();
        let mut step_ms: Vec<u32> = Vec::new();
        let mut spoke = 0;
        for k in 0..100u32 {
            let at = 1_000 + k * 100;
            let mark = s.born_seq;
            let before = s.v2.last_step_ms;
            push(&mut s, SoundKind::Typed, at, 0.0, false);
            if s.v2.last_step_ms != before {
                step_ms.push(s.v2.last_step_ms);
            }
            if !since(&s, mark).is_empty() {
                spoke += 1;
            }
        }
        for w in step_ms.windows(2) {
            assert!(
                w[1] - w[0] >= STEP_GATE_MS,
                "two steps {} ms apart, inside the {STEP_GATE_MS} ms gate",
                w[1] - w[0]
            );
        }
        // 10 s at 10 cps ⇒ one step per 300 ms (the gate rounded up to the
        // key grid), ±1.
        let expect = 10_000 / 300 + 1;
        assert!(
            (step_ms.len() as i64 - expect).abs() <= 1,
            "10 cps produced {} steps, expected ~{expect}",
            step_ms.len()
        );
        assert_eq!(spoke, 100, "at 10 cps every key still speaks");

        // 30 Hz auto-repeat: the state advances 60x in 2 s, the ear hears at
        // most one onset per 60 ms, and no STEP is ever thinned.
        let mut s = synth();
        let mut onsets = 0;
        let mut steps = 0;
        for k in 0..60u32 {
            let at = 1_000 + k * 33;
            let mark = s.born_seq;
            let before = s.v2.last_step_ms;
            push(&mut s, SoundKind::Typed, at, 0.0, false);
            let spawned = since(&s, mark);
            if !spawned.is_empty() {
                onsets += 1;
            }
            if s.v2.last_step_ms != before {
                steps += 1;
                assert!(!spawned.is_empty(), "a STEP was thinned — never legal");
            }
        }
        assert!(
            onsets <= 34,
            "a 30 Hz roll produced {onsets} onsets in 2 s — over the 16.7/s \
             roughness ceiling"
        );
        assert!(steps >= 8, "the verse stalled under auto-repeat ({steps})");
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

    /// Drive the bench's prose EXACTLY as `keyboard_song_ab` renders it —
    /// the bench's seed, the music box reached by voice, unstamped `push` on
    /// the synth's own 512-frame block clock — logging every spawn and the
    /// melody's `word_pos` after every cue.
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
    /// typed onset is a unison (a re-strike), the verse's own next note (at
    /// most three degrees, the theme's widest step), or a mid-word rest
    /// cadence folded to a sixth ([`WORD_LEAP_MAX_DEG`]). **One exemption,
    /// §21.4's own:** the form wrap, A″'s peak G6 leaning back onto A's C5
    /// (degree 8 → 0), which is the piece's shape and not a ghost.
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
        let (mut pairs, mut near, mut wraps) = (0usize, 0usize, 0usize);
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
            if a.deg == TUNE_DEG_HI as i8 && b.deg == TUNE_DEG_LO as i8 {
                wraps += 1;
                continue;
            }
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
        assert!(wraps >= 3, "the take never wrapped the form ({wraps})");
        // §21.4: ≥ 60 % unison-or-one-degree at 10 cps (the music-box
        // tremolo is what replaces the ghosts).
        let pct = 100.0 * near as f32 / pairs as f32;
        assert!(
            pct >= 60.0,
            "only {pct:.0} % of {pairs} in-word pairs were a unison or one degree"
        );

        // THE FINDING: every lead pair wider than a sixth is the wrap or
        // straddles a line feed.
        let lead: Vec<Spawn> = log
            .iter()
            .filter(|e| matches!(e.lane, LANE_TUNE | LANE_CASCADE | LANE_ECHO) && e.f0 > 0.0)
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
        assert_eq!(
            (s.v2.walk(), s.v2.word_pos()),
            (0, 2),
            "fixture: C5, two letters in"
        );
        let mark = s.born_seq;
        push(&mut s, SoundKind::Typed, 2_100, 0.0, false);
        let v = tune_voices(&since(&s, mark))[0];
        assert_eq!(
            s.v2.theme_pos(),
            SONG_FORM[1],
            "the pause must still cadence the phrase and open the next"
        );
        let ratio = v.p[0].f0 / penta(TINE_BASE_HZ, 0);
        assert!(
            ratio <= 5.0 / 3.0 + 1e-4,
            "the mid-word cadence leapt {ratio:.3}x from C5 — E6 is a tenth, the ghost \
             defect on the one key still inside the word"
        );
        assert_eq!(
            s.v2.walk(),
            4,
            "the cadence takes the chord tone nearest the phrase's final degree inside a sixth"
        );
        assert!(s.v2.lit(), "the folded cadence is a chord tone");

        // Between words: §10.2 verbatim.
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        push(&mut s, SoundKind::Space, 1_100, 0.0, false);
        push(&mut s, SoundKind::Typed, 2_100, 0.0, false);
        assert_eq!(
            s.v2.walk(),
            SONG_THEME[usize::from(SONG_FORM[1]) - 1],
            "between words the rest closes onto the phrase's own final degree"
        );
        assert_eq!(s.v2.theme_pos(), SONG_FORM[1]);
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
        let mut s = synth();
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
        let after = (s.v2.theme_pos(), s.v2.walk());

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
            (s.v2.theme_pos(), s.v2.walk()),
            after,
            "the playhead did not land where it had been"
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
            let landed: Vec<Voice> = since(&s, mark)
                .into_iter()
                .filter(|v| v.delay > 0.0)
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
        let after_enter = s.v2.theme_pos();
        push(&mut s, SoundKind::Jump, 2_020, 0.0, false);
        let born = since(&s, mark);
        assert!(
            born.iter().all(|v| v.lane != LANE_CASCADE),
            "the keyed Return's own line-feed echo minted a cascade"
        );
        assert_eq!(
            s.v2.theme_pos(),
            after_enter,
            "the swallowed echo moved the playhead"
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
    /// lane (glint, echo) has fade-stolen a SOUNDING voice under the 40 ms
    /// age guard. (A voice damped before its pre-delay ran out never
    /// sounded — "expires unheard" — and is not a steal.)
    fn assert_caps(s: &TrailSynth) {
        for lane in [
            LANE_TUNE,
            LANE_ECHO,
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
        let first = (s.v2.theme_pos(), s.v2.walk());
        push(&mut s, SoundKind::Typed, 1_300, 0.0, false);
        push(&mut s, SoundKind::Typed, 1_600, 0.0, false);
        assert_ne!(
            (s.v2.theme_pos(), s.v2.walk()),
            first,
            "three steps must have moved the playhead"
        );
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
        let mark = s.born_seq;
        push(&mut s, SoundKind::Typed, 2_300, 0.0, false);
        let retyped: Vec<f32> = tune_voices(&since(&s, mark))
            .iter()
            .map(|v| v.p[0].f0)
            .collect();
        assert_eq!(
            retyped, first_f0,
            "after three held deletions the retyped letter did not sing the first letter's note"
        );
        assert_eq!(
            (s.v2.theme_pos(), s.v2.walk()),
            first,
            "the playhead did not rewind one note per repeat"
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
    /// key: accounted, it cadences the phrase and resets the IOI;
    /// unaccounted, it is merely the fourth step.
    #[test]
    fn a_pause_the_render_clock_never_saw_still_rests_the_phrase() {
        let drive = |account: bool| -> (u8, i8, f32) {
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
            (s.v2.theme_pos(), s.v2.walk(), s.v2.ioi_ms())
        };
        let phrase_1 = SONG_FORM[1];
        let (pos, walk, ioi) = drive(true);
        assert_eq!(
            pos, phrase_1,
            "an accounted 5 s pause did not cadence the phrase (theme_pos {pos})"
        );
        assert_eq!(
            walk,
            SONG_THEME[usize::from(phrase_1) - 1],
            "the cadence did not close onto the phrase's final degree"
        );
        assert_eq!(ioi, IOI_DEFAULT_MS, "a ≥ 2 s gap must restart the IOI");
        let (pos, walk, _) = drive(false);
        assert_eq!(
            (pos, walk),
            (4, SONG_THEME[3]),
            "the unaccounted pause must read as no pause at all — the control is broken"
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
    /// wrapping key is a re-strike, the step gate is shut until a rest
    /// re-anchors it, and then the verse walks on.
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
            s.v2.theme_pos(),
            2,
            "past 49.7 days the second key must still be a step — the clock saturated (theme_pos {})",
            s.v2.theme_pos()
        );

        // ACROSS THE WRAP: the cost is a wrapped host stamp's, no more.
        let mut s = synth();
        s.clock_s = WRAP_S - 0.296;
        key(&mut s);
        assert_eq!(s.v2.theme_pos(), 1, "the first key steps");
        render_ms(&mut s, 300);
        key(&mut s);
        assert_eq!(
            s.v2.theme_pos(),
            1,
            "the wrapping key reads a zero gap: a re-strike, as a wrapped host stamp is"
        );
        render_ms(&mut s, 1_000);
        key(&mut s);
        assert_eq!(
            s.v2.theme_pos(),
            SONG_FORM[1],
            "the rest after the wrap must cadence the phrase and re-anchor the gate"
        );
        render_ms(&mut s, 300);
        key(&mut s);
        assert_eq!(
            s.v2.theme_pos(),
            SONG_FORM[1] + 1,
            "healed, the verse must walk on"
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

    /// **A SING-ALONG ADMITS STEPS ONLY, AND HANDS THE KEY BACK AT A PHRASE
    /// BOUNDARY** (§10.2, A31).
    ///
    /// Under a live riff a 30 Hz auto-repeat spawns ≤ 4.5 pitched TUNE
    /// onsets a second (the steps) and the re-strikes drop to the mallet
    /// alone; when the riff dies the borrowed `song_key` is NOT snapped — it
    /// is held until the verse reaches a phrase boundary and handed back
    /// there, once, before that phrase's first note.
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
        let (mut pitched, mut mallets) = (0usize, 0usize);
        for k in 0..60u32 {
            let mark = s.born_seq;
            push(&mut s, SoundKind::Typed, 1_100 + k * 33, 0.0, false);
            for v in tune_voices(&since(&s, mark)) {
                if v.p[0].lvl > 0.0 {
                    pitched += 1;
                } else {
                    mallets += 1;
                }
            }
            for _ in 0..3 {
                s.render(&mut buf);
            }
        }
        assert!(
            s.sing > 0.0,
            "the riff must outlive the burst for the law to be tested"
        );
        assert!(
            (6..=9).contains(&pitched),
            "{pitched} pitched TUNE onsets in 2 s under the riff — the steps alone are ≤ 4.5/s"
        );
        assert!(
            mallets > 0,
            "re-strikes under the riff drop to the mallet alone"
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

        // Typing on: the key changes exactly once, to neutral, and only on a
        // key that opens a phrase.
        let mut changes = Vec::new();
        for k in 0..40u32 {
            let pos = s.v2.theme_pos();
            let before = s.song_key;
            push(&mut s, SoundKind::Typed, 12_000 + k * 250, 0.0, false);
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
        assert!(
            SONG_FORM.contains(&pos),
            "the key was handed back MID-PHRASE, at theme_pos {pos}"
        );
        assert!(!s.v2_key_pending, "the handback must clear itself");
    }

    /// **A v1 VOICE AFTER THE MUSIC BOX SNAPS A PENDING KEY; IT DOES NOT HOLD
    /// IT FOR EVER** (§10.2, §16 row 9 — the v1 chain keeps v1's law).
    ///
    /// The v2 latch is sticky and the phrase-boundary handback is detected on
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
            LANE_ECHO,
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
        let mut s = synth();
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
    /// typing — sits inside A14's +1 / −3 dB of the 4 cps reference at 10, 15
    /// and 20 cps (measured ≈ −0.4, −1.2, −2.9 on the 0.60 ladder; §9.6's
    /// table, written on the 0.70 ladder, says +0.1, −0.6, −0.8 — its 20 cps
    /// row predates §10.2's 60 ms coalescing, which silences every other
    /// re-strike at a 50 ms key spacing, and the law's −3 dB floor is what
    /// still holds: 20 cps is one step, L₂ and L₄ per 250 ms, `0.36 × 4 ×
    /// (1 + 0.26 + 0.136)` = 2.01 re 4.0 = −2.99 dB exactly). **8 cps is
    /// pinned to the table, not to the cap:** the table's own arithmetic puts
    /// it at +0.54 (g² 0.833 × (4 + 4·0.36) re 4.0; +0.94 on the 0.70 ladder,
    /// which the ±1 / ±1.5 dB seeded velocity — mean bias +0.02 dB, ±0.07 dB
    /// over this 20 s window — pushed over the cap on any finite sample) —
    /// measured +0.64. The roof, meanwhile, is a non-decreasing
    /// function of the rate for every touch and lighting, and heat moves the
    /// roof and never the gain: the arc buys brightness with speed, never a
    /// decibel.
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
            if *cps == 8 {
                // Pinned to the table's own arithmetic on the 0.60 ladder
                // (+0.54 exact) — see the doc above.
                assert!(
                    (db - 0.54).abs() <= 0.35,
                    "8 cps: per-second TUNE energy is {db:+.2} dB re 4 cps; §9.6's table on the 0.60 ladder says +0.54; arc {arc:?}"
                );
            } else {
                assert!(
                    (-3.0..=1.0).contains(db),
                    "{cps} cps: per-second TUNE energy is {db:+.2} dB re 4 cps, outside +1/−3; arc {arc:?}"
                );
            }
        }
        for lit in [false, true] {
            for touch in [Touch::Step, Touch::ReStrike] {
                let mut last = 0.0f32;
                for cps in [2.0f32, 4.0, 6.0, 8.0, 10.0, 12.0, 15.0, 20.0] {
                    let roof = roof_hz(cps, lit, 0.5, touch);
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
}
