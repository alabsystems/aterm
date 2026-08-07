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
//! | Enter, a wrapped line, an arrow up or down | **hops** the row | the ground line moved, so it must leave the floor |
//! | backspace, or any retreat | **startles** | the thing it was chasing came back at it; it flinches and turns around |
//! | forward, back, forward, back (editing) | **frolics** | direction reversals are play, not travel |
//! | stops | **sits**, then **grooms** | it caught up; there is nothing to chase |
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
//! ## Clocklessness and determinism
//!
//! Like every engine in this crate, [`PetBrain::tick`] takes an injected `now`
//! and reads no wall clock. It also draws no randomness: the "occasional"
//! choices — when the tail flicks, when the wash happens — are pure functions of
//! how long it has been QUIET, i.e. of the settle the user is actually watching,
//! so a test can drive the exact sequence a user sees and the flourishes cannot
//! depend on how long the window happened to have been open.

use core::f32::consts::TAU;

use web_time::Instant;

use crate::pet_glyphs_gen::PetGlyphId;

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
/// Cells of travel per full four-frame gait cycle. The pet is ~2.9 cells wide,
/// so a shade over half a body length per cycle is a natural stride: shorter
/// scurries, longer moon-walks.
const STRIDE_CELLS: f32 = 1.7;

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
/// Seconds of flight per cell of pounce distance, and the clamp either side.
/// Flight time grows with distance (a longer jump takes longer) but sub-linearly,
/// so a jump clear across many columns is quick and a five-cell hop is not
/// instant. Deliberately NOT phrased "screen-wide": that is a reserved scope
/// phrase, and these are timing scalars measuring the DISTANCE one pounce
/// covers, not the reach of an enforcer. Rewording states that directly instead
/// of asserting an obligation via `scope-waiver:` and then excusing it.
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
/// down, not a leap.
const HOP_DUR: f32 = 0.20;
const HOP_ARC: f32 = 0.42;

// ── the reactive states ─────────────────────────────────────────────────────

/// A retreat of at least this many columns startles the pet: back arched, tail
/// bottled, ears pinned. One column is enough — a single backspace *is* the
/// thing coming back at it.
const STARTLE_COLS: f32 = 1.0;
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
const PURR_DEPTH: f32 = 0.022;

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
    /// Settled and content — a sit with the eyes shut and the chest working.
    Purr,
    /// A sit interrupted by a wash.
    Groom,
    /// Ears up, head high: it has noticed the caret is somewhere else.
    Perk,
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
}

