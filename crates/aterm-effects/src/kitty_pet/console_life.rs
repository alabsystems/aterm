// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Located console attention within the resident brain.

use super::*;
use crate::pet_world::TaskbarProgress;
use crate::pet_world::{
    PetAnchor, PetPane, PetRect, PetReserve, PetWorld, PetWorldFacts, PetWorldStamp,
};
use aterm_core::terminal::BlockState;

const INPUT_HOLD: f32 = 0.65;
const _: () = assert!(TRICK_SETTLE > INPUT_HOLD);
const EDIT_HOLD: f32 = 1.10;
const CONTACT_HOLD: f32 = 0.40;
const RESULT_HOLD: f32 = 1.20;
const REACTION_TTL: f32 = 2.0;
const CLEARANCE: f32 = 0.10;
const CONTACT_MARGIN: f32 = 0.45;
const TYPED_RUN_MAX: usize = 4;
const PERCH_REACH: f32 = 24.0;
const CHEER_STAGE_COLS: f32 = 96.0;
const CHEER_STAGE_TENTHS_UP: u8 = 60;

/// A landing's stage ([`PetBrain::cheer_stage`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CheerStage {
    pub(super) at: Option<(f32, f32)>,
    pub(super) air: CheerAir,
}

const HOME_REACH: f32 = 6.0;
const RIDE_ROWS_MAX: usize = 3;
const REST_REACH: f32 = 64.0;

const SEAT_KEPT: f32 = 0.5;

const HIDDEN_HOLD_MAX: f32 = 0.25;

const PERCH_DWELL: f32 = 3.0;

const VETO_DWELL: f32 = 0.25;
const VETO_FADE: f32 = 0.20;
const INV_VETO_FADE: f32 = 1.0 / VETO_FADE;
const VETO_BLIND: f32 = 0.50;
const Z_DWELL: f32 = VETO_DWELL;

#[inline]
const fn dur_from_secs_f32(secs: f32) -> Duration {
    Duration::from_nanos((secs as f64 * 1_000_000_000.0).round() as u64)
}

const INPUT_HOLD_DUR: Duration = dur_from_secs_f32(INPUT_HOLD);
const EDIT_HOLD_DUR: Duration = dur_from_secs_f32(EDIT_HOLD);
const CONTACT_HOLD_DUR: Duration = dur_from_secs_f32(CONTACT_HOLD);
const RESULT_HOLD_DUR: Duration = dur_from_secs_f32(RESULT_HOLD);
const PERCH_DWELL_DUR: Duration = dur_from_secs_f32(PERCH_DWELL);
const Z_DWELL_DUR: Duration = dur_from_secs_f32(Z_DWELL);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaretSeat {
    Hold,
    Escort,
    Yield,
}

#[derive(Clone, Copy, Debug)]
struct VisibilityHold {
    shown: bool,
    contrary_since: Option<Instant>,
    blind_since: Option<Instant>,
    cover: f32,
    at: Option<Instant>,
    by_fiat: bool,
}

impl Default for VisibilityHold {
    #[inline]
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl VisibilityHold {
    const DEFAULT: Self = Self {
        shown: true,
        contrary_since: None,
        blind_since: None,
        cover: 1.0,
        at: None,
        by_fiat: false,
    };

    fn update(&mut self, now: Instant, verdict: Option<bool>) -> f32 {
        let dt = self.advance(now);
        if self.by_fiat {
            if verdict != Some(true) {
                return self.cover;
            }
            self.by_fiat = false;
            self.shown = true;
            self.cover = 1.0;
            self.contrary_since = None;
            self.blind_since = None;
            return self.cover;
        }
        let observed = match verdict {
            Some(clear) => {
                self.blind_since = None;
                Some(clear)
            }
            None => {
                let since = *self.blind_since.get_or_insert(now);
                (now.saturating_duration_since(since).as_secs_f32() >= VETO_BLIND).then_some(false)
            }
        };
        match observed {
            Some(observed) if observed == self.shown => self.contrary_since = None,
            Some(_) => {
                let since = *self.contrary_since.get_or_insert(now);
                if now.saturating_duration_since(since).as_secs_f32() >= VETO_DWELL {
                    self.shown = !self.shown;
                    self.contrary_since = None;
                }
            }
            None => {}
        }
        self.ramp(dt)
    }

    #[inline]
    fn idle(&mut self, now: Instant) -> f32 {
        self.contrary_since = None;
        self.blind_since = None;
        let dt = self.advance(now);
        self.ramp(dt)
    }

    #[inline]
    fn advance(&mut self, now: Instant) -> f32 {
        let dt = self
            .at
            .map_or(0.0, |at| now.saturating_duration_since(at).as_secs_f32())
            .clamp(0.0, 2.0);
        self.at = Some(now);
        dt
    }

    #[inline]
    const fn ramp(&mut self, dt: f32) -> f32 {
        let target = if self.shown { 1.0 } else { 0.0 };
        let step = dt * INV_VETO_FADE;
        self.cover = if self.cover < target {
            (self.cover + step).min(target)
        } else {
            (self.cover - step).max(target)
        };
        self.cover
    }

    #[inline]
    const fn hide_now(&mut self, now: Instant) {
        self.shown = false;
        self.cover = 0.0;
        self.contrary_since = None;
        self.blind_since = None;
        self.at = Some(now);
        self.by_fiat = true;
    }

    #[inline]
    const fn settled(&self) -> bool {
        let target = if self.shown { 1.0 } else { 0.0 };
        self.cover == target
            && self.contrary_since.is_none()
            && !(self.blind_since.is_some() && self.shown)
    }
}

#[derive(Clone, Copy, Debug)]
struct ZHold {
    under: bool,
    clear_since: Option<Instant>,
}

impl Default for ZHold {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl ZHold {
    const EMPTY: Self = Self {
        under: false,
        clear_since: None,
    };

    #[inline]
    fn update(&mut self, now: Instant, ink: bool) -> bool {
        if ink {
            self.under = true;
            self.clear_since = None;
        } else if self.under {
            let since = *self.clear_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= Z_DWELL_DUR {
                *self = Self::EMPTY;
            }
        }
        self.under
    }

    #[inline]
    fn deadline(&self) -> Option<Instant> {
        self.clear_since.map(|since| since + Z_DWELL_DUR)
    }
}

#[inline]
const fn passing(frame: &PetFrame) -> bool {
    matches!(frame.action, PetAction::Leap)
}

#[inline]
const fn cover_alpha(alpha: u8, cover: f32) -> u8 {
    if alpha == 0 || cover <= 0.0 {
        return 0;
    }
    let scaled = ((alpha as f32) * cover.min(1.0)).round() as u8;
    if scaled > 1 { scaled } else { 1 }
}

/// An admitted user-input intent, never a claim that the PTY displayed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PetInputKind {
    Text,
    Delete,
    Navigate,
    Submit,
    Paste,
    /// A kill chord that moves the caret (`^U`, `^W`): reaches the cursor engines as a kill; the pet reads it as an edit like a delete.
    Kill,
    /// A kill chord that leaves the caret where it is (`^K`).
    KillForward,
}

impl PetInputKind {
    #[inline]
    #[must_use]
    pub const fn parse(kind: &str) -> Option<Self> {
        match kind.as_bytes() {
            b"text" => Some(Self::Text),
            b"delete" => Some(Self::Delete),
            b"navigate" => Some(Self::Navigate),
            b"submit" => Some(Self::Submit),
            b"paste" => Some(Self::Paste),
            b"kill" => Some(Self::Kill),
            b"kill-forward" => Some(Self::KillForward),
            _ => None,
        }
    }
}

/// The source of the resident's current attention.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PetAttention {
    #[default]
    Rest,
    Typing,
    Contact,
    Editing,
    Output,
    Progress,
    Reading,
    Result,
    Exploring,
    Yielding,
}

