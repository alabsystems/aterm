// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Located console attention within the resident brain. Input is intent;
//! only coherent displayed occupancy licenses contact and placement.

use super::*;
use crate::pet_world::TaskbarProgress;
use crate::pet_world::{
    PetAnchor, PetPane, PetRect, PetReserve, PetWorld, PetWorldFacts, PetWorldStamp,
};
use aterm_core::terminal::BlockState;

const INPUT_HOLD: f32 = 0.65;
// A typed kitty command is heard only once this hold has lapsed: inside it
// every tick wipes the verdict latches and the frame wears the typing pose,
// so a trick consumed there would be half-erased as it began. Structural,
// because the two constants live in different files.
const _: () = assert!(TRICK_SETTLE > INPUT_HOLD);
const EDIT_HOLD: f32 = 1.10;
const CONTACT_HOLD: f32 = 0.40;
const RESULT_HOLD: f32 = 1.20;
const REACTION_TTL: f32 = 2.0;
const CLEARANCE: f32 = 0.10;
const CONTACT_MARGIN: f32 = 0.45;
/// The most cells of one frame's typing [`typed_run`] discounts from contact.
const TYPED_RUN_MAX: usize = 4;
const PERCH_REACH: f32 = 24.0;
/// How far the cat goes for a landing's stage ([`PetBrain::cheer_stage`]):
/// columns either way, and rows up in tenths (never down: below the cat is
/// the prompt it keeps clear of, or the floor it stands on).
const CHEER_STAGE_COLS: f32 = 96.0;
const CHEER_STAGE_TENTHS_UP: u8 = 60;

/// A landing's stage ([`PetBrain::cheer_stage`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CheerStage {
    /// Where to land, `(col, row)`, the brain's own seat; `None` where the
    /// cat already stands on it.
    pub(super) at: Option<(f32, f32)>,
    /// How its ♪/♥ take the air there: in their lanes on the side the cat
    /// will face, or the compact spray.
    pub(super) air: CheerAir,
}

/// The caret is home. Located output may direct the gaze, but must not park
/// the resident across the pane. Rows cost twice a column in this bound.
const HOME_REACH: f32 = 6.0;
/// The most rows a body rides with the content round it in one observation
/// ([`PetBrain::content_ride`]): a composer growing a line or two at once.
const RIDE_ROWS_MAX: usize = 3;
/// How far a resident moved off a protected stand ([`PetBrain::console_station`])
/// looks for CLEAN ground — no glyph under its body — before it settles behind
/// text within [`HOME_REACH`]. Rows cost twice a column, and the two add as a
/// Euclidean length ([`PetWorld::nearest_clear_perch`]).
///
/// Wide enough for the ground a full-screen program leaves beside its own
/// chrome: under Claude Code 2.1.304 the composer row sits between two rules
/// aterm paints over (protected), so a 1.7-row body cannot stand at the caret,
/// and the nearest blank seat is two rows under the footer on a fresh screen
/// (5.7 away). On a full one it is beside the last line above the rule, or the
/// footer's blank tail on the last two rows: 30 to 53 away at 120 columns, by
/// how long those lines are. The ink-blind [`HOME_REACH`] search alone sat the
/// sleeping cat on the footer's words (`auto🐱de`, owner-eye-6, 2026-10-20).
const REST_REACH: f32 = 64.0;

/// How far from a kept rest seat the search may find it again: its own
/// place, give or take the float rounding of the foot row it is stored by
/// ([`PetBrain::console_station`]).
const SEAT_KEPT: f32 = 0.5;

/// How long the resident holds still for an unpainted caret before the
/// ordinary ladder takes over again. A DECTCEM hide means "I am repainting",
/// which takes milliseconds; past this the pet follows the (still known) caret
/// rather than sitting through a long build or pager with its cursor hidden.
const HIDDEN_HOLD_MAX: f32 = 0.25;

/// How long a spent interest (a content perch with no live event left) may
/// hold the resident before the pet gets its own life back; unbounded, the pet
/// could never breathe, blink, settle, sleep or notice the caret leaving.
const PERCH_DWELL: f32 = 3.0;

/// The visibility hold's dwell, in both directions. Per-frame presence can reverse every
/// ~158 ms during typing and a shorter hold only slows that strobe, so this is 1.6× that.
/// It is also the budget the pet has to walk out of the way before it must fade instead.
const VETO_DWELL: f32 = 0.25;
/// When an obstruction is real and sustained the pet yields over a ramp, not a
/// hard cut: a fifth of a second reads as the cat stepping aside. The ramp back
/// in is normally the pet's own [`FADE_IN`], because a body hidden long enough
/// to be reseated returns at a new station (`reseat_unshown_console_body`),
/// which retires the held verdict.
const VETO_FADE: f32 = 0.20;
/// An unbroken run of unknown verdicts this long (a surface that never
/// regains coherence) counts as obstruction, so a retired world cannot strand
/// an opaque pet on glass. Counts only unknowns about a body being emitted; a
/// frame with no body goes to [`VisibilityHold::idle`].
const VETO_BLIND: f32 = 0.50;
/// Coming back in front of the glyphs waits this long with the drawn body clear
/// of ink: [`VETO_DWELL`]'s value and reasoning (longer than a keystroke's
/// half-period, so a body crossing the last typed glyph cannot strobe).
const Z_DWELL: f32 = VETO_DWELL;

/// What the resident does about a body beside a live caret — see
/// [`PetBrain::caret_seat`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaretSeat {
    /// The body is where the escort left it: hold it there and perform.
    Hold,
    /// The body is elsewhere: the escort closes the distance, to the same
    /// stand, so a visible resident resigns the body for now.
    Escort,
    /// The stand is a protected surface with no correction in reach.
    Yield,
}

/// The pet's presence as a held state rather than a per-frame boolean. Fed the
/// three-valued clearance verdict for the emitted sprite rectangle, it answers
/// a 0..=1 cover factor: a `None` verdict neither flips the state nor resets a
/// run in progress, and contrary evidence must persist for [`VETO_DWELL`]
/// before the held verdict changes, the cover then ramping over [`VETO_FADE`].
#[derive(Clone, Copy, Debug)]
struct VisibilityHold {
    /// The held verdict: may the pet be on glass at all?
    shown: bool,
    /// Start of the current unbroken run of evidence contrary to `shown`.
    contrary_since: Option<Instant>,
    /// Start of the current unbroken run of "I cannot see there".
    blind_since: Option<Instant>,
    /// 0..=1, multiplied into the emitted alpha. Ramps toward `shown`.
    cover: f32,
    /// The instant the ramp last advanced; `None` before the first frame.
    at: Option<Instant>,
    /// Whether the hidden state was entered by fiat (a selection, or a
    /// placement search that came back empty) rather than by perception. Those
    /// yield at once, and are not charged the dwell on the way back either,
    /// since the dwell discounts perception wobble. Pinned by the
    /// `ProtectAll`/`Release` steps of
    /// `real_cursor_home_placement_conforms_and_rejects_the_historical_hidden_body`.
    by_fiat: bool,
}

impl Default for VisibilityHold {
    fn default() -> Self {
        // Shown and opaque: the pet's own arrival fade owns the ramp-in, and a
        // hold that started hidden would fight it.
        Self {
            shown: true,
            contrary_since: None,
            blind_since: None,
            cover: 1.0,
            at: None,
            by_fiat: false,
        }
    }
}

impl VisibilityHold {
    /// Advance one emitted frame. `verdict` is [`PetWorld::clearance`] over
    /// the sprite rectangle: `Some(true)` clear, `Some(false)` obstructed,
    /// `None` unobservable.
    fn update(&mut self, now: Instant, verdict: Option<bool>) -> f32 {
        let dt = self.advance(now);
        // A yield by fiat restores at once on the first clear verdict rather
        // than spending a dwell meant for perception wobble.
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
            // An unknown is not evidence either way: it neither advances a
            // run nor cancels one already under way.
            None => {}
        }
        self.ramp(dt)
    }

    /// Nothing was drawn at all, a different `None` from "I cannot see there":
    /// retire any incomplete dwell or blind run about a body no longer on glass
    /// and advance the ramp, never feeding [`VETO_BLIND`].
    fn idle(&mut self, now: Instant) -> f32 {
        self.contrary_since = None;
        self.blind_since = None;
        let dt = self.advance(now);
        self.ramp(dt)
    }

    /// Seconds since the last advance, clamped, with the clock moved on.
    fn advance(&mut self, now: Instant) -> f32 {
        let dt = self
            .at
            .map_or(0.0, |at| now.saturating_duration_since(at).as_secs_f32())
            .clamp(0.0, 2.0);
        self.at = Some(now);
        dt
    }

    /// Move the cover one step toward the held verdict.
    fn ramp(&mut self, dt: f32) -> f32 {
        let target = if self.shown { 1.0 } else { 0.0 };
        let step = dt / VETO_FADE;
        self.cover = if self.cover < target {
            (self.cover + step).min(target)
        } else {
            (self.cover - step).max(target)
        };
        self.cover
    }

    /// Put the envelope where the glass already is: hidden and settled, with no
    /// run in flight. For the paths that yield at once by design.
    fn hide_now(&mut self, now: Instant) {
        self.shown = false;
        self.cover = 0.0;
        self.contrary_since = None;
        self.blind_since = None;
        self.at = Some(now);
        self.by_fiat = true;
    }

    /// Nothing further will change without new evidence: no ramp in flight,
    /// no dwell counting down, and no blind run that could still time out.
    fn settled(&self) -> bool {
        let target = if self.shown { 1.0 } else { 0.0 };
        self.cover == target
            && self.contrary_since.is_none()
            && !(self.blind_since.is_some() && self.shown)
    }
}

/// The emitted z-order (`PetFrame::under_ink`) as a held state; recomputed per frame it
/// would flip at the keystroke rate. Going under is immediate (a body is never painted in
/// front of the user's text); coming back over needs the body clear of ink for
/// [`Z_DWELL`]. A frame that draws nothing releases the hold.
#[derive(Clone, Copy, Debug, Default)]
struct ZHold {
    under: bool,
    /// Start of the current unbroken run of clear frames spent `under`.
    clear_since: Option<Instant>,
}

impl ZHold {
    /// Advance one drawn frame; `ink` is real ink under the drawn body.
    fn update(&mut self, now: Instant, ink: bool) -> bool {
        if ink {
            self.under = true;
            self.clear_since = None;
        } else if self.under {
            let since = *self.clear_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= Duration::from_secs_f32(Z_DWELL) {
                *self = Self::default();
            }
        }
        self.under
    }

    /// When the clear run under way lands, so a quiet pet still surfaces.
    fn deadline(&self) -> Option<Instant> {
        self.clear_since
            .map(|since| since + Duration::from_secs_f32(Z_DWELL))
    }
}

/// Fold the hold's cover into an emitted opacity byte, floored at 1 while any
/// cover remains: hosts gate "is the pet on glass" on `alpha > 0`, and rounding
/// a faint body to zero would drop the sprite, its hit box and its motes.
fn cover_alpha(alpha: u8, cover: f32) -> u8 {
    if alpha == 0 || cover <= 0.0 {
        return 0;
    }
    ((f32::from(alpha) * cover.min(1.0)).round() as u8).max(1)
}

/// An admitted user-input intent, never a claim that the PTY displayed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PetInputKind {
    Text,
    Delete,
    Navigate,
    Submit,
    Paste,
    /// A kill chord that moves the caret (`^U`, `^W`): reaches the cursor
    /// engines as a kill; the pet reads it as an edit like a delete.
    Kill,
    /// A kill chord that leaves the caret where it is (`^K`).
    KillForward,
}

impl PetInputKind {
    #[must_use]
    pub fn parse(kind: &str) -> Option<Self> {
        match kind {
            "text" => Some(Self::Text),
            "delete" => Some(Self::Delete),
            "navigate" => Some(Self::Navigate),
            "submit" => Some(Self::Submit),
            "paste" => Some(Self::Paste),
            "kill" => Some(Self::Kill),
            "kill-forward" => Some(Self::KillForward),
            _ => None,
        }
    }
}

/// The source of the resident's current attention. Read-only introspection
/// exposes this decision without retaining or reproducing terminal text.
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

#[derive(Default)]
pub(super) struct ConsoleLife {
    world: Option<Box<PetWorld>>,
    /// The last retired map's buffer, never read: an unfocused surface is
    /// observed and retired every frame, and reusing this keeps that from
    /// allocating and dropping the whole bounded map each time.
    spare_world: Option<Box<PetWorld>>,
    presentable: bool,
    seq: u64,
    input: Option<InputEpisode>,
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
    /// Seconds the caret has been continuously unpainted (see
    /// [`PetBrain::hold_unpainted_caret`] and [`HIDDEN_HOLD_MAX`]). Zeroed by
    /// any frame that paints one, so a repainting console never accumulates.
    hidden_hold: f32,
    /// The last tick read an unpainted caret as the painted home.
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
    /// The brain's own `(col, row)` when [`Self::last_body`] was drawn: what
    /// the no-caret hold re-seats from. The drawn rect is not a seat — it
    /// carries the pose's lift and the row-0 pixel clamp.
    pub(super) last_seat: Option<(f32, f32)>,
    /// Whether the last emission put pixels on glass. Not inferable from
    /// `last_body` being `None`: the visibility veto also writes that, so a
    /// vetoed frame would read as never drawn.
    on_glass: bool,
    hold: VisibilityHold,
    z: ZHold,
    /// Last real visible caret on this coherent coordinate surface. This is
    /// placement memory only; it never enters the keyboard/motion sensor.
    cursor_home: Option<(u16, u16)>,
    contact_changed: bool,
    observed_seq: u64,
    progress_seq: u64,
    /// When the settled spent perch was taken; `Some` only while the content
    /// perch owns the resident with nothing live to attend. Its dwell end is
    /// offered by [`PetBrain::console_deadline`].
    perched_since: Option<Instant>,
    /// Synthesized composer caret when a TUI parks its hidden DECTCEM cursor
    /// at the bottom-left corner and draws an inverse/block cursor in the prompt.
    faux_caret: Option<(u16, u16)>,
    /// THE REST SEAT [`PetBrain::console_station`] last chose on clean
    /// ground, `(col, foot row)`: kept while it is still clean, reachable
    /// and within [`REST_REACH`] of the stand, so the cat moves only when
    /// its ground is taken. Written from that `&self` reading (both the
    /// escort and the resident ask it); dropped with the surface.
    pub(super) rest_seat: std::cell::Cell<Option<(f32, f32)>>,
    /// Whether the station [`PetBrain::console_station`] last answered is
    /// that rest seat: ground that stays put while the caret moves, so a
    /// flight to it aims at it, with no caret drift
    /// ([`PetBrain::aims_ahead`]). Aimed ahead like a caret's stand, a hop
    /// to a seat 64 columns off landed five past it.
    pub(super) rest_seated: std::cell::Cell<bool>,
    /// THE ROWS AS THE LAST TWO DIFFERENT OBSERVATIONS WROTE THEM: one hash
    /// of each pane row's characters (`row_sig`), and the same for the last
    /// observation whose rows differed (`row_sig_prev`), on one surface and
    /// grid. What [`PetBrain::content_ride`] reads to tell that the rows
    /// round the body moved down together (a composer growing a line pushes
    /// its rule and the footer under it a row down) and to carry the body
    /// with them.
    row_sig: Vec<u64>,
    row_sig_prev: Vec<u64>,
    /// Scratch for the observation being hashed, kept for its capacity.
    row_sig_next: Vec<u64>,
}

/// One pane row's characters as a hash (FNV-1a), for [`ConsoleLife::row_sig`];
/// 0 for a row with nothing written on it.
fn row_signature(row: &[aterm_core::terminal::RenderCell]) -> u64 {
    let mut blank = true;
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for cell in row {
        blank &= matches!(cell.ch, ' ' | '\0');
        h = (h ^ u64::from(u32::from(cell.ch))).wrapping_mul(0x0100_0000_01b3);
    }
    if blank { 0 } else { h.max(1) }
}

impl ConsoleLife {
    /// A fresh console life for a retired coordinate space. It keeps only the
    /// world's buffer and row-signature scratch vectors, retired and cleared
    /// as the spare capacity the next observation fills.
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
        let mut row_sig_next = core::mem::take(&mut self.row_sig_next);
        row_sig.clear();
        row_sig_prev.clear();
        row_sig_next.clear();
        Self {
            spare_world,
            row_sig,
            row_sig_prev,
            row_sig_next,
            ..Self::default()
        }
    }
}

