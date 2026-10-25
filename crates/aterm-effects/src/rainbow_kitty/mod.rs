// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rainbow kitty cursor companion state machine, sprite baking, and ribbon orchestration.
pub mod companion;
pub mod meteor;
pub mod ribbon;
pub mod spine;
pub mod stardust;
pub mod timing;
pub mod witness;

pub use spine::Flow;

use aterm_time::Instant;
use std::time::Duration;

use aterm_render::{BeamVertex, GlowQuad, RainHalo};

use crate::cursor_glow::{Geom, GlowConfig, SoundCue, band_pos, band_row};
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
    pub out: &'a mut Vec<GlowQuad>,
    pub halos: &'a mut Vec<RainHalo>,
    pub beams: &'a mut Vec<BeamVertex>,
    pub cues: &'a mut Vec<SoundCue>,
    pub caret: CaretSeam,
    pub companion: Option<CompanionImpulse>,
    pub fp: u64,
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

impl Ctx<'_> {
    /// Whether a light at `(x, y)` reaching `reach` px stands within a cell of the caret's block on
    /// paper, where stars keep a cell clear of the block on every side; on a dark page the caret's
    /// own light keeps them off, and this is false.
    #[inline]
    #[must_use]
    pub const fn near_paper_caret(&self, (x, y): (i32, i32), reach: i32) -> bool {
        let (row, col) = self.caret;
        if self.cfg.dark_theme || row as usize >= self.geom.rows || col as usize >= self.geom.cols {
            return false;
        }
        let (cw, ch) = (self.geom.cw as i32, self.geom.ch as i32);
        let x0 = self.geom.origin_x as i32 + col as i32 * cw;
        let y0 = self.geom.origin_y as i32 + row as i32 * ch;
        x + reach >= x0 - cw
            && x - reach < x0 + 2 * cw
            && y + reach >= y0 - ch
            && y - reach < y0 + 2 * ch
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
        if at > self.now && self.best.is_none_or(|b| at < b) {
            self.best = Some(at);
        }
    }

    /// A fade whose next visible step is `step_s` away.
    #[inline]
    pub(crate) fn tail(&mut self, step_s: f32) {
        let at = if step_s.is_finite() && step_s > TAIL_FLOOR.as_secs_f32() {
            self.now + Duration::from_secs_f32(step_s)
        } else {
            self.now + TAIL_FLOOR
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
        self.best.is_some_and(|b| b <= self.now + TAIL_FLOOR)
    }

    /// The fold: the next frame when brisk, else the earliest offer.
    #[inline]
    #[must_use]
    pub(crate) fn take(self) -> Option<Instant> {
        if self.brisk {
            Some(self.now + FRAME_CADENCE)
        } else {
            self.best
        }
    }
}

/// Seconds until a `spend` fade (`(1 − u)²`, §2.5) next changes its u8 level.
#[inline]
#[must_use]
pub(crate) fn level_step_spend(peak: f32, u: f32, span_s: f32) -> Option<f32> {
    if !(peak.is_finite() && span_s.is_finite() && u.is_finite()) || peak <= 0.0 || span_s <= 0.0 {
        return None;
    }
    let u = timing::clamp01(u);
    let level = (peak * timing::spend(u)).round();
    if level < 1.0 {
        return None;
    }
    let u_next = 1.0 - ((level - 0.5) / peak).sqrt();
    Some(((u_next - u) * span_s).max(0.0))
}

/// The echo patience, seconds, before a press is forgotten as swallowed.
pub const ECHO_PATIENCE_S: f32 = crate::cursor_glow::IN_FLIGHT_PATIENCE_S;

/// The most cells one observed move may lay from the ledger.
pub const ECHO_LEDGER_DEPTH: usize = crate::cursor_glow::TYPED_STAMP_DEPTH;

