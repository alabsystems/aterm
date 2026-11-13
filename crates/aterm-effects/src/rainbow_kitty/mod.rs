// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rainbow kitty cursor companion state machine, sprite baking, and ribbon orchestration.
pub mod companion;
pub mod meteor;
pub mod prism_glass;
pub mod ribbon;
pub mod spine;
pub mod stardust;
pub mod timing;
pub mod witness;

pub use spine::Flow;

use aterm_time::Instant;
use std::time::Duration;

use aterm_render::{BeamVertex, GlowQuad, GlowRamps, RainHalo, RampSink};

use crate::cursor_glow::{Geom, GlowConfig, Light, SoundCue, band_pos, band_row};
use crate::spectrum::spectrum_snap;
use crate::trail_sound::SoundKind;
use meteor::{Meteors, Spawn};
use ribbon::Ribbon;
use spine::Spine;
#[cfg(test)]
use stardust::StarBudget;
use stardust::{GlyphProbe, Stardust};
use timing::{JUMP_MIN_CELLS, SPRING_SNAP_RESPONSE_S, half_life};
use witness::{RowSample, Witness};

const RELIGHT_LIFE_S: f32 = SPRING_SNAP_RESPONSE_S + 8.0 * SPRING_SNAP_RESPONSE_S;
const SPRING_SNAP_RESPONSE_DUR: Duration =
    Duration::from_micros(((SPRING_SNAP_RESPONSE_S as f64) * 1_000_000.0 + 0.5) as u64);
const RELIGHT_LIFE_DUR: Duration =
    Duration::from_micros(((RELIGHT_LIFE_S as f64) * 1_000_000.0 + 0.5) as u64);

/// Travel direction, resolved to the dominant axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Right,
    Left,
    Up,
    Down,
}

impl Dir {
    /// The direction of a cell-space delta, by the dominant axis (D15's rule; a tie goes
    /// horizontal).
    #[inline]
    #[must_use]
    pub const fn of(dcol: i32, drow: i32) -> Self {
        if dcol.abs() >= drow.abs() {
            if dcol > 0 { Self::Right } else { Self::Left }
        } else if drow < 0 {
            Self::Up
        } else {
            Self::Down
        }
    }

    /// The `SoundCue::dir` sign.
    #[inline]
    #[must_use]
    pub const fn sign(self) -> i8 {
        match self {
            Self::Right | Self::Up => 1,
            Self::Left | Self::Down => -1,
        }
    }

    /// True where D15 sends the flight to `ribbon_beam_v`.
    #[inline]
    #[must_use]
    pub const fn is_vertical(self) -> bool {
        matches!(self, Self::Up | Self::Down)
    }
}

/// Why a move was admitted, as the licence gate (`CursorGlow::move_licensed`) and `classify_move`
/// decided upstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Licence {
    Typed,
    Nav,
    Return,
    Synthetic,
    Insert,
    Pty,
}

impl Licence {
    /// The hand's own echo, a typed key's or a delivered insert's.
    #[inline]
    #[must_use]
    pub const fn is_echo(self) -> bool {
        matches!(self, Self::Typed | Self::Insert)
    }

    /// A keyed jump or hop (an arrow, a word motion, a scripted preview).
    #[inline]
    #[must_use]
    pub const fn is_keyed_nav(self) -> bool {
        matches!(self, Self::Nav | Self::Synthetic)
    }
}

/// What kind of glyph an [`Event::Typed`] carried, for the hero deal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedClass {
    Glyph,
    Capital,
    Bang,
    Space,
}

/// The scale of a kill chord.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillScope {
    Word,
    Line,
}

/// One engine event (§17.1).
#[derive(Clone, Copy, Debug)]
pub enum Event {
    Typed {
        cells: u16,
        shifted: bool,
        class: TypedClass,
    },
    Erase,
    Kill {
        cells: u16,
        scope: KillScope,
    },
    Move {
        from: (u16, u16),
        to: (u16, u16),
        licence: Licence,
        dir: Dir,
    },
    Return,
    ComposerNewline,
    CancelComposerNewline,
    Focus(bool),
    ReducedMotion(bool),
    Sweep {
        row: u16,
        col0: u16,
        col1: u16,
    },
    Rewrite {
        row: u16,
        col: u16,
        cells: u16,
    },
}

/// Everything v2 needs from the host's `GlowConfig`, and nothing else.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub dark_theme: bool,
    pub intensity: f32,
    pub duration: Duration,
    pub ribbon_tall: bool,
    pub ribbon_flat: bool,
    pub theme_fg: u32,
    pub theme_bg: u32,
    pub reduced_motion: bool,
    /// The frame's light ([`GlowConfig::light`]).
    pub light: Light,
    /// Where the caret's insertion point sits in its cell across ([`GlowConfig::head_dx`]): a
    /// bar's 0.08, a block's 0.5. The band the ribbon lays under the caret ends there
    /// (`Ribbon::plan_run`'s attach): across the whole cell a block covers, just past a bar.
    pub head_dx: f32,
}

impl Config {
    /// Project the host's `GlowConfig` onto v2's needs.
    #[inline]
    #[must_use]
    pub const fn from_glow(cfg: &GlowConfig, reduced_motion: bool) -> Self {
        Self {
            dark_theme: cfg.dark_theme,
            intensity: cfg.intensity,
            duration: cfg.duration,
            ribbon_tall: cfg.ribbon_tall,
            ribbon_flat: cfg.ribbon_flat,
            theme_fg: cfg.theme_fg,
            theme_bg: cfg.theme_bg,
            reduced_motion,
            light: cfg.light,
            head_dx: cfg.head_dx,
        }
    }
}

/// The caret seam (§7.1).
#[derive(Clone, Copy, Debug)]
pub struct CaretSeam {
    pub flare_at: Option<Instant>,
    pub field_t: f32,
    pub paint: f32,
}

impl CaretSeam {
    /// Unflared caret seam at `field_t = 0.0`, `paint = 0.0`.
    pub const EMPTY: Self = Self {
        flare_at: None,
        field_t: 0.0,
        paint: 0.0,
    };
}

impl Default for CaretSeam {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// One impulse, two bodies (D13, §7.2): the pet or the flying head reads it
/// ([`companion::impulse_for`]).
#[derive(Clone, Copy, Debug)]
pub enum CompanionImpulse {
    Meteor {
        dir: Dir,
        t0: Instant,
        t_flight: Duration,
    },
    Land,
    Wince,
    Delight,
}

/// One tick's output (§17.1).
pub struct Frame<'a> {
    pub under: &'a mut Vec<GlowQuad>,
    /// The ramps laid among `under`'s quads (the ribbon's slabs, the
    /// meteors' colour trains), each slab's rows in a band one ramp.
    pub under_ramps: &'a mut GlowRamps,
    pub out: &'a mut Vec<GlowQuad>,
    /// The ramps laid among `out`'s quads (the meteors' white trains).
    pub out_ramps: &'a mut GlowRamps,
    pub halos: &'a mut Vec<RainHalo>,
    pub beams: &'a mut Vec<BeamVertex>,
    pub cues: &'a mut Vec<SoundCue>,
    pub caret: CaretSeam,
    pub companion: Option<CompanionImpulse>,
    pub fp: u64,
    /// A test's cost for the beams' runs in place of the measured one
    /// ([`RampCost::NEVER`](aterm_render::RampCost::NEVER): the oracle,
    /// every row laid as its quad).
    #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
    pub rows_cost: Option<aterm_render::RampCost>,
}

impl Frame<'_> {
    /// The under stream as a beam's sink.
    #[inline]
    pub(crate) fn under_sink(&mut self) -> RampSink<'_> {
        #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
        let cost = self.rows_cost;
        #[cfg(not(any(test, debug_assertions, feature = "dev-seams")))]
        let cost = None;
        crate::cursor_glow::rows_sink(self.under, self.under_ramps, cost)
    }

    /// The over stream as a beam's sink.
    #[inline]
    pub(crate) fn out_sink(&mut self) -> RampSink<'_> {
        #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
        let cost = self.rows_cost;
        #[cfg(not(any(test, debug_assertions, feature = "dev-seams")))]
        let cost = None;
        crate::cursor_glow::rows_sink(self.out, self.out_ramps, cost)
    }

    /// Lay [`Self::beams`] into the over stream as a comet beam
    /// ([`comet_beam`](aterm_render::comet_beam)) `thick` px across in
    /// `step` px slabs.
    #[inline]
    pub(crate) fn lay_beam(&mut self, clip: aterm_render::BeamClip, thick: f32, step: usize) {
        #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
        let cost = self.rows_cost;
        #[cfg(not(any(test, debug_assertions, feature = "dev-seams")))]
        let cost = None;
        let mut sink = crate::cursor_glow::rows_sink(self.out, self.out_ramps, cost);
        aterm_render::comet_beam(&mut sink, clip, self.beams, thick, step, 0.0);
    }
}

/// The mend mark (§23 "The mend").
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mend {
    pub at: Instant,
    pub disp: f32,
    pub row: u16,
    pub col: u16,
    pub deletes: u8,
}

/// The per-tick read-only context handed to every producer, so everything born on a frame is
/// priced by one evaluation of the spine.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub now: Instant,
    pub geom: Geom,
    pub cfg: &'a Config,
    pub disp: f32,
    pub birth_disp: f32,
    pub phase: f32,
    pub caret: (u16, u16),
    pub caret_t: f32,
    pub caret_walk: Option<(f32, f32)>,
    pub mend: Option<Mend>,
    pub surge: f32,
    pub flow: Flow,
}

/// How many px clear of the caret's block a dark star keeps
/// ([`Ctx::near_paper_caret`]): an arm ending at the block's edge reads as
/// part of it.
pub const DARK_CARET_GAP_PX: i32 = 3;

/// How many cells either side of the caret's block its row is kept clear of
/// stars on a dark page ([`Ctx::beside_dark_caret`]): the cells the next keys
/// land in. After Solarized Dark's Return at the bottom a faint navy star
/// stood two cells past the new caret on its row
/// (`rainbow-kitty-pet-dark-enter-bottom.e120`, looks9 Q10). A landing's
/// sparks on a dark page still fly from the block itself: they are the
/// landing, thrown from the caret.
pub const DARK_CARET_ROW_CELLS: i32 = 3;

impl Ctx<'_> {
    /// Whether a light at `(x, y)` reaching `reach` px stands on the caret:
    /// on paper within a cell of its block, every side; on a dark page on
    /// the block's own cell or within [`DARK_CARET_GAP_PX`] of it (after the
    /// gallery's dark Return a fan star's crosshair stood at the new caret's
    /// corner, looks7 M3).
    #[inline]
    #[must_use]
    pub const fn near_paper_caret(&self, (x, y): (i32, i32), reach: i32) -> bool {
        self.caret_box_meets((x, y), reach, 0)
    }

    /// Whether a light at `(x, y)` reaching `reach` px stands on a dark
    /// page's caret row within [`DARK_CARET_ROW_CELLS`] of its block, either
    /// side, where the next keys land (looks9 Q10); `false` on paper. A star
    /// reads this once, at its first paint ([`super::stardust::Star`]'s
    /// `beside_caret`): read every frame, a lit star the caret typed past
    /// went dark and came back, and one born beside a resting caret popped
    /// in mid-life once typing moved the caret on (looks9 round 3's review).
    #[inline]
    #[must_use]
    pub const fn beside_dark_caret(&self, (x, y): (i32, i32), reach: i32) -> bool {
        self.cfg.dark_theme
            && self.caret_box_meets((x, y), reach, DARK_CARET_ROW_CELLS * self.geom.cw as i32)
    }

    /// Whether `(x, y)` reaching `reach` px meets the caret's block grown by
    /// its margin (a cell on paper, [`DARK_CARET_GAP_PX`] on a dark page),
    /// along its row by `wide` px either side where that is wider.
    #[inline]
    const fn caret_box_meets(&self, (x, y): (i32, i32), reach: i32, wide: i32) -> bool {
        let (row, col) = self.caret;
        if row as usize >= self.geom.rows || col as usize >= self.geom.cols {
            return false;
        }
        let (cw, ch) = (self.geom.cw as i32, self.geom.ch as i32);
        let x0 = self.geom.origin_x as i32 + col as i32 * cw;
        let y0 = self.geom.origin_y as i32 + row as i32 * ch;
        let (mx, my) = if self.cfg.dark_theme {
            (DARK_CARET_GAP_PX, DARK_CARET_GAP_PX)
        } else {
            (cw, ch)
        };
        let mx = if wide > mx { wide } else { mx };
        x + reach >= x0 - mx
            && x - reach < x0 + cw + mx
            && y + reach >= y0 - my
            && y - reach < y0 + ch + my
    }
}