impl PetAttention {
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rest => "rest",
            Self::Typing => "typing",
            Self::Contact => "contact",
            Self::Editing => "editing",
            Self::Output => "output",
            Self::Progress => "progress",
            Self::Reading => "reading",
            Self::Result => "result",
            Self::Exploring => "exploring",
            Self::Yielding => "yielding",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct InputEpisode {
    seq: u64,
    kind: PetInputKind,
    at: Instant,
    target: Option<(f32, f32)>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PerchTrip {
    pub(super) from: (f32, f32),
    pub(super) to: (f32, f32),
    pub(super) elapsed: f32,
    pub(super) duration: f32,
    pub(super) hop: bool,
}

pub(super) struct ConsoleLife {
    pub(super) world: Option<Box<PetWorld>>,
    pub(super) spare_world: Option<Box<PetWorld>>,
    presentable: bool,
    seq: u64,
    input: Option<InputEpisode>,
    typed_at: Option<Instant>,
    typed_kind: Option<PetInputKind>,
    consumed: u64,
    attention: PetAttention,
    reason: &'static str,
    anchor: Option<PetAnchor>,
    pub(super) target: Option<PetRect>,
    gaze: Option<(f32, f32)>,
    resident: bool,
    resident_handoff: bool,
    moving: bool,
    clipped: bool,
    contact_at: Option<Instant>,
    contact_armed: bool,
    completion: Option<(Instant, bool)>,
    result_at: Option<(Instant, bool)>,
    pose: Option<PetGlyphId>,
    still: bool,
    hidden_hold: f32,
    caret_held: bool,
    tick_stamp: Option<PetWorldStamp>,
    repair_until: Option<Instant>,
    pub(super) repair_target: Option<(f32, f32)>,
    progress_key: Option<u64>,
    progress: Option<TaskbarProgress>,
    progress_at: Option<Instant>,
    pub(super) trip: Option<PerchTrip>,
    lift: f32,
    source_seq: u64,
    completion_seq: u64,
    failure_quiet_until: Option<Instant>,
    last_body: Option<PetRect>,
    pub(super) last_seat: Option<(f32, f32)>,
    on_glass: bool,
    hold: VisibilityHold,
    z: ZHold,
    cursor_home: Option<(u16, u16)>,
    contact_changed: bool,
    observed_seq: u64,
    progress_seq: u64,
    perched_since: Option<Instant>,
    faux_caret: Option<(u16, u16)>,
    pub(super) rest_seat: std::cell::Cell<Option<(f32, f32)>>,
    pub(super) rest_seated: std::cell::Cell<bool>,
    row_sig: Vec<u64>,
    row_sig_prev: Vec<u64>,
}

impl Default for ConsoleLife {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
const fn row_signature(row: &[aterm_core::terminal::RenderCell]) -> u64 {
    let mut blank = true;
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut i = 0;
    while i < row.len() {
        let ch = row[i].ch;
        blank &= matches!(ch, ' ' | '\0');
        h = (h ^ (ch as u32 as u64)).wrapping_mul(0x0100_0000_01b3);
        i += 1;
    }
    if blank {
        0
    } else if h > 1 {
        h
    } else {
        1
    }
}

impl ConsoleLife {
    #[inline]
    #[must_use]
    pub(super) const fn new() -> Self {
        Self {
            world: None,
            spare_world: None,
            presentable: false,
            seq: 0,
            input: None,
            typed_at: None,
            typed_kind: None,
            consumed: 0,
            attention: PetAttention::Rest,
            reason: "",
            anchor: None,
            target: None,
            gaze: None,
            resident: false,
            resident_handoff: false,
            moving: false,
            clipped: false,
            contact_at: None,
            contact_armed: false,
            completion: None,
            result_at: None,
            pose: None,
            still: false,
            hidden_hold: 0.0,
            caret_held: false,
            tick_stamp: None,
            repair_until: None,
            repair_target: None,
            progress_key: None,
            progress: None,
            progress_at: None,
            trip: None,
            lift: 0.0,
            source_seq: 0,
            completion_seq: 0,
            failure_quiet_until: None,
            last_body: None,
            last_seat: None,
            on_glass: false,
            hold: VisibilityHold::DEFAULT,
            z: ZHold::EMPTY,
            cursor_home: None,
            contact_changed: false,
            observed_seq: 0,
            progress_seq: 0,
            perched_since: None,
            faux_caret: None,
            rest_seat: std::cell::Cell::new(None),
            rest_seated: std::cell::Cell::new(false),
            row_sig: Vec::new(),
            row_sig_prev: Vec::new(),
        }
    }

    /// A fresh console life for a retired coordinate space.
    pub(super) fn retired(&mut self) -> Self {
        let spare_world = self
            .world
            .take()
            .map(|mut world| {
                world.retire();
                world
            })
            .or_else(|| self.spare_world.take());
        let mut row_sig = core::mem::take(&mut self.row_sig);
        let mut row_sig_prev = core::mem::take(&mut self.row_sig_prev);
        row_sig.clear();
        row_sig_prev.clear();
        Self {
            spare_world,
            row_sig,
            row_sig_prev,
            ..Self::default()
        }
    }
}

impl PetBrain {
    /// Exactly one note per admitted gesture/bundle.
    pub fn note_console_input(&mut self, now: Instant, kind: PetInputKind) {
        let repair_live = self.console.repair_until.is_some_and(|end| now < end);
        let erases = matches!(
            kind,
            PetInputKind::Delete | PetInputKind::Kill | PetInputKind::KillForward
        );
        if erases || (kind == PetInputKind::Text && repair_live) {
            if !repair_live {
                self.console.repair_target =
                    self.last_caret.map(|(r, c)| (f32::from(r), f32::from(c)));
            }
            self.console.repair_until = Some(now + EDIT_HOLD_DUR);
        } else {
            self.console.repair_until = None;
            self.console.repair_target = None;
        }
        self.console.seq = self.console.seq.wrapping_add(1).max(1);
        self.console.typed_at = Some(now);
        self.console.typed_kind = Some(kind);
        self.console.input = Some(InputEpisode {
            seq: self.console.seq,
            kind,
            at: now,
            target: self.last_caret.map(|(r, c)| (f32::from(r), f32::from(c))),
        });
        if matches!(
            kind,
            PetInputKind::Text | PetInputKind::Delete | PetInputKind::Submit
        ) && let Some(trick) = self.pending_trick.as_mut()
        {
            trick.stamped = now;
        }
    }

    #[inline]
    #[must_use]
    pub const fn console_input_seq(&self) -> u64 {
        self.console.seq
    }

    /// The console world the last observation read, as the pet judges a seat by it (`None` before any): what a host proves its exclusions against — a cell the host painted chrome on reads [`crate::pet_world::PetCell::Protected`] here or the pet may sit on it.
    #[inline]
    #[must_use]
    pub const fn console_world(&self) -> Option<&PetWorld> {
        match &self.console.world {
            Some(world) => Some(world),
            None => None,
        }
    }

    #[inline]
    #[must_use]
    pub const fn console_input_consumed_seq(&self) -> u64 {
        self.console.consumed
    }

    #[inline]
    #[must_use]
    pub const fn console_input_kind(&self) -> Option<PetInputKind> {
        match self.console.input {
            Some(e) => Some(e.kind),
            None => None,
        }
    }

    #[inline]
    #[must_use]
    pub const fn console_attention(&self) -> PetAttention {
        self.console.attention
    }

    #[inline]
    #[must_use]
    pub const fn console_reason(&self) -> &'static str {
        if self.console.reason.is_empty() {
            "none"
        } else {
            self.console.reason
        }
    }

    #[inline]
    #[must_use]
    pub const fn console_anchor_id(&self) -> Option<u64> {
        match self.console.anchor {
            Some(a) => Some(a.block_id),
            None => None,
        }
    }

    #[inline]
    #[must_use]
    pub const fn console_event_seq(&self) -> u64 {
        self.console.source_seq
    }

    #[inline]
    #[must_use]
    pub const fn pose_name(&self) -> &'static str {
        PET_GLYPHS[self.species.skin(self.last_pose) as usize].id
    }

    /// Clear occupancy is read from the exact cell plane being presented.
    #[inline]
    pub fn observe_console(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
    ) {
        self.observe_console_with_exclusions(input, facts, pane, &[]);
    }

    /// Cells the last world observation classified (at most `MAX_WORLD_ROWS` × `MAX_WORLD_COLS` per presented frame), so a running product can report the cost.
    #[inline]
    #[must_use]
    pub const fn observed_cells(&self) -> usize {
        match &self.console.world {
            Some(world) => world.examined_cells(),
            None => 0,
        }
    }

    #[inline]
    pub fn observe_console_with_exclusions(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
        exclusions: &[PetRect],
    ) {
        self.observe_console_with_reserve(input, facts, pane, PetReserve::protected(exclusions));
    }

    /// As [`Self::observe_console_with_exclusions`], with the host's transient overlays apart (`overlays`: the Autopilot menu, the find bar; [`PetWorld::observe_with_overlays`]).
    #[inline]
    pub fn observe_console_with_overlays(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
        exclusions: &[PetRect],
        overlays: &[PetRect],
    ) {
        self.observe_console_with_reserve(
            input,
            facts,
            pane,
            PetReserve {
                protected: exclusions,
                overlays,
                ink: &[],
            },
        );
    }

    /// The host's late overlays told apart ([`PetReserve`]): its chrome and its transient overlays protected, the person's typed glyphs (the predicted echo, the IME preedit) as ink.
    pub fn observe_console_with_reserve(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
        reserve: PetReserve<'_>,
    ) {
        let held_center = self
            .console
            .cursor_home
            .and(self.console.last_body)
            .filter(|_| {
                !input.cursor_visible
                    && self.console.presentable
                    && self.alpha > 0.0
                    && self
                        .console
                        .tick_stamp
                        .is_some_and(|prior| facts.stamp.preserves_cursor_home(prior))
            })
            .map(|body| (body.row + body.rows * 0.5, body.col + body.cols * 0.5));
        let world = self
            .console
            .world
            .get_or_insert_with(|| self.console.spare_world.take().unwrap_or_default());
        let prior_stamp = world.stamp();
        let prior_selection = world.selection_target();
        let prior_progress = world.progress();
        let typed = typed_run(input, pane, self.last_caret);
        let old_ink = if self.console.contact_changed {
            None
        } else {
            self.console
                .last_body
                .and_then(|r| local_ink(world, r, typed))
        };
        world.observe_with_exclusions_near(input, facts, pane, reserve, held_center);
        if world.stamp().is_some()
            && (prior_stamp != world.stamp()
                || prior_selection != world.selection_target()
                || prior_progress != world.progress())
        {
            self.console.observed_seq = self.console.observed_seq.wrapping_add(1).max(1);
        }
        let fixed_view = prior_stamp.zip(world.stamp()).is_some_and(|(a, b)| {
            a.surface == b.surface
                && a.top_absolute_row == b.top_absolute_row
                && a.display_offset == b.display_offset
                && a.uniform_up_rows == b.uniform_up_rows
        });
        if fixed_view {
            // Multiple coherent reads may precede one tick. A duplicate
            // observation cannot erase new local ink already observed.
            if let Some(old) = old_ink
                && let Some(r) = self.console.last_body
                && let Some(new) = local_ink(world, r, typed)
            {
                self.console.contact_changed |=
                    old.iter().zip(new).any(|(old, new)| new & !old != 0);
            }
        } else {
            // Moving existing ink through a pane-local mask is not contact
            // from newly displayed text. Reflow/unknown snapshots revoke too.
            self.console.contact_changed = false;
        }
        if prior_stamp
            .zip(world.stamp())
            .is_some_and(|(a, b)| a.surface != b.surface)
        {
            self.console.input = None;
            self.console.consumed = self.console.seq;
            self.console.repair_until = None;
            self.console.repair_target = None;
            self.console.source_seq = 0;
            self.console.completion = None;
            self.console.failure_quiet_until = None;
            self.console.faux_caret = None;
        }
        self.note_row_signatures(input, pane, prior_stamp);
        self.console.faux_caret = if !input.cursor_visible
            && input.display_offset == 0
            && input.cursor_row + 2 >= pane.row + pane.rows
        {
            detect_faux_composer_caret(input, pane)
                .filter(|&(r, _)| pane.row + usize::from(r) < input.cursor_row)
        } else {
            None
        };
    }

    fn note_row_signatures(
        &mut self,
        input: &aterm_core::render::RenderInput,
        pane: PetPane,
        prior: Option<PetWorldStamp>,
    ) {
        let console = &mut self.console;
        let Some(stamp) = console.world.as_ref().and_then(|w| w.stamp()) else {
            console.row_sig.clear();
            console.row_sig_prev.clear();
            return;
        };
        let first = pane.row.min(input.cells.len());
        let rows = pane.rows.min(input.cells.len() - first);
        let same_grid =
            prior.is_some_and(|p| p.surface == stamp.surface) && console.row_sig.len() == rows;
        if !same_grid {
            console.row_sig_prev.clear();
            console.row_sig.clear();
            console.row_sig.reserve(rows);
            for row in &input.cells[first..first + rows] {
                let end = (pane.col + pane.cols).min(row.len());
                console
                    .row_sig
                    .push(row_signature(row.get(pane.col..end).unwrap_or(&[])));
            }
            return;
        }
        let mut swapped = false;
        for (i, row) in input.cells[first..first + rows].iter().enumerate() {
            let end = (pane.col + pane.cols).min(row.len());
            let sig = row_signature(row.get(pane.col..end).unwrap_or(&[]));
            if !swapped {
                if console.row_sig[i] == sig {
                    continue;
                }
                std::mem::swap(&mut console.row_sig_prev, &mut console.row_sig);
                console.row_sig.resize(rows, 0);
                console.row_sig[..i].copy_from_slice(&console.row_sig_prev[..i]);
                swapped = true;
            }
            console.row_sig[i] = sig;
        }
    }

    const fn content_ride(&self, rect: PetRect) -> Option<f32> {
        let (cur, prev) = (
            self.console.row_sig.as_slice(),
            self.console.row_sig_prev.as_slice(),
        );
        if prev.is_empty() || prev.len() != cur.len() || !rect.valid() {
            return None;
        }
        let top_i = rect.row.floor() as i64 - 1;
        let top = (if top_i > 0 { top_i } else { 0 }) as usize;
        let bot_i = (rect.row + rect.rows).ceil() as i64 - 1;
        let bottom = (if bot_i > 0 { bot_i } else { 0 }) as usize;
        if bottom >= prev.len() {
            return None;
        }
        let len = bottom - top + 1;
        let mut same = true;
        let mut has_nonzero = false;
        let mut i = 0;
        while i < len {
            let p = prev[top + i];
            if p != cur[top + i] {
                same = false;
            }
            if p != 0 {
                has_nonzero = true;
            }
            i += 1;
        }
        if same || !has_nonzero {
            return None;
        }
        let mut k = 1;
        while k <= RIDE_ROWS_MAX {
            if bottom + k < cur.len() {
                let mut matches = true;
                let mut j = 0;
                while j < len {
                    if prev[top + j] != cur[top + k + j] {
                        matches = false;
                        break;
                    }
                    j += 1;
                }
                if matches {
                    return Some(k as f32);
                }
            }
            k += 1;
        }
        None
    }

    /// Whether the console's last observation holds a command still running
    /// (shell integration's executing block).
    pub(super) const fn console_executing(&self) -> bool {
        let Some(world) = self.console.world.as_ref() else {
            return false;
        };
        let anchors = world.anchors();
        let mut i = 0;
        while i < anchors.len() {
            if let Some(a) = &anchors[i]
                && matches!(a.state, BlockState::Executing)
            {
                return true;
            }
            i += 1;
        }
        false
    }

    /// The last observed letterform ([`crate::pet_world::PetWorld::letter_at`])
    /// among the cells `[a, b)` of `row`: `Some(Some(col))`, `Some(None)` where
    /// the map certifies none, and `None` where it covers none of them. An
    /// unobserved cell inside a covered run reads as no letter: the law that
    /// asks keeps its body off what it saw, never off what it guessed.
    pub(super) const fn observed_last_letter(
        &self,
        row: f32,
        a: f32,
        b: f32,
    ) -> Option<Option<usize>> {
        if !self.console.presentable
            || !row.is_finite()
            || row < 0.0
            || !a.is_finite()
            || !b.is_finite()
            || b <= a
        {
            return None;
        }
        #[cfg(any(test, debug_assertions))]
        if self.letters_unseen {
            return None;
        }
        let Some(world) = self.console.world.as_ref() else {
            return None;
        };
        let (first, end) = (a.max(0.0).floor() as usize, b.ceil() as usize);
        if end <= first || end - first > crate::pet_world::MAX_WORLD_COLS {
            return None;
        }
        let r = row.round() as usize;
        world.last_letter_in_span(r, first, end)
    }

    /// Exact locomotion occupancy where the map covers the whole footprint.
    pub(super) const fn observed_ink_overlaps(
        &self,
        col: f32,
        row: f32,
        width: f32,
    ) -> Option<bool> {
        let (a, b) = (col + INK_PAD, col + width - INK_PAD);
        if self.console.presentable
            && let Some(world) = self.console.world.as_ref()
            && row.is_finite()
            && row >= 0.0
            && a.is_finite()
            && a >= 0.0
            && b.is_finite()
            && b > a
        {
            let first = a.floor() as usize;
            let end = b.ceil() as usize;
            if end > first && end - first <= crate::pet_world::MAX_WORLD_COLS {
                let r = row.round() as usize;
                return world.locomotion_ink_in_span(r, first, end);
            }
        }
        None
    }

    /// An observed but incoherent surface cannot license new locomotion from the older ink spans.
    #[inline]
    pub(super) const fn console_observation_incoherent(&self) -> bool {
        self.console.presentable
            && matches!(&self.console.world, Some(world) if world.stamp().is_none())
    }

    /// Ownership/custody is independent of whether a live caret exists.
    #[inline]
    pub const fn set_console_presentable(&mut self, presentable: bool) {
        if self.console.presentable != presentable {
            self.console.hold = VisibilityHold::DEFAULT;
            self.console.z = ZHold::EMPTY;
            self.console.rest_seat = std::cell::Cell::new(None);
            self.console.rest_seated = std::cell::Cell::new(false);
        }
        self.console.presentable = presentable;
    }

    #[inline]
    #[must_use]
    pub const fn has_reading_interest(&self) -> bool {
        matches!(&self.console.world, Some(w) if w.selection_target().is_some())
    }

    #[inline]
    const fn console_body(&self, sense: PetSense) -> PetRect {
        still_body_rect(
            (self.col, self.row),
            Stance::NEUTRAL,
            (sense.cell_w, sense.cell_h),
            (sense.cols, sense.rows),
        )
    }

    /// A LANDING'S STAGE (§11.3 of `docs/DESIGN-harness-autopilot-2026-10-15.md`): ground where the cat can cheer with its ♪/♥ legible — its body, and the air the three of them fill side by side in their lanes on the side it faces ([`PetBrain::cheer_air`]), clear of ink, the caret's ring and anything protected: [`PetWorld::clear`], the law every mote on glass is held to (`finish_console_frame`).
    pub(super) const fn cheer_stage(
        &self,
        sense: PetSense,
        width: f32,
        prefer: Option<(f32, f32)>,
    ) -> Option<CheerStage> {
        #[inline]
        const fn stage_at(
            brain: &PetBrain,
            sense: PetSense,
            width: f32,
            air: CheerAir,
            (col, row): (f32, f32),
        ) -> PetRect {
            let body = still_body_rect(
                (col, row),
                Stance::NEUTRAL,
                (sense.cell_w, sense.cell_h),
                (sense.cols, sense.rows),
            );
            let (left, top, right, bottom) =
                brain.cheer_air((col, row), (sense.cell_w, sense.cell_h), width, air);
            let (row0, col0) = (top.min(body.row), left.min(body.col));
            PetRect::new(
                row0,
                col0,
                bottom.max(body.row + body.rows) - row0,
                right.max(body.col + body.cols) - col0,
            )
        }

        #[inline]
        const fn reachable(
            brain: &PetBrain,
            world: &PetWorld,
            sense: PetSense,
            width: f32,
            from: Option<PetRect>,
            air: CheerAir,
            (dc, dr): (f32, f32),
        ) -> bool {
            let stage = stage_at(brain, sense, width, air, (brain.col + dc, brain.row + dr));
            world.clear(stage, CLEARANCE)
                && match from {
                    None => true,
                    Some(from) => world.under_text_corridor_clear_past_caret(
                        from,
                        PetRect::new(from.row + dr, from.col + dc, from.rows, from.cols),
                        CLEARANCE,
                    ),
                }
        }

        if !self.console.presentable {
            return None;
        }
        let Some(world) = self.console.world.as_ref() else {
            return None;
        };
        if world.stamp().is_none() {
            return None;
        }
        let body = self.console_body(sense);
        let from = if !matches!(
            world.under_text_clearance_past_caret(body, CLEARANCE),
            Some(false)
        ) {
            Some(body)
        } else {
            None
        };
        let cw1 = if sense.cell_w > 1 { sense.cell_w } else { 1 };
        let half_px = 0.5 / (cw1 as f32);
        let tiers = [
            [
                Some(CheerAir::Lanes(self.facing_left)),
                Some(CheerAir::Lanes(!self.facing_left)),
            ],
            [Some(CheerAir::Compact), None],
        ];
        let mut ti = 0;
        while ti < tiers.len() {
            let tier = tiers[ti];
            ti += 1;
            let mut stages = [None; 2];
            let mut si = 0;
            while si < 2 {
                if let Some(air) = tier[si] {
                    let stage = stage_at(self, sense, width, air, (self.col, self.row));
                    let center = stage.col - CLEARANCE - half_px;
                    let c_lo_raw = (center - CHEER_STAGE_COLS).floor().max(0.0) as u16;
                    let c_lo = if c_lo_raw < sense.cols {
                        c_lo_raw
                    } else {
                        sense.cols
                    };
                    let c_hi_raw =
                        ((center + CHEER_STAGE_COLS).ceil().max(0.0) as u16).saturating_add(1);
                    let c_hi = if c_hi_raw < sense.cols {
                        c_hi_raw
                    } else {
                        sense.cols
                    };
                    stages[si] = Some((air, stage, c_lo, c_hi));
                }
                si += 1;
            }
            let mut si = 0;
            while si < 2 {
                if let Some((air, stage, _, _)) = stages[si]
                    && world.clear(stage, CLEARANCE)
                {
                    return Some(CheerStage { at: None, air });
                }
                si += 1;
            }
            if let Some((col, row)) = prefer {
                let mut si = 0;
                while si < 2 {
                    if let Some((air, _, _, _)) = stages[si]
                        && reachable(
                            self,
                            world,
                            sense,
                            width,
                            from,
                            air,
                            (col - self.col, row - self.row),
                        )
                    {
                        return Some(CheerStage {
                            at: Some((col, row)),
                            air,
                        });
                    }
                    si += 1;
                }
            }
            let mut best: Option<(f32, (f32, f32), CheerAir)> = None;
            let dirs = [-1.0f32, 1.0f32];
            let mut di = 0;
            while di < 2 {
                let dir = dirs[di];
                di += 1;
                let mut step = 0u8;
                while step <= CHEER_STAGE_TENTHS_UP {
                    if dir > 0.0 && step == 0 {
                        step += 1;
                        continue;
                    }
                    let dr = dir * 0.1 * (step as f32);
                    let row_dist = 2.0 * dr.abs();
                    if row_dist > CHEER_STAGE_COLS
                        || matches!(best, Some((d, _, _)) if row_dist >= d)
                    {
                        break;
                    }
                    let mut si = 0;
                    while si < 2 {
                        if let Some((air, stage, c_lo, c_hi)) = stages[si] {
                            let mut c = c_lo;
                            while c < c_hi {
                                let dc = (c as f32) + CLEARANCE + half_px - stage.col;
                                let distance = row_dist.max(dc.abs());
                                if distance > CHEER_STAGE_COLS
                                    || matches!(best, Some((d, _, _)) if distance >= d)
                                {
                                    if dc > 0.0 {
                                        break;
                                    }
                                    c += 1;
                                    continue;
                                }
                                if reachable(self, world, sense, width, from, air, (dc, dr)) {
                                    best = Some((distance, (dc, dr), air));
                                }
                                c += 1;
                            }
                        }
                        si += 1;
                    }
                    step += 1;
                }
                if best.is_some() {
                    break;
                }
            }
            if let Some((_, (dc, dr), air)) = best {
                return Some(CheerStage {
                    at: Some((self.col + dc, self.row + dr)),
                    air,
                });
            }
        }
        None
    }

    /// A LANDING SEEN AT FIRST LOOK (§11.3): a cat off glass when its cheer comes due (its home under the host's chrome, where the console law hides it, or not yet faded in) has no flight anyone could watch, and a flight from a refused body is stepped aside on its first frame, the whole arc translated with it, so it lands off its stage.
    pub(super) const fn appear_on_cheer_stage(&mut self, sense: PetSense, width: f32) -> bool {
        if self.console.on_glass || self.flight.is_some() {
            return false;
        }
        let stage = self.cheer_stage(sense, width, None);
        self.room_cheer_air = match stage {
            Some(s) => Some(s.air),
            None => None,
        };
        let Some(CheerStage {
            at: Some((col, row)),
            ..
        }) = stage
        else {
            self.room_cheer_trips = CHEER_STAGE_TRIPS;
            return false;
        };
        self.room_cheer_trips = 1;
        self.room_cheer_stage = Some((col, row));
        if let Some(CheerAir::Lanes(facing_left)) = self.room_cheer_air {
            self.facing_left = facing_left;
        }
        self.col = col;
        self.row = row;
        self.home = col;
        self.col_at_tick = col;
        self.alpha = 0.0;
        self.console.hold = VisibilityHold::DEFAULT;
        self.console.trip = None;
        self.console.target = None;
        true
    }

    /// Whether the re-sighting ghost may stay on its own glass, judged at the boundary ([`PetBrain::resight_coordinate_space`]) before the old map is retired: the body standing still at its seat in `last_stance` is admitted only where the map certifies nothing protected under it.
    #[inline]
    pub(super) const fn console_still_body_clear(&self) -> bool {
        matches!(
            self.console_still_rect_verdict(self.col, self.row, self.last_stance),
            Some(true)
        )
    }

    /// [`Self::console_still_body_clear`]'s verdict for a still body at `(col, row)` in `stance` (a ghost still on glass at the boundary, `Departure::own_clear`), over what the map sees of it (`PetWorld::seen_under_text_clearance_past_caret`): `None` where the covered part is clear and the rest unseen, `Some(false)` over a protected cell or with no certified map.
    #[inline]
    pub(super) const fn console_still_rect_verdict(
        &self,
        col: f32,
        row: f32,
        stance: Stance,
    ) -> Option<bool> {
        let Some(world) = self.console.world.as_ref() else {
            return Some(false);
        };
        if world.stamp().is_none() {
            return Some(false);
        }
        let rect = still_body_rect(
            (col, row),
            stance,
            (self.last_cell_w, self.last_cell_h),
            self.last_grid,
        );
        world.seen_under_text_clearance_past_caret(rect)
    }

    /// WHERE A SLEEPER MAY STRETCH: the body where the cat lies covers no ink and nothing protected the map sees ([`PetWorld::ink_clearance`], which counts as ink every cell anything protects: the host's chrome, an image, a selection, an overlay, a row it cannot see), so the stretch is drawn in front of blank ground and never under a word.
    #[inline]
    pub(super) const fn console_stretch_on_blank(&self) -> bool {
        self.console_stretch_on_blank_at((self.col, self.row))
    }

    /// [`Self::console_stretch_on_blank`] for a body at `at` (cells): the sleeper's fall judges where it falls from and where it lands.
    #[inline]
    pub(super) const fn console_stretch_on_blank_at(&self, at: (f32, f32)) -> bool {
        if !self.console.presentable {
            return true;
        }
        let Some(world) = self.console.world.as_ref() else {
            return true;
        };
        let rect = still_body_rect(
            at,
            Stance::NEUTRAL,
            (self.last_cell_w, self.last_cell_h),
            self.last_grid,
        );
        matches!(world.ink_clearance(rect, 0.0), Some(true))
    }

    /// An examined obstruction under the pet's natural body.
    #[inline]
    pub(super) fn console_obstructed(&self, sense: PetSense) -> bool {
        self.console.presentable
            && matches!(
                self.console.world.as_ref(),
                Some(world)
                    if matches!(
                        Self::stand_clearance(
                            world,
                            self.console_body(sense),
                            self.console_ground_passing(),
                        ),
                        Some(false)
                    )
            )
    }

    #[inline]
    const fn watching_stream(&self) -> bool {
        (self.watch_heat >= WATCH_GATE && !self.watch_spent) || self.vigil_armed || self.vigil
    }

    pub(super) fn reseat_unshown_console_body(&mut self, sense: PetSense, width: f32) {
        // Nothing was on glass last emission, so a first appearance or a
        // fully faded resident may start its fade at a current safe station;
        // a body the user can still see must use locomotion.
        if self.console.on_glass
            || self.console.resident
            || self.flight.is_some()
            || !self.console_obstructed(sense)
        {
            return;
        }
        if let Some((r, c)) = sense.caret {
            let (col, row) = self.station_safe((r, c), sense.cols, sense.rows, width);
            if col != self.col || row != self.row {
                self.col = col;
                self.row = row;
                self.alpha = 0.0;
                self.last_caret = None;
                self.console.hold = VisibilityHold::DEFAULT;
            }
        }
    }

    /// Read once per brain tick, before choosing between caret work and a located reading/output interest.
    pub(super) fn begin_console_tick(&mut self, sense: PetSense, width: f32) {
        let contact_changed = core::mem::take(&mut self.console.contact_changed);
        let perched_since = self.console.perched_since.take();
        let settled_since = if self.console.moving {
            sense.now
        } else {
            perched_since.unwrap_or(sense.now)
        };
        self.console.pose = None;
        self.console.still = false;
        self.console.lift = 0.0;
        self.console.clipped = false;
        self.console.attention = PetAttention::Rest;
        self.console.reason = "quiet";
        let body = self.console_body(sense);
        let Some(stamp) = self.console.world.as_ref().and_then(|w| w.stamp()) else {
            self.console.cursor_home = None;
            self.console.rest_seat.set(None);
            self.console.rest_seated.set(false);
            self.console.resident = false;
            self.console.trip = None;
            return;
        };
        let prior = self.console.tick_stamp.replace(stamp);
        let same_surface = prior.is_some_and(|p| p.surface == stamp.surface);
        let new_content = same_surface && prior.is_some_and(|p| p.content_seq != stamp.content_seq);
        if !prior.is_some_and(|previous| stamp.preserves_cursor_home(previous)) {
            self.console.cursor_home = None;
        }
        if !same_surface {
            self.console.anchor = None;
            self.console.target = None;
            self.console.rest_seat.set(None);
            self.console.rest_seated.set(false);
            self.console.trip = None;
            self.console.progress_key = None;
            self.console.progress_at = None;
            self.console.result_at = None;
            self.console.contact_at = None;
            self.console.contact_armed = true;
        }
        if !self.console.presentable {
            self.console.cursor_home = None;
            self.console.rest_seat.set(None);
            self.console.rest_seated.set(false);
            self.console.resident = false;
            self.console.trip = None;
            self.console.anchor = None;
            self.console.target = None;
            self.console.result_at = None;
            self.console.completion = None;
            self.console.contact_at = None;
            self.console.consumed = self.console.seq;
            self.console.last_body = None;
            self.console.last_seat = None;
            return;
        }
        if let Some(caret) = sense.caret.filter(|_| sense.caret_drawn) {
            self.console.cursor_home = Some(caret);
        }

        let pending_input = self
            .console
            .input
            .filter(|e| e.seq != self.console.consumed);
        self.console.consumed = self.console.seq;
        let fresh_input = pending_input
            .filter(|e| sense.now.saturating_duration_since(e.at).as_secs_f32() < INPUT_HOLD);
        if let Some(e) = fresh_input {
            self.console.source_seq = e.seq;
            self.console.anchor = None;
            self.console.target = None;
            self.console.trip = None;
            self.console.resident = false;
            self.console.result_at = None;
            self.console.completion = None;
            self.clear_play();
            self.pending_pounce = false;
            self.pending_big_jump = false;
            self.pending_pointer_pounce = None;
            self.pending_cheer = None;
            self.drop_room_cheer();
            self.pending_sulk = false;
            self.pending_vigil_pounce = false;
            self.vigil_cheer = None;
            self.quiet = 0.0;
            self.clear_trick_settle();
            self.drop_trick_go();
            self.song_left = 0;
            if self.alpha > 0.0 && self.action == PetAction::Sleep && sense.caret.is_some() {
                self.set_action(PetAction::Waking);
            }
        }
        let age = |at: Instant| sense.now.saturating_duration_since(at).as_secs_f32();
        let input_live = self.console.input.is_some_and(|e| age(e.at) < INPUT_HOLD);
        if self
            .console
            .completion
            .is_some_and(|(at, _)| age(at) >= REACTION_TTL)
            || input_live
        {
            self.console.completion = None;
            self.pending_sulk = false;
            self.pending_cheer = None;
            self.pending_vigil_pounce = false;
            self.vigil_cheer = None;
        }
        let world = self.console.world.as_ref().expect("observed above");
        let home = self.console_caret_home(sense, width);
        let selection = world.selection_target();
        if let Some(gaze) = selection {
            self.console.attention = PetAttention::Reading;
            self.console.reason = "selection";
            self.console.source_seq = self.console.observed_seq;
            self.console.gaze = Some(gaze);
            self.console.anchor = None;
            self.console.target = self.console_seat(world, body, home);
            self.console.resident = true;
            self.console.result_at = None;
            self.console.completion = None;
            self.pending_sulk = false;
            self.pending_cheer = None;
            self.drop_room_cheer();
            self.drop_room_cue();
            self.pending_vigil_pounce = false;
            self.vigil_cheer = None;
            self.console.pose = Some(if gaze.0 < self.row - 0.5 {
                PetGlyphId::PetEdgeLookUp
            } else if gaze.0 > self.row + 0.5 {
                PetGlyphId::PetEdgeLookDown
            } else {
                PetGlyphId::PetEdgePerch
            });
            self.console.still = true;
            return;
        }

        let repairing = self.console.repair_until.is_some_and(|end| sense.now < end)
            && self
                .console
                .repair_target
                .zip(sense.caret)
                .is_some_and(|((r, c), (cr, cc))| {
                    (r - f32::from(cr)).abs() < 0.5 && (c - f32::from(cc)).abs() <= 6.0
                });
        if input_live || repairing {
            self.console.resident = false;
            self.console.trip = None;
            self.console.attention = if repairing {
                PetAttention::Editing
            } else {
                PetAttention::Typing
            };
            self.console.reason = if repairing {
                "committed-edit-intent"
            } else {
                "committed-input"
            };
            self.console.gaze = self
                .console
                .repair_target
                .or_else(|| self.console.input.and_then(|e| e.target));
            self.console.pose = Some(if repairing {
                PetGlyphId::PetInspectDown
            } else {
                PetGlyphId::PetStandEar
            });
            let caret_overlaps_body = sense.caret.is_some_and(|(cr, cc)| {
                let cr = f32::from(cr);
                let cc = f32::from(cc);
                cr + 1.0 > body.row
                    && cr < body.row + body.rows
                    && cc + 1.0 > body.col
                    && cc < body.col + body.cols
            });
            let stand_pushed_past = home.is_some_and(|stand| {
                (body.row - stand.row).abs() >= 0.25
                    || if self.wall_side {
                        body.col > stand.col + ARRIVED
                    } else {
                        body.col + ARRIVED < stand.col
                    }
            });
            if repairing
                && !caret_overlaps_body
                && !stand_pushed_past
                && Self::console_may_stand(world, body, CLEARANCE, self.console_ground_passing())
                && self.flight.is_none()
            {
                self.console.target = Some(body);
                self.console.resident = true;
                self.console.still = true;
            }
            let roomy = world.clear_past_caret(body, CONTACT_MARGIN);
            if roomy
                && self
                    .console
                    .contact_at
                    .is_none_or(|at| age(at) >= CONTACT_HOLD)
            {
                self.console.contact_armed = true;
            }
            if new_content
                && contact_changed
                && !roomy
                && self.console.contact_armed
                && self
                    .console
                    .input
                    .is_some_and(|e| e.kind == PetInputKind::Text && age(e.at) < INPUT_HOLD)
            {
                self.console.contact_at = Some(sense.now);
                self.console.contact_armed = false;
            }
            if let Some(at) = self.console.contact_at
                && age(at) < CONTACT_HOLD
            {
                self.console.attention = PetAttention::Contact;
                self.console.reason = "advancing-ink";
                self.console.pose = Some(if age(at) < 0.14 {
                    if self.facing_left && world.clear_past_caret(body, CLEARANCE) {
                        PetGlyphId::PetTailTuck
                    } else {
                        PetGlyphId::PetContactBrace
                    }
                } else {
                    PetGlyphId::PetContactRecover
                });
            }
            return;
        }

        if self.room_cheer && !self.room_cheer_in_play() {
            if home.is_some_and(|stand| {
                world.under_text_clearance_past_caret(stand, CLEARANCE) == Some(false)
            }) {
                self.room_cheer_leaving = true;
            } else {
                self.room_cheer = false;
            }
        }
        if self.room_cue_act && !self.room_cue_in_play() {
            self.room_cue_act = false;
        }
        let cue_ready = self.action.settled()
            && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
            && ((self.pending_room.is_some()
                && self.room_at.is_some_and(|at| age(at) <= ROOM_CUE_TTL))
                || (self.pending_bat.is_some()
                    && self.bat_at.is_some_and(|at| age(at) <= BAT_TTL))
                || (self.pending_look.is_some()
                    && self.look_at.is_some_and(|at| age(at) <= LOOK_TTL))
                || self
                    .pending_perk
                    .is_some_and(|at| sense.now >= at && age(at) <= V2_PERK_LATE)
                || (self.catch_cool <= 0.0
                    && self
                        .pending_catch
                        .is_some_and(|star| age(star.born) <= V2_CATCH_TTL)))
            && home.is_some_and(|stand| self.caret_seat(world, body, stand) == CaretSeat::Hold);
        if self.pending_pet > 0
            || self.pet_hold_t > 0.0
            || self.pending_bell.is_some()
            || self.flinch_t > 0.0
            || self.trick_engaged()
            || self.room_cheer
            || self.room_cue_act
            || cue_ready
        {
            self.console.resident = false;
            self.console.target = None;
            self.console.trip = None;
            self.console.pose = None;
            return;
        }

        let executing = world
            .anchors()
            .iter()
            .flatten()
            .find(|a| a.state == BlockState::Executing)
            .copied();
        let completion = self
            .console
            .completion
            .take()
            .filter(|(at, _)| age(*at) < REACTION_TTL);
        if let Some((_, failed)) = completion {
            self.console.result_at = Some((sense.now, failed));
            self.console.source_seq = self.console.completion_seq;
            if failed {
                self.console.failure_quiet_until = Some(sense.now + RESULT_HOLD_DUR);
            }
        }
        if completion.is_some() || self.console.result_at.is_some() {
            self.pending_sulk = false;
            self.pending_cheer = None;
            self.pending_vigil_pounce = false;
            self.vigil_cheer = None;
        }
        let result = self
            .console
            .result_at
            .filter(|(at, _)| age(*at) < RESULT_HOLD);
        if result.is_none() {
            self.console.result_at = None;
        }

        if let Some(subject) = executing {
            let next_progress = world.progress();
            if self.console.progress_key != Some(subject.block_id) {
                self.console.progress_key = Some(subject.block_id);
                self.console.progress = next_progress;
                self.console.progress_at = None;
            } else if next_progress != self.console.progress {
                self.console.progress = next_progress;
                if matches!(
                    next_progress,
                    Some(TaskbarProgress::Normal(_) | TaskbarProgress::Error(_))
                ) {
                    self.console.progress_seq = self.console.progress_seq.wrapping_add(1).max(1);
                    self.console.progress_at = Some(sense.now);
                } else {
                    self.console.progress_at = None;
                }
            }
            let progress = self.console.progress_at.is_some_and(|at| age(at) < 0.35);
            self.console.attention = if progress {
                PetAttention::Progress
            } else {
                PetAttention::Output
            };
            self.console.reason = if progress {
                "explicit-progress-change"
            } else {
                "command-output"
            };
            self.console.source_seq = if progress {
                self.console.progress_seq
            } else {
                stamp.content_seq
            };
            let interactive_caret = sense.caret.filter(|&(cr, _)| {
                sense.caret_drawn
                    && (subject.row > f32::from(cr) + 0.5
                        || self.console.faux_caret.is_some()
                        || self.console.input.is_some_and(|e| {
                            matches!(
                                e.kind,
                                PetInputKind::Text
                                    | PetInputKind::Delete
                                    | PetInputKind::Navigate
                                    | PetInputKind::Paste
                            )
                        }))
            });
            self.console.gaze = Some(if let Some((cr, cc)) = interactive_caret {
                (f32::from(cr), f32::from(cc))
            } else {
                (subject.row, subject.col)
            });
            self.console.pose = Some(if progress {
                PetGlyphId::PetPerkTurn
            } else if interactive_caret.is_some() {
                PetGlyphId::PetSit
            } else if subject.row < self.row - 0.5 {
                PetGlyphId::PetEdgeLookUp
            } else if subject.row > self.row + 0.5 {
                PetGlyphId::PetEdgeLookDown
            } else {
                PetGlyphId::PetEdgeLean
            });
            self.choose_console_perch(subject, body, home, stamp, prior);
            self.console.resident = true;
            self.console.still = true;
        } else if let Some((_, failed)) = result {
            self.console.attention = PetAttention::Result;
            self.console.reason = if failed {
                "command-exit-failed"
            } else {
                "command-exit-success"
            };
            self.console.pose = if failed {
                Some(PetGlyphId::PetInspectDown)
            } else {
                self.console_stretch_on_blank()
                    .then_some(PetGlyphId::PetStretchHind)
            };
            self.console.target =
                Self::console_may_stand(world, body, CLEARANCE, self.console_ground_passing())
                    .then_some(body);
            self.console.resident = true;
            self.console.still = true;
        } else if let Some(anchor) = self.console.anchor.and_then(|a| {
            world.resolve(a).or_else(|| {
                world
                    .anchors()
                    .iter()
                    .flatten()
                    .find(|current| current.surface == a.surface && current.block_id == a.block_id)
                    .copied()
            })
        }) {
            // One content anchor may survive a completed command; no timer
            // grants another excursion. The perch is a visit, not a tenancy:
            // [`PERCH_DWELL`] bounds it and `console_deadline` offers its end.
            if sense
                .now
                .saturating_duration_since(settled_since)
                .as_secs_f32()
                < PERCH_DWELL
            {
                self.console.perched_since = Some(settled_since);
                self.console.attention = PetAttention::Exploring;
                self.console.reason = "content-perch";
                self.console.gaze = Some((anchor.row, anchor.col));
                self.console.pose = Some(if anchor.row < self.row - 0.5 {
                    PetGlyphId::PetEdgeLookUp
                } else if anchor.row > self.row + 0.5 {
                    PetGlyphId::PetEdgeLookDown
                } else {
                    PetGlyphId::PetEdgePerch
                });
                self.choose_console_perch(anchor, body, home, stamp, prior);
                self.console.resident = true;
                self.console.still = true;
            } else {
                self.console.resident = false;
                self.console.anchor = None;
                self.console.target = None;
                self.console.trip = None;
                self.console.reason = "perch-spent";
            }
        } else {
            self.console.resident = false;
            self.console.anchor = None;
            self.console.target = None;
            self.console.trip = None;
            self.console.progress_at = None;
            self.console.progress_key = None;
        }
    }

    fn choose_console_perch(
        &mut self,
        subject: PetAnchor,
        mut body: PetRect,
        home: Option<PetRect>,
        stamp: PetWorldStamp,
        prior: Option<PetWorldStamp>,
    ) {
        let world = self.console.world.as_ref().expect("observed");
        let retained = self
            .console
            .anchor
            .filter(|a| a.block_id == subject.block_id)
            .and_then(|a| world.resolve(a));
        if let Some(anchor) = retained
            && let Some(mut target) = self.console.target
        {
            let mut leaving_view = false;
            if let Some(old) = prior {
                let exact_scroll = old.surface == stamp.surface
                    && old.display_offset == 0
                    && stamp.display_offset == 0
                    && stamp.top_absolute_row >= old.top_absolute_row
                    && stamp.top_absolute_row - old.top_absolute_row
                        == stamp.uniform_up_rows.saturating_sub(old.uniform_up_rows);
                if exact_scroll {
                    let dy = (stamp.top_absolute_row - old.top_absolute_row) as f32;
                    target.row -= dy;
                    body.row -= dy;
                    let scrolled_off_top = self.row - dy < 0.0;
                    if !scrolled_off_top {
                        self.row -= dy;
                        if let Some(trip) = self.console.trip.as_mut() {
                            trip.from.1 -= dy;
                            trip.to.1 -= dy;
                        }
                        if let Some(look) = self.adopted_look.as_mut() {
                            look.seat.1 -= dy;
                        }
                        for d in self
                            .departures
                            .iter_mut()
                            .flatten()
                            .filter(|d| d.still && !d.resight && d.own_glass.is_none())
                        {
                            d.row -= dy;
                        }
                    }
                    leaving_view = dy > 0.0 && (target.row < 2.0 || scrolled_off_top);
                } else if old.top_absolute_row != stamp.top_absolute_row {
                    self.console.anchor = None;
                    self.console.target = None;
                    self.console.trip = None;
                    return;
                }
            }
            if Self::near_console_home(target, home)
                && world.under_text_clear(target, CLEARANCE)
                && !leaving_view
            {
                self.console.anchor = Some(anchor);
                self.console.target = Some(target);
                return;
            }
        }
        let next = self.console_seat(world, body, home);
        self.console.anchor = Some(subject);
        self.console.target = next;
        self.console.trip = None;
    }

    /// Whether ordinary locomotion still owes a flight or landing step.
    #[inline]
    #[must_use]
    pub const fn console_legacy_motion_pending(&self) -> bool {
        self.flight.is_some()
            || self.land_t > 0.0
            || self.bound2.is_some()
            || self.retired_flight_lift.is_some()
            || self.deferred_hidden_landing.is_some()
    }

    /// Whether a deferred console owner still owes its first resident tick.
    #[inline]
    #[must_use]
    pub const fn console_resident_handoff_pending(&self) -> bool {
        self.console.resident_handoff
    }

    /// The repaint hold: for up to [`HIDDEN_HOLD_MAX`] an unpainted caret (a DECTCEM hide means "I am repainting") reads as the last caret this surface painted (`ConsoleLife::cursor_home`, while the observation vouches for it), so the brain sees a painted caret that has not moved: nothing chases, a flight lands where it was going, a sleeper sleeps.
    pub(super) fn hold_unpainted_caret(&mut self, sense: PetSense, dt: f32) -> PetSense {
        if sense.caret_drawn && sense.caret.is_some() {
            self.console.hidden_hold = 0.0;
            self.console.caret_held = false;
            return sense;
        }
        self.console.hidden_hold += dt.max(0.0);
        if self.console.presentable
            && let Some(faux) = self.console.faux_caret
        {
            self.console.cursor_home = Some(faux);
            self.console.caret_held = true;
            return PetSense {
                caret: Some(faux),
                caret_drawn: true,
                ..sense
            };
        }
        let vouched = || {
            let now = self.console.world.as_ref().and_then(|w| w.stamp());
            now.zip(self.console.tick_stamp)
                .is_some_and(|(now, prior)| now.preserves_cursor_home(prior))
        };
        let corner_park = |home: (u16, u16)| {
            let now = self.console.world.as_ref().and_then(|w| w.stamp());
            now.zip(self.console.tick_stamp)
                .is_some_and(|(now, prior)| now.top_absolute_row == prior.top_absolute_row)
                && sense.caret.is_some_and(|(cr, cc)| {
                    cc == 0 && cr.saturating_add(1) >= sense.rows && (cr, cc) != home
                })
        };
        match self.console.cursor_home {
            Some(home)
                if sense.caret.is_some()
                    && self.console.presentable
                    && ((self.console.hidden_hold <= HIDDEN_HOLD_MAX && vouched())
                        || corner_park(home)) =>
            {
                self.console.caret_held = true;
                PetSense {
                    caret: Some(home),
                    caret_drawn: true,
                    ..sense
                }
            }
            _ => {
                if core::mem::take(&mut self.console.caret_held) && sense.caret.is_some() {
                    self.last_caret = None;
                }
                sense
            }
        }
    }

    /// New interests use the resident's position, distance-driven gait and authored poses.
    pub(super) fn tick_console_resident(
        &mut self,
        sense: PetSense,
        width: f32,
        dt: f32,
    ) -> Option<PetFrame> {
        let holding = self.console.presentable
            && sense.caret.is_none()
            && self.console.cursor_home.is_some()
            && self.alpha > 0.0
            && !self.console_legacy_motion_pending();
        if holding {
            if let Some((col, row)) = self.console.last_seat {
                self.col = col;
                self.row = row;
            }
            self.console.resident = true;
            self.console.target = Some(self.console_body(sense));
            self.console.trip = None;
            if self.console.attention == PetAttention::Rest {
                self.console.reason = "cursor-hidden-home";
                if self.action != PetAction::Sleep {
                    self.console.pose = Some(PetGlyphId::PetLoaf);
                }
            }
            self.console.still = true;
            self.last_caret = None;
        } else if !sense.caret_drawn && sense.caret.is_some() && self.console.presentable {
            self.console.resident = false;
            self.console.target = None;
            self.console.trip = None;
            self.console.still = false;
        }
        if !self.console.resident || !self.console.presentable {
            self.console.resident_handoff = false;
            return None;
        }
        if self.console_legacy_motion_pending() {
            self.console.resident_handoff = true;
            return None;
        }
        if matches!(
            self.console.attention,
            PetAttention::Output | PetAttention::Progress
        ) && self.watching_stream()
        {
            self.console.resident = false;
            self.console.target = None;
            self.console.trip = None;
            return None;
        }
        self.console.resident_handoff = false;
        let stroke_point = if !holding
            && !sense.reduced_motion
            && self.alpha > 0.0
            && self.last_caret.is_some()
            && sense.caret == self.last_caret
            && self.console.trip.is_none()
            && !self.console.clipped
            && matches!(
                self.console.attention,
                PetAttention::Output
                    | PetAttention::Progress
                    | PetAttention::Result
                    | PetAttention::Exploring
            ) {
            self.stroke_point(&sense, width)
        } else {
            None
        };
        if self.stroke.sample(sense.now, stroke_point, width) {
            self.note_petted(sense.now);
            self.console.resident = false;
            self.console.target = None;
            self.console.trip = None;
            self.console.pose = None;
            return None;
        }
        self.pet_hold_t = 0.0;
        self.pending_pet = 0;
        self.pet_at = None;
        let repairing = self.console.repair_until.is_some_and(|end| sense.now < end);
        let latched = self.pending_trick.filter(|_| repairing);
        self.drop_tricks();
        self.pending_trick = latched;
        let body = self.console_body(sense);
        let world = self.console.world.as_ref()?;
        let home = self.console_caret_home(sense, width);
        let caret_overlaps_body = sense.caret.is_some_and(|(cr, cc)| {
            let cr = f32::from(cr);
            let cc = f32::from(cc);
            cr + 1.0 > body.row
                && cr < body.row + body.rows
                && cc + 1.0 > body.col
                && cc < body.col + body.cols
        });
        let stand_pushed_past = |stand: PetRect| {
            (body.row - stand.row).abs() >= 0.25
                || if self.wall_side {
                    body.col > stand.col + ARRIVED
                } else {
                    body.col + ARRIVED < stand.col
                }
        };
        let claimed = matches!(self.console.attention, PetAttention::Result)
            || (self.console.attention == PetAttention::Editing
                && !caret_overlaps_body
                && home.is_none_or(|stand| !stand_pushed_past(stand)));
        let target = match home.filter(|_| sense.caret.is_some()) {
            Some(stand)
                if claimed
                    && Self::near_console_home(body, Some(stand))
                    && Self::console_may_stand(
                        world,
                        body,
                        CLEARANCE,
                        self.console_ground_passing(),
                    ) =>
            {
                Some(body)
            }
            Some(stand) => match self.caret_seat(world, body, stand) {
                CaretSeat::Hold => Some(body),
                CaretSeat::Escort if !sense.reduced_motion && self.alpha > 0.0 => {
                    self.console.resident = false;
                    self.console.target = None;
                    self.console.trip = None;
                    return None;
                }
                CaretSeat::Escort => Some(stand),
                CaretSeat::Yield => world.nearest_under_text_perch(stand, CLEARANCE, HOME_REACH),
            },
            None => self
                .console
                .target
                .filter(|r| world.under_text_clear(*r, CLEARANCE))
                .or_else(|| {
                    if Self::console_may_stand(
                        world,
                        body,
                        CLEARANCE,
                        self.console_ground_passing(),
                    ) {
                        Some(body)
                    } else {
                        world.home_perch(body, CLEARANCE, PERCH_REACH)
                    }
                }),
        };
        self.console.target = target;
        let Some(target) = target else {
            self.console.clipped = true;
            self.console.moving = false;
            self.console.attention = PetAttention::Yielding;
            self.console.reason = "protected-surface";
            self.console.trip = None;
            self.speed = 0.0;
            self.last_caret = sense.caret;
            self.last_burst = sense.output_burst;
            self.twitch_t = 0.0;
            self.stumble_t = 0.0;
            self.clear_play();
            self.pending_pounce = false;
            self.pending_big_jump = false;
            self.pending_bell = None;
            self.pending_flow = None;
            self.pending_perk = None;
            self.pending_catch = None;
            self.pending_look = None;
            self.watch_heat = 0.0;
            self.stream = false;
            self.motes = [None; PET_MOTES_MAX];
            self.land_t = 0.0;
            self.retired_flight_lift = None;
            self.drop_room_cue();
            return Some(self.emit(sense, width));
        };
        let destination = if target == body {
            (self.col, self.row)
        } else {
            (target.col, target.foot_row())
        };
        if self.alpha <= 0.0 {
            self.col = destination.0;
            self.row = destination.1;
        }
        self.alpha = if sense.reduced_motion {
            1.0
        } else {
            (self.alpha + dt * (1.0 / FADE_IN)).min(1.0)
        };
        self.last_caret = sense.caret;
        self.quiet += dt;
        let burst_edge = sense.output_burst && !self.last_burst;
        self.last_burst = sense.output_burst;
        self.twitch_t = if sense.reduced_motion {
            0.0
        } else {
            (self.twitch_t - dt).max(0.0)
        };
        self.stumble_t = 0.0;
        let prev_pointer = (
            self.last_pointer,
            self.pointer_vx,
            self.pointer_vy,
            self.pointer_speed,
            self.pointer_heat,
        );
        let prev_v2 = (self.pending_perk, self.pending_catch);
        self.clear_play();
        self.catch_cool = if sense.reduced_motion {
            0.0
        } else {
            (self.catch_cool - dt).max(0.0)
        };
        let v2_live = !holding
            && !sense.reduced_motion
            && !self.console.clipped
            && self.console.attention != PetAttention::Reading;
        let age = |at: Instant| sense.now.saturating_duration_since(at).as_secs_f32();
        self.pending_pounce = false;
        self.pending_big_jump = false;
        self.pending_bell = None;
        self.pending_flow = None;
        self.pending_perk = prev_v2.0.filter(|at| v2_live && age(*at) <= V2_PERK_LATE);
        self.pending_catch = prev_v2
            .1
            .filter(|star| v2_live && age(star.born) <= V2_CATCH_TTL);
        if !v2_live || self.room_at.is_none_or(|at| age(at) > ROOM_CUE_TTL) {
            self.drop_room_cue();
        }
        self.pending_bat = None;
        self.pending_look = None;
        self.watch_heat = 0.0;
        self.stream = false;
        self.motes = [None; PET_MOTES_MAX];
        self.land_t = 0.0;
        self.retired_flight_lift = None;
        self.deferred_hidden_landing = None;
        self.console.moving = false;

        let body = self.console_body(sense);
        let world = self.console.world.as_ref().expect("observed");
        if sense.reduced_motion {
            self.col = destination.0;
            self.row = destination.1;
            self.console.trip = None;
        } else if (self.col - destination.0)
            .abs()
            .max((self.row - destination.1).abs())
            > 0.02
        {
            if !world.under_text_corridor_clear_past_caret(body, target, CLEARANCE) {
                self.console.trip = None;
                if Self::console_may_stand(world, body, CLEARANCE, self.console_ground_passing()) {
                    self.console.target = Some(body);
                } else {
                    self.console.target = Some(target);
                    self.console.reason = "protected-displacement";
                }
            } else {
                if self.console.trip.is_some_and(|trip| trip.to != destination) {
                    self.console.trip = None;
                }
                let trip = self.console.trip.get_or_insert_with(|| {
                    let dist = (self.col - destination.0)
                        .abs()
                        .max((self.row - destination.1).abs() * ROW_AS_COLS);
                    let hop = (self.row - destination.1).abs() > 0.25;
                    PerchTrip {
                        from: (self.col, self.row),
                        to: destination,
                        elapsed: 0.0,
                        duration: (dist * (1.0 / 22.0)).max(0.22),
                        hop,
                    }
                });
                trip.elapsed += dt;
                let u = (trip.elapsed / trip.duration).clamp(0.0, 1.0);
                let s = if trip.hop { u } else { u * u * (3.0 - 2.0 * u) };
                let x = trip.from.0 + (trip.to.0 - trip.from.0) * s;
                let y = trip.from.1 + (trip.to.1 - trip.from.1) * s;
                let travelled = (x - self.col).abs();
                self.speed = if dt > 0.0 && u < 1.0 {
                    (x - self.col) / dt
                } else {
                    0.0
                };
                let amble = !trip.hop && (trip.to.0 - trip.from.0).abs() <= 2.0;
                let inv_step = if amble {
                    1.0 / LAT_STRIDE_CELLS
                } else {
                    1.0 / STRIDE_CELLS
                };
                self.col = x;
                self.row = y;
                let next_stride = self.stride + travelled * inv_step;
                self.stride = if (0.0..1024.0).contains(&next_stride) {
                    next_stride
                } else {
                    next_stride.rem_euclid(1024.0)
                };
                self.console.moving = u < 1.0;
                let arc = PetRect::new(
                    body.row.min(target.row) - 0.25,
                    body.col.min(target.col),
                    body.rows + (body.row - target.row).abs() + 0.25,
                    body.cols + (body.col - target.col).abs(),
                );
                self.console.lift =
                    if trip.hop && Self::console_may_stand(world, arc, CLEARANCE, false) {
                        0.25 * (core::f32::consts::PI * u).sin()
                    } else {
                        0.0
                    };
                if u == 1.0 {
                    self.console.trip = None;
                    self.console.lift = 0.0;
                } else {
                    self.console.pose = Some(if trip.hop {
                        if u < 0.25 {
                            PetGlyphId::PetLeapRise
                        } else if u < 0.65 {
                            PetGlyphId::PetHop
                        } else {
                            PetGlyphId::PetLeapDescend
                        }
                    } else if amble {
                        Self::CYCLE_LWALK[self.gait_index(Self::CYCLE_LWALK.len())]
                    } else {
                        Self::CYCLE_WALK[self.gait_index(Self::CYCLE_WALK.len())]
                    });
                }
            }
        } else {
            self.console.trip = None;
        }
        if !(holding && self.action == PetAction::Sleep) {
            self.set_action_keep(if self.console.moving {
                PetAction::Walk
            } else {
                PetAction::Sit
            });
        }
        if !self.console.moving {
            self.speed = 0.0;
            self.plant_stride(dt);
            if !holding
                && !sense.reduced_motion
                && !self.console.clipped
                && self.action != PetAction::Sleep
                && matches!(
                    self.console.attention,
                    PetAttention::Output | PetAttention::Exploring
                )
            {
                (
                    self.last_pointer,
                    self.pointer_vx,
                    self.pointer_vy,
                    self.pointer_speed,
                    self.pointer_heat,
                ) = prev_pointer;
                match sense.pointer {
                    Some((px, py)) if px.is_finite() && py.is_finite() => {
                        if let Some((lx, ly)) = self.last_pointer
                            && dt > 1e-4
                        {
                            let stationary = px == lx && py == ly;
                            if !stationary
                                || self.pointer_vx != 0.0
                                || self.pointer_vy != 0.0
                                || self.pointer_speed != 0.0
                            {
                                let k = 1.0 - (-dt * (1.0 / DASH_WINDOW)).exp();
                                let (vx_inst, vy_inst, inst) = if stationary {
                                    (0.0, 0.0, 0.0)
                                } else {
                                    let inv_dt = 1.0 / dt;
                                    let vx = (px - lx) * inv_dt;
                                    let vy = (py - ly) * inv_dt;
                                    (vx, vy, vx.hypot(vy))
                                };
                                self.pointer_vx += (vx_inst - self.pointer_vx) * k;
                                self.pointer_vy += (vy_inst - self.pointer_vy) * k;
                                self.pointer_speed += (inst - self.pointer_speed) * k;
                                if stationary && self.pointer_speed < 1e-5 {
                                    self.pointer_vx = 0.0;
                                    self.pointer_vy = 0.0;
                                    self.pointer_speed = 0.0;
                                }
                            }
                        }
                        self.last_pointer = Some((px, py));
                        self.pointer_heat = if self.pointer_speed >= GAZE_FOLLOW_SPEED_MIN {
                            (self.pointer_heat + dt * POINTER_RISE).min(1.0)
                        } else {
                            (self.pointer_heat - dt * POINTER_FALL).max(0.0)
                        };
                    }
                    _ => {
                        self.last_pointer = None;
                        self.pointer_vx = 0.0;
                        self.pointer_vy = 0.0;
                        self.pointer_speed = 0.0;
                        self.pointer_heat = 0.0;
                    }
                }
            }
            let pointer_gaze = (self.pointer_heat > 0.0)
                .then_some(self.last_pointer)
                .flatten()
                .map(|(px, py)| (py, px));
            if let Some((r, c)) = pointer_gaze.or(self.console.gaze) {
                self.facing_left = c < self.col + width * 0.5;
                if matches!(
                    self.console.attention,
                    PetAttention::Reading | PetAttention::Output | PetAttention::Exploring
                ) && (self.console.pose != Some(PetGlyphId::PetSit)
                    || pointer_gaze.is_some_and(|(pr, _)| (pr - self.row).abs() > 0.5))
                {
                    self.console.pose = Some(if r < self.row - 0.5 {
                        PetGlyphId::PetEdgeLookUp
                    } else if r > self.row + 0.5 {
                        PetGlyphId::PetEdgeLookDown
                    } else if self.console.pose == Some(PetGlyphId::PetSit) {
                        PetGlyphId::PetSit
                    } else if matches!(
                        self.console.attention,
                        PetAttention::Reading | PetAttention::Exploring
                    ) {
                        PetGlyphId::PetEdgePerch
                    } else {
                        PetGlyphId::PetEdgeLean
                    });
                }
            }
            if !holding
                && !sense.reduced_motion
                && !self.console.clipped
                && self.action != PetAction::Sleep
                && self.console.pose == Some(PetGlyphId::PetSit)
            {
                if burst_edge && self.twitch_t <= 0.0 {
                    self.twitch_t = TWITCH_DUR;
                    self.twitch_up = self.mote_serial.is_multiple_of(2);
                    self.mote_serial = self.mote_serial.wrapping_add(1);
                }
                if self.twitch_t > 0.0 {
                    self.console.pose = Some(if self.twitch_up {
                        PetGlyphId::PetSitEar
                    } else {
                        PetGlyphId::PetSitEarFar
                    });
                }
            } else {
                self.twitch_t = 0.0;
            }
        } else if self.speed.abs() > f32::EPSILON {
            self.twitch_t = 0.0;
            self.facing_left = self.speed < 0.0;
        } else if let Some(trip) = self.console.trip
            && (trip.to.0 - trip.from.0).abs() > f32::EPSILON
        {
            self.twitch_t = 0.0;
            self.facing_left = trip.to.0 < trip.from.0;
        } else {
            self.twitch_t = 0.0;
        }
        Some(self.emit(sense, width))
    }

    #[inline]
    const fn console_may_stand(
        world: &PetWorld,
        rect: PetRect,
        margin: f32,
        passing: bool,
    ) -> bool {
        matches!(
            Self::ground_clearance(world, rect, margin, passing),
            Some(true)
        )
    }

    /// THE ESCORT PASSES UNDER THE HOST'S LABELS.
    #[inline]
    pub(super) fn console_escorting(&self) -> bool {
        self.escorted_within(SIT_AFTER)
    }

    #[inline]
    fn escorted_within(&self, secs: f32) -> bool {
        self.typed_within(secs)
            || (self.watch_heat < WATCH_GATE
                && self
                    .rhythm_at
                    .is_some_and(|at| ((self.clock - at) as f32) < secs))
    }

    #[inline]
    fn typed_within(&self, secs: f32) -> bool {
        self.console
            .typed_at
            .zip(self.last_now)
            .is_some_and(|(at, now)| now.saturating_duration_since(at).as_secs_f32() < secs)
    }

    /// The sequence number of the last key the app noted ([`Self::note_console_input`]), when it was an Enter: one Enter, told from the next.
    #[inline]
    pub(super) const fn last_submit_seq(&self) -> Option<u64> {
        if matches!(self.console.typed_kind, Some(PetInputKind::Submit)) {
            Some(self.console.seq)
        } else {
            None
        }
    }

    /// The kind of the last key the app noted ([`Self::note_console_input`]), when it was pressed within `secs`: a letter typed is writing that goes forward before its echo has moved the caret.
    #[inline]
    pub(super) fn key_within(&self, secs: f32) -> Option<PetInputKind> {
        self.console.typed_kind.filter(|_| self.typed_within(secs))
    }

    #[inline]
    fn console_ground_passing(&self) -> bool {
        matches!(self.action, PetAction::Walk | PetAction::Run)
            || (self.escorted_within(SIT_AFTER + SIT_FOLD)
                && self
                    .last_caret
                    .is_some_and(|(cr, _)| (self.row - f32::from(cr)).abs() < 1.5))
    }

    #[inline]
    const fn ground_clearance(
        world: &PetWorld,
        rect: PetRect,
        margin: f32,
        passing: bool,
    ) -> Option<bool> {
        if passing {
            world.passing_under_text_clearance_past_caret(rect, margin)
        } else {
            world.under_text_clearance_past_caret(rect, margin)
        }
    }

    #[inline]
    const fn on_its_feet(body: PetRect) -> PetRect {
        PetRect::new(
            body.row,
            body.col,
            (body.rows - CLEARANCE).max(f32::EPSILON),
            body.cols,
        )
    }

    #[inline]
    const fn stand_clearance(world: &PetWorld, body: PetRect, passing: bool) -> Option<bool> {
        let (feet, margin) = Self::stand_rect(world, body);
        Self::ground_clearance(world, feet, margin, passing)
    }

    #[inline]
    const fn stand_rect(world: &PetWorld, body: PetRect) -> (PetRect, f32) {
        let feet = Self::on_its_feet(body);
        (feet, world.margin_in_map(feet, CLEARANCE))
    }

    #[inline]
    const fn console_must_yield(world: &PetWorld, rect: PetRect, passing: bool) -> bool {
        matches!(
            Self::ground_clearance(world, rect, 0.0, passing),
            Some(false)
        )
    }

    /// The escort's own station, corrected only where the ladder is blind.
    pub(super) fn console_station(
        &self,
        desired: (f32, f32),
        sense_dims: (u16, u16),
        width: f32,
    ) -> Option<(f32, f32)> {
        self.console.rest_seated.set(false);
        if !self.console.presentable {
            return None;
        }
        let world = self.console.world.as_ref()?;
        world.stamp()?;
        let body = Self::console_stand_rect(desired, sense_dims.0, width);
        let from = self.rest_reach_from(world, sense_dims.0, width);
        let escorting = self.console_escorting();
        let walled = from.is_some_and(|from| {
            PetWorld::same_ground(from, body) && Self::console_walled(world, from, body, escorting)
        });
        if escorting
            && !walled
            && self.flight.is_none()
            && !matches!(self.action, PetAction::Walk | PetAction::Run)
            && !self.escorted_within(RHYTHM_WINDOW)
            && (self.row - desired.1).abs() < 0.5
            && (self.col - desired.0).abs() > ARRIVED
            && Self::stand_clearance(world, body, false) == Some(false)
        {
            let here = Self::console_stand_rect((self.col, desired.1), sense_dims.0, width);
            if Self::stand_clearance(world, here, true) != Some(false) {
                return Some((self.col, desired.1));
            }
        }
        if !walled && Self::stand_clearance(world, body, escorting) != Some(false) {
            self.console.rest_seat.set(None);
            return None;
        }
        if !escorting
            && self.flight.is_none()
            && (self.row - desired.1).abs() < 0.5
            && Self::stand_clearance(world, body, true) != Some(false)
        {
            const STEP: f32 = 0.25;
            const INV_STEP: f32 = 1.0 / STEP;
            let limit = (f32::from(sense_dims.1) - width).max(0.0);
            let steps = (REST_REACH * INV_STEP) as usize;
            let here = Self::console_stand_rect((self.col, self.row), sense_dims.0, width);
            let may = |c: f32| {
                let rect = Self::console_stand_rect((c, desired.1), sense_dims.0, width);
                Self::stand_clearance(world, rect, false) == Some(true)
                    && !Self::console_walled(world, here, rect, true)
            };
            if let Some(seat) = (1..=steps)
                .flat_map(|k| [desired.0 - STEP * k as f32, desired.0 + STEP * k as f32])
                .filter(|&c| (0.0..=limit).contains(&c))
                .find(|&c| may(c))
                .map(|c| (c, desired.1))
            {
                self.console.rest_seat.set(Some(seat));
                self.console.rest_seated.set(true);
                return Some(seat);
            }
        }
        if !walled && {
            let (feet, margin) = Self::stand_rect(world, body);
            world.ground_under_text_clearance_past_caret(feet, margin) != Some(false)
        } {
            let next = world.nearest_under_text_perch(body, CLEARANCE, HOME_REACH)?;
            return Some((next.col, next.foot_row()));
        }
        let resting = (self.flight.is_none() && self.alpha > 0.0).then_some((self.col, self.row));
        let on_the_line = |row: f32| (row - desired.1).abs() < 0.5;
        let held_seat = self.console.rest_seat.get();
        if let Some((col, _)) = resting.filter(|&(_, row)| {
            on_the_line(row) && held_seat.is_none_or(|(_, seat_row)| on_the_line(seat_row))
        }) {
            let at = |c: f32| Self::console_stand_rect((c, desired.1), sense_dims.0, width);
            let may = |c: f32| {
                let rect = at(c);
                Self::stand_clearance(world, rect, escorting) == Some(true)
                    && from.is_none_or(|from| !Self::console_walled(world, from, rect, escorting))
            };
            if (desired.0 - col).abs() <= REST_REACH && may(col) {
                const STEP: f32 = 0.25;
                const INV_STEP: f32 = 1.0 / STEP;
                let toward = STEP * (col - desired.0).signum();
                let steps = ((desired.0 - col).abs() * INV_STEP).floor() as usize;
                let wait = (1..=steps)
                    .map(|k| desired.0 + toward * k as f32)
                    .find(|&c| may(c))
                    .unwrap_or(col);
                let seat = (wait, desired.1);
                self.console.rest_seat.set(Some(seat));
                self.console.rest_seated.set(true);
                return Some(seat);
            }
        }
        let pane_reach = || {
            let across = body
                .col
                .abs()
                .max((f32::from(sense_dims.1) - body.col).abs());
            (2.0 * f32::from(sense_dims.0)).hypot(across)
        };
        const REST_REACH_SQ: f32 = REST_REACH * REST_REACH;
        let keep = |seat: (f32, f32)| {
            let held = Self::console_stand_rect(seat, sense_dims.0, width);
            let dr = 2.0 * (held.row - body.row);
            let dc = held.col - body.col;
            (dr * dr + dc * dc <= REST_REACH_SQ)
                .then(|| world.nearest_clear_perch(held, CLEARANCE, SEAT_KEPT, from))
                .flatten()
        };
        let keep_any_clear = |seat: (f32, f32)| {
            let held = Self::console_stand_rect(seat, sense_dims.0, width);
            world.nearest_clear_perch(held, CLEARANCE, SEAT_KEPT, from)
        };
        let kept = held_seat.and_then(keep).or_else(|| resting.and_then(keep));
        if let Some(next) = kept
            .or_else(|| world.nearest_clear_perch(body, CLEARANCE, REST_REACH, from))
            .or_else(|| {
                held_seat
                    .and_then(keep_any_clear)
                    .or_else(|| resting.and_then(keep_any_clear))
            })
            .or_else(|| {
                // The search requires a finite reach. Cover the whole pane,
                // including a stand whose column lies outside it.
                world.nearest_clear_perch(body, CLEARANCE, pane_reach(), from)
            })
        {
            let seat = (next.col, next.foot_row());
            self.console.rest_seat.set(Some(seat));
            self.console.rest_seated.set(true);
            return Some(seat);
        }
        let keep_under = |seat: (f32, f32)| {
            let held = Self::console_stand_rect(seat, sense_dims.0, width);
            let dr = 2.0 * (held.row - body.row);
            let dc = held.col - body.col;
            (dr * dr + dc * dc <= REST_REACH_SQ)
                .then(|| world.nearest_under_text_perch_from(held, CLEARANCE, SEAT_KEPT, from))
                .flatten()
        };
        let keep_any_under = |seat: (f32, f32)| {
            let held = Self::console_stand_rect(seat, sense_dims.0, width);
            world.nearest_under_text_perch_from(held, CLEARANCE, SEAT_KEPT, from)
        };
        let next = held_seat
            .and_then(keep_under)
            .or_else(|| resting.and_then(keep_under))
            .or_else(|| world.nearest_under_text_perch_from(body, CLEARANCE, HOME_REACH, from))
            .or_else(|| {
                held_seat
                    .and_then(keep_any_under)
                    .or_else(|| resting.and_then(keep_any_under))
            })
            .or_else(|| world.nearest_under_text_perch_from(body, CLEARANCE, pane_reach(), from));
        if let Some(next) = next {
            let seat = (next.col, next.foot_row());
            self.console.rest_seat.set(Some(seat));
            self.console.rest_seated.set(true);
            return Some(seat);
        }
        self.console.rest_seat.set(None);
        None
    }

    fn rest_reach_from(&self, world: &PetWorld, rows: u16, width: f32) -> Option<PetRect> {
        if self.alpha <= 0.0 {
            return None;
        }
        let seat = self
            .flight
            .as_ref()
            .map_or((self.col, self.row), |f| (f.to_col, f.to_row));
        let body = Self::console_stand_rect(seat, rows, width);
        let judged = if self.console_ground_passing() {
            PetWorld::feet_row_of(body)
        } else {
            body
        };
        match world.ground_under_text_clearance_past_caret(judged, 0.0) {
            Some(true) => Some(body),
            Some(false) => world.nearest_under_text_perch(body, CLEARANCE, HOME_REACH),
            None => None,
        }
    }

    /// Whether a flight's aim leads by the caret's drift: a rhythm is open and the station follows the caret.
    #[inline]
    pub(super) const fn aims_ahead(&self) -> bool {
        self.rhythm_open() && !self.console.rest_seated.get()
    }

    #[inline]
    const fn console_walled(world: &PetWorld, from: PetRect, to: PetRect, passing: bool) -> bool {
        if passing {
            // The way the feet go: chrome over the ground is walked under
            // ([`Self::console_escorting`]), chrome on it or across it is a
            // wall.
            world.ground_corridor_walled(PetWorld::feet_row_of(from), PetWorld::feet_row_of(to))
        } else {
            world.ground_corridor_walled(from, to)
        }
    }

    /// Whether the console bars the escort from a stand at `desired` (`(col, foot row)`): protected ground under it, or between the body on glass and it ([`Self::console_walled`]).
    #[inline]
    pub(super) fn console_bars(&self, desired: (f32, f32), rows: u16, width: f32) -> bool {
        if !self.console.presentable {
            return false;
        }
        let Some(world) = self.console.world.as_ref().filter(|w| w.stamp().is_some()) else {
            return false;
        };
        let body = Self::console_stand_rect(desired, rows, width);
        Self::stand_clearance(world, body, self.console_escorting()) == Some(false)
            || self
                .rest_reach_from(world, rows, width)
                .is_some_and(|from| {
                    Self::console_walled(world, from, body, self.console_escorting())
                })
    }

    #[inline]
    const fn console_stand_rect(desired: (f32, f32), rows: u16, width: f32) -> PetRect {
        PetRect::new(
            (desired.1 + 1.0 - ART_ROWS).clamp(
                CLEARANCE,
                (rows as f32 - ART_ROWS - CLEARANCE).max(CLEARANCE),
            ),
            desired.0,
            ART_ROWS,
            width,
        )
    }

    fn console_caret_home(&self, sense: PetSense, width: f32) -> Option<PetRect> {
        let (r, c) = sense.caret.or(self.console.cursor_home)?;
        let want = if self.wall_side && f32::from(sense.cols) - width > 0.0 {
            (f32::from(c) - width - STATION_LEAD - self.standoff()).max(0.0)
        } else {
            Self::station_with(c, sense.cols, width, self.standoff())
        };
        let stand = self.ink_stand_for(
            want,
            f32::from(r),
            width,
            sense.cols,
            sense.rows,
            Some((r, c)),
        );
        let stand = self
            .console_station(stand, (sense.rows, sense.cols), width)
            .unwrap_or(stand);
        Some(Self::console_stand_rect(stand, sense.rows, width))
    }

    #[inline]
    fn caret_seat(&self, world: &PetWorld, body: PetRect, stand: PetRect) -> CaretSeat {
        if self.speed.abs() <= 0.01
            && self.carry_speed() < FLIP_SPEED
            && self.at_escort_stand(body, stand)
            && Self::console_may_stand(world, body, CLEARANCE, self.console_ground_passing())
        {
            CaretSeat::Hold
        } else if !Self::console_must_yield(world, stand, self.console_escorting()) {
            CaretSeat::Escort
        } else {
            CaretSeat::Yield
        }
    }

    #[inline]
    const fn at_escort_stand(&self, body: PetRect, stand: PetRect) -> bool {
        if Self::at_console_stand(body, stand) {
            return true;
        }
        let same_home = matches!(
            (self.held_for, self.console.cursor_home),
            (Some((r0, c0)), Some((r1, c1))) if r0 == r1 && c0 == c1
        );
        let lead = if !self.wall_side && !self.console.rest_seated.get() && same_home {
            self.lead()
        } else {
            0.0
        };
        lead > 0.0
            && (body.row - stand.row).abs() < 0.25
            && body.col >= stand.col - ARRIVED
            && body.col <= stand.col + lead + ARRIVED
    }

    #[inline]
    fn console_seat(
        &self,
        world: &PetWorld,
        body: PetRect,
        home: Option<PetRect>,
    ) -> Option<PetRect> {
        match home {
            Some(stand) => match self.caret_seat(world, body, stand) {
                CaretSeat::Hold => Some(body),
                CaretSeat::Escort => Some(stand),
                CaretSeat::Yield => world.nearest_under_text_perch(stand, CLEARANCE, HOME_REACH),
            },
            None if Self::console_may_stand(
                world,
                body,
                CLEARANCE,
                self.console_ground_passing(),
            ) =>
            {
                Some(body)
            }
            None => world.home_perch(body, CLEARANCE, HOME_REACH),
        }
    }

    #[inline]
    const fn at_console_stand(body: PetRect, stand: PetRect) -> bool {
        (body.col - stand.col).abs() <= ARRIVED && (body.row - stand.row).abs() < 0.25
    }

    #[inline]
    const fn near_console_home(rect: PetRect, home: Option<PetRect>) -> bool {
        match home {
            None => true,
            Some(home) => {
                (rect.col - home.col)
                    .abs()
                    .max((rect.row - home.row).abs() * 2.0)
                    <= HOME_REACH
            }
        }
    }

    fn place_console_body_at_home(&mut self, frame: &mut PetFrame, sense: PetSense) {
        if self.console.clipped || frame.alpha == 0 {
            return;
        }
        if passing(frame) {
            return;
        }
        let Some(world) = self.console.world.as_ref() else {
            return;
        };
        let Some(rect) = body_rect(frame, sense) else {
            return;
        };
        let passing = self.console_ground_passing();
        if !Self::console_must_yield(world, rect, passing) {
            return;
        }
        if (frame.scale_x != 1.0 || frame.scale_y != 1.0) && {
            let natural = still_body_rect(
                (frame.col, frame.row),
                Stance {
                    scale_x: 1.0,
                    scale_y: 1.0,
                    lift: frame.lift,
                },
                (sense.cell_w, sense.cell_h),
                (sense.cols, sense.rows),
            );
            !Self::console_must_yield(world, natural, passing)
        } {
            return;
        }
        if let Some(dy) = self.content_ride(rect) {
            let ridden = PetFrame {
                row: frame.row + dy,
                ..*frame
            };
            if body_rect(&ridden, sense).is_some_and(|r| world.under_text_clear(r, CLEARANCE)) {
                self.carry_console_body(frame, ridden, dy);
                self.console.reason = "content-ride";
                return;
            }
        }
        let Some(safe) = world.nearest_under_text_perch(rect, CLEARANCE, HOME_REACH) else {
            self.console.clipped = true;
            self.console.reason = "protected-home-unavailable";
            return;
        };
        let cw = f32::from(sense.cell_w.max(1));
        let ch = f32::from(sense.cell_h.max(1));
        let natural_h = (ART_ROWS * ch).round();
        let natural_w = ((natural_h * ART_ASPECT).round() as i32).clamp(1, i32::from(u16::MAX));
        let dest_w = (rect.cols * cw).round() as i32;
        let dest_h = (rect.rows * ch).round();
        let ground_row = ((safe.row * ch).round() + dest_h) / ch - 1.0;
        let shifted = PetFrame {
            col: ((safe.col * cw).round() - (natural_w / 2) as f32 + (dest_w / 2) as f32) / cw,
            row: ((safe.row * ch).round() + dest_h + (frame.lift * ch).round()) / ch - 1.0,
            ..*frame
        };
        if body_rect(&shifted, sense).is_none_or(|r| !world.under_text_clear(r, 0.0)) {
            self.console.clipped = true;
            self.console.reason = "protected-home-rounding";
            return;
        }
        let ground_dy = ground_row - frame.row;
        self.carry_console_body(frame, shifted, ground_dy);
        self.console.reason = "cursor-home";
    }

    /// Move the drawn body to `to` and every motion coordinate with it, or the next tick's lerp would restore the body to where it was.
    pub(super) const fn carry_console_body(
        &mut self,
        frame: &mut PetFrame,
        to: PetFrame,
        ground_dy: f32,
    ) {
        let (dx, dy) = (to.col - frame.col, to.row - frame.row);
        let old_seat = (self.col, self.row);
        *frame = to;
        self.col += dx;
        self.row += ground_dy;
        self.home += dx;
        self.col_at_tick += dx;
        self.skid_from += dx;
        if let Some(over) = &mut self.brake_over {
            *over += dx;
        }
        if let Some((tc, tr)) = &mut self.play_to
            && (*tc - old_seat.0).abs() <= 1e-4
            && (*tr - old_seat.1).abs() <= 1e-4
        {
            *tc += dx;
            *tr += ground_dy;
        }
        if let Some(look) = &mut self.adopted_look {
            look.seat.0 += dx;
            look.seat.1 += ground_dy;
        }
        let mut i = 0;
        while i < self.departures.len() {
            if let Some(record) = &mut self.departures[i]
                && record.still
                && !record.resight
            {
                record.from_col += dx;
                record.edge += dx;
                record.row += ground_dy;
                if let Some(drawn) = &mut frame.departures[i] {
                    drawn.col += dx;
                    drawn.row += dy;
                }
            }
            i += 1;
        }
        if let Some(flight) = &mut self.flight {
            flight.from_col += dx;
            flight.to_col += dx;
            flight.from_row += ground_dy;
            flight.to_row += ground_dy;
        }
        if let Some((col, row, _)) = &mut self.bound2 {
            *col += dx;
            *row += ground_dy;
        }
        if let Some((col, row)) = &mut self.deferred_hidden_landing {
            *col += dx;
            *row += ground_dy;
        }
        if let Some(target) = &mut self.console.target {
            target.col += dx;
            target.row += ground_dy;
        }
        if let Some(trip) = &mut self.console.trip {
            trip.from.0 += dx;
            trip.to.0 += dx;
            trip.from.1 += ground_dy;
            trip.to.1 += ground_dy;
        }
    }

    pub(super) fn finish_console_frame(&mut self, frame: &mut PetFrame, sense: PetSense) {
        self.console_posed = false;
        if !self.console.presentable {
            return;
        }
        if let Some(pose) = self.console.pose {
            let travelling = matches!(self.action, PetAction::Walk | PetAction::Run);
            if !self.console_legacy_motion_pending()
                && self.action != PetAction::Land
                && (self.console.resident
                    || (self.console.attention == PetAttention::Contact && !travelling))
            {
                frame.pose = self.species.skin(pose);
                self.last_pose = pose;
                frame.scale_x = 1.0;
                frame.scale_y = 1.0;
                frame.purr = 0.0;
                self.console_posed = true;
            }
        }
        if self.console.resident {
            if !self.console_legacy_motion_pending() && self.action != PetAction::Land {
                frame.lift = self.console.lift;
            }
            frame.motes = [None; PET_MOTES_MAX];
        }
        self.place_console_body_at_home(frame, sense);
        let Some(world) = self.console.world.as_ref() else {
            for (slot, live) in frame.departures.iter_mut().zip(&self.departures) {
                if live.is_some_and(|d| d.resight) {
                    *slot = None;
                }
            }
            return;
        };
        if world.stamp().is_none() {
            frame.motes = [None; PET_MOTES_MAX];
            self.console.attention = PetAttention::Yielding;
            self.console.reason = "incoherent-surface";
            if self.console.on_glass && frame.alpha > 0 {
                let cover = self.console.hold.update(sense.now, None);
                frame.alpha = cover_alpha(frame.alpha, cover);
                frame.lane_alpha = cover_alpha(frame.lane_alpha, cover);
                frame.under_ink |= self.console.z.under;
                for (slot, live) in frame.departures.iter_mut().zip(&self.departures) {
                    if live.is_some_and(|d| d.resight) {
                        *slot = None;
                    }
                }
                if frame.alpha == 0 {
                    frame.departures = [None; PET_DEPARTURES_MAX];
                }
                self.console.on_glass = frame.alpha > 0;
                return;
            }
            frame.alpha = 0;
            frame.lane_alpha = 0;
            frame.departures = [None; PET_DEPARTURES_MAX];
            self.console.clipped = true;
            self.console.hold.idle(sense.now);
            self.console.z = ZHold::default();
            return;
        }
        let body = body_rect(frame, sense);
        if let Some(rect) = body {
            self.console.last_body = Some(rect);
            self.console.last_seat = Some((self.col, self.row));
        }
        let selected = body
            .zip(world.selection_rect())
            .is_some_and(|(b, s)| b.overlaps(s));
        if self.console.clipped || selected {
            frame.alpha = 0;
            frame.lane_alpha = 0;
            self.console.hold.hide_now(sense.now);
            self.console.z = ZHold::default();
            self.console.clipped = true;
            if !self.console.reason.starts_with("protected-") {
                self.console.reason = "protected-footprint";
            }
        } else {
            let ground_passing = self.console_ground_passing();
            let verdict = body
                .filter(|_| !passing(frame))
                .map(|rect| Self::ground_clearance(world, rect, 0.0, ground_passing));
            let cover = match verdict {
                Some(verdict) => self.console.hold.update(sense.now, verdict),
                None => self.console.hold.idle(sense.now),
            };
            let verdict = verdict.flatten();
            if cover < 1.0 {
                frame.alpha = cover_alpha(frame.alpha, cover);
                frame.lane_alpha = cover_alpha(frame.lane_alpha, cover);
            }
            if verdict == Some(false) {
                if !self.console.reason.starts_with("protected-") {
                    self.console.reason = "protected-footprint";
                }
                self.console.clipped |= frame.alpha == 0;
            }
            match body.filter(|_| frame.alpha > 0) {
                Some(rect) => {
                    let ink = world.ink_clearance(rect, 0.0) != Some(true);
                    frame.under_ink |= self.console.z.update(sense.now, ink);
                }
                None => self.console.z = ZHold::default(),
            }
        }
        if frame.alpha == 0 {
            frame.motes = [None; PET_MOTES_MAX];
            frame.departures = [None; PET_DEPARTURES_MAX];
        }
        self.console.on_glass = frame.alpha > 0;
        if frame.motes.iter().any(Option::is_some) {
            let min_ch = f32::from(sense.cell_h.max(6));
            let inv_cw = 1.0 / f32::from(sense.cell_w.max(1));
            let inv_ch = 1.0 / f32::from(sense.cell_h.max(1));
            for mote in &mut frame.motes {
                if mote.is_some_and(|m| {
                    let radius = min_ch * m.scale + 1.0;
                    let rw = radius * inv_cw;
                    let rh = radius * inv_ch;
                    !world.clear(
                        PetRect::new(m.row - rh, m.col - rw, 2.0 * rh, 2.0 * rw),
                        CLEARANCE,
                    )
                }) {
                    *mote = None;
                }
            }
        }
        if frame.departures.iter().any(Option::is_some) {
            let base_lane_alpha = frame.lane_alpha;
            let selection = world.selection_rect();
            let mut refused = 0u16;
            let mut handed = 0.0f32;
            for (departure, live) in frame.departures.iter_mut().zip(self.departures.iter_mut()) {
                let Some(d) = departure.as_mut() else {
                    continue;
                };
                if live.is_some_and(|l| l.own_glass.is_some()) {
                    d.under_ink = true;
                    continue;
                }
                let ghost_rect =
                    (d.alpha != 0 && sense.cell_w != 0 && sense.cell_h != 0).then(|| {
                        still_body_rect(
                            (d.col, d.row),
                            Stance {
                                scale_x: d.scale_x,
                                scale_y: d.scale_y,
                                lift: d.lift,
                            },
                            (sense.cell_w, sense.cell_h),
                            (sense.cols, sense.rows),
                        )
                    });
                let keep = match ghost_rect {
                    None => false,
                    Some(rect) if selection.is_some_and(|s| rect.overlaps(s)) => false,
                    Some(rect) if live.is_some_and(|l| l.resight) => {
                        d.under_ink = world.ink_clearance(rect, 0.0) != Some(true);
                        world
                            .seen_under_text_clearance_past_caret(rect)
                            .unwrap_or_else(|| live.is_some_and(|l| l.own_clear))
                    }
                    Some(_) => true,
                };
                if !keep {
                    if let Some(l) = live.take_if(|l| l.resight) {
                        refused += u16::from(d.alpha) * u16::from(base_lane_alpha) / 255;
                        handed += l.envelope((self.lane_clock - l.born) as f32);
                    }
                    *departure = None;
                }
            }
            if refused > 0 {
                frame.alpha =
                    (u16::from(frame.alpha) + refused).min(u16::from(frame.lane_alpha)) as u8;
            }
            if handed > 0.0 {
                let remaining = self.arrive_t / self.arrive_in;
                if remaining > 0.0 {
                    self.arrive_from = (self.arrive_from + handed / remaining).min(1.0);
                }
            }
        }
    }

    #[inline]
    pub(super) const fn console_hold_settled(&self) -> bool {
        self.console.hold.settled()
    }

    pub(super) const fn console_frames(&self) -> Option<bool> {
        if !self.console.presentable || self.console_legacy_motion_pending() {
            return None;
        }
        if !self.console.hold.settled() {
            return Some(true);
        }
        if self.arrive_t > 0.0 {
            return Some(true);
        }
        let mut i = 0;
        while i < self.departures.len() {
            if self.departures[i].is_some() {
                return Some(true);
            }
            i += 1;
        }
        if !self.console.resident {
            return None;
        }
        Some(
            self.console.resident_handoff
                || self.console.completion.is_some()
                || self.console.seq != self.console.consumed
                || self.pending_pet > 0
                || self.pending_bell.is_some()
                || self.pending_bat.is_some()
                || self.pending_look.is_some()
                || (!self.console.clipped
                    && (self.console.moving
                        || self.alpha < 1.0
                        || self.pointer_heat > 0.0
                        || self.twitch_t > 0.0)),
        )
    }

    pub(super) fn console_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.console.presentable {
            return None;
        }
        let input = self.console.input.map(|e| e.at + INPUT_HOLD_DUR);
        let contact_recover = self
            .console
            .contact_at
            .map(|at| at + Duration::from_millis(140));
        let contact = self.console.contact_at.map(|at| at + CONTACT_HOLD_DUR);
        let result = self.console.result_at.map(|(at, _)| at + RESULT_HOLD_DUR);
        let progress = self
            .console
            .progress_at
            .map(|at| at + Duration::from_millis(350));
        let perch = self.console.perched_since.map(|at| at + PERCH_DWELL_DUR);
        let floor = self.last_now.unwrap_or(now);
        [
            input,
            contact_recover,
            contact,
            result,
            progress,
            perch,
            self.console.z.deadline(),
            self.console.repair_until,
            self.console.failure_quiet_until,
        ]
        .into_iter()
        .flatten()
        .filter(|at| *at > floor)
        .min()
        .map(|at| at.max(now + ARM_MIN))
    }