/// The ribbon's birth floor, seconds.
pub const SWEEP_BIRTH_FLOOR_S: f32 = crate::cursor_glow::CursorGlow::TYPE_HINT_FRESH;

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
    fn bank(&mut self, at: Instant, cells: u16) {
        if cells == 0 {
            return;
        }
        #[cfg(test)]
        {
            self.tally.banked += u32::from(cells);
        }
        if let Some(free) = self.slots.iter().position(Option::is_none) {
            self.slots[free] = Some((at, cells));
        } else {
            #[cfg(test)]
            {
                self.tally.forfeited += self.slots[0].map_or(0, |(_, c)| u32::from(c));
            }
            self.slots.copy_within(1.., 0);
            self.slots[ECHO_LEDGER_DEPTH - 1] = Some((at, cells));
        }
    }

    #[inline]
    fn expire(&mut self, now: Instant) {
        let stale = |at: Instant| now.saturating_duration_since(at).as_secs_f32() > ECHO_PATIENCE_S;
        let mut any = false;
        for slot in &mut self.slots {
            let Some((at, _)) = *slot else { break };
            if stale(at) {
                #[cfg(test)]
                {
                    self.tally.expired += slot.map_or(0, |(_, c)| u32::from(c));
                }
                *slot = None;
                any = true;
            }
        }
        if any {
            self.repack();
        }
    }

    #[inline]
    const fn repack(&mut self) {
        let mut write = 0;
        let mut read = 0;
        while read < ECHO_LEDGER_DEPTH {
            if let Some(entry) = self.slots[read] {
                if write != read {
                    self.slots[write] = Some(entry);
                    self.slots[read] = None;
                }
                write += 1;
            }
            read += 1;
        }
    }

    #[inline]
    fn cells_where(&self, keep: impl Fn(Instant) -> bool) -> usize {
        self.slots
            .iter()
            .map_while(Option::as_ref)
            .filter(|&&(at, _)| keep(at))
            .map(|&(_, cells)| usize::from(cells))
            .sum()
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
        self.cells_where(|at| at < key)
    }

    #[inline]
    fn older_and_total(&self, key: Instant) -> (usize, usize) {
        let (mut older, mut total) = (0, 0);
        for &(at, cells) in self.slots.iter().map_while(Option::as_ref) {
            let c = usize::from(cells);
            if at < key {
                older += c;
            }
            total += c;
        }
        (older, total)
    }

    #[inline]
    const fn spend(&mut self, mut cells: usize) {
        let mut any = false;
        let mut i = 0;
        while i < ECHO_LEDGER_DEPTH {
            if cells == 0 {
                break;
            }
            let Some((at, have)) = self.slots[i] else {
                break;
            };
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
            self.slots[i] = if left > 0 { Some((at, left)) } else { None };
            any |= left == 0;
            i += 1;
        }
        if any {
            self.repack();
        }
    }

    #[inline]
    const fn clear(&mut self) {
        #[cfg(test)]
        {
            self.tally.forfeited += self.total() as u32;
        }
        self.slots = [None; ECHO_LEDGER_DEPTH];
    }

    #[inline]
    fn forget(&mut self, now: Instant) {
        self.expire(now);
        self.clear();
    }

    #[cfg(test)]
    fn partition(&self, key: Instant, now: Instant) -> (usize, usize, usize) {
        let (mut older, mut younger, mut stale) = (0, 0, 0);
        for &(at, cells) in self.slots.iter().flatten() {
            let cells = usize::from(cells);
            if now.saturating_duration_since(at).as_secs_f32() > ECHO_PATIENCE_S {
                stale += cells;
            } else if at < key {
                older += cells;
            } else {
                younger += cells;
            }
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
    flush_clock: Option<Instant>,
    soft_wrap: Option<(ribbon::SoftWrap, Option<Instant>)>,
    lift: Option<(ribbon::Lift, Option<Instant>)>,
    reflow: Option<(ribbon::SoftWrap, (u16, u16))>,
    revive: Option<ribbon::Revive>,
    hidden: bool,
}

impl Default for Engine {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    /// A disengaged engine with nothing live and a full glint bucket, so the first capital of a
    /// session chimes.
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
        if let Some(park) = self.flush_clock
            && let Some(i) = self
                .events
                .iter()
                .position(|(e, at)| matches!(e, Event::Typed { .. }) && *at > park)
        {
            self.events.insert(i, (ev, now));
            return;
        }
        self.events.push((ev, now));
    }

    /// The clock a held park's flush is judged at ([`Engine::on_event`]).
    #[inline]
    pub const fn set_flush_clock(&mut self, at: Option<Instant>) {
        self.flush_clock = at;
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

    fn on_move(&mut self, from: (u16, u16), to: (u16, u16), licence: Licence, now: Instant) {
        let host_sweep = self.host_sweep_for(from, to);
        #[cfg(test)]
        {
            self.last_host_sweep_at = host_sweep;
        }
        let bridge = self.echo_bridge(from, to, licence, host_sweep, now);
        let floor = now
            .checked_sub(Duration::from_secs_f32(SWEEP_BIRTH_FLOOR_S))
            .unwrap_or(now);
        let birth = host_sweep.map(|at| at.max(floor));
        if let (Some(birth), Some((Event::Sweep { .. }, at))) = (birth, self.events.last_mut()) {
            *at = birth;
        }
        if let Some(col0) = bridge
            && (col0 < from.1 || host_sweep.is_none())
        {
            self.lay_bridge(from, to, col0, birth.unwrap_or(now), host_sweep.is_some());
        }
        self.record_pre_move(from, to, licence);
        self.caret = to;
        self.caret_known = true;
    }

    #[inline]
    const fn host_sweep_for(&self, from: (u16, u16), to: (u16, u16)) -> Option<Instant> {
        match self.events.as_slice().last() {
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
        let soft = self.soft_wrap.take().and_then(|(sw, key_at)| {
            let at_move = self.events.iter().position(|(e, _)| {
                matches!(*e, Event::Move { from, to, .. } if from == sw.origin && to == sw.landing)
            })?;
            let at_key = key_at.and_then(|key| {
                self.events[..at_move]
                    .iter()
                    .position(|&(e, at)| matches!(e, Event::Typed { .. }) && at == key)
            });
            Some((sw, at_move, at_key))
        });
        let lift = self.lift.take().and_then(|(lift, key_at)| {
            let at_move = self.events.iter().position(|(e, _)| {
                matches!(*e, Event::Move { from, to, .. } if from == lift.origin && to == lift.landing)
            })?;
            let at_key = key_at.and_then(|key| {
                self.events[..at_move]
                    .iter()
                    .rposition(|&(e, at)| matches!(e, Event::Typed { .. }) && at == key)
            });
            Some((lift, at_move, at_key))
        });
        let reflow = self.reflow.take().and_then(|(sw, to)| {
            let at_move = self.events.iter().position(|(e, _)| {
                matches!(*e, Event::Move { from, to: t, .. } if from == sw.landing && t == to)
            })?;
            Some((sw, at_move))
        });
        let revive = self.revive.take().and_then(|rv| {
            let at_sweep = self.events.iter().position(|(e, _)| {
                matches!(*e, Event::Sweep { row, col0, col1 }
                    if row == rv.row && col0 == rv.col0 && col1 == rv.col1)
            })?;
            Some((rv, at_sweep))
        });
        if let Some((rv, at_sweep)) = revive {
            let (ev, at) = self.events[at_sweep];
            self.ribbon.revive_sweep(&ev, at, ctx, rv);
        }
        let mut held_cells: u16 = 0;
        let mut soft_held: u16 = 0;
        for (index, &(ev, at)) in self.events.iter().enumerate() {
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
                    && let Some((row, col)) = swept_space_cell(&self.events, index)
                {
                    self.ribbon.observe_space_cell(row, col);
                }
                continue;
            };
            if index >= tail_start {
                let cell = self
                    .caret_known
                    .then(|| (self.caret.0, self.caret.1.saturating_add(held_cells)));
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
        self.events
            .as_slice()
            .split_at(self.unechoed_tail_start())
            .1
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
            n = n.saturating_add(
                u32::try_from(self.ribbon.retire_cells(&self.retire_scratch, now))
                    .unwrap_or(u32::MAX),
            );
        }
        if !self.release_scratch.is_empty() {
            n = n.saturating_add(
                u32::try_from(self.ribbon.release_cells(&self.release_scratch, now))
                    .unwrap_or(u32::MAX),
            );
        }
        n = n.saturating_sub(u32::try_from(recounted).unwrap_or(u32::MAX));
        if n > 0 {
            self.retired += u64::from(n);
            self.status.retired = self.retired;
        }
        n
    }

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

    /// The laid ribbon, read-only.
    #[inline]
    #[must_use]
    pub const fn ribbon(&self) -> &Ribbon {
        &self.ribbon
    }

    /// The rows the witness wants, written into `out`.
    #[inline]
    pub fn ribbon_rows_for(&self, arming: Option<u16>, out: &mut [u16]) -> usize {
        self.ribbon_rows_at_caret(arming, None, out)
    }

    /// [`Engine::ribbon_rows_for`] with this frame's caret move `(from, to)`.
    pub fn ribbon_rows_at_caret(
        &self,
        arming: Option<u16>,
        caret_move: Option<(u16, u16)>,
        out: &mut [u16],
    ) -> usize {
        if !self.engaged {
            return 0;
        }
        let add = |out: &mut [u16], n: &mut usize, row: u16| {
            if (self.grid_rows == 0 || row < self.grid_rows)
                && *n < out.len()
                && !out[..*n].contains(&row)
            {
                out[*n] = row;
                *n += 1;
            }
        };
        let budget = out.len().min(witness::WITNESS_ROWS);
        let mut n = self.band_rows(&mut out[..budget], caret_move, add);
        if let Some(row) = arming {
            for r in [Some(row), row.checked_sub(1), row.checked_add(1)]
                .into_iter()
                .flatten()
            {
                add(out, &mut n, r);
            }
        }
        n
    }

    fn band_rows(
        &self,
        out: &mut [u16],
        caret_move: Option<(u16, u16)>,
        add: impl Fn(&mut [u16], &mut usize, u16),
    ) -> usize {
        let cohorts = self.ribbon.cohorts();
        if cohorts.is_empty() || out.is_empty() {
            return 0;
        }
        let mut n = 0usize;
        let focus = cohorts
            .iter()
            .max_by_key(|coh| (self.caret_known && coh.row == self.caret.0, coh.alive_at))
            .map(|coh| coh.row);
        let carry = self.witness.scroll_carry();
        let back = |row: u16| row.checked_add(carry).filter(|_| carry > 0);
        let lead = caret_move
            .map(|(from, to)| i32::from(to) - i32::from(from))
            .filter(|&d| d.unsigned_abs() > 2 && d != i32::from(carry));
        let ahead = |row: u16| lead.and_then(|d| u16::try_from(i32::from(row) + d).ok());
        if let Some(focus) = focus {
            for row in [
                Some(focus),
                focus.checked_sub(1),
                focus.checked_add(1),
                back(focus),
                ahead(focus),
            ]
            .into_iter()
            .flatten()
            {
                add(out, &mut n, row);
            }
            if n == out.len() {
                return n;
            }
        }
        for coh in cohorts {
            add(out, &mut n, coh.row);
            if n == out.len() {
                return n;
            }
        }
        if carry > 0 {
            for coh in cohorts {
                let Some(row) = back(coh.row) else {
                    break;
                };
                add(out, &mut n, row);
                if n == out.len() {
                    return n;
                }
            }
        }
        if lead.is_some() {
            for coh in cohorts {
                let Some(row) = ahead(coh.row) else {
                    continue;
                };
                add(out, &mut n, row);
                if n == out.len() {
                    return n;
                }
            }
        }
        for coh in cohorts {
            for nb in [coh.row.checked_sub(1), coh.row.checked_add(1)] {
                if n == out.len() {
                    return n;
                }
                let Some(nb) = nb else {
                    continue;
                };
                add(out, &mut n, nb);
            }
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
        for &r in asked {
            let readable = r < self.grid_rows
                && self
                    .pane_rows
                    .is_none_or(|(first, n)| r >= first && r - first < n);
            if readable
                && !rows.iter().any(|s| s.row == r)
                && !self.withheld[..self.withheld_n].contains(&r)
                && self.withheld_n < self.withheld.len()
            {
                self.withheld[self.withheld_n] = r;
                self.withheld_n += 1;
            }
        }
        self.note_rewrap_starts(rows, park);
        let caret = self.caret_known.then_some(self.caret);
        let same_caret_row = self.caret_seen.is_none_or(|(r, _)| r == self.caret.0);
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
        for k in 0..self.shift_scratch.len() {
            let run = self.shift_scratch[k];
            self.moved_scratch.clear();
            let moved = self.ribbon.shift_run(run, &mut self.moved_scratch);
            if moved == 0 {
                continue;
            }
            self.witness.shift_cells(&mut self.moved_scratch, run.dc);
            n = n.saturating_add(u32::try_from(moved).unwrap_or(u32::MAX));
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
        let hand = self
            .hand_rides(rows, hand_withheld, lifted)
            .or_else(|| self.hand_returns(rows));
        self.witness.end_scroll_carry();
        if let Some(h) = hand
            && !self
                .follow_scratch
                .iter()
                .any(|r| r.row == h.row && r.cohort == h.cohort && r.dr == h.dr)
        {
            self.follow_scratch
                .retain(|r| r.row != h.row || r.cohort != h.cohort);
            self.follow_scratch.push(h);
        }
        if self.follow_scratch.is_empty() {
            if n > 0 {
                self.followed += u64::from(n);
                self.status.followed = self.followed;
            }
            return (n, false);
        }
        let park_owner = park.and_then(|(from, to)| {
            if from.0 != to.0 || !(1..=2).contains(&from.1.saturating_sub(to.1)) {
                return None;
            }
            let last_col = from.1.checked_sub(1)?;
            self.ribbon
                .cells()
                .iter()
                .find(|cell| !cell.leaving() && cell.row == from.0 && cell.col == last_col)
                .map(|cell| (from.0, cell.cohort))
        });
        let hand_row = hand.map_or(self.caret.0, |h| h.row);
        for k in 0..self.follow_scratch.len() {
            let run = self.follow_scratch[k];
            let Some(target) = run.row.checked_add_signed(run.dr) else {
                continue;
            };
            let away = !self.caret_known || target != self.caret.0;
            let park_owns_run =
                park_owner.is_some_and(|(row, cohort)| row == run.row && cohort == run.cohort);
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
            n = n.saturating_add(u32::try_from(moved).unwrap_or(u32::MAX));
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
            let waits = [-2i16, -1, 1, 2]
                .into_iter()
                .filter_map(|dr| row.checked_add_signed(dr))
                .find_map(|to| self.hand_line_follow(rows, Some(to)));
            self.witness
                .await_caret(waits.map(|run| (run.row, run.cohort)));
            return None;
        };
        let to = run.row.checked_add_signed(run.dr)?;
        self.lift_hand_anchors(run.row, to);
        self.hand_lift = Some((run.row, to));
        Some(run)
    }

    fn hand_returns(&mut self, rows: &[RowSample<'_>]) -> Option<witness::FollowRun> {
        if !self.caret_known {
            return None;
        }
        let seen_row = self.caret_seen.map(|(r, _)| r);
        let run = self
            .follow_scratch
            .iter()
            .copied()
            .find(|r| {
                r.row == self.caret.0
                    && (r.dr.unsigned_abs() > 2
                        || (seen_row.is_some() && r.row.checked_add_signed(r.dr) == seen_row))
            })
            .or_else(|| {
                let sr = seen_row.filter(|&sr| sr != self.caret.0)?;
                self.hand_line_follow(rows, Some(sr))
                    .filter(|r| r.row == self.caret.0 && r.row.checked_add_signed(r.dr) == Some(sr))
            })?;
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
    pub(crate) fn lift_hand(&mut self, run: witness::FollowRun) {
        let Some(to) = run.row.checked_add_signed(run.dr) else {
            return;
        };
        self.lift_hand_anchors(run.row, to);
        self.hand_run = Some(run);
    }

    /// The hand's anchors lifted with a line that has no typed run to carry: [`Engine::lift_hand`]
    /// with no run.
    #[inline]
    pub(crate) fn lift_caret(&mut self, row: u16, to: u16) {
        self.lift_hand_anchors(row, to);
    }

    fn lift_hand_anchors(&mut self, row: u16, to: u16) {
        let map = |r: u16| if r == row { to } else { r };
        if self.caret_known {
            self.caret.0 = map(self.caret.0);
        }
        self.caret_seen = self.caret_seen.map(|(r, c)| (map(r), c));
        self.erased.0 = map(self.erased.0);
        for (cell, _) in &mut self.earned {
            cell.0 = map(cell.0);
        }
        self.ribbon.lift_hand(row, to);
        self.translate_events(|r| Some(map(r)), map);
        self.translate_pre_move(|r| Some(map(r)));
        if let Some((sw, _)) = &mut self.soft_wrap {
            sw.origin.0 = map(sw.origin.0);
            sw.landing.0 = map(sw.landing.0);
        }
        if let Some((sw, reflow_to)) = &mut self.reflow {
            sw.origin.0 = map(sw.origin.0);
            sw.landing.0 = map(sw.landing.0);
            reflow_to.0 = map(reflow_to.0);
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

    fn note_rewrap_starts(
        &mut self,
        rows: &[RowSample<'_>],
        park: Option<((u16, u16), (u16, u16))>,
    ) {
        if self.events.is_empty() && park.is_none() {
            return;
        }
        let pane_col0 = self.pane.map_or(0, |(c0, _)| c0);
        let found = self
            .events
            .iter()
            .rev()
            .find_map(|(ev, _)| match *ev {
                Event::Move {
                    from, to, licence, ..
                } if licence == Licence::Typed && from.0 == to.0 && to.1 < from.1 => {
                    Some((from, to))
                }
                _ => None,
            })
            .or_else(|| park.filter(|&(from, to)| from.0 == to.0 && to.1 < from.1));
        let Some((from, to)) = found else {
            return;
        };
        let Some(sample) = rows.iter().find(|r| r.row == to.0) else {
            return;
        };
        let tail_vacated = (to.1..from.1).all(|c| {
            sample
                .cols
                .get(usize::from(c))
                .is_none_or(|&ch| ch == ' ' || ch == '\0')
        });
        if !tail_vacated {
            return;
        }
        let start = (pane_col0..to.1).find(|&c| {
            sample
                .cols
                .get(usize::from(c))
                .is_some_and(|&ch| ch != ' ' && ch != '\0')
        });
        if let Some(start) = start {
            self.ribbon.note_rewrap_start(from, to, start, None, None);
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
    pub fn earn_hero(&mut self, now: Instant, row: u16, col: u16) {
        if !self.engaged {
            return;
        }
        let impulse = CompanionImpulse::Delight;
        self.offer(impulse);
        for _ in 0..companion::heroes_earned(impulse) {
            self.earned.push(((row, col), now));
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
    pub fn tick(&mut self, now: Instant, geom: Geom, cfg: &Config, out: &mut Frame<'_>) {
        self.tick_with_restored_rows(now, geom, cfg, out, &[]);
    }

    /// The host's sampled rows can lift a prior partial release on the very frame that shows its
    /// text again, after queued edits have replayed.
    pub(crate) fn tick_with_restored_rows(
        &mut self,
        now: Instant,
        geom: Geom,
        cfg: &Config,
        out: &mut Frame<'_>,
        rows: &[RowSample<'_>],
    ) {
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
        if self.caret_seam.flare_at.is_some_and(|t| {
            now.saturating_duration_since(t).as_secs_f32() >= SPRING_SNAP_RESPONSE_S
        }) {
            self.caret_seam.flare_at = None;
        }
        if self
            .paint_at
            .is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() >= RELIGHT_LIFE_S)
        {
            self.paint_at = None;
        }
        let cfg_live = Config {
            reduced_motion: cfg.reduced_motion || self.reduced_motion,
            ..*cfg
        };
        self.grid_rows = u16::try_from(geom.rows).unwrap_or(u16::MAX);
        let mut keys = 0usize;
        let mut saw_erase = false;
        for (ev, _) in &self.events {
            match ev {
                Event::Erase => saw_erase = true,
                Event::Typed { .. } => keys += 1,
                _ => {}
            }
        }
        if saw_erase {
            self.erased = self.caret;
        }
        let mend = self.mend.take().map(|m| Mend {
            at: m.at,
            disp: m.disp,
            row: self.erased.0,
            col: self.erased.1,
            deletes: m.count,
        });
        let price = self.spine.birth_disp();
        self.spine
            .note_keys(now, u32::try_from(keys).unwrap_or(u32::MAX), price);
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
            for i in 0..self.events.len() {
                let (ev, at) = self.events[i];
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

            for &(ev, at) in &self.events {
                self.stardust.on_event(&ev, at, &ctx, &self.ribbon);
            }
            self.events.clear();
        }
        if !self.earned.is_empty() {
            for &(cell, at) in &self.earned {
                self.stardust.earn_hero(at, cell, &ctx, &self.ribbon);
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
        let hidden = std::mem::take(&mut self.hidden);
        self.meteor.emit(&ctx, out);
        self.meteor.sow_into(&mut self.stardust, &ctx);
        self.ribbon.emit(&ctx, out);
        self.stardust.emit(&ctx, out);
        if hidden {
            out.under.truncate(u0);
            out.out.truncate(o0);
            out.halos.truncate(h0);
            self.stardust.clear_marks();
        }
        self.caret_seam.field_t = ctx.caret_t;
        self.caret_seam.paint = self.caret_paint(now);
        out.caret = self.caret_seam;
        out.cues.append(&mut self.pending_cues);
        out.companion = self.companion;

        self.fp = liveness(&out.under[u0..], &out.out[o0..], &out.halos[h0..]);
        out.fp = self.fp;
        self.status = Status {
            quads: ((out.under.len() - u0) + (out.out.len() - o0)) as u32,
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
    pub fn ribbon_hue_bands(&self) -> usize {
        let mut seen = 0u32;
        for seg in self.ribbon.lit_segments() {
            let band = ((meteor::tri(seg.t) * 20.0) as usize).min(19);
            seen |= 1u32 << band;
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
        let relight = self.paint_at.map_or(0.0, |at| {
            let age = now.saturating_duration_since(at).as_secs_f32();
            if age >= RELIGHT_LIFE_S {
                0.0
            } else {
                half_life(age, SPRING_SNAP_RESPONSE_S, SPRING_SNAP_RESPONSE_S)
            }
        });
        self.spine.disp().max(relight)
    }

    /// Seam point 8.
    #[inline]
    #[must_use]
    pub const fn needs_frame_cadence(&self) -> bool {
        self.engaged && self.brisk
    }

    /// Seam point 9.
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
        if !cad.best.is_some_and(|b| b <= now + FRAME_CADENCE)
            && let Some(d) = self.stardust.next_change_deadline(now)
        {
            cad.edge(d);
        }
        if !cad.at_tail_floor()
            && let Some(step) = self.ribbon.settle_step_s(self.spine.disp())
        {
            cad.tail(step);
        }
        cad.take().map(|at| at.max(now + ARM_MIN))
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
    #[must_use]
    pub fn pet_offer(&self, geom: Geom, pet: Option<companion::PetOnGlass>) -> companion::PetOffer {
        let perk_at = self.companion.and_then(|imp| {
            match companion::impulse_for(companion::Body::Pet, imp) {
                companion::BodyImpulse::Perk { at } => Some(at),
                _ => None,
            }
        });
        let Some(pet) = pet else {
            return companion::PetOffer {
                perk_at,
                ..companion::PetOffer::EMPTY
            };
        };
        let (lo, hi) = pet.span();
        let mote_cell = ((lo + hi) * 0.5).floor().max(0.0) as u32;
        let mote_t = u16::try_from(mote_cell)
            .ok()
            .and_then(|col| self.ribbon.field_at(pet.row, col));
        let catch = pet
            .contented()
            .then(|| {
                self.stardust
                    .live_iter()
                    .filter_map(|s| {
                        if !(s.gold
                            && s.class == stardust::StarClass::M1
                            && s.lane.is_sky()
                            && s.finish.is_none()
                            && s.in_sky_of(pet.row, geom))
                        {
                            return None;
                        }
                        let col = s.grid_col(geom);
                        (pet.columns_to(col) <= companion::CATCH_REACH_CELLS).then_some(
                            companion::StarCatch {
                                x: s.x,
                                y: s.y,
                                born: s.born,
                                col,
                            },
                        )
                    })
                    .max_by_key(|c| c.born)
            })
            .flatten();
        companion::PetOffer {
            perk_at,
            catch,
            mote_t,
            mote_rgb: mote_t.map(spectrum_snap),
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
    /// on it and the ribbon's body never feathers up into it.
    #[inline]
    pub fn note_keep_out(
        &mut self,
        (icons, cells): (
            impl IntoIterator<Item = (i32, i32, i32, i32)>,
            impl IntoIterator<Item = (i32, i32, i32, i32)>,
        ),
        rows: impl IntoIterator<Item = (i32, i32, i32, i32)>,
    ) {
        self.stardust.keep_out.set_split(icons, cells);
        self.stardust.keep_out.set_rows(rows);
        self.ribbon.keep_out.clone_from(&self.stardust.keep_out);
        self.meteor.keep_out.clone_from(&self.stardust.keep_out);
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
    #[must_use]
    pub const fn pet_shade(&self) -> Option<(i32, i32, i32, i32)> {
        self.stardust.shade.body()
    }

    /// The boxes the sky and the ribbon last kept off ([`companion::KeepOut::boxes`]).
    #[cfg(any(test, debug_assertions))]
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
    pub fn translate_scroll(&mut self, rows: u16, cell_h: u16) {
        if !self.engaged || rows == 0 {
            return;
        }
        self.ribbon.translate_scroll(rows);
        let bottom = self
            .pane_rows
            .map_or(self.grid_rows, |(first, n)| first.saturating_add(n));
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
        self.lift = self.lift.and_then(|(lift, key_at)| {
            mark(lift.dst.0).map(|dst_row| {
                (
                    ribbon::Lift {
                        origin: (pos(lift.origin.0), lift.origin.1),
                        landing: (pos(lift.landing.0), lift.landing.1),
                        dst: (dst_row, lift.dst.1),
                    },
                    key_at,
                )
            })
        });
        self.revive = self
            .revive
            .and_then(|rv| mark(rv.row).map(|row| ribbon::Revive { row, ..rv }));
    }

    #[inline]
    fn translate_pre_move(&mut self, mark: impl Fn(u16) -> Option<u16>) {
        if let Some((_, caret)) = &mut self.pre_move
            && let Some((row, col)) = *caret
        {
            *caret = mark(row).map(|row| (row, col));
        }
    }

    /// Seam point 12 (`translate_band_state`), the band twin of [`Engine::translate_scroll`].
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
mod swept_space_tests {
    use super::*;

    fn key(cells: u16, class: TypedClass) -> Event {
        Event::Typed {
            cells,
            shifted: false,
            class,
        }
    }

    fn batch(keys: &[Event], width: u16, licence: Licence) -> Vec<(Event, Instant)> {
        let now = Instant::now();
        let mut events: Vec<_> = keys.iter().map(|&event| (event, now)).collect();
        events.push((
            Event::Sweep {
                row: 4,
                col0: 40,
                col1: 40 + width,
            },
            now,
        ));
        events.push((
            Event::Move {
                from: (4, 40),
                to: (4, 40 + width),
                licence,
                dir: Dir::Right,
            },
            now,
        ));
        events
    }

    #[test]
    fn a_swept_space_uses_the_last_space_and_counts_wide_glyph_cells() {
        let keys = [
            key(2, TypedClass::Glyph),
            key(1, TypedClass::Space),
            key(1, TypedClass::Glyph),
            key(1, TypedClass::Space),
            key(2, TypedClass::Glyph),
        ];
        let events = batch(&keys, 7, Licence::Typed);
        assert_eq!(swept_space_cell(&events, keys.len()), Some((4, 44)));
    }

    #[test]
    fn a_swept_space_requires_the_complete_key_run_and_typed_move() {
        let keys = [key(1, TypedClass::Space), key(1, TypedClass::Glyph)];
        for licence in [
            Licence::Insert,
            Licence::Nav,
            Licence::Return,
            Licence::Pty,
            Licence::Synthetic,
        ] {
            assert_eq!(swept_space_cell(&batch(&keys, 2, licence), 2), None);
        }
        for width in [0, 1, 3] {
            assert_eq!(
                swept_space_cell(&batch(&keys, width, Licence::Typed), 2),
                None
            );
        }
        for malformed in [
            vec![key(0, TypedClass::Space), key(2, TypedClass::Glyph)],
            vec![key(2, TypedClass::Space)],
            vec![keys[0], Event::Erase, keys[1]],
        ] {
            assert_eq!(
                swept_space_cell(&batch(&malformed, 2, Licence::Typed), malformed.len()),
                None
            );
        }
        let mut wrong_move = batch(&keys, 2, Licence::Typed);
        wrong_move[3].0 = Event::Move {
            from: (4, 39),
            to: (4, 42),
            licence: Licence::Typed,
            dir: Dir::Right,
        };
        assert_eq!(swept_space_cell(&wrong_move, 2), None);
        wrong_move.pop();
        assert_eq!(swept_space_cell(&wrong_move, 2), None);
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn geom() -> Geom {
        Geom {
            cw: 9,
            ch: 18,
            rows: 40,
            cols: 120,
            origin_x: 0,
            origin_y: 0,
            win_w: 1080,
            win_h: 720,
            head: 0,
        }
    }

    fn config() -> Config {
        Config {
            dark_theme: true,
            intensity: 1.0,
            duration: Duration::from_millis(900),
            ribbon_tall: true,
            ribbon_flat: false,
            theme_fg: 0x00E8_E8F0,
            theme_bg: 0x0016_161C,
            reduced_motion: false,
        }
    }

    #[derive(Default)]
    struct Scratch {
        under: Vec<GlowQuad>,
        out: Vec<GlowQuad>,
        halos: Vec<RainHalo>,
        beams: Vec<BeamVertex>,
        cues: Vec<SoundCue>,
    }

    impl Scratch {
        fn frame(&mut self) -> Frame<'_> {
            Frame {
                under: &mut self.under,
                out: &mut self.out,
                halos: &mut self.halos,
                beams: &mut self.beams,
                cues: &mut self.cues,
                caret: CaretSeam::default(),
                companion: None,
                fp: 0,
            }
        }
    }

    #[test]
    fn a_disengaged_engine_is_inert_at_every_seam_point() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = Engine::new();
        assert!(!eng.engaged());
        let mut sc = Scratch::default();

        eng.on_event(
            Event::Typed {
                cells: 1,
                shifted: false,
                class: TypedClass::Glyph,
            },
            t0,
        );
        eng.on_event(
            Event::Move {
                from: (3, 0),
                to: (3, 80),
                licence: Licence::Nav,
                dir: Dir::Right,
            },
            t0,
        );
        eng.celebrate(t0, 1.0);
        eng.earn_hero(t0, 3, 4);
        eng.translate_scroll(1, 18);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert_eq!(fr.fp, 0);
        assert!(sc.under.is_empty() && sc.out.is_empty() && sc.halos.is_empty());
        assert!(sc.cues.is_empty(), "a disengaged engine mints no cue");
        assert!(!eng.needs_frame_cadence());
        assert!(eng.next_change_deadline(t0).is_none());
        assert_eq!(
            eng.momentum_display(),
            0.0,
            "a disengaged spine never builds, not even under a sing-along"
        );
        assert_eq!(eng.status().stars, 0, "a disengaged engine earns no hero");
        assert!(eng.take_companion_impulse().is_none());
        assert!(eng.drain_sound_cues().next().is_none());
    }

    fn engaged() -> Engine {
        let mut eng = Engine::new();
        eng.set_engaged(true);
        eng
    }

    fn blank_row(eng: &mut Engine, row: u16) {
        eng.probe_mut().probe_row(i32::from(row), &[false; 120]);
    }

    fn typed(n: u16) -> Event {
        Event::Typed {
            cells: n,
            shifted: false,
            class: TypedClass::Glyph,
        }
    }

    fn mv(from: (u16, u16), to: (u16, u16), licence: Licence) -> Event {
        Event::Move {
            from,
            to,
            licence,
            dir: Dir::of(
                i32::from(to.1) - i32::from(from.1),
                i32::from(to.0) - i32::from(from.0),
            ),
        }
    }

    fn tick_at(eng: &mut Engine, t: Instant) {
        let mut sc = Scratch::default();
        let mut fr = sc.frame();
        eng.tick(t, geom(), &config(), &mut fr);
    }

    fn type_key_at(eng: &mut Engine, t: Instant, col: u16) -> Instant {
        eng.on_event(typed(1), t);
        tick_at(eng, t);
        let echo = t + Duration::from_millis(8);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: col,
                col1: col + 1,
            },
            t,
        );
        eng.on_event(mv((3, col), (3, col + 1), Licence::Typed), echo);
        tick_at(eng, echo);
        echo
    }

    fn three_typed_cells(eng: &mut Engine, t0: Instant) -> Instant {
        blank_row(eng, 2);
        eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
        tick_at(eng, t0);
        let mut t = t0;
        for col in 2..5u16 {
            t += Duration::from_millis(100);
            t = type_key_at(eng, t, col);
        }
        t
    }

    /// Typing a row and taking a Return on paper, no painted star comes within a cell of the
    /// caret; on dark (the control) the strikes lay stars there.
    #[test]
    fn paper_stars_stand_a_cell_clear_of_the_caret() {
        let ms = Duration::from_millis;
        let g = geom();
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        let near = |dark: bool| -> usize {
            let mut cfg = config();
            cfg.dark_theme = dark;
            if !dark {
                cfg.theme_fg = 0x0024_292F;
                cfg.theme_bg = 0x00FF_FFFF;
            }
            let mut eng = engaged();
            for row in 1..8u16 {
                blank_row(&mut eng, row);
            }
            let mut sc = Scratch::default();
            let mut caret = (3u16, 2u16);
            let mut found = 0;
            let mut look = |eng: &mut Engine, t: Instant, caret: (u16, u16), sc: &mut Scratch| {
                let mut fr = sc.frame();
                eng.tick(t, g, &cfg, &mut fr);
                let x0 = i32::from(g.origin_x) + i32::from(caret.1) * cw;
                let y0 = i32::from(g.origin_y) + i32::from(caret.0) * ch;
                found += eng
                    .sky_marks()
                    .iter()
                    .filter(|m| {
                        let (mx0, mx1, my0, my1) = m.rect;
                        mx0 < x0 + 2 * cw && mx1 > x0 - cw && my0 < y0 + 2 * ch && my1 > y0 - ch
                    })
                    .count();
            };
            let t0 = Instant::now();
            eng.on_event(mv((3, 0), caret, Licence::Typed), t0);
            look(&mut eng, t0, caret, &mut sc);
            let mut t = t0;
            for col in 2..24u16 {
                t += ms(70);
                eng.on_event(typed(1), t);
                look(&mut eng, t, caret, &mut sc);
                let echo = t + ms(8);
                eng.on_event(
                    Event::Sweep {
                        row: 3,
                        col0: col,
                        col1: col + 1,
                    },
                    t,
                );
                eng.on_event(mv(caret, (3, col + 1), Licence::Typed), echo);
                caret = (3, col + 1);
                look(&mut eng, echo, caret, &mut sc);
                t = echo;
            }
            t += ms(120);
            eng.on_event(mv(caret, (4, 2), Licence::Return), t);
            caret = (4, 2);
            for _ in 0..30 {
                look(&mut eng, t, caret, &mut sc);
                t += ms(16);
            }
            found
        };
        let paper = near(false);
        assert_eq!(paper, 0, "{paper} stars within a cell of the paper caret");
        assert!(
            near(true) > 0,
            "control: on dark no star stood by the caret"
        );
    }

    fn echo_band_fence_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            EchoBandFence {
                const Buggy = 0;
                var pending = 1;
                var translated = 0;
                var done = 0;
                action Noop when (done == 0) { done = 1; }
                action Translate when (done == 0) {
                    done = 1;
                    translated = 1;
                    pending = if Buggy == 0 { 0 } else { pending };
                }
                invariant NoCreditAcrossBand: translated == 0 || pending == 0;
                invariant Bounds: pending <= 1 && translated <= 1 && done <= 1;
            }
        }
    }

    #[test]
    fn real_band_transforms_retire_echo_credits_without_resetting_earned_light() {
        let model = echo_band_fence_model();
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut negative_controls = 0;
        for (top, bottom, delta) in [
            (2, 10, 1),  // Codex composer moves down.
            (2, 10, -1), // Transcript moves up.
            (8, 10, 1),  // Composer is outside the changed band.
            (3, 3, 1),   // Old marks leave; the caret position saturates.
            (2, 10, 0),  // No movement: no fence.
            (10, 2, 1),  // Invalid band: no fence.
        ] {
            let valid = delta != 0 && top <= bottom;
            for restore_stale_credit in [false, true] {
                if restore_stale_credit && !valid {
                    continue;
                }
                let mut eng = engaged();
                let at = three_typed_cells(&mut eng, t0) + ms(100);
                assert_eq!(eng.echo.total(), 0, "the earlier keys were acknowledged");
                eng.on_event(typed(1), at);
                tick_at(&mut eng, at);
                assert_eq!(
                    eng.echo.total(),
                    1,
                    "one press is still waiting for its echo"
                );
                let stale = eng.echo;
                let momentum = eng.spine.momentum(at);
                let display = eng.momentum_display();
                assert!(momentum > 0.0 && display > 0.0, "earned light must be live");
                let field = eng.field_at(3, 4).expect("the acknowledged cohort is lit");

                eng.translate_band(top, bottom, delta, geom().ch as u16, 0);
                let action = if valid { "Translate" } else { "Noop" };
                let post = std::collections::BTreeMap::from([
                    ("pending", i64::from(eng.echo.total() > 0)),
                    ("translated", i64::from(valid)),
                    ("done", 1),
                ]);
                assert!(
                    model
                        .successors(action, &model.init_state())
                        .contains(&post)
                );
                assert!(model.check_invariant("NoCreditAcrossBand", &post));
                assert_eq!(
                    eng.spine.momentum(at),
                    momentum,
                    "scroll is not a key reset"
                );
                assert_eq!(
                    eng.momentum_display(),
                    display,
                    "the transform advances no clock"
                );
                let row = if valid {
                    band_pos(3, top, bottom, delta)
                } else {
                    3
                };
                assert_eq!(eng.caret, (row, 5));
                let mark = if valid {
                    band_row(3, top, bottom, delta)
                } else {
                    Some(3)
                };
                if let Some(mark_row) = mark {
                    assert_eq!(
                        eng.field_at(mark_row, 4),
                        Some(field),
                        "earned field moves with text"
                    );
                } else {
                    assert_eq!(
                        eng.field_at(3, 4),
                        None,
                        "departed light must not clamp to the edge"
                    );
                }
                if !valid {
                    continue;
                }
                if restore_stale_credit {
                    eng.echo = stale;
                    let mut omitted = post.clone();
                    omitted.insert("pending", i64::from(eng.echo.total() > 0));
                    assert!(
                        !model
                            .successors(action, &model.init_state())
                            .contains(&omitted)
                    );
                    assert!(!model.check_invariant("NoCreditAcrossBand", &omitted));
                    negative_controls += 1;
                }

                let press = at + ms(300);
                eng.on_event(typed(1), press);
                tick_at(&mut eng, press);
                eng.on_event(
                    Event::Sweep {
                        row,
                        col0: 6,
                        col1: 7,
                    },
                    press,
                );
                eng.on_event(mv((row, 6), (row, 7), Licence::Typed), press + ms(8));
                tick_at(&mut eng, press + ms(8));
                assert_eq!(
                    eng.field_at(row, 5).is_some(),
                    restore_stale_credit,
                    "only the missing-fence control can paint the program's gap"
                );
                assert!(
                    eng.field_at(row, 6).is_some(),
                    "the new licensed key still paints"
                );
                assert_eq!(eng.status().bridged, u32::from(restore_stale_credit));
            }
        }
        assert_eq!(negative_controls, 4);
    }

    #[test]
    fn a_cold_engine_s_first_coalesced_echo_lays_the_keys_it_held() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let drive = |presses: u16| -> Engine {
            let mut eng = engaged();
            blank_row(&mut eng, 4);
            let mut t = t0;
            for _ in 0..presses {
                eng.on_event(typed(1), t);
                tick_at(&mut eng, t);
                t += ms(83);
            }
            let echo = t + ms(30);
            eng.on_event(mv((5, 2), (5, 4), Licence::Typed), echo);
            tick_at(&mut eng, echo);
            eng
        };
        let cold = drive(2);
        assert!(
            cold.field_at(5, 2).is_some() && cold.field_at(5, 3).is_some(),
            "both held keys' cells are laid by the line's first echo: {:?}",
            cold.ribbon()
                .cells()
                .iter()
                .map(|c| (c.row, c.col, c.typing))
                .collect::<Vec<_>>()
        );
        assert!(
            cold.field_at(5, 4).is_none(),
            "the landing is the caret's own cell, not a glyph's"
        );
        assert_eq!(cold.status().bridged, 2, "two cells laid from the ledger");
        assert_eq!(cold.echo.total(), 0, "both presses spent");
        assert_eq!(cold.caret_mirror(), (5, 4), "…and the mirror is seeded");
        let control = drive(0);
        assert!(
            control.field_at(5, 2).is_none() && control.field_at(5, 3).is_none(),
            "a cold hop no press explains is not typing"
        );
        assert_eq!(control.status().bridged, 0);
        let short = drive(1);
        assert!(
            short.field_at(5, 2).is_none() && short.field_at(5, 3).is_none(),
            "one press, two cells: refused"
        );
        assert_eq!(short.status().bridged, 0);
    }

    #[test]
    fn a_reset_or_a_curtain_forgets_the_presses() {
        let ms = Duration::from_millis;
        type Cut = fn(&mut Engine, Instant);
        let cuts: [(&str, Cut); 2] = [
            ("reset", |eng, _| eng.reset()),
            ("curtain", |eng, t| eng.curtain(t)),
        ];
        for (what, cut) in cuts {
            let t0 = Instant::now();
            let mut eng = engaged();
            let t = three_typed_cells(&mut eng, t0) + ms(100);
            eng.on_event(typed(1), t);
            tick_at(&mut eng, t);
            assert_eq!(eng.echo.total(), 1, "{what}: PRECONDITION: one press waits");
            cut(&mut eng, t);
            let hop = t + ms(30);
            eng.on_event(mv((3, 5), (3, 6), Licence::Typed), hop);
            tick_at(&mut eng, hop);
            assert_eq!(
                eng.status().bridged,
                0,
                "{what}: a press banked before the cut paid for the first move after it"
            );
        }
    }

    #[test]
    fn a_rewrite_offers_no_impulse_drains_no_spine_throws_no_star_and_keeps_the_ledger() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let t = three_typed_cells(&mut eng, t0);
        let t = t + ms(100);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        assert_eq!(eng.echo.total(), 1, "PRECONDITION: one press on the ledger");
        let stars_before = eng.status().stars;
        let disp_before = eng.spine().disp();
        let t = t + ms(300);
        eng.on_event(
            Event::Rewrite {
                row: 3,
                col: 3,
                cells: 2,
            },
            t,
        );
        assert_eq!(
            eng.spine().disp(),
            disp_before,
            "a rewrite drains no spine at its edge"
        );
        assert_eq!(eng.caret, (3, 3), "the caret mirror moves to the rewrite");
        assert_eq!(eng.echo.total(), 1, "the ledger is kept");
        let mut fr = sc.frame();
        eng.tick(t, geom(), &cfg, &mut fr);
        assert!(fr.companion.is_none(), "no impulse on the rewrite's frame");
        assert!(eng.take_companion_impulse().is_none(), "no Wince");
        assert!(sc.cues.is_empty(), "a rewrite sounds nothing");
        assert!(
            eng.status().stars <= stars_before,
            "a rewrite throws no star (the retracted cells' field stars may finish): {} > {stars_before}",
            eng.status().stars
        );
        let done = t + ms((12 * 2 + 240) + 240 + 20);
        tick_at(&mut eng, done);
        assert!(
            eng.field_at(3, 3).is_none() && eng.field_at(3, 4).is_none(),
            "the cells right of the rewrite's caret are retracted"
        );
        let bridged = eng.status().bridged;
        let t = type_key_at(&mut eng, done + ms(50), 3);
        tick_at(&mut eng, t);
        assert_eq!(
            eng.status().bridged,
            bridged,
            "no gap opened, nothing bridged"
        );
        assert!(
            eng.field_at(3, 3).is_some(),
            "the key after the rewrite lays at the new caret"
        );
    }

    #[test]
    fn an_inserts_sweep_pays_the_hole_a_late_key_left_before_it() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        let key = t + ms(100);
        eng.on_event(typed(1), key);
        tick_at(&mut eng, key);
        let delivered = key + ms(400);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 6,
                col1: 17,
            },
            delivered,
        );
        eng.on_event(mv((3, 6), (3, 17), Licence::Insert), delivered + ms(20));
        tick_at(&mut eng, delivered + ms(20));
        assert!(
            eng.field_at(3, 5).is_some(),
            "the late key's cell is paid by its own press through the insert's sweep"
        );
        assert!(
            (6..17u16).all(|col| eng.field_at(3, col).is_some()),
            "the insert's span is lit"
        );
        assert_eq!(eng.status().bridged, 1, "one bridged cell — the key's");
        assert_eq!(eng.echo.total(), 0, "the press is spent");
        assert_eq!(eng.caret, (3, 17));
        let mut typed_eng = engaged();
        let t = three_typed_cells(&mut typed_eng, t0);
        let key = t + ms(100);
        typed_eng.on_event(typed(1), key);
        tick_at(&mut typed_eng, key);
        let delivered = key + ms(400);
        typed_eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 6,
                col1: 17,
            },
            delivered,
        );
        typed_eng.on_event(mv((3, 6), (3, 17), Licence::Typed), delivered + ms(20));
        tick_at(&mut typed_eng, delivered + ms(20));
        assert!(typed_eng.field_at(3, 5).is_none());
        assert_eq!(typed_eng.echo.total(), 0, "…and the press is forfeited");
    }

    #[test]
    fn a_sweep_on_the_origin_row_before_a_fold_move_lays_the_last_column_and_clears_the_ledger() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        let key = t + ms(100);
        eng.on_event(typed(1), key);
        tick_at(&mut eng, key);
        assert_eq!(eng.echo.total(), 1, "PRECONDITION: one press on the ledger");
        let echo = key + ms(400);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 39,
                col1: 40,
            },
            echo - ms(250),
        );
        eng.on_event(mv((3, 39), (4, 0), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        assert!(
            eng.field_at(3, 39).is_some(),
            "the origin row's last column is lit by the seam's sweep"
        );
        assert_eq!(eng.echo.total(), 0, "a row change clears the ledger");
        assert_eq!(eng.status().bridged, 0, "…and bridges nothing");
        assert_eq!(eng.caret, (4, 0), "the mirror lands on the new row");
    }

    #[test]
    fn the_real_engine_conforms_to_the_echo_ledger_model() {
        let model = aterm_models_effects::echo_ledger_bridge_model();
        let ms = Duration::from_millis;
        let patience = Duration::from_secs_f32(ECHO_PATIENCE_S);
        type State = aterm_spec::interp::State;

        #[derive(Clone, Copy, Default)]
        struct Last {
            laid_hole: i64,
            bridged_delta: i64,
            last_older: i64,
            just_refused: i64,
        }
        let n = |v: usize| i64::try_from(v).expect("bounded ledger");
        let project = |eng: &Engine, now: Instant, key: Instant, host_col: u16, last: Last| {
            let (older, younger, stale) = eng.echo.partition(key, now);
            let tally = eng.echo.tally;
            let mut st = model.init_state();
            st.insert("older", n(older));
            st.insert("younger", n(younger));
            st.insert("stale", n(stale));
            st.insert("hole", i64::from(host_col - eng.caret.1));
            st.insert("banked", i64::from(tally.banked));
            st.insert("spent", i64::from(tally.spent));
            st.insert("forfeited", i64::from(tally.forfeited));
            st.insert("expired", i64::from(tally.expired));
            st.insert("stale_gone", i64::from(tally.expired) + n(stale));
            st.insert("bridged", i64::from(eng.status().bridged));
            st.insert("laid_hole", last.laid_hole);
            st.insert("bridged_delta", last.bridged_delta);
            st.insert("last_older", last.last_older);
            st.insert("just_refused", last.just_refused);
            st
        };
        let bind = |prev: &State, next: &State, action: &str, label: &str| {
            let (ok, why) = aterm_spec::verify::validate_transition_tiered(
                &model,
                &[],
                prev,
                next,
                Some(action),
                label,
            );
            assert!(ok, "{label}: real engine step rejected by the model: {why}");
        };
        let refuse = |prev: &State, next: &State, action: &str, label: &str| {
            let (ok, _) = aterm_spec::verify::validate_transition_tiered(
                &model,
                &[],
                prev,
                next,
                Some(action),
                label,
            );
            assert!(!ok, "{label}: the model must refuse this step");
        };
        let seeded = |t0: Instant, col: u16| -> Engine {
            let mut eng = engaged();
            eng.on_event(mv((3, 0), (3, col), Licence::Typed), t0);
            tick_at(&mut eng, t0);
            eng
        };
        let press = |eng: &mut Engine, t: Instant| {
            eng.on_event(typed(1), t);
            tick_at(eng, t);
        };
        let moved = |eng: &mut Engine,
                     from: (u16, u16),
                     to: (u16, u16),
                     licence: Licence,
                     key: Option<Instant>,
                     at: Instant,
                     prev: &State,
                     just_refused: i64|
         -> Last {
            let before = eng.status().bridged;
            if let Some(key) = key {
                eng.on_event(
                    Event::Sweep {
                        row: to.0,
                        col0: from.1,
                        col1: to.1,
                    },
                    key,
                );
            }
            eng.on_event(mv(from, to, licence), at);
            tick_at(eng, at);
            assert_eq!(
                eng.last_host_sweep_at(),
                key,
                "the engine partitions by the host's sweep clock, the key's"
            );
            let delta = eng.status().bridged - before;
            let past_from = if key.is_some() {
                0
            } else {
                u32::from(to.1.saturating_sub(from.1))
            };
            Last {
                laid_hole: i64::from(delta.saturating_sub(past_from)),
                bridged_delta: i64::from(delta),
                last_older: prev["older"],
                just_refused,
            }
        };
        let cleared = |eng: &mut Engine, ev: Event, at: Instant, last: Last| -> Last {
            let before = eng.status().bridged;
            eng.on_event(ev, at);
            tick_at(eng, at);
            let delta = i64::from(eng.status().bridged - before);
            Last {
                laid_hole: delta,
                bridged_delta: delta,
                ..last
            }
        };

        let t0 = Instant::now();
        let mut eng = seeded(t0, 2);
        let mut last = Last::default();
        let s0 = project(&eng, t0, t0, 2, last);
        assert_eq!(
            s0,
            model.init_state(),
            "a seeded engine is the model's init"
        );

        let t1 = t0 + ms(100);
        press(&mut eng, t1);
        let s1 = project(&eng, t1, t1, 2, last);
        assert_eq!((s1["older"], s1["younger"], s1["banked"]), (0, 1, 1));
        bind(&s0, &s1, "KeyPressed", "Engine press");
        let mut predrawn = s1.clone();
        predrawn.insert("bridged", 1);
        refuse(
            &s0,
            &predrawn,
            "KeyPressed",
            "Engine keydown pre-draw control",
        );

        let e1 = t1 + ms(8);
        last = moved(
            &mut eng,
            (3, 2),
            (3, 3),
            Licence::Typed,
            Some(t1),
            e1,
            &s1,
            0,
        );
        let s2 = project(&eng, e1, t1, 3, last);
        assert_eq!((s2["spent"], s2["bridged"], s2["younger"]), (1, 0, 0));
        assert_eq!((last.laid_hole, last.bridged_delta), (0, 0));
        bind(&s1, &s2, "SweptMovePays", "Engine on-time echo");
        let mut reused = s2.clone();
        reused.insert("younger", 1);
        refuse(
            &s1,
            &reused,
            "SweptMovePays",
            "Engine one-press-one-cell control",
        );

        let t2 = e1 + ms(300);
        press(&mut eng, t2);
        let s3 = project(&eng, t2, t2, 3, last);
        bind(&s2, &s3, "KeyPressed", "Engine stalled press");
        let s4 = project(&eng, t2 + ms(300), t2, 4, last);
        assert_eq!(s4["hole"], 1, "the caret advanced with no licensed move");
        bind(&s3, &s4, "UnlicensedAdvance", "Engine refused echo");

        let t3 = t2 + ms(400);
        press(&mut eng, t3);
        let s5 = project(&eng, t3, t3, 4, last);
        assert_eq!((s5["older"], s5["younger"], s5["hole"]), (1, 1, 1));
        bind(&s4, &s5, "KeyPressed", "Engine next key");

        let e3 = t3 + ms(8);
        last = moved(
            &mut eng,
            (3, 4),
            (3, 5),
            Licence::Typed,
            Some(t3),
            e3,
            &s5,
            0,
        );
        let s6 = project(&eng, e3, t3, 5, last);
        assert_eq!(s6["bridged"], 1, "the refused key's cell is relit");
        assert!(eng.field_at(3, 3).is_some(), "…and is a ribbon cell");
        assert_eq!(
            (last.laid_hole, last.bridged_delta, last.last_older),
            (1, 1, 1)
        );
        assert_eq!(
            (s6["spent"], s6["older"] + s6["younger"], s6["hole"]),
            (3, 0, 0)
        );
        bind(&s5, &s6, "SweptMovePays", "Engine relight");
        let mut overlaid = s6.clone();
        overlaid.insert("bridged", 2);
        overlaid.insert("bridged_delta", 2);
        overlaid.insert("laid_hole", 2);
        refuse(
            &s5,
            &overlaid,
            "SweptMovePays",
            "Engine one-older-press-two-cells control",
        );

        let u0 = Instant::now();
        let mut eng = seeded(u0, 5);
        let mut last = Last::default();
        let b0 = project(&eng, u0, u0, 5, last);
        let b1 = project(&eng, u0 + ms(100), u0, 6, last);
        bind(&b0, &b1, "UnlicensedAdvance", "Engine program nudge");
        let u1 = u0 + ms(300);
        press(&mut eng, u1);
        let b2 = project(&eng, u1, u1, 6, last);
        bind(&b1, &b2, "KeyPressed", "Engine burst key 1");
        let u2 = u1 + ms(40);
        press(&mut eng, u2);
        let b3 = project(&eng, u2, u1, 6, last);
        assert_eq!((b3["older"], b3["younger"], b3["hole"]), (0, 2, 1));
        bind(&b2, &b3, "PressInFlight", "Engine burst key 2");
        let e1 = u2 + ms(8);
        last = moved(
            &mut eng,
            (3, 6),
            (3, 7),
            Licence::Typed,
            Some(u1),
            e1,
            &b3,
            1,
        );
        let b4 = project(&eng, e1, u1, 7, last);
        assert_eq!(b4["bridged"], 0, "the nudge's cell is not the burst's");
        assert!(eng.field_at(3, 5).is_none(), "…and stays dark");
        assert_eq!((b4["forfeited"], b4["older"] + b4["younger"]), (2, 0));
        assert_eq!((last.laid_hole, last.bridged_delta), (0, 0));
        bind(&b3, &b4, "SweptMoveRefuses", "Engine burst refusal");
        let mut kept = b4.clone();
        kept.insert("younger", 2);
        kept.insert("forfeited", 0);
        refuse(
            &b3,
            &kept,
            "SweptMoveRefuses",
            "Engine forfeited-credits control",
        );
        let mut laid_anyway = b4.clone();
        laid_anyway.insert("bridged", 1);
        laid_anyway.insert("bridged_delta", 1);
        refuse(
            &b3,
            &laid_anyway,
            "SweptMoveRefuses",
            "Engine no-bridge-on-a-refusal control",
        );
        let e2 = e1 + ms(8);
        last = moved(
            &mut eng,
            (3, 7),
            (3, 8),
            Licence::Typed,
            Some(u2),
            e2,
            &b4,
            1,
        );
        let b5 = project(&eng, e2, u2, 8, last);
        assert_eq!(b5["bridged"], 0);
        assert!(
            eng.field_at(3, 7).is_some(),
            "the key's own glyph is the host's"
        );
        bind(&b4, &b5, "SweptMoveRefuses", "Engine burst key 2 echo");

        let v0 = Instant::now();
        let mut eng = seeded(v0, 5);
        let mut last = Last::default();
        let p0 = project(&eng, v0, v0, 5, last);
        let v1 = v0 + ms(100);
        press(&mut eng, v1);
        let p1 = project(&eng, v1, v1, 5, last);
        bind(&p0, &p1, "KeyPressed", "Engine swallowed press");
        let v2 = v1 + ms(200);
        press(&mut eng, v2);
        let p2 = project(&eng, v2, v2, 5, last);
        assert_eq!((p2["older"], p2["younger"], p2["hole"]), (1, 1, 0));
        bind(&p1, &p2, "KeyPressed", "Engine ordinary key");
        let e2 = v2 + ms(8);
        last = moved(
            &mut eng,
            (3, 5),
            (3, 6),
            Licence::Typed,
            Some(v2),
            e2,
            &p2,
            1,
        );
        let p3 = project(&eng, e2, v2, 6, last);
        assert_eq!((p3["forfeited"], p3["older"] + p3["younger"]), (2, 0));
        bind(&p2, &p3, "SweptMoveRefuses", "Engine phantom dropped");
        let p4 = project(&eng, e2 + ms(100), v2, 7, last);
        bind(
            &p3,
            &p4,
            "UnlicensedAdvance",
            "Engine nudge after the phantom",
        );
        let v3 = e2 + ms(200);
        press(&mut eng, v3);
        last.just_refused = 0;
        let p5 = project(&eng, v3, v3, 7, last);
        bind(&p4, &p5, "KeyPressed", "Engine key after the nudge");
        let e3 = v3 + ms(8);
        last = moved(
            &mut eng,
            (3, 7),
            (3, 8),
            Licence::Typed,
            Some(v3),
            e3,
            &p5,
            1,
        );
        let p6 = project(&eng, e3, v3, 8, last);
        assert_eq!(p6["bridged"], 0, "the phantom credit never rolled forward");
        assert!(eng.field_at(3, 6).is_none());
        bind(&p5, &p6, "SweptMoveRefuses", "Engine phantom cannot pay");

        let w0 = Instant::now();
        let mut eng = seeded(w0, 5);
        let mut last = Last::default();
        let r0 = project(&eng, w0, w0, 5, last);
        let w1 = w0 + ms(100);
        press(&mut eng, w1);
        let r1 = project(&eng, w1, w1, 5, last);
        bind(&r0, &r1, "KeyPressed", "Engine press before a retreat");
        let r2 = project(&eng, w1 + ms(100), w1, 6, last);
        bind(&r1, &r2, "UnlicensedAdvance", "Engine hop before a retreat");
        let w2 = w1 + ms(200);
        last = moved(&mut eng, (3, 6), (3, 4), Licence::Typed, None, w2, &r2, 0);
        let r3 = project(&eng, w2, w1, 4, last);
        assert_eq!((r3["younger"], r3["hole"], r3["forfeited"]), (1, 0, 0));
        bind(&r2, &r3, "RetreatKeepsPresses", "Engine fresh retreat");
        let w3 = w2 + ms(200);
        last = moved(&mut eng, (3, 4), (3, 9), Licence::Nav, None, w3, &r3, 0);
        let r4 = project(&eng, w3, w1, 9, last);
        assert_eq!((r4["older"] + r4["younger"], r4["forfeited"]), (0, 1));
        bind(&r3, &r4, "LedgerForgotten", "Engine nav hop");
        let w4 = w3 + ms(100);
        press(&mut eng, w4);
        let r5 = project(&eng, w4, w4, 9, last);
        bind(
            &r4,
            &r5,
            "KeyPressed",
            "Engine press before a stale retreat",
        );
        let w5 = w4 + patience + ms(100);
        let r6 = project(&eng, w5, w4, 9, last);
        assert_eq!((r6["stale"], r6["younger"]), (1, 0));
        bind(&r5, &r6, "TimePasses", "Engine patience before a retreat");
        last = moved(&mut eng, (3, 9), (3, 8), Licence::Typed, None, w5, &r6, 0);
        let r7 = project(&eng, w5, w4, 8, last);
        assert_eq!(
            (r7["stale"], r7["expired"], r7["younger"], r7["hole"]),
            (0, 1, 0, 0)
        );
        bind(&r6, &r7, "RetreatKeepsPresses", "Engine stale retreat");

        let d0 = Instant::now();
        let mut eng = seeded(d0, 8);
        let mut last = Last::default();
        let a0 = project(&eng, d0, d0, 8, last);
        let d1 = d0 + ms(100);
        press(&mut eng, d1);
        let a1 = project(&eng, d1, d1, 8, last);
        bind(&a0, &a1, "KeyPressed", "Engine press before an erase");
        let d2 = d1 + patience + ms(100);
        let a2 = project(&eng, d2, d1, 8, last);
        bind(&a1, &a2, "TimePasses", "Engine patience before an erase");
        last = cleared(&mut eng, Event::Erase, d2, last);
        let a3 = project(&eng, d2, d1, 8, last);
        assert_eq!((a3["stale"], a3["expired"], a3["forfeited"]), (0, 1, 0));
        bind(
            &a2,
            &a3,
            "LedgerClearedInPlace",
            "Engine erase over a stale press",
        );
        let d3 = d2 + ms(100);
        press(&mut eng, d3);
        let a4 = project(&eng, d3, d3, 8, last);
        bind(&a3, &a4, "KeyPressed", "Engine press before a kill");
        let a5 = project(&eng, d3 + ms(50), d3, 9, last);
        bind(&a4, &a5, "UnlicensedAdvance", "Engine hop before a kill");
        let d4 = d3 + ms(100);
        last = cleared(
            &mut eng,
            Event::Kill {
                cells: 1,
                scope: KillScope::Word,
            },
            d4,
            last,
        );
        let a6 = project(&eng, d4, d3, 9, last);
        assert_eq!((a6["younger"], a6["forfeited"], a6["hole"]), (0, 1, 1));
        bind(
            &a5,
            &a6,
            "LedgerClearedInPlace",
            "Engine kill beside a hole",
        );
        let d5 = d4 + ms(100);
        press(&mut eng, d5);
        let a7 = project(&eng, d5, d5, 9, last);
        bind(&a6, &a7, "KeyPressed", "Engine press before a focus loss");
        let d6 = d5 + ms(100);
        last = cleared(&mut eng, Event::Focus(false), d6, last);
        let a8 = project(&eng, d6, d5, 9, last);
        assert_eq!((a8["forfeited"], a8["hole"]), (2, 1));
        bind(&a7, &a8, "LedgerClearedInPlace", "Engine focus loss");

        let z0 = Instant::now();
        let mut eng = seeded(z0, 5);
        let mut last = Last::default();
        let c0 = project(&eng, z0, z0, 5, last);
        let z1 = z0 + ms(100);
        press(&mut eng, z1);
        let c1 = project(&eng, z1, z1, 5, last);
        bind(&c0, &c1, "KeyPressed", "Engine press before vim's w");
        let z2 = z1 + ms(50);
        last = moved(&mut eng, (3, 5), (3, 8), Licence::Typed, None, z2, &c1, 1);
        let c2 = project(&eng, z2, z1, 8, last);
        assert_eq!(
            (c2["forfeited"], c2["bridged"], c2["older"] + c2["younger"]),
            (1, 0, 0)
        );
        assert_eq!((last.laid_hole, last.bridged_delta), (0, 0));
        bind(&c1, &c2, "UnsweptMoveRefuses", "Engine vim's w");

        let x0 = Instant::now();
        let mut eng = seeded(x0, 5);
        let mut last = Last::default();
        let q0 = project(&eng, x0, x0, 5, last);
        let x1 = x0 + ms(100);
        press(&mut eng, x1);
        let q1 = project(&eng, x1, x1, 5, last);
        bind(&q0, &q1, "KeyPressed", "Engine press before the patience");
        let x2 = x1 + patience + ms(500);
        let q2 = project(&eng, x2, x1, 5, last);
        assert_eq!((q2["stale"], q2["younger"], q2["stale_gone"]), (1, 0, 1));
        bind(&q1, &q2, "TimePasses", "Engine patience elapses");
        let q3 = project(&eng, x2, x1, 6, last);
        bind(
            &q2,
            &q3,
            "UnlicensedAdvance",
            "Engine hop after the patience",
        );
        press(&mut eng, x2);
        let q4 = project(&eng, x2, x2, 6, last);
        assert_eq!((q4["older"], q4["younger"], q4["stale"]), (0, 1, 1));
        bind(&q3, &q4, "KeyPressed", "Engine key after the patience");
        let e2 = x2 + ms(8);
        last = moved(
            &mut eng,
            (3, 6),
            (3, 7),
            Licence::Typed,
            Some(x2),
            e2,
            &q4,
            1,
        );
        let q5 = project(&eng, e2, x2, 7, last);
        assert_eq!((q5["expired"], q5["forfeited"], q5["bridged"]), (1, 1, 0));
        assert!(eng.field_at(3, 5).is_none(), "a swallowed key buys no cell");
        bind(&q4, &q5, "SweptMoveRefuses", "Engine expired press");

        let f0 = Instant::now();
        let mut eng = seeded(f0, 5);
        let mut last = Last::default();
        let g0 = project(&eng, f0, f0, 5, last);
        let f1 = f0 + ms(100);
        press(&mut eng, f1);
        let g1 = project(&eng, f1, f1, 5, last);
        bind(
            &g0,
            &g1,
            "KeyPressed",
            "Engine press before a partial expiry",
        );
        let f2 = f1 + ms(1500);
        press(&mut eng, f2);
        let g2 = project(&eng, f2, f2, 5, last);
        assert_eq!((g2["older"], g2["younger"], g2["stale"]), (1, 1, 0));
        bind(&g1, &g2, "KeyPressed", "Engine key before a partial expiry");
        let f3 = f1 + patience + ms(100);
        let g3 = project(&eng, f3, f2, 5, last);
        assert_eq!((g3["older"], g3["younger"], g3["stale"]), (0, 1, 1));
        bind(
            &g2,
            &g3,
            "OnePressGoesStale",
            "Engine oldest press goes stale",
        );
        last = moved(
            &mut eng,
            (3, 5),
            (3, 6),
            Licence::Typed,
            Some(f2),
            f3,
            &g3,
            0,
        );
        let g4 = project(&eng, f3, f2, 6, last);
        assert_eq!(
            (
                g4["expired"],
                g4["stale"],
                g4["younger"],
                g4["spent"],
                g4["bridged"]
            ),
            (1, 0, 0, 1, 0)
        );
        bind(
            &g3,
            &g4,
            "SweptMovePays",
            "Engine echo after a partial expiry",
        );

        let h0 = Instant::now();
        let mut eng = seeded(h0, 5);
        let mut last = Last::default();
        let k0 = project(&eng, h0, h0, 5, last);
        let h1 = h0 + ms(100);
        press(&mut eng, h1);
        let k1 = project(&eng, h1, h1, 5, last);
        bind(&k0, &k1, "KeyPressed", "Engine press that will go stale");
        let h2 = h1 + patience + ms(100);
        let k2 = project(&eng, h2, h1, 5, last);
        bind(&k1, &k2, "TimePasses", "Engine press gone stale");
        press(&mut eng, h2);
        let k3 = project(&eng, h2, h2, 5, last);
        assert_eq!(
            (k3["older"], k3["younger"], k3["stale"], k3["hole"]),
            (0, 1, 1, 0)
        );
        bind(&k2, &k3, "KeyPressed", "Engine key beside a stale press");
        let e3 = h2 + ms(8);
        last = moved(
            &mut eng,
            (3, 5),
            (3, 6),
            Licence::Typed,
            Some(h2),
            e3,
            &k3,
            0,
        );
        let k4 = project(&eng, e3, h2, 6, last);
        assert_eq!(
            (k4["expired"], k4["stale"], k4["younger"], k4["spent"]),
            (1, 0, 0, 1)
        );
        bind(
            &k3,
            &k4,
            "SweptMovePays",
            "Engine key pays beside a stale press",
        );
        let mut stale_paid = k4.clone();
        stale_paid.insert("expired", 0);
        stale_paid.insert("younger", 1);
        stale_paid.insert("spent", 1);
        refuse(
            &k3,
            &stale_paid,
            "SweptMovePays",
            "Engine stale-press-pays control",
        );

        let y0 = Instant::now();
        let mut eng = seeded(y0, 5);
        let mut last = Last::default();
        let m0 = project(&eng, y0, y0, 5, last);
        let y1 = y0 + ms(100);
        press(&mut eng, y1);
        let m1 = project(&eng, y1, y1, 5, last);
        bind(&m0, &m1, "KeyPressed", "Engine batch press 1");
        press(&mut eng, y1 + ms(700));
        let m2 = project(&eng, y1 + ms(700), y1, 5, last);
        bind(&m1, &m2, "PressInFlight", "Engine batch press 2");
        let y2 = y1 + ms(750);
        last = moved(&mut eng, (3, 5), (3, 7), Licence::Typed, None, y2, &m2, 0);
        let m3 = project(&eng, y2, y1, 7, last);
        assert_eq!(
            (m3["bridged"], m3["spent"], m3["older"] + m3["younger"]),
            (2, 2, 0)
        );
        assert_eq!(
            (last.laid_hole, last.bridged_delta),
            (0, 2),
            "both cells are the key's and the one behind it, none the hole's"
        );
        assert!(eng.field_at(3, 5).is_some() && eng.field_at(3, 6).is_some());
        bind(&m2, &m3, "UnsweptMovePays", "Engine unswept batch");
        let y3 = y2 + ms(200);
        press(&mut eng, y3);
        let m4 = project(&eng, y3, y3, 7, last);
        bind(&m3, &m4, "KeyPressed", "Engine batch press 3");
        press(&mut eng, y3 + ms(100));
        let m5 = project(&eng, y3 + ms(100), y3, 7, last);
        bind(&m4, &m5, "PressInFlight", "Engine batch press 4");
        let m6 = project(&eng, y3 + ms(120), y3, 8, last);
        bind(&m5, &m6, "UnlicensedAdvance", "Engine hop before a batch");
        let y4 = y3 + ms(150);
        last = moved(&mut eng, (3, 8), (3, 10), Licence::Typed, None, y4, &m6, 1);
        let m7 = project(&eng, y4, y3, 10, last);
        assert_eq!((m7["bridged"], m7["forfeited"]), (2, 2));
        assert_eq!((last.laid_hole, last.bridged_delta), (0, 0));
        assert!(eng.field_at(3, 7).is_none(), "the hop's cell stays dark");
        bind(
            &m6,
            &m7,
            "UnsweptMoveRefuses",
            "Engine unswept batch after a hop",
        );

        let v0 = Instant::now();
        let mut eng = seeded(v0, 5);
        let mut last = Last::default();
        let o0 = project(&eng, v0, v0, 5, last);
        let v1 = v0 + ms(100);
        press(&mut eng, v1);
        let o1 = project(&eng, v1, v1, 5, last);
        bind(&o0, &o1, "KeyPressed", "Engine press before a relocation");
        let o2 = project(&eng, v1 + ms(50), v1, 6, last);
        bind(
            &o1,
            &o2,
            "UnlicensedAdvance",
            "Engine hop before a relocation",
        );
        let v2 = v1 + ms(100);
        let before = eng.status().bridged;
        eng.observe_caret((4, 2));
        tick_at(&mut eng, v2);
        assert_eq!(
            eng.caret_mirror(),
            (4, 2),
            "the mirror followed the observed row"
        );
        last = Last {
            laid_hole: 0,
            bridged_delta: i64::from(eng.status().bridged - before),
            last_older: o2["older"],
            ..last
        };
        let o3 = project(&eng, v2, v1, 2, last);
        assert_eq!(
            (o3["older"] + o3["younger"], o3["forfeited"], o3["hole"]),
            (0, 1, 0),
            "the press is forfeited and the hole is measured on the new row"
        );
        bind(&o2, &o3, "LedgerForgotten", "Engine declined relocation");
        let v3 = v2 + ms(100);
        press(&mut eng, v3);
        let o4 = project(&eng, v3, v3, 2, last);
        bind(
            &o3,
            &o4,
            "KeyPressed",
            "Engine press that goes stale before a relocation",
        );
        let v4 = v3 + patience + ms(100);
        let o5 = project(&eng, v4, v3, 2, last);
        bind(
            &o4,
            &o5,
            "TimePasses",
            "Engine patience before a relocation",
        );
        eng.observe_caret((6, 0));
        tick_at(&mut eng, v4);
        last = Last {
            laid_hole: 0,
            bridged_delta: 0,
            last_older: o5["older"],
            ..last
        };
        let o6 = project(&eng, v4, v3, 0, last);
        assert_eq!((o6["expired"], o6["forfeited"], o6["stale"]), (1, 1, 0));
        bind(
            &o5,
            &o6,
            "LedgerForgotten",
            "Engine relocation over a stale press",
        );
    }

    #[test]
    fn a_declined_row_change_moves_the_mirror_after_the_ingest_and_the_next_key_lays_on_the_new_row()
     {
        let t0 = Instant::now();
        let mut eng = engaged();
        eng.on_event(mv((3, 0), (3, 5), Licence::Typed), t0);
        tick_at(&mut eng, t0);
        assert_eq!(eng.caret_mirror(), (3, 5));
        let t1 = t0 + Duration::from_millis(100);
        eng.on_event(typed(1), t1);
        eng.observe_caret((5, 2));
        assert_eq!(
            eng.caret_mirror(),
            (3, 5),
            "the mirror moves at the END of the tick"
        );
        tick_at(&mut eng, t1);
        assert!(
            eng.field_at(3, 4).is_none(),
            "the buffered key is held at the caret it was typed at, not laid"
        );
        assert!(
            eng.field_at(5, 1).is_none(),
            "…and nothing on the new row yet"
        );
        assert_eq!(
            eng.caret_mirror(),
            (5, 2),
            "from the next tick the mirror is the observed caret"
        );
        assert_eq!(eng.echo.total(), 0, "the press was forgotten with the row");
        let t2 = t1 + Duration::from_millis(100);
        eng.on_event(typed(1), t2);
        tick_at(&mut eng, t2);
        assert!(
            eng.field_at(5, 1).is_none(),
            "a key whose echo has not landed lays nothing — not the marker's cell"
        );
        assert!(
            eng.field_at(3, 5).is_none() && eng.field_at(3, 6).is_none(),
            "…and nothing new on the row the caret left"
        );
        let e2 = t2 + Duration::from_millis(8);
        eng.on_event(
            Event::Sweep {
                row: 5,
                col0: 2,
                col1: 3,
            },
            t2,
        );
        eng.on_event(mv((5, 2), (5, 3), Licence::Typed), e2);
        tick_at(&mut eng, e2);
        assert_eq!(eng.status().bridged, 0, "no phantom credit lit a cell");
        assert!(
            eng.field_at(5, 2).is_some(),
            "the host's sweep laid the glyph cell"
        );
        assert!(
            eng.field_at(5, 1).is_none(),
            "the key after the relocation lays on the new row, at its own cell only"
        );
    }

    #[test]
    fn a_same_row_observation_leaves_the_mirror_for_the_ledger_to_measure() {
        let t0 = Instant::now();
        let mut eng = engaged();
        eng.on_event(mv((3, 0), (3, 5), Licence::Typed), t0);
        tick_at(&mut eng, t0);
        let t1 = t0 + Duration::from_millis(100);
        eng.on_event(typed(1), t1);
        tick_at(&mut eng, t1);
        eng.observe_caret((3, 6));
        tick_at(&mut eng, t1 + Duration::from_millis(300));
        assert_eq!(
            eng.caret_mirror(),
            (3, 5),
            "a same-row observation is not a move"
        );
        assert_eq!(eng.echo.total(), 1, "…and the press keeps waiting");
        let t2 = t1 + Duration::from_millis(400);
        eng.on_event(typed(1), t2);
        tick_at(&mut eng, t2);
        let e2 = t2 + Duration::from_millis(8);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 6,
                col1: 7,
            },
            t2,
        );
        eng.on_event(mv((3, 6), (3, 7), Licence::Typed), e2);
        tick_at(&mut eng, e2);
        assert_eq!(
            eng.status().bridged,
            1,
            "the late echo's cell was relit by the ledger"
        );
        assert!(eng.field_at(3, 5).is_some(), "…at the hole");
    }

    #[test]
    fn a_fresh_engine_ignores_an_observed_caret_until_a_licensed_move_seeds_the_mirror() {
        let t0 = Instant::now();
        let mut eng = engaged();
        eng.observe_caret((5, 0));
        tick_at(&mut eng, t0);
        assert_eq!(eng.caret_mirror(), (0, 0), "an observation seeds nothing");
        let t1 = t0 + Duration::from_millis(100);
        eng.on_event(typed(1), t1);
        tick_at(&mut eng, t1);
        assert_eq!(
            eng.status().cells,
            0,
            "a key before the first licensed move lays nothing — not on row 4's last cell either"
        );
    }

    #[test]
    fn a_witness_retirement_is_counted_survives_a_reset_and_is_never_undone_by_a_payout() {
        let t0 = Instant::now();
        let mut eng = engaged();
        eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
        tick_at(&mut eng, t0);
        let mut t = t0;
        for col in 2..5u16 {
            t = type_key_at(&mut eng, t + Duration::from_millis(100), col);
        }
        assert_eq!(eng.status().cells, 3, "three cells: `abc`");
        assert!(
            eng.field_at(3, 1).is_none(),
            "no neighbour cell before the run"
        );
        let abc: Vec<char> = "  abc".chars().collect();
        let axc: Vec<char> = "  abX".chars().collect();
        let seen_abc = [RowSample { row: 3, cols: &abc }];
        let seen_axc = [RowSample { row: 3, cols: &axc }];
        assert_eq!(
            eng.witness_rows(&seen_abc, t),
            0,
            "the first glyph seen arms"
        );
        assert_eq!(
            eng.witness_rows(&seen_abc, t),
            0,
            "the same glyph retires nothing"
        );
        let fired = t + Duration::from_millis(50);
        assert_eq!(
            eng.witness_rows(&seen_axc, fired),
            1,
            "c -> X retires the `c` cell"
        );
        assert_eq!(eng.status().retired, 1);
        let leaving = |eng: &Engine| -> Vec<u16> {
            let mut v: Vec<u16> = eng
                .ribbon()
                .cells()
                .iter()
                .filter(|c| c.leaving())
                .map(|c| c.col)
                .collect();
            v.sort_unstable();
            v
        };
        assert_eq!(
            leaving(&eng),
            vec![4],
            "`c` is on the melt; `a` and `b` stand"
        );
        let p1 = fired + Duration::from_millis(10);
        eng.on_event(typed(1), p1);
        tick_at(&mut eng, p1);
        let p2 = p1 + Duration::from_millis(20);
        eng.on_event(typed(1), p2);
        tick_at(&mut eng, p2);
        let e2 = p2 + Duration::from_millis(8);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 6,
                col1: 7,
            },
            p2,
        );
        eng.on_event(mv((3, 6), (3, 7), Licence::Typed), e2);
        tick_at(&mut eng, e2);
        assert_eq!(eng.status().bridged, 1, "the hole was paid");
        assert!(eng.field_at(3, 5).is_some() && eng.field_at(3, 6).is_some());
        assert_eq!(
            leaving(&eng),
            vec![4],
            "the payout did not revive the retired cell"
        );
        tick_at(&mut eng, e2 + Duration::from_millis(200));
        assert!(
            leaving(&eng).is_empty(),
            "the melt ended: nothing is leaving"
        );
        assert!(
            eng.field_at(3, 4).is_none(),
            "…and the retired cell is out of the pool"
        );
        assert!(
            eng.field_at(3, 2).is_some()
                && eng.field_at(3, 3).is_some()
                && eng.field_at(3, 5).is_some(),
            "…while `a`, `b` and the paid cell stand"
        );
        eng.reset();
        assert_eq!(
            eng.status().retired,
            1,
            "the window's tally survives a reset"
        );
        assert!(eng.witness.is_empty());
    }

    #[test]
    fn three_disjoint_cohorts_keep_the_hands_lower_follow_row_in_the_witness() {
        let ms = Duration::from_millis;
        let (mut eng, t) = typed_runs(&[3, 7, 11]);
        let text: Vec<char> = "  ab".chars().collect();
        let blank: Vec<char> = "    ".chars().collect();
        let mut wanted = [0u16; witness::WITNESS_ROWS];
        let n = eng.ribbon_rows_for(None, &mut wanted);
        assert!(
            wanted[..n].contains(&12),
            "the hand's run on row 11 needs row 12 to follow its text; requested {:?}",
            &wanted[..n]
        );
        let original = wanted[..n]
            .iter()
            .map(|&row| RowSample {
                row,
                cols: if row == 3 || row == 7 || row == 11 {
                    &text
                } else {
                    &blank
                },
            })
            .collect::<Vec<_>>();
        assert_eq!(eng.witness_rows(&original, t), 0, "arm the three runs");
        let moved = wanted[..n]
            .iter()
            .map(|&row| RowSample {
                row,
                cols: if row == 12 || row == 3 || row == 7 {
                    &text
                } else {
                    &blank
                },
            })
            .collect::<Vec<_>>();
        assert_eq!(eng.follow_rows(&moved, &wanted[..n], t + ms(5)), 2);
        tick_at(&mut eng, t + ms(5));
        assert!(
            eng.field_at(12, 2).is_some() && eng.field_at(12, 3).is_some(),
            "the lit run followed its text onto row 12"
        );
    }

    #[test]
    fn a_box_redrawn_in_place_under_a_scroll_carries_its_lines_back() {
        let ms = Duration::from_millis;
        let (mut eng, t) = typed_runs(&[14, 15]);
        let text: Vec<char> = "  ab".chars().collect();
        let blank: Vec<char> = "    ".chars().collect();
        let sample = |asked: &[u16], lines: &[u16]| {
            asked
                .iter()
                .map(|&row| RowSample {
                    row,
                    cols: if lines.contains(&row) { &text } else { &blank },
                })
                .collect::<Vec<_>>()
        };
        let mut asked = [0u16; witness::WITNESS_ROWS];
        let n = eng.ribbon_rows_for(None, &mut asked);
        assert_eq!(eng.witness_rows(&sample(&asked[..n], &[14, 15]), t), 0);
        eng.translate_scroll(3, 16);
        assert!(
            eng.field_at(11, 2).is_some() && eng.field_at(12, 2).is_some(),
            "fixture: the scroll carried both runs up three rows"
        );
        assert_eq!(eng.witness.scroll_carry(), 3);
        let n = eng.ribbon_rows_for(None, &mut asked);
        let asked = &asked[..n];
        assert!(
            asked.contains(&14) && asked.contains(&15),
            "the rows the lines stood on are named: {asked:?}"
        );
        let redrawn = sample(asked, &[14, 15]);
        let now = t + ms(5);
        assert_eq!(eng.follow_rows(&redrawn, asked, now), 4);
        tick_at(&mut eng, now);
        assert_eq!(eng.witness_rows(&redrawn, now), 0, "nothing retired");
        for row in [14, 15] {
            assert!(
                eng.field_at(row, 2).is_some() && eng.field_at(row, 3).is_some(),
                "row {row}'s run is back under its text"
            );
        }
        assert!(
            eng.ribbon()
                .cells()
                .iter()
                .all(|c| c.row == 14 || c.row == 15),
            "no cell is left where the scroll put it"
        );
        assert_eq!(
            eng.witness.scroll_carry(),
            0,
            "the follow pass ends the carry"
        );
        eng.translate_scroll(2, 16);
        assert_eq!(
            eng.witness.scroll_carry(),
            0,
            "two rows are within the follow offsets"
        );
    }

    #[test]
    fn a_box_pushed_past_the_follow_offsets_follows_the_caret() {
        fn sample<'a>(lines: &[(u16, &'a [char])]) -> Vec<RowSample<'a>> {
            const BLANK: [char; 4] = [' '; 4];
            (9..=15u16)
                .map(|row| RowSample {
                    row,
                    cols: lines
                        .iter()
                        .find(|(r, _)| *r == row)
                        .map_or(&BLANK[..], |(_, l)| l),
                })
                .collect()
        }
        let ms = Duration::from_millis;
        let text: Vec<char> = "  ab".chars().collect();
        let other: Vec<char> = "  xy".chars().collect();
        let asked: Vec<u16> = (9..=15).collect();
        let take = |caret: bool, old_row: &[char]| {
            let (mut eng, t) = typed_runs(&[10]);
            assert_eq!(eng.witness_rows(&sample(&[(10, &text)]), t), 0);
            let now = t + ms(300);
            tick_at(&mut eng, now);
            let mut named = [0u16; witness::WITNESS_ROWS];
            let n = eng.ribbon_rows_at_caret(None, caret.then_some((10, 14)), &mut named);
            assert_eq!(
                named[..n].contains(&14),
                caret,
                "row 14 is named exactly when the caret went there: {:?}",
                &named[..n]
            );
            if caret {
                eng.observe_caret((14, 4));
                eng.carry_caret(4);
            }
            let pushed = sample(&[(10, old_row), (14, &text)]);
            let followed = eng.follow_rows(&pushed, &asked, now);
            tick_at(&mut eng, now);
            let retired = eng.witness_rows(&pushed, now);
            (eng, followed, retired)
        };
        let (eng, followed, retired) = take(true, &other);
        assert_eq!(followed, 2, "the run followed the caret");
        assert_eq!(retired, 0, "nothing melts");
        assert!(
            eng.field_at(14, 2).is_some() && eng.field_at(14, 3).is_some(),
            "the run is under its text on row 14"
        );
        assert!(
            eng.ribbon().cells().iter().all(|c| c.row == 14),
            "nothing is left on row 10"
        );
        assert_eq!(eng.caret.0, 14, "the hand rode with its line");
        let (_, followed, retired) = take(false, &other);
        assert_eq!(followed, 0, "control: no caret, no search four rows down");
        assert_eq!(retired, 2, "control: the run melts under the new text");
        let (eng, followed, retired) = take(true, &text);
        assert_eq!(followed, 0, "control: the text stayed, so nothing moved");
        assert_eq!(retired, 0, "control: and nothing melts");
        assert!(
            eng.ribbon().cells().iter().all(|c| c.row == 10),
            "control: the run stays on row 10"
        );
    }

    #[test]
    fn a_box_shifted_within_the_follow_offsets_keeps_the_hand_on_its_line() {
        fn sample<'a>(lines: &[(u16, &'a [char])]) -> Vec<RowSample<'a>> {
            const BLANK: [char; 4] = [' '; 4];
            (8..=12u16)
                .map(|row| RowSample {
                    row,
                    cols: lines
                        .iter()
                        .find(|(r, _)| *r == row)
                        .map_or(&BLANK[..], |(_, l)| l),
                })
                .collect()
        }
        let ms = Duration::from_millis;
        let text: Vec<char> = "  ab".chars().collect();
        let other: Vec<char> = "  xy".chars().collect();
        let asked: Vec<u16> = (8..=12).collect();
        for target in [8u16, 9, 11, 12] {
            let (mut eng, t) = typed_runs(&[10]);
            assert_eq!(eng.witness_rows(&sample(&[(10, &text)]), t), 0);
            let now = t + ms(600);
            tick_at(&mut eng, now);
            eng.observe_caret((target, 4));
            let shifted = sample(&[(10, &other), (target, &text)]);
            let followed = eng.follow_rows(&shifted, &asked, now);
            assert_eq!(followed, 2, "target {target}: the run followed the caret");
            assert_eq!(
                eng.caret.0, target,
                "target {target}: the hand rode with its line"
            );
            assert_eq!(
                eng.take_hand_lift(),
                Some((10, target)),
                "target {target}: the host is notified of the hand lift"
            );
            tick_at(&mut eng, now + ms(350));
            assert!(
                eng.field_at(target, 2).is_some() && eng.field_at(target, 3).is_some(),
                "target {target}: the run was not flowed away as an abandoned line"
            );
        }
    }

    #[test]
    fn an_unrelated_follow_on_the_parked_row_does_not_prove_the_park() {
        let model = aterm_models_effects::rainbow_short_wrap_park_model();
        let held = model.successors("Hold", &model.init_state())[0].clone();
        for (origin, landing, local_run, should_prove) in
            [(4, 3, false, true), (32, 31, true, false)]
        {
            let (mut eng, mut t) = typed_runs(&[3]);
            if local_run {
                eng.observe_caret((3, 31));
                tick_at(&mut eng, t);
                let key = t + Duration::from_millis(10);
                eng.on_event(typed(1), key);
                tick_at(&mut eng, key);
                let echo = key + Duration::from_millis(1);
                eng.on_event(
                    Event::Sweep {
                        row: 3,
                        col0: 31,
                        col1: 32,
                    },
                    key,
                );
                eng.on_event(mv((3, 31), (3, 32), Licence::Typed), echo);
                tick_at(&mut eng, echo);
                t = echo;
                let far = eng
                    .ribbon()
                    .cells()
                    .iter()
                    .find(|cell| cell.row == 3 && cell.col == 2)
                    .expect("the moving run stands at col 2")
                    .cohort;
                let local = eng
                    .ribbon()
                    .cells()
                    .iter()
                    .find(|cell| cell.row == 3 && cell.col == 31)
                    .expect("the parked caret has its own typed run")
                    .cohort;
                assert_ne!(far, local, "premise: only the distant run moves");
            } else {
                eng.observe_caret((3, origin));
                tick_at(&mut eng, t);
            }
            let mut moving_text = vec![' '; 36];
            moving_text[2] = 'a';
            moving_text[3] = 'b';
            let mut text = moving_text.clone();
            if local_run {
                text[31] = 'z';
            }
            let blank = vec![' '; 36];
            let mut remaining = blank.clone();
            if local_run {
                remaining[31] = 'z';
            }
            let before = [
                RowSample {
                    row: 3,
                    cols: &text,
                },
                RowSample {
                    row: 4,
                    cols: &blank,
                },
            ];
            assert_eq!(eng.witness_rows(&before, t), 0);
            let after = [
                RowSample {
                    row: 3,
                    cols: &remaining,
                },
                RowSample {
                    row: 4,
                    cols: &moving_text,
                },
            ];
            let (moved, proved) = eng.follow_rows_with_park(
                &after,
                &[3, 4],
                t + Duration::from_millis(5),
                Some(((3, origin), (3, landing))),
                false,
            );
            assert_eq!(moved, 2, "the unrelated run still follows its text");
            let action = if should_prove {
                "FollowExact"
            } else {
                "FollowOther"
            };
            let projected = model.successors(action, &held)[0].clone();
            assert_eq!(projected.get("content_moved"), Some(&1));
            assert_eq!(
                proved,
                projected.get("exact") == Some(&1),
                "the parked origin {origin} must borrow only its own run"
            );
        }
    }

    #[test]
    fn a_withheld_follow_lifts_the_hand_once() {
        let ms = Duration::from_millis;
        let (mut eng, t) = typed_runs(&[4, 3]);
        assert_eq!(eng.caret, (3, 4), "fixture: the hand on row 3");
        let mut hand = [' '; 36];
        hand[2] = 'a';
        hand[3] = 'b';
        let mut lower = [' '; 36];
        lower[2] = 'c';
        lower[3] = 'd';
        let blank = [' '; 36];
        fn at(row: u16, cols: &[char]) -> RowSample<'_> {
            RowSample { row, cols }
        }
        eng.witness_rows(
            &[at(2, &blank), at(3, &hand), at(4, &lower), at(5, &blank)],
            t,
        );
        let (moved, _) = eng.follow_rows_with_park(
            &[at(2, &blank), at(3, &blank), at(4, &hand), at(5, &lower)],
            &[2, 3, 4, 5],
            t + ms(5),
            None,
            true,
        );
        assert_eq!(moved, 4, "fixture: both lines followed");
        assert_eq!(eng.caret, (4, 4), "the hand rides its own line, once");
        assert_eq!(
            eng.take_hand_lift(),
            Some((3, 4)),
            "the seam is told the hand's own lift"
        );
        assert_eq!(
            eng.ribbon.seam_row(),
            None,
            "the hand's line is no new line"
        );
    }

    #[test]
    fn the_seam_rides_with_the_hands_line() {
        let ms = Duration::from_millis;
        let (mut eng, t) = typed_runs(&[3]);
        eng.ribbon.arm_seam(3, t);
        let mut hand = [' '; 36];
        hand[2] = 'a';
        hand[3] = 'b';
        let blank = [' '; 36];
        fn at(row: u16, cols: &[char]) -> RowSample<'_> {
            RowSample { row, cols }
        }
        eng.witness_rows(&[at(2, &blank), at(3, &hand), at(4, &blank)], t);
        eng.follow_rows_with_park(
            &[at(2, &hand), at(3, &blank), at(4, &blank)],
            &[2, 3, 4],
            t + ms(5),
            None,
            true,
        );
        assert_eq!(eng.caret, (2, 4), "fixture: the hand rode");
        assert_eq!(
            eng.ribbon.seam_row(),
            Some(2),
            "the seam rides with its line"
        );
    }

    #[test]
    fn the_hands_floor_rides_with_its_line() {
        let ms = Duration::from_millis;
        let (mut eng, t) = typed_runs(&[3]);
        assert_eq!(
            eng.ribbon.hand_floor_cell(),
            Some((3, 2)),
            "fixture: the floor at the run's start"
        );
        let mut hand = [' '; 36];
        hand[2] = 'a';
        hand[3] = 'b';
        let blank = [' '; 36];
        fn at(row: u16, cols: &[char]) -> RowSample<'_> {
            RowSample { row, cols }
        }
        eng.witness_rows(&[at(2, &blank), at(3, &hand), at(4, &blank)], t);
        eng.follow_rows_with_park(
            &[at(2, &hand), at(3, &blank), at(4, &blank)],
            &[2, 3, 4],
            t + ms(5),
            None,
            true,
        );
        assert_eq!(eng.caret, (2, 4), "fixture: the hand rode");
        let key = t + ms(20);
        eng.on_event(typed(1), key);
        eng.on_event(
            Event::Sweep {
                row: 2,
                col0: 4,
                col1: 5,
            },
            key,
        );
        eng.on_event(mv((2, 4), (2, 5), Licence::Typed), key + ms(1));
        tick_at(&mut eng, key + ms(1));
        assert_eq!(
            eng.ribbon.hand_floor_cell(),
            Some((2, 2)),
            "the floor rode with the hand's line"
        );
    }

    fn typed_runs(rows: &[u16]) -> (Engine, Instant) {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        eng.on_event(mv((rows[0], 0), (rows[0], 2), Licence::Typed), t0);
        tick_at(&mut eng, t0);
        let mut t = t0;
        for &row in rows {
            if row != rows[0] {
                eng.observe_caret((row, 2));
                tick_at(&mut eng, t);
            }
            for col in 2..4u16 {
                t += ms(12);
                eng.on_event(typed(1), t);
                tick_at(&mut eng, t);
                let echo = t + ms(1);
                eng.on_event(
                    Event::Sweep {
                        row,
                        col0: col,
                        col1: col + 1,
                    },
                    t,
                );
                eng.on_event(mv((row, col), (row, col + 1), Licence::Typed), echo);
                tick_at(&mut eng, echo);
                t = echo;
            }
            assert!(
                eng.field_at(row, 2).is_some(),
                "fixture: a typed ribbon run on row {row}"
            );
        }
        (eng, t)
    }

    #[test]
    fn a_waiting_key_s_arming_rows_have_slots_of_their_own() {
        let (eng, _) = typed_runs(&[3, 7, 11, 15]);
        let mut bands = [0u16; witness::WITNESS_ROWS];
        let b = eng.ribbon_rows_for(None, &mut bands);
        assert_eq!(
            b,
            witness::WITNESS_ROWS,
            "fixture: the bands fill their budget"
        );
        assert_eq!(
            bands[..3],
            [15, 14, 16],
            "the hand's run and its landing rows lead: {bands:?}"
        );
        let mut out = [0u16; witness::WITNESS_ROWS + witness::ARMING_ROWS];
        let n = eng.ribbon_rows_for(Some(19), &mut out);
        assert_eq!(n, witness::WITNESS_ROWS + witness::ARMING_ROWS);
        assert_eq!(out[..b], bands, "the bands' rows are untouched: {out:?}");
        assert_eq!(
            out[b..n],
            [19, 18, 20],
            "the arming row and its neighbours in their own slots: {out:?}"
        );
        assert!(
            !out.contains(&17) && !out.contains(&21),
            "no row two away from the arming row: {out:?}"
        );
        let mut short = [0u16; witness::WITNESS_ROWS];
        assert_eq!(
            eng.ribbon_rows_for(Some(19), &mut short),
            witness::WITNESS_ROWS
        );
        assert_eq!(short, bands, "a short caller keeps every band's row");
        let last = u16::try_from(geom().rows - 1).expect("a small grid");
        let n = eng.ribbon_rows_for(Some(last), &mut out);
        assert_eq!(
            out[b..n],
            [last, last - 1],
            "the rows past the grid are not neighbours: {:?}",
            &out[..n]
        );
    }

    #[test]
    fn withheld_is_what_the_host_was_asked_for_and_did_not_deliver() {
        let (mut eng, t) = typed_runs(&[3, 7, 11, 15]);
        let blank: Vec<char> = "    ".chars().collect();
        let deliver = |rows: &[u16]| {
            rows.iter()
                .map(|&row| RowSample { row, cols: &blank })
                .collect::<Vec<_>>()
        };
        let withheld = |eng: &Engine| eng.withheld[..eng.withheld_n].to_vec();
        let mut asked = [0u16; witness::WITNESS_ROWS + witness::ARMING_ROWS];
        let n = eng.ribbon_rows_for(Some(19), &mut asked);
        let asked = &asked[..n];
        assert_eq!(
            asked[n - 3..],
            [19, 18, 20],
            "fixture: the arming rows are asked for"
        );
        eng.follow_rows(&deliver(asked), asked, t);
        assert_eq!(
            withheld(&eng),
            Vec::<u16>::new(),
            "every row asked for came"
        );
        let far = asked[n - 1];
        let without_far: Vec<u16> = asked.iter().copied().filter(|&r| r != far).collect();
        eng.follow_rows(&deliver(&without_far), asked, t);
        assert_eq!(withheld(&eng), vec![far], "one row asked for did not come");
        eng.set_pane_rows(Some((0, 17)));
        let in_pane: Vec<u16> = asked.iter().copied().filter(|&r| r <= 16).collect();
        eng.follow_rows(&deliver(&in_pane), asked, t);
        assert_eq!(
            withheld(&eng),
            Vec::<u16>::new(),
            "rows outside the pane are not withheld"
        );
        eng.follow_rows(&deliver(&without_far), asked, t);
        assert_eq!(
            withheld(&eng),
            Vec::<u16>::new(),
            "row 20 is outside the pane"
        );
        let inside = asked[0];
        let without_inside: Vec<u16> = in_pane.iter().copied().filter(|&r| r != inside).collect();
        eng.follow_rows(&deliver(&without_inside), asked, t);
        assert_eq!(
            withheld(&eng),
            vec![inside],
            "…and a row inside it still is"
        );
        eng.set_pane_rows(None);
        let past = u16::try_from(geom().rows).expect("a small grid");
        eng.follow_rows(&deliver(&[15]), &[15, past, past + 1], t);
        assert_eq!(
            withheld(&eng),
            Vec::<u16>::new(),
            "rows past the grid are not withheld"
        );
    }

    #[test]
    fn a_licensed_jump_mints_one_meteor_cue_on_the_shared_clock() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(mv((3, 0), (3, 20), Licence::Nav), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);

        assert_eq!(sc.cues.len(), 1, "one gesture on the observed move");
        let cue = sc.cues[0];
        let SoundKind::Meteor { dir, cells, armed } = cue.kind else {
            panic!("a nav jump is a Meteor cue, got {:?}", cue.kind);
        };
        assert_eq!((dir, cells, armed), (1, 20, false));
        assert_eq!(cue.col, 20, "the cue pans to the landing column");
        assert_eq!(cue.dir, 1);
        assert!(!cue.shifted, "an echo has no key behind it");
        let Some(CompanionImpulse::Meteor {
            t_flight, t0: at, ..
        }) = eng.take_companion_impulse()
        else {
            panic!("the spawn frame carries the kitty's meteor impulse");
        };
        assert_eq!(at, t0);
        assert_eq!(
            timing::flight(f32::from(cells)),
            t_flight,
            "the bell's delay and the landing edge must be one number"
        );
        assert_eq!(
            eng.budget().tokens(),
            0.0,
            "a meteor empties the typing bucket on its move edge (§13)"
        );
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(8), geom(), &cfg, &mut fr);
        assert_eq!(sc.cues.len(), 1);
    }

    #[test]
    fn a_return_is_the_cadence_and_a_pty_feed_is_the_cascade() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(mv((5, 60), (6, 0), Licence::Return), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        let caret = fr.caret;
        assert_eq!(sc.cues.len(), 1);
        let SoundKind::Enter { cells } = sc.cues[0].kind else {
            panic!("a keyed Enter is the cadence, got {:?}", sc.cues[0].kind);
        };
        assert_eq!(cells, 60, "the true path of the down-left vector, in cells");
        assert_eq!(sc.cues[0].col, 0);
        let Some(CompanionImpulse::Meteor { t_flight, .. }) = eng.take_companion_impulse() else {
            panic!("a credited Enter carries the kitty's meteor impulse");
        };
        assert_eq!(
            timing::flight(f32::from(cells)),
            t_flight,
            "the cadence's delay and the landing edge are one number on a diagonal path"
        );
        assert_eq!(caret.flare_at, Some(t0), "an Enter is credited: it flares");
        assert_eq!(caret.paint, 1.0, "never dimmed at the destination");

        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(mv((10, 0), (11, 0), Licence::Pty), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        let caret = fr.caret;
        assert_eq!(sc.cues.len(), 1);
        assert_eq!(sc.cues[0].kind, SoundKind::Jump, "the brrrring lane");
        assert!(
            eng.take_companion_impulse().is_none(),
            "program output offers the kitty nothing (§8.2)"
        );
        assert_eq!(caret.flare_at, None, "no credit, no flare");
        assert_eq!(caret.paint, 0.0, "no credit, no paint");
        assert!(
            eng.status().meteors > 0,
            "…but the meteor still flies (D18)"
        );
    }

    #[test]
    fn a_sub_floor_hop_is_one_nav_tick_and_a_no_op_is_silent() {
        let t0 = Instant::now();
        let cfg = config();
        let ms = Duration::from_millis;

        let mut eng = engaged();
        let mut sc = Scratch::default();
        let mut t = t0;
        for k in 0..20u16 {
            t += ms(30);
            eng.on_event(mv((3, k), (3, k + 1), Licence::Typed), t);
            eng.on_event(typed(1), t);
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
        }
        let before = eng.momentum_display();
        let hop = t + ms(30);
        eng.on_event(mv((3, 20), (3, 15), Licence::Nav), hop);
        let mut sc = Scratch::default();
        let mut fr = sc.frame();
        eng.tick(hop, geom(), &cfg, &mut fr);
        let cue = sc
            .cues
            .iter()
            .find(|c| c.kind == SoundKind::Navigation)
            .expect("the hop ticked");
        assert_eq!(
            sc.cues
                .iter()
                .filter(|c| c.kind == SoundKind::Navigation)
                .count(),
            1
        );
        assert_ne!(
            eng.momentum_display(),
            before,
            "the hop frame's update moved the spine, so the two frames differ"
        );
        assert_eq!(
            cue.heat,
            eng.momentum_display(),
            "the tick is priced by the hop frame's own spine"
        );
        assert_eq!(
            cue.hue,
            eng.field_at(3, 15).expect("column 15 is laid"),
            "C2: the tick wears the landing cell's stop"
        );
        assert_eq!((cue.col, cue.dir), (15, -1));

        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(mv((3, 10), (3, 15), Licence::Nav), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert_eq!(sc.cues.len(), 1);
        assert_eq!(sc.cues[0].kind, SoundKind::Navigation);
        assert_eq!((sc.cues[0].col, sc.cues[0].dir), (15, 1));
        assert!(
            eng.status().stars > 0,
            "the mini-fan is on glass with its tick"
        );
        assert_eq!(eng.status().meteors, 0, "…and nothing flew");
        assert!(
            eng.take_companion_impulse().is_none(),
            "a hop is not a kitty event"
        );

        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(mv((3, 10), (3, 9), Licence::Nav), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert_eq!(sc.cues.len(), 1, "an arrow ticks");
        assert_eq!(sc.cues[0].kind, SoundKind::Navigation);
        assert_eq!(sc.cues[0].dir, -1);
        assert_eq!(eng.status().stars, 0, "a one-cell move draws no mini-fan");

        for (from, to, licence) in [
            ((3, 10), (3, 10), Licence::Nav),
            ((3, 10), (3, 15), Licence::Pty),
            ((3, 10), (3, 11), Licence::Typed),
        ] {
            let mut eng = engaged();
            let mut sc = Scratch::default();
            eng.on_event(mv(from, to, licence), t0);
            let mut fr = sc.frame();
            eng.tick(t0, geom(), &cfg, &mut fr);
            assert!(
                sc.cues.is_empty(),
                "{licence:?} {from:?}→{to:?} must be silent"
            );
        }
    }

    #[test]
    fn an_erase_winces_the_kitty_and_a_meteor_outranks_it() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();

        eng.on_event(Event::Erase, t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert!(matches!(fr.companion, Some(CompanionImpulse::Wince)));
        assert!(matches!(
            eng.take_companion_impulse(),
            Some(CompanionImpulse::Wince)
        ));
        assert!(
            eng.take_companion_impulse().is_none(),
            "the take transfers it"
        );

        eng.on_event(
            Event::Kill {
                cells: 6,
                scope: KillScope::Word,
            },
            t0,
        );
        assert!(matches!(
            eng.take_companion_impulse(),
            Some(CompanionImpulse::Wince)
        ));

        eng.on_event(Event::Erase, t0);
        eng.on_event(mv((3, 40), (3, 0), Licence::Nav), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert!(matches!(
            fr.companion,
            Some(CompanionImpulse::Meteor { dir: Dir::Left, .. })
        ));
        eng.on_event(Event::Erase, t0 + Duration::from_millis(1));
        assert!(matches!(
            eng.take_companion_impulse(),
            Some(CompanionImpulse::Meteor { .. })
        ));
    }

    const TICK: Duration = Duration::from_micros(8_333);

    fn drive_ticks(
        eng: &mut Engine,
        sc: &mut Scratch,
        t0: Instant,
        ticks: std::ops::RangeInclusive<u64>,
        sched: &[(u64, Event)],
    ) {
        let cfg = config();
        for i in ticks {
            let now = t0 + TICK * u32::try_from(i).expect("a short script");
            for (_, ev) in sched.iter().filter(|(at, _)| *at == i) {
                eng.on_event(*ev, now);
            }
            let mut fr = sc.frame();
            eng.tick(now, geom(), &cfg, &mut fr);
        }
    }

    fn flow_run(
        eng: &mut Engine,
        sc: &mut Scratch,
        t0: Instant,
        keys: u32,
        period_ms: u64,
    ) -> Vec<(f32, Flow)> {
        let cfg = config();
        let period = Duration::from_millis(period_ms);
        let mut out = Vec::new();
        let mut t = t0;
        for (col, k) in (4u16..).zip(1..=keys) {
            let key = t0 + period * k;
            while t + TICK <= key {
                t += TICK;
                let mut fr = sc.frame();
                eng.tick(t, geom(), &cfg, &mut fr);
            }
            t = key;
            eng.on_event(mv((3, col), (3, col + 1), Licence::Typed), t);
            eng.on_event(typed(1), t);
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
            out.push((
                key.saturating_duration_since(t0 + period).as_secs_f32(),
                eng.status().flow,
            ));
        }
        out
    }

    #[test]
    fn a_run_of_fast_clean_keys_opens_the_theme_and_a_delete_closes_it() {
        let cfg = config();
        for (label, period_ms) in [("12 cps", 83u64), ("8 cps", 125)] {
            let t0 = Instant::now();
            let mut eng = engaged();
            let mut sc = Scratch::default();
            let rows = flow_run(&mut eng, &mut sc, t0, 44, period_ms);

            assert!(
                rows.iter().any(|(_, f)| f.combo == spine::FLOW_ENTRY_KEYS),
                "{label}: a clean run at speed must reach the entry"
            );

            let mut last = 0.0f32;
            for (_, f) in &rows {
                assert!(
                    f.heat >= last - 1e-6 || f.combo == 0,
                    "the heat fell inside an unbroken run"
                );
                last = f.heat;
                if f.combo <= spine::FLOW_ENTRY_KEYS - spine::FLOW_OPEN_EASE_KEYS {
                    assert_eq!(
                        f.heat, 0.0,
                        "{label}: combo {} must be EXACTLY identity",
                        f.combo
                    );
                } else if f.combo >= spine::FLOW_ENTRY_KEYS {
                    assert_eq!(f.heat, 1.0, "{label}: combo {} must be open", f.combo);
                } else {
                    assert!(
                        f.heat > 0.0 && f.heat < 1.0,
                        "{label}: combo {} must be easing, got {}",
                        f.combo,
                        f.heat
                    );
                }
                assert!(f.best >= f.combo);
            }
            let held = eng.status().flow;
            assert!(held.open(), "{label}: the run must still be open");

            let t = t0 + Duration::from_millis(period_ms) * 45;
            eng.on_event(Event::Erase, t);
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
            let after = eng.status().flow;
            assert_eq!(after.combo, 0, "{label}: a delete must break the run");
            assert_eq!(after.heat, 0.0, "{label}: …and close the theme");
            assert_eq!(
                after.best, held.best,
                "{label}: a broken run does not un-earn the run already held"
            );

            let mut t2 = t;
            for k in 1..=2u32 {
                t2 = t + Duration::from_millis(period_ms) * k;
                eng.on_event(typed(1), t2);
                let mut fr = sc.frame();
                eng.tick(t2, geom(), &cfg, &mut fr);
            }
            assert!(eng.status().flow.combo > 0, "{label}: the run restarted");
            t2 += Duration::from_millis(period_ms);
            eng.on_event(
                Event::Kill {
                    cells: 4,
                    scope: KillScope::Word,
                },
                t2,
            );
            let mut fr = sc.frame();
            eng.tick(t2, geom(), &cfg, &mut fr);
            assert_eq!(
                eng.status().flow.combo,
                0,
                "{label}: a kill must break the run"
            );

            let cold = t2 + Duration::from_secs(30);
            eng.on_event(typed(1), cold);
            let mut fr = sc.frame();
            eng.tick(cold, geom(), &cfg, &mut fr);
            assert_eq!(
                eng.status().flow.combo,
                0,
                "{label}: a key born under the floor may not count"
            );
        }
    }

    #[test]
    fn flow_draws_nothing_of_its_own() {
        let cfg = config();
        let t0 = Instant::now();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let rows = flow_run(&mut eng, &mut sc, t0, 44, 83);
        assert!(
            rows.last().expect("keys").1.open(),
            "the fixture must reach flow first"
        );

        let last_key = t0 + Duration::from_millis(83) * 44;
        let mut dark_from = None;
        let mut t = last_key;
        while t < last_key + Duration::from_secs(5) {
            t += TICK;
            sc.under.clear();
            sc.out.clear();
            sc.halos.clear();
            sc.cues.clear();
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
            let fp = fr.fp;
            let empty = sc.under.is_empty()
                && sc.out.is_empty()
                && sc.halos.is_empty()
                && sc.cues.is_empty();
            match dark_from {
                None if fp == 0 && empty => dark_from = Some(t),
                None => {}
                Some(from) => {
                    assert_eq!(
                        fp,
                        0,
                        "an OPEN theme lit a frame with no keystroke behind it, {:.3} s after the glass went dark",
                        t.saturating_duration_since(from).as_secs_f32()
                    );
                    assert!(empty, "an OPEN theme wrote a quad with no key behind it");
                    assert!(
                        !eng.needs_frame_cadence(),
                        "an OPEN theme held the host awake"
                    );
                }
            }
        }
        assert!(
            dark_from.is_some(),
            "the glass must go dark inside five seconds"
        );
        let held = eng.status().flow;
        assert!(
            held.open() && held.heat == 1.0,
            "the run is unbroken through the silence — that is what an agent holds its turn on: {held:?}"
        );
        assert!(
            eng.next_change_deadline(t).is_none(),
            "an OPEN theme names a deadline over dark glass"
        );
    }

    fn exit_sample(eng: &mut Engine, now: Instant) -> (i64, i64) {
        let mut sc = Scratch::default();
        let mut fr = sc.frame();
        eng.tick(now, geom(), &config(), &mut fr);
        let fp = fr.fp;
        let visible = fp != 0
            || !sc.under.is_empty()
            || !sc.out.is_empty()
            || !sc.halos.is_empty()
            || !sc.beams.is_empty();
        let active = eng.needs_frame_cadence() || eng.next_change_deadline(now).is_some();
        (i64::from(visible), i64::from(active))
    }

    type ExitFault = fn(&mut Engine, Instant);

    type TypingShape = fn(&mut Engine, Instant) -> Instant;

    fn no_exit_fault(_: &mut Engine, _: Instant) {}

    fn callback_relative_swoosh(eng: &mut Engine, late: Instant) {
        eng.ribbon.restart_unobserved_swooshes(late);
    }

    fn exit_sampling_walk(
        model: &aterm_spec::derive::Model,
        fault: ExitFault,
    ) -> Result<usize, String> {
        let shapes: [(&str, TypingShape); 3] = [
            ("one key", |eng, t0| {
                blank_row(eng, 2);
                eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
                tick_at(eng, t0);
                type_key_at(eng, t0 + Duration::from_millis(100), 2)
            }),
            ("a slow word", three_typed_cells),
            ("a flow run", |eng, t0| {
                flow_run(eng, &mut Scratch::default(), t0, 44, 83);
                t0 + Duration::from_millis(83) * 44
            }),
        ];
        let mut observed = 0usize;
        for (shape, type_it) in shapes {
            for silence in [5u64, 30, 600] {
                let at =
                    |state: &aterm_spec::interp::State| format!("{shape}, {silence} s: {state:?}");
                let mut eng = engaged();
                let last = type_it(&mut eng, Instant::now());
                let (visible, active) = exit_sample(&mut eng, last);
                let lit = std::collections::BTreeMap::from([
                    ("logical_done", 0),
                    ("sampled", 0),
                    ("visible", visible),
                    ("active", active),
                ]);
                if lit != model.init_state() {
                    return Err(format!(
                        "the last key does not leave a live ribbon: {}",
                        at(&lit)
                    ));
                }
                let mut elapsed = lit.clone();
                elapsed.insert("logical_done", 1);
                if model.successors("ElapseDone", &lit) != vec![elapsed.clone()] {
                    return Err(format!(
                        "the silence is not the model's ElapseDone: {}",
                        at(&lit)
                    ));
                }
                let late = last + Duration::from_secs(silence);
                fault(&mut eng, late);
                let (visible, active) = exit_sample(&mut eng, late);
                let mut sampled = elapsed.clone();
                sampled.insert("sampled", 1);
                sampled.insert("visible", visible);
                sampled.insert("active", active);
                let expected = model.successors("ObserveDone", &elapsed);
                if expected != vec![sampled.clone()] {
                    return Err(format!(
                        "real ObserveDone took {} to {sampled:?}, the model's ObserveDone to \
                         {expected:?}",
                        at(&elapsed)
                    ));
                }
                observed += 1;
            }
        }
        Ok(observed)
    }

    #[test]
    fn a_late_tick_samples_the_settled_exit_swoosh() {
        let model = aterm_models_effects::rainbow_exit_sampling_model();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let observed =
            exit_sampling_walk(&model, no_exit_fault).unwrap_or_else(|why| panic!("{why}"));
        assert_eq!(observed, 9, "three typing shapes, three silences each");
        let refused = exit_sampling_walk(&model, callback_relative_swoosh)
            .expect_err("the healthy model must refuse a callback-relative swoosh");
        assert!(refused.contains("real ObserveDone"), "{refused}");
        exit_sampling_walk(&buggy, callback_relative_swoosh).unwrap_or_else(|why| {
            panic!("the buggy model must describe the callback-relative swoosh: {why}")
        });
        exit_sampling_walk(&buggy, no_exit_fault)
            .expect_err("the shipped engine must not conform to the callback-relative model");
    }

    fn live_cell(eng: &Engine, row: u16, col: u16) -> Option<ribbon::Cell> {
        eng.ribbon
            .cells()
            .iter()
            .find(|c| c.row == row && c.col == col && c.retract_at.is_none())
            .copied()
    }

    fn typo_script(
        erase_after: u64,
        deletes: u16,
        fix_after: u64,
    ) -> (Vec<(u64, Event)>, u64, u64) {
        let mut out = Vec::new();
        let mut col = 4u16;
        for k in 0..8u64 {
            out.push((15 * k, typed(1)));
            out.push((15 * k, mv((3, col), (3, col + 1), Licence::Typed)));
            col += 1;
        }
        let erase = 15 * 7 + erase_after;
        let mut last_erase = erase;
        for d in 0..deletes {
            last_erase = erase + 15 * u64::from(d);
            out.push((last_erase, Event::Erase));
            out.push((last_erase, mv((3, col), (3, col - 1), Licence::Typed)));
            col -= 1;
        }
        let fix = last_erase + fix_after;
        out.push((fix, typed(1)));
        out.push((fix, mv((3, col), (3, col + 1), Licence::Typed)));
        (out, last_erase, fix)
    }

    #[test]
    fn a_typo_fixed_within_a_breath_is_born_at_the_momentum_it_interrupted() {
        for (label, erase_after, deletes, fix_after) in [
            ("one-slot fix", 15u64, 1u16, 15u64),
            ("500 ms fix", 15, 1, 60),
            ("250 ms pause, two deletes", 30, 2, 15),
        ] {
            let t0 = Instant::now();
            let mut eng = engaged();
            let mut sc = Scratch::default();
            let (sched, erase, fix) = typo_script(erase_after, deletes, fix_after);
            let first_erase = erase - 15 * (u64::from(deletes) - 1);
            drive_ticks(&mut eng, &mut sc, t0, 0..=first_erase - 1, &sched);
            let interrupted = eng.spine().birth_disp();
            drive_ticks(&mut eng, &mut sc, t0, first_erase..=erase, &sched);
            let fix_col = 12 - deletes;
            assert!(
                live_cell(&eng, 3, fix_col).is_none(),
                "{label}: the erased cell is retracting on the erase frame (T5)"
            );
            let retracting = eng
                .ribbon
                .cells()
                .iter()
                .find(|c| c.row == 3 && c.col == fix_col)
                .copied()
                .expect("the erased cell is still on glass, retracting");
            assert_eq!(
                retracting.retract_at,
                Some(t0 + TICK * u32::try_from(erase).expect("short")),
                "{label}: the erased cell's retract starts on the erase edge, as before"
            );
            assert_eq!(
                eng.erased,
                (3, fix_col),
                "{label}: the erased cell is known"
            );

            drive_ticks(&mut eng, &mut sc, t0, erase + 1..=fix, &sched);
            let fixed =
                live_cell(&eng, 3, fix_col).expect("the fix key owns its light on its own frame");
            let left = live_cell(&eng, 3, fix_col - 1).expect("the neighbour is still laid");
            assert_eq!(
                fixed.born,
                t0 + TICK * u32::try_from(fix).expect("short"),
                "{label}: frame-0 — the fix cell is born on the fix key's frame"
            );
            assert!(
                fixed.birth_disp >= interrupted - 1e-4,
                "{label}: born at {:.4}, the delete interrupted {interrupted:.4} — a {:.4} dip of the birth spine ({:.1} %); live birth_disp {:.4}",
                fixed.birth_disp,
                interrupted - fixed.birth_disp,
                (interrupted - fixed.birth_disp) / interrupted * 100.0,
                eng.spine().birth_disp()
            );
            assert!(
                fixed.cov0 >= left.cov0 && fixed.life_s >= left.life_s,
                "{label}: the fix ({:.4}, {:.3} s) is not below its left neighbour ({:.4}, {:.3} s)",
                fixed.cov0,
                fixed.life_s,
                left.cov0,
                left.life_s
            );
            assert!(
                eng.mend.is_none() && eng.spine().erase_mark().is_none(),
                "{label}: the fix spends the mark"
            );
        }
    }

    #[test]
    fn a_slow_correction_is_not_a_mend() {
        for (label, deletes, fix_after) in [("2 s later", 1u16, 240u64), ("three deletes", 3, 15)] {
            let t0 = Instant::now();
            let mut eng = engaged();
            let mut sc = Scratch::default();
            let (sched, erase, fix) = typo_script(15, deletes, fix_after);
            let first_erase = erase - 15 * (u64::from(deletes) - 1);
            drive_ticks(&mut eng, &mut sc, t0, 0..=first_erase - 1, &sched);
            let interrupted = eng.spine().birth_disp();
            drive_ticks(&mut eng, &mut sc, t0, first_erase..=fix - 1, &sched);
            let t_fix = t0 + TICK * u32::try_from(fix).expect("short");
            let mark = eng
                .spine()
                .erase_mark()
                .expect("the mark is a value, still there");
            assert_eq!(mark.count, u8::try_from(deletes).expect("small"));
            assert_eq!(
                eng.spine().mend(t_fix),
                None,
                "{label}: the key that comes now is not a fix"
            );
            drive_ticks(&mut eng, &mut sc, t0, fix..=fix, &sched);
            let fix_col = 12 - deletes;
            let fixed = live_cell(&eng, 3, fix_col).expect("the key still lays its cell");
            let live = eng.spine().birth_disp();
            assert!(
                (fixed.birth_disp - live).abs() < 1e-6,
                "{label}: priced live — born at {:.4}, live birth_disp {live:.4}",
                fixed.birth_disp
            );
            assert!(
                fixed.birth_disp < interrupted,
                "{label}: and the live price ({:.4}) is below the momentum interrupted ({interrupted:.4}) — the honest re-earn",
                fixed.birth_disp
            );
            assert!(
                eng.spine().erase_mark().is_none(),
                "{label}: the key closes the run"
            );
        }
    }

    #[test]
    fn the_mend_mark_is_a_value_not_a_timer() {
        let t0 = Instant::now();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let sched = [
            (0, typed(1)),
            (0, mv((3, 4), (3, 5), Licence::Typed)),
            (15, Event::Erase),
            (15, mv((3, 5), (3, 4), Licence::Typed)),
        ];
        let t_erase = t0 + TICK * 15;
        let window = Duration::from_secs_f32(spine::MEND_WINDOW_S);
        drive_ticks(&mut eng, &mut sc, t0, 0..=15, &sched);
        let mark = eng
            .spine()
            .erase_mark()
            .expect("the Backspace leaves its mark");

        let mut i = 15u64;
        let dark = loop {
            i += 1;
            let now = t0 + TICK * u32::try_from(i).expect("short");
            drive_ticks(&mut eng, &mut sc, t0, i..=i, &sched);
            if eng.fingerprint() == 0 && eng.next_change_deadline(now).is_none() {
                break i;
            }
            assert!(
                now < t_erase + window,
                "the precondition: a lone erased key must go dark inside the mend window"
            );
        };
        let t_dark = t0 + TICK * u32::try_from(dark).expect("short");
        assert_eq!(
            eng.spine().mend(t_dark),
            Some(mark),
            "the mend is still live over the dark glass"
        );
        assert!(!eng.needs_frame_cadence(), "…and asks for no cadence");
        assert_eq!(
            eng.next_change_deadline(t_dark),
            None,
            "…and names no deadline"
        );

        let mut mended = eng.clone();
        let mut sc2 = Scratch::default();
        let fix = dark + 1;
        let fix_sched = [(fix, typed(1)), (fix, mv((3, 4), (3, 5), Licence::Typed))];
        drive_ticks(&mut mended, &mut sc2, t0, fix..=fix, &fix_sched);
        let cell = live_cell(&mended, 3, 4).expect("the key lays its cell");
        let live = mended
            .spine()
            .disp()
            .max(spine::DISP_PEAK_FLOOR_SHARE * mended.spine().disp_peak());
        assert!(
            (cell.birth_disp - mark.disp).abs() < 1e-6 && mark.disp > live + 0.01,
            "born at {:.4}: the mark {:.4}, over a live price of {live:.4}",
            cell.birth_disp,
            mark.disp
        );

        let past = 15 + (window.as_micros() as u64 + 100_000).div_ceil(TICK.as_micros() as u64);
        for j in dark + 1..=past {
            let now = t0 + TICK * u32::try_from(j).expect("short");
            drive_ticks(&mut eng, &mut sc, t0, j..=j, &sched);
            assert_eq!(eng.fingerprint(), 0, "idle → zero, mark or no mark");
            assert!(!eng.needs_frame_cadence());
            assert_eq!(
                eng.next_change_deadline(now),
                None,
                "a pending mark names no deadline (tick {j})"
            );
        }
        let t_past = t0 + TICK * u32::try_from(past).expect("short");
        assert_eq!(
            eng.spine().erase_mark(),
            Some(mark),
            "the mark is a value: still there, unchanged, after the window"
        );
        assert_eq!(eng.spine().mend(t_past), None, "…and not a mend any more");
    }

    #[test]
    fn reduced_motion_lands_the_kitty_without_a_flight_or_a_flash() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(Event::ReducedMotion(true), t0);
        eng.on_event(mv((3, 0), (3, 30), Licence::Nav), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert!(matches!(fr.companion, Some(CompanionImpulse::Land)));
        assert_eq!(fr.caret.flare_at, None, "no flash under reduced motion");
        assert_eq!(fr.caret.paint, 1.0, "…but the caret is still not dimmed");
        assert_eq!(sc.cues.len(), 1);
        assert!(matches!(sc.cues[0].kind, SoundKind::Meteor { .. }));
    }

    #[test]
    fn the_caret_flare_is_one_edge_that_ages_out() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(mv((3, 0), (3, 30), Licence::Nav), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert_eq!(fr.caret.flare_at, Some(t0));
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(100), geom(), &cfg, &mut fr);
        assert_eq!(fr.caret.flare_at, Some(t0), "live through the relax");
        let settled =
            t0 + Duration::from_secs_f32(SPRING_SNAP_RESPONSE_S) + Duration::from_millis(1);
        let mut fr = sc.frame();
        eng.tick(settled, geom(), &cfg, &mut fr);
        assert_eq!(
            fr.caret.flare_at, None,
            "cleared once spring-snap has settled"
        );
    }

    #[test]
    fn the_caret_paint_is_the_display_spine_floored_by_the_landing_relight() {
        let t0 = Instant::now();
        let cfg = config();
        let ms = Duration::from_millis;

        let mut eng = engaged();
        let mut sc = Scratch::default();
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert_eq!(fr.caret.paint, 0.0, "idle: nothing lit, nothing landed");

        let mut t = t0;
        let mut while_typing = 0.0;
        for _ in 0..30 {
            t += ms(30);
            eng.on_event(typed(1), t);
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
            while_typing = fr.caret.paint;
        }
        let typing = eng.momentum_display();
        assert!(typing > 0.3, "sustained typing lit the spine: {typing}");
        assert_eq!(while_typing, typing, "while typing, paint is the spine");

        let quiet = t + ms(220);
        let mut fr = sc.frame();
        eng.tick(quiet, geom(), &cfg, &mut fr);
        let disp = eng.momentum_display();
        assert!(disp < typing, "the release is under way");
        assert_eq!(fr.caret.paint, disp, "paint is the spine, to the bit");
        assert_eq!(eng.caret_paint(quiet), disp, "the pure read agrees");
        assert!(
            disp >= 0.7 * typing,
            "220 ms into a τ 0.85 s release keeps ≥ 70 %: {disp} of {typing}"
        );

        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(typed(1), t0);
        let mut fr = sc.frame();
        eng.tick(t0 + ms(16), geom(), &cfg, &mut fr);
        assert_eq!(fr.caret.paint, eng.momentum_display());
        assert!(fr.caret.paint < 0.5, "one key is not a landing");

        let mut eng = engaged();
        let mut sc = Scratch::default();
        let land = t0 + ms(1);
        eng.on_event(mv((3, 1), (3, 40), Licence::Nav), land);
        let mut fr = sc.frame();
        eng.tick(land, geom(), &cfg, &mut fr);
        assert_eq!(eng.momentum_display(), 0.0, "a jump builds no momentum");
        assert_eq!(fr.caret.paint, 1.0, "never dimmed at the destination");

        let hold = Duration::from_secs_f32(SPRING_SNAP_RESPONSE_S);
        let mut fr = sc.frame();
        eng.tick(land + hold - ms(1), geom(), &cfg, &mut fr);
        assert_eq!(fr.caret.paint, 1.0, "held through the flare's relax");

        let mut fr = sc.frame();
        eng.tick(land + hold + hold, geom(), &cfg, &mut fr);
        assert!(
            (fr.caret.paint - 0.5).abs() < 1e-4,
            "half one constant after the hold: {}",
            fr.caret.paint
        );
        assert!(
            fr.caret.paint >= eng.momentum_display(),
            "the re-light is a floor under the spine, never a ceiling"
        );

        let dead = land + Duration::from_secs_f32(RELIGHT_LIFE_S) + ms(1);
        let mut fr = sc.frame();
        eng.tick(dead, geom(), &cfg, &mut fr);
        assert_eq!(fr.caret.paint, 0.0, "back to the bare spine at its life");
        assert_eq!(eng.caret_paint(dead), 0.0);
    }

    #[test]
    fn delight_earns_one_hero_that_chimes_only_with_a_token() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let ms = Duration::from_millis;

        let rows = [3u16, 6, 9, 12];
        for (k, &row) in rows.iter().enumerate() {
            let at = t0 + ms(k as u64);
            blank_row(&mut eng, row - 1);
            eng.earn_hero(at, row, 5);
            let mut fr = sc.frame();
            eng.tick(at, geom(), &cfg, &mut fr);
            assert!(
                matches!(fr.companion, Some(CompanionImpulse::Delight)),
                "the frame carries the pose"
            );
            eng.take_companion_impulse();
            assert_eq!(
                eng.status().stars as usize,
                k + 1,
                "an earned hero is always born (delight {k})"
            );
        }
        assert_eq!(
            sc.cues.len(),
            3,
            "three tokens, three glints, the fourth silent"
        );
        for (cue, _) in sc.cues.iter().zip(rows) {
            assert!(matches!(cue.kind, SoundKind::Stardust { .. }));
            assert_eq!(cue.col, 5, "the glint sits at its star's column");
        }
    }

    #[test]
    fn a_reported_bar_mints_its_fan_and_a_withdrawn_one_mints_nothing() {
        let t0 = Instant::now();
        let cfg = config();
        let mut sc = Scratch::default();

        let mut eng = engaged();
        for row in 0..26u16 {
            blank_row(&mut eng, row);
        }
        eng.on_event(mv((3, 5), (3, 6), Licence::Typed), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        let before = eng.status().stars;
        eng.party(
            t0 + Duration::from_millis(1),
            crate::kitty_sing::BAR_FAN_CAP,
            true,
        );
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(2), geom(), &cfg, &mut fr);
        let after = eng.status().stars;
        assert!(
            after > before,
            "the host's bar minted NOTHING: stars {before} -> {after}"
        );

        let mut eng = engaged();
        for row in 0..26u16 {
            blank_row(&mut eng, row);
        }
        eng.on_event(mv((3, 5), (3, 6), Licence::Typed), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        eng.party(
            t0 + Duration::from_millis(1),
            crate::kitty_sing::BAR_FAN_CAP,
            true,
        );
        eng.reset();
        for row in 0..26u16 {
            blank_row(&mut eng, row);
        }
        eng.on_event(
            mv((9, 40), (9, 41), Licence::Typed),
            t0 + Duration::from_millis(2),
        );
        let before = eng.status().stars;
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(3), geom(), &cfg, &mut fr);
        assert_eq!(
            eng.status().stars,
            before,
            "a bar banked before a space cut fanned into the new space"
        );

        let mut eng = engaged();
        for row in 0..26u16 {
            blank_row(&mut eng, row);
        }
        eng.party(t0, crate::kitty_sing::BAR_FAN_CAP, true);
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(1), geom(), &cfg, &mut fr);
        assert_eq!(
            eng.status().stars,
            0,
            "a bar laid before the caret was ever observed"
        );
    }

    #[test]
    fn an_unprobed_sky_bears_no_star() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        assert!(eng.probe().is_empty());
        eng.earn_hero(t0, 3, 5);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert_eq!(eng.status().stars, 0, "unknown → no star");
        assert!(sc.cues.is_empty(), "…and no glint for a star not born");

        blank_row(&mut eng, 2);
        eng.earn_hero(t0 + Duration::from_millis(1), 3, 5);
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(1), geom(), &cfg, &mut fr);
        assert_eq!(eng.status().stars, 1, "probed blank → born");
    }

    #[test]
    fn a_band_move_moves_the_caret_with_its_band_and_keeps_the_spine() {
        let t0 = Instant::now();
        let cfg = config();
        let g = geom();
        let ch = g.ch as u16;
        let ms = Duration::from_millis;
        let lay = |eng: &mut Engine| {
            for row in 2..=4 {
                blank_row(eng, row);
            }
            let mut sc = Scratch::default();
            for k in 0..6u16 {
                let at = t0 + ms(80 * u64::from(k));
                eng.on_event(mv((3, k), (3, k + 1), Licence::Typed), at);
                eng.on_event(typed(1), at);
                let mut fr = sc.frame();
                eng.tick(at, g, &cfg, &mut fr);
            }
        };
        let mut eng = engaged();
        lay(&mut eng);
        let t_last = t0 + ms(400);
        let disp_before = eng.status().disp;
        assert!(disp_before > 0.0, "fixture: the hand earned momentum");
        let field_before = eng.field_at(3, 5).expect("fixture: the cell is laid");
        assert_eq!(eng.caret, (3, 6));
        eng.erased = (3, 2);
        eng.earned.push(((3, 4), t_last));
        eng.earned.push(((10, 4), t_last));
        eng.earned.push(((20, 4), t_last));

        eng.translate_band(2, 10, 1, ch, 0);

        assert_eq!(eng.caret, (4, 6), "the caret rides its band");
        assert_eq!(eng.erased, (4, 2), "so does the erased cell");
        assert_eq!(
            eng.earned.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
            vec![(4, 4), (20, 4)],
            "a hero owed inside the band moves, one outside it stays, one pushed past the edge is owed nowhere"
        );
        assert_eq!(
            eng.field_at(4, 5),
            Some(field_before),
            "the field moved with its cell"
        );
        assert_eq!(eng.field_at(3, 5), None, "the vacated cell answers nothing");
        assert_eq!(eng.status().disp, disp_before, "a transform ticks nothing");
        let t1 = t_last + ms(8);
        let mut sc = Scratch::default();
        let mut fr = sc.frame();
        eng.tick(t1, g, &cfg, &mut fr);
        let disp_after = eng.status().disp;
        assert!(
            disp_after > 0.0 && (disp_after - disp_before).abs() < 0.05,
            "the spine kept going across the band move: {disp_before} → {disp_after}"
        );
        assert!(
            eng.ribbon_segments() >= 1,
            "the ribbon is lit on the moved row"
        );

        let mut reset = engaged();
        lay(&mut reset);
        reset.reset();
        let mut sc = Scratch::default();
        let mut fr = sc.frame();
        reset.tick(t1, g, &cfg, &mut fr);
        assert_eq!(
            reset.status().disp,
            0.0,
            "the reset is what the owner saw: 0.99 → 0"
        );

        eng.translate_band(20, 30, 1, ch, 0);
        assert_eq!(eng.caret, (4, 6));
        assert_eq!(eng.field_at(4, 5), Some(field_before));
        eng.translate_band(2, 4, 1, ch, 0);
        assert_eq!(
            eng.caret,
            (4, 6),
            "a position saturates; it cannot be dropped"
        );
        assert_eq!(eng.field_at(4, 5), None);
        assert_eq!(eng.field_at(5, 5), None, "nothing is parked past the edge");
        let mut idle = Engine::new();
        idle.caret = (3, 3);
        idle.translate_band(2, 10, 1, ch, 0);
        assert_eq!(idle.caret, (3, 3));
        eng.translate_band(5, 4, 1, ch, 0);
        eng.translate_band(2, 10, 0, ch, 0);
        assert_eq!(eng.caret, (4, 6));
    }

    #[test]
    fn a_scroll_moves_every_live_star_and_the_caret_with_the_viewport() {
        for dark_theme in [true, false] {
            scroll_moves_every_mark(&Config {
                dark_theme,
                ..config()
            });
        }
    }

    fn scroll_moves_every_mark(cfg: &Config) {
        let theme = if cfg.dark_theme { "dark" } else { "light" };
        let t0 = Instant::now();
        let g = geom();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let ms = Duration::from_millis;

        blank_row(&mut eng, 2);
        eng.earn_hero(t0, 3, 5);
        eng.on_event(mv((3, 0), (3, 6), Licence::Typed), t0);
        eng.on_event(typed(6), t0);
        let mut fr = sc.frame();
        eng.tick(t0, g, cfg, &mut fr);
        let t0 = t0 + ms(1);
        eng.on_event(mv((3, 6), (3, 5), Licence::Nav), t0);
        let mut sc = Scratch::default();
        let mut fr = sc.frame();
        eng.tick(t0, g, cfg, &mut fr);
        let live = eng.status().stars;
        assert!(live >= 1, "{theme}: the earned hero is on glass");
        let quads: Vec<(u16, u16)> = sc.out.iter().map(|q| (q.row, q.y)).collect();
        let halos: Vec<(u16, u16, u16)> = sc.halos.iter().map(|a| (a.row, a.y, a.cy)).collect();
        if cfg.dark_theme {
            assert!(
                !quads.is_empty(),
                "{theme}: the star and the hot edge wrote additive quads"
            );
        } else {
            assert!(!halos.is_empty(), "{theme}: the ink fork wrote its halos");
        }
        let t_caret = eng.field();
        assert_eq!(
            eng.field_at(3, 5),
            Some(t_caret),
            "the caret sits on a laid cell, so field() is that cell's t"
        );
        assert_ne!(
            eng.field_at(3, 4),
            eng.field_at(3, 5),
            "the two laid cells differ, so a stale caret would read the other"
        );

        eng.translate_scroll(1, g.ch as u16);
        let mut sc2 = Scratch::default();
        let mut fr = sc2.frame();
        eng.tick(t0, g, cfg, &mut fr);
        assert_eq!(
            eng.status().stars,
            live,
            "{theme}: one row up keeps every star"
        );
        let ch = g.ch as u32;
        let quads2: Vec<(u16, u16)> = sc2.out.iter().map(|q| (q.row, q.y)).collect();
        let halos2: Vec<(u16, u16, u16)> = sc2.halos.iter().map(|a| (a.row, a.y, a.cy)).collect();
        assert_eq!(quads.len(), quads2.len(), "{theme}: every quad survived");
        assert_eq!(halos.len(), halos2.len(), "{theme}: every halo survived");
        for (b, a) in quads.iter().zip(&quads2) {
            assert_eq!(a.0 + 1, b.0, "{theme}: every quad moved up one row");
            assert_eq!(
                u32::from(a.1) + ch,
                u32::from(b.1),
                "{theme}: …by exactly ch px"
            );
        }
        for (b, a) in halos.iter().zip(&halos2) {
            assert_eq!(a.0 + 1, b.0, "{theme}: every halo moved up one row");
            assert_eq!(
                u32::from(a.1) + ch,
                u32::from(b.1),
                "{theme}: …its band by exactly ch px"
            );
            assert_eq!(
                u32::from(a.2) + ch,
                u32::from(b.2),
                "{theme}: …and its centre with it"
            );
        }
        assert_eq!(
            eng.field(),
            t_caret,
            "{theme}: the caret moved with its cell: field() still reads the same t"
        );
        assert_eq!(eng.field_at(2, 5), Some(t_caret));

        eng.translate_scroll(g.rows as u16, g.ch as u16);
        let mut fr = sc2.frame();
        eng.tick(t0 + ms(2), g, cfg, &mut fr);
        assert_eq!(
            eng.status().stars,
            0,
            "{theme}: a star that left the grid is dropped"
        );
        assert_eq!(eng.status().cells, 0, "{theme}: so is the ribbon");
    }

    #[test]
    fn a_hide_is_taken_by_one_frame_and_an_un_taken_one_is_dropped_by_a_reset_or_a_curtain() {
        let cfg = config();
        let ms = |base: Instant, ms: u64| base + Duration::from_millis(ms);
        let drawn = |eng: &mut Engine, at: Instant| -> usize {
            let mut sc = Scratch::default();
            let mut f = sc.frame();
            eng.tick(at, geom(), &cfg, &mut f);
            f.under.len()
        };

        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        assert!(drawn(&mut eng, ms(t, 8)) > 0, "fixture: a band is drawn");
        eng.hide_next_frame();
        assert!(eng.hide_armed());
        let cells = eng.status().cells;
        assert_eq!(
            drawn(&mut eng, ms(t, 16)),
            0,
            "the hidden frame writes nothing"
        );
        assert_eq!(
            (eng.status().quads, eng.fingerprint(), eng.status().cells),
            (0, 0, cells),
            "…reports nothing written, and keeps every cell"
        );
        assert!(!eng.hide_armed(), "…and the tick took the arm");
        assert!(drawn(&mut eng, ms(t, 24)) > 0, "the next frame draws again");

        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        assert!(drawn(&mut eng, ms(t, 8)) > 0);
        eng.hide_next_frame();
        eng.reset();
        assert!(!eng.hide_armed(), "a reset drops an un-taken hide");

        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        assert!(drawn(&mut eng, ms(t, 8)) > 0);
        eng.hide_next_frame();
        eng.curtain(ms(t, 12));
        assert!(!eng.hide_armed(), "a curtain drops an un-taken hide");
        assert!(
            !eng.caret_known,
            "a curtain forgets the caret: it is learned from the next observed move"
        );
        assert!(
            drawn(&mut eng, ms(t, 20)) > 0,
            "the curtain's first frame was eaten by a hide armed before it"
        );
    }

    #[test]
    fn a_falling_curtain_is_reported_drawn_on_every_frame_it_composites() {
        let cfg = config();
        let ms = |base: Instant, ms: u64| base + Duration::from_millis(ms);
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        let quads = |eng: &mut Engine, at: Instant| -> (usize, bool) {
            let mut sc = Scratch::default();
            let mut f = sc.frame();
            eng.tick(at, geom(), &cfg, &mut f);
            let ink = f.under.iter().any(|q| {
                ribbon::reads_as_ink(aterm_render::over_premul(cfg.theme_bg, q.color, q.alpha))
            });
            (f.under.len(), ink)
        };
        assert!(quads(&mut eng, ms(t, 8)).1);
        let fell = ms(t, 16);
        eng.curtain(fell);
        let mut lied = Vec::new();
        let mut honest = 0;
        let mut covered = 0;
        for at in (0..=240u64).step_by(8) {
            let (n, ink) = quads(&mut eng, ms(fell, at));
            let drawn = eng.ribbon_drawn();
            let left = eng.curtain_left_ms(ms(fell, at));
            if n > 0 {
                assert!(
                    left.is_some(),
                    "+{at} ms: {n} quads composited and the row says no curtain is falling"
                );
            }
            if ink {
                if drawn == 0 {
                    lied.push((at, n));
                } else {
                    honest += 1;
                }
                if eng.ribbon_segments() == 0 {
                    covered += 1;
                    assert!(
                        left.is_some_and(|ms| ms > 0),
                        "+{at} ms: the claim is at zero over a visible band and the row does \
                         not say a curtain is why"
                    );
                }
            }
        }
        assert!(
            honest >= 10,
            "the sweep never saw a visible curtain frame ({honest})"
        );
        assert!(
            lied.is_empty(),
            "the row reported `ribbon_drawn=0` on frames whose own pixels read as band ink: \
             {lied:?}"
        );
        assert!(
            covered >= 2,
            "the sweep never reached the window the defect lives in: \
             {covered} frames with the claim at zero over a visible band"
        );
    }

    #[test]
    fn a_style_switch_drops_an_unticked_event_and_never_a_minted_cue() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();

        eng.on_event(mv((3, 10), (3, 15), Licence::Nav), t0);
        assert!(
            eng.drain_sound_cues().next().is_none(),
            "nothing is minted on the edge"
        );
        eng.set_engaged(false);
        assert!(eng.drain_sound_cues().next().is_none());
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert!(sc.cues.is_empty(), "the unticked hop went with the switch");
        eng.set_engaged(true);
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(1), geom(), &cfg, &mut fr);
        assert!(sc.cues.is_empty(), "…and does not resurface on re-engage");
        assert_eq!(eng.status().stars, 0, "nor does its light");

        eng.on_event(
            mv((3, 10), (3, 15), Licence::Nav),
            t0 + Duration::from_millis(2),
        );
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(2), geom(), &cfg, &mut fr);
        assert_eq!(sc.cues.len(), 1, "the ticked hop rode out on its frame");
        assert!(
            eng.drain_sound_cues().next().is_none(),
            "the frame is the one sink: nothing is left for the drain"
        );
        eng.set_engaged(false);
        assert!(eng.drain_sound_cues().next().is_none());
    }

    fn cat(row: u16, col: f32, settled: bool, purr: f32) -> companion::PetOnGlass {
        companion::PetOnGlass {
            col,
            width: 6.0,
            row,
            settled,
            purr,
            contented: false,
        }
    }

    #[test]
    fn the_meteor_s_perk_reaches_the_resident_at_the_landing_s_own_instant() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();

        eng.on_event(mv((3, 0), (3, 40), Licence::Nav), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        let imp = fr.companion.expect("the spawn frame carries the impulse");

        assert_eq!(eng.meteor.live.len(), 1, "the jump must have flown");
        let arrival = eng.meteor.live[0].arrival();
        let flight = arrival.saturating_duration_since(t0);
        assert!(
            (60..=120).contains(&flight.as_millis()),
            "fixture: T must be a real flight, got {flight:?}"
        );

        let offer = eng.pet_offer(geom(), Some(cat(3, 12.0, true, 0.4)));
        assert_eq!(
            offer.perk_at,
            Some(arrival),
            "the perk edge must be the meteor's own arrival edge"
        );
        assert_ne!(
            offer.perk_at,
            Some(t0),
            "the perk is the LANDING's instant, never the launch's"
        );

        let mut t = t0;
        let mut pinned = None;
        for _ in 0..16 {
            t += Duration::from_millis(8);
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
            if let Some(l) = eng.meteor.landings.first() {
                pinned = Some(l.pin.at);
                break;
            }
        }
        assert_eq!(
            pinned,
            Some(arrival),
            "the landing pin is born on the arrival edge"
        );
        assert_eq!(
            offer.perk_at, pinned,
            "the pet's perk and the landing pin must be the SAME instant"
        );
        assert!(
            eng.caret_seam.flare_at.is_some() || eng.paint_at.is_some(),
            "fixture: the caret's own edge is live for the same landing"
        );

        assert_eq!(
            companion::impulse_for(companion::Body::Pet, imp),
            companion::BodyImpulse::Perk { at: arrival }
        );

        assert_eq!(eng.pet_offer(geom(), None).perk_at, Some(arrival));

        let mut slow = engaged();
        let mut sc2 = Scratch::default();
        let reduced = Config {
            reduced_motion: true,
            ..config()
        };
        slow.on_event(mv((3, 0), (3, 40), Licence::Nav), t0);
        let mut fr2 = sc2.frame();
        slow.tick(t0, geom(), &reduced, &mut fr2);
        assert!(matches!(fr2.companion, Some(CompanionImpulse::Land)));
        assert_eq!(
            slow.pet_offer(geom(), Some(cat(3, 12.0, true, 0.4)))
                .perk_at,
            None,
            "a landing with no flight behind it is a pose, not an edge"
        );
    }

    #[test]
    fn a_gold_star_near_a_contented_cat_is_offered_and_caught() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let ms = Duration::from_millis;
        blank_row(&mut eng, 2);

        let mut gold_col = None;
        for col in (6u16..90).step_by(4) {
            let at = t0 + ms(u64::from(col));
            blank_row(&mut eng, 2);
            eng.earn_hero(at, 3, col);
            let mut fr = sc.frame();
            eng.tick(at, geom(), &cfg, &mut fr);
            if eng
                .stardust
                .live_iter()
                .any(|s| s.gold && s.class == stardust::StarClass::M1)
            {
                gold_col = Some((col, at));
                break;
            }
        }
        let (col, born_at) = gold_col.expect("fixture: the deal must produce a gold m1");
        let stars_before = eng.status().stars;
        assert!(stars_before >= 1);

        let near = cat(3, f32::from(col) + 4.0, true, 0.4);
        let offer = eng.pet_offer(geom(), Some(near));
        let star = offer.catch.expect("a gold m1 in reach is offered");
        assert_eq!(star.born, born_at, "the offer names the star's own edge");
        assert!(
            near.columns_to(star.col) <= companion::CATCH_REACH_CELLS,
            "the offered star must be inside the reach it was offered under"
        );

        assert_eq!(
            eng.pet_offer(geom(), Some(cat(3, f32::from(col) + 4.0, false, 0.4)))
                .catch,
            None,
            "a cat that is not settled is offered nothing"
        );
        assert_eq!(
            eng.pet_offer(geom(), Some(cat(3, f32::from(col) + 4.0, true, 0.0)))
                .catch,
            None,
            "a cat that is not purring is offered nothing"
        );
        assert_eq!(
            eng.pet_offer(geom(), Some(cat(3, f32::from(col) + 24.0, true, 0.4)))
                .catch,
            None,
            "a star out of reach is not offered"
        );
        assert_eq!(
            eng.pet_offer(geom(), Some(cat(9, f32::from(col) + 4.0, true, 0.4)))
                .catch,
            None,
            "a paw reaches along its own line, not three lines up"
        );

        let paw = born_at + ms(200);
        let others: Vec<(f32, f32, Option<stardust::Finish>)> = eng
            .stardust
            .live_iter()
            .filter(|s| s.born != star.born || s.x != star.x)
            .map(|s| (s.x, s.y, s.finish))
            .collect();
        let deadline_before = eng.next_change_deadline(paw);
        assert!(eng.catch_star(star, paw), "the offered star must be there");

        let caught = eng
            .stardust
            .live_iter()
            .find(|s| s.born == star.born && s.x == star.x && s.y == star.y)
            .expect("the caught star is still in the pool on the paw's frame");
        assert_eq!(
            caught.finish,
            Some(stardust::Finish {
                at: paw,
                span_s: stardust::CATCH_FINISH_MS / 1000.0,
            }),
            "the catch spends the star on the sky's own 40 ms finish"
        );
        for (x, y, finish) in &others {
            let s = eng
                .stardust
                .live_iter()
                .find(|s| s.x == *x && s.y == *y)
                .expect("a bystander must not leave the pool");
            assert_eq!(s.finish, *finish, "a bystander's life was shortened too");
        }

        assert!(
            !caught.dead(paw + ms(10)),
            "the catch is a finish, not a pop"
        );
        assert!(
            caught.dead(paw + ms(40)),
            "the caught star must be off the glass within 40 ms"
        );

        let cues = sc.cues.len();
        let mut fr = sc.frame();
        eng.tick(paw, geom(), &cfg, &mut fr);
        assert!(
            eng.status().stars <= stars_before,
            "the catch minted a star"
        );
        assert_eq!(sc.cues.len(), cues, "the catch minted a sound");
        assert_eq!(
            eng.pet_offer(geom(), Some(near)).catch,
            None,
            "an already-caught star is not offered again"
        );

        if let (Some(before), Some(after)) = (deadline_before, eng.next_change_deadline(paw)) {
            assert!(
                after <= before,
                "the catch may not ask for a later frame than the sky already wanted"
            );
        }

        assert!(
            !eng.catch_star(star, paw + ms(10)),
            "a second catch during the finish must return false"
        );
        let still = eng
            .stardust
            .live_iter()
            .find(|s| s.born == star.born && s.x == star.x)
            .expect("still finishing at +10 ms");
        assert_eq!(
            still.finish.map(|f| f.at),
            Some(paw),
            "a second catch must never give the star more time"
        );
        assert!(
            !eng.catch_star(star, paw + ms(300)),
            "a catch after the star died must return false"
        );
    }

    #[test]
    fn the_fingerprint_folds_only_this_ticks_own_marks() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = Engine::new();
        eng.set_engaged(true);
        let mut sc = Scratch::default();
        sc.out.push(GlowQuad {
            row: 2,
            x: 40,
            y: 40,
            w: 3,
            h: 3,
            color: 0x0011_2233,
            alpha: 0,
            color2: 0x0011_2233,
            alpha2: 0,
        });
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert_eq!(fr.fp, 0, "a foreign quad is not v2's light");
        assert_eq!(sc.out.len(), 1, "and v2 must not disturb it");
    }

    const SIM_MIN: Duration = Duration::from_millis(1);

    const SIM_ROW: u16 = 12;

    type Turn = (Instant, Option<Instant>, bool);

    type Ask<'a> = &'a dyn Fn(&Engine, Instant) -> Option<Instant>;

    fn engine_with_sky() -> Engine {
        let mut eng = Engine::new();
        eng.set_engaged(true);
        let blank = vec![false; geom().cols];
        for r in [SIM_ROW - 1, SIM_ROW, SIM_ROW + 1] {
            eng.probe_mut().probe_row(i32::from(r), &blank);
        }
        eng
    }

    fn secs(s: f32) -> Duration {
        Duration::from_secs_f32(s)
    }

    fn prose_script(t0: Instant, keys: usize) -> Vec<(Instant, Event)> {
        const TEXT: &[u8] = b"The quick brown fox jumps over the lazy dog and keeps on going ";
        let mut out = Vec::with_capacity(keys * 2);
        for k in 0..keys {
            let at = t0 + secs(k as f32 / 12.0);
            let ch = TEXT[k % TEXT.len()] as char;
            let col = 4 + k as u16;
            out.push((
                at,
                Event::Move {
                    from: (SIM_ROW, col),
                    to: (SIM_ROW, col + 1),
                    licence: Licence::Typed,
                    dir: Dir::Right,
                },
            ));
            let class = if ch == ' ' {
                TypedClass::Space
            } else if ch.is_ascii_uppercase() {
                TypedClass::Capital
            } else {
                TypedClass::Glyph
            };
            out.push((
                at,
                Event::Typed {
                    cells: 1,
                    shifted: class == TypedClass::Capital,
                    class,
                },
            ));
        }
        out
    }

    fn meteor_script(t0: Instant) -> Vec<(Instant, Event)> {
        vec![(
            t0,
            Event::Move {
                from: (SIM_ROW, 10),
                to: (SIM_ROW, 50),
                licence: Licence::Nav,
                dir: Dir::Right,
            },
        )]
    }

    fn drive(
        eng: &mut Engine,
        cfg: &Config,
        script: &[(Instant, Event)],
        from: Instant,
        until: Instant,
        ask: Ask<'_>,
    ) -> Vec<Turn> {
        let mut sc = Scratch::default();
        let mut turns = Vec::new();
        let mut next = 0usize;
        let mut now = from;
        loop {
            while let Some(&(at, ev)) = script.get(next)
                && at <= now
            {
                eng.on_event(ev, at);
                next += 1;
            }
            sc.under.clear();
            sc.out.clear();
            sc.halos.clear();
            sc.cues.clear();
            let mut fr = sc.frame();
            eng.tick(now, geom(), cfg, &mut fr);
            let d = ask(eng, now);
            turns.push((now, d, eng.needs_frame_cadence()));
            let wake = d.map(|d| d.max(now + SIM_MIN));
            let ev = script.get(next).map(|e| e.0);
            let Some(n) = (match (wake, ev) {
                (Some(w), Some(e)) => Some(w.min(e)),
                (w, e) => w.or(e),
            }) else {
                break;
            };
            if n >= until {
                break;
            }
            now = n;
        }
        turns
    }

    fn arms_in(turns: &[Turn], origin: Instant, a: f32, b: f32) -> usize {
        turns
            .iter()
            .filter(|(t, d, _)| d.is_some() && *t >= origin + secs(a) && *t < origin + secs(b))
            .count()
    }

    #[test]
    fn a_settling_sky_asks_for_no_more_frames_than_its_light_can_show() {
        let cfg = config();
        let t0 = Instant::now();
        let script = prose_script(t0, 24);
        let last = script.last().map(|e| e.0).unwrap_or(t0);
        let mut eng = engine_with_sky();
        let turns = drive(&mut eng, &cfg, &script, t0, last + secs(2.6), &|e, now| {
            e.next_change_deadline(now)
        });
        let budget = |a: f32, b: f32| ((b - a) * 25.0).ceil() as usize + 1;
        let grace = arms_in(&turns, last, 0.50, 0.75);
        assert!(
            grace <= budget(0.50, 0.75),
            "grace tail: {grace} arms in 250 ms (budget {})",
            budget(0.50, 0.75)
        );
        let (f0, f1) = (ribbon::SWOOSH_TOTAL_S - 0.24, ribbon::SWOOSH_TOTAL_S);
        let fade = arms_in(&turns, last, f0, f1);
        assert!(
            fade <= budget(f0, f1),
            "swoosh fade: {fade} arms in 240 ms (budget {})",
            budget(f0, f1)
        );
        for (t, d, _) in &turns {
            if let Some(d) = d {
                assert!(
                    *d >= *t + ARM_MIN,
                    "a deadline {:?} before now + 1 ms at +{:.1} ms",
                    d.saturating_duration_since(*t),
                    t.saturating_duration_since(last).as_secs_f32() * 1000.0
                );
            }
        }
        let idle_at = turns
            .iter()
            .find(|(t, d, _)| *t >= last && d.is_none())
            .map(|(t, _, _)| *t)
            .expect("the engine never reached idle");
        assert!(
            idle_at <= last + secs(2.5),
            "idle only at +{:?}",
            idle_at.saturating_duration_since(last)
        );
        assert!(
            turns.iter().all(|(t, _, _)| *t <= idle_at),
            "the sim woke after idle"
        );
        assert!(!eng.needs_frame_cadence());
        assert!(eng.next_change_deadline(idle_at + secs(1.0)).is_none());
        let st = eng.status();
        assert_eq!((st.stars, st.cells, st.meteors), (0, 0, 0));
    }

    #[test]
    fn the_cadence_fold_floors_tails_and_ignores_past_edges() {
        let now = Instant::now();
        assert!(Cadence::at(now).take().is_none());
        let mut c = Cadence::at(now);
        c.edge(now - secs(0.5));
        assert!(c.take().is_none(), "a past edge is not a next change");
        let mut c = Cadence::at(now);
        c.tail(0.010);
        assert_eq!(c.take(), Some(now + TAIL_FLOOR), "a 10 ms step is floored");
        let mut c = Cadence::at(now);
        c.tail(0.080);
        assert_eq!(c.take(), Some(now + secs(0.080)), "an 80 ms step stands");
        let mut c = Cadence::at(now);
        c.tail_at(now - secs(1.0));
        assert_eq!(c.take(), Some(now + TAIL_FLOOR), "a past tail is the floor");
        let mut c = Cadence::at(now);
        c.tail(0.080);
        c.edge(now + secs(0.030));
        assert_eq!(
            c.take(),
            Some(now + secs(0.030)),
            "an edge inside a tail wins"
        );
        let mut c = Cadence::at(now);
        c.edge(now + secs(0.002));
        c.brisk();
        assert_eq!(
            c.take(),
            Some(now + FRAME_CADENCE),
            "brisk is the next frame"
        );
    }

    #[test]
    fn a_flight_asks_for_every_frame() {
        let cfg = config();
        let t0 = Instant::now();
        let mut eng = engine_with_sky();
        let flight = timing::flight(40.0);
        let until = t0 + flight + secs(0.150);
        let turns = drive(&mut eng, &cfg, &meteor_script(t0), t0, until, &|e, now| {
            e.next_change_deadline(now)
        });
        let expect = FRAME_CADENCE.as_secs_f32();
        assert!(
            turns.len() >= 20,
            "only {} turns over the flight",
            turns.len()
        );
        for w in turns.windows(2) {
            let gap = w[1].0.saturating_duration_since(w[0].0).as_secs_f32();
            assert!(
                (gap - expect).abs() < 1e-4,
                "a {:.2} ms wake at +{:.1} ms; the flight asks for {:.2} ms",
                gap * 1000.0,
                w[0].0.saturating_duration_since(t0).as_secs_f32() * 1000.0,
                expect * 1000.0
            );
        }
        assert!(
            turns.iter().all(|(_, _, brisk)| *brisk),
            "seam point 8 said no cadence during the flight"
        );
    }

    #[test]
    fn a_typed_fold_wraps_at_the_pane_edge_not_the_grid_edge() {
        let t0 = Instant::now();
        let cfg = config();
        let mut g = geom();
        g.cols = 100;
        g.win_w = 900;
        let drive = |pane: Option<(u16, u16)>| -> Engine {
            let mut eng = engaged();
            eng.set_pane(pane);
            eng.on_event(mv((2, 50), (2, 51), Licence::Typed), t0);
            eng.on_event(typed(1), t0);
            let mut sc = Scratch::default();
            let mut fr = sc.frame();
            eng.tick(t0, g, &cfg, &mut fr);
            let fold = t0 + Duration::from_millis(20);
            eng.on_event(typed(1), fold);
            eng.on_event(mv((2, 51), (3, 12), Licence::Typed), fold);
            let mut sc = Scratch::default();
            let mut fr = sc.frame();
            eng.tick(fold, g, &cfg, &mut fr);
            eng
        };
        let paned = drive(Some((12, 40)));
        assert!(
            paned.field_at(2, 51).is_some(),
            "the fold's glyph is the pane's last cell"
        );
        assert!(
            paned.field_at(2, 99).is_none(),
            "no ribbon cell leaks into the neighbouring pane"
        );
        let whole = drive(None);
        assert!(
            whole.field_at(3, 11).is_some() && whole.field_at(2, 51).is_none(),
            "the control: told nothing of the pane, the fold lights the cell \
             before the caret in the neighbouring pane — the frame this law \
             is about"
        );
    }

    #[test]
    fn forty_presses_typed_into_a_stall_fit_the_ledger() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        let first = t + ms(100);
        let mut last = first;
        for k in 0..40u64 {
            last = first + ms(85 * k);
            eng.on_event(typed(1), last);
            tick_at(&mut eng, last);
        }
        let echo = last + ms(400);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 5,
                col1: 45,
            },
            first,
        );
        eng.on_event(mv((3, 5), (3, 45), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        let tally = eng.echo.tally;
        assert_eq!(
            (tally.spent, tally.forfeited, tally.expired),
            (43, 0, 0),
            "the three pre-roll presses and the forty of the batch are spent, none forfeited"
        );
        for col in 5..45u16 {
            assert!(eng.field_at(3, col).is_some(), "col {col} is dark");
        }
    }

    #[test]
    fn a_batch_sweep_on_the_oldest_press_clock_is_born_no_earlier_than_the_birth_floor() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        let first = t + ms(100);
        let mut last = first;
        for k in 0..12u64 {
            last = first + ms(85 * k);
            eng.on_event(typed(1), last);
            tick_at(&mut eng, last);
        }
        let echo = last + ms(3000);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 5,
                col1: 17,
            },
            first,
        );
        eng.on_event(mv((3, 5), (3, 17), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        for col in 5..17u16 {
            assert!(
                eng.field_at(3, col).is_some(),
                "col {col} is dark in the echoing frame"
            );
        }
        tick_at(&mut eng, echo + ms(100));
        for col in 5..17u16 {
            assert!(
                eng.field_at(3, col).is_some(),
                "col {col} is dark 100 ms after the echo"
            );
        }
    }

    #[test]
    fn a_scroll_or_a_band_move_carries_the_pre_move_caret_with_the_text_it_names() {
        type Seam = (&'static str, fn(&mut Engine));
        let seams: [Seam; 2] = [
            ("scroll", |eng| eng.translate_scroll(1, geom().ch as u16)),
            ("band", |eng| {
                eng.translate_band(0, 10, -1, geom().ch as u16, 0)
            }),
        ];
        for (seam, shift) in seams {
            let ms = Duration::from_millis;
            let t0 = Instant::now();
            let mut eng = engaged();
            blank_row(&mut eng, 2);
            blank_row(&mut eng, 3);
            eng.on_event(mv((3, 0), (3, 5), Licence::Typed), t0);
            tick_at(&mut eng, t0);
            eng.on_event(typed(1), t0 + ms(100));
            eng.on_event(mv((3, 5), (3, 20), Licence::Nav), t0 + ms(110));
            shift(&mut eng);
            tick_at(&mut eng, t0 + ms(120));
            assert!(
                eng.ribbon()
                    .cells()
                    .iter()
                    .any(|c| c.typing && (c.row, c.col) == (2, 4)),
                "the key rides the {seam} with its glyph: {:?}",
                typing_cells(&eng)
            );
            assert!(
                eng.ribbon().cells().iter().all(|c| !c.typing || c.row != 3),
                "…and nothing of it is left a row below ({seam}): {:?}",
                typing_cells(&eng)
            );
        }
    }

    #[test]
    fn a_pre_move_caret_carried_off_the_top_is_forgotten_not_clamped() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        blank_row(&mut eng, 0);
        blank_row(&mut eng, 3);
        eng.on_event(mv((3, 0), (3, 5), Licence::Typed), t0);
        tick_at(&mut eng, t0);
        eng.on_event(typed(1), t0 + ms(100));
        eng.on_event(mv((3, 5), (3, 20), Licence::Nav), t0 + ms(110));
        eng.translate_scroll(4, geom().ch as u16);
        tick_at(&mut eng, t0 + ms(120));
        assert!(
            eng.ribbon().cells().iter().all(|c| !c.typing),
            "the key whose row scrolled away lays nowhere — not at row 0, not \
             on the row it was typed on: {:?}",
            typing_cells(&eng)
        );
    }

    fn typing_cells(eng: &Engine) -> Vec<(u16, u16)> {
        eng.ribbon()
            .cells()
            .iter()
            .filter(|c| c.typing)
            .map(|c| (c.row, c.col))
            .collect()
    }

    #[test]
    fn a_bridge_sweep_inserted_after_a_one_shot_move_does_not_shift_the_pre_move_caret() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        blank_row(&mut eng, 2);
        eng.on_event(mv((3, 0), (3, 5), Licence::Typed), t0);
        tick_at(&mut eng, t0);
        let k = t0 + ms(100);
        eng.on_event(typed(1), k);
        eng.on_event(mv((3, 5), (3, 20), Licence::Nav), k + ms(10));
        eng.on_event(typed(1), k + ms(20));
        eng.on_event(mv((3, 20), (3, 21), Licence::Typed), k + ms(30));
        tick_at(&mut eng, k + ms(30));
        assert_eq!(
            eng.status().bridged,
            1,
            "the second glyph's unswept echo is the ledger's bridge — the insert at index 0"
        );
        assert!(
            eng.field_at(3, 4).is_some(),
            "the glyph typed before the arrow's hop replays at the caret before it"
        );
    }

    #[test]
    fn a_reset_or_a_curtain_forgets_the_pre_move_caret_with_the_events_it_indexed() {
        let ms = Duration::from_millis;
        type Seam = (&'static str, fn(&mut Engine, Instant), u64);
        let seams: [Seam; 2] = [
            ("reset", |eng, _| eng.reset(), 200),
            (
                "curtain",
                |eng, t0| eng.curtain(t0 + Duration::from_millis(120)),
                900,
            ),
        ];
        for (seam, forget, key_ms) in seams {
            let t0 = Instant::now();
            let mut eng = engaged();
            blank_row(&mut eng, 2);
            eng.on_event(mv((3, 0), (3, 5), Licence::Typed), t0);
            tick_at(&mut eng, t0);
            eng.on_event(typed(1), t0 + ms(100));
            eng.on_event(mv((3, 5), (3, 20), Licence::Nav), t0 + ms(110));
            forget(&mut eng, t0);
            let k = t0 + ms(key_ms);
            eng.on_event(typed(1), k);
            eng.on_event(mv((7, 0), (7, 10), Licence::Typed), k + ms(8));
            tick_at(&mut eng, k + ms(8));
            assert!(
                eng.field_at(3, 4).is_none(),
                "the key is not replayed at a caret the {seam} forgot"
            );
            assert!(
                eng.field_at(7, 9).is_some(),
                "…it lays at its own echo ({seam})"
            );
        }
    }

    #[test]
    fn a_key_typed_before_the_first_licensed_move_is_held_not_laid_at_an_unset_mirror() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        blank_row(&mut eng, 2);
        eng.on_event(typed(1), t0);
        eng.on_event(mv((3, 10), (5, 2), Licence::Nav), t0 + ms(10));
        tick_at(&mut eng, t0 + ms(10));
        assert!(
            eng.ribbon().cells().iter().all(|c| !c.typing),
            "nothing typed is lit — not at the unset mirror, not at the landing's neighbour: {:?}",
            eng.ribbon()
                .cells()
                .iter()
                .map(|c| (c.row, c.col, c.typing))
                .collect::<Vec<_>>()
        );
        let echo = t0 + ms(30);
        eng.on_event(
            Event::Sweep {
                row: 5,
                col0: 2,
                col1: 3,
            },
            t0,
        );
        eng.on_event(mv((5, 2), (5, 3), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        assert!(
            eng.field_at(5, 2).is_some(),
            "the key's cell comes with its echo"
        );
    }

    #[test]
    fn a_held_space_measures_the_word_the_composer_moves_from_its_own_cell() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        blank_row(&mut eng, 2);
        eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
        tick_at(&mut eng, t0);
        let mut t = t0;
        for col in 2..4u16 {
            t += ms(100);
            t = type_key_at(&mut eng, t, col);
        }
        let space_at = t + ms(100);
        eng.on_event(
            Event::Typed {
                cells: 1,
                shifted: false,
                class: TypedClass::Space,
            },
            space_at,
        );
        tick_at(&mut eng, space_at);
        let echo = space_at + ms(16);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 4,
                col1: 5,
            },
            space_at,
        );
        eng.on_event(mv((3, 4), (3, 5), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        let mut t = echo;
        for col in 5..7u16 {
            t += ms(100);
            t = type_key_at(&mut eng, t, col);
        }
        let k = t + ms(100);
        eng.on_event(typed(1), k);
        eng.on_event(mv((3, 7), (4, 5), Licence::Typed), k + ms(8));
        tick_at(&mut eng, k + ms(8));
        assert!(
            eng.field_at(4, 2).is_some() && eng.field_at(4, 3).is_some(),
            "the moved word's two cells are relaid"
        );
        assert!(
            eng.field_at(4, 1).is_none(),
            "the word is `cd`, two cells — not three from a Space measured one cell left"
        );
    }

    #[test]
    fn the_meteor_pool_is_handed_the_hand_s_floor() {
        let t0 = Instant::now();
        let mut eng = engaged();
        for row in 5..=7 {
            blank_row(&mut eng, row);
        }
        assert_eq!(
            eng.meteor.hand_floor_cell(),
            None,
            "a pool that has seen no hand is bounded by nothing"
        );

        let mut t = t0;
        for col in 28..43_u16 {
            eng.on_event(typed(1), t);
            tick_at(&mut eng, t);
            let echo = t + Duration::from_millis(8);
            eng.on_event(
                Event::Sweep {
                    row: 6,
                    col0: col,
                    col1: col + 1,
                },
                t,
            );
            eng.on_event(mv((6, col), (6, col + 1), Licence::Typed), echo);
            tick_at(&mut eng, echo);
            t = echo + Duration::from_millis(60);
        }
        assert_eq!(
            eng.meteor.hand_floor_cell(),
            Some((6, 28)),
            "the typing's own leftmost column is the floor"
        );

        eng.on_event(mv((6, 43), (6, 28), Licence::Nav), t);
        tick_at(&mut eng, t);
        assert_eq!(
            eng.meteor.hand_floor_cell(),
            Some((6, 28)),
            "the ^A's landing is the hand reaching a column, and the pool has it on \
             the tick the flight is born"
        );
    }

    #[test]
    fn curtain_updates_fingerprint_and_brisk_and_translate_scroll_carries_erased_and_earned() {
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        let settle = t + Duration::from_millis(60);
        tick_at(&mut eng, settle);
        assert_eq!(eng.fingerprint(), 1);
        assert!(
            !eng.needs_frame_cadence(),
            "settled in Grace: no per-frame motion before the curtain"
        );

        eng.curtain(settle);
        assert_eq!(
            eng.fingerprint(),
            1,
            "a falling curtain over laid cells must stay live (fingerprint != 0)"
        );
        assert!(
            eng.needs_frame_cadence(),
            "a falling curtain over laid cells must immediately report brisk frame cadence"
        );
        assert_eq!(eng.status().cells, 3);

        let mut flight_only = engaged();
        flight_only.on_event(mv((3, 5), (10, 5), Licence::Nav), t0);
        tick_at(&mut flight_only, t0);
        assert!(flight_only.needs_frame_cadence());
        flight_only.curtain(t0 + Duration::from_millis(10));
        assert_eq!(flight_only.fingerprint(), 0);
        assert!(!flight_only.needs_frame_cadence());

        let mut sc_eng = engaged();
        let t_end = three_typed_cells(&mut sc_eng, t0);
        sc_eng.on_event(Event::Erase, t_end + Duration::from_millis(20));
        sc_eng.on_event(
            mv((3, 7), (3, 6), Licence::Typed),
            t_end + Duration::from_millis(20),
        );
        tick_at(&mut sc_eng, t_end + Duration::from_millis(20));
        assert_eq!(sc_eng.erased, (3, 6));
        sc_eng.observe_caret((5, 2));
        sc_eng.earn_hero(t_end + Duration::from_millis(25), 3, 6);
        sc_eng.translate_scroll(2, 18);
        assert_eq!(
            sc_eng.erased,
            (1, 6),
            "erased cell row translates with scroll"
        );
        assert_eq!(
            sc_eng.caret_seen,
            Some((3, 2)),
            "pending observe_caret row translates with scroll"
        );
        assert_eq!(
            sc_eng.earned.first().map(|(c, _)| *c),
            Some((1, 6)),
            "pending earned hero cell translates with scroll"
        );

        sc_eng.note_lift(
            ribbon::Lift {
                origin: (4, 10),
                landing: (4, 5),
                dst: (3, 70),
            },
            Some(t_end),
        );
        sc_eng.note_soft_wrap(
            ribbon::SoftWrap {
                origin: (4, 79),
                landing: (5, 0),
                word_col0: 72,
            },
            Some(t_end),
        );
        sc_eng.translate_scroll(1, 18);
        assert_eq!(
            sc_eng.lift.map(|(l, _)| (l.origin, l.landing, l.dst)),
            Some(((3, 10), (3, 5), (2, 70))),
            "pending lift verdict translates with scroll"
        );
        assert!(
            sc_eng.soft_wrap.is_none(),
            "pending soft_wrap verdict is cleared by scroll"
        );
        sc_eng.earn_hero(t_end + Duration::from_millis(30), 3, 9);
        sc_eng.prove_short_reanchor((3, 10), (3, 9), t_end);
        sc_eng.lift_caret(3, 2);
        assert_eq!(
            sc_eng.lift.map(|(l, _)| (l.origin, l.landing, l.dst)),
            Some(((2, 10), (2, 5), (2, 70))),
            "lift_caret translates pending lift verdict on the lifted row"
        );
        assert_eq!(
            sc_eng.earned.last().map(|(c, _)| *c),
            Some((2, 9)),
            "lift_caret translates pending earned hero cell on the lifted row"
        );

        sc_eng.hand_lift = Some((3, 2));
        sc_eng.withheld_n = 1;
        sc_eng.set_flush_clock(Some(t_end));
        sc_eng.curtain(t_end + Duration::from_millis(40));
        assert!(sc_eng.take_hand_lift().is_none());
        assert_eq!(sc_eng.withheld_n, 0);
        assert!(sc_eng.flush_clock.is_none());

        sc_eng.hand_lift = Some((3, 2));
        sc_eng.withheld_n = 1;
        sc_eng.set_flush_clock(Some(t_end));
        sc_eng.reset();
        assert!(sc_eng.take_hand_lift().is_none());
        assert_eq!(sc_eng.withheld_n, 0);
        assert!(sc_eng.flush_clock.is_none());
    }
}