/// `trail status`'s v2 rows (seam point 12).
#[derive(Clone, Copy, Debug)]
pub struct Status {
    pub quads: u32,
    pub halos: u32,
    pub stars: u32,
    pub meteors: u32,
    pub cells: u32,
    pub bridged: u32,
    pub retired: u64,
    pub followed: u64,
    pub follow_missed: u64,
    pub disp: f32,
    pub flow: Flow,
    pub tokens: f32,
    pub fp: u64,
}

impl Status {
    /// Zeroed status snapshot.
    pub const EMPTY: Self = Self {
        quads: 0,
        halos: 0,
        stars: 0,
        meteors: 0,
        cells: 0,
        bridged: 0,
        retired: 0,
        followed: 0,
        follow_missed: 0,
        disp: 0.0,
        flow: Flow::EMPTY,
        tokens: 0.0,
        fp: 0,
    };
}

impl Default for Status {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// The frame cadence a producer asks for while something on glass moves every frame.
pub const FRAME_CADENCE: Duration = Duration::from_micros(8_333);

/// The tail floor.
pub const TAIL_FLOOR: Duration = Duration::from_millis(45);

/// No busy re-arm.
pub const ARM_MIN: Duration = Duration::from_millis(1);

/// A producer's answer to "when do you next change what is on glass?", folded from three kinds of
/// offer.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Cadence {
    now: Instant,
    best: Option<Instant>,
    brisk: bool,
}

impl Cadence {
    /// An empty fold at `now`.
    #[inline]
    #[must_use]
    pub(crate) const fn at(now: Instant) -> Self {
        Self {
            now,
            best: None,
            brisk: false,
        }
    }

    /// Per-frame motion is live.
    #[inline]
    pub(crate) const fn brisk(&mut self) {
        self.brisk = true;
    }

    /// An exact instant.
    #[inline]
    pub(crate) fn edge(&mut self, at: Instant) {
        if at > self.now && !matches!(self.best, Some(b) if at >= b) {
            self.best = Some(at);
        }
    }

    /// A fade whose next visible step is `step_s` away.
    #[inline]
    pub(crate) fn tail(&mut self, step_s: f32) {
        const TAIL_FLOOR_S: f32 = TAIL_FLOOR.as_secs_f32();
        let at = if step_s.is_finite() && step_s > TAIL_FLOOR_S {
            crate::effect_util::saturating_add_secs_f32(self.now, step_s)
        } else {
            crate::effect_util::saturating_add_duration(self.now, TAIL_FLOOR)
        };
        self.edge(at);
    }

    /// A tail whose next visible step is the instant `at`.
    #[inline]
    pub(crate) fn tail_at(&mut self, at: Instant) {
        self.tail(at.saturating_duration_since(self.now).as_secs_f32());
    }

    /// True once [`Self::best`] is at or before `now + TAIL_FLOOR`, so no further
    /// [`Self::tail`] or [`Self::tail_at`] can move it earlier.
    #[inline]
    #[must_use]
    pub(crate) fn at_tail_floor(&self) -> bool {
        let floor = crate::effect_util::saturating_add_duration(self.now, TAIL_FLOOR);
        matches!(self.best, Some(b) if b <= floor)
    }

    /// The fold: the next frame when brisk, else the earliest offer.
    #[inline]
    #[must_use]
    pub(crate) fn take(self) -> Option<Instant> {
        if self.brisk {
            Some(crate::effect_util::saturating_add_duration(
                self.now,
                FRAME_CADENCE,
            ))
        } else {
            self.best
        }
    }
}

/// Seconds until a `spend` fade (`(1 − u)²`, §2.5) next changes its u8 level.
#[inline]
#[must_use]
pub(crate) const fn level_step_spend(peak: f32, u: f32, span_s: f32) -> Option<f32> {
    if !(peak.is_finite() && span_s.is_finite() && u.is_finite()) || peak <= 0.0 || span_s <= 0.0 {
        return None;
    }
    let u = timing::clamp01(u);
    let level = (peak * timing::spend(u)).round();
    if level < 1.0 {
        return None;
    }
    let u_next = 1.0 - crate::color_math::sqrt_f32((level - 0.5) / peak);
    Some(((u_next - u) * span_s).max(0.0))
}

/// The echo patience, seconds, before a press is forgotten as swallowed.
pub const ECHO_PATIENCE_S: f32 = crate::cursor_glow::IN_FLIGHT_PATIENCE_S;

/// The most cells one observed move may lay from the ledger.
pub const ECHO_LEDGER_DEPTH: usize = crate::cursor_glow::TYPED_STAMP_DEPTH;

/// The ribbon's birth floor, seconds.
pub const SWEEP_BIRTH_FLOOR_S: f32 = crate::cursor_glow::CursorGlow::TYPE_HINT_FRESH;
const SWEEP_BIRTH_FLOOR_DUR: Duration =
    Duration::from_nanos((SWEEP_BIRTH_FLOOR_S as f64 * 1_000_000_000.0).round() as u64);

/// The echo ledger.
#[derive(Clone, Copy, Debug)]
pub struct EchoLedger {
    slots: [Option<(Instant, u16)>; ECHO_LEDGER_DEPTH],
    #[cfg(test)]
    tally: LedgerTally,
}

impl Default for EchoLedger {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
struct LedgerTally {
    banked: u32,
    spent: u32,
    forfeited: u32,
    expired: u32,
}

#[cfg(test)]
impl LedgerTally {
    const EMPTY: Self = Self {
        banked: 0,
        spent: 0,
        forfeited: 0,
        expired: 0,
    };
}

impl EchoLedger {
    /// Empty echo ledger.
    pub const EMPTY: Self = Self {
        slots: [None; ECHO_LEDGER_DEPTH],
        #[cfg(test)]
        tally: LedgerTally::EMPTY,
    };
    #[inline]
    const fn bank(&mut self, at: Instant, cells: u16) {
        if cells == 0 {
            return;
        }
        #[cfg(test)]
        {
            self.tally.banked += cells as u32;
        }
        let mut free = 0;
        while free < ECHO_LEDGER_DEPTH {
            if self.slots[free].is_none() {
                self.slots[free] = Some((at, cells));
                return;
            }
            free += 1;
        }
        #[cfg(test)]
        {
            self.tally.forfeited += match self.slots[0] {
                Some((_, c)) => c as u32,
                None => 0,
            };
        }
        let mut i = 0;
        while i + 1 < ECHO_LEDGER_DEPTH {
            self.slots[i] = self.slots[i + 1];
            i += 1;
        }
        self.slots[ECHO_LEDGER_DEPTH - 1] = Some((at, cells));
    }

    #[inline]
    fn expire(&mut self, now: Instant) {
        let mut write = 0;
        let mut read = 0;
        while read < ECHO_LEDGER_DEPTH {
            let Some((at, c)) = self.slots[read] else {
                break;
            };
            if now.saturating_duration_since(at) > crate::cursor_glow::IN_FLIGHT_PATIENCE_DUR {
                #[cfg(test)]
                {
                    self.tally.expired += u32::from(c);
                }
                self.slots[read] = None;
            } else {
                if write != read {
                    self.slots[write] = Some((at, c));
                    self.slots[read] = None;
                }
                write += 1;
            }
            read += 1;
        }
    }

    #[inline]
    const fn total(&self) -> usize {
        let mut sum = 0;
        let mut i = 0;
        while i < ECHO_LEDGER_DEPTH {
            let Some((_, cells)) = self.slots[i] else {
                break;
            };
            sum += cells as usize;
            i += 1;
        }
        sum
    }

    #[inline]
    fn older_than(&self, key: Instant) -> usize {
        self.older_and_total(key).0
    }

    #[inline]
    fn older_and_total(&self, key: Instant) -> (usize, usize) {
        let (mut older, mut total) = (0, 0);
        let mut i = 0;
        while i < ECHO_LEDGER_DEPTH {
            let Some((at, cells)) = self.slots[i] else {
                break;
            };
            let c = cells as usize;
            if at < key {
                older += c;
            }
            total += c;
            i += 1;
        }
        (older, total)
    }

    #[inline]
    const fn spend(&mut self, mut cells: usize) {
        if cells == 0 {
            return;
        }
        let mut write = 0;
        let mut read = 0;
        while read < ECHO_LEDGER_DEPTH {
            let Some((at, have)) = self.slots[read] else {
                break;
            };
            if cells > 0 {
                let have_usize = have as usize;
                let take = if have_usize < cells {
                    have_usize
                } else {
                    cells
                };
                cells -= take;
                #[cfg(test)]
                {
                    self.tally.spent += take as u32;
                }
                let left = have - take as u16;
                if left > 0 {
                    if write != read {
                        self.slots[write] = Some((at, left));
                        self.slots[read] = None;
                    } else {
                        self.slots[write] = Some((at, left));
                    }
                    write += 1;
                } else {
                    self.slots[read] = None;
                }
            } else {
                if write != read {
                    self.slots[write] = Some((at, have));
                    self.slots[read] = None;
                } else {
                    break;
                }
                write += 1;
            }
            read += 1;
        }
    }

    #[inline]
    const fn clear(&mut self) {
        #[cfg(test)]
        {
            self.tally.forfeited += self.total() as u32;
        }
        let mut i = 0;
        while i < ECHO_LEDGER_DEPTH && self.slots[i].is_some() {
            self.slots[i] = None;
            i += 1;
        }
    }

    #[inline]
    fn forget(&mut self, #[cfg_attr(not(test), allow(unused_variables))] now: Instant) {
        #[cfg(test)]
        self.expire(now);
        self.clear();
    }