    #[inline]
    pub(super) fn note_console_completion(&mut self, now: Instant, failed: bool) {
        self.console.completion_seq = self.console.completion_seq.wrapping_add(1).max(1);
        self.console.completion = Some((now, failed));
        self.console.failure_quiet_until = failed.then(|| now + RESULT_HOLD_DUR);
    }

    #[inline]
    pub(super) fn console_grieving(&self) -> bool {
        self.console
            .failure_quiet_until
            .zip(self.last_now)
            .is_some_and(|(until, now)| now < until)
    }
}

#[inline]
const fn still_body_rect(
    seat: (f32, f32),
    stance: Stance,
    cell: (u16, u16),
    grid: (u16, u16),
) -> PetRect {
    let cw = if cell.0 > 1 { cell.0 } else { 1 };
    let ch = if cell.1 > 1 { cell.1 } else { 1 };
    let (x0, x1, y0, y1) = drawn_body_px(seat, stance, (cw, ch), grid);
    let (cw, ch) = (cw as f32, ch as f32);
    PetRect::new(
        y0 as f32 / ch,
        x0 as f32 / cw,
        (y1 - y0) as f32 / ch,
        (x1 - x0) as f32 / cw,
    )
}

#[inline]
const fn body_rect(frame: &PetFrame, sense: PetSense) -> Option<PetRect> {
    let Some((x0, x1, y0, y1)) = frame.body_px(sense.cell_w, sense.cell_h, sense.cols, sense.rows)
    else {
        return None;
    };
    let cw = (if sense.cell_w > 1 { sense.cell_w } else { 1 }) as f32;
    let ch = (if sense.cell_h > 1 { sense.cell_h } else { 1 }) as f32;
    Some(PetRect::new(
        y0 as f32 / ch,
        x0 as f32 / cw,
        (y1 - y0) as f32 / ch,
        (x1 - x0) as f32 / cw,
    ))
}