impl PetAction {
    /// True while the pet is settled — the states a caret move interrupts.
    #[must_use]
    pub fn settled(self) -> bool {
        matches!(
            self,
            Self::Sleep | Self::Sit | Self::Purr | Self::Groom | Self::Perk
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
    pub rows: u16,
    pub cols: u16,
    pub cell_w: u16,
    pub cell_h: u16,
    /// Reduced motion: no chase, no gait, no arcs — the pet simply sits at its
    /// station and the frame settles.
    pub reduced_motion: bool,
}

/// One frame's resolved pet.
#[derive(Clone, Copy, Debug)]
pub struct PetFrame {
    /// Opacity 0..=255 (0 ⇒ nothing to draw).
    pub alpha: u8,
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
}

impl PetFrame {
    /// Quantized fingerprint for the host's repaint key: non-zero while anything
    /// is animating, and byte-stable (so it settles) once nothing is.
    #[must_use]
    pub fn fp(&self) -> u64 {
        if self.alpha == 0 {
            return 0;
        }
        let q = |v: f32, s: f32| ((v * s) as i64) as u64;
        let mut h = 0xCBF2_9CE4_8422_2325u64;
        for value in [
            u64::from(self.alpha),
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
        h
    }
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
}

/// The pet's decision layer: a caret follower with a behaviour state machine on
/// top. `tick` is the whole API; everything else is derived from it.
pub struct PetBrain {
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
    clock: f32,

    /// The caret cell as of the previous tick — the pet's own move sensor.
    last_caret: Option<(u16, u16)>,
    last_now: Option<Instant>,

    flight: Option<Flight>,
    /// Seconds remaining in the landing recovery.
    land_t: f32,

    /// Direction-reversal detector for the frolic.
    last_dir: i8,
    reversals: u8,
    reversal_t: f32,

    content: f32,
    /// A jump was seen; pounce as soon as the pet is free to act on it.
    pending_pounce: bool,
    /// Whether the pet has been seen at all yet (drives the fade-in).
    alpha: f32,

    /// The `(coat, iris)` this appearance is WEARING — the pet's copy of the
    /// flying kitty's per-appearance look latch (`kitty_cursor`'s two-path
    /// rule: one appearance wears one cat). `None` until the host first
    /// dresses it.
    worn: Option<(u8, u8)>,
    /// A host sync that arrived mid-appearance, parked until the fade
    /// envelope returns to zero.
    pending_worn: Option<(u8, u8)>,
}

impl Default for PetBrain {
    fn default() -> Self {
        Self {
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
            last_caret: None,
            last_now: None,
            flight: None,
            land_t: 0.0,
            last_dir: 0,
            reversals: 0,
            reversal_t: 0.0,
            content: 0.0,
            pending_pounce: false,
            alpha: 0.0,
            worn: None,
            pending_worn: None,
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
        self.clock += elapsed;

        let width = art_cols(sense.cell_w, sense.cell_h);
        let Some((cr, cc)) = sense.caret else {
            // No caret: settle, fade out, hold position — and FORGET where the
            // caret was. Keeping it would make the next sighting diff against a
            // cell from before the hide: a full-screen app that hides the cursor
            // and a fresh prompt that shows it back at column 3 would read as a
            // 78-column retreat, and the pet would fade in already flinching
            // from a backspace nobody typed. A caret that went away and came
            // back is a NEW sighting, not a move.
            self.alpha = (self.alpha - dt / FADE_OUT).max(0.0);
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
            self.enter_settled(dt);
            return self.emit(sense, width);
        };
        self.alpha = (self.alpha + dt / FADE_IN).min(1.0);

        // First sighting: materialise at the station rather than sliding in from
        // the origin.
        let prev = self.last_caret;
        if prev.is_none() {
            self.col = Self::station(cc, sense.cols, width);
            self.row = f32::from(cr);
        }
        self.last_caret = Some((cr, cc));

        // ── the move sensor ────────────────────────────────────────────────
        let moved = prev.is_some_and(|(pr, pc)| pr != cr || pc != cc);
        if moved {
            let (pr, pc) = prev.expect("moved implies a previous caret");
            let dc = f32::from(cc) - f32::from(pc);
            let dr = f32::from(cr) - f32::from(pr);
            self.on_move(dr, dc, sense.reduced_motion);
            self.quiet = 0.0;
        } else {
            self.quiet += elapsed;
        }

        if sense.reduced_motion {
            // No chase, no gait, no arc: the pet simply IS at its station.
            self.col = Self::station(cc, sense.cols, width);
            self.row = f32::from(cr);
            self.speed = 0.0;
            self.flight = None;
            self.land_t = 0.0;
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

        self.action_t += dt;
        self.land_t = (self.land_t - dt).max(0.0);

        let target = Self::station(cc, sense.cols, width);
        let target_row = f32::from(cr);

        // ── flight owns the position while it lasts ────────────────────────
        if let Some(mut f) = self.flight {
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
                self.land_t = LAND_DUR;
                self.set_action(PetAction::Land);
            } else {
                f.t = f.t.min(f.dur);
                self.flight = Some(f);
                self.set_action_keep(PetAction::Leap);
            }
            return self.emit(sense, width);
        }

        // ── one-shot holds ─────────────────────────────────────────────────
        match self.action {
            PetAction::Crouch if self.action_t >= CROUCH_DUR => {
                self.launch(target, target_row);
                return self.emit(sense, width);
            }
            PetAction::Crouch => return self.emit(sense, width),
            PetAction::Startle if self.action_t < STARTLE_HOLD => {
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Frolic if self.action_t < FROLIC_HOLD => {
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Waking if self.action_t < WAKE_DUR => {
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Perk if self.action_t < PERK_HOLD => {
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Land if self.land_t > 0.0 => {
                // Landing recovery: the pet is on its feet but still absorbing.
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            _ => {}
        }

        // ── a row change is always a hop ───────────────────────────────────
        if (target_row - self.row).abs() >= 0.5 {
            self.begin_flight(target, target_row, true);
            return self.emit(sense, width);
        }

        // ── the chase ──────────────────────────────────────────────────────
        let gap = target - self.col;

        // Out of walking range: gather and pounce rather than sprinting the
        // whole way. Two ways in — a LATCHED jump (set when the move was seen,
        // so it outlives any hold it landed during) or a standing gap that has
        // simply become too big to run down. Both are consumed here, after every
        // hold, which is what keeps a leap from silently degrading into a walk.
        if (self.pending_pounce || gap.abs() >= POUNCE_GAP) && gap.abs() > ARRIVED {
            self.pending_pounce = false;
            self.set_action(PetAction::Crouch);
            return self.emit(sense, width);
        }
        self.pending_pounce = false;

        let want = (gap.abs() * CHASE_GAIN).min(MAX_SPEED) * gap.signum();
        let k = 1.0 - (-dt / SPEED_TAU).exp();
        self.speed += (want - self.speed) * k;
        let step = self.speed * dt;
        self.col += step;
        let travelled = step.abs();
        self.stride = (self.stride + travelled / STRIDE_CELLS).rem_euclid(1024.0);
        self.content = (self.content + travelled * CONTENT_PER_CELL).min(1.0);

        // Overshoot guard: a follower must never oscillate around its station.
        if (target - self.col).signum() != gap.signum() {
            self.col = target;
            self.speed *= 0.25;
        }

        if self.speed.abs() > FLIP_SPEED {
            self.facing_left = self.speed < 0.0;
        }

        let arrived = (target - self.col).abs() <= ARRIVED && self.speed.abs() < FLIP_SPEED;
        if arrived {
            self.content = (self.content - CONTENT_DECAY * dt).max(0.0);
            self.enter_settled(dt);
        } else {
            self.content = (self.content - CONTENT_MOVING_DECAY * dt).max(0.0);
            let fast = match self.action {
                PetAction::Run => self.speed.abs() > RUN_SPEED - GAIT_HYST,
                _ => self.speed.abs() > RUN_SPEED + GAIT_HYST,
            };
            self.set_action_keep(if fast {
                PetAction::Run
            } else {
                PetAction::Walk
            });
        }

        self.emit(sense, width)
    }

    /// React to an observed caret move of `(dr, dc)` cells.
    fn on_move(&mut self, dr: f32, dc: f32, reduced: bool) {
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
        }
        if reduced {
            return;
        }

        let woke = self.action == PetAction::Sleep;
        if woke {
            self.set_action(PetAction::Waking);
            return;
        }

        // A retreat startles — and an already-startled pet EXTENDS the hold
        // rather than re-firing, so a held backspace is one long fright.
        if dr == 0.0 && dc <= -STARTLE_COLS {
            if self.action == PetAction::Startle {
                self.action_t = 0.0;
            } else {
                self.set_action(PetAction::Startle);
            }
            self.facing_left = true;
            return;
        }

        if self.reversals >= FROLIC_REVERSALS && self.action != PetAction::Frolic {
            self.reversals = 0;
            self.set_action(PetAction::Frolic);
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
            } else {
                PetAction::Sit
            }
        } else {
            // Just arrived: hold the last gait's standing frame for a beat so the
            // stop reads as a stop rather than an instant fold.
            PetAction::Walk
        };
        self.set_action_keep(next);
    }

    /// Begin a pounce/hop toward `(to_col, to_row)`.
    fn begin_flight(&mut self, to_col: f32, to_row: f32, vertical: bool) {
        let span = (to_col - self.col).abs();
        let (dur, arc) = if vertical {
            // Clamped exactly like the horizontal branch, and for a sharper
            // reason: `span` here is the HORIZONTAL distance, and the commonest
            // vertical move — a wrap or Enter from the end of a long line — has
            // a span of nearly the whole grid width. Unclamped, that is seconds
            // of flight on one flat arc, during which the flight block owns the
            // position and every caret move you make is ignored. A row change
            // must always read as one hop.
            (
                (HOP_DUR + span * FLIGHT_PER_CELL * 0.5).clamp(HOP_DUR, FLIGHT_MAX),
                HOP_ARC,
            )
        } else {
            (
                (FLIGHT_MIN + span * FLIGHT_PER_CELL).clamp(FLIGHT_MIN, FLIGHT_MAX),
                (ARC_BASE + span * ARC_PER_CELL).min(ARC_MAX),
            )
        };
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
        });
        self.set_action(PetAction::Leap);
    }

    fn launch(&mut self, to_col: f32, to_row: f32) {
        self.begin_flight(to_col, to_row, false);
    }

    fn set_action(&mut self, a: PetAction) {
        if a != self.action {
            self.action = a;
        }
        self.action_t = 0.0;
    }

    /// Change action WITHOUT resetting the hold clock — for the continuous
    /// states (walk/run/leap) whose `action_t` is not a one-shot timer.
    fn set_action_keep(&mut self, a: PetAction) {
        if a != self.action {
            self.action = a;
            self.action_t = 0.0;
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
                let s = (TAU * self.clock / BREATH_PERIOD).sin();
                scale_y += s * BREATH_DEPTH;
                scale_x -= s * BREATH_DEPTH * 0.5;
                if s >= 0.0 {
                    PetGlyphId::PetSleep1
                } else {
                    PetGlyphId::PetSleep0
                }
            }
            PetAction::Waking => PetGlyphId::PetStretch,
            PetAction::Sit if sense.reduced_motion => PetGlyphId::PetSit,
            PetAction::Sit => {
                // The tail flick: a settled cat is never perfectly inert.
                // Phased on QUIET, not on the free-running clock, so it is a
                // function of this settle rather than of how long the window has
                // been open — which is what makes the sequence reproducible.
                let beat = ((self.quiet - SIT_AFTER).max(0.0) * 0.5).rem_euclid(1.0);
                if beat > 0.86 {
                    PetGlyphId::PetSitFlick
                } else {
                    PetGlyphId::PetSit
                }
            }
            PetAction::Purr => {
                let s = (TAU * PURR_HZ * self.clock).sin();
                scale_y += s * PURR_DEPTH;
                scale_x -= s * PURR_DEPTH * 0.6;
                PetGlyphId::PetPurr
            }
            PetAction::Groom => PetGlyphId::PetGroom,
            PetAction::Perk => PetGlyphId::PetPerk,
            PetAction::Startle => PetGlyphId::PetStartle,
            PetAction::Frolic => {
                if ((self.action_t / (FROLIC_HOLD * 0.5)) as u32).is_multiple_of(2) {
                    PetGlyphId::PetPlaybow
                } else {
                    PetGlyphId::PetBat
                }
            }
            PetAction::Crouch => PetGlyphId::PetCrouch,
            PetAction::Leap => {
                if let Some(f) = self.flight {
                    let u = (f.t / f.dur).clamp(0.0, 1.0);
                    // A parabola through 0 at both ends, peaking at u = 0.5.
                    lift = f.arc * 4.0 * u * (1.0 - u);
                    // Stretch into the rise, gather at the apex.
                    let rise = (1.0 - 2.0 * u).abs();
                    scale_y += 0.10 * rise;
                    scale_x -= 0.06 * rise;
                }
                PetGlyphId::PetLeap
            }
            PetAction::Land => {
                let u = 1.0 - (self.land_t / LAND_DUR).clamp(0.0, 1.0);
                let q = LAND_SQUASH * (-3.0 * u).exp() * (TAU * 1.35 * u).cos();
                scale_y *= 1.0 - q;
                scale_x *= 1.0 + q * 0.6;
                PetGlyphId::PetLand
            }
            PetAction::Walk => Self::CYCLE_WALK[self.gait_index(Self::CYCLE_WALK.len())],
            PetAction::Run => Self::CYCLE_RUN[self.gait_index(Self::CYCLE_RUN.len())],
        };

        PetFrame {
            alpha: (self.alpha.clamp(0.0, 1.0) * 255.0) as u8,
            action: self.action,
            pose,
            col: self.col,
            row: self.row,
            lift,
            facing_left: self.facing_left,
            scale_x,
            scale_y,
            purr,
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

    /// Host sync for the pet's collected identity — the flying kitty's
    /// per-appearance look latch (`kitty_cursor::CursorCat::set_look`),
    /// extended to the pet: ONE APPEARANCE WEARS ONE CAT. The host passes
    /// this frame's GLOBAL `(coat, iris)` verdict and draws whatever comes
    /// back.
    ///
    /// While the fade envelope is at zero (or the pet has never been dressed)
    /// there is nothing on screen to protect, so the sync applies
    /// immediately. While the pet is visible, a differing verdict PARKS: the
    /// walking cat keeps its coat, and the parked pair lands once the
    /// envelope returns to zero (`tick`'s no-caret arm). The host re-syncs
    /// every emission, so the parking slot always holds the latest verdict,
    /// never a stale intermediate. A typed discovery in pet mode therefore
    /// latches SILENTLY for the next appearance — the pet has no
    /// collection-hello presentation, by design.
    pub fn sync_look(&mut self, coat: u8, iris: u8) -> (u8, u8) {
        let pair = (coat, iris);
        match self.worn {
            Some(worn) if self.alpha > 0.0 => {
                self.pending_worn = (pair != worn).then_some(pair);
                worn
            }
            _ => {
                self.worn = Some(pair);
                self.pending_worn = None;
                pair
            }
        }
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
        if self.flight.is_some() || self.land_t > 0.0 || self.speed.abs() > 0.01 {
            return true;
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

    fn sense(now: Instant, caret: Option<(u16, u16)>) -> PetSense {
        PetSense {
            now,
            caret,
            rows: 30,
            cols: 100,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
        }
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

    /// REGRESSION: the vertical branch of `begin_flight` had no duration clamp,
    /// and its `span` is the HORIZONTAL distance — so an Enter from the end of a
    /// long line floated the cat for seconds, ignoring every move meanwhile.
    #[test]
    fn a_wrap_from_a_long_line_is_one_hop_not_a_glide() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 190);
        let mut airborne = 0;
        for _ in 0..200 {
            t += Duration::from_millis(16);
            // Enter at the end of a 200-column line: row +1, column 0.
            let mut s = sense(t, Some((5, 0)));
            s.cols = 200;
            let f = pet.tick(s);
            if f.action == PetAction::Leap {
                airborne += 1;
            }
        }
        let secs = airborne as f32 * 0.016;
        assert!(
            secs <= FLIGHT_MAX + 0.05,
            "a row hop must be bounded by FLIGHT_MAX, floated {secs:.2}s"
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
                    assert!(f.lift > 0.0 || f.pose == PetGlyphId::PetLeap);
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

    #[test]
    fn a_backspace_startles_the_pet_and_turns_it_around() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, _) = type_run(&mut pet, start, 2, 10, 8, 0.06);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((2, 17))));
        assert_eq!(f.action, PetAction::Startle);
        assert!(f.facing_left, "it flinches back the way the caret went");
        assert_eq!(f.pose, PetGlyphId::PetStartle);
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
            action: PetAction::Sit,
            pose: PetGlyphId::PetSit,
            col: 0.0,
            row: 0.0,
            lift: 0.0,
            facing_left: false,
            scale_x: 1.0,
            scale_y: 1.0,
            purr: 0.0,
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
                // The appear ramp is not "animation" — sample only once opaque.
                if f.alpha == 255 {
                    fps.insert(f.fp());
                }
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
        for _ in 0..60 {
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
        assert_eq!(pet.sync_look(3, 1), (3, 1), "the first dress applies");
        assert_eq!(
            pet.sync_look(9, 4),
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
            pet.sync_look(9, 4),
            (9, 4),
            "the parked pair landed at zero alpha, so the new appearance wears it"
        );
    }

    /// While hidden the sync applies immediately — there is nothing on screen
    /// to protect (the same arm `kitty_cursor::set_look` documents).
    #[test]
    fn a_hidden_pet_takes_a_look_sync_immediately() {
        let mut pet = PetBrain::default();
        assert!(!pet.is_active(), "a fresh pet is hidden");
        assert_eq!(pet.sync_look(3, 1), (3, 1));
        assert_eq!(
            pet.sync_look(9, 4),
            (9, 4),
            "hidden: no appearance to protect, the swap is free"
        );
        assert_eq!(
            pet.sync_look(9, 4),
            (9, 4),
            "and the agreeing re-sync is a no-op"
        );
    }
}