    #[cfg(test)]
    fn partition(&self, key: Instant, now: Instant) -> (usize, usize, usize) {
        let (mut older, mut younger, mut stale) = (0, 0, 0);
        let mut i = 0;
        while i < self.slots.len() {
            if let Some((at, cells)) = self.slots[i] {
                let cells = usize::from(cells);
                if now.saturating_duration_since(at) > crate::cursor_glow::IN_FLIGHT_PATIENCE_DUR {
                    stale += cells;
                } else if at < key {
                    older += cells;
                } else {
                    younger += cells;
                }
            }
            i += 1;
        }
        (older, younger, stale)
    }
}

/// The v2 engine (§17.1) `CursorGlow` delegates to at the twelve seam points.
#[derive(Clone, Debug)]
pub struct Engine {
    engaged: bool,
    reduced_motion: bool,
    spine: Spine,
    ribbon: Ribbon,
    stardust: Stardust,
    meteor: Meteors,
    caret_seam: CaretSeam,
    caret: (u16, u16),
    caret_known: bool,
    caret_seen: Option<(u16, u16)>,
    echo: EchoLedger,
    bridged: u32,
    #[cfg(test)]
    last_host_sweep_at: Option<Instant>,
    #[cfg(test)]
    kills: u32,
    erased: (u16, u16),
    mend: Option<spine::EraseMark>,
    pane: Option<(u16, u16)>,
    pane_rows: Option<(u16, u16)>,
    events: Vec<(Event, Instant)>,
    pre_move: Option<(usize, Option<(u16, u16)>)>,
    earned: Vec<((u16, u16), Instant)>,
    pending_cues: Vec<SoundCue>,
    companion: Option<CompanionImpulse>,
    party: Option<(Instant, u8, bool)>,
    paint_at: Option<Instant>,
    fp: u64,
    status: Status,
    brisk: bool,
    witness: Witness,
    retire_scratch: Vec<(u16, u16, Instant)>,
    release_scratch: Vec<(u16, u16, Instant)>,
    restore_scratch: Vec<(u32, Option<Instant>)>,
    restored_scratch: Vec<u32>,
    retired: u64,
    followed: u64,
    follow_missed: u64,
    follow_scratch: Vec<witness::FollowRun>,
    shift_scratch: Vec<witness::ShiftRun>,
    moved_scratch: Vec<(u16, u16, Instant)>,
    hand_lift: Option<(u16, u16)>,
    hand_run: Option<witness::FollowRun>,
    withheld: [u16; witness::WITNESS_ROWS + witness::ARMING_ROWS],
    withheld_n: usize,
    grid_rows: u16,
    flush_clock: Option<(Instant, Option<usize>)>,
    soft_wrap: Option<(ribbon::SoftWrap, Option<Instant>)>,
    lift: Option<(ribbon::Lift, Option<Instant>)>,
    reflow: Option<(ribbon::SoftWrap, (u16, u16))>,
    revive: Option<ribbon::Revive>,
    hidden: bool,
}

/// Whether `ch` is a box-drawing or vertical block-bar border glyph (`│`, `┃`, `║`, `▏`, `▌`, `▐`, `▕`, …) rather than prose content.
#[inline]
#[must_use]
pub(crate) const fn is_box_border_glyph(ch: char) -> bool {
    matches!(ch, '\u{2500}'..='\u{257F}' | '\u{258C}'..='\u{2590}' | '\u{2595}')
}

impl Default for Engine {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// A box in window pixels, `(x0, x1, y0, y1)`.
type PxBox = (i32, i32, i32, i32);

impl Engine {
    /// A disengaged engine with nothing live and a full glint bucket, so the first capital of a
    /// session chimes.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self {
            engaged: false,
            reduced_motion: false,
            spine: Spine::new(),
            ribbon: Ribbon::new(),
            stardust: Stardust::new(),
            meteor: Meteors::new(),
            caret_seam: CaretSeam::EMPTY,
            caret: (0, 0),
            caret_known: false,
            caret_seen: None,
            echo: EchoLedger::EMPTY,
            bridged: 0,
            #[cfg(test)]
            last_host_sweep_at: None,
            #[cfg(test)]
            kills: 0,
            erased: (0, 0),
            mend: None,
            pane: None,
            pane_rows: None,
            events: Vec::new(),
            pre_move: None,
            earned: Vec::new(),
            pending_cues: Vec::new(),
            companion: None,
            paint_at: None,
            fp: 0,
            status: Status::EMPTY,
            brisk: false,
            party: None,
            witness: Witness::new(),
            retire_scratch: Vec::new(),
            release_scratch: Vec::new(),
            restore_scratch: Vec::new(),
            restored_scratch: Vec::new(),
            retired: 0,
            followed: 0,
            follow_missed: 0,
            follow_scratch: Vec::new(),
            shift_scratch: Vec::new(),
            moved_scratch: Vec::new(),
            hand_lift: None,
            hand_run: None,
            withheld: [0; witness::WITNESS_ROWS + witness::ARMING_ROWS],
            withheld_n: 0,
            grid_rows: 0,
            flush_clock: None,
            soft_wrap: None,
            lift: None,
            reflow: None,
            revive: None,
            hidden: false,
        }
    }

    /// `true` when this engine owns the frame.
    #[inline]
    #[must_use]
    pub const fn engaged(&self) -> bool {
        self.engaged
    }

    /// Engage or disengage.
    #[inline]
    pub fn set_engaged(&mut self, on: bool) {
        if self.engaged != on {
            self.engaged = on;
            self.reset();
        }
    }

    /// Seam points 1 and 2.
    #[inline]
    pub fn on_event(&mut self, ev: Event, now: Instant) {
        if !self.engaged {
            return;
        }
        match ev {
            Event::Typed { cells, .. } => {
                let mend = self.spine.mend(now);
                self.spine.advance(now);
                if mend.is_some() {
                    self.mend = mend;
                }
                self.echo.bank(now, cells);
            }
            Event::Erase => {
                self.spine.drain_delete(now);
                self.echo.forget(now);
                self.offer(CompanionImpulse::Wince);
            }
            Event::Kill { .. } => {
                #[cfg(test)]
                {
                    self.kills += 1;
                }
                self.spine.drain_kill(now);
                self.echo.forget(now);
                self.offer(CompanionImpulse::Wince);
            }
            Event::Move {
                from, to, licence, ..
            } => self.on_move(from, to, licence, now),
            Event::ReducedMotion(on) => self.reduced_motion = on,
            Event::Focus(false) => self.echo.forget(now),
            Event::Rewrite { row, col, .. } => {
                self.caret = (row, col);
                self.caret_known = true;
                self.witness.forget_left_of(row, col);
            }
            Event::Focus(true)
            | Event::Return
            | Event::ComposerNewline
            | Event::CancelComposerNewline
            | Event::Sweep { .. } => {}
        }
        if let Some((park, skip)) = self.flush_clock {
            let mut seen_typed = 0usize;
            let mut idx = 0;
            while idx < self.events.len() {
                let (e, at) = &self.events[idx];
                if matches!(e, Event::Typed { .. }) {
                    let after_park = match skip {
                        Some(n) => seen_typed >= n || *at > park,
                        None => *at > park,
                    };
                    if after_park {
                        self.events.insert(idx, (ev, now));
                        return;
                    }
                    seen_typed += 1;
                }
                idx += 1;
            }
        }
        self.events.push((ev, now));
    }

    /// The clock a held park's flush is judged at ([`Engine::on_event`]).
    #[inline]
    pub const fn set_flush_clock(&mut self, at: Option<Instant>) {
        self.flush_clock = match at {
            Some(t) => Some((t, None)),
            None => None,
        };
    }

    /// The clock and buffered-typed-event boundary a held park's flush is judged at ([`Engine::on_event`]).
    #[inline]
    pub const fn set_flush_park(&mut self, park: Option<(Instant, usize)>) {
        self.flush_clock = match park {
            Some((at, skip)) => Some((at, Some(skip))),
            None => None,
        };
    }

    /// Unreplayed [`Event::Typed`] entries currently queued in this tick's event buffer.
    #[inline]
    #[must_use]
    pub const fn buffered_typed_events(&self) -> usize {
        let events = self.events.as_slice();
        let mut n = 0;
        let mut i = 0;
        while i < events.len() {
            if matches!(events[i].0, Event::Typed { .. }) {
                n += 1;
            }
            i += 1;
        }
        n
    }

    /// The soft-wrapped caret ([`ribbon::SoftWrap`]).
    #[inline]
    pub const fn note_soft_wrap(&mut self, sw: ribbon::SoftWrap, key_at: Option<Instant>) {
        if self.engaged {
            self.soft_wrap = Some((sw, key_at));
        }
    }

    /// The lifted word ([`ribbon::Lift`]).
    #[inline]
    pub const fn note_lift(&mut self, lift: ribbon::Lift, key_at: Option<Instant>) {
        if self.engaged {
            self.lift = Some((lift, key_at));
        }
    }

    /// The soft-wrapped word the ribbon keeps for the key that reflows it ([`ribbon::SoftWrap`]),
    /// if any.
    #[inline]
    #[must_use]
    pub const fn soft_wrap_carry(&self) -> Option<ribbon::SoftWrap> {
        self.ribbon.carry()
    }

    /// The soft-wrapped word came down.
    #[inline]
    pub const fn note_reflow(&mut self, sw: ribbon::SoftWrap, to: (u16, u16)) {
        if self.engaged {
            self.reflow = Some((sw, to));
        }
    }

    /// The revive ([`ribbon::Revive`]).
    #[inline]
    pub const fn note_revive(&mut self, rv: ribbon::Revive) {
        if self.engaged {
            self.revive = Some(rv);
        }
    }

    #[inline]
    fn on_move(&mut self, from: (u16, u16), to: (u16, u16), licence: Licence, now: Instant) {
        let host_sweep = self.host_sweep_for(from, to);
        #[cfg(test)]
        {
            self.last_host_sweep_at = host_sweep;
        }
        let bridge = self.echo_bridge(from, to, licence, host_sweep, now);
        let floor = now.checked_sub(SWEEP_BIRTH_FLOOR_DUR).unwrap_or(now);
        if let (Some(hs), Some((Event::Sweep { .. }, at))) = (host_sweep, self.events.last_mut()) {
            *at = hs.max(floor);
        }
        if let Some(col0) = bridge
            && (col0 < from.1 || host_sweep.is_none())
        {
            let birth = match host_sweep {
                Some(at) => at.max(floor),
                None => now,
            };
            self.lay_bridge(from, to, col0, birth, host_sweep.is_some());
        }
        self.record_pre_move(from, to, licence);
        self.caret = to;
        self.caret_known = true;
    }

    #[inline]
    const fn host_sweep_for(&self, from: (u16, u16), to: (u16, u16)) -> Option<Instant> {
        // Vec's deref coercion is not const; retain the explicit slice boundary.
        let events = self.events.as_slice();
        match events.last() {
            Some(&(Event::Sweep { row, col0, col1 }, at))
                if row == to.0 && col0 == from.1 && col1 == to.1 =>
            {
                Some(at)
            }
            _ => None,
        }
    }

    #[inline]
    fn lay_bridge(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        col0: u16,
        birth: Instant,
        host_swept: bool,
    ) {
        let host_end = if host_swept { from.1 } else { to.1 };
        self.bridged = self.bridged.saturating_add(u32::from(host_end - col0));
        self.events.insert(
            0,
            (
                Event::Sweep {
                    row: to.0,
                    col0,
                    col1: to.1,
                },
                birth,
            ),
        );
        if let Some((index, _)) = &mut self.pre_move {
            *index += 1;
        }
    }

