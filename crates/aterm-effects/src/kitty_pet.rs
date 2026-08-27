// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rainbow kitty **pet** — a whole cat that lives on the text plane.
//!
//! The flying companion in [`crate::kitty_cursor`] is a sticker pinned 3/4 of a
//! cell ahead of the caret: it never has a position of its own, so it can only
//! ever *be* where the caret is. This module is the other design. The pet has a
//! position, a velocity, and a ground line, and the caret is not where it *is*
//! but where it is **going**. Everything else — the whole behaviour vocabulary —
//! falls out of that one decision:
//!
//! | what the caret does | what the pet does | why |
//! |---|---|---|
//! | one column, unhurried | **walks** | the gap it must close is one cell |
//! | column after column, fast | **runs** | the caret is outpacing it |
//! | a word jump / Home / End / tab-complete | **pounces** | too far to walk: crouch, ballistic arc, landing squash |
//! | Enter, a wrapped line, an arrow up or down | **hops** the row | the ground line moved, so it must leave the floor — one gathered tick, then an arc sized to the span |
//! | a backspace or two | **flinches** | a typo being fixed is not a threat: a duck and a hard look at the caret |
//! | a real retreat — four columns, or a held delete | **startles** | the thing it was chasing is coming back at it: the full bottle, facing the caret |
//! | forward, back, forward, back (editing) | **frolics** | direction reversals are play, not travel |
//! | stops | **stands**, then **sits**, then **grooms** | it caught up; there is nothing to chase |
//! | stays stopped | **sleeps** | and stretches itself awake when you come back |
//! | a good long run, then quiet | **purrs** | contentment is earned by the run and spent while settled |
//!
//! Nothing here is a random flourish keyed off a wall clock. Every action is a
//! function of caret geometry and elapsed time, so the same typing always
//! produces the same cat — which is what makes the behaviour testable, and what
//! makes it read as an animal reacting to *you* rather than a loop playing near
//! your text.
//!
//! ## Screen awareness
//!
//! The pet knows the grid it lives in, not just the cell it is chasing:
//!
//! * It never leaves the viewport. Position is clamped to
//!   `[0, cols − width]` × `[0, rows − 1]` every tick.
//! * It knows the **wall**. Its natural station is just ahead of the caret, but
//!   when that station would hang off the right margin it crosses to the
//!   caret's *left* ([`PetBrain::station`]) instead of piling up against the
//!   edge — and it turns round to face back the way it came, because that is
//!   where the writing now is.
//! * It faces where it is going. Facing is a hysteretic function of velocity
//!   ([`FLIP_SPEED`]), emitted as `flip_x` on the sprite, so the art is authored
//!   once, facing right.
//! * Its feet stay on the row's baseline. Vertical travel is never a slide: a
//!   row change is a hop with a real arc ([`PetFrame::lift`]), so the pet is
//!   only ever *on* a line or *between* lines, never smeared across two.
//!
//! ## Gait is driven by distance, not by the clock
//!
//! The walk and run cycles advance by how far the pet has actually travelled
//! ([`STRIDE_CELLS`]), never by elapsed seconds. A wall-clock cycle moon-walks
//! the instant the pet's speed and the animation rate disagree — the feet slide
//! along a floor they are supposed to be pushing against. Distance-driven feet
//! cannot: half a stride is half a cell, at every speed, forever.
//!
//! ## The breed handoff (two kitties on glass)
//!
//! The pet's LOOK — the `(coat, iris)` pair the host syncs from the companion
//! verdict — is latched per appearance ([`PetBrain::sync_look`]): a differing
//! verdict PARKS while the pet is visible. A park that stays stable through
//! [`HANDOFF_DEBOUNCE`] earns the HANDOFF: the pet swaps to the new look IN
//! PLACE — position, gait, mood and sleep carried through exactly like a
//! species reskin — while a **departing body** wearing the OLD look spawns
//! where it stood and runs off toward the nearest edge on its side of the
//! caret, fading out as it goes. For a moment the outgoing kitty and the
//! incoming one are genuinely both on screen; that is the show (owner ask,
//! 2026-08-15: "multiple kitties per screen while they are running a swap").
//! Since the 2026-08-17 rulings the verdict changes rarely and deliberately —
//! a program that has EARNED the cursor through the host's tenure gate, the
//! base cat returning after the program is gone, a favourite pin — so the
//! handoff is a ceremony, not a metronome; the debounce stays as the guard
//! against any host that syncs a flapping verdict. And the swap is soft: the
//! incoming kitty fades UP over [`ARRIVE_IN`] while the departing body runs
//! off — the old cat leaving, the new one arriving — never a one-frame
//! recolour (owner: "switching the cats all the time is too abrupt").
//!
//! Departing bodies are scripted transients, not second brains: each is a
//! pure function of its spawn record and the brain's own clock (clockless,
//! dieless, bounded by [`DEPART_MAX`]), resolved into [`PetFrame::departures`]
//! and drawn by the emitter as extra sprites. At most [`PET_DEPARTURES_MAX`]
//! ride at once — rapid identity churn drops the OLDEST, never the newest.
//! The instant paths stay instant: reduced motion, a hidden pet, and the
//! zero-alpha land all swap with no theater and spawn no body.
//!
//! ## Clocklessness and determinism
//!
//! Like every engine in this crate, [`PetBrain::tick`] takes an injected `now`
//! and reads no wall clock. It also draws no randomness: the "occasional"
//! choices — when the tail flicks, when the wash happens — are pure functions of
//! how long it has been QUIET, i.e. of the settle the user is actually watching,
//! so a test can drive the exact sequence a user sees and the flourishes cannot
//! depend on how long the window happened to have been open.

use core::f32::consts::TAU;

use aterm_time::Instant;

use crate::cat_baker::CatColorKey;
use crate::pet_glyphs_gen::{PET_GLYPH_IDS, PET_GLYPHS, PetGlyphId};

// ── the chase ───────────────────────────────────────────────────────────────

/// Where the pet wants to stand relative to the caret, in cells, measured from
/// the caret cell's left edge to the pet's left edge. Just past the caret: the
/// companion leads the writing the way the flying kitty always has, so the cell
/// you are about to fill is never under the cat.
const STATION_LEAD: f32 = 1.0;
/// Chase gain (1/sec): the pet's *desired* speed is `gap × CHASE_GAIN`, so it
/// eases into a stop instead of arriving at full tilt and stopping dead. At the
/// 6-cell walk/run threshold this asks for ~24 cells/s — already a run — which
/// is exactly right: a caret six cells away has genuinely got away from it.
const CHASE_GAIN: f32 = 4.0;
/// Ceiling on chase speed (cells/sec). Beyond this the pet stops trying to run
/// the distance down and pounces instead, so this is really "the fastest a cat
/// bothers to run rather than jump".
const MAX_SPEED: f32 = 26.0;
/// Speed follower time constant (seconds). The pet has mass: it spends this long
/// getting up to the speed the gap is asking for, which is what makes a burst of
/// typing read as *acceleration* rather than teleportation.
const SPEED_TAU: f32 = 0.11;
/// Gap (cells) inside which the pet considers itself arrived and may settle.
/// Sized just under one cell so a single unconsumed keystroke of lead does not
/// keep an otherwise idle cat on its feet.
const ARRIVED: f32 = 0.85;
/// Speed (cells/sec) separating a walk from a run. A comfortable prose cadence
/// (~8 keys/sec) puts the caret 8 cells/sec away from a standing start, so the
/// threshold sits below that: sustained real typing runs the cat, a hunt-and-peck
/// letter every second or two only walks it.
const RUN_SPEED: f32 = 6.5;
/// Hysteresis band on [`RUN_SPEED`] so a cat hovering at the boundary does not
/// flicker between two gaits frame to frame.
const GAIT_HYST: f32 = 1.6;
/// Speed (cells/sec) above which the pet is moving enough to re-aim its facing.
/// Below it, facing is held — a cat that has stopped keeps looking where it was.
const FLIP_SPEED: f32 = 1.2;
/// Cells of travel per full four-frame WALK cycle. The pet is ~2.9 cells wide,
/// so a shade over half a body length per cycle is a natural stride: shorter
/// scurries, longer moon-walks.
const STRIDE_CELLS: f32 = 1.7;
/// Cells of travel per full four-frame RUN cycle. A gallop covers ground in
/// bounds, so its stride is double the walk's — and the number is a frame-rate
/// law, not taste: at [`MAX_SPEED`] a 1.7-cell stride cycles at 15.3 Hz, which
/// is exactly one art frame per 60 fps tick — a strobe, not a gait. Doubling
/// the stride tops the gallop out at ~7.6 cycles/s, so every frame holds the
/// glass for at least two ticks.
const RUN_STRIDE_CELLS: f32 = 3.4;
/// THE FRAME-RATE LAW ITSELF, held where a violation cannot ship. Both terms
/// are constants, so the gallop's worst-case cycle rate is decidable at build
/// time — it does not need a test run to have happened, and a future speed-up
/// of [`MAX_SPEED`] or a shortening of the stride fails the BUILD rather than
/// waiting for someone to run the suite. What a test still owes is the part no
/// constant can state: that the run gait actually USES this stride on glass —
/// see `the_gallop_cycle_never_exceeds_half_frame_rate`.
const _: () = assert!(
    MAX_SPEED / RUN_STRIDE_CELLS <= 30.0,
    "the gallop cycle must stay at or under half of 60 fps"
);

// ── the keep-ahead lead ─────────────────────────────────────────────────────
//
// "I'd like for it to keep ahead of the cursor" (owner). A pure follower can
// only ever trail: with gain g it lags a caret moving at v by v/g cells, so
// the faster you type the further BEHIND the companion falls — the exact
// opposite of leading the writing. The fix is a caret-velocity estimate and a
// station that moves ahead by it, gated on a real typing rhythm so a cat
// waiting on a hunt-and-peck author never scoots around on speculation.

/// EMA time constant (seconds) for v̂, the caret-velocity estimate. Every tick
/// decays the estimate by `exp(-dt/VEL_TAU)` and every observed move adds
/// `dc / VEL_TAU` — the impulse-train low-pass of the caret's true velocity,
/// so under steady typing v̂ converges on the caret's cells-per-second and a
/// pause eases it (and the lead built on it) back to zero over the same tau.
const VEL_TAU: f32 = 0.35;
/// Sanity ceiling on |v̂| (cells/s). A paste or a held word-jump can move the
/// caret hundreds of cells a second; the pet's PLANNING tops out at "very
/// fast typing" rather than chasing an arbitrarily silly number.
const VEL_MAX: f32 = 60.0;
/// Seconds of caret travel the station leads by — exactly `1 / CHASE_GAIN`,
/// so at the base gain the lead cancels the follower's steady-state lag
/// (`v / CHASE_GAIN`) and the pet holds [`STATION_LEAD`] while you type
/// instead of trailing the caret by the lag.
const LEAD_TIME: f32 = 0.25;
/// Ceiling on the velocity lead (cells). Deliberately conservative until the
/// baker's per-pose body bbox lands (see [`PetBrain::set_body_left_px`]) —
/// an anchored visible edge is what makes a bigger lead read as "ahead"
/// rather than "gone".
const LEAD_MAX: f32 = 4.0;
/// The rhythm gate: the lead grows only once this many SAME-direction moves
/// have landed, each within [`RHYTHM_WINDOW`] seconds of the last.
/// Hunt-and-peck — a letter every second or two, or editing back and forth —
/// never opens the gate, so the pet never scoots ahead of a caret that is
/// not actually going anywhere.
const RHYTHM_MOVES: u8 = 3;
const RHYTHM_WINDOW: f32 = 0.6;
/// The fast chase gain (1/s), blended in as |v̂| climbs past [`RUN_SPEED`]
/// (fully in by 2×). A galloping caret needs the stiffer follower or its lag
/// outruns the clamped lead; the overshoot guard in `tick` is what lets the
/// gain rise without ringing.
const CHASE_GAIN_FAST: f32 = 6.0;
/// Wall hysteresis (cells): having crossed to the caret's LEFT, the pet only
/// crosses back once the lead-free station clears the right margin by this
/// much — typing one column at the crossing must not flip-flop the cat over
/// your cursor with every keystroke.
const WALL_RETURN_MARGIN: f32 = 3.0;
/// The wall transit: a station flip crosses the caret column, and a cat does
/// not walk through your cursor — it hops it, one small quick arc.
const WALL_HOP_DUR: f32 = 0.25;
const WALL_HOP_ARC: f32 = 0.9;

// ── the screen-crossing jump ────────────────────────────────────────────────
//
// "More variance jumping across the screen, that is cute" (owner). The pounce
// is transport; THIS is theatre — notice, butt-wiggle gather, a real ballistic
// crossing with rise/descend frames, a heavy landing with skid and dust, and
// for the truly screen-wide span, two bounds. Deterministic throughout: every
// number below keys off the span and the grid, never a die roll.

/// Trigger (a): a SINGLE move of at least `max(BIG_JUMP_COLS,
/// BIG_JUMP_COLS_FRAC × cols)` is a screen-crossing jump, latched like the
/// pounce so it survives whatever hold it lands during.
const BIG_JUMP_COLS: f32 = 24.0;
const BIG_JUMP_COLS_FRAC: f32 = 0.4;
/// Trigger (b): a STANDING gap of half the grid once a hold resolves — the
/// pet got left truly behind, and a sprint across the whole window would
/// read as a glitch where a leap reads as an animal.
const BIG_GAP_FRAC: f32 = 0.5;
/// Trigger (c), the JOY variant: a settled cat carrying at least this much
/// contentment answers a mere word jump with the whole show.
const JOY_CONTENT: f32 = 0.85;
/// The gather: longer than the pounce's crouch, alternating
/// `PetCrouch`/`PetCrouchWiggle` at ~7 Hz — the butt-wiggle.
const BIG_CROUCH_DUR: f32 = 0.28;
const WIGGLE_HZ: f32 = 7.0;
/// Flight time and arc, by span: `clamp(0.45 + span·0.006, 0.45, 0.85)` s
/// under `clamp(1.2 + span·0.02, 1.2, 3.2)` cell heights — ADDITIONALLY
/// clamped so the sprite's top never leaves the viewport (the 509c9f01
/// faith: nothing renders above the line).
const BIG_FLIGHT_BASE: f32 = 0.45;
const BIG_FLIGHT_PER_CELL: f32 = 0.006;
const BIG_FLIGHT_MAX: f32 = 0.85;
const BIG_ARC_BASE: f32 = 1.2;
const BIG_ARC_PER_CELL: f32 = 0.02;
const BIG_ARC_MAX: f32 = 3.2;
/// The landing has WEIGHT: a 0.40 s recovery whose squash scales with the
/// span (`q0 = LAND_SQUASH · (0.7 + 0.3·min(span/20, 1))`), plus an 0.8-cell
/// exponential skid the flight aims SHORT by, so the slide carries the paws
/// exactly onto the station.
const BIG_LAND_DUR: f32 = 0.40;
const BIG_SQUASH_NEAR: f32 = 0.7;
const BIG_SQUASH_SPAN: f32 = 20.0;
const SKID_CELLS: f32 = 0.8;
/// The variance flourish: a span past this fraction of the grid splits into
/// TWO bounds, 60/40, with a brief touch-land between — deterministic from
/// the span alone.
const TWO_BOUND_FRAC: f32 = 0.6;
const BOUND_SPLIT: f32 = 0.6;
const TOUCH_LAND_DUR: f32 = 0.13;

// ── the sleep z's and the purr tell ─────────────────────────────────────────
//
// "when the kitty is sleeping, there should be little 'zzz' and more of a
// cute animation" (owner). The z's ride the mote lane under its laws — free-
// floating, rotated, scaling, fading, accent-tinted — and they live entirely
// INSIDE the idle-to-zero window: light sleep drifts them, deep sleep spawns
// nothing, and the last one dies before the frame lane releases.

/// LIGHT sleep only: a z-mote every ~1.2 s, drifting up-and-away for ~2.2 s,
/// at most [`ZEE_ALIVE_MAX`] alive. Spawning stops [`ZEE_LIFE`] before the
/// breath window closes, so deep sleep begins with an empty lane — the
/// 0.15.0 idle-to-zero contract, kept to the letter.
///
/// Was 1.6 s. The sleeping loaf is the pose a long-idle terminal parks on, so
/// it is the one the owner spends the most time looking at, and at 1.6 s the
/// ~7.8 s spawn window (`SLEEP_AFTER + BREATH_WINDOW - ZEE_LIFE`) fitted only
/// four z's — long enough between them that the tell read as intermittent
/// rather than as breathing. 1.2 s fits six and puts the first one up 0.4 s
/// sooner. It is a CADENCE change, not a brightness one: 0.83 Hz of births,
/// each a slow fade-in/fade-out over 2.2 s, against WCAG 2.3.1's 3 Hz
/// general-flash bound — the same bound `cursor_rainbow`'s twinkle is pinned
/// under. Going much below ~1.1 s would start stacking three at once against
/// [`ZEE_ALIVE_MAX`] (`ZEE_LIFE / ZEE_EVERY`), which is where a drift turns
/// into a swarm.
const ZEE_EVERY: f32 = 1.2;
const ZEE_LIFE: f32 = 2.2;
const ZEE_ALIVE_MAX: usize = 3;
/// Waking pops ONE startled z — bigger, faster, briefer — before the stretch.
const ZEE_POP_LIFE: f32 = 0.55;
/// The purr tell: a floating ♪ or heart every ~2 s while purring, same lane,
/// same laws.
const PURR_MOTE_EVERY: f32 = 2.0;
const PURR_MOTE_LIFE: f32 = 1.9;

// ── the settled gaze and the loaf ───────────────────────────────────────────
//
// Review #6: a settled pet should acknowledge YOU. The settle-turn reads the
// caret's side a beat after arriving; a close caret earns the face-on sit
// (eye contact at rest), a far caret BEHIND the facing earns the
// over-the-shoulder peek — the body never flips at rest, because a flip
// reads as a twitch where a glance reads as a cat.

/// Seconds after arriving before the pet acknowledges where the caret is.
const SETTLE_TURN: f32 = 0.5;
/// |caret gap| (caret column to the pet's left edge, cells) inside which the
/// settle makes eye contact ([`PetGlyphId::PetSitFront`]).
const SIT_FRONT_GAP: f32 = 3.0;

// ── the settle-turn's brakes (owner ruling, 2026-08-12) ─────────────────────
//
// OWNER: "when it's sleeping, and the cursor is near it and moving back and
// forth, having the kitty flip instantly looks bad. I'd like for that to be
// more subtle and realistic."
//
// The mechanism, exactly: `settle_gaze` runs on EVERY settled tick, `Sleep` is
// a settled pose, and while pointer attention is live the gaze target IS the
// pointer — which no caret move ever wakes the cat away from, because a
// pointer is not a keystroke. So a mouse jiggling over a sleeping cat's
// midline used to re-decide `facing_left` sixty times a second. The block
// above already states the principle it was violating — "the body never flips
// at rest, because a flip reads as a twitch where a glance reads as a cat" —
// and the near branch flipped the body anyway.
//
// Three brakes, because one is not enough and the awake chase must not pay for
// any of them (a walking cat still faces where it is going: that facing comes
// from `FLIP_SPEED` on the follower, and from `begin_arc` in the air, neither
// of which is touched):
//
//  1. A DEAD BAND around the pet's own midline, inside which the target has no
//     SIDE at all. Sub-cell jitter across the centre line stops being a signal
//     rather than becoming a slower signal.
//  2. A DWELL: the target must hold the far side CONTINUOUSLY before the body
//     comes round, and any frame it is not committed there rewinds the clock
//     to zero. This is the one that answers "back and forth" as such — an
//     oscillation never accumulates, however wide it swings, because it is the
//     staying that is being measured and not the being-there.
//  3. DEEP SLEEP DOES NOT TURN AT ALL. Past the breath window this module
//     already promises a pet "deeply asleep and perfectly still", whose frame
//     settles so the host can idle to zero. A body flip there is a lie about
//     the animal and a lie about the frame lane at the same time.
//
// A hop-and-turn ANIMATION was weighed and rejected for this change: all 23
// poses are authored facing right and mirrored by `flip_x`, so a real turn is
// new art, and a faked one (squashing `scale_x` through zero) would be a new
// motion vocabulary invented for a single case — and on its own it makes the
// oscillation WORSE, a cat swivelling like a radar dish. Animation is what you
// add after the flip has stopped firing, not instead of stopping it.

/// Cells either side of the pet's midline inside which a gaze target has no
/// side — the settle-turn's hysteresis. Under half a cell is inside the
/// caret's own cell, so it cannot honestly be called a direction.
const GAZE_FLIP_BAND: f32 = 0.4;
/// …and the band a SLEEPING pet uses: over twice as wide, so the pointer must
/// be somewhere a cat could plausibly have noticed rather than merely on the
/// other side of its nose.
const SLEEP_FLIP_BAND: f32 = 1.0;
/// Seconds a gaze target must HOLD the far side before an awake settled pet
/// turns to meet it. Six frames: long enough that nothing flips on a single
/// crossing, short enough that eye contact still reads as prompt.
const SETTLE_FLIP_DWELL: f32 = 0.10;
/// …and the dwell a SLEEPING pet demands: more than a second of the pointer
/// staying put. A cat does not spin to face a mouse pointer; it comes round,
/// eventually, to something that is still there.
const SLEEP_FLIP_DWELL: f32 = 1.2;
/// Quiet (seconds) before the sit melts into the loaf — ~2× [`SIT_AFTER`],
/// the review's LONG-dwell pose (and its C2 low-profile shape: barely over a
/// row tall, so a parked cat shades nothing above the prompt).
///
/// C2 DEFERRAL: the review also wants the loaf FORCED while row-1 above the
/// pet is inked. The brain's inputs ([`PetSense`]) carry no grid content, so
/// that probe cannot be wired from here without a new host-supplied bit —
/// deferred until the sense grows one, rather than faked from geometry.
const LOAF_AFTER: f32 = 2.8;

// ── ink awareness (the 0.19.0 gauntlet's F1 seam) ───────────────────────────
//
// The capture gauntlet's ship-blocker: every EVENT pose — the notice-gather,
// the touch-land, the cheer and droop stances, the groom, the wake — chose its
// station by caret geometry alone, so a hold that outlived a prompt print left
// the cat squatting on the user's own words (seven frames, one root). The
// brain now carries a host-fed per-row ink map ([`PetBrain::sense_ink`]): the
// chase station dodges glyphs, every grounded pose is evicted to blank cells
// the moment ink invades its footprint, the watch hugs the newest inked row,
// and the sleep z's die before they can drift into a text band. Absent the
// feed the map is empty and every rule below is inert — pure geometry keeps
// exactly today's behavior, which is what makes the seam safe to ship ahead
// of the host wiring.

/// Horizontal margin (cells) shaved off both sides of the authored tile
/// before the footprint is judged against ink — the art carries transparent
/// side margins, and evicting a cat whose EMPTY border grazes a glyph would
/// fidget it forever.
const INK_PAD: f32 = 0.30;
/// Clearance (cells) kept between a re-anchored footprint and the ink it fled.
const INK_GAP: f32 = 0.35;
/// Ceiling (cells) on how far the ink law may move a station — a **step
/// aside**, roughly one body width and a half.
///
/// OWNER, 2026-08-10: "the cursor kitty gets pushed around in the text when
/// editing in the middle of the text." Exactly so, and the mechanism is this
/// law without a ceiling. `ink_spans` knows only a row's FIRST and END
/// column, so the only blank stand it can ever name for a caret sitting
/// inside a sentence is past the end of that sentence — measured, a caret at
/// column 12 stationed the pet at 31.35. Insert a character there and `end`
/// grows by one, so the station grows by one, and the cat is shoved a full
/// cell right per keystroke by text it is nowhere near. It is not following
/// the caret at all; it is riding the end of the line.
///
/// Past this distance the law yields and the pet keeps the plain station —
/// standing over the words, which reads as a cat sitting on the line you are
/// writing, not as a cat that left. That is the principle the walled-shut
/// case already states ("the answer to 'no blank ground here' is never
/// 'teleport somewhere else entirely'"), extended from *no blank ground* to
/// *no blank ground NEARBY*.
///
/// End-of-line typing — the common case — is untouched: there the station
/// sits right of the caret on genuinely blank cells, so `ink_overlaps` is
/// false and the law never runs.
const INK_EVICT_MAX: f32 = 4.0;
/// Speed (cells/sec) of a step-aside eviction. Grounded poses return before
/// the chase, so their eviction is its own tiny follower rather than a cut —
/// a sleeper slides out from under an arriving line over ~a third of a second
/// instead of popping there between two frames. Only SHORT evictions glide:
/// a pet left behind entirely (gauntlet F1 — the wake atop "printf", the
/// droop skulking on a failed command) is re-anchored outright, because
/// walking it back would be a different animal from re-staging a pose.
const INK_EVICT_SPEED: f32 = 12.0;
/// How late in an arc a mid-air re-aim may still rebase its launch point.
/// The rebase solves `from' = (col - to'·u)/(1 - u)`, so the answer runs away
/// as `u` approaches 1; past this bar the re-aim is dropped and the flight is
/// allowed to finish, which is the better read anyway — a cat that is already
/// landing should land, not swerve.
const REAIM_MAX_U: f32 = 0.85;
/// Slop (cells) allowed when reading a line WRAP off the caret delta — a
/// double-width glyph at the margin wraps from `cols - 2`.
const WRAP_SLOP: f32 = 2.0;

/// THE FOLD BAR: how many columns a caret delta must span, against a
/// `w`-column grid, to read as the seam of a line wrap. One shared
/// `const fn`, by law: the heuristic fold, the fact-gated fold and the tests
/// must all measure the SAME seam, or a pane width exists where they
/// disagree. `w - WRAP_SLOP` admits the double-width glyph that wraps from
/// `cols - 2`; the [`POUNCE_JUMP`] floor keeps a narrow pane from reading
/// ordinary moves as seams — a delta too small to be mistaken for a jump has
/// nothing to be rescued from. Note the bar is deliberately NARROW: a
/// coalesced host frame carrying `(5,198) → (6,3)` is not a wrap to
/// anything; widening the slop, not weakening the bar, is the honest fix.
const fn wrap_bar(w: f32) -> f32 {
    let bar = w - WRAP_SLOP;
    if bar > POUNCE_JUMP { bar } else { POUNCE_JUMP }
}
/// PERK-AND-WATCH (gauntlet F5): how far (rows) the live output edge may run
/// from the watcher's feet before it hops to hug the newest line again — the
/// hysteresis that keeps a fast stream from strobing the pursuit.
const WATCH_HUG_ROWS: f32 = 2.0;
/// Z-MOTE ink fade (gauntlet F8): rows of clearance over which a drifting z
/// eases to zero as it nears an inked band above — gone BEFORE the crossing,
/// never on it.
const ZEE_INK_FADE_BAND: f32 = 0.6;
/// Gauntlet F9: the face-on settle's muzzle margin. Eye contact parks the
/// visible muzzle clear of the caret cell instead of inside it — the caret
/// wins z-order either way, but a blotted face is a blotted face.
const PECK_CLEAR: f32 = 0.30;

// ── the pounce ──────────────────────────────────────────────────────────────

/// A **gap** (not a single move) of at least this many columns is out of
/// walking range and is answered with a pounce.
///
/// Keying this off the standing gap rather than off the move that opened it is
/// what makes the pounce survive an intervening one-shot: a jump that lands
/// while the pet is still stretching awake, or mid-double-take, is still a jump
/// when the pet finally looks up. A move-triggered pounce silently degrades to a
/// sprint in exactly those cases.
///
/// The value is set well above the worst-case *chase lag*, so ordinary typing
/// can never trip it. A follower with gain [`CHASE_GAIN`] trails a caret moving
/// at `v` cells/sec by `v / CHASE_GAIN` cells; even a furious 20 keys/sec is
/// only 5 cells. The extra headroom covers the other way a gap opens: the pet
/// standing still through a one-shot hold (a startle, a frolic, a wake-up
/// stretch) while you keep typing. That is a cat that got left behind and has to
/// sprint, not a cat that saw something jump.
const POUNCE_GAP: f32 = 14.0;
/// A SINGLE observed caret move of at least this many columns is a jump —
/// a word-skip, a tab completion, `End`, a paste — and earns a pounce outright,
/// however small the resulting gap. Latched into [`PetBrain::pending_pounce`]
/// rather than acted on immediately, because the move may land while the pet is
/// mid-hold; the intent has to outlive the hold or the leap silently degrades
/// into a walk.
const POUNCE_JUMP: f32 = 6.0;
/// Anticipation: how long the cat gathers before it leaves the ground. Short
/// enough not to feel laggy, long enough that the launch reads as a decision.
const CROUCH_DUR: f32 = 0.10;
/// THE SCALED COIL (DESIGN-kitty-motion-2026-08-19 §3, rung 7): a longer jump
/// gathers longer. The pounce's coil grows from [`CROUCH_DUR`] by this many
/// seconds per cell of span — 28.6% of [`FLIGHT_PER_CELL`], so the wind-up
/// grows with the jump but never rivals the flight it is winding up for.
const COIL_PER_CELL: f32 = 0.006;
/// Ceiling on the scaled coil, and THE ORDERING LAW: `COIL_MAX <
/// BIG_CROUCH_DUR`, held below by the build assert, so the three gathers stay
/// strictly ranked — the Enter hop's one-tick gather, then the pounce's
/// scaled coil (at most this), then the big jump's 0.28 s butt-wiggle. A
/// pounce that out-gathered the screen-crossing show would invert the
/// vocabulary: more wind-up must always mean a bigger jump.
const COIL_MAX: f32 = 0.22;
const _: () = assert!(
    COIL_MAX < BIG_CROUCH_DUR,
    "the gather ordering law: the pounce's longest coil stays under the big wiggle"
);
/// How deep a full coil compresses (`scale_y` bottoms at `1 - COIL_SQUASH`).
/// Derivation, not taste: equal to [`LAUNCH_STRETCH`], so the coil stores
/// exactly the elongation the launch spends and the release ramp crosses
/// rest (`1.0`) halfway through — the spring metaphor made arithmetic. The
/// depth is further scaled by the coil's own length (`dur / COIL_MAX`): a
/// quick gather is a shallow one, which is also what bounds the envelope's
/// per-frame slope at `COIL_SQUASH / COIL_MAX` however short the coil. The
/// compression is volume-preserving (`scale_x = 1 / scale_y`) — a wind-up is
/// muscle, not impact, so unlike the landing's shape law no volume is lost,
/// and §8's no-pop bar (no >2% frame-to-frame volume step) holds through the
/// whole attack by construction.
const COIL_SQUASH: f32 = 0.14;
/// THE RELEASE (§3: "anticipation without release is a stall"). Through the
/// last `COIL_RELEASE` seconds of a gather the body ramps per-axis from the
/// coil's compression into the launch frame's stretch (`scale_x
/// 1 + LAUNCH_STRETCH`, `scale_y 1 - 0.6·LAUNCH_STRETCH` — the dominant
/// axis of virtually every gathered flight is horizontal; a rare
/// row-dominant bound transposes the two at launch, which preserves the
/// volume the no-pop law measures). Without this ramp the naive coil grew
/// BOTH axes in one tick at the crouch→launch cut — a volume pop exactly
/// where the eye is most engaged. ~2.4 frames at 60 fps: long enough that
/// no single frame steps the volume more than ~2%, short enough that the
/// release still reads as a snap of intent.
const COIL_RELEASE: f32 = 0.04;
/// Seconds of flight per cell of pounce distance, and the clamp either side.
/// Flight time grows with distance (a longer jump takes longer) but sub-linearly,
/// so a jump clear across many columns is quick and a five-cell hop is not
/// instant. Deliberately phrased without a reserved scope word: these are
/// timing scalars measuring the DISTANCE one pounce covers, not the reach of an
/// enforcer. Saying so directly beats asserting a scope obligation and then
/// excusing it with a waiver.
const FLIGHT_PER_CELL: f32 = 0.021;
const FLIGHT_MIN: f32 = 0.16;
const FLIGHT_MAX: f32 = 0.42;
/// Peak height of a pounce arc, in cell heights, and how far it grows with
/// distance. A jump that stays flat reads as a slide.
const ARC_BASE: f32 = 0.55;
const ARC_PER_CELL: f32 = 0.030;
const ARC_MAX: f32 = 1.5;
/// The landing: how long the braced frame holds, and the squash it lands with.
/// The recovery is the shape law the flying kitty already uses — shorter by `q`,
/// wider by `0.6 q` — so both companions land like the same animal.
const LAND_DUR: f32 = 0.26;
const LAND_SQUASH: f32 = 0.24;
/// A row change always arcs, however small the column delta: the pet is on a
/// line, and lines are surfaces. Vertical arcs are shorter and lower than a
/// horizontal pounce of the same span — dropping to the next line is a step
/// down, not a leap. The arc grows with the column span, clamped to one cell
/// height: an Enter from column four is a tidy step down, an Enter from the
/// end of a long line is a real jump and earns a real arc.
const HOP_DUR: f32 = 0.20;
const HOP_ARC: f32 = 0.42;
const HOP_ARC_PER_CELL: f32 = 0.012;
const HOP_ARC_MAX: f32 = 1.0;

// ── the airborne body (DESIGN-kitty-motion-2026-08-19 §3) ───────────────────

/// THE ASYMMETRIC STRETCH (rung 7b): a body leaves the ground harder than it
/// approaches it. The climb elongates along the motion axis by up to
/// `LAUNCH_STRETCH`; the descent by only `DESCEND_STRETCH` — half — which
/// halves the cliff the touchdown frame has to absorb. The counter-axis
/// follows at 0.6× (the landing's own shape-law ratio), and the split is
/// applied in BOTH dominant-axis branches of the Leap arm: a hop or an
/// Enter-arc whose pixels travel mostly vertically earns the same asymmetry
/// as a horizontal pounce, or the touchdown cliff survives on exactly the
/// flights that land between lines.
const LAUNCH_STRETCH: f32 = 0.14;
const DESCEND_STRETCH: f32 = 0.07;
/// THE TIME-BASED POSE SCHEDULE (rung 9), replacing the fixed `0.25/0.6`
/// u-window: the rise and the landing approach are BEATS, authored in
/// seconds, because a pose must hold the glass long enough to read (~4 ticks
/// at 60 fps) however long the flight is. The u-window crushed the rise to
/// 2.4 ticks on a minimum pounce and stretched the descend to a third of a
/// screen-crossing bound. These values are the old window evaluated at the
/// modal pounce (`dur ≈ 0.26 s`: rise 25%, descend 40%), stated in seconds
/// so they no longer scale with the flight.
const LEAP_RISE_T: f32 = 0.07;
const LEAP_DESC_T: f32 = 0.11;
/// The hero pose's floor (2.88 ticks at 60 fps — the least glass a pose can
/// hold and still register as a beat rather than a flicker), and the
/// NORMALIZER's pivot: when `LEAP_RISE_T + LEAP_DESC_T + MIN_HERO` exceed
/// the flight, both approaches scale by `(dur − MIN_HERO)/(rise + desc)` —
/// one shared factor, never two independent clamp chains, which is what let
/// the reviewed patch clamp a descend LONGER than its own flight. Below
/// `3·MIN_HERO` the flight cannot fit three legible beats at all and the
/// schedule falls back to two, split at the apex.
const MIN_HERO: f32 = 0.048;
/// THE HANG (rung 10), `f.big` only: a screen-crossing bound floats its apex.
/// `h(u) = 1 − |2u−1|^p` with `p = 3.0` rising and `HANG_P_DOWN` falling —
/// steeper up, a shade softer down — grows the `lift ≥ 0.9·arc` window from
/// 0.3162 of the flight (the parabola, `p = 2`) to 0.4236. Big only: the
/// same flattening on an ordinary pounce is not a hang, it is a slide across
/// the reading zone on the highest-frequency airborne event in the product.
/// Determinism (§5): the up-branch is hand-expanded as `s·s·s` in
/// [`PetBrain::flight_lift`] — exact and portable — so this down-branch
/// exponent is the file's one non-integer `powf`.
const HANG_P_DOWN: f32 = 2.4;

// ── the launch law ──────────────────────────────────────────────────────────
//
// "A change of place is paid for by the body, or it is not a change of place,
// it is a repaint" (DESIGN-kitty-motion-2026-08-19 §0) — and an animation is
// not a defence: a 0.42 s crossing of 191 cells has an animation and reads as
// a teleport. The per-frame law (`the_cat_never_changes_position_without_
// animating_it`) cannot see this, because it judges each frame against the
// live arc's OWN rate — every flight is lawful at whatever rate it chose.
// These bounds close the hole at the LAUNCH: `begin_arc` asserts the rate of
// the arc the caller chose, and the span caps derived below are what keep
// every launcher inside it.

/// Ceiling (cells/sec) on an ORDINARY launch — hop, pounce, wall transit,
/// ink re-anchor. Derivation, not taste: `flight_shape`'s vertical branch
/// saturates its duration at [`FLIGHT_MAX`] once the span passes ~21 cells,
/// so a saturated legitimate row hop is 22 cells / 0.42 s = 52.4 c/s — 55
/// gives that real animal a little headroom, and a teleport none.
const HOP_SPEED_MAX: f32 = 55.0;
/// Ceiling (cells/sec) on a SCREEN-CROSSING bound (`big`). The ratified
/// two-bound split peaks at [`BOUND_SPLIT`] of a 191.4-cell crossing flown
/// in [`BIG_FLIGHT_MAX`]: 114.8 / 0.85 s = 135.1 c/s — 140 pins that
/// ceiling. The law does NOT exempt `big` (the wrap's loudest form IS big);
/// it only grants the bound the bar its shipped shape already earns.
const BIG_HOP_SPEED_MAX: f32 = 140.0;
/// The longest span a pounce may launch across — derived, never tuned: a
/// pounce's flight time clamps at [`FLIGHT_MAX`], so the longest span it can
/// cover lawfully is `HOP_SPEED_MAX × FLIGHT_MAX` ≈ 23.1 cells. With
/// [`POUNCE_GAP`] this turns the standing-gap pounce door into a BAND:
/// nearer than the band walks, past it the arm declines and the gap falls
/// to the big-jump door or the follower's gallop.
const POUNCE_SPAN_MAX: f32 = HOP_SPEED_MAX * FLIGHT_MAX;
/// The longest span a wall transit may hop — same derivation: the transit
/// arc's duration is FIXED at [`WALL_HOP_DUR`], so its lawful reach is
/// `HOP_SPEED_MAX × WALL_HOP_DUR` = 13.75 cells. A wider transit hops this
/// far and leaves the remaining gap to the ladder on later ticks.
const WALL_SPAN_MAX: f32 = HOP_SPEED_MAX * WALL_HOP_DUR;

// ── the reactive states ─────────────────────────────────────────────────────

/// A SINGLE retreat of at least this many columns is a real retreat and earns
/// the full bottle: back arched, tail up, ears pinned. Below it the pet only
/// flinches — one backspace is a typo being fixed, not a threat, and firing
/// the whole fright at every correction taught the eye to ignore the pose.
const STARTLE_COLS: f32 = 4.0;
/// A held delete bottles too: this many retreats, each arriving within
/// [`STARTLE_RUN_WINDOW`] seconds of the last, are one sustained thing coming
/// back at the pet, whatever their individual size.
const STARTLE_RUN: u8 = 3;
const STARTLE_RUN_WINDOW: f32 = 0.40;
/// The small-retreat flinch, built entirely from plumbing that already ships:
/// the perk hold freezes the feet while this envelope ducks the body — shorter
/// by [`FLINCH_SQUASH`], wider by 0.6× of it (the crate's landing shape law) —
/// decaying to rest over this many seconds. The bottle's pinned ears
/// (`ear_flat = 1.0`) are baked into the startle pose at art time, so the
/// flinch cannot borrow a half-weight of them at runtime; that floor is queued
/// for the art wave.
const FLINCH_DUR: f32 = 0.18;
const FLINCH_SQUASH: f32 = 0.10;
/// How long the startle holds. Long enough to read at a glance; short enough
/// that a held backspace reads as one sustained fright rather than a strobe
/// (each further delete inside the hold extends it without re-firing the pose).
const STARTLE_HOLD: f32 = 0.38;
/// Direction reversals inside this window mean you are *editing* — fixing a
/// word, walking a cursor back and forth — not travelling. That is play.
const FROLIC_WINDOW: f32 = 1.30;
/// Reversals needed to trigger it. Two is an ordinary correction; three inside
/// 1.3 s is unmistakably fussing.
const FROLIC_REVERSALS: u8 = 3;
/// How long a frolic plays out before the pet goes back to work.
const FROLIC_HOLD: f32 = 0.85;
/// A caret that jumps at least [`POUNCE_JUMP`] away while the pet is settled
/// gets a double-take: ears up, head high, *then* the pounce. Deliberately the
/// SAME threshold, so a jump always reads as the one choreography — notice,
/// gather, leap — rather than two rules that can disagree about whether a given
/// move was big.
const PERK_HOLD: f32 = 0.30;

// ── the settled states ──────────────────────────────────────────────────────

/// Quiet (seconds) before a standing pet folds down into a sit.
const SIT_AFTER: f32 = 1.4;
/// The stand→sit fold (seconds): the brief scale ease either side of
/// [`SIT_AFTER`] that lowers a standing cat into the sit — the stand gathers
/// down over its last beats, the sit rises back to rest over its first — so
/// the two poses meet at the same height instead of snapping.
const SIT_FOLD: f32 = 0.22;
/// Quiet (seconds) before a sitting pet curls up and sleeps. Long enough that it
/// never happens mid-thought at the keyboard; short enough that a coffee break
/// comes back to a sleeping cat.
const SLEEP_AFTER: f32 = 22.0;
/// The wake-up stretch, played once on the way out of [`PetAction::Sleep`]
/// before the pet is allowed to chase anything.
const WAKE_DUR: f32 = 0.62;
/// Quiet (seconds) inside a sit before the pet grooms, and how long a groom
/// lasts. Sitting perfectly still forever is the one thing a cat never does.
const GROOM_AFTER: f32 = 6.5;
const GROOM_DUR: f32 = 1.70;
/// Sleep breathing: seconds per full in-out cycle. A resting cat is around
/// 25 breaths/min; 2.4 s is a shade slower, which reads as deeply asleep.
const BREATH_PERIOD: f32 = 2.4;
/// Depth of the sleeping swell, as a fraction of the sprite box.
const BREATH_DEPTH: f32 = 0.030;

// ── the purr ────────────────────────────────────────────────────────────────

/// Contentment is EARNED by travel and SPENT while settled — a cat purrs
/// because it has just had a good time, not because a timer fired. Gained per
/// cell actually run (not per keystroke: the pet has to have done the work),
/// clamped to 1.
const CONTENT_PER_CELL: f32 = 0.020;
/// Contentment decay while settled (per second). ~14 s from full to empty: one
/// good burst of writing buys a decent purr, and it fades if you never come back.
const CONTENT_DECAY: f32 = 0.070;
/// Contentment decay while moving (per second) — travel keeps topping it up, so
/// this only matters for a long unhappy scrub with no forward progress.
const CONTENT_MOVING_DECAY: f32 = 0.020;
/// Contentment at/above which a settled pet purrs instead of merely sitting.
/// ~13 cells of running earns it: a sentence, not a word.
const PURR_GATE: f32 = 0.26;
/// Purr rhythm (Hz) and the depth of its body swell. Cats purr at 20–30 Hz,
/// which at any sane frame rate is a blur — so this is the *breathing* under the
/// purr, not the purr itself, and the exported [`PetFrame::purr`] carries the
/// real intensity for anything that can render it faster (audio, a glow pulse).
const PURR_HZ: f32 = 1.9;
/// ~5% (review finding 7): the old 2.2% swell was sub-pixel at most cell
/// sizes, so the purr read as a frozen frame — the chest has to visibly work.
const PURR_DEPTH: f32 = 0.05;

// ── the interactive stimuli (wave 1) ────────────────────────────────────────
//
// "The more dynamic and interactive it is, the more fun!" (owner). Three
// stimuli beyond the caret: the terminal BEL, the shell's exit status, and a
// click on the cat itself. All three arrive as `note_*` methods that only
// LATCH (the `pending_pounce` idiom — never act at note time, because the
// stimulus may land mid-hold or mid-flight and the intent has to outlive it);
// `tick` consumes them once the pet is free, below every caret-travel intent.
// Deterministic throughout: injected `now` only, variation by mote-serial
// parity, never a die roll.

/// How long a latched BEL stays actionable. A bell that rings while the pet
/// is airborne either lands inside this window (and the fright plays on the
/// ground) or expires unconsumed — a startle at something that rang a second
/// ago reads as a glitch, not a reflex.
const BELL_LATCH_TTL: f32 = 0.6;
/// A command must have RUN this long (ms) for its success to be worth a
/// cheer: two seconds separates "the shell answered" from "the machine
/// worked and came back happy".
const CHEER_MIN_MS: u64 = 2000;
/// Past this (ms) the success was a BUILD — the cheer upgrades to the ♪/♥
/// alternation, by mote-serial parity like the purr tell.
const CHEER_BIG_MS: u64 = 30_000;
/// Contentment bought by a long success. Deliberately UNDER [`PURR_GATE`]:
/// one green build cannot buy a purr from a cold cat (the cold-purr law) —
/// it warms the cat toward one.
const CHEER_CONTENT: f32 = 0.18;
/// The fast-success nudge: a command that finished in under [`CHEER_MIN_MS`]
/// moves the ledger, not the body — no latch, no choreography.
const CHEER_FAST_CONTENT: f32 = 0.06;
/// Contentment LOST to a failure. Bigger than the cheer's gain, because a
/// broken build outweighs a green one — the pet cools faster than it warms.
const SULK_CONTENT: f32 = 0.25;
/// How long the sulk holds: feet planted, body low, zero motes. Grief is
/// slower than fright ([`STARTLE_HOLD`]) and slower than play
/// ([`FROLIC_HOLD`]) — that is what makes it read as a mood, not a flinch.
const DROOP_HOLD: f32 = 1.6;
/// How long a latched pet (a click on the cat) stays actionable — long
/// enough to survive any single flight + landing, short enough that a click
/// from before a burst of typing does not purr a minute later.
const PET_LATCH_TTL: f32 = 2.0;
/// How many pets can queue. Clicks past the cap are absorbed silently —
/// the cat is already as petted as it can be.
const PET_LATCH_MAX: u8 = 3;
/// The petting hold: a purr-flavored beat per consumption, whatever the
/// contentment says — affection is answered even by a cold cat.
const PET_HOLD: f32 = 1.1;
/// Contentment per pet consumed. One click cannot buy a sustained purr
/// (0.08 < [`PURR_GATE`]); four inside the TTL can — that is the point, and
/// eleven walk the ledger up to [`JOY_CONTENT`], where the next word jump
/// answers with the whole screen-crossing show.
const PET_CONTENT: f32 = 0.08;

// ── the interactive stimuli (wave 2) ────────────────────────────────────────
//
// "The more dynamic and interactive it is, the more fun!" (owner), continued.
// Wave 2 is the pet PLAYING: it watches a pane that suddenly streams output.
// Frame-state stimuli arrive on [`PetSense`] (the sanctioned path for signals
// the host already computes per frame); event stimuli stay `note_*`. The
// same laws throughout: latch-don't-act, finite envelopes, injected time,
// mote-serial parity, never a die roll.

/// PERK-AND-WATCH: how fast the watch heat rises (per second) while the
/// focused pane is genuinely streaming ([`PetSense::output_burst`]). ~0.14 s
/// of sustained output crosses [`WATCH_GATE`] — a burst, not a keystroke.
const WATCH_RISE: f32 = 2.5;
/// How fast the watch heat decays (per second) once the stream pauses. Slower
/// than the rise: a stuttering build holds the pet's attention between
/// chunks instead of strobing the perk.
const WATCH_FALL: f32 = 1.2;
/// Heat at which a settled pet turns toward the stream and watches (enters
/// Perk, held tick-side while the heat stays above the gate — deliberately
/// NOT an `on_move` re-entry, so the Perk-pin law is untouched).
const WATCH_GATE: f32 = 0.35;
/// Hard cap on one continuous watch (seconds). Past it the pet drops to the
/// sit ladder with its gaze parked toward the output — a settled still
/// sticker — and cannot re-watch until the heat has fallen back under the
/// gate. LAW: this is well under [`SLEEP_AFTER`], so the watch always dies
/// inside the frame-lane window and idle-to-zero holds.
const WATCH_MAX: f32 = 8.0;

/// POINTER PLAY tier (a), the gaze-follow: the pointer must genuinely WANDER
/// — at least [`GAZE_FOLLOW_MIN`] cells of travel per [`GAZE_FOLLOW_WINDOW`]
/// seconds — before a settled pet's eyes track it. A parked pointer, or one
/// nudged a pixel by a bumped desk, is furniture.
const GAZE_FOLLOW_MIN: f32 = 2.0;
const GAZE_FOLLOW_WINDOW: f32 = 0.3;
/// Pointer-attention rise (per second) while the pointer wanders. The spec
/// names only the fall; the rise is chosen conservatively — twice the fall,
/// so attention arrives in about a third of a second and leaves in under
/// two thirds, same shape as the watch heat.
const POINTER_RISE: f32 = 3.0;
/// Pointer-attention decay (per second) once the pointer stops. Heat at
/// zero parks the gaze back on the caret and releases the frame lane.
const POINTER_FALL: f32 = 1.5;
/// POINTER PLAY tier (b), the pounce: the pointer's smoothed speed (EMA over
/// [`DASH_WINDOW`] seconds) must hold at least [`DASH_SPEED`] cells/s for
/// [`DASH_MIN_T`] seconds — a real DASH, the toy-on-a-string gesture, never
/// a flick.
const DASH_SPEED: f32 = 40.0;
const DASH_WINDOW: f32 = 0.25;
const DASH_MIN_T: f32 = 0.35;
/// Only a HAPPY cat plays: the pounce additionally needs this much earned
/// contentment on the ledger. A cold cat watches the dasher and stays put.
const PLAY_CONTENT: f32 = 0.5;
/// And the dasher must be worth the trip: at most this many columns away
/// when the dash matures.
const POUNCE_RANGE: f32 = 20.0;
/// The post-pounce flourish: a frolic beat on the landing before the chase
/// ladder walks the pet home (earning content per cell, as ever).
const POINTER_PLAY_HOLD: f32 = 0.9;

// ── POINTER PURSUIT (wave 3) ────────────────────────────────────────────────
// Owner, 2026-08-10: "the kitty should chase the mouse cursor, but 'home
// base' and the primary focus is the typing cursor." The dash pounce answers
// a FLICK; the pursuit answers a TEASE — a toy moving at real-but-under-dash
// speed near a playful cat earns a RUNNING chase on the existing follower
// controller. Work outranks play at every door: entry needs caret quiet, and
// any caret-travel latch drops the chase mid-stride.

/// How far (column-equivalents; rows count double) the teasing toy may be.
const PURSUIT_RANGE: f32 = 24.0;
/// The teasing band's floor, cells/s: slower is the stakeout's business,
/// [`DASH_SPEED`] and past belongs to the pounce.
const PURSUIT_MIN_SPEED: f32 = 8.0;
/// Attention needed before the cat commits paws — the gaze tier watches for
/// free; a chase costs dignity.
const PURSUIT_HEAT: f32 = 0.6;
/// The caret must have been quiet this long. Home base outranks the toy.
const PURSUIT_QUIET: f32 = 1.0;
/// Stamina: a chase this long without a catch breaks off — and grooms.
const PURSUIT_MAX_T: f32 = 5.0;
/// Cooldown after a break-off or a catch before the next chase can begin.
const PURSUIT_COOL: f32 = 6.0;
/// Row hysteresis while pursuing ([`WATCH_HUG_ROWS`]' law, the toy's
/// edition): the chase hops rows only when the toy clearly changed lines.
const PURSUIT_HUG_ROWS: f32 = 2.0;
/// THE CATCH: within this reach of a slowed toy the chase ends in the bat
/// visit (the swipe-at-the-toy the peek machinery already knows how to do).
const PURSUIT_CATCH: f32 = 2.5;
/// THE STAKEOUT: a creeping toy inside this range pins a settled cat into
/// the hunting crouch; the strike stays the dash pounce's to make.
const STALK_RANGE: f32 = 12.0;
/// The creep band's floor, cells/s — under this the pointer is parked, not
/// prey (creeps never build gaze heat, so the stakeout gates on the band
/// directly).
const STALK_MIN_SPEED: f32 = 1.0;
/// The stakeout's patience: a crouch this long without a strike stands
/// down (with a short cool so the same creep cannot re-pin it instantly).
const STAKEOUT_MAX: f32 = 6.0;
/// Post-chase dignity: the groom hold after a break-off (cats groom when
/// embarrassed; an owed groom is consumed at the next arrival).
const GROOM_HOLD: f32 = 1.2;
/// A peek beyond [`BAT_RANGE`] but inside this earns the LOOK tier: the pet
/// perks and faces the head — noticing from afar, never abandoning the
/// caret for scenery.
const LOOK_RANGE: f32 = 24.0;
/// The look's TTL — the same honesty as [`BAT_TTL`]: a peek the pet's work
/// outlasted is not news any more.
const LOOK_TTL: f32 = 1.5;

// ── wave 4: motion comedy (owner, 2026-08-10: variations in how it moves,
// and funny actions when the cursor runs forward and backward quickly) ──────

/// THE TENNIS WATCH: a SECOND frolic earned within this window of the
/// last one sits the cat down to watch the rally — the facing ping-pongs
/// with every caret move until the rally lapses.
const TENNIS_AFTER: f32 = 2.0;
/// The rally is over this long after its last hit; the watch stands down.
const TENNIS_LAPSE: f32 = 1.2;
/// THE SCRAMBLE: past this chase speed (cells/s) the gallop is a scramble —
/// every few strides one paw slips (a one-beat squash and a dust puff).
const STUMBLE_SPEED: f32 = 20.0;
/// The slip's squash beat, seconds.
const STUMBLE_DUR: f32 = 0.12;
/// THE DRIFT-BRAKE: an arrival that closed at least this many columns at a
/// gallop overshoots by [`BRAKE_OVERSHOOT`] and trots back — the zoomie's
/// signature failure to stop.
const BRAKE_DIST: f32 = 12.0;
const BRAKE_OVERSHOOT: f32 = 2.0;
/// THE SKIP: a content cat's walk bobs one stride in four — a skip, not a
/// march.
const SKIP_CONTENT: f32 = 0.6;

// ── wave 4b: the bored-cat vignettes (owner: bat the cursor, roll around,
// playfully attack the cursor) ──────────────────────────────────────────────

/// Boredom's window opens here — after the groom, well before sleep — and a
/// content cat in it demands attention from the nearest thing that blinks:
/// your cursor.
const BORED_AFTER: f32 = 8.0;
/// The window closes early enough that a cat winding down to sleep is left
/// alone to do it.
const BORED_UNTIL: f32 = SLEEP_AFTER - 4.0;
/// One vignette per cooldown: a treat, not a tic.
const BORED_COOL: f32 = 24.0;
/// THE WRIGGLE: seconds of rolling around on its back (the roll is faked
/// with the sleep/loaf frames flip-flopping — a true roll cycle is authored
/// art, queued for the rig).
const WRIGGLE_DUR: f32 = 2.2;
/// The wriggle's flip-flop beat.
const WRIGGLE_BEAT: f32 = 0.35;
/// HIDE-BEHIND-WORDS: how far (cols) the pet will walk to duck behind a
/// word on its own row.
const HIDE_RANGE: f32 = 10.0;
/// How long it lurks back there before strolling home.
const HIDE_DWELL: f32 = 3.5;
/// One pounce per dash: the trigger re-arms only after the pointer heat has
/// decayed under this — a long drag cannot machine-gun the cat.
const POUNCE_REARM: f32 = 0.1;

/// THE WORD-CAT BAT: an ambient peek landing within reach gets batted.
/// Range gate at NOTE time, in columns (rows count double — cells are about
/// half as wide as they are tall, so a row is two columns of travel): a far
/// peek is scenery, not a toy.
const BAT_RANGE: f32 = 10.0;
/// The approach stops this many cells short of the peek — the swipe needs a
/// forepaw's reach, not a body slam (the peek itself DUCKS via the existing
/// companion-yield law the moment the pet's body arrives).
const BAT_STANDOFF: f32 = 1.5;
/// How long a noted peek stays actionable. Sightings fire once per episode
/// at the LANDING, but the head dwells seconds longer — this window lets a
/// mid-chase pet finish its work and still catch the toy, without batting
/// at a spot the head left long ago.
const BAT_TTL: f32 = 1.5;
/// The swipe: a Frolic-held beat in the bat pose at the standoff, one Dust
/// mote at the peek cell (a neutral puff — the fake-glyph law bars anything
/// that could read as terminal output).
const BAT_HOLD: f32 = 0.6;

/// BREED HANDOFF: a look sync parked mid-appearance ([`PetBrain::sync_look`])
/// must have stayed STABLE this long before the handoff fires. The host's
/// tenure gate already keeps the verdict from flapping (a program earns the
/// cursor by holding the pane for seconds; the cat lingers after it exits),
/// so this is the second guard: a host syncing a flapping verdict must never
/// turn the pet into a metronome of ghost bodies — short flips clear the park
/// (and its clock) long before this.
const HANDOFF_DEBOUNCE: f32 = 2.5;
/// The departing body's tail fade (seconds) — deliberately faster than
/// [`FADE_OUT`]: the old kitty is EXITING on purpose, not dissolving. The
/// fade covers the last stretch of the run (or the whole visit, when the
/// body spawns already at its edge).
const EDGE_FADE: f32 = 0.25;
/// The departing body's run speed toward its exit edge (cells/sec) — a
/// purposeful sprint between [`RUN_SPEED`] and [`MAX_SPEED`]: fast enough to
/// be gone in about a second on an ordinary grid, slow enough that the run
/// cycle still reads as feet. Since the exit-at-full-opacity wave this is
/// the FLOOR of a per-departure [`Departure::speed`]: a pane whose exit
/// span outruns `DEPART_SPEED * (DEPART_MAX - EDGE_FADE)` solves a faster
/// sprint at spawn instead, because the door is a promise (see
/// [`DEPART_MAX`]).
const DEPART_SPEED: f32 = 18.0;
/// Hard ceiling on a departing body's life (seconds) — the departure is
/// strictly finite BY CONSTRUCTION, so it can never pin `needs_frames`
/// past its own window (the idle-to-zero law). Raised 2.0 → 4.0 (THE DOC's
/// rung 2): at 2.0 the reachable run was 36 cells, so the §1.1(b) worst
/// ordinary case (80 cols, span 63.4) dissolved in open ground at about
/// column 47 — the owner's "the running off screen was better" complaint,
/// literally drawn. 4.0 covers that span at [`DEPART_SPEED`]; wider spans
/// keep the cap and raise the departure's own speed at spawn instead, so
/// THE GHOST ALWAYS REACHES ITS DOOR. ⚠ This is now LONGER than
/// [`HANDOFF_DEBOUNCE`] (2.5): a ghost can outlive the spacing between
/// births, so the emitter's per-slot held tiles are keyed by the
/// departure's `born` stamp (`word_decorations::pet_depart_tiles`) —
/// re-widen that key before ever raising this past the debounce again.
const DEPART_MAX: f32 = 4.0;
/// THE ARRIVAL: the incoming kitty of a breed handoff fades UP over this long
/// (owner, 2026-08-17: "switching the cats all the time is too abrupt" — the
/// old handoff recoloured the standing pet in one frame). Longer than
/// [`FADE_IN`] on purpose: this is a change of character, not a first
/// sighting, and it overlaps the departing body's run so for a moment the
/// old cat is leaving while the new one is still faint — a handover, not a
/// cut. Finite by construction, so `needs_frames` releases after it.
const ARRIVE_IN: f32 = 0.9;
/// THE RE-DRESS ramp (THE DOC §2.0.6): a HOMECOMING — a cat this window
/// already owns — fades up over this long instead of [`ARRIVE_IN`], while
/// its old coat fades OUT in place as a zero-span, `still` departure.
/// LAW: `HOMECOMING_IN == EDGE_FADE`, and the reason is SYMMETRY, not
/// conservation. A zero-span [`Departure`]'s `life()` is exactly
/// `EDGE_FADE` by the shipped arithmetic ([`Departure::life`]), so equal
/// ramps make the two envelopes complementary (`1 − t/T` against `t/T`)
/// and the crossfade never dips further than it must. The bodies are
/// separate sprites composited alpha-over — NOT a conserved ink sum — so
/// the true minimum is ~75% coverage at the midpoint, one cat, mid-colour
/// (the candidate law's "never a dip" claim was a sum, and was corrected).
/// A future [`EDGE_FADE`] tune must move both, or the dissolve dips
/// deeper than it needs to.
const HOMECOMING_IN: f32 = EDGE_FADE;
/// The arrival ramp's floor: the incoming kitty's alpha on the landing frame
/// (× 255 ≈ 15) — faint, but on glass, so hosts that gate the pet (and its
/// departing body) on `alpha > 0` never see a blank frame at the swap.
const ARRIVE_FLOOR: f32 = 0.06;

/// THE CAP on concurrent departing bodies — the documented "multiple kitties
/// per screen" bound. Rapid tab cycling may layer departures; past this many
/// the OLDEST is dropped (its story was nearly over anyway), never the
/// newest. Three is deliberate: with the [`HANDOFF_DEBOUNCE`] pacing swaps,
/// even pathological churn never needs more, and the cap is the defensive
/// law that keeps the lane bounded whatever future seams spawn from.
pub const PET_DEPARTURES_MAX: usize = 3;

/// IDLE MICRO-LIFE, constraint-first: everything here lives INSIDE the
/// existing lane-hot windows, so idle-to-zero survives by construction.
/// The LOAF's tail thump: a quiet-phased beat every this many seconds (the
/// `tend_motes` beat-index scheme — a pure function of THIS settle, never
/// a wall clock), each thump [`FLICK_DUR`] long. The sit's own flick
/// (`PetSitFlick`) predates this wave and keeps its shipped cadence; the
/// loaf had no life at all until now.
const FLICK_EVERY: f32 = 3.0;
const FLICK_DUR: f32 = 0.15;
/// The ear twitch: an output PULSE (a burst rising edge) at a settled pet
/// still below [`WATCH_GATE`] flicks one ear — shipped art-free as a
/// [`TWITCH_BOB`] procedural head-scale bob (the flinch envelope's
/// precedent), upgraded to real ear frames when the art wave lands them.
const TWITCH_DUR: f32 = 0.12;
const TWITCH_BOB: f32 = 0.02;

// ── fade ────────────────────────────────────────────────────────────────────

/// Appear/disappear ramps (seconds). The pet fades in when the caret becomes
/// visible and out when it goes away, so a hidden cursor never leaves a cat
/// stranded on screen.
const FADE_IN: f32 = 0.30;
const FADE_OUT: f32 = 0.45;

/// How long after falling asleep the pet keeps breathing before going
/// completely still — the **idle-to-zero** law, which this crate holds as
/// structural (`lib.rs`: "`is_active()` reports when nothing is animating; the
/// host returns to 0% idle").
///
/// A cat that breathes forever is a 60 fps wake train forever, on a window where
/// nothing is happening and nobody is looking. So the settled animations — the
/// tail flick, the wash, curling up, and the breath itself — all play out over
/// the window that starts when you stop typing and ends here, and then the pet
/// is a still sleeping sticker until you touch the keyboard. Everything a user
/// could actually watch happens inside it; what is given up is the frame nobody
/// sees.
const BREATH_WINDOW: f32 = 10.0;

/// The pet's authored art box, in cells: the roster's 232×136 viewbox at the
/// [`ART_ROWS`] height the host bakes it to. Kept here (not in the art) because
/// it is a *layout* fact the brain needs to know before any tile exists — the
/// station, the wall check, and the clamp are all in units of it.
pub const ART_ROWS: f32 = 1.70;
/// Art aspect (w/h) of every pet pose. All 23 frames share one viewbox by
/// construction — the roster is a swap-in-place animation set — so this is a
/// constant rather than a per-frame lookup. Pinned to the assets by
/// `pet_art_quality`'s viewbox check, so it cannot drift from the art.
pub const ART_ASPECT: f32 = 244.0 / 148.0;
/// The pet's width in cells, given a cell aspect. Cells are taller than they are
/// wide (~0.5), so the pet is a little under three columns across.
#[must_use]
pub fn art_cols(cell_w: u16, cell_h: u16) -> f32 {
    if cell_w == 0 || cell_h == 0 {
        return 3.0;
    }
    ART_ROWS * ART_ASPECT * f32::from(cell_h) / f32::from(cell_w)
}

/// Cap on live pet motes — the pet's ONE small particle lane (landing dust
/// now; sleep z's and purr notes/hearts ride the same slots). Tiny by design:
/// these are accents on an animal, not a particle system.
pub const PET_MOTES_MAX: usize = 4;

/// What a pet mote is drawn as. One lane, few costumes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PetMoteKind {
    /// Landing dust at the heels of a screen-crossing jump — the delete
    /// poof's neutral puff, at pet scale.
    Dust,
    /// A light-sleep z, drifting up and away from the sleeper's head.
    Zee,
    /// The startled z popped by waking — bigger, faster, gone in a blink.
    ZeePop,
    /// A floating ♪ from a purring chest.
    Note,
    /// A floating heart from a purring chest.
    Heart,
}

/// One resolved mote for this frame, in fractional grid CELLS (centre
/// coordinates). THE LAWS: motes are free-floating — never grid-aligned,
/// never text-colored (the fake-glyph hazard) — and always rotated, scaling
/// and alpha-fading, so nothing in this lane can ever read as a glyph the
/// terminal emitted.
#[derive(Clone, Copy, Debug)]
pub struct PetMoteSprite {
    pub kind: PetMoteKind,
    /// Mote centre, fractional columns/rows from the grid origin.
    pub col: f32,
    pub row: f32,
    /// Scale of the emitter's natural mote tile (grows over life).
    pub scale: f32,
    /// Rotation in radians — nonzero by construction.
    pub rot: f32,
    /// 0..=255 fade envelope; the emitter multiplies the pet's own alpha in.
    pub alpha: u8,
}

/// One live mote's birth record (brain-side; sprites are derived per frame).
#[derive(Clone, Copy, Debug)]
struct Mote {
    kind: PetMoteKind,
    /// `PetBrain::clock` at spawn, and how long it lives.
    born: f64,
    life: f32,
    /// Birth anchor (fractional cells) and horizontal drift direction.
    col: f32,
    row: f32,
    dir: f32,
    /// Deterministic scatter index (a serial, not a die roll).
    seed: u8,
}

/// One resolved DEPARTING BODY for this frame (the breed handoff's outgoing
/// kitty — see the module's handoff section). The emitter draws it exactly
/// like the live body — the same feet-anchored dest law via a synthetic
/// [`PetFrame`] through [`PetFrame::body_px`] — but wearing the OLD
/// `(coat, iris)` pair carried here, which is the whole point: the pet body's
/// look IS only those two ramp indices, so the ghost differs in nothing else.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PetDepartureSprite {
    /// The run-cycle frame, already species-skinned (a dog departs as a dog).
    pub pose: PetGlyphId,
    /// Left edge in fractional columns / feet row — [`PetFrame`] semantics.
    pub col: f32,
    pub row: f32,
    pub facing_left: bool,
    /// 0..=255 fade envelope; the emitter multiplies the pet's own
    /// PRESENCE (`PetFrame::lane_alpha`) in — never the arrival ramp,
    /// which belongs to the incoming body alone.
    pub alpha: u8,
    /// The OUTGOING look's ramp indices.
    pub coat: u8,
    pub iris: u8,
    /// The birth stamp of the [`Departure`] this sprite resolves — pure
    /// IDENTITY, not appearance (it never folds into [`PetFrame::fp`]).
    /// The emitter keys its per-slot held tiles on it: with
    /// [`DEPART_MAX`] (4.0) longer than [`HANDOFF_DEBOUNCE`] (2.5), a
    /// slot can be reborn while a stale held tile survives, and an
    /// unkeyed hold would dress the new ghost in the old ghost's pose.
    pub born: f64,
}

/// One live departing body's birth record (brain-side; the sprite is a pure
/// function of this record and `PetBrain::clock` — no per-frame mutation, no
/// wall clock, no randomness, exactly the [`Mote`] discipline).
#[derive(Clone, Copy, Debug)]
struct Departure {
    /// `PetBrain::lane_clock` at the swap instant — the LANE clock, not the
    /// free-running `clock`: the ghost is motion, and motion rides the dt
    /// clamp (the two-clocks law at `tick`'s prologue), so one long frame
    /// can never sprint a ghost a screen.
    born: f64,
    /// The look the pet was wearing when the handoff fired.
    coat: u8,
    iris: u8,
    /// Where the body spawned (the live pet's feet at that instant)…
    from_col: f32,
    row: f32,
    /// …and the exit edge column it runs to (the clamp limit or 0.0, chosen
    /// on the pet's side of the caret at spawn — the exit never crosses your
    /// cursor).
    edge: f32,
    /// This body's own run speed (cells/sec), solved ONCE at spawn:
    /// [`DEPART_SPEED`] floored, raised to `span / (DEPART_MAX − EDGE_FADE)`
    /// when the span outruns it — the door is a promise, so a wide pane
    /// buys a faster sprint, never a mid-screen dissolve. Carried here (not
    /// read from the const) so [`Self::life`] and the run arithmetic in
    /// `resolve_departures` stay consistent with each other for every
    /// departure a future lane may spawn at another pace.
    speed: f32,
    /// THE QUIET RE-DRESS's flag (THE DOC §2.0.6, mandatory groundwork): a
    /// still departure is a pure in-place fade — it keeps the carried
    /// `facing_left` and the captured `pose0` for its WHOLE life instead of
    /// the run-derived direction and run-cycle frame. Unfixed, a zero-span
    /// departure always resolved `dir = +1` (mirror-flipping a left-facing
    /// cat) and froze on run frame 0 (a gallop pose for the whole dissolve).
    /// Nothing spawns `true` yet — the re-dress wave will.
    still: bool,
    /// The live body's facing at spawn — honored only while `still` (a
    /// running ghost faces its door, which IS its direction of travel).
    facing_left: bool,
    /// The live body's gait phase (cycles) at spawn: the ghost's feet keep
    /// counting from HERE, so the first ghost frame continues the body's
    /// own stride instead of snapping to run frame 0 — the gallop-from-a-
    /// sit pop, killed at the source.
    stride0: f32,
    /// The live body's last emitted pose at spawn (pre-skin — the record
    /// stays species-blind, exactly like the run cycle it substitutes for).
    /// A `still` ghost wears it for its whole fade.
    pose0: PetGlyphId,
}

impl Departure {
    /// Seconds this visit lasts: the run to the edge plus the tail fade,
    /// hard-capped at [`DEPART_MAX`]. With [`Self::speed`] solved at spawn
    /// the cap never truncates a reachable run — it is the belt-and-braces
    /// bound that keeps the visit strictly finite whatever a future spawn
    /// site passes.
    fn life(&self) -> f32 {
        ((self.edge - self.from_col).abs() / self.speed + EDGE_FADE).min(DEPART_MAX)
    }
}

/// How a parked look is owed to LAND — the host's homecoming verdict,
/// carried with the park (THE DOC §2.0: the ceremony is committed where it
/// RENDERS, not where it is authorised, so the host hands the verdict to
/// [`PetBrain::sync_look`] and the debounced handoff performs it). The
/// verdict rations the THEATER only, never the costume: both variants land
/// the same `(coat, iris)`, at the same debounce, on the same ladder rung.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PetArrival {
    /// The full theater, owed to a stranger — a cat this window has never
    /// worn (or not within the host's homecoming window): the incoming
    /// body fades up over [`ARRIVE_IN`] while the old coat RUNS off as a
    /// departing body.
    Ceremony,
    /// THE RE-DRESS (THE DOC §2.0.6), owed to a cat coming home: a
    /// [`HOMECOMING_IN`] (0.25 s) in-place crossfade — the old coat fades
    /// out as a zero-span `still` departure at the pet's own feet, at zero
    /// displacement of either body. Not a recolour, not a run, not
    /// nothing.
    Quiet,
}

/// What [`PetBrain::sync_look`] did with one frame's verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncLookOutcome {
    /// The pair the appearance is WEARING after the call — the latch the
    /// host must draw, never the verdict it just passed.
    pub worn: (u8, u8),
    /// True exactly when THIS call parked a genuinely differing pair (a
    /// fresh park, or a park moved to a different pair). The host commits
    /// its homecoming bookkeeping (the greeted bit, the hello floor) on
    /// this edge; an agreeing per-frame re-sync of an already-parked pair
    /// reports `false`, so re-syncing every emission can never restamp
    /// the floor for one performance.
    pub parked: bool,
}

/// What the pet is doing. The art frame for a given action comes from
/// [`PetFrame::pose`]; this is the decision, not the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PetAction {
    /// Curled up. Breathing, not chasing.
    Sleep,
    /// The one-shot wake-up stretch on the way out of [`Self::Sleep`].
    Waking,
    /// Settled on its haunch by the caret.
    Sit,
    /// The LONG dwell: legs tucked, low profile — the sit melted down.
    Loaf,
    /// Settled and content — a sit with the eyes shut and the chest working.
    Purr,
    /// A sit interrupted by a wash.
    Groom,
    /// Ears up, head high: it has noticed the caret is somewhere else.
    Perk,
    /// On its feet by the caret: arrived, but not yet folded into the sit.
    Stand,
    /// Closing a small gap on foot.
    Walk,
    /// Closing a large gap at speed.
    Run,
    /// Gathered, about to leave the ground.
    Crouch,
    /// Airborne on a pounce or a row hop.
    Leap,
    /// The braced, compressed frame that absorbs a landing.
    Land,
    /// Flinched away from a retreating caret.
    Startle,
    /// Playing: the caret is being fussed with rather than driven.
    Frolic,
    /// The sulk: a command failed, and the pet takes it personally — body
    /// low, feet planted, zero motes. Borrows the loaf art until the art
    /// wave lands a flat-ears frame.
    Droop,
}

impl PetAction {
    /// True while the pet is settled — the states a caret move interrupts.
    #[must_use]
    pub fn settled(self) -> bool {
        matches!(
            self,
            Self::Sleep
                | Self::Sit
                | Self::Loaf
                | Self::Purr
                | Self::Groom
                | Self::Perk
                | Self::Stand
        )
    }

    /// True while the pet is off the ground.
    #[must_use]
    pub fn airborne(self) -> bool {
        matches!(self, Self::Leap)
    }
}

/// What the host observed this frame. The pet is its OWN move sensor: it diffs
/// the caret cell itself rather than subscribing to the cursor-glow classifier,
/// which is gated on the trail style, on `cfg.enabled`, on `intensity > 0`, and
/// is only ticked for the focused single-pane path. A pet that stopped behaving
/// because a *trail* was turned down would be a bug with no explanation.
#[derive(Clone, Copy, Debug)]
pub struct PetSense {
    pub now: Instant,
    /// The visible caret cell, or `None` when the cursor is hidden or scrolled
    /// out of view.
    pub caret: Option<(u16, u16)>,
    /// The emulator wrapped the caret since the last host read; a fact from
    /// the grid, never a heuristic. It is the ONLY signal that separates the
    /// bottom-row scrolled wrap — where the caret's row never changes, so the
    /// delta reads as a full-width retreat — from `Home` pressed at the last
    /// column, which looks byte-identical on the grid. `caret_delta`'s
    /// fact-gated arm folds it to the one character the text really moved,
    /// which is what stops the full-bottle Startle on every line a shell
    /// prints past the margin. A host that cannot supply the fact passes
    /// `false` and keeps the `dr == ±1` heuristic fold, which still catches
    /// every typing wrap.
    pub wrapped: bool,
    pub rows: u16,
    pub cols: u16,
    pub cell_w: u16,
    pub cell_h: u16,
    /// Reduced motion: no chase, no gait, no arcs — the pet simply sits at its
    /// station and the frame settles.
    pub reduced_motion: bool,
    /// The focused pane is genuinely STREAMING this frame (wave 2, the
    /// perk-and-watch): the host computes it from per-frame state it already
    /// holds — `(scrolled_rows > 0 || content_seq advanced) && shell
    /// executing && live bottom`. The AND with the shell's OSC 133/633
    /// Execute phase is load-bearing: keystroke echo advances the content
    /// clock too, and a pet that perked at your own typing would never stop.
    pub output_burst: bool,
    /// The mouse pointer in fractional grid cells `(col, row)` of the pet's
    /// pane, `None` when it is outside that pane (wave 2, pointer play). The
    /// brain diffs it itself — the own-sensor doctrine — so the host only
    /// converts pixels to cells and never classifies motion.
    pub pointer: Option<(f32, f32)>,
}

/// One frame's resolved pet.
#[derive(Clone, Copy, Debug)]
pub struct PetFrame {
    /// Opacity 0..=255 (0 ⇒ nothing to draw).
    pub alpha: u8,
    /// THE LANE'S OPACITY: the presence envelope alone, 0..=255 — [`Self::alpha`]
    /// WITHOUT the arrival factor folded in. `alpha` carries presence ×
    /// arrival for the LIVE body; the departure lane composites against
    /// THIS byte instead, because the ghost is not the arriver and must not
    /// wear the arriver's ramp (§1.1(a)'s shipped bug: both cats starting
    /// at 6% on the swap frame). Invariant, by construction at every
    /// emission: `lane_alpha >= alpha` (arrival ≤ 1), so `lane_alpha == 0`
    /// still means nothing at all is on glass — which is why [`Self::fp`]
    /// and the emitter's early return key on it.
    pub lane_alpha: u8,
    pub action: PetAction,
    /// The authored art frame to bake for this present.
    pub pose: PetGlyphId,
    /// The pet's LEFT edge, in fractional columns from the grid's left edge.
    pub col: f32,
    /// The row the pet's feet are on, fractional while hopping between lines.
    pub row: f32,
    /// Extra height above the ground line, in cell heights (`+` = up). Non-zero
    /// only while airborne; the row itself never carries the arc, so a landing
    /// is always exactly on a baseline.
    pub lift: f32,
    /// Draw mirrored: the art is authored facing right.
    pub facing_left: bool,
    /// Dest-rect scale about the sprite centre (squash & stretch).
    pub scale_x: f32,
    pub scale_y: f32,
    /// Purr intensity 0..=1 — the settled contentment, exported so a host that
    /// can render faster than the body swell (audio, a glow pulse) has the real
    /// signal rather than the animation's stand-in.
    pub purr: f32,
    /// HIDE-BEHIND-WORDS (wave 4c): draw the pet UNDER the glyphs this
    /// frame — the word in front stays readable, the cat peers out from
    /// behind it (the ink law's under-exception).
    pub under_ink: bool,
    /// The pet's mote lane, resolved for this frame (dust, z's, notes) —
    /// `None`-padded, bounded, allocation-free. Rides the frame so both host
    /// render paths carry it with zero extra plumbing.
    pub motes: [Option<PetMoteSprite>; PET_MOTES_MAX],
    /// The BREED HANDOFF's departing bodies, resolved for this frame —
    /// `None`-padded, capped at [`PET_DEPARTURES_MAX`]. Riding the frame (the
    /// mote lane's precedent) is what makes every consumer honest for free:
    /// the single-pane present, the composed present and both capture splices
    /// all draw whatever the frame carries, so a ghost can never exist on one
    /// surface and not another.
    pub departures: [Option<PetDepartureSprite>; PET_DEPARTURES_MAX],
}

impl PetFrame {
    /// Quantized fingerprint for the host's repaint key: non-zero while anything
    /// is animating, and byte-stable (so it settles) once nothing is.
    #[must_use]
    pub fn fp(&self) -> u64 {
        // Keyed on the PRESENCE byte, not the composed one: the arrival
        // multiply can truncate a faint body's `alpha` to 0 while its
        // ghosts are still live on the lane, and an `alpha`-keyed early-out
        // would freeze their exit. `lane_alpha == 0` implies `alpha == 0`
        // (the struct's invariant), so the settle contract is unchanged.
        if self.lane_alpha == 0 {
            return 0;
        }
        let q = |v: f32, s: f32| ((v * s) as i64) as u64;
        let mut h = 0xCBF2_9CE4_8422_2325u64;
        for value in [
            u64::from(self.alpha),
            u64::from(self.lane_alpha),
            self.action as u64,
            self.pose as u64,
            q(self.col, 64.0),
            q(self.row, 64.0),
            q(self.lift, 256.0),
            u64::from(self.facing_left),
            q(self.scale_x, 256.0),
            q(self.scale_y, 256.0),
            q(self.purr, 64.0),
        ] {
            h ^= value;
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
        // The mote lane animates while the body may be perfectly still (a
        // sleeping cat under drifting z's), so the motes MUST move the
        // fingerprint or the host's early-out swallows their frames.
        for m in self.motes.iter().flatten() {
            for value in [
                m.kind as u64,
                q(m.col, 64.0),
                q(m.row, 64.0),
                q(m.scale, 64.0),
                q(m.rot, 64.0),
                u64::from(m.alpha),
            ] {
                h ^= value;
                h = h.wrapping_mul(0x0000_0100_0000_01B3);
            }
        }
        // The departing bodies fold for the same reason: a ghost sprinting
        // off beside a settled pet is often the only change on glass, and
        // the early-out must not swallow its exit (the motes lesson).
        for d in self.departures.iter().flatten() {
            for value in [
                d.pose as u64,
                q(d.col, 64.0),
                q(d.row, 64.0),
                u64::from(d.facing_left),
                u64::from(d.alpha),
                u64::from(d.coat),
                u64::from(d.iris),
            ] {
                h ^= value;
                h = h.wrapping_mul(0x0000_0100_0000_01B3);
            }
        }
        h
    }

    /// The pet's LIVE drawn body this frame, in grid pixels — `(x0, x1, y0,
    /// y1)`, right/bottom exclusive: exactly the dest rect the emitter draws
    /// (`WordDecorations::pet_cursor`, which calls this so the two can never
    /// drift). Feet on the row baseline, the arc's lift folded in,
    /// squash/stretch about the feet, clamped to the grid on ALL FOUR sides.
    ///
    /// THE VERTICAL CLAMP IS NOT DECORATION, it is the containment law. This lane's
    /// dest rect is a `FreeSprite`, whose plane honours a SIGNED origin so a sprite
    /// may deliberately peek off-grid (`stamp_free_sprite`), and the tab-strip splice
    /// shifts every free sprite down by `strip_rows · cell_h`. The art is `ART_ROWS`
    /// (1.7) tall standing on its row's baseline, so a pet on terminal row 0 resolves
    /// a top edge ~0.7 cells ABOVE the grid — which after the splice is not empty
    /// padding but the TAB STRIP, and the cat's head paints over the tab titles.
    /// Clamping keeps the resident inside the terminal it lives in: on row 0 it
    /// stands a little low rather than climbing into the chrome. (Robi is the
    /// deliberate exception — he is opt-in, he has his own `robi::body_px`, and going
    /// up the ladder past the top is his whole act.)
    ///
    /// Hosts hand this rect to the word-cat engine as the companion's
    /// pixel-yield box (`CompanionOnGlass::body_px`): the one-cat-per-caret
    /// suppression collides SPRITES, and a resident animal that wanders whole
    /// rows away from the caret is nowhere near the caret-anchored band that
    /// models the flying head — without the real body, an ambient cat rises
    /// straight underneath it.
    ///
    /// `None` when nothing is on glass (`alpha == 0`) or the cell metrics are
    /// degenerate — the same frames the emitter draws nothing for.
    #[must_use]
    pub fn body_px(
        &self,
        cell_w: u16,
        cell_h: u16,
        cols: u16,
        rows: u16,
    ) -> Option<(i32, i32, i32, i32)> {
        if self.alpha == 0 || cell_w == 0 || cell_h == 0 {
            return None;
        }
        // Natural size from the authored art height — the same rounding the
        // emitter bakes with, so the box IS the sprite, not a model of it.
        let nat_h = (ART_ROWS * f32::from(cell_h)).round();
        let nat_w = (nat_h * ART_ASPECT).round();
        let nat_h = (nat_h as i32).clamp(1, i32::from(u16::MAX));
        let nat_w = (nat_w as i32).clamp(1, i32::from(u16::MAX));
        // Dest rect: squash/stretch about the FEET (a landing compresses down
        // onto the floor; scaling about the centre would sink it through the
        // line), horizontally centred on the natural box.
        let dest_w = ((nat_w as f32 * self.scale_x).round() as i32).clamp(1, i32::from(u16::MAX));
        let dest_h = ((nat_h as f32 * self.scale_y).round() as i32).clamp(1, i32::from(u16::MAX));
        let grid_w = i32::from(cols).saturating_mul(i32::from(cell_w));
        let grid_h = i32::from(rows).saturating_mul(i32::from(cell_h));
        let cx = (self.col * f32::from(cell_w)).round() as i32 + nat_w / 2;
        let baseline = ((self.row + 1.0) * f32::from(cell_h)).round() as i32
            - (self.lift * f32::from(cell_h)).round() as i32;
        let x0 = (cx - dest_w / 2).clamp(0, (grid_w - dest_w).max(0));
        // The y twin of the x clamp on the line above — see the doc note: a free
        // sprite that leaves the grid vertically lands in the tab strip, not in
        // empty padding.
        let y0 = (baseline - dest_h).clamp(0, (grid_h - dest_h).max(0));
        Some((x0, x0 + dest_w, y0, y0 + dest_h))
    }
}

/// What a PLAY flight owes on landing (wave 2). Play flights ride the normal
/// Crouch → Leap → Land pipeline toward a non-caret target; this is the one
/// extra beat between the landing recovery and the walk home.
#[derive(Clone, Copy, Debug)]
enum PlayLand {
    /// A pounced pointer-dasher: frolic for [`POINTER_PLAY_HOLD`].
    PointerFrolic,
    /// A batted word-cat peek: face the head at `(col, row)`, swipe (the
    /// bat pose) for [`BAT_HOLD`], and puff one Dust mote at the peek cell.
    BatSwipe { col: f32, row: f32 },
}

/// The airborne phase of a pounce or hop.
#[derive(Clone, Copy, Debug)]
struct Flight {
    from_col: f32,
    from_row: f32,
    to_col: f32,
    to_row: f32,
    /// Seconds of flight.
    dur: f32,
    /// Peak arc height in cell heights.
    arc: f32,
    /// Elapsed seconds.
    t: f32,
    /// The one mid-air re-aim has been spent. A large caret move while
    /// airborne may retarget `to_col` ONCE — never the duration (the flight's
    /// clock is the law), and never twice (one correction per leap keeps a
    /// flight finite under a held word-jump).
    reaimed: bool,
    /// A screen-crossing bound: lands with the heavy recovery (weighted
    /// squash, skid, dust) instead of the pounce's.
    big: bool,
}

/// Which animal the pet is drawn as.
///
/// A SKIN, NOT A SECOND ANIMAL. The behaviour layer below is written entirely
/// against the CAT pose vocabulary — the walk's four beats, the settle ladder,
/// the leap's three frames — and every species' roster is generated from the
/// same rig with the same pose idents (`art/pet/poses.py` emits `pet_walk_0`
/// and `pet_dog_walk_0` from one `Pose`). So a species never changes what the
/// pet DOES, only which sprite that decision resolves to, and the two rosters
/// can never drift out of gait sync.
///
/// Adding a third animal is a new variant here, a new prefix in `poses.py`, and
/// nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum PetSpecies {
    /// The original companion (`cursor_trail_style = "rainbow kitty pet"`).
    #[default]
    Cat,
    /// The dog (`cursor_trail_style = "rainbow dog pet"`).
    Dog,
}

impl PetSpecies {
    /// Map a CAT pose onto this species' equivalent frame.
    ///
    /// Resolution is by pose IDENT, through the generated roster's `id`
    /// strings — never by enum discriminant arithmetic, which would silently
    /// re-point every pose the moment a new asset lands in the directory and
    /// shifts the sort order. A species with no counterpart for some pose
    /// falls back to the cat frame rather than drawing nothing.
    #[must_use]
    pub fn skin(self, pose: PetGlyphId) -> PetGlyphId {
        let Self::Dog = self else { return pose };
        let cat_id = PET_GLYPHS[pose as usize].id;
        // Already a dog frame (a re-skin of an already-skinned pose is a no-op).
        if cat_id.starts_with("pet_dog_") {
            return pose;
        }
        let Some(tail) = cat_id.strip_prefix("pet_") else {
            return pose;
        };
        PET_GLYPH_IDS
            .iter()
            .copied()
            .find(|id| {
                PET_GLYPHS[*id as usize]
                    .id
                    .strip_prefix("pet_dog_")
                    .is_some_and(|t| t == tail)
            })
            .unwrap_or(pose)
    }
}

/// The pet's decision layer: a caret follower with a behaviour state machine on
/// top. `tick` is the whole API; everything else is derived from it.
pub struct PetBrain {
    /// Which animal to draw. Pure presentation — see [`PetSpecies`]; the
    /// behaviour below never reads it, and `emit` is the only consumer.
    species: PetSpecies,
    /// Position of the pet's LEFT edge / feet, in fractional cells.
    col: f32,
    row: f32,
    /// Signed horizontal speed (cells/sec), eased toward the chase demand.
    speed: f32,
    facing_left: bool,

    action: PetAction,
    /// Seconds spent in the current action (for the one-shot holds).
    action_t: f32,
    /// Seconds since the caret last moved.
    quiet: f32,
    /// Gait phase in cycles, advanced by DISTANCE travelled.
    stride: f32,
    /// Free-running seconds, for the time-driven idle animations only.
    clock: f64,
    /// THE LANE CLOCK: seconds advanced by the dt clamp (`elapsed.min(0.10)`)
    /// rather than the raw elapsed that feeds `clock`. The departure lane is
    /// MOTION, and motion rides dt (the two-clocks law at `tick`'s
    /// prologue): a hidden window, a breakpoint or a suspended machine must
    /// move a ghost by at most `speed × 0.10` on the first frame back, never
    /// a screenful. [`Departure::born`] is stamped against THIS clock;
    /// `clock` keeps the quiet/animation semantics for everything else.
    lane_clock: f64,

    /// The caret cell as of the previous tick — the pet's own move sensor.
    last_caret: Option<(u16, u16)>,
    last_now: Option<Instant>,

    flight: Option<Flight>,
    /// A flight destination retired while the caret was withheld. The visible
    /// fading body stays at its last authored position; this landing is applied
    /// only once the fade reaches zero. A caret that returns sooner cancels it
    /// and lets the ordinary follower walk from the still-visible body.
    deferred_hidden_landing: Option<(f32, f32)>,
    /// Retired flight lift plus its per-second settle rate. Keeping this as a
    /// render offset preserves top-row arcs whose visible feet are above row
    /// zero; folding that offset into `row` would be undone by `emit`'s lawful
    /// grounded-row clamp.
    retired_flight_lift: Option<(f32, f32)>,
    /// Seconds remaining in the landing recovery.
    land_t: f32,
    /// The NEXT crouch is the Enter hop's one-tick anticipation: gathered this
    /// frame, airborne the next. Distinct from the pounce's timed crouch,
    /// which launches through its span-scaled coil ([`PetBrain::coil_dur`])
    /// into a horizontal flight.
    hop_crouch: bool,
    /// The pounce coil's live length (seconds): [`PetBrain::coil_dur`] of the
    /// span being wound up for, written at the coil's entry and refreshed by
    /// the launch arm from the live target. Read by `emit`'s envelope for the
    /// PLAIN pounce coil ONLY — the big gather and the Enter hop derive their
    /// denominators from their own KIND at emit time (§3's staleness law: a
    /// write-once pounce field would hold 64% of the butt-wiggle at full
    /// [`COIL_SQUASH`]).
    coil_d: f32,
    /// Seconds remaining in the small-retreat flinch's squash envelope.
    flinch_t: f32,

    /// Direction-reversal detector for the frolic.
    last_dir: i8,
    reversals: u8,
    reversal_t: f32,
    /// EMA caret velocity v̂ (cells/s) — the keep-ahead estimate, see
    /// [`VEL_TAU`].
    vhat: f32,
    /// The rhythm run (the lead's gate): direction of the current
    /// same-direction move run, moves in it, and seconds since its last move.
    run_dir: i8,
    run_count: u8,
    run_t: f32,
    /// Parked on the caret's LEFT (the wall side). Hysteretic — crossed only
    /// when the led station hangs off the right margin, returned only when it
    /// clears the margin by [`WALL_RETURN_MARGIN`].
    wall_side: bool,
    /// The station flipped sides this tick: hop the caret column rather than
    /// walking through it. Latched like `pending_pounce` (a flight in
    /// progress finishes first) and consumed in the chase.
    pending_wall_transit: bool,
    /// BBOX ANCHOR SEAM (grid pixels): how far the CURRENT pose's visible
    /// left edge sits inside the authored tile. The review wants the visible
    /// edge at caret+1, which needs the baker's per-pose body bbox — a file
    /// the art wave owns right now — so the seam ships with a documented
    /// default of 0 (anchor on the authored box) and the export lands later.
    /// See [`Self::set_body_left_px`].
    body_left_px: f32,
    /// Held-delete detector: retreats in the current run, and seconds since
    /// the last one (a retreat within [`STARTLE_RUN_WINDOW`] continues the
    /// run, a later one starts a fresh run of one).
    retreat_run: u8,
    retreat_gap: f32,
    /// THE INK MAP (the F1 seam), host-fed via [`Self::sense_ink`]:
    /// `ink_spans[i]` is viewport row `ink_base + i`'s inked columns as a
    /// half-open `[first, end)` span, `(0, 0)` for a blank row. Empty until a
    /// host feeds it, and every ink-aware rule is inert while it is.
    ink_base: u16,
    ink_spans: Vec<(u16, u16)>,
    /// The newest inked row on glass — the live output edge the watch hugs.
    ink_live: Option<u16>,

    content: f32,
    /// A BEL rang in the focused pane (wave-1 stimulus): when it was noted,
    /// so `tick` can expire it against [`BELL_LATCH_TTL`] on the injected
    /// clock. Consumed on the ground, below every caret-travel intent.
    pending_bell: Option<Instant>,
    /// A long command succeeded: `Some(big)` where `big` is the
    /// [`CHEER_BIG_MS`] upgrade. No TTL — a cheer waits out any chase (the
    /// latch itself holds `needs_frames` open, so it is always consumed or
    /// cleared, never stranded).
    pending_cheer: Option<bool>,
    /// A command failed: droop as soon as the pet is free.
    pending_sulk: bool,
    /// Queued pets (clicks on the cat), capped at [`PET_LATCH_MAX`], and
    /// when the LAST one landed (the [`PET_LATCH_TTL`] clock).
    pending_pet: u8,
    pet_at: Option<Instant>,
    /// Seconds remaining in the petting hold — the purr-flavored beat a
    /// consumed pet buys regardless of the contentment ledger.
    pet_hold_t: f32,
    /// PERK-AND-WATCH (wave 2): the stream-attention accumulator — rises
    /// [`WATCH_RISE`]/s while [`PetSense::output_burst`] holds, decays
    /// [`WATCH_FALL`]/s otherwise, clamped to `[0, 1]`. Heat, not a latch:
    /// the watch is an ambient state a settled pet drifts into, not an event
    /// it owes a reaction to.
    watch_heat: f32,
    /// Seconds spent in the current continuous watch (the [`WATCH_MAX`] cap's
    /// clock). Reset whenever the heat falls back under the gate.
    watch_t: f32,
    /// The cap fired: no re-watch until the heat has fallen back under
    /// [`WATCH_GATE`] — an all-day stream gets ONE bounded stare, then a
    /// parked gaze, never a pinned frame lane.
    watch_spent: bool,
    /// POINTER PLAY (wave 2): the pointer cell as of the previous tick — the
    /// pet's own pointer sensor, exactly like `last_caret`.
    last_pointer: Option<(f32, f32)>,
    /// Pointer velocity EMA (cells/s, per axis) over [`DASH_WINDOW`] — the
    /// pounce's linear-lead aim.
    pointer_vx: f32,
    pointer_vy: f32,
    /// Pointer SPEED EMA (cells/s, magnitude-of-instantaneous, same window)
    /// — the motion detector for the heat and the dash. Deliberately not
    /// `|v⃗|` of the vector EMA: a circling pointer nets to zero there, and
    /// a cat absolutely watches a circling toy.
    pointer_speed: f32,
    /// Pointer attention 0..=1: rises while the pointer wanders, decays
    /// [`POINTER_FALL`]/s once it stops. While above zero a settled pet's
    /// gaze tracks the pointer instead of the caret.
    pointer_heat: f32,
    /// Seconds the smoothed pointer speed has held ≥ [`DASH_SPEED`].
    dash_t: f32,
    /// The pounce trigger is armed. Disarmed by firing; re-armed only when
    /// the pointer heat decays under [`POUNCE_REARM`] — one pounce per dash.
    pointer_armed: bool,
    /// A matured dash latched a pounce at this predicted `(col, row)`,
    /// waiting for the ground. Cleared by any caret-travel latch: the caret
    /// is work, the pointer is play.
    pending_pointer_pounce: Option<(f32, f32)>,
    /// A play flight's target `(col, row)`, consumed when the crouch
    /// expires — the play twin of the station launch.
    play_to: Option<(f32, f32)>,
    /// What the play flight owes on landing (see [`PlayLand`]). Cleared by
    /// caret-travel latches: work skips the flourish.
    play_land: Option<PlayLand>,
    /// A play flourish holds the current Frolic for `play_hold` seconds
    /// (instead of [`FROLIC_HOLD`]), then falls through to the walk home.
    play_frolic: bool,
    play_hold: f32,
    /// The live flourish is the bat SWIPE: pin the Frolic to the bat pose
    /// (one forepaw out) instead of the playbow alternation.
    swipe: bool,
    /// THE WORD-CAT BAT: a noted peek head at `(col, row)` (fractional
    /// cells), waiting for the ground, and when it was noted (the
    /// [`BAT_TTL`] clock). Deliberately NOT cleared by caret work — the bat
    /// waits its turn and the TTL retires it honestly.
    pending_bat: Option<(f32, f32)>,
    bat_at: Option<Instant>,
    /// POINTER PURSUIT (wave 3): `Some(elapsed)` while the chase is live —
    /// a MODE steering the follower controller at the toy, not a one-shot
    /// latch. Cleared by caret work, the toy leaving, a catch, or stamina.
    pursuit_t: Option<f32>,
    /// Seconds until the next chase may begin (a break-off or catch arms it;
    /// caret work arms a short one so typing truly ends the game).
    pursuit_cool: f32,
    /// The break-off owes a groom — consumed at the next arrival (dignity).
    groom_owed: bool,
    /// THE STAKEOUT (wave 3): the current Crouch is the hunting crouch at a
    /// creeping toy, held while the toy stays slow and near — never the
    /// launch coil (the holds ladder checks this flag first).
    stakeout: bool,
    /// Seconds the current stakeout has held — its patience clock.
    stakeout_t: f32,
    /// THE LOOK (wave 3): a far peek's head column, waiting for the ground —
    /// the perk-and-face tier of the word-cat reaction. TTL-retired like the
    /// bat's latch.
    pending_look: Option<f32>,
    look_at: Option<Instant>,
    /// Serial-parity variety for the NEAR peek visit: odd visits greet (the
    /// playbow alternation) instead of swiping — set at note time from the
    /// mote serial, the wave-1 variation law.
    bat_greet: bool,
    /// THE TENNIS WATCH (wave 4): sitting out a caret rally, facing flipped
    /// per move; the clock is the last rally hit (lapses at TENNIS_LAPSE).
    tennis: bool,
    tennis_last: f64,
    /// When (brain clock) the last frolic fired — TENNIS_AFTER's anchor.
    last_frolic: f64,
    /// One-beat stumble squash remaining (the scramble's slipping paw).
    stumble_t: f32,
    /// Columns closed since the current chase leg began — the drift-brake's
    /// odometer, reset at every arrival.
    leg_dist: f32,
    /// The overshoot is live: the pet blew past its station on purpose and
    /// is trotting back.
    braking: bool,
    /// The drift-brake's RUN-OUT: the aim past the station, live only until
    /// the paws reach it. `Some` is what makes the overshoot a place the pet
    /// runs to rather than a place it is put — see the guard in `tick`.
    /// `braking` outlives it, so the trot home cannot arm a second brake.
    brake_over: Option<f32>,
    /// The pose (and its hold clock) a re-anchor hop owes back on landing.
    ///
    /// F1's law is "pose and hold kept, feet moved". Now that the feet move
    /// by hopping (owner ruling, 2026-08-10 — nothing changes position
    /// without animating), the hop borrows the pose for its own duration and
    /// hands it back the moment the paws are down: a sleeper hops out from
    /// under an arriving line and goes straight back to sleep, a droop
    /// resumes grieving on ground where the grief can be read.
    resume: Option<(PetAction, f32)>,
    /// Boredom cooldown (wave 4b): seconds until the next vignette may fire.
    bored_cool: f32,
    /// HIDE-BEHIND-WORDS (wave 4c): the walk target behind a word's ink,
    /// then the crouched hide itself with its dwell clock.
    hide_to: Option<f32>,
    hiding: bool,
    hide_t: f32,
    /// Seconds left of rolling around on its back (0 = not wriggling).
    wriggle_t: f32,
    /// IDLE MICRO-LIFE (wave 2): seconds remaining in the ear-twitch bob,
    /// which ear (by mote-serial parity at arm time), and the burst level
    /// of the previous tick (the pulse's edge detector).
    twitch_t: f32,
    twitch_up: bool,
    last_burst: bool,
    /// A jump was seen; pounce as soon as the pet is free to act on it.
    pending_pounce: bool,
    /// A SCREEN-CROSSING jump was seen (or earned by joy); play the full
    /// notice → wiggle → bound(s) choreography as soon as the pet is free.
    pending_big_jump: bool,
    /// The current crouch is the big jump's gather (the butt-wiggle), not
    /// the pounce's quick coil.
    big_gather: bool,
    /// The second bound of a split screen-crossing jump: final `(col, row)`,
    /// launched when the touch-land between bounds expires.
    bound2: Option<(f32, f32)>,
    /// Span of the big landing currently being absorbed (0 = a normal
    /// landing): scales the recovery squash and drives the skid.
    land_span: f32,
    /// The skid: where the paws touched, and which way they slide.
    skid_from: f32,
    skid_dir: f32,
    /// The mote lane (dust / z's / notes) and its deterministic serial.
    motes: [Option<Mote>; PET_MOTES_MAX],
    mote_serial: u8,
    /// Cadence mark for the settled emissions (sleep z's, purr notes): the
    /// last quiet-derived beat index a mote was spawned for — a pure
    /// function of the settle the user is watching, never of a wall clock.
    mote_mark: i64,
    /// The settled gaze (review #6), re-read every settled tick past
    /// [`SETTLE_TURN`]: eye contact (the face-on sit) for a close caret,
    /// the over-the-shoulder peek for a far caret behind the facing.
    sit_front: bool,
    peek: bool,
    /// The settle-turn's dwell clock (seconds): how long the gaze target has
    /// held the side the pet is NOT facing, CONTINUOUSLY. Rewound to zero by
    /// any frame the target is not committed to that side, which is what makes
    /// a caret or pointer swinging back and forth earn nothing at all.
    gaze_dwell: f32,
    /// Whether the pet has been seen at all yet (drives the fade-in).
    alpha: f32,

    /// Contrast palette frozen for this resident-pet appearance. The flying
    /// cursor kitty has its own episode latch; sharing that latch made a hidden
    /// flying cat donate stale colours to a pet that could remain on glass for
    /// minutes. This one clears at zero alpha and on explicit host context
    /// invalidation (session/theme/geometry/source changes).
    colors: Option<CatColorKey>,

    /// The `(coat, iris)` this appearance is WEARING — the pet's copy of the
    /// flying kitty's per-appearance look latch (`kitty_cursor`'s two-path
    /// rule: one appearance wears one cat). `None` until the host first
    /// dresses it.
    worn: Option<(u8, u8)>,
    /// A host sync that arrived mid-appearance, parked until the fade
    /// envelope returns to zero.
    pending_worn: Option<(u8, u8)>,
    /// How the pair at `pending_worn` is owed to land ([`PetArrival`]).
    /// THE LAST PARK'S ARRIVAL WINS: every sync that parks a differing
    /// pair — including an agreeing re-park of the same pair — overwrites
    /// this with its own verdict, so the handoff always performs the
    /// host's LATEST ruling (the host re-syncs every emission, and its
    /// verdict is the sticky latch; a fancier upgrade-only rule would
    /// second-guess it). Meaningful only while `pending_worn` is `Some`;
    /// stale — and never read — otherwise.
    pending_arrival: PetArrival,
    /// BREED HANDOFF (wave 2): when (free-running clock seconds) the CURRENT
    /// parked pair was parked — the [`HANDOFF_DEBOUNCE`] anchor. Restamped
    /// whenever the park changes, cleared when it dissolves or lands.
    handoff_parked_clock: Option<f64>,
    /// Seconds left of the breed handoff's ARRIVAL ramp: the emitted alpha
    /// is scaled by `1 − arrive_t/arrive_in`, so the incoming kitty fades
    /// up in place while the departing body runs off (or, on the quiet
    /// path, fades out where it stands). Zero outside a handoff.
    arrive_t: f32,
    /// The CURRENT ramp's own length (seconds) — `arrive_t`'s denominator
    /// at emit: [`ARRIVE_IN`] for a ceremony, [`HOMECOMING_IN`] for the
    /// quiet re-dress. Written in the same breath as `arrive_t`, every
    /// time, and defaulted to `ARRIVE_IN`, so the ramp is never divided
    /// by zero and a stale length can never outlive its own ramp.
    arrive_in: f32,
    /// The live departing bodies (the handoff's outgoing kitties), each a
    /// self-expiring birth record resolved per frame — the mote lane's shape,
    /// capped at [`PET_DEPARTURES_MAX`] with oldest-drops-first overflow.
    departures: [Option<Departure>; PET_DEPARTURES_MAX],
    /// The live body's last emitted pose (pre-skin — the brain stays
    /// species-blind), refreshed by every `emit` and consumed ONLY at
    /// departure spawn (`Departure::pose0`), which the live ladder guards —
    /// so the retired zero-alpha fast path may skip the write harmlessly:
    /// nothing can read it while the pet is hidden.
    last_pose: PetGlyphId,
}

impl Default for PetBrain {
    fn default() -> Self {
        Self {
            species: PetSpecies::Cat,
            col: 0.0,
            row: 0.0,
            speed: 0.0,
            facing_left: false,
            action: PetAction::Sleep,
            action_t: 0.0,
            // A FRESH PET IS ASLEEP. Seeding the quiet clock at the sleep
            // threshold means a newly opened window shows a curled, breathing
            // cat, and your first keystroke is what stretches it awake — which
            // is both the nicest introduction the companion gets and the honest
            // reading of the state (nothing has happened yet). The alternative,
            // waking up mid-sit for no reason, would have the pet asleep after
            // 22 s of quiet but never at the one moment quiet is guaranteed.
            quiet: SLEEP_AFTER,
            stride: 0.0,
            clock: 0.0,
            lane_clock: 0.0,
            last_caret: None,
            last_now: None,
            flight: None,
            deferred_hidden_landing: None,
            retired_flight_lift: None,
            land_t: 0.0,
            hop_crouch: false,
            coil_d: CROUCH_DUR,
            flinch_t: 0.0,
            last_dir: 0,
            reversals: 0,
            reversal_t: 0.0,
            vhat: 0.0,
            run_dir: 0,
            run_count: 0,
            // Far outside the window, like `retreat_gap`: the first move ever
            // seen starts a fresh run of one, not a phantom continuation.
            run_t: 60.0,
            wall_side: false,
            pending_wall_transit: false,
            body_left_px: 0.0,
            retreat_run: 0,
            // Far outside the window, so the first retreat ever seen starts a
            // fresh run of one rather than continuing a phantom one.
            retreat_gap: 60.0,
            ink_base: 0,
            ink_spans: Vec::new(),
            ink_live: None,
            content: 0.0,
            pending_bell: None,
            pending_cheer: None,
            pending_sulk: false,
            pending_pet: 0,
            pet_at: None,
            pet_hold_t: 0.0,
            watch_heat: 0.0,
            watch_t: 0.0,
            watch_spent: false,
            last_pointer: None,
            pointer_vx: 0.0,
            pointer_vy: 0.0,
            pointer_speed: 0.0,
            pointer_heat: 0.0,
            dash_t: 0.0,
            pointer_armed: true,
            pending_pointer_pounce: None,
            play_to: None,
            play_land: None,
            play_frolic: false,
            play_hold: 0.0,
            swipe: false,
            pending_bat: None,
            bat_at: None,
            pursuit_t: None,
            pursuit_cool: 0.0,
            groom_owed: false,
            stakeout: false,
            stakeout_t: 0.0,
            pending_look: None,
            look_at: None,
            bat_greet: false,
            tennis: false,
            tennis_last: 0.0,
            last_frolic: -1.0e9,
            stumble_t: 0.0,
            leg_dist: 0.0,
            braking: false,
            brake_over: None,
            resume: None,
            bored_cool: 0.0,
            wriggle_t: 0.0,
            hide_to: None,
            hiding: false,
            hide_t: 0.0,
            twitch_t: 0.0,
            twitch_up: false,
            last_burst: false,
            pending_pounce: false,
            pending_big_jump: false,
            big_gather: false,
            bound2: None,
            land_span: 0.0,
            skid_from: 0.0,
            skid_dir: 0.0,
            motes: [None; PET_MOTES_MAX],
            mote_serial: 0,
            mote_mark: 0,
            sit_front: false,
            peek: false,
            gaze_dwell: 0.0,
            alpha: 0.0,
            colors: None,
            worn: None,
            pending_worn: None,
            // The latch is only meaningful beside a `Some` park; Ceremony
            // is today's whole behavior, so it is the inert default.
            pending_arrival: PetArrival::Ceremony,
            handoff_parked_clock: None,
            arrive_t: 0.0,
            arrive_in: ARRIVE_IN,
            departures: [None; PET_DEPARTURES_MAX],
            // A fresh pet is asleep (see `quiet` above), so its last look
            // is the sleep frame — consistent with what the first emit
            // would write.
            last_pose: PetGlyphId::PetSleep0,
        }
    }
}

impl PetBrain {
    /// Where the pet wants to stand for a caret at `(row, col)`, in fractional
    /// columns of its LEFT edge — the screen-awareness rule.
    ///
    /// Its station is just past the caret. When that station would hang the pet
    /// off the right margin it crosses to the caret's LEFT instead: at the end
    /// of a long line the writing is behind the caret anyway, so standing there
    /// is both legal and correct. If the grid is too narrow to hold the pet on
    /// either side, it parks flush left and the clamp does the rest.
    #[must_use]
    pub fn station(caret_col: u16, cols: u16, width: f32) -> f32 {
        let right = f32::from(caret_col) + STATION_LEAD;
        let limit = f32::from(cols) - width;
        if limit <= 0.0 {
            return 0.0;
        }
        if right <= limit {
            return right;
        }
        // The wall: stand on the other side of the caret, one clear cell back.
        (f32::from(caret_col) - width - STATION_LEAD).max(0.0)
    }

    /// The rhythm gate is open: a real same-direction typing run is under way,
    /// so the lead (and the flight-aim prediction built on the same estimate)
    /// may act on v̂.
    fn rhythm_open(&self) -> bool {
        self.run_count >= RHYTHM_MOVES
    }

    /// The keep-ahead lead (cells): how far past [`STATION_LEAD`] the station
    /// sits, `clamp(v̂ · LEAD_TIME, 0, LEAD_MAX)` — zero without a rhythm,
    /// zero for a leftward run (there is no "ahead" behind the writing), and
    /// eased back by v̂'s own decay the moment you pause.
    fn lead(&self) -> f32 {
        if !self.rhythm_open() {
            return 0.0;
        }
        (self.vhat * LEAD_TIME).clamp(0.0, LEAD_MAX)
    }

    /// Hysteretic wall side: cross LEFT only when the led station would hang
    /// off the right margin, cross back only when the station clears the
    /// margin by [`WALL_RETURN_MARGIN`]. Returns whether the side flipped
    /// this call — a flip is a transit over the caret column, which the chase
    /// answers with a small arc rather than a walk through the cursor.
    fn update_wall(&mut self, caret_col: u16, cols: u16, width: f32) -> bool {
        if f32::from(cols) - width <= 0.0 {
            return false; // grid narrower than the cat: no sides to keep
        }
        let edge = f32::from(caret_col) + STATION_LEAD + self.lead() + width;
        let was = self.wall_side;
        if !was && edge > f32::from(cols) {
            self.wall_side = true;
        } else if was && edge < f32::from(cols) - WALL_RETURN_MARGIN {
            self.wall_side = false;
        }
        was != self.wall_side
    }

    /// The LIVE station: [`Self::station`]'s law plus the keep-ahead lead and
    /// the hysteretic wall side. The static `station` stays as the lead-free
    /// base (first sightings, reduced motion, hosts) — this is the one the
    /// chase follows.
    fn station_now(&self, caret_col: u16, cols: u16, width: f32, cell_w: u16) -> f32 {
        let limit = f32::from(cols) - width;
        if limit <= 0.0 {
            return 0.0;
        }
        if self.wall_side {
            // Parked wall-side: no lead — there is nowhere ahead to lead to.
            return (f32::from(caret_col) - width - STATION_LEAD).max(0.0);
        }
        // BBOX ANCHOR SEAM: shift left by the pose's body inset so the
        // VISIBLE left edge (not the authored tile's) sits at caret+1.
        // `body_left_px` defaults to 0 until the baker's export lands.
        let inset = if cell_w == 0 {
            0.0
        } else {
            self.body_left_px / f32::from(cell_w)
        };
        (f32::from(caret_col) + STATION_LEAD + self.lead() - inset).clamp(0.0, limit)
    }

    /// BBOX ANCHOR SEAM: the current pose's visible-body left inset, in grid
    /// PIXELS inside the authored tile. The design review's rule is that the
    /// pet's VISIBLE left edge sits at caret+1; the authored tile carries
    /// transparent margin, so anchoring the tile there parks the visible cat
    /// further right, pose by pose.
    ///
    /// TODO(pet-art): feed this from `pet_baker`'s per-pose body bbox export
    /// once the art wave lands it (`pet_baker` is owned by that wave right
    /// now, so the export cannot be added here). Until then the documented
    /// default of 0 keeps today's authored-box anchor, and [`LEAD_MAX`] stays
    /// conservative for the same reason.
    /// Choose which animal the pet is drawn as ([`PetSpecies`]).
    ///
    /// Safe to call every frame — it is a plain field write with no state to
    /// invalidate, which is the point of applying the skin at `emit`: switching
    /// `rainbow kitty pet` → `rainbow dog pet` in Settings swaps the sprite on
    /// the very next frame WITHOUT resetting the companion. The pet keeps its
    /// position, its gait phase, its mood and its sleep clock, so the change
    /// reads as the same creature changing coat rather than a new pet
    /// teleporting in mid-stride.
    pub fn set_species(&mut self, species: PetSpecies) {
        self.species = species;
    }

    /// The animal currently being drawn.
    #[must_use]
    pub fn species(&self) -> PetSpecies {
        self.species
    }

    pub fn set_body_left_px(&mut self, px: f32) {
        // NaN.max(0.0) is NaN — a non-finite inset keeps the last good one
        // (codex review, 2026-08-10).
        if px.is_finite() {
            self.body_left_px = px.max(0.0);
        }
    }

    /// THE INK SEAM (gauntlet F1/F5/F8): the host's per-frame ink probe.
    /// `spans[i]` is viewport row `base_row + i`'s inked columns, half-open
    /// `[first, end)` with `(0, 0)` meaning blank; `live_row` is the newest
    /// inked row on glass (the live output edge). Feed it just before `tick`
    /// — `WordDecorations::pet_ink` computes exactly this shape from the rows
    /// it already scans, so the host wiring is a hand-off, not a walk.
    ///
    /// Never fed ⇒ the map stays empty and every ink-aware rule is inert:
    /// the brain falls back to pure caret geometry, byte-for-byte the
    /// pre-seam behavior. Copies into resident storage — allocation-free
    /// after warmup, the crate's per-frame law.
    pub fn sense_ink(&mut self, base_row: u16, spans: &[(u16, u16)], live_row: Option<u16>) {
        self.ink_base = base_row;
        self.ink_spans.clear();
        self.ink_spans.extend_from_slice(spans);
        self.ink_live = live_row;
    }

    /// Retire every fact whose meaning depends on the current terminal surface.
    ///
    /// A pane/tab switch replaces both the caret's owner and the origin under
    /// this brain's pane-local `(col, row)`. Carrying a visible body across that
    /// boundary makes it jump to an unrelated pane without an authored move;
    /// carrying flights, pointer heat, ink or pending reactions is the same bug
    /// one layer deeper. The replacement surface therefore starts as a fresh
    /// sighting (alpha zero, no cadence debt), while the pet's durable identity
    /// survives: species, worn look, earned contentment, awake/asleep disposition
    /// and deterministic lifetime clocks are not coordinates.
    ///
    /// Idempotent. Hosts may call this both at the canonical front-owner switch
    /// and at a later presentation boundary without erasing additional state.
    pub fn retire_coordinate_space(&mut self) {
        let species = self.species;
        let body_left_px = self.body_left_px;
        let content = self.content;
        let quiet = self.quiet;
        let clock = self.clock;
        // A replacement look parked during the outgoing appearance is the
        // newest durable identity verdict. Retirement ends that appearance,
        // so land the parked pair exactly as the ordinary fade-to-zero path
        // does instead of resurrecting the coat it superseded.
        let worn = self.pending_worn.or(self.worn);
        let mote_serial = self.mote_serial;
        let mote_mark = self.mote_mark;
        let last_frolic = self.last_frolic;
        let bored_cool = self.bored_cool;
        let mut ink_spans = std::mem::take(&mut self.ink_spans);
        ink_spans.clear();

        // A LITERAL, not a default-then-assign: the carried-over fields are
        // the whole point of this fn, and spelling them in the initializer is
        // what makes "everything else is reset" readable at a glance (and what
        // `clippy::field_reassign_with_default` asks for).
        *self = Self {
            species,
            body_left_px,
            content,
            quiet,
            clock,
            action: if quiet >= SLEEP_AFTER {
                PetAction::Sleep
            } else {
                PetAction::Sit
            },
            worn,
            mote_serial,
            mote_mark,
            last_frolic,
            bored_cool,
            ink_spans,
            ..Default::default()
        };
    }

    /// THE GRIEF WINDOW (gauntlet F4a): true from a failed command's note
    /// until its droop has finished playing. The host reads this to hush the
    /// caret-jump fanfare — the rainbow meteor's terminus star ring fired ON
    /// the failure prompt in the gauntlet, a celebration emotionally cheering
    /// a failure (and landing '+' crosses on the user's own glyphs). The pet
    /// cannot reach that emitter from here; it can say when to be quiet.
    ///
    /// THE BORROWED DROOP COUNTS. A re-anchor hop replaces the pose with
    /// `Leap`/`Land` for the half-second it is in the air, so reading the
    /// action alone would blink this window SHUT mid-grief and open it again
    /// on landing. That is worse than never having hushed at all: the one
    /// moment a re-anchor happens is the moment a fresh prompt printed over
    /// the cat — precisely when the caret-jump fanfare fires, and precisely
    /// the F4a gauntlet failure (a star ring cheering on the failure prompt).
    /// A pose the hop OWES BACK is a pose the pet is still in.
    #[must_use]
    pub fn grieving(&self) -> bool {
        self.pending_sulk
            || self.action == PetAction::Droop
            || matches!(self.resume, Some((PetAction::Droop, _)))
    }

    /// The ink span of an integer row index, in fractional columns —
    /// `None` when the row is unknown to the map or blank.
    fn ink_span_idx(&self, r: i64) -> Option<(f32, f32)> {
        let idx = usize::try_from(r.checked_sub(i64::from(self.ink_base))?).ok()?;
        let &(first, end) = self.ink_spans.get(idx)?;
        (end > first).then(|| (f32::from(first), f32::from(end)))
    }

    /// The ink span under a FEET row (settled rows are integral; `round`
    /// keeps a mid-hop fraction honest).
    fn ink_span(&self, row: f32) -> Option<(f32, f32)> {
        if !row.is_finite() || row < -0.5 {
            return None;
        }
        self.ink_span_idx(row.round() as i64)
    }

    /// Whether a footprint standing at `col` on `row` covers glyphs: the
    /// authored tile's transparent side margins are shaved off first
    /// ([`INK_PAD`]), so only the BODY over ink counts.
    fn ink_overlaps(&self, col: f32, row: f32, width: f32) -> bool {
        let Some((first, end)) = self.ink_span(row) else {
            return false;
        };
        let (a, b) = (col + INK_PAD, col + width - INK_PAD);
        b > first && a < end
    }

    /// F1's retarget: nudge a station off glyphs to the nearest blank stand
    /// beside the row's ink — right of its end, or left of its start when
    /// that is closer — viewport-clamped. A row walled shut on both sides
    /// keeps the plain want: the answer to "no blank ground here" is never
    /// "teleport somewhere else entirely". Neither is the answer to "no blank
    /// ground within reach" — a stand further than [`INK_EVICT_MAX`] from the
    /// want is refused for the same reason, and that is what keeps a caret
    /// editing mid-sentence from riding the cat along on the end of the line —
    /// see [`Self::ink_stand`], the ladder that owns that ordering.
    fn ink_safe_col(&self, want: f32, row: f32, width: f32, cols: u16) -> f32 {
        if !self.ink_overlaps(want, row, width) {
            return want;
        }
        let Some((first, end)) = self.ink_span(row) else {
            return want;
        };
        let limit = (f32::from(cols) - width).max(0.0);
        let right = end + INK_GAP;
        let left = first - width - INK_GAP;
        match (right <= limit, left >= 0.0) {
            (true, true) if (left - want).abs() < (right - want).abs() => left,
            (true, _) => right,
            (false, true) => left,
            (false, false) => want,
        }
    }

    /// Where a stand at `want` on `row` actually goes — the **ink ladder**,
    /// tried in order of how little it asks of the cat:
    ///
    /// 1. `want` is already on blank cells. Ordinary end-of-line typing lives
    ///    here and never reads the rest of this function.
    /// 2. A NEAR blank stand on the same row ([`INK_EVICT_MAX`]) — a step
    ///    aside, the cheapest correction there is.
    /// 3. The same column one row DOWN, else one row UP. The mid-line editing
    ///    answer, and the one the owner was missing: a caret inside a sentence
    ///    has no blank ground beside it, but the line under it is almost
    ///    always empty, and a cat sitting one line off beside your cursor is
    ///    still WITH you.
    /// 4. The far stand beside this row's ink — the original F1 law, which
    ///    also carries its own walled-shut fallback (stand on the words; the
    ///    answer to "nowhere to stand" was never "leave"). Reached only when
    ///    the neighbouring rows are inked too, i.e. a screen full of text,
    ///    where "beside the paragraph" really is the best ground on offer.
    ///
    /// Rule 3 is what the owner's report bought (2026-08-10: "the cursor kitty
    /// gets pushed around in the text when editing in the middle of the
    /// text"). Before it, rule 4 fired for every mid-line edit and parked the
    /// cat past the END of the sentence — measured, a caret at column 12
    /// stationed the pet at 31.35 — and because inserting a character grows
    /// that end, the cat was shoved a cell right per keystroke by text it was
    /// nowhere near. Rules 1, 2 and 4 are the behaviour that shipped; 3 only
    /// ever runs where 4 used to give an answer that bad.
    fn ink_stand(&self, want: f32, row: f32, width: f32, cols: u16, rows: u16) -> (f32, f32) {
        if !self.ink_overlaps(want, row, width) {
            return (want, row);
        }
        let beside = self.ink_safe_col(want, row, width, cols);
        if (beside - want).abs() <= INK_EVICT_MAX {
            return (beside, row);
        }
        // Down first: the row under the caret is the one the stream has not
        // reached yet, so it is blank far more often than the row above — and
        // a cat below the line never covers the line.
        for r in [row + 1.0, row - 1.0] {
            if r >= 0.0 && r < f32::from(rows) && !self.ink_overlaps(want, r, width) {
                return (want, r);
            }
        }
        (beside, row)
    }

    /// Is this eviction a RE-ANCHOR (a hop) rather than a step aside (a
    /// glide)? Two ways to earn the arc: a column displacement past
    /// [`INK_EVICT_MAX`], or any change of row at all — a cat does not walk
    /// between lines, it hops them, which is the law the chase has enforced
    /// since the Enter hop.
    fn is_re_anchor(from_col: f32, to_col: f32, from_row: f32, to_row: f32) -> bool {
        (to_col - from_col).abs() > INK_EVICT_MAX || (to_row - from_row).abs() >= 0.5
    }

    /// A grounded pose's step aside: `from` moved toward `to`, at most
    /// [`INK_EVICT_SPEED`] cells this frame. Only ever called for a trip that
    /// IS a step aside — anything past [`INK_EVICT_MAX`] is the F1 re-anchor
    /// and [`Self::is_re_anchor`] routes it to the hop instead, so the
    /// old one-frame landing here is gone. The clamp survives as a guard: a
    /// glide never overshoots its stand, however long the frame was.
    fn evict_toward(from: f32, to: f32, dt: f32) -> f32 {
        let delta = to - from;
        let step = INK_EVICT_SPEED * dt.max(0.0);
        if delta.abs() <= step {
            to
        } else {
            from + delta.signum() * step
        }
    }

    /// The caret's move in TEXT cells, not grid cells: a line WRAP is ONE
    /// character however it looks on the grid.
    ///
    /// Typing past the right margin moves the caret from `(r, cols-1)` to
    /// `(r+1, 0)` — one row down and nearly the full width LEFT — and
    /// backspacing over that seam moves it back the other way. Read
    /// literally, those are the two largest single moves a caret can make on
    /// a wide window, so both cleared [`POUNCE_JUMP`] *and* the
    /// screen-crossing [`BIG_JUMP_COLS`] bar: every wrap fired a
    /// gather-and-bound, and a held backspace over a wrapped line fired one
    /// per seam. What the author did was type — or delete — one character.
    ///
    /// A wrap is the only move that crosses exactly one row boundary while
    /// travelling nearly the whole width the OTHER way, so it is identifiable
    /// from the delta alone. Fold the row back into the column and the answer
    /// is the ±1 the text really moved; the row is then zero, which also
    /// hands a backspace-over-wrap to the retreat arm that a literal
    /// `dr == -1` used to route past. Everything else — a real row jump, a
    /// click, a scroll — keeps its literal delta, because folding those would
    /// invent a column move nobody made.
    ///
    /// EXCEPT at the bottom of a scrolling pane, where a wrap SCROLLS and the
    /// caret's row never changes: the delta reads as a full-width retreat,
    /// byte-identical to `Home` pressed at the last column, and no heuristic
    /// can tell them apart. The emulator can — it knows exactly when it
    /// wrapped — and `wrapped` carries that fact ([`PetSense::wrapped`]).
    /// The fact-gated arm folds the scrolled seam to `(1.0, dc + w)`: one
    /// line of text gained, one character on. Both arms measure the same
    /// seam through the one shared [`wrap_bar`]. Hosts without the fact pass
    /// `false` and keep the heuristic arm's behaviour exactly.
    fn caret_delta(prev: (u16, u16), now: (u16, u16), cols: u16, wrapped: bool) -> (f32, f32) {
        let dr = f32::from(now.0) - f32::from(prev.0);
        let dc = f32::from(now.1) - f32::from(prev.1);
        let w = f32::from(cols);
        let bar = wrap_bar(w);
        if dr.abs() == 1.0 && dc.signum() != dr.signum() && dc.abs() >= bar {
            (0.0, dc + dr * w)
        } else if wrapped && dr == 0.0 && dc.signum() < 0.0 && dc.abs() >= bar {
            // The scrolled wrap, folded on the fact alone: the grid moved a
            // line up underneath a caret pinned to the bottom row. The bar
            // still guards the arm — the fact says "a wrap happened since
            // the last read", and only a delta shaped like the seam may
            // claim it (a `Home` pressed mid-line the same frame keeps its
            // literal retreat).
            (1.0, dc + w)
        } else {
            (dr, dc)
        }
    }

    /// The chase target with the ink law folded in: [`Self::station_now`] run
    /// through the [`Self::ink_stand`] ladder, which is why it answers with a
    /// ROW as well as a column — rule 3 can seat the cat one line off the
    /// caret's. The chase, the evictions and the arrived test all read THIS
    /// one stand, so they can never fight each other over where the pet
    /// belongs.
    fn station_safe(
        &self,
        caret: (u16, u16),
        cols: u16,
        rows: u16,
        width: f32,
        cell_w: u16,
    ) -> (f32, f32) {
        let want = self.station_now(caret.1, cols, width, cell_w);
        self.ink_stand(want, f32::from(caret.0), width, cols, rows)
    }

    /// The live output edge as a row coordinate — `None` until the host
    /// feeds the ink map, which keeps the watch's hug (F5) inert without it.
    fn ink_live_row(&self) -> Option<f32> {
        self.ink_live.map(f32::from)
    }

    /// F8: how visible a z-mote may be at `(col, row)` — 1 in clear sky,
    /// easing to 0 across [`ZEE_INK_FADE_BAND`] rows as it drifts up toward
    /// an inked band, exactly 0 inside one. Mote rows are band-edge
    /// coordinates (`row * cell_h` is the band boundary), so the band a mote
    /// occupies is `floor(row)`.
    fn zee_ink_fade(&self, col: f32, row: f32) -> f32 {
        if !row.is_finite() {
            return 1.0;
        }
        let covered = |r: i64| {
            self.ink_span_idx(r)
                .is_some_and(|(first, end)| col >= first - INK_PAD && col <= end + INK_PAD)
        };
        let band = row.floor() as i64;
        if covered(band) {
            return 0.0;
        }
        // The nearest inked band above (z's climb ~1.7 rows in a life).
        for k in 1..=2i64 {
            if covered(band - k) {
                // Distance down from that band's bottom edge to the mote.
                let edge = (band - k + 1) as f32;
                return ((row - edge) / ZEE_INK_FADE_BAND).clamp(0.0, 1.0);
            }
        }
        1.0
    }

    // ── the wave-1 stimuli: note, never act ─────────────────────────────
    //
    // The `kitty_sing::note_char` idiom: stimuli LATCH here and `tick`
    // consumes them under the existing precedence ladder. Acting at note
    // time would fire mid-hold or mid-flight — exactly the bug the pounce
    // latch exists to prevent.

    /// The terminal BEL rang in the pane this pet is chasing. Latched
    /// against [`BELL_LATCH_TTL`]: a sleeping pet wakes with the startled z
    /// and the stretch, a settled pet flinches toward the caret, and a bell
    /// that expires mid-flight is simply never acted on.
    pub fn note_bell(&mut self, now: Instant) {
        self.pending_bell = Some(now);
    }

    /// A shell command COMPLETED in the focused pane (OSC 133/633 D, edge-
    /// deduped by the host). The ledger moves NOW — contentment is
    /// bookkeeping, and it must move under reduced motion too — but the
    /// choreography only latches: a failure queues the droop, a success
    /// that ran at least [`CHEER_MIN_MS`] queues the cheer (upgraded past
    /// [`CHEER_BIG_MS`]), and a fast success only nudges.
    pub fn note_command_done(&mut self, now: Instant, failed: bool, dur_ms: Option<u64>) {
        // The injected clock is the idiom's signature; this stimulus keys
        // its consumption off the ledger and the latch, not off a TTL.
        let _ = now;
        if failed {
            self.content = (self.content - SULK_CONTENT).max(0.0);
            self.pending_sulk = true;
            return;
        }
        match dur_ms {
            Some(dur) if dur >= CHEER_MIN_MS => {
                self.content = (self.content + CHEER_CONTENT).min(1.0);
                self.pending_cheer = Some(dur >= CHEER_BIG_MS);
            }
            _ => {
                // Under two seconds (or no timestamps at all): the ledger
                // only — no latch, no choreography.
                self.content = (self.content + CHEER_FAST_CONTENT).min(1.0);
            }
        }
    }

    /// The user clicked the cat. Pets queue up to [`PET_LATCH_MAX`] (extras
    /// are absorbed silently) and stay actionable for [`PET_LATCH_TTL`]
    /// from the LAST click, so a pet mid-flight waits for the ground.
    pub fn note_petted(&mut self, now: Instant) {
        self.pending_pet = self.pending_pet.saturating_add(1).min(PET_LATCH_MAX);
        self.pet_at = Some(now);
    }

    /// Queued, not-yet-consumed pets — host observability (the click seam's
    /// unit tests assert the latch moved without driving a full frame).
    #[must_use]
    pub fn pending_pets(&self) -> u8 {
        self.pending_pet
    }

    /// An ambient word-cat peek LANDED at `(col, row)` (fractional cells of
    /// this pet's pane — the head's center, not the word's: the head peeks
    /// rows away from its text, and the bat swipes at the head). Wave 2's
    /// cross-crate stimulus: the engine records the positioned cue, the
    /// host drains and forwards, and the brain range-checks HERE, at note
    /// time — a far peek never even latches. Latch-don't-act, as ever:
    /// consumption waits for the ground below every caret intent, and
    /// [`BAT_TTL`] retires a peek the work outlasted.
    pub fn note_peek(&mut self, now: Instant, col: f32, row: f32) {
        // Non-finite coordinates never latch: NaN compares false with every
        // range gate below, so it would slip straight into the flight target
        // and poison the pose state (codex review, 2026-08-10).
        if !col.is_finite() || !row.is_finite() {
            return;
        }
        let dc = (col - self.col).abs();
        let dr = (row - self.row).abs();
        // Rows count double: cells are ~half as wide as tall, so the range
        // is judged in column-equivalents (a deterministic pane-geometry
        // fact, not a tuning surface).
        let range = dc.max(dr * 2.0);
        if range > BAT_RANGE {
            // THE LOOK tier (wave 3): out of paw reach but near enough to
            // notice — latch the perk-and-face, never a trip. Beyond
            // LOOK_RANGE the peek is scenery.
            if range <= LOOK_RANGE {
                self.pending_look = Some(col);
                self.look_at = Some(now);
            }
            return;
        }
        // The NEAR visit varies by serial parity (the wave-1 variation law):
        // even swipes, odd greets with the playbow alternation.
        self.bat_greet = !self.mote_serial.is_multiple_of(2);
        self.mote_serial = self.mote_serial.wrapping_add(1);
        self.pending_bat = Some((col, row));
        self.bat_at = Some(now);
    }

    /// Advance one frame and resolve what to draw.
    pub fn tick(&mut self, sense: PetSense) -> PetFrame {
        // TWO clocks from one elapsed, deliberately. `dt` drives MOTION and is
        // clamped hard: a hidden window, a breakpoint, or a suspended machine
        // must not teleport the cat across the screen on the first frame back.
        // `elapsed` drives the QUIET and animation clocks and is not: once the
        // pet stops asking for frame cadence the host drops to the blink's ~1 Hz,
        // and a clamped quiet clock would make "22 seconds of silence" mean
        // several minutes of wall time — the cat would never fall asleep on an
        // idle window, which is the one window where it should.
        let elapsed = match self.last_now {
            Some(prev) => sense
                .now
                .saturating_duration_since(prev)
                .as_secs_f32()
                .clamp(0.0, 5.0),
            None => 0.0,
        };
        let dt = elapsed.min(0.10);
        self.last_now = Some(sense.now);
        self.clock += f64::from(elapsed);
        // The departure lane is MOTION, so its clock rides the dt clamp:
        // ghosts advance `speed × dt` worth of run per tick like every
        // other moving body, while `clock` above keeps taking the full
        // elapsed for the quiet and animation clocks (the doctrine above).
        self.lane_clock += f64::from(dt);

        let width = art_cols(sense.cell_w, sense.cell_h);
        // Sampled BEFORE either fade arm moves it: "was the pet off the glass
        // when this frame began?" is the question the first-sighting seed
        // below has to ask, and by the time it runs the fade-in has already
        // lifted a true first sighting off zero.
        let was_invisible = self.alpha <= 0.0;
        let Some((cr, cc)) = sense.caret else {
            // Retire the OLD presentation's arc immediately, but do not move
            // a still-visible fading body to its far destination. If the fade
            // completes, landing there is invisible and gives the next cold
            // sighting the intended station. If the caret returns sooner, the
            // live body keeps continuity and the normal follower walks from it.
            if let Some(flight) = self.flight.take() {
                let u = (flight.t / flight.dur).clamp(0.0, 1.0);
                // THROUGH THE SHARED CURVE, not a second copy of the
                // parabola: rung 10 gave big bounds a HANG, so recomputing
                // `arc * 4u(1-u)` here would hand the fade a lift the
                // emitter never drew — the visible feet would step the
                // instant the caret went away.
                let lift = Self::flight_lift(flight.arc, u, flight.big);
                // Hold the exact authored parabola while hidden. A quick
                // return settles it over the normal fade-in window instead
                // of dropping an opaque body to its unlifted row.
                self.retired_flight_lift = (lift > 0.0).then_some((lift, lift / FADE_IN));
                self.deferred_hidden_landing = Some((flight.to_col, flight.to_row));
            }
            // No caret: settle, fade out, hold position — and FORGET where the
            // caret was. Keeping it would make the next sighting diff against a
            // cell from before the hide: a full-screen app that hides the cursor
            // and a fresh prompt that shows it back at column 3 would read as a
            // 78-column retreat, and the pet would fade in already flinching
            // from a backspace nobody typed. A caret that went away and came
            // back is a NEW sighting, not a move.
            //
            // ── the retired fast path (PET-04) ─────────────────────────────
            // The host ticks this brain once per presented frame FOREVER —
            // unconditionally, so `needs_frames()` stays truthful — which
            // means a pet the user never enabled (or whose fade finished
            // long ago) still pays the whole arm below every frame: ~40
            // stores of values that are provably already there, one `exp()`
            // in `enter_settled`, and a full `emit()` whose frame the host
            // discards at its very first test (`alpha > 0`). Skip all of it,
            // but ONLY when every skipped write is a no-op:
            //
            //   alpha == 0.0, last_caret None — the fade write and the
            //     forget write have already landed;
            //   pending_worn None — the look-park landing has nothing to
            //     land (a parked pair takes the slow arm exactly once, which
            //     lands it at zero alpha, then this path resumes);
            //   action == Sleep AND quiet ≥ SLEEP_AFTER + BREATH_WINDOW —
            //     `enter_settled` would verdict `Sleep` (quiet only grows in
            //     this arm, so that verdict is now permanent) making its
            //     `set_action_keep` a no-op, and `emit` would take the
            //     past-the-breath-window Sleep arm, whose frame is exactly
            //     the CONSTANT built below (no breath phase, no scale mods,
            //     no purr);
            //   speed == 0.0, stride integral — `enter_settled`'s two other
            //     writes are no-ops. An eased stride usually stalls a few
            //     ULP shy of its integral target, so a pet that ever WALKED
            //     simply keeps taking the slow arm — correct, just not
            //     accelerated; the never-enabled brain this path exists for
            //     has stride exactly 0.0 forever;
            //   stimulus latches empty — the arm DROPS bell/cheer/sulk/pet/
            //     bat/look (no audience, no theater), so a stimulus noted
            //     while hidden must still meet the slow arm and be dropped,
            //     never survive into the next appearance;
            //   motes/departures empty — `resolve_motes`/`_departures` have
            //     no lives left to expire (until the lanes empty, the slow
            //     arm keeps running their clocks every frame).
            //
            // Everything else the arm clears is written ONLY by the live
            // path or by the arm itself, and `alpha == 0 && last_caret ==
            // None` is reachable only through this arm (or `default()`,
            // whose values are all rest values) — so those fields are
            // already at rest by induction, and the only public methods
            // that can disturb rest between ticks are exactly the gates
            // above (the note_* latches and `sync_look`'s park).
            //
            // The clocks still advance: `last_now`/`clock` moved in the
            // prologue, and `quiet` moves here — a frozen quiet would wake
            // the pet in the wrong pose (`enter_settled` reads it on the
            // first frame back). The emitted constant matches `emit`'s
            // deep-sleep frame field for field, including `emit`'s ONE
            // state effect on this path: the position clamp (live geometry
            // can shrink while the pet is retired; the wake's
            // `station_safe` re-places the body anyway, but the clamp keeps
            // the held position bit-identical to the slow arm's).
            if self.alpha == 0.0
                && self.last_caret.is_none()
                && self.pending_worn.is_none()
                && self.deferred_hidden_landing.is_none()
                && self.retired_flight_lift.is_none()
                && self.action == PetAction::Sleep
                && self.quiet >= SLEEP_AFTER + BREATH_WINDOW
                && self.speed == 0.0
                && self.stride == self.stride.round()
                && self.pending_bell.is_none()
                && self.pending_cheer.is_none()
                && !self.pending_sulk
                && self.pending_pet == 0
                && self.pet_at.is_none()
                && self.pending_bat.is_none()
                && self.pending_look.is_none()
                && self.motes.iter().all(Option::is_none)
                && self.departures.iter().all(Option::is_none)
            {
                self.quiet += elapsed;
                let max_col = (f32::from(sense.cols) - width).max(0.0);
                let max_row = f32::from(sense.rows.saturating_sub(1));
                self.col = self.col.clamp(0.0, max_col);
                self.row = self.row.clamp(0.0, max_row);
                return PetFrame {
                    alpha: 0,
                    lane_alpha: 0,
                    action: self.action,
                    pose: self.species.skin(PetGlyphId::PetSleep0),
                    col: self.col,
                    row: self.row,
                    lift: 0.0,
                    facing_left: self.facing_left,
                    scale_x: 1.0,
                    scale_y: 1.0,
                    purr: 0.0,
                    under_ink: self.hiding,
                    motes: [None; PET_MOTES_MAX],
                    departures: [None; PET_DEPARTURES_MAX],
                };
            }
            self.alpha = (self.alpha - dt / FADE_OUT).max(0.0);
            if self.alpha == 0.0 {
                self.colors = None;
                if let Some((col, row)) = self.deferred_hidden_landing.take() {
                    self.col = col;
                    self.row = row;
                }
                self.retired_flight_lift = None;
            }
            // The envelope has reached zero: a look sync parked mid-appearance
            // ([`Self::sync_look`]) lands now, so the NEXT appearance is the
            // one that wears the new cat.
            if self.alpha == 0.0
                && let Some(pair) = self.pending_worn.take()
            {
                self.worn = Some(pair);
            }
            self.quiet += elapsed;
            self.speed = 0.0;
            self.last_caret = None;
            self.hop_crouch = false;
            self.flinch_t = 0.0;
            // A caret that went away took its velocity with it: the next
            // sighting is a NEW sighting, and a stale v̂ would scoot the pet
            // ahead of a caret that has not moved yet.
            self.vhat = 0.0;
            self.run_count = 0;
            self.pending_wall_transit = false;
            // No caret, no audience: the wave-1 stimuli are dropped, not
            // parked — a cheer/fright/pet with nothing on glass to act it
            // out would otherwise pin `needs_frames` on a hidden cat.
            self.pending_bell = None;
            self.pending_cheer = None;
            self.pending_sulk = false;
            self.pending_pet = 0;
            self.pet_at = None;
            self.pet_hold_t = 0.0;
            // The wave-2 heat too: a hidden caret means no stream on this
            // surface worth watching, and a stale watch must not resume
            // against a fresh sighting.
            self.watch_heat = 0.0;
            self.watch_t = 0.0;
            self.watch_spent = false;
            // And the play state: no audience, no toys (dropped, not
            // parked, the wave-1 rule).
            self.clear_play();
            // A hide mid-flight drops the WHOLE trip, second bound and
            // landing recovery included: a stranded `bound2` latch used to
            // dress the next ordinary landing in the touch-land coil and
            // then relaunch a screen-crossing bound at a target from before
            // the hide (review, 2026-08-10).
            self.bound2 = None;
            self.land_t = 0.0;
            self.land_span = 0.0;
            self.skid_dir = 0.0;
            // THE DOC's `:2402` find — the live arc a hidden pet was still
            // flying — is NOT fixed here. It was fixed upstream, and better:
            // `retired_flight_lift` / `deferred_hidden_landing` (below) keep
            // the authored parabola while the body fades, land it invisibly
            // if the fade completes, and hand continuity back to the
            // follower if the caret returns first. Snapping the paws to the
            // destination here would teleport the still-visible body and
            // steal the arc that custody needs.
            // …and the pose a re-anchor hop owed back: no audience, no debt.
            self.resume = None;
            // A hidden caret retires the handoff outright: the park landed
            // (or will land) at zero alpha above, and departing bodies are
            // dropped, not parked — no audience, no theater (the wave-1
            // rule), and nothing left to pin `needs_frames` on a hidden cat.
            self.departures = [None; PET_DEPARTURES_MAX];
            self.handoff_parked_clock = None;
            self.arrive_t = 0.0;
            // Micro-life sleeps with the audience gone.
            self.twitch_t = 0.0;
            self.last_burst = false;
            // No caret, no gaze: a hidden cursor is not a thing to look at,
            // and a dwell it was earning dies with it.
            self.sit_front = false;
            self.peek = false;
            self.gaze_dwell = 0.0;
            // The wave-4 comedy dies with the audience too.
            self.tennis = false;
            self.stumble_t = 0.0;
            self.braking = false;
            self.brake_over = None;
            self.leg_dist = 0.0;
            self.wriggle_t = 0.0;
            self.hide_to = None;
            self.hiding = false;
            self.enter_settled(dt);
            return self.emit(sense, width);
        };
        // The caret returned while the body was still fading, so the deferred
        // invisible landing no longer has authority. Keep the live position;
        // the existing first-sighting follower below walks it to the new lawful
        // station without replaying the retired arc.
        let resumed_hidden_flight = self.deferred_hidden_landing.take().is_some();
        if !resumed_hidden_flight && let Some((lift, fall)) = self.retired_flight_lift.as_mut() {
            *lift = (*lift - *fall * dt).max(0.0);
            if *lift == 0.0 {
                self.retired_flight_lift = None;
            }
        }
        if was_invisible {
            // A new sighting samples the palette under its own prospective
            // body. Never inherit a previous resident appearance.
            self.colors = None;
        }
        self.alpha = (self.alpha + dt / FADE_IN).min(1.0);

        // First sighting: materialise at the station rather than sliding in from
        // the origin.
        let prev = self.last_caret;
        if prev.is_none() {
            // Seed the wall side from the lead-free base rule (v̂ is zero on a
            // fresh sighting, so `station_now` agrees with `station` exactly).
            self.wall_side = f32::from(cc) + STATION_LEAD + width > f32::from(sense.cols);
            // …but only MATERIALISE a pet that is actually off the glass.
            //
            // `last_caret` is cleared by the no-caret arm above, and the host
            // feeds `caret: None` for a single frame on any scrollback scroll
            // or DECTCEM hide — so a fully-drawn cat routinely arrives here
            // with the fade-out having barely started. Hard-assigning then is
            // the teleport the owner reported: measured, `(col 31.30, row 20)
            // → (col 4.00, row 8)` between two frames at alpha 241/255. A cat
            // the eye can still see has to WALK; the chase below already
            // aims at this exact station, and the move sensor stays quiet
            // (`prev` is `None`, so nothing reads as a jump) which is what
            // keeps the walk from also firing a fright.
            //
            // Ink-safe either way: a caret that reappears mid-line must not
            // materialise the cat on top of the line.
            if was_invisible {
                let (c, r) =
                    self.station_safe((cr, cc), sense.cols, sense.rows, width, sense.cell_w);
                self.col = c;
                self.row = r;
            }
        }
        self.last_caret = Some((cr, cc));

        // ── the move sensor ────────────────────────────────────────────────
        let moved = prev.is_some_and(|(pr, pc)| pr != cr || pc != cc);
        let mut move_dc = 0.0f32;
        if moved {
            let pcell = prev.expect("moved implies a previous caret");
            // The emulator's wrap fact rides the same diff: it is consumed
            // here and nowhere else, so the whole brain sees a wrap as the
            // one-character move it is.
            let (dr, dc) = Self::caret_delta(pcell, (cr, cc), sense.cols, sense.wrapped);
            move_dc = dc;
            let was_asleep = self.action == PetAction::Sleep;
            self.on_move(dr, dc, f32::from(cc), sense.cols, sense.reduced_motion);
            // Waking pops ONE startled z ahead of the stretch — the sleep
            // thought bursting, not a new stream.
            if was_asleep && self.action == PetAction::Waking {
                self.spawn_wake_pop(width);
            }
            self.quiet = 0.0;
        } else {
            self.quiet += elapsed;
        }

        if sense.reduced_motion {
            // No fade, chase, gait, or arc: the pet simply IS at its station.
            // Reduced motion owns no frame-cadence lane in the host, so leaving
            // the ordinary first-sighting fade at alpha 0 here strands the
            // resident until unrelated redraws happen to advance it. Snap to
            // the static final opacity on this very first live-caret sample.
            self.alpha = 1.0;
            self.col = Self::station(cc, sense.cols, width);
            self.row = f32::from(cr);
            self.speed = 0.0;
            self.flight = None;
            self.land_t = 0.0;
            self.hop_crouch = false;
            // A re-anchor hop cancelled mid-air owes nothing: reduced motion
            // has no poses to hand back to, only the station.
            self.resume = None;
            self.flinch_t = 0.0;
            self.vhat = 0.0;
            self.pending_wall_transit = false;
            // Reduced motion plays NO choreography and spawns NO particles.
            self.pending_big_jump = false;
            self.big_gather = false;
            self.bound2 = None;
            self.land_span = 0.0;
            self.motes = [None; PET_MOTES_MAX];
            // The wave-1 stimuli are BOOKKEEPING ONLY here: the contentment
            // a pet would have bought still lands (the exit-status ledger
            // already moved at note time), but no wake, no droop, no purr
            // hold, and no motes — the tests' motion contract.
            self.content = (self.content + f32::from(self.pending_pet) * PET_CONTENT).min(1.0);
            self.pending_pet = 0;
            self.pet_at = None;
            self.pending_bell = None;
            self.pending_cheer = None;
            self.pending_sulk = false;
            self.pet_hold_t = 0.0;
            // Wave 2 is theater and theater only: no perk at a stream under
            // reduced motion, no pointer play, and no heat left behind to
            // fire either later.
            self.watch_heat = 0.0;
            self.watch_t = 0.0;
            self.watch_spent = false;
            self.clear_play();
            // No micro-life either: the bob is motion.
            self.twitch_t = 0.0;
            self.last_burst = false;
            // BREED HANDOFF under reduced motion: the parked look applies
            // IMMEDIATELY (the departure is theater; the costume is state),
            // and no departing body ever spawns — or survives a mid-flight
            // switch into reduced motion.
            if let Some(pair) = self.pending_worn.take() {
                self.worn = Some(pair);
            }
            self.handoff_parked_clock = None;
            self.departures = [None; PET_DEPARTURES_MAX];
            self.arrive_t = 0.0;
            self.action = if self.quiet >= SLEEP_AFTER {
                PetAction::Sleep
            } else {
                PetAction::Sit
            };
            return self.emit(sense, width);
        }

        // ── reversal bookkeeping (the frolic detector) ─────────────────────
        self.reversal_t += dt;
        if self.reversal_t > FROLIC_WINDOW {
            self.reversals = 0;
            self.reversal_t = 0.0;
        }
        // The held-delete window (capped so an idle hour cannot overflow it).
        self.retreat_gap = (self.retreat_gap + dt).min(60.0);
        // v̂ decays every tick; observed moves add their impulse in `on_move`.
        // A pause therefore eases the lead back over VEL_TAU, and the station
        // drifts home to caret+1 instead of staying parked out ahead.
        self.vhat *= (-dt / VEL_TAU).exp();
        // The rhythm run's clock (capped like `retreat_gap`).
        self.run_t = (self.run_t + dt).min(60.0);
        if self.run_t > RHYTHM_WINDOW {
            self.run_count = 0;
        }

        self.action_t += dt;
        self.land_t = (self.land_t - dt).max(0.0);
        self.flinch_t = (self.flinch_t - dt).max(0.0);
        self.pet_hold_t = (self.pet_hold_t - dt).max(0.0);
        // The wave-1 latch TTLs, on the injected clock (never dt, which is
        // motion-clamped): a bell that outlived its window mid-flight
        // expires unconsumed; a pet expires two seconds after the LAST click.
        if self.pending_bell.is_some_and(|at| {
            sense.now.saturating_duration_since(at).as_secs_f32() > BELL_LATCH_TTL
        }) {
            self.pending_bell = None;
        }
        if self
            .pet_at
            .is_some_and(|at| sense.now.saturating_duration_since(at).as_secs_f32() > PET_LATCH_TTL)
        {
            self.pending_pet = 0;
            self.pet_at = None;
        }
        // The bat's TTL (wave 2): a peek the pet's work outlasted is not a
        // toy any more — the head has pulled back behind its line.
        if self
            .bat_at
            .is_some_and(|at| sense.now.saturating_duration_since(at).as_secs_f32() > BAT_TTL)
        {
            self.pending_bat = None;
            self.bat_at = None;
        }
        // The look's TTL (wave 3): the far tier retires with the same
        // honesty — old scenery earns no double-take.
        if self
            .look_at
            .is_some_and(|at| sense.now.saturating_duration_since(at).as_secs_f32() > LOOK_TTL)
        {
            self.pending_look = None;
            self.look_at = None;
        }
        // PERK-AND-WATCH heat (wave 2): pure per-tick bookkeeping, integrated
        // unconditionally (like the flinch envelope) so a burst that lands
        // mid-hold still charges the watch the pet enters once it is free.
        self.watch_heat = if sense.output_burst {
            (self.watch_heat + dt * WATCH_RISE).min(1.0)
        } else {
            (self.watch_heat - dt * WATCH_FALL).max(0.0)
        };
        if self.watch_heat < WATCH_GATE {
            // Under the gate the watch is over (or never began): the cap
            // clock rewinds and the spent latch re-arms — the NEXT stream
            // earns a fresh stare.
            self.watch_t = 0.0;
            self.watch_spent = false;
        }
        // IDLE MICRO-LIFE (wave 2), the ear twitch: an output PULSE — the
        // burst's rising edge — at a settled, awake pet still below the
        // watch gate flicks one ear (which ear: mote-serial parity at arm
        // time, the wave-1 variation law). A pulse that grows into a real
        // stream hands over to the watch the moment the heat crosses the
        // gate; a sleeper's ears stay down with the rest of it.
        let burst_edge = sense.output_burst && !self.last_burst;
        self.last_burst = sense.output_burst;
        self.twitch_t = (self.twitch_t - dt).max(0.0);
        self.stumble_t = (self.stumble_t - dt).max(0.0);
        self.bored_cool = (self.bored_cool - dt).max(0.0);
        self.arrive_t = (self.arrive_t - dt).max(0.0);
        if burst_edge
            && self.twitch_t <= 0.0
            && self.watch_heat < WATCH_GATE
            && self.action.settled()
            && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
        {
            self.twitch_t = TWITCH_DUR;
            self.twitch_up = self.mote_serial.is_multiple_of(2);
            self.mote_serial = self.mote_serial.wrapping_add(1);
        }
        // POINTER PLAY (wave 2): the pet's own pointer sensor — velocity EMA
        // over DASH_WINDOW, attention heat, the dash clock, and (when a dash
        // matures near a happy cat) the pounce LATCH. Bookkeeping only;
        // consumption waits for the ground like every other intent.
        match sense.pointer {
            // A non-finite pointer is treated as NO pointer (codex review,
            // 2026-08-10): NaN would flow through the velocity EMAs into the
            // pounce aim and the chase target, and no clamp downstream can
            // recover a poisoned coordinate.
            Some((px, py)) if px.is_finite() && py.is_finite() => {
                if let Some((lx, ly)) = self.last_pointer
                    && dt > 1e-4
                {
                    let k = 1.0 - (-dt / DASH_WINDOW).exp();
                    self.pointer_vx += ((px - lx) / dt - self.pointer_vx) * k;
                    self.pointer_vy += ((py - ly) / dt - self.pointer_vy) * k;
                    // Speed is smoothed as a MAGNITUDE, separately from the
                    // vector: a circling toy nets the vector to zero, and a
                    // cat absolutely watches a circling toy.
                    let inst = ((px - lx) / dt).hypot((py - ly) / dt);
                    self.pointer_speed += (inst - self.pointer_speed) * k;
                }
                self.last_pointer = Some((px, py));
                let speed = self.pointer_speed;
                self.pointer_heat = if speed >= GAZE_FOLLOW_MIN / GAZE_FOLLOW_WINDOW {
                    (self.pointer_heat + dt * POINTER_RISE).min(1.0)
                } else {
                    (self.pointer_heat - dt * POINTER_FALL).max(0.0)
                };
                if self.pointer_heat < POUNCE_REARM {
                    self.pointer_armed = true; // the dash is over: re-arm
                }
                self.dash_t = if speed >= DASH_SPEED {
                    self.dash_t + dt
                } else {
                    0.0
                };
                let range = (px - (self.col + width * 0.5)).abs();
                if self.pointer_armed
                    && self.dash_t >= DASH_MIN_T
                    && self.content >= PLAY_CONTENT
                    && range <= POUNCE_RANGE
                    && !self.pending_pounce
                    && !self.pending_big_jump
                {
                    // The keep-ahead aim: land where the dasher will BE.
                    self.pointer_armed = false;
                    self.dash_t = 0.0;
                    self.pending_pointer_pounce = Some((
                        px + self.pointer_vx * LEAD_TIME,
                        py + self.pointer_vy * LEAD_TIME,
                    ));
                }
                // POINTER PURSUIT (wave 3): the tease band. Entry, stamina,
                // and the catch all live HERE in the sensor; the steering is
                // a target override below (the follower controller does the
                // running — no new motion vocabulary).
                self.pursuit_cool = (self.pursuit_cool - dt).max(0.0);
                let dist = (px - (self.col + width * 0.5))
                    .abs()
                    .max((py - self.row).abs() * 2.0);
                match self.pursuit_t {
                    None => {
                        if self.pursuit_cool <= 0.0
                            && self.pointer_heat >= PURSUIT_HEAT
                            && (PURSUIT_MIN_SPEED..DASH_SPEED).contains(&speed)
                            && dist <= PURSUIT_RANGE
                            && dist > PURSUIT_CATCH
                            && self.content >= PLAY_CONTENT
                            && self.quiet >= PURSUIT_QUIET
                            && self.flight.is_none()
                            && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
                        {
                            self.pursuit_t = Some(0.0);
                        }
                    }
                    Some(t) => {
                        let t = t + dt;
                        self.pursuit_t = Some(t);
                        if self.quiet < PURSUIT_QUIET {
                            // Typing resumed mid-stride: the game is over,
                            // home base calls (the travel latch also clears
                            // this — two doors, one law).
                            self.pursuit_t = None;
                            self.pursuit_cool = self.pursuit_cool.max(1.0);
                        } else if speed < PURSUIT_MIN_SPEED * 0.5 && dist <= PURSUIT_CATCH {
                            // THE CATCH: the toy slowed inside paw reach —
                            // the chase ends in the bat visit (the swipe the
                            // peek machinery already owns), with the greet
                            // parity for variety.
                            self.pursuit_t = None;
                            self.pursuit_cool = PURSUIT_COOL;
                            self.bat_greet = !self.mote_serial.is_multiple_of(2);
                            self.mote_serial = self.mote_serial.wrapping_add(1);
                            self.pending_bat = Some((px, py));
                            self.bat_at = Some(sense.now);
                        } else if t > PURSUIT_MAX_T || dist > PURSUIT_RANGE * 1.5 {
                            // Stamina spent, or the toy left the yard: break
                            // off with dignity — the groom is owed, the walk
                            // home is the ordinary chase.
                            self.pursuit_t = None;
                            self.pursuit_cool = PURSUIT_COOL;
                            self.groom_owed = true;
                        }
                    }
                }
            }
            _ => {
                // Outside the pane the pointer does not exist (and a
                // non-finite pointer is treated the same): heat clears
                // (the gaze parks) and a latched pounce dies with its toy —
                // and so do the chase and the stakeout (wave 3): no toy, no
                // game, no cooldown owed.
                self.last_pointer = None;
                self.pointer_vx = 0.0;
                self.pointer_vy = 0.0;
                self.pointer_speed = 0.0;
                self.pointer_heat = 0.0;
                self.dash_t = 0.0;
                self.pending_pointer_pounce = None;
                self.pursuit_t = None;
                // The stakeout flag is NOT cleared here: the hold arm owns
                // the stand-down (an orphaned hunting crouch would fall
                // through to the launch coil — the phantom leap).
            }
        }

        // The wall is hysteretic, and a side flip is a TRANSIT over the caret
        // column — latched here, consumed in the chase (a flight in progress
        // finishes first; its landing recomputes the wall next tick).
        if self.flight.is_none() && self.update_wall(cc, sense.cols, width) {
            self.pending_wall_transit = true;
        }
        // The stand carries its own row: the ink ladder's rule 3 seats the cat
        // one line off the caret when the caret's own line has no ground near
        // it, and the row hop below is what makes that read as a cat stepping
        // down rather than a cat blinking somewhere else.
        let (target, target_row) =
            self.station_safe((cr, cc), sense.cols, sense.rows, width, sense.cell_w);

        // ── the ink eviction (gauntlet F1, the systemic root) ──────────────
        // A grounded pose froze its feet while prompts printed and typing ran
        // UNDER it: the notice-gather on "jumpline", the groom on the W's,
        // the cheer straddling two inked prompt rows, the droop skulking over
        // the failed command, the wake atop "printf". The moment glyphs
        // invade a grounded footprint, the pose re-anchors to the ink-safe
        // station — and it TRAVELS there. A step aside glides
        // ([`Self::evict_toward`]); anything further, or any change of row,
        // is a HOP, the pet's own vocabulary for changing where it is. The
        // pose and its hold clock are borrowed for the arc and handed back
        // on landing (`resume`), so F1's "pose and hold kept, feet moved"
        // still holds — the sleeper wakes up in mid-air and goes straight
        // back to sleep on the far side, the droop resumes grieving where
        // the grief can be READ. A live watcher re-stations inside its own
        // row instead — the hug below owns its row choice. Travel states are
        // exempt: a flight repositions by construction, and its aim is
        // already ink-safe at launch.
        //
        // A SETTLED pose with travel already latched is exempt too, and the
        // exemption is load-bearing: evicting it would snap the pet to the
        // very station the jump is aimed at, zero the gap, and silently eat
        // the whole choreography. The launch is the eviction there — and the
        // notice-hold bypass below keeps it from loitering on the words
        // first. One-shot holds (droop, startle, frolic, the wake stretch)
        // keep their eviction even with travel latched, because they hold
        // the ground for seconds either way.
        if self.flight.is_none()
            && !(self.action.settled() && (self.pending_pounce || self.pending_big_jump))
            && matches!(
                self.action,
                PetAction::Sleep
                    | PetAction::Waking
                    | PetAction::Sit
                    | PetAction::Loaf
                    | PetAction::Purr
                    | PetAction::Groom
                    | PetAction::Perk
                    | PetAction::Stand
                    | PetAction::Startle
                    | PetAction::Frolic
                    | PetAction::Droop
            )
            && self.ink_overlaps(self.col, self.row, width)
        {
            let watching = self.action == PetAction::Perk
                && self.watch_heat >= WATCH_GATE
                && !self.watch_spent;
            let (safe, safe_row) = if watching {
                // A live watcher re-stations inside its OWN row — the hug
                // below owns its row choice, so the ladder's row rules are
                // not its to take.
                (
                    self.ink_safe_col(self.col, self.row, width, sense.cols),
                    self.row,
                )
            } else {
                self.station_safe((cr, cc), sense.cols, sense.rows, width, sense.cell_w)
            };
            let moved_off = (safe - self.col).abs() > f32::EPSILON;
            let safe_row = if watching { self.row } else { safe_row };
            // Grief replays where it can be READ — but only once the pose has
            // ground to replay ON. Restarting the clock when the law has
            // nowhere to send the pet (a row walled shut on both sides, its
            // neighbours inked) would pin the droop forever. Read BEFORE the
            // hop below borrows the pose, and folded into what the hop owes
            // back, so the grief resumes from its top either way.
            if !watching && self.action == PetAction::Droop && moved_off {
                self.action_t = 0.0;
            }
            if Self::is_re_anchor(self.col, safe, self.row, safe_row) {
                // THE RE-ANCHOR IS A HOP (owner ruling, 2026-08-10: "when the
                // kitty teleports without an animation, it looks bad"). It
                // used to land in ONE frame — up to a screen's width and a
                // row of it — on the argument that gliding a grounded pose
                // across the glass on its belly was a worse lie than the cut.
                // Both are lies; the pet has a third answer and has had it
                // all along. `flight_shape`'s vertical branch is the same
                // curve the "a row change is always a hop" arm uses, so a
                // re-anchor reads as the hop it always should have been,
                // clamped at [`FLIGHT_MAX`] however far it has to go.
                //
                // No gather: the crouch's anticipation is for a jump the pet
                // CHOSE. This one is the world shoving it off its spot, and a
                // shoved cat leaves at once — which is also what keeps the
                // F1 law that a pose may never loiter on the user's words.
                self.resume = Some((self.action, self.action_t));
                self.hop_crouch = false;
                self.big_gather = false;
                let span = (safe - self.col).abs();
                if span > POUNCE_SPAN_MAX {
                    // THE SPAN CAP (the launch law): `flight_shape`'s
                    // vertical branch saturates its duration at
                    // [`FLIGHT_MAX`] whatever the span, so a re-anchor
                    // across a typed-through wrap flew 191 cells in 0.42 s —
                    // 455.7 c/s, measured, the fastest teleport in the
                    // product. Past the lawful pounce span the shove is
                    // answered with `launch_big` instead: ink-safe at its
                    // touch point, apex-clamped, split at [`TWO_BOUND_FRAC`]
                    // — and still with no perk and no gather, exactly as the
                    // big-jump arm's ink collapse skips the notice, because
                    // the no-gather rule above is the point: a shoved cat
                    // leaves at once.
                    self.launch_big(safe, safe_row, sense.cols, width);
                } else {
                    let (dur, arc) = Self::flight_shape(span, true);
                    self.begin_arc(safe, safe_row, dur, arc, false);
                }
            } else {
                // A step aside is a step aside: the column glides and the row
                // does not move at all. `is_re_anchor` already proved the
                // ladder's row is this one (stands are whole rows, so "within
                // half a cell" means "the same"), and assigning it anyway
                // would leave the law with a half-cell of unanimated slack
                // for no behaviour at all.
                self.col = Self::evict_toward(self.col, safe, dt);
            }
        }

        // ── the watch station (gauntlet F5): hug the live edge ─────────────
        // The gauntlet caught the watcher flopped EIGHT ROWS above the live
        // output edge — a cat in old scrollback, not a watcher. While the
        // watch is live and the host has fed the ink map, the station IS the
        // stream's newest inked row: the target swap below rides the
        // existing hop/chase machinery (no new motion vocabulary), with
        // [`WATCH_HUG_ROWS`] of hysteresis so a fast stream reads as pursuit
        // rather than a strobe. Any travel latch drops the override at once —
        // work outranks attention, exactly as the watch always ruled.
        let (target, target_row) = if self.watch_heat >= WATCH_GATE
            && !self.watch_spent
            && !self.pending_pounce
            && !self.pending_big_jump
            && self.flight.is_none()
            && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
            && let Some(live) = self.ink_live_row()
        {
            if (live - self.row).abs() >= WATCH_HUG_ROWS {
                // Re-station beside the newest line's end — blank ground by
                // construction, one clear lead cell off the ink.
                let col = self
                    .ink_span(live)
                    .map(|(_, end)| {
                        (end + STATION_LEAD).clamp(0.0, (f32::from(sense.cols) - width).max(0.0))
                    })
                    .unwrap_or(self.col);
                (col, live)
            } else {
                // Inside the band: the current stand keeps watching.
                (self.col, self.row)
            }
        } else {
            (target, target_row)
        };

        // ── POINTER PURSUIT steering (wave 3) ──────────────────────────────
        // While the chase is live the follower's target IS the toy (lead-
        // corrected, centered) — the same controller, gains and gait laws
        // that follow the caret. Ranked below every travel latch: a caret
        // intent landing this tick reclaims the target before a paw moves.
        let (target, target_row) = if self.pursuit_t.is_some()
            && !self.pending_pounce
            && !self.pending_big_jump
            && self.flight.is_none()
            && let Some((px, py)) = self.last_pointer
        {
            let col = (px + self.pointer_vx * LEAD_TIME - width * 0.5)
                .clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
            let row = if (py - self.row).abs() >= PURSUIT_HUG_ROWS {
                py.round()
                    .clamp(0.0, f32::from(sense.rows.saturating_sub(1)))
            } else {
                self.row
            };
            (col, row)
        } else {
            (target, target_row)
        };

        // ── HIDE-BEHIND-WORDS walk (wave 4c): the target is the spot behind
        // the word; arrival drops into the crouched hide. Any travel latch
        // clears the trip (the play-clear below).
        let (target, target_row) = if let Some(dest) = self.hide_to
            && !self.pending_pounce
            && !self.pending_big_jump
            && self.flight.is_none()
        {
            if (dest - self.col).abs() <= ARRIVED {
                self.hide_to = None;
                self.hiding = true;
                self.hide_t = HIDE_DWELL;
                self.set_action(PetAction::Crouch);
            }
            (dest, self.row)
        } else {
            (target, target_row)
        };

        // ── flight owns the position while it lasts ────────────────────────
        if let Some(mut f) = self.flight {
            // The ONE mid-air re-aim: a large caret move while airborne
            // retargets the landing column at the newly predicted station —
            // never the duration, and never twice. Small moves still only
            // latch `pending_pounce` (the interruption law is unchanged).
            if move_dc.abs() >= POUNCE_JUMP && !f.reaimed {
                f.reaimed = true;
                let t_rem = (f.dur - f.t).max(0.0) + LAND_DUR;
                let predict = if self.rhythm_open() {
                    self.vhat * t_rem
                } else {
                    0.0
                };
                // Column only: an in-flight re-aim keeps the row it launched
                // at, because the arc's shape, its landing recovery and any
                // second bound were all cut against that row. A ladder row
                // the flight did not take is picked up by the eviction one
                // tick after the paws are down.
                let aim = self
                    .station_safe((cr, cc), sense.cols, sense.rows, width, sense.cell_w)
                    .0;
                let to = (aim + predict).clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
                // REBASE, or the re-aim IS a teleport.
                //
                // Position on an arc is `from + (to - from)·u`, so moving `to`
                // under a live `u` moves the cat THIS FRAME by `(to - to')·u`
                // — mid-flight, that is half the retarget distance in one
                // tick. Measured by the law test before this line existed: a
                // Leap crossing 35 cells between two frames, 76.10 -> 41.04,
                // against a bound of 1.51. A re-aim changes where the arc is
                // GOING; it may never change where the cat IS.
                //
                // Solve for the launch point that keeps the current drawn
                // position on the new curve at the CURRENT `u` (this runs
                // before `f.t` advances, so `u` is the frame already on
                // glass): `from' + (to' - from')·u == col`.
                let u_now = (f.t / f.dur).clamp(0.0, 1.0);
                if u_now <= REAIM_MAX_U {
                    f.from_col = (self.col - to * u_now) / (1.0 - u_now);
                    f.to_col = to;
                }
                // Past the bar the rebase is skipped entirely rather than
                // clamped: `1 - u` is vanishing there, so the solved launch
                // point runs away to infinity, and a landing that close is
                // better served by letting the flight finish and the chase
                // pick up the new station on its feet.
                // A re-aimed FIRST bound orphans its second: the split was
                // computed against the old target, and keeping it would land
                // the cat at the new station only to bound all the way back
                // to a caret that no longer exists (review, 2026-08-10).
                // The show degrades to one bound — a re-aim already is the
                // exceptional path.
                self.bound2 = None;
            }
            f.t += dt;
            let u = (f.t / f.dur).clamp(0.0, 1.0);
            self.col = f.from_col + (f.to_col - f.from_col) * u;
            self.row = f.from_row + (f.to_row - f.from_row) * u;
            self.speed = (f.to_col - f.from_col) / f.dur;
            if u >= 1.0 {
                // A pounce is exertion too: it buys contentment exactly like the
                // same distance run, or a cat that only ever jumps would never
                // earn a purr.
                let span = (f.to_col - f.from_col).abs();
                self.content = (self.content + span * CONTENT_PER_CELL).min(1.0);
                self.flight = None;
                if f.big && self.bound2.is_some() {
                    // The touch-land between the two bounds of a split
                    // crossing: brief, light, and the relaunch consumes
                    // `bound2` the moment it expires.
                    self.land_t = TOUCH_LAND_DUR;
                    self.land_span = 0.0;
                } else if f.big {
                    // The heavy landing: weighted squash, skid, and dust at
                    // the heels (the delete poof's puff, at pet scale).
                    self.land_t = BIG_LAND_DUR;
                    self.land_span = span.max(1.0);
                    self.skid_from = self.col;
                    self.skid_dir = if f.to_col > f.from_col {
                        1.0
                    } else if f.to_col < f.from_col {
                        -1.0
                    } else {
                        0.0
                    };
                    self.spawn_dust(width);
                } else {
                    self.land_t = LAND_DUR;
                    self.land_span = 0.0;
                }
                self.set_action(PetAction::Land);
            } else {
                f.t = f.t.min(f.dur);
                self.flight = Some(f);
                self.set_action_keep(PetAction::Leap);
            }
            return self.emit(sense, width);
        }

        // The second bound of a split crossing: its touch-land just expired,
        // so the pet leaves the ground again — before any other rule can
        // claim the tick (the split is ONE choreography, not two decisions).
        if self.action == PetAction::Land
            && self.land_t <= 0.0
            && let Some((c2, r2)) = self.bound2.take()
        {
            self.begin_big_arc(c2, r2);
            return self.emit(sense, width);
        }

        // A RE-ANCHOR hop's landing recovery just expired: the borrowed pose
        // comes back with its clock, which is what keeps F1's "pose and hold
        // kept" true across an arc that necessarily replaced the pose to draw
        // the cat in the air.
        //
        // Unconditional, unlike the play flourish below — and that is the
        // point. This is not a reward the pet has earned and can be talked
        // out of; it is the state it was already IN, which a hop borrowed for
        // a third of a second. A latched pounce is not lost by handing it
        // back: the one-shot holds have always outranked travel ("they hold
        // the ground for seconds either way"), so the wake stretch plays out
        // and the pounce fires after it, exactly as it would have if the
        // world had never inked the cat's spot.
        if self.action == PetAction::Land
            && self.land_t <= 0.0
            && let Some((pose, held)) = self.resume.take()
        {
            self.set_action(pose);
            self.action_t = held;
            return self.emit(sense, width);
        }

        // A PLAY flight's landing recovery just expired (wave 2): the owed
        // flourish plays — unless caret work latched meanwhile, in which
        // case the flourish is skipped outright (work first) and the ladder
        // below consumes the travel intent as ever.
        if self.action == PetAction::Land && self.land_t <= 0.0 && self.play_land.is_some() {
            let owed = self.play_land.take().expect("checked above");
            if !self.pending_pounce && !self.pending_big_jump {
                match owed {
                    PlayLand::PointerFrolic => {
                        self.play_frolic = true;
                        self.play_hold = POINTER_PLAY_HOLD;
                        self.set_action(PetAction::Frolic);
                        return self.emit(sense, width);
                    }
                    PlayLand::BatSwipe { col, row } => {
                        // Face the head — then SWIPE or GREET by the serial
                        // parity latched at note time (wave 3): the swipe
                        // pins the bat pose and puffs contact dust; the
                        // greet leaves the playbow alternation to play, no
                        // contact (a bow is not a hit).
                        self.facing_left = col < self.col + width * 0.5;
                        self.play_frolic = true;
                        self.swipe = !self.bat_greet;
                        self.play_hold = BAT_HOLD;
                        self.set_action(PetAction::Frolic);
                        if !self.bat_greet {
                            self.spawn_bat_dust(col, row);
                        }
                        return self.emit(sense, width);
                    }
                }
            }
        }

        // ── one-shot holds ─────────────────────────────────────────────────
        match self.action {
            PetAction::Crouch if self.big_gather => {
                // The big jump's gather: the butt-wiggle plays through
                // BIG_CROUCH_DUR, then the bound (or bounds) launch at the
                // freshly re-read target. Over INK the wiggle is cut to the
                // pounce's quick coil (gauntlet F1) — a blink of gather, then
                // gone off the words.
                let dur = if self.ink_overlaps(self.col, self.row, width) {
                    CROUCH_DUR
                } else {
                    BIG_CROUCH_DUR
                };
                if self.action_t >= dur {
                    self.big_gather = false;
                    self.launch_big(target, target_row, sense.cols, width);
                }
                return self.emit(sense, width);
            }
            PetAction::Crouch if self.hop_crouch => {
                // The Enter hop's one-tick anticipation: gathered last frame,
                // airborne this one. The target is re-read fresh, so a caret
                // that moved again during the gather is still the one chased.
                self.hop_crouch = false;
                self.begin_flight(target, target_row, true, sense.cols, width);
                return self.emit(sense, width);
            }
            PetAction::Loaf if self.wriggle_t > 0.0 => {
                // THE WRIGGLE (wave 4b): rolling around on its back — held
                // here, drawn by `emit` (frame flip-flop + belly bob), and
                // finished with the waking stretch like any nap would be.
                self.wriggle_t = (self.wriggle_t - dt).max(0.0);
                self.speed = 0.0;
                if self.wriggle_t <= 0.0 {
                    self.set_action(PetAction::Waking);
                }
                return self.emit(sense, width);
            }
            PetAction::Sit if self.tennis => {
                // THE TENNIS WATCH holds the sit while the rally lives; the
                // facing already tracks each hit in `on_move`. The rally
                // lapsing (or the caret going quiet enough to sleep) stands
                // the watch down into the ordinary settle.
                if (self.clock - self.tennis_last) as f32 <= TENNIS_LAPSE {
                    self.speed = 0.0;
                    return self.emit(sense, width);
                }
                self.tennis = false;
            }
            PetAction::Crouch if self.hiding => {
                // THE HIDE (wave 4c): lurking behind the word — drawn under
                // the glyphs (PetFrame::under_ink), dwelling its beat, then
                // strolling home. Any caret work cleared it long before this
                // arm (the travel clear); the dwell is the only exit here.
                self.hide_t -= dt;
                if self.hide_t > 0.0 {
                    self.speed = 0.0;
                    return self.emit(sense, width);
                }
                self.hiding = false;
                self.set_action_keep(PetAction::Stand);
                return self.emit(sense, width);
            }
            PetAction::Crouch if self.stakeout => {
                // THE STAKEOUT (wave 3): the hunting crouch at a creeping
                // toy — held while the toy stays slow and near, INCLUDING a
                // toy that stops dead (a cat freezes at stopped prey), up to
                // its own patience. A latched dash pounce converts the
                // stakeout into the strike (the same play flight the pounce
                // always was); anything else lapsing stands the cat back up,
                // never the launch coil.
                if let Some((ac, ar)) = self.pending_pointer_pounce.take() {
                    self.stakeout = false;
                    let to_col =
                        (ac - width * 0.5).clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
                    let to_row = ar.clamp(0.0, f32::from(sense.rows.saturating_sub(1)));
                    self.play_to = Some((to_col, to_row));
                    self.play_land = Some(PlayLand::PointerFrolic);
                    return self.emit(sense, width);
                }
                self.stakeout_t += dt;
                // A toy speeding out of the creep band HANDS OFF: the chase
                // (or the dash) takes it from here — standing down without a
                // cool so the sensor may bite this very second.
                if self.pursuit_t.is_some() || self.pointer_speed >= PURSUIT_MIN_SPEED {
                    self.stakeout = false;
                    self.set_action_keep(PetAction::Stand);
                    return self.emit(sense, width);
                }
                let live = match self.last_pointer {
                    Some((px, py)) => {
                        let dist = (px - (self.col + width * 0.5))
                            .abs()
                            .max((py - self.row).abs() * 2.0);
                        self.facing_left = px < self.col + width * 0.5;
                        dist <= STALK_RANGE * 1.25 && self.stakeout_t < STAKEOUT_MAX
                    }
                    None => false,
                };
                if live {
                    self.speed = 0.0;
                    return self.emit(sense, width);
                }
                // Patience spent (or the toy left): stand down with a short
                // cool so the same creep cannot re-pin the crouch instantly.
                self.stakeout = false;
                self.pursuit_cool = self.pursuit_cool.max(2.0);
                self.set_action_keep(PetAction::Stand);
                return self.emit(sense, width);
            }
            PetAction::Groom if self.groom_owed && self.action_t < GROOM_HOLD => {
                // Post-chase dignity (wave 3): the break-off's groom holds
                // its beat, then the ladder resumes — the walk home already
                // happened; this is the sitting-down-and-pretending-that-
                // was-on-purpose part.
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Groom if self.groom_owed => {
                self.groom_owed = false;
            }
            PetAction::Crouch if self.action_t >= CROUCH_DUR => {
                if let Some((tc, tr)) = self.play_to.take() {
                    // A PLAY launch (wave 2): the same coiled crouch, aimed
                    // at the toy instead of the station — and deliberately
                    // WITHOUT the keep-ahead caret prediction, which models
                    // typing, not a dasher (the aim already carries the
                    // pointer's own lead). The launch law's span cap applies
                    // to play like any other ordinary flight: a toy further
                    // than [`POUNCE_SPAN_MAX`] is struck short and the
                    // pursuit closes the rest on paws.
                    let vertical = (tr - self.row).abs() >= 0.5;
                    let tc = self.col + (tc - self.col).clamp(-POUNCE_SPAN_MAX, POUNCE_SPAN_MAX);
                    let (dur, arc) = Self::flight_shape((tc - self.col).abs(), vertical);
                    self.begin_arc(tc, tr, dur, arc, false);
                } else {
                    // THE SCALED COIL (rung 7): a longer jump gathers
                    // longer — the hold stretches past CROUCH_DUR to the
                    // live span's coil_dur, refreshed every tick because
                    // the aim is live (the launch below re-reads the
                    // station, so its wind-up must track the same target).
                    // `emit` reads the refreshed field for the envelope.
                    self.coil_d = Self::coil_dur((target - self.col).abs());
                    if self.action_t < self.coil_d {
                        return self.emit(sense, width);
                    }
                    self.launch(target, target_row, sense.cols, width);
                }
                return self.emit(sense, width);
            }
            PetAction::Crouch => return self.emit(sense, width),
            PetAction::Startle if self.action_t < STARTLE_HOLD => {
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Frolic if self.play_frolic => {
                // A play flourish (wave 2) holds for its OWN beat, then
                // falls through to the chase — the walk home, earning
                // content per cell like any other travel.
                if self.action_t < self.play_hold {
                    self.speed = 0.0;
                    return self.emit(sense, width);
                }
                self.play_frolic = false;
                self.play_hold = 0.0;
                self.swipe = false;
            }
            PetAction::Frolic if self.action_t < FROLIC_HOLD => {
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Droop if self.action_t < DROOP_HOLD => {
                // The sulk: feet planted through the whole hold, no motes.
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Purr if self.pet_hold_t > 0.0 => {
                // The petting hold — Purr on the glass whatever the ledger
                // says. The guard is the timer, not `action_t`, so an
                // earned purr (`enter_settled`) is untouched by this arm.
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Waking if self.action_t < WAKE_DUR => {
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Perk if self.action_t < PERK_HOLD => {
                // The double-take never LOITERS on ink (gauntlet F1,
                // b02_right_025: the notice-gather squatting on "jumpline"):
                // with travel latched and glyphs underfoot, the hold
                // collapses and the launch ladder below takes over this
                // very tick. On blank ground the notice keeps its beat.
                if !((self.pending_pounce || self.pending_big_jump)
                    && self.ink_overlaps(self.col, self.row, width))
                {
                    self.speed = 0.0;
                    return self.emit(sense, width);
                }
            }
            PetAction::Land if self.land_t > 0.0 => {
                // Landing recovery: the pet is on its feet but still absorbing.
                // A BIG landing also skids — 0.8 cells of exponentially dying
                // slide the flight aimed short by, so the paws come to rest
                // exactly on the station they were thrown at.
                if self.land_span > 0.0 && self.skid_dir != 0.0 {
                    let u = (1.0 - self.land_t / BIG_LAND_DUR).clamp(0.0, 1.0);
                    let s = (1.0 - (-4.0 * u).exp()) / (1.0 - (-4.0f32).exp());
                    self.col = self.skid_from + self.skid_dir * SKID_CELLS * s;
                }
                // THE RECOVERY (rung 8): the landing steps off on a CONTACT
                // frame. While the body absorbs, the stride eases to the
                // nearest foot-down frame under `enter_settled`'s own law
                // (`1 − exp(−dt/0.09)`) — the gait otherwise resumed at the
                // phase frozen at launch, mid-swing, paws sliding on ground
                // they never covered: the moon-walk the module doc forbids.
                // Eased, never snapped — a snap is the same crime paid in
                // one frame (up to half a stride of foot travel for zero
                // cells of ground).
                let planted = self.stride.round();
                self.stride += (planted - self.stride) * (1.0 - (-dt / 0.09).exp());
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            _ => {}
        }

        let gap = target - self.col;

        // ── the screen-crossing jump ───────────────────────────────────────
        // Consumed BEFORE the row hop (a big jump owns its row change — the
        // flight carries `to_row`) and after every hold, like the pounce
        // latch. Three ways in: the latched single move, a standing gap of
        // half the grid once a hold resolves, and the latched joy variant.
        // The choreography is always notice → wiggle → bound(s): a pet not
        // already perked perks first, and the gather begins when that hold
        // has played out.
        if (self.pending_big_jump
            || (self.pursuit_t.is_none() && gap.abs() >= BIG_GAP_FRAC * f32::from(sense.cols)))
            && gap.abs() > ARRIVED
        {
            // On ink the notice is SKIPPED outright (gauntlet F1): the perk
            // and the long wiggle are loitering, and loitering on the user's
            // words is the one thing an event pose may never do. The gather
            // still plays — cut to the pounce's quick coil by the arm above.
            if self.action == PetAction::Perk || self.ink_overlaps(self.col, self.row, width) {
                self.pending_big_jump = false;
                self.pending_pounce = false;
                self.hop_crouch = false;
                self.big_gather = true;
                self.set_action(PetAction::Crouch);
            } else {
                self.pending_big_jump = true;
                self.set_action(PetAction::Perk);
            }
            return self.emit(sense, width);
        }
        self.pending_big_jump = false;

        // ── a row change is always a hop ───────────────────────────────────
        if (target_row - self.row).abs() >= 0.5 {
            // One tick of gathered crouch first — the anticipation that makes
            // the hop read as a jump rather than a launch from flat feet.
            self.hop_crouch = true;
            // A stale big gather must not hijack this hop into a bound.
            self.big_gather = false;
            self.set_action(PetAction::Crouch);
            return self.emit(sense, width);
        }

        // ── the chase ──────────────────────────────────────────────────────

        // A latched wall transit crosses the caret column: a cat does not
        // walk through your cursor, it hops it — one small quick arc to the
        // far side. Consumed here (after every hold) like the pounce latch.
        if self.pending_wall_transit {
            self.pending_wall_transit = false;
            if gap.abs() > ARRIVED {
                self.hop_crouch = false;
                // THE SPAN CAP (the launch law): the transit's duration is
                // FIXED, so the span is what carries the rate — a latch that
                // survived a hold used to cross whatever gap had opened
                // meanwhile in one 0.25 s arc, span-blind and unbounded.
                // The hop reaches at most [`WALL_SPAN_MAX`] cells; the
                // remaining gap falls to the ladder on later ticks (the
                // pounce band, the follower). The row-hop arm above already
                // owns any real row change, so `target_row` here is the row
                // the pet is on.
                let to = if gap.abs() > WALL_SPAN_MAX {
                    self.col + WALL_SPAN_MAX * gap.signum()
                } else {
                    target
                };
                self.begin_arc(to, target_row, WALL_HOP_DUR, WALL_HOP_ARC, false);
                return self.emit(sense, width);
            }
        }

        // Out of walking range: gather and pounce rather than sprinting the
        // whole way. Two ways in — a LATCHED jump (set when the move was seen,
        // so it outlives any hold it landed during) or a standing gap that has
        // simply become too big to run down. Both are consumed here, after every
        // hold, which is what keeps a leap from silently degrading into a walk.
        // A live pursuit keeps the standing-gap doors SHUT: a chase runs on
        // paws (that is what makes it a chase) — only a latched caret intent
        // may still fly.
        //
        // The door is a BAND, not an open half-line (the launch law): a
        // pounce's flight time clamps at [`FLIGHT_MAX`], so a span past
        // [`POUNCE_SPAN_MAX`] cannot be flown lawfully — an uncapped door
        // launched a just-under-half-grid gap at 238 c/s on a 200-column
        // pane, measured. Past the band the arm declines and the gap falls
        // through to the follower's gallop (the big-jump door above already
        // owns the half-grid tier), which carries the cat until the gap
        // shrinks INTO the band and the pounce fires at a speed a body
        // could choose.
        if (self.pending_pounce || (self.pursuit_t.is_none() && gap.abs() >= POUNCE_GAP))
            && gap.abs() > ARRIVED
            && gap.abs() <= POUNCE_SPAN_MAX
        {
            self.pending_pounce = false;
            // A stale hop or big gather (its trigger vanished before launch)
            // must not hijack this pounce's crouch.
            self.hop_crouch = false;
            self.big_gather = false;
            // THE SCALED COIL (rung 7): the gather's length is set by the
            // span it is winding up for — and kept live by the launch arm.
            self.coil_d = Self::coil_dur(gap.abs());
            self.set_action(PetAction::Crouch);
            return self.emit(sense, width);
        }
        self.pending_pounce = false;

        // The gain schedule: a galloping caret gets the stiffer follower
        // (CHASE_GAIN → CHASE_GAIN_FAST as v̂ climbs past RUN_SPEED, fully in
        // by 2×), or the lag alone outruns the clamped lead. The overshoot
        // guard below is what keeps the higher gain from ringing.
        let sched = ((self.vhat.abs() - RUN_SPEED) / RUN_SPEED).clamp(0.0, 1.0);
        let gain = CHASE_GAIN + (CHASE_GAIN_FAST - CHASE_GAIN) * sched;
        let want = (gap.abs() * gain).min(MAX_SPEED) * gap.signum();
        let k = 1.0 - (-dt / SPEED_TAU).exp();
        self.speed += (want - self.speed) * k;
        let step = self.speed * dt;
        self.col += step;
        let travelled = step.abs();
        // The gallop's four frames cover twice the ground per cycle — see
        // [`RUN_STRIDE_CELLS`]. Keyed on the gait the glass is showing (last
        // frame's action), which the hysteresis keeps stable.
        let stride_cells = if self.action == PetAction::Run {
            RUN_STRIDE_CELLS
        } else {
            STRIDE_CELLS
        };
        self.stride = (self.stride + travelled / stride_cells).rem_euclid(1024.0);
        self.content = (self.content + travelled * CONTENT_PER_CELL).min(1.0);

        // THE DRIFT-BRAKE (wave 4): a long gallop that crosses its station
        // blows past it by [`BRAKE_OVERSHOOT`] on purpose, flips to face its
        // mistake, and trots back. Once per leg.
        //
        // The overshoot is a place the pet RUNS to, never a place it is PUT
        // (owner ruling, 2026-08-10). Wave 4 assigned the far side of the
        // station outright, which is up to two cells of pop between two
        // frames — six times what [`MAX_SPEED`] can legally cover in one —
        // during the very moment the gallop is meant to read as momentum.
        // Arming only moves the AIM; the guard below stands down until the
        // paws get there, and the stumble is the same beat it always was.
        if !self.braking
            && (target - self.col).signum() != gap.signum()
            && self.pursuit_t.is_none()
            && self.leg_dist >= BRAKE_DIST
            && self.speed.abs() > RUN_SPEED
            && self.content >= SKIP_CONTENT
        {
            self.braking = true;
            // Wall-clamped like every other aim: an overshoot the viewport
            // will not let the paws reach is one the run-out could never end
            // on, and it would hold the aim off the station indefinitely.
            self.brake_over = Some(
                (target + BRAKE_OVERSHOOT * gap.signum())
                    .clamp(0.0, (f32::from(sense.cols) - width).max(0.0)),
            );
            self.speed *= 0.15;
            self.stumble_t = STUMBLE_DUR;
            self.spawn_dust(width);
        }
        // The run-out is spent the moment the paws reach it: the aim comes
        // home and the cat trots back. `braking` outlives the run-out (it is
        // cleared at arrival) so the trot cannot arm a brake of its own and
        // set the follower wobbling.
        if let Some(over) = self.brake_over
            && (over - self.col).abs() <= ARRIVED
        {
            self.brake_over = None;
        }

        // Overshoot guard: a follower must never oscillate around its aim, so
        // a step that CROSSES the aim stops on it — a clamp, which can only
        // ever shorten the frame's travel.
        let aim = self.brake_over.unwrap_or(target);
        let prev_col = self.col - step;
        if (aim - self.col).signum() != (aim - prev_col).signum() {
            self.col = aim;
            self.speed *= 0.25;
        }

        if self.speed.abs() > FLIP_SPEED {
            self.facing_left = self.speed < 0.0;
        }

        let arrived = (target - self.col).abs() <= ARRIVED && self.speed.abs() < FLIP_SPEED;
        if arrived {
            // The chase leg is over: the drift-brake's odometer rewinds and
            // a live overshoot is done trotting back (wave 4).
            self.leg_dist = 0.0;
            self.braking = false;
            self.brake_over = None;
            // The wave-1 stimuli are consumed HERE — on the ground, below
            // every caret-travel intent (flight, hop, wall transit, pounce,
            // big jump) and after every one-shot hold, exactly like the
            // pounce latch. One stimulus per tick: fright before grief
            // before joy before affection.
            if self.consume_stimuli(cc, width) {
                return self.emit(sense, width);
            }
            // THE WORD-CAT BAT (wave 2): below caret work by construction
            // (this ladder never runs while travel is pending), above
            // pointer play — the peek is the rarer toy.
            if self.consume_bat(&sense, width) {
                return self.emit(sense, width);
            }
            // THE LOOK (wave 3): the far peek's tier — perk and face the
            // head, never travel. Below the bat (a reachable toy beats
            // scenery), above pointer play.
            if let Some(lx) = self.pending_look.take() {
                self.look_at = None;
                if !matches!(self.action, PetAction::Sleep | PetAction::Waking) {
                    self.facing_left = lx < self.col + width * 0.5;
                    if self.action.settled() && self.action != PetAction::Perk {
                        self.set_action(PetAction::Perk);
                    }
                    return self.emit(sense, width);
                }
            }
            // POINTER PLAY (wave 2): a latched dash pounce, below every
            // event stimulus (the caret's ladder never even reaches here
            // while work is pending — work outranks play by construction).
            if self.consume_pointer_pounce(&sense, width) {
                return self.emit(sense, width);
            }
            // BREED HANDOFF (the module's handoff section): a parked look
            // that has stayed stable through the debounce lands NOW, on the
            // ground — the pet reskins in place (position, gait, mood and
            // sleep clock all carried, exactly like a species swap) and the
            // OLD look leaves as a departing body, in the shape the parked
            // verdict owes ([`PetArrival`]): a CEREMONY runs it off toward
            // the nearest edge on the pet's side of the caret (the exit
            // never crosses your cursor); the QUIET RE-DRESS fades it out
            // where it stands. Ranked below every toy (the costume can wait
            // a beat) and above the ambient watch. Round-trip flips cleared
            // the park (and its clock) long before this could fire; a
            // sleeper keeps its coat (waking a cat for a costume change is
            // backwards — the next wake or hide lands it).
            // Deliberately NOT a `return`: the swap is bookkeeping plus a
            // ghost, never a pose — it steals no beat from the ladder.
            if let Some(pair) = self.pending_worn
                && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
                && self
                    .handoff_parked_clock
                    .is_some_and(|at| self.clock - at >= f64::from(HANDOFF_DEBOUNCE))
            {
                let arrival = self.pending_arrival;
                let old = self.worn.unwrap_or(pair);
                self.worn = Some(pair);
                self.pending_worn = None;
                self.handoff_parked_clock = None;
                // A swap between identical pairs (only possible via the
                // `unwrap_or` above on a never-dressed pet) spawns no ghost:
                // two identical kitties on glass would read as a render bug.
                if old != pair {
                    match arrival {
                        PetArrival::Ceremony => {
                            let limit = (f32::from(sense.cols) - width).max(0.0);
                            let edge = if self.col >= f32::from(cc) {
                                limit
                            } else {
                                0.0
                            };
                            // The new cat ARRIVES (fades up over ARRIVE_IN)
                            // while the old one runs off — never a one-frame
                            // recolour.
                            self.arrive_t = ARRIVE_IN;
                            self.arrive_in = ARRIVE_IN;
                            // THE DOOR IS A PROMISE: the sprint is solved so
                            // the run to the edge plus the tail fade fits
                            // DEPART_MAX — a span DEPART_SPEED cannot cover
                            // in time buys a faster ghost, never a
                            // mid-screen dissolve (§1.1(b)).
                            let span = (edge - self.col).abs();
                            let speed = DEPART_SPEED.max(span / (DEPART_MAX - EDGE_FADE));
                            self.spawn_departure(Departure {
                                born: self.lane_clock,
                                coat: old.0,
                                iris: old.1,
                                from_col: self.col,
                                row: self.row,
                                edge,
                                speed,
                                still: false,
                                facing_left: self.facing_left,
                                // The ghost's feet pick up the body's own
                                // gait phase and last pose — no
                                // gallop-from-a-sit pop.
                                stride0: self.stride,
                                pose0: self.last_pose,
                            });
                        }
                        PetArrival::Quiet => {
                            // THE RE-DRESS (THE DOC §2.0.6): a coming-home
                            // cat is not news, so its swap is a 0.25 s
                            // in-place crossfade — the old coat fades out
                            // as a ZERO-SPAN still departure at the pet's
                            // own feet (life() == EDGE_FADE by the shipped
                            // arithmetic) while the new one fades up over
                            // the complementary HOMECOMING_IN. Zero
                            // displacement of either body, no second
                            // silhouette, no run.
                            self.arrive_t = HOMECOMING_IN;
                            self.arrive_in = HOMECOMING_IN;
                            self.spawn_departure(Departure {
                                born: self.lane_clock,
                                coat: old.0,
                                iris: old.1,
                                from_col: self.col,
                                row: self.row,
                                // ZERO SPAN — no run, no travel; the speed
                                // is inert (nothing to cover) and the
                                // still flag keeps the carried facing and
                                // captured pose for the whole fade.
                                edge: self.col,
                                speed: DEPART_SPEED,
                                still: true,
                                facing_left: self.facing_left,
                                stride0: self.stride,
                                pose0: self.last_pose,
                            });
                            // The beat of recognition: one ear-twitch bob,
                            // fired only at a settled pet — stealing a bob
                            // from a run would break the ladder's own
                            // "steals no beat" rule. The Sleep|Waking
                            // exclusion is belt-and-braces: this arm's own
                            // gate already excludes both, but `settled()`
                            // counts Sleep, and a sleeping cat must never
                            // bob for a costume (the shipped ear-twitch
                            // arm's precedent).
                            if self.action.settled()
                                && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
                            {
                                self.twitch_t = TWITCH_DUR;
                                self.twitch_up = self.mote_serial.is_multiple_of(2);
                                self.mote_serial = self.mote_serial.wrapping_add(1);
                            }
                        }
                    }
                }
            }
            // Post-chase dignity (wave 3): an owed groom is consumed on the
            // ground below every toy and above the ambient watch — the
            // break-off already walked home; the groom is the arrival.
            if self.groom_owed && !matches!(self.action, PetAction::Sleep | PetAction::Waking) {
                if self.action != PetAction::Groom {
                    self.set_action(PetAction::Groom);
                }
                return self.emit(sense, width);
            }
            // PERK-AND-WATCH (wave 2): an ambient hold, ranked below every
            // latched stimulus (events beat heat) and above the settle
            // ladder it borrows the ground from.
            if self.consume_watch(dt, cc) {
                return self.emit(sense, width);
            }
            // THE BORED-CAT VIGNETTES (wave 4b): a content cat in boredom's
            // window — past the groom, shy of sleep — demands attention.
            // The serial deals the act: bat the cursor, ATTACK the cursor
            // (two swipes), or roll around on its back. One per cooldown,
            // never during a game, never over a live toy.
            if self.bored_cool <= 0.0
                && self.wriggle_t <= 0.0
                && !self.tennis
                && self.pursuit_t.is_none()
                && !self.stakeout
                && self.pointer_heat <= 0.0
                && self.quiet >= BORED_AFTER
                && self.quiet < BORED_UNTIL
                && self.content >= PLAY_CONTENT
                && self.action.settled()
                && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
            {
                self.bored_cool = BORED_COOL;
                let deal = self.mote_serial % 4;
                self.mote_serial = self.mote_serial.wrapping_add(1);
                // HIDE (deal 3): duck behind a word on this row, if one is
                // near enough — otherwise the deal falls back to the bat.
                if deal == 3
                    && let Some((first, end)) = self.ink_span(self.row)
                    && end - first >= 2.0
                {
                    let dest = (end - width * 0.7).max(first - width * 0.3);
                    if (dest - self.col).abs() <= HIDE_RANGE {
                        self.hide_to = Some(dest);
                        return self.emit(sense, width);
                    }
                }
                if deal == 2 {
                    // ROLL AROUND: flop over and wriggle (the emit fakes the
                    // roll with the sleep/loaf frames flip-flopping).
                    self.wriggle_t = WRIGGLE_DUR;
                    self.set_action(PetAction::Loaf);
                } else {
                    // BAT (deal 0) or double-swipe ATTACK (deal 1) at the
                    // cursor: face it, swipe, puff the contact dust at the
                    // caret cell itself — playful, the claws are in.
                    self.facing_left = f32::from(cc) < self.col + width * 0.5;
                    self.play_frolic = true;
                    self.swipe = true;
                    self.play_hold = BAT_HOLD * if deal == 1 { 2.0 } else { 1.0 };
                    self.set_action(PetAction::Frolic);
                    self.spawn_bat_dust(f32::from(cc), self.row);
                }
                return self.emit(sense, width);
            }
            // THE STAKEOUT (wave 3): a creeping toy nearby pins the hunting
            // crouch — ambient like the watch, below it (a live stream is
            // closer to work), above the settle ladder. Gated on the SLOW
            // band directly, not on heat: heat only rises above the gaze
            // threshold (~6.7 cells/s), and a creep never gets there — that
            // is exactly what makes it a creep.
            if !self.stakeout
                && self.pursuit_t.is_none()
                && self.pursuit_cool <= 0.0
                && self.quiet >= PURSUIT_QUIET
                && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
                && let Some((px, py)) = self.last_pointer
            {
                let dist = (px - (self.col + width * 0.5))
                    .abs()
                    .max((py - self.row).abs() * 2.0);
                if dist <= STALK_RANGE
                    && dist > PURSUIT_CATCH
                    && self.pointer_speed >= STALK_MIN_SPEED
                    && self.pointer_speed < PURSUIT_MIN_SPEED
                    && self.content >= PLAY_CONTENT
                {
                    self.stakeout = true;
                    self.stakeout_t = 0.0;
                    self.hop_crouch = false;
                    self.big_gather = false;
                    self.facing_left = px < self.col + width * 0.5;
                    self.set_action(PetAction::Crouch);
                    return self.emit(sense, width);
                }
            }
            self.content = (self.content - CONTENT_DECAY * dt).max(0.0);
            self.enter_settled(dt);
            // The settle-turn (review #6) and the settled emissions (sleep
            // z's, the purr tell) — only on the visible, full-motion path:
            // reduced motion returned above, and a hidden caret never
            // reaches the chase. While pointer attention is live the gaze
            // tracks the POINTER through the same range laws; heat at zero
            // parks it back on the caret (wave 2, gaze-follow tier).
            let gaze_col = match sense.pointer {
                Some((px, _)) if self.pointer_heat > 0.0 => px,
                _ => f32::from(cc),
            };
            self.settle_gaze(gaze_col, width, dt);
            // Gauntlet F9: the face-on sit (the hunt-and-peck stare) parks
            // its muzzle CLEAR of the caret cell — one small deterministic
            // scoot at settle-turn time, only when the caret sits to the
            // pet's left (wall-side eye contact looks the other way), and
            // never on a first sighting (a re-materialisation lands exactly
            // at the station, the regression that pins it).
            // Only when the gaze is the CARET's: while pointer attention is
            // live, `sit_front`/`facing_left` describe the TOY, and scooting
            // by caret math against a pointer gaze teleported the pet across
            // the caret in a loop (review, 2026-08-10).
            if prev.is_some()
                && self.action == PetAction::Sit
                && self.sit_front
                && self.facing_left
                && self.pointer_heat <= 0.0
            {
                let clear = (f32::from(cc) + STATION_LEAD + PECK_CLEAR)
                    .min((f32::from(sense.cols) - width).max(0.0));
                if self.col < clear {
                    // Rationed like the step aside, for the same reason: the
                    // scoot is usually PECK_CLEAR of a cell and looks like
                    // nothing either way, but from a stand ARRIVED short of
                    // the station it is over a cell, and no cell of this cat
                    // moves without being walked (owner ruling, 2026-08-10).
                    self.col = Self::evict_toward(self.col, clear, dt);
                }
            }
            self.tend_motes(width);
        } else {
            self.content = (self.content - CONTENT_MOVING_DECAY * dt).max(0.0);
            self.leg_dist += travelled;
            let fast = match self.action {
                PetAction::Run => self.speed.abs() > RUN_SPEED - GAIT_HYST,
                _ => self.speed.abs() > RUN_SPEED + GAIT_HYST,
            };
            // THE SCRAMBLE (wave 4): past STUMBLE_SPEED the gallop loses a
            // paw every few strides — a one-beat squash and a puff, keyed on
            // the stride odometer crossing a whole number (deterministic,
            // serial-scattered), never two beats in a row.
            if fast
                && self.speed.abs() > STUMBLE_SPEED
                && self.stumble_t <= 0.0
                && self.stride.fract() < travelled / RUN_STRIDE_CELLS
                && (self.stride as u32).wrapping_add(u32::from(self.mote_serial)) % 5 == 0
            {
                self.stumble_t = STUMBLE_DUR;
                self.spawn_dust(width);
            }
            self.set_action_keep(if fast {
                PetAction::Run
            } else {
                PetAction::Walk
            });
        }

        self.emit(sense, width)
    }

    /// Resolve an explicit windowless still capture. Such a capture has no
    /// animation cadence after this call, so the ordinary first fade-in tick
    /// (`dt == 0`, alpha 0) would produce an image with no resident pet and
    /// leave `is_active()` false until somebody requested a second image.
    /// Materialize that first sighting at full opacity after the normal tick
    /// has seeded its lawful station and action; later captures use the normal
    /// live state unchanged. Glass/video callers must continue using [`tick`].
    pub fn tick_static_capture(&mut self, sense: PetSense) -> PetFrame {
        let frame = self.tick(sense);
        if sense.caret.is_some() && frame.alpha == 0 && self.needs_frames() {
            self.alpha = 1.0;
            return self.emit(sense, art_cols(sense.cell_w, sense.cell_h));
        }
        frame
    }

    /// React to an observed caret move of `(dr, dc)` cells, landing on
    /// `caret_col` (needed so a fright can face the caret itself, not a
    /// compass point). `cols` sizes the screen-crossing threshold.
    fn on_move(&mut self, dr: f32, dc: f32, caret_col: f32, cols: u16, reduced: bool) {
        // Reversal bookkeeping runs even under reduced motion so the counter
        // does not go stale, but nothing acts on it there.
        let dir = if dc > 0.0 {
            1
        } else if dc < 0.0 {
            -1
        } else {
            0
        };
        if dir != 0 {
            if self.last_dir != 0 && dir != self.last_dir {
                self.reversals = self.reversals.saturating_add(1);
                self.reversal_t = 0.0;
            }
            self.last_dir = dir;
            // The rhythm run (the lead's gate): same-direction moves, each
            // within RHYTHM_WINDOW of the last, keep the run alive; anything
            // else starts a fresh run of one. Like the reversal counter this
            // runs even under reduced motion so it cannot go stale.
            if dir == self.run_dir && self.run_t <= RHYTHM_WINDOW {
                self.run_count = self.run_count.saturating_add(1);
            } else {
                self.run_dir = dir;
                self.run_count = 1;
            }
            self.run_t = 0.0;
        }
        if reduced {
            return;
        }
        // v̂'s impulse half (the decay lives in `tick`): this move's columns,
        // spread over the EMA window.
        self.vhat = (self.vhat + dc / VEL_TAU).clamp(-VEL_MAX, VEL_MAX);

        // THE TENNIS WATCH (wave 4), ABOVE the whole reaction ladder: while
        // the watch holds, a rally hit is a rally hit — not a fright, not a
        // frolic, not a travel intent. Facing follows the ball; the rally
        // clock re-stamps; only a REAL jump breaks the spell and falls
        // through to the ordinary sensors.
        if self.tennis {
            if dc.abs() >= POUNCE_JUMP {
                self.tennis = false;
            } else {
                self.tennis_last = self.clock;
                if dc.abs() > 0.0 {
                    self.facing_left = dc < 0.0;
                }
                return;
            }
        }

        let woke = self.action == PetAction::Sleep;
        if woke {
            self.set_action(PetAction::Waking);
            return;
        }

        // A retreat frightens — in two tiers. A REAL retreat (STARTLE_COLS in
        // one move, or a held-delete run) earns the full bottle; the everyday
        // backspace or two earns a cheap flinch instead, because a fright
        // fired at every typo taught the eye to ignore the pose. Either way
        // the pet faces the CARET, not a compass point: at the right-margin
        // wall it parks LEFT of the caret, and the old unconditional
        // `facing_left = true` turned it away from the very deletion that
        // scared it.
        if dr == 0.0 && dc < 0.0 {
            self.retreat_run = if self.retreat_gap <= STARTLE_RUN_WINDOW {
                self.retreat_run.saturating_add(1)
            } else {
                1
            };
            self.retreat_gap = 0.0;
            self.facing_left = caret_col < self.col;
            if dc <= -STARTLE_COLS || self.retreat_run >= STARTLE_RUN {
                // The bottle — and an already-startled pet EXTENDS the hold
                // rather than re-firing, so a held delete is one long fright.
                if self.action == PetAction::Startle {
                    self.action_t = 0.0;
                } else {
                    self.set_action(PetAction::Startle);
                }
            } else {
                // The flinch: existing plumbing only. The perk hold plants
                // the feet and raises the head toward the deletion; the
                // squash envelope ([`FLINCH_DUR`]) ducks the body for a few
                // frames. No bottle, no pinned-ear silhouette erasure.
                self.flinch_t = FLINCH_DUR;
                if self.action == PetAction::Perk {
                    self.action_t = 0.0;
                } else {
                    self.set_action(PetAction::Perk);
                }
            }
            return;
        }

        if self.reversals >= FROLIC_REVERSALS && self.action != PetAction::Frolic {
            self.reversals = 0;
            // Fool me twice (wave 4): a SECOND frolic earned within
            // TENNIS_AFTER of the last one means the rally is not stopping —
            // the cat gives up participating, sits down and WATCHES, facing
            // ping-ponging with every hit until the rally lapses.
            if (self.clock - self.last_frolic) as f32 <= TENNIS_AFTER {
                self.tennis = true;
                self.tennis_last = self.clock;
                self.sit_front = true;
                self.set_action(PetAction::Sit);
            } else {
                self.last_frolic = self.clock;
                self.set_action(PetAction::Frolic);
            }
            return;
        }

        // A jump. Latch the intent so it survives whatever hold the pet is in,
        // and give a settled cat the double-take first.
        //
        // `self.action != Perk` is load-bearing, not tidiness. `set_action`
        // resets the hold clock unconditionally, and Perk is itself a `settled()`
        // action — so without this guard every further jump would restart the
        // 0.30 s double-take. A repeat faster than that (a held word-jump, a held
        // Tab, any program streaming a few hundred chars/s onto one line) would
        // then re-arm the hold forever: `action_t` never reaches `PERK_HOLD`, the
        // one-shot arm in `tick` returns early with `speed = 0`, and the chase,
        // the row hop and the latched pounce below it are never reached. The cat
        // freezes solid for the whole burst while the caret runs off down the
        // line — the exact opposite of noticing something.
        if dc.abs() >= POUNCE_JUMP {
            self.pending_pounce = true;
            // The caret is work, the pointer is play: a travel latch CLEARS
            // every pending play intent (wave 2) — the un-launched play
            // crouch re-aims at the station, and a play flight already in
            // the air lands into work instead of the flourish. The wave-3
            // games end the same way: chase dropped mid-stride (a short
            // cooldown so typing truly ends it), stakeout stood down, the
            // far look and the owed groom forgotten — work first.
            self.pending_pointer_pounce = None;
            self.play_to = None;
            self.play_land = None;
            self.pursuit_t = None;
            self.pursuit_cool = self.pursuit_cool.max(1.0);
            self.stakeout = false;
            self.pending_look = None;
            self.look_at = None;
            self.groom_owed = false;
            self.hide_to = None;
            self.hiding = false;
            // The SCREEN-CROSSING latch, two of its three doors: (a) one move
            // covering max(BIG_JUMP_COLS, BIG_JUMP_COLS_FRAC·cols) is a jump
            // across the screen whatever the resulting gap; (c) a settled cat
            // carrying JOY_CONTENT answers even a word jump with the show.
            if dc.abs() >= BIG_JUMP_COLS.max(BIG_JUMP_COLS_FRAC * f32::from(cols))
                || (self.action.settled() && self.content >= JOY_CONTENT)
            {
                self.pending_big_jump = true;
            }
            if self.action.settled() && self.action != PetAction::Perk {
                self.set_action(PetAction::Perk);
            }
        }
    }

    /// Settle into the sit → groom → sleep ladder by how long it has been quiet.
    fn enter_settled(&mut self, dt: f32) {
        self.speed = 0.0;
        // The stride idles toward a foot-down frame so a stopping cat plants its
        // feet instead of freezing mid-swing.
        let target = self.stride.round();
        self.stride += (target - self.stride) * (1.0 - (-dt / 0.09).exp());

        let next = if self.quiet >= SLEEP_AFTER {
            PetAction::Sleep
        } else if self.quiet >= SIT_AFTER {
            let since_sit = self.quiet - SIT_AFTER;
            // One wash per groom cycle, deterministically placed.
            let cycle = GROOM_AFTER + GROOM_DUR;
            if since_sit >= GROOM_AFTER && (since_sit % cycle) >= GROOM_AFTER {
                PetAction::Groom
            } else if self.content >= PURR_GATE {
                PetAction::Purr
            } else if self.quiet >= LOAF_AFTER {
                // The LONG dwell: the sit melts into the loaf, and sleep
                // still follows it on the same quiet ladder.
                PetAction::Loaf
            } else {
                PetAction::Sit
            }
        } else {
            // Just arrived: STAND. The pose shipped in the roster from day
            // one and was never emitted — every stop freeze-framed a mid-gait
            // walk pose until the sit. On its feet, square, all four down.
            PetAction::Stand
        };
        self.set_action_keep(next);
    }

    /// THE SCALED COIL's length (rung 7) for a pounce winding up `span`
    /// columns: `(CROUCH_DUR + span·COIL_PER_CELL).clamp(CROUCH_DUR,
    /// COIL_MAX)` — a longer jump gathers longer, never below the old fixed
    /// crouch and never past [`COIL_MAX`]'s ordering law (the big gather
    /// must stay the longest wind-up in the vocabulary).
    fn coil_dur(span: f32) -> f32 {
        (CROUCH_DUR + span * COIL_PER_CELL).clamp(CROUCH_DUR, COIL_MAX)
    }

    /// The gather's squash-and-release envelope (rung 7): `(scale_y,
    /// scale_x)` multipliers at `t` seconds into a coil `dur` long. Two
    /// phases, continuous at their joint and at the launch:
    ///
    /// * THE ATTACK (`t < dur − COIL_RELEASE`): a linear wind down to
    ///   `1 − q`, `q` capped at [`COIL_SQUASH`] and scaled by `dur /
    ///   COIL_MAX` (a quick gather is a shallow one). Volume-preserving —
    ///   `scale_x = 1 / scale_y` — so no attack frame can step the volume
    ///   at all.
    /// * THE RELEASE (the last [`COIL_RELEASE`] seconds): a per-axis ramp
    ///   from the full coil into the launch frame the Leap arm will emit at
    ///   `u = 0` (`1 + LAUNCH_STRETCH` along the motion, `1 −
    ///   0.6·LAUNCH_STRETCH` across it), so the crouch→launch cut carries
    ///   no pop — the coil and its release land as ONE change, exactly as
    ///   §3 orders, because a coil without the release CREATES the pop it
    ///   was meant to prevent.
    fn coil_envelope(t: f32, dur: f32) -> (f32, f32) {
        let q_full = COIL_SQUASH * (dur / COIL_MAX).min(1.0);
        let release0 = (dur - COIL_RELEASE).max(f32::EPSILON);
        if t < release0 {
            let q = q_full * (t / release0).min(1.0);
            (1.0 - q, 1.0 / (1.0 - q))
        } else {
            let r = ((t - release0) / COIL_RELEASE).clamp(0.0, 1.0);
            let (y0, x0) = (1.0 - q_full, 1.0 / (1.0 - q_full));
            let (y1, x1) = (1.0 - 0.6 * LAUNCH_STRETCH, 1.0 + LAUNCH_STRETCH);
            (y0 + (y1 - y0) * r, x0 + (x1 - x0) * r)
        }
    }

    /// The arc's height profile at `u` of the flight (rung 10). A non-big
    /// flight keeps the parabola `4u(1−u)` BYTE-EXACTLY — `fp` is
    /// byte-compared, and an ordinary pounce flattened even slightly is not
    /// a hang but a slide across the reading zone — while a `big` bound
    /// HANGS: `h(u) = 1 − |2u−1|^p`, `p = 3.0` rising and [`HANG_P_DOWN`]
    /// falling, continuous with the parabola at both ends and the apex.
    /// Determinism (§5's caveat): the up-branch is hand-expanded as `s·s·s`
    /// — exact, portable — so the one non-integer `powf` here is the only
    /// one in the flight math.
    fn flight_lift(arc: f32, u: f32, big: bool) -> f32 {
        if big {
            if u < 0.5 {
                let s = 1.0 - 2.0 * u;
                arc * (1.0 - s * s * s)
            } else {
                arc * (1.0 - (2.0 * u - 1.0).powf(HANG_P_DOWN))
            }
        } else {
            arc * 4.0 * u * (1.0 - u)
        }
    }

    /// Duration and arc for a flight covering `span` columns. The vertical
    /// branch is clamped exactly like the horizontal one, and for a sharper
    /// reason: its `span` is the HORIZONTAL distance, and the commonest
    /// vertical move — a wrap or Enter from the end of a long line — has a
    /// span of nearly the whole grid width. Unclamped, that is seconds of
    /// flight on one flat arc, during which the flight block owns the
    /// position and every caret move you make is ignored. A row change must
    /// always read as one hop.
    fn flight_shape(span: f32, vertical: bool) -> (f32, f32) {
        if vertical {
            (
                (HOP_DUR + span * FLIGHT_PER_CELL * 0.5).clamp(HOP_DUR, FLIGHT_MAX),
                // The arc earns its height from the span, up to one cell.
                (HOP_ARC + span * HOP_ARC_PER_CELL).min(HOP_ARC_MAX),
            )
        } else {
            (
                (FLIGHT_MIN + span * FLIGHT_PER_CELL).clamp(FLIGHT_MIN, FLIGHT_MAX),
                (ARC_BASE + span * ARC_PER_CELL).min(ARC_MAX),
            )
        }
    }

    /// Begin a pounce/hop toward `(to_col, to_row)`.
    ///
    /// FLIGHT AIM: the paws should come down where the station will BE, not
    /// where it was when the crouch began — so under an open rhythm the
    /// target is advanced by `v̂ · (flight + landing)`, wall-clamped like
    /// every station. Without the rhythm (a lone Enter, a one-off jump) the
    /// aim is the plain target, exactly as before.
    fn begin_flight(&mut self, to_col: f32, to_row: f32, vertical: bool, cols: u16, width: f32) {
        let (dur0, _) = Self::flight_shape((to_col - self.col).abs(), vertical);
        let to_col = if self.rhythm_open() {
            (to_col + self.vhat * (dur0 + LAND_DUR)).clamp(0.0, (f32::from(cols) - width).max(0.0))
        } else {
            to_col
        };
        // THE SPAN CAP (the launch law): both branches of `flight_shape`
        // saturate their duration at [`FLIGHT_MAX`], so the longest span an
        // ordinary flight can cover lawfully is [`POUNCE_SPAN_MAX`] — the
        // same bar that bands the pounce door, because it is the same
        // arithmetic. A wider ask — a caret that kept moving through the
        // gather, a keep-ahead prediction stretching the aim, a mid-line
        // Enter from far out — lands SHORT, on the aimed row, and the
        // ladder closes the remainder on its feet. The clamp is on the
        // span, never the row: a row change is still paid in full by this
        // one arc.
        let to_col = self.col + (to_col - self.col).clamp(-POUNCE_SPAN_MAX, POUNCE_SPAN_MAX);
        let (dur, arc) = Self::flight_shape((to_col - self.col).abs(), vertical);
        self.begin_arc(to_col, to_row, dur, arc, false);
    }

    /// The flight primitive: face the travel, own the position, leap.
    ///
    /// THE LAUNCH LAW — `no_flight_may_outrun_the_animal_that_flies_it` —
    /// is asserted here, at launch time, against the arc the caller CHOSE:
    /// `|to_col − from_col| / dur` may not exceed [`HOP_SPEED_MAX`], or
    /// [`BIG_HOP_SPEED_MAX`] for a `big` bound — the bound gets its own bar,
    /// never an exemption, because the wrap's loudest form IS big. The
    /// per-frame law cannot see this defect: it judges each frame against
    /// the live arc's own rate, so an unlawful arc is lawful by its own
    /// account. The rate is measured over the `dur.max(0.05)` the flight
    /// will actually fly, with 1e-3 of slack for the span caps derived as
    /// exactly `bound × dur`. Reduced motion is exempt by the ladder's
    /// shape, not by a branch here: its arm returns before any arc can be
    /// launched, so no flight ever exists under it.
    fn begin_arc(&mut self, to_col: f32, to_row: f32, dur: f32, arc: f32, big: bool) {
        let bound = if big {
            BIG_HOP_SPEED_MAX
        } else {
            HOP_SPEED_MAX
        };
        let rate = (to_col - self.col).abs() / dur.max(0.05);
        debug_assert!(
            rate <= bound + 1e-3,
            "no flight may outrun the animal that flies it: {rate:.1} c/s over \
             the {bound:.0} c/s launch bound ({from:.2} -> {to_col:.2} in \
             {dur:.3}s, big={big})",
            from = self.col
        );
        if to_col > self.col {
            self.facing_left = false;
        } else if to_col < self.col {
            self.facing_left = true;
        }
        self.flight = Some(Flight {
            from_col: self.col,
            from_row: self.row,
            to_col,
            to_row,
            dur: dur.max(0.05),
            arc,
            t: 0.0,
            reaimed: false,
            big,
        });
        self.set_action(PetAction::Leap);
    }

    fn launch(&mut self, to_col: f32, to_row: f32, cols: u16, width: f32) {
        self.begin_flight(to_col, to_row, false, cols, width);
    }

    /// Duration and arc for one screen-crossing bound of `span` columns —
    /// the review's curves, before the viewport clamp in [`Self::begin_big_arc`].
    fn big_shape(span: f32) -> (f32, f32) {
        (
            (BIG_FLIGHT_BASE + span * BIG_FLIGHT_PER_CELL).clamp(BIG_FLIGHT_BASE, BIG_FLIGHT_MAX),
            (BIG_ARC_BASE + span * BIG_ARC_PER_CELL).clamp(BIG_ARC_BASE, BIG_ARC_MAX),
        )
    }

    /// One screen-crossing bound toward `(to_col, to_row)`, apex clamped so
    /// the sprite's TOP never leaves the viewport: the body stands
    /// [`ART_ROWS`] above its baseline, so the arc may use at most
    /// `mid_row + 1 − ART_ROWS` cell heights (the 509c9f01 faith — nothing
    /// renders above the line).
    fn begin_big_arc(&mut self, to_col: f32, to_row: f32) {
        let (dur, arc) = Self::big_shape((to_col - self.col).abs());
        let mid_row = (self.row + to_row) * 0.5;
        let arc = arc.min((mid_row + 1.0 - ART_ROWS).max(0.0));
        self.begin_arc(to_col, to_row, dur, arc, true);
    }

    /// The screen-crossing launch: aim predicted and wall-clamped like every
    /// flight, shortened by the skid (the slide finishes the journey), and —
    /// the variance flourish — split 60/40 into TWO bounds when the span
    /// crosses [`TWO_BOUND_FRAC`] of the grid. Deterministic from the span.
    fn launch_big(&mut self, to_col: f32, to_row: f32, cols: u16, width: f32) {
        let (dur0, _) = Self::big_shape((to_col - self.col).abs());
        let mut aim = if self.rhythm_open() {
            to_col + self.vhat * (dur0 + BIG_LAND_DUR)
        } else {
            to_col
        };
        aim = aim.clamp(0.0, (f32::from(cols) - width).max(0.0));
        let delta = aim - self.col;
        let dir = if delta > 0.0 {
            1.0
        } else if delta < 0.0 {
            -1.0
        } else {
            0.0
        };
        // Land short by the skid, but never short enough to flip direction.
        let aim_short = if delta.abs() > SKID_CELLS {
            aim - dir * SKID_CELLS
        } else {
            aim
        };
        if delta.abs() >= TWO_BOUND_FRAC * f32::from(cols) {
            // The touch point between bounds must come down on BLANK cells
            // (gauntlet F1: the relaunch paws clipped "touchland"'s
            // ascenders): nudge the 60% point off the row's ink, and when
            // the midfield is inked wall to wall — a typed line spanning
            // the whole crossing — spend the span as ONE bound instead.
            // There is no ground there worth touching.
            let mid = self.col + delta * BOUND_SPLIT;
            let touch = self.ink_safe_col(mid, to_row, width, cols);
            // Both bounds must stay REAL bounds: a nudged touch point that
            // leaves less than a few cells to either end degenerates the
            // split into a long arc plus a bunny hop — one bound reads
            // better than that.
            let (lo, hi) = if delta > 0.0 {
                (self.col + 4.0, aim_short - 4.0)
            } else {
                (aim_short + 4.0, self.col - 4.0)
            };
            if !self.ink_overlaps(touch, to_row, width) && touch >= lo && touch <= hi {
                self.bound2 = Some((aim_short, to_row));
                self.begin_big_arc(touch, to_row);
            } else {
                self.bound2 = None;
                self.begin_big_arc(aim_short, to_row);
            }
        } else {
            self.bound2 = None;
            self.begin_big_arc(aim_short, to_row);
        }
    }

    /// Dust at the heels of a heavy landing — the delete poof's puff, at pet
    /// scale, on the pet's own mote lane (the glow engine's poof is caret
    /// plumbing and lives in another file). Three puffs kicked BACK along
    /// the skid, deterministically scattered by the mote serial.
    fn spawn_dust(&mut self, width: f32) {
        let heel = self.col + width * 0.5;
        for k in 0..3u8 {
            let seed = self.mote_serial.wrapping_add(k);
            self.spawn_mote(Mote {
                kind: PetMoteKind::Dust,
                born: self.clock,
                life: 0.45 + 0.07 * f32::from(k),
                // Fractional by construction: no dust grain is ever
                // cell-aligned, whatever column the paws stopped on.
                col: heel + 0.23 + 0.17 * f32::from(k % 3),
                row: self.row + 0.31,
                dir: -self.skid_dir,
                seed,
            });
        }
        self.mote_serial = self.mote_serial.wrapping_add(3);
    }

    /// Spawn into a free slot; a full lane drops the mote (bounded by law).
    fn spawn_mote(&mut self, m: Mote) {
        if let Some(slot) = self.motes.iter_mut().find(|s| s.is_none()) {
            *slot = Some(m);
        }
    }

    /// The settle-turn (review #6): a beat after arriving, the pet
    /// acknowledges what it is watching — the caret, or (while pointer
    /// attention is live, wave 2) the wandering pointer, through the SAME
    /// range laws. Close by, it turns for eye contact and the sit goes
    /// face-on; far away and BEHIND its facing, it keeps its body and peeks
    /// over the shoulder instead — a body flip at rest reads as a twitch,
    /// an over-the-shoulder glance reads as a cat. Re-read every settled
    /// tick, so the flags can never go stale against a target that moved
    /// (a caret move resets `quiet` and closes the gaze until the next
    /// settle anyway).
    ///
    /// THE BODY'S TURN IS RATIONED; the glance is not. A settled pet's facing
    /// is the only thing here a jittering target could strobe, so the turn is
    /// gated by a dead band, a dwell, and — deeply asleep — nothing at all;
    /// see the [`GAZE_FLIP_BAND`] block for the ruling and the argument. The
    /// over-the-shoulder `peek` stays instantaneous on purpose: a glance
    /// costs the animal nothing, which is exactly why it is the answer to a
    /// target the body should not chase.
    fn settle_gaze(&mut self, target_col: f32, width: f32, dt: f32) {
        if !self.action.settled() || self.quiet < SETTLE_TURN {
            self.sit_front = false;
            self.peek = false;
            self.gaze_dwell = 0.0;
            return;
        }
        let asleep = self.action == PetAction::Sleep;
        let gap = self.col - target_col;
        // Which side of the pet's own midline the target is on — or NEITHER,
        // inside the dead band, where a sub-cell wobble is not a direction.
        let off = target_col + 0.5 - (self.col + width * 0.5);
        let band = if asleep {
            SLEEP_FLIP_BAND
        } else {
            GAZE_FLIP_BAND
        };
        let side = if off < -band {
            Some(true)
        } else if off > band {
            Some(false)
        } else {
            None
        };
        self.sit_front = gap.abs() < SIT_FRONT_GAP;
        let behind = side.is_some_and(|left| left != self.facing_left);
        if self.sit_front {
            // Eye contact: face the thing you are meeting — once it has held
            // still long enough to be worth turning over for.
            //
            // The dwell clock measures STAYING, not being, so a target that
            // keeps changing its mind rewinds it every time it crosses back.
            // It runs only in HERE, where a turn is actually on the table: a
            // clock ticking while the pet was only ever going to glance would
            // hand its whole balance to whatever wandered into range next,
            // which is the instant flip wearing a longer name.
            if behind {
                self.gaze_dwell += dt;
            } else {
                self.gaze_dwell = 0.0;
            }
            let dwell = if asleep {
                SLEEP_FLIP_DWELL
            } else {
                SETTLE_FLIP_DWELL
            };
            // Deep sleep never turns: the pose's whole contract past the
            // breath window is that the frame has settled for good.
            let deep = asleep && self.quiet >= SLEEP_AFTER + BREATH_WINDOW;
            if behind && !deep && self.gaze_dwell >= dwell {
                self.facing_left = !self.facing_left;
                self.gaze_dwell = 0.0;
            }
            self.peek = false;
        } else {
            // Behind the facing ⇒ the over-the-shoulder peek; ahead of it
            // the pet is already looking the right way and simply sits.
            self.gaze_dwell = 0.0;
            self.peek = behind;
        }
    }

    /// Where the sleeping head is, in fractional cells — the z's anchor.
    /// The art faces right, so the head sits on the facing side; the fixed
    /// fractional offsets keep every birth off the cell lattice.
    fn head_anchor(&self, width: f32) -> (f32, f32) {
        let x = if self.facing_left { 0.28 } else { 0.72 };
        (self.col + width * x + 0.19, self.row - 0.61)
    }

    /// The settled emissions: light-sleep z's and the purr tell. Cadence is
    /// a beat index derived from `quiet` — a pure function of THIS settle —
    /// and spawning stops early enough that the last mote dies before the
    /// breath window closes and the frame lane releases (idle-to-zero).
    fn tend_motes(&mut self, width: f32) {
        match self.action {
            PetAction::Sleep => {
                // LIGHT sleep only: no new z once its life would outlive the
                // breath window, so DEEP sleep begins with an empty lane.
                if self.quiet > SLEEP_AFTER + BREATH_WINDOW - ZEE_LIFE {
                    return;
                }
                let beat = ((self.quiet - SLEEP_AFTER) / ZEE_EVERY).floor() as i64;
                let alive = self
                    .motes
                    .iter()
                    .flatten()
                    .filter(|m| m.kind == PetMoteKind::Zee)
                    .count();
                if beat >= 1 && beat != self.mote_mark && alive < ZEE_ALIVE_MAX {
                    self.mote_mark = beat;
                    let (hx, hy) = self.head_anchor(width);
                    let seed = self.mote_serial;
                    self.mote_serial = self.mote_serial.wrapping_add(1);
                    self.spawn_mote(Mote {
                        kind: PetMoteKind::Zee,
                        born: self.clock,
                        life: ZEE_LIFE,
                        col: hx,
                        row: hy,
                        dir: if self.facing_left { -1.0 } else { 1.0 },
                        seed,
                    });
                }
            }
            PetAction::Purr => {
                let beat = ((self.quiet - SIT_AFTER).max(0.0) / PURR_MOTE_EVERY).floor() as i64;
                if beat >= 1 && beat != self.mote_mark {
                    self.mote_mark = beat;
                    let seed = self.mote_serial;
                    self.mote_serial = self.mote_serial.wrapping_add(1);
                    // Alternate ♪ and ♥ deterministically by serial parity.
                    let kind = if seed.is_multiple_of(2) {
                        PetMoteKind::Note
                    } else {
                        PetMoteKind::Heart
                    };
                    let chest_x = if self.facing_left { 0.38 } else { 0.62 };
                    self.spawn_mote(Mote {
                        kind,
                        born: self.clock,
                        life: PURR_MOTE_LIFE,
                        col: self.col + width * chest_x + 0.23,
                        row: self.row - 0.27,
                        dir: if self.facing_left { -1.0 } else { 1.0 },
                        seed,
                    });
                }
            }
            _ => {}
        }
    }

    /// The one startled z popped by waking, ahead of the stretch.
    fn spawn_wake_pop(&mut self, width: f32) {
        let (hx, hy) = self.head_anchor(width);
        let seed = self.mote_serial;
        self.mote_serial = self.mote_serial.wrapping_add(1);
        self.spawn_mote(Mote {
            kind: PetMoteKind::ZeePop,
            born: self.clock,
            life: ZEE_POP_LIFE,
            col: hx,
            row: hy,
            dir: if self.facing_left { -1.0 } else { 1.0 },
            seed,
        });
    }

    /// Consume ONE latched wave-1 stimulus, on the ground. Called from the
    /// arrived branch only, so every caret-travel intent and every one-shot
    /// hold has already had its turn — the latches survive holds by
    /// construction (the `pending_pounce` law). Returns whether it acted.
    ///
    /// A sleeping pet always plays the EXISTING wake path first (the
    /// startled z + the stretch); every latch except the bell is kept
    /// across the wake, so the droop/cheer/purr follows the stretch —
    /// the bell IS the wake in its case, and is spent by it.
    fn consume_stimuli(&mut self, caret_col: u16, width: f32) -> bool {
        // Fright first: the bell interrupts what the others would start.
        if self.pending_bell.take().is_some() {
            self.quiet = 0.0;
            if self.action == PetAction::Sleep {
                self.set_action(PetAction::Waking);
                self.spawn_wake_pop(width);
            } else {
                // The FLINCH tier, deliberately — a bell is a noise, not a
                // held delete — facing the caret like every fright.
                self.facing_left = f32::from(caret_col) < self.col;
                self.flinch_t = FLINCH_DUR;
                if self.action == PetAction::Perk {
                    self.action_t = 0.0;
                } else {
                    self.set_action(PetAction::Perk);
                }
            }
            return true;
        }
        // Grief before joy: a failure and a success latched together (two
        // commands finishing across one hold) sulk first, cheer after.
        if self.pending_sulk {
            if self.action == PetAction::Sleep {
                self.quiet = 0.0;
                self.set_action(PetAction::Waking);
                self.spawn_wake_pop(width);
                return true; // latch kept: the droop follows the stretch
            }
            self.pending_sulk = false;
            self.quiet = 0.0;
            self.set_action(PetAction::Droop);
            return true;
        }
        if let Some(big) = self.pending_cheer {
            if self.action == PetAction::Sleep {
                self.quiet = 0.0;
                self.set_action(PetAction::Waking);
                self.spawn_wake_pop(width);
                return true; // latch kept: the cheer follows the stretch
            }
            if self.action != PetAction::Perk {
                // The notice leads the celebration, exactly like the big
                // jump's choreography: perk first, frolic when it resolves.
                self.set_action(PetAction::Perk);
                return true; // latch kept
            }
            self.pending_cheer = None;
            self.quiet = 0.0;
            self.set_action(PetAction::Frolic);
            self.spawn_cheer_motes(big, width);
            return true;
        }
        if self.pending_pet > 0 {
            if self.action == PetAction::Sleep {
                self.quiet = 0.0;
                self.set_action(PetAction::Waking);
                self.spawn_wake_pop(width);
                return true; // latch kept: the purr follows the stretch
            }
            let n = self.pending_pet;
            self.pending_pet = 0;
            self.pet_at = None;
            // Affection is bookkeeping too: each consumed pet warms the
            // ledger, and enough of them earn the REAL purr (and, kept up,
            // the JOY threshold) — the escalation is the point.
            self.content = (self.content + f32::from(n) * PET_CONTENT).min(1.0);
            self.pet_hold_t = PET_HOLD;
            // NOT a quiet reset: a hand on the cat is not a caret event,
            // so a long-settled cat returns from the hold to its ladder.
            self.set_action(PetAction::Purr);
            self.spawn_pet_hearts(n, width);
            return true;
        }
        false
    }

    /// Drop every wave-2 play envelope — the no-audience and reduced-motion
    /// arms' shared clear (dropped, not parked, the wave-1 stimulus rule).
    fn clear_play(&mut self) {
        self.last_pointer = None;
        self.pointer_vx = 0.0;
        self.pointer_vy = 0.0;
        self.pointer_speed = 0.0;
        self.pointer_heat = 0.0;
        self.dash_t = 0.0;
        self.pointer_armed = true;
        self.pending_pointer_pounce = None;
        self.play_to = None;
        self.play_land = None;
        self.play_frolic = false;
        self.play_hold = 0.0;
        self.swipe = false;
        self.pending_bat = None;
        self.bat_at = None;
        // The wave-3 games drop with the rest of play (no cooldown owed:
        // a hidden or reduced pet was not beaten by the toy).
        self.pursuit_t = None;
        self.stakeout = false;
        self.pending_look = None;
        self.look_at = None;
        self.groom_owed = false;
    }

    /// THE WORD-CAT BAT (wave 2): consume a noted peek, on the ground —
    /// crouch, fly to the standoff beside the head, and owe the swipe on
    /// landing. Returns whether it acted.
    fn consume_bat(&mut self, sense: &PetSense, width: f32) -> bool {
        let Some((bx, by)) = self.pending_bat else {
            return false;
        };
        if matches!(self.action, PetAction::Sleep | PetAction::Waking) {
            // A sleeper ignores the peek — the TTL retires it; waking a cat
            // for scenery would teach the eye to ignore the wake.
            return false;
        }
        self.pending_bat = None;
        self.bat_at = None;
        // Approach from the side the pet already stands on: the near paw
        // ends BAT_STANDOFF short of the head, never through it.
        let to_col = if self.col + width * 0.5 <= bx {
            bx - BAT_STANDOFF - width
        } else {
            bx + BAT_STANDOFF
        }
        .clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
        let to_row = by.clamp(0.0, f32::from(sense.rows.saturating_sub(1)));
        self.play_to = Some((to_col, to_row));
        self.play_land = Some(PlayLand::BatSwipe { col: bx, row: by });
        self.hop_crouch = false;
        self.big_gather = false;
        self.set_action(PetAction::Crouch);
        true
    }

    /// The swipe's contact puff: ONE Dust mote at the peek cell — neutral,
    /// fractional, rotated, fading (the mote lane's laws; the fake-glyph
    /// rule bars anything that could read as terminal output, so never a
    /// "!" or any glyph-shaped mark).
    fn spawn_bat_dust(&mut self, col: f32, row: f32) {
        let seed = self.mote_serial;
        self.mote_serial = self.mote_serial.wrapping_add(1);
        self.spawn_mote(Mote {
            kind: PetMoteKind::Dust,
            born: self.clock,
            life: 0.5,
            // Fractional offsets by law: no grain sits on the cell lattice.
            col: col + 0.23,
            row: row + 0.12,
            // Kicked the way the swipe travels — the pet faces the head.
            dir: if self.facing_left { -1.0 } else { 1.0 },
            seed,
        });
    }

    /// POINTER PLAY (wave 2): consume a latched dash pounce, on the ground.
    /// The flight aims at the predicted toy (already lead-corrected at latch
    /// time), lands, frolics one beat, and the ordinary chase ladder walks
    /// the pet home. Returns whether it acted.
    fn consume_pointer_pounce(&mut self, sense: &PetSense, width: f32) -> bool {
        let Some((ac, ar)) = self.pending_pointer_pounce.take() else {
            return false;
        };
        // A sleeper cannot play (unreachable in practice: PLAY_CONTENT has
        // decayed to zero long before SLEEP_AFTER — the guard is a law, not
        // a path). The latch is dropped, never parked against a sleeper.
        if matches!(self.action, PetAction::Sleep | PetAction::Waking) {
            return false;
        }
        // Land CENTERED on the toy: the aim is a pointer cell, the pet's
        // `col` is its left edge.
        let to_col = (ac - width * 0.5).clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
        let to_row = ar.clamp(0.0, f32::from(sense.rows.saturating_sub(1)));
        self.play_to = Some((to_col, to_row));
        self.play_land = Some(PlayLand::PointerFrolic);
        self.hop_crouch = false;
        self.big_gather = false;
        self.set_action(PetAction::Crouch);
        true
    }

    /// PERK-AND-WATCH (wave 2): hold the perk while a quiet pane streams.
    /// Called from the arrived branch only, below every latched stimulus —
    /// the watch is ambient attention, and it never outranks an event.
    /// Returns whether the watch owns this tick.
    ///
    /// A sleeper sleeps through a stream, deliberately: `quiet` is
    /// caret-quiet, so a pane streaming under a parked caret can be hours
    /// past [`SLEEP_AFTER`], and a pet that woke for every burst of an
    /// all-night build would never idle to zero. (Waking finishes its
    /// stretch through the one-shot ladder before this is ever reached.)
    fn consume_watch(&mut self, dt: f32, caret_col: u16) -> bool {
        if self.watch_spent || self.watch_heat < WATCH_GATE {
            return false;
        }
        if matches!(self.action, PetAction::Sleep | PetAction::Waking) {
            return false;
        }
        self.watch_t += dt;
        if self.watch_t >= WATCH_MAX {
            // The hard cap: one bounded stare per stream. Falling through to
            // the settle ladder drops the pet to the sit with `settle_gaze`
            // parking the eyes toward the caret — which is where the output
            // is pouring — as a still, frame-free sticker.
            self.watch_spent = true;
            return false;
        }
        // The watch: perk toward the stream (the caret rides the output's
        // tail, so facing the caret IS facing the text). Tick-side re-arm,
        // never an `on_move` re-entry — travel intents latched during the
        // watch still consume the moment the heat releases the ground.
        self.facing_left = f32::from(caret_col) < self.col;
        self.sit_front = false;
        self.peek = false;
        self.gaze_dwell = 0.0;
        if self.action != PetAction::Perk {
            self.set_action(PetAction::Perk);
        }
        self.speed = 0.0;
        true
    }

    /// The cheer's motes off the chest anchor: the ♪/♥ alternation
    /// (mote-serial parity, the purr tell's law) — two for an ordinary long
    /// success, three for a [`CHEER_BIG_MS`] build. RAISED from 1/2 in the
    /// 0.19.0 gauntlet pass (F4b): two 3-4 px accent specks read as stray
    /// sleep-z's, and a party nobody can see is not a party. The emitter
    /// dresses these gold and pink and a size up — the warmth is its half.
    fn spawn_cheer_motes(&mut self, big: bool, width: f32) {
        let n: u8 = if big { 3 } else { 2 };
        let chest_x = if self.facing_left { 0.38 } else { 0.62 };
        for k in 0..n {
            let seed = self.mote_serial;
            self.mote_serial = self.mote_serial.wrapping_add(1);
            // Note and heart ALTERNATE at every tier — a lone teal speck
            // was the old small-cheer, and it read as a z.
            let kind = if seed.is_multiple_of(2) {
                PetMoteKind::Note
            } else {
                PetMoteKind::Heart
            };
            self.spawn_mote(Mote {
                kind,
                born: self.clock,
                life: PURR_MOTE_LIFE,
                col: self.col + width * chest_x + 0.23 + 0.31 * f32::from(k),
                row: self.row - 0.27 - 0.13 * f32::from(k),
                dir: if self.facing_left { -1.0 } else { 1.0 },
                seed,
            });
        }
    }

    /// One heart per consumed pet, off the chest anchor — the 4-slot mote
    /// cap drops extras silently, by the lane's own law.
    fn spawn_pet_hearts(&mut self, n: u8, width: f32) {
        let chest_x = if self.facing_left { 0.38 } else { 0.62 };
        for k in 0..n {
            let seed = self.mote_serial;
            self.mote_serial = self.mote_serial.wrapping_add(1);
            self.spawn_mote(Mote {
                kind: PetMoteKind::Heart,
                born: self.clock,
                life: PURR_MOTE_LIFE,
                col: self.col + width * chest_x + 0.23 + 0.17 * f32::from(k),
                row: self.row - 0.27 - 0.11 * f32::from(k),
                dir: if self.facing_left { -1.0 } else { 1.0 },
                seed,
            });
        }
    }

    /// Cull dead motes and resolve the live ones into frame sprites — pure
    /// functions of age and seed, never of a die roll.
    fn resolve_motes(&mut self) -> [Option<PetMoteSprite>; PET_MOTES_MAX] {
        let mut out = [None; PET_MOTES_MAX];
        let clock = self.clock;
        // Walk the OUTPUT slots, carrying the index for the parallel
        // `self.motes` lane. The two arrays are the same length and the same
        // slot means the same mote in both, but they cannot be zipped: the body
        // both frees a slot (`self.motes[i] = None`) and asks `self` a question
        // (`zee_ink_fade`), so a mutable borrow of `self.motes` across the loop
        // would not hold. Indexing only the lane that needs it keeps the
        // out-of-bounds surface to the one array whose bound is the loop.
        for (i, slot) in out.iter_mut().enumerate() {
            let Some(m) = self.motes[i] else { continue };
            let u = ((clock - m.born) as f32) / m.life.max(0.01);
            if !(0.0..1.0).contains(&u) {
                self.motes[i] = None;
                continue;
            }
            let fade_in = (u / 0.15).min(1.0);
            let fade_out = ((1.0 - u) / 0.35).clamp(0.0, 1.0);
            let spin = if m.seed % 2 == 0 { 1.0 } else { -1.0 };
            let wobble = (TAU * (u * 0.8 + 0.31 * f32::from(m.seed % 4))).sin();
            *slot = Some(match m.kind {
                PetMoteKind::Dust => PetMoteSprite {
                    kind: m.kind,
                    // Kicked back along the skid, hugging the floor: a low
                    // puff that spreads and thins.
                    col: m.col + m.dir * (0.25 + 0.85 * u) * (0.6 + 0.2 * f32::from(m.seed % 3)),
                    row: m.row - (0.08 + 0.22 * u),
                    scale: 0.5 + 1.3 * u,
                    // Nonzero at every age, by construction.
                    rot: spin * (0.12 + 0.05 * f32::from(m.seed % 3) + 0.6 * u),
                    alpha: (fade_in * fade_out * 235.0) as u8,
                },
                PetMoteKind::Zee | PetMoteKind::ZeePop => {
                    let (col, row, scale, rot, peak) = if m.kind == PetMoteKind::Zee {
                        (
                            // Up and away from the head, with a lazy sway and
                            // a slow tilt — a sleep thought, not a projectile.
                            m.col + m.dir * (0.30 + 0.55 * u) + 0.10 * wobble,
                            m.row - 1.05 * u,
                            0.55 + 0.60 * u,
                            spin * (0.10 + 0.04 * f32::from(m.seed % 3) + 0.55 * u),
                            220.0,
                        )
                    } else {
                        (
                            // The startled pop: bigger, quicker, one sharp rise.
                            m.col + m.dir * 0.25 * u,
                            m.row - 0.55 * u,
                            0.85 + 0.55 * u,
                            spin * (0.14 + 0.8 * u),
                            245.0,
                        )
                    };
                    // Gauntlet F8: a z drifting toward an inked row band
                    // fades to NOTHING before it crosses — a pale glyph-ish
                    // mark inside real text is the fake-glyph hazard in its
                    // purest form. Dead-on-arrival z's free their slot, so
                    // the lane (and the frame cadence riding it) releases.
                    let ink = self.zee_ink_fade(col, row);
                    if ink <= 0.0 {
                        self.motes[i] = None;
                        continue;
                    }
                    PetMoteSprite {
                        kind: m.kind,
                        col,
                        row,
                        scale,
                        rot,
                        alpha: (fade_in * fade_out * ink * peak) as u8,
                    }
                }
                PetMoteKind::Note | PetMoteKind::Heart => PetMoteSprite {
                    kind: m.kind,
                    // A contented drift off the working chest. A size up and
                    // a shade brighter since the gauntlet (F4b/F7): the purr
                    // tell was a near-invisible speck at 1x, and the cheer
                    // has to read as a party from across the room.
                    col: m.col + m.dir * (0.18 + 0.30 * u) + 0.12 * wobble,
                    row: m.row - 0.85 * u,
                    scale: 0.78 + 0.50 * u,
                    rot: spin * (0.11 + 0.05 * f32::from(m.seed % 3) + 0.4 * u),
                    alpha: (fade_in * fade_out * 240.0) as u8,
                },
            });
        }
        out
    }

    /// BREED HANDOFF: admit one departing body, dropping the OLDEST when the
    /// lane is full — [`PET_DEPARTURES_MAX`] is the documented cap, and the
    /// newest ghost is the one whose story the user just caused.
    fn spawn_departure(&mut self, d: Departure) {
        let slot = match self.departures.iter().position(Option::is_none) {
            Some(free) => free,
            None => {
                // Full: evict the oldest birth (its story was nearly over).
                let mut oldest = 0;
                let mut oldest_born = f64::INFINITY;
                for (i, live) in self.departures.iter().enumerate() {
                    if let Some(a) = live
                        && a.born < oldest_born
                    {
                        oldest_born = a.born;
                        oldest = i;
                    }
                }
                oldest
            }
        };
        self.departures[slot] = Some(d);
    }

    /// Resolve the departing bodies for this frame — a pure function of each
    /// birth record and `self.lane_clock` (clockless, dieless; the LANE
    /// clock, so a stalled frame moves a ghost one dt clamp's worth, never a
    /// screen): the body runs from its spawn column toward its edge at its
    /// own [`Departure::speed`] on the RUN cycle (feet driven by distance
    /// covered, the gait law, phase-seeded from the live body's stride at
    /// spawn), holds at the edge if it gets there early, and fades over the
    /// visit's last [`EDGE_FADE`]. A `still` departure never runs at all: it
    /// keeps its carried facing and captured pose for the whole fade (the
    /// §2.0.6 mandatory fix — the run-derived `dir`/frame mirror-flipped a
    /// left-facing cat and froze it in a gallop). Expired slots free
    /// themselves here, exactly like the mote lane.
    fn resolve_departures(&mut self) -> [Option<PetDepartureSprite>; PET_DEPARTURES_MAX] {
        let mut out = [None; PET_DEPARTURES_MAX];
        for (i, slot) in out.iter_mut().enumerate() {
            let Some(d) = self.departures[i] else {
                continue;
            };
            let t = (self.lane_clock - d.born) as f32;
            let life = d.life();
            if !(0.0..life).contains(&t) {
                self.departures[i] = None;
                continue;
            }
            let span = (d.edge - d.from_col).abs();
            let dir = if d.edge < d.from_col { -1.0 } else { 1.0 };
            let run = (d.speed * t).min(span);
            let fade_start = life - EDGE_FADE;
            let alpha = if t <= fade_start {
                1.0
            } else {
                (1.0 - (t - fade_start) / EDGE_FADE).clamp(0.0, 1.0)
            };
            let (pose, facing_left) = if d.still {
                // The in-place fade: the ghost is the body the user was
                // just looking at, so it holds that body's exact look.
                (d.pose0, d.facing_left)
            } else {
                // Distance-driven feet (the module's gait law — wall-clock
                // feet moon-walk), on the run cycle's own stride, counting
                // on from the live body's phase at spawn.
                let stride = d.stride0 + run / RUN_STRIDE_CELLS;
                let frame = ((stride.rem_euclid(1.0) * Self::CYCLE_RUN.len() as f32) as usize)
                    .min(Self::CYCLE_RUN.len() - 1);
                (Self::CYCLE_RUN[frame], dir < 0.0)
            };
            *slot = Some(PetDepartureSprite {
                // The species skin applies here for the same reason the live
                // body applies it at emit: the record stays species-blind.
                pose: self.species.skin(pose),
                col: d.from_col + dir * run,
                row: d.row,
                facing_left,
                alpha: (alpha * 255.0) as u8,
                coat: d.coat,
                iris: d.iris,
                born: d.born,
            });
        }
        out
    }

    fn set_action(&mut self, a: PetAction) {
        if a != self.action {
            self.action = a;
            self.mote_mark = 0;
        }
        self.action_t = 0.0;
    }

    /// Change action WITHOUT resetting the hold clock — for the continuous
    /// states (walk/run/leap) whose `action_t` is not a one-shot timer.
    fn set_action_keep(&mut self, a: PetAction) {
        if a != self.action {
            self.action = a;
            self.action_t = 0.0;
            self.mote_mark = 0;
        }
    }

    /// Resolve the art frame and the living-cartoon pose for this present.
    fn emit(&mut self, sense: PetSense, width: f32) -> PetFrame {
        let max_col = (f32::from(sense.cols) - width).max(0.0);
        let max_row = f32::from(sense.rows.saturating_sub(1));
        self.col = self.col.clamp(0.0, max_col);
        self.row = self.row.clamp(0.0, max_row);

        let mut scale_x = 1.0f32;
        let mut scale_y = 1.0f32;
        let mut lift = 0.0f32;
        let purr = if matches!(self.action, PetAction::Purr) {
            self.content
        } else {
            0.0
        };

        let pose = match self.action {
            // The breath is the ONE animation a settled pet keeps, so reduced
            // motion has to suppress it explicitly — a sleeping cat is otherwise
            // the one state that never lets the frame settle.
            // Past the breath window the pet is deeply asleep and perfectly
            // still — the frame settles and the host idles to zero.
            PetAction::Sleep
                if sense.reduced_motion || self.quiet >= SLEEP_AFTER + BREATH_WINDOW =>
            {
                PetGlyphId::PetSleep0
            }
            PetAction::Sleep => {
                let t = (self.clock % f64::from(BREATH_PERIOD)) as f32;
                let s = (TAU * t / BREATH_PERIOD).sin();
                scale_y += s * BREATH_DEPTH;
                scale_x -= s * BREATH_DEPTH * 0.5;
                if s >= 0.0 {
                    PetGlyphId::PetSleep1
                } else {
                    PetGlyphId::PetSleep0
                }
            }
            PetAction::Waking => PetGlyphId::PetStretch,
            PetAction::Stand => {
                // The last beats of the stand gather down toward the fold
                // ([`SIT_FOLD`]), so the sit does not snap on.
                let pre = ((self.quiet - (SIT_AFTER - SIT_FOLD)) / SIT_FOLD).clamp(0.0, 1.0);
                scale_y -= 0.05 * pre;
                scale_x += 0.03 * pre;
                PetGlyphId::PetStand
            }
            PetAction::Sit if sense.reduced_motion => PetGlyphId::PetSit,
            PetAction::Sit => {
                // The tail flick: a settled cat is never perfectly inert.
                // Phased on QUIET, not on the free-running clock, so it is a
                // function of this settle rather than of how long the window has
                // been open — which is what makes the sequence reproducible.
                let beat = ((self.quiet - SIT_AFTER).max(0.0) * 0.5).rem_euclid(1.0);
                // The other half of the stand→sit fold: the sit's first beats
                // rise back from the stand's gathered-down height to rest.
                let fold = (1.0 - (self.quiet - SIT_AFTER) / SIT_FOLD).clamp(0.0, 1.0);
                scale_y -= 0.05 * fold;
                scale_x += 0.03 * fold;
                // The settled gaze (review #6): eye contact face-on when the
                // caret is close, the over-the-shoulder peek when it is
                // behind — the plain sit (and its flick) otherwise.
                if self.sit_front {
                    PetGlyphId::PetSitFront
                } else if self.peek {
                    PetGlyphId::PetPeekShoulder
                } else if beat > 0.86 {
                    PetGlyphId::PetSitFlick
                } else {
                    PetGlyphId::PetSit
                }
            }
            PetAction::Loaf if self.wriggle_t > 0.0 => {
                // THE ROLL (wave 4c art): the authored belly-up cycle —
                // flop → flat-on-the-back → over-twist and back — dealt on
                // the wiggle beat, with a whisper of belly bob on top. The
                // frames carry the squirm now; the code only deals them.
                let beat = ((WRIGGLE_DUR - self.wriggle_t) / WRIGGLE_BEAT) as u32;
                let u = (WRIGGLE_DUR - self.wriggle_t) * 9.0;
                scale_y *= 1.0 + 0.04 * u.sin();
                match beat % 4 {
                    0 => PetGlyphId::PetRoll0,
                    2 => PetGlyphId::PetRoll2,
                    _ => PetGlyphId::PetRoll1,
                }
            }
            PetAction::Loaf => {
                // The melt: the loaf's first beats ease down from the sit's
                // height, through the same envelope plumbing as every other
                // settled transition — no snapping.
                let fold = (1.0 - (self.quiet - LOAF_AFTER) / SIT_FOLD).clamp(0.0, 1.0);
                scale_y += 0.05 * fold;
                scale_x -= 0.02 * fold;
                // IDLE MICRO-LIFE (wave 2): the loaf's tail thump — a tiny
                // half-sine bob on quiet-phased beats (the `tend_motes`
                // beat-index scheme: a pure function of THIS settle, so the
                // sequence is replay-identical), alternating direction by
                // beat parity. Structurally inside the lane-hot window:
                // the loaf ends at SLEEP_AFTER, and the settle window's
                // frames carry it — deep sleep stays byte-stable.
                let since = (self.quiet - LOAF_AFTER).max(0.0);
                let beat = (since / FLICK_EVERY).floor();
                let phase = since - beat * FLICK_EVERY;
                if beat >= 1.0 && phase < FLICK_DUR {
                    let amp = TWITCH_BOB * (core::f32::consts::PI * phase / FLICK_DUR).sin();
                    let dir = if (beat as i64) % 2 == 0 { 1.0 } else { -1.0 };
                    scale_y += amp * dir;
                }
                PetGlyphId::PetLoaf
            }
            PetAction::Purr => {
                // A purr entered straight off the fold eases in the same way.
                let fold = (1.0 - (self.quiet - SIT_AFTER) / SIT_FOLD).clamp(0.0, 1.0);
                let t = (self.clock % f64::from(1.0 / PURR_HZ)) as f32;
                let s = (TAU * PURR_HZ * t).sin();
                scale_y += s * PURR_DEPTH - 0.05 * fold;
                scale_x += 0.03 * fold - s * PURR_DEPTH * 0.6;
                PetGlyphId::PetPurr
            }
            PetAction::Groom => PetGlyphId::PetGroom,
            PetAction::Perk => PetGlyphId::PetPerk,
            PetAction::Startle => PetGlyphId::PetStartle,
            PetAction::Frolic if self.swipe => {
                // The bat swipe (wave 2): pinned to the forepaw pose for the
                // whole hold — a swipe that playbows halfway reads as two
                // gestures. The dedicated BatSwipe frame is queued for the
                // art wave; PetBat is the shipping placeholder.
                PetGlyphId::PetBat
            }
            PetAction::Frolic => {
                if ((self.action_t / (FROLIC_HOLD * 0.5)) as u32).is_multiple_of(2) {
                    PetGlyphId::PetPlaybow
                } else {
                    PetGlyphId::PetBat
                }
            }
            PetAction::Droop => {
                // The sulk sinks: a shade shorter and wider as the hold
                // plays out (the landing shape law at grief pace) over the
                // borrowed loaf art — the low pose the roster has, until
                // the art wave lands a flat-ears frame. Zero motes.
                let u = (self.action_t / DROOP_HOLD).clamp(0.0, 1.0);
                scale_y -= 0.06 * u;
                scale_x += 0.03 * u;
                PetGlyphId::PetLoaf
            }
            PetAction::Crouch if self.big_gather => {
                // THE COIL (rung 7) under the butt-wiggle. The envelope's
                // denominator is derived from the KIND right here — the big
                // gather's own duration, cut to the pounce's quick coil over
                // ink exactly as the launch arm cuts it (the ink cut is
                // gauntlet F1's law; the envelope composes with it, never
                // replaces it) — and never from the pounce's field, which
                // §3's staleness law shows would hold 64% of this wiggle at
                // full squash.
                let dur = if self.ink_overlaps(self.col, self.row, width) {
                    CROUCH_DUR
                } else {
                    BIG_CROUCH_DUR
                };
                let (cy, cx) = Self::coil_envelope(self.action_t, dur);
                scale_y *= cy;
                scale_x *= cx;
                // The butt-wiggle: PetCrouch / PetCrouchWiggle alternating at
                // ~WIGGLE_HZ full cycles per second through the big gather.
                if ((self.action_t * WIGGLE_HZ * 2.0) as u32).is_multiple_of(2) {
                    PetGlyphId::PetCrouch
                } else {
                    PetGlyphId::PetCrouchWiggle
                }
            }
            PetAction::Crouch if self.hop_crouch => {
                // The Enter hop's one-tick gather samples its coil at t = 0
                // (an identity today) — keyed on the KIND all the same, so
                // the hop can never inherit a stale pounce denominator.
                let (cy, cx) = Self::coil_envelope(self.action_t, CROUCH_DUR);
                scale_y *= cy;
                scale_x *= cx;
                PetGlyphId::PetCrouch
            }
            PetAction::Crouch if !self.hiding && !self.stakeout && self.play_to.is_none() => {
                // The pounce coil, squashing through its span-scaled length
                // (kept live by the launch arm). The guard is the KIND law's
                // other half: the hide, the stakeout and the play crouches
                // are deliberate HOLDS measured in seconds — an envelope
                // keyed on `PetAction::Crouch` alone would squash a cat that
                // is lying in wait.
                let (cy, cx) = Self::coil_envelope(self.action_t, self.coil_d);
                scale_y *= cy;
                scale_x *= cx;
                PetGlyphId::PetCrouch
            }
            PetAction::Crouch => PetGlyphId::PetCrouch,
            PetAction::Leap => {
                let mut u = 0.5;
                if let Some(f) = self.flight {
                    u = (f.t / f.dur).clamp(0.0, 1.0);
                    // The parabola — or, on a big bound alone, THE HANG
                    // (rung 10): see `flight_lift` for both laws.
                    lift = Self::flight_lift(f.arc, u, f.big);
                    // Stretch into the rise, gather at the apex — ALONG THE
                    // MOTION. A body elongates on its velocity axis: stretch
                    // applied blindly to height made a horizontal pounce leave
                    // the ground taller and narrower, which is backwards. The
                    // dominant axis is judged in pixels (cells are ~half as
                    // wide as they are tall); vertical-dominant flights keep
                    // the tall stretch.
                    //
                    // THE ASYMMETRIC STRETCH (rung 7b): the symmetric
                    // `rise = |1−2u|` is split at the apex — the climb
                    // elongates at LAUNCH_STRETCH, the fall at half that —
                    // in BOTH axis branches, so a row-dominant hop sheds its
                    // touchdown cliff exactly like a horizontal pounce.
                    let stretch = if u < 0.5 {
                        LAUNCH_STRETCH * (1.0 - 2.0 * u)
                    } else {
                        DESCEND_STRETCH * (2.0 * u - 1.0)
                    };
                    let dx = (f.to_col - f.from_col).abs() * f32::from(sense.cell_w);
                    let dy = (f.to_row - f.from_row).abs() * f32::from(sense.cell_h);
                    if dx > dy {
                        scale_x += stretch;
                        scale_y -= 0.6 * stretch;
                    } else {
                        scale_y += stretch;
                        scale_x -= 0.6 * stretch;
                    }
                }
                // EVERY flight gets an ascent and a landing approach — now
                // scheduled in SECONDS through rung 9's normalizer: the rise
                // and the approach are beats a pose must hold long enough to
                // read, however long or short the flight (the old 0.25/0.6
                // u-window crushed the rise on a minimum pounce and
                // stretched the descend to a third of a bound). A flightless
                // leap frame keeps the hero (`u` defaults to 0.5).
                match self.flight {
                    None => PetGlyphId::PetLeap,
                    Some(f) if f.dur < 3.0 * MIN_HERO => {
                        // Too short for three legible beats: two poses,
                        // split at the apex — the hero is never crushed
                        // below MIN_HERO, and a descend can never clamp
                        // longer than its own flight.
                        if u < 0.5 {
                            PetGlyphId::PetLeapRise
                        } else {
                            PetGlyphId::PetLeapDescend
                        }
                    }
                    Some(f) => {
                        let mut rise = LEAP_RISE_T;
                        let mut desc = LEAP_DESC_T;
                        if rise + desc + MIN_HERO > f.dur {
                            // ONE shared scale factor for both approaches —
                            // the explicit normalizer, never two independent
                            // clamp chains — leaving the hero exactly its
                            // floor.
                            let s = (f.dur - MIN_HERO) / (rise + desc);
                            rise *= s;
                            desc *= s;
                        }
                        let t = u * f.dur;
                        if t < rise {
                            PetGlyphId::PetLeapRise
                        } else if t <= f.dur - desc {
                            PetGlyphId::PetLeap
                        } else {
                            PetGlyphId::PetLeapDescend
                        }
                    }
                }
            }
            PetAction::Land => {
                // Three recoveries, one shape law (shorter by q, wider by
                // 0.6 q): the touch-land between bounds is brief and light,
                // the big landing is long and WEIGHTED by its span, and the
                // pounce landing is exactly what it always was.
                //
                // The TOUCH between bounds wears the COIL, not the braced
                // landing (gauntlet F6): the Land pose's open-mouth face
                // smears into a black-muzzle blotch at tile LOD — in BOTH
                // directions; the "leftward mirroring bug" was this pose
                // over glyphs — and the touch is the one landing that
                // lingers mid-text. A touch-and-relaunch IS a re-gather, so
                // the crouch is also the truer frame, and it mirrors
                // cleanly because its face is a tucked profile.
                let (dur, q0, pose) = if self.bound2.is_some() {
                    (TOUCH_LAND_DUR, LAND_SQUASH * 0.5, PetGlyphId::PetCrouch)
                } else if self.land_span > 0.0 {
                    (
                        BIG_LAND_DUR,
                        LAND_SQUASH
                            * (BIG_SQUASH_NEAR
                                + (1.0 - BIG_SQUASH_NEAR)
                                    * (self.land_span / BIG_SQUASH_SPAN).min(1.0)),
                        PetGlyphId::PetLand,
                    )
                } else {
                    (LAND_DUR, LAND_SQUASH, PetGlyphId::PetLand)
                };
                let u = 1.0 - (self.land_t / dur).clamp(0.0, 1.0);
                let q = q0 * (-3.0 * u).exp() * (TAU * 1.35 * u).cos();
                scale_y *= 1.0 - q;
                scale_x *= 1.0 + q * 0.6;
                pose
            }
            PetAction::Walk => Self::CYCLE_WALK[self.gait_index(Self::CYCLE_WALK.len())],
            PetAction::Run => Self::CYCLE_RUN[self.gait_index(Self::CYCLE_RUN.len())],
        };

        // The small-retreat flinch: a decaying duck over whatever pose is up
        // (in practice the perk), shaped by the landing's law — shorter by q,
        // wider by 0.6 q.
        if self.flinch_t > 0.0 {
            let q = FLINCH_SQUASH * (self.flinch_t / FLINCH_DUR).clamp(0.0, 1.0);
            scale_y *= 1.0 - q;
            scale_x *= 1.0 + q * 0.6;
        }
        // THE SCRAMBLE's slipping paw (wave 4): one squashed beat over the
        // gallop (or the drift-brake's plant), same envelope family as the
        // flinch.
        if self.stumble_t > 0.0 {
            let q = 0.18 * (self.stumble_t / STUMBLE_DUR).clamp(0.0, 1.0);
            scale_y *= 1.0 - q;
            scale_x *= 1.0 + q * 0.7;
        }
        // THE SKIP (wave 4): a content walk bobs one stride in four — the
        // bob rides the stride phase (pure function of ground covered, the
        // gait's own clock), so it lands on the same paws every cycle.
        if self.action == PetAction::Walk && self.content >= SKIP_CONTENT {
            let whole = self.stride.rem_euclid(4.0);
            if whole < 1.0 {
                lift += 0.07 * (whole * core::f32::consts::PI).sin().max(0.0);
            }
        }

        // IDLE MICRO-LIFE (wave 2): the ear-twitch stand-in — a 2% head-
        // scale bob over the settled pose (the flinch envelope's precedent),
        // which ear by mote-serial parity at arm time. Upgraded to real
        // ear-twitch frames when the art wave lands them.
        if self.twitch_t > 0.0 {
            let q = TWITCH_BOB * (self.twitch_t / TWITCH_DUR).clamp(0.0, 1.0);
            scale_y *= if self.twitch_up { 1.0 + q } else { 1.0 - q };
        }

        // The breed handoff's ARRIVAL: the incoming kitty fades up over the
        // ramp's own length (`arrive_in` — ARRIVE_IN for a ceremony,
        // HOMECOMING_IN for the quiet re-dress) — multiplied into the fade
        // envelope, never replacing it. Floored so the landing frame is
        // FAINT, never absent: hosts gate "is the pet on glass" on
        // alpha > 0, and a zero here would blank the pet, its hit-box and
        // the just-spawned departing body for one frame — the very cut the
        // arrival exists to remove.
        let arrival = if self.arrive_t > 0.0 {
            (1.0 - (self.arrive_t / self.arrive_in).clamp(0.0, 1.0)).max(ARRIVE_FLOOR)
        } else {
            1.0
        };
        // A flight retired under DECTCEM can leave an opaque body in mid-air.
        // Hold that exact authored lift throughout the hidden fade; after a
        // quick return the live branch eases it down. Applying the custody
        // here covers every early action return through this one emitter.
        lift += self
            .retired_flight_lift
            .map_or(0.0, |(held_lift, _)| held_lift);
        // The last look, captured pre-skin (the brain stays species-blind)
        // for the departure lane: a ghost spawned NOW wears what the user
        // was just looking at, not run frame 0.
        self.last_pose = pose;
        PetFrame {
            alpha: (self.alpha.clamp(0.0, 1.0) * arrival * 255.0) as u8,
            // The presence envelope ALONE — the departure lane's byte. The
            // arrival ramp above belongs to the live body only (§1.1(a)):
            // the leaver departs at full opacity while the arriver fades up.
            lane_alpha: (self.alpha.clamp(0.0, 1.0) * 255.0) as u8,
            action: self.action,
            // THE SPECIES SKIN, APPLIED LAST. Everything above chose a CAT
            // pose; `skin` swaps in the same frame from another species'
            // roster. Doing it here — at the one place a pose becomes a
            // frame — is what lets the whole brain stay species-blind.
            pose: self.species.skin(pose),
            col: self.col,
            row: self.row,
            lift,
            facing_left: self.facing_left,
            scale_x,
            scale_y,
            purr,
            under_ink: self.hiding,
            motes: self.resolve_motes(),
            departures: self.resolve_departures(),
        }
    }

    const CYCLE_WALK: [PetGlyphId; 4] = [
        PetGlyphId::PetWalk0,
        PetGlyphId::PetWalk1,
        PetGlyphId::PetWalk2,
        PetGlyphId::PetWalk3,
    ];
    const CYCLE_RUN: [PetGlyphId; 4] = [
        PetGlyphId::PetRun0,
        PetGlyphId::PetRun1,
        PetGlyphId::PetRun2,
        PetGlyphId::PetRun3,
    ];

    /// The distance-driven gait frame — see the module note on why this is not a
    /// function of elapsed time.
    fn gait_index(&self, len: usize) -> usize {
        let phase = self.stride.rem_euclid(1.0);
        ((phase * len as f32) as usize).min(len - 1)
    }

    /// The current action (for hosts that gate sound or other effects on it).
    #[must_use]
    pub fn action(&self) -> PetAction {
        self.action
    }

    /// True while the pet is visible at all (the host's "is there a cat" test).
    /// NOT the frame-cadence signal — see [`Self::needs_frames`].
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.alpha > 0.0
    }

    /// THE RESIDENT'S SWITCH, HONOURED AT ONCE: drop everything this brain owns
    /// because its owner went away — the cursor-trail master turned off, the trail
    /// style stopped being the pet style, or serious mode took the glass.
    ///
    /// The flying kitty has had this for as long as it has had an owner
    /// (`kitty_cursor::CursorCat::retire_unowned_cursor_motion`, driven by the host's
    /// `retire_kitty_cursor_without_owner`); the resident pet had NOTHING. Hosts could
    /// only stop feeding it a caret, and `tick`'s no-caret arm is a graceful EXIT, not
    /// a switch: it ramps `alpha` down over [`FADE_OUT`] and — read the arm — drops
    /// departures, latches and play state while deliberately leaving the MOTE lane
    /// running ("until the lanes empty, the slow arm keeps running their clocks every
    /// frame").
    ///
    /// WHAT THAT COSTS — measured against the host, not assumed. The pet's PIXELS were
    /// already safe: every emitter gates on the same presentation predicate the switch
    /// moves, so the first frame after the flip draws no pet and no mote. What survived
    /// the switch was this brain's STATE, and through it the FRAME TRAIN.
    /// [`Self::needs_frames`] is true while `alpha` is mid-fade and true for every mote
    /// still in the lane, and that is exactly what a host's effects scheduler consumes
    /// (in the native GUI, `cursor_dependents_need_frame_cadence` — which takes the
    /// pet's `needs_frames()` and does NOT take the trail master as a term). So
    /// switching the companion off left the window running its 60 fps effects lane, for
    /// the whole fade plus the whole remaining life of every z already in flight, to
    /// animate a companion nobody can see. A switch that keeps costing frames after it
    /// is thrown has not been honoured.
    ///
    /// So: no fade, no drift, no owed frames. This is the exact state a fresh window
    /// starts in ([`Self::default`] — asleep, invisible, lanes empty, `needs_frames()`
    /// false), which is also what `WindowState::drain_serious_effects` already
    /// assigns for the serious-mode edge; this method is that same law made reachable
    /// per-frame and idempotent.
    ///
    /// LEVEL, NOT EDGE — the `retire_kitty_cursor_without_owner` discipline. Hosts
    /// call it every frame the pet has no owner, so a config reload, a style swap, a
    /// serious-mode toggle and a startup with the trail already off all take the same
    /// path and none of them needs an edge detector. An already-retired brain returns
    /// on the guard below without writing anything, so the steady "off" state costs
    /// one predicate per frame and cannot churn the resident's identity.
    ///
    /// It is NOT the presentation gate. Focus loss, a scrolled viewport and a
    /// load-shed all hide the pet WITHOUT taking its ownership away, and a resident
    /// that forgot itself every time the window blurred would come back a different
    /// cat. Hosts pass ownership only: `pet_mode && cursor_trail && style == the pet`.
    pub fn retire_unowned(&mut self) {
        if self.alpha == 0.0
            && self.last_caret.is_none()
            && self.motes.iter().all(Option::is_none)
            && self.departures.iter().all(Option::is_none)
            && !self.needs_frames()
        {
            return;
        }
        // The species is host-asserted every frame (`set_species`) and the worn look
        // is re-synced on every emission (`sync_look`), so neither needs carrying
        // across: the next appearance dresses itself from the live verdict.
        *self = Self::default();
    }

    /// Host sync for the pet's identity — the flying kitty's per-appearance
    /// look latch (`kitty_cursor::CursorCat::set_look`), extended to the pet:
    /// ONE APPEARANCE WEARS ONE CAT. The host passes this frame's verdict for
    /// its window — `(coat, iris)` of a pinned favourite, else the tenured
    /// program cat, else the launch kitty — plus how that verdict is owed to
    /// LAND ([`PetArrival`]: the full ceremony, or the quiet re-dress of a
    /// cat coming home), and draws the `worn` pair that comes back.
    ///
    /// While the fade envelope is at zero (or the pet has never been dressed)
    /// there is nothing on screen to protect, so the sync applies
    /// immediately — and `arrival` is IGNORED: the silent change and the
    /// reduced-motion apply are tiers of their own, below both shapes of
    /// theater. While the pet is visible, a differing verdict PARKS: the
    /// walking cat keeps its coat, and the parked pair lands either at the
    /// debounced BREED HANDOFF (the in-place swap with a departing body —
    /// the module's handoff section, which performs the parked arrival) or
    /// once the envelope returns to zero (`tick`'s no-caret arm). The host
    /// re-syncs every emission, so the parking slot always holds the latest
    /// verdict — and THE LAST PARK'S ARRIVAL WINS ([`Self::pending_arrival`]
    /// is overwritten by every parking sync), so the performed shape is the
    /// host's latest ruling too, never a stale intermediate. A favourite
    /// pinned in pet mode therefore latches SILENTLY for the handoff or the
    /// next appearance — the pet has no collection-hello presentation, by
    /// design.
    ///
    /// The outcome's `parked` edge is the host's commit signal — see
    /// [`SyncLookOutcome`].
    pub fn sync_look(&mut self, pair: (u8, u8), arrival: PetArrival) -> SyncLookOutcome {
        match self.worn {
            Some(worn) if self.alpha > 0.0 => {
                let park = (pair != worn).then_some(pair);
                let parked = park.is_some() && park != self.pending_worn;
                if park != self.pending_worn {
                    // BREED HANDOFF: a NEW park (or a changed one) restarts
                    // the swap debounce; a park that dissolved (the
                    // round-trip flip came home) clears the clock — no swap,
                    // no ghost, exactly as if nothing had ever been asked.
                    self.handoff_parked_clock = park.is_some().then_some(self.clock);
                }
                if park.is_some() {
                    // The LAST park's arrival wins — on the fresh park and
                    // on every agreeing re-sync alike.
                    self.pending_arrival = arrival;
                }
                self.pending_worn = park;
                SyncLookOutcome { worn, parked }
            }
            _ => {
                self.worn = Some(pair);
                self.pending_worn = None;
                self.handoff_parked_clock = None;
                SyncLookOutcome {
                    worn: pair,
                    parked: false,
                }
            }
        }
    }

    /// The `(coat, iris)` the pet is wearing RIGHT NOW — the latch, never
    /// the verdict: a parked homecoming has not landed yet, and this stays
    /// the old pair until the handoff performs it. `None` until the host
    /// first dresses the pet. Read-only, so hosts can key their own
    /// bookkeeping (the homecoming roster's commit) to the drawn coat
    /// without a second source of truth.
    #[must_use]
    pub fn worn_pair(&self) -> Option<(u8, u8)> {
        self.worn
    }

    /// Freeze the first prospective-footprint palette sample for the current
    /// resident appearance. Later motion frames reuse it so walking never
    /// thrashes between atlas tiles as the text beneath the body changes.
    pub fn colors_for_appearance(&mut self, sampled: CatColorKey) -> CatColorKey {
        *self.colors.get_or_insert(sampled)
    }

    /// Return the palette already frozen for this resident appearance.
    #[must_use]
    pub fn appearance_colors(&self) -> Option<CatColorKey> {
        self.colors
    }

    /// Retire only the pet's contrast sample. Hosts call this when the pet's
    /// coordinate or palette context changes while it remains visible; the
    /// behaviour, position and breed handoff continue uninterrupted.
    pub fn invalidate_colors(&mut self) {
        self.colors = None;
    }

    /// Whether the pet needs the host's 60 fps lane this frame — the
    /// **idle-to-zero** predicate.
    ///
    /// Deliberately NOT `is_active()`. The pet is a resident, so it is visible
    /// essentially always; arming the cadence on visibility would pin a full
    /// frame rate on a window where nothing is happening, forever. It asks for
    /// frames only while something is genuinely moving — a fade, a chase, a
    /// flight, a landing, any one-shot hold — plus the bounded settle-in window
    /// ([`BREATH_WINDOW`]) that carries the tail flick, the wash, the curl and
    /// the last breath. After that the cat is still, and so is the window.
    #[must_use]
    pub fn needs_frames(&self) -> bool {
        if self.alpha < 1.0 {
            // Mid-fade — INCLUDING alpha == 0 with a caret in view, which is the
            // very first tick of a fade-IN (`last_now` is None, so `dt` is 0 and
            // the ramp has not moved yet). Releasing the lane there would leave
            // the pet invisible until some unrelated repaint happened to tick it
            // again, spreading a 0.30 s appear over seconds of blink intervals.
            return self.last_caret.is_some() || self.alpha > 0.0;
        }
        if self.flight.is_some()
            || self.deferred_hidden_landing.is_some()
            || self.retired_flight_lift.is_some()
            || self.land_t > 0.0
            || self.speed.abs() > 0.01
        {
            return true;
        }
        // The wave-1 envelopes: a live petting hold animates, and a latched
        // stimulus needs ticks to be consumed or to expire — every one of
        // them is finite (TTL, hold, or the consumption itself), so none
        // can pin the lane past its own window (idle-to-zero holds).
        if self.pet_hold_t > 0.0
            || self.pending_bell.is_some()
            || self.pending_cheer.is_some()
            || self.pending_sulk
            || self.pending_pet > 0
        {
            return true;
        }
        // The wave-2 watch: live heat needs ticks to hold the perk AND to
        // decay back to zero — spent or not (codex review, 2026-08-10): a
        // spent watch that released the lane with heat still above the gate
        // could never decay under it, so `watch_spent` never re-armed and no
        // LATER stream was ever watched again. Keeping the lane for the
        // decay is bounded by construction: heat ≤ 1.0 falls at
        // [`WATCH_FALL`]/s once the stream ends — under a second of ticks —
        // and during the stream its own redraws pay for the bookkeeping.
        if self.watch_heat > 0.0 {
            return true;
        }
        // The wave-2 play envelopes: live pointer attention decays to zero
        // on its own clock (the gaze must park), and a latched play intent
        // needs ticks to be consumed — every one finite (heat decay, one
        // crouch, one flight, one flourish).
        if self.pointer_heat > 0.0
            || self.pending_pointer_pounce.is_some()
            || self.play_to.is_some()
            || self.play_land.is_some()
            || self.pending_bat.is_some()
            // The wave-3 games: a live chase runs, a stakeout holds a pose
            // that must re-check its toy, a latched look and an owed groom
            // wait for the ground — all finite (stamina, heat decay, TTL,
            // one hold each).
            || self.pursuit_t.is_some()
            || self.stakeout
            || self.pending_look.is_some()
            || self.groom_owed
            // The wave-4 envelopes: a rally watch must tick to lapse, a
            // stumble to un-squash, a brake to trot home, a wriggle to roll
            // out — all finite.
            || self.tennis
            || self.stumble_t > 0.0
            || self.braking
            || self.wriggle_t > 0.0
            || self.hide_to.is_some()
            || self.hiding
        {
            return true;
        }
        // The breed handoff: a live departing body animates for its strictly
        // finite visit ([`DEPART_MAX`] hard cap — the lane releases the tick
        // after the last ghost expires), and a parked look on an AWAKE pet
        // pins the lane just long enough for the debounce to fire (bounded:
        // debounce + one departure). A parked look on a SLEEPER pins
        // nothing — idle-to-zero outranks costume theater, and the next wake
        // or hide lands the look instead.
        if self.departures.iter().any(Option::is_some) {
            return true;
        }
        if self.pending_worn.is_some() && self.action != PetAction::Sleep {
            return true;
        }
        if self.arrive_t > 0.0 {
            return true; // the incoming kitty is still fading up (finite)
        }
        // Micro-life: a live twitch bob is 0.12 s of finite envelope (the
        // loaf thump needs no term — it lives inside the settle window's
        // own frames, and the loaf ends at SLEEP_AFTER by the ladder).
        if self.twitch_t > 0.0 {
            return true;
        }
        if self.motes.iter().any(Option::is_some) {
            return true; // a mote is drifting: the lane releases AFTER it fades
        }
        if !self.action.settled() {
            return true; // walking, running, startled, frolicking, waking
        }
        self.quiet < SLEEP_AFTER + BREATH_WINDOW
    }

    /// Earned contentment 0..=1.
    #[must_use]
    pub fn content(&self) -> f32 {
        self.content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// THE SPECIES CONTRACT: every cat pose the brain can choose has a dog
    /// counterpart, and the mapping is a permutation — distinct poses stay
    /// distinct. Without both halves a dog could silently fall back to a cat
    /// frame mid-gait (one wrong sprite in a four-beat walk is a visible
    /// stutter), which the `unwrap_or(pose)` fallback would otherwise hide.
    #[test]
    fn every_cat_pose_has_a_distinct_dog_counterpart() {
        let cats: Vec<PetGlyphId> = PET_GLYPH_IDS
            .iter()
            .copied()
            .filter(|id| !PET_GLYPHS[*id as usize].id.starts_with("pet_dog_"))
            .collect();
        assert!(!cats.is_empty(), "the cat roster must not be empty");
        // HashSet, not BTreeSet: `PetGlyphId` is codegen (cat_glyphs_codegen
        // emits `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]` for all
        // three glyph enums), and this test wants set semantics, not ordering.
        // Deriving `Ord` on a generated id enum to satisfy one assertion would
        // widen the public trait surface of three types and invite code that
        // depends on variant declaration order — which is emit order, not a
        // meaningful rank.
        let mut seen = std::collections::HashSet::new();
        for cat in cats {
            let dog = PetSpecies::Dog.skin(cat);
            let (cat_id, dog_id) = (PET_GLYPHS[cat as usize].id, PET_GLYPHS[dog as usize].id);
            assert_ne!(
                dog, cat,
                "{cat_id} fell back to the cat frame — no dog counterpart"
            );
            assert_eq!(
                dog_id,
                &format!("pet_dog_{}", cat_id.strip_prefix("pet_").unwrap()),
                "{cat_id} mapped to the wrong dog pose"
            );
            assert!(seen.insert(dog), "{dog_id} claimed by two cat poses");
        }
    }

    /// The cat skin is the identity, and re-skinning an already-skinned pose is
    /// a no-op — `emit` applies `skin` every frame, so a non-idempotent mapping
    /// would drift the sprite on each present.
    #[test]
    fn skinning_is_identity_for_cats_and_idempotent_for_dogs() {
        for &id in PET_GLYPH_IDS {
            assert_eq!(PetSpecies::Cat.skin(id), id, "the cat skin is identity");
            let dog = PetSpecies::Dog.skin(id);
            assert_eq!(
                PetSpecies::Dog.skin(dog),
                dog,
                "{} re-skinned to a different pose",
                PET_GLYPHS[dog as usize].id
            );
        }
    }

    /// Switching species must not disturb the companion: the pet keeps its
    /// place, its gait phase and its mood, because the skin is applied at
    /// `emit` rather than by rebuilding the brain.
    #[test]
    fn changing_species_preserves_the_pets_state() {
        let mut pet = PetBrain::default();
        let t = Instant::now();
        for i in 0..40 {
            pet.tick(sense(
                t + Duration::from_millis(i * 40),
                Some((4, 10 + i as u16 % 7)),
            ));
        }
        let before = pet.tick(sense(t + Duration::from_millis(1600), Some((4, 20))));
        pet.set_species(PetSpecies::Dog);
        let after = pet.tick(sense(t + Duration::from_millis(1640), Some((4, 20))));
        assert_eq!(pet.species(), PetSpecies::Dog);
        assert_eq!(after.action, before.action, "the action must not reset");
        assert!(
            (after.col - before.col).abs() < 1.0,
            "the pet must not teleport when its coat changes"
        );
        assert!(
            PET_GLYPHS[after.pose as usize].id.starts_with("pet_dog_"),
            "the very next frame must already be a dog"
        );
    }

    #[test]
    fn coordinate_retirement_is_dark_idempotent_and_preserves_identity() {
        let mut pet = PetBrain::default();
        let old_now = Instant::now();
        pet.species = PetSpecies::Dog;
        pet.body_left_px = 3.5;
        pet.content = 0.65;
        pet.quiet = 4.0;
        pet.clock = 19.0;
        pet.worn = Some((2, 3));
        pet.mote_serial = 7;
        pet.mote_mark = 11;
        pet.last_frolic = 13.0;
        pet.bored_cool = 2.0;

        // Representative surface-relative and one-shot state from the old
        // pane. A true owner edge may carry none of it into the replacement
        // coordinate system.
        pet.alpha = 1.0;
        pet.col = 72.0;
        pet.row = 18.0;
        pet.action = PetAction::Run;
        pet.last_caret = Some((18, 70));
        pet.last_now = Some(old_now);
        pet.ink_base = 10;
        pet.ink_spans.extend_from_slice(&[(2, 20), (0, 0)]);
        pet.ink_live = Some(10);
        pet.last_pointer = Some((71.0, 18.0));
        pet.pointer_heat = 1.0;
        pet.pending_pounce = true;
        pet.pending_cheer = Some(true);
        pet.pending_sulk = true;
        pet.deferred_hidden_landing = Some((74.0, 18.0));
        pet.retired_flight_lift = Some((0.75, 2.5));

        pet.retire_coordinate_space();
        assert_eq!(pet.species(), PetSpecies::Dog);
        assert_eq!(pet.body_left_px, 3.5);
        assert_eq!(pet.content, 0.65);
        assert_eq!(pet.quiet, 4.0);
        assert_eq!(pet.clock, 19.0);
        assert_eq!(pet.worn, Some((2, 3)));
        assert_eq!(pet.mote_serial, 7);
        assert_eq!(pet.mote_mark, 11);
        assert_eq!(pet.last_frolic, 13.0);
        assert_eq!(pet.bored_cool, 2.0);
        assert_eq!(pet.action(), PetAction::Sit);
        assert!(!pet.is_active());
        assert!(!pet.needs_frames());
        assert!(pet.last_caret.is_none() && pet.last_now.is_none());
        assert!(pet.flight.is_none());
        assert!(pet.deferred_hidden_landing.is_none());
        assert!(pet.retired_flight_lift.is_none());
        assert!(pet.ink_spans.is_empty() && pet.ink_live.is_none());
        assert!(pet.last_pointer.is_none());
        assert_eq!(pet.pointer_heat, 0.0);
        assert!(!pet.pending_pounce);
        assert!(pet.pending_cheer.is_none() && !pet.pending_sulk);

        // A duplicate lifecycle notification changes nothing, and the next
        // sighting starts at the new pane's lawful station rather than flying
        // from the old pane's stored position.
        pet.retire_coordinate_space();
        assert_eq!(
            (pet.content, pet.worn, pet.species()),
            (0.65, Some((2, 3)), PetSpecies::Dog)
        );
        let new_now = old_now + Duration::from_secs(1);
        let expected_col = pet.station_now(12, 100, art_cols(10, 20), 10);
        let frame = pet.tick_static_capture(sense(new_now, Some((3, 12))));
        assert_eq!(frame.alpha, 255);
        assert_eq!(frame.row, 3.0);
        assert!((frame.col - expected_col).abs() < f32::EPSILON);
        assert!(
            (frame.col - 72.0).abs() > 1.0,
            "old-pane position was retired"
        );
    }

    #[test]
    fn coordinate_retirement_lands_the_latest_parked_identity() {
        let mut pet = PetBrain::default();
        let now = Instant::now();
        assert_eq!(pet.sync_look((2, 3), PetArrival::Ceremony).worn, (2, 3));
        pet.alpha = 1.0;
        assert_eq!(
            pet.sync_look((7, 8), PetArrival::Ceremony).worn,
            (2, 3),
            "the visible outgoing appearance keeps its old look"
        );
        assert_eq!(pet.worn, Some((2, 3)));
        assert_eq!(pet.pending_worn, Some((7, 8)));

        pet.retire_coordinate_space();
        assert_eq!(pet.worn, Some((7, 8)));
        assert!(pet.pending_worn.is_none());
        assert!(!pet.is_active());

        let frame = pet.tick_static_capture(sense(now, Some((3, 12))));
        assert!(frame.alpha > 0);
        assert_eq!(
            pet.sync_look((7, 8), PetArrival::Ceremony).worn,
            (7, 8),
            "the next lawful sighting wears the latest parked identity"
        );
        assert!(pet.pending_worn.is_none());
    }

    fn sense(now: Instant, caret: Option<(u16, u16)>) -> PetSense {
        PetSense {
            now,
            caret,
            wrapped: false,
            rows: 30,
            cols: 100,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        }
    }

    /// One tick with the pointer at `ptr` (fractional cells).
    fn ptick(
        pet: &mut PetBrain,
        t: Instant,
        caret: (u16, u16),
        ptr: Option<(f32, f32)>,
    ) -> PetFrame {
        let mut s = sense(t, Some(caret));
        s.pointer = ptr;
        pet.tick(s)
    }

    /// Drive `secs` of 60 fps frames with the STREAM on (`output_burst`),
    /// the caret parked at `caret` — the perk-and-watch fixture.
    fn stream(
        pet: &mut PetBrain,
        start: Instant,
        caret: (u16, u16),
        secs: f32,
    ) -> (Instant, PetFrame) {
        let mut t = start;
        let end = start + Duration::from_secs_f32(secs);
        let mut s = sense(t, Some(caret));
        s.output_burst = true;
        let mut frame = pet.tick(s);
        while t < end {
            t += Duration::from_millis(16);
            let mut s = sense(t, Some(caret));
            s.output_burst = true;
            frame = pet.tick(s);
        }
        (t, frame)
    }

    /// Drive `keys` forward keystrokes at `gap` seconds each from `(row, col)`,
    /// ticking the brain at 60 fps in between (the host's real cadence).
    fn type_run(
        pet: &mut PetBrain,
        start: Instant,
        row: u16,
        col0: u16,
        keys: u16,
        gap: f32,
    ) -> (Instant, PetFrame) {
        let mut t = start;
        let mut frame = pet.tick(sense(t, Some((row, col0))));
        // `keys == 0` is a legal (if degenerate) run: the seeding tick above is
        // then the returned frame, so the binding is genuinely read.
        for k in 1..=keys {
            let until = t + Duration::from_secs_f32(gap);
            // The between-keys frames advance the brain; only the frame at the
            // end of the run is inspected.
            while t < until {
                t += Duration::from_millis(16);
                let _ = pet.tick(sense(t, Some((row, col0 + k - 1))));
            }
            t = until;
            frame = pet.tick(sense(t, Some((row, col0 + k))));
        }
        (t, frame)
    }

    /// Take a fresh (asleep) pet through its wake-up so a test can start from a
    /// settled, awake cat standing at its station.
    fn awake(pet: &mut PetBrain, start: Instant, row: u16, col: u16) -> Instant {
        let (t, _) = type_run(pet, start, row, col, 2, 0.05);
        let (t, _) = idle(pet, t, (row, col + 2), SIT_AFTER + 0.2);
        t
    }

    fn idle(
        pet: &mut PetBrain,
        start: Instant,
        caret: (u16, u16),
        secs: f32,
    ) -> (Instant, PetFrame) {
        let mut t = start;
        let end = start + Duration::from_secs_f32(secs);
        let mut frame = pet.tick(sense(t, Some(caret)));
        while t < end {
            t += Duration::from_millis(16);
            frame = pet.tick(sense(t, Some(caret)));
        }
        (t, frame)
    }

    #[test]
    fn a_first_sighting_materialises_at_the_station_not_at_the_origin() {
        let mut pet = PetBrain::default();
        let f = pet.tick(sense(Instant::now(), Some((5, 40))));
        let w = art_cols(10, 20);
        assert!(
            (f.col - PetBrain::station(40, 100, w)).abs() < 0.01,
            "first frame must appear at its station, got col {}",
            f.col
        );
        assert!((f.row - 5.0).abs() < 0.01, "and on the caret's row");
    }

    #[test]
    fn steady_typing_walks_and_fast_typing_runs() {
        let start = Instant::now();
        let mut slow = PetBrain::default();
        let t = awake(&mut slow, start, 3, 5);
        let (_, slow_f) = type_run(&mut slow, t, 3, 7, 10, 0.30);
        let mut fast = PetBrain::default();
        let t = awake(&mut fast, start, 3, 5);
        let (_, fast_f) = type_run(&mut fast, t, 3, 7, 30, 0.045);
        assert_eq!(
            slow_f.action,
            PetAction::Walk,
            "an unhurried cadence walks the pet"
        );
        assert_eq!(
            fast_f.action,
            PetAction::Run,
            "a fast cadence outruns a walk"
        );
    }

    #[test]
    fn the_gait_cycle_is_driven_by_distance_not_by_the_clock() {
        // Same wall-clock elapsed, very different distance travelled: the stride
        // phase must follow the DISTANCE. A wall-clock cycle would tie.
        let start = Instant::now();
        let mut near = PetBrain::default();
        let t = awake(&mut near, start, 3, 5);
        let _ = type_run(&mut near, t, 3, 7, 4, 0.05);
        let mut far = PetBrain::default();
        let t = awake(&mut far, start, 3, 5);
        let _ = type_run(&mut far, t, 3, 7, 30, 0.05);
        assert!(
            far.stride > near.stride * 2.0,
            "more travel = more stride ({} vs {})",
            far.stride,
            near.stride
        );
    }

    #[test]
    fn a_settled_pet_stops_asking_for_frames_so_the_host_idles_to_zero() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (t, _) = type_run(&mut pet, start, 4, 10, 6, 0.05);
        assert!(pet.needs_frames(), "a chasing pet needs the 60 fps lane");
        // Long past the sit, the sleep, and the breath window.
        let (_, f) = idle(&mut pet, t, (4, 16), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert_eq!(f.action, PetAction::Sleep);
        assert!(
            !pet.needs_frames(),
            "a deeply asleep pet must release the frame lane"
        );
        assert!(pet.is_active(), "it is still on screen, just still");
        // DEEP sleep spawns nothing, and the light-sleep z's have all died
        // BEFORE the lane released: the mote lane must be empty here or the
        // byte-stability below would be a lie.
        assert!(
            f.motes.iter().all(Option::is_none),
            "deep sleep must hold an empty mote lane"
        );
        // And the frame it draws is byte-stable.
        let a = pet.tick(sense(start + Duration::from_secs(60), Some((4, 16))));
        let b = pet.tick(sense(start + Duration::from_secs(61), Some((4, 16))));
        assert_eq!(a.fp(), b.fp(), "a still cat must produce a settled frame");
    }

    #[test]
    fn a_low_cadence_host_still_puts_the_pet_to_sleep_on_time() {
        // Once the pet releases the frame lane the host drops to the blink's
        // ~1 Hz. The quiet clock must still run in WALL time, or "22 seconds of
        // silence" would mean minutes and an idle window would never get a
        // sleeping cat.
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, _) = type_run(&mut pet, start, 4, 10, 4, 0.05);
        let mut slept_at = None;
        for i in 0..40 {
            t += Duration::from_millis(1000);
            let f = pet.tick(sense(t, Some((4, 14))));
            if f.action == PetAction::Sleep && slept_at.is_none() {
                slept_at = Some(i);
            }
        }
        let i = slept_at.expect("a 1 Hz host must still reach sleep");
        assert!(
            (SLEEP_AFTER as i32 - 2..=SLEEP_AFTER as i32 + 2).contains(&i),
            "slept after {i} one-second frames, expected ~{SLEEP_AFTER}"
        );
    }

    /// REGRESSION: a repeating jump used to re-enter `Perk` on every move, and
    /// `set_action` resets the hold clock — so a held word-jump pinned the pet
    /// in the 0.30 s double-take forever and it never chased, hopped or pounced.
    #[test]
    fn a_repeating_jump_must_not_pin_the_pet_in_its_double_take() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 4);
        let start_col = pet.tick(sense(t, Some((4, 6)))).col;
        // A held word-jump: +8 columns every frame, faster than PERK_HOLD.
        let mut caret = 6u16;
        let mut travelled = 0.0f32;
        let mut last = start_col;
        for _ in 0..60 {
            t += Duration::from_millis(16);
            caret = (caret + 8).min(95);
            let f = pet.tick(sense(t, Some((4, caret))));
            travelled += (f.col - last).abs();
            last = f.col;
        }
        assert!(
            travelled > 5.0,
            "the pet froze through a held jump: {travelled} cells of travel while \
             the caret ran to column {caret}"
        );
    }

    /// REGRESSION, upgraded by the screen-crossing choreography: the vertical
    /// branch of `begin_flight` had no duration clamp, and its `span` is the
    /// HORIZONTAL distance — so an Enter from the end of a long line floated
    /// the cat for seconds, ignoring every move meanwhile. A wrap that long
    /// now trips the BIG-JUMP threshold and plays the ratified show instead —
    /// but every single airborne stretch stays bounded by BIG_FLIGHT_MAX, so
    /// the caret is only ever owned one bound at a time, never one glide.
    #[test]
    fn a_wrap_from_a_long_line_bounds_across_never_glides() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 190);
        let mut run = 0u32;
        let mut max_run = 0u32;
        let mut last_row = 4.0f32;
        for _ in 0..260 {
            t += Duration::from_millis(16);
            // Enter at the end of a 200-column line: row +1, column 0.
            let mut s = sense(t, Some((5, 0)));
            s.cols = 200;
            let f = pet.tick(s);
            if f.action == PetAction::Leap {
                run += 1;
                max_run = max_run.max(run);
            } else {
                run = 0;
            }
            last_row = f.row;
        }
        let secs = max_run as f32 * 0.016;
        assert!(
            secs <= BIG_FLIGHT_MAX + 0.05,
            "each airborne stretch must be bounded by BIG_FLIGHT_MAX, floated {secs:.2}s"
        );
        assert!(
            (last_row - 5.0).abs() < 0.01,
            "and the crossing must actually arrive on the new row, got {last_row}"
        );
    }

    /// REGRESSION: the no-caret branch kept `last_caret`, so a cursor that hid
    /// and reappeared at a fresh prompt diffed against a stale cell and the pet
    /// faded back in flinching from a retreat nobody typed.
    #[test]
    fn a_caret_that_hides_and_returns_elsewhere_is_a_new_sighting_not_a_retreat() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 78);
        for _ in 0..60 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, None));
        }
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((5, 3))));
        assert_ne!(
            f.action,
            PetAction::Startle,
            "reappearing elsewhere is not a backspace"
        );
        let w = art_cols(10, 20);
        assert!(
            (f.col - PetBrain::station(3, 100, w)).abs() < 0.01,
            "and it re-materialises at the new station, got col {}",
            f.col
        );
    }

    /// REGRESSION: `needs_frames()` returned false on the very first tick (alpha
    /// is still exactly 0 because `dt` is 0), releasing the frame lane for a pet
    /// that was about to fade in — so it appeared over seconds of blink ticks.
    #[test]
    fn the_first_tick_of_a_fade_in_keeps_the_frame_lane() {
        let mut pet = PetBrain::default();
        let f = pet.tick(sense(Instant::now(), Some((5, 10))));
        assert_eq!(f.alpha, 0, "the ramp has not moved on the seeding tick");
        assert!(
            pet.needs_frames(),
            "but the pet is arriving, so it still needs frames"
        );
    }

    #[test]
    fn the_first_windowless_still_materializes_the_resident_pet() {
        let mut pet = PetBrain::default();
        let f = pet.tick_static_capture(sense(Instant::now(), Some((5, 10))));
        assert_eq!(f.alpha, 255, "a requested still contains the resident pet");
        assert!(pet.is_active(), "status agrees with the captured pixels");
        assert!(
            (f.col - PetBrain::station(10, 100, art_cols(10, 20))).abs() < 0.01,
            "the static capture uses the normal first-sighting station"
        );
    }

    #[test]
    fn a_fresh_pet_is_asleep_until_you_type() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let f = pet.tick(sense(start, Some((4, 10))));
        assert_eq!(f.action, PetAction::Sleep, "an untouched window naps");
        let g = pet.tick(sense(start + Duration::from_millis(16), Some((4, 11))));
        assert_eq!(g.action, PetAction::Waking, "and the first key wakes it");
    }

    #[test]
    fn a_word_jump_pounces_through_a_crouch_and_a_landing() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        // A 20-column forward jump.
        let mut saw = (false, false, false);
        for _ in 0..140 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 30))));
            match f.action {
                PetAction::Crouch => saw.0 = true,
                PetAction::Leap => {
                    saw.1 = true;
                    assert!(
                        f.lift > 0.0
                            || matches!(
                                f.pose,
                                PetGlyphId::PetLeapRise
                                    | PetGlyphId::PetLeap
                                    | PetGlyphId::PetLeapDescend
                            )
                    );
                }
                PetAction::Land => saw.2 = true,
                _ => {}
            }
        }
        assert_eq!(
            saw,
            (true, true, true),
            "a pounce is crouch -> leap -> land, got {saw:?}"
        );
    }

    #[test]
    fn the_leap_arc_leaves_the_ground_and_lands_back_on_a_baseline() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        let mut peak = 0.0f32;
        let mut last = 0.0f32;
        for _ in 0..160 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 40))));
            peak = peak.max(f.lift);
            last = f.lift;
            if f.action == PetAction::Land {
                break;
            }
        }
        assert!(
            peak > 0.3,
            "the arc must actually leave the floor, got {peak}"
        );
        assert!(
            last.abs() < 1e-3,
            "and the landing must be exactly on the baseline, got {last}"
        );
    }

    #[test]
    fn a_new_line_hops_rather_than_sliding_diagonally() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 30);
        let mut airborne = false;
        let mut rows_seen: Vec<f32> = Vec::new();
        for _ in 0..160 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((5, 0))));
            rows_seen.push(f.row);
            if f.action == PetAction::Leap && f.lift > 0.0 {
                airborne = true;
            }
        }
        assert!(airborne, "a row change must arc, not slide");
        let landed = rows_seen.last().copied().unwrap_or(0.0);
        assert!(
            (landed - 5.0).abs() < 0.01,
            "and land on row 5, got {landed}"
        );
    }

    /// One backspace is a typo being fixed, not a threat: the pet ducks (the
    /// squash envelope) and looks hard at the caret (the perk hold) — the
    /// full bottled-tail fright is reserved for real retreats.
    #[test]
    fn a_single_backspace_flinches_without_the_full_bottle() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, _) = type_run(&mut pet, start, 2, 10, 8, 0.06);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((2, 17))));
        assert_ne!(
            f.action,
            PetAction::Startle,
            "one backspace must not fire the whole bottle"
        );
        assert_eq!(f.action, PetAction::Perk, "the flinch is a perk-and-duck");
        assert_eq!(f.pose, PetGlyphId::PetPerk);
        assert!(
            f.scale_y < 1.0 && f.scale_x > 1.0,
            "and the duck must be visible in the envelope: y {} x {}",
            f.scale_y,
            f.scale_x
        );
    }

    #[test]
    fn a_big_retreat_still_bottles() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (t, _) = type_run(&mut pet, start, 2, 10, 8, 0.06);
        // Let the pet arrive at its station past the caret, so the retreat's
        // geometry is deterministic: the caret ends clearly to its left.
        let (mut t, _) = idle(&mut pet, t, (2, 18), 0.5);
        t += Duration::from_millis(16);
        // Five columns back in one move: a real retreat.
        let f = pet.tick(sense(t, Some((2, 13))));
        assert_eq!(f.action, PetAction::Startle);
        assert_eq!(f.pose, PetGlyphId::PetStartle);
        assert!(
            f.facing_left,
            "in open grid the caret retreats to the pet's left"
        );
    }

    /// C3: the startle used to set `facing_left = true` unconditionally — at
    /// the right-margin wall, where the pet parks LEFT of the caret, that
    /// faced it AWAY from the very deletion that scared it.
    #[test]
    fn startle_faces_the_caret_even_at_the_wall() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 93);
        // The caret ended at column 95; its station hangs off the right
        // margin, so the pet crossed to the caret's LEFT.
        assert!(
            pet.col < 90.0,
            "fixture: the pet must be parked left of the caret, col {}",
            pet.col
        );
        t += Duration::from_millis(16);
        // A five-column retreat that still lands RIGHT of the pet.
        let f = pet.tick(sense(t, Some((5, 90))));
        assert_eq!(f.action, PetAction::Startle);
        assert!(
            !f.facing_left,
            "the fright must face the caret, not a compass point"
        );
    }

    #[test]
    fn a_held_backspace_is_one_sustained_fright_not_a_strobe() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, _) = type_run(&mut pet, start, 2, 10, 10, 0.05);
        let mut col = 19u16;
        let mut episodes = 0;
        let mut was = PetAction::Walk;
        for _ in 0..8 {
            t += Duration::from_millis(40);
            col -= 1;
            let f = pet.tick(sense(t, Some((2, col))));
            if f.action == PetAction::Startle && was != PetAction::Startle {
                episodes += 1;
            }
            was = f.action;
        }
        assert_eq!(episodes, 1, "one burst, one fright — got {episodes}");
    }

    #[test]
    fn fussing_back_and_forth_makes_it_frolic() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 6, 20);
        let cols = [23u16, 22, 23, 22, 23];
        let mut frolicked = false;
        for c in cols {
            for _ in 0..6 {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((6, c))));
                if f.action == PetAction::Frolic {
                    frolicked = true;
                }
            }
        }
        assert!(frolicked, "three reversals inside the window is play");
    }

    #[test]
    fn quiet_settles_then_sleeps_and_typing_wakes_it_with_a_stretch() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 18);
        let (t, f) = idle(&mut pet, t, (4, 20), 0.4);
        assert!(
            matches!(f.action, PetAction::Sit | PetAction::Purr),
            "a short quiet sits, got {:?}",
            f.action
        );
        let (mut t, f) = idle(&mut pet, t, (4, 20), SLEEP_AFTER);
        assert_eq!(f.action, PetAction::Sleep, "a long quiet sleeps");
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 21))));
        assert_eq!(f.action, PetAction::Waking);
        assert_eq!(f.pose, PetGlyphId::PetStretch);
    }

    #[test]
    fn sleep_breathes_so_the_frame_never_settles_to_a_dead_sticker() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, _) = idle(&mut pet, start, (4, 20), 0.2);
        let mut poses = std::collections::BTreeSet::new();
        let mut scales = std::collections::BTreeSet::new();
        for _ in 0..200 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 20))));
            poses.insert(f.pose as u8);
            scales.insert((f.scale_y * 4096.0) as i64);
        }
        assert_eq!(poses.len(), 2, "sleep alternates its two breath frames");
        assert!(scales.len() > 20, "and the swell is continuous");
    }

    #[test]
    fn a_good_run_earns_a_purr_and_a_cold_start_does_not() {
        let start = Instant::now();
        let mut worked = PetBrain::default();
        let t = awake(&mut worked, start, 3, 2);
        let (t, _) = type_run(&mut worked, t, 3, 4, 40, 0.05);
        let (_, f) = idle(&mut worked, t, (3, 44), SIT_AFTER + 0.6);
        assert_eq!(f.action, PetAction::Purr, "a sentence of running earns it");
        assert!(f.purr > 0.0);

        // A cat that has only just woken up and taken two steps has not earned
        // anything: contentment is bought with travel, not with elapsed time.
        let mut idle_cat = PetBrain::default();
        let t = awake(&mut idle_cat, start, 3, 40);
        let (_, g) = idle(&mut idle_cat, t, (3, 42), 0.6);
        assert_eq!(g.action, PetAction::Sit, "sitting down cold does not");
        assert_eq!(g.purr, 0.0);
    }

    #[test]
    fn a_long_sit_grooms_itself() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 18);
        let (_, f) = idle(&mut pet, t, (4, 20), GROOM_AFTER + 0.3);
        assert_eq!(f.action, PetAction::Groom);
    }

    #[test]
    fn the_pet_knows_the_wall_and_crosses_to_the_caret_s_left() {
        let w = art_cols(10, 20);
        let mid = PetBrain::station(40, 100, w);
        assert!(mid > 40.0, "in open grid it stands past the caret");
        let edge = PetBrain::station(99, 100, w);
        assert!(
            edge + w <= 100.0,
            "at the right margin it must still fit on screen ({edge} + {w})"
        );
        assert!(edge < 99.0, "which means crossing to the caret's left");
    }

    #[test]
    fn the_pet_never_leaves_the_viewport() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        let w = art_cols(10, 20);
        for col in [0u16, 99, 0, 99, 50] {
            for _ in 0..40 {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((0, col))));
                assert!(f.col >= 0.0, "left edge escaped: {}", f.col);
                assert!(f.col + w <= 100.0 + 1e-3, "right edge escaped: {}", f.col);
                assert!(f.row >= 0.0 && f.row <= 29.0, "row escaped: {}", f.row);
            }
        }
    }

    #[test]
    fn a_hidden_caret_fades_the_pet_out_and_the_fingerprint_settles_to_zero() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, _) = idle(&mut pet, start, (4, 20), 1.0);
        let mut last = PetFrame {
            alpha: 255,
            lane_alpha: 255,
            action: PetAction::Sit,
            pose: PetGlyphId::PetSit,
            col: 0.0,
            row: 0.0,
            lift: 0.0,
            facing_left: false,
            scale_x: 1.0,
            scale_y: 1.0,
            purr: 0.0,
            under_ink: false,
            motes: [None; PET_MOTES_MAX],
            departures: [None; PET_DEPARTURES_MAX],
        };
        for _ in 0..80 {
            t += Duration::from_millis(16);
            last = pet.tick(sense(t, None));
        }
        assert_eq!(last.alpha, 0, "a hidden caret grounds the pet");
        assert_eq!(last.fp(), 0, "and the repaint key settles");
        assert!(!pet.is_active());
    }

    #[test]
    fn reduced_motion_pins_the_pet_at_its_station_with_no_arc_or_gait() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        let w = art_cols(10, 20);
        let mut fps = std::collections::BTreeSet::new();
        for col in [10u16, 60, 12] {
            for _ in 0..20 {
                t += Duration::from_millis(16);
                let mut s = sense(t, Some((3, col)));
                s.reduced_motion = true;
                let f = pet.tick(s);
                assert_eq!(f.lift, 0.0, "no arcs under reduced motion");
                assert_eq!(f.scale_x, 1.0);
                assert_eq!(f.scale_y, 1.0);
                assert!(matches!(f.action, PetAction::Sit | PetAction::Sleep));
                assert!((f.col - PetBrain::station(col, 100, w)).abs() < 1e-3);
                assert_eq!(f.alpha, 255, "a static pet appears in one sample");
                fps.insert(f.fp());
            }
        }
        assert!(
            fps.len() <= 3,
            "a reduced-motion pet must SETTLE (one still frame per caret cell), \
             not animate: {} distinct frames",
            fps.len()
        );
    }

    #[test]
    fn reduced_motion_first_live_caret_sample_is_an_opaque_static_pet() {
        let now = Instant::now();
        let mut pet = PetBrain::default();
        let mut input = sense(now, Some((4, 12)));
        input.reduced_motion = true;
        let frame = pet.tick(input);

        assert_eq!(
            frame.alpha, 255,
            "the first requested still is already visible"
        );
        assert_eq!(frame.lift, 0.0);
        assert_eq!((frame.scale_x, frame.scale_y), (1.0, 1.0));
        assert!(matches!(frame.action, PetAction::Sit | PetAction::Sleep));
    }

    #[test]
    fn a_suspended_host_clock_cannot_teleport_the_pet() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (_, f0) = idle(&mut pet, start, (4, 10), 1.0);
        // One frame, one hour later, with the caret across the screen.
        let f1 = pet.tick(sense(start + Duration::from_secs(3600), Some((4, 90))));
        let jump = (f1.col - f0.col).abs();
        assert!(
            jump < 5.0,
            "a clamped dt must bound the per-frame step, moved {jump} cells"
        );
    }

    #[test]
    fn the_chase_converges_without_oscillating_around_the_station() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        let w = art_cols(10, 20);
        let target = PetBrain::station(60, 100, w);
        let mut crossings = 0;
        let mut side = 0i8;
        for _ in 0..200 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((3, 60))));
            let s = if f.col > target + 0.02 {
                1
            } else if f.col < target - 0.02 {
                -1
            } else {
                0
            };
            if s != 0 && side != 0 && s != side {
                crossings += 1;
            }
            if s != 0 {
                side = s;
            }
        }
        assert!(
            crossings <= 1,
            "a follower must not ring: {crossings} crossings"
        );
    }

    #[test]
    fn facing_follows_travel_with_hysteresis() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        let mut f = pet.tick(sense(t, Some((3, 10))));
        // 140 ticks: enough for the wake, the big-jump notice and gather
        // (the 70-column move now plays the screen-crossing choreography),
        // and the launch that sets the travel facing.
        for _ in 0..140 {
            t += Duration::from_millis(16);
            f = pet.tick(sense(t, Some((3, 80))));
        }
        assert!(!f.facing_left, "chasing right, facing right");
        for _ in 0..80 {
            t += Duration::from_millis(16);
            f = pet.tick(sense(t, Some((3, 5))));
        }
        assert!(f.facing_left, "chasing left, facing left");
    }

    /// ONE APPEARANCE WEARS ONE CAT — the flying kitty's `set_look` latch,
    /// extended to the pet: a companion repoint that lands mid-walk (a typed
    /// discovery in pet mode) must not re-skin the cat on screen. The sync
    /// parks, the appearance keeps its coat, and the parked pair lands once
    /// the fade envelope has returned to zero — so the NEXT appearance is the
    /// one that wears it, even though every sync after the wake arrives while
    /// the pet is already visible again.
    #[test]
    fn a_mid_appearance_look_sync_parks_until_the_pet_has_faded_out() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 10);
        assert!(pet.is_active(), "the fixture needs a visible pet");
        assert_eq!(
            pet.sync_look((3, 1), PetArrival::Ceremony).worn,
            (3, 1),
            "the first dress applies"
        );
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (3, 1),
            "a repoint mid-appearance keeps the worn coat"
        );
        // The caret hides; the envelope runs to zero (FADE_OUT is 0.45 s).
        for _ in 0..120 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, None));
        }
        assert!(!pet.is_active(), "the fade-out must complete");
        // A fresh appearance: the caret returns and the pet is visible again
        // BEFORE the next sync arrives — exactly the emission order the host
        // uses (tick, then sync at draw time).
        for _ in 0..4 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((5, 10))));
        }
        assert!(pet.is_active(), "the next appearance is under way");
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (9, 4),
            "the parked pair landed at zero alpha, so the new appearance wears it"
        );
    }

    #[test]
    fn resident_palette_is_owned_by_one_pet_appearance() {
        let dark = CatColorKey {
            accent: 1,
            background: 0,
        };
        let light = CatColorKey {
            accent: 7,
            background: 3,
        };
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 10);
        assert_eq!(pet.appearance_colors(), None);
        assert_eq!(pet.colors_for_appearance(dark), dark);
        assert_eq!(
            pet.colors_for_appearance(light),
            dark,
            "walking frames keep one stable atlas palette"
        );

        pet.invalidate_colors();
        assert_eq!(
            pet.colors_for_appearance(light),
            light,
            "a host context edge resamples without resetting the brain"
        );
        for _ in 0..120 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, None));
        }
        assert!(!pet.is_active(), "the fixture completes its fade-out");
        assert_eq!(
            pet.appearance_colors(),
            None,
            "zero alpha retires the old appearance palette"
        );

        for _ in 0..4 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((5, 10))));
        }
        assert!(pet.is_active());
        assert_eq!(pet.colors_for_appearance(dark), dark);
    }

    /// The pet_stand pose shipped in the binary from day one and was never
    /// emitted: every stop froze a mid-gait walk frame for the 1.4 s until
    /// the sit. Arrived-but-not-yet-sitting is now a real state with the
    /// real art.
    #[test]
    fn stand_is_emitted_between_arrival_and_sit() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (t, _) = type_run(&mut pet, t, 4, 12, 3, 0.06);
        // Long enough to arrive, well short of the sit.
        let (t, f) = idle(&mut pet, t, (4, 15), 1.0);
        assert_eq!(f.action, PetAction::Stand, "arrived + quiet < SIT_AFTER");
        assert_eq!(f.pose, PetGlyphId::PetStand, "the shipped art, on glass");
        // And the fold still lands where it always did.
        let (_, g) = idle(&mut pet, t, (4, 15), SIT_AFTER);
        assert!(
            matches!(g.action, PetAction::Sit | PetAction::Purr),
            "the stand folds into the sit, got {:?}",
            g.action
        );
    }

    /// A body elongates along its velocity: a horizontal pounce used to leave
    /// the ground TALLER and narrower, which is backwards.
    #[test]
    fn horizontal_flight_stretches_along_the_motion_axis() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        let mut stretched = false;
        for _ in 0..140 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 30))));
            if f.action == PetAction::Leap {
                assert!(
                    f.scale_x >= f.scale_y,
                    "a horizontal leap must never go tall: x {} y {}",
                    f.scale_x,
                    f.scale_y
                );
                if f.scale_x > 1.02 {
                    stretched = true;
                    assert!(f.scale_y < 1.0, "the stretch squashes the height");
                }
            }
        }
        assert!(stretched, "the leap must actually stretch along its travel");
    }

    /// At MAX_SPEED a 1.7-cell stride cycled at 15.3 Hz — exactly one art
    /// frame per 60 fps tick, a strobe. The gallop's own stride keeps the
    /// cycle at or under half the frame rate, and the run gait must actually
    /// be using it.
    #[test]
    fn the_gallop_cycle_never_exceeds_half_frame_rate() {
        // The constant-vs-constant half of this law is a build-time `const _`
        // beside `RUN_STRIDE_CELLS`; what is left here is the half only a run
        // can show — that the gait on glass is the one that stride describes.
        // Drive a sustained chase: one column every three ticks (~20.8
        // cells/s) gallops the pet without ever tripping the pounce gap,
        // then measure cells travelled per stride cycle over the gallop.
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 3, 2);
        let mut caret = 4u16;
        let mut first: Option<(f32, f32)> = None;
        let mut last = (0.0f32, 0.0f32);
        for i in 0..240 {
            t += Duration::from_millis(16);
            if i % 3 == 2 {
                caret = (caret + 1).min(95);
            }
            let f = pet.tick(sense(t, Some((3, caret))));
            assert_ne!(
                f.action,
                PetAction::Leap,
                "fixture: this cadence must run the pet, not launch it"
            );
            if f.action == PetAction::Run {
                if first.is_none() {
                    first = Some((f.col, pet.stride));
                }
                last = (f.col, pet.stride);
            }
        }
        let (c0, s0) = first.expect("a 20 cells/s chase must reach a gallop");
        let cells = (last.0 - c0).abs();
        let cycles = last.1 - s0;
        assert!(cells > 10.0, "need a sustained gallop to measure: {cells}");
        let eff = cells / cycles;
        assert!(
            eff > 3.0,
            "the gallop must pace itself by RUN_STRIDE_CELLS, measured \
             {eff} cells per cycle"
        );
    }

    // ── the keep-ahead lead ─────────────────────────────────────────────

    /// "I'd like for it to keep ahead of the cursor": under a sustained
    /// same-direction rhythm the station moves out past caret+1, so the pet
    /// stands AHEAD of the writing instead of trailing it by the chase lag.
    #[test]
    fn a_typing_rhythm_grows_the_lead_past_the_base_station() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 5);
        // 40 keys at 20 keys/s: an unambiguous rhythm.
        let (_, f) = type_run(&mut pet, t, 3, 7, 40, 0.05);
        assert!(
            f.col >= 47.0 + STATION_LEAD + 0.5,
            "a galloping caret must be LED, not chased: pet at {} for caret 47",
            f.col
        );
    }

    /// Hunt-and-peck — a letter every second — never opens the rhythm gate,
    /// so the pet keeps its plain station and never scoots on speculation.
    #[test]
    fn hunt_and_peck_never_sees_the_lead() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 5);
        let (t, _) = type_run(&mut pet, t, 3, 7, 6, 1.0);
        let (_, f) = idle(&mut pet, t, (3, 13), 0.8);
        assert!(
            (f.col - (13.0 + STATION_LEAD)).abs() <= 0.3,
            "one key a second is not a rhythm: pet at {} for caret 13",
            f.col
        );
    }

    /// Alternating directions (editing) break the same-direction run, so the
    /// pet never leads a caret that is being fussed with rather than driven.
    #[test]
    fn alternating_moves_never_lead() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 3, 20);
        let mut max_col = 0.0f32;
        for k in 0..24u16 {
            let c = if k % 2 == 0 { 23 } else { 22 };
            for _ in 0..8 {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((3, c))));
                max_col = max_col.max(f.col);
            }
        }
        assert!(
            max_col <= 23.0 + STATION_LEAD + 0.6,
            "an edited caret must not be led: pet reached {max_col}"
        );
    }

    /// A pause decays v̂ on VEL_TAU, so the lead eases back and the pet ends
    /// up settled at the base station rather than parked out ahead.
    #[test]
    fn a_pause_eases_the_lead_back_to_the_base_station() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 5);
        let (t, _) = type_run(&mut pet, t, 3, 7, 30, 0.05);
        let (_, f) = idle(&mut pet, t, (3, 37), 2.5);
        assert!(
            (f.col - (37.0 + STATION_LEAD)).abs() <= 1.0,
            "the lead must ease back on a pause: pet at {} for caret 37",
            f.col
        );
        assert!(
            f.action.settled(),
            "and the pet settles, got {:?}",
            f.action
        );
    }

    /// The whole point of the schedule: at 20 keys/s the gain rises and the
    /// lead cancels the lag, so the pet is never BEHIND the caret it leads.
    #[test]
    fn the_gallop_lag_is_cancelled_at_twenty_keys_per_second() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 3);
        let (_, f) = type_run(&mut pet, t, 3, 5, 60, 0.05);
        assert!(
            f.col >= 65.0 + 0.3,
            "at 20 keys/s the schedule must keep the pet ahead: pet at {} \
             for caret 65",
            f.col
        );
    }

    /// FLIGHT AIM: a pounce launched mid-rhythm lands at the PREDICTED
    /// station, not the stale one — a stale aim puts the paws down behind a
    /// caret that kept moving through the whole flight.
    #[test]
    fn a_pounce_mid_rhythm_lands_at_the_predicted_station() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        // Build the rhythm, then jump a word, then KEEP TYPING.
        let (mut t, _) = type_run(&mut pet, t, 4, 12, 20, 0.05);
        let mut caret = 40u16; // the +8 word jump off caret 32
        let mut landed: Option<(f32, u16)> = None;
        let mut next_key = t + Duration::from_millis(50);
        for _ in 0..120 {
            t += Duration::from_millis(16);
            if t >= next_key {
                next_key += Duration::from_millis(50);
                caret = (caret + 1).min(95);
            }
            let f = pet.tick(sense(t, Some((4, caret))));
            if f.action == PetAction::Land && landed.is_none() {
                landed = Some((f.col, caret));
            }
        }
        let (col, caret_at_land) = landed.expect("the word jump must pounce");
        assert!(
            col > f32::from(caret_at_land),
            "a predicted aim lands AHEAD of the still-moving caret: landed \
             at {col} while the caret was at {caret_at_land}"
        );
    }

    /// The wall is hysteretic: typing one column at the crossing must not
    /// flip-flop the cat over the cursor with every keystroke.
    #[test]
    fn wall_hysteresis_does_not_flip_flop_at_the_crossing() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 92);
        // The awake run ends at caret 94, which crosses the pet to the LEFT.
        // Now alternate 93 ↔ 94 slowly (outside the frolic window).
        let mut left_frames = 0u32;
        let mut right_frames = 0u32;
        for k in 0..6u16 {
            let c = if k % 2 == 0 { 93 } else { 94 };
            for _ in 0..90 {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((5, c))));
                if f.col < f32::from(c) {
                    left_frames += 1;
                } else {
                    right_frames += 1;
                }
            }
        }
        assert!(left_frames > 400, "the pet must be parked wall-side");
        assert!(
            right_frames == 0,
            "and STAY there through the alternation: {right_frames} frames \
             back on the right of the caret"
        );
    }

    /// A station flip is a transit over the caret column: the pet HOPS the
    /// cursor (one small arc) rather than walking through it.
    #[test]
    fn the_wall_crossing_transits_with_a_small_arc() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 86);
        // awake leaves the caret at 88 (station ~89). Two modest moves: the
        // second hangs the led station off the margin and flips the side.
        let mut airborne = false;
        let mut f = pet.tick(sense(t, Some((5, 91))));
        for _ in 0..90 {
            t += Duration::from_millis(16);
            f = pet.tick(sense(t, Some((5, 94))));
            if f.action == PetAction::Leap && f.lift > 0.0 {
                airborne = true;
            }
        }
        assert!(airborne, "the side flip must arc over the caret");
        assert!(
            f.col < 94.0 - 5.0,
            "and end parked clear on the caret's left, got {}",
            f.col
        );
    }

    // ── the screen-crossing jump ────────────────────────────────────────

    /// Poses seen across one drive, for choreography assertions.
    fn drive_and_collect(
        pet: &mut PetBrain,
        mut t: Instant,
        caret: (u16, u16),
        ticks: u32,
    ) -> (Vec<PetGlyphId>, Vec<PetFrame>) {
        let mut poses = Vec::new();
        let mut frames = Vec::new();
        for _ in 0..ticks {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some(caret)));
            poses.push(f.pose);
            frames.push(f);
        }
        (poses, frames)
    }

    /// Trigger (a): one screen-crossing move plays the WHOLE show — notice,
    /// butt-wiggle gather, rise/peak/descend flight, heavy landing — and the
    /// heels kick dust: free-floating, rotated, fractional motes.
    #[test]
    fn a_screen_crossing_move_plays_the_whole_choreography() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        // caret 12 → 60: 48 columns, past max(24, 0.4·100).
        let (poses, frames) = drive_and_collect(&mut pet, t, (4, 60), 200);
        assert!(
            poses.contains(&PetGlyphId::PetCrouchWiggle) && poses.contains(&PetGlyphId::PetCrouch),
            "the gather must alternate crouch and wiggle"
        );
        for id in [
            PetGlyphId::PetPerk,
            PetGlyphId::PetLeapRise,
            PetGlyphId::PetLeap,
            PetGlyphId::PetLeapDescend,
            PetGlyphId::PetLand,
        ] {
            assert!(poses.contains(&id), "choreography must include {id:?}");
        }
        // Dust at the heels: on the frame lane, fractional and rotated.
        let dust: Vec<_> = frames
            .iter()
            .flat_map(|f| f.motes.iter().flatten())
            .filter(|m| m.kind == PetMoteKind::Dust)
            .collect();
        assert!(!dust.is_empty(), "a heavy landing kicks dust");
        for m in &dust {
            assert!(
                m.col.fract() != 0.0 || m.row.fract() != 0.0,
                "motes are never grid-aligned: ({}, {})",
                m.col,
                m.row
            );
            assert!(m.rot != 0.0, "and always rotated");
        }
    }

    /// A mere word jump (below both thresholds, no joy) keeps the plain
    /// pounce: no wiggle, and the short flight it always had.
    #[test]
    fn a_word_jump_stays_a_pounce_below_the_thresholds() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (poses, frames) = drive_and_collect(&mut pet, t, (4, 32), 160);
        assert!(
            !poses.contains(&PetGlyphId::PetCrouchWiggle),
            "a 20-column jump is transport, not theatre"
        );
        let mut run = 0u32;
        let mut max_run = 0u32;
        for f in &frames {
            if f.action == PetAction::Leap {
                run += 1;
                max_run = max_run.max(run);
            } else {
                run = 0;
            }
        }
        assert!(
            max_run as f32 * 0.016 <= FLIGHT_MAX + 0.05,
            "and its flight keeps the pounce clamp"
        );
    }

    /// Trigger (b): a standing gap of half the grid, opened while a hold
    /// (here the wake-up stretch) had the pet frozen, still gets the show
    /// once the hold resolves.
    #[test]
    fn a_half_grid_standing_gap_jumps_once_the_hold_resolves() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let _ = pet.tick(sense(start, Some((3, 5))));
        // The caret lands 65 columns away while the pet is mid-stretch.
        let (poses, frames) =
            drive_and_collect(&mut pet, start + Duration::from_millis(16), (3, 70), 260);
        assert!(
            poses.contains(&PetGlyphId::PetCrouchWiggle),
            "a half-grid gap earns the gather"
        );
        let last = frames.last().expect("frames");
        assert!(
            (last.col - 71.0).abs() <= SKID_CELLS + 1.0,
            "and the bounds arrive at the station, got {}",
            last.col
        );
    }

    /// Trigger (c): the JOY variant — a settled cat carrying ≥ 0.85
    /// contentment answers a word jump with the show a cold cat answers
    /// with a plain pounce.
    #[test]
    fn joy_upgrades_a_word_jump_into_the_show() {
        let start = Instant::now();
        // The joyful cat: a long fast run earns full contentment.
        let mut joyful = PetBrain::default();
        let t = awake(&mut joyful, start, 3, 2);
        let (t, _) = type_run(&mut joyful, t, 3, 4, 60, 0.05);
        // Long enough for the keep-ahead lead to ease back and the pet to
        // actually SETTLE (the joy door opens from a settled action only).
        let (t, _) = idle(&mut joyful, t, (3, 64), 2.0);
        assert!(
            joyful.content() >= JOY_CONTENT,
            "fixture: joy must be earned"
        );
        let (poses, _) = drive_and_collect(&mut joyful, t, (3, 72), 200);
        assert!(
            poses.contains(&PetGlyphId::PetCrouchWiggle),
            "a joyful settled cat gives the +8 jump the whole show"
        );

        // The cold cat: same jump, no contentment, plain pounce.
        let mut cold = PetBrain::default();
        let t = awake(&mut cold, start, 3, 62);
        let (poses, _) = drive_and_collect(&mut cold, t, (3, 72), 200);
        assert!(
            !poses.contains(&PetGlyphId::PetCrouchWiggle),
            "a cold sit answers the same jump with a pounce"
        );
    }

    /// The variance flourish: a span past 60% of the grid splits into TWO
    /// bounds with a touch-land between — deterministically, from the span.
    #[test]
    fn a_screen_wide_span_splits_into_two_bounds() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 5);
        let (_, frames) = drive_and_collect(&mut pet, t, (4, 78), 260);
        let mut episodes = 0;
        let mut airborne_was = false;
        for f in &frames {
            let airborne = f.action == PetAction::Leap;
            if airborne && !airborne_was {
                episodes += 1;
            }
            airborne_was = airborne;
        }
        assert_eq!(
            episodes, 2,
            "60%+ of the grid is two bounds, got {episodes}"
        );
    }

    /// The apex clamp: a bound from row 1 keeps the sprite's TOP inside the
    /// viewport — the arc gives up height rather than clipping (509c9f01).
    #[test]
    fn the_bound_apex_never_clips_the_viewport_top() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 1, 10);
        let (_, frames) = drive_and_collect(&mut pet, t, (1, 60), 220);
        for f in &frames {
            if f.action == PetAction::Leap {
                let top = f.row + 1.0 - ART_ROWS - f.lift;
                assert!(
                    top >= -0.05,
                    "the sprite top left the viewport: row {} lift {}",
                    f.row,
                    f.lift
                );
            }
        }
    }

    /// The u-schedule reaches the OLD flights too: a plain pounce and a
    /// plain Enter hop both get an ascent, a peak, and a landing approach.
    #[test]
    fn rise_and_descend_frames_schedule_on_pounces_and_hops_too() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (poses, _) = drive_and_collect(&mut pet, t, (4, 32), 160);
        for id in [
            PetGlyphId::PetLeapRise,
            PetGlyphId::PetLeap,
            PetGlyphId::PetLeapDescend,
        ] {
            assert!(poses.contains(&id), "the pounce must schedule {id:?}");
        }
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 30);
        let (poses, _) = drive_and_collect(&mut pet, t, (5, 0), 160);
        for id in [
            PetGlyphId::PetLeapRise,
            PetGlyphId::PetLeap,
            PetGlyphId::PetLeapDescend,
        ] {
            assert!(poses.contains(&id), "the hop must schedule {id:?}");
        }
    }

    /// The landing squash carries the WEIGHT of the crossing: a short joyful
    /// bound lands lighter than a 45-column one.
    #[test]
    fn the_landing_squash_scales_with_the_span() {
        let start = Instant::now();
        let min_land_scale = |caret_from: u16, caret_to: u16, joy: bool| -> f32 {
            let mut pet = PetBrain::default();
            let mut t = awake(&mut pet, start, 3, caret_from);
            if joy {
                let (t2, _) = type_run(&mut pet, t, 3, caret_from + 2, 60, 0.05);
                let (t2, _) = idle(&mut pet, t2, (3, caret_from + 62), 2.0);
                assert!(pet.content() >= JOY_CONTENT, "fixture: joy must be earned");
                t = t2;
            }
            let (_, frames) = drive_and_collect(&mut pet, t, (3, caret_to), 240);
            frames
                .iter()
                .filter(|f| f.action == PetAction::Land)
                .map(|f| f.scale_y)
                .fold(1.0f32, f32::min)
        };
        // Joyful +8 jump: span ~8. Cold +48 jump: span ~48 (squash saturated).
        let light = min_land_scale(2, 72, true);
        let heavy = min_land_scale(10, 60, false);
        assert!(
            light > heavy + 0.01,
            "a short bound lands lighter: light {light} vs heavy {heavy}"
        );
    }

    // ── the athletic vocabulary (DESIGN-kitty-motion §3, rungs 7-10) ────

    /// §8.7: the coil and its release land as ONE change — across the
    /// crouch→launch boundary no frame steps the body's volume
    /// (`scale_x · scale_y`) by more than 2%. Both real gathers are driven:
    /// the pounce's scaled coil and the big jump's butt-wiggle. The LANDING
    /// side is deliberately not asserted — its damped ring
    /// (`exp(−3u)·cos(2π·1.35u)`) is authored to oscillate, and flagging it
    /// would flag shipped behavior.
    #[test]
    fn the_coil_releases_without_a_volume_pop() {
        let no_pop = |caret_to: u16, label: &str| {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let mut t = awake(&mut pet, start, 4, 10);
            let mut vols: Vec<f32> = Vec::new();
            for _ in 0..300 {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((4, caret_to))));
                match f.action {
                    PetAction::Crouch | PetAction::Leap => {
                        vols.push(f.scale_x * f.scale_y);
                    }
                    PetAction::Land if !vols.is_empty() => break,
                    _ => {}
                }
            }
            assert!(vols.len() > 3, "{label}: fixture must gather and fly");
            for w in vols.windows(2) {
                assert!(
                    (w[1] - w[0]).abs() <= 0.02,
                    "{label}: a volume pop at the coil: {} -> {}",
                    w[0],
                    w[1]
                );
            }
        };
        // An 18-column jump pounces; a 48-column jump plays the big show.
        no_pop(30, "the pounce coil");
        no_pop(60, "the big gather");
    }

    /// §8.8: the landing steps off on a CONTACT frame — after `land_t` the
    /// gait phase sits within ε of an integer, and it got there by EASING
    /// (`enter_settled`'s own law), never by a snap.
    #[test]
    fn the_landing_steps_off_on_a_contact_frame() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        let mut landed = false;
        let mut max_step = 0.0f32;
        for _ in 0..400 {
            t += Duration::from_millis(16);
            let before = pet.stride;
            let f = pet.tick(sense(t, Some((4, 30))));
            if f.action == PetAction::Land {
                if !landed {
                    landed = true;
                    // Force a mid-swing phase the moment the paws touch, so
                    // the recovery has real work to show.
                    pet.stride = pet.stride.round() + 0.37;
                } else {
                    max_step = max_step.max((pet.stride - before).abs());
                }
            } else if landed {
                break;
            }
        }
        assert!(landed, "fixture: the word jump must pounce and land");
        let planted = (pet.stride - pet.stride.round()).abs();
        assert!(
            planted < 0.05,
            "the gait must step off on a contact frame, {planted} off a plant"
        );
        assert!(
            max_step < 0.15,
            "and reach it by EASING, not a snap: worst tick moved {max_step}"
        );
    }

    /// §8.9, the guard over every athletics change: the same typing produces
    /// the SAME cat, byte for byte, wherever the wall clock started — the
    /// coil, the release, the asymmetric stretch, the recovery ease, the
    /// time schedule and the hang are all pure arithmetic on the brain's own
    /// clocks (`flick_beats_are_quiet_phased`'s replay law, stretched over
    /// the whole athletic vocabulary: a pounce, an Enter hop, a two-bound
    /// crossing and every landing between).
    #[test]
    fn the_same_typing_still_produces_the_same_cat() {
        let run = |start: Instant| -> Vec<u64> {
            let mut pet = PetBrain::default();
            let mut t = awake(&mut pet, start, 4, 10);
            let mut fps = Vec::new();
            for (row, col, ticks) in [(4u16, 30u16, 90u32), (5, 2, 70), (5, 80, 240), (5, 82, 60)] {
                for _ in 0..ticks {
                    t += Duration::from_millis(16);
                    fps.push(pet.tick(sense(t, Some((row, col)))).fp());
                }
            }
            fps
        };
        let a = run(Instant::now());
        let b = run(Instant::now() + Duration::from_millis(7_919));
        assert_eq!(a, b, "the same typing must produce the same cat");
        assert!(
            a.windows(2).any(|w| w[0] != w[1]),
            "and the script actually animates"
        );
    }

    /// Rung 10: a big bound HANGS — the share of the flight spent at or
    /// above `0.9·arc` grows from the parabola's 0.3162 to ~0.4236 (§3's
    /// 20,001-sample figure), on the pure shape and on real glass.
    #[test]
    fn a_big_bound_hangs_at_its_apex() {
        let frac = |big: bool| -> f32 {
            let n = 20_001u32;
            let mut hung = 0u32;
            for i in 0..n {
                let u = i as f32 / (n - 1) as f32;
                if PetBrain::flight_lift(1.0, u, big) >= 0.9 {
                    hung += 1;
                }
            }
            hung as f32 / n as f32
        };
        let parabola = frac(false);
        let hang = frac(true);
        assert!(
            (parabola - 0.3162).abs() < 0.01,
            "the parabola's apex window drifted: {parabola}"
        );
        assert!(
            (hang - 0.4236).abs() < 0.02,
            "the hang's apex window must be ~0.4236, got {hang}"
        );
        // And on glass: a real single bound spends roughly that share of
        // its airborne frames up high (loose — frames quantize the window).
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        let mut lifts: Vec<f32> = Vec::new();
        for _ in 0..300 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 60))));
            if f.action == PetAction::Leap {
                lifts.push(f.lift);
            }
        }
        let apex = lifts.iter().copied().fold(0.0f32, f32::max);
        assert!(
            apex > 0.5,
            "fixture: the bound must actually fly, apex {apex}"
        );
        let high = lifts.iter().filter(|l| **l >= 0.9 * apex).count() as f32;
        let share = high / lifts.len() as f32;
        assert!(
            (0.30..=0.55).contains(&share),
            "a big bound hangs ~0.42 of its flight up high, measured {share}"
        );
    }

    /// Rung 10's gate, the other face: a NON-big flight keeps the parabola
    /// `4u(1−u)` byte-for-byte — `fp` is byte-compared, and a flattened
    /// pounce is not a hang but a slide across the reading zone.
    #[test]
    fn a_small_hop_keeps_its_parabola() {
        for i in 0..=10_000u32 {
            let u = i as f32 / 10_000.0;
            let arc = 0.87f32;
            assert_eq!(
                PetBrain::flight_lift(arc, u, false).to_bits(),
                (arc * 4.0 * u * (1.0 - u)).to_bits(),
                "the small flight's parabola drifted at u = {u}"
            );
        }
        // And on glass: every airborne frame of an Enter hop carries the
        // exact parabola of its own live arc.
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 30);
        let mut airborne = false;
        for _ in 0..200 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((5, 2))));
            if f.action == PetAction::Leap
                && let Some(fl) = pet.flight
            {
                assert!(!fl.big, "fixture: an Enter hop is not a bound");
                let u = (fl.t / fl.dur).clamp(0.0, 1.0);
                assert_eq!(
                    f.lift.to_bits(),
                    (fl.arc * 4.0 * u * (1.0 - u)).to_bits(),
                    "the hop's lift left its parabola at u = {u}"
                );
                airborne = true;
            }
        }
        assert!(airborne, "fixture: the hop must actually fly");
    }

    // ── the sleep z's and the purr tell ─────────────────────────────────

    /// LIGHT sleep drifts little z's: spawned on the quiet clock, capped at
    /// three alive, free-floating and rotated and scaling and fading — the
    /// full mote law, owner ask made physical.
    #[test]
    fn light_sleep_drifts_zees_capped_and_free_floating() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = idle(&mut pet, t, (4, 12), SLEEP_AFTER + 0.1);
        let mut seen = 0usize;
        let mut scales = std::collections::BTreeSet::new();
        let mut alphas = std::collections::BTreeSet::new();
        for _ in 0..400 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 12))));
            assert_eq!(f.action, PetAction::Sleep, "the fixture stays asleep");
            let zees: Vec<_> = f
                .motes
                .iter()
                .flatten()
                .filter(|m| matches!(m.kind, PetMoteKind::Zee))
                .collect();
            assert!(zees.len() <= ZEE_ALIVE_MAX, "never more than three z's");
            for z in &zees {
                seen += 1;
                assert!(
                    z.col.fract() != 0.0 || z.row.fract() != 0.0,
                    "z's are never grid-aligned: ({}, {})",
                    z.col,
                    z.row
                );
                assert!(z.rot != 0.0, "and always rotated");
                // (alpha is 0 on the birth tick — the fade-in ramp's foot.)
                scales.insert((z.scale * 1024.0) as i64);
                alphas.insert(z.alpha);
            }
        }
        assert!(seen > 0, "light sleep must actually dream");
        assert!(scales.len() > 8, "the z's scale as they drift");
        assert!(alphas.len() > 8, "and fade as they go");
    }

    /// [`ZEE_EVERY`] is pinned from BOTH sides, on the real tick loop.
    ///
    /// The owner asked for the sleeper's z's more often (it is the pose an
    /// idle terminal parks on, so it is the one they watch); the cadence has
    /// to answer that without becoming the swarm the [`ZEE_ALIVE_MAX`] cap
    /// exists to catch. So: the light-sleep spawn window must fit at least
    /// five births — at the old 1.6 s it fitted four, and four over ~7.8 s
    /// reads as intermittent rather than as breathing — while the lane never
    /// actually reaches the cap, which is what keeps it a drift. The birth
    /// rate is asserted under WCAG 2.3.1's 3 Hz general-flash bound too: this
    /// is the crate's photosensitivity budget (see `cursor_rainbow`'s
    /// twinkle), and a "more often" ask is exactly how one gets spent by
    /// accident.
    #[test]
    fn the_sleep_zees_drift_at_a_breathing_cadence() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = idle(&mut pet, t, (4, 12), SLEEP_AFTER + 0.02);
        // Slot-occupancy edges, so a birth is counted once however long the
        // mote then lives (the sprite array is indexed, and `flatten()` —
        // which the sibling test uses — would throw the identity away).
        let mut was_zee = [false; PET_MOTES_MAX];
        let mut births = 0usize;
        let mut peak_alive = 0usize;
        // The whole light-sleep window plus a margin: spawning is cut off at
        // `SLEEP_AFTER + BREATH_WINDOW - ZEE_LIFE` by the idle-to-zero law.
        let ticks = ((BREATH_WINDOW + 1.0) / 0.016) as u32;
        for _ in 0..ticks {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 12))));
            let mut alive = 0usize;
            for (slot, m) in f.motes.iter().enumerate() {
                let zee = matches!(m, Some(s) if s.kind == PetMoteKind::Zee);
                births += usize::from(zee && !was_zee[slot]);
                alive += usize::from(zee);
                was_zee[slot] = zee;
            }
            peak_alive = peak_alive.max(alive);
        }
        assert!(
            births >= 5,
            "the light-sleep window fitted only {births} z's — the tell reads \
             as intermittent, not as breathing"
        );
        assert!(
            peak_alive < ZEE_ALIVE_MAX,
            "{peak_alive} z's alive at once reaches the cap: that is a swarm, \
             not a drift"
        );
        let hz = 1.0 / ZEE_EVERY;
        assert!(
            hz < 3.0,
            "z births at {hz} Hz — over WCAG 2.3.1's 3 Hz general-flash bound"
        );
    }

    /// Drive a fresh brain to the exact state the orphan-mote report caught on
    /// glass: light sleep, with a `Zee` actually drifting off the head anchor.
    /// Returns the clock and that frame. Deterministic (this engine reads no wall
    /// clock and draws no randomness), so two brains driven this way are equal.
    fn sleeper_with_a_live_zee(start: Instant) -> (PetBrain, Instant) {
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = idle(&mut pet, t, (4, 12), SLEEP_AFTER + 0.1);
        for _ in 0..400 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 12))));
            if f.motes
                .iter()
                .flatten()
                .any(|m| m.kind == PetMoteKind::Zee && m.alpha > 0)
            {
                return (pet, t);
            }
        }
        panic!("fixture: the sleeper never dreamed a visible z");
    }

    /// NOTHING THE PET OWNS MAY OUTLIVE ITS SWITCH.
    ///
    /// Withholding the caret is a graceful EXIT, not a switch: `tick`'s no-caret
    /// arm ramps the body down over `FADE_OUT` and deliberately leaves the mote
    /// lane running ("until the lanes empty, the slow arm keeps running their
    /// clocks every frame"). That is right for a pet that lost sight of the cursor
    /// and wrong for one whose OWNER went away: a fade and a drift are requests for
    /// MORE frames, made at the instant the user said stop. Both halves of that
    /// leftover are pinned below — the live mote, and the `needs_frames()` claim on
    /// the host's 60 fps effects lane that a mote (or a half-faded body) keeps
    /// alive.
    ///
    /// So this pins the two levers against one another on one fixture: the
    /// caret-withheld CONTROL still carries the z and still asks for the lane, and
    /// [`PetBrain::retire_unowned`] does neither.
    #[test]
    fn retire_unowned_kills_a_live_mote_where_withholding_the_caret_does_not() {
        let start = Instant::now();

        // CONTROL — the only lever hosts had before: stop feeding a caret.
        let (mut control, mut ct) = sleeper_with_a_live_zee(start);
        ct += Duration::from_millis(16);
        let drifting = control.tick(sense(ct, None));
        assert!(
            drifting.motes.iter().flatten().any(|m| m.alpha > 0),
            "fixture: withholding the caret leaves the mote lane drifting — that \
             is the behaviour this switch exists to overrule"
        );
        assert!(
            control.needs_frames(),
            "…and it is still asking for the 60 fps lane it is about to lose"
        );

        // THE SWITCH, on an identical brain.
        let (mut pet, mut t) = sleeper_with_a_live_zee(start);
        assert!(pet.is_active(), "fixture: a visible pet");
        assert!(pet.needs_frames(), "fixture: a drifting z owns the lane");
        pet.retire_unowned();
        assert!(!pet.is_active(), "a retired pet paints nothing");
        assert!(!pet.needs_frames(), "…and owes the scheduler no frames");

        // The next frame a host would emit from carries nothing at all — no fade
        // to sit through, no mote left in the lane.
        t += Duration::from_millis(16);
        let after = pet.tick(sense(t, None));
        assert_eq!(after.alpha, 0, "the body is gone at once, not fading");
        assert!(
            after.motes.iter().all(Option::is_none),
            "the mote lane must be empty, found {:?}",
            after
                .motes
                .iter()
                .flatten()
                .map(|m| m.kind)
                .collect::<Vec<_>>()
        );
        assert!(
            after.departures.iter().all(Option::is_none),
            "and no departing body either"
        );

        // LEVEL, NOT EDGE: hosts call it every frame the switch is off, so an
        // already-retired brain must be a no-op that still owes nothing.
        pet.retire_unowned();
        assert!(!pet.needs_frames(), "retirement is idempotent");
        t += Duration::from_millis(16);
        let still = pet.tick(sense(t, None));
        assert_eq!(still.alpha, 0);
        assert!(still.motes.iter().all(Option::is_none));
    }

    /// The purr tell: a settled, contented cat floats ♪ and ♥ motes on the
    /// same lane — and its chest swell is the deepened 5%.
    #[test]
    fn a_purring_cat_floats_notes_and_hearts() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 2);
        let (t, _) = type_run(&mut pet, t, 3, 4, 40, 0.05);
        let (mut t, _) = idle(&mut pet, t, (3, 44), SIT_AFTER + 0.6);
        let mut kinds = std::collections::BTreeSet::new();
        let mut min_scale_y = 1.0f32;
        let mut max_scale_y = 1.0f32;
        for _ in 0..320 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((3, 44))));
            if f.action == PetAction::Purr {
                min_scale_y = min_scale_y.min(f.scale_y);
                max_scale_y = max_scale_y.max(f.scale_y);
            }
            for m in f.motes.iter().flatten() {
                kinds.insert(match m.kind {
                    PetMoteKind::Note => 1u8,
                    PetMoteKind::Heart => 2,
                    _ => 0,
                });
                assert!(
                    m.col.fract() != 0.0 || m.row.fract() != 0.0,
                    "purr motes are never grid-aligned"
                );
                assert!(m.rot != 0.0, "and always rotated");
            }
        }
        assert!(kinds.contains(&1), "the purr floats a note");
        assert!(kinds.contains(&2), "and a heart");
        assert!(
            max_scale_y - min_scale_y >= PURR_DEPTH,
            "the chest swell must reach the deepened depth: {} .. {}",
            min_scale_y,
            max_scale_y
        );
    }

    /// Waking pops exactly ONE startled z ahead of the stretch.
    #[test]
    fn waking_pops_one_startled_z_before_the_stretch() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, f) = idle(&mut pet, start, (4, 20), 0.5);
        assert_eq!(f.action, PetAction::Sleep, "a fresh pet naps");
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 21))));
        assert_eq!(f.action, PetAction::Waking);
        assert_eq!(f.pose, PetGlyphId::PetStretch);
        let pops = f
            .motes
            .iter()
            .flatten()
            .filter(|m| m.kind == PetMoteKind::ZeePop)
            .count();
        assert_eq!(pops, 1, "one startled z, not a stream");
    }

    /// Reduced motion spawns NOTHING on the mote lane — ever.
    #[test]
    fn reduced_motion_never_spawns_motes() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        for _ in 0..600 {
            t += Duration::from_millis(64);
            let mut s = sense(t, Some((4, 20)));
            s.reduced_motion = true;
            let f = pet.tick(s);
            assert!(
                f.motes.iter().all(Option::is_none),
                "reduced motion must never particle"
            );
        }
    }

    // ── the settled gaze and the loaf ───────────────────────────────────

    /// The settle-turn: half a second after arriving the pet turns to face
    /// the caret it just chased past — before that, it still faces its
    /// travel (a stopped cat keeps looking where it was).
    #[test]
    fn the_settle_turn_faces_the_caret() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (t, _) = type_run(&mut pet, t, 4, 12, 3, 0.06);
        // The pet chased RIGHT to its station one cell past the caret.
        let (t, f) = idle(&mut pet, t, (4, 15), 0.30);
        assert!(!f.facing_left, "before the turn it faces its travel");
        let (_, f) = idle(&mut pet, t, (4, 15), 0.80);
        assert!(
            f.facing_left,
            "half a second after arriving it faces the caret behind it"
        );
    }

    /// Eye contact at rest: a settle with the caret within 3 cells sits
    /// FACE-ON.
    #[test]
    fn a_close_gap_settle_makes_eye_contact() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (t, _) = type_run(&mut pet, t, 4, 12, 3, 0.06);
        let (_, f) = idle(&mut pet, t, (4, 15), SIT_AFTER + 0.3);
        assert_eq!(f.action, PetAction::Sit);
        assert_eq!(
            f.pose,
            PetGlyphId::PetSitFront,
            "one cell away is eye-contact range"
        );
    }

    /// The away-facing settle: parked wall-side, body facing left, caret
    /// BEHIND on the right — the pet keeps its body and peeks over the
    /// shoulder instead of twitch-flipping.
    #[test]
    fn a_far_caret_behind_earns_the_over_shoulder_peek() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 5, 93);
        // The awake run crossed the pet to the caret's LEFT (caret 95).
        assert!(pet.col < 90.0, "fixture: parked wall-side, col {}", pet.col);
        let (_, f) = idle(&mut pet, t, (5, 95), 0.5);
        assert_eq!(f.action, PetAction::Sit);
        assert!(f.facing_left, "the body keeps its travel facing");
        assert_eq!(
            f.pose,
            PetGlyphId::PetPeekShoulder,
            "a far caret behind the facing earns the peek"
        );
    }

    /// LONG dwell: past ~2× SIT_AFTER the sit melts into the loaf, and
    /// sleep still follows the loaf on the same quiet ladder.
    #[test]
    fn long_dwell_loafs_and_sleep_still_follows() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (t, f) = idle(&mut pet, t, (4, 12), LOAF_AFTER + 0.4);
        assert_eq!(f.action, PetAction::Loaf, "long dwell loafs");
        assert_eq!(f.pose, PetGlyphId::PetLoaf);
        let (_, f) = idle(&mut pet, t, (4, 12), SLEEP_AFTER);
        assert_eq!(f.action, PetAction::Sleep, "and sleep still follows");
    }

    /// While hidden the sync applies immediately — there is nothing on screen
    /// to protect (the same arm `kitty_cursor::set_look` documents).
    #[test]
    fn a_hidden_pet_takes_a_look_sync_immediately() {
        let mut pet = PetBrain::default();
        assert!(!pet.is_active(), "a fresh pet is hidden");
        assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (9, 4),
            "hidden: no appearance to protect, the swap is free"
        );
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (9, 4),
            "and the agreeing re-sync is a no-op"
        );
    }

    // ── wave 1: the bell startle ────────────────────────────────────────

    #[test]
    fn a_bell_wakes_the_sleeper() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        // A fresh pet materializes asleep; let the fade complete.
        let (mut t, f) = idle(&mut pet, start, (4, 10), 1.0);
        assert_eq!(
            f.action,
            PetAction::Sleep,
            "fixture: an untouched window naps"
        );
        pet.note_bell(t);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 10))));
        assert_eq!(
            f.action,
            PetAction::Waking,
            "the bell takes the EXISTING wake path"
        );
        assert_eq!(f.pose, PetGlyphId::PetStretch, "stretch and all");
        assert!(
            f.motes
                .iter()
                .flatten()
                .any(|m| m.kind == PetMoteKind::ZeePop),
            "with the startled z popped ahead of it"
        );
    }

    #[test]
    fn a_bell_flinches_the_settled_pet() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 10);
        let (mut t, _) = idle(&mut pet, t, (3, 12), 0.5);
        pet.note_bell(t);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((3, 12))));
        assert_eq!(
            f.action,
            PetAction::Perk,
            "a bell is a noise, not a held delete: the FLINCH tier, never the bottle"
        );
        assert!(
            f.scale_y < 1.0 && f.scale_x > 1.0,
            "the duck must be visible: y {} x {}",
            f.scale_y,
            f.scale_x
        );
        assert!(
            f.facing_left,
            "and the fright faces the caret (left of the station)"
        );
    }

    #[test]
    fn a_bell_during_flight_expires_unconsumed() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        // A 20-column jump: notice (0.30) → crouch → flight → landing is
        // ~1.1 s of holds and air, past BELL_LATCH_TTL by construction.
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((4, 30))));
        pet.note_bell(t);
        let mut landed = false;
        let mut acted_after_ground = false;
        for _ in 0..160 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 30))));
            if f.action == PetAction::Land {
                landed = true;
            }
            // Once on the ground, a consumed bell would show as the flinch
            // (Perk + duck) or a wake — neither may fire off a dead latch.
            if landed && matches!(f.action, PetAction::Perk | PetAction::Waking) {
                acted_after_ground = true;
            }
        }
        assert!(landed, "fixture: the jump must land inside the window");
        assert!(
            pet.pending_bell.is_none(),
            "the latch must expire per its TTL"
        );
        assert!(
            !acted_after_ground,
            "a bell from before the leap must not fire after it"
        );
    }

    #[test]
    fn a_bell_under_reduced_motion_is_bookkeeping_only() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        let reduced = |t: Instant| {
            let mut s = sense(t, Some((3, 10)));
            s.reduced_motion = true;
            s
        };
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let _ = pet.tick(reduced(t));
        }
        pet.note_bell(t);
        for _ in 0..10 {
            t += Duration::from_millis(16);
            let f = pet.tick(reduced(t));
            assert!(
                matches!(f.action, PetAction::Sit | PetAction::Sleep),
                "no action change under reduced motion, got {:?}",
                f.action
            );
            assert_eq!(f.scale_x, 1.0);
            assert_eq!(f.scale_y, 1.0);
            assert!(f.motes.iter().all(Option::is_none), "and no motes");
        }
        assert!(
            pet.pending_bell.is_none(),
            "the latch clears as bookkeeping"
        );
    }

    // ── wave 1: exit-code empathy ───────────────────────────────────────

    #[test]
    fn a_long_success_cheers_and_warms() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 10);
        let (mut t, _) = idle(&mut pet, t, (3, 12), 0.3);
        let c0 = pet.content();
        pet.note_command_done(t, false, Some(2500));
        assert!(
            pet.content() > c0 + CHEER_CONTENT - 0.01,
            "the ledger warms at note time"
        );
        let (mut saw_perk, mut saw_frolic, mut saw_note) = (false, false, false);
        for _ in 0..120 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((3, 12))));
            match f.action {
                PetAction::Perk => saw_perk = true,
                PetAction::Frolic => {
                    saw_frolic = true;
                    if f.motes
                        .iter()
                        .flatten()
                        .any(|m| m.kind == PetMoteKind::Note)
                    {
                        saw_note = true;
                    }
                }
                _ => {}
            }
        }
        assert!(saw_perk, "the notice leads the celebration");
        assert!(saw_frolic, "then the frolic");
        assert!(saw_note, "with the one floating note");
    }

    #[test]
    fn a_failure_droops_and_cools() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 2);
        // Earn real warmth first, so the cooling is measurable.
        let (t, _) = type_run(&mut pet, t, 3, 4, 30, 0.05);
        let (mut t, _) = idle(&mut pet, t, (3, 34), 0.6);
        let c0 = pet.content();
        assert!(c0 > 0.3, "fixture: the run must have warmed the cat ({c0})");
        pet.note_command_done(t, true, Some(120_000));
        assert!(
            pet.content() <= c0 - SULK_CONTENT + 1e-5,
            "a failure cools harder than a success warms"
        );
        let mut saw_droop = false;
        for _ in 0..160 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((3, 34))));
            if f.action == PetAction::Droop {
                saw_droop = true;
                assert_eq!(
                    f.pose,
                    PetGlyphId::PetLoaf,
                    "the droop borrows the loaf until the flat-ears frame lands"
                );
                assert!(f.motes.iter().all(Option::is_none), "grief floats NOTHING");
            }
        }
        assert!(saw_droop, "a failure must droop");
    }

    #[test]
    fn a_fast_command_only_nudges() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 10);
        let (mut t, _) = idle(&mut pet, t, (3, 12), 0.4);
        let c0 = pet.content();
        pet.note_command_done(t, false, Some(300));
        assert!(pet.content() > c0, "the ledger still moves");
        for _ in 0..80 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((3, 12))));
            assert!(
                !matches!(
                    f.action,
                    PetAction::Perk | PetAction::Frolic | PetAction::Droop
                ),
                "a bare `ls` earns no choreography, got {:?}",
                f.action
            );
        }
    }

    #[test]
    fn a_cheer_survives_a_hold() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (t, _) = type_run(&mut pet, start, 2, 10, 8, 0.06);
        let (mut t, _) = idle(&mut pet, t, (2, 20), 0.5);
        t += Duration::from_millis(16);
        // A real retreat bottles the pet — the hold the cheer must outlive.
        let f = pet.tick(sense(t, Some((2, 15))));
        assert_eq!(f.action, PetAction::Startle, "fixture: the hold is live");
        pet.note_command_done(t, false, Some(5000));
        let mut saw_frolic = false;
        for _ in 0..200 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((2, 15))));
            if f.action == PetAction::Frolic {
                saw_frolic = true;
            }
        }
        assert!(
            saw_frolic,
            "the cheer latch must outlive the startle hold and the walk home"
        );
    }

    #[test]
    fn exit_bump_respects_the_cold_purr_law() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 40);
        let (t, _) = idle(&mut pet, t, (3, 42), 0.4);
        assert!(pet.content() < 0.1, "fixture: a cold cat");
        pet.note_command_done(t, false, Some(10_000));
        // Let the whole cheer play, then settle well past the sit.
        let (_, f) = idle(
            &mut pet,
            t,
            (3, 42),
            SIT_AFTER + PERK_HOLD + FROLIC_HOLD + 0.6,
        );
        assert_ne!(
            f.action,
            PetAction::Purr,
            "0.18 < PURR_GATE: one green build cannot buy a purr from a cold cat"
        );
        assert!(pet.content() < PURR_GATE);
    }

    #[test]
    fn reduced_motion_exit_is_bookkeeping() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        let reduced = |t: Instant| {
            let mut s = sense(t, Some((3, 10)));
            s.reduced_motion = true;
            s
        };
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let _ = pet.tick(reduced(t));
        }
        let c0 = pet.content();
        pet.note_command_done(t, false, Some(240_000));
        let warmed = pet.content();
        assert!(warmed > c0, "the ledger still warms");
        pet.note_command_done(t, true, Some(1000));
        assert!(pet.content() < warmed, "and still cools");
        for _ in 0..60 {
            t += Duration::from_millis(16);
            let f = pet.tick(reduced(t));
            assert!(
                matches!(f.action, PetAction::Sit | PetAction::Sleep),
                "no droop, no frolic under reduced motion, got {:?}",
                f.action
            );
            assert!(f.motes.iter().all(Option::is_none), "and no motes");
        }
        assert!(
            pet.pending_cheer.is_none() && !pet.pending_sulk,
            "the latches clear as bookkeeping"
        );
    }

    // ── wave 1: petting ─────────────────────────────────────────────────

    #[test]
    fn a_pet_on_the_head_makes_a_heart() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = idle(&mut pet, t, (4, 12), 0.4);
        let c0 = pet.content();
        pet.note_petted(t);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 12))));
        assert_eq!(
            f.action,
            PetAction::Purr,
            "affection is answered even by a cold cat: the purr-flavored hold"
        );
        assert!(
            f.motes
                .iter()
                .flatten()
                .any(|m| m.kind == PetMoteKind::Heart),
            "with a heart"
        );
        assert!(pet.content() > c0, "and warmth");
        assert!(pet.pet_hold_t > 0.0, "held, not earned — yet");
    }

    #[test]
    fn petting_enough_earns_a_purr() {
        let start = Instant::now();
        // ONE pet cannot buy a purr…
        let mut lone = PetBrain::default();
        let t = awake(&mut lone, start, 4, 10);
        let (t, _) = idle(&mut lone, t, (4, 12), SIT_AFTER + 0.4);
        lone.note_petted(t);
        let (_, f) = idle(&mut lone, t, (4, 12), PET_HOLD + 0.5);
        assert_eq!(lone.pet_hold_t, 0.0, "the hold has fully played out");
        assert_ne!(
            f.action,
            PetAction::Purr,
            "one pet cannot buy a sustained purr (0.08 < PURR_GATE)"
        );
        // …four inside the TTL can — the point.
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = idle(&mut pet, t, (4, 12), SIT_AFTER + 0.4);
        pet.note_petted(t);
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((4, 12)))); // consumes the first pet
        pet.note_petted(t);
        pet.note_petted(t);
        pet.note_petted(t); // three more queue through the hold
        let (t, _) = idle(&mut pet, t, (4, 12), PET_HOLD + 0.2);
        let (_, f) = idle(&mut pet, t, (4, 12), PET_HOLD + 0.3);
        assert_eq!(pet.pet_hold_t, 0.0, "both holds have played out");
        assert_eq!(
            f.action,
            PetAction::Purr,
            "four pets bought the REAL purr off the settle ladder"
        );
        assert!(pet.content() >= PURR_GATE);
    }

    #[test]
    fn a_pet_during_flight_waits_for_the_ground() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((4, 30)))); // an 18-col jump begins
        pet.note_petted(t);
        let (mut landed, mut purr_mid_air, mut purr_after_land) = (false, false, false);
        for _ in 0..200 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 30))));
            if f.action == PetAction::Land {
                landed = true;
            }
            if f.action == PetAction::Purr {
                if landed {
                    purr_after_land = true;
                } else {
                    purr_mid_air = true;
                }
            }
        }
        assert!(landed, "fixture: the pounce must land");
        assert!(!purr_mid_air, "a pet mid-flight waits for the ground");
        assert!(
            purr_after_land,
            "and is honored on landing (inside its TTL)"
        );
    }

    #[test]
    fn a_stale_pet_expires() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 2);
        pet.note_petted(t);
        // 50 keys at ~22/s: the pet chases (never arrives) past PET_LATCH_TTL.
        let (t, _) = type_run(&mut pet, t, 3, 4, 50, 0.045);
        assert_eq!(pet.pending_pets(), 0, "the latch expired mid-chase");
        let (_, _f) = idle(&mut pet, t, (3, 54), 0.6);
        assert_eq!(
            pet.pet_hold_t, 0.0,
            "no purr hold fires for a click from before the sprint"
        );
    }

    #[test]
    fn a_pet_under_reduced_motion_is_bookkeeping_only() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        let reduced = |t: Instant| {
            let mut s = sense(t, Some((3, 10)));
            s.reduced_motion = true;
            s
        };
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let _ = pet.tick(reduced(t));
        }
        let c0 = pet.content();
        pet.note_petted(t);
        t += Duration::from_millis(16);
        let f = pet.tick(reduced(t));
        assert!(
            matches!(f.action, PetAction::Sit | PetAction::Sleep),
            "no purr hold under reduced motion"
        );
        assert!(f.motes.iter().all(Option::is_none), "no heart");
        assert!(pet.content() > c0, "but the warmth still lands");
        assert_eq!(pet.pending_pets(), 0, "the latch is spent, not parked");
        assert_eq!(pet.pet_hold_t, 0.0);
    }

    /// The wave-1 latches hold `needs_frames` open (they need ticks to be
    /// consumed or to expire) and release it once spent — the idle-to-zero
    /// law extended to the new envelopes.
    #[test]
    fn wave_one_latches_arm_and_release_the_frame_lane() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (t, _) = type_run(&mut pet, start, 4, 10, 6, 0.05);
        // Deep-settle the pet: the lane must be released before the stimulus.
        let (mut t, _) = idle(&mut pet, t, (4, 16), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert!(!pet.needs_frames(), "fixture: deeply asleep, lane released");
        pet.note_petted(t);
        assert!(pet.needs_frames(), "a latched pet re-arms the lane");
        // Wake (the pet is asleep, so the click stretches it first), purr,
        // and settle all the way back down: the lane must release again.
        let (t2, _) = idle(&mut pet, t, (4, 16), 6.0);
        t = t2;
        assert_eq!(pet.pending_pets(), 0, "consumed on the ground");
        let (_, f) = idle(&mut pet, t, (4, 16), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert_eq!(f.action, PetAction::Sleep);
        assert!(
            !pet.needs_frames(),
            "every wave-1 envelope is finite: the lane releases again"
        );
    }

    // ── wave 2: perk-and-watch ──────────────────────────────────────────

    #[test]
    fn a_streaming_pane_perks_the_sitter() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (t, f) = idle(&mut pet, t, (4, 12), SIT_AFTER + 0.4);
        assert!(f.action.settled(), "fixture: a settled cat");
        // ~0.6 s of sustained output: the heat crosses WATCH_GATE (0.14 s)
        // and the perk holds for as long as the stream does.
        let (_, f) = stream(&mut pet, t, (4, 12), 0.6);
        assert_eq!(
            f.action,
            PetAction::Perk,
            "the pet turns toward the streaming text and watches"
        );
        assert!(
            !f.facing_left || f32::from(12u16) < f.col,
            "and it faces the output (the caret side)"
        );
    }

    #[test]
    fn typing_echo_never_perks() {
        // The brain half of the echo law: the host only raises
        // `output_burst` when the shell is EXECUTING, so plain typing
        // arrives with the flag down — and a flagless frame must never
        // charge the watch.
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = idle(&mut pet, t, (4, 12), SIT_AFTER + 0.4);
        for _ in 0..120 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 12))));
            assert_ne!(
                f.action,
                PetAction::Perk,
                "no burst, no watch — the sitter just sits"
            );
        }
        assert_eq!(pet.watch_heat, 0.0, "and no heat accumulates");
    }

    #[test]
    fn the_watch_ends_before_sleep_is_due() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (t, _) = idle(&mut pet, t, (4, 12), SIT_AFTER + 0.2);
        // A stream longer than the cap: the watch must drop the pet back to
        // the settle ladder while the stream is STILL running.
        let (t, f) = stream(&mut pet, t, (4, 12), WATCH_MAX + 2.0);
        assert_ne!(
            f.action,
            PetAction::Perk,
            "the hard cap ends the stare — a settled still sticker remains"
        );
        assert!(pet.watch_spent, "the cap latched");
        // The stream stops; the heat decays; the pet settles all the way to
        // deep sleep and the frame lane releases (the 2327 shape).
        let (t, _) = idle(&mut pet, t, (4, 12), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert!(
            !pet.needs_frames(),
            "the watch is finite: idle-to-zero survives an endless stream"
        );
        let f1 = pet.tick(sense(t + Duration::from_secs(1), Some((4, 12))));
        let f2 = pet.tick(sense(t + Duration::from_secs(2), Some((4, 12))));
        assert_eq!(f1.fp(), f2.fp(), "and the frame is byte-stable");
    }

    #[test]
    fn a_caret_jump_interrupts_the_watch() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (t, _) = idle(&mut pet, t, (4, 12), SIT_AFTER + 0.3);
        let (mut t, f) = stream(&mut pet, t, (4, 12), 1.0);
        assert_eq!(f.action, PetAction::Perk, "fixture: mid-watch");
        // A word jump lands mid-watch: work outranks attention.
        let mut saw_travel = false;
        for k in 0..120 {
            t += Duration::from_millis(16);
            let caret = (4u16, 30u16);
            let mut s = sense(t, Some(caret));
            s.output_burst = k < 30; // the stream keeps going a beat longer
            let f = pet.tick(s);
            if matches!(f.action, PetAction::Crouch | PetAction::Leap) {
                saw_travel = true;
            }
        }
        assert!(
            saw_travel,
            "the latched jump must pull the watcher off its perch"
        );
    }

    // ── wave 2: the word-cat bat ────────────────────────────────────────

    #[test]
    fn a_nearby_peek_gets_batted() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.3);
        // A word-cat head lands 7 columns away, same row: a toy in reach.
        // An EVEN serial deals the SWIPE (wave 3's greet parity — the odd
        // half of the visits playbow instead; see
        // `near_peek_alternates_swipe_and_greeting`).
        pet.mote_serial = 0;
        pet.note_peek(t, 22.0, 4.0);
        assert!(pet.pending_bat.is_some(), "in range: the bat latches");
        let (mut crouched, mut leapt, mut swiped, mut dusted) = (false, false, false, false);
        for _ in 0..150 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 14))));
            match f.action {
                PetAction::Crouch => crouched = true,
                PetAction::Leap => leapt = true,
                PetAction::Frolic if f.pose == PetGlyphId::PetBat => {
                    swiped = true;
                    if f.motes
                        .iter()
                        .flatten()
                        .any(|m| m.kind == PetMoteKind::Dust)
                    {
                        dusted = true;
                    }
                }
                _ => {}
            }
        }
        assert!(crouched && leapt, "travel to the standoff: crouch, fly");
        assert!(swiped, "the swipe: the bat pose, pinned for the hold");
        assert!(dusted, "with one neutral puff at the peek cell");
        // And home again: the ordinary chase ladder walks it back.
        let (_, f) = idle(&mut pet, t, (4, 14), 4.0);
        let w = art_cols(10, 20);
        assert!(
            (f.col - PetBrain::station(14, 100, w)).abs() <= ARRIVED + SIT_FRONT_GAP,
            "home by the caret, got col {}",
            f.col
        );
    }

    #[test]
    fn a_far_peek_is_ignored() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.3);
        // 40 columns away: scenery, not a toy — the note never latches.
        pet.note_peek(t, 55.0, 4.0);
        assert!(
            pet.pending_bat.is_none(),
            "a far peek is ignored at note time"
        );
        for _ in 0..80 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 14))));
            assert!(
                !matches!(f.action, PetAction::Crouch | PetAction::Leap),
                "and nothing travels, got {:?}",
                f.action
            );
        }
    }

    #[test]
    fn a_stale_peek_expires() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 2);
        // Get the chase up to speed FIRST, then note a peek beside the
        // running pet: the work ahead outlasts BAT_TTL by construction.
        let (t, _) = type_run(&mut pet, t, 3, 4, 6, 0.045);
        pet.note_peek(t, pet.col + 5.0, 3.0);
        assert!(pet.pending_bat.is_some(), "fixture: latched in range");
        // 44 more keys at ~22 cols/s: never arrived, ~2 s past the note.
        let (t, _) = type_run(&mut pet, t, 3, 10, 44, 0.045);
        assert!(
            pet.pending_bat.is_none(),
            "the head pulled back long ago: the latch expired mid-chase"
        );
        let mut t2 = t;
        let mut swiped = false;
        for _ in 0..120 {
            t2 += Duration::from_millis(16);
            let f = pet.tick(sense(t2, Some((3, 54))));
            swiped |= f.action == PetAction::Frolic;
        }
        assert!(!swiped, "no bat fires off a dead latch");
    }

    #[test]
    fn work_outranks_the_bat() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.3);
        pet.note_peek(t, 22.0, 4.0);
        // The caret jumps the same instant: work first.
        let mut first_land: Option<f32> = None;
        let mut swiped_after_land = false;
        for _ in 0..250 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 40))));
            if f.action == PetAction::Land && first_land.is_none() {
                first_land = Some(f.col);
            }
            if f.action == PetAction::Frolic && f.pose == PetGlyphId::PetBat && first_land.is_some()
            {
                swiped_after_land = true;
            }
        }
        let w = art_cols(10, 20);
        let station = PetBrain::station(40, 100, w);
        let land = first_land.expect("the caret jump must fly");
        assert!(
            (land - station).abs() < 6.0,
            "the FIRST flight serves the caret (landed {land}, station {station}), \
             never the toy"
        );
        assert!(
            swiped_after_land,
            "and the bat still fires after the work, inside its TTL"
        );
    }

    #[test]
    fn reduced_motion_peek_is_nothing_at_all() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((3, 10))));
        }
        pet.note_peek(t, 15.0, 3.0);
        for _ in 0..60 {
            t += Duration::from_millis(16);
            let mut s = sense(t, Some((3, 10)));
            s.reduced_motion = true;
            let f = pet.tick(s);
            assert!(
                matches!(f.action, PetAction::Sit | PetAction::Sleep),
                "no bat under reduced motion, got {:?}",
                f.action
            );
            assert!(f.motes.iter().all(Option::is_none), "and no dust");
        }
        assert!(pet.pending_bat.is_none(), "the latch clears as bookkeeping");
    }

    // ── wave 2: idle micro-life ─────────────────────────────────────────

    #[test]
    fn an_output_pulse_twitches_the_settled_ear() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        let (mut t, _) = idle(&mut pet, t, (4, 14), 0.4);
        // ONE burst frame — a pulse, not a stream (heat stays far below
        // the watch gate).
        t += Duration::from_millis(16);
        let mut s = sense(t, Some((4, 14)));
        s.output_burst = true;
        let _ = pet.tick(s);
        let mut bobbed = false;
        for _ in 0..10 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 14))));
            assert_ne!(f.action, PetAction::Perk, "a pulse is not a stream");
            assert!(f.motes.iter().all(Option::is_none), "and floats nothing");
            if (f.scale_y - 1.0).abs() > 0.005 {
                bobbed = true;
            }
        }
        assert!(bobbed, "the ear twitch: a visible 2% bob, nothing more");
    }

    #[test]
    fn flick_beats_are_quiet_phased() {
        // Replay-identical: the same settle driven from two different
        // ABSOLUTE instants produces byte-identical frames — the loaf's
        // thump is a function of the quiet, never of the wall clock.
        let run = |start: Instant| -> Vec<u64> {
            let mut pet = PetBrain::default();
            let t = awake(&mut pet, start, 4, 12);
            let (mut t, _) = idle(&mut pet, t, (4, 14), LOAF_AFTER);
            let mut fps = Vec::new();
            for _ in 0..((FLICK_EVERY * 2.5 / 0.016) as u32) {
                t += Duration::from_millis(16);
                fps.push(pet.tick(sense(t, Some((4, 14)))).fp());
            }
            fps
        };
        let a = run(Instant::now());
        let b = run(Instant::now() + Duration::from_millis(4_321));
        assert_eq!(a, b, "the thump schedule is quiet-phased, not wall-clocked");
        assert!(
            a.windows(2).any(|w| w[0] != w[1]),
            "and a thump actually plays inside the window"
        );
    }

    #[test]
    fn micro_life_dies_with_the_breath_window() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        // A pulse (one twitch), then a long march through loaf and sleep
        // to the deep-sleep window's far side.
        let (mut t, _) = idle(&mut pet, t, (4, 14), 0.3);
        t += Duration::from_millis(16);
        let mut s = sense(t, Some((4, 14)));
        s.output_burst = true;
        let _ = pet.tick(s);
        let (t, f) = idle(&mut pet, t, (4, 14), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert_eq!(f.action, PetAction::Sleep, "fixture: deeply asleep");
        assert!(
            !pet.needs_frames(),
            "every micro-life envelope died inside the settle window"
        );
        let f1 = pet.tick(sense(t + Duration::from_secs(1), Some((4, 14))));
        let f2 = pet.tick(sense(t + Duration::from_secs(2), Some((4, 14))));
        assert_eq!(f1.fp(), f2.fp(), "deep sleep is byte-stable — the 2327 law");
    }

    // ── wave 2: the breed handoff (in-place swap + departing body) ──────

    #[test]
    fn a_debounced_park_swaps_in_place_and_a_departing_body_runs_off() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 48);
        assert_eq!(
            pet.sync_look((3, 1), PetArrival::Ceremony).worn,
            (3, 1),
            "the first dress applies"
        );
        let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (3, 1),
            "fixture: the repoint parks mid-appearance"
        );
        let w = art_cols(10, 20);
        let station = PetBrain::station(50, 100, w);
        let mut ghost_frames = 0u32;
        let mut ghost_first: Option<f32> = None;
        let mut ghost_last = 0.0f32;
        let mut swap_frame_moved = false;
        let mut swapped = false;
        let mut prev_col = None::<f32>;
        let mut last_departures_live = false;
        // THE ARRIVAL: the incoming kitty fades UP over ARRIVE_IN from the
        // swap frame (never a one-frame recolour); before the swap and after
        // the ramp the live pet is fully opaque.
        let mut swap_at: Option<Instant> = None;
        let mut prev_alpha = 255u8;
        let mut ramp_frames = 0u32;
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 50))));
            let worn = pet.sync_look((9, 4), PetArrival::Ceremony).worn; // the host's per-frame re-sync
            if swap_at.is_none() && worn == (9, 4) {
                swap_at = Some(t);
                assert!(
                    f.alpha > 0 && f.alpha < 64,
                    "the incoming kitty starts faint — but ON glass — at the swap ({})",
                    f.alpha
                );
            }
            match swap_at {
                None => assert_eq!(f.alpha, 255, "before the swap: fully there"),
                Some(at) if at == t => {} // the swap frame itself: the dip
                Some(at) if t.duration_since(at).as_secs_f32() <= ARRIVE_IN => {
                    assert!(f.alpha >= prev_alpha, "the arrival only ever fades UP");
                    ramp_frames += 1;
                }
                Some(_) => assert_eq!(f.alpha, 255, "after the ramp: fully there"),
            }
            prev_alpha = f.alpha;
            // THE LIVE PET IS UNBOTHERED: it never leaves its post — the swap
            // is an arrival in place, not a trip.
            assert!(
                (f.col - station).abs() < 4.0,
                "…and never leaves its station ({} vs {station})",
                f.col
            );
            let live: Vec<_> = f.departures.iter().flatten().collect();
            assert!(live.len() <= 1, "one swap spawns exactly one ghost");
            if let Some(d) = live.first() {
                ghost_frames += 1;
                assert_eq!(
                    (d.coat, d.iris),
                    (3, 1),
                    "the ghost wears the OUTGOING look"
                );
                assert_eq!(
                    worn,
                    (9, 4),
                    "…while the live pet already wears the incoming one"
                );
                if ghost_first.is_none() {
                    ghost_first = Some(d.col);
                    // The swap tick did not teleport the live pet.
                    if let Some(p) = prev_col {
                        swap_frame_moved = (f.col - p).abs() > 0.01;
                    }
                }
                ghost_last = d.col;
            }
            swapped |= worn == (9, 4);
            prev_col = Some(f.col);
            last_departures_live = f.departures.iter().any(Option::is_some);
        }
        assert!(swapped, "the debounced park lands the new look");
        assert!(!swap_frame_moved, "the swap never moves the live pet");
        assert!(
            ramp_frames >= (ARRIVE_IN / 0.016) as u32 - 2,
            "the arrival ramp ran its whole length ({ramp_frames} frames)"
        );
        let first = ghost_first.expect("the swap spawned a departing body");
        assert!(
            ghost_last > first + 10.0,
            "the ghost genuinely ran toward its edge ({first} → {ghost_last})"
        );
        assert!(
            ghost_frames >= 30 && ghost_frames <= (DEPART_MAX / 0.016) as u32 + 4,
            "the visit is brief and strictly bounded ({ghost_frames} frames)"
        );
        assert!(
            !last_departures_live,
            "every departing body has fully retired by the end"
        );
    }

    /// THE DOC §8.4: on the swap frame the outgoing ghost leaves at FULL
    /// opacity while the incoming body sits at the [`ARRIVE_FLOOR`] —
    /// presence and arrival ride separate bytes now (`lane_alpha`), so the
    /// emitter's ghost multiply (`d.alpha × lane_alpha / 255`) can no
    /// longer dim the leaver by the arriver's ramp (§1.1(a)'s shipped bug:
    /// both cats starting at 6%).
    #[test]
    fn the_departing_ghost_leaves_at_full_opacity() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 48);
        assert_eq!(
            pet.sync_look((3, 1), PetArrival::Ceremony).worn,
            (3, 1),
            "the first dress applies"
        );
        let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (3, 1),
            "fixture: the repoint parks"
        );
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 50))));
            if pet.sync_look((9, 4), PetArrival::Ceremony).worn == (9, 4) {
                // The swap frame itself.
                let d = f
                    .departures
                    .iter()
                    .flatten()
                    .next()
                    .expect("the swap spawned its ghost on the same tick");
                assert_eq!(
                    f.alpha,
                    (ARRIVE_FLOOR * 255.0) as u8,
                    "the incoming body lands at the arrival floor"
                );
                assert_eq!(f.lane_alpha, 255, "…while the LANE is at full presence");
                let composed = u16::from(d.alpha) * u16::from(f.lane_alpha) / 255;
                assert!(
                    composed >= 250,
                    "the ghost leaves at full opacity through the emitter's \
                     own multiply ({composed})"
                );
                return;
            }
        }
        panic!("the debounced park never landed");
    }

    /// THE DOC §2.0.10.5: the quiet re-dress NEVER flips the cat. The
    /// zero-span ghost is the body the user was just looking at, so it
    /// holds the live pet's `facing_left` and its last emitted pose for
    /// the whole 0.25 s fade. Fails before the `still`/carried-facing fix:
    /// the run-derived `dir` (zero span ⇒ `+1`) mirror-flipped a
    /// left-facing cat and froze it in run frame 0 — a gallop pose for the
    /// whole dissolve, visibly worse than the recolour it replaces.
    #[test]
    fn a_quiet_redress_never_flips_the_cat() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 48);
        assert_eq!(
            pet.sync_look((3, 1), PetArrival::Ceremony).worn,
            (3, 1),
            "the first dress applies"
        );
        // Walk the caret LEFT so the settled pet faces LEFT — exactly the
        // shape the run-derived dir used to mirror-flip.
        let (mut t, f) = idle(&mut pet, t, (4, 30), 2.0);
        assert!(f.facing_left, "fixture: the pet must be facing left");
        assert!(f.action.settled(), "fixture: …and settled");
        let out = pet.sync_look((9, 4), PetArrival::Quiet);
        assert_eq!(out.worn, (3, 1), "fixture: the homecoming parks");
        assert!(out.parked, "…and reports the genuine park");
        let mut prev_pose = f.pose;
        let mut prev_facing = f.facing_left;
        let mut worn_pose: Option<PetGlyphId> = None;
        let mut swap_col = 0.0f32;
        let mut swapped = false;
        let mut ghost_frames = 0u32;
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 30))));
            let out = pet.sync_look((9, 4), PetArrival::Quiet);
            if !swapped && out.worn == (9, 4) {
                swapped = true;
                swap_col = f.col;
                assert!(prev_facing, "fixture: still facing left at the swap");
                // The pose the ghost must wear: the live body's last
                // emitted frame — captured the tick BEFORE the swap.
                worn_pose = Some(prev_pose);
            }
            if let Some(d) = f.departures.iter().flatten().next() {
                ghost_frames += 1;
                assert!(
                    d.facing_left,
                    "the zero-span ghost holds the live pet's facing — \
                     never the run-derived dir"
                );
                assert_eq!(
                    d.pose,
                    worn_pose.expect("no ghost before the swap"),
                    "…and the cached pose, for its whole fade"
                );
                assert!(
                    (d.col - swap_col).abs() < 1e-3,
                    "…without ever moving ({} vs {swap_col})",
                    d.col
                );
            }
            if !swapped {
                prev_pose = f.pose;
                prev_facing = f.facing_left;
            }
        }
        assert!(swapped, "the debounced park landed");
        let want = (EDGE_FADE / 0.016) as u32;
        assert!(
            ghost_frames >= want - 2 && ghost_frames <= want + 2,
            "the still ghost held for the whole 0.25 s ({ghost_frames} frames)"
        );
    }

    /// THE DOC §2.0.6: the quiet path is a 0.25 s IN-PLACE cross-dissolve —
    /// not a recolour, not a run, not nothing. The zero-span ghost's
    /// `life()` is exactly [`EDGE_FADE`] by the shipped arithmetic, neither
    /// body is displaced by so much as a cell fraction, and the live ramp
    /// (`arrive_in == HOMECOMING_IN == EDGE_FADE` — the symmetry law) is
    /// back at full opacity a quarter second after the swap.
    #[test]
    fn a_quiet_redress_is_a_quarter_second_crossfade() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 48);
        assert_eq!(
            pet.sync_look((3, 1), PetArrival::Ceremony).worn,
            (3, 1),
            "the first dress applies"
        );
        let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
        let out = pet.sync_look((9, 4), PetArrival::Quiet);
        assert_eq!(out.worn, (3, 1), "fixture: the homecoming parks");
        assert!(out.parked, "…and reports the genuine park");
        let mut swap_at: Option<Instant> = None;
        let mut live_col0 = 0.0f32;
        let mut ghost_col0: Option<f32> = None;
        let mut ghost_frames = 0u32;
        let mut prev_alpha = 255u8;
        let mut prev_ghost_alpha = 255u8;
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 50))));
            let out = pet.sync_look((9, 4), PetArrival::Quiet);
            if swap_at.is_none() && out.worn == (9, 4) {
                swap_at = Some(t);
                live_col0 = f.col;
                assert_eq!(
                    f.alpha,
                    (ARRIVE_FLOOR * 255.0) as u8,
                    "the landing frame sits at the arrival floor"
                );
                assert_eq!(f.lane_alpha, 255, "…with the LANE at full presence");
            }
            let live: Vec<_> = f.departures.iter().flatten().collect();
            assert!(live.len() <= 1, "one re-dress spawns exactly one ghost");
            if let Some(d) = live.first() {
                ghost_frames += 1;
                let g0 = *ghost_col0.get_or_insert(d.col);
                assert!(
                    (d.col - g0).abs() < 1e-3,
                    "zero displacement: the ghost never moves ({} vs {g0})",
                    d.col
                );
                // 0.05 cells (half a pixel at cell_w 10) of slack: the
                // settled pet's own station ease-home drifts ~0.01 cells
                // over the fade, and that drift is the pet's, not the
                // re-dress's — any genuine relocation is whole cells.
                assert!(
                    (f.col - live_col0).abs() < 0.05,
                    "…and neither does the live body ({} vs {live_col0})",
                    f.col
                );
                assert!(
                    d.alpha <= prev_ghost_alpha,
                    "the old coat only ever fades DOWN"
                );
                prev_ghost_alpha = d.alpha;
            }
            match swap_at {
                None => assert_eq!(f.alpha, 255, "before the swap: fully there"),
                Some(at) if at == t => {} // the swap frame itself: the floor
                Some(at) if t.duration_since(at).as_secs_f32() < HOMECOMING_IN => {
                    assert!(f.alpha >= prev_alpha, "the arrival only ever fades UP");
                }
                Some(_) => assert_eq!(
                    f.alpha, 255,
                    "a quarter second after the swap the new coat is FULLY there"
                ),
            }
            prev_alpha = f.alpha;
        }
        assert!(swap_at.is_some(), "the debounced park landed");
        let want = (EDGE_FADE / 0.016) as u32;
        assert!(
            ghost_frames >= want - 2 && ghost_frames <= want + 2,
            "the zero-span ghost's life is exactly EDGE_FADE \
             ({ghost_frames} frames)"
        );
    }

    /// The other half of the ladder: a CEREMONY park is untouched by the
    /// quiet path — the stranger still gets today's full theater, the
    /// [`ARRIVE_IN`] fade-up overlapping a departing body that genuinely
    /// RUNS to its door (never the quarter-second in-place fade).
    #[test]
    fn a_ceremony_park_still_runs_today_s_theater() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 48);
        assert_eq!(
            pet.sync_look((3, 1), PetArrival::Ceremony).worn,
            (3, 1),
            "the first dress applies"
        );
        let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
        let out = pet.sync_look((9, 4), PetArrival::Ceremony);
        assert_eq!(out.worn, (3, 1), "fixture: the stranger parks");
        assert!(out.parked, "…and reports the genuine park");
        let mut swap_at: Option<Instant> = None;
        let mut ghost_first: Option<f32> = None;
        let mut ghost_last = 0.0f32;
        let mut ghost_frames = 0u32;
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 50))));
            let out = pet.sync_look((9, 4), PetArrival::Ceremony);
            if swap_at.is_none() && out.worn == (9, 4) {
                swap_at = Some(t);
                assert!(
                    f.alpha > 0 && f.alpha < 64,
                    "the incoming kitty starts faint at the swap ({})",
                    f.alpha
                );
            }
            if let Some(at) = swap_at {
                let since = t.duration_since(at).as_secs_f32();
                if (0.4..0.6).contains(&since) {
                    // The quiet ramp would already be DONE here (0.25 s);
                    // the ceremony's ARRIVE_IN (0.9 s) is still climbing.
                    assert!(
                        f.alpha < 255,
                        "the ceremony keeps the FULL ARRIVE_IN ramp ({})",
                        f.alpha
                    );
                }
                if since > ARRIVE_IN + 0.05 {
                    assert_eq!(f.alpha, 255, "after the ramp: fully there");
                }
            }
            if let Some(d) = f.departures.iter().flatten().next() {
                ghost_frames += 1;
                ghost_first.get_or_insert(d.col);
                ghost_last = d.col;
            }
        }
        assert!(swap_at.is_some(), "the debounced park landed");
        let first = ghost_first.expect("the ceremony spawned a departing body");
        assert!(
            ghost_last > first + 10.0,
            "the ghost genuinely RAN toward its door ({first} → {ghost_last})"
        );
        assert!(
            ghost_frames > (EDGE_FADE / 0.016) as u32 + 4,
            "…and lived a run's life, not a re-dress fade ({ghost_frames} frames)"
        );
    }

    /// THE DOC §8.5: on a WIDE pane the exit still reaches its door — the
    /// departure solves its own speed at spawn ([`Departure::speed`]), so
    /// the ghost's last live frame is AT the edge, never a mid-screen
    /// dissolve. Fails before this wave: `DEPART_MAX` 2.0 capped the
    /// reachable run at 36 cells and the ghost evaporated in open ground.
    #[test]
    fn a_ghost_reaches_its_door_on_a_wide_pane() {
        let wide = |now: Instant, caret: Option<(u16, u16)>| PetSense {
            cols: 200,
            ..sense(now, caret)
        };
        let start = Instant::now();
        let mut pet = PetBrain::default();
        // Wake with two keystrokes ending on caret 10, then settle at the
        // station just past it (the `awake` fixture, on the wide pane).
        let mut t = start;
        let _ = pet.tick(wide(t, Some((4, 8))));
        for k in 1..=2u16 {
            let until = t + Duration::from_secs_f32(0.05);
            while t < until {
                t += Duration::from_millis(16);
                let _ = pet.tick(wide(t, Some((4, 8 + k - 1))));
            }
            t = until;
            let _ = pet.tick(wide(t, Some((4, 8 + k))));
        }
        let settle = t + Duration::from_secs_f32(SIT_AFTER + 0.2);
        while t < settle {
            t += Duration::from_millis(16);
            let _ = pet.tick(wide(t, Some((4, 10))));
        }
        assert_eq!(
            pet.sync_look((3, 1), PetArrival::Ceremony).worn,
            (3, 1),
            "the first dress applies"
        );
        let hold = t + Duration::from_secs_f32(0.2);
        while t < hold {
            t += Duration::from_millis(16);
            let _ = pet.tick(wide(t, Some((4, 10))));
        }
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (3, 1),
            "fixture: the repoint parks"
        );
        let w = art_cols(10, 20);
        // The pet stands PAST the caret, so its exit is the far right door.
        let door = 200.0 - w;
        let mut first: Option<f32> = None;
        let mut last: Option<f32> = None;
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(wide(t, Some((4, 10))));
            let _ = pet.sync_look((9, 4), PetArrival::Ceremony).worn;
            if let Some(d) = f.departures.iter().flatten().next() {
                first.get_or_insert(d.col);
                last = Some(d.col);
            }
        }
        let first = first.expect("the swap spawned a departing body");
        assert!(
            first < 20.0,
            "the ghost was born at the pet's feet by the caret ({first})"
        );
        let last = last.expect("the ghost lived at least one frame");
        assert!(
            (last - door).abs() < 0.5,
            "the ghost's last live frame is AT its door, not a mid-screen \
             dissolve ({last} vs {door})"
        );
    }

    /// THE DOC §8.6: the departure lane rides the dt clamp, so a single
    /// 5 s stalled tick (a hidden window, a breakpoint, a suspended
    /// machine) advances a ghost by at most `speed × 0.10` — one clamped
    /// frame of run — and never expires it mid-flight. Fails before this
    /// wave: the lane ran on the unclamped quiet clock, and one long frame
    /// aged a ghost five seconds.
    #[test]
    fn one_long_frame_cannot_move_a_ghost_a_screen() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 48);
        assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
        let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (3, 1),
            "fixture: the repoint parks"
        );
        // Walk up to the swap frame at the host's real cadence.
        let mut before: Option<f32> = None;
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 50))));
            let _ = pet.sync_look((9, 4), PetArrival::Ceremony).worn;
            if let Some(d) = f.departures.iter().flatten().next() {
                before = Some(d.col);
                break;
            }
        }
        let before = before.expect("the swap spawned a departing body");
        // ONE stalled frame: five seconds of wall time in a single tick.
        t += Duration::from_secs(5);
        let f = pet.tick(sense(t, Some((4, 50))));
        let d = f.departures.iter().flatten().next().expect(
            "the ghost survives the stall — the lane aged one clamped \
             frame, not five seconds",
        );
        let moved = d.col - before;
        assert!(moved >= 0.0, "the lane never runs backwards ({moved})");
        assert!(
            moved <= DEPART_SPEED * 0.10 + 1e-3,
            "one long frame moves a ghost at most speed × 0.10 cells ({moved})"
        );
    }

    #[test]
    fn a_round_trip_flip_never_walks() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 48);
        assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
        let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
        // The app flips the identity... and flips back within a second —
        // every command round-trip does this.
        let _ = pet.sync_look((9, 4), PetArrival::Ceremony).worn;
        let (t2, _) = idle(&mut pet, t, (4, 50), 1.0);
        t = t2;
        let _ = pet.sync_look((3, 1), PetArrival::Ceremony).worn; // home again: the park dissolves
        let w = art_cols(10, 20);
        let station = PetBrain::station(50, 100, w);
        for _ in 0..400 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 50))));
            assert!(
                (f.col - station).abs() < 4.0,
                "a round-trip flip must never march the pet off ({} vs {station})",
                f.col
            );
            assert_eq!(f.alpha, 255, "and never fades it");
            assert!(
                f.departures.iter().all(Option::is_none),
                "…and never spawns a departing body"
            );
        }
    }

    /// WORK OUTRANKS THEATER: a screen-crossing caret jump landing right at
    /// the debounce boundary is served FIRST — no swap and no ghost while the
    /// pet is airborne — and the handoff fires at the next arrival, spawning
    /// the departing body at the pet's NEW station.
    #[test]
    fn a_caret_jump_is_served_before_the_swap() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
        let (mut t, _) = idle(&mut pet, t, (4, 12), 0.2);
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (3, 1),
            "fixture: the repoint parks"
        );
        // Sit through MOST of the debounce, then jump the caret across the
        // screen: the choreography (perk → wiggle → bound) straddles the
        // 2.5 s mark, so the swap becomes due while the pet is mid-trip.
        let (t2, _) = idle(&mut pet, t, (4, 12), HANDOFF_DEBOUNCE - 0.3);
        t = t2;
        let mut saw_flight = false;
        let mut ghost_after_flight = false;
        for _ in 0..400 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 90))));
            let worn = pet.sync_look((9, 4), PetArrival::Ceremony).worn;
            if f.action.airborne() {
                saw_flight = true;
                assert_eq!(worn, (3, 1), "no swap mid-air — work first, always");
                assert!(
                    f.departures.iter().all(Option::is_none),
                    "and no ghost spawns mid-air"
                );
            }
            if let Some(d) = f.departures.iter().flatten().next() {
                ghost_after_flight = true;
                assert!(saw_flight, "the ghost is born only after the trip");
                assert_eq!(worn, (9, 4), "the ghost's birth IS the swap");
                assert_eq!((d.coat, d.iris), (3, 1), "it wears the old look");
                assert!(
                    d.col > 50.0,
                    "…and spawned at the pet's NEW station, not the old one ({})",
                    d.col
                );
            }
        }
        assert!(saw_flight, "fixture: the jump really flew the pet");
        assert!(ghost_after_flight, "the handoff fired at the next arrival");
    }

    #[test]
    fn reduced_motion_swaps_instantly() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((3, 10))));
        }
        assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (3, 1),
            "parks while visible"
        );
        t += Duration::from_millis(16);
        let mut s = sense(t, Some((3, 10)));
        s.reduced_motion = true;
        let f = pet.tick(s);
        assert!(
            matches!(f.action, PetAction::Sit | PetAction::Sleep),
            "no walk under reduced motion"
        );
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (9, 4),
            "the parked look applied immediately — theater is motion, \
             the costume is state"
        );
        assert!(
            f.departures.iter().all(Option::is_none),
            "no departing body under reduced motion — departures are theater"
        );
    }

    /// The departure lane's cap: at most [`PET_DEPARTURES_MAX`] ghosts ride
    /// at once, overflow drops the OLDEST, and expiry frees every slot —
    /// after which the lane pins no frames (idle-to-zero holds).
    #[test]
    fn departures_cap_at_the_documented_bound_and_drop_the_oldest() {
        let mut pet = PetBrain::default();
        for k in 0..4u8 {
            pet.lane_clock = f64::from(k) * 0.1;
            pet.spawn_departure(Departure {
                born: pet.lane_clock,
                coat: k,
                iris: 0,
                from_col: 10.0,
                row: 4.0,
                edge: 90.0,
                speed: DEPART_SPEED,
                still: false,
                facing_left: false,
                stride0: 0.0,
                pose0: PetGlyphId::PetSit,
            });
        }
        let live: Vec<u8> = pet
            .resolve_departures()
            .iter()
            .flatten()
            .map(|d| d.coat)
            .collect();
        assert_eq!(live.len(), PET_DEPARTURES_MAX, "the cap holds");
        assert!(
            !live.contains(&0) && [1, 2, 3].iter().all(|c| live.contains(c)),
            "the OLDEST ghost was the one dropped ({live:?})"
        );
        // Expiry frees the lane and releases the frame cadence.
        pet.lane_clock += f64::from(DEPART_MAX) + 0.1;
        let gone = pet.resolve_departures();
        assert!(gone.iter().all(Option::is_none), "all visits expired");
        assert!(
            pet.departures.iter().all(Option::is_none),
            "…and their slots are freed, not parked"
        );
        assert!(
            !pet.needs_frames(),
            "no lingering animating=true once the ghosts are gone"
        );
    }

    /// A departing body is a PURE FUNCTION of (birth record, clock): two
    /// brains resolving the same record at the same clocks agree byte for
    /// byte, the run is monotone toward the edge, and the tail fades to zero
    /// inside [`DEPART_MAX`] — clockless determinism, no dice at tick time.
    #[test]
    fn a_departing_body_is_a_pure_function_of_its_record() {
        let record = Departure {
            born: 5.0,
            coat: 7,
            iris: 2,
            from_col: 20.0,
            row: 6.0,
            edge: 94.0,
            speed: DEPART_SPEED,
            still: false,
            facing_left: false,
            stride0: 0.0,
            pose0: PetGlyphId::PetSit,
        };
        let mut a = PetBrain::default();
        let mut b = PetBrain::default();
        for pet in [&mut a, &mut b] {
            pet.lane_clock = 5.0;
            pet.spawn_departure(record);
        }
        let mut prev_col = record.from_col - 0.001;
        let mut faded = false;
        for step in 0..=((DEPART_MAX / 0.05) as u32 + 2) {
            let clock = 5.0 + f64::from(step) * 0.05;
            a.lane_clock = clock;
            b.lane_clock = clock;
            let (ra, rb) = (a.resolve_departures(), b.resolve_departures());
            match (&ra[0], &rb[0]) {
                (Some(da), Some(db)) => {
                    assert_eq!(da, db, "same record + clock ⇒ same sprite");
                    assert!(da.col >= prev_col, "the run is monotone");
                    assert!(!da.facing_left, "…and faces its edge");
                    prev_col = da.col;
                }
                (None, None) => faded = true,
                _ => panic!("the two brains disagree about expiry"),
            }
        }
        assert!(faded, "the visit ends inside DEPART_MAX");
        assert!(
            prev_col > record.from_col + 10.0,
            "the body genuinely travelled ({prev_col})"
        );
    }

    // ── wave 2: pointer play ────────────────────────────────────────────

    /// Warm a settled pet past PLAY_CONTENT: a long run earns the ledger,
    /// and the short settle spends almost none of it.
    fn warm_settled(pet: &mut PetBrain, start: Instant) -> Instant {
        let t = awake(pet, start, 4, 2);
        let (t, _) = type_run(pet, t, 4, 4, 40, 0.045);
        let (t, _) = idle(pet, t, (4, 44), SIT_AFTER + 0.3);
        assert!(
            pet.content() >= PLAY_CONTENT,
            "fixture: the run must have warmed the cat ({})",
            pet.content()
        );
        t
    }

    #[test]
    fn the_gaze_follows_a_wandering_pointer() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        let (mut t, f) = idle(&mut pet, t, (4, 14), 0.1);
        assert_eq!(
            f.pose,
            PetGlyphId::PetSitFront,
            "fixture: eye contact at rest"
        );
        // The pointer wanders on the pet's RIGHT (behind the leftward gaze),
        // fast enough to matter: ~12 cells/s of gentle circling.
        let mut px = 40.0f32;
        let mut frame = f;
        for k in 0..30 {
            t += Duration::from_millis(16);
            px += if k % 10 < 5 { 0.2 } else { -0.2 };
            frame = ptick(&mut pet, t, (4, 14), Some((px, 4.0)));
        }
        assert!(pet.pointer_heat > 0.0, "the wandering built attention");
        assert_eq!(
            frame.pose,
            PetGlyphId::PetPeekShoulder,
            "a far pointer behind the facing earns the over-the-shoulder peek"
        );
        // The pointer stops: the heat decays and the gaze parks back on the
        // caret — the flags, not the pose, are the contract (long quiet
        // melts the sit into the loaf, which shows no gaze pose at all).
        for _ in 0..80 {
            t += Duration::from_millis(16);
            let _ = ptick(&mut pet, t, (4, 14), Some((px, 4.0)));
        }
        assert_eq!(pet.pointer_heat, 0.0, "a parked pointer is furniture");
        assert!(
            pet.sit_front && !pet.peek,
            "the gaze parks back on the caret (eye contact, no peek)"
        );
    }

    /// The caret every sleeping-gaze fixture below parks beside.
    const SLEEPER_CARET: (u16, u16) = (4, 12);

    /// A cat asleep beside [`SLEEPER_CARET`], and the frame it dozed off on.
    ///
    /// `hop_first` picks WHICH sleeper, and both are needed because they face
    /// opposite ways. A cat that simply settles ALWAYS dozes off facing left —
    /// the station parks it one cell past the caret, so the only thing near
    /// enough to make eye contact with is behind it on the left. The
    /// right-facing sleeper is real but arrives the other way: a prompt
    /// printing under a sleeper re-anchors it with a HOP, and a hop rightward
    /// sets the travel facing on the way out, so the cat lands still asleep
    /// with its back to the caret's side of the world.
    fn a_sleeping_cat(hop_first: bool) -> (PetBrain, Instant, PetFrame) {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let w = art_cols(10, 20);
        let t = awake(&mut pet, start, SLEEPER_CARET.0, SLEEPER_CARET.1);
        let (mut t, mut f) = idle(&mut pet, t, SLEEPER_CARET, SLEEP_AFTER + 1.0);
        assert_eq!(f.action, PetAction::Sleep, "fixture: a sleeping cat");
        if hop_first {
            let inked_to = (f.col + w + 3.0) as u16;
            let spans = vec![(0u16, inked_to); 8];
            pet.sense_ink(0, &spans, Some(SLEEPER_CARET.0));
            for _ in 0..60 {
                t += Duration::from_millis(16);
                f = pet.tick(sense(t, Some(SLEEPER_CARET)));
            }
            assert_eq!(
                f.action,
                PetAction::Sleep,
                "fixture: the hop handed the sleep back"
            );
            assert!(!f.facing_left, "fixture: the hop left it facing RIGHT");
        }
        (pet, t, f)
    }

    /// Swing the POINTER back and forth straight across a sleeping cat for
    /// ~10 s at ~1.5 Hz, ±2.2 cells. Returns `(facing it dozed off with, body
    /// turns, midline crossings, frames the gaze was in flip range)`.
    fn swing_over_a_sleeper(hop_first: bool) -> (bool, u32, u32, u32) {
        let caret = SLEEPER_CARET;
        let w = art_cols(10, 20);
        let (mut pet, mut t, f) = a_sleeping_cat(hop_first);
        let (mid, row, facing0) = (f.col + w * 0.5, f.row, f.facing_left);
        let (mut flips, mut crossed, mut in_range) = (0u32, 0u32, 0u32);
        let (mut prev, mut side) = (facing0, 0.0f32);
        for i in 0..600u16 {
            let swing = 2.2 * (f32::from(i) * 0.016 * TAU * 1.5).sin();
            t += Duration::from_millis(16);
            let f = ptick(&mut pet, t, caret, Some((mid + swing, row)));
            assert_eq!(
                f.action,
                PetAction::Sleep,
                "fixture: a wandering pointer does not wake it"
            );
            if swing.signum() != side.signum() {
                crossed += 1;
                side = swing;
            }
            if pet.sit_front {
                in_range += 1;
            }
            if f.facing_left != prev {
                flips += 1;
                prev = f.facing_left;
            }
        }
        assert!(
            pet.pointer_heat > 0.0,
            "fixture: the swinging is fast enough to hold the pet's gaze"
        );
        (facing0, flips, crossed, in_range)
    }

    /// OWNER, 2026-08-12: "when it's sleeping, and the cursor is near it and
    /// moving back and forth, having the kitty flip instantly looks bad. I'd
    /// like for that to be more subtle and realistic."
    ///
    /// The POINTER is the oscillator, and that is the whole reason the bug
    /// could exist: a caret move wakes the cat before it can turn it, but a
    /// mouse is not a keystroke, so a sleeping cat kept re-deciding its facing
    /// off a wandering pointer sixty times a second.
    ///
    /// BOTH doze-off facings are driven, because they are stopped by
    /// DIFFERENT brakes and one fixture would have pinned only one of them.
    /// A cat asleep facing LEFT is held by the DEAD BAND: the pointer can
    /// only get far enough round to its right to count as a side by leaving
    /// eye-contact range altogether, where the body does not turn at all. A
    /// cat asleep facing RIGHT genuinely does see the pointer arrive behind
    /// it, in range, several times a second — and is held by the DWELL,
    /// because not one of those crossings ever stays.
    #[test]
    fn a_cursor_swinging_over_a_sleeping_cat_never_flips_it() {
        for hop_first in [false, true] {
            let (facing0, flips, crossed, in_range) = swing_over_a_sleeper(hop_first);
            // The fixture really does swing over the cat, repeatedly, with the
            // gaze inside the range where a body turn is even possible…
            assert!(
                crossed >= 20,
                "fixture is inert — the pointer crossed the cat only {crossed} \
                 times (facing_left {facing0})"
            );
            assert!(
                in_range >= 100,
                "fixture never reached eye-contact range ({in_range} frames, \
                 facing_left {facing0}) — the turn branch was never entered"
            );
            // …and the sleeping cat does not answer it with its body.
            assert_eq!(
                flips, 0,
                "the sleeping cat (facing_left {facing0}) turned over {flips} \
                 times chasing the cursor"
            );
        }
    }

    /// The DEAD BAND's own case, which no amount of dwell would cover: a
    /// pointer that is not swinging across the cat at all but simply SITTING
    /// on it — a hair behind its nose, held there, moving only up and down so
    /// the gaze stays live. That target never changes its mind, so the dwell
    /// clock fills; the only thing standing between a sleeping cat and a
    /// body-flip at three-fifths of a cell is the band saying that is not a
    /// direction, it is the middle.
    #[test]
    fn a_cursor_resting_on_a_sleeping_cat_is_not_a_side_to_turn_to() {
        let w = art_cols(10, 20);
        let (mut pet, mut t, f) = a_sleeping_cat(true);
        assert!(!f.facing_left, "fixture: asleep facing right");
        // Just BEHIND the nose — inside the band, well inside eye-contact
        // range — and pinned there for ten seconds.
        let px = f.col + w * 0.5 - 0.5 - 0.6;
        let facing0 = f.facing_left;
        let (mut flips, mut prev, mut in_range) = (0u32, facing0, 0u32);
        for i in 0..600u16 {
            // Vertical only: the pointer is plainly alive (~10 cells/s, over
            // the gaze-follow bar) without its column ever moving.
            let py = f.row + 0.8 * (f32::from(i) * 0.016 * TAU * 2.0).sin();
            t += Duration::from_millis(16);
            let g = ptick(&mut pet, t, SLEEPER_CARET, Some((px, py)));
            assert_eq!(g.action, PetAction::Sleep, "fixture: it stays asleep");
            if pet.sit_front {
                in_range += 1;
            }
            if g.facing_left != prev {
                flips += 1;
                prev = g.facing_left;
            }
        }
        assert!(
            pet.pointer_heat > 0.0,
            "fixture: the pointer is live, so the gaze is really on it"
        );
        assert!(
            in_range >= 500,
            "fixture: the pointer sat in eye-contact range ({in_range} frames)"
        );
        assert_eq!(
            flips, 0,
            "a cursor resting on the cat turned it over {flips} times"
        );
    }

    /// Hold the POINTER at `off` cells from a sleeping cat's midline for
    /// `secs`, alive but with its column pinned, and report the frame index it
    /// turned over on (if it ever did).
    fn hover_beside_a_sleeper(
        pet: &mut PetBrain,
        start: Instant,
        at: &PetFrame,
        off: f32,
        secs: f32,
    ) -> Option<u16> {
        let w = art_cols(10, 20);
        let px = at.col + w * 0.5 - 0.5 + off;
        let (mut t, mut turned) = (start, None);
        let frames = (secs / 0.016) as u16;
        for i in 0..frames {
            let py = at.row + 0.8 * (f32::from(i) * 0.016 * TAU * 2.0).sin();
            t += Duration::from_millis(16);
            let g = ptick(pet, t, SLEEPER_CARET, Some((px, py)));
            assert_eq!(g.action, PetAction::Sleep, "fixture: it stays asleep");
            if g.facing_left != at.facing_left && turned.is_none() {
                turned = Some(i);
            }
        }
        assert!(pet.pointer_heat > 0.0, "fixture: the pointer stayed live");
        turned
    }

    /// The third brake, and the proof the other two did not simply nail the
    /// cat's head on. A cursor that COMMITS — parked past the dead band,
    /// behind the cat, and left there — is not an oscillation, and a lightly
    /// sleeping cat does eventually come round to it: that is the drowsy
    /// shift the ruling asked for, not the instant flip it forbade. Past the
    /// breath window it stops even doing that, because a deeply asleep pet is
    /// the one state this module promises is perfectly still, so the frame
    /// settles and the host can idle to zero.
    #[test]
    fn a_light_sleeper_comes_round_slowly_and_a_deep_one_not_at_all() {
        // LIGHT sleep: it turns, but only after the dwell has been paid.
        let (mut pet, t, f) = a_sleeping_cat(true);
        assert!(!f.facing_left, "fixture: asleep facing right");
        let turned = hover_beside_a_sleeper(&mut pet, t, &f, -1.6, 3.0)
            .expect("a light sleeper still notices what stays put");
        assert!(
            f32::from(turned) * 0.016 >= SLEEP_FLIP_DWELL,
            "it came round on frame {turned} — before the dwell was even paid"
        );

        // DEEP sleep: the same cursor, in the same place, for the same time.
        let (mut pet, t, _) = a_sleeping_cat(true);
        let (t, f) = idle(&mut pet, t, SLEEPER_CARET, BREATH_WINDOW + 1.0);
        assert_eq!(f.action, PetAction::Sleep, "fixture: still asleep");
        assert!(
            pet.quiet >= SLEEP_AFTER + BREATH_WINDOW,
            "fixture: past the breath window (quiet {})",
            pet.quiet
        );
        assert!(!f.facing_left, "fixture: still facing right");
        assert_eq!(
            hover_beside_a_sleeper(&mut pet, t, &f, -1.6, 3.0),
            None,
            "a deeply asleep cat is perfectly still — it does not turn over"
        );
    }

    /// The other half of the ruling: the brakes are on the SETTLED gaze, not
    /// on the animal. A sleeping cat still wakes, and the moment it is awake
    /// and walking it faces where it is going on the ordinary
    /// [`FLIP_SPEED`] rule — no dwell, no dead band, no waiting.
    #[test]
    fn the_gaze_brakes_never_reach_the_waking_chase() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 60);
        let (mut t, f) = idle(&mut pet, t, (4, 60), SLEEP_AFTER + 1.0);
        assert_eq!(f.action, PetAction::Sleep, "fixture: a sleeping cat");
        // Whichever way it dozed off, the caret reappears BEHIND it — so the
        // chase can only reach the caret by turning the body round.
        let asleep_facing = f.facing_left;
        let caret = if asleep_facing { 95 } else { 10 };
        let mut turned = None;
        for i in 0..180u16 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, caret))));
            if f.facing_left != asleep_facing && turned.is_none() {
                turned = Some((i, f.action));
            }
        }
        let (frame, action) = turned.expect("it must turn to chase a caret behind it");
        assert!(
            matches!(action, PetAction::Leap | PetAction::Crouch) || !action.settled(),
            "the turn came from travel, not from the settled gaze (got {action:?})"
        );
        assert!(
            frame < 120,
            "and it turned while getting under way, not {frame} frames later"
        );
    }

    /// Drive one straight leftward dash from `from` toward `to` at ~62
    /// cells/s, returning what the pet did along the way.
    fn dash(
        pet: &mut PetBrain,
        start: Instant,
        caret: (u16, u16),
        from: f32,
        to: f32,
    ) -> (Instant, bool, bool) {
        let mut t = start;
        let mut px = from;
        let (mut leapt, mut frolicked) = (false, false);
        while px > to {
            t += Duration::from_millis(16);
            px -= 1.0;
            let f = ptick(pet, t, caret, Some((px, f32::from(caret.0))));
            leapt |= f.action == PetAction::Leap;
            frolicked |= f.action == PetAction::Frolic;
        }
        (t, leapt, frolicked)
    }

    #[test]
    fn a_dashing_pointer_gets_pounced_only_when_content() {
        let start = Instant::now();
        // A COLD cat only watches the dasher.
        let mut cold = PetBrain::default();
        let t = awake(&mut cold, start, 4, 12);
        let (t, _) = idle(&mut cold, t, (4, 14), SIT_AFTER + 0.4);
        assert!(cold.content() < PLAY_CONTENT, "fixture: a cold cat");
        let (_, leapt, frolicked) = dash(&mut cold, t, (4, 14), 90.0, 5.0);
        assert!(!leapt && !frolicked, "a cold cat does not play");
        // A WARM one pounces the dasher and frolics on the landing.
        let mut warm = PetBrain::default();
        let t = warm_settled(&mut warm, start);
        let (t, leapt, _) = dash(&mut warm, t, (4, 44), 90.0, 5.0);
        assert!(leapt, "a happy cat pounces the dasher");
        // The flourish and the walk home play out after the dash ends.
        let mut frolicked = false;
        let mut t2 = t;
        for _ in 0..200 {
            t2 += Duration::from_millis(16);
            let f = ptick(&mut warm, t2, (4, 44), Some((5.0, 4.0)));
            frolicked |= f.action == PetAction::Frolic;
        }
        assert!(frolicked, "and frolics one beat on the landing");
    }

    #[test]
    fn the_cat_walks_home_after_playing() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = warm_settled(&mut pet, start);
        let (t, leapt, _) = dash(&mut pet, t, (4, 44), 90.0, 20.0);
        assert!(leapt, "fixture: the pounce fired");
        // The toy stops; the flourish plays; the chase ladder walks home.
        // Sampled BEFORE boredom's window opens (wave 4b: a content cat at
        // BORED_AFTER of quiet starts demanding attention from the cursor,
        // which is its own pin below).
        let (t, f) = idle(&mut pet, t, (4, 44), 3.5);
        let w = art_cols(10, 20);
        let station = PetBrain::station(44, 100, w);
        assert!(
            (f.col - station).abs() <= ARRIVED + SIT_FRONT_GAP,
            "home by the caret again, got col {} (station {station})",
            f.col
        );
        assert!(f.action.settled(), "and settled, got {:?}", f.action);
        // And then boredom: the same content cat, left alone long enough,
        // turns on the cursor — a bat, an attack, or the wriggle (the
        // serial deals it). The vignette is finite and re-settles.
        let mut t = t;
        let mut acted = false;
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 44))));
            acted |= matches!(f.action, PetAction::Frolic | PetAction::Loaf) || pet.wriggle_t > 0.0;
        }
        assert!(acted, "a bored content cat demands attention");
        assert!(pet.bored_cool > 0.0, "and pays the cooldown for it");
    }

    #[test]
    fn caret_work_outranks_pointer_play() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = warm_settled(&mut pet, start);
        // A latched dash pounce... (the pointer must still be IN the pane:
        // a pointer that left takes its toy with it.)
        pet.pending_pointer_pounce = Some((60.0, 4.0));
        t += Duration::from_millis(16);
        let f = ptick(&mut pet, t, (4, 44), Some((60.0, 4.0)));
        assert_eq!(
            f.action,
            PetAction::Crouch,
            "fixture: the play crouch began"
        );
        // ...interrupted by REAL work: a word jump mid-play.
        let mut frolicked = false;
        for _ in 0..250 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 70))));
            frolicked |= f.action == PetAction::Frolic;
        }
        assert!(
            !frolicked,
            "the caret jump cleared the flourish: work first, no play beat"
        );
        let (_, f) = idle(&mut pet, t, (4, 70), 1.0);
        let w = art_cols(10, 20);
        let station = PetBrain::station(70, 100, w);
        assert!(
            (f.col - station).abs() <= ARRIVED + 0.5,
            "and the pet is at the CARET's station, got {} ({station})",
            f.col
        );
    }

    #[test]
    fn one_pounce_per_dash() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = warm_settled(&mut pet, start);
        // One long dash straight through the pet's range and out the far
        // side: exactly one pounce may fire, however long the drag runs.
        let mut px = 90.0f32;
        let mut t = t;
        let mut crouches = 0u32;
        let mut last = pet.action();
        for _ in 0..300 {
            t += Duration::from_millis(16);
            px = (px - 1.0).max(2.0);
            let f = ptick(&mut pet, t, (4, 44), Some((px, 4.0)));
            if f.action == PetAction::Crouch && last != PetAction::Crouch {
                crouches += 1;
            }
            last = f.action;
        }
        assert_eq!(
            crouches, 1,
            "one pounce per dash: the trigger re-arms only under POUNCE_REARM"
        );
    }

    #[test]
    fn pointer_none_clears_heat() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.4);
        let mut px = 30.0f32;
        for _ in 0..40 {
            t += Duration::from_millis(16);
            px += 0.2;
            let _ = ptick(&mut pet, t, (4, 14), Some((px, 4.0)));
        }
        assert!(pet.pointer_heat > 0.0, "fixture: attention is live");
        t += Duration::from_millis(16);
        let _ = ptick(&mut pet, t, (4, 14), None);
        assert_eq!(
            pet.pointer_heat, 0.0,
            "outside the pane the pointer does not exist"
        );
        assert!(pet.last_pointer.is_none(), "and the sensor forgets it");
    }

    #[test]
    fn reduced_motion_pointer_is_nothing_at_all() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((3, 10))));
        }
        let mut px = 60.0f32;
        for _ in 0..120 {
            t += Duration::from_millis(16);
            px -= 1.0; // a full-speed dash, every frame
            let mut s = sense(t, Some((3, 10)));
            s.reduced_motion = true;
            s.pointer = Some((px.max(2.0), 3.0));
            let f = pet.tick(s);
            assert!(
                matches!(f.action, PetAction::Sit | PetAction::Sleep),
                "no gaze, no pounce under reduced motion, got {:?}",
                f.action
            );
        }
        assert_eq!(pet.pointer_heat, 0.0);
        assert!(pet.pending_pointer_pounce.is_none());
    }

    #[test]
    fn reduced_motion_stream_is_nothing_at_all() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((3, 10))));
        }
        for _ in 0..120 {
            t += Duration::from_millis(16);
            let mut s = sense(t, Some((3, 10)));
            s.reduced_motion = true;
            s.output_burst = true;
            let f = pet.tick(s);
            assert!(
                matches!(f.action, PetAction::Sit | PetAction::Sleep),
                "no perk under reduced motion, got {:?}",
                f.action
            );
        }
        assert_eq!(pet.watch_heat, 0.0, "and no heat is left behind");
    }

    // ── the 0.19.0 gauntlet's ink law (F1/F2/F4/F5/F8/F9) ───────────────

    /// The pet's footprint on its feet row, judged the way the eviction
    /// judges it — the assertion the whole F1 battery leans on.
    fn on_ink(f: &PetFrame, span: (u16, u16), w: f32) -> bool {
        let (first, end) = (f32::from(span.0), f32::from(span.1));
        let (a, b) = (f.col + INK_PAD, f.col + w - INK_PAD);
        b > first && a < end
    }

    /// F1, the systemic root: a settled pose whose ground the world has typed
    /// over re-anchors to blank cells — pose and hold kept, feet moved.
    ///
    /// The feet now move by HOPPING (owner ruling, 2026-08-10), so the "very
    /// next tick" clause of the original had to go: no animation finishes in
    /// 16 ms. What F1 was really buying is intact and is what is asserted
    /// here — the pose does not LOITER on the user's words (it is airborne on
    /// the tick the ink lands, no gather, no dawdling), it ends up on blank
    /// ground, and the settle it was holding is the settle it comes back to.
    #[test]
    fn event_pose_retargets_off_ink() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let w = art_cols(10, 20);
        let mut t = awake(&mut pet, start, 4, 10);
        let (t2, f) = idle(&mut pet, t, (4, 12), SIT_AFTER + 0.4);
        t = t2;
        assert!(f.action.settled(), "fixture: a settled cat");
        // A prompt prints UNDER the sitter: row 4 is now inked through the
        // pet's footprint and well past it (the b05x droop/cheer geometry).
        let inked_to = (f.col + w + 6.0) as u16;
        let spans = vec![(0u16, inked_to); 6];
        pet.sense_ink(0, &spans, Some(4));
        t += Duration::from_millis(16);
        let f1 = pet.tick(sense(t, Some((4, 12))));
        assert_eq!(
            f1.action,
            PetAction::Leap,
            "one tick after the ink invades the pose is already leaving, \
             got {:?}",
            f1.action
        );
        let (_, f2) = idle(&mut pet, t, (4, 12), 1.2);
        assert!(
            !on_ink(&f2, (0, inked_to), w),
            "and the trip ends on blank cells (col {}, ink to {inked_to})",
            f2.col
        );
        assert!(
            f2.action.settled(),
            "and the settle survives the move, got {:?}",
            f2.action
        );
    }

    /// F2: the sleeper's anchor follows the prompt. Typing under a sleeping
    /// cat wakes it — and the wake stretch must play on blank ground, not on
    /// top of the words that grew while it slept (b07_wake_009).
    ///
    /// Post-ruling the wake and the move are two beats instead of one: the
    /// stretch is BORROWED by the hop that carries the cat off the words and
    /// handed back when the paws are down, so the stretch still plays — just
    /// where it can be seen, which is the whole point of b07_wake_009.
    #[test]
    fn the_sleeper_follows_the_prompt() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let w = art_cols(10, 20);
        let t = awake(&mut pet, start, 2, 8);
        let (mut t, f) = idle(&mut pet, t, (2, 10), SLEEP_AFTER + 1.0);
        assert_eq!(f.action, PetAction::Sleep, "fixture: a sleeping cat");
        // The user types the line out right through the pet's bedroom.
        let inked_to = (f.col + w + 2.0) as u16;
        let spans = vec![(0u16, inked_to); 6];
        pet.sense_ink(0, &spans, Some(2));
        t += Duration::from_millis(16);
        let f1 = pet.tick(sense(t, Some((2, inked_to))));
        assert_eq!(
            pet.resume.map(|(a, _)| a),
            Some(PetAction::Waking),
            "typing wakes it, and the hop owes the stretch back"
        );
        assert_eq!(
            f1.action,
            PetAction::Leap,
            "and it is already off the words"
        );
        // The stretch lands where it can be read.
        let mut stretched_clear = false;
        for _ in 0..90 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((2, inked_to))));
            if f.action == PetAction::Waking {
                assert!(
                    !on_ink(&f, (0, inked_to), w),
                    "the stretch plays OFF the ink, not atop the fresh words \
                     (col {}, ink to {inked_to})",
                    f.col
                );
                stretched_clear = true;
            }
        }
        assert!(stretched_clear, "the borrowed stretch must actually play");
    }

    /// F2's frame-lane rider, RE-DECIDED (owner ruling, 2026-08-10).
    ///
    /// This test used to assert that a DEEP sleeper re-anchors instantly and
    /// silently, on the reasoning that anchor math without animation is free
    /// and a released frame lane should stay released. The sleeper is the one
    /// case where the new law costs something real, so: it was decided
    /// AGAINST the exemption, deliberately, because
    ///
    ///  * a deep sleeper is not invisible, it is merely still — the host has
    ///    it fully drawn, and the instant it is re-anchored the next repaint
    ///    shows it somewhere else. The teleport the owner judged on glass is
    ///    exactly as visible on a sleeping cat as on a sitting one;
    ///  * the lane is a question about MOTION, and the honest answer once
    ///    there is motion to draw is `true`. Its cost is bounded by the arc
    ///    ([`FLIGHT_MAX`] plus its landing) and is paid on a screen that is
    ///    already repainting, because the only thing that fires this eviction
    ///    is output arriving under the cat;
    ///  * an exemption here would carve the law's hole in precisely the case
    ///    the owner sees most — a log stream printing under a long-idle cat.
    ///
    /// What the test still owns, and the reason it is worth keeping, is that
    /// the lane comes BACK: a re-anchor may not leave a sleeping cat pinning
    /// the frame lane forever.
    #[test]
    fn a_deep_sleep_re_anchor_hops_then_gives_the_lane_back() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let w = art_cols(10, 20);
        let t = awake(&mut pet, start, 3, 10);
        let (mut t, f) = idle(&mut pet, t, (3, 12), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert_eq!(f.action, PetAction::Sleep);
        assert!(!pet.needs_frames(), "fixture: deep sleep, lane released");
        // Output inks the sleeper's row without moving the caret (a stream
        // under a parked prompt).
        let inked_to = (f.col + w + 4.0) as u16;
        let spans = vec![(0u16, inked_to); 6];
        pet.sense_ink(0, &spans, Some(3));
        t += Duration::from_millis(16);
        let f2 = pet.tick(sense(t, Some((3, 12))));
        assert_eq!(
            f2.action,
            PetAction::Leap,
            "a shoved cat gets up, got {:?}",
            f2.action
        );
        assert!(pet.needs_frames(), "and the hop is owed frames to draw it");
        // …and it goes straight back to sleep on the far side, with the lane
        // released again — the deep sleeper's idle-to-zero is intact.
        let (_, f3) = idle(&mut pet, t, (3, 12), 1.5);
        assert_eq!(f3.action, PetAction::Sleep, "asleep again");
        assert!(
            !on_ink(&f3, (0, inked_to), w),
            "re-anchored off the ink (col {})",
            f3.col
        );
        assert!(
            !pet.needs_frames(),
            "and the lane is released again once the paws are down"
        );
    }

    /// THE OWNER'S REPORT, 2026-08-10: "the cursor kitty gets pushed around
    /// in the text when editing in the middle of the text."
    ///
    /// The mechanism, isolated: the caret does not move at all, and only the
    /// row's ink END grows — which is exactly what inserting a character
    /// mid-sentence does to every column to its right. Under the old law the
    /// station WAS `end + INK_GAP`, so the cat was towed a full cell right per
    /// keystroke by text it was nowhere near. The ladder's rule 3 seats it
    /// beside the caret on the blank row below instead, and a growing end is
    /// then none of its business.
    #[test]
    fn a_growing_line_never_tows_the_cat_along_behind_it() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 12);
        // Row 4 is a sentence; the rows around it are blank, as they are at
        // any prompt you are editing.
        let mut spans = vec![(0u16, 0u16); 8];
        spans[4] = (0, 30);
        pet.sense_ink(0, &spans, Some(4));
        let (t2, settled) = idle(&mut pet, t, (4, 12), 1.2);
        t = t2;
        assert!(
            settled.col < 20.0,
            "the cat stations beside the CARET, not past the end of the line \
             (col {}, line ends at 30)",
            settled.col
        );
        assert!(
            (settled.row - 5.0).abs() < 0.01,
            "and it takes the blank row under the sentence, got row {}",
            settled.row
        );

        // Now insert three characters ahead of the caret: the end marches,
        // the caret does not.
        let anchor = settled.col;
        for end in [31u16, 32, 33] {
            spans[4] = (0, end);
            pet.sense_ink(0, &spans, Some(4));
            let (t3, f) = idle(&mut pet, t, (4, 12), 0.5);
            t = t3;
            assert!(
                (f.col - anchor).abs() < 0.5,
                "a longer line must not move a cat whose caret stood still \
                 (col {} vs {anchor} when the line reached {end})",
                f.col
            );
        }
    }

    /// The ladder's rule 4 is still there: with the neighbouring rows inked
    /// too — a screen full of text — there is no blank row to step down to,
    /// and standing beside the paragraph really is the best ground on offer.
    /// This is the clause `event_pose_retargets_off_ink` and
    /// `the_sleeper_follows_the_prompt` exercise from the pose side; here it
    /// is read straight off the ladder so the ORDER of the two rules is
    /// pinned, not just their endpoints.
    #[test]
    fn the_ink_ladder_prefers_a_blank_row_to_a_distant_stand() {
        let mut pet = PetBrain::default();
        let w = art_cols(10, 20);
        let mut spans = vec![(0u16, 0u16); 8];
        spans[4] = (0, 30);
        pet.sense_ink(0, &spans, Some(4));
        // Rule 3: one row down, same column.
        assert_eq!(
            pet.ink_stand(13.0, 4.0, w, 100, 30),
            (13.0, 5.0),
            "a blank row below beats a stand 17 cells away"
        );
        // Rule 2 is still ahead of it: ink that ends just past the want is a
        // step aside, not a reason to change rows.
        spans[4] = (0, 14);
        pet.sense_ink(0, &spans, Some(4));
        assert_eq!(
            pet.ink_stand(13.0, 4.0, w, 100, 30),
            (14.0 + INK_GAP, 4.0),
            "a near stand is cheaper than a row change"
        );
        // Rule 4: every row inked, so there is nowhere to step down to.
        let spans = vec![(0u16, 30u16); 8];
        pet.sense_ink(0, &spans, Some(4));
        assert_eq!(
            pet.ink_stand(13.0, 4.0, w, 100, 30),
            (30.0 + INK_GAP, 4.0),
            "a full screen still evicts beside the paragraph"
        );
        // Rule 1 is untouched by all of it.
        assert_eq!(
            pet.ink_stand(40.0, 4.0, w, 100, 30),
            (40.0, 4.0),
            "blank ground under the want is answered first and alone"
        );
    }

    /// A line WRAP is one character. Read literally off the grid it is the
    /// biggest move a caret can make, and it used to clear both the pounce
    /// bar and the screen-crossing bar — so typing past the right margin, or
    /// backspacing over the seam, fired a gather-and-bound every time.
    #[test]
    fn a_wrap_is_one_character_not_a_screen_crossing_jump() {
        // Forward: the caret falls off the right margin onto the next row.
        assert_eq!(
            PetBrain::caret_delta((5, 99), (6, 0), 100, false),
            (0.0, 1.0),
            "typing past the margin moved the text on by one"
        );
        // Backward over the same seam — and the row folds to zero, which is
        // what hands it to the retreat arm instead of the jump ladder.
        assert_eq!(
            PetBrain::caret_delta((6, 0), (5, 99), 100, false),
            (0.0, -1.0),
            "backspacing over the seam deleted one"
        );
        // A double-width glyph wraps from cols - 2; WRAP_SLOP covers it.
        assert_eq!(
            PetBrain::caret_delta((5, 98), (6, 0), 100, false),
            (0.0, 2.0)
        );
        // NEGATIVE CONTROLS — everything else keeps its literal delta.
        assert_eq!(
            PetBrain::caret_delta((10, 5), (2, 70), 100, false),
            (-8.0, 65.0),
            "a real row jump is not a seam"
        );
        assert_eq!(
            PetBrain::caret_delta((5, 2), (6, 70), 100, false),
            (1.0, 68.0),
            "down AND right is not a wrap in either direction"
        );
        assert_eq!(
            PetBrain::caret_delta((5, 16), (6, 1), 20, false),
            (1.0, -15.0),
            "a 15-cell move in a 20-wide pane is a jump, not a seam"
        );
    }

    /// The same seam, driven through the whole brain — and PAST the wake,
    /// this time. The old assertion landed ~0.25 s into a fixture whose wake
    /// stretch holds the ladder shut for [`WAKE_DUR`] = 0.62 s, so
    /// `action != Crouch` was true of a cat that could not have crouched at
    /// anything. Repaired to bite: the completed line's INK lands with the
    /// wrap (a full line is what a margin wrap IS), the clock runs seconds
    /// past the wake, and the WHOLE history is judged — no travel latch on
    /// any frame (the fold), no gathered Crouch ever (the crossing is the
    /// shoved re-anchor, which launches at once; a Crouch is the
    /// standing-gap ceremony's signature), the cat genuinely across and
    /// settled, and no trailing arc after it settles (the wall-transit
    /// tail).
    #[test]
    fn typing_past_the_margin_does_not_launch_a_bound() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        // Slow prose at the margin: key gaps past RHYTHM_WINDOW keep the
        // keep-ahead prediction out of the flight aim, so the wrap's answer
        // is the plain geometry under test.
        let mut t = start;
        let _ = pet.tick(sense(t, Some((5, 95))));
        for k in 1..=4u16 {
            let until = t + Duration::from_secs_f32(0.7);
            while t < until {
                t += Duration::from_millis(16);
                let _ = pet.tick(sense(t, Some((5, 95 + k - 1))));
            }
            t = until;
            let _ = pet.tick(sense(t, Some((5, 95 + k))));
        }
        // A beat's pause at the full line, so the wrap lands on a grounded
        // cat rather than mid-stride (a walking cat is travel, and travel
        // is exempt from the eviction by design).
        let hold = t + Duration::from_secs_f32(0.5);
        while t < hold {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((5, 99))));
        }
        // …and the next keystroke wraps: the caret lands on (6, 0), and the
        // line it completed is ink under the pet from this frame on.
        let mut spans = vec![(0u16, 0u16); 8];
        spans[5] = (0, 100);
        spans[6] = (0, 1);
        let mut startled = false;
        let mut crouched = false;
        let mut settled_at: Option<Instant> = None;
        let mut tail_leap = false;
        let mut last = None;
        let end = t + Duration::from_secs_f32(3.2);
        while t < end {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, Some(6));
            let f = pet.tick(sense(t, Some((6, 0))));
            assert!(
                !pet.pending_big_jump && !pet.pending_pounce,
                "the wrap latched travel: big={} pounce={}",
                pet.pending_big_jump,
                pet.pending_pounce
            );
            startled |= f.action == PetAction::Startle;
            crouched |= f.action == PetAction::Crouch;
            match settled_at {
                None if f.action.settled() => settled_at = Some(t),
                Some(at)
                    if f.action == PetAction::Leap && t.duration_since(at).as_secs_f32() < 0.8 =>
                {
                    tail_leap = true;
                }
                _ => {}
            }
            last = Some(f);
        }
        let f = last.expect("the loop ran");
        assert!(!startled, "a wrap is not a retreat");
        assert!(
            !crouched,
            "nothing gathered to leap: a wrap's crossing is the shoved \
             re-anchor, never the standing-gap ceremony"
        );
        assert!(
            settled_at.is_some(),
            "the crossing must finish and hand the pose back"
        );
        assert!(!tail_leap, "no wall-transit tail after the crossing");
        assert!(
            (f.row - 6.0).abs() < 0.01 && f.col < 8.0,
            "and the pet crossed with the wrap, got ({}, {})",
            f.col,
            f.row
        );
    }

    /// THE SCROLLED WRAP IS A WRAP — the seam no heuristic can see. At the
    /// bottom of a scrolling pane a wrap SCROLLS: the caret's row never
    /// changes, so the delta reads as a 99-column retreat, byte-identical to
    /// `Home` pressed at the last column — and the pet bottled up in fright
    /// on every line a shell printed past the margin. Only the emulator's
    /// fact ([`PetSense::wrapped`]) separates the two.
    ///
    /// Unit half: the fact-gated fold answers with the one character the
    /// text really moved, and only for a delta shaped like the seam. Brain
    /// half: a scripted bottom-row scrolled wrap under a live stream shows
    /// NO Startle and NO big-jump show — the watch holds the ground (its
    /// station override is what keeps the standing-gap door shut), and the
    /// fold plugs the one leak upstream of it: `on_move` fired the bottle
    /// before the watch could say a word. The control run drives the
    /// identical script with the fact withheld and DEMANDS the bottle, so
    /// this test cannot pass vacuously.
    #[test]
    fn a_scrolled_wrap_is_a_wrap() {
        assert_eq!(
            PetBrain::caret_delta((29, 99), (29, 0), 100, true),
            (1.0, 1.0),
            "a scrolled wrap is one character on"
        );
        assert_eq!(
            PetBrain::caret_delta((29, 99), (29, 0), 100, false),
            (0.0, -99.0),
            "without the fact the delta stays literal — the fact, never a \
             looser heuristic, is what tells the seam from Home"
        );
        assert_eq!(
            PetBrain::caret_delta((29, 40), (29, 0), 100, true),
            (0.0, -40.0),
            "the shared bar still guards the fact arm: a mid-line Home in a \
             frame that also wrapped keeps its literal retreat"
        );

        let script = |fact: bool| -> Vec<PetAction> {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            // A settled cat near the end of the bottom row…
            let mut t = awake(&mut pet, start, 29, 90);
            // …while the shell streams a long line toward the margin. The
            // rows above hold the output that already printed — they are
            // what keeps the ink ladder from re-seating the cat off its
            // row — and the watch warms on the burst.
            let mut spans = vec![(0u16, 0u16); 30];
            spans[27] = (0, 100);
            spans[28] = (0, 100);
            let mut caret = 92u16;
            while caret < 99 {
                caret += 1;
                for _ in 0..3 {
                    t += Duration::from_millis(16);
                    spans[29] = (0, caret);
                    pet.sense_ink(0, &spans, Some(29));
                    let mut s = sense(t, Some((29, caret)));
                    s.output_burst = true;
                    let _ = pet.tick(s);
                }
            }
            // The line lingers at the margin a beat (the shell flushing),
            // so any wall-hop theatrics from the approach have landed and
            // the wrap arrives at a GROUNDED watcher in both runs — a
            // startle fired mid-air is swallowed by the flight, and the
            // control below must be able to see the bottle.
            for _ in 0..50 {
                t += Duration::from_millis(16);
                spans[29] = (0, 100);
                pet.sense_ink(0, &spans, Some(29));
                let mut s = sense(t, Some((29, 99)));
                s.output_burst = true;
                let _ = pet.tick(s);
            }
            // THE WRAP: the long line scrolls up a row, the caret lands on
            // column 0 of the same bottom row, and the fact rides exactly
            // one host frame.
            let mut actions = Vec::new();
            t += Duration::from_millis(16);
            spans[28] = (0, 100);
            spans[29] = (0, 1);
            pet.sense_ink(0, &spans, Some(29));
            let mut s = sense(t, Some((29, 0)));
            s.output_burst = true;
            s.wrapped = fact;
            actions.push(pet.tick(s).action);
            // …and the next line begins to print.
            for i in 1..=45u16 {
                t += Duration::from_millis(16);
                let col = (i / 3).clamp(1, 15);
                spans[29] = (0, col);
                pet.sense_ink(0, &spans, Some(29));
                let mut s = sense(t, Some((29, col)));
                s.output_burst = true;
                actions.push(pet.tick(s).action);
            }
            actions
        };

        let with_fact = script(true);
        assert!(
            !with_fact.contains(&PetAction::Startle),
            "the fact folds the seam — no bottle on a scrolled wrap, got \
             {with_fact:?}"
        );
        assert!(
            !with_fact.contains(&PetAction::Crouch) && !with_fact.contains(&PetAction::Leap),
            "and no big-jump show: the watch keeps the ground and the fold \
             keeps the latch, got {with_fact:?}"
        );
        let blind = script(false);
        assert!(
            blind.contains(&PetAction::Startle),
            "control: without the fact the same script must bottle up — \
             otherwise this test asserts nothing, got {blind:?}"
        );
    }

    /// §0.1's MISSING LAW, test-side. The `debug_assert` in `begin_arc`
    /// vanishes from a release build, so the formerly-red launch shapes are
    /// driven through the real tick ladder here and every arc they launch is
    /// judged at launch time against the law: `span / dur` within
    /// [`HOP_SPEED_MAX`], or [`BIG_HOP_SPEED_MAX`] for a bound — never an
    /// exemption for `big`. Before the span caps these shapes measured
    /// 455.7 c/s (the ink re-anchor across a typed-through wrap), 137+ c/s
    /// (the span-blind wall transit), 95+ c/s (the pounce door as an open
    /// half-line), and 135.1 c/s (the ratified split bound — lawful under
    /// its own bar, and the shape that DERIVES that bar).
    ///
    /// The caret is parked during every flight, so no mid-air re-aim ever
    /// rebases a launch point: each live `Flight` still carries exactly the
    /// rate `begin_arc` chose.
    #[test]
    fn no_flight_may_outrun_the_animal_that_flies_it() {
        /// Judge every live arc, count fresh launches, remember the fastest.
        struct Law {
            in_flight: bool,
            launches: u32,
            fastest: f32,
        }
        impl Law {
            fn new() -> Self {
                Law {
                    in_flight: false,
                    launches: 0,
                    fastest: 0.0,
                }
            }
            fn judge(&mut self, pet: &PetBrain) {
                match pet.flight {
                    Some(f) => {
                        let rate = (f.to_col - f.from_col).abs() / f.dur;
                        let bound = if f.big {
                            BIG_HOP_SPEED_MAX
                        } else {
                            HOP_SPEED_MAX
                        };
                        assert!(
                            rate <= bound + 1e-3,
                            "a launched arc outruns the animal that flies it: \
                             {rate:.1} c/s against {bound:.0} (big={})",
                            f.big
                        );
                        if !self.in_flight {
                            self.launches += 1;
                            self.fastest = self.fastest.max(rate);
                        }
                        self.in_flight = true;
                    }
                    None => self.in_flight = false,
                }
            }
        }
        let wide = |now: Instant, caret: Option<(u16, u16)>| PetSense {
            cols: 200,
            ..sense(now, caret)
        };
        /// Wake and settle a fresh pet on the 200-column pane.
        fn awake_wide(pet: &mut PetBrain, start: Instant, row: u16, col: u16) -> Instant {
            let wide = |now: Instant, caret: (u16, u16)| PetSense {
                cols: 200,
                ..sense(now, Some(caret))
            };
            let mut t = start;
            let _ = pet.tick(wide(t, (row, col)));
            for k in 1..=2u16 {
                let until = t + Duration::from_secs_f32(0.05);
                while t < until {
                    t += Duration::from_millis(16);
                    let _ = pet.tick(wide(t, (row, col + k - 1)));
                }
                t = until;
                let _ = pet.tick(wide(t, (row, col + k)));
            }
            let settle = t + Duration::from_secs_f32(SIT_AFTER + 0.4);
            while t < settle {
                t += Duration::from_millis(16);
                let _ = pet.tick(wide(t, (row, col + 2)));
            }
            t
        }
        let start = Instant::now();

        // (1) THE TYPED-THROUGH WRAP'S INK RE-ANCHOR at cols = 200 — the
        // measured 455.7 c/s streak, now routed through `launch_big` and
        // split. The screen is dense (the rows around the pair are inked),
        // so the ink ladder cannot re-seat the cat off its row pre-wrap.
        let mut pet = PetBrain::default();
        let mut law = Law::new();
        let mut t = start;
        let _ = pet.tick(wide(t, Some((6, 195))));
        let mut spans = vec![(0u16, 0u16); 12];
        for k in 1..=4u16 {
            let until = t + Duration::from_secs_f32(0.7);
            while t < until {
                t += Duration::from_millis(16);
                spans[5] = (0, 200);
                spans[6] = (0, 195 + k);
                spans[7] = (0, 200);
                pet.sense_ink(0, &spans, Some(6));
                let _ = pet.tick(wide(t, Some((6, 195 + k - 1))));
                law.judge(&pet);
            }
            t = until;
            let _ = pet.tick(wide(t, Some((6, 195 + k))));
            law.judge(&pet);
        }
        let hold = t + Duration::from_secs_f32(0.5);
        while t < hold {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, Some(6));
            let _ = pet.tick(wide(t, Some((6, 199))));
            law.judge(&pet);
        }
        // The wrap: the pair completes, the fresh line opens below.
        spans[6] = (0, 200);
        spans[7] = (0, 1);
        let end = t + Duration::from_secs_f32(3.0);
        while t < end {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, Some(7));
            let _ = pet.tick(wide(t, Some((7, 0))));
            law.judge(&pet);
        }
        assert!(
            law.launches >= 2 && law.fastest > HOP_SPEED_MAX,
            "fixture: the wrap crossing must fly, split, and fly faster than \
             any ordinary hop may ({} launches, fastest {:.1})",
            law.launches,
            law.fastest
        );

        // (2) THE WALL TRANSIT across a wide span: the fixed-duration arc
        // was span-blind. A settled wall-side cat whose caret leaps 42
        // columns back re-crosses in a capped hop, and the remainder falls
        // to the pounce band on later ticks.
        let mut pet = PetBrain::default();
        let mut law = Law::new();
        let mut t = awake(&mut pet, start, 6, 95);
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((6, 55))));
        law.judge(&pet);
        let end = t + Duration::from_secs_f32(3.0);
        while t < end {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((6, 55))));
            law.judge(&pet);
        }
        assert!(
            law.launches >= 2,
            "fixture: the capped transit hop and the band pounce must both \
             fly, got {}",
            law.launches
        );

        // (3) THE POUNCE at cols = 200: the door used to be an open
        // half-line, launching a 40-cell standing gap inside FLIGHT_MAX.
        // Now it declines past the band, the follower gallops the distance
        // down, and the pounce fires only once a pounce can be flown.
        let mut pet = PetBrain::default();
        let mut law = Law::new();
        let mut t = awake_wide(&mut pet, start, 6, 8);
        t += Duration::from_millis(16);
        let _ = pet.tick(wide(t, Some((6, 50))));
        law.judge(&pet);
        let end = t + Duration::from_secs_f32(4.0);
        let mut final_col = 0.0f32;
        while t < end {
            t += Duration::from_millis(16);
            final_col = pet.tick(wide(t, Some((6, 50)))).col;
            law.judge(&pet);
        }
        assert!(
            law.launches >= 1 && law.fastest <= HOP_SPEED_MAX + 1e-3,
            "fixture: the gap must close with a lawful pounce ({} launches, \
             fastest {:.1})",
            law.launches,
            law.fastest
        );
        assert!(
            (final_col - 51.0).abs() < 2.5,
            "and the cat still arrives, got col {final_col}"
        );

        // (4) THE SPLIT BOUND — the shape that derives the big bar: its
        // first bound must fly faster than any ordinary hop and still land
        // under BIG_HOP_SPEED_MAX.
        let mut pet = PetBrain::default();
        let mut law = Law::new();
        let mut t = awake_wide(&mut pet, start, 6, 4);
        t += Duration::from_millis(16);
        let _ = pet.tick(wide(t, Some((6, 199))));
        law.judge(&pet);
        let end = t + Duration::from_secs_f32(4.0);
        while t < end {
            t += Duration::from_millis(16);
            let _ = pet.tick(wide(t, Some((6, 199))));
            law.judge(&pet);
        }
        assert!(
            law.launches >= 2
                && law.fastest > HOP_SPEED_MAX
                && law.fastest <= BIG_HOP_SPEED_MAX + 1e-3,
            "fixture: the crossing must split into bounds that outrun a hop \
             yet obey the bound's own bar ({} launches, fastest {:.1})",
            law.launches,
            law.fastest
        );
    }

    /// A caret that blinks out for ONE frame — a scrollback scroll, a DECTCEM
    /// hide — used to teleport a fully-drawn cat: the no-caret arm forgets
    /// `last_caret`, and the first-sighting seed then hard-assigned position
    /// with no regard for whether the pet was still on the glass. Measured on
    /// the real brain: `(col 31.30, row 20) → (col 4.00, row 8)` between two
    /// frames, at alpha 241/255.
    #[test]
    fn a_one_frame_caret_hide_never_teleports_a_visible_cat() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 20, 60);
        let (t2, before) = idle(&mut pet, t, (20, 60), 0.5);
        t = t2;
        assert!(before.alpha > 200, "fixture: a cat plainly on the glass");

        // One frame with the cursor hidden…
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, None));
        // …and it comes back somewhere else entirely.
        t += Duration::from_millis(16);
        let after = pet.tick(sense(t, Some((8, 4))));
        assert!(
            after.alpha > 150,
            "one hidden frame must not erase the cat (alpha {})",
            after.alpha
        );
        assert!(
            (after.col - before.col).abs() < 1.0 && (after.row - before.row).abs() < 1.0,
            "a cat the eye can still see has to travel, not blink: \
             ({}, {}) → ({}, {})",
            before.col,
            before.row,
            after.col,
            after.row
        );
    }

    /// A cursor hide can leave the fading resident on glass, so retiring a live
    /// arc may not move that body to its far landing while it is still opaque.
    /// The authored landing is deferred until the fade is completely dark.
    #[test]
    fn a_caret_withheld_mid_flight_holds_its_fade_and_lands_when_dark() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 0, 10);
        let width = art_cols(10, 20);
        let return_caret = (0, 69);
        let target_col = PetBrain::station(return_caret.1, 100, width);
        let target_row = f32::from(return_caret.0);

        // Seed through the production flight primitive, then advance into the
        // arc. The distance-to-target assertion keeps the retirement check
        // non-vacuous: this is not a flight already at its landing frame.
        // 0.45, not 0.40: this fixture hand-builds an arc rather than going
        // through `flight_shape`, and a 56.7-cell span in 0.40 s is 141.8 c/s
        // — past `BIG_HOP_SPEED_MAX` and refused by the launch law (§0.1).
        // The extra 50 ms only moves the 100 ms sample EARLIER in the arc, so
        // the mid-flight assertions below hold with more room, not less.
        pet.begin_arc(target_col, target_row, 0.45, 1.0, true);
        pet.bound2 = Some((target_col + 5.0, target_row));
        t += Duration::from_millis(100);
        let mid = pet.tick(sense(t, Some((0, 12))));
        let live = pet.flight.expect("the fixture is genuinely mid-flight");
        assert!(live.t > 0.0 && live.t < live.dur);
        assert!(
            (mid.col - live.to_col).abs() > 10.0,
            "fixture reached the landing instead of the middle: {} -> {}",
            mid.col,
            live.to_col
        );

        // The first withheld frame retires every live arc debt but keeps the
        // visible feet where the previous frame authored them. Every remaining
        // fade frame must hold there; the far destination becomes internal
        // state only on the zero-alpha frame where it cannot pop on glass.
        let mut hidden_frames = 0;
        loop {
            t += Duration::from_millis(16);
            let hidden = pet.tick(sense(t, None));
            hidden_frames += 1;
            assert!(
                pet.flight.is_none(),
                "a hidden resident retained its old arc"
            );
            assert!(
                pet.bound2.is_none(),
                "a hidden resident retained a second bound"
            );
            if hidden.alpha > 0 {
                assert!((hidden.col - mid.col).abs() < 1e-4);
                assert!(
                    (hidden.row - hidden.lift - (mid.row - mid.lift)).abs() < 1e-4,
                    "withholding the caret moved the visible feet: row/lift \
                     ({}, {}) -> ({}, {})",
                    mid.row,
                    mid.lift,
                    hidden.row,
                    hidden.lift
                );
                assert_eq!(pet.deferred_hidden_landing, Some((target_col, target_row)));
            } else {
                assert!((pet.col - target_col).abs() < 1e-4);
                assert!((pet.row - target_row).abs() < 1e-4);
                assert!(pet.deferred_hidden_landing.is_none());
                break;
            }
            assert!(hidden_frames < 64, "the bounded fade must reach zero");
        }
        assert!(
            hidden_frames > 1,
            "the fixture inspected live hidden frames"
        );

        // A later cold sighting uses the invisible landing and never revives
        // the retired arc.
        t += Duration::from_millis(16);
        let returned = pet.tick(sense(t, Some(return_caret)));
        assert!(pet.flight.is_none());
        assert_ne!(returned.action, PetAction::Leap);
        assert!((returned.col - target_col).abs() < 1e-4);
        assert!((returned.row - target_row).abs() < 1e-4);
    }

    #[test]
    fn a_quick_caret_return_cancels_the_deferred_hidden_landing() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 0, 10);
        let width = art_cols(10, 20);
        let return_caret = (0, 69);
        let target_col = PetBrain::station(return_caret.1, 100, width);
        let target_row = f32::from(return_caret.0);

        // 0.45, not 0.40: this fixture hand-builds an arc rather than going
        // through `flight_shape`, and a 56.7-cell span in 0.40 s is 141.8 c/s
        // — past `BIG_HOP_SPEED_MAX` and refused by the launch law (§0.1).
        // The extra 50 ms only moves the 100 ms sample EARLIER in the arc, so
        // the mid-flight assertions below hold with more room, not less.
        pet.begin_arc(target_col, target_row, 0.45, 1.0, true);
        t += Duration::from_millis(100);
        let mid = pet.tick(sense(t, Some((0, 12))));
        assert!(pet.flight.is_some());
        assert!((mid.col - target_col).abs() > 10.0);

        let hidden = pet.tick(sense(t, None));
        assert!(hidden.alpha > 0);
        assert!((hidden.col - mid.col).abs() < 1e-4);
        assert!(
            (hidden.row - hidden.lift - (mid.row - mid.lift)).abs() < 1e-4,
            "the first hidden frame must retain the airborne feet baseline"
        );
        assert_eq!(pet.deferred_hidden_landing, Some((target_col, target_row)));
        assert!(
            pet.needs_frames(),
            "a zero-elapsed hide must retain fade/landing scheduler ownership"
        );

        // The caret returns before the fade is dark. The deferred invisible
        // landing loses authority: the still-visible body continues from its
        // held position and the normal follower owns any new trip.
        t += Duration::from_millis(16);
        let returned = pet.tick(sense(t, Some(return_caret)));
        assert!(pet.deferred_hidden_landing.is_none());
        assert!((returned.col - hidden.col).abs() < 1.0);
        assert!(
            (returned.row - returned.lift - (hidden.row - hidden.lift)).abs() < 1.0,
            "quick return must resume from the fading feet baseline"
        );
        assert!(
            (returned.col - target_col).abs() > 10.0,
            "a quick return teleported to the deferred landing"
        );
        if let Some(flight) = pet.flight {
            assert!((flight.from_col - hidden.col).abs() < 1.0);
        }

        // The visual custody is finite after cancellation: every live tick
        // owns its settle, and the retired lift is gone within FADE_IN rather
        // than pinning cadence or leaving the pet floating forever.
        let mut settled = false;
        for _ in 0..32 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some(return_caret)));
            if pet.retired_flight_lift.is_none() {
                settled = true;
                break;
            }
            assert!(
                pet.needs_frames(),
                "retired lift must keep cadence until it reaches zero"
            );
        }
        assert!(settled, "retired lift must settle within FADE_IN");
    }

    /// The step aside is a little follower; the RE-ANCHOR is a hop.
    ///
    /// This test used to assert the opposite of its second half — that a
    /// displacement past [`INK_EVICT_MAX`] landed in ONE frame, on the
    /// argument that sliding a grounded pose across the glass on its belly
    /// was a worse lie than the cut. The owner judged the cut on glass and
    /// overruled it (2026-08-10: "the curso kitty sometimes teleports if the
    /// text line suddenly changes… when the kitty teleports without an
    /// animation, it looks bad"). The belly-slide was never the only other
    /// answer: the pet has owned a way to change position since the Enter
    /// hop, and the re-anchor now uses it.
    #[test]
    fn a_step_aside_glides_and_a_re_anchor_hops() {
        let dt = 1.0 / 60.0;
        let step = INK_EVICT_SPEED * dt;
        let mid = PetBrain::evict_toward(10.0, 13.0, dt);
        assert!(
            (mid - (10.0 + step)).abs() < 1e-4,
            "a 3-cell sidestep is rationed, got {mid}"
        );
        assert!(
            (PetBrain::evict_toward(10.0, 10.05, dt) - 10.05).abs() < 1e-6,
            "and never overshoots the last fraction of a cell"
        );
        // The routing law itself: what the glide is allowed to be asked to do.
        assert!(
            !PetBrain::is_re_anchor(10.0, 13.0, 4.0, 4.0),
            "a 3-cell sidestep on one row is a glide"
        );
        assert!(
            PetBrain::is_re_anchor(10.0, 40.0, 4.0, 4.0),
            "past INK_EVICT_MAX it is a hop"
        );
        assert!(
            PetBrain::is_re_anchor(40.0, 10.0, 4.0, 4.0),
            "in either direction"
        );
        assert!(
            PetBrain::is_re_anchor(10.0, 10.0, 4.0, 5.0),
            "and a row change is a hop however short the column move"
        );
    }

    /// …and on the real brain: the eviction that used to CUT now leaves the
    /// ground. A settled cat with a full-width line printed under it is
    /// airborne on the tick the ink lands, and every frame of the trip is a
    /// step a cat could take.
    #[test]
    fn a_re_anchor_leaves_the_ground_instead_of_cutting() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let w = art_cols(10, 20);
        let mut t = awake(&mut pet, start, 6, 58);
        let (t2, before) = idle(&mut pet, t, (6, 60), SIT_AFTER + 0.4);
        t = t2;
        assert!(before.action.settled(), "fixture: a settled cat");
        // A wide block of output prints across the sitter's row AND its
        // neighbours, so the ladder has to reach past the paragraph — the F1
        // re-anchor at range, with the caret never moving (which is what
        // keeps the big-jump choreography out of this).
        let spans = vec![(0u16, 80u16); 12];
        pet.sense_ink(0, &spans, Some(6));
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((6, 60))));
        assert_eq!(
            f.action,
            PetAction::Leap,
            "the re-anchor leaves the ground, got {:?}",
            f.action
        );
        assert!(
            (f.col - before.col).abs() < 6.0,
            "and it is still near where it was on the first frame of the trip \
             ({} → {})",
            before.col,
            f.col
        );
        // It gets there, and it gets its pose back with its clock.
        let (_, after) = idle(&mut pet, t, (6, 60), 1.2);
        assert!(
            !on_ink(&after, (0, 80), w),
            "the trip ends on blank ground (col {})",
            after.col
        );
        assert!(
            after.action.settled(),
            "and the borrowed pose is handed back, got {:?}",
            after.action
        );
    }

    // ── THE LAW: nothing moves without moving ──────────────────────────

    /// One 60 fps frame, judged against the law before it is handed back.
    ///
    /// `last` carries the previous frame's position AND the previous frame's
    /// grid; the grid is part of it because a resize moves the coordinate
    /// space, not the cat, and the viewport clamp in `emit` may take back
    /// exactly as much as the space moved.
    ///
    /// The ceiling is the pet's fastest LEGAL motion for this frame:
    /// [`MAX_SPEED`] on the ground, and — while a flight owns the position —
    /// that arc's own interpolation rate, read on BOTH sides of the tick
    /// because the frame an arc finishes on is still a flown frame.
    fn lawful_tick(pet: &mut PetBrain, s: PetSense, last: &mut (f32, f32, u16, u16)) -> PetFrame {
        const DT: f32 = 0.016;
        let (c0, r0, cols0, rows0) = *last;
        let w = art_cols(s.cell_w, s.cell_h);
        let rate = |fl: Option<Flight>| {
            fl.map_or((0.0, 0.0), |f| {
                (
                    (f.to_col - f.from_col).abs() / f.dur,
                    (f.to_row - f.from_row).abs() / f.dur,
                )
            })
        };
        let (pre_c, pre_r) = rate(pet.flight);
        let f = pet.tick(s);
        let (post_c, post_r) = rate(pet.flight);
        let give_col = ((f32::from(cols0) - w).max(0.0) - (f32::from(s.cols) - w).max(0.0)).abs();
        let give_row =
            (f32::from(rows0.saturating_sub(1)) - f32::from(s.rows.saturating_sub(1))).abs();
        let col_bound = MAX_SPEED.max(pre_c).max(post_c) * DT + give_col + 1e-3;
        // Nothing on the GROUND may change row: a row change is always a hop,
        // so the only row budget is an arc's.
        let row_bound = pre_r.max(post_r) * DT + give_row + 1e-3;
        assert!(
            (f.col - c0).abs() <= col_bound,
            "the cat teleported {} cells sideways in one frame (bound {col_bound}): \
             col {c0} → {}, action {:?}",
            (f.col - c0).abs(),
            f.col,
            f.action
        );
        assert!(
            (f.row - r0).abs() <= row_bound,
            "the cat teleported {} rows in one frame (bound {row_bound}): \
             row {r0} → {}, action {:?}",
            (f.row - r0).abs(),
            f.row,
            f.action
        );
        *last = (f.col, f.row, s.cols, s.rows);
        f
    }

    /// THE LAW (owner ruling, 2026-08-10): "when the kitty teleports without
    /// an animation, it looks bad."
    ///
    /// Pinned as a law rather than as a list of cases. A demanding script —
    /// typing, a line cleared out from under the cat, an ink storm of
    /// suggested text, a caret hidden and returned somewhere else, a resize
    /// both ways — is walked at 60 fps, and EVERY frame is checked: the pet's
    /// displacement never exceeds what its fastest legal motion could have
    /// covered in that frame. Six case tests cannot say that; this can.
    ///
    /// REDUCED MOTION IS EXEMPT, and is excluded here on purpose rather than
    /// by accident: its whole contract is that the pet simply IS at its
    /// station, with no motion of any kind in between, so under it every
    /// caret move is a jump BY DESIGN. `reduced_motion` is never set in this
    /// script; the reduced-motion tests own that arm.
    #[test]
    fn the_cat_never_changes_position_without_animating_it() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        // The first sighting materialises at the station (alpha-gated, and
        // correct: nothing was on the glass to move), so the law is seeded
        // FROM it rather than applied TO it.
        let f0 = pet.tick(sense(t, Some((6, 20))));
        let mut last = (f0.col, f0.row, 100u16, 30u16);

        // 1. TYPING: a long fast run across the pane, ink growing behind it.
        for k in 0..280u16 {
            let col = 20 + k % 70;
            let mut spans = vec![(0u16, 0u16); 20];
            spans[6] = (0, col);
            pet.sense_ink(0, &spans, Some(6));
            t += Duration::from_millis(16);
            let _ = lawful_tick(&mut pet, sense(t, Some((6, col))), &mut last);
        }

        // 2. THE LINE CLEAR: the caret snaps home and the row's ink vanishes
        // between two frames — Claude Code clearing its prompt.
        for i in 0..120u16 {
            let spans = vec![(0u16, 0u16); 20];
            pet.sense_ink(0, &spans, Some(6));
            t += Duration::from_millis(16);
            let _ = lawful_tick(&mut pet, sense(t, Some((6, 2 + i % 3))), &mut last);
        }

        // 3. THE INK STORM: suggested text painting and un-painting under the
        // cat while the caret sits still.
        for i in 0..240u16 {
            let mut spans = vec![(0u16, 0u16); 20];
            let end = if i % 2 == 0 { 70 } else { 4 };
            spans[4..9].fill((0, end));
            pet.sense_ink(0, &spans, Some(6));
            t += Duration::from_millis(16);
            let _ = lawful_tick(&mut pet, sense(t, Some((6, 3))), &mut last);
        }

        // 4. THE CARET HIDE and a return somewhere else entirely.
        pet.sense_ink(0, &[(0u16, 0u16); 20], Some(6));
        t += Duration::from_millis(16);
        let f = lawful_tick(&mut pet, sense(t, None), &mut last);
        assert!(f.alpha > 100, "fixture: still plainly on the glass");
        for _ in 0..90 {
            t += Duration::from_millis(16);
            let _ = lawful_tick(&mut pet, sense(t, Some((22, 80))), &mut last);
        }

        // 5. THE RESIZE, both ways — the one frame where the coordinate space
        // itself moves under the cat.
        for (cols, rows, frames) in [(40u16, 12u16, 90), (100, 30, 90)] {
            for _ in 0..frames {
                t += Duration::from_millis(16);
                let mut s = sense(t, Some((6, 20)));
                s.cols = cols;
                s.rows = rows;
                let _ = lawful_tick(&mut pet, s, &mut last);
            }
        }

        // 6. …and a long settle, which is where the peck scoot and the
        // idle vignettes live.
        for _ in 0..900 {
            t += Duration::from_millis(16);
            let _ = lawful_tick(&mut pet, sense(t, Some((6, 20))), &mut last);
        }
    }

    /// THE OWNER'S REPRO, 2026-08-10: "the curso kitty sometimes teleports if
    /// the text line suddenly changes (like in claude code if you clear the
    /// line) — and there is a suggested text."
    ///
    /// The mechanism, isolated: the CARET barely moves, and the row's INK
    /// changes drastically between frames — which is exactly what a ghost /
    /// suggested completion does when it paints and un-paints under the cat.
    /// The old law answered that with a cut of up to a screen's width; every
    /// frame of the answer is now a step a cat could take.
    #[test]
    fn ghost_text_flickering_under_the_cat_never_teleports_it() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 6, 12);
        let (t2, f) = idle(&mut pet, t, (6, 12), SIT_AFTER + 0.4);
        t = t2;
        assert!(
            f.action.settled(),
            "fixture: a settled cat beside the prompt"
        );
        let mut last = (f.col, f.row, 100u16, 30u16);
        let mut worst = 0.0f32;
        for i in 0..600u16 {
            // The suggestion appears for a beat, then is dismissed: the row
            // the cat is standing on goes from a stub to nearly the full
            // width and back, several times a second.
            let mut spans = vec![(0u16, 0u16); 20];
            let end = if (i / 12) % 2 == 0 { 90 } else { 13 };
            spans[6] = (0, end);
            spans[5] = (0, end);
            pet.sense_ink(0, &spans, Some(6));
            t += Duration::from_millis(16);
            let prev = (last.0, last.1);
            let f = lawful_tick(&mut pet, sense(t, Some((6, 12))), &mut last);
            worst = worst
                .max((f.col - prev.0).abs())
                .max((f.row - prev.1).abs());
        }
        // …and the law is not passing vacuously: the cat really did have to
        // move, it just never blinked.
        assert!(
            worst > 0.01,
            "fixture is inert — the ink storm never moved the cat at all"
        );
    }

    /// [`REAIM_MAX_U`]'s own case, which the law script never reaches: a jump
    /// arriving in the LAST few frames of an arc.
    ///
    /// The rebase solves `from' = (col - to'·u)/(1 - u)`. That denominator is
    /// the whole reason the bar exists: at `u = 0.99` a two-cell retarget
    /// throws the solved launch point two hundred cells off the glass, and
    /// `u = 1.0` divides by zero outright — an infinite `from_col` makes the
    /// very next `from + (to - from)·u` a NaN, and a NaN column is a cat that
    /// is nowhere at all. Past the bar the re-aim is DROPPED (a cat that is
    /// already landing should land), so this asserts the landing stayed where
    /// the flight aimed it and every frame of the arc stayed finite.
    #[test]
    fn a_re_aim_in_the_last_frames_of_an_arc_is_dropped_not_solved() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 6, 20);
        // A pounce: far enough to be a jump, ordinary enough to be one arc.
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((6, 20 + POUNCE_JUMP as u16 + 4))));
        let caret = 20 + POUNCE_JUMP as u16 + 4;
        let mut airborne = 0;
        while pet.flight.is_none() && airborne < 30 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((6, caret))));
            airborne += 1;
        }
        let f = pet.flight.expect("fixture: the pounce got off the ground");
        let aimed = f.to_col;
        // Fly it to the far side of the bar — the window the guard owns.
        let mut last = pet.col;
        while pet
            .flight
            .is_some_and(|f| (f.t / f.dur).clamp(0.0, 1.0) <= REAIM_MAX_U)
        {
            t += Duration::from_millis(16);
            last = pet.tick(sense(t, Some((6, caret)))).col;
        }
        assert!(
            pet.flight.is_some(),
            "fixture: still airborne past the bar, with frames left to draw"
        );
        // …and NOW the caret jumps clear across the pane.
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((6, 2))));
        assert!(
            f.col.is_finite() && f.row.is_finite(),
            "the dropped re-aim left the cat somewhere real, got {} / {}",
            f.col,
            f.row
        );
        assert!(
            (f.col - last).abs() < 2.0,
            "and it did not swerve on the way down ({last} → {})",
            f.col
        );
        assert_eq!(
            pet.flight.map(|f| f.to_col),
            Some(aimed),
            "the flight keeps the target it was already landing on"
        );
        // Every remaining frame of the arc, and the landing itself, is real.
        for _ in 0..30 {
            t += Duration::from_millis(16);
            let prev = last;
            let f = pet.tick(sense(t, Some((6, 2))));
            last = f.col;
            assert!(
                f.col.is_finite() && (f.col - prev).abs() < 2.0,
                "the arc finished without a teleport ({prev} → {})",
                f.col
            );
        }
    }

    /// F4a: the grief window. From the failure's note to the end of the
    /// droop the brain reports `grieving()`, and no celebratory mote ever
    /// shares the glass with it — the host reads the window to hush the
    /// caret-jump fanfare that cheered a failure in the gauntlet.
    #[test]
    fn a_failure_never_fires_the_party_ring() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = idle(&mut pet, t, (4, 12), 0.5);
        pet.note_command_done(t, true, Some(3000));
        assert!(pet.grieving(), "the note opens the grief window");
        let mut saw_droop = false;
        for _ in 0..200 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 12))));
            if f.action == PetAction::Droop {
                saw_droop = true;
                assert!(pet.grieving(), "the droop IS the grief window");
            }
            for m in f.motes.iter().flatten() {
                assert!(
                    !matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart),
                    "no party mote may share the glass with grief"
                );
            }
        }
        assert!(saw_droop, "the droop must have played");
        assert!(!pet.grieving(), "and the window closes when it ends");
    }

    /// F1 × F4: grief replays where it can be read. A droop whose ground a
    /// fresh prompt prints over re-anchors beside the new caret and RESTARTS
    /// its hold — the sulk is a mood, and a mood interrupted at 0.1 s is a
    /// glitch.
    ///
    /// RE-DECIDED with the no-teleport ruling (2026-08-10). This test used to
    /// demand the droop pose on the very tick the ink arrived, which was only
    /// ever true because the re-anchor was a CUT — the pose could survive a
    /// move that took no time. Now the move takes time: the hop borrows the
    /// pose, and what the test owes is the same F4 promise stated over the
    /// arc instead of over one frame — the trip is ANIMATED, the pet ends up
    /// on ground where the grief can be read, the droop comes back, and it
    /// comes back with a FULL beat. The grief WINDOW is asserted straight
    /// through, every frame, because a hush that blinks off mid-hop lets the
    /// fanfare fire on the failure prompt — the F4a gauntlet bug exactly.
    #[test]
    fn grief_replays_beside_the_new_prompt() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let w = art_cols(10, 20);
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = idle(&mut pet, t, (4, 12), 0.5);
        pet.note_command_done(t, true, Some(3000));
        // Let the droop start...
        let mut f = pet.tick(sense(t + Duration::from_millis(16), Some((4, 12))));
        t += Duration::from_millis(16);
        for _ in 0..40 {
            t += Duration::from_millis(16);
            f = pet.tick(sense(t, Some((4, 12))));
            if f.action == PetAction::Droop {
                break;
            }
        }
        assert_eq!(f.action, PetAction::Droop, "fixture: mid-droop");
        // ...then the prompt prints under it and the caret advances.
        let inked_to = (f.col + w + 3.0) as u16;
        let spans = vec![(0u16, inked_to); 6];
        pet.sense_ink(0, &spans, Some(4));
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, inked_to))));
        // The move is a HOP, not a cut: the pose is airborne, and the hop
        // owes it back with a clock already rewound to the top of the sulk.
        assert_eq!(
            f.action,
            PetAction::Leap,
            "the droop LEAVES the words instead of blinking off them"
        );
        assert_eq!(
            pet.resume,
            Some((PetAction::Droop, 0.0)),
            "and the hop owes the droop back, from the top of its beat"
        );
        // The window never blinks: the host's hush holds across the arc, and
        // the pose comes back on ground the grief can be read on.
        let mut replayed = false;
        for _ in 0..90 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, inked_to))));
            assert!(
                pet.grieving(),
                "the grief window stays shut over the fanfare for the whole \
                 trip, got {:?}",
                f.action
            );
            if f.action == PetAction::Droop {
                assert!(
                    !on_ink(&f, (0, inked_to), w),
                    "replayed on blank ground (col {})",
                    f.col
                );
                replayed = true;
                break;
            }
        }
        assert!(replayed, "the borrowed droop must actually come back");
        // And it holds its FULL beat again from the re-anchor — measured from
        // the LANDING, since that is when the restarted clock began.
        let (_, f) = idle(&mut pet, t, (4, inked_to), DROOP_HOLD * 0.8);
        assert_eq!(
            f.action,
            PetAction::Droop,
            "the hold restarted so the grief can actually be read"
        );
    }

    /// F4b: the cheer stands the pet UP. From a loaf, the perk → frolic
    /// chain plays upright poses and floats warm motes — never the loaf art
    /// lying through its own party.
    #[test]
    fn the_cheer_stands_up_from_the_loaf() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, f) = idle(&mut pet, t, (4, 12), LOAF_AFTER + 1.0);
        assert_eq!(f.action, PetAction::Loaf, "fixture: a loafed cat");
        pet.note_command_done(t, false, Some(CHEER_MIN_MS + 500));
        let mut kinds = Vec::new();
        let mut saw_frolic = false;
        for _ in 0..150 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 12))));
            if f.action == PetAction::Frolic {
                saw_frolic = true;
            }
            if matches!(f.action, PetAction::Perk | PetAction::Frolic) {
                assert_ne!(
                    f.pose,
                    PetGlyphId::PetLoaf,
                    "the party is played on its feet, never from the loaf"
                );
            }
            for m in f.motes.iter().flatten() {
                kinds.push(m.kind);
            }
        }
        assert!(saw_frolic, "the cheer frolic must play");
        assert!(
            kinds.contains(&PetMoteKind::Note) && kinds.contains(&PetMoteKind::Heart),
            "and the party floats BOTH the note and the heart, got {kinds:?}"
        );
    }

    /// F5: the watcher hugs the live edge. A stream that runs rows below the
    /// perch pulls the watcher down to the newest inked row — station beside
    /// its end, tracking as the edge advances.
    #[test]
    fn the_watcher_hugs_the_live_edge() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 2, 10);
        let (mut t, f) = idle(&mut pet, t, (2, 12), SIT_AFTER + 0.4);
        assert!(f.action.settled(), "fixture: a settled cat");
        // A stream fills rows 3..=10 while the caret stays parked — the
        // b09x geometry, where the shipped pet flopped 8 rows above the
        // edge. Each row is a short "stream line N".
        let mut spans = vec![(0u16, 13u16); 3];
        let mut f = f;
        for edge in 3u16..=10 {
            spans.push((0, 14));
            pet.sense_ink(0, &spans, Some(edge));
            for _ in 0..12 {
                t += Duration::from_millis(16);
                let mut s = sense(t, Some((2, 12)));
                s.output_burst = true;
                f = pet.tick(s);
            }
        }
        assert!(
            (f.row - 10.0).abs() < WATCH_HUG_ROWS,
            "the watcher must hug the newest inked row: feet at row {}, edge 10",
            f.row
        );
        assert!(
            f.col >= 14.0,
            "and stand clear of the line's ink, col {}",
            f.col
        );
    }

    /// F6's brain half: the touch between two bounds wears the COIL — the
    /// pose whose face survives tile LOD — and it wears it in BOTH
    /// directions, so the twins mirror cleanly.
    #[test]
    fn touch_land_mirrors_cleanly() {
        let start = Instant::now();
        let touch_poses = |from: u16, to: u16| -> Vec<(PetGlyphId, bool)> {
            let mut pet = PetBrain::default();
            let t = awake(&mut pet, start, 4, from);
            let (_, frames) = drive_and_collect(&mut pet, t, (4, to), 260);
            frames
                .iter()
                .filter(|f| f.action == PetAction::Land && pet_touching(f))
                .map(|f| (f.pose, f.facing_left))
                .collect()
        };
        fn pet_touching(f: &PetFrame) -> bool {
            // The touch is the only Land whose squash law halves — cheap to
            // spot by pose: the coil IS the touch's costume now.
            f.pose == PetGlyphId::PetCrouch
        }
        let rightward = touch_poses(2, 78);
        let leftward = touch_poses(78, 2);
        assert!(
            !rightward.is_empty() && !leftward.is_empty(),
            "both twin-bound crossings must touch down between bounds \
             (right {rightward:?}, left {leftward:?})"
        );
        assert!(
            rightward
                .iter()
                .all(|&(p, face)| p == PetGlyphId::PetCrouch && !face),
            "rightward touch: the coil, facing right — got {rightward:?}"
        );
        assert!(
            leftward
                .iter()
                .all(|&(p, face)| p == PetGlyphId::PetCrouch && face),
            "leftward touch: the SAME coil, mirrored — got {leftward:?}"
        );
    }

    /// F1's flight half: a two-bound crossing whose midfield is inked wall
    /// to wall spends the span as ONE bound — there is no ground between
    /// worth touching (b02w_TR_116's relaunch clipped "touchland").
    #[test]
    fn the_touch_point_dodges_the_ink() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 2);
        // Row 4 is inked from col 2 to col 76 — the typed line the twin
        // bounds would cross.
        let spans = vec![(0u16, 0u16), (0, 0), (0, 0), (0, 0), (2, 76)];
        pet.sense_ink(0, &spans, Some(4));
        let (_, frames) = drive_and_collect(&mut pet, t, (4, 78), 260);
        let mut episodes = 0;
        let mut airborne_was = false;
        for f in &frames {
            let airborne = f.action == PetAction::Leap;
            if airborne && !airborne_was {
                episodes += 1;
            }
            airborne_was = airborne;
        }
        assert_eq!(
            episodes, 1,
            "an inked midfield collapses the split into one clean bound"
        );
    }

    /// F8: the z's die before the ink. With an inked band over the
    /// sleeper's head, no z-mote is ever drawn inside it — the fade reaches
    /// zero BEFORE the crossing, and the dead mote frees its slot.
    #[test]
    fn the_zees_die_before_the_ink() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        // Rows 0..=2 carry a printed command right above the sleeper — the
        // b07_lightsleep geometry.
        let spans = vec![(0u16, 40u16), (0, 40), (0, 40), (0, 0), (0, 0)];
        pet.sense_ink(0, &spans, Some(2));
        let (mut t, f) = idle(&mut pet, t, (4, 12), SLEEP_AFTER + 0.2);
        assert_eq!(f.action, PetAction::Sleep, "fixture: light sleep");
        for _ in 0..((ZEE_EVERY * 3.0 / 0.016) as u32) {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 12))));
            for m in f.motes.iter().flatten() {
                if matches!(m.kind, PetMoteKind::Zee | PetMoteKind::ZeePop) && m.alpha > 0 {
                    assert!(
                        m.row >= 3.0,
                        "a visible z must never enter the inked band: row {}",
                        m.row
                    );
                }
            }
        }
    }

    /// F9: eye contact keeps the muzzle out of the caret cell — the face-on
    /// sit parks a clear margin past caret+1.
    #[test]
    fn the_peck_stance_clears_the_caret_cell() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (t, _) = type_run(&mut pet, t, 4, 12, 3, 0.06);
        let (_, f) = idle(&mut pet, t, (4, 15), SIT_AFTER + 0.5);
        assert_eq!(f.pose, PetGlyphId::PetSitFront, "fixture: the stare");
        assert!(
            f.col >= 15.0 + STATION_LEAD + PECK_CLEAR - 0.01,
            "the face-on sit parks clear of the caret cell, col {}",
            f.col
        );
    }

    /// The seam's safety proof: a brain that is never fed ink behaves
    /// byte-for-byte like the pre-seam brain — same stations, same frames —
    /// so shipping the seam ahead of the host wiring risks nothing.
    #[test]
    fn an_unfed_ink_map_changes_nothing() {
        let start = Instant::now();
        let mut fed = PetBrain::default();
        let mut unfed = PetBrain::default();
        fed.sense_ink(0, &[], None); // fed with EMPTINESS, not with rows
        let script: &[(u16, u16, f32)] = &[
            (4, 12, 0.5),
            (4, 40, 1.0),
            (5, 2, 0.8),
            (5, 30, SIT_AFTER + 1.0),
        ];
        let mut t = start;
        for &(row, col, dwell) in script {
            let end = t + Duration::from_secs_f32(dwell);
            while t < end {
                t += Duration::from_millis(16);
                let a = fed.tick(sense(t, Some((row, col))));
                let b = unfed.tick(sense(t, Some((row, col))));
                assert_eq!(a.fp(), b.fp(), "empty map ⇒ identical frames");
            }
        }
    }

    // ── wave 3: the chase, the stakeout, and the word-cat tiers ────────────

    /// A toy teasing at under-dash speed earns a RUNNING chase: the pet
    /// leaves its station on paws (never the flight doors), closes on the
    /// pointer, and — when the toy never lets itself be caught — breaks off
    /// at stamina, owes the groom of dignity, and walks home.
    #[test]
    fn tease_earns_a_running_chase_then_dignity() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = warm_settled(&mut pet, start);
        // The toy circles ~12 cols right of the pet: amplitude 8, ~2.5 rad/s
        // — mean speed well inside [PURSUIT_MIN_SPEED, DASH_SPEED).
        let base = pet.col + 12.0;
        let mut chased = false;
        let mut ran = false;
        let mut phase = 0.0f32;
        for _ in 0..700 {
            t += Duration::from_millis(16);
            phase += 0.04;
            let px = base + 8.0 * phase.sin();
            let f = ptick(&mut pet, t, (4, 44), Some((px, 4.0)));
            if pet.pursuit_t.is_some() {
                chased = true;
                if matches!(f.action, PetAction::Walk | PetAction::Run) {
                    ran = true;
                }
                assert!(
                    pet.flight.is_none(),
                    "a chase runs on paws: the standing-gap doors stay shut"
                );
            }
        }
        assert!(chased, "the tease must bait a chase");
        assert!(ran, "and the chase must actually run");
        // 700 ticks = 11.2 s of toy time: stamina (5 s) has broken the chase
        // even if the stakeout phase ate the opening seconds.
        assert!(pet.pursuit_t.is_none(), "stamina breaks the chase off");
        assert!(
            pet.pursuit_cool > 0.0,
            "and the cooldown guards the pet's dignity"
        );
        // The groom lands on the next arrival (or already played): run the
        // toy away and let the pet come home.
        let mut groomed = pet.action == PetAction::Groom;
        for _ in 0..500 {
            t += Duration::from_millis(16);
            let f = ptick(&mut pet, t, (4, 44), None);
            groomed |= f.action == PetAction::Groom;
        }
        assert!(groomed, "the break-off owes the groom of dignity");
        assert!(!pet.groom_owed, "and the debt is paid, not pinned");
    }

    /// THE HOME-BASE LAW: one keystroke ends the game mid-stride. The chase
    /// drops, a fresh chase is cooldown-blocked while typing is recent, and
    /// the pet answers the caret.
    #[test]
    fn typing_aborts_the_chase_instantly() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = warm_settled(&mut pet, start);
        let base = pet.col + 12.0;
        let mut phase = 0.0f32;
        for _ in 0..120 {
            t += Duration::from_millis(16);
            phase += 0.04;
            let px = base + 8.0 * phase.sin();
            let _ = ptick(&mut pet, t, (4, 44), Some((px, 4.0)));
            if pet.pursuit_t.is_some() {
                break;
            }
        }
        assert!(pet.pursuit_t.is_some(), "fixture: the chase is live");
        // The caret moves: work reclaims the cat this very tick.
        t += Duration::from_millis(16);
        let _ = ptick(&mut pet, t, (4, 52), Some((base, 4.0)));
        assert!(pet.pursuit_t.is_none(), "typing ends the game instantly");
        // And the toy cannot re-bait while the keyboard is warm: quiet is
        // under PURSUIT_QUIET for the next second whatever the pointer does.
        for _ in 0..30 {
            t += Duration::from_millis(16);
            phase += 0.04;
            let px = base + 8.0 * phase.sin();
            let _ = ptick(&mut pet, t, (4, 52), Some((px, 4.0)));
            assert!(
                pet.pursuit_t.is_none(),
                "the chase stays down while typing is recent"
            );
        }
    }

    /// A CREEPING toy nearby pins the hunting crouch — the stakeout — and a
    /// vanished toy stands the cat back down, never launching the coil.
    #[test]
    fn creeping_toy_pins_the_stakeout() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = warm_settled(&mut pet, start);
        // Creep at ~3 cells/s, 8 cols away: enough heat to matter, far too
        // slow for the chase or the dash.
        let mut px = pet.col + 8.0;
        let mut staked = false;
        for k in 0..300 {
            t += Duration::from_millis(16);
            px += if k % 40 < 20 { 0.05 } else { -0.05 };
            let f = ptick(&mut pet, t, (4, 44), Some((px, 4.0)));
            if pet.stakeout {
                staked = true;
                assert_eq!(
                    f.action,
                    PetAction::Crouch,
                    "the stakeout is the hunting crouch"
                );
            }
        }
        assert!(staked, "a creeping toy must pin the stakeout");
        // The toy vanishes: the cat stands down — no coil, no launch.
        t += Duration::from_millis(16);
        let f = ptick(&mut pet, t, (4, 44), None);
        assert!(!pet.stakeout, "no toy, no stakeout");
        assert_ne!(f.action, PetAction::Leap, "and never a phantom launch");
    }

    /// THE LOOK TIER: a peek beyond bat reach but inside LOOK_RANGE earns a
    /// perk and a turned face — never a trip.
    #[test]
    fn far_peek_gets_a_look_not_a_trip() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.4);
        let col0 = pet.col;
        // 15 column-equivalents to the right: past BAT_RANGE, inside LOOK.
        pet.note_peek(t, col0 + 15.0, 4.0);
        assert!(pet.pending_bat.is_none(), "out of paw reach: no visit");
        assert!(pet.pending_look.is_some(), "but well worth a look");
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 14))));
        assert_eq!(f.action, PetAction::Perk, "the look is the double-take");
        assert!(!pet.facing_left, "faced toward the peek");
        assert!(
            (pet.col - col0).abs() < 0.5,
            "and the paws never left the station"
        );
    }

    /// NEAR-visit variety: the serial parity dealt at note time alternates
    /// the swipe and the playbow greeting.
    #[test]
    fn near_peek_alternates_swipe_and_greeting() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        let (t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.4);
        pet.mote_serial = 0; // even: the swipe
        pet.note_peek(t, pet.col + 4.0, 4.0);
        assert!(!pet.bat_greet, "even serial swipes");
        pet.pending_bat = None;
        pet.mote_serial = 1; // odd: the greeting
        pet.note_peek(t, pet.col + 4.0, 4.0);
        assert!(pet.bat_greet, "odd serial greets with the playbow");
    }

    /// Codex review, 2026-08-10: non-finite coordinates never reach the
    /// pose state — a NaN pointer is no pointer, a NaN peek never latches.
    #[test]
    fn non_finite_inputs_never_poison_the_pose() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        let (mut t, _) = idle(&mut pet, t, (4, 14), 0.5);
        pet.note_peek(t, f32::NAN, 4.0);
        assert!(pet.pending_bat.is_none() && pet.pending_look.is_none());
        for _ in 0..20 {
            t += Duration::from_millis(16);
            let f = ptick(&mut pet, t, (4, 14), Some((f32::NAN, f32::INFINITY)));
            assert!(f.col.is_finite(), "the pose stays finite");
        }
        assert_eq!(pet.pointer_heat, 0.0, "a NaN pointer is no pointer");
        pet.set_body_left_px(f32::NAN);
        assert!(pet.body_left_px.is_finite());
    }

    /// THE TENNIS WATCH (wave 4): a rally that outlives the frolic sits the
    /// cat down to watch — facing ping-pongs with the caret — and the rally
    /// lapsing stands it back down.
    #[test]
    fn a_long_rally_earns_the_tennis_watch() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = warm_settled(&mut pet, start);
        // Rally: the caret ping-pongs 3 columns either side, every 150 ms.
        let (mut col, mut right) = (44i32, true);
        let mut watched = false;
        let (mut faced_left, mut faced_right) = (false, false);
        for _ in 0..60 {
            t += Duration::from_millis(150);
            right = !right;
            col += if right { 3 } else { -3 };
            let f = pet.tick(sense(t, Some((4, col as u16))));
            if pet.tennis {
                watched = true;
                assert_eq!(f.action, PetAction::Sit, "the watch sits");
                faced_left |= pet.facing_left;
                faced_right |= !pet.facing_left;
            }
        }
        assert!(watched, "a sustained rally must earn the watch");
        assert!(
            faced_left && faced_right,
            "and the facing follows the ball both ways"
        );
        // The rally ends: the watch lapses back into the ordinary settle.
        let (_, _) = idle(&mut pet, t, (4, col as u16), TENNIS_LAPSE + 0.5);
        assert!(!pet.tennis, "the rally over, the watch stands down");
    }

    /// THE DRIFT-BRAKE (wave 4): a long gallop overshoots its station on
    /// purpose and trots back; the follower still ends AT the station.
    #[test]
    fn a_long_gallop_overshoots_then_trots_back() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = warm_settled(&mut pet, start);
        let from = pet.col;
        // One big caret jump far to the right of a warmed cat: the chase
        // leg is long and fast enough to earn the brake — but under the
        // big-jump door, so it stays a run.
        let target_col = 84u16;
        let mut overshot = false;
        let mut max_col = from;
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((4, target_col))));
            max_col = max_col.max(pet.col);
            if pet.braking {
                overshot = true;
            }
        }
        let station = PetBrain::station(target_col, 100, art_cols(10, 20));
        // The brake is content- and distance-gated; when it fires the paws
        // must still come to rest at the station.
        if overshot {
            assert!(
                max_col > station + 0.5,
                "the brake means actually blowing past the station"
            );
        }
        assert!(
            (pet.col - station).abs() <= ARRIVED + 0.6,
            "however the leg ended, home is home (got {} want {station})",
            pet.col
        );
    }

    /// HIDE-BEHIND-WORDS (wave 4c): a bored cat with a word in reach ducks
    /// behind it — drawn UNDER the glyphs while it lurks — then strolls
    /// home and surfaces.
    #[test]
    fn a_bored_cat_hides_behind_a_word_and_comes_back() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = warm_settled(&mut pet, start);
        // A word on the pet's row, a few columns left of its station.
        let mut spans = [(0u16, 0u16); 12];
        let word_row = pet.row as usize;
        spans[word_row] = (34, 42);
        let mut hid = false;
        let mut under = false;
        for _ in 0..900 {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, None);
            if !pet.hiding && pet.hide_to.is_none() {
                // Pin the deal against serial drift (twitches and motes
                // advance it between here and boredom's window opening), and
                // keep the cat CONTENT: boredom is an energetic state — the
                // content gate means a cold idle drains into sleep instead,
                // so the fixture stands in for the play that would normally
                // precede a bored window.
                pet.mote_serial = 3;
                pet.content = pet.content.max(0.9);
            }
            let f = pet.tick(sense(t, Some((4, 44))));
            if pet.hiding {
                hid = true;
                under |= f.under_ink;
            }
        }
        assert!(hid, "the deal must send the cat behind the word");
        assert!(under, "and back there it draws UNDER the glyphs");
        assert!(!pet.hiding, "the dwell ends the lurk");
        // Home again, over the ink like always.
        let (_, f) = idle(&mut pet, t, (4, 44), 2.0);
        assert!(!f.under_ink, "surfaced on the walk home");
    }

    /// Codex review, 2026-08-10: a spent watch must still get the ticks to
    /// decay its heat under the gate, or no LATER stream is ever watched.
    #[test]
    fn a_spent_watch_rearms_once_the_stream_ends() {
        let mut pet = PetBrain {
            watch_heat: 1.0,
            watch_spent: true,
            alpha: 1.0,
            ..PetBrain::default()
        };
        assert!(
            pet.needs_frames(),
            "live heat keeps the lane so the decay can run"
        );
        let start = Instant::now();
        let mut t = start;
        let _ = pet.tick(sense(t, Some((4, 14))));
        for _ in 0..80 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((4, 14))));
        }
        assert!(
            pet.watch_heat < WATCH_GATE,
            "the heat decayed under the gate"
        );
        assert!(!pet.watch_spent, "and the next stream earns a fresh stare");
    }

    // ── the NEW-LINE probe (a ruler, not a law — `#[ignore]`d) ──────────
    //
    // Drives the real brain at 60 fps through every caret shape a new line
    // takes and PRINTS what the pet did: each flight the brain launched,
    // attributed to the code path that launched it; the action sequence with
    // timestamps; the largest single-frame displacement; and how long until
    // the cat is settled again. Nothing is asserted — later waves re-run it
    // to show their numbers moved. Run with:
    //   targo --unverified test -p aterm-effects --lib \
    //     kitty_pet::tests::probe_newline_scenarios -- --ignored --nocapture

    /// One 60 fps frame.
    const PROBE_FRAME: Duration = Duration::from_micros(16_667);
    const PROBE_ROWS: u16 = 24;

    /// One flight, as the probe saw it launch.
    #[derive(Clone, Copy, Debug)]
    struct ProbeFlight {
        /// Seconds from the probe's epoch.
        at: f32,
        site: &'static str,
        from_col: f32,
        to_col: f32,
        from_row: f32,
        to_row: f32,
        dur: f32,
        arc: f32,
        big: bool,
    }

    /// The probe's world: a `cols`×24 pane at 10×20 px cells, an ink map the
    /// scenario paints by hand, and the recorder.
    struct Probe {
        pet: PetBrain,
        t: Instant,
        t0: Instant,
        cols: u16,
        spans: Vec<(u16, u16)>,
        live: Option<u16>,
        last: Option<PetFrame>,
        /// The trigger's timestamp (seconds from `t0`).
        trigger: Option<f32>,
        /// The frame on glass when the trigger was marked.
        at_trigger: Option<PetFrame>,
        /// Log flights/actions from this time on (seconds from `t0`).
        log_from: f32,
        flights: Vec<ProbeFlight>,
        actions: Vec<(f32, PetAction)>,
        max_dcol: (f32, f32, PetAction),
        max_drow: (f32, f32, PetAction),
        settled_since: Option<f32>,
        settle_at: Option<f32>,
        settle_where: (f32, f32, PetAction),
    }

    impl Probe {
        fn new(cols: u16) -> Self {
            let t0 = Instant::now();
            Self {
                pet: PetBrain::default(),
                t: t0,
                t0,
                cols,
                spans: vec![(0, 0); usize::from(PROBE_ROWS)],
                live: None,
                last: None,
                trigger: None,
                at_trigger: None,
                log_from: 0.0,
                flights: Vec::new(),
                actions: Vec::new(),
                max_dcol: (0.0, 0.0, PetAction::Sit),
                max_drow: (0.0, 0.0, PetAction::Sit),
                settled_since: None,
                settle_at: None,
                settle_where: (0.0, 0.0, PetAction::Sit),
            }
        }

        fn now(&self) -> f32 {
            self.t.saturating_duration_since(self.t0).as_secs_f32()
        }

        fn sense_at(&self, caret: Option<(u16, u16)>) -> PetSense {
            let mut s = sense(self.t, caret);
            s.cols = self.cols;
            s.rows = PROBE_ROWS;
            s
        }

        /// Paint row `row`'s ink as the half-open span `[first, end)`.
        fn ink(&mut self, row: u16, first: u16, end: u16) {
            self.spans[usize::from(row)] = (first, end);
        }

        /// Stamp the trigger: everything after this frame is measured.
        fn mark(&mut self) {
            self.trigger = Some(self.now());
            self.at_trigger = self.last;
            self.max_dcol = (0.0, 0.0, PetAction::Sit);
            self.max_drow = (0.0, 0.0, PetAction::Sit);
            self.settled_since = None;
            self.settle_at = None;
        }

        /// One 60 fps frame with the caret at `caret`, recorded.
        fn frame(&mut self, caret: Option<(u16, u16)>) -> PetFrame {
            self.t += PROBE_FRAME;
            let now = self.now();
            self.pet.sense_ink(0, &self.spans, self.live);
            let w = art_cols(10, 20);
            let pre_action = self.pet.action;
            let pre_hop = self.pet.hop_crouch;
            let pre_bound2 = self.pet.bound2.is_some();
            let pre_wall = self.pet.pending_wall_transit;
            let pre_play = self.pet.play_to.is_some();
            let pre_resume = self.pet.resume.is_some();
            let pre_flight = self.pet.flight;
            let s = self.sense_at(caret);
            let f = self.pet.tick(s);

            // A NEW flight (none was live) or the one mid-air re-aim.
            if let Some(fl) = self.pet.flight {
                let launched = pre_flight.is_none();
                let reaimed = pre_flight.is_some_and(|p| (p.to_col - fl.to_col).abs() > 1e-6);
                if (launched || reaimed) && now >= self.log_from {
                    let site = if reaimed {
                        "mid-air re-aim (:3044 rebase)"
                    } else if fl.big && pre_action == PetAction::Land && pre_bound2 {
                        "big bound 2 (:3109 bound2 -> begin_big_arc)"
                    } else if fl.big {
                        "big bound (:3184 launch_big)"
                    } else if pre_action == PetAction::Crouch && pre_hop {
                        "row hop (:3407 hop_crouch -> :3193 begin_flight vertical)"
                    } else if pre_action == PetAction::Crouch && pre_play {
                        "play (:3287)"
                    } else if pre_action == PetAction::Crouch {
                        "pounce (:3444 Crouch -> :3301 launch)"
                    } else if self.pet.resume.is_some() && !pre_resume {
                        "ink re-anchor (:2912 begin_arc, flight_shape vertical)"
                    } else if pre_wall {
                        "wall transit (:3423 begin_arc WALL_HOP_DUR)"
                    } else {
                        "unknown"
                    };
                    self.flights.push(ProbeFlight {
                        at: now,
                        site,
                        from_col: fl.from_col,
                        to_col: fl.to_col,
                        from_row: fl.from_row,
                        to_row: fl.to_row,
                        dur: fl.dur,
                        arc: fl.arc,
                        big: fl.big,
                    });
                }
            }
            if now >= self.log_from && self.last.is_none_or(|l| l.action != f.action) {
                self.actions.push((now, f.action));
            }
            if let (Some(l), Some(_)) = (self.last, self.trigger) {
                let dc = (f.col - l.col).abs();
                let dr = (f.row - l.row).abs();
                if dc > self.max_dcol.0 {
                    self.max_dcol = (dc, now, f.action);
                }
                if dr > self.max_drow.0 {
                    self.max_drow = (dr, now, f.action);
                }
            }
            if self.trigger.is_some() && self.settle_at.is_none() {
                let ok = caret.is_some_and(|c| {
                    let (tc, tr) = self.pet.station_safe(c, self.cols, PROBE_ROWS, w, 10);
                    f.action.settled()
                        && self.pet.flight.is_none()
                        && (f.col - tc).abs() < 1.0
                        && (f.row - tr).abs() < 0.01
                });
                if ok {
                    let since = *self.settled_since.get_or_insert(now);
                    if now - since >= 0.5 {
                        self.settle_at = Some(since);
                        self.settle_where = (f.col, f.row, f.action);
                    }
                } else {
                    self.settled_since = None;
                }
            }
            self.last = Some(f);
            f
        }

        fn hold_frames(&mut self, caret: Option<(u16, u16)>, n: u32) {
            for _ in 0..n {
                let _ = self.frame(caret);
            }
        }

        fn hold(&mut self, caret: Option<(u16, u16)>, secs: f32) {
            // Frames, not seconds: 60 fps is the host's cadence.
            let n = (secs * 60.0).round();
            assert!(n >= 0.0 && n < 1e6, "probe: bad hold {secs}");
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            self.hold_frames(caret, n as u32);
        }

        /// Type from `(row, from)` to `(row, to)`: one caret move per 100 ms
        /// (six frames), the row's ink growing with the caret.
        fn type_to(&mut self, row: u16, from: u16, to: u16) {
            for c in (from + 1)..=to {
                self.ink(row, 0, c);
                let _ = self.frame(Some((row, c)));
                self.hold_frames(Some((row, c)), 5);
            }
        }

        /// The common history: sighted 16 columns short of `col0`, typed to
        /// 8 short, 2.5 s quiet (awake, settled), then typed to `col0` — so
        /// the cat is a cat that has been following you, not a fresh sleeper.
        fn warm(&mut self, row: u16, col0: u16) {
            let start = col0.saturating_sub(16);
            let mid = col0.saturating_sub(8);
            self.live = Some(row);
            self.ink(row, 0, start);
            let _ = self.frame(Some((row, start)));
            self.type_to(row, start, mid);
            self.hold(Some((row, mid)), 2.5);
            self.log_from = self.now();
            self.type_to(row, mid, col0);
        }

        fn report(&self, name: &str, setup: &str) {
            let tr = self.trigger.unwrap_or(0.0);
            println!("== {name} ==");
            println!("   setup: {setup}");
            if let Some(f) = self.at_trigger {
                println!(
                    "   at trigger: col {:.2} row {:.0} {:?} alpha {}",
                    f.col, f.row, f.action, f.alpha
                );
            }
            let acts: Vec<String> = self
                .actions
                .iter()
                .map(|(t, a)| format!("{a:?}@{:+.2}", t - tr))
                .collect();
            println!("   actions: {}", acts.join(" "));
            if self.flights.is_empty() {
                println!("   flights: none");
            }
            for fl in &self.flights {
                let span = (fl.to_col - fl.from_col).abs();
                println!(
                    "   flight @{:+.2}s [{}] ({:.2},r{:.0})->({:.2},r{:.0}) span={:.1} dur={:.3}s speed={:.1} c/s vspeed={:.1} r/s arc={:.2} big={}",
                    fl.at - tr,
                    fl.site,
                    fl.from_col,
                    fl.from_row,
                    fl.to_col,
                    fl.to_row,
                    span,
                    fl.dur,
                    span / fl.dur,
                    (fl.to_row - fl.from_row).abs() / fl.dur,
                    fl.arc,
                    fl.big
                );
            }
            println!(
                "   max frame |dcol|={:.2} @{:+.2}s {:?}; max frame |drow|={:.2} @{:+.2}s {:?}",
                self.max_dcol.0,
                self.max_dcol.1 - tr,
                self.max_dcol.2,
                self.max_drow.0,
                self.max_drow.1 - tr,
                self.max_drow.2
            );
            match self.settle_at {
                Some(s) => println!(
                    "   settled {:.2}s after the trigger at (col {:.2}, row {:.0}) as {:?}",
                    s - tr,
                    self.settle_where.0,
                    self.settle_where.1,
                    self.settle_where.2
                ),
                None => println!("   NOT settled within the run"),
            }
        }
    }

    #[test]
    #[ignore = "a ruler, not a law: run by hand with --ignored --nocapture"]
    fn probe_newline_scenarios() {
        // 1–3: Enter from col 10 / 40 / 78, settled first and mid-typing.
        // The shell prints "$ " on the next row; the caret lands at (7, 2).
        for (n, col0, label) in [
            (1, 10u16, "col 10"),
            (2, 40, "col 40"),
            (3, 78, "col 78 (wall side)"),
        ] {
            for settled in [true, false] {
                let mut p = Probe::new(80);
                p.warm(6, col0);
                if settled {
                    p.hold(Some((6, col0)), 2.5);
                }
                p.mark();
                p.ink(7, 0, 2);
                p.live = Some(7);
                let _ = p.frame(Some((7, 2)));
                p.hold(Some((7, 2)), 5.0);
                let variant = if settled {
                    "settled first"
                } else {
                    "mid-typing"
                };
                p.report(
                    &format!(
                        "S{n}{} Enter from {label}, {variant}",
                        if settled { "a" } else { "b" }
                    ),
                    &format!("80x24; row 6 ink 0..{col0}; Enter -> (7,2), row 7 ink 0..2"),
                );
            }
        }

        // 4: the typed margin wrap — the caret falls off the right margin
        // onto the next row and typing carries on there.
        for cols in [80u16, 200] {
            let mut p = Probe::new(cols);
            p.warm(6, cols - 1);
            p.mark();
            p.live = Some(7);
            p.ink(7, 0, 0);
            let _ = p.frame(Some((7, 0)));
            p.hold_frames(Some((7, 0)), 5);
            p.type_to(7, 0, 5);
            p.hold(Some((7, 5)), 4.0);
            p.report(
                &format!("S4 typed margin wrap, cols={cols}"),
                &format!(
                    "{cols}x24; typed to (6,{}); wrap -> (7,0), then (7,1)..(7,5) at 100 ms",
                    cols - 1
                ),
            );
        }

        // 5: the scrolled wrap on the LAST row: the caret comes back to col 0
        // on the SAME row (23) and the line it left scrolls up to row 22.
        {
            let mut p = Probe::new(80);
            p.warm(23, 79);
            p.mark();
            p.ink(22, 0, 80);
            p.ink(23, 0, 0);
            p.live = Some(23);
            let _ = p.frame(Some((23, 0)));
            p.hold_frames(Some((23, 0)), 5);
            p.type_to(23, 0, 5);
            p.hold(Some((23, 5)), 4.0);
            p.report(
                "S5 scrolled wrap on the last row",
                "80x24; typed to (23,79); wrap -> (23,0) with row 22 = 0..80, then (23,1)..(23,5)",
            );
        }

        // 6: the prompt redraw — ONE frame of hidden caret, then (7,2).
        {
            let mut p = Probe::new(80);
            p.warm(6, 40);
            p.hold(Some((6, 40)), 2.5);
            p.mark();
            let _ = p.frame(None);
            p.ink(7, 0, 2);
            p.live = Some(7);
            let _ = p.frame(Some((7, 2)));
            p.hold(Some((7, 2)), 5.0);
            p.report(
                "S6 prompt redraw (one None frame) from col 40",
                "80x24; settled at (6,40); caret None for 1 frame; then (7,2) with row 7 ink 0..2",
            );
        }

        // 7: Enter while asleep.
        {
            let mut p = Probe::new(80);
            p.warm(6, 30);
            p.hold(Some((6, 30)), 25.0);
            p.mark();
            p.ink(7, 0, 2);
            p.live = Some(7);
            let _ = p.frame(Some((7, 2)));
            p.hold(Some((7, 2)), 5.0);
            p.report(
                "S7 Enter while asleep from col 30",
                "80x24; 25 s quiet at (6,30); Enter -> (7,2), row 7 ink 0..2",
            );
        }

        // 8: a three-line prompt.
        {
            let mut p = Probe::new(80);
            p.warm(6, 30);
            p.hold(Some((6, 30)), 2.5);
            p.mark();
            for r in 7..=9 {
                p.ink(r, 0, 2);
            }
            p.live = Some(9);
            let _ = p.frame(Some((9, 2)));
            p.hold(Some((9, 2)), 5.0);
            p.report(
                "S8 Enter with a 3-line prompt from col 30",
                "80x24; settled at (6,30); -> (9,2), rows 7..9 ink 0..2",
            );
        }

        // 9: Home from col 70 — a genuine 70-cell retreat, for contrast.
        {
            let mut p = Probe::new(80);
            p.warm(6, 70);
            p.hold(Some((6, 70)), 2.5);
            p.mark();
            let _ = p.frame(Some((6, 0)));
            p.hold(Some((6, 0)), 5.0);
            p.report(
                "S9 Home from col 70 (contrast)",
                "80x24; settled at (6,70), row 6 ink 0..70; Home -> (6,0)",
            );
        }

        // 10: a paste of +30 columns on the same row, for contrast.
        {
            let mut p = Probe::new(80);
            p.warm(6, 10);
            p.hold(Some((6, 10)), 2.5);
            p.mark();
            p.ink(6, 0, 40);
            let _ = p.frame(Some((6, 40)));
            p.hold(Some((6, 40)), 5.0);
            p.report(
                "S10 paste +30 on the same row (contrast)",
                "80x24; settled at (6,10); paste -> (6,40), row 6 ink 0..40",
            );
        }

        // 11: the prompt redraw at the BOTTOM — the shell scrolled.
        {
            let mut p = Probe::new(80);
            p.warm(23, 40);
            p.hold(Some((23, 40)), 2.5);
            p.mark();
            let _ = p.frame(None);
            p.ink(22, 0, 40);
            p.ink(23, 0, 2);
            p.live = Some(23);
            let _ = p.frame(Some((23, 2)));
            p.hold(Some((23, 2)), 5.0);
            p.report(
                "S11 prompt redraw at the bottom from (23,40)",
                "80x24; settled at (23,40); caret None for 1 frame; then (23,2) with row 22 = 0..40, row 23 = 0..2",
            );
        }
    }
}