#[inline]
const fn typed_run(
    input: &aterm_core::render::RenderInput,
    pane: PetPane,
    last: Option<(u16, u16)>,
) -> Option<(usize, usize, usize)> {
    if input.display_offset != 0 {
        return None;
    }
    let Some(row) = input.cursor_row.checked_sub(pane.row) else {
        return None;
    };
    let Some(col) = input.cursor_col.checked_sub(pane.col) else {
        return None;
    };
    if !input.cursor_visible {
        let Some((r, c)) = last else {
            return None;
        };
        let r = r as usize;
        let c = c as usize;
        if r == row && c < col && col - c <= TYPED_RUN_MAX {
            return Some((row, c, col + 1));
        }
        return None;
    }
    let first = match last {
        Some((r, c)) if (r as usize) == row && (c as usize) < col => c as usize,
        _ => col.saturating_sub(1),
    };
    let floor = col.saturating_sub(TYPED_RUN_MAX);
    Some((row, if first > floor { first } else { floor }, col))
}

#[inline]
const fn local_ink(
    world: &PetWorld,
    body: PetRect,
    typed: Option<(usize, usize, usize)>,
) -> Option<[u64; 8]> {
    if world.stamp().is_none() {
        return None;
    }
    let r0 = (body.row - CONTACT_MARGIN).floor().max(0.0) as usize;
    let c0 = (body.col - CONTACT_MARGIN).floor().max(0.0) as usize;
    let r1 = (body.row + body.rows + CONTACT_MARGIN).ceil() as usize;
    let c1 = (body.col + body.cols + CONTACT_MARGIN).ceil() as usize;
    if r1.saturating_sub(r0) > 16 || c1.saturating_sub(c0) > 32 {
        return None;
    }
    let mut bits = [0; 8];
    let mut r = r0;
    while r < r1 {
        let dr = r - r0;
        let Some(raw_bits) = world.ink_row_bits(r, c0, c1) else {
            return None;
        };
        let mut row_bits = raw_bits as u64;
        if let Some((row, first, end)) = typed
            && row == r
        {
            let t0 = if first > c0 { first } else { c0 };
            let t1 = if end < c1 { end } else { c1 };
            if t1 > t0 {
                row_bits &= !(((1u64 << (t1 - t0)) - 1) << (t0 - c0));
            }
        }
        bits[dr >> 1] |= row_bits << ((dr & 1) << 5);
        r += 1;
    }
    Some(bits)
}

#[inline]
const fn bg_eq(a: [u8; 3], b: [u8; 3]) -> bool {
    a[0] == b[0] && a[1] == b[1] && a[2] == b[2]
}