impl PetBrain {
    /// Exactly one note per admitted gesture/bundle. A later output echo
    /// may complete an explicit witness, but never creates another input.
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
            self.console.repair_until = Some(now + Duration::from_secs_f32(EDIT_HOLD));
        } else {
            self.console.repair_until = None;
            self.console.repair_target = None;
        }
        self.console.seq = self.console.seq.wrapping_add(1).max(1);
        self.console.input = Some(InputEpisode {
            seq: self.console.seq,
            kind,
            at: now,
            target: self.last_caret.map(|(r, c)| (f32::from(r), f32::from(c))),
        });
        // The kitty command's one re-stamp: a latched trick waits out the user's own
        // typing, one note per admitted press. The move sensor must not re-stamp (a build
        // log would hold the latch for its whole cap); navigation, paste and kill chords
        // re-stamp nothing (the listener has already poisoned or reset the line).
        if matches!(
            kind,
            PetInputKind::Text | PetInputKind::Delete | PetInputKind::Submit
        ) && let Some(trick) = self.pending_trick.as_mut()
        {
            trick.stamped = now;
        }
    }

    #[must_use]
    pub fn console_input_seq(&self) -> u64 {
        self.console.seq
    }

    /// The console world the last observation read, as the pet judges a
    /// seat by it (`None` before any): what a host proves its exclusions
    /// against — a cell the host painted chrome on reads
    /// [`crate::pet_world::PetCell::Protected`] here or the pet may sit on it.
    #[must_use]
    pub fn console_world(&self) -> Option<&PetWorld> {
        self.console.world.as_deref()
    }

    #[must_use]
    pub fn console_input_consumed_seq(&self) -> u64 {
        self.console.consumed
    }

    #[must_use]
    pub fn console_input_kind(&self) -> Option<PetInputKind> {
        self.console.input.map(|e| e.kind)
    }

    #[must_use]
    pub fn console_attention(&self) -> PetAttention {
        self.console.attention
    }

    #[must_use]
    pub fn console_reason(&self) -> &'static str {
        if self.console.reason.is_empty() {
            "none"
        } else {
            self.console.reason
        }
    }

    #[must_use]
    pub fn console_anchor_id(&self) -> Option<u64> {
        self.console.anchor.map(|a| a.block_id)
    }

    #[must_use]
    pub fn console_event_seq(&self) -> u64 {
        self.console.source_seq
    }

    #[must_use]
    pub fn pose_name(&self) -> &'static str {
        PET_GLYPHS[self.species.skin(self.last_pose) as usize].id
    }

    /// Clear occupancy is read from the exact cell plane being presented.
    /// Calling this observer does not advance an animation or an input seq.
    pub fn observe_console(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
    ) {
        self.observe_console_with_exclusions(input, facts, pane, &[]);
    }

    /// Cells the last world observation classified (at most
    /// `MAX_WORLD_ROWS` × `MAX_WORLD_COLS` per presented frame), so a running
    /// product can report the cost. Zero before any coherent observation and
    /// after a refused one ([`PetWorld::retire`] clears the count).
    #[must_use]
    pub fn observed_cells(&self) -> usize {
        self.console
            .world
            .as_ref()
            .map_or(0, |world| world.examined_cells())
    }

    pub fn observe_console_with_exclusions(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
        exclusions: &[PetRect],
    ) {
        self.observe_console_with_reserve(input, facts, pane, PetReserve::protected(exclusions));
    }

    /// As [`Self::observe_console_with_exclusions`], with the host's
    /// transient overlays apart (`overlays`: the Autopilot menu, the find
    /// bar; [`PetWorld::observe_with_overlays`]). They protect their cells
    /// alike, but the rest seat is solved without them, as without a live
    /// selection ([`PetBrain::console_station`]).
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

    /// The host's late overlays told apart ([`PetReserve`]): its chrome and
    /// its transient overlays protected, the person's typed glyphs (the
    /// predicted echo, the IME preedit) as ink.
    pub fn observe_console_with_reserve(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
        reserve: PetReserve<'_>,
    ) {
        // The hidden-cursor resident holds its last drawn body, so keep that
        // body's cells inside the observation window (centering a large pane
        // would make an unchanged blank home look unknown). A new
        // surface/history/geometry cannot inherit the old body's hint.
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
        let old_ink = self
            .console
            .last_body
            .and_then(|r| local_ink(world, r, typed));
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
            // Producers retire the old coordinate owner before dispatching
            // the new owner's first input. A missed retirement is fail-closed.
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

    /// Hash this observation's pane rows ([`ConsoleLife::row_sig`]). Rows
    /// unchanged since the last observation keep both sets as they are, so a
    /// second read of one frame, or a frame that only moved the cat, still
    /// sees the move before it. A new surface or grid has no rows before it.
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
        let rows = pane.rows.min(input.cells.len().saturating_sub(pane.row));
        // Coordinate/content identity does not cover render-time projection:
        // SCP can reorder these characters without advancing content_seq.
        // Read the actual pane rows before deciding that their signatures match.
        console.row_sig_next.clear();
        console.row_sig_next.reserve(rows);
        for r in pane.row..pane.row + rows {
            let row = &input.cells[r];
            let end = (pane.col + pane.cols).min(row.len());
            console
                .row_sig_next
                .push(row_signature(row.get(pane.col..end).unwrap_or(&[])));
        }
        let same_grid = prior.is_some_and(|p| p.surface == stamp.surface)
            && console.row_sig.len() == console.row_sig_next.len();
        if !same_grid {
            console.row_sig_prev.clear();
            std::mem::swap(&mut console.row_sig, &mut console.row_sig_next);
        } else if console.row_sig != console.row_sig_next {
            std::mem::swap(&mut console.row_sig_prev, &mut console.row_sig);
            std::mem::swap(&mut console.row_sig, &mut console.row_sig_next);
        }
    }

    /// How many rows the content round `rect` moved down (1 to
    /// [`RIDE_ROWS_MAX`]) between the last two different observations: every
    /// row from the one above the body to its last one is found again that
    /// many rows lower, and they are not all blank (a blank row matches any
    /// other). `None` when the rows did not move together.
    fn content_ride(&self, rect: PetRect) -> Option<f32> {
        let (cur, prev) = (&self.console.row_sig, &self.console.row_sig_prev);
        if prev.is_empty() || prev.len() != cur.len() || !rect.valid() {
            return None;
        }
        let top = (rect.row.floor() as i64 - 1).max(0) as usize;
        let bottom = ((rect.row + rect.rows).ceil() as i64 - 1).max(0) as usize;
        let prev_rows = prev.get(top..=bottom)?;
        if prev_rows == &cur[top..=bottom] || !prev_rows.iter().any(|&s| s != 0) {
            return None;
        }
        (1..=RIDE_ROWS_MAX).find_map(|k| {
            (bottom + k < cur.len() && prev_rows == &cur[top + k..=bottom + k]).then_some(k as f32)
        })
    }

    /// Exact locomotion occupancy where the map covers the whole footprint.
    /// `None` retains the span fallback; it never licenses an unobserved gap.
    pub(super) fn observed_ink_overlaps(&self, col: f32, row: f32, width: f32) -> Option<bool> {
        let (a, b) = (col + INK_PAD, col + width - INK_PAD);
        // A first/last glyph span is a hull, not solid ink: coding consoles
        // animate sparse particles around a stationary composer. Use the
        // console's exact glyph occupancy for a completely observed footprint.
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
                return (first..end).try_fold(false, |occupied, c| {
                    world.locomotion_ink_at(r, c).map(|ink| occupied || ink)
                });
            }
        }
        None
    }

    /// An observed but incoherent surface cannot license new locomotion from
    /// the older ink spans. An unfed legacy host has no such observation.
    pub(super) fn console_observation_incoherent(&self) -> bool {
        self.console.presentable
            && self
                .console
                .world
                .as_ref()
                .is_some_and(|world| world.stamp().is_none())
    }

    /// Ownership/custody is independent of whether a live caret exists.
    /// This permits a valid reading anchor without manufacturing a caret.
    pub fn set_console_presentable(&mut self, presentable: bool) {
        if self.console.presentable != presentable {
            // Custody changed hands. A frozen ramp and a stale verdict from
            // the previous custodian are not evidence about this one.
            self.console.hold = VisibilityHold::default();
            self.console.z = ZHold::default();
            self.console.rest_seat.set(None);
            self.console.rest_seated.set(false);
        }
        self.console.presentable = presentable;
    }

    #[must_use]
    pub fn has_reading_interest(&self) -> bool {
        self.console
            .world
            .as_ref()
            .is_some_and(|w| w.selection_target().is_some())
    }

    /// Natural, pixel-rounded sprite coverage in fractional grid cells.
    fn console_body(&self, sense: PetSense) -> PetRect {
        still_body_rect(
            (self.col, self.row),
            Stance::NEUTRAL,
            (sense.cell_w, sense.cell_h),
            (sense.cols, sense.rows),
        )
    }

    /// A LANDING'S STAGE (§11.3 of `docs/DESIGN-harness-autopilot-2026-10-15.md`):
    /// ground where the cat can cheer with its ♪/♥ legible — its body, and
    /// the air the three of them fill side by side in their lanes on the
    /// side it faces ([`PetBrain::cheer_air`]), clear of ink, the caret's
    /// ring and anything protected: [`PetWorld::clear`], the law every mote
    /// on glass is held to (`finish_console_frame`). The side it faces is
    /// tried first, then the other, and the cat cheers facing the side the
    /// stage kept ([`CheerStage::air`]); only where neither side's lanes
    /// find air within reach (a narrow pane) does the stage keep the compact
    /// spray's ([`CheerAir::Compact`]), its ♪/♥ close together but on glass.
    /// An agent's transcript leaves one blank row between its lines, too
    /// little for a ♪, so the cat goes to the nearest open ground beside a short line, on its
    /// own rows or up to six above (where nothing above is open, as a cat
    /// resting under an agent's footer finds it, up to six below), by a path
    /// that crosses nothing protected (the host's chrome; a body already
    /// over some is judged by where it lands). `None` with no observed world
    /// (the pane is all the cat's), or where no stage is within reach: it
    /// cheers where it stands, as it faces. A stage with no `at` where it
    /// already stands on one. Else where to land, `(col, row)`, the brain's
    /// own seat: `prefer` (the stage it last set out for) while it is still
    /// clear and reachable, else the nearest. The search below is
    /// pinned by `a_home_under_the_footer_cheers_on_the_ground_below_it` and
    /// `a_landing_in_a_narrow_pane_shows_its_notes`.
    pub(super) fn cheer_stage(
        &self,
        sense: PetSense,
        width: f32,
        prefer: Option<(f32, f32)>,
    ) -> Option<CheerStage> {
        if !self.console.presentable {
            return None;
        }
        let world = self.console.world.as_ref()?;
        world.stamp()?;
        let body = self.console_body(sense);
        // The stage for `air` with the cat seated at `(col, row)`: its body
        // there as it will be drawn (rounded to the pixel, held inside the
        // grid), and the air over it. A seat the search lands on is judged
        // by exactly the rect the cat will find there on arrival, or a body
        // half a pixel off its shifted copy reads the ink beside it and hops
        // half a pixel more.
        let stage_at = |air: CheerAir, (col, row): (f32, f32)| {
            let body = still_body_rect(
                (col, row),
                Stance::NEUTRAL,
                (sense.cell_w, sense.cell_h),
                (sense.cols, sense.rows),
            );
            let (left, top, right, bottom) =
                self.cheer_air((col, row), (sense.cell_w, sense.cell_h), width, air);
            let (row0, col0) = (top.min(body.row), left.min(body.col));
            PetRect::new(
                row0,
                col0,
                bottom.max(body.row + body.rows) - row0,
                right.max(body.col + body.cols) - col0,
            )
        };
        let shifted =
            |r: PetRect, (dc, dr): (f32, f32)| PetRect::new(r.row + dr, r.col + dc, r.rows, r.cols);
        // The path is the swept body. A body the map already holds over
        // something protected (a sleeper whose feet rest, to the pixel, on a
        // rule the host's chrome reserves) would sweep it into every corridor
        // and never leave; its hop off is judged by where it lands, as the
        // eviction's is.
        let from =
            (world.under_text_clearance_past_caret(body, CLEARANCE) != Some(false)).then_some(body);
        let reachable = |air: CheerAir, (dc, dr): (f32, f32)| {
            world.clear(stage_at(air, (self.col + dc, self.row + dr)), CLEARANCE)
                && from.is_none_or(|from| {
                    world.under_text_corridor_clear_past_caret(
                        from,
                        shifted(from, (dc, dr)),
                        CLEARANCE,
                    )
                })
        };
        // Half a pixel, the most a seat's rounding moves the body: a stage
        // aligned to a column is aligned that much right of it.
        let half_px = 0.5 / f32::from(sense.cell_w.max(1));
        // The lanes either side, the side it faces first; then, only where
        // neither finds air within reach, the compact spray.
        let tiers = [
            [
                Some(CheerAir::Lanes(self.facing_left)),
                Some(CheerAir::Lanes(!self.facing_left)),
            ],
            [Some(CheerAir::Compact), None],
        ];
        for tier in tiers {
            let stages = tier.map(|opt| {
                opt.map(|air| {
                    let stage = stage_at(air, (self.col, self.row));
                    let center = stage.col - CLEARANCE - half_px;
                    let c_lo =
                        ((center - CHEER_STAGE_COLS).floor().max(0.0) as u16).min(sense.cols);
                    let c_hi = (((center + CHEER_STAGE_COLS).ceil().max(0.0) as u16)
                        .saturating_add(1))
                    .min(sense.cols);
                    (air, stage, c_lo, c_hi)
                })
            });
            if let Some(&(air, _, _, _)) = stages
                .iter()
                .flatten()
                .find(|(_, stage, _, _)| world.clear(*stage, CLEARANCE))
            {
                return Some(CheerStage { at: None, air });
            }
            // A cat that came down off the stage it set out for goes there
            // again: the search below climbs first, and the stage may lie
            // below where it came down.
            if let Some((col, row)) = prefer
                && let Some(&(air, _, _, _)) = stages
                    .iter()
                    .flatten()
                    .find(|&&(air, _, _, _)| reachable(air, (col - self.col, row - self.row)))
            {
                return Some(CheerStage {
                    at: Some((col, row)),
                    air,
                });
            }
            let mut best: Option<(f32, (f32, f32), CheerAir)> = None;
            // Up first, as far as six rows; then, only where nothing above is
            // open, as far down. A cat at rest on the clean ground under an
            // agent's footer (`console_station`'s rest seat) has the footer's
            // words over its head and the composer's rules beyond them, which
            // it may not cross, while the blank rows under it are open air:
            // its stage is below.
            for dir in [-1.0, 1.0] {
                for step in 0..=CHEER_STAGE_TENTHS_UP {
                    if dir > 0.0 && step == 0 {
                        continue;
                    }
                    let dr = dir * 0.1 * f32::from(step);
                    let row_dist = 2.0 * dr.abs();
                    if row_dist > CHEER_STAGE_COLS || best.is_some_and(|(d, _, _)| row_dist >= d) {
                        break;
                    }
                    for &(air, stage, c_lo, c_hi) in stages.iter().flatten() {
                        // Unseen cells are never clear, so the grid bounds the
                        // search and the map's coverage window bounds the
                        // answers.
                        for c in c_lo..c_hi {
                            let dc = f32::from(c) + CLEARANCE + half_px - stage.col;
                            let distance = row_dist.max(dc.abs());
                            if distance > CHEER_STAGE_COLS
                                || best.is_some_and(|(d, _, _)| distance >= d)
                            {
                                continue;
                            }
                            if reachable(air, (dc, dr)) {
                                best = Some((distance, (dc, dr), air));
                            }
                        }
                    }
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

    /// A LANDING SEEN AT FIRST LOOK (§11.3): a cat off glass when its cheer
    /// comes due (its home under the host's chrome, where the console law
    /// hides it, or not yet faded in) has no flight anyone could watch, and a
    /// flight from a refused body is stepped aside on its first frame, the
    /// whole arc translated with it, so it lands off its stage. It appears on
    /// the stage instead ([`Self::cheer_stage`]), fading in there as any
    /// unshown body is re-seated ([`Self::reseat_unshown_console_body`]).
    /// Whether it did, which is the stage's first trip: `false` on glass,
    /// and `false` with no observed world or no open ground within reach,
    /// where the search is spent and it cheers where it stands.
    pub(super) fn appear_on_cheer_stage(&mut self, sense: PetSense, width: f32) -> bool {
        if self.console.on_glass || self.flight.is_some() {
            return false;
        }
        let stage = self.cheer_stage(sense, width, None);
        self.room_cheer_air = stage.map(|s| s.air);
        let Some((col, row)) = stage.and_then(|s| s.at) else {
            self.room_cheer_trips = CHEER_STAGE_TRIPS;
            return false;
        };
        self.room_cheer_trips = 1;
        self.room_cheer_stage = Some((col, row));
        if let Some(CheerAir::Lanes(facing_left)) = self.room_cheer_air {
            self.facing_left = facing_left;
        }
        // A completed static placement, as reduced motion's: the motion
        // sensor synced, or `needs_frames` reads the move as travel.
        self.col = col;
        self.row = row;
        self.home = col;
        self.col_at_tick = col;
        self.alpha = 0.0;
        self.console.hold = VisibilityHold::default();
        self.console.trip = None;
        self.console.target = None;
        true
    }

    /// THE RE-SIGHTING GHOST'S ADMISSION ON ITS OWN GLASS, judged at the
    /// boundary ([`PetBrain::resight_coordinate_space`]) — before the retire
    /// drops the old surface's map, so the map is the observation the body
    /// was drawn against on the frame the user saw. The ghost is the body
    /// standing still at its seat in the stance that frame drew it at
    /// (`PetBrain::last_stance`: a ghost is drawn at it), at the last tick's
    /// cell metrics and grid; it is admitted only where that map certifies
    /// nothing protected under it — no selection, image, overlay or unseen
    /// cell — exactly the live body's own verdict
    /// (`under_text_clearance_past_caret`). An incoherent map, or none,
    /// admits nothing.
    pub(super) fn console_still_body_clear(&self) -> bool {
        self.console_still_rect_verdict(self.col, self.row, self.last_stance) == Some(true)
    }

    /// [`Self::console_still_body_clear`]'s verdict for a still body at
    /// `(col, row)` in `stance` (a ghost still on glass at the boundary,
    /// `Departure::own_clear`), over what the map sees of it
    /// (`PetWorld::seen_under_text_clearance_past_caret`): `None` where the
    /// covered part is clear and the rest unseen, `Some(false)` over a
    /// protected cell or with no certified map.
    pub(super) fn console_still_rect_verdict(
        &self,
        col: f32,
        row: f32,
        stance: Stance,
    ) -> Option<bool> {
        let Some(world) = self.console.world.as_ref().filter(|w| w.stamp().is_some()) else {
            return Some(false);
        };
        let rect = still_body_rect(
            (col, row),
            stance,
            (self.last_cell_w, self.last_cell_h),
            self.last_grid,
        );
        world.seen_under_text_clearance_past_caret(rect)
    }

    /// WHERE A SLEEPER MAY STRETCH (2026-10-20): the body where the cat lies
    /// covers no ink and nothing protected the map sees
    /// ([`PetWorld::ink_clearance`], which counts as ink every cell anything
    /// protects: the host's chrome, an image, a selection, an overlay, a row
    /// it cannot see), so the stretch is drawn in front of blank ground and
    /// never under a word. The stretch's
    /// own widening ([`STRETCH_AMP`], 3% a side, a sixth of a cell at most)
    /// is not judged: it reaches no glyph's ink, and judging it would take
    /// the stretch from a key's wake beside the letter it just echoed. The
    /// owner's captures of a landing in front showed the stretch's ears
    /// through the foot of the done line's first word: the console law had
    /// stepped the sleeper off the reserved rule onto ground behind the text
    /// (`place_console_body_at_home`), where ordinary ink permits residency,
    /// and it stretched there. Where this is `false` only the stretch is
    /// withheld: the landing's cheer goes straight to its stage, the
    /// console's success keeps the cat's own pose (no other sign of it),
    /// every other wake (a key, a trick, an eviction's handed-back sleeper)
    /// is drawn sitting up (the look's `Waking` arm), and a sleeper's fall
    /// stays asleep through the beat it would stretch in the air (the look's
    /// `Leap` arm). The cat is still drawn where it lies, behind the words,
    /// as ordinary ink's residency has always let it: this moves no cat off
    /// a word. `true` with no observed world (the pane is all the cat's);
    /// `false` on a map that cannot see that ground, or none certified. At
    /// the last tick's cell metrics and grid.
    pub(super) fn console_stretch_on_blank(&self) -> bool {
        self.console_stretch_on_blank_at((self.col, self.row))
    }

    /// [`Self::console_stretch_on_blank`] for a body at `at` (cells): the
    /// sleeper's fall judges where it falls from and where it lands.
    pub(super) fn console_stretch_on_blank_at(&self, at: (f32, f32)) -> bool {
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
        world.ink_clearance(rect, 0.0) == Some(true)
    }

    /// An examined obstruction under the pet's natural body. `clear` would
    /// also answer `false` for "outside the coverage window", and this
    /// predicate authorizes a teleport, so it acts only on a certified
    /// `Some(false)`.
    ///
    /// The caret is not an obstruction to its own escort: [`STATION_LEAD`] seats the escort
    /// one cell past the caret, inside its 3x3 keep-off ring. Glyphs are `ink_overlaps`'s to
    /// judge; selections, images and uncertified geometry are this one's.
    pub(super) fn console_obstructed(&self, sense: PetSense) -> bool {
        self.console.presentable
            && self.console.world.as_ref().is_some_and(|world| {
                Self::stand_clearance(world, self.console_body(sense)) == Some(false)
            })
    }

    /// The brain's own stream watch has the cat: a stare under way, or the
    /// vigil it armed (see `tick_console_resident`).
    fn watching_stream(&self) -> bool {
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
                // A held verdict is a fact about a place and this body just
                // moved to a safe one, so retire the evidence: recovery is one
                // arrival fade, not this envelope's dwell and ramp on top.
                self.console.hold = VisibilityHold::default();
            }
        }
    }

    /// Read once per brain tick, before choosing between caret work and a
    /// located reading/output interest. No host classifies pet behavior.
    pub(super) fn begin_console_tick(&mut self, sense: PetSense, width: f32) {
        let contact_changed = core::mem::take(&mut self.console.contact_changed);
        // Taken, not read: only the spent-perch arm below re-establishes it,
        // so every other verdict restarts the dwell.
        let perched_since = self.console.perched_since.take();
        // A body still gliding to its station has not settled, so the dwell
        // measures settled time and a long trip cannot eat it.
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
        // Only a painted caret is a home: an unpainted one is a program's
        // repaint, and its cell may be the program's write position.
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
            // A commanded seat stands down on its quiet clock, and a "heard you" beat the
            // hand typed over owes its verb no longer. The trick latch is kept: the word's
            // own Enter is a fresh input, and the verb is owed to the prompt that follows.
            self.clear_trick_settle();
            self.drop_trick_go();
            self.song_left = 0;
            // Intent is enough for attention even if the caret stays still.
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
        // Surface protection outranks everything. A new key still cancels the
        // old trip, but cannot license a performance over a held selection.
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

        // A small repair holds its local subject until the caret overlaps the body
        // or the stand moves off its row or past it. Long deletion releases it to
        // the retreat gait; arrow movement cannot create a repair.
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
                && Self::console_may_stand(world, body, CLEARANCE)
                && self.flight.is_none()
            {
                self.console.target = Some(body);
                self.console.resident = true;
                self.console.still = true;
            }
            // The clearance margin is real contact geometry: never brace on
            // input alone or a duplicate frame of the same ink. Read past the
            // caret's blank ring, or a cat at the caret's shoulder is never
            // `roomy` and `contact_armed` never re-arms.
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

        // Eligible direct affection outranks program watching, and a typed kitty command is
        // the same kind of address while it holds the cat's attention or body
        // ([`PetBrain::trick_engaged`]). Each term is finite or interruptible and none is in
        // `console_frames`, so a resident that owns the tick still parks.
        //
        // So is a landing's cheer (`PetBrain::note_room_landed`, §11.3 of
        // `docs/DESIGN-harness-autopilot-2026-10-15.md`): the agent's work came
        // down and the person is looking, in a pane whose agent is one running
        // command. It holds the cat until it has played, its ♪/♥ gone; a fresh
        // key and a held selection (above) wipe it first.
        //
        // Spent, it hands the cat back. Where the escort's stand is refused
        // (the host's chrome over it, no perch within reach: the stand
        // `console_caret_home` gives, already corrected where a perch is), the
        // escort would chase a station the cat can never stand on, and the
        // console law steps every flight there aside onto ground it refuses
        // again on landing: a hop in place, frames without end. So the cat
        // leaves the glass where it stands instead and goes home off glass,
        // as a re-sighting finds it (`PetBrain::room_cheer_leaving`).
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

        // Program state is read only from explicit shell/progress metadata.
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
                self.console.failure_quiet_until =
                    Some(sense.now + Duration::from_secs_f32(RESULT_HOLD));
            }
        }
        // The located response replaces the cheer/sulk latches: no cheer lands
        // after the user has already resumed another activity.
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
                self.console.progress = next_progress; // silent task baseline
                self.console.progress_at = None;
            } else if next_progress != self.console.progress {
                self.console.progress = next_progress;
                // Paused and indeterminate are quiet levels, not clocks.
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
            // The success's stretch only on blank ground
            // ([`Self::console_stretch_on_blank`]): over a word it would be
            // drawn under the glyphs, ears through them, so there the cat
            // keeps its own pose through the hold, still behind the words,
            // and the success shows no other sign.
            self.console.pose = if failed {
                Some(PetGlyphId::PetInspectDown)
            } else {
                self.console_stretch_on_blank()
                    .then_some(PetGlyphId::PetStretchHind)
            };
            self.console.target = Self::console_may_stand(world, body, CLEARANCE).then_some(body);
            self.console.resident = true;
            self.console.still = true;
        } else if let Some(anchor) = self.console.anchor.and_then(|a| {
            world.resolve(a).or_else(|| {
                // A coalesced output scroll can remove the old row while later
                // rows of the same observed block remain on screen.
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
                    // The content carries the body only while it stays on
                    // glass: a scroll that would carry it off the top (where
                    // `emit` would clamp it to row 0) leaves it in its visible
                    // seat, gives up the perch, and lets the escort re-station it.
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
        // Output directs attention; the caret remains home, and home is the escort's stand
        // ([`Self::console_seat`]). A certified resident is kept even on ordinary ink,
        // avoiding a fresh search per identical frame.
        let next = self.console_seat(world, body, home);
        self.console.anchor = Some(subject);
        self.console.target = next;
        self.console.trip = None;
    }

    /// Whether ordinary locomotion still owes a flight or landing step.
    /// Pure projection for scheduler introspection and model conformance.
    #[must_use]
    pub fn console_legacy_motion_pending(&self) -> bool {
        self.flight.is_some()
            || self.land_t > 0.0
            || self.bound2.is_some()
            || self.retired_flight_lift.is_some()
            || self.deferred_hidden_landing.is_some()
    }

    /// Whether a deferred console owner still owes its first resident tick.
    /// Reading this latch never consumes it or advances the motion clock.
    #[must_use]
    pub fn console_resident_handoff_pending(&self) -> bool {
        self.console.resident_handoff
    }

    /// The repaint hold: for up to [`HIDDEN_HOLD_MAX`] an unpainted caret (a DECTCEM hide
    /// means "I am repainting") reads as the last caret this surface painted
    /// (`ConsoleLife::cursor_home`, while the observation vouches for it), so the brain sees
    /// a painted caret that has not moved: nothing chases, a flight lands where it was going,
    /// a sleeper sleeps. Past the bound the unpainted caret is followed as a re-sighting,
    /// the chase taking it quietly from the pet's own feet.
    ///
    /// It holds the caret, not the body: pinning the body would let a same-tick escort
    /// resign hand the hidden caret (the program's write position) to the ordinary ladder.
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

    /// New interests use the resident's position, distance-driven gait and
    /// authored poses. This is one branch of PetBrain::tick, not a host driver.
    pub(super) fn tick_console_resident(
        &mut self,
        sense: PetSense,
        width: f32,
        dt: f32,
    ) -> Option<PetFrame> {
        // Two carets the pet cannot take at its word:
        //  * caret absent (the viewport is scrolled into history): hold the cat where it
        //    stands, unbounded, without inventing a caret for the move sensor. It is
        //    re-seated at `last_seat`: this tick's scroll compensation may have moved it,
        //    and output may not carry the resident away from input. A flight in the air is
        //    left to the no-caret arm, which eases its lift down.
        //  * caret present but unpainted past [`HIDDEN_HOLD_MAX`]: a build or a pager.
        // Admission follows `begin_console_tick`'s selection, input and OSC command
        // handling, so a hold neither erases attention nor loses a completion.
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
                // A parked resident loafs; a sleeping one sleeps on.
                if self.action != PetAction::Sleep {
                    self.console.pose = Some(PetGlyphId::PetLoaf);
                }
            }
            self.console.still = true;
            // The caret that comes back is a new sighting, as for the
            // no-caret arm this one stands in for.
            self.last_caret = None;
        } else if !sense.caret_drawn && sense.caret.is_some() && self.console.presentable {
            // The hold outlasted its repaint and the caret is in plain sight of
            // the brain though not drawn. Residency would survive on its own at
            // a stale seat, so hand the body back to the caret's escort; a frame
            // that paints the caret again re-establishes residency.
            self.console.resident = false;
            self.console.target = None;
            self.console.trip = None;
            self.console.still = false;
        }
        if !self.console.resident || !self.console.presentable {
            self.console.resident_handoff = false;
            return None;
        }
        // An already committed normal flight lands through its existing door.
        if self.console_legacy_motion_pending() {
            self.console.resident_handoff = true;
            return None;
        }
        // THE STREAM WATCH OUTRANKS THE OUTPUT HOLD. Both are the cat
        // watching a running command's output (one rung of the attention
        // precedence): the brain's stare, its hug and its vigil, and this
        // hold's gaze. A stare under way (heat over the gate, its cap not
        // spent) or a vigil armed by one keeps the cat. The hold zeroes the
        // heat, and it took the cat the first frame its seat could be held,
        // so whether a streamed answer was watched at all turned on whether
        // the seat cleared the console law by a tenth of a cell.
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
        // A gentle hand stroked over a settled watching resident earns the same
        // affection as on an idle pet, while protected reading, repair holds,
        // hidden-caret holds and transit reset any partial stroke.
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
        // A resident branch has already yielded affection to protected reading or another
        // interest; retire its suppressed latch even with no safe footprint, so it cannot
        // keep a hidden pet polling or replay when the selection is released.
        self.pet_hold_t = 0.0;
        self.pending_pet = 0;
        self.pet_at = None;
        // The kitty command goes the same way, so a held selection or hidden
        // caret cannot replay a stale verb later. The exception is the latch
        // only: while a repair is live, fixing a typo is still typing the word,
        // and the repair's hold ([`EDIT_HOLD`]) is well inside the latch's TTL.
        // Keyed on the repair's own clock, since advancing ink labels a repair
        // `Contact` at first.
        let repairing = self.console.repair_until.is_some_and(|end| sense.now < end);
        let latched = self.pending_trick.filter(|_| repairing);
        self.drop_tricks();
        self.pending_trick = latched;
        let body = self.console_body(sense);
        let world = self.console.world.as_ref()?;
        let home = self.console_caret_home(sense, width);
        // The escort delivers, the resident holds. Beside a live caret the seat is decided
        // from the escort's stand every frame: a body not yet there is the cursor follower's
        // to move, one off glass or under reduced motion is placed directly. A result claims
        // the body where it is if it may stand within [`HOME_REACH`] of the stand; a repair
        // does too unless the caret overlaps the body or the body sits off the stand's row
        // or on the caret's side of it. With the caret hidden (or no home) the admitted body
        // is held where it is.
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
                    && Self::console_may_stand(world, body, CLEARANCE) =>
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
                    if Self::console_may_stand(world, body, CLEARANCE) {
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
        // A certificate for the current displayed body means stay here;
        // converting the pixel rect back to fractional feet would move a
        // settled resident.
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
            (self.alpha + dt / FADE_IN).min(1.0)
        };
        self.last_caret = sense.caret; // the real caret only, including None
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
        // The departure lane is not emptied here: a look-swap crossfade has
        // one rule for both halves, applied in `finish_console_frame`.
        self.land_t = 0.0;
        self.retired_flight_lift = None;
        self.deferred_hidden_landing = None;
        self.console.moving = false;

        let body = self.console_body(sense);
        let world = self.console.world.as_ref().expect("observed");
        if sense.reduced_motion {
            // Static placement is the existing reduced-motion contract.
            self.col = destination.0;
            self.row = destination.1;
            self.console.trip = None;
        } else if (self.col - destination.0)
            .abs()
            .max((self.row - destination.1).abs())
            > 0.02
        {
            if !world.under_text_corridor_clear_past_caret(body, target, CLEARANCE) {
                // Watching from here is enough; a refused trip is never retried
                // by an idle timer. New displayed facts may choose another.
                self.console.trip = None;
                if Self::console_may_stand(world, body, CLEARANCE) {
                    self.console.target = Some(body);
                } else {
                    // The caret or selection just claimed the old cells. Keep
                    // presence intact: the final footprint resolver displaces
                    // the body to nearby protected-free ground.
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
                        duration: (dist / 22.0).max(0.22),
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
                let step = if amble {
                    LAT_STRIDE_CELLS
                } else {
                    STRIDE_CELLS
                };
                self.col = x;
                self.row = y;
                self.stride = (self.stride + travelled / step).rem_euclid(1024.0);
                self.console.moving = u < 1.0;
                // A row transfer tucks its paws. It uses a shallow arc only
                // when the entire swept footprint above the path is clear.
                let arc = PetRect::new(
                    body.row.min(target.row) - 0.25,
                    body.col.min(target.col),
                    body.rows + (body.row - target.row).abs() + 0.25,
                    body.cols + (body.col - target.col).abs(),
                );
                self.console.lift = if trip.hop && Self::console_may_stand(world, arc, CLEARANCE) {
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
                            let k = 1.0 - (-dt / DASH_WINDOW).exp();
                            self.pointer_vx += ((px - lx) / dt - self.pointer_vx) * k;
                            self.pointer_vy += ((py - ly) / dt - self.pointer_vy) * k;
                            let inst = ((px - lx) / dt).hypot((py - ly) / dt);
                            self.pointer_speed += (inst - self.pointer_speed) * k;
                        }
                        self.last_pointer = Some((px, py));
                        self.pointer_heat =
                            if self.pointer_speed >= GAZE_FOLLOW_MIN / GAZE_FOLLOW_WINDOW {
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
                // First placement and the last transit frame use the gaze
                // from the final station. No idle redraw is needed to settle.
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

    /// Whether the pet may be where it now is. Every console arm asking this
    /// of the current body asks through here, and the caret's own keep-off
    /// ring never counts: [`STATION_LEAD`] seats the escort inside it.
    ///
    /// Choosing a new station is the opposite question and keeps the strict
    /// reading (`home_perch`, `nearest_under_text_perch`, the rounding
    /// re-check in [`Self::place_console_body_at_home`]), so the pet is never
    /// parked on the cursor.
    fn console_may_stand(world: &PetWorld, rect: PetRect, margin: f32) -> bool {
        world.under_text_clearance_past_caret(rect, margin) == Some(true)
    }

    /// A stand's body as its clearance is judged ([`Self::stand_clearance`]):
    /// [`CLEARANCE`] short at the feet, so the margin round it stops at the
    /// ground it stands on.
    fn on_its_feet(body: PetRect) -> PetRect {
        PetRect::new(
            body.row,
            body.col,
            (body.rows - CLEARANCE).max(f32::EPSILON),
            body.cols,
        )
    }

    /// Whether a stand at `body` is clear behind text past the caret
    /// ([`PetWorld::under_text_clearance_past_caret`]), with [`CLEARANCE`] to
    /// spare above and to the sides and none under the feet. A cat stands on
    /// the row below it; its paws on that row's top edge cover nothing there.
    /// Judged with the margin under the feet too, the stand on Claude Code's
    /// composer row was refused wherever the host's chips sat on the rule
    /// under it (all but the first fifth of the line with the footer on), and
    /// the cat waited on the blank row over the composer, sixty cells behind
    /// the caret, until a kept seat ran out of reach and it leapt the line
    /// (2026-10-20, real Claude Code 2.1.305, windowed).
    fn stand_clearance(world: &PetWorld, body: PetRect) -> Option<bool> {
        let (feet, margin) = Self::stand_rect(world, body);
        world.under_text_clearance_past_caret(feet, margin)
    }

    /// What a stand at `body` is judged by: [`Self::on_its_feet`], and
    /// [`CLEARANCE`] round it, less against the pane's edge
    /// ([`PetWorld::margin_in_map`]). Grown past the right edge, the reading
    /// answered unknown, which a stand takes as clear: at the right margin
    /// the escort's stand under the composer's reserved upper rule was
    /// accepted, the cat flew onto the rule, and the console law, finding
    /// nowhere to step it, hid it there until the wrap (2026-10-20, real
    /// Claude Code 2.1.305 with the harness on, windowed).
    fn stand_rect(world: &PetWorld, body: PetRect) -> (PetRect, f32) {
        let feet = Self::on_its_feet(body);
        (feet, world.margin_in_map(feet, CLEARANCE))
    }

    /// Whether this body must move: the same reading asked the other way.
    ///
    /// [`Self::console_may_stand`] is fail-closed, right for choosing a
    /// station. For a body that already exists, `None` usually means it left
    /// the caret-centred coverage window, which slides as the user types, so
    /// only a positive observation of a protected surface moves it.
    fn console_must_yield(world: &PetWorld, rect: PetRect) -> bool {
        world.under_text_clearance_past_caret(rect, 0.0) == Some(false)
    }

    /// The escort's own station, corrected only where the ladder is blind.
    ///
    /// The ink ladder knows glyphs and nothing else, so it can seat the escort
    /// on a selection, an image, or a row with no certified cell-to-pixel
    /// projection. This answers `Some` for exactly those and `None` otherwise,
    /// leaving the caret its escort.
    ///
    /// Not reasons to move the escort:
    ///
    ///  * the caret's own keep-off ring ([`STATION_LEAD`] seats the pet inside
    ///    it);
    ///  * ordinary ink (standing behind text is a z-order fact,
    ///    `frame.under_ink`);
    ///  * an unknown (`None` is a bound on this map, not a claim about the
    ///    terminal).
    ///
    /// Which correction, by what refuses the stand:
    ///
    ///  * ONLY A SELECTION OR A TRANSIENT OVERLAY (the Autopilot menu, the
    ///    find bar; the person's typed glyphs, the predicted echo and an IME
    ///    preedit, are ink, never either; on the ground plane the stand is not
    ///    refused, [`PetWorld::ground_under_text_clearance_past_caret`]): the
    ///    closest place it may stand behind text
    ///    ([`PetWorld::nearest_under_text_perch`], within [`HOME_REACH`]), or
    ///    none, and the pet yields where it is. A hand's selection never
    ///    sends the cat across the pane.
    ///  * WHAT PROTECTS THE GROUND FOR GOOD (the host's chrome round an
    ///    agent's composer, an image), under the stand or BETWEEN THE BODY ON
    ///    GLASS AND IT ([`Self::console_walled`]: a stand it cannot walk to is
    ///    no stand; a draft wrapped onto the composer's second line put the
    ///    stand inside the box, clear, and the cat resting above the rule
    ///    hopped at it, stepped back off the rule each time, for as long as
    ///    the keys came): REST GROUND. The nearest clean seat —
    ///    nothing under the body, not even a gap inside a line
    ///    ([`PetWorld::nearest_clear_perch`], within [`REST_REACH`], solved
    ///    without the selection and overlays, so they never move it) — that
    ///    the body on glass can walk to without crossing that chrome
    ///    ([`Self::rest_reach_from`]); then the closest place behind text
    ///    within [`HOME_REACH`] it can walk to; then none. A seat beyond the
    ///    rules is never chased: the console law stepped every flight there
    ///    back off the rule, and the cat leapt in place for as long as the
    ///    window stayed up. A seat chosen so is KEPT while it is still clean
    ///    ground the body can reach, within `REST_REACH` of the stand
    ///    (`ConsoleLife::rest_seat`): the cat moves when its ground is taken,
    ///    not whenever nearer ground opens. A command list that Claude Code
    ///    redraws under the composer at every key sent it leaping up and
    ///    down the pane after the list's end.
    ///
    /// Both the escort and the resident read this one answer, so they agree.
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
        // Walled is a walk's: a stand on another row is a flight away, and
        // a flight passes over the host's chrome.
        let walled = from.is_some_and(|from| {
            PetWorld::same_ground(from, body) && Self::console_walled(world, from, body)
        });
        if !walled && Self::stand_clearance(world, body) != Some(false) {
            self.console.rest_seat.set(None);
            return None;
        }
        // No perch in reach is `None`, not "stay where you are": both callers
        // fall back to the baseline station, so the pet keeps following the
        // caret.
        if !walled && {
            let (feet, margin) = Self::stand_rect(world, body);
            world.ground_under_text_clearance_past_caret(feet, margin) != Some(false)
        } {
            let next = world.nearest_under_text_perch(body, CLEARANCE, HOME_REACH)?;
            return Some((next.col, next.foot_row()));
        }
        let resting = (self.flight.is_none() && self.alpha > 0.0).then_some((self.col, self.row));
        // A cat on the stand's own line, escorting the caret along it, waits
        // on it: at the last of the line's ground short of the refused stand,
        // judged as a stand is and walked to, while the stand is within
        // [`REST_REACH`]. The host's label on the composer's upper rule
        // refuses the stands under it at the line's end; sent to a seat off
        // the line, the cat hopped up off the line there and down again at
        // the wrap, two flights where the wrap's one does (2026-10-20, real
        // Claude Code 2.1.305, windowed).
        let on_the_line = |row: f32| (row - desired.1).abs() < 0.5;
        let held_seat = self.console.rest_seat.get();
        if let Some((col, _)) = resting.filter(|&(_, row)| {
            on_the_line(row) && held_seat.is_none_or(|(_, seat_row)| on_the_line(seat_row))
        }) {
            let at = |c: f32| Self::console_stand_rect((c, desired.1), sense_dims.0, width);
            let may = |c: f32| {
                let rect = at(c);
                Self::stand_clearance(world, rect) == Some(true)
                    && from.is_none_or(|from| !Self::console_walled(world, from, rect))
            };
            if (desired.0 - col).abs() <= REST_REACH && may(col) {
                const STEP: f32 = 0.25;
                let toward = STEP * (col - desired.0).signum();
                let steps = ((desired.0 - col).abs() / STEP).floor() as usize;
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
        // The seat kept: the search's own acceptance, at the seat alone. A
        // seat gone, the ground the cat rests on is kept in its place while
        // it is a seat: the seat can move on while the cat stays (a frolic,
        // a startle), and a caret run back toward it is no reason to leave.
        let pane_reach = || {
            let across = body
                .col
                .abs()
                .max((f32::from(sense_dims.1) - body.col).abs());
            (2.0 * f32::from(sense_dims.0)).hypot(across)
        };
        let keep = |seat: (f32, f32)| {
            let held = Self::console_stand_rect(seat, sense_dims.0, width);
            ((2.0 * (held.row - body.row)).hypot(held.col - body.col) <= REST_REACH)
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
            ((2.0 * (held.row - body.row)).hypot(held.col - body.col) <= REST_REACH)
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

    /// Where a rest seat must be reachable from ([`Self::console_station`]):
    /// the body on glass, at the seat a flight in the air is bound for. A
    /// body over ground the host's chrome protects is where the console law
    /// will step it to ([`Self::place_console_body_at_home`]'s search). A
    /// body not on glass (a first sighting, after a fade) has no path to
    /// keep: `None`, and the seat is the nearest one anywhere.
    fn rest_reach_from(&self, world: &PetWorld, rows: u16, width: f32) -> Option<PetRect> {
        if self.alpha <= 0.0 {
            return None;
        }
        let seat = self
            .flight
            .as_ref()
            .map_or((self.col, self.row), |f| (f.to_col, f.to_row));
        let body = Self::console_stand_rect(seat, rows, width);
        match world.ground_under_text_clearance_past_caret(body, 0.0) {
            Some(true) => Some(body),
            Some(false) => world.nearest_under_text_perch(body, CLEARANCE, HOME_REACH),
            None => None,
        }
    }

    /// Whether a flight's aim leads by the caret's drift: a rhythm is open
    /// and the station follows the caret. A console rest seat stays put
    /// while the caret runs ([`Self::console_station`]), so a flight to it
    /// lands on it.
    pub(super) fn aims_ahead(&self) -> bool {
        self.rhythm_open() && !self.console.rest_seated.get()
    }

    /// Whether the host's chrome stands between the body on glass, `from`,
    /// and `to`: a protected cell observed in the box a flight between them
    /// sweeps ([`PetWorld::ground_corridor_walled`]). An unknown cell is no
    /// wall: only a positive observation bars the way, as only one moves a
    /// body.
    fn console_walled(world: &PetWorld, from: PetRect, to: PetRect) -> bool {
        world.ground_corridor_walled(from, to)
    }

    /// Whether the console bars the escort from a stand at `desired`
    /// (`(col, foot row)`): protected ground under it, or between the body
    /// on glass and it ([`Self::console_walled`]). The live-edge hug asks
    /// this of its own stand ([`PetBrain::tick`]'s watch station) and stays
    /// where it is when barred: chased, the console law stepped every
    /// flight across the composer's rule back off it.
    pub(super) fn console_bars(&self, desired: (f32, f32), rows: u16, width: f32) -> bool {
        if !self.console.presentable {
            return false;
        }
        let Some(world) = self.console.world.as_ref().filter(|w| w.stamp().is_some()) else {
            return false;
        };
        let body = Self::console_stand_rect(desired, rows, width);
        Self::stand_clearance(world, body) == Some(false)
            || self
                .rest_reach_from(world, rows, width)
                .is_some_and(|from| Self::console_walled(world, from, body))
    }

    /// The body rect the escort's own station puts the pet in, unchanged. The
    /// resident's caret home ([`Self::console_caret_home`]) is built from it,
    /// so escort and resident agree on the seat.
    fn console_stand_rect(desired: (f32, f32), rows: u16, width: f32) -> PetRect {
        PetRect::new(
            (desired.1 + 1.0 - ART_ROWS).clamp(
                CLEARANCE,
                (f32::from(rows) - ART_ROWS - CLEARANCE).max(CLEARANCE),
            ),
            desired.0,
            ART_ROWS,
            width,
        )
    }

    /// The resident's home is the escort's stand, one distance from the caret
    /// (`tests/pet_resident_standoff.rs` pins that they agree): the lead-free
    /// [`PetBrain::station`] with a controller lease's far stand folded in, kept
    /// on the caret's left while the pet holds the wall side, run through the ink
    /// ladder against this caret, and corrected by [`Self::console_station`].
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

    /// What the resident does about a body beside a live caret whose home is
    /// `stand` ([`Self::console_caret_home`]). In order:
    ///
    ///  * [`CaretSeat::Hold`]: a body the escort has delivered stays: at rest,
    ///    within the chase's [`ARRIVED`] tolerance of the stand on its row, and
    ///    allowed to stand there. Correcting the chase's arrival residue would
    ///    slide a settled cat every time a command started.
    ///  * [`CaretSeat::Escort`]: anywhere else is the escort's to close. Only a
    ///    positive observation of a protected surface refuses the stand
    ///    ([`Self::console_must_yield`]). This arm does not walk the body: the
    ///    chase already does, and a second mover is what
    ///    `pet_position_authority` refuses.
    ///  * [`CaretSeat::Yield`]: a protected stand the escort's correction
    ///    could not fix; the caller asks [`PetWorld::nearest_under_text_perch`]
    ///    once more and yields on `None`.
    fn caret_seat(&self, world: &PetWorld, body: PetRect, stand: PetRect) -> CaretSeat {
        if self.speed.abs() <= 0.01
            && self.carry_speed() < FLIP_SPEED
            && self.at_escort_stand(body, stand)
            && Self::console_may_stand(world, body, CLEARANCE)
        {
            CaretSeat::Hold
        } else if !Self::console_must_yield(world, stand) {
            CaretSeat::Escort
        } else {
            CaretSeat::Yield
        }
    }

    /// Whether `body` is arrived at the stand the escort delivers to: either
    /// the lead-free `stand` ([`Self::at_console_stand`]), or up to the
    /// escort's held keep-ahead ([`Self::lead`]) ahead of `stand` when the
    /// escort parked it beside this caret (`held_for`).
    fn at_escort_stand(&self, body: PetRect, stand: PetRect) -> bool {
        if Self::at_console_stand(body, stand) {
            return true;
        }
        let lead = if !self.wall_side
            && !self.console.rest_seated.get()
            && self.console.cursor_home.is_some()
            && self.held_for == self.console.cursor_home
        {
            self.lead()
        } else {
            0.0
        };
        lead > 0.0
            && (body.row - stand.row).abs() < 0.25
            && body.col >= stand.col - ARRIVED
            && body.col <= stand.col + lead + ARRIVED
    }

    /// The seat as a target rect for the arms that only record one
    /// (`begin_console_tick`'s selection reading,
    /// [`Self::choose_console_perch`]): the same three answers as
    /// [`Self::caret_seat`], with the bounded local search where there is no
    /// caret home. Advisory: [`Self::tick_console_resident`] decides from
    /// `caret_seat` (scroll compensation and the step-aside shift read this).
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
            None if Self::console_may_stand(world, body, CLEARANCE) => Some(body),
            None => world.home_perch(body, CLEARANCE, HOME_REACH),
        }
    }

    /// Whether `body` is arrived at `stand` as the escort judges it: within
    /// [`ARRIVED`] in columns, and on the stand's row (a quarter row covers
    /// `body_px`'s rounding at cell heights of 4 px or more; a body between
    /// rows is mid-hop).
    fn at_console_stand(body: PetRect, stand: PetRect) -> bool {
        (body.col - stand.col).abs() <= ARRIVED && (body.row - stand.row).abs() < 0.25
    }

    fn near_console_home(rect: PetRect, home: Option<PetRect>) -> bool {
        home.is_none_or(|home| {
            (rect.col - home.col)
                .abs()
                .max((rect.row - home.row).abs() * 2.0)
                <= HOME_REACH
        })
    }

    /// Protected cells constrain every emitted body, including a pose hold or
    /// a flight: a body on a selection, an image or a row with no certified
    /// projection steps aside with one bounded local placement, keeping the
    /// full body and its alpha. Ordinary ink permits residency.
    ///
    /// The caret's home is not a constraint here (see below). When a step
    /// aside happens the motion coordinates are translated with it, or the
    /// next tick's lerp would restore the body to the protected spot.
    fn place_console_body_at_home(&mut self, frame: &mut PetFrame, sense: PetSense) {
        if self.console.clipped || frame.alpha == 0 {
            return;
        }
        // A body in the air is passing: over a rule, over the host's chips,
        // over the words, for the fraction of a second its arc takes, and
        // what it lands on is its stand or seat, chosen clear. The law is for
        // a body that stays. Stepped aside in the air, a bound across the
        // composer's rule was turned back off the rule at every frame and
        // the cat leapt in place; walled off from such stands, it rested out
        // of the caret's reach and took two flights at every wrap; and its
        // arcs, flattened to keep off the chips, glided across the pane with
        // no rise (2026-10-20).
        if frame.action == PetAction::Leap {
            return;
        }
        let Some(world) = self.console.world.as_ref() else {
            return;
        };
        let Some(rect) = body_rect(frame, sense) else {
            return;
        };
        // Not a second opinion about position: distance from the caret is the escort's
        // business, and pulling a far body home here would make two writers fight. What
        // remains is the one correction the escort cannot make: a body on a protected surface.
        //
        // Its place is its body at its own size, as its stand was judged: a coil's
        // squash and a landing's spread widen the drawn body by up to half a cell
        // for a tenth of a second, over the blank margin a stand keeps from the
        // host's chrome. Stepped aside for that, the cat waiting on the composer's
        // line short of the harness switch jumped six pixels sideways as it
        // gathered for the wrap's bound (2026-10-20, the generating replay with
        // the harness's switch reserved).
        if !Self::console_must_yield(world, rect) {
            return;
        }
        if (frame.scale_x != 1.0 || frame.scale_y != 1.0)
            && body_rect(
                &PetFrame {
                    scale_x: 1.0,
                    scale_y: 1.0,
                    ..*frame
                },
                sense,
            )
            .is_none_or(|natural| !Self::console_must_yield(world, natural))
        {
            return;
        }
        // The rows round the body moved down together and brought the
        // protected cells with them (the host's chips ride the rule of a
        // composer that grew a line): the body goes down with its ground, the
        // whole rows they went and no column sideways, in the frame its
        // ground moved in. The nearest perch instead slid it a column or more
        // sideways onto the rule, or a row down and two columns over.
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
        // It steps aside, not home: the shortest move off the protected
        // surface is all this layer owes.
        let Some(safe) = world.nearest_under_text_perch(rect, CLEARANCE, HOME_REACH) else {
            self.console.clipped = true;
            self.console.reason = "protected-home-unavailable";
            return;
        };
        // Invert body_px's projection rather than adding a delta to a possibly
        // viewport-clamped source: after a wrap the raw flight can be past the
        // right edge.
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

    /// Move the drawn body to `to` and every motion coordinate with it, or
    /// the next tick's lerp would restore the body to where it was.
    pub(super) fn carry_console_body(
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
        if let Some(over) = self.brake_over.as_mut() {
            *over += dx;
        }
        if let Some((tc, tr)) = self.play_to.as_mut()
            && (*tc - old_seat.0).abs() <= 1e-4
            && (*tr - old_seat.1).abs() <= 1e-4
        {
            *tc += dx;
            *tr += ground_dy;
        }
        if let Some(look) = self.adopted_look.as_mut() {
            look.seat.0 += dx;
            look.seat.1 += ground_dy;
        }
        // A still ghost is this body a frame ago (the fading half of an
        // in-place crossfade), so it steps aside with the body or the swap
        // frame splits one cat in two. A running ghost keeps its own motion,
        // and a re-sighting ghost is the replaced surface's body, not this one.
        for (record, drawn) in self.departures.iter_mut().zip(frame.departures.iter_mut()) {
            if let Some(record) = record.as_mut().filter(|d| d.still && !d.resight) {
                record.from_col += dx;
                record.edge += dx;
                record.row += ground_dy;
                if let Some(drawn) = drawn.as_mut() {
                    drawn.col += dx;
                    drawn.row += dy;
                }
            }
        }
        if let Some(flight) = self.flight.as_mut() {
            flight.from_col += dx;
            flight.to_col += dx;
            flight.from_row += ground_dy;
            flight.to_row += ground_dy;
        }
        if let Some((col, row, _)) = self.bound2.as_mut() {
            *col += dx;
            *row += ground_dy;
        }
        if let Some((col, row)) = self.deferred_hidden_landing.as_mut() {
            *col += dx;
            *row += ground_dy;
        }
        if let Some(target) = self.console.target.as_mut() {
            target.col += dx;
            target.row += ground_dy;
        }
        if let Some(trip) = self.console.trip.as_mut() {
            trip.from.0 += dx;
            trip.to.0 += dx;
            trip.from.1 += ground_dy;
            trip.to.1 += ground_dy;
        }
    }

    pub(super) fn finish_console_frame(&mut self, frame: &mut PetFrame, sense: PetSense) {
        if !self.console.presentable {
            return;
        }
        if let Some(pose) = self.console.pose {
            // The contact is a pose over the existing displacement; flights keep
            // their own silhouette unless this is the console's trip. A settled
            // cat's own animation (sit, loaf, groom, sleep) is left alone while
            // typing; contact ([`CONTACT_HOLD`]) still applies. A body the chase
            // is walking wears its gait: a brace or its recovery drawn over a
            // walk slid the still pose along the row.
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
            }
        }
        if self.console.resident {
            if !self.console_legacy_motion_pending() && self.action != PetAction::Land {
                frame.lift = self.console.lift;
            }
            frame.motes = [None; PET_MOTES_MAX];
            // Not `frame.departures`: the departing ghost is the other half of
            // a crossfade and is judged below with the body.
        }
        self.place_console_body_at_home(frame, sense);
        let Some(world) = self.console.world.as_ref() else {
            // A host that feeds no world cannot judge a re-sighting ghost's
            // new ground (`Departure::resight`), so it never draws one.
            for (slot, live) in frame.departures.iter_mut().zip(&self.departures) {
                if live.is_some_and(|d| d.resight) {
                    *slot = None;
                }
            }
            return;
        };
        if world.stamp().is_none() {
            // No certified map at all (a stale snapshot, a resize frame whose
            // cells and facts straddle two grids): nothing new is licensed, and
            // the tick froze the body where it stood.
            frame.motes = [None; PET_MOTES_MAX];
            self.console.attention = PetAttention::Yielding;
            self.console.reason = "incoherent-surface";
            if self.console.on_glass && frame.alpha > 0 {
                // A body already on glass is held, not cut: "I cannot see" is the
                // visibility hold's own unknown, so only an unbroken blind run of
                // [`VETO_BLIND`] fades it, and the z-order is held too. Cutting would blink
                // the resident on every incoherent read (a resize frame, a reused snapshot).
                let cover = self.console.hold.update(sense.now, None);
                frame.alpha = cover_alpha(frame.alpha, cover);
                frame.lane_alpha = cover_alpha(frame.lane_alpha, cover);
                frame.under_ink |= self.console.z.under;
                // A re-sighting ghost stands on glass this blind map would
                // have to judge (`Departure::resight`): it goes.
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
            // Nothing was on glass: the pet is drawn only over pixels an
            // observation certified, so a first appearance on an incoherent
            // surface draws nothing. This bypasses the visibility hold:
            // momentary incoherence is not evidence of ink, must not spend the
            // dwell, and must not clear `on_glass` (granting a reseat teleport).
            frame.alpha = 0;
            frame.lane_alpha = 0;
            frame.departures = [None; PET_DEPARTURES_MAX];
            self.console.clipped = true;
            self.console.hold.idle(sense.now);
            self.console.z = ZHold::default();
            return;
        }
        let body = body_rect(frame, sense);
        // The body is still where it is, whatever the veto decides; clearing it
        // would make the next reseat teleport the pet and restart its fade.
        // `on_glass`, set below, is the separate fact the reseat reads.
        if let Some(rect) = body {
            self.console.last_body = Some(rect);
            self.console.last_seat = Some((self.col, self.row));
        }
        // Selected text subordinates the pet at once, without the hold: a
        // selection is a user act, not perception wobble. The union of selected
        // cells is a conservative box (over-yielding is safe), and selections
        // change too slowly to strobe.
        let selected = body
            .zip(world.selection_rect())
            .is_some_and(|(b, s)| b.overlaps(s));
        if self.console.clipped || selected {
            // Placement searched the whole reach this tick and found nowhere to
            // stand (a dense screen): a searched result, so it yields at once,
            // moving the envelope to where the glass already is.
            frame.alpha = 0;
            frame.lane_alpha = 0;
            self.console.hold.hide_now(sense.now);
            self.console.z = ZHold::default();
            self.console.clipped = true;
            if !self.console.reason.starts_with("protected-") {
                self.console.reason = "protected-footprint";
            }
        } else {
            // The visibility verdict is three-valued: `None` ("cannot see there", e.g.
            // outside the caret-centred coverage window) keeps the hold's previous verdict.
            // Under text: ink in front of the body is a z-order fact, not a reason to hide.
            // Past caret: a pet walking past the caret (the escort lives in that ring) must
            // not vanish.
            let verdict = body.map(|rect| world.under_text_clearance_past_caret(rect, 0.0));
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
                // The footprint is refused only once the yield has completed:
                // while the ramp runs the pet is on glass and owes frames, so
                // `clipped` waits for the cover to reach zero.
                self.console.clipped |= frame.alpha == 0;
            }
            // Ordinary ink in front of the body changes the held z-order (`ZHold`) and
            // nothing else. Only real ink is asked about, never the caret's keep-off ring;
            // an unobserved rectangle answers inked and draws under the text, the safe
            // direction for layering.
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
        // `clipped` is set only by the two refusing branches above, not here:
        // a transparent frame may just be fading in, and latching a refusal
        // then would blank the next, clear frame.
        self.console.on_glass = frame.alpha > 0;
        // The whole emitted sprite, including motes and departing bodies,
        // is subordinate to selected text and the current occupancy map.
        let min_ch = f32::from(sense.cell_h.max(6));
        let cw = f32::from(sense.cell_w.max(1));
        let ch = f32::from(sense.cell_h.max(1));
        for mote in &mut frame.motes {
            if mote.is_some_and(|m| {
                // Conservative around the center, including minimum tile
                // size, scaling and pixel rounding on every side.
                let radius = min_ch * m.scale + 1.0;
                let rw = radius / cw;
                let rh = radius / ch;
                !world.clear(
                    PetRect::new(m.row - rh, m.col - rw, 2.0 * rh, 2.0 * rw),
                    CLEARANCE,
                )
            }) {
                *mote = None;
            }
        }
        // One crossfade, one visibility rule: the emitter draws a departing ghost at
        // `d.alpha × lane_alpha / 255`, and `lane_alpha` carries this frame's cover, so body
        // and ghost ramp together. A ghost gets the body's rule: selected text subordinates
        // it at once, nothing else culls it, and one with no on-grid rectangle is dropped.
        //
        // Exceptions: a re-sighting ghost (`Departure::resight`, the old surface's body
        // fading on glass a new screen owns) is kept only where this map sees nothing
        // protected, beneath the new screen's ink; where the map cannot see, it keeps its
        // boundary verdict (`Departure::own_clear`), since unknown is not evidence. Refused,
        // its presence goes to its arriver. A ghost on its own glass (`Departure::own_glass`,
        // admitted by `PetBrain::displace_resight`) stays beneath that pane's ink.
        if frame.departures.iter().any(Option::is_some) {
            let base = *frame;
            let selection = world.selection_rect();
            // The presence of the crossfade ghosts refused this frame, as drawn,
            // and their envelopes, which the arriver's ramp takes over below.
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
                // Copying the frame before setting the ghost fields also avoids
                // a stock-rustc MIR ICE for PetFrame's const-sized arrays on
                // foreign targets (Linux ARM and Windows).
                let mut ghost = base;
                ghost.alpha = d.alpha;
                ghost.col = d.col;
                ghost.row = d.row;
                // Judged where it is drawn: at its own stance
                // (`Departure::stance0`), not the body's.
                ghost.lift = d.lift;
                ghost.scale_x = d.scale_x;
                ghost.scale_y = d.scale_y;
                let keep = match body_rect(&ghost, sense) {
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
                        refused += u16::from(d.alpha) * u16::from(base.lane_alpha) / 255;
                        handed += l.envelope((self.lane_clock - l.born) as f32);
                    }
                    *departure = None;
                }
            }
            // A refused crossfade ghost hands its presence to its arriver (one cat changing
            // places on one ramp): the arriver shows what the ghost would have, never more
            // than the lane's presence.
            if refused > 0 {
                frame.alpha =
                    (u16::from(frame.alpha) + refused).min(u16::from(frame.lane_alpha)) as u8;
            }
            // The hand-over is for good: the refused ghost left the lane above, and lifting
            // `arrive_from` by `envelope / remaining` adds exactly the ghost's envelope to every
            // later frame's arrival; judging the refusal afresh each frame would count it twice.
            // With no ramp left nothing is owed.
            let remaining = self.arrive_t / self.arrive_in;
            if handed > 0.0 && remaining > 0.0 {
                self.arrive_from = (self.arrive_from + handed / remaining).min(1.0);
            }
        }
    }

    pub(super) fn console_hold_settled(&self) -> bool {
        self.console.hold.settled()
    }

    pub(super) fn console_frames(&self) -> Option<bool> {
        if !self.console.presentable || self.console_legacy_motion_pending() {
            return None;
        }
        // A visibility hold in transit is a fade on glass and a counting dwell:
        // it owes frames, or the ramp stalls until an unrelated repaint.
        if !self.console.hold.settled() {
            return Some(true);
        }
        // So is a look swap in transit (the incoming arrival ramp and the
        // departing ghost's fade). This layer answers the frame-train question
        // first for a resident, so it must count them here or the swap stalls.
        if self.arrive_t > 0.0 || self.departures.iter().any(Option::is_some) {
            return Some(true);
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
        let input = self
            .console
            .input
            .map(|e| e.at + Duration::from_secs_f32(INPUT_HOLD));
        let contact_recover = self
            .console
            .contact_at
            .map(|at| at + Duration::from_millis(140));
        let contact = self
            .console
            .contact_at
            .map(|at| at + Duration::from_secs_f32(CONTACT_HOLD));
        let result = self
            .console
            .result_at
            .map(|(at, _)| at + Duration::from_secs_f32(RESULT_HOLD));
        let progress = self
            .console
            .progress_at
            .map(|at| at + Duration::from_millis(350));
        // The spent perch's own edge; without it the dwell would be a timer
        // nothing ticks. One dwell per perch.
        let perch = self
            .console
            .perched_since
            .map(|at| at + Duration::from_secs_f32(PERCH_DWELL));
        let floor = self.last_now.unwrap_or(now);
        [
            input,
            contact_recover,
            contact,
            result,
            progress,
            perch,
            // A pet held under the glyphs surfaces when its clear run lands,
            // even if nothing else on screen asks for a frame by then.
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

    pub(super) fn note_console_completion(&mut self, now: Instant, failed: bool) {
        self.console.completion_seq = self.console.completion_seq.wrapping_add(1).max(1);
        self.console.completion = Some((now, failed));
        // Even when direct work or reading suppresses a visual result, a
        // failure must not earn the caret's celebratory star ring. This is
        // a finite hush, independent of the selected body pose.
        self.console.failure_quiet_until =
            failed.then(|| now + Duration::from_secs_f32(RESULT_HOLD));
    }

    pub(super) fn console_grieving(&self) -> bool {
        self.console
            .failure_quiet_until
            .zip(self.last_now)
            .is_some_and(|(until, now)| now < until)
    }
}

/// A body standing still at `(col, row)` in `stance` (no lift, no squash), in fractional
/// grid cells: the `PetFrame::body_px` projection itself (`drawn_body_px`), origin, feet and
/// grid clamp included, since a fractional landing can straddle a protected cell that the
/// rounded body clears.
fn still_body_rect(
    seat: (f32, f32),
    stance: Stance,
    cell: (u16, u16),
    grid: (u16, u16),
) -> PetRect {
    let (cw, ch) = (cell.0.max(1), cell.1.max(1));
    let (x0, x1, y0, y1) = drawn_body_px(seat, stance, (cw, ch), grid);
    let (cw, ch) = (f32::from(cw), f32::from(ch));
    PetRect::new(
        y0 as f32 / ch,
        x0 as f32 / cw,
        (y1 - y0) as f32 / ch,
        (x1 - x0) as f32 / cw,
    )
}

/// The frame's body in cell units: [`PetFrame::body_px`]'s pixel box divided
/// back through the cell size.
fn body_rect(frame: &PetFrame, sense: PetSense) -> Option<PetRect> {
    frame
        .body_px(sense.cell_w, sense.cell_h, sense.cols, sense.rows)
        .map(|(x0, x1, y0, y1)| {
            let cw = f32::from(sense.cell_w.max(1));
            let ch = f32::from(sense.cell_h.max(1));
            PetRect::new(
                y0 as f32 / ch,
                x0 as f32 / cw,
                (y1 - y0) as f32 / ch,
                (x1 - x0) as f32 / cw,
            )
        })
}

/// The caret's own writing this frame, pane-local `(row, first, end)`: the
/// cells between the last ticked caret and the visible one on its row (the cell
/// just behind the caret when it changed rows or went unseen), at most
/// [`TYPED_RUN_MAX`]. A glyph typed there is the escort's next step, not ink
/// arriving at the cat (as contact it would brace the escort on every burst's
/// first key); ink anywhere else still touches.
fn typed_run(
    input: &aterm_core::render::RenderInput,
    pane: PetPane,
    last: Option<(u16, u16)>,
) -> Option<(usize, usize, usize)> {
    if input.display_offset != 0 {
        return None;
    }
    let row = input.cursor_row.checked_sub(pane.row)?;
    let col = input.cursor_col.checked_sub(pane.col)?;
    if !input.cursor_visible {
        let (_, c) = last.filter(|&(r, c)| {
            usize::from(r) == row && usize::from(c) < col && col - usize::from(c) <= TYPED_RUN_MAX
        })?;
        return Some((row, usize::from(c), col + 1));
    }
    let first = last
        .filter(|&(r, c)| usize::from(r) == row && usize::from(c) < col)
        .map_or(col.saturating_sub(1), |(_, c)| usize::from(c));
    Some((row, first.max(col.saturating_sub(TYPED_RUN_MAX)), col))
}

/// Compare occupied ink at the same body rectangle before/after observation.
/// Moving a cat or a caret toward old ink cannot manufacture contact. This
/// tiny independent mask includes at most 16×32 cells; overflow is no claim.
/// Cells of the caret's own `typed` run ([`typed_run`]) read as blank.
fn local_ink(
    world: &PetWorld,
    body: PetRect,
    typed: Option<(usize, usize, usize)>,
) -> Option<[u64; 8]> {
    world.stamp()?;
    let r0 = (body.row - CONTACT_MARGIN).floor().max(0.0) as usize;
    let c0 = (body.col - CONTACT_MARGIN).floor().max(0.0) as usize;
    let r1 = (body.row + body.rows + CONTACT_MARGIN).ceil() as usize;
    let c1 = (body.col + body.cols + CONTACT_MARGIN).ceil() as usize;
    if r1.saturating_sub(r0) > 16 || c1.saturating_sub(c0) > 32 {
        return None;
    }
    let mut bits = [0; 8];
    for r in r0..r1 {
        let row_base = (r - r0) * 32;
        let typed_cols = typed.and_then(|(row, first, end)| (row == r).then_some(first..end));
        for c in c0..c1 {
            match world.ink_at(r, c) {
                None => return None,
                Some(true) if !typed_cols.as_ref().is_some_and(|cols| cols.contains(&c)) => {
                    let i = row_base + c - c0;
                    bits[i / 64] |= 1 << (i % 64);
                }
                Some(_) => {}
            }
        }
    }
    Some(bits)
}

/// Detect a TUI composer's painted cursor (or boxed prompt text tail) when the
/// program hides the hardware DECTCEM cursor and parks it on the status bar
/// below the composer.
fn detect_faux_composer_caret(
    input: &aterm_core::render::RenderInput,
    pane: PetPane,
) -> Option<(u16, u16)> {
    let start_r = pane.rows / 2;
    let end_r = pane.rows.saturating_sub(1);
    for local_r in (start_r..end_r).rev() {
        let frame_r = pane.row + local_r;
        let Some(row) = input.cells.get(frame_r) else {
            continue;
        };
        let mut prompt_c = None;
        let mut boxed = false;
        for local_c in 0..6.min(pane.cols) {
            let Some(cell) = row.get(pane.col + local_c) else {
                break;
            };
            match cell.ch {
                '│' | '┃' | '║' | '|' if local_c == 0 => boxed = true,
                ' ' => {}
                '>' | '❯' | '›' => {
                    prompt_c = Some(local_c);
                    break;
                }
                _ => break,
            }
        }
        let Some(prompt_c) = prompt_c else {
            continue;
        };
        let base_bg = row.get(pane.col + prompt_c).map(|c| c.bg);
        let mut last_text: Option<(usize, usize, usize)> = None;
        for comp_r in local_r..end_r {
            let Some(comp_row) = input.cells.get(pane.row + comp_r) else {
                break;
            };
            let Some(first_cell) = comp_row.get(pane.col) else {
                break;
            };
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
                    let indented = (0..=prompt_c).all(|c| {
                        comp_row
                            .get(pane.col + c)
                            .is_some_and(|cell| cell.ch == ' ')
                    });
                    if !indented {
                        break;
                    }
                }
            }
            let mut right_limit = pane.cols.saturating_sub(1);
            if boxed {
                for c in ((prompt_c + 1)..pane.cols).rev() {
                    let Some(cell) = comp_row.get(pane.col + c) else {
                        continue;
                    };
                    if cell.ch == ' ' {
                        continue;
                    }
                    if matches!(cell.ch, '│' | '┃' | '║' | '|') {
                        right_limit = c;
                    }
                    break;
                }
            }
            let start_c = prompt_c + 1;
            for local_c in start_c..right_limit {
                let Some(cell) = comp_row.get(pane.col + local_c) else {
                    break;
                };
                let inverse_caret = base_bg.is_some_and(|bg| cell.bg != bg)
                    && comp_row
                        .get(pane.col + local_c + 1)
                        .is_none_or(|next| next.bg != cell.bg);
                if cell.ch == '█' || inverse_caret {
                    return Some((comp_r as u16, local_c as u16));
                }
                if cell.ch != ' ' && cell.ch != '\0' {
                    last_text = Some((comp_r, local_c, right_limit));
                }
            }
        }
        if boxed {
            if let Some((tr, tc, right_limit)) = last_text {
                let col = (tc + 1).min(right_limit.saturating_sub(1));
                return Some((tr as u16, col as u16));
            }
            let col = (prompt_c + 2).min(pane.cols.saturating_sub(1));
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
        // Record this exact footprint with blank space at its near edge. The
        // held lead makes col 20 the station (caret 16 + 1 + 3): the cat has
        // arrived, so the brace is drawn on a body at rest.
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

        // SCP reprojects stored characters without changing the world stamp.
        // Refresh the row history from the rendered cells, then retain that
        // history when the exact same observation is read again.
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

        // A palette-only change preserves the characters and stamp, but the
        // inverse-colour painted caret has gone. Detect the actual text tail.
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
    }

    /// A contact on a body the chase is walking keeps the walk on glass: the
    /// brace and its recovery are poses of a body at rest, and drawn over a
    /// walk they slid along the row (the long-insert replay: 15 frames of
    /// `PetContactRecover` gliding a cell and a quarter). The contact itself
    /// is still noticed and held. Negative control: the same ink on the same
    /// body at rest is braced (`contact_requires_new_local_ink_…`).
    #[test]
    fn a_walking_body_wears_its_gait_through_a_contact() {
        let mut s = Scene::new();
        // No held lead: the station is col 17, three cells behind the body.
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

    /// An incoherent snapshot licenses nothing (no move toward its caret, no
    /// legacy placement, no first appearance), but a body already on glass
    /// holds where the last certified frame judged it until an unbroken blind
    /// run of `VETO_BLIND` fades it. Controls: the hold ends, and a pet not on
    /// glass draws nothing.
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
        // The grace is bounded: blindness that never ends is obstruction.
        let mut last = f;
        for _ in 0..80 {
            last = incoherent(&mut s, 16);
        }
        assert_eq!(last.alpha, 0, "an unbroken blind run fades the body out");
        assert_eq!((last.col, last.row), (before.col, before.row));

        // A pet that was not on glass is never materialized by an uncertified map.
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

    /// A content perch taken after a command finished must not leave the
    /// resident with no future (`console_frames()` `Some(false)` and
    /// `console_deadline()` `None`): it must still breathe, blink, settle,
    /// sleep and notice the caret leaving.
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
        // Let the pet arrive (it walks there, costing real frames), then hold
        // the screen still; the dwell measures settled time, so wait on the pet
        // itself.
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
        // That future is its own life: the perch is spent, so the pet goes back to
        // escorting the caret. The budget runs just past `PERCH_DWELL`.
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

    /// The escort: with no perch in reach, `console_station` answers `None` rather than
    /// "stay where you are", so the pet keeps following the caret. The fixture's DECDWL
    /// lines have no certified cell-to-pixel projection, so every cell is `Protected` and
    /// the search comes back empty.
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

        // A first sighting can use clear ground beyond the nearby search.
        // Only these two rows have a certified projection; their blank left
        // strip is more than REST_REACH from the protected desired stand.
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

    /// THE STREAM WATCH OUTRANKS THE OUTPUT HOLD. A running command's
    /// output hold has the cat at a seat it may hold, and the brain is
    /// already staring at the stream: the stare goes on (the cat perks, the
    /// heat only cools) and the resident stands aside, instead of zeroing the
    /// heat on its first held frame. Measured in the recorded Claude Code take
    /// (`full_screen_tests`): the inline answer was stared at, hugged and
    /// waited up for only while the cat's seat at the pane's wall failed the
    /// console law's clearance; a seat a fifth of a cell inside it lost all
    /// three.
    /// NEGATIVE CONTROL (as run, failing this test): `watching_stream`
    /// answering `false`.
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

    /// Checks the retained brain field itself, not a body that happens to stay nearby.
    /// Every snapshot comes from a real Terminal; no input intent is manufactured.
    #[test]
    fn hidden_cursor_home_continuity_conforms_to_real_terminal_motion() {
        use aterm_core::terminal::{ContentScrollDelta, ContentScrollState};
        use std::collections::BTreeMap;
        let model = hidden_home_continuity_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        let known: &[&[u8]] = &[
            b"", // hiding alone preserves the existing home
            b"\x1b[1;18r\x1b[18;1H\n",
            b"\x1b[3;18r\x1b[8;1H\x1b[2L",
            b"\x1b[3;18r\x1b[8;1H\x1b[2M",
            b"\x1b[3;18r\x1b[3;1H\x1bM",
            b"\x1b[20;1H\n", // ordinary full-screen uniform scroll
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
                // SU scrolls the rectangular margins even when the cursor is
                // outside them; LF at that column would be a no-op.
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
                _ => {} // a different session owns the same terminal snapshot
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
            // Observe coherent resized geometry too; rejecting an incoherent
            // pane would not prove the durable-geometry fence.
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

        // Multi-step trace on one Scene: KnownMove -> UnknownMove -> Observe -> KnownMove.
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

        // 1. KnownMove: hide cursor and scroll within region.
        s.term
            .process(b"\x1b[?25l\x1b7\x1b[1;18r\x1b[18;1H\n\x1b[r\x1b8");
        s.frame(0.016);
        let next = state_of(&s, 1);
        assert!(model.successors("KnownMove", &model_state).contains(&next));
        model_state = next;

        // 2. UnknownMove: switch to alternate screen while hidden.
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

        // 3. Observe: show cursor on the new surface to re-establish home.
        s.term.process(b"\x1b[6;9H\x1b[?25h");
        s.frame(0.016);
        let next = state_of(&s, 0);
        assert!(model.successors("Observe", &model_state).contains(&next));
        assert_eq!(s.pet.console.cursor_home, Some((5, 8)));
        model_state = next;

        // 4. KnownMove on the re-established home preserves it.
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
        // Settle the resident into deep sleep on glass (`elapsed` clamps at 5.0s).
        for _ in 0..10 {
            s.frame(5.0);
        }
        assert_eq!(s.pet.action(), PetAction::Sleep);
        assert!(s.pet.console.on_glass);
        assert!(s.pet.console.hold.settled());

        // Start a blind run on glass (unsettled while shown).
        let _ = s.pet.console.hold.update(s.now, None);
        assert!(!s.pet.console.hold.settled());
        assert!(s.pet.needs_frames());

        // Retire the caret on a new surface so the pet fades to alpha 0 in sleep
        // (`FADE_OUT` = 0.45s < `VETO_BLIND` = 0.50s).
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
    /// A look swap that lands while a command's output owns the resident must
    /// still ramp the incoming cat in, not leave it at partial opacity until
    /// the prompt returns: resident-owned ticks advance the arrival clock.
    #[test]
    fn a_look_swaps_arrival_ramp_runs_while_the_console_resident_owns_the_tick() {
        let mut s = Scene::new();
        // A held selection makes the resident own every tick (the reading
        // arm returns its own frame before the caret follower runs).
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
        // The quiet homecoming's ramp, exactly as the handoff arms it.
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

    /// A look swap on a cat standing on text: the departing ghost (the same
    /// body a frame earlier) keeps the body's rule, so ink does not cull it
    /// while a protected surface still does.
    #[test]
    fn a_departing_ghost_over_ordinary_ink_survives_and_over_a_selection_does_not() {
        let mut s = Scene::new();
        // Ink under the whole body (row 6 is the caret row, the body spans
        // rows 5-6, columns 21-26 at cell 10x20 → text at 1-based 5;21..).
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
        // A selection over the same cells: the body steps aside and its still
        // ghost steps aside with it, one cat and one move.
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
        // A running ghost has its own door and does not follow the body: over
        // the selection it is culled at once, the only cull a ghost is dealt
        // alone.
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

    /// A ghost on its own glass (`PetBrain::displace_resight`) is spent in the
    /// pane the focus left only where that pane's last observation certified
    /// nothing protected under it; it then stands beneath that pane's text
    /// while the arriver comes up at its own station. Control: a selection
    /// under the body refuses it.
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
            // The new pane's caret, elsewhere: the arriver's station.
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

    /// A ghost is carried across a boundary at what its own slot drew
    /// (`PetBrain::last_drawn`), slot for slot. Control: a slot that drew
    /// nothing is not carried.
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

    /// A look swap landing while a hidden-caret resident holds the body keeps
    /// both halves of the crossfade: the arriver ramps in and the departing
    /// ghost is drawn under the same hold.
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
        // The debounced handoff lands on a free tick (the streamer's
        // caret-visible frame between two repaints), never on a resident's.
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
        // The Claude Code shape: the caret hides for the repaint and the
        // resident holds the body, owning every tick.
        s.term.process(b"\x1b[?25l");
        let mut ticks = 0u32;
        let mut resident_ticks = 0u32;
        loop {
            let f = s.frame(0.016);
            ticks += 1;
            if s.pet.console.resident {
                resident_ticks += 1;
            }
            // The verdict's factor on this frame: the lane byte carries the
            // hold's cover and nothing else.
            let cover = u32::from(f.lane_alpha);
            let ghost = f
                .departures
                .iter()
                .flatten()
                .map(|d| u32::from(d.alpha) * cover / 255)
                .max()
                .unwrap_or(0);
            let arriver = u32::from(f.alpha);
            // Complementary ramps: max(1 − u, u) ≥ ½ on every tick …
            assert!(
                ghost.max(arriver) * 2 + 2 >= pre * cover / 255,
                "tick {ticks}: max(ghost {ghost}, arriver {arriver}) fell under half the \
                 pre-swap alpha × cover ({pre} × {cover}/255)"
            );
            // … and the two sprites composited alpha-over never cover less
            // than ~75 % of what the body alone covered before the swap.
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

    /// The ghost's own rectangle gets the body's rule and nothing stricter: a
    /// selection culls it at once and nothing else culls it alone. Here the
    /// ghost stands on a double-width line (protected, not a selection) while
    /// the body is clear three rows up; the ghost runs its ramp to the end.
    #[test]
    fn a_departing_ghost_over_a_protected_line_fades_with_the_body_instead_of_vanishing() {
        let mut s = Scene::new();
        // 1-based rows 9-10 double width: the ghost's two rows.
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

    // ── the typed kitty commands, with an observed world ─────────────────
    // The brain's own trick tests run with no console world, so
    // `begin_console_tick` returns before it can wipe a latch or seat a
    // resident. These drive the real terminal, input notes and OSC 133 states.

    impl Scene {
        /// Type `text` the way the host does: one input note per key, then
        /// the echo, a frame every 50 ms.
        fn type_text(&mut self, text: &str) {
            for ch in text.bytes() {
                self.input(PetInputKind::Text);
                self.term.process(&[ch]);
                self.frame(0.05);
            }
        }

        /// A command that is still running: prompt, command line, the `C`
        /// mark and a line of output, the caret left on the output row.
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

        /// Frames at 60 fps for `secs`, collected.
        fn watch(&mut self, secs: f32) -> Vec<PetFrame> {
            (0..(secs / 0.016).ceil() as usize)
                .map(|_| self.frame(0.016))
                .collect()
        }
    }

    /// Treat frolics on glass: performed directly, past [`INPUT_HOLD`] (the
    /// console layer wipes `pending_cheer` on every tick inside the hold), it
    /// plays with its motes and poses.
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

    /// A commanded sleep survives a running command: the `C..D` block's
    /// resident forces `Sit`, so the affection arm reads the level
    /// (`trick_settle`), not the latch, and the nap lasts.
    #[test]
    fn a_commanded_sleep_survives_an_executing_block() {
        let mut s = Scene::new();
        s.executing();
        s.pet.note_trick(s.now, Trick::Sleep, true);
        // The latch is not a term of `console_frames` (a parked resident
        // answers the cadence question first). The host asks for the frame that
        // reads the latch, and that tick takes the cat off the resident and
        // arms the lane.
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
        // Output that moves the caret is a caret move: the cat wakes, the
        // level stands down, and the command has its watcher back.
        s.term.process(b"\r\nmore output");
        s.frame(0.016);
        assert!(s.pet.trick_settle.is_none());
        let _ = s.watch(2.5);
        assert!(s.pet.console.resident, "the resident gets the cat back");
        assert_eq!(s.pet.console_attention(), PetAttention::Output);
    }

    /// …and so does a one-shot: a roll asked for during a build plays out
    /// instead of being cut to its first pose by the resident's forced seat.
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

    /// A LANDING CHEERS OVER A RUNNING COMMAND (§11.3 of
    /// `docs/DESIGN-harness-autopilot-2026-10-15.md`): an agent's TUI is one
    /// executing block (`C` with no `D`), whose resident watches its output
    /// and holds the cat, wiping its motes, on every tick it owns. The
    /// landing's cheer outranks that watch as affection does: the cat is
    /// taken off the resident, perks, frolics with its three ♪/♥ on glass
    /// together, and once the last of them has gone the running command has
    /// its watcher back and the frames stop. (The ♪/♥ are counted in the
    /// air, in the brain: on glass the console law hides each where it would
    /// cross ink or the caret's ring, as it does every mote.) NEGATIVE
    /// CONTROL (as run, failing this test): the cheer's term left out of the
    /// release, the resident keeps every tick and the cheer never plays.
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

    /// An agent's pane as Claude Code 2.1.304 leaves it at a task's end
    /// (read off a lab instance, `aterm ctl text`): its banner, the prompt
    /// on its full-width band, the reply, one blank row between lines, the one-row composer boxed by
    /// two rules, the mode line, and the terminal's own cursor hidden at the
    /// composer, all inside one running command. 30 x 100 at 15 x 28 px
    /// cells, fed the way the host feeds a hidden caret (present, not
    /// painted), minus the host's chrome over it as the host reserves it
    /// (`pet_console_exclusions`): the harness card's line on the upper rule
    /// and the footer's chips on the lower one. The third field is the
    /// pane's geometry ([`PaneGeom`]): the lab's 100 columns at 2x, or a
    /// narrower pane ([`AgentPane::narrow`]).
    struct AgentPane(Scene, Option<PetRect>, PaneGeom);

    /// An agent pane's size and cell metrics, and the host's chrome over
    /// its composer's rules: the harness card's line on the upper rule (its
    /// last 38 columns) and the footer's chips on the lower.
    #[derive(Clone, Copy, Debug)]
    struct PaneGeom {
        cols: u16,
        /// 30 (open ground under the composer), or 16: the composer at the
        /// pane's foot, as a session's pane is once its transcript fills it.
        rows: u16,
        cell: (u16, u16),
    }

    impl PaneGeom {
        /// The lab's pane: 100 x 30 at 2x (15 x 28 px cells).
        const WIDE: Self = Self {
            cols: 100,
            rows: 30,
            cell: (15, 28),
        };

        fn chrome(self) -> [PetRect; 2] {
            let card = f32::from(self.cols.min(38));
            [
                PetRect::new(12.0, f32::from(self.cols) - card, 1.0, card),
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

        /// The pane with `transcript` (its twelve rows over the composer)
        /// drawn first, then the composer's rules, prompt and footer on rows
        /// 12 to 15, the rows under them blank.
        fn with_transcript(transcript: &str, warm: usize) -> Self {
            Self::with_geom(transcript, warm, PaneGeom::WIDE)
        }

        /// A narrow agent pane, `cols` wide at `cell` px cells, as the lab
        /// leaves Claude Code 2.1.304 at 44 columns when Land lands (read off
        /// a lab instance, `aterm ctl text`): the tail of the first turn, the
        /// harness's prompt on its band, the clean defense wrapped onto two
        /// rows, and the done line beside open ground, over the composer.
        fn narrow(cols: u16, cell: (u16, u16)) -> Self {
            Self::narrow_at(cols, 30, cell)
        }

        /// [`Self::narrow`], `rows` tall ([`PaneGeom::rows`]).
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

        /// Frames until the cat parks (no frame owed); whether it did within
        /// `secs`.
        fn parks_within(&mut self, secs: f32) -> bool {
            for _ in 0..(secs / 0.016) as usize {
                self.frame();
                if !self.0.pet.needs_frames() {
                    return true;
                }
            }
            false
        }

        /// The pane as the host observes it, its chrome reserved, with no
        /// tick.
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

    /// A LANDING IN AN AGENT'S PANE CHEERS ON OPEN GROUND (§11.3): the
    /// transcript's single blank rows are too little air for a ♪, and the
    /// console law hides every mote that would cross ink or the caret's ring
    /// (`finish_console_frame`), so the cat hops once to ground where its
    /// body and the air above it are clear (`PetBrain::cheer_stage`), perks,
    /// frolics with all three ♪/♥ on glass, and stays while they rise; then
    /// it goes home on glass, as it came, and parks: a home it may stand on
    /// is no reason to leave the glass.
    /// NEGATIVE CONTROLS (as run, each failing this test): no stage (the
    /// cheer played where the cat stands), not one ♪/♥ reaches the glass; a
    /// spent cheer that leaves the glass whatever its home
    /// (`room_cheer_leaving` always set), the cat fades out on its stage.
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

    /// A CAT ASLEEP ON THE TRANSCRIPT WAKES INTO THE CHEER, NOT INTO AN
    /// EVICTION (§11.3): where the landing finds the cat (asleep under the
    /// agent's last line, its feet on the composer's upper rule, which the
    /// host reserves whole at a task's end, as a lab found it), the eviction
    /// would hop it off with its pose handed back on landing, and in an
    /// agent's pane, where the escort's stand is never clear, hop it again on
    /// every landing. A landing's cheer is never evicted: the cat wakes,
    /// goes once to its stage, and cheers there with its three ♪/♥ on
    /// glass. It wakes without the stretch (2026-10-20): its body there lies
    /// over the done line's `✻`, and the stretch drawn there showed its ears
    /// through the foot of that word in the owner's captures
    /// ([`a_sleeper_stretches_only_on_blank_ground`]). Spent, it goes home on
    /// glass, to the clean ground beside its caret it can walk to
    /// (`console_station`'s rest seat: the escort's stand is under the
    /// reserved rule), and parks. NEGATIVE CONTROLS (as run, each failing
    /// this test): the cheer evicted like any pose, the cat hops and never
    /// frolics; the stage's path swept from a body already over the reserved
    /// rule, no stage is ever clear and the ♪/♥ rise into the words, where
    /// none is drawn; the rest seat chosen with no reach (`rest_reach_from`
    /// answering `None`), the spent cat chases a seat beyond the composer's
    /// rules and never parks.
    #[test]
    fn a_cat_asleep_on_the_transcript_wakes_into_the_cheer_not_an_eviction() {
        let mut p = AgentPane::new();
        // The upper rule reserved whole, as the host reserved it at a task's
        // end in a lab instance (its map read cell by cell at the landing),
        // and the sleeper where that lab found it: its feet, rounded to the
        // pixel, on that rule.
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

    /// A SUCCESS STRETCHES ONLY ON BLANK GROUND (the console's result,
    /// 2026-10-20): a command that ends well earns a stretch where the cat
    /// is, held for [`RESULT_HOLD`]; where a line of output runs through its
    /// body that stretch would be drawn under the words, ears through them,
    /// so there it keeps its own pose through the hold, the result still
    /// answered; on blank ground it stretches, in front, as before. Under
    /// the words only the stretch is withheld: the cat stays where it lies,
    /// on glass, drawn behind the words in its own pose, as ordinary ink's
    /// residency let it lie there before the success, and nothing else
    /// marks the success. NEGATIVE CONTROLS (as run, each failing this
    /// test): `console_stretch_on_blank` answering `true`, the stretch is
    /// drawn under the output's words; the `Result` arm always stretching,
    /// the same; the ink's z-order not held (`ZHold::update` answering
    /// `false`), the cat is drawn over the words.
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
                // A line of output across the cat's body, its caret back home.
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

    /// EVERY WAKE STRETCHES ONLY ON BLANK GROUND (2026-10-20): the landing's
    /// and the success's stretches are not the only ones. A key wakes a
    /// sleeper wherever it lies, and a shell's ordinary ink lets a sleeper lie
    /// under a line of output (drawn behind the glyphs); a trick asks for a
    /// stretch too. Each is the brain's `Waking`, and its look draws the
    /// stretch, which there would be drawn under the words. Where the
    /// stretched body would cover any ink the cat wakes in place sitting up
    /// instead (`PetBrain::console_stretch_on_blank`, read by the look); on
    /// blank ground it stretches as before. NEGATIVE CONTROL (as run, failing
    /// this test): the look's guard removed, the key's wake is drawn as a
    /// stretch under the output's words.
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
                // A line of output across the cat's body, its caret back home.
                s.term.process(
                    format!(
                        "\x1b[{};{}Hall 42 tests passed in 31.9s\x1b[6;17H",
                        row as usize + 1,
                        col.floor() as usize
                    )
                    .as_bytes(),
                );
            }
            // The output read and settled, then asleep where it stands.
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

    /// A SLEEPER'S FALL STRETCHES ONLY ON BLANK GROUND (2026-10-20): a line
    /// typed beside a sleeping cat that wraps under it takes the row it lay
    /// on, and the cat falls one row (the drop), spending its wake stretch in
    /// the air, which was drawn over the words that took the row (a lab's
    /// shell showed it over `echo 'all 42`). The fall is judged at both ends:
    /// where it falls from (its head under the line above) and where it
    /// lands (its feet on the row the caret opened). Over ink at either end
    /// it stays asleep through that beat, then descends; where both are
    /// blank (spaces) the fall still stretches. Four floors: the typed words
    /// (ink at both ends), a blank line (none), a line of output over its
    /// head with a blank line under it (ink where it falls from only), and a
    /// painted band under it (ink where it lands only: a band is no glyph,
    /// so it never refuses the drop). NEGATIVE CONTROLS (as run, each
    /// failing this test): the fall's guard removed, the stretch is drawn
    /// over the typed words; only the start judged, the stretch is drawn
    /// over the band; only the end judged, it is drawn under the output.
    #[test]
    fn a_sleepers_fall_stretches_only_on_blank_ground() {
        // Each floor: what is written on the row over the sleeper's head and
        // on the row under its feet before it sleeps, whether words are
        // typed (else spaces), and whether the ground is blank where it
        // falls from and where it lands. The fall may stretch only where
        // both are.
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
            // The rows over the sleeper's head and under its feet, the caret
            // back home (row 5, column 16).
            s.term
                .process(format!("\x1b[5;1H{above}\x1b[7;1H{below}\x1b[6;17H").as_bytes());
            s.watch(3.0);
            s.pet.set_action(PetAction::Sleep);
            s.pet.quiet = 30.0;
            let asleep = s.watch(0.064).pop().expect("a frame");
            assert_eq!(asleep.action, PetAction::Sleep, "{why}: fixture: asleep");
            assert_eq!(s.pet.row, 5.0, "{why}: fixture: asleep on the caret's row");
            // From the caret to the row's end and three cells on: the caret
            // wraps one row down, under the sleeper's row.
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
            // The two ends as the guard judges them, for the fixture.
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
        /// Every frame that draws the stretch (`PetStretch`,
        /// `PetStretchHind`) over a cell of the map that is not blank
        /// ground: `(frame, cells)` for each.
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

    /// A SLEEPER STRETCHES ONLY ON BLANK GROUND (2026-10-20): the owner's
    /// captures of a landing in front showed the stretch drawn over the foot
    /// of the done line's first word — the console law had stepped the
    /// sleeper off the composer's reserved rule onto ground behind the text,
    /// and the cheer's wake stretched there, under the glyphs. Where the
    /// stretch would cover any cell that is not blank ground the cat wakes
    /// without it, straight into its hop to the stage, with no wake beat
    /// before it (`PetBrain::console_stretch_on_blank`); asleep on blank
    /// ground it still stretches awake. Either way it frolics with its three
    /// ♪/♥ on glass. Ground with no glyph on it that the host's chrome is
    /// drawn over is no blank ground for the stretch either
    /// (`PetWorld::ink_clearance` counts a protected cell as ink); the
    /// console law moves a body off such ground on its next frame (an
    /// eviction, `console_obstructed`), before a landing could wake it
    /// there, so the guard's own answer is what is asserted. NEGATIVE
    /// CONTROLS (as run, each failing this test): the stretch played
    /// wherever the cat lies (`console_stretch_on_blank` answering `true`),
    /// it is drawn over the done line's `✻` at the lab's spot; the landing's
    /// guard dropped, a wake beat before the hop (the `Waking` the look
    /// draws sitting up over the words); the map's ink not counting what
    /// protects a cell (`others` dropped from `inked` in `PetWorld`), the
    /// guard answers yes under the host's chrome.
    #[test]
    fn a_sleeper_stretches_only_on_blank_ground() {
        // Asleep where the lab found it at a landing in front: its body over
        // the done line's `✻`, its feet on the reserved rule.
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

        // Asleep at its home, the clean ground beside the transcript's last
        // line: the stretch plays there, on blank ground.
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

        // Asleep at that home with the host's chrome drawn over the blank
        // ground under it: no ink there, but protected.
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
        /// The pane as a re-sighting leaves it at a task's end: the upper
        /// rule reserved whole, and the cat at its home (`console_station`'s
        /// rest seat: clean ground beside the caret, the escort's stand
        /// being under that rule) but off glass, as a switch onto the landed
        /// tab or a window's focus finds it, a fresh sighting not yet faded
        /// in. Its home, `(col, row)`, is the second half.
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

        /// After a cheer: the cat parks within ten seconds on glass, at its
        /// home (`home`, where given) on clean ground — no glyph, rule or
        /// chrome under its body — owing nothing.
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

    /// A LANDING SEEN AT FIRST LOOK (§11.3: a switch onto the landed tab, a
    /// window's focus): the cat is off glass at its home, clean ground beside
    /// the caret (the escort's stand is under the host's chrome). It has no
    /// flight anyone could watch. It appears on its stage instead, fading in
    /// there (a sleeper stretching awake), perks and frolics with its three
    /// ♪/♥ on glass; spent, it goes home on glass and parks there. Awake and
    /// asleep. NEGATIVE CONTROL (as run, failing this test): no appearance
    /// (`appear_on_cheer_stage` answering `false`), the cat is seen flying
    /// in from its home.
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
        /// The landing's cheer from where the cat stands: five seconds of
        /// frames, and the stage's air the brain chose on the way
        /// (`PetBrain::room_cheer_air`, taken by the frolic).
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

        /// The most ♪/♥ a frame of `frames` draws on glass together, and
        /// how many times two of their drawn tiles meet
        /// (`crate::kitty_pet::mote_box_px`, the emitter's own placement).
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

    /// A LANDING IN A NARROW PANE SHOWS ITS NOTES (§11.3, 2026-10-20): the
    /// stage kept the lanes' air on both sides of the cat, which no ground
    /// within reach of a 44-column pane held at 2x or at 1x, so the cat
    /// cheered where it stood and the console law hid every ♪/♥ against the
    /// reply's words (a lab's capture at 44 columns: not one on screen,
    /// where main's three rose bunched beside the done line). The stage now
    /// keeps the lanes' air on one side, the side the cat faces first, then
    /// the other ([`CheerAir::Lanes`]); only where neither finds air within
    /// reach, the compact spray's ([`CheerAir::Compact`]). At 44 columns,
    /// 2x and 1x, the three rise in their lanes on glass together, no two
    /// tiles meeting; at 41 columns at 1x, where no lanes' air is within
    /// reach, they rise in the compact spray, close but on glass. None
    /// takes more than one flight (`a_stage_is_reached_in_one_flight`).
    /// These panes end at the composer's footer, as a session's does once
    /// its transcript fills it: the cat rests above the composer, by the
    /// reply. With open ground under the composer, the cat rests there
    /// (clean ground a bound over the composer's rules), and at 41 columns
    /// at 1x the lanes have air beside it.
    /// NEGATIVE CONTROLS (as run, each failing this test): the lanes' air
    /// kept on both sides (the `Lanes` arm of `cheer_air` symmetric), at 44
    /// columns at 1x the compact spray, the lanes' air nowhere within reach;
    /// that with the compact tier dropped too (the stage as first built),
    /// not one ♪/♥ on glass there; the compact tier dropped alone, not one
    /// at 41 columns at 1x.
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

    /// A STAGE IS REACHED IN ONE FLIGHT (§11.3, 2026-10-20): a stage is
    /// judged at the seat as the body will be drawn there (rounded to the
    /// pixel), so a cat that lands on it finds the ground it was promised
    /// and cheers. In a 60-column pane at 1x, the composer's upper rule
    /// reserved, a cat at rest behind the reply's last line hops once to
    /// ground beside it and frolics with its three ♪/♥ on glass. NEGATIVE
    /// CONTROL (as run, failing this test): the stage judged at its shifted
    /// copy (the body where the cat stands, moved), the seat as drawn sits
    /// half a pixel off it and the cat hops a second time.
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

    /// A CAT AT THE PANE'S EDGE CHEERS FACING INTO THE PANE (§11.3,
    /// 2026-10-20): the stage keeps the lanes' air on one side only, so the
    /// cat faces that side when it perks and when it frolics; its ♪/♥ rise
    /// the way it faces. A cat on open ground against the pane's right edge,
    /// facing the edge, has no air there: it turns into the pane, where the
    /// air is, perks and frolics facing left, and its three ♪/♥ are on glass
    /// together, no two tiles meeting. NEGATIVE CONTROLS (as run, each
    /// failing this test): the stage's side not taken (the perk and the
    /// frolic keep the facing the cat had), it frolics facing the edge; the
    /// lanes' air kept on both sides, there is none where it stands and it
    /// hops away.
    #[test]
    fn a_cat_at_the_panes_edge_cheers_facing_into_the_pane() {
        let mut p = AgentPane::narrow(60, (15, 28));
        assert!(p.parks_within(10.0), "fixture: the cat parks");
        // Open ground right of the first done line, against the edge.
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

    /// THE HOST RESERVES THE RULE AS THE LANDING COMES DOWN (§11.3): the cat
    /// was at its caret home on the composer's row (where the escort sat it
    /// before its rest seat moved to clean ground), and on the frame the end
    /// lands the composer's upper rule is reserved whole over it. Its flight
    /// to its stage passes over the rule: the console law leaves a body in
    /// the air alone, so the cat comes down on the stage it set out for after
    /// one flight and cheers there with its three ♪/♥ on glass; then it goes
    /// home on clean ground and parks. The cat is seated there by hand: the
    /// fixture's home is clean ground now.
    /// NEGATIVE CONTROL (as run, failing this test): the law stepping a body
    /// in the air aside, as it did, the cat's flights over the composer's
    /// rules are turned back off them and it never parks.
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
        // The landing eases onto the stage; within half a pixel (15 px cells)
        // it is there.
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

    /// A HOME UNDER THE FOOTER CHEERS ON THE GROUND BELOW IT (§11.3): in a
    /// tall pane the cat at rest on the blank rows just under the composer's
    /// footer keeps its ground there (`console_station`'s kept seat: the
    /// ground a cat rests on, clean and within reach of the caret's refused
    /// stand, is its seat). At a task's end, with the
    /// composer's upper rule reserved whole, nothing above its home is open
    /// ground it can reach: the composer's rules and the footer are between.
    /// The stage search, finding nothing above, looks below, so the cat goes
    /// once to the open ground under its home, perks and frolics with its
    /// three ♪/♥ on glass, and goes home on glass and parks. NEGATIVE CONTROL
    /// (as run, failing this test): the stage searched upward only, no stage
    /// is found and not one ♪/♥ reaches the glass.
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

    /// A KEY SPENDS A LANDING'S CHEER (§11.3.4): a key in the agent's pane
    /// before the cheer has played wipes it, the console law for every
    /// verdict latch, and takes the cat for the typing; nothing plays, then or
    /// after, and the running command has its watcher back. NEGATIVE CONTROL
    /// (as run, failing this test): a key that leaves the landing owed (the
    /// fresh key's and the live key's wipes of the latch left out), the cheer
    /// plays after the typing.
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

    /// A held selection drops the latch: nothing plays over a selection, and
    /// nothing replays when it is released.
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

    /// Program output never re-stamps the latch: with `sleep 1 && make` a
    /// streaming build moves the caret on every line, but the latch expires by
    /// [`TRICK_TTL`] and the running command's resident gets its perch back.
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
                // Two line lengths: at the bottom row the pane scrolls instead
                // of the caret stepping down, and only the column changes.
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

    /// Fixing a typo is still typing the word: `sot`, two backspaces, `it`,
    /// Space fires with the repair's resident hold standing; the trick latch
    /// rides it out and plays when the hold lapses.
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

    /// The same repair asking for `sit`: the latch rides the repair out, the
    /// cat looks up when the hand stops, then holds the seat it was asked for
    /// with no resident reseating it and no typing pose overriding it.
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

    /// A key wipes the verdict latches (cheer, sulk) and stands a commanded
    /// seat down, but never takes an owed trick: the word's own Enter is a
    /// fresh input, and the verb is owed to the following prompt.
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
        // A key the program swallowed: an input note, and no echo.
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

    /// Exit-127 forgiveness reaches the console layer: no failed result pose
    /// and no failure hush, where an ordinary fast failure gets both.
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

    /// Enter inside the heard-you beat, in the key handler's order (input note
    /// and confirmation first, then echo and tick): `roll`, a pause, the cat
    /// looks up, and Enter lands a tenth of a second in. A look is not an
    /// answer, so the confirmation latches and plays once at the new prompt.
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
        // Backspace starts a repair, then forward typing advances past the repair target.
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

        // Place a right-hand box border ahead on the same row and type toward it;
        // the station must never step leftward onto the caret or sawtooth as lead decays.
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
        // Draw a boxed composer at rows 15..=17 (1-indexed 16..=18) and a status bar on row 19 (20),
        // then hide DECTCEM and park the hardware cursor at the bottom-left cell (row 19, col 0).
        s.term.process(
            b"\x1b[16;1H\xe2\x95\xad\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x95\xae\
              \x1b[17;1H\xe2\x94\x82 > hello world             \xe2\x94\x82\
              \x1b[18;1H\xe2\x95\xb0\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x95\xaf\
              \x1b[20;1H  26 files changed\x1b[20;1H\x1b[?25l",
        );
        // Run well past HIDDEN_HOLD_MAX (0.25s) feeding the unpainted caret at (19, 0) as CompanionOwner does:
        // the kitty must never chase the hidden bottom-left corner cursor and get stuck there.
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

        // Now wrap the boxed composer onto a second line with a block cursor `█` on row 17 (1-indexed 18)
        // and park the hidden hardware cursor at the end of the status bar (row 19, col 18).
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
    }

    #[test]
    fn bordered_tui_box_places_station_in_interior_blank_gap_instead_of_on_words() {
        let mut s = Scene::new();
        // Draw a full-width 80-column walled composer box on row 10 (1-indexed 11)
        // with top and bottom borders on rows 9 and 11, and put the caret at `hello` (col 6).
        let border = format!("+{}+", "-".repeat(78));
        let composer = format!("| > hello world{}|", " ".repeat(63));
        s.term.process(
            format!("\x1b[10;1H{border}\x1b[11;1H{composer}\x1b[12;1H{border}\x1b[11;7H")
                .as_bytes(),
        );
        // Feed the per-row hull spans (0..80 on all three rows because of the outer box walls).
        s.pet.sense_ink(9, &[(0, 80), (0, 80), (0, 80)], None);
        let frames = s.watch(1.5);
        let last = frames.last().copied().expect("frame");
        // Words `> hello world` occupy cols 2..15 on row 10. Inside the box, cols 16..79 are blank!
        assert!(
            last.col >= 15.0 && last.col <= 74.0,
            "kitty inside walled TUI box should step past the words into the interior blank space (col >= 15), got col {} row {}",
            last.col,
            last.row
        );
    }

    /// A full-screen program's console as the host shows it to the pet: a
    /// terminal of any size, the host's chrome (`aterm-gui`'s `pet_chrome`)
    /// as exclusions, the program's own cursor hidden or not, ticked at 60 fps
    /// from a pet that has never been on glass.
    struct Console {
        term: Terminal,
        pet: PetBrain,
        now: Instant,
        rows: u16,
        cols: u16,
        chrome: Vec<PetRect>,
        /// The host's transient overlays (the Autopilot menu's card).
        overlays: Vec<PetRect>,
        /// Whether the host's ink probe is fed before each tick
        /// ([`PetBrain::sense_ink`], every row's inked span and the newest
        /// inked row, as `WordDecorations::pet_ink` reads them), and whether
        /// the program's output is a burst ([`PetSense::output_burst`]).
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

        /// An agent's console: `screen` drawn by a program the shell runs
        /// (an executing OSC 133 block, as Claude Code is), so the resident
        /// watches its output.
        fn agent(rows: u16, cols: u16, screen: &str, chrome: Vec<PetRect>) -> Self {
            Self::new(
                rows,
                cols,
                &format!("\x1b]133;A\x07$ \x1b]133;B\x07claude\r\n\x1b]133;C\x07\x1b[2J{screen}"),
                chrome,
            )
        }

        /// The program repaints its whole screen.
        fn repaint(&mut self, screen: &str, chrome: Vec<PetRect>) {
            self.term.process(b"\x1b[2J");
            self.term.process(screen.as_bytes());
            self.chrome = chrome;
        }

        /// Frames for `secs`: the leaps among them, and when the pet first
        /// owed no frame (parked), in seconds.
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

        /// The frame after `secs` of quiet: long enough to arrive and settle.
        fn rest(&mut self, secs: f32) -> PetFrame {
            let mut last = self.frame();
            for _ in 1..(secs * 60.0) as usize {
                last = self.frame();
            }
            last
        }

        /// Every cell the drawn body covers that is not clear ground — a
        /// glyph, a coloured background, the host's chrome — by the world the
        /// pet itself observed.
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

    /// Claude Code 2.1.304's screen as the lab read it: the launch card on
    /// rows 0..=2, the composer between its two rules with Claude's painted
    /// block caret at column 2 (where it parks the hidden cursor), and the
    /// footer's words under the lower rule. `history` rows of words sit
    /// between the card and the upper rule, which is on row `rule`.
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

    /// The chrome aterm paints over that screen, as the lab's pet world read
    /// it (`r05:PPPP…`, `r07:IIIPPPPPIII…PPPP`): the harness card's glass
    /// with its air (rows 0..=4 when the upper rule is row 5), the upper
    /// rule's rainbow and labels across the pane, and the lower rule's chips
    /// (`∞` at the left, read when `⚙` still stood beside it, so its rect
    /// is a few cells wider than the chip; the model and the path at the
    /// right).
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

    /// OWNER-EYE-6 (2026-10-20), traced on a real Claude Code 2.1.304 in the
    /// lab at 120x40 and 220x50: the hidden cursor is ON Claude's painted
    /// composer caret, `(6, 2)`, so the faux-caret correction has nothing to
    /// correct. The 1.7-row body cannot stand at the caret — both rules round
    /// the composer are aterm's protected chrome — and the station's
    /// correction, ink-blind, seated it on the footer's words: `auto🐱de`.
    /// The resting pet now takes the nearest clean ground (two blank rows
    /// under the footer), and its home stays the real composer caret.
    ///
    /// NEGATIVE CONTROL: with the correction searching under text only
    /// (`nearest_under_text_perch` alone, as before), the body covers the
    /// footer's ink on row 8.
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

    /// The same screen near the bottom of a full pane, nothing clean above
    /// the upper rule (Claude's words to the margin): the nearest clean
    /// ground is the footer's blank tail on the last two rows, right of its
    /// words — never the words.
    ///
    /// NEGATIVE CONTROL: the under-text correction alone seats the body on the
    /// footer's words (row 14).
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

    /// A REAL CARET KEEPS ITS STAND. Beside a visible caret on a prompt with
    /// output to the margin above it, the ink ladder's stand is the caret's
    /// own row (its feet on blank ground, its ears over the line above, as
    /// they always were): nothing there is protected, so no clean-ground
    /// search moves the resting pet off its caret.
    ///
    /// NEGATIVE CONTROL (as run, failing this test): a correction that seats
    /// the pet beside every caret on clean ground (the protected gate and the
    /// ground plane's branch both removed, so every stand goes to
    /// `nearest_clear_perch`) moves the stand off the prompt row, to row
    /// 10.85. The protected gate alone removed passes here (the stand is
    /// not refused, and the ground plane's search answers the stand itself);
    /// it fails the full-screen replays instead
    /// (`full_screen_tests::a_main_screen_stream_still_perks_hugs_and_waits`
    /// and `a_full_screen_program_stands_down_a_vigil_armed_before_it`: the
    /// vigil never arms).
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

    /// The transcript a first turn leaves over the composer in the lab
    /// (Claude Code 2.1.304, `aterm ctl text`): the prompt on its band, the
    /// reply, the turn's status line, one blank row between each.
    const TURN: [(usize, &str); 3] = [
        (5, "\x1b[48;2;55;55;55m\u{276F} hi there\x1b[K\x1b[0m"),
        (
            7,
            "\u{23FA} Hi! I am the local lab stub, not a model. (reply 1)",
        ),
        (9, "\u{273B} Cooked for 0s \u{b7} done 6:54 AM"),
    ];

    /// A TURN MOVES THE COMPOSER PAST THE RESTING CAT (review of
    /// owner-eye-6, 2026-10-20; lab `rv-tl`, Claude Code 2.1.304 at
    /// 120 x 40): the cat rests on clean ground under the fresh footer; a
    /// turn repaints the transcript over it and the composer six rows down,
    /// the upper rule aterm paints across the pane between them. The clean
    /// ground nearest the new caret is under the new footer, beyond that
    /// rule. Chased, the console law stepped every flight back off the rule,
    /// and the cat leapt in place on `✻ Co🐱ked` for the whole 20 s window
    /// (about 890 frames under video); walled off from it instead, the cat
    /// parked out of the caret's way above the rule and took two flights at
    /// the next wrap. The law leaves a body in the air alone, so the cat
    /// makes one bound over the rule to the clean ground under the new
    /// footer, beside the caret, and parks there.
    ///
    /// NEGATIVE CONTROL (as run): the law stepping a body in the air aside,
    /// as it did, the cat never parks.
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

    /// Claude Code's composer as it redraws at each key (lab `tc-b1`, Claude
    /// Code 2.1.304 at 120 x 40, one turn above it): the upper rule on row
    /// 11, `❯ ` and the draft wrapped at 118 columns with its painted block
    /// caret after it (the hidden cursor on that caret), the lower rule
    /// under the draft's last line and the footer under that, the rows
    /// below blank. Its chrome as the host reserves it: the card's glass,
    /// the upper rule whole, the lower rule's chips.
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

    /// The pane a turn leaves, its composer at the pane's foot (a session's
    /// usual pane: no clean ground under the footer), the cat resting on the
    /// nearest clean ground, above the composer's upper rule, the host's ink
    /// probe fed as the host feeds it.
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

    /// A landing off its seats: where it came down, and the seats the
    /// station kept while it flew.
    type Landing = ((f32, f32), Vec<(f32, f32)>);

    /// A watch for hops, frame by frame: the flights it starts, its frames
    /// the console law stepped back off protected ground (`cursor-home`),
    /// and each landing neither where the flight was last aimed nor on any of
    /// the seats the station kept while it flew.
    #[derive(Default)]
    struct Hops {
        leaps: usize,
        asides: usize,
        missed: Vec<Landing>,
        seats: Vec<(f32, f32)>,
        aim: Option<(f32, f32)>,
        aloft: bool,
        /// Where the last frame drew the body, `(col, row)`.
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

    /// TYPED UP TO A LABEL ON THE RULE, THE CAT WAITS ON THE LINE (lab,
    /// real Claude Code 2.1.305 with the harness off, windowed, 2026-10-20).
    /// With the harness off the host still draws its switch's label at the
    /// right end of the composer's upper rule (`claude harness ○ off [on]`),
    /// and it is the host's chrome: the stands under it are refused. The cat
    /// escorting the caret along the composer's line was sent to a seat off
    /// the line there, so it hopped up off the line at the label and down
    /// again at the wrap. It now waits on the line, at the last stand short
    /// of the label, and the wrap is its one bound.
    ///
    /// NEGATIVE CONTROL (as run): sent to a seat off the line, the cat hops
    /// up off it at key 75, a flight before the wrap.
    #[test]
    fn typed_up_to_a_label_on_the_rule_the_cat_waits_on_the_line() {
        let mut s = resting_above_the_composer();
        let draft = "the quick brown fox jumps over the lazy dog ".repeat(4);
        // The harness off: no card, the label at the upper rule's right end,
        // the chips on the lower rule.
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

    /// TYPED TO THE RIGHT MARGIN, THE CAT STAYS ON GLASS (lab, real Claude
    /// Code 2.1.305 with the harness on, windowed, 2026-10-20). The
    /// composer's upper rule is reserved whole, so the stand on its first
    /// line is refused and the cat rests off it. As the caret nears the
    /// right margin the escort's stand is clamped against the pane's edge,
    /// where its clearance margin ran past the pane: the reading left the
    /// map, answered unknown, and the stand was taken as clear. The cat
    /// flew onto the reserved rule and the console law, finding nowhere to
    /// step it, hid it there until the wrap. The margin now stops at the
    /// pane's edge (`PetBrain::stand_rect`): typed to the margin, no frame
    /// hides the cat.
    ///
    /// NEGATIVE CONTROL (as run): the margin grown past the edge, the cat
    /// lands on the rule at key 113 and is hidden for 31 frames
    /// (`protected-home-unavailable`).
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

    /// TYPING INTO AN AGENT'S COMPOSER NEVER SENDS THE CAT HOPPING (lab
    /// `tt-3`, Claude Code 2.1.304 at 120 x 40: keys typed into the composer
    /// and backspaced again, the cat resting above the composer's upper
    /// rule, which the host reserves whole). The stand on the composer's
    /// first line reaches into that rule, so while the first line is typed
    /// the cat keeps its seat, and the seat moves only when the stand is past
    /// [`REST_REACH`] (once here) and a flight to it lands on it: a seat does
    /// not run with the caret, so the aim leads by no drift
    /// (`PetBrain::aims_ahead`; aimed ahead, it landed five columns past).
    /// A draft wrapped onto the second line puts the stand inside the box,
    /// clear: the cat bounds over the rule onto it, once, and walks the line
    /// with the caret. Chased with the console law stepping a body in the
    /// air aside, every flight across the rule was turned back off it and the
    /// cat hopped in place; walled off from the stand instead, it rested out
    /// of reach of the caret it escorts. Backspacing, a held delete
    /// frightens it once and it follows the caret back (a bound when the
    /// caret runs from it), and when the draft unwraps it leaves the line
    /// for a seat; the line it stood on becomes the rule, and its crouch
    /// there, its body clear at its own size, is never stepped aside.
    ///
    /// NEGATIVE CONTROLS (as run, each failing this test): the law stepping
    /// a body in the air aside (as it did), the aim led by the caret's drift
    /// at a seat too (`aims_ahead` as `rhythm_open`), the law judging the
    /// crouch at its drawn width (one frame stepped aside, 0.20 cells).
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

    /// THE LIVE-EDGE HUG NEVER CHASES ACROSS THE HOST'S CHROME (lab `tt-3`):
    /// while Claude Code's output is a burst, the watch re-stations the cat
    /// beside the newest inked row's end, the footer under the composer.
    /// From its seat above the composer's upper rule that stand is beyond
    /// the rule, which the host reserves whole: the console law stepped
    /// every flight there back off it, and the cat hopped in place every
    /// 0.6 s for as long as the burst lasted. A stand the console bars
    /// (`PetBrain::console_bars`) is no hug: the cat keeps watching from
    /// its seat.
    ///
    /// NEGATIVE CONTROLS (as run, each failing this test): the hug that
    /// never asks the console, and `console_walled` answering `false` (the
    /// footer's stand is clear; only the rule on the way bars it): 7 leaps
    /// and 29 frames stepped back off the rule in four seconds.
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

    /// The pane of a shell after a long listing, as the review's lab drew it
    /// (100 x 30): output to row 27, `short`, the prompt on the last row.
    fn shell_after_output() -> String {
        let mut screen = String::new();
        for i in 12..=40 {
            screen.push_str(&format!("line {i} of the output, words words words\r\n"));
        }
        screen.push_str("$ ");
        screen
    }

    /// A HAND'S SELECTION NEVER SENDS THE CAT ACROSS THE PANE (review, lab
    /// `rv-drag`: a shell at 100 x 30, a block selection grown a row at a
    /// time from the prompt up and right over the words). Only the
    /// selection refuses the escort's stand there, so the correction is the
    /// local one it always was: the closest place behind text within
    /// `HOME_REACH`, or the pet yields where it is. When the selection goes
    /// it is back at its stand. The rest seat's long search ran for any
    /// refused stand, and walked the cat about 45 columns along the
    /// selection's edge. Here the stand is under the last output line's
    /// words, as a shell's usually is, and the nearest clean ground is past
    /// their end, 37 columns away.
    ///
    /// NEGATIVE CONTROLS (as run, each failing this test): the seat solved
    /// on the live plane (the selection counted), and the long search for
    /// every refused stand (the ground plane's question skipped): the cat
    /// travels farther than `HOME_REACH`.
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

    /// A SELECTION OVER THE REST SEAT HIDES THE CAT WHERE IT LIES (review,
    /// lab `rv-csel`: Claude Code at 120 x 30, the hand copy-drags the last
    /// output): the seat is solved on the ground plane, without the live
    /// selection, so a selection growing over the sleeping cat gives it no
    /// new seat. The console law moves it no farther than off a selection
    /// anywhere (a step within `HOME_REACH`; here nothing that near is
    /// free, so it yields where it lies), and it is back there when the
    /// selection goes. The live-plane search sent it leaping ten rows up
    /// after the selection, and flying back on clear.
    ///
    /// NEGATIVE CONTROL (as run, failing this test): the seat solved on the
    /// live plane (selections counted), the cat moves off its seat.
    #[test]
    fn a_selection_over_the_rest_seat_hides_the_cat_in_place() {
        // The composer at the pane's foot: the clean ground is beside the
        // turn's last line, where the hand selects.
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

    /// OPENING THE AUTOPILOT MENU OVER THE SLEEPING CAT MOVES NOTHING
    /// (review, lab `rv-day-menu`: a fresh Claude Code screen, 120 x 40). The
    /// menu's card is a transient overlay
    /// ([`PetBrain::observe_console_with_overlays`]), not ground: the rest
    /// seat is solved without it, so the cat yields where it lies under the
    /// card (nothing within `HOME_REACH` is free) and is there again when it
    /// closes. As chrome, the card sent the cat eleven rows down under it,
    /// then ten back up on close.
    ///
    /// NEGATIVE CONTROLS (as run, each failing this test): the card fed as
    /// chrome (an overlay counted as lasting), and the ground plane not kept
    /// (the seat solved on the live plane): the cat leaves its seat.
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
        // The card drops from the footer's Autopilot chip over the blank
        // rows under it, where the cat sleeps.
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

    /// A COMMAND LIST REDRAWN AT EVERY KEY MOVES THE CAT ONCE (lab
    /// `b-day-slash`, Claude Code 2.1.304 at 120 x 40: `/compa` sent a key at
    /// a time, then backspaced): the list Claude Code draws under the
    /// composer covers the cat's seat under the footer, and it goes once to
    /// the clean ground past the list's end. As the list shrinks with the
    /// next keys, that seat is still clean ground it can reach, so it stays:
    /// nearer ground opening up is no reason to move. A longer list over the
    /// seat moves it again.
    ///
    /// NEGATIVE CONTROL (as run, failing this test): no seat kept
    /// (`ConsoleLife::rest_seat` never read), the cat leaps back up after the
    /// shorter list's end.
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

    /// A GAP INSIDE A LINE IS NOT GROUND (review, lab `rv-slash`: `/compa`
    /// typed into a fresh composer, Claude Code drawing its command list
    /// under the footer). The rest seat stays off every line's ink hull, so
    /// the cat never sleeps between `/cd` and its description; it rests past
    /// the list's longest line, or under it.
    ///
    /// NEGATIVE CONTROL: with the hulls ignored, the seat is in the list's
    /// gutter.
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

    /// A FULL TRANSCRIPT ABOVE THE COMPOSER: with no clean ground near the
    /// composer, the cat keeps the nearest under-text seat (behind the
    /// footer's words, a bound over the composer's rules, or behind the
    /// reply's), and typing never hops it, braces it as foreign contact or
    /// pops it to its feet at every key.
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
        // Move the caret and cat to row 12 while the completed block's anchor stays on row 8.
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

        // Now trigger a talking sibling cue to the right of the cat.
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

        // Wave the mouse pointer above and to the right of the settled resident.
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

        // Removing the pointer immediately clears pointer heat and returns to idle-to-zero.
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
        // Place the caret above the executing block (or after a key) so the
        // resident hops to its interactive-caret seat and settles in `PetSit`.
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

        // A rising edge of `output_burst` flicks an ear for `TWITCH_DUR`.
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

        // Once the twitch duration elapses, the resident returns to `PetSit` and idles to zero.
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

        // Noting a bell or a look while clipped on a protected surface must be
        // retired on the next tick so `console_frames()` does not stay pinned.
        s.pet.note_bell(s.now);
        s.pet.note_peek(s.now, s.pet.col + 20.0, s.pet.row);
        assert!(s.pet.pending_look.is_some());
        assert!(s.pet.needs_frames());
        s.frame(0.016);
        assert!(
            s.pet.pending_bell.is_none() && s.pet.pending_look.is_none() && !s.pet.needs_frames(),
            "protected-surface yield must retire pending_bell and pending_look on its tick"
        );

        // `carry_with_regrid` shifts `console.target` and `console.trip` by `dy`.
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