    #[inline]
    fn replay_events(&mut self, ctx: &Ctx<'_>) {
        self.ribbon.settle_revoked_parts();
        self.ribbon.set_pane(self.pane);
        self.ribbon.set_pane_rows(self.pane_rows);
        let pre_move = self.pre_move.take();
        if self.events.is_empty() {
            self.soft_wrap = None;
            self.lift = None;
            self.reflow = None;
            self.revive = None;
            return;
        }
        let tail_start = self.unechoed_tail_start();
        let soft = if let Some((sw, key_at)) = self.soft_wrap.take() {
            let mut at_move = None;
            let mut i = 0;
            while i < self.events.len() {
                if matches!(self.events[i].0, Event::Move { from, to, .. } if from == sw.origin && to == sw.landing)
                {
                    at_move = Some(i);
                    break;
                }
                i += 1;
            }
            if let Some(at_move) = at_move {
                let at_key = match key_at {
                    Some(key) => {
                        let mut k = 0;
                        let mut found = None;
                        while k < at_move {
                            let (e, at) = self.events[k];
                            if matches!(e, Event::Typed { .. }) && at == key {
                                found = Some(k);
                                break;
                            }
                            k += 1;
                        }
                        found
                    }
                    None => None,
                };
                Some((sw, at_move, at_key))
            } else {
                None
            }
        } else {
            None
        };
        let lift = if let Some((lift, key_at)) = self.lift.take() {
            let mut at_move = None;
            let mut i = 0;
            while i < self.events.len() {
                if matches!(self.events[i].0, Event::Move { from, to, .. } if from == lift.origin && to == lift.landing)
                {
                    at_move = Some(i);
                    break;
                }
                i += 1;
            }
            if let Some(at_move) = at_move {
                let at_key = match key_at {
                    Some(key) => {
                        let mut k = at_move;
                        let mut found = None;
                        while k > 0 {
                            k -= 1;
                            let (e, at) = self.events[k];
                            if matches!(e, Event::Typed { .. }) && at == key {
                                found = Some(k);
                                break;
                            }
                        }
                        found
                    }
                    None => None,
                };
                Some((lift, at_move, at_key))
            } else {
                None
            }
        } else {
            None
        };
        let reflow = if let Some((sw, to)) = self.reflow.take() {
            let mut found = None;
            let mut i = 0;
            while i < self.events.len() {
                if matches!(self.events[i].0, Event::Move { from, to: t, .. } if from == sw.landing && t == to)
                {
                    found = Some((sw, i));
                    break;
                }
                i += 1;
            }
            found
        } else {
            None
        };
        let revive = if let Some(rv) = self.revive.take() {
            let mut found = None;
            let mut i = 0;
            while i < self.events.len() {
                if matches!(self.events[i].0, Event::Sweep { row, col0, col1 }
                    if row == rv.row && col0 == rv.col0 && col1 == rv.col1)
                {
                    found = Some((rv, i));
                    break;
                }
                i += 1;
            }
            found
        } else {
            None
        };
        if let Some((rv, at_sweep)) = revive {
            let (ev, at) = self.events[at_sweep];
            self.ribbon.revive_sweep(&ev, at, ctx, rv);
        }
        let mut held_cells: u16 = 0;
        let mut soft_held: u16 = 0;
        let events = self.events.as_slice();
        let mut index = 0;
        while index < events.len() {
            let (ev, at) = events[index];
            let Event::Typed { cells, .. } = ev else {
                match (soft, lift, reflow, revive) {
                    (Some((sw, at_move, _)), _, _, _) if at_move == index => {
                        self.ribbon.soft_wrap_move(&ev, at, ctx, sw);
                    }
                    (_, Some((lift, at_move, _)), _, _) if at_move == index => {
                        self.ribbon.lift_move(&ev, at, ctx, lift);
                    }
                    (_, _, Some((sw, at_move)), _) if at_move == index => {
                        self.ribbon.reflow_move(&ev, at, ctx, sw);
                    }
                    (_, _, _, Some((_, at_sweep))) if at_sweep == index => {}
                    _ => self.ribbon.on_event(&ev, at, ctx),
                }
                if matches!(ev, Event::Sweep { .. })
                    && let Some((row, col)) = swept_space_cell(events, index)
                {
                    self.ribbon.observe_space_cell(row, col);
                }
                index += 1;
                continue;
            };
            if index >= tail_start {
                let cell = if self.caret_known {
                    Some((self.caret.0, self.caret.1.saturating_add(held_cells)))
                } else {
                    None
                };
                self.ribbon.hold_typed(&ev, at, ctx, cell);
                held_cells = held_cells.saturating_add(cells);
            } else if let Some((first_move, caret)) = pre_move
                && index < first_move
            {
                match caret {
                    Some(caret) => self.ribbon.on_event(&ev, at, &Ctx { caret, ..*ctx }),
                    None => self.ribbon.hold_typed(&ev, at, ctx, None),
                }
            } else if let Some((sw, _, Some(at_key))) = soft
                && at_key == index
            {
                let caret = (sw.origin.0, sw.origin.1.saturating_add(cells));
                self.ribbon.on_event(&ev, at, &Ctx { caret, ..*ctx });
            } else if let Some((sw, at_move, Some(at_key))) = soft
                && at_key < index
                && index < at_move
            {
                let cell = (sw.landing.0, sw.landing.1.saturating_add(soft_held));
                self.ribbon.hold_typed(&ev, at, ctx, Some(cell));
                soft_held = soft_held.saturating_add(cells);
            } else if let Some((_, _, Some(at_key))) = lift
                && at_key == index
            {
                // The Space that lifted the word: its blank was eaten at the
                // end of the row above, and left of the frame's caret is the
                // blank indent — held, with no cell.
                self.ribbon.hold_typed(&ev, at, ctx, None);
            } else {
                self.ribbon.on_event(&ev, at, ctx);
            }
            index += 1;
        }
    }

    #[inline]
    const fn unechoed_tail_start(&self) -> usize {
        let events = self.events.as_slice();
        let mut i = events.len();
        while i > 0 {
            i -= 1;
            if matches!(events[i].0, Event::Move { .. } | Event::Rewrite { .. }) {
                return i + 1;
            }
        }
        0
    }

    #[inline]
    const fn unechoed_tail(&self) -> &[(Event, Instant)] {
        let events = self.events.as_slice();
        events.split_at(self.unechoed_tail_start()).1
    }

    #[inline]
    const fn is_erase_retreat(&self, from: (u16, u16), to: (u16, u16), licence: Licence) -> bool {
        if !matches!(licence, Licence::Typed) || to.0 != from.0 || to.1 >= from.1 {
            return false;
        }
        let tail = self.unechoed_tail();
        let mut i = 0;
        while i < tail.len() {
            if matches!(tail[i].0, Event::Erase | Event::Kill { .. }) {
                return true;
            }
            i += 1;
        }
        false
    }

    #[inline]
    const fn record_pre_move(&mut self, from: (u16, u16), to: (u16, u16), licence: Licence) {
        let one_shot = !licence.is_echo() || self.is_erase_retreat(from, to, licence);
        if one_shot && self.pre_move.is_none() {
            let caret = if self.caret_known {
                Some(self.caret)
            } else {
                None
            };
            self.pre_move = Some((self.events.len(), caret));
        }
    }

    #[inline]
    fn echo_bridge(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        licence: Licence,
        key_at: Option<Instant>,
        now: Instant,
    ) -> Option<u16> {
        self.echo.expire(now);
        let known = self.caret_known;
        let (mrow, mcol) = if known { self.caret } else { from };
        let insert = licence == Licence::Insert;
        if !licence.is_echo() || from.0 != mrow || to.0 != mrow {
            self.echo.clear();
            return None;
        }
        if to.1 <= from.1 {
            // A retreat, a re-anchor, a rewrite landing left of its launch:
            // not an echo's shape. The presses keep waiting.
            return None;
        }
        let Some(gap) = from.1.checked_sub(mcol) else {
            self.echo.clear();
            return None;
        };
        let advance = usize::from(to.1 - from.1);
        let gap = usize::from(gap);
        if gap + advance > ECHO_LEDGER_DEPTH {
            self.echo.clear();
            return None;
        }
        let paid = match key_at {
            Some(key) if insert => self.echo.older_than(key) == gap,
            Some(key) => {
                let (older, total) = self.echo.older_and_total(key);
                older == gap && advance <= total - gap
            }
            None => gap == 0 && advance <= self.echo.total(),
        };
        if !paid {
            self.echo.clear();
            return None;
        }
        self.echo.spend(if insert { gap } else { gap + advance });
        Some(mcol)
    }

    #[cfg(test)]
    const fn last_host_sweep_at(&self) -> Option<Instant> {
        self.last_host_sweep_at
    }

    /// Every [`Event::Kill`] this engine has received (test-only).
    #[cfg(test)]
    pub(crate) const fn kills(&self) -> u32 {
        self.kills
    }

    /// The focused pane's `(first column, width)`, or `None` for the whole grid.
    #[inline]
    pub const fn set_pane(&mut self, pane: Option<(u16, u16)>) {
        self.pane = pane;
    }

    /// The focused pane's `(first row, rows)`, or `None` for the whole grid.
    #[inline]
    pub const fn set_pane_rows(&mut self, pane_rows: Option<(u16, u16)>) {
        self.pane_rows = pane_rows;
    }

    /// The mirror follows the observed caret across a row.
    #[inline]
    pub const fn observe_caret(&mut self, to: (u16, u16)) {
        if !self.engaged || !self.caret_known || to.0 == self.caret.0 {
            return;
        }
        self.caret_seen = Some(to);
    }