const fn detect_faux_composer_caret(
    input: &aterm_core::render::RenderInput,
    pane: PetPane,
) -> Option<(u16, u16)> {
    let start_r = pane.rows / 2;
    let max_end_r = pane.rows.saturating_sub(1);
    let sub_r = input.cursor_row.saturating_sub(pane.row);
    let end_r = if sub_r < max_end_r { sub_r } else { max_end_r };
    let cells = input.cells.as_slice();
    let mut local_r = end_r;
    while local_r > start_r {
        local_r -= 1;
        let frame_r = pane.row + local_r;
        if frame_r >= cells.len() {
            continue;
        }
        let row = cells[frame_r].as_slice();
        let mut prompt_c = None;
        let mut boxed = false;
        let scan_cols = if pane.cols < 6 { pane.cols } else { 6 };
        let mut local_c = 0;
        while local_c < scan_cols {
            let idx = pane.col + local_c;
            if idx >= row.len() {
                break;
            }
            match row[idx].ch {
                '│' | '┃' | '║' | '|' if local_c == 0 => boxed = true,
                ' ' => {}
                '>' | '❯' | '›' => {
                    prompt_c = Some(local_c);
                    break;
                }
                _ => break,
            }
            local_c += 1;
        }
        let Some(prompt_c) = prompt_c else {
            continue;
        };
        let prompt_idx = pane.col + prompt_c;
        let base_bg = if prompt_idx < row.len() {
            Some(row[prompt_idx].bg)
        } else {
            None
        };
        let mut last_text: Option<(usize, usize, usize)> = None;
        let mut comp_r = local_r;
        while comp_r < end_r {
            let comp_frame_r = pane.row + comp_r;
            if comp_frame_r >= cells.len() {
                break;
            }
            let comp_row = cells[comp_frame_r].as_slice();
            if pane.col >= comp_row.len() {
                break;
            }
            let first_cell = &comp_row[pane.col];
            if matches!(first_cell.ch, '╰' | '└' | '╭' | '┌' | '─' | '━' | '═' | '+')
            {
                break;
            }
            if comp_r > local_r {
                if boxed {
                    if !matches!(first_cell.ch, '│' | '┃' | '║' | '|') {
                        break;
                    }
                } else {
                    let mut indented = true;
                    let mut c = 0;
                    while c <= prompt_c {
                        let idx = pane.col + c;
                        if idx >= comp_row.len() || comp_row[idx].ch != ' ' {
                            indented = false;
                            break;
                        }
                        c += 1;
                    }
                    if !indented {
                        break;
                    }
                }
            }
            let mut right_limit = pane.cols.saturating_sub(1);
            if boxed {
                let mut c = pane.cols;
                while c > prompt_c + 1 {
                    c -= 1;
                    let idx = pane.col + c;
                    if idx >= comp_row.len() {
                        continue;
                    }
                    let ch = comp_row[idx].ch;
                    if ch == ' ' {
                        continue;
                    }
                    if matches!(ch, '│' | '┃' | '║' | '|') {
                        right_limit = c;
                    }
                    break;
                }
            }
            let start_c = prompt_c + 1;
            let mut row_has_ink = false;
            let mut scan_c = start_c;
            while scan_c < right_limit {
                let idx = pane.col + scan_c;
                if idx >= comp_row.len() {
                    break;
                }
                let cell = &comp_row[idx];
                let diff_base = match base_bg {
                    Some(bg) => !bg_eq(cell.bg, bg),
                    None => false,
                };
                let diff_prev = idx == 0 || !bg_eq(comp_row[idx - 1].bg, cell.bg);
                let diff_next = idx + 1 >= comp_row.len() || !bg_eq(comp_row[idx + 1].bg, cell.bg);
                let inverse_caret = diff_base && diff_prev && diff_next;
                if cell.ch == '█' || inverse_caret {
                    return Some((comp_r as u16, scan_c as u16));
                }
                if cell.ch != ' ' && cell.ch != '\0' {
                    row_has_ink = true;
                    last_text = Some((comp_r, scan_c, right_limit));
                }
                scan_c += 1;
            }
            if !boxed && !row_has_ink {
                break;
            }
            comp_r += 1;
        }
        if boxed {
            if let Some((tr, tc, right_limit)) = last_text {
                let limit_m1 = right_limit.saturating_sub(1);
                let next_c = tc + 1;
                let col = if next_c < limit_m1 { next_c } else { limit_m1 };
                return Some((tr as u16, col as u16));
            }
            let limit_m1 = pane.cols.saturating_sub(1);
            let next_c = prompt_c + 2;
            let col = if next_c < limit_m1 { next_c } else { limit_m1 };
            return Some((local_r as u16, col as u16));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pet_world::PetCell;
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_core::terminal::Terminal;

    struct Scene {
        term: Terminal,
        pet: PetBrain,
        now: Instant,
    }

    impl Scene {
        fn new() -> Self {
            let mut term = Terminal::new(20, 80);
            term.process(b"\x1b]11;#000000\x07\x1b[6;17H");
            let now = Instant::now();
            let pet = PetBrain {
                col: 20.0,
                row: 5.0,
                alpha: 1.0,
                action: PetAction::Sit,
                quiet: 2.0,
                last_caret: Some((5, 16)),
                last_now: Some(now),
                ..PetBrain::default()
            };
            let mut scene = Self { term, pet, now };
            scene.frame(0.0);
            scene
        }

        fn frame(&mut self, seconds: f32) -> PetFrame {
            self.now += Duration::from_secs_f32(seconds);
            let input = self.term.cell_frame(20, 80);
            let facts = PetWorldFacts::read(&self.term, 17);
            self.pet
                .observe_console(&input, &facts, PetPane::full(&input));
            self.pet.set_console_presentable(true);
            let c = self.term.cursor();
            self.pet.tick(PetSense {
                caret_drawn: true,
                now: self.now,
                caret: (self.term.cursor_visible() && self.term.grid().display_offset() == 0)
                    .then_some((c.row, c.col)),
                wrapped: false,
                rows: 20,
                cols: 80,
                cell_w: 10,
                cell_h: 20,
                reduced_motion: false,
                output_burst: false,
                pointer: None,
            })
        }

        fn input(&mut self, kind: PetInputKind) {
            self.pet.note_console_input(self.now, kind);
        }
    }

    #[test]
    fn contact_requires_new_local_ink_not_a_redraw_or_body_motion() {
        let mut s = Scene::new();
        s.pet.col = 20.0;
        s.pet.held_lead = 3.0;
        s.pet.console.last_body = Some(PetRect::new(4.3, 20.0, 1.7, 5.6));
        s.input(PetInputKind::Text);
        s.term.process(b"\x1b[15;3Hunrelated\x1b[6;17H");
        s.frame(0.016);
        assert_ne!(s.pet.console_attention(), PetAttention::Contact);
        s.pet.col = 20.0;
        s.pet.console.last_body = Some(PetRect::new(4.3, 20.0, 1.7, 5.6));
        s.input(PetInputKind::Text);
        s.term.process(b"\x1b[6;20HX\x1b[6;17H");
        let f = s.frame(0.016);
        assert_eq!(s.pet.console_attention(), PetAttention::Contact);
        let f = if matches!(
            f.pose,
            PetGlyphId::PetContactBrace | PetGlyphId::PetTailTuck
        ) {
            f
        } else {
            s.frame(0.016)
        };
        assert!(matches!(
            f.pose,
            PetGlyphId::PetContactBrace | PetGlyphId::PetTailTuck
        ));
        let at = s.pet.console.contact_at;
        assert_eq!(
            s.pet.console_deadline(s.now),
            at.map(|time| time + Duration::from_millis(140)),
            "the recovery pose has its own finite wake edge"
        );
        s.frame(0.016);
        assert_eq!(
            s.pet.console.contact_at, at,
            "same ink never restarts contact"
        );
        s.frame(0.20);
        assert_eq!(s.pet.last_pose, PetGlyphId::PetContactRecover);
        s.frame(1.0);
        assert_ne!(s.pet.console_attention(), PetAttention::Contact);

        let mut term = Terminal::new(4, 12);
        term.set_default_background(aterm_types::Rgb { r: 0, g: 0, b: 0 });
        term.set_palette_color_components(1, 255, 0, 0);
        let clock = aterm_core::terminal::ClockReading {
            monotonic: s.now,
            wall_ms: Some(1_000),
        };
        term.process_at(
            "abc \u{05D0}\u{05D1}\u{05D2}\x1b[3;1H│> \x1b[41m \x1b[0mabc   │\x1b[4;1H\x1b[?25l"
                .as_bytes(),
            clock,
        );
        term.process_at(b"\x1b[2 k", clock);
        let rtl = term.cell_frame(4, 12);
        let before_facts = PetWorldFacts::read(&term, 17);
        let mut pet = PetBrain::default();
        pet.observe_console(&rtl, &before_facts, PetPane::full(&rtl));
        let before = pet.console.row_sig.clone();
        assert_eq!(pet.console.faux_caret, Some((2, 3)));

        term.process_at(b"\x1b[1 k", clock);
        let ltr = term.cell_frame(4, 12);
        let after_facts = PetWorldFacts::read(&term, 17);
        assert_eq!(before_facts.stamp, after_facts.stamp);
        assert_ne!(after_facts.stamp.content_seq, 0);
        let chars = |input: &aterm_core::render::RenderInput| {
            input.cells[0]
                .iter()
                .map(|cell| cell.ch)
                .collect::<Vec<_>>()
        };
        assert_ne!(chars(&rtl), chars(&ltr), "SCP reprojects the same row");
        pet.observe_console(&ltr, &after_facts, PetPane::full(&ltr));
        assert_ne!(pet.console.row_sig, before, "changed glass refreshes rows");
        assert_eq!(pet.console.row_sig_prev, before);
        let after = pet.console.row_sig.clone();
        pet.observe_console(&ltr, &after_facts, PetPane::full(&ltr));
        assert_eq!(pet.console.row_sig, after);
        assert_eq!(
            pet.console.row_sig_prev, before,
            "a duplicate read retains the previous different rows"
        );

        assert_ne!(ltr.cells[2][3].bg, ltr.cells[2][1].bg);
        term.set_palette_color(1, term.default_background());
        let recoloured = term.cell_frame(4, 12);
        let recoloured_facts = PetWorldFacts::read(&term, 17);
        assert_eq!(after_facts.stamp, recoloured_facts.stamp);
        assert_eq!(recoloured.cells[2][3].bg, recoloured.cells[2][1].bg);
        pet.observe_console(&recoloured, &recoloured_facts, PetPane::full(&recoloured));
        assert_eq!(pet.console.row_sig, after);
        assert_eq!(
            pet.console.faux_caret,
            Some((2, 7)),
            "palette changes revalidate the painted caret"
        );

        let pane = PetPane {
            row: 2,
            col: 0,
            rows: 2,
            cols: 12,
        };
        let mut pane_facts = PetWorldFacts::read(&term, 17);
        pane_facts.stamp.surface.rows = pane.rows;
        for (available, signatures) in [(4, 2), (3, 1), (1, 0), (2, 0), (0, 0), (4, 2)] {
            let mut input = term.cell_frame(4, 12);
            input.cells.truncate(available);
            for _ in 0..2 {
                pet.observe_console(&input, &pane_facts, pane);
                assert_eq!(pet.console.row_sig.len(), signatures);
                let world = pet.console_world().unwrap();
                assert_eq!(world.stamp(), Some(pane_facts.stamp));
                if available < 4 {
                    assert_eq!(world.cell(1, 0), PetCell::Unknown);
                }
                if available < 3 {
                    assert_eq!(world.cell(0, 0), PetCell::Unknown);
                    assert!(pet.console.row_sig_prev.is_empty());
                }
            }
        }
    }

    #[test]
    fn a_walking_body_wears_its_gait_through_a_contact() {
        let mut s = Scene::new();
        s.pet.col = 20.0;
        s.pet.console.last_body = Some(PetRect::new(4.3, 20.0, 1.7, 5.6));
        s.input(PetInputKind::Text);
        s.term.process(b"\x1b[6;20HX\x1b[6;17H");
        let f = s.frame(0.016);
        assert_eq!(s.pet.console_attention(), PetAttention::Contact);
        let gait = |p: PetGlyphId| {
            matches!(
                p,
                PetGlyphId::PetWalk0
                    | PetGlyphId::PetWalk1
                    | PetGlyphId::PetWalk2
                    | PetGlyphId::PetWalk3
                    | PetGlyphId::PetLwalk0
                    | PetGlyphId::PetLwalk1
                    | PetGlyphId::PetLwalk2
                    | PetGlyphId::PetLwalk3
                    | PetGlyphId::PetRun0
                    | PetGlyphId::PetRun1
                    | PetGlyphId::PetRun2
                    | PetGlyphId::PetRun3
            )
        };
        assert_eq!(f.action, PetAction::Walk, "fixture: the chase walks it");
        assert!(
            gait(f.pose),
            "a walking body wears its gait, not {:?}",
            f.pose
        );
        for _ in 0..20 {
            let f = s.frame(0.016);
            if matches!(f.action, PetAction::Walk | PetAction::Run) {
                assert!(gait(f.pose), "through the hold too, not {:?}", f.pose);
            }
        }
    }

    #[test]
    fn repair_holds_its_body_in_the_inspect_pose() {
        let mut s = Scene::new();
        s.input(PetInputKind::Delete);
        s.term.process(b"\x1b[6;15H");
        let inspect = s.frame(0.016);
        assert_eq!(s.pet.console_attention(), PetAttention::Editing);
        assert_eq!(inspect.pose, PetGlyphId::PetInspectDown);
        let before = (inspect.col, inspect.row);
        s.frame(0.016);
        assert_eq!((s.pet.col, s.pet.row), before);
    }

    #[test]
    fn held_selection_has_real_presence_without_a_caret_and_stops_waking() {
        let mut s = Scene::new();
        s.term.process(b"\x1b[3;3Hread this\x1b[?25l");
        s.term.text_selection_mut().start_selection(
            2,
            2,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        s.term
            .text_selection_mut()
            .update_selection(2, 10, SelectionSide::Right);
        let first = s.frame(0.016);
        assert_eq!(s.pet.console_attention(), PetAttention::Reading);
        assert!(first.alpha > 0);
        assert!(
            s.pet.last_caret.is_none(),
            "selection never fabricates a cursor"
        );
        let next = s.frame(2.0);
        assert_eq!(
            first.fp(),
            next.fp(),
            "stable reading has no background performance"
        );
        assert!(!s.pet.needs_frames());
        assert!(s.pet.next_change_deadline(s.now).is_none());
    }

    #[test]
    fn an_incoherent_observation_never_falls_back_to_uncertified_legacy_placement() {
        let incoherent = |s: &mut Scene, ms: u64| {
            let input = s.term.cell_frame(20, 80);
            s.term.process(b"changed");
            let facts = PetWorldFacts::read(&s.term, 17);
            s.pet.observe_console(&input, &facts, PetPane::full(&input));
            s.now += Duration::from_millis(ms);
            s.pet.tick(PetSense {
                caret_drawn: true,
                now: s.now,
                caret: Some((5, 23)),
                wrapped: false,
                rows: 20,
                cols: 80,
                cell_w: 10,
                cell_h: 20,
                reduced_motion: false,
                output_burst: false,
                pointer: None,
            })
        };
        let mut s = Scene::new();
        let before = s.frame(0.016);
        assert_eq!(before.alpha, 255, "fixture: the resident is on glass");
        let f = incoherent(&mut s, 16);
        assert_eq!(s.pet.console_reason(), "incoherent-surface");
        assert_eq!(
            (f.alpha, f.lane_alpha),
            (before.alpha, before.lane_alpha),
            "the body on glass is held, not cut"
        );
        assert_eq!(
            (f.col, f.row),
            (before.col, before.row),
            "…exactly where the last certified frame judged it"
        );
        let mut last = f;
        for _ in 0..80 {
            last = incoherent(&mut s, 16);
        }
        assert_eq!(last.alpha, 0, "an unbroken blind run fades the body out");
        assert_eq!((last.col, last.row), (before.col, before.row));

        let mut cold = Scene::new();
        cold.pet.console.on_glass = false;
        let f = incoherent(&mut cold, 16);
        assert_eq!((f.alpha, f.lane_alpha), (0, 0));
        assert_eq!(cold.pet.console_reason(), "incoherent-surface");
    }

    #[test]
    fn progress_baselines_each_command_and_duplicate_reports_are_not_events() {
        let mut s = Scene::new();
        s.term.process(b"\x1b]9;4;1;100\x07\x1b[8;1H\x1b]133;A\x07> \x1b]133;B\x07build\r\n\x1b]133;C\x07working");
        s.frame(0.1);
        assert_eq!(
            s.pet.console_attention(),
            PetAttention::Output,
            "old100 is a baseline, not completion"
        );
        s.term.process(b"\x1b]9;4;1;20\x07");
        s.frame(0.1);
        assert_eq!(s.pet.console_attention(), PetAttention::Progress);
        let at = s.pet.console.progress_at;
        s.term.process(b"\x1b]9;4;1;20\x07");
        s.frame(0.1);
        assert_eq!(s.pet.console.progress_at, at);
        s.frame(0.4);
        assert_eq!(s.pet.console_attention(), PetAttention::Output);
        assert_ne!(s.pet.console_attention(), PetAttention::Result);
    }

    #[test]
    fn suppressed_failure_still_hushes_celebration_for_a_finite_window() {
        let mut s = Scene::new();
        s.input(PetInputKind::Text);
        s.pet.note_command_done(s.now, true, Some(100));
        s.frame(0.016);
        assert_eq!(s.pet.console_attention(), PetAttention::Typing);
        assert!(s.pet.grieving(), "no celebration over a fresh failed exit");
        s.frame(1.3);
        assert!(
            !s.pet.grieving(),
            "suppressed reactions leave no unbounded hush"
        );
        assert_ne!(s.pet.console_attention(), PetAttention::Result);
    }

    #[test]
    fn a_spent_content_perch_hands_the_pet_back_its_own_life() {
        let mut s = Scene::new();
        s.term
            .process(b"\x1b[8;1H\x1b]133;A\x07> \x1b]133;B\x07build\r\n\x1b]133;C\x07working");
        s.frame(0.1);
        assert_eq!(s.pet.console_attention(), PetAttention::Output);
        s.term.process(b"\x1b]133;D;0\x07");
        s.frame(0.1);
        assert_eq!(
            s.pet.console_attention(),
            PetAttention::Exploring,
            "fixture: the surviving anchor is the content perch"
        );
        for _ in 0..240 {
            s.frame(0.016);
            if !s.pet.console.moving && !s.pet.needs_frames() {
                break;
            }
        }
        assert!(
            s.pet.needs_frames() || s.pet.next_change_deadline(s.now).is_some(),
            "a resident with nothing left to react to must still own a future"
        );
        for _ in 0..((PERCH_DWELL / 0.016).ceil() as usize + 2) {
            s.frame(0.016);
        }
        assert_eq!(
            s.pet.console_attention(),
            PetAttention::Rest,
            "a spent perch is released, not held forever"
        );
        assert!(
            !s.pet.console.resident,
            "a released perch is no longer a console resident"
        );
    }

    #[test]
    fn no_perch_in_reach_is_not_an_opinion_about_where_the_pet_belongs() {
        let mut s = Scene::new();
        for row in 1..=20 {
            let line = format!("\x1b[{row};1H\x1b#6{}", "X".repeat(40));
            s.term.process(line.as_bytes());
        }
        s.term.process(b"\x1b[6;17H");
        s.frame(0.016);
        let width = art_cols(10, 20);
        assert_eq!(
            s.pet.console_station((70.0, 5.0), (20, 80), width),
            None,
            "a dense screen offers no perch, and that is not a station"
        );
        let here = (s.pet.col, s.pet.row);
        assert_ne!(
            s.pet.station_safe((5, 70), 80, 20, width),
            here,
            "the baseline escort must still answer when the console cannot"
        );

        for row in [15, 16] {
            let line = format!("\x1b[{row};1H\x1b#5\x1b[2K\x1b[{row};9H{}", "X".repeat(72));
            s.term.process(line.as_bytes());
        }
        s.term.process(b"\x1b[6;17H");
        s.frame(0.016);
        s.pet.alpha = 0.0;
        s.pet.flight = None;
        s.pet.console.rest_seat.set(None);
        let desired = (70.0, 5.0);
        let body = PetBrain::console_stand_rect(desired, 20, width);
        let world = s.pet.console.world.as_ref().expect("observed pane");
        assert_eq!(
            world.nearest_clear_perch(body, CLEARANCE, REST_REACH, None),
            None,
            "the nearby search cannot reach the clear strip"
        );
        let seat = s
            .pet
            .console_station(desired, (20, 80), width)
            .expect("clear ground elsewhere in the pane");
        let seated = PetBrain::console_stand_rect(seat, 20, width);
        assert!(
            (2.0 * (seated.row - body.row)).hypot(seated.col - body.col) > REST_REACH,
            "the fallback reaches beyond the nearby search: {seated:?}"
        );
        assert!(
            world.ground_clear(seated, CLEARANCE),
            "the distant seat must still be certified clear ground"
        );
    }

    #[test]
    fn a_stream_the_cat_is_staring_at_is_not_taken_by_the_output_hold() {
        let mut s = Scene::new();
        s.executing();
        s.pet.watch_heat = 1.0;
        let frames = s.watch(0.3);
        assert!(
            s.pet.watch_heat >= WATCH_GATE,
            "the heat only cools, it is not taken: {:.2}",
            s.pet.watch_heat
        );
        assert!(
            frames.iter().all(|f| f.action == PetAction::Perk),
            "perked at the stream throughout: {:?}",
            frames.iter().map(|f| f.action).collect::<Vec<_>>()
        );
        assert!(!s.pet.console.resident, "the hold stood aside");
    }

    #[test]
    fn a_late_wake_read_still_offers_the_unconsumed_expiry() {
        let mut s = Scene::new();
        s.input(PetInputKind::Delete);
        s.frame(0.016);
        assert_eq!(s.pet.console_attention(), PetAttention::Editing);
        let late = s.now + Duration::from_secs(5);
        let deadline = s
            .pet
            .next_change_deadline(late)
            .expect("expired pose still owes one tick");
        assert!(deadline >= late && deadline <= late + Duration::from_millis(20));
        s.frame(5.0);
        assert_ne!(s.pet.console_attention(), PetAttention::Editing);
    }

    fn hidden_home_continuity_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            HiddenCursorHomeContinuity {
                const Buggy = 0;
                var home = 1;
                var hidden = 0;
                var event = 0;
                action KnownMove when (home == 1) {
                    hidden = 1;
                    home = if Buggy == 1 { 0 } else { 1 };
                    event = 1;
                }
                action UnknownMove when (home <= 1) {
                    hidden = 1; home = 0; event = 2;
                }
                action Observe when (home <= 1) {
                    hidden = 0; home = 1; event = 0;
                }
                invariant Bounds: home <= 1 && hidden <= 1 && event <= 2;
                invariant KnownKeepsHome: event == 0 || event == 2 || home == 1;
                invariant UnknownClearsHome: event <= 1 || home == 0;
                invariant VisibleHasHome: hidden == 1 || home == 1;
            }
        }
    }

    #[test]
    fn hidden_cursor_home_continuity_conforms_to_real_terminal_motion() {
        use aterm_core::terminal::{ContentScrollDelta, ContentScrollState};
        use std::collections::BTreeMap;
        let model = hidden_home_continuity_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        let known: &[&[u8]] = &[
            b"",
            b"\x1b[1;18r\x1b[18;1H\n",
            b"\x1b[3;18r\x1b[8;1H\x1b[2L",
            b"\x1b[3;18r\x1b[8;1H\x1b[2M",
            b"\x1b[3;18r\x1b[3;1H\x1bM",
            b"\x1b[20;1H\n",
        ];
        for motion in known {
            let mut s = Scene::new();
            let home = s
                .pet
                .console
                .cursor_home
                .expect("the visible fixture has a home");
            let input_seq = s.pet.console_input_seq();
            let before = s.term.content_scroll_state();
            let mut bytes = b"\x1b[?25l\x1b7".to_vec();
            bytes.extend_from_slice(motion);
            bytes.extend_from_slice(b"\x1b[r\x1b8");
            s.term.process(&bytes);
            assert!(!matches!(
                ContentScrollState::delta_since(Some(before), s.term.content_scroll_state()),
                ContentScrollDelta::Invalidate | ContentScrollDelta::Baseline
            ));
            s.frame(0.016);
            let actual = BTreeMap::from([
                ("home", i64::from(s.pet.console.cursor_home.is_some())),
                ("hidden", i64::from(!s.term.cursor_visible())),
                ("event", 1),
            ]);
            assert!(
                model
                    .successors("KnownMove", &model.init_state())
                    .contains(&actual)
            );
            assert_eq!(
                s.pet.console.cursor_home,
                Some(home),
                "home must not follow moved text"
            );
            assert_eq!(s.pet.console_input_seq(), input_seq);
            let mut historical = actual.clone();
            historical.insert("home", 0);
            assert!(!model.check_invariant("KnownKeepsHome", &historical));
            assert!(
                !model
                    .successors("KnownMove", &model.init_state())
                    .contains(&historical)
            );
        }
        for fault in 0..7 {
            let mut s = Scene::new();
            let before = s.term.content_scroll_state();
            s.term.process(b"\x1b[?25l");
            match fault {
                0 => s.term.process(b"\x1bc\x1b[?25l"),
                1 => s.term.process(b"\x1b[?1049h\x1b[?25l"),
                2 => s
                    .term
                    .process(b"\x1b[?69h\x1b[2;6s\x1b[2;18r\x1b[18;1H\x1b[S"),
                3 => {
                    for _ in 0..17 {
                        s.term.process(b"\x1b[1;18r\x1b[18;1H\n");
                    }
                }
                4 => s.term.resize(21, 81),
                5 => {
                    s.term = Terminal::new(20, 80);
                    s.term.process(b"\x1b[?25l");
                }
                _ => {}
            }
            if fault <= 3 {
                assert!(
                    matches!(
                        ContentScrollState::delta_since(
                            Some(before),
                            s.term.content_scroll_state()
                        ),
                        ContentScrollDelta::Invalidate
                    ),
                    "fault {fault} must actually invalidate the terminal motion reader"
                );
            }
            let rows = s.term.grid().rows();
            let cols = s.term.grid().cols();
            let input = s.term.cell_frame(usize::from(rows), usize::from(cols));
            let facts = PetWorldFacts::read(&s.term, if fault == 6 { 18 } else { 17 });
            s.pet.observe_console(&input, &facts, PetPane::full(&input));
            assert!(s.pet.console.world.as_ref().unwrap().stamp().is_some());
            s.pet.set_console_presentable(true);
            s.now += Duration::from_millis(16);
            let input_seq = s.pet.console_input_seq();
            s.pet.tick(PetSense {
                caret_drawn: true,
                now: s.now,
                caret: None,
                wrapped: false,
                rows,
                cols,
                cell_w: 10,
                cell_h: 20,
                reduced_motion: true,
                output_burst: false,
                pointer: None,
            });
            let actual = BTreeMap::from([
                ("home", i64::from(s.pet.console.cursor_home.is_some())),
                ("hidden", i64::from(!s.term.cursor_visible())),
                ("event", 2),
            ]);
            assert!(
                model
                    .successors("UnknownMove", &model.init_state())
                    .contains(&actual),
                "fault {fault}: {actual:?}"
            );
            assert_eq!(s.pet.console_input_seq(), input_seq);
            let mut leaked = actual;
            leaked.insert("home", 1);
            assert!(!model.check_invariant("UnknownClearsHome", &leaked));
        }

        let mut s = Scene::new();
        let state_of = |s: &Scene, event: i64| {
            BTreeMap::from([
                ("home", i64::from(s.pet.console.cursor_home.is_some())),
                ("hidden", i64::from(!s.term.cursor_visible())),
                ("event", event),
            ])
        };
        let mut model_state = model.init_state();
        assert_eq!(state_of(&s, 0), model_state);

        s.term
            .process(b"\x1b[?25l\x1b7\x1b[1;18r\x1b[18;1H\n\x1b[r\x1b8");
        s.frame(0.016);
        let next = state_of(&s, 1);
        assert!(model.successors("KnownMove", &model_state).contains(&next));
        model_state = next;

        s.term.process(b"\x1b[?1049h\x1b[?25l");
        s.frame(0.016);
        let next = state_of(&s, 2);
        assert!(
            model
                .successors("UnknownMove", &model_state)
                .contains(&next)
        );
        assert!(
            model.successors("KnownMove", &next).is_empty(),
            "KnownMove is disabled once home is cleared"
        );
        model_state = next;

        s.term.process(b"\x1b[6;9H\x1b[?25h");
        s.frame(0.016);
        let next = state_of(&s, 0);
        assert!(model.successors("Observe", &model_state).contains(&next));
        assert_eq!(s.pet.console.cursor_home, Some((5, 8)));
        model_state = next;

        s.term.process(b"\x1b[?25l\x1b7\x1b[20;1H\n\x1b8");
        s.frame(0.016);
        let next = state_of(&s, 1);
        assert!(model.successors("KnownMove", &model_state).contains(&next));
        assert_eq!(s.pet.console.cursor_home, Some((5, 8)));
    }

    #[test]
    fn no_caret_retirement_settles_pending_visibility_runs_and_releases_frames() {
        let mut s = Scene::new();
        s.pet.col = 17.0;
        for _ in 0..10 {
            s.frame(5.0);
        }
        assert_eq!(s.pet.action(), PetAction::Sleep);
        assert!(s.pet.console.on_glass);
        assert!(s.pet.console.hold.settled());

        let _ = s.pet.console.hold.update(s.now, None);
        assert!(!s.pet.console.hold.settled());
        assert!(s.pet.needs_frames());

        s.term.process(b"\x1b[?1049h\x1b[?25l");
        for _ in 0..35 {
            s.frame(0.016);
        }
        s.frame(5.0);
        assert_eq!(s.pet.alpha, 0.0);
        assert!(
            s.pet.console.hold.settled(),
            "idle emission must clear pending visibility runs once the body fades off glass"
        );
        assert!(
            !s.pet.needs_frames(),
            "a faded-out sleeper must not stay on the 60 fps train"
        );
    }
    #[test]
    fn a_look_swaps_arrival_ramp_runs_while_the_console_resident_owns_the_tick() {
        let mut s = Scene::new();
        s.term.process(b"\x1b[3;3Hread this\x1b[?25l");
        s.term.text_selection_mut().start_selection(
            2,
            2,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        s.term
            .text_selection_mut()
            .update_selection(2, 10, SelectionSide::Right);
        let settled = s.frame(0.016);
        assert_eq!(s.pet.console_attention(), PetAttention::Reading);
        assert_eq!(settled.alpha, 255, "the resident is opaque before the swap");
        s.pet.arrive_t = HOMECOMING_IN;
        s.pet.arrive_in = HOMECOMING_IN;
        assert!(
            s.pet.needs_frames(),
            "a ramp in flight owes frames even when the resident has nothing else to draw"
        );
        let first = s.frame(0.016);
        assert_eq!(s.pet.console_attention(), PetAttention::Reading);
        assert!(
            first.alpha < 64,
            "the incoming body starts faint ({}), the ghost carries the crossfade",
            first.alpha
        );
        let mut ticks = 0;
        let mut alpha = first.alpha;
        while alpha < 255 && ticks < 40 {
            alpha = s.frame(0.016).alpha;
            ticks += 1;
            assert_eq!(s.pet.console_attention(), PetAttention::Reading);
        }
        let budget = (HOMECOMING_IN / 0.016).ceil() as u32 + 2;
        assert!(
            alpha == 255 && ticks <= budget,
            "the ramp must finish in HOMECOMING_IN under a resident-owned tick: alpha {alpha} after {ticks} ticks (budget {budget})"
        );
    }

    #[test]
    fn a_departing_ghost_over_ordinary_ink_survives_and_over_a_selection_does_not() {
        let mut s = Scene::new();
        s.term
            .process(b"\x1b[5;21Hxxxxxxxx\x1b[6;21Hxxxxxxxx\x1b[6;17H");
        let ghost = |s: &Scene| Departure {
            born: s.pet.lane_clock,
            id: 0,
            coat: 1,
            iris: 1,
            from_col: s.pet.col,
            row: s.pet.row,
            edge: s.pet.col,
            speed: DEPART_SPEED,
            still: true,
            facing_left: false,
            stride0: 0.0,
            pose0: PetGlyphId::PetSit,
            stance0: Stance::NEUTRAL,
            resight: false,
            own_glass: None,
            own_clear: false,
            peak: 1.0,
            colors: None,
            surface: None,
        };
        s.pet.departures[0] = Some(ghost(&s));
        let f = s.frame(0.016);
        assert!(f.alpha > 0, "the live body stands under the ink");
        assert!(
            f.departures[0].is_some(),
            "ordinary ink under the ghost is a z-order fact, not a cull"
        );
        s.term.text_selection_mut().start_selection(
            4,
            20,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        s.term
            .text_selection_mut()
            .update_selection(5, 30, SelectionSide::Right);
        s.pet.departures[0] = Some(ghost(&s));
        let f = s.frame(0.016);
        assert!(f.alpha > 0, "the body stepped aside rather than hiding");
        let d = f.departures[0].expect(
            "the still ghost stepped aside with the body instead of being culled on its own",
        );
        assert!(
            (d.col - f.col).abs() < 1e-3 && (d.row - f.row).abs() < 1e-3,
            "…to the very same stand ({}, {}) vs ({}, {})",
            d.col,
            d.row,
            f.col,
            f.row
        );
        s.pet.departures[0] = Some(Departure {
            still: false,
            edge: 0.0,
            from_col: 20.0,
            row: 5.0,
            ..ghost(&s)
        });
        let f = s.frame(0.016);
        assert!(f.alpha > 0, "the body is still drawn");
        assert!(
            f.departures[0].is_none(),
            "a selection under a running ghost culls it at once, as it culls the body"
        );
    }

    #[test]
    fn a_ghost_on_its_own_glass_needs_its_own_worlds_admission() {
        let switch = |select: bool| {
            let mut s = Scene::new();
            s.pet.worn = Some((1, 1));
            let before = s.frame(0.016);
            assert_eq!(before.alpha, 255, "fixture: on glass");
            if select {
                s.term.text_selection_mut().start_selection(
                    4,
                    20,
                    SelectionSide::Left,
                    SelectionType::Simple,
                );
                s.term
                    .text_selection_mut()
                    .update_selection(5, 30, SelectionSide::Right);
                let input = s.term.cell_frame(20, 80);
                let facts = PetWorldFacts::read(&s.term, 17);
                s.pet.observe_console(&input, &facts, PetPane::full(&input));
            }
            s.pet.resight_coordinate_space();
            s.pet.displace_resight(1);
            let kept = s.pet.has_own_glass_ghost();
            s.term.process(b"\x1b[15;40H");
            (kept, before, s.frame(0.016))
        };
        let (kept, before, f) = switch(false);
        assert!(kept, "clear ground admits the ghost");
        let d = f.departures[0].expect("the ghost is spent on its own glass");
        assert!(d.own_glass && d.under_ink, "{d:?}");
        assert_eq!(
            (d.col, d.row),
            (before.col, before.row),
            "it stands at the old body's cell, whatever the new world holds there"
        );
        assert!(f.alpha > 0, "beside the arriver");

        let (_, _, f) = switch(true);
        assert!(
            f.departures.iter().all(Option::is_none),
            "control: a selection under the body refuses it"
        );
    }

    #[test]
    fn a_carried_ghost_keeps_what_its_own_slot_drew() {
        let carry = |drawn: f32| {
            let mut s = Scene::new();
            s.pet.departures[1] = Some(Departure {
                born: s.pet.lane_clock,
                id: 0,
                coat: 1,
                iris: 1,
                from_col: s.pet.col,
                row: s.pet.row,
                edge: s.pet.col,
                speed: DEPART_SPEED,
                still: true,
                facing_left: false,
                stride0: 0.0,
                pose0: PetGlyphId::PetSit,
                stance0: Stance::NEUTRAL,
                resight: false,
                own_glass: None,
                own_clear: false,
                peak: 1.0,
                colors: None,
                surface: None,
            });
            s.pet.last_drawn = [0.0; PET_DEPARTURES_MAX];
            s.pet.last_drawn[1] = drawn;
            s.pet.resight_coordinate_space();
            s.pet
                .carried
                .iter()
                .flatten()
                .map(|c| c.presence)
                .collect::<Vec<_>>()
        };
        assert_eq!(carry(0.4), [0.4], "carried at what its slot drew");
        assert!(carry(0.0).is_empty(), "control: a ghost that drew nothing");
    }

    #[test]
    fn a_look_swap_landing_under_a_hidden_caret_resident_keeps_both_halves_of_the_crossfade() {
        let mut s = Scene::new();
        assert_eq!(
            s.pet.sync_look((3, 1), PetArrival::Ceremony).worn,
            (3, 1),
            "the first dress applies"
        );
        s.frame(0.016);
        assert_eq!(
            s.pet.sync_look((9, 4), PetArrival::Quiet).worn,
            (3, 1),
            "fixture: the homecoming parks"
        );
        let mut landing = None;
        for _ in 0..400 {
            let f = s.frame(0.016);
            if s.pet.sync_look((9, 4), PetArrival::Quiet).worn == (9, 4) {
                landing = Some(f);
                break;
            }
        }
        let landing = landing.expect("the debounced homecoming landed");
        assert_eq!(
            landing.lane_alpha, 255,
            "the lane is at full presence at the swap"
        );
        assert_eq!(
            landing.departures.iter().flatten().count(),
            1,
            "the swap spawned its ghost"
        );
        let pre = u32::from(landing.lane_alpha);
        s.term.process(b"\x1b[?25l");
        let mut ticks = 0u32;
        let mut resident_ticks = 0u32;
        loop {
            let f = s.frame(0.016);
            ticks += 1;
            if s.pet.console.resident {
                resident_ticks += 1;
            }
            let cover = u32::from(f.lane_alpha);
            let ghost = f
                .departures
                .iter()
                .flatten()
                .map(|d| u32::from(d.alpha) * cover / 255)
                .max()
                .unwrap_or(0);
            let arriver = u32::from(f.alpha);
            assert!(
                ghost.max(arriver) * 2 + 2 >= pre * cover / 255,
                "tick {ticks}: max(ghost {ghost}, arriver {arriver}) fell under half the \
                 pre-swap alpha × cover ({pre} × {cover}/255)"
            );
            let composite = 255 - (255 - ghost) * (255 - arriver) / 255;
            assert!(
                composite * 100 >= pre * cover / 255 * 70,
                "tick {ticks}: composite {composite} under 70 % of the pre-swap cover {cover}"
            );
            if s.pet.arrive_t <= 0.0 && s.pet.departures.iter().all(Option::is_none) {
                break;
            }
            assert!(ticks < 60, "the crossfade is a quarter second, not a stall");
        }
        assert!(
            resident_ticks + 1 >= ticks,
            "fixture: the hidden caret handed the resident every tick of the crossfade \
             ({resident_ticks} of {ticks})"
        );
    }

    #[test]
    fn a_departing_ghost_over_a_protected_line_fades_with_the_body_instead_of_vanishing() {
        let mut s = Scene::new();
        s.term.process(b"\x1b[9;1H\x1b#6\x1b[10;1H\x1b#6\x1b[6;17H");
        s.frame(0.016);
        let ghost = Departure {
            born: s.pet.lane_clock,
            id: 0,
            coat: 1,
            iris: 1,
            from_col: s.pet.col,
            row: s.pet.row + 3.0,
            edge: s.pet.col,
            speed: DEPART_SPEED,
            still: true,
            facing_left: false,
            stride0: 0.0,
            pose0: PetGlyphId::PetSit,
            stance0: Stance::NEUTRAL,
            resight: false,
            own_glass: None,
            own_clear: false,
            peak: 1.0,
            colors: None,
            surface: None,
        };
        s.pet.departures[0] = Some(ghost);
        let f = s.frame(0.016);
        assert!(f.alpha > 0, "the live body stands clear");
        let world = s.pet.console.world.as_ref().expect("observed");
        let d = f.departures[0].expect(
            "a protected line under the ghost is the body's dwell-and-fade, never the ghost's cull",
        );
        let (x0, x1, y0, y1) = PetFrame {
            alpha: d.alpha,
            col: d.col,
            row: d.row,
            lift: 0.0,
            ..f
        }
        .body_px(10, 20, 80, 20)
        .expect("the ghost is on the grid");
        let rect = PetRect::new(
            y0 as f32 / 20.0,
            x0 as f32 / 10.0,
            (y1 - y0) as f32 / 20.0,
            (x1 - x0) as f32 / 10.0,
        );
        assert_eq!(
            world.under_text_clearance_past_caret(rect, CLEARANCE),
            Some(false),
            "fixture: the ghost's ground is protected"
        );
        let mut frames = 1u32;
        let mut prev = d.alpha;
        while s.pet.departures[0].is_some() {
            let f = s.frame(0.016);
            frames += 1;
            if let Some(d) = s.pet.departures[0] {
                let drawn = f.departures[0].unwrap_or_else(|| {
                    panic!(
                        "frame {frames}: the ghost (born {}) was culled while the body is drawn at {}",
                        d.born, f.alpha
                    )
                });
                assert!(drawn.alpha <= prev, "the old coat only ever fades DOWN");
                prev = drawn.alpha;
            }
            assert!(frames < 40, "a zero-span ghost's life is EDGE_FADE");
        }
        let want = (EDGE_FADE / 0.016) as u32;
        assert!(
            frames + 2 >= want,
            "the ghost ran its whole ramp ({frames} frames, want ≥ {want})"
        );
    }

    impl Scene {
        fn type_text(&mut self, text: &str) {
            for ch in text.bytes() {
                self.input(PetInputKind::Text);
                self.term.process(&[ch]);
                self.frame(0.05);
            }
        }

        fn executing(&mut self) {
            self.term
                .process(b"\x1b[8;1H\x1b]133;A\x07> \x1b]133;B\x07build\r\n\x1b]133;C\x07working");
            for _ in 0..90 {
                self.frame(0.016);
                if self.pet.console.resident && !self.pet.console.moving && !self.pet.needs_frames()
                {
                    break;
                }
            }
            assert_eq!(self.pet.console_attention(), PetAttention::Output);
            assert!(
                self.pet.console.resident,
                "fixture: the running command owns the cat"
            );
        }

        fn watch(&mut self, secs: f32) -> Vec<PetFrame> {
            (0..(secs / 0.016).ceil() as usize)
                .map(|_| self.frame(0.016))
                .collect()
        }
    }

    #[test]
    fn a_typed_treat_frolics_with_the_console_layer_watching() {
        let mut s = Scene::new();
        s.type_text("treat ");
        assert!(
            matches!(
                s.pet.console_attention(),
                PetAttention::Typing | PetAttention::Contact
            ),
            "fixture: the last key is live — the window the cheer latch is wiped in"
        );
        s.pet.note_trick(s.now, Trick::Treat, true);
        let frames = s.watch(2.0);
        let frolic = frames
            .iter()
            .position(|f| f.action == PetAction::Frolic)
            .expect("the treat frolics");
        assert!(
            frames[..frolic].iter().any(|f| f.action == PetAction::Perk),
            "heard first"
        );
        assert!(
            matches!(
                frames[frolic].pose,
                PetGlyphId::PetPlaybow | PetGlyphId::PetBat
            ),
            "in the frolic's own frames, not a console pose: {:?}",
            frames[frolic].pose
        );
        assert!(
            frames[frolic..].iter().any(|f| f
                .motes
                .iter()
                .flatten()
                .any(|m| m.kind == PetMoteKind::Note)),
            "with its motes on glass"
        );
        assert!(
            s.pet.pending_cheer.is_none(),
            "and never through the cheer latch"
        );
    }

    #[test]
    fn a_commanded_sleep_survives_an_executing_block() {
        let mut s = Scene::new();
        s.executing();
        s.pet.note_trick(s.now, Trick::Sleep, true);
        assert!(
            !s.pet.needs_frames(),
            "a parked resident still answers first"
        );
        s.frame(0.016);
        assert!(!s.pet.console.resident && s.pet.needs_frames());
        let frames = s.watch(3.0);
        assert!(frames.iter().any(|f| f.action == PetAction::Perk), "heard");
        let last = frames.last().expect("frames");
        assert_eq!(
            last.action,
            PetAction::Sleep,
            "asleep a second later, and still"
        );
        assert!(
            matches!(last.pose, PetGlyphId::PetSleep0 | PetGlyphId::PetSleep1),
            "in the sleeper's own frame, not the resident's: {:?}",
            last.pose
        );
        assert!(
            !s.pet.console.resident,
            "the running command does not reseat it"
        );
        assert!(!s.pet.needs_frames(), "and a sleeping cat rides the offer");
        s.term.process(b"\r\nmore output");
        s.frame(0.016);
        assert!(s.pet.trick_settle.is_none());
        let _ = s.watch(2.5);
        assert!(s.pet.console.resident, "the resident gets the cat back");
        assert_eq!(s.pet.console_attention(), PetAttention::Output);
    }

    #[test]
    fn a_roll_plays_through_an_executing_block() {
        let mut s = Scene::new();
        s.executing();
        s.pet.note_trick(s.now, Trick::Roll, true);
        let frames = s.watch(4.5);
        let rolled = frames
            .iter()
            .filter(|f| {
                matches!(
                    f.pose,
                    PetGlyphId::PetRoll0 | PetGlyphId::PetRoll1 | PetGlyphId::PetRoll2
                )
            })
            .count();
        assert!(
            rolled as f32 * 0.016 > WRIGGLE_DUR - 0.2,
            "the whole roll, not a frame of it: {rolled} frames"
        );
        assert!(
            !s.pet.trick_act,
            "and the flag comes down with the performance"
        );
        assert!(
            s.pet.console.resident,
            "after which the command has its watcher back"
        );
    }

    #[test]
    fn a_landing_cheers_over_a_running_command_and_hands_the_cat_back() {
        let mut s = Scene::new();
        s.executing();
        assert!(
            !s.pet.needs_frames(),
            "fixture: the running command's resident is parked"
        );
        s.pet.note_room_landed();
        let mut frames = Vec::new();
        let mut aloft = 0;
        for _ in 0..(5.0 / 0.016) as usize {
            frames.push(s.frame(0.016));
            aloft = aloft.max(s.pet.cheer_motes_aloft());
        }
        let frolic = frames
            .iter()
            .position(|f| f.action == PetAction::Frolic)
            .expect("the landing frolics over the running command");
        assert!(
            frames[..frolic].iter().any(|f| f.action == PetAction::Perk),
            "perked first"
        );
        assert_eq!(aloft, 3, "its three ♪/♥ in the air together");
        assert!(s.pet.pending_cheer.is_none() && !s.pet.room_cheer, "spent");
        assert!(
            s.pet.console.resident,
            "the running command has its watcher back"
        );
        assert_eq!(s.pet.console_attention(), PetAttention::Output);
        for _ in 0..240 {
            s.frame(0.016);
            if !s.pet.needs_frames() {
                break;
            }
        }
        assert!(!s.pet.needs_frames(), "and the frames stop");
    }

    struct AgentPane(Scene, Option<PetRect>, PaneGeom);

    #[derive(Clone, Copy, Debug)]
    struct PaneGeom {
        cols: u16,
        rows: u16,
        cell: (u16, u16),
    }

    impl PaneGeom {
        const WIDE: Self = Self {
            cols: 100,
            rows: 30,
            cell: (15, 28),
        };

        const fn chrome(self) -> [PetRect; 2] {
            let min_cols = if self.cols < 38 { self.cols } else { 38 };
            let card = min_cols as f32;
            [
                PetRect::new(12.0, self.cols as f32 - card, 1.0, card),
                PetRect::new(14.0, 0.0, 1.0, 16.0),
            ]
        }
    }

    impl AgentPane {
        fn new() -> Self {
            Self::new_warm(200)
        }

        fn new_warm(warm: usize) -> Self {
            Self::with_transcript(
                "\x1b[2J\x1b[1;1H \u{2590}\u{259b}\u{2588}\u{2588}\u{2588}\u{259b}\u{2588}   \
                 Claude Code v2.1.304\r\n\u{259d}\u{259c}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\
                 \u{2588}\u{2580}  Opus 5.5 \u{b7} API Usage Billing\r\n \u{259d}\u{259d}   \
                 \u{259d}\u{259d}   /private/tmp/proj\r\n\r\n\r\n\x1b[48;2;55;55;55m\u{276f} finish the parser\
                 \x1b[K\x1b[0m\r\n\r\n\
                 \u{23fa} Committed and pushed the parser and its tests.\r\n  ATERM-TASK-DONE\r\n\r\n\
                 \u{273b} Churned for 40s \u{b7} done 3:01 AM\r\n\r\n",
                warm,
            )
        }

        fn with_transcript(transcript: &str, warm: usize) -> Self {
            Self::with_geom(transcript, warm, PaneGeom::WIDE)
        }

        fn narrow(cols: u16, cell: (u16, u16)) -> Self {
            Self::narrow_at(cols, 30, cell)
        }

        fn narrow_at(cols: u16, rows: u16, cell: (u16, u16)) -> Self {
            Self::with_geom(
                "\x1b[2J\x1b[1;1H  pushed to origin/main.\r\n\r\n\
                 \u{273b} Baked for 40s \u{b7} done 1:39 PM\r\n\r\n\
                 \x1b[48;2;55;55;55m\u{276f} [aterm harness] Autopilot is Land: land\x1b[K\r\n\
                 \x20 remote main, defend that in a few lines\x1b[K\x1b[0m\r\n\r\n\
                 \u{23fa} Nothing left: the work is committed and\r\n\
                 \x20 pushed to origin/main.\r\n\r\n\
                 \u{273b} Cooked for 40s \u{b7} done 1:41 PM\r\n\r\n",
                200,
                PaneGeom { cols, rows, cell },
            )
        }

        fn with_geom(transcript: &str, warm: usize, geom: PaneGeom) -> Self {
            let mut term = Terminal::new(geom.rows, geom.cols);
            term.process(b"\x1b]11;#000000\x07");
            term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07claude\r\n\x1b]133;C\x07");
            term.process(transcript.as_bytes());
            let rule = "\u{2500}".repeat(usize::from(geom.cols));
            term.process(
                format!(
                    "{rule}\u{276f} \r\n{rule}  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle)\
                     \x1b[?25l\x1b[14;3H"
                )
                .as_bytes(),
            );
            let now = Instant::now();
            let pet = PetBrain {
                col: 6.0,
                row: 10.0,
                alpha: 1.0,
                action: PetAction::Sit,
                quiet: 2.0,
                last_caret: Some((13, 2)),
                last_now: Some(now),
                ..PetBrain::default()
            };
            let mut pane = Self(Scene { term, pet, now }, None, geom);
            for _ in 0..warm {
                pane.frame();
            }
            pane
        }

        fn parks_within(&mut self, secs: f32) -> bool {
            for _ in 0..(secs / 0.016) as usize {
                self.frame();
                if !self.0.pet.needs_frames() {
                    return true;
                }
            }
            false
        }

        fn observe(&mut self) {
            let (extra, geom) = (self.1, self.2);
            let s = &mut self.0;
            let input = s
                .term
                .cell_frame(usize::from(geom.rows), usize::from(geom.cols));
            let facts = PetWorldFacts::read(&s.term, 17);
            let [card, chips] = geom.chrome();
            let chrome = [card, chips, extra.unwrap_or_default()];
            let n = if extra.is_some() { 3 } else { 2 };
            s.pet.observe_console_with_exclusions(
                &input,
                &facts,
                PetPane::full(&input),
                &chrome[..n],
            );
            s.pet.set_console_presentable(true);
        }

        fn frame(&mut self) -> PetFrame {
            self.0.now += Duration::from_millis(16);
            self.observe();
            let geom = self.2;
            let s = &mut self.0;
            let c = s.term.cursor();
            s.pet.tick(PetSense {
                caret_drawn: s.term.cursor_visible(),
                now: s.now,
                caret: Some((c.row, c.col)),
                wrapped: false,
                rows: geom.rows,
                cols: geom.cols,
                cell_w: geom.cell.0,
                cell_h: geom.cell.1,
                reduced_motion: false,
                output_burst: false,
                pointer: None,
            })
        }
    }

    #[test]
    fn a_landing_in_an_agents_pane_cheers_on_open_ground_with_its_notes_on_glass() {
        let mut p = AgentPane::new();
        assert!(p.parks_within(10.0), "fixture: the cat parks");
        p.0.pet.note_room_landed();
        let mut frames: Vec<PetFrame> = (0..(4.0 / 0.016) as usize).map(|_| p.frame()).collect();
        let frolic = frames
            .iter()
            .position(|f| f.action == PetAction::Frolic)
            .expect("the landing frolics in the agent's pane");
        assert!(
            frames[..frolic].iter().any(|f| f.action == PetAction::Perk),
            "perked first"
        );
        assert_eq!(
            frames[frolic..]
                .iter()
                .map(|f| f.motes.iter().flatten().count())
                .max(),
            Some(3),
            "its three ♪/♥ on glass together"
        );
        let stage = (frames[frolic].col, frames[frolic].row);
        assert_ne!(
            stage,
            (frames[0].col, frames[0].row),
            "it went to open ground"
        );
        assert!(
            frames[frolic..]
                .iter()
                .take_while(|f| f.motes.iter().flatten().count() > 0)
                .all(|f| f.action != PetAction::Leap),
            "the cat stays on its stage while they rise"
        );
        for _ in 0..(10.0 / 0.016) as usize {
            frames.push(p.frame());
            if !p.0.pet.needs_frames() {
                break;
            }
        }
        assert!(!p.0.pet.needs_frames(), "then it parks");
        assert!(
            !p.0.pet.room_cheer && p.0.pet.pending_cheer.is_none(),
            "spent"
        );
        assert_ne!(
            (p.0.pet.col, p.0.pet.row),
            stage,
            "and the cat has gone home"
        );
        let faded = frames[frolic..]
            .iter()
            .filter(|f| f.alpha < 255)
            .map(|f| (f.action, f.col, f.row, f.alpha))
            .collect::<Vec<_>>();
        assert!(
            faded.is_empty() && p.0.pet.console.on_glass,
            "on glass all the way home: a home it may stand on is no reason to \
             leave: {faded:?}"
        );
    }

    #[test]
    fn a_cat_asleep_on_the_transcript_wakes_into_the_cheer_not_an_eviction() {
        let mut p = AgentPane::new();
        p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 100.0));
        p.0.pet.col = 0.371;
        p.0.pet.row = 10.995;
        p.0.pet.set_action(PetAction::Sleep);
        p.0.pet.note_room_landed();
        let frames: Vec<PetFrame> = (0..(4.0 / 0.016) as usize).map(|_| p.frame()).collect();
        let woke = frames
            .iter()
            .position(|f| f.action != PetAction::Sleep)
            .expect("it wakes");
        let frolic = frames
            .iter()
            .position(|f| f.action == PetAction::Frolic)
            .expect("and frolics");
        assert!(woke < frolic);
        assert_eq!(
            frames[frolic..]
                .iter()
                .map(|f| f.motes.iter().flatten().count())
                .max(),
            Some(3),
            "its three ♪/♥ on glass together"
        );
        p.assert_home_on_clean_ground("the sleeper's cheer", None);
    }

    #[test]
    fn a_success_stretches_only_on_blank_ground() {
        for under_a_word in [true, false] {
            let why = if under_a_word {
                "under a word"
            } else {
                "on blank ground"
            };
            let mut s = Scene::new();
            for _ in 0..30 {
                s.frame(0.016);
            }
            let (col, row) = (s.pet.col, s.pet.row);
            if under_a_word {
                s.term.process(
                    format!(
                        "\x1b[{};{}Hall 42 tests passed in 31.9s\x1b[6;17H",
                        row as usize + 1,
                        col.floor() as usize
                    )
                    .as_bytes(),
                );
            }
            s.pet
                .note_command_done(s.now, false, Some(CHEER_BIG_MS + 1_000));
            let first = s.frame(0.016);
            assert_eq!(
                s.pet.console_reason(),
                "command-exit-success",
                "{why}: the result is answered"
            );
            let mut frames = vec![first];
            frames.extend(s.watch(RESULT_HOLD + 0.5));
            let world = s.pet.console_world().expect("observed");
            let stretches: Vec<_> = frames
                .iter()
                .filter(|f| {
                    f.alpha > 0
                        && matches!(f.pose, PetGlyphId::PetStretch | PetGlyphId::PetStretchHind)
                })
                .map(|f| {
                    let (x0, x1, y0, y1) = f.body_px(10, 20, 80, 20).expect("on glass");
                    let rect = PetRect::new(
                        y0 as f32 / 20.0,
                        x0 as f32 / 10.0,
                        (y1 - y0) as f32 / 20.0,
                        (x1 - x0) as f32 / 10.0,
                    );
                    world.ink_clearance(rect, 0.0)
                })
                .collect();
            if under_a_word {
                assert!(
                    stretches.is_empty(),
                    "{why}: no stretch drawn under the words: {stretches:?}"
                );
                let held: Vec<_> = frames
                    .iter()
                    .take_while(|f| f.action != PetAction::Leap)
                    .take((RESULT_HOLD / 0.016) as usize)
                    .map(|f| {
                        let (x0, x1, y0, y1) = f.body_px(10, 20, 80, 20).expect("on glass");
                        let rect = PetRect::new(
                            y0 as f32 / 20.0,
                            x0 as f32 / 10.0,
                            (y1 - y0) as f32 / 20.0,
                            (x1 - x0) as f32 / 10.0,
                        );
                        (f.alpha, f.under_ink, world.ink_clearance(rect, 0.0), f.pose)
                    })
                    .collect();
                assert!(
                    held.len() >= (RESULT_HOLD / 0.016) as usize - 1
                        && held.iter().all(|&(alpha, under, ink, _)| {
                            alpha > 0 && under && ink == Some(false)
                        }),
                    "{why}: through the hold the cat stays where it lies, on glass, drawn \
                     behind the words: {held:?}"
                );
            } else {
                assert!(
                    !stretches.is_empty() && stretches.iter().all(|&c| c == Some(true)),
                    "{why}: the stretch plays, in front of blank ground: {stretches:?}"
                );
            }
        }
    }

    #[test]
    fn a_key_wakes_a_sleeper_under_a_word_without_the_stretch() {
        for under_a_word in [true, false] {
            let why = if under_a_word {
                "under a word"
            } else {
                "on blank ground"
            };
            let mut s = Scene::new();
            for _ in 0..30 {
                s.frame(0.016);
            }
            let (col, row) = (s.pet.col, s.pet.row);
            if under_a_word {
                s.term.process(
                    format!(
                        "\x1b[{};{}Hall 42 tests passed in 31.9s\x1b[6;17H",
                        row as usize + 1,
                        col.floor() as usize
                    )
                    .as_bytes(),
                );
            }
            s.watch(3.0);
            s.pet.set_action(PetAction::Sleep);
            s.pet.quiet = 30.0;
            let asleep = s.watch(0.064).pop().expect("a frame");
            assert_eq!(asleep.action, PetAction::Sleep, "{why}: fixture: asleep");
            let (x0, x1, y0, y1) = asleep.body_px(10, 20, 80, 20).expect("on glass");
            let sleeper = PetRect::new(
                y0 as f32 / 20.0,
                x0 as f32 / 10.0,
                (y1 - y0) as f32 / 20.0,
                (x1 - x0) as f32 / 10.0,
            );
            assert_eq!(
                s.pet
                    .console_world()
                    .expect("observed")
                    .ink_clearance(sleeper, 0.0),
                Some(!under_a_word),
                "{why}: fixture: the sleeper's body and the words"
            );
            s.input(PetInputKind::Text);
            let frames = s.watch(1.5);
            assert!(
                frames.iter().any(|f| f.action == PetAction::Waking),
                "{why}: the key wakes it"
            );
            let world = s.pet.console_world().expect("observed");
            let stretches: Vec<_> = frames
                .iter()
                .filter(|f| {
                    f.alpha > 0
                        && matches!(f.pose, PetGlyphId::PetStretch | PetGlyphId::PetStretchHind)
                })
                .map(|f| {
                    let (x0, x1, y0, y1) = f.body_px(10, 20, 80, 20).expect("on glass");
                    let rect = PetRect::new(
                        y0 as f32 / 20.0,
                        x0 as f32 / 10.0,
                        (y1 - y0) as f32 / 20.0,
                        (x1 - x0) as f32 / 10.0,
                    );
                    world.ink_clearance(rect, 0.0)
                })
                .collect();
            if under_a_word {
                assert!(
                    stretches.is_empty(),
                    "{why}: no stretch drawn under the words: {stretches:?}"
                );
            } else {
                assert!(
                    !stretches.is_empty() && stretches.iter().all(|&c| c == Some(true)),
                    "{why}: the stretch plays, in front of blank ground: {stretches:?}"
                );
            }
        }
    }

    #[test]
    fn a_sleepers_fall_stretches_only_on_blank_ground() {
        let words = |n: usize| -> String {
            "echo 'all 42 tests passed in 31.9s; the parser and its tests are pushed' "
                .chars()
                .cycle()
                .take(n)
                .collect()
        };
        let floors: [(&str, &str, &str, bool, [bool; 2]); 4] = [
            ("over the typed words", "", "", true, [false, false]),
            ("over a blank line", "", "", false, [true, true]),
            (
                "from under a line of output onto a blank line",
                "test result: ok. 271 passed; 0 failed; 0 ignored; 0 measured; 1776 filtered out",
                "",
                false,
                [false, true],
            ),
            (
                "from blank ground onto a painted band",
                "",
                "\x1b[48;2;55;55;55m\x1b[K\x1b[0m",
                false,
                [true, false],
            ),
        ];
        for (why, above, below, typed_words, blank) in floors {
            let mut s = Scene::new();
            for _ in 0..30 {
                s.frame(0.016);
            }
            s.term
                .process(format!("\x1b[5;1H{above}\x1b[7;1H{below}\x1b[6;17H").as_bytes());
            s.watch(3.0);
            s.pet.set_action(PetAction::Sleep);
            s.pet.quiet = 30.0;
            let asleep = s.watch(0.064).pop().expect("a frame");
            assert_eq!(asleep.action, PetAction::Sleep, "{why}: fixture: asleep");
            assert_eq!(s.pet.row, 5.0, "{why}: fixture: asleep on the caret's row");
            let wrap = 80 - usize::from(s.term.cursor().col) + 3;
            let line = if typed_words {
                words(wrap)
            } else {
                " ".repeat(wrap)
            };
            s.term.process(line.as_bytes());
            let mut fell = None;
            let mut frames = Vec::new();
            for _ in 0..(1.0 / 0.016) as usize {
                frames.push(s.frame(0.016));
                fell = fell.or(s.pet.flight.filter(|f| f.drop));
            }
            let fell = fell.unwrap_or_else(|| panic!("{why}: fixture: the sleeper falls one row"));
            let ends = [
                s.pet
                    .console_stretch_on_blank_at((fell.from_col, fell.from_row)),
                s.pet
                    .console_stretch_on_blank_at((fell.to_col, fell.to_row)),
            ];
            assert_eq!(ends, blank, "{why}: fixture: blank ground at (from, to)");
            let world = s.pet.console_world().expect("observed");
            let stretches: Vec<_> = frames
                .iter()
                .filter(|f| {
                    f.alpha > 0
                        && matches!(f.pose, PetGlyphId::PetStretch | PetGlyphId::PetStretchHind)
                })
                .map(|f| {
                    let (x0, x1, y0, y1) = f.body_px(10, 20, 80, 20).expect("on glass");
                    let rect = PetRect::new(
                        y0 as f32 / 20.0,
                        x0 as f32 / 10.0,
                        (y1 - y0) as f32 / 20.0,
                        (x1 - x0) as f32 / 10.0,
                    );
                    world.ink_clearance(rect, 0.0)
                })
                .collect();
            if blank == [true, true] {
                assert!(
                    !stretches.is_empty() && stretches.iter().all(|&c| c == Some(true)),
                    "{why}: the fall stretches, in front of blank ground: {stretches:?}"
                );
            } else {
                assert!(
                    stretches.is_empty(),
                    "{why}: no stretch drawn in the fall: {stretches:?}"
                );
            }
        }
    }

    impl AgentPane {
        fn stretches_over_ink(&self, frames: &[PetFrame]) -> Vec<(usize, Vec<(i32, i32)>)> {
            let world = self.0.pet.console_world().expect("observed");
            let PaneGeom {
                cols,
                cell: (cw, ch),
                ..
            } = self.2;
            let (cw, ch) = (i32::from(cw), i32::from(ch));
            frames
                .iter()
                .enumerate()
                .filter(|(_, f)| {
                    f.alpha > 0
                        && matches!(f.pose, PetGlyphId::PetStretch | PetGlyphId::PetStretchHind)
                })
                .filter_map(|(k, f)| {
                    let (x0, x1, y0, y1) =
                        f.body_px(self.2.cell.0, self.2.cell.1, cols, self.2.rows)?;
                    let under: Vec<_> = (y0.div_euclid(ch)..(y1 + ch - 1).div_euclid(ch))
                        .flat_map(|r| {
                            (x0.div_euclid(cw)..(x1 + cw - 1).div_euclid(cw)).map(move |c| (r, c))
                        })
                        .filter(|&(r, c)| {
                            world.cell(r as usize, c as usize) != crate::pet_world::PetCell::Clear
                        })
                        .collect();
                    (!under.is_empty()).then_some((k, under))
                })
                .collect()
        }
    }

    #[test]
    fn a_sleeper_stretches_only_on_blank_ground() {
        let mut p = AgentPane::new();
        p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 100.0));
        p.0.pet.col = 0.371;
        p.0.pet.row = 10.995;
        p.0.pet.set_action(PetAction::Sleep);
        p.0.pet.note_room_landed();
        let frames: Vec<PetFrame> = (0..(4.0 / 0.016) as usize).map(|_| p.frame()).collect();
        let over = p.stretches_over_ink(&frames);
        assert!(
            over.is_empty(),
            "no stretch drawn over a word: {over:?} (frames, cells under the body)"
        );
        let leap = frames
            .iter()
            .position(|f| f.action == PetAction::Leap)
            .expect("it hops to its stage");
        assert!(
            frames[..leap].iter().all(|f| f.action == PetAction::Sleep),
            "no wake beat before the hop: {:?}",
            frames[..leap].iter().map(|f| f.action).collect::<Vec<_>>()
        );
        let frolic = frames
            .iter()
            .position(|f| f.action == PetAction::Frolic)
            .expect("it wakes and frolics on its stage");
        assert_eq!(
            frames[frolic..]
                .iter()
                .map(|f| f.motes.iter().flatten().count())
                .max(),
            Some(3),
            "its three ♪/♥ on glass together"
        );

        let mut p = AgentPane::new();
        assert!(p.parks_within(10.0), "fixture: the cat parks at home");
        p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 100.0));
        p.0.pet.set_action(PetAction::Sleep);
        p.0.pet.note_room_landed();
        let frames: Vec<PetFrame> = (0..(4.0 / 0.016) as usize).map(|_| p.frame()).collect();
        let stretched = frames
            .iter()
            .filter(|f| matches!(f.pose, PetGlyphId::PetStretch | PetGlyphId::PetStretchHind))
            .count();
        assert!(stretched > 0, "a sleeper on blank ground stretches awake");
        let over = p.stretches_over_ink(&frames);
        assert!(over.is_empty(), "on blank ground: {over:?}");
        assert!(
            frames.iter().any(|f| f.action == PetAction::Frolic),
            "and frolics"
        );

        let mut p = AgentPane::new();
        assert!(p.parks_within(10.0), "fixture: the cat parks at home");
        p.0.pet.set_action(PetAction::Sleep);
        p.frame();
        assert!(
            p.0.pet.console_stretch_on_blank(),
            "fixture: asleep on blank ground"
        );
        let body = p.0.pet.console_body(PetSense {
            caret_drawn: false,
            now: p.0.now,
            caret: None,
            wrapped: false,
            rows: 30,
            cols: 100,
            cell_w: 15,
            cell_h: 28,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        });
        p.1 = Some(body);
        p.observe();
        let world = p.0.pet.console_world().expect("observed");
        assert_eq!(
            world.under_text_clearance_past_caret(body, 0.0),
            Some(false),
            "fixture: the host's chrome over the sleeper"
        );
        assert!(
            !p.0.pet.console_stretch_on_blank(),
            "under the host's chrome: no blank ground for the stretch"
        );
    }

    impl AgentPane {
        fn at_first_look(asleep: bool) -> (Self, (f32, f32)) {
            let mut p = Self::new();
            p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 100.0));
            assert!(p.parks_within(10.0), "fixture: the cat parks at home");
            assert!(
                p.0.pet.console.on_glass,
                "fixture: its home is clean ground it may stand on: {}",
                p.0.pet.console.reason
            );
            let home = (p.0.pet.col, p.0.pet.row);
            p.0.pet.alpha = 0.0;
            p.0.pet.console.on_glass = false;
            p.0.pet.set_action(if asleep {
                PetAction::Sleep
            } else {
                PetAction::Sit
            });
            (p, home)
        }

        fn assert_home_on_clean_ground(&mut self, why: &str, home: Option<(f32, f32)>) {
            assert!(
                self.parks_within(10.0),
                "{why}: the cat parks after the cheer, not a hop in place: \
                 {:?} at {:.2},{:.2}",
                self.0.pet.action,
                self.0.pet.col,
                self.0.pet.row
            );
            let f = self.frame();
            assert!(
                self.0.pet.console.on_glass && f.alpha > 0,
                "{why}: home on glass ({})",
                self.0.pet.console.reason
            );
            let PaneGeom {
                cols,
                rows,
                cell: (cw, ch),
            } = self.2;
            let (x0, x1, y0, y1) = f.body_px(cw, ch, cols, rows).expect("on glass");
            let world = self.0.pet.console_world().expect("observed");
            let (cw, ch) = (i32::from(cw), i32::from(ch));
            let under: Vec<_> = (y0.div_euclid(ch)..(y1 + ch - 1).div_euclid(ch))
                .flat_map(|r| {
                    (x0.div_euclid(cw)..(x1 + cw - 1).div_euclid(cw)).map(move |c| (r, c))
                })
                .filter(|&(r, c)| {
                    world.cell(r as usize, c as usize) != crate::pet_world::PetCell::Clear
                })
                .collect();
            assert!(
                under.is_empty(),
                "{why}: nothing under the cat at home ({:.2}, {:.2}): {under:?}",
                self.0.pet.col,
                self.0.pet.row
            );
            if let Some((col, row)) = home {
                assert!(
                    (self.0.pet.col - col).abs() < 0.5 && (self.0.pet.row - row).abs() < 0.5,
                    "{why}: back at its home ({col:.2}, {row:.2}): {:.2},{:.2}",
                    self.0.pet.col,
                    self.0.pet.row
                );
            }
            assert!(
                !self.0.pet.room_cheer && self.0.pet.pending_cheer.is_none(),
                "{why}: spent"
            );
        }
    }

    #[test]
    fn a_first_look_appears_on_open_ground_cheers_and_goes_home() {
        for asleep in [false, true] {
            let why = if asleep { "asleep" } else { "awake" };
            let (mut p, home) = AgentPane::at_first_look(asleep);
            p.0.pet.note_room_landed();
            let frames: Vec<PetFrame> = (0..(4.0 / 0.016) as usize).map(|_| p.frame()).collect();
            let frolic = frames
                .iter()
                .position(|f| f.action == PetAction::Frolic)
                .unwrap_or_else(|| panic!("{why}: the landing frolics"));
            assert!(
                frames[..frolic].iter().any(|f| f.action == PetAction::Perk),
                "{why}: perked first"
            );
            assert_eq!(
                frames[frolic..]
                    .iter()
                    .map(|f| f.motes.iter().flatten().count())
                    .max(),
                Some(3),
                "{why}: its three ♪/♥ on glass together"
            );
            let stage = (frames[frolic].col, frames[frolic].row);
            let seen_elsewhere = frames[..frolic]
                .iter()
                .filter(|f| f.alpha > 0 && (f.col, f.row) != stage)
                .map(|f| (f.action, f.col, f.row))
                .collect::<Vec<_>>();
            assert!(
                seen_elsewhere.is_empty(),
                "{why}: the cat is seen only on its stage {stage:?}, never flying \
                 in: {seen_elsewhere:?}"
            );
            p.assert_home_on_clean_ground(why, Some(home));
        }
    }

    impl AgentPane {
        fn landing(&mut self) -> (Vec<PetFrame>, Option<CheerAir>) {
            self.0.pet.note_room_landed();
            let mut air = None;
            let frames = (0..(5.0 / 0.016) as usize)
                .map(|_| {
                    let f = self.frame();
                    air = self.0.pet.room_cheer_air.or(air);
                    f
                })
                .collect();
            (frames, air)
        }

        fn notes_on_glass(&self, frames: &[PetFrame]) -> (usize, usize) {
            let PaneGeom { cols, rows, cell } = self.2;
            let mut most = 0;
            let mut meets = 0;
            for f in frames.iter().filter(|f| f.alpha > 0) {
                let boxes: Vec<_> = f
                    .motes
                    .iter()
                    .flatten()
                    .filter(|m| {
                        matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart) && m.alpha > 0
                    })
                    .filter_map(|m| {
                        crate::kitty_pet::mote_box_px((m.col, m.row), m.scale, cell, (cols, rows))
                    })
                    .collect();
                most = most.max(boxes.len());
                for (i, a) in boxes.iter().enumerate() {
                    meets += boxes[i + 1..]
                        .iter()
                        .filter(|b| {
                            a.0 < b.0 + i32::from(b.2)
                                && b.0 < a.0 + i32::from(a.2)
                                && a.1 < b.1 + i32::from(b.2)
                                && b.1 < a.1 + i32::from(a.2)
                        })
                        .count();
                }
            }
            (most, meets)
        }
    }

    #[test]
    fn a_landing_in_a_narrow_pane_shows_its_notes() {
        for (cols, rows, cell, compact) in [
            (44, 16, (15, 28), false),
            (44, 16, (7, 14), false),
            (41, 16, (7, 14), true),
            (41, 30, (7, 14), false),
        ] {
            let why = format!("{cols} x {rows} at {} x {} px", cell.0, cell.1);
            let mut p = AgentPane::narrow_at(cols, rows, cell);
            assert!(p.parks_within(10.0), "{why}: fixture: the cat parks");
            let (frames, air) = p.landing();
            let frolic = frames
                .iter()
                .position(|f| f.action == PetAction::Frolic)
                .unwrap_or_else(|| panic!("{why}: the landing frolics"));
            let flights = frames[..frolic]
                .windows(2)
                .filter(|w| w[1].action == PetAction::Leap && w[0].action != PetAction::Leap)
                .count();
            assert!(
                flights <= 1,
                "{why}: {flights} flights, not one to its stage"
            );
            let (most, meets) = p.notes_on_glass(&frames[frolic..]);
            assert_eq!(most, 3, "{why}: its three ♪/♥ on glass together");
            assert_eq!(
                air.map(|a| a == CheerAir::Compact),
                Some(compact),
                "{why}: the stage's air ({air:?})"
            );
            if !compact {
                assert_eq!(meets, 0, "{why}: no two ♪/♥ tiles meet");
            }
        }
    }

    #[test]
    fn a_stage_is_reached_in_one_flight() {
        let mut p = AgentPane::narrow(60, (7, 14));
        assert!(p.parks_within(10.0), "fixture: the cat parks");
        p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 60.0));
        (p.0.pet.col, p.0.pet.row) = (20.0, 8.495);
        let (frames, air) = p.landing();
        let frolic = frames
            .iter()
            .position(|f| f.action == PetAction::Frolic)
            .expect("the landing frolics");
        let flights = frames[..frolic]
            .windows(2)
            .filter(|w| w[1].action == PetAction::Leap && w[0].action != PetAction::Leap)
            .count()
            + usize::from(frames[0].action == PetAction::Leap);
        assert_eq!(flights, 1, "one flight to its stage ({air:?})");
        let (most, meets) = p.notes_on_glass(&frames[frolic..]);
        assert_eq!(most, 3, "its three ♪/♥ on glass together");
        assert_eq!(meets, 0, "no two ♪/♥ tiles meet");
    }

    #[test]
    fn a_cat_at_the_panes_edge_cheers_facing_into_the_pane() {
        let mut p = AgentPane::narrow(60, (15, 28));
        assert!(p.parks_within(10.0), "fixture: the cat parks");
        let w = art_cols(15, 28);
        let edge = 60.0 - w - 0.2;
        (p.0.pet.col, p.0.pet.row) = (edge, 2.85);
        p.0.pet.facing_left = false;
        let (frames, air) = p.landing();
        assert_eq!(air, Some(CheerAir::Lanes(true)), "the air is to its left");
        let frolic = frames
            .iter()
            .position(|f| f.action == PetAction::Frolic)
            .expect("the landing frolics");
        assert!(
            frames[..=frolic]
                .iter()
                .all(|f| f.action != PetAction::Leap && (f.col - edge).abs() < 0.5),
            "it cheers where it stands, against the edge"
        );
        assert!(
            frames[frolic..]
                .iter()
                .take_while(|f| f.action == PetAction::Frolic)
                .all(|f| f.facing_left),
            "it frolics facing into the pane"
        );
        let (most, meets) = p.notes_on_glass(&frames[frolic..]);
        assert_eq!(most, 3, "its three ♪/♥ on glass together");
        assert_eq!(meets, 0, "no two ♪/♥ tiles meet");
    }

    #[test]
    fn a_flight_over_a_reserved_rule_reaches_its_stage_in_one() {
        let mut p = AgentPane::new();
        assert!(p.parks_within(10.0), "fixture: the cat parks at home");
        assert!(p.0.pet.console.on_glass, "fixture: on glass");
        p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 100.0));
        (p.0.pet.col, p.0.pet.row) = (3.0, 13.0);
        p.0.pet.note_room_landed();
        let mut bound_for = None;
        let mut frames = Vec::new();
        for _ in 0..(5.0 / 0.016) as usize {
            frames.push(p.frame());
            bound_for = bound_for.or(p.0.pet.room_cheer_stage);
        }
        let to_frolic = frames
            .iter()
            .position(|f| f.action == PetAction::Frolic)
            .expect("the landing frolics");
        let flights = frames[..to_frolic]
            .windows(2)
            .filter(|w| w[1].action == PetAction::Leap && w[0].action != PetAction::Leap)
            .count()
            + usize::from(frames[0].action == PetAction::Leap);
        assert_eq!(flights, 1, "one flight, over the rule, to its stage");
        let bound_for = bound_for.expect("it set out for a stage");
        let frolic = frames
            .iter()
            .position(|f| f.action == PetAction::Frolic)
            .expect("the landing frolics");
        let at = (frames[frolic].col, frames[frolic].row);
        assert!(
            (at.0 - bound_for.0).abs() * 15.0 < 0.5 && at.1 == bound_for.1,
            "it cheers on the stage it set out for: at {at:?}, bound for {bound_for:?}"
        );
        assert_eq!(
            frames[frolic..]
                .iter()
                .map(|f| f.motes.iter().flatten().count())
                .max(),
            Some(3),
            "its three ♪/♥ on glass together"
        );
        p.assert_home_on_clean_ground("the cheer over the rule", None);
    }

    #[test]
    fn a_home_under_the_footer_cheers_on_the_ground_below_it() {
        let mut p = AgentPane::new_warm(0);
        p.0.pet.col = 4.0;
        p.0.pet.row = 17.0;
        for _ in 0..200 {
            p.frame();
        }
        assert!(p.parks_within(10.0), "fixture: the cat parks at home");
        let home = (p.0.pet.col, p.0.pet.row);
        assert!(
            p.0.pet.console.on_glass && home.1 > 15.0,
            "fixture: its home is under the footer, on glass: {home:?} ({})",
            p.0.pet.console.reason
        );
        p.1 = Some(PetRect::new(12.0, 0.0, 1.0, 100.0));
        p.0.pet.note_room_landed();
        let frames: Vec<PetFrame> = (0..(4.0 / 0.016) as usize).map(|_| p.frame()).collect();
        let frolic = frames
            .iter()
            .position(|f| f.action == PetAction::Frolic)
            .expect("the landing frolics");
        assert!(
            frames[..frolic].iter().any(|f| f.action == PetAction::Perk),
            "perked first"
        );
        assert_eq!(
            frames[frolic..]
                .iter()
                .map(|f| f.motes.iter().flatten().count())
                .max(),
            Some(3),
            "its three ♪/♥ on glass together"
        );
        let stage = (frames[frolic].col, frames[frolic].row);
        assert!(
            stage.1 > home.1,
            "its stage is the open ground below its home {home:?}: {stage:?}"
        );
        p.assert_home_on_clean_ground("the home under the footer", Some(home));
    }

    #[test]
    fn a_key_before_a_landings_cheer_spends_it_unplayed() {
        let mut s = Scene::new();
        s.executing();
        s.pet.note_room_landed();
        s.input(PetInputKind::Text);
        s.term.process(b"x");
        let mut frames = Vec::new();
        let mut aloft = 0;
        for _ in 0..(4.0 / 0.016) as usize {
            frames.push(s.frame(0.016));
            aloft = aloft.max(s.pet.cheer_motes_aloft());
        }
        assert!(
            frames.iter().all(|f| f.action != PetAction::Frolic) && aloft == 0,
            "nothing plays"
        );
        assert!(
            s.pet.pending_cheer.is_none() && !s.pet.room_cheer,
            "spent, not owed"
        );
        assert!(
            s.pet.console.resident,
            "the running command has its watcher back"
        );
    }

    #[test]
    fn a_held_selection_drops_a_trick_and_never_replays_it() {
        let mut s = Scene::new();
        s.term.process(b"\x1b[3;3Hread this");
        s.term.text_selection_mut().start_selection(
            2,
            2,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        s.term
            .text_selection_mut()
            .update_selection(2, 10, SelectionSide::Right);
        for _ in 0..240 {
            s.frame(0.016);
            if !s.pet.needs_frames() {
                break;
            }
        }
        assert_eq!(s.pet.console_attention(), PetAttention::Reading);
        assert!(
            !s.pet.needs_frames(),
            "fixture: the reading resident is parked"
        );
        s.pet.note_trick(s.now, Trick::Roll, true);
        s.frame(0.016);
        assert_eq!(
            s.pet.pending_trick(),
            None,
            "dropped by the resident that owns the tick"
        );
        assert!(!s.pet.needs_frames(), "and a held selection still parks");
        s.term.text_selection_mut().clear();
        let frames = s.watch(3.0);
        assert!(
            frames.iter().all(|f| f.pose != PetGlyphId::PetRoll1),
            "released, it replays nothing"
        );
    }

    #[test]
    fn streamed_output_lets_a_trick_expire_and_the_resident_takes_its_perch_back() {
        let mut s = Scene::new();
        s.executing();
        s.pet.note_trick(s.now, Trick::Roll, true);
        let t0 = s.now;
        let mut gone = None;
        let mut rolled = false;
        for step in 0..625u32 {
            if step % 6 == 0 {
                s.term.process(if step % 12 == 0 {
                    b"\r\ncompiling another crate".as_slice()
                } else {
                    b"\r\ncompiling".as_slice()
                });
            }
            let f = s.frame(0.016);
            rolled |= f.pose == PetGlyphId::PetRoll1;
            if gone.is_none() && s.pet.pending_trick().is_none() {
                gone = Some((s.now - t0).as_secs_f32());
            }
        }
        let gone = gone.expect("the latch is finite");
        assert!(
            (TRICK_TTL..TRICK_TTL + 0.1).contains(&gone),
            "gone at +{gone} s: output re-stamps nothing, want {TRICK_TTL}"
        );
        assert!(!rolled, "and an expired trick is never performed");
        assert!(
            s.pet.console.resident,
            "the running command has its watcher back"
        );
        assert_eq!(s.pet.console_attention(), PetAttention::Output);
    }

    #[test]
    fn a_trick_typed_through_a_repair_is_still_performed() {
        let mut s = Scene::new();
        s.type_text("rot");
        for _ in 0..2 {
            s.input(PetInputKind::Delete);
            s.term.process(b"\x08 \x08");
            s.frame(0.05);
        }
        s.type_text("oll ");
        s.pet.note_trick(s.now, Trick::Roll, true);
        s.frame(0.016);
        assert_eq!(
            s.pet.pending_trick(),
            Some(Trick::Roll),
            "the repair's resident does not eat the word it was repairing"
        );
        let frames = s.watch(3.5);
        assert!(
            frames.iter().any(|f| f.pose == PetGlyphId::PetRoll1),
            "and the roll plays once the hand has stopped"
        );
    }

    #[test]
    fn a_seat_typed_through_a_repair_is_taken_and_held() {
        let mut s = Scene::new();
        s.type_text("sot");
        for _ in 0..2 {
            s.input(PetInputKind::Delete);
            s.term.process(b"\x08 \x08");
            s.frame(0.05);
        }
        s.type_text("it ");
        s.pet.note_trick(s.now, Trick::Sit, true);
        let frames = s.watch(3.0);
        let heard = frames
            .iter()
            .position(|f| f.action == PetAction::Perk)
            .expect("heard");
        let seated = frames[heard..]
            .iter()
            .filter(|f| f.pose == PetGlyphId::PetSitLookup)
            .count();
        assert!(
            seated as f32 * 0.016 > 1.0,
            "the commanded seat is on glass and held: {seated} frames"
        );
        assert_eq!(s.pet.trick_settle, Some(Trick::Sit), "and still is");
    }

    #[test]
    fn fresh_input_stands_a_commanded_seat_down_but_keeps_an_owed_latch() {
        let mut s = Scene::new();
        s.pet.note_trick(s.now, Trick::Sit, true);
        let frames = s.watch(1.0);
        assert!(
            frames.iter().any(|f| f.pose == PetGlyphId::PetSitLookup),
            "fixture: the commanded seat is on glass"
        );
        assert_eq!(s.pet.trick_settle, Some(Trick::Sit));
        s.input(PetInputKind::Text);
        let f = s.frame(0.016);
        assert!(
            s.pet.trick_settle.is_none(),
            "the seat stands down with the key"
        );
        assert_ne!(f.pose, PetGlyphId::PetSitLookup);

        s.pet.note_trick(s.now, Trick::Roll, true);
        s.pet.pending_cheer = Some(false);
        s.input(PetInputKind::Submit);
        s.term.process(b"\r\n> ");
        s.frame(0.016);
        assert_eq!(s.pet.pending_cheer, None, "fixture: the clear ran");
        assert_eq!(
            s.pet.pending_trick(),
            Some(Trick::Roll),
            "the latch is owed to the prompt after the Enter"
        );
        let frames = s.watch(3.5);
        assert!(
            frames.iter().any(|f| f.pose == PetGlyphId::PetRoll1),
            "and it plays there"
        );
    }

    #[test]
    fn a_forgiven_pet_only_submit_leaves_no_failed_result_on_the_console() {
        let failed_result = |pet_talk: bool| -> (bool, bool) {
            let mut s = Scene::new();
            s.type_text("sit");
            s.input(PetInputKind::Submit);
            if pet_talk {
                s.pet.note_trick_submit(s.now);
            }
            s.term.process(b"\r\nzsh: command not found: sit\r\n> ");
            s.frame(0.03);
            s.pet.note_command_done(s.now, true, Some(3));
            let hushed = s.pet.grieving();
            let mut inspected = false;
            for _ in 0..150 {
                s.frame(0.016);
                inspected |= s.pet.console_attention() == PetAttention::Result;
            }
            (hushed, inspected)
        };
        assert_eq!(
            failed_result(true),
            (false, false),
            "pet talk: no hush, no failed-result inspection"
        );
        let (hushed, _) = failed_result(false);
        assert!(hushed, "control: an ordinary fast failure is hushed");
    }

    #[test]
    fn an_enter_inside_the_heard_you_beat_still_gets_its_trick() {
        let rolling = |f: &PetFrame| {
            matches!(
                f.pose,
                PetGlyphId::PetRoll0 | PetGlyphId::PetRoll1 | PetGlyphId::PetRoll2
            )
        };
        let mut s = Scene::new();
        s.type_text("roll ");
        s.pet.note_trick(s.now, Trick::Roll, false);
        for _ in 0..120 {
            s.frame(0.016);
            if s.pet.trick_go.is_some() {
                break;
            }
        }
        assert_eq!(
            s.pet.trick_go,
            Some(Trick::Roll),
            "fixture: heard, verb owed"
        );
        let _ = s.watch(0.1);
        assert!(s.pet.trick_go.is_some(), "fixture: still inside the beat");
        s.input(PetInputKind::Submit);
        s.pet.note_trick(s.now, Trick::Roll, true);
        s.term.process(b"\r\n> ");
        let f = s.frame(0.016);
        assert!(
            s.pet.trick_go.is_none() && !rolling(&f),
            "the Enter took the beat"
        );
        assert_eq!(
            s.pet.pending_trick(),
            Some(Trick::Roll),
            "…and the confirmation still owes the roll"
        );
        let frames = s.watch(4.5);
        let rolls = frames
            .windows(2)
            .filter(|w| !rolling(&w[0]) && rolling(&w[1]))
            .count();
        assert_eq!(rolls, 1, "performed at the new prompt, once");
    }

    #[test]
    fn typing_after_repair_or_near_box_wall_never_sticks_on_caret_or_jiggles() {
        let mut s = Scene::new();
        s.type_text("hello");
        let _ = s.watch(0.5);
        s.input(PetInputKind::Delete);
        s.term.process(b"\x08 \x08");
        s.frame(0.016);
        for ch in "world!".chars() {
            s.type_text(&ch.to_string());
            let _ = s.watch(0.12);
            let f = s.frame(0.016);
            let cc = f32::from(s.term.cursor().col);
            assert!(
                f.col >= cc + 0.5 || f.col + art_cols(10, 20) <= cc + 0.1,
                "cat at col {} must not stick on advancing caret at col {cc}",
                f.col,
            );
        }

        s.term.process(b"\x1b[s\x1b[6;32H|\x1b[u");
        for ch in "abcd".chars() {
            s.type_text(&ch.to_string());
            for _ in 0..10 {
                let f = s.frame(0.02);
                let cr = f32::from(s.term.cursor().row);
                let cc = f32::from(s.term.cursor().col);
                let on_caret_row = (f.row - cr).abs() < 0.5;
                assert!(
                    !on_caret_row || f.col >= cc + 0.5 || f.col + art_cols(10, 20) <= cc + 0.1,
                    "cat at ({}, {}) must never park on caret ({cr}, {cc}) near box border",
                    f.col,
                    f.row
                );
            }
        }
    }

    #[test]
    fn faux_composer_caret_and_corner_park_prevent_bottom_left_stuck_kitty() {
        let mut s = Scene::new();
        s.term.process(
            b"\x1b[16;1H\xe2\x95\xad\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x95\xae\
              \x1b[17;1H\xe2\x94\x82 > hello world             \xe2\x94\x82\
              \x1b[18;1H\xe2\x95\xb0\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x95\xaf\
              \x1b[20;1H  26 files changed\x1b[20;1H\x1b[?25l",
        );
        let mut last = s.frame(0.016);
        for _ in 0..120 {
            s.now += Duration::from_secs_f32(0.02);
            let input = s.term.cell_frame(20, 80);
            let facts = PetWorldFacts::read(&s.term, 17);
            s.pet.observe_console(&input, &facts, PetPane::full(&input));
            s.pet.set_console_presentable(true);
            let c = s.term.cursor();
            last = s.pet.tick(PetSense {
                caret_drawn: s.term.cursor_visible(),
                now: s.now,
                caret: Some((c.row, c.col)),
                wrapped: false,
                rows: 20,
                cols: 80,
                cell_w: 10,
                cell_h: 20,
                reduced_motion: false,
                output_burst: false,
                pointer: None,
            });
        }
        assert!(
            !(last.row >= 18.5 && last.col <= 2.5),
            "kitty must not get stuck in bottom-left corner over status bar: got ({}, {})",
            last.col,
            last.row
        );
        assert!(
            (last.row - 16.0).abs() <= 1.5,
            "kitty should station at the boxed composer row (row 16), got ({}, {})",
            last.col,
            last.row
        );

        s.term.process(
            b"\x1b[16;1H\xe2\x95\xad\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x95\xae\
              \x1b[17;1H\xe2\x94\x82 > first line of prompt     \xe2\x94\x82\
              \x1b[18;1H\xe2\x94\x82   second line\xe2\x96\x88            \xe2\x94\x82\
              \x1b[19;1H\xe2\x95\xb0\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x95\xaf\
              \x1b[20;1H  26 files changed\x1b[?25l",
        );
        let input = s.term.cell_frame(20, 80);
        assert_eq!(
            detect_faux_composer_caret(&input, PetPane::full(&input)),
            Some((17, 15)),
            "multiline composer should locate the block cursor on the second composer line"
        );

        s.term.process(
            b"\x1b[17;1H\xe2\x94\x82 > \x1b[48;5;24m[Image #1]\x1b[0m hello \x1b[7m \x1b[0m       \xe2\x94\x82\
              \x1b[18;1H\xe2\x95\xb0\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x95\xaf\
              \x1b[19;1H                              \
              \x1b[20;1H  26 files changed\x1b[?25l",
        );
        let input = s.term.cell_frame(20, 80);
        assert_eq!(
            detect_faux_composer_caret(&input, PetPane::full(&input)),
            Some((16, 21)),
            "multi-cell background pill must not be mistaken for the 1-cell inverse caret at col 21"
        );
    }

    #[test]
    fn bordered_tui_box_places_station_in_interior_blank_gap_instead_of_on_words() {
        let mut s = Scene::new();
        let border = format!("+{}+", "-".repeat(78));
        let composer = format!("| > hello world{}|", " ".repeat(63));
        s.term.process(
            format!("\x1b[10;1H{border}\x1b[11;1H{composer}\x1b[12;1H{border}\x1b[11;7H")
                .as_bytes(),
        );
        s.pet.sense_ink(9, &[(0, 80), (0, 80), (0, 80)], None);
        let frames = s.watch(1.5);
        let last = frames.last().copied().expect("frame");
        assert!(
            last.col >= 15.0 && last.col <= 74.0,
            "kitty inside walled TUI box should step past the words into the interior blank space (col >= 15), got col {} row {}",
            last.col,
            last.row
        );
    }

    struct Console {
        term: Terminal,
        pet: PetBrain,
        now: Instant,
        rows: u16,
        cols: u16,
        chrome: Vec<PetRect>,
        overlays: Vec<PetRect>,
        ink: bool,
        burst: bool,
    }

    impl Console {
        fn new(rows: u16, cols: u16, screen: &str, chrome: Vec<PetRect>) -> Self {
            let mut term = Terminal::new(rows, cols);
            term.process(b"\x1b]11;#000000\x07");
            term.process(screen.as_bytes());
            Self {
                term,
                pet: PetBrain::default(),
                now: Instant::now(),
                rows,
                cols,
                chrome,
                overlays: Vec::new(),
                ink: false,
                burst: false,
            }
        }

        fn agent(rows: u16, cols: u16, screen: &str, chrome: Vec<PetRect>) -> Self {
            Self::new(
                rows,
                cols,
                &format!("\x1b]133;A\x07$ \x1b]133;B\x07claude\r\n\x1b]133;C\x07\x1b[2J{screen}"),
                chrome,
            )
        }

        fn repaint(&mut self, screen: &str, chrome: Vec<PetRect>) {
            self.term.process(b"\x1b[2J");
            self.term.process(screen.as_bytes());
            self.chrome = chrome;
        }

        fn watch(&mut self, secs: f32) -> (usize, Option<f32>, PetFrame) {
            let (mut leaps, mut parked) = (0, None);
            let mut last = self.frame();
            for i in 1..(secs * 60.0) as usize {
                last = self.frame();
                if last.action == PetAction::Leap {
                    leaps += 1;
                }
                if parked.is_none() && !self.pet.needs_frames() {
                    parked = Some(i as f32 / 60.0);
                }
            }
            (leaps, parked, last)
        }

        fn frame(&mut self) -> PetFrame {
            self.now += Duration::from_secs_f32(1.0 / 60.0);
            let input = self
                .term
                .cell_frame(usize::from(self.rows), usize::from(self.cols));
            let facts = PetWorldFacts::read(&self.term, 17);
            self.pet.observe_console_with_overlays(
                &input,
                &facts,
                PetPane::full(&input),
                &self.chrome,
                &self.overlays,
            );
            self.pet.set_console_presentable(true);
            if self.ink {
                let spans: Vec<(u16, u16)> = (0..usize::from(self.rows))
                    .map(|r| {
                        let text: Vec<char> =
                            self.term.row_text(r).unwrap_or_default().chars().collect();
                        let first = text.iter().position(|c| !c.is_whitespace());
                        let end = text.iter().rposition(|c| !c.is_whitespace());
                        first
                            .zip(end)
                            .map_or((0, 0), |(f, e)| (f as u16, e as u16 + 1))
                    })
                    .collect();
                let live = spans.iter().rposition(|&(f, e)| e > f).map(|r| r as u16);
                self.pet.sense_ink(0, &spans, live);
            }
            let c = self.term.cursor();
            self.pet.tick(PetSense {
                caret_drawn: self.term.cursor_visible(),
                now: self.now,
                caret: Some((c.row, c.col)),
                wrapped: false,
                rows: self.rows,
                cols: self.cols,
                cell_w: 10,
                cell_h: 20,
                reduced_motion: false,
                output_burst: self.burst,
                pointer: None,
            })
        }

        fn rest(&mut self, secs: f32) -> PetFrame {
            let mut last = self.frame();
            for _ in 1..(secs * 60.0) as usize {
                last = self.frame();
            }
            last
        }

        fn covered(&self, frame: &PetFrame) -> Vec<(usize, usize, PetCell)> {
            let (x0, x1, y0, y1) = frame
                .body_px(10, 20, self.cols, self.rows)
                .expect("the pet is on glass");
            let world = self.pet.console_world().expect("observed");
            let mut out = Vec::new();
            for r in y0.div_euclid(20)..(y1 + 19).div_euclid(20) {
                for c in x0.div_euclid(10)..(x1 + 9).div_euclid(10) {
                    let cell = world.cell(r as usize, c as usize);
                    if cell != PetCell::Clear {
                        out.push((r as usize, c as usize, cell));
                    }
                }
            }
            out
        }
    }

    fn claude_screen(cols: usize, rule: usize, history: &[(usize, &str)]) -> String {
        let line = "\u{2500}".repeat(cols);
        let mut out = String::from(
            "\x1b[1;1H \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code v2.1.304\
             \x1b[2;1H\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  Opus 5.5 \u{b7} API Usage Billing\
             \x1b[3;1H \u{259D}\u{259D}   \u{259D}\u{259D}   /private/tmp/lab/proj",
        );
        for (row, text) in history {
            out.push_str(&format!("\x1b[{};1H{text}", row + 1));
        }
        out.push_str(&format!(
            "\x1b[{};1H{line}\x1b[{};1H\u{276F} \x1b[7m \x1b[27m\x1b[{};1H{line}\
             \x1b[{};1H  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)\x1b[{};3H\x1b[?25l",
            rule + 1,
            rule + 2,
            rule + 3,
            rule + 4,
            rule + 2,
        ));
        out
    }

    fn claude_chrome(cols: usize, rule: usize, card: bool) -> Vec<PetRect> {
        let cols = cols as f32;
        let mut chrome = vec![
            PetRect::new(rule as f32, 0.0, 1.0, cols),
            PetRect::new(rule as f32 + 2.0, 3.0, 1.0, 5.0),
            PetRect::new(rule as f32 + 2.0, cols - 36.0, 1.0, 35.0),
        ];
        if card {
            chrome.push(PetRect::new(0.0, 0.0, 5.0, 87.0));
        }
        chrome
    }

    #[test]
    fn a_hidden_composer_caret_at_the_top_of_a_tall_pane_rests_clear_of_every_word() {
        let mut s = Console::new(
            40,
            120,
            &claude_screen(120, 5, &[]),
            claude_chrome(120, 5, true),
        );
        let last = s.rest(6.0);
        assert!(
            matches!(
                last.action,
                PetAction::Sit | PetAction::Loaf | PetAction::Sleep
            ),
            "fixture: the pet has come to rest, got {:?}",
            last.action
        );
        assert_eq!(
            s.pet.last_caret,
            Some((6, 2)),
            "the home is the real composer caret, not one synthesized for it"
        );
        let covered = s.covered(&last);
        assert!(
            covered.is_empty(),
            "the resting pet covers no word and no chrome; at ({}, {}) it covers {covered:?}",
            last.col,
            last.row
        );
        assert!(
            last.row > 8.5 && last.row < 11.0 && last.col < 8.0,
            "the nearest clean ground: under the footer, at the caret's side, got ({}, {})",
            last.col,
            last.row
        );
    }

    #[test]
    fn near_the_bottom_the_resting_pet_takes_the_footers_blank_tail_not_its_words() {
        let words =
            "Claude wrote this line all the way to the margin of the pane and on ".repeat(2);
        let history: Vec<(usize, &str)> = (3..=10).map(|row| (row, &words[..120])).collect();
        let mut s = Console::new(
            16,
            120,
            &claude_screen(120, 11, &history),
            claude_chrome(120, 11, false),
        );
        let last = s.rest(6.0);
        assert_eq!(
            s.pet.last_caret,
            Some((12, 2)),
            "fixture: the composer caret"
        );
        let covered = s.covered(&last);
        assert!(
            covered.is_empty(),
            "the resting pet covers no word and no chrome; at ({}, {}) it covers {covered:?}",
            last.col,
            last.row
        );
        assert!(
            last.row > 14.0 && last.col >= 38.0,
            "the footer's blank tail, right of `(shift+tab to cycle)`, got ({}, {})",
            last.col,
            last.row
        );
    }

    #[test]
    fn a_real_caret_keeps_its_stand_beside_it() {
        let words = "x".repeat(80);
        let mut screen = String::new();
        for row in 1..=10 {
            screen.push_str(&format!("\x1b[{row};1H{words}"));
        }
        screen.push_str("\x1b[11;1H$ ");
        let mut s = Console::new(24, 80, &screen, Vec::new());
        let last = s.rest(6.0);
        assert!(s.term.cursor_visible(), "fixture: a real, visible caret");
        assert_eq!(s.pet.last_caret, Some((10, 2)));
        let width = art_cols(10, 20);
        let (col, row) = s.pet.station_safe((10, 2), 80, 24, width);
        assert!(
            (row - 10.0).abs() < 0.25,
            "the escort's stand stays on the caret's row, got {row}"
        );
        assert!(
            (last.col - col).abs() <= ARRIVED && (last.row - 10.0).abs() < 0.25,
            "the resting pet stays at its caret's stand ({col}, {row}), got ({}, {})",
            last.col,
            last.row
        );
    }

    const TURN: [(usize, &str); 3] = [
        (5, "\x1b[48;2;55;55;55m\u{276F} hi there\x1b[K\x1b[0m"),
        (
            7,
            "\u{23FA} Hi! I am the local lab stub, not a model. (reply 1)",
        ),
        (9, "\u{273B} Cooked for 0s \u{b7} done 6:54 AM"),
    ];

    #[test]
    fn a_turn_under_the_resting_cat_bounds_once_to_the_clean_ground_by_the_caret() {
        let mut s = Console::agent(
            40,
            120,
            &claude_screen(120, 5, &[]),
            claude_chrome(120, 5, true),
        );
        let rested = s.rest(6.0);
        assert!(
            s.covered(&rested).is_empty() && rested.row > 8.5,
            "fixture: at rest under the fresh footer, at ({}, {})",
            rested.col,
            rested.row
        );
        s.repaint(&claude_screen(120, 11, &TURN), claude_chrome(120, 11, true));
        let mut hops = Hops::default();
        let mut parked = None;
        for i in 0..(4 * 60) {
            hops.frame(&mut s);
            if parked.is_none() && !s.pet.needs_frames() {
                parked = Some(i);
            }
        }
        let last = s.frame();
        assert!(
            parked.is_some() && hops.leaps == 1 && hops.asides == 0 && hops.missed.is_empty(),
            "the cat parks after one bound: parked at frame {parked:?}, {} flights, {} frames \
             stepped aside, landings off their aim {:?}, at ({}, {}) {:?}",
            hops.leaps,
            hops.asides,
            hops.missed,
            last.col,
            last.row,
            last.action
        );
        let seat = (last.col, last.row);
        let (leaps, _, last) = s.watch(16.0);
        assert!(
            leaps == 0 && (last.col - seat.0).abs() < 0.05 && (last.row - seat.1).abs() < 0.05,
            "and stays parked on its seat {seat:?}: {leaps} leap frames, at ({}, {})",
            last.col,
            last.row
        );
        let covered = s.covered(&last);
        assert!(
            covered.is_empty(),
            "at rest it covers no word and no chrome; at ({}, {}) it covers {covered:?}",
            last.col,
            last.row
        );
        assert!(
            last.row > 14.0 && last.col < 12.0,
            "on the clean ground under the new footer, by the caret, got ({}, {})",
            last.col,
            last.row
        );
    }

    fn composer_with(draft: &str) -> (String, Vec<PetRect>) {
        let rule = "\u{2500}".repeat(120);
        let chars: Vec<char> = draft.chars().collect();
        let lines: Vec<String> = if chars.is_empty() {
            vec![String::new()]
        } else {
            chars.chunks(118).map(|c| c.iter().collect()).collect()
        };
        let mut out = String::new();
        for row in 11..20 {
            out.push_str(&format!("\x1b[{};1H\x1b[2K", row + 1));
        }
        out.push_str(&format!("\x1b[12;1H{rule}"));
        for (i, line) in lines.iter().enumerate() {
            let lead = if i == 0 { "\u{276F} " } else { "  " };
            out.push_str(&format!("\x1b[{};1H{lead}{line}", 13 + i));
        }
        let last = lines.len() - 1;
        let caret_col = 2 + lines[last].chars().count();
        out.push_str("\x1b[7m \x1b[27m");
        let lower = 12 + lines.len();
        out.push_str(&format!(
            "\x1b[{};1H{rule}\x1b[{};1H  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)\x1b[{};{}H\x1b[?25l",
            lower + 1,
            lower + 2,
            13 + last,
            caret_col + 1
        ));
        let lower = lower as f32;
        let chrome = vec![
            PetRect::new(0.0, 0.0, 5.0, 87.0),
            PetRect::new(11.0, 0.0, 1.0, 120.0),
            PetRect::new(lower, 3.0, 1.0, 5.0),
            PetRect::new(lower, 84.0, 1.0, 35.0),
        ];
        (out, chrome)
    }

    #[test]
    fn a_cat_stopped_in_the_escorts_grace_never_sets_out_under_the_chrome_to_turn_back() {
        let rule = 20usize;
        let mark = vec![PetRect::new(rule as f32, 2.0, 1.0, 16.0)];
        let mut s = Console::agent(40, 120, &claude_screen(120, rule, &[]), mark);
        s.ink = true;
        s.rest(6.0);
        let line = rule as f32 + 1.0;
        s.pet.col = 9.0;
        s.pet.row = line;
        s.pet.speed = 0.0;
        s.pet.set_action(PetAction::Sit);
        s.pet
            .note_console_input(s.now - Duration::from_secs_f32(1.0), PetInputKind::Delete);
        let mut cols = Vec::new();
        for _ in 0..(3 * 60) {
            let f = s.frame();
            if f.alpha > 0 && f.action != PetAction::Leap {
                cols.push(f.col);
            }
        }
        let (mut dir, mut anchor) = (0.0f32, cols[0]);
        let mut turns = Vec::new();
        for &c in &cols {
            let d = c - anchor;
            if d.abs() >= 0.5 {
                if dir != 0.0 && d.signum() != dir {
                    turns.push((anchor, c));
                }
                dir = d.signum();
                anchor = c;
            }
        }
        let end = *cols.last().expect("the cat is drawn");
        assert!(
            turns.is_empty(),
            "the cat turned back on the composer's line: {turns:?} (from 9.00 to {end:.2})"
        );
        assert!(
            end >= 18.0,
            "fixture: it ends at its rest seat clear of the mark, at {end:.2}"
        );
    }

    fn resting_above_the_composer() -> Console {
        let mut s = Console::agent(
            16,
            120,
            &claude_screen(120, 5, &[]),
            claude_chrome(120, 5, true),
        );
        s.ink = true;
        s.rest(6.0);
        let (bottom, chrome) = composer_with("");
        s.repaint(
            &format!("{}{bottom}", claude_screen(120, 11, &TURN)),
            chrome,
        );
        let rested = s.rest(8.0);
        assert!(
            rested.row < 11.0 && s.covered(&rested).is_empty() && !s.pet.needs_frames(),
            "fixture: at rest above the composer's upper rule, on clean ground, at ({}, {})",
            rested.col,
            rested.row
        );
        s
    }

    type Landing = ((f32, f32), Vec<(f32, f32)>);

    #[derive(Default)]
    struct Hops {
        leaps: usize,
        asides: usize,
        missed: Vec<Landing>,
        seats: Vec<(f32, f32)>,
        aim: Option<(f32, f32)>,
        aloft: bool,
        at: Option<(f32, f32)>,
    }

    impl Hops {
        fn frame(&mut self, s: &mut Console) {
            let f = s.frame();
            let aloft = f.action == PetAction::Leap;
            if aloft && !self.aloft {
                self.leaps += 1;
                self.seats.clear();
            }
            if let Some(seat) = s.pet.console.rest_seat.get() {
                self.seats.push(seat);
            }
            if let Some(flight) = s.pet.flight.as_ref() {
                self.aim = Some((flight.to_col, flight.to_row));
            }
            if self.aloft && !aloft {
                let on =
                    |&(c, r): &(f32, f32)| (f.col - c).abs() <= 1.0 && (f.row - r).abs() < 0.25;
                if !self.seats.iter().any(on) && !self.aim.as_ref().is_some_and(on) {
                    self.seats.dedup();
                    self.missed
                        .push(((f.col, f.row), std::mem::take(&mut self.seats)));
                }
            }
            self.aloft = aloft;
            if s.pet.console.reason == "cursor-home" {
                self.asides += 1;
            }
            self.at = Some((f.col, f.row));
        }
    }

    #[test]
    fn typed_up_to_a_label_on_the_rule_the_cat_waits_on_the_line() {
        let mut s = resting_above_the_composer();
        let draft = "the quick brown fox jumps over the lazy dog ".repeat(4);
        let chrome_off = |chrome: Vec<PetRect>| {
            let lower = chrome[2].row;
            vec![
                PetRect::new(11.0, 88.0, 1.0, 32.0),
                PetRect::new(lower, 3.0, 1.0, 5.0),
                PetRect::new(lower, 84.0, 1.0, 35.0),
            ]
        };
        let mut hops = Hops::default();
        let mut on_line = None;
        let mut off_line = Vec::new();
        let mut wrap_leaps = None;
        for n in 1..=125usize {
            s.pet.note_console_input(s.now, PetInputKind::Text);
            let (bottom, chrome) = composer_with(&draft[..n]);
            s.term.process(bottom.as_bytes());
            s.chrome = chrome_off(chrome);
            if n == 119 {
                wrap_leaps = Some(hops.leaps);
            }
            for _ in 0..6 {
                hops.frame(&mut s);
                let (col, row) = hops.at.expect("a frame");
                if n < 119 && !hops.aloft {
                    if (row - 12.0).abs() < 0.01 {
                        on_line = on_line.or(Some((n, hops.leaps)));
                    } else if on_line.is_some() {
                        off_line.push((n, col, row, s.pet.action));
                    }
                }
            }
        }
        let (from_key, leaps_on) =
            on_line.expect("fixture: the cat comes onto the composer's line");
        let before_wrap = wrap_leaps.expect("the draft wraps") - leaps_on;
        let at_wrap = hops.leaps - wrap_leaps.unwrap_or(0);
        assert!(
            off_line.is_empty() && before_wrap == 0 && at_wrap == 1,
            "on the line from key {from_key}: {before_wrap} flights before the wrap, {at_wrap} \
             at it, frames off the line {:?}",
            &off_line[..off_line.len().min(4)]
        );
    }

    #[test]
    fn typed_to_the_right_margin_the_cat_stays_on_glass() {
        let mut s = resting_above_the_composer();
        let draft = "the quick brown fox jumps over the lazy dog ".repeat(3);
        let mut hidden = Vec::new();
        for n in 1..=118usize {
            s.pet.note_console_input(s.now, PetInputKind::Text);
            let (bottom, chrome) = composer_with(&draft[..n]);
            s.term.process(bottom.as_bytes());
            s.chrome = chrome;
            for _ in 0..6 {
                let f = s.frame();
                if f.alpha == 0 || s.pet.console.clipped {
                    hidden.push((n, f.col, f.row, s.pet.console.reason));
                }
            }
        }
        assert!(
            hidden.is_empty(),
            "{} frames hid the cat (key, col, row, why): {:?}",
            hidden.len(),
            &hidden[..hidden.len().min(4)]
        );
    }

    #[test]
    fn typing_into_an_agents_composer_never_sends_the_cat_hopping() {
        let mut s = resting_above_the_composer();
        let draft = "the quick brown fox jumps over the lazy dog ".repeat(4);
        let mut hops = Hops::default();
        let keys = (1..=150)
            .map(|n| (n, PetInputKind::Text))
            .chain((0..150).rev().map(|n| (n, PetInputKind::Delete)));
        let mut typed_leaps = 0;
        for (i, (n, kind)) in keys.enumerate() {
            if i == 150 {
                typed_leaps = hops.leaps;
            }
            s.pet.note_console_input(s.now, kind);
            let (bottom, chrome) = composer_with(&draft[..n]);
            s.term.process(bottom.as_bytes());
            s.chrome = chrome;
            for _ in 0..(2 + i % 2) {
                hops.frame(&mut s);
            }
        }
        for _ in 0..(3 * 60) {
            hops.frame(&mut s);
        }
        let deleted_leaps = hops.leaps - typed_leaps;
        assert!(
            hops.asides == 0 && typed_leaps <= 2 && deleted_leaps <= 3 && hops.missed.is_empty(),
            "the cat goes where the caret goes and lands where it aims: {typed_leaps} leaps \
             typing and {deleted_leaps} deleting, {} frames stepped aside, landings off their \
             aim {:?}",
            hops.asides,
            hops.missed
        );
        let (_, parked, last) = s.watch(6.0);
        assert!(
            parked.is_some() && s.covered(&last).is_empty(),
            "and parks on clean ground, at ({}, {})",
            last.col,
            last.row
        );
    }

    #[test]
    fn the_live_edge_hug_never_chases_across_the_hosts_chrome() {
        let mut s = resting_above_the_composer();
        let seat = (s.pet.col, s.pet.row);
        s.burst = true;
        let mut hops = Hops::default();
        let mut armed = 0;
        for _ in 0..(4 * 60) {
            hops.frame(&mut s);
            armed += usize::from(s.pet.hug_armed);
        }
        assert!(armed > 0, "fixture: the burst armed the live-edge hug");
        assert!(
            hops.asides == 0 && hops.leaps == 0,
            "the cat watches from its seat {seat:?}: {} leaps, {} frames stepped back off \
             the rule, at ({:.2}, {:.2})",
            hops.leaps,
            hops.asides,
            s.pet.col,
            s.pet.row
        );
    }

    fn shell_after_output() -> String {
        let mut screen = String::new();
        for i in 12..=40 {
            screen.push_str(&format!("line {i} of the output, words words words\r\n"));
        }
        screen.push_str("$ ");
        screen
    }

    #[test]
    fn a_growing_selection_moves_the_shell_pet_no_farther_than_home_reach() {
        let mut s = Console::new(30, 100, &shell_after_output(), Vec::new());
        let rested = s.rest(5.0);
        let home = (rested.col, rested.row);
        assert!(
            (home.1 - 29.0).abs() < 0.25,
            "fixture: at rest beside the prompt, at {home:?}"
        );
        let mut farthest = 0.0f32;
        for k in 1..=14u16 {
            let sel = s.term.text_selection_mut();
            sel.start_selection(
                29 - i32::from(k),
                2,
                SelectionSide::Left,
                SelectionType::Block,
            );
            sel.update_selection(29, 14 + 4 * k, SelectionSide::Right);
            for _ in 0..18 {
                let f = s.frame();
                if f.alpha > 0 {
                    farthest =
                        farthest.max((f.col - home.0).abs().max((f.row - home.1).abs() * 2.0));
                }
            }
        }
        assert!(
            farthest <= HOME_REACH + ARRIVED,
            "seen no farther than HOME_REACH from its stand, got {farthest}"
        );
        s.term.text_selection_mut().clear();
        let back = s.rest(4.0);
        assert!(
            (back.col - home.0).abs() <= ARRIVED && (back.row - home.1).abs() < 0.25,
            "back at its stand {home:?}, got ({}, {})",
            back.col,
            back.row
        );
    }

    #[test]
    fn a_selection_over_the_rest_seat_hides_the_cat_in_place() {
        let mut s = Console::agent(
            16,
            120,
            &claude_screen(120, 5, &[]),
            claude_chrome(120, 5, true),
        );
        s.rest(6.0);
        s.repaint(&claude_screen(120, 11, &TURN), claude_chrome(120, 11, true));
        let (_, parked, rested) = s.watch(6.0);
        assert!(parked.is_some(), "fixture: parked");
        let seat = (rested.col, rested.row);
        assert!(
            seat.1 < 11.0 && s.covered(&rested).is_empty(),
            "fixture: at rest beside the turn's last line, at {seat:?}"
        );
        let mut moved = 0.0f32;
        for k in 1..=8u16 {
            let sel = s.term.text_selection_mut();
            sel.start_selection(10, 0, SelectionSide::Left, SelectionType::Block);
            sel.update_selection(10 - i32::from(k), 20 + 9 * k, SelectionSide::Right);
            for _ in 0..20 {
                s.frame();
                moved = moved.max(
                    (s.pet.col - seat.0)
                        .abs()
                        .max((s.pet.row - seat.1).abs() * 2.0),
                );
            }
        }
        assert!(
            moved <= ARRIVED,
            "the cat keeps its seat under the selection, moved {moved}"
        );
        s.term.text_selection_mut().clear();
        let (_, parked, back) = s.watch(3.0);
        assert!(
            parked.is_some()
                && back.alpha > 0
                && (back.col - seat.0).abs() <= ARRIVED
                && (back.row - seat.1).abs() < 0.25,
            "back on its seat {seat:?}, seen: ({}, {}) alpha {}",
            back.col,
            back.row,
            back.alpha
        );
    }

    #[test]
    fn the_autopilot_menu_over_the_cat_hides_it_in_place() {
        let mut s = Console::agent(
            40,
            120,
            &claude_screen(120, 5, &[]),
            claude_chrome(120, 5, true),
        );
        let rested = s.rest(6.0);
        let seat = (rested.col, rested.row);
        assert!(
            s.covered(&rested).is_empty() && seat.1 > 8.5,
            "fixture: at rest under the footer, at {seat:?}"
        );
        s.overlays = vec![PetRect::new(8.0, 2.0, 11.0, 40.0)];
        let mut moved = 0.0f32;
        let mut seen = 0;
        for _ in 0..(3.0 * 60.0) as usize {
            let f = s.frame();
            moved = moved.max(
                (s.pet.col - seat.0)
                    .abs()
                    .max((s.pet.row - seat.1).abs() * 2.0),
            );
            if f.alpha > 0 {
                seen += 1;
            }
        }
        assert!(
            moved <= ARRIVED,
            "the cat keeps its seat under the menu, moved {moved}"
        );
        assert!(seen < 30, "and yields there, seen {seen} frames");
        s.overlays.clear();
        let (_, parked, back) = s.watch(3.0);
        assert!(
            parked.is_some()
                && back.alpha > 0
                && (back.col - seat.0).abs() <= ARRIVED
                && (back.row - seat.1).abs() < 0.25,
            "the menu closed, it is on its seat {seat:?}: ({}, {}) alpha {}",
            back.col,
            back.row,
            back.alpha
        );
    }

    #[test]
    fn a_command_list_redrawn_at_every_key_moves_the_resting_cat_once() {
        let list = |rows: usize| {
            let mut screen = claude_screen(120, 5, &[]);
            for row in 9..9 + rows {
                screen.push_str(&format!(
                    "\x1b[{};1H    /command-{row:<16}A long description of what the command does, \
                     and when to use it",
                    row + 1
                ));
            }
            screen.push_str("\x1b[7;3H");
            screen
        };
        let mut s = Console::agent(
            40,
            120,
            &claude_screen(120, 5, &[]),
            claude_chrome(120, 5, true),
        );
        let rested = s.rest(6.0);
        assert!(
            s.covered(&rested).is_empty() && rested.row < 12.0,
            "fixture: at rest under the fresh footer, at ({}, {})",
            rested.col,
            rested.row
        );
        s.repaint(&list(16), claude_chrome(120, 5, true));
        let (_, parked, moved) = s.watch(4.0);
        assert!(
            parked.is_some() && moved.row > 25.0 && s.covered(&moved).is_empty(),
            "the long list took its seat: it went past the list's end, at ({}, {})",
            moved.col,
            moved.row
        );
        let seat = (moved.col, moved.row);
        for rows in [12, 8, 4] {
            s.repaint(&list(rows), claude_chrome(120, 5, true));
            let (leaps, _, last) = s.watch(3.0);
            assert!(
                leaps == 0
                    && (last.col - seat.0).abs() <= ARRIVED
                    && (last.row - seat.1).abs() < 0.25,
                "a {rows}-row list leaves its seat {seat:?} clean: it stays, {leaps} leap \
                 frames, at ({}, {})",
                last.col,
                last.row
            );
        }
        s.repaint(&list(20), claude_chrome(120, 5, true));
        let (_, parked, last) = s.watch(4.0);
        assert!(
            parked.is_some() && last.row > 29.0 && s.covered(&last).is_empty(),
            "a longer list over its seat moves it again, past the end, at ({}, {})",
            last.col,
            last.row
        );
    }

    #[test]
    fn the_rest_seat_never_takes_the_gutter_of_a_command_list() {
        let list = [
            (
                9,
                "  /cd                         Change the working directory",
            ),
            (10, "  /clear                      Clear the conversation"),
            (11, "  /compact                    Compact the conversation"),
            (12, "  /config                     Open the settings"),
            (13, "  /copy                       Copy the last reply"),
        ];
        let mut screen = claude_screen(120, 5, &[]);
        for (row, text) in list {
            screen.push_str(&format!("\x1b[{};1H{text}", row + 1));
        }
        screen.push_str("\x1b[7;3H");
        let mut s = Console::agent(40, 120, &screen, claude_chrome(120, 5, true));
        let last = s.rest(6.0);
        let covered = s.covered(&last);
        assert!(covered.is_empty(), "no glyph under the cat: {covered:?}");
        let (x0, x1, y0, y1) = last.body_px(10, 20, 120, 40).expect("on glass");
        let cols = x0.div_euclid(10)..(x1 + 9).div_euclid(10);
        for row in y0.div_euclid(20)..(y1 + 19).div_euclid(20) {
            let Some((_, text)) = list.iter().find(|(r, _)| *r == row) else {
                continue;
            };
            let first = text.find(|c: char| c != ' ').unwrap_or(0) as i32;
            let end = text.trim_end().chars().count() as i32;
            assert!(
                cols.end <= first || cols.start >= end,
                "off the list's lines, not in their gutter: columns {cols:?} on row {row}, \
                 whose words run {first}..{end}, the cat at ({}, {})",
                last.col,
                last.row
            );
        }
    }

    #[test]
    fn full_transcript_above_the_composer_keeps_an_under_text_seat_without_hopping_or_bobbing() {
        let mut s = resting_above_the_composer();
        let full = "Claude wrote this line all the way to the margin of the pane and on ".repeat(2);
        let dense: Vec<(usize, &str)> = (5..=10).map(|row| (row, &full[..120])).collect();
        let (bottom, chrome) = composer_with("");
        s.repaint(
            &format!("{}{bottom}", claude_screen(120, 11, &dense)),
            chrome,
        );
        let rested = s.rest(4.0);
        assert!(
            matches!(rested.action, PetAction::Sit | PetAction::Loaf),
            "fixture: seated on the nearest under-text ground, got ({}, {}) {:?}",
            rested.col,
            rested.row,
            rested.action
        );
        let draft = "the quick brown fox jumps over the lazy dog ".repeat(2);
        let mut hops = Hops::default();
        let mut stands = 0usize;
        let mut contacts = 0usize;
        for (i, n) in (1..=60).chain((0..30).rev()).enumerate() {
            let kind = if i < 60 {
                PetInputKind::Text
            } else {
                PetInputKind::Delete
            };
            s.pet.note_console_input(s.now, kind);
            let (bottom, chrome) = composer_with(&draft[..n]);
            s.term.process(bottom.as_bytes());
            s.chrome = chrome;
            for _ in 0..3 {
                hops.frame(&mut s);
                if !hops.aloft && s.pet.action == PetAction::Stand {
                    stands += 1;
                }
                if s.pet.console_attention() == PetAttention::Contact {
                    contacts += 1;
                }
            }
        }
        assert!(
            hops.asides == 0 && hops.leaps <= 2 && hops.missed.is_empty(),
            "reachable under-text seat above the rule is kept: {} leaps, {} cursor-home asides, \
             missed {:?}",
            hops.leaps,
            hops.asides,
            hops.missed
        );
        assert_eq!(
            contacts, 0,
            "typing in a hidden-cursor composer never braces the cat as foreign contact"
        );
        assert!(
            stands <= 30,
            "seated cat on its rest seat does not pop into Stand on every key (got {stands} Stand frames)"
        );
    }

    #[test]
    fn interactive_typing_in_executing_block_holds_escort_lead_and_keeps_caret_gaze_past_loaf_after()
     {
        let mut s = Scene::new();
        s.executing();
        s.type_text("hello world");
        assert!(
            s.pet.held_lead > ARRIVED,
            "fixture: forward typing run earned a held lead ({})",
            s.pet.held_lead
        );
        let frames = s.watch(3.5);
        assert!(
            s.pet.console.resident && !s.pet.needs_frames(),
            "resident holds the escort's led stand and parks without oscillating (resident={}, needs_frames={})",
            s.pet.console.resident,
            s.pet.needs_frames()
        );
        let last = frames.last().expect("frames");
        assert_eq!(
            last.pose,
            PetGlyphId::PetSit,
            "interactive session in an executing block keeps PetSit past LOAF_AFTER without snapping to PetEdgeLean"
        );
    }

    #[test]
    fn gentle_pointer_stroke_pets_settled_console_resident_and_returns_to_watch() {
        let mut s = Scene::new();
        s.executing();
        let _ = s.watch(1.1);
        let col = s.pet.col;
        let row = s.pet.row;
        let mut frames = Vec::new();
        for step in 0..=30 {
            let u = step as f32 / 30.0;
            let px = col + 1.0 + u * 3.2;
            let py = row - 0.1;
            s.now += Duration::from_secs_f32(0.016);
            let input = s.term.cell_frame(20, 80);
            let facts = PetWorldFacts::read(&s.term, 17);
            s.pet.observe_console(&input, &facts, PetPane::full(&input));
            s.pet.set_console_presentable(true);
            let c = s.term.cursor();
            frames.push(s.pet.tick(PetSense {
                caret_drawn: true,
                now: s.now,
                caret: Some((c.row, c.col)),
                wrapped: false,
                rows: 20,
                cols: 80,
                cell_w: 10,
                cell_h: 20,
                reduced_motion: false,
                output_burst: false,
                pointer: Some((px, py)),
            }));
        }
        frames.extend(s.watch(2.5));
        assert!(
            frames.iter().any(|f| f.action == PetAction::Purr),
            "gentle pointer stroke over a settled console resident earns Purr"
        );
        assert!(
            s.pet.console.resident && s.pet.console_attention() == PetAttention::Output,
            "resident watch resumes once the affection hold finishes"
        );
    }

    #[test]
    fn terminal_bell_flinches_settled_console_resident_and_returns_to_watch() {
        let mut s = Scene::new();
        s.executing();
        assert!(!s.pet.needs_frames());
        s.pet.note_bell(s.now);
        assert!(
            s.pet.needs_frames(),
            "pending bell wakes the frame train even while a console resident is parked"
        );
        let first = s.frame(0.016);
        assert!(
            !s.pet.console.resident && (first.action == PetAction::Perk || s.pet.flinch_t > 0.0),
            "bell flinches the resident instead of being silently swallowed (resident={}, action={:?}, flinch_t={})",
            s.pet.console.resident,
            first.action,
            s.pet.flinch_t
        );
        let _ = s.watch(1.5);
        assert!(
            s.pet.console.resident && s.pet.console_attention() == PetAttention::Output,
            "resident watch resumes after the bell flinch completes"
        );
    }

    #[test]
    fn exploring_content_perch_orients_vertical_gaze_toward_anchor_row() {
        let mut s = Scene::new();
        s.executing();
        s.term.process(b"\x1b]133;D;0\x07");
        let _ = s.watch(RESULT_HOLD + 0.2);
        s.term.process(b"\x1b[13;17H");
        for _ in 0..90 {
            s.frame(0.016);
            if s.pet.console.resident
                && s.pet.console_attention() == PetAttention::Exploring
                && !s.pet.needs_frames()
            {
                break;
            }
        }
        assert_eq!(s.pet.console_attention(), PetAttention::Exploring);
        let settled = s.frame(0.016);
        assert_eq!(
            settled.pose,
            PetGlyphId::PetEdgeLookUp,
            "content-perch below the observed anchor looks up toward the anchor row"
        );
    }

    #[test]
    fn perch_trip_hop_cycles_rise_tuck_and_descend_poses() {
        let mut s = Scene::new();
        s.executing();
        s.term.process(b"\x1b[?25l");
        s.pet.console.cursor_home = None;
        s.pet.row -= 2.0;
        let mut poses = Vec::new();
        for _ in 0..30 {
            let f = s.frame(0.016);
            if s.pet.console.moving {
                poses.push(f.pose);
            } else if !poses.is_empty() {
                break;
            }
        }
        assert!(
            poses.contains(&PetGlyphId::PetLeapRise)
                && poses.contains(&PetGlyphId::PetHop)
                && poses.contains(&PetGlyphId::PetLeapDescend),
            "vertical PerchTrip hop should progress through rise, tuck, and descend poses: {poses:?}"
        );
    }

    #[test]
    fn room_mail_and_sibling_cues_play_over_executing_resident_and_return_to_watch() {
        let mut s = Scene::new();
        s.executing();
        s.pet.note_room_inbox(s.now, RoomInbox::default());
        s.pet.note_room_inbox(
            s.now,
            RoomInbox {
                unseen: 1,
                overdue: false,
                hold: false,
            },
        );
        assert!(
            !s.pet.needs_frames(),
            "room facts must never add a wake on their own"
        );
        let mut frames = Vec::new();
        let mut aloft = 0;
        for _ in 0..(2.0 / 0.016) as usize {
            frames.push(s.frame(0.016));
            aloft = aloft.max(s.pet.cheer_motes_aloft());
        }
        assert!(
            frames.iter().any(|f| f.action == PetAction::Perk),
            "unread mail perks the resident in place"
        );
        assert!(
            aloft >= 1,
            "unread mail spawns a musical note mote in the air"
        );
        assert!(
            s.pet.console.resident && s.pet.console_attention() == PetAttention::Output,
            "resident watch resumes once the mail beat finishes"
        );

        s.pet.note_room_sibling(s.now, Some((60.0, s.pet.row)));
        assert!(!s.pet.needs_frames());
        let first = s.frame(0.016);
        assert_eq!(first.action, PetAction::Perk);
        assert!(
            !first.facing_left,
            "resident turns toward the talking sibling pane on the right"
        );
        let _ = s.watch(1.0);
        assert!(
            s.pet.console.resident && s.pet.console_attention() == PetAttention::Output,
            "resident watch resumes once the sibling look finishes"
        );
    }

    #[test]
    fn word_cat_peek_bats_from_executing_resident_and_returns_to_watch() {
        let mut s = Scene::new();
        s.executing();
        assert!(!s.pet.needs_frames());
        s.pet.note_peek(s.now, s.pet.col + 6.0, s.pet.row);
        assert!(
            s.pet.needs_frames(),
            "nearby word-cat peek wakes the parked resident"
        );
        let frames = s.watch(3.0);
        assert!(
            frames.iter().any(|f| f.action == PetAction::Frolic),
            "resident hops to bat/greet the nearby word-cat peek"
        );
        assert!(
            s.pet.console.resident && s.pet.console_attention() == PetAttention::Output,
            "resident returns to its watch stand after batting the word-cat"
        );
    }

    #[test]
    fn v2_meteor_perk_and_star_catch_play_in_place_over_executing_resident() {
        let mut s = Scene::new();
        s.executing();
        let stand = (s.pet.col.to_bits(), s.pet.row.to_bits());
        let perk_at = s.now + Duration::from_millis(80);
        let perk_offer = PetOffer {
            perk_at: Some(perk_at),
            ..PetOffer::default()
        };
        assert!(s.pet.note_v2_offer(&perk_offer).is_none());
        assert!(
            !s.pet.needs_frames(),
            "v2 offer latches must never wake the frame train on their own"
        );

        let mut perked_at = None;
        for _ in 0..20 {
            let f = s.frame(0.016);
            assert!(s.pet.note_v2_offer(&perk_offer).is_none());
            if f.action == PetAction::Perk && perked_at.is_none() {
                perked_at = Some((s.now, f));
            }
        }
        let (at, perk_frame) = perked_at.expect("settled resident perks at the meteor arrival");
        assert!(at >= perk_at, "perk must not fire before perk_at");
        assert_eq!(
            (perk_frame.col.to_bits(), perk_frame.row.to_bits()),
            stand,
            "v2 perk in place must never relocate the resident"
        );
        let _ = s.watch(0.6);
        assert!(
            s.pet.console.resident && s.pet.console_attention() == PetAttention::Output,
            "resident resumes watching output after the v2 perk"
        );

        let star = StarCatch {
            x: (s.pet.col + 4.0) * 10.0,
            y: s.pet.row * 20.0,
            born: s.now,
            col: s.pet.col.round() as i32 + 4,
        };
        let catch_offer = PetOffer {
            catch: Some(star),
            ..PetOffer::default()
        };
        assert!(s.pet.note_v2_offer(&catch_offer).is_none());
        let bat_frame = s.frame(0.016);
        let caught = s.pet.note_v2_offer(&catch_offer);
        assert_eq!(
            caught,
            Some(star),
            "settled resident bats and returns the offered gold star"
        );
        assert_eq!(bat_frame.action, PetAction::Frolic);
        assert_eq!(bat_frame.pose, PetGlyphId::PetBat);
        assert_eq!(
            (bat_frame.col.to_bits(), bat_frame.row.to_bits()),
            stand,
            "v2 star catch bats in place without relocating"
        );
        let _ = s.watch(BAT_HOLD + 0.1);
        assert!(
            s.pet.console.resident && s.pet.console_attention() == PetAttention::Output,
            "resident resumes watching output after batting the star"
        );
    }

    #[test]
    fn pointer_gaze_tracks_moving_mouse_on_settled_resident_and_idles_when_gone() {
        let mut s = Scene::new();
        s.executing();
        assert!(!s.pet.needs_frames());

        let mut looked_up_right = false;
        for step in 0..10 {
            s.now += Duration::from_secs_f32(0.016);
            let input = s.term.cell_frame(20, 80);
            let facts = PetWorldFacts::read(&s.term, 17);
            s.pet.observe_console(&input, &facts, PetPane::full(&input));
            s.pet.set_console_presentable(true);
            let c = s.term.cursor();
            let px = s.pet.col + 10.0 + step as f32 * 1.2;
            let py = (s.pet.row - 3.0).max(0.0);
            let f = s.pet.tick(PetSense {
                caret_drawn: true,
                now: s.now,
                caret: Some((c.row, c.col)),
                wrapped: false,
                rows: 20,
                cols: 80,
                cell_w: 10,
                cell_h: 20,
                reduced_motion: false,
                output_burst: false,
                pointer: Some((px, py)),
            });
            if f.pose == PetGlyphId::PetEdgeLookUp && !f.facing_left {
                looked_up_right = true;
            }
        }
        assert!(
            looked_up_right && s.pet.needs_frames(),
            "settled resident turns its gaze toward a moving pointer above and to the right"
        );

        let settled = s.frame(0.016);
        assert!(
            !s.pet.needs_frames(),
            "pointer leaving the pane immediately releases the resident's frame train"
        );
        assert_eq!(
            settled.pose,
            PetGlyphId::PetEdgeLean,
            "output resident returns to its output gaze once the pointer leaves"
        );
    }

    #[test]
    fn interactive_caret_output_resident_flicks_ear_on_output_burst_edge_and_settles() {
        let mut s = Scene::new();
        s.executing();
        s.term.process(b"\x1b[4;17H");
        for _ in 0..120 {
            s.frame(0.016);
            if s.pet.console.resident && !s.pet.console.moving && !s.pet.needs_frames() {
                break;
            }
        }
        let base = s.frame(0.016);
        assert_eq!(s.pet.console_attention(), PetAttention::Output);
        assert_eq!(base.pose, PetGlyphId::PetSit);
        assert!(!s.pet.needs_frames());

        s.now += Duration::from_secs_f32(0.016);
        let input = s.term.cell_frame(20, 80);
        let facts = PetWorldFacts::read(&s.term, 17);
        s.pet.observe_console(&input, &facts, PetPane::full(&input));
        s.pet.set_console_presentable(true);
        let c = s.term.cursor();
        let twitch = s.pet.tick(PetSense {
            caret_drawn: true,
            now: s.now,
            caret: Some((c.row, c.col)),
            wrapped: false,
            rows: 20,
            cols: 80,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: true,
            pointer: None,
        });
        assert!(
            matches!(
                twitch.pose,
                PetGlyphId::PetSitEar | PetGlyphId::PetSitEarFar
            ),
            "interactive-caret resident flicks an ear on output burst edge, got {:?}",
            twitch.pose
        );
        assert!(s.pet.needs_frames(), "ear flick keeps the frame train live");

        let settled = *s.watch(TWITCH_DUR + 0.1).last().unwrap();
        assert_eq!(settled.pose, PetGlyphId::PetSit);
        assert!(!s.pet.needs_frames());
    }

    #[test]
    fn protected_surface_resident_retires_pending_bell_and_look_and_regrid_shifts_console_trip() {
        let mut s = Scene::new();
        s.term.process(b"\x1b[3;3Hread this");
        s.term.text_selection_mut().start_selection(
            0,
            0,
            SelectionSide::Left,
            SelectionType::Block,
        );
        s.term
            .text_selection_mut()
            .update_selection(19, 79, SelectionSide::Right);
        for _ in 0..240 {
            s.frame(0.016);
            if !s.pet.needs_frames() {
                break;
            }
        }
        assert_eq!(s.pet.console.reason, "protected-surface");
        assert!(!s.pet.needs_frames());

        s.pet.note_bell(s.now);
        s.pet.note_peek(s.now, s.pet.col + 20.0, s.pet.row);
        assert!(s.pet.pending_look.is_some());
        assert!(s.pet.needs_frames());
        s.frame(0.016);
        assert!(
            s.pet.pending_bell.is_none() && s.pet.pending_look.is_none() && !s.pet.needs_frames(),
            "protected-surface yield must retire pending_bell and pending_look on its tick"
        );

        s.pet.row = 5.0;
        s.pet.console.target = Some(PetRect::new(4.0, 10.0, 1.7, 3.4));
        s.pet.console.trip = Some(PerchTrip {
            from: (10.0, 5.0),
            to: (14.0, 6.0),
            elapsed: 0.1,
            duration: 0.4,
            hop: false,
        });
        assert!(s.pet.carry_with_regrid(3.0, 20));
        assert_eq!(s.pet.console.target.unwrap().row, 7.0);
        let trip = s.pet.console.trip.unwrap();
        assert_eq!((trip.from.1, trip.to.1), (8.0, 9.0));
    }
}