    /// The content witness's walk ([`witness`]).
    #[inline]
    pub fn witness_rows(&mut self, rows: &[RowSample<'_>], now: Instant) -> u32 {
        let withheld_n = std::mem::take(&mut self.withheld_n);
        if !self.engaged || rows.is_empty() {
            return 0;
        }
        self.restore_released_rows(rows);
        let recounted = self.witness.walk(
            self.ribbon.cells(),
            rows,
            &self.withheld[..withheld_n],
            now,
            &mut self.retire_scratch,
            &mut self.release_scratch,
        );
        let mut n = 0u32;
        if !self.retire_scratch.is_empty() {
            let retired = self.ribbon.retire_cells(&self.retire_scratch, now);
            n = n.saturating_add(retired.min(u32::MAX as usize) as u32);
        }
        if !self.release_scratch.is_empty() {
            let released = self.ribbon.release_cells(&self.release_scratch, now);
            n = n.saturating_add(released.min(u32::MAX as usize) as u32);
        }
        n = n.saturating_sub(recounted.min(u32::MAX as usize) as u32);
        if n > 0 {
            self.retired += u64::from(n);
            self.status.retired = self.retired;
        }
        n
    }

    #[inline]
    fn restore_released_rows(&mut self, rows: &[RowSample<'_>]) {
        self.witness
            .restored_runs(self.ribbon.cells(), rows, &mut self.restore_scratch);
        if !self.restore_scratch.is_empty() {
            self.ribbon
                .restore_releases(&self.restore_scratch, &mut self.restored_scratch);
            if !self.restored_scratch.is_empty() {
                self.witness
                    .restored(&self.restored_scratch, self.ribbon.cells());
            }
        }
    }

    /// An exact returning typed cell still occupies its column while its partial release waits
    /// for the frame's queued edits to finish. This query lifts no stamp and renews no clock.
    #[inline]
    pub(crate) fn returning_partial_cell(
        &self,
        row: u16,
        col: u16,
        cols: &[char],
        now: Instant,
    ) -> bool {
        let cells = self.ribbon.cells();
        let mut i = 0;
        while i < cells.len() {
            let cell = &cells[i];
            if cell.row == row
                && cell.col == col
                && cell.typing
                && self.witness.released_cell_matches(cell, cols)
                && self.ribbon.partial_custody_valid(cell, now)
            {
                return true;
            }
            i += 1;
        }
        false
    }

    /// The laid ribbon, read-only.
    #[inline]
    #[must_use]
    pub const fn ribbon(&self) -> &Ribbon {
        &self.ribbon
    }

    /// The laid ribbon, for a test's negative control.
    #[cfg(any(test, debug_assertions))]
    pub(crate) const fn ribbon_mut(&mut self) -> &mut Ribbon {
        &mut self.ribbon
    }

    /// The rows the witness wants, written into `out`.
    #[inline]
    pub fn ribbon_rows_for(&self, arming: Option<u16>, out: &mut [u16]) -> usize {
        self.ribbon_rows_at_caret(arming, None, out)
    }

    #[inline]
    const fn push_unique_row(out: &mut [u16], n: &mut usize, grid_rows: u16, row: u16) {
        if (grid_rows == 0 || row < grid_rows) && *n < out.len() {
            let mut i = 0;
            while i < *n {
                if out[i] == row {
                    return;
                }
                i += 1;
            }
            out[*n] = row;
            *n += 1;
        }
    }

    #[inline]
    const fn carry_back_row(row: u16, carry: u16) -> Option<u16> {
        if carry > 0 {
            row.checked_add(carry)
        } else {
            None
        }
    }

    #[inline]
    const fn lead_ahead_row(row: u16, lead: Option<i32>) -> Option<u16> {
        let Some(d) = lead else {
            return None;
        };
        let r = row as i32 + d;
        if r >= 0 && r <= u16::MAX as i32 {
            Some(r as u16)
        } else {
            None
        }
    }

    /// [`Engine::ribbon_rows_for`] with this frame's caret move `(from, to)`.
    #[inline]
    pub fn ribbon_rows_at_caret(
        &self,
        arming: Option<u16>,
        caret_move: Option<(u16, u16)>,
        out: &mut [u16],
    ) -> usize {
        if !self.engaged {
            return 0;
        }
        let grid_rows = self.grid_rows;
        let budget = if out.len() < witness::WITNESS_ROWS {
            out.len()
        } else {
            witness::WITNESS_ROWS
        };
        let mut n = self.band_rows(&mut out[..budget], caret_move);
        if let Some(row) = arming {
            Self::push_unique_row(out, &mut n, grid_rows, row);
            if let Some(r) = row.checked_sub(1) {
                Self::push_unique_row(out, &mut n, grid_rows, r);
            }
            if let Some(r) = row.checked_add(1) {
                Self::push_unique_row(out, &mut n, grid_rows, r);
            }
        }
        n
    }

    #[inline]
    fn band_rows(&self, out: &mut [u16], caret_move: Option<(u16, u16)>) -> usize {
        let cohorts = self.ribbon.cohorts();
        if cohorts.is_empty() || out.is_empty() {
            return 0;
        }
        let grid_rows = self.grid_rows;
        let caret_row = if self.caret_known {
            Some(self.caret.0)
        } else {
            None
        };
        let mut n = 0usize;
        let mut focus: Option<(u16, (bool, Instant))> = None;
        let mut ci = 0;
        while ci < cohorts.len() {
            let coh = &cohorts[ci];
            let key = (caret_row == Some(coh.row), coh.alive_at);
            if !matches!(focus, Some((_, prev_key)) if prev_key > key) {
                focus = Some((coh.row, key));
            }
            ci += 1;
        }
        let carry = self.witness.scroll_carry();
        let lead = match caret_move {
            Some((from, to)) => {
                let d = i32::from(to) - i32::from(from);
                if d.unsigned_abs() > 2 && d != i32::from(carry) {
                    Some(d)
                } else {
                    None
                }
            }
            None => None,
        };
        if let Some((focus, _)) = focus {
            Self::push_unique_row(out, &mut n, grid_rows, focus);
            if let Some(row) = focus.checked_sub(1) {
                Self::push_unique_row(out, &mut n, grid_rows, row);
            }
            if let Some(row) = focus.checked_add(1) {
                Self::push_unique_row(out, &mut n, grid_rows, row);
            }
            if let Some(row) = Self::carry_back_row(focus, carry) {
                Self::push_unique_row(out, &mut n, grid_rows, row);
            }
            if let Some(row) = Self::lead_ahead_row(focus, lead) {
                Self::push_unique_row(out, &mut n, grid_rows, row);
            }
            if n == out.len() {
                return n;
            }
        }
        let mut i = 0;
        while i < cohorts.len() {
            Self::push_unique_row(out, &mut n, grid_rows, cohorts[i].row);
            if n == out.len() {
                return n;
            }
            i += 1;
        }
        if carry > 0 {
            let mut i = 0;
            while i < cohorts.len() {
                let Some(row) = Self::carry_back_row(cohorts[i].row, carry) else {
                    break;
                };
                Self::push_unique_row(out, &mut n, grid_rows, row);
                if n == out.len() {
                    return n;
                }
                i += 1;
            }
        }
        if lead.is_some() {
            let mut i = 0;
            while i < cohorts.len() {
                if let Some(row) = Self::lead_ahead_row(cohorts[i].row, lead) {
                    Self::push_unique_row(out, &mut n, grid_rows, row);
                    if n == out.len() {
                        return n;
                    }
                }
                i += 1;
            }
        }
        let mut i = 0;
        while i < cohorts.len() {
            let r = cohorts[i].row;
            if let Some(nb) = r.checked_sub(1) {
                Self::push_unique_row(out, &mut n, grid_rows, nb);
                if n == out.len() {
                    return n;
                }
            }
            if let Some(nb) = r.checked_add(1) {
                Self::push_unique_row(out, &mut n, grid_rows, nb);
                if n == out.len() {
                    return n;
                }
            }
            i += 1;
        }
        n
    }

    /// The follow pass.
    #[inline]
    pub fn follow_rows(&mut self, rows: &[RowSample<'_>], asked: &[u16], now: Instant) -> u32 {
        self.follow_rows_with_park(rows, asked, now, None, false).0
    }

    /// The same content follow, with the one exact short held park the host may be waiting to
    /// judge.
    #[inline]
    pub(crate) fn follow_rows_with_park(
        &mut self,
        rows: &[RowSample<'_>],
        asked: &[u16],
        now: Instant,
        park: Option<((u16, u16), (u16, u16))>,
        hand_withheld: bool,
    ) -> (u32, bool) {
        self.withheld_n = 0;
        self.hand_lift = None;
        let lifted = self.hand_run.take();
        self.witness.await_caret(None);
        if !self.engaged || rows.is_empty() {
            return (0, false);
        }
        let mut ai = 0;
        while ai < asked.len() {
            let r = asked[ai];
            let readable = r < self.grid_rows
                && !matches!(self.pane_rows, Some((first, n)) if r < first || r - first >= n);
            if readable && self.withheld_n < self.withheld.len() {
                let mut dup = false;
                let mut wi = 0;
                while wi < self.withheld_n {
                    if self.withheld[wi] == r {
                        dup = true;
                        break;
                    }
                    wi += 1;
                }
                if !dup {
                    let mut present = false;
                    let mut si = 0;
                    while si < rows.len() {
                        if rows[si].row == r {
                            present = true;
                            break;
                        }
                        si += 1;
                    }
                    if !present {
                        self.withheld[self.withheld_n] = r;
                        self.withheld_n += 1;
                    }
                }
            }
            ai += 1;
        }
        self.note_rewrap_starts(rows, park);
        let caret = if self.caret_known {
            Some(self.caret)
        } else {
            None
        };
        let same_caret_row = !matches!(self.caret_seen, Some((r, _)) if r != self.caret.0);
        if self.witness.scroll_carry() == 0
            && self.witness.caret_carry() == 0
            && lifted.is_none()
            && same_caret_row
        {
            self.witness.shift_runs(
                self.ribbon.cells(),
                rows,
                caret,
                now,
                &mut self.shift_scratch,
            );
        } else {
            self.shift_scratch.clear();
        }
        let mut n = 0u32;
        let mut park_proved = false;
        let mut k = 0;
        while k < self.shift_scratch.len() {
            let run = self.shift_scratch[k];
            self.moved_scratch.clear();
            let moved = self.ribbon.shift_run(run, &mut self.moved_scratch);
            if moved > 0 {
                self.witness.shift_cells(&mut self.moved_scratch, run.dc);
                n = n.saturating_add(moved.min(u32::MAX as usize) as u32);
            }
            k += 1;
        }
        let score = self.witness.follow_runs_with_short_park(
            self.ribbon.cells(),
            rows,
            park,
            &mut self.follow_scratch,
        );
        if score.missed > 0 {
            self.follow_missed += u64::from(score.missed);
            self.status.follow_missed = self.follow_missed;
        }
        let hand = match self.hand_rides(rows, hand_withheld, lifted) {
            Some(h) => Some(h),
            None => self.hand_returns(rows),
        };
        self.witness.end_scroll_carry();
        if let Some(h) = hand {
            let mut has_hand = false;
            let mut fi = 0;
            while fi < self.follow_scratch.len() {
                let r = &self.follow_scratch[fi];
                if r.row == h.row && r.cohort == h.cohort && r.dr == h.dr {
                    has_hand = true;
                    break;
                }
                fi += 1;
            }
            if !has_hand {
                self.follow_scratch
                    .retain(|r| r.row != h.row || r.cohort != h.cohort);
                self.follow_scratch.push(h);
            }
        }
        if self.follow_scratch.is_empty() {
            if n > 0 {
                self.followed += u64::from(n);
                self.status.followed = self.followed;
            }
            return (n, false);
        }
        let park_owner = if let Some((from, to)) = park
            && from.0 == to.0
            && (1..=2).contains(&from.1.saturating_sub(to.1))
            && let Some(last_col) = from.1.checked_sub(1)
        {
            let mut owner = None;
            let cells = self.ribbon.cells();
            let mut ci = 0;
            while ci < cells.len() {
                let cell = &cells[ci];
                if !cell.leaving() && cell.row == from.0 && cell.col == last_col {
                    owner = Some((from.0, cell.cohort));
                    break;
                }
                ci += 1;
            }
            owner
        } else {
            None
        };
        let hand_row = match hand {
            Some(h) => h.row,
            None => self.caret.0,
        };
        let mut k = 0;
        while k < self.follow_scratch.len() {
            let run = self.follow_scratch[k];
            k += 1;
            let Some(target) = run.row.checked_add_signed(run.dr) else {
                continue;
            };
            let away = !self.caret_known || target != self.caret.0;
            let park_owns_run = park_owner == Some((run.row, run.cohort));
            self.moved_scratch.clear();
            let moved = self
                .ribbon
                .translate_run(run, now, away, &mut self.moved_scratch);
            if moved == 0 {
                continue;
            }
            if away && park_owns_run {
                park_proved = true;
            }
            if away && self.caret_known && run.row == hand_row {
                self.ribbon.arm_seam(self.caret.0, now);
            }
            self.witness
                .translate_cells(&mut self.moved_scratch, run.dr);
            n = n.saturating_add(moved.min(u32::MAX as usize) as u32);
        }
        if let Some(h) = hand
            && let Some(to) = h.row.checked_add_signed(h.dr)
        {
            self.ribbon.lift_seam(h.row, to);
        }
        if n > 0 {
            self.followed += u64::from(n);
            self.status.followed = self.followed;
        }
        (n, park_proved)
    }

    #[inline]
    fn hand_rides(
        &mut self,
        rows: &[RowSample<'_>],
        hand_withheld: bool,
        lifted: Option<witness::FollowRun>,
    ) -> Option<witness::FollowRun> {
        if lifted.is_some() || !hand_withheld {
            return lifted;
        }
        let Some(run) = self.hand_line_follow(rows, None) else {
            let row = self.caret.0;
            let mut waits = None;
            const DRS: [i16; 4] = [-2, -1, 1, 2];
            let mut di = 0;
            while di < DRS.len() {
                if let Some(to) = row.checked_add_signed(DRS[di])
                    && let Some(found) = self.hand_line_follow(rows, Some(to))
                {
                    waits = Some((found.row, found.cohort));
                    break;
                }
                di += 1;
            }
            self.witness.await_caret(waits);
            return None;
        };
        let to = run.row.checked_add_signed(run.dr)?;
        self.lift_hand_anchors(run.row, to);
        self.hand_lift = Some((run.row, to));
        Some(run)
    }

    #[inline]
    fn hand_returns(&mut self, rows: &[RowSample<'_>]) -> Option<witness::FollowRun> {
        if !self.caret_known {
            return None;
        }
        let mut found = None;
        let mut fi = 0;
        while fi < self.follow_scratch.len() {
            let r = self.follow_scratch[fi];
            if r.row == self.caret.0
                && (r.dr.unsigned_abs() > 2
                    || matches!(
                        self.caret_seen,
                        Some((sr, _)) if r.row.checked_add_signed(r.dr) == Some(sr)
                    ))
            {
                found = Some(r);
                break;
            }
            fi += 1;
        }
        let run = match found {
            Some(run) => run,
            None => {
                let (sr, _) = self.caret_seen?;
                if sr == self.caret.0 {
                    return None;
                }
                let r = self.hand_line_follow(rows, Some(sr))?;
                if r.row != self.caret.0 || r.row.checked_add_signed(r.dr) != Some(sr) {
                    return None;
                }
                r
            }
        };
        let to = run.row.checked_add_signed(run.dr)?;
        self.lift_hand_anchors(run.row, to);
        self.hand_lift = Some((run.row, to));
        Some(run)
    }

    /// The caret's carry ([`witness::Witness::carry_caret`]).
    #[inline]
    pub(crate) const fn carry_caret(&mut self, dr: i16) {
        if self.engaged {
            self.witness.carry_caret(dr);
        }
    }

    /// The follow verdict on the hand's own line this frame.
    #[inline]
    pub(crate) fn hand_line_follow(
        &mut self,
        rows: &[RowSample<'_>],
        rode_to: Option<u16>,
    ) -> Option<witness::FollowRun> {
        if !self.engaged || !self.caret_known || rows.is_empty() {
            return None;
        }
        self.witness
            .follow_hand(self.ribbon.cells(), rows, self.caret, rode_to)
    }

    /// The hand rides its line: its anchors lift from `run.row` by `run.dr`.
    #[inline]
    pub(crate) const fn lift_hand(&mut self, run: witness::FollowRun) {
        let Some(to) = run.row.checked_add_signed(run.dr) else {
            return;
        };
        self.lift_hand_anchors(run.row, to);
        self.hand_run = Some(run);
    }

    /// The hand's anchors lifted with a line that has no typed run to carry: [`Engine::lift_hand`]
    /// with no run.
    #[inline]
    pub(crate) const fn lift_caret(&mut self, row: u16, to: u16) {
        self.lift_hand_anchors(row, to);
    }

    #[inline]
    const fn map_hand_row(r: u16, row: u16, to: u16) -> u16 {
        if r == row { to } else { r }
    }

    #[inline]
    const fn lift_hand_anchors(&mut self, row: u16, to: u16) {
        if row == to {
            return;
        }
        if self.caret_known {
            self.caret.0 = Self::map_hand_row(self.caret.0, row, to);
        }
        if let Some((r, _)) = &mut self.caret_seen {
            *r = Self::map_hand_row(*r, row, to);
        }
        self.erased.0 = Self::map_hand_row(self.erased.0, row, to);
        let earned = self.earned.as_mut_slice();
        let mut i = 0;
        while i < earned.len() {
            earned[i].0.0 = Self::map_hand_row(earned[i].0.0, row, to);
            i += 1;
        }
        self.ribbon.lift_hand(row, to);
        let events = self.events.as_mut_slice();
        let mut ei = 0;
        while ei < events.len() {
            match &mut events[ei].0 {
                Event::Sweep { row: r, .. } | Event::Rewrite { row: r, .. } => {
                    *r = Self::map_hand_row(*r, row, to);
                }
                Event::Move { from, to: t, .. } => {
                    from.0 = Self::map_hand_row(from.0, row, to);
                    t.0 = Self::map_hand_row(t.0, row, to);
                }
                _ => {}
            }
            ei += 1;
        }
        if let Some((lift, _)) = &mut self.lift {
            lift.origin.0 = Self::map_hand_row(lift.origin.0, row, to);
            lift.landing.0 = Self::map_hand_row(lift.landing.0, row, to);
            lift.dst.0 = Self::map_hand_row(lift.dst.0, row, to);
        }
        if let Some(rv) = &mut self.revive {
            rv.row = Self::map_hand_row(rv.row, row, to);
        }
        if let Some((_, caret)) = &mut self.pre_move
            && let Some((r, _)) = caret
        {
            *r = Self::map_hand_row(*r, row, to);
        }
        if let Some((sw, _)) = &mut self.soft_wrap {
            sw.origin.0 = Self::map_hand_row(sw.origin.0, row, to);
            sw.landing.0 = Self::map_hand_row(sw.landing.0, row, to);
        }
        if let Some((sw, reflow_to)) = &mut self.reflow {
            sw.origin.0 = Self::map_hand_row(sw.origin.0, row, to);
            sw.landing.0 = Self::map_hand_row(sw.landing.0, row, to);
            reflow_to.0 = Self::map_hand_row(reflow_to.0, row, to);
        }
    }

    /// The hand's line the last follow pass lifted with the hand, `(from row, to row)`, taken once.
    #[inline]
    pub(crate) const fn take_hand_lift(&mut self) -> Option<(u16, u16)> {
        self.hand_lift.take()
    }

    /// Bind one content-proved held park to the move its flush just queued.
    #[inline]
    pub(crate) const fn prove_short_reanchor(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        at: Instant,
    ) {
        self.ribbon.prove_short_reanchor(from, to, at);
    }

    /// Bind the seam's exact content-and-key proof to this cross-row rewrap.
    #[inline]
    pub(crate) const fn prove_typed_rewrap(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        start: u16,
        width: u16,
        space: Option<u16>,
    ) {
        self.ribbon
            .note_rewrap_start(from, to, start, Some(width), space);
    }

    #[inline]
    fn note_rewrap_starts(
        &mut self,
        rows: &[RowSample<'_>],
        park: Option<((u16, u16), (u16, u16))>,
    ) {
        if self.events.is_empty() && park.is_none() {
            return;
        }
        let pane_col0 = match self.pane {
            Some((c0, _)) => c0,
            None => 0,
        };
        let mut found = None;
        let mut idx = self.events.len();
        while idx > 0 {
            idx -= 1;
            if let Event::Move {
                from, to, licence, ..
            } = self.events[idx].0
                && licence == Licence::Typed
                && from.0 == to.0
                && to.1 < from.1
            {
                found = Some((from, to));
                break;
            }
        }
        if found.is_none()
            && let Some((from, to)) = park
            && from.0 == to.0
            && to.1 < from.1
        {
            found = Some((from, to));
        }
        let Some((from, to)) = found else {
            return;
        };
        let mut sample = None;
        let mut ri = 0;
        while ri < rows.len() {
            if rows[ri].row == to.0 {
                sample = Some(&rows[ri]);
                break;
            }
            ri += 1;
        }
        let Some(sample) = sample else {
            return;
        };
        let mut c = to.1;
        while c < from.1 {
            if matches!(
                sample.cols.get(usize::from(c)),
                Some(&ch) if ch != ' ' && ch != '\0'
            ) {
                return;
            }
            c += 1;
        }
        let mut c = pane_col0;
        let mut skipped_gutter = false;
        while c < to.1 {
            let ch = match sample.cols.get(usize::from(c)) {
                Some(&ch) => ch,
                None => ' ',
            };
            if ch == ' ' || ch == '\0' {
                c += 1;
                continue;
            }
            if !skipped_gutter
                && (is_box_border_glyph(ch)
                    || (!ch.is_alphanumeric()
                        && c + 1 < to.1
                        && matches!(sample.cols.get(usize::from(c + 1)), Some(' '))))
            {
                skipped_gutter = true;
                c += 1;
                continue;
            }
            self.ribbon.note_rewrap_start(from, to, c, None, None);
            break;
        }
    }

    /// The verdict.
    #[inline]
    pub const fn note_verdict(&mut self, now: Instant) {
        if !self.engaged {
            return;
        }
        self.spine.note_verdict(now);
    }

    /// The kitty's Delight edge, reported by the host.
    #[inline]
    pub fn earn_hero(&mut self, now: Instant, row: u16, col: u16) {
        if !self.engaged {
            return;
        }
        let impulse = CompanionImpulse::Delight;
        self.offer(impulse);
        let n = companion::heroes_earned(impulse);
        let mut i = 0;
        while i < n {
            self.earned.push(((row, col), now));
            i += 1;
        }
    }

    /// Pin the spine to at least `floor` while a sing-along is armed.
    #[inline]
    pub fn celebrate(&mut self, now: Instant, floor: f32) {
        if !self.engaged {
            return;
        }
        self.spine.drive(now, floor);
    }

    /// The grief gate, [`Self::celebrate`]'s inverse.
    #[inline]
    pub fn hush_fanfare(&mut self, now: Instant) {
        if !self.engaged {
            return;
        }
        self.spine.hush(now);
        self.party = None;
        self.earned.clear();
        if matches!(self.companion, Some(CompanionImpulse::Delight)) {
            self.companion = None;
        }
    }

    /// Queue a sing-along bar fan of `n` stars (a landing on a `ring` bar), staged at the caret on
    /// the next tick.
    #[inline]
    pub const fn party(&mut self, now: Instant, n: u8, ring: bool) {
        if !self.engaged {
            return;
        }
        self.party = Some((now, n, ring));
    }

    #[inline]
    const fn offer(&mut self, impulse: CompanionImpulse) {
        if !matches!(
            self.companion,
            Some(CompanionImpulse::Meteor { .. } | CompanionImpulse::Land)
        ) {
            self.companion = Some(impulse);
        }
    }

    /// Seam point 3, the emit phase.
    #[inline]
    pub fn tick(&mut self, now: Instant, geom: Geom, cfg: &Config, out: &mut Frame<'_>) {
        self.tick_with_restored_rows(now, geom, cfg, out, &[]);
    }

    /// The host's sampled rows can lift a prior partial release on the very frame that shows its
    /// text again, after queued edits have replayed.
    #[inline]
    pub(crate) fn tick_with_restored_rows(
        &mut self,
        now: Instant,
        geom: Geom,
        cfg: &Config,
        out: &mut Frame<'_>,
        rows: &[RowSample<'_>],
    ) {
        self.ribbon.forget_rails();
        out.caret = self.caret_seam;
        out.companion = self.companion;
        out.fp = 0;
        if !self.engaged {
            self.events.clear();
            self.pre_move = None;
            self.soft_wrap = None;
            self.lift = None;
            self.hand_run = None;
            self.hand_lift = None;
            self.withheld_n = 0;
            self.flush_clock = None;
            self.reflow = None;
            self.revive = None;
            self.earned.clear();
            self.party = None;
            self.mend = None;
            self.caret_seen = None;
            self.fp = 0;
            self.brisk = false;
            return;
        }

        self.spine.update(now);
        self.stardust.budget.refill(now);
        if let Some(t) = self.caret_seam.flare_at
            && now.saturating_duration_since(t) >= SPRING_SNAP_RESPONSE_DUR
        {
            self.caret_seam.flare_at = None;
        }
        if let Some(t) = self.paint_at
            && now.saturating_duration_since(t) >= RELIGHT_LIFE_DUR
        {
            self.paint_at = None;
        }
        let cfg_live = Config {
            reduced_motion: cfg.reduced_motion || self.reduced_motion,
            ..*cfg
        };
        self.grid_rows = geom.rows.min(u16::MAX as usize) as u16;
        let mut keys = 0usize;
        let mut saw_erase = false;
        let mut ei = 0;
        while ei < self.events.len() {
            match self.events[ei].0 {
                Event::Erase => saw_erase = true,
                Event::Typed { .. } => keys += 1,
                _ => {}
            }
            ei += 1;
        }
        if saw_erase {
            self.erased = self.caret;
        }
        let mend = match self.mend.take() {
            Some(m) => Some(Mend {
                at: m.at,
                disp: m.disp,
                row: self.erased.0,
                col: self.erased.1,
                deletes: m.count,
            }),
            None => None,
        };
        let price = self.spine.birth_disp();
        self.spine
            .note_keys(now, keys.min(u32::MAX as usize) as u32, price);
        let mut ctx = Ctx {
            now,
            geom,
            cfg: &cfg_live,
            disp: self.spine.disp(),
            birth_disp: self.spine.birth_disp(),
            phase: self.spine.phase(),
            caret: self.caret,
            caret_t: 0.0,
            caret_walk: None,
            mend,
            surge: self.spine.surge(),
            flow: self.spine.flow(),
        };

        self.replay_events(&ctx);
        if !rows.is_empty() {
            self.restore_released_rows(rows);
        }
        self.ribbon.plan_over(&ctx, Some(self.stardust.probe()));
        ctx.caret_t = self.ribbon.field_at_caret();

        self.meteor.set_hand_floor(self.ribbon.hand_floor_cell());

        if !self.events.is_empty() {
            let mut i = 0;
            while i < self.events.len() {
                let (ev, at) = self.events[i];
                i += 1;
                let Event::Move {
                    from,
                    to,
                    licence,
                    dir,
                } = ev
                else {
                    self.meteor.on_event(&ev, at, &ctx);
                    continue;
                };
                let mut move_ctx = ctx;
                move_ctx.caret_t = self.ribbon.field_at(to.0, to.1).unwrap_or(ctx.caret_t);
                move_ctx.caret_walk = self.ribbon.walk_origin_at(to.0, to.1);
                if let Some(spawn) = self.meteor.on_event(&ev, at, &move_ctx) {
                    self.on_spawn(&spawn, licence, &move_ctx);
                }
                if sub_floor_hop(from, to, licence) {
                    self.pending_cues.push(SoundCue {
                        kind: SoundKind::Navigation,
                        col: to.1,
                        heat: move_ctx.disp,
                        hue: move_ctx.caret_t,
                        dir: dir.sign(),
                        shifted: false,
                    });
                }
            }

            let mut si = 0;
            while si < self.events.len() {
                let (ev, at) = self.events[si];
                self.stardust.on_event(&ev, at, &ctx, &self.ribbon);
                si += 1;
            }
            self.events.clear();
        }
        if !self.earned.is_empty() {
            let mut hi = 0;
            while hi < self.earned.len() {
                let (cell, at) = self.earned[hi];
                self.stardust.earn_hero(at, cell, &ctx, &self.ribbon);
                hi += 1;
            }
            self.earned.clear();
        }
        if let Some((at, n, ring)) = self.party.take()
            && self.caret_known
        {
            self.meteor.party(at, n, ring, &ctx);
        }
        if keys > 0
            && !cfg_live.reduced_motion
            && ctx.flow.combo > stardust::LADDER_GOLD_KEYS
            && ctx.flow.combo.is_multiple_of(stardust::LADDER_GOLD_KEYS)
        {
            self.caret_seam.flare_at = Some(now);
        }
        if let Some(seen) = self.caret_seen.take() {
            self.caret = seen;
            self.echo.forget(now);
        }

        let (u0, o0, h0) = (out.under.len(), out.out.len(), out.halos.len());
        let (ur0, or0) = (out.under_ramps.mark(), out.out_ramps.mark());
        let hidden = std::mem::take(&mut self.hidden);
        self.meteor.emit(&ctx, out);
        self.meteor.sow_into(&mut self.stardust, &ctx);
        self.ribbon.emit(&ctx, out);
        self.stardust.emit(&ctx, out);
        if hidden {
            out.under.truncate(u0);
            out.under_ramps.truncate(ur0);
            out.out.truncate(o0);
            out.out_ramps.truncate(or0);
            out.halos.truncate(h0);
            self.stardust.clear_marks();
            self.ribbon.forget_rails();
            self.ribbon.unlay_glass();
        }
        self.caret_seam.field_t = ctx.caret_t;
        self.caret_seam.paint = self.caret_paint(now);
        out.caret = self.caret_seam;
        out.cues.append(&mut self.pending_cues);
        out.companion = self.companion;

        let ramps_lit = (out.under_ramps.lit - ur0.lit()) + (out.out_ramps.lit - or0.lit());
        self.fp =
            liveness(&out.under[u0..], &out.out[o0..], &out.halos[h0..]) | u64::from(ramps_lit > 0);
        out.fp = self.fp;
        self.status = Status {
            quads: ((out.under.len() - u0) + (out.out.len() - o0) + ramps_lit) as u32,
            halos: (out.halos.len() - h0) as u32,
            stars: self.stardust.live() as u32,
            meteors: self.meteor.live() as u32,
            cells: self.ribbon.live_cells() as u32,
            bridged: self.bridged,
            retired: self.retired,
            followed: self.followed,
            follow_missed: self.follow_missed,
            disp: ctx.disp,
            flow: ctx.flow,
            tokens: self.stardust.budget.tokens(),
            fp: self.fp,
        };
        self.brisk = self.meteor.brisk(now) || self.ribbon.brisk(now) || self.stardust.brisk(now);
    }

    #[inline]
    fn on_spawn(&mut self, spawn: &Spawn, licence: Licence, ctx: &Ctx<'_>) {
        let cells = spawn.cells;
        let (kind, dir) = match licence {
            Licence::Nav | Licence::Synthetic => (
                SoundKind::Meteor {
                    dir: spawn.dir.sign(),
                    cells,
                    armed: false,
                },
                spawn.dir.sign(),
            ),
            Licence::Return => (SoundKind::Enter { cells }, 0),
            Licence::Pty => (SoundKind::Jump, 0),
            Licence::Typed | Licence::Insert => return,
        };
        let credited = licence != Licence::Pty;
        let reduced = ctx.cfg.reduced_motion;
        if credited {
            self.companion = Some(if reduced {
                CompanionImpulse::Land
            } else {
                CompanionImpulse::Meteor {
                    dir: spawn.dir,
                    t0: spawn.t0,
                    t_flight: spawn.t_flight,
                }
            });
            if !reduced {
                self.caret_seam.flare_at = Some(spawn.t0);
            }
            self.paint_at = Some(spawn.t0);
        }
        self.stardust.budget.empty();
        self.pending_cues.push(SoundCue {
            kind,
            col: spawn.landing.1,
            heat: ctx.disp,
            hue: ctx.caret_t,
            dir,
            shifted: false,
        });
    }

    /// Seam point 4 (`rainbow_field`).
    #[inline]
    #[must_use]
    pub fn field(&self) -> f32 {
        self.ribbon.field_at_caret()
    }

    /// Seam point 4.
    #[inline]
    #[must_use]
    pub const fn field_at(&self, row: u16, col: u16) -> Option<f32> {
        self.ribbon.field_at(row, col)
    }

    /// Seam point 5 (`rainbow_head_rgb`).
    #[inline]
    #[must_use]
    pub fn head_rgb(&self, cfg: &Config) -> Option<u32> {
        self.ribbon.head_rgb(cfg)
    }

    /// Seam point 6 (`momentum_display`): the eased spine.
    #[inline]
    #[must_use]
    pub const fn momentum_display(&self) -> f32 {
        self.spine.disp()
    }

    /// `trail status`'s `ribbon_segments=` while v2 owns the frame.
    #[inline]
    #[must_use]
    pub const fn ribbon_segments(&self) -> usize {
        self.ribbon.lit_segment_count()
    }

    /// `trail status`'s `ribbon_drawn=` while v2 owns the frame.
    #[inline]
    #[must_use]
    pub const fn ribbon_drawn(&self) -> usize {
        self.ribbon.ink_quad_count()
    }

    /// `trail status`'s `ribbon_curtain_ms=`.
    #[inline]
    #[must_use]
    pub fn curtain_left_ms(&self, now: Instant) -> Option<u32> {
        self.ribbon.curtain_left_ms(now)
    }

    /// The ribbon's laid cells, live or retracting.
    #[cfg(test)]
    pub(crate) const fn laid_cells(&self) -> usize {
        self.ribbon.live_cells()
    }

    /// The caret mirror, the cell a `Typed` key lays against.
    #[cfg(test)]
    pub(crate) const fn caret_mirror(&self) -> (u16, u16) {
        self.caret
    }

    /// Lay a dark train into the letter guard's reach below a lettered row (the brown segment's
    /// negative control).
    #[cfg(test)]
    pub(crate) const fn lay_trains_in_guard(&mut self, on: bool) {
        self.meteor.trains_in_guard = on;
    }

    /// The events buffered for the next frame, in the order the host handed them over.
    #[cfg(test)]
    pub(crate) const fn pending_events(&self) -> &[(Event, Instant)] {
        self.events.as_slice()
    }

    /// `trail status`'s `ribbon_hue_bands=` while v2 owns the frame.
    #[inline]
    #[must_use]
    pub const fn ribbon_hue_bands(&self) -> usize {
        let plan = self.ribbon.plan_segments();
        let mut seen = 0u32;
        let mut i = 0;
        while i < plan.len() {
            let seg = &plan[i];
            if seg.cov >= ribbon::STATUS_LIT_COV {
                let b = (meteor::tri(seg.t) * 20.0) as usize;
                let band = if b < 19 { b } else { 19 };
                seen |= 1u32 << band;
            }
            i += 1;
        }
        seen.count_ones() as usize
    }

    /// Seam point 7 (`rainbow_phase`).
    #[inline]
    #[must_use]
    pub const fn phase(&self) -> f32 {
        self.spine.phase()
    }

    /// The spine itself, for the producers and for tests that need the raw metric beside its
    /// display twin.
    #[inline]
    #[must_use]
    pub const fn spine(&self) -> &Spine {
        &self.spine
    }

    /// The one glint bucket, kept on [`Stardust`] because only stardust spends it.
    #[must_use]
    #[cfg(test)]
    pub const fn budget(&self) -> &StarBudget {
        &self.stardust.budget
    }

    /// The glyph probe (§5.4's hard gate), written by the host before seam point 3 from the same
    /// row probes it takes for the ledger.
    #[inline]
    pub const fn probe_mut(&mut self) -> &mut GlyphProbe {
        self.stardust.probe_mut()
    }

    /// Read-only view of the frame's glyph truth.
    #[must_use]
    #[cfg(test)]
    pub const fn probe(&self) -> &GlyphProbe {
        self.stardust.probe()
    }

    /// The caret's paint at `now` (§7.1, [`CaretSeam::paint`]).
    #[inline]
    #[must_use]
    pub fn caret_paint(&self, now: Instant) -> f32 {
        let disp = self.spine.disp();
        if disp >= 1.0 {
            return disp;
        }
        let relight = match self.paint_at {
            Some(at) => {
                let elapsed = now.saturating_duration_since(at);
                if elapsed >= RELIGHT_LIFE_DUR {
                    0.0
                } else {
                    half_life(
                        elapsed.as_secs_f32(),
                        SPRING_SNAP_RESPONSE_S,
                        SPRING_SNAP_RESPONSE_S,
                    )
                }
            }
            None => 0.0,
        };
        disp.max(relight)
    }

    /// Seam point 8.
    #[inline]
    #[must_use]
    pub const fn needs_frame_cadence(&self) -> bool {
        self.engaged && self.brisk
    }

    /// Seam point 9.
    #[inline]
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.engaged {
            return None;
        }
        let mut cad = Cadence::at(now);
        if let Some(d) = self.meteor.next_change_deadline(now) {
            cad.edge(d);
        }
        if let Some(d) = self.ribbon.next_change_deadline(now) {
            cad.edge(d);
        }
        if !matches!(cad.best, Some(b) if b <= now + FRAME_CADENCE)
            && let Some(d) = self.stardust.next_change_deadline(now)
        {
            cad.edge(d);
        }
        if !cad.at_tail_floor()
            && let Some(step) = self.ribbon.settle_step_s(self.spine.disp())
        {
            cad.tail(step);
        }
        let at = cad.take()?;
        Some(at.max(now + ARM_MIN))
    }

    /// The last frame's liveness, which `is_active` follows when engaged (§17.2).
    #[inline]
    #[must_use]
    pub const fn fingerprint(&self) -> u64 {
        self.fp
    }

    /// Seam point 10 (`take_cursor_cat_motion_pulse`).
    #[inline]
    pub const fn take_companion_impulse(&mut self) -> Option<CompanionImpulse> {
        self.companion.take()
    }

    /// The resident pet's receiving end (D13). The host reads it after its own pet tick:
    ///
    /// ```
    /// # use aterm_effects::{cursor_glow::Geom, kitty_pet::{PetBrain, PetFrame}, rainbow_kitty::{Engine, companion}};
    /// # use aterm_time::Instant;
    /// # fn offer_after_tick(v2: &mut Engine, pet: &mut PetBrain, pet_frame: &PetFrame, geom: Geom, now: Instant) {
    /// let offer = v2.pet_offer(geom, companion::PetOnGlass::of(pet_frame, geom));
    /// if let Some(star) = pet.note_v2_offer(&offer) { v2.catch_star(star, now); }
    /// # }
    /// ```
    #[inline]
    #[must_use]
    pub fn pet_offer(&self, geom: Geom, pet: Option<companion::PetOnGlass>) -> companion::PetOffer {
        let perk_at = if let Some(imp) = self.companion
            && let companion::BodyImpulse::Perk { at } =
                companion::impulse_for(companion::Body::Pet, imp)
        {
            Some(at)
        } else {
            None
        };
        let Some(pet) = pet else {
            return companion::PetOffer {
                perk_at,
                ..companion::PetOffer::EMPTY
            };
        };
        let (lo, hi) = pet.span();
        let mote_cell = ((lo + hi) * 0.5).floor().max(0.0) as u32;
        let (mote_t, mote_rgb) = if mote_cell <= u32::from(u16::MAX) {
            match self.ribbon.field_at(pet.row, mote_cell as u16) {
                Some(t) => (Some(t), Some(spectrum_snap(t))),
                None => (None, None),
            }
        } else {
            (None, None)
        };
        let mut catch: Option<companion::StarCatch> = None;
        if pet.contented() {
            let stars = self.stardust.live_stars();
            let mut si = 0;
            while si < stars.len() {
                let s = &stars[si];
                si += 1;
                if !(s.gold
                    && s.class == stardust::StarClass::M1
                    && s.lane.is_sky()
                    && s.finish.is_none()
                    && !matches!(catch, Some(prev) if prev.born > s.born)
                    && s.in_sky_of(pet.row, geom))
                {
                    continue;
                }
                let col = s.grid_col(geom);
                if pet.columns_to(col) <= companion::CATCH_REACH_CELLS {
                    catch = Some(companion::StarCatch {
                        x: s.x,
                        y: s.y,
                        born: s.born,
                        col,
                    });
                }
            }
        }
        companion::PetOffer {
            perk_at,
            catch,
            mote_t,
            mote_rgb,
        }
    }

    /// The cat catches the star.
    #[inline]
    pub fn catch_star(&mut self, star: companion::StarCatch, at: Instant) -> bool {
        self.stardust.catch(star.x, star.y, star.born, at)
    }

    /// The pet's drawn box this frame, window pixels (`None`: off glass): no star is drawn on its
    /// body ([`companion::PetShade`]).
    #[inline]
    pub const fn note_pet_body(&mut self, body: Option<(i32, i32, i32, i32)>) {
        self.stardust.shade.note(body);
    }

    /// This frame's [`companion::KeepOut`], window pixels: no star is drawn
    /// on it and the ribbon's body never feathers up into it; its filled
    /// cells' runs (`filled`) keep the ribbon's rail off them, and the feet
    /// of the rows whose glyphs were named (`feet`, on the rows `soles`)
    /// part the rail it lays in its own row's foot band
    /// ([`companion::KeepOut::foot_share`]).
    #[inline]
    pub fn note_keep_out(
        &mut self,
        (icons, cells): (
            impl IntoIterator<Item = (i32, i32, i32, i32)>,
            impl IntoIterator<Item = (i32, i32, i32, i32)>,
        ),
        (rows, past): (impl IntoIterator<Item = (i32, i32, i32, i32)>, i32),
        filled: impl IntoIterator<Item = (i32, i32, i32, i32)>,
        (feet, soles): (
            impl IntoIterator<Item = (i32, i32, i32, i32)>,
            impl IntoIterator<Item = (i32, i32, i32, i32)>,
        ),
    ) {
        self.stardust.keep_out.set_split(icons, cells);
        self.stardust.keep_out.set_rows_past(rows, past);
        self.stardust.keep_out.set_filled(filled);
        self.stardust.keep_out.set_feet(feet, soles);
        self.ribbon.keep_out.clone_from(&self.stardust.keep_out);
        self.meteor.keep_out.clone_from(&self.stardust.keep_out);
    }

    /// [`Self::note_keep_out`] from slices, the icons' boxes moved by
    /// `icon_offset`.
    #[inline]
    pub(crate) fn note_keep_out_slices(
        &mut self,
        icons: &[PxBox],
        icon_offset: (i32, i32),
        cells: &[PxBox],
        (rows, past): (&[PxBox], i32),
        (filled, letters): (&[PxBox], &[PxBox]),
        (feet, soles): (&[PxBox], &[PxBox]),
    ) {
        self.stardust
            .keep_out
            .set_split_slices(icons, icon_offset, cells);
        self.stardust.keep_out.set_rows_past_slice(rows, past);
        self.stardust.keep_out.set_filled_slice(filled);
        self.stardust.keep_out.set_letters_slice(letters);
        self.stardust.keep_out.set_feet_slices(feet, soles);
        self.ribbon.keep_out.clone_from(&self.stardust.keep_out);
        self.meteor.keep_out.clone_from(&self.stardust.keep_out);
    }

    /// This frame's foot cells' ink for the ribbon's rail ([`companion::FootSpan`]), and
    /// whether the caret is a bar or an underline; after [`Self::note_keep_out_slices`],
    /// which lays the ribbon's keep-outs anew.
    #[inline]
    pub(crate) fn note_foot_spans(
        &mut self,
        spans: impl IntoIterator<Item = companion::FootSpan>,
        thin_caret: bool,
    ) {
        self.ribbon.keep_out.set_foot_spans(spans);
        self.ribbon.set_thin_caret(thin_caret);
    }

    /// Each star the sky painted this frame ([`stardust::SkyMark`]).
    #[inline]
    #[must_use]
    pub const fn sky_marks(&self) -> &[stardust::SkyMark] {
        self.stardust.marks()
    }

    /// The keys of the lights this frame's stars painted ([`stardust::SkyMark::lights`]).
    #[inline]
    #[must_use]
    pub const fn sky_mark_lights(&self) -> &[u64] {
        self.stardust.mark_lights()
    }

    /// The body box the sky last drew no star on ([`companion::PetShade::body`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub const fn pet_shade(&self) -> Option<(i32, i32, i32, i32)> {
        self.stardust.shade.body()
    }

    /// The boxes the sky and the ribbon last kept off ([`companion::KeepOut::boxes`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub const fn keep_out(&self) -> &[(i32, i32, i32, i32)] {
        self.stardust.keep_out.boxes()
    }

    /// Seam point 11 (`drain_sound_cues`).
    #[inline]
    pub fn drain_sound_cues(&mut self) -> std::vec::Drain<'_, SoundCue> {
        self.pending_cues.drain(..)
    }

    /// The curtain.
    #[inline]
    pub fn curtain(&mut self, now: Instant) {
        if !self.engaged {
            return;
        }
        self.ribbon.curtain(now);
        self.clear_non_ribbon_state();
        self.fp = u64::from(!self.ribbon.at_rest());
        self.status = Status {
            cells: self.ribbon.live_cells() as u32,
            fp: self.fp,
            retired: self.retired,
            followed: self.followed,
            follow_missed: self.follow_missed,
            ..Status::EMPTY
        };
        self.brisk = self.ribbon.brisk(now);
    }

    /// Hide the next frame.
    #[inline]
    pub const fn hide_next_frame(&mut self) {
        if self.engaged {
            self.hidden = true;
        }
    }

    /// Whether a hide is armed and not yet taken.
    #[inline]
    #[must_use]
    pub const fn hide_armed(&self) -> bool {
        self.hidden
    }

    /// Seam point 12 (`reset`).
    #[inline]
    pub fn reset(&mut self) {
        self.ribbon.reset();
        self.clear_non_ribbon_state();
        self.fp = 0;
        self.status = Status {
            retired: self.retired,
            followed: self.followed,
            follow_missed: self.follow_missed,
            ..Status::EMPTY
        };
        self.brisk = false;
    }

    #[inline]
    fn clear_non_ribbon_state(&mut self) {
        self.hidden = false;
        self.spine.reset();
        self.stardust.reset();
        self.meteor.reset();
        self.caret_seam = CaretSeam::EMPTY;
        self.caret = (0, 0);
        self.caret_known = false;
        self.caret_seen = None;
        self.echo.clear();
        self.bridged = 0;
        self.erased = (0, 0);
        self.mend = None;
        self.events.clear();
        self.pre_move = None;
        self.soft_wrap = None;
        self.lift = None;
        self.hand_run = None;
        self.hand_lift = None;
        self.withheld_n = 0;
        self.flush_clock = None;
        self.reflow = None;
        self.revive = None;
        self.earned.clear();
        self.party = None;
        self.pending_cues.clear();
        self.companion = None;
        self.paint_at = None;
        self.witness.clear();
        self.retire_scratch.clear();
        self.release_scratch.clear();
        self.restore_scratch.clear();
        self.restored_scratch.clear();
        self.follow_scratch.clear();
        self.shift_scratch.clear();
        self.moved_scratch.clear();
    }

    /// Seam point 12 (`translate_scroll_state`).
    #[inline]
    pub fn translate_scroll(&mut self, rows: u16, cell_h: u16) {
        if !self.engaged || rows == 0 {
            return;
        }
        self.ribbon.translate_scroll(rows);
        let bottom = match self.pane_rows {
            Some((first, n)) => first.saturating_add(n),
            None => self.grid_rows,
        };
        self.witness.translate(rows, bottom);
        self.meteor.translate_scroll(rows, cell_h);
        self.stardust.translate_scroll(rows, cell_h);
        self.caret.0 = self.caret.0.saturating_sub(rows);
        if let Some((row, _)) = &mut self.caret_seen {
            *row = row.saturating_sub(rows);
        }
        self.erased.0 = self.erased.0.saturating_sub(rows);
        self.earned
            .retain_mut(|(cell, _)| match cell.0.checked_sub(rows) {
                Some(row) => {
                    cell.0 = row;
                    true
                }
                None => false,
            });
        self.echo.clear();
        self.soft_wrap = None;
        self.reflow = None;
        self.translate_events(|r| r.checked_sub(rows), |r| r.saturating_sub(rows));
        self.translate_pre_move(|r| r.checked_sub(rows));
    }

    #[inline]
    fn translate_events(&mut self, mark: impl Fn(u16) -> Option<u16>, pos: impl Fn(u16) -> u16) {
        self.events.retain_mut(|(ev, _)| match ev {
            Event::Sweep { row, .. } | Event::Rewrite { row, .. } => match mark(*row) {
                Some(r) => {
                    *row = r;
                    true
                }
                None => false,
            },
            Event::Move { from, to, .. } => {
                from.0 = pos(from.0);
                to.0 = pos(to.0);
                true
            }
            _ => true,
        });
        if let Some((lift, _)) = &mut self.lift {
            match mark(lift.dst.0) {
                Some(dst_row) => {
                    lift.origin.0 = pos(lift.origin.0);
                    lift.landing.0 = pos(lift.landing.0);
                    lift.dst.0 = dst_row;
                }
                None => self.lift = None,
            }
        }
        if let Some(rv) = &mut self.revive {
            match mark(rv.row) {
                Some(row) => rv.row = row,
                None => self.revive = None,
            }
        }
    }

    #[inline]
    fn translate_pre_move(&mut self, mark: impl Fn(u16) -> Option<u16>) {
        if let Some((_, caret)) = &mut self.pre_move
            && let Some((row, _)) = caret
        {
            match mark(*row) {
                Some(r) => *row = r,
                None => *caret = None,
            }
        }
    }

    /// Seam point 12 (`translate_band_state`), the band twin of [`Engine::translate_scroll`].
    #[inline]
    pub fn translate_band(
        &mut self,
        top: u16,
        bottom: u16,
        delta: i16,
        cell_h: u16,
        origin_y: u16,
    ) {
        if !self.engaged || delta == 0 || top > bottom {
            return;
        }
        self.echo.clear();
        self.soft_wrap = None;
        self.reflow = None;
        self.ribbon.translate_band(top, bottom, delta);
        self.witness.translate_band(top, bottom, delta);
        self.meteor
            .translate_band(top, bottom, delta, cell_h, origin_y);
        self.stardust
            .translate_band(top, bottom, delta, cell_h, origin_y);
        self.caret.0 = band_pos(self.caret.0, top, bottom, delta);
        if let Some((row, _)) = &mut self.caret_seen {
            *row = band_pos(*row, top, bottom, delta);
        }
        self.erased.0 = band_pos(self.erased.0, top, bottom, delta);
        self.earned
            .retain_mut(|(cell, _)| match band_row(cell.0, top, bottom, delta) {
                Some(row) => {
                    cell.0 = row;
                    true
                }
                None => false,
            });
        self.translate_events(
            |r| band_row(r, top, bottom, delta),
            |r| band_pos(r, top, bottom, delta),
        );
        self.translate_pre_move(|r| band_row(r, top, bottom, delta));
    }

    /// Seam point 12 (`v2_status`): the frame's `trail status` counters.
    #[inline]
    #[must_use]
    pub const fn status(&self) -> Status {
        self.status
    }
}

#[inline]
const fn swept_space_cell(events: &[(Event, Instant)], index: usize) -> Option<(u16, u16)> {
    if index + 1 >= events.len() {
        return None;
    }
    let Event::Sweep { row, col0, col1 } = events[index].0 else {
        return None;
    };
    let Event::Move {
        from,
        to,
        licence: Licence::Typed,
        ..
    } = events[index + 1].0
    else {
        return None;
    };
    if from.0 != row || from.1 != col0 || to.0 != row || to.1 != col1 || col1 <= col0 {
        return None;
    }
    let mut remaining = col1 - col0;
    let mut space = None;
    let mut i = index;
    while i > 0 {
        i -= 1;
        let Event::Typed { cells, class, .. } = events[i].0 else {
            break;
        };
        let is_space = matches!(class, TypedClass::Space);
        if cells == 0 || (is_space && cells != 1) {
            return None;
        }
        let Some(next_rem) = remaining.checked_sub(cells) else {
            return None;
        };
        remaining = next_rem;
        if space.is_none() && is_space {
            space = Some(col0 + remaining);
        }
    }
    if remaining == 0 {
        match space {
            Some(col) => Some((row, col)),
            None => None,
        }
    } else {
        None
    }
}

#[cfg(test)]
#[path = "mod_swept_space_tests.rs"]
mod swept_space_tests;

#[inline]
const fn sub_floor_hop(from: (u16, u16), to: (u16, u16), licence: Licence) -> bool {
    if !licence.is_keyed_nav() || from.0 != to.0 {
        return false;
    }
    let d = to.1.abs_diff(from.1);
    d >= 1 && d < JUMP_MIN_CELLS
}

#[inline]
const fn liveness(under: &[GlowQuad], out: &[GlowQuad], halos: &[RainHalo]) -> u64 {
    !(under.is_empty() && out.is_empty() && halos.is_empty()) as u64
}

#[cfg(any(test, debug_assertions))]
mod state_words;
#[cfg(any(test, debug_assertions))]
pub(crate) use state_words::caret_seam_words;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
